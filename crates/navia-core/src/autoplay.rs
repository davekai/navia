//! Autoplay, which picks what plays once the queue runs out.
//! The Server mode uses `getSimilarSongs2` seeded with recent choices, and the Navia mode uses [`crate::engine`].
//! Apps keep the queue, and this module decides what Autoplay offers and when it takes over.

use std::collections::HashSet;

use crate::audio::player::Repeat;
use crate::client::{Client, Song};
use crate::engine::{self, AutoplayMode};
use crate::library::Library;

/// How many of the listener's latest choices seed Autoplay.
pub const SEED_COUNT: usize = 3;
/// Similar songs asked per seed in the Server mode.
pub const PER_SEED: u32 = 40;
/// How many of the listener's latest choices the Navia mode keeps to fill its seeds.
const CHOSEN_KEEP: usize = 12;
/// How many preceding songs decide which versions Autoplay may continue with.
const JUNK_CONTEXT: usize = 3;
/// how many Autoplay songs the queue shows; the plan goes on past them, so the next of it fills in
/// behind as songs play or are removed.
pub const SHOWN: usize = 25;

/// Why the queue's Autoplay section looks the way it does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutoplayState {
    /// Songs to show.
    On,
    /// Turned off in Settings.
    Off,
    /// Repeat is on, so the list never ends. Shows a note instead of songs.
    Repeat,
    /// A station is playing, so there's no section.
    Station,
    /// The list is over but the plan isn't ready. Shows the heading with placeholder rows.
    Finding,
    /// The plan is ready and has nothing to offer, so there's no section.
    Empty,
    /// The list still has songs after the current one. No section yet, but songs can be fetched meanwhile.
    Later,
}

/// The section's state. `list_left` counts the list's songs still to come, not songs the listener queued.
pub fn state(enabled: bool, repeat: Repeat, station: bool, has_songs: bool, list_left: usize, ready: bool) -> AutoplayState {
    if station {
        AutoplayState::Station
    } else if !enabled {
        AutoplayState::Off
    } else if repeat != Repeat::Off {
        AutoplayState::Repeat
    } else if list_left > 0 {
        AutoplayState::Later
    } else if !ready {
        AutoplayState::Finding
    } else if has_songs {
        AutoplayState::On
    } else {
        AutoplayState::Empty
    }
}

/// Whether Autoplay should hand the player its next song now.
/// True only when nothing the listener queued or started is left after the current song.
pub fn starts_now(enabled: bool, repeat: Repeat, station: bool, left_after_current: usize) -> bool {
    enabled && !station && repeat == Repeat::Off && left_after_current == 0
}

/// A key shared by versions of a song. It's the folded title up to any bracket or spaced dash, plus the main artist.
pub fn version_key(title: &str, artist: &str) -> String {
    let lower = title.to_lowercase();
    let mut cut = lower.len();
    for mark in ["(", "[", " - ", " – ", " — ", " feat.", " ft."] {
        if let Some(i) = lower.find(mark)
            && i > 0
        {
            cut = cut.min(i);
        }
    }
    let base: Vec<&str> = lower[..cut].split_whitespace().collect();
    format!("{}|{}", base.join(" "), artist.trim().to_lowercase())
}

/// The main artist, the first credited one or else the display artist.
pub fn main_artist(song: &Song) -> &str {
    song.artists.first().map(|a| a.name.as_str()).or(song.display_artist.as_deref()).or(song.artist.as_deref()).unwrap_or("")
}

/// [`version_key`] for a song.
pub fn song_version(song: &Song) -> String {
    version_key(&song.title, main_artist(song))
}

/// A song Autoplay must not offer: its id and its [`version_key`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Taken {
    pub id: String,
    pub version: String,
}

impl Taken {
    pub fn of(song: &Song) -> Self {
        Self { id: song.id.clone(), version: song_version(song) }
    }
}

/// Autoplay for one listening session.
/// Apps attach the library, report plays and choices, fetch when [`Autoplay::wanted_fetch`] asks,
/// and show [`Autoplay::songs`].
#[derive(Debug, Default)]
pub struct Autoplay {
    /// The library, for the mode setting, the play log and song details.
    lib: Option<Attached>,
    /// The mode in use while attached.
    mode: AutoplayMode,
    navia: engine::Navia,
    /// The listener's latest choices, newest first.
    chosen: Vec<String>,
    /// The queue context the anchors were taken for.
    context_key: Option<u64>,
    /// The songs last offered.
    offer: Vec<String>,
    heard: HashSet<String>,
    heard_versions: HashSet<String>,
    removed: HashSet<String>,
    removed_versions: HashSet<String>,
    /// The seeds wanted now, newest choice first.
    seeds: Vec<String>,
    /// The seeds `answers` were asked for.
    answered: Option<Vec<String>>,
    /// The seeds being asked for now.
    asking: Option<Vec<String>>,
    /// The server's answers, one list per seed, in seed order.
    answers: Vec<Vec<Song>>,
}

/// The attached library, wrapped because it has no `Debug`.
#[derive(Clone)]
struct Attached(Library);

impl std::fmt::Debug for Attached {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Attached").field(&self.0.path()).finish()
    }
}

impl Autoplay {
    pub fn new() -> Self {
        Self::default()
    }

    /// Attaches the library. Attaching a different library starts over.
    pub fn attach(&mut self, db: Library) {
        if self.lib.as_ref().is_some_and(|a| a.0.path() != db.path()) {
            self.reset();
        }
        self.lib = Some(Attached(db));
        self.refresh_mode();
    }

    /// The account's mode once attached, else Server.
    pub fn mode(&self) -> AutoplayMode {
        if self.lib.is_some() { self.mode } else { AutoplayMode::Server }
    }

    /// Songs to ask the server for per seed.
    pub fn per_seed(&self) -> u32 {
        match self.mode() {
            AutoplayMode::Navia => engine::plan::SIMILAR_ASK,
            AutoplayMode::Server => PER_SEED,
        }
    }

    /// Follows a change of the mode setting. Answers are dropped, but heard and removed songs stay.
    fn refresh_mode(&mut self) {
        let Some(db) = self.lib.as_ref() else { return };
        let mode = engine::mode(&db.0);
        if mode != self.mode {
            self.mode = mode;
            self.answered = None;
            self.asking = None;
            self.answers.clear();
            self.navia.clear();
            // Ask about the context again in the new mode.
            self.context_key = None;
            self.chosen.clear();
        }
    }

    fn db(&self) -> Option<&Library> {
        self.lib.as_ref().map(|a| &a.0)
    }

    /// A song started playing, so it and its versions are never offered again this session.
    pub fn heard(&mut self, song: &Song) {
        let taken = Taken::of(song);
        self.heard.insert(taken.id);
        self.heard_versions.insert(taken.version);
    }

    /// Like [`Autoplay::heard`], for a song known only by id and version.
    pub fn heard_taken(&mut self, taken: Taken) {
        self.heard.insert(taken.id);
        self.heard_versions.insert(taken.version);
    }

    /// The listener removed a song from Autoplay, so it won't come back this session.
    pub fn remove(&mut self, song: &Song) {
        let taken = Taken::of(song);
        self.removed.insert(taken.id);
        self.removed_versions.insert(taken.version);
    }

    /// Like [`Autoplay::remove`], for a song known only by id and version.
    pub fn remove_taken(&mut self, taken: Taken) {
        self.removed.insert(taken.id);
        self.removed_versions.insert(taken.version);
    }

    /// Sets what the listener started. A new `key` makes the anchors the new seeds.
    /// The same key changes nothing, so a song Autoplay picked never starts a new context.
    pub fn set_context<'a>(&mut self, key: u64, anchors: impl DoubleEndedIterator<Item = &'a str>) {
        self.refresh_mode();
        if self.context_key == Some(key) {
            return;
        }
        let latest = Self::latest(anchors);
        // The library doesn't have the songs yet, so ask again next time.
        if latest.is_empty() {
            return;
        }
        self.context_key = Some(key);
        self.start_context(latest);
    }

    /// [`Autoplay::set_context`] without a queue, for tests and examples. It can only guess at new contexts.
    pub fn set_chosen<'a>(&mut self, chosen: impl DoubleEndedIterator<Item = &'a str>) {
        let latest = Self::latest(chosen);
        self.refresh_mode();
        if latest.first().is_some_and(|lead| !self.chosen.contains(lead)) {
            self.context_key = None;
            self.start_context(latest);
        } else {
            self.seeds = latest.iter().take(SEED_COUNT).cloned().collect();
            self.chosen = latest;
        }
    }

    fn start_context(&mut self, latest: Vec<String>) {
        self.seeds = latest.iter().take(SEED_COUNT).cloned().collect();
        if self.mode() == AutoplayMode::Navia {
            let lib = self.lib.clone();
            self.navia.set_context(lib.as_ref().map(|a| &a.0), latest.iter().take(engine::ANCHORS).cloned().collect());
        }
        self.offer.clear();
        self.chosen = latest;
    }

    /// Whether the offer for this context is final.
    /// Until then apps show a finding state, so the rows shown never change order.
    pub fn ready(&self) -> bool {
        match self.mode() {
            AutoplayMode::Navia => self.navia.anchors().is_empty() || self.navia.wanted_fetch().is_none() && !self.navia.fetching(),
            AutoplayMode::Server => self.seeds.is_empty() || self.answered.as_ref().and_then(|a| a.first()) == self.seeds.first(),
        }
    }

    /// Distinct ids from the end of `chosen`, newest first, up to [`CHOSEN_KEEP`].
    fn latest<'a>(chosen: impl DoubleEndedIterator<Item = &'a str>) -> Vec<String> {
        let mut latest: Vec<String> = Vec::with_capacity(CHOSEN_KEEP);
        for id in chosen.rev() {
            if latest.len() == CHOSEN_KEEP {
                break;
            }
            if !id.is_empty() && !latest.iter().any(|s| s == id) {
                latest.push(id.to_string());
            }
        }
        latest
    }

    /// The Navia mode's context id, which changes whenever the listener starts something. 0 in the Server mode.
    pub fn context_id(&self) -> u64 {
        if self.mode() == AutoplayMode::Navia { self.navia.context_id() } else { 0 }
    }

    /// Whether `id` was offered for the current context. Apps hand a song over only then, so stale picks never play.
    pub fn offers(&self, id: &str) -> bool {
        self.offer.iter().any(|o| o == id)
    }

    /// The seeds in use, newest first.
    pub fn seeds(&self) -> &[String] {
        match self.mode() {
            AutoplayMode::Navia => self.navia.anchors(),
            AutoplayMode::Server => &self.seeds,
        }
    }

    /// Seeds to ask the server about, if the answers in hand are for other seeds and no ask is running.
    pub fn wanted_fetch(&self) -> Option<Vec<String>> {
        if self.mode() == AutoplayMode::Navia {
            return self.navia.wanted_fetch();
        }
        if self.seeds.is_empty() || self.answered.as_ref() == Some(&self.seeds) || self.asking.as_ref() == Some(&self.seeds) {
            return None;
        }
        Some(self.seeds.clone())
    }

    pub fn begin_fetch(&mut self, seeds: &[String]) {
        if self.mode() == AutoplayMode::Navia {
            return self.navia.begin_fetch(seeds);
        }
        self.asking = Some(seeds.to_vec());
    }

    /// Stores the server's answers for `seeds`, one list per seed.
    /// Nothing is offered until the newest choice is answered.
    pub fn finish_fetch(&mut self, seeds: Vec<String>, answers: Vec<Vec<Song>>) {
        if self.mode() == AutoplayMode::Navia {
            return self.navia.finish(seeds, Answers { similar: answers, ..Default::default() });
        }
        if self.asking.as_ref() == Some(&seeds) {
            self.asking = None;
        }
        self.answered = Some(seeds);
        self.answers = answers;
    }

    pub fn fetching(&self) -> bool {
        self.asking.is_some() || self.navia.fetching()
    }

    /// [`Autoplay::finish_fetch`] with what [`fetch_answers`] brought.
    pub fn finish_answers(&mut self, seeds: Vec<String>, answers: Answers) {
        if self.mode() == AutoplayMode::Navia {
            return self.navia.finish(seeds, answers);
        }
        self.finish_fetch(seeds, answers.similar);
    }

    /// Song ids heard this session, for Fewer Repeats (`queue_order::recency`).
    pub fn heard_ids(&self) -> &HashSet<String> {
        &self.heard
    }

    /// Forgets everything, for sign out or a server change. Stays attached.
    pub fn reset(&mut self) {
        let lib = self.lib.take();
        *self = Self::default();
        if let Some(lib) = lib {
            self.attach(lib.0);
        }
    }

    /// Up to `limit` songs to offer after `queue`, skipping queued, heard and removed songs and their versions.
    pub fn songs(&mut self, queue: &[Taken], limit: usize) -> Vec<Song> {
        if self.mode() == AutoplayMode::Navia {
            let ids: HashSet<String> = queue.iter().map(|t| t.id.clone()).collect();
            let versions: HashSet<String> = queue.iter().map(|t| t.version.clone()).collect();
            let order: Vec<String> = queue.iter().map(|t| t.id.clone()).collect();
            let junk = self.junk_filter(&order);
            let skip = engine::Skip { ids: vec![&self.heard, &self.removed, &ids], versions: vec![&self.heard_versions, &self.removed_versions, &versions], junk };
            let lib = self.lib.clone();
            let out = self.navia.songs(lib.as_ref().map(|a| &a.0), &order, &skip, limit);
            if let Some(db) = self.db() {
                db.engine_shared().note_offered(out.iter().map(|s| s.id.as_str()), AutoplayMode::Navia);
            }
            self.offer = out.iter().map(|s| s.id.clone()).collect();
            return out;
        }
        // Offer nothing until the newest choice is answered, never the last context's songs.
        if self.answered.as_ref().and_then(|a| a.first()) != self.seeds.first() {
            return Vec::new();
        }
        let order: Vec<String> = queue.iter().map(|t| t.id.clone()).collect();
        let junk = self.junk_filter(&order);
        let out = self.server_songs(queue, limit, |s| self.lib.is_some() && junk.leaves_out(s));
        if let Some(db) = self.db() {
            db.engine_shared().note_offered(out.iter().map(|s| s.id.as_str()), AutoplayMode::Server);
        }
        self.offer = out.iter().map(|s| s.id.clone()).collect();
        out
    }

    /// Leaves out short tracks and instrumental versions after `queue`, unless that kind was just playing.
    fn junk_filter(&self, queue: &[String]) -> engine::junk::Filter {
        self.db().map(|db| engine::junk::Filter::after(&engine::queue_tail(db, queue, JUNK_CONTEXT))).unwrap_or_default()
    }

    fn server_songs(&self, queue: &[Taken], limit: usize, leave_out: impl Fn(&Song) -> bool) -> Vec<Song> {
        let queued: HashSet<&str> = queue.iter().map(|t| t.id.as_str()).collect();
        let queued_versions: HashSet<&str> = queue.iter().map(|t| t.version.as_str()).collect();
        let mut picked_ids: HashSet<&str> = HashSet::new();
        let mut picked_versions: HashSet<String> = HashSet::new();
        let mut out = Vec::new();
        let longest = self.answers.iter().map(Vec::len).max().unwrap_or(0);
        'rounds: for k in 0..longest {
            for list in &self.answers {
                if out.len() >= limit {
                    break 'rounds;
                }
                let Some(song) = list.get(k) else { continue };
                let id = song.id.as_str();
                if leave_out(song) {
                    continue;
                }
                if self.heard.contains(id) || self.removed.contains(id) || queued.contains(id) || picked_ids.contains(id) {
                    continue;
                }
                let version = song_version(song);
                if self.heard_versions.contains(&version) || self.removed_versions.contains(&version) || queued_versions.contains(version.as_str()) || picked_versions.contains(&version) {
                    continue;
                }
                picked_ids.insert(id);
                picked_versions.insert(version);
                out.push(song.clone());
            }
        }
        out
    }
}

/// The answers to one ask, the server's similar songs per seed plus Deezer's in the Navia mode.
#[derive(Clone, Debug, Default)]
pub struct Answers {
    pub similar: Vec<Vec<Song>>,
    pub deezer: Option<engine::deezer_src::DeezerAnswers>,
    /// per seed song id: ListenBrainz's similar recordings as library songs.
    pub listenbrainz: Vec<(String, Vec<Song>)>,
}

/// the outside services the Navia mode asks with the recommendations switch on.
#[derive(Clone, Copy)]
pub struct Externals<'a> {
    pub deezer: &'a crate::deezer::Deezer,
    pub listenbrainz: &'a crate::listenbrainz::ListenBrainz,
}

impl Externals<'static> {
    /// the ones the apps use.
    pub fn shared() -> Self {
        Self { deezer: crate::deezer::Deezer::shared(), listenbrainz: crate::listenbrainz::ListenBrainz::shared() }
    }
}

/// Asks for `seeds` as `mode` needs, from the server and, in the Navia mode with the gate open, from Deezer.
/// Read-only on the server. Needs a Tokio runtime.
pub async fn fetch_answers(client: &Client, db: &Library, seeds: &[String], mode: AutoplayMode) -> Answers {
    fetch_answers_after(client, db, Externals::shared(), seeds, mode, engine::EXTERNAL_DEBOUNCE).await
}

/// [`fetch_answers`] with given outside services and no wait before them (radios, tests).
pub async fn fetch_answers_with(client: &Client, db: &Library, ext: Externals<'_>, seeds: &[String], mode: AutoplayMode) -> Answers {
    fetch_answers_after(client, db, ext, seeds, mode, std::time::Duration::ZERO).await
}

/// [`fetch_answers`] with given outside services and wait before asking them.
pub async fn fetch_answers_after(client: &Client, db: &Library, ext: Externals<'_>, seeds: &[String], mode: AutoplayMode, wait: std::time::Duration) -> Answers {
    if mode == AutoplayMode::Server {
        return Answers { similar: fetch_count(client, seeds, PER_SEED).await, ..Default::default() };
    }
    let similar = engine::server_src::similar(client, db, seeds, engine::plan::SIMILAR_ASK);
    if !crate::deezer::gate(db).is_open() {
        return Answers { similar: similar.await, ..Default::default() };
    }
    let outside = async {
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
            if !db.engine_shared().is_current(seeds) {
                return (None, Vec::new());
            }
        }
        let seed_songs: Vec<Song> = seeds.iter().filter_map(|id| db.song(id).ok().flatten()).collect();
        let deadline = tokio::time::Instant::now() + engine::EXTERNAL_TIMEOUT;
        let deezer = async {
            let mut found = engine::deezer_src::seed_sources(db, ext.deezer, &seed_songs).await;
            let mut names: Vec<String> = seed_songs.iter().map(|s| main_artist(s).to_string()).collect();
            for (artist, _) in engine::plan::neighbours(Some(db), &seed_songs, Some(&found)).into_iter().take(engine::deezer_src::TOP_ARTISTS) {
                if let Some(a) = db.artist(&artist).ok().flatten() {
                    names.push(a.name);
                }
            }
            found.popular = engine::deezer_src::top_places(db, ext.deezer, &names, deadline).await;
            found
        };
        let listenbrainz = engine::lb_src::seed_sources(db, ext.listenbrainz, &seed_songs, deadline);
        let (deezer, listenbrainz) = tokio::join!(tokio::time::timeout(engine::EXTERNAL_TIMEOUT, deezer), tokio::time::timeout(engine::EXTERNAL_TIMEOUT, listenbrainz));
        (deezer.ok(), listenbrainz.unwrap_or_default())
    };
    let (similar, (deezer, listenbrainz)) = tokio::join!(similar, outside);
    Answers { similar, deezer, listenbrainz }
}

/// The BPM of an untagged song from Deezer, when the gate is open. Kept for good.
pub async fn look_up_bpm(db: &Library, song_id: &str) -> Option<u32> {
    let song = db.song(song_id).ok().flatten()?;
    if song.bpm.is_some_and(|b| b > 0) || !crate::deezer::gate(db).is_open() {
        return None;
    }
    engine::deezer_src::bpm(db, crate::deezer::Deezer::shared(), &song).await
}

/// Asks the server for songs similar to each seed, all at once.
/// A seed the server can't answer gets an empty list.
pub async fn fetch(client: &Client, seeds: &[String]) -> Vec<Vec<Song>> {
    fetch_count(client, seeds, PER_SEED).await
}

/// [`fetch`] with `count` songs asked for per seed.
pub async fn fetch_count(client: &Client, seeds: &[String], count: u32) -> Vec<Vec<Song>> {
    let mut set = tokio::task::JoinSet::new();
    for (i, seed) in seeds.iter().enumerate() {
        let (client, seed) = (client.clone(), seed.clone());
        set.spawn(async move { (i, client.similar_songs2(&seed, count).await.unwrap_or_default()) });
    }
    let mut answers = vec![Vec::new(); seeds.len()];
    while let Some(done) = set.join_next().await {
        if let Ok((i, songs)) = done {
            answers[i] = songs;
        }
    }
    answers
}

/// How many songs a radio from one song queues in the Navia mode.
pub const RADIO_LEN: usize = 50;

/// A radio from one song, queued after it by Start Song Radio and by songs played from Search.
/// Follows the Autoplay mode, and falls back to the same artists' songs when the server has no similar songs.
pub async fn radio(client: &Client, db: &Library, song_id: &str) -> Vec<Song> {
    radio_with(client, db, Externals::shared(), song_id).await
}

/// [`radio`] with given outside services (tests use fakes).
pub async fn radio_with(client: &Client, db: &Library, ext: Externals<'_>, song_id: &str) -> Vec<Song> {
    let seed = db.song(song_id).ok().flatten().unwrap_or_else(|| Song { id: song_id.to_string(), ..Default::default() });
    let mode = engine::mode(db);
    if mode == AutoplayMode::Navia {
        let answers = fetch_answers_with(client, db, ext, std::slice::from_ref(&seed.id), mode).await;
        return navia_radio_songs(db, &seed, answers);
    }
    let answers = fetch(client, std::slice::from_ref(&seed.id)).await;
    let similar = radio_songs(&seed, &answers);
    if !similar.is_empty() {
        return similar;
    }
    radio_songs(&seed, &[shuffled_same_artist_songs(db, &seed)])
}

/// The Navia mode's radio from `seed`, up to [`RADIO_LEN`] songs of the engine's sealed plan.
/// Not recorded as Autoplay's offer, since the radio is the list the listener started.
pub fn navia_radio_songs(db: &Library, seed: &Song, answers: Answers) -> Vec<Song> {
    let seeds = vec![seed.id.clone()];
    let mut navia = engine::Navia::default();
    navia.set_context(None, seeds.clone());
    navia.finish(seeds.clone(), answers);
    let ids: HashSet<String> = HashSet::from([seed.id.clone()]);
    let versions: HashSet<String> = HashSet::from([song_version(seed)]);
    let junk = engine::junk::Filter::after(std::slice::from_ref(seed));
    let skip = engine::Skip { ids: vec![&ids], versions: vec![&versions], junk };
    navia.songs(Some(db), &seeds, &skip, RADIO_LEN)
}

fn shuffled_same_artist_songs(db: &Library, seed: &Song) -> Vec<Song> {
    let own = same_artist_songs(db, seed);
    crate::queue_order::apply_order(&own, &crate::queue_order::shuffled_order(own.len(), None, crate::queue_order::random_seed()))
}

/// Up to [`PER_SEED`] radio songs from the server's answers, without `seed`, its versions or repeats.
pub fn radio_songs(seed: &Song, answers: &[Vec<Song>]) -> Vec<Song> {
    let mut picker = Autoplay::new();
    picker.heard(seed);
    picker.answers = answers.to_vec();
    picker.songs(&[], PER_SEED as usize)
}

/// Songs on the albums of the seed's artists, from the library.
fn same_artist_songs(db: &Library, seed: &Song) -> Vec<Song> {
    let mut artists: Vec<&str> = seed.artists.iter().map(|a| a.id.as_str()).chain(seed.artist_id.as_deref()).filter(|id| !id.is_empty()).collect();
    artists.dedup();
    let mut rows: Vec<u32> = Vec::new();
    for id in artists {
        for row in db.artist_album_rowids(id).unwrap_or_default() {
            if !rows.contains(&row) {
                rows.push(row);
            }
        }
    }
    db.songs_by_rowid(&rows).unwrap_or_default().into_iter().map(|(_, s)| s).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::ArtistRef;

    fn song(id: &str, title: &str, artist: &str) -> Song {
        Song { id: id.into(), title: title.into(), artist: Some(artist.into()), artists: vec![ArtistRef { id: format!("ar-{artist}"), name: artist.into() }], ..Default::default() }
    }

    fn ids(songs: &[Song]) -> Vec<&str> {
        songs.iter().map(|s| s.id.as_str()).collect()
    }

    fn answered(answers: Vec<Vec<Song>>) -> Autoplay {
        let mut a = Autoplay::new();
        a.set_chosen(["s1", "s2", "s3"].into_iter());
        let seeds = a.wanted_fetch().unwrap();
        a.begin_fetch(&seeds);
        a.finish_fetch(seeds, answers);
        a
    }

    #[test]
    fn seeds_are_the_last_three_choices_newest_first() {
        let mut a = Autoplay::new();
        a.set_chosen(["a", "b", "c", "d", "d"].into_iter());
        assert_eq!(a.seeds(), ["d", "c", "b"]);
        a.set_chosen(["a"].into_iter());
        assert_eq!(a.seeds(), ["a"]);
    }

    #[test]
    fn asks_once_per_set_of_seeds() {
        let mut a = Autoplay::new();
        assert_eq!(a.wanted_fetch(), None, "nothing chosen, nothing to ask");
        a.set_chosen(["a", "b"].into_iter());
        let seeds = a.wanted_fetch().unwrap();
        assert_eq!(seeds, ["b", "a"]);
        a.begin_fetch(&seeds);
        assert!(a.fetching());
        assert_eq!(a.wanted_fetch(), None, "already asking");
        a.finish_fetch(seeds, vec![vec![], vec![]]);
        assert!(!a.fetching());
        assert_eq!(a.wanted_fetch(), None, "answered");
        a.set_chosen(["a", "b", "c"].into_iter());
        assert_eq!(a.wanted_fetch().unwrap(), ["c", "b", "a"]);
    }

    #[test]
    fn takes_turns_between_seeds_in_the_servers_order() {
        let mut a = answered(vec![
            vec![song("x1", "One", "X"), song("x2", "Two", "X")],
            vec![song("y1", "Uno", "Y")],
            vec![song("z1", "Eins", "Z"), song("z2", "Zwei", "Z"), song("z3", "Drei", "Z")],
        ]);
        assert_eq!(ids(&a.songs(&[], 10)), ["x1", "y1", "z1", "x2", "z2", "z3"]);
        assert_eq!(ids(&a.songs(&[], 2)), ["x1", "y1"]);
    }

    #[test]
    fn skips_heard_queued_removed_and_their_other_versions() {
        let mut a = answered(vec![vec![
            song("1", "Heard", "A"),
            song("2", "Heard (Live)", "A"),
            song("3", "Queued", "A"),
            song("4", "Queued - Radio Edit", "A"),
            song("5", "Removed", "A"),
            song("6", "removed  [2011 Remaster]", "A"),
            song("7", "Keep", "A"),
            song("8", "Keep", "A"),
            song("9", "Keep", "B"),
        ]]);
        a.heard(&song("h", "Heard", "A"));
        a.remove(&song("5", "Removed", "A"));
        let queue = [Taken::of(&song("q", "Queued", "A"))];
        // "Keep" by A twice is one song, but by B it's another.
        assert_eq!(ids(&a.songs(&queue, 10)), ["7", "9"]);
    }

    #[test]
    fn a_song_that_played_drops_out_and_the_next_comes_in() {
        const TEN: usize = 10;
        let mut a = answered(vec![(1..=12).map(|i| song(&format!("s{i}"), &format!("Song {i}"), "A")).collect()]);
        assert_eq!(a.songs(&[], TEN).len(), TEN);
        let first = a.songs(&[], 1).remove(0);
        a.heard(&first);
        let shown = a.songs(&[], TEN);
        assert_eq!(shown[0].id, "s2");
        assert_eq!(shown.last().unwrap().id, "s11");
    }

    #[test]
    fn radio_skips_the_song_and_its_versions_in_the_servers_order() {
        let seed = song("seed", "Dreams", "Fleetwood Mac");
        let answer = vec![
            song("seed", "Dreams", "Fleetwood Mac"),
            song("a", "Landslide", "Fleetwood Mac"),
            song("live", "Dreams (Live)", "Fleetwood Mac"),
            song("b", "Linger", "The Cranberries"),
            song("a", "Landslide", "Fleetwood Mac"),
            song("c", "Dreams", "The Cranberries"),
        ];
        assert_eq!(ids(&radio_songs(&seed, &[answer])), ["a", "b", "c"]);
    }

    #[test]
    fn radio_is_capped_and_empty_without_an_answer() {
        let seed = song("seed", "Seed", "A");
        let many: Vec<Song> = (0..100).map(|i| song(&format!("s{i}"), &format!("Song {i}"), "B")).collect();
        let picked = radio_songs(&seed, &[many]);
        assert_eq!(picked.len(), PER_SEED as usize);
        assert_eq!(picked[0].id, "s0");
        assert!(radio_songs(&seed, &[vec![]]).is_empty());
        assert!(radio_songs(&seed, &[vec![seed.clone()]]).is_empty(), "only the song itself is no radio");
    }

    #[test]
    fn radio_falls_back_to_the_same_artists_songs() {
        use crate::client::{Album, MusicFolder};
        use crate::library::LibraryConfig;
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open(&LibraryConfig { path: dir.path().join("lib.sqlite") }).unwrap();
        lib.replace_libraries(&[MusicFolder { id: "1".into(), name: "L".into() }]).unwrap();
        let cur = lib.begin_sync("1", None, false).unwrap();
        let album = |id: &str, artist: &str| Album { id: id.into(), name: id.into(), artist: Some(artist.into()), artist_id: Some(format!("ar-{artist}")), ..Default::default() };
        lib.apply_albums("1", &cur, 3, &[album("al1", "A"), album("al2", "A"), album("al3", "B")]).unwrap();
        let on = |mut s: Song, album: &str| {
            s.album_id = Some(album.into());
            s
        };
        lib.apply_songs("1", &cur, 5, &[
            on(song("seed", "Seed", "A"), "al1"),
            on(song("a1", "One", "A"), "al1"),
            on(song("v", "Seed (Live)", "A"), "al2"),
            on(song("a2", "Two", "A"), "al2"),
            on(song("b1", "Other", "B"), "al3"),
        ]).unwrap();
        let seed = lib.song("seed").unwrap().unwrap();
        let mut got: Vec<String> = radio_songs(&seed, &[same_artist_songs(&lib, &seed)]).into_iter().map(|s| s.id).collect();
        got.sort();
        assert_eq!(got, ["a1", "a2"], "the artist's other songs, not the song, its live version or another artist");
    }

    fn attached(mode: Option<AutoplayMode>) -> (tempfile::TempDir, Library, Autoplay) {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open(&crate::library::LibraryConfig { path: dir.path().join("lib.sqlite") }).unwrap();
        if let Some(mode) = mode {
            engine::set_mode(&lib, mode).unwrap();
        }
        let mut a = Autoplay::new();
        a.attach(lib.clone());
        (dir, lib, a)
    }

    fn fetched(a: &mut Autoplay, answer: impl Fn(&str) -> Vec<Song>) {
        let seeds = a.wanted_fetch().expect("seeds to ask about");
        a.begin_fetch(&seeds);
        assert_eq!(a.wanted_fetch(), None, "already asking");
        let answers = seeds.iter().map(|s| answer(s)).collect();
        a.finish_fetch(seeds, answers);
    }

    #[test]
    fn navia_is_the_default_and_server_mode_is_unchanged() {
        let (_d, _lib, a) = attached(None);
        assert_eq!(a.mode(), AutoplayMode::Navia);
        assert_eq!(a.per_seed(), engine::plan::SIMILAR_ASK);
        assert_eq!(Autoplay::new().mode(), AutoplayMode::Server, "without a library there's no engine");
        let (_d, _lib, mut a) = attached(Some(AutoplayMode::Server));
        assert_eq!(a.per_seed(), PER_SEED);
        a.set_chosen(["s1", "s2", "s3"].into_iter());
        fetched(&mut a, |seed| match seed {
            "s3" => vec![song("x1", "One", "X"), song("x2", "Two", "X")],
            "s2" => vec![song("y1", "Uno", "Y")],
            _ => vec![song("z1", "Eins", "Z"), song("z2", "Zwei", "Z"), song("z3", "Drei", "Z")],
        });
        assert_eq!(ids(&a.songs(&[], 10)), ["x1", "y1", "z1", "x2", "z2", "z3"], "the server's order, taking turns");
    }

    #[test]
    fn switching_modes_drops_the_other_modes_answers() {
        let (_d, lib, mut a) = attached(None);
        a.set_chosen(["s1"].into_iter());
        fetched(&mut a, |_| vec![song("x1", "One", "X")]);
        assert_eq!(ids(&a.songs(&[], 10)), ["x1"]);
        engine::set_mode(&lib, AutoplayMode::Server).unwrap();
        a.set_chosen(["s1"].into_iter());
        assert_eq!(a.wanted_fetch().unwrap(), ["s1"], "asked again, for the Server mode");
        assert!(a.songs(&[], 10).is_empty());
    }

    #[test]
    fn navia_plans_once_sealed_and_only_walks_it() {
        const TEN: usize = 10;
        let (_d, _lib, mut a) = attached(None);
        a.set_chosen(["s1", "s2"].into_iter());
        assert_eq!(a.seeds(), ["s2", "s1"], "the last songs of the started list anchor it");
        let context = a.context_id();
        // Both anchors name "both" late, and one artist fills the top of s2's answer.
        fetched(&mut a, |seed| {
            let mut answer: Vec<Song> = Vec::new();
            if seed == "s2" {
                answer.extend((0..8).map(|i| song(&format!("m{i}"), &format!("Many {i}"), "M")));
            }
            answer.extend((0..40).map(|i| song(&format!("{seed}-{i}"), &format!("{seed} song {i}"), &format!("{seed}-artist{i}"))));
            answer.insert(30, song("both", "Both", "B"));
            answer
        });
        let shown = a.songs(&[], TEN);
        assert_eq!(shown.len(), TEN);
        assert!(a.navia.sealed());
        let artists: Vec<&str> = shown.iter().map(main_artist).collect();
        for (i, artist) in artists.iter().enumerate() {
            assert!(!artists[i.saturating_sub(engine::sequence::ARTIST_GAP)..i].contains(artist), "{artists:?}");
        }
        assert!(artists.iter().filter(|a| **a == "M").count() <= 2, "{artists:?}");
        // The offer stays while it shows. Heard songs move the rest up and removed ones leave no gap.
        assert_eq!(ids(&a.songs(&[], TEN)), ids(&shown));
        a.heard(&shown[0]);
        a.remove(&shown[1]);
        let after = a.songs(&[], TEN);
        assert_eq!(ids(&after)[..TEN - 2], ids(&shown)[2..]);
        let plan: Vec<String> = a.navia.planned().iter().map(|s| s.id.clone()).collect();
        assert_eq!(ids(&after)[TEN - 2..], plan.iter().filter(|id| !ids(&shown).contains(&id.as_str())).take(2).map(String::as_str).collect::<Vec<_>>());
        // Agreement lifts a late song, and the same choices keep the context.
        let both = plan.iter().position(|id| id == "both").unwrap();
        let tail = plan.iter().position(|id| id == "s1-29").unwrap();
        assert!(both < tail, "{plan:?}");
        a.set_chosen(["s1", "s2"].into_iter());
        assert_eq!(a.context_id(), context);
        assert!(a.offers(&after[0].id));
    }

    #[test]
    fn plays_never_seed_and_a_session_before_changes_nothing() {
        use engine::playlog::{Ended, Origin, now_ms, record_end, record_start};
        let answer = |seed: &str| (0..30).map(|i| song(&format!("{seed}-{i}"), &format!("{seed} {i}"), &format!("{seed}-artist{i}"))).collect::<Vec<_>>();
        let (_d, _lib, mut alone) = attached(None);
        alone.set_chosen(["new"].into_iter());
        fetched(&mut alone, answer);
        let first = ids(&alone.songs(&[], 50)).into_iter().map(str::to_string).collect::<Vec<_>>();
        // The same song after an unrelated session of finished songs.
        let (_d2, lib, mut after) = attached(None);
        for (i, id) in ["u1", "u2", "u3", "u4", "u5"].into_iter().enumerate() {
            let row = record_start(&lib, id, Some(200_000), Origin::User, now_ms() - 900_000 + i as i64 * 1000).unwrap();
            record_end(&lib, row, 200_000, Ended::Finished, false, now_ms() - 800_000 + i as i64 * 1000).unwrap();
        }
        after.set_chosen(["u1", "u2", "u3", "u4", "u5"].into_iter());
        after.set_chosen(["new"].into_iter());
        assert_eq!(after.seeds(), ["new"]);
        fetched(&mut after, answer);
        assert_eq!(ids(&after.songs(&[], 50)), first.iter().map(String::as_str).collect::<Vec<_>>());
    }

    #[test]
    fn starting_something_new_drops_the_last_contexts_offer_in_both_modes() {
        const TEN: usize = 10;
        use engine::playlog::{Ended, Origin, now_ms, record_end, record_start};
        for mode in [AutoplayMode::Navia, AutoplayMode::Server] {
            let (_d, lib, mut a) = attached(Some(mode));
            // A plays alone and is listened through, then Autoplay offers X with "Woman Like Me" in it.
            let row = record_start(&lib, "a", Some(200_000), Origin::User, now_ms() - 250_000).unwrap();
            a.set_chosen(["a"].into_iter());
            let answer = |seed: &str| -> Vec<Song> {
                let mut list: Vec<Song> = (0..20).map(|i| song(&format!("{seed}-{i}"), &format!("{seed} {i}"), &format!("{seed}-artist{i}"))).collect();
                if seed == "a" {
                    list.insert(0, song("wlm", "Woman Like Me", "Adele"));
                }
                list
            };
            fetched(&mut a, answer);
            let x = a.songs(&[], 50);
            assert!(x.iter().any(|s| s.id == "wlm"), "{mode:?}");
            record_end(&lib, row, 200_000, Ended::Finished, false, now_ms() - 50_000).unwrap();
            // The listener starts B, and nothing of A's offer shows while B is asked about...
            a.set_chosen(["b"].into_iter());
            assert!(a.songs(&[], TEN).is_empty(), "{mode:?}: the old offer stays up");
            // ...and only B's once answered, since A no longer seeds.
            if mode == AutoplayMode::Navia {
                assert_eq!(a.seeds(), ["b"], "B anchors it; A is gone");
                assert!(!a.offers("wlm"), "a pick of A's is never handed over now");
            }
            fetched(&mut a, answer);
            let offer = a.songs(&[], TEN);
            assert_eq!(offer.len(), TEN, "{mode:?}");
            assert!(offer.iter().all(|s| s.id.starts_with("b-")), "{mode:?}: {:?}", ids(&offer));
        }
    }

    #[test]
    fn both_modes_leave_out_short_tracks_and_instrumentals() {
        let answer = |_: &str| {
            let mut short = song("short", "Intro", "S");
            short.duration = Some(20);
            let mut out = vec![song("inst", "Hit (Instrumental)", "I"), short, song("kar", "Hit [Karaoke Version]", "K")];
            out.extend((0..5).map(|i| song(&format!("ok{i}"), &format!("Song {i}"), &format!("A{i}"))));
            out
        };
        for mode in [AutoplayMode::Server, AutoplayMode::Navia] {
            let (_d, _lib, mut a) = attached(Some(mode));
            a.set_chosen(["s1"].into_iter());
            fetched(&mut a, answer);
            let mut got = ids(&a.songs(&[], 10)).into_iter().map(str::to_string).collect::<Vec<_>>();
            got.sort();
            assert_eq!(got, ["ok0", "ok1", "ok2", "ok3", "ok4"], "{mode:?}");
        }
        // Song radio without a library is unchanged.
        let seed = song("seed", "Seed", "Z");
        assert_eq!(radio_songs(&seed, &[answer("")]).len(), 8);
    }

    /// What an app does with the queue model, setting the context's key and anchors.
    fn context_from(a: &mut Autoplay, q: &crate::play_queue::PlayQueue<String>) {
        let anchors = q.anchor_tail(CHOSEN_KEEP);
        a.set_context(q.context(), anchors.iter().map(String::as_str));
    }

    fn taken_of(q: &crate::play_queue::PlayQueue<String>) -> Vec<Taken> {
        q.songs().into_iter().map(|id| Taken { version: format!("v-{id}"), id }).collect()
    }

    fn plan_answer(seed: &str) -> Vec<Song> {
        (0..80).map(|i| song(&format!("{seed}-{i}"), &format!("{seed} song {i}"), &format!("{seed}-artist{i}"))).collect()
    }

    #[test]
    fn an_autoplay_song_that_plays_never_starts_a_context() {
        use crate::play_queue::PlayQueue;
        for mode in [AutoplayMode::Navia, AutoplayMode::Server] {
            let (_d, _lib, mut a) = attached(Some(mode));
            let mut q: PlayQueue<String> = PlayQueue::new();
            q.start(vec!["own".to_string()], 0, false);
            context_from(&mut a, &q);
            assert!(!a.ready(), "{mode:?}: asked first");
            fetched(&mut a, plan_answer);
            assert!(a.ready(), "{mode:?}");
            let context = a.context_id();
            let plan = a.songs(&taken_of(&q), SHOWN);
            assert_eq!(plan.len(), SHOWN, "{mode:?}");
            // Autoplay's first song is handed over, then starts by a skip or the song's end.
            q.hand_over(plan[0].id.clone());
            q.track_changed(1, None);
            a.heard(&plan[0]);
            context_from(&mut a, &q);
            assert_eq!(q.anchor_tail(CHOSEN_KEEP), ["own"], "{mode:?}: Autoplay's song is no anchor");
            assert_eq!(a.context_id(), context, "{mode:?}");
            assert_eq!(a.wanted_fetch(), None, "{mode:?}: nothing asked again");
            assert!(a.ready(), "{mode:?}");
            let after = a.songs(&taken_of(&q), SHOWN);
            assert_eq!(ids(&after)[..SHOWN - 1], ids(&plan)[1..], "{mode:?}: the plan walked, whole");
            assert_eq!(after.len(), SHOWN, "{mode:?}: the next of the plan comes in at the end");
            // A song picked from further down plays on its own and the ones above it are dropped.
            let pick = after[3].clone();
            for above in &after[..3] {
                a.remove(above);
            }
            q.play_other(pick.id.clone());
            q.track_changed(2, None);
            a.heard(&pick);
            context_from(&mut a, &q);
            assert_eq!(a.context_id(), context, "{mode:?}: a direct pick keeps the context");
            let rest = a.songs(&taken_of(&q), SHOWN);
            assert_eq!(ids(&rest)[..SHOWN - 4], ids(&after)[4..], "{mode:?}: the plan goes on from the pick");
            // A song queued meanwhile is no new context either.
            q.insert(vec!["queued".to_string()], 0, true);
            context_from(&mut a, &q);
            assert_eq!(a.context_id(), context, "{mode:?}");
            assert_eq!(ids(&a.songs(&taken_of(&q), SHOWN)), ids(&rest), "{mode:?}");
            // Starting something makes a new context with nothing of the old offer.
            q.start(vec!["other".to_string()], 0, false);
            context_from(&mut a, &q);
            assert!(!a.ready(), "{mode:?}");
            assert!(!a.offers(&rest[0].id), "{mode:?}");
            if mode == AutoplayMode::Navia {
                assert_ne!(a.context_id(), context);
            }
            assert_eq!(a.seeds(), ["other"], "{mode:?}");
        }
    }

    /// Random sessions over the queue model and Autoplay, driven as the apps drive them.
    /// Short of starting something, the offer is always the sealed plan minus heard, removed and queued songs.
    #[test]
    fn random_sessions_keep_the_plan_whole() {
        use crate::play_queue::{Kind, PlayQueue};
        let sessions = std::env::var("NAVIA_AUTOPLAY_FUZZ").ok().and_then(|v| v.parse().ok()).unwrap_or(40u64);
        for session in 0..sessions {
            let mut rng = session.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x5eed;
            let mut next = move |n: u64| {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                rng % n
            };
            let (_d, _lib, mut a) = attached(Some(if session % 4 == 3 { AutoplayMode::Server } else { AutoplayMode::Navia }));
            let mut q: PlayQueue<String> = PlayQueue::new();
            let mut starts = 0;
            let mut fresh = 0;
            // The sealed plan of the context in use, and what left it.
            let mut plan: Vec<String> = Vec::new();
            let mut gone: HashSet<String> = HashSet::new();
            let mut log: Vec<String> = Vec::new();
            for step in 0..120 {
                let started = step == 0 || next(12) == 0;
                if started {
                    starts += 1;
                    let list: Vec<String> = (0..1 + next(3)).map(|i| format!("list{starts}x{i}")).collect();
                    let at = next(list.len() as u64) as usize;
                    q.start(list, at, false);
                    log.push(format!("start {starts} at {at}"));
                } else {
                    let after = q.entries().len() - q.pos() - 1;
                    let tail = q.other_tail();
                    let offer = a.songs(&taken_of(&q), SHOWN);
                    match next(7) {
                        // The next song starts, by a skip or the song's end.
                        0 | 1 if after > 0 => {
                            let left = q.current().is_some_and(|e| e.kind == Kind::Queued).then(|| q.pos());
                            let to = if left.is_some() { q.pos() } else { q.pos() + 1 };
                            q.track_changed(to, left);
                            log.push("next".into());
                        }
                        // A song picked from the offer drops the ones above it.
                        2 if !offer.is_empty() => {
                            let k = next(offer.len().min(6) as u64) as usize;
                            for above in &offer[..k] {
                                a.remove(above);
                                gone.insert(above.id.clone());
                            }
                            q.drop_other_tail();
                            q.play_other(offer[k].id.clone());
                            q.track_changed(q.pos() + 1, None);
                            log.push(format!("pick {k}"));
                        }
                        // One removed from the offer.
                        3 if !offer.is_empty() => {
                            let k = next(offer.len().min(8) as u64) as usize;
                            a.remove(&offer[k]);
                            gone.insert(offer[k].id.clone());
                            log.push(format!("remove {k}"));
                        }
                        // A song queued, or one of the offer dragged into Next in Queue.
                        4 => {
                            fresh += 1;
                            q.insert(vec![format!("queued{fresh}")], 0, true);
                            log.push("queue".into());
                        }
                        5 if tail > 0 => {
                            let edits = q.remove(&[after - 1]);
                            assert!(!edits.is_empty());
                            let id = offer.first().map(|s| s.id.clone());
                            log.push(format!("remove the handed-over song, next {id:?}"));
                        }
                        _ => {}
                    }
                }
                // What the apps do after every change.
                if let Some(cur) = q.current() {
                    let id = cur.song.clone();
                    a.heard_taken(Taken { version: format!("v-{id}"), id: id.clone() });
                    gone.insert(id);
                }
                let before = a.context_id();
                let (was_key, was_ready) = (a.context_key, a.ready());
                context_from(&mut a, &q);
                let ctx = || format!("session {session}, step {step}: {log:?}");
                if started {
                    assert_ne!(a.context_key, was_key, "a start is a new context, {}", ctx());
                    assert!(q.entries()[q.pos() + 1..].iter().all(|e| e.kind != Kind::Other), "no pick of the last context waits, {}", ctx());
                    assert!(!a.ready() || a.wanted_fetch().is_none(), "{}", ctx());
                    fetched(&mut a, plan_answer);
                    plan = a.songs(&[], usize::MAX).into_iter().map(|s| s.id).collect();
                    gone.clear();
                    gone.extend(q.current().map(|e| e.song.clone()));
                } else {
                    assert_eq!(a.context_key, was_key, "only a start is a new context, {}", ctx());
                    assert_eq!(a.context_id(), before, "{}", ctx());
                    assert!(was_ready && a.ready() && a.wanted_fetch().is_none(), "the plan stays sealed, {}", ctx());
                }
                assert!(q.anchor_tail(CHOSEN_KEEP).iter().all(|s| s.starts_with("list") || s.starts_with("queued")), "Autoplay's songs never anchor, {}", ctx());
                // The offer is the plan in order, minus heard, removed and queued songs.
                let in_queue: HashSet<String> = q.songs().into_iter().collect();
                let want: Vec<&String> = plan.iter().filter(|id| !gone.contains(*id) && !in_queue.contains(*id)).take(SHOWN).collect();
                let offer = a.songs(&taken_of(&q), SHOWN);
                assert_eq!(offer.iter().map(|s| &s.id).collect::<Vec<_>>(), want, "the remaining plan, intact, {}", ctx());
                assert_eq!(offer.len(), SHOWN.min(plan.iter().filter(|id| !gone.contains(*id) && !in_queue.contains(*id)).count()), "{}", ctx());
                // Nothing of the listener's is left, so the first of the offer is handed over.
                if q.entries().len() == q.pos() + 1 && let Some(first) = offer.first() {
                    assert!(a.offers(&first.id));
                    q.hand_over(first.id.clone());
                }
            }
        }
    }

    #[test]
    fn version_keys_fold_editions() {
        let k = version_key("Dreams", "Fleetwood Mac");
        assert_eq!(version_key("Dreams (2004 Remaster)", "Fleetwood Mac"), k);
        assert_eq!(version_key("DREAMS  - Live", "fleetwood mac"), k);
        assert_eq!(version_key("Dreams [Take 2]", "Fleetwood Mac"), k);
        assert_ne!(version_key("Dreams", "The Cranberries"), k);
        assert_ne!(version_key("Dreamer", "Fleetwood Mac"), k);
        // A title that starts with a bracket keeps it.
        assert_eq!(version_key("(What's the Story)", "Oasis"), "(what's the story)|oasis");
    }

    #[test]
    fn never_starts_under_repeat_or_a_station_or_when_off() {
        assert!(starts_now(true, Repeat::Off, false, 0));
        assert!(!starts_now(true, Repeat::All, false, 0));
        assert!(!starts_now(true, Repeat::One, false, 0));
        assert!(!starts_now(false, Repeat::Off, false, 0));
        assert!(!starts_now(true, Repeat::Off, true, 0));
        // Anything of the listener's left, shuffled or not, plays first.
        assert!(!starts_now(true, Repeat::Off, false, 1));
    }

    #[test]
    fn section_states() {
        assert_eq!(state(true, Repeat::Off, false, true, 0, true), AutoplayState::On, "the list's last song, or a song on its own");
        assert_eq!(state(true, Repeat::Off, false, false, 0, true), AutoplayState::Empty);
        assert_eq!(state(true, Repeat::All, false, true, 0, true), AutoplayState::Repeat);
        assert_eq!(state(false, Repeat::All, false, true, 0, true), AutoplayState::Off);
        assert_eq!(state(true, Repeat::Off, true, true, 0, true), AutoplayState::Station, "a station hides it");
        assert_eq!(state(false, Repeat::Off, true, true, 0, true), AutoplayState::Station);
        // While the list has songs to come, it's not time yet, ready or not.
        assert_eq!(state(true, Repeat::Off, false, true, 1, true), AutoplayState::Later);
        assert_eq!(state(true, Repeat::Off, false, false, 40, true), AutoplayState::Later);
        // The Repeat note stays wherever the list is.
        assert_eq!(state(true, Repeat::One, false, true, 5, true), AutoplayState::Repeat);
        assert_eq!(state(false, Repeat::Off, false, true, 5, true), AutoplayState::Off);
        // Finding only when the list is over and the plan isn't ready,
        // never while the list has songs, Repeat is on or Autoplay is off.
        assert_eq!(state(true, Repeat::Off, false, false, 0, false), AutoplayState::Finding);
        assert_eq!(state(true, Repeat::Off, false, true, 0, false), AutoplayState::Finding);
        assert_eq!(state(true, Repeat::Off, false, false, 3, false), AutoplayState::Later);
        assert_eq!(state(true, Repeat::All, false, false, 0, false), AutoplayState::Repeat);
        assert_eq!(state(false, Repeat::Off, false, false, 0, false), AutoplayState::Off);
    }
}
