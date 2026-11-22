//! The core as the Swift app uses it. Opens the local library offline, syncs in the background, answers row-based
//! queries and hands out a player. Nothing is written to the server unless reporting is on, through `set_reporting` or
//! `NAVIA_REPORT_PLAYS=1`.

use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use navia_core::artwork::{ArtworkCache, ArtworkConfig};
use navia_core::lyrics::LyricsCache;
use navia_core::audio::output::{CaptureBackend, CaptureBuffer, CpalBackend, OutputBackend};
use navia_core::audio::player::{Player, PlayerConfig, ServerResolver};
use navia_core::downloads::{CollectionEvent, DownloadEvent, DownloadManager, DownloadRequest};
use navia_core::library::collections::CollectionKind as CoreCollectionKind;
use navia_core::client::native::{PlaylistImageError, PlaylistImageSupport};
use navia_core::capabilities::ServerFeatures;
use navia_core::client::{Album, Artist, Client, ClientConfig, Credentials, Playlist, Song, StarTarget, WritePolicy};
use navia_core::connect::{self, Probe};
use navia_core::library::browse::{AlbumOrder, RankedArtist};
use navia_core::library::discography;
use navia_core::library::sync::{BackgroundRefresh, PlaylistEditError, SyncOptions, Syncer};
use navia_core::library::{Library, LibraryConfig, SongOrder, StarTable};
use navia_core::song_filters::{FilterId, FilterKind, FilterState, FilterValue, OptionLabel};
use navia_core::queue_sync::{QueueState, QueueSync, RemoteQueue};
use navia_core::reporting::PlayReporter;
use navia_core::session::attach_reporting;
use tokio::runtime::Runtime;
use tokio::task::JoinHandle;

use crate::{AlbumRow, ArtistRow, CoverArtRow, NaviaError, NaviaPlayer, OutputPolicy, PlaylistRow, SearchResultsRow, SongRow, album_row, artist_row, playlist_row, song_row};

/// How often the library, playlists and play counts are refreshed in the background.
const REFRESH_EVERY: Duration = Duration::from_secs(20 * 60);
/// Artwork cache limit, the same as the GPUI app.
const ARTWORK_CACHE_BYTES: u64 = navia_core::artwork::DEFAULT_MAX_BYTES;

#[derive(uniffi::Record)]
pub struct CoreOptions {
    /// Allows writes to the server like scrobbles and stars. Set by Settings, or by `NAVIA_REPORT_PLAYS=1` for testing.
    pub report_plays: bool,
    /// Library and artwork base directory. None uses the normal per-user location.
    pub data_dir: Option<String>,
    /// A scripted test run. No external lookups or playlist fetches, so nothing can touch a shared server's stored
    /// data.
    pub test_run: bool,
}

/// One part of the artist page, in page order.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReleaseSection {
    Albums,
    SinglesAndEps,
    Compilations,
    AppearsOn,
}

impl From<discography::ReleaseSection> for ReleaseSection {
    fn from(s: discography::ReleaseSection) -> Self {
        match s {
            discography::ReleaseSection::Albums => ReleaseSection::Albums,
            discography::ReleaseSection::SinglesAndEps => ReleaseSection::SinglesAndEps,
            discography::ReleaseSection::Compilations => ReleaseSection::Compilations,
            discography::ReleaseSection::AppearsOn => ReleaseSection::AppearsOn,
        }
    }
}

/// A section's albums as library rows, newest first.
#[derive(uniffi::Record)]
pub struct DiscographySectionRow {
    pub section: ReleaseSection,
    pub rowids: Vec<u32>,
}

/// one song's Deezer `rank` for the album page's Heat column (`navia_core::album_heat`).
#[derive(uniffi::Record)]
pub struct SongHeatRow {
    pub song_id: String,
    pub rank: u64,
}

fn heat_rows(heat: std::collections::HashMap<String, u64>) -> Vec<SongHeatRow> {
    heat.into_iter().map(|(song_id, rank)| SongHeatRow { song_id, rank }).collect()
}

/// "More by <album artist>" under an album (`navia_core::library::discography::more_by`).
#[derive(uniffi::Record)]
pub struct MoreByRow {
    pub artist: ArtistRow,
    /// The album artist's other albums, newest first.
    pub albums: Vec<AlbumRow>,
    /// The discography section "See Discography" opens.
    pub section: ReleaseSection,
}

/// An artist's albums by section. Empty sections are left out.
#[derive(uniffi::Record)]
pub struct DiscographyRow {
    pub sections: Vec<DiscographySectionRow>,
    /// Whether the server's list was used rather than the library's songs.
    pub from_server: bool,
    /// Albums the server listed that this device hasn't synced yet.
    pub not_synced: u32,
}

/// `id` can be absent on success if the server didn't say it.
#[derive(uniffi::Record)]
pub struct CreatedPlaylist {
    pub id: Option<String>,
    pub problem: Option<String>,
}

/// The result of saving a smart playlist: its id and the rules as stored, as JSON, or `problem` with the message to
/// show.
#[derive(uniffi::Record)]
pub struct SmartSaveResult {
    pub id: Option<String>,
    pub rules: Option<String>,
    pub problem: Option<String>,
}

/// What a test run puts in `NAVIA_TEST_PLAYLIST` to let it create one smart playlist of its own.
const NEW_SMART_TEST: &str = "new-smart";

/// Why a smart playlist with this name and rules can't be saved, or None when it can. Only checks the form, so the
/// server can still refuse.
#[uniffi::export]
pub fn smart_rules_problem(name: String, criteria: String) -> Option<String> {
    navia_core::smart_rules::checked_name(&name).and_then(|_| navia_core::smart_rules::checked_criteria(&criteria)).err().map(|p| p.0)
}

/// The result of changing a playlist's image. `cover_art` is the new cover id once the server has one, and `problem` is
/// the message when it failed.
#[derive(uniffi::Record)]
pub struct PlaylistImageResult {
    pub cover_art: Option<String>,
    pub problem: Option<String>,
}

/// What a server test found, from `connect::probe`. Read-only, one ping.
#[derive(uniffi::Enum)]
pub enum ServerProbe {
    Connected { server: String, api: String, username: String },
    Found { server: String, api: String },
    BadAddress,
    Refused,
    TimedOut,
    UnknownHost,
    BadCertificate,
    NotSecure,
    Unreachable { detail: String },
    WrongLogin,
    TokenUnsupported,
    NotNavidrome { name: String },
    NotSubsonic,
    ServerError { code: i64, message: String },
}

impl From<Probe> for ServerProbe {
    fn from(p: Probe) -> Self {
        match p {
            Probe::Connected { server, api, username } => Self::Connected { server, api, username },
            Probe::Found { server, api } => Self::Found { server, api },
            Probe::BadAddress => Self::BadAddress,
            Probe::Refused => Self::Refused,
            Probe::TimedOut => Self::TimedOut,
            Probe::UnknownHost => Self::UnknownHost,
            Probe::BadCertificate => Self::BadCertificate,
            Probe::NotSecure => Self::NotSecure,
            Probe::Unreachable { detail } => Self::Unreachable { detail },
            Probe::WrongLogin => Self::WrongLogin,
            Probe::TokenUnsupported => Self::TokenUnsupported,
            Probe::NotNavidrome { name } => Self::NotNavidrome { name },
            Probe::NotSubsonic => Self::NotSubsonic,
            Probe::ServerError { code, message } => Self::ServerError { code, message },
        }
    }
}

/// A typed server address, normalized.
#[derive(uniffi::Record)]
pub struct ServerAddress {
    pub url: String,
    pub host: String,
    /// Plain HTTP to something that isn't on the local network.
    pub unencrypted: bool,
}

/// Turns what was typed into a server address. None when it can't be one.
#[uniffi::export]
pub fn parse_server_address(raw: String) -> Option<ServerAddress> {
    connect::parse_address(&raw).map(|a| ServerAddress { url: a.url, host: a.host, unencrypted: a.unencrypted })
}

/// Whether the developer override (`NAVIA_DEV=1`) is on. Only then may the login come from `NAVIDROME_*` variables or a
/// developer login file.
#[uniffi::export]
pub fn developer_override() -> bool {
    connect::developer_override()
}

/// Today's local date as days since 1970-01-01, from the core's clock, so day-keyed rows turn over when the core's do.
#[uniffi::export]
pub fn local_day() -> i64 {
    navia_core::library::taste::local_day(navia_core::library::taste::now_unix())
}

/// Checks an address and login with one read-only ping and no retries. An empty username or password only checks the
/// server.
#[uniffi::export(async_runtime = "tokio")]
pub async fn probe_server(address: String, username: String, password: String) -> ServerProbe {
    connect::probe(&address, &username, &password).await.into()
}

/// The stable per-server, per-user key that the core's data folder is named after.
#[uniffi::export]
pub fn account_key(server_url: String, username: String) -> String {
    Credentials::new(username, String::new()).account_key(&server_url)
}

/// The app's release version for the User-Agent, without any "+N" build suffix. Call once at launch, before
/// `open_core`.
#[uniffi::export]
pub fn set_app_version(version: String) {
    navia_core::client::set_app_version(&version);
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum ReleaseCheckRow {
    UpToDate,
    Available { version: String, url: String },
    Failed,
}

/// Asks GitHub whether a release newer than `running_version` is out. Blocks, so call it off the main thread.
#[uniffi::export]
pub fn check_latest_release(running_version: String) -> ReleaseCheckRow {
    use navia_core::updates::ReleaseCheck;
    match navia_core::updates::check_latest_release(&running_version) {
        Ok(ReleaseCheck::UpToDate) => ReleaseCheckRow::UpToDate,
        Ok(ReleaseCheck::Available { version, url }) => ReleaseCheckRow::Available { version, url },
        Err(error) => {
            tracing::warn!("update check failed: {error}");
            ReleaseCheckRow::Failed
        }
    }
}

/// Whether this run may check for updates by itself.
#[uniffi::export]
pub fn automatic_update_check_allowed(test_run: bool) -> bool {
    navia_core::updates::automatic_check_allowed(test_run)
}

/// Seconds between automatic update checks after the one at launch.
#[uniffi::export]
pub fn update_check_interval_secs() -> u64 {
    navia_core::updates::UPDATE_CHECK_INTERVAL.as_secs()
}

/// A client's display name. Navia's own is always "Navia".
#[uniffi::export]
pub fn client_display_name(name: String) -> String {
    navia_core::client::client_display_name(&name)
}

/// When a play counts for this account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct PlayThresholdRow {
    /// Percent of the song, one of `play_threshold_choices`.
    pub percent: u32,
    /// Also count after four minutes, if that comes first.
    pub cap_at_four_minutes: bool,
}

/// What Navia keeps on this device, for Settings > Storage. All sizes in bytes.
#[derive(uniffi::Record)]
pub struct StorageUsage {
    pub song_cache_bytes: u64,
    pub cached_songs: u64,
    /// The song cache limit. 0 means off.
    pub song_cache_limit: u64,
    /// Caching stopped because the disk is nearly full.
    pub song_cache_low_disk: bool,
    pub artwork_bytes: u64,
    pub lyrics_bytes: u64,
    pub downloads_bytes: u64,
    pub downloaded_songs: u64,
}

/// One finished download and its size on disk.
#[derive(uniffi::Record)]
pub struct DownloadedRow {
    pub id: String,
    pub bytes: u64,
}

/// A song in the song cache: total size of its copies, and when it was last played or cached.
#[derive(uniffi::Record)]
pub struct CachedSongRow {
    pub id: String,
    pub bytes: u64,
    pub last_played_ms: i64,
}

#[derive(uniffi::Enum)]
pub enum CoreEvent {
    Connected { ok: bool },
    SyncProgress,
    /// Sync progress moved, started or ended. Only the meter needs redrawing.
    SyncMeter,
    /// The catalog, playlists or annotations changed, so lists should reload.
    SyncDone,
    /// The sync stopped partway, so a first sync's library may be incomplete.
    SyncFailed,
    /// For You has something new stored (a mix landed, similar artists came in): reload it with
    /// `for_you`.
    ForYouChanged,
    /// The server refused the saved login (Subsonic error 40 or 41, or HTTP 401). Sent once per
    /// core, before `Connected { ok: false }`, so the app can ask to sign in again.
    LoginRejected,
}

#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncStageRow {
    Artists,
    Albums,
    Songs,
    Playlists,
}

/// A running sync's progress (`navia_core::library::sync::SyncMeter`). `stage` is None until the first page, and
/// `total` is set only when the server reported one.
#[derive(uniffi::Record, Clone, Debug)]
pub struct SyncProgressRow {
    pub stage: Option<SyncStageRow>,
    pub done: u32,
    pub total: Option<u32>,
    /// 0 to 1 for the bar, across the whole sync when every total is known or else the stage. None means sweep.
    pub fraction: Option<f32>,
    /// The library had never finished a sync, so it may be empty.
    pub first: bool,
}

/// Lossless above 16-bit or 48 kHz, other lossless, or lossy (`navia_core::audio_quality::Tier`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum QualityTier {
    HiRes,
    Lossless,
    Lossy,
}

/// A song's original file, for the quality badge. Unknown values are None.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AudioFileRow {
    /// The format's short name, like FLAC or AAC.
    pub format: String,
    pub tier: QualityTier,
    pub kbps: Option<u32>,
    pub bit_depth: Option<u32>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
}

#[derive(uniffi::Enum)]
pub enum DownloadEventRow {
    Progress { id: String, done: u64, total: Option<u64> },
    Finished { id: String, size: u64 },
    Failed { id: String, message: String },
    Removed { id: String },
}

/// Receives download events on a background thread.
#[uniffi::export(callback_interface)]
pub trait DownloadListener: Send + Sync {
    fn on_event(&self, event: DownloadEventRow);
}

/// A playlist or album that can be kept on the device.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollectionKind {
    Playlist,
    Album,
}

impl CollectionKind {
    fn core(self) -> CoreCollectionKind {
        match self {
            CollectionKind::Playlist => CoreCollectionKind::Playlist,
            CollectionKind::Album => CoreCollectionKind::Album,
        }
    }

    fn from_core(kind: CoreCollectionKind) -> Self {
        match kind {
            CoreCollectionKind::Playlist => CollectionKind::Playlist,
            CoreCollectionKind::Album => CollectionKind::Album,
        }
    }
}

/// What a kept collection is doing, for the switch's status line.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollectionPhase {
    Done,
    Downloading,
    /// It was complete, and songs added since are downloading.
    Syncing,
    /// Paused on a metered connection.
    Metered,
    Offline,
    /// Songs are missing and nothing is fetching them, because they failed or the playlist hasn't been fetched yet.
    Waiting,
}

#[derive(uniffi::Record)]
pub struct CollectionStatusRow {
    pub phase: CollectionPhase,
    pub have: u32,
    pub total: u32,
    pub missing: u32,
    pub failed: u32,
    /// 0 to 1, counting in-flight songs by their progress.
    pub progress: f64,
    /// Bytes its songs take on the device.
    pub bytes: u64,
}

#[derive(uniffi::Record)]
pub struct SyncedCollectionRow {
    pub kind: CollectionKind,
    pub id: String,
    /// When the switch was turned on, in Unix milliseconds.
    pub since_ms: i64,
}

/// What turning a collection off does, for the Cancel / Keep Songs / Remove dialog.
#[derive(uniffi::Record)]
pub struct RemovalPlanRow {
    /// Songs only this collection holds. Remove deletes them, and Keep Songs keeps them as single downloads.
    pub remove: u32,
    pub remove_bytes: u64,
    /// Songs another collection or a single download also holds. They stay either way.
    pub keep: u32,
}

/// Called on a background thread when a kept collection changes. Read its status again.
#[uniffi::export(callback_interface)]
pub trait CollectionListener: Send + Sync {
    fn on_collection_changed(&self, kind: CollectionKind, id: String);
}

/// Receives background events on a background thread.
#[uniffi::export(callback_interface)]
pub trait CoreListener: Send + Sync {
    fn on_event(&self, event: CoreEvent);
}

/// One of the Songs or Artists filters (`navia_core::song_filters::FilterId`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum SongFilterId {
    Genre,
    Artist,
    Year,
    Favorite,
    Plays,
    Added,
    Quality,
    Downloaded,
    AlbumArtist,
    LastPlayed,
    Format,
    Bitrate,
    Length,
    Explicit,
    Bpm,
    Rating,
    Albums,
    Role,
}

macro_rules! filter_ids {
    ($($v:ident),*) => {
        impl From<SongFilterId> for FilterId {
            fn from(id: SongFilterId) -> Self { match id { $(SongFilterId::$v => FilterId::$v),* } }
        }
        impl From<FilterId> for SongFilterId {
            fn from(id: FilterId) -> Self { match id { $(FilterId::$v => SongFilterId::$v),* } }
        }
    };
}
filter_ids!(Genre, Artist, Year, Favorite, Plays, Added, Quality, Downloaded, AlbumArtist, LastPlayed, Format, Bitrate, Length, Explicit, Bpm, Rating, Albums, Role);

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum SongFilterKind {
    List,
    Range,
    Toggle,
}

/// A filter as the Songs page offers it. Labels are English catalog keys.
#[derive(Debug, Clone, uniffi::Record)]
pub struct SongFilterInfo {
    pub id: SongFilterId,
    pub label: String,
    /// Shown up front. The rest go under More Filters.
    pub main: bool,
    pub kind: SongFilterKind,
    /// A list that allows at most one pick.
    pub single: bool,
    /// The find field's placeholder for a long list of names.
    pub find: Option<String>,
    /// Most digits a range value takes.
    pub digits: u32,
}

/// The Artists split list's line under a name: album and song counts, counted the way the filters count.
#[derive(Debug, Clone, uniffi::Record)]
pub struct ArtistCountsRow {
    pub albums: u32,
    pub songs: u32,
}

/// A choice of a list filter. `label` is the server's text, or an English catalog key when `translate`.
#[derive(Debug, Clone, uniffi::Record)]
pub struct SongFilterOptionRow {
    pub value: String,
    pub label: String,
    pub translate: bool,
    pub count: u32,
}

/// A range's quick pick, like a decade.
#[derive(Debug, Clone, uniffi::Record)]
pub struct SongFilterQuickRow {
    pub label: String,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum SongFilterValue {
    List { values: Vec<String> },
    Range { from: String, to: String },
    On,
}

/// One filter that's set on the page.
#[derive(Debug, Clone, uniffi::Record)]
pub struct SongFilterSetting {
    pub id: SongFilterId,
    pub value: SongFilterValue,
}

/// Save as Smart Playlist's Navidrome criteria, and the filters the server has no field for.
#[derive(Debug, Clone, uniffi::Record)]
pub struct SongFilterRules {
    pub json: String,
    pub dropped: Vec<SongFilterId>,
}

/// The clock the Songs filters' date choices count back from.
fn filters_now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn filter_state(settings: &[SongFilterSetting]) -> FilterState {
    let mut state = FilterState::default();
    for s in settings {
        let value = match &s.value {
            SongFilterValue::List { values } => FilterValue::List(values.clone()),
            SongFilterValue::Range { from, to } => FilterValue::Range { from: from.clone(), to: to.clone() },
            SongFilterValue::On => FilterValue::On,
        };
        state.set(s.id.into(), Some(value));
    }
    state
}

/// Every Songs filter in chip order. `NaviaCore::song_filters_available` says which the library can offer.
#[uniffi::export]
pub fn song_filter_catalog() -> Vec<SongFilterInfo> {
    filter_infos(&navia_core::song_filters::FILTERS)
}

/// Every Artists filter in chip order. `NaviaCore::artist_filters_available` says which the library can offer.
#[uniffi::export]
pub fn artist_filter_catalog() -> Vec<SongFilterInfo> {
    filter_infos(&navia_core::artist_filters::ARTIST_FILTERS)
}

fn filter_infos(specs: &[navia_core::song_filters::FilterSpec]) -> Vec<SongFilterInfo> {
    specs
        .iter()
        .map(|f| {
            let (kind, single, find, digits) = match f.kind {
                FilterKind::List { single, find } => (SongFilterKind::List, single, find.map(str::to_owned), 0),
                FilterKind::Range { digits } => (SongFilterKind::Range, false, None, digits as u32),
                FilterKind::Toggle => (SongFilterKind::Toggle, false, None, 0),
            };
            SongFilterInfo { id: f.id.into(), label: f.label.to_owned(), main: f.main, kind, single, find, digits }
        })
        .collect()
}

fn option_rows(options: Vec<navia_core::song_filters::FilterOption>) -> Vec<SongFilterOptionRow> {
    options
        .into_iter()
        .map(|o| {
            let (label, translate) = match o.label {
                OptionLabel::Text(t) => (t, false),
                OptionLabel::Key(k) => (k.to_owned(), true),
            };
            SongFilterOptionRow { value: o.value, label, translate, count: o.count }
        })
        .collect()
}

/// Why a typed range can't be used yet, as an English catalog key, or None.
#[uniffi::export]
pub fn song_filter_range_problem(id: SongFilterId, from: String, to: String) -> Option<String> {
    navia_core::song_filters::range_problem(id.into(), &from, &to).map(str::to_owned)
}

/// The part of typed text a From or To field keeps: digits only, at most as many as the range takes.
#[uniffi::export]
pub fn song_filter_clean_input(id: SongFilterId, text: String) -> String {
    navia_core::song_filters::clean_range_input(id.into(), &text)
}

/// Save as Smart Playlist's rules for the set filters and the page's sort.
#[uniffi::export]
pub fn song_filter_rules(filters: Vec<SongFilterSetting>, order: SongOrderBy) -> SongFilterRules {
    let (sort, descending) = navia_core::song_filters::smart_sort(order.into());
    let (json, dropped) = navia_core::song_filters::to_rules(&filter_state(&filters), sort, descending);
    SongFilterRules { json, dropped: dropped.into_iter().map(Into::into).collect() }
}

#[derive(uniffi::Enum)]
pub enum SongOrderBy {
    MostPlayed,
    Title,
    Artist,
    Album,
    Year,
    Duration,
    /// Newest first, by date added.
    Added,
}

impl From<SongOrderBy> for SongOrder {
    fn from(o: SongOrderBy) -> Self {
        match o {
            SongOrderBy::MostPlayed => SongOrder::MostPlayed,
            SongOrderBy::Title => SongOrder::Title,
            SongOrderBy::Artist => SongOrder::Artist,
            SongOrderBy::Album => SongOrder::Album,
            SongOrderBy::Year => SongOrder::Year,
            SongOrderBy::Duration => SongOrder::Duration,
            SongOrderBy::Added => SongOrder::Added,
        }
    }
}

#[derive(uniffi::Enum)]
pub enum AlbumOrderBy {
    MostPlayed,
    RecentlyAdded,
    Name,
    Year,
}

impl From<AlbumOrderBy> for AlbumOrder {
    fn from(o: AlbumOrderBy) -> Self {
        match o {
            AlbumOrderBy::MostPlayed => AlbumOrder::MostPlayed,
            AlbumOrderBy::RecentlyAdded => AlbumOrder::RecentlyAdded,
            AlbumOrderBy::Name => AlbumOrder::Name,
            AlbumOrderBy::Year => AlbumOrder::Year,
        }
    }
}

#[derive(uniffi::Record)]
pub struct AlbumOrderRow {
    pub rowids: Vec<u32>,
    pub total_songs: u64,
}

#[derive(uniffi::Record)]
pub struct RankedArtistRow {
    pub artist: ArtistRow,
    pub plays: u64,
    pub own_albums: u32,
    /// The recent-taste score where the list is ranked by it, otherwise 0.
    pub score: f64,
}

/// "Because you've been playing X": the seed and similar artists in the library.
#[derive(uniffi::Record)]
pub struct BecauseRow {
    pub seed: ArtistRow,
    pub artists: Vec<ArtistRow>,
}

/// One Artist Radio station on Home (`navia_core::stations`): the seed artist and its neighbors with photos.
#[derive(uniffi::Record)]
pub struct ArtistStationRow {
    pub seed: ArtistRow,
    pub neighbours: Vec<ArtistRow>,
}

/// One of today's mixes on For You (`library::for_you`).
#[derive(uniffi::Record)]
pub struct MixRow {
    /// `artist:<id>` or `genre:<name>`, the mix page's address.
    pub id: String,
    pub is_genre: bool,
    /// The artist or genre name. The title is "<name> Mix".
    pub name: String,
    pub songs: Vec<SongRow>,
    /// Main artists in order of first appearance, for the "A, B, C and more" line.
    pub artists: Vec<String>,
}

/// Discover's For You page. Each row is empty when the server has no data for it.
#[derive(uniffi::Record)]
pub struct ForYouRow {
    pub mixes: Vec<MixRow>,
    pub new_to_you: Vec<AlbumRow>,
    pub unplayed: Vec<AlbumRow>,
    pub forgotten: Vec<SongRow>,
    pub on_this_day: Vec<AlbumRow>,
    pub not_played_recently: Vec<AlbumRow>,
    /// Today's mixes and similar artists haven't been requested yet. Call `refresh_for_you`.
    pub needs_refresh: bool,
    /// today's mixes are being made right now: an empty page isn't final yet.
    pub building: bool,
}

/// what a change check found (`NaviaCore::check_for_changes`).
#[derive(uniffi::Record, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChangesRow {
    /// the server's library changed and was synced now.
    pub library: bool,
    /// the server is scanning; its changes come with a later check.
    pub server_scanning: bool,
    /// a sync was already running; its own `SyncDone` follows when it ends.
    pub sync_running: bool,
    pub playlists: bool,
    pub stars: bool,
    pub plays: bool,
}

#[derive(uniffi::Record)]
pub struct GenreRow {
    pub name: String,
    pub songs: u64,
}

/// A genre on the Genres page: its server name, album count, and the album ids whose covers it shows.
#[derive(uniffi::Record)]
pub struct GenreEntryRow {
    pub name: String,
    pub albums: u32,
    pub cover_album_ids: Vec<String>,
}

/// The Genres page: top genre tiles with up to three covers each, never repeated, then every genre as a row with one
/// cover. `browse` holds Search's "Browse by Genre" tiles.
#[derive(uniffi::Record)]
pub struct GenrePageRow {
    pub top: Vec<GenreEntryRow>,
    pub all: Vec<GenreEntryRow>,
    pub browse: Vec<GenreEntryRow>,
}

#[derive(uniffi::Record)]
pub struct LyricLineRow {
    /// Milliseconds into the song. None for unsynced lyrics.
    pub start_ms: Option<u32>,
    pub text: String,
    /// The exact end, when the server's source has one (`songLyrics` v2).
    pub end_ms: Option<u32>,
    /// Real word timing (`songLyrics` v2), or empty when the line has none. Read it through [`lyric_sung_fraction`].
    pub words: Vec<LyricWordRow>,
}

/// One timed word or syllable as a byte range of the line's UTF-8 text, end exclusive.
#[derive(uniffi::Record, Clone)]
pub struct LyricWordRow {
    pub start: u32,
    pub end: u32,
    pub start_ms: u32,
    pub end_ms: Option<u32>,
}

impl From<navia_core::lyrics::LyricLine> for LyricLineRow {
    fn from(l: navia_core::lyrics::LyricLine) -> Self {
        let words = l.words.iter().map(|w| LyricWordRow { start: w.start, end: w.end, start_ms: w.start_ms, end_ms: w.end_ms }).collect();
        Self { start_ms: l.start_ms, text: l.text, end_ms: l.end_ms, words }
    }
}

/// How much of a line is sung at `ms`, as a fraction of its characters, from real word timing. None when there's none,
/// so the app estimates it.
#[uniffi::export]
pub fn lyric_sung_fraction(line: LyricLineRow, ms: u32) -> Option<f32> {
    if line.words.is_empty() {
        return None;
    }
    let words = line.words.iter().map(|w| navia_core::lyrics::LyricWord { start: w.start, end: w.end, start_ms: w.start_ms, end_ms: w.end_ms }).collect();
    navia_core::lyrics::LyricLine { start_ms: line.start_ms, text: line.text, end_ms: line.end_ms, words }.sung_fraction(ms)
}

/// A queue saved on the server by another client or device, offered at launch.
#[derive(uniffi::Record)]
pub struct RemoteQueueRow {
    /// The name the saving client gave itself. May be empty.
    pub device: String,
    /// Unix milliseconds.
    pub changed_ms: u64,
    pub songs: Vec<SongRow>,
    pub index: u32,
    pub position_ms: u64,
}

#[derive(uniffi::Record)]
pub struct LyricsRow {
    pub synced: bool,
    pub lines: Vec<LyricLineRow>,
}

#[derive(uniffi::Record)]
pub struct StationRow {
    pub name: String,
    pub url: String,
}

#[derive(uniffi::Record)]
pub struct ArtistInfoRow {
    pub biography: String,
    pub similar: Vec<String>,
}

// MARK: Get Info (`navia_core::library::info`). Only what the server sent, so every field is optional.

#[derive(uniffi::Record)]
pub struct CreditNameRow {
    pub name: String,
    pub artist_id: Option<String>,
    /// OpenSubsonic `subRole`, such as a performer's instrument.
    pub sub_role: Option<String>,
}

/// Everyone credited in one role, like composer or producer. Roles come in the usual credit order.
#[derive(uniffi::Record)]
pub struct CreditRow {
    pub role: String,
    pub names: Vec<CreditNameRow>,
}

#[derive(uniffi::Record)]
pub struct ReplayGainRow {
    pub track_gain: Option<f64>,
    pub album_gain: Option<f64>,
    pub track_peak: Option<f64>,
    pub album_peak: Option<f64>,
}

#[derive(uniffi::Record)]
pub struct ItemDateRow {
    pub year: Option<i32>,
    pub month: Option<i32>,
    pub day: Option<i32>,
}

#[derive(uniffi::Record)]
pub struct SongDetailsRow {
    /// The file format's short name, like FLAC or AAC, from the suffix and bitrate.
    pub format: Option<String>,
    pub id: String,
    pub title: String,
    pub album: Option<String>,
    pub album_id: Option<String>,
    pub artist: Option<String>,
    pub artist_id: Option<String>,
    /// OpenSubsonic `artists`, only when there's more than one.
    pub artists: Vec<CreditNameRow>,
    pub track: Option<u32>,
    pub disc: Option<u32>,
    pub year: Option<i32>,
    pub genres: Vec<String>,
    pub duration_seconds: Option<u32>,
    pub size: Option<u64>,
    pub suffix: Option<String>,
    pub content_type: Option<String>,
    pub bit_rate_kbps: Option<u32>,
    pub bit_depth: Option<u32>,
    pub sampling_rate: Option<u32>,
    pub channel_count: Option<u32>,
    pub path: Option<String>,
    pub bpm: Option<u32>,
    pub replay_gain: Option<ReplayGainRow>,
    pub isrc: Vec<String>,
    pub music_brainz_id: Option<String>,
    pub comment: Option<String>,
    /// Credits other than artist and album artist.
    pub credits: Vec<CreditRow>,
    /// "explicit" or "clean".
    pub explicit_status: Option<String>,
    /// Date added on the server.
    pub created: Option<String>,
    /// The signed-in user's own plays.
    pub play_count: Option<u64>,
    pub played: Option<String>,
    pub starred: Option<String>,
}

#[derive(uniffi::Record)]
pub struct AlbumDetailsRow {
    pub id: String,
    pub name: String,
    pub artist: Option<String>,
    pub artist_id: Option<String>,
    pub artists: Vec<CreditNameRow>,
    /// The version as a label (`navia_core::display::edition`). None for explicit or clean.
    pub version: Option<String>,
    pub record_labels: Vec<String>,
    pub release_types: Vec<String>,
    pub release_date: Option<ItemDateRow>,
    pub original_release_date: Option<ItemDateRow>,
    pub year: Option<i32>,
    pub is_compilation: Option<bool>,
    pub music_brainz_id: Option<String>,
    pub genres: Vec<String>,
    pub explicit_status: Option<String>,
    pub song_count: Option<u32>,
    pub duration_seconds: Option<u64>,
    /// Only when more than one.
    pub disc_count: Option<u32>,
    /// Sum of its songs' sizes in the library.
    pub size: Option<u64>,
    pub created: Option<String>,
    pub play_count: Option<u64>,
    pub played: Option<String>,
    pub starred: Option<String>,
}

#[derive(uniffi::Record)]
pub struct PlaylistDetailsRow {
    pub id: String,
    pub name: String,
    pub owner: Option<String>,
    pub public: Option<bool>,
    pub comment: Option<String>,
    pub created: Option<String>,
    pub changed: Option<String>,
    pub song_count: Option<u32>,
    pub duration_seconds: Option<u64>,
    /// Sum of its stored songs' sizes. None until its songs are stored.
    pub size: Option<u64>,
    pub readonly: Option<bool>,
    /// An evaluated smart playlist. Its rules come from `playlist_rules`.
    pub smart: bool,
    /// Smart playlists only. The songs stand until then.
    pub valid_until: Option<String>,
}

#[derive(uniffi::Record)]
pub struct ArtistDetailsRow {
    pub id: String,
    pub name: String,
    pub album_count: Option<u32>,
    /// Songs in the library by them or on their albums.
    pub song_count: u32,
    /// OpenSubsonic `roles` as sent. Navidrome also sends its own `maincredit`.
    pub roles: Vec<String>,
    pub music_brainz_id: Option<String>,
    /// The signed-in user's plays of those songs.
    pub play_count: u64,
    pub starred: Option<String>,
}

fn credit_names(list: Vec<navia_core::library::info::CreditName>) -> Vec<CreditNameRow> {
    list.into_iter().map(|n| CreditNameRow { name: n.name, artist_id: n.artist_id, sub_role: n.sub_role }).collect()
}

fn item_date(d: Option<navia_core::client::ItemDate>) -> Option<ItemDateRow> {
    d.map(|d| ItemDateRow { year: d.year, month: d.month, day: d.day })
}

#[derive(uniffi::Record)]
pub struct AccountRow {
    pub username: String,
    pub admin: Option<bool>,
}

/// Album notes and biographies come from Last.fm with HTML tags and entities.
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
    out.replace("&amp;", "&").replace("&quot;", "\"").replace("&#39;", "'").replace("&apos;", "'").replace("&lt;", "<").replace("&gt;", ">").replace("&nbsp;", " ").trim().to_string()
}

/// An ISO timestamp like `2024-05-01T12:00:00Z` from a Unix time, for comparing with stored `played` stamps.
fn iso_timestamp(unix_seconds: i64) -> String {
    let days = unix_seconds.div_euclid(86_400);
    let secs = unix_seconds.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", secs / 3600, secs % 3600 / 60, secs % 60)
}

fn because_from(db: &navia_core::library::Library, state: navia_core::library::taste::Because) -> Option<BecauseRow> {
    let navia_core::library::taste::Because::Ready { seed, artists } = state else { return None };
    let seed = db.artists_by_rowid(&[seed]).ok()?.into_iter().next().map(|(r, a)| artist_row(Some(r), a))?;
    let artists = db.artists_by_rowid(&artists).ok()?.into_iter().map(|(r, a)| artist_row(Some(r), a)).collect();
    Some(BecauseRow { seed, artists })
}

fn to_ranked(r: RankedArtist) -> RankedArtistRow {
    RankedArtistRow { artist: artist_row(Some(r.rowid), r.artist), plays: r.plays, own_albums: r.own_albums, score: r.score }
}

/// Opens the local library at once without the network, so a slow or missing server can't delay the first paint. Start
/// the network side with [`NaviaCore::start_background`].
#[uniffi::export]
pub fn open_core(server_url: String, username: String, password: String, options: CoreOptions) -> Result<Arc<NaviaCore>, NaviaError> {
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).thread_name("navia-core").enable_time().enable_io().build().map_err(|e| NaviaError::Failed { message: format!("runtime: {e}") })?;
    let credentials = Credentials::new(username, password);
    let mut config = ClientConfig::new(server_url.clone(), credentials.clone());
    config.write_policy = if options.report_plays { WritePolicy::Allowed } else { WritePolicy::ReadOnly };
    let client = Client::new(config)?;
    let writer = client.copy_with_policy(if options.test_run && !options.report_plays { WritePolicy::ReadOnly } else { WritePolicy::Allowed })?;

    let key = credentials.account_key(&server_url);
    let dirs = directories::ProjectDirs::from("", "", "Navia");
    let base = match &options.data_dir {
        Some(dir) => std::path::PathBuf::from(dir).join(&key),
        None => dirs.as_ref().ok_or_else(|| NaviaError::Failed { message: "no per-user data directory".into() })?.data_dir().join(&key),
    };
    let artwork_dir = match (&options.data_dir, &dirs) {
        (None, Some(d)) => d.cache_dir().join("artwork").join(&key),
        _ => base.join("artwork"),
    };
    let lyrics_dir = match (&options.data_dir, &dirs) {
        (None, Some(d)) => d.cache_dir().join("lyrics").join(&key),
        _ => base.join("lyrics"),
    };
    let lyrics = Arc::new(LyricsCache::new(lyrics_dir, navia_core::lyrics::DEFAULT_MAX_BYTES));
    let db = Library::open(&LibraryConfig { path: base.join("library.sqlite") })?;
    let artwork = {
        let _guard = rt.enter();
        ArtworkCache::new(client.clone(), ArtworkConfig { dir: artwork_dir, max_bytes: ARTWORK_CACHE_BYTES })?
    };
    // A store-backed reporter keeps plays only when reporting is on. It starts private.
    let reporter = Arc::new(PlayReporter::with_store(Arc::new(client.clone()), Arc::new(db.clone())));
    rt.block_on(reporter.set_private(!options.report_plays));
    let queue_sync = QueueSync::new(client.clone(), db.clone(), reporter.clone(), rt.handle().clone());
    let downloads = {
        let _guard = rt.enter();
        Arc::new(DownloadManager::new(db.clone(), client.clone(), base.join("downloads"))?)
    };
    let friends = navia_core::lanyard::watch::FriendsWatch::start(rt.handle());
    let sync_ping: Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>> = Arc::default();
    let sync_watch = Arc::new(navia_core::library::sync::SyncWatch::new({
        let ping = sync_ping.clone();
        move || {
            let ping = ping.lock().unwrap().clone();
            if let Some(ping) = ping {
                ping();
            }
        }
    }));
    Ok(Arc::new(NaviaCore { friends, rt: ManuallyDrop::new(rt), client, writer, db, artwork, lyrics, downloads, reporter, queue_sync, queue_offer: Mutex::new(None), report_plays: AtomicBool::new(options.report_plays), private_mode: AtomicBool::new(false), test_run: options.test_run, background: Mutex::new(None), background_sync: AtomicBool::new(true), first_sync: Mutex::new(None), daily_mixes: Mutex::new(None), listener: Mutex::new(None), features: OnceLock::new(), admin: Mutex::new(None), image_support: Mutex::new(None), sharing: Mutex::new(None), started: AtomicBool::new(false), login_rejected: AtomicBool::new(false), sync_watch, sync_ping }))
}

#[derive(uniffi::Object)]
pub struct NaviaCore {
    /// Taken apart in `Drop`, because dropping a runtime normally blocks and panics on one of its own threads.
    rt: ManuallyDrop<Runtime>,
    pub(crate) client: Client,
    /// For the user's own edits, such as creating a playlist, which don't depend on Scrobble
    /// plays. Read-only in a test run unless it asks for writes (`NAVIA_REPORT_PLAYS=1`).
    writer: Client,
    pub(crate) db: Library,
    artwork: ArtworkCache,
    lyrics: Arc<LyricsCache>,
    downloads: Arc<DownloadManager>,
    reporter: Arc<PlayReporter>,
    /// The play queue on the server.
    queue_sync: QueueSync,
    /// The last queue offered from another device, until answered.
    queue_offer: Mutex<Option<RemoteQueue>>,
    report_plays: AtomicBool,
    private_mode: AtomicBool,
    pub(crate) test_run: bool,
    background: Mutex<Option<BackgroundRefresh>>,
    /// Whether the periodic refresh should run. The first sync after launch always runs.
    background_sync: AtomicBool,
    first_sync: Mutex<Option<JoinHandle<()>>>,
    /// makes For You's mixes once a day in the background (`for_you::keep_mixes_daily`).
    daily_mixes: Mutex<Option<JoinHandle<()>>>,
    /// the app's listener, once `start_background` has it.
    listener: Mutex<Option<Arc<Mutex<Box<dyn CoreListener>>>>>,
    /// The OpenSubsonic extensions in use, once `getOpenSubsonicExtensions` answers.
    features: OnceLock<ServerFeatures>,
    admin: Mutex<Option<bool>>,
    /// What the server allows for playlist images. None until connected.
    image_support: Mutex<Option<PlaylistImageSupport>>,
    /// Whether getShares answers, and whether the user may download, since a link can allow downloads.
    sharing: Mutex<Option<SharingRow>>,
    started: AtomicBool,
    /// The server refused the saved login once already (`CoreEvent::LoginRejected`).
    login_rejected: AtomicBool,
    /// Spotify friends through Lanyard (`navia_core::lanyard`).
    friends: navia_core::lanyard::watch::FriendsWatch,
    sync_watch: Arc<navia_core::library::sync::SyncWatch>,
    /// Tells the app's listener that the watch changed. Set while background work runs.
    sync_ping: Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>>,
}

impl Drop for NaviaCore {
    fn drop(&mut self) {
        // SAFETY: `rt` isn't used again. This is the last access.
        let rt = unsafe { ManuallyDrop::take(&mut self.rt) };
        rt.shutdown_background();
    }
}

impl NaviaCore {
    fn mix_row(&self, m: navia_core::library::for_you::MixRows) -> MixRow {
        MixRow { id: m.id, is_genre: m.kind == navia_core::library::for_you::MixKind::Genre, name: m.name, songs: self.songs_by_rowid(m.songs), artists: m.artists }
    }

    /// The periodic refresh. Plays that couldn't be sent are retried only while reporting is on.
    fn start_refresh(self: &Arc<Self>) {
        if !self.background_sync.load(Ordering::SeqCst) || self.background.lock().unwrap().is_some() {
            return;
        }
        let _guard = self.rt.enter();
        let syncer = Syncer::new(self.client.clone(), self.db.clone(), self.sync_options());
        *self.background.lock().unwrap() = Some(BackgroundRefresh::spawn_with(syncer, REFRESH_EVERY, Some(self.reporter.clone())));
    }

    /// today's mixes, made in the background once a day after the launch sync, so For You has
    /// them before it's opened. test runs only read what's stored.
    fn start_daily_mixes(self: &Arc<Self>) {
        if !self.external_allowed() || self.daily_mixes.lock().unwrap().is_some() {
            return;
        }
        let listener = self.listener.lock().unwrap().clone();
        let changed: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if let Some(l) = &listener
                && let Ok(l) = l.lock()
            {
                l.on_event(CoreEvent::ForYouChanged);
            }
        });
        let task = self.rt.spawn(navia_core::library::for_you::keep_mixes_daily(self.client.clone(), self.db.clone(), changed));
        *self.daily_mixes.lock().unwrap() = Some(task);
    }

    /// Whether nothing should be reported right now.
    fn quiet(&self) -> bool {
        self.private_mode.load(Ordering::SeqCst) || !self.report_plays.load(Ordering::SeqCst)
    }

    /// Runs a future on the core's runtime, so all networking shares one reactor.
    pub(crate) async fn on_runtime<T: Send + 'static>(&self, f: impl std::future::Future<Output = T> + Send + 'static) -> Result<T, NaviaError> {
        self.rt.spawn(f).await.map_err(|e| NaviaError::Failed { message: format!("task: {e}") })
    }

    /// The client for image changes. It's the user's writer, which a test run keeps read-only except for the one
    /// playlist named in `NAVIA_TEST_COVER_PLAYLIST`.
    fn image_client(&self, id: &str) -> Result<Client, NaviaError> {
        if self.test_run && std::env::var("NAVIA_TEST_COVER_PLAYLIST").is_ok_and(|named| !named.is_empty() && named == id) {
            return Ok(self.writer.writable_copy()?);
        }
        Ok(self.writer.clone())
    }

    fn form_post(&self) -> bool {
        self.features.get().is_some_and(|f| f.form_post)
    }

    /// The client for changing a playlist. In a test run, a writable one only for the playlist named in
    /// `NAVIA_TEST_PLAYLIST` (or [`NEW_SMART_TEST`]).
    fn playlist_writer(&self, id: &str) -> Result<Client, NaviaError> {
        if self.test_run && std::env::var("NAVIA_TEST_PLAYLIST").is_ok_and(|named| !named.is_empty() && named == id) {
            return Ok(self.writer.writable_copy()?);
        }
        Ok(self.writer.clone())
    }

    fn cover_id(&self, key: &str) -> String {
        // Artist, playlist and direct ids pass through. A bare id is an album's.
        if key.contains('-') && ["ar-", "pl-", "al-", "mf-"].iter().any(|p| key.starts_with(p)) {
            return key.to_string();
        }
        self.db.album(key).ok().flatten().and_then(|a| a.cover_art).filter(|c| !c.is_empty()).unwrap_or_else(|| format!("al-{key}"))
    }

    /// External lookups are off for scripted test runs, because a stale item makes the server
    /// refresh its stored info, unless `NAVIA_TEST_EXTERNAL=1` asks for them.
    pub(crate) fn external_allowed(&self) -> bool {
        !self.test_run || std::env::var_os("NAVIA_TEST_EXTERNAL").is_some_and(|v| v == "1")
    }

    /// Fetching play history and library totals logs in to the server, so test runs skip it.
    /// `NAVIA_TEST_SERVER_TOTALS=1` fetches totals, but only against a local stand-in.
    fn sync_options(&self) -> SyncOptions {
        let native = !self.test_run || std::env::var("NAVIA_TEST_SERVER_TOTALS").is_ok_and(|v| v == "1");
        SyncOptions { play_history: !self.test_run, synced_smart: !self.test_run, watch: Some(self.sync_watch.clone()), server_totals: native, song_updates: native, ..Default::default() }
    }
}

#[uniffi::export]
impl NaviaCore {
    /// Connects, syncs and refreshes on a schedule in the background. Call once.
    pub fn start_background(self: Arc<Self>, listener: Box<dyn CoreListener>) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let this = self.clone();
        let task = self.rt.spawn(async move {
            match this.client.ping().await {
                Ok(_) => {
                    this.downloads.set_online(true);
                    listener.on_event(CoreEvent::Connected { ok: true });
                }
                Err(e) => {
                    this.downloads.set_online(false);
                    // Only once, so a dismissed sign-in screen stays dismissed when the app retries.
                    if e.is_auth_failure() && !this.login_rejected.swap(true, Ordering::SeqCst) {
                        listener.on_event(CoreEvent::LoginRejected);
                    }
                    listener.on_event(CoreEvent::Connected { ok: false });
                    return;
                }
            }
            let user = this.client.user().await.ok();
            *this.admin.lock().unwrap() = user.as_ref().and_then(|u| u.admin_role);
            let on = this.client.shares().await.is_ok();
            *this.sharing.lock().unwrap() = Some(SharingRow { on, downloads: user.and_then(|u| u.download_role).unwrap_or(false) });
            *this.image_support.lock().unwrap() = this.client.playlist_image_support().await.ok();
            if let Some(features) = ServerFeatures::detect(&this.client).await {
                let _ = this.features.set(features);
                this.reporter.set_live_reports(features.playback_report);
                this.queue_sync.set_features(features);
            }

            let syncer = Syncer::new(this.client.clone(), this.db.clone(), this.sync_options());
            let first_run = this.db.counts().map(|c| c.songs == 0).unwrap_or(true);
            let on_progress = {
                let ping = Arc::new(Mutex::new(std::time::Instant::now()));
                let listener = Arc::new(Mutex::new(listener));
                let progress = listener.clone();
                let tick = ping.clone();
                let hook = move |_: navia_core::library::sync::SyncProgress| {
                    let mut last = tick.lock().unwrap();
                    if last.elapsed() > Duration::from_millis(1200) {
                        *last = std::time::Instant::now();
                        if let Ok(l) = progress.lock() {
                            l.on_event(CoreEvent::SyncProgress);
                        }
                    }
                };
                (listener, hook)
            };
            let (listener, hook) = on_progress;
            *this.listener.lock().unwrap() = Some(listener.clone());
            *this.sync_ping.lock().unwrap() = Some(Arc::new({
                let listener = listener.clone();
                move || {
                    if let Ok(l) = listener.lock() {
                        l.on_event(CoreEvent::SyncMeter);
                    }
                }
            }));
            let result = if first_run { syncer.full_sync(&hook).await.map(|_| ()) } else { syncer.refresh(false, &hook).await.map(|_| ()) };
            if let Err(e) = result {
                eprintln!("navia: library sync failed: {e}");
                if let Ok(l) = listener.lock() {
                    l.on_event(CoreEvent::SyncFailed);
                }
            }
            // Smart playlists are only listed here. See `Syncer::sync_playlists`.
            if let Err(e) = syncer.sync_playlists().await {
                eprintln!("navia: playlist sync failed: {e}");
            }
            if let Ok(l) = listener.lock() {
                l.on_event(CoreEvent::SyncDone);
            }
            if let Err(e) = syncer.refresh_annotations().await {
                eprintln!("navia: annotation refresh failed: {e}");
            }
            if let Ok(l) = listener.lock() {
                l.on_event(CoreEvent::SyncDone);
            }
            this.start_refresh();
            this.start_daily_mixes();
        });
        *self.first_sync.lock().unwrap() = Some(task);
    }

    /// Stops all background work before leaving this server: sync, the refresh schedule and in-flight downloads.
    /// Finished downloads stay on disk.
    pub fn shutdown(&self) {
        self.sync_ping.lock().unwrap().take();
        self.background.lock().unwrap().take();
        if let Some(task) = self.first_sync.lock().unwrap().take() {
            task.abort();
        }
        if let Some(task) = self.daily_mixes.lock().unwrap().take() {
            task.abort();
        }
        self.downloads.cancel_all();
    }

    /// Turns the periodic refresh on or off. The sync at launch happens either way.
    pub fn set_background_sync(self: Arc<Self>, on: bool) {
        self.background_sync.store(on, Ordering::SeqCst);
        if on {
            if self.started.load(Ordering::SeqCst) {
                self.start_refresh();
            }
        } else {
            self.background.lock().unwrap().take();
        }
    }

    /// Whether the server answers now. Read-only.
    pub async fn check_server(&self) -> bool {
        let client = self.client.clone();
        self.on_runtime(async move { client.ping().await.is_ok() }).await.unwrap_or(false)
    }

    /// Restarts background work after [`Self::start_background`] gave up because the server was unreachable at launch.
    /// Does nothing once the refresh is running.
    pub fn restart_background(self: Arc<Self>, listener: Box<dyn CoreListener>) {
        if self.background.lock().unwrap().is_some() {
            return;
        }
        self.started.store(false, Ordering::SeqCst);
        self.start_background(listener);
    }

    pub fn song_count(&self) -> u64 {
        self.db.counts().map(|c| c.songs).unwrap_or(0)
    }

    /// The sync to show, if any. A first sync shows from the start, and others only once they've run `show_after_ms`,
    /// so a quick refresh stays silent.
    pub fn sync_progress(&self, show_after_ms: u64) -> Option<SyncProgressRow> {
        let shown = self.sync_watch.visible(Duration::from_millis(show_after_ms))?;
        let stage = shown.meter.map(|m| match m.stage {
            navia_core::library::sync::SyncStage::Artists => SyncStageRow::Artists,
            navia_core::library::sync::SyncStage::Albums => SyncStageRow::Albums,
            navia_core::library::sync::SyncStage::Songs => SyncStageRow::Songs,
            navia_core::library::sync::SyncStage::Playlists => SyncStageRow::Playlists,
        });
        Some(SyncProgressRow { stage, done: shown.meter.map_or(0, |m| m.done), total: shown.meter.and_then(|m| m.total), fraction: shown.meter.and_then(|m| m.fraction()), first: shown.first })
    }

    pub fn song_order(&self, order: SongOrderBy, query: Option<String>, genre: Option<String>, favorites: bool) -> Vec<u32> {
        self.db.song_order_filtered(order.into(), query.as_deref(), genre.as_deref(), favorites).unwrap_or_default()
    }

    /// The Songs list narrowed by the search text and the Songs filters.
    pub fn song_order_with_filters(&self, order: SongOrderBy, query: Option<String>, filters: Vec<SongFilterSetting>) -> Vec<u32> {
        self.db.filtered_song_order(order.into(), query.as_deref(), &filter_state(&filters), filters_now_ms()).unwrap_or_default()
    }

    /// The Songs filters this library has the data for, in chip order.
    pub fn song_filters_available(&self) -> Vec<SongFilterId> {
        self.db.song_filters().map(|i| i.available().into_iter().map(Into::into).collect()).unwrap_or_default()
    }

    /// A list filter's choices with their song counts.
    pub fn song_filter_options(&self, id: SongFilterId) -> Vec<SongFilterOptionRow> {
        let Ok(index) = self.db.song_filters() else { return Vec::new() };
        option_rows(index.options(id.into(), filters_now_ms()))
    }

    /// Every artist in `ranked_artists` order, narrowed by the search text and the Artists filters.
    pub fn ranked_artists_with_filters(&self, query: Option<String>, filters: Vec<SongFilterSetting>) -> Vec<RankedArtistRow> {
        self.db.filtered_artists(query.as_deref(), &filter_state(&filters), filters_now_ms()).unwrap_or_default().into_iter().map(to_ranked).collect()
    }

    /// The Artists filters this library has the data for, in chip order.
    pub fn artist_filters_available(&self) -> Vec<SongFilterId> {
        self.db.artist_filters().map(|i| i.available().into_iter().map(Into::into).collect()).unwrap_or_default()
    }

    /// Every artist's album and song counts by id, for the Artists split list. Empty until the library has artists.
    pub fn artist_counts(&self) -> std::collections::HashMap<String, ArtistCountsRow> {
        let Ok(index) = self.db.artist_filters() else { return Default::default() };
        index.all_counts().map(|(id, c)| (id.to_owned(), ArtistCountsRow { albums: c.albums, songs: c.songs })).collect()
    }

    /// An Artists list filter's choices with their artist counts.
    pub fn artist_filter_options(&self, id: SongFilterId) -> Vec<SongFilterOptionRow> {
        let Ok(index) = self.db.artist_filters() else { return Vec::new() };
        option_rows(index.options(id.into(), filters_now_ms()))
    }

    /// A range filter's quick picks, the library's decades.
    pub fn song_filter_quick(&self, id: SongFilterId) -> Vec<SongFilterQuickRow> {
        let Ok(index) = self.db.song_filters() else { return Vec::new() };
        index.quick_ranges(id.into()).into_iter().map(|q| SongFilterQuickRow { label: q.label, from: q.from, to: q.to }).collect()
    }

    pub fn songs_by_rowid(&self, rowids: Vec<u32>) -> Vec<SongRow> {
        self.db.songs_by_rowid(&rowids).unwrap_or_default().into_iter().map(|(r, s)| song_row(Some(r), s)).collect()
    }

    pub fn song_by_id(&self, id: String) -> Option<SongRow> {
        self.db.song(&id).ok().flatten().map(|s| song_row(None, s))
    }

    pub fn songs_by_id(&self, ids: Vec<String>) -> Vec<SongRow> {
        let rowids = self.db.song_rowids(&ids).unwrap_or_default();
        self.songs_by_rowid(rowids)
    }

    pub fn album_order(&self, order: AlbumOrderBy, query: Option<String>, genre: Option<String>, artist_id: Option<String>) -> AlbumOrderRow {
        let list = self.db.album_order(order.into(), query.as_deref(), genre.as_deref(), artist_id.as_deref()).unwrap_or_default();
        AlbumOrderRow { rowids: list.rowids, total_songs: list.total_songs }
    }

    pub fn albums_by_rowid(&self, rowids: Vec<u32>) -> Vec<AlbumRow> {
        self.db.albums_by_rowid(&rowids).unwrap_or_default().into_iter().map(|(r, a)| album_row(Some(r), a)).collect()
    }

    pub fn album(&self, id: String) -> Option<AlbumRow> {
        self.db.album(&id).ok().flatten().map(|a| album_row(None, a))
    }

    pub fn album_songs(&self, album_id: String) -> Vec<SongRow> {
        self.db.album_rows(&album_id).unwrap_or_default().into_iter().map(|(r, s)| song_row(Some(r), s)).collect()
    }

    pub fn ranked_artists(&self, query: Option<String>, limit: Option<u32>) -> Vec<RankedArtistRow> {
        self.db.ranked_artists(query.as_deref(), limit).unwrap_or_default().into_iter().map(to_ranked).collect()
    }

    pub fn artist(&self, id: String) -> Option<ArtistRow> {
        self.db.artist(&id).ok().flatten().map(|a: Artist| artist_row(None, a))
    }

    pub fn artist_by_name(&self, name: String) -> Option<ArtistRow> {
        self.db.artist_by_name(&name).ok().flatten().map(|a| artist_row(None, a))
    }

    pub fn artist_album_songs(&self, artist_id: String, limit: u32) -> Vec<SongRow> {
        let mut rowids = self.db.artist_album_rowids(&artist_id).unwrap_or_default();
        rowids.truncate(limit as usize);
        self.songs_by_rowid(rowids)
    }

    pub fn recent_albums(&self, limit: u32) -> Vec<AlbumRow> {
        self.db.list_albums(navia_core::library::AlbumSort::RecentlyAdded, 0, limit, None).unwrap_or_default().into_iter().map(|a: Album| album_row(None, a)).collect()
    }

    /// Albums by recent taste (`library::taste`), only ones with real plays. The Albums page's "Most Played" sort stays
    /// all-time.
    pub fn top_albums(&self, limit: u32) -> Vec<AlbumRow> {
        self.db.taste_albums(Some(limit)).unwrap_or_default().into_iter().map(|(rowid, a)| album_row(Some(rowid), a)).collect()
    }

    /// Artists by recent taste, best first, only ones with real plays. The Artists page uses `ranked_artists` instead.
    pub fn taste_artists(&self, limit: Option<u32>) -> Vec<RankedArtistRow> {
        self.db.taste_artists(limit).unwrap_or_default().into_iter().map(to_ranked).collect()
    }

    /// On Repeat: songs played 3+ times in the last 4 weeks, most first. Empty without dated plays.
    pub fn on_repeat(&self) -> Vec<SongRow> {
        let rowids = self.db.taste().map(|t| t.on_repeat.clone()).unwrap_or_default();
        self.songs_by_rowid(rowids)
    }

    /// Back in Rotation: old favorites gone quiet. Empty without dated plays.
    pub fn back_in_rotation(&self) -> Vec<ArtistRow> {
        let rowids = self.db.taste().map(|t| t.back_in_rotation.clone()).unwrap_or_default();
        self.db.artists_by_rowid(&rowids).unwrap_or_default().into_iter().map(|(r, a)| artist_row(Some(r), a)).collect()
    }

    /// Today's "Because you've been playing X" from stored data, without asking the server.
    pub fn because_row(&self) -> Option<BecauseRow> {
        let day = navia_core::library::taste::local_day(navia_core::library::taste::now_unix());
        because_from(&self.db, self.db.because(day).ok()?)
    }

    /// Today's "World of" artist, one of the top five by recent taste with at least four albums. It stays the same for
    /// the local day.
    pub fn world_of_artist(&self) -> Option<RankedArtistRow> {
        let day = navia_core::library::taste::local_day(navia_core::library::taste::now_unix());
        self.db.world_of(day).ok().flatten().map(to_ranked)
    }

    pub fn forgotten_albums(&self, min_plays: u32, played_before_unix: i64, limit: u32) -> Vec<AlbumRow> {
        self.db.forgotten_albums(min_plays, &iso_timestamp(played_before_unix), limit).unwrap_or_default().into_iter().map(|a| album_row(None, a)).collect()
    }

    pub fn deep_cuts(&self, top_artists: u32, limit: u32) -> Vec<AlbumRow> {
        self.db.deep_cuts(top_artists, limit).unwrap_or_default().into_iter().map(|a| album_row(None, a)).collect()
    }

    /// "Your Top Songs", what was played in about the last six months. See `library::recent`.
    pub fn top_songs_local(&self, limit: u32) -> Vec<SongRow> {
        let rowids = self.db.top_song_rowids(limit).unwrap_or_default();
        self.songs_by_rowid(rowids)
    }

    pub fn genres(&self) -> Vec<GenreRow> {
        self.db.genre_counts().unwrap_or_default().into_iter().map(|g| GenreRow { name: g.name, songs: g.songs }).collect()
    }

    /// The Genres page from the synced library. Call off the main thread.
    pub fn genre_page(&self) -> GenrePageRow {
        let page = self.db.genre_page().unwrap_or_default();
        let row = |g: navia_core::library::genres::GenreEntry| GenreEntryRow { name: g.name, albums: g.albums, cover_album_ids: g.covers.into_iter().map(|c| c.id).collect() };
        GenrePageRow { top: page.top.into_iter().map(row).collect(), all: page.all.into_iter().map(row).collect(), browse: page.browse.into_iter().map(row).collect() }
    }

    pub fn playlists(&self) -> Vec<PlaylistRow> {
        self.db.playlists().unwrap_or_default().into_iter().map(|p: Playlist| playlist_row(p)).collect()
    }

    pub fn playlist_songs(&self, id: String) -> Vec<SongRow> {
        self.db.playlist(&id).ok().flatten().map(|p| p.entry.into_iter().map(|s| song_row(None, s)).collect()).unwrap_or_default()
    }

    pub fn search(&self, query: String, limit_per_kind: u32) -> SearchResultsRow {
        let r = self.db.search(&query, limit_per_kind, None).unwrap_or_default();
        SearchResultsRow {
            artists: r.artists.into_iter().map(|a| artist_row(None, a)).collect(),
            albums: r.albums.into_iter().map(|a| album_row(None, a)).collect(),
            songs: r.songs.into_iter().map(|s| song_row(None, s)).collect(),
            best: r.best.map(Into::into),
        }
    }

    pub fn starred_song_ids(&self) -> Vec<String> {
        self.db.starred_ids(StarTable::Song).unwrap_or_default()
    }

    pub fn starred_album_ids(&self) -> Vec<String> {
        self.db.starred_ids(StarTable::Album).unwrap_or_default()
    }

    pub fn starred_artist_ids(&self) -> Vec<String> {
        self.db.starred_ids(StarTable::Artist).unwrap_or_default()
    }

    pub fn username(&self) -> String {
        self.client.credentials().username().to_string()
    }

    pub fn is_admin(&self) -> Option<bool> {
        *self.admin.lock().unwrap()
    }

    pub fn reports_plays(&self) -> bool {
        self.report_plays.load(Ordering::SeqCst)
    }

    /// Private mode turns off reports and queue sync. Reporting also needs `report_plays`.
    pub fn set_private_mode(&self, private: bool) {
        self.private_mode.store(private, Ordering::SeqCst);
        let quiet = self.quiet();
        let reporter = self.reporter.clone();
        self.rt.spawn(async move { reporter.set_private(quiet).await });
    }

    /// "Share what I'm playing", opt-in. Playback reports go out either way, under the marked client name while on.
    pub fn set_sharing(&self, on: bool) {
        let reporter = self.reporter.clone();
        self.rt.spawn(async move { reporter.set_sharing(on).await });
    }

    /// Every `getNowPlaying` entry, for [`Self::listening_now`] and [`follow_step`]. Live states only where the server
    /// has `playbackReport`. Read-only.
    pub async fn now_playing(&self) -> Result<Vec<NowPlayingRow>, NaviaError> {
        // Test runs can read `getNowPlaying` entries from a JSON file on each check, so following can be tested without
        // real people.
        if self.test_run
            && let Some(path) = std::env::var_os("NAVIA_TEST_NOW_PLAYING")
        {
            let text = std::fs::read(path).unwrap_or_default();
            return Ok(navia_core::listen_along::from_json(&text).into_iter().map(|e| self.now_playing_row(e)).collect());
        }
        let client = self.client.clone();
        let live = self.features.get().is_some_and(|f| f.playback_report);
        let entries = self.on_runtime(async move { navia_core::listen_along::fetch(&client, live).await }).await??;
        Ok(entries.into_iter().map(|e| self.now_playing_row(e)).collect())
    }

    /// Listening Now from [`Self::now_playing`]: the other people who share, one row each. Empty unless `active`,
    /// meaning this listener shares and isn't private.
    pub fn listening_now(&self, entries: Vec<NowPlayingRow>, active: bool) -> Vec<NowPlayingRow> {
        let core: Vec<navia_core::listen_along::NowPlaying> = entries.iter().map(NowPlayingRow::core).collect();
        let shown = navia_core::listen_along::people(&core, &self.username(), active);
        shown.iter().filter_map(|e| entries.iter().find(|r| r.matches(e)).cloned()).collect()
    }

    /// The listener's queue, current song or position changed. Autoplay songs are never included. Song changes are
    /// saved a few seconds later, and a position alone waits for [`Self::flush_queue`]. Nothing is saved while private.
    pub fn note_queue(&self, song_ids: Vec<String>, index: u32, position_ms: u64) {
        self.queue_sync.note(QueueState { song_ids, index: index as usize, position_ms });
    }

    /// The listener continued or dismissed the offered queue, so it isn't offered again.
    pub fn answer_queue_offer(&self) {
        if let Some(offer) = self.queue_offer.lock().unwrap().take() {
            self.queue_sync.answered(&offer);
        }
    }

    /// Turns scrobbling on or off from Settings, which allows writes and starts reporting. Off by default, and a test
    /// run can't turn it on.
    pub fn set_reporting(&self, on: bool) {
        if self.test_run && on {
            return;
        }
        self.report_plays.store(on, Ordering::SeqCst);
        self.client.set_write_policy(if on { WritePolicy::Allowed } else { WritePolicy::ReadOnly });
        self.set_private_mode(self.private_mode.load(Ordering::SeqCst));
    }

    /// Get Info for a song by server id. None when it isn't in the library. The playing file's details come from
    /// `audio_file` and the player's `delivered_label`.
    pub fn song_details(&self, id: String) -> Option<SongDetailsRow> {
        let s = self.db.song_info(&id).ok().flatten()?;
        // The format name comes from the shared quality logic, the same as the badge.
        let format = s.suffix.clone().filter(|x| !x.trim().is_empty()).map(|suffix| {
            navia_core::audio_quality::FileFacts { suffix, kbps: s.bit_rate_kbps, bit_depth: s.bit_depth, sample_rate: s.sampling_rate, channels: s.channel_count }.format_name()
        });
        Some(SongDetailsRow {
            format,
            id: s.id,
            title: s.title,
            album: s.album.as_deref().map(|a| navia_core::display::song_album_name(s.album_id.as_deref(), a)),
            album_id: s.album_id,
            artist: s.artist,
            artist_id: s.artist_id,
            artists: credit_names(s.artists),
            track: s.track,
            disc: s.disc,
            year: s.year,
            genres: s.genres,
            duration_seconds: s.duration_s,
            size: s.size,
            suffix: s.suffix,
            content_type: s.content_type,
            bit_rate_kbps: s.bit_rate_kbps,
            bit_depth: s.bit_depth,
            sampling_rate: s.sampling_rate,
            channel_count: s.channel_count,
            path: s.path,
            bpm: s.bpm,
            replay_gain: s.replay_gain.map(|g| ReplayGainRow { track_gain: g.track_gain, album_gain: g.album_gain, track_peak: g.track_peak, album_peak: g.album_peak }),
            isrc: s.isrc,
            music_brainz_id: s.music_brainz_id,
            comment: s.comment,
            credits: s.credits.into_iter().map(|c| CreditRow { role: c.role, names: credit_names(c.names) }).collect(),
            explicit_status: s.explicit_status,
            created: s.created,
            play_count: s.play_count,
            played: s.played,
            starred: s.starred,
        })
    }

    /// Get Info for an album. Totals come from its songs in the library.
    pub fn album_details(&self, id: String) -> Option<AlbumDetailsRow> {
        let a = self.db.album_info(&id).ok().flatten()?;
        Some(AlbumDetailsRow {
            id: a.id,
            // Shown without the version Navidrome appends. `version` is the label, None for explicit or clean.
            name: navia_core::display::album_name(&a.name, a.version.as_deref()).into_owned(),
            artist: a.artist,
            artist_id: a.artist_id,
            artists: credit_names(a.artists),
            version: navia_core::display::edition(a.version.as_deref()),
            record_labels: a.record_labels,
            release_types: a.release_types,
            release_date: item_date(a.release_date),
            original_release_date: item_date(a.original_release_date),
            year: a.year,
            is_compilation: a.is_compilation,
            music_brainz_id: a.music_brainz_id,
            genres: a.genres,
            explicit_status: a.explicit_status,
            song_count: a.song_count,
            duration_seconds: a.duration_s,
            disc_count: a.disc_count,
            size: a.size,
            created: a.created,
            play_count: a.play_count,
            played: a.played,
            starred: a.starred,
        })
    }

    /// Get Info for a playlist. Smart rules come from `playlist_rules`.
    pub fn playlist_details(&self, id: String) -> Option<PlaylistDetailsRow> {
        let p = self.db.playlist_info(&id).ok().flatten()?;
        Some(PlaylistDetailsRow { id: p.id, name: p.name, owner: p.owner, public: p.public, comment: p.comment, created: p.created, changed: p.changed, song_count: p.song_count, duration_seconds: p.duration_s, size: p.size, readonly: p.readonly, smart: p.smart, valid_until: p.valid_until })
    }

    /// Get Info for an artist. The biography comes from `artist_info`.
    pub fn artist_details(&self, id: String) -> Option<ArtistDetailsRow> {
        let a = self.db.artist_info(&id).ok().flatten()?;
        Some(ArtistDetailsRow { id: a.id, name: a.name, album_count: a.album_count, song_count: a.song_count, roles: a.roles, music_brainz_id: a.music_brainz_id, play_count: a.play_count, starred: a.starred })
    }

    /// This song's original file as the server describes it, for the quality badge. None when the song isn't stored or
    /// the server didn't say.
    pub fn audio_file(&self, rowid: u32) -> Option<AudioFileRow> {
        let (_, song) = self.db.songs_by_rowid(&[rowid]).ok()?.into_iter().next()?;
        let f = navia_core::audio_quality::FileFacts::from_song(&song)?;
        Some(AudioFileRow {
            format: f.format_name(),
            tier: match f.tier() {
                navia_core::audio_quality::Tier::HiRes => QualityTier::HiRes,
                navia_core::audio_quality::Tier::Lossless => QualityTier::Lossless,
                navia_core::audio_quality::Tier::Lossy => QualityTier::Lossy,
            },
            kbps: f.kbps,
            bit_depth: f.bit_depth,
            sample_rate: f.sample_rate,
            channels: f.channels,
        })
    }

    /// Queues songs for offline play by server id. Only a finished file counts as downloaded.
    pub fn download_songs(&self, ids: Vec<String>) -> u32 {
        self.downloads.enqueue(ids.into_iter().map(|id| DownloadRequest { id, suffix: None, size: None }).collect()) as u32
    }

    pub fn cancel_downloads(&self, ids: Vec<String>) {
        self.downloads.cancel(&ids);
    }

    pub fn remove_downloads(&self, ids: Vec<String>) {
        let _ = self.downloads.remove(&ids);
    }

    pub fn downloaded_song_ids(&self) -> Vec<String> {
        self.downloads.downloaded_ids().unwrap_or_default()
    }

    /// Every finished download with its real size on disk.
    pub fn downloaded_entries(&self) -> Vec<DownloadedRow> {
        self.downloads.entries().unwrap_or_default().into_iter().map(|(id, bytes)| DownloadedRow { id, bytes }).collect()
    }

    pub fn downloads_in_progress(&self) -> Vec<String> {
        self.downloads.in_progress()
    }

    pub fn set_download_listener(&self, listener: Box<dyn DownloadListener>) {
        self.downloads.add_listener(move |event: &DownloadEvent| {
            listener.on_event(match event.clone() {
                DownloadEvent::Progress { id, done, total } => DownloadEventRow::Progress { id, done, total },
                DownloadEvent::Finished { id, size } => DownloadEventRow::Finished { id, size },
                DownloadEvent::Failed { id, message } => DownloadEventRow::Failed { id, message },
                DownloadEvent::Removed { id } => DownloadEventRow::Removed { id },
            });
        });
    }

    /// Turns on a playlist's or album's Download switch and starts its songs, even on a metered connection since the
    /// user asked. A playlist whose songs aren't stored yet is fetched first, except in test runs where it waits.
    pub async fn download_collection(&self, kind: CollectionKind, id: String) -> Result<(), NaviaError> {
        if kind == CollectionKind::Playlist && !self.test_run && self.db.collection_needs_fetch(kind.core(), &id)? {
            let (client, db, fetch) = (self.client.clone(), self.db.clone(), id.clone());
            self.on_runtime(async move { Syncer::new(client, db, SyncOptions::default()).refresh_playlist(&fetch).await }).await??;
        }
        Ok(self.downloads.collection_download_on(kind.core(), &id)?)
    }

    /// Turns the switch off. `keep_songs` keeps its songs as single downloads, otherwise songs nothing else holds are
    /// deleted.
    pub fn stop_collection_download(&self, kind: CollectionKind, id: String, keep_songs: bool) -> Result<(), NaviaError> {
        Ok(self.downloads.collection_download_off(kind.core(), &id, keep_songs)?)
    }

    pub fn collection_removal_plan(&self, kind: CollectionKind, id: String) -> RemovalPlanRow {
        let plan = self.downloads.removal_plan(kind.core(), &id).unwrap_or_default();
        RemovalPlanRow { remove: plan.remove, remove_bytes: plan.remove_bytes, keep: plan.keep }
    }

    /// The switch's status. None when it's off.
    pub fn collection_status(&self, kind: CollectionKind, id: String) -> Option<CollectionStatusRow> {
        let s = self.downloads.collection_status(kind.core(), &id).ok().flatten()?;
        let phase = match s.phase {
            navia_core::downloads::CollectionPhase::Done => CollectionPhase::Done,
            navia_core::downloads::CollectionPhase::Downloading => CollectionPhase::Downloading,
            navia_core::downloads::CollectionPhase::Syncing => CollectionPhase::Syncing,
            navia_core::downloads::CollectionPhase::Metered => CollectionPhase::Metered,
            navia_core::downloads::CollectionPhase::Offline => CollectionPhase::Offline,
            navia_core::downloads::CollectionPhase::Waiting => CollectionPhase::Waiting,
        };
        Some(CollectionStatusRow { phase, have: s.have, total: s.total, missing: s.missing, failed: s.failed, progress: s.progress, bytes: s.bytes })
    }

    /// Kept collections, most recently turned on first.
    pub fn synced_collections(&self) -> Vec<SyncedCollectionRow> {
        self.downloads.synced_collections().unwrap_or_default().into_iter().map(|c| SyncedCollectionRow { kind: CollectionKind::from_core(c.kind), id: c.id, since_ms: c.since_ms }).collect()
    }

    /// A kept collection's songs on the device, in its order.
    pub fn collection_songs_on_device(&self, kind: CollectionKind, id: String) -> Vec<String> {
        self.downloads.collection_songs_on_device(kind.core(), &id).unwrap_or_default()
    }

    /// Songs downloaded on their own.
    pub fn single_download_ids(&self) -> Vec<String> {
        self.downloads.single_download_ids().unwrap_or_default()
    }

    pub fn is_single_download(&self, id: String) -> bool {
        self.downloads.is_single_download(&id)
    }

    /// Brings kept collections up to date now and retries failed songs. The core already does this after library and
    /// playlist changes and on reconnect.
    pub fn sync_collections(&self) {
        let _ = self.downloads.sync_collections();
    }

    /// Clear All on the Downloads page. Turns every switch off and deletes every download.
    pub fn clear_all_downloads(&self) {
        let _ = self.downloads.clear_all();
    }

    pub fn set_collection_listener(&self, listener: Box<dyn CollectionListener>) {
        self.downloads.add_collection_listener(move |event: &CollectionEvent| listener.on_collection_changed(CollectionKind::from_core(event.kind), event.id.clone()));
    }

    /// "Download on metered connections". Off by default.
    pub fn download_on_metered(&self) -> bool {
        self.downloads.download_on_metered()
    }

    pub fn set_download_on_metered(&self, on: bool) -> Result<(), NaviaError> {
        Ok(self.downloads.set_download_on_metered(on)?)
    }

    /// Whether the server can be reached, from the app's own check. Automatic downloads wait while offline, and coming
    /// back syncs kept collections and retries.
    pub fn set_online(&self, online: bool) {
        self.downloads.set_online(online);
    }

    /// the offline switch: while on, nothing is sent to the server and every request fails as
    /// if it couldn't be reached. call it before `start_background` so a launch with it on
    /// doesn't try the server; turning it off needs the app's usual reconnect.
    pub fn set_offline(&self, on: bool) {
        self.client.set_offline(on);
        if on {
            self.downloads.set_online(false);
        }
    }

    pub fn is_offline(&self) -> bool {
        self.client.is_offline()
    }

    /// Sends a star change when writes are allowed. Without them nothing leaves the app; the
    /// caller keeps the change locally.
    pub fn send_star(&self, table: StarKind, id: String, on: bool) {
        if !self.report_plays.load(Ordering::SeqCst) {
            return;
        }
        let (client, db) = (self.client.clone(), self.db.clone());
        self.rt.spawn(async move {
            let target = match table {
                StarKind::Song => StarTarget::Song(&id),
                StarKind::Album => StarTarget::Album(&id),
                StarKind::Artist => StarTarget::Artist(&id),
            };
            let sent = if on { client.star(target).await } else { client.unstar(target).await };
            if sent.is_ok() {
                let t = match table {
                    StarKind::Song => StarTable::Song,
                    StarKind::Album => StarTable::Album,
                    StarKind::Artist => StarTable::Artist,
                };
                let _ = db.set_starred(t, &id, on);
            }
        });
    }

    /// Starts finding AirPlay receivers. `config_dir` holds the pairings, with the identity in the Keychain through
    /// `secrets`. `listener` gets remote buttons and list changes.
    pub fn start_airplay(&self, config_dir: String, secrets: Box<dyn crate::airplay_api::SecretStoreCallback>, listener: Box<dyn crate::airplay_api::AirPlayListener>) -> Result<Arc<crate::airplay_api::NaviaAirPlay>, NaviaError> {
        use navia_core::audio::airplay::{AirPlay, AirPlayConfig, ClientNowPlaying, DEFAULT_PORTS};
        let listener: Arc<dyn crate::airplay_api::AirPlayListener> = Arc::from(listener);
        let on_remote = listener.clone();
        let config = AirPlayConfig {
            config_dir: config_dir.into(),
            secrets: Some(Arc::new(crate::airplay_api::KeychainSecrets(secrets))),
            ports: DEFAULT_PORTS,
            now_playing: Some(Arc::new(ClientNowPlaying::new(self.client.clone()))),
            remote: Some(Arc::new(move |cmd| {
                if let Some(c) = crate::airplay_api::remote(cmd) {
                    on_remote.on_remote(c);
                }
            })),
        };
        let airplay = AirPlay::new(config)?;
        let on_changed = listener.clone();
        airplay.start_discovery(move || on_changed.on_changed())?;
        Ok(crate::airplay_api::new(airplay, listener))
    }

    /// The audio player for this server, with AirPlay receivers alongside the system's devices. Pick one with
    /// `set_output_device` and its `airplay:<id>`.
    pub fn make_player_with_airplay(&self, policy: OutputPolicy, airplay: Arc<crate::airplay_api::NaviaAirPlay>) -> Result<Arc<NaviaPlayer>, NaviaError> {
        let _guard = self.rt.enter();
        let resolver = Arc::new(ServerResolver::new(self.client.clone())?.with_downloads(self.downloads.clone()));
        let config = PlayerConfig { policy: policy.into(), ..Default::default() };
        let player = Player::with_backend(resolver, Box::new(airplay.inner.router()), config)?;
        crate::airplay_api::forward_device_volume(&player, &airplay);
        player.set_album_lookup(navia_core::audio::player::library_album_lookup(self.db.clone()));
        player.set_edge_store(navia_core::audio::player::library_edge_store(self.db.clone()));
        attach_reporting(&player, self.reporter.clone());
        navia_core::engine::playlog::attach_play_log(&player, self.db.clone());
        Ok(Arc::new(NaviaPlayer { inner: player, capture: None }))
    }

    /// The audio player for this server. `capture` writes to memory instead of a device for sample-accurate checks.
    /// Save it with [`NaviaPlayer::save_capture`].
    pub fn make_player(&self, policy: OutputPolicy, capture: bool) -> Result<Arc<NaviaPlayer>, NaviaError> {
        let _guard = self.rt.enter();
        let resolver = Arc::new(ServerResolver::new(self.client.clone())?.with_downloads(self.downloads.clone()));
        let config = PlayerConfig { policy: policy.into(), ..Default::default() };
        let mut buffer: Option<Arc<CaptureBuffer>> = None;
        let backend: Box<dyn OutputBackend> = if capture {
            let backend = CaptureBackend { speed: 1.0, rates: (8_000, 384_000), ..Default::default() };
            buffer = Some(backend.capture.clone());
            Box::new(backend)
        } else {
            Box::new(CpalBackend::default())
        };
        let player = Player::with_backend(resolver, backend, config)?;
        player.set_album_lookup(navia_core::audio::player::library_album_lookup(self.db.clone()));
        player.set_edge_store(navia_core::audio::player::library_edge_store(self.db.clone()));
        // The reporter stays private while reporting is off, so this sends nothing until it's on.
        attach_reporting(&player, self.reporter.clone());
        // The play log is local only.
        navia_core::engine::playlog::attach_play_log(&player, self.db.clone());
        Ok(Arc::new(NaviaPlayer { inner: player, capture: buffer }))
    }
}

#[derive(uniffi::Enum)]
pub enum StarKind {
    Song,
    Album,
    Artist,
}

#[uniffi::export(async_runtime = "tokio")]
impl NaviaCore {
    /// The cover for an album id or an `ar-`/`pl-` id, on disk, at the given size or the original.
    pub async fn cover(&self, key: String, size: Option<u32>) -> Result<CoverArtRow, NaviaError> {
        let artwork = self.artwork.clone();
        let id = self.cover_id(&key);
        let cover = self.on_runtime(async move { artwork.cover(&id, size).await }).await??;
        Ok(CoverArtRow { path: cover.path.display().to_string(), width: cover.width, height: cover.height, tint_rgb: cover.tint.to_vec(), placeholder: cover.placeholder })
    }

    /// the cover's frames for display at `size` pixels (longest side), or `None` when it's still.
    /// ask only where art is large (page headers, player, full screen); elsewhere show `cover`.
    pub async fn animated_artwork(&self, key: String, size: u32) -> Result<Option<Arc<NaviaAnimatedArt>>, NaviaError> {
        let artwork = self.artwork.clone();
        let id = self.cover_id(&key);
        let art = self.on_runtime(async move { artwork.animated_artwork(&id, size).await }).await??;
        Ok(art.map(|inner| Arc::new(NaviaAnimatedArt { inner })))
    }

    /// from the cache only: true when the cover plays as an animation, `None` if not known yet.
    pub fn cover_is_animated(&self, key: String) -> Option<bool> {
        self.artwork.is_animated(&self.cover_id(&key))
    }

    /// From the lyrics cache while fresh, else the server, else whatever is cached.
    pub async fn lyrics(&self, song_id: String, artist: String, title: String) -> Result<Option<LyricsRow>, NaviaError> {
        let (client, cache) = (self.client.clone(), self.lyrics.clone());
        let features = self.features.get().copied();
        let found = self
            .on_runtime(async move {
                // Servers without the extension still answer the classic endpoint, unsynced.
                let lyrics = navia_core::lyrics::lyrics_for(&client, Some(&cache), &song_id, Some((&artist, &title)), features).await?;
                Some(LyricsRow { synced: lyrics.synced, lines: lyrics.lines.into_iter().map(LyricLineRow::from).collect() })
            })
            .await?;
        Ok(found)
    }

    /// An artist's top songs without duplicates, by id where the server has `topSongsByArtistId`, else by name. Empty
    /// in test runs and on servers without the lookup.
    pub async fn top_songs(&self, artist_id: String, artist_name: String) -> Result<Vec<SongRow>, NaviaError> {
        if !self.external_allowed() {
            return Ok(Vec::new());
        }
        let (client, db) = (self.client.clone(), self.db.clone());
        let by_id = self.features.get().is_some_and(|f| f.top_songs_by_id);
        let day = local_day();
        let ids = self.on_runtime(async move { navia_core::library::essentials::top_songs(&client, &db, &artist_id, &artist_name, by_id, day).await }).await??;
        Ok(self.songs_by_rowid(self.db.top_song_rows(&ids).unwrap_or_default()))
    }

    /// home's Essentials from what's stored for today: the artists' ids, best lately first.
    pub fn essential_artists(&self) -> Vec<String> {
        self.db.essentials(local_day()).unwrap_or_default().into_iter().map(|e| e.artist_id).collect()
    }

    /// Essentials for today, asking the server about the top artists not asked yet today, a few at
    /// once; each answer is kept for the day, so their songs come from `top_songs` at once. empty
    /// in test runs.
    pub async fn refresh_essential_artists(&self) -> Vec<String> {
        if !self.external_allowed() {
            return self.essential_artists();
        }
        let (client, db) = (self.client.clone(), self.db.clone());
        let by_id = self.features.get().is_some_and(|f| f.top_songs_by_id);
        let day = local_day();
        let found = self.on_runtime(async move { navia_core::library::essentials::refresh_essentials(&client, &db, by_id, day).await }).await;
        match found {
            Ok(Ok(list)) => list.into_iter().map(|e| e.artist_id).collect(),
            _ => self.essential_artists(),
        }
    }

    /// asks the server what changed since the last look and brings in only that (the library
    /// after a new scan, playlists, stars, play counts); near-instant when nothing did. for the
    /// refresh button on every page that has one, library pages included. sends `SyncDone` when
    /// anything stored changed, so lists reload as after the background sync; a library sync it
    /// starts shows on the sync meter.
    pub async fn check_for_changes(&self) -> Result<ChangesRow, NaviaError> {
        let syncer = Syncer::new(self.client.clone(), self.db.clone(), self.sync_options());
        let c = self.on_runtime(async move { syncer.check_for_changes(&|_| {}).await }).await??;
        if c.any()
            && let Some(l) = self.listener.lock().unwrap().clone()
            && let Ok(l) = l.lock()
        {
            l.on_event(CoreEvent::SyncDone);
        }
        Ok(ChangesRow { library: c.library, server_scanning: c.server_scanning, sync_running: c.sync_running, playlists: c.playlists, stars: c.stars, plays: c.plays })
    }

    /// the artist's albums by release type: Navidrome's list of albums they're credited on, else the library's own songs.
    pub async fn artist_discography(&self, artist_id: String) -> Result<DiscographyRow, NaviaError> {
        let (client, db) = (self.client.clone(), self.db.clone());
        let use_server = discography::server_list_allowed(self.test_run);
        let found = self.on_runtime(async move { discography::artist_discography(&client, &db, &artist_id, use_server).await }).await??;
        Ok(DiscographyRow {
            sections: found.sections.into_iter().map(|(section, albums)| DiscographySectionRow { section: section.into(), rowids: albums.into_iter().map(|a| a.rowid).collect() }).collect(),
            from_server: found.source == discography::DiscographySource::Server,
            not_synced: found.not_synced as u32,
        })
    }

    /// A newer queue saved on the server by another client, for a "Continue from …" offer in the player. None when
    /// there's nothing worth offering. Read-only.
    pub async fn remote_queue_offer(&self) -> Option<RemoteQueueRow> {
        let sync = self.queue_sync.clone();
        let offer = self.on_runtime(async move { sync.remote_offer().await.ok().flatten() }).await.ok().flatten()?;
        // Use library rows where this device has the song, so downloads and stars apply. Otherwise the server's data,
        // which plays the same.
        let songs = offer.songs.iter().cloned().map(|s| (self.db.song_rowids(std::slice::from_ref(&s.id)).ok().and_then(|r| r.first().copied()), s)).map(|(r, s)| song_row(r, s)).collect();
        let row = RemoteQueueRow { device: offer.changed_by.clone(), changed_ms: offer.changed_ms, songs, index: offer.index as u32, position_ms: offer.position_ms };
        *self.queue_offer.lock().unwrap() = Some(offer);
        Some(row)
    }

    /// Saves the current queue if it differs from the last save, without waiting.
    pub async fn flush_queue(&self) {
        let sync = self.queue_sync.clone();
        let _ = self.on_runtime(async move { sync.flush().await }).await;
    }

    /// For quitting. Saves the queue and ends this player's now-playing entry, waiting at most two seconds.
    pub async fn sync_before_quit(&self) {
        let (sync, reporter) = (self.queue_sync.clone(), self.reporter.clone());
        let work = async move {
            let _ = tokio::time::timeout(Duration::from_secs(2), async {
                let _ = sync.flush().await;
                reporter.stop_live().await;
            })
            .await;
        };
        let _ = self.on_runtime(work).await;
    }

    pub async fn similar_songs(&self, artist_id: String, limit: u32) -> Result<Vec<SongRow>, NaviaError> {
        if !self.external_allowed() {
            return Ok(Vec::new());
        }
        let client = self.client.clone();
        let songs = self.on_runtime(async move { client.similar_songs2(&artist_id, limit).await }).await?.unwrap_or_default();
        Ok(dedup(songs))
    }

    pub async fn artist_info(&self, artist_id: String) -> Result<ArtistInfoRow, NaviaError> {
        if !self.external_allowed() {
            return Ok(ArtistInfoRow { biography: String::new(), similar: Vec::new() });
        }
        // One request per artist per day, shared with Similar Artists (`navia_core::similar_artists`).
        let (client, db) = (self.client.clone(), self.db.clone());
        let info = self.on_runtime(async move { navia_core::similar_artists::artist_page_info(&client, &db, &artist_id).await }).await?.unwrap_or_default();
        Ok(ArtistInfoRow { biography: plain_text(&info.biography.unwrap_or_default()), similar: info.similar_artist.into_iter().map(|a| a.name).collect() })
    }

    /// The artist page's Similar Artists: the server's similar library artists, with Deezer's merged in when enabled.
    /// Asks the server at most once a day per artist. Test runs only read what's stored.
    pub async fn similar_artists(&self, artist_id: String) -> Vec<ArtistRow> {
        let (client, db, ask) = (self.client.clone(), self.db.clone(), self.external_allowed());
        let found = self
            .on_runtime(async move { navia_core::similar_artists::similar_artists(&client, &db, navia_core::deezer::Deezer::shared(), &artist_id, ask).await })
            .await
            .unwrap_or_default();
        found.into_iter().map(|(r, a)| artist_row(Some(r), a)).collect()
    }

    /// the album's Heat column: its songs' Deezer ranks, with the listener's switch on. asks Deezer
    /// at most once a week per album (a miss once a day); test runs only read what's kept.
    pub async fn album_heat(&self, album_id: String) -> Vec<SongHeatRow> {
        if !self.external_allowed() {
            return self.kept_album_heat(album_id);
        }
        let db = self.db.clone();
        let heat = self.on_runtime(async move { navia_core::album_heat::album_heat(&db, navia_core::deezer::Deezer::shared(), &album_id).await }).await.unwrap_or_default();
        heat_rows(heat)
    }

    /// the Heat column from what's kept only (any age; no request), for showing at once.
    pub fn kept_album_heat(&self, album_id: String) -> Vec<SongHeatRow> {
        heat_rows(navia_core::album_heat::kept_album_heat(&self.db, &album_id).unwrap_or_default())
    }

    /// "More by <album artist>" under an album, from the library: `None` for Various Artists or
    /// when the album artist has nothing else.
    pub fn more_by(&self, album_id: String) -> Option<MoreByRow> {
        let found = self.db.more_by(&album_id).ok().flatten()?;
        let artist = self.db.artist(&found.artist_id).ok().flatten()?;
        let artist_rowid = self.db.artist_rowids(std::slice::from_ref(&found.artist_id)).ok().and_then(|r| r.first().copied());
        let rowids: Vec<u32> = found.albums.iter().map(|a| a.rowid).collect();
        let mut by_row: std::collections::HashMap<u32, navia_core::client::Album> = self.db.albums_by_rowid(&rowids).ok()?.into_iter().collect();
        let albums: Vec<AlbumRow> = rowids.into_iter().filter_map(|r| by_row.remove(&r).map(|a| album_row(Some(r), a))).collect();
        (!albums.is_empty()).then(|| MoreByRow { artist: artist_row(artist_rowid, artist), albums, section: found.section.into() })
    }

    /// Asks the server for today's seeds' similar artists when they aren't stored yet, a few calls a day at most, then
    /// returns the row. Test runs only use what's stored.
    pub async fn refresh_because(&self) -> Option<BecauseRow> {
        let day = navia_core::library::taste::local_day(navia_core::library::taste::now_unix());
        if !self.external_allowed() {
            return self.because_row();
        }
        let (client, db) = (self.client.clone(), self.db.clone());
        let state = self.on_runtime(async move { navia_core::library::taste::refresh_because(&client, &db, day).await }).await.ok()?.ok()?;
        because_from(&self.db, state)
    }

    /// Home's Artist Radio row (`navia_core::stations`), today's stations with the best seed first. Asks the server,
    /// and Deezer when enabled, at most once per candidate per day. Test runs only read what's stored. Call off the
    /// main thread.
    pub async fn artist_stations(&self) -> Vec<ArtistStationRow> {
        let (client, db, ask) = (self.client.clone(), self.db.clone(), self.external_allowed());
        let found = self
            .on_runtime(async move { navia_core::stations::artist_stations(&client, &db, navia_core::deezer::Deezer::shared(), ask).await })
            .await
            .unwrap_or_default();
        found
            .into_iter()
            .map(|s| ArtistStationRow { seed: artist_row(Some(s.seed.0), s.seed.1), neighbours: s.neighbours.into_iter().map(|(r, a)| artist_row(Some(r), a)).collect() })
            .collect()
    }

    /// For You from stored data (`library::for_you`). Fast, so call it per visit and after a sync.
    pub fn for_you(&self) -> ForYouRow {
        let day = navia_core::library::taste::local_day(navia_core::library::taste::now_unix());
        let Ok(page) = self.db.for_you(day) else {
            return ForYouRow { mixes: Vec::new(), new_to_you: Vec::new(), unplayed: Vec::new(), forgotten: Vec::new(), on_this_day: Vec::new(), not_played_recently: Vec::new(), needs_refresh: false, building: false };
        };
        let albums = |rows: &[u32]| self.db.albums_by_rowid(rows).unwrap_or_default().into_iter().map(|(r, a)| album_row(Some(r), a)).collect::<Vec<_>>();
        ForYouRow {
            mixes: page.mixes.into_iter().map(|m| self.mix_row(m)).collect(),
            new_to_you: albums(&page.new_to_you),
            unplayed: albums(&page.unplayed),
            forgotten: self.songs_by_rowid(page.forgotten),
            on_this_day: albums(&page.on_this_day),
            not_played_recently: albums(&page.not_played_recently),
            needs_refresh: page.needs_refresh,
            building: page.building,
        }
    }

    /// One of today's mixes by its address, for the mix page. None once it's gone on a new day.
    pub fn mix(&self, id: String) -> Option<MixRow> {
        let day = navia_core::library::taste::local_day(navia_core::library::taste::now_unix());
        self.db.mix(day, &id).ok().flatten().map(|m| self.mix_row(m))
    }

    pub async fn album_notes(&self, album_id: String) -> Result<String, NaviaError> {
        if !self.external_allowed() {
            return Ok(String::new());
        }
        let client = self.client.clone();
        let info = self.on_runtime(async move { client.album_info2(&album_id).await }).await?.unwrap_or_default();
        Ok(plain_text(&info.notes.unwrap_or_default()))
    }

    pub async fn stations(&self) -> Result<Vec<StationRow>, NaviaError> {
        let client = self.client.clone();
        let list = self.on_runtime(async move { client.internet_radio_stations().await }).await?.unwrap_or_default();
        Ok(list.into_iter().filter_map(|s| s.stream_url.map(|url| StationRow { name: s.name, url })).collect())
    }

    /// A smart playlist's rules as Navidrome stores them, as JSON text. Empty in test runs because reading them logs
    /// in, except for the one playlist named in `NAVIA_TEST_PLAYLIST`.
    pub async fn playlist_rules(&self, id: String) -> Result<Option<String>, NaviaError> {
        if self.test_run && std::env::var("NAVIA_TEST_PLAYLIST").ok().as_deref() != Some(id.as_str()) {
            return Ok(None);
        }
        let client = self.client.clone();
        Ok(self.on_runtime(async move { client.playlist_rules(&id).await }).await??)
    }

    /// Creates a regular playlist and stores it, with a message to show when it fails.
    pub async fn create_playlist(&self, name: String) -> Result<CreatedPlaylist, NaviaError> {
        let (client, db) = (self.writer.clone(), self.db.clone());
        let created = self.on_runtime(async move { Syncer::new(client, db, SyncOptions::default()).create_playlist(&name).await }).await?;
        Ok(match created {
            Ok(id) => CreatedPlaylist { id, problem: None },
            Err(navia_core::Error::WritesDisabled { .. }) => CreatedPlaylist { id: None, problem: Some("Test and read-only runs can't create playlists.".to_string()) },
            Err(e) => CreatedPlaylist { id: None, problem: Some(format!("Couldn't create the playlist: {e}")) },
        })
    }

    /// Saves a smart playlist's name and rules as Navidrome JSON, creating it when `id` is None, and stores the
    /// evaluated playlist. Only for the owner or an admin. Test and read-only runs are refused, except for the one
    /// playlist named in `NAVIA_TEST_PLAYLIST` (`new-smart` lets a test create one).
    pub async fn save_smart_playlist(&self, id: Option<String>, name: String, criteria: String) -> Result<SmartSaveResult, NaviaError> {
        let client = self.playlist_writer(id.as_deref().unwrap_or(NEW_SMART_TEST))?;
        let (db, admin) = (self.db.clone(), *self.admin.lock().unwrap());
        let saved = self.on_runtime(async move { Syncer::new(client, db, SyncOptions::default()).save_smart_playlist(id.as_deref(), &name, &criteria, admin).await }).await?;
        Ok(match saved {
            Ok(s) => SmartSaveResult { id: Some(s.id), rules: s.rules, problem: None },
            Err(e) => SmartSaveResult { id: None, rules: None, problem: Some(e.to_string()) },
        })
    }

    /// Whether this server lets the user upload and remove playlist images. False until connected, and on servers
    /// without the feature.
    pub fn can_change_playlist_images(&self) -> bool {
        let admin = *self.admin.lock().unwrap();
        self.image_support.lock().unwrap().as_ref().is_some_and(|s| s.allowed_for(admin))
    }

    /// Replaces a playlist's image with the file at `path`. Always returns a result to show.
    pub async fn set_playlist_image(&self, id: String, path: String) -> Result<PlaylistImageResult, NaviaError> {
        let (client, db) = (self.image_client(&id)?, self.db.clone());
        let change = self.on_runtime(async move { Syncer::new(client, db, SyncOptions::default()).set_playlist_image(&id, std::path::Path::new(&path)).await }).await?;
        Ok(image_result(change))
    }

    /// Removes a playlist's uploaded image, restoring the server's own cover.
    pub async fn remove_playlist_image(&self, id: String) -> Result<PlaylistImageResult, NaviaError> {
        let (client, db) = (self.image_client(&id)?, self.db.clone());
        let change = self.on_runtime(async move { Syncer::new(client, db, SyncOptions::default()).remove_playlist_image(&id).await }).await?;
        Ok(image_result(change))
    }

    /// Changes a playlist's name, description or Public setting. None leaves a field as is, and an empty description
    /// clears it. Returns a message when it didn't save. Test and read-only runs are refused, except for the playlist
    /// named in `NAVIA_TEST_PLAYLIST`.
    pub async fn update_playlist_details(&self, id: String, name: Option<String>, comment: Option<String>, public: Option<bool>) -> Result<Option<String>, NaviaError> {
        let (client, db) = (self.playlist_writer(&id)?, self.db.clone());
        let details = navia_core::client::PlaylistDetails { name, comment, public };
        let saved = self.on_runtime(async move { Syncer::new(client, db, SyncOptions::default()).update_playlist_details(&id, details).await }).await?;
        Ok(match saved {
            Ok(()) => None,
            Err(navia_core::Error::WritesDisabled { .. }) => Some("Test and read-only runs can't change playlists.".to_string()),
            Err(e) => Some(format!("The server didn't save the change: {e}")),
        })
    }

    /// deletes a playlist on the server (its owner or an admin), then the stored copy. returns the
    /// words to show when it didn't go, `None` when it did. test and read-only runs are refused,
    /// except for the one playlist a test names in `NAVIA_TEST_PLAYLIST`.
    pub async fn delete_playlist(&self, id: String) -> Result<Option<String>, NaviaError> {
        let (client, db) = (self.playlist_writer(&id)?, self.db.clone());
        let done = self.on_runtime(async move { Syncer::new(client, db, SyncOptions::default()).delete_playlist(&id).await }).await?;
        Ok(match done {
            Ok(()) => None,
            Err(navia_core::Error::WritesDisabled { .. }) => Some("Test and read-only runs can't change playlists.".to_string()),
            Err(e) => Some(format!("The server didn't delete it: {e}")),
        })
    }

    /// Saves a new song order for a playlist this user can edit (not smart, not read-only).
    /// `expected` is the order shown, `order` the new one; the playlist is fetched first and the
    /// save is refused if it changed on the server, and its fresh copy is stored either way.
    /// Test and read-only runs are refused, except for the one playlist a test names in
    /// `NAVIA_TEST_PLAYLIST`, which it created itself.
    pub async fn save_playlist_order(&self, id: String, expected: Vec<String>, order: Vec<String>) -> Result<PlaylistEditResult, NaviaError> {
        let (client, db) = (self.playlist_writer(&id)?, self.db.clone());
        let saved = self.on_runtime(async move { Syncer::new(client, db, SyncOptions::default()).save_playlist_order(&id, &expected, &order).await }).await?;
        Ok(edit_result(saved))
    }

    /// Playlists offered by Add to Playlist: the user's own regular playlists, most recently changed first.
    pub fn playlist_targets(&self) -> Vec<PlaylistRow> {
        self.db.playlist_targets().unwrap_or_default().into_iter().map(playlist_row).collect()
    }

    /// Which of `song_ids` the stored playlist already holds, in request order, so an add can warn about duplicates.
    pub fn already_in_playlist(&self, id: String, song_ids: Vec<String>) -> Vec<String> {
        self.db.already_in_playlist(&id, &song_ids).unwrap_or_default()
    }

    /// Appends songs to a regular playlist this user owns, then stores the server's copy. Refused in test and read-only
    /// runs like [`Self::save_playlist_order`].
    pub async fn add_to_playlist(&self, id: String, song_ids: Vec<String>) -> Result<PlaylistEditResult, NaviaError> {
        let (client, db, form) = (self.playlist_writer(&id)?, self.db.clone(), self.form_post());
        let done = self.on_runtime(async move { Syncer::new(client, db, SyncOptions::default()).add_to_playlist(&id, &song_ids, form).await }).await?;
        Ok(edit_result(done))
    }

    /// Removes the entries at these positions. `expected` is the order shown, and nothing is removed if the server's
    /// differs. The fresh copy is stored either way.
    pub async fn remove_from_playlist(&self, id: String, expected: Vec<String>, indexes: Vec<u32>) -> Result<PlaylistEditResult, NaviaError> {
        let (client, db, form) = (self.playlist_writer(&id)?, self.db.clone(), self.form_post());
        let indexes: Vec<usize> = indexes.into_iter().map(|i| i as usize).collect();
        let done = self.on_runtime(async move { Syncer::new(client, db, SyncOptions::default()).remove_from_playlist(&id, &expected, &indexes, form).await }).await?;
        Ok(edit_result(done))
    }

    /// Creates a regular playlist holding these songs and stores it.
    pub async fn create_playlist_with(&self, name: String, song_ids: Vec<String>) -> Result<CreatedPlaylist, NaviaError> {
        let (client, db, form) = (self.writer.clone(), self.db.clone(), self.form_post());
        let created = self.on_runtime(async move { Syncer::new(client, db, SyncOptions::default()).create_playlist_with(&name, &song_ids, form).await }).await?;
        Ok(match created {
            Ok(id) => CreatedPlaylist { id, problem: None },
            Err(navia_core::Error::WritesDisabled { .. }) => CreatedPlaylist { id: None, problem: Some("Test and read-only runs can't create playlists.".to_string()) },
            Err(e) => CreatedPlaylist { id: None, problem: Some(format!("Couldn't create the playlist: {e}")) },
        })
    }

    /// Fetches one playlist so its songs are stored, as when it's opened. Refused in test runs, since fetching a smart
    /// playlist changes its stamp on the server.
    pub async fn refresh_playlist(&self, id: String) -> Result<(), NaviaError> {
        if self.test_run {
            return Ok(());
        }
        let (client, db) = (self.client.clone(), self.db.clone());
        self.on_runtime(async move { Syncer::new(client, db, SyncOptions::default()).refresh_playlist(&id).await }).await??;
        Ok(())
    }
}

fn image_result(change: Result<Option<String>, PlaylistImageError>) -> PlaylistImageResult {
    match change {
        Ok(cover_art) => PlaylistImageResult { cover_art, problem: None },
        Err(problem) => PlaylistImageResult { cover_art: None, problem: Some(problem.to_string()) },
    }
}

fn dedup(songs: Vec<Song>) -> Vec<SongRow> {
    let mut seen = std::collections::HashSet::new();
    songs.into_iter().filter(|s| seen.insert(s.id.clone())).map(|s| song_row(None, s)).collect()
}

/// A transcode format the app can offer, with its bitrate choices in kbps.
#[derive(uniffi::Record)]
pub struct QualityFormat {
    pub id: String,
    pub label: String,
    pub bitrates: Vec<u32>,
    pub default_kbps: u32,
}

/// A saved choice: `original` (kbps 0), or a format id and its maximum bitrate.
#[derive(uniffi::Record)]
pub struct QualityChoice {
    pub format: String,
    pub kbps: u32,
}

fn quality_choice(q: navia_core::quality::Quality) -> QualityChoice {
    QualityChoice { format: q.format_id().to_string(), kbps: q.kbps() }
}

/// Quality and volume leveling settings, kept in the account's library.
#[uniffi::export]
impl NaviaCore {
    /// The formats this build can decode, in display order.
    pub fn quality_formats(&self) -> Vec<QualityFormat> {
        navia_core::quality::offered_codecs().into_iter().map(|c| QualityFormat { id: c.id().to_string(), label: c.label().to_string(), bitrates: c.bitrates().to_vec(), default_kbps: c.default_kbps() }).collect()
    }

    pub fn streaming_quality(&self) -> QualityChoice {
        quality_choice(self.downloads.quality_control().streaming())
    }

    pub fn set_streaming_quality(&self, format: String, kbps: u32) -> Result<(), NaviaError> {
        Ok(self.downloads.quality_control().set_streaming(navia_core::quality::Quality::new(&format, kbps)?)?)
    }

    pub fn metered_quality(&self) -> QualityChoice {
        quality_choice(self.downloads.quality_control().metered_streaming())
    }

    pub fn set_metered_quality(&self, format: String, kbps: u32) -> Result<(), NaviaError> {
        Ok(self.downloads.quality_control().set_metered_streaming(navia_core::quality::Quality::new(&format, kbps)?)?)
    }

    pub fn download_quality(&self) -> QualityChoice {
        quality_choice(self.downloads.quality_control().download())
    }

    pub fn set_download_quality(&self, format: String, kbps: u32) -> Result<(), NaviaError> {
        Ok(self.downloads.quality_control().set_download(navia_core::quality::Quality::new(&format, kbps)?)?)
    }

    /// The system's metered signal. The next track picks up the matching quality.
    pub fn set_connection_metered(&self, metered: bool) {
        self.downloads.quality_control().set_metered(metered);
    }

    pub fn connection_metered(&self) -> bool {
        self.downloads.quality_control().is_metered()
    }

    /// `off`, `track` or `album`.
    pub fn leveling(&self) -> String {
        self.downloads.quality_control().leveling().id().to_string()
    }

    pub fn set_leveling(&self, mode: String) -> Result<(), NaviaError> {
        let parsed = navia_core::quality::Leveling::from_id(&mode).ok_or_else(|| NaviaError::Failed { message: format!("{mode} isn't a leveling mode") })?;
        Ok(self.downloads.quality_control().set_leveling(parsed)?)
    }

    /// Format ids the server says it can't produce. Empty when it can produce them all or there's no way to tell.
    /// Blocks on the network, so call it off the main thread.
    pub fn unavailable_formats(&self) -> Vec<String> {
        self.downloads.quality_control().detect_formats().unwrap_or_default().into_iter().filter(|(_, can)| !can).map(|(c, _)| c.id().to_string()).collect()
    }
}

/// Settings > Storage: the song cache limit, cache sizes, and clearing them.
#[uniffi::export]
impl NaviaCore {
    /// This account's Deezer recommendations choice. None until the listener chooses, which counts as off.
    pub fn deezer_recommendations(&self) -> Option<bool> {
        navia_core::deezer::setting(&self.db)
    }

    /// Saves the choice for this account. None forgets it, so the app asks again.
    pub fn set_deezer_recommendations(&self, on: Option<bool>) -> Result<(), NaviaError> {
        navia_core::deezer::set_setting(&self.db, on)?;
        Ok(())
    }

    /// This account's Autoplay mode. Navia unless chosen.
    pub fn autoplay_mode(&self) -> AutoplayModeRow {
        navia_core::engine::mode(&self.db).into()
    }

    /// Saves the Autoplay mode. A running `NaviaAutoplay` follows it at its next `set_context`.
    pub fn set_autoplay_mode(&self, mode: AutoplayModeRow) -> Result<(), NaviaError> {
        navia_core::engine::set_mode(&self.db, mode.into())?;
        Ok(())
    }

    /// The song percentages Settings offers for "Count a play after".
    pub fn play_threshold_choices(&self) -> Vec<u32> {
        navia_core::reporting::PlayThreshold::PERCENT_CHOICES.iter().map(|&p| u32::from(p)).collect()
    }

    /// When a play counts for this account. 60% or four minutes unless changed.
    pub fn play_threshold(&self) -> PlayThresholdRow {
        let t = self.reporter.play_threshold();
        PlayThresholdRow { percent: u32::from(t.percent), cap_at_four_minutes: t.cap_at_four_minutes }
    }

    /// Saves when a play counts for this account. It applies to the current song too if it hasn't counted yet. A
    /// percentage that isn't one of the choices is refused.
    pub fn set_play_threshold(&self, threshold: PlayThresholdRow) -> Result<(), NaviaError> {
        let t = u8::try_from(threshold.percent).ok().and_then(|p| navia_core::reporting::PlayThreshold::new(p, threshold.cap_at_four_minutes))
            .ok_or_else(|| NaviaError::Failed { message: format!("{}% isn't one of the choices", threshold.percent) })?;
        self.reporter.set_play_threshold(t)?;
        Ok(())
    }

    /// The limits Settings offers, in bytes. 0 means off.
    pub fn song_cache_limit_choices(&self) -> Vec<u64> {
        navia_core::songcache::LIMIT_CHOICES.to_vec()
    }

    pub fn song_cache_limit(&self) -> u64 {
        self.downloads.song_cache().map_or(0, |c| c.limit())
    }

    /// Saved for this account. A lower limit evicts at once, and 0 turns caching off and empties it.
    pub fn set_song_cache_limit(&self, bytes: u64) -> Result<(), NaviaError> {
        if let Some(cache) = self.downloads.song_cache() {
            cache.set_limit(bytes)?;
        }
        Ok(())
    }

    /// Reads the disk, so call it off the main thread.
    pub fn storage_usage(&self) -> StorageUsage {
        let cache = self.downloads.song_cache();
        let (song_cache_bytes, cached_songs) = cache.as_ref().map_or((0, 0), |c| c.usage());
        let downloads = self.downloads.entries().unwrap_or_default();
        StorageUsage {
            song_cache_bytes,
            cached_songs,
            song_cache_limit: cache.as_ref().map_or(0, |c| c.limit()),
            song_cache_low_disk: cache.as_ref().is_some_and(|c| c.low_disk()),
            artwork_bytes: self.artwork.usage(),
            lyrics_bytes: self.lyrics.usage(),
            downloads_bytes: downloads.iter().map(|(_, b)| b).sum(),
            downloaded_songs: downloads.len() as u64,
        }
    }

    /// Clears cached songs, artwork and lyrics. Downloads and the library stay.
    pub fn clear_caches(&self) {
        if let Some(cache) = self.downloads.song_cache() {
            cache.clear();
        }
        self.artwork.clear();
        self.lyrics.clear();
    }

    /// Whether a song is in the song cache. Checks only the index, so it's cheap enough for the main thread.
    pub fn is_song_cached(&self, id: String) -> bool {
        self.downloads.song_cache().is_some_and(|c| c.contains(&id))
    }

    /// Songs in the song cache, which play offline like downloads.
    pub fn cached_song_ids(&self) -> Vec<String> {
        self.downloads.song_cache().map(|c| c.song_ids()).unwrap_or_default()
    }

    /// Every cached song, most recently played or cached first. Reads only the index, and only complete files are ever
    /// kept.
    pub fn cached_songs(&self) -> Vec<CachedSongRow> {
        self.downloads.song_cache().map(|c| c.songs()).unwrap_or_default().into_iter().map(|s| CachedSongRow { id: s.song_id, bytes: s.bytes, last_played_ms: s.last_played_ms }).collect()
    }

    /// Deletes these songs' cached copies and returns the bytes freed. Call off the main thread.
    pub fn remove_cached_songs(&self, ids: Vec<String>) -> u64 {
        self.downloads.song_cache().map_or(0, |c| c.remove_songs(&ids))
    }

    /// Deletes every cached song, but not artwork, lyrics or downloads. Call off the main thread.
    pub fn clear_song_cache(&self) {
        if let Some(cache) = self.downloads.song_cache() {
            cache.clear();
        }
    }
}

fn edit_result(r: Result<(), PlaylistEditError>) -> PlaylistEditResult {
    match r {
        Ok(()) => PlaylistEditResult::Saved,
        Err(PlaylistEditError::WritesDisabled) => PlaylistEditResult::WritesDisabled,
        Err(PlaylistEditError::ReadOnly) => PlaylistEditResult::ReadOnly,
        Err(PlaylistEditError::Changed) => PlaylistEditResult::Changed,
        Err(PlaylistEditError::NotFound) => PlaylistEditResult::Failed { message: "the playlist isn't in the library".into() },
        Err(PlaylistEditError::Failed(message)) => PlaylistEditResult::Failed { message },
    }
}

/// The result of a change to a playlist's songs. The app shows a message for everything but `Saved`, and changes
/// nothing it showed.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum PlaylistEditResult {
    Saved,
    /// Test and read-only runs can't change playlists.
    WritesDisabled,
    /// Smart, or someone else's.
    ReadOnly,
    /// It changed on the server since it was shown. The fresh copy is stored.
    Changed,
    Failed { message: String },
}

/// A song in the queue, so Autoplay can skip it and its other versions.
#[derive(uniffi::Record)]
pub struct AutoplaySong {
    pub id: String,
    pub title: String,
    /// The first credited artist, else the display artist.
    pub artist: String,
}

impl AutoplaySong {
    fn taken(&self) -> navia_core::autoplay::Taken {
        navia_core::autoplay::Taken { id: self.id.clone(), version: navia_core::autoplay::version_key(&self.title, &self.artist) }
    }
}

/// Settings > Playback > Autoplay mode.
#[derive(uniffi::Enum, Clone, Copy, PartialEq, Eq)]
pub enum AutoplayModeRow {
    /// Navia's own engine, the default.
    Navia,
    /// The server's similar songs in its order.
    Server,
}

impl From<navia_core::engine::AutoplayMode> for AutoplayModeRow {
    fn from(mode: navia_core::engine::AutoplayMode) -> Self {
        match mode {
            navia_core::engine::AutoplayMode::Navia => AutoplayModeRow::Navia,
            navia_core::engine::AutoplayMode::Server => AutoplayModeRow::Server,
        }
    }
}

impl From<AutoplayModeRow> for navia_core::engine::AutoplayMode {
    fn from(mode: AutoplayModeRow) -> Self {
        match mode {
            AutoplayModeRow::Navia => navia_core::engine::AutoplayMode::Navia,
            AutoplayModeRow::Server => navia_core::engine::AutoplayMode::Server,
        }
    }
}

/// Settings > Playback > Shuffle, either Fewer Repeats or plain.
#[derive(uniffi::Enum, Clone, Copy, PartialEq, Eq)]
pub enum ShuffleStyle {
    FewerRepeats,
    Standard,
}

/// Autoplay (`navia_core::autoplay`). Asking the server is read-only, so test runs may do it too.
#[derive(uniffi::Object)]
pub struct NaviaAutoplay {
    core: Arc<NaviaCore>,
    pub(crate) state: Mutex<navia_core::autoplay::Autoplay>,
}

#[uniffi::export]
impl NaviaCore {
    pub fn make_autoplay(self: Arc<Self>) -> Arc<NaviaAutoplay> {
        let mut state = navia_core::autoplay::Autoplay::new();
        state.attach(self.db.clone());
        Arc::new(NaviaAutoplay { core: self, state: Mutex::new(state) })
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl NaviaAutoplay {
    /// A song started playing.
    pub fn heard(&self, song: AutoplaySong) {
        self.state.lock().unwrap().heard_taken(song.taken());
    }

    /// The listener took a song out of Autoplay.
    pub fn remove(&self, song: AutoplaySong) {
        let mut state = self.state.lock().unwrap();
        let taken = song.taken();
        state.remove(&Song { id: taken.id, title: song.title, artists: vec![navia_core::client::ArtistRef { id: String::new(), name: song.artist }], ..Default::default() });
    }

    /// What the listener started: the queue's context (`NaviaQueue::context`) and its anchor songs, oldest first. Only
    /// a new context starts a new plan.
    pub fn set_context(&self, key: u64, anchors: Vec<String>) {
        self.state.lock().unwrap().set_context(key, anchors.iter().map(String::as_str));
    }

    /// Whether the offer is final (`Autoplay::ready`). Until then the queue shows "finding" and `songs` is only a
    /// fallback if the context ends early.
    pub fn ready(&self) -> bool {
        self.state.lock().unwrap().ready()
    }

    /// Asks the server when the seeds changed. Returns true when there are new answers.
    pub async fn refresh(&self) -> bool {
        let seeds = {
            let mut state = self.state.lock().unwrap();
            let Some(seeds) = state.wanted_fetch() else { return false };
            state.begin_fetch(&seeds);
            seeds
        };
        let mode = self.state.lock().unwrap().mode();
        let (client, db) = (self.core.client.clone(), self.core.db.clone());
        let asked = seeds.clone();
        let answers = self.core.on_runtime(async move { navia_core::autoplay::fetch_answers(&client, &db, &asked, mode).await }).await.unwrap_or_default();
        self.state.lock().unwrap().finish_answers(seeds, answers);
        true
    }

    /// Looks up the BPM of a song Autoplay is about to queue when it has no BPM tag and Deezer is allowed. Call when
    /// handing a song to the player.
    pub async fn look_up_bpm(&self, song_id: String) {
        let db = self.core.db.clone();
        let _ = self.core.on_runtime(async move { navia_core::autoplay::look_up_bpm(&db, &song_id).await }).await;
    }

    /// Up to `limit` songs to offer after `queue`, as library rows where possible.
    pub fn songs(&self, queue: Vec<AutoplaySong>, limit: u32) -> Vec<SongRow> {
        let taken: Vec<_> = queue.iter().map(AutoplaySong::taken).collect();
        let songs = self.state.lock().unwrap().songs(&taken, limit as usize);
        self.core.library_rows(songs)
    }

    pub fn reset(&self) {
        self.state.lock().unwrap().reset();
    }
}

/// Recommended songs under one playlist for one visit (`navia_core::recommend`). Asking the server is read-only, so
/// test runs may do it too.
#[derive(uniffi::Object)]
pub struct NaviaRecommender {
    core: Arc<NaviaCore>,
    playlist: String,
    state: Arc<Mutex<Option<navia_core::recommend::Recommender>>>,
}

#[uniffi::export]
impl NaviaCore {
    /// A new visit's Recommended for a playlist.
    pub fn make_recommender(self: Arc<Self>, playlist_id: String) -> Arc<NaviaRecommender> {
        Arc::new(NaviaRecommender { core: self, playlist: playlist_id, state: Default::default() })
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl NaviaRecommender {
    /// The next batch: the best songs not yet shown this visit, asking for the server's similar songs as needed. Empty
    /// when there's nothing to suggest.
    pub async fn next_batch(&self) -> Vec<SongRow> {
        let (client, db, id, state) = (self.core.client.clone(), self.core.db.clone(), self.playlist.clone(), self.state.clone());
        let songs = self.core.on_runtime(async move { navia_core::recommend::batch_for_playlist(&client, &db, &id, &state).await }).await.unwrap_or_default();
        self.core.library_rows(songs).into_iter().filter(|r| r.rowid.is_some()).collect()
    }
}

impl NaviaCore {
    /// Songs from the server as library rows where the library has them, in order.
    fn library_rows(&self, songs: Vec<Song>) -> Vec<SongRow> {
        songs
            .into_iter()
            .map(|s| {
                let rowid = self.db.song_rowids(std::slice::from_ref(&s.id)).ok().and_then(|r| r.into_iter().next());
                match rowid.and_then(|r| self.db.songs_by_rowid(&[r]).ok()).and_then(|v| v.into_iter().next()) {
                    Some((r, local)) => song_row(Some(r), local),
                    None => song_row(None, s),
                }
            })
            .collect()
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl NaviaCore {
    /// A radio from one song, for Start Song Radio and songs played from Search. Returns the songs to queue after it
    /// (`navia_core::autoplay::radio`). Empty when neither the server nor the library has anything. Read-only, so test
    /// runs may ask too.
    pub async fn radio_songs(&self, song_id: String) -> Vec<SongRow> {
        let (client, db) = (self.client.clone(), self.db.clone());
        let songs = self.on_runtime(async move { navia_core::autoplay::radio(&client, &db, &song_id).await }).await.unwrap_or_default();
        self.library_rows(songs)
    }

    /// Start Artist Radio (`navia_core::artist_radio`). Uses Deezer's artist radio mapped onto the library when Deezer
    /// is enabled, filled from the server's `getSimilarSongs2`. Library songs only, in radio order. Empty in test runs
    /// or when neither source has anything. Read-only.
    pub async fn artist_radio(&self, artist_id: String, artist_name: String) -> Vec<SongRow> {
        if !self.external_allowed() {
            return Vec::new();
        }
        let (client, db) = (self.client.clone(), self.db.clone());
        let deezer = navia_core::artist_radio::Deezer::shared();
        let radio = self.on_runtime(async move { navia_core::artist_radio::artist_radio(&client, &db, deezer, &artist_id, &artist_name).await }).await;
        radio.map(|r| self.library_rows(r.songs)).unwrap_or_default()
    }
}

/// The queue's Autoplay section, as in `navia_core::autoplay::state`.
#[derive(uniffi::Enum, Clone, Copy, PartialEq, Eq)]
pub enum AutoplaySection {
    On,
    Off,
    Repeat,
    Station,
    Empty,
    /// The list still has songs to come, so no section until its last song plays.
    Later,
    /// The list is over and the plan isn't ready, so the heading shows with placeholder rows.
    Finding,
}

/// How many Autoplay songs the queue shows (`navia_core::autoplay::SHOWN`).
#[uniffi::export]
pub fn autoplay_rows_shown() -> u32 {
    navia_core::autoplay::SHOWN as u32
}

/// The Autoplay section's state. `list_left` counts the list's songs after the current one, excluding queued songs, and
/// `ready` is `NaviaAutoplay::ready`.
#[uniffi::export]
pub fn autoplay_section(enabled: bool, repeat: crate::RepeatMode, station: bool, has_songs: bool, list_left: u64, ready: bool) -> AutoplaySection {
    use navia_core::autoplay::AutoplayState as S;
    match navia_core::autoplay::state(enabled, repeat.into(), station, has_songs, list_left as usize, ready) {
        S::On => AutoplaySection::On,
        S::Off => AutoplaySection::Off,
        S::Repeat => AutoplaySection::Repeat,
        S::Station => AutoplaySection::Station,
        S::Empty => AutoplaySection::Empty,
        S::Later => AutoplaySection::Later,
        S::Finding => AutoplaySection::Finding,
    }
}

/// A session's state in `getNowPlaying`, where the server has `playbackReport`.
#[derive(uniffi::Enum, Clone, Copy, PartialEq, Eq, Debug)]
pub enum NowPlayingState {
    Starting,
    Playing,
    Paused,
}

/// One `getNowPlaying` entry (`navia_core::listen_along::NowPlaying`) with its song as a row.
#[derive(uniffi::Record, Clone, Debug)]
pub struct NowPlayingRow {
    pub user: String,
    pub player: String,
    pub song: SongRow,
    pub state: Option<NowPlayingState>,
    pub position_ms: Option<u64>,
    pub can_listen_along: bool,
}

impl NaviaCore {
    fn now_playing_row(&self, e: navia_core::listen_along::NowPlaying) -> NowPlayingRow {
        let rowid = self.db.song_rowids(std::slice::from_ref(&e.song.id)).ok().and_then(|r| r.first().copied());
        let can_listen_along = e.can_listen_along();
        let state = e.state.map(|s| match s {
            navia_core::listen_along::SessionState::Starting => NowPlayingState::Starting,
            navia_core::listen_along::SessionState::Playing => NowPlayingState::Playing,
            navia_core::listen_along::SessionState::Paused => NowPlayingState::Paused,
        });
        NowPlayingRow { user: e.user, player: e.player, song: song_row(rowid, e.song), state, position_ms: e.position_ms, can_listen_along }
    }
}

impl NowPlayingRow {
    fn core(&self) -> navia_core::listen_along::NowPlaying {
        use navia_core::listen_along::SessionState as S;
        let state = self.state.map(|s| match s {
            NowPlayingState::Starting => S::Starting,
            NowPlayingState::Playing => S::Playing,
            NowPlayingState::Paused => S::Paused,
        });
        let song = navia_core::client::Song { id: self.song.id.clone(), ..Default::default() };
        navia_core::listen_along::NowPlaying { user: self.user.clone(), player: self.player.clone(), song, state, position_ms: self.position_ms, minutes_ago: 0 }
    }

    fn matches(&self, e: &navia_core::listen_along::NowPlaying) -> bool {
        self.user == e.user && self.player == e.player && self.song.id == e.song.id
    }
}

/// What one listen-along check asks of the player (`navia_core::listen_along::follow_step`).
#[derive(uniffi::Enum, Clone, Debug)]
pub enum FollowStep {
    /// They stopped or can't be followed. Keep the song, pause, and show "<user> stopped listening".
    Ended,
    /// Play `entry`'s song from its position, matching their play or pause state.
    Resync { entry: NowPlayingRow, player: String },
    /// In step. `drift_ms` is how far ahead this player is.
    Keep { drift_ms: i64, player: String },
    /// the listener's own change is on its way to the host: "Sending to <user>…".
    Sending { player: String },
    /// the host didn't take the listener's change: say so, then play `entry` as `Resync` does.
    NotTaken { entry: NowPlayingRow, player: String },
    /// they follow this player back and this side leads: stop following, keep playing.
    Lead { user: String },
    /// A followed Spotify friend's song isn't in the library: pause, say it was skipped (once per
    /// `key`), keep following.
    NotInLibrary { key: String, song: String, artist: String },
}

/// This player's state while following (`navia_core::listen_along::Local`). `held` means the listener paused it
/// themselves. It also carries when they last pressed play or pause, and the followed player's previous state.
#[derive(uniffi::Record, Clone, Debug)]
pub struct FollowLocal {
    pub song_id: String,
    pub position_ms: u64,
    pub playing: bool,
    pub held: bool,
    pub own_change_ms: Option<u64>,
    pub lead: bool,
    pub seen: Option<FollowSeen>,
    pub ended: bool,
    pub viewer: String,
    pub two_way_since: Option<u64>,
    pub now_ms: u64,
}

/// the followed player at a check (`navia_core::listen_along::Seen`).
#[derive(uniffi::Record, Clone, Debug)]
pub struct FollowSeen {
    pub song_id: String,
    pub position_ms: u64,
    pub playing: bool,
    pub at_ms: u64,
}

impl FollowLocal {
    fn core(self) -> navia_core::listen_along::Local {
        navia_core::listen_along::Local {
            song_id: self.song_id,
            position_ms: self.position_ms,
            playing: self.playing,
            held: self.held,
            own_change_ms: self.own_change_ms,
            lead: self.lead,
            seen: self.seen.map(|s| navia_core::listen_along::Seen { song_id: s.song_id, position_ms: s.position_ms, playing: s.playing, at_ms: s.at_ms }),
            ended: self.ended,
            viewer: self.viewer,
            two_way_since: self.two_way_since,
            now_ms: self.now_ms,
        }
    }
}

/// Whether the followed person's session in these entries is playing. None when it's gone.
#[uniffi::export]
pub fn follow_seen(user: String, player: String, entries: Vec<NowPlayingRow>, now_ms: u64) -> Option<FollowSeen> {
    use navia_core::listen_along as la;
    let core: Vec<la::NowPlaying> = entries.iter().map(NowPlayingRow::core).collect();
    la::Seen::of(&la::Followed { user, player }, &core, now_ms).map(|s| FollowSeen { song_id: s.song_id, position_ms: s.position_ms, playing: s.playing, at_ms: s.at_ms })
}

/// whether the followed person follows `viewer` back (`navia_core::listen_along::two_way`).
#[uniffi::export]
pub fn follows_back(user: String, player: String, entries: Vec<NowPlayingRow>, viewer: String) -> bool {
    use navia_core::listen_along as la;
    let core: Vec<la::NowPlaying> = entries.iter().map(NowPlayingRow::core).collect();
    la::two_way(&la::Followed { user, player }, &core, &viewer)
}

/// One check while following `user` on `player`, from a fresh [`NaviaCore::now_playing`].
#[uniffi::export]
pub fn follow_step(user: String, player: String, entries: Vec<NowPlayingRow>, local: FollowLocal) -> FollowStep {
    use navia_core::listen_along as la;
    let core: Vec<la::NowPlaying> = entries.iter().map(NowPlayingRow::core).collect();
    match la::follow_step(&la::Followed { user, player }, &core, &local.core()) {
        la::FollowStep::Ended => FollowStep::Ended,
        la::FollowStep::Keep { drift_ms, player } => FollowStep::Keep { drift_ms, player },
        la::FollowStep::Resync { entry, player } => match entries.iter().find(|r| r.matches(&entry)) {
            Some(row) => FollowStep::Resync { entry: row.clone(), player },
            None => FollowStep::Ended,
        },
        la::FollowStep::NotTaken { entry, player } => match entries.iter().find(|r| r.matches(&entry)) {
            Some(row) => FollowStep::NotTaken { entry: row.clone(), player },
            None => FollowStep::Ended,
        },
        la::FollowStep::Sending { player } => FollowStep::Sending { player },
        la::FollowStep::Lead { user } => FollowStep::Lead { user },
        la::FollowStep::NotInLibrary { key, song, artist } => FollowStep::NotInLibrary { key, song, artist },
    }
}

/// Listen-along timing in milliseconds (`navia_core::listen_along`): list refresh, refresh while following, the drift
/// that triggers a seek, and how long "stopped listening" shows.
#[derive(uniffi::Record)]
pub struct ListenAlongTiming {
    pub list_poll_ms: u64,
    pub follow_poll_ms: u64,
    pub resync_drift_ms: u64,
    pub ended_notice_ms: u64,
    /// Listening Now from the last good check is dropped once this old.
    pub listing_keep_ms: u64,
    /// two people seen following each other this long: the one who leads stops following.
    pub two_way_settle_ms: u64,
}

#[uniffi::export]
pub fn listen_along_timing() -> ListenAlongTiming {
    use navia_core::listen_along as la;
    let ms = |d: std::time::Duration| d.as_millis() as u64;
    ListenAlongTiming { list_poll_ms: ms(la::LIST_POLL), follow_poll_ms: ms(la::FOLLOW_POLL), resync_drift_ms: ms(la::RESYNC_DRIFT), ended_notice_ms: ms(la::ENDED_NOTICE), listing_keep_ms: ms(la::LISTING_KEEP), two_way_settle_ms: ms(la::TWO_WAY_SETTLE) }
}

/// Whether Autoplay should hand the player its next song now.
#[uniffi::export]
pub fn autoplay_starts_now(enabled: bool, repeat: crate::RepeatMode, station: bool, left_after_current: u64) -> bool {
    navia_core::autoplay::starts_now(enabled, repeat.into(), station, left_after_current as usize)
}

impl NaviaCore {
    /// The server address as signed in.
    pub(crate) fn server_url(&self) -> &str {
        self.client.base_url()
    }

    /// for Discord art. older libraries may lack ISRCs; the art thread then asks the server.
    pub(crate) fn song_isrc_local(&self, song_id: &str) -> Option<String> {
        self.db.song(song_id).ok().flatten().and_then(|s| s.isrc.into_iter().next())
    }
}

/// Discord art's server lookups, only on the art thread. Never in Private mode, with Discord status off, or in a test
/// run.
impl navia_integrations::art::ServerLookup for NaviaCore {
    fn album_image_link(&self, album_id: &str) -> Result<Option<String>, navia_integrations::art::ArtError> {
        if self.test_run {
            return Ok(None);
        }
        let (client, id) = (self.client.clone(), album_id.to_owned());
        let info = self.rt.block_on(async move { client.album_info2(&id).await }).map_err(|e| navia_integrations::art::ArtError(e.to_string()))?;
        Ok(info.large_image_url.filter(|u| !u.is_empty()))
    }

    fn song_isrc(&self, song_id: &str) -> Result<Option<String>, navia_integrations::art::ArtError> {
        if let Some(isrc) = self.song_isrc_local(song_id) {
            return Ok(Some(isrc));
        }
        if self.test_run {
            return Ok(None);
        }
        let (client, id) = (self.client.clone(), song_id.to_owned());
        let song = self.rt.block_on(async move { client.song(&id).await }).map_err(|e| navia_integrations::art::ArtError(e.to_string()))?;
        Ok(song.isrc.into_iter().next())
    }
}

// MARK: Share links (`navia_core::shares`)

/// What the server allows: links at all (getShares answers), and links that allow downloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct SharingRow {
    pub on: bool,
    pub downloads: bool,
}

/// What a link holds. `kind` is song, album, playlist or artist.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ShareTargetRow {
    pub kind: String,
    pub ids: Vec<String>,
}

/// One item the server lists in a link, an album or a song.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ShareEntryRow {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_id: String,
}

/// One of the user's links.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct ShareLinkRow {
    pub id: String,
    pub url: String,
    /// Empty when the server only has its own placeholder (`shares::is_server_label`) or nothing.
    pub description: String,
    pub created_ms: u64,
    pub expires_ms: u64,
    pub last_visited_ms: Option<u64>,
    pub visits: u64,
    pub target: ShareTargetRow,
    pub downloadable: Option<bool>,
    pub entries: Vec<ShareEntryRow>,
}

fn kind_name(kind: navia_core::shares::ShareKind) -> String {
    use navia_core::shares::ShareKind::*;
    match kind { Song => "song", Album => "album", Playlist => "playlist", Artist => "artist" }.to_string()
}

fn kind_of(name: &str) -> navia_core::shares::ShareKind {
    use navia_core::shares::ShareKind::*;
    match name { "album" => Album, "playlist" => Playlist, "artist" => Artist, _ => Song }
}

fn link_row(l: navia_core::shares::ShareLink) -> ShareLinkRow {
    let title = l.entries.first().and_then(|e| e.title.clone()).unwrap_or_default();
    let description = if navia_core::shares::is_server_label(&l.description, &title) { String::new() } else { l.description };
    ShareLinkRow {
        id: l.id,
        url: l.url,
        description,
        created_ms: l.created_ms,
        expires_ms: l.expires_ms,
        last_visited_ms: l.last_visited_ms,
        visits: l.visits,
        target: ShareTargetRow { kind: kind_name(l.target.kind), ids: l.target.ids },
        downloadable: l.downloadable,
        entries: l.entries.into_iter().map(|e| ShareEntryRow {
            id: e.id,
            title: e.title.unwrap_or_default(),
            artist: e.artist.unwrap_or_default(),
            album: e.album.unwrap_or_default(),
            album_id: e.album_id.unwrap_or_default(),
        }).collect(),
    }
}

/// A share call's failure as a message for the app. `L` translates the fixed ones.
fn share_problem(e: navia_core::Error) -> NaviaError {
    let message = match e {
        navia_core::Error::WritesDisabled { .. } => "Test and read-only runs can't change share links.".to_string(),
        navia_core::Error::HttpStatus { status: 501, .. } => "Sharing is turned off on this server".to_string(),
        e => e.to_string(),
    };
    NaviaError::Failed { message }
}

impl NaviaCore {
    /// Writes links with the user's writer, which a test run keeps read-only unless it sets `NAVIA_TEST_SHARE=1` for
    /// one throwaway link.
    fn share_writer(&self) -> Result<Client, NaviaError> {
        if self.test_run && std::env::var("NAVIA_TEST_SHARE").is_ok_and(|v| v == "1") {
            return Ok(self.writer.writable_copy()?);
        }
        Ok(self.writer.clone())
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl NaviaCore {
    /// Whether the server allows links, and links with downloads. None until connected.
    pub fn sharing(&self) -> Option<SharingRow> {
        *self.sharing.lock().unwrap()
    }

    /// The user's links, newest first.
    pub async fn shares(&self) -> Result<Vec<ShareLinkRow>, NaviaError> {
        let (client, db) = (self.client.clone(), self.db.clone());
        let links = self.on_runtime(async move { navia_core::shares::list(&client, &db).await }).await?.map_err(share_problem)?;
        Ok(links.into_iter().map(link_row).collect())
    }

    /// Creates a link. With `expires_ms` None the server picks the date.
    pub async fn create_share(&self, target: ShareTargetRow, description: String, expires_ms: Option<u64>, downloadable: bool) -> Result<ShareLinkRow, NaviaError> {
        let (client, db) = (self.share_writer()?, self.db.clone());
        let downloads = self.sharing().is_some_and(|s| s.downloads);
        let target = navia_core::shares::ShareTarget { kind: kind_of(&target.kind), ids: target.ids };
        let choice = navia_core::shares::ShareChoice { description, expires_ms, downloadable };
        let link = self.on_runtime(async move { navia_core::shares::create(&client, &db, target, choice, downloads).await }).await?.map_err(share_problem)?;
        Ok(link_row(link))
    }

    /// Edits a link's description, downloads, and date when given.
    pub async fn update_share(&self, link: ShareLinkRow, description: String, expires_ms: Option<u64>, downloadable: bool) -> Result<(), NaviaError> {
        let (client, db) = (self.share_writer()?, self.db.clone());
        let downloads = self.sharing().is_some_and(|s| s.downloads);
        let link = navia_core::shares::ShareLink {
            id: link.id,
            url: link.url,
            description: link.description,
            created_ms: link.created_ms,
            expires_ms: link.expires_ms,
            last_visited_ms: link.last_visited_ms,
            visits: link.visits,
            target: navia_core::shares::ShareTarget { kind: kind_of(&link.target.kind), ids: link.target.ids },
            downloadable: link.downloadable,
            entries: Vec::new(),
        };
        let choice = navia_core::shares::ShareChoice { description, expires_ms, downloadable };
        self.on_runtime(async move { navia_core::shares::update(&client, &db, &link, choice, downloads).await }).await?.map_err(share_problem)
    }

    pub async fn delete_share(&self, id: String) -> Result<(), NaviaError> {
        let (client, db) = (self.share_writer()?, self.db.clone());
        self.on_runtime(async move { navia_core::shares::delete(&client, &db, &id).await }).await?.map_err(share_problem)
    }
}

/// The end of a local calendar day as Unix ms, for a date picked in the share dialog.
#[uniffi::export]
pub fn share_end_of_day_ms(year: i32, month: u32, day: u32) -> Option<u64> {
    let now = navia_core::library::taste::now_unix();
    navia_core::shares::end_of_day_ms(year, month, day, navia_core::library::taste::utc_offset(now))
}

// MARK: Spotify friends

/// A friend added in Settings: their Discord user id and an optional display name.
#[derive(uniffi::Record, Clone, Debug)]
pub struct SpotifyFriend {
    pub id: String,
    pub name: Option<String>,
}

/// Whether Lanyard can tell Navia about a friend (`navia_core::lanyard::Reach`).
#[derive(uniffi::Enum, Clone, Copy, PartialEq, Eq, Debug)]
pub enum FriendReach {
    Waiting,
    Readable,
    NotMonitored,
    Unreachable,
}

/// A friend's current state: their Spotify song while listening, and that song in the library if it's there.
#[derive(uniffi::Record, Clone, Debug)]
pub struct SpotifyFriendRow {
    pub id: String,
    /// The name given in Settings, if any.
    pub given_name: Option<String>,
    /// The given name, else their Discord name, else the id.
    pub name: String,
    pub initials: String,
    pub avatar_url: Option<String>,
    pub reach: FriendReach,
    /// Their Spotify activity shows a song.
    pub listening: bool,
    pub title: String,
    /// Spotify's artists as one line, and the first of them.
    pub artist: String,
    pub lead_artist: String,
    pub album: String,
    /// Identifies the song, as the key for the "skipped" notice.
    pub key: String,
    /// The song in the library. None shows "Not in your library".
    pub song: Option<SongRow>,
    /// Their current position in it.
    pub position_ms: u64,
    /// The `player` value for following them, for [`spotify_follow_step`] and the chip.
    pub player: String,
}

#[uniffi::export]
impl NaviaCore {
    /// Sets the Lanyard address and the friends. An empty address means the public one. With no friends nothing
    /// contacts Lanyard. A test run only reads a local stand-in unless `NAVIA_TEST_EXTERNAL=1`.
    pub fn set_spotify_friends(&self, address: String, friends: Vec<SpotifyFriend>) {
        let address = navia_core::lanyard::parse_address(&address).unwrap_or_else(|| navia_core::lanyard::PUBLIC_ADDRESS.to_string());
        let friends = if !self.external_allowed() && !navia_core::lanyard::is_loopback(&address) { Vec::new() } else { friends.into_iter().map(|f| navia_core::lanyard::Friend { id: f.id, name: f.name }).collect() };
        self.friends.configure(&address, friends);
    }

    /// Changes whenever a friend's presence or the friend list changes.
    pub fn spotify_generation(&self) -> u64 {
        self.friends.generation()
    }

    /// Every friend as Lanyard last reported, with songs matched in the library.
    pub fn spotify_friends(&self) -> Vec<SpotifyFriendRow> {
        let now = navia_core::lanyard::now_ms();
        self.friends.friends_matched(&self.db).into_iter().map(|f| self.friend_row(f, now)).collect()
    }

    /// While following a friend, Lanyard is polled more often when its socket can't be used.
    pub fn set_following_friend(&self, on: bool) {
        self.friends.set_following(on);
    }

    /// One check while following the friend named by `player`, with this player's song, position and state. `Resync`
    /// carries the song to play from their position.
    pub fn spotify_follow_step(&self, user: String, player: String, local: FollowLocal) -> FollowStep {
        use navia_core::listen_along as la;
        let now = navia_core::lanyard::now_ms();
        let friends = self.friends.friends_matched(&self.db);
        let followed_player = player.clone();
        match navia_core::lanyard::friend_step(&la::Followed { user, player }, &friends, &local.core(), now) {
            la::FollowStep::Ended => FollowStep::Ended,
            la::FollowStep::Keep { drift_ms, player } => FollowStep::Keep { drift_ms, player },
            la::FollowStep::NotInLibrary { key, song, artist } => FollowStep::NotInLibrary { key, song, artist },
            la::FollowStep::Resync { entry, player } | la::FollowStep::NotTaken { entry, player } => FollowStep::Resync { entry: self.now_playing_row(entry), player },
            la::FollowStep::Sending { player } => FollowStep::Sending { player },
            la::FollowStep::Lead { .. } => FollowStep::Keep { drift_ms: 0, player: followed_player },
        }
    }

    /// A friend's song as a session to follow, when it's in the library.
    pub fn spotify_session(&self, id: String) -> Option<NowPlayingRow> {
        let now = navia_core::lanyard::now_ms();
        let friend = self.friends.friends_matched(&self.db).into_iter().find(|f| f.friend.id == id)?;
        friend.session(now).map(|e| self.now_playing_row(e))
    }
}

impl NaviaCore {
    fn friend_row(&self, f: navia_core::lanyard::FriendNow, now: u64) -> SpotifyFriendRow {
        use navia_core::lanyard::Reach;
        let name = f.display_name();
        let reach = match f.reach {
            Reach::Waiting => FriendReach::Waiting,
            Reach::Readable => FriendReach::Readable,
            Reach::NotMonitored => FriendReach::NotMonitored,
            Reach::Unreachable => FriendReach::Unreachable,
        };
        let track = f.track.clone().unwrap_or_default();
        let song = f.song.clone().map(|s| {
            let rowid = self.db.song_rowids(std::slice::from_ref(&s.id)).ok().and_then(|r| r.first().copied());
            song_row(rowid, s)
        });
        SpotifyFriendRow {
            id: f.friend.id.clone(),
            given_name: f.friend.name.clone(),
            initials: navia_core::lanyard::initials(&name),
            avatar_url: f.avatar_url(),
            reach,
            listening: f.listening(),
            title: track.song.clone(),
            artist: track.artist_line(),
            lead_artist: track.first_artist().to_string(),
            album: track.album.clone(),
            key: track.key(),
            song,
            position_ms: track.position_ms(now),
            player: navia_core::lanyard::friend_player(&f.friend.id),
            name,
        }
    }
}

/// A Discord user id as typed (17 to 20 digits), or None.
#[uniffi::export]
pub fn lanyard_friend_id(text: String) -> Option<String> {
    navia_core::lanyard::parse_friend_id(&text)
}

/// The Lanyard address as typed. Empty means the public one, and None means it isn't an http(s) address.
#[uniffi::export]
pub fn lanyard_address(text: String) -> Option<String> {
    navia_core::lanyard::parse_address(&text)
}

/// Lanyard's public address, for the setting's placeholder.
#[uniffi::export]
pub fn lanyard_public_address() -> String {
    navia_core::lanyard::PUBLIC_ADDRESS.to_string()
}

/// Where Lanyard is explained and friends join it, for Settings.
#[uniffi::export]
pub fn lanyard_project_url() -> String {
    navia_core::lanyard::PROJECT_URL.to_string()
}

/// The friend's Discord id when a followed `player` belongs to a Spotify friend.
#[uniffi::export]
pub fn lanyard_friend_of(player: String) -> Option<String> {
    navia_core::lanyard::friend_of(&player).map(str::to_string)
}

// MARK: Listeners controlling the host ("Let listeners control playback")

/// Someone listening along with this Navia (`navia_core::listen_along::Listener`).
#[derive(uniffi::Record, Clone, Debug)]
pub struct ListenerRow {
    pub user: String,
    pub player: String,
    pub song_id: String,
    pub position_ms: u64,
    pub playing: bool,
    pub in_step: bool,
    pub seen_ms: u64,
    /// they have a song queued for the listening session.
    pub queued: bool,
    pub starting: bool,
}

/// the listener whose song or place the host took (`navia_core::listen_along::Led`).
#[derive(uniffi::Record, Clone, Debug)]
pub struct LedRow {
    pub user: String,
    pub player: String,
    pub song_id: String,
    pub at_ms: u64,
}

/// this Navia as the host: its song, position, state, when its user last changed playback, and
/// the listener it follows the place of.
#[derive(uniffi::Record, Clone, Debug)]
pub struct HostRow {
    pub song_id: String,
    pub position_ms: u64,
    pub playing: bool,
    pub own_change_ms: u64,
    pub led: Option<LedRow>,
}

/// what the host does to mirror a listener (`navia_core::listen_along::HostAction`). a pause or
/// play carries the listener's place when the two are apart.
#[derive(uniffi::Enum, Clone, Debug)]
pub enum HostAction {
    Pause { position_ms: Option<u64> },
    Play { position_ms: Option<u64> },
    Seek { position_ms: u64 },
    Song { song_id: String, position_ms: u64, playing: bool },
}

/// a host action, whose it is, and the lead to keep after carrying it out.
#[derive(uniffi::Record, Clone, Debug)]
pub struct HostMirror {
    pub action: HostAction,
    pub user: String,
    pub led: Option<LedRow>,
}

impl HostRow {
    fn core(&self) -> navia_core::listen_along::Host {
        let led = self.led.as_ref().map(|l| navia_core::listen_along::Led { user: l.user.clone(), player: l.player.clone(), song_id: l.song_id.clone(), at_ms: l.at_ms });
        navia_core::listen_along::Host { song_id: self.song_id.clone(), position_ms: self.position_ms, playing: self.playing, own_change_ms: self.own_change_ms, led }
    }
}

fn listener_core(l: &ListenerRow) -> navia_core::listen_along::Listener {
    navia_core::listen_along::Listener { user: l.user.clone(), player: l.player.clone(), song_id: l.song_id.clone(), position_ms: l.position_ms, playing: l.playing, in_step: l.in_step, seen_ms: l.seen_ms, queued: l.queued, starting: l.starting }
}

#[uniffi::export]
impl NaviaCore {
    /// Who is listening along with this Navia now, from a fresh [`Self::now_playing`].
    pub fn listeners(&self, entries: Vec<NowPlayingRow>, host: HostRow, prev: Vec<ListenerRow>) -> Vec<ListenerRow> {
        use navia_core::listen_along as la;
        let core: Vec<la::NowPlaying> = entries.iter().map(NowPlayingRow::core).collect();
        let prev: Vec<la::Listener> = prev.iter().map(listener_core).collect();
        la::listeners(&core, &self.username(), &host.core(), &prev, navia_core::lanyard::now_ms())
            .into_iter()
            .map(|l| ListenerRow { user: l.user, player: l.player, song_id: l.song_id, position_ms: l.position_ms, playing: l.playing, in_step: l.in_step, seen_ms: l.seen_ms, queued: l.queued, starting: l.starting })
            .collect()
    }

    /// "Let listeners control playback", whether this player follows someone, and whether it has
    /// a song queued for the listening session: the reports' client name carries the marks (only
    /// while sharing).
    pub fn set_listen_marks(&self, control: bool, following: bool, queued: bool) {
        let reporter = self.reporter.clone();
        self.rt.spawn(async move { reporter.set_listen_marks(control, following, queued).await });
    }
}

/// What the host does to mirror its listeners, if anything. `control` is whether the setting is on.
#[uniffi::export]
pub fn host_mirror(host: HostRow, prev: Vec<ListenerRow>, now: Vec<ListenerRow>, control: bool) -> Option<HostMirror> {
    use navia_core::listen_along::HostAction as A;
    let prev: Vec<_> = prev.iter().map(listener_core).collect();
    let now: Vec<_> = now.iter().map(listener_core).collect();
    let host = host.core();
    let m = navia_core::listen_along::host_mirror(&host, &prev, &now, control)?;
    let led = m.led(&host, navia_core::lanyard::now_ms()).map(|l| LedRow { user: l.user, player: l.player, song_id: l.song_id, at_ms: l.at_ms });
    let action = match m.action {
        A::Pause(position_ms) => HostAction::Pause { position_ms },
        A::Play(position_ms) => HostAction::Play { position_ms },
        A::Seek(position_ms) => HostAction::Seek { position_ms },
        A::Song { song_id, position_ms, playing } => HostAction::Song { song_id, position_ms, playing },
    };
    Some(HostMirror { action, user: m.user, led })
}

/// whose session song plays at the end of the current one (`navia_core::listen_along::Turn`).
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionTurn {
    Mine,
    Theirs,
    Nobody,
}

impl From<navia_core::listen_along::Turn> for SessionTurn {
    fn from(t: navia_core::listen_along::Turn) -> Self {
        use navia_core::listen_along::Turn;
        match t {
            Turn::Mine => Self::Mine,
            Turn::Theirs => Self::Theirs,
            Turn::Nobody => Self::Nobody,
        }
    }
}

/// one side's hand-over of session songs at a song's end (`navia_core::listen_along::Handover`).
#[derive(uniffi::Object, Default)]
pub struct SessionHandover(std::sync::Mutex<navia_core::listen_along::Handover>);

#[uniffi::export]
impl SessionHandover {
    #[uniffi::constructor]
    pub fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self::default())
    }

    /// the turn for the coming end, from the last stretch of the current song.
    pub fn plan(&self, mine: bool, theirs: bool, host: bool) -> SessionTurn {
        self.0.lock().expect("handover").plan(mine, theirs, host).into()
    }

    /// the turn worked out for the coming end, if any.
    pub fn turn(&self) -> Option<SessionTurn> {
        self.0.lock().expect("handover").turn.map(Into::into)
    }

    pub fn at_end(&self, mine: bool, theirs: bool, host: bool) -> SessionTurn {
        self.0.lock().expect("handover").at_end(mine, theirs, host, navia_core::lanyard::now_ms()).into()
    }

    pub fn waiting(&self) -> bool {
        self.0.lock().expect("handover").waiting_since.is_some()
    }

    pub fn started_mine(&self, song_id: String) {
        self.0.lock().expect("handover").started_mine(&song_id, navia_core::lanyard::now_ms());
    }

    /// the own session song to put back first after a double start, if any.
    pub fn theirs_arrived(&self) -> Option<String> {
        self.0.lock().expect("handover").theirs_arrived(navia_core::lanyard::now_ms())
    }

    pub fn wait_over(&self) -> bool {
        self.0.lock().expect("handover").wait_over(navia_core::lanyard::now_ms())
    }

    pub fn song_changed(&self) {
        self.0.lock().expect("handover").song_changed();
    }

    pub fn reset(&self) {
        *self.0.lock().expect("handover") = Default::default();
    }
}

/// milliseconds until the next listen-along check (`navia_core::listen_along::check_every`).
#[uniffi::export]
pub fn listen_check_every_ms(session: bool, left_ms: Option<u64>, waiting: bool) -> u64 {
    navia_core::listen_along::check_every(session, left_ms, waiting).as_millis() as u64
}

/// whether the client name of a `getNowPlaying` entry says it has a song queued for the session.
#[uniffi::export]
pub fn has_session_queue(player: String) -> bool {
    player.contains(navia_core::listen_along::SHARE_MARK) && player.contains(navia_core::listen_along::QUEUE_MARK)
}

/// Whether a `getNowPlaying` session lets listeners control it, from its client name's mark.
#[uniffi::export]
pub fn allows_control(player: String) -> bool {
    player.contains(navia_core::listen_along::SHARE_MARK) && player.contains(navia_core::listen_along::CONTROL_MARK)
}

/// The current time in Unix milliseconds, as the listen-along checks use it.
#[uniffi::export]
pub fn listen_now_ms() -> u64 {
    navia_core::lanyard::now_ms()
}

/// an animated cover at one display size. frames are straight-alpha RGBA, `width` by `height`,
/// looping forever with `delays_ms`. the frame calls read the disk, so call them off the main thread.
#[derive(uniffi::Object)]
pub struct NaviaAnimatedArt {
    inner: navia_core::artwork::AnimatedArt,
}

#[uniffi::export]
impl NaviaAnimatedArt {
    pub fn width(&self) -> u32 {
        self.inner.width
    }

    pub fn height(&self) -> u32 {
        self.inner.height
    }

    pub fn delays_ms(&self) -> Vec<u32> {
        self.inner.delays_ms.clone()
    }

    pub fn frame_count(&self) -> u32 {
        self.inner.frame_count() as u32
    }

    /// no transparent pixels, so the frames can be drawn without alpha.
    pub fn opaque(&self) -> bool {
        self.inner.opaque
    }

    /// memory every frame takes decoded; never more than 64 MB.
    pub fn decoded_bytes(&self) -> u64 {
        self.inner.decoded_bytes()
    }

    pub fn frame(&self, index: u32) -> Result<Vec<u8>, NaviaError> {
        Ok(self.inner.frame(index as usize)?.into_raw())
    }

    /// every frame, from one read of the file.
    pub fn frames(&self) -> Result<Vec<Vec<u8>>, NaviaError> {
        Ok(self.inner.frames()?.into_iter().map(|f| f.into_raw()).collect())
    }
}
