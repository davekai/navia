//! Saved interface choices, in a plain JSON file so nothing depends on the server or core.

use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::{Value, json};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PlayerMode {
    Dock,
    Bar,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DockView {
    Lyrics,
    Artwork,
    Queue,
}

/// Navidrome has no pins, so they stay on the device. own playlists start pinned and others' don't; these lists hold the exceptions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SidebarPins {
    pub pinned: Vec<String>,
    pub unpinned: Vec<String>,
}

impl SidebarPins {
    pub fn is_pinned(&self, id: &str, by_default: bool) -> bool {
        if self.pinned.iter().any(|p| p == id) {
            return true;
        }
        if self.unpinned.iter().any(|p| p == id) {
            return false;
        }
        by_default
    }

    pub fn set(&mut self, id: &str, on: bool, by_default: bool) {
        self.pinned.retain(|p| p != id);
        self.unpinned.retain(|p| p != id);
        if on != by_default {
            if on { &mut self.pinned } else { &mut self.unpinned }.push(id.to_string());
        }
    }
}

/// what happens between songs. Smart crossfades except where the next song continues the album in order (gapless).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BetweenSongs {
    Gapless,
    Crossfade,
    Smart,
    /// Smart transitions: each join shaped from the songs; the crossfade length is only its fallback.
    Mix,
}

impl BetweenSongs {
    /// album tracks in order join gapless.
    pub fn smart(self) -> bool {
        matches!(self, BetweenSongs::Smart | BetweenSongs::Mix)
    }
}

#[derive(Clone, Debug)]
pub struct Prefs {
    pub mode: PlayerMode,
    pub view: DockView,
    pub sidebar_open: bool,
    /// Interface size in percent, 70 to 130.
    pub zoom: u32,
    pub accent: u32,
    /// follows the desktop's accent, snapped to the closest preset, instead of `accent`.
    pub accent_system: bool,
    pub accent_system_migrated: bool,
    pub volume: f32,
    pub muted: bool,
    /// The user's Translucency choice. `None` follows the desktop, on only where blur behind
    /// windows was detected. See `translucent_on`.
    pub translucent: Option<bool>,
    /// The desktop draws the title bar and window buttons instead of Navia's own frame, where the
    /// Linux compositor offers it. Off by default. The legacy `own_frame` key is ignored.
    pub system_title_bar: bool,
    /// The dock's width in tokens, so it survives interface size changes.
    pub dock_width: f32,
    /// The sidebar's width in tokens, dragged on its right edge. `SIDEBAR_W` by default.
    pub sidebar_width: f32,
    /// Plays aren't reported and Discord is hidden. Stays on until turned off.
    pub private_mode: bool,
    /// Share what I'm playing. Listen-along sharing is opt-in, so it's off by default.
    pub share_listening: bool,
    /// Let listeners control playback. Off by default.
    pub listeners_control: bool,
    /// Friends followed through Lanyard, and the Lanyard address. An empty address means the public one.
    pub spotify_friends: Vec<navia_core::lanyard::Friend>,
    pub lanyard_address: String,
    /// Lyrics blur and dim by distance from the current line.
    pub lyric_blur: bool,
    /// lyric effects.
    pub lyric_effects: bool,
    /// off, the current line lights whole, with no rising words.
    pub lyric_sweep: bool,
    /// off, bracketed backing vocals stay inline, as written.
    pub backing_vocals_line: bool,
    pub between_songs: BetweenSongs,
    /// Crossfade length, 1 to 12 seconds.
    pub crossfade_s: u32,
    /// The chosen output's device id, or `None` for the system default.
    pub output_device: Option<String>,
    /// The sidebar's custom playlist order, by account, as playlist ids. Absent means the default.
    pub sidebar_orders: std::collections::HashMap<String, Vec<String>>,
    /// sidebar pins by account; only changes from the default are kept.
    pub sidebar_pins: std::collections::HashMap<String, SidebarPins>,
    /// the song tables' optional columns in `view_rows::OptCol` order.
    pub columns: [bool; 5],
    /// the album tables' columns in `view_rows::AlbumCol` order, kept apart from `columns`.
    /// heat starts on (shown only while offered) and a pick flips just that column, so it stays on unless hidden itself.
    pub album_columns: [bool; 3],
    /// Settings > Playback > Autoplay, on by default.
    pub autoplay: bool,
    /// Shuffle style, Fewer Repeats by default or Standard.
    pub shuffle_style: navia_core::queue_order::ShuffleStyle,
    /// Each playlist's column sort, like "title:asc", by playlist id. Without one a playlist keeps
    /// its own order.
    pub playlist_sorts: std::collections::HashMap<String, String>,
    /// Scrobbling choices by account (`Library::account_key`). A missing entry means on. Private
    /// mode overrides it. The legacy `scrobble` field is ignored because it never reflected a choice.
    pub scrobble_choices: std::collections::HashMap<String, bool>,
    /// accounts with the offline switch on.
    pub offline_accounts: std::collections::BTreeSet<String>,
    /// Discord status. Off until the user turns it on. Private mode overrides it.
    pub discord_status: bool,
    pub discord_art: DiscordArt,
    /// An `https` address Discord can load the server's covers from. Empty by default.
    pub discord_public_address: String,
    /// Where Discord covers come from. Automatic guesses from the server's address.
    pub discord_covers_from: navia_integrations::art::CoverSource,
    /// Refresh the library in the background. It always refreshes at launch.
    pub library_sync: bool,
    /// Interface language as a code, or `None` to follow the system.
    pub language: Option<String>,
    /// The mini player's last size in tokens, width and height. `None` is the compact size.
    pub mini_size: Option<(f32, f32)>,
    /// The mini player's top-left corner in logical pixels, relative to the work area on Linux
    /// layer-shell or the screen on Windows. `None` opens it in the bottom-right corner.
    pub mini_at: Option<(f32, f32)>,
    /// The one-time Always on Top hint was dismissed, on desktops without layer-shell.
    pub mini_hint_seen: bool,
    /// Minimizing the main window opens the mini player. Off by default.
    pub mini_on_minimize: bool,
    /// Keep playing in the tray when the window closes, the default, or quit. Without a tray host,
    /// closing always quits.
    pub close_to_tray: bool,
    /// Check for updates automatically. On by default.
    pub check_updates: bool,
    /// Albums and Artists show the split view instead of the grid.
    pub album_split: bool,
    pub artist_split: bool,
    /// the time under the seek bar shows what's left (`-1:23`) rather than the length.
    pub time_remaining: bool,
}

/// Where Discord's album art comes from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DiscordArt {
    /// Look the album up on public cover art sites.
    MatchPublic,
    /// Always the app's default image.
    Default,
}

impl Default for Prefs {
    fn default() -> Self {
        Self { mode: PlayerMode::Dock, view: DockView::Artwork, sidebar_open: true, zoom: 100, accent: crate::color::DEFAULT_ACCENT, accent_system: false, accent_system_migrated: false, volume: 80.0, muted: false, translucent: None, system_title_bar: false, dock_width: crate::theme::size::DOCK_W, sidebar_width: crate::theme::size::SIDEBAR_W, private_mode: false, share_listening: false, listeners_control: false, spotify_friends: Vec::new(), lanyard_address: String::new(), lyric_blur: true, lyric_effects: true, lyric_sweep: true, backing_vocals_line: true, between_songs: BetweenSongs::Gapless, crossfade_s: 6, output_device: None, sidebar_orders: Default::default(), sidebar_pins: Default::default(), columns: [true, true, false, false, false], album_columns: [true, false, true], autoplay: true, shuffle_style: Default::default(), playlist_sorts: Default::default(), scrobble_choices: Default::default(), offline_accounts: Default::default(), discord_status: false, discord_art: DiscordArt::MatchPublic, discord_public_address: String::new(), discord_covers_from: Default::default(), library_sync: true, language: None, mini_size: None, mini_at: None, mini_hint_seen: false, mini_on_minimize: false, close_to_tray: true, check_updates: true, album_split: false, artist_split: false, time_remaining: false }
    }
}

static ACCOUNT_KEY: Mutex<Option<String>> = Mutex::new(None);

/// Says which server and user per-account files belong to.
pub fn set_active_account(key: Option<String>) {
    *ACCOUNT_KEY.lock().unwrap() = key;
}

/// The folder for one server and user's files, such as the queue and sidebar order. `None`
/// before anyone signs in.
pub fn account_dir() -> Option<PathBuf> {
    let key = ACCOUNT_KEY.lock().unwrap().clone()?;
    Some(config_dir()?.join("accounts").join(key))
}

/// A file in the active account's folder.
pub fn account_file(name: &str) -> Option<PathBuf> {
    account_dir().map(|d| d.join(name))
}

/// Whether Navia runs inside a Flatpak sandbox.
pub fn in_flatpak() -> bool {
    cfg!(target_os = "linux") && std::path::Path::new("/.flatpak-info").exists()
}

/// Where the shell keeps its files. `NAVIA_CONFIG_DIR` overrides it for tests.
pub fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("NAVIA_CONFIG_DIR") {
        return Some(PathBuf::from(dir));
    }
    #[cfg(target_os = "linux")]
    {
        let dir = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(dir.join("navia"))
    }
    // Windows has no XDG or HOME, so use %APPDATA%\Navia\config.
    #[cfg(not(target_os = "linux"))]
    {
        directories::ProjectDirs::from("", "", "Navia").map(|d| d.config_dir().to_path_buf())
    }
}

fn path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("shell.json"))
}

impl Prefs {
    /// Whether the window is translucent. The user's choice, else on only where the desktop blurs
    /// behind windows. Some desktops blur through extensions, so the user can still turn it on.
    pub fn translucent_on(&self, blur_detected: bool) -> bool {
        self.translucent.unwrap_or(blur_detected)
    }

    /// one-release migration: every install moves to the System accent once, then later choices stick. true when it ran.
    pub fn migrate_to_system_accent(&mut self) -> bool {
        if self.accent_system_migrated {
            return false;
        }
        self.accent_system = true;
        self.accent_system_migrated = true;
        true
    }

    pub fn load() -> Self {
        let mut prefs = Self::default();
        let Some(path) = path() else { return prefs };
        let Ok(text) = std::fs::read_to_string(path) else { return prefs };
        let Ok(value) = serde_json::from_str::<Value>(&text) else { return prefs };
        if let Some(m) = value.get("mode").and_then(Value::as_str) {
            prefs.mode = if m == "bar" { PlayerMode::Bar } else { PlayerMode::Dock };
        }
        if let Some(v) = value.get("view").and_then(Value::as_str) {
            prefs.view = match v {
                "lyrics" => DockView::Lyrics,
                "queue" => DockView::Queue,
                _ => DockView::Artwork,
            };
        }
        if let Some(b) = value.get("sidebar").and_then(Value::as_bool) {
            prefs.sidebar_open = b;
        }
        if let Some(z) = value.get("zoom").and_then(Value::as_u64) {
            prefs.zoom = z.clamp(70, 130) as u32;
        }
        if let Some(a) = value.get("accent").and_then(Value::as_str) {
            if let Ok(c) = u32::from_str_radix(a.trim_start_matches('#'), 16) {
                prefs.accent = c;
            }
        }
        if let Some(on) = value.get("accent_system").and_then(Value::as_bool) {
            prefs.accent_system = on;
        }
        prefs.accent_system_migrated = value.get("accent_system_migrated").and_then(Value::as_bool).unwrap_or(false);
        if let Some(v) = value.get("volume").and_then(Value::as_f64) {
            prefs.volume = v as f32;
        }
        if let Some(m) = value.get("muted").and_then(Value::as_bool) {
            prefs.muted = m;
        }
        // Legacy files always wrote "translucent", so only a false there was a real choice.
        prefs.translucent = match value.get("translucency_choice").and_then(Value::as_bool) {
            Some(chosen) => Some(chosen),
            None => value.get("translucent").and_then(Value::as_bool).filter(|on| !on),
        };
        if let Some(f) = value.get("system_title_bar").and_then(Value::as_bool) {
            prefs.system_title_bar = f;
        }
        prefs.share_listening = value.get("share_listening").and_then(Value::as_bool).unwrap_or(false);
        prefs.listeners_control = value.get("listeners_control").and_then(Value::as_bool).unwrap_or(false);
        prefs.spotify_friends = value.get("spotify_friends").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default();
        prefs.lanyard_address = value.get("lanyard_address").and_then(Value::as_str).unwrap_or_default().to_string();
        if let Some(p) = value.get("private_mode").and_then(Value::as_bool) {
            prefs.private_mode = p;
        }
        if let Some(w) = value.get("dock_width").and_then(Value::as_f64) {
            prefs.dock_width = (w as f32).clamp(crate::theme::size::DOCK_MIN_W, crate::theme::size::DOCK_MAX_W);
        }
        if let Some(w) = value.get("sidebar_width").and_then(Value::as_f64) {
            prefs.sidebar_width = (w as f32).clamp(crate::theme::size::SIDEBAR_W_MIN, crate::theme::size::SIDEBAR_W_MAX);
        }
        if let Some(list) = value.get("album_columns").and_then(Value::as_array) {
            let names: Vec<&str> = list.iter().filter_map(Value::as_str).collect();
            prefs.album_columns = crate::view_rows::AlbumCol::ALL.map(|c| names.contains(&c.key()));
        }
        if let Some(list) = value.get("columns").and_then(Value::as_array) {
            let names: Vec<&str> = list.iter().filter_map(Value::as_str).collect();
            prefs.columns = crate::view_rows::OptCol::ALL.map(|c| names.contains(&c.key()));
        } else {
            // Older files only have `show_genre`.
            prefs.columns[2] = value.get("show_genre").and_then(Value::as_bool).unwrap_or(false);
        }
        prefs.autoplay = value.get("autoplay").and_then(Value::as_bool).unwrap_or(true);
        if value.get("shuffle_style").and_then(Value::as_str) == Some("standard") {
            prefs.shuffle_style = navia_core::queue_order::ShuffleStyle::Standard;
        }
        if let Some(map) = value.get("playlist_sorts").and_then(Value::as_object) {
            prefs.playlist_sorts = map.iter().filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string()))).collect();
        }
        if let Some(map) = value.get("sidebar_pins").and_then(Value::as_object) {
            let ids = |v: &Value, key: &str| v.get(key).and_then(Value::as_array).map(|ids| ids.iter().filter_map(|i| i.as_str().map(str::to_string)).collect()).unwrap_or_default();
            prefs.sidebar_pins = map.iter().map(|(k, v)| (k.clone(), SidebarPins { pinned: ids(v, "pinned"), unpinned: ids(v, "unpinned") })).collect();
        }
        if let Some(map) = value.get("sidebar_orders").and_then(Value::as_object) {
            prefs.sidebar_orders = map.iter().map(|(k, v)| (k.clone(), v.as_array().map(|ids| ids.iter().filter_map(|i| i.as_str().map(str::to_string)).collect()).unwrap_or_default())).collect();
        }
        // A legacy "pins" list is ignored and dropped at the next save.
        if let Some(b) = value.get("lyric_blur").and_then(Value::as_bool) {
            prefs.lyric_blur = b;
        }
        if let Some(b) = value.get("lyric_effects").and_then(Value::as_bool) {
            prefs.lyric_effects = b;
        }
        if let Some(b) = value.get("lyric_sweep").and_then(Value::as_bool) {
            prefs.lyric_sweep = b;
        }
        if let Some(b) = value.get("backing_vocals_line").and_then(Value::as_bool) {
            prefs.backing_vocals_line = b;
        }
        prefs.language = value.get("language").and_then(Value::as_str).filter(|l| *l != "system").map(str::to_string);
        if let Some(s) = value.get("between_songs").and_then(Value::as_str) {
            prefs.between_songs = match s {
                "crossfade" => BetweenSongs::Crossfade,
                "smart" => BetweenSongs::Smart,
                "mix" => BetweenSongs::Mix,
                _ => BetweenSongs::Gapless,
            };
        }
        if let Some(n) = value.get("crossfade_s").and_then(Value::as_u64) {
            prefs.crossfade_s = (n as u32).clamp(1, 12);
        }
        prefs.output_device = value.get("output_device").and_then(Value::as_str).map(str::to_string);
        if let Some(map) = value.get("scrobble_choices").and_then(Value::as_object) {
            prefs.scrobble_choices = map.iter().filter_map(|(k, v)| v.as_bool().map(|b| (k.clone(), b))).collect();
        }
        if let Some(list) = value.get("offline_accounts").and_then(Value::as_array) {
            prefs.offline_accounts = list.iter().filter_map(Value::as_str).map(str::to_string).collect();
        }
        if let Some(b) = value.get("discord_status").and_then(Value::as_bool) {
            prefs.discord_status = b;
        }
        if let Some(address) = value.get("discord_public_address").and_then(Value::as_str) {
            prefs.discord_public_address = address.to_string();
        }
        prefs.discord_covers_from = match value.get("discord_covers_from").and_then(Value::as_str) {
            Some("server") => navia_integrations::art::CoverSource::Server,
            Some("search") => navia_integrations::art::CoverSource::Search,
            _ => navia_integrations::art::CoverSource::Automatic,
        };
        if value.get("discord_art").and_then(Value::as_str) == Some("default") {
            prefs.discord_art = DiscordArt::Default;
        }
        if let Some(b) = value.get("library_sync").and_then(Value::as_bool) {
            prefs.library_sync = b;
        }
        // Legacy "compact" or "large" values fall back to the compact size.
        if let Some([w, h]) = value.get("mini_size").and_then(Value::as_array).map(Vec::as_slice) {
            prefs.mini_size = w.as_f64().zip(h.as_f64()).map(|(w, h)| (w as f32, h as f32));
        }
        if let Some([x, y]) = value.get("mini_at").and_then(Value::as_array).map(Vec::as_slice) {
            prefs.mini_at = x.as_f64().zip(y.as_f64()).map(|(x, y)| (x as f32, y as f32));
        }
        prefs.mini_hint_seen = value.get("mini_hint_seen").and_then(Value::as_bool).unwrap_or(false);
        prefs.mini_on_minimize = value.get("mini_on_minimize").and_then(Value::as_bool).unwrap_or(false);
        prefs.close_to_tray = value.get("close_to_tray").and_then(Value::as_bool).unwrap_or(true);
        prefs.check_updates = value.get("check_updates").and_then(Value::as_bool).unwrap_or(true);
        prefs.time_remaining = value.get("time_remaining").and_then(Value::as_bool).unwrap_or(false);
        prefs.album_split = value.get("album_layout").and_then(Value::as_str) == Some("split");
        prefs.artist_split = value.get("artist_layout").and_then(Value::as_str) == Some("split");
        // Drop the legacy "generated_covers" key from the file.
        if value.get("generated_covers").is_some() {
            prefs.save();
        }
        prefs
    }

    pub fn crossfade_ms(&self) -> u32 {
        if self.between_songs == BetweenSongs::Gapless { 0 } else { self.crossfade_s * 1000 }
    }


    pub fn save(&self) {
        let Some(path) = path() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let value = json!({
            "mode": if self.mode == PlayerMode::Bar { "bar" } else { "dock" },
            "view": match self.view {
                DockView::Lyrics => "lyrics",
                DockView::Artwork => "artwork",
                DockView::Queue => "queue",
            },
            "sidebar": self.sidebar_open,
            "zoom": self.zoom,
            "accent": format!("#{:06X}", self.accent),
            "accent_system": self.accent_system,
            "accent_system_migrated": self.accent_system_migrated,
            "volume": self.volume,
            "muted": self.muted,
            "translucency_choice": self.translucent,
            "system_title_bar": self.system_title_bar,
            "private_mode": self.private_mode,
            "share_listening": self.share_listening,
            "listeners_control": self.listeners_control,
            "spotify_friends": self.spotify_friends,
            "lanyard_address": self.lanyard_address,
            "dock_width": self.dock_width,
            "sidebar_width": self.sidebar_width,
            "lyric_blur": self.lyric_blur,
            "lyric_effects": self.lyric_effects,
            "lyric_sweep": self.lyric_sweep,
            "backing_vocals_line": self.backing_vocals_line,
            "language": self.language,
            "between_songs": match self.between_songs {
                BetweenSongs::Gapless => "gapless",
                BetweenSongs::Crossfade => "crossfade",
                BetweenSongs::Smart => "smart",
                BetweenSongs::Mix => "mix",
            },
            "crossfade_s": self.crossfade_s,
            "output_device": self.output_device,
            "scrobble_choices": self.scrobble_choices,
            "offline_accounts": self.offline_accounts,
            "discord_status": self.discord_status,
            "discord_art": if self.discord_art == DiscordArt::Default { "default" } else { "public" },
            "discord_public_address": self.discord_public_address,
            "discord_covers_from": match self.discord_covers_from {
                navia_integrations::art::CoverSource::Automatic => "auto",
                navia_integrations::art::CoverSource::Server => "server",
                navia_integrations::art::CoverSource::Search => "search",
            },
            "library_sync": self.library_sync,
            "sidebar_orders": self.sidebar_orders,
            "sidebar_pins": self.sidebar_pins.iter().map(|(k, p)| (k.clone(), serde_json::json!({ "pinned": p.pinned, "unpinned": p.unpinned }))).collect::<serde_json::Map<String, Value>>(),
            "columns": crate::view_rows::OptCol::ALL.iter().filter(|c| self.columns[**c as usize]).map(|c| c.key()).collect::<Vec<_>>(),
            "album_columns": crate::view_rows::AlbumCol::ALL.iter().filter(|c| self.album_columns[**c as usize]).map(|c| c.key()).collect::<Vec<_>>(),
            "autoplay": self.autoplay,
            "shuffle_style": if self.shuffle_style == navia_core::queue_order::ShuffleStyle::Standard { "standard" } else { "fewer" },
            "playlist_sorts": self.playlist_sorts,
            "mini_size": self.mini_size.map(|(w, h)| [w, h]),
            "mini_at": self.mini_at.map(|(x, y)| [x, y]),
            "mini_hint_seen": self.mini_hint_seen,
            "mini_on_minimize": self.mini_on_minimize,
            "close_to_tray": self.close_to_tray,
            "check_updates": self.check_updates,
            "album_layout": if self.album_split { "split" } else { "grid" },
            "time_remaining": self.time_remaining,
            "artist_layout": if self.artist_split { "split" } else { "grid" },
        });
        let _ = std::fs::write(path, serde_json::to_vec_pretty(&value).unwrap_or_default());
    }
}

#[cfg(test)]
mod system_accent_migration_tests {
    use super::*;

    #[test]
    fn a_saved_colour_moves_to_system_once() {
        let mut prefs = Prefs { accent: 0xf2685c, ..Prefs::default() };
        assert!(prefs.migrate_to_system_accent());
        assert!(prefs.accent_system && prefs.accent_system_migrated);
        assert_eq!(prefs.accent, 0xf2685c);
    }

    #[test]
    fn a_choice_after_the_migration_sticks() {
        let mut prefs = Prefs { accent: 0x8b7cf6, accent_system: false, accent_system_migrated: true, ..Prefs::default() };
        assert!(!prefs.migrate_to_system_accent());
        assert!(!prefs.accent_system);
    }
}
