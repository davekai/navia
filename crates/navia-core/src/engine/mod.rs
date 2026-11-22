//! Navia's own Autoplay engine. The "Server" mode lives in `crate::autoplay` instead.
//!
//! A context starts when the listener starts a song, list or radio. The server is asked at once
//! and Deezer only after [`DEEZER_DEBOUNCE`] of play. Until both answer, a provisional plan plays.
//! Then the plan is sealed and only walked, never reordered or refilled.

pub mod deezer_src;
pub mod eval;
pub mod fatigue;
pub mod junk;
pub mod lb_src;
pub mod plan;
pub mod playlog;
pub mod sequence;
pub mod server_src;

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use self::deezer_src::DeezerAnswers;

use crate::client::Song;
use crate::library::Library;

/// Which Autoplay plays.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AutoplayMode {
    /// Navia's own engine. The default.
    #[default]
    Navia,
    /// The server's similar songs in its order, cleaned of repeats.
    Server,
}

impl AutoplayMode {
    pub fn as_str(self) -> &'static str {
        match self {
            AutoplayMode::Navia => "navia",
            AutoplayMode::Server => "server",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "navia" => Some(AutoplayMode::Navia),
            "server" => Some(AutoplayMode::Server),
            _ => None,
        }
    }
}

/// The mode's key in the library's `meta` table, "navia" or "server". Absent means Navia.
const MODE_KEY: &str = "autoplay_mode";

/// This account's Autoplay mode. Navia unless the listener chose Server.
pub fn mode(db: &Library) -> AutoplayMode {
    db.meta(MODE_KEY).ok().flatten().as_deref().and_then(AutoplayMode::parse).unwrap_or_default()
}

/// Saves this account's Autoplay mode.
pub fn set_mode(db: &Library, mode: AutoplayMode) -> crate::Result<()> {
    db.set_meta(MODE_KEY, Some(mode.as_str()))
}

/// The `meta` key recording which one-time mode migrations this library has had.
const MODE_MIGRATION_KEY: &str = "autoplay_mode_migration";
/// Migration 1 puts every account on the Navia mode once. Choosing Server later sticks.
const MODE_MIGRATION: &str = "1";

/// Runs the mode migration once per library. The mode and the mark are written together, so an
/// interrupted run repeats rather than half-applies.
pub(crate) fn migrate_mode(db: &Library) -> crate::Result<()> {
    if db.meta(MODE_MIGRATION_KEY)?.as_deref() == Some(MODE_MIGRATION) {
        return Ok(());
    }
    db.write_quiet(|tx| {
        let set = "INSERT INTO meta(key, value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value";
        tx.execute(set, [MODE_KEY, AutoplayMode::Navia.as_str()])?;
        tx.execute(set, [MODE_MIGRATION_KEY, MODE_MIGRATION])?;
        Ok(())
    })
}

#[cfg(test)]
mod mode_tests {
    use super::*;
    use crate::library::LibraryConfig;

    #[test]
    fn the_update_puts_every_account_on_navia_once() {
        let dir = tempfile::tempdir().unwrap();
        let config = LibraryConfig { path: dir.path().join("lib.sqlite") };
        // An existing account that had chosen Server.
        {
            let lib = Library::open(&config).unwrap();
            set_mode(&lib, AutoplayMode::Server).unwrap();
            lib.set_meta(MODE_MIGRATION_KEY, None).unwrap();
        }
        let lib = Library::open(&config).unwrap();
        assert_eq!(mode(&lib), AutoplayMode::Navia);
        // Choosing Server afterwards sticks across launches.
        set_mode(&lib, AutoplayMode::Server).unwrap();
        drop(lib);
        let lib = Library::open(&config).unwrap();
        assert_eq!(mode(&lib), AutoplayMode::Server);
        drop(lib);
        // A new account starts on Navia and is marked, so its later choice sticks too.
        let fresh = LibraryConfig { path: dir.path().join("fresh.sqlite") };
        let lib = Library::open(&fresh).unwrap();
        assert_eq!(mode(&lib), AutoplayMode::Navia);
        assert_eq!(lib.meta(MODE_MIGRATION_KEY).unwrap().as_deref(), Some(MODE_MIGRATION));
    }
}

/// How long an offered song counts as Autoplay's pick if it starts playing.
const OFFER_KEEP: Duration = Duration::from_secs(6 * 3600);

/// In-memory state shared through the [`Library`] handle, so the play recorder and Autoplay agree
/// on who chose a song.
#[derive(Default)]
pub struct Shared {
    offered: Mutex<HashMap<String, (AutoplayMode, Instant)>>,
    /// The current context's anchors. A fetch for older ones skips Deezer.
    current: Mutex<Vec<String>>,
    /// Autoplay's picks skipped early in a row in this context, and the artists held back for it.
    skips: Mutex<SkipRun>,
    /// the server's cleaned similar songs per anchor for this session (`server_src`), oldest first.
    cleaned: Mutex<Vec<(String, Vec<Song>)>>,
}

/// After [`SKIP_RUN`] early skips of Autoplay's picks in a row, their artists are held back for
/// the rest of the context.
#[derive(Default)]
struct SkipRun {
    run: Vec<String>,
    held: HashSet<String>,
}

/// Early skips of Autoplay's picks in a row that hold their artists back.
pub const SKIP_RUN: usize = 2;

impl Shared {
    /// Sets the current context's anchors. A new context forgets the last one's skips.
    pub fn set_current(&self, anchors: &[String]) {
        *self.current.lock().expect("current") = anchors.to_vec();
        *self.skips.lock().expect("skips") = SkipRun::default();
    }

    /// Notes the end of a play. `Some(artist)` for an Autoplay pick skipped early, `None` for
    /// anything else, which ends the run.
    pub fn note_play_end(&self, picked_skip: Option<&str>) {
        let mut s = self.skips.lock().expect("skips");
        match picked_skip {
            Some(artist) => {
                s.run.push(artist.to_string());
                if s.run.len() >= SKIP_RUN {
                    let run = s.run.clone();
                    s.held.extend(run.into_iter().filter(|a| !a.is_empty()));
                }
            }
            None => s.run.clear(),
        }
    }

    /// The artists held back for the rest of this context.
    pub fn held_artists(&self) -> HashSet<String> {
        self.skips.lock().expect("skips").held.clone()
    }

    /// Whether these are still the Autoplay context's anchors.
    pub fn is_current(&self, anchors: &[String]) -> bool {
        *self.current.lock().expect("current") == anchors
    }

    /// the server's cleaned similar songs for this anchor, when known this session.
    pub fn cleaned(&self, anchor: &str) -> Option<Vec<Song>> {
        self.cleaned.lock().expect("cleaned").iter().find(|(a, _)| a == anchor).map(|(_, s)| s.clone())
    }

    /// keeps the server's cleaned similar songs for this anchor for the session.
    pub fn keep_cleaned(&self, anchor: &str, songs: Vec<Song>) {
        let mut kept = self.cleaned.lock().expect("cleaned");
        kept.retain(|(a, _)| a != anchor);
        kept.push((anchor.to_string(), songs));
        let over = kept.len().saturating_sub(server_src::KEEP_ANCHORS);
        kept.drain(..over);
    }

    /// Songs Autoplay is offering now, in `mode`.
    pub fn note_offered<'a>(&self, ids: impl Iterator<Item = &'a str>, mode: AutoplayMode) {
        let now = Instant::now();
        let mut offered = self.offered.lock().expect("offered");
        offered.retain(|_, (_, at)| now.duration_since(*at) < OFFER_KEEP);
        for id in ids {
            offered.insert(id.to_string(), (mode, now));
        }
    }

    /// Whether a song that just started was Autoplay's, and in which mode. Forgets it after.
    pub fn take_offered(&self, id: &str) -> Option<AutoplayMode> {
        let mut offered = self.offered.lock().expect("offered");
        let (mode, at) = offered.remove(id)?;
        (at.elapsed() < OFFER_KEEP).then_some(mode)
    }
}

/// Deezer and ListenBrainz are asked only once the context has played this long, so quick changes
/// ask nothing.
pub const EXTERNAL_DEBOUNCE: Duration = Duration::from_secs(10);
/// how long their part may take before the plan is sealed without the rest.
pub const EXTERNAL_TIMEOUT: Duration = Duration::from_secs(6);
/// A started list's last songs that anchor its context.
pub const ANCHORS: usize = 3;

/// A song's genre tag, or "".
pub fn genre_of(song: &Song) -> &str {
    song.genre.as_deref().filter(|g| !g.is_empty()).or_else(|| song.genres.first().map(|g| g.name.as_str())).unwrap_or("")
}

/// The Navia mode's state for one listening session, kept inside [`crate::autoplay::Autoplay`].
#[derive(Debug, Default)]
pub struct Navia {
    /// Increments with every new context. Picks carry it.
    context: u64,
    /// The context's anchors, newest first.
    anchors: Vec<String>,
    /// The server's similar songs per anchor, kept for the session.
    similar: HashMap<String, Vec<Song>>,
    /// Deezer's answer once the ask finished. `Some(None)` when it's off or timed out.
    deezer: Option<Option<DeezerAnswers>>,
    /// ListenBrainz's similar recordings per anchor as library songs, with Deezer's answer.
    listenbrainz: Vec<(String, Vec<Song>)>,
    asking: Option<Vec<String>>,
    plan: Option<Plan>,
    /// Artists held back after early skips, as applied to the plan.
    held: HashSet<String>,
    /// Songs before this place have been offered. Only the rest may move.
    shown_upto: usize,
}

/// How far a held-back artist's song moves down the part of the plan not yet offered.
pub const HELD_DEFER: usize = 10;

/// Moves `held` artists' songs in `songs[from..]` [`HELD_DEFER`] places later, keeping the rest
/// in order.
pub fn defer_held(songs: &mut [Song], from: usize, held: &HashSet<String>) {
    if held.is_empty() || from >= songs.len() {
        return;
    }
    let tail = &mut songs[from..];
    let mut keyed: Vec<(usize, bool, Song)> = tail.iter().enumerate().map(|(i, s)| {
        let down = held.contains(&crate::recommend::artist_key(s));
        (if down { i + HELD_DEFER } else { i }, down, s.clone())
    }).collect();
    keyed.sort_by_key(|k| (k.0, k.1));
    for (slot, (_, _, song)) in tail.iter_mut().zip(keyed) {
        *slot = song;
    }
}

#[derive(Debug)]
struct Plan {
    songs: Vec<Song>,
    sealed: bool,
}

/// What Autoplay must not offer: songs heard, removed or queued, by id or version, and junk.
pub struct Skip<'a> {
    pub ids: Vec<&'a HashSet<String>>,
    pub versions: Vec<&'a HashSet<String>>,
    pub junk: junk::Filter,
}

impl Skip<'_> {
    fn skips(&self, song: &Song, version: &str) -> bool {
        self.ids.iter().any(|s| s.contains(&song.id)) || self.versions.iter().any(|s| s.contains(version)) || self.junk.leaves_out(song)
    }
}

impl Navia {
    /// Starts a context with these anchors unless it's the current one, dropping the old plan and
    /// any pending ask. Returns the context's id.
    pub fn set_context(&mut self, db: Option<&Library>, anchors: Vec<String>) -> u64 {
        if anchors.is_empty() || (anchors == self.anchors && self.context > 0) {
            return self.context;
        }
        self.context += 1;
        self.anchors = anchors;
        self.deezer = None;
        self.listenbrainz.clear();
        self.asking = None;
        self.plan = None;
        self.held.clear();
        self.shown_upto = 0;
        if let Some(db) = db {
            db.engine_shared().set_current(&self.anchors);
        }
        self.context
    }

    /// The current context's id, 0 before the first.
    pub fn context_id(&self) -> u64 {
        self.context
    }

    /// The anchors, newest first.
    pub fn anchors(&self) -> &[String] {
        &self.anchors
    }

    /// The anchors to ask about, if answers are missing and no ask is pending.
    pub fn wanted_fetch(&self) -> Option<Vec<String>> {
        if self.anchors.is_empty() || self.asking.as_ref() == Some(&self.anchors) || self.done() {
            return None;
        }
        Some(self.anchors.clone())
    }

    fn done(&self) -> bool {
        self.deezer.is_some() && self.anchors.iter().all(|a| self.similar.contains_key(a))
    }

    pub fn begin_fetch(&mut self, seeds: &[String]) {
        self.asking = Some(seeds.to_vec());
    }

    /// What an ask for `seeds` brought. The server's answers are kept for later contexts; for
    /// this context's anchors it completes the context, so the next [`Navia::songs`] seals the
    /// plan. Answers for an older context only fill the cache.
    pub fn finish(&mut self, seeds: Vec<String>, answers: crate::autoplay::Answers) {
        if self.asking.as_ref() == Some(&seeds) {
            self.asking = None;
        }
        for (seed, songs) in seeds.iter().zip(answers.similar) {
            self.similar.insert(seed.clone(), songs);
        }
        if seeds == self.anchors && self.deezer.is_none() {
            self.deezer = Some(answers.deezer);
            self.listenbrainz = answers.listenbrainz;
            if self.plan.as_ref().is_some_and(|p| !p.sealed) {
                self.plan = None;
            }
        }
    }

    pub fn fetching(&self) -> bool {
        self.asking.is_some()
    }

    /// The plan as it stands.
    pub fn planned(&self) -> &[Song] {
        self.plan.as_ref().map_or(&[], |p| p.songs.as_slice())
    }

    /// Whether the plan is sealed.
    pub fn sealed(&self) -> bool {
        self.plan.as_ref().is_some_and(|p| p.sealed)
    }

    /// Up to `limit` songs to offer after `queue`, skipping what `skip` or the queue holds. Before
    /// the plan is sealed this walks a provisional one, so a short context still plays on.
    pub fn songs(&mut self, db: Option<&Library>, queue: &[String], skip: &Skip, limit: usize) -> Vec<Song> {
        if self.anchors.is_empty() {
            return Vec::new();
        }
        let held = db.map(|db| db.engine_shared().held_artists()).unwrap_or_default();
        if self.plan.is_none() || (!self.sealed() && self.done()) {
            let sealed = self.done();
            let mut songs = self.build(db, queue, skip, sealed);
            // Nothing of a new plan was offered, so held-back artists move down all of it.
            defer_held(&mut songs, 0, &held);
            self.plan = Some(Plan { songs, sealed });
            self.shown_upto = 0;
            self.held = held;
        } else if held != self.held {
            // Two early skips in a row. Only what hasn't been offered yet moves.
            if let Some(plan) = self.plan.as_mut() {
                defer_held(&mut plan.songs, self.shown_upto, &held);
            }
            self.held = held;
        }
        let plan = self.plan.as_ref().expect("plan");
        let mut out = Vec::new();
        for (i, s) in plan.songs.iter().enumerate() {
            if out.len() >= limit {
                break;
            }
            if !skip.skips(s, &crate::autoplay::song_version(s)) && !queue.contains(&s.id) {
                out.push(s.clone());
                self.shown_upto = self.shown_upto.max(i + 1);
            }
        }
        out
    }

    fn build(&self, db: Option<&Library>, queue: &[String], skip: &Skip, sealed: bool) -> Vec<Song> {
        let anchors: Vec<Song> = self.anchors.iter().map(|id| db.and_then(|db| db.song(id).ok().flatten()).unwrap_or_else(|| Song { id: id.clone(), ..Default::default() })).collect();
        let similar: Vec<Vec<Song>> = self.anchors.iter().filter_map(|a| self.similar.get(a).cloned()).collect();
        let day = playlog::now_ms().div_euclid(86_400_000);
        let lead = self.anchors[0].as_str();
        let recent = db.map(|db| recent_offers(db, lead, day)).unwrap_or_default();
        let fatigue = db.map(|db| fatigue::multipliers(db, playlog::now_ms())).unwrap_or_default();
        let inputs = plan::Inputs { anchors: &anchors, similar: &similar, deezer: self.deezer.as_ref().and_then(Option::as_ref), listenbrainz: &self.listenbrainz, recent: &recent, fatigue: &fatigue, day, provisional: !sealed };
        let history = history(db, queue);
        let songs: Vec<Song> = plan::build(db, &inputs, &|s, v| skip.skips(s, v) || queue.contains(&s.id), &history).into_iter().map(|p| p.song).collect();
        if sealed && let Some(db) = db {
            note_offers(db, lead, day, &songs);
        }
        songs
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// A list built around `anchors`, such as a For You mix, using the Navia plan without the
/// anchors themselves. Unlike Autoplay's plan it isn't noted as an offer and ignores held-back
/// artists.
pub fn list_from(db: Option<&Library>, anchors: &[Song], answers: crate::autoplay::Answers, limit: usize) -> Vec<Song> {
    if anchors.is_empty() {
        return Vec::new();
    }
    let now = playlog::now_ms();
    let day = now.div_euclid(86_400_000);
    let fatigue = db.map(|db| fatigue::multipliers(db, now)).unwrap_or_default();
    let recent = HashSet::new();
    let inputs = plan::Inputs { anchors, similar: &answers.similar, deezer: answers.deezer.as_ref(), listenbrainz: &answers.listenbrainz, recent: &recent, fatigue: &fatigue, day, provisional: false };
    let ids: HashSet<&str> = anchors.iter().map(|a| a.id.as_str()).collect();
    let versions: HashSet<String> = anchors.iter().map(crate::autoplay::song_version).collect();
    let junk = junk::Filter::after(anchors);
    let skip = |s: &Song, v: &str| ids.contains(s.id.as_str()) || versions.contains(v) || junk.leaves_out(s);
    plan::build(db, &inputs, &skip, &[]).into_iter().take(limit).map(|p| p.song).collect()
}

/// How many of a sealed plan's songs are remembered as offered for an anchor.
const RECENT_KEEP: usize = 50;
/// How many anchors' offers are remembered.
const RECENT_ANCHORS: usize = 300;
const RECENT_KEY: &str = "autoplay_recent";

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Offered {
    day: i64,
    ids: Vec<String>,
    /// What was offered on the most recent earlier day.
    before: Vec<String>,
    used: i64,
}

fn read_recent(db: &Library) -> HashMap<String, Offered> {
    db.meta(RECENT_KEY).ok().flatten().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// Songs offered for this anchor on an earlier day.
fn recent_offers(db: &Library, anchor: &str, day: i64) -> HashSet<String> {
    let all = read_recent(db);
    let Some(o) = all.get(anchor) else { return HashSet::new() };
    let ids = if o.day == day { &o.before } else { &o.ids };
    ids.iter().cloned().collect()
}

/// Remembers what a sealed plan offers for this anchor today.
fn note_offers(db: &Library, anchor: &str, day: i64, songs: &[Song]) {
    let mut all = read_recent(db);
    let ids: Vec<String> = songs.iter().take(RECENT_KEEP).map(|s| s.id.clone()).collect();
    let entry = all.entry(anchor.to_string()).or_default();
    if entry.day != day {
        entry.before = std::mem::take(&mut entry.ids);
        entry.day = day;
    }
    entry.ids = ids;
    entry.used = playlog::now_ms();
    if all.len() > RECENT_ANCHORS {
        let mut by_use: Vec<(String, i64)> = all.iter().map(|(k, v)| (k.clone(), v.used)).collect();
        by_use.sort_by_key(|e| e.1);
        for (k, _) in by_use.into_iter().take(all.len() - RECENT_ANCHORS) {
            all.remove(&k);
        }
    }
    if let Ok(text) = serde_json::to_string(&all) {
        let _ = db.set_meta(RECENT_KEY, Some(&text));
    }
}

/// The last `n` songs of `queue`, oldest first, from the library.
pub fn queue_tail(db: &Library, queue: &[String], n: usize) -> Vec<Song> {
    queue[queue.len().saturating_sub(n)..].iter().filter_map(|id| db.song(id).ok().flatten()).collect()
}

/// What spacing counts before the plan, the end of the queue.
fn history(db: Option<&Library>, queue: &[String]) -> Vec<sequence::Item> {
    let Some(db) = db else { return Vec::new() };
    queue_tail(db, queue, sequence::ARTIST_WINDOW).iter().map(sequence::Item::of).collect()
}

#[cfg(test)]
mod tests {
    use super::playlog::{Ended, Origin, now_ms, record_end, record_start};
    use super::*;
    use crate::client::ArtistRef;
    use crate::recommend::artist_key;

    fn song(id: &str, artist: &str) -> Song {
        Song { id: id.into(), title: id.into(), artist: Some(artist.into()), artists: vec![ArtistRef { id: format!("ar-{artist}"), name: artist.into() }], album_id: Some(format!("al-{id}")), ..Default::default() }
    }

    fn library() -> (tempfile::TempDir, Library) {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open(&crate::library::LibraryConfig { path: dir.path().join("lib.sqlite") }).unwrap();
        (dir, lib)
    }

    /// The server's answer for the anchor: 60 songs over 12 artists, best first.
    fn answer() -> Vec<Song> {
        (0..60).map(|i| song(&format!("s{i}"), &format!("A{}", i % 12))).collect()
    }

    #[test]
    fn an_app_list_ranks_the_plan_without_the_anchor_its_versions_or_junk() {
        let (_dir, db) = library();
        let anchor = song("a", "Lead");
        let mut similar = answer();
        let mut short = song("short", "A1");
        short.duration = Some(30);
        let mut karaoke = song("k", "A2");
        karaoke.title = "Hit (Karaoke Version)".into();
        let mut again = song("a2", "Lead");
        again.title = "a (Live)".into();
        similar.splice(0..0, [short, karaoke, again]);
        let answers = crate::autoplay::Answers { similar: vec![similar], ..Default::default() };
        let list = list_from(Some(&db), std::slice::from_ref(&anchor), answers, 20);
        assert_eq!(list.len(), 20);
        assert!(list.iter().all(|s| !["a", "a2", "short", "k"].contains(&s.id.as_str())), "{:?}", list.iter().map(|s| &s.id).collect::<Vec<_>>());
        assert!(list_from(Some(&db), &[], crate::autoplay::Answers::default(), 20).is_empty());
    }

    fn sealed(db: &Library, anchor: &str) -> Navia {
        let mut n = Navia::default();
        n.set_context(Some(db), vec![anchor.to_string()]);
        let seeds = n.wanted_fetch().unwrap();
        n.begin_fetch(&seeds);
        n.finish(seeds, crate::autoplay::Answers { similar: vec![answer()], ..Default::default() });
        n
    }

    fn nothing() -> Skip<'static> {
        Skip { ids: Vec::new(), versions: Vec::new(), junk: junk::Filter::default() }
    }

    fn ids(songs: &[Song]) -> Vec<String> {
        songs.iter().map(|s| s.id.clone()).collect()
    }

    #[test]
    fn early_skips_in_a_row_move_only_what_hasnt_been_offered() {
        let (_d, db) = library();
        let mut n = sealed(&db, "anchor");
        let shown = n.songs(Some(&db), &[], &nothing(), 10);
        assert!(n.sealed() && shown.len() == 10);
        let before = ids(n.planned());
        // One skip, then a song listened to, is no run.
        let shared = db.engine_shared();
        shared.note_play_end(Some(&artist_key(&shown[0])));
        shared.note_play_end(None);
        shared.note_play_end(Some(&artist_key(&shown[1])));
        assert!(shared.held_artists().is_empty());
        assert_eq!(ids(&n.songs(Some(&db), &[], &nothing(), 10)), ids(&shown));
        // Two in a row hold both artists back, but rows already offered stay.
        shared.note_play_end(Some(&artist_key(&shown[2])));
        let held = shared.held_artists();
        assert_eq!(held.len(), 2);
        assert_eq!(ids(&n.songs(Some(&db), &[], &nothing(), 10)), ids(&shown), "shown rows never move");
        let after = ids(n.planned());
        assert_eq!(after[..10], before[..10]);
        let mut sorted_a = after.clone();
        let mut sorted_b = before.clone();
        sorted_a.sort();
        sorted_b.sort();
        assert_eq!(sorted_a, sorted_b, "nothing dropped: held back, not banned");
        let plan = n.planned().to_vec();
        for (i, s) in plan.iter().enumerate().skip(10) {
            let old = before.iter().position(|id| *id == s.id).unwrap();
            if held.contains(&artist_key(s)) {
                assert!(i >= old, "{} moved up", s.id);
            }
        }
        let first_held = plan.iter().skip(10).position(|s| held.contains(&artist_key(s))).unwrap() + 10;
        let was = before.iter().skip(10).position(|id| plan.iter().any(|s| s.id == *id && held.contains(&artist_key(s)))).unwrap() + 10;
        assert!(first_held > was, "the first held-back song after the shown rows moved down ({was} → {first_held})");
        // A new context forgets them.
        n.set_context(Some(&db), vec!["other".into()]);
        assert!(db.engine_shared().held_artists().is_empty());
    }

    #[test]
    fn a_song_skipped_lately_moves_down_but_stays_and_finishing_it_brings_it_back() {
        let (_d, db) = library();
        let top = sealed(&db, "anchor").songs(Some(&db), &[], &nothing(), 60);
        let target = top[0].id.clone();
        let place = |db: &Library| ids(&sealed(db, "anchor").songs(Some(db), &[], &nothing(), 60)).iter().position(|id| *id == target);
        assert_eq!(place(&db), Some(0));
        // Reached by advancing and skipped early on three separate days.
        let now = now_ms();
        for d in (1..=3).rev() {
            let at = now - d * 86_400_000;
            let before = record_start(&db, "prev", Some(200_000), Origin::User, at - 200_000).unwrap();
            record_end(&db, before, 200_000, Ended::Finished, false, at).unwrap();
            let row = record_start(&db, &target, Some(200_000), Origin::Autoplay(AutoplayMode::Navia), at).unwrap();
            record_end(&db, row, 4_000, Ended::Skipped, false, at + 4_000).unwrap();
        }
        let tired = place(&db).expect("never left out");
        assert!(tired > 0, "moved down");
        // Still not past clearly less related songs. The server's 25th stays after it.
        assert!(tired < ids(&sealed(&db, "anchor").songs(Some(&db), &[], &nothing(), 60)).iter().position(|id| id == "s25").unwrap());
        // Listened through today, so back at the top.
        let row = record_start(&db, &target, Some(200_000), Origin::User, now - 300_000).unwrap();
        record_end(&db, row, 200_000, Ended::Finished, false, now - 100_000).unwrap();
        assert_eq!(place(&db), Some(0));
    }

    #[test]
    fn fewer_repeats_plays_a_song_skipped_lately_a_little_later() {
        let (_d, db) = library();
        let now = now_ms();
        let before = record_start(&db, "prev", Some(200_000), Origin::User, now - 400_000).unwrap();
        record_end(&db, before, 200_000, Ended::Finished, false, now - 200_000).unwrap();
        let row = record_start(&db, "tired", Some(200_000), Origin::User, now - 200_000).unwrap();
        record_end(&db, row, 3_000, Ended::Skipped, false, now - 197_000).unwrap();
        let r = db.recency_by_id(&["tired".to_string(), "fresh".to_string()], &HashSet::new(), now as u64).unwrap();
        assert!(r[0] > 0.0 && r[0] <= fatigue::RECENCY_MAX && r[1] == 0.0, "{r:?}");
    }
}
