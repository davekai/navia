//! AirPlay receivers as audio outputs: the service, and the shell's calls for the Output menu
//! and the PIN dialog. Choosing a receiver uses `set_output_device` with `airplay:<id>`.

use std::sync::Arc;

use futures::channel::mpsc::UnboundedSender;
use gpui::Context;
use gpui::prelude::*;
use navia_core::audio::airplay::{AirPlay, AirPlayConfig, AirPlayOutput, ClientNowPlaying, DEFAULT_PORTS, RemoteCommand};
use navia_integrations::PlayerCommand;

use crate::core_library::CoreLibrary;
use crate::core_player::CoreEvent;
use crate::shell::Shell;

/// Starts the AirPlay service, with discovery, stored pairings, track metadata and remote buttons as media keys.
pub fn start(lib: &Arc<CoreLibrary>, events: UnboundedSender<CoreEvent>) -> Option<AirPlay> {
    let dir = crate::persist::config_dir()?;
    let remote = events.clone();
    let config = AirPlayConfig {
        config_dir: dir,
        secrets: Some(Arc::new(crate::account::KeyringSecrets)),
        ports: DEFAULT_PORTS,
        now_playing: Some(Arc::new(ClientNowPlaying::new(lib.client.clone()))),
        remote: Some(Arc::new(move |cmd| {
            let command = match cmd {
                RemoteCommand::PlayPause => PlayerCommand::PlayPause,
                RemoteCommand::Play => PlayerCommand::Play,
                RemoteCommand::Pause => PlayerCommand::Pause,
                RemoteCommand::Stop => PlayerCommand::Stop,
                RemoteCommand::Next => PlayerCommand::Next,
                RemoteCommand::Previous => PlayerCommand::Previous,
                // Receivers handle their own volume keys. The resulting volume reaches the slider
                // through `PlayerEvent::DeviceVolume`.
                RemoteCommand::VolumeUp | RemoteCommand::VolumeDown => return,
            };
            let _ = remote.unbounded_send(CoreEvent::Command(command));
        })),
    };
    let airplay = match AirPlay::new(config) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("navia: AirPlay is off: {e}");
            return None;
        }
    };
    let changed = events;
    if let Err(e) = airplay.start_discovery(move || {
        let _ = changed.unbounded_send(CoreEvent::AirPlayChanged);
    }) {
        eprintln!("navia: AirPlay receivers can't be found: {e}");
    }
    Some(airplay)
}

impl Shell {
    /// for the Output menu.
    pub fn airplay_outputs(&self) -> Vec<AirPlayOutput> {
        self.core.as_ref().and_then(|c| c.airplay.as_ref()).map(|a| a.outputs()).unwrap_or_default()
    }

    /// Asks a receiver to show its PIN. `done` gets the outcome on the UI thread.
    pub fn airplay_pair_begin(&mut self, id: String, cx: &mut Context<Self>, done: impl FnOnce(&mut Self, Result<(), String>, &mut Context<Self>) + 'static) {
        let Some(ap) = self.core.as_ref().and_then(|c| c.airplay.clone()) else { return };
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { ap.pair_begin(&id).map_err(|e| e.to_string()) }).await;
            let _ = this.update(cx, |this, cx| done(this, result, cx));
        })
        .detach();
    }

    pub fn airplay_pair_finish(&mut self, id: String, pin: String, cx: &mut Context<Self>, done: impl FnOnce(&mut Self, Result<(), String>, &mut Context<Self>) + 'static) {
        let Some(ap) = self.core.as_ref().and_then(|c| c.airplay.clone()) else { return };
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { ap.pair_finish(&id, &pin).map_err(|e| e.to_string()) }).await;
            let _ = this.update(cx, |this, cx| done(this, result, cx));
        })
        .detach();
    }

    pub fn airplay_forget(&mut self, id: &str) {
        if let Some(ap) = self.core.as_ref().and_then(|c| c.airplay.as_ref()) {
            let _ = ap.forget(id);
        }
    }
}
