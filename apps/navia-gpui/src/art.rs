//! Artwork processing on background threads. Thumbnails, dock artwork with its pre-blurred
//! reflection, and tinted backgrounds, since GPUI has no blur or masks.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};

use fast_image_resize::images::Image;
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
use futures::channel::mpsc::UnboundedSender;
use gpui::{RenderImage, SharedString};
use image::{Frame, ImageBuffer, Rgba, RgbaImage};

use crate::color::{Srgb, player_vivid, tint_bar_glow};
use crate::theme::size::BAR_GLOW_ALPHA;
use crate::model::{Library, PlaylistCover};

pub type Rgba16 = ImageBuffer<Rgba<u16>, Vec<u16>>;

pub fn resize_rgba(src: &RgbaImage, w: u32, h: u32, filter: FilterType) -> RgbaImage {
    let source = Image::from_vec_u8(src.width(), src.height(), src.as_raw().clone(), PixelType::U8x4).unwrap();
    let mut target = Image::new(w.max(1), h.max(1), PixelType::U8x4);
    Resizer::new()
        .resize(&source, &mut target, &ResizeOptions::new().resize_alg(ResizeAlg::Convolution(filter)))
        .unwrap();
    RgbaImage::from_raw(w.max(1), h.max(1), target.into_vec()).unwrap()
}

fn resize_rgba16(src: &Rgba16, w: u32, h: u32, filter: FilterType) -> Rgba16 {
    let bytes: Vec<u8> = src.as_raw().iter().flat_map(|v| v.to_ne_bytes()).collect();
    let source = Image::from_vec_u8(src.width(), src.height(), bytes, PixelType::U16x4).unwrap();
    let mut target = Image::new(w.max(1), h.max(1), PixelType::U16x4);
    Resizer::new()
        .resize(&source, &mut target, &ResizeOptions::new().resize_alg(ResizeAlg::Convolution(filter)))
        .unwrap();
    let out = target.into_vec().chunks_exact(2).map(|b| u16::from_ne_bytes([b[0], b[1]])).collect();
    ImageBuffer::from_raw(w.max(1), h.max(1), out).unwrap()
}

/// Converts straight-alpha RGBA to the BGRA GPUI wants.
pub fn into_render_image(mut rgba: RgbaImage) -> Arc<RenderImage> {
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    Arc::new(RenderImage::new(vec![Frame::new(rgba)]))
}

/// A thumbnail with the same center crop as CSS `object-fit: cover`.
pub fn thumbnail(src: &RgbaImage, size: u32) -> Arc<RenderImage> {
    let (w, h) = src.dimensions();
    let side = w.min(h).max(1);
    let (x, y) = ((w - side) / 2, (h - side) / 2);
    let crop = image::imageops::crop_imm(src, x, y, side, side).to_image();
    into_render_image(resize_rgba(&crop, size, size, FilterType::Lanczos3))
}

pub struct Rng(u32);

impl Rng {
    pub fn new() -> Self {
        Self(0x9e37_79b9)
    }
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 >> 16) as f32 / 65535.0
    }
    /// Triangular noise of about one 8-bit step.
    pub fn tri(&mut self) -> f32 {
        self.next() - self.next()
    }
}

/// The cover's tint from the core's `dominant_tint`, so every platform takes the same color
/// from a cover.
pub fn tint_of(src: &RgbaImage) -> Srgb {
    let [r, g, b] = navia_core::artwork::dominant_tint(src);
    Srgb { r: f32::from(r) / 255.0, g: f32::from(g) / 255.0, b: f32::from(b) / 255.0 }
}

/// Sizes for one dock bake, in device pixels.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct DockGeometry {
    pub width: u32,
    pub height: u32,
    pub art_h: u32,
    pub reflect_top: u32,
    pub reflect_h: u32,
    /// Device pixels per logical pixel.
    pub blur_px: u32,
}

/// One album's dock images.
pub struct DockBake {
    pub geometry: DockGeometry,
    /// The gradient the dock sits on, dithered.
    pub background: Arc<RenderImage>,
    /// Artwork with its fade and the blurred reflection below, in straight alpha.
    pub art_stack: Arc<RenderImage>,
}

fn cover_crop(src: &RgbaImage, w: u32, h: u32, anchor_bottom: bool) -> RgbaImage {
    let (sw, sh) = (src.width() as f32, src.height() as f32);
    let scale = (w as f32 / sw).max(h as f32 / sh);
    let (rw, rh) = ((sw * scale).ceil() as u32, (sh * scale).ceil() as u32);
    let resized = resize_rgba(src, rw, rh, FilterType::Lanczos3);
    let x = (rw - w.min(rw)) / 2;
    let y = if anchor_bottom { rh - h.min(rh) } else { 0 };
    image::imageops::crop_imm(&resized, x, y, w, h).to_image()
}

fn saturate(px: [f32; 3], amount: f32) -> [f32; 3] {
    let luma = 0.2126 * px[0] + 0.7152 * px[1] + 0.0722 * px[2];
    [luma + (px[0] - luma) * amount, luma + (px[1] - luma) * amount, luma + (px[2] - luma) * amount]
}

/// A vertical gradient as a dithered opaque sprite. A one-row image stretched by GPUI would
/// band, because the dither can't survive scaling.
fn vertical_gradient(w: u32, h: u32, stops: &[(f32, Srgb)]) -> Arc<RenderImage> {
    let mut rng = Rng::new();
    let mut out = RgbaImage::new(w.max(1), h.max(1));
    for y in 0..h.max(1) {
        let t = y as f32 / (h.max(2) - 1) as f32;
        let color = sample_stops(stops, t);
        for x in 0..w.max(1) {
            let n = rng.tri();
            let c = |v: f32| (v * 255.0 + n).round().clamp(0.0, 255.0) as u8;
            out.put_pixel(x, y, Rgba([c(color.r), c(color.g), c(color.b), 255]));
        }
    }
    into_render_image(out)
}

fn sample_stops(stops: &[(f32, Srgb)], t: f32) -> Srgb {
    if t <= stops[0].0 {
        return stops[0].1;
    }
    for pair in stops.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if t <= b.0 {
            let local = ((t - a.0) / (b.0 - a.0)).clamp(0.0, 1.0);
            return b.1.mix(a.1, local);
        }
    }
    stops[stops.len() - 1].1
}

/// Bakes the dock background and the artwork stack, the art with its bottom fade over the
/// blurred reflection. Everything is at device size so nothing scales at draw time.
pub fn bake_dock(src: &RgbaImage, g: DockGeometry, tint: Srgb) -> DockBake {
    let vivid = player_vivid(tint);
    let bottom = vivid.mix(Srgb::hex(crate::theme::DOCK_SHADE), crate::theme::PLAYER_SHADE_MIX);
    // The vivid tint down to 35%, then mixed toward the dark end at the bottom.
    let background = vertical_gradient(g.width, g.height, &[(0.0, vivid), (0.35, vivid), (1.0, bottom)]);

    let stack_h = (g.reflect_top + g.reflect_h).max(g.art_h);
    let (w, h) = (g.width as usize, stack_h as usize);
    // Premultiplied RGBA in floats, so the blend adds no rounding of its own.
    let mut buf = vec![[0f32; 4]; w * h];

    // The reflection is the bottom of the cover, flipped and blurred at low resolution.
    let reflect_src = cover_crop(src, g.width, g.reflect_h, true);
    let small_w = (g.width / 4).max(8);
    let small_h = (g.reflect_h / 4).max(8);
    let small = resize_rgba(&reflect_src, small_w, small_h, FilterType::Bilinear);
    let small16: Rgba16 = ImageBuffer::from_fn(small_w, small_h, |x, y| {
        let p = small.get_pixel(x, small_h - 1 - y).0;
        Rgba(p.map(|c| u16::from(c) * 257))
    });
    let blurred = image::imageops::blur(&small16, g.blur_px as f32 / 4.0);
    let upscaled = resize_rgba16(&blurred, g.width, g.reflect_h, FilterType::CatmullRom);
    // Mask the reflection, opaque at its top and gone by its bottom.
    let fade = |y: f32| -> f32 {
        let t = 1.0 - y / g.reflect_h as f32;
        ((t - 0.0) / 0.8).clamp(0.0, 1.0)
    };
    for y in 0..g.reflect_h as usize {
        let dest_y = g.reflect_top as usize + y;
        if dest_y >= h {
            break;
        }
        let m = fade(y as f32) * crate::theme::size::REFLECT_OPACITY;
        for x in 0..w {
            let p = upscaled.get_pixel(x as u32, y as u32).0;
            let [r, g2, b] = saturate(
                [f32::from(p[0]) / 65535.0, f32::from(p[1]) / 65535.0, f32::from(p[2]) / 65535.0],
                crate::theme::size::REFLECT_SATURATION,
            );
            buf[dest_y * w + x] = [r.clamp(0.0, 1.0) * m, g2.clamp(0.0, 1.0) * m, b.clamp(0.0, 1.0) * m, m];
        }
    }

    // Artwork on top, faded out over its last 20%.
    let art = cover_crop(src, g.width, g.art_h, false);
    let art_h = g.art_h as usize;
    for y in 0..art_h.min(h) {
        let t = y as f32 / g.art_h as f32;
        let m = if t < 0.8 { 1.0 } else { ((1.0 - t) / 0.2).clamp(0.0, 1.0) };
        for x in 0..w {
            let p = art.get_pixel(x as u32, y as u32).0;
            let sa = m * f32::from(p[3]) / 255.0;
            let src_px = [f32::from(p[0]) / 255.0 * sa, f32::from(p[1]) / 255.0 * sa, f32::from(p[2]) / 255.0 * sa, sa];
            let d = &mut buf[y * w + x];
            for ch in 0..4 {
                d[ch] = src_px[ch] + d[ch] * (1.0 - sa);
            }
        }
    }

    // Dither down to 8 bits, and un-premultiply for GPUI's straight-alpha sprites.
    let mut rng = Rng::new();
    let mut out = RgbaImage::new(g.width, stack_h);
    for y in 0..h {
        for x in 0..w {
            let d = buf[y * w + x];
            let a = d[3];
            if a <= 0.001 {
                continue;
            }
            let n = rng.tri();
            let c = |v: f32| ((v / a).clamp(0.0, 1.0) * 255.0 + n).round().clamp(0.0, 255.0) as u8;
            let alpha = (a * 255.0 + n).round().clamp(0.0, 255.0) as u8;
            out.put_pixel(x as u32, y as u32, Rgba([c(d[0]), c(d[1]), c(d[2]), alpha]));
        }
    }
    DockBake { geometry: g, background, art_stack: into_render_image(out) }
}

/// The brightest luminance in each cell of a baked backdrop, so the expanded player can scrim
/// its text only as much as the pixels underneath need.
#[derive(Debug, Default)]
pub struct PeakGrid {
    pub cell: u32,
    pub cols: u32,
    pub rows: u32,
    pub peaks: Vec<f32>,
}

impl PeakGrid {
    pub const CELL: u32 = 8;

    /// The brightest cell touching the rectangle, in the backdrop's device pixels.
    pub fn max_in(&self, x0: f32, y0: f32, x1: f32, y1: f32) -> f32 {
        if self.cols == 0 || self.rows == 0 || x1 <= x0 || y1 <= y0 {
            return 0.0;
        }
        let c = self.cell as f32;
        let (cx0, cy0) = ((x0 / c).floor().max(0.0) as u32, (y0 / c).floor().max(0.0) as u32);
        let (cx1, cy1) = (((x1 / c).ceil() as u32).min(self.cols), ((y1 / c).ceil() as u32).min(self.rows));
        let mut peak = 0f32;
        for y in cy0..cy1 {
            for x in cx0..cx1 {
                peak = peak.max(self.peaks[(y * self.cols + x) as usize]);
            }
        }
        peak
    }
}

/// The expanded player's backdrop, a diagonal gradient plus a blurred, tinted copy of the art,
/// and its peak grid. `blur_px` is `theme::PLAYER_GLOW_BLUR` in device pixels.
pub fn bake_expanded(src: &RgbaImage, width: u32, height: u32, tint: Srgb, blur_px: f32) -> (Arc<RenderImage>, Arc<PeakGrid>) {
    let vivid = player_vivid(tint);
    let c0 = vivid.mix(Srgb::hex(crate::theme::PLAYER_SHADE_START), crate::theme::PLAYER_SHADE_MIX);
    let c2 = vivid.mix(Srgb::hex(crate::theme::PLAYER_SHADE_END), 0.58);
    // The glow is the cover filling a centered box 140% of the window, of which the window shows
    // the middle. A small copy of that middle plus room for the blur is blurred by the same
    // deviation, then the window's part is scaled up.
    let (w, h) = (width.max(1) as f32, height.max(1) as f32);
    let side = 1.4 * w.max(h);
    let (sw, sh) = (src.width().max(1) as f32, src.height().max(1) as f32);
    let cover = (side / sw).max(side / sh);
    let (small_w, small_h) = (96u32, ((96.0 * h / w).round() as u32).max(1));
    let per_small = w / small_w as f32;
    let sigma = (blur_px / per_small).max(0.5);
    let margin = (2.0 * sigma).ceil() as u32;
    let (grid_w, grid_h) = (small_w + margin * 2, small_h + margin * 2);
    // Each small pixel samples a bilinear copy of the cover, clamped at its edges.
    let src_small = resize_rgba(src, 128, 128, FilterType::Bilinear);
    let to_src = |gx: u32, gy: u32| {
        let wx = (gx as f32 - margin as f32 + 0.5) * per_small - w / 2.0;
        let wy = (gy as f32 - margin as f32 + 0.5) * per_small - h / 2.0;
        let (u, v) = ((wx / cover + sw / 2.0) / sw, (wy / cover + sh / 2.0) / sh);
        let (x, y) = ((u * 128.0).clamp(0.0, 127.0) as u32, (v * 128.0).clamp(0.0, 127.0) as u32);
        src_small.get_pixel(x, y).0
    };
    let small16: Rgba16 = ImageBuffer::from_fn(grid_w, grid_h, |x, y| {
        let p = to_src(x, y);
        let [r, g, b] = saturate(
            [f32::from(p[0]) / 255.0, f32::from(p[1]) / 255.0, f32::from(p[2]) / 255.0],
            crate::theme::size::PLAYER_GLOW_SATURATION,
        );
        Rgba([r, g, b, 1.0].map(|c: f32| (c * crate::theme::size::PLAYER_GLOW_BRIGHTNESS).clamp(0.0, 1.0)).map(|c| (c * 65535.0) as u16))
    });
    let blurred = image::imageops::blur(&small16, sigma);
    let inner = image::imageops::crop_imm(&blurred, margin, margin, small_w, small_h).to_image();
    let glow = resize_rgba16(&inner, width, height, FilterType::CatmullRom);
    let mut rng = Rng::new();
    let mut out = RgbaImage::new(width.max(1), height.max(1));
    let (fw, fh) = (width.max(2) as f32 - 1.0, height.max(2) as f32 - 1.0);
    let cell = PeakGrid::CELL;
    let (cols, rows) = (width.div_ceil(cell), height.div_ceil(cell));
    let mut peaks = vec![0f32; (cols * rows) as usize];
    for y in 0..height {
        for x in 0..width {
            // 135 degree gradient with stops at 0%, 42% and 100%.
            let t = (x as f32 / fw + y as f32 / fh) / 2.0;
            let base = if t < 0.42 { vivid.mix(c0, t / 0.42) } else { c2.mix(vivid, (t - 0.42) / 0.58) };
            let g = glow.get_pixel(x, y).0;
            let n = rng.tri();
            let m = crate::theme::size::PLAYER_GLOW_OPACITY;
            let c = |b: f32, g: u16| ((b * (1.0 - m) + f32::from(g) / 65535.0 * m) * 255.0 + n).round().clamp(0.0, 255.0) as u8;
            let px = [c(base.r, g[0]), c(base.g, g[1]), c(base.b, g[2])];
            let lum = Srgb { r: f32::from(px[0]) / 255.0, g: f32::from(px[1]) / 255.0, b: f32::from(px[2]) / 255.0 }.luminance();
            let slot = &mut peaks[((y / cell) * cols + x / cell) as usize];
            *slot = slot.max(lum);
            out.put_pixel(x, y, Rgba([px[0], px[1], px[2], 255]));
        }
    }
    (into_render_image(out), Arc::new(PeakGrid { cell, cols, rows, peaks }))
}

/// Soft colored light below the bottom bar, an ellipse at the bottom edge baked as an alpha sprite.
pub fn bake_bar_glow(w: u32, h: u32, tint: Srgb) -> Arc<RenderImage> {
    let glow = tint_bar_glow(tint);
    let mut rng = Rng::new();
    let mut out = RgbaImage::new(w.max(1), h.max(1));
    let (cx, cy) = (w as f32 / 2.0, h as f32);
    let (rx, ry) = (w as f32 * 0.52, h as f32);
    for y in 0..h {
        for x in 0..w {
            let d = (((x as f32 - cx) / rx).powi(2) + ((y as f32 - cy) / ry).powi(2)).sqrt();
            let a = (1.0 - d).clamp(0.0, 1.0) * BAR_GLOW_ALPHA;
            if a <= 0.0 {
                continue;
            }
            let n = rng.tri();
            let alpha = (a * 255.0 + n * 0.5).round().clamp(0.0, 255.0) as u8;
            out.put_pixel(x, y, Rgba([(glow.r * 255.0) as u8, (glow.g * 255.0) as u8, (glow.b * 255.0) as u8, alpha]));
        }
    }
    into_render_image(out)
}

/// The first `MAX_COLLAGE` distinct values, in order.
pub fn first_distinct(items: impl IntoIterator<Item = SharedString>) -> Vec<SharedString> {
    let mut out: Vec<SharedString> = Vec::with_capacity(MAX_COLLAGE);
    for item in items {
        if !out.contains(&item) {
            out.push(item);
            if out.len() == MAX_COLLAGE {
                break;
            }
        }
    }
    out
}

const MAX_COLLAGE: usize = 4;

/// Which covers fill the grid's four cells, the way Navidrome tiles its own. Reading order,
/// with leftover cells repeated so none is empty. A single cover fills the square.
fn collage_cells(count: usize) -> &'static [usize] {
    match count {
        0 => &[],
        1 => &[0],
        2 => &[0, 1, 1, 0],
        3 => &[0, 1, 2, 0],
        _ => &[0, 1, 2, 3],
    }
}

/// A 2x2 grid of covers, or one cover filling the square when there's only one.
fn collage(tiles: &[RgbaImage], size: u32) -> RgbaImage {
    collage_fill(tiles, size, size, crop_fill)
}

/// the collage stretched to `width` by `height`, each cell cropped by `crop` to fill its share.
fn collage_fill(tiles: &[RgbaImage], width: u32, height: u32, crop: impl Fn(&RgbaImage, u32, u32) -> RgbaImage) -> RgbaImage {
    let (width, height) = (width.max(1), height.max(1));
    let cells = collage_cells(tiles.len());
    let mut out = RgbaImage::from_pixel(width, height, Rgba([0x26, 0x26, 0x28, 255]));
    if let [only] = cells {
        let fill = crop(&tiles[*only], width, height);
        image::imageops::replace(&mut out, &fill, 0, 0);
        return out;
    }
    let (half_w, half_h) = (width / 2, height / 2);
    for (position, tile) in cells.iter().enumerate() {
        let (column, row) = (position as u32 % 2, position as u32 / 2);
        let cell_w = if column == 0 { half_w } else { width - half_w };
        let cell_h = if row == 0 { half_h } else { height - half_h };
        let cell = crop(&tiles[*tile], cell_w.max(1), cell_h.max(1));
        image::imageops::replace(&mut out, &cell, i64::from(column * half_w), i64::from(row * half_h));
    }
    out
}

/// Center-crops `src` to the aspect of `width` by `height` and scales it to fit exactly.
fn crop_fill(src: &RgbaImage, width: u32, height: u32) -> RgbaImage {
    let (sw, sh) = (src.width().max(1), src.height().max(1));
    let scale = (width as f32 / sw as f32).max(height as f32 / sh as f32);
    let (cw, ch) = (((width as f32 / scale) as u32).clamp(1, sw), ((height as f32 / scale) as u32).clamp(1, sh));
    let (x, y) = ((sw - cw) / 2, (sh - ch) / 2);
    let cropped = image::imageops::crop_imm(src, x, y, cw, ch).to_image();
    image::imageops::resize(&cropped, width, height, image::imageops::FilterType::Triangle)
}

/// The cover a playlist shows. The server's, a grid from its albums, or `None` for the
/// plain placeholder.
fn make_playlist_cover(library: &dyn Library, id: &str, size: u32) -> Option<Arc<RenderImage>> {
    match playlist_cover_pixels(library, id, size)? {
        CoverPixels::Server(art) => Some(thumbnail(&art, size)),
        CoverPixels::Grid(grid) => Some(into_render_image(grid)),
    }
}

enum CoverPixels {
    Server(RgbaImage),
    Grid(RgbaImage),
}

impl CoverPixels {
    fn image(&self) -> &RgbaImage {
        match self {
            Self::Server(image) | Self::Grid(image) => image,
        }
    }
}

/// The pixels a playlist's cover shows, for a thumbnail or its page tint.
fn playlist_cover_pixels(library: &dyn Library, id: &str, size: u32) -> Option<CoverPixels> {
    match library.playlist_cover(id, size) {
        PlaylistCover::Server(bytes) => image::load_from_memory(&bytes).ok().map(|i| CoverPixels::Server(i.into_rgba8())),
        PlaylistCover::Albums(albums) => {
            let cell = (size / 2).max(1);
            let tiles: Vec<RgbaImage> = albums.iter().filter_map(|a| library.album_thumbnail(a, cell)).filter_map(|bytes| image::load_from_memory(&bytes).ok()).map(|i| i.into_rgba8()).collect();
            (!tiles.is_empty()).then(|| CoverPixels::Grid(collage(&tiles, size)))
        }
        PlaylistCover::Empty => None,
    }
}

/// A cover of albums laid out as a playlist grid, for a mix (`mix:<ids>`). `None` until every
/// album loads, so the thumbnail cache asks again.
fn make_album_grid(library: &dyn Library, ids: &str, size: u32) -> Option<Arc<RenderImage>> {
    album_grid_tiles(library, ids, size).map(|tiles| into_render_image(collage(&tiles, size)))
}

fn album_grid_tiles(library: &dyn Library, ids: &str, size: u32) -> Option<Vec<RgbaImage>> {
    let ids: Vec<&str> = ids.split('|').filter(|s| !s.is_empty()).collect();
    let cell = (size / 2).max(1);
    let tiles: Vec<RgbaImage> = ids.iter().filter_map(|id| library.album_thumbnail(id, cell)).filter_map(|bytes| image::load_from_memory(&bytes).ok()).map(|i| i.into_rgba8()).collect();
    (!tiles.is_empty() && tiles.len() == ids.len()).then_some(tiles)
}

/// what a bake starts from: a cover, a photo, or a playlist's or mix's grid filling `width` by `height`, its cells cropped like a sheet's cover.
fn bake_source(library: &dyn Library, id: &str, width: u32, height: u32) -> Option<RgbaImage> {
    let size = width.max(height);
    let crop = |tile: &RgbaImage, w: u32, h: u32| cover_crop_at(tile, w, h, crate::theme::size::SHEET_COVER_FOCUS_Y);
    if let Some(playlist) = id.strip_prefix("pl:") {
        return match library.playlist_cover(playlist, size) {
            PlaylistCover::Server(bytes) => image::load_from_memory(&bytes).ok().map(|i| i.into_rgba8()),
            PlaylistCover::Albums(albums) => {
                let cell = (size / 2).max(1);
                let tiles: Vec<RgbaImage> = albums.iter().filter_map(|a| library.album_thumbnail(a, cell)).filter_map(|bytes| image::load_from_memory(&bytes).ok()).map(|i| i.into_rgba8()).collect();
                (!tiles.is_empty()).then(|| collage_fill(&tiles, width, height, crop))
            }
            PlaylistCover::Empty => None,
        };
    }
    if let Some(ids) = id.strip_prefix("mix:") {
        return album_grid_tiles(library, ids, size).map(|tiles| collage_fill(&tiles, width, height, crop));
    }
    if id == LIKED_COVER {
        return liked_cover_pixels();
    }
    library.album_artwork(id).and_then(|bytes| image::load_from_memory(&bytes).ok()).map(|i| i.into_rgba8())
}

/// the art key of Navia's own Liked Songs cover, served like any thumbnail through the art cache.
pub const LIKED_COVER: &str = "nv:liked";

/// sized for the page header.
const LIKED_COVER_BYTES: &[u8] = include_bytes!("../assets/covers/liked-songs-512.webp");

fn liked_cover_pixels() -> Option<RgbaImage> {
    image::load_from_memory(LIKED_COVER_BYTES).ok().map(|i| i.into_rgba8())
}

/// the tint of a playlist's current cover, by the album rule; `None` for a placeholder. `key` is from `Shell::playlist_art_key`.
fn playlist_tint(library: &dyn Library, key: &str) -> Option<Srgb> {
    if let Some(ids) = key.strip_prefix("gtint:") {
        return genre_tint(library, ids);
    }
    if key == LIKED_COVER {
        return liked_cover_pixels().map(|pixels| tint_of(&pixels));
    }
    let id = key.strip_prefix("pl:")?;
    playlist_cover_pixels(library, id, 256).map(|pixels| tint_of(pixels.image()))
}

/// The art key of a genre tile's fan. Encodes hover, device pixels per token and the album ids,
/// front first. The thumbnail size requested with it is the tile's width.
pub fn genre_fan_key(hover: bool, px_per_token: f32, album_ids: &[SharedString]) -> SharedString {
    let ids: Vec<&str> = album_ids.iter().map(|s| s.as_ref()).collect();
    format!("gfan:{}:{}:{}", u8::from(hover), (px_per_token * 1000.0).round() as u32, ids.join("|")).into()
}

/// The tint key of a genre tile, for `Job::PlaylistTint`.
pub fn genre_tint_key(album_ids: &[SharedString]) -> SharedString {
    let ids: Vec<&str> = album_ids.iter().map(|s| s.as_ref()).collect();
    format!("gtint:{}", ids.join("|")).into()
}

/// A genre tile's tint, from its covers laid out as a playlist grid.
fn genre_tint(library: &dyn Library, ids: &str) -> Option<Srgb> {
    let ids: Vec<&str> = ids.split('|').filter(|s| !s.is_empty()).collect();
    let tiles: Vec<RgbaImage> = ids.iter().filter_map(|id| library.album_thumbnail(id, 128)).filter_map(|bytes| image::load_from_memory(&bytes).ok()).map(|i| i.into_rgba8()).collect();
    (!tiles.is_empty() && tiles.len() == ids.len()).then(|| tint_of(&collage(&tiles, 256)))
}

/// A genre tile's covers fanned out of its bottom right corner, back card first, on a
/// transparent image the size of the tile. GPUI can't rotate images, so the fan is drawn here.
/// `None` until every cover loads, so the thumbnail cache asks again.
fn make_genre_fan(library: &dyn Library, key: &str, width: u32) -> Option<Arc<RenderImage>> {
    use crate::theme::size::{GENRE_FAN, GENRE_FAN_BOTTOM, GENRE_FAN_HOVER, GENRE_FAN_RIGHT, GENRE_FAN_SINGLE, GENRE_FAN_SIZE, GENRE_TILE_RATIO, R_SM, SHADOW_ART};
    let mut parts = key.strip_prefix("gfan:")?.splitn(3, ':');
    let hover = parts.next()? == "1";
    let scale = parts.next()?.parse::<f32>().ok()? / 1000.0;
    let ids: Vec<&str> = parts.next()?.split('|').filter(|s| !s.is_empty()).collect();
    let (w, h) = (width.max(1), ((width as f32 / GENRE_TILE_RATIO).round() as u32).max(1));
    let side = w as f32 * GENRE_FAN_SIZE;
    let px_side = side.ceil().max(1.0) as u32;
    let covers: Vec<RgbaImage> = ids.iter().filter_map(|id| library.album_thumbnail(id, px_side)).filter_map(|b| image::load_from_memory(&b).ok()).map(|i| crop_fill(&i.into_rgba8(), px_side, px_side)).collect();
    if covers.is_empty() || covers.len() != ids.len() {
        return None;
    }
    // A single cover rests at its own angle and on hover moves like the front card of a fan.
    let cards = match (covers.len(), hover) {
        (_, true) => GENRE_FAN_HOVER,
        (1, false) => [GENRE_FAN_SINGLE; 3],
        _ => GENRE_FAN,
    };
    let (shadow_color, shadow_y, shadow_blur, shadow_spread) = SHADOW_ART;
    let shadow_alpha = (shadow_color & 0xff) as f32 / 255.0;
    let (shadow_y, sigma, spread, radius) = (shadow_y * scale, shadow_blur * scale / 2.0, shadow_spread * scale, R_SM * scale);
    // The front card's box before its transform. `right` and `bottom` are fractions of the tile.
    let (x1, y1) = (w as f32 * (1.0 - GENRE_FAN_RIGHT), h as f32 * (1.0 - GENRE_FAN_BOTTOM));
    // The transform origin is the box's bottom center.
    let origin = (x1 - side / 2.0, y1);
    let (wi, hi) = (w as usize, h as usize);
    // Premultiplied RGBA, so cards and shadows stack with plain "over".
    let mut acc = vec![[0f32; 4]; wi * hi];
    // Signed distance to a rounded box centered at 0 with half size `b`, in pixels.
    let rounded = |qx: f32, qy: f32, b: f32, r: f32| {
        let (dx, dy) = (qx.abs() - b + r, qy.abs() - b + r);
        (dx.max(0.0).hypot(dy.max(0.0))) + dx.max(dy).min(0.0) - r
    };
    for i in (0..covers.len()).rev() {
        let card = cards[i];
        let (sin, cos) = card.deg.to_radians().sin_cos();
        let (tx, ty) = (card.x * side, card.y * side);
        // Maps a tile pixel into the card's own frame, measured from the card's center.
        let local = |px: f32, py: f32| {
            let (dx, dy) = (px - origin.0 - tx, py - origin.1 - ty);
            let (lx, ly) = (dx * cos + dy * sin, -dx * sin + dy * cos);
            (lx, ly + side / 2.0)
        };
        // The shadow is the card's box grown by the spread, moved down in the card's frame and blurred.
        let pad = (sigma * 3.0).ceil() as usize;
        let (mw, mh) = (wi + pad * 2, hi + pad * 2);
        let mut mask = image::GrayImage::new(mw as u32, mh as u32);
        let half = side / 2.0 + spread;
        for my in 0..mh {
            for mx in 0..mw {
                let (lx, ly) = local(mx as f32 - pad as f32 + 0.5, my as f32 - pad as f32 + 0.5);
                let d = rounded(lx, ly - shadow_y, half.max(0.0), (radius + spread).max(0.0));
                let cover = (0.5 - d).clamp(0.0, 1.0);
                if cover > 0.0 {
                    mask.put_pixel(mx as u32, my as u32, image::Luma([(cover * 255.0).round() as u8]));
                }
            }
        }
        let mask = if sigma > 0.3 { image::imageops::blur(&mask, sigma) } else { mask };
        for y in 0..hi {
            for x in 0..wi {
                let m = f32::from(mask.get_pixel((x + pad) as u32, (y + pad) as u32).0[0]) / 255.0 * shadow_alpha;
                if m > 0.0 {
                    let p = &mut acc[y * wi + x];
                    for c in p.iter_mut().take(3) {
                        *c *= 1.0 - m;
                    }
                    p[3] = m + p[3] * (1.0 - m);
                }
            }
        }
        // The card, sampled bilinearly with an antialiased rounded edge.
        let cover = &covers[i];
        let n = cover.width() as f32;
        let k = n / side;
        for y in 0..hi {
            for x in 0..wi {
                let (lx, ly) = local(x as f32 + 0.5, y as f32 + 0.5);
                let a = (0.5 - rounded(lx, ly, side / 2.0, radius)).clamp(0.0, 1.0);
                if a <= 0.0 {
                    continue;
                }
                let (u, v) = (((lx + side / 2.0) * k - 0.5).clamp(0.0, n - 1.0), ((ly + side / 2.0) * k - 0.5).clamp(0.0, n - 1.0));
                let (u0, v0) = (u.floor() as u32, v.floor() as u32);
                let (u1, v1) = ((u0 + 1).min(cover.width() - 1), (v0 + 1).min(cover.height() - 1));
                let (fu, fv) = (u - u0 as f32, v - v0 as f32);
                let p = acc.get_mut(y * wi + x).expect("in bounds");
                for c in 0..3 {
                    let s = |xx: u32, yy: u32| f32::from(cover.get_pixel(xx, yy).0[c]) / 255.0;
                    let top = s(u0, v0) * (1.0 - fu) + s(u1, v0) * fu;
                    let bottom = s(u0, v1) * (1.0 - fu) + s(u1, v1) * fu;
                    p[c] = (top * (1.0 - fv) + bottom * fv) * a + p[c] * (1.0 - a);
                }
                p[3] = a + p[3] * (1.0 - a);
            }
        }
    }
    let mut out = RgbaImage::new(w, h);
    for (i, p) in acc.iter().enumerate() {
        if p[3] <= 0.0 {
            continue;
        }
        let to = |v: f32| (v / p[3] * 255.0).round().clamp(0.0, 255.0) as u8;
        out.put_pixel((i % wi) as u32, (i / wi) as u32, Rgba([to(p[0]), to(p[1]), to(p[2]), (p[3] * 255.0).round().clamp(0.0, 255.0) as u8]));
    }
    Some(into_render_image(out))
}

/// The artist header's photo, `h` device pixels tall and as wide as its aspect, up to `max_w`.
/// A wider photo is cropped to that.
pub fn bake_artist_art(src: &RgbaImage, max_w: u32, h: u32) -> Arc<RenderImage> {
    let h = h.max(1);
    let natural = (src.width() as f32 / src.height().max(1) as f32 * h as f32).round() as u32;
    into_render_image(cover_crop_at(src, natural.clamp(1, max_w.max(1)), h, 0.5))
}

/// A cover sheet's picture filling `w` by `h`. Opaque down to `SHEET_COVER_FADE_FROM` of its
/// height, then fading out into the sheet.
pub fn bake_cover_fade(src: &RgbaImage, w: u32, h: u32) -> Arc<RenderImage> {
    use crate::theme::size::SHEET_COVER_FADE_FROM;
    let mut out = cover_crop_at(src, w.max(1), h.max(1), crate::theme::size::SHEET_COVER_FOCUS_Y);
    let h = out.height();
    for y in 0..h {
        let t = (y as f32 + 0.5) / h as f32;
        let a = if t < SHEET_COVER_FADE_FROM { 1.0 } else { ((1.0 - t) / (1.0 - SHEET_COVER_FADE_FROM)).clamp(0.0, 1.0) };
        for x in 0..out.width() {
            let p = out.get_pixel_mut(x, y);
            p.0[3] = (f32::from(p.0[3]) * a).round() as u8;
        }
    }
    into_render_image(out)
}

/// The soft light from the artist's photo, behind the whole app so it reaches the sidebar.
pub fn bake_page_glow(src: &RgbaImage, w: u32, h: u32) -> Arc<RenderImage> {
    let (w, h) = (w.max(1), h.max(1));
    let cover = cover_crop_at(src, w.min(256).max(16), h.min(160).max(16), 0.3);
    let small = resize_rgba(&cover, 48, 32, FilterType::Bilinear);
    let small16: Rgba16 = ImageBuffer::from_fn(48, 32, |x, y| {
        let p = small.get_pixel(x, y).0;
        let [r, g, b] = saturate([f32::from(p[0]) / 255.0, f32::from(p[1]) / 255.0, f32::from(p[2]) / 255.0], crate::theme::size::PAGE_GLOW_SATURATION);
        Rgba([r, g, b, 1.0].map(|c: f32| (c * crate::theme::size::PAGE_GLOW_BRIGHTNESS).clamp(0.0, 1.0)).map(|c| (c * 65535.0) as u16))
    });
    let blurred = image::imageops::blur(&small16, 6.0);
    let large = resize_rgba16(&blurred, w, h, FilterType::CatmullRom);
    let mut rng = Rng::new();
    let mut out = RgbaImage::new(w, h);
    for y in 0..h {
        let t = y as f32 / (h.max(2) - 1) as f32;
        let mask = if t < 0.45 { 1.0 } else { ((1.0 - t) / 0.55).clamp(0.0, 1.0) };
        for x in 0..w {
            let n = rng.tri();
            let p = large.get_pixel(x, y).0;
            let c = |v: u16| (f32::from(v) / 257.0 + n).round().clamp(0.0, 255.0) as u8;
            let alpha = (crate::theme::size::PAGE_GLOW_OPACITY * mask * 255.0 + n * 0.5).round().clamp(0.0, 255.0) as u8;
            out.put_pixel(x, y, Rgba([c(p[0]), c(p[1]), c(p[2]), alpha]));
        }
    }
    into_render_image(out)
}

fn cover_crop_at(src: &RgbaImage, w: u32, h: u32, anchor_y: f32) -> RgbaImage {
    let (sw, sh) = (src.width() as f32, src.height() as f32);
    let scale = (w as f32 / sw).max(h as f32 / sh);
    let (rw, rh) = ((sw * scale).ceil() as u32, (sh * scale).ceil() as u32);
    let resized = resize_rgba(src, rw, rh, FilterType::Lanczos3);
    let x = (rw - w.min(rw)) / 2;
    let y = (((rh - h.min(rh)) as f32) * anchor_y) as u32;
    image::imageops::crop_imm(&resized, x, y, w, h).to_image()
}

/// Requests served by the worker threads.
pub enum Job {
    Thumb { album_id: SharedString, size: u32 },
    Dock { album_id: SharedString, geometry: DockGeometry },
    Expanded { album_id: SharedString, width: u32, height: u32, blur_px: f32 },
    BarGlow { album_id: SharedString, width: u32, height: u32 },
    Tint { album_id: SharedString },
    PlaylistTint { key: SharedString, generation: u64 },
    Bake { kind: BakeKind, id: SharedString, width: u32, height: u32 },
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum BakeKind {
    /// The artist header's photo, uncropped.
    ArtistArt,
    /// A cover sheet's picture fading out.
    CoverFade,
    PageGlow,
}

pub enum ArtEvent {
    /// `placeholder`: the server's no-cover placeholder, kept only until it expires.
    Thumb { album_id: SharedString, size: u32, image: Option<Arc<RenderImage>>, placeholder: bool },
    Dock { album_id: SharedString, bake: Arc<DockBake> },
    Expanded { album_id: SharedString, width: u32, height: u32, image: Arc<RenderImage>, peaks: Arc<PeakGrid> },
    BarGlow { album_id: SharedString, width: u32, height: u32, image: Arc<RenderImage> },
    Tint { album_id: SharedString, tint: Srgb },
    PlaylistTint { key: SharedString, generation: u64, tint: Option<Srgb> },
    Bake { kind: BakeKind, id: SharedString, width: u32, height: u32, image: Option<Arc<RenderImage>> },
    LyricLayout { key: crate::lyrics::LyricKey, layout: crate::lyrics::LyricLayout },
    LyricLine { key: crate::lyrics::LyricKey, ix: usize, sprite: crate::lyrics::LineSprite },
}

struct Queue {
    jobs: Mutex<VecDeque<Job>>,
    signal: Condvar,
    closed: std::sync::atomic::AtomicBool,
}

/// Decoded covers are shared between dock and thumbnail jobs so a dock re-bake doesn't decode again.
#[derive(Default)]
struct DecodeCache {
    entries: Vec<(SharedString, Arc<RgbaImage>)>,
}

impl DecodeCache {
    const MAX: usize = 2;

    fn get(&self, id: &SharedString) -> Option<Arc<RgbaImage>> {
        self.entries.iter().find(|(k, _)| k == id).map(|(_, v)| v.clone())
    }

    fn put(&mut self, id: SharedString, img: Arc<RgbaImage>) {
        self.entries.retain(|(k, _)| k != &id);
        if self.entries.len() >= Self::MAX {
            self.entries.remove(0);
        }
        self.entries.push((id, img));
    }
}

/// Prefix for thumbnail ids of an image at a URL, such as a friend's avatar.
pub const AVATAR_PREFIX: &str = "avatar:";

/// Worker pool, serving the newest request first so fast scrolling stays responsive.
pub struct ArtWorkers {
    queue: Arc<Queue>,
}

impl ArtWorkers {
    /// A screenful of thumbnails is about 15, and old requests are for rows already scrolled past.
    const MAX_QUEUED: usize = 96;

    pub fn start(library: Arc<dyn Library>, events: UnboundedSender<ArtEvent>) -> Arc<Self> {
        let queue = Arc::new(Queue { jobs: Mutex::new(VecDeque::new()), signal: Condvar::new(), closed: std::sync::atomic::AtomicBool::new(false) });
        let this = Arc::new(Self { queue: queue.clone() });
        let cache = Arc::new(Mutex::new(DecodeCache::default()));
        for _ in 0..3 {
            let (queue, library, events, cache) = (queue.clone(), library.clone(), events.clone(), cache.clone());
            std::thread::Builder::new()
                .name("navia-art".into())
                .spawn(move || worker_loop(&queue, library, &events, &cache))
                .expect("spawn art worker");
        }
        this
    }

    /// A pool with no threads, used until the library has loaded.
    pub fn idle() -> Arc<Self> {
        Arc::new(Self { queue: Arc::new(Queue { jobs: Mutex::new(VecDeque::new()), signal: Condvar::new(), closed: std::sync::atomic::AtomicBool::new(false) }) })
    }

    /// takes over what was asked for meanwhile, since the idle pool has no threads and those jobs would wait forever.
    pub fn replace_with(&self, library: Arc<dyn Library>, events: UnboundedSender<ArtEvent>) -> Arc<Self> {
        let pending: Vec<Job> = self.queue.jobs.lock().unwrap().drain(..).collect();
        self.close();
        let next = Self::start(library, events);
        next.queue.jobs.lock().unwrap().extend(pending);
        next.queue.signal.notify_all();
        next
    }

    /// Stops the workers and drops what's queued, releasing the library they hold.
    pub fn close(&self) {
        self.queue.closed.store(true, std::sync::atomic::Ordering::Release);
        self.queue.jobs.lock().unwrap().clear();
        self.queue.signal.notify_all();
    }

    /// Queues a job, newest first. When full, the oldest job is dropped and returned so the caller
    /// can forget it and ask again if still needed.
    pub fn request(&self, job: Job) -> Option<Job> {
        let mut jobs = self.queue.jobs.lock().unwrap();
        jobs.push_front(job);
        let dropped = if jobs.len() > Self::MAX_QUEUED { jobs.pop_back() } else { None };
        drop(jobs);
        self.queue.signal.notify_one();
        dropped
    }
}

fn decode(library: &dyn Library, cache: &Mutex<DecodeCache>, album_id: &SharedString) -> Option<Arc<RgbaImage>> {
    if let Some(img) = cache.lock().unwrap().get(album_id) {
        return Some(img);
    }
    let bytes = library.album_artwork(album_id)?;
    let rgba = image::load_from_memory(&bytes).ok()?.into_rgba8();
    // Big covers decode slowly, and the dock is under 800 device pixels wide, so downsize once.
    let max = 1600u32;
    let rgba = if rgba.width().max(rgba.height()) > max {
        let scale = max as f32 / rgba.width().max(rgba.height()) as f32;
        resize_rgba(&rgba, (rgba.width() as f32 * scale) as u32, (rgba.height() as f32 * scale) as u32, FilterType::Bilinear)
    } else {
        rgba
    };
    let img = Arc::new(rgba);
    if !library.artwork_is_placeholder(album_id) {
        cache.lock().unwrap().put(album_id.clone(), img.clone());
    }
    Some(img)
}

fn worker_loop(queue: &Queue, library: Arc<dyn Library>, events: &UnboundedSender<ArtEvent>, cache: &Mutex<DecodeCache>) {
    loop {
        let job = {
            let mut jobs = queue.jobs.lock().unwrap();
            loop {
                if queue.closed.load(std::sync::atomic::Ordering::Acquire) {
                    return;
                }
                if let Some(job) = jobs.pop_front() {
                    break job;
                }
                jobs = queue.signal.wait(jobs).unwrap();
            }
        };
        let event = match job {
            Job::Thumb { album_id, size } if album_id.starts_with("pl:") => {
                let image = make_playlist_cover(&*library, &album_id[3..], size);
                ArtEvent::Thumb { album_id, size, image, placeholder: false }
            }
            Job::Thumb { album_id, size } if album_id == LIKED_COVER => {
                let image = liked_cover_pixels().map(|pixels| thumbnail(&pixels, size));
                ArtEvent::Thumb { album_id, size, image, placeholder: false }
            }
            Job::Thumb { album_id, size } if album_id.starts_with("mix:") => {
                let image = make_album_grid(&*library, &album_id[4..], size);
                ArtEvent::Thumb { album_id, size, image, placeholder: false }
            }
            // A Spotify friend's Discord avatar, from its URL.
            Job::Thumb { album_id, size } if album_id.starts_with(AVATAR_PREFIX) => {
                let image = navia_core::lanyard::watch::fetch_image(&album_id[AVATAR_PREFIX.len()..]).and_then(|bytes| image::load_from_memory(&bytes).ok()).map(|i| thumbnail(&i.into_rgba8(), size));
                ArtEvent::Thumb { album_id, size, image, placeholder: false }
            }
            // Navia's own artwork, not cover art.
            Job::Thumb { album_id, size } if album_id.starts_with("gfan:") => {
                let image = make_genre_fan(&*library, &album_id, size);
                ArtEvent::Thumb { album_id, size, image, placeholder: false }
            }
            Job::Bake { kind, id, width, height } => {
                let image = bake_source(&*library, &id, width, height).map(|img| match kind {
                    BakeKind::ArtistArt => bake_artist_art(&img, width, height),
                    BakeKind::CoverFade => bake_cover_fade(&img, width, height),
                    BakeKind::PageGlow => bake_page_glow(&img, width, height),
                });
                ArtEvent::Bake { kind, id, width, height, image }
            }
            Job::Thumb { album_id, size } => {
                let image = library.album_thumbnail(&album_id, size).and_then(|bytes| image::load_from_memory(&bytes).ok()).map(|i| thumbnail(&i.into_rgba8(), size));
                let placeholder = library.artwork_is_placeholder(&album_id);
                ArtEvent::Thumb { album_id, size, image, placeholder }
            }
            Job::Tint { album_id } => {
                let tint = decode(&*library, cache, &album_id).map(|src| tint_of(&src)).unwrap_or(Srgb::hex(crate::theme::TINT_FALLBACK));
                ArtEvent::Tint { album_id, tint }
            }
            Job::PlaylistTint { key, generation } => {
                let tint = playlist_tint(&*library, &key);
                ArtEvent::PlaylistTint { key, generation, tint }
            }
            Job::Dock { album_id, geometry } => {
                let src = decode(&*library, cache, &album_id).unwrap_or_else(|| Arc::new(RgbaImage::from_pixel(64, 64, Rgba([0x34, 0x50, 0x6b, 255]))));
                let tint = tint_of(&src);
                let bake = bake_dock(&src, geometry, tint);
                ArtEvent::Dock { album_id, bake: Arc::new(bake) }
            }
            Job::Expanded { album_id, width, height, blur_px } => {
                let src = decode(&*library, cache, &album_id).unwrap_or_else(|| Arc::new(RgbaImage::from_pixel(64, 64, Rgba([0x34, 0x50, 0x6b, 255]))));
                let tint = tint_of(&src);
                {
                    let (image, peaks) = bake_expanded(&src, width, height, tint, blur_px);
                    ArtEvent::Expanded { album_id, width, height, image, peaks }
                }
            }
            Job::BarGlow { album_id, width, height } => {
                let src = decode(&*library, cache, &album_id).unwrap_or_else(|| Arc::new(RgbaImage::from_pixel(64, 64, Rgba([0x34, 0x50, 0x6b, 255]))));
                let tint = tint_of(&src);
                ArtEvent::BarGlow { album_id, width, height, image: bake_bar_glow(width, height, tint) }
            }
        };
        if events.unbounded_send(event).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_liked_cover_decodes() {
        assert_eq!(liked_cover_pixels().map(|p| p.dimensions()), Some((512, 512)));
    }

    use gpui::SharedString;
    use image::{Rgba, RgbaImage};

    #[test]
    fn collage_cells_repeat_like_the_server() {
        assert_eq!(collage_cells(0), &[] as &[usize]);
        assert_eq!(collage_cells(1), &[0]);
        assert_eq!(collage_cells(2), &[0, 1, 1, 0]);
        assert_eq!(collage_cells(3), &[0, 1, 2, 0]);
        assert_eq!(collage_cells(4), &[0, 1, 2, 3]);
    }

    #[test]
    fn first_distinct_keeps_order_and_stops_at_four() {
        let ids = ["a", "a", "b", "c", "b", "d", "e"].map(SharedString::from);
        assert_eq!(first_distinct(ids), ["a", "b", "c", "d"].map(SharedString::from));
    }

    #[test]
    fn collage_fills_every_pixel() {
        let color = |c| RgbaImage::from_pixel(50, 30, Rgba([c, c, c, 255]));
        let out = collage(&[color(10), color(90), color(170), color(250)], 41);
        assert_eq!(out.dimensions(), (41, 41));
        for (x, y, expect) in [(1, 1, 10), (39, 1, 90), (1, 39, 170), (39, 39, 250)] {
            assert_eq!(out.get_pixel(x, y).0[0], expect);
        }
        assert!(out.pixels().all(|p| p.0[0] != 0x26));
    }

    use super::*;

    #[test]
    fn tint_of_flat_color_keeps_saturation_cap() {
        let img = RgbaImage::from_pixel(32, 32, Rgba([16, 43, 67, 255]));
        let t = tint_of(&img);
        assert!((t.r - 16.0 / 255.0).abs() < 0.02, "{t:?}");
    }
}
