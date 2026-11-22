//! The library the shell shows, backed by navia-core's local SQLite database and the server.
//! Rows load by row id on demand, so memory doesn't grow with the library. The only server
//! writes are ones the user asks for, namely stars and scrobbles when enabled and not in Private
//! mode. `NAVIA_READ_ONLY=1` makes a run read-only.

use crate::i18n::{tr, trf};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::ops::Deref;
use std::time::{Duration, Instant};

use futures::channel::mpsc::UnboundedSender;
use gpui::SharedString;
use navia_core::artwork::{ArtworkCache, ArtworkConfig};
use navia_core::lyrics::LyricsCache;
use navia_core::client::native::{PlaylistImageError, PlaylistImageSupport, SmartSaveError};
use navia_core::capabilities::ServerFeatures;
use navia_core::client::{Client, ClientConfig, Credentials, Song as CoreSong, StarTarget, WritePolicy};
use navia_core::library::discography;
use navia_core::library::sync::{BackgroundRefresh, SyncOptions, Syncer};
use navia_core::library::taste::{self, Because, Taste};
use navia_core::downloads::DownloadManager;
use navia_core::library::{AlbumSort, Library as CoreDb, LibraryConfig, SongOrder, StarTable};
use navia_core::queue_sync::QueueSync;
use navia_core::similar_artists;
use navia_core::reporting::PlayReporter;
use tokio::runtime::Runtime;

use crate::core_player::CoreEvent;
use crate::model::{Account, Album, Upcoming, UpcomingTrack, UpcomingView, AlbumTrack, Artist, ArtistInfo, Discography, ForYouPage, ReleaseSection, Genre, GenrePage, HomeTaste, Library, MixInfo, LyricLine, MoreBy, PlaylistCover, PlaylistDetails, PlaylistSummary, RadioStation, Song};
use crate::shell::SortKey;

pub enum Startup {
    Library(Arc<dyn Library>, Option<Arc<CoreLibrary>>),
    SignIn(crate::account::SignInPrompt),
}

/// Opens the real library, sample data with `NAVIA_SAMPLE=1`, or the sign-in screen when no one
/// is signed in. Doesn't touch the network.
pub fn open_library() -> Startup {
    if std::env::var_os("NAVIA_SAMPLE").is_some() {
        return Startup::Library(crate::sample::open(), None);
    }
    let connection = match crate::account::resolve() {
        Ok(connection) => connection,
        Err(prompt) => return Startup::SignIn(prompt),
    };
    match CoreLibrary::open(connection) {
        Ok(lib) => Startup::Library(lib.clone(), Some(lib)),
        Err(e) => {
            eprintln!("navia: {e}");
            Startup::SignIn(crate::account::SignInPrompt { account: None, note: Some((tr("Couldn't open the library on this device"), e.to_string())) })
        }
    }
}

/// A tokio runtime that can be dropped from any thread, including one of its own.
struct OwnedRuntime(Option<Runtime>);

impl Deref for OwnedRuntime {
    type Target = Runtime;
    fn deref(&self) -> &Runtime {
        self.0.as_ref().expect("runtime")
    }
}

impl Drop for OwnedRuntime {
    fn drop(&mut self) {
        if let Some(rt) = self.0.take() {
            std::thread::spawn(move || rt.shutdown_background());
        }
    }
}

/// How often the library, playlists and play counts are refreshed in the background.
const REFRESH_EVERY: Duration = Duration::from_secs(20 * 60);

pub struct Connection {
    pub url: String,
    pub user: String,
    pub password: String,
    /// Whether it came from the `NAVIDROME_*` developer override under `NAVIA_DEV=1`.
    pub from_env: bool,
}

/// Whether this run must never write to the server, whatever the settings say. Scripted and
/// benchmark runs count, unless `NAVIA_REPORT_PLAYS=1` asks for reporting on purpose.
pub fn read_only_run() -> bool {
    let driven = std::env::var_os("NAVIA_SCRIPT").is_some() || std::env::var_os("NAVIA_SCROLL_BENCH").is_some();
    let forced_on = std::env::var_os("NAVIA_REPORT_PLAYS").is_some_and(|v| v == "1");
    std::env::var_os("NAVIA_READ_ONLY").is_some_and(|v| v == "1") || (driven && !forced_on)
}

/// Sync options that skip server-side work in read-only runs. Fetching play history and library
/// totals logs in, and fetching downloaded smart playlists re-evaluates them on the server. The
/// `NAVIA_TEST_*` switches enable these against a local stand-in server only.
fn history_sync_options(watch: &Arc<navia_core::library::sync::SyncWatch>) -> SyncOptions {
    let test_override = std::env::var_os("NAVIA_TEST_PLAY_HISTORY").is_some_and(|v| v == "1");
    let totals_override = std::env::var_os("NAVIA_TEST_SERVER_TOTALS").is_some_and(|v| v == "1");
    let native = !read_only_run() || totals_override;
    SyncOptions { play_history: !read_only_run() || test_override, synced_smart: !read_only_run(), watch: Some(watch.clone()), server_totals: native, song_updates: native, ..Default::default() }
}

impl Connection {
    /// The login from `NAVIDROME_URL`, `NAVIDROME_USER` and `NAVIDROME_PASSWORD`, only when
    /// `NAVIA_DEV=1` enables the developer override.
    pub fn from_env() -> Option<Self> {
        let (url, user, password) = navia_core::connect::developer_login()?;
        Some(Self { url, user, password, from_env: true })
    }

    pub fn saved(url: String, user: String, password: String) -> Self {
        Self { url, user, password, from_env: false }
    }
}

fn data_dir(connection: &Connection, credentials: &Credentials) -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("NAVIA_DATA_DIR") {
        return Some(PathBuf::from(dir).join(credentials.account_key(&connection.url)));
    }
    let key = credentials.account_key(&connection.url);
    // On Windows use %LOCALAPPDATA%\NaviaData, beside the install rather than in it, because the
    // installer replaces and the uninstaller removes all of %LOCALAPPDATA%\Navia.
    #[cfg(target_os = "windows")]
    return Some(directories::BaseDirs::new()?.data_local_dir().join("NaviaData").join(key));
    // Local, not roaming. ~/.local/share/navia on Linux, Application Support on the Mac.
    #[cfg(not(target_os = "windows"))]
    return Some(directories::ProjectDirs::from("", "", "Navia")?.data_local_dir().join(key));
}

pub struct CoreLibrary {
    /// Spotify friends through Lanyard, on this runtime.
    pub friends: navia_core::lanyard::watch::FriendsWatch,
    rt: OwnedRuntime,
    pub client: Client,
    pub db: CoreDb,
    pub artwork: ArtworkCache,
    /// Lyrics cached on disk, for replays and offline use.
    pub lyrics: LyricsCache,
    pub reporter: Arc<PlayReporter>,
    /// False for a read-only run, which sends no stars or plays whatever the settings say.
    pub writes_allowed: bool,
    pub from_env: bool,
    /// Whether to refresh the library periodically.
    background_sync: AtomicBool,
    login_rejected: AtomicBool,
    tasks: Mutex<Vec<tokio::task::AbortHandle>>,
    pub downloads: Arc<DownloadManager>,
    events: Arc<Mutex<Option<UnboundedSender<CoreEvent>>>>,
    background: Mutex<Option<BackgroundRefresh>>,
    catalog: Mutex<Option<Arc<Catalog>>>,
    genres: Mutex<Arc<Vec<Genre>>>,
    stations: Mutex<Arc<Vec<RadioStation>>>,
    can_share: AtomicBool,
    /// Whether the server lets this user download, so share links can allow downloads.
    share_downloads: AtomicBool,
    /// What the server allows for playlist images. `None` until connected.
    image_support: Mutex<Option<PlaylistImageSupport>>,
    /// Server answers such as top songs, similar songs and artist info, by artist id.
    remote: Arc<Mutex<Remote>>,
    /// Playlists whose songs were fetched this run, so each is fetched only once.
    opened_playlists: Mutex<HashSet<String>>,
    /// Admin status from `getUser`. `None` until it answers.
    admin: Mutex<Option<bool>>,
    /// once `getOpenSubsonicExtensions` answers.
    features: OnceLock<ServerFeatures>,
    /// The play queue saved on the server.
    pub queue_sync: QueueSync,
    first_sync_done: AtomicBool,
    retry: AtomicBool,
    /// a page's refresh is under way.
    refreshing: AtomicBool,
    /// each change sends `CoreEvent::SyncProgress`.
    pub sync_watch: Arc<navia_core::library::sync::SyncWatch>,
    /// art keys whose last answer was the server's no-cover placeholder.
    placeholder_covers: Mutex<HashSet<String>>,
    /// covers whose failure was already logged, so a broken one is reported once per run.
    failed_covers: Mutex<HashSet<String>>,
}

impl CoreLibrary {
    /// Opens the local database and returns at once. There's no network here, so a missing or slow
    /// server can't delay the first paint. Start the network side with [`Self::start_background`].
    pub fn open(connection: Connection) -> anyhow::Result<Arc<Self>> {
        let writes_allowed = !read_only_run();
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).thread_name("navia-core").enable_time().enable_io().build()?;
        let credentials = Credentials::new(connection.user.clone(), connection.password.clone());
        let mut config = ClientConfig::new(connection.url.clone(), credentials.clone());
        // The client refuses writes in a read-only run too, not just the callers.
        config.write_policy = if writes_allowed { WritePolicy::Allowed } else { WritePolicy::ReadOnly };
        let client = Client::new(config)?;
        let base = data_dir(&connection, &credentials).ok_or_else(|| anyhow::anyhow!(tr("no per-user data directory on this system")))?;
        let db = CoreDb::open(&LibraryConfig { path: base.join("library.sqlite") })?;
        let artwork = {
            let _guard = rt.enter();
            ArtworkCache::new(client.clone(), ArtworkConfig { dir: base.join("artwork"), max_bytes: navia_core::artwork::DEFAULT_MAX_BYTES })?
        };
        let lyrics = LyricsCache::new(base.join("lyrics"), navia_core::lyrics::DEFAULT_MAX_BYTES);
        let reporter = Arc::new(PlayReporter::with_store(Arc::new(client.clone()), Arc::new(db.clone())));
        let downloads = Arc::new(DownloadManager::new(db.clone(), client.clone(), base.join("downloads"))?);
        // Private until the shell applies the user's settings, so plays are neither queued nor sent.
        rt.block_on(reporter.set_private(true));
        let queue_sync = QueueSync::new(client.clone(), db.clone(), reporter.clone(), rt.handle().clone());
        let friends = navia_core::lanyard::watch::FriendsWatch::start(rt.handle());
        let events: Arc<Mutex<Option<UnboundedSender<CoreEvent>>>> = Arc::default();
        let sync_watch = Arc::new(navia_core::library::sync::SyncWatch::new({
            let events = events.clone();
            move || {
                if let Some(tx) = events.lock().unwrap().as_ref() {
                    let _ = tx.unbounded_send(CoreEvent::SyncProgress);
                }
            }
        }));
        let lib = Arc::new(Self {
            friends,
            rt: OwnedRuntime(Some(rt)),
            client,
            downloads,
            db,
            artwork,
            lyrics,
            reporter,
            writes_allowed,
            from_env: connection.from_env,
            background_sync: AtomicBool::new(true),
            login_rejected: AtomicBool::new(false),
            tasks: Mutex::default(),
            events,
            background: Mutex::new(None),
            catalog: Mutex::new(None),
            genres: Mutex::new(Arc::default()),
            stations: Mutex::new(Arc::default()),
            can_share: AtomicBool::new(false),
            share_downloads: AtomicBool::new(false),
            image_support: Mutex::new(None),
            remote: Arc::default(),
            opened_playlists: Mutex::new(HashSet::new()),
            placeholder_covers: Mutex::default(),
            failed_covers: Mutex::default(),
            admin: Mutex::new(None),
            features: OnceLock::new(),
            queue_sync,
            first_sync_done: AtomicBool::new(false),
            retry: AtomicBool::new(false),
            refreshing: AtomicBool::new(false),
            sync_watch,
        });
        // Built here, off the UI thread, so the first frames don't wait for it.
        lib.rebuild_catalog();
        Ok(lib)
    }

    pub fn enter_runtime(&self) -> tokio::runtime::EnterGuard<'_> {
        self.rt.enter()
    }

    pub fn song_count(&self) -> usize {
        self.db.counts().map(|c| c.songs as usize).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.song_count() == 0
    }

    pub fn start_background(self: &Arc<Self>, events: UnboundedSender<CoreEvent>) {
        *self.events.lock().unwrap() = Some(events.clone());
        let this = self.clone();
        let task = self.rt.spawn(async move {
            // Wait for the server at launch however long it takes, then keep checking so an outage shows
            // even with nothing playing.
            this.wait_for_server(&events).await;
            let monitor = this.clone();
            let monitor_events = events.clone();
            let watcher = this.rt.spawn(async move { monitor.watch_connection(monitor_events).await });
            this.tasks.lock().unwrap().push(watcher.abort_handle());
            let user = this.client.user().await.ok();
            *this.admin.lock().unwrap() = user.as_ref().and_then(|u| u.admin_role);
            this.share_downloads.store(user.and_then(|u| u.download_role).unwrap_or(false), Ordering::Relaxed);
            this.can_share.store(this.client.shares().await.is_ok(), Ordering::Relaxed);
            *this.image_support.lock().unwrap() = this.client.playlist_image_support().await.ok();
            this.load_server_lists().await;
            if let Some(features) = ServerFeatures::detect(&this.client).await {
                let _ = this.features.set(features);
                this.reporter.set_live_reports(features.playback_report);
                this.queue_sync.set_features(features);
            }

            let syncer = Syncer::new(this.client.clone(), this.db.clone(), history_sync_options(&this.sync_watch));
            let started = Instant::now();
            let first_run = this.is_empty();
            let progress_events = events.clone();
            let on_progress = move |_: navia_core::library::sync::SyncProgress| {
                let _ = progress_events.unbounded_send(CoreEvent::SyncProgress);
            };
            let result = if first_run { syncer.full_sync(&on_progress).await.map(|_| ()) } else { syncer.refresh(false, &on_progress).await.map(|_| ()) };
            if let Err(e) = result {
                eprintln!("navia: library sync failed: {e}");
                let _ = events.unbounded_send(CoreEvent::SyncFailed);
            }
            eprintln!("navia: {} complete in {:.1} s", if first_run { "first sync" } else { "refresh" }, started.elapsed().as_secs_f32());
            this.first_sync_done.store(true, Ordering::Relaxed);
            // List playlists, fetching songs only for ones this user owns. Smart playlists are fetched only
            // when opened.
            if let Err(e) = syncer.sync_playlists().await {
                eprintln!("navia: playlist sync failed: {e}");
            }
            this.rebuild_catalog();
            let _ = events.unbounded_send(CoreEvent::SyncDone);
            if let Err(e) = syncer.refresh_annotations().await {
                eprintln!("navia: annotation refresh failed: {e}");
            }
            this.rebuild_catalog();
            let _ = events.unbounded_send(CoreEvent::SyncDone);
            this.first_sync_done.store(true, Ordering::Relaxed);
            this.apply_background_sync();
            this.start_daily_mixes();
        });
        self.tasks.lock().unwrap().push(task.abort_handle());
    }

    /// the server's genres and internet radio stations; a list the server doesn't answer stays as it was.
    async fn load_server_lists(&self) -> bool {
        let mut changed = false;
        if let Ok(genres) = self.client.genres().await {
            let mut genres: Vec<Genre> = genres.into_iter().filter(|g| g.song_count.unwrap_or(0) > 0).map(|g| Genre { name: g.value.into(), song_count: g.song_count.unwrap_or(0) as u32 }).collect();
            genres.sort_by_key(|g| std::cmp::Reverse(g.song_count));
            let mut slot = self.genres.lock().unwrap();
            changed |= **slot != genres;
            *slot = Arc::new(genres);
        }
        if let Ok(stations) = self.client.internet_radio_stations().await {
            let stations: Vec<RadioStation> = stations.into_iter().map(|s| RadioStation { id: s.id.into(), name: s.name.into(), url: s.stream_url.unwrap_or_default().into() }).collect();
            let mut slot = self.stations.lock().unwrap();
            changed |= **slot != stations;
            *slot = Arc::new(stations);
        }
        changed
    }

    /// a page's refresh: asks the server what changed since the last look and brings in only that (the library
    /// after a scan, playlists, stars, play counts), and reads the genres and radio stations again. the rows on
    /// screen stay until `SyncDone` brings the new ones; `Refreshed` follows either way. false while one runs.
    pub fn refresh_now(self: &Arc<Self>) -> bool {
        if self.refreshing.swap(true, Ordering::SeqCst) {
            return false;
        }
        let this = self.clone();
        self.rt.spawn(async move {
            let started = Instant::now();
            let syncer = Syncer::new(this.client.clone(), this.db.clone(), history_sync_options(&this.sync_watch));
            let progress = this.events.lock().unwrap().clone();
            let on_progress = move |_: navia_core::library::sync::SyncProgress| {
                if let Some(tx) = progress.as_ref() {
                    let _ = tx.unbounded_send(CoreEvent::SyncProgress);
                }
            };
            let (changes, lists) = tokio::join!(syncer.check_for_changes(&on_progress), this.load_server_lists());
            let changed = match changes {
                Ok(c) => {
                    eprintln!("navia: page refresh in {:.1} s: {c:?}, lists changed {lists}", started.elapsed().as_secs_f32());
                    c.any()
                }
                Err(e) => {
                    eprintln!("navia: refresh failed: {e}");
                    false
                }
            };
            if changed || lists {
                this.rebuild_catalog();
                this.send(CoreEvent::SyncDone);
            }
            this.refreshing.store(false, Ordering::SeqCst);
            this.send(CoreEvent::Refreshed);
        });
        true
    }

    /// whether a page's refresh runs, or with `sync` also any library sync.
    pub fn is_refreshing(&self, sync: bool) -> bool {
        self.refreshing.load(Ordering::SeqCst) || sync && self.sync_watch.current().is_some()
    }

    /// today's mixes, made in the background once a day after the launch sync, so For You has
    /// them before it's opened; For You reloads as each lands. runs that mustn't reach external
    /// lookups only read what's stored.
    fn start_daily_mixes(self: &Arc<Self>) {
        if !external_allowed() {
            return;
        }
        let (remote, events) = (self.remote.clone(), self.events.clone());
        let changed: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            remote.lock().unwrap().for_you_rev += 1;
            if let Some(tx) = events.lock().unwrap().as_ref() {
                let _ = tx.unbounded_send(CoreEvent::RemoteReady);
            }
        });
        let task = self.rt.spawn(navia_core::library::for_you::keep_mixes_daily(self.client.clone(), self.db.clone(), changed));
        self.tasks.lock().unwrap().push(task.abort_handle());
    }

    /// Starts or stops the periodic refresh to match the setting.
    fn apply_background_sync(self: &Arc<Self>) {
        let on = self.background_sync.load(Ordering::Relaxed) && self.first_sync_done.load(Ordering::Relaxed);
        let mut slot = self.background.lock().unwrap();
        if !on {
            *slot = None;
        } else if slot.is_none() {
            let _guard = self.rt.enter();
            let syncer = Syncer::new(self.client.clone(), self.db.clone(), history_sync_options(&self.sync_watch));
            *slot = Some(BackgroundRefresh::spawn_with(syncer, REFRESH_EVERY, self.writes_allowed.then(|| self.reporter.clone())));
        }
    }

    pub fn set_background_sync(self: &Arc<Self>, on: bool) {
        self.background_sync.store(on, Ordering::Relaxed);
        self.apply_background_sync();
    }

    /// a read-only run is refused by the client.
    pub fn create_playlist(self: &Arc<Self>, name: String) {
        let (this, client, db) = (self.clone(), self.client.clone(), self.db.clone());
        self.rt.spawn(async move {
            let syncer = Syncer::new(client, db, SyncOptions::default());
            let event = match syncer.create_playlist(&name).await {
                Ok(id) => CoreEvent::PlaylistCreated { id: id.unwrap_or_default(), name, with_songs: false },
                Err(e) => CoreEvent::PlaylistCreateFailed { message: create_message(&e) },
            };
            this.send(event);
        });
    }

    pub fn update_playlist_details(self: &Arc<Self>, id: String, details: navia_core::client::PlaylistDetails) {
        let (this, db, client) = (self.clone(), self.db.clone(), self.playlist_client(&id));
        self.rt.spawn(async move {
            let result = Syncer::new(client, db, SyncOptions::default()).update_playlist_details(&id, details).await.map_err(|e| match e {
                navia_core::Error::WritesDisabled { .. } => tr("Test and read-only runs can't change playlists.").to_string(),
                e => trf("The server didn't save the change: {error}", &[("error", &e.to_string())]),
            });
            this.send(CoreEvent::PlaylistDetailsSaved { id, result });
        });
    }

    pub fn delete_playlist(self: &Arc<Self>, id: String) {
        let (this, db, client) = (self.clone(), self.db.clone(), self.playlist_client(&id));
        self.rt.spawn(async move {
            let result = Syncer::new(client, db, SyncOptions::default()).delete_playlist(&id).await.map_err(|e| match e {
                navia_core::Error::WritesDisabled { .. } => tr("Test and read-only runs can't change playlists.").to_string(),
                e => trf("The server didn't delete the playlist: {error}", &[("error", &e.to_string())]),
            });
            // its download stops and its songs leave the device unless another download holds them.
            if result.is_ok() && let Err(e) = this.downloads.collection_download_off(navia_core::library::collections::CollectionKind::Playlist, &id, false) {
                eprintln!("navia: couldn't turn off a deleted playlist's download: {e}");
            }
            this.send(CoreEvent::PlaylistDeleted { id, result });
        });
    }

    /// the save is refused if the playlist changed on the server since.
    pub fn save_playlist_move(self: &Arc<Self>, id: String, positions: Vec<usize>, before: usize) {
        let (this, db, client) = (self.clone(), self.db.clone(), self.playlist_client(&id));
        self.rt.spawn(async move {
            let stored = db.playlist(&id).ok().flatten();
            let expected: Vec<String> = stored.map(|p| p.entry.into_iter().map(|s| s.id).collect()).unwrap_or_default();
            let order = navia_core::queue_order::moved_order(expected.len(), &positions, before).map(|o| navia_core::queue_order::apply_order(&expected, &o));
            let result = match order {
                Some(order) => Syncer::new(client, db, SyncOptions::default()).save_playlist_order(&id, &expected, &order).await,
                None => Ok(()),
            };
            this.send(CoreEvent::PlaylistOrderSaved { id, result });
        });
    }

    /// The user's own regular playlists that take songs, most recently changed first, as ids and names.
    pub fn playlist_targets(&self) -> Vec<(SharedString, SharedString)> {
        self.db.playlist_targets().unwrap_or_default().into_iter().map(|p| (p.id.into(), p.name.into())).collect()
    }

    /// Appends songs to a playlist the user owns, then stores the server's copy. `toast` replaces
    /// "Added to <playlist>" once done.
    pub fn add_to_playlist(self: &Arc<Self>, id: String, name: String, rowids: Vec<u32>, toast: Option<String>) {
        let ids: Vec<String> = self.queue_ids(&rowids).into_iter().map(|(_, id, _)| id).collect();
        let (this, db, client, form) = (self.clone(), self.db.clone(), self.playlist_client(&id), self.form_post());
        self.rt.spawn(async move {
            let result = Syncer::new(client, db, SyncOptions::default()).add_to_playlist(&id, &ids, form).await;
            this.send(CoreEvent::PlaylistSongsChanged { id, name, added: true, toast, result });
        });
    }

    /// Which of these songs the stored playlist already holds, for the duplicate warning.
    pub fn already_in_playlist(&self, id: &str, rowids: &[u32]) -> Vec<u32> {
        let rows = self.queue_ids(rowids);
        let ids: Vec<String> = rows.iter().map(|(_, id, _)| id.clone()).collect();
        let held: std::collections::HashSet<String> = self.db.already_in_playlist(id, &ids).unwrap_or_default().into_iter().collect();
        rows.into_iter().filter(|(_, id, _)| held.contains(id)).map(|(rowid, _, _)| rowid).collect()
    }

    /// Removes songs at these positions of the playlist's shown list, which holds the stored songs
    /// that are in the library. They're mapped back to the playlist's own positions, and nothing is
    /// removed if the server's copy changed since.
    pub fn remove_from_playlist(self: &Arc<Self>, id: String, name: String, shown: Vec<usize>) {
        let (this, db, client, form) = (self.clone(), self.db.clone(), self.playlist_client(&id), self.form_post());
        self.rt.spawn(async move {
            let entry: Vec<String> = db.playlist(&id).ok().flatten().map(|p| p.entry.into_iter().map(|s| s.id).collect()).unwrap_or_default();
            // The same filter `playlist_songs` applies, hiding songs missing from the library.
            let present: Vec<usize> = entry.iter().enumerate().filter(|(_, s)| db.song_rowids(std::slice::from_ref(*s)).is_ok_and(|r| !r.is_empty())).map(|(i, _)| i).collect();
            let indexes: Vec<usize> = shown.iter().filter_map(|&k| present.get(k).copied()).collect();
            let result = Syncer::new(client, db, SyncOptions::default()).remove_from_playlist(&id, &entry, &indexes, form).await;
            this.send(CoreEvent::PlaylistSongsChanged { id, name, added: false, toast: None, result });
        });
    }

    /// a read-only run is refused by the client.
    pub fn create_playlist_with(self: &Arc<Self>, name: String, rowids: Vec<u32>) {
        let ids: Vec<String> = self.queue_ids(&rowids).into_iter().map(|(_, id, _)| id).collect();
        let (this, client, db, form) = (self.clone(), self.client.clone(), self.db.clone(), self.form_post());
        self.rt.spawn(async move {
            let event = match Syncer::new(client, db, SyncOptions::default()).create_playlist_with(&name, &ids, form).await {
                Ok(id) => CoreEvent::PlaylistCreated { id: id.unwrap_or_default(), name, with_songs: true },
                Err(e) => CoreEvent::PlaylistCreateFailed { message: create_message(&e) },
            };
            this.send(event);
        });
    }

    /// Saves a smart playlist's name and rules in Navidrome's JSON, creating it when `id` is `None`.
    /// The core refuses read-only runs, unusable rules and playlists this user can't change before
    /// sending anything. The stored rules replace the cached ones.
    pub fn save_smart_playlist(self: &Arc<Self>, id: Option<String>, name: String, criteria: String) {
        let (this, db, admin) = (self.clone(), self.db.clone(), *self.admin.lock().unwrap());
        let client = self.playlist_client(id.as_deref().unwrap_or(NEW_SMART_TEST));
        self.rt.spawn(async move {
            let event = match Syncer::new(client, db, SyncOptions::default()).save_smart_playlist(id.as_deref(), &name, &criteria, admin).await {
                Ok(saved) => {
                    if let Some(rules) = saved.rules {
                        this.remote.lock().unwrap().rules.insert(saved.id.clone(), rules);
                    }
                    CoreEvent::SmartSaved { id: saved.id, name: name.trim().to_string(), created: id.is_none() }
                }
                Err(e) => CoreEvent::SmartSaveFailed { message: smart_message(&e) },
            };
            this.send(event);
        });
    }

    fn form_post(&self) -> bool {
        self.features.get().is_some_and(|f| f.form_post)
    }

    /// The client share links are written with. Refused in test and read-only runs, except with
    /// `NAVIA_TEST_SHARE=1` for a check that makes one throwaway link and deletes it.
    fn share_client(&self) -> Client {
        if read_only_run() && std::env::var("NAVIA_TEST_SHARE").is_ok_and(|v| v == "1")
            && let Ok(writable) = self.client.writable_copy()
        {
            return writable;
        }
        self.client.clone()
    }

    /// Whether a link can allow downloads, because the server lets this user download.
    pub fn share_downloads(&self) -> bool {
        self.share_downloads.load(Ordering::Relaxed)
    }

    /// Loads the user's share links, newest first.
    pub fn load_shares(self: &Arc<Self>) {
        let (this, client, db) = (self.clone(), self.client.clone(), self.db.clone());
        self.rt.spawn(async move {
            let result = navia_core::shares::list(&client, &db).await.map_err(|e| share_message(&e));
            this.send(CoreEvent::Shares(result));
        });
    }

    pub fn create_share(self: &Arc<Self>, target: navia_core::shares::ShareTarget, choice: navia_core::shares::ShareChoice) {
        let (this, client, db, downloads) = (self.clone(), self.share_client(), self.db.clone(), self.share_downloads());
        self.rt.spawn(async move {
            let result = navia_core::shares::create(&client, &db, target, choice, downloads).await.map_err(|e| share_message(&e));
            this.send(CoreEvent::ShareCreated(result));
        });
    }

    pub fn update_share(self: &Arc<Self>, link: navia_core::shares::ShareLink, choice: navia_core::shares::ShareChoice) {
        let (this, client, db, downloads) = (self.clone(), self.share_client(), self.db.clone(), self.share_downloads());
        self.rt.spawn(async move {
            let result = navia_core::shares::update(&client, &db, &link, choice, downloads).await.map_err(|e| share_message(&e));
            this.send(CoreEvent::ShareSaved(result));
        });
    }

    pub fn delete_share(self: &Arc<Self>, id: String) {
        let (this, client, db) = (self.clone(), self.share_client(), self.db.clone());
        self.rt.spawn(async move {
            let result = navia_core::shares::delete(&client, &db, &id).await.map(|_| id).map_err(|e| share_message(&e));
            this.send(CoreEvent::ShareDeleted(result));
        });
    }

    fn playlist_client(&self, id: &str) -> Client {
        if read_only_run() && named_test_playlist(id)
            && let Ok(writable) = self.client.writable_copy()
        {
            return writable;
        }
        self.client.clone()
    }

    fn send(&self, event: CoreEvent) {
        if let Some(tx) = self.events.lock().unwrap().as_ref() {
            let _ = tx.unbounded_send(event);
        }
    }

    pub fn can_change_playlist_images(&self) -> bool {
        let admin = *self.admin.lock().unwrap();
        self.image_support.lock().unwrap().as_ref().is_some_and(|s| s.allowed_for(admin))
    }

    /// Replaces a playlist's image with this file, or removes the upload with `None`. A read-only
    /// run is refused unless a test names this exact playlist.
    pub fn change_playlist_image(self: &Arc<Self>, id: String, file: Option<std::path::PathBuf>) {
        let (this, db) = (self.clone(), self.db.clone());
        self.rt.spawn(async move {
            let removed = file.is_none();
            let client = this.image_client(&id);
            let syncer = Syncer::new(client, db, SyncOptions::default());
            let change = match &file {
                Some(path) => syncer.set_playlist_image(&id, path).await,
                None => syncer.remove_playlist_image(&id).await,
            };
            let event = match change {
                Ok(_) => CoreEvent::PlaylistImageChanged { id, removed },
                Err(problem) => CoreEvent::PlaylistImageFailed { message: image_message(&problem, removed) },
            };
            if let Some(tx) = this.events.lock().unwrap().as_ref() {
                let _ = tx.unbounded_send(event);
            }
        });
    }

    /// The client for image changes. `NAVIA_TEST_COVER_PLAYLIST` lets one scripted test change a
    /// playlist it created. Everything else keeps this run's write policy.
    fn image_client(&self, id: &str) -> Client {
        if read_only_run() && std::env::var("NAVIA_TEST_COVER_PLAYLIST").is_ok_and(|named| !named.is_empty() && named == id)
            && let Ok(writable) = self.client.writable_copy()
        {
            return writable;
        }
        self.client.clone()
    }

    /// Notes the listener's queue as rows, never Autoplay's, to save on the server a little later.
    /// Cheap when only the position moved.
    pub fn note_queue(&self, rows: &[u32], index: usize, position_ms: u64) {
        self.queue_sync.note_rows(rows, index, position_ms);
    }

    /// Saves the queue now if it changed since the last save, as on pause. Doesn't wait.
    pub fn flush_queue(&self) {
        let sync = self.queue_sync.clone();
        self.rt.spawn(async move {
            let _ = sync.flush().await;
        });
    }

    /// Saves the queue and ends this player's now-playing entry on quit, waiting at most two seconds.
    pub fn sync_before_quit(&self) {
        let (sync, reporter) = (self.queue_sync.clone(), self.reporter.clone());
        // The timer is created inside the runtime, since it needs its clock.
        self.rt.block_on(async move {
            let _ = tokio::time::timeout(Duration::from_secs(2), async move {
                let _ = sync.flush().await;
                reporter.stop_live().await;
            })
            .await;
        });
    }

    /// A newer queue from another client or device, as this library's rows, with who saved it, the
    /// current index and position. Blocks, so call it off the UI thread. Read-only.
    pub fn remote_queue_offer(&self) -> Option<(navia_core::queue_sync::RemoteQueue, Vec<u32>, usize)> {
        let offer = self.rt.block_on(self.queue_sync.remote_offer()).ok().flatten()?;
        let (rows, index) = offer.local_rows(&self.db)?;
        Some((offer, rows, index))
    }

    /// Records the listener's answer to the offer so it isn't offered again.
    pub fn answer_queue_offer(&self, offer: &navia_core::queue_sync::RemoteQueue) {
        self.queue_sync.answered(offer);
    }

    /// Whether top songs can be requested by artist id (`topSongsByArtistId`).
    fn top_by_id(&self) -> bool {
        self.features.get().is_some_and(|f| f.top_songs_by_id)
    }

    pub fn set_reporting(&self, on: bool) {
        let reporter = self.reporter.clone();
        let quiet = !on || !self.writes_allowed;
        self.rt.spawn(async move { reporter.set_private(quiet).await });
    }

    /// Stops everything running for this account so another can start. Downloads on disk stay.
    pub fn close(&self) {
        for task in self.tasks.lock().unwrap().drain(..) {
            task.abort();
        }
        *self.background.lock().unwrap() = None;
        *self.events.lock().unwrap() = None;
        self.downloads.cancel_all();
    }

    pub fn server_url(&self) -> &str {
        self.client.base_url()
    }

    /// Sets the share mark in the playback reports' client name.
    pub fn set_sharing(&self, on: bool) {
        self.rt.block_on(self.reporter.set_sharing(on));
    }

    /// the reports' client name carries the marks, only while sharing.
    pub fn set_listen_marks(&self, control: bool, following: bool, queued: bool) {
        self.rt.block_on(self.reporter.set_listen_marks(control, following, queued));
    }

    /// `getNowPlaying`, parsed, with live states where the server has `playbackReport`. Blocks, so
    /// call it off the UI thread. `None` when the server can't be reached.
    pub fn now_playing(&self) -> Option<Vec<navia_core::listen_along::NowPlaying>> {
        // Scripted runs can answer from a JSON file of `getNowPlaying` entries, reread on each check so
        // a test can move the other player. That tests following without real people.
        if let Some(path) = std::env::var_os("NAVIA_TEST_NOW_PLAYING").filter(|_| read_only_run()) {
            return Some(navia_core::listen_along::from_json(&std::fs::read(path).ok()?));
        }
        let live = self.features.get().is_some_and(|f| f.playback_report);
        self.rt.block_on(navia_core::listen_along::fetch(&self.client, live)).ok()
    }

    /// Sets the Lanyard address and Spotify friends. Nothing contacts Lanyard without friends, and a
    /// scripted run only reads a local stand-in unless `NAVIA_TEST_EXTERNAL=1`.
    pub fn configure_friends(&self, address: &str, friends: Vec<navia_core::lanyard::Friend>) {
        use navia_core::lanyard::{PUBLIC_ADDRESS, is_loopback, parse_address};
        let address = parse_address(address).unwrap_or_else(|| PUBLIC_ADDRESS.to_string());
        let friends = if external_allowed() || is_loopback(&address) { friends } else { Vec::new() };
        self.friends.configure(&address, friends);
    }

    /// Every Spotify friend as Lanyard last reported, with songs matched in the library. Reads the
    /// database, so call it off the UI thread.
    pub fn spotify_friends(&self) -> Vec<navia_core::lanyard::FriendNow> {
        self.friends.friends_matched(&self.db)
    }

    pub fn username(&self) -> &str {
        self.client.credentials().username()
    }

    /// retries with a growing delay.
    async fn wait_for_server(&self, events: &UnboundedSender<CoreEvent>) {
        let mut delay = Duration::from_secs(2);
        loop {
            match self.client.ping().await {
                Ok(_) => {
                    self.downloads.set_online(true);
                    let _ = events.unbounded_send(CoreEvent::Connected(true));
                    return;
                }
                Err(e) => {
                    eprintln!("navia: can't reach the server: {e}");
                    // Only once, so Cancel on the sign-in screen stays canceled.
                    if e.is_auth_failure() && !self.login_rejected.swap(true, Ordering::Relaxed) {
                        let _ = events.unbounded_send(CoreEvent::LoginRejected);
                    }
                    self.downloads.set_online(false);
                    let _ = events.unbounded_send(CoreEvent::Connected(false));
                }
            }
            self.retry_wait(delay).await;
            delay = (delay * 2).min(Duration::from_secs(60));
        }
    }

    /// Sleeps for `delay`, or until the user asks to retry now.
    async fn retry_wait(&self, delay: Duration) {
        self.retry.store(false, Ordering::Relaxed);
        let until = Instant::now() + delay;
        while Instant::now() < until && !self.retry.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn watch_connection(self: Arc<Self>, events: UnboundedSender<CoreEvent>) {
        let mut up = true;
        loop {
            self.retry_wait(if up { Duration::from_secs(30) } else { Duration::from_secs(10) }).await;
            let reachable = self.client.ping().await.is_ok();
            if reachable != up {
                up = reachable;
                // Automatic downloads wait while the server is away. Coming back syncs and retries.
                self.downloads.set_online(up);
                let _ = events.unbounded_send(CoreEvent::Connected(up));
            }
        }
    }

    /// Asks the launch wait and the monitor to try the server now.
    pub fn retry_connection(&self) {
        self.retry.store(true, Ordering::Relaxed);
    }

    /// the offline switch. on, every server request fails at once as if it couldn't be reached,
    /// so the connection check reports offline without sending anything; off, it checks now.
    pub fn set_offline(&self, on: bool) {
        self.client.set_offline(on);
        if on {
            self.downloads.set_online(false);
        }
        self.retry_connection();
    }

    /// off the UI thread: a large library takes some ms.
    pub fn order(&self, sort: SortKey, query: &str, filters: &navia_core::song_filters::FilterState) -> Vec<u32> {
        let filter = (!query.trim().is_empty()).then_some(query);
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
        self.db.filtered_song_order(song_order_of(sort), filter, filters, now).unwrap_or_default()
    }

    /// What "Play" on an artist plays. The server's top songs, or their albums when there are none,
    /// as is usual without Last.fm. Duplicates are removed.
    pub fn artist_play_rowids(&self, artist_id: &str, artist_name: &str) -> Vec<u32> {
        // The count applies before matching against the library, so ask for plenty.
        let top = self.rt.block_on(self.client.artist_top_songs(artist_id, artist_name, self.top_by_id(), 50)).unwrap_or_default();
        let mut seen = std::collections::HashSet::new();
        let ids: Vec<String> = top.into_iter().map(|s| s.id).filter(|id| seen.insert(id.clone())).collect();
        let rowids = self.db.song_rowids(&ids).unwrap_or_default();
        if rowids.is_empty() { self.db.artist_album_rowids(artist_id).unwrap_or_default() } else { rowids }
    }

    /// Row ids of the songs in the song cache, which play offline like downloads.
    pub fn cached_song_rowids(&self) -> Vec<u32> {
        let ids = self.downloads.song_cache().map(|c| c.song_ids()).unwrap_or_default();
        self.db.song_rowids(&ids).unwrap_or_default()
    }

    /// Whether a song is in the song cache, with one indexed lookup.
    pub fn is_row_cached(&self, rowid: u32) -> bool {
        let Some(cache) = self.downloads.song_cache() else { return false };
        self.rows(&[rowid]).first().is_some_and(|(_, s)| cache.contains(&s.id))
    }

    pub fn rowid_for_song(&self, id: &str) -> Option<u32> {
        self.db.song_rowids(&[id.to_string()]).ok()?.into_iter().next()
    }

    /// Artist Radio for an artist id, as library rows in the radio's order, cached. Uses Deezer's
    /// artist radio mapped onto the library while the Deezer switch is on, filled from the
    /// server's `getSimilarSongs2`. Slow the first time, so only call it from a background thread.
    pub fn artist_similar_rowids(&self, id: &str) -> Vec<u32> {
        // A scripted run allows one lookup on purpose with NAVIA_TEST_RADIO.
        if !external_allowed() && std::env::var_os("NAVIA_TEST_RADIO").is_none() {
            return Vec::new();
        }
        let deezer = navia_core::artist_radio::Deezer::shared();
        let radio = self.rt.block_on(navia_core::artist_radio::artist_radio(&self.client, &self.db, deezer, id, ""));
        let ids: Vec<String> = radio.songs.into_iter().map(|s| s.id).collect();
        let rows = self.rowids_in_order(&ids);
        self.remote.lock().unwrap().similar.insert(id.to_string(), rows.clone());
        rows
    }

    /// This account's Deezer recommendations choice. `None` until the listener chooses, which counts as off.
    pub fn deezer_recommendations(&self) -> Option<bool> {
        navia_core::deezer::setting(&self.db)
    }

    /// Saves the choice for this account. `None` forgets it, so the app asks again.
    pub fn set_deezer_recommendations(&self, on: Option<bool>) -> navia_core::Result<()> {
        {
            let mut remote = self.remote.lock().unwrap();
            remote.heat.clear();
            remote.upcoming.generation += 1;
        }
        navia_core::deezer::set_setting(&self.db, on)
    }

    /// an album's Deezer ranks by song row, from the library's song ids.
    fn heat_rows(db: &CoreDb, album_id: &str, ranks: &HashMap<String, u64>) -> HashMap<u32, u64> {
        db.album_rows(album_id).unwrap_or_default().into_iter().filter_map(|(rowid, s)| ranks.get(&s.id).map(|r| (rowid, *r))).collect()
    }

    pub fn autoplay_mode(&self) -> navia_core::engine::AutoplayMode {
        navia_core::engine::mode(&self.db)
    }

    /// Saves the Autoplay mode. Autoplay follows it at its next update.
    pub fn set_autoplay_mode(&self, mode: navia_core::engine::AutoplayMode) -> navia_core::Result<()> {
        navia_core::engine::set_mode(&self.db, mode)
    }

    /// A radio from one song, as the library rows to queue after it. Read-only. Slow the first time,
    /// so never call it on the UI thread.
    pub fn radio_rowids(&self, song_id: &str) -> Vec<u32> {
        let songs = self.rt.block_on(navia_core::autoplay::radio(&self.client, &self.db, song_id));
        let ids: Vec<String> = songs.into_iter().map(|s| s.id).collect();
        self.rowids_in_order(&ids)
    }

    /// Autoplay's view of queue rows, each row's song id and its other-versions key.
    pub fn autoplay_taken(&self, rowids: &[u32]) -> Vec<(u32, navia_core::autoplay::Taken)> {
        self.db.songs_by_rowid(rowids).unwrap_or_default().iter().map(|(r, s)| (*r, navia_core::autoplay::Taken::of(s))).collect()
    }

    /// Fetches Autoplay's seeds as `mode` needs. The server's similar songs, which are read-only so
    /// test runs may ask too, and Deezer's sources while the Deezer switch is on. Slow the first
    /// time, so never call it on the UI thread.
    pub fn autoplay_answers(&self, seeds: &[String], mode: navia_core::engine::AutoplayMode) -> navia_core::autoplay::Answers {
        self.rt.block_on(navia_core::autoplay::fetch_answers(&self.client, &self.db, seeds, mode))
    }

    /// Looks up the BPM of an Autoplay song about to be queued. Background only.
    pub fn autoplay_bpm(&self, rowid: u32) {
        let Some(id) = self.db.songs_by_rowid(&[rowid]).ok().and_then(|s| s.into_iter().next()).map(|(_, s)| s.id) else { return };
        self.rt.block_on(navia_core::autoplay::look_up_bpm(&self.db, &id));
    }

    /// The next Recommended batch for a playlist, as library rows. Asks the server for similar songs
    /// using the playlist's songs and its artists' stored similar artists. `rec` starts the visit's
    /// recommender when empty.
    pub fn recommended_batch(&self, playlist_id: &str, rec: &Mutex<Option<navia_core::recommend::Recommender>>) -> Vec<u32> {
        let songs = self.rt.block_on(navia_core::recommend::batch_for_playlist(&self.client, &self.db, playlist_id, rec));
        let ids: Vec<String> = songs.into_iter().map(|s| s.id).collect();
        self.rowids_in_order(&ids)
    }

    /// skips songs the library doesn't have.
    pub fn rowids_in_order(&self, ids: &[String]) -> Vec<u32> {
        ids.iter().filter_map(|id| self.db.song_rowids(std::slice::from_ref(id)).ok()?.into_iter().next()).collect()
    }

    fn catalog(&self) -> Arc<Catalog> {
        if let Some(c) = self.catalog.lock().unwrap().as_ref() {
            return c.clone();
        }
        self.rebuild_catalog()
    }

    /// Reads every album and artist. Slow on a big library, so never call it from the UI thread.
    fn rebuild_catalog(&self) -> Arc<Catalog> {
        let built = Arc::new(self.build_catalog());
        // The seeds may have moved. Today's server answers are stored, so this is cheap.
        {
            let mut remote = self.remote.lock().unwrap();
            remote.forget_taste();
            remote.more_by.clear();
            remote.heat.clear();
            remote.upcoming.generation += 1;
            // The library may have new albums; ask again on the next visit, showing the old answer meanwhile.
            let answered = std::mem::take(&mut remote.discography);
            remote.discography_stale.extend(answered);
        }
        *self.catalog.lock().unwrap() = Some(built.clone());
        built
    }

    fn build_catalog(&self) -> Catalog {
        let albums_raw = self.db.list_albums(AlbumSort::MostPlayed, 0, u32::MAX, None).unwrap_or_default();
        let mut artist_plays: HashMap<String, u32> = HashMap::new();
        let mut artist_albums: HashMap<String, Vec<u32>> = HashMap::new();
        let albums: Vec<Album> = albums_raw
            .into_iter()
            .enumerate()
            .map(|(i, a)| {
                let plays = a.play_count.unwrap_or(0).max(0) as u32;
                if let Some(id) = &a.artist_id {
                    *artist_plays.entry(id.clone()).or_default() += plays;
                    artist_albums.entry(id.clone()).or_default().push(i as u32);
                }
                let release_day = navia_core::upcoming::release_day(&a);
                Album {
                    id: a.id.into(),
                    // Shown without the version Navidrome appends to the name.
                    name: navia_core::display::album_name(&a.name, a.version.as_deref()).into_owned().into(),
                    edition: navia_core::display::edition(a.version.as_deref()).map(Into::into),
                    edition_on_card: false,
                    artist: a.artist.unwrap_or_default().into(),
                    artist_id: a.artist_id.map(Into::into),
                    year: a.year.unwrap_or(0).max(0) as u32,
                    song_count: a.song_count.unwrap_or(0).max(0) as u32,
                    duration_s: a.duration.unwrap_or(0).max(0) as u32,
                    genre: a.genre.or_else(|| a.genres.first().map(|g| g.name.clone())).unwrap_or_default().into(),
                    added: a.created.as_deref().map_or(0, iso_seconds),
                    plays,
                    last_played: a.played.as_deref().map_or(0, iso_seconds),
                    starred: a.starred.is_some(),
                    explicit: navia_core::client::is_explicit(a.explicit_status.as_deref()),
                    disc_titles: a.disc_titles.iter().filter(|d| d.disc > 0 && !d.title.trim().is_empty()).map(|d| (d.disc as u32, SharedString::from(d.title.clone()))).collect(),
                    release_day,
                }
            })
            .collect();
        // Cards name an album's edition when another by the same artist has the same name.
        let mut albums = albums;
        {
            let mut seen: HashMap<(String, String), u32> = HashMap::new();
            for a in &albums {
                *seen.entry((a.artist.to_lowercase(), a.name.to_lowercase())).or_default() += 1;
            }
            for a in albums.iter_mut() {
                a.edition_on_card = a.edition.is_some() && seen.get(&(a.artist.to_lowercase(), a.name.to_lowercase())).copied().unwrap_or(0) > 1;
            }
        }
        // newest first, as the trait promises.
        for list in artist_albums.values_mut() {
            list.sort_by_key(|i| std::cmp::Reverse(albums[*i as usize].year));
        }
        let mut artists: Vec<Artist> = self
            .db
            .list_artists(0, u32::MAX)
            .unwrap_or_default()
            .into_iter()
            .map(|a| {
                let plays = artist_plays.get(&a.id).copied().unwrap_or(0);
                let image = a.cover_art.filter(|c| !c.is_empty()).or_else(|| Some(format!("ar-{}", a.id))).map(Into::into);
                Artist { id: a.id.into(), name: a.name.into(), image, plays, starred: a.starred.is_some() }
            })
            .collect();
        artists.sort_by_key(|a| std::cmp::Reverse(a.plays));
        let artist_index: HashMap<String, u32> = artists.iter().enumerate().map(|(i, a)| (a.id.to_string(), i as u32)).collect();
        let album_index: HashMap<String, u32> = albums.iter().enumerate().map(|(i, a)| (a.id.to_string(), i as u32)).collect();
        let taste = self.db.taste().map_err(|e| eprintln!("navia: couldn't score recent plays: {e}")).ok();
        let rotation = taste
            .as_ref()
            .map(|t| self.db.artists_by_rowid(&t.back_in_rotation).unwrap_or_default().into_iter().filter_map(|(_, a)| artist_index.get(&a.id).copied()).collect())
            .unwrap_or_default();
        let genre_page = self.db.genre_page().map_err(|e| eprintln!("navia: couldn't read genres: {e}")).unwrap_or_default();
        let genre_page = Arc::new(GenrePage::from_core(genre_page, |c| album_index.get(&c.id).copied()));
        Catalog { albums: Arc::new(albums), artists: Arc::new(artists), artist_albums, taste, artist_index, album_index, rotation, genre_page }
    }

    /// Fetches one server answer in the background and tells the shell when it lands, so nothing
    /// blocks a frame.
    fn fetch_remote<Fut: std::future::Future<Output = ()> + Send + 'static>(&self, key: String, fetch: impl FnOnce() -> Fut + Send + 'static) {
        if !self.remote.lock().unwrap().pending.insert(key.clone()) {
            return;
        }
        let (remote, events) = (self.remote.clone(), self.events.clone());
        self.rt.spawn(async move {
            fetch().await;
            remote.lock().unwrap().pending.remove(&key);
            if let Some(tx) = events.lock().unwrap().as_ref() {
                let _ = tx.unbounded_send(CoreEvent::RemoteReady);
            }
        });
    }

    /// Fetches the artist page's info from `getArtistInfo2`, once per artist and day. The biography
    /// lands first, then Similar Artists with Deezer's merged in while its switch is on. Runs that
    /// mustn't reach external lookups only read earlier answers.
    fn fetch_artist_page(&self, artist_id: &str) {
        let (client, db, remote, events, id) = (self.client.clone(), self.db.clone(), self.remote.clone(), self.events.clone(), artist_id.to_string());
        let ask = external_allowed();
        self.fetch_remote(format!("artist-page:{artist_id}"), move || async move {
            if ask {
                let info = similar_artists::artist_page_info(&client, &db, &id).await.unwrap_or_default();
                let found = ArtistInfo { biography: plain_text(&info.biography.unwrap_or_default()).into(), similar: info.similar_artist.into_iter().map(|a| a.name.into()).collect() };
                remote.lock().unwrap().info.insert(id.clone(), found);
                if let Some(tx) = events.lock().unwrap().as_ref() {
                    let _ = tx.unbounded_send(CoreEvent::RemoteReady);
                }
            }
            let found = similar_artists::similar_artists(&client, &db, navia_core::deezer::Deezer::shared(), &id, ask).await;
            remote.lock().unwrap().similar_artists.insert(id, found.into_iter().map(|(_, a)| a.id).collect());
        });
    }

    fn artist_name(&self, artist_id: &str) -> Option<String> {
        self.db.artist(artist_id).ok().flatten().map(|a| a.name)
    }

    /// Sends a star change to the server when writes are allowed. Otherwise the change stays in
    /// memory only. A refused write undoes the local change.
    pub fn send_star(&self, table: StarTable, id: String, rowid: Option<u32>, on: bool) {
        if !self.writes_allowed {
            return;
        }
        let (client, db, events) = (self.client.clone(), self.db.clone(), self.events.clone());
        self.rt.spawn(async move {
            let id = match (rowid, id.is_empty()) {
                (Some(rowid), true) => db.songs_by_rowid(&[rowid]).ok().and_then(|r| r.into_iter().next()).map(|(_, s)| s.id).unwrap_or_default(),
                _ => id,
            };
            if id.is_empty() {
                return;
            }
            let target = match table {
                StarTable::Song => StarTarget::Song(&id),
                StarTable::Album => StarTarget::Album(&id),
                StarTable::Artist => StarTarget::Artist(&id),
            };
            let sent = if on { client.star(target).await } else { client.unstar(target).await };
            match sent {
                Ok(()) => {
                    if let Err(e) = db.set_starred(table, &id, on) {
                        eprintln!("navia: couldn't store a star: {e}");
                    }
                }
                Err(e) => {
                    eprintln!("navia: couldn't change a star: {e}");
                    if let Some(tx) = events.lock().unwrap().as_ref() {
                        let _ = tx.unbounded_send(CoreEvent::StarFailed { table, id, rowid, on });
                    }
                }
            }
        });
    }

    /// Today's "Because you've been playing X" as ids, once known. The first request each day runs
    /// in the background with a few `getArtistInfo2` calls at most, and Home fills in when it lands.
    /// Runs that mustn't reach external lookups only use what's stored.
    fn because(&self, day: i64) -> Option<(String, Vec<String>)> {
        let (shown, generation) = {
            let remote = self.remote.lock().unwrap();
            match remote.because.as_ref().map(|answer| remote.taste_answer(answer, day)) {
                Some(TasteAnswer::Current(found)) => return found.clone(),
                Some(TasteAnswer::Stale(found)) => (found.clone(), remote.taste_generation),
                _ => (None, remote.taste_generation),
            }
        };
        let (client, db, remote) = (self.client.clone(), self.db.clone(), self.remote.clone());
        let ask = external_allowed();
        self.fetch_remote(format!("because:{day}:{generation}"), move || async move {
            let state = if ask { taste::refresh_because(&client, &db, day).await } else { db.because(day) };
            let found = match state {
                Ok(Because::Ready { seed, artists }) => {
                    let id_of = |rows: &[u32]| db.artists_by_rowid(rows).unwrap_or_default().into_iter().map(|(_, a)| a.id).collect::<Vec<_>>();
                    id_of(&[seed]).into_iter().next().map(|seed| (seed, id_of(&artists)))
                }
                Ok(_) => None,
                Err(e) => {
                    eprintln!("navia: similar artists: {e}");
                    None
                }
            };
            remote.lock().unwrap().land_taste(generation, |r| r.because = Some(TasteMemo { day, generation, found }));
        });
        shown
    }

    /// Today's Artist Radio stations as ids, once known. Worked out in the background with at most
    /// one `getArtistInfo2` per candidate per day, plus Deezer while its switch is on. Home fills in
    /// when it lands. Runs that mustn't reach external lookups use what's stored.
    fn stations(&self, day: i64) -> Vec<(String, Vec<String>)> {
        let (shown, generation) = {
            let remote = self.remote.lock().unwrap();
            match remote.stations.as_ref().map(|answer| remote.taste_answer(answer, day)) {
                Some(TasteAnswer::Current(found)) => return found.clone(),
                Some(TasteAnswer::Stale(found)) => (found.clone(), remote.taste_generation),
                _ => (Vec::new(), remote.taste_generation),
            }
        };
        let (client, db, remote) = (self.client.clone(), self.db.clone(), self.remote.clone());
        let ask = external_allowed();
        self.fetch_remote(format!("stations:{day}:{generation}"), move || async move {
            let found = navia_core::stations::artist_stations(&client, &db, navia_core::deezer::Deezer::shared(), ask).await;
            let found = found.into_iter().map(|s| (s.seed.1.id, s.neighbours.into_iter().map(|(_, a)| a.id).collect())).collect();
            remote.lock().unwrap().land_taste(generation, |r| r.stations = Some(TasteMemo { day, generation, found }));
        });
        shown
    }

    /// Today's Essentials as artist ids (`navia_core::library::essentials`): the top artists' top
    /// songs, asked a few at once and kept for the day, so a relaunch shows them at once. runs that
    /// mustn't reach external lookups use what's stored.
    fn essentials(&self, day: i64) -> Vec<String> {
        let (shown, generation) = {
            let remote = self.remote.lock().unwrap();
            match remote.essentials.as_ref().map(|answer| remote.taste_answer(answer, day)) {
                Some(TasteAnswer::Current(found)) => return found.clone(),
                Some(TasteAnswer::Stale(found)) => (found.clone(), remote.taste_generation),
                _ => (Vec::new(), remote.taste_generation),
            }
        };
        let (client, db, remote, by_id) = (self.client.clone(), self.db.clone(), self.remote.clone(), self.top_by_id());
        let ask = external_allowed();
        self.fetch_remote(format!("essentials:{day}:{generation}"), move || async move {
            let found = if ask { navia_core::library::essentials::refresh_essentials(&client, &db, by_id, day).await } else { db.essentials(day) };
            let found = found.unwrap_or_default().into_iter().map(|e| e.artist_id).collect();
            remote.lock().unwrap().land_taste(generation, |r| r.essentials = Some(TasteMemo { day, generation, found }));
        });
        shown
    }

    /// "Your Top Songs": what was played in about the last six months, most played first.
    pub fn top_recent_rows(&self, limit: usize) -> Vec<u32> {
        self.db.top_song_rowids(limit as u32).unwrap_or_default()
    }

    /// Fetches and stores a playlist's songs with the read-only `getPlaylist`, for enabling its
    /// download before this device has them. Blocks, so call it off the UI thread.
    pub fn fetch_playlist_songs(&self, id: &str) {
        let syncer = Syncer::new(self.client.clone(), self.db.clone(), SyncOptions::default());
        if let Err(e) = self.rt.block_on(syncer.refresh_playlist(id)) {
            eprintln!("navia: couldn't fetch a playlist: {e}");
        }
    }

    /// The song's original file as the server describes it, for the quality badge.
    pub fn audio_file(&self, rowid: u32) -> Option<navia_core::audio_quality::FileFacts> {
        let (_, song) = self.db.songs_by_rowid(&[rowid]).ok()?.into_iter().next()?;
        navia_core::audio_quality::FileFacts::from_song(&song)
    }

    pub fn rows(&self, rowids: &[u32]) -> Vec<(u32, Song)> {
        self.db.songs_by_rowid(rowids).unwrap_or_default().into_iter().map(|(r, s)| (r, to_song(s))).collect()
    }

    /// Each row's recency for Fewer Repeats. This session's plays in `heard`, else the server's
    /// last-played date, else 0.
    pub fn recency(&self, rowids: &[u32], heard: &std::collections::HashSet<String>) -> Vec<f32> {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
        self.db.recency_by_rowid(rowids, heard, now).unwrap_or_else(|_| vec![0.0; rowids.len()])
    }

    pub fn queue_ids(&self, rowids: &[u32]) -> Vec<(u32, String, Option<u64>)> {
        // The database returns each song once, but a queue can hold the same song twice.
        let found: HashMap<u32, (String, Option<u64>)> = self.db.songs_by_rowid(rowids).unwrap_or_default().into_iter().map(|(r, s)| (r, (s.id, s.duration.map(|d| d as u64 * 1000)))).collect();
        rowids.iter().filter_map(|r| found.get(r).map(|(id, d)| (*r, id.clone(), *d))).collect()
    }

    pub fn starred_rowids(&self) -> Vec<u32> {
        self.db.starred_song_rowids().unwrap_or_default()
    }

    /// for Discord art; skips the lookup when not needed.
    pub fn album_mbid(&self, album_id: &str) -> Option<String> {
        self.db.album(album_id).ok().flatten().and_then(|a| a.music_brainz_id)
    }

    /// The song's ISRC from the library, for Discord art. Libraries synced before ISRCs were stored
    /// lack it, and the art thread asks the server instead.
    pub fn song_isrc(&self, song_id: &str) -> Option<String> {
        self.db.song(song_id).ok().flatten().and_then(|s| s.isrc.into_iter().next())
    }

    /// Cover art for the OS media controls, as a file on disk.
    pub fn cover_file(&self, album_id: &str) -> Option<String> {
        let found = self.cover(album_id, Some(512))?;
        Some(format!("file://{}", found.path.display()))
    }

    /// the cover from the core's cache or the server, noting placeholders and logging a failure once.
    fn cover(&self, id: &str, size: Option<u32>) -> Option<navia_core::artwork::CoverArt> {
        let cover = self.cover_id(id);
        match self.rt.block_on(self.artwork.cover(&cover, size)) {
            Ok(found) => {
                let mut placeholders = self.placeholder_covers.lock().unwrap();
                if found.placeholder {
                    placeholders.insert(id.to_string());
                } else {
                    placeholders.remove(id);
                }
                Some(found)
            }
            Err(e) => {
                if self.failed_covers.lock().unwrap().insert(cover.clone()) {
                    eprintln!("navia: cover {cover} failed: {e}");
                }
                None
            }
        }
    }

    fn cover_id(&self, id: &str) -> String {
        if id.starts_with("ar-") || id.starts_with("pl-") {
            return id.to_string();
        }
        self.db.album(id).ok().flatten().and_then(|a| a.cover_art).filter(|c| !c.is_empty()).unwrap_or_else(|| format!("al-{id}"))
    }

    fn cover_bytes(&self, id: &str, size: Option<u32>) -> Option<Vec<u8>> {
        if let Some(image) = id.strip_prefix(UPCOMING_COVER) {
            return self.rt.block_on(navia_core::upcoming::cover(&self.db, navia_core::deezer::Deezer::shared(), image, size.unwrap_or(UPCOMING_COVER_MAX)));
        }
        let found = self.cover(id, size)?;
        match std::fs::read(&found.path) {
            Ok(bytes) => Some(bytes),
            Err(e) => {
                if self.failed_covers.lock().unwrap().insert(found.path.display().to_string()) {
                    eprintln!("navia: cover file {} unreadable: {e}", found.path.display());
                }
                None
            }
        }
    }
}

fn create_message(e: &navia_core::Error) -> String {
    match e {
        navia_core::Error::WritesDisabled { .. } => tr("Test and read-only runs can't create playlists.").to_string(),
        e => trf("Couldn't create the playlist: {error}", &[("error", &e.to_string())]),
    }
}

/// The `NAVIA_TEST_PLAYLIST` value that lets a test create one smart playlist of its own.
const NEW_SMART_TEST: &str = "new-smart";

/// A share link call's failure, in the user's language.
fn share_message(e: &navia_core::Error) -> String {
    match e {
        navia_core::Error::WritesDisabled { .. } => tr("Test and read-only runs can't change share links.").to_string(),
        navia_core::Error::HttpStatus { status: 501, .. } => tr("Sharing is turned off on this server").to_string(),
        e => trf("Couldn't reach the server: {reason}", &[("reason", &e.to_string())]),
    }
}

fn named_test_playlist(id: &str) -> bool {
    std::env::var("NAVIA_TEST_PLAYLIST").is_ok_and(|named| !named.is_empty() && named == id)
}

/// The core's reasons in the user's language. Rules problems name fields, so they're kept as is.
fn smart_message(e: &SmartSaveError) -> String {
    let text = match e {
        SmartSaveError::WritesDisabled => tr("Test and read-only runs can't change playlists."),
        SmartSaveError::NotSmart => tr("This isn't a smart playlist."),
        SmartSaveError::NotAllowed => tr("Only its owner can change this playlist"),
        SmartSaveError::NotFound => tr("This playlist no longer exists on the server."),
        SmartSaveError::Unsupported => tr("This server can't save smart playlists."),
        SmartSaveError::Busy => tr("The server is limiting sign-ins right now. Try again in a little while."),
        SmartSaveError::Invalid(said) => return crate::i18n::tr_dyn(said).to_string(),
        SmartSaveError::Connection(_) | SmartSaveError::Server(_) => return trf("Couldn't save the playlist. {problem}", &[("problem", &e.to_string())]),
    };
    text.to_string()
}

fn image_message(problem: &PlaylistImageError, removed: bool) -> String {
    match problem {
        PlaylistImageError::File(_) | PlaylistImageError::TooLarge | PlaylistImageError::NotAnImage => problem.to_string(),
        _ if removed => trf("Couldn't remove the image. {problem}", &[("problem", &problem.to_string())]),
        _ => trf("Couldn't upload the image. {problem}", &[("problem", &problem.to_string())]),
    }
}

/// a stale item makes Navidrome refresh its stored info (a write), so scripted runs skip external lookups
/// unless `NAVIA_TEST_EXTERNAL=1`.
/// never stored (Deezer's terms).
pub const UPCOMING_COVER: &str = "dz:";
/// for the page's tint, which asks without a size.
const UPCOMING_COVER_MAX: u32 = 500;

fn upcoming(album: navia_core::upcoming::UpcomingAlbum) -> Upcoming {
    Upcoming {
        id: album.id,
        release_moment: navia_core::upcoming::release_moment(&album.release_date).unwrap_or_default(),
        title: album.title.into(),
        artist: album.artist.into(),
        artist_id: album.artist_id.into(),
        cover: format!("{UPCOMING_COVER}{}", album.cover).into(),
        release_date: album.release_date.into(),
        total: album.total,
        explicit: album.explicit,
    }
}

fn external_allowed() -> bool {
    let driven = std::env::var_os("NAVIA_SCRIPT").is_some() || std::env::var_os("NAVIA_SCROLL_BENCH").is_some();
    !driven || std::env::var_os("NAVIA_TEST_EXTERNAL").is_some_and(|v| v == "1")
}

/// Strips HTML tags and entities from Last.fm album notes and biographies.
fn plain_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = out.replace("&amp;", "&").replace("&quot;", "\"").replace("&#39;", "'").replace("&apos;", "'").replace("&lt;", "<").replace("&gt;", ">").replace("&nbsp;", " ");
    out.trim().to_string()
}

/// Albums as catalog indexes.
fn for_you_page(db: &CoreDb, catalog: &Catalog, page: navia_core::library::for_you::ForYou) -> ForYouPage {
    let albums = |rows: &[u32]| -> Vec<u32> { db.albums_by_rowid(rows).unwrap_or_default().into_iter().filter_map(|(_, a)| catalog.album_index.get(&a.id).copied()).collect() };
    let mixes = page
        .mixes
        .into_iter()
        .map(|m| {
            let songs = db.songs_by_rowid(&m.songs).unwrap_or_default();
            let mut covers: Vec<SharedString> = Vec::new();
            for (_, s) in &songs {
                if let Some(al) = &s.album_id
                    && covers.len() < 4
                    && !covers.iter().any(|c| c.as_ref() == al)
                {
                    covers.push(al.clone().into());
                }
            }
            MixInfo {
                id: m.id.into(),
                genre: m.kind == navia_core::library::for_you::MixKind::Genre,
                name: m.name.into(),
                duration_s: songs.iter().map(|(_, s)| s.duration.unwrap_or(0).max(0) as u32).sum(),
                songs: m.songs,
                artists: m.artists.into_iter().map(SharedString::from).collect(),
                covers,
            }
        })
        .collect();
    ForYouPage {
        mixes,
        new_to_you: albums(&page.new_to_you),
        unplayed: albums(&page.unplayed),
        forgotten: page.forgotten,
        on_this_day: albums(&page.on_this_day),
        not_played_recently: albums(&page.not_played_recently),
        loading: page.building || (page.needs_refresh && external_allowed()),
    }
}

/// built once per sync.
struct Catalog {
    albums: Arc<Vec<Album>>,
    artists: Arc<Vec<Artist>>,
    artist_albums: HashMap<String, Vec<u32>>,
    /// Recent-taste scores as of this build, which Home follows.
    taste: Option<Arc<Taste>>,
    artist_index: HashMap<String, u32>,
    album_index: HashMap<String, u32>,
    /// Back in Rotation as artist indexes, since the taste holds row ids.
    rotation: Vec<u32>,
    /// built here so it never runs on the UI thread.
    genre_page: Arc<GenrePage>,
}

#[derive(Default)]
struct Remote {
    pending: HashSet<String>,
    top: HashMap<String, Vec<u32>>,
    similar: HashMap<String, Vec<u32>>,
    info: HashMap<String, ArtistInfo>,
    similar_artists: HashMap<String, Vec<String>>,
    /// "More by" per album id, from the library. Cleared with the catalog.
    more_by: HashMap<String, Option<navia_core::library::discography::MoreBy>>,
    /// album id to Deezer rank by song row (`navia_core::album_heat`); cleared with the catalog and the Deezer switch.
    heat: HashMap<String, HashMap<u32, u64>>,
    upcoming: UpcomingAnswers,
    rules: HashMap<String, String>,
    /// Artists' albums by release type, as album ids. `discography_stale` holds the answers from
    /// before the last catalog rebuild, shown until new ones land.
    discography: HashMap<String, Vec<(ReleaseSection, Vec<String>)>>,
    discography_stale: HashMap<String, Vec<(ReleaseSection, Vec<String>)>>,
    descriptions: HashMap<String, SharedString>,
    /// Today's "Because you've been playing" as seed id and similar artist ids.
    because: Option<TasteMemo<Option<(String, Vec<String>)>>>,
    /// Today's Artist Radio stations as (seed id, neighbor ids) pairs.
    stations: Option<TasteMemo<Vec<(String, Vec<String>)>>>,
    /// Bumped by every catalog rebuild, so answers worked out before it are stale.
    taste_generation: u64,
    /// For You as (local day, the catalog it was mapped with, `for_you_rev`, the page).
    for_you: Option<(i64, usize, u64, Arc<ForYouPage>)>,
    /// moves each time something new for For You is stored (a mix landed).
    for_you_rev: u64,
    /// Today's Essentials as artist ids (`navia_core::library::essentials`).
    essentials: Option<TasteMemo<Vec<String>>>,
}

/// an answer from an older generation (a rebuild, the Deezer switch) shows while it's asked again.
#[derive(Default)]
struct UpcomingAnswers {
    generation: u64,
    artists: HashMap<String, (u64, Vec<Upcoming>)>,
    home: Option<(u64, Vec<Upcoming>)>,
    pages: HashMap<u64, (u64, UpcomingView)>,
}

/// A Home row worked out in the background from the play history, with the day and catalog
/// generation it was worked out for.
struct TasteMemo<T> {
    day: i64,
    generation: u64,
    found: T,
}

enum TasteAnswer<'a, T> {
    /// From today and the current library.
    Current(&'a T),
    /// From today but an older library. Shown while it's worked out again.
    Stale(&'a T),
    /// From another day. Not shown.
    Expired,
}

impl Remote {
    /// The library changed, so Home's taste rows are worked out again and late answers are dropped.
    fn forget_taste(&mut self) {
        self.taste_generation += 1;
    }

    fn taste_answer<'a, T>(&self, memo: &'a TasteMemo<T>, day: i64) -> TasteAnswer<'a, T> {
        match (memo.day == day, memo.generation == self.taste_generation) {
            (true, true) => TasteAnswer::Current(&memo.found),
            (true, false) => TasteAnswer::Stale(&memo.found),
            _ => TasteAnswer::Expired,
        }
    }

    /// Stores an answer unless the library changed since it was started.
    fn land_taste(&mut self, generation: u64, store: impl FnOnce(&mut Self)) {
        if generation == self.taste_generation {
            store(self);
        }
    }
}

/// `validUntil` only appears once a smart playlist has been evaluated, and the owner's `readonly`
/// is only true for smart ones. Someone else's unevaluated smart playlist can't be detected.
fn is_smart(p: &navia_core::client::Playlist, me: &str) -> bool {
    p.valid_until.is_some() || p.readonly.unwrap_or(false) && p.owner.as_deref().is_some_and(|o| crate::model::same_user(o, me))
}

/// Snaps to a few fixed sizes so Navidrome caches each once, and artists share sizes with albums.
fn cover_size(px: u32) -> u32 {
    [128u32, 256, 512].into_iter().find(|s| *s >= px).unwrap_or(512)
}

/// Parses `YYYY-MM-DDTHH:MM:SS...` to Unix seconds, or 0 if it doesn't parse.
fn iso_seconds(text: &str) -> i64 {
    let number = |range: std::ops::Range<usize>| text.get(range).and_then(|t| t.parse::<i64>().ok());
    let (Some(y), Some(m), Some(d), Some(h), Some(mi), Some(s)) = (number(0..4), number(5..7), number(8..10), number(11..13), number(14..16), number(17..19)) else { return 0 };
    // Days from civil (Howard Hinnant's algorithm).
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146_097 + doe - 719_468) * 86_400 + h * 3600 + mi * 60 + s
}

/// A song from the server's data, for one that may not be in the library, like a followed song.
pub fn song_from_core(s: CoreSong) -> Song {
    to_song(s)
}

fn to_song(s: CoreSong) -> Song {
    let artist = s.artist_line().unwrap_or_default();
    let (artists, artist_art): (Vec<SharedString>, Vec<SharedString>) = if s.artists.is_empty() {
        (vec![SharedString::from(artist.clone())], vec![])
    } else {
        s.artists.iter().map(|a| (SharedString::from(a.name.clone()), SharedString::from(format!("ar-{}", a.id)))).unzip()
    };
    let genre = s.genre.clone().or_else(|| s.genres.first().map(|g| g.name.clone())).unwrap_or_default();
    let starred = s.starred.is_some();
    let explicit = navia_core::client::is_explicit(s.explicit_status.as_deref());
    let added = s.created.as_deref().map_or(0, iso_seconds);
    Song {
        id: s.id.into(),
        title: s.title.into(),
        artist: artist.into(),
        album: s.album.as_deref().map(|a| navia_core::display::song_album_name(s.album_id.as_deref(), a)).unwrap_or_default().into(),
        album_id: s.album_id.unwrap_or_default().into(),
        genre: genre.into(),
        year: s.year.unwrap_or(0).max(0) as u32,
        duration_s: s.duration.unwrap_or(0).max(0) as u32,
        plays: s.play_count.unwrap_or(0).max(0) as u32,
        starred,
        downloaded: false,
        // Known only once its lyrics have been fetched. See `Shell::on_lyrics_loaded`.
        has_lyrics: false,
        artists,
        artist_art,
        explicit,
        added,
        disc: s.disc_number.unwrap_or(0).max(0) as u32,
    }
}

impl Library for CoreLibrary {
    fn songs(&self) -> Arc<Vec<Song>> {
        Arc::new(Vec::new())
    }

    fn now_playing(&self) -> Option<usize> {
        None
    }

    fn account_key(&self) -> String {
        self.client.credentials().account_key(self.client.base_url())
    }

    fn account(&self) -> Option<Account> {
        Some(Account { username: self.client.credentials().username().to_string().into(), admin: *self.admin.lock().unwrap() })
    }

    fn playlists(&self) -> Vec<PlaylistSummary> {
        let me = self.client.credentials().username();
        self.db
            .playlists()
            .unwrap_or_default()
            .into_iter()
            .map(|p| PlaylistSummary {
                smart: is_smart(&p, me),
                id: p.id.into(),
                name: p.name.into(),
                mine: crate::model::owner_is(p.owner.as_deref(), me),
                public: p.public.unwrap_or(false),
                owner: p.owner.unwrap_or_default().into(),
                readonly: p.readonly.unwrap_or(false),
            })
            .collect()
    }

    fn album_artwork(&self, album_id: &str) -> Option<Vec<u8>> {
        self.cover_bytes(album_id, None)
    }

    fn album_thumbnail(&self, id: &str, px: u32) -> Option<Vec<u8>> {
        self.cover_bytes(id, Some(cover_size(px)))
    }

    fn artwork_is_placeholder(&self, id: &str) -> bool {
        self.placeholder_covers.lock().unwrap().contains(id)
    }

    fn animated_cover(&self, key: &str, px: u32) -> Option<navia_core::artwork::AnimatedArt> {
        // other `prefix:` keys are art Navia draws itself (Liked Songs, mixes, genre fans, avatars).
        let cover = match key.strip_prefix("pl:") {
            Some(id) => self.db.playlist_header(id).ok().flatten()?.cover_art.filter(|c| !c.is_empty())?,
            None if key.contains(':') => return None,
            None => self.cover_id(key),
        };
        self.rt.block_on(self.artwork.animated_artwork(&cover, cover_size(px))).ok().flatten()
    }

    fn playlist_cover(&self, id: &str, px: u32) -> PlaylistCover {
        let Some(playlist) = self.db.playlist_header(id).ok().flatten() else { return PlaylistCover::Empty };
        // Uses only the cover id from the playlist list and `getCoverArt`. Never `getPlaylist`, which
        // would re-evaluate a smart playlist and change its `changed` stamp for every user.
        if let Some(cover) = playlist.cover_art.as_deref().filter(|c| !c.is_empty())
            && let Some(bytes) = self.cover_bytes(cover, Some(cover_size(px)))
        {
            return PlaylistCover::Server(bytes);
        }
        // Songs are only stored for playlists this user can edit or has opened, so an unread playlist
        // stays plain instead of being fetched for its art.
        let songs = self.db.playlist(id).ok().flatten().map(|p| p.entry).unwrap_or_default();
        let albums = crate::art::first_distinct(songs.into_iter().filter_map(|s| s.album_id).filter(|a| !a.is_empty()).map(SharedString::from));
        if albums.is_empty() { PlaylistCover::Empty } else { PlaylistCover::Albums(albums) }
    }

    fn albums(&self) -> Arc<Vec<Album>> {
        self.catalog().albums.clone()
    }

    fn artists(&self) -> Arc<Vec<Artist>> {
        self.catalog().artists.clone()
    }

    fn genres(&self) -> Arc<Vec<Genre>> {
        self.genres.lock().unwrap().clone()
    }

    fn radio_stations(&self) -> Arc<Vec<RadioStation>> {
        self.stations.lock().unwrap().clone()
    }

    fn can_share(&self) -> bool {
        self.can_share.load(Ordering::Relaxed)
    }

    fn playlist_details(&self) -> Arc<Vec<PlaylistDetails>> {
        let me = self.client.credentials().username().to_string();
        let admin = *self.admin.lock().unwrap();
        let rules = self.remote.lock().unwrap().rules.clone();
        Arc::new(
            self.db
                .playlists()
                .unwrap_or_default()
                .into_iter()
                .map(|p| {
                    let id = p.id.clone();
                    // Admins can edit other people's playlists even though the server's `readonly` says otherwise.
                    // Smart playlists stay read-only. An unknown owner counts as yours for sections, not for editing.
                    let mine = crate::model::owner_is(p.owner.as_deref(), &me);
                    let owned = p.owner.as_deref().is_some_and(|o| crate::model::same_user(o, &me)) || admin == Some(true);
                    let smart = is_smart(&p, &me);
                    PlaylistDetails {
                        id: p.id.into(),
                        name: p.name.into(),
                        song_count: p.song_count.unwrap_or(0).max(0) as u32,
                        duration_s: p.duration.unwrap_or(0).max(0) as u32,
                        description: p.comment.unwrap_or_default().into(),
                        smart,
                        public: p.public.unwrap_or(false),
                        owned,
                        mine,
                        owner: p.owner.unwrap_or_default().into(),
                        rules: rules.get(&id).cloned(),
                        // Like `order_editable` in the core, a server that doesn't say counts as read-only.
                        readonly: p.valid_until.is_some() || p.readonly != Some(false),
                    }
                })
                .collect(),
        )
    }

    fn album_tracks(&self, album_id: &str) -> Vec<AlbumTrack> {
        self.db
            .album_rows(album_id)
            .unwrap_or_default()
            .into_iter()
            .map(|(rowid, s)| AlbumTrack {
                song: rowid,
                number: s.track.map(|n| n.max(0) as u32),
                disc: s.disc_number.unwrap_or(1).max(0) as u32,
                hi_res: s.bit_depth.unwrap_or(0) > 16 || s.sampling_rate.unwrap_or(0) > 48_000,
            })
            .collect()
    }

    fn artist_albums(&self, artist_id: &str) -> Vec<u32> {
        self.catalog().artist_albums.get(artist_id).cloned().unwrap_or_default()
    }

    fn genre_page(&self) -> Arc<GenrePage> {
        self.catalog().genre_page.clone()
    }

    fn home_taste(&self) -> HomeTaste {
        let catalog = self.catalog();
        let Some(t) = catalog.taste.clone() else { return HomeTaste::default() };
        let artist = |id: &str| catalog.artist_index.get(id).copied();
        let day = taste::local_day(taste::now_unix());
        let because = self.because(day).and_then(|(seed, ids)| {
            let list: Vec<u32> = ids.iter().filter_map(|id| artist(id)).collect();
            artist(&seed).map(|s| (s, list))
        });
        HomeTaste {
            artists: t.artists.iter().filter_map(|a| artist(&a.id)).collect(),
            albums: t.albums.iter().filter_map(|a| catalog.album_index.get(&a.id).copied()).collect(),
            world_of: t.world_of(self.db.home_turn(day).unwrap_or(day)).and_then(|a| artist(&a.id)),
            on_repeat: t.on_repeat.clone(),
            back_in_rotation: catalog.rotation.clone(),
            because,
            stations: self
                .stations(day)
                .iter()
                .filter_map(|(seed, list)| Some((artist(seed)?, list.iter().filter_map(|id| artist(id)).collect())))
                .collect(),
            essentials: self.essentials(day).iter().filter_map(|id| artist(id)).collect(),
        }
    }

    fn artist_songs(&self, artist_id: &str) -> Vec<u32> {
        self.db.artist_album_rowids(artist_id).unwrap_or_default()
    }

    /// For You from the core, worked out in the background. The first visit each day asks the server
    /// for the day's mixes and similar artists, read-only, and the page fills in when it lands. Until
    /// then the last page shows, or nothing.
    fn for_you(&self) -> Arc<ForYouPage> {
        let catalog = self.catalog();
        let day = taste::local_day(taste::now_unix());
        let stamp = Arc::as_ptr(&catalog) as usize;
        let (last, rev) = {
            let remote = self.remote.lock().unwrap();
            (remote.for_you.clone(), remote.for_you_rev)
        };
        if let Some((d, s, r, page)) = &last
            && *d == day
            && *s == stamp
            && *r == rev
        {
            return page.clone();
        }
        let (db, remote) = (self.db.clone(), self.remote.clone());
        self.fetch_remote(format!("for_you:{day}:{stamp}:{rev}"), move || async move {
            let page = match db.for_you(day) {
                Ok(page) => for_you_page(&db, &catalog, page),
                Err(e) => {
                    eprintln!("navia: For You: {e}");
                    ForYouPage::default()
                }
            };
            remote.lock().unwrap().for_you = Some((day, stamp, rev, Arc::new(page)));
        });
        last.map(|(_, _, _, page)| page).unwrap_or_else(|| Arc::new(ForYouPage { loading: true, ..Default::default() }))
    }

    fn playlist_entry_count(&self, playlist_id: &str) -> usize {
        self.db.playlist(playlist_id).ok().flatten().map_or(0, |p| p.entry.len())
    }

    fn stored_playlist_songs(&self, playlist_id: &str) -> Vec<u32> {
        let Some(playlist) = self.db.playlist(playlist_id).ok().flatten() else { return Vec::new() };
        let ids: Vec<String> = playlist.entry.into_iter().map(|s| s.id).collect();
        self.db.song_rowids(&ids).unwrap_or_default()
    }

    fn playlist_songs(&self, playlist_id: &str) -> Vec<u32> {
        // A real run fetches the songs once when a playlist is opened. Test and read-only runs never do,
        // because fetching a smart playlist changes its stamp on the server. The exception is the
        // playlist a test created itself and named in `NAVIA_TEST_PLAYLIST`.
        let driven = std::env::var_os("NAVIA_SCRIPT").is_some() || std::env::var_os("NAVIA_SCROLL_BENCH").is_some();
        let no_fetch = (driven || read_only_run()) && !named_test_playlist(playlist_id);
        let opening = !no_fetch && self.opened_playlists.lock().unwrap().insert(format!("rules:{playlist_id}"));
        if opening {
            let smart = self.db.playlists().ok().and_then(|ps| ps.into_iter().find(|p| p.id == playlist_id)).is_some_and(|p| p.valid_until.is_some());
            if smart {
                let (client, remote, id) = (self.client.clone(), self.remote.clone(), playlist_id.to_string());
                self.fetch_remote(format!("rules:{playlist_id}"), move || async move {
                    if let Ok(Some(rules)) = client.playlist_rules(&id).await {
                        remote.lock().unwrap().rules.insert(id, rules);
                    }
                });
            }
        }
        if !no_fetch && self.opened_playlists.lock().unwrap().insert(playlist_id.to_string()) {
            let (client, db, id) = (self.client.clone(), self.db.clone(), playlist_id.to_string());
            self.fetch_remote(format!("playlist:{playlist_id}"), move || async move {
                let syncer = Syncer::new(client, db, SyncOptions::default());
                if let Err(e) = syncer.refresh_playlist(&id).await {
                    eprintln!("navia: couldn't fetch a playlist: {e}");
                }
            });
        }
        self.stored_playlist_songs(playlist_id)
    }

    fn album_description(&self, album_id: &str) -> SharedString {
        if let Some(text) = self.remote.lock().unwrap().descriptions.get(album_id) {
            return text.clone();
        }
        if !external_allowed() {
            return SharedString::default();
        }
        let (client, remote, id) = (self.client.clone(), self.remote.clone(), album_id.to_string());
        self.fetch_remote(format!("album:{album_id}"), move || async move {
            let notes = client.album_info2(&id).await.ok().and_then(|i| i.notes).unwrap_or_default();
            remote.lock().unwrap().descriptions.insert(id, plain_text(&notes).into());
        });
        SharedString::default()
    }

    fn artist_top_songs(&self, artist_id: &str) -> Vec<u32> {
        if let Some(found) = self.remote.lock().unwrap().top.get(artist_id) {
            return found.clone();
        }
        let Some(name) = self.artist_name(artist_id) else { return Vec::new() };
        if !external_allowed() {
            return Vec::new();
        }
        let (client, db, remote, id, by_id) = (self.client.clone(), self.db.clone(), self.remote.clone(), artist_id.to_string(), self.top_by_id());
        self.fetch_remote(format!("top:{artist_id}"), move || async move {
            let day = taste::local_day(taste::now_unix());
            let ids = navia_core::library::essentials::top_songs(&client, &db, &id, &name, by_id, day).await.unwrap_or_default();
            let rows = db.top_song_rows(&ids).unwrap_or_default();
            remote.lock().unwrap().top.insert(id, rows);
        });
        Vec::new()
    }

    fn artist_discography(&self, artist_id: &str) -> Option<Discography> {
        let (answer, wanted) = {
            let remote = self.remote.lock().unwrap();
            match remote.discography.get(artist_id) {
                Some(found) => (Some(found.clone()), false),
                None => (remote.discography_stale.get(artist_id).cloned(), true),
            }
        };
        if wanted {
            let (client, db, remote, id) = (self.client.clone(), self.db.clone(), self.remote.clone(), artist_id.to_string());
            let use_server = discography::server_list_allowed(read_only_run());
            self.fetch_remote(format!("discography:{artist_id}"), move || async move {
                match discography::artist_discography(&client, &db, &id, use_server).await {
                    Ok(found) => {
                        let sections = found.sections.into_iter().map(|(s, albums)| (s, albums.into_iter().map(|a| a.id).collect())).collect();
                        let mut remote = remote.lock().unwrap();
                        remote.discography_stale.remove(&id);
                        remote.discography.insert(id, sections);
                    }
                    Err(e) => {
                        // Store an answer, old or empty, so the page doesn't ask again on every redraw.
                        eprintln!("navia: couldn't list an artist's albums: {e}");
                        let mut remote = remote.lock().unwrap();
                        let kept = remote.discography_stale.remove(&id).unwrap_or_default();
                        remote.discography.insert(id, kept);
                    }
                }
            });
        }
        let catalog = self.catalog();
        answer.map(|sections| {
            sections
                .into_iter()
                .map(|(s, ids)| (s, ids.iter().filter_map(|id| catalog.album_index.get(id).copied()).collect::<Vec<u32>>()))
                .filter(|(_, list)| !list.is_empty())
                .collect()
        })
    }

    fn similar_known(&self, artist_id: &str) -> Option<Vec<u32>> {
        self.remote.lock().unwrap().similar.get(artist_id).cloned()
    }

    fn similar_songs(&self, artist_id: &str) -> Vec<u32> {
        if let Some(found) = self.remote.lock().unwrap().similar.get(artist_id) {
            return found.clone();
        }
        if !external_allowed() {
            return Vec::new();
        }
        let (client, db, remote, id) = (self.client.clone(), self.db.clone(), self.remote.clone(), artist_id.to_string());
        self.fetch_remote(format!("similar:{artist_id}"), move || async move {
            let found = client.similar_songs2(&id, 30).await.unwrap_or_default();
            let ids: Vec<String> = found.into_iter().map(|s| s.id).collect();
            let rows = db.song_rowids(&ids).unwrap_or_default();
            remote.lock().unwrap().similar.insert(id, rows);
        });
        Vec::new()
    }

    fn artist_info(&self, artist_id: &str) -> ArtistInfo {
        if let Some(found) = self.remote.lock().unwrap().info.get(artist_id) {
            return found.clone();
        }
        if external_allowed() {
            self.fetch_artist_page(artist_id);
        }
        ArtistInfo { biography: SharedString::default(), similar: Vec::new() }
    }

    fn upcoming_for_artist(&self, artist_id: &str) -> Vec<Upcoming> {
        let (found, generation) = {
            let remote = self.remote.lock().unwrap();
            (remote.upcoming.artists.get(artist_id).cloned(), remote.upcoming.generation)
        };
        if found.as_ref().is_none_or(|(g, _)| *g != generation) {
            let (client, db, remote, id, ask, barcode) = (self.client.clone(), self.db.clone(), self.remote.clone(), artist_id.to_string(), external_allowed(), !read_only_run());
            self.fetch_remote(format!("upcoming-artist:{artist_id}"), move || async move {
                let found = navia_core::upcoming::for_artist(barcode.then_some(&client), &db, navia_core::deezer::Deezer::shared(), &id, ask).await;
                remote.lock().unwrap().upcoming.artists.insert(id, (generation, found.into_iter().map(upcoming).collect()));
            });
        }
        found.map(|(_, list)| list).unwrap_or_default()
    }

    fn upcoming_home(&self) -> Vec<Upcoming> {
        let (found, generation) = {
            let remote = self.remote.lock().unwrap();
            (remote.upcoming.home.clone(), remote.upcoming.generation)
        };
        if found.as_ref().is_none_or(|(g, _)| *g != generation) {
            let (client, db, remote, ask, barcode) = (self.client.clone(), self.db.clone(), self.remote.clone(), external_allowed(), !read_only_run());
            self.fetch_remote("upcoming-home".to_string(), move || async move {
                let found = navia_core::upcoming::for_top_artists(barcode.then_some(&client), &db, navia_core::deezer::Deezer::shared(), ask).await;
                remote.lock().unwrap().upcoming.home = Some((generation, found.into_iter().map(upcoming).collect()));
            });
        }
        found.map(|(_, list)| list).unwrap_or_default()
    }

    fn upcoming_matching(&self, query: &str) -> Vec<Upcoming> {
        navia_core::upcoming::kept_matching(&self.db, query).into_iter().map(upcoming).collect()
    }

    fn upcoming_page(&self, id: u64) -> UpcomingView {
        let (found, generation) = {
            let remote = self.remote.lock().unwrap();
            (remote.upcoming.pages.get(&id).cloned(), remote.upcoming.generation)
        };
        if found.as_ref().is_none_or(|(g, _)| *g != generation) {
            let (client, db, remote, ask, barcode) = (self.client.clone(), self.db.clone(), self.remote.clone(), external_allowed(), !read_only_run());
            self.fetch_remote(format!("upcoming-page:{id}"), move || async move {
                let view = match navia_core::upcoming::page(barcode.then_some(&client), &db, navia_core::deezer::Deezer::shared(), id, ask).await {
                    Some(navia_core::upcoming::UpcomingPage::Upcoming { album, tracks }) => {
                        let tracks = tracks
                            .into_iter()
                            .map(|p| UpcomingTrack { title: p.track.title.into(), artist: p.track.artist.into(), duration_s: p.track.duration_s as u32, explicit: p.track.explicit, song: p.song.map(|(rowid, _)| rowid) })
                            .collect();
                        UpcomingView::Upcoming(upcoming(album), tracks)
                    }
                    Some(navia_core::upcoming::UpcomingPage::OnServer(album_id)) => UpcomingView::OnServer(album_id.into()),
                    None => UpcomingView::Missing,
                };
                remote.lock().unwrap().upcoming.pages.insert(id, (generation, view));
            });
        }
        found.map_or(UpcomingView::Loading, |(_, view)| view)
    }

    fn similar_artists(&self, artist_id: &str) -> Vec<u32> {
        let found = self.remote.lock().unwrap().similar_artists.get(artist_id).cloned();
        let Some(ids) = found else {
            self.fetch_artist_page(artist_id);
            return Vec::new();
        };
        let catalog = self.catalog();
        ids.iter().filter_map(|id| catalog.artist_index.get(id).copied()).collect()
    }

    fn more_by(&self, album_id: &str) -> Option<MoreBy> {
        let known = self.remote.lock().unwrap().more_by.get(album_id).cloned();
        let found = match known {
            Some(found) => found,
            None => {
                // Local and quick, one album artist's albums. Kept until the catalog is rebuilt.
                let found = self.db.more_by(album_id).map_err(|e| eprintln!("navia: more by: {e}")).ok().flatten();
                self.remote.lock().unwrap().more_by.insert(album_id.to_string(), found.clone());
                found
            }
        }?;
        let catalog = self.catalog();
        let artist = catalog.artist_index.get(&found.artist_id).copied()?;
        let albums: Vec<u32> = found.albums.iter().filter_map(|a| catalog.album_index.get(&a.id).copied()).collect();
        (!albums.is_empty()).then_some(MoreBy { artist, albums, section: found.section })
    }

    fn album_heat(&self, album_id: &str) -> HashMap<u32, u64> {
        if let Some(found) = self.remote.lock().unwrap().heat.get(album_id) {
            return found.clone();
        }
        // what's kept shows at once, while a fresh answer is asked for when it's over a week old.
        let kept = navia_core::album_heat::kept_album_heat(&self.db, album_id).map(|r| Self::heat_rows(&self.db, album_id, &r)).unwrap_or_default();
        if !external_allowed() {
            self.remote.lock().unwrap().heat.insert(album_id.to_string(), kept.clone());
            return kept;
        }
        let (db, remote, id) = (self.db.clone(), self.remote.clone(), album_id.to_string());
        self.fetch_remote(format!("heat:{album_id}"), move || async move {
            let found = navia_core::album_heat::album_heat(&db, navia_core::deezer::Deezer::shared(), &id).await;
            let rows = Self::heat_rows(&db, &id, &found);
            remote.lock().unwrap().heat.insert(id, rows);
        });
        kept
    }

    fn reask_missing_heat(&self, album_id: &str) {
        let mut remote = self.remote.lock().unwrap();
        if remote.heat.get(album_id).is_some_and(HashMap::is_empty) {
            remote.heat.remove(album_id);
        }
    }

    fn lyrics_writers(&self, song_id: &str) -> Vec<String> {
        let Ok(Some(info)) = self.db.song_info(song_id) else { return Vec::new() };
        let names = |role: &str| info.credits.iter().filter(|c| c.role == role).flat_map(|c| c.names.iter().map(|n| n.name.clone())).collect::<Vec<_>>();
        let lyricists = names("lyricist");
        if lyricists.is_empty() { names("composer") } else { lyricists }
    }

    /// fresh cache, else the server, else whatever is cached.
    fn lyrics(&self, song_id: &str) -> Vec<LyricLine> {
        let Some(found) = self.rt.block_on(navia_core::lyrics::lyrics_for(&self.client, Some(&self.lyrics), song_id, None, self.features.get().copied())) else { return Vec::new() };
        let synced = found.synced;
        found.lines.into_iter().map(|l| LyricLine { start_ms: l.start_ms.unwrap_or(0), text: l.text.clone().into(), synced, timed: (!l.words.is_empty()).then(|| Arc::new(l)), backing: None, backing_first: false }).collect()
    }
}

/// Discord art's server lookups. Only the art thread asks, so never in Private mode, with Discord
/// status off or in a test run.
impl navia_integrations::art::ServerLookup for CoreLibrary {
    fn album_image_link(&self, album_id: &str) -> Result<Option<String>, navia_integrations::art::ArtError> {
        let info = self.rt.block_on(self.client.album_info2(album_id)).map_err(|e| navia_integrations::art::ArtError(e.to_string()))?;
        Ok(info.large_image_url.filter(|u| !u.is_empty()))
    }

    fn song_isrc(&self, song_id: &str) -> Result<Option<String>, navia_integrations::art::ArtError> {
        if let Some(isrc) = CoreLibrary::song_isrc(self, song_id) {
            return Ok(Some(isrc));
        }
        let song = self.rt.block_on(self.client.song(song_id)).map_err(|e| navia_integrations::art::ArtError(e.to_string()))?;
        Ok(song.isrc.into_iter().next())
    }
}

pub fn song_order_of(sort: SortKey) -> SongOrder {
    match sort {
        SortKey::MostPlayed => SongOrder::MostPlayed,
        SortKey::Title => SongOrder::Title,
        SortKey::Artist => SongOrder::Artist,
        SortKey::Album => SongOrder::Album,
        SortKey::Year => SongOrder::Year,
        SortKey::Length => SongOrder::Duration,
        SortKey::Added => SongOrder::Added,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stations(remote: &Remote, day: i64) -> Option<(bool, usize)> {
        match remote.taste_answer(remote.stations.as_ref()?, day) {
            TasteAnswer::Current(found) => Some((true, found.len())),
            TasteAnswer::Stale(found) => Some((false, found.len())),
            TasteAnswer::Expired => None,
        }
    }

    /// An answer worked out before the first sync goes stale at the rebuild, and a late one is dropped.
    #[test]
    fn a_catalog_rebuild_makes_taste_rows_stale_and_drops_late_answers() {
        let mut remote = Remote::default();
        let early = remote.taste_generation;
        remote.land_taste(early, |r| r.stations = Some(TasteMemo { day: 7, generation: early, found: Vec::new() }));
        assert_eq!(stations(&remote, 7), Some((true, 0)));

        let in_flight = remote.taste_generation;
        remote.forget_taste();
        assert_eq!(stations(&remote, 7), Some((false, 0)), "shown until worked out again");
        remote.land_taste(in_flight, |r| r.stations = Some(TasteMemo { day: 7, generation: in_flight, found: Vec::new() }));
        assert_eq!(stations(&remote, 7), Some((false, 0)), "a late answer from the old library isn't kept");

        let now = remote.taste_generation;
        let found = vec![("queen".to_string(), vec!["a".to_string(), "b".to_string()])];
        remote.land_taste(now, |r| r.stations = Some(TasteMemo { day: 7, generation: now, found }));
        assert_eq!(stations(&remote, 7), Some((true, 1)));
        assert_eq!(stations(&remote, 8), None, "another day starts over");
    }
}
