//! the player's title and artists: text that doesn't fit scrolls across (`--marquee-pause`, `--marquee-fade`), all lines on one clock so they never drift apart.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    AnyElement, AnyView, App, Bounds, ContentMask, Context, CursorStyle, Element, ElementId, Entity, EntityId, Font, FontWeight, GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId, LayoutId, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, ShapedLine, SharedString, Style, StyleRefinement, TextRun, UnderlineStyle, WeakEntity, Window, div, point, px, size,
};

use crate::shell::Shell;
use crate::slots::Part;
use crate::song_links::LinkTarget;
use crate::theme::motion;
use crate::theme::size::{MARQUEE_FADE, MARQUEE_GAP};

/// where a line is drawn: after the dock or bar's slot, after the expanded player's, or in the mini player's window.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Host {
    Player,
    Expanded,
    Mini,
}

impl Host {
    pub fn of(part: Part) -> Self {
        if part == Part::Expanded { Self::Expanded } else { Self::Player }
    }
}

/// what a line shows: pieces of text, each a link or not, in one style.
#[derive(Clone, PartialEq)]
pub struct Spec {
    pub pieces: Vec<(SharedString, Option<LinkTarget>)>,
    pub color: Hsla,
    pub size: Pixels,
    pub weight: FontWeight,
    pub line_height: Pixels,
}

impl Spec {
    fn text(&self) -> SharedString {
        self.pieces.iter().map(|(t, _)| t.as_ref()).collect::<String>().into()
    }
    fn font(&self) -> Font {
        Font { weight: self.weight, ..gpui::font(crate::assets::FONT_FAMILY) }
    }
    /// the link at byte `index`, with its byte range.
    fn link_at(&self, index: usize) -> Option<(std::ops::Range<usize>, LinkTarget)> {
        let mut at = 0;
        for (text, link) in &self.pieces {
            let range = at..at + text.len();
            at = range.end;
            if range.contains(&index) {
                return link.clone().map(|l| (range, l));
            }
        }
        None
    }
}

#[derive(Default)]
pub struct Marquees {
    lines: HashMap<&'static str, Line>,
    /// the part being drawn now.
    pub current: Option<Host>,
    /// the expanded player's slide offset when its lines were last told to repaint.
    expanded_offset: gpui::Point<Pixels>,
}

struct Line {
    view: Entity<MarqueeView>,
    anchor: Rc<Cell<Option<Bounds<Pixels>>>>,
    host: Host,
    seen: bool,
}

pub struct MarqueeView {
    shell: WeakEntity<Shell>,
    spec: Spec,
    shaped: Option<ShapedLine>,
    /// where the placeholder sits in the window, set while its part is drawn; `None` when it isn't shown.
    anchor: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// the wake-up already asked for, and the offset last painted.
    wake: Option<Instant>,
    offset: Pixels,
    hovered: Option<std::ops::Range<usize>>,
    pressed: bool,
    /// how visible the line is: the bar fades it while its times show.
    opacity: f32,
}

impl Render for MarqueeView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        MarqueeElement { view: cx.entity() }
    }
}

impl Shell {
    /// the placeholder to put where the line goes (as wide as the text, shrinking with the room); the line is its own view over it.
    pub fn marquee(&mut self, key: &'static str, host: Host, spec: Spec, opacity: f32, cx: &mut Context<Self>) -> AnyElement {
        use crate::ui::FullTextTip;
        let (full, size, weight) = (spec.text(), spec.size, spec.weight);
        // whole pixels up, so layout rounding never leaves the placeholder a hair short of the shaped line
        let width = crate::ui::text_width_app(cx, &spec.text(), spec.size, spec.weight).ceil();
        let line_height = spec.line_height;
        let shell = cx.entity().downgrade();
        let line = self.marquees.lines.entry(key).or_insert_with(|| {
            let anchor: Rc<Cell<Option<Bounds<Pixels>>>> = Default::default();
            let view = cx.new(|_| MarqueeView { shell, spec: spec.clone(), shaped: None, anchor: anchor.clone(), wake: None, offset: px(0.0), hovered: None, pressed: false, opacity });
            Line { view, anchor, host, seen: false }
        });
        line.seen = true;
        line.host = host;
        line.view.update(cx, |v, cx| {
            if v.spec != spec {
                v.spec = spec;
                v.shaped = None;
                v.hovered = None;
                cx.notify();
            }
            if v.opacity != opacity {
                v.opacity = opacity;
                cx.notify();
            }
        });
        let anchor = line.anchor.clone();
        let entity = line.view.entity_id();
        // in the line's own style, so its tip measures what the line draws.
        div()
            .id(key)
            .text_size(size)
            .font_weight(weight)
            .full_text_tip(&self.theme, full)
            .flex_shrink(1.0)
            .min_w(px(0.0))
            .w(width)
            .h(line_height)
            .child(
                gpui::canvas(
                    move |bounds, window, cx| {
                        // only what shows: the placeholder can reach past a parent that clips it.
                        let bounds = bounds.intersect(&window.content_mask().bounds);
                        if anchor.get() != Some(bounds) {
                            anchor.set(Some(bounds));
                            cx.notify(entity);
                        }
                    },
                    |_, _, _, _| {},
                )
                .size_full(),
            )
            .into_any_element()
    }

    /// a part is about to be drawn: its lines count as gone unless it draws them again.
    pub fn marquees_begin(&mut self, host: Host) {
        self.marquees.current = Some(host);
        for line in self.marquees.lines.values_mut().filter(|l| l.host == host) {
            line.seen = false;
        }
    }

    /// a part was drawn: lines it didn't draw this time hide.
    pub fn marquees_end(&mut self, host: Host, cx: &mut App) {
        for line in self.marquees.lines.values().filter(|l| l.host == host && !l.seen) {
            if line.anchor.take().is_some() {
                cx.notify(line.view.entity_id());
            }
        }
    }

    /// the expanded player slides under a paint offset, and a replayed view keeps the offset it was painted with, so its lines repaint when it moves.
    pub fn marquees_follow_offset(&mut self, offset: gpui::Point<Pixels>, cx: &mut App) {
        if self.marquees.expanded_offset == offset {
            return;
        }
        self.marquees.expanded_offset = offset;
        for line in self.marquees.lines.values().filter(|l| l.host == Host::Expanded) {
            cx.notify(line.view.entity_id());
        }
    }

    /// redraws every line, when a window's focus changes (they rest while it's unfocused).
    pub fn marquees_refresh(&self, cx: &mut App) {
        for line in self.marquees.lines.values() {
            cx.notify(line.view.entity_id());
        }
    }

    /// the lines drawn in `host`, each its own cached view, to put right after the part that holds them.
    pub fn marquee_overlays(&self, host: Host) -> Vec<AnyElement> {
        self.marquees
            .lines
            .values()
            .filter(|l| l.host == host)
            .map(|l| AnyView::from(l.view.clone()).cached(StyleRefinement::default().absolute().top_0().left_0().size_0()).into_any_element())
            .collect()
    }
}

struct MarqueeElement {
    view: Entity<MarqueeView>,
}

impl IntoElement for MarqueeElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

/// strips each faded edge is painted in, over the few glyphs that cross it.
const FADE_STEPS: usize = 8;

impl Element for MarqueeElement {
    type RequestLayoutState = ();
    type PrepaintState = Option<Hitbox>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.position = gpui::Position::Absolute;
        (window.request_layout(style, None, cx), ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) -> Option<Hitbox> {
        let view = self.view.read(cx);
        let anchor = view.anchor.get().filter(|_| view.opacity > 0.01)?;
        Some(window.insert_hitbox(anchor, HitboxBehavior::Normal))
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), hitbox: &mut Option<Hitbox>, window: &mut Window, cx: &mut App) {
        let Some(hitbox) = hitbox.clone() else {
            leave_clock(self.view.entity_id(), cx);
            return;
        };
        let b = hitbox.bounds;
        let entity = self.view.clone();
        let (gap, fade_w, speed) = {
            let shell = self.view.read(cx).shell.upgrade();
            let t = shell.map(|s| s.read(cx).theme);
            match t {
                Some(t) => (t.s(MARQUEE_GAP), t.s(MARQUEE_FADE), f32::from(t.s(motion::MARQUEE_SPEED))),
                None => return,
            }
        };
        let now = Instant::now();
        let reduced = motion::reduced();
        let id = self.view.entity_id();
        let (shaped, spec, offset, rest_until, running, others) = self.view.update(cx, |v, _| {
            if v.shaped.is_none() {
                let runs = vec![TextRun { len: v.spec.text().len(), font: v.spec.font(), color: v.spec.color, background_color: None, underline: None, strikethrough: None }];
                v.shaped = Some(window.text_system().shape_line(v.spec.text(), v.spec.size, &runs, None));
            }
            let line = v.shaped.clone().unwrap();
            let overflows = line.width > b.size.width + px(0.5);
            // not while the window isn't focused, or while the bar has it faded out: it rests at its start.
            let running = overflows && !reduced && window.is_window_active() && v.opacity > 0.01;
            let distance = f32::from(line.width + gap);
            let (others, epoch, cycle) = CLOCK.with(|c| {
                let mut c = c.borrow_mut();
                let others = c.note(id, running.then_some(distance / speed), now);
                (others, c.epoch, c.cycle())
            });
            let mut offset = px(0.0);
            let mut rest_until = None;
            if running {
                let elapsed = now.duration_since(epoch).as_secs_f32();
                let start = epoch + Duration::from_secs_f32((elapsed / cycle).floor() * cycle);
                let phase = elapsed % cycle;
                let travelled = (phase - motion::MARQUEE_PAUSE) * speed;
                if travelled <= 0.0 {
                    rest_until = Some(start + Duration::from_secs_f32(motion::MARQUEE_PAUSE));
                } else if travelled >= distance {
                    // done before the longest line: its second copy holds the first one's place until the pass ends.
                    rest_until = Some(start + Duration::from_secs_f32(cycle));
                } else {
                    // on whole device pixels, and the next frame only when the text has moved one more.
                    let scale = window.scale_factor();
                    let pixels = (travelled * scale).floor();
                    offset = -px(pixels / scale);
                    let next = (pixels + 1.0) / scale - travelled;
                    rest_until = Some(now + Duration::from_secs_f32(next / speed));
                }
            }
            v.offset = offset;
            let mut spec = v.spec.clone();
            spec.color.a *= v.opacity;
            (line, spec, offset, rest_until, running, others)
        });
        for other in others {
            cx.notify(other);
        }

        let baseline = (spec.line_height - shaped.ascent - shaped.descent) / 2.0 + shaped.ascent;
        let overflows = shaped.width > b.size.width + px(0.5);
        let hovered = self.view.read(cx).hovered.clone();
        window.with_content_mask(Some(ContentMask { bounds: b }), |window| {
            let fade_start = offset < px(0.0);
            let fade_end = overflows;
            let copies: &[Pixels] = if running { &[offset, offset + shaped.width + gap] } else { &[px(0.0)] };
            for &x0 in copies {
                for run in shaped.runs.iter() {
                    for glyph in &run.glyphs {
                        let gx = b.left() + x0 + glyph.position.x;
                        let gw = spec.size;
                        if gx > b.right() || gx + gw < b.left() {
                            continue;
                        }
                        let origin = point(gx, b.top() + baseline);
                        let paint = |window: &mut Window, alpha: f32| {
                            let color = Hsla { a: spec.color.a * alpha, ..spec.color };
                            let _ = if glyph.is_emoji { window.paint_emoji(origin, run.font_id, glyph.id, spec.size) } else { window.paint_glyph(origin, run.font_id, glyph.id, spec.size, color) };
                        };
                        let in_start = fade_start && gx < b.left() + fade_w;
                        let in_end = fade_end && gx + gw > b.right() - fade_w;
                        if !in_start && !in_end {
                            paint(window, 1.0);
                            continue;
                        }
                        // a glyph crossing a faded edge is painted strip by strip, each at its strength.
                        let step = fade_w / FADE_STEPS as f32;
                        let mut middle_from = b.left();
                        let mut middle_to = b.right();
                        if in_start {
                            middle_from = b.left() + fade_w;
                            for k in 0..FADE_STEPS {
                                let x = b.left() + step * k as f32;
                                strip(window, b, x, step, |w| paint(w, (k as f32 + 0.5) / FADE_STEPS as f32));
                            }
                        }
                        if in_end {
                            middle_to = b.right() - fade_w;
                            for k in 0..FADE_STEPS {
                                let x = b.right() - step * (k + 1) as f32;
                                strip(window, b, x, step, |w| paint(w, (k as f32 + 0.5) / FADE_STEPS as f32));
                            }
                        }
                        if middle_to > middle_from {
                            strip(window, b, middle_from, middle_to - middle_from, |w| paint(w, 1.0));
                        }
                    }
                }
                // the hovered link is underlined, like links elsewhere.
                if let Some(range) = hovered.clone() {
                    let x_from = b.left() + x0 + shaped.x_for_index(range.start);
                    let x_to = b.left() + x0 + shaped.x_for_index(range.end);
                    let y = b.top() + baseline + shaped.descent / 2.0;
                    window.paint_underline(point(x_from, y), x_to - x_from, &UnderlineStyle { thickness: px(crate::theme::size::LINK_UNDERLINE), color: Some(spec.color), wavy: false });
                }
            }
        });

        // hover, the pointing hand over a link, and clicks that follow it.
        if hovered.is_some() {
            window.set_cursor_style(CursorStyle::PointingHand, &hitbox);
        }
        let link_under = {
            let shaped = shaped.clone();
            let spec = spec.clone();
            move |x: Pixels| -> Option<(std::ops::Range<usize>, LinkTarget)> {
                let width = shaped.width + gap;
                let mut local = x - b.left() - offset;
                if local >= width {
                    local -= width;
                }
                spec.link_at(shaped.index_for_x(local)?)
            }
        };
        {
            let (entity, hitbox, link_under) = (entity.clone(), hitbox.clone(), link_under.clone());
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase != gpui::DispatchPhase::Bubble {
                    return;
                }
                let over = hitbox.is_hovered(window).then(|| link_under(event.position.x)).flatten().map(|(r, _)| r);
                entity.update(cx, |v, cx| {
                    if v.hovered != over {
                        v.hovered = over;
                        cx.notify();
                    }
                });
            });
        }
        {
            let (entity, hitbox, link_under) = (entity.clone(), hitbox.clone(), link_under.clone());
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if phase != gpui::DispatchPhase::Bubble || event.button != MouseButton::Left || !hitbox.is_hovered(window) {
                    return;
                }
                let m = event.modifiers;
                if link_under(event.position.x).is_some() && !(m.shift || m.control || m.platform) {
                    entity.update(cx, |v, cx| {
                        v.pressed = true;
                        if let Some(shell) = v.shell.upgrade() {
                            shell.update(cx, |s, _| s.link_pressed = Some(Instant::now()));
                        }
                    });
                }
            });
        }
        {
            let (entity, hitbox) = (entity.clone(), hitbox.clone());
            window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                if phase != gpui::DispatchPhase::Bubble || event.button != MouseButton::Left {
                    return;
                }
                let pressed = entity.update(cx, |v, _| std::mem::take(&mut v.pressed));
                if !pressed || !hitbox.is_hovered(window) {
                    return;
                }
                if let Some((_, target)) = link_under(event.position.x) {
                    let shell = entity.read(cx).shell.upgrade();
                    if let Some(shell) = shell {
                        cx.stop_propagation();
                        shell.update(cx, |s, cx| {
                            s.link_pressed = None;
                            s.follow_link(target, cx);
                        });
                    }
                }
            });
        }

        // one wake-up when the text has moved another device pixel, or when a pause ends; nothing otherwise.
        if let Some(resume) = rest_until {
            let pending = self.view.update(cx, |v, _| std::mem::replace(&mut v.wake, Some(resume)) == Some(resume));
            if !pending {
                let id = self.view.entity_id();
                cx.spawn(async move |cx| {
                    cx.background_executor().timer(resume.saturating_duration_since(Instant::now())).await;
                    let _ = cx.update(|cx| cx.notify(id));
                })
                .detach();
            }
        }
    }
}

thread_local! {
    static CLOCK: RefCell<Clock> = RefCell::new(Clock { epoch: Instant::now(), rolls: HashMap::new() });
}

/// the moving lines' shared pass: each one's roll (seconds to travel its width and gap) and when they started together.
struct Clock {
    epoch: Instant,
    rolls: HashMap<EntityId, f32>,
}

impl Clock {
    /// any change starts every line over from the rest; returns the other lines so they can be redrawn.
    fn note(&mut self, id: EntityId, roll: Option<f32>, now: Instant) -> Vec<EntityId> {
        let changed = match roll {
            Some(r) => self.rolls.insert(id, r).is_none_or(|old| (old - r).abs() > 0.001),
            None => self.rolls.remove(&id).is_some(),
        };
        if !changed {
            return Vec::new();
        }
        self.epoch = now;
        self.rolls.keys().copied().filter(|&k| k != id).collect()
    }
    /// one pass: the rest, then the longest roll.
    fn cycle(&self) -> f32 {
        motion::MARQUEE_PAUSE + self.rolls.values().copied().fold(0.0, f32::max)
    }
}

/// forgets line `id` once it's no longer drawn, and redraws the others on a new pass.
fn leave_clock(id: EntityId, cx: &mut App) {
    let others = CLOCK.with(|c| c.borrow_mut().note(id, None, Instant::now()));
    for other in others {
        cx.notify(other);
    }
}

/// paints `f` clipped to a vertical strip `w` wide at `x` inside `b`.
fn strip(window: &mut Window, b: Bounds<Pixels>, x: Pixels, w: Pixels, f: impl FnOnce(&mut Window)) {
    let strip = Bounds { origin: point(x, b.top()), size: size(w, b.size.height) };
    window.with_content_mask(Some(ContentMask { bounds: strip }), f);
}

