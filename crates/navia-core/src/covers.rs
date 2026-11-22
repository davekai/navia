//! ready-made playlist covers: nine styles drawn in software, each carrying the playlist's title. the New Playlist
//! dialog shows them and uploads the chosen one as the playlist's image, so the picture is the same from every app.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use tiny_skia::{
    BlendMode, Color, FillRule, FilterQuality, GradientStop, LinearGradient, Paint, PathBuilder, Pattern, Pixmap, Point,
    RadialGradient, Rect, Shader, SpreadMode, Transform,
};
use ttf_parser::{Face, OutlineBuilder};

/// pixels on a side of the image uploaded for a chosen cover.
pub const COVER_EXPORT_PX: u32 = 1000;

// the app's bold face, so the title is the same on every platform.
static TITLE_FONT: &[u8] = include_bytes!("../../../apps/navia-gpui/assets/fonts/Inter-Bold.ttf");

type Rgb = [u8; 3];

/// a cover style: `id` is stable, `name` is its English catalog key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoverStyle {
    pub id: &'static str,
    pub name: &'static str,
}

/// in the order the dialog offers them; the first is the default.
pub const COVER_STYLES: [CoverStyle; 9] = [
    CoverStyle { id: "ember", name: "Ember" },
    CoverStyle { id: "lagoon", name: "Lagoon" },
    CoverStyle { id: "dusk", name: "Dusk" },
    CoverStyle { id: "paper-sun", name: "Paper sun" },
    CoverStyle { id: "moss", name: "Moss" },
    CoverStyle { id: "glacier", name: "Glacier" },
    CoverStyle { id: "ink", name: "Ink" },
    CoverStyle { id: "blush", name: "Blush" },
    CoverStyle { id: "slate", name: "Slate" },
];

struct Canvas {
    px: Pixmap,
    s: f32,
}

fn rgba(c: Rgb, a: f32) -> Color {
    Color::from_rgba(c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, a.clamp(0.0, 1.0)).unwrap_or(Color::TRANSPARENT)
}

fn hex(v: u32) -> Rgb {
    [(v >> 16) as u8, (v >> 8) as u8, v as u8]
}

impl Canvas {
    fn new(size: u32) -> Option<Self> {
        Some(Self { px: Pixmap::new(size, size)?, s: size as f32 })
    }

    fn paint(&mut self, shader: Shader, blend: BlendMode, rect: Rect, transform: Transform) {
        let paint = Paint { shader, blend_mode: blend, anti_alias: true, ..Paint::default() };
        self.px.fill_rect(rect, &paint, transform, None);
    }

    fn all(&self) -> Rect {
        Rect::from_xywh(0.0, 0.0, self.s, self.s).expect("a square")
    }

    fn fill(&mut self, color: u32) {
        self.px.fill(rgba(hex(color), 1.0));
    }

    fn lin(&mut self, stops: &[(f32, u32)]) {
        let stops = stops.iter().map(|&(o, c)| GradientStop::new(o, rgba(hex(c), 1.0))).collect();
        if let Some(shader) = LinearGradient::new(Point::from_xy(0.0, 0.0), Point::from_xy(self.s, self.s), stops, SpreadMode::Pad, Transform::identity()) {
            let all = self.all();
            self.paint(shader, BlendMode::SourceOver, all, Transform::identity());
        }
    }

    /// a soft round patch of colour that fades to nothing at `r` (all in fractions of the side).
    fn blob(&mut self, x: f32, y: f32, r: f32, col: Rgb, a: f32) {
        let s = self.s;
        let stops = vec![
            GradientStop::new(0.0, rgba(col, a)),
            GradientStop::new(0.35, rgba(col, a * 0.62)),
            GradientStop::new(0.7, rgba(col, a * 0.2)),
            GradientStop::new(1.0, rgba(col, 0.0)),
        ];
        let at = Point::from_xy(x * s, y * s);
        if let Some(shader) = RadialGradient::new(at, at, r * s, stops, SpreadMode::Pad, Transform::identity()) {
            let all = self.all();
            self.paint(shader, BlendMode::SourceOver, all, Transform::identity());
        }
    }

    /// film grain from a seeded noise tile, so a style looks the same every time it's drawn.
    fn grain(&mut self, seed: u32, amount: f32) {
        let s = self.s;
        let n = ((if s > 500.0 { s / 2.0 } else { s }).round() as u32).max(64);
        let Some(mut tile) = Pixmap::new(n, n) else { return };
        let mut rng = Mulberry(seed);
        for p in tile.data_mut().chunks_exact_mut(4) {
            let v = if rng.next() > 0.5 { 255.0 } else { 0.0 };
            let alpha = rng.next();
            let pre = (v * alpha).round() as u8;
            p.copy_from_slice(&[pre, pre, pre, (alpha * 255.0).round() as u8]);
        }
        let scale = s / n as f32;
        let shader = Pattern::new(tile.as_ref(), SpreadMode::Pad, FilterQuality::Bilinear, amount * 0.5, Transform::from_scale(scale, scale));
        let all = self.all();
        self.paint(shader, BlendMode::Overlay, all, Transform::identity());
    }
}

/// mulberry32, so the grain matches the canvas renderer's.
struct Mulberry(u32);

impl Mulberry {
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_add(0x6D2B_79F5);
        let mut t = (self.0 ^ (self.0 >> 15)).wrapping_mul(1 | self.0);
        t = t.wrapping_add((t ^ (t >> 7)).wrapping_mul(61 | t)) ^ t;
        ((t ^ (t >> 14)) as f64 / 4_294_967_296.0) as f32
    }
}

fn draw_art(c: &mut Canvas, id: &str) -> bool {
    match id {
        "ember" => {
            c.fill(0xb8300d);
            c.blob(0.15, 0.1, 0.8, [255, 160, 30], 0.9);
            c.blob(0.85, 0.1, 0.6, [200, 20, 70], 0.9);
            c.blob(0.75, 0.95, 0.8, [255, 120, 90], 0.85);
            c.blob(0.35, 0.85, 0.5, [255, 225, 185], 0.75);
            c.grain(1, 0.18);
        }
        "lagoon" => {
            c.fill(0x04202b);
            c.blob(0.2, 0.9, 0.8, [20, 170, 170], 0.9);
            c.blob(0.95, 0.35, 0.7, [30, 90, 210], 0.85);
            c.blob(0.55, 0.62, 0.35, [130, 245, 205], 0.55);
            c.grain(2, 0.25);
        }
        "dusk" => {
            c.fill(0x3a2a72);
            c.blob(0.9, 0.2, 0.7, [150, 90, 235], 0.9);
            c.blob(0.2, 0.75, 0.75, [255, 115, 170], 0.9);
            c.blob(0.7, 1.0, 0.6, [255, 195, 140], 0.95);
            c.grain(3, 0.2);
        }
        "paper-sun" => {
            c.fill(0xf3eadc);
            c.blob(0.68, 0.66, 0.62, [255, 190, 125], 0.9);
            c.blob(0.68, 0.66, 0.36, [255, 105, 45], 1.0);
            c.blob(0.1, 1.0, 0.5, [255, 215, 190], 0.6);
            c.grain(4, 0.22);
        }
        "moss" => {
            c.lin(&[(0.0, 0x0d2619), (0.55, 0x4f8a3c), (1.0, 0xc3e68a)]);
            c.blob(0.8, 0.2, 0.5, [20, 60, 35], 0.7);
            c.blob(0.3, 0.85, 0.4, [225, 250, 170], 0.55);
            c.grain(6, 0.45);
        }
        "glacier" => {
            c.fill(0xeef6fc);
            c.blob(0.9, 0.95, 0.85, [70, 125, 225], 0.9);
            c.blob(0.3, 0.7, 0.6, [135, 200, 245], 0.8);
            c.blob(0.05, 0.1, 0.5, [255, 255, 255], 1.0);
            c.grain(8, 0.14);
        }
        "ink" => {
            c.fill(0x09090b);
            c.blob(0.82, 0.9, 0.7, [255, 75, 55], 0.95);
            c.blob(0.9, 0.8, 0.3, [255, 190, 110], 0.8);
            c.grain(10, 0.35);
        }
        "blush" => {
            c.fill(0xfbe4dd);
            c.blob(0.25, 1.02, 0.75, [255, 150, 145], 0.95);
            c.blob(0.9, 0.9, 0.65, [240, 110, 140], 0.85);
            c.blob(0.6, 0.55, 0.4, [255, 215, 180], 0.85);
            c.blob(0.95, 0.1, 0.45, [255, 255, 255], 0.9);
            c.grain(12, 0.14);
        }
        "slate" => {
            c.lin(&[(0.0, 0x3c485a), (1.0, 0x1f2632)]);
            c.blob(0.72, 0.72, 0.5, [195, 212, 238], 0.7);
            c.blob(0.72, 0.72, 0.22, [235, 243, 255], 0.6);
            c.grain(16, 0.3);
        }
        _ => return false,
    }
    true
}

fn luminance(r: u8, g: u8, b: u8) -> f32 {
    let f = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b)
}

/// white or near-black, whichever contrasts more with the art where the title sits.
fn ink(c: &Canvas) -> Color {
    let s = c.s;
    let (x, y, w, h) = ((s * 0.06).round() as usize, (s * 0.06).round() as usize, (s * 0.8).round() as usize, (s * 0.3).round() as usize);
    let side = c.px.width() as usize;
    let data = c.px.data();
    let (mut total, mut n) = (0.0, 0);
    for row in y..(y + h).min(side) {
        for col in (x..(x + w).min(side)).step_by(7) {
            let i = (row * side + col) * 4;
            total += luminance(data[i], data[i + 1], data[i + 2]);
            n += 1;
        }
    }
    let l = if n > 0 { total / n as f32 } else { 0.0 };
    if 1.05 / (l + 0.05) >= (l + 0.05) / 0.055 { Color::WHITE } else { rgba([24, 24, 28], 0.88) }
}

struct Outline<'a> {
    path: &'a mut PathBuilder,
    scale: f32,
    x: f32,
    y: f32,
}

impl OutlineBuilder for Outline<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.path.move_to(self.x + x * self.scale, self.y - y * self.scale);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.path.line_to(self.x + x * self.scale, self.y - y * self.scale);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.path.quad_to(self.x + x1 * self.scale, self.y - y1 * self.scale, self.x + x * self.scale, self.y - y * self.scale);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.path.cubic_to(self.x + x1 * self.scale, self.y - y1 * self.scale, self.x + x2 * self.scale, self.y - y2 * self.scale, self.x + x * self.scale, self.y - y * self.scale);
    }
    fn close(&mut self) {
        self.path.close();
    }
}

fn face() -> Option<&'static Face<'static>> {
    static FACE: OnceLock<Option<Face<'static>>> = OnceLock::new();
    FACE.get_or_init(|| Face::parse(TITLE_FONT, 0).ok()).as_ref()
}

/// the text's width at `size`, or `None` when the face lacks one of its characters.
fn measure(face: &Face, text: &str, size: f32) -> Option<f32> {
    let scale = size / face.units_per_em() as f32;
    text.chars().try_fold(0.0, |w, ch| {
        let glyph = face.glyph_index(ch)?;
        Some(w + face.glyph_hor_advance(glyph)? as f32 * scale)
    })
}

fn wrap(face: &Face, text: &str, size: f32, max_w: f32) -> Option<Vec<String>> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let joined = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
        if cur.is_empty() || measure(face, &joined, size)? <= max_w {
            cur = joined;
        } else {
            lines.push(std::mem::replace(&mut cur, word.to_string()));
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    Some(lines)
}

/// the title at the top left, as large as fits in two lines, then smaller, then cut with an ellipsis. a title with
/// characters the face lacks is left off, so the cover never shows empty boxes.
fn draw_title(px: &mut Pixmap, text: &str, color: Color) {
    let Some(face) = face() else { return };
    let text = text.trim();
    if text.is_empty() || measure(face, text, 1.0).is_none() {
        return;
    }
    let s = px.width() as f32;
    let (pad, step, smallest) = (s * 0.085, s * 0.005, s * 0.07);
    let max_w = s - pad * 2.0;
    let fits = |lines: &[String], size: f32| lines.len() <= 2 && lines.iter().all(|l| measure(face, l, size).is_some_and(|w| w <= max_w));
    let mut size = s * 0.13;
    let mut lines = wrap(face, text, size, max_w).unwrap_or_default();
    while !fits(&lines, size) && size - step >= smallest {
        size -= step;
        lines = wrap(face, text, size, max_w).unwrap_or_default();
    }
    if lines.len() > 2 {
        lines.truncate(2);
        lines[1].push('…');
    }
    for line in &mut lines {
        while measure(face, line, size).is_some_and(|w| w > max_w) && line.chars().count() > 1 {
            let keep = line.chars().count().saturating_sub(2);
            *line = line.chars().take(keep).collect::<String>() + "…";
        }
    }
    let scale = size / face.units_per_em() as f32;
    // the em box sits with its top at the line's top, as a canvas does with a `top` baseline.
    let (ascent, descent) = (face.ascender() as f32, face.descender() as f32);
    let rise = size * ascent / (ascent - descent);
    let mut path = PathBuilder::new();
    for (i, line) in lines.iter().enumerate() {
        let mut x = pad;
        let y = pad + i as f32 * size * 1.08 + rise;
        for ch in line.chars() {
            let Some(glyph) = face.glyph_index(ch) else { continue };
            face.outline_glyph(glyph, &mut Outline { path: &mut path, scale, x, y });
            x += face.glyph_hor_advance(glyph).unwrap_or(0) as f32 * scale;
        }
    }
    if let Some(path) = path.finish() {
        let paint = Paint { shader: Shader::SolidColor(color), anti_alias: true, ..Paint::default() };
        px.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    }
}

struct Art {
    px: Pixmap,
    ink: Color,
}

/// each style's art without a title, per size, with the title colour it takes; typing only redraws the title.
fn art(id: &str, size: u32) -> Option<(Pixmap, Color)> {
    static CACHE: OnceLock<Mutex<HashMap<(String, u32), Art>>> = OnceLock::new();
    let key = (id.to_string(), size);
    let mut held = CACHE.get_or_init(Default::default).lock().ok()?;
    if let Some(hit) = held.get(&key) {
        return Some((hit.px.clone(), hit.ink));
    }
    let mut canvas = Canvas::new(size)?;
    if !draw_art(&mut canvas, id) {
        return None;
    }
    let ink = ink(&canvas);
    // the upload size is drawn once per playlist; only the sizes shown while typing are worth keeping.
    if size < COVER_EXPORT_PX {
        held.insert(key, Art { px: canvas.px.clone(), ink });
    }
    Some((canvas.px, ink))
}

/// `style` with `title`, `size` pixels square, as straight RGBA bytes. `None` for an unknown style or a zero size.
pub fn render_cover_rgba(style: &str, title: &str, size: u32) -> Option<Vec<u8>> {
    let (mut px, ink) = art(style, size)?;
    draw_title(&mut px, title, ink);
    // every style paints the whole square opaque, so premultiplied and straight bytes are the same.
    Some(px.take())
}

/// `style` with `title`, `size` pixels square, as a PNG.
pub fn render_cover_png(style: &str, title: &str, size: u32) -> Option<Vec<u8>> {
    let rgba = render_cover_rgba(style, title, size)?;
    let image = image::RgbaImage::from_raw(size, size, rgba)?;
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image).to_rgb8().write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_style_renders_at_its_size() {
        for style in COVER_STYLES {
            let rgba = render_cover_rgba(style.id, "Road Trip", 200).expect(style.id);
            assert_eq!(rgba.len(), 200 * 200 * 4, "{}", style.id);
            assert!(rgba.chunks_exact(4).all(|p| p[3] == 255), "{} is opaque", style.id);
            let first = &rgba[..4];
            assert!(rgba.chunks_exact(4).any(|p| p != first), "{} is not one flat colour", style.id);
        }
    }

    #[test]
    fn png_has_the_asked_size() {
        let png = render_cover_png("dusk", "Road Trip", 320).unwrap();
        let image = image::load_from_memory(&png).unwrap();
        assert_eq!((image.width(), image.height()), (320, 320));
    }

    #[test]
    fn same_input_same_pixels() {
        assert_eq!(render_cover_rgba("moss", "Late Night", 128), render_cover_rgba("moss", "Late Night", 128));
    }

    #[test]
    fn the_title_changes_the_picture() {
        assert_ne!(render_cover_rgba("ink", "", 200), render_cover_rgba("ink", "Road Trip", 200));
        // no glyphs for these in the face: the art is drawn without a title.
        assert_eq!(render_cover_rgba("ink", "", 200), render_cover_rgba("ink", "夜のドライブ", 200));
    }

    #[test]
    fn a_long_title_stays_inside() {
        let size = 300;
        let plain = render_cover_rgba("slate", "", size).unwrap();
        let long = render_cover_rgba("slate", "Songs for a very long drive across the whole country and back again", size).unwrap();
        let changed_below = |from: usize| (from..size as usize).any(|y| (0..size as usize).any(|x| {
            let i = (y * size as usize + x) * 4;
            plain[i..i + 4] != long[i..i + 4]
        }));
        assert!(changed_below(0));
        assert!(!changed_below(size as usize / 2), "two lines at most");
    }

    #[test]
    fn unknown_style_is_none() {
        assert!(render_cover_rgba("nope", "x", 100).is_none());
        assert!(render_cover_rgba("ember", "x", 0).is_none());
    }
}
