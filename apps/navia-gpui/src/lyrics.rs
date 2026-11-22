//! Blur-by-distance lyrics. GPUI can't blur an element's content, so text is rasterized and
//! blurred here, one sprite per line per blur level, on a background thread and cached.

use std::sync::{Arc, Condvar, Mutex};

use cosmic_text::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache, Weight, Wrap};
use futures::channel::mpsc::UnboundedSender;
use gpui::{RenderImage, SharedString};
use image::{GrayImage, RgbaImage, imageops};

use crate::art::{ArtEvent, into_render_image};

/// Blur levels: 0 is sharp, then one per line of distance from the current line.
pub const BLUR_LEVELS: usize = 6;
/// Levels from this one up are stored at half resolution. The GPU scales them up and the blur hides it.
const HALF_RES_FROM: usize = 1;

/// The blur in logical pixels at a distance from the current line.
fn blur_css(level: usize) -> f32 {
    use crate::theme::size::{LYRIC_BLUR_MAX, LYRIC_BLUR_NEAR, LYRIC_BLUR_STEP};
    if level == 0 { 0.0 } else { (LYRIC_BLUR_NEAR + LYRIC_BLUR_STEP * level as f32).min(LYRIC_BLUR_MAX) }
}

/// A text line's identity. The text stays the same and only its layout varies.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LyricKey {
    pub song: SharedString,
    /// Device pixels, scaled to integers so keys compare exactly.
    pub font_px_x100: u32,
    pub wrap_px: u32,
    /// Device pixels per logical pixel.
    pub css_scale_x1000: u32,
    pub gap_px_x100: u32,
    /// bracketed parts moved under their lines or left as written.
    pub split: bool,
}

impl LyricKey {
    pub fn font_px(&self) -> f32 {
        self.font_px_x100 as f32 / 100.0
    }
}

#[derive(Clone)]
pub struct LineSprite {
    pub levels: Vec<Arc<RenderImage>>,
    /// Full sprite size, including the padding the blur spreads into, in device pixels.
    pub width: f32,
    pub height: f32,
    pub padding: f32,
    /// the held last word's letter-shaped halo, alone, so it never lights a neighbour.
    pub glow: Option<WordGlow>,
    /// The wrapped text's visual lines, top to bottom, so the sweep can fill them one at a time.
    pub rows: Vec<SweepRow>,
    /// the last word's letters drawn on their own, for a line with the server's word timing whose last word can roll.
    pub letters: Option<RollingLetters>,
}

/// a halo over the sprite's columns `x0..x0 + width` at its full height (device pixels).
#[derive(Clone)]
pub struct WordGlow {
    pub image: Arc<RenderImage>,
    pub x0: f32,
    pub width: f32,
}

/// the rolling word's even and odd letters in two images each, so a letter reaching over its neighbour (a kerned
/// pair) is drawn whole: sharp ones and their halos, both over the sprite's columns `x0..x0 + width` at its full
/// height (device pixels).
#[derive(Clone)]
pub struct RollingLetters {
    pub images: [Arc<RenderImage>; 2],
    pub glows: [Arc<RenderImage>; 2],
    pub x0: f32,
    pub width: f32,
}

/// One visual line of a wrapped lyric, in device pixels from the text's origin (inside the padding).
#[derive(Clone, Debug, PartialEq)]
pub struct SweepRow {
    pub top: f32,
    pub height: f32,
    pub x0: f32,
    pub x1: f32,
    /// Glyphs on this row. The sweep's progress is shared out by glyph in reading order.
    pub glyphs: usize,
    pub rtl: bool,
    /// each glyph's bytes in the lyric's text and its left and right edge, for lighting one timed word.
    pub spans: Vec<GlyphSpan>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlyphSpan {
    pub start: usize,
    pub end: usize,
    pub x0: f32,
    pub x1: f32,
}

impl SweepRow {
    /// where the glyphs of the text's bytes `start..end` sit on this row, if any do.
    pub fn span_of(&self, start: usize, end: usize) -> Option<(f32, f32)> {
        self.spans.iter().filter(|g| g.start < end && g.end > start).fold(None, |acc, g| Some(acc.map_or((g.x0, g.x1), |(a, b): (f32, f32)| (a.min(g.x0), b.max(g.x1)))))
    }
}

/// scripts whose letters join, stack or reorder when shaped (Arabic, Syriac, N'Ko, Indic, Southeast Asian, Tibetan,
/// Mongolian and the joiners), so their letters can't be moved one by one.
pub const JOINED_SCRIPTS: [(u32, u32); 13] = [
    (0x0600, 0x08FF),
    (0x0900, 0x0DFF),
    (0x0E00, 0x0FFF),
    (0x1000, 0x109F),
    (0x1780, 0x18AF),
    (0x1A00, 0x1AAF),
    (0x1B00, 0x1BFF),
    (0x200C, 0x200D),
    (0xA840, 0xA87F),
    (0xA980, 0xA9DF),
    (0xFB50, 0xFDFF),
    (0xFE70, 0xFEFF),
    (0x10D00, 0x11FFF),
];

/// the byte range of the text's last word, trailing spaces left out.
pub fn last_word(text: &str) -> (usize, usize) {
    let end = text.trim_end().len();
    let start = text[..end].char_indices().filter(|(_, c)| c.is_whitespace()).last().map_or(0, |(k, c)| k + c.len_utf8());
    (start, end)
}

/// each letter (grapheme cluster) of `text[start..end]` as a byte range, when every one can move by itself: a single
/// character outside the joined scripts. combining sequences and joined scripts keep the word whole.
pub fn wave_letters(text: &str, start: usize, end: usize) -> Option<Vec<(usize, usize)>> {
    use unicode_segmentation::UnicodeSegmentation;
    let word = text.get(start..end).filter(|w| !w.is_empty())?;
    let joined = |c: char| JOINED_SCRIPTS.iter().any(|&(a, b)| (a..=b).contains(&(c as u32)));
    word.grapheme_indices(true)
        .map(|(at, g)| {
            let mut chars = g.chars();
            let c = chars.next()?;
            (chars.next().is_none() && !joined(c)).then_some((start + at, start + at + g.len()))
        })
        .collect()
}

/// where each letter sits: its row and left and right edges, in reading order. none unless they're all on one row and
/// each has glyphs of its own, so a ligature or a wrapped word stays whole.
pub fn letter_boxes(rows: &[SweepRow], letters: &[(usize, usize)]) -> Option<(usize, Vec<(f32, f32)>)> {
    let (first, last) = (letters.first()?, letters.last()?);
    let row = rows.iter().position(|r| r.span_of(first.0, first.1).is_some())?;
    rows[row].span_of(last.0, last.1)?;
    let boxes: Vec<(f32, f32)> = letters.iter().map(|&(a, b)| rows[row].span_of(a, b)).collect::<Option<_>>()?;
    let mut sorted = boxes.clone();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    sorted.windows(2).all(|w| w[0].1 <= w[1].0 + 0.5).then_some((row, boxes))
}

/// How much of each row is lit at `progress`, from 0 to 1 over the whole lyric.
pub fn row_fractions(rows: &[SweepRow], progress: f32) -> Vec<f32> {
    let total: usize = rows.iter().map(|r| r.glyphs).sum();
    let mut filled = progress.clamp(0.0, 1.0) * total.max(1) as f32;
    rows.iter()
        .map(|r| {
            let f = if r.glyphs == 0 { 1.0 } else { (filled / r.glyphs as f32).clamp(0.0, 1.0) };
            filled -= r.glyphs as f32;
            f
        })
        .collect()
}

/// a share of the row: the soft edge of a fill without word timing.
pub const LYRIC_SWEEP_SOFT: f32 = 0.14;
/// a share of the row's height: the soft edge inside a timed word, about a syllable.
pub const LYRIC_WORD_SOFT: f32 = 0.8;
/// past any row, in device pixels.
const FAR: f32 = 1.0e6;

/// how lit a row is, along its reading direction (`u` is `x`, or `-x` right to left): whole before `full_to`, then
/// fading to nothing at `edge` over `soft`, and nothing from `stop` on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RowFill {
    pub rtl: bool,
    pub full_to: f32,
    pub edge: f32,
    pub soft: f32,
    pub stop: f32,
}

impl RowFill {
    const LIT: Self = Self { rtl: false, full_to: FAR, edge: FAR, soft: 1.0, stop: FAR };
    const UNLIT: Self = Self { rtl: false, full_to: -FAR, edge: -FAR, soft: 1.0, stop: -FAR };

    /// the lit stretches as (left, right, 0 to 1) in `x`, left to right, the fade in strips about `step` wide.
    pub fn pieces(&self, step: f32) -> Vec<(f32, f32, f32)> {
        let full_end = self.full_to.max((self.edge - self.soft).min(self.stop)).min(FAR);
        let mut out = Vec::new();
        if full_end > -FAR {
            out.push((-FAR, full_end, 1.0));
        }
        let (r0, r1) = (full_end.max(-FAR), self.edge.min(self.stop).min(FAR));
        if r1 > r0 {
            let n = ((r1 - r0) / step.max(0.01)).ceil().clamp(1.0, LYRIC_SWEEP_STEPS_MAX as f32) as usize;
            for k in 0..n {
                let (a, b) = (r0 + (r1 - r0) * k as f32 / n as f32, r0 + (r1 - r0) * (k + 1) as f32 / n as f32);
                let lit = ((self.edge - (a + b) / 2.0) / self.soft).clamp(0.0, 1.0);
                if lit > 0.0 {
                    out.push((a, b, lit));
                }
            }
        }
        if self.rtl {
            out = out.into_iter().rev().map(|(a, b, lit)| (-b, -a, lit)).collect();
        }
        out
    }
}

/// so a long fade can't cost hundreds of strips.
pub const LYRIC_SWEEP_STEPS_MAX: usize = 40;

/// each row's fill at `progress` (0 to 1 of the text's characters). with word timing (`timed`) the fill follows the
/// glyphs and stays inside the word being sung, so neither the word before nor the one after is lit by its edge;
/// without, the row fills by glyph count with a soft edge reaching ahead, as wide as `LYRIC_SWEEP_SOFT` of the row.
pub fn row_fills(rows: &[SweepRow], text: &str, progress: f32, timed: bool) -> Vec<RowFill> {
    if !timed {
        return rows
            .iter()
            .zip(row_fractions(rows, progress))
            .map(|(row, f)| {
                let (ua, ub) = if row.rtl { (-row.x1, -row.x0) } else { (row.x0, row.x1) };
                let soft = ((ub - ua) * LYRIC_SWEEP_SOFT).max(1.0);
                let cut = ua + f * (ub - ua);
                match f {
                    f if f <= 0.0 => RowFill { rtl: row.rtl, ..RowFill::UNLIT },
                    f if f >= 1.0 => RowFill { rtl: row.rtl, ..RowFill::LIT },
                    _ => RowFill { rtl: row.rtl, full_to: cut, edge: cut + soft, soft, stop: FAR },
                }
            })
            .collect();
    }
    let chars: Vec<usize> = text.char_indices().map(|(at, _)| at).chain(std::iter::once(text.len())).collect();
    let position = progress.clamp(0.0, 1.0) * (chars.len() - 1) as f32;
    let k = (position.floor() as usize).min(chars.len() - 1);
    let byte = chars[k] as f32 + position.fract() * (chars.get(k + 1).copied().unwrap_or(chars[k]) - chars[k]) as f32;
    rows.iter().map(|row| timed_row_fill(row, text, byte)).collect()
}

fn timed_row_fill(row: &SweepRow, text: &str, byte: f32) -> RowFill {
    let (first, last) = row.spans.iter().fold((usize::MAX, 0), |(a, b), g| (a.min(g.start), b.max(g.end)));
    if row.spans.is_empty() || last as f32 <= byte {
        return RowFill { rtl: row.rtl, ..RowFill::LIT };
    }
    if first as f32 >= byte {
        return RowFill { rtl: row.rtl, ..RowFill::UNLIT };
    }
    // the row's words in reading order as (first byte, end byte, start, end along the reading direction).
    let mut glyphs: Vec<&GlyphSpan> = row.spans.iter().collect();
    glyphs.sort_by_key(|g| g.start);
    let along = |g: &GlyphSpan| if row.rtl { (-g.x1, -g.x0) } else { (g.x0, g.x1) };
    let mut words: Vec<(usize, usize, f32, f32)> = Vec::new();
    let mut joined = false;
    for g in glyphs.iter().copied() {
        if text.get(g.start..g.end).is_none_or(|t| t.chars().all(char::is_whitespace)) {
            joined = false;
            continue;
        }
        let (a, b) = along(g);
        match words.last_mut() {
            Some(w) if joined => *w = (w.0.min(g.start), w.1.max(g.end), w.2.min(a), w.3.max(b)),
            _ => words.push((g.start, g.end, a, b)),
        }
        joined = true;
    }
    words.sort_by(|a, b| a.2.total_cmp(&b.2));
    // each word owns the room halfway to its neighbours, so a letter's overhang fills with it.
    let room = row.height * 0.5;
    let owned = |j: usize| {
        let (_, _, a, b) = words[j];
        (if j == 0 { a - room } else { (words[j - 1].3 + a) / 2.0 }, if j + 1 == words.len() { b + room } else { (b + words[j + 1].2) / 2.0 })
    };
    let soft = (row.height * LYRIC_WORD_SOFT).max(1.0);
    if let Some(j) = words.iter().position(|w| (w.0 as f32) < byte && byte < w.1 as f32) {
        let (_, _, a, b) = words[j];
        let at = glyphs
            .iter()
            .find(|g| (g.start as f32) <= byte && byte < g.end as f32)
            .map_or(a, |g| {
                let (ga, gb) = along(g);
                ga + (gb - ga) * (byte - g.start as f32) / (g.end - g.start).max(1) as f32
            });
        let t = ((at - a) / (b - a).max(1.0)).clamp(0.0, 1.0);
        let (oa, ob) = owned(j);
        return RowFill { rtl: row.rtl, full_to: oa, edge: oa + t * (ob - oa + soft), soft, stop: ob };
    }
    match words.iter().position(|w| w.0 as f32 >= byte) {
        Some(j) => {
            let (oa, _) = owned(j);
            RowFill { rtl: row.rtl, full_to: oa, edge: -FAR, soft, stop: oa }
        }
        None => RowFill { rtl: row.rtl, ..RowFill::LIT },
    }
}

fn sweep_rows(buffer: &Buffer) -> Vec<SweepRow> {
    buffer
        .layout_runs()
        .map(|run| {
            let x0 = run.glyphs.iter().map(|g| g.x).fold(f32::INFINITY, f32::min);
            let x1 = run.glyphs.iter().map(|g| g.x + g.w).fold(f32::NEG_INFINITY, f32::max);
            let (x0, x1) = if run.glyphs.is_empty() { (0.0, 0.0) } else { (x0, x1) };
            let spans = run.glyphs.iter().map(|g| GlyphSpan { start: g.start, end: g.end, x0: g.x, x1: g.x + g.w }).collect();
            SweepRow { top: run.line_top, height: run.line_height, x0, x1, glyphs: run.glyphs.len(), rtl: run.rtl, spans }
        })
        .collect()
}

pub struct LyricLayout {
    /// Height of each line's text in device pixels, known before any sprite is ready.
    pub heights: Vec<f32>,
}

pub struct LyricJob {
    pub key: LyricKey,
    pub lines: Vec<SharedString>,
    /// lines with the server's word timing, whose last word may roll.
    pub rolls: Vec<bool>,
    /// Lines near this one are rasterized first.
    pub near: usize,
}

struct Slot {
    job: Mutex<Option<LyricJob>>,
    ready: Condvar,
}

/// A single worker. A new request replaces any waiting one and stops one in progress.
pub struct LyricRaster {
    slot: Arc<Slot>,
}

impl LyricRaster {
    pub fn start(events: UnboundedSender<ArtEvent>) -> Self {
        let slot = Arc::new(Slot { job: Mutex::new(None), ready: Condvar::new() });
        let worker = slot.clone();
        std::thread::Builder::new()
            .name("navia-lyrics".into())
            .spawn(move || worker_loop(&worker, &events))
            .expect("spawn lyric worker");
        Self { slot }
    }

    pub fn request(&self, job: LyricJob) {
        *self.slot.job.lock().unwrap() = Some(job);
        self.slot.ready.notify_one();
    }
}

fn load_fonts() -> FontSystem {
    let mut fonts = FontSystem::new();
    for bytes in [
        include_bytes!("../assets/fonts/Inter-SemiBold.ttf").as_slice(),
        include_bytes!("../assets/fonts/Inter-Bold.ttf").as_slice(),
    ] {
        fonts.db_mut().load_font_data(bytes.to_vec());
    }
    fonts
}

fn worker_loop(slot: &Slot, events: &UnboundedSender<ArtEvent>) {
    // Loading system fonts is slow and costly, so it waits for the first lyrics.
    let mut fonts: Option<FontSystem> = None;
    let mut cache = SwashCache::new();
    loop {
        let job = {
            let mut guard = slot.job.lock().unwrap();
            loop {
                if let Some(job) = guard.take() {
                    break job;
                }
                guard = slot.ready.wait(guard).unwrap();
            }
        };
        run_job(&job, slot, fonts.get_or_insert_with(load_fonts), &mut cache, events);
    }
}

fn line_height(key: &LyricKey) -> f32 {
    (key.font_px() * crate::theme::size::LYRIC_LINE_HEIGHT).ceil()
}

fn run_job(job: &LyricJob, slot: &Slot, fonts: &mut FontSystem, cache: &mut SwashCache, events: &UnboundedSender<ArtEvent>) {
    let key = &job.key;
    // Shape everything first, since heights decide where every line goes.
    let mut buffers = Vec::with_capacity(job.lines.len());
    let mut heights = Vec::with_capacity(job.lines.len());
    for line in &job.lines {
        let (buffer, height) = shape(line, key, fonts);
        heights.push(height);
        buffers.push(buffer);
    }
    if events.unbounded_send(ArtEvent::LyricLayout { key: key.clone(), layout: LyricLayout { heights } }).is_err() {
        return;
    }
    let started = std::time::Instant::now();
    let mut order: Vec<usize> = (0..job.lines.len()).collect();
    order.sort_by_key(|i| i.abs_diff(job.near));
    for ix in order {
        if slot.job.lock().unwrap().is_some() {
            return;
        }
        let rolls = job.rolls.get(ix).copied().unwrap_or(false);
        let sprite = rasterize(&buffers[ix], key, fonts, cache, job.lines[ix].as_ref(), rolls);
        if events.unbounded_send(ArtEvent::LyricLine { key: key.clone(), ix, sprite }).is_err() {
            return;
        }
    }
    eprintln!("navia: lyrics rasterized {} lines in {:.0} ms", job.lines.len(), started.elapsed().as_secs_f64() * 1000.0);
}

fn shape(text: &str, key: &LyricKey, fonts: &mut FontSystem) -> (Buffer, f32) {
    let line_h = line_height(key);
    let attrs = Attrs::new().family(Family::Name(crate::assets::FONT_FAMILY)).weight(Weight::BOLD).letter_spacing(-0.01);
    let mut buffer = Buffer::new(fonts, Metrics::new(key.font_px(), line_h));
    {
        let mut b = buffer.borrow_with(fonts);
        b.set_wrap(Wrap::WordOrGlyph);
        b.set_size(Some(key.wrap_px as f32), None);
        b.set_text(text, &attrs, Shaping::Advanced, None);
        b.shape_until_scroll(false);
    }
    let lines = buffer.layout_runs().count().max(1);
    (buffer, lines as f32 * line_h)
}

/// bracketed backing vocals move to `backing` and a long intro gets an empty first line; `split` off keeps lines as written.
pub fn prepare(lines: Vec<crate::model::LyricLine>, split: bool) -> Vec<crate::model::LyricLine> {
    use crate::theme::size::LYRIC_GAP_MIN;
    let synced = lines.first().is_some_and(|l| l.synced);
    let mut out: Vec<crate::model::LyricLine> = Vec::with_capacity(lines.len() + 1);
    if synced && lines.first().is_some_and(|l| l.start_ms as f32 >= LYRIC_GAP_MIN * 1000.0) {
        out.push(crate::model::LyricLine { start_ms: 0, text: SharedString::default(), synced, timed: None, backing: None, backing_first: false });
    }
    for mut line in lines {
        if split && line.timed.is_none() {
            let (main, backing) = split_backing(&line.text);
            if let Some(backing) = backing {
                line.backing_first = line.text.trim_start().starts_with('(');
                line.text = main.into();
                line.backing = Some(backing.into());
            }
        }
        out.push(line);
    }
    out
}

/// a line's text without its bracketed parts, and those parts joined, when it has any.
fn split_backing(text: &str) -> (String, Option<String>) {
    let (mut main, mut backing, mut part) = (String::new(), Vec::new(), String::new());
    let mut depth = 0;
    for c in text.chars() {
        match c {
            '(' => {
                if depth > 0 {
                    part.push(c);
                }
                depth += 1;
            }
            ')' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    backing.push(std::mem::take(&mut part).trim().to_string());
                } else {
                    part.push(c);
                }
            }
            _ if depth > 0 => part.push(c),
            _ => main.push(c),
        }
    }
    // an unclosed bracket isn't a backing part: keep the line as it was.
    if depth > 0 || backing.iter().all(String::is_empty) {
        return (text.to_string(), None);
    }
    let main = main.split_whitespace().collect::<Vec<_>>().join(" ");
    (main, Some(backing.into_iter().filter(|b| !b.is_empty()).collect::<Vec<_>>().join(" ")))
}

/// `rolls`: the last word's letters may roll.
fn rasterize(buffer: &Buffer, key: &LyricKey, fonts: &mut FontSystem, cache: &mut SwashCache, text: &str, rolls: bool) -> LineSprite {
    let css_scale = key.css_scale_x1000 as f32 / 1000.0;
    let line_h = line_height(key);
    let lines = buffer.layout_runs().count().max(1) as u32;
    let halos = glow_layers(line_h);
    // reaching 3 deviations of the widest halo.
    let glow_reach = (3.0 * halos[1].0).ceil() as u32 + 4;
    // enough room that the widest blur and the halo fade out before the sprite's edge.
    let padding = ((3.0 * blur_css(BLUR_LEVELS - 1) * css_scale).ceil() as u32 + 2).max(glow_reach);
    // Only as wide as the text: lines are often much shorter than the column.
    let text_w = buffer.layout_runs().map(|r| r.line_w).fold(0.0f32, f32::max).ceil().max(1.0) as u32;
    let width = text_w + padding * 2;
    let height = lines * line_h as u32 + padding * 2;

    let mut coverage = GrayImage::new(width, height);
    let mut borrowed = buffer.clone();
    let mut b = borrowed.borrow_with(fonts);
    b.draw(cache, Color::rgb(255, 255, 255), |x, y, w, h, color| {
        let alpha = color.a();
        if alpha == 0 {
            return;
        }
        for py in y..y + h as i32 {
            for px in x..x + w as i32 {
                let (cx, cy) = (px + padding as i32, py + padding as i32);
                if cx >= 0 && cy >= 0 && (cx as u32) < width && (cy as u32) < height {
                    let existing = coverage.get_pixel(cx as u32, cy as u32).0[0];
                    coverage.put_pixel(cx as u32, cy as u32, image::Luma([existing.max(alpha)]));
                }
            }
        }
    });

    // the last word's glyphs on their own: all of them in `word`, and each letter in its parity's image when it rolls.
    let (start, end) = last_word(text);
    let letters = if rolls { wave_letters(text, start, end) } else { None };
    let mut word = GrayImage::new(width, height);
    let mut parity = letters.as_ref().map(|_| [GrayImage::new(width, height), GrayImage::new(width, height)]);
    let (mut lo, mut hi) = (i32::MAX, i32::MIN);
    for run in buffer.layout_runs() {
        for glyph in run.glyphs {
            if glyph.start < start || glyph.end > end || start >= end {
                continue;
            }
            let k = letters.as_ref().and_then(|l| l.iter().position(|&(a, b)| glyph.start >= a && glyph.end <= b));
            let physical = glyph.physical((0., run.line_y), 1.0);
            cache.with_pixels(fonts, physical.cache_key, Color::rgb(255, 255, 255), |x, y, color| {
                let (cx, cy) = (physical.x + x + padding as i32, physical.y + y + padding as i32);
                if color.a() == 0 || cx < 0 || cy < 0 || cx as u32 >= width || cy as u32 >= height {
                    return;
                }
                let put = |image: &mut GrayImage| {
                    let existing = image.get_pixel(cx as u32, cy as u32).0[0];
                    image.put_pixel(cx as u32, cy as u32, image::Luma([existing.max(color.a())]));
                };
                put(&mut word);
                if let (Some(k), Some(p)) = (k, parity.as_mut()) {
                    put(&mut p[k % 2]);
                }
                (lo, hi) = (lo.min(cx), hi.max(cx));
            });
        }
    }
    // the word's columns with room for its halo.
    let columns = (lo <= hi).then(|| {
        let x0 = (lo - glow_reach as i32).max(0) as u32;
        (x0, ((hi + 1 + glow_reach as i32) as u32).min(width) - x0)
    });
    let crop = |image: &GrayImage, (x0, w): (u32, u32)| imageops::crop_imm(image, x0, 0, w, height).to_image();
    let glow = columns.map(|c| WordGlow { image: coverage_to_image(&halo_of(&crop(&word, c), halos), 1.0), x0: c.0 as f32, width: c.1 as f32 });
    let letters = columns.zip(parity).map(|(c, parity)| {
        let crops = parity.each_ref().map(|p| crop(p, c));
        RollingLetters {
            images: crops.each_ref().map(|c| coverage_to_image(c, 1.0)),
            glows: crops.each_ref().map(|c| coverage_to_image(&halo_of(c, halos), 1.0)),
            x0: c.0 as f32,
            width: c.1 as f32,
        }
    });

    let mut levels = Vec::with_capacity(BLUR_LEVELS);
    for level in 0..BLUR_LEVELS {
        let sigma = blur_css(level) * css_scale;
        let image = if level == 0 {
            coverage_to_image(&coverage, 1.0)
        } else if level >= HALF_RES_FROM {
            let half = imageops::resize(&coverage, width / 2, height / 2, imageops::FilterType::Triangle);
            coverage_to_image(&imageops::blur(&half, sigma / 2.0), 1.0)
        } else {
            coverage_to_image(&imageops::blur(&coverage, sigma), 1.0)
        };
        levels.push(image);
    }

    LineSprite { levels, width: width as f32, height: height as f32, padding: padding as f32, glow, rows: sweep_rows(buffer), letters }
}

/// a held last word's tight and wide halo as (gaussian deviation, opacity), for lines `line_h` tall.
pub fn glow_layers(line_h: f32) -> [(f32, f32); 2] {
    use crate::theme::size::{LYRIC_GLOW_TIGHT, LYRIC_GLOW_TIGHT_ALPHA, LYRIC_GLOW_WIDE, LYRIC_GLOW_WIDE_ALPHA};
    [(LYRIC_GLOW_TIGHT * line_h, LYRIC_GLOW_TIGHT_ALPHA), (LYRIC_GLOW_WIDE * line_h, LYRIC_GLOW_WIDE_ALPHA)]
}

/// the letter-shaped halo of `coverage`, at full size so it sits exactly round the letters.
fn halo_of(coverage: &GrayImage, [(tight_sigma, tight_alpha), (wide_sigma, wide_alpha)]: [(f32, f32); 2]) -> GrayImage {
    let (tight, wide) = (imageops::blur(coverage, tight_sigma), imageops::blur(coverage, wide_sigma));
    let mut halo = GrayImage::new(coverage.width(), coverage.height());
    for ((out, t), w) in halo.pixels_mut().zip(tight.pixels()).zip(wide.pixels()) {
        let v = f32::from(t.0[0]) * tight_alpha + f32::from(w.0[0]) * wide_alpha;
        out.0[0] = v.round().clamp(0.0, 255.0) as u8;
    }
    halo
}

fn coverage_to_image(coverage: &GrayImage, gain: f32) -> Arc<RenderImage> {
    let mut rgba = RgbaImage::new(coverage.width(), coverage.height());
    for (src, dst) in coverage.pixels().zip(rgba.pixels_mut()) {
        dst.0 = [255, 255, 255, (f32::from(src.0[0]) * gain).round().clamp(0.0, 255.0) as u8];
    }
    into_render_image(rgba)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(glyphs: usize) -> SweepRow {
        SweepRow { top: 0.0, height: 10.0, x0: 0.0, x1: 100.0, glyphs, rtl: false, spans: Vec::new() }
    }

    fn line(start_ms: u32, text: &str) -> crate::model::LyricLine {
        crate::model::LyricLine { start_ms, text: text.to_string().into(), synced: true, timed: None, backing: None, backing_first: false }
    }

    #[test]
    fn bracketed_parts_move_under_their_line() {
        assert_eq!(split_backing("Nothin' scares me anymore (One, two, three, four)"), ("Nothin' scares me anymore".into(), Some("One, two, three, four".into())));
        assert_eq!(split_backing("(Yeah-yeah, yo)"), (String::new(), Some("Yeah-yeah, yo".into())));
        assert_eq!(split_backing("No brackets here"), ("No brackets here".into(), None));
        assert_eq!(split_backing("Unclosed (bracket"), ("Unclosed (bracket".into(), None));
    }

    #[test]
    fn a_leading_bracket_is_sung_first() {
        let lines = prepare(vec![line(0, "(Ooh) here it comes"), line(3_000, "Here it comes (ooh)")], true);
        assert!(lines[0].backing_first && lines[0].text.as_ref() == "here it comes");
        assert!(!lines[1].backing_first);
    }

    #[test]
    fn unsplit_brackets_stay_in_the_line() {
        let lines = prepare(vec![line(0, "Here it comes (ooh)")], false);
        assert!(lines[0].backing.is_none() && lines[0].text.as_ref() == "Here it comes (ooh)");
    }

    #[test]
    fn a_long_intro_gets_its_own_gap_line() {
        let long = prepare(vec![line(12_000, "First"), line(15_000, "Second")], true);
        assert_eq!(long.len(), 3);
        assert!(long[0].text.is_empty() && long[0].start_ms == 0);
        let short = prepare(vec![line(2_000, "First")], true);
        assert_eq!(short.len(), 1);
    }

    #[test]
    fn rows_fill_in_reading_order() {
        let rows = [row(30), row(10)];
        assert_eq!(row_fractions(&rows, 0.0), vec![0.0, 0.0]);
        assert_eq!(row_fractions(&rows, 0.375), vec![0.5, 0.0]);
        assert_eq!(row_fractions(&rows, 0.75), vec![1.0, 0.0]);
        assert_eq!(row_fractions(&rows, 0.875), vec![1.0, 0.5]);
        assert_eq!(row_fractions(&rows, 1.0), vec![1.0, 1.0]);
    }

    #[test]
    fn real_text_wraps_into_rows_with_direction() {
        let mut fonts = FontSystem::new();
        let key = LyricKey { song: "s".into(), font_px_x100: 3000, wrap_px: 300, css_scale_x1000: 1000, gap_px_x100: 0, split: true };
        let (buffer, _) = shape("In the stillness of the evening light, remembering what you had", &key, &mut fonts);
        let rows = sweep_rows(&buffer);
        assert!(rows.len() >= 2, "{rows:?}");
        assert!(rows.windows(2).all(|w| w[1].top > w[0].top));
        assert!(rows.iter().all(|r| !r.rtl && r.x1 > r.x0));
        let (buffer, _) = shape("في سكون الليل الطويل نتذكر كل ما كان لدينا", &key, &mut fonts);
        assert!(sweep_rows(&buffer).iter().all(|r| r.rtl));
    }

    #[test]
    fn a_timed_word_is_found_by_its_bytes_on_its_row() {
        let mut fonts = FontSystem::new();
        let key = LyricKey { song: "s".into(), font_px_x100: 3000, wrap_px: 2000, css_scale_x1000: 1000, gap_px_x100: 0, split: true };
        let text = "Hold the line";
        let (buffer, _) = shape(text, &key, &mut fonts);
        let rows = sweep_rows(&buffer);
        let (h0, h1) = rows[0].span_of(0, 4).expect("Hold");
        let (l0, l1) = rows[0].span_of(9, 13).expect("line");
        assert!(h0 < h1 && h1 < l0 && l0 < l1, "{h0} {h1} {l0} {l1}");
        assert!(rows[0].span_of(4, 5).is_none_or(|(a, b)| a >= h1 - 1.0 && b <= l0), "the space sits between");
        assert!(rows[0].span_of(40, 50).is_none());
    }

    #[test]
    fn the_last_word_splits_into_letters_unless_they_join() {
        let text = "I want you to stay ";
        let (a, b) = last_word(text);
        assert_eq!(&text[a..b], "stay");
        assert_eq!(wave_letters(text, a, b), Some(vec![(14, 15), (15, 16), (16, 17), (17, 18)]));
        let accented = "Je t'attends, café";
        let (a, b) = last_word(accented);
        assert_eq!(wave_letters(accented, a, b).map(|l| l.len()), Some(4), "a precomposed letter is one character");
        let combining = "cafe\u{301}";
        assert_eq!(wave_letters(combining, 0, combining.len()), None);
        let arabic = "حبيبي";
        assert_eq!(wave_letters(arabic, 0, arabic.len()), None);
        let hindi = "नमस्ते";
        assert_eq!(wave_letters(hindi, 0, hindi.len()), None);
        let family = "👨\u{200d}👩";
        assert_eq!(wave_letters(family, 0, family.len()), None);
        assert_eq!(wave_letters("", 0, 0), None);
    }

    #[test]
    fn the_wave_rolls_through_the_letters_and_rests_at_both_ends() {
        use crate::theme::motion::{LYRIC_WAVE_HEIGHT, LYRIC_WAVE_PERIOD, LYRIC_WAVE_PHASE, lyric_wave};
        // a held word as the server timed it: "STAY" from 4.728 s, the list moving on at 7.733 s.
        let held = 7.733 - 4.728;
        for k in 0..4 {
            assert_eq!(lyric_wave(k, 0.0, held), 0.0);
            assert_eq!(lyric_wave(k, held, held), 0.0);
            assert!(lyric_wave(k, held - 0.01, held) < LYRIC_WAVE_HEIGHT * 0.01);
        }
        let samples: Vec<f32> = (1..300).map(|n| lyric_wave(0, held * n as f32 / 300.0, held)).collect();
        let top = samples.iter().copied().fold(0.0, f32::max);
        assert!(top > LYRIC_WAVE_HEIGHT * 0.99 && top <= LYRIC_WAVE_HEIGHT, "{top}");
        // mid-hold, each letter follows the one before it by a phase of the cycle.
        let t = 1.5;
        assert!((lyric_wave(1, t + LYRIC_WAVE_PHASE * LYRIC_WAVE_PERIOD, held) - lyric_wave(0, t, held)).abs() < 1e-5);
    }

    #[test]
    fn a_rolling_line_gets_its_last_words_letters_in_two_images() {
        let mut fonts = FontSystem::new();
        let mut cache = SwashCache::new();
        let key = LyricKey { song: "s".into(), font_px_x100: 3000, wrap_px: 2000, css_scale_x1000: 1000, gap_px_x100: 0, split: true };
        let text = "I want you to stay";
        let (buffer, _) = shape(text, &key, &mut fonts);
        let sprite = rasterize(&buffer, &key, &mut fonts, &mut cache, text, true);
        let letters = sprite.letters.expect("letters");
        let (a, b) = sprite.rows[0].span_of(14, 18).unwrap();
        assert!(letters.x0 <= a + sprite.padding && letters.x0 + letters.width >= b + sprite.padding, "covers the word");
        assert!(letters.width < sprite.width / 2.0, "only the word's columns");
        assert!(rasterize(&buffer, &key, &mut fonts, &mut cache, text, false).letters.is_none());
        let arabic = "في سكون الليل";
        let (buffer, _) = shape(arabic, &key, &mut fonts);
        assert!(rasterize(&buffer, &key, &mut fonts, &mut cache, arabic, true).letters.is_none(), "joined letters stay whole");
    }

    #[test]
    fn each_letter_of_a_shaped_word_has_its_own_box() {
        let mut fonts = FontSystem::new();
        let key = LyricKey { song: "s".into(), font_px_x100: 3000, wrap_px: 2000, css_scale_x1000: 1000, gap_px_x100: 0, split: true };
        let text = "I want you to stay";
        let (buffer, _) = shape(text, &key, &mut fonts);
        let rows = sweep_rows(&buffer);
        let (a, b) = last_word(text);
        let letters = wave_letters(text, a, b).unwrap();
        let (row, boxes) = letter_boxes(&rows, &letters).expect("one row");
        assert_eq!(row, 0);
        assert_eq!(boxes.len(), 4);
        assert!(boxes.windows(2).all(|w| w[0].1 <= w[1].0 + 0.5 && w[0].0 < w[1].0), "{boxes:?}");
    }

    fn lit_at(fill: &RowFill, x: f32) -> f32 {
        fill.pieces(0.5).iter().find(|p| p.0 <= x && x < p.1).map_or(0.0, |p| p.2)
    }

    /// the byte of `text` where character `n` starts, as a share of the characters.
    fn at_char(text: &str, n: usize) -> f32 {
        n as f32 / text.chars().count() as f32
    }

    #[test]
    fn a_timed_fill_lights_only_the_word_being_sung() {
        let mut fonts = FontSystem::new();
        let key = LyricKey { song: "s".into(), font_px_x100: 3000, wrap_px: 2000, css_scale_x1000: 1000, gap_px_x100: 0, split: true };
        let text = "starry night";
        let (buffer, _) = shape(text, &key, &mut fonts);
        let rows = sweep_rows(&buffer);
        let (s0, s1) = rows[0].span_of(0, 6).unwrap();
        let (n0, n1) = rows[0].span_of(7, 12).unwrap();
        let ink = |a: f32, b: f32| (0..20).map(move |k| a + (b - a) * (k as f32 + 0.5) / 20.0);
        // waiting for "night": all of "starry" lit, none of "night"
        let fill = row_fills(&rows, text, at_char(text, 7), true)[0];
        assert!(ink(s0, s1).all(|x| lit_at(&fill, x) == 1.0), "{fill:?}");
        assert!(ink(n0, n1).all(|x| lit_at(&fill, x) == 0.0), "{fill:?}");
        // a little into "night": "starry" stays whole, nothing past "night" is lit
        let fill = row_fills(&rows, text, at_char(text, 8), true)[0];
        assert!(ink(s0, s1).all(|x| lit_at(&fill, x) == 1.0));
        assert!(lit_at(&fill, n0 + 1.0) > 0.0 && lit_at(&fill, n1 + 40.0) == 0.0);
        // the fill moves smoothly: more of the word is lit a moment later, never less
        let mut before = 0.0;
        for k in 0..=50 {
            let fill = row_fills(&rows, text, (7.0 + 5.0 * k as f32 / 50.0) / 12.0, true)[0];
            let total: f32 = ink(n0, n1).map(|x| lit_at(&fill, x)).sum();
            assert!(total >= before - 1e-3, "step {k}: {total} < {before}");
            before = total;
        }
        assert!(ink(n0, n1).all(|x| lit_at(&row_fills(&rows, text, 1.0, true)[0], x) == 1.0));
    }

    #[test]
    fn the_glow_holds_only_the_last_word() {
        let mut fonts = FontSystem::new();
        let mut cache = SwashCache::new();
        let key = LyricKey { song: "s".into(), font_px_x100: 3000, wrap_px: 2000, css_scale_x1000: 1000, gap_px_x100: 0, split: true };
        let text = "starry night";
        let (buffer, _) = shape(text, &key, &mut fonts);
        let sprite = rasterize(&buffer, &key, &mut fonts, &mut cache, text, false);
        let glow = sprite.glow.expect("glow");
        let (n0, n1) = sprite.rows[0].span_of(7, 12).unwrap();
        let (_, s1) = sprite.rows[0].span_of(0, 6).unwrap();
        assert!(glow.x0 <= n0 + sprite.padding && glow.x0 + glow.width >= n1 + sprite.padding, "covers the word");
        assert!(glow.x0 > s1 + sprite.padding - 2.0 * sprite.padding, "starts near the word, not the line");
        assert_eq!(glow.image.size(0).height.0 as f32, sprite.height, "full size, in the sprite's frame");
    }
}
