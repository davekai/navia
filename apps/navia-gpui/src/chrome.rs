//! What the platform offers the window, namely its translucency and which buttons to draw.

use crate::i18n::{tr, n_};
use gpui::{App, WindowBackgroundAppearance, WindowButton, WindowButtonLayout};

/// The compositor effect to request when translucency is on, one small function per platform.
#[cfg(target_os = "windows")]
fn translucent_appearance() -> WindowBackgroundAppearance {
    WindowBackgroundAppearance::MicaBackdrop
}

#[cfg(not(target_os = "windows"))]
fn translucent_appearance() -> WindowBackgroundAppearance {
    WindowBackgroundAppearance::Blurred
}

pub fn window_background(translucent: bool) -> WindowBackgroundAppearance {
    if translucent { translucent_appearance() } else { WindowBackgroundAppearance::Opaque }
}

/// Linux is Wayland-only. Outside a working Wayland session this says so and exits before GPUI
/// can start headless or panic on the connection.
#[cfg(target_os = "linux")]
pub fn require_wayland_session() {
    const MESSAGE: &str = n_("Navia needs a Wayland session and can't start without one. X11 isn't supported.");
    if !wayland::session_reachable() {
        eprintln!("{MESSAGE}");
        wayland::notify(tr(MESSAGE));
        std::process::exit(1);
    }
}

#[cfg(not(target_os = "linux"))]
pub fn require_wayland_session() {}

/// What the desktop can do for the window, checked once at launch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Support {
    /// The desktop blurs behind translucent windows, via `ext-background-effect-v1` or KDE's blur on
    /// Wayland, or Mica on Windows. Translucency defaults on only then. A failed probe counts as no blur.
    pub blur: bool,
    /// The desktop can draw the window frame and title bar, via `xdg-decoration`. Most Wayland
    /// desktops support it, but GNOME doesn't.
    pub system_frame: bool,
    /// The desktop has `wlr-layer-shell`, so the mini player can be an overlay above every window
    /// that Navia positions itself. GNOME lacks it.
    pub layer_shell: bool,
}

/// Test overrides. `NAVIA_NO_SYSTEM_FRAME=1`, `NAVIA_NO_LAYER_SHELL=1` and `NAVIA_NO_BLUR=1`
/// each act as a desktop without that feature.
#[cfg(target_os = "linux")]
pub fn support() -> Support {
    let found = wayland::probe_desktop();
    Support {
        blur: found.blur && std::env::var_os("NAVIA_NO_BLUR").is_none(),
        system_frame: found.decoration && std::env::var_os("NAVIA_NO_SYSTEM_FRAME").is_none(),
        layer_shell: found.layer_shell && std::env::var_os("NAVIA_NO_LAYER_SHELL").is_none(),
    }
}

/// Mica needs Windows 11 22H2, and older builds silently ignore the request. Windows always
/// uses Navia's own frame.
#[cfg(target_os = "windows")]
pub fn support() -> Support {
    Support { blur: windows_build() >= 22621, system_frame: false, layer_shell: false }
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub fn support() -> Support {
    Support::default()
}

/// The frame to request. Navia's own, unless the user chose the desktop's title bar and the
/// desktop has one. The compositor can still refuse, so `window.window_decorations()` decides.
pub fn decorations(support: Support, system_title_bar: bool) -> gpui::WindowDecorations {
    if support.system_frame && system_title_bar { gpui::WindowDecorations::Server } else { gpui::WindowDecorations::Client }
}

/// What a double-click on Navia's title areas does. GNOME makes it configurable, everywhere
/// else it maximizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DoubleClick {
    Maximize,
    Minimize,
    Menu,
    Nothing,
}

impl DoubleClick {
    /// GNOME's `action-double-click-titlebar`. Actions Wayland can't do, like lower, count as none.
    /// Maximizing in one direction isn't possible either, so it maximizes fully.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fn from_gnome(value: &str) -> Self {
        match value {
            "minimize" => DoubleClick::Minimize,
            "menu" => DoubleClick::Menu,
            "none" | "lower" => DoubleClick::Nothing,
            _ => DoubleClick::Maximize,
        }
    }
}

static DOUBLE_CLICK: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub fn double_click() -> DoubleClick {
    match DOUBLE_CLICK.load(std::sync::atomic::Ordering::Relaxed) {
        1 => DoubleClick::Minimize,
        2 => DoubleClick::Menu,
        3 => DoubleClick::Nothing,
        _ => DoubleClick::Maximize,
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn set_double_click(action: DoubleClick) {
    let code = match action {
        DoubleClick::Maximize => 0,
        DoubleClick::Minimize => 1,
        DoubleClick::Menu => 2,
        DoubleClick::Nothing => 3,
    };
    DOUBLE_CLICK.store(code, std::sync::atomic::Ordering::Relaxed);
}

/// Reads one setting through the settings portal on a background thread and follows its changes.
/// `apply` gets the value now and after every change, and is never called without a portal or key.
#[cfg(target_os = "linux")]
pub fn watch_portal_setting(thread: &str, namespace: &'static str, key: &'static str, apply: impl Fn(zbus::zvariant::OwnedValue) + Send + 'static) {
    let _ = std::thread::Builder::new().name(thread.into()).spawn(move || {
        let Ok(connection) = zbus::blocking::Connection::session() else { return };
        let Ok(proxy) = zbus::blocking::Proxy::new(&connection, "org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", "org.freedesktop.portal.Settings") else { return };
        // Subscribe before reading, so a change in between isn't lost.
        let changes = proxy.receive_signal("SettingChanged");
        let read = proxy
            .call::<_, _, zbus::zvariant::OwnedValue>("ReadOne", &(namespace, key))
            .ok()
            // Portals before version 2 only have Read, which wraps the value once more.
            .or_else(|| {
                let value: zbus::zvariant::OwnedValue = proxy.call("Read", &(namespace, key)).ok()?;
                zbus::zvariant::Value::try_from(value).ok()?.downcast().ok()
            });
        if let Some(value) = read {
            apply(value);
        }
        let Ok(changes) = changes else { return };
        for message in changes {
            let Ok((changed_namespace, changed_key, value)) = message.body().deserialize::<(String, String, zbus::zvariant::OwnedValue)>() else { continue };
            if changed_namespace == namespace && changed_key == key {
                apply(value);
            }
        }
    });
}

/// GNOME's title bar double-click setting. Stays maximize without a portal or key.
#[cfg(target_os = "linux")]
pub fn watch_double_click() {
    watch_portal_setting("navia-titlebar-setting", "org.gnome.desktop.wm.preferences", "action-double-click-titlebar", |value| {
        if let Ok(value) = String::try_from(value) {
            set_double_click(DoubleClick::from_gnome(&value));
        }
    });
}

#[cfg(not(target_os = "linux"))]
pub fn watch_double_click() {}

/// Reduce Motion from the desktop's `enable-animations`, which KDE's portal also maps to,
/// followed live into `motion::set_reduced`. Motion stays on without a portal or key.
#[cfg(target_os = "linux")]
pub fn watch_reduce_motion(cx: &mut App) {
    use futures::StreamExt;
    let (tx, mut rx) = futures::channel::mpsc::unbounded::<bool>();
    watch_portal_setting("navia-motion-setting", "org.gnome.desktop.interface", "enable-animations", move |value| {
        if let Ok(enabled) = bool::try_from(value) {
            let _ = tx.unbounded_send(!enabled);
        }
    });
    cx.spawn(async move |cx| {
        while let Some(reduced) = rx.next().await {
            cx.update(|cx| crate::theme::motion::set_reduced(cx, reduced));
        }
    })
    .detach();
}

#[cfg(not(target_os = "linux"))]
pub fn watch_reduce_motion(_cx: &mut App) {}

#[cfg(target_os = "windows")]
fn windows_build() -> u32 {
    #[repr(C)]
    struct OsVersionInfoW {
        size: u32,
        major: u32,
        minor: u32,
        build: u32,
        platform: u32,
        csd: [u16; 128],
    }
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn RtlGetVersion(info: *mut OsVersionInfoW) -> i32;
    }
    let mut info = OsVersionInfoW { size: std::mem::size_of::<OsVersionInfoW>() as u32, major: 0, minor: 0, build: 0, platform: 0, csd: [0; 128] };
    // RtlGetVersion reports the real build, where GetVersionEx lies to unmanifested apps.
    if unsafe { RtlGetVersion(&mut info) } == 0 { info.build } else { 0 }
}

#[cfg(target_os = "linux")]
mod wayland {
    use std::ffi::OsString;
    use std::io::{ErrorKind, Read, Write};
    use std::os::unix::net::UnixStream;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    const EXT_BLUR: &str = "ext_background_effect_manager_v1";
    const KDE_BLUR: &str = "org_kde_kwin_blur_manager";
    const DECORATION: &str = "zxdg_decoration_manager_v1";
    const LAYER_SHELL: &str = "zwlr_layer_shell_v1";

    /// What the compositor advertises.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Found {
        pub blur: bool,
        pub decoration: bool,
        pub layer_shell: bool,
    }

    /// WAYLAND_DISPLAY as GPUI reads it. Unset and empty both mean no session.
    fn display() -> Option<OsString> {
        std::env::var_os("WAYLAND_DISPLAY").filter(|display| !display.is_empty())
    }

    fn connect() -> Option<UnixStream> {
        let path = PathBuf::from(display()?);
        let path = if path.is_absolute() { path } else { PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?).join(path) };
        UnixStream::connect(path).ok()
    }

    pub fn session_reachable() -> bool {
        if display().is_none() {
            return false;
        }
        // The launcher already opened the connection and GPUI takes it over, so there's nothing to test.
        if std::env::var_os("WAYLAND_SOCKET").is_some() {
            return true;
        }
        connect().is_some_and(|stream| {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            answers(stream)
        })
    }

    /// false only when the compositor hangs up; a slow one still counts, so a busy desktop at login isn't mistaken for none.
    fn answers(mut stream: UnixStream) -> bool {
        if request(&mut stream, 1, 0, &[2]).is_err() {
            return false; // wl_display.sync -> wl_callback#2
        }
        match stream.read(&mut [0u8; 1]) {
            Ok(n) => n > 0,
            Err(e) => matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted),
        }
    }

    /// Shows a desktop notification, since someone launching from a menu sees no terminal. Waits
    /// briefly so a launcher that cleans up can't kill notify-send. Does nothing if it's missing.
    pub fn notify(message: &str) {
        let Ok(mut child) = Command::new("notify-send")
            .args(["--app-name=Navia", "--icon=navia", "Navia", message])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            return;
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if !matches!(child.try_wait(), Ok(None)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    pub fn probe_desktop() -> Found {
        connect()
            .and_then(|stream| {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
                probe(stream)
            })
            .unwrap_or_default()
    }

    /// A minimal Wayland client. Lists the globals and, for the ext protocol, asks whether blur is on.
    pub(super) fn probe(mut stream: UnixStream) -> Option<Found> {
        request(&mut stream, 1, 1, &[2]).ok()?; // wl_display.get_registry -> wl_registry#2
        request(&mut stream, 1, 0, &[3]).ok()?; // wl_display.sync -> wl_callback#3
        let mut reader = Reader { stream, buffer: Vec::new() };
        let mut ext = None;
        let mut kde = false;
        let mut decoration = false;
        let mut layer_shell = false;
        loop {
            let (object, opcode, args) = reader.next()?;
            match (object, opcode) {
                (1, 0) => return None, // wl_display.error
                (2, 0) => match global_interface(&args)? {
                    (name, interface) if interface == EXT_BLUR => ext = Some(name),
                    (_, interface) if interface == KDE_BLUR => kde = true,
                    (_, interface) if interface == DECORATION => decoration = true,
                    (_, interface) if interface == LAYER_SHELL => layer_shell = true,
                    _ => {}
                },
                (3, 0) => break,
                _ => {}
            }
        }
        let Some(name) = ext else { return Some(Found { blur: kde, decoration, layer_shell }) };

        // The global stays advertised with blur off, and only its capabilities tell.
        let mut bind = name.to_ne_bytes().to_vec();
        push_string(&mut bind, EXT_BLUR);
        bind.extend_from_slice(&1u32.to_ne_bytes());
        bind.extend_from_slice(&4u32.to_ne_bytes());
        send(&mut reader.stream, 2, 0, &bind).ok()?; // wl_registry.bind -> manager#4
        request(&mut reader.stream, 1, 0, &[5]).ok()?; // wl_display.sync -> wl_callback#5
        let mut blur = false;
        loop {
            let (object, opcode, args) = reader.next()?;
            match (object, opcode) {
                (1, 0) => return None,
                (4, 0) => blur = u32::from_ne_bytes(args.get(..4)?.try_into().ok()?) & 1 != 0,
                (5, 0) => return Some(Found { blur, decoration, layer_shell }),
                _ => {}
            }
        }
    }

    fn request(stream: &mut UnixStream, object: u32, opcode: u16, args: &[u32]) -> std::io::Result<()> {
        let body: Vec<u8> = args.iter().flat_map(|a| a.to_ne_bytes()).collect();
        send(stream, object, opcode, &body)
    }

    fn send(stream: &mut UnixStream, object: u32, opcode: u16, body: &[u8]) -> std::io::Result<()> {
        let mut message = object.to_ne_bytes().to_vec();
        message.extend_from_slice(&(((8 + body.len() as u32) << 16) | u32::from(opcode)).to_ne_bytes());
        message.extend_from_slice(body);
        stream.write_all(&message)
    }

    fn push_string(out: &mut Vec<u8>, s: &str) {
        let with_nul = s.len() as u32 + 1;
        out.extend_from_slice(&with_nul.to_ne_bytes());
        out.extend_from_slice(s.as_bytes());
        out.push(0);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }

    /// Parses wl_registry.global into name, interface and version.
    fn global_interface(args: &[u8]) -> Option<(u32, String)> {
        let name = u32::from_ne_bytes(args.get(..4)?.try_into().ok()?);
        let len = u32::from_ne_bytes(args.get(4..8)?.try_into().ok()?) as usize;
        let bytes = args.get(8..8 + len.checked_sub(1)?)?;
        Some((name, String::from_utf8_lossy(bytes).into_owned()))
    }

    struct Reader {
        stream: UnixStream,
        buffer: Vec<u8>,
    }

    impl Reader {
        fn next(&mut self) -> Option<(u32, u16, Vec<u8>)> {
            loop {
                if self.buffer.len() >= 8 {
                    let object = u32::from_ne_bytes(self.buffer[..4].try_into().ok()?);
                    let word = u32::from_ne_bytes(self.buffer[4..8].try_into().ok()?);
                    let size = (word >> 16) as usize;
                    if size < 8 {
                        return None;
                    }
                    if self.buffer.len() >= size {
                        let args = self.buffer[8..size].to_vec();
                        self.buffer.drain(..size);
                        return Some((object, word as u16, args));
                    }
                }
                let mut chunk = [0u8; 4096];
                let n = self.stream.read(&mut chunk).ok()?;
                if n == 0 {
                    return None;
                }
                self.buffer.extend_from_slice(&chunk[..n]);
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn global(name: u32, interface: &str) -> Vec<u8> {
            let mut body = name.to_ne_bytes().to_vec();
            push_string(&mut body, interface);
            body.extend_from_slice(&1u32.to_ne_bytes());
            event(2, 0, &body)
        }

        fn event(object: u32, opcode: u16, body: &[u8]) -> Vec<u8> {
            let mut m = object.to_ne_bytes().to_vec();
            m.extend_from_slice(&(((8 + body.len() as u32) << 16) | u32::from(opcode)).to_ne_bytes());
            m.extend_from_slice(body);
            m
        }

        fn run(script: Vec<u8>) -> Option<Found> {
            let (client, mut compositor) = UnixStream::pair().unwrap();
            compositor.write_all(&script).unwrap();
            client.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
            // Kept open while probing, because the probe writes its requests to it.
            let result = probe(client);
            drop(compositor);
            result
        }

        fn blur(script: Vec<u8>) -> Option<bool> {
            run(script).map(|found| found.blur)
        }

        #[test]
        fn a_compositor_that_hangs_up_is_no_session() {
            let (client, compositor) = UnixStream::pair().unwrap();
            drop(compositor);
            assert!(!answers(client));
        }

        #[test]
        fn a_reply_or_a_slow_compositor_counts_as_a_session() {
            let (client, mut compositor) = UnixStream::pair().unwrap();
            compositor.write_all(&event(2, 0, &[0; 4])).unwrap();
            assert!(answers(client));

            let (client, _compositor) = UnixStream::pair().unwrap();
            client.set_read_timeout(Some(Duration::from_millis(10))).unwrap();
            assert!(answers(client));
        }

        #[test]
        fn neither_protocol_means_no_blur() {
            let mut s = global(1, "wl_compositor");
            s.extend(event(3, 0, &[0; 4]));
            assert_eq!(blur(s), Some(false));
        }

        #[test]
        fn kde_blur_alone_is_enough() {
            let mut s = global(1, KDE_BLUR);
            s.extend(event(3, 0, &[0; 4]));
            assert_eq!(blur(s), Some(true));
        }

        #[test]
        fn ext_protocol_follows_its_capabilities() {
            for (caps, want) in [(1u32, true), (0u32, false)] {
                let mut s = global(7, EXT_BLUR);
                s.extend(event(3, 0, &[0; 4]));
                s.extend(event(4, 0, &caps.to_ne_bytes()));
                s.extend(event(5, 0, &[0; 4]));
                assert_eq!(blur(s), Some(want));
            }
        }

        #[test]
        fn the_decoration_manager_means_a_system_frame() {
            let mut s = global(1, "wl_compositor");
            s.extend(event(3, 0, &[0; 4]));
            assert_eq!(run(s), Some(Found { blur: false, decoration: false, layer_shell: false }));

            // Found with either blur path, before or after the ext protocol's round trip.
            let mut s = global(1, DECORATION);
            s.extend(global(2, KDE_BLUR));
            s.extend(event(3, 0, &[0; 4]));
            assert_eq!(run(s), Some(Found { blur: true, decoration: true, layer_shell: false }));

            let mut s = global(1, DECORATION);
            s.extend(global(7, EXT_BLUR));
            s.extend(event(3, 0, &[0; 4]));
            s.extend(event(4, 0, &1u32.to_ne_bytes()));
            s.extend(event(5, 0, &[0; 4]));
            assert_eq!(run(s), Some(Found { blur: true, decoration: true, layer_shell: false }));
        }

        #[test]
        fn layer_shell_is_found_with_or_without_blur() {
            let mut s = global(1, LAYER_SHELL);
            s.extend(event(3, 0, &[0; 4]));
            assert_eq!(run(s), Some(Found { blur: false, decoration: false, layer_shell: true }));

            let mut s = global(1, LAYER_SHELL);
            s.extend(global(7, EXT_BLUR));
            s.extend(event(3, 0, &[0; 4]));
            s.extend(event(4, 0, &1u32.to_ne_bytes()));
            s.extend(event(5, 0, &[0; 4]));
            assert_eq!(run(s), Some(Found { blur: true, decoration: false, layer_shell: true }));
        }
    }
}

/// Window button style, drawn by Navia and picked from the desktop. No theme files are read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ButtonStyle {
    Breeze,
    Adwaita,
    Neutral,
}

impl ButtonStyle {
    /// Windows draws Navia's own round buttons, registered as caption buttons so Snap layouts still work.
    #[cfg(target_os = "windows")]
    pub fn detect() -> Self {
        ButtonStyle::Neutral
    }

    #[cfg(not(target_os = "windows"))]
    pub fn detect() -> Self {
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_ascii_uppercase();
        if desktop.contains("KDE") {
            ButtonStyle::Breeze
        } else if desktop.contains("GNOME") {
            ButtonStyle::Adwaita
        } else {
            ButtonStyle::Neutral
        }
    }
}

/// Minimize, maximize and close on the right, when the platform doesn't say otherwise.
fn default_layout() -> WindowButtonLayout {
    WindowButtonLayout { left: [None; 3], right: [Some(WindowButton::Minimize), Some(WindowButton::Maximize), Some(WindowButton::Close)] }
}

/// A GNOME-style layout string, for the debug override and the script driver.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
pub fn parse_layout(layout: &str) -> Option<WindowButtonLayout> {
    WindowButtonLayout::parse(layout).ok()
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
pub fn parse_layout(_layout: &str) -> Option<WindowButtonLayout> {
    None
}

/// `NAVIA_BUTTON_LAYOUT` forces a layout for testing, like `close,minimize:maximize`.
pub fn forced_layout() -> Option<WindowButtonLayout> {
    std::env::var("NAVIA_BUTTON_LAYOUT").ok().and_then(|v| parse_layout(&v))
}

/// The desktop's layout. On Linux GPUI reads it from the settings portal, falling back to the
/// same right-hand default.
pub fn current_layout(cx: &App) -> WindowButtonLayout {
    forced_layout().or_else(|| cx.button_layout()).unwrap_or_else(default_layout)
}


/// The window's edge on Linux with client-side decorations. A shadow margin on every untiled
/// side, and rounded corners where both neighbors are untiled.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Frame {
    pub top: gpui::Pixels,
    pub right: gpui::Pixels,
    pub bottom: gpui::Pixels,
    pub left: gpui::Pixels,
    pub corners: gpui::Corners<gpui::Pixels>,
    /// The value for `set_client_inset`.
    pub inset: gpui::Pixels,
    /// The window draws its own outline and shadow.
    pub drawn: bool,
    pub tiling: gpui::Tiling,
}

/// The margin a new window needs around its visible size.
pub fn shadow_margin() -> f32 {
    if cfg!(target_os = "linux") { crate::theme::size::FRAME_SHADOW_MARGIN } else { 0.0 }
}

impl Frame {
    /// `tiling` is `None` with server-side decorations or where the OS draws the frame.
    pub fn new(tiling: Option<gpui::Tiling>, maximized: bool, fullscreen: bool, radius: gpui::Pixels) -> Self {
        let Some(tiling) = tiling.filter(|_| cfg!(target_os = "linux")) else { return Self::default() };
        let tiling = if maximized || fullscreen { gpui::Tiling { top: true, left: true, right: true, bottom: true } } else { tiling };
        let margin = gpui::px(crate::theme::size::FRAME_SHADOW_MARGIN);
        let side = |tiled: bool| if tiled { gpui::px(0.0) } else { margin };
        let corner = |a: bool, b: bool| if a || b { gpui::px(0.0) } else { radius };
        Self {
            top: side(tiling.top),
            right: side(tiling.right),
            bottom: side(tiling.bottom),
            left: side(tiling.left),
            corners: gpui::Corners {
                top_left: corner(tiling.top, tiling.left),
                top_right: corner(tiling.top, tiling.right),
                bottom_left: corner(tiling.bottom, tiling.left),
                bottom_right: corner(tiling.bottom, tiling.right),
            },
            inset: if fullscreen { gpui::px(0.0) } else { margin },
            drawn: !(tiling.top && tiling.right && tiling.bottom && tiling.left),
            tiling,
        }
    }

    /// The part of the surface that takes the pointer, which is the window plus FRAME_GRIP_OUTSIDE
    /// of the shadow margin for resize grips. `None` means the whole surface.
    pub fn input_region(&self, surface: gpui::Size<gpui::Pixels>) -> Option<gpui::Bounds<gpui::Pixels>> {
        if [self.top, self.right, self.bottom, self.left].iter().all(|m| *m <= gpui::px(0.0)) {
            return None;
        }
        let grip = gpui::px(crate::theme::size::FRAME_GRIP_OUTSIDE);
        let out = |margin: gpui::Pixels| margin.min(grip);
        let origin = gpui::point(self.left - out(self.left), self.top - out(self.top));
        let end = gpui::point(surface.width - self.right + out(self.right), surface.height - self.bottom + out(self.bottom));
        Some(gpui::Bounds { origin, size: gpui::size(end.x - origin.x, end.y - origin.y) })
    }

    /// The window's visible area inside a surface of this size.
    pub fn content_size(&self, surface: gpui::Size<gpui::Pixels>) -> gpui::Size<gpui::Pixels> {
        gpui::size(surface.width - self.left - self.right, surface.height - self.top - self.bottom)
    }
}

/// Rounds an element to the window's corners, which can differ when it's tiled on one side.
pub fn round_corners<E: gpui::Styled>(element: E, corners: gpui::Corners<gpui::Pixels>) -> E {
    element.rounded_tl(corners.top_left).rounded_tr(corners.top_right).rounded_bl(corners.bottom_left).rounded_br(corners.bottom_right)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gnome_double_click_values() {
        assert_eq!(DoubleClick::from_gnome("toggle-maximize"), DoubleClick::Maximize);
        assert_eq!(DoubleClick::from_gnome("toggle-maximize-vertically"), DoubleClick::Maximize);
        assert_eq!(DoubleClick::from_gnome("minimize"), DoubleClick::Minimize);
        assert_eq!(DoubleClick::from_gnome("menu"), DoubleClick::Menu);
        assert_eq!(DoubleClick::from_gnome("none"), DoubleClick::Nothing);
        assert_eq!(DoubleClick::from_gnome("lower"), DoubleClick::Nothing);
    }

    #[test]
    fn the_system_frame_only_when_offered_and_chosen() {
        use gpui::WindowDecorations::{Client, Server};
        let offered = Support { blur: true, system_frame: true, layer_shell: false };
        assert_eq!(decorations(offered, false), Client);
        assert_eq!(decorations(offered, true), Server);
        let none = Support { blur: true, system_frame: false, layer_shell: false };
        assert_eq!(decorations(none, true), Client);
        assert_eq!(decorations(none, false), Client);
    }
}
