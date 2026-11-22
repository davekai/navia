//! The playback engine: queue, decode, resample, crossfade, processing, ring buffer.
//! [`RenderCore`] pulls from the ring in any block size, as a device callback would.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use ringbuf::traits::{Observer as _, Producer as _, Split};
use ringbuf::{HeapProd, HeapRb};

use crate::audio::decode::Decoder;
use crate::audio::processing::ProcessingChain;
use crate::audio::render::{Marker, MarkerKind, NO_HELD_POSITION, RenderCore, RenderShared};
use crate::audio::resample::StreamResampler;
use crate::audio::stream::{HttpStream, RETRY_MAX, RETRY_MIN};
use crate::audio::transition::{self, JoinKind, Transition, TraitsSlot};
use crate::error::{Error, Result};

/// Output channel count. The engine renders everything to stereo.
pub const OUTPUT_CHANNELS: usize = 2;
const MARKER_CAPACITY: usize = 64;
/// the held tail is written in pieces this long, so an interrupt loses none of it.
const RELEASE_CHUNK_FRAMES: usize = 4_096;
/// the rate assumed before any track has set one.
const DEFAULT_RATE: u32 = 48_000;
/// Start opening the next track this long before the current one ends (decode position).
const PRELOAD_LEAD_MS: u64 = 30_000;
/// With the ring full, wait for this share of it to drain before writing again, so the engine
/// writes in a few large batches instead of waking for every small gap.
const REFILL_SHARE: usize = 16;
/// The longest a full ring waits while playing and while paused. Both stay well under the
/// ring's own length.
const REFILL_WAIT_PLAYING: Duration = Duration::from_millis(100);
const REFILL_WAIT_PAUSED: Duration = Duration::from_secs(1);

/// How the output sample rate relates to the track's sample rate.
#[derive(Clone, Debug)]
pub enum OutputMode {
    /// Bit-perfect: the device follows the track. A rate change forces a device reopen.
    FollowTrack,
    /// Follow the track when the device supports its rate, otherwise resample to `fallback`.
    /// Ranges are inclusive.
    FollowWithin { ranges: Vec<(u32, u32)>, fallback: u32 },
    /// The device stays at one rate and tracks are resampled to it.
    FixedRate(u32),
}

impl OutputMode {
    /// Whether a stream already at `rate` may stay there under this mode.
    fn accepts(&self, rate: u32) -> bool {
        match self {
            OutputMode::FollowTrack => true,
            OutputMode::FixedRate(r) => *r == rate,
            OutputMode::FollowWithin { ranges, fallback } => *fallback == rate || ranges.iter().any(|(lo, hi)| (*lo..=*hi).contains(&rate)),
        }
    }

    fn rate_for(&self, source: u32) -> u32 {
        match self {
            OutputMode::FollowTrack => source,
            OutputMode::FixedRate(r) => *r,
            OutputMode::FollowWithin { ranges, fallback } => {
                if ranges.iter().any(|(lo, hi)| (*lo..=*hi).contains(&source)) { source } else { *fallback }
            }
        }
    }
}

pub struct EngineConfig {
    pub output: OutputMode,
    pub crossfade_frames: usize,
    /// Smart crossfade, where joins that continue an album in order play gapless.
    pub smart_joins: bool,
    /// Smart transitions: each join's length and place come from the songs' measured edges.
    pub smart_transitions: bool,
    pub ring_frames: usize,
    /// Stages applied to the final mix. Leave empty for bit-exact playback.
    pub processing: ProcessingChain,
}

impl EngineConfig {
    pub fn new(output: OutputMode) -> Self {
        Self { output, crossfade_frames: 0, smart_joins: false, smart_transitions: false, ring_frames: 48_000, processing: ProcessingChain::new() }
    }
}

/// Events the engine reports, in output frame positions.
#[derive(Clone, Debug)]
pub enum EngineEvent {
    TrackStart {
        track: usize,
        /// Output frame where this track begins, including the overlap when crossfading.
        output_frame: u64,
        output_rate: u32,
        /// The output rate changed here, so a real device must reopen.
        reopen: bool,
        /// Frames shared with the previous track when crossfading.
        overlap: usize,
        /// Position in the track where output begins, after a seek.
        start_ms: u64,
    },
    TrackEnd {
        track: usize,
        decode_errors: u64,
    },
    Error {
        track: usize,
        message: String,
    },
    /// The feed ran out and all audio has been written.
    QueueFinished,
    /// The server can't be reached. The engine stays on this track and keeps retrying.
    WaitingForServer { message: String },
    /// A wait for the server has ended, either way.
    WaitEnded,
}

/// Opens a track at `start_ms` and returns the decoder and where it really starts.
pub trait TrackOpener: Send + Sync {
    fn open(&self, start_ms: u64) -> Result<(Decoder, u64)>;

    /// Volume leveling gain for this track, applied first. 1.0 changes nothing.
    fn gain(&self) -> f32 {
        1.0
    }

    /// What this track is being delivered as, once it's open.
    fn delivered(&self) -> Option<crate::quality::Delivered> {
        None
    }
}

struct FileOpener(PathBuf);

impl TrackOpener for FileOpener {
    fn open(&self, start_ms: u64) -> Result<(Decoder, u64)> {
        let mut decoder = Decoder::open(&self.0)?;
        if start_ms > 0 {
            decoder.seek_ms(start_ms)?;
        }
        Ok((decoder, start_ms))
    }
}

#[derive(Clone)]
pub struct TrackRequest {
    /// Identifies this queue entry in events.
    pub token: usize,
    pub opener: Arc<dyn TrackOpener>,
    pub start_ms: u64,
    /// Total length if known, used to time the preload.
    pub duration_ms: Option<u64>,
    /// Where the song sits on its album, for Smart crossfade. `None` when unknown.
    pub album: Option<AlbumSpot>,
    /// the song's measured edges and facts for smart transitions, filled in the background.
    pub traits: Option<TraitsSlot>,
}

impl TrackRequest {
    /// a file on disk, for tests and tools.
    pub fn file(token: usize, path: PathBuf) -> Self {
        Self { token, opener: Arc::new(FileOpener(path)), start_ms: 0, duration_ms: None, album: None, traits: None }
    }
}

/// A song's album id, disc and track number as the server tags them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlbumSpot {
    pub album_id: String,
    pub disc: Option<i32>,
    pub track: Option<i32>,
}

impl AlbumSpot {
    /// True when this song is the next track on the same disc as `previous`, or the first track
    /// of the next disc. A missing track number never continues, and a missing disc is disc 1.
    pub fn continues(&self, previous: &AlbumSpot) -> bool {
        if self.album_id.is_empty() || self.album_id != previous.album_id {
            return false;
        }
        let (Some(track), Some(prev_track)) = (self.track, previous.track) else { return false };
        let (disc, prev_disc) = (self.disc.unwrap_or(1), previous.disc.unwrap_or(1));
        (disc == prev_disc && track == prev_track + 1) || (disc == prev_disc + 1 && track == 1)
    }
}

/// Supplies tracks to the engine.
pub trait TrackFeed: Send {
    /// The track after the current one, without moving to it.
    fn peek(&mut self) -> Option<TrackRequest>;
    /// Move to the next track and return it.
    fn advance(&mut self) -> Option<TrackRequest>;
    /// What follows the entry `token` now. None when the entry is gone, `Some(None)` when
    /// nothing follows it.
    fn following(&mut self, _token: usize) -> Option<Option<usize>> {
        None
    }
    /// Make `token` the decoder's place again, so [`Self::advance`] returns what follows it.
    /// False when it's gone.
    fn rewind_to(&mut self, _token: usize) -> bool {
        false
    }
}

struct VecFeed {
    queue: Vec<TrackRequest>,
    index: usize,
}

impl TrackFeed for VecFeed {
    fn peek(&mut self) -> Option<TrackRequest> {
        self.queue.get(self.index).cloned()
    }

    fn advance(&mut self) -> Option<TrackRequest> {
        let out = self.queue.get(self.index).cloned();
        if out.is_some() {
            self.index += 1;
        }
        out
    }
}

pub(crate) enum EngineCommand {
    Load(TrackRequest),
    Seek(u64),
    Stop,
    SetCrossfade(usize),
    /// Smart crossfade, where joins that continue an album play gapless.
    SetSmartJoins(bool),
    SetSmartTransitions(bool),
    SetOutputMode(OutputMode),
    /// Stop waiting out a backoff and try the server now.
    RetryNow,
    /// The queue changed. Audio decoded ahead that no longer follows the song being heard is
    /// cut at the join and replaced, so the join stays gapless.
    Realign,
    Exit,
    /// Kills the engine thread, for tests of what happens when it dies.
    #[cfg(test)]
    Crash,
}

/// Controls a running engine from any thread.
#[derive(Clone)]
pub struct EngineControl {
    tx: Sender<EngineCommand>,
    /// The stream the decode thread may be blocked on.
    reading: Arc<Mutex<Option<Arc<HttpStream>>>>,
}

impl EngineControl {
    /// Abandon the current track and play this one.
    pub fn load(&self, req: TrackRequest) {
        self.interrupt_read();
        let _ = self.tx.send(EngineCommand::Load(req));
    }

    pub fn seek(&self, ms: u64) {
        self.interrupt_read();
        let _ = self.tx.send(EngineCommand::Seek(ms));
    }

    pub fn stop(&self) {
        self.interrupt_read();
        let _ = self.tx.send(EngineCommand::Stop);
    }

    pub fn retry_now(&self) {
        let _ = self.tx.send(EngineCommand::RetryNow);
    }

    /// Report a queue change, so audio decoded ahead for the wrong song is replaced. Doesn't
    /// interrupt a read, since the song being heard may still be decoding.
    pub fn realign(&self) {
        let _ = self.tx.send(EngineCommand::Realign);
    }

    /// Wake the decode thread, which may be blocked in a read during a long outage.
    fn interrupt_read(&self) {
        if let Some(stream) = self.reading.lock().expect("reading").as_ref() {
            stream.interrupt();
        }
    }

    pub fn set_crossfade(&self, frames: usize) {
        let _ = self.tx.send(EngineCommand::SetCrossfade(frames));
    }

    /// With a crossfade set, play joins that continue the album in order gapless.
    pub fn set_smart_joins(&self, on: bool) {
        let _ = self.tx.send(EngineCommand::SetSmartJoins(on));
    }

    /// with a crossfade set, shape each join from the songs' measured edges.
    pub fn set_smart_transitions(&self, on: bool) {
        let _ = self.tx.send(EngineCommand::SetSmartTransitions(on));
    }

    pub fn set_output_mode(&self, mode: OutputMode) {
        let _ = self.tx.send(EngineCommand::SetOutputMode(mode));
    }

    pub(crate) fn shutdown(&self) {
        self.interrupt_read();
        let _ = self.tx.send(EngineCommand::Exit);
    }

    /// Make the engine thread panic, so tests can check that its death is reported.
    #[cfg(test)]
    pub(crate) fn crash(&self) {
        let _ = self.tx.send(EngineCommand::Crash);
    }
}

/// Marks the worker as finished however it ends, including by panic.
struct FinishedOnDrop(Arc<AtomicBool>);

impl Drop for FinishedOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// The parts handed to a player, built by `spawn_feed`.
pub struct EngineParts {
    pub core: RenderCore,
    pub shared: Arc<RenderShared>,
    pub events: Receiver<EngineEvent>,
    pub control: EngineControl,
    pub finished: Arc<AtomicBool>,
    pub worker: JoinHandle<()>,
}

/// Run an engine fed by `feed`, starting with `first` if given. With `exit_when_idle`, the
/// worker stops when the queue runs out instead of waiting for another command.
pub fn spawn_feed(feed: Box<dyn TrackFeed>, config: EngineConfig, first: Option<TrackRequest>, exit_when_idle: bool) -> EngineParts {
    let (prod, cons) = HeapRb::<f32>::new(config.ring_frames * OUTPUT_CHANNELS).split();
    let (marker_prod, marker_cons) = HeapRb::<Marker>::new(MARKER_CAPACITY).split();
    let (core, shared) = RenderCore::new(cons, marker_cons);
    let (tx, events) = channel();
    let (cmd_tx, cmd_rx) = channel();
    let finished = Arc::new(AtomicBool::new(false));
    let done = FinishedOnDrop(finished.clone());
    let worker_shared = shared.clone();
    let reading: Arc<Mutex<Option<Arc<HttpStream>>>> = Arc::default();
    let worker_reading = reading.clone();
    let worker = thread::Builder::new()
        .name("navia-engine".into())
        .spawn(move || {
            let _done = done;
            let mut w = Worker {
                prod,
                markers: marker_prod,
                shared: worker_shared,
                tx,
                cmds: cmd_rx,
                feed,
                mode: config.output,
                crossfade_frames: config.crossfade_frames,
                next_crossfade: None,
                smart_joins: config.smart_joins,
                smart_transitions: config.smart_transitions,
                last_spot: None,
                join_fades: false,
                join_continues: false,
                join_pending: false,
                held_owner: None,
                tail_traits: None,
                tail_gain: 1.0,
                chain: config.processing,
                written: 0,
                epoch: 0,
                held: VecDeque::new(),
                resampler: None,
                current_rate: None,
                preload: None,
                exit_when_idle,
                reading: worker_reading,
                retry_now: false,
                pending: None,
                drain_mark: None,
                drain_rate: 0.0,
                started: Vec::new(),
                decoding: None,
                decoding_started: false,
                run: 0,
                continuing: false,
                last_req: None,
            };
            w.run(first);
        })
        .expect("spawn engine thread");
    EngineParts { core, shared, events, control: EngineControl { tx: cmd_tx, reading }, finished, worker }
}

/// The output side of a headless engine, for tests and offline checks.
pub struct EngineOutput {
    core: RenderCore,
    shared: Arc<RenderShared>,
    finished: Arc<AtomicBool>,
    pub events: Receiver<EngineEvent>,
    worker: Option<JoinHandle<()>>,
    pub control: EngineControl,
}

impl EngineOutput {
    /// Fill `out` with interleaved stereo and return the samples written. A rate change applies
    /// at once, since there's no device to reopen.
    pub fn fill(&mut self, out: &mut [f32]) -> usize {
        let requested = self.shared.rate_request.swap(0, Ordering::AcqRel);
        if requested != 0 {
            self.core.set_stream_rate(requested);
        }
        let frames = self.core.render(out);
        frames * OUTPUT_CHANNELS
    }

    pub fn shared(&self) -> &Arc<RenderShared> {
        &self.shared
    }

    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }

    pub fn join(mut self) {
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

/// Play `queue` in order on a decoding thread.
pub fn spawn(queue: Vec<PathBuf>, config: EngineConfig) -> EngineOutput {
    spawn_with_albums(queue.into_iter().map(|p| (p, None)).collect(), config)
}

/// [`spawn`] with each file's place on its album, for Smart crossfade.
pub fn spawn_with_albums(queue: Vec<(PathBuf, Option<AlbumSpot>)>, config: EngineConfig) -> EngineOutput {
    let requests: Vec<TrackRequest> = queue
        .into_iter()
        .enumerate()
        .map(|(i, (p, album))| TrackRequest { album, ..TrackRequest::file(i, p) })
        .collect();
    let first = requests.first().cloned();
    let feed = VecFeed { queue: requests, index: 1 };
    output_from(spawn_feed(Box::new(feed), config, first, true))
}

/// A headless output around parts, for tests that also need to drive the engine.
pub fn output_from(parts: EngineParts) -> EngineOutput {
    EngineOutput { core: parts.core, shared: parts.shared, finished: parts.finished, events: parts.events, worker: Some(parts.worker), control: parts.control }
}

/// Why decoding stops on the current track. Only these reach the run loop.
enum Leave {
    Load(TrackRequest),
    /// Drop what was written from `from` on and carry on after the entry `keep`.
    Cut { from: u64, keep: usize },
    Stop,
    Exit,
}

/// What interrupts decoding. Seeks and resumes are handled inside [`Worker::play_track`], which
/// only returns a [`Leave`].
enum Interrupt {
    Leave(Leave),
    Seek(u64),
    /// The server went away under a stream that can't wait in place, so reopen the track here.
    Resume(u64),
}

/// Where the run loop goes after a [`Leave`].
enum Flow {
    Play(TrackRequest),
    Idle,
    Exit,
}

impl From<Leave> for Interrupt {
    fn from(leave: Leave) -> Self {
        Interrupt::Leave(leave)
    }
}

struct OpenJob {
    token: usize,
    start_ms: u64,
    rx: Receiver<Result<(Decoder, u64)>>,
}

fn spawn_open(req: &TrackRequest) -> OpenJob {
    let (tx, rx) = channel();
    let opener = req.opener.clone();
    let start_ms = req.start_ms;
    // If the thread can't start, the sender drops and the open fails like any other.
    let _ = thread::Builder::new().name("navia-open".into()).spawn(move || {
        let _ = tx.send(opener.open(start_ms));
    });
    OpenJob { token: req.token, start_ms, rx }
}

struct Worker {
    prod: HeapProd<f32>,
    markers: HeapProd<Marker>,
    shared: Arc<RenderShared>,
    tx: Sender<EngineEvent>,
    cmds: Receiver<EngineCommand>,
    feed: Box<dyn TrackFeed>,
    mode: OutputMode,
    crossfade_frames: usize,
    next_crossfade: Option<usize>,
    smart_joins: bool,
    /// Album place of the song whose tail is held, to judge the next join.
    last_spot: Option<AlbumSpot>,
    /// Whether the join into the track being decoded fades. Decided once per track.
    join_fades: bool,
    smart_transitions: bool,
    /// the track being decoded continues the held song's album in order.
    join_continues: bool,
    /// the join into the track being decoded is still to be shaped by its first `body`.
    join_pending: bool,
    /// the track whose audio the held tail is.
    held_owner: Option<usize>,
    /// measured edges of the song whose end is being held, and its leveling gain.
    tail_traits: Option<TraitsSlot>,
    tail_gain: f32,
    chain: ProcessingChain,
    /// Frames written to the ring buffer so far, including stale ones later flushed.
    written: u64,
    epoch: u64,
    /// Held-back end of the previous track, waiting to crossfade.
    held: VecDeque<f32>,
    resampler: Option<StreamResampler>,
    current_rate: Option<u32>,
    preload: Option<OpenJob>,
    exit_when_idle: bool,
    reading: Arc<Mutex<Option<Arc<HttpStream>>>>,
    retry_now: bool,
    /// A command that arrived while waiting. Checked before the channel.
    pending: Option<EngineCommand>,
    /// Frames heard by the output at a moment, to measure how fast it drains.
    drain_mark: Option<(Instant, u64)>,
    /// The fastest drain seen, in frames per second. A capture can run faster than real time.
    drain_rate: f64,
    /// Tracks started since the last flush, oldest first, to find where a cut goes.
    started: Vec<Started>,
    /// The entry being opened or decoded, or None while idle.
    decoding: Option<usize>,
    decoding_started: bool,
    /// Counts tracks begun, so a replay of the same entry is told apart from a seek.
    run: u64,
    /// The next track continues from audio already written, so its place isn't held.
    continuing: bool,
    /// The track last begun, whose end may still be playing after the queue ran out.
    last_req: Option<TrackRequest>,
}

#[derive(Clone, Copy, Debug)]
struct Started {
    token: usize,
    /// Output frame where it starts, including any crossfade overlap.
    frame: u64,
    rate: u32,
}

/// How many started tracks to remember. A few tracks fill the ring.
const STARTED_KEPT: usize = 16;

impl Worker {
    fn run(&mut self, first: Option<TrackRequest>) {
        let mut next = first;
        loop {
            self.decoding = None;
            let Some(req) = next.take().or_else(|| self.wait_for_load()) else { return };
            // Nothing is audible until this track's first frames arrive, so report its place.
            if !std::mem::take(&mut self.continuing) {
                self.hold_position(req.start_ms);
            }
            next = Some(req);
            while let Some(req) = next.take() {
                let leave = match self.play_track(req) {
                    Ok(Some(following)) => {
                        next = Some(following);
                        continue;
                    }
                    // The queue ran out. A load, seek or stop can still interrupt writing its
                    // end, and must be obeyed.
                    Ok(None) => match self.finish_queue() {
                        Ok(()) => continue,
                        Err(leave) => leave,
                    },
                    Err(leave) => leave,
                };
                match self.settle(leave) {
                    Flow::Play(r) => next = Some(r),
                    Flow::Idle => {}
                    Flow::Exit => return,
                }
            }
            if self.exit_when_idle {
                return;
            }
        }
    }

    /// Carry out a [`Leave`], including one that interrupts writing the queue's end.
    fn settle(&mut self, mut leave: Leave) -> Flow {
        loop {
            match leave {
                Leave::Load(r) => {
                    self.flush();
                    self.hold_position(r.start_ms);
                    return Flow::Play(r);
                }
                Leave::Cut { from, keep } => match self.cut_and_follow(from, keep) {
                    Some(r) => return Flow::Play(r),
                    None => match self.finish_queue() {
                        Ok(()) => return Flow::Idle,
                        Err(next) => leave = next,
                    },
                },
                Leave::Stop => {
                    self.flush();
                    self.send_marker(self.written, MarkerKind::QueueEnd);
                    return Flow::Idle;
                }
                Leave::Exit => return Flow::Exit,
            }
        }
    }

    /// Block until a track arrives. Returns None when the engine should exit.
    fn wait_for_load(&mut self) -> Option<TrackRequest> {
        if self.exit_when_idle {
            return None;
        }
        loop {
            let Some(cmd) = self.pending.take().or_else(|| self.cmds.recv().ok()) else { return None };
            match cmd {
                // The last song's end may still be in the ring. What's loaded now replaces it.
                EngineCommand::Load(r) => {
                    self.flush();
                    return Some(r);
                }
                // The queue ran out while its last song plays. Songs added since carry on from it.
                EngineCommand::Realign => {
                    if let Some((from, keep)) = self.realign_target() {
                        match self.cut_and_follow(from, keep) {
                            Some(r) => {
                                self.continuing = true;
                                return Some(r);
                            }
                            // Nothing follows the song being heard now, so the queue ends after it.
                            None => {
                                if let Err(leave) = self.finish_queue() {
                                    match self.settle(leave) {
                                        Flow::Play(r) => {
                                            self.continuing = true;
                                            return Some(r);
                                        }
                                        Flow::Idle => {}
                                        Flow::Exit => return None,
                                    }
                                }
                            }
                        }
                    }
                }
                EngineCommand::SetCrossfade(frames) => self.crossfade_frames = frames,
                EngineCommand::SetSmartJoins(on) => self.smart_joins = on,
                EngineCommand::SetSmartTransitions(on) => self.smart_transitions = on,
                EngineCommand::SetOutputMode(mode) => self.mode = mode,
                EngineCommand::Exit => return None,
                EngineCommand::Seek(_) | EngineCommand::Stop | EngineCommand::RetryNow => {}
                #[cfg(test)]
                EngineCommand::Crash => panic!("test crash"),
            }
        }
    }

    /// Sleep until a command arrives or `timeout` passes. The command is kept for the next
    /// [`Worker::poll`], so waiting never delays a seek or skip.
    fn wait_command(&mut self, timeout: Duration) {
        if self.pending.is_some() {
            return;
        }
        match self.cmds.recv_timeout(timeout) {
            Ok(cmd) => self.pending = Some(cmd),
            Err(RecvTimeoutError::Timeout) => {}
            // No sender is left. Keep the old pace rather than spin.
            Err(RecvTimeoutError::Disconnected) => thread::sleep(timeout),
        }
    }

    /// The latest pending interrupt. Non-interrupting commands are applied along the way.
    fn poll(&mut self) -> std::result::Result<(), Interrupt> {
        loop {
            let next = match self.pending.take() {
                Some(cmd) => Ok(cmd),
                None => self.cmds.try_recv(),
            };
            match next {
                Ok(EngineCommand::SetCrossfade(frames)) => self.next_crossfade = Some(frames),
                Ok(EngineCommand::SetSmartJoins(on)) => self.smart_joins = on,
                Ok(EngineCommand::SetSmartTransitions(on)) => self.smart_transitions = on,
                Ok(EngineCommand::SetOutputMode(mode)) => self.mode = mode,
                Ok(EngineCommand::RetryNow) => self.retry_now = true,
                Ok(EngineCommand::Realign) => {
                    if let Some((from, keep)) = self.realign_target() {
                        return Err(Leave::Cut { from, keep }.into());
                    }
                }
                Ok(EngineCommand::Load(r)) => return Err(Leave::Load(r).into()),
                Ok(EngineCommand::Seek(ms)) => return Err(Interrupt::Seek(ms)),
                Ok(EngineCommand::Stop) => return Err(Leave::Stop.into()),
                Ok(EngineCommand::Exit) => return Err(Leave::Exit.into()),
                #[cfg(test)]
                Ok(EngineCommand::Crash) => panic!("test crash"),
                Err(_) => return Ok(()),
            }
        }
    }

    /// A canceled stream always comes with a command. Wait briefly for it, so the canceled read
    /// isn't mistaken for the track ending.
    fn wait_for_interrupt(&mut self) -> std::result::Result<(), Interrupt> {
        let until = Instant::now() + Duration::from_secs(3);
        loop {
            self.poll()?;
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(());
            }
            self.wait_command(left);
        }
    }

    /// Discard everything not yet heard.
    fn flush(&mut self) {
        self.held.clear();
        self.held_owner = None;
        self.resampler = None;
        self.preload = None;
        self.chain.reset();
        self.started.clear();
        self.epoch += 1;
        self.shared.cut_from.store(crate::audio::render::NO_CUT, Ordering::Release);
        self.shared.flush_until.store(self.written, Ordering::Release);
        self.shared.epoch.store(self.epoch, Ordering::Release);
        self.shared.held_position.store(NO_HELD_POSITION, Ordering::Release);
    }

    /// Where to cut after a queue change, as (first frame to drop, entry to carry on after).
    /// None when what was decoded ahead still fits. A stale preload is dropped either way.
    fn realign_target(&mut self) -> Option<(u64, usize)> {
        let read = self.shared.read_frames.load(Ordering::Acquire);
        let at = self.started.iter().rposition(|s| s.frame <= read)?;
        if self.preload.as_ref().is_some_and(|job| Some(job.token) != self.feed.peek().map(|r| r.token)) {
            self.preload = None;
        }
        // Cut at the first song decoded ahead that no longer follows the one before it. A change
        // further down, like Shuffle Queue, can leave the next one in place.
        let mut prev = self.started[at];
        for next in self.started[at + 1..].iter().copied() {
            let expected = self.feed.following(prev.token)?;
            if Some(next.token) != expected {
                return Some((next.frame, prev.token));
            }
            prev = next;
        }
        let expected = self.feed.following(prev.token)?;
        // Nothing after `prev` has started. The decoder is on it, opening what follows, or idle.
        let ahead = match self.decoding {
            Some(t) if t == prev.token && self.decoding_started => return None,
            Some(t) => Some(t),
            None => None,
        };
        match ahead {
            Some(token) if Some(token) == expected => None,
            Some(_) => Some((self.written, prev.token)),
            None if expected.is_some() => Some((self.written, prev.token)),
            None => None,
        }
    }

    /// Drop what was written from `from` on, keeping the song heard to its end, and return what
    /// follows `keep` now. With a crossfade, the song heard loses the part already mixed.
    fn cut_and_follow(&mut self, from: u64, keep: usize) -> Option<TrackRequest> {
        let kept = self.started.iter().rposition(|s| s.token == keep);
        if let Some(k) = kept {
            self.current_rate = Some(self.started[k].rate);
            self.started.truncate(k + 1);
        }
        // the kept song's held end isn't written yet; it stays to join what follows now.
        if !(self.held_owner == Some(keep) && from >= self.written) {
            self.held.clear();
        }
        self.resampler = None;
        self.preload = None;
        self.chain.reset();
        self.epoch += 1;
        self.shared.cut_from.store(from, Ordering::Release);
        self.shared.flush_until.store(self.written, Ordering::Release);
        self.shared.epoch.store(self.epoch, Ordering::Release);
        if !self.feed.rewind_to(keep) {
            return None;
        }
        self.feed.advance()
    }

    /// Report this position until the track's first audio arrives, since a load or seek can
    /// wait on the server for a long time.
    fn hold_position(&self, ms: u64) {
        self.shared.held_position.store(ms, Ordering::Release);
    }

    fn send_marker(&mut self, frame: u64, kind: MarkerKind) {
        let mut marker = Marker { frame, epoch: self.epoch, kind };
        while let Err(m) = self.markers.try_push(marker) {
            marker = m;
            thread::sleep(Duration::from_millis(1));
        }
    }

    /// Open a track. While the server is unreachable this retries with backoff and the queue
    /// keeps its place. Any command interrupts the wait.
    fn open(&mut self, req: &TrackRequest) -> std::result::Result<Option<(Decoder, u64)>, Interrupt> {
        let job = match self.preload.take() {
            Some(job) if job.token == req.token && job.start_ms == req.start_ms => job,
            _ => spawn_open(req),
        };
        let mut waiting = false;
        let result = self.open_with_retries(job, req, &mut waiting);
        if waiting {
            let _ = self.tx.send(EngineEvent::WaitEnded);
        }
        result
    }

    fn open_with_retries(&mut self, mut job: OpenJob, req: &TrackRequest, waiting: &mut bool) -> std::result::Result<Option<(Decoder, u64)>, Interrupt> {
        let mut delay = RETRY_MIN;
        loop {
            self.poll()?;
            match job.rx.recv_timeout(Duration::from_millis(5)) {
                Ok(Ok(opened)) => return Ok(Some(opened)),
                Ok(Err(Error::Cancelled)) => {
                    self.wait_for_interrupt()?;
                    return Ok(None);
                }
                Ok(Err(e)) if e.is_outage() => {
                    if !*waiting {
                        *waiting = true;
                        let _ = self.tx.send(EngineEvent::WaitingForServer { message: e.to_string() });
                    }
                    self.backoff(delay)?;
                    delay = (delay * 2).min(RETRY_MAX);
                    job = spawn_open(req);
                }
                Ok(Err(e)) => {
                    let _ = self.tx.send(EngineEvent::Error { track: req.token, message: e.to_string() });
                    return Ok(None);
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    let _ = self.tx.send(EngineEvent::Error { track: req.token, message: "open failed".to_string() });
                    return Ok(None);
                }
            }
        }
    }

    /// Wait out a retry delay, cut short by a command.
    fn backoff(&mut self, delay: Duration) -> std::result::Result<(), Interrupt> {
        self.retry_now = false;
        let until = Instant::now() + delay;
        loop {
            self.poll()?;
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() || self.retry_now {
                return Ok(());
            }
            self.wait_command(left);
        }
    }

    /// Open `req` at `start_ms`. A seek during the open restarts it at the new place, so only
    /// a load, stop or exit leaves. Returns None when the track can't be played.
    fn open_track(&mut self, req: &TrackRequest, mut start_ms: u64) -> std::result::Result<Option<(Decoder, u64)>, Leave> {
        loop {
            match self.open(&TrackRequest { start_ms, ..req.clone() }) {
                Ok(opened) => return Ok(opened),
                Err(Interrupt::Leave(leave)) => return Err(leave),
                Err(Interrupt::Seek(ms)) => {
                    self.flush();
                    self.hold_position(ms);
                    start_ms = ms;
                }
                Err(Interrupt::Resume(ms)) => start_ms = ms,
            }
        }
    }

    fn set_reading(&self, stream: Option<Arc<HttpStream>>) {
        *self.reading.lock().expect("reading") = stream;
    }

    /// Play one track to its end. Returns the next request, or None when the feed is done.
    fn play_track(&mut self, req: TrackRequest) -> std::result::Result<Option<TrackRequest>, Leave> {
        self.decoding = Some(req.token);
        self.decoding_started = false;
        self.run += 1;
        self.last_req = Some(req.clone());
        let Some((decoder, base_ms)) = self.open_track(&req, req.start_ms)? else { return Ok(self.feed.advance()) };
        self.set_reading(decoder.stream());
        if let Some(frames) = self.next_crossfade.take() {
            self.crossfade_frames = frames;
        }
        let continues = matches!((&req.album, &self.last_spot), (Some(spot), Some(last)) if spot.continues(last));
        self.join_continues = continues;
        self.join_pending = true;
        self.join_fades = if self.shaping() { self.plan_join(&req).kind != JoinKind::Gapless } else { self.crossfade_frames > 0 && !(self.smart_joins && continues) };
        let finished = self.decode_track(decoder, base_ms, &req);
        self.set_reading(None);
        let decode_errors = finished?;
        let _ = self.tx.send(EngineEvent::TrackEnd { track: req.token, decode_errors });
        Ok(self.feed.advance())
    }

    /// Decode the opened track to its end, through seeks and waits for the server. Returns its
    /// decode error count.
    fn decode_track(&mut self, mut decoder: Decoder, mut base_ms: u64, req: &TrackRequest) -> std::result::Result<u64, Leave> {
        loop {
            match self.body(&mut decoder, req, base_ms) {
                Ok(()) => return Ok(decoder.decode_errors),
                Err(Interrupt::Leave(leave)) => return Err(leave),
                Err(Interrupt::Resume(ms)) => match self.open_track(req, ms)? {
                    // The audio already written keeps playing. Carry on from where the decoder got.
                    Some((reopened, at)) => {
                        decoder = reopened;
                        base_ms = at;
                        self.set_reading(decoder.stream());
                    }
                    None => return Ok(decoder.decode_errors),
                },
                Err(Interrupt::Seek(ms)) => match self.seek_track(&mut decoder, req, ms)? {
                    Some(at) => base_ms = at,
                    None => return Ok(decoder.decode_errors),
                },
            }
        }
    }

    /// Seek within the open track. During an outage the seek can block, and the next command
    /// interrupts it. Returns the new position, or None when the seek failed for good.
    fn seek_track(&mut self, decoder: &mut Decoder, req: &TrackRequest, mut ms: u64) -> std::result::Result<Option<u64>, Leave> {
        loop {
            self.flush();
            self.hold_position(ms);
            if let Some(stream) = decoder.stream() {
                stream.clear_interrupt();
            }
            if !decoder.seekable {
                // Live transcodes can't seek, so ask the server to start at this offset.
                return Err(Leave::Load(TrackRequest { start_ms: ms, ..req.clone() }));
            }
            match decoder.seek_ms(ms) {
                Ok(()) => return Ok(Some(ms)),
                Err(Error::Cancelled) => match self.wait_for_interrupt() {
                    Err(Interrupt::Leave(leave)) => return Err(leave),
                    Err(Interrupt::Seek(next)) | Err(Interrupt::Resume(next)) => ms = next,
                    // Interrupted with nothing to follow, so reopen the track at the target.
                    Ok(()) => return Err(Leave::Load(TrackRequest { start_ms: ms, ..req.clone() })),
                },
                // The stream can't wait in place. Reopening at this offset does wait.
                Err(e) if e.is_outage() => return Err(Leave::Load(TrackRequest { start_ms: ms, ..req.clone() })),
                Err(e) => {
                    let _ = self.tx.send(EngineEvent::Error { track: req.token, message: e.to_string() });
                    return Ok(None);
                }
            }
        }
    }

    /// Decode and emit one track from its current position.
    fn body(&mut self, decoder: &mut Decoder, req: &TrackRequest, base_ms: u64) -> std::result::Result<(), Interrupt> {
        let source_rate = decoder.sample_rate;
        // a join that fades into a held tail keeps the device at its rate and resamples the
        // incoming track to it; a fresh start or a gapless join gets a reopen when the track wants
        // a new rate. a rate the output mode no longer allows (another device since) is never kept.
        let fades_into_tail = self.join_fades && !self.held.is_empty();
        let output_rate = match (fades_into_tail, self.current_rate) {
            (true, Some(current)) if self.mode.accepts(current) => current,
            _ => self.mode.rate_for(source_rate),
        };
        let reopen = self.current_rate.is_some_and(|r| r != output_rate);

        // A source rate change ends the resampler run. A crossfade keeps resampler state per
        // track so each tail can be flushed exactly.
        let run_ends = self.resampler.as_ref().is_some_and(|r| r.source_rate != source_rate || r.target_rate != output_rate);
        if run_ends || (self.join_fades && self.resampler.is_some()) {
            let mut sink = Vec::new();
            self.resampler.take().expect("checked").finish(&mut sink);
            self.emit_with_hold(&sink)?;
        }
        // A join that doesn't fade plays the held tail as is, then this track right after it.
        if reopen || !self.join_fades {
            let held: Vec<f32> = std::mem::take(&mut self.held).into();
            self.push_all(&held)?;
        }
        let plan = (std::mem::take(&mut self.join_pending) && self.shaping() && !self.held.is_empty()).then(|| self.plan_join(req));
        if let Some(p) = &plan {
            let id = |slot: Option<&TraitsSlot>| slot.and_then(|s| s.get()).map_or("unmeasured".to_string(), |t| t.song_id.clone());
            tracing::info!(target: "navia_core::mix", "{} -> {}: {p}", id(self.tail_traits.as_ref()), id(req.traits.as_ref()));
        }
        let mut head_target = self.held.len() / OUTPUT_CHANNELS;
        let mut skip = 0usize;
        if let Some(p) = &plan {
            let rate = self.current_rate.unwrap_or(DEFAULT_RATE);
            let start = ms_to_frames(p.start_before_ms, rate).min(head_target);
            let overlap = ms_to_frames(p.overlap_ms, rate).min(start);
            // written before this song starts, so a queue change meanwhile keeps the held song's end.
            self.release_held(head_target - start)?;
            // the outgoing song has faded out by the overlap's end; the rest is never heard.
            self.held.truncate(overlap * OUTPUT_CHANNELS);
            head_target = overlap;
            skip = ms_to_frames(p.skip_ms, rate);
        }
        self.tail_traits = req.traits.clone();
        self.tail_gain = req.opener.gain();
        self.last_spot = req.album.clone();
        if source_rate != output_rate && self.resampler.is_none() {
            match StreamResampler::new(source_rate, output_rate) {
                Ok(r) => self.resampler = Some(r),
                Err(e) => {
                    let _ = self.tx.send(EngineEvent::Error { track: req.token, message: e.to_string() });
                    return Ok(());
                }
            }
        }
        if self.current_rate != Some(output_rate) {
            self.send_marker(self.written, MarkerKind::RateChange { rate: output_rate });
        }
        self.current_rate = Some(output_rate);
        self.chain.set_output(output_rate, OUTPUT_CHANNELS);

        let start_ms = base_ms + plan.as_ref().map_or(0, |p| u64::from(p.skip_ms));
        let held_frames = self.held.len() / OUTPUT_CHANNELS;
        // Start of this track in output frames, excluding the crossfade overlap.
        let mut start_frame = self.written + held_frames as u64;
        if let Some(r) = self.resampler.as_ref() {
            start_frame += r.pending_output();
        }

        let mut block = Vec::new();
        let mut head: Vec<f32> = Vec::with_capacity(head_target * OUTPUT_CHANNELS);
        let mut started = false;
        let mut out = Vec::new();
        let mut decoded_frames = 0u64;
        let position_ms = |frames: u64| base_ms + frames * 1000 / u64::from(source_rate);
        let total_ms = decoder.total_frames().map(|t| t * 1000 / u64::from(source_rate)).or(req.duration_ms);

        loop {
            self.poll()?;
            match decoder.next_block(&mut block) {
                Ok(true) => {}
                Ok(false) => break,
                Err(Error::Cancelled) => {
                    self.wait_for_interrupt()?;
                    break;
                }
                Err(e) if e.is_outage() => return Err(Interrupt::Resume(position_ms(decoded_frames))),
                Err(e) => {
                    let _ = self.tx.send(EngineEvent::Error { track: req.token, message: e.to_string() });
                    break;
                }
            }
            decoded_frames += (block.len() / OUTPUT_CHANNELS) as u64;
            self.maybe_preload(position_ms(decoded_frames), total_ms);
            // Per-track gain, applied before mixing so it changes exactly at a gapless join.
            // At 1.0 the samples aren't touched.
            let gain = req.opener.gain();
            if gain != 1.0 {
                for sample in block.iter_mut() {
                    *sample *= gain;
                }
            }
            out.clear();
            match self.resampler.as_mut() {
                Some(r) => r.push(&block, &mut out),
                None => out.extend_from_slice(&block),
            }
            let mut data = &out[..];
            if skip > 0 {
                let take = (skip * OUTPUT_CHANNELS).min(data.len());
                data = &data[take..];
                skip -= take / OUTPUT_CHANNELS;
                if data.is_empty() {
                    continue;
                }
            }
            if head_target > 0 && head.len() / 2 < head_target {
                let take = (head_target * 2 - head.len()).min(data.len());
                head.extend_from_slice(&data[..take]);
                data = &data[take..];
                if head.len() / 2 < head_target {
                    continue;
                }
                self.signal_start(req.token, start_frame - head_target as u64, output_rate, reopen, head_target, start_ms);
                started = true;
                self.mix_overlap(&head, plan.as_ref())?;
                head_target = 0;
            } else if !started && head_target == 0 {
                self.signal_start(req.token, start_frame, output_rate, reopen, 0, start_ms);
                started = true;
            }
            self.emit_with_hold(data)?;
        }

        // The track ended while collecting the crossfade head, so it's shorter than the fade.
        if head_target > 0 && !head.is_empty() {
            let overlap = head.len() / 2;
            self.signal_start(req.token, start_frame - overlap as u64, output_rate, reopen, overlap, start_ms);
            started = true;
            self.mix_overlap(&head, plan.as_ref())?;
        }
        if !started {
            self.signal_start(req.token, start_frame, output_rate, reopen, 0, start_ms);
        }
        // Without a crossfade there is nothing to hold; the resampler persists for gapless. With
        // Smart the next join may be gapless, so the run ends at the next track's start instead,
        // once it's known whether that join fades.
        if self.crossfade_frames > 0 && !self.smart_joins && !self.smart_transitions && let Some(mut r) = self.resampler.take() {
            let mut sink = Vec::new();
            r.finish(&mut sink);
            self.emit_with_hold(&sink)?;
        }
        Ok(())
    }

    fn signal_start(&mut self, token: usize, frame: u64, rate: u32, reopen: bool, overlap: usize, start_ms: u64) {
        self.held_owner = Some(token);
        if self.started.len() >= STARTED_KEPT {
            self.started.remove(0);
        }
        self.started.push(Started { token, frame, rate });
        self.decoding_started = true;
        let _ = self.tx.send(EngineEvent::TrackStart { track: token, output_frame: frame, output_rate: rate, reopen, overlap, start_ms });
        self.send_marker(frame, MarkerKind::TrackStart { token, base_ms: start_ms, rate, run: self.run });
    }

    /// Start opening the next track shortly before this one finishes decoding. A stale preload
    /// is replaced.
    fn maybe_preload(&mut self, decoded_ms: u64, total_ms: Option<u64>) {
        let due = match total_ms {
            Some(total) => total.saturating_sub(decoded_ms) <= PRELOAD_LEAD_MS,
            None => false,
        };
        if !due {
            return;
        }
        let next = self.feed.peek();
        match (&self.preload, &next) {
            (Some(job), Some(n)) if job.token == n.token => {}
            (None, None) => {}
            _ => self.preload = next.as_ref().map(spawn_open),
        }
    }

    /// Write what the resampler and a crossfade still hold of the last track. That can mean
    /// seconds of waiting for room, so a command that arrives comes back as a [`Leave`].
    fn finish_queue(&mut self) -> std::result::Result<(), Leave> {
        self.shared.held_position.store(NO_HELD_POSITION, Ordering::Release);
        if let Some(mut r) = self.resampler.take() {
            let mut sink = Vec::new();
            r.finish(&mut sink);
            if let Err(i) = self.emit_with_hold(&sink) {
                return Err(self.finish_interrupted(i));
            }
        }
        let held: Vec<f32> = std::mem::take(&mut self.held).into();
        // The marker goes first so the output never sees the ring run dry without knowing why.
        self.send_marker(self.written + (held.len() / OUTPUT_CHANNELS) as u64, MarkerKind::QueueEnd);
        if let Err(i) = self.push_all(&held) {
            return Err(self.finish_interrupted(i));
        }
        let _ = self.tx.send(EngineEvent::QueueFinished);
        Ok(())
    }

    /// What an interrupt while writing the queue's end means. A seek reopens the last track at
    /// that place.
    fn finish_interrupted(&self, interrupt: Interrupt) -> Leave {
        match interrupt {
            Interrupt::Leave(leave) => leave,
            Interrupt::Seek(ms) | Interrupt::Resume(ms) => match &self.last_req {
                Some(req) => Leave::Load(TrackRequest { start_ms: ms, ..req.clone() }),
                None => Leave::Stop,
            },
        }
    }

    /// mix the held tail with `head`, then emit the result. a smart transition's `plan` shapes the mix.
    fn mix_overlap(&mut self, head: &[f32], plan: Option<&Transition>) -> std::result::Result<(), Interrupt> {
        let frames = head.len() / 2;
        let tail: Vec<f32> = std::mem::take(&mut self.held).into();
        let (pre, overlap_tail) = tail.split_at(tail.len() - head.len());
        self.push_all(pre)?;
        if let Some(plan) = plan {
            let mixed = transition::mix(overlap_tail, head, self.current_rate.unwrap_or(DEFAULT_RATE), plan);
            return self.push_all(&mixed);
        }
        let mut mixed = Vec::with_capacity(head.len());
        for k in 0..frames {
            let t = (k as f32 + 0.5) / frames as f32;
            let (gain_out, gain_in) = crossfade_gains(t);
            mixed.push(overlap_tail[k * 2] * gain_out + head[k * 2] * gain_in);
            mixed.push(overlap_tail[k * 2 + 1] * gain_out + head[k * 2 + 1] * gain_in);
        }
        self.push_all(&mixed)
    }

    fn emit_with_hold(&mut self, data: &[f32]) -> std::result::Result<(), Interrupt> {
        let hold = self.hold_frames() * OUTPUT_CHANNELS;
        if hold == 0 {
            return self.push_all(data);
        }
        self.held.extend(data.iter().copied());
        if self.held.len() > hold {
            let excess = self.held.len() - hold;
            let release: Vec<f32> = self.held.drain(..excess).collect();
            self.push_all(&release)?;
        }
        Ok(())
    }

    /// write the first `frames` of the held tail, dropping from it only what was written, so an
    /// interrupt leaves the rest held.
    fn release_held(&mut self, frames: usize) -> std::result::Result<(), Interrupt> {
        let mut left = frames;
        while left > 0 {
            let n = left.min(RELEASE_CHUNK_FRAMES);
            let chunk: Vec<f32> = self.held.range(..n * OUTPUT_CHANNELS).copied().collect();
            let before = self.written;
            let pushed = self.push_all(&chunk);
            let done = (self.written - before) as usize;
            self.held.drain(..done * OUTPUT_CHANNELS);
            pushed?;
            left -= done;
        }
        Ok(())
    }

    /// whether joins are shaped from measured edges: smart transitions with a crossfade set.
    fn shaping(&self) -> bool {
        self.smart_transitions && self.crossfade_frames > 0
    }

    /// how much of the song being decoded to hold back for its join: the crossfade, or with
    /// smart transitions as much as its measured end may need.
    fn hold_frames(&self) -> usize {
        let edges = self.tail_traits.as_ref().and_then(|slot| slot.get()).and_then(|t| t.edges.as_ref());
        match edges {
            Some(edges) if self.shaping() => {
                let ms = transition::hold_ms(edges).min(crate::audio::edges::EDGE_WINDOW_MS as u32);
                self.crossfade_frames.max(ms_to_frames(ms, self.current_rate.unwrap_or(DEFAULT_RATE)))
            }
            _ => self.crossfade_frames,
        }
    }

    /// the smart transition from the held song into `req`.
    fn plan_join(&self, req: &TrackRequest) -> Transition {
        let rate = self.current_rate.unwrap_or(DEFAULT_RATE);
        let out = self.tail_traits.as_ref().and_then(|slot| slot.get()).cloned();
        let inc = req.traits.as_ref().and_then(|slot| slot.get()).cloned();
        transition::plan(transition::Join {
            out: out.as_deref(),
            inc: inc.as_deref(),
            out_gain: self.tail_gain,
            in_gain: req.opener.gain(),
            continues: self.join_continues,
            plain_ms: frames_to_ms(self.crossfade_frames, rate),
            held_ms: frames_to_ms(self.held.len() / OUTPUT_CHANNELS, rate),
        })
    }

    /// How long to wait for room in a full ring before trying again: until `needed` frames
    /// (at least a share of the ring) have drained at the output rate, measured or nominal,
    /// whichever is faster. Paused, nothing drains, so it waits longer. A command ends any wait at once, and the cap stays well under the
    /// ring's length, so the output never runs dry while this sleeps, even right after a resume.
    fn full_ring_wait(&mut self, needed: usize) -> Duration {
        // After a flush the ring holds frames the output is about to skip, which takes one
        // callback. Check often, since a seek or skip waits on it.
        if self.shared.read_frames.load(Ordering::Acquire) < self.shared.flush_until.load(Ordering::Acquire)
            && self.shared.cut_from.load(Ordering::Acquire) == crate::audio::render::NO_CUT
        {
            return Duration::from_millis(1);
        }
        let heard = self.shared.heard_frames.load(Ordering::Acquire);
        let now = Instant::now();
        if let Some((then, before)) = self.drain_mark {
            let spent = now.duration_since(then);
            if spent >= Duration::from_millis(20) && heard >= before {
                self.drain_rate = self.drain_rate.max((heard - before) as f64 / spent.as_secs_f64());
            }
        }
        self.drain_mark = Some((now, heard));
        let capacity = self.prod.capacity().get() / OUTPUT_CHANNELS;
        let nominal = f64::from(self.current_rate.unwrap_or(DEFAULT_RATE).max(1));
        let rate = self.drain_rate.max(nominal) as u64;
        let frames = needed.max(capacity / REFILL_SHARE).min(capacity / 2) as u64;
        let ring = Duration::from_micros(capacity as u64 * 1_000_000 / rate);
        let (wait, cap) = if self.shared.paused.load(Ordering::Acquire) {
            (REFILL_WAIT_PAUSED, REFILL_WAIT_PAUSED.min(ring / 2))
        } else {
            (Duration::from_micros(frames * 1_000_000 / rate), REFILL_WAIT_PLAYING.min(ring / 4))
        };
        wait.min(cap).max(Duration::from_millis(1))
    }

    /// Run the processing chain over `data` and push it to the ring. Commands are handled while
    /// the ring is full, so seeks never wait for it to drain.
    fn push_all(&mut self, data: &[f32]) -> std::result::Result<(), Interrupt> {
        let mut pending = Vec::new();
        let processed: &[f32] = if self.chain.is_bypassed() {
            data
        } else {
            pending.extend_from_slice(self.chain.apply(data));
            &pending
        };
        let mut rest = processed;
        while !rest.is_empty() {
            let n = self.prod.push_slice(rest);
            rest = &rest[n..];
            self.written += (n / OUTPUT_CHANNELS) as u64;
            if !rest.is_empty() {
                if let Err(i) = self.poll() {
                    return Err(i);
                }
                let needed = (rest.len() / OUTPUT_CHANNELS).saturating_sub(self.prod.vacant_len() / OUTPUT_CHANNELS);
                let wait = self.full_ring_wait(needed);
                self.wait_command(wait);
            }
        }
        Ok(())
    }
}

fn ms_to_frames(ms: u32, rate: u32) -> usize {
    (u64::from(ms) * u64::from(rate) / 1000) as usize
}

fn frames_to_ms(frames: usize, rate: u32) -> u32 {
    (frames as u64 * 1000 / u64::from(rate.max(1))) as u32
}

/// Equal-power gains at position `t` in 0..1, as (outgoing, incoming).
pub fn crossfade_gains(t: f32) -> (f32, f32) {
    let angle = t * std::f32::consts::FRAC_PI_2;
    (angle.cos(), angle.sin())
}

