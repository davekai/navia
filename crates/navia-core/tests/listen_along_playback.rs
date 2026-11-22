//! Listening along in each between-songs mode, against the real engine and an in-memory capture device.
//! A simulated host moves through songs, and the listener follows with the core's own follow steps.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use navia_core::audio::engine::AlbumSpot;
use navia_core::audio::output::{CaptureBackend, CaptureBuffer};
use navia_core::audio::player::{FileResolver, Player, PlayerConfig, PlayerStatus, QueueItem};
use navia_core::client::Song;
use navia_core::lanyard::{self, Friend, FriendNow, Reach, SpotifyTrack};
use navia_core::listen_along::{self as la, FollowStep, Followed, Local, NowPlaying, SessionState, SHARE_MARK};

const RATE: usize = 48_000;
/// Song length. Short songs keep runs quick, while the 5 s ring and a 3 s crossfade still fit.
const SONG_S: usize = 20;
const CROSSFADE_MS: u32 = 3_000;
/// How fast the capture device runs, as a multiple of real time.
const SPEED: f64 = 4.0;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    Gapless,
    Crossfade,
    Smart,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    /// Someone sharing on the same server, read from `getNowPlaying`.
    Server,
    /// A Spotify friend through Lanyard.
    Spotify,
}

/// A 48 kHz WAV whose left channel says which song it is and whose right channel counts tens of ms,
/// so the capture tells what was heard.
fn song_wav(path: &Path, level: i16) {
    let frames = SONG_S * RATE;
    let mut data = Vec::with_capacity(frames * 4);
    for n in 0..frames {
        data.extend_from_slice(&level.to_le_bytes());
        data.extend_from_slice(&((n / 480) as i16).to_le_bytes());
    }
    let mut f = Vec::new();
    for chunk in [&b"RIFF"[..], &(36 + data.len() as u32).to_le_bytes(), b"WAVEfmt ", &16u32.to_le_bytes(), &1u16.to_le_bytes(), &2u16.to_le_bytes(), &48_000u32.to_le_bytes(), &192_000u32.to_le_bytes(), &4u16.to_le_bytes(), &16u16.to_le_bytes(), b"data", &(data.len() as u32).to_le_bytes(), &data] {
        f.extend_from_slice(chunk);
    }
    std::fs::write(path, f).unwrap();
}

/// What one stretch of the capture holds.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Heard {
    Silence,
    /// Song number and where in it, in ms.
    Song(usize, u64),
    /// Two songs mixed in a crossfade.
    Mix,
}

fn heard(frame: &[f32]) -> Heard {
    if frame[0] == 0.0 && frame[1] == 0.0 {
        return Heard::Silence;
    }
    let level = frame[0] * 32768.0 / 1000.0;
    if (level - level.round()).abs() > 0.001 || level.round() < 1.0 {
        return Heard::Mix;
    }
    Heard::Song(level.round() as usize - 1, (frame[1] * 32768.0).round() as u64 * 10)
}

struct Rig {
    _dir: tempfile::TempDir,
    player: Player,
    capture: Arc<CaptureBuffer>,
}

impl Rig {
    fn new(mode: Mode) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut files: HashMap<String, PathBuf> = HashMap::new();
        for k in 0..6 {
            let path = dir.path().join(format!("{k}.wav"));
            song_wav(&path, 1000 * (k as i16 + 1));
            files.insert(format!("s{k}"), path);
        }
        let backend = CaptureBackend { speed: SPEED, ..Default::default() };
        let capture = backend.capture.clone();
        let player = Player::with_backend(Arc::new(FileResolver(files)), Box::new(backend), PlayerConfig::default()).unwrap();
        // s0 s1 continue one album, as do s2 s3, so Smart has album joins to keep gapless.
        player.set_album_lookup(Arc::new(|id: &str| {
            let k: i32 = id.trim_start_matches('s').parse().ok()?;
            Some(AlbumSpot { album_id: format!("al{}", k / 2), disc: Some(1), track: Some(k % 2 + 1) })
        }));
        player.set_crossfade_ms(if mode == Mode::Gapless { 0 } else { CROSSFADE_MS });
        player.set_smart_crossfade(mode == Mode::Smart);
        Rig { _dir: dir, player, capture }
    }

    /// The capture's clock, in ms of audio since the device opened.
    fn now_ms(&self) -> u64 {
        (self.capture.len() / 2 * 1000 / RATE) as u64
    }

    fn wait_until(&self, ms: u64) {
        let limit = Instant::now() + Duration::from_secs(60);
        while self.now_ms() < ms {
            assert!(Instant::now() < limit, "the capture stalled at {} ms", self.now_ms());
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn wait_position(&self, ms: u64) {
        let limit = Instant::now() + Duration::from_secs(60);
        while self.player.snapshot().position_ms < ms {
            assert!(Instant::now() < limit, "never reached {ms} ms: {:?}", self.player.snapshot().status);
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// What was heard from `from_ms` on, in 50 ms steps, sampled at each step's middle frame.
    fn heard_from(&self, from_ms: u64) -> Vec<(u64, Heard)> {
        let samples = self.capture.samples();
        let step = RATE / 20;
        let first = from_ms as usize * RATE / 1000;
        (first..samples.len() / 2).step_by(step).filter(|f| (f + step / 2) * 2 + 1 < samples.len()).map(|f| ((f * 1000 / RATE) as u64, heard(&samples[(f + step / 2) * 2..]))).collect()
    }

    /// The first frame of audio from `from_ms` on, with its time in ms.
    fn first_audio_after(&self, from_ms: u64) -> Option<(u64, Heard)> {
        let samples = self.capture.samples();
        let first = from_ms as usize * RATE / 1000;
        (first..samples.len() / 2).map(|f| (f, heard(&samples[f * 2..]))).find(|(_, h)| *h != Heard::Silence).map(|(f, h)| ((f * 1000 / RATE) as u64, h))
    }
}

fn song(k: usize) -> Song {
    Song { id: format!("s{k}"), title: format!("Song {k}"), album_id: Some(format!("al{}", k / 2)), duration: Some(SONG_S as i64), ..Default::default() }
}

/// Where the host is, a song and position, playing or paused.
#[derive(Clone, Copy, Debug)]
struct HostNow {
    song: usize,
    position_ms: u64,
    playing: bool,
}

const FRIEND_ID: &str = "123456789012345678";

/// The listener's side of following, as the apps do it, with the followed song alone in the player.
struct Listener<'a> {
    rig: &'a Rig,
    kind: Kind,
    followed: Followed,
    song: usize,
    held: bool,
}

impl<'a> Listener<'a> {
    fn start(rig: &'a Rig, kind: Kind, host: HostNow) -> Self {
        let player = match kind {
            Kind::Server => format!("Navia{SHARE_MARK}"),
            Kind::Spotify => lanyard::friend_player(FRIEND_ID),
        };
        let mut me = Listener { rig, kind, followed: Followed { user: "Jo".into(), player }, song: usize::MAX, held: false };
        me.load(host.song, host.position_ms, host.playing);
        me
    }

    fn load(&mut self, k: usize, at: u64, playing: bool) {
        self.rig.player.set_following(true);
        self.rig.player.set_queue(vec![QueueItem::new(format!("s{k}"), Some(SONG_S as u64 * 1000))], 0, at, !playing);
        self.song = k;
    }

    fn local(&self) -> Local {
        let snap = self.rig.player.snapshot();
        Local { song_id: format!("s{}", self.song), position_ms: snap.position_ms, playing: matches!(snap.status, PlayerStatus::Playing | PlayerStatus::Buffering), held: self.held, ..Default::default() }
    }

    /// One check, with the host as the server or Lanyard reports it now.
    fn check(&mut self, host: HostNow) {
        let now_ms = 10_000_000;
        let step = match self.kind {
            Kind::Server => {
                let entry = NowPlaying { user: "Jo".into(), player: self.followed.player.clone(), song: song(host.song), state: Some(if host.playing { SessionState::Playing } else { SessionState::Paused }), position_ms: Some(host.position_ms), minutes_ago: 0 };
                la::follow_step(&self.followed, &[entry], &self.local())
            }
            Kind::Spotify => {
                // Spotify drops a paused friend's activity, so this host never pauses on Spotify.
                let start = now_ms - host.position_ms;
                let track = SpotifyTrack { song: format!("Song {}", host.song), start_ms: Some(start), end_ms: Some(start + SONG_S as u64 * 1000), ..Default::default() };
                let friend = FriendNow { friend: Friend { id: FRIEND_ID.into(), name: Some("Jo".into()) }, reach: Reach::Readable, user: None, track: Some(track), quiet_since_ms: None, song: Some(song(host.song)) };
                lanyard::friend_step(&self.followed, &[friend], &self.local(), now_ms)
            }
        };
        match step {
            FollowStep::Resync { entry, .. } => {
                self.held = false;
                let k: usize = entry.song.id.trim_start_matches('s').parse().unwrap();
                let at = entry.position_ms.unwrap_or(0);
                let playing = entry.state.is_some_and(SessionState::is_playing);
                if k != self.song {
                    self.load(k, at, playing);
                    return;
                }
                if self.local().position_ms.abs_diff(at) > la::RESYNC_DRIFT.as_millis() as u64 {
                    self.rig.player.seek_ms(at);
                }
                if playing != self.local().playing {
                    if playing { self.rig.player.play() } else { self.rig.player.pause() }
                }
            }
            FollowStep::Keep { .. } => {}
            other => panic!("unexpected step {other:?}"),
        }
    }

    /// Following ends. The listener's own queue goes back around the followed song,
    /// and their own song carries on from where it was.
    fn stop(self, own_before: Vec<usize>, own_after: Vec<usize>) {
        self.rig.player.set_following(false);
        let item = |k: &usize| QueueItem::new(format!("s{k}"), Some(SONG_S as u64 * 1000));
        if !own_before.is_empty() {
            self.rig.player.insert_at(0, own_before.iter().map(item).collect());
        }
        self.rig.player.insert_at(own_before.len() + 1, own_after.iter().map(item).collect());
    }
}

/// Runs of the same sound at least `min` steps long, as (start ms, what).
/// A crossfade is seconds of [`Heard::Mix`], while a step or two is a volume ramp or a coincidence.
fn runs(heard: &[(u64, Heard)], min: usize) -> Vec<(u64, String)> {
    let tag = |h: &Heard| match h {
        Heard::Song(k, _) => format!("s{k}"),
        Heard::Mix => "mix".to_string(),
        Heard::Silence => "-".to_string(),
    };
    let mut out: Vec<(u64, String)> = Vec::new();
    let mut i = 0;
    while i < heard.len() {
        let t = tag(&heard[i].1);
        let j = (i..heard.len()).find(|&j| tag(&heard[j].1) != t).unwrap_or(heard.len());
        if j - i >= min && out.last().is_none_or(|l| l.1 != t) {
            out.push((heard[i].0, t));
        }
        i = j;
    }
    out
}

/// Silent stretches at least `min_ms` long in `heard`, as (from, to).
fn silences(heard: &[(u64, Heard)], min_ms: u64) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    let mut start = None;
    for &(at, h) in heard {
        match (h, start) {
            (Heard::Silence, None) => start = Some(at),
            (Heard::Silence, Some(_)) => {}
            (_, Some(s)) => {
                if at - s >= min_ms {
                    out.push((s, at));
                }
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push((s, heard.last().map_or(s, |h| h.0)));
    }
    out
}

/// A whole session in one mode, following one kind of host.
/// Checks run every half second, harsher than either real pace, while the host changes songs and seeks
/// and the listener pauses.
fn follow_session(mode: Mode, kind: Kind) {
    let rig = Rig::new(mode);
    // The listener's own queue plays from near s0's end, so a fade is under way.
    rig.player.set_queue(vec![QueueItem::new("s0".into(), Some(20_000)), QueueItem::new("s4".into(), Some(20_000))], 0, 14_000, false);
    rig.wait_position(18_400);
    let snap = rig.player.snapshot();
    let mid_fade = mode != Mode::Gapless && snap.item.as_ref().is_some_and(|i| i.id == "s4");
    // The host plays s2 from 6 s, then s3, a jump in s3, then s5 cut in mid-song.
    let t0 = rig.now_ms();
    let host = move |t: u64| -> HostNow {
        let e = t.saturating_sub(t0) + 6_000;
        if e < 20_000 {
            HostNow { song: 2, position_ms: e, playing: true }
        } else if e < 25_000 {
            HostNow { song: 3, position_ms: e - 20_000, playing: true }
        } else if e < 30_000 {
            // They seek forward in s3.
            HostNow { song: 3, position_ms: e - 25_000 + 12_000, playing: true }
        } else {
            HostNow { song: 5, position_ms: e - 30_000 + 1_000, playing: true }
        }
    };
    let mut me = Listener::start(&rig, kind, host(t0));
    let started = rig.first_audio_after_until(t0, 3_000);
    if mode != Mode::Gapless {
        assert!(mid_fade, "{mode:?}: following should start mid-crossfade");
    }
    let mut t = t0;
    // For a server host, the listener pauses on s3 while the host carries on, and the host's next song
    // plays here anyway. A Spotify friend's song change doesn't lift a held pause, so that run doesn't pause.
    let pause_at = if kind == Kind::Server { t0 + 20_000 } else { u64::MAX };
    let mut paused_from = u64::MAX;
    let resume_due = t0 + 24_000; // When the host moves to s5.
    while t < t0 + 30_000 {
        t += 500;
        rig.wait_until(t);
        if t >= pause_at && paused_from == u64::MAX {
            rig.player.pause();
            me.held = true;
            paused_from = t;
        }
        me.check(host(t));
    }
    let end_follow = t;
    // Following ends and the own queue comes back, with s1 after the followed song.
    me.stop(vec![0], vec![1]);
    let total = SONG_S as u64 * 1000;
    rig.wait_until(end_follow + total + 4_000);

    let what = format!("{mode:?} following {kind:?}");
    let h = rig.heard_from(t0);
    let at = |ms: u64| h.iter().rev().find(|(a, _)| *a <= ms).map(|x| x.1).unwrap();
    // 1. Their song comes in at once, cut, never faded into the listener's own song.
    let first = h.iter().find(|(_, x)| *x != Heard::Silence && !matches!(x, Heard::Song(0 | 4, _))).copied();
    let (first_ms, first) = first.unwrap_or_else(|| panic!("{what}: nothing of theirs heard"));
    assert!(first_ms - t0 < 400, "{what}: their song took {} ms to start", first_ms - t0);
    assert!(matches!(first, Heard::Song(2, p) if p.abs_diff(6_000) < 400), "{what}: started with {first:?}");
    assert!(started.is_some(), "{what}: silent after following started");
    // 2. While following nothing crossfades.
    let following: Vec<_> = h.iter().filter(|(a, _)| *a >= first_ms && *a < end_follow).copied().collect();
    let fades: Vec<_> = runs(&following, 3).into_iter().filter(|r| r.1 == "mix").map(|r| r.0 - t0).collect();
    assert!(fades.is_empty(), "{what}: a crossfade while following at {fades:?}");
    // 3. Each of the host's songs is heard, near where the host is.
    for probe in [t0 + 8_000, t0 + 16_000, t0 + 19_900] {
        let want = host(probe);
        match at(probe) {
            Heard::Song(k, p) if k == want.song && p.abs_diff(want.position_ms) <= 2_600 => {}
            other => panic!("{what}: at {} ms heard {other:?}, the host is on {want:?}", probe - t0),
        }
    }
    // 4. No silence while the host plays and the listener hasn't paused, beyond a check's worth
    // around a song change.
    let quiet: Vec<_> = silences(&h, 1_000).into_iter().filter(|(a, b)| *b <= paused_from.min(end_follow) || (*a >= resume_due + 1_500 && *b <= end_follow)).collect();
    assert!(quiet.is_empty(), "{what}: silent while following at {:?} (from following's start)", quiet.iter().map(|(a, b)| (a - t0, b - t0)).collect::<Vec<_>>());
    // 5. The held pause is silent until the host's next song, which then plays.
    if paused_from != u64::MAX {
    let after_pause = rig.first_audio_after(paused_from + 300).expect("something after the pause");
    assert!(after_pause.0 >= resume_due - 500, "{what}: the pause didn't hold ({} ms)", after_pause.0 - t0);
    let next = runs(&rig.heard_from(after_pause.0), 3).into_iter().next().map(|r| r.1);
    assert_eq!(next.as_deref(), Some("s5"), "{what}: after the pause");
    }
    // 6. After following, the followed song plays out and the own queue carries on. The join into it is cut,
    // since it was decoded while following, and the next own join follows the mode.
    let after: Vec<_> = h.iter().filter(|(a, _)| *a >= end_follow).collect();
    let own = after.iter().find(|(_, x)| matches!(x, Heard::Song(1, _))).unwrap_or_else(|| panic!("{what}: own queue never came back"));
    assert!(matches!(at(own.0 - 50), Heard::Song(5, _)), "{what}: the own song follows their last song directly");
    let before_own: Vec<_> = after.iter().filter(|(a, _)| *a < own.0).map(|x| **x).collect();
    assert!(runs(&before_own, 3).iter().all(|r| r.1 != "mix"), "{what}: their last song faded into the own queue");
    assert_eq!(rig.player.snapshot().crossfade_ms, if mode == Mode::Gapless { 0 } else { CROSSFADE_MS }, "{what}: the setting is kept");
    rig.player.shutdown();
}

impl Rig {
    /// [`Self::first_audio_after`] once it has had `ms` of audio to show up.
    fn first_audio_after_until(&self, from_ms: u64, ms: u64) -> Option<(u64, Heard)> {
        self.wait_until(from_ms + ms);
        self.first_audio_after(from_ms)
    }
}

#[test]
fn following_on_the_server_gapless() {
    follow_session(Mode::Gapless, Kind::Server);
}

#[test]
fn following_on_the_server_with_crossfade() {
    follow_session(Mode::Crossfade, Kind::Server);
}

#[test]
fn following_on_the_server_with_smart_crossfade() {
    follow_session(Mode::Smart, Kind::Server);
}

#[test]
fn following_a_spotify_friend_gapless() {
    follow_session(Mode::Gapless, Kind::Spotify);
}

#[test]
fn following_a_spotify_friend_with_crossfade() {
    follow_session(Mode::Crossfade, Kind::Spotify);
}

#[test]
fn following_a_spotify_friend_with_smart_crossfade() {
    follow_session(Mode::Smart, Kind::Spotify);
}

/// The last song in the queue ends with a crossfade set, so its held tail is written seconds before it's heard.
/// A song change or seek arriving then must still land. Following is off, so this is the engine's own behavior.
fn change_while_the_tail_is_written(mode: Mode, seek: bool) {
    let rig = Rig::new(mode);
    rig.player.set_queue(vec![QueueItem::new("s2".into(), Some(20_000))], 0, 4_000, false);
    // The decoder is done with s2 once the 5 s ring and the 3 s held tail cover its end,
    // and writing that tail takes until 5 s before the end.
    rig.wait_position(13_000);
    let at = rig.now_ms();
    if seek {
        rig.player.seek_ms(2_000);
    } else {
        rig.player.set_queue(vec![QueueItem::new("s3".into(), Some(20_000))], 0, 1_000, false);
    }
    rig.wait_until(at + 3_000);
    let what = format!("{mode:?}, {} while the end is written", if seek { "a seek" } else { "a new song" });
    let (when, first) = rig.first_audio_after(at + 250).unwrap_or_else(|| panic!("{what}: silent"));
    assert!(when - at < 1_000, "{what}: {} ms of old audio or silence", when - at);
    let want = if seek { 2 } else { 3 };
    assert!(matches!(first, Heard::Song(k, _) if k == want), "{what}: heard {first:?}");
    let quiet = silences(&rig.heard_from(at + 500), 300);
    assert!(quiet.is_empty(), "{what}: silent at {quiet:?}");
    assert_eq!(rig.player.snapshot().status, PlayerStatus::Playing, "{what}");
    rig.player.shutdown();
}

#[test]
fn a_new_song_while_a_crossfade_tail_is_written_plays() {
    for mode in [Mode::Gapless, Mode::Crossfade, Mode::Smart] {
        change_while_the_tail_is_written(mode, false);
    }
}

#[test]
fn a_seek_while_a_crossfade_tail_is_written_lands() {
    for mode in [Mode::Gapless, Mode::Crossfade, Mode::Smart] {
        change_while_the_tail_is_written(mode, true);
    }
}

/// Once the decoder has finished the queue, the last song is still heard for the ring's length.
/// A seek in it must still land.
#[test]
fn a_seek_after_the_decoder_finished_the_queue_lands() {
    for mode in [Mode::Gapless, Mode::Crossfade, Mode::Smart] {
        let rig = Rig::new(mode);
        rig.player.set_queue(vec![QueueItem::new("s2".into(), Some(20_000))], 0, 4_000, false);
        rig.wait_position(17_000);
        let at = rig.now_ms();
        rig.player.seek_ms(5_000);
        rig.wait_until(at + 1_500);
        let h = rig.heard_from(at + 600);
        assert!(matches!(h[0].1, Heard::Song(2, p) if p.abs_diff(5_600) < 400), "{mode:?}: heard {:?} after the seek", h[0].1);
        rig.player.shutdown();
    }
}

/// Following turns the crossfade off only while it lasts. The own queue fades again afterwards,
/// and Smart still keeps an album gapless.
#[test]
fn the_crossfade_comes_back_after_following() {
    for mode in [Mode::Crossfade, Mode::Smart] {
        let rig = Rig::new(mode);
        rig.player.set_following(true);
        rig.player.set_queue(vec![QueueItem::new("s2".into(), Some(20_000))], 0, 2_000, false);
        rig.wait_position(4_000);
        rig.player.set_following(false);
        // Own queue after the followed song, s4 from another album, then s0.
        rig.player.insert_at(1, vec![QueueItem::new("s4".into(), Some(20_000)), QueueItem::new("s0".into(), Some(20_000))]);
        let start = rig.now_ms();
        rig.wait_until(start + 16_000 + 2 * 20_000 - 2_000);
        let order: Vec<String> = runs(&rig.heard_from(start), 3).into_iter().map(|r| r.1).collect();
        // s2 was decoded while following, so it cuts into s4. s4 into s0 changes album, so it fades.
        assert_eq!(&order[..4], ["s2", "s4", "mix", "s0"], "{mode:?}: {order:?}");
        rig.player.shutdown();
    }
}

/// a host waiting on a listener's session song: the queue ends after the playing song, so its own
/// next song isn't started; Next then plays it. in every mode, since a crossfade holds a tail.
#[test]
fn holding_after_the_current_song_ends_the_queue_there() {
    for mode in [Mode::Gapless, Mode::Crossfade, Mode::Smart] {
        let rig = Rig::new(mode);
        rig.player.set_queue(vec![QueueItem::new("s0".into(), Some(SONG_S as u64 * 1000)), QueueItem::new("s1".into(), Some(SONG_S as u64 * 1000))], 0, 9_000, false);
        rig.wait_position(10_000);
        rig.player.set_hold_after_current(true);
        let limit = Instant::now() + Duration::from_secs(60);
        while rig.player.snapshot().status != PlayerStatus::Stopped {
            assert!(Instant::now() < limit, "{mode:?}: the queue never ended");
            std::thread::sleep(Duration::from_millis(5));
        }
        let start = rig.now_ms();
        assert!(rig.heard_from(0).iter().all(|(_, h)| !matches!(h, Heard::Song(1, _) | Heard::Mix)), "{mode:?}: the next song was heard while held");
        rig.player.set_hold_after_current(false);
        rig.player.next();
        rig.wait_until(start + 1_500);
        assert!(matches!(rig.first_audio_after(start), Some((_, Heard::Song(1, _)))), "{mode:?}: Next didn't move on");
    }
}
