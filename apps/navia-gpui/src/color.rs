//! Color math specified in CSS terms like oklch and color-mix. Values are sRGB bytes, except
//! the oklch conversions, which go through linear sRGB.

use gpui::Hsla;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Srgb {
    pub r: f32,
    pub g: f32,
    pub b: f32,
}

impl Srgb {
    pub fn hex(rgb: u32) -> Self {
        Self {
            r: ((rgb >> 16) & 0xff) as f32 / 255.0,
            g: ((rgb >> 8) & 0xff) as f32 / 255.0,
            b: (rgb & 0xff) as f32 / 255.0,
        }
    }

    /// Straight-alpha mix, like `color-mix(in srgb, self t, other)`.
    pub fn mix(self, other: Srgb, t: f32) -> Srgb {
        Srgb {
            r: self.r * t + other.r * (1.0 - t),
            g: self.g * t + other.g * (1.0 - t),
            b: self.b * t + other.b * (1.0 - t),
        }
    }

    pub fn hsla(self, alpha: f32) -> Hsla {
        Hsla::from(gpui::Rgba { r: self.r, g: self.g, b: self.b, a: alpha })
    }

    pub fn luminance(self) -> f32 {
        let f = |c: f32| if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
        0.2126 * f(self.r) + 0.7152 * f(self.g) + 0.0722 * f(self.b)
    }
}

// APCA 0.0.98G, a perceptual lightness contrast. WCAG 2 ratios favor black on mid-tones where
// white reads better. Tokens.swift uses the same constants.
const APCA_TRC: f32 = 2.4;
const APCA_COEFFS: [f32; 3] = [0.212_672_9, 0.715_152_2, 0.072_175];
const APCA_BLK_THRS: f32 = 0.022;
const APCA_BLK_CLMP: f32 = 1.414;
const APCA_NORM_BG: f32 = 0.56;
const APCA_NORM_TXT: f32 = 0.57;
const APCA_REV_TXT: f32 = 0.62;
const APCA_REV_BG: f32 = 0.65;
const APCA_SCALE: f32 = 1.14;
const APCA_LO_OFFSET: f32 = 0.027;
const APCA_LO_CLIP: f32 = 0.1;
const APCA_DELTA_Y_MIN: f32 = 0.0005;

/// with its soft clamp near black.
fn apca_y(c: Srgb) -> f32 {
    let [r, g, b] = APCA_COEFFS;
    let y = r * c.r.powf(APCA_TRC) + g * c.g.powf(APCA_TRC) + b * c.b.powf(APCA_TRC);
    if y < APCA_BLK_THRS { y + (APCA_BLK_THRS - y).powf(APCA_BLK_CLMP) } else { y }
}

/// APCA lightness contrast (Lc) of `text` on `background`. Positive for dark on light, negative
/// for light on dark.
pub fn apca_lc(text: Srgb, background: Srgb) -> f32 {
    let (t, b) = (apca_y(text), apca_y(background));
    if (b - t).abs() < APCA_DELTA_Y_MIN {
        return 0.0;
    }
    if b > t {
        let s = (b.powf(APCA_NORM_BG) - t.powf(APCA_NORM_TXT)) * APCA_SCALE;
        if s < APCA_LO_CLIP { 0.0 } else { (s - APCA_LO_OFFSET) * 100.0 }
    } else {
        let s = (b.powf(APCA_REV_BG) - t.powf(APCA_REV_TXT)) * APCA_SCALE;
        if s > -APCA_LO_CLIP { 0.0 } else { (s + APCA_LO_OFFSET) * 100.0 }
    }
}

fn to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Oklch {
    pub l: f32,
    pub c: f32,
    pub h: f32,
}

pub fn to_oklch(c: Srgb) -> Oklch {
    let (r, g, b) = (to_linear(c.r), to_linear(c.g), to_linear(c.b));
    let l = 0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b;
    let m = 0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b;
    let s = 0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b;
    let (l_, m_, s_) = (l.cbrt(), m.cbrt(), s.cbrt());
    let lab_l = 0.2104542553 * l_ + 0.7936177850 * m_ - 0.0040720468 * s_;
    let a = 1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_;
    let bb = 0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_;
    Oklch { l: lab_l, c: (a * a + bb * bb).sqrt(), h: bb.atan2(a).to_degrees().rem_euclid(360.0) }
}

pub fn from_oklch(c: Oklch) -> Srgb {
    let (a, b) = (c.c * c.h.to_radians().cos(), c.c * c.h.to_radians().sin());
    let l_ = c.l + 0.3963377774 * a + 0.2158037573 * b;
    let m_ = c.l - 0.1055613458 * a - 0.0638541728 * b;
    let s_ = c.l - 0.0894841775 * a - 1.2914855480 * b;
    let (l, m, s) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    let r = 4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s;
    let g = -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s;
    let bb = -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s;
    Srgb { r: to_srgb(r), g: to_srgb(g), b: to_srgb(bb) }
}

/// The vivid tint, `oklch(from tint clamp(.27, l + .03, .6) min(.15, c * 1.25) h)`.
pub fn tint_vivid(tint: Srgb) -> Srgb {
    let o = to_oklch(tint);
    from_oklch(Oklch { l: (o.l + 0.03).clamp(0.27, 0.6), c: (o.c * 1.25).min(0.15), h: o.h })
}

/// The player's background. The vivid tint with lightness capped at .47 instead of .6, so
/// light covers keep 4.5:1 under the player's text.
pub fn player_vivid(tint: Srgb) -> Srgb {
    let o = to_oklch(tint);
    from_oklch(Oklch { l: (o.l + 0.03).clamp(0.27, crate::theme::size::TINT_TEXT_SAFE_L), c: (o.c * 1.25).min(0.15), h: o.h })
}

/// The gradient for a tile with light text on a tint, such as a genre tile. The capped vivid
/// tint at the top left, darkened by `TILE_FILL_SHADE` toward the opposite corner.
pub fn tile_fill(tint: Srgb) -> (Srgb, Srgb) {
    let top = player_vivid(tint);
    let o = to_oklch(top);
    (top, from_oklch(Oklch { l: o.l * crate::theme::size::TILE_FILL_SHADE, ..o }))
}

/// The player's brightest foreground, for the controls and the progress and volume fills.
/// `oklch(from tint .90 min(.13, c * 1.8) h)`, so a gray cover stays gray.
pub fn player_fg(tint: Srgb) -> Srgb {
    let o = to_oklch(tint);
    from_oklch(Oklch { l: 0.90, c: (o.c * 1.8).min(0.13), h: o.h })
}

/// The player's dimmer foreground, for the artist, times and volume icons.
/// `oklch(from tint .85 min(.11, c * 1.6) h)`, fully opaque.
pub fn player_fg_dim(tint: Srgb) -> Srgb {
    let o = to_oklch(tint);
    from_oklch(Oklch { l: 0.85, c: (o.c * 1.6).min(0.11), h: o.h })
}

/// for tests.
#[cfg(test)]
pub fn to_hex(c: Srgb) -> u32 {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    (b(c.r) << 16) | (b(c.g) << 8) | b(c.b)
}

/// Contrast the expanded player's scrim keeps for the dim text and for the controls and title.
const SCRIM_DIM_CONTRAST: f32 = 3.0;
const SCRIM_FG_CONTRAST: f32 = 4.5;

/// The expanded player's text scrim opacity. Black just strong enough to bring the brightest
/// spot under the controls (`peak`, relative luminance) to 3:1 against the dim foreground and
/// 4.5:1 against the bright one, capped at `GLOW_SCRIM_MAX`.
pub fn glow_scrim_alpha(peak: f32, tint: Srgb) -> f32 {
    let dim = (player_fg_dim(tint).luminance() + 0.05) / SCRIM_DIM_CONTRAST;
    let fg = (player_fg(tint).luminance() + 0.05) / SCRIM_FG_CONTRAST;
    let target = dim.min(fg) - 0.05;
    if peak <= target {
        return 0.0;
    }
    // A black layer scales sRGB values, and luminance follows at about the 2.2 power.
    (((1.0 - (target / peak).powf(1.0 / 2.2)) * 100.0).ceil() / 100.0).min(crate::theme::size::GLOW_SCRIM_MAX)
}

/// The bar glow's color, at a fixed lightness so dark covers still glow.
/// `oklch(from tint .62 clamp(.05, c * 1.7, .15) h)`.
pub fn tint_bar_glow(tint: Srgb) -> Srgb {
    let o = to_oklch(tint);
    from_oklch(Oklch { l: 0.62, c: (o.c * 1.7).clamp(0.05, 0.15), h: o.h })
}

/// The tint foreground, `oklch(from tint .86 min(.13, c * 1.5) h)`.
pub fn tint_fg(tint: Srgb) -> Srgb {
    let o = to_oklch(tint);
    from_oklch(Oklch { l: 0.86, c: (o.c * 1.5).min(0.13), h: o.h })
}

/// The dim tint foreground, `oklch(from tint .88 .06 h)`.
pub fn tint_fg_dim(tint: Srgb) -> Srgb {
    let o = to_oklch(tint);
    from_oklch(Oklch { l: 0.88, c: 0.06, h: o.h })
}

pub fn is_hex(v: &str) -> bool {
    v.len() == 7 && v.starts_with('#') && v[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn parse_hex(v: &str) -> Option<u32> {
    is_hex(v).then(|| u32::from_str_radix(&v[1..], 16).ok()).flatten()
}

pub const DEFAULT_ACCENT: u32 = 0x5a93f5;
pub const ACCENT_PRESETS: [u32; 7] = [DEFAULT_ACCENT, 0x50ffff, 0x8b7cf6, 0xe879a6, 0xf2685c, 0xf5a33d, 0x4cc38a];
/// below this HSL saturation the system accent has no real hue (graphite, grey).
pub const SYSTEM_ACCENT_MIN_S: f32 = 0.1;

/// the system's accent with its hue and chroma kept and its lightness held to the presets' range, so it reads on
/// Navia's surfaces like they do; chroma drops only as far as sRGB needs. the default without one or without a real hue.
pub fn system_accent(system: Option<u32>) -> u32 {
    let Some(rgb) = system else { return DEFAULT_ACCENT };
    let c = Srgb::hex(rgb);
    let (high, low) = (c.r.max(c.g).max(c.b), c.r.min(c.g).min(c.b));
    let l = (high + low) / 2.0;
    let s = if high == low { 0.0 } else { (high - low) / (1.0 - (2.0 * l - 1.0).abs()) };
    if s < SYSTEM_ACCENT_MIN_S {
        return DEFAULT_ACCENT;
    }
    let lightness = ACCENT_PRESETS.map(|p| to_oklch(Srgb::hex(p)).l);
    let (lo, hi) = lightness.iter().fold((f32::MAX, f32::MIN), |(a, b), &l| (a.min(l), b.max(l)));
    let o = to_oklch(c);
    let mut held = Oklch { l: o.l.clamp(lo, hi), ..o };
    if !in_srgb(held) {
        let (mut fits, mut over) = (0.0, held.c);
        for _ in 0..24 {
            let mid = (fits + over) / 2.0;
            if in_srgb(Oklch { c: mid, ..held }) { fits = mid } else { over = mid }
        }
        held.c = fits;
    }
    let out = from_oklch(held);
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    (byte(out.r) << 16) | (byte(out.g) << 8) | byte(out.b)
}

/// whether the colour is inside sRGB before any clipping.
fn in_srgb(c: Oklch) -> bool {
    let (a, b) = (c.c * c.h.to_radians().cos(), c.c * c.h.to_radians().sin());
    let (l_, m_, s_) = (c.l + 0.3963377774 * a + 0.2158037573 * b, c.l - 0.1055613458 * a - 0.0638541728 * b, c.l - 0.0894841775 * a - 1.2914855480 * b);
    let (l, m, s) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    let rgb = [4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s, -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s, -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s];
    rgb.iter().all(|v| (-1e-4..=1.0 + 1e-4).contains(v))
}

/// the default accent's saturation and lightness, so it sits on the slider.
pub const ACCENT_S: f32 = 0.886;
pub const ACCENT_L: f32 = 0.657;

/// The accent color at a hue in degrees, as a packed RGB value.
pub fn hue_to_hex(hue: f32) -> u32 {
    let (s, l) = (ACCENT_S, ACCENT_L);
    let f = |n: f32| -> u32 {
        let k = (n + hue / 30.0) % 12.0;
        let v = l - s * l.min(1.0 - l) * (-1f32).max((k - 3.0).min(9.0 - k).min(1.0));
        (v * 255.0).round() as u32
    };
    (f(0.0) << 16) | (f(8.0) << 8) | f(4.0)
}

/// The hue in degrees of an accent color, for the slider.
pub fn hex_to_hue(rgb: u32) -> f32 {
    let c = Srgb::hex(rgb);
    let (r, g, b) = (c.r, c.g, c.b);
    let max = r.max(g).max(b);
    let d = max - r.min(g).min(b);
    if d == 0.0 {
        return 0.0;
    }
    let h = if max == r { ((g - b) / d).rem_euclid(6.0) } else if max == g { (b - r) / d + 2.0 } else { (r - g) / d + 4.0 };
    ((h * 60.0 + 360.0) % 360.0).round()
}

#[cfg(test)]
mod player_tint_tests {
    use super::*;

    #[test]
    fn system_accents_keep_their_own_hue() {
        let hue = |rgb: u32| to_oklch(Srgb::hex(rgb)).h;
        let near = |a: f32, b: f32| ((a - b + 540.0) % 360.0 - 180.0).abs() < 4.0;
        // a dark olive desktop accent stays olive, only lighter
        for olive in [0x717339, 0x5a5b33] {
            let out = system_accent(Some(olive));
            assert!(near(hue(out), hue(olive)), "{olive:06x} -> {out:06x}");
            assert!(to_oklch(Srgb::hex(out)).l > to_oklch(Srgb::hex(olive)).l);
            assert!(!ACCENT_PRESETS.contains(&out), "{out:06x}");
        }
        // the platform's purple, pink, orange, yellow, green and red stay themselves
        for system in [0x953d96, 0xf74f9e, 0xf7821b, 0xffc600, 0x62ba46, 0xff5257] {
            let out = system_accent(Some(system));
            assert!(near(hue(out), hue(system)), "{system:06x} -> {out:06x}");
        }
        // inside the presets' lightness it's the colour itself
        assert_eq!(system_accent(Some(0x8b7cf6)), 0x8b7cf6);
        // grey or none gives the default
        assert_eq!(system_accent(Some(0x8c8c8c)), DEFAULT_ACCENT);
        assert_eq!(system_accent(None), DEFAULT_ACCENT);
    }

    // reference values for the player tint, oklch with per-channel sRGB clipping as browsers draw it.
    // four cover tints, one of them gray, and the fallback.
    const REFERENCE: [(u32, u32, u32, u32); 5] = [
        (0x29609e, 0xa0e3ff, 0x9bd2ff, 0x055aa9),
        (0x0c0f33, 0xc4d8ff, 0xb7c9ff, 0x1b2050),
        (0x9e9e9e, 0xdedede, 0xcecece, 0x5b5b5b),
        (0x9e2827, 0xffbcb1, 0xffb2a8, 0x9e2c2a),
        (0x34506b, 0xa8e4ff, 0x9fd3ff, 0x34597b),
    ];

    fn close(a: u32, b: u32) -> bool {
        (0..3).all(|i| (((a >> (i * 8)) & 0xff) as i32 - ((b >> (i * 8)) & 0xff) as i32).abs() <= 1)
    }

    #[test]
    fn matches_the_reference_tints() {
        for (tint, fg, dim, vivid) in REFERENCE {
            let t = Srgb::hex(tint);
            assert!(close(to_hex(player_fg(t)), fg), "fg {tint:06x}: {:06x} vs {fg:06x}", to_hex(player_fg(t)));
            assert!(close(to_hex(player_fg_dim(t)), dim), "dim {tint:06x}: {:06x} vs {dim:06x}", to_hex(player_fg_dim(t)));
            assert!(close(to_hex(player_vivid(t)), vivid), "vivid {tint:06x}: {:06x} vs {vivid:06x}", to_hex(player_vivid(t)));
        }
        assert_eq!(to_hex(player_fg(Srgb::hex(0x29609e))), 0xa0e3ff);
    }

    #[test]
    fn grey_stays_grey() {
        let c = player_fg(Srgb::hex(0x9e9e9e));
        assert!((c.r - c.g).abs() < 0.004 && (c.g - c.b).abs() < 0.004);
    }

    #[test]
    fn text_keeps_contrast_on_its_background() {
        let ratio = |a: f32, b: f32| (a.max(b) + 0.05) / (a.min(b) + 0.05);
        for (tint, ..) in REFERENCE {
            let t = Srgb::hex(tint);
            // The controls keep 4.5:1. The dim step is about 4.3:1 on the lightest backgrounds. Keeping
            // 4.5 there would need a background cap of .45 instead of .47.
            assert!(ratio(player_fg(t).luminance(), player_vivid(t).luminance()) >= 4.5, "{tint:06x}");
            assert!(ratio(player_fg_dim(t).luminance(), player_vivid(t).luminance()) >= 4.25, "{tint:06x}");
        }
    }

    #[test]
    fn scrim_only_when_needed() {
        let t = Srgb::hex(0x9e9e9e);
        assert_eq!(glow_scrim_alpha(0.05, t), 0.0);
        // A backdrop bright enough to need a scrim, but under the cap.
        let a = glow_scrim_alpha(0.25, t);
        assert!(a > 0.0 && a <= crate::theme::size::GLOW_SCRIM_MAX);
        // After the scrim the peak is at or under the targets.
        let scaled = 0.25 * (1.0 - a).powf(2.2);
        assert!((player_fg_dim(t).luminance() + 0.05) / (scaled + 0.05) >= 3.0);
        assert!((player_fg(t).luminance() + 0.05) / (scaled + 0.05) >= 4.5);
        // A very bright backdrop stops at the most the scrim may darken.
        assert_eq!(glow_scrim_alpha(0.9, t), crate::theme::size::GLOW_SCRIM_MAX);
    }
}

#[cfg(test)]
mod accent_tests {
    use super::*;

    #[test]
    fn apca_known_values() {
        // White on the default blue reads better than the dark ink, though WCAG 2 picks the ink.
        let blue = Srgb::hex(0x5a93f5);
        assert!((apca_lc(Srgb::hex(0xffffff), blue) + 62.2).abs() < 0.2);
        assert!((apca_lc(Srgb::hex(0x0b1220), blue) - 46.4).abs() < 0.2);
        assert_eq!(apca_lc(blue, blue), 0.0);
    }

    #[test]
    fn hex_checks() {
        assert!(is_hex("#5a93f5"));
        assert!(!is_hex("5a93f5"));
        assert!(!is_hex("#5a93f"));
        assert_eq!(parse_hex("#5A93F5"), Some(0x5a93f5));
    }

    #[test]
    fn hue_round_trips_within_a_degree() {
        for hue in (0..360).step_by(17) {
            let back = hex_to_hue(hue_to_hex(hue as f32));
            assert!((back - hue as f32).abs() <= 1.5 || (back - hue as f32).abs() >= 358.5, "{hue} -> {back}");
        }
        assert_eq!(hex_to_hue(0xffffff), 0.0);
    }
}
