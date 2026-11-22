//! The tray icon, with Play/Pause, Next, Previous, Show Navia and Quit. A click shows Navia.
//! Linux uses a StatusNotifierItem via `ksni`, Windows the notification area via `tray-icon`.
//! Commands reach the shell through a channel.

use futures::channel::mpsc::UnboundedSender;

use crate::i18n::tr;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrayCommand {
    PlayPause,
    Next,
    Previous,
    Show,
    Quit,
    /// The Windows taskbar switched between light and dark.
    Restyle,
}

/// The menu state. Pause while playing, and playback items only with a song.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct TrayState {
    pub playing: bool,
    pub has_song: bool,
}

/// `None` is a separator.
pub fn menu_items(state: TrayState) -> [Option<(TrayCommand, &'static str, bool)>; 6] {
    [
        Some((TrayCommand::PlayPause, if state.playing { tr("Pause") } else { tr("Play") }, state.has_song)),
        Some((TrayCommand::Next, tr("Next"), state.has_song)),
        Some((TrayCommand::Previous, tr("Previous"), state.has_song)),
        None,
        Some((TrayCommand::Show, tr("Show Navia"), true)),
        Some((TrayCommand::Quit, tr("Quit"), true)),
    ]
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use ksni::blocking::TrayMethods;
    use ksni::menu::{MenuItem, StandardItem};

    struct Item {
        state: TrayState,
        tx: UnboundedSender<TrayCommand>,
        /// for hosts that look the name up there.
        theme_path: String,
        /// Picks the pixmap fallback. The white mark on a dark panel, the black one on a light panel.
        dark_panel: bool,
        white: Vec<ksni::Icon>,
        black: Vec<ksni::Icon>,
    }

    /// The symbolic icon's name, the app ID plus ".tray", which Plasma recolors for the panel. There's
    /// no dash before "-symbolic" because icon loaders retry a missing name with its last dash part
    /// dropped, which would load the app icon instead.
    const ICON_NAME: &str = "com.davekai.Navia.tray-symbolic";
    const SYMBOLIC: &[u8] = include_bytes!("../assets/tray/com.davekai.Navia.tray-symbolic.svg");
    /// so KDE's icon loader accepts the folder as a theme.
    const INDEX_THEME: &[u8] = b"[Icon Theme]\nName=hicolor\nDirectories=scalable/apps\n\n[scalable/apps]\nSize=64\nMinSize=8\nMaxSize=512\nType=Scalable\n";

    /// Writes the symbolic icon loose in a folder for GTK and in a hicolor layout for KDE.
    /// Returns the folder, or an empty string when it can't be written.
    fn write_theme_path() -> String {
        let Some(dirs) = directories::ProjectDirs::from("", "", "navia") else { return String::new() };
        let root = dirs.cache_dir().join("tray-icons");
        let apps = root.join("hicolor/scalable/apps");
        let file = format!("{ICON_NAME}.svg");
        let same = |path: &std::path::Path, data: &[u8]| std::fs::read(path).is_ok_and(|d| d == data) || std::fs::write(path, data).is_ok();
        let written = std::fs::create_dir_all(&apps).is_ok()
            && [root.join(&file), apps.join(&file)].iter().all(|path| same(path, SYMBOLIC))
            && same(&root.join("hicolor/index.theme"), INDEX_THEME);
        if written { root.to_string_lossy().into_owned() } else { String::new() }
    }

    impl ksni::Tray for Item {
        fn id(&self) -> String {
            crate::autostart::app_name().to_string()
        }
        fn title(&self) -> String {
            navia_core::client::APP_DISPLAY_NAME.to_string()
        }
        fn icon_theme_path(&self) -> String {
            self.theme_path.clone()
        }
        fn icon_name(&self) -> String {
            if self.theme_path.is_empty() { String::new() } else { ICON_NAME.to_string() }
        }
        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            if self.dark_panel { self.white.clone() } else { self.black.clone() }
        }
        fn activate(&mut self, _x: i32, _y: i32) {
            let _ = self.tx.unbounded_send(TrayCommand::Show);
        }
        fn menu(&self) -> Vec<MenuItem<Self>> {
            menu_items(self.state)
                .into_iter()
                .map(|item| match item {
                    None => MenuItem::Separator,
                    Some((command, label, enabled)) => StandardItem {
                        label: label.replace('_', "__"),
                        enabled,
                        activate: Box::new(move |this: &mut Self| {
                            let _ = this.tx.unbounded_send(command);
                        }),
                        ..Default::default()
                    }
                    .into(),
                })
                .collect()
        }
    }

    /// ARGB32 in network byte order.
    fn icons(pngs: [&[u8]; 4]) -> Vec<ksni::Icon> {
        pngs.iter()
            .filter_map(|png| image::load_from_memory(png).ok())
            .map(|img| {
                let rgba = img.to_rgba8();
                let (width, height) = rgba.dimensions();
                let data = rgba.pixels().flat_map(|p| [p[3], p[0], p[1], p[2]]).collect();
                ksni::Icon { width: width as i32, height: height as i32, data }
            })
            .collect()
    }

    pub struct AppTray {
        handle: ksni::blocking::Handle<Item>,
        state: TrayState,
    }

    impl AppTray {
        /// Starts even with no tray host yet, so the icon appears once one does. A login session's
        /// panel can start after Navia.
        pub fn start(tx: UnboundedSender<TrayCommand>) -> Option<Self> {
            let white = icons([
                include_bytes!("../assets/tray/mark-white-22.png"),
                include_bytes!("../assets/tray/mark-white-32.png"),
                include_bytes!("../assets/tray/mark-white-48.png"),
                include_bytes!("../assets/tray/mark-white-64.png"),
            ]);
            let black = icons([
                include_bytes!("../assets/tray/mark-black-22.png"),
                include_bytes!("../assets/tray/mark-black-32.png"),
                include_bytes!("../assets/tray/mark-black-48.png"),
                include_bytes!("../assets/tray/mark-black-64.png"),
            ]);
            // Panels are dark unless the desktop prefers light.
            let item = Item { state: TrayState::default(), tx, theme_path: write_theme_path(), dark_panel: true, white, black };
            // a Flatpak can't own the item's usual name, so it registers under its connection's own name.
            match item.assume_sni_available(true).disable_dbus_name(crate::persist::in_flatpak()).spawn() {
                Ok(handle) => {
                    // The desktop's color scheme picks the pixmap, followed live. 1 prefers dark, 2 light.
                    let watched = handle.clone();
                    crate::chrome::watch_portal_setting("navia-tray-scheme", "org.freedesktop.appearance", "color-scheme", move |value| {
                        if let Ok(scheme) = u32::try_from(value) {
                            watched.update(|item| item.dark_panel = scheme != 2);
                        }
                    });
                    Some(Self { handle, state: TrayState::default() })
                }
                Err(err) => {
                    eprintln!("navia: no tray icon: {err}");
                    None
                }
            }
        }

        pub fn set_state(&mut self, state: TrayState) {
            if state != self.state {
                self.state = state;
                self.handle.update(|item| item.state = state);
            }
        }

        /// Updates the labels after a language change.
        pub fn refresh(&self) {
            self.handle.update(|_| {});
        }

        /// Linux follows the color scheme on its own through the portal watch in `start`.
        pub fn restyle(&mut self) {}
    }

    /// Whether a tray host is showing items, meaning the StatusNotifierWatcher has a host
    /// registered. Without one, closing the window quits instead of hiding it.
    pub fn host_available() -> bool {
        let Ok(conn) = zbus::blocking::Connection::session() else { return false };
        let Ok(proxy) = zbus::blocking::Proxy::new(&conn, "org.kde.StatusNotifierWatcher", "/StatusNotifierWatcher", "org.kde.StatusNotifierWatcher") else { return false };
        proxy.get_property::<bool>("IsStatusNotifierHostRegistered").unwrap_or(false)
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
    use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

    pub struct AppTray {
        icon: TrayIcon,
        items: Vec<(TrayCommand, MenuItem)>,
        state: TrayState,
        /// The taskbar is light, so the mark is black.
        light: bool,
    }

    /// The mark for the notification area, white on a dark taskbar and black on a light one.
    fn mark_icon(light: bool) -> Option<tray_icon::Icon> {
        let png: &[u8] = if light { include_bytes!("../assets/tray/mark-black-32.png") } else { include_bytes!("../assets/tray/mark-white-32.png") };
        let rgba = image::load_from_memory(png).ok()?.to_rgba8();
        let (width, height) = rgba.dimensions();
        tray_icon::Icon::from_rgba(rgba.into_raw(), width, height).ok()
    }

    const PERSONALIZE: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Whether the taskbar is light, from `SystemUsesLightTheme`. Apps use a different key. Dark when unset.
    fn light_taskbar() -> bool {
        use windows_sys::Win32::Foundation::ERROR_SUCCESS;
        use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
        let (mut value, mut size) = (0u32, 4u32);
        let read = unsafe {
            RegGetValueW(HKEY_CURRENT_USER, wide(PERSONALIZE).as_ptr(), wide("SystemUsesLightTheme").as_ptr(), RRF_RT_REG_DWORD, std::ptr::null_mut(), (&mut value as *mut u32).cast(), &mut size)
        };
        read == ERROR_SUCCESS && value == 1
    }

    /// Watches the theme key so the icon follows the taskbar live.
    fn watch_taskbar_theme(tx: UnboundedSender<TrayCommand>) {
        use windows_sys::Win32::Foundation::ERROR_SUCCESS;
        use windows_sys::Win32::System::Registry::{HKEY, HKEY_CURRENT_USER, KEY_NOTIFY, KEY_QUERY_VALUE, REG_NOTIFY_CHANGE_LAST_SET, RegCloseKey, RegNotifyChangeKeyValue, RegOpenKeyExW};
        let _ = std::thread::Builder::new().name("navia-taskbar-theme".into()).spawn(move || {
            let mut key: HKEY = std::ptr::null_mut();
            if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, wide(PERSONALIZE).as_ptr(), 0, KEY_NOTIFY | KEY_QUERY_VALUE, &mut key) } != ERROR_SUCCESS {
                return;
            }
            // Blocks until a value under the key changes.
            while unsafe { RegNotifyChangeKeyValue(key, 0, REG_NOTIFY_CHANGE_LAST_SET, std::ptr::null_mut(), 0) } == ERROR_SUCCESS {
                if tx.unbounded_send(TrayCommand::Restyle).is_err() {
                    break;
                }
            }
            unsafe { RegCloseKey(key) };
        });
    }

    impl AppTray {
        /// Created on the main thread, which runs the message loop the icon needs.
        pub fn start(tx: UnboundedSender<TrayCommand>) -> Option<Self> {
            let menu = Menu::new();
            let mut items = Vec::new();
            for item in menu_items(TrayState::default()) {
                match item {
                    None => menu.append(&PredefinedMenuItem::separator()).ok()?,
                    Some((command, label, enabled)) => {
                        let entry = MenuItem::new(label, enabled, None);
                        menu.append(&entry).ok()?;
                        items.push((command, entry));
                    }
                }
            }
            let ids: Vec<(MenuId, TrayCommand)> = items.iter().map(|(c, i)| (i.id().clone(), *c)).collect();
            let menu_tx = tx.clone();
            let tx_theme = tx.clone();
            MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                if let Some((_, command)) = ids.iter().find(|(id, _)| *id == event.id) {
                    let _ = menu_tx.unbounded_send(*command);
                }
            }));
            TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
                if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                    let _ = tx.unbounded_send(TrayCommand::Show);
                }
            }));
            let light = light_taskbar();
            let icon = mark_icon(light)?;
            watch_taskbar_theme(tx_theme);
            let built = TrayIconBuilder::new().with_menu(Box::new(menu)).with_tooltip(navia_core::client::APP_DISPLAY_NAME).with_icon(icon).with_menu_on_left_click(false).build();
            match built {
                Ok(icon) => Some(Self { icon, items, state: TrayState::default(), light }),
                Err(err) => {
                    eprintln!("navia: no tray icon: {err}");
                    None
                }
            }
        }

        pub fn set_state(&mut self, state: TrayState) {
            if state != self.state {
                self.state = state;
                self.refresh();
            }
        }

        /// Switches the mark between white and black after a taskbar theme change.
        pub fn restyle(&mut self) {
            let light = light_taskbar();
            if light != self.light {
                self.light = light;
                let _ = self.icon.set_icon(mark_icon(light));
            }
        }

        pub fn refresh(&self) {
            let labels = menu_items(self.state);
            for (command, item) in &self.items {
                if let Some((_, label, enabled)) = labels.iter().flatten().find(|(c, _, _)| c == command) {
                    item.set_text(label);
                    item.set_enabled(*enabled);
                }
            }
        }
    }

    /// The notification area is always there on Windows.
    pub fn host_available() -> bool {
        true
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod imp {
    use super::*;
    pub struct AppTray;
    impl AppTray {
        pub fn start(_: UnboundedSender<TrayCommand>) -> Option<Self> {
            None
        }
        pub fn set_state(&mut self, _: TrayState) {}
        pub fn refresh(&self) {}
        pub fn restyle(&mut self) {}
    }
    pub fn host_available() -> bool {
        false
    }
}

pub use imp::{AppTray, host_available};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn play_pause_follows_state() {
        let idle = menu_items(TrayState::default());
        assert_eq!(idle[0], Some((TrayCommand::PlayPause, "Play", false)));
        let playing = menu_items(TrayState { playing: true, has_song: true });
        assert_eq!(playing[0], Some((TrayCommand::PlayPause, "Pause", true)));
        assert_eq!(playing[1], Some((TrayCommand::Next, "Next", true)));
        assert!(playing[3].is_none());
        assert_eq!(playing[4].map(|i| i.0), Some(TrayCommand::Show));
        assert_eq!(playing[5].map(|i| (i.0, i.2)), Some((TrayCommand::Quit, true)));
    }
}
