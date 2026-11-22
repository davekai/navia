//! SQLite schema and migrations. `PRAGMA user_version` holds the applied migration count.

use rusqlite::Connection;

use crate::error::Result;

const FTS_OPTIONS: &str = "content='', contentless_delete=1, tokenize='unicode61 remove_diacritics 2', prefix='2 3'";

/// Migrations in order. Never edit an entry once shipped; append a new one.
fn migrations() -> Vec<String> {
    vec![format!(
        "CREATE TABLE library (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL
        );
        CREATE TABLE artist (
            rowid INTEGER PRIMARY KEY,
            id TEXT NOT NULL UNIQUE,
            name TEXT NOT NULL,
            sort_name TEXT NOT NULL,
            album_count INTEGER NOT NULL DEFAULT 0,
            starred TEXT,
            content_hash TEXT NOT NULL,
            data TEXT NOT NULL
        );
        CREATE INDEX idx_artist_sort ON artist(sort_name);
        CREATE TABLE artist_library (
            artist_rowid INTEGER NOT NULL REFERENCES artist(rowid) ON DELETE CASCADE,
            library_id TEXT NOT NULL,
            generation INTEGER NOT NULL,
            PRIMARY KEY (artist_rowid, library_id)
        ) WITHOUT ROWID;
        CREATE INDEX idx_artist_library ON artist_library(library_id, generation);
        CREATE TABLE album (
            rowid INTEGER PRIMARY KEY,
            id TEXT NOT NULL UNIQUE,
            library_id TEXT NOT NULL,
            name TEXT NOT NULL,
            sort_name TEXT NOT NULL,
            artist TEXT,
            artist_id TEXT,
            year INTEGER,
            created TEXT,
            play_count INTEGER NOT NULL DEFAULT 0,
            played TEXT,
            starred TEXT,
            generation INTEGER NOT NULL,
            content_hash TEXT NOT NULL,
            data TEXT NOT NULL
        );
        CREATE INDEX idx_album_library ON album(library_id, generation);
        CREATE INDEX idx_album_sort ON album(sort_name);
        CREATE INDEX idx_album_artist ON album(artist_id);
        CREATE INDEX idx_album_created ON album(created);
        CREATE TABLE song (
            rowid INTEGER PRIMARY KEY,
            id TEXT NOT NULL UNIQUE,
            library_id TEXT NOT NULL,
            title TEXT NOT NULL,
            sort_name TEXT NOT NULL,
            album_id TEXT,
            artist_id TEXT,
            track INTEGER,
            disc INTEGER,
            year INTEGER,
            duration INTEGER,
            created TEXT,
            play_count INTEGER NOT NULL DEFAULT 0,
            played TEXT,
            starred TEXT,
            generation INTEGER NOT NULL,
            content_hash TEXT NOT NULL,
            data TEXT NOT NULL
        );
        CREATE INDEX idx_song_library ON song(library_id, generation);
        CREATE INDEX idx_song_album ON song(album_id, disc, track);
        CREATE INDEX idx_song_artist ON song(artist_id);
        CREATE INDEX idx_song_sort ON song(sort_name);
        CREATE INDEX idx_song_created ON song(created);
        CREATE VIRTUAL TABLE fts_artist USING fts5(name, {FTS_OPTIONS});
        CREATE VIRTUAL TABLE fts_album USING fts5(name, artist, {FTS_OPTIONS});
        CREATE VIRTUAL TABLE fts_song USING fts5(title, artist, album, album_artist, {FTS_OPTIONS});
        CREATE TABLE sync_state (
            library_id TEXT PRIMARY KEY,
            generation INTEGER NOT NULL DEFAULT 0,
            active INTEGER NOT NULL DEFAULT 0,
            phase TEXT NOT NULL DEFAULT 'artists',
            offset INTEGER NOT NULL DEFAULT 0,
            scan_marker TEXT,
            hold_removal INTEGER NOT NULL DEFAULT 0,
            pending_shrink INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);"
    ),
    // Local copy of playlists, and plays waiting to reach the server.
    "CREATE TABLE playlist (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        sort_name TEXT NOT NULL,
        song_count INTEGER NOT NULL DEFAULT 0,
        changed TEXT,
        readonly INTEGER NOT NULL DEFAULT 0,
        data TEXT NOT NULL
    );
    CREATE INDEX idx_playlist_sort ON playlist(sort_name);
    CREATE TABLE playlist_song (
        playlist_id TEXT NOT NULL REFERENCES playlist(id) ON DELETE CASCADE,
        position INTEGER NOT NULL,
        song_id TEXT NOT NULL,
        data TEXT NOT NULL,
        PRIMARY KEY (playlist_id, position)
    ) WITHOUT ROWID;
    CREATE INDEX idx_playlist_song_song ON playlist_song(song_id);
    CREATE TABLE unsent_play (
        seq INTEGER PRIMARY KEY AUTOINCREMENT,
        song_id TEXT NOT NULL,
        played_at_ms INTEGER NOT NULL
    );".to_string(),
    // Song list sorting walks these instead of sorting the table for every page.
    "CREATE INDEX idx_song_plays ON song(play_count DESC, sort_name);
    CREATE INDEX idx_song_year ON song(year DESC, sort_name);
    CREATE INDEX idx_song_duration ON song(duration DESC, sort_name);
    CREATE INDEX idx_song_artist_name ON song(lower(coalesce(json_extract(data, '$.displayArtist'), json_extract(data, '$.artist'), '')), sort_name);
    CREATE INDEX idx_song_album_name ON song(lower(coalesce(json_extract(data, '$.album'), '')), album_id, disc, track);".to_string(),
    // Files kept for offline playback. Paths are relative to the downloads folder.
    "CREATE TABLE download (
        song_id TEXT PRIMARY KEY,
        path TEXT NOT NULL,
        size INTEGER NOT NULL,
        added_at INTEGER NOT NULL
    ) WITHOUT ROWID;".to_string(),
    // What each download was made as. Null means the original file.
    "ALTER TABLE download ADD COLUMN quality_format TEXT;
    ALTER TABLE download ADD COLUMN quality_kbps INTEGER;".to_string(),
    // This account's plays inside the Top Songs window, from the server's scrobble history.
    "CREATE TABLE recent_play (
        id INTEGER PRIMARY KEY,
        song_id TEXT NOT NULL,
        played_at INTEGER NOT NULL
    );
    CREATE INDEX idx_recent_play_time ON recent_play(played_at);".to_string(),
    // Library artists the server calls similar, kept for the local day they were asked on.
    "CREATE TABLE similar_artist (
        artist_id TEXT PRIMARY KEY,
        day INTEGER NOT NULL,
        similar TEXT NOT NULL
    ) WITHOUT ROWID;".to_string(),
    // Albums keep their disc titles: forget the last scan so the next sync reads them again.
    "DELETE FROM meta WHERE key = 'last_scan_marker';".to_string(),
    // Playlists and albums kept on the device, each with its songs as of its last sync.
    "CREATE TABLE synced_collection (
        kind TEXT NOT NULL,
        id TEXT NOT NULL,
        since_ms INTEGER NOT NULL,
        complete INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (kind, id)
    ) WITHOUT ROWID;
    CREATE TABLE collection_song (
        kind TEXT NOT NULL,
        id TEXT NOT NULL,
        song_id TEXT NOT NULL,
        PRIMARY KEY (kind, id, song_id),
        FOREIGN KEY (kind, id) REFERENCES synced_collection(kind, id) ON DELETE CASCADE
    ) WITHOUT ROWID;
    CREATE INDEX idx_collection_song_song ON collection_song(song_id);
    ALTER TABLE download ADD COLUMN single INTEGER NOT NULL DEFAULT 1;".to_string(),
    // The Autoplay play log, plus running counts per song and artist. `ended` stays null when
    // the app quit mid-song.
    "CREATE TABLE play_event (
        id INTEGER PRIMARY KEY,
        song_id TEXT NOT NULL,
        artist_id TEXT,
        session INTEGER NOT NULL,
        started_ms INTEGER NOT NULL,
        ended_ms INTEGER,
        heard_ms INTEGER,
        duration_ms INTEGER,
        ended TEXT,
        origin TEXT NOT NULL,
        mode TEXT
    );
    CREATE INDEX idx_play_event_session ON play_event(session, started_ms);
    CREATE INDEX idx_play_event_song ON play_event(song_id);
    CREATE TABLE song_stat (
        song_id TEXT PRIMARY KEY,
        completions INTEGER NOT NULL DEFAULT 0,
        early_skips INTEGER NOT NULL DEFAULT 0,
        late_skips INTEGER NOT NULL DEFAULT 0,
        last_completed_ms INTEGER,
        last_early_skip_ms INTEGER
    ) WITHOUT ROWID;
    CREATE TABLE artist_stat (
        artist_id TEXT PRIMARY KEY,
        completions INTEGER NOT NULL DEFAULT 0,
        early_skips INTEGER NOT NULL DEFAULT 0,
        late_skips INTEGER NOT NULL DEFAULT 0
    ) WITHOUT ROWID;".to_string(),
    // Cached Deezer answers for Autoplay and Artist Radio, evicted least recently used first.
    "CREATE TABLE deezer_cache (
        key TEXT PRIMARY KEY,
        fetched_ms INTEGER NOT NULL,
        used_ms INTEGER NOT NULL,
        value TEXT NOT NULL
    ) WITHOUT ROWID;
    CREATE INDEX idx_deezer_cache_used ON deezer_cache(used_ms);".to_string(),
    // Smart transitions: each played song's measured start and end (`audio::edges`), as JSON,
    // with the analysis version and the file size it was measured from.
    "CREATE TABLE song_edges (
        song_id TEXT PRIMARY KEY,
        version INTEGER NOT NULL,
        size INTEGER,
        data TEXT NOT NULL
    ) WITHOUT ROWID;".to_string(),
    // an artist's top song ids as the server answered them, kept for the local day they were asked on.
    "CREATE TABLE top_songs (
        artist_id TEXT PRIMARY KEY,
        day INTEGER NOT NULL,
        ids TEXT NOT NULL
    ) WITHOUT ROWID;".to_string()]
}

/// Apply any migrations that haven't run. Each runs in its own transaction with its version
/// bump, so an interrupted upgrade leaves a clean earlier version.
pub fn migrate(conn: &mut Connection) -> Result<()> {
    let applied: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (i, sql) in migrations().iter().enumerate().skip(applied as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", (i + 1) as i64)?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn current_version() -> usize {
    migrations().len()
}
