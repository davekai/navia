//! This account's dated plays, and "Your Top Songs" from them.
//! Dated plays come from Navidrome's scrobble history, which only the native API exposes. Without
//! it, ranking falls back to last-played dates and all-time play counts.

use rusqlite::params;

use super::Library;
use super::taste::now_unix;
use crate::client::Client;
use crate::client::native::ScrobbleEntry;
use crate::error::Result;

/// How far back "recent" reaches, about six months.
pub const TOP_SONGS_WINDOW_DAYS: i64 = 183;
/// How much dated history is kept. "Back in Rotation" looks well past the Top Songs window.
pub const HISTORY_DAYS: i64 = 730;
const HISTORY_SECS: i64 = HISTORY_DAYS * 86_400;
/// Each fetch logs in and moves the account's last-login time on the server, so fetches are spaced out.
const MIN_REFETCH_SECS: i64 = 6 * 3600;
/// A cap well beyond two years of plays, so a misbehaving server can't fill the disk.
const MAX_HISTORY_ROWS: usize = 200_000;

pub(super) const META_FETCHED_AT: &str = "play_history_fetched_at";
const META_PLAYED_MARK: &str = "play_history_played_mark";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayHistoryRefresh {
    /// Fetched too recently, or nothing has been played since.
    Skipped,
    /// The server has no history route, so the fallback ranking applies.
    Unsupported,
    /// The window was fetched and this many plays stored.
    Stored(usize),
}

impl Library {
    /// Row ids for "Your Top Songs", at most `limit`, by recent-taste score. Never padded with
    /// songs from outside the window.
    pub fn top_song_rowids(&self, limit: u32) -> Result<Vec<u32>> {
        Ok(self.taste()?.top_songs(limit as usize))
    }

    pub fn top_song_rowids_at(&self, limit: u32, now: i64) -> Result<Vec<u32>> {
        Ok(self.taste_at(now)?.top_songs(limit as usize))
    }

    /// Replace the stored window. The whole window is fetched each time, so late or deleted
    /// scrobbles are corrected too.
    pub fn replace_recent_plays(&self, plays: &[ScrobbleEntry]) -> Result<()> {
        self.write(|tx| {
            tx.execute("DELETE FROM recent_play", [])?;
            let mut st = tx.prepare_cached("INSERT OR REPLACE INTO recent_play (id, song_id, played_at) VALUES (?1, ?2, ?3)")?;
            for play in plays {
                st.execute(params![play.id, play.song_id, play.played_at])?;
            }
            Ok(())
        })
    }

    fn newest_played(&self) -> Result<Option<String>> {
        self.read(|c| Ok(c.query_row("SELECT max(played) FROM song", [], |r| r.get(0))?))
    }
}

/// Bring the stored window up to date if plays have moved and it's been a while.
pub async fn refresh_play_history(client: &Client, library: &Library) -> Result<PlayHistoryRefresh> {
    refresh_play_history_at(client, library, now_unix()).await
}

/// a failure while the server has asked the client to wait (it limited or failed the request) is
/// left unstamped, so it's retried once that wait is over ([`Client::native_retry_in`]); any other
/// failure waits for the slow schedule, like a server without the history.
pub async fn refresh_play_history_at(client: &Client, library: &Library, now: i64) -> Result<PlayHistoryRefresh> {
    let fetched: Option<i64> = library.meta(META_FETCHED_AT)?.and_then(|v| v.parse().ok());
    let mark = library.newest_played()?;
    if let Some(fetched) = fetched
        && (now - fetched < MIN_REFETCH_SECS || library.meta(META_PLAYED_MARK)? == mark)
    {
        return Ok(PlayHistoryRefresh::Skipped);
    }
    let outcome = match fetch_window(client, library, now).await {
        Ok(outcome) => outcome,
        Err(e) => {
            if client.native_retry_in().is_none() {
                library.set_meta(META_FETCHED_AT, Some(&now.to_string()))?;
            }
            return Err(e);
        }
    };
    library.set_meta(META_FETCHED_AT, Some(&now.to_string()))?;
    library.set_meta(META_PLAYED_MARK, mark.as_deref())?;
    Ok(outcome)
}

async fn fetch_window(client: &Client, library: &Library, now: i64) -> Result<PlayHistoryRefresh> {
    if client.has_scrobble_history().await? {
        let plays = client.scrobbles_since(now - HISTORY_SECS, MAX_HISTORY_ROWS).await?;
        let stored = plays.len();
        library.blocking(move |l| l.replace_recent_plays(&plays)).await?;
        Ok(PlayHistoryRefresh::Stored(stored))
    } else {
        library.blocking(|l| l.replace_recent_plays(&[])).await?;
        Ok(PlayHistoryRefresh::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{MusicFolder, Song};
    use crate::library::LibraryConfig;

    const NOW: i64 = 1_800_000_000;
    const CUTOFF: i64 = NOW - TOP_SONGS_WINDOW_DAYS * 86_400;

    fn iso(unix: i64) -> String {
        crate::library::extras::iso8601_ms(unix as u64 * 1000)
    }

    fn song(id: &str, plays: i64, played: Option<i64>) -> Song {
        Song { id: id.into(), title: id.to_uppercase(), play_count: Some(plays), played: played.map(iso), ..Default::default() }
    }

    fn library(dir: &tempfile::TempDir, name: &str, songs: &[Song]) -> Library {
        let lib = Library::open(&LibraryConfig { path: dir.path().join(name) }).unwrap();
        lib.replace_libraries(&[MusicFolder { id: "1".into(), name: "L".into() }]).unwrap();
        let cursor = lib.begin_sync("1", None, false).unwrap();
        lib.apply_songs("1", &cursor, songs.len() as u32, songs).unwrap();
        lib
    }

    fn play(id: i64, song: &str, at: i64) -> ScrobbleEntry {
        ScrobbleEntry { id, song_id: song.into(), played_at: at }
    }

    fn ids(lib: &Library, limit: u32, now: i64) -> Vec<String> {
        let rows = lib.top_song_rowids_at(limit, now).unwrap();
        lib.songs_by_rowid(&rows).unwrap().into_iter().map(|(_, s)| s.id).collect()
    }

    #[test]
    fn real_plays_rank_by_plays_inside_the_window_not_all_time() {
        let dir = tempfile::tempdir().unwrap();
        let lib = library(&dir, "a", &[song("old-hit", 900, Some(NOW - 10)), song("lately", 4, Some(NOW - 20))]);
        lib.replace_recent_plays(&[play(1, "lately", NOW - 20), play(2, "lately", NOW - 30), play(3, "old-hit", NOW - 10)]).unwrap();
        assert_eq!(ids(&lib, 16, NOW), ["lately", "old-hit"]);
    }

    #[test]
    fn real_plays_window_boundary_is_inclusive() {
        let dir = tempfile::tempdir().unwrap();
        let lib = library(&dir, "a", &[song("on", 1, None), song("off", 1, None)]);
        lib.replace_recent_plays(&[play(1, "on", CUTOFF), play(2, "off", CUTOFF - 1)]).unwrap();
        assert_eq!(ids(&lib, 16, NOW), ["on"]);
        // A second later the play on the edge has left the window, with no new fetch.
        assert!(ids(&lib, 16, NOW + 1).is_empty());
    }

    #[test]
    fn fallback_uses_last_played_inside_the_window_and_is_inclusive() {
        let dir = tempfile::tempdir().unwrap();
        let lib = library(&dir, "a", &[song("edge", 2, Some(CUTOFF)), song("big", 50, Some(NOW - 5)), song("gone", 500, Some(CUTOFF - 1)), song("never", 0, None), song("no-date", 9, None)]);
        assert_eq!(ids(&lib, 16, NOW), ["big", "edge"]);
    }

    #[test]
    fn shows_fewer_rather_than_padding() {
        let dir = tempfile::tempdir().unwrap();
        let lib = library(&dir, "a", &[song("one", 3, Some(NOW - 9)), song("old", 800, Some(CUTOFF - 86_400))]);
        assert_eq!(ids(&lib, 16, NOW), ["one"]);
        let empty = library(&dir, "b", &[song("old", 800, Some(CUTOFF - 86_400)), song("never", 0, None)]);
        assert!(empty.top_song_rowids_at(16, NOW).unwrap().is_empty());
    }

    #[test]
    fn real_history_wins_even_when_it_has_fewer_songs_than_the_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let lib = library(&dir, "a", &[song("a", 40, Some(NOW - 5)), song("b", 30, Some(NOW - 6))]);
        lib.replace_recent_plays(&[play(1, "b", NOW - 6)]).unwrap();
        assert_eq!(ids(&lib, 16, NOW), ["b"]);
    }

    #[test]
    fn a_song_no_longer_in_the_library_drops_out() {
        let dir = tempfile::tempdir().unwrap();
        let lib = library(&dir, "a", &[song("kept", 1, Some(NOW - 5))]);
        lib.replace_recent_plays(&[play(1, "deleted", NOW - 1), play(2, "deleted", NOW - 2), play(3, "kept", NOW - 5)]).unwrap();
        assert_eq!(ids(&lib, 16, NOW), ["kept"]);
    }

    #[test]
    fn replacing_drops_plays_the_server_no_longer_has() {
        let dir = tempfile::tempdir().unwrap();
        let lib = library(&dir, "a", &[song("x", 1, None), song("y", 1, None)]);
        lib.replace_recent_plays(&[play(1, "x", NOW - 1), play(2, "x", NOW - 2), play(3, "y", NOW - 3)]).unwrap();
        lib.replace_recent_plays(&[play(3, "y", NOW - 3)]).unwrap();
        assert_eq!(ids(&lib, 16, NOW), ["y"]);
    }

    #[test]
    fn accounts_keep_separate_history() {
        let dir = tempfile::tempdir().unwrap();
        let songs = [song("x", 1, None), song("y", 1, None)];
        let (first, second) = (library(&dir, "first", &songs), library(&dir, "second", &songs));
        first.replace_recent_plays(&[play(1, "x", NOW - 1)]).unwrap();
        second.replace_recent_plays(&[play(1, "y", NOW - 1)]).unwrap();
        assert_eq!(ids(&first, 16, NOW), ["x"]);
        assert_eq!(ids(&second, 16, NOW), ["y"]);
    }
}
