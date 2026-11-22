//! Settings-style controls: switches, sliders, segmented pills, the glass select with its
//! filter, and text fields. Shared by Settings, the Equalizer and the smart playlist editor.

use crate::i18n::tr;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui::prelude::*;
use crate::focus::FocusStop;
use gpui::{AnyElement, BoxShadow, Bounds, Context, FontWeight, Hsla, MouseButton, Pixels, Point, ScrollStrategy, SharedString, UniformListScrollHandle, Window, anchored, deferred, div, point, px, relative, uniform_list};

use crate::shell::{Drag, Route, Shell};
use crate::color::Srgb;
use crate::theme::Theme;
use crate::theme::size::*;
use crate::ui::{glass_edge, icon};

pub const FILTER_FROM: usize = 9;

#[derive(Clone, PartialEq, Debug)]
pub enum TextTarget {
    AccentHex,
    /// The Discord public server address in Settings.
    DiscordAddress,
    /// A new friend's Discord id and name, and the Lanyard address, in Settings.
    FriendId,
    FriendName,
    LanyardAddress,
    EqPresetName,
    SmartName,
    NewPlaylistName,
    NewPlaylistDescription,
    /// The Edit Details dialog's name and description.
    PlaylistName,
    PlaylistDescription,
    /// The share dialog's description and custom date.
    ShareDescription,
    ShareDate,
    SmartLimit,
    SmartValue { row: u64, side: u8 },
    SelectFilter,
    ConnectAddress,
    ConnectUser,
    ConnectPassword,
    Page,
    /// The filter of an open submenu, such as Add to Playlist.
    MenuFind,
    /// The AirPlay PIN dialog's digits.
    AirPlayPin,
    /// The open Songs filter's find field, and a range filter's From and To.
    FilterFind,
    FilterFrom,
    FilterTo,
}

pub struct PointerDrag {
    pub on_move: Rc<dyn Fn(&mut Shell, Point<Pixels>, &mut Context<Shell>)>,
    pub on_end: Option<Rc<dyn Fn(&mut Shell, &mut Context<Shell>)>>,
}

#[derive(Clone)]
pub struct SelectOption {
    pub value: SharedString,
    pub label: SharedString,
    pub group: Option<SharedString>,
    /// Not selectable. Says why when picked.
    pub unavailable: Option<&'static str>,
}

impl SelectOption {
    pub fn new(value: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        Self { value: value.into(), label: label.into(), group: None, unavailable: None }
    }

    pub fn unavailable(value: impl Into<SharedString>, label: impl Into<SharedString>, reason: &'static str) -> Self {
        Self { value: value.into(), label: label.into(), group: None, unavailable: Some(reason) }
    }

    pub fn grouped(value: impl Into<SharedString>, label: impl Into<SharedString>, group: impl Into<SharedString>) -> Self {
        Self { value: value.into(), label: label.into(), group: Some(group.into()), unavailable: None }
    }
}

pub type SelectChange = Rc<dyn Fn(&mut Shell, SharedString, &mut Context<Shell>)>;
pub type SliderChange = Rc<dyn Fn(&mut Shell, f32, &mut Context<Shell>)>;

pub struct SelectState {
    key: String,
    value: SharedString,
    options: Rc<Vec<SelectOption>>,
    on_change: SelectChange,
    anchor: Bounds<Pixels>,
    query: String,
    active: usize,
    scroll: UniformListScrollHandle,
}

#[derive(Clone)]
enum SelectRow {
    Heading(SharedString),
    Option { index: usize, position: usize },
}

#[derive(Default)]
pub struct ControlState {
    pub bounds: HashMap<String, Rc<RefCell<Bounds<Pixels>>>>,
    pub text: Option<TextTarget>,
    pub select: Option<SelectState>,
    pub pointer: Option<PointerDrag>,
    pub hex_text: String,
    pub edit: Rc<RefCell<crate::text_field::FieldEdit>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SliderLook {
    /// A fill grows from the origin in the accent color.
    Accent,
    Plain,
    /// The accent hue, on a fixed rainbow track.
    Hue,
}

pub struct SliderSpec {
    pub key: String,
    pub value: f32,
    pub min: f32,
    pub max: f32,
    pub step: f32,
    pub origin: f32,
    pub look: SliderLook,
    /// Width in tokens, or `None` to fill the available width.
    pub width: Option<f32>,
    pub enabled: bool,
    pub on_change: SliderChange,
    /// Runs when the drag ends, to save the value.
    pub on_end: Option<Rc<dyn Fn(&mut Shell, &mut Context<Shell>)>>,
}

impl SliderSpec {
    pub fn new(key: impl Into<String>, value: f32, min: f32, max: f32, on_change: impl Fn(&mut Shell, f32, &mut Context<Shell>) + 'static) -> Self {
        Self { key: key.into(), value, min, max, step: 1.0, origin: min, look: SliderLook::Accent, width: Some(200.0), enabled: true, on_change: Rc::new(on_change), on_end: None }
    }

    pub fn step(mut self, step: f32) -> Self {
        self.step = step;
        self
    }

    pub fn origin(mut self, origin: f32) -> Self {
        self.origin = origin;
        self
    }

    pub fn look(mut self, look: SliderLook) -> Self {
        self.look = look;
        self
    }

    pub fn width(mut self, width: Option<f32>) -> Self {
        self.width = width;
        self
    }

    pub fn enabled(mut self, on: bool) -> Self {
        self.enabled = on;
        self
    }

    pub fn on_end(mut self, f: impl Fn(&mut Shell, &mut Context<Shell>) + 'static) -> Self {
        self.on_end = Some(Rc::new(f));
        self
    }
}

pub fn switch(t: &Theme, id: impl Into<SharedString>, on: bool, enabled: bool, cx: &mut Context<Shell>, handler: impl Fn(&mut Shell, &mut Context<Shell>) + 'static) -> gpui::Stateful<gpui::Div> {
    switch_look(t, on, 1.0)
        .id(gpui::ElementId::Name(id.into()))
        .when(!enabled, |d| d.opacity(DISABLED_OPACITY))
        .when(enabled, |d| d.stop(&t).on_click(cx.listener(move |this, _, _, cx| handler(this, cx))))
}

/// The switch itself, at `scale`. 1, or `SWITCH_COMPACT` inside a menu row.
pub fn switch_look(t: &Theme, on: bool, scale: f32) -> gpui::Div {
    let s = |v: f32| t.s(v * scale);
    div()
        .relative()
        .flex_none()
        .w(s(44.0))
        .h(s(26.0))
        .rounded_full()
        .bg(if on { t.accent_hsla() } else { t.panel_2().into() })
        .when(!on, |d| d.shadow(vec![line_edge(t)]))
        .child(div().absolute().top(s(3.0)).left(s(if on { 21.0 } else { 3.0 })).size(s(20.0)).rounded_full().bg(t.knob()))
}

pub fn line_edge(t: &Theme) -> BoxShadow {
    BoxShadow { color: t.line().into(), offset: point(px(0.0), px(0.0)), blur_radius: px(0.0), spread_radius: px(1.0), inset: true }
}

/// The accent ring on a focused field, or the warning ring on an invalid one.
pub fn ring(color: impl Into<Hsla>) -> BoxShadow {
    BoxShadow { color: color.into(), offset: point(px(0.0), px(0.0)), blur_radius: px(0.0), spread_radius: px(2.0), inset: false }
}

pub fn with_ring(mut base: Vec<BoxShadow>, color: impl Into<Hsla>) -> Vec<BoxShadow> {
    base.insert(0, ring(color));
    base
}

/// How wide a select pill is. Its container's width, a token width, or just its text.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PillWidth {
    Full,
    Fixed(f32),
    Fit,
}

/// Adds the thin invalid ring around a field or value with a problem.
pub fn with_invalid_ring(mut base: Vec<BoxShadow>) -> Vec<BoxShadow> {
    base.insert(0, BoxShadow { color: gpui::rgba(crate::theme::INVALID_RING).into(), offset: point(px(0.0), px(0.0)), blur_radius: px(0.0), spread_radius: px(crate::theme::INVALID_RING_W), inset: false });
    base
}

/// A glass pill of mutually exclusive choices.
pub fn segmented_glass<T: Copy + PartialEq + 'static>(t: &Theme, id: &'static str, value: T, options: &[(T, &'static str)], cx: &mut Context<Shell>, handler: impl Fn(&mut Shell, T, &mut Context<Shell>) + Clone + 'static) -> gpui::Div {
    let options: Vec<_> = options.iter().map(|&(option, label)| (option, label, None)).collect();
    segmented(t, SegmentedLook::Glass, id, value, &options, cx, handler)
}

/// an option with a reason is disabled and says why.
pub fn segmented_settings<T: Copy + PartialEq + 'static>(t: &Theme, id: &'static str, value: T, options: &[(T, &'static str, Option<&'static str>)], cx: &mut Context<Shell>, handler: impl Fn(&mut Shell, T, &mut Context<Shell>) + Clone + 'static) -> gpui::Div {
    segmented(t, SegmentedLook::Settings, id, value, options, cx, handler)
}

/// `Glass`: page toolbars, the larger size.
#[derive(Clone, Copy, PartialEq)]
enum SegmentedLook {
    Settings,
    Glass,
}

/// equal segments, with a divider between two that aren't selected.
fn segmented<T: Copy + PartialEq + 'static>(t: &Theme, look: SegmentedLook, id: &'static str, value: T, options: &[(T, &'static str, Option<&'static str>)], cx: &mut Context<Shell>, handler: impl Fn(&mut Shell, T, &mut Context<Shell>) + Clone + 'static) -> gpui::Div {
    let (h, radius) = match look {
        SegmentedLook::Settings => (t.s(SEGMENTED_H), t.s(SEGMENTED_R)),
        SegmentedLook::Glass => (t.s(SEGMENTED_GLASS_H), t.s(SEGMENTED_GLASS_H) / 2.0),
    };
    let eased: Vec<(f32, AnyElement)> = options.iter().enumerate().map(|(i, (option, ..))| crate::ui::toggle_ease((id, i), *option == value)).collect();
    let shown: Vec<f32> = eased.iter().map(|(shown, _)| *shown).collect();
    let mut group = div().grid().grid_cols_min_content(options.len() as u16).flex_none().h(h).rounded(radius).bg(t.segmented_track());
    for ((i, (option, label, reason)), (_, ticker)) in options.iter().enumerate().zip(eased) {
        let on = *option == value;
        let (option, handler, reason) = (*option, handler.clone(), *reason);
        let ink = crate::ui::ink((id, i));
        let rest: Hsla = if reason.is_some() { t.t3().into() } else { ink.color(t.t2(), t.hover_ink()) };
        let divider = (i > 0).then(|| {
            div()
                .absolute()
                .left(t.s(-SEGMENTED_DIVIDER_W / 2.0))
                .top(t.s(SEGMENTED_DIVIDER_INSET))
                .bottom(t.s(SEGMENTED_DIVIDER_INSET))
                .w(t.s(SEGMENTED_DIVIDER_W))
                .bg(t.segmented_divider())
                .opacity(1.0 - shown[i].max(shown[i - 1]))
        });
        let segment = div().id((id, i)).relative().child(ticker).children(divider);
        group = group.child(
            if reason.is_none() { crate::ui::pressable(segment, (id, i)) } else { segment }
                .flex()
                .items_center()
                .justify_center()
                .h_full()
                .px(t.s(SEGMENTED_PAD_X))
                .rounded(radius)
                .whitespace_nowrap()
                .text_size(t.text(TEXT_SM))
                .text_color(crate::ui::mix_hsla(rest, t.on_accent.hsla(1.0), shown[i]))
                .when(shown[i] > 0.0, |d| d.bg(t.accent_hsla().opacity(shown[i])))
                .when(reason.is_none() && !on, |d| d.opacity(ink.opacity(1.0)))
                .stop(&t).on_click(cx.listener(move |this, _, _, cx| match reason {
                    Some(r) => this.show_toast(r, cx),
                    None => handler(this, option, cx),
                }))
                .child(*label),
        );
    }
    group
}

pub fn round_button(t: &Theme, id: impl Into<gpui::ElementId>, label: impl Into<gpui::SharedString>, name: &'static str) -> gpui::Stateful<gpui::Div> {
    let id = id.into();
    crate::ui::pressable(div().id(id.clone()).tooltip(crate::ui::tip(t, label)), id).flex().items_center().justify_center().flex_none().size(t.s(CONTROL_H)).rounded_full().bg(t.glass_fill()).shadow(glass_edge()).child(crate::ui::hover_fill(crate::ui::HoverShape::Pill, t.glass_hover(), t.glass_press())).child(icon(t, name, 18.0, t.t1()))
}

/// A glass text button, such as a dialog's secondary button.
pub fn glass_action(t: &Theme, id: impl Into<gpui::ElementId>) -> gpui::Stateful<gpui::Div> {
    let id = id.into();
    crate::ui::pressable(div().id(id.clone()), id).flex().flex_row().items_center().justify_center().flex_none().gap(t.s(crate::theme::size::BUTTON_GAP)).h(t.s(CONTROL_H)).px(t.s(BUTTON_PAD)).rounded_full().bg(t.glass_fill()).shadow(glass_edge()).font_weight(FontWeight::MEDIUM).text_color(t.t1()).child(crate::ui::hover_fill(crate::ui::HoverShape::Pill, t.glass_hover(), t.glass_press()))
}

/// Any action button at the small size: `BUTTON_SMALL_H` tall, `BUTTON_SMALL_PAD` padding, `TEXT_XS` text.
pub fn small_action(t: &Theme, button: gpui::Stateful<gpui::Div>) -> gpui::Stateful<gpui::Div> {
    button.h(t.s(crate::theme::size::BUTTON_SMALL_H)).px(t.s(crate::theme::size::BUTTON_SMALL_PAD)).text_size(t.text(crate::theme::size::TEXT_XS))
}

/// How wide [`small_action`] draws a filled action with a semibold `label`, plus a leading
/// `CHIP_GLYPH` icon when `glyph` is set.
pub fn small_filled_action_width(t: &Theme, window: &mut gpui::Window, label: &str, glyph: bool) -> Pixels {
    use crate::theme::size::{BUTTON_SMALL_PAD, CHIP_GLYPH, BUTTON_GAP, TEXT_XS};
    let icon = if glyph { t.s(CHIP_GLYPH + BUTTON_GAP) } else { px(0.0) };
    t.s(BUTTON_SMALL_PAD * 2.0) + icon + crate::ui::text_width(window, label, t.text(TEXT_XS), FontWeight::SEMIBOLD)
}

/// The small glass text button, such as a toast's Undo.
pub fn small_glass_action(t: &Theme, id: impl Into<gpui::ElementId>) -> gpui::Stateful<gpui::Div> {
    small_action(t, glass_action(t, id))
}

/// A filled action, such as a dialog's primary or danger button. When disabled the fill stays
/// faintly visible under a legible secondary-text label.
fn filled_action(t: &Theme, id: impl Into<gpui::ElementId>, enabled: bool, fill: Srgb, ink: Hsla) -> gpui::Stateful<gpui::Div> {
    let id = id.into();
    let button = div()
        .id(id.clone())
        .flex()
        .items_center()
        .justify_center()
        .flex_none()
        .gap(t.s(crate::theme::size::BUTTON_GAP))
        .h(t.s(CONTROL_H))
        .px(t.s(BUTTON_PAD))
        .rounded_full()
        .font_weight(FontWeight::SEMIBOLD)
        .bg(fill.hsla(if enabled { 1.0 } else { DISABLED_OPACITY }))
        .shadow(crate::ui::button_edge())
        .text_color(if enabled { ink } else { t.t2().into() });
    if enabled { crate::ui::pressable(button, id) } else { button }
}

pub fn danger_action(t: &Theme, id: impl Into<gpui::ElementId>, enabled: bool) -> gpui::Stateful<gpui::Div> {
    filled_action(t, id, enabled, Srgb::hex(crate::theme::DANGER), t.on_danger())
}

/// A dialog's button, using the shared actions at the dialog's padding.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DialogButton {
    Ghost,
    Primary,
    Danger,
}

pub fn dialog_button(t: &Theme, id: impl Into<gpui::ElementId>, kind: DialogButton, enabled: bool) -> gpui::Stateful<gpui::Div> {
    match kind {
        DialogButton::Ghost => glass_action(t, id).when(!enabled, |d| d.opacity(DISABLED_OPACITY)),
        DialogButton::Primary => primary_action(t, id, enabled),
        DialogButton::Danger => danger_action(t, id, enabled),
    }
}

/// An empty state's action, a glass text button a little below the message.
pub fn empty_action(t: &Theme, id: impl Into<gpui::ElementId>) -> gpui::Stateful<gpui::Div> {
    glass_action(t, id).mt(t.s(8.0)).text_color(t.t1())
}

pub fn primary_action(t: &Theme, id: impl Into<gpui::ElementId>, enabled: bool) -> gpui::Stateful<gpui::Div> {
    filled_action(t, id, enabled, t.accent, t.on_accent.hsla(1.0))
}

/// The glass pill a select or field sits in, without its contents.
pub fn pill_shell(t: &Theme, id: impl Into<gpui::ElementId>) -> gpui::Stateful<gpui::Div> {
    div().id(id).flex().flex_row().items_center().flex_none().min_w(px(0.0)).h(t.s(CONTROL_H)).rounded_full().bg(t.glass_fill()).shadow(glass_edge())
}

impl Shell {
    pub fn bounds_store(&mut self, key: &str) -> Rc<RefCell<Bounds<Pixels>>> {
        self.controls.bounds.entry(key.to_string()).or_default().clone()
    }

    pub fn clear_control_bounds(&mut self) {
        self.controls.bounds.clear();
        self.controls.select = None;
        self.controls.text = None;
    }

    pub fn render_slider(&mut self, spec: SliderSpec, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let store = self.bounds_store(&spec.key);
        let span = (spec.max - spec.min).max(f32::EPSILON);
        let fraction = |v: f32| ((v - spec.min) / span).clamp(0.0, 1.0);
        let (now, origin) = (fraction(spec.value), fraction(spec.origin));
        let (from, to) = (now.min(origin), now.max(origin));
        let (track_h, track_top) = if spec.look == SliderLook::Hue { (6.0, 5.0) } else { (4.0, 6.0) };
        let mut track = div().absolute().left(px(0.0)).right(px(0.0)).top(t.s(track_top)).h(t.s(track_h)).rounded(t.s(track_h / 2.0)).overflow_hidden().bg(t.panel_2());
        if spec.look == SliderLook::Accent {
            track = track.child(div().absolute().top(px(0.0)).bottom(px(0.0)).left(relative(from)).w(relative((to - from).max(0.0))).bg(t.accent_hsla()));
        } else if spec.look == SliderLook::Hue {
            track = track.flex().flex_row().children(hue_segments());
        }
        let thumb = div()
            .absolute()
            .top(t.s(1.0))
            .left(relative(now))
            .ml(t.s(-14.0 * now))
            .size(t.s(14.0))
            .rounded_full()
            .bg(if spec.look == SliderLook::Hue { t.knob() } else { t.t1().into() })
            .shadow(vec![crate::ui::shadow_knob(&t)]);
        let (min, max, step, on_change, on_end) = (spec.min, spec.max, spec.step, spec.on_change.clone(), spec.on_end.clone());
        let thumb_w = t.s(14.0);
        let press_store = store.clone();
        let mut el = div().id(gpui::ElementId::Name(SharedString::from(spec.key.clone()))).relative().h(t.s(16.0)).flex_none();
        el = match spec.width {
            Some(w) => el.w(t.s(w)),
            None => el.w_full(),
        };
        el
            .when(!spec.enabled, |d| d.opacity(0.4))
            .child(track_bounds(store))
            .child(track)
            .child(thumb)
            .when(spec.enabled, |d| {
                d.on_mouse_down(MouseButton::Left, cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.controls.text = None;
                    let (on_change, press_store) = (on_change.clone(), press_store.clone());
                    let apply = Rc::new(move |this: &mut Shell, position: Point<Pixels>, cx: &mut Context<Shell>| {
                        let b = *press_store.borrow();
                        let room = f32::from(b.size.width - thumb_w).max(1.0);
                        let frac = (f32::from(position.x - b.origin.x - thumb_w / 2.0) / room).clamp(0.0, 1.0);
                        let raw = min + frac * (max - min);
                        let snapped = ((raw - min) / step).round() * step + min;
                        on_change(this, snapped.clamp(min, max), cx);
                    });
                    apply(this, event.position, cx);
                    this.controls.pointer = Some(PointerDrag { on_move: apply, on_end: on_end.clone() });
                    this.dragging = Some(Drag::Pointer);
                    cx.notify();
                }))
            })
            .into_any_element()
    }

    pub fn pointer_drag_move(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        if let Some(on_move) = self.controls.pointer.as_ref().map(|p| p.on_move.clone()) {
            on_move(self, position, cx);
        }
    }

    pub fn pointer_drag_end(&mut self, cx: &mut Context<Self>) {
        if let Some(on_end) = self.controls.pointer.take().and_then(|p| p.on_end) {
            on_end(self, cx);
        }
    }

    /// opens the shared glass list.
    #[allow(clippy::too_many_arguments)]
    pub fn select_pill(&mut self, key: &str, value: impl Into<SharedString>, options: Rc<Vec<SelectOption>>, placeholder: &'static str, width: Option<f32>, on_change: impl Fn(&mut Shell, SharedString, &mut Context<Shell>) + 'static, cx: &mut Context<Self>) -> AnyElement {
        self.select_pill_sized(key, value, options, placeholder, width.map_or(PillWidth::Full, PillWidth::Fixed), on_change, cx)
    }

    /// `select_pill` with any width mode, including one sized to its text.
    #[allow(clippy::too_many_arguments)]
    pub fn select_pill_sized(&mut self, key: &str, value: impl Into<SharedString>, options: Rc<Vec<SelectOption>>, placeholder: &'static str, width: PillWidth, on_change: impl Fn(&mut Shell, SharedString, &mut Context<Shell>) + 'static, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let value: SharedString = value.into();
        let store = self.bounds_store(key);
        let open = self.controls.select.as_ref().is_some_and(|s| s.key == key);
        let current = options.iter().find(|o| o.value == value);
        let (text, is_placeholder): (SharedString, bool) = match current {
            Some(o) => (o.label.clone(), false),
            None if !value.is_empty() => (value.clone(), false),
            None => (SharedString::from(placeholder), true),
        };
        let key_owned = key.to_string();
        let on_change: SelectChange = Rc::new(on_change);
        let mut el = div()
            .id(gpui::ElementId::Name(SharedString::from(key.to_string())))
            .relative()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(t.s(6.0))
            .min_w(px(0.0))
            .h(t.s(CONTROL_H))
            .pl(t.s(16.0))
            .pr(t.s(12.0))
            .rounded_full()
            .bg(t.glass_fill())
            .shadow(if open { with_ring(glass_edge(), t.accent_hsla()) } else { glass_edge() })
            .child(crate::ui::hover_fill(crate::ui::HoverShape::Pill, t.glass_hover(), t.glass_press()))
            .child(track_bounds(store.clone()))
            .child(div().when(width != PillWidth::Fit, |d| d.flex_1().min_w(px(0.0)).truncate()).when(is_placeholder, |d| d.text_color(t.t3())).child(text))
            .child(icon(&t, "caret-up-down-bold", 14.0, t.t2()))
            .stop(&t).on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                let anchor = *store.borrow();
                this.toggle_select(key_owned.clone(), value.clone(), options.clone(), on_change.clone(), anchor, cx);
            }));
        el = match width {
            PillWidth::Fixed(w) => el.w(t.s(w)).flex_none(),
            PillWidth::Full => el.w_full(),
            PillWidth::Fit => el.flex_none(),
        };
        el.into_any_element()
    }

    pub fn toggle_select_public(&mut self, key: String, value: SharedString, options: Rc<Vec<SelectOption>>, on_change: SelectChange, anchor: Bounds<Pixels>, cx: &mut Context<Self>) {
        self.toggle_select(key, value, options, on_change, anchor, cx);
    }

    fn toggle_select(&mut self, key: String, value: SharedString, options: Rc<Vec<SelectOption>>, on_change: SelectChange, anchor: Bounds<Pixels>, cx: &mut Context<Self>) {
        if self.controls.select.as_ref().is_some_and(|s| s.key == key) {
            self.close_select(cx);
            return;
        }
        let active = options.iter().position(|o| o.value == value).unwrap_or(0);
        let filtering = options.len() >= FILTER_FROM;
        let scroll = UniformListScrollHandle::new();
        scroll.scroll_to_item(active, ScrollStrategy::Center);
        self.controls.select = Some(SelectState { key, value, options, on_change, anchor, query: String::new(), active, scroll });
        self.controls.text = filtering.then_some(TextTarget::SelectFilter);
        self.search_active = false;
        cx.notify();
    }

    /// Updates the options of the open select named `key` without closing it, as when devices change.
    pub fn refresh_select(&mut self, key: &str, options: Rc<Vec<SelectOption>>, cx: &mut Context<Self>) {
        let Some(state) = self.controls.select.as_mut().filter(|s| s.key == key) else { return };
        state.active = options.iter().position(|o| o.value == state.value).unwrap_or(0).min(options.len().saturating_sub(1));
        state.options = options;
        cx.notify();
    }

    pub fn close_select(&mut self, cx: &mut Context<Self>) {
        if self.controls.select.take().is_some() {
            self.controls.text = None;
            cx.notify();
        }
    }

    fn select_rows(state: &SelectState) -> Vec<SelectRow> {
        let q = state.query.trim().to_lowercase();
        let mut rows = Vec::new();
        let mut last_group: Option<SharedString> = None;
        let mut position = 0;
        for (index, option) in state.options.iter().enumerate() {
            if !q.is_empty() && !option.label.to_lowercase().contains(&q) && !option.group.as_ref().is_some_and(|g| g.to_lowercase().contains(&q)) {
                continue;
            }
            if option.group.is_some() && option.group != last_group {
                rows.push(SelectRow::Heading(option.group.clone().expect("checked")));
            }
            last_group = option.group.clone();
            rows.push(SelectRow::Option { index, position });
            position += 1;
        }
        rows
    }

    pub fn select_choose(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(state) = self.controls.select.take() else { return };
        self.controls.text = None;
        if let Some(option) = state.options.get(index) {
            if let Some(reason) = option.unavailable {
                self.show_toast(reason, cx);
            } else if option.value != state.value {
                (state.on_change)(self, option.value.clone(), cx);
            }
        }
        cx.notify();
    }

    pub fn select_move(&mut self, delta: i32, cx: &mut Context<Self>) {
        let Some(state) = self.controls.select.as_mut() else { return };
        let count = Self::select_rows(state).iter().filter(|r| matches!(r, SelectRow::Option { .. })).count();
        if count == 0 {
            return;
        }
        state.active = (state.active as i32 + delta).clamp(0, count as i32 - 1) as usize;
        self.select_scroll_to_active();
        cx.notify();
    }

    fn select_jump(&mut self, last: bool, cx: &mut Context<Self>) {
        let Some(state) = self.controls.select.as_mut() else { return };
        let count = Self::select_rows(state).iter().filter(|r| matches!(r, SelectRow::Option { .. })).count();
        state.active = if last { count.saturating_sub(1) } else { 0 };
        self.select_scroll_to_active();
        cx.notify();
    }

    fn select_scroll_to_active(&mut self) {
        let Some(state) = self.controls.select.as_ref() else { return };
        let rows = Self::select_rows(state);
        let target = rows.iter().position(|r| matches!(r, SelectRow::Option { position, .. } if *position == state.active));
        if let Some(target) = target {
            state.scroll.scroll_to_item(target, ScrollStrategy::Nearest);
        }
    }

    fn select_activate(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.controls.select.as_ref() else { return };
        let rows = Self::select_rows(state);
        let found = rows.iter().find_map(|r| match r {
            SelectRow::Option { index, position } if *position == state.active => Some(*index),
            _ => None,
        });
        if let Some(index) = found {
            self.select_choose(index, cx);
        }
    }

    /// drawn above everything.
    pub fn render_select(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = self.theme;
        let state = self.controls.select.as_ref()?;
        let rows = Self::select_rows(state);
        let filtering = state.options.len() >= FILTER_FROM;
        let anchor = state.anchor;
        let active = state.active;
        let value = state.value.clone();
        let scroll = state.scroll.clone();
        let query = state.query.clone();
        let row_h = f32::from(t.s(MENU_ROW_H));
        let filter_h = if filtering { f32::from(t.s(MENU_FIND_H + MENU_FIND_GAP)) } else { 0.0 };
        let padding = f32::from(t.s(MENU_PAD * 2.0));
        let rows_h = rows.len().max(1) as f32 * row_h;
        let natural = rows_h + filter_h + padding;
        let viewport = window.viewport_size();
        let top_limit = f32::from(self.edge.top) + 10.0;
        let bottom_limit = f32::from(viewport.height - self.edge.bottom) - 10.0;
        let gap = f32::from(t.s(6.0));
        let (bottom, top) = (f32::from(anchor.origin.y + anchor.size.height), f32::from(anchor.origin.y));
        let below = bottom_limit - bottom - gap;
        let above = top - gap - top_limit;
        let flip = natural > below && above > below;
        let max_h = (if flip { above } else { below }).max(160.0);
        let height = natural.min(max_h);
        let y = if flip { (top - gap - height).max(top_limit) } else { bottom + gap };
        let width = f32::from(anchor.size.width).max(f32::from(t.s(MENU_MIN_W))).min(f32::from(t.s(MENU_MAX_W)));
        let left_limit = f32::from(self.edge.left) + 10.0;
        let right_limit = f32::from(viewport.width - self.edge.right) - 10.0;
        let x = f32::from(anchor.origin.x).min(right_limit - width).max(left_limit);
        let list_h = height - filter_h - padding;
        let count = rows.len();
        let menu_rows = rows.clone();
        let list = uniform_list("select-options", count, cx.processor(move |this: &mut Shell, range: std::ops::Range<usize>, _window, cx| {
            let t = this.theme;
            let Some(options) = this.controls.select.as_ref().map(|s| s.options.clone()) else { return Vec::new() };
            range
                .map(|i| match &menu_rows[i] {
                    SelectRow::Heading(label) => div().flex().items_center().h(px(row_h)).px(t.s(MENU_ROW_PAD_X)).text_size(t.text(MENU_DETAIL_TEXT)).font_weight(FontWeight::SEMIBOLD).text_color(t.t2()).child(label.clone()).into_any_element(),
                    SelectRow::Option { index, position } => {
                        let (index, position) = (*index, *position);
                        let option = &options[index];
                        let is_active = position == active;
                        div()
                            .id(("select-option", index))
                            .w_full()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(t.s(MENU_GAP))
                            .h(px(row_h))
                            .px(t.s(MENU_ROW_PAD_X))
                            .rounded(t.s(MENU_ROW_R))
                            .when(is_active, |d| d.bg(t.hover()))
                            .on_click(cx.listener(move |this, _, _, cx| this.select_choose(index, cx)))
                            .when(option.unavailable.is_some(), |d| d.text_color(t.t3()))
                            .child(div().flex_1().min_w(px(0.0)).truncate().child(option.label.clone()))
                            .when(option.value == value, |d| d.child(icon(&t, "check-bold", MENU_ICON, t.accent_hsla())))
                            .into_any_element()
                    }
                })
                .collect()
        }))
        .track_scroll(&scroll)
        .h(px(list_h));
        let mut menu = crate::ui::float_panel(&t).w(px(width)).h(px(height)).p(t.s(MENU_PAD)).flex().flex_col().text_size(t.text(MENU_TEXT)).text_color(t.t1());
        if filtering {
            let line = self.text_line(&TextTarget::SelectFilter, &query, tr("Filter"), "select-filter-text", window);
            let clear = self.search_clear(TextTarget::SelectFilter, "select-filter-clear", &query, SEARCH_CLEAR_SM, cx);
            menu = menu.child(crate::ui::float_filter(&t, "select-filter", line, clear));
        }
        menu = menu.child(if count == 0 {
            div().px(t.s(MENU_ROW_PAD_X)).py(t.s(MENU_EMPTY_PAD_Y)).text_color(t.t2()).child(tr("No matches")).into_any_element()
        } else {
            list.into_any_element()
        });
        let scrim = div()
            .id("select-scrim")
            .absolute()
            .inset_0()
            .occlude()
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.close_select(cx)))
            .on_scroll_wheel(cx.listener(|this, _: &gpui::ScrollWheelEvent, _, cx| this.close_select(cx)))
            .child(anchored().position(point(px(x), px(y))).snap_to_window_with_margin(self.float_margin()).child(menu));
        Some(deferred(scrim).with_priority(10).into_any_element())
    }

    pub fn focus_text(&mut self, target: TextTarget, cx: &mut Context<Self>) {
        self.focus_field(target, cx);
    }

    /// Keys for an open select and the focused field. Returns true when handled.
    pub fn controls_key(&mut self, event: &gpui::KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let key = event.keystroke.key.as_str();
        if self.controls.select.is_some() {
            match key {
                "down" => self.select_move(1, cx),
                "up" => self.select_move(-1, cx),
                "home" => self.select_jump(false, cx),
                "end" => self.select_jump(true, cx),
                "enter" => self.select_activate(cx),
                "space" if self.controls.text.is_none() => self.select_activate(cx),
                _ => {
                    if self.controls.text == Some(TextTarget::SelectFilter) {
                        self.type_into(&TextTarget::SelectFilter, event, cx);
                    }
                }
            }
            return true;
        }
        if let Some(target) = self.controls.text.clone() {
            self.type_into(&target, event, cx);
            return true;
        }
        if matches!(self.route, Route::Equalizer) {
            return self.eq_key(event, cx);
        }
        false
    }

    fn type_into(&mut self, target: &TextTarget, event: &gpui::KeyDownEvent, cx: &mut Context<Self>) {
        self.field_key(target, event, cx);
    }

    pub fn text_value(&self, target: &TextTarget) -> String {
        match target {
            TextTarget::AccentHex => self.controls.hex_text.clone(),
            TextTarget::DiscordAddress => self.prefs.discord_public_address.clone(),
            TextTarget::FriendId => self.listen.draft_id.clone(),
            TextTarget::FriendName => self.listen.draft_name.clone(),
            TextTarget::LanyardAddress => self.prefs.lanyard_address.clone(),
            TextTarget::EqPresetName => self.eq_ui.preset_name.clone(),
            TextTarget::SmartName => self.smart.name.clone(),
            TextTarget::NewPlaylistName => self.new_playlist.as_ref().map(|d| d.name.clone()).unwrap_or_default(),
            TextTarget::NewPlaylistDescription => self.new_playlist.as_ref().map(|d| d.description.clone()).unwrap_or_default(),
            TextTarget::ShareDescription | TextTarget::ShareDate => self.share_text(target),
            TextTarget::PlaylistName | TextTarget::PlaylistDescription => self.edit_playlist_text(target),
            TextTarget::SmartLimit => self.smart.state.limit.clone(),
            TextTarget::SmartValue { row, side } => self.smart_value_text(*row, *side),
            TextTarget::SelectFilter => self.controls.select.as_ref().map(|s| s.query.clone()).unwrap_or_default(),
            TextTarget::ConnectAddress | TextTarget::ConnectUser | TextTarget::ConnectPassword => self.connect_field(target),
            TextTarget::Page => self.page_field_value(),
            TextTarget::MenuFind => self.menu_find_text(),
            TextTarget::AirPlayPin => self.airplay_pin_text(),
            TextTarget::FilterFind | TextTarget::FilterFrom | TextTarget::FilterTo => self.filter_text(target),
        }
    }

    pub fn set_text_value(&mut self, target: &TextTarget, value: String, cx: &mut Context<Self>) {
        match target {
            TextTarget::AccentHex => self.accent_hex_typed(value, cx),
            TextTarget::DiscordAddress => {
                self.prefs.discord_public_address = value;
                self.prefs.save();
                cx.notify();
            }
            TextTarget::FriendId => {
                self.listen.draft_id = value.chars().filter(|c| !c.is_whitespace()).take(24).collect();
                cx.notify();
            }
            TextTarget::FriendName => {
                self.listen.draft_name = value;
                cx.notify();
            }
            TextTarget::LanyardAddress => {
                self.prefs.lanyard_address = value;
                self.prefs.save();
                cx.notify();
            }
            TextTarget::EqPresetName => {
                self.eq_ui.preset_name = value.chars().take(crate::eq_model::NAME_MAX).collect();
                cx.notify();
            }
            TextTarget::NewPlaylistName | TextTarget::NewPlaylistDescription => {
                if let Some(dialog) = self.new_playlist.as_mut() {
                    if *target == TextTarget::NewPlaylistName { dialog.name = value } else { dialog.description = value }
                    dialog.error = None;
                    cx.notify();
                }
            }
            TextTarget::ShareDescription | TextTarget::ShareDate => self.set_share_text(target, value, cx),
            TextTarget::PlaylistName | TextTarget::PlaylistDescription => self.set_edit_playlist_text(target, value, cx),
            TextTarget::SmartName => {
                self.smart.name = value;
                self.smart.error = None;
                cx.notify();
            }
            TextTarget::SmartLimit => {
                self.smart.state.set_limit(&value);
                self.smart_changed(cx);
            }
            TextTarget::SmartValue { row, side } => self.smart_value_typed(*row, *side, value, cx),
            TextTarget::SelectFilter => {
                if let Some(state) = self.controls.select.as_mut() {
                    state.query = value;
                    state.active = 0;
                    self.select_scroll_to_active();
                }
                cx.notify();
            }
            TextTarget::ConnectAddress | TextTarget::ConnectUser | TextTarget::ConnectPassword => self.connect_field_typed(target, value, cx),
            TextTarget::Page => self.set_page_field(value, cx),
            TextTarget::MenuFind => self.set_menu_find_text(value, cx),
            TextTarget::AirPlayPin => self.airplay_pin_typed(value, cx),
            TextTarget::FilterFind | TextTarget::FilterFrom | TextTarget::FilterTo => self.filter_text_typed(target, value, cx),
        }
    }

    /// Enter in a field commits it, and is otherwise harmless.
    pub fn controls_enter(&mut self, cx: &mut Context<Self>) -> bool {
        if self.controls.select.is_some() {
            self.select_activate(cx);
            return true;
        }
        match self.controls.text.clone() {
            Some(TextTarget::EqPresetName) => {
                self.eq_commit_preset(cx);
                true
            }
            Some(TextTarget::FriendId | TextTarget::FriendName) => {
                self.add_friend(cx);
                true
            }
            Some(TextTarget::LanyardAddress) => {
                self.controls.text = None;
                self.apply_friends(cx);
                true
            }
            Some(TextTarget::ConnectAddress | TextTarget::ConnectUser | TextTarget::ConnectPassword) => {
                self.connect_submit(cx);
                true
            }
            Some(TextTarget::NewPlaylistName | TextTarget::NewPlaylistDescription) => {
                self.new_playlist_submit(cx);
                true
            }
            Some(TextTarget::AirPlayPin) => {
                self.airplay_pin_submit(cx);
                true
            }
            Some(TextTarget::ShareDescription | TextTarget::ShareDate) => {
                self.share_submit(cx);
                true
            }
            Some(TextTarget::PlaylistName | TextTarget::PlaylistDescription) => {
                self.edit_playlist_submit(cx);
                true
            }
            Some(_) => {
                self.controls.text = None;
                cx.notify();
                true
            }
            None => false,
        }
    }

    /// Esc closes a select or leaves a field. Returns true when something closed.
    pub fn controls_dismiss(&mut self, cx: &mut Context<Self>) -> bool {
        if self.controls.select.is_some() {
            self.close_select(cx);
            return true;
        }
        if let Some(target) = self.controls.text.take() {
            if target == TextTarget::EqPresetName {
                self.eq_ui.naming = false;
            }
            cx.notify();
            return true;
        }
        false
    }

    /// Leaves the current field on a press outside any field or select. Runs before the press
    /// reaches anything, so a pressed field takes the keyboard straight back.
    pub fn controls_outside_press(&mut self, cx: &mut Context<Self>) {
        if self.controls.select.is_some() {
            return;
        }
        let page_field = std::mem::take(&mut self.search_active);
        if self.controls.text.take().is_some() || page_field {
            cx.notify();
        }
    }
}

/// relative to its parent, for pointer math.
pub fn track_bounds(store: Rc<RefCell<Bounds<Pixels>>>) -> impl IntoElement {
    crate::view_topbar::track_bounds(store)
}

fn hue_segments() -> Vec<AnyElement> {
    const STOPS: [u32; 8] = [0xff6666, 0xffcc66, 0x99ff66, 0x66ffcc, 0x66ccff, 0x9966ff, 0xff66cc, 0xff6666];
    (0..STOPS.len() - 1)
        .map(|i| div().flex_1().h_full().bg(gpui::linear_gradient(90.0, gpui::linear_color_stop(gpui::rgb(STOPS[i]), 0.0), gpui::linear_color_stop(gpui::rgb(STOPS[i + 1]), 1.0))).into_any_element())
        .collect()
}


