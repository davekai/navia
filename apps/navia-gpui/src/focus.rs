//! Keyboard focus for every control. Tab and Shift+Tab walk visible controls in paint order,
//! the ring shows only after keyboard navigation, and GPUI turns Enter and Space on a focused
//! stop into a click. Elements call `.stop(&t)`.
//!
//! Handles are keyed by element id and occurrence and live in a thread-local pool, so the
//! builders in `ui.rs` and `ui_controls.rs` need no context. `begin_frame` refills the pool to
//! twice what the last frame used. A frame that runs out builds the extra controls without a
//! stop and requests another frame, so it never panics.

use std::cell::RefCell;
use std::collections::HashMap;

use gpui::prelude::*;
use gpui::{AbsoluteLength, App, Bounds, BorderStyle, Corners, Div, ElementId, FocusHandle, Hsla, Pixels, Stateful, Window, canvas, px, quad, transparent_black};

use crate::theme::Theme;
use crate::ui_controls::TextTarget;

/// What Enter and Space mean on a stop.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StopKind {
    /// Enter and Space press it, like a button reached by keyboard.
    Button,
    /// A song row. Enter plays it, Space still toggles playback.
    Row,
    /// A text field that typing goes to.
    Field,
}

struct Entry {
    handle: FocusHandle,
    kind: StopKind,
    field: Option<TextTarget>,
    last_frame: u64,
}

#[derive(Default)]
struct Stops {
    map: HashMap<(Scope, ElementId, u32), Entry>,
    counts: HashMap<(Scope, ElementId), u32>,
    /// Which cached part of the window is being built. Keys are per scope, so a cached part keeps
    /// its handles however the others count.
    scope: Scope,
    pool: Vec<FocusHandle>,
    /// New handles requested this frame, whether the pool had them or not.
    wanted: usize,
    /// Controls built without a stop this frame, because the pool ran out.
    missed: usize,
    frame: u64,
    /// A dialog is open, so only its own controls take the keyboard.
    modal: bool,
    /// The last focus move came from the keyboard, so the ring shows and Space may press.
    keyboard: bool,
    /// Tab order section: sidebar, top bar, page, dock. `gpui::list` builds rows lazily, so this
    /// is left at the page's value after rendering.
    section: isize,
    /// Asks the focused stop to report where it landed, once, after a Tab.
    want_report: bool,
    report: Option<(Bounds<Pixels>, Bounds<Pixels>)>,
}

thread_local! {
    static STOPS: RefCell<Stops> = RefCell::default();
}

/// The parts of the window that are built separately. See `slots.rs`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Scope {
    #[default]
    Shell,
    Sidebar,
    Main,
    Player,
    Expanded,
}

/// Builds `build` as `scope` in tab section `section`, keeping both set for anything built later
/// during layout, like list rows. Both are restored afterward.
pub fn scoped(scope: Scope, section: isize, build: impl FnOnce() -> gpui::AnyElement) -> gpui::AnyElement {
    let before = with(|s| (std::mem::replace(&mut s.scope, scope), std::mem::replace(&mut s.section, section)));
    let child = build();
    with(|s| (s.scope, s.section) = before);
    Scoped { scope, section, child }.into_any_element()
}

struct Scoped {
    scope: Scope,
    section: isize,
    child: gpui::AnyElement,
}

impl Scoped {
    fn within<R>(&mut self, f: impl FnOnce(&mut gpui::AnyElement) -> R) -> R {
        let (scope, section) = (self.scope, self.section);
        let before = with(|s| (std::mem::replace(&mut s.scope, scope), std::mem::replace(&mut s.section, section)));
        let r = f(&mut self.child);
        with(|s| (s.scope, s.section) = before);
        r
    }
}

impl IntoElement for Scoped {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl gpui::Element for Scoped {
    type RequestLayoutState = ();
    type PrepaintState = ();
    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }
    fn request_layout(&mut self, _: Option<&gpui::GlobalElementId>, _: Option<&gpui::InspectorElementId>, window: &mut Window, cx: &mut App) -> (gpui::LayoutId, ()) {
        (self.within(|c| c.request_layout(window, cx)), ())
    }
    fn prepaint(&mut self, _: Option<&gpui::GlobalElementId>, _: Option<&gpui::InspectorElementId>, _: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) {
        self.within(|c| {
            c.prepaint(window, cx);
        });
    }
    fn paint(&mut self, _: Option<&gpui::GlobalElementId>, _: Option<&gpui::InspectorElementId>, _: Bounds<Pixels>, _: &mut (), _: &mut (), window: &mut Window, cx: &mut App) {
        self.within(|c| c.paint(window, cx));
    }
}

/// Smallest pool kept ready. Bigger than most frames need, so a miss is rare.
const POOL_MIN: usize = 256;

/// Starts a frame. Refills the handle pool and drops stops that have been gone a while.
pub fn begin_frame(frame: u64, modal: bool, cx: &mut App) {
    STOPS.with(|s| {
        let mut s = s.borrow_mut();
        s.frame = frame;
        s.counts.clear();
        s.scope = Scope::Shell;
        s.modal = modal;
        let target = POOL_MIN.max(s.wanted.saturating_mul(2));
        while s.pool.len() < target {
            s.pool.push(cx.focus_handle());
        }
        s.wanted = 0;
        s.missed = 0;
        s.map.retain(|_, entry| entry.last_frame + 300 > frame);
    });
}

fn with<R>(f: impl FnOnce(&mut Stops) -> R) -> R {
    STOPS.with(|s| f(&mut s.borrow_mut()))
}

pub const SECTION_SIDEBAR: isize = 0;
pub const SECTION_TOPBAR: isize = 1;
pub const SECTION_PAGE: isize = 2;
pub const SECTION_DOCK: isize = 3;

/// Stops built from here on come at this point in the tab order.
pub fn set_section(section: isize) {
    with(|s| s.section = section);
}

pub fn keyboard() -> bool {
    with(|s| s.keyboard)
}

pub fn set_keyboard(on: bool) {
    with(|s| s.keyboard = on);
}

/// Makes the focused stop report its position after the next paint, so it can be scrolled into view.
pub fn request_report() {
    with(|s| s.want_report = true);
}

pub fn take_report() -> Option<(Bounds<Pixels>, Bounds<Pixels>)> {
    with(|s| s.report.take())
}

/// Whether window focus is on a stop that wasn't rendered last frame.
pub fn focus_is_stale(window: &Window, cx: &App) -> bool {
    let Some(focused) = window.focused(cx) else { return true };
    with(|s| s.map.values().any(|e| e.handle == focused && e.last_frame + 1 < s.frame))
}

/// What the window's focus is on, if it's a stop.
pub fn focused_kind(window: &Window, cx: &App) -> Option<StopKind> {
    let focused = window.focused(cx)?;
    with(|s| s.map.values().find(|e| e.handle == focused).map(|e| e.kind))
}

/// The field the focus is on, if it's a field stop.
pub fn focused_field(window: &Window, cx: &App) -> Option<TextTarget> {
    let focused = window.focused(cx)?;
    with(|s| s.map.values().find(|e| e.handle == focused).and_then(|e| e.field.clone()))
}

pub fn handle_for_field(target: &TextTarget) -> Option<FocusHandle> {
    with(|s| s.map.values().find(|e| e.field.as_ref() == Some(target)).map(|e| e.handle.clone()))
}

/// Whether some controls this frame had no handle and need one more frame to get one.
pub fn missed_this_frame() -> bool {
    with(|s| s.missed > 0)
}

/// `None` only when the pool ran out; the control stays clickable and gets its stop next frame.
/// `live`: the stop takes focus this frame; one under a modal layer doesn't, so focus left on it counts as stale.
fn entry_for(id: ElementId, kind: StopKind, field: Option<TextTarget>, live: bool) -> Option<FocusHandle> {
    with(|s| {
        let scope = s.scope;
        let n = s.counts.entry((scope, id.clone())).or_insert(0);
        let key = (scope, id, *n);
        *n += 1;
        let mut frame = s.frame;
        let handle = match s.map.get(&key) {
            Some(e) => {
                if !live {
                    frame = e.last_frame;
                }
                e.handle.clone()
            }
            None => {
                s.wanted += 1;
                match s.pool.pop() {
                    Some(h) => h,
                    None => {
                        s.missed += 1;
                        return None;
                    }
                }
            }
        };
        s.map.insert(key, Entry { handle: handle.clone(), kind, field, last_frame: frame });
        Some(handle)
    })
}

/// Thickness and gap of the focus ring, 2px each, scaled like the UI.
fn ring_px(t: &Theme) -> Pixels {
    t.s(2.0)
}

fn resolve(length: Option<AbsoluteLength>, rem: Pixels) -> Pixels {
    length.map_or(px(0.0), |l| l.to_pixels(rem))
}

/// The ring, painted just outside the element. `inset` makes it follow a row's inset highlight pill.
fn ring(handle: FocusHandle, radius: Option<AbsoluteLength>, inset: Pixels, thickness: Pixels, color: Hsla) -> impl IntoElement {
    canvas(|_, _, _| {}, move |bounds: Bounds<Pixels>, _, window: &mut Window, _| {
        if !handle.is_focused(window) {
            return;
        }
        let report = with(|s| std::mem::take(&mut s.want_report));
        if report {
            let visible = window.content_mask().bounds;
            with(|s| s.report = Some((bounds, visible)));
        }
        if !keyboard() {
            return;
        }
        let pill = Bounds::new(gpui::point(bounds.origin.x, bounds.origin.y + inset), gpui::size(bounds.size.width, bounds.size.height - inset * 2.0));
        let outer = pill.dilate(thickness * 2.0);
        let r = resolve(radius, window.rem_size()).min(pill.size.height / 2.0).max(px(0.0)) + thickness * 2.0;
        let ring = quad(outer, Corners::all(r.min(outer.size.height / 2.0)), transparent_black(), thickness, color, BorderStyle::Solid);
        window.paint_quad(ring);
    })
    .absolute()
    .inset_0()
}

fn attach(mut el: Stateful<Div>, t: &Theme, kind: StopKind, field: Option<TextTarget>, top: bool, inset: Pixels) -> Stateful<Div> {
    let Some(id) = el.interactivity().element_id.clone() else { return el };
    let modal = with(|s| s.modal);
    let Some(handle) = entry_for(id, kind, field, !modal || top) else { return el };
    if modal && !top {
        return el;
    }
    let section = with(|s| s.section);
    let handle = handle.tab_index(section).tab_stop(true);
    if kind == StopKind::Field {
        // Fields have their own ring, shown while focus is inside them.
        return el.track_focus(&handle);
    }
    let mut radius = el.style().corner_radii.top_left;
    if kind == StopKind::Row && radius.is_none() {
        // The highlight pill's own corners.
        radius = Some(t.s(crate::theme::size::R_ROW).into());
    }
    el.track_focus(&handle).child(ring(handle, radius, inset, ring_px(t), t.accent_hsla()))
}

pub trait FocusStop {
    /// A button. Enter and Space press it after keyboard navigation.
    fn stop(self, t: &Theme) -> Self;
    /// A control inside a dialog, which keeps focus while everything else is beneath it.
    fn stop_top(self, t: &Theme) -> Self;
    /// A song row. Enter activates and Space toggles playback. `inset` fits the ring to the
    /// row's highlight pill.
    fn stop_row(self, t: &Theme, inset: Pixels) -> Self;
    fn stop_field(self, t: &Theme, target: TextTarget) -> Self;
}

impl FocusStop for Stateful<Div> {
    fn stop(self, t: &Theme) -> Self {
        attach(self, t, StopKind::Button, None, false, px(0.0))
    }

    fn stop_top(self, t: &Theme) -> Self {
        attach(self, t, StopKind::Button, None, true, px(0.0))
    }

    fn stop_row(self, t: &Theme, inset: Pixels) -> Self {
        attach(self, t, StopKind::Row, None, false, inset)
    }

    fn stop_field(self, t: &Theme, target: TextTarget) -> Self {
        // The dialog's field belongs to the dialog's layer.
        let top = matches!(target, TextTarget::NewPlaylistName | TextTarget::NewPlaylistDescription | TextTarget::PlaylistName | TextTarget::PlaylistDescription | TextTarget::ShareDescription | TextTarget::ShareDate);
        attach(self, t, StopKind::Field, Some(target), top, px(0.0))
    }
}
