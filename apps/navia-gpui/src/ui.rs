//! Small building blocks that replace framework defaults, such as icons, glass pills and icon buttons.

use crate::i18n::n_;
use gpui::prelude::*;
use gpui::{AnyElement, BoxShadow, Div, ElementId, FontWeight, Hsla, SharedString, Stateful, Svg, div, point, px, relative, svg};

use crate::assets::icon as icon_path;
use crate::animation;
use crate::theme::{PressPhase, Theme, motion};
use crate::theme::size::*;

/// Whether the renderer can blur what's beneath an element, as reported by the main window's
/// renderer. Windows assumes not until then, since its DirectX blur pipeline can fail to build.
static BACKDROP_BLUR: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(cfg!(not(target_os = "windows")));

pub fn has_backdrop_blur() -> bool {
    BACKDROP_BLUR.load(std::sync::atomic::Ordering::Relaxed)
}

/// Records the answer from a newly opened main window's renderer and logs it once.
pub fn note_backdrop_blur(window: &gpui::Window) {
    static SAID: std::sync::Once = std::sync::Once::new();
    let on = window.supports_backdrop_blur();
    BACKDROP_BLUR.store(on, std::sync::atomic::Ordering::Relaxed);
    SAID.call_once(|| {
        if on {
            eprintln!("navia: backdrop blur on");
        } else {
            eprintln!("navia: backdrop blur unavailable, using solid stand-in fills");
        }
    });
}

/// Blurs what has already been painted beneath this element's rounded bounds. Later children
/// composite over the blur, so put the fill and contents after it.
pub fn backdrop_blur(corners: gpui::Corners<gpui::Pixels>, radius: gpui::Pixels, saturation: f32, bottom_fade: gpui::Pixels) -> impl IntoElement {
    backdrop_blur_faded(corners, radius, saturation, gpui::Edges { bottom: bottom_fade, ..Default::default() })
}

/// [`backdrop_blur`] fading out at its edges. `fades` is how far in from the top, right, bottom
/// and left the blur thins to nothing, with zero for a hard edge.
pub fn backdrop_blur_faded(corners: gpui::Corners<gpui::Pixels>, radius: gpui::Pixels, saturation: f32, fades: gpui::Edges<gpui::Pixels>) -> impl IntoElement {
    gpui::canvas(|_, _, _| {}, move |bounds, _, window, _| {
        if has_backdrop_blur() {
            window.paint_backdrop_blur_faded(bounds, corners, radius, saturation, fades);
        }
    })
    // Pin to all four edges. An absolute child with no insets sits inside the parent's padding,
    // which would shift the blur over the right and bottom edges.
    .absolute()
    .inset_0()
}

/// The layers of a glass surface, the blur of what's beneath and then its fill and edge. Call
/// it before the element's children. Without backdrop blur it falls back to a nearly opaque fill.
pub fn glass_layers<E: gpui::ParentElement + gpui::Styled>(el: E, t: &Theme, radius: gpui::Pixels, blur: f32, saturation: f32, frosted_fill: gpui::Rgba, stand_in_fill: gpui::Rgba, edge: Vec<BoxShadow>) -> E {
    if !has_backdrop_blur() {
        return el.bg(stand_in_fill).shadow(edge);
    }
    el.child(backdrop_blur(gpui::Corners::all(radius), t.s(blur), saturation, px(0.0)))
        .child(div().absolute().inset_0().rounded(radius).bg(frosted_fill).shadow(edge))
}

pub fn icon(t: &Theme, name: &str, token_size: f32, color: impl Into<Hsla>) -> Svg {
    svg().path(icon_path(&format!("{name}.svg"))).flex_none().size(t.s(token_size)).text_color(color)
}

/// Draws every image in the app, always with `Cover` fit so images never stretch. A bake drawn
/// while its box has a new size is cropped until the new bake arrives. Don't call GPUI's `img()`
/// directly, since it defaults to another fit.
pub fn image(source: std::sync::Arc<gpui::RenderImage>) -> gpui::Img {
    gpui::img(gpui::ImageSource::Render(source)).object_fit(gpui::ObjectFit::Cover)
}

/// The inset light edge every glass surface has.
pub fn glass_edge() -> Vec<BoxShadow> {
    edge_ring(crate::theme::GLASS_EDGE)
}

/// `--menu-edge`: the fainter hairline of menus and popovers.
pub fn menu_edge() -> Vec<BoxShadow> {
    edge_ring(crate::theme::MENU_EDGE)
}

fn edge_ring(ring: u32) -> Vec<BoxShadow> {
    vec![
        BoxShadow {
            color: gpui::rgba(ring).into(),
            offset: point(px(0.0), px(0.0)),
            blur_radius: px(0.0),
            spread_radius: px(1.0),
            inset: true,
        },
        BoxShadow {
            color: gpui::rgba(crate::theme::GLASS_EDGE_TOP).into(),
            offset: point(px(0.0), px(1.0)),
            blur_radius: px(0.0),
            spread_radius: px(0.0),
            inset: true,
        },
    ]
}

/// The inset light hairline of a filled button.
pub fn button_edge() -> Vec<BoxShadow> {
    vec![BoxShadow { color: gpui::rgba(crate::theme::BUTTON_EDGE).into(), offset: point(px(0.0), px(0.0)), blur_radius: px(0.0), spread_radius: px(1.0), inset: true }]
}

/// The glass edge followed by a drop shadow.
pub fn edge_and(shadow: BoxShadow) -> Vec<BoxShadow> {
    glass_edge().into_iter().chain([shadow]).collect()
}

fn drop(t: &Theme, color: u32, y: f32, blur: f32, spread: f32) -> BoxShadow {
    BoxShadow { color: gpui::rgba(color).into(), offset: point(px(0.0), t.s(y)), blur_radius: t.s(blur), spread_radius: t.s(spread), inset: false }
}

/// The shadow for floating panels, dialogs and the bottom bar.
pub fn shadow_float(t: &Theme) -> BoxShadow {
    let (color, y, blur, spread) = SHADOW_FLOAT;
    drop(t, color, y, blur, spread)
}

/// How far the float shadow reaches past its element on its longest side, in tokens. A window
/// of its own, like the mini player, keeps this much room. GPUI treats a shadow's blur radius
/// as the gaussian's sigma and draws out to three of them.
pub fn shadow_float_reach() -> f32 {
    let (_, y, blur, spread) = SHADOW_FLOAT;
    y + blur * 3.0 + spread
}

/// The shadow for large cover art on album, playlist and Discover pages.
pub fn shadow_art(t: &Theme) -> BoxShadow {
    let (color, y, blur, spread) = SHADOW_ART;
    drop(t, color, y, blur, spread)
}

/// header art with `--shadow-art`; the shadow's box stays the art's size however the header aligns or stretches it.
pub fn hero_art_frame(t: &Theme, radius: f32, art: impl IntoElement) -> Div {
    div().flex_none().flex().flex_col().items_start().child(div().relative().flex_none().rounded(t.s(radius)).shadow(vec![shadow_art(t)]).child(art))
}

/// `--shadow-pill`: the top bar's glass pills.
pub fn shadow_pill(t: &Theme) -> BoxShadow {
    drop(t, 0x00000080, 8.0, 26.0, -10.0)
}

/// a box of `color` blurred as css `filter: blur()` draws it, `blur` its deviation; the caller sets its insets.
/// a drop shadow is kept off its own element, so a copy moved out of the way casts it back onto the box.
pub fn blurred_fill(color: impl Into<Hsla>, radius: gpui::Pixels, blur: gpui::Pixels) -> Div {
    // further than any box plus its blur's reach, so the cut-out under the caster misses the shadow
    let away = px(1.0e5);
    // a shadow's blur radius is about twice css blur()'s deviation
    let shadow = BoxShadow { color: color.into(), offset: point(away, px(0.0)), blur_radius: blur * 2.0, spread_radius: px(0.0), inset: false };
    div().absolute().child(div().absolute().top(px(0.0)).bottom(px(0.0)).left(-away).right(away).rounded(radius).shadow(vec![shadow]))
}

/// `--shadow-player-art`: the expanded player's cover.
pub fn shadow_player_art(t: &Theme) -> BoxShadow {
    drop(t, 0x00000099, 30.0, 70.0, -22.0)
}

/// The dock's shadow onto the page, to its left.
pub fn shadow_dock(t: &Theme) -> BoxShadow {
    BoxShadow { color: gpui::rgba(0x0000004f).into(), offset: point(t.s(-11.0), px(0.0)), blur_radius: t.s(22.0), spread_radius: t.s(-7.0), inset: false }
}

/// An inset 1px ring for edges drawn as an overlay above artwork, since inset shadows paint under children.
pub fn inset_ring(color: gpui::Rgba) -> BoxShadow {
    BoxShadow { color: color.into(), offset: point(px(0.0), px(0.0)), blur_radius: px(0.0), spread_radius: px(1.0), inset: true }
}

/// The shadow for a sidebar row being dragged.
pub fn shadow_ghost(t: &Theme) -> BoxShadow {
    drop(t, 0x00000066, 6.0, 18.0, 0.0)
}

/// `--shadow-knob`: slider thumbs.
pub fn shadow_knob(t: &Theme) -> BoxShadow {
    drop(t, 0x00000066, 1.0, 4.0, 0.0)
}

/// The shadow for the EQ graph's handles.
pub fn shadow_eq_handle(t: &Theme) -> BoxShadow {
    drop(t, 0x00000073, 2.0, 8.0, 0.0)
}

/// The accent pill with the count on lifted songs, like "3 songs".
pub fn count_badge(t: &Theme, text: impl Into<SharedString>) -> Div {
    div().h(t.s(20.0)).px(t.s(8.0)).flex().items_center().rounded(t.s(10.0)).bg(t.accent_hsla()).text_color(t.on_accent.hsla(1.0)).text_size(t.s(12.0)).font_weight(FontWeight::SEMIBOLD).whitespace_nowrap().child(text.into())
}

/// The shadow for a row lifted by a drag.
pub fn shadow_lift(t: &Theme) -> BoxShadow {
    drop(t, 0x00000073, 12.0, 30.0, 0.0)
}

/// A pill-shaped glass container for one or more controls. Falls back to a mostly opaque fill
/// without backdrop blur.
pub fn glass_pill(t: &Theme) -> Div {
    let pill = div().flex().flex_row().items_center().flex_none().h(t.s(CONTROL_H)).px(t.s(3.0)).rounded_full();
    glass_layers(pill, t, t.s(CONTROL_H / 2.0), GLASS_BLUR, GLASS_SATURATION, t.glass(), t.glass_fill(), glass_edge())
}

/// A small single-line float chip, like the mini player's time chip. It moves with the playhead
/// every frame, so it uses the stand-in fill instead of its own backdrop blur.
pub fn float_chip(t: &Theme) -> Div {
    div().flex_none().px(t.s(8.0)).py(t.s(3.0)).rounded_full().bg(t.float_fill()).shadow(glass_edge()).text_size(t.text(TEXT_XS)).line_height(relative(1.2)).text_color(t.t1()).whitespace_nowrap()
}

/// A hover tip shown by GPUI after its hover delay. Pass `tip(&t, text)` to `.tooltip(...)`.
pub struct Tip {
    theme: Theme,
    text: SharedString,
    /// shown only while this is set (see [`FullTextTip`]).
    when: Option<std::rc::Rc<std::cell::Cell<bool>>>,
}

impl gpui::Render for Tip {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        if self.when.as_ref().is_some_and(|on| !on.get()) {
            return div();
        }
        float_chip(&self.theme).child(self.text.clone())
    }
}

pub fn tip(t: &Theme, text: impl Into<SharedString>) -> impl Fn(&mut gpui::Window, &mut gpui::App) -> gpui::AnyView + 'static {
    let (theme, text) = (*t, text.into());
    move |_, cx| cx.new(|_| Tip { theme, text: text.clone(), when: None }).into()
}

/// text that can be cut short (`truncate`, `line_clamp`) shows its full text as its tip, only while it's cut.
/// call it on the box that cuts; credits pass their joined list.
pub trait FullTextTip: Sized {
    fn full_text_tip(self, t: &Theme, text: impl Into<SharedString>) -> Self;
}

impl FullTextTip for Stateful<Div> {
    fn full_text_tip(self, t: &Theme, text: impl Into<SharedString>) -> Self {
        let (theme, text) = (*t, text.into());
        let cut = std::rc::Rc::new(std::cell::Cell::new(false));
        let probe = {
            let (cut, text) = (cut.clone(), text.clone());
            gpui::canvas(move |bounds, window, _| cut.set(is_cut(window, &text, bounds.size.width)), |_, _, _, _| {}).absolute().size_full()
        };
        // Taffy places an absolute child in its own parent, so the element keeps its own position.
        self.child(probe).tooltip(move |_, cx| cx.new(|_| Tip { theme, text: text.clone(), when: Some(cut.clone()) }).into())
    }
}

/// whether `text` in the inherited style overflows `width`: past the width on one line, or past the line clamp when wrapped.
fn is_cut(window: &mut gpui::Window, text: &SharedString, width: gpui::Pixels) -> bool {
    if text.is_empty() || width <= px(0.0) {
        return false;
    }
    let style = window.text_style();
    let size = style.font_size.to_pixels(window.rem_size());
    let run = style.to_run(text.len());
    let line = text.replace('\n', " ");
    match style.line_clamp {
        Some(lines) => window
            .text_system()
            .shape_text(line.into(), size, &[run], Some(width), None)
            .map(|wrapped| wrapped.iter().map(|l| l.wrap_boundaries().len() + 1).sum::<usize>() > lines)
            .unwrap_or(false),
        // half a pixel of slack for subpixel layout.
        None => window.text_system().shape_line(line.into(), size, &[run], None).width > width + px(0.5),
    }
}

/// a floating panel (`--menu-glass`, `--r-float`, `--shadow-float`); `relative` so its glass layers fill it, and a later `absolute()` still wins.
pub fn float_panel(t: &Theme) -> Div {
    let radius = t.s(R_FLOAT);
    let edge = menu_edge().into_iter().chain([shadow_float(t)]).collect();
    glass_layers(div().relative().rounded(radius).occlude(), t, radius, MENU_BLUR, MENU_SATURATION, t.menu_glass(), t.float_fill(), edge)
}

/// the filter field at the top of a floating list. `line` is the field's text line.
pub fn float_filter(t: &Theme, id: &'static str, line: impl IntoElement, clear: Option<AnyElement>) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .flex_none()
        .gap(t.s(MENU_FIND_GLYPH_GAP))
        .h(t.s(MENU_FIND_H))
        .mb(t.s(MENU_FIND_GAP))
        .px(t.s(MENU_FIND_PAD))
        .rounded(t.s(MENU_ROW_R))
        .bg(t.row())
        .text_size(t.text(MENU_TEXT))
        .child(icon(t, "magnifying-glass-regular", MENU_FIND_ICON, t.t2()))
        .child(line)
        .children(clear)
}

/// A dialog's or toast's glass surface with the given corner radius.
pub fn dialog_surface(t: &Theme, radius: gpui::Pixels) -> Div {
    glass_layers(div().relative().rounded(radius), t, radius, FLOAT_BLUR, FLOAT_SATURATION, t.dialog_glass(), t.dialog_fill(), edge_and(shadow_float(t)))
}

/// A modal dialog's scrim over the window, centering its panel. The caller handles closing on
/// an outside click, and the panel comes from `dialog_panel`.
pub fn dialog_scrim(t: &Theme, id: &'static str) -> Stateful<Div> {
    div().absolute().inset_0().id(id).occlude().flex().items_center().justify_center().bg(t.scrim())
}

/// A modal dialog's panel, which keeps clicks inside it from reaching the scrim. Add
/// `dialog_title`, text, fields and `dialog_actions` as children.
pub fn dialog_panel(t: &Theme, id: &'static str) -> Stateful<Div> {
    dialog_surface(t, t.s(DIALOG_R))
        .id(id)
        .w(t.s(DIALOG_W))
        .max_w_full()
        .p(t.s(DIALOG_PAD))
        .flex()
        .flex_col()
        .gap(t.s(DIALOG_GAP))
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

/// The dialog's sheet variant, as used by Get Info. Its height is capped by a token and the
/// window. Children are a head, a scrolling body (`flex_1`, `min_h(0)`) and `dialog_sheet_actions`.
pub fn dialog_sheet(t: &Theme, id: &'static str, window_h: gpui::Pixels) -> Stateful<Div> {
    dialog_panel(t, id).w(t.s(SHEET_W)).max_h(t.s(SHEET_MAX_H).min(window_h - t.s(40.0))).pb(t.s(DIALOG_PAD - DIALOG_ACTIONS_TOP))
}

/// The dialog's cover variant, as used by an artist's About. The sheet's size with no padding
/// so its picture runs to the edges. Children are a scrolling body and a round close button.
pub fn dialog_cover(t: &Theme, id: &'static str, window_h: gpui::Pixels) -> Stateful<Div> {
    dialog_surface(t, t.s(DIALOG_R))
        .id(id)
        .w(t.s(SHEET_W))
        .max_w_full()
        .max_h(t.s(SHEET_MAX_H).min(window_h - t.s(40.0)))
        .flex()
        .flex_col()
        .overflow_hidden()
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

/// A cover sheet's picture at the top of its body, already faded out, with the text below
/// rising `SHEET_COVER_OVERLAP` over the fade. Its space is held while it bakes.
pub fn dialog_cover_picture(t: &Theme, image: Option<std::sync::Arc<gpui::RenderImage>>) -> Div {
    div()
        .h(t.s(SHEET_COVER_H))
        .mx(-t.s(DIALOG_PAD))
        .mb(-t.s(SHEET_COVER_OVERLAP))
        .when_some(image, |d, image| d.child(self::image(image).size_full()))
}

/// A sheet's buttons, right-aligned under a hairline across the sheet's full width.
pub fn dialog_sheet_actions(t: &Theme) -> Div {
    dialog_actions(t).mt(px(0.0)).mx(-t.s(DIALOG_PAD)).px(t.s(DIALOG_PAD)).pt(t.s(DIALOG_GAP)).border_t_1().border_color(t.line())
}

/// A dialog's title.
pub fn dialog_title(t: &Theme, text: impl Into<SharedString>) -> Div {
    div().text_size(t.text(TEXT_SECTION)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(t.t1()).child(text.into())
}

/// Small danger text explaining why something failed, shown under the form it belongs to.
pub fn error_text(t: &Theme, text: impl Into<SharedString>) -> Div {
    div().text_size(t.text(TEXT_SM)).text_color(gpui::rgb(crate::theme::DANGER)).line_height(gpui::relative(1.35)).child(text.into())
}

/// A dialog's body text.
pub fn dialog_text(t: &Theme, text: impl Into<SharedString>) -> Div {
    div().text_color(t.t2()).line_height(gpui::relative(1.4)).child(text.into())
}

/// A dialog's small print.
pub fn dialog_note(t: &Theme, text: impl Into<SharedString>) -> Div {
    div().text_size(t.text(TEXT_SM)).text_color(t.t3()).line_height(gpui::relative(1.4)).child(text.into())
}

/// A labeled control in a dialog, with the label above it.
pub fn dialog_block(t: &Theme, label: impl Into<SharedString>, control: impl IntoElement) -> Div {
    div().flex().flex_col().gap(t.s(6.0)).text_size(t.text(TEXT_SM)).text_color(t.t2()).child(label.into()).child(control)
}

/// A dialog's switch row, with its label and hint and the switch at the end.
pub fn dialog_switch(t: &Theme, label: impl Into<SharedString>, hint: impl Into<SharedString>, control: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap(t.s(16.0))
        .child(div().min_w(px(0.0)).child(div().font_weight(gpui::FontWeight::MEDIUM).child(label.into())).child(div().text_size(t.text(TEXT_SM)).text_color(t.t2()).child(hint.into())))
        .child(control)
}

/// A dialog's buttons, right-aligned.
pub fn dialog_actions(t: &Theme) -> Div {
    div().flex().flex_row().justify_end().gap(t.s(DIALOG_ACTIONS_GAP)).mt(t.s(DIALOG_ACTIONS_TOP))
}

/// A dialog's text field. A glass pill with the focus ring, an optional icon, then `line`.
pub fn dialog_field(t: &Theme, id: &'static str, focused: bool, glyph: Option<&str>, line: impl IntoElement) -> Stateful<Div> {
    let edge = if focused { crate::ui_controls::with_ring(glass_edge(), t.accent_hsla()) } else { glass_edge() };
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap(t.s(9.0))
        .h(t.s(CONTROL_H))
        .px(t.s(FIELD_PAD))
        .rounded_full()
        .bg(t.glass_fill())
        .shadow(edge)
        .text_color(t.t2())
        .cursor_text()
        .when_some(glyph, |d, g| d.child(icon(t, g, 16.0, t.t2())))
        .child(line)
}

/// The 2px accent focus ring as a separate outline around a translucent control. As a shadow it
/// would show through under a frosted fill.
pub fn focus_ring(t: &Theme, radius: gpui::Pixels) -> Div {
    outline_ring(radius, t.accent_hsla(), px(2.0))
}

/// An outer ring in `color` around a translucent control, drawn as its own outline. Used for the
/// focus ring and the invalid ring.
pub fn outline_ring(radius: gpui::Pixels, color: impl Into<Hsla>, w: gpui::Pixels) -> Div {
    div().absolute().top(-w).bottom(-w).left(-w).right(-w).rounded(radius + w).border(w).border_color(color.into())
}

/// A search field's size, a page toolbar's or the Search page's large one.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SearchSize {
    Toolbar,
    Large,
}

/// a glass search field; focus is the outer accent ring, not a border, so nothing inside moves.
pub fn search_field(t: &Theme, id: &'static str, size: SearchSize, width: SearchWidth, focused: bool, line: impl IntoElement, clear: Option<AnyElement>) -> Stateful<Div> {
    let (h, pad, gap, glyph) = size.metrics();
    let edge = if size == SearchSize::Large { edge_and(shadow_pill(t)) } else { glass_edge() };
    let el = div().id(id).relative().flex().flex_row().items_center().gap(t.s(gap)).h(t.s(h)).px(t.s(pad)).rounded_full();
    let el = match width {
        SearchWidth::Fixed(w) => el.w(t.s(w)),
        SearchWidth::Fit { step, fit } => el.w(fit).flex_shrink(1.0).min_w(t.s(step)),
        SearchWidth::Fill { min, max } => el.w_full().min_w(t.s(min)).max_w(t.s(max)),
    };
    glass_layers(el, t, t.s(h / 2.0), GLASS_BLUR, GLASS_SATURATION, t.glass(), t.glass_fill(), edge)
        .when(focused, |d| d.child(focus_ring(t, t.s(h / 2.0))))
        .occlude()
        .cursor_text()
        .child(icon(t, "magnifying-glass-regular", glyph, t.t2()))
        .child(line)
        .children(clear)
}

/// a toolbar search field's narrowest, in steps as the page narrows (`--field-w`).
pub fn pill_search_width(main_tokens: f32) -> f32 {
    if main_tokens <= 620.0 {
        SEARCH_PILL_W_NARROW
    } else if main_tokens <= 1000.0 {
        SEARCH_PILL_W_MID
    } else {
        SEARCH_PILL_W
    }
}

/// how wide a search field is: fixed, or `fit` (its placeholder whole) and narrowing to `step`
/// only when the bar has no more room.
#[derive(Clone, Copy)]
pub enum SearchWidth {
    Fixed(f32),
    Fit { step: f32, fit: gpui::Pixels },
    /// the room it's given, between `min` and `max` tokens.
    Fill { min: f32, max: f32 },
}

impl SearchSize {
    /// height, side padding, gap and glyph, in tokens.
    fn metrics(self) -> (f32, f32, f32, f32) {
        match self {
            SearchSize::Toolbar => (CONTROL_H, SEARCH_PILL_PAD, SEARCH_PILL_GAP, 18.0),
            SearchSize::Large => (53.0, 19.0, 11.0, 22.0),
        }
    }

    /// as wide as `placeholder` shows whole in this field.
    pub fn fit(self, t: &Theme, cx: &gpui::App, placeholder: &str) -> gpui::Pixels {
        let (_, pad, gap, glyph) = self.metrics();
        let text = if self == SearchSize::Large { TEXT_LG } else { TEXT_MD };
        t.s(2.0 * pad + gap + glyph) + text_width_app(cx, placeholder, t.text(text), FontWeight::NORMAL).ceil()
    }

    /// its clear button's size (`SEARCH_CLEAR*`).
    pub fn clear(self) -> f32 {
        match self {
            SearchSize::Toolbar => SEARCH_CLEAR,
            SearchSize::Large => SEARCH_CLEAR_LG,
        }
    }
}

/// the dark veil and glyph over artwork while `key`'s `hover_track` is under the pointer. `radius` is the art's.
pub fn art_overlay(t: &Theme, key: impl Into<ElementId>, radius: gpui::Pixels, glyph: &str, size: f32) -> Option<Div> {
    let shown = ink(key).amount;
    (shown > 0.0).then(|| {
        div()
            .absolute()
            .inset_0()
            .rounded(radius)
            .bg(gpui::rgba(crate::theme::WINDOW_BUTTON_ON_ART))
            .flex()
            .items_center()
            .justify_center()
            .opacity(shown)
            .child(icon(t, glyph, size, t.t_on_art()))
    })
}

pub fn divider(t: &Theme) -> Div {
    div().w(px(1.0)).h(t.s(20.0)).mx(t.s(3.0)).bg(t.glass_divider()).flex_none()
}

/// `--t1` at `ICON_REST`, brightening on hover; on the glass when active. every icon-only button takes its `label` as its tip.
pub fn icon_button(t: &Theme, id: impl Into<ElementId>, label: impl Into<SharedString>, name: &str, active: bool) -> Stateful<Div> {
    icon_button_colored(t, id, label, name, active, t.t1().into(), t.hover_ink())
}

/// a small icon button in `color` that brightens to `hover`; active, it sits on `--glass-on`, easing in and out.
pub fn icon_button_colored(t: &Theme, id: impl Into<ElementId>, label: impl Into<SharedString>, name: &str, active: bool, color: Hsla, hover: Hsla) -> Stateful<Div> {
    icon_button_drawn(t, id, label, active, color, hover, |glyph| glyph, name)
}

fn icon_button_drawn(t: &Theme, id: impl Into<ElementId>, label: impl Into<SharedString>, active: bool, color: Hsla, hover: Hsla, shape: impl FnOnce(Svg) -> Svg, name: &str) -> Stateful<Div> {
    let id = id.into();
    let (on, ticker) = toggle_ease(id.clone(), active);
    let ink = ink(id.clone());
    let fill = t.glass_on();
    icon_button_shell(t, id.clone(), label)
        .child(ticker)
        .when(on > 0.0, |d| d.bg(gpui::Rgba { a: fill.a * on, ..fill }))
        .child(hover_track(id.clone()))
        .opacity(ink.opacity(ICON_REST) + (1.0 - ink.opacity(ICON_REST)) * on)
        .paint_scale(ink.press_scale())
        .child(popped(&id, shape(icon(t, name, 20.0, ink.color(color, hover)))))
}

/// a page's refresh, the last of its controls, in its own pill: `arrows-clockwise` turning once per `SPIN_PERIOD`
/// while `busy`, ending on a whole turn; still while motion is reduced. a click while `busy` does nothing.
pub fn refresh_button(t: &Theme, busy: bool, on_click: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static) -> Div {
    use crate::focus::FocusStop;
    use gpui::{Transformation, percentage};
    let spin = ElementId::Name("refresh-spin".into());
    let turning = animation::current(spin.clone()).is_some_and(|m| m.moving);
    if busy && !turning && !motion::reduced() {
        animation::restart(spin.clone(), 0.0, 1.0, animation::Curve::ease(SPIN_PERIOD as f32 / 1000.0, motion::LINEAR));
    }
    let turn = animation::current(spin.clone()).filter(|m| m.moving).map_or(0.0, |m| m.value);
    let button = icon_button_drawn(t, "refresh", crate::i18n::tr("Refresh"), false, t.t1().into(), t.hover_ink(), |glyph| glyph.with_transformation(Transformation::rotate(percentage(turn))), "arrows-clockwise-regular")
        .child(animation::ticker([spin]))
        .stop(t)
        .on_click(move |event, window, cx| {
            if !busy {
                on_click(event, window, cx);
            }
        });
    glass_pill(t).occlude().child(button)
}

/// a small icon button whose glyph (`size` tokens) rests at `ICON_REST` in `rest` and brightens to full `hover`.
pub fn ink_button(t: &Theme, id: impl Into<ElementId>, label: impl Into<SharedString>, name: &str, size: f32, rest: Hsla, hover: Hsla) -> Stateful<Div> {
    let id = id.into();
    let ink = ink(id.clone());
    icon_button_shell(t, id.clone(), label).child(hover_track(id)).opacity(ink.opacity(ICON_REST)).paint_scale(ink.press_scale()).child(icon(t, name, size, ink.color(rest, hover)))
}

/// a player control's `ink_button`, pressed as its `feel` says: in the bar a round `--active-fill` shows while held.
pub fn player_button(t: &Theme, id: impl Into<ElementId>, label: impl Into<SharedString>, name: &str, size: f32, rest: Hsla, hover: Hsla, feel: Feel) -> Stateful<Div> {
    player_button_with(t, id.into(), label.into(), name, size, rest, hover, feel, false)
}

/// a player control whose glyph cross-fades over `--toggle-fade` when `name` changes, such as the volume level.
pub fn player_button_fading(t: &Theme, id: impl Into<ElementId>, label: impl Into<SharedString>, name: &'static str, size: f32, rest: Hsla, hover: Hsla, feel: Feel) -> Stateful<Div> {
    let id = id.into();
    let (fade, ticker) = swap(ElementId::Name(format!("glyph:{id}").into()), &name, motion::TOGGLE_FADE);
    let Some(old) = fade.leaving else { return player_button(t, id, label, name, size, rest, hover, feel) };
    let color = ink(id.clone()).color(rest, hover);
    let layer = |name: &str, opacity: f32| div().absolute().inset_0().flex().items_center().justify_center().opacity(opacity).child(icon(t, name, size, color));
    let glyph = div().relative().flex_none().size(t.s(size)).child(layer(old, 1.0 - fade.shown)).child(layer(name, fade.shown)).child(ticker).into_any_element();
    player_button_around(t, id, label.into(), glyph, size, feel, false)
}

/// a player control that opens a menu on mouse down: its whole press plays at once (`menu_press_track`).
pub fn player_menu_button(t: &Theme, id: impl Into<ElementId>, label: impl Into<SharedString>, name: &str, size: f32, rest: Hsla, hover: Hsla, feel: Feel) -> Stateful<Div> {
    player_button_with(t, id.into(), label.into(), name, size, rest, hover, feel, true)
}

#[allow(clippy::too_many_arguments)]
fn player_button_with(t: &Theme, id: ElementId, label: SharedString, name: &str, size: f32, rest: Hsla, hover: Hsla, feel: Feel, menu: bool) -> Stateful<Div> {
    let glyph = popped(&id, icon(t, name, size, ink(id.clone()).color(rest, hover))).into_any_element();
    player_button_around(t, id, label, glyph, size, feel, menu)
}

fn player_button_around(t: &Theme, id: ElementId, label: SharedString, glyph: AnyElement, size: f32, feel: Feel, menu: bool) -> Stateful<Div> {
    let ink = ink(id.clone());
    let track = if menu { menu_press_track(id.clone(), feel).into_any_element() } else { press_track(id.clone(), feel).into_any_element() };
    icon_button_shell(t, id, label)
        .relative()
        .child(track)
        .child(active_fill(t, ink, feel, (ICON_W, ICON_BUTTON_H), size))
        // on a pressing surface they rest at full strength.
        .opacity(ink.opacity(if feel.phase { 1.0 } else { ICON_REST }))
        .paint_scale(ink.press_scale_in(feel))
        .child(glyph)
}

/// the held fill's size as a share of its full size: from `--active-fill-start` up as the press is held.
fn fill_growth(ink: Ink) -> f32 {
    ACTIVE_FILL_START + (1.0 - ACTIVE_FILL_START) * ink.grown
}

/// `--active-fill` behind a held text control in the bar: a pill `--active-fill-pad` past the text.
pub fn active_pill(t: &Theme, ink: Ink, feel: Feel) -> AnyElement {
    if !feel.fill || ink.fill <= 0.0 {
        return div().into_any_element();
    }
    let (fill, pad) = (t.active_fill(), t.s(ACTIVE_FILL_PAD));
    div()
        .absolute()
        .top(-pad)
        .bottom(-pad)
        .left(-pad * 2.0)
        .right(-pad * 2.0)
        .rounded_full()
        .bg(gpui::Rgba { a: fill.a * ink.fill, ..fill })
        .paint_scale(fill_growth(ink))
        .into_any_element()
}

/// the round `--active-fill` behind a held bar control: grows from `--active-fill-start` while held, fades at its reached size so it hands over without a blink.
pub fn active_fill(t: &Theme, ink: Ink, feel: Feel, box_tokens: (f32, f32), glyph: f32) -> AnyElement {
    let press = ink.fill;
    if !feel.fill || press <= 0.0 {
        return div().into_any_element();
    }
    let d = glyph * ACTIVE_FILL_SIZE * fill_growth(ink);
    let fill = t.active_fill();
    div()
        .absolute()
        .left(t.s((box_tokens.0 - d) / 2.0))
        .top(t.s((box_tokens.1 - d) / 2.0))
        .size(t.s(d))
        .rounded_full()
        .bg(gpui::Rgba { a: fill.a * press, ..fill })
        .into_any_element()
}

/// The margin around an icon drawn to be scaled (`assets::GROW_PREFIX`), as a fraction of its side.
pub const GROW_MARGIN: f32 = 0.1;

/// Play and skip: the glyph grows on hover and shrinks while pressed, as its `feel` says.
pub fn transport_button(t: &Theme, id: impl Into<ElementId>, label: impl Into<SharedString>, name: &str, size: f32, color: Hsla, enabled: bool, feel: Feel) -> Stateful<Div> {
    let id = id.into();
    let ink = ink(id.clone());
    let scale = if enabled { ink.scale_in(feel) } else { 1.0 };
    let glyph = grow_glyph(t, name, size, color, scale, 1.0);
    transport_shell(id, enabled, feel, transport_box(t, &ink, feel, size, enabled).child(glyph)).tooltip(tip(t, label))
}

/// Play and pause crossfade over `--press-swap`; a plain swap under Reduce Motion.
pub fn play_button(t: &Theme, id: impl Into<ElementId>, playing: bool, size: f32, color: Hsla, feel: Feel) -> Stateful<Div> {
    let id = id.into();
    let ink = ink(id.clone());
    let (shown, ticker) = ease_to(ElementId::from((id.clone(), "swap")), playing, motion::PRESS_SWAP, motion::EASE_OUT);
    let shown = if motion::reduced() { if playing { 1.0 } else { 0.0 } } else { shown };
    let scale = ink.scale_in(feel);
    let glyphs = transport_box(t, &ink, feel, size, true)
        .when(shown < 1.0, |d| d.child(grow_glyph(t, "play-fill", size, color, scale, 1.0 - shown)))
        .when(shown > 0.0, |d| d.child(grow_glyph(t, "pause-fill", size, color, scale, shown)));
    transport_shell(id, true, feel, glyphs).child(ticker).tooltip(tip(t, if playing { crate::i18n::tr("Pause") } else { crate::i18n::tr("Play") }))
}

/// a transport button's glyph box, with the active fill behind its glyph.
fn transport_box(t: &Theme, ink: &Ink, feel: Feel, size: f32, enabled: bool) -> Div {
    div().relative().size(t.s(size)).when(enabled, |d| d.child(active_fill(t, *ink, feel, (size, size), size)))
}

/// a transport button: its hover track around the glyph box `inner`.
fn transport_shell(id: ElementId, enabled: bool, feel: Feel, inner: Div) -> Stateful<Div> {
    div()
        .id(id.clone())
        .relative()
        .flex()
        .items_center()
        .justify_center()
        .flex_none()
        .when(enabled, |d| d.child(press_track(id, feel)))
        .child(inner)
}

/// a transport glyph at `scale` of `size`, centred in its box.
fn grow_glyph(t: &Theme, name: &str, size: f32, color: Hsla, scale: f32, opacity: f32) -> Div {
    // one raster at the grown size, scaled on the GPU so the glyph moves by fractions of a pixel; its margin keeps neighbours out.
    let grown = size * HOVER_GROW;
    let side = t.s(grown * (1.0 + 2.0 * GROW_MARGIN));
    let glyph = svg()
        .path(format!("{}{name}.svg", crate::assets::GROW_PREFIX))
        .absolute()
        .size(side)
        .text_color(color.opacity(opacity))
        .with_transformation(gpui::Transformation::scale(gpui::size(scale / HOVER_GROW, scale / HOVER_GROW)));
    // the atlas samples linearly, so the glyph is clipped a pixel inside its scaled raster to avoid a neighbour's edge.
    let inset = side * (1.0 - scale / HOVER_GROW) / 2.0 + px(1.0);
    div()
        .absolute()
        .left((t.s(size) - side) / 2.0 + inset)
        .top((t.s(size) - side) / 2.0 + inset)
        .size(side - inset * 2.0)
        .overflow_hidden()
        .child(glyph.left(-inset).top(-inset))
}

/// `icon_button` with a drawn glyph in place of an icon (the Download switch's progress ring).
pub fn icon_button_with(t: &Theme, id: impl Into<ElementId>, label: impl Into<SharedString>, active: bool, glyph: impl IntoElement) -> Stateful<Div> {
    let id = id.into();
    let (on, ticker) = toggle_ease(id.clone(), active);
    let fill = t.glass_on();
    pressable(icon_button_shell(t, id.clone(), label), id).child(ticker).when(on > 0.0, |d| d.bg(gpui::Rgba { a: fill.a * on, ..fill })).child(glyph)
}

/// The download mark. Filled when downloaded, a filling ring while downloading, gray while
/// waiting. `fetching` is a quarter ring and `queued` a bare ring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownloadGlyph {
    Done,
    Running,
    Paused,
    Queued,
    Fetching,
}

/// `--accent-wheel`: every hue at the accent's saturation and lightness, red at the top, clockwise.
pub fn accent_wheel() -> impl IntoElement {
    const WEDGES: usize = 72;
    gpui::canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let r = bounds.size.width / 2.0;
            let centre = bounds.center();
            let at = |f: f32| {
                let angle = f * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
                centre + point(r * angle.cos(), r * angle.sin())
            };
            for i in 0..WEDGES {
                let (from, to) = (i as f32 / WEDGES as f32, (i as f32 + 1.5) / WEDGES as f32);
                let mut b = gpui::PathBuilder::fill();
                b.move_to(centre);
                b.line_to(at(from));
                b.arc_to(point(r, r), px(0.0), false, true, at(to.min(1.0 + 0.5 / WEDGES as f32)));
                b.close();
                if let Ok(path) = b.build() {
                    let hue = (i as f32 + 0.5) * 360.0 / WEDGES as f32;
                    window.paint_path(path, Hsla::from(gpui::rgb(crate::color::hue_to_hex(hue))));
                }
            }
        },
    )
    .absolute()
    .size_full()
}

/// `size` tokens square; `progress` 0 to 1 fills the ring; `ink` is the arrow's color while not done.
pub fn download_glyph(t: &Theme, glyph: DownloadGlyph, progress: f32, size: f32, ink: Hsla) -> impl IntoElement {
    let (done_fill, done_ink, track, paused) = (t.downloaded(), t.downloaded_ink(), t.download_track(), t.download_paused());
    let arc_color: Hsla = if glyph == DownloadGlyph::Paused { paused.into() } else { done_fill };
    let arrow_color: Hsla = match glyph {
        DownloadGlyph::Done => done_ink,
        DownloadGlyph::Queued => paused.into(),
        _ => ink,
    };
    let arc = match glyph {
        DownloadGlyph::Fetching => 0.25,
        DownloadGlyph::Done | DownloadGlyph::Queued => 0.0,
        _ => progress.clamp(0.04, 1.0),
    };
    gpui::canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            // The glyph's 24-unit design grid, scaled to the bounds.
            let u = bounds.size.width / 24.0;
            let at = |x: f32, y: f32| bounds.origin + point(u * x, u * y);
            let ring = |r: f32, stroke: Option<f32>, sweep_to: f32| {
                let mut b = match stroke {
                    Some(w) => gpui::PathBuilder::stroke(u * w),
                    None => gpui::PathBuilder::fill(),
                };
                let angle = |f: f32| (f * std::f32::consts::TAU) - std::f32::consts::FRAC_PI_2;
                let p = |f: f32| at(12.0 + r * angle(f).cos(), 12.0 + r * angle(f).sin());
                b.move_to(p(0.0));
                // At most two halves, so each arc is unambiguous.
                let mid = sweep_to.min(0.5);
                b.arc_to(point(u * r, u * r), px(0.0), false, true, p(mid));
                if sweep_to > 0.5 {
                    b.arc_to(point(u * r, u * r), px(0.0), false, true, p(sweep_to.min(0.9999)));
                }
                b.build().ok()
            };
            if glyph == DownloadGlyph::Done {
                if let Some(path) = ring(11.0, None, 0.9999) {
                    window.paint_path(path, done_fill);
                }
            } else {
                if let Some(path) = ring(10.0, Some(2.0), 0.9999) {
                    window.paint_path(path, track);
                }
                if arc > 0.0
                    && let Some(path) = ring(10.0, Some(2.0), arc)
                {
                    window.paint_path(path, arc_color);
                }
            }
            let w = if glyph == DownloadGlyph::Done { 2.4 } else { 2.0 };
            let mut arrow = gpui::PathBuilder::stroke(u * w);
            arrow.move_to(at(12.0, 6.5));
            arrow.line_to(at(12.0, 17.0));
            arrow.move_to(at(7.5, 12.5));
            arrow.line_to(at(12.0, 17.0));
            arrow.line_to(at(16.5, 12.5));
            if let Ok(path) = arrow.build() {
                window.paint_path(path, arrow_color);
            }
        },
    )
    .flex_none()
    .size(t.s(size))
}

fn icon_button_shell(t: &Theme, id: ElementId, label: impl Into<SharedString>) -> Stateful<Div> {
    div()
        .id(id)
        .tooltip(tip(t, label))
        .flex()
        .items_center()
        .justify_center()
        .flex_none()
        .w(t.s(ICON_W))
        .h(t.s(ICON_BUTTON_H))
        .rounded_full()
}

/// The small "E" after a title the server flags explicit, sized from the text it follows.
pub fn explicit_mark(t: &Theme, text: f32) -> gpui::Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .ml(t.s(text * 0.27))
        .size(t.s(text * 0.85))
        .rounded(t.s(3.0))
        .bg(gpui::rgba(crate::theme::EXPLICIT_FILL))
        .text_color(gpui::rgba(crate::theme::EXPLICIT_INK))
        .text_size(t.s(text * 0.68))
        .font_weight(gpui::FontWeight::BOLD)
        .line_height(gpui::relative(1.0))
        .child("E")
}

/// a one-line title, followed by its explicit mark; `title` should truncate itself (and take a [`FullTextTip`]).
pub fn titled(t: &Theme, title: impl IntoElement + Styled, explicit: bool, text: f32) -> gpui::Div {
    div().flex().flex_row().items_center().min_w(gpui::px(0.0)).child(title.min_w(gpui::px(0.0))).when(explicit, |d| d.child(explicit_mark(t, text)))
}

/// The pointing hand cursor, for text links only, like artist and album names or a sortable
/// column heading. Buttons, rows, cards and other controls keep the arrow.
pub trait LinkCursor: Styled + Sized {
    fn link_cursor(self) -> Self {
        self.cursor(gpui::CursorStyle::PointingHand)
    }
}

impl<E: Styled> LinkCursor for E {}

/// Dims a tile, card or other large link while pressed. Rows and small controls use their own hover.
pub fn press_dim(el: Stateful<Div>) -> Stateful<Div> {
    el.active(|s| s.opacity(PRESS_DIM))
}

/// The usual hover for a button or a segment that can't be selected.
pub fn hover_pill(t: &Theme) -> impl IntoElement {
    hover_fill(HoverShape::Pill, t.hover(), t.press())
}

/// A small control's hover. There's no fill, the ink brightens on hover and dims while pressed,
/// and play and skip grow instead. Read it while building the control with the same key as its
/// `hover_track` child. It lags the pointer by a frame.
#[derive(Clone, Copy, Default)]
pub struct Ink {
    pub amount: f32,
    pub pressed: bool,
    /// Press progress from 0 to 1, over `HOVER_PRESS` going down and `HOVER_IN` coming back up.
    pub press: f32,
    /// the scale of the four-step press (`PressPhase`).
    pub phase: f32,
    /// how far a held control's fill has grown, 0 to 1 over `ACTIVE_FILL_GROW`; it keeps its size once let go.
    pub grown: f32,
    /// the held fill's strength: in over `ACTIVE_FILL_GROW`, out over `ACTIVE_FILL_FADE`.
    pub fill: f32,
}

impl Ink {
    /// The ink between its resting color and the hover color.
    pub fn color(self, rest: impl Into<Hsla>, hover: impl Into<Hsla>) -> Hsla {
        let (a, b) = (rest.into(), hover.into());
        let t = self.amount;
        let (ra, ba) = (a.to_rgb(), b.to_rgb());
        gpui::Rgba { r: ra.r + (ba.r - ra.r) * t, g: ra.g + (ba.g - ra.g) * t, b: ra.b + (ba.b - ra.b) * t, a: ra.a + (ba.a - ra.a) * t }.into()
    }
    /// The control's opacity, from `rest` up to 1, dimmed while pressed.
    pub fn opacity(self, rest: f32) -> f32 {
        if self.pressed { PRESS_DIM } else { rest + (1.0 - rest) * self.amount }
    }
    /// every other button: `--press-shrink` while held, easing there over `--hover-press` and back over `--hover-in`.
    pub fn press_scale(self) -> f32 {
        self.press_scale_in(Feel::PLAIN)
    }
    /// Play and skip: grown on hover, shrunk while pressed; with the four-step press there's no hover grow.
    pub fn scale_in(self, feel: Feel) -> f32 {
        if feel.phases() {
            return self.phase;
        }
        let hover = 1.0 + (motion::hover_grow() - 1.0) * self.amount;
        hover + (PRESS_SHRINK - hover) * self.press
    }
    /// `press_scale` where the control sits.
    pub fn press_scale_in(self, feel: Feel) -> f32 {
        if feel.phases() { self.phase } else { 1.0 + (PRESS_SHRINK - 1.0) * self.press }
    }
}

/// where a player control sits: the bar shows `--active-fill` while held; the bar, the side player and full screen
/// press in `PressPhase` steps.
#[derive(Clone, Copy)]
pub struct Feel {
    pub fill: bool,
    pub phase: bool,
}

impl Feel {
    pub const BAR: Feel = Feel { fill: true, phase: true };
    pub const PLAYER: Feel = Feel { fill: false, phase: true };
    pub const PLAIN: Feel = Feel { fill: false, phase: false };

    /// Reduce Motion keeps the plain `--press-shrink`.
    fn phases(self) -> bool {
        self.phase && !motion::reduced()
    }
}

/// a button drawn as a whole shrinks to `--press-shrink` while held, without moving anything around it.
pub fn pressable(el: Stateful<Div>, key: impl Into<ElementId>) -> Stateful<Div> {
    pressable_in(el, key, Feel::PLAIN)
}

type MenuOpen = dyn Fn(&mut crate::shell::Shell, gpui::Point<gpui::Pixels>, &mut gpui::Context<crate::shell::Shell>);

/// a menu button (`player_menu_button`): its menu opens on mouse down, or on Enter and Space.
pub fn menu_on_press(el: Stateful<Div>, cx: &mut gpui::Context<crate::shell::Shell>, open: impl Fn(&mut crate::shell::Shell, gpui::Point<gpui::Pixels>, &mut gpui::Context<crate::shell::Shell>) + 'static) -> Stateful<Div> {
    let open: std::rc::Rc<MenuOpen> = std::rc::Rc::new(open);
    let keys = open.clone();
    el.on_mouse_down(gpui::MouseButton::Left, cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
        cx.stop_propagation();
        open(this, event.position, cx);
    }))
    .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
        if event.is_keyboard() {
            keys(this, event.position(), cx);
        }
    }))
}

/// `pressable` for a player control, pressed as its `feel` says.
pub fn pressable_in(el: Stateful<Div>, key: impl Into<ElementId>, feel: Feel) -> Stateful<Div> {
    let key = key.into();
    let scale = ink(key.clone()).press_scale_in(feel);
    el.child(press_track(key, feel)).paint_scale(scale)
}

/// how far a toggle's on look shows (0 to 1), easing over `--toggle-fade`; add the returned element as a child.
pub fn toggle_ease(key: impl Into<ElementId>, on: bool) -> (f32, AnyElement) {
    ease_to(key, on, motion::TOGGLE_FADE, motion::HOVER_EASE)
}

/// [`toggle_ease`] over `secs` on its own `curve` (a CSS cubic-bezier, which may overshoot).
pub fn ease_to(key: impl Into<ElementId>, on: bool, secs: f32, curve: [f32; 4]) -> (f32, AnyElement) {
    let key = key.into();
    let (amount, running) = eased(key.clone(), on, secs, curve);
    (amount, if running { ease_ticker(key) } else { animation::idle() })
}

/// [`ease_to`]'s value and whether it's still moving, for a caller that keeps its own ticker; a change of `on`
/// mid-way turns back from where it is.
pub fn eased(key: impl Into<ElementId>, on: bool, secs: f32, curve: [f32; 4]) -> (f32, bool) {
    let motion = animation::animate(key, if on { 1.0 } else { 0.0 }, animation::Curve::ease(secs, curve));
    (motion.value, motion.moving)
}

/// asks for frames while `key`'s [`eased`] value moves; add it as a child of the view that reads the value.
pub fn ease_ticker(key: impl Into<ElementId>) -> AnyElement {
    animation::ticker([key])
}

thread_local! {
    static SWAPS: std::cell::RefCell<std::collections::HashMap<ElementId, SwapState>> = Default::default();
}

struct SwapState {
    current: Box<dyn std::any::Any>,
    leaving: Option<Box<dyn std::any::Any>>,
}

/// what a cross-fade by key shows: the incoming content's share (0 to 1, eased) and the one it replaces while that fades.
pub struct Swapping<K> {
    pub shown: f32,
    pub leaving: Option<K>,
}

/// cross-fades content keyed by `current` over `secs`: when the key changes, the old one fades out over the new one,
/// which fades in. only the latest old key is kept, so quick changes never pile up. add the element as a child of the
/// view that draws both; it asks for frames while the fade runs.
pub fn swap<K: Clone + PartialEq + 'static>(id: impl Into<ElementId>, current: &K, secs: f32) -> (Swapping<K>, AnyElement) {
    let id = id.into();
    let curve = animation::Curve::ease(secs, motion::EASE);
    let (shown, leaving) = SWAPS.with(|m| {
        let mut m = m.borrow_mut();
        let state = m.entry(id.clone()).or_insert_with(|| {
            animation::restart(id.clone(), 0.0, 1.0, curve);
            SwapState { current: Box::new(current.clone()), leaving: None }
        });
        if state.current.downcast_ref::<K>() != Some(current) {
            let old = std::mem::replace(&mut state.current, Box::new(current.clone()));
            state.leaving = (!motion::reduced()).then_some(old);
            animation::restart(id.clone(), 0.0, 1.0, curve);
        }
        let shown = animation::current(id.clone()).unwrap_or(animation::Motion { value: 1.0, moving: false });
        if !shown.moving {
            state.leaving = None;
        }
        (shown.value, state.leaving.as_ref().and_then(|k| k.downcast_ref::<K>().cloned()))
    });
    let ticker = if leaving.is_some() { animation::ticker([id]) } else { animation::idle() };
    (Swapping { shown, leaving }, ticker)
}

/// a layer of a [`swap`] filling its `relative` parent: the incoming one fades in rising `rise`, the leaving one fades out.
pub fn swap_layer(content: impl IntoElement, shown: f32, rise: gpui::Pixels, incoming: bool) -> Div {
    let (opacity, lift) = if incoming { (shown, rise * (1.0 - shown)) } else { (1.0 - shown, px(0.0)) };
    // the rise is painted, not laid out, so it moves by fractions of a pixel
    div().absolute().inset_0().when(lift != px(0.0), |d| d.paint_offset(gpui::point(px(0.0), lift))).when(opacity < 1.0, |d| d.opacity(opacity)).child(content)
}

fn pop_key(id: &ElementId) -> ElementId {
    ElementId::NamedChild(std::sync::Arc::new(id.clone()), "pop".into())
}

/// pops the glyph of icon button `id`, as a heart does when liking; a second pop restarts it.
pub fn pop(id: impl Into<ElementId>) {
    if !motion::reduced() {
        animation::restart(pop_key(&id.into()), 0.0, 1.0, animation::Curve::ease(motion::LIKE_POP, motion::EASE));
    }
}

/// the glyph's scale `LIKE_POP` seconds through a pop: up to `LIKE_POP_SCALE` at `LIKE_POP_PEAK` and back, on `--ease`.
fn pop_scale(id: &ElementId) -> Option<f32> {
    let key = pop_key(id);
    let shown = animation::current(key.clone())?;
    if !shown.moving {
        animation::forget(key);
        return None;
    }
    let e = shown.value;
    let k = if e < motion::LIKE_POP_PEAK { e / motion::LIKE_POP_PEAK } else { (1.0 - e) / (1.0 - motion::LIKE_POP_PEAK) };
    Some(1.0 + (motion::LIKE_POP_SCALE - 1.0) * k)
}

/// `glyph`, scaled while a [`pop`] of `id` runs.
fn popped(id: &ElementId, glyph: Svg) -> AnyElement {
    let Some(scale) = pop_scale(id) else { return glyph.into_any_element() };
    div().flex_none().relative().paint_scale(scale).child(animation::ticker([pop_key(id)])).child(glyph).into_any_element()
}

fn card_key(group: &str) -> ElementId {
    ElementId::Name(SharedString::from(format!("{group}:hover")))
}

/// how far a card's hover look shows (0 to 1), easing both ways over `secs` once `card_track(group)` sees the pointer; add the returned element as a child.
pub fn card_hover(group: &str, secs: f32) -> (f32, AnyElement) {
    let key = card_key(group);
    let hovered = INKS.with(|m| m.borrow().get(&key).is_some_and(|f| f.hovered));
    ease_to(key, hovered, secs, motion::EASE)
}

/// tracks the pointer over a card for `card_hover`; add it last to the card, which must be `relative`.
pub fn card_track(group: &str) -> impl IntoElement {
    hover_track(card_key(group))
}

/// one segment of a glass pill of choices: `--glass-on` behind it while on; draw the label in the ink returned.
pub fn segment(t: &Theme, id: impl Into<ElementId>, on: bool, rest: impl Into<Hsla>) -> (Stateful<Div>, Hsla) {
    let id = id.into();
    let ink = ink(id.clone());
    let (shown, ticker) = toggle_ease(id.clone(), on);
    let fill = t.glass_on();
    let color = mix_hsla(ink.color(rest, t.hover_ink()), t.t_em(), shown);
    let el = pressable(div().id(id.clone()), id).child(ticker).rounded_full().when(shown > 0.0, |d| d.bg(gpui::Rgba { a: fill.a * shown, ..fill })).when(!on, |d| d.opacity(ink.opacity(1.0)));
    (el, color)
}

/// `a` mixed toward `b` by `t`.
pub fn mix_hsla(a: impl Into<Hsla>, b: impl Into<Hsla>, t: f32) -> Hsla {
    Ink { amount: t, ..Default::default() }.color(a, b)
}

thread_local! {
    static INKS: std::cell::RefCell<std::collections::HashMap<ElementId, HoverFade>> = Default::default();
}

/// The hover state `hover_track(key)` last painted.
pub fn ink(key: impl Into<ElementId>) -> Ink {
    let key = key.into();
    let now = std::time::Instant::now();
    INKS.with(|m| {
        m.borrow().get(&key).map(|f| Ink { amount: f.amount(now), pressed: f.pressed.get(), press: f.press(now), phase: f.phase_moving(now).0, grown: f.grown(now), fill: f.fill(now) }).unwrap_or_default()
    })
}

/// Tracks the pointer over a small control for `ink(key)`. Add it as the control's first child.
/// It redraws the view while the ink fades and on enter, leave and press.
pub fn hover_track(key: impl Into<ElementId>) -> impl IntoElement {
    track(key.into(), 0.0, false)
}

/// `hover_track` for a player control.
pub fn press_track(key: impl Into<ElementId>, _feel: Feel) -> impl IntoElement {
    hover_track(key)
}

/// `press_track` for a control whose menu opens on mouse down, so it never sees the release: the press plays
/// squash, over and rest at once, with the held fill through it where `feel` has one.
pub fn menu_press_track(key: impl Into<ElementId>, feel: Feel) -> impl IntoElement {
    track(key.into(), 0.0, feel.phases())
}

/// tracks the pointer over an area that reveals controls; add it last so its hitbox isn't hidden by them. they stay `--reveal-grace` after leaving.
pub fn reveal_track(key: impl Into<ElementId>) -> impl IntoElement {
    track(key.into(), motion::REVEAL_GRACE, false)
}

thread_local! {
    /// When the pointer left each `reveal_track` still within its grace period.
    static LEFT: std::cell::RefCell<std::collections::HashMap<ElementId, std::time::Instant>> = Default::default();
}

fn track(key: ElementId, grace: f32, burst: bool) -> impl IntoElement {
    gpui::canvas(
        |bounds, window, _| window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal),
        move |_, hitbox, window, _| {
            let now = std::time::Instant::now();
            // The hit test keeps the last position once the pointer leaves the window, so check the window too.
            let mut hovered = window.is_window_hovered() && hitbox.is_hovered(window);
            if grace > 0.0 {
                let was = INKS.with(|m| m.borrow().get(&key).is_some_and(|f| f.hovered));
                hovered = LEFT.with(|l| {
                    let mut l = l.borrow_mut();
                    if hovered || !was {
                        l.remove(&key);
                        return hovered;
                    }
                    let left = *l.entry(key.clone()).or_insert(now);
                    let held = now.saturating_duration_since(left).as_secs_f32() < grace;
                    if held {
                        crate::animation::request_frame(window);
                    } else {
                        l.remove(&key);
                    }
                    held
                });
            }
            let (amount, pressed, press, moving, fade) = INKS.with(|m| {
                let mut m = m.borrow_mut();
                let state = m.entry(key.clone()).or_insert_with(|| HoverFade { hovered: false, since: now, from: 0.0, pressed: Default::default(), press: Default::default() });
                if state.hovered != hovered {
                    let from = state.amount(now);
                    *state = HoverFade { hovered, since: now, from, pressed: state.pressed.clone(), press: state.press.clone() };
                }
                state.follow_press(now);
                (state.amount(now), state.pressed.clone(), state.press(now), state.press_moving(now), state.press.clone())
            });
            if !hovered && pressed.get() {
                pressed.set(false);
            }
            let pressing = moving || if pressed.get() { press < 1.0 } else { press > 0.0 };
            if (hovered && amount < 1.0) || (!hovered && amount > 0.0) || pressing {
                crate::animation::request_frame(window);
            }
            if hover_log() {
                eprintln!("navia: hover {key:?} {:.1} ms amount {amount:.4} press {press:.4}", crate::testing::since_start_ms());
            }
            track_pointer(window, &hitbox, hovered, pressed, burst.then_some(fade));
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

thread_local! {
    static POINTER_BUTTON: std::cell::Cell<Option<gpui::MouseButton>> = const { std::cell::Cell::new(None) };
}

/// hover only follows pointer moves, so a row stays lit after the pointer leaves the window; this moves the
/// pointer away from the window when it leaves. add it once to each window's root.
pub fn hover_clears_on_leave() -> AnyElement {
    gpui::canvas(
        |_, _, _| {},
        |_, _, window, _| {
            window.on_mouse_event(|event: &gpui::MouseMoveEvent, phase, _, _| {
                if phase == gpui::DispatchPhase::Capture {
                    POINTER_BUTTON.set(event.pressed_button);
                }
            });
            window.on_mouse_event(|_: &gpui::MouseExitEvent, phase, window, _| {
                // a drag carries on outside the window
                if phase == gpui::DispatchPhase::Capture && POINTER_BUTTON.get().is_none() {
                    // not from inside this dispatch, which holds the frame's listeners
                    window.on_next_frame(|window, cx| {
                        if !window.is_window_hovered() {
                            let away = window.viewport_size();
                            let event = gpui::MouseMoveEvent { position: point(-away.width, -away.height), pressed_button: None, modifiers: window.modifiers() };
                            window.dispatch_event(gpui::PlatformInput::MouseMove(event), cx);
                        }
                    });
                }
            });
        },
    )
    .absolute()
    .size_0()
    .into_any_element()
}

/// Redraws the current view when the pointer crosses `hitbox` or presses and releases on it.
/// with `burst`, a press plays its release at once instead of holding.
fn track_pointer(window: &mut gpui::Window, hitbox: &gpui::Hitbox, hovered: bool, pressed: std::rc::Rc<std::cell::Cell<bool>>, burst: Option<std::rc::Rc<std::cell::Cell<PressFade>>>) {
    let view = window.current_view();
    let moved = hitbox.clone();
    window.on_mouse_event(move |_: &gpui::MouseMoveEvent, phase, window, cx| {
        if phase == gpui::DispatchPhase::Capture && moved.is_hovered(window) != hovered {
            cx.notify(view);
        }
    });
    let down = pressed.clone();
    let downbox = hitbox.clone();
    window.on_mouse_event(move |event: &gpui::MouseDownEvent, phase, window, cx| {
        if phase == gpui::DispatchPhase::Capture && event.button == gpui::MouseButton::Left && downbox.is_hovered(window) {
            match &burst {
                Some(fade) => fade.set(PressFade { since: Some(std::time::Instant::now()), scale_from: 1.0, grown: 1.0, burst: true, ..Default::default() }),
                None => down.set(true),
            }
            cx.notify(view);
        }
    });
    window.on_mouse_event(move |_: &gpui::MouseUpEvent, phase, _, cx| {
        if phase == gpui::DispatchPhase::Capture && pressed.get() {
            pressed.set(false);
            cx.notify(view);
        }
    });
}

/// Where a button's hover fill sits inside its bounds.
#[derive(Clone, Copy)]
pub enum HoverShape {
    /// The whole control with fully rounded ends, like a pill segment or round button.
    Pill,
    /// A circle of this diameter centered on the control, for row icons wider than their glyph.
    Circle(gpui::Pixels),
    /// The whole control with this corner radius, like a small square button.
    Rounded(gpui::Pixels),
}

#[derive(Clone)]
struct HoverFade {
    hovered: bool,
    since: std::time::Instant,
    from: f32,
    pressed: std::rc::Rc<std::cell::Cell<bool>>,
    /// the press's own fade, restarted when the button goes down or comes up.
    press: std::rc::Rc<std::cell::Cell<PressFade>>,
}

/// a press going down or coming up: since when, and where its ease, scale and fill growth were then.
#[derive(Clone, Copy, Default)]
struct PressFade {
    down: bool,
    since: Option<std::time::Instant>,
    from: f32,
    scale_from: f32,
    grown: f32,
    fill_from: f32,
    /// down and up in one go (`menu_press_track`): the fill holds through the whole release.
    burst: bool,
}

/// from `from` toward `phase`'s scale, `dt` seconds into the step.
fn phase_step(from: f32, phase: PressPhase, dt: f32) -> f32 {
    let (secs, curve) = phase.motion();
    from + (phase.scale() - from) * motion::cubic_bezier(curve, motion::progress(dt, secs))
}

const RELEASE: [PressPhase; 3] = [PressPhase::Squash, PressPhase::Over, PressPhase::Rest];

/// one transition from `from` towards `to` over `secs`, `dt` seconds in; a change partway restarts from where it is, as CSS does.
fn hover_step(from: f32, to: f32, secs: f32, dt: f32) -> f32 {
    from + (to - from) * motion::hover_ease(motion::progress(dt, secs))
}

impl HoverFade {
    /// A quick fade in and a gentler fade out.
    fn amount(&self, now: std::time::Instant) -> f32 {
        let dt = now.saturating_duration_since(self.since).as_secs_f32();
        if self.hovered { hover_step(self.from, 1.0, motion::HOVER_IN, dt) } else { hover_step(self.from, 0.0, motion::HOVER_OUT, dt) }
    }

    /// The press, going down over the press time and back up over the fade-in time.
    fn press(&self, now: std::time::Instant) -> f32 {
        let p = self.press.get();
        let Some(since) = p.since else { return if p.down { 1.0 } else { 0.0 } };
        let dt = now.saturating_duration_since(since).as_secs_f32();
        if p.down { hover_step(p.from, 1.0, motion::HOVER_PRESS, dt) } else { hover_step(p.from, 0.0, motion::HOVER_IN, dt) }
    }

    /// the `PressPhase` scale, and whether it's still moving: `Held` while down; let go, each step after the last.
    fn phase_moving(&self, now: std::time::Instant) -> (f32, bool) {
        let p = self.press.get();
        let Some(since) = p.since else { return (if p.down { PressPhase::Held.scale() } else { 1.0 }, false) };
        let mut dt = now.saturating_duration_since(since).as_secs_f32();
        if p.down {
            return (phase_step(p.scale_from, PressPhase::Held, dt), dt < PressPhase::Held.motion().0);
        }
        let mut from = p.scale_from;
        for phase in RELEASE {
            let secs = phase.motion().0;
            if dt < secs {
                return (phase_step(from, phase, dt), true);
            }
            dt -= secs;
            from = phase.scale();
        }
        (1.0, false)
    }

    /// the held fill's growth: up over `ACTIVE_FILL_GROW` while held, kept as it was once let go.
    fn grown(&self, now: std::time::Instant) -> f32 {
        let p = self.press.get();
        match (p.down, p.since) {
            (true, Some(since)) => motion::cubic_bezier(motion::EASE_OUT, motion::progress(now.saturating_duration_since(since).as_secs_f32(), motion::ACTIVE_FILL_GROW)),
            _ => p.grown,
        }
    }

    /// the held fill's strength: up over `ACTIVE_FILL_GROW` while held, down over `ACTIVE_FILL_FADE` once let go.
    fn fill(&self, now: std::time::Instant) -> f32 {
        let p = self.press.get();
        let Some(since) = p.since else { return if p.down { 1.0 } else { 0.0 } };
        let dt = now.saturating_duration_since(since).as_secs_f32();
        if p.burst {
            let held: f32 = RELEASE.iter().map(|phase| phase.motion().0).sum();
            return if dt < held {
                motion::cubic_bezier(motion::EASE_OUT, motion::progress(dt, motion::PRESS_IN))
            } else {
                1.0 - motion::cubic_bezier(motion::EASE_OUT, motion::progress(dt - held, motion::ACTIVE_FILL_FADE))
            };
        }
        let (to, secs) = if p.down { (1.0, motion::ACTIVE_FILL_GROW) } else { (0.0, motion::ACTIVE_FILL_FADE) };
        p.fill_from + (to - p.fill_from) * motion::cubic_bezier(motion::EASE_OUT, motion::progress(dt, secs))
    }

    /// the press or the fill still needs frames.
    fn press_moving(&self, now: std::time::Instant) -> bool {
        let p = self.press.get();
        let fill = self.fill(now);
        let filling = if p.down { fill < 1.0 || self.grown(now) < 1.0 } else { fill > 0.0 };
        p.since.is_some() && (self.phase_moving(now).1 || filling)
    }

    /// Starts the press's fade when the button has gone down or come up since the last frame.
    fn follow_press(&self, now: std::time::Instant) {
        let down = self.pressed.get();
        if down != self.press.get().down {
            let grown = if down { 0.0 } else { self.grown(now) };
            self.press.set(PressFade { down, since: Some(now), from: self.press(now), scale_from: self.phase_moving(now).0, grown, fill_from: self.fill(now), burst: false });
        }
    }
}

/// With `NAVIA_HOVER_LOG=1`, logs every frame of a small control's hover and press.
fn hover_log() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("NAVIA_HOVER_LOG").is_some())
}

/// The hover fill for a button. Add it as the first child so the glyph paints over it, and only
/// while the button is enabled and not selected. It fades on its own and shows `press` while held.
/// The fade lives in element state, so the button needs an id.
pub fn hover_fill(shape: HoverShape, hover: gpui::Rgba, press: gpui::Rgba) -> impl IntoElement {
    gpui::canvas(
        |bounds, window, _| window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal),
        move |bounds, hitbox, window, _| {
            let hovered = hitbox.is_hovered(window);
            let now = std::time::Instant::now();
            let (amount, pressed) = window.with_global_id("hover-fill".into(), |id, window| {
                window.with_element_state::<HoverFade, _>(id, |state, _| {
                    let mut state = state.unwrap_or(HoverFade { hovered: false, since: now, from: 0.0, pressed: Default::default(), press: Default::default() });
                    if state.hovered != hovered {
                        let from = state.amount(now);
                        state = HoverFade { hovered, since: now, from, pressed: state.pressed.clone(), press: state.press.clone() };
                    }
                    let amount = state.amount(now);
                    ((amount, state.pressed.clone()), state)
                })
            });
            if !hovered {
                pressed.set(false);
            }
            if amount > 0.0 {
                let color = if pressed.get() { press } else { gpui::Rgba { a: hover.a * amount, ..hover } };
                let (rect, radius) = match shape {
                    HoverShape::Pill => (bounds, bounds.size.height.min(bounds.size.width) / 2.0),
                    HoverShape::Circle(d) => (gpui::Bounds::centered_at(bounds.center(), gpui::size(d, d)), d / 2.0),
                    HoverShape::Rounded(r) => (bounds, r),
                };
                window.paint_quad(gpui::fill(rect, color).corner_radii(radius));
            }
            if (hovered && amount < 1.0) || (!hovered && amount > 0.0) {
                crate::animation::request_frame(window);
            }
            track_pointer(window, &hitbox, hovered, pressed, None);
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// Art fading in over its placeholder, or just the art once fully opaque.
pub fn fade_in(art: gpui::Img, placeholder: impl IntoElement, opacity: f32) -> AnyElement {
    if opacity >= 1.0 {
        return art.into_any_element();
    }
    div().relative().flex_none().child(placeholder).child(art.absolute().left(px(0.0)).top(px(0.0)).opacity(opacity)).into_any_element()
}

pub const STATION_NO_STAR: &str = n_("Stations can't be favorited");
pub const STATION_NO_SKIP: &str = n_("Stations can't be skipped");
pub const STATION_NO_MODES: &str = n_("Stations can't be shuffled or repeated");

/// A control that can't be used right now, dimmed and inert. Don't set a hover style before
/// this, since GPUI panics if hover is set twice.
pub fn locked(el: Stateful<Div>) -> Stateful<Div> {
    el.opacity(0.35)
}

pub fn clock(seconds: u32) -> SharedString {
    SharedString::from(format!("{}:{:02}", seconds / 60, seconds % 60))
}

/// The mark and wordmark side by side, `mark` tall. Art from tools/brand_icons.py.
pub fn brand_lockup(t: &Theme, mark: f32, color: impl Into<Hsla>) -> Div {
    let color = color.into();
    let word = mark * BRAND_WORD_RATIO;
    div()
        .flex()
        .flex_row()
        .items_center()
        .flex_none()
        .gap(t.s(mark * BRAND_GAP_RATIO))
        .child(svg().path(icon_path("nv-brand-mark.svg")).flex_none().size(t.s(mark)).text_color(color))
        .child(svg().path(icon_path("nv-brand-word.svg")).flex_none().h(t.s(word)).w(t.s(word * crate::assets::BRAND_WORD_ASPECT)).text_color(color))
}

/// A glass card on its own backdrop, as on the sign-in form.
pub fn glass_card(t: &Theme) -> Div {
    let radius = t.s(R_LG);
    glass_layers(div().relative().rounded(radius), t, radius, FLOAT_BLUR, FLOAT_SATURATION, t.float_glass(), t.float_fill(), edge_and(shadow_float(t)))
}

/// A one-time question inside a panel, like the queue's Deezer card. A title, one fact beside its
/// glyph, then the answers side by side.
pub fn prompt_card(t: &Theme, title: SharedString, glyph: &str, fact: SharedString, answers: Vec<AnyElement>) -> Div {
    glass_card(t).p(t.s(PROMPT_CARD_PAD)).child(
        div()
            .relative()
            .flex()
            .flex_col()
            .gap(t.s(PROMPT_CARD_GAP))
            .child(div().text_size(t.text(TEXT_MD)).font_weight(FontWeight::SEMIBOLD).text_color(t.t_em()).line_height(relative(1.3)).child(title))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(t.s(PROMPT_CARD_GAP))
                    .text_size(t.text(TEXT_SM))
                    .text_color(t.t2())
                    .line_height(relative(1.35))
                    .child(icon(t, glyph, 16.0, t.t2()))
                    .child(div().flex_1().min_w(px(0.0)).child(fact)),
            )
            .child(div().flex().flex_row().gap(t.s(DIALOG_ACTIONS_GAP)).children(answers)),
    )
}

/// A labeled text field, with the label above `field` and an optional message below it.
pub fn text_field_block(t: &Theme, label: impl Into<SharedString>, field: impl IntoElement, message: Option<AnyElement>) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(t.s(FIELD_MESSAGE_GAP))
        .child(div().flex().flex_col().gap(t.s(FIELD_LABEL_GAP)).child(div().text_size(t.text(TEXT_SM)).text_color(t.t2()).font_weight(FontWeight::MEDIUM).child(label.into())).child(field))
        .children(message)
}

/// The glass pill of a labeled text field, with the focus ring or the invalid ring. Add the text
/// line, then an optional `field_action` at its right end.
pub fn text_field_box(t: &Theme, id: impl Into<ElementId>, focused: bool, invalid: bool) -> Stateful<Div> {
    let edge = if invalid { crate::ui_controls::with_invalid_ring(glass_edge()) } else if focused { crate::ui_controls::with_ring(glass_edge(), t.accent_hsla()) } else { glass_edge() };
    div().id(id.into()).relative().flex().flex_row().items_center().h(t.s(CONTROL_H)).pl(t.s(FIELD_PAD)).pr(t.s(FIELD_INSET)).rounded_full().bg(t.glass_fill()).shadow(edge).cursor_text()
}

/// An action at a text field's right end, like Show password. Quiet ink that brightens on hover.
pub fn field_action(t: &Theme, id: impl Into<ElementId>, enabled: bool) -> Stateful<Div> {
    let id = id.into();
    let ink = ink(id.clone());
    div()
        .id(id.clone())
        .flex()
        .flex_row()
        .items_center()
        .justify_center()
        .flex_none()
        .min_w(t.s(ICON_W))
        .h(t.s(CONTROL_H - FIELD_INSET * 2.0))
        .px(t.s(FIELD_PAD / 2.0))
        .rounded_full()
        .text_size(t.text(TEXT_SM))
        .font_weight(FontWeight::MEDIUM)
        .cursor_default()
        .when(enabled, |d| d.child(hover_track(id)).text_color(ink.color(Hsla::from(t.t2()), t.hover_ink())))
        .when(!enabled, |d| d.text_color(t.t2()).opacity(DISABLED_OPACITY))
}

/// The busy glyph, an arc turning once per `SPIN_PERIOD_MS`, still while motion is reduced.
/// `key` gives each spinner its own animation.
pub fn spinner(t: &Theme, key: impl Into<ElementId>, size: f32, color: impl Into<Hsla>) -> AnyElement {
    use gpui::{Animation, AnimationExt, Transformation, percentage};
    let glyph = icon(t, "circle-notch-regular", size, color);
    if motion::reduced() {
        return glyph.into_any_element();
    }
    glyph
        .with_animation(key, Animation::new(std::time::Duration::from_millis(SPIN_PERIOD)).repeat(), |svg, delta| svg.with_transformation(Transformation::rotate(percentage(delta))))
        .into_any_element()
}

/// A dialog's small print in the error color.
pub fn dialog_note_error(t: &Theme, text: impl Into<SharedString>) -> Div {
    dialog_note(t, text).text_color(crate::theme::danger_ink())
}

/// A short code typed digit by digit, like an AirPlay PIN. Shows `len` boxes with the next one
/// ringed while `focused` and all ringed in red when `invalid`. The caller handles the keys.
pub fn dialog_pin(t: &Theme, id: &'static str, value: &str, len: usize, focused: bool, invalid: bool, disabled: bool) -> Stateful<Div> {
    let digits: Vec<char> = value.chars().collect();
    let at = digits.len().min(len.saturating_sub(1));
    div().id(id).flex().flex_row().justify_center().gap(t.s(PIN_GAP)).cursor_text().children((0..len).map(|i| {
        let edge = if invalid {
            crate::ui_controls::with_invalid_ring(glass_edge())
        } else if focused && !disabled && i == at {
            crate::ui_controls::with_ring(glass_edge(), t.accent_hsla())
        } else {
            glass_edge()
        };
        div()
            .flex()
            .items_center()
            .justify_center()
            .w(t.s(PIN_BOX_W))
            .h(t.s(PIN_BOX_H))
            .rounded(t.s(R_MD))
            .bg(t.glass_fill())
            .shadow(edge)
            .text_size(t.text(PIN_TEXT))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(t.t1())
            .when(disabled, |d| d.opacity(DISABLED_OPACITY))
            .child(digits.get(i).map(|c| c.to_string()).unwrap_or_default())
    }))
}

/// A collapsible part of a dialog with a caret and label. The caller adds the content while `open`.
pub fn dialog_disclosure(t: &Theme, id: &'static str, label: impl Into<SharedString>, open: bool) -> Stateful<Div> {
    let go = ink(id);
    div()
        .id(id)
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .gap(t.s(6.0))
        .text_size(t.text(TEXT_SM))
        .text_color(go.color(t.t2(), t.t1()))
        .child(hover_track(id))
        .child(icon(t, if open { "caret-down-bold" } else { "caret-right-bold" }, 12.0, go.color(t.t2(), t.t1())))
        .child(label.into())
}

/// A command to run, in wrapping monospace text on glass, with a `copy` button at its bottom right.
pub fn dialog_code(t: &Theme, code: impl Into<SharedString>, copy: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_col()
        .items_end()
        .gap(t.s(8.0))
        .p(t.s(CODE_PAD))
        .rounded(t.s(R_MD))
        .bg(t.glass_fill())
        .shadow(glass_edge())
        .child(div().w_full().font_family(crate::assets::font_mono()).text_size(t.text(TEXT_XS)).line_height(relative(1.5)).text_color(t.t1()).child(code.into()))
        .child(copy)
}

/// The filter chip, `CHIP_H` tall, glass at rest and filled when on. Add the label and glyphs as
/// children. It's the whole chip unless there's an X ([`filter_chip_remove`]).
pub fn filter_chip(t: &Theme, id: impl Into<ElementId>, on: bool) -> Stateful<Div> {
    let id = id.into();
    let chip = pressable(div().id(id.clone()), id)
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .flex_none()
        .gap(t.s(CHIP_GAP))
        .h(t.s(CHIP_H))
        .px(t.s(CHIP_PAD))
        .rounded_full()
        .text_size(t.text(TEXT_SM))
        .font_weight(FontWeight::MEDIUM)
        .whitespace_nowrap();
    if on {
        chip.bg(t.chip_on()).text_color(t.on_chip())
    } else {
        chip.bg(t.glass_fill()).shadow(glass_edge()).text_color(t.t1()).child(hover_fill(HoverShape::Pill, t.glass_hover(), t.glass_press()))
    }
}

/// A chip's glyph, such as its caret, the More Filters plus or the X.
pub fn chip_glyph(t: &Theme, name: &str, on: bool) -> Svg {
    icon(t, name, CHIP_GLYPH, if on { Hsla::from(t.on_chip()) } else { Hsla::from(t.t1()) })
}

/// An active chip's X, a separate button at the chip's end. Put [`filter_chip`] with
/// `pr(CHIP_GAP)` and this in [`filter_chip_pair`].
pub fn filter_chip_remove(t: &Theme, id: impl Into<ElementId>) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .flex_none()
        .h_full()
        .pl(t.s(CHIP_GAP))
        .pr(t.s(CHIP_PAD - CHIP_GAP))
        .rounded_full()
        .opacity(0.7)
        .hover(|s| s.opacity(1.0))
        .child(chip_glyph(t, "x-bold", true))
}

/// An active chip with its X, the on fill spanning both buttons.
pub fn filter_chip_pair(t: &Theme, main: Stateful<Div>, remove: Stateful<Div>) -> Div {
    div().flex().flex_row().items_center().flex_none().h(t.s(CHIP_H)).rounded_full().bg(t.chip_on()).text_color(t.on_chip()).child(main.pr(t.s(CHIP_GAP))).child(remove)
}

/// The width of `text` in the app's font as a line of it draws. [`fold_chips`] needs it before layout.
pub fn text_width(window: &mut gpui::Window, text: &str, size: gpui::Pixels, weight: FontWeight) -> gpui::Pixels {
    let mut font = window.text_style().font();
    font.weight = weight;
    let run = gpui::TextRun { len: text.len(), font, color: Hsla::default(), background_color: None, underline: None, strikethrough: None };
    window.text_system().shape_line(SharedString::from(text.to_string()), size, &[run], None).width
}

/// How wide [`filter_chip`] draws a chip, with the caret when it opens a popover.
pub fn filter_chip_width(t: &Theme, window: &mut gpui::Window, label: &str, menu: bool) -> gpui::Pixels {
    let glyph = if menu { t.s(CHIP_GAP + CHIP_GLYPH) } else { px(0.0) };
    t.s(CHIP_PAD * 2.0) + text_width(window, label, t.text(TEXT_SM), FontWeight::MEDIUM) + glyph
}

/// How wide [`filter_chip_pair`] draws an active chip, with its name, value and X.
pub fn filter_chip_pair_width(t: &Theme, window: &mut gpui::Window, name: &str, value: &str) -> gpui::Pixels {
    let size = t.text(TEXT_SM);
    let value = text_width(window, value, size, FontWeight::MEDIUM).min(t.s(CHIP_VALUE_MAX_W));
    t.s(CHIP_PAD * 2.0 + CHIP_GAP * 2.0 + CHIP_GLYPH) + text_width(window, name, size, FontWeight::MEDIUM) + value
}

/// An active chip's value, truncated with an ellipsis past `CHIP_VALUE_MAX_W`.
pub fn chip_value(t: &Theme, value: impl Into<SharedString>) -> Div {
    div().max_w(t.s(CHIP_VALUE_MAX_W)).truncate().child(value.into())
}

/// Which chips of a one-line row show, so the row never wraps or overflows. Kept chips show first,
/// then the others in order until one doesn't fit, and the rest fold into More. The end group
/// (`end_w`) sits at the right and moves into More's menu when there's no room for it. `pin`
/// always shows. `rest` means filters only More can add, so More shows even when all chips fit.
pub struct ChipFold {
    pub shown: Vec<bool>,
    pub more: bool,
    pub end_in_more: bool,
}

#[allow(clippy::too_many_arguments)]
pub fn fold_chips(widths: &[f32], keep: &[bool], pin: Option<usize>, more_w: f32, end_w: f32, gap: f32, room: f32, rest: bool) -> ChipFold {
    let end_room = if end_w > 0.0 { gap + end_w } else { 0.0 };
    let all = widths.iter().map(|w| w + gap).sum::<f32>() - if widths.is_empty() { 0.0 } else { gap };
    if !rest && all + end_room <= room {
        return ChipFold { shown: vec![true; widths.len()], more: false, end_in_more: false };
    }
    let must = |i: usize| keep[i] || pin == Some(i);
    let kept: f32 = widths.iter().enumerate().filter(|(i, _)| must(*i)).map(|(_, w)| w + gap).sum();
    let budget = room - end_room - more_w;
    if kept <= budget {
        let mut used = kept;
        let mut folding = false;
        let shown = widths
            .iter()
            .enumerate()
            .map(|(i, w)| {
                if must(i) {
                    return true;
                }
                if !folding && used + w + gap <= budget {
                    used += w + gap;
                    return true;
                }
                folding = true;
                false
            })
            .collect();
        return ChipFold { shown, more: true, end_in_more: false };
    }
    // No room for the end group, so it goes into More and the kept chips show while they fit.
    let mut used = pin.map_or(0.0, |p| widths[p] + gap);
    let mut folding = false;
    let shown = widths
        .iter()
        .enumerate()
        .map(|(i, w)| {
            if pin == Some(i) {
                return true;
            }
            if keep[i] && !folding && used + w + gap <= room - more_w {
                used += w + gap;
                return true;
            }
            if keep[i] {
                folding = true;
            }
            false
        })
        .collect();
    ChipFold { shown, more: true, end_in_more: end_w > 0.0 }
}

/// A typed-number field, like Year or BPM. Shows the focus ring while typing, or the invalid ring
/// when its value is the problem and it isn't focused.
pub fn number_field(t: &Theme, id: impl Into<ElementId>, focused: bool, bad: bool, line: impl IntoElement) -> Stateful<Div> {
    let ring = if focused { Some((Hsla::from(t.accent_hsla()), px(2.0))) } else if bad { Some((Hsla::from(gpui::rgba(crate::theme::INVALID_RING)), px(crate::theme::INVALID_RING_W))) } else { None };
    div()
        .id(id)
        .flex()
        .items_center()
        .flex_none()
        .w(t.s(NUMBER_FIELD_W))
        .h(t.s(CHIP_H))
        .px(t.s(CHIP_PAD))
        .rounded_full()
        .bg(t.row())
        .text_size(t.text(TEXT_SM))
        .relative()
        .cursor_text()
        .child(line)
        .when_some(ring, |d, (c, w)| d.child(outline_ring(t.s(CHIP_H / 2.0), c, w)))
}

#[cfg(test)]
mod tests {
    use super::fold_chips;

    #[test]
    fn chips_fold_from_the_first_that_does_not_fit_and_keep_active_ones() {
        // Everything fits and there's nothing else to add, so no More.
        let f = fold_chips(&[50.0, 50.0], &[false, false], None, 40.0, 0.0, 5.0, 200.0, false);
        assert_eq!((f.shown, f.more, f.end_in_more), (vec![true, true], false, false));
        // Room for two plus More. The third and later chips fold, even one that would fit.
        let f = fold_chips(&[50.0, 50.0, 80.0, 10.0], &[false; 4], None, 40.0, 0.0, 5.0, 160.0, false);
        assert_eq!((f.shown, f.more), (vec![true, true, false, false], true));
        // An active chip stays, and the end group takes its share.
        let f = fold_chips(&[50.0, 50.0, 80.0], &[false, false, true], None, 40.0, 60.0, 5.0, 200.0, false);
        assert_eq!((f.shown, f.more, f.end_in_more), (vec![false, false, true], true, false));
        // Filters only More can add keep it on the line.
        let f = fold_chips(&[50.0], &[false], None, 40.0, 0.0, 5.0, 500.0, true);
        assert_eq!((f.shown, f.more), (vec![true], true));
    }

    #[test]
    fn no_room_moves_the_end_group_into_more_and_folds_active_chips_in_order() {
        // Three active chips leave no room for Clear All, so it goes into More and the third one folds.
        let f = fold_chips(&[60.0, 60.0, 60.0], &[true, true, true], None, 40.0, 80.0, 5.0, 180.0, false);
        assert_eq!((f.shown, f.more, f.end_in_more), (vec![true, true, false], true, true));
        // The one being opened stays, ahead of the others.
        let f = fold_chips(&[60.0, 60.0, 60.0], &[true, true, true], Some(2), 40.0, 80.0, 5.0, 180.0, false);
        assert_eq!(f.shown, vec![true, false, true]);
    }
}

/// Which corner a [`cover_label`] sits in. Bottom-left, or top-left when the bottom has a name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LabelAt {
    Top,
    Bottom,
}

/// how a [`cover_label`] looks: the glass chip, or `Mark`, the bare glyph and uppercase word over a photo, sized by
/// the cover's `width` (`--cover-mark-*`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LabelStyle {
    Chip,
    Mark { glyph: &'static str, width: f32 },
}

/// the quiet label on art the app made; the chip is `--scrim` with the glass edge, `--cover-label-*` from the corner.
pub fn cover_label(t: &Theme, label: impl Into<SharedString>, at: LabelAt, style: LabelStyle) -> Div {
    let label: SharedString = label.into();
    let inset = match style {
        LabelStyle::Chip => COVER_LABEL_INSET,
        LabelStyle::Mark { width, .. } => width * COVER_MARK_INSET,
    };
    let el = div().absolute().left(t.s(inset));
    let el = match at {
        LabelAt::Top => el.top(t.s(inset)),
        LabelAt::Bottom => el.bottom(t.s(inset)),
    };
    match style {
        LabelStyle::Chip => el
            .px(t.s(COVER_LABEL_PAD_X))
            .py(t.s(COVER_LABEL_PAD_Y))
            .rounded_full()
            .bg(t.scrim())
            .shadow(glass_edge())
            .text_size(t.text(TEXT_XS))
            .font_weight(FontWeight::SEMIBOLD)
            .line_height(relative(1.3))
            .text_color(t.t_on_art())
            .child(label),
        LabelStyle::Mark { glyph, width } => el
            .flex()
            .items_center()
            .gap(t.s(width * COVER_MARK_GAP))
            .text_size(t.s(width * COVER_MARK_TEXT))
            .font_weight(FontWeight::SEMIBOLD)
            .line_height(relative(1.0))
            .text_color(t.cover_mark_ink())
            .child(icon(t, glyph, width * COVER_MARK_GLYPH, t.cover_mark_ink()))
            .child(label.to_uppercase()),
    }
}

/// so counting numbers don't shift.
pub fn tabular_figures() -> gpui::FontFeatures {
    gpui::FontFeatures(std::sync::Arc::new(vec![("tnum".into(), 1)]))
}

/// the `tile_fill` ramp along `TILE_FILL_ANGLE`, or `--panel-2` while the tint isn't known.
pub fn tile_fill_bg(t: &Theme, tint: Option<crate::color::Srgb>) -> gpui::Background {
    match tint {
        Some(tint) => {
            let (top, bottom) = crate::color::tile_fill(tint);
            gpui::linear_gradient(TILE_FILL_ANGLE, gpui::linear_color_stop(top.hsla(1.0), 0.0), gpui::linear_color_stop(bottom.hsla(1.0), 1.0))
        }
        None => t.panel_2().into(),
    }
}

/// one line's width from glyph advances, for layout outside paint; kerning isn't counted, so it errs slightly wide.
/// watches the room an element gets: when whether `need` fits differs from `fits` (what was drawn),
/// it draws again, so a whole-or-nothing choice follows a resize without waiting for another event.
pub fn watch_fit(store: std::rc::Rc<std::cell::RefCell<gpui::Bounds<gpui::Pixels>>>, need: gpui::Pixels, fits: bool) -> impl IntoElement {
    gpui::canvas(
        move |bounds, window, _| {
            *store.borrow_mut() = bounds;
            if (bounds.size.width + px(0.5) >= need) != fits {
                window.refresh();
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .inset_0()
}

/// shaped widths already measured, by text, size and weight; cleared when it grows past this
const TEXT_WIDTHS_KEPT: usize = 512;

thread_local! {
    static TEXT_WIDTHS: std::cell::RefCell<std::collections::HashMap<(String, u32, u32), gpui::Pixels>> = Default::default();
}

/// the width `text` draws at in the app's font, shaped as painting shapes it (kerning and all), so
/// a fit decided by it matches what's drawn; for places without a window
pub fn text_width_app(cx: &gpui::App, text: &str, size: gpui::Pixels, weight: FontWeight) -> gpui::Pixels {
    let key = (text.to_string(), f32::from(size).to_bits(), weight.0.to_bits());
    if let Some(width) = TEXT_WIDTHS.with(|w| w.borrow().get(&key).copied()) {
        return width;
    }
    let mut font = gpui::font(crate::assets::FONT_FAMILY);
    font.weight = weight;
    let run = gpui::TextRun { len: text.len(), font, color: Hsla::default(), background_color: None, underline: None, strikethrough: None };
    let width = gpui::WindowTextSystem::new(cx.text_system().clone()).layout_line(text, size, &[run], None).width;
    TEXT_WIDTHS.with(|w| {
        let mut w = w.borrow_mut();
        if w.len() >= TEXT_WIDTHS_KEPT {
            w.clear();
        }
        w.insert(key, width);
    });
    width
}

/// draws `child` fading out to clear towards its right edge, so the page behind shows through
/// whatever it is (a tint, the translucent window). the last `gap` of the width is clear; the fade
/// runs over the `fade` before it through `stops`. both are window pixels; nothing is faded at 0.
pub fn fade_right(child: AnyElement, fade: gpui::Pixels, gap: gpui::Pixels, stops: [(f32, f32); 2]) -> AnyElement {
    FadeRight { child, fade, gap, stops }.into_any_element()
}

struct FadeRight {
    child: AnyElement,
    fade: gpui::Pixels,
    gap: gpui::Pixels,
    stops: [(f32, f32); 2],
}

impl FadeRight {
    fn within<R>(&mut self, bounds: gpui::Bounds<gpui::Pixels>, window: &mut gpui::Window, f: impl FnOnce(&mut AnyElement, &mut gpui::Window) -> R) -> R {
        let clip = gpui::Bounds { origin: bounds.origin, size: gpui::size((bounds.size.width - self.gap).max(px(0.0)), bounds.size.height) };
        let fade = gpui::ClipFade { width: self.fade, stops: self.stops };
        window.with_faded_clip(clip, fade, |window| f(&mut self.child, window))
    }
}

impl IntoElement for FadeRight {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl gpui::Element for FadeRight {
    type RequestLayoutState = ();
    type PrepaintState = ();
    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }
    fn request_layout(&mut self, _: Option<&gpui::GlobalElementId>, _: Option<&gpui::InspectorElementId>, window: &mut gpui::Window, cx: &mut gpui::App) -> (gpui::LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }
    fn prepaint(&mut self, _: Option<&gpui::GlobalElementId>, _: Option<&gpui::InspectorElementId>, bounds: gpui::Bounds<gpui::Pixels>, _: &mut (), window: &mut gpui::Window, cx: &mut gpui::App) {
        self.within(bounds, window, |child, window| {
            child.prepaint(window, cx);
        });
    }
    fn paint(&mut self, _: Option<&gpui::GlobalElementId>, _: Option<&gpui::InspectorElementId>, bounds: gpui::Bounds<gpui::Pixels>, _: &mut (), _: &mut (), window: &mut gpui::Window, cx: &mut gpui::App) {
        self.within(bounds, window, |child, window| child.paint(window, cx));
    }
}
