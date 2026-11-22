//! Opt-in listen-along between users of the same server.
//! Sharing Navias mark their client name with [`SHARE_MARK`], and following needs the `playbackReport` extension.
//! Everything here is pure, and the apps call [`fetch`] on their own timers.

use std::time::Duration;

use serde_json::Value;

use crate::client::{Client, Song};
use crate::error::Result;

/// Ends the client name of a sharing Navia. A zero-width space, so it's invisible where the name is shown.
pub const SHARE_MARK: char = '\u{200B}';
/// Follows the share mark when this Navia lets listeners control playback. A zero-width non-joiner.
pub const CONTROL_MARK: char = '\u{200C}';
/// Follows the share mark when this Navia is listening along with someone. A zero-width joiner.
pub const FOLLOW_MARK: char = '\u{200D}';
/// after the share mark: this Navia has a song queued for the listening session (a word joiner).
pub const QUEUE_MARK: char = '\u{2060}';
/// every mark a sharing Navia may append to its client name.
pub const MARKS: [char; 4] = [SHARE_MARK, CONTROL_MARK, FOLLOW_MARK, QUEUE_MARK];

/// How often Listening Now is checked, and how often while following.
pub const LIST_POLL: Duration = Duration::from_secs(15);
pub const FOLLOW_POLL: Duration = Duration::from_secs(5);
/// Following seeks only when the two players are further apart than this.
pub const RESYNC_DRIFT: Duration = Duration::from_secs(2);
/// How long the last good Listening Now is kept while checks fail, about two missed checks.
pub const LISTING_KEEP: Duration = Duration::from_secs(45);
/// How long "<user> stopped listening" shows after following ends by itself.
pub const ENDED_NOTICE: Duration = Duration::from_secs(6);
/// how long a listener's own change waits for the host to take it before giving up.
pub const SEND_WAIT: Duration = Duration::from_secs(15);
/// how long two people seen following each other stay so before the one who leads stops following.
pub const TWO_WAY_SETTLE: Duration = Duration::from_secs(8);
/// the end of a song, while a listening session is on: checks run every [`END_POLL`] so the next
/// session song is handed over in about a second.
pub const END_WINDOW: Duration = Duration::from_secs(10);
pub const END_POLL: Duration = Duration::from_secs(1);
/// how long the side whose turn it isn't waits at a song's end for the other side's session song.
pub const SESSION_WAIT: Duration = Duration::from_secs(5);

fn ms(d: Duration) -> u64 {
    d.as_millis() as u64
}

/// A session's state, where the server has `playbackReport`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    Starting,
    Playing,
    Paused,
}

impl SessionState {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "starting" => Some(Self::Starting),
            "playing" => Some(Self::Playing),
            "paused" => Some(Self::Paused),
            _ => None,
        }
    }

    /// Starting counts as playing.
    pub fn is_playing(self) -> bool {
        self != Self::Paused
    }
}

/// One `getNowPlaying` entry. `state` and `position_ms` are `None` without `playbackReport`.
#[derive(Clone, Debug, PartialEq)]
pub struct NowPlaying {
    pub user: String,
    pub player: String,
    pub song: Song,
    pub state: Option<SessionState>,
    pub position_ms: Option<u64>,
    pub minutes_ago: u32,
}

impl NowPlaying {
    /// Parses one entry, skipping entries without a song or user.
    /// `live` says whether the server has `playbackReport`. Without it, state and position are ignored.
    pub fn from_entry(entry: &Value, live: bool) -> Option<Self> {
        let song: Song = serde_json::from_value(entry.clone()).ok().filter(|s: &Song| !s.id.is_empty())?;
        let user = entry.get("username").and_then(Value::as_str)?.to_string();
        let player = entry.get("playerName").and_then(Value::as_str).unwrap_or_default().to_string();
        let state = if live { entry.get("state").and_then(Value::as_str).and_then(SessionState::parse) } else { None };
        let position_ms = if live && state.is_some() { entry.get("positionMs").and_then(Value::as_u64) } else { None };
        let minutes_ago = entry.get("minutesAgo").and_then(Value::as_u64).unwrap_or(0) as u32;
        Some(Self { user, player, song, state, position_ms, minutes_ago })
    }

    /// Shared from a Navia with "Share what I'm playing" on.
    pub fn shared(&self) -> bool {
        self.player.contains(SHARE_MARK)
    }

    /// Shared from a Navia with "Let listeners control playback" on.
    pub fn allows_control(&self) -> bool {
        self.shared() && self.player.contains(CONTROL_MARK)
    }

    /// Shared from a Navia that is listening along with someone.
    pub fn follows(&self) -> bool {
        self.shared() && self.player.contains(FOLLOW_MARK)
    }

    /// shared from a Navia with a song queued for the listening session.
    pub fn has_session_queue(&self) -> bool {
        self.shared() && self.player.contains(QUEUE_MARK)
    }

    pub fn playing(&self) -> bool {
        self.state.is_some_and(SessionState::is_playing)
    }

    /// Whether this person can be followed: the server says what plays and where.
    pub fn can_listen_along(&self) -> bool {
        self.state.is_some() && self.position_ms.is_some()
    }
}

/// Fetches and parses `getNowPlaying`. `live` says whether the server has `playbackReport`.
pub async fn fetch(client: &Client, live: bool) -> Result<Vec<NowPlaying>> {
    let asked = std::time::Instant::now();
    let mut entries: Vec<NowPlaying> = client.now_playing().await?.iter().filter_map(|e| NowPlaying::from_entry(e, live)).collect();
    advance(&mut entries, asked.elapsed().as_millis() as u64 / 2);
    Ok(entries)
}

/// moves playing positions on by `by_ms`, as play would have, never past the song's end. the
/// server only moves `playing` sessions itself; `starting` stays where it was reported.
pub fn advance(entries: &mut [NowPlaying], by_ms: u64) {
    for e in entries.iter_mut().filter(|e| e.state == Some(SessionState::Playing)) {
        if let Some(at) = e.position_ms.as_mut() {
            let end = e.song.duration.filter(|d| *d > 0).map_or(u64::MAX, |d| d as u64 * 1000);
            *at = (*at + by_ms).min(end);
        }
    }
}

/// Reads `getNowPlaying` entries from a JSON array, so tests can check following without real people.
pub fn from_json(bytes: &[u8]) -> Vec<NowPlaying> {
    let entries: Vec<Value> = serde_json::from_slice(bytes).unwrap_or_default();
    entries.iter().filter_map(|e| NowPlaying::from_entry(e, true)).collect()
}

fn same_user(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// the same player, whatever marks it carries now: marks change while it plays.
fn same_player(a: &str, b: &str) -> bool {
    a.trim_end_matches(MARKS) == b.trim_end_matches(MARKS)
}

/// Listening Now for `viewer`: everyone else who shares, one row per person in the server's
/// order (newest first). Someone on two players shows the one that is playing, else the newest.
/// Empty unless `active` (the viewer's own sharing on and Private mode off).
pub fn people(entries: &[NowPlaying], viewer: &str, active: bool) -> Vec<NowPlaying> {
    if !active {
        return Vec::new();
    }
    let mut out: Vec<NowPlaying> = Vec::new();
    for e in entries.iter().filter(|e| e.shared() && !same_user(&e.user, viewer)) {
        match out.iter().position(|o| same_user(&o.user, &e.user)) {
            None => out.push(e.clone()),
            Some(at) => {
                let have_playing = out[at].state.is_some_and(SessionState::is_playing);
                if !have_playing && e.state.is_some_and(SessionState::is_playing) {
                    out[at] = e.clone();
                }
            }
        }
    }
    out
}

/// Who is being followed, and the player last followed since they may have two.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Followed {
    pub user: String,
    pub player: String,
}

/// the followed player as the previous check saw it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Seen {
    pub song_id: String,
    pub position_ms: u64,
    pub playing: bool,
    /// when it was read (Unix ms).
    pub at_ms: u64,
}

impl Seen {
    /// the followed session in `entries`, as the next check will compare against.
    pub fn of(followed: &Followed, entries: &[NowPlaying], now_ms: u64) -> Option<Self> {
        let e = followed_entry(followed, entries)?;
        Some(Self { song_id: e.song.id.clone(), position_ms: e.position_ms.unwrap_or(0), playing: e.playing(), at_ms: now_ms })
    }
}

/// What the listener's own player is doing while following.
///
/// Play and pause are shared both ways, and whoever changed last wins. Song changes, skips and
/// seeks stay with the host.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Local {
    pub song_id: String,
    pub position_ms: u64,
    pub playing: bool,
    /// The listener paused it themselves. It stays paused until they play again or the host changes state after them.
    pub held: bool,
    /// When the listener last pressed play or pause while following, in Unix ms.
    pub own_change_ms: Option<u64>,
    /// the listener chose this song or place: the host comes to it, not the other way round,
    /// until the host changes something itself.
    pub lead: bool,
    /// the followed player at the previous check.
    pub seen: Option<Seen>,
    /// the followed song finished here (the player stopped at its end).
    pub ended: bool,
    /// the listener's user name, for spotting two people following each other; empty skips it.
    pub viewer: String,
    /// since when the followed person has been seen following the listener back (Unix ms).
    pub two_way_since: Option<u64>,
    pub now_ms: u64,
}

/// What one check while following asks of the listener's player.
#[derive(Clone, Debug, PartialEq)]
pub enum FollowStep {
    /// They stopped, went quiet or can't be followed anymore.
    /// Keep the song, pause and end following with "<user> stopped listening".
    Ended,
    /// Play `entry`'s song from its position, matching its play state.
    /// Sent for a song change, a pause or resume, or drift past [`RESYNC_DRIFT`].
    Resync { entry: NowPlaying, player: String },
    /// In step. `drift_ms` is how far ahead the listener is, negative when behind.
    Keep { drift_ms: i64, player: String },
    /// the listener's own change is on its way to the host: keep playing it ("Sending to <user>…").
    Sending { player: String },
    /// the host didn't take the listener's change within [`SEND_WAIT`]: say so, then play `entry`.
    NotTaken { entry: NowPlaying, player: String },
    /// the followed person follows the listener back, and the listener leads: stop following and
    /// keep playing; they stay listening along.
    Lead { user: String },
    /// A followed Spotify friend plays a song that isn't in the library ([`crate::lanyard`]):
    /// pause, say it was skipped (once per `key`), and keep following.
    NotInLibrary { key: String, song: String, artist: String },
}

/// The followed person's shared session in a `getNowPlaying` answer.
/// Prefers the same player, then one that's playing, then the newest.
pub fn followed_entry<'a>(followed: &Followed, entries: &'a [NowPlaying]) -> Option<&'a NowPlaying> {
    let theirs: Vec<&NowPlaying> = entries.iter().filter(|e| e.shared() && same_user(&e.user, &followed.user)).collect();
    theirs
        .iter()
        .find(|e| e.player == followed.player)
        .or_else(|| theirs.iter().find(|e| same_player(&e.player, &followed.player)))
        .or_else(|| theirs.iter().find(|e| e.playing()))
        .or_else(|| theirs.first())
        .copied()
}

/// whether the followed person follows someone, and nobody they could be following but the
/// viewer plays their song: two people following each other.
pub fn two_way(followed: &Followed, entries: &[NowPlaying], viewer: &str) -> bool {
    let Some(them) = followed_entry(followed, entries) else { return false };
    them.follows()
        && !entries.iter().any(|o| o.shared() && !o.follows() && !same_user(&o.user, &them.user) && !same_user(&o.user, viewer) && o.song.id == them.song.id)
}

/// of two people following each other, the one who stops following and stays the host: the
/// first user name in case-insensitive order, so both sides agree.
pub fn leads(viewer: &str, other: &str) -> bool {
    viewer.to_lowercase() < other.to_lowercase()
}

/// One check while following `followed`, from a fresh `getNowPlaying` answer.
///
/// When play states differ, the newer of the listener's change and the host's change wins.
pub fn follow_step(followed: &Followed, entries: &[NowPlaying], local: &Local) -> FollowStep {
    let Some(entry) = followed_entry(followed, entries).filter(|e| e.can_listen_along()) else { return FollowStep::Ended };
    let player = entry.player.clone();
    let keep = |drift_ms| FollowStep::Keep { drift_ms, player: player.clone() };
    let resync = || FollowStep::Resync { entry: entry.clone(), player: player.clone() };
    let at = entry.position_ms.unwrap_or(0);
    let playing = entry.playing();
    let drift_ms = local.position_ms as i64 - at as i64;
    // a starting session's position stays where it was reported until it plays
    let far = entry.state != Some(SessionState::Starting) && drift_ms.unsigned_abs() > ms(RESYNC_DRIFT);
    let same_song = entry.song.id == local.song_id;
    let seen = local.seen.as_ref();
    // what the host did since the previous check.
    let state_moved = seen.is_some_and(|s| s.playing != playing);
    let song_moved = seen.is_some_and(|s| s.song_id != entry.song.id);
    let jumped = seen.is_some_and(|s| {
        let elapsed = if s.playing || playing { local.now_ms.saturating_sub(s.at_ms) } else { 0 };
        s.song_id == entry.song.id && (at + ms(RESYNC_DRIFT) < s.position_ms || at > s.position_ms + elapsed + ms(RESYNC_DRIFT))
    });
    if !local.viewer.is_empty() && two_way(followed, entries, &local.viewer) && leads(&local.viewer, &entry.user) {
        if local.two_way_since.is_some_and(|t| local.now_ms.saturating_sub(t) >= ms(TWO_WAY_SETTLE)) {
            return FollowStep::Lead { user: entry.user.clone() };
        }
        // they follow this side too: a song they haven't changed since the last look is one they
        // are about to leave for this side's, so it isn't taken
        if local.own_change_ms.is_none() && !song_moved {
            return keep(0);
        }
    }
    if let Some(changed) = local.own_change_ms {
        if same_song && playing == local.playing && (!far || local.lead) {
            return keep(drift_ms);
        }
        let looked_since = seen.is_some_and(|s| s.at_ms > changed);
        if looked_since && ((song_moved && !same_song) || (state_moved && playing != local.playing) || jumped) {
            return resync();
        }
        if local.now_ms.saturating_sub(changed) > ms(SEND_WAIT) {
            return FollowStep::NotTaken { entry: entry.clone(), player };
        }
        return FollowStep::Sending { player };
    }
    if !same_song {
        return resync();
    }
    if local.ended {
        return keep(0);
    }
    if playing != local.playing {
        if local.held && !state_moved {
            return keep(0);
        }
        return resync();
    }
    if far && !(local.lead && !jumped) {
        return resync();
    }
    keep(drift_ms)
}

/// Someone listening along with this Navia, as the host sees them in `getNowPlaying`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listener {
    pub user: String,
    pub player: String,
    pub song_id: String,
    pub position_ms: u64,
    pub playing: bool,
    /// On the host's song at this look.
    pub in_step: bool,
    /// When this was read, in Unix ms.
    pub seen_ms: u64,
    /// they have a song queued for the listening session.
    pub queued: bool,
    /// their song is still starting: the server holds its position where it was reported.
    pub starting: bool,
}

/// the listener whose song or place the host took, while the host plays it: the host comes to
/// them when the two drift apart, until the host's user changes something.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Led {
    pub user: String,
    pub player: String,
    pub song_id: String,
    /// when the host took it (Unix ms).
    pub at_ms: u64,
}

/// The host's own player, for [`listeners`] and [`host_mirror`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Host {
    pub song_id: String,
    pub position_ms: u64,
    pub playing: bool,
    /// When the host's user last pressed play or pause, seeked or played something, in Unix ms.
    /// Mirroring a listener doesn't count.
    pub own_change_ms: u64,
    pub led: Option<Led>,
}

/// what the host does to mirror a listener. a pause or resume carries the listener's place when
/// the two are further apart than [`RESYNC_DRIFT`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostAction {
    Pause(Option<u64>),
    Play(Option<u64>),
    Seek(u64),
    /// Play this song from this position, then the host's queue carries on.
    Song { song_id: String, position_ms: u64, playing: bool },
}

/// a host action and the listener it comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mirror {
    pub action: HostAction,
    pub user: String,
    pub player: String,
}

impl Mirror {
    /// what the host keeps after carrying it out: a song or place it took leads until the host's
    /// user changes something; a pause or resume leaves the lead as it was.
    pub fn led(&self, host: &Host, now_ms: u64) -> Option<Led> {
        let led = |song_id: &str| Some(Led { user: self.user.clone(), player: self.player.clone(), song_id: song_id.to_string(), at_ms: now_ms });
        match &self.action {
            HostAction::Song { song_id, .. } => led(song_id),
            HostAction::Seek(_) => led(&host.song_id),
            _ => host.led.clone(),
        }
    }
}

impl Listener {
    fn same(&self, other: &Listener) -> bool {
        same_player(&self.player, &other.player) && same_user(&self.user, &other.user)
    }
}

/// Who listens along with `viewer` now.
/// That's other people's shared sessions marked as following and on the host's song, now or at the last look.
pub fn listeners(entries: &[NowPlaying], viewer: &str, host: &Host, prev: &[Listener], now_ms: u64) -> Vec<Listener> {
    let slack = ms(RESYNC_DRIFT + FOLLOW_POLL);
    entries
        .iter()
        .filter(|e| e.follows() && !same_user(&e.user, viewer) && e.can_listen_along())
        .filter_map(|e| {
            let playing = e.playing();
            let position_ms = e.position_ms.unwrap_or(0);
            let on_song = e.song.id == host.song_id;
            let in_step = on_song && playing == host.playing && position_ms.abs_diff(host.position_ms) <= slack;
            let known = prev.iter().any(|p| same_player(&p.player, &e.player) && same_user(&p.user, &e.user) && (p.in_step || on_song));
            (known || in_step).then(|| Listener {
                user: e.user.clone(),
                player: e.player.clone(),
                song_id: e.song.id.clone(),
                position_ms,
                playing,
                in_step: on_song,
                seen_ms: now_ms,
                queued: e.has_session_queue(),
                starting: e.state == Some(SessionState::Starting),
            })
        })
        .collect()
}

/// whether `user` listens along with this Navia: following someone else then would make two
/// people follow each other.
pub fn listens_along(listeners: &[Listener], user: &str) -> bool {
    listeners.iter().any(|l| same_user(&l.user, user))
}

/// What the host does to mirror its listeners, if anything: the first listener (in the server's
/// order) who changed since the last look (`prev` to `now`) to something the host isn't doing,
/// while the host's own last change is older than that look. Last change wins: a change of the
/// host's own after the look stands, and the listener follows it. A listener who merely followed
/// the host is where the host is, so nothing bounces back; and after one listener's change is
/// mirrored, another's older change doesn't count (the host changed after it). the listener whose
/// song or place the host took ([`Host::led`]) is the reference: the host seeks to them when the
/// two drift apart.
///
/// `control`: "Let listeners control playback" is on. Off, nothing is mirrored.
pub fn host_mirror(host: &Host, prev: &[Listener], now: &[Listener], control: bool) -> Option<Mirror> {
    if !control {
        return None;
    }
    let drift = ms(RESYNC_DRIFT);
    let mirror = |n: &Listener, action| Some(Mirror { action, user: n.user.clone(), player: n.player.clone() });
    for n in now {
        let Some(p) = prev.iter().find(|p| p.same(n)) else { continue };
        if host.own_change_ms > p.seen_ms {
            continue;
        }
        if n.song_id != p.song_id {
            if n.song_id != host.song_id {
                return mirror(n, HostAction::Song { song_id: n.song_id.clone(), position_ms: n.position_ms, playing: n.playing });
            }
            continue;
        }
        if n.song_id != host.song_id {
            continue;
        }
        let apart = !n.starting && n.position_ms.abs_diff(host.position_ms) > drift;
        let place = apart.then_some(n.position_ms);
        // Where plain play could have taken them since the last look (any of it, if they paused or
        // resumed in between); outside that, they seeked.
        let elapsed = if p.playing || n.playing { n.seen_ms.saturating_sub(p.seen_ms) } else { 0 };
        let jumped = !n.starting && !p.starting && (n.position_ms + drift < p.position_ms || n.position_ms > p.position_ms + elapsed + drift);
        if p.playing != n.playing && n.playing != host.playing {
            return mirror(n, if n.playing { HostAction::Play(place) } else { HostAction::Pause(place) });
        }
        if jumped && apart {
            return mirror(n, HostAction::Seek(n.position_ms));
        }
        let leads = host.led.as_ref().is_some_and(|l| l.song_id == host.song_id && same_user(&l.user, &n.user) && same_player(&l.player, &n.player) && host.own_change_ms <= l.at_ms);
        if leads && apart && n.playing == host.playing {
            return mirror(n, HostAction::Seek(n.position_ms));
        }
    }
    None
}

/// whose session song plays when the current song ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Turn {
    Mine,
    Theirs,
    /// nobody has one: the host's own queue carries on.
    Nobody,
}

/// which side played the last session song.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Mine,
    Theirs,
}

impl Side {
    pub fn flip(self) -> Self {
        match self {
            Self::Mine => Self::Theirs,
            Self::Theirs => Self::Mine,
        }
    }
}

/// the session song at the end of the current one, from what each side can see: its own queue,
/// the other side's queue mark, who played the last session song, and its role. both sides
/// queued: they take turns; at the start of a session the host goes first. both sides work it
/// out alike from the same view, so one starts and the other waits.
pub fn session_turn(mine: bool, theirs: bool, last: Option<Side>, host: bool) -> Turn {
    match (mine, theirs) {
        (false, false) => Turn::Nobody,
        (true, false) => Turn::Mine,
        (false, true) => Turn::Theirs,
        (true, true) => match last {
            Some(Side::Mine) => Turn::Theirs,
            Some(Side::Theirs) => Turn::Mine,
            None if host => Turn::Mine,
            None => Turn::Theirs,
        },
    }
}

/// how long until the next check while following or hosting: [`END_POLL`] in the last
/// [`END_WINDOW`] of a song (and while waiting at its end) during a listening session, otherwise
/// [`FOLLOW_POLL`].
pub fn check_every(session: bool, left_ms: Option<u64>, waiting: bool) -> Duration {
    if session && (waiting || left_ms.is_some_and(|l| l <= ms(END_WINDOW))) { END_POLL } else { FOLLOW_POLL }
}

/// one side's part of the hand-over at a song's end in a listening session. the app keeps it and
/// tells it what happens; it says what to do.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Handover {
    /// who played the last session song.
    pub last: Option<Side>,
    /// the turn worked out for the end of the current song, once in its last stretch.
    pub turn: Option<Turn>,
    /// waiting for the other side's song since (Unix ms).
    pub waiting_since: Option<u64>,
    /// this side's own session song started (its id and when, Unix ms), for a double start.
    pub started: Option<(String, u64)>,
}

impl Handover {
    /// the turn for the coming end, worked out again on each check in the song's last stretch.
    pub fn plan(&mut self, mine: bool, theirs: bool, host: bool) -> Turn {
        let turn = session_turn(mine, theirs, self.last, host);
        self.turn = Some(turn);
        turn
    }

    /// the current song ended here. `Mine`: play this side's next session song; `Theirs`: wait
    /// (bounded by [`SESSION_WAIT`]); `Nobody`: carry on as without a session.
    pub fn at_end(&mut self, mine: bool, theirs: bool, host: bool, now_ms: u64) -> Turn {
        let turn = self.plan(mine, theirs, host);
        self.waiting_since = (turn == Turn::Theirs).then_some(now_ms);
        turn
    }

    /// this side started its own session song.
    pub fn started_mine(&mut self, song_id: &str, now_ms: u64) {
        self.last = Some(Side::Mine);
        self.started = Some((song_id.to_string(), now_ms));
        self.turn = None;
        self.waiting_since = None;
    }

    /// a song from the other side arrived (the host took a listener's song, or the host moved to
    /// a new song a listener follows). returns the own session song to put back first when both
    /// sides started one at once (the listener's wins), if this side had just started one.
    pub fn theirs_arrived(&mut self, now_ms: u64) -> Option<String> {
        let was_waiting = self.waiting_since.take().is_some() || self.turn == Some(Turn::Theirs);
        let clash = self.started.take().filter(|(_, at)| now_ms.saturating_sub(*at) <= ms(SESSION_WAIT)).map(|(id, _)| id);
        if was_waiting || clash.is_some() {
            self.last = Some(Side::Theirs);
        }
        self.turn = None;
        clash
    }

    /// the wait at the end is over: their song didn't come within [`SESSION_WAIT`].
    pub fn wait_over(&mut self, now_ms: u64) -> bool {
        let over = self.waiting_since.is_some_and(|at| now_ms.saturating_sub(at) > ms(SESSION_WAIT));
        if over {
            self.waiting_since = None;
            self.turn = None;
        }
        over
    }

    /// a new song started that wasn't a hand-over (an own pick, the host's queue carrying on).
    pub fn song_changed(&mut self) {
        self.turn = None;
        self.waiting_since = None;
        self.started = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn marked() -> String {
        format!("navia{SHARE_MARK}")
    }

    fn entry(user: &str, player: &str, song: &str, state: Option<&str>, pos: Option<u64>) -> NowPlaying {
        let mut v = json!({ "id": song, "title": format!("Song {song}"), "username": user, "playerName": player, "minutesAgo": 0 });
        if let Some(s) = state {
            v["state"] = json!(s);
        }
        if let Some(p) = pos {
            v["positionMs"] = json!(p);
        }
        NowPlaying::from_entry(&v, true).unwrap()
    }

    fn seen(song: &str, playing: bool, at_ms: u64) -> Seen {
        Seen { song_id: song.into(), position_ms: 0, playing, at_ms }
    }

    fn local(song: &str, pos: u64, playing: bool) -> Local {
        Local { song_id: song.into(), position_ms: pos, playing, ..Default::default() }
    }

    #[test]
    fn entries_parse_with_and_without_playback_report() {
        let v = json!({ "id": "s1", "title": "One", "albumId": "al", "coverArt": "al", "username": "Alex", "playerName": marked(), "minutesAgo": 3, "state": "paused", "positionMs": 4100 });
        let e = NowPlaying::from_entry(&v, true).unwrap();
        assert_eq!((e.user.as_str(), e.state, e.position_ms, e.minutes_ago), ("Alex", Some(SessionState::Paused), Some(4100), 3));
        assert!(e.shared() && e.can_listen_along());
        assert_eq!(e.song.album_id.as_deref(), Some("al"));
        // A server without playbackReport is still listed, but without a live mark or Listen Along.
        let legacy = NowPlaying::from_entry(&v, false).unwrap();
        assert_eq!((legacy.state, legacy.position_ms), (None, None));
        assert!(legacy.shared() && !legacy.can_listen_along());
        // Entries with no user or no song are skipped.
        assert!(NowPlaying::from_entry(&json!({ "id": "s", "title": "t" }), true).is_none());
        assert!(NowPlaying::from_entry(&json!({ "username": "x" }), true).is_none());
    }

    #[test]
    fn listening_now_shows_other_sharers_once_each() {
        let m = marked();
        let entries = vec![
            entry("Alex", &m, "a1", Some("paused"), Some(5)),
            entry("Jo", "navia", "j1", Some("playing"), Some(5)),
            entry("Sam", "phone", "x1", Some("playing"), Some(5)),
            entry("me", &m, "m1", Some("playing"), Some(5)),
            entry("ME", &m, "m2", Some("playing"), Some(5)),
            entry("alex", &m, "a2", Some("playing"), Some(9)),
            entry("Mira", &m, "r1", Some("paused"), Some(5)),
            entry("Mira", &m, "r2", Some("paused"), Some(5)),
        ];
        let shown = people(&entries, "Me", true);
        let rows: Vec<(&str, &str)> = shown.iter().map(|e| (e.user.as_str(), e.song.id.as_str())).collect();
        // Only marked players of other people show, one row each, preferring the playing player.
        assert_eq!(rows, vec![("alex", "a2"), ("Mira", "r1")]);
        // Nobody shows with the viewer's own sharing off or in Private mode.
        assert!(people(&entries, "Me", false).is_empty());
        assert!(people(&[], "Me", true).is_empty());
    }

    #[test]
    fn following_resyncs_on_changes_and_drift_only() {
        let m = marked();
        let f = Followed { user: "Alex".into(), player: m.clone() };
        let now = |song: &str, state: &str, pos: u64| vec![entry("Alex", &m, song, Some(state), Some(pos))];
        // In step, a second apart, so no jump.
        assert_eq!(follow_step(&f, &now("a1", "playing", 61_000), &local("a1", 62_000, true)), FollowStep::Keep { drift_ms: 1000, player: m.clone() });
        // More than 2 s apart either way resyncs.
        assert!(matches!(follow_step(&f, &now("a1", "playing", 61_000), &local("a1", 63_500, true)), FollowStep::Resync { .. }));
        assert!(matches!(follow_step(&f, &now("a1", "playing", 61_000), &local("a1", 58_000, true)), FollowStep::Resync { .. }));
        // A new song, a pause or a resume resyncs at once, whatever the drift.
        assert!(matches!(follow_step(&f, &now("a2", "playing", 1000), &local("a1", 1000, true)), FollowStep::Resync { .. }));
        assert!(matches!(follow_step(&f, &now("a1", "paused", 61_000), &local("a1", 61_000, true)), FollowStep::Resync { .. }));
        assert!(matches!(follow_step(&f, &now("a1", "playing", 61_000), &local("a1", 61_000, false)), FollowStep::Resync { .. }));
        // Starting counts as playing.
        assert!(matches!(follow_step(&f, &now("a1", "starting", 0), &local("a1", 500, true)), FollowStep::Keep { .. }));
        match follow_step(&f, &now("a2", "paused", 7000), &local("a1", 1000, true)) {
            FollowStep::Resync { entry, .. } => assert_eq!((entry.song.id.as_str(), entry.position_ms, entry.state), ("a2", Some(7000), Some(SessionState::Paused))),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_listeners_pause_holds_and_waits_for_the_host_to_mirror_it() {
        let m = marked();
        let f = Followed { user: "Alex".into(), player: m.clone() };
        let playing = |pos| vec![entry("Alex", &m, "a1", Some("playing"), Some(pos))];
        let paused = |pos| vec![entry("Alex", &m, "a1", Some("paused"), Some(pos))];
        // The listener paused at t=110 after last seeing the host playing at t=100: wait for the mirror.
        let mine = Local { held: true, own_change_ms: Some(110), seen: Some(seen("a1", true, 100)), ..local("a1", 30_000, false) };
        assert!(matches!(follow_step(&f, &playing(31_000), &mine), FollowStep::Sending { .. }));
        // the host hasn't mirrored by the next check (seen at 115): the listener's pause is still on its way.
        let later = Local { seen: Some(at("a1", 35_000, true, 115)), ..mine.clone() };
        assert!(matches!(follow_step(&f, &playing(36_000), &later), FollowStep::Sending { .. }));
        // The host mirrored: in step, paused.
        assert!(matches!(follow_step(&f, &paused(30_500), &later), FollowStep::Keep { .. }));
        // The host plays again after that (seen paused at 120): the host's change wins.
        let host_resumed = Local { seen: Some(seen("a1", false, 120)), ..later.clone() };
        match follow_step(&f, &playing(30_600), &host_resumed) {
            FollowStep::Resync { entry, .. } => assert_eq!(entry.state, Some(SessionState::Playing)),
            other => panic!("{other:?}"),
        }
        // A host song change after the listener's last change is followed, held or not.
        assert!(matches!(follow_step(&f, &[entry("Alex", &m, "a2", Some("playing"), Some(0))], &later), FollowStep::Resync { .. }));
        // The listener's own song change (control on) waits for the host to mirror it.
        let picked = Local { held: false, own_change_ms: Some(130), seen: Some(seen("a1", true, 125)), ..local("b9", 1_000, true) };
        assert!(matches!(follow_step(&f, &playing(40_000), &picked), FollowStep::Sending { .. }));
        // The listener plays at 130 while the host is paused (seen at 125): wait for the host to resume.
        let resume = Local { held: false, own_change_ms: Some(130), seen: Some(seen("a1", false, 125)), ..local("a1", 30_500, true) };
        assert!(matches!(follow_step(&f, &paused(30_500), &resume), FollowStep::Sending { .. }));
        // They stop: following still ends.
        assert_eq!(follow_step(&f, &[], &mine), FollowStep::Ended);
    }

    fn host(song: &str, pos: u64, playing: bool, changed: u64) -> Host {
        Host { song_id: song.into(), position_ms: pos, playing, own_change_ms: changed, led: None }
    }

    fn follower() -> String {
        format!("navia{SHARE_MARK}{FOLLOW_MARK}")
    }

    #[test]
    fn marks_read_back() {
        let e = entry("Jo", &format!("navia{SHARE_MARK}{CONTROL_MARK}"), "a1", Some("playing"), Some(0));
        assert!(e.shared() && e.allows_control() && !e.follows());
        assert!(entry("Jo", &follower(), "a1", Some("playing"), Some(0)).follows());
        assert!(!entry("Jo", "navia", "a1", Some("playing"), Some(0)).shared());
        assert_eq!(crate::client::client_display_name(&format!("navia{SHARE_MARK}{CONTROL_MARK}{FOLLOW_MARK}")), "Navia");
    }

    /// Players named by the current build ("Navia") and by older ones ("navia") are read alike.
    #[test]
    fn old_and_new_client_names_share_alike() {
        let old = entry("Jo", &format!("navia{SHARE_MARK}{CONTROL_MARK}"), "a1", Some("playing"), Some(0));
        let new = entry("Sam", &format!("Navia{SHARE_MARK}{CONTROL_MARK}"), "b1", Some("playing"), Some(0));
        for e in [&old, &new] {
            assert!(e.shared() && e.allows_control());
            assert!(crate::client::is_navia_client(&e.player));
        }
        assert_eq!(people(&[old, new], "me", true).len(), 2);
        assert!(!entry("Jo", "Navia", "a1", Some("playing"), Some(0)).shared());
    }

    #[test]
    fn the_host_mirrors_a_listeners_pause_and_resume() {
        let m = follower();
        let h = host("a1", 60_000, true, 10);
        // Jo follows and plays the host's song in step, so Jo is a listener. Sam plays another song,
        // and Kim plays the same song without following anyone, so neither is one.
        let first = listeners(&[entry("Jo", &m, "a1", Some("playing"), Some(59_000)), entry("Sam", &m, "b1", Some("playing"), Some(60_000)), entry("Kim", &marked(), "a1", Some("playing"), Some(60_000))], "Me", &h, &[], 100_000);
        assert_eq!(first.iter().map(|l| l.user.as_str()).collect::<Vec<_>>(), ["Jo"]);
        assert_eq!(host_mirror(&h, &[], &first, true).map(|m| m.action), None);
        // Jo pauses: the host pauses too, only with control on.
        let h5 = host("a1", 65_000, true, 10);
        let second = listeners(&[entry("Jo", &m, "a1", Some("paused"), Some(62_000))], "Me", &h5, &first, 105_000);
        assert_eq!(second.len(), 1, "still a listener while paused, though out of step");
        assert_eq!(host_mirror(&h5, &first, &second, false).map(|m| m.action), None);
        assert_eq!(host_mirror(&h5, &first, &second, true).map(|m| m.action), Some(HostAction::Pause(Some(62_000))), "with jo's place, 3 s from the host's");
        // The host is paused now (a mirror, so its own change time stays). Jo resumes: the host resumes.
        let h_paused = host("a1", 65_000, false, 10);
        let third = listeners(&[entry("Jo", &m, "a1", Some("playing"), Some(62_500))], "Me", &h_paused, &second, 110_000);
        assert_eq!(host_mirror(&h_paused, &second, &third, true).map(|m| m.action), Some(HostAction::Play(Some(62_500))));
    }

    #[test]
    fn the_host_mirrors_a_listeners_song_and_seek() {
        let m = follower();
        let h = host("a1", 60_000, true, 10);
        let first = listeners(&[entry("Jo", &m, "a1", Some("playing"), Some(60_000))], "Me", &h, &[], 100_000);
        // Jo plays another song, and the host plays it from Jo's position.
        let h2 = host("a1", 65_000, true, 10);
        let song = listeners(&[entry("Jo", &m, "b7", Some("playing"), Some(3_000))], "Me", &h2, &first, 105_000);
        assert_eq!(song.len(), 1, "a listener who changed song is still one");
        assert_eq!(host_mirror(&h2, &first, &song, true).map(|m| m.action), Some(HostAction::Song { song_id: "b7".into(), position_ms: 3_000, playing: true }));
        // Jo seeks to 2:00 on the host's song: the host seeks.
        let seek = listeners(&[entry("Jo", &m, "a1", Some("playing"), Some(120_000))], "Me", &h2, &first, 105_000);
        assert_eq!(host_mirror(&h2, &first, &seek, true).map(|m| m.action), Some(HostAction::Seek(120_000)));
        // Plain play moving on 5 s is no seek.
        let on = listeners(&[entry("Jo", &m, "a1", Some("playing"), Some(65_000))], "Me", &h2, &first, 105_000);
        assert_eq!(host_mirror(&h2, &first, &on, true).map(|m| m.action), None);
        // Someone no longer on the host's song at two looks in a row isn't a listener any more.
        let drifted = listeners(&[entry("Jo", &m, "c1", Some("playing"), Some(0))], "Me", &h2, &[Listener { in_step: false, ..song[0].clone() }], 110_000);
        assert!(drifted.is_empty());
    }

    #[test]
    fn a_listener_leaving_starting_is_no_seek() {
        let h = host("a1", 20_000, true, 10);
        let jo = |position_ms, seen_ms, starting| Listener { user: "Jo".into(), player: follower(), song_id: "a1".into(), position_ms, playing: true, in_step: true, seen_ms, queued: false, starting };
        // held at 5 s while starting, then playing at 12 s: the held place is stale, not a jump back
        assert_eq!(host_mirror(&h, &[jo(5_000, 100_000, true)], &[jo(12_000, 105_000, false)], true).map(|m| m.action), None);
        assert_eq!(host_mirror(&h, &[jo(5_000, 100_000, false)], &[jo(12_000, 101_000, false)], true).map(|m| m.action), Some(HostAction::Seek(12_000)), "the same move from playing is a seek");
    }

    #[test]
    fn two_listeners_in_one_check_have_one_winner_and_nothing_loops() {
        let m = follower();
        let h = host("a1", 60_000, true, 10);
        let first = listeners(&[entry("Jo", &m, "a1", Some("playing"), Some(60_000)), entry("Ana", &m, "a1", Some("playing"), Some(60_000))], "Me", &h, &[], 100_000);
        // Jo plays b7 and Ana pauses within the same check. Only the first listener's change is mirrored.
        let h2 = host("a1", 65_000, true, 10);
        let both = listeners(&[entry("Jo", &m, "b7", Some("playing"), Some(1_000)), entry("Ana", &m, "a1", Some("paused"), Some(62_000))], "Me", &h2, &first, 105_000);
        assert_eq!(host_mirror(&h2, &first, &both, true).map(|m| m.action), Some(HostAction::Song { song_id: "b7".into(), position_ms: 1_000, playing: true }));
        // Next look: the host plays b7 (a mirror). Jo is there; Ana's pause is old news (no change
        // since the last look), and she follows the host to b7: nothing more to mirror.
        let h3 = host("b7", 5_000, true, 10);
        let after = listeners(&[entry("Jo", &m, "b7", Some("playing"), Some(5_000)), entry("Ana", &m, "a1", Some("paused"), Some(62_000))], "Me", &h3, &both, 110_000);
        assert_eq!(host_mirror(&h3, &both, &after, true).map(|m| m.action), None);
        // On Ana's side: her pause was before her last look at the host, and the host changed song
        // since: she follows the host.
        let ana = Local { held: true, own_change_ms: Some(104_000), seen: Some(seen("a1", true, 106_000)), ..local("a1", 62_000, false) };
        let f = Followed { user: "Me".into(), player: marked() };
        assert!(matches!(follow_step(&f, &[entry("Me", &marked(), "b7", Some("playing"), Some(5_000))], &ana), FollowStep::Resync { .. }));
    }

    #[test]
    fn nothing_bounces_back_and_the_last_change_wins() {
        let m = follower();
        let prev = vec![Listener { user: "Jo".into(), player: m.clone(), song_id: "a1".into(), position_ms: 60_000, playing: true, in_step: true, seen_ms: 100_000, queued: false, starting: false }];
        // The host's user paused at 102; Jo paused too (mirroring the host): same state, nothing to do.
        let h = host("a1", 60_000, false, 102_000);
        let now = listeners(&[entry("Jo", &m, "a1", Some("paused"), Some(60_000))], "Me", &h, &prev, 105_000);
        assert_eq!(host_mirror(&h, &prev, &now, true).map(|m| m.action), None);
        // Both toggled within one check from the same state: both paused, no mirror on either side.
        let jo = Local { held: true, own_change_ms: Some(103_000), seen: Some(seen("a1", true, 100_000)), ..local("a1", 60_000, false) };
        let f = Followed { user: "Me".into(), player: marked() };
        assert!(matches!(follow_step(&f, &[entry("Me", &marked(), "a1", Some("paused"), Some(60_000))], &jo), FollowStep::Keep { .. }));
        // Jo resumed at 104 but the host's user paused at 106, after the host last looked at Jo at 105.
        // The host's change is newer, so Jo follows the host's pause.
        let h2 = host("a1", 60_000, false, 106_000);
        let jo_seen_paused = vec![Listener { playing: false, seen_ms: 105_000, ..prev[0].clone() }];
        let jo_now = listeners(&[entry("Jo", &m, "a1", Some("playing"), Some(60_500))], "Me", &h2, &jo_seen_paused, 110_000);
        assert_eq!(host_mirror(&h2, &jo_seen_paused, &jo_now, true).map(|m| m.action), None);
        let jo_local = Local { held: false, own_change_ms: Some(104_000), seen: Some(seen("a1", true, 105_000)), ..local("a1", 60_500, true) };
        match follow_step(&f, &[entry("Me", &marked(), "a1", Some("paused"), Some(60_000))], &jo_local) {
            FollowStep::Resync { entry, .. } => assert_eq!(entry.state, Some(SessionState::Paused)),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn following_ends_when_they_stop_or_cant_be_followed() {
        let m = marked();
        let f = Followed { user: "Alex".into(), player: m.clone() };
        let here = local("a1", 1000, true);
        // Gone from the answer, stopped or quiet too long.
        assert_eq!(follow_step(&f, &[], &here), FollowStep::Ended);
        // Still playing but no longer shared, or another app's session, so not theirs to follow.
        assert_eq!(follow_step(&f, &[entry("Alex", "navia", "a1", Some("playing"), Some(1000))], &here), FollowStep::Ended);
        // A legacy answer has no state or position, so it can't be followed.
        let legacy = NowPlaying { state: None, position_ms: None, ..entry("Alex", &m, "a1", Some("playing"), Some(1000)) };
        assert_eq!(follow_step(&f, &[legacy], &here), FollowStep::Ended);
    }

    #[test]
    fn following_keeps_to_the_same_player_or_the_one_playing() {
        let m = marked();
        let other = format!("Navia{SHARE_MARK}");
        let f = Followed { user: "Alex".into(), player: m.clone() };
        // With two shared players, the followed one wins even when paused.
        let both = vec![entry("Alex", &other, "b1", Some("playing"), Some(1000)), entry("Alex", &m, "a1", Some("paused"), Some(1000))];
        assert!(matches!(follow_step(&f, &both, &local("a1", 1000, false)), FollowStep::Keep { .. }));
        // When the followed player is gone, the one that plays takes over.
        let moved = vec![entry("Alex", &other, "b1", Some("paused"), Some(1000)), entry("alex", &other, "b2", Some("playing"), Some(3000))];
        match follow_step(&f, &moved, &local("a1", 1000, true)) {
            FollowStep::Resync { entry, player } => assert_eq!((entry.song.id.as_str(), player), ("b2", other)),
            other => panic!("{other:?}"),
        }
    }

    fn at(song: &str, pos: u64, playing: bool, at_ms: u64) -> Seen {
        Seen { song_id: song.into(), position_ms: pos, playing, at_ms }
    }

    /// a listener skips; the host takes it a check later and starts behind. the listener keeps
    /// playing, the host comes to it, and they end up within the resync drift.
    #[test]
    fn a_listeners_skip_is_never_pulled_back_and_the_host_comes_to_it() {
        let host_name = format!("navia{SHARE_MARK}{CONTROL_MARK}");
        let f = Followed { user: "Alex".into(), player: host_name.clone() };
        let alex = |song: &str, state: &str, pos: u64| vec![entry("Alex", &host_name, song, Some(state), Some(pos))];
        // jo skips to b7 at t=100 s, having last seen the host on a1 at 99 s
        let mut jo = Local { own_change_ms: Some(100_000), lead: true, seen: Some(at("a1", 60_000, true, 99_000)), now_ms: 102_000, ..local("b7", 2_000, true) };
        assert_eq!(follow_step(&f, &alex("a1", "playing", 63_000), &jo), FollowStep::Sending { player: host_name.clone() });
        // still not taken at the next look: still sending, never back to a1
        jo.seen = Some(at("a1", 63_000, true, 102_000));
        jo.now_ms = 107_000;
        jo.position_ms = 7_000;
        assert!(matches!(follow_step(&f, &alex("a1", "playing", 68_000), &jo), FollowStep::Sending { .. }));
        // the host's look at 104 s: jo was on a1 at 99 s and is on b7 at 4 s now
        let m = follower();
        let h = Host { song_id: "a1".into(), position_ms: 65_000, playing: true, own_change_ms: 10, led: None };
        let before = listeners(&[entry("Jo", &m, "a1", Some("playing"), Some(59_000))], "Alex", &Host { position_ms: 60_000, ..h.clone() }, &[], 99_000);
        let now = listeners(&[entry("Jo", &m, "b7", Some("playing"), Some(4_000))], "Alex", &h, &before, 104_000);
        let mirror = host_mirror(&h, &before, &now, true).unwrap();
        assert_eq!(mirror.action, HostAction::Song { song_id: "b7".into(), position_ms: 4_000, playing: true });
        // the host starts late: 3 s behind jo. jo's next look: taken, and not pulled back
        let led = mirror.led(&h, 104_000);
        jo.seen = Some(at("a1", 68_000, true, 107_000));
        jo.now_ms = 109_000;
        jo.position_ms = 9_000;
        assert_eq!(follow_step(&f, &alex("b7", "playing", 6_000), &jo), FollowStep::Keep { drift_ms: 3_000, player: host_name.clone() });
        // the app clears the change once taken; the lead stays, so drift still doesn't pull jo back
        jo.own_change_ms = None;
        jo.seen = Some(at("b7", 6_000, true, 109_000));
        jo.now_ms = 111_000;
        jo.position_ms = 11_000;
        assert!(matches!(follow_step(&f, &alex("b7", "playing", 8_000), &jo), FollowStep::Keep { drift_ms: 3_000, .. }));
        // the host's next look: jo leads b7 and is 3 s ahead, so the host seeks to jo
        let h2 = Host { song_id: "b7".into(), position_ms: 8_000, playing: true, own_change_ms: 10, led };
        let later = listeners(&[entry("Jo", &m, "b7", Some("playing"), Some(11_000))], "Alex", &h2, &now, 111_000);
        assert_eq!(host_mirror(&h2, &now, &later, true).map(|m| m.action), Some(HostAction::Seek(11_000)));
        // in step now: within the drift on both sides, nothing more to do
        let h3 = Host { position_ms: 13_100, ..h2.clone() };
        let settled = listeners(&[entry("Jo", &m, "b7", Some("playing"), Some(13_000))], "Alex", &h3, &later, 113_000);
        assert_eq!(host_mirror(&h3, &later, &settled, true), None);
        jo.seen = Some(at("b7", 8_000, true, 111_000));
        jo.now_ms = 113_000;
        jo.position_ms = 13_000;
        match follow_step(&f, &alex("b7", "playing", 13_100), &jo) {
            FollowStep::Keep { drift_ms, .. } => assert!(drift_ms.unsigned_abs() <= ms(RESYNC_DRIFT)),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_hosts_own_seek_still_wins_over_a_listeners_lead() {
        let m = marked();
        let f = Followed { user: "Alex".into(), player: m.clone() };
        let jo = Local { lead: true, seen: Some(at("b7", 20_000, true, 100_000)), now_ms: 105_000, ..local("b7", 25_000, true) };
        // alex seeks to 2:00 themselves: jo follows
        assert!(matches!(follow_step(&f, &[entry("Alex", &m, "b7", Some("playing"), Some(120_000))], &jo), FollowStep::Resync { .. }));
        // a host's song that is still starting holds its reported place: no pull back to it
        let starting = Local { lead: false, ..jo.clone() };
        assert!(matches!(follow_step(&f, &[entry("Alex", &m, "b7", Some("starting"), Some(0))], &starting), FollowStep::Keep { .. }));
    }

    #[test]
    fn a_change_the_host_never_takes_is_given_up_after_a_while() {
        let m = marked();
        let f = Followed { user: "Alex".into(), player: m.clone() };
        let jo = Local { own_change_ms: Some(100_000), lead: true, seen: Some(at("a1", 60_000, true, 101_000)), now_ms: 100_000 + ms(SEND_WAIT) + 1, ..local("b7", 15_000, true) };
        assert!(matches!(follow_step(&f, &[entry("Alex", &m, "a1", Some("playing"), Some(75_000))], &jo), FollowStep::NotTaken { .. }));
        // the host's own new song after the change wins at once
        let moved = Local { now_ms: 106_000, ..jo.clone() };
        assert!(matches!(follow_step(&f, &[entry("Alex", &m, "c3", Some("playing"), Some(0))], &moved), FollowStep::Resync { .. }));
    }

    #[test]
    fn positions_move_on_by_the_age_of_the_answer() {
        let mut e = vec![entry("Alex", &marked(), "a1", Some("playing"), Some(1_000)), entry("Jo", &marked(), "b1", Some("paused"), Some(1_000)), entry("Kim", &marked(), "c1", Some("starting"), Some(0))];
        e[0].song.duration = Some(2);
        advance(&mut e, 400);
        assert_eq!(e.iter().map(|e| e.position_ms.unwrap()).collect::<Vec<_>>(), [1_400, 1_000, 0]);
        advance(&mut e, 5_000);
        assert_eq!(e[0].position_ms, Some(2_000), "never past the end");
    }

    fn follows_back() -> String {
        format!("Navia{SHARE_MARK}{CONTROL_MARK}{FOLLOW_MARK}")
    }

    /// Ana and Ben follow each other. whoever changes the song, it never snaps back, and after a
    /// while only Ben (later in name order) still follows.
    #[test]
    fn two_people_following_each_other_never_snap_back() {
        let p = follows_back();
        let ana_sees = |song: &str, pos| vec![entry("Ben", &p, song, Some("playing"), Some(pos)), entry("Ana", &p, "x", Some("playing"), Some(0))];
        let ben_sees = |song: &str, pos| vec![entry("Ana", &p, song, Some("playing"), Some(pos)), entry("Ben", &p, "x", Some("playing"), Some(0))];
        let follow_ben = Followed { user: "Ben".into(), player: p.clone() };
        let follow_ana = Followed { user: "Ana".into(), player: p.clone() };
        assert!(two_way(&follow_ben, &ana_sees("a1", 10_000), "Ana"));
        // someone else, not following, plays Ben's song: Ben follows them, not Ana
        let mut chain = ana_sees("a1", 10_000);
        chain.push(entry("Cy", &marked(), "a1", Some("playing"), Some(10_000)));
        assert!(!two_way(&follow_ben, &chain, "Ana"));

        // ana changes to a2 at 100 s; ben still shows a1
        let ana = Local { viewer: "Ana".into(), own_change_ms: Some(100_000), lead: true, seen: Some(at("a1", 9_000, true, 99_000)), now_ms: 101_000, two_way_since: Some(95_000), ..local("a2", 1_000, true) };
        assert!(matches!(follow_step(&follow_ben, &ana_sees("a1", 11_000), &ana), FollowStep::Sending { .. }));
        // ben (who follows ana and doesn't lead) takes it
        let ben = Local { viewer: "Ben".into(), seen: Some(at("a1", 9_000, true, 99_000)), now_ms: 102_000, two_way_since: Some(95_000), ..local("a1", 11_000, true) };
        assert!(matches!(follow_step(&follow_ana, &ben_sees("a2", 2_000), &ben), FollowStep::Resync { .. }));
        // ana's next look: ben is on a2, nothing goes back; past the settle time ana stops following
        let ana_then = Local { own_change_ms: None, seen: Some(at("a1", 11_000, true, 101_000)), now_ms: 104_000, position_ms: 4_000, ..ana.clone() };
        assert_eq!(follow_step(&follow_ben, &ana_sees("a2", 3_800), &ana_then), FollowStep::Lead { user: "Ben".into() });
        let early = Local { two_way_since: Some(103_000), ..ana_then.clone() };
        assert!(matches!(follow_step(&follow_ben, &ana_sees("a2", 3_800), &early), FollowStep::Keep { .. }));

        // the other way round: ben changes to b5; ana, who leads, takes it as a new change of ben's
        let ben_change = Local { own_change_ms: Some(200_000), lead: true, seen: Some(at("a2", 50_000, true, 199_000)), now_ms: 201_000, ..local("b5", 1_000, true) };
        assert!(matches!(follow_step(&follow_ana, &ben_sees("a2", 51_000), &ben_change), FollowStep::Sending { .. }));
        let ana_follows = Local { viewer: "Ana".into(), seen: Some(at("a2", 50_000, true, 199_000)), now_ms: 202_000, two_way_since: Some(201_000), ..local("a2", 52_000, true) };
        assert!(matches!(follow_step(&follow_ben, &ana_sees("b5", 1_500), &ana_follows), FollowStep::Resync { .. }));
        // both pressed Listen Along at once and swapped songs: ana (who leads) keeps hers, ben comes to it
        let swapped = Local { viewer: "Ana".into(), seen: None, now_ms: 300_000, two_way_since: Some(300_000), ..local("b1", 30_000, true) };
        assert!(matches!(follow_step(&follow_ben, &ana_sees("a1", 30_000), &swapped), FollowStep::Keep { .. }));
        let ben_swapped = Local { viewer: "Ben".into(), seen: None, now_ms: 300_000, two_way_since: Some(300_000), ..local("a1", 30_000, true) };
        assert!(matches!(follow_step(&follow_ana, &ben_sees("b1", 30_000), &ben_swapped), FollowStep::Resync { .. }));
    }

    #[test]
    fn listen_along_with_someone_who_listens_along_with_you_is_spotted() {
        let h = host("a1", 60_000, true, 10);
        let now = listeners(&[entry("Jo", &follower(), "a1", Some("playing"), Some(60_500))], "Me", &h, &[], 100_000);
        assert!(listens_along(&now, "jo"));
        assert!(!listens_along(&now, "Sam"));
    }

    #[test]
    fn the_listener_whose_song_the_host_took_leads_until_the_host_changes() {
        let m = follower();
        let h = Host { led: Some(Led { user: "Jo".into(), player: m.clone(), song_id: "a1".into(), at_ms: 50 }), ..host("a1", 60_000, true, 10) };
        let prev = listeners(&[entry("Jo", &m, "a1", Some("playing"), Some(62_500))], "Me", &h, &[], 100_000);
        let now = listeners(&[entry("Jo", &m, "a1", Some("playing"), Some(67_600))], "Me", &Host { position_ms: 65_000, ..h.clone() }, &prev, 105_000);
        let h5 = Host { position_ms: 65_000, ..h.clone() };
        assert_eq!(host_mirror(&h5, &prev, &now, true).map(|m| m.action), Some(HostAction::Seek(67_600)));
        // the host's user changed something since: no more coming to jo
        assert_eq!(host_mirror(&Host { own_change_ms: 60, ..h5.clone() }, &prev, &now, true), None);
        // a pause far apart carries jo's place
        let paused = listeners(&[entry("Jo", &m, "a1", Some("paused"), Some(62_000))], "Me", &h5, &prev, 105_000);
        assert_eq!(host_mirror(&h5, &prev, &paused, true).map(|m| m.action), Some(HostAction::Pause(Some(62_000))));
    }

    #[test]
    fn session_turns_alternate_and_both_sides_agree() {
        // nobody, one side, both: the host first, then turns
        assert_eq!(session_turn(false, false, None, true), Turn::Nobody);
        assert_eq!(session_turn(true, false, Some(Side::Mine), false), Turn::Mine);
        assert_eq!(session_turn(false, true, None, true), Turn::Theirs);
        for last in [None, Some(Side::Mine), Some(Side::Theirs)] {
            let host = session_turn(true, true, last, true);
            let listener = session_turn(true, true, last.map(Side::flip), false);
            assert_ne!(host, listener, "exactly one side starts ({last:?})");
            assert_ne!(host, Turn::Nobody);
        }
        // three ends with both queued: host, listener, host
        let (mut h, mut l) = (Handover::default(), Handover::default());
        let mut order = Vec::new();
        for k in 0..3u64 {
            let now = 1_000_000 + k * 200_000;
            let (ht, lt) = (h.at_end(true, true, true, now), l.at_end(true, true, false, now));
            assert_ne!(ht, lt);
            if ht == Turn::Mine {
                h.started_mine("h", now);
                assert_eq!(l.theirs_arrived(now + 800), None);
                order.push("host");
            } else {
                l.started_mine("l", now);
                assert_eq!(h.theirs_arrived(now + 800), None);
                order.push("listener");
            }
        }
        assert_eq!(order, ["host", "listener", "host"]);
    }

    #[test]
    fn the_wait_at_the_end_is_bounded_and_a_double_start_is_undone() {
        let mut h = Handover::default();
        assert_eq!(h.at_end(false, true, true, 1_000), Turn::Theirs);
        assert!(!h.wait_over(1_000 + ms(SESSION_WAIT)));
        assert!(h.wait_over(1_001 + ms(SESSION_WAIT)), "carries on after the wait");
        assert_eq!(h.waiting_since, None);
        // the listener's queue mark came too late for the host to see: both started a song. the
        // listener's wins; the host's own goes back to the front of its session queue
        let mut h = Handover::default();
        assert_eq!(h.at_end(true, false, true, 5_000), Turn::Mine);
        h.started_mine("own", 5_000);
        assert_eq!(h.theirs_arrived(6_000), Some("own".to_string()));
        assert_eq!(h.last, Some(Side::Theirs));
        // later on, a song of theirs isn't a clash
        h.started_mine("own2", 10_000);
        assert_eq!(h.theirs_arrived(10_000 + ms(SESSION_WAIT) + 1), None);
    }

    #[test]
    fn checks_speed_up_only_at_the_end_of_a_song_in_a_session() {
        assert_eq!(check_every(false, Some(3_000), false), FOLLOW_POLL);
        assert_eq!(check_every(true, Some(60_000), false), FOLLOW_POLL);
        assert_eq!(check_every(true, Some(9_000), false), END_POLL);
        assert_eq!(check_every(true, None, true), END_POLL);
    }

    #[test]
    fn the_queue_mark_reads_back_and_is_stripped_from_the_name() {
        let name = format!("Navia{SHARE_MARK}{CONTROL_MARK}{QUEUE_MARK}");
        let e = entry("Jo", &name, "a1", Some("playing"), Some(0));
        assert!(e.has_session_queue() && e.allows_control() && !e.follows());
        assert!(!entry("Jo", &format!("Navia{QUEUE_MARK}"), "a1", Some("playing"), Some(0)).has_session_queue(), "only while sharing");
        assert_eq!(crate::client::client_display_name(&name), "Navia");
        // the same player when only its marks changed
        let f = Followed { user: "Jo".into(), player: format!("Navia{SHARE_MARK}{CONTROL_MARK}") };
        let other = entry("Jo", &format!("Phone{SHARE_MARK}"), "z", Some("playing"), Some(0));
        assert_eq!(followed_entry(&f, &[other, e.clone()]), Some(&e));
    }
}
