//! Parts of the library beyond the catalog: playlists, starred items, play counts and plays
//! waiting to be sent.

use rusqlite::{OptionalExtension, Transaction, params};

use super::Library;
use crate::client::{Album, Playlist, Song, Starred};
use crate::error::{Error, Result};
use crate::library::text::normalize;

/// Oldest plays are dropped beyond this, matching the in-memory queue bound.
const MAX_UNSENT_PLAYS: u32 = 2000;

/// How many annotations a refresh changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AnnotationChanges {
    pub songs: u64,
    pub albums: u64,
    pub artists: u64,
}

/// A play that has not yet reached the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnsentPlay {
    pub seq: i64,
    pub song_id: String,
    pub played_at_ms: u64,
}

/// Which table a star belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StarTable {
    Song,
    Album,
    Artist,
}

impl StarTable {
    pub(crate) fn name(self) -> &'static str {
        match self {
            StarTable::Song => "song",
            StarTable::Album => "album",
            StarTable::Artist => "artist",
        }
    }
}

fn json_err(e: serde_json::Error) -> Error {
    Error::Sync(e.to_string())
}

impl Library {
    // ---- playlists ----------------------------------------------------------------------

    /// Playlists in name order, without their entries.
    pub fn playlists(&self) -> Result<Vec<Playlist>> {
        self.read(|c| {
            let mut st = c.prepare_cached("SELECT data FROM playlist ORDER BY sort_name")?;
            let rows = st.query_map([], |r| r.get::<_, String>(0))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(serde_json::from_str(&row?).map_err(json_err)?);
            }
            Ok(out)
        })
    }

    /// Playlists this user can add songs to, most recently changed first, without their entries.
    pub fn playlist_targets(&self) -> Result<Vec<Playlist>> {
        let mut list: Vec<Playlist> = self.playlists()?.into_iter().filter(crate::library::sync::order_editable).collect();
        // ISO 8601 stamps from one server sort as text; a missing one goes last.
        list.sort_by(|a, b| b.changed.cmp(&a.changed));
        Ok(list)
    }

    /// Which of `song_ids` the stored playlist already holds, in that order, so adding them can
    /// warn first.
    pub fn already_in_playlist(&self, id: &str, song_ids: &[String]) -> Result<Vec<String>> {
        let held: std::collections::HashSet<String> = self.read(|c| {
            let mut st = c.prepare_cached("SELECT song_id FROM playlist_song WHERE playlist_id = ?1")?;
            let rows = st.query_map([id], |r| r.get::<_, String>(0))?;
            rows.collect::<std::result::Result<_, _>>().map_err(Into::into)
        })?;
        Ok(song_ids.iter().filter(|s| held.contains(*s)).cloned().collect())
    }

    /// One playlist with its songs in order, preferring the current library copy of each song.
    pub fn playlist(&self, id: &str) -> Result<Option<Playlist>> {
        self.read(|c| {
            let row: Option<String> = c.query_row("SELECT data FROM playlist WHERE id = ?1", [id], |r| r.get(0)).optional()?;
            let Some(data) = row else { return Ok(None) };
            let mut playlist: Playlist = serde_json::from_str(&data).map_err(json_err)?;
            let mut st = c.prepare_cached(
                "SELECT coalesce(s.data, p.data) FROM playlist_song p LEFT JOIN song s ON s.id = p.song_id
                 WHERE p.playlist_id = ?1 ORDER BY p.position",
            )?;
            let rows = st.query_map([id], |r| r.get::<_, String>(0))?;
            for row in rows {
                playlist.entry.push(serde_json::from_str::<Song>(&row?).map_err(json_err)?);
            }
            Ok(Some(playlist))
        })
    }

    /// One playlist's list entry, without reading its songs.
    pub fn playlist_header(&self, id: &str) -> Result<Option<Playlist>> {
        self.read(|c| {
            let row: Option<String> = c.query_row("SELECT data FROM playlist WHERE id = ?1", [id], |r| r.get(0)).optional()?;
            row.map(|data| serde_json::from_str::<Playlist>(&data).map_err(json_err)).transpose()
        })
    }

    /// What a sync compares against the server's list: changed stamp, song count, cover id.
    pub(crate) fn playlist_marker(&self, id: &str) -> Result<Option<(Option<String>, i64, Option<String>)>> {
        self.read(|c| Ok(c.query_row("SELECT changed, song_count, json_extract(data, '$.coverArt') FROM playlist WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?))
    }

    pub(crate) fn store_playlist(&self, playlist: &Playlist) -> Result<()> {
        self.store_playlist_rows(playlist)?;
        self.contents_changed();
        Ok(())
    }

    fn store_playlist_rows(&self, playlist: &Playlist) -> Result<()> {
        self.write(|tx| {
            let mut header = playlist.clone();
            header.entry.clear();
            let data = serde_json::to_string(&header).map_err(json_err)?;
            tx.execute(
                "INSERT INTO playlist (id, name, sort_name, song_count, changed, readonly, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(id) DO UPDATE SET name=excluded.name, sort_name=excluded.sort_name, song_count=excluded.song_count, changed=excluded.changed, readonly=excluded.readonly, data=excluded.data",
                params![playlist.id, playlist.name, normalize(&playlist.name), playlist.song_count.unwrap_or(playlist.entry.len() as i64), playlist.changed, playlist.readonly.unwrap_or(false), data],
            )?;
            tx.execute("DELETE FROM playlist_song WHERE playlist_id = ?1", [&playlist.id])?;
            let mut st = tx.prepare_cached("INSERT INTO playlist_song (playlist_id, position, song_id, data) VALUES (?1, ?2, ?3, ?4)")?;
            for (position, song) in playlist.entry.iter().enumerate() {
                st.execute(params![playlist.id, position as i64, song.id, serde_json::to_string(song).map_err(json_err)?])?;
            }
            Ok(())
        })
    }

    /// Store a playlist's list entry without touching its stored songs.
    pub(crate) fn store_playlist_header(&self, playlist: &Playlist) -> Result<()> {
        let data = serde_json::to_string(&Playlist { entry: Vec::new(), ..playlist.clone() }).map_err(json_err)?;
        self.write(|tx| {
            tx.execute(
                "INSERT INTO playlist (id, name, sort_name, song_count, changed, readonly, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(id) DO UPDATE SET name=excluded.name, sort_name=excluded.sort_name, song_count=excluded.song_count, changed=excluded.changed, readonly=excluded.readonly, data=excluded.data",
                params![playlist.id, playlist.name, normalize(&playlist.name), playlist.song_count.unwrap_or(0), playlist.changed, playlist.readonly.unwrap_or(false), data],
            )?;
            Ok(())
        })
    }

    pub(crate) fn set_playlist_markers(&self, list: &[Playlist]) -> Result<()> {
        self.write(|tx| {
            let mut st = tx.prepare_cached("UPDATE playlist SET changed = ?1, song_count = coalesce(?2, song_count) WHERE id = ?3")?;
            for p in list {
                st.execute(params![p.changed, p.song_count, p.id])?;
            }
            Ok(())
        })
    }

    pub(crate) fn delete_playlist(&self, id: &str) -> Result<()> {
        let removed = self.write(|tx| Ok(tx.execute("DELETE FROM playlist WHERE id = ?1", [id])?))?;
        if removed > 0 {
            self.contents_changed();
        }
        Ok(())
    }

    /// Remove stored playlists the server no longer lists. Returns how many went.
    pub(crate) fn delete_playlists_except(&self, keep: &[String]) -> Result<u64> {
        self.write(|tx| {
            tx.execute("CREATE TEMP TABLE IF NOT EXISTS playlist_keep (id TEXT PRIMARY KEY)", [])?;
            tx.execute("DELETE FROM playlist_keep", [])?;
            {
                let mut st = tx.prepare_cached("INSERT OR IGNORE INTO playlist_keep VALUES (?1)")?;
                for id in keep {
                    st.execute([id])?;
                }
            }
            Ok(tx.execute("DELETE FROM playlist WHERE id NOT IN (SELECT id FROM playlist_keep)", [])? as u64)
        })
        .inspect(|&removed| {
            if removed > 0 {
                self.contents_changed();
            }
        })
    }

    // ---- stars and play counts ----------------------------------------------------------

    /// Make the local stars match the server's list exactly.
    pub(crate) fn apply_starred(&self, starred: &Starred) -> Result<AnnotationChanges> {
        self.write(|tx| {
            let songs = apply_stars(tx, "song", starred.song.iter().map(|s| (s.id.as_str(), s.starred.as_deref())))?;
            let albums = apply_stars(tx, "album", starred.album.iter().map(|a| (a.id.as_str(), a.starred.as_deref())))?;
            let artists = apply_stars(tx, "artist", starred.artist.iter().map(|a| (a.id.as_str(), a.starred.as_deref())))?;
            Ok(AnnotationChanges { songs, albums, artists })
        })
    }

    /// True if the server's copy of this album's plays differs from the local one.
    pub(crate) fn album_annotation_differs(&self, album: &Album) -> Result<bool> {
        self.read(|c| {
            let row: Option<(i64, Option<String>)> =
                c.query_row("SELECT play_count, played FROM album WHERE id = ?1", [&album.id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
            Ok(match row {
                Some((count, played)) => count != album.play_count.unwrap_or(0) || played != album.played,
                None => false,
            })
        })
    }

    /// Update an album and its songs from a freshly fetched album. Returns the songs changed.
    pub(crate) fn apply_album_annotations(&self, album: &Album) -> Result<u64> {
        self.write(|tx| {
            set_annotation(tx, "album", &album.id, album.play_count, album.played.as_deref())?;
            let mut changed = 0;
            for song in &album.song {
                changed += set_annotation(tx, "song", &song.id, song.play_count, song.played.as_deref())?;
            }
            Ok(changed)
        })
    }

    /// A play just reached the server: count it locally so it shows before the next refresh.
    pub fn record_play(&self, song_id: &str, played_at_ms: u64) -> Result<()> {
        self.write(|tx| {
            let played = iso8601_ms(played_at_ms);
            let album_id: Option<String> = tx.query_row("SELECT album_id FROM song WHERE id = ?1", [song_id], |r| r.get(0)).optional()?.flatten();
            bump_plays(tx, "song", song_id, &played)?;
            if let Some(album) = album_id {
                bump_plays(tx, "album", &album, &played)?;
            }
            Ok(())
        })
    }

    /// Set or clear a local star after the server accepted the change.
    pub fn set_starred(&self, table: StarTable, id: &str, on: bool) -> Result<()> {
        let table = table.name();
        let stamp = on.then(|| iso8601_ms(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)));
        self.write(|tx| {
            let sql = if stamp.is_some() {
                format!("UPDATE {table} SET starred = ?1, data = json_set(data, '$.starred', ?1) WHERE id = ?2")
            } else {
                format!("UPDATE {table} SET starred = NULL, data = json_remove(data, '$.starred') WHERE ?1 IS NULL AND id = ?2")
            };
            tx.execute(&sql, params![stamp, id])?;
            Ok(())
        })
    }

    // ---- unsent plays -------------------------------------------------------------------

    pub fn unsent_plays(&self) -> Result<Vec<UnsentPlay>> {
        self.read(|c| {
            let mut st = c.prepare_cached("SELECT seq, song_id, played_at_ms FROM unsent_play ORDER BY seq")?;
            let rows = st.query_map([], |r| Ok(UnsentPlay { seq: r.get(0)?, song_id: r.get(1)?, played_at_ms: r.get::<_, i64>(2)? as u64 }))?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    pub fn push_unsent_play(&self, song_id: &str, played_at_ms: u64, max_kept: u32) -> Result<i64> {
        self.write(|tx| {
            tx.execute("INSERT INTO unsent_play (song_id, played_at_ms) VALUES (?1, ?2)", params![song_id, played_at_ms as i64])?;
            let seq = tx.last_insert_rowid();
            tx.execute("DELETE FROM unsent_play WHERE seq <= ?1 - ?2", params![seq, max_kept])?;
            Ok(seq)
        })
    }

    pub fn remove_unsent_play(&self, seq: i64) -> Result<()> {
        self.write(|tx| {
            tx.execute("DELETE FROM unsent_play WHERE seq = ?1", [seq])?;
            Ok(())
        })
    }
}

/// Reconcile one table's stars with the server's list.
fn apply_stars<'a>(tx: &Transaction, table: &str, items: impl Iterator<Item = (&'a str, Option<&'a str>)>) -> Result<u64> {
    tx.execute_batch("CREATE TEMP TABLE IF NOT EXISTS star_incoming (id TEXT PRIMARY KEY, starred TEXT); DELETE FROM star_incoming;")?;
    {
        let mut st = tx.prepare_cached("INSERT OR REPLACE INTO star_incoming VALUES (?1, ?2)")?;
        for (id, starred) in items {
            st.execute(params![id, starred])?;
        }
    }
    let removed = tx.execute(&format!("UPDATE {table} SET starred = NULL, data = json_remove(data, '$.starred') WHERE starred IS NOT NULL AND id NOT IN (SELECT id FROM star_incoming)"), [])?;
    let added = tx.execute(
        &format!(
            "UPDATE {table} SET starred = t.starred, data = json_set(data, '$.starred', t.starred)
             FROM star_incoming t WHERE {table}.id = t.id AND {table}.starred IS NOT t.starred"
        ),
        [],
    )?;
    Ok((removed + added) as u64)
}

/// Set one row's play count and last-played. Returns how many rows changed.
fn set_annotation(tx: &Transaction, table: &str, id: &str, play_count: Option<i64>, played: Option<&str>) -> Result<u64> {
    let count = play_count.unwrap_or(0);
    Ok(tx.execute(
        &format!(
            "UPDATE {table} SET play_count = ?1, played = ?2,
                data = CASE WHEN ?2 IS NULL THEN json_remove(json_set(data, '$.playCount', ?1), '$.played')
                       ELSE json_set(data, '$.playCount', ?1, '$.played', ?2) END
             WHERE id = ?3 AND (play_count != ?1 OR played IS NOT ?2)"
        ),
        params![count, played, id],
    )? as u64)
}

fn bump_plays(tx: &Transaction, table: &str, id: &str, played: &str) -> Result<()> {
    tx.execute(
        &format!(
            "UPDATE {table} SET play_count = play_count + 1, played = ?1,
                data = json_set(data, '$.playCount', play_count + 1, '$.played', ?1) WHERE id = ?2"
        ),
        params![played, id],
    )?;
    Ok(())
}

/// Milliseconds since the Unix epoch to `YYYY-MM-DDTHH:MM:SS.mmmZ`.
pub(super) fn iso8601_ms(ms: u64) -> String {
    let secs = ms / 1000;
    let days = secs / 86_400;
    let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    let (year, month, day) = civil_from_days(days as i64);
    format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}.{:03}Z", ms % 1000)
}

/// Days since 1970-01-01 to a calendar date (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_dates() {
        assert_eq!(iso8601_ms(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso8601_ms(1_700_000_000_123), "2023-11-14T22:13:20.123Z");
    }
}

impl crate::reporting::PlayStore for Library {
    fn load(&self) -> Vec<(i64, String, u64)> {
        self.unsent_plays()
            .map(|rows| rows.into_iter().map(|p| (p.seq, p.song_id, p.played_at_ms)).collect())
            .unwrap_or_else(|e| {
                tracing::warn!("couldn't read unsent plays: {e}");
                Vec::new()
            })
    }

    fn add(&self, song_id: &str, played_at_ms: u64) -> Option<i64> {
        match self.push_unsent_play(song_id, played_at_ms, MAX_UNSENT_PLAYS) {
            Ok(seq) => Some(seq),
            Err(e) => {
                tracing::warn!("couldn't store an unsent play: {e}");
                None
            }
        }
    }

    fn remove(&self, seq: i64) {
        if let Err(e) = self.remove_unsent_play(seq) {
            tracing::warn!("couldn't clear a sent play: {e}");
        }
    }

    fn played(&self, song_id: &str, played_at_ms: u64) {
        if let Err(e) = self.record_play(song_id, played_at_ms) {
            tracing::warn!("couldn't record a play locally: {e}");
        }
    }

    fn play_threshold(&self) -> Option<crate::reporting::PlayThreshold> {
        self.meta(PLAY_THRESHOLD_KEY).ok().flatten().and_then(|v| crate::reporting::PlayThreshold::from_stored(&v))
    }

    fn save_play_threshold(&self, threshold: crate::reporting::PlayThreshold) -> Result<()> {
        self.set_meta(PLAY_THRESHOLD_KEY, Some(&threshold.to_stored()))
    }
}

/// Library meta key for this account's [`crate::reporting::PlayThreshold`].
const PLAY_THRESHOLD_KEY: &str = "reporting.threshold";
