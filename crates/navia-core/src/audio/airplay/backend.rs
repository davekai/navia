//! AirPlay as an output beside the system's devices: the app-wide [`AirPlay`] service and the
//! [`OutputRouter`] the player opens outputs through. Output ids are `airplay:<deviceid>`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use navia_airplay::rtsp::dmap::TrackMetadata;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use super::dacp::{Dacp, RemoteHandler, RemoteIds, Request, RequestHandler};
use super::device::{AirPlayDevice, Pairing, Route, Support};
use super::discovery::Discovery;
use super::sender::{Ctl, SYNC_LATENCY, Sender, SenderState};
use super::session::{self, PinPairing, Ports, SAMPLE_RATE, Session};
use super::store::{PairingStore, SecretStore};
use super::volume::{Reported, VolumeLink};
use crate::audio::output::{CpalBackend, DeviceCaps, DeviceInfo, OutputBackend, OutputHandle};
use crate::audio::render::{RenderCore, RenderShared};
use crate::error::{Error, Result};

/// Output ids for AirPlay receivers start with this.
pub const ID_PREFIX: &str = "airplay:";
/// Longest edge of cover art sent to a receiver.
const ART_EDGE: u32 = 800;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(12);
/// How long a session may go without hearing from the receiver before a firewall is suspected.
/// Receivers ask for timing within a second of SETUP.
const FIREWALL_GRACE: Duration = Duration::from_secs(4);

/// True when the receiver was given our ports and nothing ever came back.
fn firewall_suspected(timing_requests: Option<u64>, inbound: u64) -> bool {
    timing_requests == Some(0) && inbound == 0
}
/// How long opening waits for discovery to find a receiver.
const DISCOVERY_WAIT: Duration = Duration::from_secs(5);

/// What a receiver shows for a song.
#[derive(Clone, Debug, Default)]
pub struct TrackInfo {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: Option<u64>,
    /// The server's cover art id, if the song has one.
    pub cover: Option<String>,
}

/// Where song details and cover art come from, usually the app's library and image cache.
pub trait NowPlayingSource: Send + Sync {
    fn track(&self, song_id: &str) -> Option<TrackInfo>;
    /// The cover image in JPEG, PNG or WebP, any size.
    fn artwork(&self, cover_id: &str) -> Option<Vec<u8>>;
}

/// What an AirPlay output is doing, for the UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutputState {
    Connecting,
    Playing,
    /// Needs pairing with a PIN first.
    NeedsPairing,
    /// The receiver never asked us for timing, so a firewall is likely blocking it.
    FirewallSuspected,
    /// Lost or refused, with the reason.
    Failed(String),
}

/// One receiver as the UI lists it.
#[derive(Clone, Debug)]
pub struct AirPlayOutput {
    /// `airplay:<deviceid>`.
    pub id: String,
    pub name: String,
    pub model: String,
    pub support: Support,
    pub paired: bool,
    pub needs_pin: bool,
    pub state: Option<OutputState>,
    /// Discovery sees it now. A paired receiver that's asleep is still listed with its saved name.
    pub found: bool,
    /// Its first IPv4 address, for a firewall rule scoped to its network.
    pub address: Option<std::net::IpAddr>,
}

pub struct AirPlayConfig {
    /// The config directory for pairing files. The identity goes to `secrets`.
    pub config_dir: PathBuf,
    pub secrets: Option<Arc<dyn SecretStore>>,
    pub ports: Ports,
    pub now_playing: Option<Arc<dyn NowPlayingSource>>,
    /// Handles the receiver's remote buttons.
    pub remote: Option<RemoteHandler>,
}

struct Inner {
    config: AirPlayConfig,
    store: PairingStore,
    /// Always `Some` until dropped. It shuts down in the background so dropping never blocks.
    rt: Option<tokio::runtime::Runtime>,
    discovery: Mutex<Option<Discovery>>,
    dacp: Mutex<Option<Dacp>>,
    ids: RemoteIds,
    states: Mutex<HashMap<String, OutputState>>,
    pairing: Mutex<Option<(String, PinPairing)>>,
    /// Called when the list or a receiver's state changes.
    on_change: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    /// The session playing now, for the volume its receiver reports over DACP.
    active: Mutex<Option<UnboundedSender<Msg>>>,
}

impl Inner {
    fn runtime(&self) -> &tokio::runtime::Runtime {
        self.rt.as_ref().expect("AirPlay runtime")
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}

/// The app's AirPlay service.
#[derive(Clone)]
pub struct AirPlay {
    inner: Arc<Inner>,
}

impl AirPlay {
    pub fn new(config: AirPlayConfig) -> Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).thread_name("navia-airplay").enable_all().build()?;
        let store = PairingStore::new(config.config_dir.clone(), config.secrets.clone());
        Ok(Self { inner: Arc::new(Inner { config, store, rt: Some(runtime), discovery: Mutex::new(None), dacp: Mutex::new(None), ids: RemoteIds::new(), states: Mutex::new(HashMap::new()), pairing: Mutex::new(None), on_change: Mutex::new(None), active: Mutex::new(None) }) })
    }

    /// Starts finding receivers. `on_change` runs on a background thread when anything changes.
    pub fn start_discovery(&self, on_change: impl Fn() + Send + Sync + 'static) -> Result<()> {
        let mut d = self.inner.discovery.lock().expect("discovery");
        if d.is_none() {
            let changed: Arc<dyn Fn() + Send + Sync> = Arc::new(on_change);
            *self.inner.on_change.lock().expect("on_change") = Some(changed.clone());
            let started = Discovery::start(move || changed())?;
            // Buttons go to the app, and the receiver's volume goes to the session playing.
            let (buttons, inner) = (self.inner.config.remote.clone(), Arc::downgrade(&self.inner));
            let handler: RequestHandler = Arc::new(move |req| match req {
                Request::Remote(cmd) => {
                    if let Some(b) = &buttons {
                        b(cmd);
                    }
                }
                Request::Volume(level) => {
                    if let Some(tx) = inner.upgrade().and_then(|i| i.active.lock().expect("active").clone()) {
                        let _ = tx.send(Msg::Reported(level));
                    }
                }
            });
            // Without it, sound still works; only the receiver's remote buttons don't.
            match Dacp::start(&started, self.inner.config.ports.dacp, &self.inner.ids, handler) {
                Ok(dacp) => *self.inner.dacp.lock().expect("dacp") = Some(dacp),
                Err(e) => tracing::warn!("airplay remote control: {e}"),
            }
            *d = Some(started);
        }
        Ok(())
    }

    /// ask the network for receivers again, for a list about to be shown or after this machine woke.
    pub fn rescan(&self) {
        if let Some(d) = self.inner.discovery.lock().expect("discovery").as_ref() {
            d.rescan();
        }
    }

    /// Receivers on the network that can take audio, then paired ones that aren't seen now.
    pub fn outputs(&self) -> Vec<AirPlayOutput> {
        let devices = self.inner.discovery.lock().expect("discovery").as_ref().map(|d| d.devices()).unwrap_or_default();
        let paired = self.inner.store.paired_ids();
        let states = self.inner.states.lock().expect("states");
        let mut list: Vec<AirPlayOutput> = devices
            .into_iter()
            .map(|d| AirPlayOutput {
                id: format!("{ID_PREFIX}{}", d.id),
                paired: paired.contains(&d.id),
                needs_pin: d.needs_pin(),
                state: states.get(&d.id).cloned(),
                address: d.addresses.iter().find(|a| a.is_ipv4()).or(d.addresses.first()).copied(),
                found: true,
                name: d.name,
                model: d.model,
                support: d.support,
            })
            .collect();
        for id in paired {
            if list.iter().any(|o| o.id.strip_prefix(ID_PREFIX).is_some_and(|x| x.eq_ignore_ascii_case(&id))) {
                continue;
            }
            let Some(saved) = self.inner.store.device(&id) else { continue };
            list.push(AirPlayOutput {
                id: format!("{ID_PREFIX}{id}"),
                name: saved.name,
                model: String::new(),
                support: Support::Ready(Route { pairing: Pairing::Pin, pcm: false }),
                paired: true,
                needs_pin: true,
                state: states.get(&id).cloned(),
                found: false,
                address: None,
            });
        }
        list
    }

    /// A receiver by output id. Waits a little, since discovery may not have found it right after start.
    fn device(&self, output_id: &str) -> Result<AirPlayDevice> {
        let id = output_id.strip_prefix(ID_PREFIX).unwrap_or(output_id);
        let deadline = std::time::Instant::now() + DISCOVERY_WAIT;
        loop {
            if let Some(d) = self.inner.discovery.lock().expect("discovery").as_ref().and_then(|d| d.find(id)) {
                return Ok(d);
            }
            if std::time::Instant::now() >= deadline {
                return Err(Error::AirPlay("the receiver isn't on the network".into()));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Asks the receiver to show its PIN.
    pub fn pair_begin(&self, output_id: &str) -> Result<()> {
        let dev = self.device(output_id)?;
        let p = self.inner.runtime().block_on(async { tokio::time::timeout(CONNECT_TIMEOUT, session::pair_begin(&dev, &self.inner.ids)).await }).map_err(|_| Error::AirPlay("the receiver didn't answer".into()))??;
        *self.inner.pairing.lock().expect("pairing") = Some((dev.id, p));
        Ok(())
    }

    /// Finishes pairing with the PIN read off the receiver.
    pub fn pair_finish(&self, output_id: &str, pin: &str) -> Result<()> {
        let id = output_id.strip_prefix(ID_PREFIX).unwrap_or(output_id).to_string();
        let (pid, p) = self.inner.pairing.lock().expect("pairing").take().ok_or_else(|| Error::AirPlay("no pairing in progress".into()))?;
        if !pid.eq_ignore_ascii_case(&id) {
            return Err(Error::AirPlay("a different receiver is pairing".into()));
        }
        let store = &self.inner.store;
        self.inner.runtime().block_on(async { tokio::time::timeout(CONNECT_TIMEOUT, session::pair_finish(p, pin.trim(), store)).await }).map_err(|_| Error::AirPlay("the receiver didn't answer".into()))??;
        self.inner.states.lock().expect("states").remove(&id);
        Ok(())
    }

    pub fn forget(&self, output_id: &str) -> Result<()> {
        let id = output_id.strip_prefix(ID_PREFIX).unwrap_or(output_id);
        self.inner.states.lock().expect("states").remove(id);
        self.inner.store.forget(id)
    }

    /// The output backend for a player: the system's devices plus AirPlay receivers.
    pub fn router(&self) -> OutputRouter {
        OutputRouter { cpal: CpalBackend::default(), airplay: self.clone() }
    }

    fn state(&self, id: &str) -> Option<OutputState> {
        self.inner.states.lock().expect("states").get(id).cloned()
    }

    fn set_state(&self, id: &str, state: Option<OutputState>) {
        let changed = {
            let mut s = self.inner.states.lock().expect("states");
            let changed = s.get(id) != state.as_ref();
            match state {
                Some(st) => s.insert(id.to_string(), st),
                None => s.remove(id),
            };
            changed
        };
        if changed && let Some(cb) = self.inner.on_change.lock().expect("on_change").clone() {
            cb();
        }
    }

    fn open(&self, output_id: &str, rate: u32, core: Arc<Mutex<RenderCore>>) -> Result<Box<dyn OutputHandle>> {
        if rate != SAMPLE_RATE {
            return Err(Error::AirPlay(format!("AirPlay streams at {SAMPLE_RATE} Hz, not {rate}")));
        }
        let dev = self.device(output_id)?;
        let route = match dev.support {
            Support::Ready(r) => r,
            Support::NeedsPtp => return Err(Error::AirPlay("this receiver needs PTP timing, which Navia doesn't do yet".into())),
            Support::AirPlay1Only => return Err(Error::AirPlay("AirPlay 1 receivers aren't supported yet".into())),
            Support::Restricted => return Err(Error::AirPlay("this receiver only allows devices of its own home".into())),
            Support::NeedsPassword => return Err(Error::AirPlay("password-protected receivers aren't supported yet".into())),
        };
        if route.pairing == Pairing::Pin && self.inner.store.device(&dev.id).is_none() {
            self.set_state(&dev.id, Some(OutputState::NeedsPairing));
            return Err(Error::AirPlay("needs pairing".into()));
        }
        self.set_state(&dev.id, Some(OutputState::Connecting));
        let result = self.connect(&dev, route).or_else(|(e, firewall)| {
            // Some receivers refuse ALAC, but every realtime receiver takes raw PCM.
            if !firewall && e.to_string().contains("audio SETUP was refused") { self.connect(&dev, Route { pcm: true, ..route }) } else { Err((e, firewall)) }
        });
        let (mut session, streaming, pcm) = match result {
            Ok(v) => v,
            Err((_, true)) => {
                // On a first connect or a reconnect, the prompt offers the rule.
                self.set_state(&dev.id, Some(OutputState::FirewallSuspected));
                return Err(Error::AirPlay("the receiver can't reach Navia: a firewall is likely blocking it".into()));
            }
            Err((e, false)) => {
                self.set_state(&dev.id, Some(OutputState::Failed(e.to_string())));
                return Err(e);
            }
        };
        let shared = core.lock().expect("core").shared().clone();
        core.lock().expect("core").set_stream_rate(SAMPLE_RATE);
        shared.output_latency.store(u64::from(SYNC_LATENCY), Ordering::Release);
        let (ctl_tx, ctl_rx) = unbounded_channel();
        let sender = match Sender::start(streaming, pcm, core, shared.clone(), ctl_tx) {
            Ok(s) => s,
            Err(e) => {
                shared.output_latency.store(0, Ordering::Release);
                self.set_state(&dev.id, Some(OutputState::Failed(e.to_string())));
                return Err(e);
            }
        };
        // Start the slider where the receiver is. Nothing is sent.
        let reported = Arc::new(Reported::default());
        let volume = self.inner.runtime().block_on(VolumeLink::start(&mut session, reported.clone()));
        let (msg_tx, msg_rx) = unbounded_channel();
        let task = Task { session, airplay: self.clone(), device_id: dev.id.clone(), state: sender.state.clone(), shared: shared.clone(), source: self.inner.config.now_playing.clone(), shown_cover: None, duration_ms: None, volume };
        *self.inner.active.lock().expect("active") = Some(msg_tx.clone());
        self.inner.runtime().spawn(task.run(ctl_rx, msg_rx));
        self.set_state(&dev.id, Some(OutputState::Playing));
        Ok(Box::new(AirPlayHandle { sender: Some(sender), shared, msgs: msg_tx, reported, airplay: self.clone() }))
    }

    /// Connects and sets up. A failure also says whether it looks like a firewall.
    fn connect(&self, dev: &AirPlayDevice, route: Route) -> std::result::Result<(Session, session::Streaming, bool), (Error, bool)> {
        let ports = self.inner.config.ports;
        let (store, ids) = (&self.inner.store, &self.inner.ids);
        let no_answer = || Error::AirPlay("the receiver didn't answer".into());
        self.inner.runtime().block_on(async {
            let started = std::time::Instant::now();
            let mut s = match tokio::time::timeout(CONNECT_TIMEOUT, Session::connect(dev, route, store, ids)).await {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => return Err((e, false)),
                Err(_) => return Err((no_answer(), false)),
            };
            // Keep the session on a setup timeout so its timing count can still be read.
            let left = CONNECT_TIMEOUT.saturating_sub(started.elapsed());
            let setup = tokio::time::timeout(left, s.setup(ports)).await;
            match setup {
                Ok(Ok(st)) => Ok((s, st, route.pcm)),
                Ok(Err(e)) => {
                    let firewall = firewall_suspected(s.timing_requests(), 0);
                    s.teardown().await;
                    Err((e, firewall))
                }
                Err(_) => {
                    let firewall = firewall_suspected(s.timing_requests(), 0);
                    s.teardown().await;
                    Err((no_answer(), firewall))
                }
            }
        })
    }
}

/// Messages from the player's handle to the session task.
enum Msg {
    /// The listener set the volume, 0 for mute.
    Volume(f32),
    /// The receiver reported its own volume over DACP.
    Reported(f32),
    NowPlaying { song_id: String, position_ms: u64, duration_ms: Option<u64> },
    Stop(std::sync::mpsc::Sender<()>),
}

struct Task {
    session: Session,
    airplay: AirPlay,
    device_id: String,
    state: Arc<SenderState>,
    shared: Arc<RenderShared>,
    source: Option<Arc<dyn NowPlayingSource>>,
    shown_cover: Option<String>,
    duration_ms: Option<u64>,
    volume: VolumeLink,
}

impl Task {
    async fn run(mut self, mut ctl: UnboundedReceiver<Ctl>, mut msgs: UnboundedReceiver<Msg>) {
        let mut keepalive = tokio::time::interval(Duration::from_secs(2));
        let mut failures = 0;
        let started = std::time::Instant::now();
        loop {
            let volume_due = self.volume.due();
            tokio::select! {
                _ = tokio::time::sleep_until(volume_due.unwrap_or_else(std::time::Instant::now).into()), if volume_due.is_some() => {
                    self.volume.send_due(&mut self.session).await;
                }
                Some(c) = ctl.recv() => {
                    let r = match c {
                        Ctl::Flush { seq, rtptime } => self.session.flush(seq, rtptime).await,
                        Ctl::Resume { seq, rtptime } => {
                            let r = self.session.record(seq, rtptime).await;
                            self.progress().await;
                            r
                        }
                    };
                    if let Err(e) = r {
                        tracing::warn!("airplay: {e}");
                    }
                }
                Some(m) = msgs.recv() => match m {
                    Msg::Volume(v) => {
                        // Sent at once if the last send was long enough ago, otherwise when due.
                        self.volume.request(v);
                        self.volume.send_due(&mut self.session).await;
                    }
                    Msg::Reported(v) => self.volume.report(v),
                    Msg::NowPlaying { song_id, position_ms, duration_ms } => self.now_playing(&song_id, position_ms, duration_ms).await,
                    Msg::Stop(done) => {
                        self.session.teardown().await;
                        let _ = done.send(());
                        return;
                    }
                },
                _ = keepalive.tick() => {
                    // Nothing at all from the receiver, so its packets aren't getting through.
                    let blocked = firewall_suspected(self.session.timing_requests(), self.state.inbound.load(Ordering::Acquire));
                    if self.session.feedback().await.is_err() {
                        failures += 1;
                        if failures >= 3 {
                            self.state.failed.store(true, Ordering::Release);
                            let state = if blocked { OutputState::FirewallSuspected } else { OutputState::Failed("the receiver stopped answering".into()) };
                            self.airplay.set_state(&self.device_id, Some(state));
                        }
                    } else {
                        failures = 0;
                        // Follow the receiver's own remote where DACP doesn't report it.
                        self.volume.poll(&mut self.session).await;
                    }
                    if failures < 3 && started.elapsed() > FIREWALL_GRACE {
                        if blocked {
                            self.airplay.set_state(&self.device_id, Some(OutputState::FirewallSuspected));
                        } else if self.airplay.state(&self.device_id) == Some(OutputState::FirewallSuspected) {
                            // Its packets arrive after all, likely because a rule was just added.
                            self.airplay.set_state(&self.device_id, Some(OutputState::Playing));
                        }
                    }
                }
                else => return,
            }
        }
    }

    /// Title, artist, album, cover and progress, timed to what the receiver is playing now.
    async fn now_playing(&mut self, song_id: &str, position_ms: u64, duration_ms: Option<u64>) {
        let Some(source) = self.source.clone() else { return };
        let id = song_id.to_string();
        let info = tokio::task::spawn_blocking({
            let s = source.clone();
            move || s.track(&id)
        })
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
        self.duration_ms = duration_ms.or(info.duration_ms);
        let rtptime = self.state.heard_rtp();
        let meta = TrackMetadata { title: Some(info.title.clone()), artist: Some(info.artist.clone()), album: Some(info.album.clone()), duration_ms: self.duration_ms.map(|d| d as u32) };
        if let Err(e) = self.session.metadata(&meta, rtptime).await {
            tracing::debug!("airplay metadata: {e}");
        }
        if info.cover != self.shown_cover {
            let art = match info.cover.clone() {
                Some(cover) => {
                    let s = source.clone();
                    tokio::task::spawn_blocking(move || s.artwork(&cover).and_then(|b| sized_jpeg(&b))).await.ok().flatten()
                }
                None => None,
            };
            let sent = match art {
                Some(jpeg) => self.session.artwork(jpeg, "image/jpeg", rtptime).await,
                None => self.session.artwork(Vec::new(), "image/none", rtptime).await,
            };
            if let Err(e) = sent {
                tracing::debug!("airplay artwork: {e}");
            }
            self.shown_cover = info.cover;
        }
        self.progress_at(position_ms).await;
    }

    async fn progress(&mut self) {
        let position = self.shared.position_ms();
        self.progress_at(position).await;
    }

    async fn progress_at(&mut self, position_ms: u64) {
        let Some(duration) = self.duration_ms else { return };
        let now = self.state.heard_rtp();
        let frames = |ms: u64| (ms * u64::from(SAMPLE_RATE) / 1000) as u32;
        let start = now.wrapping_sub(frames(position_ms));
        if let Err(e) = self.session.progress(start, now, start.wrapping_add(frames(duration))).await {
            tracing::debug!("airplay progress: {e}");
        }
    }
}

/// Cover art as a JPEG no larger than [`ART_EDGE`] on its longest side.
fn sized_jpeg(bytes: &[u8]) -> Option<Vec<u8>> {
    let img = image::load_from_memory(bytes).ok()?;
    let img = if img.width().max(img.height()) > ART_EDGE { img.resize(ART_EDGE, ART_EDGE, image::imageops::FilterType::Lanczos3) } else { img };
    let mut out = Vec::new();
    let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 88);
    img.to_rgb8().write_with_encoder(enc).ok()?;
    Some(out)
}

struct AirPlayHandle {
    sender: Option<Sender>,
    shared: Arc<RenderShared>,
    msgs: UnboundedSender<Msg>,
    reported: Arc<Reported>,
    airplay: AirPlay,
}

impl OutputHandle for AirPlayHandle {
    fn latency_frames(&self) -> u64 {
        u64::from(SYNC_LATENCY)
    }
    fn failed(&self) -> bool {
        self.sender.as_ref().is_some_and(|s| s.state.failed.load(Ordering::Acquire))
    }
    fn rate(&self) -> u32 {
        SAMPLE_RATE
    }
    fn xruns(&self) -> u64 {
        0
    }
    fn remote_volume(&self) -> bool {
        true
    }
    fn set_volume(&self, volume: f32) {
        let _ = self.msgs.send(Msg::Volume(volume));
    }
    fn reported_volume(&self) -> Option<(u64, f32)> {
        self.reported.get()
    }
    fn now_playing(&self, song_id: &str, position_ms: u64, duration_ms: Option<u64>) {
        let _ = self.msgs.send(Msg::NowPlaying { song_id: song_id.to_string(), position_ms, duration_ms });
    }
}

impl Drop for AirPlayHandle {
    fn drop(&mut self) {
        if let Some(mut s) = self.sender.take() {
            s.stop();
        }
        self.shared.output_latency.store(0, Ordering::Release);
        {
            let mut active = self.airplay.inner.active.lock().expect("active");
            if active.as_ref().is_some_and(|a| a.same_channel(&self.msgs)) {
                *active = None;
            }
        }
        let (tx, rx) = std::sync::mpsc::channel();
        if self.msgs.send(Msg::Stop(tx)).is_ok() {
            // TEARDOWN so the receiver leaves its music screen, without holding the player long.
            let _ = rx.recv_timeout(Duration::from_secs(2));
        }
    }
}

/// The system's devices plus AirPlay receivers, as one output backend.
pub struct OutputRouter {
    cpal: CpalBackend,
    airplay: AirPlay,
}

impl OutputBackend for OutputRouter {
    fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        let mut list = self.cpal.list_devices()?;
        list.extend(self.airplay.outputs().into_iter().filter(|o| o.found && matches!(o.support, Support::Ready(_))).map(|o| DeviceInfo { id: o.id, name: o.name, is_default: false }));
        Ok(list)
    }

    fn caps(&self, device: Option<&str>) -> Result<DeviceCaps> {
        match device {
            Some(id) if id.starts_with(ID_PREFIX) => Ok(DeviceCaps { rates: vec![(SAMPLE_RATE, SAMPLE_RATE)], default_rate: SAMPLE_RATE }),
            _ => self.cpal.caps(device),
        }
    }

    fn open(&mut self, device: Option<&str>, rate: u32, core: Arc<Mutex<RenderCore>>) -> Result<Box<dyn OutputHandle>> {
        match device {
            Some(id) if id.starts_with(ID_PREFIX) => self.airplay.open(id, rate, core),
            _ => self.cpal.open(device, rate, core),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::firewall_suspected;

    #[test]
    fn firewall_is_suspected_only_when_the_receiver_was_told_our_ports_and_never_answered() {
        // Failed before SETUP sent our ports, so not a firewall.
        assert!(!firewall_suspected(None, 0));
        // Told our ports, and nothing came back.
        assert!(firewall_suspected(Some(0), 0));
        // It asked for timing or sent to the control port, so packets get through.
        assert!(!firewall_suspected(Some(3), 0));
        assert!(!firewall_suspected(Some(0), 1));
    }
}
