//! Bundled icons and fonts, embedded so a packaged build has no files to find.

use std::borrow::Cow;

use anyhow::Result;
use gpui::{App, AssetSource, SharedString};

pub struct Assets;

macro_rules! icons {
    ($($name:literal),* $(,)?) => {
        const ICONS: &[(&str, &[u8])] = &[$(($name, include_bytes!(concat!("../assets/icons/", $name, ".svg")))),*];
    };
}

icons!(
    "sidebar-simple-regular", "magnifying-glass-regular", "house-regular", "house-fill", "squares-four-regular", "broadcast-regular",
    "music-note-regular", "music-note-fill", "microphone-stage-regular",
    "stack-regular", "guitar-regular", "playlist-regular", "gear-regular", "caret-down-bold", "caret-up-fill", "caret-down-fill", "caret-right-bold", "caret-left-bold",
    "arrows-down-up-regular", "funnel-simple-regular", "funnel-simple-fill", "list-bullets-regular", "list-bullets-fill", "rows-regular", "rows-fill",
    "flame-regular", "flame-fill", "star-four-regular", "star-four-fill", "arrow-down-regular", "heart-regular", "heart-fill", "dots-three-bold", "dots-six-vertical-bold",
    "play-fill", "play-regular", "pause-lines-regular", "laptop-regular", "rewind-fill", "fast-forward-fill", "pause-fill", "repeat-bold", "shuffle-bold", "broom-bold", "headphones-regular", "repeat-regular", "shuffle-regular",
    "speaker-high-regular", "speaker-low-regular", "speaker-none-regular", "speaker-slash-regular", "x-regular", "minus-regular", "square-regular",
    "nv-win-close", "nv-win-min-line", "nv-win-max-square", "nv-win-restore-squares", "nv-win-min-chevron", "nv-win-max-chevron", "nv-win-restore-diamond",
    "nv-lyrics", "nv-expand", "nv-collapse", "nv-bar", "nv-dock", "nv-library", "nv-airplay",
    "picture-in-picture-regular", "app-window-regular", "arrows-in-simple-bold", "arrows-out-simple-bold",
    "queue-regular", "quotes-regular", "plus-regular", "plus-fill", "caret-up-down-bold", "arrow-down-fill", "arrow-bend-up-right-regular", "list-plus-regular", "disc-regular", "radio-regular", "share-network-regular", "grid-four-regular", "grid-four-fill", "user-plus-regular", "pencil-simple-regular", "export-regular", "lightning-fill", "sparkle-regular", "image-regular", "upload-regular", "users-regular", "link-simple-regular", "eye-slash-regular", "cloud-slash-regular", "cloud-slash-fill", "stop-regular", "waveform-regular", "music-notes-regular", "music-notes-fill", "clock-regular", "user-fill", "copy-regular", "check-bold", "info-regular", "music-note-bold", "floppy-disk-regular", "trash-regular", "arrow-counter-clockwise-regular", "warning-fill", "lock-regular",
    "navidrome-logo", "eye-regular", "lock-key-open-fill", "check-circle-fill", "warning-circle-fill", "circle-notch-regular", "hard-drives-regular", "sign-out-regular", "lightning-regular",
    "device-mobile-speaker-regular", "x-bold", "minus-circle-regular", "arrows-clockwise-bold", "arrows-clockwise-regular",
    "nv-brand-mark", "nv-brand-word", "paper-plane-tilt-regular", "key-regular", "plus-bold", "lightbulb-fill", "copy-simple-regular",
    "push-pin-regular", "push-pin-slash-regular", "x-circle-fill", "columns-regular", "wave-sine-bold",
);

/// Width over height of `nv-brand-word.svg`, from its viewBox. A test checks they agree.
pub const BRAND_WORD_ASPECT: f32 = 2.967;

/// Prefix for icons drawn with a transparent margin of `ui::GROW_MARGIN` on each edge.
/// The margin lets them scale as sprites without sampling a neighbor in the icon atlas.
pub const GROW_PREFIX: &str = "grow/";

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let (grow, name) = match path.strip_prefix(GROW_PREFIX) {
            Some(rest) => (true, rest.trim_end_matches(".svg")),
            None => (false, path.trim_end_matches(".svg")),
        };
        let Some((_, bytes)) = ICONS.iter().find(|(n, _)| *n == name) else { return Ok(None) };
        if !grow {
            return Ok(Some(Cow::Borrowed(*bytes)));
        }
        Ok(with_margin(bytes, crate::ui::GROW_MARGIN).map(|s| Cow::Owned(s.into_bytes())))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS.iter().filter(|(n, _)| n.starts_with(path)).map(|(n, _)| SharedString::from(*n)).collect())
    }
}

/// widened by `margin` of its size on every side.
fn with_margin(svg: &[u8], margin: f32) -> Option<String> {
    let text = std::str::from_utf8(svg).ok()?;
    let start = text.find("viewBox=\"")? + "viewBox=\"".len();
    let end = start + text[start..].find('"')?;
    let v: Vec<f32> = text[start..end].split([' ', ',']).filter(|s| !s.is_empty()).filter_map(|s| s.parse().ok()).collect();
    let [x, y, w, h] = v[..] else { return None };
    let (mx, my) = (w * margin, h * margin);
    Some(format!("{}{} {} {} {}{}", &text[..start], x - mx, y - my, w + 2.0 * mx, h + 2.0 * my, &text[end..]))
}

pub const FONT_FAMILY: &str = "Inter";

/// The system's monospace family as fontconfig names it, since GPUI has no generic family.
/// Falls back to a common one when fontconfig is unavailable.
pub fn font_mono() -> SharedString {
    static MONO: std::sync::OnceLock<SharedString> = std::sync::OnceLock::new();
    MONO.get_or_init(|| {
        let asked = std::process::Command::new("fc-match").args(["-f", "%{family[0]}", "monospace"]).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|s| !s.is_empty());
        SharedString::from(asked.unwrap_or_else(|| if cfg!(target_os = "macos") { "Menlo".into() } else { "DejaVu Sans Mono".into() }))
    })
    .clone()
}

pub fn register_fonts(cx: &mut App) {
    let fonts: Vec<Cow<'static, [u8]>> = [
        include_bytes!("../assets/fonts/Inter-Regular.ttf").as_slice(),
        include_bytes!("../assets/fonts/Inter-Medium.ttf").as_slice(),
        include_bytes!("../assets/fonts/Inter-SemiBold.ttf").as_slice(),
        include_bytes!("../assets/fonts/Inter-Bold.ttf").as_slice(),
    ]
    .into_iter()
    .map(Cow::Borrowed)
    .collect();
    if let Err(e) = cx.text_system().add_fonts(fonts) {
        eprintln!("navia: bundled fonts failed to load: {e}");
    }
}

pub fn icon(name: &str) -> SharedString {
    SharedString::from(name.to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn brand_word_aspect_matches_its_art() {
        let svg = std::str::from_utf8(include_bytes!("../assets/icons/nv-brand-word.svg")).unwrap();
        let start = svg.find("viewBox=\"").unwrap() + 9;
        let v: Vec<f32> = svg[start..start + svg[start..].find('"').unwrap()].split(' ').map(|s| s.parse().unwrap()).collect();
        assert!((v[2] / v[3] - super::BRAND_WORD_ASPECT).abs() < 0.01, "BRAND_WORD_ASPECT should be {}", v[2] / v[3]);
    }
}
