//! The mini player, a small window of its own showing the playing song. It moves freely and
//! stays above other windows where the desktop allows.
//!
//! - With layer-shell (KDE, Hyprland, Sway) it's an overlay surface anchored top-left that Navia
//!   moves by changing its margins. A brief background probe reports the work area to stay in.
//! - Without layer-shell, as on GNOME, it's an ordinary small window with a one-time hint about
//!   the window menu's Always on Top.
//! - On Windows it's a topmost pop-up window.
//!
//! The main window minimizes while it's open. There is one player, and the mini window draws the
//! shell's state with the shell's own controls.

use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    AnyWindowHandle, Bounds, Context, Entity, FocusHandle, Pixels, Point, Size, Subscription, Window, WindowBackgroundAppearance, WindowBounds,
    WindowHandle, WindowKind, WindowOptions, point, px, size,
};

use crate::shell::Shell;
use crate::theme::size::*;

/// how long a minimized main window gets to come back after an activation request before it's opened again
const RAISE_WAIT: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MiniKind {
    /// A layer-shell overlay that Navia moves itself.
    Layer,
    /// An ordinary window others can cover.
    Window,
    /// A topmost pop-up, on Windows.
    Topmost,
}

impl MiniKind {
    pub fn for_desktop(layer_shell: bool) -> Self {
        if cfg!(target_os = "windows") {
            MiniKind::Topmost
        } else if layer_shell {
            MiniKind::Layer
        } else {
            MiniKind::Window
        }
    }
}

#[derive(Default)]
pub struct MiniPlayer {
    pub window: Option<WindowHandle<MiniView>>,
    /// The window was requested but isn't open yet.
    opening: bool,
    /// The work-area probe while it's open, on layer-shell only.
    probe: Option<AnyWindowHandle>,
    /// The main window, minimized while the mini player is open.
    main: Option<AnyWindowHandle>,
    /// The main window was hidden after the mini player opened, so its return from the taskbar
    /// means the mini player should close.
    pub main_hidden: bool,
    /// The output's work area from the probe on layer-shell, or the display's bounds on Windows.
    area: Option<Bounds<Pixels>>,
    /// The display the main window was on.
    display: Option<gpui::DisplayId>,
    /// The card's top-left corner, in the work area on layer-shell or on the screen on Windows.
    pub at: Point<Pixels>,
    /// Where the pointer grabbed a layer-shell mini player being dragged.
    pub grab: Option<Point<Pixels>>,
    /// A resize of a layer-shell mini player from one of its grips.
    pub resize: Option<MiniResize>,
    /// The margins last sent, so a move that rounds to the same pixels isn't sent again.
    sent: Option<(i32, i32)>,
    /// The input region last set on layer-shell. It's the card itself, so clicks on its shadow pass through.
    pub region: Option<Bounds<Pixels>>,
    /// The size the window was last given, to resize it when the size or the hint changes.
    pub surface: Option<Size<Pixels>>,
    /// The GNOME hint's measured height, once laid out.
    pub hint_h: std::rc::Rc<std::cell::RefCell<Option<Pixels>>>,
}

impl MiniPlayer {
    pub fn is_open(&self) -> bool {
        self.window.is_some() || self.probe.is_some() || self.opening
    }
}

/// A layer-shell resize in progress. The grip, and the pointer, card position and size at the press.
#[derive(Clone, Copy)]
pub struct MiniResize {
    pub edge: gpui::ResizeEdge,
    pointer: Point<Pixels>,
    at: Point<Pixels>,
    size: Size<Pixels>,
}

/// The mini player window's root. It draws the shell and redraws whenever the shell changes.
pub struct MiniView {
    shell: Entity<Shell>,
    pub focus: FocusHandle,
    _observe: Subscription,
}

impl Render for MiniView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focus = self.focus.clone();
        self.shell.update(cx, |shell, cx| {
            use crate::marquee::Host;
            shell.marquees_begin(Host::Mini);
            let mini = shell.render_mini(&focus, window, cx);
            shell.marquees_end(Host::Mini, cx);
            // the scrolling names are drawn over the card, each a view of its own.
            gpui::div().size_full().child(crate::ui::hover_clears_on_leave()).child(mini).children(shell.marquee_overlays(Host::Mini)).into_any_element()
        })
    }
}

/// A background surface stretched over the output's work area on layer-shell desktops. Its
/// configured size is that area, which it reports before closing.
struct WorkAreaProbe {
    shell: gpui::WeakEntity<Shell>,
    reported: bool,
}

impl Render for WorkAreaProbe {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let area = window.viewport_size();
        if !self.reported && area.width > px(0.0) && area.height > px(0.0) {
            self.reported = true;
            let shell = self.shell.clone();
            cx.defer(move |cx| {
                shell.update(cx, |shell, cx| shell.mini_area_known(Some(area), cx)).ok();
            });
        }
        gpui::div()
    }
}

impl Shell {
    /// The card's size in tokens, as last resized, within its limits.
    pub fn mini_tokens(&self) -> (f32, f32) {
        let (w, h) = self.prefs.mini_size.unwrap_or((MINI_W, MINI_H));
        (w.clamp(MINI_W, MINI_MAX), h.clamp(MINI_H, MINI_MAX))
    }

    /// The mini player's card in window pixels.
    pub fn mini_card_size(&self) -> Size<Pixels> {
        let (w, h) = self.mini_tokens();
        size(self.theme.s(w), self.theme.s(h))
    }

    /// Whether to use the stacked large layout, when the card is tall and square enough.
    pub fn mini_stacked(&self) -> bool {
        let (w, h) = self.mini_tokens();
        h >= MINI_STACK_H && h >= w * MINI_STACK_RATIO
    }

    /// Transparent room around the card inside its window, so its shadow isn't cut. Windows has
    /// none, because DWM provides corners and shadow and a transparent margin renders black there.
    pub fn mini_room(&self) -> Pixels {
        if cfg!(target_os = "windows") { px(0.0) } else { self.theme.s(crate::ui::shadow_float_reach()) }
    }

    /// The card's corner radius as Navia draws it. None on Windows, where DWM rounds the window.
    pub fn mini_radius(&self) -> Pixels {
        if cfg!(target_os = "windows") { px(0.0) } else { self.theme.s(MINI_R) }
    }

    /// Whether the card is translucent glass. Needs translucency on and a window edge to blur
    /// behind, so never on Windows.
    pub fn mini_glass(&self) -> bool {
        self.translucent() && cfg!(not(target_os = "windows"))
    }

    /// Whether the GNOME hint shows under the card, until it's dismissed.
    pub fn mini_hint_shown(&self) -> bool {
        self.mini_kind() == MiniKind::Window && !self.prefs.mini_hint_seen
    }

    pub fn mini_kind(&self) -> MiniKind {
        MiniKind::for_desktop(self.layer_shell_available)
    }

    /// The window's whole size, including the card, its shadow room and the hint while it shows.
    fn mini_surface_size(&self) -> Size<Pixels> {
        let card = self.mini_card_size();
        let room = self.mini_room();
        let mut s = size(card.width + room * 2.0, card.height + room * 2.0);
        if self.mini_hint_shown() {
            let t = self.theme;
            // The hint's height is known after layout, and the window grows to it next frame.
            s.width = s.width.max(t.s(MINI_HINT_W) + room * 2.0);
            if let Some(hint_h) = *self.mini.hint_h.borrow() {
                s.height += t.s(MINI_HINT_GAP) + hint_h;
            }
        }
        s
    }

    /// Opens the mini player or returns to the full window, for the button, menu item and shortcut.
    pub fn toggle_mini_player(&mut self, cx: &mut Context<Self>) {
        if self.mini.is_open() {
            self.close_mini_player(true, cx);
        } else {
            self.open_mini_player(cx);
        }
    }

    /// Opens the mini player. Window work is deferred past the current update, since the main
    /// window may be the one being updated.
    pub fn open_mini_player(&mut self, cx: &mut Context<Self>) {
        if self.mini.is_open() || self.current_song().is_none() {
            return;
        }
        let this = self.this.clone();
        cx.defer(move |cx| {
            this.update(cx, |shell, cx| shell.open_mini_now(cx)).ok();
        });
    }

    fn open_mini_now(&mut self, cx: &mut Context<Self>) {
        if self.mini.is_open() {
            return;
        }
        let main = self.main_window;
        self.mini.main = main;
        // Opened because the main window was minimized, so its return closes the mini player.
        self.mini.main_hidden = !self.window_shown;
        let (display, bounds) = main
            .and_then(|h| h.update(cx, |_, window, cx| window.display(cx).map(|d| (d.id(), d.bounds()))).ok().flatten())
            .unzip();
        self.mini.display = display;
        match self.mini_kind() {
            MiniKind::Layer => self.open_work_area_probe(cx),
            MiniKind::Topmost => {
                self.mini.area = bounds;
                self.open_mini_window(cx);
            }
            MiniKind::Window => self.open_mini_window(cx),
        }
    }

    /// Learns the work area on layer-shell before placing the mini player. If the probe fails or
    /// never answers, the mini player opens where it was last left.
    #[cfg(target_os = "linux")]
    fn open_work_area_probe(&mut self, cx: &mut Context<Self>) {
        use gpui::layer_shell::{Anchor, KeyboardInteractivity, Layer, LayerShellOptions};
        let options = WindowOptions {
            kind: WindowKind::LayerShell(LayerShellOptions {
                namespace: "navia-work-area".into(),
                layer: Layer::Background,
                anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                exclusive_zone: None,
                exclusive_edge: None,
                margin: None,
                keyboard_interactivity: KeyboardInteractivity::None,
            }),
            window_bounds: Some(WindowBounds::Windowed(Bounds { origin: point(px(0.0), px(0.0)), size: size(px(0.0), px(0.0)) })),
            focus: false,
            display_id: self.mini.display,
            window_background: WindowBackgroundAppearance::Transparent,
            app_id: Some(navia_core::client::APP_ID.into()),
            ..Default::default()
        };
        let shell = self.this.clone();
        match cx.open_window(options, move |window, cx| {
            // Clicks fall through to the desktop while it's up.
            window.set_input_region(Some(&[]));
            cx.new(|_| WorkAreaProbe { shell, reported: false })
        }) {
            Ok(handle) => {
                self.mini.probe = Some(handle.into());
                let this = self.this.clone();
                cx.spawn(async move |_, cx| {
                    cx.background_executor().timer(Duration::from_millis(600)).await;
                    this.update(cx, |shell, cx| shell.mini_area_known(None, cx)).ok();
                })
                .detach();
            }
            Err(err) => {
                eprintln!("navia: work area probe failed: {err}");
                self.mini_area_known(None, cx);
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn open_work_area_probe(&mut self, cx: &mut Context<Self>) {
        self.open_mini_window(cx);
    }

    /// Closes the probe once it answers or gives up, and opens the mini player inside the area.
    pub fn mini_area_known(&mut self, area: Option<Size<Pixels>>, cx: &mut Context<Self>) {
        let Some(probe) = self.mini.probe.take() else { return };
        probe.update(cx, |_, window, _| window.remove_window()).ok();
        if let Some(area) = area {
            self.mini.area = Some(Bounds { origin: point(px(0.0), px(0.0)), size: area });
            if std::env::var_os("NAVIA_MINI_LOG").is_some() {
                eprintln!("navia: mini work area {:.0}x{:.0}", f32::from(area.width), f32::from(area.height));
            }
        }
        if self.mini.window.is_none() {
            self.open_mini_window(cx);
        }
    }

    /// Where the card opens. Where it was left, or the work area's bottom-right corner the first
    /// time, kept inside the area.
    fn mini_start(&self) -> Point<Pixels> {
        let card = self.mini_card_size();
        let margin = self.theme.s(MINI_CORNER_MARGIN);
        let area = self.mini.area;
        let at = match (self.prefs.mini_at, area) {
            (Some((x, y)), _) => point(px(x), px(y)),
            (None, Some(a)) => point(a.origin.x + a.size.width - card.width - margin, a.origin.y + a.size.height - card.height - margin),
            (None, None) => point(margin, margin),
        };
        self.clamp_mini(at)
    }

    /// Keeps the card inside the work area.
    pub fn clamp_mini(&self, at: Point<Pixels>) -> Point<Pixels> {
        let Some(area) = self.mini.area else { return at };
        let card = self.mini_card_size();
        let max_x = (area.origin.x + area.size.width - card.width).max(area.origin.x);
        let max_y = (area.origin.y + area.size.height - card.height).max(area.origin.y);
        point(at.x.clamp(area.origin.x, max_x), at.y.clamp(area.origin.y, max_y))
    }

    fn open_mini_window(&mut self, cx: &mut Context<Self>) {
        let kind = self.mini_kind();
        let room = self.mini_room();
        let surface = self.mini_surface_size();
        self.mini.at = self.mini_start();
        self.mini.sent = None;
        self.mini.region = None;
        self.mini.surface = Some(surface);
        let background = if cfg!(target_os = "windows") {
            WindowBackgroundAppearance::Opaque
        } else if self.mini_glass() {
            WindowBackgroundAppearance::Blurred
        } else {
            WindowBackgroundAppearance::Transparent
        };
        let mut options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds { origin: point(px(0.0), px(0.0)), size: surface })),
            titlebar: Some(gpui::TitlebarOptions { title: Some(crate::i18n::tr("Mini Player").into()), appears_transparent: true, traffic_light_position: None }),
            focus: true,
            show: true,
            // Navia resizes layer-shell surfaces itself. The desktop resizes windows from the card's grips.
            is_resizable: kind != MiniKind::Layer,
            window_min_size: Some(size(self.theme.s(MINI_W) + room * 2.0, self.theme.s(MINI_H) + room * 2.0)),
            window_max_size: Some(size(self.theme.s(MINI_MAX) + room * 2.0, self.theme.s(MINI_MAX) + room * 2.0)),
            is_minimizable: kind == MiniKind::Window,
            display_id: self.mini.display,
            window_background: background,
            app_id: Some(navia_core::client::APP_ID.into()),
            window_decorations: Some(gpui::WindowDecorations::Client),
            ..Default::default()
        };
        match kind {
            #[cfg(target_os = "linux")]
            MiniKind::Layer => {
                use gpui::layer_shell::{Anchor, KeyboardInteractivity, Layer, LayerShellOptions};
                let at = self.mini.at;
                options.kind = WindowKind::LayerShell(LayerShellOptions {
                    namespace: "navia-mini-player".into(),
                    layer: Layer::Overlay,
                    anchor: Anchor::TOP | Anchor::LEFT,
                    exclusive_zone: None,
                    exclusive_edge: None,
                    margin: Some((at.y - room, px(0.0), px(0.0), at.x - room)),
                    keyboard_interactivity: KeyboardInteractivity::OnDemand,
                });
                self.mini.sent = Some((f32::from(at.y - room).round() as i32, f32::from(at.x - room).round() as i32));
            }
            MiniKind::Topmost => {
                options.kind = WindowKind::PopUp;
                let at = self.mini.at;
                if let Some(bounds) = options.window_bounds.as_mut().map(|b| match b {
                    WindowBounds::Windowed(b) => b,
                    _ => unreachable!(),
                }) {
                    bounds.origin = point(at.x - room, at.y - room);
                }
            }
            _ => {}
        }
        // The new window draws its first frame from the shell while opening, so open it after this update.
        self.mini.opening = true;
        let shell = cx.entity();
        cx.defer(move |cx| {
            let view_shell = shell.clone();
            let opened = cx.open_window(options, move |window, cx| {
                let focus = cx.focus_handle();
                window.focus(&focus, cx);
                let view = cx.new(|cx| {
                    let observe = cx.observe(&view_shell, |_, _, cx| cx.notify());
                    let shell = view_shell.clone();
                    cx.observe_window_activation(window, move |_, _, cx| shell.update(cx, |shell, cx| shell.marquees_refresh(cx))).detach();
                    MiniView { shell: view_shell.clone(), focus, _observe: observe }
                });
                // Closed from the desktop, such as with Alt+F4, so return to the full window.
                let closing = view_shell.downgrade();
                window.on_window_should_close(cx, move |_, cx| {
                    closing.update(cx, |shell, cx| shell.mini_closed_by_desktop(cx)).ok();
                    true
                });
                #[cfg(target_os = "windows")]
                round_window_corners(window);
                view
            });
            shell.update(cx, |shell, cx| shell.mini_window_opened(kind, opened, cx));
        });
    }

    fn mini_window_opened(&mut self, kind: MiniKind, opened: anyhow::Result<WindowHandle<MiniView>>, cx: &mut Context<Self>) {
        self.mini.opening = false;
        match opened {
            Ok(handle) => {
                self.mini.window = Some(handle);
                // Minimize the main window, so its taskbar entry is the way back.
                if let Some(main) = self.mini.main {
                    main.update(cx, |_, window, _| window.minimize_window()).ok();
                }
                if std::env::var_os("NAVIA_MINI_LOG").is_some() {
                    eprintln!("navia: mini opened {:?} at {:.0},{:.0} {}", kind, f32::from(self.mini.at.x), f32::from(self.mini.at.y), if self.mini_stacked() { "stacked" } else { "compact" });
                }
                cx.notify();
            }
            Err(err) => {
                eprintln!("navia: mini player window failed: {err}");
                self.mini = Default::default();
            }
        }
    }

    /// Closes the mini player. `restore` brings the main window back first. The mini window goes a
    /// moment later so its activation request still counts.
    pub fn close_mini_player(&mut self, restore: bool, cx: &mut Context<Self>) {
        self.remember_mini_position(cx);
        if let Some(probe) = self.mini.probe.take() {
            let _ = probe.update(cx, |_, window, _| window.remove_window());
        }
        let Some(handle) = self.mini.window.take() else { return };
        let main = self.mini.main.take();
        self.mini.grab = None;
        if restore {
            // Closed to the tray meanwhile, so the window opens again.
            if self.main_window.is_none() {
                self.show_main_window(cx);
            } else if let Some(main) = main {
                main.update(cx, |_, window, _| window.activate_window()).ok();
                // on Wayland a minimized window comes back only for an activation tied to a recent press on one of
                // Navia's surfaces; from the global menu or the desktop there's none, so the window opens again
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(RAISE_WAIT).await;
                    this.update(cx, |shell, cx| shell.reopen_if_still_hidden(main, cx)).ok();
                })
                .detach();
            }
        }
        if std::env::var_os("NAVIA_MINI_LOG").is_some() {
            eprintln!("navia: mini closed{}", if restore { ", main window back" } else { "" });
        }
        cx.spawn(async move |_, cx| {
            cx.background_executor().timer(Duration::from_millis(if restore { 150 } else { 0 })).await;
            handle.update(cx, |_, window, _| window.remove_window()).ok();
        })
        .detach();
        cx.notify();
    }

    fn reopen_if_still_hidden(&mut self, main: gpui::AnyWindowHandle, cx: &mut Context<Self>) {
        if self.window_shown || self.mini.is_open() || self.main_window != Some(main) {
            return;
        }
        if let Ok(size) = main.update(cx, |_, window, _| window.bounds().size) {
            self.closed_size = Some(size);
        }
        main.update(cx, |_, window, _| window.remove_window()).ok();
        self.main_window = None;
        self.show_main_window(cx);
    }

    fn mini_closed_by_desktop(&mut self, cx: &mut Context<Self>) {
        if self.mini.window.is_some() {
            self.close_mini_player(true, cx);
        }
    }

    /// Closes the mini player when the main window comes back by itself from the taskbar.
    pub fn mini_main_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        if self.mini.window.is_none() {
            return;
        }
        if !shown {
            self.mini.main_hidden = true;
        } else if self.mini.main_hidden {
            self.close_mini_player(false, cx);
        }
    }

    /// Opens the mini player when the main window goes out of view, if that setting is on. Windows
    /// reports minimizing, and Wayland reports `suspended`, which KWin also sets for a window on
    /// another virtual desktop. Closing the window is handled by the close setting instead.
    pub fn mini_on_minimize(&mut self, shown: bool, cx: &mut Context<Self>) {
        if shown || !self.prefs.mini_on_minimize || self.mini.is_open() || self.main_window.is_none() {
            return;
        }
        if std::env::var_os("NAVIA_MINI_LOG").is_some() {
            eprintln!("navia: main window minimized, opening the mini player");
        }
        self.open_mini_player(cx);
    }

    /// Saves where the card is, as the layer-shell margins or the window position on Windows.
    /// Where the desktop places windows itself, there's nothing to save.
    fn remember_mini_position(&mut self, cx: &mut Context<Self>) {
        let room = self.mini_room();
        let at = match self.mini_kind() {
            MiniKind::Layer => Some(self.mini.at),
            MiniKind::Topmost => self.mini.window.and_then(|h| h.update(cx, |_, window, _| window.bounds().origin).ok()).map(|o| point(o.x + room, o.y + room)),
            MiniKind::Window => None,
        };
        if let Some(at) = at {
            let saved = Some((f32::from(at.x).round(), f32::from(at.y).round()));
            if self.prefs.mini_at != saved {
                self.prefs.mini_at = saved;
                self.prefs.save();
            }
        }
    }

    /// Toggles between compact and large. On layer-shell the corner nearest the card stays put.
    pub fn toggle_mini_size(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let next = if self.mini_stacked() { (MINI_W, MINI_H) } else { (MINI_LARGE, MINI_LARGE) };
        let before = self.mini_card_size();
        self.prefs.mini_size = Some(next);
        self.prefs.save();
        let after = self.mini_card_size();
        if self.mini_kind() == MiniKind::Layer
            && let Some(area) = self.mini.area
        {
            let mut at = self.mini.at;
            let center = point(at.x + before.width / 2.0, at.y + before.height / 2.0);
            if center.x > area.origin.x + area.size.width / 2.0 {
                at.x += before.width - after.width;
            }
            if center.y > area.origin.y + area.size.height / 2.0 {
                at.y += before.height - after.height;
            }
            self.mini.at = self.clamp_mini(at);
            self.mini.region = None;
            self.apply_mini_margin(window);
            self.remember_mini_position(cx);
        }
        self.resize_mini_surface(window);
        cx.notify();
    }

    /// A press on a resize grip. A normal window asks the desktop to resize it. On layer-shell the
    /// compositor applies a new size and margins together only frames later, so per-move updates
    /// would lag. Instead the surface covers the whole work area while the grip is held, and
    /// shrinks back around the card on release.
    pub fn mini_resize_press(&mut self, edge: gpui::ResizeEdge, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        match self.mini_kind() {
            MiniKind::Layer => {
                let Some(area) = self.mini.area else { return };
                let room = self.mini_room();
                let pointer = self.mini.at + (position - point(room, room));
                self.mini.resize = Some(MiniResize { edge, pointer, at: self.mini.at, size: self.mini_card_size() });
                self.mini.surface = Some(area.size);
                self.mini.sent = None;
                window.resize(area.size);
                window.set_layer_margin(area.origin.y, px(0.0), px(0.0), area.origin.x);
                cx.notify();
            }
            MiniKind::Window | MiniKind::Topmost => window.start_window_resize(edge),
        }
    }

    /// Whether the layer surface covers the whole work area during a resize, so window positions
    /// are the area's.
    pub fn mini_surface_full(&self, window: &Window) -> bool {
        self.mini.resize.is_some() && self.mini.area.is_some_and(|a| window.viewport_size() == a.size)
    }

    /// Where the card is drawn in its window. Inside its shadow room, or at its own place while the
    /// surface covers the work area.
    pub fn mini_card_origin(&self, window: &Window) -> Point<Pixels> {
        if self.mini_surface_full(window) {
            self.mini.at
        } else {
            let room = self.mini_room();
            point(room, room)
        }
    }

    /// Moves the card's held edges with the pointer during a layer-shell resize, within its limits
    /// and the work area.
    pub fn mini_resize_move(&mut self, position: Point<Pixels>, window: &mut Window) {
        use gpui::ResizeEdge as E;
        let Some(r) = self.mini.resize else { return };
        let Some(area) = self.mini.area else { return };
        let t = self.theme;
        // Until the compositor has the full-area surface, positions are the small surface's, which
        // sits where the card was at the press.
        let pointer = if self.mini_surface_full(window) { position + area.origin } else { r.at + (position - point(self.mini_room(), self.mini_room())) };
        let d = pointer - r.pointer;
        let (min_w, min_h, max) = (t.s(MINI_W), t.s(MINI_H), t.s(MINI_MAX));
        let (right, bottom) = (area.origin.x + area.size.width, area.origin.y + area.size.height);
        let (mut x, mut y, mut w, mut h) = (r.at.x, r.at.y, r.size.width, r.size.height);
        let east = matches!(r.edge, E::Right | E::TopRight | E::BottomRight);
        let west = matches!(r.edge, E::Left | E::TopLeft | E::BottomLeft);
        let south = matches!(r.edge, E::Bottom | E::BottomLeft | E::BottomRight);
        let north = matches!(r.edge, E::Top | E::TopLeft | E::TopRight);
        if east {
            w = (w + d.x).max(min_w).min(max).min(right - x);
        }
        if south {
            h = (h + d.y).max(min_h).min(max).min(bottom - y);
        }
        if west {
            w = (w - d.x).max(min_w).min(max).min(r.at.x + r.size.width - area.origin.x);
            x = r.at.x + r.size.width - w;
        }
        if north {
            h = (h - d.y).max(min_h).min(max).min(r.at.y + r.size.height - area.origin.y);
            y = r.at.y + r.size.height - h;
        }
        let tokens = (f32::from(w) / t.token_scale(), f32::from(h) / t.token_scale());
        if self.prefs.mini_size != Some(tokens) || self.mini.at != point(x, y) {
            self.prefs.mini_size = Some(tokens);
            self.mini.at = point(x, y);
            window.refresh();
        }
    }

    /// Shrinks the surface back around the card when the grip is released.
    pub fn mini_resize_end(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.mini.resize.take().is_some() {
            self.prefs.save();
            self.mini.region = None;
            self.resize_mini_surface(window);
            self.apply_mini_margin(window);
            self.remember_mini_position(cx);
            if std::env::var_os("NAVIA_MINI_LOG").is_some() {
                let (w, h) = self.mini_tokens();
                eprintln!("navia: mini resized to {w:.0}x{h:.0} tokens ({}) at {:.0},{:.0}", if self.mini_stacked() { "stacked" } else { "compact" }, f32::from(self.mini.at.x), f32::from(self.mini.at.y));
            }
            cx.notify();
        }
    }

    /// Makes the card follow a normal window the desktop resized, within its limits.
    pub fn mini_follow_window(&mut self, card: Size<Pixels>) {
        let t = self.theme;
        let tokens = ((f32::from(card.width) / t.token_scale()).clamp(MINI_W, MINI_MAX).round(), (f32::from(card.height) / t.token_scale()).clamp(MINI_H, MINI_MAX).round());
        if self.prefs.mini_size != Some(tokens) && self.mini.surface.is_some() {
            self.prefs.mini_size = Some(tokens);
            self.prefs.save();
        }
    }

    /// Gives the window the size it needs now, for the card, its room and the hint.
    pub fn resize_mini_surface(&mut self, window: &mut Window) {
        let want = self.mini_surface_size();
        if self.mini.surface != Some(want) {
            self.mini.surface = Some(want);
            window.resize(want);
        }
    }

    /// Sends the card's position to the compositor as the surface's margins, on layer-shell.
    fn apply_mini_margin(&mut self, window: &mut Window) {
        let room = self.mini_room();
        let top = self.mini.at.y - room;
        let left = self.mini.at.x - room;
        let key = (f32::from(top).round() as i32, f32::from(left).round() as i32);
        if self.mini.sent != Some(key) {
            self.mini.sent = Some(key);
            window.set_layer_margin(top, px(0.0), px(0.0), left);
        }
    }

    /// A press on an empty spot of the card. Layer-shell starts Navia's own drag, and a normal
    /// window asks the desktop to move it.
    pub fn mini_press(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        match self.mini_kind() {
            MiniKind::Layer => {
                self.mini.grab = Some(position);
                cx.notify();
            }
            MiniKind::Window => window.start_window_move(),
            MiniKind::Topmost => {}
        }
    }

    /// Moves the held card with the pointer, inside the work area. Positions are relative to the
    /// window, which moves with the card, so the offset from the grab point is the move.
    pub fn mini_drag_move(&mut self, position: Point<Pixels>, window: &mut Window) {
        let Some(grab) = self.mini.grab else { return };
        let at = self.clamp_mini(self.mini.at + (position - grab));
        if at != self.mini.at {
            self.mini.at = at;
            self.apply_mini_margin(window);
        }
    }

    pub fn mini_drag_end(&mut self, cx: &mut Context<Self>) {
        if self.mini.grab.take().is_some() {
            self.remember_mini_position(cx);
            if std::env::var_os("NAVIA_MINI_LOG").is_some() {
                eprintln!("navia: mini dropped at {:.0},{:.0}", f32::from(self.mini.at.x), f32::from(self.mini.at.y));
            }
            cx.notify();
        }
    }

    /// Brings the full window back on a page after a link in the mini player was followed.
    pub fn mini_follow_link(&mut self, cx: &mut Context<Self>) {
        if self.mini.window.is_some() {
            self.close_mini_player(true, cx);
        }
    }

    /// Ctrl+Alt+M, outside text fields.
    pub fn is_mini_shortcut(keystroke: &gpui::Keystroke) -> bool {
        let m = keystroke.modifiers;
        keystroke.key == "m" && m.control && m.alt && !m.shift && !m.platform
    }

    /// The shortcut as the menu and tooltip show it.
    pub fn mini_shortcut_label() -> &'static str {
        if cfg!(target_os = "macos") { "⌥⌘M" } else { "Ctrl+Alt+M" }
    }

    pub fn dismiss_mini_hint(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prefs.mini_hint_seen = true;
        self.prefs.save();
        self.resize_mini_surface(window);
        cx.notify();
    }
}

/// Windows 11 rounds ordinary windows but not pop-ups unless asked, so this asks DWM for
/// rounded corners and a border. Older Windows ignores it.
#[cfg(target_os = "windows")]
fn round_window_corners(window: &Window) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::Graphics::Dwm::{DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute};
    let Ok(handle) = HasWindowHandle::window_handle(window) else { return };
    let RawWindowHandle::Win32(h) = handle.as_raw() else { return };
    let preference = DWMWCP_ROUND;
    unsafe {
        DwmSetWindowAttribute(h.hwnd.get() as _, DWMWA_WINDOW_CORNER_PREFERENCE as u32, &preference as *const _ as *const _, std::mem::size_of_val(&preference) as u32);
    }
}
