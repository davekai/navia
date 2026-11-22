//! Swift bindings for Navia's core, generated with UniFFI. The types here are plain data for the FFI boundary, and the
//! real work is in `navia-core`.

use std::sync::Arc;
use std::sync::Mutex;

use navia_core::audio::output::CaptureBuffer;
use navia_core::audio::player::{OutputPolicy as CorePolicy, Player, PlayerConfig, PlayerEvent, PlayerStatus as CoreStatus, QueueItem as CoreQueueItem, Repeat as CoreRepeat};
use navia_core::client::{Album, Artist, Credentials, Playlist, Song, WritePolicy};
use navia_core::library::{AlbumSort as CoreAlbumSort, SongSort as CoreSongSort};
use navia_core::session::{Session, SessionConfig};

uniffi::setup_scaffolding!();

mod airplay_api;
mod logging;
pub use logging::*;
mod core_api;
pub use core_api::*;
mod discord_api;
pub use discord_api::*;
mod queue_api;

mod smart_api;
mod upcoming_api;
pub use upcoming_api::*;
pub use queue_api::*;


#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum NaviaError {
    #[error("{message}")]
    Failed { message: String },
}

impl From<navia_core::Error> for NaviaError {
    fn from(e: navia_core::Error) -> Self {
        NaviaError::Failed { message: e.to_string() }
    }
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct SongRow {
    pub id: String,
    pub title: String,
    pub artist: Option<String>,
    pub artist_id: Option<String>,
    pub album: Option<String>,
    pub album_id: Option<String>,
    pub track: Option<i32>,
    pub disc: Option<i32>,
    pub year: Option<i32>,
    pub duration_seconds: Option<i64>,
    pub cover_art: Option<String>,
    pub starred: bool,
    pub play_count: Option<i64>,
    /// The local database row, when the row came from the catalog.
    pub rowid: Option<u32>,
    pub genre: Option<String>,
    pub bit_depth: Option<i32>,
    pub sampling_rate: Option<i32>,
    pub artists: Vec<ArtistCredit>,
    /// The server marks it explicit (OpenSubsonic `explicitStatus`).
    pub explicit: bool,
    /// When the song was added to the server's library, in ISO 8601.
    pub created: Option<String>,
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct ArtistCredit {
    pub id: String,
    pub name: String,
}

pub(crate) fn song_row(rowid: Option<u32>, s: Song) -> SongRow {
    let artist = s.artist_line();
    let genre = s.genre.or_else(|| s.genres.into_iter().next().map(|g| g.name));
    let explicit = navia_core::client::is_explicit(s.explicit_status.as_deref());
    SongRow {
        id: s.id,
        title: s.title,
        artist,
        artist_id: s.artist_id,
        // Shown without the version Navidrome appends to the name.
        album: s.album.as_deref().map(|a| navia_core::display::song_album_name(s.album_id.as_deref(), a)),
        album_id: s.album_id,
        track: s.track,
        disc: s.disc_number,
        year: s.year,
        duration_seconds: s.duration,
        cover_art: s.cover_art,
        starred: s.starred.is_some(),
        play_count: s.play_count,
        rowid,
        genre,
        bit_depth: s.bit_depth,
        sampling_rate: s.sampling_rate,
        artists: s.artists.into_iter().map(|a| ArtistCredit { id: a.id, name: a.name }).collect(),
        explicit,
        created: s.created,
    }
}

impl From<Song> for SongRow {
    fn from(s: Song) -> Self {
        song_row(None, s)
    }
}

#[derive(uniffi::Record)]
pub struct AlbumRow {
    pub id: String,
    pub name: String,
    pub artist: Option<String>,
    pub artist_id: Option<String>,
    pub year: Option<i32>,
    pub cover_art: Option<String>,
    pub song_count: Option<i64>,
    pub duration_seconds: Option<i64>,
    pub starred: bool,
    pub play_count: Option<i64>,
    pub rowid: Option<u32>,
    pub genre: Option<String>,
    /// The server marks it explicit (OpenSubsonic `explicitStatus`).
    pub explicit: bool,
    /// Each disc's own name where the server has one (OpenSubsonic `discTitles`). Empty titles are left out.
    pub disc_titles: Vec<DiscTitleRow>,
    /// Every credited album artist, in credit order.
    pub artists: Vec<ArtistCredit>,
    /// The version as a label, like "Deluxe Edition". None for explicit or clean, or without a version.
    pub edition: Option<String>,
    /// by the first-release date, so a reissue isn't new.
    pub new_release: bool,
}

#[derive(uniffi::Record)]
pub struct DiscTitleRow {
    pub disc: i32,
    pub title: String,
}

pub(crate) fn album_row(rowid: Option<u32>, a: Album) -> AlbumRow {
    let new_release = navia_core::upcoming::is_new_release(&a, navia_core::library::taste::local_day(navia_core::library::taste::now_unix()));
    let genre = a.genre.or_else(|| a.genres.into_iter().next().map(|g| g.name));
    let explicit = navia_core::client::is_explicit(a.explicit_status.as_deref());
    let disc_titles = a.disc_titles.into_iter().filter(|d| !d.title.trim().is_empty()).map(|d| DiscTitleRow { disc: d.disc, title: d.title }).collect();
    AlbumRow {
        new_release,
        id: a.id,
        // Shown without the version Navidrome appends to the name. The version is `edition`.
        name: navia_core::display::album_name(&a.name, a.version.as_deref()).into_owned(),
        edition: navia_core::display::edition(a.version.as_deref()),
        artist: a.display_artist.or(a.artist),
        artist_id: a.artist_id,
        year: a.year,
        cover_art: a.cover_art,
        song_count: a.song_count,
        duration_seconds: a.duration,
        starred: a.starred.is_some(),
        play_count: a.play_count,
        rowid,
        genre,
        explicit,
        disc_titles,
        artists: a.artists.into_iter().map(|r| ArtistCredit { id: r.id, name: r.name }).collect(),
    }
}

impl From<Album> for AlbumRow {
    fn from(a: Album) -> Self {
        album_row(None, a)
    }
}

#[derive(uniffi::Record)]
pub struct ArtistRow {
    pub id: String,
    pub name: String,
    pub album_count: Option<i64>,
    pub cover_art: Option<String>,
    pub starred: bool,
    pub rowid: Option<u32>,
}

pub(crate) fn artist_row(rowid: Option<u32>, a: Artist) -> ArtistRow {
    ArtistRow { id: a.id, name: a.name, album_count: a.album_count, cover_art: a.cover_art, starred: a.starred.is_some(), rowid }
}

impl From<Artist> for ArtistRow {
    fn from(a: Artist) -> Self {
        artist_row(None, a)
    }
}

#[derive(uniffi::Record)]
pub struct PlaylistRow {
    pub id: String,
    pub name: String,
    pub song_count: Option<i64>,
    pub duration_seconds: Option<i64>,
    pub owner: Option<String>,
    pub public: Option<bool>,
    pub readonly: Option<bool>,
    pub cover_art: Option<String>,
    pub comment: Option<String>,
    /// True only for smart playlists the server has evaluated.
    pub smart: bool,
}

pub(crate) fn playlist_row(p: Playlist) -> PlaylistRow {
    PlaylistRow { smart: p.valid_until.is_some(), id: p.id, name: p.name, song_count: p.song_count, duration_seconds: p.duration, owner: p.owner, public: p.public, readonly: p.readonly, cover_art: p.cover_art, comment: p.comment }
}

impl From<Playlist> for PlaylistRow {
    fn from(p: Playlist) -> Self {
        playlist_row(p)
    }
}

#[derive(uniffi::Record)]
pub struct SearchResultsRow {
    pub artists: Vec<ArtistRow>,
    pub albums: Vec<AlbumRow>,
    pub songs: Vec<SongRow>,
    /// The list whose first item is the best overall match. Only an artist here makes the top result an artist.
    pub best: Option<SearchKind>,
}

/// Which list of a search a result is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum SearchKind {
    Artist,
    Album,
    Song,
}

impl From<navia_core::library::SearchKind> for SearchKind {
    fn from(k: navia_core::library::SearchKind) -> Self {
        match k {
            navia_core::library::SearchKind::Artist => SearchKind::Artist,
            navia_core::library::SearchKind::Album => SearchKind::Album,
            navia_core::library::SearchKind::Song => SearchKind::Song,
        }
    }
}

#[derive(uniffi::Record)]
pub struct LibraryCounts {
    pub artists: u64,
    pub albums: u64,
    pub songs: u64,
}

#[derive(uniffi::Record)]
pub struct CoverArtRow {
    pub path: String,
    pub width: u32,
    pub height: u32,
    pub tint_rgb: Vec<u8>,
    /// the server's no-cover placeholder: show it, but ask again after `placeholder_ttl_seconds()`,
    /// since a real cover may have been added by then.
    pub placeholder: bool,
}

/// how long a placeholder cover (`CoverArtRow.placeholder`) may be shown before it's asked for again.
#[uniffi::export]
pub fn placeholder_ttl_seconds() -> u32 {
    navia_core::artwork::NO_STORE_TTL.as_secs() as u32
}

#[derive(uniffi::Record)]
pub struct GlowRow {
    pub path: String,
    pub width: u32,
    pub height: u32,
}

#[derive(uniffi::Enum)]
pub enum SongSort {
    Title,
    RecentlyAdded,
    MostPlayed,
}

impl From<SongSort> for CoreSongSort {
    fn from(s: SongSort) -> Self {
        match s {
            SongSort::Title => CoreSongSort::Title,
            SongSort::RecentlyAdded => CoreSongSort::RecentlyAdded,
            SongSort::MostPlayed => CoreSongSort::MostPlayed,
        }
    }
}

#[derive(uniffi::Enum)]
pub enum AlbumSort {
    Name,
    RecentlyAdded,
    MostPlayed,
}

impl From<AlbumSort> for CoreAlbumSort {
    fn from(s: AlbumSort) -> Self {
        match s {
            AlbumSort::Name => CoreAlbumSort::Name,
            AlbumSort::RecentlyAdded => CoreAlbumSort::RecentlyAdded,
            AlbumSort::MostPlayed => CoreAlbumSort::MostPlayed,
        }
    }
}

#[derive(Clone, uniffi::Record)]
pub struct QueueItem {
    pub id: String,
    pub duration_ms: Option<u64>,
    /// Queued by the listener in Next in Queue, rather than part of the list playing.
    #[uniffi(default = false)]
    pub queued: bool,
}

impl From<QueueItem> for CoreQueueItem {
    fn from(i: QueueItem) -> Self {
        CoreQueueItem::new(i.id, i.duration_ms).queued(i.queued)
    }
}

impl From<CoreQueueItem> for QueueItem {
    fn from(i: CoreQueueItem) -> Self {
        QueueItem { id: i.id, duration_ms: i.duration_ms, queued: i.queued }
    }
}

/// What the queue plays from, for its "Next from: <source>" heading.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct QueueSource {
    pub name: String,
    pub radio: bool,
    pub link: Option<SourceLink>,
}

/// The page a queue source links to.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum SourceLink {
    Album { id: String },
    Playlist { id: String },
    Artist { id: String },
    ArtistTop { id: String },
    Genre { name: String },
    Mix { id: String },
    Liked,
    Songs,
}

impl From<QueueSource> for navia_core::queue_order::QueueSource {
    fn from(s: QueueSource) -> Self {
        use navia_core::queue_order::SourceLink as L;
        let link = s.link.map(|l| match l {
            SourceLink::Album { id } => L::Album(id),
            SourceLink::Playlist { id } => L::Playlist(id),
            SourceLink::Artist { id } => L::Artist(id),
            SourceLink::ArtistTop { id } => L::ArtistTop(id),
            SourceLink::Genre { name } => L::Genre(name),
            SourceLink::Mix { id } => L::Mix(id),
            SourceLink::Liked => L::Liked,
            SourceLink::Songs => L::Songs,
        });
        Self { name: s.name, radio: s.radio, link }
    }
}

impl From<navia_core::queue_order::QueueSource> for QueueSource {
    fn from(s: navia_core::queue_order::QueueSource) -> Self {
        use navia_core::queue_order::SourceLink as L;
        let link = s.link.map(|l| match l {
            L::Album(id) => SourceLink::Album { id },
            L::Playlist(id) => SourceLink::Playlist { id },
            L::Artist(id) => SourceLink::Artist { id },
            L::ArtistTop(id) => SourceLink::ArtistTop { id },
            L::Genre(name) => SourceLink::Genre { name },
            L::Mix(id) => SourceLink::Mix { id },
            L::Liked => SourceLink::Liked,
            L::Songs => SourceLink::Songs,
        });
        Self { name: s.name, radio: s.radio, link }
    }
}

#[derive(uniffi::Enum)]
pub enum RepeatMode {
    Off,
    All,
    One,
}

impl From<RepeatMode> for CoreRepeat {
    fn from(r: RepeatMode) -> Self {
        match r {
            RepeatMode::Off => CoreRepeat::Off,
            RepeatMode::All => CoreRepeat::All,
            RepeatMode::One => CoreRepeat::One,
        }
    }
}

impl From<CoreRepeat> for RepeatMode {
    fn from(r: CoreRepeat) -> Self {
        match r {
            CoreRepeat::Off => RepeatMode::Off,
            CoreRepeat::All => RepeatMode::All,
            CoreRepeat::One => RepeatMode::One,
        }
    }
}

#[derive(uniffi::Enum)]
pub enum PlaybackStatus {
    Stopped,
    Buffering,
    Playing,
    Paused,
}

impl From<CoreStatus> for PlaybackStatus {
    fn from(s: CoreStatus) -> Self {
        match s {
            CoreStatus::Stopped => PlaybackStatus::Stopped,
            CoreStatus::Buffering => PlaybackStatus::Buffering,
            CoreStatus::Playing => PlaybackStatus::Playing,
            CoreStatus::Paused => PlaybackStatus::Paused,
        }
    }
}

#[derive(uniffi::Enum)]
pub enum OutputPolicy {
    FollowTrack,
    DeviceDefault,
    Fixed { rate: u32 },
}

impl From<OutputPolicy> for CorePolicy {
    fn from(p: OutputPolicy) -> Self {
        match p {
            OutputPolicy::FollowTrack => CorePolicy::FollowTrack,
            OutputPolicy::DeviceDefault => CorePolicy::DeviceDefault,
            OutputPolicy::Fixed { rate } => CorePolicy::Fixed(rate),
        }
    }
}

#[derive(uniffi::Record)]
pub struct OutputDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

/// One filter of the equalizer.
#[derive(uniffi::Enum)]
pub enum EqFilterShape {
    Peaking,
    LowShelf,
    HighShelf,
}

#[derive(uniffi::Record)]
pub struct EqBandRow {
    pub shape: EqFilterShape,
    pub freq_hz: f32,
    pub gain_db: f32,
    pub q: f32,
}

/// The equalizer's stages in processing order. None on `set_eq` means bypassed and bit-exact.
#[derive(uniffi::Record)]
pub struct EqSettingsRow {
    pub preamp_db: f32,
    pub bands: Vec<EqBandRow>,
    pub limiter: bool,
    /// -1 is full left, 1 is full right.
    pub balance: f32,
    pub mono: bool,
    /// 0 to 1, or None when off.
    pub crossfeed: Option<f32>,
}

#[derive(uniffi::Record)]
pub struct PlayerSnapshot {
    pub status: PlaybackStatus,
    pub index: Option<u64>,
    pub item: Option<QueueItem>,
    pub position_ms: u64,
    pub volume: f32,
    pub muted: bool,
    pub repeat_mode: RepeatMode,
    pub crossfade_ms: u32,
    pub queue: Vec<QueueItem>,
    pub output_rate: u32,
    pub device: Option<String>,
    pub underrun_frames: u64,
    pub rebuffer_frames: u64,
    pub xruns: u64,
    /// A live stream is playing instead of the queue.
    pub live: bool,
    /// True while playback is waiting for the server to come back. Buffered audio plays first.
    pub waiting_for_server: bool,
    /// What the song is being delivered as, like `MP3 192`. Empty for the original file.
    pub delivered: String,
    /// Why the chosen quality couldn't be used, when the original plays instead.
    pub delivered_note: Option<String>,
    /// What the queue plays from.
    pub source: Option<QueueSource>,
}

/// Where the audible song's bytes come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum PlaySource {
    Stream,
    Cache,
    Download,
}

/// What the player is decoding, for the quality badge: the source, and the format and kbps the server transcoded to.
/// Both are None for the original file.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DeliveryRow {
    pub source: PlaySource,
    pub transcode_format: Option<String>,
    pub transcode_kbps: Option<u32>,
}

/// A queued song that played and left the queue: its index, and its number for `ack_left`.
#[derive(uniffi::Record)]
pub struct LeftQueue {
    pub index: u64,
    pub seq: u64,
}

#[derive(uniffi::Enum)]
pub enum PlayerEventRow {
    /// `left` is the queued song heard before this one, which left the queue at that index. `index` already accounts
    /// for it.
    TrackChanged { index: u64, item: QueueItem, left: Option<LeftQueue> },
    StatusChanged { status: PlaybackStatus },
    Position { ms: u64 },
    Error { message: String },
    /// The output device couldn't start, which isn't the song's fault. `device` is its name when known.
    OutputError { device: Option<String>, message: String },
    QueueChanged,
    QueueEnded,
    /// `device_rate`: what the device runs at, when the output can tell.
    OutputChanged { rate: u32, device_rate: Option<u32>, device: Option<String> },
    /// The player started or stopped waiting for the server during an outage.
    Connection { waiting: bool, message: Option<String> },
    /// Next or Previous with Repeat one switched it to Repeat all.
    RepeatChanged { mode: RepeatMode },
}

fn event_row(event: &PlayerEvent) -> Option<PlayerEventRow> {
    Some(match event {
        PlayerEvent::TrackChanged { index, item, left } => PlayerEventRow::TrackChanged { index: *index as u64, item: item.clone().into(), left: left.map(|l| LeftQueue { index: l.index as u64, seq: l.seq }) },
        PlayerEvent::StatusChanged(s) => PlayerEventRow::StatusChanged { status: (*s).into() },
        PlayerEvent::Position { ms } => PlayerEventRow::Position { ms: *ms },
        PlayerEvent::Error(m) => PlayerEventRow::Error { message: m.clone() },
        PlayerEvent::OutputError { device, message } => PlayerEventRow::OutputError { device: device.clone(), message: message.clone() },
        PlayerEvent::QueueChanged => PlayerEventRow::QueueChanged,
        PlayerEvent::QueueEnded => PlayerEventRow::QueueEnded,
        PlayerEvent::OutputChanged { rate, device_rate, device } => PlayerEventRow::OutputChanged { rate: *rate, device_rate: *device_rate, device: device.clone() },
        PlayerEvent::Connection { waiting, message } => PlayerEventRow::Connection { waiting: *waiting, message: message.clone() },
        PlayerEvent::RepeatChanged(r) => PlayerEventRow::RepeatChanged {
            mode: match r {
                CoreRepeat::Off => RepeatMode::Off,
                CoreRepeat::All => RepeatMode::All,
                CoreRepeat::One => RepeatMode::One,
            },
        },
        // The receiver's volume reaches the Mac as `AirPlayRemote::VolumeChanged`.
        PlayerEvent::Heard { .. } | PlayerEvent::PlayEnded { .. } | PlayerEvent::DeviceVolume(_) => return None,
    })
}

/// Receives player events on a background thread.
#[uniffi::export(callback_interface)]
pub trait PlayerListener: Send + Sync {
    fn on_event(&self, event: PlayerEventRow);
}

/// The audio player. Commands return immediately. Observe state with [`Self::snapshot`] or a listener.
#[derive(uniffi::Object)]
pub struct NaviaPlayer {
    pub(crate) inner: Player,
    /// Set for sample-accurate test runs. See [`NaviaCore::make_player`].
    capture: Option<Arc<CaptureBuffer>>,
}

#[uniffi::export]
impl NaviaPlayer {
    pub fn set_listener(&self, listener: Box<dyn PlayerListener>) {
        let listener = Mutex::new(listener);
        self.inner.add_listener(move |event| {
            if let Some(row) = event_row(event)
                && let Ok(l) = listener.lock()
            {
                l.on_event(row);
            }
        });
    }

    pub fn set_queue(&self, items: Vec<QueueItem>, start_index: u64, start_ms: u64, paused: bool) {
        self.inner.set_queue(items.into_iter().map(Into::into).collect(), start_index as usize, start_ms, paused);
    }

    pub fn append(&self, items: Vec<QueueItem>) {
        self.inner.append(items.into_iter().map(Into::into).collect());
    }

    pub fn insert_next(&self, items: Vec<QueueItem>) {
        self.inner.insert_next(items.into_iter().map(Into::into).collect());
    }

    pub fn remove(&self, index: u64) {
        self.inner.remove(index as usize);
    }

    /// Removes the entries at `positions` (queue indexes) in one edit. Audio already decoded for a removed song is
    /// replaced at the join, so playback stays gapless.
    pub fn remove_items(&self, positions: Vec<u64>) {
        self.inner.remove_items(positions.into_iter().map(|p| p as usize).collect());
    }

    pub fn move_item(&self, from: u64, to: u64) {
        self.inner.move_item(from as usize, to as usize);
    }

    /// Moves the entries at `positions` (queue indexes), in queue order, before the entry now at `before`. Use the
    /// queue length for the end. Audio decoded for a song that no longer follows the current one is replaced at the
    /// join, so playback stays gapless.
    pub fn move_items(&self, positions: Vec<u64>, before: u64) {
        self.inner.move_items(positions.into_iter().map(|p| p as usize).collect(), before as usize);
    }

    pub fn clear(&self) {
        self.inner.clear();
    }

    /// Marks entries (queue indexes) as queued by the listener or as part of the list.
    pub fn set_queued(&self, positions: Vec<u64>, queued: bool) {
        self.inner.set_queued(positions.into_iter().map(|p| p as usize).collect(), queued);
    }

    /// Clear Queue. Removes the queued songs after the current one.
    pub fn clear_queued(&self) {
        self.inner.clear_queued();
    }

    /// The app's queue caught up with the left song numbered `seq`. Until then its indexes are rebased.
    pub fn ack_left(&self, seq: u64) {
        self.inner.ack_left(seq);
    }

    /// Shuffle Queue. Mixes the queued songs once, as `shuffled_queued_order` with this `seed`.
    pub fn shuffle_queued(&self, seed: u64) {
        self.inner.shuffle_queued(seed);
    }

    /// Clear <source>. Removes the list after the queued songs, keeping Autoplay's last `keep_tail` entries.
    pub fn clear_list(&self, keep_tail: u64) {
        self.inner.clear_list(keep_tail as usize);
    }

    /// Inserts items at queue index `index`, for Undo after a clear.
    pub fn insert_at(&self, index: u64, items: Vec<QueueItem>) {
        self.inner.insert_at(index as usize, items.into_iter().map(Into::into).collect());
    }

    /// What the queue plays from. Set with each new list.
    pub fn set_source(&self, source: Option<QueueSource>) {
        self.inner.set_source(source.map(Into::into));
    }

    pub fn source(&self) -> Option<QueueSource> {
        self.inner.source().map(Into::into)
    }

    /// Replaces everything after the current track while keeping what's already decoding, for toggling shuffle
    /// mid-playback.
    pub fn replace_upcoming(&self, items: Vec<QueueItem>) {
        self.inner.replace_upcoming(items.into_iter().map(Into::into).collect());
    }

    pub fn jump_to(&self, index: u64) {
        self.inner.jump_to(index as usize);
    }

    pub fn play(&self) {
        self.inner.play();
    }

    pub fn pause(&self) {
        self.inner.pause();
    }

    pub fn toggle(&self) {
        self.inner.toggle();
    }

    pub fn seek_ms(&self, ms: u64) {
        self.inner.seek_ms(ms);
    }

    pub fn next(&self) {
        self.inner.next();
    }

    pub fn previous(&self) {
        self.inner.previous();
    }

    pub fn stop(&self) {
        self.inner.stop();
    }

    pub fn set_volume(&self, volume: f32) {
        self.inner.set_volume(volume);
    }

    pub fn set_muted(&self, muted: bool) {
        self.inner.set_muted(muted);
    }

    pub fn set_repeat(&self, mode: RepeatMode) {
        self.inner.set_repeat(mode.into());
    }

    pub fn set_crossfade_ms(&self, ms: u32) {
        self.inner.set_crossfade_ms(ms);
    }

    /// Listening along with someone. Crossfade is off while it lasts so their song changes cut in at once. Turn it on
    /// before loading their song and off when following ends.
    pub fn set_following(&self, on: bool) {
        self.inner.set_following(on);
    }

    /// a listening session waiting on the other side's song: the queue ends after the playing
    /// song instead of going on; Next still moves on.
    pub fn set_hold_after_current(&self, on: bool) {
        self.inner.set_hold_after_current(on);
    }

    /// Smart crossfade: with a crossfade set, songs that continue an album in order join gapless.
    pub fn set_smart_crossfade(&self, on: bool) {
        self.inner.set_smart_crossfade(on);
    }

    /// Smart transitions: with a crossfade set, each join's length and place come from the
    /// songs' measured starts and ends, with beat alignment, a bass swap and loudness matching.
    pub fn set_smart_transitions(&self, on: bool) {
        self.inner.set_smart_transitions(on);
    }

    pub fn set_output_device(&self, device: Option<String>) {
        self.inner.set_output_device(device);
    }

    /// The equalizer's chain, or None for unprocessed bit-exact playback.
    pub fn set_eq(&self, settings: Option<EqSettingsRow>) {
        use navia_core::audio::processing::{EqBand, EqSettings, FilterShape};
        let settings = settings.map(|s| EqSettings {
            preamp_db: s.preamp_db,
            bands: s.bands.into_iter().map(|b| EqBand { shape: match b.shape { EqFilterShape::Peaking => FilterShape::Peaking, EqFilterShape::LowShelf => FilterShape::LowShelf, EqFilterShape::HighShelf => FilterShape::HighShelf }, freq_hz: b.freq_hz, gain_db: b.gain_db, q: b.q }).collect(),
            limiter: s.limiter,
            balance: s.balance,
            mono: s.mono,
            crossfeed: s.crossfeed,
        });
        self.inner.set_eq(settings.as_ref());
    }

    /// Plays an internet radio stream in place of the queue. `stop` ends it and the queue resumes.
    pub fn play_live(&self, item: QueueItem, url: String) {
        self.inner.play_live(item.into(), url);
    }

    /// Tries the server again now instead of waiting for the next retry.
    pub fn retry_now(&self) {
        self.inner.retry_now();
    }

    /// the song a live stream says is playing, when it says.
    pub fn stream_title(&self) -> Option<String> {
        self.inner.stream_title()
    }

    pub fn set_output_policy(&self, policy: OutputPolicy) {
        self.inner.set_output_policy(policy.into());
    }

    /// The position without locking the queue, cheap enough for a UI tick.
    pub fn quick_position_ms(&self) -> u64 {
        self.inner.quick_state().1
    }

    /// A test capture's samples as a 32-bit float WAV file. Returns its frame count.
    pub fn save_capture(&self, path: String) -> Result<u64, NaviaError> {
        let Some(capture) = &self.capture else { return Err(NaviaError::Failed { message: "not a capture player".into() }) };
        let samples = capture.samples();
        let rate = capture.opens().first().map_or(48_000, |(r, _)| *r);
        write_float_wav(&path, &samples, rate).map_err(|e| NaviaError::Failed { message: e.to_string() })?;
        Ok(samples.len() as u64 / 2)
    }

    /// Where the audible song plays from and the server's transcoding, for the quality badge. None until a song has
    /// opened.
    pub fn delivery(&self) -> Option<DeliveryRow> {
        self.inner.delivered().map(|d| {
            let d = navia_core::audio_quality::Delivery::of(&d);
            DeliveryRow {
                source: match d.source {
                    navia_core::audio_quality::Source::Stream => PlaySource::Stream,
                    navia_core::audio_quality::Source::Cache => PlaySource::Cache,
                    navia_core::audio_quality::Source::Download => PlaySource::Download,
                },
                transcode_format: d.transcode.map(|(codec, _)| codec.label().to_string()),
                transcode_kbps: d.transcode.map(|(_, kbps)| kbps),
            }
        })
    }

    pub fn snapshot(&self) -> PlayerSnapshot {
        let s = self.inner.snapshot();
        PlayerSnapshot {
            status: s.status.into(),
            index: s.index.map(|i| i as u64),
            item: s.item.map(Into::into),
            position_ms: s.position_ms,
            volume: s.volume,
            muted: s.muted,
            repeat_mode: s.repeat.into(),
            crossfade_ms: s.crossfade_ms,
            queue: s.queue.into_iter().map(Into::into).collect(),
            output_rate: s.output_rate,
            device: s.device,
            underrun_frames: s.underrun_frames,
            rebuffer_frames: s.rebuffer_frames,
            xruns: s.xruns,
            live: s.live,
            waiting_for_server: s.waiting_for_server,
            delivered: s.delivered.as_ref().map(|d| d.label()).unwrap_or_default(),
            delivered_note: s.delivered.as_ref().and_then(|d| d.fallback_reason()),
            source: s.source.map(Into::into),
        }
    }
}

/// One signed-in server. Create it with [`connect_session`].
#[derive(uniffi::Object)]
pub struct NaviaSession {
    session: Session,
}

/// Connects to a server and opens its local library. Fails only on connection or auth problems.
#[uniffi::export(async_runtime = "tokio")]
pub async fn connect_session(server_url: String, username: String, password: String, read_only: bool, data_dir: Option<String>) -> Result<Arc<NaviaSession>, NaviaError> {
    let (library_path, artwork_dir) = match &data_dir {
        Some(dir) => (Some(std::path::Path::new(dir).join("library.sqlite")), Some(std::path::Path::new(dir).join("artwork"))),
        None => (None, None),
    };
    let session = Session::connect(SessionConfig {
        server_url,
        credentials: Credentials::new(username, password),
        write_policy: if read_only { WritePolicy::ReadOnly } else { WritePolicy::Allowed },
        library_path,
        artwork_dir,
    })
    .await?;
    Ok(Arc::new(NaviaSession { session }))
}

#[uniffi::export(async_runtime = "tokio")]
impl NaviaSession {
    /// Syncs the catalog with the server, resuming an interrupted sync.
    pub async fn sync(&self) -> Result<(), NaviaError> {
        self.session.sync(&|_| {}).await?;
        Ok(())
    }

    pub async fn refresh(&self, force: bool) -> Result<bool, NaviaError> {
        Ok(matches!(self.session.refresh(force).await?, navia_core::library::sync::RefreshOutcome::Synced(_)))
    }

    pub async fn sync_playlists(&self) -> Result<u64, NaviaError> {
        Ok(self.session.sync_playlists().await?.listed as u64)
    }

    /// Fetches one playlist now, as when it's opened. Smart playlists only update when fetched.
    pub async fn refresh_playlist(&self, id: String) -> Result<(), NaviaError> {
        Ok(self.session.refresh_playlist(&id).await?)
    }

    pub async fn refresh_annotations(&self) -> Result<u64, NaviaError> {
        Ok(self.session.refresh_annotations().await?.stars_changed)
    }

    pub async fn cover(&self, cover_art_id: String, size: Option<u32>) -> Result<CoverArtRow, NaviaError> {
        let cover = self.session.artwork().cover(&cover_art_id, size).await?;
        Ok(CoverArtRow { path: cover.path.display().to_string(), width: cover.width, height: cover.height, tint_rgb: cover.tint.to_vec(), placeholder: cover.placeholder })
    }

    pub async fn glow(&self, cover_art_id: String, width: u32, height: u32) -> Result<GlowRow, NaviaError> {
        let glow = self.session.artwork().glow(&cover_art_id, width, height).await?;
        Ok(GlowRow { path: glow.path.display().to_string(), width: glow.width, height: glow.height })
    }

    pub async fn make_player(&self, device_id: Option<String>, policy: OutputPolicy) -> Result<Arc<NaviaPlayer>, NaviaError> {
        let player = self.session.player(PlayerConfig { device: device_id, policy: policy.into(), ..Default::default() })?;
        Ok(Arc::new(NaviaPlayer { inner: player, capture: None }))
    }

    pub fn counts(&self) -> Result<LibraryCounts, NaviaError> {
        let c = self.session.library().counts()?;
        Ok(LibraryCounts { artists: c.artists, albums: c.albums, songs: c.songs })
    }

    pub fn list_songs(&self, sort: SongSort, offset: u32, limit: u32) -> Result<Vec<SongRow>, NaviaError> {
        Ok(self.session.library().list_songs(sort.into(), offset, limit, None)?.into_iter().map(Into::into).collect())
    }

    pub fn list_albums(&self, sort: AlbumSort, offset: u32, limit: u32) -> Result<Vec<AlbumRow>, NaviaError> {
        Ok(self.session.library().list_albums(sort.into(), offset, limit, None)?.into_iter().map(Into::into).collect())
    }

    pub fn list_artists(&self, offset: u32, limit: u32) -> Result<Vec<ArtistRow>, NaviaError> {
        Ok(self.session.library().list_artists(offset, limit)?.into_iter().map(Into::into).collect())
    }

    pub fn album_songs(&self, album_id: String) -> Result<Vec<SongRow>, NaviaError> {
        Ok(self.session.library().album_songs(&album_id)?.into_iter().map(Into::into).collect())
    }

    pub fn search(&self, query: String, limit_per_kind: u32) -> Result<SearchResultsRow, NaviaError> {
        let r = self.session.library().search(&query, limit_per_kind, None)?;
        Ok(SearchResultsRow { artists: r.artists.into_iter().map(Into::into).collect(), albums: r.albums.into_iter().map(Into::into).collect(), songs: r.songs.into_iter().map(Into::into).collect(), best: r.best.map(Into::into) })
    }

    pub fn playlists(&self) -> Result<Vec<PlaylistRow>, NaviaError> {
        Ok(self.session.library().playlists()?.into_iter().map(Into::into).collect())
    }

    pub fn playlist_songs(&self, id: String) -> Result<Vec<SongRow>, NaviaError> {
        Ok(self.session.library().playlist(&id)?.map(|p| p.entry.into_iter().map(Into::into).collect()).unwrap_or_default())
    }

    pub fn song(&self, id: String) -> Result<Option<SongRow>, NaviaError> {
        Ok(self.session.library().song(&id)?.map(Into::into))
    }

    /// A queue item for a song the UI already has.
    pub fn queue_item(&self, song: SongRow) -> QueueItem {
        QueueItem { id: song.id, duration_ms: song.duration_seconds.map(|d| (d as u64).saturating_mul(1000)), queued: false }
    }

    pub fn set_private_mode(&self, private: bool) -> bool {
        // Reporting state is cheap to change, so do it on the caller's thread.
        let reporter = self.session.reporter();
        futures_block(async move { reporter.set_private(private).await });
        private
    }
}

fn futures_block<F: std::future::Future<Output = ()>>(f: F) {
    tokio_handle().block_on(f);
}

fn tokio_handle() -> &'static tokio::runtime::Runtime {
    static RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    RT.get_or_init(|| tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().expect("runtime"))
}

/// Writes interleaved stereo f32 samples as a WAV file, for comparing against ffmpeg decodes.
fn write_float_wav(path: &str, samples: &[f32], rate: u32) -> std::io::Result<()> {
    use std::io::Write;
    let data_len = (samples.len() * 4) as u32;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_len).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&3u16.to_le_bytes())?; // IEEE float
    f.write_all(&2u16.to_le_bytes())?;
    f.write_all(&rate.to_le_bytes())?;
    f.write_all(&(rate * 8).to_le_bytes())?;
    f.write_all(&8u16.to_le_bytes())?;
    f.write_all(&32u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_len.to_le_bytes())?;
    for s in samples {
        f.write_all(&s.to_le_bytes())?;
    }
    f.flush()
}

/// The order a list of `len` items gets when `positions` move before `before`, as old positions (`order[new] = old`).
/// Empty when nothing would move. It's the same move the player and playlist editor make, for lists the app keeps
/// itself.
#[uniffi::export]
pub fn moved_order(len: u64, positions: Vec<u64>, before: u64) -> Vec<u64> {
    let positions: Vec<usize> = positions.into_iter().map(|p| p as usize).collect();
    navia_core::queue_order::moved_order(len as usize, &positions, before as usize).unwrap_or_default().into_iter().map(|i| i as u64).collect()
}


/// a ready-made playlist cover: `id` is stable, `name` is its English catalog key.
#[derive(uniffi::Record, Clone, Debug)]
pub struct CoverStyle {
    pub id: String,
    pub name: String,
}

/// the ready-made playlist covers, in the order the New Playlist dialog offers them.
#[uniffi::export]
pub fn cover_styles() -> Vec<CoverStyle> {
    navia_core::covers::COVER_STYLES.iter().map(|s| CoverStyle { id: s.id.into(), name: s.name.into() }).collect()
}

/// pixels on a side of the image uploaded for a chosen cover.
#[uniffi::export]
pub fn cover_export_px() -> u32 {
    navia_core::covers::COVER_EXPORT_PX
}

/// a cover style with the playlist's title, `size` pixels square, as a PNG. `None` for an unknown style.
#[uniffi::export]
pub fn render_cover_png(style: String, title: String, size: u32) -> Option<Vec<u8>> {
    navia_core::covers::render_cover_png(&style, &title, size)
}

#[uniffi::export]
pub fn navia_core_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Output devices the system offers.
#[uniffi::export]
pub fn output_devices() -> Result<Vec<OutputDevice>, NaviaError> {
    Ok(navia_core::audio::player::Player::list_devices()?.into_iter().map(|d| OutputDevice { id: d.id, name: d.name, is_default: d.is_default }).collect())
}
