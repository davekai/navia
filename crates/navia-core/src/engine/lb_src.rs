//! ListenBrainz as a source for the Navia mode, only when
//! [`crate::deezer::gate`] is open: each anchor's similar recordings ([`crate::listenbrainz`])
//! mapped to library songs, by recording MBID first, then by name.

use std::collections::{HashMap, HashSet};

use tokio::time::Instant;

use crate::artist_radio::owned_artists;
use crate::client::Song;
use crate::lanyard::matching::{bare_title, find_named_or_bare, first_credited, key, names};
use crate::library::Library;
use crate::listenbrainz::{ListenBrainz, Recording};

/// anchors asked about per context at most.
pub const MAX_ANCHORS: usize = 3;

/// the credit and title an anchor is looked up by: the whole credit as shown.
fn credit(song: &Song) -> String {
    song.display_artist.clone().or_else(|| song.artist.clone()).unwrap_or_else(|| crate::autoplay::main_artist(song).to_string())
}

/// the recording MBID of each seed: its tag, else `acr-lookup` by the names as given, then bare.
async fn mbids(lb: &ListenBrainz, seeds: &[Song], deadline: Instant) -> Vec<Option<String>> {
    let mut out: Vec<Option<String>> = seeds.iter().map(|s| s.music_brainz_id.clone().filter(|m| !m.trim().is_empty())).collect();
    let exact: Vec<(usize, (String, String))> = seeds.iter().enumerate().filter(|(i, _)| out[*i].is_none()).map(|(i, s)| (i, (credit(s), s.title.clone()))).collect();
    if exact.is_empty() {
        return out;
    }
    let found = lb.recording_mbids(&exact.iter().map(|(_, p)| p.clone()).collect::<Vec<_>>(), deadline).await;
    for ((i, _), m) in exact.iter().zip(found) {
        out[*i] = m;
    }
    let bare: Vec<(usize, (String, String))> = exact
        .iter()
        .filter_map(|(i, given)| {
            let pair = (first_credited(&given.0).to_string(), bare_title(&given.1).to_string());
            (out[*i].is_none() && !pair.0.is_empty() && !pair.1.is_empty() && pair != *given).then_some((*i, pair))
        })
        .collect();
    if !bare.is_empty() {
        let found = lb.recording_mbids(&bare.iter().map(|(_, p)| p.clone()).collect::<Vec<_>>(), deadline).await;
        for ((i, _), m) in bare.iter().zip(found) {
            out[*i] = m;
        }
    }
    out
}

/// recordings mapped to library songs, in their order, once each: by recording MBID, then by name
/// for artists the library has.
pub fn map_to_library(db: &Library, recordings: &[Recording], owned: &HashSet<String>) -> Vec<Song> {
    let wanted: Vec<String> = recordings.iter().map(|r| r.recording_mbid.clone()).filter(|m| !m.is_empty()).collect();
    let mut by_mbid: HashMap<String, Song> = HashMap::new();
    for (_, song, mbid) in db.songs_with_mbid(&wanted).unwrap_or_default() {
        by_mbid.entry(mbid).or_insert(song);
    }
    let mut seen = HashSet::new();
    recordings
        .iter()
        .filter_map(|r| {
            by_mbid.get(&r.recording_mbid.to_ascii_lowercase()).cloned().or_else(|| {
                let artist = r.artist_credit_name.as_str();
                let known = owned.contains(&key(artist)) || names(artist).iter().any(|n| owned.contains(n));
                known.then(|| find_named_or_bare(db, &r.recording_name, &[artist], r.release_name.as_deref().unwrap_or_default(), None).map(|(_, s)| s)).flatten()
            })
        })
        .filter(|s| seen.insert(s.id.clone()))
        .collect()
}

/// each seed's similar recordings as library songs (up to [`MAX_ANCHORS`] seeds), within
/// `deadline`. a seed it can't answer is left out.
pub async fn seed_sources(db: &Library, lb: &ListenBrainz, seeds: &[Song], deadline: Instant) -> Vec<(String, Vec<Song>)> {
    let seeds = &seeds[..seeds.len().min(MAX_ANCHORS)];
    let ids = mbids(lb, seeds, deadline).await;
    let asks = seeds.iter().zip(&ids).filter_map(|(s, m)| Some((s, m.clone()?)));
    let answers = futures_util::future::join_all(asks.map(|(seed, mbid)| async move { (seed, lb.similar(&mbid, deadline).await) })).await;
    let owned = owned_artists(db);
    answers
        .into_iter()
        .filter_map(|(seed, found)| {
            let songs: Vec<Song> = map_to_library(db, &found?, &owned).into_iter().filter(|s| s.id != seed.id).collect();
            (!songs.is_empty()).then(|| (seed.id.clone(), songs))
        })
        .collect()
}
