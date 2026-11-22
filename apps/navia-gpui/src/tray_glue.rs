//! The shell's side of the tray: closing to the tray or quitting, tray commands, and
//! bringing the window back.

use futures::StreamExt;
use futures::channel::mpsc::unbounded;
use gpui::{AppContext, Context, Window};
use navia_integrations::PlayerCommand;

use crate::autostart::OpenAtLogin;
use crate::shell::Shell;
use crate::tray::{AppTray, TrayCommand, TrayState};

/// Starts the tray icon and the task its commands arrive on. `NAVIA_NO_TRAY=1` skips it for tests.
pub fn start_tray(cx: &mut Context<Shell>) -> Option<AppTray> {
    if std::env::var_os("NAVIA_NO_TRAY").is_some_and(|v| v == "1") {
        return None;
    }
    let (tx, mut rx) = unbounded();
    let tray = AppTray::start(tx)?;
    cx.spawn(async move |this, cx| {
        while let Some(command) = rx.next().await {
            if this.update(cx, |shell, cx| shell.tray_command(command, cx)).is_err() {
                return;
            }
        }
    })
    .detach();
    Some(tray)
}

/// the global menu and the task its clicks arrive on, dispatched as the item's action in the main window.
#[cfg(target_os = "linux")]
pub fn start_app_menu(cx: &mut Context<Shell>) -> Option<crate::app_menu::AppMenu> {
    let (tx, mut rx) = unbounded();
    let menu = crate::app_menu::AppMenu::start(tx)?;
    cx.spawn(async move |this, cx| {
        while let Some(id) = rx.next().await {
            let target = this.update(cx, |shell, _| shell.app_menu.as_ref().and_then(|m| m.action(id)).zip(shell.main_window));
            let Ok(target) = target else { return };
            if let Some((action, window)) = target {
                window.update(cx, |_, window, cx| window.dispatch_action(action, cx)).ok();
            }
        }
    })
    .detach();
    Some(menu)
}

impl Shell {
    fn tray_command(&mut self, command: TrayCommand, cx: &mut Context<Self>) {
        match command {
            TrayCommand::PlayPause => self.on_control_command(PlayerCommand::PlayPause, cx),
            TrayCommand::Next => self.on_control_command(PlayerCommand::Next, cx),
            TrayCommand::Previous => self.on_control_command(PlayerCommand::Previous, cx),
            TrayCommand::Show => self.show_main_window(cx),
            TrayCommand::Quit => cx.quit(),
            TrayCommand::Restyle => {
                if let Some(tray) = self.tray.as_mut() {
                    tray.restyle();
                }
            }
        }
    }

    /// Updates the menu for playback. It shows Pause while playing and no playback items without a song.
    pub fn tray_sync(&mut self, playing: bool, has_song: bool) {
        if let Some(tray) = self.tray.as_mut() {
            tray.set_state(TrayState { playing, has_song });
        }
    }

    /// Whether closing the window keeps Navia in the tray. That needs the setting on, the icon up
    /// and a tray host to show it. Otherwise closing quits, so there is always a way back.
    pub fn closes_to_tray(&self) -> bool {
        self.prefs.close_to_tray && self.tray.is_some() && crate::tray::host_available()
    }

    /// Handles the main window closing. With the tray, the window goes and playback continues,
    /// otherwise Navia quits. Always returns true.
    pub fn main_window_closing(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.closes_to_tray() {
            cx.quit();
            return true;
        }
        self.closed_size = Some(window.bounds().size);
        self.main_window = None;
        self.set_window_shown(false, cx);
        // The window and its GPU atlas go away. Its images are rebuilt when it opens again.
        self.release_hidden_memory(None, cx);
        self.core_save_session();
        if std::env::var_os("NAVIA_FRAME_LOG").is_some() {
            eprintln!("navia: window closed to the tray");
        }
        true
    }

    /// Shows Navia by raising the window, or reopening it at its last size.
    pub fn show_main_window(&mut self, cx: &mut Context<Self>) {
        if let Some(main) = self.main_window {
            if main.update(cx, |_, window, _| window.activate_window()).is_ok() {
                return;
            }
            self.main_window = None;
        }
        let shell = cx.entity();
        let size = self.closed_size;
        cx.defer(move |cx| crate::open_main_window(shell, size, cx));
    }

    /// Whether a tray host is present, for Settings. It's checked off the UI thread at most every
    /// few seconds, so the answer can lag.
    pub fn tray_host_seen(&mut self, cx: &mut Context<Self>) -> bool {
        const EVERY: std::time::Duration = std::time::Duration::from_secs(5);
        let (seen, asked) = self.tray_host;
        if self.tray.is_none() {
            return false;
        }
        if asked.is_none_or(|at| at.elapsed() >= EVERY) {
            self.tray_host.1 = Some(std::time::Instant::now());
            cx.spawn(async move |this, cx| {
                let now = cx.background_spawn(async { crate::tray::host_available() }).await;
                this.update(cx, |shell, cx| {
                    if shell.tray_host.0 != now {
                        shell.tray_host.0 = now;
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
        seen
    }

    pub fn set_close_to_tray(&mut self, on: bool, cx: &mut Context<Self>) {
        self.prefs.close_to_tray = on;
        self.prefs.save();
        cx.notify();
    }

    /// Writes or removes the login entry. Settings shows whatever is there afterward.
    pub fn set_open_at_login(&mut self, choice: OpenAtLogin, cx: &mut Context<Self>) {
        if let Err(err) = crate::autostart::set(choice) {
            eprintln!("navia: open at login: {err}");
            self.show_toast(crate::i18n::tr("Couldn't change Open at login."), cx);
        }
        self.open_at_login = crate::autostart::current();
        cx.notify();
    }
}
