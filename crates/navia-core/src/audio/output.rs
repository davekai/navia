//! Device output through `cpal`, plus a capture backend for testing the player without a sound
//! device. A device pulls samples from a [`RenderCore`] at one rate, and the player reopens it for another.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, DeviceId, FromSample, Sample, SampleFormat, SizedSample, StreamConfig, SupportedStreamConfigRange};

use crate::audio::engine::OUTPUT_CHANNELS;
use crate::audio::render::RenderCore;
use crate::error::{Error, Result};

/// An output device a user can pick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Stable identifier to save in settings.
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

/// What a device can do.
#[derive(Clone, Debug)]
pub struct DeviceCaps {
    /// Inclusive sample rate ranges for stereo output.
    pub rates: Vec<(u32, u32)>,
    pub default_rate: u32,
}

/// A running output. Dropping it stops the device.
pub trait OutputHandle {
    /// Frames between what the callback writes and what the device plays.
    fn latency_frames(&self) -> u64;
    /// True if the device was lost or failed and output must be rebuilt.
    fn failed(&self) -> bool;
    /// Rate of the device, in frames per second.
    fn rate(&self) -> u32;
    /// the rate the device itself runs at, for showing; `None` where something between may
    /// convert the stream (the Linux sound server, an ALSA plugin), so the stream's rate isn't it
    fn device_rate(&self) -> Option<u32> {
        Some(self.rate())
    }
    /// Device-side buffer overruns or underruns reported by the system.
    fn xruns(&self) -> u64;
    /// True when the device's own volume follows the volume control, as on AirPlay.
    /// The engine then stays at unity gain.
    fn remote_volume(&self) -> bool {
        false
    }
    /// Sets the device's own volume, 0 to 1. Called only when the listener changes it.
    fn set_volume(&self, _volume: f32) {}
    /// The device's own volume, 0 to 1, as last reported, with a counter that moves on each report.
    /// `None` until it says.
    fn reported_volume(&self) -> Option<(u64, f32)> {
        None
    }
    /// The song now heard and the position in it, for a device that shows what's playing.
    fn now_playing(&self, _song_id: &str, _position_ms: u64, _duration_ms: Option<u64>) {}
    /// What's playing, as "Title – Artist", for outputs the system lists by name, such as a Linux
    /// mixer stream. Called often, so send only on change.
    fn set_title(&self, _title: &str) {}
}

/// Opens outputs. Lives on the player's control thread.
pub trait OutputBackend: Send {
    fn list_devices(&self) -> Result<Vec<DeviceInfo>>;
    fn caps(&self, device: Option<&str>) -> Result<DeviceCaps>;
    fn open(&mut self, device: Option<&str>, rate: u32, core: Arc<Mutex<RenderCore>>) -> Result<Box<dyn OutputHandle>>;

    /// The name people know `device` by, for messages. `None` means the system default.
    fn device_name(&self, device: Option<&str>) -> Option<String> {
        let list = self.list_devices().ok()?;
        let found = match device {
            Some(id) => list.into_iter().find(|d| d.id == id),
            None => list.into_iter().find(|d| d.is_default),
        };
        found.map(|d| d.name).filter(|n| !n.is_empty())
    }
}

/// The system's audio devices. On Linux with a sound server, Navia's own pulse stream
/// ([`crate::audio::pulse`]), falling back to ALSA through cpal with [`ALSA_PERIOD_FRAMES`]
/// periods. Elsewhere cpal's default host.
pub struct CpalBackend {
    host: cpal::Host,
    /// The Linux sound server, when one answers.
    #[cfg(target_os = "linux")]
    pulse: Mutex<Option<crate::audio::pulse::PulseServer>>,
}

impl Default for CpalBackend {
    fn default() -> Self {
        Self {
            host: cpal::default_host(),
            #[cfg(target_os = "linux")]
            pulse: Mutex::new(crate::audio::pulse::PulseServer::connect()),
        }
    }
}

/// ALSA period without a sound server: about 43 ms, with two periods buffered.
/// ALSA's own default period crackled under load (cpal issue #1390). A device that refuses this
/// size opens with its default.
#[cfg(target_os = "linux")]
pub const ALSA_PERIOD_FRAMES: u32 = 2048;

static STALL_MS: AtomicU64 = AtomicU64::new(0);

/// For tests: the output callback sleeps `ms` once per second of audio from the third second on,
/// like a stalled app. 0 turns it off.
pub fn inject_output_stalls(ms: u64) {
    STALL_MS.store(ms, Ordering::Relaxed);
}

/// Where a callback writing `frames` after `written` frames sleeps, when stalls are injected.
pub(crate) fn stall_point(written: u64, frames: u64, rate: u32) {
    let ms = STALL_MS.load(Ordering::Relaxed);
    let rate = u64::from(rate.max(1));
    if ms > 0 && written >= 2 * rate && written / rate != (written + frames) / rate {
        thread::sleep(Duration::from_millis(ms));
    }
}

fn cpal_err(context: &str, e: impl std::fmt::Display) -> Error {
    Error::Audio(format!("{context}: {e}"))
}

impl CpalBackend {
    /// The sound server's connection, reconnected if it went away, such as after a pipewire-pulse restart.
    #[cfg(target_os = "linux")]
    fn with_pulse<T>(&self, f: impl Fn(&crate::audio::pulse::PulseServer) -> Result<T>) -> Option<Result<T>> {
        let mut slot = self.pulse.lock().expect("pulse");
        if let Some(server) = slot.as_ref() {
            match f(server) {
                Ok(v) => return Some(Ok(v)),
                Err(e) => tracing::info!("audio: sound server: {e}; reconnecting"),
            }
        }
        *slot = crate::audio::pulse::PulseServer::connect();
        slot.as_ref().map(f)
    }

    /// Devices to try, best first: the chosen one, then the system default.
    /// A saved device that is gone plays on the default instead.
    fn candidates(&self, id: Option<&str>) -> Result<Vec<cpal::Device>> {
        Ok(self.candidates_marked(id)?.into_iter().map(|(d, _)| d).collect())
    }

    /// The same, each marked true when it's the system default.
    fn candidates_marked(&self, id: Option<&str>) -> Result<Vec<(cpal::Device, bool)>> {
        let mut out = Vec::new();
        if let Some(id) = id {
            match id.parse::<DeviceId>() {
                Ok(parsed) => {
                    let host = (parsed.host() == self.host.id()).then_some(&self.host);
                    match host.and_then(|h| h.device_by_id(&parsed)) {
                        Some(d) => out.push((d, false)),
                        None => tracing::warn!("output device {id} not found, using the system default"),
                    }
                }
                Err(e) => tracing::warn!("output device {id}: {e}, using the system default"),
            }
        }
        out.extend(self.host.default_output_device().map(|d| (d, true)));
        if out.is_empty() {
            return Err(Error::Audio("no default output device".to_string()));
        }
        Ok(out)
    }

    fn stereo_ranges(device: &cpal::Device) -> Result<Vec<SupportedStreamConfigRange>> {
        let configs: Vec<_> = device.supported_output_configs().map_err(|e| cpal_err("device configs", e))?.collect();
        let stereo: Vec<_> = configs.iter().filter(|c| c.channels() as usize == OUTPUT_CHANNELS).cloned().collect();
        Ok(if stereo.is_empty() { configs } else { stereo })
    }

    /// CoreAudio moves the device to the stream's rate; WASAPI's shared mode converts to the
    /// mix format's; on Linux ALSA's default device is often a converting plugin, so unknown
    fn device_rate_of(device: &cpal::Device, rate: u32) -> Option<u32> {
        if cfg!(target_os = "macos") {
            Some(rate)
        } else if cfg!(target_os = "windows") {
            device.default_output_config().ok().map(|c| c.sample_rate())
        } else {
            None
        }
    }

    fn caps_of(device: &cpal::Device) -> Result<DeviceCaps> {
        let rates = Self::stereo_ranges(device)?.iter().map(|c| (c.min_sample_rate(), c.max_sample_rate())).collect();
        let default_rate = device.default_output_config().map_err(|e| cpal_err("default config", e))?.sample_rate();
        Ok(DeviceCaps { rates, default_rate })
    }

    fn open_device(device: &cpal::Device, rate: u32, core: Arc<Mutex<RenderCore>>, follows_default: bool) -> Result<Box<dyn OutputHandle>> {
        #[cfg(target_os = "linux")]
        if device.id().is_ok_and(|id| id.host() == cpal::HostId::Alsa) {
            return Self::open_with(device, rate, core.clone(), follows_default, BufferSize::Fixed(ALSA_PERIOD_FRAMES)).or_else(|e| {
                tracing::info!("output: {ALSA_PERIOD_FRAMES}-frame periods refused ({e}); using the device's own");
                Self::open_with(device, rate, core, follows_default, BufferSize::Default)
            });
        }
        Self::open_with(device, rate, core, follows_default, BufferSize::Default)
    }

    fn open_with(device: &cpal::Device, rate: u32, core: Arc<Mutex<RenderCore>>, follows_default: bool, buffer: BufferSize) -> Result<Box<dyn OutputHandle>> {
        let mut ranges: Vec<_> = Self::stereo_ranges(device)?.into_iter().filter(|c| (c.min_sample_rate()..=c.max_sample_rate()).contains(&rate)).collect();
        ranges.sort_by_key(|c| format_rank(c.sample_format()));
        let chosen = ranges.into_iter().next().ok_or_else(|| Error::Audio(format!("device doesn't support {rate} Hz")))?;
        let supported = chosen.with_sample_rate(rate);
        let channels = supported.channels() as usize;
        let format = supported.sample_format();
        let mut config = supported.config();
        config.buffer_size = buffer;
        let shared = Arc::new(HandleState { follows_default, ..Default::default() });
        #[cfg(target_os = "macos")]
        shared.stream_latency_ms.store(macos::default_stream_latency_ms().to_bits(), Ordering::Release);
        let render_shared = core.lock().expect("core").shared().clone();
        // Some hosts call back as soon as the stream is built. The core must already know the rate,
        // or it takes the first block for a rate change and asks for a reopen.
        core.lock().expect("core").set_stream_rate(rate);
        let stream = match format {
            SampleFormat::F32 => build::<f32>(device, &config, channels, core, shared.clone(), rate),
            SampleFormat::F64 => build::<f64>(device, &config, channels, core, shared.clone(), rate),
            SampleFormat::I32 => build::<i32>(device, &config, channels, core, shared.clone(), rate),
            SampleFormat::I24 => build::<cpal::I24>(device, &config, channels, core, shared.clone(), rate),
            SampleFormat::U32 => build::<u32>(device, &config, channels, core, shared.clone(), rate),
            SampleFormat::U24 => build::<cpal::U24>(device, &config, channels, core, shared.clone(), rate),
            SampleFormat::I16 => build::<i16>(device, &config, channels, core, shared.clone(), rate),
            SampleFormat::U16 => build::<u16>(device, &config, channels, core, shared.clone(), rate),
            other => return Err(Error::Audio(format!("unsupported device sample format {other:?}"))),
        }?;
        stream.play().map_err(|e| cpal_err("start stream", e))?;
        Ok(Box::new(CpalHandle { _stream: stream, shared, rate, device_rate: Self::device_rate_of(device, rate), render_shared }))
    }
}

/// Best format first: float, then integer widest to narrowest.
fn format_rank(format: SampleFormat) -> u32 {
    match format {
        SampleFormat::F32 => 0,
        SampleFormat::F64 => 1,
        SampleFormat::I32 => 2,
        SampleFormat::I24 => 3,
        SampleFormat::U32 => 4,
        SampleFormat::U24 => 5,
        SampleFormat::I16 => 6,
        SampleFormat::U16 => 7,
        _ => 9,
    }
}

impl OutputBackend for CpalBackend {
    fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        #[cfg(target_os = "linux")]
        if let Some(Ok(list)) = self.with_pulse(|p| p.list()) {
            return Ok(list);
        }
        let default_id = self.host.default_output_device().and_then(|d| d.id().ok()).map(|i| i.to_string());
        let devices = self.host.output_devices().map_err(|e| cpal_err("list devices", e))?;
        let mut out = Vec::new();
        for device in devices {
            let Ok(id) = device.id().map(|i| i.to_string()) else { continue };
            let name = device.description().map(|d| d.name().to_string()).unwrap_or_else(|_| id.clone());
            let is_default = default_id.as_deref() == Some(id.as_str());
            out.push(DeviceInfo { id, name, is_default });
        }
        Ok(out)
    }

    fn caps(&self, device: Option<&str>) -> Result<DeviceCaps> {
        #[cfg(target_os = "linux")]
        if !device.is_some_and(|d| d.starts_with("alsa:")) {
            let pulse = device.filter(|d| d.starts_with(crate::audio::pulse::ID_PREFIX));
            if let Some(Ok(caps)) = self.with_pulse(|p| p.caps(pulse).or_else(|_| p.caps(None))) {
                return Ok(caps);
            }
        }
        let mut last = None;
        for d in self.candidates(device)? {
            match Self::caps_of(&d) {
                Ok(caps) => return Ok(caps),
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| Error::Audio("no output device".to_string())))
    }

    fn open(&mut self, device_id: Option<&str>, rate: u32, core: Arc<Mutex<RenderCore>>) -> Result<Box<dyn OutputHandle>> {
        // Try the sound server first, unless an ALSA device was picked by hand.
        // A fresh connection, in case the server restarted.
        #[cfg(target_os = "linux")]
        if !device_id.is_some_and(|d| d.starts_with("alsa:")) {
            let sink = device_id.filter(|d| d.starts_with(crate::audio::pulse::ID_PREFIX));
            let server = crate::audio::pulse::PulseServer::connect();
            match server.as_ref().map(|p| p.open(sink, rate, core.clone())) {
                Some(Ok(handle)) => {
                    *self.pulse.lock().expect("pulse") = server;
                    return Ok(Box::new(handle));
                }
                // A sink named in the environment, such as a test's null sink, is never swapped for ALSA's default.
                Some(Err(e)) if device_id.is_none() && crate::audio::pulse::env_sink().is_some() => return Err(e),
                Some(Err(e)) => tracing::warn!("output: the sound server's stream didn't open ({e}); trying ALSA"),
                None => {}
            }
        }
        let mut last = None;
        for (d, follows_default) in self.candidates_marked(device_id)? {
            match Self::open_device(&d, rate, core.clone(), follows_default) {
                Ok(handle) => return Ok(handle),
                Err(e) => {
                    tracing::warn!("output {}: {e}", d.id().map(|i| i.to_string()).unwrap_or_default());
                    last = Some(e);
                }
            }
        }
        Err(last.unwrap_or_else(|| Error::Audio("no output device".to_string())))
    }
}

#[derive(Default)]
struct HandleState {
    latency_frames: AtomicU64,
    failed: AtomicBool,
    xruns: AtomicU64,
    /// Opened as the system default, which the system moves to a new device by itself on the Mac.
    /// A change of default then isn't a lost device.
    follows_default: bool,
    /// The device's stream latency in ms as f64 bits, which cpal's timestamps leave out. Mac only.
    #[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
    stream_latency_ms: AtomicU64,
    /// Frames the callback has written, for injected stalls.
    written: AtomicU64,
}

struct CpalHandle {
    _stream: cpal::Stream,
    shared: Arc<HandleState>,
    rate: u32,
    device_rate: Option<u32>,
    /// Where the heard latency goes, reset when the device closes.
    render_shared: Arc<crate::audio::render::RenderShared>,
}

impl Drop for CpalHandle {
    fn drop(&mut self) {
        if cfg!(any(target_os = "macos", target_os = "linux")) {
            self.render_shared.output_latency.store(0, Ordering::Release);
        }
    }
}

/// Below this, output latency is left out of the heard position. Above it, as on AirPlay or
/// Bluetooth, progress, lyrics and track changes follow what's heard.
pub const MIN_HEARD_LATENCY_MS: f64 = 50.0;

/// Frames between rendering and hearing: cpal's timestamp gap plus the stream's own latency.
/// 0 below [`MIN_HEARD_LATENCY_MS`].
pub fn heard_latency_frames(timestamp_gap_ms: f64, stream_latency_ms: f64, rate: u32) -> u64 {
    let total = timestamp_gap_ms.max(0.0) + stream_latency_ms.max(0.0);
    if total < MIN_HEARD_LATENCY_MS {
        return 0;
    }
    (total * f64::from(rate) / 1000.0).round() as u64
}

impl OutputHandle for CpalHandle {
    fn latency_frames(&self) -> u64 {
        self.shared.latency_frames.load(Ordering::Acquire)
    }

    fn failed(&self) -> bool {
        self.shared.failed.load(Ordering::Acquire)
    }

    fn rate(&self) -> u32 {
        self.rate
    }

    fn device_rate(&self) -> Option<u32> {
        self.device_rate
    }

    fn xruns(&self) -> u64 {
        self.shared.xruns.load(Ordering::Acquire)
    }
}

/// Renders into f32, then converts for the device. Other channel counts get the stereo pair on
/// the first two channels, and mono gets the average.
fn build<T>(device: &cpal::Device, config: &StreamConfig, channels: usize, core: Arc<Mutex<RenderCore>>, shared: Arc<HandleState>, rate: u32) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let err_state = shared.clone();
    let mut scratch = vec![0.0f32; 16 * 1024];
    let data_state = shared.clone();
    let data_callback = move |data: &mut [T], info: &cpal::OutputCallbackInfo| {
        let timestamp = info.timestamp();
        let latency = timestamp.playback.duration_since(timestamp.callback);
        let latency_frames = (latency.as_secs_f64() * f64::from(rate)).max(0.0) as u64;
        data_state.latency_frames.store(latency_frames, Ordering::Release);
        let frames = data.len() / channels;
        stall_point(data_state.written.load(Ordering::Relaxed), frames as u64, rate);
        data_state.written.fetch_add(frames as u64, Ordering::Relaxed);
        let mut written = 0;
        match core.try_lock() {
            Ok(mut core) => {
                // What's heard trails what's rendered by the device's whole latency, which can be
                // seconds on a Mac AirPlay route.
                #[cfg(any(target_os = "macos", target_os = "linux"))]
                {
                    let stream_ms = f64::from_bits(data_state.stream_latency_ms.load(Ordering::Acquire));
                    let heard = heard_latency_frames(latency.as_secs_f64() * 1000.0, stream_ms, rate);
                    let shared = core.shared();
                    if shared.output_latency.load(Ordering::Acquire) != heard {
                        shared.output_latency.store(heard, Ordering::Release);
                    }
                }
                while written < frames {
                    let n = (frames - written).min(scratch.len() / OUTPUT_CHANNELS);
                    core.render(&mut scratch[..n * OUTPUT_CHANNELS]);
                    write_frames(&scratch[..n * OUTPUT_CHANNELS], &mut data[written * channels..(written + n) * channels], channels);
                    written += n;
                }
            }
            Err(_) => data.fill(T::EQUILIBRIUM),
        }
    };
    let error_callback = move |e: cpal::Error| {
        let kind = e.kind();
        // The Mac's default output unit already moved to the new default, so keep playing with
        // the new device's latency.
        if kind == cpal::ErrorKind::DeviceChanged && err_state.follows_default && cfg!(target_os = "macos") {
            #[cfg(target_os = "macos")]
            err_state.stream_latency_ms.store(macos::default_stream_latency_ms().to_bits(), Ordering::Release);
            tracing::info!("audio: the system default output changed; following it");
            return;
        }
        if matches!(kind, cpal::ErrorKind::DeviceNotAvailable | cpal::ErrorKind::HostUnavailable | cpal::ErrorKind::DeviceChanged) {
            err_state.failed.store(true, Ordering::Release);
        } else {
            err_state.xruns.fetch_add(1, Ordering::AcqRel);
        }
        tracing::warn!("audio stream error: {e}");
    };
    device.build_output_stream::<T, _, _>(*config, data_callback, error_callback, None).map_err(|e| cpal_err("open stream", e))
}

fn write_frames<T: Sample + FromSample<f32>>(stereo: &[f32], out: &mut [T], channels: usize) {
    match channels {
        OUTPUT_CHANNELS => out.iter_mut().zip(stereo).for_each(|(o, s)| *o = s.to_sample()),
        1 => out.iter_mut().zip(stereo.chunks_exact(2)).for_each(|(o, f)| *o = ((f[0] + f[1]) * 0.5).to_sample()),
        n => {
            for (frame, input) in out.chunks_exact_mut(n).zip(stereo.chunks_exact(2)) {
                frame[0] = input[0].to_sample();
                frame[1] = input[1].to_sample();
                frame[2..].iter_mut().for_each(|s| *s = T::EQUILIBRIUM);
            }
        }
    }
}

/// A pretend device that renders into memory in real time or faster, for testing the player.
pub struct CaptureBackend {
    /// Everything the device was asked to play, including silence.
    pub capture: Arc<CaptureBuffer>,
    /// 1.0 is real time, larger is faster, 0 pulls as fast as possible.
    pub speed: f64,
    /// Inclusive supported rate range.
    pub rates: (u32, u32),
    pub default_rate: u32,
    /// Simulated time to open a stream.
    pub open_delay: Duration,
}

#[derive(Default)]
pub struct CaptureBuffer {
    inner: Mutex<CaptureInner>,
}

#[derive(Default)]
struct CaptureInner {
    samples: Vec<f32>,
    /// Rate, and the sample index where this stream's audio starts.
    opens: Vec<(u32, usize)>,
}

impl CaptureBuffer {
    pub fn samples(&self) -> Vec<f32> {
        self.inner.lock().expect("capture").samples.clone()
    }

    pub fn len(&self) -> usize {
        self.inner.lock().expect("capture").samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Each time the device was opened: the rate and the frame index it began writing.
    pub fn opens(&self) -> Vec<(u32, usize)> {
        self.inner.lock().expect("capture").opens.clone()
    }
}

impl Default for CaptureBackend {
    fn default() -> Self {
        Self { capture: Arc::new(CaptureBuffer::default()), speed: 1.0, rates: (8_000, 384_000), default_rate: 48_000, open_delay: Duration::ZERO }
    }
}

struct CaptureHandle {
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    rate: u32,
}

impl Drop for CaptureHandle {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl OutputHandle for CaptureHandle {
    fn latency_frames(&self) -> u64 {
        0
    }

    fn failed(&self) -> bool {
        false
    }

    fn rate(&self) -> u32 {
        self.rate
    }

    fn xruns(&self) -> u64 {
        0
    }
}

/// Cycling block sizes, as real device callbacks vary.
const CAPTURE_BLOCKS: [usize; 5] = [512, 480, 1024, 256, 333];

impl OutputBackend for CaptureBackend {
    fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        Ok(vec![DeviceInfo { id: "capture".to_string(), name: "Capture".to_string(), is_default: true }])
    }

    fn caps(&self, _device: Option<&str>) -> Result<DeviceCaps> {
        Ok(DeviceCaps { rates: vec![self.rates], default_rate: self.default_rate })
    }

    fn open(&mut self, _device: Option<&str>, rate: u32, core: Arc<Mutex<RenderCore>>) -> Result<Box<dyn OutputHandle>> {
        thread::sleep(self.open_delay);
        core.lock().expect("core").set_stream_rate(rate);
        {
            let mut inner = self.capture.inner.lock().expect("capture");
            let frame = inner.samples.len() / OUTPUT_CHANNELS;
            inner.opens.push((rate, frame));
        }
        let running = Arc::new(AtomicBool::new(true));
        let (flag, capture, speed) = (running.clone(), self.capture.clone(), self.speed);
        let thread = thread::Builder::new()
            .name("navia-capture".into())
            .spawn(move || {
                let mut block = vec![0.0f32; 4096];
                let mut i = 0;
                let started = std::time::Instant::now();
                let mut total = 0u64;
                while flag.load(Ordering::Acquire) {
                    let frames = CAPTURE_BLOCKS[i % CAPTURE_BLOCKS.len()];
                    i += 1;
                    core.lock().expect("core").render(&mut block[..frames * OUTPUT_CHANNELS]);
                    capture.inner.lock().expect("capture").samples.extend_from_slice(&block[..frames * OUTPUT_CHANNELS]);
                    total += frames as u64;
                    if speed > 0.0 {
                        let target = Duration::from_secs_f64(total as f64 / f64::from(rate) / speed);
                        if let Some(wait) = target.checked_sub(started.elapsed()) {
                            thread::sleep(wait);
                        }
                    } else {
                        thread::yield_now();
                    }
                }
            })
            .map_err(Error::Io)?;
        Ok(Box::new(CaptureHandle { running, thread: Some(thread), rate }))
    }
}

/// The default output's stream latency, which cpal leaves out of its timestamps.
#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::c_void;
    use std::ptr::NonNull;

    use objc2_core_audio::{
        AudioObjectGetPropertyData, AudioObjectID, AudioObjectPropertyAddress, kAudioDevicePropertyNominalSampleRate, kAudioDevicePropertyStreams, kAudioHardwarePropertyDefaultOutputDevice,
        kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeOutput, kAudioObjectSystemObject, kAudioStreamPropertyLatency,
    };

    fn get<T: Default>(object: AudioObjectID, selector: u32, scope: u32) -> Option<T> {
        let address = AudioObjectPropertyAddress { mSelector: selector, mScope: scope, mElement: kAudioObjectPropertyElementMain };
        let mut value = T::default();
        let mut size = std::mem::size_of::<T>() as u32;
        // SAFETY: `value` is a plain value of the property's size, and CoreAudio writes at most `size` bytes.
        let status = unsafe { AudioObjectGetPropertyData(object, NonNull::from(&address), 0, std::ptr::null(), NonNull::from(&mut size), NonNull::new(&mut value as *mut T as *mut c_void)?) };
        (status == 0).then_some(value)
    }

    /// The default output device's first stream latency in ms, or 0 when it can't be read.
    pub fn default_stream_latency_ms() -> f64 {
        let Some(device) = get::<AudioObjectID>(kAudioObjectSystemObject as AudioObjectID, kAudioHardwarePropertyDefaultOutputDevice, kAudioObjectPropertyScopeGlobal) else { return 0.0 };
        let Some(stream) = get::<[AudioObjectID; 16]>(device, kAudioDevicePropertyStreams, kAudioObjectPropertyScopeOutput).map(|s| s[0]).filter(|&s| s != 0) else { return 0.0 };
        let Some(frames) = get::<u32>(stream, kAudioStreamPropertyLatency, kAudioObjectPropertyScopeGlobal) else { return 0.0 };
        let rate = get::<f64>(device, kAudioDevicePropertyNominalSampleRate, kAudioObjectPropertyScopeGlobal).filter(|r| *r > 0.0).unwrap_or(48_000.0);
        f64::from(frames) * 1000.0 / rate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_latency_is_left_out() {
        assert_eq!(heard_latency_frames(10.0, 0.0, 48_000), 0);
        assert_eq!(heard_latency_frames(30.0, 15.0, 48_000), 0);
    }

    #[test]
    fn long_latency_counts_with_the_stream_latency() {
        // An AirPlay route: about two seconds from the device, plus the stream's own.
        assert_eq!(heard_latency_frames(1_990.0, 10.0, 44_100), 88_200);
        assert_eq!(heard_latency_frames(-5.0, 60.0, 48_000), 2_880);
    }
}
