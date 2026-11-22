//! Offline downloads of the server's original files, kept in Navia's data folder and recorded in the library.
//! Whole playlists and albums can be kept in sync, see [`crate::library::collections`].
//! Automatic downloads wait while offline or metered. Downloads the user starts always run.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use reqwest::StatusCode;
use reqwest::blocking::Client as BlockingClient;
use reqwest::header::CONTENT_TYPE;

use crate::audio::Decoder;
use crate::client::{Client, StreamOptions};
use crate::error::{Error, Result};
use crate::library::Library;
use crate::library::collections::{CollectionKind, CollectionSongs, SyncedCollection};
use crate::quality::{Codec, Plan, Quality, QualityControl, Route, plan};
use crate::songcache::SongCache;

/// Files fetched at once.
const WORKERS: usize = 2;
/// Most songs waiting to download at one time.
const MAX_QUEUED: usize = 20_000;
const PROGRESS_EVERY: Duration = Duration::from_millis(400);
const CHUNK: usize = 256 * 1024;
/// How often a worker checks again while every queued song waits.
const PAUSED_RECHECK: Duration = Duration::from_secs(1);

/// A song to download. The suffix names the file, and the content decides how it plays.
#[derive(Clone, Debug)]
pub struct DownloadRequest {
    pub id: String,
    pub suffix: Option<String>,
    /// Expected size in bytes, for progress before the server says.
    pub size: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DownloadEvent {
    Progress { id: String, done: u64, total: Option<u64> },
    Finished { id: String, size: u64 },
    Failed { id: String, message: String },
    Removed { id: String },
}

/// A kept playlist or album changed, so read its status again.
/// Song progress within a collection comes as [`DownloadEvent::Progress`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectionEvent {
    pub kind: CollectionKind,
    pub id: String,
}

/// What a kept collection is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollectionPhase {
    /// Every song is on the device.
    Done,
    /// Songs are downloading for the first time.
    Downloading,
    /// It was complete, and songs added since are downloading.
    Syncing,
    /// Songs are waiting because the connection is metered.
    Metered,
    /// Songs are waiting because the server can't be reached.
    Offline,
    /// Songs are missing and nothing is fetching them, because they failed or a playlist's songs aren't fetched yet.
    Waiting,
}

/// A kept collection's switch and progress.
#[derive(Clone, Debug, PartialEq)]
pub struct CollectionStatus {
    pub phase: CollectionPhase,
    /// Its songs on the device, for any reason.
    pub have: u32,
    pub total: u32,
    pub missing: u32,
    /// Missing songs whose last download failed. They're retried on reconnect and on
    /// [`DownloadManager::sync_collections`].
    pub failed: u32,
    /// Progress from 0 to 1, counting partly fetched songs by how far along they are.
    pub progress: f64,
    /// Bytes its songs on the device take.
    pub bytes: u64,
}

/// What turning a collection off would do to the songs it has on the device.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RemovalPlan {
    /// Songs only this collection holds. Remove deletes them and Keep turns them into single downloads.
    pub remove: u32,
    pub remove_bytes: u64,
    /// Songs another kept collection or a single download still holds, which stay either way.
    pub keep: u32,
}

type Listener = Box<dyn Fn(&DownloadEvent) + Send>;
type CollectionListener = Box<dyn Fn(&CollectionEvent) + Send>;

/// Why a song is downloading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Origin {
    /// Asked for on its own.
    Single,
    /// For a kept collection. `manual` is true when the user just turned it on.
    Collection { manual: bool },
}

impl Origin {
    fn automatic(self) -> bool {
        self == Origin::Collection { manual: false }
    }
}

struct Job {
    request: DownloadRequest,
    origin: Origin,
}

struct Active {
    cancel: Arc<AtomicBool>,
    done: u64,
    total: Option<u64>,
}

#[derive(Default)]
struct State {
    /// Waiting songs in order, with their jobs in `jobs`.
    order: VecDeque<String>,
    jobs: HashMap<String, Job>,
    /// Songs being fetched.
    active: HashMap<String, Active>,
    /// Collection songs being fetched that were also asked for on their own meanwhile.
    wanted_single: HashSet<String>,
    /// Collection songs whose last try failed, left alone until a reconnect or an explicit sync.
    failed: HashSet<String>,
}

impl State {
    fn remove_job(&mut self, id: &str) -> Option<Job> {
        let job = self.jobs.remove(id)?;
        self.order.retain(|o| o != id);
        Some(job)
    }
}

/// Work for the collection thread.
#[derive(Default)]
struct Pending {
    reconcile: bool,
    retry_failed: bool,
    /// Tell the apps about every collection, because pausing or resuming moves all of them.
    announce: bool,
}

struct Inner {
    library: Library,
    client: Client,
    http: BlockingClient,
    dir: PathBuf,
    state: Mutex<State>,
    cond: Condvar,
    listeners: Mutex<Vec<Listener>>,
    collection_listeners: Mutex<Vec<CollectionListener>>,
    stopping: AtomicBool,
    quality: Arc<QualityControl>,
    /// Whether the server can be reached, as the app last said. Starts true.
    online: AtomicBool,
    /// One collection pass at a time.
    reconcile_lock: Mutex<()>,
    pending: Mutex<Pending>,
    pending_cond: Condvar,
}

impl Inner {
    fn emit(&self, event: DownloadEvent) {
        for listener in self.listeners.lock().expect("listeners").iter() {
            listener(&event);
        }
    }

    fn emit_collection(&self, kind: CollectionKind, id: &str) {
        let event = CollectionEvent { kind, id: id.to_string() };
        for listener in self.collection_listeners.lock().expect("listeners").iter() {
            listener(&event);
        }
    }

    fn path_for(&self, id: &str, suffix: &str) -> (String, PathBuf) {
        let hash = format!("{:x}", md5::compute(id.as_bytes()));
        let relative = format!("{}/{}.{}", &hash[..2], hash, suffix);
        let full = self.dir.join(&relative);
        (relative, full)
    }

    fn resolve(&self, relative: &str) -> PathBuf {
        self.dir.join(relative)
    }

    /// Whether automatic downloads wait right now.
    fn automatic_paused(&self) -> bool {
        !self.online.load(Ordering::Acquire) || self.quality.automatic_downloads_paused()
    }

    fn schedule(&self, change: impl FnOnce(&mut Pending)) {
        change(&mut self.pending.lock().expect("pending"));
        self.pending_cond.notify_all();
    }

    fn local_path(&self, id: &str) -> Option<PathBuf> {
        self.library.download_path(id).ok().flatten().map(|relative| self.resolve(&relative)).filter(|p| p.is_file())
    }

    fn cancel(&self, ids: &[String]) {
        let mut state = self.state.lock().expect("download state");
        for id in ids {
            state.remove_job(id);
            if let Some(active) = state.active.get(id) {
                active.cancel.store(true, Ordering::Release);
            }
        }
    }

    /// Deletes these songs' files and records, and says so.
    fn delete_files(&self, ids: &[String]) -> Result<()> {
        self.cancel(ids);
        for id in ids {
            if let Some(relative) = self.library.download_path(id)? {
                // It may already be gone.
                let _ = fs::remove_file(self.resolve(&relative));
                self.library.download_forget(id)?;
                self.emit(DownloadEvent::Removed { id: id.clone() });
            }
        }
        Ok(())
    }

    /// Lets go of songs a collection no longer holds. Songs nothing else holds are canceled and deleted,
    /// unless `keep` turns them into single downloads.
    fn release(&self, songs: &[String], keep: bool) -> Result<()> {
        if songs.is_empty() {
            return Ok(());
        }
        let held = self.library.held_song_ids()?;
        let loose: Vec<String> = songs.iter().filter(|s| !held.contains(*s)).cloned().collect();
        if keep {
            let on_device = self.library.downloaded_among(&loose)?;
            let (kept, rest): (Vec<String>, Vec<String>) = loose.into_iter().partition(|s| on_device.contains_key(s));
            self.library.download_set_single(&kept, true)?;
            self.cancel(&rest);
            Ok(())
        } else {
            self.delete_files(&loose)
        }
    }

    /// Queues songs for kept collections. Returns how many were added.
    fn enqueue_for_collections(&self, wanted: Vec<(String, bool)>, retry_failed: bool) -> usize {
        let mut added = 0;
        {
            let mut state = self.state.lock().expect("download state");
            for (id, manual) in wanted {
                if state.active.contains_key(&id) {
                    continue;
                }
                if let Some(job) = state.jobs.get_mut(&id) {
                    if manual && job.origin.automatic() {
                        job.origin = Origin::Collection { manual: true };
                    }
                    continue;
                }
                if (!retry_failed && !manual && state.failed.contains(&id)) || state.order.len() >= MAX_QUEUED {
                    continue;
                }
                state.failed.remove(&id);
                let song = self.library.song(&id).ok().flatten();
                let request = DownloadRequest { id: id.clone(), suffix: song.as_ref().and_then(|s| s.suffix.clone()), size: song.and_then(|s| s.size).map(|s| s as u64) };
                state.order.push_back(id.clone());
                state.jobs.insert(id, Job { request, origin: Origin::Collection { manual } });
                added += 1;
            }
        }
        self.cond.notify_all();
        added
    }

    /// Brings every kept collection in step with the library, queuing missing songs and releasing ones that left.
    /// A collection that's gone from the library is turned off.
    fn reconcile(&self, manual_for: Option<(CollectionKind, &str)>, retry_failed: bool) -> Result<()> {
        let _pass = self.reconcile_lock.lock().expect("reconcile");
        if retry_failed {
            self.state.lock().expect("download state").failed.clear();
        }
        let collections = self.library.synced_collections()?;
        let mut released = Vec::new();
        let mut changed: Vec<(CollectionKind, String)> = Vec::new();
        let mut wanted = Vec::new();
        let mut waiting: Vec<(CollectionKind, String)> = Vec::new();
        // Oldest first, so the first collection turned on downloads first.
        for c in collections.iter().rev() {
            let songs = match self.library.collection_songs(c.kind, &c.id)? {
                CollectionSongs::Gone => {
                    released.extend(self.library.remove_synced_collection(c.kind, &c.id)?);
                    changed.push((c.kind, c.id.clone()));
                    continue;
                }
                // A playlist not fetched yet keeps what it held until its songs are known.
                CollectionSongs::Unknown { .. } => continue,
                CollectionSongs::Known(songs) => songs,
            };
            let old: HashSet<String> = self.library.collection_members(c.kind, &c.id)?.into_iter().collect();
            let new: HashSet<&String> = songs.iter().collect();
            if old.len() != new.len() || old.iter().any(|s| !new.contains(s)) {
                released.extend(old.iter().filter(|s| !new.contains(s)).cloned());
                self.library.set_collection_members(c.kind, &c.id, &songs)?;
                changed.push((c.kind, c.id.clone()));
            }
            let on_device = self.library.downloaded_among(&songs)?;
            let manual = manual_for.is_some_and(|(kind, id)| kind == c.kind && id == c.id);
            let missing: Vec<(String, bool)> = songs.into_iter().filter(|s| !on_device.contains_key(s)).map(|s| (s, manual)).collect();
            if missing.is_empty() {
                if !c.complete {
                    self.library.set_collection_complete(c.kind, &c.id, true)?;
                    changed.push((c.kind, c.id.clone()));
                }
            } else {
                waiting.push((c.kind, c.id.clone()));
                wanted.extend(missing);
            }
        }
        released.sort();
        released.dedup();
        self.release(&released, false)?;
        if self.enqueue_for_collections(wanted, retry_failed) > 0 {
            changed.extend(waiting);
        }
        changed.sort();
        changed.dedup();
        for (kind, id) in changed {
            self.emit_collection(kind, &id);
        }
        Ok(())
    }

    /// After a song finished or failed, marks the collections it completes and tells the apps.
    fn after_song(&self, id: &str) {
        let Ok(holding) = self.library.collections_holding(id) else { return };
        for (kind, cid) in holding {
            if let (Ok(Some(record)), Ok(CollectionSongs::Known(songs))) = (self.library.synced_collection(kind, &cid), self.library.collection_songs(kind, &cid))
                && !record.complete
                && self.library.downloaded_among(&songs).is_ok_and(|have| have.len() == songs.len())
            {
                let _ = self.library.set_collection_complete(kind, &cid, true);
            }
            self.emit_collection(kind, &cid);
        }
    }
}

/// Runs downloads on background threads. Cheap to share.
pub struct DownloadManager {
    inner: Arc<Inner>,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

impl DownloadManager {
    /// Files go under `dir`, and the song cache goes in `song-cache` beside it.
    /// Stale partial files and records of missing files are cleaned first.
    pub fn new(library: Library, client: Client, dir: PathBuf) -> Result<Self> {
        let cache_dir = dir.with_file_name("song-cache");
        Self::with_song_cache(library, client, dir, Some(cache_dir))
    }

    /// Like [`Self::new`], with the song cache at the given path, or none.
    pub fn with_song_cache(library: Library, client: Client, dir: PathBuf, song_cache: Option<PathBuf>) -> Result<Self> {
        fs::create_dir_all(&dir)?;
        remove_partials(&dir);
        // Older builds kept finished transcodes here, which the song cache holds instead.
        let legacy = dir.join("transcode-cache");
        if legacy.is_dir() {
            let _ = fs::remove_dir_all(legacy);
        }
        for (id, relative, _) in library.downloads()? {
            if !dir.join(&relative).is_file() {
                library.download_forget(&id)?;
            }
        }
        let http = BlockingClient::builder().user_agent(crate::client::user_agent()).connect_timeout(Duration::from_secs(10)).timeout(Duration::from_secs(120)).build()?;
        let quality = QualityControl::new(library.clone(), client.clone(), song_cache)?;
        let inner = Arc::new(Inner {
            quality,
            library,
            client,
            http,
            dir,
            state: Mutex::new(State::default()),
            cond: Condvar::new(),
            listeners: Mutex::new(Vec::new()),
            collection_listeners: Mutex::new(Vec::new()),
            stopping: AtomicBool::new(false),
            online: AtomicBool::new(true),
            reconcile_lock: Mutex::new(()),
            // Collections left with missing songs last time are queued again at once.
            pending: Mutex::new(Pending { reconcile: true, ..Pending::default() }),
            pending_cond: Condvar::new(),
        });
        let weak = Arc::downgrade(&inner);
        inner.library.on_contents_changed(move || {
            if let Some(inner) = weak.upgrade() {
                inner.schedule(|p| p.reconcile = true);
            }
        });
        let weak = Arc::downgrade(&inner);
        inner.quality.watch_metered(move || {
            if let Some(inner) = weak.upgrade() {
                inner.cond.notify_all();
                inner.schedule(|p| p.announce = true);
            }
        });
        let mut workers = Vec::new();
        for n in 0..WORKERS {
            let worker = inner.clone();
            workers.push(thread::Builder::new().name(format!("navia-download-{n}")).spawn(move || run_worker(worker)).map_err(Error::Io)?);
        }
        let weak = Arc::downgrade(&inner);
        workers.push(thread::Builder::new().name("navia-collections".to_string()).spawn(move || run_collections(weak)).map_err(Error::Io)?);
        Ok(Self { inner, workers: Mutex::new(workers) })
    }

    pub fn add_listener(&self, listener: impl Fn(&DownloadEvent) + Send + 'static) {
        self.inner.listeners.lock().expect("listeners").push(Box::new(listener));
    }

    /// Calls `listener` when a kept playlist or album changes, on a background thread.
    pub fn add_collection_listener(&self, listener: impl Fn(&CollectionEvent) + Send + 'static) {
        self.inner.collection_listeners.lock().expect("listeners").push(Box::new(listener));
    }

    /// Queues songs the user asked for on their own, skipping ones already downloaded, queued or running.
    /// Returns how many were added, since the queue is capped.
    pub fn enqueue(&self, requests: Vec<DownloadRequest>) -> usize {
        let mut added = 0;
        let mut now_single = Vec::new();
        {
            let mut state = self.inner.state.lock().expect("download state");
            for request in requests {
                if state.active.contains_key(&request.id) {
                    state.wanted_single.insert(request.id);
                    continue;
                }
                if let Some(job) = state.jobs.get_mut(&request.id) {
                    job.origin = Origin::Single;
                    continue;
                }
                if self.local_path(&request.id).is_some() {
                    now_single.push(request.id);
                    continue;
                }
                if state.order.len() >= MAX_QUEUED {
                    break;
                }
                state.order.push_back(request.id.clone());
                state.jobs.insert(request.id.clone(), Job { request, origin: Origin::Single });
                added += 1;
            }
        }
        let _ = self.inner.library.download_set_single(&now_single, true);
        self.inner.cond.notify_all();
        added
    }

    /// Quality settings for this account: streaming, metered streaming and downloads.
    pub fn quality_control(&self) -> Arc<QualityControl> {
        self.inner.quality.clone()
    }

    /// Songs kept as they were streamed, for this account.
    pub fn song_cache(&self) -> Option<Arc<SongCache>> {
        self.inner.quality.cache().cloned()
    }

    /// The format and rate a song was downloaded as, or `None` for the original file.
    pub fn delivered_quality(&self, id: &str) -> Option<(Codec, u32)> {
        let (format, kbps) = self.inner.library.download_quality(id).ok().flatten()?;
        Some((Codec::from_id(&format)?, kbps))
    }

    /// The downloaded file for a song, if it's on disk.
    pub fn local_path(&self, id: &str) -> Option<PathBuf> {
        self.inner.local_path(id)
    }

    /// Stops these songs now. Running ones stop within one read and leave nothing behind.
    pub fn cancel(&self, ids: &[String]) {
        self.inner.cancel(ids);
    }

    /// Cancels everything queued and running.
    pub fn cancel_all(&self) {
        let mut state = self.inner.state.lock().expect("download state");
        state.order.clear();
        state.jobs.clear();
        for active in state.active.values() {
            active.cancel.store(true, Ordering::Release);
        }
    }

    /// Removes single downloads, canceling running ones and deleting files.
    /// A song a kept collection holds stays on the device but stops counting as a single download.
    pub fn remove(&self, ids: &[String]) -> Result<()> {
        let mut gone = Vec::new();
        let mut held = Vec::new();
        for id in ids {
            if self.inner.library.collections_holding(id)?.is_empty() { gone.push(id.clone()) } else { held.push(id.clone()) }
        }
        {
            let mut state = self.inner.state.lock().expect("download state");
            for id in &held {
                state.wanted_single.remove(id);
                if let Some(job) = state.jobs.get_mut(id)
                    && job.origin == Origin::Single
                {
                    job.origin = Origin::Collection { manual: false };
                }
            }
        }
        self.inner.library.download_set_single(&held, false)?;
        self.inner.delete_files(&gone)
    }

    /// Turns every download off, turning off kept collections and deleting everything.
    pub fn clear_all(&self) -> Result<()> {
        let _pass = self.inner.reconcile_lock.lock().expect("reconcile");
        let collections = self.inner.library.synced_collections()?;
        self.inner.library.clear_synced_collections()?;
        self.cancel_all();
        self.inner.delete_files(&self.downloaded_ids()?)?;
        for c in collections {
            self.inner.emit_collection(c.kind, &c.id);
        }
        Ok(())
    }

    /// Ids of every downloaded song.
    pub fn downloaded_ids(&self) -> Result<Vec<String>> {
        Ok(self.inner.library.downloads()?.into_iter().map(|(id, _, _)| id).collect())
    }

    /// Ids of songs downloaded on their own, for the Downloads page.
    pub fn single_download_ids(&self) -> Result<Vec<String>> {
        self.inner.library.single_download_ids()
    }

    /// Whether a song was downloaded on its own.
    pub fn is_single_download(&self, id: &str) -> bool {
        self.inner.library.is_single_download(id).unwrap_or(false)
    }

    /// Whether a song is on the device or on its way, through a kept collection or on its own.
    pub fn is_held(&self, id: &str) -> bool {
        self.inner.library.is_song_held(id).unwrap_or(false)
    }

    /// Ids waiting or being fetched.
    pub fn in_progress(&self) -> Vec<String> {
        let state = self.inner.state.lock().expect("download state");
        state.active.keys().cloned().chain(state.order.iter().cloned()).collect()
    }

    /// Every finished download with its size on disk, as (song id, bytes).
    pub fn entries(&self) -> Result<Vec<(String, u64)>> {
        Ok(self.inner.library.downloads()?.into_iter().map(|(id, relative, recorded)| {
            let size = fs::metadata(self.inner.resolve(&relative)).map_or(recorded, |m| m.len());
            (id, size)
        }).collect())
    }

    /// Total bytes the finished downloads take.
    pub fn total_size(&self) -> Result<u64> {
        Ok(self.inner.library.downloads()?.iter().map(|(_, _, size)| *size).sum())
    }

    // Kept playlists and albums.

    /// Turns a collection's Download switch on and starts its songs at once, even on a metered connection.
    /// A playlist whose songs aren't stored yet waits until [`Library::collection_needs_fetch`] is satisfied.
    pub fn collection_download_on(&self, kind: CollectionKind, id: &str) -> Result<()> {
        let added = self.inner.library.add_synced_collection(kind, id)?;
        self.inner.reconcile(Some((kind, id)), false)?;
        if added {
            self.inner.emit_collection(kind, id);
        }
        Ok(())
    }

    /// Turns a collection's Download switch off. Waiting songs are canceled.
    /// With `keep_songs` its songs stay as single downloads, otherwise the ones nothing else holds are deleted.
    pub fn collection_download_off(&self, kind: CollectionKind, id: &str, keep_songs: bool) -> Result<()> {
        let _pass = self.inner.reconcile_lock.lock().expect("reconcile");
        if self.inner.library.synced_collection(kind, id)?.is_none() {
            return Ok(());
        }
        let members = self.inner.library.remove_synced_collection(kind, id)?;
        self.inner.release(&members, keep_songs)?;
        self.inner.emit_collection(kind, id);
        Ok(())
    }

    /// What turning a collection off would do, for the Keep Songs / Remove dialog.
    pub fn removal_plan(&self, kind: CollectionKind, id: &str) -> Result<RemovalPlan> {
        let members = self.inner.library.collection_members(kind, id)?;
        let on_device = self.inner.library.downloaded_among(&members)?;
        let others = self.inner.library.songs_held_by_others(kind, id)?;
        let mut plan = RemovalPlan::default();
        for (song, bytes) in on_device {
            if others.contains(&song) || self.inner.library.is_single_download(&song)? {
                plan.keep += 1;
            } else {
                plan.remove += 1;
                plan.remove_bytes += bytes;
            }
        }
        Ok(plan)
    }

    /// Kept collections, the most recently turned on first.
    pub fn synced_collections(&self) -> Result<Vec<SyncedCollection>> {
        self.inner.library.synced_collections()
    }

    /// A collection's songs on the device, in its order, for the Downloads page.
    pub fn collection_songs_on_device(&self, kind: CollectionKind, id: &str) -> Result<Vec<String>> {
        let songs = match self.inner.library.collection_songs(kind, id)? {
            CollectionSongs::Known(songs) => songs,
            _ => self.inner.library.collection_members(kind, id)?,
        };
        let on_device = self.inner.library.downloaded_among(&songs)?;
        Ok(songs.into_iter().filter(|s| on_device.contains_key(s)).collect())
    }

    /// A collection's switch and progress, or `None` when its switch is off.
    pub fn collection_status(&self, kind: CollectionKind, id: &str) -> Result<Option<CollectionStatus>> {
        let Some(record) = self.inner.library.synced_collection(kind, id)? else { return Ok(None) };
        let (songs, total) = match self.inner.library.collection_songs(kind, id)? {
            CollectionSongs::Known(songs) => {
                let n = songs.len() as u32;
                (songs, n)
            }
            CollectionSongs::Unknown { expected } => (self.inner.library.collection_members(kind, id)?, expected),
            CollectionSongs::Gone => {
                let members = self.inner.library.collection_members(kind, id)?;
                let n = members.len() as u32;
                (members, n)
            }
        };
        let on_device = self.inner.library.downloaded_among(&songs)?;
        let have = on_device.len() as u32;
        let bytes = on_device.values().sum();
        let missing = total.saturating_sub(have);
        let (mut failed, mut running, mut queued, mut queued_auto, mut partial) = (0u32, false, false, false, 0f64);
        {
            let state = self.inner.state.lock().expect("download state");
            for song in songs.iter().filter(|s| !on_device.contains_key(*s)) {
                if let Some(active) = state.active.get(song) {
                    running = true;
                    partial += active.total.filter(|t| *t > 0).map_or(0.0, |t| (active.done as f64 / t as f64).min(1.0));
                } else if let Some(job) = state.jobs.get(song) {
                    queued = true;
                    queued_auto |= job.origin.automatic();
                } else if state.failed.contains(song) {
                    failed += 1;
                }
            }
        }
        let paused = self.inner.automatic_paused();
        let fetching = if record.complete { CollectionPhase::Syncing } else { CollectionPhase::Downloading };
        let phase = if missing == 0 {
            CollectionPhase::Done
        } else if !self.inner.online.load(Ordering::Acquire) {
            CollectionPhase::Offline
        } else if running {
            fetching
        } else if queued && paused && queued_auto {
            CollectionPhase::Metered
        } else if queued {
            fetching
        } else {
            CollectionPhase::Waiting
        };
        let progress = if total == 0 { 1.0 } else { ((f64::from(have) + partial) / f64::from(total)).min(1.0) };
        Ok(Some(CollectionStatus { phase, have, total, missing, failed, progress, bytes }))
    }

    /// Brings every kept collection in step now and retries failed songs. Apps call it for a manual retry.
    pub fn sync_collections(&self) -> Result<()> {
        self.inner.reconcile(None, true)
    }

    /// Sets whether the server can be reached. Automatic downloads wait while it's false.
    /// Coming back online syncs and retries.
    pub fn set_online(&self, online: bool) {
        if self.inner.online.swap(online, Ordering::AcqRel) == online {
            return;
        }
        self.inner.cond.notify_all();
        self.inner.schedule(|p| {
            p.announce = true;
            if online {
                p.reconcile = true;
                p.retry_failed = true;
            }
        });
    }

    /// The system's metered signal, the same as [`QualityControl::set_metered`].
    pub fn set_metered(&self, metered: bool) {
        self.inner.quality.set_metered(metered);
    }

    /// "Download on metered connections", the same as [`QualityControl::download_on_metered`].
    pub fn download_on_metered(&self) -> bool {
        self.inner.quality.download_on_metered()
    }

    pub fn set_download_on_metered(&self, on: bool) -> Result<()> {
        self.inner.quality.set_download_on_metered(on)
    }
}

impl Drop for DownloadManager {
    fn drop(&mut self) {
        self.inner.stopping.store(true, Ordering::Release);
        self.cancel_all();
        self.inner.cond.notify_all();
        self.inner.pending_cond.notify_all();
        for worker in self.workers.lock().expect("workers").drain(..) {
            let _ = worker.join();
        }
    }
}

fn remove_partials(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            remove_partials(&path);
        } else if path.extension().is_some_and(|e| e == "part") {
            let _ = fs::remove_file(path);
        }
    }
}

/// Runs collection passes when asked. Holds only a weak reference between passes, so the manager can drop.
fn run_collections(weak: Weak<Inner>) {
    loop {
        let Some(inner) = weak.upgrade() else { return };
        let work = {
            let mut pending = inner.pending.lock().expect("pending");
            loop {
                if inner.stopping.load(Ordering::Acquire) {
                    return;
                }
                if pending.reconcile || pending.announce {
                    break std::mem::take(&mut *pending);
                }
                pending = inner.pending_cond.wait_timeout(pending, Duration::from_millis(500)).expect("pending").0;
                // Let the manager drop while idle.
                if Arc::strong_count(&inner) == 1 {
                    return;
                }
            }
        };
        if work.reconcile
            && let Err(e) = inner.reconcile(None, work.retry_failed)
        {
            tracing::warn!("keeping downloaded playlists and albums in step failed: {e}");
        }
        if work.announce {
            for c in inner.library.synced_collections().unwrap_or_default() {
                inner.emit_collection(c.kind, &c.id);
            }
        }
    }
}

fn run_worker(inner: Arc<Inner>) {
    loop {
        let (job, cancel) = {
            let mut state = inner.state.lock().expect("download state");
            loop {
                if inner.stopping.load(Ordering::Acquire) {
                    return;
                }
                let paused = inner.automatic_paused();
                let next = state.order.iter().position(|id| state.jobs.get(id).is_some_and(|j| !(paused && j.origin.automatic())));
                if let Some(at) = next {
                    let id = state.order.remove(at).expect("queued id");
                    let job = state.jobs.remove(&id).expect("queued job");
                    let cancel = Arc::new(AtomicBool::new(false));
                    state.active.insert(id, Active { cancel: cancel.clone(), done: 0, total: job.request.size });
                    break (job, cancel);
                }
                state = if state.order.is_empty() { inner.cond.wait(state).expect("download state") } else { inner.cond.wait_timeout(state, PAUSED_RECHECK).expect("download state").0 };
            }
        };
        let id = job.request.id.clone();
        let outcome = fetch_with_retry(&inner, &job.request, &cancel, job.origin == Origin::Single);
        let wanted_single = {
            let mut state = inner.state.lock().expect("download state");
            state.active.remove(&id);
            let wanted = state.wanted_single.remove(&id);
            if matches!(outcome, Err(ref e) if !matches!(e, Error::Cancelled)) && job.origin != Origin::Single {
                state.failed.insert(id.clone());
            }
            wanted
        };
        match outcome {
            Ok(size) => {
                // Downloads always play first, so a cached copy would only take space twice.
                if let Some(cache) = inner.quality.cache() {
                    cache.forget_song(&id);
                }
                if wanted_single {
                    let _ = inner.library.download_set_single(std::slice::from_ref(&id), true);
                }
                // Its collection may have let it go while it was downloading.
                if job.origin != Origin::Single && !wanted_single && !inner.library.is_song_held(&id).unwrap_or(true) {
                    let _ = inner.delete_files(std::slice::from_ref(&id));
                    continue;
                }
                inner.emit(DownloadEvent::Finished { id: id.clone(), size });
                inner.after_song(&id);
            }
            Err(Error::Cancelled) => {}
            Err(e) => {
                inner.emit(DownloadEvent::Failed { id: id.clone(), message: e.to_string() });
                inner.after_song(&id);
            }
        }
    }
}

fn fetch_with_retry(inner: &Inner, request: &DownloadRequest, cancel: &AtomicBool, single: bool) -> Result<u64> {
    let mut attempts = 0;
    loop {
        attempts += 1;
        match fetch(inner, request, cancel, single) {
            Err(e) if attempts < 3 && e.is_transient() && !cancel.load(Ordering::Acquire) => thread::sleep(Duration::from_secs(attempts)),
            other => return other,
        }
    }
}

/// Fetches one song to a partial file, then moves it into place.
/// `single` is whether the user asked for it on its own.
fn fetch(inner: &Inner, request: &DownloadRequest, cancel: &AtomicBool, single: bool) -> Result<u64> {
    let id = &request.id;
    let source = inner.quality.source(id);
    let wanted = plan(inner.quality.download(), source.as_ref());
    // The URL to fetch, and for transcoded downloads the format and rate.
    let (url, transcoded) = match wanted {
        Plan::Transcode { codec, kbps } => match inner.quality.route(id, codec, kbps, 0, &Mutex::new(None))? {
            Route::Transcode { url } => (url, Some((codec, kbps))),
            Route::Original => (inner.client.stream_url(id, &StreamOptions { format: Some("raw".to_string()), ..Default::default() })?, None),
            Route::Unavailable(reason) => return Err(Error::Audio(format!("the server can't make {} for this song: {reason}", codec.label()))),
        },
        Plan::Original => (inner.client.stream_url(id, &StreamOptions { format: Some("raw".to_string()), ..Default::default() })?, None),
    };
    if let Some(done) = from_cache(inner, request, wanted, source.as_ref(), single) {
        return Ok(done);
    }
    let suffix: String = match transcoded {
        Some((codec, _)) => codec.extension().to_string(),
        None => request.suffix.as_deref().unwrap_or("audio").chars().filter(|c| c.is_ascii_alphanumeric()).take(8).collect(),
    };
    let suffix = if suffix.is_empty() { "audio".to_string() } else { suffix.to_ascii_lowercase() };
    let (relative, path) = inner.path_for(id, &suffix);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let part = path.with_extension(format!("{suffix}.part"));
    let mut resp = inner.client.form_post().request_blocking(&inner.http, &url).send().map_err(|e| Error::Network(e.without_url().to_string()))?;
    let status = resp.status();
    if status != StatusCode::OK && status != StatusCode::PARTIAL_CONTENT {
        return Err(Error::HttpStatus { status: status.as_u16(), endpoint: "stream".to_string() });
    }
    let content_type = resp.headers().get(CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    if content_type.contains("json") || content_type.contains("xml") {
        return Err(Error::Subsonic { code: 0, endpoint: "stream".to_string(), message: "the server sent a document instead of the file".to_string() });
    }
    if let Some((codec, _)) = transcoded
        && !codec.content_type_matches(&content_type)
    {
        return Err(Error::Audio(format!("the server can't make {} for this song", codec.label())));
    }
    // A transcode has no known length, so estimate its size from the duration and rate.
    let estimate = transcoded.and_then(|(_, kbps)| source.as_ref().and_then(|s| s.duration_s).map(|d| u64::from(d) * u64::from(kbps) * 125));
    let total = resp.content_length().or(estimate).or(request.size);
    let exact = resp.content_length();
    let mut file = fs::File::create(&part)?;
    let mut done = 0u64;
    let mut buf = vec![0u8; CHUNK];
    let mut last_progress = Instant::now();
    let finish = |result: Result<u64>| -> Result<u64> {
        if result.is_err() {
            let _ = fs::remove_file(&part);
        }
        result
    };
    loop {
        if cancel.load(Ordering::Acquire) {
            drop(file);
            return finish(Err(Error::Cancelled));
        }
        let n = resp.read(&mut buf).map_err(|e| Error::Network(e.to_string()));
        let n = match n {
            Ok(n) => n,
            Err(e) => return finish(Err(e)),
        };
        if n == 0 {
            break;
        }
        if let Err(e) = file.write_all(&buf[..n]) {
            return finish(Err(Error::Io(e)));
        }
        done += n as u64;
        if last_progress.elapsed() >= PROGRESS_EVERY {
            last_progress = Instant::now();
            if let Some(active) = inner.state.lock().expect("download state").active.get_mut(id) {
                active.done = done;
                active.total = total;
            }
            inner.emit(DownloadEvent::Progress { id: id.clone(), done, total });
        }
    }
    if let Err(e) = file.sync_all() {
        return finish(Err(Error::Io(e)));
    }
    drop(file);
    if let Some(exact) = exact
        && done != exact
    {
        return finish(Err(Error::Network(format!("the transfer stopped at {done} of {exact} bytes"))));
    }
    // A small file the decoder can't open isn't something to keep.
    if let Err(e) = Decoder::open(&part) {
        return finish(Err(Error::Audio(format!("the downloaded file can't be played: {e}"))));
    }
    if let Err(e) = fs::rename(&part, &path) {
        return finish(Err(Error::Io(e)));
    }
    if let Err(e) = inner.library.download_record_held(id, &relative, done, transcoded.map(|(c, k)| (c.id(), k)), single) {
        let _ = fs::remove_file(&path);
        return Err(e);
    }
    inner.emit(DownloadEvent::Progress { id: id.clone(), done, total: Some(done) });
    Ok(done)
}

/// Moves a song already in the song cache at the wanted quality instead of fetching it again.
fn from_cache(inner: &Inner, request: &DownloadRequest, wanted: Plan, source: Option<&crate::quality::SourceInfo>, single: bool) -> Option<u64> {
    let cache = inner.quality.cache()?;
    let (quality, suffix, recorded) = match wanted {
        Plan::Original => (Quality::Original, request.suffix.as_deref().unwrap_or("audio").chars().filter(|c| c.is_ascii_alphanumeric()).take(8).collect::<String>(), None),
        Plan::Transcode { codec, kbps } => (Quality::Transcode { codec, kbps }, codec.extension().to_string(), Some((codec.id(), kbps))),
    };
    let suffix = if suffix.is_empty() { "audio".to_string() } else { suffix.to_ascii_lowercase() };
    let (relative, path) = inner.path_for(&request.id, &suffix);
    let size = source.and_then(|s| s.size).unwrap_or(0);
    let bytes = cache.take(&request.id, quality, size, &path)?;
    if inner.library.download_record_held(&request.id, &relative, bytes, recorded, single).is_err() {
        let _ = fs::remove_file(&path);
        return None;
    }
    inner.emit(DownloadEvent::Progress { id: request.id.clone(), done: bytes, total: Some(bytes) });
    Some(bytes)
}
