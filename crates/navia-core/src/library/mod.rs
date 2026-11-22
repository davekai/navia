//! The local library: a per-user SQLite database with FTS5 search, kept in step by [`sync`].
//! A sync stamps every row it sees with a new generation, then removes rows with an older one.
//! WAL mode and separate read and write connections keep the UI from waiting on a sync.

pub mod browse;
pub mod for_you;
pub mod collections;
pub mod copyright;
pub mod discography;
pub mod essentials;
mod downloads;
mod extras;
pub use extras::StarTable;
pub mod genres;
pub mod info;
pub mod recent;
mod rows;
mod schema;
pub mod search;
pub mod sync;
pub mod taste;
pub mod text;

pub use rows::SongOrder;
pub use search::SearchKind;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::de::DeserializeOwned;

use crate::client::{Album, Artist, Credentials, MusicFolder, Song};
use crate::error::{Error, Result};

use search::{album_fields, song_fields};
use text::normalize;

/// an album a change pass fetched again: its new list entry when the entry changed, and the songs
/// `getAlbum` listed.
pub(crate) struct RefetchedAlbum {
    pub id: String,
    pub row: Option<Album>,
    pub song_ids: Vec<String>,
}

/// fewer missing rows than this never count as a mass shrink, whatever the library's size.
const MASS_SHRINK_MIN: i64 = 50;

/// Where the database file lives, injectable for tests.
#[derive(Clone, Debug)]
pub struct LibraryConfig {
    pub path: PathBuf,
}

impl LibraryConfig {
    /// The normal per-user data directory, one database per server and account so people sharing
    /// a computer never see each other's plays or favorites.
    pub fn default_for(credentials: &Credentials, server_url: &str) -> Result<Self> {
        let dirs = directories::ProjectDirs::from("", "", "Navia").ok_or(Error::NoDataDirectory)?;
        let name = format!("{}.sqlite", credentials.account_key(server_url));
        // Local, not roaming, so %LOCALAPPDATA% on Windows.
        Ok(Self { path: dirs.data_local_dir().join("libraries").join(name) })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SongSort {
    Title,
    RecentlyAdded,
    MostPlayed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlbumSort {
    Name,
    RecentlyAdded,
    MostPlayed,
}

/// Results of one local search, best match first within each list.
#[derive(Clone, Debug, Default)]
pub struct SearchResults {
    pub artists: Vec<Artist>,
    pub albums: Vec<Album>,
    pub songs: Vec<Song>,
    /// The list whose first item is the best match overall. Artists and albums win ties with
    /// songs, so "weekend" puts a song called "Weekend" ahead of The Weeknd only when it matches better.
    pub best: Option<SearchKind>,
}

impl SearchResults {
    pub fn is_empty(&self) -> bool {
        self.artists.is_empty() && self.albums.is_empty() && self.songs.is_empty()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LibraryCounts {
    pub artists: u64,
    pub albums: u64,
    pub songs: u64,
}

#[derive(Clone)]
pub struct Library {
    inner: Arc<Inner>,
}

struct Inner {
    path: PathBuf,
    writer: Mutex<Connection>,
    reader: Mutex<Connection>,
    /// Only one sync may run at a time.
    sync_gate: tokio::sync::Mutex<()>,
    /// only one build of For You's mixes, and one change check, at a time; a second caller waits.
    for_you_gate: tokio::sync::Mutex<()>,
    check_gate: tokio::sync::Mutex<()>,
    /// Moves on every write, so derived caches such as the taste scores know they're stale.
    version: AtomicU64,
    taste: Mutex<Option<Arc<taste::Taste>>>,
    /// The Songs filters' facts, with the `version` they were read at.
    song_filters: Mutex<Option<(u64, Arc<crate::song_filters::FilterIndex>)>>,
    /// The Artists filters' facts, likewise.
    artist_filters: Mutex<Option<(u64, Arc<crate::artist_filters::ArtistFilterIndex>)>>,
    /// Home's rotation this session as (local day, turn). `None` until Home first asks.
    home_turn: Mutex<Option<(i64, i64)>>,
    /// Called after a write that can change which songs a playlist or album holds.
    contents_hooks: Mutex<Vec<ContentsHook>>,
    /// The Autoplay engine's in-memory state shared by every holder of this library.
    engine: crate::engine::Shared,
    /// The fuzzy search index, kept current from the writer's changes.
    search: search::SearchState,
}

type ContentsHook = Box<dyn Fn() + Send + Sync>;

fn configure(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    Ok(())
}

impl Library {
    pub fn open(config: &LibraryConfig) -> Result<Self> {
        if let Some(parent) = config.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut writer = Connection::open(&config.path)?;
        configure(&writer)?;
        schema::migrate(&mut writer)?;
        let search = search::SearchState::default();
        search.watch(&mut writer)?;
        let reader = Connection::open(&config.path)?;
        configure(&reader)?;
        // Album versions, so songs show their album's name without the version Navidrome appends.
        {
            let mut st = reader.prepare("SELECT id, json_extract(data, '$.version') FROM album WHERE ifnull(json_extract(data, '$.version'), '') <> ''")?;
            let rows: Vec<(String, String)> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.filter_map(|r| r.ok()).collect();
            crate::display::remember_versions(rows.iter().map(|(id, v)| (id.as_str(), Some(v.as_str()))));
        }
        let lib = Self {
            inner: Arc::new(Inner {
                path: config.path.clone(),
                writer: Mutex::new(writer),
                reader: Mutex::new(reader),
                sync_gate: tokio::sync::Mutex::new(()),
                for_you_gate: tokio::sync::Mutex::new(()),
                check_gate: tokio::sync::Mutex::new(()),
                version: AtomicU64::new(0),
                taste: Mutex::new(None),
                song_filters: Mutex::new(None),
                artist_filters: Mutex::new(None),
                home_turn: Mutex::new(None),
                contents_hooks: Mutex::new(Vec::new()),
                engine: Default::default(),
                search,
            }),
        };
        // One-time setting changes, each run once per library.
        if let Err(e) = crate::engine::migrate_mode(&lib) {
            tracing::warn!("autoplay mode migration: {e}");
        }
        // Build the search index off the caller's thread, so the first search is instant.
        let warm = lib.clone();
        let _ = std::thread::Builder::new().name("navia-search-index".into()).spawn(move || {
            if let Err(e) = warm.inner.search.warm(&warm.inner.path) {
                tracing::warn!("search index: {e}");
            }
        });
        Ok(lib)
    }

    /// The Autoplay engine's in-memory state for this library.
    pub(crate) fn engine_shared(&self) -> &crate::engine::Shared {
        &self.inner.engine
    }

    /// Changes whenever this process has written to the library.
    pub(crate) fn version(&self) -> u64 {
        self.inner.version.load(Ordering::Relaxed)
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Calls `hook` after every write that can change which songs a playlist or album holds.
    /// It runs on the writing thread, so it must only signal, never wait.
    pub fn on_contents_changed(&self, hook: impl Fn() + Send + Sync + 'static) {
        self.inner.contents_hooks.lock().expect("library hooks").push(Box::new(hook));
    }

    pub(crate) fn contents_changed(&self) {
        for hook in self.inner.contents_hooks.lock().expect("library hooks").iter() {
            hook();
        }
    }

    fn write<T>(&self, f: impl FnOnce(&Transaction) -> Result<T>) -> Result<T> {
        let mut conn = self.inner.writer.lock().expect("library writer poisoned");
        let out = self.in_transaction(&mut conn, f)?;
        self.inner.version.fetch_add(1, Ordering::Relaxed);
        Ok(out)
    }

    /// A write that no derived cache depends on, so it leaves `version` alone.
    pub(crate) fn write_quiet<T>(&self, f: impl FnOnce(&Transaction) -> Result<T>) -> Result<T> {
        let mut conn = self.inner.writer.lock().expect("library writer poisoned");
        self.in_transaction(&mut conn, f)
    }

    /// Runs `f` in a write transaction and tells search which rows it changed, once committed.
    fn in_transaction<T>(&self, conn: &mut Connection, f: impl FnOnce(&Transaction) -> Result<T>) -> Result<T> {
        let result = (|| {
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let out = f(&tx)?;
            tx.commit()?;
            Ok(out)
        })();
        self.inner.search.finish_write(result.is_ok());
        result
    }

    pub(crate) fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = self.inner.reader.lock().expect("library reader poisoned");
        f(&conn)
    }

    // ---- queries -------------------------------------------------------------------------

    pub fn counts(&self) -> Result<LibraryCounts> {
        self.read(|c| {
            Ok(LibraryCounts {
                artists: c.query_row("SELECT COUNT(*) FROM artist", [], |r| r.get::<_, i64>(0))? as u64,
                albums: c.query_row("SELECT COUNT(*) FROM album", [], |r| r.get::<_, i64>(0))? as u64,
                songs: c.query_row("SELECT COUNT(*) FROM song", [], |r| r.get::<_, i64>(0))? as u64,
            })
        })
    }

    pub fn libraries(&self) -> Result<Vec<MusicFolder>> {
        self.read(|c| {
            let mut st = c.prepare_cached("SELECT id, name FROM library ORDER BY name COLLATE NOCASE")?;
            let rows = st.query_map([], |r| Ok(MusicFolder { id: r.get(0)?, name: r.get(1)? }))?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    pub fn song(&self, id: &str) -> Result<Option<Song>> {
        self.read(|c| fetch_data(c, "SELECT data FROM song WHERE id = ?1", [id]))
    }

    pub fn album(&self, id: &str) -> Result<Option<Album>> {
        self.read(|c| fetch_data(c, "SELECT data FROM album WHERE id = ?1", [id]))
    }

    pub fn artist(&self, id: &str) -> Result<Option<Artist>> {
        self.read(|c| fetch_data(c, "SELECT data FROM artist WHERE id = ?1", [id]))
    }

    /// The artist with the most albums, or `None` before the first sync.
    pub fn top_artist(&self) -> Result<Option<Artist>> {
        self.read(|c| fetch_data(c, "SELECT data FROM artist ORDER BY album_count DESC, sort_name LIMIT 1", []))
    }

    /// An album's songs in disc and track order.
    pub fn album_songs(&self, album_id: &str) -> Result<Vec<Song>> {
        self.read(|c| fetch_list(c, "SELECT data FROM song WHERE album_id = ?1 ORDER BY disc, track, sort_name", [album_id]))
    }

    pub fn list_songs(&self, sort: SongSort, offset: u32, limit: u32, libraries: Option<&[String]>) -> Result<Vec<Song>> {
        let order = match sort {
            SongSort::Title => "sort_name",
            SongSort::RecentlyAdded => "created DESC, sort_name",
            SongSort::MostPlayed => "play_count DESC, sort_name",
        };
        self.list("song", order, offset, limit, libraries)
    }

    pub fn list_albums(&self, sort: AlbumSort, offset: u32, limit: u32, libraries: Option<&[String]>) -> Result<Vec<Album>> {
        let order = match sort {
            AlbumSort::Name => "sort_name",
            AlbumSort::RecentlyAdded => "created DESC, sort_name",
            AlbumSort::MostPlayed => "play_count DESC, sort_name",
        };
        self.list("album", order, offset, limit, libraries)
    }

    pub fn list_artists(&self, offset: u32, limit: u32) -> Result<Vec<Artist>> {
        self.read(|c| fetch_list(c, "SELECT data FROM artist ORDER BY sort_name LIMIT ?1 OFFSET ?2", params![limit, offset]))
    }

    fn list<T: DeserializeOwned>(&self, table: &str, order: &str, offset: u32, limit: u32, libraries: Option<&[String]>) -> Result<Vec<T>> {
        let filter = library_filter(libraries);
        let sql = format!("SELECT data FROM {table} WHERE {filter} ORDER BY {order} LIMIT ? OFFSET ?");
        let mut values: Vec<rusqlite::types::Value> = library_params(libraries);
        values.push(rusqlite::types::Value::Integer(limit as i64));
        values.push(rusqlite::types::Value::Integer(offset as i64));
        self.read(|c| fetch_list(c, &sql, rusqlite::params_from_iter(values)))
    }

    /// Instant local search across artists, albums and songs, forgiving typos, accents,
    /// punctuation and joined or split words.
    pub fn search(&self, query: &str, limit_per_kind: u32, libraries: Option<&[String]>) -> Result<SearchResults> {
        let limit = limit_per_kind.max(1) as usize;
        self.read(|c| {
            let ranked = self.inner.search.search(&self.inner.path, c, query, limit, libraries)?;
            let [artists, albums, songs] = &ranked.hits;
            Ok(SearchResults {
                artists: search::load(c, SearchKind::Artist, artists)?,
                albums: search::load(c, SearchKind::Album, albums)?,
                songs: search::load(c, SearchKind::Song, songs)?,
                best: ranked.best,
            })
        })
    }

    // ---- sync support (crate-private) ----------------------------------------------------

    /// Record the libraries this user can see, removing data from ones they lost access to.
    pub(crate) fn replace_libraries(&self, folders: &[MusicFolder]) -> Result<()> {
        self.write(|tx| {
            let known: Vec<String> = tx.prepare("SELECT id FROM library")?.query_map([], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
            for id in known.iter().filter(|id| !folders.iter().any(|f| &f.id == *id)) {
                delete_library_data(tx, id)?;
                tx.execute("DELETE FROM library WHERE id = ?1", [id])?;
                tx.execute("DELETE FROM sync_state WHERE library_id = ?1", [id])?;
            }
            for f in folders {
                tx.execute("INSERT INTO library(id, name) VALUES (?1, ?2) ON CONFLICT(id) DO UPDATE SET name = excluded.name", [&f.id, &f.name])?;
            }
            remove_orphan_artists(tx)
        })
    }

    /// Start a new generation, or continue the interrupted one.
    pub(crate) fn begin_sync(&self, library_id: &str, marker: Option<&str>, scanning: bool) -> Result<sync::SyncCursor> {
        self.write(|tx| {
            if let Some(mut cur) = read_cursor(tx, library_id)? {
                if cur.active {
                    // Resuming keeps the original marker and hold flag, so the final check compares
                    // against the state when the generation began.
                    return Ok(cur);
                }
                cur = sync::SyncCursor { generation: cur.generation + 1, active: true, phase: sync::SyncPhase::Artists, offset: 0, scan_marker: marker.map(str::to_string), hold_removal: scanning, pending_shrink: cur.pending_shrink };
                write_cursor(tx, library_id, &cur)?;
                return Ok(cur);
            }
            let cur = sync::SyncCursor { generation: 1, active: true, phase: sync::SyncPhase::Artists, offset: 0, scan_marker: marker.map(str::to_string), hold_removal: scanning, pending_shrink: false };
            write_cursor(tx, library_id, &cur)?;
            Ok(cur)
        })
    }

    pub(crate) fn apply_artists(&self, library_id: &str, cursor: &sync::SyncCursor, next_offset: u32, artists: &[Artist]) -> Result<()> {
        self.write(|tx| {
            for a in artists.iter().filter(|a| !a.id.is_empty()) {
                upsert_artist(tx, library_id, cursor.generation, a)?;
            }
            write_progress(tx, library_id, sync::SyncPhase::Artists, next_offset)
        })
    }

    pub(crate) fn apply_albums(&self, library_id: &str, cursor: &sync::SyncCursor, next_offset: u32, albums: &[Album]) -> Result<()> {
        crate::display::remember_versions(albums.iter().map(|a| (a.id.as_str(), a.version.as_deref())));
        self.write(|tx| {
            for a in albums.iter().filter(|a| !a.id.is_empty()) {
                upsert_album(tx, library_id, cursor.generation, a)?;
            }
            write_progress(tx, library_id, sync::SyncPhase::Albums, next_offset)
        })
    }

    pub(crate) fn apply_songs(&self, library_id: &str, cursor: &sync::SyncCursor, next_offset: u32, songs: &[Song]) -> Result<()> {
        self.write(|tx| {
            for s in songs.iter().filter(|s| !s.id.is_empty()) {
                upsert_song(tx, library_id, cursor.generation, s)?;
            }
            write_progress(tx, library_id, sync::SyncPhase::Songs, next_offset)
        })
    }

    pub(crate) fn advance_phase(&self, library_id: &str, phase: sync::SyncPhase) -> Result<()> {
        self.write(|tx| write_progress(tx, library_id, phase, 0))
    }

    /// Close a library's generation: remove what wasn't seen, unless removal is being held.
    pub(crate) fn finish_library(&self, library_id: &str, cursor: &sync::SyncCursor, hold_because: Option<String>) -> Result<sync::LibraryFinish> {
        let finish = self.finish_library_rows(library_id, cursor, hold_because);
        self.contents_changed();
        finish
    }

    fn finish_library_rows(&self, library_id: &str, cursor: &sync::SyncCursor, hold_because: Option<String>) -> Result<sync::LibraryFinish> {
        self.write(|tx| {
            let generation = cursor.generation;
            let stale: i64 = tx.query_row("SELECT COUNT(*) FROM song WHERE library_id=?1 AND generation<?2", params![library_id, generation], |r| r.get(0))?;
            let seen: i64 = tx.query_row("SELECT COUNT(*) FROM song WHERE library_id=?1 AND generation=?2", params![library_id, generation], |r| r.get(0))?;
            // require a second sync to confirm, so one bad scan can't wipe the local copy.
            let shrinks = looks_like_mass_shrink(stale, seen);
            let mut hold = hold_because;
            if hold.is_none() && shrinks && !cursor.pending_shrink {
                hold = Some(format!("{stale} songs would be removed against {seen} found; waiting for a second sync to confirm"));
            }
            let mut removed = LibraryCounts::default();
            let pending_shrink = shrinks && hold.is_some();
            if hold.is_none() {
                removed = prune_library(tx, library_id, generation)?;
            }
            tx.execute(
                "UPDATE sync_state SET active=0, phase='done', offset=0, hold_removal=0, pending_shrink=?2 WHERE library_id=?1",
                params![library_id, pending_shrink],
            )?;
            Ok(sync::LibraryFinish { removed, held: hold })
        })
    }

    /// the generation of this library's last finished sync, or `None` when none has finished or one
    /// was interrupted and has to resume.
    pub(crate) fn settled_generation(&self, library_id: &str) -> Result<Option<i64>> {
        self.read(|c| Ok(read_cursor(c, library_id)?.filter(|cur| !cur.active).map(|cur| cur.generation)))
    }

    /// one page of a change pass's album list. albums whose catalog entry is unchanged are stored
    /// (so their plays and stars stay current); the rest come back, each with whether it's new here.
    pub(crate) fn apply_album_page(&self, library_id: &str, generation: i64, albums: &[Album]) -> Result<Vec<(Album, bool)>> {
        crate::display::remember_versions(albums.iter().map(|a| (a.id.as_str(), a.version.as_deref())));
        self.write(|tx| {
            let mut changed = Vec::new();
            for a in albums.iter().filter(|a| !a.id.is_empty()) {
                let stored: Option<String> = tx.prepare_cached("SELECT data FROM album WHERE id = ?1")?.query_row([&a.id], |r| r.get(0)).optional()?;
                let stored: Option<Album> = stored.and_then(|d| serde_json::from_str(&d).ok());
                match stored {
                    Some(old) if catalog_entry(&old) == catalog_entry(a) => upsert_album(tx, library_id, generation, a)?,
                    Some(_) => changed.push((a.clone(), false)),
                    None => changed.push((a.clone(), true)),
                }
            }
            Ok(changed)
        })
    }

    /// one page of a change pass's artist list.
    pub(crate) fn apply_artist_page(&self, library_id: &str, generation: i64, artists: &[Artist]) -> Result<()> {
        self.write(|tx| {
            for a in artists.iter().filter(|a| !a.id.is_empty()) {
                upsert_artist(tx, library_id, generation, a)?;
            }
            Ok(())
        })
    }

    /// stores the songs `getAlbum` gave for an album a change pass refetched, with `new_row` when the
    /// album is new here. a changed album's row waits for [`Self::finish_changes`], so a pass cut
    /// short finds it changed again and fetches it next time.
    pub(crate) fn apply_album_songs(&self, library_id: &str, generation: i64, new_row: Option<&Album>, songs: &[Song]) -> Result<()> {
        self.write(|tx| {
            for s in songs.iter().filter(|s| !s.id.is_empty()) {
                upsert_song(tx, library_id, generation, s)?;
            }
            if let Some(album) = new_row {
                upsert_album(tx, library_id, generation, album)?;
            }
            Ok(())
        })
    }

    /// this library's stored albums missing from `seen`, the album list a change pass just read.
    pub(crate) fn albums_not_in(&self, library_id: &str, seen: &std::collections::HashSet<String>) -> Result<Vec<String>> {
        self.read(|c| {
            let ids: Vec<String> = c.prepare("SELECT id FROM album WHERE library_id=?1")?.query_map([library_id], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
            Ok(ids.into_iter().filter(|id| !seen.contains(id)).collect())
        })
    }

    /// ends a change pass for one library: stores the changed albums' rows, then, unless removal is
    /// held, drops the songs those albums no longer list, the albums that left the list with their
    /// songs, and the library's links to artists it no longer lists (`seen_artists`, when they were
    /// read). songs that moved to another album were already re-pointed by that album's songs, so
    /// they stay. a list that suddenly lost most of its albums holds removal, as a full sync does.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn finish_changes(&self, library_id: &str, generation: i64, changed: &[RefetchedAlbum], vanished: &[String], seen_albums: usize, seen_artists: Option<&std::collections::HashSet<String>>, hold_because: Option<String>) -> Result<sync::LibraryFinish> {
        let finish = self.write(|tx| {
            for album in changed.iter().filter_map(|c| c.row.as_ref()) {
                upsert_album(tx, library_id, generation, album)?;
            }
            let mut hold = hold_because;
            if hold.is_none() && looks_like_mass_shrink(vanished.len() as i64, seen_albums as i64) {
                hold = Some(format!("{} albums would be removed against {seen_albums} listed; waiting for a full sync to confirm", vanished.len()));
            }
            let mut removed = LibraryCounts::default();
            if hold.is_some() {
                return Ok(sync::LibraryFinish { removed, held: hold });
            }
            for album in changed {
                let keep: std::collections::HashSet<&str> = album.song_ids.iter().map(String::as_str).collect();
                let rows: Vec<(i64, String)> = tx.prepare_cached("SELECT rowid, id FROM song WHERE library_id=?1 AND album_id=?2")?.query_map(params![library_id, album.id], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<std::result::Result<_, _>>()?;
                let dropped: Vec<i64> = rows.into_iter().filter(|(_, id)| !keep.contains(id.as_str())).map(|(rowid, _)| rowid).collect();
                removed.songs += delete_rowids(tx, "song", "fts_song", &dropped)?;
            }
            for id in vanished {
                let songs: Vec<i64> = tx.prepare_cached("SELECT rowid FROM song WHERE library_id=?1 AND album_id=?2")?.query_map(params![library_id, id], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
                removed.songs += delete_rowids(tx, "song", "fts_song", &songs)?;
                let album: Vec<i64> = tx.prepare_cached("SELECT rowid FROM album WHERE library_id=?1 AND id=?2")?.query_map(params![library_id, id], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
                removed.albums += delete_rowids(tx, "album", "fts_album", &album)?;
            }
            if let Some(seen) = seen_artists {
                let links: Vec<(i64, String)> = tx.prepare("SELECT l.artist_rowid, a.id FROM artist_library l JOIN artist a ON a.rowid = l.artist_rowid WHERE l.library_id=?1")?.query_map([library_id], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<std::result::Result<_, _>>()?;
                let mut unlinked = 0;
                for (rowid, _) in links.iter().filter(|(_, id)| !seen.contains(id)) {
                    unlinked += tx.prepare_cached("DELETE FROM artist_library WHERE artist_rowid=?1 AND library_id=?2")?.execute(params![rowid, library_id])?;
                }
                if unlinked > 0 {
                    removed.artists = remove_orphan_artists_count(tx)?;
                }
            }
            Ok(sync::LibraryFinish { removed, held: None })
        });
        self.contents_changed();
        finish
    }

    pub(crate) fn set_meta(&self, key: &str, value: Option<&str>) -> Result<()> {
        self.write(|tx| {
            match value {
                Some(v) => tx.execute("INSERT INTO meta(key, value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [key, v])?,
                None => tx.execute("DELETE FROM meta WHERE key=?1", [key])?,
            };
            Ok(())
        })
    }

    pub(crate) fn meta(&self, key: &str) -> Result<Option<String>> {
        self.read(|c| Ok(c.query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0)).optional()?))
    }

    pub(crate) fn try_sync_gate(&self) -> Option<tokio::sync::MutexGuard<'_, ()>> {
        self.inner.sync_gate.try_lock().ok()
    }

    pub(crate) async fn sync_gate(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.inner.sync_gate.lock().await
    }

    pub(crate) async fn for_you_gate(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.inner.for_you_gate.lock().await
    }

    /// whether today's mixes are being made right now.
    pub fn for_you_building(&self) -> bool {
        self.inner.for_you_gate.try_lock().is_err()
    }

    pub(crate) async fn check_gate(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.inner.check_gate.lock().await
    }

    /// Run a blocking database call off the async runtime.
    pub(crate) async fn blocking<T: Send + 'static>(&self, f: impl FnOnce(Library) -> Result<T> + Send + 'static) -> Result<T> {
        let lib = self.clone();
        tokio::task::spawn_blocking(move || f(lib)).await.map_err(|e| Error::DatabaseTask(e.to_string()))?
    }
}

/// insert or update one artist and link it to the library at this generation.
fn upsert_artist(tx: &Transaction, library_id: &str, generation: i64, a: &Artist) -> Result<()> {
    let data = serde_json::to_string(a).map_err(|e| Error::Sync(e.to_string()))?;
    let hash = format!("{:x}", md5::compute(&data));
    let sort = a.sort_name.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| a.name.to_lowercase());
    let existing: Option<(i64, String)> = tx
        .prepare_cached("SELECT rowid, content_hash FROM artist WHERE id = ?1")?
        .query_row([&a.id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    let rowid = match existing {
        Some((rowid, old_hash)) if old_hash == hash => rowid,
        Some((rowid, _)) => {
            tx.prepare_cached("UPDATE artist SET name=?2, sort_name=?3, album_count=?4, starred=?5, content_hash=?6, data=?7 WHERE rowid=?1")?
                .execute(params![rowid, a.name, sort, a.album_count.unwrap_or(0), a.starred, hash, data])?;
            tx.prepare_cached("DELETE FROM fts_artist WHERE rowid=?1")?.execute([rowid])?;
            tx.prepare_cached("INSERT INTO fts_artist(rowid, name) VALUES (?1, ?2)")?.execute(params![rowid, normalize(&a.name)])?;
            rowid
        }
        None => {
            tx.prepare_cached("INSERT INTO artist(id, name, sort_name, album_count, starred, content_hash, data) VALUES (?1,?2,?3,?4,?5,?6,?7)")?
                .execute(params![a.id, a.name, sort, a.album_count.unwrap_or(0), a.starred, hash, data])?;
            let rowid = tx.last_insert_rowid();
            tx.prepare_cached("INSERT INTO fts_artist(rowid, name) VALUES (?1, ?2)")?.execute(params![rowid, normalize(&a.name)])?;
            rowid
        }
    };
    tx.prepare_cached("INSERT INTO artist_library(artist_rowid, library_id, generation) VALUES (?1,?2,?3) ON CONFLICT(artist_rowid, library_id) DO UPDATE SET generation=excluded.generation")?
        .execute(params![rowid, library_id, generation])?;
    Ok(())
}

/// insert or update one album, stamped with this generation.
fn upsert_album(tx: &Transaction, library_id: &str, generation: i64, a: &Album) -> Result<()> {
    let data = serde_json::to_string(a).map_err(|e| Error::Sync(e.to_string()))?;
    let hash = format!("{:x}", md5::compute(&data));
    let sort = a.sort_name.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| a.name.to_lowercase());
    let existing: Option<(i64, String)> = tx
        .prepare_cached("SELECT rowid, content_hash FROM album WHERE id = ?1")?
        .query_row([&a.id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    let index_text = |a: &Album| {
        let [name, artist] = album_fields(a);
        (normalize(&name), normalize(&artist))
    };
    match existing {
        Some((rowid, old_hash)) if old_hash == hash => {
            tx.prepare_cached("UPDATE album SET generation=?2, library_id=?3 WHERE rowid=?1")?.execute(params![rowid, generation, library_id])?;
        }
        Some((rowid, _)) => {
            tx.prepare_cached("UPDATE album SET library_id=?2, name=?3, sort_name=?4, artist=?5, artist_id=?6, year=?7, created=?8, play_count=?9, played=?10, starred=?11, generation=?12, content_hash=?13, data=?14 WHERE rowid=?1")?
                .execute(params![rowid, library_id, a.name, sort, a.artist, a.artist_id, a.year, a.created, a.play_count.unwrap_or(0), a.played, a.starred, generation, hash, data])?;
            tx.prepare_cached("DELETE FROM fts_album WHERE rowid=?1")?.execute([rowid])?;
            let (n, ar) = index_text(a);
            tx.prepare_cached("INSERT INTO fts_album(rowid, name, artist) VALUES (?1,?2,?3)")?.execute(params![rowid, n, ar])?;
        }
        None => {
            tx.prepare_cached("INSERT INTO album(id, library_id, name, sort_name, artist, artist_id, year, created, play_count, played, starred, generation, content_hash, data) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)")?
                .execute(params![a.id, library_id, a.name, sort, a.artist, a.artist_id, a.year, a.created, a.play_count.unwrap_or(0), a.played, a.starred, generation, hash, data])?;
            let rowid = tx.last_insert_rowid();
            let (n, ar) = index_text(a);
            tx.prepare_cached("INSERT INTO fts_album(rowid, name, artist) VALUES (?1,?2,?3)")?.execute(params![rowid, n, ar])?;
        }
    }
    Ok(())
}

/// insert or update one song, stamped with this generation. an unchanged song keeps its rowid.
fn upsert_song(tx: &Transaction, library_id: &str, generation: i64, s: &Song) -> Result<()> {
    let data = serde_json::to_string(s).map_err(|e| Error::Sync(e.to_string()))?;
    let hash = format!("{:x}", md5::compute(&data));
    let sort = s.sort_name.clone().filter(|v| !v.is_empty()).unwrap_or_else(|| s.title.to_lowercase());
    let existing: Option<(i64, String)> = tx
        .prepare_cached("SELECT rowid, content_hash FROM song WHERE id = ?1")?
        .query_row([&s.id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    let index_text = |s: &Song| {
        let [title, artist, album, album_artist] = song_fields(s);
        (normalize(&title), normalize(&artist), normalize(&album), normalize(&album_artist))
    };
    match existing {
        Some((rowid, old_hash)) if old_hash == hash => {
            tx.prepare_cached("UPDATE song SET generation=?2, library_id=?3 WHERE rowid=?1")?.execute(params![rowid, generation, library_id])?;
        }
        Some((rowid, _)) => {
            tx.prepare_cached("UPDATE song SET library_id=?2, title=?3, sort_name=?4, album_id=?5, artist_id=?6, track=?7, disc=?8, year=?9, duration=?10, created=?11, play_count=?12, played=?13, starred=?14, generation=?15, content_hash=?16, data=?17 WHERE rowid=?1")?
                .execute(params![rowid, library_id, s.title, sort, s.album_id, s.artist_id, s.track, s.disc_number, s.year, s.duration, s.created, s.play_count.unwrap_or(0), s.played, s.starred, generation, hash, data])?;
            tx.prepare_cached("DELETE FROM fts_song WHERE rowid=?1")?.execute([rowid])?;
            let (t, ar, al, aa) = index_text(s);
            tx.prepare_cached("INSERT INTO fts_song(rowid, title, artist, album, album_artist) VALUES (?1,?2,?3,?4,?5)")?.execute(params![rowid, t, ar, al, aa])?;
        }
        None => {
            tx.prepare_cached("INSERT INTO song(id, library_id, title, sort_name, album_id, artist_id, track, disc, year, duration, created, play_count, played, starred, generation, content_hash, data) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)")?
                .execute(params![s.id, library_id, s.title, sort, s.album_id, s.artist_id, s.track, s.disc_number, s.year, s.duration, s.created, s.play_count.unwrap_or(0), s.played, s.starred, generation, hash, data])?;
            let rowid = tx.last_insert_rowid();
            let (t, ar, al, aa) = index_text(s);
            tx.prepare_cached("INSERT INTO fts_song(rowid, title, artist, album, album_artist) VALUES (?1,?2,?3,?4,?5)")?.execute(params![rowid, t, ar, al, aa])?;
        }
    }
    Ok(())
}

fn write_progress(tx: &Transaction, library_id: &str, phase: sync::SyncPhase, offset: u32) -> Result<()> {
    tx.prepare_cached("UPDATE sync_state SET phase=?2, offset=?3 WHERE library_id=?1")?
        .execute(params![library_id, phase.as_str(), offset])?;
    Ok(())
}

fn read_cursor(c: &Connection, library_id: &str) -> Result<Option<sync::SyncCursor>> {
    Ok(c.query_row(
        "SELECT generation, active, phase, offset, scan_marker, hold_removal, pending_shrink FROM sync_state WHERE library_id=?1",
        [library_id],
        |r| {
            Ok(sync::SyncCursor {
                generation: r.get(0)?,
                active: r.get(1)?,
                phase: sync::SyncPhase::parse(&r.get::<_, String>(2)?),
                offset: r.get(3)?,
                scan_marker: r.get(4)?,
                hold_removal: r.get(5)?,
                pending_shrink: r.get(6)?,
            })
        },
    )
    .optional()?)
}

fn write_cursor(tx: &Transaction, library_id: &str, cur: &sync::SyncCursor) -> Result<()> {
    tx.execute(
        "INSERT INTO sync_state(library_id, generation, active, phase, offset, scan_marker, hold_removal, pending_shrink) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
         ON CONFLICT(library_id) DO UPDATE SET generation=excluded.generation, active=excluded.active, phase=excluded.phase, offset=excluded.offset, scan_marker=excluded.scan_marker, hold_removal=excluded.hold_removal, pending_shrink=excluded.pending_shrink",
        params![library_id, cur.generation, cur.active, cur.phase.as_str(), cur.offset, cur.scan_marker, cur.hold_removal, cur.pending_shrink],
    )?;
    Ok(())
}

fn prune_library(tx: &Transaction, library_id: &str, generation: i64) -> Result<LibraryCounts> {
    let songs = delete_stale(tx, "song", "fts_song", library_id, generation)?;
    let albums = delete_stale(tx, "album", "fts_album", library_id, generation)?;
    let links = tx.execute("DELETE FROM artist_library WHERE library_id=?1 AND generation<?2", params![library_id, generation])?;
    let artists = if links > 0 { remove_orphan_artists_count(tx)? } else { 0 };
    Ok(LibraryCounts { artists, albums, songs })
}

fn delete_stale(tx: &Transaction, table: &str, fts: &str, library_id: &str, generation: i64) -> Result<u64> {
    let sql = format!("SELECT rowid FROM {table} WHERE library_id=?1 AND generation<?2");
    let ids: Vec<i64> = tx.prepare(&sql)?.query_map(params![library_id, generation], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
    delete_rowids(tx, table, fts, &ids)
}

fn delete_rowids(tx: &Transaction, table: &str, fts: &str, rowids: &[i64]) -> Result<u64> {
    let delete_fts = format!("DELETE FROM {fts} WHERE rowid=?1");
    let delete_row = format!("DELETE FROM {table} WHERE rowid=?1");
    for id in rowids {
        tx.prepare_cached(&delete_fts)?.execute([id])?;
        tx.prepare_cached(&delete_row)?.execute([id])?;
    }
    Ok(rowids.len() as u64)
}

/// a library that suddenly looks mostly gone is more likely a server problem, for example a
/// detached music folder, than a real mass deletion.
fn looks_like_mass_shrink(stale: i64, seen: i64) -> bool {
    (seen == 0 && stale > 0) || (stale >= MASS_SHRINK_MIN && stale > seen)
}

/// the catalog part of an album entry: what a scan changes, without this user's plays, stars and rating.
fn catalog_entry(a: &Album) -> Album {
    Album { starred: None, played: None, play_count: None, user_rating: None, ..a.clone() }
}

fn delete_library_data(tx: &Transaction, library_id: &str) -> Result<()> {
    delete_stale(tx, "song", "fts_song", library_id, i64::MAX)?;
    delete_stale(tx, "album", "fts_album", library_id, i64::MAX)?;
    tx.execute("DELETE FROM artist_library WHERE library_id=?1", [library_id])?;
    Ok(())
}

fn remove_orphan_artists(tx: &Transaction) -> Result<()> {
    remove_orphan_artists_count(tx).map(|_| ())
}

fn remove_orphan_artists_count(tx: &Transaction) -> Result<u64> {
    let ids: Vec<i64> = tx
        .prepare("SELECT rowid FROM artist WHERE rowid NOT IN (SELECT artist_rowid FROM artist_library)")?
        .query_map([], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    for id in &ids {
        tx.prepare_cached("DELETE FROM fts_artist WHERE rowid=?1")?.execute([id])?;
        tx.prepare_cached("DELETE FROM artist WHERE rowid=?1")?.execute([id])?;
    }
    Ok(ids.len() as u64)
}

fn fetch_data<T: DeserializeOwned, P: rusqlite::Params>(c: &Connection, sql: &str, p: P) -> Result<Option<T>> {
    let data: Option<String> = c.prepare_cached(sql)?.query_row(p, |r| r.get(0)).optional()?;
    data.map(|d| serde_json::from_str(&d).map_err(|e| Error::Sync(e.to_string()))).transpose()
}

fn fetch_list<T: DeserializeOwned, P: rusqlite::Params>(c: &Connection, sql: &str, p: P) -> Result<Vec<T>> {
    let mut st = c.prepare_cached(sql)?;
    let rows = st.query_map(p, |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for data in rows {
        out.push(serde_json::from_str(&data?).map_err(|e| Error::Sync(e.to_string()))?);
    }
    Ok(out)
}

fn placeholders(n: usize) -> String {
    vec!["?"; n].join(",")
}

fn library_filter(libraries: Option<&[String]>) -> String {
    library_filter_qualified("library_id", libraries)
}

fn library_filter_qualified(column: &str, libraries: Option<&[String]>) -> String {
    match libraries {
        Some(ids) => format!("{column} IN ({})", placeholders(ids.len())),
        None => "1=1".to_string(),
    }
}

fn library_params(libraries: Option<&[String]>) -> Vec<rusqlite::types::Value> {
    libraries
        .unwrap_or_default()
        .iter()
        .map(|s| rusqlite::types::Value::Text(s.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_library() -> (tempfile::TempDir, Library) {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open(&LibraryConfig { path: dir.path().join("lib.sqlite") }).unwrap();
        (dir, lib)
    }

    fn song(id: &str, title: &str, artist: &str, album: &str) -> Song {
        Song {
            id: id.into(),
            title: title.into(),
            artist: Some(artist.into()),
            album: Some(album.into()),
            album_id: Some(format!("al-{album}")),
            display_artist: Some(artist.into()),
            ..Default::default()
        }
    }

    fn cursor(lib: &Library, id: &str) -> sync::SyncCursor {
        lib.replace_libraries(&[MusicFolder { id: id.into(), name: "L".into() }]).unwrap();
        lib.begin_sync(id, None, false).unwrap()
    }

    #[test]
    fn migrations_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lib.sqlite");
        drop(Library::open(&LibraryConfig { path: path.clone() }).unwrap());
        drop(Library::open(&LibraryConfig { path: path.clone() }).unwrap());
        let c = Connection::open(&path).unwrap();
        let v: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v as usize, schema::current_version());
    }

    #[test]
    fn search_finds_by_prefix_across_fields_and_ranks_exact_titles_first() {
        let (_d, lib) = temp_library();
        let cur = cursor(&lib, "1");
        lib.apply_songs("1", &cur, 3, &[
            song("a", "Come Together", "The Beatles", "Abbey Road"),
            song("b", "Together Again", "Janet", "The Velvet Rope"),
            song("c", "Together", "Someone", "Other"),
        ]).unwrap();
        let r = lib.search("abb", 10, None).unwrap();
        assert_eq!(r.songs.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["a"]);
        let r = lib.search("beatles come", 10, None).unwrap();
        assert_eq!(r.songs[0].id, "a");
        let r = lib.search("together", 10, None).unwrap();
        assert_eq!(r.songs[0].id, "c", "exact title match ranks first");
    }

    #[test]
    fn search_handles_diacritics_cjk_and_arabic() {
        let (_d, lib) = temp_library();
        let cur = cursor(&lib, "1");
        lib.apply_songs("1", &cur, 3, &[
            song("a", "Déjà Vu", "Beyoncé", "B'Day"),
            song("b", "堕天使", "King Gnu", "CEREMONY"),
            song("c", "حبيبي", "فيروز", "ألبوم"),
        ]).unwrap();
        assert_eq!(lib.search("deja", 10, None).unwrap().songs[0].id, "a");
        assert_eq!(lib.search("beyonce", 10, None).unwrap().songs[0].id, "a");
        assert_eq!(lib.search("天使", 10, None).unwrap().songs[0].id, "b", "substring inside CJK");
        assert_eq!(lib.search("king gnu", 10, None).unwrap().songs[0].id, "b");
        assert_eq!(lib.search("فيروز", 10, None).unwrap().songs[0].id, "c");
        assert_eq!(lib.search("حبيب", 10, None).unwrap().songs[0].id, "c", "arabic prefix");
    }

    #[test]
    fn search_never_errors_on_hostile_input() {
        let (_d, lib) = temp_library();
        let cur = cursor(&lib, "1");
        lib.apply_songs("1", &cur, 1, &[song("a", "Hello", "World", "X")]).unwrap();
        for q in ["\"", "NEAR(", "title:hello", "a AND", "***", "(", "ähm \" OR", "\u{0}", &"x".repeat(5000)] {
            lib.search(q, 10, None).unwrap();
        }
    }

    #[test]
    fn unchanged_song_keeps_rowid_changed_song_updates_search_text() {
        let (_d, lib) = temp_library();
        let cur = cursor(&lib, "1");
        lib.apply_songs("1", &cur, 1, &[song("a", "Old Name", "X", "Y")]).unwrap();
        lib.apply_songs("1", &cur, 1, &[song("a", "New Name", "X", "Y")]).unwrap();
        assert_eq!(lib.search("old", 10, None).unwrap().songs.len(), 0);
        assert_eq!(lib.search("new", 10, None).unwrap().songs.len(), 1);
        assert_eq!(lib.counts().unwrap().songs, 1);
    }

    #[test]
    fn library_filter_limits_results() {
        let (_d, lib) = temp_library();
        lib.replace_libraries(&[
            MusicFolder { id: "1".into(), name: "A".into() },
            MusicFolder { id: "2".into(), name: "B".into() },
        ]).unwrap();
        let c1 = lib.begin_sync("1", None, false).unwrap();
        let c2 = lib.begin_sync("2", None, false).unwrap();
        lib.apply_songs("1", &c1, 1, &[song("a", "Shared Word", "X", "Y")]).unwrap();
        lib.apply_songs("2", &c2, 1, &[song("b", "Shared Word", "X", "Y")]).unwrap();
        assert_eq!(lib.search("shared", 10, Some(&["2".to_string()])).unwrap().songs[0].id, "b");
        assert_eq!(lib.search("shared", 10, None).unwrap().songs.len(), 2);
    }

    /// Scale check for very large libraries. Slow to build, so run it explicitly in release:
    /// `cargo test --release -p navia-core --lib huge -- --ignored --nocapture`.
    #[test]
    #[ignore = "builds a million-row database"]
    fn search_stays_fast_on_a_huge_library() {
        let (dir, lib) = temp_library();
        let cur = cursor(&lib, "1");
        let words = ["night", "love", "river", "golden", "shadow", "dance", "echo", "stone", "summer", "dream", "fire", "blue"];
        let total = 1_000_000usize;
        let started = std::time::Instant::now();
        let mut batch = Vec::with_capacity(500);
        for i in 0..total {
            let t = format!("{} {} {}", words[i % 12], words[(i / 12) % 12], i);
            batch.push(song(&format!("s{i}"), &t, &format!("Artist {}", i % 9000), &format!("Album {}", i % 70000)));
            if batch.len() == 500 {
                lib.apply_songs("1", &cur, i as u32, &batch).unwrap();
                batch.clear();
            }
        }
        println!("indexed {total} songs in {:.1}s", started.elapsed().as_secs_f64());
        let db = lib.path().to_path_buf();
        let size = std::fs::metadata(&db).unwrap().len() + std::fs::metadata(db.with_extension("sqlite-wal")).map(|m| m.len()).unwrap_or(0);
        println!("database {:.0} MB", size as f64 / 1_048_576.0);
        let rss = || std::fs::read_to_string("/proc/self/status").ok().and_then(|s| s.lines().find(|l| l.starts_with("VmRSS")).map(str::to_string)).unwrap_or_default();
        println!("before the search index: {}", rss());
        let t = std::time::Instant::now();
        lib.search("warm", 20, None).unwrap();
        println!("search index ready after {:.1}s, {}", t.elapsed().as_secs_f64(), rss());
        let mut timings: Vec<(String, f64)> = Vec::new();
        for q in ["night", "n", "l", "shadow dance", "golden 123", "artist 42", "qqqqq", "a", "nihgt shadw", "goldn", "shadowdance"] {
            let t = std::time::Instant::now();
            lib.search(q, 20, None).unwrap();
            timings.push((q.to_string(), t.elapsed().as_secs_f64() * 1000.0));
        }
        for (q, ms) in &timings {
            println!("  {q:>14}: {ms:.1} ms");
        }
        assert!(timings.iter().all(|(_, ms)| *ms < 500.0));
        drop(dir);
    }

    #[test]
    fn listing_and_album_order() {
        let (_d, lib) = temp_library();
        let cur = cursor(&lib, "1");
        let mut s1 = song("1", "Track One", "A", "Alb");
        s1.track = Some(1);
        let mut s2 = song("2", "Track Two", "A", "Alb");
        s2.track = Some(2);
        lib.apply_songs("1", &cur, 2, &[s2, s1]).unwrap();
        let album = lib.album_songs("al-Alb").unwrap();
        assert_eq!(album.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["1", "2"]);
        assert_eq!(lib.list_songs(SongSort::Title, 0, 10, None).unwrap().len(), 2);
    }
}
