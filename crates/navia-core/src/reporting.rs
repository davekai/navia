//! Tells the server what was played.
//! Navidrome only counts a play on `scrobble` with `submission=true`. Plays are stored locally first, so none are lost.
//! Private mode blocks every report, and a track played while private is never reported.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;

use crate::audio::player::PlayerStatus;
use crate::client::{Client, PlaybackState};
use crate::error::{Error, Result};

const MIN_TRACK: Duration = Duration::from_secs(30);
const MAX_THRESHOLD: Duration = Duration::from_secs(240);

/// How much of a song must be heard before it counts as played. Saved per account.
///
/// A change applies to the playing song too, unless it has already counted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlayThreshold {
    /// Share of the song, one of [`PlayThreshold::PERCENT_CHOICES`].
    pub percent: u8,
    /// Also count once four minutes were heard, when that comes first.
    pub cap_at_four_minutes: bool,
}

impl Default for PlayThreshold {
    fn default() -> Self {
        Self { percent: 60, cap_at_four_minutes: true }
    }
}

impl PlayThreshold {
    pub const PERCENT_CHOICES: [u8; 5] = [25, 50, 60, 75, 90];

    /// A threshold, if `percent` is one of the choices.
    pub fn new(percent: u8, cap_at_four_minutes: bool) -> Option<Self> {
        Self::PERCENT_CHOICES.contains(&percent).then_some(Self { percent, cap_at_four_minutes })
    }

    /// How much of a song this long must be heard, or `None` for songs under 30 s, which never count.
    pub fn needed(self, duration: Duration) -> Option<Duration> {
        if duration < MIN_TRACK {
            return None;
        }
        let share = duration * u32::from(self.percent) / 100;
        Some(if self.cap_at_four_minutes { share.min(MAX_THRESHOLD) } else { share })
    }

    /// As kept in the library, such as "60 1".
    pub fn to_stored(self) -> String {
        format!("{} {}", self.percent, u8::from(self.cap_at_four_minutes))
    }

    pub fn from_stored(text: &str) -> Option<Self> {
        let (percent, cap) = text.trim().split_once(' ')?;
        let cap = match cap {
            "1" => true,
            "0" => false,
            _ => return None,
        };
        Self::new(percent.parse().ok()?, cap)
    }
}
const MAX_QUEUED: usize = 2000;

/// Where reports go. Implemented by [`Client`], and tests use a recording fake.
#[async_trait]
pub trait ReportSink: Send + Sync {
    async fn now_playing(&self, song_id: &str) -> Result<()>;
    async fn submit(&self, song_id: &str, played_at_ms: u64) -> Result<()>;
    /// Sends a live playback state with `reportPlayback`.
    async fn playback(&self, _song_id: &str, _state: PlaybackState, _position_ms: u64) -> Result<()> {
        Ok(())
    }
    /// Marks the playback reports that follow as shared or not.
    fn set_shared(&self, _on: bool) {}
    /// the further listen-along marks: listeners may control this player; it follows someone; it
    /// has a song queued for the listening session.
    fn set_listen_marks(&self, _control: bool, _following: bool, _queued: bool) {}
    /// the client name live states go out under, where the sink has one.
    fn report_name(&self) -> Option<String> {
        None
    }
    /// a live state under an earlier client name (`None`: the current one).
    async fn playback_as(&self, _name: Option<&str>, song_id: &str, state: PlaybackState, position_ms: u64) -> Result<()> {
        self.playback(song_id, state, position_ms).await
    }
}

#[async_trait]
impl ReportSink for Client {
    async fn playback(&self, song_id: &str, state: PlaybackState, position_ms: u64) -> Result<()> {
        self.report_playback(song_id, state, position_ms).await
    }

    async fn now_playing(&self, song_id: &str) -> Result<()> {
        self.scrobble(song_id, false, None).await
    }

    async fn submit(&self, song_id: &str, played_at_ms: u64) -> Result<()> {
        self.scrobble(song_id, true, Some(played_at_ms)).await
    }

    fn set_shared(&self, on: bool) {
        self.set_sharing(on);
    }

    fn set_listen_marks(&self, control: bool, following: bool, queued: bool) {
        Client::set_listen_marks(self, control, following, queued);
    }

    fn report_name(&self) -> Option<String> {
        Some(self.report_client_name())
    }

    async fn playback_as(&self, name: Option<&str>, song_id: &str, state: PlaybackState, position_ms: u64) -> Result<()> {
        match name {
            Some(name) => self.report_playback_as(name, song_id, state, position_ms).await,
            None => self.report_playback(song_id, state, position_ms).await,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TrackPlay {
    pub song_id: String,
    pub duration: Duration,
    /// Wall-clock start of playback, in milliseconds since the Unix epoch.
    pub started_at_ms: u64,
}

struct Current {
    play: TrackPlay,
    /// Audio actually heard, excluding pauses and seeks.
    heard: Duration,
    /// Never report this track.
    suppressed: bool,
    submitted: bool,
}

/// Durable storage for unsent plays. Implemented by [`crate::library::Library`].
pub trait PlayStore: Send + Sync {
    /// Plays already waiting, oldest first, as (seq, song id, played-at ms).
    fn load(&self) -> Vec<(i64, String, u64)>;
    /// Persists a play. Returns its sequence number.
    fn add(&self, song_id: &str, played_at_ms: u64) -> Option<i64>;
    fn remove(&self, seq: i64);
    /// The server accepted this play.
    fn played(&self, song_id: &str, played_at_ms: u64);
    /// This account's saved threshold, if one was saved.
    fn play_threshold(&self) -> Option<PlayThreshold> {
        None
    }
    fn save_play_threshold(&self, _threshold: PlayThreshold) -> Result<()> {
        Ok(())
    }
}

struct Pending {
    seq: Option<i64>,
    song_id: String,
    played_at_ms: u64,
}

struct State {
    private: bool,
    threshold: PlayThreshold,
    current: Option<Current>,
    queue: VecDeque<Pending>,
}

pub struct PlayReporter {
    sink: Arc<dyn ReportSink>,
    store: Option<Arc<dyn PlayStore>>,
    state: Mutex<State>,
    /// Whether the server takes live states through `playbackReport`. Off until told.
    live_enabled: AtomicBool,
    live: Mutex<LiveTracker>,
}

/// What the player did, for live playback reports.
#[derive(Clone, Debug)]
pub enum LiveEvent {
    /// A song became current. `None` for something that isn't a server song, like a station, which isn't reported.
    Track(Option<String>),
    Status(PlayerStatus),
    Position(u64),
    /// The queue ran out.
    Ended,
}

/// seeks show up as a position this far from where playing would have taken it. under half the
/// listen-along resync drift, so a song started mid-way is reported where it really is.
const SEEK_JUMP_MS: u64 = 1000;

/// Turns player events into `reportPlayback` states, one per change and after a seek.
/// The server moves its own clock between reports. A song restored paused isn't reported until it plays.
#[derive(Debug, Default)]
pub struct LiveTracker {
    song: Option<String>,
    status: Option<PlayerStatus>,
    position_ms: u64,
    /// When `position_ms` was seen, for spotting seeks while playing.
    position_at: Option<std::time::Instant>,
    /// The server has been told about this song, with starting sent and no stop since.
    announced: bool,
}

impl LiveTracker {
    pub fn on_event(&mut self, event: LiveEvent, now: std::time::Instant) -> Vec<(String, PlaybackState, u64)> {
        let mut out = Vec::new();
        let playing = self.status == Some(PlayerStatus::Playing);
        match event {
            LiveEvent::Track(id) => {
                if id.is_none() && self.announced {
                    self.emit(&mut out, PlaybackState::Stopped);
                }
                self.song = id;
                self.position_ms = 0;
                self.position_at = Some(now);
                self.announced = false;
                if matches!(self.status, Some(PlayerStatus::Playing | PlayerStatus::Buffering)) {
                    self.announce(&mut out);
                    if playing {
                        self.emit(&mut out, PlaybackState::Playing);
                    }
                }
            }
            LiveEvent::Status(status) => {
                let was = self.status.replace(status);
                if was == Some(status) {
                    return out;
                }
                match status {
                    PlayerStatus::Playing => {
                        if !self.announced {
                            self.announce(&mut out);
                        }
                        self.position_at = Some(now);
                        self.emit(&mut out, PlaybackState::Playing);
                    }
                    PlayerStatus::Buffering => {
                        if !self.announced {
                            self.announce(&mut out);
                        }
                    }
                    PlayerStatus::Paused if self.announced => self.emit(&mut out, PlaybackState::Paused),
                    PlayerStatus::Stopped if self.announced => {
                        self.emit(&mut out, PlaybackState::Stopped);
                        self.announced = false;
                    }
                    _ => {}
                }
            }
            LiveEvent::Position(ms) => {
                let expected = match (playing, self.position_at) {
                    (true, Some(at)) => self.position_ms + now.saturating_duration_since(at).as_millis() as u64,
                    _ => self.position_ms,
                };
                let jumped = ms.abs_diff(expected) > SEEK_JUMP_MS;
                self.position_ms = ms;
                self.position_at = Some(now);
                if jumped && self.announced {
                    match self.status {
                        Some(PlayerStatus::Playing) => self.emit(&mut out, PlaybackState::Playing),
                        Some(PlayerStatus::Paused) => self.emit(&mut out, PlaybackState::Paused),
                        _ => {}
                    }
                }
            }
            LiveEvent::Ended => {
                if self.announced {
                    self.emit(&mut out, PlaybackState::Stopped);
                    self.announced = false;
                }
            }
        }
        out
    }

    /// The current song's state again from `starting`, for a session that begins mid-song under a new client name.
    pub fn restart(&mut self) -> Vec<(String, PlaybackState, u64)> {
        let mut out = Vec::new();
        match self.status {
            Some(PlayerStatus::Playing) => {
                self.announce(&mut out);
                self.emit(&mut out, PlaybackState::Playing);
            }
            Some(PlayerStatus::Paused) => {
                self.announce(&mut out);
                self.emit(&mut out, PlaybackState::Paused);
            }
            Some(PlayerStatus::Buffering) => self.announce(&mut out),
            _ => {}
        }
        out
    }

    /// Ends the server's session for this song, for quitting, private mode or reporting off.
    pub fn stop(&mut self) -> Option<(String, PlaybackState, u64)> {
        if !self.announced {
            return None;
        }
        self.announced = false;
        Some((self.song.clone()?, PlaybackState::Stopped, self.position_ms))
    }

    fn announce(&mut self, out: &mut Vec<(String, PlaybackState, u64)>) {
        if self.song.is_some() {
            self.announced = true;
            self.emit(out, PlaybackState::Starting);
        }
    }

    fn emit(&self, out: &mut Vec<(String, PlaybackState, u64)>, state: PlaybackState) {
        if let Some(id) = &self.song {
            out.push((id.clone(), state, self.position_ms));
        }
    }
}

/// True once enough of the track was heard for it to count as a play.
pub fn play_counts(duration: Duration, heard: Duration, threshold: PlayThreshold) -> bool {
    threshold.needed(duration).is_some_and(|needed| heard >= needed)
}

impl PlayReporter {
    pub fn new(sink: Arc<dyn ReportSink>) -> Self {
        Self { sink, store: None, state: Mutex::new(State { private: false, threshold: PlayThreshold::default(), current: None, queue: VecDeque::new() }), live_enabled: AtomicBool::new(false), live: Mutex::default() }
    }

    /// Turns live playback reports on where the server has `playbackReport`.
    pub fn set_live_reports(&self, on: bool) {
        self.live_enabled.store(on, AtomicOrdering::Release);
    }

    /// Feeds one player event to the live reports and sends what it gives, in order.
    /// Nothing goes out while private or without the extension. A lost report isn't retried.
    pub async fn live_event(&self, event: LiveEvent) {
        let reports = self.live.lock().expect("live reports").on_event(event, std::time::Instant::now());
        if reports.is_empty() || !self.live_enabled.load(AtomicOrdering::Acquire) || self.is_private() {
            return;
        }
        for (id, state, position) in reports {
            let _ = self.sink.playback(&id, state, position).await;
        }
    }

    /// Tells the server this player stopped, if it was told something is playing.
    pub async fn stop_live(&self) {
        let report = self.live.lock().expect("live reports").stop();
        if let Some((id, state, position)) = report
            && self.live_enabled.load(AtomicOrdering::Acquire)
        {
            let _ = self.sink.playback(&id, state, position).await;
        }
    }

    /// "Share what I'm playing". Reports go on either way, under the marked client name while sharing.
    /// The server keeps a session per client name, so the playing song restarts under the new one.
    pub async fn set_sharing(&self, on: bool) {
        self.relabel(|sink| sink.set_shared(on)).await;
    }

    /// the listen-along marks changed ("Let listeners control playback", following someone, a song
    /// queued for the session): like [`Self::set_sharing`], the session moves to the new name.
    pub async fn set_listen_marks(&self, control: bool, following: bool, queued: bool) {
        self.relabel(|sink| sink.set_listen_marks(control, following, queued)).await;
    }

    /// where the sink names its reports, the session starts under the new name before the old one
    /// stops, so whoever follows this player never sees it gone in between.
    async fn relabel(&self, change: impl FnOnce(&dyn ReportSink)) {
        let old = self.sink.report_name();
        let live = self.live_enabled.load(AtomicOrdering::Acquire) && !self.is_private();
        let ended = if live { self.live.lock().expect("live reports").stop() } else { None };
        // a sink without names can only stop the old session before it changes
        if old.is_none()
            && let Some((id, state, position)) = &ended
        {
            let _ = self.sink.playback(id, *state, *position).await;
        }
        change(&*self.sink);
        let Some((id, state, position)) = ended else { return };
        let renamed = old.is_none() || old != self.sink.report_name();
        let again = self.live.lock().expect("live reports").restart();
        if !renamed {
            return;
        }
        for (id, state, position) in again {
            let _ = self.sink.playback(&id, state, position).await;
        }
        if old.is_some() {
            let _ = self.sink.playback_as(old.as_deref(), &id, state, position).await;
        }
    }

    /// A reporter that keeps unsent plays in `store` across restarts. Call [`Self::flush`] to retry them.
    pub fn with_store(sink: Arc<dyn ReportSink>, store: Arc<dyn PlayStore>) -> Self {
        let queue = store.load().into_iter().map(|(seq, song_id, played_at_ms)| Pending { seq: Some(seq), song_id, played_at_ms }).collect();
        let threshold = store.play_threshold().unwrap_or_default();
        Self { sink, store: Some(store), state: Mutex::new(State { private: false, threshold, current: None, queue }), live_enabled: AtomicBool::new(false), live: Mutex::default() }
    }

    /// When a play counts, for this account.
    pub fn play_threshold(&self) -> PlayThreshold {
        self.state.lock().expect("reporter state").threshold
    }

    /// Changes when a play counts and saves it with the account.
    /// Applies to the playing song too if it hasn't counted yet.
    pub fn set_play_threshold(&self, threshold: PlayThreshold) -> Result<()> {
        self.state.lock().expect("reporter state").threshold = threshold;
        match &self.store {
            Some(store) => store.save_play_threshold(threshold),
            None => Ok(()),
        }
    }

    pub fn is_private(&self) -> bool {
        self.state.lock().expect("reporter state").private
    }

    /// Plays waiting to be submitted after a network failure or private mode.
    pub fn queued(&self) -> usize {
        self.state.lock().expect("reporter state").queue.len()
    }

    /// Turns private mode on or off. Turning it off flushes queued plays.
    pub async fn set_private(&self, private: bool) {
        let leaving_private = {
            let mut st = self.state.lock().expect("reporter state");
            let was = st.private;
            st.private = private;
            if private && let Some(cur) = st.current.as_mut() {
                cur.suppressed = true;
            }
            (was && !private, private && !was)
        };
        let (leaving_private, entering_private) = leaving_private;
        // The server's now-playing entry ends as the player goes quiet.
        if entering_private {
            self.stop_live().await;
        }
        if leaving_private {
            let _ = self.flush().await;
        }
    }

    /// A new track began. Sends a now-playing report unless private.
    pub async fn track_started(&self, play: TrackPlay) {
        let (notify, song_id) = {
            let mut st = self.state.lock().expect("reporter state");
            let suppressed = st.private;
            let id = play.song_id.clone();
            st.current = Some(Current { play, heard: Duration::ZERO, suppressed, submitted: false });
            (!suppressed, id)
        };
        if notify {
            // A lost now-playing report isn't worth retrying.
            let _ = self.sink.now_playing(&song_id).await;
        }
    }

    /// Adds audio heard since the last call, and queues the play once it crosses the threshold.
    pub async fn advance(&self, heard_since_last: Duration) {
        let submit = {
            let mut st = self.state.lock().expect("reporter state");
            let threshold = st.threshold;
            let Some(cur) = st.current.as_mut() else { return };
            cur.heard += heard_since_last;
            if !cur.suppressed && !cur.submitted && play_counts(cur.play.duration, cur.heard, threshold) {
                cur.submitted = true;
                let song_id = cur.play.song_id.clone();
                let played_at_ms = cur.play.started_at_ms;
                let seq = self.store.as_ref().and_then(|s| s.add(&song_id, played_at_ms));
                push_bounded(&mut st.queue, Pending { seq, song_id, played_at_ms });
                !st.private
            } else {
                false
            }
        };
        if submit {
            let _ = self.flush().await;
        }
    }

    /// The current track ended or was replaced.
    pub fn track_ended(&self) {
        self.state.lock().expect("reporter state").current = None;
    }

    /// Tries to send queued plays, stopping at the first failure so order is kept.
    /// Does nothing in private mode.
    pub async fn flush(&self) -> Result<()> {
        loop {
            let next = {
                let st = self.state.lock().expect("reporter state");
                if st.private {
                    return Ok(());
                }
                st.queue.front().map(|p| (p.seq, p.song_id.clone(), p.played_at_ms))
            };
            let Some((seq, id, at)) = next else { return Ok(()) };
            match self.sink.submit(&id, at).await {
                Ok(()) => {
                    self.forget(seq);
                    if let Some(store) = &self.store {
                        store.played(&id, at);
                    }
                }
                // The server refused it, so it will never succeed. Drop it.
                Err(Error::Subsonic { .. }) => self.forget(seq),
                Err(e) => return Err(e),
            }
        }
    }

    fn forget(&self, seq: Option<i64>) {
        self.state.lock().expect("reporter state").queue.pop_front();
        if let (Some(store), Some(seq)) = (&self.store, seq) {
            store.remove(seq);
        }
    }
}

fn push_bounded(queue: &mut VecDeque<Pending>, item: Pending) {
    if queue.len() >= MAX_QUEUED {
        queue.pop_front();
    }
    queue.push_back(item);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Fake {
        now_playing: Mutex<Vec<String>>,
        submitted: Mutex<Vec<(String, u64)>>,
        fail: Mutex<bool>,
    }

    #[async_trait]
    impl ReportSink for Fake {
        async fn now_playing(&self, id: &str) -> Result<()> {
            self.now_playing.lock().unwrap().push(id.to_string());
            Ok(())
        }
        async fn submit(&self, id: &str, at: u64) -> Result<()> {
            if *self.fail.lock().unwrap() {
                return Err(Error::Network("offline".into()));
            }
            self.submitted.lock().unwrap().push((id.to_string(), at));
            Ok(())
        }
    }

    /// Records live reports with the client name they went out under.
    #[derive(Default)]
    struct LiveFake {
        shared: std::sync::atomic::AtomicBool,
        sent: Mutex<Vec<(bool, PlaybackState)>>,
    }

    #[async_trait]
    impl ReportSink for LiveFake {
        async fn now_playing(&self, _: &str) -> Result<()> {
            Ok(())
        }
        async fn submit(&self, _: &str, _: u64) -> Result<()> {
            Ok(())
        }
        async fn playback(&self, _: &str, state: PlaybackState, _: u64) -> Result<()> {
            self.sent.lock().unwrap().push((self.shared.load(AtomicOrdering::Acquire), state));
            Ok(())
        }
        fn set_shared(&self, on: bool) {
            self.shared.store(on, AtomicOrdering::Release);
        }
    }

    /// like [`LiveFake`], with names: the stop of the old session goes out under its old name.
    #[derive(Default)]
    struct NamedFake(LiveFake);

    #[async_trait]
    impl ReportSink for NamedFake {
        async fn now_playing(&self, _: &str) -> Result<()> {
            Ok(())
        }
        async fn submit(&self, _: &str, _: u64) -> Result<()> {
            Ok(())
        }
        async fn playback(&self, id: &str, state: PlaybackState, at: u64) -> Result<()> {
            self.0.playback(id, state, at).await
        }
        fn set_shared(&self, on: bool) {
            self.0.set_shared(on);
        }
        fn report_name(&self) -> Option<String> {
            Some(self.0.shared.load(AtomicOrdering::Acquire).to_string())
        }
        async fn playback_as(&self, name: Option<&str>, _: &str, state: PlaybackState, _: u64) -> Result<()> {
            let shared = name.map_or(self.0.shared.load(AtomicOrdering::Acquire), |n| n == "true");
            self.0.sent.lock().unwrap().push((shared, state));
            Ok(())
        }
    }

    /// the new session starts before the old one stops, so a follower never sees the player gone.
    #[tokio::test]
    async fn a_renamed_session_starts_before_the_old_one_stops() {
        let sink = Arc::new(NamedFake::default());
        let r = PlayReporter::new(sink.clone());
        r.set_live_reports(true);
        r.live_event(LiveEvent::Track(Some("s".into()))).await;
        r.live_event(LiveEvent::Status(PlayerStatus::Playing)).await;
        r.set_sharing(true).await;
        // the same name again: nothing to send
        r.set_sharing(true).await;
        use PlaybackState::*;
        assert_eq!(*sink.0.sent.lock().unwrap(), vec![(false, Starting), (false, Playing), (true, Starting), (true, Playing), (false, Stopped)]);
    }

    #[tokio::test]
    async fn sharing_moves_the_session_to_the_marked_name_and_never_stops_reporting() {
        let sink = Arc::new(LiveFake::default());
        let r = PlayReporter::new(sink.clone());
        r.set_live_reports(true);
        r.live_event(LiveEvent::Track(Some("s".into()))).await;
        r.live_event(LiveEvent::Status(PlayerStatus::Playing)).await;
        r.set_sharing(true).await;
        r.live_event(LiveEvent::Status(PlayerStatus::Paused)).await;
        r.set_sharing(false).await;
        use PlaybackState::*;
        assert_eq!(
            *sink.sent.lock().unwrap(),
            vec![(false, Starting), (false, Playing), (false, Stopped), (true, Starting), (true, Playing), (true, Paused), (true, Stopped), (false, Starting), (false, Paused)],
        );
        // In private mode the mark still follows the setting, but nothing is sent.
        sink.sent.lock().unwrap().clear();
        r.set_private(true).await;
        sink.sent.lock().unwrap().clear();
        r.set_sharing(true).await;
        assert!(sink.sent.lock().unwrap().is_empty());
        assert!(sink.shared.load(AtomicOrdering::Acquire));
    }

    fn play(id: &str, secs: u64) -> TrackPlay {
        TrackPlay { song_id: id.into(), duration: Duration::from_secs(secs), started_at_ms: 1_700_000_000_000 }
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn default_is_sixty_percent_or_four_minutes() {
        let t = PlayThreshold::default();
        assert_eq!(t, PlayThreshold { percent: 60, cap_at_four_minutes: true });
        assert!(!play_counts(secs(20), secs(20), t), "too short");
        assert!(!play_counts(secs(29), secs(29), t), "just under 30 s");
        assert!(play_counts(secs(30), secs(18), t), "30 s counts");
        assert!(!play_counts(secs(200), secs(119), t));
        assert!(play_counts(secs(200), secs(120), t));
        assert!(!play_counts(secs(3600), secs(239), t));
        assert!(play_counts(secs(3600), secs(240), t), "four minutes on a long track");
    }

    #[test]
    fn each_share_counts_at_its_own_point() {
        for (percent, needed) in [(25, 75), (50, 150), (60, 180), (75, 225), (90, 270)] {
            let t = PlayThreshold::new(percent, false).unwrap();
            assert!(!play_counts(secs(300), secs(needed - 1), t), "{percent}% one second early");
            assert!(play_counts(secs(300), secs(needed), t), "{percent}% at {needed} s");
            assert_eq!(t.needed(secs(10)), None, "{percent}%: under 30 s never counts");
        }
        assert_eq!(PlayThreshold::new(40, true), None, "not a choice");
        assert_eq!(PlayThreshold::new(0, true), None);
    }

    #[test]
    fn four_minute_cap_only_when_on() {
        let on = PlayThreshold::new(90, true).unwrap();
        let off = PlayThreshold::new(90, false).unwrap();
        // For a 10 min song, 90% is 9 min.
        assert!(play_counts(secs(600), secs(240), on));
        assert!(!play_counts(secs(600), secs(240), off));
        assert!(play_counts(secs(600), secs(540), off));
        // A short song reaches its share before four minutes either way.
        assert_eq!(on.needed(secs(100)), Some(secs(90)));
        assert_eq!(off.needed(secs(100)), Some(secs(90)));
        // 25% of 20 minutes is 5 min, so the cap still wins.
        assert_eq!(PlayThreshold::new(25, true).unwrap().needed(secs(1200)), Some(secs(240)));
    }

    #[test]
    fn stored_form_round_trips_and_refuses_junk() {
        for percent in PlayThreshold::PERCENT_CHOICES {
            for cap in [true, false] {
                let t = PlayThreshold::new(percent, cap).unwrap();
                assert_eq!(PlayThreshold::from_stored(&t.to_stored()), Some(t));
            }
        }
        for junk in ["", "60", "61 1", "60 2", "sixty 1", "60 1 1"] {
            assert_eq!(PlayThreshold::from_stored(junk), None, "{junk:?}");
        }
    }

    #[tokio::test]
    async fn a_change_applies_to_the_song_playing_until_it_counts() {
        let fake = Arc::new(Fake::default());
        let r = PlayReporter::new(fake.clone());
        r.track_started(play("a", 300)).await;
        r.advance(secs(100)).await;
        assert!(fake.submitted.lock().unwrap().is_empty(), "100 of 300 s is under 60%");
        // Lowering it below what was already heard counts at the next bit of audio.
        r.set_play_threshold(PlayThreshold::new(25, true).unwrap()).unwrap();
        r.advance(Duration::from_millis(10)).await;
        assert_eq!(fake.submitted.lock().unwrap().len(), 1);
        // Raising it after it counted doesn't take the play back or send it twice.
        r.set_play_threshold(PlayThreshold::new(90, false).unwrap()).unwrap();
        r.advance(secs(200)).await;
        assert_eq!(fake.submitted.lock().unwrap().len(), 1);
        r.track_ended();

        // Raised before the next song crosses, so that song waits for the new point.
        r.track_started(play("b", 300)).await;
        r.advance(secs(240)).await;
        assert_eq!(fake.submitted.lock().unwrap().len(), 1, "90% with no cap needs 270 s");
        r.advance(secs(30)).await;
        assert_eq!(fake.submitted.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn submits_once_at_threshold_with_start_time() {
        let fake = Arc::new(Fake::default());
        let r = PlayReporter::new(fake.clone());
        r.track_started(play("a", 200)).await;
        assert_eq!(*fake.now_playing.lock().unwrap(), ["a"]);
        r.advance(Duration::from_secs(60)).await;
        r.advance(Duration::from_secs(59)).await;
        assert!(fake.submitted.lock().unwrap().is_empty(), "119 s of 200 is under 60%");
        r.advance(Duration::from_secs(1)).await;
        r.advance(Duration::from_secs(60)).await;
        assert_eq!(*fake.submitted.lock().unwrap(), [("a".to_string(), 1_700_000_000_000)]);
    }

    #[tokio::test]
    async fn private_mode_reports_nothing() {
        let fake = Arc::new(Fake::default());
        let r = PlayReporter::new(fake.clone());
        r.set_private(true).await;
        r.track_started(play("a", 200)).await;
        r.advance(Duration::from_secs(200)).await;
        r.track_ended();
        assert!(fake.now_playing.lock().unwrap().is_empty());
        assert!(fake.submitted.lock().unwrap().is_empty());
        assert_eq!(r.queued(), 0);
    }

    #[tokio::test]
    async fn turning_private_on_mid_track_cancels_it_and_off_does_not_revive_it() {
        let fake = Arc::new(Fake::default());
        let r = PlayReporter::new(fake.clone());
        r.track_started(play("a", 200)).await;
        r.advance(Duration::from_secs(10)).await;
        r.set_private(true).await;
        r.advance(Duration::from_secs(150)).await;
        r.set_private(false).await;
        r.advance(Duration::from_secs(50)).await;
        assert!(fake.submitted.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn track_started_while_private_stays_private_after_mode_ends() {
        let fake = Arc::new(Fake::default());
        let r = PlayReporter::new(fake.clone());
        r.set_private(true).await;
        r.track_started(play("a", 200)).await;
        r.set_private(false).await;
        r.advance(Duration::from_secs(200)).await;
        assert!(fake.submitted.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn queued_plays_wait_through_private_mode_and_failures_keep_order() {
        let fake = Arc::new(Fake::default());
        let r = PlayReporter::new(fake.clone());
        *fake.fail.lock().unwrap() = true;
        r.track_started(play("a", 200)).await;
        r.advance(Duration::from_secs(150)).await;
        r.track_ended();
        r.track_started(play("b", 200)).await;
        r.advance(Duration::from_secs(150)).await;
        r.track_ended();
        assert_eq!(r.queued(), 2, "network failure keeps plays queued");
        r.set_private(true).await;
        *fake.fail.lock().unwrap() = false;
        r.flush().await.unwrap();
        assert!(fake.submitted.lock().unwrap().is_empty(), "no flush in private mode");
        r.set_private(false).await;
        let submitted = fake.submitted.lock().unwrap().clone();
        assert_eq!(submitted.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(r.queued(), 0);
    }

    #[derive(Default)]
    struct MemStore {
        rows: Mutex<Vec<(i64, String, u64)>>,
        played: Mutex<Vec<String>>,
        next: Mutex<i64>,
    }

    impl PlayStore for MemStore {
        fn load(&self) -> Vec<(i64, String, u64)> {
            self.rows.lock().unwrap().clone()
        }
        fn add(&self, song_id: &str, at: u64) -> Option<i64> {
            let mut n = self.next.lock().unwrap();
            *n += 1;
            self.rows.lock().unwrap().push((*n, song_id.to_string(), at));
            Some(*n)
        }
        fn remove(&self, seq: i64) {
            self.rows.lock().unwrap().retain(|(s, _, _)| *s != seq);
        }
        fn played(&self, song_id: &str, _: u64) {
            self.played.lock().unwrap().push(song_id.to_string());
        }
    }

    #[tokio::test]
    async fn unsent_plays_survive_a_restart_and_are_counted_locally() {
        let fake = Arc::new(Fake::default());
        *fake.fail.lock().unwrap() = true;
        let store = Arc::new(MemStore::default());
        {
            let r = PlayReporter::with_store(fake.clone(), store.clone());
            r.track_started(play("a", 200)).await;
            r.advance(Duration::from_secs(150)).await;
            r.track_ended();
            assert_eq!(r.queued(), 1);
        }
        assert_eq!(store.rows.lock().unwrap().len(), 1, "persisted before sending");

        // A new reporter after a crash picks it up and delivers it.
        *fake.fail.lock().unwrap() = false;
        let r = PlayReporter::with_store(fake.clone(), store.clone());
        assert_eq!(r.queued(), 1);
        r.flush().await.unwrap();
        assert_eq!(fake.submitted.lock().unwrap().iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["a"]);
        assert!(store.rows.lock().unwrap().is_empty());
        assert_eq!(*store.played.lock().unwrap(), ["a"]);
    }

    #[tokio::test]
    async fn server_rejection_drops_the_play_without_retrying() {
        struct Reject;
        #[async_trait]
        impl ReportSink for Reject {
            async fn now_playing(&self, _: &str) -> Result<()> { Ok(()) }
            async fn submit(&self, _: &str, _: u64) -> Result<()> {
                Err(Error::Subsonic { code: 70, endpoint: "scrobble".into(), message: "not found".into() })
            }
        }
        let r = PlayReporter::new(Arc::new(Reject));
        r.track_started(play("a", 200)).await;
        r.advance(Duration::from_secs(200)).await;
        assert_eq!(r.queued(), 0);
    }
}
