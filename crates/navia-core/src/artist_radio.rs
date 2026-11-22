//! Artist Radio: Deezer's artist radio matched to the library, filled up with the server's own radio.
//! Deezer is only asked when [`crate::deezer::gate`] is open, and a failure falls back silently.
//! Tracks match library songs by name, then by ISRC for a bounded number of misses.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use tokio::time::Instant;

use crate::autoplay::song_version;
use crate::client::{Client, Song};
pub use crate::deezer::{Deezer, DeezerError, DeezerTrack, Gate};
use crate::lanyard::matching::{find_named_or_bare, key, names};
use crate::library::Library;

/// Songs per radio, and how many the server's radio is asked for.
pub const RADIO_LEN: usize = 50;
/// Radio calls per start at most. Deezer mixes differently on each call, so a second call can add matches.
const RADIO_CALLS: usize = 2;
/// ISRC lookups per radio at most (one request each).
const ISRC_LOOKUPS: usize = 10;

impl Deezer {
    /// Deezer's radio for an artist as library songs, in radio order without repeats or other versions.
    /// Errors only when Deezer gave nothing usable.
    pub async fn library_radio(&self, db: &Library, artist_name: &str) -> Result<DeezerRadio, DeezerError> {
        let deadline = Instant::now() + self.budget;
        let Some(artist) = self.artist_id(db, artist_name, deadline).await? else {
            return Ok(DeezerRadio { found_artist: false, ..Default::default() });
        };
        let owned = owned_artists(db);
        let (mut tracks, mut calls) = match self.cached_radio(artist) {
            Some(tracks) => (tracks, RADIO_CALLS),
            None => (self.radio_call(artist, deadline).await?, 1),
        };
        let mut matches = match_by_name(db, &tracks, &owned);
        while calls < RADIO_CALLS && matches.iter().flatten().count() < RADIO_LEN && deadline.saturating_duration_since(Instant::now()) > Duration::from_secs(1) {
            calls += 1;
            match self.radio_call(artist, deadline).await {
                Ok(more) if more.len() > tracks.len() => {
                    tracks = more;
                    matches = match_by_name(db, &tracks, &owned);
                }
                _ => break,
            }
        }
        // Look up ISRCs only while names left the radio short, and only for artists the library has.
        let short = matches.iter().flatten().count() < RADIO_LEN;
        let wanted: Vec<u64> = tracks.iter().zip(&matches).filter(|(t, m)| short && m.is_none() && artist_owned(&owned, t)).map(|(t, _)| t.id).take(ISRC_LOOKUPS).collect();
        let mut isrcs: HashMap<u64, String> = HashMap::new();
        if !wanted.is_empty() {
            let found = futures_util::future::join_all(wanted.iter().map(|id| self.isrc(*id, deadline))).await;
            isrcs = wanted.into_iter().zip(found).filter_map(|(id, isrc)| Some((id, isrc?))).collect();
        }
        let mut by_isrc = 0;
        if !isrcs.is_empty() {
            let list: Vec<String> = isrcs.values().cloned().collect();
            let songs = db.songs_with_isrc(&list).unwrap_or_default();
            for (track, slot) in tracks.iter().zip(matches.iter_mut()) {
                if slot.is_some() {
                    continue;
                }
                let Some(isrc) = isrcs.get(&track.id) else { continue };
                // Prefer the same album, else the most played song with that recording.
                let album = key(&track.album.title);
                let hits: Vec<&(u32, Song, String)> = songs.iter().filter(|(_, _, i)| i == isrc).collect();
                let pick = hits.iter().find(|(_, s, _)| key(s.album.as_deref().unwrap_or_default()) == album).or(hits.first());
                if let Some((_, song, _)) = pick {
                    *slot = Some(song.clone());
                    by_isrc += 1;
                }
            }
        }
        let mut songs = Vec::new();
        let mut track_ids = Vec::new();
        let mut taken = Taken::default();
        for (track, song) in tracks.iter().zip(matches) {
            if let Some(song) = song.filter(|s| taken.take(s)) {
                songs.push(song);
                track_ids.push(track.id);
            }
        }
        Ok(DeezerRadio { found_artist: true, tracks: tracks.len(), by_isrc, songs, track_ids })
    }
}

/// What Deezer's radio gave for one start.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeezerRadio {
    /// Deezer has an artist by this name.
    pub found_artist: bool,
    /// Tracks in the radio.
    pub tracks: usize,
    /// Of `songs`, how many were found by ISRC.
    pub by_isrc: usize,
    /// The library's songs for them, in the radio's order.
    pub songs: Vec<Song>,
    /// The Deezer track each of `songs` stands for.
    pub track_ids: Vec<u64>,
}

/// Where a radio's songs came from, for tests and logs.
#[derive(Clone, Debug, PartialEq)]
pub enum DeezerUse {
    /// The gate was closed, so Deezer wasn't asked.
    NotAllowed(Gate),
    /// Deezer failed and the server's radio was used.
    Failed(DeezerError),
    Used(DeezerRadio),
}

/// An Artist Radio of library songs, Deezer's first.
#[derive(Clone, Debug, PartialEq)]
pub struct ArtistRadio {
    pub songs: Vec<Song>,
    /// How many songs at the start of `songs` came from Deezer.
    pub from_deezer: usize,
    pub deezer: DeezerUse,
}

/// Songs already in the radio, by id and by [`song_version`].
#[derive(Default)]
struct Taken {
    ids: HashSet<String>,
    versions: HashSet<String>,
}

impl Taken {
    fn take(&mut self, song: &Song) -> bool {
        let version = song_version(song);
        if self.ids.contains(&song.id) || (!version.is_empty() && self.versions.contains(&version)) {
            return false;
        }
        self.ids.insert(song.id.clone());
        self.versions.insert(version);
        true
    }
}

/// The library's artist names, as [`key`]s (every name of a joined credit).
pub fn owned_artists(db: &Library) -> HashSet<String> {
    let mut out = HashSet::new();
    for artist in db.list_artists(0, u32::MAX).unwrap_or_default() {
        out.insert(key(&artist.name));
        out.extend(names(&artist.name));
    }
    out
}

/// Whether the library has the track's artist, without which it can't have the song.
fn artist_owned(owned: &HashSet<String>, track: &DeezerTrack) -> bool {
    owned.contains(&key(&track.artist.name)) || names(&track.artist.name).iter().any(|n| owned.contains(n))
}

/// each track's library song by name (the names as given, then bare: `find_named_or_bare`), or
/// `None`.
pub(crate) fn match_by_name(db: &Library, tracks: &[DeezerTrack], owned: &HashSet<String>) -> Vec<Option<Song>> {
    tracks
        .iter()
        .map(|t| {
            if !artist_owned(owned, t) {
                return None;
            }
            find_named_or_bare(db, &t.title, &[t.artist.name.as_str()], &t.album.title, t.duration.map(|d| d * 1000)).map(|(_, s)| s)
        })
        .collect()
}

/// Artist Radio for a library artist, from Deezer's radio and the server's `getSimilarSongs2`.
/// Needs a Tokio runtime.
pub async fn artist_radio(client: &Client, db: &Library, deezer: &Deezer, artist_id: &str, artist_name: &str) -> ArtistRadio {
    let gate = crate::deezer::gate(db);
    let name = if artist_name.trim().is_empty() { db.artist(artist_id).ok().flatten().map(|a| a.name).unwrap_or_default() } else { artist_name.to_string() };
    // Ask the server right away, and don't wait for it if Deezer fills the radio.
    let server = {
        let (client, id) = (client.clone(), artist_id.to_string());
        tokio::spawn(async move { client.similar_songs2(&id, RADIO_LEN as u32).await.unwrap_or_default() })
    };
    let deezer_answer = if gate.is_open() && !name.trim().is_empty() { Some(deezer.library_radio(db, &name).await) } else { None };
    let (first, used) = match deezer_answer {
        None => (Vec::new(), DeezerUse::NotAllowed(gate)),
        Some(Err(e)) => {
            tracing::info!(error = %e, "Artist Radio: Deezer gave nothing, using the server's radio");
            (Vec::new(), DeezerUse::Failed(e))
        }
        Some(Ok(radio)) => (radio.songs.clone(), DeezerUse::Used(radio)),
    };
    // Like Autoplay, skip short tracks and instrumental versions.
    let junk = crate::engine::junk::Filter::default();
    let mut taken = Taken::default();
    let mut songs = Vec::with_capacity(RADIO_LEN);
    for song in first {
        if songs.len() < RADIO_LEN && !junk.leaves_out(&song) && taken.take(&song) {
            songs.push(song);
        }
    }
    let from_deezer = songs.len();
    let server = if songs.len() < RADIO_LEN {
        server.await.unwrap_or_default()
    } else {
        server.abort();
        Vec::new()
    };
    for song in server {
        if songs.len() < RADIO_LEN && !junk.leaves_out(&song) && taken.take(&song) {
            songs.push(song);
        }
    }
    // The Navia mode spaces artists and albums like Autoplay does.
    if crate::engine::mode(db) == crate::engine::AutoplayMode::Navia {
        use crate::engine::sequence;
        let items: Vec<sequence::Item> = songs.iter().map(|s| sequence::Item::of(s)).collect();
        songs = sequence::respace(&items, &[], items.len()).into_iter().map(|i| songs[i].clone()).collect();
    }
    if let DeezerUse::Used(radio) = &used {
        tracing::info!(tracks = radio.tracks, matched = radio.songs.len(), by_isrc = radio.by_isrc, from_deezer, total = songs.len(), "Artist Radio from Deezer");
    }
    ArtistRadio { songs, from_deezer, deezer: used }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taken_drops_repeats_and_other_versions() {
        let song = |id: &str, title: &str| Song { id: id.into(), title: title.into(), artist: Some("A".into()), ..Default::default() };
        let mut taken = Taken::default();
        assert!(taken.take(&song("1", "Song")));
        assert!(!taken.take(&song("1", "Song")));
        assert!(!taken.take(&song("2", "Song - Remastered 2011")));
        assert!(taken.take(&song("3", "Other")));
    }

    #[test]
    fn tracks_read_leniently() {
        let t: DeezerTrack = serde_json::from_value(serde_json::json!({"id": 5, "title": "T", "duration": 200, "artist": {"id": 1, "name": "A", "picture": "x"}, "album": {"title": "Al", "cover": "y"}})).unwrap();
        assert_eq!((t.id, t.duration, t.artist.name.as_str(), t.album.title.as_str()), (5, Some(200), "A", "Al"));
        let bare: DeezerTrack = serde_json::from_value(serde_json::json!({"id": 6})).unwrap();
        assert!(bare.title.is_empty() && bare.duration.is_none());
    }
}

