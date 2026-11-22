//! Navia's desktop shell in GPUI.
// the settings file is one `json!` object, longer than the macro's default recursion allows.
#![recursion_limit = "256"]
// no console window in release builds; debug builds keep it for logs.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod account;
mod airplay;
mod view_airplay;
mod art;
mod anim_art;
mod animation;
mod autoplay;
mod marquee;
mod heat;
mod catalog;
mod chrome;
mod assets;
mod color;
mod system_accent;
mod core_library;
mod core_player;
mod deezer_choice;
mod queue_bridge;
mod lyrics;
mod mini_player;
mod view_mini;
mod model;
mod persist;
mod playlist_menu;
mod sample;
mod selection;
mod shell;
mod shell_data;
mod testing;
mod theme;
mod text_field;
mod thumb_cache;
mod view_new_playlist;
mod view_edit_playlist;
mod quality_badge;
mod view_playlist_image;
mod view_queue_offer;
mod ui;
mod ui_scroll;
mod view_bar;
mod view_search;
mod view_rows;
mod view_playlists;
mod view_playlist;
mod view_recommended;
mod view_menu;
mod view_home;
mod view_liked;
mod view_discover;
mod view_for_you;
mod view_chrome;
mod view_cards;
mod view_connect;
mod view_sync;
mod view_cached;
mod view_downloads;
mod view_shares;
mod view_browse;
mod view_artists;
mod view_artist;
mod view_albums;
mod view_split;
mod view_upcoming;
mod view_album;
mod pages;
mod view_dock;
mod view_expanded;
mod view_lyrics;
mod view_queue;
mod view_info;
mod view_about;
mod collection_download;
mod row_drag;
mod song_drop;
mod song_links;
mod sidebar_playlists;
mod slots;
mod view_sidebar;
mod listen_along;
mod view_listening;
mod view_settings;
mod view_storage;
mod view_equalizer;
mod view_smart;
mod view_filters;
mod ui_controls;
mod eq_model;
mod smart_model;
mod focus;
mod i18n;
mod view_songs;
mod view_topbar;
mod view_window_buttons;
mod view_account;
mod autostart;
mod updates;
mod memory;
mod tray;
mod tray_glue;
mod app_menu;

use gpui::{App, AppContext, Bounds, Entity, Pixels, QuitMode, Size, TitlebarOptions, WindowBounds, WindowDecorations, WindowOptions, point, px, size};

fn window_size() -> (f32, f32) {
    std::env::var("NAVIA_WINDOW")
        .ok()
        .and_then(|v| {
            let (w, h) = v.split_once('x')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        })
        .unwrap_or((1600.0, 1000.0))
}

fn main() {
    // The installer and uninstaller run Navia with their own arguments. This handles them and exits.
    #[cfg(target_os = "windows")]
    updates::run_installer_hooks();
    // Answered before touching a display, so packaging can check the binary headless.
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("{} {}", option_env!("NAVIA_PACKAGE_NAME").unwrap_or(env!("CARGO_PKG_NAME")), updates::version_label());
        return;
    }
    // Set before any client exists. The User-Agent carries the release version.
    navia_core::client::set_app_version(env!("CARGO_PKG_VERSION"));
    i18n::apply(persist::Prefs::load().language.as_deref());
    chrome::require_wayland_session();
    testing::mark_process_start();
    testing::start_frame_log();
    core_player::limit_allocator_arenas();
    #[cfg(target_os = "windows")]
    testing::spawn_windows_memory_log();
    gpui_platform::application().with_assets(assets::Assets).run(move |cx: &mut App| {
        assets::register_fonts(cx);
        shell::bind_keys(cx);
        // closing the window may leave Navia playing in the tray.
        cx.set_quit_mode(QuitMode::Explicit);
        let mut prefs = persist::Prefs::load();
        if prefs.migrate_to_system_accent() {
            prefs.save();
        }
        let support = chrome::support();
        chrome::watch_double_click();
        chrome::watch_reduce_motion(cx);
        let shell = shell::Shell::new(prefs, support, cx);
        cx.set_global(KeepShell(shell.clone()));
        if autostart::started_hidden() {
            show_unless_tray(shell, cx);
        } else {
            open_main_window(shell, None, cx);
        }
    });
}

/// while its window is closed to the tray.
struct KeepShell(#[allow(dead_code)] Entity<shell::Shell>);
impl gpui::Global for KeepShell {}

/// Opens the window anyway after a hidden start if no tray host appears, so Navia is never
/// unreachable. The desktop panel may start a little after Navia.
fn show_unless_tray(shell: Entity<shell::Shell>, cx: &mut App) {
    const WAIT: std::time::Duration = std::time::Duration::from_secs(15);
    const STEP: std::time::Duration = std::time::Duration::from_millis(500);
    let has_tray = shell.read(cx).tray.is_some();
    cx.spawn(async move |cx| {
        let start = std::time::Instant::now();
        while has_tray && start.elapsed() < WAIT {
            if cx.background_spawn(async { tray::host_available() }).await {
                return;
            }
            cx.background_executor().timer(STEP).await;
        }
        cx.update(|cx| open_main_window(shell, None, cx));
    })
    .detach();
}

/// Opens the main window, at launch or from the tray. `size_was` is its size when closed,
/// including Navia's frame margin.
pub fn open_main_window(shell: Entity<shell::Shell>, size_was: Option<Size<Pixels>>, cx: &mut App) {
    let (support, prefs) = {
        let s = shell.read(cx);
        (chrome::Support { blur: s.blur_detected, system_frame: s.system_frame_available, layer_shell: s.layer_shell_available }, s.prefs.clone())
    };
    let decorations = chrome::decorations(support, prefs.system_title_bar);
    let surface = size_was.unwrap_or_else(|| {
        let (w, h) = window_size();
        // With client decorations the surface includes the shadow margin, so the visible window
        // stays the requested size.
        let margin = if decorations == WindowDecorations::Client { chrome::shadow_margin() } else { 0.0 };
        size(px(w + margin * 2.0), px(h + margin * 2.0))
    });
    let bounds = Bounds::centered(None, surface, cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions { title: Some(navia_core::client::APP_DISPLAY_NAME.into()), appears_transparent: true, traffic_light_position: Some(point(px(16.0), px(18.0))) }),
        window_decorations: Some(decorations),
        app_id: Some(navia_core::client::APP_ID.to_string()),
        window_background: chrome::window_background(prefs.translucent_on(support.blur)),
        ..Default::default()
    };
    let opened = cx.open_window(options, move |window, cx| {
        shell.update(cx, |shell, cx| shell.attach_window(window, cx));
        shell
    });
    if let Err(err) = opened {
        eprintln!("navia: main window failed: {err}");
        cx.quit();
        return;
    }
    cx.activate(true);
}
