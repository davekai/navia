//! The root view: app state, layout decisions, and the window frame.

use crate::i18n::n_;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::channel::mpsc::unbounded;
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, Context, CursorStyle, Decorations, Entity, FocusHandle, KeyBinding, KeyDownEvent, Pixels, RenderImage,
    ResizeEdge, SharedString, Subscription, Tiling, UniformListScrollHandle, WeakEntity, Window, WindowButtonLayout, WindowControls, actions, canvas, div, point, px,
    relative,
};

use crate::art::{ArtEvent, ArtWorkers, DockBake, DockGeometry, Job};
use crate::core_library::Startup;
use crate::focus;
use crate::assets::FONT_FAMILY;
use crate::chrome::{self, ButtonStyle};
use crate::color::Srgb;
use crate::core_library::CoreLibrary;
use crate::core_player::Core;
use crate::lyrics::LyricRaster;
use crate::model::{Library, LyricLine, PlaylistSummary, Song};
use crate::view_lyrics::LyricsState;
use crate::persist::{DockView, PlayerMode, Prefs};
use crate::selection::Selection;
use crate::testing::{self, FrameClock, Sample, ScriptAction};
use crate::theme::size::*;
use crate::theme::{BASE_SCALE, Theme, motion};
use crate::thumb_cache::ThumbCache;

actions!(navia, [ZoomIn, ZoomOut, ZoomReset, Dismiss, FocusSearch, PlaySelected, OpenSettings, GetInfo, ToggleMiniPlayer, CheckForUpdates, Quit]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-=", ZoomIn, None),
        KeyBinding::new("ctrl-+", ZoomIn, None),
        KeyBinding::new("ctrl--", ZoomOut, None),
        KeyBinding::new("ctrl-0", ZoomReset, None),
        KeyBinding::new("escape", Dismiss, None),
        KeyBinding::new("ctrl-f", FocusSearch, None),
        KeyBinding::new("ctrl-,", OpenSettings, None),
        KeyBinding::new("ctrl-i", GetInfo, None),
        KeyBinding::new("enter", PlaySelected, None),
    ]);
}

/// Whether an empty library really means empty, or that nothing is known yet.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LibraryState {
    /// The local library is still opening.
    Opening,
    /// Connected to a server and the first sync is running.
    Syncing,
    /// A sync finished, so empty pages really are empty.
    Ready,
    /// No server is set up.
    NoServer,
    /// The server can't be reached.
    Unreachable,
    /// The server answered but the sync failed.
    SyncFailed,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SortKey {
    MostPlayed,
    /// Newest first, as Recently Added opens it.
    Added,
    Title,
    Artist,
    Album,
    Year,
    Length,
}

impl SortKey {
    /// The list's own order by date added first, then the column sorts, then the others.
    pub const ALL: [(SortKey, &'static str); 7] = [
        (SortKey::Added, n_("Date Added")),
        (SortKey::Title, n_("Title")),
        (SortKey::Artist, n_("Artist")),
        (SortKey::Album, n_("Album")),
        (SortKey::Length, n_("Length")),
        (SortKey::MostPlayed, n_("Most Played")),
        (SortKey::Year, n_("Year")),
    ];
}

pub use crate::pages::Route;

/// the bar's progress line rising after `BAR_SCRUB_DELAY`; `shown` is how far it's up this frame (0 to 1), eased.
#[derive(Default)]
pub struct BarRaise {
    pub raised: bool,
    pub over_block: bool,
    pub generation: u64,
    pub shown: f32,
}

/// the pointer is on the bar's speaker or its panel; leaving both closes the panel after a moment so the gap can be crossed.
#[derive(Default)]
pub struct BarVolumeHover {
    pub open: bool,
    pub speaker: bool,
    pub panel: bool,
    pub generation: u64,
}

/// Which list a selection belongs to. Selecting in another list starts over.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ListId {
    Songs,
    Queue,
    Page(usize),
    /// The Cached Songs list.
    Cached,
}

pub struct Player {
    pub queue: Vec<u32>,
    pub queue_pos: usize,
    pub song_ix: Option<usize>,
    pub position: f32,
    pub playing: bool,
    /// Off, all, or one. With one the song repeats at its end, but Next and Previous still move.
    pub repeat: navia_core::audio::player::Repeat,
    pub shuffle: bool,
    /// With a real player, the current song from the engine. `song_ix` is then its row id.
    pub current: Option<Song>,
    pub status: navia_core::audio::player::PlayerStatus,
    /// The internet radio station playing instead of the queue, which waits underneath.
    pub station: Option<crate::model::RadioStation>,
    /// The player is holding its place until the server comes back.
    pub waiting_for_server: bool,
    /// How many listener-queued songs wait right after the current one. They play before the rest
    /// of the list, and shuffle leaves them in place.
    pub queued_run: usize,
    /// The current song was queued by the listener. It leaves the queue once another plays, so a
    /// saved session keeps this.
    pub current_queued: bool,
    /// songs at the queue's end that Autoplay handed the player and haven't started; shown under Autoplay, not Up Next.
    pub auto_tail: usize,
    /// What the queue plays from, for "Next from: <source>". `None` shows "Up Next".
    pub source: Option<navia_core::queue_order::QueueSource>,
    /// The AirPlay receiver's own volume while it's the output, 0 to 100 like `prefs.volume`. The
    /// slider shows and moves this instead, and `prefs.volume` stays for this computer's devices.
    pub device_volume: Option<f32>,
    /// the output device's own rate while it's known (not through the Linux sound server)
    pub device_rate: Option<u32>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Drag {
    Seek,
    Volume,
    /// Resizing the dock. Holds the pointer's offset from the dock's left edge at the press.
    DockWidth { grab_offset: Pixels },
    /// Resizing the sidebar. Holds the pointer's offset from its right edge at the press.
    SidebarWidth { grab_offset: Pixels },
    /// Dragging a scrollbar's thumb, with where on the thumb it was grabbed.
    Scroll { id: &'static str, grab: Pixels },
    /// A slider or the equalizer graph. `Shell::pointer_drag_move` runs its handler.
    Pointer,
    /// A press on a sidebar playlist row. The state is in `Shell::sidebar_drag`.
    SidebarPlaylist,
    /// A press on a reorderable song row. The state is in `Shell::row_drag`.
    Rows,
}

struct Bench {
    phase: usize,
    phase_frame: usize,
    offset: f32,
    rng: u32,
    report: Vec<(String, Vec<Sample>)>,
}

#[derive(Clone, Copy)]
struct BenchPhase {
    name: &'static str,
    /// Pixels per frame. Negative means jump to a random row each frame.
    speed: f32,
    frames: usize,
}

const BENCH: [BenchPhase; 4] = [
    BenchPhase { name: "idle", speed: 0.0, frames: 120 },
    BenchPhase { name: "scroll 30 px/frame", speed: 30.0, frames: 600 },
    BenchPhase { name: "scroll 600 px/frame", speed: 600.0, frames: 600 },
    BenchPhase { name: "random jump each frame", speed: -1.0, frames: 600 },
];

#[derive(Clone)]
pub struct Layout {
    pub viewport: gpui::Size<Pixels>,
    pub scale: f32,
    /// Window width in token units.
    pub tokens_w: f32,
    pub mode: PlayerMode,
    /// False with no play history, so there's no dock and no bar.
    pub player: bool,
    /// Rendering inside the split albums view, which has a narrower gutter.
    pub embedded: bool,
    pub sidebar_visible: bool,
    pub sidebar_overlay: bool,
    pub dock_width: Pixels,
    /// The sidebar's drawn width. The user's choice, within what the window leaves.
    pub sidebar_width: Pixels,
    pub main_width: Pixels,
    /// `main_width` with the panels at rest, where a slide ends: grids lay out for it, so their cards and artwork
    /// sizes change once rather than every frame of a slide.
    pub settled_width: Pixels,
    /// how far the sidebar, side player and bar are in (0 to 1), sliding over `PANEL_SLIDE`; while one slides out
    /// it's still drawn.
    pub sidebar_shown: f32,
    /// the in-flow sidebar is on, or coming in; what moves between it and the top bar follows this rather than the slide.
    pub sidebar_in: bool,
    pub dock_shown: f32,
    pub bar_shown: f32,
    /// the room the in-flow sidebar and the side player take from the page, following their slides.
    pub sidebar_room: Pixels,
    pub dock_room: Pixels,
    /// a panel is sliding, so the shell asks for frames.
    pub panels_moving: bool,
}

impl Layout {
    pub fn clone_for_width(&self, width: Pixels) -> Self {
        let mut layout = self.clone();
        layout.main_width = width;
        layout.settled_width = width;
        layout.embedded = true;
        layout
    }
}

pub struct Shell {
    pub prefs: Prefs,
    pub theme: Theme,
    pub library: Option<Arc<dyn Library>>,
    pub songs: Arc<Vec<Song>>,
    pub search_keys: Arc<Vec<String>>,
    pub order: Vec<u32>,
    pub total: usize,
    pub sort: SortKey,
    /// A column sort set from the Songs header runs descending.
    pub sort_desc: bool,
    /// Whether the Songs table's Date Added header is the active sort, showing an arrow.
    pub songs_added_head: bool,
    pub query: String,
    pub compact: bool,
    pub trailing_rows: usize,
    pub search_active: bool,
    pub starred: HashSet<u32>,
    /// Favorited songs, most recently liked first. `starred` decides what's still in the list.
    pub liked_order: Vec<u32>,
    pub liked_view: crate::view_liked::LikedView,
    pub downloaded: HashSet<u32>,
    /// Storage settings, and which songs the song cache can play offline.
    pub storage: crate::view_storage::StorageState,
    pub playlists: Vec<PlaylistSummary>,
    pub route: Route,
    pub albums: Arc<Vec<crate::model::Album>>,
    pub artists: Arc<Vec<crate::model::Artist>>,
    pub playlists_full: Arc<Vec<crate::model::PlaylistDetails>>,
    pub new_playlist: Option<crate::view_new_playlist::NewPlaylist>,
    /// what a playlist being created gets once the server has made it (description, Public, its cover).
    pub new_playlist_after: Option<crate::view_new_playlist::AfterCreate>,
    /// the playlist whose first cover is uploading: its change needs no toast, and its file is removed after.
    pub cover_upload: Option<crate::view_new_playlist::CoverUpload>,
    /// AirPlay's dialogs, the lost receiver and script previews (`view_airplay.rs`).
    pub airplay_ui: crate::view_airplay::AirPlayUi,
    pub edit_playlist: Option<crate::view_edit_playlist::EditPlaylist>,
    pub public_pending: crate::view_edit_playlist::PublicPending,
    /// Share links, with the list and the Share or Edit Link dialog.
    pub shares: crate::view_shares::SharesState,
    pub genres: Arc<Vec<crate::model::Genre>>,
    pub artist_index: HashMap<SharedString, u32>,
    pub top_song_cache: HashMap<SharedString, Vec<u32>>,
    pub similar_cache: HashMap<SharedString, Vec<u32>>,
    /// Autoplay for this session, shared with the background work that queries the server.
    pub autoplay: std::sync::Arc<std::sync::Mutex<navia_core::autoplay::Autoplay>>,
    /// The songs Autoplay offers now, as library rows, after any it already handed the player.
    pub autoplay_rows: Vec<u32>,
    /// Bumped by every Autoplay recompute, so a late answer for an older queue is dropped.
    pub autoplay_generation: u64,
    /// The queue context the offer belongs to. A new context clears it.
    pub autoplay_context: u64,
    /// Whether the offer is final. Until then the queue says it's finding songs.
    pub autoplay_ready: bool,
    /// Autoplay has new seeds to ask the server about once the queue shows.
    pub autoplay_wants_fetch: bool,
    /// songs the server answered with no similar songs for, so Song Radio can hide.
    pub no_song_radio: HashSet<SharedString>,
    /// The source of the next list played, set by whoever starts it, such as a radio. Otherwise the page's.
    pub play_source: Option<navia_core::queue_order::QueueSource>,
    /// Counts lists started with `core_play_from`, so a late answer can tell it's still current.
    pub plays: u64,
    /// The play whose song is waiting for its radio. Autoplay holds off meanwhile.
    pub radio_pending: Option<u64>,
    /// Songs downloading now and how far along each is, from 0 to 1.
    pub download_progress: HashMap<u32, f32>,
    /// Synced playlists and albums and their progress.
    pub synced: crate::collection_download::Synced,
    pub album_track_cache: HashMap<SharedString, Vec<u32>>,
    pub album_stars: HashSet<SharedString>,
    pub artist_stars: HashSet<SharedString>,
    pub library_state: LibraryState,
    /// Whether the page drew the sync meter under its empty state this frame and the last. The
    /// sidebar card waits meanwhile, so progress shows in one place.
    pub sync_meter_now: std::cell::Cell<bool>,
    pub sync_meter_last: std::cell::Cell<bool>,
    pub page: crate::pages::Page,
    pub page_data: u64,
    pub shelf_counter: usize,
    pub page_numbers: Rc<Vec<Option<u32>>>,
    /// for the page tint.
    pub upcoming_cover: Option<SharedString>,
    pub page_countdown: bool,
    pub page_heat: crate::heat::PageHeat,
    pub page_mini_artist: bool,
    /// For You as last built, which its mix cards and the mix page read.
    pub for_you: std::sync::Arc<crate::model::ForYouPage>,
    /// The mix page's column sort. Mixes otherwise keep their own order.
    pub mix_sort: Option<crate::view_rows::ColumnSort>,
    /// Save as Playlist is in progress.
    pub saving_mix: bool,
    pub window_scale: f32,
    pub albums_view: crate::view_albums::AlbumsView,
    pub album_detail: crate::view_album::AlbumDetail,
    pub artist_detail: crate::view_artist::ArtistDetail,
    pub playlists_view: crate::view_playlists::PlaylistsView,
    pub genres_view: crate::view_browse::GenresView,
    pub artists_view: crate::view_artists::ArtistsView,
    pub playlist_detail: crate::view_playlist::PlaylistDetail,
    pub search_query: String,
    /// Settings' search field.
    pub settings_query: String,
    pub search_view: crate::view_search::SearchView,
    pub menu: Option<crate::view_menu::MenuState>,
    /// Which account button opened the current menu, if any.
    pub account_menu_open: Option<crate::view_account::AccountPlace>,
    /// The Albums sort the next route change starts with, before reverting to Most Played.
    pub albums_sort_on_open: Option<crate::view_albums::AlbumSort>,
    /// How each MORE text last wrapped, by id.
    pub more_measures: std::collections::HashMap<&'static str, std::rc::Rc<std::cell::RefCell<Option<crate::view_album::MoreMeasure>>>>,
    /// The sign-in screen, which replaces the whole shell while it's up.
    pub connect: Option<crate::view_connect::ConnectState>,
    pub confirm: Option<crate::view_connect::Confirm>,
    /// The Get Info sheet, while open.
    pub info: Option<crate::view_info::InfoSheet>,
    pub about: Option<crate::view_about::AboutSheet>,
    /// Get Info's link and button keys this frame, and the copy button last pressed.
    pub info_links: usize,
    pub info_copied: Option<(SharedString, Instant)>,
    /// The password couldn't be saved, so Settings says it will be asked for again.
    pub keyring_note: bool,
    /// This account's Deezer choice.
    pub deezer: crate::deezer_choice::DeezerChoice,
    /// This account's Autoplay mode, read when the account attaches.
    pub autoplay_mode: navia_core::engine::AutoplayMode,
    pub downloads_view: crate::view_downloads::DownloadsView,
    pub cached_view: crate::view_cached::CachedView,
    /// OS media controls and Discord, which outlive any one account.
    pub integrations: crate::core_player::Integrations,
    /// Bumped on attach and detach, so an old account's late events are ignored.
    pub core_generation: u64,
    art_events: futures::channel::mpsc::UnboundedSender<ArtEvent>,
    pub toast: Option<crate::view_menu::Toast>,
    pub toast_generation: u64,
    pub private_mode: bool,
    /// this account's offline switch, kept in `Prefs::offline_accounts`.
    pub offline_switch: bool,
    /// Listening Now and following someone (`listen_along.rs`).
    pub listen: crate::listen_along::ListenState,
    pub controls: crate::ui_controls::ControlState,
    pub eq: crate::eq_model::EqState,
    pub eq_ui: crate::view_equalizer::EqUi,
    pub smart: crate::view_smart::SmartEditor,
    /// Output devices from the audio engine, as (id, name).
    pub outputs: Vec<(String, String)>,
    pub outputs_requested: bool,
    /// Refreshes `outputs` when the sound server's devices change, on Linux.
    pub output_watch: Option<navia_core::audio::device_watch::DeviceWatch>,
    /// Formats the server can't transcode to, from the check run when Settings first opens.
    pub unavailable_formats: Vec<navia_core::quality::Codec>,
    pub format_check_started: bool,
    pub sample_mode: bool,
    pub quality_pop: crate::quality_badge::QualityPopover,
    /// The playlist table's Remove from This Playlist state, when its rows can be removed.
    pub playlist_remove: Option<crate::view_playlist::PlaylistRemove>,
    pub layout_override: Option<Pixels>,
    pub hero_bakes: crate::shell_data::BakeCache,
    bench_route: Option<Route>,
    pub history: Vec<Route>,
    pub history_pos: usize,
    /// counts page changes, and which way the last one went (1 forward, -1 Back), for the page's entrance.
    pub page_entry: (u64, f32),
    /// songs just queued or added to a playlist, and when, so their rows light and fade back (`ROW_FLASH`).
    pub row_flash: Option<(Rc<HashSet<u32>>, Instant)>,
    /// when the open page's content arrived after the page was shown, so its blocks fade in one after another.
    pub page_land: Option<Instant>,
    pub expanded: bool,
    pub sidebar_drawer: bool,
    pub selection: Selection,
    pub selection_list: ListId,
    pub player: Player,
    pub bar_volume: BarVolumeHover,
    pub bar_raise: BarRaise,
    pub dragging: Option<Drag>,
    pub seek_bounds: Rc<RefCell<Bounds<Pixels>>>,
    pub volume_bounds: Rc<RefCell<Bounds<Pixels>>>,
    pub art: Arc<ArtWorkers>,
    pub thumbs: ThumbCache,
    pub core: Option<Core>,
    pub frame: u64,
    /// A thumbnail is mid-fade, so another frame is needed.
    pub thumbs_fading: bool,
    pub dock_bake: Option<(SharedString, Arc<DockBake>)>,
    pub dock_requested: Option<(SharedString, DockGeometry)>,
    dock_generation: u64,
    /// How many dock bakes were requested, for the scripted drag check.
    dock_bake_requests: u64,
    pub tints: HashMap<SharedString, Srgb>,
    pub playlist_tints: crate::shell_data::PlaylistTints,
    pub lyrics: Vec<LyricLine>,
    /// the current song's lines as the server sent them, for preparing again when the backing line switch changes.
    pub lyrics_written: Vec<LyricLine>,
    /// the names after "Written By:" at the end of the lyrics, when the server credits any.
    pub lyrics_by: Option<SharedString>,
    pub lyrics_loading: bool,
    pub tint_requested: HashSet<SharedString>,
    /// The Songs page's filters.
    pub song_filters: crate::view_filters::LibraryFilters,
    /// The Artists page's filters.
    pub artist_filters: crate::view_filters::LibraryFilters,
    /// Save as Smart Playlist's rules, and a note on what didn't carry over, for the editor it opens.
    pub smart_prefill: Option<(String, Option<String>)>,
    pub lyrics_view: LyricsState,
    pub lyric_raster: LyricRaster,
    /// The user scrolled the lyrics. Following resumes at the next line.
    pub lyrics_user_scrolled: bool,
    /// Something in the lyrics is still easing, so another frame is needed.
    pub lyrics_animating: bool,
    /// the player's scrolling names, one view each (`marquee`).
    pub marquees: crate::marquee::Marquees,
    pub queue_list: gpui::ListState,
    pub(crate) queue_key: (usize, usize, usize, bool, bool, usize),
    pub(crate) lyrics_song: Option<SharedString>,
    pub expanded_bg: Option<(SharedString, u32, u32, Arc<RenderImage>)>,
    /// The expanded backdrop's brightness grid, for the text scrim.
    pub expanded_peaks: Option<Arc<crate::art::PeakGrid>>,
    /// The player root and the controls, as drawn last frame.
    pub expanded_controls: Rc<RefCell<[Bounds<Pixels>; 2]>>,
    pub expanded_requested: Option<(SharedString, u32, u32)>,
    pub bar_glow: Option<(SharedString, u32, u32, Arc<RenderImage>)>,
    pub bar_glow_requested: Option<(SharedString, u32, u32)>,
    pub list_scroll: UniformListScrollHandle,
    pub featured_scroll: gpui::ScrollHandle,
    pub dock_art_scroll: gpui::ScrollHandle,
    pub scrollbars: crate::ui_scroll::Scrollbars,
    pub scrollbar_animating: bool,
    pub lyrics_max_scroll: f32,
    pub sidebar_scroll: gpui::ScrollHandle,
    /// Indexes into `playlists` in sidebar order. The user's own, then other people's public playlists.
    pub sidebar_order: Vec<usize>,
    pub sidebar_shared: Vec<usize>,
    /// Folded sidebar groups for Library, Playlists and Shared. Not saved.
    pub sidebar_folded: [bool; 3],
    /// The soonest a visible thumbnail can be requested again, and whether a timer waits for it.
    pub next_thumb_retry: Option<Instant>,
    pub anim: crate::anim_art::AnimCache,
    pub thumb_retry_pending: bool,
    pub sidebar_drag: Option<crate::sidebar_playlists::SidebarDrag>,
    pub sidebar_autoscrolling: bool,
    /// Press-and-hold reordering of song rows.
    pub row_drag: Option<crate::row_drag::RowDrag>,
    /// Where drop targets for songs carried out of their list were last drawn.
    pub drop_marks: crate::song_drop::DropMarks,
    pub row_drag_seq: u64,
    /// A link in a song row took this press, so the row ignores it.
    pub link_pressed: Option<Instant>,
    /// A lifted row's list is scrolling at its edge, or its rows are still sliding to open the gap.
    pub rows_autoscrolling: bool,
    /// The queue list item of the first Up Next row, set as the queue renders.
    pub queue_rows_first: usize,
    /// The page song list whose rows can be dragged now, set by the playlist page.
    pub reorder_list: Option<usize>,
    /// This visit's Recommended section on a playlist page, and its rows' page list.
    pub recommended: Option<crate::view_recommended::Recommended>,
    pub recommended_list: Option<usize>,
    /// A playlist order shown while its save is in flight, by playlist.
    pub playlist_order_override: HashMap<SharedString, Vec<u32>>,
    /// the sidebar and main column as cached views (see `slots.rs`).
    pub slots: crate::slots::Slots,
    /// False while the system says the window isn't shown, such as minimized or on another desktop.
    /// A covered window still counts as shown on KWin and Windows. Visual-only work waits while
    /// playback carries on. See `set_window_shown`.
    pub window_shown: bool,
    /// The release that ended a drag mustn't also count as a click on the row.
    pub suppress_row_click: bool,
    pub focus: FocusHandle,
    /// What had keyboard focus when a dialog or the expanded player opened.
    focus_return: Option<FocusHandle>,
    modal_was: bool,
    /// This view's own handle, for input handlers and listeners outside `cx.listener`.
    pub this: WeakEntity<Shell>,
    script: Vec<(u64, ScriptAction)>,
    /// Whether the script's steps were scheduled on the first main window.
    script_attached: bool,
    script_started: Instant,
    pub clock: FrameClock,
    bench: Option<Bench>,
    pub csd: bool,
    /// The desktop blurs behind windows, so Translucency defaults on and its hint follows. The
    /// switch works either way.
    pub blur_detected: bool,
    /// the desktop's accent colour, followed live for the System accent choice.
    pub system_accent: Option<u32>,
    /// the desktop can draw the title bar (`xdg-decoration`), so Navia's own frame is a choice in Settings.
    pub system_frame_available: bool,
    /// The desktop has layer-shell, so the mini player is an overlay Navia moves.
    pub layer_shell_available: bool,
    /// The main window, which the mini player minimizes and brings back.
    pub main_window: Option<gpui::AnyWindowHandle>,
    pub mini: crate::mini_player::MiniPlayer,
    /// A frame change from Settings, sent to the compositor on the next frame.
    frame_request: Option<gpui::WindowDecorations>,
    /// What the compositor granted last frame, to log changes under `NAVIA_FRAME_LOG`.
    frame_granted: Option<(Decorations, gpui::Size<Pixels>)>,
    pub button_layout: WindowButtonLayout,
    pub button_style: ButtonStyle,
    pub window_controls: WindowControls,
    pub maximized: bool,
    /// The window's edge this frame, with shadow margin and corners per side. None when tiled.
    pub edge: chrome::Frame,
    /// The main window's input region as last set.
    input_region: Option<Option<gpui::Bounds<Pixels>>>,
    /// The main window's observers for button layout, activation and visibility. Recreated each
    /// time the window opens.
    _window_subs: Vec<Subscription>,
    pending_scroll: Option<f32>,
    /// The tray icon, whose commands arrive on the shell's own task.
    pub tray: Option<crate::tray::AppTray>,
    #[cfg(target_os = "linux")]
    pub app_menu: Option<crate::app_menu::AppMenu>,
    pub app_menu_state: Option<crate::app_menu::MenuState>,
    /// Settings > Open at login, as the desktop has it (`autostart.rs`).
    pub open_at_login: crate::autostart::OpenAtLogin,
    /// The last update check's outcome.
    pub update_status: crate::updates::UpdateStatus,
    /// The main window's size when it was last closed to the tray, so it comes back the same.
    pub closed_size: Option<gpui::Size<Pixels>>,
    /// Whether a tray host was present when last checked, and when. For Settings' hint.
    pub tray_host: (bool, Option<Instant>),
}

impl Shell {
    /// Creates the shell, which lives for the whole run with or without a window. Closing to the
    /// tray drops the window but keeps the player. `attach_window` connects a window.
    pub fn new(prefs: Prefs, support: chrome::Support, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let mut shell = Self::build(prefs, support, cx);
            shell.start_update_checks(cx);
            shell
        })
    }

    /// Connects a newly opened main window. Sets up observers, focus, the close button's tray or
    /// quit choice, and a scripted run's steps the first time.
    pub fn attach_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let button_layout_changed = cx.observe_button_layout_changed(window, |this, _, cx| {
            this.button_layout = chrome::current_layout(cx);
            cx.notify();
        });
        // The window shadow is lighter when the window isn't active.
        let activation_changed = cx.observe_window_activation(window, |this, _, cx| {
            this.marquees_refresh(cx);
            cx.notify()
        });
        let visibility_changed = cx.observe_window_visibility(window, |this, visibility, window, cx| this.window_visibility_changed(visibility.is_visible(), window, cx));
        self._window_subs = vec![button_layout_changed, activation_changed, visibility_changed];
        self.button_layout = chrome::current_layout(cx);
        window.focus(&self.focus, cx);
        crate::ui::note_backdrop_blur(window);
        self.main_window = Some(window.window_handle());
        #[cfg(target_os = "linux")]
        if let Some(menu) = &self.app_menu {
            menu.attach(window);
        }
        #[cfg(target_os = "windows")]
        {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(handle) = HasWindowHandle::window_handle(window) {
                if let RawWindowHandle::Win32(h) = handle.as_raw() {
                    self.integrations.hwnd = Some(h.hwnd.get() as usize);
                    // Media controls waited for this handle if the library attached first.
                    let target = self.integrations.target.lock().unwrap().clone();
                    if let Some(tx) = target {
                        self.ensure_media_controls(tx);
                    }
                }
            }
        }
        self.set_window_shown(window.is_visible(), cx);
        // Closed from the desktop, like Alt+F4, so make the same choice as Navia's own button.
        let this = self.this.clone();
        window.on_window_should_close(cx, move |window, cx| this.update(cx, |shell, cx| shell.main_window_closing(window, cx)).unwrap_or(true));
        if !self.script.is_empty() && !self.script_attached {
            self.script_attached = true;
            self.script_started = Instant::now();
            let times: Vec<u64> = self.script.iter().map(|(t, _)| *t).collect();
            cx.spawn_in(window, async move |this, cx| {
                let start = Instant::now();
                for t in times {
                    let due = Duration::from_millis(t).saturating_sub(start.elapsed());
                    cx.background_executor().timer(due).await;
                    if this.update_in(cx, |shell, window, cx| shell.run_script_step(t, window, cx)).is_err() {
                        return;
                    }
                }
            })
            .detach();
        }
    }

    fn build(prefs: Prefs, support: chrome::Support, cx: &mut Context<Self>) -> Self {
        let blur_detected = support.blur;
        let system_accent = crate::system_accent::read();
        Self::watch_system_accent(cx);
        let accent = if prefs.accent_system { crate::color::system_accent(system_accent) } else { prefs.accent };
        let theme = Theme::new(accent, prefs.zoom as f32 / 100.0, prefs.translucent_on(blur_detected));
        let button_layout = chrome::current_layout(cx);
        let focus = cx.focus_handle();

        let (tx, mut rx) = unbounded::<ArtEvent>();
        let art_events = tx.clone();
        let lyric_raster = LyricRaster::start(tx.clone());
        cx.spawn(async move |this, cx| {
            while let Some(event) = rx.next().await {
                if this.update(cx, |shell, cx| shell.apply_art_event(event, cx)).is_err() {
                    return;
                }
            }
        })
        .detach();

        // Art workers need a library handle, so they start when the library has loaded.
        let art = ArtWorkers::idle();

        cx.spawn(async move |this, cx| {
            let startup = cx.background_spawn(async { open_library() }).await;
            this.update(cx, |shell, cx| match startup {
                Startup::Library(lib, core) => shell.on_library(lib, core, cx),
                Startup::SignIn(prompt) => shell.show_sign_in(prompt, cx),
            })
            .ok();
        })
        .detach();

        // Without a real player, playback is simulated in the shell with no sound.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(200)).await;
                if this.update(cx, |shell, cx| shell.tick(cx)).is_err() {
                    return;
                }
            }
        })
        .detach();

        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if this.update(cx, |shell, cx| if shell.page_countdown { cx.notify() }).is_err() {
                    return;
                }
            }
        })
        .detach();

        // rows that change with the local day are rebuilt when the date changes while the app runs.
        cx.spawn(async move |this, cx| {
            use navia_core::library::taste::{local_day, now_unix};
            let mut day = local_day(now_unix());
            loop {
                cx.background_executor().timer(Duration::from_secs(30)).await;
                let today = local_day(now_unix());
                if today == day {
                    continue;
                }
                day = today;
                if this.update(cx, |shell, cx| shell.new_local_day(cx)).is_err() {
                    return;
                }
            }
        })
        .detach();

        let script = std::env::var("NAVIA_SCRIPT").ok().map(|s| testing::parse_script(&s)).unwrap_or_default();
        let tray = crate::tray_glue::start_tray(cx);
        #[cfg(target_os = "linux")]
        let app_menu = crate::tray_glue::start_app_menu(cx);
        let bench = std::env::var_os("NAVIA_SCROLL_BENCH").map(|_| Bench {
            phase: 0,
            phase_frame: 0,
            offset: 0.0,
            rng: 0x1234_5678,
            report: vec![(BENCH[0].name.to_string(), Vec::new())],
        });

        let order = Vec::new();
        let prefs_private = prefs.private_mode;
        Self {
            prefs,
            theme,
            library: None,
            songs: Arc::new(Vec::new()),
            search_keys: Arc::new(Vec::new()),
            order,
            total: 0,
            // The Songs page's own order is date added.
            sort: SortKey::Added,
            sort_desc: false,
            songs_added_head: false,
            query: String::new(),
            compact: false,
            trailing_rows: 1,
            search_active: false,
            starred: HashSet::new(),
            liked_order: Vec::new(),
            liked_view: Default::default(),
            downloaded: HashSet::new(),
            storage: Default::default(),
            playlists: Vec::new(),
            route: Route::Home,
            albums: Arc::default(),
            artists: Arc::default(),
            playlists_full: Arc::default(),
            new_playlist: None,
            new_playlist_after: None,
            cover_upload: None,
            airplay_ui: Default::default(),
            edit_playlist: None,
            public_pending: Default::default(),
            shares: Default::default(),
            genres: Arc::default(),
            artist_index: HashMap::new(),
            top_song_cache: HashMap::new(),
            similar_cache: HashMap::new(),
            autoplay: Default::default(),
            autoplay_rows: Vec::new(),
            autoplay_generation: 0,
            autoplay_context: 0,
            autoplay_ready: false,
            autoplay_wants_fetch: false,
            no_song_radio: HashSet::new(),
            plays: 0,
            play_source: None,
            radio_pending: None,
            download_progress: HashMap::new(),
            synced: Default::default(),
            album_track_cache: HashMap::new(),
            album_stars: HashSet::new(),
            artist_stars: HashSet::new(),
            library_state: LibraryState::Opening,
            sync_meter_now: Default::default(),
            sync_meter_last: Default::default(),
            page: crate::pages::Page::default(),
            page_data: 0,
            shelf_counter: 0,
            page_numbers: Rc::default(),
            upcoming_cover: None,
            page_countdown: false,
            page_heat: Default::default(),
            page_mini_artist: false,
            for_you: Default::default(),
            mix_sort: None,
            saving_mix: false,
            window_scale: 1.0,
            albums_view: Default::default(),
            album_detail: Default::default(),
            artist_detail: Default::default(),
            playlists_view: Default::default(),
            genres_view: Default::default(),
            artists_view: Default::default(),
            playlist_detail: Default::default(),
            search_query: String::new(),
            settings_query: String::new(),
            search_view: Default::default(),
            menu: None,
            account_menu_open: None,
            albums_sort_on_open: None,
            more_measures: Default::default(),
            connect: None,
            confirm: None,
            info: None,
            about: None,
            info_links: 0,
            info_copied: None,
            keyring_note: false,
            deezer: Default::default(),
            autoplay_mode: Default::default(),
            downloads_view: Default::default(),
            cached_view: Default::default(),
            integrations: Default::default(),
            core_generation: 0,
            art_events,
            toast: None,
            toast_generation: 0,
            private_mode: prefs_private,
            offline_switch: false,
            listen: Default::default(),
            controls: Default::default(),
            eq: crate::eq_model::EqState::load(),
            eq_ui: Default::default(),
            smart: Default::default(),
            outputs: Vec::new(),
            outputs_requested: false,
            output_watch: None,
            unavailable_formats: Vec::new(),
            format_check_started: false,
            sample_mode: std::env::var_os("NAVIA_SAMPLE").is_some(),
            playlist_remove: None,
            quality_pop: Default::default(),
            layout_override: None,
            hero_bakes: Default::default(),
            bench_route: None,
            history: vec![Route::Home],
            history_pos: 0,
            page_entry: (0, 1.0),
            row_flash: None,
            page_land: None,
            expanded: false,
            sidebar_drawer: false,
            selection: Selection::default(),
            selection_list: ListId::Songs,
            bar_volume: Default::default(),
            bar_raise: Default::default(),
            player: Player { queue: Vec::new(), queue_pos: 0, song_ix: None, position: 42.0, playing: false, repeat: navia_core::audio::player::Repeat::Off, shuffle: false, current: None, status: navia_core::audio::player::PlayerStatus::Stopped, station: None, waiting_for_server: false, queued_run: 0, current_queued: false, auto_tail: 0, source: None, device_volume: None, device_rate: None },
            dragging: None,
            seek_bounds: Rc::new(RefCell::new(Bounds::default())),
            volume_bounds: Rc::new(RefCell::new(Bounds::default())),
            art,
            thumbs: ThumbCache::default(),
            core: None,
            frame: 0,
            thumbs_fading: false,
            dock_bake: None,
            dock_requested: None,
            dock_generation: 0,
            dock_bake_requests: 0,
            tints: HashMap::new(),
            playlist_tints: Default::default(),
            lyrics: Vec::new(),
            lyrics_written: Vec::new(),
            lyrics_by: None,
            lyrics_loading: false,
            tint_requested: HashSet::new(),
            song_filters: Default::default(),
            artist_filters: Default::default(),
            smart_prefill: None,
            lyrics_view: LyricsState::default(),
            lyric_raster,
            lyrics_user_scrolled: false,
            lyrics_animating: false,
            marquees: Default::default(),
            queue_list: gpui::ListState::new(0, gpui::ListAlignment::Top, px(240.0)),
            queue_key: (usize::MAX, usize::MAX, usize::MAX, false, false, 0),
            lyrics_song: None,
            expanded_bg: None,
            expanded_peaks: None,
            expanded_controls: Rc::new(RefCell::new([Bounds::default(); 2])),
            expanded_requested: None,
            bar_glow: None,
            bar_glow_requested: None,
            list_scroll: UniformListScrollHandle::new(),
            featured_scroll: gpui::ScrollHandle::new(),
            dock_art_scroll: gpui::ScrollHandle::new(),
            scrollbars: Default::default(),
            scrollbar_animating: false,
            lyrics_max_scroll: 0.0,
            sidebar_scroll: gpui::ScrollHandle::new(),
            sidebar_order: Vec::new(),
            sidebar_shared: Vec::new(),
            sidebar_folded: [false; 3],
            next_thumb_retry: None,
            anim: Default::default(),
            thumb_retry_pending: false,
            sidebar_drag: None,
            sidebar_autoscrolling: false,
            row_drag: None,
            drop_marks: Default::default(),
            row_drag_seq: 0,
            link_pressed: None,
            rows_autoscrolling: false,
            queue_rows_first: 0,
            reorder_list: None,
            recommended: None,
            recommended_list: None,
            playlist_order_override: HashMap::new(),
            slots: crate::slots::Slots::new(cx),
            window_shown: false,
            suppress_row_click: false,
            focus,
            focus_return: None,
            modal_was: false,
            this: cx.entity().downgrade(),
            script,
            script_started: Instant::now(),
            clock: FrameClock::default(),
            bench,
            csd: false,
            blur_detected,
            system_accent,
            system_frame_available: support.system_frame,
            layer_shell_available: support.layer_shell,
            main_window: None,
            mini: Default::default(),
            frame_request: None,
            frame_granted: None,
            button_layout,
            button_style: ButtonStyle::detect(),
            window_controls: WindowControls::default(),
            maximized: false,
            edge: chrome::Frame::default(),
            input_region: None,
            _window_subs: Vec::new(),
            pending_scroll: None,
            tray,
            open_at_login: crate::autostart::current(),
            update_status: crate::updates::UpdateStatus::Idle,
            closed_size: None,
            tray_host: (true, None),
            #[cfg(target_os = "linux")]
            app_menu,
            app_menu_state: None,
            script_attached: false,
        }
    }

    pub fn on_library(&mut self, library: Arc<dyn Library>, core: Option<Arc<CoreLibrary>>, cx: &mut Context<Self>) {
        let tx = self.art_events.clone();
        if let Some(core) = core {
            self.art = self.art.replace_with(library.clone(), tx);
            self.load_catalog(&library);
            // From the local database, so the sidebar is complete even with the server down.
            self.playlists = library.playlists();
            self.library = Some(library);
            self.refresh_sidebar_order();
            self.library_state = LibraryState::Syncing;
            self.attach_core(core, cx);
            self.apply_playback_settings();
            self.watch_outputs(cx);
            cx.notify();
            return;
        }
        // Art workers were started before the library existed; restart them with the real one.
        self.art = self.art.replace_with(library.clone(), tx);
        self.songs = library.songs();
        self.total = self.songs.len();
        let songs = self.songs.clone();
        self.search_keys = Arc::new(songs.iter().map(|s| format!("{} {} {}", s.title, s.artist, s.album).to_lowercase()).collect());
        self.starred = songs.iter().enumerate().filter(|(_, s)| s.starred).map(|(i, _)| i as u32).collect();
        self.liked_order = songs.iter().enumerate().filter(|(_, s)| s.starred).map(|(i, _)| i as u32).collect();
        self.downloaded = songs.iter().enumerate().filter(|(_, s)| s.downloaded).map(|(i, _)| i as u32).collect();
        self.playlists = library.playlists();
        self.player.song_ix = library.now_playing();
        self.library_state = if self.sample_mode { LibraryState::Ready } else { LibraryState::NoServer };
        self.load_catalog(&library);
        self.library = Some(library);
        self.refresh_sidebar_order();
        self.rebuild_order();
        if let Some(name) = std::env::var_os("NAVIA_BENCH_ROUTE") {
            self.bench_route = Some(self.script_route(&name.to_string_lossy()));
        }
        self.request_tint(cx);
        cx.notify();
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        if self.core.is_some() {
            self.core_maintenance();
            return; // Real playback reports through player events.
        }
        if self.player.playing {
            self.player.position += 0.2;
            let Some(song) = self.current_song().cloned() else { return };
            if self.player.position >= song.duration_s as f32 {
                self.next(cx);
            }
            self.notify_visual(cx);
        }
    }

    /// Clears everything shown for the signed-in account, back to an empty, opening app.
    pub fn reset_account_view(&mut self, cx: &mut Context<Self>) {
        self.library = None;
        self.songs = Arc::default();
        self.search_keys = Arc::default();
        self.order.clear();
        self.total = 0;
        self.query.clear();
        self.search_active = false;
        self.starred.clear();
        self.liked_order.clear();
        self.liked_view = Default::default();
        self.downloaded.clear();
        self.playlists.clear();
        self.route = Route::Home;
        self.albums = Arc::default();
        self.artists = Arc::default();
        self.playlists_full = Arc::default();
        self.genres = Arc::default();
        self.artist_index.clear();
        self.top_song_cache.clear();
        self.similar_cache.clear();
        self.autoplay.lock().unwrap().reset();
        self.autoplay_rows.clear();
        self.autoplay_context = 0;
        self.autoplay_ready = false;
        self.autoplay_generation += 1;
        self.no_song_radio.clear();
        self.download_progress.clear();
        self.album_track_cache.clear();
        self.album_stars.clear();
        self.artist_stars.clear();
        self.library_state = LibraryState::Opening;
        self.page = crate::pages::Page::default();
        self.page_numbers = Rc::default();
        self.albums_view = Default::default();
        self.album_detail = Default::default();
        self.artist_detail = Default::default();
        self.playlists_view = Default::default();
        self.genres_view = Default::default();
        self.artists_view = Default::default();
        self.playlist_detail = Default::default();
        self.search_query.clear();
        self.search_view = Default::default();
        self.menu = None;
        self.confirm = None;
        self.clear_toast();
        self.smart = Default::default();
        self.hero_bakes = Default::default();
        self.history = vec![Route::Home];
        self.history_pos = 0;
        self.expanded = false;
        self.selection = Selection::default();
        self.player = Player { queue: Vec::new(), queue_pos: 0, song_ix: None, position: 42.0, playing: false, repeat: navia_core::audio::player::Repeat::Off, shuffle: false, current: None, status: navia_core::audio::player::PlayerStatus::Stopped, station: None, waiting_for_server: false, queued_run: 0, current_queued: false, auto_tail: 0, source: None, device_volume: None, device_rate: None };
        self.dock_bake = None;
        self.dock_requested = None;
        self.expanded_bg = None;
        self.expanded_peaks = None;
        self.expanded_requested = None;
        self.bar_glow = None;
        self.bar_glow_requested = None;
        self.tints.clear();
        self.playlist_tints = Default::default();
        self.tint_requested.clear();
        self.lyrics.clear();
        self.lyrics_by = None;
        self.lyrics_song = None;
        self.lyrics_loading = false;
        self.song_filters = Default::default();
        self.artist_filters = Default::default();
        self.queue_key = (usize::MAX, usize::MAX, usize::MAX, false, false, 0);
        self.downloads_view = Default::default();
        self.prefs.save();
        cx.notify();
    }

    pub fn current_song(&self) -> Option<&Song> {
        self.player.current.as_ref().or_else(|| if self.core.is_some() { None } else { self.player.song_ix.and_then(|i| self.songs.get(i)) })
    }

    pub fn rebuild_order(&mut self) {
        if self.core.is_some() {
            self.core_rebuild_order();
            return;
        }
        let query = self.query.to_lowercase();
        let keys = self.search_keys.clone();
        let mut order: Vec<u32> = if query.is_empty() {
            (0..self.songs.len() as u32).collect()
        } else {
            (0..self.songs.len() as u32).filter(|i| keys[*i as usize].contains(&query)).collect()
        };
        let songs = self.songs.clone();
        // Sample data has no filter data, so only a picked genre narrows it.
        if let Some(navia_core::song_filters::FilterValue::List(genres)) = self.song_filters.state.get(navia_core::song_filters::FilterId::Genre) {
            order.retain(|i| genres.iter().any(|g| *g == songs[*i as usize].genre.as_ref()));
        }
        let started = Instant::now();
        match self.sort {
            SortKey::MostPlayed => {}
            SortKey::Title => order.sort_by_cached_key(|i| songs[*i as usize].title.to_lowercase()),
            SortKey::Artist => order.sort_by_cached_key(|i| songs[*i as usize].artist.to_lowercase()),
            SortKey::Album => order.sort_by_cached_key(|i| songs[*i as usize].album.to_lowercase()),
            SortKey::Year => order.sort_by_key(|i| std::cmp::Reverse(songs[*i as usize].year)),
            // Shortest first, like the other columns' A to Z. A second header click reverses it.
            SortKey::Length => order.sort_by_key(|i| songs[*i as usize].duration_s),
            // Sample data has no per-song date, so a song counts as added with its album.
            SortKey::Added => {
                let added: std::collections::HashMap<&str, _> = self.albums.iter().map(|a| (a.id.as_ref(), a.added)).collect();
                order.sort_by_key(|i| std::cmp::Reverse(added.get(songs[*i as usize].album_id.as_ref()).copied()));
            }
        }
        if self.sort_desc && matches!(self.sort, SortKey::Title | SortKey::Artist | SortKey::Album | SortKey::Length | SortKey::MostPlayed) {
            order.reverse();
        }
        if self.sort == SortKey::Added && self.songs_added_head && !self.sort_desc {
            order.reverse();
        }
        if !query.is_empty() || self.sort != SortKey::MostPlayed {
            eprintln!("navia: sort/filter {} rows in {:.1} ms", order.len(), started.elapsed().as_secs_f64() * 1000.0);
        }
        self.order = order;
    }

    pub fn is_starred(&self, ix: u32) -> bool {
        self.starred.contains(&ix)
    }

    pub fn is_downloaded(&self, ix: u32) -> bool {
        self.downloaded.contains(&ix)
    }

    /// Whether the server can't be reached, from whichever side noticed first.
    pub fn is_offline(&self) -> bool {
        self.player.waiting_for_server || self.core.as_ref().is_some_and(|c| c.connected == Some(false) || self.offline_switch)
    }

    /// How opaque a song row's content is. Dimmed offline when it needs the server to play.
    pub fn offline_row_opacity(&self, ix: u32) -> f32 {
        if self.is_offline() && !self.plays_offline(ix) { crate::theme::size::ROW_UNAVAILABLE_OPACITY } else { 1.0 }
    }

    /// Keeps the selection within one list.
    pub fn select_in(&mut self, list: ListId) {
        if self.selection_list != list {
            self.selection.clear();
            self.selection_list = list;
        }
    }

    pub fn on_song_changed(&mut self, cx: &mut Context<Self>) {
        self.request_tint(cx);
        // List pages mark the playing song in rows held as page blocks.
        if matches!(self.route, Route::CachedSongs | Route::Downloads) {
            self.invalidate_page();
        }
        cx.notify();
    }

    pub fn play_row(&mut self, order_ix: usize, cx: &mut Context<Self>) {
        if self.core.is_some() {
            self.lyrics_user_scrolled = false;
            self.core_play_row(order_ix, cx);
            return;
        }
        let Some(&song_ix) = self.order.get(order_ix) else { return };
        self.player.queue = self.order.clone();
        self.player.queue_pos = order_ix;
        self.player.song_ix = Some(song_ix as usize);
        self.lyrics_user_scrolled = false;
        self.player.position = 0.0;
        self.player.playing = true;
        self.request_tint(cx);
        cx.notify();
    }

    /// A left click on a song row. Selects, and a plain double click plays.
    pub fn click_row(&mut self, order_ix: usize, ctrl: bool, shift: bool, click_count: usize, cx: &mut Context<Self>) {
        self.select_in(ListId::Songs);
        self.selection.click(order_ix, &self.order, ctrl, shift);
        if click_count >= 2 && !ctrl && !shift {
            self.play_row(order_ix, cx);
        }
        cx.notify();
    }

    /// Enter plays the selected row, or the first selected one in the list.
    pub fn play_selected(&mut self, cx: &mut Context<Self>) {
        if self.search_active || self.route != Route::Songs {
            return;
        }
        if let Some(order_ix) = self.selection.first_in(&self.order) {
            self.play_row(order_ix, cx);
        }
    }

    /// A right click on a song row. The menu acts on the selection, so a row outside it becomes the
    /// selection first.
    pub fn open_row_menu(&mut self, order_ix: usize, position: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        self.select_in(ListId::Songs);
        self.selection.prepare_context(order_ix, &self.order);
        let selected: Vec<u32> = self.order.iter().copied().filter(|s| self.selection.contains(*s)).collect();
        let play = match selected[..] {
            [_] => Self::play_row_item(move |this, cx| this.play_row(order_ix, cx)),
            _ => Self::play_songs_row(&selected),
        };
        self.open_song_menu(selected, Some(play), position, false, cx);
    }

    pub fn next(&mut self, cx: &mut Context<Self>) {
        if self.core.is_some() {
            self.core_next(cx);
            return;
        }
        if self.player.queue.is_empty() {
            self.player.queue = self.order.clone();
            if let Some(song) = self.player.song_ix {
                self.player.queue_pos = self.player.queue.iter().position(|i| *i as usize == song).unwrap_or(0);
            }
        }
        if self.player.queue.is_empty() {
            return;
        }
        self.player.queue_pos = (self.player.queue_pos + 1) % self.player.queue.len();
        self.player.song_ix = Some(self.player.queue[self.player.queue_pos] as usize);
        self.player.position = 0.0;
        self.request_tint(cx);
        cx.notify();
    }

    pub fn previous(&mut self, cx: &mut Context<Self>) {
        if self.core.is_some() {
            self.core_previous(cx);
            return;
        }
        if self.player.queue.is_empty() {
            return;
        }
        let len = self.player.queue.len();
        self.player.queue_pos = (self.player.queue_pos + len - 1) % len;
        self.player.song_ix = Some(self.player.queue[self.player.queue_pos] as usize);
        self.player.position = 0.0;
        self.request_tint(cx);
        cx.notify();
    }

    /// Fetches the album color and lyrics for a new song in the background.
    pub(crate) fn request_tint(&mut self, cx: &mut Context<Self>) {
        let Some(song) = self.current_song().cloned() else { return };
        if !self.tints.contains_key(&song.album_id) {
            self.art.request(Job::Tint { album_id: song.album_id.clone() });
        }
        if self.lyrics_song.as_ref() != Some(&song.id) {
            self.lyrics_song = Some(song.id.clone());
            self.lyrics.clear();
            self.lyrics_by = None;
            self.lyrics_loading = self.library.is_some();
            if let Some(library) = self.library.clone() {
                let id = song.id.clone();
                cx.spawn(async move |this, cx| {
                    let song_id = id.clone();
                    let (lines, writers) = cx.background_spawn(async move { (library.lyrics(&id), library.lyrics_writers(&id)) }).await;
                    this.update(cx, |shell, cx| {
                        shell.on_lyrics_loaded(&song_id, !lines.is_empty());
                        shell.lyrics_by = (!writers.is_empty()).then(|| writers.join(", ").into());
                        shell.lyrics_written = lines.clone();
                        shell.lyrics = crate::lyrics::prepare(lines, shell.prefs.backing_vocals_line);
                        shell.lyrics_loading = false;
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
        } else if !self.lyrics_loading && !self.lyrics.is_empty() {
            // The same song again, like the one restored at launch. Its row was reloaded without the lyrics
            // flag, but the lyrics are already here.
            self.on_lyrics_loaded(&song.id, true);
        }
    }

    /// Records whether a song has lyrics, which real songs only know after fetching.
    fn on_lyrics_loaded(&mut self, song_id: &SharedString, found: bool) {
        if let Some(song) = self.player.current.as_mut().filter(|s| s.id == *song_id) {
            song.has_lyrics = found;
        }
    }

    pub fn tint(&self) -> Srgb {
        self.current_song().and_then(|s| self.tints.get(&s.album_id).copied()).unwrap_or(Srgb::hex(crate::theme::TINT_FALLBACK))
    }

    pub fn thumb(&mut self, album_id: &SharedString, size: u32) -> Option<Arc<RenderImage>> {
        if album_id.is_empty() {
            return None; // A song row that hasn't loaded yet.
        }
        let key = (album_id.clone(), size);
        let frame = self.frame;
        let mut retrying = false;
        if let Some(entry) = self.thumbs.get_mut(&key) {
            entry.last_used = frame;
            if let Some(due) = entry.retry_at.filter(|_| !entry.loading) {
                if Instant::now() < due {
                    self.next_thumb_retry = Some(self.next_thumb_retry.map_or(due, |at: Instant| at.min(due)));
                    return None;
                }
                entry.retry_at = None;
                entry.loading = true;
                retrying = true;
            } else if entry.expires_at.is_some_and(|at| Instant::now() >= at) && !entry.loading {
                // a placeholder that expired: keep showing it while the server is asked again.
                entry.expires_at = None;
                entry.loading = true;
                let shown = entry.image.clone();
                if let Some(Job::Thumb { album_id, size }) = self.art.request(Job::Thumb { album_id: album_id.clone(), size }) {
                    self.thumbs.remove(&(album_id, size));
                }
                return shown;
            } else {
                return entry.image.clone();
            }
        }
        if !retrying {
            self.thumbs.insert_loading(key, frame);
        }
        if let Some(Job::Thumb { album_id, size }) = self.art.request(Job::Thumb { album_id: album_id.clone(), size }) {
            // Dropped from the queue after scrolling past, so forget it and let a later frame ask again.
            self.thumbs.remove(&(album_id, size));
        }
        None
    }

    /// A cached image and how far it has faded in, so art never pops onto the page.
    pub fn thumb_faded(&mut self, album_id: &SharedString, size: u32) -> Option<(Arc<RenderImage>, f32)> {
        let image = self.thumb(album_id, size)?;
        let key = (album_id.clone(), size);
        let mut opacity = 1.0;
        if let Some(entry) = self.thumbs.get_mut(&key)
            && let Some(arrived) = entry.arrived
        {
            opacity = crate::theme::motion::progress(arrived.elapsed().as_secs_f32(), crate::theme::motion::ART_FADE);
            if opacity >= 1.0 {
                opacity = 1.0;
                entry.arrived = None;
            } else {
                self.thumbs_fading = true;
            }
        }
        Some((image, opacity.max(0.0)))
    }

    fn apply_art_event(&mut self, event: ArtEvent, cx: &mut Context<Self>) {
        match event {
            ArtEvent::Thumb { album_id, size, image, placeholder } => {
                self.thumbs.set_image(&(album_id, size), image, placeholder);
            }
            ArtEvent::Dock { album_id, bake } => {
                self.dock_bake = Some((album_id, bake));
            }
            ArtEvent::Tint { album_id, tint } => {
                self.tints.insert(album_id, tint);
            }
            ArtEvent::PlaylistTint { key, generation, tint } => {
                self.apply_playlist_tint(key, generation, tint);
            }
            ArtEvent::Expanded { album_id, width, height, image, peaks } => {
                self.expanded_bg = Some((album_id, width, height, image));
                self.expanded_peaks = Some(peaks);
            }
            ArtEvent::BarGlow { album_id, width, height, image } => {
                self.bar_glow = Some((album_id, width, height, image));
            }
            ArtEvent::Bake { kind, id, width, height, image } => self.apply_bake(kind, id, width, height, image),
            ArtEvent::LyricLayout { key, layout } => self.apply_lyric_layout(key, layout),
            ArtEvent::LyricLine { key, ix, sprite } => self.apply_lyric_line(key, ix, sprite),
        }
        self.notify_visual(cx);
    }

    /// Requests a new dock bake when the song or size changes, debounced so a live resize doesn't
    /// queue one per frame.
    fn ensure_dock_bake(&mut self, geometry: DockGeometry, cx: &mut Context<Self>) {
        let Some(song) = self.current_song().cloned() else { return };
        let album = song.album_id.clone();
        // During a drag the last bake is scaled, then baked again once when the drag ends.
        if self.dock_resizing() && self.dock_bake.as_ref().is_some_and(|(id, _)| *id == album) {
            return;
        }
        if self.dock_bake.as_ref().is_some_and(|(id, bake)| *id == album && bake.geometry == geometry) {
            self.dock_requested = None;
            return;
        }
        if self.dock_requested.as_ref().is_some_and(|(id, g)| *id == album && *g == geometry) {
            return;
        }
        self.dock_requested = Some((album.clone(), geometry));
        self.dock_generation += 1;
        self.dock_bake_requests += 1;
        let generation = self.dock_generation;
        // Song changes bake at once, and size changes wait until resizing settles.
        let debounce = self.dock_bake.as_ref().is_some_and(|(id, _)| *id == album);
        let art = self.art.clone();
        cx.spawn(async move |this, cx| {
            if debounce {
                cx.background_executor().timer(Duration::from_millis(120)).await;
            }
            let _ = this.update(cx, |shell, _| {
                if shell.dock_generation == generation {
                    art.request(Job::Dock { album_id: album, geometry });
                }
            });
        })
        .detach();
    }

    fn ensure_backdrops(&mut self, layout: &Layout, cx: &mut Context<Self>) {
        let Some(song) = self.current_song().cloned() else { return };
        let (w, h) = ((f32::from(layout.viewport.width) * layout.scale) as u32, (f32::from(layout.viewport.height) * layout.scale) as u32);
        if layout.mode == PlayerMode::Dock && !self.expanded {
            self.ensure_dock_bake(self.dock_geometry(layout), cx);
        }
        if self.expanded {
            let want = (song.album_id.clone(), w.max(1), h.max(1));
            let have = self.expanded_bg.as_ref().map(|(a, w, h, _)| (a.clone(), *w, *h));
            if have.as_ref() != Some(&want) && self.expanded_requested.as_ref() != Some(&want) {
                self.expanded_requested = Some(want.clone());
                let blur_px = f32::from(self.theme.s(crate::theme::size::PLAYER_GLOW_BLUR)) * layout.scale;
                self.art.request(Job::Expanded { album_id: want.0, width: want.1, height: want.2, blur_px });
            }
        }
        if layout.mode == PlayerMode::Bar && !self.expanded {
            let t = self.theme;
            let gw = (f32::from(layout.main_width + layout.dock_room) * layout.scale) as u32;
            let gh = (f32::from(t.s(150.0)) * layout.scale) as u32;
            let want = (song.album_id.clone(), gw.max(1), gh.max(1));
            let have = self.bar_glow.as_ref().map(|(a, w, h, _)| (a.clone(), *w, *h));
            if have.as_ref() != Some(&want) && self.bar_glow_requested.as_ref() != Some(&want) {
                self.bar_glow_requested = Some(want.clone());
                self.art.request(Job::BarGlow { album_id: want.0, width: want.1, height: want.2 });
            }
        }
        let _ = cx;
    }

    pub fn dock_geometry(&self, layout: &Layout) -> DockGeometry {
        let t = self.theme;
        let scale = layout.scale;
        let dp = |p: Pixels| (f32::from(p) * scale).round() as u32;
        let width = dp(layout.dock_width);
        let height = dp(layout.viewport.height - t.s(WINDOW_INSET) * 2.0);
        // The dock's art is min(502, 56% of the window's height), in token units.
        let art_h = dp(t.s(502.0_f32.min(f32::from(layout.viewport.height) * 0.56)));
        let reflect_h = dp(t.s(240.0));
        let reflect_top = art_h.saturating_sub(dp(t.s(95.0)));
        DockGeometry { width, height, art_h, reflect_top, reflect_h, blur_px: dp(t.s(REFLECT_BLUR)) }
    }

    pub fn layout(&self, viewport: gpui::Size<Pixels>, scale: f32) -> Layout {
        let t = self.theme;
        let tokens_w = f32::from(viewport.width) / (BASE_SCALE * t.zoom);
        let can_dock = tokens_w >= DOCK_MIN_WINDOW;
        let sidebar_fits = tokens_w >= SIDEBAR_MIN_WINDOW;
        let mode = if self.prefs.mode == PlayerMode::Dock && !can_dock { PlayerMode::Bar } else { self.prefs.mode };
        let sidebar_overlay = !sidebar_fits;
        let player = self.player.song_ix.is_some();
        let docked = mode == PlayerMode::Dock && player;
        let sidebar_tokens = self.sidebar_tokens(tokens_w, docked);
        let sidebar_w = t.s(sidebar_tokens);
        let in_flow = sidebar_fits && self.prefs.sidebar_open;
        let dock_w = t.s(self.prefs.dock_width.clamp(DOCK_MIN_W, self.dock_max_tokens(tokens_w, if in_flow { Some(sidebar_tokens) } else { None })));
        let inset = t.s(WINDOW_INSET);
        let slide = |key: &'static str, on: bool| crate::ui::eased(key, on, motion::PANEL_SLIDE, motion::EASE);
        let (sidebar_shown, sidebar_moving) = slide("panel-sidebar", in_flow);
        // by mode alone, so a player appearing (a session restored at launch) is just there.
        let (dock_shown, dock_moving) = slide("panel-dock", mode == PlayerMode::Dock);
        let (bar_shown, bar_moving) = slide("panel-bar", mode == PlayerMode::Bar);
        let sidebar_visible = if sidebar_fits { sidebar_shown > 0.0 } else { self.sidebar_drawer };
        let sidebar_room = if sidebar_fits { (sidebar_w + inset) * sidebar_shown } else { px(0.0) };
        let dock_room = if player { (dock_w + inset) * dock_shown } else { px(0.0) };
        let main_w = viewport.width - sidebar_room - dock_room;
        let settled_w = viewport.width - if in_flow { sidebar_w + inset } else { px(0.0) } - if docked { dock_w + inset } else { px(0.0) };
        Layout {
            viewport,
            scale,
            tokens_w,
            mode,
            player,
            embedded: false,
            sidebar_visible,
            sidebar_overlay,
            dock_width: dock_w,
            sidebar_width: sidebar_w,
            main_width: main_w,
            settled_width: settled_w,
            sidebar_shown: if sidebar_fits { sidebar_shown } else { 0.0 },
            sidebar_in: in_flow,
            dock_shown: if player { dock_shown } else { 0.0 },
            bar_shown: if player { bar_shown } else { 0.0 },
            sidebar_room,
            dock_room,
            panels_moving: sidebar_moving || (player && (dock_moving || bar_moving)),
        }
    }

    /// The sidebar's width in tokens. The user's, within its min and max, narrowed when the window
    /// wouldn't otherwise leave the page `DOCK_MIN_CONTENT` beside a narrowest dock.
    fn sidebar_tokens(&self, tokens_w: f32, docked: bool) -> f32 {
        self.prefs.sidebar_width.clamp(SIDEBAR_W_MIN, Self::sidebar_room(tokens_w, docked))
    }

    /// The widest the sidebar can be while the page keeps `DOCK_MIN_CONTENT`, beside a narrowest dock
    /// when docked.
    fn sidebar_room(tokens_w: f32, docked: bool) -> f32 {
        let dock = if docked { DOCK_MIN_W } else { 0.0 };
        (tokens_w - dock - 2.0 * WINDOW_INSET - DOCK_MIN_CONTENT).clamp(SIDEBAR_W_MIN, SIDEBAR_W_MAX)
    }

    pub fn sidebar_resizing(&self) -> bool {
        matches!(self.dragging, Some(Drag::SidebarWidth { .. }))
    }

    /// Sets the sidebar's width from a drag or script, limited to what the window allows now.
    pub fn set_sidebar_width(&mut self, tokens: f32, viewport: gpui::Size<Pixels>, scale: f32, cx: &mut Context<Self>) {
        let layout = self.layout(viewport, scale);
        let docked = layout.mode == PlayerMode::Dock && layout.player;
        self.prefs.sidebar_width = tokens.clamp(SIDEBAR_W_MIN, Self::sidebar_room(layout.tokens_w, docked));
        cx.notify();
    }

    pub fn reset_sidebar_width(&mut self, cx: &mut Context<Self>) {
        self.prefs.sidebar_width = SIDEBAR_W;
        self.prefs.save();
        cx.notify();
    }

    pub fn begin_sidebar_drag(&mut self, mouse_x: Pixels, viewport: gpui::Size<Pixels>, scale: f32) {
        let layout = self.layout(viewport, scale);
        let right = self.theme.s(WINDOW_INSET) + layout.sidebar_width;
        self.dragging = Some(Drag::SidebarWidth { grab_offset: mouse_x - right });
    }

    /// The widest the dock can be now, keeping at least `DOCK_MIN_CONTENT` for the page. `sidebar` is
    /// the sidebar's width in tokens when it takes room in the row.
    fn dock_max_tokens(&self, tokens_w: f32, sidebar: Option<f32>) -> f32 {
        let sidebar = sidebar.map_or(0.0, |w| w + WINDOW_INSET);
        (tokens_w - sidebar - WINDOW_INSET - DOCK_MIN_CONTENT).clamp(DOCK_MIN_W, DOCK_MAX_W)
    }

    pub fn dock_resizing(&self) -> bool {
        matches!(self.dragging, Some(Drag::DockWidth { .. }))
    }

    /// Sets the dock width from a drag or script. Only what the window allows now is kept, so the
    /// dock doesn't jump when the pointer passes the limit and comes back.
    pub fn set_dock_width(&mut self, tokens: f32, viewport: gpui::Size<Pixels>, scale: f32, cx: &mut Context<Self>) {
        let layout = self.layout(viewport, scale);
        let sidebar = layout.sidebar_in.then(|| f32::from(layout.sidebar_width) / self.theme.token_scale());
        self.prefs.dock_width = tokens.clamp(DOCK_MIN_W, self.dock_max_tokens(layout.tokens_w, sidebar));
        cx.notify();
    }

    pub fn reset_dock_width(&mut self, cx: &mut Context<Self>) {
        self.prefs.dock_width = DOCK_W;
        self.prefs.save();
        cx.notify();
    }

    pub fn begin_dock_drag(&mut self, mouse_x: Pixels, viewport: gpui::Size<Pixels>, scale: f32) {
        let layout = self.layout(viewport, scale);
        let left = viewport.width - self.theme.s(WINDOW_INSET) - layout.dock_width;
        self.dragging = Some(Drag::DockWidth { grab_offset: mouse_x - left });
    }

    /// Ends any running drag, saving the dock width if it was a dock drag.
    pub(crate) fn end_drag(&mut self, cx: &mut Context<Self>) {
        if matches!(self.dragging, Some(Drag::Seek)) && self.core.is_some() {
            let ms = (self.player.position.max(0.0) * 1000.0) as u64;
            self.core_seek_ms(ms, cx);
        }
        if self.dock_resizing() || self.sidebar_resizing() {
            self.prefs.save();
        }
        if matches!(self.dragging, Some(Drag::Pointer)) {
            self.pointer_drag_end(cx);
        }
        if matches!(self.dragging, Some(Drag::SidebarPlaylist)) {
            self.sidebar_drag_end(cx);
        }
        if matches!(self.dragging, Some(Drag::Rows)) {
            self.rows_drag_end(cx);
        }
        self.dragging = None;
        cx.notify();
    }

    pub fn can_dock(&self, layout: &Layout) -> bool {
        layout.tokens_w >= DOCK_MIN_WINDOW
    }

    pub fn set_mode(&mut self, mode: PlayerMode, cx: &mut Context<Self>) {
        self.prefs.mode = mode;
        self.prefs.save();
        cx.notify();
    }

    /// Opens the bar's Lyrics or Queue in the side player, making the dock the saved form. Goes full
    /// screen while the window is too narrow for the dock.
    pub fn open_side_view(&mut self, view: DockView, can_dock: bool, cx: &mut Context<Self>) {
        self.prefs.view = view;
        if can_dock {
            self.prefs.mode = PlayerMode::Dock;
        } else {
            self.expanded = true;
        }
        self.prefs.save();
        cx.notify();
    }

    pub fn set_view(&mut self, view: DockView, cx: &mut Context<Self>) {
        self.prefs.view = view;
        self.prefs.save();
        cx.notify();
    }

    /// A song without lyrics shows artwork without changing the saved view.
    pub fn shown_view(&self) -> DockView {
        if self.prefs.view == DockView::Lyrics && !self.may_have_lyrics() { DockView::Artwork } else { self.prefs.view }
    }

    /// the current song has lyrics, or they're still loading: Lyrics stays put until it's known there are none.
    pub fn may_have_lyrics(&self) -> bool {
        self.current_song().is_some_and(|s| s.has_lyrics || (self.lyrics_loading && self.lyrics_song.as_ref() == Some(&s.id)))
    }

    pub fn set_zoom(&mut self, zoom: u32, cx: &mut Context<Self>) {
        self.prefs.zoom = zoom.clamp(70, 130);
        self.prefs.save();
        self.refresh_theme();
        cx.notify();
    }

    /// Switches translucency live, telling the compositor and the page together.
    pub fn set_translucent(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.prefs.translucent = Some(on);
        self.prefs.save();
        self.refresh_theme();
        window.set_background_appearance(chrome::window_background(on));
        cx.notify();
    }

    /// Switches live between the desktop's title bar and Navia's own frame. The compositor replies
    /// with a configure, and each frame reads what it granted.
    pub fn set_system_title_bar(&mut self, on: bool, cx: &mut Context<Self>) {
        if !self.system_frame_available {
            return;
        }
        self.prefs.system_title_bar = on;
        self.prefs.save();
        let support = chrome::Support { blur: self.blur_detected, system_frame: true, layer_shell: self.layer_shell_available };
        self.frame_request = Some(chrome::decorations(support, on));
        cx.notify();
    }

    pub fn refresh_theme(&mut self) {
        self.theme = Theme::new(self.accent_now(), self.prefs.zoom as f32 / 100.0, self.translucent());
    }

    /// the chosen accent, or the preset closest to the desktop's while System is chosen.
    pub fn accent_now(&self) -> u32 {
        if self.prefs.accent_system { crate::color::system_accent(self.system_accent) } else { self.prefs.accent }
    }

    fn watch_system_accent(cx: &mut Context<Self>) {
        use futures::StreamExt;
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        crate::system_accent::watch(tx);
        cx.spawn(async move |this, cx| {
            while let Some(accent) = rx.next().await {
                let changed = this.update(cx, |shell, cx| {
                    if shell.system_accent != accent {
                        shell.system_accent = accent;
                        shell.refresh_theme();
                        cx.notify();
                    }
                });
                if changed.is_err() {
                    return;
                }
            }
        })
        .detach();
    }

    /// Whether the window is translucent now, from the choice or else the desktop's blur.
    pub fn translucent(&self) -> bool {
        self.prefs.translucent_on(self.blur_detected)
    }

    pub fn toggle_sidebar(&mut self, tokens_w: f32, cx: &mut Context<Self>) {
        if tokens_w >= SIDEBAR_MIN_WINDOW {
            self.prefs.sidebar_open = !self.prefs.sidebar_open;
            self.prefs.save();
        } else {
            self.sidebar_drawer = !self.sidebar_drawer;
        }
        cx.notify();
    }

    fn run_script_step(&mut self, t: u64, window: &mut Window, cx: &mut Context<Self>) {
        let due = std::mem::take(&mut self.script);
        let (run, keep): (Vec<_>, Vec<_>) = due.into_iter().partition(|(at, _)| *at == t);
        self.script = keep;
        for (_, action) in run {
            match action {
                ScriptAction::Mode(m) => self.set_mode(m, cx),
                ScriptAction::Expand(e) => self.expanded = e,
                ScriptAction::Scroll(v) => self.pending_scroll = Some(v),
                ScriptAction::Compact(c) => self.compact = c,
                ScriptAction::Translucent(on) => self.set_translucent(on, window, cx),
                ScriptAction::OwnFrame(on) => self.set_system_title_bar(!on, cx),
                ScriptAction::RowClick { row, ctrl, shift, count } => self.click_row(row, ctrl, shift, count, cx),
                ScriptAction::RowMenu(row) => self.open_row_menu(row, point(px(0.0), px(0.0)), cx),
                ScriptAction::Enter => self.play_selected(cx),
                ScriptAction::Page(op) => self.page_script(&op, window.viewport_size(), cx),
                ScriptAction::DockWidth(w) => {
                    self.set_dock_width(w, self.edge.content_size(window.viewport_size()), window.scale_factor(), cx);
                    if !self.dock_resizing() {
                        self.prefs.save();
                    }
                }
                ScriptAction::DockReset => self.reset_dock_width(cx),
                ScriptAction::Hidden(hidden) => self.window_visibility_changed(!hidden, window, cx),
                ScriptAction::Reduce(on) => crate::theme::motion::set_reduced(cx, on),
                ScriptAction::DockDrag(true) => self.dragging = Some(Drag::DockWidth { grab_offset: px(0.0) }),
                ScriptAction::DockDrag(false) => self.end_drag(cx),
                ScriptAction::Report => {
                    let layout = self.layout(self.edge.content_size(window.viewport_size()), window.scale_factor());
                    let (page, _) = crate::ui::swap("page-entry", &self.page_entry.0, motion::PAGE_FADE);
                    eprintln!(
                        "navia: report land={:?} blocks={} page={:.3}:{} {:.0} ms window={:.0} tokens_w={:.0} mode={:?} saved_dock={:.1} dock={:.1} main={:.1} bakes={} shown sidebar={:.3} dock={:.3} bar={:.3}",
                        self.page_land.map(|t| t.elapsed().as_millis()),
                        self.page.blocks.len(),
                        page.shown,
                        self.page_entry.1,
                        crate::testing::since_start_ms(),
                        f32::from(layout.viewport.width),
                        layout.tokens_w,
                        layout.mode,
                        self.prefs.dock_width,
                        f32::from(layout.dock_width) / self.theme.token_scale(),
                        f32::from(layout.main_width) / self.theme.token_scale(),
                        self.dock_bake_requests,
                        layout.sidebar_shown,
                        layout.dock_shown,
                        layout.bar_shown
                    );
                }
                ScriptAction::Maximize => window.zoom_window(),
                ScriptAction::Fullscreen => window.toggle_fullscreen(),
                ScriptAction::Buttons(layout) => {
                    if let Some(layout) = chrome::parse_layout(&layout) {
                        self.button_layout = layout;
                    }
                }
                ScriptAction::Core(op) => self.core_script(&op, cx),
                ScriptAction::Play(i) => self.play_row(i, cx),
                ScriptAction::Query(q) => {
                    self.query = q;
                    self.rebuild_order();
                }
                ScriptAction::Toggle("sidebar") => {
                    self.prefs.sidebar_open = !self.prefs.sidebar_open;
                    self.prefs.save();
                }
                ScriptAction::Toggle("sortmenu") => self.open_sort_menu(point(px(800.0), px(80.0)), cx),
                ScriptAction::Toggle("view-lyrics") => self.set_view(DockView::Lyrics, cx),
                ScriptAction::Toggle("view-queue") => self.set_view(DockView::Queue, cx),
                ScriptAction::Toggle("view-artwork") => self.set_view(DockView::Artwork, cx),
                ScriptAction::Toggle("quality") => self.pin_quality_popover(cx),
                ScriptAction::Toggle("account") => self.open_account_menu(crate::view_account::AccountPlace::TopBar, cx),
                ScriptAction::Toggle("refresh") => {
                    let started = self.core.as_ref().is_some_and(|c| c.lib.refresh_now());
                    eprintln!("navia: refresh asked, started={started}");
                }
                ScriptAction::Toggle(_) => {}
                ScriptAction::Back => self.go_back(cx),
                ScriptAction::Like => {
                    if let Some(song) = self.player.song_ix {
                        if !self.is_starred(song as u32) {
                            crate::ui::pop("meta-star");
                        }
                        self.toggle_song_star(song as u32, cx);
                    }
                }
                ScriptAction::AddQueue(n) => {
                    let songs: Vec<u32> = self.order.iter().take(n).copied().collect();
                    self.add_to_queue(&songs, cx);
                }
                ScriptAction::Route(name) => {
                    let r = self.script_route(&name);
                    self.navigate(r, cx);
                }
                ScriptAction::Close => {
                    if self.main_window_closing(window, cx) {
                        window.remove_window();
                    }
                    return;
                }
                ScriptAction::Quit => {
                    self.core_save_session();
                    eprintln!("navia: script quit after {:.1} s", self.script_started.elapsed().as_secs_f32());
                    cx.quit();
                }
            }
        }
        cx.notify();
    }

    fn advance_bench(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sample = self.clock.begin();
        let settling = self.order.is_empty() || self.thumbs_settling();
        // With NAVIA_BENCH_ROUTE the same phases scroll that page instead of the song table.
        if let Some(route) = self.bench_route.take() {
            self.route = route.clone();
            self.history = vec![route];
            self.history_pos = 0;
            self.invalidate_page();
        }
        let page_mode = self.page_uses_list();
        let Some(bench) = self.bench.as_mut() else { return };
        if settling {
            window.request_animation_frame();
            return;
        }
        if let Some(sample) = sample {
            if bench.phase_frame > 0 {
                bench.report[bench.phase].1.push(sample);
            }
        }
        window.request_animation_frame();
        bench.phase_frame += 1;
        if bench.phase_frame > BENCH[bench.phase].frames {
            bench.phase += 1;
            bench.phase_frame = 0;
            if bench.phase == BENCH.len() {
                testing::print_table(&bench.report);
                testing::print_memory();
                println!("thumbnails decoded and cached: {}", self.thumbs.iter().filter(|(_, e)| e.image.is_some()).count());
                cx.quit();
                return;
            }
            bench.report.push((BENCH[bench.phase].name.to_string(), Vec::new()));
        }
        if page_mode {
            let phase = BENCH[bench.phase];
            let count = self.page.blocks.len().max(1);
            if phase.speed > 0.0 {
                let top = self.page.state.logical_scroll_top();
                if top.item_ix + 1 >= count {
                    self.page.state.scroll_to(gpui::ListOffset { item_ix: 0, offset_in_item: px(0.0) });
                } else {
                    self.page.state.scroll_by(px(phase.speed));
                }
            } else if phase.speed < 0.0 {
                bench.rng = bench.rng.wrapping_mul(1664525).wrapping_add(1013904223);
                self.page.state.scroll_to(gpui::ListOffset { item_ix: (bench.rng >> 8) as usize % count, offset_in_item: px(0.0) });
            }
            return;
        }
        let t = self.theme;
        let max = (self.order.len() as f32 + 1.0) * f32::from(t.s(ROW_H)).round() - 400.0;
        let phase = BENCH[bench.phase];
        if phase.speed > 0.0 {
            bench.offset = (bench.offset + phase.speed) % max;
        } else if phase.speed < 0.0 {
            bench.rng = bench.rng.wrapping_mul(1664525).wrapping_add(1013904223);
            bench.offset = (bench.rng >> 8) as f32 % max;
        }
        let offset = bench.offset;
        self.list_scroll.0.borrow().base_handle.set_offset(point(px(0.0), px(-offset)));
    }

    /// Whether the first frames are still decoding thumbnails, which the bench shouldn't measure.
    fn thumbs_settling(&self) -> bool {
        self.frame < 180
    }

    fn evict_thumbs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.thumbs.evict(self.frame, window);
        self.evict_moving_covers(window, cx);
    }
}

use crate::core_library::open_library;

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::testing::frame_begin();
        self.sync_meter_last.set(self.sync_meter_now.replace(false));
        self.advance_bench(window, cx);
        self.app_menu_sync(cx);
        // A lifted row: scroll near the list's edges and find the gap from the last layout.
        self.row_drag_frame();
        if let Some(y) = self.pending_scroll.take() {
            // Block pages scroll their list, and the song table scrolls itself.
            if self.page_uses_list() {
                self.page.state.scroll_by(px(y));
            } else {
                self.list_scroll.0.borrow().base_handle.set_offset(point(px(0.0), px(-y)));
            }
        }

        if let Some(request) = self.frame_request.take() {
            window.request_decorations(request);
        }
        let decorations = window.window_decorations();
        if self.frame_granted != Some((decorations, window.viewport_size())) {
            self.frame_granted = Some((decorations, window.viewport_size()));
            if std::env::var_os("NAVIA_FRAME_LOG").is_some() {
                eprintln!("navia: frame {:?} surface={:?}", decorations, window.viewport_size());
            }
        }
        // Windows always uses Navia's own frame, with the system title bar hidden by
        // `appears_transparent`, and reports server decorations since Windows draws the edge.
        self.csd = cfg!(target_os = "windows") || matches!(decorations, Decorations::Client { .. });
        self.maximized = window.is_maximized();
        self.window_controls = window.window_controls();
        // Square with no margin on any side that's tiled or against the screen edge.
        let tiling = match decorations {
            Decorations::Client { tiling } => Some(tiling),
            Decorations::Server => None,
        };
        let t = self.theme;
        self.edge = chrome::Frame::new(tiling, self.maximized, window.is_fullscreen(), t.s(WINDOW_RADIUS));
        window.set_client_inset(self.edge.inset);
        // The blur region is cut to the window's shape, inside the shadow margin.
        window.set_client_corner_radii(self.edge.corners);
        // Only the window and its resize grips take the pointer. The rest of the shadow margin passes
        // clicks through.
        let region = self.edge.input_region(window.viewport_size());
        if self.input_region != Some(region) {
            self.input_region = Some(region);
            window.set_input_region(region.as_ref().map(std::slice::from_ref));
        }

        let surface = window.viewport_size();
        let viewport = self.edge.content_size(surface);
        let layout = self.layout(viewport, window.scale_factor());
        // Only frames where the shell itself changed count. Cached parts are still the ones drawn then,
        // so per-frame tracking stays correct.
        if self.slots.begin(&layout) {
            self.frame += 1;
        }
        self.ensure_backdrops(&layout, cx);
        self.evict_thumbs(window, cx);
        self.drop_lyric_garbage(window);
        self.scrollbar_animating = false;
        self.thumbs_fading = false;
        focus::begin_frame(self.frame, self.confirm.is_some() || self.new_playlist.is_some() || self.airplay_dialog_open() || self.info.is_some() || self.about.is_some() || self.shares.dialog.is_some(), cx);
        self.sync_focus(window, cx);
        let mut root = div()
            .id("navia-root")
            .relative()
            .size_full()
            .flex()
            .flex_row()
            .bg(t.bg())
            // Keeps tall lists out of the shadow margin, which the surface edge doesn't clip.
            .overflow_hidden()
            .text_color(t.t1())
            .font_family(FONT_FAMILY)
            .text_size(t.text(TEXT_MD))
            .line_height(relative(1.35))
            .track_focus(&self.focus)
            .child(crate::ui::hover_clears_on_leave())
            .on_action(cx.listener(|this, _: &ZoomIn, _, cx| this.set_zoom(this.prefs.zoom + 5, cx)))
            .on_action(cx.listener(|this, _: &ZoomOut, _, cx| this.set_zoom(this.prefs.zoom.saturating_sub(5), cx)))
            .on_action(cx.listener(|this, _: &ZoomReset, _, cx| this.set_zoom(100, cx)))
            .on_action(cx.listener(|this, _: &OpenSettings, _, cx| this.open_settings(cx)))
            .on_action(cx.listener(|this, _: &GetInfo, _, cx| this.info_shortcut(cx)))
            .on_action(cx.listener(|this, _: &ToggleMiniPlayer, _, cx| this.toggle_mini_player(cx)))
            .on_action(cx.listener(|this, _: &CheckForUpdates, _, cx| {
                this.open_settings(cx);
                this.check_for_updates(cx);
            }))
            .on_action(cx.listener(|_, _: &Quit, _, cx| cx.quit()))
            .on_action(cx.listener(|this, _: &Dismiss, _, cx| this.dismiss_top(cx)))
            .on_action(cx.listener(|this, _: &PlaySelected, window, cx| {
                // A control reached by keyboard handles Enter itself, as a click.
                if focus::keyboard() && this.menu.is_none() && this.controls.select.is_none() && this.focused_target().is_none() && focus::focused_kind(window, cx).is_some_and(|k| k != focus::StopKind::Field) {
                    cx.propagate();
                    return;
                }
                if this.menu.is_some() {
                    this.menu_activate(cx);
                } else if !this.controls_enter(cx) && this.connect.is_none() {
                    this.play_selected(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &FocusSearch, _, cx| {
                if this.page_has_field() {
                    this.focus_field(crate::ui_controls::TextTarget::Page, cx);
                } else {
                    this.navigate(Route::Browse, cx);
                }
            }))
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| this.on_key_capture(event, window, cx)))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| this.on_key(event, cx)))
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                this.on_drag_move(event.position, cx);
            }))
            .on_mouse_up(gpui::MouseButton::Left, cx.listener(|this, _, _, cx| {
                this.end_drag(cx);
            }));
        // The expanded player covers the window, so what's under it isn't built. It can't take the
        // keyboard or show focus rings, and each frame does less.
        let signing_in = self.connect.is_some();
        // the expanded player comes in from the player's side and goes back there; what's under it is built until covered.
        let (entered, enter_ticker) = crate::ui::ease_to("player-enter", self.expanded && layout.player, motion::PLAYER_ENTER, motion::HOVER_EASE);
        let covered = (self.expanded && layout.player && entered >= 1.0) || signing_in;
        if !covered {
            root = root.child(self.render_page_backdrop(&layout, cx));
            if self.csd {
                // Below everything else, so it only takes presses in the gaps along the top beside the sidebar
                // and dock. The top bar, sidebar header and dock head are their own drag areas.
                root = root.child(Self::drag_area(div().id("drag-band").absolute().top(px(0.0)).left(px(0.0)).right(px(0.0)).h(t.s(TOPBAR_H)), cx));
            }
            focus::set_section(focus::SECTION_SIDEBAR);
            // Painted after the page and dock, so what passes under it is already drawn when its backdrop
            // blur runs.
            let sidebar = (layout.sidebar_visible && !layout.sidebar_overlay).then(|| self.slots.sidebar_view());
            focus::set_section(focus::SECTION_PAGE);
            root = root.child(self.slots.main_view(self.main_column_margin(&layout), layout.dock_room));
            if layout.player {
                // the player's own slot over the page and the dock's room, then its scrolling names over it.
                let style = gpui::StyleRefinement::default().absolute().top_0().bottom_0().right_0().left(self.main_column_margin(&layout));
                root = root.child(self.slots.player_view(style)).children(self.marquee_overlays(crate::marquee::Host::Player));
            }
            if layout.dock_shown >= 1.0 {
                focus::set_section(focus::SECTION_DOCK);
                root = root.child(self.render_dock_grab(&layout, cx));
            }
            root = root.children(sidebar);
            if layout.sidebar_shown >= 1.0 && !layout.sidebar_overlay {
                root = root.child(self.render_sidebar_grab(&layout, cx));
            }
            if layout.panels_moving {
                root = root.child(crate::animation::ticker(["panel-sidebar", "panel-dock", "panel-bar"]));
            }
            // Rows in lists are built after this, so they count as the page.
            focus::set_section(focus::SECTION_PAGE);
            if layout.sidebar_visible && layout.sidebar_overlay {
                root = root.child(self.render_sidebar_overlay(&layout, cx));
            }
        }
        if signing_in {
            focus::set_section(focus::SECTION_PAGE);
            root = root.child(self.render_connect(&layout, cx));
        } else if layout.player && (self.expanded || entered > 0.0) {
            let away = 1.0 - entered;
            let snap = |v: Pixels| px((f32::from(v) * away * layout.scale).round() / layout.scale);
            let from = if layout.mode == PlayerMode::Dock { point(snap(layout.viewport.width), px(0.0)) } else { point(px(0.0), snap(layout.viewport.height)) };
            self.marquees_follow_offset(from, cx);
            root = root.child(div().absolute().inset_0().paint_offset(from).child(enter_ticker).child(self.slots.expanded_view()).children(self.marquee_overlays(crate::marquee::Host::Expanded)));
        }
        if let Some(confirm) = self.render_confirm(cx) {
            root = root.child(confirm);
        }
        if let Some(dialog) = self.render_new_playlist(cx) {
            root = root.child(dialog);
        }
        if let Some(dialog) = self.render_airplay_dialog(cx) {
            root = root.child(dialog);
        }
        if let Some(dialog) = self.render_edit_playlist(cx) {
            root = root.child(dialog);
        }
        if let Some(dialog) = self.render_share_dialog(&layout, cx) {
            root = root.child(dialog);
        }
        self.info_links = 0;
        if let Some(sheet) = self.render_info(&layout, window, cx) {
            root = root.child(sheet);
        }
        if let Some(sheet) = self.render_about(&layout, window, cx) {
            root = root.child(sheet);
        }
        if let Some(toast) = self.render_toast(&layout, cx) {
            root = root.child(toast);
        }
        if let Some(pop) = self.render_quality_popover(window, cx) {
            root = root.child(pop);
        }
        if let Some(pop) = self.render_filter_pop(window, cx) {
            root = root.child(pop);
        }
        if let Some(select) = self.render_select(window, cx) {
            root = root.child(select);
        }
        if let Some(menu) = self.render_menu(window, cx) {
            root = root.child(menu);
        }
        self.window_scale = window.scale_factor();
        if self.edge.drawn {
            root = root.child(frame_outline(self.edge));
        }
        let frame = self.edge;
        let mut window_root = div()
            .id("navia-window")
            .relative()
            .size_full()
            .pt(frame.top)
            .pr(frame.right)
            .pb(frame.bottom)
            .pl(frame.left);
        if frame.drawn {
            window_root = window_root.child(frame_shadow(frame, surface, window.is_window_active()));
        }
        window_root = window_root.child(chrome::round_corners(root, frame.corners));
        if let Decorations::Client { tiling } = decorations {
            window_root = window_root.child(resize_edges(frame, surface, tiling, window));
        }
        if self.dragging.is_some() {
            // Resizing the dock keeps its cursor. Carried songs show a closed hand, or not-allowed over a
            // target that refuses them. Nothing else highlights under them.
            let cover = if self.dock_resizing() {
                Some(CursorStyle::ResizeColumn)
            } else if self.row_dragging() {
                let refused = self.row_drag.as_ref().is_some_and(|d| d.out && d.target == Some(crate::song_drop::SongTarget::Refused));
                Some(if refused { CursorStyle::OperationNotAllowed } else { CursorStyle::ClosedHand })
            } else {
                None
            };
            window_root = window_root.child(self.drag_tracker(cover, cx));
        }
        if self.row_dragging() {
            let layout = self.layout_ref(window);
            if let Some(overlay) = self.render_row_drag_overlay(&layout) {
                window_root = window_root.child(overlay);
            }
        }
        if let Some(due) = self.next_thumb_retry.take()
            && !self.thumb_retry_pending
        {
            self.thumb_retry_pending = true;
            cx.spawn(async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                cx.background_executor().timer(due.saturating_duration_since(Instant::now())).await;
                let _ = this.update(cx, |this, cx| {
                    this.thumb_retry_pending = false;
                    cx.notify();
                });
            })
            .detach();
        }
        // The cached parts are built after this returns, so their animations are checked at paint.
        // The lyrics only need the shell's own elements redrawn.
        if self.window_shown && (self.scrollbar_animating || self.thumbs_fading || self.sidebar_autoscrolling || self.rows_autoscrolling) {
            window.request_animation_frame();
        }
        window_root.child(self.press_watcher(cx)).child(
            canvas(|_, _, _| {}, {
                let clock = self.clock.clone();
                let this = self.this.clone();
                move |_, _, window, cx| {
                    clock.paint_finished();
                    // Lists build their rows after `render` returns, so requested rows can only be sent from here.
                    // Otherwise an idle window never fetches them.
                    let fading = this.update(cx, |shell, _| {
                        shell.core_flush_requests();
                        shell.window_shown && (shell.thumbs_fading || shell.scrollbar_animating || shell.sidebar_autoscrolling || shell.rows_autoscrolling)
                    });
                    // Rows fade in after `render`, so this is the first place that knows. Controls that got no focus
                    // handle this frame get one on the next.
                    if fading.unwrap_or(false) || focus::missed_this_frame() {
                        window.request_animation_frame();
                    }
                }
            })
            .size_0(),
        )
    }
}

impl Shell {
    /// Handles the main window being minimized, hidden or shown again.
    pub fn window_visibility_changed(&mut self, shown: bool, window: &mut Window, cx: &mut Context<Self>) {
        let was = self.window_shown;
        self.set_window_shown(shown, cx);
        if was && !shown {
            self.release_hidden_memory(Some(window), cx);
        }
    }

    pub fn set_window_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        if self.window_shown == shown {
            return;
        }
        self.window_shown = shown;
        if let Some(handle) = self.main_window {
            crate::animation::set_window_shown(handle.window_id(), shown);
        }
        self.mini_main_shown(shown, cx);
        self.mini_on_minimize(shown, cx);
        if std::env::var_os("NAVIA_FRAME_LOG").is_some() {
            eprintln!("navia: window {} at {:.1} s", if shown { "shown" } else { "hidden" }, crate::testing::since_start_ms() / 1000.0);
        }
        // Coming back, draw one frame with everything current. Lyrics jump to the current line instead
        // of gliding from where they were.
        if shown {
            self.lyrics_view.restart_clock();
            cx.notify();
        }
    }

    /// A redraw that only changes what's on screen, like the position advancing. Skipped while
    /// hidden. The state still updates, and `set_window_shown` redraws once.
    pub fn notify_visual(&self, cx: &mut Context<Self>) {
        // The mini player shows the same state while the main window is minimized.
        if self.window_shown || self.mini.window.is_some() {
            cx.notify();
        }
    }

    /// Esc closes the top layer only. An open menu keeps the row selection.
    pub fn dismiss_top(&mut self, cx: &mut Context<Self>) {
        if self.rows_drag_cancel(cx) {
            return;
        }
        if self.submenu_back(cx) {
            return;
        }
        if self.menu.is_some() {
            self.close_menu(cx);
            return;
        }
        if self.close_filter_pop(cx) {
            return;
        }
        if self.quality_pop_pinned() && self.close_quality_popover(cx) {
            return;
        }
        if self.controls_dismiss(cx) {
            return;
        }
        if self.confirm.take().is_some() {
            cx.notify();
            return;
        }
        if self.close_info(cx) || self.close_about(cx) {
            return;
        }
        if self.connect.is_some() {
            return;
        }
        if self.close_new_playlist(cx) {
            return;
        }
        if self.close_airplay_dialog(cx) {
            return;
        }
        if self.close_share_dialog(cx) {
            return;
        }
        if self.close_edit_playlist(cx) {
            return;
        }
        if self.expanded {
            self.expanded = false;
            cx.notify();
            return;
        }
        if std::mem::take(&mut self.search_active) {
            cx.notify();
            return;
        }
        self.selection.clear();
        // Cached Songs draws its rows as page blocks, which keep their look until the page rebuilds.
        if self.selection_list == ListId::Cached {
            self.invalidate_page();
        }
        cx.notify();
    }

    /// Handles keys in order. Menus and selects, then the focused field, then Space.
    fn on_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        // Space and Tab are handled in `on_key_capture`, because a pressed control must not also play.
        if self.menu.is_some() {
            self.menu_key(event, cx);
            return;
        }
        // Delete or Backspace takes selected songs out of the queue's Autoplay section.
        let key = event.keystroke.key.as_str();
        if (key == "delete" || key == "backspace") && self.confirm.is_none() && self.focused_target().is_none() && ((self.selection_list == ListId::Queue && self.queue_remove_selected(cx)) || (self.selection_list == ListId::Cached && self.cached_remove_selected(cx))) {
            return;
        }
        if self.focused_target().is_none() && self.cover_row_key(key, cx) {
            return;
        }
        if self.confirm.is_none() && self.controls_key(event, cx) {
            return;
        }
        if let Some(target) = self.focused_target() {
            self.field_key(&target, event, cx);
        }
    }

    /// Runs before anything else sees the key. Tab walks the controls, and Space presses the
    /// keyboard-focused control or toggles playback.
    fn on_key_capture(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let mods = event.keystroke.modifiers;
        let key = event.keystroke.key.as_str();
        // The mini player's shortcut, outside text fields and the sign-in screen.
        if Self::is_mini_shortcut(&event.keystroke) && self.focused_target().is_none() && self.connect.is_none() {
            self.toggle_mini_player(cx);
            cx.stop_propagation();
            return;
        }
        if key == "tab" && !mods.control && !mods.alt && !mods.platform {
            self.on_tab(mods.shift, window, cx);
            cx.stop_propagation();
            return;
        }
        if key != "space" || mods.control || mods.alt || mods.platform || mods.shift {
            return;
        }
        // No player behind the sign-in screen.
        if self.connect.is_some() {
            if focus::keyboard() && focus::focused_kind(window, cx) == Some(focus::StopKind::Button) {
                return;
            }
            if self.focused_target().is_some() {
                return;
            }
            cx.stop_propagation();
            return;
        }
        // Menus and selects have their own keys, and fields type a space.
        if self.menu.is_some() || self.controls.select.is_some() || self.focused_target().is_some() {
            return;
        }
        if focus::keyboard() && focus::focused_kind(window, cx) == Some(focus::StopKind::Button) {
            return;
        }
        self.toggle_play(cx);
        cx.stop_propagation();
    }

    /// Tab and Shift+Tab. A menu or select closes instead. Dialogs and the expanded player keep
    /// the walk inside them, since nothing else is a stop then.
    fn on_tab(&mut self, back: bool, window: &mut Window, cx: &mut Context<Self>) {
        focus::set_keyboard(true);
        if self.menu.take().is_some() {
            cx.notify();
            return;
        }
        if self.controls.select.is_some() {
            self.controls_dismiss(cx);
            return;
        }
        // Start from the field being typed in, if any, even if a click focused it.
        if let Some(target) = self.focused_target()
            && let Some(handle) = focus::handle_for_field(&target)
        {
            window.focus(&handle, cx);
        }
        if back {
            window.focus_prev(cx);
        } else {
            window.focus_next(cx);
        }
        focus::request_report();
        match focus::focused_field(window, cx) {
            Some(target) => self.focus_field(target, cx),
            None => {
                self.controls.text = None;
                self.search_active = false;
            }
        }
        cx.notify();
    }

    /// Keeps window focus valid, on stops that still exist, and moves it when a modal layer opens
    /// or closes.
    fn sync_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let modal = self.confirm.is_some() || self.new_playlist.is_some() || self.airplay_dialog_open() || self.info.is_some() || self.about.is_some() || self.shares.dialog.is_some() || self.expanded;
        if modal != self.modal_was {
            self.modal_was = modal;
            if modal {
                self.focus_return = window.focused(cx);
                if focus::keyboard() {
                    // Move into the new layer once it has painted its stops.
                    window.on_next_frame(move |window, cx| window.focus_next(cx));
                }
            } else if let Some(handle) = self.focus_return.take()
                && focus::keyboard()
            {
                // The stop isn't built again until this frame, so don't call it stale yet.
                window.focus(&handle, cx);
                return;
            }
        }
        if focus::focus_is_stale(window, cx) {
            window.focus(&self.focus, cx);
        }
        if let Some((bounds, visible)) = focus::take_report()
            && !modal
        {
            self.scroll_focused_into_view(bounds, visible);
        }
    }

    /// Scrolls the area under the focused control so all of it shows.
    fn scroll_focused_into_view(&mut self, bounds: Bounds<Pixels>, visible: Bounds<Pixels>) {
        let margin = self.theme.s(6.0);
        let overflow_bottom = bounds.origin.y + bounds.size.height + margin - (visible.origin.y + visible.size.height);
        let overflow_top = visible.origin.y - bounds.origin.y + margin;
        let delta = if f32::from(overflow_bottom) > 0.0 {
            overflow_bottom
        } else if f32::from(overflow_top) > 0.0 {
            -overflow_top
        } else {
            return;
        };
        // The sidebar scrolls on its own. Everything else in the main column is the page list or the
        // song table.
        let sidebar_right = self.edge.left + self.theme.s(WINDOW_INSET + self.prefs.sidebar_width.clamp(SIDEBAR_W_MIN, SIDEBAR_W_MAX));
        if self.prefs.sidebar_open && bounds.origin.x < sidebar_right {
            let offset = self.sidebar_scroll.offset();
            self.sidebar_scroll.set_offset(point(offset.x, offset.y - delta));
        } else if self.page_uses_list() {
            self.page.state.scroll_by(delta);
        } else {
            let handle = self.list_scroll.0.borrow().base_handle.clone();
            let offset = handle.offset();
            handle.set_offset(point(offset.x, offset.y - delta));
        }
    }

    /// Sees every press first, wherever it lands, so fields lose focus like in a browser.
    fn press_watcher(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let on_press = cx.listener(|this: &mut Self, _: &gpui::MouseDownEvent, _: &mut Window, cx: &mut Context<Self>| {
            // The pointer took over, so hide the ring and let Space toggle playback again.
            focus::set_keyboard(false);
            this.controls_outside_press(cx)
        });
        canvas(|_, _, _| {}, move |_, _, window, _| {
            window.on_mouse_event(move |event: &gpui::MouseDownEvent, phase, window, cx| {
                if phase == gpui::DispatchPhase::Capture {
                    on_press(event, window, cx);
                }
            });
        })
        .absolute()
        .size_0()
        .into_any_element()
    }

    pub fn seek_to(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let bounds = *self.seek_bounds.borrow();
        let Some(song) = self.current_song().cloned() else { return };
        if f32::from(bounds.size.width) > 0.0 {
            let frac = (f32::from(x - bounds.origin.x) / f32::from(bounds.size.width)).clamp(0.0, 1.0);
            self.player.position = frac * song.duration_s as f32;
            cx.notify();
        }
    }

    pub fn set_volume_at(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let bounds = *self.volume_bounds.borrow();
        if f32::from(bounds.size.width) > 0.0 {
            let frac = (f32::from(x - bounds.origin.x) / f32::from(bounds.size.width)).clamp(0.0, 1.0);
            self.set_volume_percent(frac * 100.0);
            self.prefs.save();
            cx.notify();
        }
    }

    pub fn change_volume(&mut self, delta: f32, cx: &mut Context<Self>) {
        self.set_volume_percent(self.volume_percent() + delta);
        self.prefs.save();
        cx.notify();
    }

    pub fn seek_dragging(&self) -> bool {
        matches!(self.dragging, Some(Drag::Seek))
    }

    /// Play or pause, for the transport buttons.
    pub fn toggle_play(&mut self, cx: &mut Context<Self>) {
        if self.player.song_ix.is_none() {
            return;
        }
        if self.core.is_some() {
            self.core_toggle(cx);
        } else {
            self.player.playing = !self.player.playing;
            cx.notify();
        }
    }

    /// Cycles repeat through off, all and one.
    pub fn toggle_repeat(&mut self, cx: &mut Context<Self>) {
        use navia_core::audio::player::Repeat;
        self.player.repeat = match self.player.repeat {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        };
        self.core_apply_modes(cx);
        cx.notify();
    }

    pub fn toggle_shuffle(&mut self, cx: &mut Context<Self>) {
        self.player.shuffle = !self.player.shuffle;
        self.core_apply_modes(cx);
        cx.notify();
    }

    /// the pointer on the bar's line (or just above it): it raises after `BAR_SCRUB_DELAY`, unless it moves on.
    pub fn bar_line_hover(&mut self, on: bool, cx: &mut Context<Self>) {
        self.bar_raise.generation += 1;
        if !on || self.bar_raise.raised {
            return;
        }
        let generation = self.bar_raise.generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs_f32(crate::theme::motion::BAR_SCRUB_DELAY)).await;
            let _ = this.update(cx, |this, cx| {
                if this.bar_raise.generation == generation && this.bar_raise.over_block {
                    this.bar_raise.raised = true;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// the pointer over the bar's now-playing block: once it leaves, the line comes down after `REVEAL_GRACE`.
    pub fn bar_block_hover(&mut self, on: bool, cx: &mut Context<Self>) {
        self.bar_raise.over_block = on;
        if on {
            return;
        }
        self.bar_raise.generation += 1;
        let generation = self.bar_raise.generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs_f32(crate::theme::motion::REVEAL_GRACE)).await;
            let _ = this.update(cx, |this, cx| {
                if this.bar_raise.generation == generation && this.bar_raise.raised && !matches!(this.dragging, Some(Drag::Seek)) {
                    this.bar_raise.raised = false;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub fn bar_volume_hover(&mut self, panel: bool, on: bool, cx: &mut Context<Self>) {
        // Dragging the slider keeps the panel open even if the pointer strays off it.
        if !on && matches!(self.dragging, Some(Drag::Volume)) {
            return;
        }
        if panel {
            self.bar_volume.panel = on;
        } else {
            self.bar_volume.speaker = on;
        }
        self.bar_volume.generation += 1;
        if self.bar_volume.speaker || self.bar_volume.panel {
            self.bar_volume.open = true;
            cx.notify();
            return;
        }
        let generation = self.bar_volume.generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(220)).await;
            let _ = this.update(cx, |this, cx| {
                if this.bar_volume.generation == generation {
                    this.bar_volume.open = false;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub fn toggle_mute(&mut self, cx: &mut Context<Self>) {
        self.prefs.muted = !self.prefs.muted;
        self.prefs.save();
        self.core_apply_volume();
        cx.notify();
    }

    fn on_drag_move(&mut self, position: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        match self.dragging {
            Some(Drag::Seek) => self.seek_to(position.x, cx),
            Some(Drag::Volume) => self.set_volume_at(position.x, cx),
            // Handled by `drag_tracker`, which knows the window's size.
            Some(Drag::Pointer) => self.pointer_drag_move(position, cx),
            Some(Drag::SidebarPlaylist) => self.sidebar_drag_move(position, cx),
            Some(Drag::Rows) => self.rows_drag_move(position, cx),
            Some(Drag::DockWidth { .. }) | Some(Drag::SidebarWidth { .. }) | Some(Drag::Scroll { .. }) | None => {}
        }
    }

    /// Follows the pointer everywhere during a drag, even over occluding controls and outside the
    /// window, where the root's listeners don't fire. For the dock it also holds the resize cursor
    /// so it doesn't flicker.
    fn drag_tracker(&mut self, cover: Option<CursorStyle>, cx: &mut Context<Self>) -> AnyElement {
        let on_move = cx.listener(|this: &mut Self, event: &gpui::MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>| {
            if event.pressed_button != Some(gpui::MouseButton::Left) {
                this.end_drag(cx);
                return;
            }
            this.follow_pointer(event.position, window, cx);
        });
        let on_up = cx.listener(|this: &mut Self, _: &gpui::MouseUpEvent, _: &mut Window, cx: &mut Context<Self>| this.end_drag(cx));
        let tracker = canvas(|_, _, _| {}, move |_, _, window, _| {
            window.on_mouse_event(move |event: &gpui::MouseMoveEvent, phase, window, cx| {
                if phase == gpui::DispatchPhase::Capture {
                    on_move(event, window, cx);
                }
            });
            window.on_mouse_event(move |event: &gpui::MouseUpEvent, phase, window, cx| {
                if phase == gpui::DispatchPhase::Capture && event.button == gpui::MouseButton::Left {
                    on_up(event, window, cx);
                }
            });
        })
        .absolute()
        .size_0();
        div().when_some(cover, |d, cursor| d.absolute().inset_0().occlude().cursor(cursor)).child(tracker).into_any_element()
    }

    fn follow_pointer(&mut self, position: gpui::Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        match self.dragging {
            Some(Drag::DockWidth { grab_offset }) => {
                let viewport = self.edge.content_size(window.viewport_size());
                let right = viewport.width - self.theme.s(WINDOW_INSET);
                let tokens = f32::from(right - (position.x - self.edge.left - grab_offset)) / self.theme.token_scale();
                self.set_dock_width(tokens, viewport, window.scale_factor(), cx);
            }
            Some(Drag::SidebarWidth { grab_offset }) => {
                let viewport = self.edge.content_size(window.viewport_size());
                let left = self.theme.s(WINDOW_INSET);
                let tokens = f32::from(position.x - self.edge.left - grab_offset - left) / self.theme.token_scale();
                self.set_sidebar_width(tokens, viewport, window.scale_factor(), cx);
            }
            Some(Drag::Scroll { id, grab }) => self.scrollbar_drag(id, grab, position, cx),
            _ => self.on_drag_move(position, cx),
        }
    }
}

/// The 1px light line just inside the window's edge, with no line on tiled sides.
fn frame_outline(frame: chrome::Frame) -> AnyElement {
    let tiling = frame.tiling;
    let line = gpui::white().opacity(FRAME_OUTLINE_ALPHA);
    let width = px(FRAME_OUTLINE_WIDTH);
    let outline = div().absolute().inset_0().border_color(line).when(!tiling.top, |d| d.border_t(width)).when(!tiling.right, |d| d.border_r(width)).when(!tiling.bottom, |d| d.border_b(width)).when(!tiling.left, |d| d.border_l(width));
    chrome::round_corners(outline, frame.corners).into_any_element()
}

/// The drop shadow outside the window. GPUI draws a shadow under its element, which a translucent
/// window would show, so each side paints the shadow clipped to its margin and the corners paint
/// only the sliver outside the rounded corner.
fn frame_shadow(frame: chrome::Frame, surface: gpui::Size<Pixels>, active: bool) -> AnyElement {
    let (w, h) = (f32::from(surface.width), f32::from(surface.height));
    let (top, right, bottom, left) = (f32::from(frame.top), f32::from(frame.right), f32::from(frame.bottom), f32::from(frame.left));
    let alpha = if active { FRAME_SHADOW_ALPHA } else { FRAME_SHADOW_ALPHA_INACTIVE };
    chrome::round_corners(div().absolute().left(px(left)).top(px(top)).w(px(w - left - right)).h(px(h - top - bottom)), frame.corners)
        .shadow(vec![gpui::BoxShadow { color: gpui::black().opacity(alpha), offset: point(px(0.0), px(0.0)), blur_radius: px(FRAME_SHADOW_BLUR), spread_radius: px(0.0), inset: false }])
        .into_any_element()
}

/// Invisible grips along the window edge. They reach FRAME_GRIP_OUTSIDE over the shadow margin and
/// FRAME_GRIP_INSIDE into the window, with corners FRAME_GRIP_CORNER larger.
fn resize_edges(frame: chrome::Frame, surface: gpui::Size<Pixels>, tiling: Tiling, window: &mut Window) -> AnyElement {
    if window.is_maximized() || window.is_fullscreen() {
        return div().into_any_element();
    }
    let (vw, vh) = (f32::from(surface.width), f32::from(surface.height));
    // Each side's grip starts where it reaches out of the window, not at the surface's edge.
    let out = |margin: Pixels| f32::from(margin).min(FRAME_GRIP_OUTSIDE);
    let (x0, y0) = (f32::from(frame.left) - out(frame.left), f32::from(frame.top) - out(frame.top));
    let (x1, y1) = (vw - f32::from(frame.right) + out(frame.right), vh - f32::from(frame.bottom) + out(frame.bottom));
    let band = |margin: Pixels| out(margin) + FRAME_GRIP_INSIDE;
    let (top, right, bottom, left) = (band(frame.top), band(frame.right), band(frame.bottom), band(frame.left));
    let reach = FRAME_GRIP_CORNER;
    let (w, h) = (x1 - x0, y1 - y0);
    let edges: [(ResizeEdge, f32, f32, f32, f32); 8] = [
        (ResizeEdge::TopLeft, x0, y0, left + reach, top + reach),
        (ResizeEdge::Top, x0 + left + reach, y0, w - left - right - reach * 2.0, top),
        (ResizeEdge::TopRight, x1 - right - reach, y0, right + reach, top + reach),
        (ResizeEdge::Left, x0, y0 + top + reach, left, h - top - bottom - reach * 2.0),
        (ResizeEdge::Right, x1 - right, y0 + top + reach, right, h - top - bottom - reach * 2.0),
        (ResizeEdge::BottomLeft, x0, y1 - bottom - reach, left + reach, bottom + reach),
        (ResizeEdge::Bottom, x0 + left + reach, y1 - bottom, w - left - right - reach * 2.0, bottom),
        (ResizeEdge::BottomRight, x1 - right - reach, y1 - bottom - reach, right + reach, bottom + reach),
    ];
    let mut container = div().absolute().inset_0();
    for (i, (edge, x, y, width, height)) in edges.into_iter().enumerate() {
        let tiled = match edge {
            ResizeEdge::Top => tiling.top,
            ResizeEdge::Bottom => tiling.bottom,
            ResizeEdge::Left => tiling.left,
            ResizeEdge::Right => tiling.right,
            ResizeEdge::TopLeft => tiling.top || tiling.left,
            ResizeEdge::TopRight => tiling.top || tiling.right,
            ResizeEdge::BottomLeft => tiling.bottom || tiling.left,
            ResizeEdge::BottomRight => tiling.bottom || tiling.right,
        };
        if tiled {
            continue;
        }
        let cursor = match edge {
            ResizeEdge::Top | ResizeEdge::Bottom => CursorStyle::ResizeUpDown,
            ResizeEdge::Left | ResizeEdge::Right => CursorStyle::ResizeLeftRight,
            ResizeEdge::TopLeft | ResizeEdge::BottomRight => CursorStyle::ResizeUpLeftDownRight,
            ResizeEdge::TopRight | ResizeEdge::BottomLeft => CursorStyle::ResizeUpRightDownLeft,
        };
        container = container.child(
            div()
                .id(("resize", i))
                .absolute()
                .left(px(x))
                .top(px(y))
                .w(px(width))
                .h(px(height))
                .occlude()
                .cursor(cursor)
                .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                    cx.stop_propagation();
                    window.start_window_resize(edge);
                }),
        );
    }
    container.into_any_element()
}
