//! Linux output through the sound server's pulse protocol, for PipeWire or PulseAudio, in pure Rust.
//! Navia uses its own stream instead of cpal's pulse host so it can name itself and the song in the
//! desktop mixer, let the server hold the buffer so a stalled app doesn't crackle, and count underruns.
//! Each stream has its own connection, since the `pulseaudio` crate's `Client` names streams by the
//! wrong index and drops underflow events.
//!
//! "System default" is a stream with no target, which the server moves when the default changes.

use std::ffi::CString;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use std::collections::HashMap;
use std::io::{BufReader, Read, Write};
use std::os::unix::net::UnixStream;

use futures_executor::block_on;
use pulseaudio::Client;
use pulseaudio::protocol::{self, Command, Prop};

use crate::audio::engine::OUTPUT_CHANNELS;
use crate::audio::output::{DeviceCaps, DeviceInfo, OutputHandle, heard_latency_frames, stall_point};
use crate::audio::render::{RenderCore, RenderShared};
use crate::error::{Error, Result};

/// Device id prefix for sinks, followed by the sink name, which survives reboots and replugs.
/// These are the ids cpal's pulse host uses, so saved choices carry over.
pub const ID_PREFIX: &str = "pulseaudio:";

/// What the server keeps queued for Navia (`tlength`). Its real-time thread holds this queue, so
/// Navia's threads can stall briefly without a gap. Pause, seek and volume are heard this much later.
pub const BUFFER_MS: u32 = 100;

/// The smallest request the server makes (`minreq`). A stall just before a refill still has
/// `BUFFER_MS` minus one request ahead. On PipeWire 1.6, 60 ms stalls caused no underruns, and
/// half the buffer as `node.latency` never pulls the desktop's quantum below its 1024 default.
pub const REQUEST_MS: u32 = 25;

/// The icon the desktop shows beside the stream, named after the app ID as the Linux packages install it.
pub const ICON_NAME: &str = crate::client::APP_ID;

/// How long a connect or a round trip may take before the server counts as stuck.
const SERVER_TIMEOUT: Duration = Duration::from_secs(2);

/// How often the stream's timing is read when nothing wakes the reader sooner.
const TIMING_INTERVAL: Duration = Duration::from_millis(100);

/// Bytes per frame: stereo 32-bit float.
const FRAME_BYTES: usize = OUTPUT_CHANNELS * 4;

fn server_err(context: &str, e: impl std::fmt::Display) -> Error {
    Error::Audio(format!("{context}: {e}"))
}

/// A connection to the sound server, named as Navia.
pub struct PulseServer {
    client: Client,
}

impl PulseServer {
    /// Connects when a server socket exists and answers in time.
    /// A stuck server leaves one thread behind, since connect has no timeout of its own.
    pub fn connect() -> Option<Self> {
        pulseaudio::socket_path_from_env()?;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("navia-pulse-connect".into())
            .spawn(move || {
                let name = CString::new(crate::client::APP_DISPLAY_NAME).expect("app name");
                let client = (|| {
                    let path = pulseaudio::socket_path_from_env().ok_or(pulseaudio::ClientError::ServerUnavailable)?;
                    Client::new_unix(name, UnixStream::connect(path)?, Some(cookie()))
                })();
                let _ = tx.send(client);
            })
            .ok()?;
        match rx.recv_timeout(SERVER_TIMEOUT) {
            Ok(Ok(client)) => Some(Self { client }),
            Ok(Err(e)) => {
                tracing::info!("audio: no pulse server ({e}); using ALSA");
                None
            }
            Err(_) => {
                tracing::warn!("audio: the pulse server didn't answer; using ALSA");
                None
            }
        }
    }

    /// The server's sinks, by their desktop names, with the default marked.
    pub fn list(&self) -> Result<Vec<DeviceInfo>> {
        let default = block_on(self.client.server_info()).map_err(|e| server_err("server info", e))?.default_sink_name;
        let sinks = block_on(self.client.list_sinks()).map_err(|e| server_err("list sinks", e))?;
        Ok(sinks
            .into_iter()
            .map(|s| {
                let name = s.name.to_string_lossy().into_owned();
                let shown = s.description.as_ref().map(|d| d.to_string_lossy().into_owned()).filter(|d| !d.is_empty()).unwrap_or_else(|| name.clone());
                DeviceInfo { is_default: default.as_ref() == Some(&s.name), id: format!("{ID_PREFIX}{name}"), name: shown }
            })
            .collect())
    }

    /// The sink an id names, or the default sink for `None`.
    fn sink(&self, device: Option<&str>) -> Result<protocol::SinkInfo> {
        let name = match device {
            Some(id) => CString::new(id.strip_prefix(ID_PREFIX).unwrap_or(id)).map_err(|e| server_err("sink name", e))?,
            None => protocol::DEFAULT_SINK.to_owned(),
        };
        block_on(self.client.sink_info_by_name(name)).map_err(|e| server_err("sink", e))
    }

    /// Any rate plays, since the server converts. The sink's own rate is the default.
    pub fn caps(&self, device: Option<&str>) -> Result<DeviceCaps> {
        let sink = self.sink(device)?;
        Ok(DeviceCaps { rates: vec![(1, protocol::sample_spec::MAX_RATE)], default_rate: sink.sample_spec.sample_rate })
    }

    /// Plays on the sink `device` names, or the system default for `None` or a sink that's gone.
    pub fn open(&self, device: Option<&str>, rate: u32, core: Arc<Mutex<RenderCore>>) -> Result<PulseHandle> {
        // `PULSE_SINK` or `PIPEWIRE_NODE` stands in for the default, as libpulse does. Tests aim at
        // a null sink that way, so a missing one fails instead of playing on the real default.
        if device.is_none()
            && let Some(name) = env_sink()
        {
            let sink = self.sink(Some(&name)).map_err(|e| Error::Audio(format!("the sink named in the environment ({name}): {e}")))?;
            return PulseHandle::open(Some(sink.index), rate, core);
        }
        let sink_index = match device {
            Some(id) => match self.sink(Some(id)) {
                Ok(sink) => Some(sink.index),
                Err(e) => {
                    tracing::warn!("output device {id} not found ({e}), using the system default");
                    None
                }
            },
            None => None,
        };
        PulseHandle::open(sink_index, rate, core)
    }
}

/// The sink `PULSE_SINK` or `PIPEWIRE_NODE` names, if any.
pub fn env_sink() -> Option<String> {
    ["PULSE_SINK", "PIPEWIRE_NODE"].iter().find_map(|v| std::env::var(v).ok().filter(|n| !n.is_empty()))
}

/// Buffer settings in bytes for [`BUFFER_MS`] and [`REQUEST_MS`] at `rate`.
fn buffer_attr(rate: u32) -> protocol::stream::BufferAttr {
    let bytes = |ms: u32| (ms_frames(ms, rate) as usize * FRAME_BYTES) as u32;
    protocol::stream::BufferAttr { max_length: bytes(BUFFER_MS), target_length: bytes(BUFFER_MS), minimum_request_length: bytes(REQUEST_MS), ..Default::default() }
}

fn ms_frames(ms: u32, rate: u32) -> u64 {
    u64::from(rate) * u64::from(ms) / 1000
}

/// App name and icon for the connection. The server also puts them on the stream.
fn app_props() -> protocol::Props {
    let mut props = protocol::Props::new();
    props.set(Prop::ApplicationName, CString::new(crate::client::APP_DISPLAY_NAME).expect("app name"));
    props.set(Prop::ApplicationIconName, CString::new(ICON_NAME).expect("icon name"));
    props.set(Prop::ApplicationProcessId, CString::new(std::process::id().to_string()).expect("pid"));
    if let Some(binary) = std::env::current_exe().ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())).and_then(|n| CString::new(n).ok()) {
        props.set(Prop::ApplicationProcessBinary, binary);
    }
    props
}

/// The stream's properties: the app's, plus the media role and name.
fn stream_props() -> protocol::Props {
    let mut props = app_props();
    props.set(Prop::MediaName, CString::new(crate::client::APP_DISPLAY_NAME).expect("app name"));
    props.set(Prop::MediaRole, c"music");
    props
}

/// What the timing reader last learned, so the callback can work out what's heard.
#[derive(Default)]
struct Timing {
    /// The stream frame heard when the reading arrived: frames played minus the sink's latency.
    heard_frames: f64,
    /// When that was, and whether the stream was playing, since heard only advances then.
    at: Option<Instant>,
    playing: bool,
}

/// A command waiting for its reply.
enum Pending {
    Create(std::sync::mpsc::Sender<std::result::Result<protocol::CreatePlaybackStreamReply, String>>),
    Latency,
    Ack(&'static str),
}

struct State {
    rate: u32,
    version: u16,
    /// The stream's channel on this connection, once created.
    channel: AtomicU64,
    failed: AtomicBool,
    xruns: AtomicU64,
    /// Frames between the newest written and the one heard, as of the last write.
    latency_frames: AtomicU64,
    /// Frames written to the stream.
    written: AtomicU64,
    /// Microseconds from `start` to the last underrun counted, so one stall counts once.
    last_xrun_us: AtomicU64,
    /// Set once the stream plays. Gaps before that are startup, not underruns.
    armed: AtomicBool,
    timing: Mutex<Timing>,
    start: Instant,
    /// Wakes the timing reader after a write, or to stop it.
    wake: (Mutex<bool>, std::sync::Condvar),
    stop: AtomicBool,
    writer: Mutex<UnixStream>,
    next_seq: AtomicU64,
    pending: Mutex<HashMap<u32, Pending>>,
}

impl State {
    /// Counts an underrun, unless one was counted within the last buffer length.
    /// One stall can show up in more than one way.
    fn note_xrun(&self, why: &str) {
        let now = self.start.elapsed().as_micros() as u64;
        let last = self.last_xrun_us.load(Ordering::Acquire);
        let window = u64::from(BUFFER_MS) * 1000;
        if last != 0 && now.saturating_sub(last) < window {
            return;
        }
        self.last_xrun_us.store(now.max(1), Ordering::Release);
        self.xruns.fetch_add(1, Ordering::AcqRel);
        tracing::warn!("audio: output underrun ({why})");
    }

    fn wake(&self) {
        let (lock, cvar) = &self.wake;
        *lock.lock().unwrap_or_else(|e| e.into_inner()) = true;
        cvar.notify_one();
    }

    /// Sends a command, noting what its reply is for.
    fn send(&self, command: &Command, pending: Pending) -> Result<()> {
        let seq = self.next_seq.fetch_add(1, Ordering::AcqRel) as u32;
        self.pending.lock().expect("pending").insert(seq, pending);
        let mut buf = Vec::new();
        protocol::write_command_message(&mut buf, seq, command, self.version).map_err(|e| server_err("encode", e))?;
        self.writer.lock().expect("writer").write_all(&buf).map_err(Error::Io)
    }

    /// Frames between the newest written and the one heard at `now`, from the last timing reading.
    /// Before any reading, everything written.
    fn latency_after(&self, written_after: u64, now: Instant) -> u64 {
        let timing = self.timing.lock().unwrap_or_else(|e| e.into_inner());
        let Some(at) = timing.at else { return written_after };
        let moved = if timing.playing { now.saturating_duration_since(at).as_secs_f64() * f64::from(self.rate) } else { 0.0 };
        let heard = (timing.heard_frames + moved).min(written_after as f64);
        (written_after as f64 - heard).max(0.0) as u64
    }
}

/// A playing stream. Dropping it closes its connection, which ends the stream.
pub struct PulseHandle {
    state: Arc<State>,
    render_shared: Arc<RenderShared>,
    threads: Vec<std::thread::JoinHandle<()>>,
    /// The media name last sent, so a song is named once.
    title: Mutex<String>,
}

/// The cookie the server asks for: the cookie file's, or 256 zero bytes when there's none, as
/// libpulse sends. PipeWire's pulse server refuses an empty one.
fn cookie() -> Vec<u8> {
    pulseaudio::cookie_path_from_env().and_then(|p| std::fs::read(p).ok()).filter(|c| !c.is_empty()).unwrap_or_else(|| vec![0; COOKIE_LEN])
}

/// The native protocol's cookie length.
const COOKIE_LEN: usize = 256;

/// Connects and introduces Navia, with a timeout on each step so a stuck server can't hang the player.
fn handshake() -> Result<(UnixStream, BufReader<UnixStream>, u16)> {
    let path = pulseaudio::socket_path_from_env().ok_or_else(|| Error::Audio("no sound server".into()))?;
    let socket = UnixStream::connect(path).map_err(Error::Io)?;
    socket.set_read_timeout(Some(SERVER_TIMEOUT)).map_err(Error::Io)?;
    socket.set_write_timeout(Some(SERVER_TIMEOUT)).map_err(Error::Io)?;
    let mut reader = BufReader::new(socket.try_clone().map_err(Error::Io)?);
    let mut writer = socket;
    let auth = protocol::AuthParams { version: protocol::MAX_VERSION, supports_shm: false, supports_memfd: false, cookie: cookie() };
    protocol::write_command_message(&mut writer, 0, &Command::Auth(auth), protocol::MAX_VERSION).map_err(|e| server_err("auth", e))?;
    let (_, reply) = protocol::read_reply_message::<protocol::AuthReply>(&mut reader, protocol::MAX_VERSION).map_err(|e| server_err("auth", e))?;
    let version = protocol::MAX_VERSION.min(reply.version);
    protocol::write_command_message(&mut writer, 1, &Command::SetClientName(app_props()), version).map_err(|e| server_err("client name", e))?;
    protocol::read_reply_message::<protocol::SetClientNameReply>(&mut reader, version).map_err(|e| server_err("client name", e))?;
    Ok((writer, reader, version))
}

impl PulseHandle {
    fn open(sink_index: Option<u32>, rate: u32, core: Arc<Mutex<RenderCore>>) -> Result<Self> {
        let (writer, reader, version) = handshake()?;
        // The audio thread blocks on reads from here on. A write that can't go out in time means
        // the server is stuck.
        reader.get_ref().set_read_timeout(None).map_err(Error::Io)?;
        let state = Arc::new(State {
            rate,
            version,
            channel: AtomicU64::new(u64::MAX),
            failed: AtomicBool::new(false),
            xruns: AtomicU64::new(0),
            latency_frames: AtomicU64::new(0),
            written: AtomicU64::new(0),
            last_xrun_us: AtomicU64::new(0),
            armed: AtomicBool::new(false),
            timing: Mutex::new(Timing::default()),
            start: Instant::now(),
            wake: (Mutex::new(false), std::sync::Condvar::new()),
            stop: AtomicBool::new(false),
            writer: Mutex::new(writer),
            next_seq: AtomicU64::new(2),
            pending: Mutex::new(HashMap::new()),
        });
        let render_shared = core.lock().expect("core").shared().clone();
        // The server asks for audio as soon as the stream exists, so the core needs the rate first.
        core.lock().expect("core").set_stream_rate(rate);
        let (created_tx, created_rx) = std::sync::mpsc::channel();
        let audio = {
            let state = state.clone();
            std::thread::Builder::new().name("navia-pulse-out".into()).spawn(move || Self::run(reader, core, state)).map_err(Error::Io)?
        };
        let mut handle = Self { state: state.clone(), render_shared, threads: vec![audio], title: Mutex::new(String::new()) };
        let params = protocol::PlaybackStreamParams {
            sample_spec: protocol::SampleSpec { format: protocol::SampleFormat::Float32Le, channels: OUTPUT_CHANNELS as u8, sample_rate: rate },
            channel_map: protocol::ChannelMap::new([protocol::ChannelPosition::FrontLeft, protocol::ChannelPosition::FrontRight]),
            sink_index,
            sink_name: None,
            buffer_attr: buffer_attr(rate),
            props: stream_props(),
            // No `adjust_latency`. With it, pipewire-pulse keeps only about 75 ms queued, which a
            // 60 ms stall ran dry at small quanta.
            flags: protocol::stream::StreamFlags::default(),
            ..Default::default()
        };
        state.send(&Command::CreatePlaybackStream(params), Pending::Create(created_tx))?;
        let created = created_rx.recv_timeout(SERVER_TIMEOUT).map_err(|_| Error::Audio("the sound server didn't open the stream in time".into()))?.map_err(|e| Error::Audio(format!("open stream: {e}")))?;
        tracing::info!("audio: sound server stream {} on sink {} ({} Hz, buffer {} B, request {} B)", created.stream_index, created.sink_index, rate, created.buffer_attr.target_length, created.buffer_attr.minimum_request_length);
        let timing = {
            let state = state.clone();
            std::thread::Builder::new().name("navia-pulse-timing".into()).spawn(move || Self::poll_timing(state)).map_err(Error::Io)?
        };
        handle.threads.push(timing);
        Ok(handle)
    }

    /// The audio thread. Reads the server's messages and answers each audio request at once.
    fn run(mut reader: BufReader<UnixStream>, core: Arc<Mutex<RenderCore>>, state: Arc<State>) {
        let mut scratch = vec![0.0f32; 16 * 1024];
        let mut bytes: Vec<u8> = Vec::new();
        let mut payload = Vec::new();
        let mut last_request: Option<Instant> = None;
        let budget = Duration::from_millis(u64::from(BUFFER_MS));
        loop {
            let desc = match protocol::read_descriptor(&mut reader) {
                Ok(d) => d,
                Err(e) => {
                    if !state.stop.load(Ordering::Acquire) {
                        tracing::warn!("audio: the sound server connection ended: {e}");
                        state.failed.store(true, Ordering::Release);
                    }
                    return;
                }
            };
            payload.resize(desc.length as usize, 0);
            if reader.read_exact(&mut payload).is_err() {
                if !state.stop.load(Ordering::Acquire) {
                    state.failed.store(true, Ordering::Release);
                }
                return;
            }
            if desc.channel != u32::MAX {
                continue; // Audio for a record stream, and there are none.
            }
            let mut cursor = std::io::Cursor::new(&payload[..]);
            let Ok((seq, command)) = Command::read_tag_prefixed(&mut cursor, state.version) else { continue };
            let requested = match command {
                Command::Reply => {
                    let pending = state.pending.lock().expect("pending").remove(&seq);
                    let mut ts = protocol::TagStructReader::new(&mut cursor, state.version);
                    match pending {
                        Some(Pending::Create(tx)) => {
                            let reply = ts.read::<protocol::CreatePlaybackStreamReply>().map_err(|e| e.to_string());
                            let requested = reply.as_ref().map(|r| r.requested_bytes).unwrap_or(0);
                            if let Ok(r) = &reply {
                                state.channel.store(u64::from(r.channel), Ordering::Release);
                            }
                            let _ = tx.send(reply);
                            requested
                        }
                        Some(Pending::Latency) => {
                            if let Ok(t) = ts.read::<protocol::PlaybackLatency>() {
                                state.note_timing(&t);
                            }
                            0
                        }
                        _ => 0,
                    }
                }
                Command::Error(e) => {
                    match state.pending.lock().expect("pending").remove(&seq) {
                        Some(Pending::Create(tx)) => {
                            let _ = tx.send(Err(format!("{e:?}")));
                        }
                        Some(Pending::Ack(what)) => tracing::debug!("audio: {what}: {e:?}"),
                        _ => tracing::debug!("audio: sound server error {e:?}"),
                    }
                    0
                }
                Command::Request(r) => r.length,
                Command::Started(_) => {
                    state.armed.store(true, Ordering::Release);
                    0
                }
                Command::Underflow(_) => {
                    if state.armed.load(Ordering::Acquire) {
                        state.note_xrun("the server ran out");
                    }
                    0
                }
                Command::PlaybackStreamKilled(_) => {
                    tracing::warn!("audio: the sound server ended the stream");
                    state.failed.store(true, Ordering::Release);
                    return;
                }
                _ => 0,
            };
            if requested == 0 {
                continue;
            }
            let now = Instant::now();
            if state.armed.load(Ordering::Acquire) {
                // At most the whole buffer was queued at the last answer, so a longer gap ran it dry.
                if let Some(prev) = last_request
                    && now.duration_since(prev) > budget
                {
                    state.note_xrun("late answer");
                }
            }
            let frames = requested as usize / FRAME_BYTES;
                if frames == 0 {
                continue;
            }
            let written_before = state.written.load(Ordering::Acquire);
            stall_point(written_before, frames as u64, state.rate);
            let written_after = written_before + frames as u64;
            let latency = state.latency_after(written_after, Instant::now());
            state.latency_frames.store(latency, Ordering::Release);
            bytes.resize(frames * FRAME_BYTES, 0);
            match core.try_lock() {
                Ok(mut core) => {
                    let heard = heard_latency_frames(latency as f64 * 1000.0 / f64::from(state.rate), 0.0, state.rate);
                    let shared = core.shared();
                    if shared.output_latency.load(Ordering::Acquire) != heard {
                        shared.output_latency.store(heard, Ordering::Release);
                    }
                    let mut done = 0;
                    while done < frames {
                        let n = (frames - done).min(scratch.len() / OUTPUT_CHANNELS);
                        let block = &mut scratch[..n * OUTPUT_CHANNELS];
                        core.render(block);
                        for (out, s) in bytes[done * FRAME_BYTES..(done + n) * FRAME_BYTES].as_chunks_mut::<4>().0.iter_mut().zip(block.iter()) {
                            out.copy_from_slice(&s.to_le_bytes());
                        }
                        done += n;
                    }
                }
                Err(_) => bytes.fill(0),
            }
            let channel = state.channel.load(Ordering::Acquire) as u32;
            let sent = {
                let mut w = state.writer.lock().expect("writer");
                protocol::write_memblock(&mut *w, channel, &bytes, 0)
            };
            if let Err(e) = sent {
                if !state.stop.load(Ordering::Acquire) {
                    tracing::warn!("audio: writing to the sound server failed: {e}");
                    state.failed.store(true, Ordering::Release);
                }
                return;
            }
            state.written.store(written_after, Ordering::Release);
            last_request = Some(Instant::now());
            state.wake();
        }
    }

    /// Asks for the stream's timing after each write and every [`TIMING_INTERVAL`].
    /// The audio thread reads the answers.
    fn poll_timing(state: Arc<State>) {
        while !state.stop.load(Ordering::Acquire) {
            let channel = state.channel.load(Ordering::Acquire) as u32;
            let waiting = state.pending.lock().expect("pending").values().any(|p| matches!(p, Pending::Latency));
            if !waiting && state.send(&Command::GetPlaybackLatency(protocol::LatencyParams { channel, now: std::time::SystemTime::now() }), Pending::Latency).is_err() {
                return;
            }
            let (lock, cvar) = &state.wake;
            let Ok(guard) = lock.lock() else { return };
            let (mut guard, _) = cvar.wait_timeout_while(guard, TIMING_INTERVAL, |woken| !*woken).unwrap_or_else(|e| e.into_inner());
            *guard = false;
        }
    }

    /// Sets the stream's media name, shown in the desktop mixer under "Navia". Sent once per change.
    pub fn set_title(&self, title: &str) {
        let title = if title.trim().is_empty() { crate::client::APP_DISPLAY_NAME } else { title };
        {
            let mut last = self.title.lock().expect("title");
            if *last == title {
                return;
            }
            *last = title.to_string();
        }
        let Ok(name) = CString::new(title.replace('\0', "")) else { return };
        let index = self.state.channel.load(Ordering::Acquire) as u32;
        if let Err(e) = self.state.send(&Command::SetPlaybackStreamName(protocol::SetStreamNameParams { index, name }), Pending::Ack("stream name")) {
            tracing::debug!("audio: couldn't name the stream: {e}");
        }
    }
}

impl State {
    /// Handles a timing answer: what's heard now, and whether the queue ran dry while playing.
    fn note_timing(&self, t: &protocol::PlaybackLatency) {
        let rate = f64::from(self.rate);
        let read = t.read_offset.max(0) as f64 / FRAME_BYTES as f64;
        let sink = t.sink_usec as f64 * rate / 1e6;
        *self.timing.lock().unwrap_or_else(|e| e.into_inner()) = Timing { heard_frames: (read - sink).max(0.0), at: Some(Instant::now()), playing: t.playing };
        if t.playing {
            self.armed.store(true, Ordering::Release);
        }
    }
}

impl Drop for PulseHandle {
    fn drop(&mut self) {
        self.state.stop.store(true, Ordering::Release);
        self.state.wake();
        // Closing the connection ends the stream and the audio thread's read.
        let _ = self.state.writer.lock().expect("writer").shutdown(std::net::Shutdown::Both);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
        self.render_shared.output_latency.store(0, Ordering::Release);
    }
}

impl OutputHandle for PulseHandle {
    fn latency_frames(&self) -> u64 {
        self.state.latency_frames.load(Ordering::Acquire)
    }

    fn failed(&self) -> bool {
        self.state.failed.load(Ordering::Acquire)
    }

    fn rate(&self) -> u32 {
        self.state.rate
    }

    // the sound server converts to the sink's rate as it likes
    fn device_rate(&self) -> Option<u32> {
        None
    }

    fn xruns(&self) -> u64 {
        self.state.xruns.load(Ordering::Acquire)
    }

    fn set_title(&self, title: &str) {
        PulseHandle::set_title(self, title);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_buffer_is_100_ms_asked_for_25_ms_at_a_time() {
        let attr = buffer_attr(48_000);
        assert_eq!(attr.target_length, 4_800 * 8);
        assert_eq!(attr.max_length, 4_800 * 8);
        assert_eq!(attr.minimum_request_length, 1_200 * 8);
        assert_eq!(buffer_attr(44_100).target_length, 4_410 * 8);
    }
}
