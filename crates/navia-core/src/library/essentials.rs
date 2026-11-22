//! home's Essentials: an artist's top songs as the server's integrations rank them (`getTopSongs`),
//! for the artists this user has really played, best lately first. each
//! artist's answer is kept for the local day, so home shows what's stored at once and a relaunch
//! doesn't ask again.

use std::collections::HashSet;

use std::sync::atomic::{AtomicUsize, Ordering};

use futures_util::StreamExt;
use rusqlite::{OptionalExtension, params};

use super::Library;
use crate::autoplay::song_version;
use crate::client::Client;
use crate::error::Result;

/// tiles at most.
pub const ESSENTIALS: usize = 12;
/// the fewest library songs that make a tile.
pub const ESSENTIALS_MIN: usize = 8;
/// how far down the top artists to look, so the work stays bounded.
pub const LOOK_AT: usize = 80;
/// artists asked about at once. each answer can mean an external lookup on the server.
pub const ASK_AT_ONCE: usize = 3;
/// songs asked for. the count applies before the server matches its library, so ask for plenty.
pub const TOP_COUNT: u32 = 50;

/// a tile: the artist and their top songs in the library, without other versions of a song.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Essential {
    pub artist_id: String,
    pub songs: Vec<u32>,
}

impl Library {
    /// an artist's top song ids as the server gave them today, if it was asked today.
    pub fn top_songs_for_day(&self, artist_id: &str, local_day: i64) -> Result<Option<Vec<String>>> {
        let stored: Option<(i64, String)> = self.read(|c| Ok(c.query_row("SELECT day, ids FROM top_songs WHERE artist_id = ?1", [artist_id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?))?;
        Ok(stored.filter(|(day, _)| *day == local_day).map(|(_, ids)| serde_json::from_str(&ids).unwrap_or_default()))
    }

    pub fn store_top_songs(&self, artist_id: &str, local_day: i64, ids: &[String]) -> Result<()> {
        let json = serde_json::to_string(ids).unwrap_or_else(|_| "[]".into());
        self.write_quiet(|tx| {
            tx.execute(
                "INSERT INTO top_songs (artist_id, day, ids) VALUES (?1, ?2, ?3)
                 ON CONFLICT(artist_id) DO UPDATE SET day = excluded.day, ids = excluded.ids",
                params![artist_id, local_day, json],
            )?;
            Ok(())
        })
    }

    /// library rows for top song ids, in order, each song once.
    pub fn top_song_rows(&self, ids: &[String]) -> Result<Vec<u32>> {
        let mut seen = HashSet::new();
        Ok(self.song_rowids(ids)?.into_iter().filter(|r| seen.insert(*r)).collect())
    }

    /// Essentials from what's stored for today, without asking the server.
    pub fn essentials(&self, local_day: i64) -> Result<Vec<Essential>> {
        let mut out = Vec::new();
        for a in self.taste()?.artists.iter().take(LOOK_AT) {
            if out.len() == ESSENTIALS {
                break;
            }
            if let Some(ids) = self.top_songs_for_day(&a.id, local_day)?
                && let Some(e) = self.essential(&a.id, &ids)?
            {
                out.push(e);
            }
        }
        Ok(out)
    }

    fn essential(&self, artist_id: &str, ids: &[String]) -> Result<Option<Essential>> {
        let rows = self.top_song_rows(ids)?;
        let mut versions = HashSet::new();
        let songs: Vec<u32> = self.songs_by_rowid(&rows)?.into_iter().filter(|(_, s)| versions.insert(song_version(s))).map(|(r, _)| r).collect();
        Ok((songs.len() >= ESSENTIALS_MIN).then(|| Essential { artist_id: artist_id.to_string(), songs }))
    }
}

/// an artist's top song ids: today's stored answer, else the server's (read-only), kept for the
/// day. a failed ask isn't kept, so it's tried again.
pub async fn top_songs(client: &Client, library: &Library, artist_id: &str, artist_name: &str, by_id: bool, local_day: i64) -> Result<Vec<String>> {
    // an empty answer is asked again: the server can come up empty for a while, such as before its agents answer.
    if let Some(ids) = library.top_songs_for_day(artist_id, local_day)?.filter(|ids| !ids.is_empty()) {
        return Ok(ids);
    }
    let songs = client.artist_top_songs(artist_id, artist_name, by_id, TOP_COUNT).await?;
    let mut seen = HashSet::new();
    let ids: Vec<String> = songs.into_iter().map(|s| s.id).filter(|id| seen.insert(id.clone())).collect();
    library.store_top_songs(artist_id, local_day, &ids)?;
    Ok(ids)
}

/// Essentials for today, asking the server about artists not asked yet today, [`ASK_AT_ONCE`] at
/// a time, until [`ESSENTIALS`] tiles are found or [`LOOK_AT`] artists were looked at. the tiles are
/// the same as asking one artist at a time.
pub async fn refresh_essentials(client: &Client, library: &Library, by_id: bool, local_day: i64) -> Result<Vec<Essential>> {
    let taste = library.taste()?;
    let mut artists = Vec::new();
    for a in taste.artists.iter().take(LOOK_AT) {
        if let Some(artist) = library.artist(&a.id)? {
            artists.push((a.id.clone(), artist.name));
        }
    }
    // answers come back in order, so an artist starting after the tiles are full is never needed.
    let found = AtomicUsize::new(0);
    let futures: Vec<_> = artists.iter().map(|(id, name)| try_artist(client, library, id, name, by_id, local_day, &found)).collect();
    let mut answers = futures_util::stream::iter(futures).buffered(ASK_AT_ONCE);
    let mut out = Vec::new();
    while let Some(answer) = answers.next().await {
        if let Some(e) = answer
            && out.len() < ESSENTIALS
        {
            out.push(e);
            found.fetch_add(1, Ordering::SeqCst);
        }
    }
    Ok(out)
}

async fn try_artist(client: &Client, library: &Library, id: &str, name: &str, by_id: bool, local_day: i64, found: &AtomicUsize) -> Option<Essential> {
    if found.load(Ordering::SeqCst) >= ESSENTIALS {
        return None;
    }
    match top_songs(client, library, id, name, by_id, local_day).await {
        Ok(ids) => library.essential(id, &ids).ok().flatten(),
        Err(e) => {
            tracing::debug!("top songs for {id}: {e}");
            None
        }
    }
}
