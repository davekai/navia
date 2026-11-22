//! AirPlay receivers as outputs for the Mac app (`navia_core::audio::airplay`). The Keychain holds the pairing identity
//! through [`SecretStoreCallback`], and remote buttons and state changes come back through [`AirPlayListener`].

use std::sync::Arc;

use navia_core::audio::airplay::{AirPlay, AirPlayOutput, OutputState, RemoteCommand, SecretStore, Support};

use crate::NaviaError;

/// Secrets in the Keychain. `get` returns None when there's no such item. `set` and `delete` return false when the
/// Keychain can't be used, and the core then falls back to a private file.
#[uniffi::export(callback_interface)]
pub trait SecretStoreCallback: Send + Sync {
    fn get(&self, key: String) -> Option<String>;
    fn set(&self, key: String, value: String) -> bool;
    fn delete(&self, key: String) -> bool;
}

/// Called on a background thread.
#[uniffi::export(callback_interface)]
pub trait AirPlayListener: Send + Sync {
    /// A button on the receiver's remote.
    fn on_remote(&self, command: AirPlayRemote);
    /// Receivers came or went, or one's state changed. Read `outputs` again.
    fn on_changed(&self);
}

#[derive(uniffi::Enum, Clone, Copy, Debug)]
pub enum AirPlayRemote {
    PlayPause,
    Play,
    Pause,
    Stop,
    Next,
    Previous,
    /// The receiver's own volume from 0 to 1, for the slider. Display it but don't send it back. None once the output
    /// isn't a receiver, so the slider returns to this Mac's own volume.
    VolumeChanged { volume: Option<f32> },
}

/// A receiver, as the Output menu lists it.
#[derive(uniffi::Record, Clone, Debug)]
pub struct AirPlayOutputRow {
    /// `airplay:<deviceid>`, the id to pass to `set_output_device`.
    pub id: String,
    pub name: String,
    /// Manufacturer and model from the receiver. Empty when it isn't on the network.
    pub model: String,
    /// Navia can stream to it.
    pub ready: bool,
    /// Why it can't be used: `restricted`, `needsPtp`, `airplay1` or `password`. The app words it.
    pub unsupported: Option<String>,
    pub paired: bool,
    pub needs_pin: bool,
    /// Discovery sees it now. A paired receiver that's asleep is listed with false.
    pub found: bool,
    /// `connecting`, `playing`, `needsPairing`, `firewall` or `failed`.
    pub state: Option<String>,
    /// The reason when `failed`.
    pub failure: Option<String>,
}

fn row(o: AirPlayOutput) -> AirPlayOutputRow {
    let unsupported = match &o.support {
        Support::Ready(_) => None,
        Support::NeedsPtp => Some("needsPtp".to_string()),
        Support::AirPlay1Only => Some("airplay1".to_string()),
        Support::Restricted => Some("restricted".to_string()),
        Support::NeedsPassword => Some("password".to_string()),
    };
    let failure = match &o.state {
        Some(OutputState::Failed(m)) => Some(m.clone()),
        _ => None,
    };
    let state = o.state.map(|s| match s {
        OutputState::Connecting => "connecting",
        OutputState::Playing => "playing",
        OutputState::NeedsPairing => "needsPairing",
        OutputState::FirewallSuspected => "firewall",
        OutputState::Failed(_) => "failed",
    }.to_string());
    AirPlayOutputRow { ready: unsupported.is_none(), unsupported, id: o.id, name: o.name, model: o.model, paired: o.paired, needs_pin: o.needs_pin, found: o.found, state, failure }
}

pub(crate) struct KeychainSecrets(pub Box<dyn SecretStoreCallback>);

impl SecretStore for KeychainSecrets {
    fn get(&self, key: &str) -> Result<Option<String>, String> {
        Ok(self.0.get(key.to_string()))
    }
    fn set(&self, key: &str, value: &str) -> Result<(), String> {
        if self.0.set(key.to_string(), value.to_string()) { Ok(()) } else { Err("the Keychain refused".into()) }
    }
    fn delete(&self, key: &str) -> Result<(), String> {
        if self.0.delete(key.to_string()) { Ok(()) } else { Err("the Keychain refused".into()) }
    }
}

pub(crate) fn remote(cmd: RemoteCommand) -> Option<AirPlayRemote> {
    Some(match cmd {
        RemoteCommand::PlayPause => AirPlayRemote::PlayPause,
        RemoteCommand::Play => AirPlayRemote::Play,
        RemoteCommand::Pause => AirPlayRemote::Pause,
        RemoteCommand::Stop => AirPlayRemote::Stop,
        RemoteCommand::Next => AirPlayRemote::Next,
        RemoteCommand::Previous => AirPlayRemote::Previous,
        // Receivers handle their own volume keys and report the result.
        RemoteCommand::VolumeUp | RemoteCommand::VolumeDown => return None,
    })
}

#[derive(uniffi::Object)]
pub struct NaviaAirPlay {
    pub(crate) inner: AirPlay,
    pub(crate) listener: Arc<dyn AirPlayListener>,
}

#[uniffi::export]
impl NaviaAirPlay {
    /// Receivers on the network, by name.
    pub fn outputs(&self) -> Vec<AirPlayOutputRow> {
        self.inner.outputs().into_iter().map(row).collect()
    }

    /// ask the network for receivers again (the output menu opened, or the Mac woke).
    pub fn rescan(&self) {
        self.inner.rescan();
    }

    /// Ask the receiver to show its PIN. Blocks for up to a few seconds: call off the main thread.
    pub fn pair_begin(&self, id: String) -> Result<(), NaviaError> {
        Ok(self.inner.pair_begin(&id)?)
    }

    /// Finishes pairing with the PIN shown on the receiver. Fails with "wrong PIN" when it's wrong.
    pub fn pair_finish(&self, id: String, pin: String) -> Result<(), NaviaError> {
        Ok(self.inner.pair_finish(&id, &pin)?)
    }

    pub fn forget(&self, id: String) -> Result<(), NaviaError> {
        Ok(self.inner.forget(&id)?)
    }
}

pub(crate) fn new(inner: AirPlay, listener: Arc<dyn AirPlayListener>) -> Arc<NaviaAirPlay> {
    Arc::new(NaviaAirPlay { inner, listener })
}

/// Forwards the player's `DeviceVolume` to the app as `VolumeChanged`, separate from its own saved volume.
pub(crate) fn forward_device_volume(player: &navia_core::audio::player::Player, airplay: &NaviaAirPlay) {
    let listener = airplay.listener.clone();
    player.add_listener(move |event| {
        if let navia_core::audio::player::PlayerEvent::DeviceVolume(v) = event {
            listener.on_remote(AirPlayRemote::VolumeChanged { volume: v.map(|v| v.level) });
        }
    });
}
