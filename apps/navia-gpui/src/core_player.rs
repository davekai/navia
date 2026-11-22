//! Wires the shell to the real player, library rows and desktop integrations. The shell keeps its
//! plain `Player` view state, and this module fills it from player events and sends commands back.

use crate::i18n::{tr, trf, n_};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use gpui::Context;
use navia_core::audio::output::{CaptureBackend, CpalBackend, OutputBackend};
use navia_core::audio::processing::{EqBand, EqSettings, FilterShape};
use navia_core::downloads::{DownloadEvent, DownloadRequest};
use navia_core::audio::player::{OutputPolicy, Player as CorePlayer, PlayerConfig, PlayerEvent, PlayerStatus, QueueItem, Repeat, ServerResolver};
use navia_core::library::StarTable;
use navia_core::play_queue::{Edit, PlayQueue};
use navia_core::queue_order::QueueSource;

use crate::queue_bridge::EditJob;
use navia_core::session::attach_reporting;
use navia_integrations::art::{ArtSource, NoArt, PublicAddress, PublicArt};
use navia_integrations::discord::{DiscordConfig, DiscordPresence};
use navia_integrations::media_controls::{MediaControls, MediaControlsConfig};
use navia_integrations::{Integration, LoopMode, PlaybackState, PlayerCommand, PlayerControl, PlayerSnapshot, PlayerState, Runner, TrackInfo, spawn_runner};
use serde_json::{Value, json};

use crate::core_library::CoreLibrary;
use crate::persist::DiscordArt;
use crate::model::{Library as _, Song};
use crate::shell::Shell;

/// `song_ix` for something with no library row, such as a station or a followed song this library lacks.
pub(crate) const LIVE_ROW: usize = u32::MAX as usize;

/// Rows kept in memory, so scrolling back doesn't refetch while the total stays small.
const ROW_CACHE_LIMIT: usize = 4096;
const SESSION_SAVE_EVERY: Duration = Duration::from_secs(5);
/// How long position reports are ignored after a seek, until the engine catches up.
const SEEK_SETTLE: Duration = Duration::from_millis(1500);
/// The toasts shown while offline.
const NEEDS_SERVER: &str = n_("This song isn't downloaded. It needs the server.");
pub const STARS_NEED_SERVER: &str = n_("Favorites need the server. Try again when you're back online.");

pub enum CoreEvent {
    Player(PlayerEvent),
    Command(PlayerCommand),
    Connected(bool),
    /// The server refused the saved login.
    LoginRejected,
    SyncProgress,
    SyncFailed,
    SyncDone,
    /// a page's refresh ended.
    Refreshed,
    Rows { rows: Vec<(u32, Song)>, requested: Vec<u32> },
    Order { generation: u64, order: Vec<u32>, total: Option<usize> },
    /// What the Songs filters know about every song, reread after the library changes.
    FilterIndex(Arc<navia_core::song_filters::FilterIndex>),
    /// The Artists page's matches for its search and filters, with `None` meaning every artist, and
    /// the artists' filter data.
    ArtistMatches { generation: u64, matches: Option<std::collections::HashSet<gpui::SharedString>>, index: Option<Arc<navia_core::artist_filters::ArtistFilterIndex>> },
    Starred(Vec<u32>),
    Restore { queue: Box<PlayQueue<u32>>, position_ms: u64, repeat: Repeat, source: Option<QueueSource> },
    PlayRows(Vec<u32>),
    Downloaded(Vec<u32>),
    Download(DownloadEvent),
    CollectionChanged,
    /// The answer to a song or artist radio request, as library rows.
    RadioReady { seed: RadioSeed, rows: Vec<u32> },
    /// The server refused a star, so undo it.
    StarFailed { table: StarTable, id: String, rowid: Option<u32>, on: bool },
    /// An answer from the server arrived for a page that's showing.
    RemoteReady,
    /// AirPlay receivers came or went, or one's state changed.
    AirPlayChanged,
    /// `createPlaylist` finished. `id` is empty when the server didn't say which it made.
    /// `with_songs` means it was made from Add to Playlist and holds the songs, so the app stays put.
    PlaylistCreated { id: String, name: String, with_songs: bool },
    PlaylistCreateFailed { message: String },
    /// Share link results, for the list, a new link, a saved edit or a deleted link.
    Shares(Result<Vec<navia_core::shares::ShareLink>, String>),
    ShareCreated(Result<navia_core::shares::ShareLink, String>),
    ShareSaved(Result<(), String>),
    ShareDeleted(Result<String, String>),
    /// An image upload or removal finished, so refresh this playlist's cover everywhere.
    PlaylistImageChanged { id: String, removed: bool },
    PlaylistImageFailed { message: String },
    /// A smart playlist's name and rules were saved. `created` means it's new.
    SmartSaved { id: String, name: String, created: bool },
    SmartSaveFailed { message: String },
    /// A playlist's new order was saved, or why not. The stored copy is current either way.
    PlaylistOrderSaved { id: String, result: Result<(), navia_core::library::sync::PlaylistEditError> },
    /// A playlist's name, description or Public setting was saved, or the reason it wasn't.
    PlaylistDetailsSaved { id: String, result: Result<(), String> },
    PlaylistDeleted { id: String, result: Result<(), String> },
    /// songs were added or removed, or weren't and why; the stored copy is current either way. `toast` replaces the success message.
    PlaylistSongsChanged { id: String, name: String, added: bool, toast: Option<String>, result: Result<(), navia_core::library::sync::PlaylistEditError> },
    TrackExtras(TrackExtras),
    /// What Autoplay offers now, or with `fetched`, that new answers arrived.
    Autoplay { generation: u64, rows: Vec<u32>, ready: bool, fetched: bool, wants_fetch: bool },
    /// The server holds a newer queue from another client or device.
    QueueOffer(QueueOffer),
}

/// What a radio queue starts from. Sent back with the answer, so the shell knows what to play.
#[derive(Clone, Debug)]
pub enum RadioSeed {
    Artist(String),
    Song { id: String, rowid: u32 },
    /// A song played from Search. It's already playing, and the radio fills Up Next after it if
    /// nothing else was played since. `play` is `Shell::plays` when it started.
    After { id: String, rowid: u32, play: u64 },
}

struct RowEntry {
    song: Song,
    used: u64,
}

/// A bounded cache of song rows by row id, filled in the background.
#[derive(Default)]
struct RowCache {
    rows: HashMap<u32, RowEntry>,
    in_flight: HashSet<u32>,
    wanted: Vec<u32>,
    frame: u64,
}

impl RowCache {
    fn get(&mut self, rowid: u32) -> Option<&Song> {
        let frame = self.frame;
        self.rows.get_mut(&rowid).map(|e| {
            e.used = frame;
            &e.song
        })
    }

    fn want(&mut self, rowid: u32) {
        if !self.rows.contains_key(&rowid) && self.in_flight.insert(rowid) {
            self.wanted.push(rowid);
        }
    }

    fn insert(&mut self, rowid: u32, song: Song) {
        let used = self.frame;
        self.rows.insert(rowid, RowEntry { song, used });
        if self.rows.len() > ROW_CACHE_LIMIT {
            let keep_after = {
                let mut used: Vec<u64> = self.rows.values().map(|e| e.used).collect();
                used.sort_unstable();
                used[self.rows.len() - ROW_CACHE_LIMIT * 3 / 4]
            };
            self.rows.retain(|_, e| e.used >= keep_after);
        }
    }

    fn clear(&mut self) {
        self.rows.clear();
    }
}

/// What the integrations read. The shell updates it whenever player state changes.
#[derive(Default)]
struct IntegrationBridge {
    snapshot: Mutex<PlayerSnapshot>,
}

impl PlayerState for IntegrationBridge {
    fn snapshot(&self) -> PlayerSnapshot {
        self.snapshot.lock().unwrap().clone()
    }
}

/// Media keys and OS controls, posted to the shell, which owns the player's state. The target
/// changes with the signed-in account, while the OS registration lasts for the app's life.
struct Controller(Arc<Mutex<Option<UnboundedSender<CoreEvent>>>>);

impl PlayerControl for Controller {
    fn command(&self, command: PlayerCommand) {
        if let Some(tx) = self.0.lock().unwrap().as_ref() {
            let _ = tx.unbounded_send(CoreEvent::Command(command));
        }
    }
}

impl Integrations {
    fn notify(&self) {
        if let Some(runner) = self.media.as_ref() {
            runner.notify();
        }
        if let Some((_, _, runner)) = self.discord.as_ref() {
            runner.notify();
        }
    }
}

/// OS media controls and Discord, which outlive any one account.
#[derive(Default)]
pub struct Integrations {
    bridge: Arc<IntegrationBridge>,
    pub(crate) target: Arc<Mutex<Option<UnboundedSender<CoreEvent>>>>,
    media: Option<Runner>,
    media_started: bool,
    /// The main window's HWND on Windows, which media controls need. Set when the window attaches.
    pub hwnd: Option<usize>,
    /// The Discord runner, while the setting allows it, with the art source and library address it
    /// was started with.
    discord: Option<(DiscordArt, usize, Runner)>,
    /// Where Discord covers come from, shared live with Discord's art source.
    discord_address: PublicAddress,
    /// Whether the connected server's address counts as public, by address, for Settings. Checked
    /// off the UI thread since it needs DNS. `None` while checking.
    pub server_public: Option<(String, Option<bool>)>,
}

/// What the library adds to the playing song for the integrations, found off the UI thread.
pub struct TrackExtras {
    pub song_id: String,
    /// Artwork for the OS media controls.
    pub cover: Option<String>,
    pub mbid: Option<String>,
    pub isrc: Option<String>,
}

pub struct Core {
    pub lib: Arc<CoreLibrary>,
    pub player: Arc<CorePlayer>,
    /// AirPlay receivers as outputs. `None` in test runs, which never reach a device.
    pub airplay: Option<navia_core::audio::airplay::AirPlay>,
    pub(crate) events: UnboundedSender<CoreEvent>,
    rows: RowCache,
    row_tx: Sender<Vec<u32>>,
    order_generation: u64,
    /// The queue model, with its list, play order, Next in Queue, Autoplay's songs and what the
    /// player holds. Every queue change goes through it.
    pub(crate) pq: PlayQueue<u32>,
    /// The player's edits, in order.
    pub(crate) edits: Sender<EditJob>,
    /// The queue's revision last written to `queue.json`.
    saved_queue: Option<u64>,
    /// Writes `queue.json`, in order, off the UI thread.
    saver: Sender<Box<dyn FnOnce() + Send>>,
    /// The position shown while a seek bar is dragged and after a seek, until the engine reports.
    seek_guard: Option<(u64, Instant)>,
    last_saved: Instant,
    extras: Option<TrackExtras>,
    sync_notified: Option<Instant>,
    pub connected: Option<bool>,
    maintenance_ticks: u32,
    /// The equalizer settings last sent to the player, so unchanged frames send nothing.
    eq_plan: Option<Option<crate::eq_model::EqPlan>>,
    /// A newer queue from another client or device, offered in the player until answered.
    pub queue_offer: Option<QueueOffer>,
}

/// Another device's queue as this library's rows, for "Continue from <device>?".
pub struct QueueOffer {
    pub device: gpui::SharedString,
    pub rows: Vec<u32>,
    pub index: usize,
    pub position_ms: u64,
    pub offer: navia_core::queue_sync::RemoteQueue,
}

/// Picks the audio backend so test runs can never reach a real device. A real run also gets
/// AirPlay receivers beside the system's devices.
fn audio_backend(airplay: Option<&navia_core::audio::airplay::AirPlay>) -> Box<dyn OutputBackend> {
    if let Some(ap) = airplay {
        return Box::new(ap.router());
    }
    real_or_capture_backend()
}

/// Whether this run may use real outputs. Scripted and benchmark runs use a test sink.
fn real_outputs_allowed() -> bool {
    let driven = std::env::var_os("NAVIA_SCRIPT").is_some() || std::env::var_os("NAVIA_SCROLL_BENCH").is_some();
    let real_output_allowed = std::env::var_os("NAVIA_TEST_REAL_OUTPUT").is_some_and(|v| v == "1");
    !driven || real_output_allowed
}

fn real_or_capture_backend() -> Box<dyn OutputBackend> {
    let driven = std::env::var_os("NAVIA_SCRIPT").is_some() || std::env::var_os("NAVIA_SCROLL_BENCH").is_some();
    // Set on purpose for the one check that playback reaches the real default device.
    let real_output_allowed = std::env::var_os("NAVIA_TEST_REAL_OUTPUT").is_some_and(|v| v == "1");
    if driven && !real_output_allowed {
        let allowed = std::env::var("NAVIA_TEST_SINK").ok().filter(|s| !s.is_empty());
        let target = std::env::var("PIPEWIRE_NODE").ok();
        if allowed.is_none() || allowed != target {
            eprintln!("navia: test run without NAVIA_TEST_SINK matching PIPEWIRE_NODE, so audio goes to an in-memory capture and no device");
            return Box::new(CaptureBackend::default());
        }
    }
    Box::new(CpalBackend::default())
}

/// Keeps glibc's per-thread arenas from holding on to freed memory. Call first in `main`.
pub fn limit_allocator_arenas() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        if std::env::var_os("NAVIA_NO_MEM_TUNING").is_some() {
            return;
        }
        const M_ARENA_MAX: i32 = -8;
        unsafe extern "C" {
            fn mallopt(param: i32, value: i32) -> i32;
        }
        unsafe {
            mallopt(M_ARENA_MAX, 2);
        }
    }
}

/// Converts the slider's 0 to 100 to the player's volume, squared so the slider feels even.
fn volume_gain(percent: f32) -> f32 {
    let v = (percent / 100.0).clamp(0.0, 1.0);
    v * v
}

/// The inverse of [`volume_gain`], a reported volume as the slider's 0 to 100.
fn volume_percent_of(gain: f32) -> f32 {
    gain.clamp(0.0, 1.0).sqrt() * 100.0
}

/// A thread that runs file writes in the order they're sent, for as long as the sender lives.
fn spawn_saver() -> Sender<Box<dyn FnOnce() + Send>> {
    let (tx, rx) = mpsc::channel::<Box<dyn FnOnce() + Send>>();
    std::thread::Builder::new()
        .name("navia-queue-save".into())
        .spawn(move || {
            for job in rx {
                job();
            }
        })
        .expect("spawn queue saver");
    tx
}

/// The player's saved queue for the signed-in account.
fn session_path() -> Option<PathBuf> {
    crate::persist::account_file("session.json")
}

/// Adopts the queue saved before per-account folders. The first account to look takes it, and
/// its song id check drops it if it belongs to another library.
fn adopt_legacy_session(path: &std::path::Path) {
    let Some(old) = crate::persist::config_dir().map(|d| d.join("session.json")) else { return };
    if !path.exists() && old.exists() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::rename(old, path);
    }
}

/// A test override from the environment. `Some(true)` for `=1`, `Some(false)` for anything else.
fn env_flag(name: &str) -> Option<bool> {
    std::env::var_os(name).map(|v| v == "1")
}

/// Why the Scrobble switch can't be changed in this run, if it can't.
pub fn scrobble_override() -> Option<&'static str> {
    if crate::core_library::read_only_run() {
        Some(tr("Off for this read-only test run."))
    } else if env_flag("NAVIA_REPORT_PLAYS").is_some() {
        Some(tr("Set by NAVIA_REPORT_PLAYS for this run."))
    } else {
        None
    }
}

/// Why the Discord status switch can't be changed in this run, if it can't.
pub fn discord_override() -> Option<&'static str> {
    if env_flag("NAVIA_DISCORD").is_some() {
        Some(tr("Set by NAVIA_DISCORD for this run."))
    } else if quiet_run() {
        Some(tr("Off for this test run."))
    } else {
        None
    }
}

/// True for scripted, benchmark and read-only test runs, which report nothing by default.
fn quiet_run() -> bool {
    env_flag("NAVIA_READ_ONLY") == Some(true) || std::env::var_os("NAVIA_SCRIPT").is_some() || std::env::var_os("NAVIA_SCROLL_BENCH").is_some()
}

impl Shell {
    /// Whether plays should go to the server now. Needs the user's setting or a test override, and
    /// neither Private mode nor a read-only run.
    pub fn reporting_wanted(&self) -> bool {
        !crate::core_library::read_only_run() && env_flag("NAVIA_REPORT_PLAYS").unwrap_or(self.scrobble_setting())
    }

    /// This account's Scrobble switch. The user's choice, or on if they never changed it.
    pub fn scrobble_setting(&self) -> bool {
        self.prefs.scrobble_choices.get(&self.order_key()).copied().unwrap_or(true)
    }

    /// Toggles scrobbling, stored as an explicit choice for this account.
    pub fn toggle_scrobble(&mut self) {
        let next = !self.scrobble_setting();
        let key = self.order_key();
        self.prefs.scrobble_choices.insert(key, next);
        self.prefs.save();
        self.apply_reporting();
    }

    /// Whether Discord should show this app's status at all.
    pub fn discord_wanted(&self) -> bool {
        match env_flag("NAVIA_DISCORD") {
            Some(forced) => forced,
            None => !quiet_run() && self.prefs.discord_status,
        }
    }

    /// Applies the settings that decide what leaves the app. Call after any of them changes.
    pub fn apply_reporting(&mut self) {
        let on = self.reporting_wanted() && !self.private_mode;
        if let Some(core) = self.core.as_ref() {
            core.lib.set_reporting(on);
        }
        self.sync_discord();
    }

    /// Passes the cover source, connected server and public address settings to Discord's art source.
    /// The address is passed only once typing is done, so a half-typed one is never looked up.
    fn apply_discord_address(&mut self) {
        let address = &self.integrations.discord_address;
        address.set_source(self.prefs.discord_covers_from);
        address.set_server(self.core.as_ref().map(|c| c.lib.server_url()));
        if self.controls.text == Some(crate::ui_controls::TextTarget::DiscordAddress) {
            return;
        }
        address.set_custom(navia_integrations::art::parse_public_address(&self.prefs.discord_public_address).ok().flatten());
    }

    /// Whether the connected server's address counts as public for covers, for Settings. `None`
    /// while it's being checked, once per address off the UI thread.
    pub fn server_address_public(&mut self, cx: &mut Context<Self>) -> Option<bool> {
        let url = self.core.as_ref().map(|c| c.lib.server_url().to_owned())?;
        if let Some((asked, known)) = &self.integrations.server_public
            && *asked == url
        {
            return *known;
        }
        self.integrations.server_public = Some((url.clone(), None));
        cx.spawn(async move |this, cx| {
            let lookup = url.clone();
            let public = cx.background_executor().spawn(async move { navia_integrations::art::public_server_address(&lookup).is_some() }).await;
            let _ = this.update(cx, |this, cx| {
                if let Some((asked, known)) = this.integrations.server_public.as_mut()
                    && *asked == url
                {
                    *known = Some(public);
                    cx.notify();
                }
            });
        })
        .detach();
        None
    }

    /// Starts or stops Discord status to match the settings, and switches its art source.
    fn sync_discord(&mut self) {
        self.apply_discord_address();
        let wanted = self.discord_wanted() && self.core.is_some();
        let art = self.prefs.discord_art;
        let lib = self.core.as_ref().map(|c| c.lib.clone());
        let lib_key = lib.as_ref().map_or(0, |l| Arc::as_ptr(l) as usize);
        if self.integrations.discord.as_ref().is_some_and(|(running, on, _)| wanted && *running == art && *on == lib_key) {
            return;
        }
        // Dropping a runner joins its thread, so do it off the UI thread.
        if let Some(old) = self.integrations.discord.take() {
            std::thread::spawn(move || drop(old));
        }
        let Some(lib) = lib.filter(|_| wanted) else { return };
        let address = self.integrations.discord_address.clone();
        let source: Arc<dyn ArtSource> = match art {
            DiscordArt::MatchPublic => PublicArt::new().and_then(|a| a.with_server(lib, address)).map_or_else(|_| Arc::new(NoArt) as Arc<dyn ArtSource>, |a| Arc::new(a)),
            DiscordArt::Default => Arc::new(NoArt),
        };
        // Navia's Discord application, the same ID as navia-ffi's DISCORD_APP_ID.
        let mut config = DiscordConfig::new("1399802340940185642");
        config.default_image = Some("navia".to_string());
        match DiscordPresence::new(config, source) {
            Ok(discord) => {
                let integrations: Vec<Box<dyn Integration + Send>> = vec![Box::new(discord)];
                let runner = spawn_runner(self.integrations.bridge.clone(), integrations, Duration::from_secs(1));
                runner.notify();
                self.integrations.discord = Some((art, lib_key, runner));
            }
            Err(e) => eprintln!("navia: Discord unavailable: {e}"),
        }
    }

    pub(crate) fn ensure_media_controls(&mut self, tx: UnboundedSender<CoreEvent>) {
        *self.integrations.target.lock().unwrap() = Some(tx);
        if self.integrations.media_started {
            return;
        }
        // Windows media controls panic without a window handle, so wait for the window to attach.
        if cfg!(target_os = "windows") && self.integrations.hwnd.is_none() {
            return;
        }
        self.integrations.media_started = true;
        let hwnd = self.integrations.hwnd.map(|h| h as *mut std::ffi::c_void);
        match MediaControls::new(MediaControlsConfig { display_name: navia_core::client::APP_DISPLAY_NAME.to_string(), dbus_name: "navia".to_string(), desktop_entry: navia_core::client::APP_ID.to_string(), hwnd }, Arc::new(Controller(self.integrations.target.clone()))) {
            Ok(controls) => {
                let integrations: Vec<Box<dyn Integration + Send>> = vec![Box::new(controls)];
                self.integrations.media = Some(spawn_runner(self.integrations.bridge.clone(), integrations, Duration::from_secs(1)));
            }
            Err(e) => eprintln!("navia: media controls unavailable: {e}"),
        }
    }

    /// Connects the real library and player. The network side runs in the background.
    pub fn attach_core(&mut self, lib: Arc<CoreLibrary>, cx: &mut Context<Self>) {
        crate::persist::set_active_account(Some(crate::account::account_key(lib.server_url(), lib.username())));
        self.load_account_state();
        let (tx, mut rx) = unbounded::<CoreEvent>();
        self.core_generation += 1;
        let generation = self.core_generation;
        cx.spawn(async move |this, cx| {
            while let Some(event) = rx.next().await {
                let live = this.update(cx, |shell, cx| {
                    let live = shell.core_generation == generation;
                    if live {
                        shell.on_core_event(event, cx);
                    }
                    live
                });
                if !matches!(live, Ok(true)) {
                    return;
                }
            }
        })
        .detach();

        // Listening Now, following and the reports' share mark.
        self.start_listen_checks(generation, cx);
        self.load_deezer_choice(&lib);
        self.autoplay_mode = lib.autoplay_mode();
        if self.prefs.share_listening {
            let lib = lib.clone();
            cx.background_executor().spawn(async move { lib.set_sharing(true) }).detach();
        }

        // The system's metered signal picks the streaming quality for the next track.
        #[cfg(any(target_os = "linux", windows))]
        {
            let control = lib.downloads.quality_control();
            navia_integrations::metered::spawn_watcher(move |metered| control.set_metered(metered));
        }

        let config = PlayerConfig { policy: OutputPolicy::FollowTrack, ..Default::default() };
        let airplay = if real_outputs_allowed() { crate::airplay::start(&lib, tx.clone()) } else { None };
        let player = {
            let _guard = lib.enter_runtime();
            let resolver = Arc::new(ServerResolver::new(lib.client.clone()).expect("stream resolver").with_downloads(lib.downloads.clone()));
            match CorePlayer::with_backend(resolver, audio_backend(airplay.as_ref()), config) {
                Ok(p) => Arc::new(p),
                Err(e) => {
                    eprintln!("navia: no audio output: {e}");
                    return;
                }
            }
        };
        player.set_album_lookup(navia_core::audio::player::library_album_lookup(lib.db.clone()));
        player.set_edge_store(navia_core::audio::player::library_edge_store(lib.db.clone()));
        // The song's name on the sound server's stream (the desktop's mixer, Discord's app audio).
        player.set_title_lookup(navia_core::audio::player::library_title_lookup(lib.db.clone()));
        player.set_volume(volume_gain(self.prefs.volume));
        player.set_muted(self.prefs.muted);
        // The reporter stays silent unless the settings allow it. See `apply_reporting`.
        if lib.writes_allowed {
            let _guard = lib.enter_runtime();
            attach_reporting(&player, lib.reporter.clone());
        } else {
            eprintln!("navia: read-only run, nothing is written to the server");
        }
        // The play log is local only, so read-only runs keep it too.
        navia_core::engine::playlog::attach_play_log(&player, lib.db.clone());
        let forward = tx.clone();
        player.add_listener(move |event: &PlayerEvent| {
            if !matches!(event, PlayerEvent::Heard { .. }) {
                let _ = forward.unbounded_send(CoreEvent::Player(event.clone()));
            }
        });

        let (row_tx, row_rx) = mpsc::channel::<Vec<u32>>();
        {
            let (lib, tx) = (lib.clone(), tx.clone());
            std::thread::Builder::new()
                .name("navia-rows".into())
                .spawn(move || {
                    while let Ok(mut batch) = row_rx.recv() {
                        // Fast scrolling queues more than a screen, but only the newest batch matters.
                        let mut skipped = Vec::new();
                        while let Ok(newer) = row_rx.try_recv() {
                            skipped.append(&mut batch);
                            batch = newer;
                        }
                        let rows = lib.rows(&batch);
                        batch.extend(skipped);
                        if tx.unbounded_send(CoreEvent::Rows { rows, requested: batch }).is_err() {
                            return;
                        }
                    }
                })
                .expect("spawn row loader");
        }

        {
            let forward = tx.clone();
            lib.downloads.add_listener(move |event: &DownloadEvent| {
                let _ = forward.unbounded_send(CoreEvent::Download(event.clone()));
            });
            // A downloaded playlist or album changed its switch, songs or progress.
            let forward = tx.clone();
            lib.downloads.add_collection_listener(move |_| {
                let _ = forward.unbounded_send(CoreEvent::CollectionChanged);
            });
        }

        self.ensure_media_controls(tx.clone());
        let player_for_edits = player.clone();
        self.core = Some(Core { lib: lib.clone(), player, airplay, events: tx.clone(), rows: RowCache::default(), row_tx, order_generation: 0, pq: PlayQueue::new(), edits: crate::queue_bridge::spawn_edit_worker(lib.clone(), player_for_edits), saved_queue: None, saver: spawn_saver(), seek_guard: None, last_saved: Instant::now(), extras: None, sync_notified: None, connected: None, maintenance_ticks: 0, eq_plan: None, queue_offer: None });
        // Once per run, so on quit the queue is saved and the server's now-playing entry is ended for
        // whichever account is signed in then.
        static QUIT_HOOK: std::sync::Once = std::sync::Once::new();
        QUIT_HOOK.call_once(|| {
            cx.on_app_quit(|this: &mut Self, _| {
                this.core_save_session();
                if let Some(core) = this.core.as_ref() {
                    // Write the queue file before the app exits.
                    let (done, wait) = mpsc::channel::<()>();
                    if core.saver.send(Box::new(move || {
                        let _ = done.send(());
                    })).is_ok() {
                        let _ = wait.recv_timeout(Duration::from_secs(3));
                    }
                    core.lib.sync_before_quit();
                }
                async {}
            })
            .detach();
        });
        self.rebuild_order();
        self.core_refresh_starred(cx);
        self.core_refresh_downloaded(cx);
        self.private_mode = self.prefs.private_mode;
        self.apply_reporting();
        // before the background starts, so a launch with the switch on never tries the server
        self.offline_switch = self.prefs.offline_accounts.contains(&lib.account_key());
        lib.set_offline(self.offline_switch);
        lib.set_background_sync(self.prefs.library_sync);
        self.refresh_downloads(cx);
        self.refresh_synced(cx);
        self.refresh_storage(cx);

        lib.start_background(tx.clone());
        self.core_restore_session(cx);
        self.core_check_queue_offer(cx);
    }

    /// Per-account housekeeping. Deletes the unused legacy `pins.json`.
    fn load_account_state(&mut self) {
        if let Some(path) = crate::persist::account_file("pins.json") {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Stops everything belonging to the signed-in account and clears its data from memory, so
    /// another account can start clean. Its queue, library and downloads stay on disk.
    pub fn detach_core(&mut self, cx: &mut Context<Self>) {
        self.core_save_session();
        if let Some(core) = self.core.as_ref() {
            core.lib.sync_before_quit();
        }
        self.core_generation += 1;
        crate::persist::set_active_account(None);
        if let Some(core) = self.core.take() {
            *self.integrations.target.lock().unwrap() = None;
            core.player.stop();
            core.lib.close();
            // Dropping the player and library joins threads, so do it off the UI thread.
            std::thread::spawn(move || drop(core));
        }
        // Drop stale art jobs and results, and the old account's art.
        self.art.close();
        self.art = crate::art::ArtWorkers::idle();
        self.thumbs = crate::thumb_cache::ThumbCache::default();
        self.sync_discord();
        self.publish_empty_integrations();
        self.reset_account_view(cx);
    }

    fn publish_empty_integrations(&mut self) {
        *self.integrations.bridge.snapshot.lock().unwrap() = PlayerSnapshot::default();
        self.integrations.notify();
    }

    /// Requests the list order in the background and applies it when it arrives.
    pub fn core_rebuild_order(&mut self) {
        let Some(core) = self.core.as_mut() else { return };
        core.order_generation += 1;
        let (lib, tx, generation) = (core.lib.clone(), core.events.clone(), core.order_generation);
        let (sort, query, filters) = (self.sort, self.query.clone(), self.song_filters.state.clone());
        // Column sorts read A to Z and shortest first, reversed by a second header click. The core
        // lists durations longest first.
        let reverse = match sort {
            crate::shell::SortKey::Length => !self.sort_desc,
            crate::shell::SortKey::Title | crate::shell::SortKey::Artist | crate::shell::SortKey::Album | crate::shell::SortKey::MostPlayed => self.sort_desc,
            // Date Added ascending is oldest first, while the list's own order is newest first.
            crate::shell::SortKey::Added => self.songs_added_head && !self.sort_desc,
            _ => false,
        };
        std::thread::spawn(move || {
            let mut order = lib.order(sort, &query, &filters);
            if reverse {
                order.reverse();
            }
            let total = (query.trim().is_empty() && filters.is_empty()).then_some(order.len());
            let _ = tx.unbounded_send(CoreEvent::Order { generation, order, total });
            // The core caches this until the library changes, so it's cheap after the first read.
            if let Ok(index) = lib.db.song_filters() {
                let _ = tx.unbounded_send(CoreEvent::FilterIndex(index));
            }
        });
    }

    /// Refreshes which rows are on disk. Row ids can move after a sync, so this runs then too.
    fn core_refresh_downloaded(&mut self, _cx: &mut Context<Self>) {
        let Some(core) = self.core.as_ref() else { return };
        let (lib, tx) = (core.lib.clone(), core.events.clone());
        std::thread::spawn(move || {
            let rows = lib.db.downloaded_rowids().unwrap_or_default();
            let _ = tx.unbounded_send(CoreEvent::Downloaded(rows));
        });
    }

    /// Downloads songs for offline play, or removes them. Only a finished file counts as downloaded.
    /// Until then a row shows its progress and can be canceled.
    pub fn core_set_downloaded(&mut self, songs: &[u32], on: bool, cx: &mut Context<Self>) {
        let Some(core) = self.core.as_ref() else { return };
        let (lib, rows) = (core.lib.clone(), songs.to_vec());
        if on {
            for s in songs {
                if !self.downloaded.contains(s) {
                    self.download_progress.entry(*s).or_insert(0.0);
                }
            }
        } else {
            for s in songs {
                self.downloaded.remove(s);
                self.download_progress.remove(s);
            }
        }
        cx.background_executor()
            .spawn(async move {
                let ids = lib.queue_ids(&rows);
                if on {
                    lib.downloads.enqueue(ids.into_iter().map(|(_, id, _)| DownloadRequest { id, suffix: None, size: None }).collect());
                } else {
                    let ids: Vec<String> = ids.into_iter().map(|(_, id, _)| id).collect();
                    let _ = lib.downloads.remove(&ids);
                }
            })
            .detach();
        cx.notify();
    }

    fn core_refresh_starred(&mut self, _cx: &mut Context<Self>) {
        let Some(core) = self.core.as_ref() else { return };
        let (lib, tx) = (core.lib.clone(), core.events.clone());
        std::thread::spawn(move || {
            let _ = tx.unbounded_send(CoreEvent::Starred(lib.starred_rowids()));
        });
    }

    /// The song at a row of the list, or an empty placeholder while it loads.
    pub fn song_for_row(&mut self, order_ix: usize) -> Song {
        let Some(&rowid) = self.order.get(order_ix) else { return Song::default() };
        if self.core.is_none() {
            return self.songs.get(rowid as usize).cloned().unwrap_or_default();
        }
        // Read around the visible rows, so a slow scroll almost never sees a placeholder.
        let (start, end) = (order_ix.saturating_sub(16), (order_ix + 48).min(self.order.len()));
        let neighbours: Vec<u32> = self.order[start..end].to_vec();
        let core = self.core.as_mut().unwrap();
        core.rows.frame = self.frame;
        if let Some(song) = core.rows.get(rowid) {
            return song.clone();
        }
        for id in neighbours {
            core.rows.want(id);
        }
        Song::default()
    }

    /// The song at an index, for pages that hold song indexes. Blank in a real library until its
    /// row arrives.
    pub fn song_at(&mut self, ix: u32) -> Option<Song> {
        if self.core.is_some() { Some(self.song_by_rowid(ix)) } else { self.songs.get(ix as usize).cloned() }
    }

    /// A song only if it's already in memory, for filtering lists.
    pub fn song_peek(&self, ix: u32) -> Option<Song> {
        match self.core.as_ref() {
            Some(core) => core.rows.rows.get(&ix).map(|e| e.song.clone()),
            None => self.songs.get(ix as usize).cloned(),
        }
    }

    /// Reads rows ahead so a page's filter box can see them. Capped for huge playlists.
    pub fn core_preload_rows(&mut self, rows: &[u32]) {
        let Some(core) = self.core.as_mut() else { return };
        for id in rows.iter().take(ROW_CACHE_LIMIT / 2) {
            core.rows.want(*id);
        }
    }

    /// The most-played songs of the library, for charts.
    pub fn top_rows(&self, limit: usize) -> Vec<u32> {
        if let Some(core) = self.core.as_ref() {
            core.lib.top_recent_rows(limit)
        } else { (0..self.songs.len().min(limit) as u32).collect() }
    }

    /// Toggles a song's star locally and tells the server when writes are allowed.
    pub fn toggle_song_star(&mut self, song_ix: u32, cx: &mut Context<Self>) {
        if self.is_offline() {
            self.show_toast(tr(STARS_NEED_SERVER), cx);
            return;
        }
        let on = !self.starred.remove(&song_ix);
        if on {
            self.starred.insert(song_ix);
            self.liked_order.retain(|s| *s != song_ix);
            self.liked_order.insert(0, song_ix);
        }
        if let Some(core) = self.core.as_ref() {
            core.lib.send_star(StarTable::Song, String::new(), Some(song_ix), on);
        }
        self.liked_changed();
        cx.notify();
    }

    pub fn core_star_album(&self, id: &gpui::SharedString, on: bool) {
        if let Some(core) = self.core.as_ref() {
            core.lib.send_star(StarTable::Album, id.to_string(), None, on);
        }
    }

    pub fn core_star_artist(&self, id: &gpui::SharedString, on: bool) {
        if let Some(core) = self.core.as_ref() {
            core.lib.send_star(StarTable::Artist, id.to_string(), None, on);
        }
    }

    /// Sets Private mode, which stops reports and hides Discord until turned off.
    pub fn set_private_mode(&mut self, on: bool, cx: &mut Context<Self>) {
        self.private_mode = on;
        self.prefs.private_mode = on;
        self.prefs.save();
        self.apply_reporting();
        // Listening Now and following show only while not private.
        self.apply_sharing(cx);
        self.publish_integrations(true);
        cx.notify();
    }

    /// Plays a list of songs, optionally shuffled, for albums, artists and playlists.
    pub fn core_play_list(&mut self, songs: Vec<u32>, pos: usize, shuffle: bool, cx: &mut Context<Self>) {
        let was = self.player.shuffle;
        if shuffle {
            self.player.shuffle = true;
        }
        self.core_play_from(Arc::new(songs), pos, cx);
        self.player.shuffle = was || shuffle;
    }

    /// Queues songs at `offset` among the songs after the current one. With `queued` they go into
    /// Next in Queue, otherwise into Next from, joining the list. Play Next is the start of Next in
    /// Queue and Add to Queue its end. Returns where they went in `Player::queue`.
    pub fn core_queue_at(&mut self, songs: &[u32], offset: usize, queued: bool, _cx: &mut Context<Self>) -> usize {
        let Some(core) = self.core.as_mut() else { return 0 };
        let empty = core.pq.is_empty();
        let (edits, at) = core.pq.insert(songs.to_vec(), offset, queued);
        // In an empty queue they wait, paused.
        self.queue_edits_at(edits, 0, empty);
        at
    }

    /// A song by row id, for the queue view and the current song.
    pub fn song_by_rowid(&mut self, rowid: u32) -> Song {
        if self.core.is_none() {
            return self.songs.get(rowid as usize).cloned().unwrap_or_default();
        }
        let core = self.core.as_mut().unwrap();
        if let Some(song) = core.rows.get(rowid) {
            return song.clone();
        }
        core.rows.want(rowid);
        Song::default()
    }

    /// Sends the row requests collected during this frame, and the equalizer if it changed.
    pub fn core_flush_requests(&mut self) {
        self.core_sync_equalizer();
        let Some(core) = self.core.as_mut() else { return };
        if !core.rows.wanted.is_empty() {
            let batch = std::mem::take(&mut core.rows.wanted);
            let _ = core.row_tx.send(batch);
        }
    }

    pub fn on_core_event(&mut self, event: CoreEvent, cx: &mut Context<Self>) {
        match event {
            CoreEvent::Player(event) => self.on_player_event(event, cx),
            CoreEvent::Command(command) => self.on_control_command(command, cx),
            CoreEvent::LoginRejected => self.on_login_rejected(cx),
            CoreEvent::Connected(ok) => {
                if let Some(core) = self.core.as_mut() {
                    core.connected = Some(ok);
                }
                // Songs cached so far play offline too, and aren't dimmed.
                if !ok {
                    self.refresh_storage(cx);
                }
                // The monitor saw the server return, so a playing queue needn't wait out its backoff.
                if ok && self.player.waiting_for_server && let Some(core) = self.core.as_ref() {
                    core.player.retry_now();
                }
                // Only a library that never synced is unreachable. One with data just browses.
                if ok && self.library_state == crate::shell::LibraryState::Unreachable {
                    self.library_state = crate::shell::LibraryState::Syncing;
                } else if !ok && self.library_state != crate::shell::LibraryState::Ready {
                    self.library_state = crate::shell::LibraryState::Unreachable;
                }
                cx.notify();
            }
            CoreEvent::SyncFailed => {
                self.library_state = crate::shell::LibraryState::SyncFailed;
                cx.notify();
            }
            CoreEvent::SyncProgress => {
                cx.notify();
                // The first sync's rows appear as they arrive, without a query per page.
                let due = self.core.as_ref().is_some_and(|c| c.sync_notified.is_none_or(|t| t.elapsed() > Duration::from_millis(1500)));
                if due {
                    if let Some(core) = self.core.as_mut() {
                        core.sync_notified = Some(Instant::now());
                    }
                    self.rebuild_order();
                }
            }
            CoreEvent::Refreshed => cx.notify(),
            CoreEvent::SyncDone => {
                if let Some(core) = self.core.as_mut() {
                    core.rows.clear();
                }
                if self.library_state == crate::shell::LibraryState::Syncing {
                    self.library_state = crate::shell::LibraryState::Ready;
                }
                self.rebuild_order();
                self.core_refresh_starred(cx);
                self.core_refresh_downloaded(cx);
                if let Some(library) = self.library.clone() {
                    self.playlists = library.playlists();
                    self.refresh_sidebar_order();
                    self.load_catalog(&library);
                }
                cx.notify();
            }
            CoreEvent::Rows { rows, requested } => {
                let Some(core) = self.core.as_mut() else { return };
                core.rows.frame = self.frame;
                for id in &requested {
                    core.rows.in_flight.remove(id);
                }
                let mut filled_current = false;
                for (rowid, song) in rows {
                    if self.player.song_ix == Some(rowid as usize) && self.player.current.is_none() {
                        self.player.current = Some(song.clone());
                        filled_current = true;
                    }
                    core.rows.insert(rowid, song);
                }
                if filled_current {
                    self.request_tint(cx);
                    self.fetch_track_extras(cx);
                    self.publish_integrations(true);
                }
                cx.notify();
            }
            CoreEvent::FilterIndex(index) => self.set_filter_index(crate::view_filters::FilterPage::Songs, index, cx),
            CoreEvent::ArtistMatches { generation, matches, index } => self.on_artist_matches(generation, matches, index, cx),
            CoreEvent::Order { generation, order, total } => {
                let Some(core) = self.core.as_ref() else { return };
                if generation != core.order_generation {
                    return;
                }
                if let Some(total) = total {
                    self.total = total;
                }
                if self.order.is_empty() && !order.is_empty() {
                    eprintln!("navia: first list at {:.0} ms ({} rows)", crate::testing::since_start_ms(), order.len());
                }
                self.order = order;
                cx.notify();
            }
            CoreEvent::Starred(rowids) => {
                self.starred = rowids.iter().copied().collect();
                self.liked_order = rowids;
                self.liked_changed();
                cx.notify();
            }
            CoreEvent::Restore { queue, position_ms, repeat, source } => {
                if self.player.playing || self.player.song_ix.is_some() {
                    return; // Something else started before the restore finished.
                }
                self.core_load_paused(*queue, position_ms, repeat, source, cx);
            }
            CoreEvent::QueueOffer(offer) => {
                if let Some(core) = self.core.as_mut() {
                    core.queue_offer = Some(offer);
                }
                cx.notify();
            }
            CoreEvent::StarFailed { table, id, rowid, on } => {
                match (table, rowid) {
                    (StarTable::Song, Some(rowid)) => {
                        if on {
                            self.starred.remove(&rowid);
                        } else {
                            self.starred.insert(rowid);
                        }
                        self.liked_changed();
                    }
                    (StarTable::Album, _) => {
                        let id = gpui::SharedString::from(id);
                        if on {
                            self.album_stars.remove(&id);
                        } else {
                            self.album_stars.insert(id);
                        }
                    }
                    (StarTable::Artist, _) => {
                        let id = gpui::SharedString::from(id);
                        if on {
                            self.artist_stars.remove(&id);
                        } else {
                            self.artist_stars.insert(id);
                        }
                    }
                    _ => {}
                }
                cx.notify();
            }
            CoreEvent::PlayRows(rows) => self.core_play_rows(rows, cx),
            CoreEvent::Downloaded(rows) => {
                self.downloaded = rows.into_iter().collect();
                cx.notify();
            }
            CoreEvent::Download(event) => self.on_download_event(event, cx),
            CoreEvent::CollectionChanged => {
                // Which songs are single downloads changes with Keep Songs.
                self.refresh_synced(cx);
                self.refresh_downloads(cx);
            }
            CoreEvent::RadioReady { seed: RadioSeed::After { id, rowid, play }, rows } => self.core_radio_after(id, rowid, play, rows, cx),
            CoreEvent::RadioReady { seed, rows } => {
                self.clear_toast();
                if rows.is_empty() {
                    self.show_toast(tr("The server has no similar songs for that one"), cx);
                    if let RadioSeed::Song { id, .. } = seed {
                        self.no_song_radio.insert(id.into());
                    }
                    self.similar_cache.clear();
                    self.invalidate_page();
                    cx.notify();
                    return;
                }
                let name = match &seed {
                    RadioSeed::Song { rowid, .. } | RadioSeed::After { rowid, .. } => self.song_peek(*rowid).map(|s| s.title.to_string()),
                    RadioSeed::Artist(id) => self.artists.iter().find(|a| a.id.as_ref() == id).map(|a| a.name.to_string()),
                };
                self.play_source = name.map(|name| QueueSource { name, radio: true, link: None });
                let queue = match seed {
                    RadioSeed::Song { rowid, .. } | RadioSeed::After { rowid, .. } => std::iter::once(rowid).chain(rows).collect(),
                    RadioSeed::Artist(_) => rows,
                };
                self.core_play_rows(queue, cx);
            }
            CoreEvent::PlaylistCreated { id, name, with_songs } => self.on_playlist_created(id, name, with_songs, cx),
            CoreEvent::PlaylistSongsChanged { id, name, added, toast, result } => self.on_playlist_songs_changed(id, name, added, toast, result, cx),
            CoreEvent::PlaylistCreateFailed { message } => self.on_playlist_create_failed(message, cx),
            CoreEvent::Shares(result) => self.on_shares(result, cx),
            CoreEvent::ShareCreated(result) => self.on_share_created(result, cx),
            CoreEvent::ShareSaved(result) => self.on_share_saved(result, cx),
            CoreEvent::ShareDeleted(result) => self.on_share_deleted(result, cx),
            CoreEvent::PlaylistImageChanged { id, removed } => self.on_playlist_image_changed(id, removed, cx),
            CoreEvent::PlaylistImageFailed { message } => {
                self.take_cover_upload(None);
                self.show_toast(message, cx)
            }
            CoreEvent::SmartSaved { id, name, created } => self.on_smart_saved(id, name, created, cx),
            CoreEvent::SmartSaveFailed { message } => self.on_smart_save_failed(message, cx),
            CoreEvent::PlaylistOrderSaved { id, result } => self.on_playlist_order_saved(id, result, cx),
            CoreEvent::PlaylistDetailsSaved { id, result } => self.on_playlist_details_saved(id, result, cx),
            CoreEvent::PlaylistDeleted { id, result } => self.on_playlist_deleted(id, result, cx),
            CoreEvent::AirPlayChanged => self.on_airplay_changed(cx),
            CoreEvent::RemoteReady => {
                self.top_song_cache.clear();
                self.similar_cache.clear();
                if let Some(library) = self.library.clone() {
                    self.playlists_full = library.playlist_details();
                }
                self.invalidate_page();
                cx.notify();
            }
            CoreEvent::TrackExtras(extras) => {
                if let Some(core) = self.core.as_mut() {
                    core.extras = Some(extras);
                    self.publish_integrations(true);
                }
            }
            CoreEvent::Autoplay { generation, rows, ready, fetched, wants_fetch } => self.on_autoplay(generation, rows, ready, fetched, wants_fetch, cx),
        }
    }

    /// Plays an internet radio station in place of the queue, or pauses it if it's playing.
    pub fn core_play_or_toggle_station(&mut self, station: crate::model::RadioStation, cx: &mut Context<Self>) {
        if self.core.is_none() {
            self.show_toast(trf("{name}: internet radio needs a real server", &[("name", &station.name)]), cx);
            return;
        }
        if self.player.station.as_ref().is_some_and(|s| s.id == station.id) {
            self.core_toggle(cx);
            return;
        }
        self.drop_along(cx);
        let song = Song { id: format!("station:{}", station.id).into(), title: station.name.clone(), artist: tr("Internet radio").into(), ..Song::default() };
        if let Some(core) = self.core.as_ref() {
            core.player.play_live(QueueItem::new(song.id.to_string(), None), station.url.to_string());
        }
        self.player.station = Some(station);
        self.player.current = Some(song);
        self.player.song_ix = Some(LIVE_ROW);
        self.player.position = 0.0;
        self.player.playing = true;
        self.player.status = PlayerStatus::Buffering;
        self.lyrics.clear();
        self.lyrics_song = None;
        self.publish_integrations(true);
        cx.notify();
    }

    /// Stops the station and restores the waiting queue, paused at its song's start.
    pub fn core_stop_station(&mut self, cx: &mut Context<Self>) {
        if let Some(core) = self.core.as_ref() {
            core.player.stop();
        }
        self.core_leave_station(cx);
    }

    fn core_leave_station(&mut self, cx: &mut Context<Self>) {
        self.player.station = None;
        self.player.position = 0.0;
        self.player.playing = false;
        self.player.status = PlayerStatus::Stopped;
        if self.player.queue.is_empty() {
            self.player.current = None;
            self.player.song_ix = None;
            self.publish_integrations(true);
            cx.notify();
            return;
        }
        let pos = self.player.queue_pos.min(self.player.queue.len() - 1);
        let rowid = self.player.queue[pos];
        self.player.song_ix = Some(rowid as usize);
        self.player.status = PlayerStatus::Paused;
        self.player.current = self.core.as_mut().and_then(|c| c.rows.get(rowid).cloned());
        if self.player.current.is_none() {
            core_want_current(self, rowid);
            self.core_flush_requests();
        }
        if let Some(core) = self.core.as_ref() {
            let edits = core.pq.load_edits();
            self.queue_edits_at(edits, 0, true);
        }
        self.request_tint(cx);
        self.fetch_track_extras(cx);
        self.publish_integrations(true);
        cx.notify();
    }

    fn on_download_event(&mut self, event: DownloadEvent, cx: &mut Context<Self>) {
        let rowid_of = |shell: &Self, id: &str| shell.core.as_ref().and_then(|c| c.lib.rowid_for_song(id));
        match event {
            DownloadEvent::Progress { id, done, total } => {
                if let Some(row) = rowid_of(self, &id) {
                    let fraction = total.filter(|t| *t > 0).map_or(0.0, |t| done as f32 / t as f32);
                    self.download_progress.insert(row, fraction);
                }
                // Progress only moves a ring on screen.
                self.notify_visual(cx);
                return;
            }
            DownloadEvent::Finished { id, .. } => {
                if let Some(row) = rowid_of(self, &id) {
                    self.download_progress.remove(&row);
                    self.downloaded.insert(row);
                }
                self.refresh_downloads(cx);
                // It left the cache, so it no longer belongs on Cached Songs.
                if self.route == crate::shell::Route::CachedSongs {
                    self.refresh_cached(cx);
                    self.refresh_storage(cx);
                }
            }
            DownloadEvent::Failed { id, message } => {
                if let Some(row) = rowid_of(self, &id) {
                    self.download_progress.remove(&row);
                }
                eprintln!("navia: download failed: {message}");
                self.show_toast(tr("A download failed"), cx);
            }
            DownloadEvent::Removed { id } => {
                if let Some(row) = rowid_of(self, &id) {
                    self.downloaded.remove(&row);
                    self.download_progress.remove(&row);
                }
                self.refresh_downloads(cx);
            }
        }
        cx.notify();
    }

    fn on_player_event(&mut self, event: PlayerEvent, cx: &mut Context<Self>) {
        if self.player.station.is_some() {
            match &event {
                // The station stands in for the queue, which waits underneath.
                PlayerEvent::TrackChanged { .. } | PlayerEvent::QueueChanged => return,
                PlayerEvent::Position { ms } => {
                    self.player.position = *ms as f32 / 1000.0;
                    let title = self.core.as_ref().and_then(|c| c.player.stream_title());
                    if let (Some(title), Some(current)) = (title, self.player.current.as_mut())
                        && current.artist != title
                    {
                        current.artist = title.into();
                        self.publish_integrations(true);
                        self.notify_visual(cx);
                        return;
                    }
                    self.notify_position(cx);
                    return;
                }
                PlayerEvent::Error(message) => {
                    let name = self.player.station.as_ref().map(|s| s.name.clone()).unwrap_or_default();
                    eprintln!("navia: station error: {message}");
                    // The decoder's context, like "read packet: ...", isn't for people. The last part is.
                    let reason = message.rsplit(": ").next().unwrap_or(message).to_string();
                    self.show_toast(format!("{name}: {reason}"), cx);
                    self.core_leave_station(cx);
                    return;
                }
                PlayerEvent::QueueEnded => {
                    let name = self.player.station.as_ref().map(|s| s.name.clone()).unwrap_or_default();
                    self.show_toast(trf("{name} stopped playing", &[("name", &name)]), cx);
                    self.core_leave_station(cx);
                    return;
                }
                _ => {}
            }
        }
        // While following, the player holds only the followed song and the queue waits, so queue events
        // don't apply. Their song ending here asks the server at once.
        if self.listen.following.is_some() {
            match &event {
                PlayerEvent::TrackChanged { .. } | PlayerEvent::QueueChanged => return,
                PlayerEvent::QueueEnded => {
                    self.player.playing = false;
                    self.player.status = PlayerStatus::Stopped;
                    self.session_followed_ended(cx);
                    self.check_listening(cx);
                    cx.notify();
                    return;
                }
                _ => {}
            }
        }
        match event {
            PlayerEvent::TrackChanged { index, item, left } => {
                // The model follows, and a queued song that played leaves it. The rest of the list feeds in when
                // it runs low. From here the player counts indexes without the song that left
                // (`Player::ack_left`), before any later edit.
                let Some(core) = self.core.as_mut() else { return };
                let feed = core.pq.track_changed(index, left.map(|l| l.index));
                if let Some(left) = left {
                    let _ = core.edits.send(EditJob::Ack(left.seq));
                }
                self.queue_edits(feed);
                // A restored track starts mid-song, so take the engine's position, not zero.
                self.player.position = self.core.as_ref().map_or(0.0, |c| c.player.quick_state().1 as f32 / 1000.0);
                if let Some(&rowid) = self.player.queue.get(self.player.queue_pos) {
                    self.resume_after_along(self.player.queue_pos, rowid);
                    self.player.song_ix = Some(rowid as usize);
                    let same = self.player.current.as_ref().is_some_and(|s| s.id.as_ref() == item.id);
                    if !same {
                        self.player.current = self.core.as_mut().and_then(|c| c.rows.get(rowid).cloned());
                        if self.player.current.is_none() {
                            core_want_current(self, rowid);
                            self.core_flush_requests();
                        }
                    }
                }
                let heard = self.player.queue.get(self.player.queue_pos).copied();
                self.session_track_changed();
                self.autoplay_update(heard, cx);
                self.request_tint(cx);
                self.fetch_track_extras(cx);
                self.publish_integrations(true);
                self.core_save_session();
                cx.notify();
            }
            PlayerEvent::StatusChanged(status) => {
                self.note_started(status);
                self.player.status = status;
                self.player.playing = matches!(status, PlayerStatus::Playing | PlayerStatus::Buffering);
                self.publish_integrations(true);
                self.core_save_session();
                if status == PlayerStatus::Paused
                    && let Some(core) = self.core.as_ref()
                {
                    core.lib.flush_queue();
                }
                cx.notify();
            }
            PlayerEvent::Position { ms } => {
                let settling = self.core.as_mut().is_some_and(|c| match c.seek_guard {
                    Some((target, at)) if ms.abs_diff(target) > 900 && at.elapsed() < SEEK_SETTLE => true,
                    _ => {
                        c.seek_guard = None;
                        false
                    }
                });
                if self.seek_dragging() || settling {
                    return;
                }
                self.player.position = ms as f32 / 1000.0;
                self.publish_integrations(false);
                self.core_tick_save();
                self.notify_position(cx);
            }
            PlayerEvent::Error(message) => {
                eprintln!("navia: playback: {message}");
                self.show_toast(tr("A song couldn't be played"), cx);
            }
            // The device failed, not the song, so name it when known.
            PlayerEvent::OutputError { device, message } => {
                eprintln!("navia: output: {message}");
                let text = match device {
                    Some(name) => trf("{device} couldn't start", &[("device", &name)]),
                    None => tr("The output device couldn't start").to_string(),
                };
                self.show_toast(text, cx);
            }
            PlayerEvent::Connection { waiting, message } => {
                if let Some(message) = message {
                    eprintln!("navia: waiting for the server: {message}");
                }
                self.player.waiting_for_server = waiting;
                self.publish_integrations(true);
                cx.notify();
            }
            // Next or Previous with Repeat One switched the player to Repeat All.
            PlayerEvent::RepeatChanged(repeat) => {
                self.player.repeat = repeat;
                self.autoplay_update(None, cx);
                self.publish_integrations(true);
                self.core_save_session();
                cx.notify();
            }
            // An AirPlay receiver's own volume, shown on the slider but never sent back.
            PlayerEvent::DeviceVolume(v) => {
                self.player.device_volume = v.map(|v| volume_percent_of(v.level));
                self.publish_integrations(true);
                cx.notify();
            }
            PlayerEvent::OutputChanged { device_rate, .. } => {
                if self.player.device_rate != device_rate {
                    self.player.device_rate = device_rate;
                    if self.quality_pop_open() {
                        cx.notify();
                    }
                }
            }
            PlayerEvent::QueueChanged | PlayerEvent::Heard { .. } | PlayerEvent::PlayEnded { .. } => {}
            PlayerEvent::QueueEnded => {
                self.player.playing = false;
                self.player.status = PlayerStatus::Stopped;
                // ended for a listening session's hand-over, not because the queue ran out
                if self.session_queue_ended(cx) {
                    cx.notify();
                    return;
                }
                // Autoplay may still be finding its songs; it carries on once they're here.
                self.autoplay_update(None, cx);
                self.publish_integrations(true);
                cx.notify();
            }
        }
    }

    pub(crate) fn on_control_command(&mut self, command: PlayerCommand, cx: &mut Context<Self>) {
        // The desktop's MPRIS Raise shows the window, which may be closed to the tray.
        if command == PlayerCommand::Raise {
            self.show_main_window(cx);
            return;
        }
        if self.core.is_none() {
            return;
        }
        // Media keys and desktop controls. Play and pause while following keep following, and the
        // rest count as the listener taking control, like the buttons.
        match command {
            PlayerCommand::Play if self.along_toggle(Some(true), cx) => return,
            PlayerCommand::Pause if self.along_toggle(Some(false), cx) => return,
            PlayerCommand::PlayPause if self.along_toggle(None, cx) => return,
            PlayerCommand::Next if self.along_skip(true, cx) => return,
            PlayerCommand::Previous if self.along_skip(false, cx) => return,
            _ => {}
        }
        if matches!(command, PlayerCommand::Play | PlayerCommand::Pause | PlayerCommand::Stop | PlayerCommand::PlayPause | PlayerCommand::Next | PlayerCommand::Previous) {
            self.take_control_from_along(cx);
            self.note_host_change();
        }
        let Some(core) = self.core.as_ref() else { return };
        let player = core.player.clone();
        match command {
            PlayerCommand::Play => player.play(),
            PlayerCommand::Pause | PlayerCommand::Stop => player.pause(),
            PlayerCommand::PlayPause => player.toggle(),
            PlayerCommand::Next => player.next(),
            PlayerCommand::Previous => player.previous(),
            PlayerCommand::SeekForward(by) => self.core_seek_ms(self.position_ms().saturating_add(by.as_millis() as u64), cx),
            PlayerCommand::SeekBackward(by) => self.core_seek_ms(self.position_ms().saturating_sub(by.as_millis() as u64), cx),
            PlayerCommand::SeekTo(to) => self.core_seek_ms(to.as_millis() as u64, cx),
            PlayerCommand::SetVolume(v) => {
                self.set_volume_percent(v as f32 * 100.0);
                self.prefs.save();
                cx.notify();
            }
            PlayerCommand::SetLoop(mode) => {
                if self.player.station.is_none() {
                    self.player.repeat = match mode {
                        LoopMode::None => Repeat::Off,
                        LoopMode::Playlist => Repeat::All,
                        LoopMode::Track => Repeat::One,
                    };
                    self.core_apply_modes(cx);
                    self.publish_integrations(true);
                    cx.notify();
                }
            }
            PlayerCommand::SetShuffle(on) => {
                if self.player.station.is_none() && self.player.shuffle != on {
                    self.player.shuffle = on;
                    self.core_apply_modes(cx);
                    self.publish_integrations(true);
                    cx.notify();
                }
            }
            PlayerCommand::Raise | PlayerCommand::Quit => {}
        }
    }

    fn position_ms(&self) -> u64 {
        (self.player.position.max(0.0) * 1000.0) as u64
    }

    /// Play or pause, which keeps following if active (`Shell::along_toggle`). Skip, previous and
    /// seek count as taking control and end following first, unless the followed person lets
    /// listeners control playback.
    pub fn core_toggle(&mut self, cx: &mut Context<Self>) {
        if self.along_toggle(None, cx) {
            return;
        }
        self.note_host_change();
        if let Some(core) = self.core.as_ref() {
            core.player.toggle();
        }
    }

    pub fn core_next(&mut self, cx: &mut Context<Self>) {
        if self.player.station.is_some() || self.along_skip(true, cx) {
            return;
        }
        self.note_host_change();
        self.take_control_from_along(cx);
        if let Some(core) = self.core.as_ref() {
            core.player.next();
        }
    }

    pub fn core_previous(&mut self, cx: &mut Context<Self>) {
        if self.player.station.is_some() || self.along_skip(false, cx) {
            return;
        }
        self.note_host_change();
        self.take_control_from_along(cx);
        if let Some(core) = self.core.as_ref() {
            core.player.previous();
        }
    }

    pub fn core_seek_ms(&mut self, ms: u64, cx: &mut Context<Self>) {
        if self.player.station.is_some() || self.along_seek(ms, cx) {
            return;
        }
        self.note_host_change();
        self.take_control_from_along(cx);
        let Some(core) = self.core.as_mut() else { return };
        core.seek_guard = Some((ms, Instant::now()));
        core.player.seek_ms(ms);
        self.player.position = ms as f32 / 1000.0;
        self.publish_integrations(true);
        self.core_save_session();
        cx.notify();
    }

    /// Applies the equalizer settings for the output in use, whenever they change.
    fn core_sync_equalizer(&mut self) {
        let output = self.prefs.output_device.clone().unwrap_or_else(|| crate::eq_model::OUTPUT_DEFAULT.to_string());
        let correction = self.eq_correction(&self.eq.profile_for(&output).headphone);
        let plan = self.eq.plan_for(&output, correction.as_ref());
        let Some(core) = self.core.as_mut() else { return };
        if core.eq_plan.as_ref() == Some(&plan) {
            return;
        }
        let settings = plan.as_ref().map(eq_settings);
        core.player.set_eq(settings.as_ref());
        core.eq_plan = Some(plan);
    }

    /// The volume slider's value from 0 to 100. The AirPlay receiver's own volume while it's the
    /// output, otherwise this computer's.
    pub fn volume_percent(&self) -> f32 {
        self.player.device_volume.unwrap_or(self.prefs.volume)
    }

    /// Sets the volume, the receiver's on AirPlay and this computer's otherwise. Unmutes unless it's
    /// 0. The caller saves the prefs.
    pub fn set_volume_percent(&mut self, percent: f32) {
        let percent = percent.clamp(0.0, 100.0);
        match self.player.device_volume.as_mut() {
            Some(v) => *v = percent,
            None => self.prefs.volume = percent,
        }
        if percent > 0.0 {
            self.prefs.muted = false;
        }
        self.core_apply_volume();
    }

    pub fn core_apply_volume(&mut self) {
        if let Some(core) = self.core.as_ref() {
            core.player.set_volume(volume_gain(self.volume_percent()));
            core.player.set_muted(self.prefs.muted);
        }
        self.publish_integrations(true);
    }

    pub fn core_apply_modes(&mut self, cx: &mut Context<Self>) {
        let Some(core) = self.core.as_mut() else { return };
        // With Repeat on the list never ends, so songs Autoplay handed over go back.
        let mut edits = if self.player.repeat != Repeat::Off { core.pq.drop_other_tail() } else { Vec::new() };
        core.player.set_repeat(self.player.repeat);
        // Shuffle with Fewer Repeats turns off at once, and turns on once the mix is computed off the UI
        // thread.
        edits.extend(core.pq.set_shuffle(self.player.shuffle));
        self.queue_edits(edits);
        self.queue_mix(cx);
        self.autoplay_update(None, cx);
        self.core_save_session();
    }

    pub fn core_play_row(&mut self, order_ix: usize, cx: &mut Context<Self>) {
        self.core_play_from(Arc::new(self.order.clone()), order_ix, cx);
    }

    /// What a list started now plays from. Whatever its starter set, like a radio, else the page's
    /// own list.
    fn take_play_source(&mut self) -> Option<QueueSource> {
        self.play_source.take().or_else(|| self.route_source())
    }

    /// Starts playing `list` at `order_ix`, queueing a window of it around that row.
    fn core_play_from(&mut self, list: Arc<Vec<u32>>, order_ix: usize, cx: &mut Context<Self>) {
        let Some(&rowid) = list.get(order_ix) else { return };
        // Following someone who lets listeners control playback, so the song plays for both.
        if self.along_play_song(rowid, cx) {
            return;
        }
        self.note_host_change();
        // Any played song replaces a station or following for good.
        self.player.station = None;
        self.drop_along(cx);
        // Offline, only songs on this device start, and a toast says why.
        if self.is_offline() && !self.plays_offline_now(rowid) {
            self.show_toast(tr(NEEDS_SERVER), cx);
            return;
        }
        let source = self.take_play_source();
        let shuffled = self.player.shuffle;
        let Some(core) = self.core.as_mut() else { return };
        self.plays += 1;
        core.player.set_source(source.clone());
        self.player.source = source;
        // Songs waiting in Next in Queue stay next and only the list is replaced. When shuffled, the
        // chosen song plays at once and the mix follows.
        let edits = core.pq.start(list.to_vec(), order_ix, shuffled);
        self.player.current = core.rows.get(rowid).cloned();
        self.queue_edits(edits);
        self.queue_mix(cx);
        self.player.song_ix = Some(rowid as usize);
        self.player.position = 0.0;
        self.player.playing = true;
        self.player.status = PlayerStatus::Buffering;
        self.request_tint(cx);
        self.fetch_track_extras(cx);
        self.publish_integrations(true);
        cx.notify();
    }

    /// Requests a radio queue from the server off the UI thread. The first request can take seconds.
    pub fn core_start_radio(&mut self, seed: RadioSeed, cx: &mut Context<Self>) {
        let Some(core) = self.core.as_ref() else { return };
        let (lib, tx) = (core.lib.clone(), core.events.clone());
        // A song from Search is already playing, and the radio fills in quietly behind it.
        if !matches!(seed, RadioSeed::After { .. }) {
            self.show_toast_for(tr("Finding similar songs"), Duration::from_secs(20), cx);
        }
        cx.background_executor()
            .spawn(async move {
                let rows = match &seed {
                    RadioSeed::Artist(id) => lib.artist_similar_rowids(id),
                    RadioSeed::Song { id, .. } | RadioSeed::After { id, .. } => lib.radio_rowids(id),
                };
                let _ = tx.unbounded_send(CoreEvent::RadioReady { seed, rows });
            })
            .detach();
    }

    /// Handles the radio for a song played from Search. It becomes the rest of the list, after any
    /// songs queued meanwhile and before anything Autoplay handed over. A later play or an empty
    /// answer leaves the queue alone.
    fn core_radio_after(&mut self, id: String, _rowid: u32, play: u64, rows: Vec<u32>, cx: &mut Context<Self>) {
        let live = self.radio_pending == Some(play) && self.plays == play;
        if self.radio_pending == Some(play) {
            self.radio_pending = None;
        }
        if live && rows.is_empty() {
            self.no_song_radio.insert(id.into());
        }
        let ended = self.player.status == PlayerStatus::Stopped && self.player.queue_pos + 1 == self.player.queue.len();
        let Some(core) = self.core.as_mut().filter(|_| live && !rows.is_empty() && self.player.station.is_none()) else {
            self.autoplay_update(None, cx);
            return;
        };
        let (mut edits, at) = core.pq.append_to_list(rows);
        if ended && !edits.is_empty() {
            edits.push(Edit::JumpTo(at));
        }
        self.queue_edits(edits);
        self.autoplay_update(None, cx);
        self.core_save_session();
        cx.notify();
    }

    /// Plays a list of songs by row id from the start, for albums, artists and playlists.
    pub fn core_play_rows(&mut self, rows: Vec<u32>, cx: &mut Context<Self>) {
        if !rows.is_empty() {
            self.core_play_from(Arc::new(rows), 0, cx);
        }
    }

    /// The artist page's Play. Top songs, or their albums when the server has none.
    pub fn core_play_artist(&mut self, artist_id: String, name: String, cx: &mut Context<Self>) {
        let Some(core) = self.core.as_ref() else { return };
        let (lib, tx) = (core.lib.clone(), core.events.clone());
        cx.background_executor()
            .spawn(async move {
                let rows = lib.artist_play_rowids(&artist_id, &name);
                let _ = tx.unbounded_send(CoreEvent::PlayRows(rows));
            })
            .detach();
    }

    /// Handles a click on a queue row.
    pub fn core_jump_queue(&mut self, queue_ix: usize, cx: &mut Context<Self>) {
        // While following, the queue waits under the followed song. It comes back first, with that song
        // ahead of the listener's own, then the jump plays from it.
        if self.take_control_from_along(cx) {
            let at = queue_ix + usize::from(queue_ix >= self.player.queue_pos);
            let Some(core) = self.core.as_mut() else { return };
            let edits = core.pq.jump(at);
            self.queue_edits(edits);
            return;
        }
        // During a station the player isn't holding this queue, so it restarts from that song.
        if self.player.station.is_some() {
            self.player.station = None;
            let Some(core) = self.core.as_mut() else { return };
            let edits = core.pq.restart_at(queue_ix);
            self.queue_edits(edits);
            self.player.position = 0.0;
            self.player.playing = true;
            self.player.status = PlayerStatus::Buffering;
            if let Some(&rowid) = self.player.queue.get(self.player.queue_pos) {
                self.player.song_ix = Some(rowid as usize);
                self.player.current = self.core.as_mut().and_then(|c| c.rows.get(rowid).cloned());
            }
            cx.notify();
            return;
        }
        if self.is_offline() && self.player.queue.get(queue_ix).is_some_and(|r| !self.plays_offline_now(*r)) {
            self.show_toast(tr(NEEDS_SERVER), cx);
            return;
        }
        // Jumping to a song further down Next in Queue drops the queued songs above it,
        // and jumping within Next from keeps them next.
        let Some(core) = self.core.as_mut() else { return };
        let edits = core.pq.jump(queue_ix);
        self.queue_edits(edits);
    }

    /// Clear Queue. The listener's queued songs after the current one leave at once and the list
    /// plays on. Undo puts them back at the top of Next in Queue, ahead of anything queued since.
    pub fn clear_queued(&mut self, cx: &mut Context<Self>) {
        let Some(core) = self.core.as_mut() else { return };
        let (edits, removed) = core.pq.clear_queued();
        if removed.is_empty() {
            return;
        }
        self.queue_edits(edits);
        self.selection = Default::default();
        self.core_save_session();
        self.show_toast_action(tr("Queue cleared"), tr("Undo"), move |this, cx| {
            this.queue_songs_at(&removed, 0, true, cx);
        }, cx);
        cx.notify();
    }

    /// Shuffle Queue. Mixes the songs in Next in Queue once, in place, here and in the player with
    /// the same seed. Shuffle's state and the list stay as they are.
    pub fn shuffle_queued(&mut self, cx: &mut Context<Self>) {
        let Some(core) = self.core.as_mut() else { return };
        let edits = core.pq.shuffle_queued(navia_core::queue_order::random_seed());
        if edits.is_empty() {
            return;
        }
        self.queue_edits(edits);
        self.selection = Default::default();
        self.core_save_session();
        cx.notify();
    }

    /// Clears the rest of the list after Next in Queue, here, in its source and in the player, so
    /// Autoplay or the end follows. Undo puts it back as it was.
    pub fn clear_list(&mut self, cx: &mut Context<Self>) {
        let Some(core) = self.core.as_mut() else { return };
        let Some((edits, undo)) = core.pq.clear_list() else { return };
        self.queue_edits(edits);
        let label = self.player.source.as_ref().map(|s| if s.radio { trf("{name} Radio", &[("name", &s.name)]) } else { s.name.clone() });
        let text = match &label {
            Some(name) => trf("Cleared {name}", &[("name", name)]),
            None => tr("Up Next cleared").to_string(),
        };
        self.selection = Default::default();
        self.autoplay_update(None, cx);
        self.core_save_session();
        self.show_toast_action(text, tr("Undo"), move |this, cx| {
            let Some(core) = this.core.as_mut() else { return };
            let edits = core.pq.undo_clear_list(undo.clone());
            this.queue_edits(edits);
            this.autoplay_update(None, cx);
            this.core_save_session();
        }, cx);
        cx.notify();
    }

    /// Removes Up Next songs by position after the playing one. List songs leave the list too, so
    /// turning shuffle off doesn't bring them back.
    pub fn remove_upcoming(&mut self, positions: &[usize], cx: &mut Context<Self>) {
        if self.player.station.is_some() {
            return;
        }
        let Some(core) = self.core.as_mut() else { return };
        let edits = core.pq.remove(positions);
        if edits.is_empty() {
            return;
        }
        self.queue_edits(edits);
        self.selection = Default::default();
        self.autoplay_update(None, cx);
        self.core_save_session();
        cx.notify();
    }

    /// Retry from the sidebar. Asks the player and the connection check to try now.
    pub fn core_retry_connection(&mut self, cx: &mut Context<Self>) {
        let Some(core) = self.core.as_ref() else { return };
        core.player.retry_now();
        core.lib.retry_connection();
        self.show_toast(tr("Trying the server"), cx);
    }

    /// the account menu's Offline switch: on, nothing goes to the server and only what's on this
    /// device plays; off, it reconnects at once the way Retry does. kept per account.
    pub fn set_offline_switch(&mut self, on: bool, cx: &mut Context<Self>) {
        let Some(core) = self.core.as_ref() else { return };
        let key = core.lib.account_key();
        self.offline_switch = on;
        if on {
            self.prefs.offline_accounts.insert(key);
        } else {
            self.prefs.offline_accounts.remove(&key);
        }
        self.prefs.save();
        core.lib.set_offline(on);
        if on {
            self.refresh_storage(cx);
        } else {
            core.player.retry_now();
        }
        self.apply_sharing(cx);
        cx.notify();
    }

    fn fetch_track_extras(&mut self, cx: &mut Context<Self>) {
        let Some(core) = self.core.as_ref() else { return };
        let Some(song) = self.player.current.as_ref().filter(|s| !s.album_id.is_empty()) else { return };
        let (lib, tx, song_id, album_id) = (core.lib.clone(), core.events.clone(), song.id.to_string(), song.album_id.to_string());
        cx.background_executor().spawn(async move {
            let cover = lib.cover_file(&album_id);
            let mbid = lib.album_mbid(&album_id);
            let isrc = lib.song_isrc(&song_id);
            let _ = tx.unbounded_send(CoreEvent::TrackExtras(TrackExtras { song_id, cover, mbid, isrc }));
        })
        .detach();
    }

    pub fn publish_integrations(&mut self, wake: bool) {
        self.apply_discord_address();
        let Some(core) = self.core.as_ref() else { return };
        let track = self.player.current.as_ref().map(|s| {
            let extras = core.extras.as_ref().filter(|e| e.song_id == s.id.as_ref());
            let (cover_url, release_mbid, isrc) = extras.map(|e| (e.cover.clone(), e.mbid.clone(), e.isrc.clone())).unwrap_or_default();
            TrackInfo {
                id: s.id.to_string(),
                title: s.title.to_string(),
                artist: s.artist.to_string(),
                album: s.album.to_string(),
                duration: (s.duration_s > 0).then(|| Duration::from_secs(u64::from(s.duration_s))),
                release_mbid,
                release_group_mbid: None,
                album_id: (!s.album_id.is_empty()).then(|| s.album_id.to_string()),
                isrc,
                cover_url,
            }
        });
        let state = match self.player.status {
            // Nothing plays while waiting for the server, and other apps should see that.
            PlayerStatus::Buffering if self.player.waiting_for_server => PlaybackState::Paused,
            PlayerStatus::Playing | PlayerStatus::Buffering => PlaybackState::Playing,
            PlayerStatus::Paused => PlaybackState::Paused,
            PlayerStatus::Stopped => PlaybackState::Stopped,
        };
        self.tray_sync(track.is_some() && state == PlaybackState::Playing, track.is_some());
        *self.integrations.bridge.snapshot.lock().unwrap() = PlayerSnapshot {
            state: if track.is_none() { PlaybackState::Stopped } else { state },
            track,
            position: Duration::from_secs_f32(self.player.position.max(0.0)),
            volume: f64::from(self.volume_percent() / 100.0),
            // Hides Discord status. `NAVIA_DISCORD=1` is only the outer gate.
            private_mode: self.private_mode,
            loop_mode: match self.player.repeat {
                Repeat::Off => LoopMode::None,
                Repeat::All => LoopMode::Playlist,
                Repeat::One => LoopMode::Track,
            },
            shuffle: self.player.shuffle,
            can_change_modes: self.player.station.is_none(),
        };
        if wake {
            self.integrations.notify();
        }
    }

    /// Runs from the 200 ms tick. Startup and big list rebuilds leave a lot freed but unreturned.
    pub fn core_maintenance(&mut self) {
        let Some(core) = self.core.as_mut() else { return };
        core.maintenance_ticks = core.maintenance_ticks.wrapping_add(1);
        // A few seconds after launch, then every 30 s.
        if core.maintenance_ticks == 25 || core.maintenance_ticks % 150 == 0 {
            crate::memory::return_free_memory(false);
        }
    }

    fn core_tick_save(&mut self) {
        let due = self.core.as_ref().is_some_and(|c| c.last_saved.elapsed() >= SESSION_SAVE_EVERY);
        if due {
            self.core_save_session();
        }
    }

    /// Saves the queue and position so the next launch can restore them, paused.
    pub fn core_save_session(&mut self) {
        // A station is live and can't be restored. The queue beneath it was already saved.
        if self.player.station.is_some() {
            return;
        }
        let Some(core) = self.core.as_mut() else { return };
        let Some(song) = self.player.current.as_ref() else { return };
        core.last_saved = Instant::now();
        // The server's copy, for other devices, has only the listener's own songs, never Autoplay's.
        let len = self.player.queue.len();
        let own = (len - self.player.auto_tail.min(len)).max(self.player.queue_pos + 1).min(len);
        core.lib.note_queue(&self.player.queue[..own], self.player.queue_pos, (self.player.position.max(0.0) * 1000.0) as u64);
        let Some(path) = session_path() else { return };
        let value = json!({
            "queue": self.player.queue,
            "pos": self.player.queue_pos,
            "position_ms": (self.player.position.max(0.0) * 1000.0) as u64,
            "song_id": song.id.as_ref(),
            "repeat": repeat_name(self.player.repeat),
            "shuffle": self.player.shuffle,
            "queued_run": self.player.queued_run,
            "current_queued": self.player.current_queued,
            "source": self.player.source,
        });
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&path, serde_json::to_vec(&value).unwrap_or_default());
        // Write the whole queue only when it changed, since it can be big.
        let Some(core) = self.core.as_mut() else { return };
        if core.saved_queue != Some(core.pq.revision()) {
            core.saved_queue = Some(core.pq.revision());
            // Written off the UI thread. A later save of a newer revision lands after it.
            let (queue, song_id, file) = (core.pq.clone(), song.id.to_string(), path.with_file_name("queue.json"));
            let _ = core.saver.send(Box::new(move || {
                let value = json!({ "song_id": song_id, "queue": queue });
                let _ = std::fs::write(file, serde_json::to_vec(&value).unwrap_or_default());
            }));
        }
    }

    /// Loads a queue paused at its place, from the saved session at launch or another device's queue
    /// when the listener chooses Continue.
    fn core_load_paused(&mut self, queue: PlayQueue<u32>, position_ms: u64, repeat: Repeat, source: Option<QueueSource>, cx: &mut Context<Self>) {
        let Some(core) = self.core.as_mut() else { return };
        let Some(rowid) = queue.current().map(|e| e.song) else { return };
        core.pq = queue;
        core.saved_queue = None;
        let edits = core.pq.load_edits();
        self.player.source = source.clone();
        core.player.set_source(source);
        core.player.set_repeat(repeat);
        self.player.song_ix = Some(rowid as usize);
        self.player.position = position_ms as f32 / 1000.0;
        self.player.playing = false;
        self.player.current = None;
        self.player.status = PlayerStatus::Paused;
        self.player.repeat = repeat;
        core_want_current(self, rowid);
        self.core_flush_requests();
        self.queue_edits_at(edits, position_ms, true);
        // A shuffle that was still being mixed when the queue was saved.
        self.queue_mix(cx);
        cx.notify();
    }

    /// Checks once after launch whether the server holds a newer queue from another client or
    /// device. Never in scripted runs unless `NAVIA_TEST_QUEUE_OFFER=1`.
    fn core_check_queue_offer(&mut self, cx: &mut Context<Self>) {
        let Some(core) = self.core.as_ref() else { return };
        let driven = std::env::var_os("NAVIA_SCRIPT").is_some() || std::env::var_os("NAVIA_SCROLL_BENCH").is_some();
        if driven && env_flag("NAVIA_TEST_QUEUE_OFFER") != Some(true) {
            return;
        }
        let (lib, tx) = (core.lib.clone(), core.events.clone());
        cx.background_executor()
            .spawn(async move {
                if let Some((offer, rows, index)) = lib.remote_queue_offer() {
                    let _ = tx.unbounded_send(CoreEvent::QueueOffer(QueueOffer { device: navia_core::client::client_display_name(&offer.changed_by).into(), rows, index, position_ms: offer.position_ms, offer }));
                }
            })
            .detach();
    }

    /// Replaces this queue with the other device's, paused at its place.
    pub fn accept_queue_offer(&mut self, cx: &mut Context<Self>) {
        let Some(offer) = self.core.as_mut().and_then(|c| c.queue_offer.take()) else { return };
        if let Some(core) = self.core.as_ref() {
            core.lib.answer_queue_offer(&offer.offer);
            core.player.stop();
        }
        self.player.station = None;
        let queue = PlayQueue::from_queue(offer.rows, offer.index, 0..0, false);
        self.core_load_paused(queue, offer.position_ms, self.player.repeat, None, cx);
        self.core_save_session();
    }

    /// Keeps the local queue, and the offer isn't made again.
    pub fn dismiss_queue_offer(&mut self, cx: &mut Context<Self>) {
        if let Some(core) = self.core.as_mut()
            && let Some(offer) = core.queue_offer.take()
        {
            core.lib.answer_queue_offer(&offer.offer);
        }
        cx.notify();
    }

    fn core_restore_session(&mut self, cx: &mut Context<Self>) {
        let Some(core) = self.core.as_ref() else { return };
        let (lib, tx) = (core.lib.clone(), core.events.clone());
        let Some(path) = session_path() else { return };
        adopt_legacy_session(&path);
        cx.background_executor().spawn(async move {
            let Ok(text) = std::fs::read(&path) else { return };
            let Ok(value) = serde_json::from_slice::<Value>(&text) else { return };
            let queue: Vec<u32> = value["queue"].as_array().map(|a| a.iter().filter_map(|v| v.as_u64().map(|n| n as u32)).collect()).unwrap_or_default();
            let pos = value["pos"].as_u64().unwrap_or(0) as usize;
            if queue.is_empty() || pos >= queue.len() {
                return;
            }
            // Row ids are local to this database, so the saved song id decides whether it still applies.
            let rowid = queue[pos];
            if lib.rows(&[rowid]).first().map(|(_, s)| s.id.to_string()).as_deref() != value["song_id"].as_str() {
                return;
            }
            let position_ms = value["position_ms"].as_u64().unwrap_or(0);
            let repeat = repeat_from(&value["repeat"]);
            let shuffle = value["shuffle"].as_bool().unwrap_or(false);
            let queued_run = value["queued_run"].as_u64().unwrap_or(0) as usize;
            let current_queued = value["current_queued"].as_bool().unwrap_or(false);
            let source = serde_json::from_value::<QueueSource>(value["source"].clone()).ok();
            // The whole queue saved beside it for the same song. Older session files only have the queue
            // as it stood, which becomes the list.
            let saved = std::fs::read(path.with_file_name("queue.json"))
                .ok()
                .and_then(|t| serde_json::from_slice::<Value>(&t).ok())
                .filter(|v| v["song_id"] == value["song_id"])
                .and_then(|v| serde_json::from_value::<PlayQueue<u32>>(v["queue"].clone()).ok())
                .filter(|q| q.current().map(|e| e.song) == Some(rowid));
            let queue = saved.unwrap_or_else(|| {
                let first = if current_queued { pos } else { pos + 1 };
                PlayQueue::from_queue(queue, pos, first..pos + 1 + queued_run, shuffle)
            });
            let _ = tx.unbounded_send(CoreEvent::Restore { queue: Box::new(queue), position_ms, repeat, source });
        })
        .detach();
    }

    /// Test driver steps, like `core=pause`, `core=resume`, `core=next`, `core=prev`,
    /// `core=seek+90.5`, `core=volume+40`, `core=mute` and `core=report`.
    pub fn core_script(&mut self, op: &str, cx: &mut Context<Self>) {
        let (name, value) = op.split_once('+').unwrap_or((op, ""));
        match name {
            "pause" => {
                if let Some(c) = self.core.as_ref() {
                    c.player.pause();
                }
            }
            "resume" => {
                if let Some(c) = self.core.as_ref() {
                    c.player.play();
                }
            }
            "next" => self.core_next(cx),
            "prev" => self.core_previous(cx),
            "seek" => {
                if let Ok(s) = value.parse::<f64>() {
                    self.core_seek_ms((s * 1000.0) as u64, cx);
                }
            }
            "volume" => {
                if let Ok(v) = value.parse::<f32>() {
                    self.set_volume_percent(v);
                }
            }
            "mute" => {
                self.prefs.muted = !self.prefs.muted;
                self.core_apply_volume();
            }
            "repeat" => {
                self.player.repeat = match value {
                    "off" => Repeat::Off,
                    "all" => Repeat::All,
                    "one" => Repeat::One,
                    _ => match self.player.repeat {
                        Repeat::Off => Repeat::All,
                        Repeat::All => Repeat::One,
                        Repeat::One => Repeat::Off,
                    },
                };
                self.core_apply_modes(cx);
            }
            "shuffle" => {
                let start = Instant::now();
                self.player.shuffle = !self.player.shuffle;
                self.core_apply_modes(cx);
                let took = start.elapsed().as_secs_f64() * 1000.0;
                // How recent the next 200 songs are compared with the whole list. Fewer Repeats pushes recent ones later.
                if let Some(core) = self.core.as_ref() {
                    let heard = self.autoplay.lock().unwrap().heard_ids().clone();
                    let mean = |rows: &[u32]| { let r = core.lib.recency(rows, &heard); r.iter().sum::<f32>() / r.len().max(1) as f32 };
                    let pos = self.player.queue_pos;
                    let first = &self.player.queue[(pos + 1).min(self.player.queue.len())..(pos + 201).min(self.player.queue.len())];
                    let all = core.pq.list_songs();
                    eprintln!("navia: shuffle={} style={:?} took {took:.1} ms list={} recency first200={:.4} list={:.4}", self.player.shuffle, self.prefs.shuffle_style, all.len(), mean(first), mean(&all));
                }
            }
            "shuffle_style" => {
                self.prefs.shuffle_style = if value == "standard" { navia_core::queue_order::ShuffleStyle::Standard } else { navia_core::queue_order::ShuffleStyle::FewerRepeats };
            }
            "memory" => {
                crate::testing::print_memory();
                crate::testing::print_memory_breakdown();
                if let Some(path) = std::env::var_os("NAVIA_SMAPS_DUMP") {
                    let _ = std::fs::copy("/proc/self/smaps", path);
                }
                eprintln!("navia: thumbs entries={} bytes={} decoded={}", self.thumbs.len(), self.thumbs.bytes(), self.thumbs.iter().filter(|(_, e)| e.image.is_some()).count());
            }
            "private" => {
                let on = !self.private_mode;
                self.set_private_mode(on, cx);
            }
            "offline" => {
                let on = !self.offline_switch;
                self.set_offline_switch(on, cx);
            }
            // Song radio from the song playing now, for tests that allow one external lookup.
            "song_radio" => {
                if let Some(song_ix) = self.player.song_ix.filter(|&i| i != LIVE_ROW) {
                    self.start_song_radio(song_ix as u32, cx);
                }
            }
            // Downloads the current song, or with `+ID` any song by server id.
            "download" => {
                let row = if value.is_empty() { self.player.song_ix.map(|i| i as u32) } else { self.core.as_ref().and_then(|c| c.lib.rowid_for_song(value)) };
                if let Some(row) = row {
                    self.core_set_downloaded(&[row], true, cx);
                }
            }
            "song_radio_id" => {
                if let Some(rowid) = self.core.as_ref().and_then(|c| c.lib.rowid_for_song(value)) {
                    self.core_start_radio(RadioSeed::Song { id: value.to_string(), rowid }, cx);
                }
            }
            // `core=station` plays the first internet radio station and `core=station_stop` stops it.
            "station" => {
                if let Some(station) = self.library.as_ref().and_then(|l| l.radio_stations().first().cloned()) {
                    self.core_play_or_toggle_station(station, cx);
                }
            }
            "station_stop" => self.core_stop_station(cx),
            // How the current song is delivered, and the song cache's state.
            "storage" => {
                let delivered = self.core.as_ref().and_then(|c| c.player.delivered());
                let cache = self.core.as_ref().and_then(|c| c.lib.downloads.song_cache());
                let (bytes, songs) = cache.as_ref().map_or((0, 0), |c| c.usage());
                eprintln!("navia: storage delivered={delivered:?} cached_songs={songs} cached_bytes={bytes} limit={:?} low_disk={:?} cached_rows={}", cache.as_ref().map(|c| c.limit()), cache.as_ref().map(|c| c.low_disk()), self.storage.cached_rows.len());
                self.refresh_storage(cx);
                // `core=storage+confirm` opens the Clear Cache dialog, and `+clear` clears at once.
                match value {
                    "confirm" => self.confirm = Some(crate::view_connect::Confirm::ClearCache),
                    "clear" => self.clear_caches(cx),
                    _ => {}
                }
            }
            // Autoplay's state for checks, with the section, the queue's tail and the queued run.
            // `core=offer+accept` or `core=offer+dismiss` answers the queue offer and prints the queue.
            "offer" => {
                let offered = self.core.as_ref().and_then(|c| c.queue_offer.as_ref()).map(|o| (o.device.to_string(), o.rows.len(), o.index, o.position_ms));
                if value == "accept" { self.accept_queue_offer(cx) } else { self.dismiss_queue_offer(cx) }
                eprintln!("navia: offer {value} {offered:?} -> queue {} at {} position {:.1} s", self.player.queue.len(), self.player.queue_pos, self.player.position);
            }
            // `core=autoplay_mode+navia|server` switches the Autoplay mode, as Settings does.
            "autoplay_mode" => {
                if let Some(mode) = navia_core::engine::AutoplayMode::parse(value) {
                    self.set_autoplay_mode(mode, cx);
                }
            }
            "autoplay" => {
                let section: Vec<String> = self.autoplay_section().iter().map(|r| self.song_by_rowid(*r).title.to_string()).collect();
                let rest: Vec<u32> = self.player.queue[(self.player.queue_pos + 1).min(self.player.queue.len())..].to_vec();
                let up_next: Vec<String> = rest.iter().map(|r| self.song_by_rowid(*r).title.to_string()).collect();
                let engine = self.core.as_ref().map_or(0, |c| c.player.snapshot().queue.len());
                let based_on = self.autoplay_based_on();
                let context = self.autoplay.lock().unwrap().context_id();
                eprintln!(
                    "navia: autoplay state={:?} ready={} context={} based_on={:?} rows={} on={} seeds={:?} queued_run={} auto_tail={} at={} queue={} engine_queue={} up_next={:?} section={:?}",
                    self.autoplay_state(),
                    self.autoplay_ready,
                    context,
                    based_on,
                    self.autoplay_section().len(),
                    self.prefs.autoplay,
                    self.autoplay.lock().unwrap().seeds(),
                    self.player.queued_run,
                    self.player.auto_tail,
                    self.player.queue_pos,
                    self.player.queue.len(),
                    engine,
                    up_next,
                    section
                );
            }
            // Queue rules. `queue_add+ROW` adds a Songs list row to the queue, `jump+I` double-clicks Up
            // Next offset I, and `queue` prints the queue beside the engine's, with [q] marking queued songs.
            // `auto_pick+K` double-clicks an Autoplay row and `auto_remove+K` removes it.
            "auto_pick" => {
                if let Ok(k) = value.parse::<usize>() {
                    let up_next = self.up_next_len();
                    self.jump_in_queue(up_next + k, cx);
                }
            }
            "auto_remove" => {
                if let Ok(k) = value.parse::<usize>() {
                    self.autoplay_remove(&[k], cx);
                }
            }
            "queue_add" => {
                if let Some(row) = value.parse::<usize>().ok().and_then(|i| self.order.get(i).copied()) {
                    self.add_to_queue(&[row], cx);
                }
            }
            "jump" => {
                if let Ok(offset) = value.parse::<usize>() {
                    self.core_jump_queue(self.player.queue_pos + 1 + offset, cx);
                }
            }
            // Queue heading buttons `clear_queued`, `shuffle_queued` and `clear_list`, and `undo` for the
            // toast's action, which prints the toast first. Listen-along uses `share+on|off`,
            // `listen_along+USER`, `listen_stop`, and `listen` to print the state.
            "share" => {
                self.prefs.share_listening = value == "on";
                self.apply_sharing(cx);
            }
            // The play/pause button, as a click on it.
            "toggle" => self.core_toggle(cx),
            // "Let listeners control playback", with `control+on|off`.
            "control" => {
                self.prefs.listeners_control = value == "on";
                self.apply_listen_marks();
            }
            "listen_along" => {
                if let Some(e) = self.listen.listing.iter().find(|e| e.user == value).cloned() {
                    self.listen_along(e, cx);
                }
            }
            "listen_stop" => {
                self.stop_listening(false, cx);
            }
            // Spotify friends. `lanyard+ADDRESS`, `friend+ID` or `friend+ID:NAME`, `unfriend+ID`,
            // `friend_confirm+ID`, `listen_friend+ID`, and `friends` to print them.
            "lanyard" => {
                self.prefs.lanyard_address = value.to_string();
                self.apply_friends(cx);
            }
            "friend" => {
                let (id, name) = value.split_once(':').unwrap_or((value, ""));
                self.listen.draft_id = id.to_string();
                self.listen.draft_name = name.to_string();
                self.add_friend(cx);
            }
            "unfriend" => self.remove_friend(value, cx),
            "listen_friend" => self.listen_along_friend(value, cx),
            "friend_confirm" => self.confirm = Some(crate::view_connect::Confirm::ListenAlongFriend { id: value.to_string() }),
            "friends" => {
                let rows: Vec<String> = self
                    .listen
                    .friends
                    .iter()
                    .map(|f| format!("{}:{:?}:{}:{}", f.display_name(), f.reach, f.track.as_ref().map_or("-".to_string(), |t| format!("{} by {}", t.song, t.artist)), f.song.as_ref().map_or("not in library".to_string(), |s| format!("song={}", s.id))))
                    .collect();
                let transport = self.core.as_ref().map(|c| c.lib.friends.transport());
                eprintln!("navia: friends transport={transport:?} listed={} rows={rows:?} toast={:?}", self.listening_friends().len(), self.toast.as_ref().map(|t| t.text.clone()));
            }
            "listen" => {
                let listed: Vec<String> = self.listen.listing.iter().map(|e| format!("{}:{}:{:?}", e.user, e.song.title, e.state)).collect();
                let following = self.listen.following.as_ref().map(|f| format!("{} song={}", f.followed.user, f.song.song.title));
                let current = self.player.current.as_ref().map(|s| s.title.to_string());
                eprintln!(
                    "navia: listen active={} listed={listed:?} following={following:?} held={:?} listeners={:?} ended={:?} current={current:?} pos={:.1} playing={} at={} queue={}",
                    self.listen_active(),
                    self.listen.following.as_ref().map(|f| f.held),
                    self.listen.listeners.iter().map(|l| format!("{}:{}:{}", l.user, l.song_id, l.playing)).collect::<Vec<_>>(),
                    self.listen.ended,
                    self.player.position,
                    self.player.playing,
                    self.player.queue_pos,
                    self.player.queue.len()
                );
            }
            "clear_queued" => self.clear_queued(cx),
            "shuffle_queued" => self.shuffle_queued(cx),
            "clear_list" => self.clear_list(cx),
            // Checks the queue model against the player for the same songs, queued flags and place.
            "verify" => {
                let Some(core) = self.core.as_ref() else { return };
                let snap = core.player.snapshot();
                let rows = core.pq.songs();
                let ids: std::collections::HashMap<u32, String> = core.lib.queue_ids(&rows).into_iter().map(|(r, id, _)| (r, id)).collect();
                let mine: Vec<(String, bool)> = core.pq.entries().iter().map(|e| (ids.get(&e.song).cloned().unwrap_or_default(), e.kind == navia_core::play_queue::Kind::Queued)).collect();
                let engine: Vec<(String, bool)> = snap.queue.iter().map(|i| (i.id.clone(), i.queued)).collect();
                let ok = mine == engine && (snap.index == Some(core.pq.pos()) || (mine.is_empty() && snap.index.is_none()));
                // The whole model, so a restart can be checked to restore the same queue.
                let digest = {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    serde_json::to_string(&core.pq).unwrap_or_default().hash(&mut h);
                    h.finish()
                };
                eprintln!(
                    "navia: verify {} digest={digest:016x} entries={} engine={} pos={} index={:?} shuffle={} mixing={} list={} left={} unfed={} run={} tail={}",
                    if ok { "ok" } else { "MISMATCH" },
                    mine.len(),
                    engine.len(),
                    core.pq.pos(),
                    snap.index,
                    core.pq.shuffle(),
                    core.pq.mixing(),
                    core.pq.list_songs().len(),
                    core.pq.list_left(),
                    core.pq.unfed(),
                    core.pq.queued_run(),
                    core.pq.other_tail()
                );
            }
            "undo" => {
                let toast = self.toast.clone();
                eprintln!("navia: toast {:?} action={:?}", toast.as_ref().map(|t| t.text.clone()), toast.as_ref().and_then(|t| t.action.as_ref().map(|a| a.label.clone())));
                if let Some(action) = toast.and_then(|t| t.action) {
                    self.clear_toast();
                    (action.run)(self, cx);
                }
            }
            "queue" => {
                let pos = self.player.queue_pos;
                let (from, to) = (pos.saturating_sub(3), (pos + 7).min(self.player.queue.len()));
                let rows: Vec<u32> = self.player.queue[from..to].to_vec();
                let mine: Vec<String> = rows
                    .iter()
                    .enumerate()
                    .map(|(k, r)| {
                        let i = from + k;
                        let current = if i == pos { ">" } else { "" };
                        let queued = if (i > pos && i <= pos + self.player.queued_run) || (i == pos && self.player.current_queued) { "[q]" } else { "" };
                        format!("{current}{queued}{}", self.song_by_rowid(*r).title)
                    })
                    .collect();
                let snap = self.core.as_ref().map(|c| c.player.snapshot());
                let engine: Vec<String> = snap.as_ref().map_or_else(Vec::new, |s| {
                    let at = s.index.unwrap_or(0);
                    let lib = self.core.as_ref().map(|c| c.lib.clone());
                    s.queue
                        .iter()
                        .enumerate()
                        .skip(at.saturating_sub(3))
                        .take(10)
                        .map(|(i, item)| {
                            let title = lib.as_ref().and_then(|l| l.rowid_for_song(&item.id)).and_then(|r| self.song_peek(r)).map_or_else(|| item.id.clone(), |s| s.title.to_string());
                            format!("{}{}{title}", if i == at { ">" } else { "" }, if item.queued { "[q]" } else { "" })
                        })
                        .collect()
                });
                eprintln!(
                    "navia: queue at={} len={} run={} source={:?} engine_at={:?} engine_len={} mine={:?} engine={:?}",
                    pos,
                    self.player.queue.len(),
                    self.player.queued_run,
                    self.player.source.as_ref().map(|s| s.name.clone()),
                    snap.as_ref().and_then(|s| s.index),
                    snap.as_ref().map_or(0, |s| s.queue.len()),
                    mine,
                    engine
                );
            }
            "report" => {
                let snap = self.core.as_ref().map(|c| c.player.snapshot());
                let current = self.player.current.as_ref().map(|s| s.title.to_string());
                eprintln!("navia: downloads done={} in_progress={}", self.downloaded.len(), self.download_progress.len());
                if self.player.station.is_some() {
                    eprintln!("navia: station={:?} live={:?} stream_title={:?} artist_line={:?}", self.player.station.as_ref().map(|s| s.name.to_string()), snap.as_ref().map(|s| s.live), self.core.as_ref().and_then(|c| c.player.stream_title()), self.player.current.as_ref().map(|s| s.artist.to_string()));
                }
                eprintln!(
                    "navia: player offline={} waiting={} private={} engine_queue={} status={:?} pos={:.2}s title={:?} queue={} at={} repeat={:?} shuffle={} volume={:.0} muted={} rate={} device={:?} underrun={} rebuffer={} xruns={}",
                    self.is_offline(),
                    self.player.waiting_for_server,
                    self.private_mode,
                    snap.as_ref().map_or(0, |s| s.queue.len()),
                    snap.as_ref().map(|s| s.status),
                    self.player.position,
                    current,
                    self.player.queue.len(),
                    self.player.queue_pos,
                    self.player.repeat,
                    self.player.shuffle,
                    self.prefs.volume,
                    self.prefs.muted,
                    snap.as_ref().map_or(0, |s| s.output_rate),
                    snap.as_ref().and_then(|s| s.device.clone()),
                    snap.as_ref().map_or(0, |s| s.underrun_frames),
                    snap.as_ref().map_or(0, |s| s.rebuffer_frames),
                    snap.as_ref().map_or(0, |s| s.xruns),
                );
            }
            _ => {}
        }
        cx.notify();
    }
}

fn eq_settings(plan: &crate::eq_model::EqPlan) -> EqSettings {
    use crate::eq_model::FilterKind;
    let bands = plan
        .filters
        .iter()
        .map(|f| EqBand {
            shape: match f.kind {
                FilterKind::Peaking => FilterShape::Peaking,
                FilterKind::LowShelf => FilterShape::LowShelf,
                FilterKind::HighShelf => FilterShape::HighShelf,
            },
            freq_hz: f.freq,
            gain_db: f.gain_db,
            q: f.q,
        })
        .collect();
    EqSettings { preamp_db: plan.preamp_db, bands, limiter: plan.limiter, balance: plan.balance, mono: plan.mono, crossfeed: plan.crossfeed }
}

fn core_want_current(shell: &mut Shell, rowid: u32) {
    if let Some(core) = shell.core.as_mut() {
        core.rows.want(rowid);
    }
}

/// How the session file writes repeat, as "off", "all" or "one". Older files hold a bool where true means all.
fn repeat_name(r: Repeat) -> &'static str {
    match r {
        Repeat::Off => "off",
        Repeat::All => "all",
        Repeat::One => "one",
    }
}

fn repeat_from(value: &Value) -> Repeat {
    match (value.as_str(), value.as_bool()) {
        (Some("all"), _) | (_, Some(true)) => Repeat::All,
        (Some("one"), _) => Repeat::One,
        _ => Repeat::Off,
    }
}
