//! The lyrics panel, used by the dock and the expanded player. Lines are pre-rendered and blurred and dimmed by
//! distance from the current line.

use crate::i18n::tr;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use crate::focus::FocusStop;
use gpui::{AnyElement, Bounds, Context, FontWeight, Pixels, RenderImage, SharedString, div, px, relative};

use crate::lyrics::{BLUR_LEVELS, LineSprite, LyricJob, LyricKey, LyricLayout, last_word, letter_boxes, row_fills, wave_letters};
use crate::shell::{Layout, Shell};
use crate::theme::motion;
use crate::theme::size::*;
use crate::ui::icon;

/// in CSS pixels.
fn lyric_gap(large: bool) -> f32 {
    if large { 44.0 } else { 20.0 }
}

/// in CSS pixels: where the first line starts and where a synced current line scrolls to.
fn lyric_lead(large: bool, font: f32, peek: bool) -> f32 {
    lyric_gap(large) + if peek { font * LYRIC_PEEK } else { 0.0 }
}

/// Rasterizing is expensive, so a resize waits for the size to settle.
const REWRAP_DEBOUNCE: Duration = Duration::from_millis(140);

/// The sprites for one layout of one song, plus the animation state that goes with it.
pub struct LyricSet {
    pub key: LyricKey,
    pub layout: LyricLayout,
    pub sprites: Vec<Option<LineSprite>>,
    /// in device pixels, from the top of the first line.
    pub tops: Vec<f32>,
    pub total: f32,
}

#[derive(Default)]
pub struct LyricsState {
    pub set: Option<LyricSet>,
    /// Images taken off screen, to free from the GPU atlas on the next frame.
    pub garbage: Vec<Arc<RenderImage>>,
    wanted: Option<LyricKey>,
    requested: Option<LyricKey>,
    generation: u64,
    /// Blur distance currently shown per line, easing toward the true distance.
    displayed: Vec<f32>,
    pub scroll: f32,
    scroll_set: bool,
    /// lines below the current one start `LYRIC_STAGGER` later each.
    moving: Option<LyricMove>,
    /// its risen words drop back together (`motion::lyric_fall`).
    fall: Option<(usize, Instant)>,
    /// a gap marker grows in over `LYRIC_MOVE` from then.
    became: Option<Instant>,
    /// last frame's panel height and sprite scale, so a resize lands at once instead of moving.
    geometry: (f32, f32),
    active: Option<usize>,
    last_frame: Option<Instant>,
    /// The hovered line, drawn sharp.
    pub hovered: Option<usize>,
    pub bounds: Rc<RefCell<Bounds<Pixels>>>,
    clock: LyricClock,
}

struct LyricMove {
    at: Instant,
    from: Vec<f32>,
    to: f32,
    active: usize,
    /// every line eases straight in together, with no spring or stagger.
    plain: bool,
}

impl LyricMove {
    /// `t` seconds into the move, and whether it's still moving.
    fn line(&self, i: usize, now: Instant) -> (f32, bool) {
        let delay = if self.plain { 0.0 } else { i.saturating_sub(self.active) as f32 * motion::LYRIC_STAGGER };
        let t = now.saturating_duration_since(self.at).as_secs_f32() - delay;
        let from = self.from.get(i).copied().unwrap_or(self.to);
        if t <= 0.0 {
            return (from, true);
        }
        // `--lyric-bounce`.
        let curve = if self.plain {
            crate::animation::Curve::ease(motion::LYRIC_MOVE, motion::EASE_OUT)
        } else {
            crate::animation::Curve::spring(motion::LYRIC_MOVE, motion::LYRIC_BOUNCE)
        };
        let (value, _, moving) = crate::animation::sample(curve, from, self.to, 0.0, t);
        (value, moving)
    }
}

/// so the sweep moves every frame instead of in steps.
#[derive(Default)]
struct LyricClock {
    anchor: f32,
    at: Option<Instant>,
    playing: bool,
    last_report: f32,
}

impl LyricClock {
    /// Capped at 0.75 s past the last report so a stalled player can't run ahead.
    fn estimate(&self, now: Instant) -> f32 {
        match self.at {
            Some(at) if self.playing => self.anchor + now.duration_since(at).as_secs_f32().min(0.75),
            _ => self.anchor,
        }
    }

    /// Takes a new report. Small differences are blended so the sweep never steps back, and seeks land at once.
    fn report(&mut self, position: f32, playing: bool, now: Instant) {
        if position == self.last_report && playing == self.playing && self.at.is_some() {
            return;
        }
        let predicted = self.estimate(now);
        self.anchor = if playing && self.playing && (position - predicted).abs() < 0.25 { predicted + (position - predicted) * 0.3 } else { position };
        self.at = Some(now);
        self.playing = playing;
        self.last_report = position;
    }
}

impl LyricsState {
    /// After the window was hidden, jump to the current line on the next frame instead of easing.
    pub fn restart_clock(&mut self) {
        self.last_frame = None;
        self.clock = LyricClock::default();
    }

    /// Takes the sprites off screen. Their images are freed from the GPU atlas with the garbage.
    fn retire_set(&mut self) {
        if let Some(old) = self.set.take() {
            for sprite in old.sprites.into_iter().flatten() {
                self.garbage.extend(sprite.levels);
                self.garbage.extend(sprite.glow.map(|g| g.image));
                if let Some(l) = sprite.letters {
                    self.garbage.extend(l.images.into_iter().chain(l.glows));
                }
            }
        }
    }

    /// Drops every sprite and the record of what was requested, so the next draw lays the lyrics out again. They must
    /// go together, or the panel stays empty until the song or size changes.
    pub fn release(&mut self) -> Vec<Arc<RenderImage>> {
        self.retire_set();
        self.wanted = None;
        self.requested = None;
        // Any pending or in-flight request is for what was just released.
        self.generation += 1;
        std::mem::take(&mut self.garbage)
    }

    fn needs_request(&self, key: &LyricKey) -> bool {
        self.requested.as_ref() != Some(key) && self.wanted.as_ref() != Some(key)
    }
}

impl Shell {
    pub fn apply_lyric_layout(&mut self, key: LyricKey, layout: LyricLayout) {
        if self.lyrics_view.requested.as_ref() != Some(&key) {
            return;
        }
        self.lyrics_view.retire_set();
        let gap = gap_device(&key);
        let mut tops = Vec::with_capacity(layout.heights.len());
        let mut y = 0.0;
        for h in &layout.heights {
            tops.push(y);
            y += h + gap;
        }
        let n = layout.heights.len();
        self.lyrics_view.displayed = vec![BLUR_LEVELS as f32; n];
        self.lyrics_view.set = Some(LyricSet { key, layout, sprites: vec![None; n], tops, total: y });
        self.lyrics_view.scroll_set = false;
    }

    pub fn apply_lyric_line(&mut self, key: LyricKey, ix: usize, sprite: LineSprite) {
        if let Some(set) = self.lyrics_view.set.as_mut() {
            if set.key == key {
                if let Some(slot) = set.sprites.get_mut(ix) {
                    *slot = Some(sprite);
                }
            }
        }
    }

    pub fn drop_lyric_garbage(&mut self, window: &mut gpui::Window) {
        for image in std::mem::take(&mut self.lyrics_view.garbage) {
            let _ = window.drop_image(image);
        }
    }

    /// a size change waits until it settles.
    fn request_lyric_sprites(&mut self, key: LyricKey, immediate: bool, cx: &mut Context<Self>) {
        if !self.lyrics_view.needs_request(&key) {
            return;
        }
        self.lyrics_view.wanted = Some(key.clone());
        self.lyrics_view.generation += 1;
        let generation = self.lyrics_view.generation;
        let near = self.current_lyric_line().unwrap_or(0);
        if immediate {
            self.start_lyric_job(key, near);
            return;
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(REWRAP_DEBOUNCE).await;
            let _ = this.update(cx, |this, _| {
                if this.lyrics_view.generation == generation {
                    this.start_lyric_job(key, near);
                }
            });
        })
        .detach();
    }

    fn start_lyric_job(&mut self, key: LyricKey, near: usize) {
        let lines: Vec<SharedString> = self.lyrics.iter().map(|l| l.text.clone()).collect();
        let rolls = self.lyrics.iter().map(|l| l.timed.is_some()).collect();
        self.lyrics_view.requested = Some(key.clone());
        self.lyric_raster.request(LyricJob { key, lines, rolls, near });
    }

    /// Whether the lyrics have timings to follow: every line synced and more than one start time.
    pub(crate) fn lyrics_synced(&self) -> bool {
        self.lyrics.iter().all(|l| l.synced) && self.lyrics.iter().map(|l| l.start_ms).collect::<std::collections::HashSet<_>>().len() > 1
    }

    /// The synced line being sung, or the first line before any has started.
    pub(crate) fn current_lyric_line(&self) -> Option<usize> {
        let position_ms = (self.player.position * 1000.0) as u32;
        // Before the first line the first line is shown as current.
        if self.lyrics.is_empty() { None } else { Some(self.lyrics.iter().rposition(|l| l.start_ms <= position_ms).unwrap_or(0)) }
    }

    /// word timing where the server has it, else `--lyric-fill-pace`, never past the next line.
    fn lyric_fill_ms(&self, i: usize) -> f32 {
        let Some(l) = self.lyrics.get(i) else { return 1.0 };
        let start = l.start_ms as f32;
        let end = self.lyrics.get(i + 1).map_or(start + 4000.0, |n| n.start_ms as f32);
        if let Some(word) = l.timed.as_ref().and_then(|t| t.words.last()) {
            return (word.start_ms as f32 - start).clamp(1.0, end - start);
        }
        let chars = l.text.chars().count() + l.backing.as_ref().map_or(0, |b| b.chars().count());
        (chars as f32 / LYRIC_FILL_PACE * 1000.0).min(end - start).max(1.0)
    }

    /// `LYRIC_MOVE_LEAD` early but never before the line above has filled; after a gap marker, at its pop.
    fn lyric_move_start_ms(&self, i: usize) -> f32 {
        let Some(l) = self.lyrics.get(i) else { return f32::MAX };
        let start = l.start_ms as f32;
        if i == 0 {
            return start;
        }
        if self.lyric_gap_ms(i - 1).is_some() {
            return start - LYRIC_GAP_POP_LEAD * 1000.0;
        }
        let above_filled = self.lyrics[i - 1].start_ms as f32 + self.lyric_fill_ms(i - 1);
        (start - motion::LYRIC_MOVE_LEAD * 1000.0).max(above_filled).min(start)
    }

    /// the next line takes over when its move starts.
    fn shown_lyric_line(&self, position_ms: f32) -> usize {
        (0..self.lyrics.len()).rposition(|i| self.lyric_move_start_ms(i) <= position_ms).unwrap_or(0)
    }

    /// `top_inset` is the dock head it must clear.
    pub fn render_lyrics_panel(&mut self, large: bool, width: Pixels, top_inset: Pixels, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        if self.lyrics.is_empty() {
            return if self.lyrics_loading { div().into_any_element() } else { self.render_lyrics_empty(top_inset) };
        }
        // Lyrics without timings scroll as plain text.
        if !self.lyrics_synced() {
            return self.render_unsynced_lyrics(large, top_inset, layout);
        }
        let scale = layout.scale;
        let tokens_w = layout.tokens_w;
        let font_tokens = if large { (LYRIC_EXPANDED_SHARE * tokens_w).clamp(LYRIC_EXPANDED_MIN, LYRIC_EXPANDED_MAX) } else { TEXT_SECTION };
        let pad_x = if large { 0.0 } else { 32.0 };
        let gap = lyric_gap(large);
        let wrap = f32::from(width) - 2.0 * f32::from(t.s(pad_x));
        let key = LyricKey {
            song: self.current_song().map(|s| s.id.clone()).unwrap_or_default(),
            font_px_x100: (f32::from(t.s(font_tokens)) * scale * 100.0).round() as u32,
            wrap_px: (wrap * scale).round().max(40.0) as u32,
            css_scale_x1000: ((t.token_scale() * scale) * 1000.0).round() as u32,
            gap_px_x100: (f32::from(t.s(gap)) * scale * 100.0).round() as u32,
            split: self.prefs.backing_vocals_line,
        };
        // During a dock drag the sprites are scaled, and laid out again once it ends.
        let resizing = self.dock_resizing() && self.lyrics_view.set.is_some();
        if !resizing {
            let immediate = self.lyrics_view.set.as_ref().is_none_or(|s| s.key.song != key.song || s.key.font_px_x100 != key.font_px_x100);
            self.request_lyric_sprites(key, immediate, cx);
        }

        let bounds = *self.lyrics_view.bounds.borrow();
        let view_h = f32::from(bounds.size.height).max(1.0);
        let now = Instant::now();
        let playing = self.player.playing && !self.player.waiting_for_server;
        self.lyrics_view.clock.report(self.player.position, playing, now);
        let position_ms = self.lyrics_view.clock.estimate(now) * 1000.0;
        let active = self.shown_lyric_line(position_ms);
        let dt = self.lyrics_view.last_frame.map_or(0.0, |t| now.duration_since(t).as_secs_f32()).min(0.1);
        self.lyrics_view.last_frame = Some(now);
        let previous = self.lyrics_view.active;
        // off: no lift, glow, peek, gap bars or springy moves; the sweep and backing line have their own switches.
        let effects = self.prefs.lyric_effects;
        // off, the current line lights whole at once.
        let sweep_on = self.prefs.lyric_sweep;
        // words rise as the fill reaches them, so they need both.
        let lift_on = effects && sweep_on && !motion::reduced();
        let active_changed = previous != Some(active);
        self.lyrics_view.active = Some(active);
        if active_changed {
            self.lyrics_view.fall = previous.filter(|_| lift_on).map(|p| (p, now));
            self.lyrics_view.became = Some(now);
        }

        // The expanded layout's text starts at the panel edge, so the clip extends past it by the sprites' widest halo.
        // The dock's padding already leaves that room.
        let bleed = if large { t.s(36.0) } else { px(0.0) };
        let mut body = div().relative().size_full();
        let Some(set) = self.lyrics_view.set.as_ref() else {
            return self.lyrics_frame(body.into_any_element(), top_inset, bleed, cx);
        };
        // Sprites are scaled during a drag, so their positions scale with them.
        let draw_scale = (wrap * scale / set.key.wrap_px.max(1) as f32).max(0.1);
        let to_logical = |device: f32| px(device * draw_scale / scale);

        // the current line sits `lead` below the panel's top, so `--lyric-peek` of the line before shows above it.
        let lead = f32::from(t.s(lyric_lead(large, font_tokens, effects))) * scale;
        let peek = if effects { f32::from(t.s(font_tokens * LYRIC_PEEK)) * scale } else { 0.0 };
        let (pad_top, align) = (lead, lead);
        let view_device = view_h * scale;
        // the bracketed part pushes later lines down; an all-bracket line shows it in place.
        let backing = self.lyrics.get(active).and_then(|l| l.backing.clone().map(|b| (b, l.text.is_empty())));
        let font_device = f32::from(t.s(font_tokens)) * scale * draw_scale;
        let backing_h = match &backing {
            Some((_, false)) => (font_device * LYRIC_BACKING_SCALE * 1.3).ceil(),
            _ => 0.0,
        };
        let target = (set.tops.get(active).copied().unwrap_or(0.0) * draw_scale + pad_top - align).max(0.0);
        let max_scroll = ((set.total * draw_scale + backing_h + view_device * if large { 0.70 } else { 0.62 }) - view_device).max(0.0);
        let target = target.min(max_scroll);
        self.lyrics_max_scroll = max_scroll;
        let geometry = (view_device, draw_scale);
        let resized = std::mem::replace(&mut self.lyrics_view.geometry, geometry) != geometry;
        let snap = !self.lyrics_view.scroll_set || active_changed && dt == 0.0 || resized;
        let n = set.tops.len();
        if snap {
            if !(resized && self.lyrics_user_scrolled && self.lyrics_view.scroll_set) {
                self.lyrics_view.scroll = target;
            }
            self.lyrics_view.scroll_set = true;
            self.lyrics_view.moving = None;
        }
        if active_changed {
            self.lyrics_view.hovered = None;
            self.lyrics_user_scrolled = false;
        }
        let mut animating = false;
        // unless the user is scrolling by hand; lines further down move a little later.
        if self.lyrics_user_scrolled {
            self.lyrics_view.moving = None;
        } else if self.lyrics_view.moving.as_ref().map_or((self.lyrics_view.scroll - target).abs() > 0.5, |m| m.to != target) {
            let from = (0..n).map(|i| self.lyrics_view.moving.as_ref().map_or(self.lyrics_view.scroll, |m| m.line(i, now).0)).collect();
            self.lyrics_view.moving = Some(LyricMove { at: now, from, to: target, active, plain: !effects });
        }
        if let Some(m) = &self.lyrics_view.moving {
            self.lyrics_view.scroll = m.line(active, now).0;
        }
        let scroll = self.lyrics_view.scroll.clamp(0.0, max_scroll);
        self.lyrics_view.scroll = scroll;
        let k_blur = motion::follow(dt, motion::LYRIC_DIM);
        let hovered = self.lyrics_view.hovered;
        let (line_start, line_end) = (
            self.lyrics.get(active).map_or(0, |l| l.start_ms) as f32,
            self.lyrics.get(active + 1).map_or_else(|| self.lyrics.get(active).map_or(0, |l| l.start_ms) as f32 + 4000.0, |l| l.start_ms as f32),
        );
        // word timing (`songLyrics` v2), else `--lyric-fill-pace`, never slower than the next line allows.
        let timed_line = self.lyrics.get(active).filter(|_| sweep_on).and_then(|l| l.timed.clone());
        // a pause after the line is held by its last word's glow; with word timing, once held past `--lyric-hold-min`.
        let fill_ms = self.lyric_fill_ms(active);
        let held_from = match timed_line.as_ref().and_then(|l| l.words.last()) {
            Some(word) => word.start_ms as f32,
            None => line_start + fill_ms,
        };
        // the glow is out by the time the list moves on to the next line.
        let shown_until = self.lyric_move_start_ms(active + 1).min(line_end);
        let holding = effects && shown_until - held_from >= LYRIC_HOLD_MIN * 1000.0;
        // a held word is filled before its glow starts to fade, so the fill never lags the glow
        let cut = timed_line.as_ref().filter(|_| holding).map(|l| l.filled_by((shown_until - LYRIC_GLOW_FADE * 1000.0).max(0.0) as u32));
        let timed = cut.as_ref().or(timed_line.as_deref()).and_then(|l| l.sung_fraction(position_ms.max(0.0) as u32));
        let (main_chars, backing_chars) = self.lyrics.get(active).map_or((0, 0), |l| (l.text.chars().count(), l.backing.as_ref().map_or(0, |b| b.chars().count())));
        // so the move starts on time rather than at the player's next report.
        if playing && self.lyric_move_start_ms(active + 1) - position_ms < 250.0 {
            animating = true;
        }
        let line_sweep = if sweep_on { timed.unwrap_or_else(|| ((position_ms - line_start) / fill_ms).clamp(0.0, 1.0)) } else { 1.0 };
        // main words and bracketed part fill in text order, each its share by length; the part sung last gets the held glow.
        let backing_first = self.lyrics.get(active).is_some_and(|l| l.backing_first);
        let main_share = if backing_chars == 0 { 1.0 } else if main_chars == 0 { 0.0 } else { main_chars as f32 / (main_chars + backing_chars) as f32 };
        let (main_from, main_to) = if backing_first { (1.0 - main_share, 1.0) } else { (0.0, main_share) };
        let (backing_from, backing_to) = if backing_first { (0.0, 1.0 - main_share) } else { (main_share, 1.0) };
        let part = |from: f32, to: f32| if to - from <= 0.0 { if line_sweep >= to { 1.0 } else { 0.0 } } else { ((line_sweep - from) / (to - from)).clamp(0.0, 1.0) };
        let sweep = part(main_from, main_to);
        let backing_sweep = part(backing_from, backing_to);
        let main_span_ms = fill_ms * (main_to - main_from).max(0.0001);
        // unclamped, so the first words still lift early when the bracketed part fills before them.
        let main_lead = (line_sweep - main_from) / (main_to - main_from).max(0.0001);
        let main_last = main_chars > 0 && (backing_first || backing_chars == 0);
        let held = holding.then(|| {
            let fade = LYRIC_GLOW_FADE * 1000.0;
            ((position_ms - held_from) / fade).min((shown_until - position_ms) / fade).clamp(0.0, 1.0)
        });
        // frames only while the line is filling or its held glow is changing.
        if playing && (line_sweep < 1.0 || effects && (held.is_some_and(|g| g > 0.0 && g < 1.0) || (position_ms < held_from + LYRIC_GLOW_FADE * 1000.0 && sweep >= 1.0))) {
            animating = true;
        }
        // a glowing last word's letters roll while it's held, only on the server's word timing: (seconds in, seconds held).
        let wave = match (held, timed_line.as_ref().and_then(|l| l.words.last())) {
            (Some(_), Some(word)) if lift_on && main_last => {
                let (since, until) = ((position_ms - word.start_ms as f32) / 1000.0, (shown_until - word.start_ms as f32) / 1000.0);
                (since > 0.0 && since < until).then_some((since, until))
            }
            _ => None,
        };
        if playing && wave.is_some() {
            animating = true;
        }

        let blur_by_distance = self.prefs.lyric_blur;
        let mut lines = Vec::new();
        let mut still_moving = false;
        for i in 0..n {
            let line_scroll = match &self.lyrics_view.moving {
                Some(m) => {
                    let (s, moving) = m.line(i, now);
                    (s, moving)
                }
                None => (scroll, false),
            };
            let top_device = set.tops[i] * draw_scale + pad_top - line_scroll.0 + if i > active { backing_h } else { 0.0 };
            let height_device = set.layout.heights[i] * draw_scale;
            if top_device + height_device < -400.0 || top_device > view_device + 400.0 {
                self.lyrics_view.displayed[i] = i.abs_diff(active).min(BLUR_LEVELS - 1) as f32;
                continue;
            }
            still_moving |= line_scroll.1;
            let goal = i.abs_diff(active).min(BLUR_LEVELS - 1) as f32;
            let shown = &mut self.lyrics_view.displayed[i];
            if snap {
                *shown = goal;
            } else {
                *shown += (goal - *shown) * k_blur;
                if (*shown - goal).abs() > 0.01 {
                    animating = true;
                } else {
                    *shown = goal;
                }
            }
            let shown = *shown;
            // GPUI has no masks, so each line fades whole near the panel's top and bottom. At the top the fade is clear
            // for `LYRIC_TOP_CLEAR` of the lead room, then ramps to full at the current line, using the value at each
            // line's middle.
            let center = (top_device + height_device / 2.0) / view_device.max(1.0);
            let clear = peek * LYRIC_TOP_CLEAR;
            let mask = move |y: f32| ((y - clear) / (peek - clear).max(1.0)).clamp(0.0, 1.0);
            let bottom_edge = ((1.0 - center) / 0.12).clamp(0.0, 1.0);
            let banded = i != active && top_device < peek;
            let edge = if banded { bottom_edge } else { mask(top_device + height_device / 2.0).min(bottom_edge) };
            if edge <= 0.01 || (banded && mask(top_device + height_device) <= 0.01) {
                continue;
            }
            let is_active = i == active;
            let is_hovered = hovered == Some(i) && !is_active;
            let opacity = if is_active {
                1.0
            } else if is_hovered {
                LYRIC_HOVER
            } else {
                (LYRIC_DIM_NEAR - shown * LYRIC_DIM_STEP).max(LYRIC_DIM_MIN).max(if shown < 1.0 { LYRIC_DIM_NEAR + (1.0 - LYRIC_DIM_NEAR) * (1.0 - shown) } else { 0.0 })
            };
            // laid out on a whole device pixel, since layout rounds lengths; the rest is a paint offset, so lines glide
            let (base, mut down) = (top_device.floor(), top_device - top_device.floor());
            let (top, height) = (px(base / scale), to_logical(set.layout.heights[i]));
            let mut line = div()
                .id(("lyric", i))
                .absolute()
                .left(t.s(pad_x))
                .top(top)
                .w(px(wrap))
                .h(height)
                .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                    let lv = &mut this.lyrics_view;
                    if *hovered {
                        lv.hovered = Some(i);
                    } else if lv.hovered == Some(i) {
                        lv.hovered = None;
                    }
                    cx.notify();
                }))
                .stop(&t).on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(ms) = this.lyrics.get(i).map(|l| l.start_ms) {
                        if this.core.is_some() {
                            this.core_seek_ms(u64::from(ms), cx);
                        } else {
                            this.player.position = ms as f32 / 1000.0;
                        }
                        this.lyrics_user_scrolled = false;
                        cx.notify();
                    }
                }));
            if let Some(sprite) = set.sprites[i].as_ref() {
                let draw_w = to_logical(sprite.width);
                let draw_h = to_logical(sprite.height);
                let pad = to_logical(sprite.padding);
                // GPUI's atlas packs images edge to edge and samples linearly, so a sprite drawn scaled or off the
                // pixel grid picks up its neighbor's outer pixels. Sprite margins are empty, so each one is clipped a
                // few device pixels inside its edge.
                let inset = px(SPRITE_EDGE / scale);
                let image = |level: usize, alpha: f32| clipped_sprite(sprite.levels[level].clone(), -pad, -pad, draw_w, draw_h, inset, alpha * edge);
                if is_active {
                    // A line that's becoming current eases in from blurred.
                    let blur_mix = shown.clamp(0.0, 1.0);
                    let sharp = 1.0 - blur_mix;
                    let w = f32::from(draw_w);
                    let h = f32::from(draw_h);
                    let p = f32::from(pad);
                    let lo = f32::from(inset);
                    let snap = |x: f32| (x * scale).round() / scale;
                    let rows = &sprite.rows;
                    // the first and last rows reach the sprite's edges, so nothing in the padding is cut.
                    let row_y = |r: usize| {
                        let row = &rows[r];
                        (snap(if r == 0 { lo } else { p + f32::from(to_logical(row.top)) }), snap(if r + 1 == rows.len() { h - lo } else { p + f32::from(to_logical(row.top + row.height)) }))
                    };
                    let text = self.lyrics.get(i).map(|l| l.text.clone()).unwrap_or_default();
                    // each row's lit stretches on whole device pixels, in reading order.
                    let fills: Vec<Vec<(f32, f32, f32)>> = row_fills(rows, &text, sweep, timed_line.is_some())
                        .iter()
                        .map(|fill| fill.pieces(LYRIC_SWEEP_STEP_PX / draw_scale).into_iter().map(|(a, b, lit)| (snap(p + a * draw_scale / scale), snap(p + b * draw_scale / scale), lit)).collect())
                        .collect();
                    // the blurred copy while it sharpens, then each stretch once, as opaque as it's lit, so sung and
                    // unsung letters keep one weight. `letters`: the rolling word's even (0) or odd (1) letters.
                    let letter_images = sprite.letters.as_ref();
                    let layers = |wx0: f32, wx1: f32, wy0: f32, wy1: f32, mul: f32, letters: Option<usize>| {
                        let mut out: Vec<gpui::Div> = Vec::new();
                        let source = match letters.zip(letter_images) {
                            Some((k, l)) => (l.images[k].clone(), to_logical(l.x0), to_logical(l.width)),
                            None => (sprite.levels[0].clone(), px(0.0), draw_w),
                        };
                        if letters.is_none() && blur_mix > 0.02 {
                            out.push(image(1, blur_mix * mul));
                        }
                        let (x_lo, x_hi) = (lo.max(wx0), (w - lo).min(wx1));
                        for (r, pieces) in fills.iter().enumerate() {
                            let (y0, y1) = row_y(r);
                            let (y0, y1) = (y0.max(wy0), y1.min(wy1));
                            if y1 <= y0 || x_hi <= x_lo {
                                continue;
                            }
                            let mut stretch = |x0: f32, x1: f32, lit: f32| {
                                if x1 > x0 {
                                    let alpha = (LYRIC_UNSUNG + (1.0 - LYRIC_UNSUNG) * lit) * sharp * edge * mul;
                                    out.push(sprite_stretch(source.0.clone(), -pad, source.1, source.2, draw_w, draw_h, Bounds::from_corners(gpui::point(px(x0), px(y0)), gpui::point(px(x1), px(y1))), alpha));
                                }
                            };
                            let mut x = x_lo;
                            for &(a, b, lit) in pieces {
                                let (a, b) = (a.clamp(x, x_hi), b.clamp(x, x_hi));
                                stretch(x, a, 0.0);
                                stretch(a, b, lit);
                                x = b;
                            }
                            stretch(x, x_hi, 0.0);
                        }
                        out
                    };
                    // each word rises whole as the fill nears it, so the line is drawn in windows per word.
                    let lead_ms = motion::LYRIC_WORD_LIFT_LEAD * 1000.0;
                    let lift_at = |since_ms: f32| if since_ms <= 0.0 { 0.0 } else { motion::cubic_bezier(motion::LYRIC_WORD_LIFT_EASE, motion::progress(since_ms / 1000.0, motion::LYRIC_WORD_LIFT_TIME)) };
                    let words: Vec<(usize, f32, f32, f32)> = if !lift_on {
                        Vec::new()
                    } else if let Some(timed) = &timed_line {
                        timed
                            .words
                            .iter()
                            .filter_map(|word| {
                                let lift = lift_at(position_ms - word.start_ms as f32 + lead_ms);
                                rows.iter().enumerate().find_map(|(r, row)| row.span_of(word.start as usize, word.end as usize).map(|(a, b)| (r, p + f32::from(to_logical(a)), p + f32::from(to_logical(b)), lift)))
                            })
                            .collect()
                    } else {
                        // each word starts as the sweep reaches its first glyph.
                        let total: usize = rows.iter().map(|r| r.glyphs).sum::<usize>().max(1);
                        let span_ms = main_span_ms;
                        let mut found = Vec::new();
                        let mut from: Option<usize> = None;
                        let bytes: Vec<(usize, char)> = text.char_indices().chain(std::iter::once((text.len(), ' '))).collect();
                        for &(at, c) in &bytes {
                            if c.is_whitespace() {
                                if let Some(start) = from.take() {
                                    let mut before = 0;
                                    for (r, row) in rows.iter().enumerate() {
                                        if let Some((a, b)) = row.span_of(start, at) {
                                            let width = (row.x1 - row.x0).max(1.0);
                                            let into = if row.rtl { (row.x1 - b) / width } else { (a - row.x0) / width };
                                            let word_sweep = (before as f32 + into.clamp(0.0, 1.0) * row.glyphs as f32) / total as f32;
                                            let lift = lift_at((main_lead - word_sweep) * span_ms + lead_ms);
                                            found.push((r, p + f32::from(to_logical(a)), p + f32::from(to_logical(b)), lift));
                                            break;
                                        }
                                        before += row.glyphs;
                                    }
                                }
                            } else if from.is_none() {
                                from = Some(at);
                            }
                        }
                        found
                    };
                    if words.iter().any(|l| l.3 > 0.0 && l.3 < 1.0) && playing {
                        animating = true;
                    }
                    // in device pixels, not rounded: the paint offset moves sprites by fractions of a pixel.
                    let rise = font_device * LYRIC_WORD_LIFT;
                    let last_lift = words.last().map_or(0.0, |l| rise * l.3);
                    // each rolling letter (index, left, right, device pixels up) in row device pixels, between the middles of the
                    // letters beside it, drawn whole from its own image; and the rolling word's own window, left out of the line.
                    let mut rolling: Vec<(usize, f32, f32, f32)> = Vec::new();
                    let mut rolling_word: Option<(usize, f32, f32)> = None;
                    if let (Some((since, until)), Some(_)) = (wave, letter_images) {
                        let timed_text: String = timed_line.as_ref().map(|l| l.text.clone()).unwrap_or_default();
                        let (start, end) = last_word(&timed_text);
                        if let (Some((r, boxes)), Some((wa, wb))) = (wave_letters(&timed_text, start, end).and_then(|l| letter_boxes(rows, &l)), rows.iter().find_map(|row| row.span_of(start, end))) {
                            let (la, lb) = (p + f32::from(to_logical(wa)), p + f32::from(to_logical(wb)));
                            let base = words.iter().filter(|e| e.0 == r && e.1 < lb && e.2 > la).map(|e| rise * e.3).fold(0.0, f32::max);
                            rolling_word = Some((r, la, lb));
                            // room for ink past the outer letters' boxes.
                            let room = rows[r].height * 0.5;
                            let mut order: Vec<(usize, (f32, f32))> = boxes.into_iter().enumerate().collect();
                            order.sort_by(|a, b| a.1.0.total_cmp(&b.1.0));
                            let middle = |j: usize| (order[j].1.0 + order[j].1.1) / 2.0;
                            for (j, &(k, (x0, x1))) in order.iter().enumerate() {
                                let left = if j == 0 { x0.min(wa) - room } else { middle(j - 1) };
                                let right = if j + 1 == order.len() { x1.max(wb) + room } else { middle(j + 1) };
                                rolling.push((k, left, right, base + font_device * motion::lyric_wave(k, since, until)));
                            }
                        }
                    }
                    let moving = |div: gpui::Div, up: f32| if up == 0.0 { div } else { div.paint_offset(gpui::point(px(0.0), px(-up / scale))) };
                    if words.iter().all(|l| l.3 <= 0.0) && rolling.is_empty() {
                        line = line.children(layers(0.0, w, 0.0, h, 1.0, None));
                    } else {
                        let window = |x0: f32, x1: f32, y0: f32, y1: f32, up: f32, letters: Option<usize>| {
                            // on whole device pixels, so neighbouring windows meet without a gap or overlap
                            let (x0, x1) = (snap(x0), snap(x1));
                            moving(
                                div().absolute().left(-pad + px(x0)).top(-pad + px(y0)).w(px((x1 - x0).max(0.0))).h(px((y1 - y0).max(0.0))).overflow_hidden().child(
                                    div().absolute().left(px(-x0) + pad).top(px(-y0) + pad).children(layers(x0, x1, y0, y1, 1.0, letters)),
                                ),
                                up,
                            )
                        };
                        for r in 0..rows.len() {
                            let (y0, y1) = row_y(r);
                            let mut on_row: Vec<(f32, f32, f32)> = words.iter().filter(|l| l.0 == r).map(|l| (l.1, l.2, rise * l.3)).collect();
                            on_row.sort_by(|a, b| a.0.total_cmp(&b.0));
                            // each word owns the room halfway to its neighbours, so a letter's overhang rises with it.
                            let mut x = 0.0;
                            for (j, &(a, b, up)) in on_row.iter().enumerate() {
                                let right = on_row.get(j + 1).map_or(w, |n| (b + n.0) / 2.0);
                                if !rolling_word.is_some_and(|(wr, la, lb)| wr == r && a < lb && b > la) {
                                    line = line.child(window(x, right, y0, y1, up, None));
                                }
                                x = right;
                            }
                            if on_row.is_empty() {
                                line = line.child(window(0.0, w, y0, y1, 0.0, None));
                            }
                            if rolling_word.is_some_and(|(wr, _, _)| wr == r) {
                                for &(k, left, right, up) in &rolling {
                                    line = line.child(window(p + f32::from(to_logical(left)), p + f32::from(to_logical(right)), y0, y1, up, Some(k % 2)));
                                }
                            }
                        }
                    }
                    // the held last word's own halo, risen as far as the word.
                    if let Some(g) = held.filter(|&g| g > 0.0 && main_last) {
                        let alpha = g * sharp * edge;
                        match letter_images.filter(|_| !rolling.is_empty()) {
                            None => {
                                if let Some(glow) = sprite.glow.as_ref() {
                                    line = line.child(moving(clipped_sprite(glow.image.clone(), -pad + to_logical(glow.x0), -pad, to_logical(glow.width), draw_h, inset, alpha), last_lift));
                                }
                            }
                            Some(letters) => {
                                // each letter's halo, from its own image, travels with it.
                                for &(k, left, right, up) in &rolling {
                                    let (x0, x1) = (p + f32::from(to_logical(left)), p + f32::from(to_logical(right)));
                                    line = line.child(moving(
                                        div().absolute().left(-pad + px(x0)).top(-pad + px(lo)).w(px(x1 - x0)).h(px(h - 2.0 * lo)).overflow_hidden().child(
                                            crate::ui::image(letters.glows[k % 2].clone()).absolute().left(px(-x0) + to_logical(letters.x0)).top(px(-lo)).w(to_logical(letters.width)).h(draw_h).opacity(alpha),
                                        ),
                                        up,
                                    ));
                                }
                            }
                        }
                    }
                    // backing vocals, smaller, with the same sweep.
                    if let Some((text, alone)) = backing.clone() {
                        let size = px(font_device * LYRIC_BACKING_SCALE / scale);
                        let top = if alone { px(0.0) } else { height };
                        let text_at = |alpha: f32| div().absolute().left(px(0.0)).top(px(0.0)).w(px(wrap)).text_size(size).line_height(relative(1.3)).font_weight(FontWeight::BOLD).whitespace_nowrap().text_color(t.t_on_art()).opacity(alpha * sharp * edge);
                        let mut part = div()
                            .absolute()
                            .left(px(0.0))
                            .top(top)
                            .w(px(wrap))
                            .h(px(backing_h.max(font_device * LYRIC_BACKING_SCALE * 1.3) / scale))
                            .child(text_at(LYRIC_UNSUNG).child(text.clone()))
                            .child(div().absolute().left(px(0.0)).top(px(0.0)).bottom(px(0.0)).w(relative(backing_sweep)).overflow_hidden().child(text_at(1.0).child(text.clone())));
                        // sung last, it holds the glow: its last word blurred live, laid out with the text it sits on.
                        if let Some(g) = held.filter(|&g| g > 0.0 && !main_last) {
                            let (start, end) = last_word(&text);
                            let ink = gpui::HighlightStyle { color: Some(t.t_on_art().into()), ..Default::default() };
                            for (sigma, alpha) in crate::lyrics::glow_layers(f32::from(size) * LYRIC_LINE_HEIGHT) {
                                let word = gpui::StyledText::new(text.clone()).with_highlights([(start..end, ink)]);
                                part = part.child(text_at(alpha * g).text_color(gpui::transparent_black()).blur(px(sigma)).child(word));
                            }
                        }
                        line = line.child(part);
                    }
                    if let Some(bars) = self.gap_marker(i, position_ms, playing, font_device / scale, height, edge, now) {
                        line = line.child(bars);
                    }
                } else {
                    let parts: Vec<(usize, f32)> = if is_hovered || !blur_by_distance {
                        vec![(0, opacity)]
                    } else {
                        let level_f = shown.clamp(0.0, BLUR_LEVELS as f32 - 1.001);
                        let (low, mix) = (level_f.floor() as usize, level_f.fract());
                        [(low, 1.0 - mix), (low + 1, mix)].into_iter().filter(|&(level, alpha)| alpha > 0.02 && level < BLUR_LEVELS).map(|(level, alpha)| (level, alpha * opacity)).collect()
                    };
                    if banded {
                        // bands on whole device pixels from the line's laid-out top, so their edges meet exactly,
                        // down to the end of the top fade, then the rest whole
                        let (d_top, d_bottom) = ((-f32::from(pad) * scale).floor(), ((f32::from(draw_h) - f32::from(pad)) * scale).ceil());
                        let d_full = (peek - top_device).ceil().clamp(d_top, d_bottom);
                        let band = |d0: f32, d1: f32, mul: f32| {
                            div().absolute().left(-pad).top(px(d0 / scale)).w(draw_w).h(px((d1 - d0).max(0.0) / scale)).overflow_hidden().child(
                                div().absolute().left(pad).top(px(-d0 / scale)).children(parts.iter().map(|&(level, alpha)| image(level, alpha * mul))),
                            )
                        };
                        let mut d = d_top;
                        while d < d_full {
                            let d1 = (d + LYRIC_PEEK_BAND_PX).min(d_full);
                            let mul = mask(top_device + (d + d1) / 2.0);
                            if mul > 0.01 {
                                line = line.child(band(d, d1, mul));
                            }
                            d = d1;
                        }
                        if d_full < d_bottom {
                            line = line.child(band(d_full, d_bottom, 1.0));
                        }
                    } else {
                        line = line.children(parts.iter().map(|&(level, alpha)| image(level, alpha)));
                    }
                    // the line that was just current drops its risen words back together.
                    if let Some((_, since)) = self.lyrics_view.fall.filter(|f| f.0 == i) {
                        if let Some(left) = motion::lyric_fall(now.saturating_duration_since(since).as_secs_f32()) {
                            animating = true;
                            down -= font_device * LYRIC_WORD_LIFT * left;
                        }
                    }
                }
            } else {
                // The sprite is still loading, so plain text holds the line's place.
                let text = self.lyrics.get(i).map(|l| l.text.clone()).unwrap_or_default();
                line = line.child(
                    div()
                        .absolute()
                        .inset_0()
                        .text_size(px(set.key.font_px() / scale * draw_scale))
                        .line_height(relative(LYRIC_LINE_HEIGHT))
                        .font_weight(FontWeight::BOLD)
                        .text_color(t.t_on_art())
                        .opacity(opacity * edge)
                        .child(text),
                );
            }
            if down != 0.0 {
                line = line.paint_offset(gpui::point(px(0.0), px(down / scale)));
            }
            lines.push(line.into_any_element());
        }
        // who wrote the words, a line gap after the last line.
        if let Some(credit) = self.lyrics_credit(f32::from(to_logical(set.key.font_px()))) {
            let end_scroll = self.lyrics_view.moving.as_ref().map_or((scroll, false), |m| m.line(n.saturating_sub(1), now)).0;
            let top = px((set.total * draw_scale + pad_top - end_scroll) / scale);
            lines.push(div().absolute().left(t.s(pad_x)).top(top).w(px(wrap)).child(credit).into_any_element());
        }
        if still_moving {
            animating = true;
        } else {
            self.lyrics_view.moving = None;
        }
        if animating {
            self.lyrics_animating = true;
        }
        body = body.children(lines);
        self.lyrics_frame(body.into_any_element(), top_inset, bleed, cx)
    }

    /// an empty timed line at least `LYRIC_GAP_MIN` before the next one.
    fn lyric_gap_ms(&self, i: usize) -> Option<(f32, f32)> {
        let l = self.lyrics.get(i)?;
        if !l.text.is_empty() || l.backing.is_some() {
            return None;
        }
        let (start, end) = (l.start_ms as f32, self.lyrics.get(i + 1)?.start_ms as f32);
        (end - start >= LYRIC_GAP_MIN * 1000.0).then_some((start, end))
    }

    /// the pop finishes just before the next line moves in.
    #[allow(clippy::too_many_arguments)]
    fn gap_marker(&self, i: usize, position_ms: f32, playing: bool, font: f32, height: Pixels, edge: f32, now: Instant) -> Option<AnyElement> {
        if !self.prefs.lyric_effects {
            return None;
        }
        let (start, end) = self.lyric_gap_ms(i)?;
        let t = self.theme;
        let fill = ((position_ms - start) / (end - start)).clamp(0.0, 1.0);
        let grow = self.lyrics_view.became.map_or(1.0, |at| motion::progress(now.saturating_duration_since(at).as_secs_f32(), motion::LYRIC_MOVE));
        let vis = motion::cubic_bezier(motion::EASE_OUT, grow);
        // so the next line comes in on an empty row
        let pop_end = end - LYRIC_GAP_POP_LEAD * 1000.0;
        let secs = |ms: f32| ms / 1000.0;
        let swell = ((secs(position_ms) - secs(pop_end) + LYRIC_GAP_POP + LYRIC_GAP_SWELL) / LYRIC_GAP_SWELL).clamp(0.0, 1.0);
        let pop = ((secs(position_ms) - secs(pop_end) + LYRIC_GAP_POP) / LYRIC_GAP_POP).clamp(0.0, 1.0);
        if pop >= 1.0 {
            return None;
        }
        let reduced = motion::reduced();
        let ease_out = |x: f32| 1.0 - (1.0 - x).powi(3);
        // reduce motion: no swell or bounce, the pop is a plain fade
        let (scale, alpha) = if reduced { (1.0, 1.0 - pop) } else { ((1.0 + LYRIC_GAP_SWELL_GROW * ease_out(swell)) * (1.0 - pop.powi(3)), 1.0) };
        let song_secs = secs(position_ms);
        let bars = gap_bars(font, f32::from(height), song_secs, fill, swell, scale, vis, reduced);
        let ink = gpui::Hsla::from(t.t_on_art());
        let glow_up = 1.0 + swell;
        let glow = move |a: f32, blur: f32| gpui::BoxShadow { color: ink.opacity((a * edge * vis * alpha).min(1.0)), offset: gpui::point(px(0.0), px(0.0)), blur_radius: px(blur), spread_radius: px(0.0), inset: false };
        let ticking = grow < 1.0 || playing;
        let rest_w = font * (LYRIC_GAP_BARS as f32 * LYRIC_GAP_WIDTH + (LYRIC_GAP_BARS - 1) as f32 * LYRIC_GAP_BAR_GAP);
        // laid out once; each bar is painted at its own fractional bounds so its size moves smoothly
        let paint = move |bounds: Bounds<Pixels>, _: (), window: &mut gpui::Window, _: &mut gpui::App| {
            if ticking {
                crate::animation::request_frame(window);
            }
            window.with_exact_paint(|window| {
                for bar in &bars {
                    let rect = Bounds::new(bounds.origin + gpui::point(px(bar.left), px(bar.top)), gpui::size(px(bar.width), px(bar.height)));
                    let round = gpui::Corners::all(px(bar.width.min(bar.height) / 2.0));
                    if crate::animation::log_enabled() && bar.index == 0 {
                        eprintln!("navia: gap-bar {:.2} ms height {:.4} device {:.4} song {song_secs:.3} s", crate::testing::since_start_ms(), bar.height, bar.height * window.scale_factor());
                    }
                    window.paint_quad(gpui::fill(rect, ink.opacity(LYRIC_UNSUNG * edge * vis * alpha)).corner_radii(round));
                    if bar.lit > 0.0 {
                        let up = bar.lit * glow_up;
                        window.paint_drop_shadows(rect, round, &[glow(LYRIC_GLOW_TIGHT_ALPHA * up, font * LYRIC_GLOW_TIGHT * 4.0 * glow_up), glow(LYRIC_GLOW_WIDE_ALPHA * up, font * LYRIC_GLOW_WIDE * 4.0 * glow_up)]);
                        window.paint_quad(gpui::fill(rect, ink.opacity(bar.lit * edge * vis * alpha)).corner_radii(round));
                    }
                }
            });
        };
        Some(gpui::canvas(|_, _, _| {}, paint).absolute().left(px(0.0)).top(px(0.0)).w(px(rest_w)).h(height).into_any_element())
    }

    fn lyrics_credit(&self, font: f32) -> Option<gpui::Div> {
        let names = self.lyrics_by.clone()?;
        let t = self.theme;
        Some(
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .gap_x(px(font * LYRICS_CREDIT_SCALE * 0.3))
                .text_size(px(font * LYRICS_CREDIT_SCALE))
                .line_height(relative(1.35))
                .text_color(t.t_on_art())
                .opacity(LYRICS_CREDIT_OPACITY)
                .child(div().font_weight(FontWeight::BOLD).child(tr("Written By:")))
                .child(div().child(names)),
        )
    }

    fn lyrics_frame(&mut self, body: AnyElement, top_inset: Pixels, bleed: Pixels, cx: &mut Context<Self>) -> AnyElement {
        let bounds_store = self.lyrics_view.bounds.clone();
        div()
            .absolute()
            .left(px(0.0))
            .right(px(0.0))
            .top(top_inset)
            .bottom(px(0.0))
            .child(crate::view_topbar::track_bounds(bounds_store))
            .child(
                div()
                    .id("lyrics-scroll")
                    .absolute()
                    .left(-bleed)
                    .right(-bleed)
                    .top(-bleed)
                    .bottom(-bleed)
                    .overflow_hidden()
                    .on_scroll(cx.listener(|this, event: &gpui::ScrollWheelEvent, window, cx| {
                        let dy = f32::from(event.delta.pixel_delta(window.line_height()).y);
                        this.lyrics_user_scrolled = true;
                        this.lyrics_view.scroll -= dy * window.scale_factor();
                        cx.notify();
                    }))
                    .child(div().absolute().left(bleed).right(bleed).top(bleed).bottom(bleed).child(body)),
            )
            .into_any_element()
    }

    fn render_unsynced_lyrics(&mut self, large: bool, top_inset: Pixels, layout: &Layout) -> AnyElement {
        let t = self.theme;
        let font_tokens = if large { (LYRIC_EXPANDED_SHARE * layout.tokens_w).clamp(LYRIC_EXPANDED_MIN, LYRIC_EXPANDED_MAX) * LYRIC_PLAIN_SCALE } else { TEXT_LG };
        let mut column = div().flex().flex_col().gap(t.s(if large { LYRIC_PLAIN_GAP } else { 10.0 })).px(t.s(if large { 0.0 } else { 32.0 })).pt(t.s(lyric_lead(large, font_tokens, true))).pb(t.s(40.0));
        for line in &self.lyrics {
            column = column.child(div().text_size(t.text(font_tokens)).font_weight(FontWeight::SEMIBOLD).line_height(relative(1.35)).text_color(gpui::Hsla::from(t.t_on_art()).opacity(LYRIC_PLAIN)).child(line.text.clone()));
        }
        column = column.children(self.lyrics_credit(f32::from(t.text(font_tokens))));
        div()
            .id("lyrics-plain")
            .absolute()
            .left(px(0.0))
            .right(px(0.0))
            .top(top_inset)
            .bottom(px(0.0))
            .overflow_y_scroll()
            .child(column)
            .into_any_element()
    }

    fn render_lyrics_empty(&mut self, top_inset: Pixels) -> AnyElement {
        let t = self.theme;
        div()
            .absolute()
            .left(px(0.0))
            .right(px(0.0))
            .top(top_inset)
            .bottom(px(0.0))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(t.s(8.0))
            .p(t.s(32.0))
            .text_center()
            .child(icon(&t, "quotes-regular", 26.0, t.t3()))
            .child(div().text_size(t.text(TEXT_LG)).font_weight(FontWeight::SEMIBOLD).text_color(t.t1()).child(tr("Lyrics aren't available")))
            .child(div().text_color(t.t2()).child(tr("The server has no synced lines for this song.")))
            .into_any_element()
    }
}

fn gap_device(key: &LyricKey) -> f32 {
    key.gap_px_x100 as f32 / 100.0
}

/// How far inside its edge a lyric sprite is clipped, in device pixels. Enough to stay clear of the atlas's linear
/// sampling into a neighbor at the quarter-size glow's scale.
const SPRITE_EDGE: f32 = 3.0;
/// the width of a fade's strips in device pixels.
const LYRIC_SWEEP_STEP_PX: f32 = 2.0;
/// GPUI has no masks, so the top fade is drawn in bands this many device pixels tall.
const LYRIC_PEEK_BAND_PX: f32 = 1.0;

/// the sprite box at (`at`, `at`) with `image` drawn `x`..`x + w` across it, shown only in `clip` (sprite box
/// coordinates, on whole device pixels). painted from the box's own origin rather than laid out per stretch, so
/// stretches side by side all place the image on the same pixels and their clips meet with no column drawn twice.
fn sprite_stretch(image: Arc<RenderImage>, at: Pixels, x: Pixels, w: Pixels, box_w: Pixels, h: Pixels, clip: Bounds<Pixels>, alpha: f32) -> gpui::Div {
    let paint = move |bounds: Bounds<Pixels>, _: (), window: &mut gpui::Window, _: &mut gpui::App| {
        let scale = window.scale_factor();
        let device = |v: Pixels| px((f32::from(v) * scale).round() / scale);
        let origin = gpui::point(device(bounds.origin.x), device(bounds.origin.y));
        let mask = stretch_mask(origin, clip, scale);
        let frame = Bounds::new(origin + gpui::point(x, px(0.0)), gpui::size(w, h));
        let placed = gpui::ObjectFit::Cover.get_bounds(frame, image.size(0));
        window.with_content_mask(Some(gpui::ContentMask { bounds: mask }), |window| {
            let _ = window.paint_image(frame, placed, gpui::Corners::default(), image.clone(), 0, false);
        });
    };
    div().absolute().left(at).top(at).w(box_w).h(h).opacity(alpha).child(gpui::canvas(|_, _, _| {}, paint).absolute().inset_0())
}

/// `clip` moved to `origin`, a hair inside each edge: a mask covers any device pixel its edge touches.
fn stretch_mask(origin: gpui::Point<Pixels>, clip: Bounds<Pixels>, scale: f32) -> Bounds<Pixels> {
    let hair = px(0.01 / scale);
    Bounds::from_corners(origin + clip.origin + gpui::point(hair, hair), origin + clip.bottom_right() - gpui::point(hair, hair))
}

/// see `SPRITE_EDGE`.
fn clipped_sprite(image: Arc<RenderImage>, left: Pixels, top: Pixels, w: Pixels, h: Pixels, inset: Pixels, alpha: f32) -> gpui::Div {
    div()
        .absolute()
        .left(left + inset)
        .top(top + inset)
        .w((w - inset * 2.0).max(px(0.0)))
        .h((h - inset * 2.0).max(px(0.0)))
        .overflow_hidden()
        .child(crate::ui::image(image).absolute().left(-inset).top(-inset).w(w).h(h).opacity(alpha))
}

/// one gap marker bar where it's painted, in logical pixels from the row's top left.
#[derive(Clone, Copy, Debug, PartialEq)]
struct GapBar {
    index: usize,
    left: f32,
    top: f32,
    width: f32,
    height: f32,
    /// 0 to 1.
    lit: f32,
}

/// bar `bar`'s bounce, 0 to 1, `secs` into the song: its own smooth loop, or still under reduced motion.
fn gap_bounce(bar: usize, secs: f32, reduced: bool) -> f32 {
    if reduced {
        return LYRIC_GAP_BAR_STILL;
    }
    let b = bar as f32;
    0.5 + 0.5 * (secs * (LYRIC_GAP_BOUNCE_RATE + b * LYRIC_GAP_BOUNCE_SPREAD) + b * LYRIC_GAP_BOUNCE_PHASE).sin()
}

/// the gap marker's bars on a row `row_h` tall at type size `font`: bounced, lit left to right by `fill`, pulled to
/// full height and lit by `swell`, sized by `scale` from the row's left and grown in by `vis`; fractional throughout.
#[allow(clippy::too_many_arguments)]
fn gap_bars(font: f32, row_h: f32, secs: f32, fill: f32, swell: f32, scale: f32, vis: f32, reduced: bool) -> [GapBar; LYRIC_GAP_BARS] {
    let smooth = |x: f32| {
        let x = x.clamp(0.0, 1.0);
        x * x * (3.0 - 2.0 * x)
    };
    let (width, gap) = (font * LYRIC_GAP_WIDTH * scale, font * LYRIC_GAP_BAR_GAP * scale);
    let (low, high) = (font * LYRIC_GAP_BAR_MIN * scale, font * LYRIC_GAP_HEIGHT * scale);
    std::array::from_fn(|index| {
        let bounce = gap_bounce(index, secs, reduced);
        let level = bounce + (1.0 - bounce) * swell * swell;
        let height = ((low + level * (high - low)) * vis).max(0.0);
        let lit = smooth(fill * LYRIC_GAP_BARS as f32 - index as f32).max(swell);
        GapBar { index, left: index as f32 * (width + gap), top: (row_h - height) / 2.0, width, height, lit }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::art::into_render_image;

    fn key() -> LyricKey {
        LyricKey { song: "song".into(), font_px_x100: 2400, wrap_px: 600, css_scale_x1000: 1000, gap_px_x100: 2000, split: true }
    }

    fn sprite() -> LineSprite {
        let image = || into_render_image(image::RgbaImage::new(2, 2));
        LineSprite { levels: vec![image(), image()], width: 2.0, height: 2.0, padding: 0.0, glow: Some(crate::lyrics::WordGlow { image: image(), x0: 0.0, width: 2.0 }), rows: Vec::new(), letters: None }
    }

    /// as before the window is hidden.
    fn laid_out() -> LyricsState {
        let mut state = LyricsState { requested: Some(key()), wanted: Some(key()), ..Default::default() };
        state.set = Some(LyricSet { key: key(), layout: LyricLayout { heights: vec![10.0, 10.0] }, sprites: vec![Some(sprite()), None], tops: vec![0.0, 20.0], total: 40.0 });
        state
    }

    #[test]
    fn gap_bars_bounce_on_their_own_loops() {
        for k in 0..50 {
            let secs = k as f32 * 0.37;
            for bar in 0..LYRIC_GAP_BARS {
                let b = bar as f32;
                let expected = 0.5 + 0.5 * (secs * (2.1 + b * 0.37) + b * 1.9).sin();
                assert!((gap_bounce(bar, secs, false) - expected).abs() < 1e-6);
                assert_eq!(gap_bounce(bar, secs, true), LYRIC_GAP_BAR_STILL);
            }
        }
    }

    /// sampled at 200 frames a second the heights move by small fractional steps, never a whole pixel at a time.
    #[test]
    fn gap_bars_move_smoothly() {
        let (font, row) = (40.0, 52.0);
        let mut last = gap_bars(font, row, 0.0, 0.0, 0.0, 1.0, 1.0, false);
        let mut fractional = 0;
        for k in 1..=600 {
            let secs = k as f32 / 200.0;
            let bars = gap_bars(font, row, secs, 0.5, 0.0, 1.0, 1.0, false);
            for (now, before) in bars.iter().zip(&last) {
                assert!((now.height - before.height).abs() < 0.5, "frame {k}: {} -> {}", before.height, now.height);
                assert!((now.top + now.height / 2.0 - row / 2.0).abs() < 1e-4);
                if now.height.fract().abs() > 1e-3 {
                    fractional += 1;
                }
            }
            last = bars;
        }
        assert!(fractional > 2000);
    }

    #[test]
    fn gap_bars_grow_from_the_left_and_hold_still_reduced() {
        let rest = gap_bars(40.0, 52.0, 3.0, 1.0, 0.0, 1.0, 1.0, true);
        let swollen = gap_bars(40.0, 52.0, 9.0, 1.0, 0.0, 1.3, 1.0, true);
        assert_eq!(rest[0].left, 0.0);
        assert_eq!(swollen[0].left, 0.0);
        for (a, b) in rest.iter().zip(&swollen) {
            assert!((b.width - a.width * 1.3).abs() < 1e-4 && (b.left - a.left * 1.3).abs() < 1e-3 && (b.height - a.height * 1.3).abs() < 1e-3);
        }
        assert!(rest.iter().all(|b| (b.height - rest[0].height).abs() < 1e-6));
        let lit = gap_bars(40.0, 52.0, 3.0, 0.5, 0.0, 1.0, 1.0, false).map(|b| b.lit);
        assert!(lit.windows(2).all(|w| w[0] >= w[1]) && lit[0] == 1.0 && lit[3] == 0.0);
    }

    #[test]
    fn drawn_lyrics_are_not_asked_for_again() {
        assert!(!laid_out().needs_request(&key()));
    }

    /// Minimizing and restoring must not leave the lyrics panel empty. When the sprites go, the request record must go
    /// too, or the panel never asks again.
    #[test]
    fn released_lyrics_are_asked_for_again() {
        let mut state = laid_out();
        let generation = state.generation;
        let images = state.release();
        assert_eq!(images.len(), 3, "both levels and the glow of the one drawn line");
        assert!(state.set.is_none() && state.garbage.is_empty());
        assert!(state.needs_request(&key()));
        assert_ne!(state.generation, generation, "a debounced request from before is dropped");
    }

    /// a fade's stretches side by side cover each device pixel once, at any scale and wherever the line sits.
    #[test]
    fn stretches_meet_on_device_pixels() {
        for scale in [1.0_f32, 1.25, 1.5, 1.75, 2.0, 2.25] {
            for origin_device in [0.0_f32, 151.0, 151.5, 377.333, 1023.75] {
                let device = |v: f32| (v * scale).round() / scale;
                let origin = gpui::point(px(device(origin_device / scale)), px(device(40.0 / scale)));
                // stretches 1 to 3 device pixels wide, as the fade's strips snap
                let mut edges = vec![0.0_f32];
                for k in 0..120 {
                    edges.push(edges[k] + (1 + k % 3) as f32);
                }
                let covered: Vec<(f32, f32)> = edges
                    .windows(2)
                    .map(|e| {
                        let clip = Bounds::from_corners(gpui::point(px(e[0] / scale), px(0.0)), gpui::point(px(e[1] / scale), px(30.0 / scale)));
                        let mask = stretch_mask(origin, clip, scale);
                        ((f32::from(mask.left()) * scale).floor(), (f32::from(mask.right()) * scale).ceil())
                    })
                    .collect();
                for (k, pair) in covered.windows(2).enumerate() {
                    assert_eq!(pair[0].1, pair[1].0, "scale {scale} origin {origin_device}: stretch {k} overlaps or leaves a gap");
                }
                for (k, &(l, r)) in covered.iter().enumerate() {
                    assert_eq!(r - l, edges[k + 1] - edges[k], "scale {scale} origin {origin_device}: stretch {k} width");
                }
            }
        }
    }
}
