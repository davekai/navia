//! The overlay scrollbar for every scrolling area, since GPUI draws none on Linux.
//! It takes no layout width. It fades in on scroll or hover and out about a second later.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{AnyElement, Bounds, Context, ListState, MouseButton, Pixels, ScrollHandle, UniformListScrollHandle, div, point, px};

use crate::shell::{Drag, Shell};
use crate::theme::motion;

const THUMB_W: f32 = 4.0;
const THUMB_HOVER_W: f32 = 6.0;
const STRIP_W: f32 = 10.0;
const THUMB_MIN: f32 = 28.0;
const VISIBLE_FOR: Duration = Duration::from_millis(1000);

#[derive(Clone)]
pub enum ScrollSource {
    Uniform(UniformListScrollHandle),
    List(ListState),
    Handle(ScrollHandle),
}

struct Metrics {
    offset: f32,
    max: f32,
    viewport: f32,
}

impl ScrollSource {
    fn metrics(&self) -> Option<Metrics> {
        let (offset, max, viewport) = match self {
            ScrollSource::Uniform(handle) => {
                let base = handle.0.borrow().base_handle.clone();
                (-f32::from(base.offset().y), f32::from(base.max_offset().y), f32::from(base.bounds().size.height))
            }
            ScrollSource::List(list) => (-f32::from(list.scroll_px_offset_for_scrollbar().y), f32::from(list.max_offset_for_scrollbar().y), f32::from(list.viewport_bounds().size.height)),
            ScrollSource::Handle(handle) => (-f32::from(handle.offset().y), f32::from(handle.max_offset().y), f32::from(handle.bounds().size.height)),
        };
        (max > 0.5 && viewport > 0.0).then_some(Metrics { offset: offset.max(0.0), max, viewport })
    }

    fn set_offset(&self, y: f32) {
        match self {
            ScrollSource::Uniform(handle) => {
                let base = handle.0.borrow().base_handle.clone();
                base.set_offset(point(px(0.0), px(-y)));
            }
            ScrollSource::List(list) => list.set_offset_from_scrollbar(point(px(0.0), px(-y))),
            ScrollSource::Handle(handle) => handle.set_offset(point(px(0.0), px(-y))),
        }
    }
}

pub struct BarState {
    source: Option<ScrollSource>,
    last_offset: f32,
    last_active: Instant,
    armed: bool,
    hovered: bool,
    track: Rc<RefCell<Bounds<Pixels>>>,
}

impl Default for BarState {
    fn default() -> Self {
        // Hidden until something scrolls or the pointer comes near.
        Self { source: None, last_offset: 0.0, last_active: Instant::now() - Duration::from_secs(3), armed: false, hovered: false, track: Rc::default() }
    }
}

pub type Scrollbars = HashMap<&'static str, BarState>;

impl Shell {
    /// add it on top of the content in the area's positioned parent; `top` and `bottom` clear fixed controls.
    pub fn render_scrollbar(&mut self, id: &'static str, source: ScrollSource, top: Pixels, bottom: Pixels, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let Some(m) = source.metrics() else { return div().into_any_element() };
        let dragging = matches!(self.dragging, Some(Drag::Scroll { id: d, .. }) if d == id);
        let now = Instant::now();
        let state = self.scrollbars.entry(id).or_default();
        state.source = Some(source);
        if (m.offset - state.last_offset).abs() > 0.5 {
            state.last_offset = m.offset;
            state.last_active = now;
        }
        if dragging || state.hovered {
            state.last_active = now;
        }
        let idle = now.duration_since(state.last_active);
        let alpha = if idle < VISIBLE_FOR { 1.0 } else { 1.0 - motion::progress((idle - VISIBLE_FOR).as_secs_f32(), motion::SCROLLBAR_FADE) };
        let track_bounds = state.track.clone();
        let hovered = state.hovered;
        if idle >= VISIBLE_FOR && alpha > 0.0 {
            self.scrollbar_animating = true;
        } else if idle < VISIBLE_FOR && !state.armed {
            // wakes when it's time to start fading.
            state.armed = true;
            let wait = VISIBLE_FOR - idle + Duration::from_millis(16);
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(wait).await;
                let _ = this.update(cx, |this, cx| {
                    if let Some(state) = this.scrollbars.get_mut(id) {
                        state.armed = false;
                    }
                    cx.notify();
                });
            })
            .detach();
        }

        let track_h = f32::from(track_bounds.borrow().size.height);
        let thumb_h = (track_h * m.viewport / (m.viewport + m.max)).max(f32::from(t.s(THUMB_MIN))).min(track_h);
        let thumb_y = (track_h - thumb_h) * (m.offset / m.max).clamp(0.0, 1.0);
        let wide = hovered || dragging;
        let thumb_w = t.s(if wide { THUMB_HOVER_W } else { THUMB_W });
        let color = gpui::white().opacity(if wide { crate::theme::SCROLL_THUMB_HOVER } else { crate::theme::SCROLL_THUMB } * alpha);
        let visible = alpha > 0.0;

        let strip = div()
            .id(id)
            .absolute()
            .top(top)
            .bottom(bottom)
            .right(t.s(2.0))
            .w(t.s(STRIP_W))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if let Some(state) = this.scrollbars.get_mut(id) {
                    state.hovered = *hovered;
                }
                cx.notify();
            }))
            .child(crate::view_topbar::track_bounds(track_bounds))
            .when(visible, |d| {
                d.on_mouse_down(MouseButton::Left, cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.scrollbar_press(id, event.position, cx);
                }))
                .child(div().absolute().right(t.s(1.0)).top(px(thumb_y)).w(thumb_w).h(px(thumb_h)).rounded_full().bg(color))
            });
        strip.into_any_element()
    }

    /// a press on the track jumps there and grabs the thumb centered.
    fn scrollbar_press(&mut self, id: &'static str, position: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        let Some((source, track)) = self.scrollbars.get(id).and_then(|s| Some((s.source.clone()?, *s.track.borrow()))) else { return };
        let Some(m) = source.metrics() else { return };
        let t = self.theme;
        let track_h = f32::from(track.size.height);
        let thumb_h = (track_h * m.viewport / (m.viewport + m.max)).max(f32::from(t.s(THUMB_MIN))).min(track_h);
        let thumb_y = (track_h - thumb_h) * (m.offset / m.max).clamp(0.0, 1.0);
        let local = f32::from(position.y - track.origin.y);
        let grab = if (thumb_y..thumb_y + thumb_h).contains(&local) {
            local - thumb_y
        } else {
            let wanted = (local - thumb_h / 2.0).clamp(0.0, (track_h - thumb_h).max(0.0));
            source.set_offset(wanted / (track_h - thumb_h).max(1.0) * m.max);
            thumb_h / 2.0
        };
        self.dragging = Some(Drag::Scroll { id, grab: px(grab) });
        cx.notify();
    }

    pub fn scrollbar_drag(&mut self, id: &'static str, grab: Pixels, position: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        let Some((source, track)) = self.scrollbars.get(id).and_then(|s| Some((s.source.clone()?, *s.track.borrow()))) else { return };
        let Some(m) = source.metrics() else { return };
        let track_h = f32::from(track.size.height);
        let thumb_h = (track_h * m.viewport / (m.viewport + m.max)).max(f32::from(self.theme.s(THUMB_MIN))).min(track_h);
        let room = (track_h - thumb_h).max(1.0);
        let y = (f32::from(position.y - track.origin.y) - f32::from(grab)).clamp(0.0, room);
        source.set_offset(y / room * m.max);
        cx.notify();
    }
}
