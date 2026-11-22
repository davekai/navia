//! Smart transitions through the real engine, on generated songs: where the overlap lands, what
//! is skipped, beat alignment, and album joins staying gapless. audio only goes into memory.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use navia_core::audio::capture;
use navia_core::audio::edges::{self, EdgeStore, Edges, SongFacts};
use navia_core::audio::engine::{AlbumSpot, EngineConfig, EngineEvent, OutputMode, TrackFeed, TrackRequest, output_from, spawn_feed};
use navia_core::audio::output::CaptureBackend;
use navia_core::audio::player::{FileResolver, Player, PlayerConfig, PlayerStatus, QueueItem};
use navia_core::audio::transition::{self, Join, JoinKind, SongTraits, TraitsCell, Transition};

const RATE: u32 = 48_000;
const CROSSFADE_MS: u32 = 6_000;

fn frames(secs: f64) -> usize {
    (secs * f64::from(RATE)) as usize
}

fn ms_frames(ms: u32) -> usize {
    (u64::from(ms) * u64::from(RATE) / 1000) as usize
}

/// noise with a 60 Hz kick every `beat` s from `first_kick` s on.
fn music(secs: f64, noise: f32, kick: f32, first_kick: f64, beat: f64, seed: u32) -> Vec<f32> {
    let mut state = seed.wrapping_mul(2_654_435_761).max(1);
    (0..frames(secs))
        .map(|n| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let t = n as f64 / f64::from(RATE);
            let since = (t - first_kick).rem_euclid(beat);
            let on = t >= first_kick;
            let k = if on && since < 0.08 { (since * 60.0 * std::f64::consts::TAU).sin() as f32 * kick * (1.0 - since as f32 / 0.08) } else { 0.0 };
            let hiss = (state as f32 / u32::MAX as f32 - 0.5) * noise;
            // the beater's click, so the kick is heard across the spectrum.
            let click = if on && kick > 0.0 && since < 0.006 { hiss * 6.0 } else { 0.0 };
            hiss + k + click
        })
        .collect()
}

fn fade_out(mut x: Vec<f32>) -> Vec<f32> {
    let n = x.len();
    x.iter_mut().enumerate().for_each(|(i, s)| *s *= 1.0 - i as f32 / n as f32);
    x
}

fn fade_in(mut x: Vec<f32>) -> Vec<f32> {
    let n = x.len();
    x.iter_mut().enumerate().for_each(|(i, s)| *s *= i as f32 / n as f32);
    x
}

fn silence(secs: f64) -> Vec<f32> {
    vec![0.0; frames(secs)]
}

/// write mono as a 16-bit stereo WAV; returns the samples as the decoder will give them.
fn write_wav(path: &Path, mono: &[f32]) -> Vec<f32> {
    let pcm: Vec<i16> = mono.iter().flat_map(|s| {
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        [v, v]
    }).collect();
    let data_len = (pcm.len() * 2) as u32;
    let mut f = Vec::with_capacity(44 + pcm.len() * 2);
    for chunk in [&b"RIFF"[..], &(36 + data_len).to_le_bytes(), b"WAVEfmt ", &16u32.to_le_bytes(), &1u16.to_le_bytes(), &2u16.to_le_bytes(), &RATE.to_le_bytes(), &(RATE * 4).to_le_bytes(), &4u16.to_le_bytes(), &16u16.to_le_bytes(), b"data", &data_len.to_le_bytes()] {
        f.extend_from_slice(chunk);
    }
    pcm.iter().for_each(|s| f.extend_from_slice(&s.to_le_bytes()));
    std::fs::write(path, f).unwrap();
    pcm.iter().map(|s| *s as f32 / 32768.0).collect()
}

struct Song {
    path: PathBuf,
    samples: Vec<f32>,
    traits: SongTraits,
}

fn song(dir: &Path, name: &str, mono: Vec<f32>, bpm: Option<f32>) -> Song {
    let path = dir.join(format!("{name}.wav"));
    let samples = write_wav(&path, &mono);
    let edges = edges::analyze(TrackRequest::file(0, path.clone()).opener.as_ref(), None).unwrap();
    Song { path, samples, traits: SongTraits { song_id: name.to_string(), edges: Some(edges), facts: SongFacts { bpm, track_gain_db: None } } }
}

/// the queue, which a test can change while the engine plays it.
#[derive(Clone)]
struct ListFeed(Arc<Mutex<Vec<TrackRequest>>>, Arc<Mutex<usize>>);

impl ListFeed {
    fn new(reqs: Vec<TrackRequest>) -> Self {
        Self(Arc::new(Mutex::new(reqs)), Arc::new(Mutex::new(1)))
    }

    fn position(&self, token: usize) -> Option<usize> {
        self.0.lock().unwrap().iter().position(|r| r.token == token)
    }
}

impl TrackFeed for ListFeed {
    fn peek(&mut self) -> Option<TrackRequest> {
        self.0.lock().unwrap().get(*self.1.lock().unwrap()).cloned()
    }

    fn advance(&mut self) -> Option<TrackRequest> {
        let mut index = self.1.lock().unwrap();
        let r = self.0.lock().unwrap().get(*index).cloned();
        *index += r.is_some() as usize;
        r
    }

    fn following(&mut self, token: usize) -> Option<Option<usize>> {
        let at = self.position(token)?;
        Some(self.0.lock().unwrap().get(at + 1).map(|r| r.token))
    }

    fn rewind_to(&mut self, token: usize) -> bool {
        let Some(at) = self.position(token) else { return false };
        *self.1.lock().unwrap() = at + 1;
        true
    }
}

fn config() -> EngineConfig {
    let mut cfg = EngineConfig::new(OutputMode::FollowTrack);
    cfg.crossfade_frames = ms_frames(CROSSFADE_MS);
    cfg.smart_transitions = true;
    cfg
}

fn no_errors(c: &capture::Capture) {
    let errors: Vec<_> = c.events.iter().filter(|e| matches!(e, EngineEvent::Error { .. })).collect();
    assert!(errors.is_empty(), "{errors:?}");
}

fn request(token: usize, s: &Song, album: Option<AlbumSpot>) -> TrackRequest {
    let slot = Arc::new(TraitsCell::default());
    slot.set(s.traits.clone());
    TrackRequest { album, traits: Some(slot), ..TrackRequest::file(token, s.path.clone()) }
}

fn run(songs: &[&Song], albums: Vec<Option<AlbumSpot>>) -> capture::Capture {
    let reqs: Vec<TrackRequest> = songs.iter().zip(albums).enumerate().map(|(i, (s, a))| request(i, s, a)).collect();
    let first = reqs[0].clone();
    let c = capture::run(output_from(spawn_feed(Box::new(ListFeed::new(reqs)), config(), Some(first), true)));
    no_errors(&c);
    c
}

/// where track `token` starts in the output, and its overlap.
fn start_of(c: &capture::Capture, token: usize) -> (usize, usize) {
    c.events
        .iter()
        .find_map(|e| match e {
            EngineEvent::TrackStart { track, output_frame, overlap, .. } if *track == token => Some((*output_frame as usize, *overlap)),
            _ => None,
        })
        .expect("track started")
}

/// the plan the engine should make for this pair: what it holds is the song's measured need.
fn expected_plan(out: &Song, inc: &Song) -> Transition {
    let held = transition::hold_ms(out.traits.edges.as_ref().unwrap()).min(edges::EDGE_WINDOW_MS as u32).max(CROSSFADE_MS);
    let held_ms = held.min((out.samples.len() / 2 * 1000 / RATE as usize) as u32);
    transition::plan(Join { out: Some(&out.traits), inc: Some(&inc.traits), out_gain: 1.0, in_gain: 1.0, continues: false, plain_ms: CROSSFADE_MS, held_ms })
}

fn bit_exact(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
}

#[test]
fn without_a_tempo_the_plain_crossfade_ends_at_the_last_sound_and_skips_the_intro_silence() {
    let dir = tempfile::tempdir().unwrap();
    let out = song(dir.path(), "out", [music(20.0, 0.4, 0.0, 0.0, 1.0, 1), fade_out(music(6.0, 0.4, 0.0, 0.0, 1.0, 2)), silence(1.0)].concat(), None);
    let inc = song(dir.path(), "in", [silence(0.6), fade_in(music(3.0, 0.4, 0.0, 0.0, 1.0, 11)), music(20.0, 0.4, 0.0, 0.0, 1.0, 3)].concat(), None);
    let plan = expected_plan(&out, &inc);
    assert_eq!(plan.kind, JoinKind::Plain, "{plan}");
    let tail = &out.traits.edges.as_ref().unwrap().tail;
    assert_eq!((plan.overlap_ms, plan.start_before_ms), (CROSSFADE_MS, tail.silence_ms + CROSSFADE_MS), "{plan}");
    assert!(plan.skip_ms >= 550, "{plan}");

    let c = run(&[&out, &inc], vec![None, None]);
    let (out_len, in_len) = (out.samples.len() / 2, inc.samples.len() / 2);
    let (start, overlap, skip) = (ms_frames(plan.start_before_ms), ms_frames(plan.overlap_ms), ms_frames(plan.skip_ms));
    assert_eq!(start_of(&c, 1), (out_len - start, overlap));
    assert_eq!(c.samples.len() / 2, out_len - start + in_len - skip, "the outgoing's faded end and the incoming's silence are gone");
    assert!(bit_exact(&c.samples[..(out_len - start) * 2], &out.samples[..(out_len - start) * 2]), "the outgoing song is untouched up to the overlap");
    let after = out_len - start + overlap;
    assert!(bit_exact(&c.samples[after * 2..], &inc.samples[(skip + overlap) * 2..]), "the incoming song is untouched after the overlap");
    let dropped = &out.samples[after * 2..];
    let rms_db = 10.0 * (dropped.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>() / dropped.len().max(1) as f64).max(1e-12).log10();
    assert!(rms_db < f64::from(edges::SILENCE_DBFS), "only silence is dropped: {rms_db:.1} dBFS over {} frames", dropped.len() / 2);
}

#[test]
fn matching_songs_blend_from_a_phrase_start_with_a_downbeat_on_a_beat() {
    let dir = tempfile::tempdir().unwrap();
    let (beat, out_phase, in_kick) = (0.5, 0.13, 0.51);
    let out = song(dir.path(), "out", [music(30.0, 0.1, 0.6, out_phase, beat, 4), fade_out(music(10.0, 0.1, 0.6, out_phase + 30.0, beat, 5))].concat(), Some(120.0));
    let inc = song(dir.path(), "in", [silence(0.3), fade_in(music(4.0, 0.1, 0.6, in_kick - 0.3, beat, 12)), music(20.0, 0.1, 0.6, in_kick - 0.3, beat, 6)].concat(), Some(121.0));
    let plan = expected_plan(&out, &inc);
    assert_eq!(plan.kind, JoinKind::Blend, "{plan}");
    let silence = out.traits.edges.as_ref().unwrap().tail.silence_ms;
    assert_eq!(plan.start_before_ms - plan.overlap_ms, silence, "ends at the last sound: {plan}");

    let c = run(&[&out, &inc], vec![None, None]);
    let (from, overlap) = start_of(&c, 1);
    assert_eq!(overlap, ms_frames(plan.overlap_ms));
    // the incoming song's first kick, in the output, against the outgoing song's kicks.
    let landing = from + frames(in_kick) - ms_frames(plan.skip_ms);
    let off = (landing as i64 - frames(out_phase) as i64).rem_euclid(frames(beat) as i64);
    let off = off.min(frames(beat) as i64 - off);
    assert!(off <= frames(0.012) as i64, "the first kick lands {} ms off the beat ({plan:?})", off * 1000 / i64::from(RATE));
}

#[test]
fn an_album_in_order_stays_gapless_whatever_the_silence() {
    let dir = tempfile::tempdir().unwrap();
    let a = song(dir.path(), "a", music(12.0, 0.4, 0.0, 0.0, 1.0, 7), None);
    let b = song(dir.path(), "b", music(12.0, 0.4, 0.0, 0.0, 1.0, 8), None);
    let spot = |t| Some(AlbumSpot { album_id: "al".into(), disc: Some(1), track: Some(t) });
    let c = run(&[&a, &b], vec![spot(1), spot(2)]);
    assert!(bit_exact(&c.samples, &[a.samples.clone(), b.samples.clone()].concat()), "bit-exact through the join");

    // a second of silence between the tracks is the album's own, and plays as released.
    let gap = song(dir.path(), "gap", [music(12.0, 0.4, 0.0, 0.0, 1.0, 9), silence(1.0)].concat(), None);
    let c = run(&[&gap, &b], vec![spot(1), spot(2)]);
    assert!(bit_exact(&c.samples, &[gap.samples.clone(), b.samples.clone()].concat()), "bit-exact with the silence kept");
}

#[test]
fn a_queue_change_while_the_long_end_is_held_keeps_the_playing_song_whole() {
    let dir = tempfile::tempdir().unwrap();
    let out = song(dir.path(), "out", [music(25.0, 0.4, 0.0, 0.0, 1.0, 13), fade_out(music(10.0, 0.4, 0.0, 0.0, 1.0, 14))].concat(), None);
    let b = song(dir.path(), "b", fade_in(music(8.0, 0.4, 0.0, 0.0, 1.0, 15)), None);
    let c = song(dir.path(), "c", fade_in(music(8.0, 0.4, 0.0, 0.0, 1.0, 16)), None);
    let plan = expected_plan(&out, &c);
    let out_len = out.samples.len() / 2;
    let start = ms_frames(plan.start_before_ms);
    let held = ms_frames(transition::hold_ms(out.traits.edges.as_ref().unwrap()));
    assert!(held > start + frames(4.0), "a long stretch is held before the overlap ({plan:?})");

    let feed = ListFeed::new(vec![request(0, &out, None), request(1, &b, None)]);
    let mut cfg = config();
    cfg.ring_frames = frames(0.5);
    let first = feed.0.lock().unwrap()[0].clone();
    let mut output = output_from(spawn_feed(Box::new(feed.clone()), cfg, Some(first), true));
    // play up to the middle of the held stretch: the engine is writing it out, with B decoding next.
    let switch_at = out_len - (held + start) / 2;
    let mut samples = Vec::new();
    let mut block = vec![0.0f32; 1024];
    while samples.len() / 2 < switch_at {
        let want = (switch_at - samples.len() / 2).min(512) * 2;
        let n = output.fill(&mut block[..want]);
        samples.extend_from_slice(&block[..n]);
        if n == 0 {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    feed.0.lock().unwrap()[1] = request(2, &c, None);
    output.control.realign();
    let rest = capture::run(output);
    samples.extend_from_slice(&rest.samples);
    no_errors(&rest);

    assert!(!rest.events.iter().any(|e| matches!(e, EngineEvent::TrackStart { track: 1, .. })), "B never starts");
    let (c_at, overlap) = start_of(&rest, 2);
    assert_eq!((c_at, overlap), (out_len - start, ms_frames(plan.overlap_ms)), "C joins where the plan says");
    assert!(bit_exact(&samples[..c_at * 2], &out.samples[..c_at * 2]), "nothing of the playing song is lost");
}

#[derive(Default)]
struct MemoryStore {
    kept: Mutex<HashMap<String, Edges>>,
    asked: Mutex<Vec<String>>,
}

struct Shared(Arc<MemoryStore>);

impl EdgeStore for Shared {
    fn facts(&self, song_id: &str) -> SongFacts {
        self.0.asked.lock().unwrap().push(song_id.to_string());
        SongFacts::default()
    }

    fn edges(&self, song_id: &str) -> Option<Edges> {
        self.0.kept.lock().unwrap().get(song_id).cloned()
    }

    fn keep(&self, song_id: &str, edges: &Edges) {
        self.0.kept.lock().unwrap().insert(song_id.to_string(), edges.clone());
    }
}

#[test]
fn the_player_measures_the_playing_and_next_songs_in_the_background() {
    let dir = tempfile::tempdir().unwrap();
    let mut files = HashMap::new();
    for i in 0..3 {
        let path = dir.path().join(format!("{i}.wav"));
        write_wav(&path, &[music(3.0, 0.4, 0.0, 0.0, 1.0, 10 + i), silence(0.5)].concat());
        files.insert(format!("s{i}"), path);
    }
    let store = Arc::new(MemoryStore::default());
    let backend = CaptureBackend { speed: 0.0, ..Default::default() };
    let player = Player::with_backend(Arc::new(FileResolver(files)), Box::new(backend), PlayerConfig::default()).unwrap();
    player.set_edge_store(Arc::new(Shared(store.clone())));
    player.set_crossfade_ms(CROSSFADE_MS);
    player.set_smart_transitions(true);
    player.set_queue((0..3).map(|i| QueueItem::new(format!("s{i}"), Some(3_500))).collect(), 0, 0, false);
    let start = Instant::now();
    while player.snapshot().status != PlayerStatus::Stopped || store.kept.lock().unwrap().len() < 3 {
        assert!(start.elapsed() < Duration::from_secs(20), "didn't finish: {:?}", store.kept.lock().unwrap().keys().collect::<Vec<_>>());
        std::thread::sleep(Duration::from_millis(5));
    }
    let kept = store.kept.lock().unwrap();
    assert!(kept.values().all(|e| e.tail.silence_ms.abs_diff(500) <= 20), "{kept:?}");
    let mut asked = store.asked.lock().unwrap().clone();
    asked.sort();
    assert_eq!(asked, ["s0", "s1", "s2"], "each song is measured once");
}
