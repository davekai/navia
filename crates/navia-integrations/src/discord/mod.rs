//! Discord "Listening to" status over the local IPC socket.

mod activity;
mod ipc;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};

use self::activity::{Presence, Signature, build};
use self::ipc::{Connection, Frame, OP_CLOSE, OP_FRAME, OP_HANDSHAKE, OP_PING, OP_PONG};
use crate::art::ArtSource;
use crate::{Integration, PlaybackState, PlayerSnapshot, TrackInfo};

pub use self::ipc::default_socket_dirs;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);
const REPLY_TIMEOUT: Duration = Duration::from_secs(1);
const TICK: Duration = Duration::from_secs(1);
const IDLE_TICK: Duration = Duration::from_secs(3600);
/// A later position within this much of the expected one isn't a seek.
const SEEK_TOLERANCE_MS: i64 = 2000;
const MAX_ART_CACHE: usize = 512;
const ART_HIT_TTL: Duration = Duration::from_secs(24 * 3600);
const ART_MISS_TTL: Duration = Duration::from_secs(6 * 3600);

/// Which line follows "Listening to" in the member list. The activity card always shows the song, artist and album.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum StatusText {
    /// The name of the Discord application.
    AppName,
    /// The default. Shows "♫ Artist" in the member list.
    #[default]
    Artist,
    Song,
}

impl StatusText {
    fn wire_value(self) -> Option<u64> {
        match self {
            StatusText::AppName => None,
            StatusText::Artist => Some(1),
            StatusText::Song => Some(2),
        }
    }
}

#[derive(Clone, Debug)]
pub struct DiscordConfig {
    /// The Application ID from Discord's developer portal.
    pub application_id: String,
    /// Shown until album art is found, or if there is none. An asset key from the developer portal, or an `https` URL.
    pub default_image: Option<String>,
    pub status_text: StatusText,
    /// Where to look for Discord's socket. `None` uses the standard places.
    pub socket_dirs: Option<Vec<PathBuf>>,
    pub reconnect_min: Duration,
    pub reconnect_max: Duration,
    /// Discord allows 5 updates per 20 seconds.
    pub rate_limit: usize,
    pub rate_window: Duration,
}

impl DiscordConfig {
    pub fn new(application_id: impl Into<String>) -> Self {
        Self {
            application_id: application_id.into(),
            default_image: None,
            status_text: StatusText::default(),
            socket_dirs: None,
            reconnect_min: Duration::from_secs(5),
            reconnect_max: Duration::from_secs(60),
            rate_limit: 5,
            rate_window: Duration::from_secs(20),
        }
    }
}

#[derive(Debug)]
pub struct InvalidApplicationId;

impl std::fmt::Display for InvalidApplicationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the Discord application id must be a number")
    }
}

impl std::error::Error for InvalidApplicationId {}

enum Message {
    Desired(Option<Presence>),
    /// `key` is the song's key. `album_key` is set when the cover applies to the whole album.
    ArtFound { generation: u64, key: String, album_key: Option<String>, url: Option<String> },
    Shutdown,
}

/// Keeps Discord's status in step with the player on a background thread.
pub struct DiscordPresence {
    sender: Sender<Message>,
    thread: Option<JoinHandle<()>>,
}

impl DiscordPresence {
    pub fn new(config: DiscordConfig, art: Arc<dyn ArtSource>) -> Result<Self, InvalidApplicationId> {
        let id = &config.application_id;
        if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(InvalidApplicationId);
        }
        let (sender, receiver) = mpsc::channel();
        let worker = Worker::new(config, art, sender.clone());
        let thread = thread::Builder::new()
            .name("navia-discord".into())
            .spawn(move || worker.run(receiver))
            .expect("spawn Discord thread");
        Ok(Self {
            sender,
            thread: Some(thread),
        })
    }

    pub fn set_snapshot(&self, snapshot: &PlayerSnapshot) {
        let desired = (snapshot.state == PlaybackState::Playing && !snapshot.private_mode)
            .then(|| snapshot.track.clone())
            .flatten()
            .map(|track| Presence {
                track,
                position: snapshot.position,
                captured_at: SystemTime::now(),
            });
        let _ = self.sender.send(Message::Desired(desired));
    }
}

impl Integration for DiscordPresence {
    fn update(&mut self, snapshot: &PlayerSnapshot) {
        self.set_snapshot(snapshot);
    }
}

impl Drop for DiscordPresence {
    fn drop(&mut self) {
        let _ = self.sender.send(Message::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct ArtJob {
    generation: u64,
    key: String,
    album_key: String,
    track: TrackInfo,
}

struct SentActivity {
    signature: Signature,
    started_at_ms: i64,
}

struct Worker {
    config: DiscordConfig,
    dirs: Vec<PathBuf>,
    connection: Option<Connection>,
    desired: Option<Presence>,
    sent: Option<SentActivity>,
    next_connect: Instant,
    backoff: Duration,
    recent_sends: VecDeque<Instant>,
    wake_at: Option<Instant>,
    nonce: u64,
    /// Found covers and misses with when each was learned, so misses are retried after a while.
    art_cache: HashMap<String, (Option<String>, Instant)>,
    art_requested: Option<String>,
    art_generation: Arc<AtomicU64>,
    art_jobs: Sender<ArtJob>,
    art: Arc<dyn ArtSource>,
    /// The art source generation the cache was filled under.
    art_seen: u64,
    /// Answers to jobs at or below this came from an older source and are dropped.
    art_floor: u64,
}

impl Worker {
    fn new(config: DiscordConfig, art: Arc<dyn ArtSource>, sender: Sender<Message>) -> Self {
        let (art_jobs, jobs) = mpsc::channel();
        let art_generation = Arc::new(AtomicU64::new(0));
        let generation = art_generation.clone();
        let source = art.clone();
        let art_seen = art.generation();
        thread::Builder::new()
            .name("navia-discord-art".into())
            .spawn(move || run_art_jobs(source, jobs, generation, sender))
            .expect("spawn art thread");
        let dirs = config.socket_dirs.clone().unwrap_or_else(default_socket_dirs);
        let backoff = config.reconnect_min;
        Self {
            config,
            dirs,
            connection: None,
            desired: None,
            sent: None,
            next_connect: Instant::now(),
            backoff,
            recent_sends: VecDeque::new(),
            wake_at: None,
            nonce: 0,
            art_cache: HashMap::new(),
            art_requested: None,
            art_generation,
            art_jobs,
            art,
            art_seen,
            art_floor: 0,
        }
    }

    fn run(mut self, receiver: Receiver<Message>) {
        loop {
            let message = match receiver.recv_timeout(self.tick()) {
                Ok(message) => Some(message),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return self.finish(),
            };
            let mut shutdown = false;
            for message in message.into_iter().chain(receiver.try_iter()) {
                match message {
                    Message::Desired(desired) => self.set_desired(desired),
                    Message::ArtFound { generation, key, album_key, url } => {
                        if generation > self.art_floor {
                            self.store_art(key, album_key, url);
                        }
                    }
                    Message::Shutdown => shutdown = true,
                }
            }
            if shutdown {
                return self.finish();
            }
            self.pump_incoming();
            self.reconcile();
        }
    }

    fn tick(&self) -> Duration {
        let tick = if self.connection.is_none() && self.desired.is_none() {
            IDLE_TICK
        } else {
            TICK
        };
        match self.wake_at {
            Some(at) => tick.min(at.saturating_duration_since(Instant::now())),
            None => tick,
        }
    }

    fn set_desired(&mut self, desired: Option<Presence>) {
        if desired.is_none() {
            // Nothing still queued may reach the network once the status is hidden, for example when private mode turns
            // on.
            self.art_generation.fetch_add(1, Ordering::SeqCst);
            self.art_requested = None;
        }
        self.desired = desired;
    }

    fn store_art(&mut self, key: String, album_key: Option<String>, url: Option<String>) {
        if self.art_cache.len() + 1 >= MAX_ART_CACHE {
            self.art_cache.clear();
        }
        if self.art_requested.as_deref() == Some(key.as_str()) {
            self.art_requested = None;
        }
        if let Some(album_key) = album_key {
            self.art_cache.insert(album_key, (url.clone(), Instant::now()));
        }
        self.art_cache.insert(key, (url, Instant::now()));
    }

    /// The cached answer for a song. A cover found for its album wins, else the song's own answer. Misses are per song
    /// because the song search may still find one.
    fn cached_cover(&self, track: &TrackInfo) -> Option<Option<String>> {
        if let Some(Some(url)) = self.cached_art(&album_key(track)) {
            return Some(Some(url.clone()));
        }
        self.cached_art(&song_key(track)).cloned()
    }

    /// A cached answer that hasn't expired. Covers keep for a day and misses for six hours.
    fn cached_art(&self, key: &str) -> Option<&Option<String>> {
        self.art_cache.get(key).filter(|(url, at)| at.elapsed() < if url.is_some() { ART_HIT_TTL } else { ART_MISS_TTL }).map(|(url, _)| url)
    }

    fn reconcile(&mut self) {
        self.wake_at = None;
        let Some(presence) = self.desired.clone() else {
            if self.sent.is_some() {
                self.send_clear();
            }
            return;
        };
        self.request_art(&presence.track);
        if !self.ensure_connected() {
            return;
        }

        let image = self
            .cached_cover(&presence.track)
            .flatten()
            .or_else(|| self.config.default_image.clone());
        let (activity, signature) = build(&presence, image.as_deref(), self.config.status_text);
        let started_at_ms = presence.started_at_ms();
        let unchanged = self.sent.as_ref().is_some_and(|sent| {
            sent.signature == signature
                && (sent.started_at_ms - started_at_ms).abs() <= SEEK_TOLERANCE_MS
        });
        if unchanged || !self.send_slot_free() {
            return;
        }
        if self.send_activity(activity) {
            self.sent = Some(SentActivity {
                signature,
                started_at_ms,
            });
        }
    }

    fn request_art(&mut self, track: &TrackInfo) {
        // A changed source, like a new public address, makes every earlier answer stale, including queued ones.
        let generation = self.art.generation();
        if generation != self.art_seen {
            self.art_seen = generation;
            self.art_cache.clear();
            self.art_requested = None;
            self.art_floor = self.art_generation.fetch_add(1, Ordering::SeqCst) + 1;
        }
        let key = song_key(track);
        if self.cached_cover(track).is_some() || self.art_requested.as_deref() == Some(key.as_str()) {
            return;
        }
        let generation = self.art_generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.art_requested = Some(key.clone());
        let _ = self.art_jobs.send(ArtJob {
            generation,
            key,
            album_key: album_key(track),
            track: track.clone(),
        });
    }

    fn ensure_connected(&mut self) -> bool {
        if self.connection.is_some() {
            return true;
        }
        let now = Instant::now();
        if now < self.next_connect {
            self.wake_at = Some(self.next_connect);
            return false;
        }
        match self.open_connection() {
            Some(connection) => {
                self.connection = Some(connection);
                self.backoff = self.config.reconnect_min;
                true
            }
            None => {
                self.next_connect = now + self.backoff;
                self.wake_at = Some(self.next_connect);
                self.backoff = (self.backoff * 2).min(self.config.reconnect_max);
                false
            }
        }
    }

    fn open_connection(&self) -> Option<Connection> {
        let mut connection = Connection::open(&self.dirs)?;
        let handshake = json!({"v": 1, "client_id": self.config.application_id});
        connection.send(OP_HANDSHAKE, &handshake).ok()?;
        let started = Instant::now();
        while started.elapsed() < HANDSHAKE_TIMEOUT {
            match connection.receive(HANDSHAKE_TIMEOUT.saturating_sub(started.elapsed())) {
                Ok(Some(Frame { opcode: OP_FRAME, body })) if body["evt"] == "READY" => {
                    return Some(connection);
                }
                Ok(Some(Frame { opcode: OP_CLOSE, body })) => {
                    log::warn!("Discord refused the connection: {}", body["message"]);
                    return None;
                }
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => return None,
            }
        }
        None
    }

    fn send_slot_free(&mut self) -> bool {
        let now = Instant::now();
        let window = self.config.rate_window;
        while self.recent_sends.front().is_some_and(|sent| now.duration_since(*sent) >= window) {
            self.recent_sends.pop_front();
        }
        if self.recent_sends.len() < self.config.rate_limit {
            return true;
        }
        self.wake_at = self.recent_sends.front().map(|sent| *sent + window);
        false
    }

    fn send_activity(&mut self, activity: Value) -> bool {
        let sent = self.send_command(activity);
        if sent {
            self.recent_sends.push_back(Instant::now());
        }
        sent
    }

    /// Clears are never held back by the rate limit, since hiding the status matters more.
    fn send_clear(&mut self) {
        self.sent = None;
        if self.connection.is_some() && self.send_command(Value::Null) {
            self.recent_sends.push_back(Instant::now());
        }
    }

    /// Returns false if the connection was lost.
    fn send_command(&mut self, activity: Value) -> bool {
        self.nonce += 1;
        let nonce = self.nonce.to_string();
        let command = json!({
            "cmd": "SET_ACTIVITY",
            "args": {"pid": std::process::id(), "activity": activity},
            "nonce": nonce,
        });
        let Some(connection) = self.connection.as_mut() else {
            return false;
        };
        if connection.send(OP_FRAME, &command).is_err() {
            self.connection_lost();
            return false;
        }
        self.pump_until_reply(&nonce)
    }

    fn pump_until_reply(&mut self, nonce: &str) -> bool {
        let started = Instant::now();
        loop {
            let remaining = REPLY_TIMEOUT.saturating_sub(started.elapsed());
            let Some(connection) = self.connection.as_mut() else {
                return false;
            };
            match connection.receive(remaining) {
                Ok(Some(frame)) => {
                    if !self.handle_frame(&frame) {
                        return false;
                    }
                    if frame.body["nonce"] == nonce {
                        return true;
                    }
                }
                Ok(None) => return true,
                Err(_) => {
                    self.connection_lost();
                    return false;
                }
            }
        }
    }

    fn pump_incoming(&mut self) {
        while let Some(connection) = self.connection.as_mut() {
            match connection.receive(Duration::ZERO) {
                Ok(Some(frame)) => {
                    self.handle_frame(&frame);
                }
                Ok(None) => return,
                Err(_) => self.connection_lost(),
            }
        }
    }

    /// Returns false if the frame ended the connection.
    fn handle_frame(&mut self, frame: &Frame) -> bool {
        match frame.opcode {
            OP_PING => {
                let pong_failed = match self.connection.as_mut() {
                    Some(connection) => connection.send(OP_PONG, &frame.body).is_err(),
                    None => false,
                };
                if pong_failed {
                    self.connection_lost();
                    return false;
                }
            }
            OP_CLOSE => {
                self.connection_lost();
                return false;
            }
            OP_FRAME if frame.body["evt"] == "ERROR" => {
                log::warn!("Discord rejected an update: {}", frame.body["data"]);
            }
            _ => {}
        }
        true
    }

    /// Discord drops a client's status when its socket closes, so there's nothing left to clear. The next reconcile
    /// reconnects and republishes.
    fn connection_lost(&mut self) {
        self.connection = None;
        self.sent = None;
        self.next_connect = Instant::now();
    }

    fn finish(mut self) {
        if self.sent.is_some() {
            self.send_clear();
        }
        if let Some(connection) = self.connection.as_mut() {
            let _ = connection.send(OP_CLOSE, &json!({}));
        }
    }
}

fn album_key(track: &TrackInfo) -> String {
    match (&track.release_mbid, &track.release_group_mbid) {
        (Some(id), _) | (None, Some(id)) => id.clone(),
        (None, None) => format!("{}\u{1f}{}", track.artist, track.album),
    }
}

fn song_key(track: &TrackInfo) -> String {
    format!("{}\u{1f}{}", album_key(track), track.title)
}

fn run_art_jobs(
    art: Arc<dyn ArtSource>,
    jobs: Receiver<ArtJob>,
    generation: Arc<AtomicU64>,
    results: Sender<Message>,
) {
    for job in jobs {
        if job.generation != generation.load(Ordering::SeqCst) {
            continue;
        }
        match art.find_art(&job.track) {
            Ok(found) => {
                let album_key = found.as_ref().filter(|art| art.album_wide).map(|_| job.album_key);
                let url = found.map(|art| art.url);
                let _ = results.send(Message::ArtFound { generation: job.generation, key: job.key, album_key, url });
            }
            Err(error) => log::warn!("album art lookup failed: {error}"),
        }
    }
}
