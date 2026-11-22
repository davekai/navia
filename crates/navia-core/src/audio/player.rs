//! The player a UI drives: a queue, transport and volume on top of the engine and an output.
//! Methods post a command and return at once. State comes back through [`Player::snapshot`]
//! and [`PlayerEvent`]s, delivered on a background thread.

use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::audio::decode::{Decoder, TrackSource};
use crate::audio::edges::{Analyzer, EdgeStore, LibraryEdgeStore};
use crate::audio::transition::TraitsSlot;
use crate::audio::engine::{AlbumSpot, EngineConfig, EngineControl, EngineEvent, EngineParts, OutputMode, TrackFeed, TrackOpener, TrackRequest, spawn_feed};
use crate::audio::live::LiveOpener;
use crate::audio::output::{CpalBackend, DeviceInfo, OutputBackend, OutputHandle};
use crate::audio::render::{NO_TRACK, RenderCore, RenderShared};
use crate::audio::stream::{CompleteSink, HttpFetcher, HttpStream, NetworkStatus, StreamConfig};
use crate::client::{Client, ReplayGain, StreamOptions};
use crate::error::{Error, Result};
use crate::quality::{Codec, Delivered, Leveling, Plan, Quality, QualityControl, Route, plan};
use crate::songcache::{CacheHit, NewEntry, SongCache, Spool};

const TICK: Duration = Duration::from_millis(15);
const POSITION_EVENT_EVERY: Duration = Duration::from_millis(250);
/// Close the device after this long paused or stopped.
const IDLE_CLOSE_AFTER: Duration = Duration::from_secs(30);
/// How long a wait for the server must stay over before it's reported as over.
const RECOVERY_HOLD: Duration = Duration::from_millis(500);
/// Past this far into a track, Previous restarts it instead of going back.
const PREVIOUS_RESTART_MS: u64 = 3_000;
const RING_FRAMES: usize = 240_000;
const DIED: &str = "Playback stopped unexpectedly";
/// The volume assumed for a device that won't report its own. Quiet (-24 dB on AirPlay), so
/// the first change can't jump it loud.
pub const ASSUMED_DEVICE_VOLUME: f32 = 0.2;

/// Lock a mutex even if a panic poisoned it, so one panicking listener can't take the player
/// down with it.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn panic_text(payload: &(dyn Any + Send)) -> String {
    match (payload.downcast_ref::<&str>(), payload.downcast_ref::<String>()) {
        (Some(s), _) => (*s).to_string(),
        (_, Some(s)) => s.clone(),
        _ => "unknown panic".to_string(),
    }
}

#[derive(Clone, Debug)]
pub struct QueueItem {
    pub id: String,
    pub duration_ms: Option<u64>,
    /// Queued by the listener (Play Next, Add to Queue) rather than part of the list playing.
    pub queued: bool,
}

impl QueueItem {
    /// A song from the list playing, not queued by the listener.
    pub fn new(id: String, duration_ms: Option<u64>) -> Self {
        Self { id, duration_ms, queued: false }
    }

    /// The same item, marked as queued by the listener or not.
    pub fn queued(mut self, queued: bool) -> Self {
        self.queued = queued;
        self
    }
}

/// A queued song that played and left the queue on its own. The app removes it from its copy
/// and calls [`Player::ack_left`] with `seq`. Until then, indexes in its commands are rebased
/// as if it were still there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LeftQueue {
    pub index: usize,
    pub seq: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Repeat {
    Off,
    All,
    One,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayerStatus {
    Stopped,
    Buffering,
    Playing,
    Paused,
}

/// How output sample rates are chosen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutputPolicy {
    /// Follow each track's rate when the device supports it, keeping playback bit-exact. A rate
    /// change between tracks reopens the device, which isn't gapless.
    FollowTrack,
    /// Stay at the device's default rate and resample. Always gapless.
    DeviceDefault,
    /// Stay at this rate and resample.
    Fixed(u32),
}

pub struct PlayerConfig {
    pub device: Option<String>,
    pub policy: OutputPolicy,
    pub ring_frames: usize,
    /// How long the output stays open while nothing is playing.
    pub idle_close_after: Duration,
}

impl Default for PlayerConfig {
    fn default() -> Self {
        Self { device: None, policy: OutputPolicy::FollowTrack, ring_frames: RING_FRAMES, idle_close_after: IDLE_CLOSE_AFTER }
    }
}

#[derive(Clone, Debug)]
pub enum PlayerEvent {
    /// A new entry is being heard. `left` is set when the previous entry was a queued song that
    /// has now left the queue. `index` already counts it gone.
    TrackChanged { index: usize, item: QueueItem, left: Option<LeftQueue> },
    StatusChanged(PlayerStatus),
    Position { ms: u64 },
    /// The server became unreachable, or came back. Playback holds its place meanwhile and
    /// resumes by itself.
    Connection { waiting: bool, message: Option<String> },
    /// Audio actually heard since the last such event, excluding pauses and seeks.
    Heard { since_last: Duration },
    /// A song, station or the player itself failed.
    Error(String),
    /// The output device couldn't start, so nothing can be heard. `device` is its name when known.
    OutputError { device: Option<String>, message: String },
    QueueChanged,
    QueueEnded,
    /// `device_rate`: what the device runs at, when the output can tell.
    OutputChanged { rate: u32, device_rate: Option<u32>, device: Option<String> },
    /// The output's own volume (AirPlay) as the slider should show it: read from the device,
    /// reported by it, or assumed. `None`: the output has no volume of its own, so the slider is
    /// the player's volume again. Only shown, never sent back.
    DeviceVolume(Option<DeviceVolume>),
    /// The player changed repeat itself. Next or Previous with Repeat one switches to Repeat all.
    RepeatChanged(Repeat),
    /// A song stopped being heard. Comes before the next song's `TrackChanged` and never for
    /// stations. `heard_ms` excludes pauses and skipped parts, and `following` means it was
    /// heard while listening along with someone.
    PlayEnded { item: QueueItem, heard_ms: u64, end: PlayEnd, following: bool },
}

/// The volume of an output that has its own, on the scale of [`Player::set_volume`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeviceVolume {
    pub level: f32,
    /// False when the device wouldn't say and `level` is [`ASSUMED_DEVICE_VOLUME`].
    pub known: bool,
}

/// How a play ended ([`PlayerEvent::PlayEnded`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayEnd {
    /// It played to its end, or to the end of the queue.
    Finished,
    /// Next moved past it.
    Skipped,
    /// Another song, list or station took its place, or playback was stopped or cleared.
    Replaced,
}

/// The song being heard, for [`PlayerEvent::PlayEnded`].
struct OpenPlay {
    item: QueueItem,
    /// `heard_frames` when it started.
    start_frames: u64,
    /// Heard while listening along with someone, at any point.
    following: bool,
}

/// A copy of the player's state at one moment.
#[derive(Clone, Debug)]
pub struct PlayerSnapshot {
    pub status: PlayerStatus,
    pub index: Option<usize>,
    pub item: Option<QueueItem>,
    pub position_ms: u64,
    /// The player's own volume, for the system's devices.
    pub volume: f32,
    pub muted: bool,
    /// Set while the output has its own volume. `set_volume` changes this instead of `volume`.
    pub device_volume: Option<DeviceVolume>,
    pub repeat: Repeat,
    pub crossfade_ms: u32,
    pub queue: Vec<QueueItem>,
    pub output_rate: u32,
    pub device: Option<String>,
    /// Frames of silence caused by the decoder or network not keeping up.
    pub underrun_frames: u64,
    pub rebuffer_frames: u64,
    pub xruns: u64,
    /// What the audible song is being delivered as, once it's open.
    pub delivered: Option<Delivered>,
    /// A live stream is playing instead of the queue, with no seek and no end.
    pub live: bool,
    /// True while playback is waiting for the server to come back.
    pub waiting_for_server: bool,
    /// Set when a player thread died. Nothing plays until the app builds a new player.
    pub error: Option<String>,
    /// What the queue plays from, for the "Next from" heading.
    pub source: Option<crate::queue_order::QueueSource>,
}

/// Makes the opener for a queue item.
pub trait TrackResolver: Send + Sync {
    fn opener(&self, item: &QueueItem) -> Arc<dyn TrackOpener>;

    /// Lets the player report and hurry waits for the server. None when there's no server.
    fn network_status(&self) -> Option<Arc<NetworkStatus>> {
        None
    }
}

/// A resolver for files on disk, keyed by id. Mainly for tests and tools.
pub struct FileResolver(pub std::collections::HashMap<String, std::path::PathBuf>);

impl TrackResolver for FileResolver {
    fn opener(&self, item: &QueueItem) -> Arc<dyn TrackOpener> {
        Arc::new(FileItemOpener(self.0.get(&item.id).cloned()))
    }
}

struct FileItemOpener(Option<std::path::PathBuf>);

impl TrackOpener for FileItemOpener {
    fn open(&self, start_ms: u64) -> Result<(Decoder, u64)> {
        let path = self.0.as_ref().ok_or_else(|| Error::Audio("no such file".to_string()))?;
        let mut decoder = Decoder::open(path)?;
        if start_ms > 0 {
            decoder.seek_ms(start_ms)?;
        }
        Ok((decoder, start_ms))
    }
}

/// Streams from the server, the original file first and a transcode for anything the decoder
/// can't read.
pub struct ServerResolver {
    client: Client,
    fetcher: HttpFetcher,
    config: StreamConfig,
    /// Formats to try for fallback transcodes, best first.
    fallback_formats: Vec<String>,
    downloads: Option<Arc<crate::downloads::DownloadManager>>,
    quality: Option<Arc<QualityControl>>,
    /// Checks whether a cached file is still current. Short timeouts, so an unreachable server
    /// falls back to the cached copy quickly.
    probe: reqwest::blocking::Client,
}

impl ServerResolver {
    pub fn new(client: Client) -> Result<Self> {
        let probe = reqwest::blocking::Client::builder().user_agent(crate::client::user_agent()).connect_timeout(Duration::from_millis(1500)).timeout(Duration::from_secs(3)).build()?;
        let fetcher = HttpFetcher::new()?.with_offline(client.offline_switch()).with_form_post(client.form_post());
        Ok(Self { client, fetcher, config: StreamConfig::default(), fallback_formats: vec!["mp3".to_string(), "opus".to_string()], downloads: None, quality: None, probe })
    }

    /// Play downloaded songs from disk and apply the downloads' quality settings.
    pub fn with_downloads(mut self, downloads: Arc<crate::downloads::DownloadManager>) -> Self {
        self.quality = Some(downloads.quality_control());
        self.downloads = Some(downloads);
        self
    }

    pub fn with_quality(mut self, quality: Arc<QualityControl>) -> Self {
        self.quality = Some(quality);
        self
    }

    pub fn with_stream_config(mut self, config: StreamConfig) -> Self {
        self.config = config;
        self
    }

    pub fn with_fallback_formats(mut self, formats: Vec<String>) -> Self {
        self.fallback_formats = formats;
        self
    }
}

impl TrackResolver for ServerResolver {
    fn opener(&self, item: &QueueItem) -> Arc<dyn TrackOpener> {
        Arc::new(ServerOpener {
            client: self.client.clone(),
            fetcher: self.fetcher.clone(),
            config: self.config,
            formats: self.fallback_formats.clone(),
            id: item.id.clone(),
            transcode: Mutex::new(None),
            downloads: self.downloads.clone(),
            quality: self.quality.clone(),
            token: Mutex::new(None),
            delivered: Mutex::new(None),
            chosen: Mutex::new(None),
            replay_gain: std::sync::OnceLock::new(),
            probe: self.probe.clone(),
            checked: Mutex::new(Vec::new()),
        })
    }

    fn network_status(&self) -> Option<Arc<NetworkStatus>> {
        Some(self.fetcher.status())
    }
}

struct ServerOpener {
    client: Client,
    fetcher: HttpFetcher,
    config: StreamConfig,
    formats: Vec<String>,
    id: String,
    /// A fallback that worked, kept for later seeks.
    transcode: Mutex<Option<String>>,
    downloads: Option<Arc<crate::downloads::DownloadManager>>,
    quality: Option<Arc<QualityControl>>,
    /// The server's signed decision for this song, reused for seeks.
    token: Mutex<Option<String>>,
    delivered: Mutex<Option<Delivered>>,
    /// The quality this track started at, so seeks stay on it if the setting or connection
    /// changes.
    chosen: Mutex<Option<Quality>>,
    /// The song's ReplayGain data, looked up once.
    replay_gain: std::sync::OnceLock<Option<ReplayGain>>,
    probe: reqwest::blocking::Client,
    /// Cached copies confirmed current for this play, so seeks don't ask again.
    checked: Mutex<Vec<Quality>>,
}

/// What the server says about the original file right now.
struct FileStamp {
    last_modified: Option<String>,
    size: Option<u64>,
}

enum Probe {
    Current(FileStamp),
    /// The server says the song doesn't exist.
    Gone,
    /// No useful answer, such as no connection, a timeout or a server error.
    Unknown,
}

/// Ask for one byte of the original file, for its `Last-Modified` and length.
fn probe_original(probe: &reqwest::blocking::Client, form: &crate::client::FormPost, url: &str) -> Probe {
    use reqwest::header::{CONTENT_RANGE, CONTENT_TYPE, LAST_MODIFIED, RANGE};
    let Ok(resp) = form.request_blocking(probe, url).header(RANGE, "bytes=0-0").send() else { return Probe::Unknown };
    let status = resp.status().as_u16();
    let header = |name| resp.headers().get(name).and_then(|v: &reqwest::header::HeaderValue| v.to_str().ok()).map(str::to_string);
    let content_type = header(CONTENT_TYPE).unwrap_or_default();
    match status {
        200 | 206 if content_type.contains("json") || content_type.contains("xml") => Probe::Gone,
        206 => Probe::Current(FileStamp { last_modified: header(LAST_MODIFIED), size: header(CONTENT_RANGE).and_then(|r| r.rsplit('/').next()?.trim().parse().ok()) }),
        // Dropping the response stops the body, so a server that ignores ranges costs nothing.
        200 => Probe::Current(FileStamp { last_modified: header(LAST_MODIFIED), size: resp.content_length() }),
        404 | 410 => Probe::Gone,
        _ => Probe::Unknown,
    }
}

/// Keeps a stream in the song cache once it's whole.
struct CacheSink {
    cache: Arc<SongCache>,
    control: Arc<QualityControl>,
    entry: NewEntry,
    /// Where to ask for the original's stamp, for a transcode, which has none of its own.
    stamp_from: Option<(reqwest::blocking::Client, crate::client::FormPost, String)>,
}

impl CompleteSink for CacheSink {
    fn fill_gaps(&self) -> bool {
        // Fetching what a seek skipped costs data, so not on a metered connection.
        self.cache.enabled() && !self.control.is_metered()
    }

    fn complete(&self, spool: &dyn Spool, len: u64, last_modified: Option<&str>) {
        if !self.cache.enabled() {
            return;
        }
        let stamp = match &self.stamp_from {
            Some((probe, form, url)) => match probe_original(probe, form, url) {
                Probe::Current(stamp) => stamp.last_modified,
                _ => None,
            },
            None => last_modified.map(str::to_string),
        };
        let outcome = self.cache.store(&self.entry, stamp, spool, len);
        tracing::debug!("song cache: {} as {}: {outcome:?}", self.entry.song_id, self.entry.quality.format_id());
    }
}

impl ServerOpener {
    fn song_cache(&self) -> Option<Arc<SongCache>> {
        self.quality.as_ref().and_then(|q| q.cache().cloned())
    }

    fn raw_url(&self) -> Result<String> {
        self.client.stream_url(&self.id, &StreamOptions { format: Some("raw".to_string()), ..Default::default() })
    }

    /// A sink that keeps this song at `quality` once its stream is whole.
    fn cache_sink(&self, quality: Quality) -> Option<Arc<dyn CompleteSink>> {
        let control = self.quality.clone()?;
        let cache = control.cache().cloned().filter(|c| c.enabled())?;
        let source = control.source(&self.id);
        let entry = NewEntry {
            song_id: self.id.clone(),
            quality,
            source_size: source.as_ref().and_then(|s| s.size).unwrap_or(0),
            expected_ms: source.as_ref().and_then(|s| s.duration_s).map(|d| u64::from(d) * 1000),
        };
        let stamp_from = match quality {
            Quality::Original => None,
            Quality::Transcode { .. } => Some((self.probe.clone(), self.client.form_post(), self.raw_url().ok()?)),
        };
        Some(Arc::new(CacheSink { cache, control, entry, stamp_from }))
    }

    /// Whether a cached copy is still the server's current file. Asked once per play. If the
    /// server can't be reached, the cached copy plays.
    fn still_current(&self, hit: &CacheHit) -> bool {
        if self.checked.lock().expect("checked").contains(&hit.quality) || self.fetcher.status().waiting() {
            return true;
        }
        let Ok(url) = self.raw_url() else { return true };
        let current = match probe_original(&self.probe, &self.client.form_post(), &url) {
            Probe::Current(stamp) => {
                let changed_stamp = matches!((&hit.stamp, &stamp.last_modified), (Some(a), Some(b)) if a != b);
                let changed_size = match (hit.quality, stamp.size) {
                    (Quality::Original, Some(size)) => size != hit.bytes,
                    (_, Some(size)) => hit.source_size > 0 && size != hit.source_size,
                    _ => false,
                };
                !(changed_stamp || changed_size)
            }
            Probe::Gone => false,
            Probe::Unknown => true,
        };
        if current {
            self.checked.lock().expect("checked").push(hit.quality);
        } else {
            tracing::info!("a cached song changed on the server; streaming it again");
        }
        current
    }

    /// Open a cached copy. A stale or broken copy is dropped and `None` sends the caller to the
    /// server.
    fn open_cached(&self, cache: &SongCache, hit: CacheHit, start_ms: u64, check: bool) -> Option<(Decoder, u64)> {
        if check && !self.still_current(&hit) {
            cache.remove(&self.id, hit.quality);
            return None;
        }
        let opened = (|| -> Result<Decoder> {
            // A transcode was cached from a stream with no length, so read it back the same way.
            let mut decoder = match hit.quality {
                Quality::Original => Decoder::open(&hit.path)?,
                Quality::Transcode { .. } => Decoder::open_spooled(&hit.path)?,
            };
            if let Quality::Transcode { codec, .. } = hit.quality {
                decoder.trim_head(codec.encoder_delay());
            }
            if start_ms > 0 {
                decoder.seek_ms(start_ms)?;
            }
            Ok(decoder)
        })();
        match opened {
            Ok(decoder) => {
                cache.mark_played(&hit);
                let quality = match hit.quality {
                    Quality::Original => None,
                    Quality::Transcode { codec, kbps } => Some((codec, kbps)),
                };
                self.set_delivered(Delivered::Cached { quality });
                Some((decoder, start_ms))
            }
            Err(e) => {
                tracing::warn!("a cached song wouldn't play, so it was dropped: {e}");
                cache.remove(&self.id, hit.quality);
                None
            }
        }
    }

    fn open_url(&self, url: String, start_ms: u64, seek_in_stream: bool) -> Result<(Decoder, u64)> {
        self.open_url_keeping(url, start_ms, seek_in_stream, None)
    }

    fn open_url_keeping(&self, url: String, start_ms: u64, seek_in_stream: bool, keep: Option<Arc<dyn CompleteSink>>) -> Result<(Decoder, u64)> {
        let stream = HttpStream::open(&self.fetcher, url, self.config)?;
        if let Some(sink) = keep {
            stream.keep_when_complete(sink);
        }
        let result = (|| {
            let mut decoder = Decoder::open_source(&TrackSource::Stream(stream.clone()))?;
            if start_ms > 0 && seek_in_stream {
                decoder.seek_ms(start_ms)?;
            }
            Ok((decoder, start_ms))
        })();
        // A failure while the server was going away is the server's, not this file's.
        result.map_err(|e: Error| if stream.lost() { Error::Network(e.to_string()) } else { e }).inspect(|_| stream.finish_opening())
    }

    fn set_delivered(&self, delivered: Delivered) {
        *self.delivered.lock().expect("delivered") = Some(delivered);
    }

    /// Open this song at the chosen format and rate. `Ok(None)` means the original is fine or
    /// needs no transcoding.
    fn open_quality(&self, quality: &QualityControl, codec: Codec, kbps: u32, start_ms: u64) -> Result<Option<(Decoder, u64)>> {
        let secs = (start_ms / 1000) as u32;
        let url = match quality.route(&self.id, codec, kbps, secs, &self.token)? {
            Route::Original => return Ok(None),
            Route::Unavailable(reason) => return Err(Error::Audio(reason)),
            Route::Transcode { url } => url,
        };
        let opened = (|| {
            let stream = HttpStream::open(&self.fetcher, url.clone(), self.config)?;
            if !codec.content_type_matches(stream.content_type()) {
                return Err(Error::Audio(format!("the server sent {} instead of {}", stream.content_type(), codec.label())));
            }
            let result = Decoder::open_source(&TrackSource::Stream(stream.clone()));
            let mut decoder = result.map_err(|e: Error| if stream.lost() { Error::Network(e.to_string()) } else { e })?;
            stream.finish_opening();
            // The server starts on a whole second, so trim the rest of the way to the target.
            decoder.trim_head(codec.encoder_delay());
            decoder.skip_ms(start_ms % 1000);
            // Only a transcode from the very start is the whole song.
            if secs == 0 && let Some(sink) = self.cache_sink(Quality::Transcode { codec, kbps }) {
                stream.keep_when_complete(sink);
            }
            Ok((decoder, start_ms))
        })();
        match opened {
            Ok(opened) => {
                self.set_delivered(Delivered::Transcoded { codec, kbps });
                Ok(Some(opened))
            }
            // A stale decision because the file changed. Ask again once.
            Err(Error::HttpStatus { status: 410, .. }) if self.token.lock().expect("token").take().is_some() => self.open_quality(quality, codec, kbps, start_ms),
            Err(e) => Err(e),
        }
    }

    fn open_transcode(&self, format: &str, start_ms: u64) -> Result<(Decoder, u64)> {
        let secs = start_ms / 1000;
        let opts = StreamOptions { format: Some(format.to_string()), time_offset: (secs > 0).then_some(secs as u32), ..Default::default() };
        let url = self.client.stream_url(&self.id, &opts)?;
        let (decoder, _) = self.open_url(url, 0, false)?;
        Ok((decoder, secs * 1000))
    }
}

impl TrackOpener for ServerOpener {
    fn gain(&self) -> f32 {
        let Some(quality) = self.quality.as_ref() else { return 1.0 };
        let mode = quality.leveling();
        if mode == Leveling::Off {
            return 1.0;
        }
        match self.replay_gain.get_or_init(|| quality.replay_gain(&self.id)) {
            Some(data) => mode.linear_gain(data),
            None => 1.0,
        }
    }

    fn delivered(&self) -> Option<Delivered> {
        self.delivered.lock().expect("delivered").clone()
    }

    fn open(&self, start_ms: u64) -> Result<(Decoder, u64)> {
        let opened = self.open_from_server(start_ms);
        // The server is gone, so any cached copy of this song will do.
        if let Err(e) = &opened
            && e.is_outage()
            && let Some(cache) = self.song_cache()
            && let Some(hit) = cache.lookup_any(&self.id)
            && let Some(opened) = self.open_cached(&cache, hit, start_ms, false)
        {
            return Ok(opened);
        }
        opened
    }
}

impl ServerOpener {
    fn open_from_server(&self, start_ms: u64) -> Result<(Decoder, u64)> {
        if let Some(path) = self.downloads.as_ref().and_then(|d| d.local_path(&self.id)) {
            let quality = self.downloads.as_ref().and_then(|d| d.delivered_quality(&self.id));
            // A cached transcode plays as a stream, with the same length and trim.
            let mut decoder = match quality {
                None => Decoder::open(&path)?,
                Some((codec, _)) => {
                    let mut decoder = Decoder::open_spooled(&path)?;
                    decoder.trim_head(codec.encoder_delay());
                    decoder
                }
            };
            if start_ms > 0 {
                decoder.seek_ms(start_ms)?;
            }
            self.set_delivered(Delivered::Downloaded { quality });
            return Ok((decoder, start_ms));
        }
        // The chosen format, when it applies to this file. Anything but losing the server falls
        // through to the original, so the music keeps playing.
        let chosen = self.quality.as_ref().map(|control| {
            let mut chosen = self.chosen.lock().expect("chosen");
            let quality = if start_ms == 0 { control.streaming_now() } else { chosen.unwrap_or_else(|| control.streaming_now()) };
            *chosen = Some(quality);
            quality
        });
        let wanted = match (self.quality.as_ref(), chosen) {
            (Some(control), Some(quality)) => match plan(quality, control.source(&self.id).as_ref()) {
                Plan::Original => Quality::Original,
                Plan::Transcode { codec, kbps } => Quality::Transcode { codec, kbps },
            },
            _ => Quality::Original,
        };
        // Prefer the cache at the wanted quality, or the original, which costs no data either.
        // While the server is out, any cached copy.
        if let Some(cache) = self.song_cache() {
            let size = self.quality.as_ref().and_then(|c| c.source(&self.id)).and_then(|s| s.size).unwrap_or(0);
            let offline = self.fetcher.status().waiting() || self.client.is_offline();
            let hit = cache
                .lookup(&self.id, wanted, size)
                .or_else(|| (wanted != Quality::Original).then(|| cache.lookup(&self.id, Quality::Original, size)).flatten())
                .or_else(|| offline.then(|| cache.lookup_any(&self.id)).flatten());
            if let Some(hit) = hit
                && let Some(opened) = self.open_cached(&cache, hit, start_ms, true)
            {
                return Ok(opened);
            }
        }
        if let (Some(control), Quality::Transcode { codec, kbps }) = (self.quality.clone(), wanted) {
            match self.open_quality(&control, codec, kbps, start_ms) {
                Ok(Some(opened)) => return Ok(opened),
                Ok(None) => {}
                Err(e) if e.is_outage() || matches!(e, Error::Cancelled) => return Err(e),
                Err(e) => {
                    tracing::warn!("can't play this song as {} {kbps}: {e}", codec.label());
                    self.set_delivered(Delivered::Fallback { wanted: codec, kbps, reason: e.to_string() });
                }
            }
        }
        if let Some(format) = self.transcode.lock().expect("transcode").clone() {
            return self.open_transcode(&format, start_ms);
        }
        // The original always streams from its first byte, so any play of it can be cached.
        let original = self.open_url_keeping(self.raw_url()?, start_ms, true, self.cache_sink(Quality::Original));
        let err = match original {
            Ok(opened) => {
                let mut delivered = self.delivered.lock().expect("delivered");
                if !matches!(*delivered, Some(Delivered::Fallback { .. })) {
                    *delivered = Some(Delivered::Original);
                }
                return Ok(opened);
            }
            Err(e) => e,
        };
        // Network and auth failures won't be fixed by a transcode.
        if matches!(err, Error::Network(_) | Error::HttpStatus { .. } | Error::Subsonic { .. }) {
            return Err(err);
        }
        tracing::info!("falling back to a server transcode: {err}");
        for format in &self.formats {
            if let Ok(opened) = self.open_transcode(format, start_ms) {
                *self.transcode.lock().expect("transcode") = Some(format.clone());
                return Ok(opened);
            }
        }
        Err(err)
    }
}

#[derive(Clone)]
struct Entry {
    token: usize,
    item: QueueItem,
    opener: Arc<dyn TrackOpener>,
    /// Its album place, looked up the first time a request needs it.
    spot: Arc<OnceLock<Option<AlbumSpot>>>,
    /// its measured edges for smart transitions, filled in the background.
    traits: TraitsSlot,
}

/// Finds a song's place on its album by song id, for Smart crossfade.
pub type AlbumLookup = Arc<dyn Fn(&str) -> Option<AlbumSpot> + Send + Sync>;

pub fn library_album_lookup(library: crate::library::Library) -> AlbumLookup {
    Arc::new(move |id: &str| {
        let song = library.song(id).ok().flatten()?;
        Some(AlbumSpot { album_id: song.album_id?, disc: song.disc_number, track: song.track })
    })
}

/// an [`EdgeStore`] in the local library.
pub fn library_edge_store(library: crate::library::Library) -> Arc<dyn EdgeStore> {
    Arc::new(LibraryEdgeStore::new(library))
}

/// A song's name for the system's lists of what's playing ("Title – Artist"), by song id.
pub type TitleLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

pub fn library_title_lookup(library: crate::library::Library) -> TitleLookup {
    Arc::new(move |id: &str| {
        let song = library.song(id).ok().flatten()?;
        Some(match song.artist.as_deref().filter(|a| !a.is_empty()) {
            Some(artist) => format!("{} \u{2013} {artist}", song.title),
            None => song.title,
        })
    })
}

struct Queue {
    entries: Vec<Entry>,
    /// The entry the decoder is on, which runs ahead of what is audible.
    decode_index: Option<usize>,
    audible_token: Option<usize>,
    repeat: Repeat,
    next_token: usize,
    resolver: Arc<dyn TrackResolver>,
    /// A live stream holds the queue, and nothing follows it whatever repeat says.
    live: bool,
    /// Album places for Smart crossfade. Without it every join is unknown.
    albums: Option<AlbumLookup>,
    /// Song names for the output's stream name.
    titles: Option<TitleLookup>,
    /// measures songs for smart transitions while they're on.
    analyzer: Option<Analyzer>,
    smart_transitions: bool,
    /// the queue ends after this entry instead of going on ([`Player::set_hold_after_current`]).
    hold: Option<usize>,
}

impl Queue {
    fn new(resolver: Arc<dyn TrackResolver>) -> Self {
        Self { entries: Vec::new(), decode_index: None, audible_token: None, repeat: Repeat::Off, next_token: 0, resolver, live: false, albums: None, titles: None, analyzer: None, smart_transitions: false, hold: None }
    }

    fn make_entry(&mut self, item: QueueItem) -> Entry {
        let token = self.next_token;
        self.next_token += 1;
        let opener = self.resolver.opener(&item);
        Entry { token, item, opener, spot: Arc::default(), traits: Arc::default() }
    }

    fn request(&self, index: usize, start_ms: u64) -> Option<TrackRequest> {
        let e = self.entries.get(index)?;
        let album = self.albums.as_ref().and_then(|lookup| e.spot.get_or_init(|| lookup(&e.item.id)).clone());
        self.measure_from(index);
        Some(TrackRequest { token: e.token, opener: e.opener.clone(), start_ms, duration_ms: e.item.duration_ms, album, traits: Some(e.traits.clone()) })
    }

    /// with smart transitions on, measure the entry at `index` and the one after it, in the
    /// background, so both are ready by the join between them.
    fn measure_from(&self, index: usize) {
        let Some(analyzer) = self.analyzer.as_ref().filter(|_| self.smart_transitions && !self.live) else { return };
        for i in [Some(index), self.after(Some(index))].into_iter().flatten() {
            if let Some(e) = self.entries.get(i) {
                analyzer.ask(&e.item.id, || self.resolver.opener(&e.item), e.item.duration_ms, &e.traits);
            }
        }
    }

    fn index_of(&self, token: usize) -> Option<usize> {
        self.entries.iter().position(|e| e.token == token)
    }

    fn after(&self, index: Option<usize>) -> Option<usize> {
        let index = index?;
        if self.live {
            return None;
        }
        if self.repeat == Repeat::One {
            return Some(index);
        }
        if self.hold.is_some() && self.entries.get(index).map(|e| e.token) == self.hold {
            return None;
        }
        self.skip_from(index)
    }

    /// Where Next goes from `index`. Repeat one still moves on, and repeat wraps at the end.
    /// start at the end when repeat is on.
    fn skip_from(&self, index: usize) -> Option<usize> {
        if self.live {
            return None;
        }
        if index + 1 < self.entries.len() {
            Some(index + 1)
        } else if self.repeat != Repeat::Off && !self.entries.is_empty() {
            Some(0)
        } else {
            None
        }
    }

    /// The entry with `token` stopped being heard. A queued song leaves the queue and its index
    /// is returned. Other entries stay, for Previous.
    fn leave_if_queued(&mut self, token: usize) -> Option<usize> {
        let index = self.index_of(token)?;
        if !self.entries[index].item.queued || self.live {
            return None;
        }
        // The engine follows entries by token, so only positions shift.
        let decoding = self.decode_index.and_then(|d| self.entries.get(d)).map(|e| e.token).filter(|&t| t != token);
        self.entries.remove(index);
        self.decode_index = decoding.or(self.audible_token).and_then(|t| self.index_of(t));
        Some(index)
    }

    /// Where Previous goes from `index`. With repeat, the first wraps to the last.
    fn back_from(&self, index: usize) -> Option<usize> {
        if index > 0 {
            Some(index - 1)
        } else if self.repeat != Repeat::Off && self.entries.len() > 1 {
            Some(self.entries.len() - 1)
        } else {
            None
        }
    }
}

struct QueueFeed(Arc<Mutex<Queue>>);

impl TrackFeed for QueueFeed {
    fn peek(&mut self) -> Option<TrackRequest> {
        let q = self.0.lock().expect("queue");
        let next = q.after(q.decode_index)?;
        q.request(next, 0)
    }

    fn advance(&mut self) -> Option<TrackRequest> {
        let mut q = self.0.lock().expect("queue");
        let next = q.after(q.decode_index);
        q.decode_index = next;
        q.request(next?, 0)
    }

    fn following(&mut self, token: usize) -> Option<Option<usize>> {
        let q = self.0.lock().expect("queue");
        let index = q.index_of(token)?;
        Some(q.after(Some(index)).map(|i| q.entries[i].token))
    }

    fn rewind_to(&mut self, token: usize) -> bool {
        let mut q = self.0.lock().expect("queue");
        let Some(index) = q.index_of(token) else { return false };
        q.decode_index = Some(index);
        true
    }
}

enum Command {
    SetQueue { items: Vec<QueueItem>, start_index: usize, start_ms: u64, paused: bool },
    Append(Vec<QueueItem>),
    InsertNext(Vec<QueueItem>),
    ReplaceUpcoming(Vec<QueueItem>),
    RemoveItems(Vec<usize>),
    Move { from: usize, to: usize },
    MoveItems { positions: Vec<usize>, before: usize },
    SetQueued { positions: Vec<usize>, queued: bool },
    ClearQueued,
    ShuffleQueued(u64),
    ClearList { keep_tail: usize },
    InsertAt { index: usize, items: Vec<QueueItem> },
    AckLeft(u64),
    Clear,
    JumpTo(usize),
    Play,
    Pause,
    Toggle,
    Seek(u64),
    Next,
    Previous,
    Stop,
    SetVolume(f32),
    SetMuted(bool),
    SetRepeat(Repeat),
    SetCrossfadeMs(u32),
    SetSmartCrossfade(bool),
    SetSmartTransitions(bool),
    SetFollowing(bool),
    HoldAfterCurrent(bool),
    SetDevice(Option<String>),
    SetPolicy(OutputPolicy),
    RetryNow,
    PlayLive { item: QueueItem, opener: Arc<LiveOpener> },
    Sync(Sender<()>),
    Shutdown,
    /// Kills the engine thread, to test that its death gets reported.
    #[cfg(test)]
    CrashEngineForTest,
}

type Listener = Box<dyn Fn(&PlayerEvent) + Send>;

/// The UI's handle to playback. Cheap to share. Commands go to a background thread.
pub struct Player {
    tx: Sender<Command>,
    net: Option<Arc<NetworkStatus>>,
    queue: Arc<Mutex<Queue>>,
    render: Arc<RenderShared>,
    state: Arc<Mutex<ControlState>>,
    listeners: Arc<Mutex<Vec<Listener>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
    live: Arc<Mutex<Option<Arc<LiveOpener>>>>,
    source: Mutex<Option<crate::queue_order::QueueSource>>,
}

#[derive(Clone)]
struct ControlState {
    status: PlayerStatus,
    volume: f32,
    muted: bool,
    device_volume: Option<DeviceVolume>,
    crossfade_ms: u32,
    device: Option<String>,
    output_rate: u32,
    xruns: u64,
    waiting: bool,
    error: Option<String>,
}

impl Player {
    /// A player using the system's audio devices.
    pub fn new(resolver: Arc<dyn TrackResolver>, config: PlayerConfig) -> Result<Self> {
        Self::with_backend(resolver, Box::new(CpalBackend::default()), config)
    }

    /// A player using the given output backend, for tests.
    pub fn with_backend(resolver: Arc<dyn TrackResolver>, backend: Box<dyn OutputBackend>, config: PlayerConfig) -> Result<Self> {
        let net = resolver.network_status();
        let queue = Arc::new(Mutex::new(Queue::new(resolver)));
        let caps = backend.caps(config.device.as_deref())?;
        let mut engine = EngineConfig::new(mode_for(&config.policy, &caps));
        engine.ring_frames = config.ring_frames;
        let parts = spawn_feed(Box::new(QueueFeed(queue.clone())), engine, None, false);
        let render = parts.shared.clone();
        let (tx, rx) = channel();
        let state = Arc::new(Mutex::new(ControlState { status: PlayerStatus::Stopped, volume: 1.0, muted: false, device_volume: None, crossfade_ms: 0, device: config.device.clone(), output_rate: 0, xruns: 0, waiting: false, error: None }));
        let listeners: Arc<Mutex<Vec<Listener>>> = Arc::new(Mutex::new(Vec::new()));
        let (ctl_queue, ctl_shared, ctl_state, ctl_listeners, policy) = (queue.clone(), render.clone(), state.clone(), listeners.clone(), config.policy);
        let live: Arc<Mutex<Option<Arc<LiveOpener>>>> = Arc::new(Mutex::new(None));
        let ctl_live = live.clone();
        let EngineParts { core, events, control: engine, worker, .. } = parts;
        let ctl_net = net.clone();
        let idle_close_after = config.idle_close_after;
        let thread = thread::Builder::new()
            .name("navia-player".into())
            .spawn(move || {
                // Built on this thread because device streams aren't Send on every platform.
                let mut control = Control {
                    queue: ctl_queue,
                    engine,
                    events,
                    worker: Some(worker),
                    core: Arc::new(Mutex::new(core)),
                    shared: ctl_shared,
                    backend,
                    handle: None,
                    policy,
                    state: ctl_state,
                    listeners: ctl_listeners,
                    last_generation: 0,
                    audible: None,
                    audible_run: 0,
                    heard_mark: 0,
                    last_position_event: Instant::now(),
                    idle_since: None,
                    drain_started: None,
                    live: ctl_live,
                    live_paused: false,
                    net: ctl_net,
                    idle_close_after,
                    engine_waiting: false,
                    waiting: false,
                    waiting_message: None,
                    stalled: false,
                    last_status_sent: PlayerStatus::Stopped,
                    recovered_at: None,
                    loaded: None,
                    engine_failed: None,
                    following: false,
                    play: None,
                    left_seq: 0,
                    unacked: Vec::new(),
                    device_report: None,
                };
                // Nothing else notices if this thread dies, so catch and report it.
                if let Err(payload) = catch_unwind(AssertUnwindSafe(|| control.run(rx))) {
                    control.thread_died(&panic_text(payload.as_ref()));
                }
            })
            .map_err(Error::Io)?;
        Ok(Self { tx, net, queue, render, state, listeners, thread: Mutex::new(Some(thread)), live, source: Mutex::new(None) })
    }

    pub fn add_listener(&self, listener: impl Fn(&PlayerEvent) + Send + 'static) {
        lock(&self.listeners).push(Box::new(listener));
    }

    fn send(&self, cmd: Command) {
        let report = !matches!(cmd, Command::Shutdown | Command::Sync(_));
        if self.tx.send(cmd).is_err() && report {
            // The player thread is gone, so report that the command can't arrive.
            let message = {
                let mut state = lock(&self.state);
                state.status = PlayerStatus::Stopped;
                state.error.clone().unwrap_or_else(|| DIED.to_string())
            };
            self.send_error(message);
        }
    }

    /// Replace the queue and start at `start_index`. With `paused`, the track loads silent,
    /// for restoring a session.
    pub fn set_queue(&self, items: Vec<QueueItem>, start_index: usize, start_ms: u64, paused: bool) {
        // Set the intent now so a snapshot taken right after isn't stale.
        if !items.is_empty() {
            self.state.lock().expect("player state").status = if paused { PlayerStatus::Paused } else { PlayerStatus::Buffering };
        }
        self.send(Command::SetQueue { items, start_index, start_ms, paused });
    }

    pub fn append(&self, items: Vec<QueueItem>) {
        self.send(Command::Append(items));
    }

    pub fn insert_next(&self, items: Vec<QueueItem>) {
        self.send(Command::InsertNext(items));
    }

    pub fn remove(&self, index: usize) {
        self.send(Command::RemoveItems(vec![index]));
    }

    /// Remove the entries at `positions` in one edit, keeping the join gapless. Removing the
    /// song being heard moves on to what took its place.
    pub fn remove_items(&self, positions: Vec<usize>) {
        self.send(Command::RemoveItems(positions));
    }

    /// Replace everything after the track being heard, for shuffle.
    pub fn replace_upcoming(&self, items: Vec<QueueItem>) {
        self.send(Command::ReplaceUpcoming(items));
    }

    pub fn move_item(&self, from: usize, to: usize) {
        self.send(Command::Move { from, to });
    }

    /// Move the entries at `positions` before the entry now at `before`, keeping their order.
    /// See [`crate::queue_order::move_items`].
    pub fn move_items(&self, positions: Vec<usize>, before: usize) {
        self.send(Command::MoveItems { positions, before });
    }

    /// Mark entries as queued by the listener or as part of the list.
    pub fn set_queued(&self, positions: Vec<usize>, queued: bool) {
        self.send(Command::SetQueued { positions, queued });
    }

    /// Clear Queue. Removes the queued songs waiting after the one being heard.
    pub fn clear_queued(&self) {
        self.send(Command::ClearQueued);
    }

    /// Shuffle Queue. Shuffles the waiting queued songs once with `seed`, as
    /// [`crate::queue_order::shuffled_queued_order`] does.
    pub fn shuffle_queued(&self, seed: u64) {
        self.send(Command::ShuffleQueued(seed));
    }

    /// Remove the rest of the list after the queued songs, keeping the last `keep_tail` entries
    /// that Autoplay added.
    pub fn clear_list(&self, keep_tail: usize) {
        self.send(Command::ClearList { keep_tail });
    }

    /// The app's queue has caught up with the [`LeftQueue`] numbered `seq` and those before it.
    pub fn ack_left(&self, seq: u64) {
        self.send(Command::AckLeft(seq));
    }

    /// Insert items at `index`, or append past the end. Used to undo a clear.
    pub fn insert_at(&self, index: usize, items: Vec<QueueItem>) {
        self.send(Command::InsertAt { index, items });
    }

    /// What the queue plays from, set with each new list.
    pub fn set_source(&self, source: Option<crate::queue_order::QueueSource>) {
        *lock(&self.source) = source;
    }

    pub fn source(&self) -> Option<crate::queue_order::QueueSource> {
        lock(&self.source).clone()
    }

    /// How many queued songs wait right after the one being heard.
    pub fn queued_run(&self) -> usize {
        let queue = lock(&self.queue);
        let audible = queue.audible_token.and_then(|t| queue.index_of(t));
        let flags: Vec<bool> = queue.entries.iter().map(|e| e.item.queued).collect();
        crate::queue_order::queued_run(&flags, audible)
    }

    pub fn clear(&self) {
        self.send(Command::Clear);
    }

    pub fn jump_to(&self, index: usize) {
        self.send(Command::JumpTo(index));
    }

    pub fn play(&self) {
        self.send(Command::Play);
    }

    pub fn pause(&self) {
        self.send(Command::Pause);
    }

    pub fn toggle(&self) {
        self.send(Command::Toggle);
    }

    pub fn seek_ms(&self, ms: u64) {
        self.send(Command::Seek(ms));
    }

    pub fn next(&self) {
        self.send(Command::Next);
    }

    pub fn previous(&self) {
        self.send(Command::Previous);
    }

    pub fn stop(&self) {
        self.send(Command::Stop);
    }

    pub fn set_volume(&self, volume: f32) {
        self.send(Command::SetVolume(volume));
    }

    pub fn set_muted(&self, muted: bool) {
        self.send(Command::SetMuted(muted));
    }

    pub fn set_repeat(&self, repeat: Repeat) {
        self.send(Command::SetRepeat(repeat));
    }

    pub fn set_crossfade_ms(&self, ms: u32) {
        self.send(Command::SetCrossfadeMs(ms));
    }

    /// Listening along with someone. While on, nothing crossfades or is held back, so their song
    /// changes cut in at once. Set it before loading their song.
    pub fn set_following(&self, on: bool) {
        self.send(Command::SetFollowing(on));
    }

    /// while on, the song playing now is the last: the queue ends after it instead of going on,
    /// and Next still moves on. for a listening session waiting on the other side's song. set it
    /// a few seconds before the end, before the next song is decoded.
    pub fn set_hold_after_current(&self, on: bool) {
        self.send(Command::HoldAfterCurrent(on));
    }

    /// Smart crossfade: with a crossfade set, a song that continues the previous one's album in
    /// order (next track, or the next disc's first) joins gapless instead of fading. Needs
    /// [`Self::set_album_lookup`] to know where songs sit.
    pub fn set_smart_crossfade(&self, on: bool) {
        self.send(Command::SetSmartCrossfade(on));
    }

    /// Smart transitions: with a crossfade set, each join's length, place, beat alignment, bass
    /// swap and loudness come from the songs' measured starts and ends. needs
    /// [`Self::set_edge_store`]; songs not measured yet get the plain crossfade.
    pub fn set_smart_transitions(&self, on: bool) {
        self.send(Command::SetSmartTransitions(on));
    }

    /// where songs' edge analyses are kept, and their tempo and ReplayGain read.
    pub fn set_edge_store(&self, store: Arc<dyn EdgeStore>) {
        self.queue.lock().expect("queue").analyzer = Some(Analyzer::spawn(store));
    }

    /// How the player names the song playing to the system (the Linux sound server's stream
    /// name, which the desktop's mixer shows).
    pub fn set_title_lookup(&self, lookup: TitleLookup) {
        self.queue.lock().expect("queue").titles = Some(lookup);
    }

    /// How the player looks up a song's album, disc and track, for Smart crossfade.
    pub fn set_album_lookup(&self, lookup: AlbumLookup) {
        let mut q = self.queue.lock().expect("queue");
        q.albums = Some(lookup);
        q.entries.iter_mut().for_each(|e| e.spot = Arc::default());
    }

    /// Set the equalizer, or None for bit-exact playback. Applies at once without clicks.
    pub fn set_eq(&self, settings: Option<&crate::audio::processing::EqSettings>) {
        self.render.set_eq(settings);
    }

    /// Play a live stream in place of the queue. `item.id` identifies it in events. Failures
    /// arrive as [`PlayerEvent::Error`].
    pub fn play_live(&self, item: QueueItem, url: String) {
        match LiveOpener::new(url) {
            Ok(opener) => self.send(Command::PlayLive { item, opener: Arc::new(opener) }),
            Err(e) => self.send_error(format!("station: {e}")),
        }
    }

    /// The song the live stream reports as playing, if any.
    pub fn stream_title(&self) -> Option<String> {
        self.live.lock().expect("live").as_ref().and_then(|o| o.title())
    }

    fn send_error(&self, message: String) {
        lock(&self.listeners).iter().for_each(|l| l(&PlayerEvent::Error(message.clone())));
    }

    pub fn set_output_device(&self, device: Option<String>) {
        self.send(Command::SetDevice(device));
    }

    pub fn set_output_policy(&self, policy: OutputPolicy) {
        self.send(Command::SetPolicy(policy));
    }

    /// Retry the server now instead of waiting.
    pub fn retry_now(&self) {
        if let Some(net) = self.net.as_ref() {
            net.retry_now();
        }
        self.send(Command::RetryNow);
    }

    /// Wait until every command sent so far has been applied.
    pub fn sync(&self) {
        let (tx, rx) = channel();
        self.send(Command::Sync(tx));
        let _ = rx.recv_timeout(Duration::from_secs(5));
    }

    pub fn list_devices() -> Result<Vec<DeviceInfo>> {
        CpalBackend::default().list_devices()
    }

    /// Status and position, without copying the queue. Cheap enough to call every frame.
    pub fn quick_state(&self) -> (PlayerStatus, u64) {
        (lock(&self.state).status, self.render.position_ms())
    }

    /// What the audible song is being delivered as, once it's open.
    pub fn delivered(&self) -> Option<Delivered> {
        let queue = lock(&self.queue);
        let audible = queue.audible_token.and_then(|t| queue.index_of(t))?;
        queue.entries.get(audible).and_then(|e| e.opener.delivered())
    }

    pub fn snapshot(&self) -> PlayerSnapshot {
        let state = lock(&self.state).clone();
        let queue = lock(&self.queue);
        let audible = queue.audible_token.and_then(|t| queue.index_of(t));
        let item = audible.and_then(|i| queue.entries.get(i)).map(|e| e.item.clone());
        let delivered = audible.and_then(|i| queue.entries.get(i)).and_then(|e| e.opener.delivered());
        PlayerSnapshot {
            status: state.status,
            index: audible,
            item,
            position_ms: self.render.position_ms(),
            volume: state.volume,
            muted: state.muted,
            device_volume: state.device_volume,
            repeat: queue.repeat,
            crossfade_ms: state.crossfade_ms,
            queue: queue.entries.iter().map(|e| e.item.clone()).collect(),
            output_rate: state.output_rate,
            device: state.device,
            underrun_frames: self.render.underrun_frames.load(Ordering::Acquire),
            rebuffer_frames: self.render.rebuffer_frames.load(Ordering::Acquire),
            xruns: state.xruns,
            live: lock(&self.live).is_some(),
            waiting_for_server: state.waiting,
            error: state.error,
            delivered,
            source: lock(&self.source).clone(),
        }
    }

    pub fn shutdown(&self) {
        self.send(Command::Shutdown);
        if let Some(t) = self.thread.lock().expect("thread").take() {
            let _ = t.join();
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn mode_for(policy: &OutputPolicy, caps: &crate::audio::output::DeviceCaps) -> OutputMode {
    match policy {
        OutputPolicy::FollowTrack => OutputMode::FollowWithin { ranges: caps.rates.clone(), fallback: caps.default_rate },
        OutputPolicy::DeviceDefault => OutputMode::FixedRate(caps.default_rate),
        OutputPolicy::Fixed(rate) => OutputMode::FixedRate(*rate),
    }
}

struct Control {
    queue: Arc<Mutex<Queue>>,
    engine: EngineControl,
    events: Receiver<EngineEvent>,
    worker: Option<JoinHandle<()>>,
    core: Arc<Mutex<RenderCore>>,
    shared: Arc<RenderShared>,
    backend: Box<dyn OutputBackend>,
    handle: Option<Box<dyn OutputHandle>>,
    policy: OutputPolicy,
    state: Arc<Mutex<ControlState>>,
    listeners: Arc<Mutex<Vec<Listener>>>,
    last_generation: u64,
    audible: Option<usize>,
    audible_run: u64,
    heard_mark: u64,
    last_position_event: Instant,
    idle_since: Option<Instant>,
    drain_started: Option<Instant>,
    live: Arc<Mutex<Option<Arc<LiveOpener>>>>,
    live_paused: bool,
    net: Option<Arc<NetworkStatus>>,
    idle_close_after: Duration,
    engine_waiting: bool,
    waiting: bool,
    waiting_message: Option<String>,
    /// Status went to Buffering because audio ran out during an outage.
    stalled: bool,
    last_status_sent: PlayerStatus,
    recovered_at: Option<Instant>,
    /// The entry last loaded on purpose, as opposed to one the decoder moved to.
    loaded: Option<usize>,
    /// Numbering for queued songs that left on their own, and those not yet acknowledged as
    /// (seq, index). See `rebase`.
    left_seq: u64,
    unacked: Vec<(u64, usize)>,
    /// The count of the last volume report taken, so each is taken once.
    device_report: Option<u64>,
    /// Set when the engine thread has died. Commands that need it report this instead.
    engine_failed: Option<String>,
    /// Listening along with someone.
    following: bool,
    /// The song being heard, until its [`PlayerEvent::PlayEnded`].
    play: Option<OpenPlay>,
}

impl Control {
    fn run(&mut self, rx: Receiver<Command>) {
        loop {
            match rx.recv_timeout(TICK) {
                Ok(cmd) => {
                    if !self.handle(cmd) {
                        break;
                    }
                    // Drain queued commands before the slower work below.
                    while let Ok(cmd) = rx.try_recv() {
                        if !self.handle(cmd) {
                            self.teardown();
                            return;
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            self.pump_events();
            self.handle_reopen();
            self.tick();
        }
        self.teardown();
    }

    fn teardown(&mut self) {
        self.end_live();
        self.engine.shutdown();
        self.handle = None;
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }

    fn emit(&self, event: PlayerEvent) {
        for listener in lock(&self.listeners).iter() {
            listener(&event);
        }
    }

    /// This thread panicked. Tell listeners, leave a state that can't look like playback, and
    /// stop the engine.
    fn thread_died(&mut self, message: &str) {
        let text = format!("{DIED}: {message}");
        tracing::error!("player thread panicked: {message}");
        {
            let mut state = lock(&self.state);
            state.status = PlayerStatus::Stopped;
            state.waiting = false;
            state.error = Some(text.clone());
        }
        // A listener may be what panicked, so guard each one.
        for listener in lock(&self.listeners).iter() {
            let _ = catch_unwind(AssertUnwindSafe(|| listener(&PlayerEvent::Error(text.clone()))));
        }
        let _ = catch_unwind(AssertUnwindSafe(|| self.teardown()));
    }

    /// Report the engine thread having stopped. It never stops during normal playback, so this
    /// is always a failure.
    fn check_engine(&mut self) {
        if !self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
            return;
        }
        let reason = match self.worker.take().map(JoinHandle::join) {
            Some(Err(payload)) => panic_text(payload.as_ref()),
            _ => "the audio engine exited".to_string(),
        };
        tracing::error!("audio engine stopped: {reason}");
        let text = format!("{DIED}: {reason}");
        self.engine_failed = Some(text.clone());
        self.engine_waiting = false;
        self.handle = None;
        lock(&self.state).error = Some(text.clone());
        self.set_status(PlayerStatus::Stopped);
        self.emit(PlayerEvent::Error(text));
    }

    fn status(&self) -> PlayerStatus {
        self.state.lock().expect("state").status
    }

    /// Compares against what listeners were last told, since `Player::set_queue` writes the
    /// status early.
    fn set_status(&mut self, status: PlayerStatus) {
        self.state.lock().expect("state").status = status;
        if self.last_status_sent != status {
            self.last_status_sent = status;
            self.emit(PlayerEvent::StatusChanged(status));
        }
    }

    /// Track the server going away or coming back. The player is only waiting once buffered
    /// audio runs out.
    fn update_waiting(&mut self) {
        let net = self.net.as_ref();
        let now_waiting = self.engine_waiting || net.is_some_and(|n| n.waiting());
        // Changing tracks drops one stream before the next reports in. That isn't recovery.
        let waiting = if now_waiting {
            self.recovered_at = None;
            true
        } else if self.waiting {
            self.recovered_at.get_or_insert_with(Instant::now).elapsed() < RECOVERY_HOLD
        } else {
            false
        };
        if waiting != self.waiting {
            self.waiting = waiting;
            self.state.lock().expect("state").waiting = waiting;
            let message = waiting.then(|| self.waiting_message.clone().or_else(|| self.net.as_ref().map(|n| n.message()))).flatten();
            self.emit(PlayerEvent::Connection { waiting, message });
        }
        if self.status() != PlayerStatus::Buffering {
            self.stalled = false;
        }
        let dry = self.core.try_lock().ok().map(|c| c.buffered_frames() == 0);
        if self.waiting && self.status() == PlayerStatus::Playing && dry == Some(true) {
            self.shared.rebuffer_request.store(true, Ordering::Release);
            self.stalled = true;
            self.set_status(PlayerStatus::Buffering);
        } else if !self.waiting && self.stalled && !self.shared.paused.load(Ordering::Acquire) && dry == Some(false) {
            // Resumed in the same track, so no track start will report it.
            self.stalled = false;
            self.set_status(PlayerStatus::Playing);
        }
    }

    /// Close the output after a while idle. Status decides, so no stale timer can close the
    /// device under new playback.
    fn close_idle_output(&mut self) {
        if !matches!(self.status(), PlayerStatus::Paused | PlayerStatus::Stopped) || self.handle.is_none() {
            self.idle_since = None;
            return;
        }
        if self.idle_since.get_or_insert_with(Instant::now).elapsed() > self.idle_close_after {
            self.handle = None;
            self.idle_since = None;
        }
    }

    /// Apply the crossfade setting, or none while following someone.
    fn apply_crossfade(&self) {
        let (ms, rate) = {
            let state = self.state.lock().expect("state");
            (state.crossfade_ms, state.output_rate.max(48_000))
        };
        let ms = if self.following { 0 } else { ms };
        self.engine.set_crossfade((u64::from(ms) * u64::from(rate) / 1000) as usize);
    }

    fn apply_volume(&self) {
        let st = self.state.lock().expect("state");
        // A device with its own volume gets the full signal. The control moves the device's
        // volume instead, and only when the listener moves it.
        let remote = self.handle.as_ref().is_some_and(|h| h.remote_volume());
        self.shared.set_volume(if remote { 1.0 } else if st.muted { 0.0 } else { st.volume });
    }

    /// Send a volume or mute change to a device with its own volume.
    fn send_remote_volume(&self) {
        let (level, muted) = {
            let st = self.state.lock().expect("state");
            (st.device_volume.map(|d| d.level), st.muted)
        };
        if let (Some(h), Some(level)) = (self.handle.as_ref().filter(|h| h.remote_volume()), level) {
            h.set_volume(if muted { 0.0 } else { level });
        }
    }

    /// Track the output's own volume as it opens and as it reports. A new output that won't say
    /// starts at [`ASSUMED_DEVICE_VOLUME`] and a reopened one keeps its last level. Nothing is sent.
    fn follow_device_volume(&mut self, opened: bool) {
        let Some(handle) = self.handle.as_ref() else { return };
        if !handle.remote_volume() {
            self.set_device_volume(None);
            return;
        }
        let report = handle.reported_volume();
        let current = self.state.lock().expect("state").device_volume;
        let next = match report {
            Some((n, level)) if opened || self.device_report != Some(n) => {
                self.device_report = Some(n);
                Some(DeviceVolume { level, known: true })
            }
            None if opened => {
                self.device_report = None;
                Some(current.unwrap_or(DeviceVolume { level: ASSUMED_DEVICE_VOLUME, known: false }))
            }
            _ => return,
        };
        self.set_device_volume(next);
    }

    fn set_device_volume(&mut self, volume: Option<DeviceVolume>) {
        let changed = {
            let mut st = self.state.lock().expect("state");
            let changed = st.device_volume != volume;
            st.device_volume = volume;
            changed
        };
        if changed {
            self.emit(PlayerEvent::DeviceVolume(volume));
        }
    }

    /// Tell an output that shows what's playing which song is heard, and where.
    fn tell_now_playing(&self) {
        let Some(handle) = self.handle.as_ref() else { return };
        let (item, titles) = {
            let q = self.queue.lock().expect("queue");
            (q.audible_token.and_then(|t| q.index_of(t)).and_then(|i| q.entries.get(i)).map(|e| e.item.clone()), q.titles.clone())
        };
        if let Some(item) = item {
            handle.now_playing(&item.id, self.shared.position_ms(), item.duration_ms);
            handle.set_title(&titles.and_then(|t| t(&item.id)).unwrap_or_default());
        }
    }

    fn device(&self) -> Option<String> {
        self.state.lock().expect("state").device.clone()
    }

    fn load_index(&mut self, index: usize, start_ms: u64) {
        let req = {
            let mut q = self.queue.lock().expect("queue");
            let req = q.request(index, start_ms);
            if req.is_some() {
                q.decode_index = Some(index);
            }
            req
        };
        if let Some(req) = req {
            self.loaded = Some(req.token);
            self.engine.load(req);
            if self.status() == PlayerStatus::Stopped {
                self.set_status(PlayerStatus::Buffering);
            }
        }
    }

    fn audible_index(&self) -> Option<usize> {
        let q = self.queue.lock().expect("queue");
        q.audible_token.and_then(|t| q.index_of(t)).or(q.decode_index)
    }

    fn ensure_stream(&mut self, rate: u32) {
        if self.handle.is_some() {
            return;
        }
        let device = self.device();
        match self.backend.open(device.as_deref(), rate, self.core.clone()) {
            Ok(h) => {
                let device_rate = h.device_rate();
                tracing::info!(target: "navia_core::output", "opened {} at {rate} Hz ({:?})", device.as_deref().unwrap_or("default"), self.policy);
                self.core.lock().expect("core").set_stream_rate(rate);
                self.handle = Some(h);
                self.follow_device_volume(true);
                self.apply_volume();
                self.tell_now_playing();
                self.state.lock().expect("state").output_rate = rate;
                self.emit(PlayerEvent::OutputChanged { rate, device_rate, device });
            }
            Err(e) => self.output_failed(device.as_deref(), &e),
        }
    }

    /// Tell listeners the output couldn't start, naming the device when possible.
    fn output_failed(&self, device: Option<&str>, error: &crate::Error) {
        tracing::warn!("output {}: {error}", device.unwrap_or("default"));
        let name = self.backend.device_name(device);
        self.emit(PlayerEvent::OutputError { device: name, message: error.to_string() });
    }

    /// End any live stream, canceling its blocked reads so the engine can move on.
    fn end_live(&mut self) {
        if let Some(opener) = self.live.lock().expect("live").take() {
            opener.cancel();
        }
        self.live_paused = false;
    }

    fn is_live(&self) -> bool {
        self.live.lock().expect("live").is_some()
    }

    /// Seek the track being heard. The decoder may already be on the next one, which gives way.
    fn seek(&mut self, ms: u64) {
        match self.audible_behind_decoder() {
            Some(index) => self.load_index(index, ms),
            None => self.engine.seek(ms),
        }
    }

    /// The audible entry's index when the decoder has moved past it or finished the queue.
    fn audible_behind_decoder(&self) -> Option<usize> {
        let stopped = self.status() == PlayerStatus::Stopped;
        let q = self.queue.lock().expect("queue");
        let audible = q.audible_token?;
        let Some(decoding) = q.decode_index.and_then(|d| q.entries.get(d)).map(|e| e.token) else {
            return if stopped || q.live { None } else { q.index_of(audible) };
        };
        if decoding == audible || self.loaded == Some(decoding) {
            return None;
        }
        q.index_of(audible)
    }

    fn handle(&mut self, cmd: Command) -> bool {
        // With the engine gone nothing can play, so report it rather than drop the request.
        if let Some(message) = self.engine_failed.clone()
            && matches!(cmd, Command::PlayLive { .. } | Command::SetQueue { .. } | Command::JumpTo(_) | Command::Play | Command::Toggle | Command::Seek(_) | Command::Next | Command::Previous)
        {
            self.set_status(PlayerStatus::Stopped);
            self.emit(PlayerEvent::Error(message));
            return true;
        }
        let cmd = self.rebase(cmd);
        match cmd {
            Command::PlayLive { item, opener } => {
                self.end_play(PlayEnd::Replaced);
                self.unacked.clear();
                self.end_live();
                // A queue end still in flight belongs to what's being replaced.
                self.shared.queue_ended.store(false, Ordering::Release);
                *self.live.lock().expect("live") = Some(opener.clone());
                let req = {
                    let mut q = self.queue.lock().expect("queue");
                    q.live = true;
                    let token = q.next_token;
                    q.next_token += 1;
                    q.entries = vec![Entry { token, item, opener: opener as Arc<dyn TrackOpener>, spot: Arc::default(), traits: Arc::default() }];
                    q.decode_index = Some(0);
                    q.audible_token = None;
                    q.request(0, 0)
                };
                self.emit(PlayerEvent::QueueChanged);
                self.shared.paused.store(false, Ordering::Release);
                self.set_status(PlayerStatus::Buffering);
                if let Some(req) = req {
                    self.engine.load(req);
                }
            }
            Command::SetQueue { items, start_index, start_ms, paused } => {
                // The same song continuing from its position, in a rebuilt queue, is the same play.
                let carries_on = start_ms > 0 && items.get(start_index).is_some_and(|i| self.play.as_ref().is_some_and(|p| p.item.id == i.id));
                if !carries_on {
                    self.end_play(PlayEnd::Replaced);
                }
                self.unacked.clear();
                self.end_live();
                self.shared.queue_ended.store(false, Ordering::Release);
                let empty = items.is_empty();
                {
                    let mut q = self.queue.lock().expect("queue");
                    let entries: Vec<Entry> = items.into_iter().map(|i| q.make_entry(i)).collect();
                    q.entries = entries;
                    q.decode_index = None;
                    q.audible_token = None;
                    q.live = false;
                }
                self.emit(PlayerEvent::QueueChanged);
                if empty {
                    self.engine.stop();
                    self.set_status(PlayerStatus::Stopped);
                } else {
                    self.shared.paused.store(paused, Ordering::Release);
                    self.set_status(if paused { PlayerStatus::Paused } else { PlayerStatus::Buffering });
                    self.load_index(start_index, start_ms);
                }
            }
            Command::Append(items) => {
                {
                    let mut q = self.queue.lock().expect("queue");
                    for i in items {
                        let e = q.make_entry(i);
                        q.entries.push(e);
                    }
                }
                self.emit(PlayerEvent::QueueChanged);
                self.engine.realign();
            }
            Command::InsertNext(items) => {
                let mut q = self.queue.lock().expect("queue");
                let at = q.audible_token.and_then(|t| q.index_of(t)).map_or(0, |i| i + 1);
                let new: Vec<Entry> = items.into_iter().map(|i| q.make_entry(i)).collect();
                let inserted = new.len();
                for (k, e) in new.into_iter().enumerate() {
                    q.entries.insert(at + k, e);
                }
                if let Some(d) = q.decode_index
                    && d >= at
                {
                    q.decode_index = Some(d + inserted);
                }
                drop(q);
                self.emit(PlayerEvent::QueueChanged);
                self.engine.realign();
            }
            Command::ReplaceUpcoming(items) => self.replace_upcoming(items),
            Command::RemoveItems(positions) => self.remove_items(positions),
            Command::Move { from, to } => self.move_items(vec![from], if to > from { to + 1 } else { to }),
            Command::MoveItems { positions, before } => self.move_items(positions, before),
            Command::SetQueued { positions, queued } => {
                let mut q = self.queue.lock().expect("queue");
                for p in positions {
                    if let Some(e) = q.entries.get_mut(p) {
                        e.item.queued = queued;
                    }
                }
                drop(q);
                self.emit(PlayerEvent::QueueChanged);
            }
            Command::ClearQueued => {
                let (at, run) = {
                    let q = self.queue.lock().expect("queue");
                    let audible = q.audible_token.and_then(|t| q.index_of(t));
                    let flags: Vec<bool> = q.entries.iter().map(|e| e.item.queued).collect();
                    (audible.map_or(0, |a| a + 1), crate::queue_order::queued_run(&flags, audible))
                };
                self.remove_items((at..at + run).collect());
            }
            Command::ShuffleQueued(seed) => {
                let order = {
                    let q = self.queue.lock().expect("queue");
                    let audible = q.audible_token.and_then(|t| q.index_of(t));
                    let flags: Vec<bool> = q.entries.iter().map(|e| e.item.queued).collect();
                    crate::queue_order::shuffled_queued_order(&flags, audible, seed)
                };
                if let Some(order) = order {
                    self.reorder(&order);
                }
            }
            Command::ClearList { keep_tail } => {
                let range = {
                    let q = self.queue.lock().expect("queue");
                    let audible = q.audible_token.and_then(|t| q.index_of(t));
                    let flags: Vec<bool> = q.entries.iter().map(|e| e.item.queued).collect();
                    crate::queue_order::list_after_queued(&flags, audible, flags.len().saturating_sub(keep_tail))
                };
                self.remove_items(range.collect());
            }
            Command::InsertAt { index, items } => {
                {
                    let mut q = self.queue.lock().expect("queue");
                    let at = index.min(q.entries.len());
                    let decode_token = q.decode_index.and_then(|d| q.entries.get(d)).map(|e| e.token);
                    let new: Vec<Entry> = items.into_iter().map(|i| q.make_entry(i)).collect();
                    q.entries.splice(at..at, new);
                    q.decode_index = decode_token.and_then(|t| q.index_of(t));
                }
                self.emit(PlayerEvent::QueueChanged);
                self.engine.realign();
            }
            Command::Clear => {
                self.end_play(PlayEnd::Replaced);
                self.unacked.clear();
                self.end_live();
                {
                    let mut q = self.queue.lock().expect("queue");
                    q.entries.clear();
                    q.decode_index = None;
                    q.audible_token = None;
                    q.live = false;
                }
                self.engine.stop();
                self.set_status(PlayerStatus::Stopped);
                self.emit(PlayerEvent::QueueChanged);
            }
            Command::JumpTo(index) => {
                self.end_play(PlayEnd::Replaced);
                self.shared.paused.store(false, Ordering::Release);
                self.set_status(PlayerStatus::Buffering);
                self.load_index(index, 0);
            }
            Command::Play => match self.status() {
                // A paused station starts again at the live edge.
                PlayerStatus::Paused if self.live_paused => {
                    self.live_paused = false;
                    self.shared.paused.store(false, Ordering::Release);
                    self.set_status(PlayerStatus::Buffering);
                    self.load_index(0, 0);
                }
                PlayerStatus::Paused => {
                    self.shared.paused.store(false, Ordering::Release);
                    let rate = self.state.lock().expect("state").output_rate;
                    if rate != 0 {
                        self.ensure_stream(rate);
                    }
                    self.set_status(PlayerStatus::Playing);
                }
                PlayerStatus::Stopped => {
                    let index = self.audible_index().unwrap_or(0);
                    self.shared.paused.store(false, Ordering::Release);
                    self.load_index(index, 0);
                }
                _ => {}
            },
            Command::Pause => {
                if matches!(self.status(), PlayerStatus::Playing | PlayerStatus::Buffering) {
                    self.shared.paused.store(true, Ordering::Release);
                    self.set_status(PlayerStatus::Paused);
                    // A station can't wait, so drop the connection and rejoin live on resume.
                    if let Some(opener) = self.live.lock().expect("live").clone() {
                        opener.cancel();
                        self.live_paused = true;
                        self.engine.stop();
                    }
                }
            }
            Command::Toggle => {
                // Include Buffering, since that's how playback gets paused during an outage.
                let cmd = if matches!(self.status(), PlayerStatus::Playing | PlayerStatus::Buffering) { Command::Pause } else { Command::Play };
                return self.handle(cmd);
            }
            Command::Seek(ms) => {
                if !self.is_live() {
                    self.seek(ms);
                }
            }
            Command::Next => {
                let target = {
                    let q = self.queue.lock().expect("queue");
                    self.audible_index_of(&q).and_then(|i| q.skip_from(i))
                };
                self.end_play(PlayEnd::Skipped);
                match target {
                    Some(index) => {
                        self.leave_repeat_one();
                        self.shared.paused.store(false, Ordering::Release);
                        self.set_status(PlayerStatus::Buffering);
                        self.load_index(index, 0);
                    }
                    None => {
                        self.engine.stop();
                        self.set_status(PlayerStatus::Stopped);
                    }
                }
            }
            Command::Previous => {
                let position = self.shared.position_ms();
                let back = {
                    let q = self.queue.lock().expect("queue");
                    self.audible_index_of(&q).map(|i| (i, q.back_from(i)))
                };
                match back {
                    Some((_, Some(to))) if position < PREVIOUS_RESTART_MS => {
                        self.end_play(PlayEnd::Replaced);
                        self.leave_repeat_one();
                        self.shared.paused.store(false, Ordering::Release);
                        self.set_status(PlayerStatus::Buffering);
                        self.load_index(to, 0);
                    }
                    Some(_) if !self.is_live() => self.seek(0),
                    _ => {}
                }
            }
            Command::Stop => {
                self.end_play(PlayEnd::Replaced);
                self.end_live();
                self.engine.stop();
                self.shared.paused.store(false, Ordering::Release);
                self.set_status(PlayerStatus::Stopped);
            }
            Command::SetVolume(v) => {
                // On a device with its own volume this sets that device's volume. The player's
                // own stays for system devices.
                let v = v.clamp(0.0, 1.0);
                let changed = {
                    let mut st = self.state.lock().expect("state");
                    match st.device_volume.as_mut() {
                        Some(d) => {
                            // Don't resend the same level.
                            let changed = (d.level - v).abs() > 1e-4 || !d.known;
                            *d = DeviceVolume { level: v, known: true };
                            changed
                        }
                        None => {
                            st.volume = v;
                            false
                        }
                    }
                };
                self.apply_volume();
                if changed {
                    self.send_remote_volume();
                }
            }
            Command::SetMuted(m) => {
                let changed = std::mem::replace(&mut self.state.lock().expect("state").muted, m) != m;
                self.apply_volume();
                if changed {
                    self.send_remote_volume();
                }
            }
            Command::SetRepeat(r) => {
                self.queue.lock().expect("queue").repeat = r;
                self.engine.realign();
            }
            Command::SetSmartCrossfade(on) => self.engine.set_smart_joins(on),
            Command::SetSmartTransitions(on) => {
                let mut q = self.queue.lock().expect("queue");
                q.smart_transitions = on;
                if let Some(index) = q.decode_index {
                    q.measure_from(index);
                }
                drop(q);
                self.engine.set_smart_transitions(on);
            }
            Command::SetCrossfadeMs(ms) => {
                self.state.lock().expect("state").crossfade_ms = ms;
                self.apply_crossfade();
            }
            Command::HoldAfterCurrent(on) => {
                let mut q = self.queue.lock().expect("queue");
                let current = self.audible_index_of(&q).and_then(|i| q.entries.get(i)).map(|e| e.token);
                q.hold = if on { current } else { None };
            }
            Command::SetFollowing(on) => {
                if self.following != on {
                    self.following = on;
                    self.apply_crossfade();
                }
                if on && let Some(play) = self.play.as_mut() {
                    play.following = true;
                }
            }
            Command::SetDevice(device) => {
                let changed = std::mem::replace(&mut self.state.lock().expect("state").device, device.clone()) != device;
                if changed {
                    // A different device, so forget the last one's volume.
                    self.set_device_volume(None);
                }
                self.rebuild_output();
            }
            Command::SetPolicy(policy) => {
                self.policy = policy;
                self.rebuild_output();
            }
            Command::RetryNow => self.engine.retry_now(),
            Command::AckLeft(seq) => self.unacked.retain(|(s, _)| *s > seq),
            Command::Sync(ack) => {
                let _ = ack.send(());
            }
            Command::Shutdown => return false,
            #[cfg(test)]
            Command::CrashEngineForTest => self.engine.crash(),
        }
        true
    }

    /// Map a command from the app's copy of the queue onto the queue as it is now. Entries that
    /// left and aren't acknowledged count as still there. An index naming such an entry is
    /// dropped for a removal or move, or held at its place for an insert, move target or jump.
    fn rebase(&self, cmd: Command) -> Command {
        if self.unacked.is_empty() {
            return cmd;
        }
        let shift = |i: usize| -> Option<usize> {
            let mut i = i;
            for &(_, left) in &self.unacked {
                if i == left {
                    return None;
                }
                if i > left {
                    i -= 1;
                }
            }
            Some(i)
        };
        let place = |i: usize| -> usize { self.unacked.iter().fold(i, |i, &(_, left)| if i > left { i - 1 } else { i }) };
        match cmd {
            Command::RemoveItems(positions) => Command::RemoveItems(positions.into_iter().filter_map(shift).collect()),
            Command::Move { from, to } => match shift(from) {
                Some(from) => Command::Move { from, to: place(to) },
                None => Command::MoveItems { positions: Vec::new(), before: 0 },
            },
            Command::MoveItems { positions, before } => Command::MoveItems { positions: positions.into_iter().filter_map(shift).collect(), before: place(before) },
            Command::SetQueued { positions, queued } => Command::SetQueued { positions: positions.into_iter().filter_map(shift).collect(), queued },
            Command::JumpTo(index) => Command::JumpTo(place(index)),
            Command::InsertAt { index, items } => Command::InsertAt { index: place(index), items },
            other => other,
        }
    }

    /// Skipping by hand with Repeat one switches to Repeat all, so the new song doesn't loop.
    fn leave_repeat_one(&mut self) {
        let changed = {
            let mut q = self.queue.lock().expect("queue");
            let one = q.repeat == Repeat::One;
            if one {
                q.repeat = Repeat::All;
            }
            one
        };
        if changed {
            self.emit(PlayerEvent::RepeatChanged(Repeat::All));
        }
    }

    fn audible_index_of(&self, q: &Queue) -> Option<usize> {
        q.audible_token.and_then(|t| q.index_of(t)).or(q.decode_index)
    }

    /// Reapply the device and rate settings and restart the current track where it was.
    fn rebuild_output(&mut self) {
        let device = self.device();
        let caps = match self.backend.caps(device.as_deref()) {
            Ok(c) => c,
            Err(e) => {
                self.output_failed(device.as_deref(), &e);
                return;
            }
        };
        self.engine.set_output_mode(mode_for(&self.policy, &caps));
        self.handle = None;
        // before anything is heard the decoder may already be past the queue's end; the song loaded last is then the one to restart
        let loaded = self.loaded.and_then(|t| self.queue.lock().expect("queue").index_of(t));
        if let Some(index) = self.audible_index().or(loaded)
            && !matches!(self.status(), PlayerStatus::Stopped)
        {
            let position = self.shared.position_ms();
            let keep_paused = self.status() == PlayerStatus::Paused;
            self.load_index(index, position);
            if keep_paused {
                self.shared.paused.store(true, Ordering::Release);
                self.set_status(PlayerStatus::Paused);
            }
        }
    }

    fn remove_items(&mut self, positions: Vec<usize>) {
        let audible_gone = {
            let mut q = self.queue.lock().expect("queue");
            let len = q.entries.len();
            let mut gone = vec![false; len];
            for &p in &positions {
                if p < len {
                    gone[p] = true;
                }
            }
            if !gone.contains(&true) {
                return;
            }
            let audible = q.audible_token.and_then(|t| q.index_of(t));
            // The song being heard goes last, through the path that moves on from it.
            let audible_gone = audible.filter(|&a| gone[a]);
            if let Some(a) = audible_gone {
                gone[a] = false;
            }
            // The decoder's entry, or the nearest kept entry before it in play order, so nothing
            // already decoded is decoded again.
            let decode_token = q.decode_index.filter(|&d| d < len).and_then(|d| {
                let mut i = d;
                loop {
                    if !gone[i] {
                        return Some(q.entries[i].token);
                    }
                    i = if i == 0 { len - 1 } else { i - 1 };
                    if i == d {
                        return None;
                    }
                }
            });
            let mut k = 0;
            q.entries.retain(|_| {
                k += 1;
                !gone[k - 1]
            });
            q.decode_index = decode_token.and_then(|t| q.index_of(t));
            audible_gone.and_then(|_| q.audible_token).and_then(|t| q.index_of(t))
        };
        self.emit(PlayerEvent::QueueChanged);
        match audible_gone {
            Some(index) => self.remove_audible(index),
            // Replace audio decoded ahead if the next song changed.
            None => self.engine.realign(),
        }
    }

    /// The track being heard was removed, so move on to whatever takes its place.
    fn remove_audible(&mut self, index: usize) {
        let exists = {
            let mut q = self.queue.lock().expect("queue");
            q.entries.remove(index);
            q.decode_index = q.decode_index.map(|d| if d > index { d - 1 } else { d });
            q.entries.len() > index
        };
        self.emit(PlayerEvent::QueueChanged);
        if exists {
            self.load_index(index, 0);
        } else {
            self.engine.stop();
            self.set_status(PlayerStatus::Stopped);
        }
    }

    fn replace_upcoming(&mut self, items: Vec<QueueItem>) {
        {
            let mut q = self.queue.lock().expect("queue");
            let Some(audible) = q.audible_token.and_then(|t| q.index_of(t)).or(q.decode_index) else { return };
            q.entries.truncate(audible + 1);
            let new: Vec<Entry> = items.into_iter().map(|i| q.make_entry(i)).collect();
            q.entries.extend(new);
            // The decoder ran ahead onto a removed entry. The engine cuts that audio at the join
            // and carries on gaplessly.
            if q.decode_index.is_some_and(|d| d > audible) {
                q.decode_index = Some(audible);
            }
        }
        self.emit(PlayerEvent::QueueChanged);
        self.engine.realign();
    }

    /// Reorder the entries so that `order[new] = old`. Every entry appears once.
    fn reorder(&mut self, order: &[usize]) {
        {
            let mut q = self.queue.lock().expect("queue");
            if order.len() != q.entries.len() {
                return;
            }
            let decode_token = q.decode_index.and_then(|d| q.entries.get(d)).map(|e| e.token);
            let mut old: Vec<Option<Entry>> = std::mem::take(&mut q.entries).into_iter().map(Some).collect();
            q.entries = order.iter().filter_map(|&i| old.get_mut(i).and_then(Option::take)).collect();
            q.decode_index = decode_token.and_then(|t| q.index_of(t));
        }
        self.emit(PlayerEvent::QueueChanged);
        // Replace audio decoded ahead if the next song changed.
        self.engine.realign();
    }

    fn move_items(&mut self, positions: Vec<usize>, before: usize) {
        {
            let mut q = self.queue.lock().expect("queue");
            let decode_token = q.decode_index.and_then(|d| q.entries.get(d)).map(|e| e.token);
            if crate::queue_order::move_items(&mut q.entries, &positions, before).is_none() {
                return;
            }
            if let Some(t) = decode_token {
                q.decode_index = q.index_of(t);
            }
        }
        self.emit(PlayerEvent::QueueChanged);
        // Replace audio decoded ahead if the next song changed.
        self.engine.realign();
    }

    fn pump_events(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                EngineEvent::TrackStart { output_rate, .. } => {
                    if self.handle.is_none() {
                        self.ensure_stream(output_rate);
                    }
                }
                EngineEvent::Error { message, .. } => self.emit(PlayerEvent::Error(message)),
                EngineEvent::QueueFinished => {}
                EngineEvent::TrackEnd { .. } => {}
                EngineEvent::WaitingForServer { message } => {
                    self.engine_waiting = true;
                    self.waiting_message = Some(message);
                }
                EngineEvent::WaitEnded => self.engine_waiting = false,
            }
        }
    }

    /// Reopen the device once the output has drained up to a rate change.
    fn handle_reopen(&mut self) {
        let requested = self.shared.rate_request.load(Ordering::Acquire);
        if requested == 0 {
            self.drain_started = None;
            return;
        }
        let Some(handle) = self.handle.as_ref() else {
            self.shared.rate_request.store(0, Ordering::Release);
            self.ensure_stream(requested);
            return;
        };
        let started = *self.drain_started.get_or_insert_with(Instant::now);
        let target = handle.latency_frames() + 2048;
        let drained = self.shared.drain_frames.load(Ordering::Acquire);
        if drained < target && started.elapsed() < Duration::from_millis(300) {
            return;
        }
        self.shared.rate_request.store(0, Ordering::Release);
        self.handle = None;
        self.ensure_stream(requested);
    }

    fn tick(&mut self) {
        self.check_engine();
        // With no device running, flushes and markers still need applying.
        if self.handle.is_none() && let Ok(mut core) = self.core.try_lock() {
            core.handle_flush();
            core.render_markers_only(&mut []);
        }
        if let Some(handle) = self.handle.as_ref() {
            let xruns = handle.xruns();
            self.state.lock().expect("state").xruns = xruns;
            if handle.failed() {
                // Reopen at once. A device that won't start reports `OutputError` then.
                tracing::warn!("the audio device was lost; opening the output again");
                self.rebuild_output();
            }
        }
        self.follow_device_volume(false);
        self.track_changes();
        self.report_heard();
        self.update_waiting();
        self.close_idle_output();
        if self.shared.queue_ended.swap(false, Ordering::AcqRel) && !self.live_paused {
            self.end_play(PlayEnd::Finished);
            self.end_live();
            self.set_status(PlayerStatus::Stopped);
            self.emit(PlayerEvent::QueueEnded);
        }
        if self.status() == PlayerStatus::Playing && self.last_position_event.elapsed() >= POSITION_EVENT_EVERY {
            self.last_position_event = Instant::now();
            self.emit(PlayerEvent::Position { ms: self.shared.position_ms() });
        }
    }

    fn track_changes(&mut self) {
        let generation = self.shared.track_generation.load(Ordering::Acquire);
        if generation == self.last_generation {
            return;
        }
        self.last_generation = generation;
        let token = self.shared.track_token.load(Ordering::Acquire);
        let previous = self.audible.filter(|&t| t != token && token != NO_TRACK);
        let (index_item, left, previous_gone) = {
            let mut q = self.queue.lock().expect("queue");
            if token != NO_TRACK {
                q.audible_token = Some(token);
            }
            // A song removed while it played didn't end on its own.
            let previous_gone = previous.is_some_and(|t| q.index_of(t).is_none());
            let left = if q.index_of(token).is_some() { previous.and_then(|t| q.leave_if_queued(t)) } else { None };
            let left = left.map(|index| {
                self.left_seq += 1;
                self.unacked.push((self.left_seq, index));
                LeftQueue { index, seq: self.left_seq }
            });
            (q.index_of(token).and_then(|i| q.entries.get(i).map(|e| (i, e.item.clone()))), left, previous_gone)
        };
        if left.is_some() {
            self.emit(PlayerEvent::QueueChanged);
        }
        // The same entry in a new run is a replay and counts as a new play. A seek keeps its run.
        let run = self.shared.track_run.load(Ordering::Acquire);
        let replay = self.audible == Some(token) && self.audible_run != run && self.shared.track_base_ms.load(Ordering::Acquire) == 0;
        let changed = self.audible != Some(token) || replay;
        self.audible = Some(token);
        self.audible_run = run;
        // A new song, replay or seek, so update what's shown.
        self.tell_now_playing();
        if self.status() == PlayerStatus::Buffering && !self.shared.paused.load(Ordering::Acquire) {
            self.set_status(PlayerStatus::Playing);
        }
        if changed && let Some((index, item)) = index_item {
            let base_ms = self.shared.track_base_ms.load(Ordering::Acquire);
            let carries_on = !replay && base_ms > 0 && self.play.as_ref().is_some_and(|p| p.item.id == item.id);
            if !carries_on {
                self.end_play(if previous_gone { PlayEnd::Replaced } else { PlayEnd::Finished });
                if item.duration_ms.is_some() && !self.is_live() {
                    self.play = Some(OpenPlay { item: item.clone(), start_frames: self.shared.heard_frames.load(Ordering::Acquire), following: self.following });
                }
            }
            self.heard_mark = self.shared.heard_frames.load(Ordering::Acquire);
            self.emit(PlayerEvent::TrackChanged { index, item, left });
        }
    }

    /// End the song being heard, if any, with a [`PlayerEvent::PlayEnded`].
    fn end_play(&mut self, end: PlayEnd) {
        let Some(play) = self.play.take() else { return };
        let frames = self.shared.heard_frames.load(Ordering::Acquire).saturating_sub(play.start_frames);
        let rate = u64::from(self.shared.track_rate.load(Ordering::Acquire));
        let heard_ms = if rate == 0 { 0 } else { frames.saturating_mul(1000) / rate };
        self.emit(PlayerEvent::PlayEnded { item: play.item, heard_ms, end, following: play.following });
    }

    fn report_heard(&mut self) {
        let now = self.shared.heard_frames.load(Ordering::Acquire);
        let delta = now - self.heard_mark;
        let rate = self.shared.track_rate.load(Ordering::Acquire);
        if delta == 0 || rate == 0 {
            return;
        }
        self.heard_mark = now;
        self.emit(PlayerEvent::Heard { since_last: Duration::from_secs_f64(delta as f64 / f64::from(rate)) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::output::CaptureBackend;

    #[test]
    fn a_dead_engine_is_reported_and_stays_reported() {
        let player = Player::with_backend(Arc::new(FileResolver(Default::default())), Box::new(CaptureBackend::default()), PlayerConfig::default()).unwrap();
        let errors = Arc::new(Mutex::new(Vec::new()));
        let sink = errors.clone();
        player.add_listener(move |e| {
            if let PlayerEvent::Error(m) = e {
                sink.lock().unwrap().push(m.clone());
            }
        });
        player.tx.send(Command::CrashEngineForTest).unwrap();
        let start = Instant::now();
        while player.snapshot().error.is_none() && start.elapsed() < Duration::from_secs(5) {
            thread::sleep(Duration::from_millis(5));
        }
        let snap = player.snapshot();
        assert_eq!(snap.status, PlayerStatus::Stopped);
        assert!(snap.error.as_deref().is_some_and(|m| m.contains("test crash")), "{snap:?}");
        assert_eq!(errors.lock().unwrap().len(), 1, "reported once, not every tick");
        player.play();
        player.set_queue(vec![QueueItem::new("a".into(), None)], 0, 0, false);
        player.sync();
        assert!(errors.lock().unwrap().len() >= 3, "commands after the crash must say so: {:?}", errors.lock().unwrap());
        assert_eq!(player.snapshot().status, PlayerStatus::Stopped, "a command can't look like it worked");
    }

    /// A 48 kHz stereo WAV at a steady level, so the audible span of a capture is easy to find.
    fn level_wav(path: &std::path::Path, frames: usize, level: i16) {
        let data: Vec<u8> = (0..frames * 2).flat_map(|_| level.to_le_bytes()).collect();
        let mut f = Vec::new();
        for chunk in [&b"RIFF"[..], &(36 + data.len() as u32).to_le_bytes(), b"WAVEfmt ", &16u32.to_le_bytes(), &1u16.to_le_bytes(), &2u16.to_le_bytes(), &48_000u32.to_le_bytes(), &192_000u32.to_le_bytes(), &4u16.to_le_bytes(), &16u16.to_le_bytes(), b"data", &(data.len() as u32).to_le_bytes(), &data] {
            f.extend_from_slice(chunk);
        }
        std::fs::write(path, f).unwrap();
    }

    /// Audible frames of a run through the player with a 250 ms crossfade, Smart on or off.
    fn smart_player_span(smart: bool, tracks: &[(&str, i32)]) -> usize {
        let dir = tempfile::tempdir().unwrap();
        let mut files = std::collections::HashMap::new();
        let mut spots = std::collections::HashMap::new();
        for (i, (album, track)) in tracks.iter().enumerate() {
            let path = dir.path().join(format!("{i}.wav"));
            level_wav(&path, 24_000, 8_000);
            files.insert(format!("s{i}"), path);
            spots.insert(format!("s{i}"), AlbumSpot { album_id: album.to_string(), disc: Some(1), track: Some(*track) });
        }
        let backend = CaptureBackend { speed: 0.0, ..Default::default() };
        let capture = backend.capture.clone();
        let player = Player::with_backend(Arc::new(FileResolver(files)), Box::new(backend), PlayerConfig::default()).unwrap();
        player.set_album_lookup(Arc::new(move |id: &str| spots.get(id).cloned()));
        player.set_crossfade_ms(250);
        player.set_smart_crossfade(smart);
        player.set_queue((0..tracks.len()).map(|i| QueueItem::new(format!("s{i}"), Some(500))).collect(), 0, 0, false);
        let start = Instant::now();
        while player.snapshot().status != PlayerStatus::Stopped || capture.is_empty() {
            assert!(start.elapsed() < Duration::from_secs(10), "didn't finish");
            thread::sleep(Duration::from_millis(5));
        }
        // Count audible frames, not the first-to-last span. A capture that pulls as fast as it
        // can runs dry on a loaded machine, and those gaps aren't the joins being measured.
        capture.samples().chunks_exact(2).filter(|f| f[0] != 0.0 || f[1] != 0.0).count()
    }

    #[test]
    fn smart_crossfade_through_the_player_uses_the_album_lookup() {
        let album = [("al", 1), ("al", 2), ("b", 7)];
        assert_eq!(smart_player_span(true, &album), 72_000 - 12_000, "only the join into the other album fades");
        assert_eq!(smart_player_span(false, &album), 72_000 - 24_000, "plain crossfade fades both");
    }
}
