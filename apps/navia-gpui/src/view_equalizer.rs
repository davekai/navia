//! The Equalizer page. Settings are saved per output in `eq_model`. Nothing reaches the audio yet, and the page says
//! so.

use crate::i18n::{tr, trf, tr_dyn};
use std::cell::RefCell;
use std::rc::Rc;

use gpui::prelude::*;
use crate::focus::FocusStop;
use gpui::{AnyElement, BoxShadow, Bounds, Context, CursorStyle, FontWeight, Hsla, MouseButton, PathBuilder, Pixels, Point, SharedString, Window, canvas, div, point, px, relative, size};

use crate::color::{tint_fg, tint_vivid};
use crate::eq_model::{self, BANDS, BAND_LABELS, BUILT_IN_PRESETS, Correction, CustomPreset, EQ_RANGE, EqMode, EqProfile, MAX_FILTERS, OUTPUT_DEFAULT, correction_curve, emptiest_freq, format_db, format_freq, freq_to_x, gain_to_y, peak_level, profile_curve, snap_freq, snap_gain, x_to_freq, y_to_gain};
use crate::pages::Block;
use crate::shell::{Drag, Layout, Shell};
use crate::theme::size::*;
use crate::ui::{glass_edge, icon};
use crate::ui_controls::{PointerDrag, SelectOption, SliderSpec, TextTarget, glass_action, line_edge, pill_shell, primary_action, round_button, segmented_glass, switch, with_ring};
use crate::view_settings::{RowPos, section_title, settings_row};

const TICKS: [f32; 5] = [12.0, 6.0, 0.0, -6.0, -12.0];
/// How close to a filter's dot a press has to land, in window pixels.
const GRAB_RADIUS: f32 = 28.0;

#[derive(Default)]
pub struct EqUi {
    pub active_filter: Option<u64>,
    /// The band or filter the arrow keys move.
    pub held: Option<usize>,
    pub naming: bool,
    pub preset_name: String,
    pub graph_cache: Rc<RefCell<Option<GraphPaths>>>,
}

/// The graph's tessellated paths, kept between frames since only their position changes.
pub struct GraphPaths {
    key: u64,
    width: Pixels,
    height: Pixels,
    paths: Vec<(gpui::Path<Pixels>, Hsla)>,
}

/// Identifies the curve and line widths the paths were built for.
fn graph_key(curve: &[eq_model::CurvePoint], extra: Option<&[eq_model::CurvePoint]>, line_w: Pixels) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    f32::from(line_w).to_bits().hash(&mut hasher);
    for (x, g) in curve.iter().chain(extra.unwrap_or(&[]).iter()) {
        x.to_bits().hash(&mut hasher);
        g.to_bits().hash(&mut hasher);
    }
    hasher.finish()
}

fn db(v: f32) -> String {
    format!("{} dB", format_db(v))
}

fn balance_text(v: f32) -> String {
    if v == 0.0 {
        tr("Center").to_string()
    } else {
        if v < 0.0 { trf("{percent}% left", &[("percent", &(v.abs().round() as i32).to_string())]) } else { trf("{percent}% right", &[("percent", &(v.abs().round() as i32).to_string())]) }
    }
}

impl Shell {
    pub fn build_equalizer(&mut self, _layout: &Layout) -> Vec<Block> {
        let header = Shell::plain_block(|this, layout, window, cx| this.render_eq_master(layout, window, cx));
        let graph = Shell::plain_block(|this, layout, window, cx| this.render_eq_graph_card(layout, window, cx));
        let gain = Shell::plain_block(|this, layout, _, cx| this.render_eq_gain(layout, cx));
        let hp = Shell::plain_block(|this, layout, _, cx| this.render_eq_headphones(layout, cx));
        let stereo = Shell::plain_block(|this, layout, _, cx| this.render_eq_stereo(layout, cx));
        vec![header, Block::Spacer(22.0), graph, Block::Spacer(30.0), gain, Block::Spacer(30.0), hp, Block::Spacer(30.0), stereo]
    }

    fn eq_frame(&self, layout: &Layout, content: gpui::Div) -> gpui::Div {
        let t = self.theme;
        div().w(layout.main_width).child(div().w_full().max_w(t.s(1040.0)).mx_auto().px(t.s(GUTTER)).child(content))
    }

    /// While the equalizer is off, everything below the master switch dims and ignores clicks.
    fn eq_inert(&self, el: gpui::Div) -> gpui::Div {
        if self.eq.enabled {
            el
        } else {
            div().relative().child(el.opacity(0.45)).child(div().absolute().inset_0().occlude())
        }
    }

    pub fn eq_correction(&self, name: &str) -> Option<Correction> {
        if name.is_empty() {
            return None;
        }
        self.eq.headphones.iter().find(|h| h.name == name).map(|h| h.correction.clone()).or_else(|| self.sample_mode.then(|| eq_model::sample_headphones().into_iter().find(|h| h.name == name).map(|h| h.correction)).flatten())
    }

    pub fn equalizer_summary(&self) -> String {
        if !self.eq.enabled {
            return tr("Off. Audio plays unprocessed.").to_string();
        }
        let p = self.eq.profile_ref();
        let name = match p.mode {
            EqMode::Graphic => p.preset,
            EqMode::Parametric => p.parametric_preset,
        };
        let name = if name.is_empty() { tr("a custom curve").to_string() } else { name };
        trf("On, using {name} for {output}.", &[("name", &tr_dyn(&name)), ("output", &self.output_label(&self.eq.output))])
    }

    fn render_eq_master(&mut self, layout: &Layout, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        self.ensure_outputs(cx);
        let enabled = self.eq.enabled;
        let hint = if enabled { tr("On. Your EQ, correction and stereo settings shape everything Navia plays.") } else { tr("Off. Audio plays unprocessed: no EQ, correction or stereo changes. Your settings are kept.") };
        let toggle = switch(&t, "eq-enabled", enabled, true, cx, |this, cx| {
            this.eq.enabled = !this.eq.enabled;
            this.eq.save();
            cx.notify();
        })
        .into_any_element();
        let outputs = self.output_options();
        let current = if outputs.iter().any(|o| o.value == self.eq.output.as_str()) { self.eq.output.clone() } else { OUTPUT_DEFAULT.to_string() };
        let picker = self.select_pill(crate::view_settings::EQ_OUTPUT_SELECT, current, outputs, tr("Choose"), Some(210.0), |this, value, cx| {
            this.eq.output = value.to_string();
            this.eq_ui.active_filter = None;
            this.eq_ui.held = None;
            this.eq.save();
            cx.notify();
        }, cx);
        let card = div()
            .flex()
            .flex_col()
            .child(settings_row(&t, tr("Equalizer"), Some(hint.into()), toggle, RowPos::First))
            .child(settings_row(&t, tr("Output"), Some(tr("Each output remembers its own settings on this page.").into()), picker, RowPos::Last));
        self.eq_frame(layout, div().flex().flex_col().child(card)).into_any_element()
    }

    fn render_eq_graph_card(&mut self, layout: &Layout, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let main_tokens = f32::from(layout.main_width) / t.token_scale();
        let narrow = main_tokens <= 620.0;
        let compact = main_tokens <= 760.0;
        let profile = self.eq.profile_ref();
        let tint = self.tint();
        let vivid = tint_vivid(tint);
        let panel = crate::color::Srgb::hex(crate::theme::PANEL);
        let top = vivid.mix(panel, 0.44).hsla(1.0);
        let fg = tint_fg(tint);
        let parametric = profile.mode == EqMode::Parametric;

        let correction = self.eq_correction(&profile.headphone);
        let toolbar = self.render_eq_toolbar(&profile, narrow, window, cx);

        let plot_h = if compact { 220.0 } else { 280.0 };
        let plot = self.render_eq_plot(&profile, correction.as_ref(), plot_h, fg, cx);
        let scale_w = if compact { 36.0 } else { 46.0 };
        let mut scale = div().relative().flex_none().w(t.s(scale_w)).h(t.s(plot_h)).my(t.s(8.0));
        for db_value in TICKS {
            let y = gain_to_y(db_value);
            let label = if db_value == 0.0 { "0 dB".to_string() } else { format_db(db_value).replace(".0", "") };
            scale = scale.child(div().absolute().right(t.s(if compact { 8.0 } else { 12.0 })).top(relative(y)).mt(t.s(-9.0)).text_size(t.text(TEXT_XS)).text_color(t.t3()).whitespace_nowrap().child(label));
        }
        let mut axis = div().relative().h(t.s(40.0));
        for (i, freq) in BANDS.iter().enumerate() {
            let label = if compact { BAND_LABELS[i].1 } else { BAND_LABELS[i].0 };
            let mut stack = div().absolute().top(t.s(4.0)).left(relative(freq_to_x(*freq))).ml(t.s(-30.0)).w(t.s(60.0)).flex().flex_col().items_center().line_height(relative(1.25)).text_size(t.text(TEXT_XS)).text_color(t.t2()).child(label);
            if !parametric {
                stack = stack.child(div().text_color(t.t3()).child(format_db(profile.gains[i])));
            }
            axis = axis.child(stack);
        }
        let graph = div().flex().flex_col().child(div().flex().flex_row().child(scale).child(plot)).child(div().flex().flex_row().child(div().flex_none().w(t.s(scale_w))).child(div().flex_1().min_w(px(0.0)).child(axis)));

        let mut card = div()
            .flex()
            .flex_col()
            .gap(t.s(14.0))
            .pt(t.s(16.0))
            .px(t.s(if narrow { 12.0 } else { 20.0 }))
            .pb(t.s(18.0))
            .rounded(t.s(18.0))
            .bg(gpui::linear_gradient(180.0, gpui::linear_color_stop(top, 0.0), gpui::linear_color_stop(Hsla::from(t.panel()), 0.9)))
            .shadow(vec![line_edge(&t)])
            .child(toolbar);
        if self.eq_ui.naming && name_taken(self.eq_ui.preset_name.trim()) {
            card = card.child(div().w_full().flex().justify_end().text_size(t.text(TEXT_SM)).text_color(t.t2()).child(tr("That name belongs to a built-in preset.")));
        }
        card = card.child(graph);
        if !profile.headphone.is_empty() && correction.is_some() {
            card = card.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(t.s(8.0))
                    .text_size(t.text(TEXT_SM))
                    .text_color(t.t2())
                    .child(dashes(&t))
                    .child(trf("Headphone correction for {name}", &[("name", &profile.headphone)])),
            );
        }
        if parametric {
            card = card.child(self.render_eq_filters(&profile, narrow, cx));
        }
        self.eq_frame(layout, self.eq_inert(card)).into_any_element()
    }

    fn render_eq_toolbar(&mut self, profile: &EqProfile, narrow: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let mode = profile.mode;
        let parametric = mode == EqMode::Parametric;
        let segmented = segmented_glass(&t, "eq-mode", mode, &[(EqMode::Graphic, tr("Graphic")), (EqMode::Parametric, tr("Parametric"))], cx, |this, next, cx| {
            this.eq.profile().mode = next;
            this.eq.save();
            cx.notify();
        });
        let bar = div().flex().flex_row().flex_wrap().items_center().justify_between().gap_x(t.s(16.0)).gap_y(t.s(10.0)).child(segmented);
        if self.eq_ui.naming {
            let name = self.eq_ui.preset_name.clone();
            let trimmed = name.trim().to_string();
            let can_save = !trimmed.is_empty() && !name_taken(&trimmed);
            let focused = self.controls.text == Some(TextTarget::EqPresetName);
            let line = self.text_line(&TextTarget::EqPresetName, &name, tr("Preset name"), "eq-name-text", window);
            let input = pill_shell(&t, "eq-name")
                .w(t.s(200.0))
                .px(t.s(16.0))
                .cursor_text()
                .when(focused, |d| d.shadow(with_ring(glass_edge(), t.accent_hsla())))
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.focus_text(TextTarget::EqPresetName, cx);
                }))
                .child(line);
            let save = primary_action(&t, "eq-name-save", can_save).child(tr("Save")).when(can_save, |d| d.stop(&t).on_click(cx.listener(|this, _, _, cx| this.eq_commit_preset(cx))));
            let cancel = glass_action(&t, "eq-name-cancel").child(tr("Cancel")).stop(&t).on_click(cx.listener(|this, _, _, cx| {
                this.eq_ui.naming = false;
                this.controls.text = None;
                cx.notify();
            }));
            return bar.child(div().flex().flex_row().flex_wrap().items_center().gap(t.s(8.0)).child(input).child(save).child(cancel)).into_any_element();
        }
        let custom: Vec<CustomPreset> = self.eq.custom.iter().filter(|p| p.mode == mode).cloned().collect();
        let preset_name = if parametric { profile.parametric_preset.clone() } else { profile.preset.clone() };
        let selected = custom.iter().find(|p| p.name == preset_name).map(|p| p.name.clone());
        let mut options: Vec<SelectOption> = if parametric { Vec::new() } else { BUILT_IN_PRESETS.iter().map(|p| SelectOption::grouped(p.name, tr(p.name), tr("Built-in"))).collect() };
        options.extend(custom.iter().map(|p| SelectOption::grouped(p.name.clone(), p.name.clone(), tr("Your presets"))));
        let mut presets = div().flex().flex_row().items_center().gap(t.s(8.0)).min_w(px(0.0)).when(narrow, |d| d.w_full());
        if !options.is_empty() {
            let placeholder = if parametric { tr("Custom filters") } else { tr("Custom curve") };
            let field = self.select_pill("eq-preset", preset_name, Rc::new(options), placeholder, if narrow { None } else { Some(210.0) }, move |this, value, cx| this.eq_apply_preset(&value, cx), cx);
            presets = presets.child(div().when(narrow, |d| d.flex_1().min_w(px(0.0))).child(field));
        }
        if let Some(name) = selected {
            presets = presets.child(round_button(&t, "eq-delete-preset", trf("Delete {name}", &[("name", &name)]), "trash-regular").stop(&t).on_click(cx.listener(move |this, _, _, cx| {
                this.eq.delete_preset(&name, mode);
                cx.notify();
            })));
        }
        presets = presets.child(round_button(&t, "eq-save-preset", tr("Save as preset"), "floppy-disk-regular").stop(&t).on_click(cx.listener(|this, _, _, cx| {
            let profile = this.eq.profile_ref();
            let name = if profile.mode == EqMode::Parametric { profile.parametric_preset } else { profile.preset };
            let custom = this.eq.custom.iter().any(|p| p.mode == profile.mode && p.name == name);
            this.eq_ui.preset_name = if custom { name } else { String::new() };
            this.eq_ui.naming = true;
            this.focus_text(TextTarget::EqPresetName, cx);
        })));
        presets = presets.child(round_button(&t, "eq-reset", if parametric { tr("Set every filter to 0 dB") } else { tr("Reset to Flat") }, "arrow-counter-clockwise-regular").stop(&t).on_click(cx.listener(|this, _, _, cx| this.eq_reset(cx))));
        bar.child(presets).into_any_element()
    }

    fn eq_apply_preset(&mut self, value: &SharedString, cx: &mut Context<Self>) {
        let profile = self.eq.profile_ref();
        let name = value.to_string();
        if profile.mode == EqMode::Parametric {
            if let Some(filters) = self.eq.custom.iter().find(|p| p.mode == EqMode::Parametric && p.name == name).and_then(|p| p.filters.clone()) {
                let p = self.eq.profile();
                p.filters = filters;
                p.parametric_preset = name;
            }
        } else {
            let gains = BUILT_IN_PRESETS.iter().find(|p| p.name == name).map(|p| p.gains).or_else(|| self.eq.custom.iter().find(|p| p.mode == EqMode::Graphic && p.name == name).and_then(|p| p.gains));
            if let Some(gains) = gains {
                let p = self.eq.profile();
                p.gains = gains;
                p.preset = name;
            }
        }
        self.eq.save();
        cx.notify();
    }

    fn eq_reset(&mut self, cx: &mut Context<Self>) {
        let p = self.eq.profile();
        if p.mode == EqMode::Parametric {
            for f in &mut p.filters {
                f.gain = 0.0;
            }
            p.parametric_preset.clear();
        } else {
            p.gains = [0.0; 10];
            p.preset = "Flat".to_string();
        }
        self.eq.save();
        cx.notify();
    }

    pub fn eq_commit_preset(&mut self, cx: &mut Context<Self>) {
        let name = self.eq_ui.preset_name.trim().to_string();
        if name.is_empty() || name_taken(&name) {
            return;
        }
        let profile = self.eq.profile_ref();
        let entry = match profile.mode {
            EqMode::Graphic => CustomPreset { name, mode: EqMode::Graphic, gains: Some(profile.gains), filters: None },
            EqMode::Parametric => CustomPreset { name, mode: EqMode::Parametric, gains: None, filters: Some(profile.filters.clone()) },
        };
        self.eq.save_preset(entry);
        self.eq_ui.naming = false;
        self.controls.text = None;
        cx.notify();
    }

    fn eq_set_band(&mut self, band: usize, gain: f32, cx: &mut Context<Self>) {
        let p = self.eq.profile();
        p.gains[band] = eq_model::clamp_gain(gain);
        p.preset.clear();
        cx.notify();
    }

    fn eq_set_filter(&mut self, index: usize, freq: Option<f32>, gain: Option<f32>, q: Option<f32>, cx: &mut Context<Self>) {
        let p = self.eq.profile();
        if let Some(f) = p.filters.get_mut(index) {
            if let Some(v) = freq {
                f.freq = eq_model::clamp_freq(v);
            }
            if let Some(v) = gain {
                f.gain = eq_model::clamp_gain(v);
            }
            if let Some(v) = q {
                f.q = v.clamp(0.2, 8.0);
            }
            p.parametric_preset.clear();
        }
        cx.notify();
    }

    fn eq_add_filter(&mut self, cx: &mut Context<Self>) {
        let freq = emptiest_freq(&self.eq.profile().filters);
        let filter = self.eq.new_filter(freq);
        self.eq.profile().filters.push(filter);
        self.eq.profile().parametric_preset.clear();
        self.eq_ui.active_filter = Some(filter.id);
        self.eq.save();
        cx.notify();
    }

    fn render_eq_plot(&mut self, profile: &EqProfile, correction: Option<&Correction>, plot_h: f32, fg: crate::color::Srgb, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let curve = profile_curve(profile);
        let extra = correction.map(correction_curve);
        let parametric = profile.mode == EqMode::Parametric;
        let line_w = t.s(2.5);
        let area_color = fg.hsla(crate::theme::EQ_AREA_OPACITY);
        let curve_color = fg.hsla(1.0);
        let extra_color: Hsla = t.t2().into();
        let extra_w = t.s(1.5);
        let dash = [t.s(5.0), t.s(5.0)];
        let key = graph_key(&curve, extra.as_deref(), line_w);
        let cache = self.eq_ui.graph_cache.clone();
        let paint = canvas(|_, _, _| {}, move |bounds, _, window, _| {
            let (w, h) = (bounds.size.width, bounds.size.height);
            for db_value in TICKS {
                let y = gain_to_y(db_value);
                let color = if db_value == 0.0 { Hsla::from(gpui::rgba(crate::theme::EQ_GRID_ZERO)) } else { Hsla::from(gpui::rgba(crate::theme::EQ_GRID)) };
                window.paint_quad(gpui::fill(Bounds { origin: point(bounds.origin.x, bounds.origin.y + h * y), size: size(w, px(1.0)) }, color));
            }
            for f in BANDS {
                let x = freq_to_x(f);
                window.paint_quad(gpui::fill(Bounds { origin: point(bounds.origin.x + w * x, bounds.origin.y), size: size(px(1.0), h) }, Hsla::from(gpui::rgba(crate::theme::EQ_GRID_BAND))));
            }
            // Paths are tessellated once per curve and size, at the origin, then moved into place.
            let mut slot = cache.borrow_mut();
            let stale = slot.as_ref().is_none_or(|c: &GraphPaths| c.key != key || c.width != w || c.height != h);
            if stale {
                let at = |x: f32, g: f32| point(w * x, h * gain_to_y(eq_model::clamp_gain(g)));
                let mut paths = Vec::with_capacity(3);
                if curve.len() > 1 {
                    let mut area = PathBuilder::fill();
                    area.move_to(at(curve[0].0, curve[0].1));
                    for (x, g) in curve.iter().skip(1) {
                        area.line_to(at(*x, *g));
                    }
                    area.line_to(at(1.0, 0.0));
                    area.line_to(at(0.0, 0.0));
                    area.close();
                    if let Ok(path) = area.build() {
                        paths.push((path, area_color));
                    }
                }
                if let Some(extra) = extra.as_ref() {
                    let mut builder = PathBuilder::stroke(extra_w).dash_array(&dash);
                    builder.move_to(at(extra[0].0, extra[0].1));
                    for (x, g) in extra.iter().skip(1) {
                        builder.line_to(at(*x, *g));
                    }
                    if let Ok(path) = builder.build() {
                        paths.push((path, extra_color));
                    }
                }
                if curve.len() > 1 {
                    let mut builder = PathBuilder::stroke(line_w);
                    builder.move_to(at(curve[0].0, curve[0].1));
                    for (x, g) in curve.iter().skip(1) {
                        builder.line_to(at(*x, *g));
                    }
                    if let Ok(path) = builder.build() {
                        paths.push((path, curve_color));
                    }
                }
                *slot = Some(GraphPaths { key, width: w, height: h, paths });
            }
            for (path, color) in &slot.as_ref().expect("built above").paths {
                let mut placed = path.clone();
                placed.bounds.origin += bounds.origin;
                for v in &mut placed.vertices {
                    v.xy_position.x += bounds.origin.x;
                    v.xy_position.y += bounds.origin.y;
                }
                window.paint_path(placed, *color);
            }
        })
        .absolute()
        .inset_0();
        let store = self.bounds_store("eq-plot");
        let handle_at = |x: f32, y: f32, d: f32| div().absolute().left(relative(x)).top(relative(y)).ml(t.s(-d / 2.0)).mt(t.s(-d / 2.0)).size(t.s(d)).rounded_full();
        let shadow = |ring: bool| {
            let mut s = vec![BoxShadow { color: gpui::rgba(crate::theme::EQ_HANDLE_RING).into(), offset: point(px(0.0), px(0.0)), blur_radius: px(0.0), spread_radius: px(1.0), inset: false }, crate::ui::shadow_eq_handle(&t)];
            if ring {
                s.insert(0, BoxShadow { color: gpui::rgba(crate::theme::EQ_HANDLE_ACTIVE_RING).into(), offset: point(px(0.0), px(0.0)), blur_radius: px(0.0), spread_radius: t.s(2.0), inset: false });
            }
            s
        };
        let mut plot = div()
            .id("eq-plot")
            .relative()
            .flex_1()
            .min_w(px(0.0))
            .h(t.s(plot_h))
            .my(t.s(8.0))
            .cursor(CursorStyle::ResizeUpDown)
            .child(crate::ui_controls::track_bounds(store.clone()))
            .child(paint);
        if parametric {
            let active = self.eq_ui.active_filter;
            let held = self.eq_ui.held;
            for (i, f) in profile.filters.iter().enumerate() {
                let is_active = active == Some(f.id) || held == Some(i);
                plot = plot.child(
                    handle_at(freq_to_x(f.freq), gain_to_y(f.gain), 24.0)
                        .bg(if is_active { t.accent_hsla() } else { Hsla::from(t.t1()) })
                        .shadow(shadow(is_active))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(t.text(TEXT_XS))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(t.on_accent.hsla(1.0))
                        .child(format!("{}", i + 1)),
                );
            }
        } else {
            for (i, freq) in BANDS.iter().enumerate() {
                let d = if self.eq_ui.held == Some(i) { 14.0 * 1.25 } else { 14.0 };
                plot = plot.child(handle_at(freq_to_x(*freq), gain_to_y(profile.gains[i]), d).bg(t.t1()).shadow(shadow(false)));
            }
        }
        let press_store = store;
        plot.on_mouse_down(MouseButton::Left, cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
            cx.stop_propagation();
            let bounds = *press_store.borrow();
            let (fx, fy) = plot_fraction(event.position, bounds);
            let profile = this.eq.profile_ref();
            let index = if profile.mode == EqMode::Parametric {
                let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
                let mut nearest = GRAB_RADIUS;
                let mut found = None;
                for (i, f) in profile.filters.iter().enumerate() {
                    let d = ((freq_to_x(f.freq) - fx) * w).hypot((gain_to_y(f.gain) - fy) * h);
                    if d <= nearest {
                        nearest = d;
                        found = Some(i);
                    }
                }
                let Some(i) = found else { return };
                this.eq_ui.active_filter = Some(profile.filters[i].id);
                i
            } else {
                (0..BANDS.len()).min_by(|a, b| (freq_to_x(BANDS[*a]) - fx).abs().total_cmp(&(freq_to_x(BANDS[*b]) - fx).abs())).unwrap_or(0)
            };
            this.eq_ui.held = Some(index);
            let press_store2 = press_store.clone();
            let apply = Rc::new(move |this: &mut Shell, position: Point<Pixels>, cx: &mut Context<Shell>| {
                let (fx, fy) = plot_fraction(position, *press_store2.borrow());
                if this.eq.profile_ref().mode == EqMode::Parametric {
                    this.eq_set_filter(index, Some(snap_freq(x_to_freq(fx))), Some(snap_gain(y_to_gain(fy))), None, cx);
                } else {
                    this.eq_set_band(index, snap_gain(y_to_gain(fy)), cx);
                }
            });
            apply(this, event.position, cx);
            this.controls.pointer = Some(PointerDrag {
                on_move: apply,
                on_end: Some(Rc::new(|this: &mut Shell, cx: &mut Context<Shell>| {
                    this.eq.save();
                    cx.notify();
                })),
            });
            this.dragging = Some(Drag::Pointer);
            cx.notify();
        }))
        .into_any_element()
    }

    /// Arrow keys move the band or filter that was pressed last.
    pub fn eq_key(&mut self, event: &gpui::KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let key = event.keystroke.key.as_str();
        if !matches!(key, "up" | "down" | "left" | "right" | "home") {
            return false;
        }
        let profile = self.eq.profile_ref();
        let shift = event.keystroke.modifiers.shift;
        let nudge = if shift { 3.0 } else { 0.5 };
        if profile.mode == EqMode::Parametric {
            let Some(id) = self.eq_ui.active_filter else { return false };
            let Some(i) = profile.filters.iter().position(|f| f.id == id) else { return false };
            let filter = profile.filters[i];
            match key {
                "left" | "right" => {
                    let step = if key == "left" { -1.0 } else { 1.0 } * if shift { 0.05 } else { 0.01 };
                    self.eq_set_filter(i, Some(snap_freq(eq_model::clamp_freq(x_to_freq(freq_to_x(filter.freq) + step)))), None, None, cx);
                }
                "up" => self.eq_set_filter(i, None, Some(filter.gain + nudge), None, cx),
                "down" => self.eq_set_filter(i, None, Some(filter.gain - nudge), None, cx),
                _ => return false,
            }
        } else {
            let Some(band) = self.eq_ui.held else { return false };
            match key {
                "up" => self.eq_set_band(band, profile.gains[band] + nudge, cx),
                "down" => self.eq_set_band(band, profile.gains[band] - nudge, cx),
                "home" => self.eq_set_band(band, 0.0, cx),
                _ => return false,
            }
        }
        self.eq.save();
        true
    }

    fn render_eq_filters(&mut self, profile: &EqProfile, narrow: bool, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let can_remove = profile.filters.len() > 1;
        let mut list = div().flex().flex_col().items_start().gap(t.s(8.0)).mt(t.s(4.0));
        for (i, filter) in profile.filters.iter().enumerate() {
            let active = self.eq_ui.active_filter == Some(filter.id);
            let id = filter.id;
            let freq_text = format_freq(filter.freq);
            let freq_x = (freq_to_x(filter.freq) * 1000.0).round();
            let freq = self.render_slider(SliderSpec::new(format!("eq-f-freq-{id}"), freq_x, 0.0, 1000.0, move |this, v, cx| this.eq_set_filter(i, Some(snap_freq(x_to_freq(v / 1000.0))), None, None, cx)).width(None).on_end(|this, _| this.eq.save()), cx);
            let gain_text = db(filter.gain);
            let gain = self.render_slider(SliderSpec::new(format!("eq-f-gain-{id}"), filter.gain, -EQ_RANGE, EQ_RANGE, move |this, v, cx| this.eq_set_filter(i, None, Some(v), None, cx)).step(0.5).origin(0.0).width(None).on_end(|this, _| this.eq.save()), cx);
            let q_text = format!("{:.1}", filter.q);
            let q = self.render_slider(SliderSpec::new(format!("eq-f-q-{id}"), filter.q, 0.2, 8.0, move |this, v, cx| this.eq_set_filter(i, None, None, Some(v), cx)).step(0.1).width(None).on_end(|this, _| this.eq.save()), cx);
            let param = |label: &'static str, value: String, child: AnyElement| {
                div()
                    .flex()
                    .flex_col()
                    .gap(t.s(6.0))
                    .flex_1()
                    .min_w(px(0.0))
                    .child(div().flex().flex_row().justify_between().gap(t.s(8.0)).text_size(t.text(TEXT_SM)).text_color(t.t2()).child(label).child(div().text_color(t.t1()).child(value)))
                    .child(child)
            };
            let badge = div().flex().items_center().justify_center().flex_none().size(t.s(24.0)).rounded_full().bg(if active { t.accent_hsla() } else { t.panel_2().into() }).text_size(t.text(TEXT_XS)).font_weight(FontWeight::SEMIBOLD).text_color(if active { t.on_accent.hsla(1.0) } else { t.t2().into() }).child(format!("{}", i + 1));
            let params = div().flex().min_w(px(0.0)).flex_1().when(narrow, |d| d.flex_col().gap(t.s(10.0))).when(!narrow, |d| d.flex_row().gap(t.s(18.0))).child(param(tr("Frequency"), freq_text, freq)).child(param(tr("Gain"), gain_text, gain)).child(param("Q", q_text, q));
            let remove = crate::ui::ink_button(&t, ("eq-f-remove", i), trf("Remove filter {n}", &[("n", &(i + 1).to_string())]), "x-regular", 16.0, t.t1().into(), t.hover_ink()).w(t.s(ACTION_W)).when(!can_remove, |d| d.opacity(0.35)).when(can_remove, |d| {
                d.stop(&t).on_click(cx.listener(move |this, _, _, cx| {
                    this.eq.profile().filters.retain(|f| f.id != id);
                    this.eq.profile().parametric_preset.clear();
                    this.eq.save();
                    cx.notify();
                }))
            });
            list = list.child(
                div()
                    .id(("eq-filter", i))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(t.s(18.0))
                    .w_full()
                    .pl(t.s(14.0))
                    .pr(t.s(8.0))
                    .py(t.s(10.0))
                    .rounded(t.s(R_MD))
                    .bg(t.row())
                    .when(active, |d| d.shadow(vec![BoxShadow { color: t.accent_hsla(), offset: point(px(0.0), px(0.0)), blur_radius: px(0.0), spread_radius: px(1.0), inset: true }]))
                    .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| {
                        this.eq_ui.active_filter = Some(id);
                        cx.notify();
                    }))
                    .child(badge)
                    .child(params)
                    .child(remove),
            );
        }
        if profile.filters.len() < MAX_FILTERS {
            list = list.child(glass_action(&t, "eq-add-filter").child(icon(&t, "plus-regular", 15.0, t.t1())).child(tr("Add Filter")).stop(&t).on_click(cx.listener(|this, _, _, cx| this.eq_add_filter(cx))));
        }
        list.into_any_element()
    }

    fn render_eq_gain(&mut self, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let profile = self.eq.profile_ref();
        let correction = self.eq_correction(&profile.headphone);
        let peak = peak_level(&profile, correction.as_ref());
        let preamp_slider = self.render_slider(SliderSpec::new("eq-preamp", profile.preamp, -EQ_RANGE, EQ_RANGE, |this, v, cx| {
            this.eq.profile().preamp = v;
            cx.notify();
        }).step(0.5).origin(0.0).on_end(|this, _| this.eq.save()), cx);
        let limiter = switch(&t, "eq-limiter", profile.limiter, true, cx, |this, cx| {
            let v = !this.eq.profile().limiter;
            this.eq.profile().limiter = v;
            this.eq.save();
            cx.notify();
        }).into_any_element();
        let limiter_hint: SharedString = if peak > 0.05 {
            trf("Catches peaks that would clip. These settings reach {level}.", &[("level", &db((peak * 10.0).round() / 10.0))]).into()
        } else {
            tr("Catches peaks that would clip. These settings stay below 0 dB.").into()
        };
        let card = div()
            .flex()
            .flex_col()
            .child(section_title(&t, tr("Gain")))
            .child(settings_row(&t, tr("Preamp"), Some(trf("{level} before the equalizer.", &[("level", &db(profile.preamp))]).into()), preamp_slider, RowPos::First))
            .child(settings_row(&t, tr("Soft limiter"), Some(limiter_hint), limiter, RowPos::Last));
        self.eq_frame(layout, self.eq_inert(card)).into_any_element()
    }

    fn render_eq_headphones(&mut self, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let profile = self.eq.profile_ref();
        let main_tokens = f32::from(layout.main_width) / t.token_scale();
        let mut options = vec![SelectOption::new("", tr("None"))];
        if self.sample_mode {
            options.extend(eq_model::sample_headphones().into_iter().map(|h| SelectOption::grouped(h.name.clone(), h.name, h.group)));
        }
        options.extend(self.eq.headphones.iter().map(|h| SelectOption::grouped(h.name.clone(), h.name.clone(), tr_dyn(&h.group))));
        let has_imported = !self.eq.headphones.is_empty();
        let select = self.select_pill("eq-headphone", profile.headphone.clone(), Rc::new(options), tr("None"), Some(if main_tokens <= 620.0 { 220.0 } else { 280.0 }), |this, value, cx| {
            this.eq.profile().headphone = value.to_string();
            this.eq.save();
            cx.notify();
        }, cx);
        let load = glass_action(&t, "eq-import").child(icon(&t, "upload-regular", 16.0, t.t1())).child(tr("Load File")).stop(&t).on_click(cx.listener(|this, _, _, cx| this.eq_import_prompt(cx)));
        let imported = self.eq.headphones.iter().any(|h| h.name == profile.headphone);
        let mut row = div().flex().flex_row().flex_wrap().justify_end().items_center().gap(t.s(8.0)).child(select).child(load);
        if imported {
            row = row.child(round_button(&t, "eq-remove-headphone", trf("Delete {name}", &[("name", &profile.headphone)]), "trash-regular").stop(&t).on_click(cx.listener(move |this, _, _, cx| {
                let name = this.eq.profile_ref().headphone;
                this.eq.headphones.retain(|h| h.name != name);
                for p in this.eq.profiles.values_mut() {
                    if p.headphone == name {
                        p.headphone.clear();
                    }
                }
                this.eq.save();
                cx.notify();
            })));
        }
        let hint: SharedString = if self.sample_mode && !has_imported {
            tr("Sample list with invented curves, or load an AutoEQ ParametricEQ.txt file.").into()
        } else if has_imported {
            tr("Choose a loaded AutoEQ profile, or load another ParametricEQ.txt file.").into()
        } else {
            tr("Load an AutoEQ ParametricEQ.txt file. AutoEQ publishes one for most headphones.").into()
        };
        let card = div().flex().flex_col().child(section_title(&t, tr("Headphone Correction"))).child(settings_row(&t, tr("Headphone model"), Some(hint), row.into_any_element(), RowPos::Single));
        self.eq_frame(layout, self.eq_inert(card)).into_any_element()
    }

    pub fn eq_import_prompt(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(gpui::PathPromptOptions { files: true, directories: false, multiple: false, prompt: None });
        cx.spawn(async move |this, cx| {
            let picked = receiver.await;
            let path = match picked {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                _ => None,
            };
            let Some(path) = path else {
                return;
            };
            let _ = this.update(cx, |this, cx| this.eq_import_path(&path, cx));
        })
        .detach();
    }

    pub fn eq_import_path(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        let name = eq_model::headphone_name_from_path(path);
        let result = std::fs::read_to_string(path).map_err(|e| trf("That file couldn't be read: {error}", &[("error", &e.to_string())])).and_then(|text| eq_model::parse_autoeq(&name, &text));
        match result {
            Ok(mut headphone) => {
                if headphone.name.is_empty() {
                    headphone.name = tr("AutoEQ profile").to_string();
                }
                let chosen = headphone.name.clone();
                self.eq.headphones.retain(|h| h.name != chosen);
                self.eq.headphones.push(headphone);
                self.eq.profile().headphone = chosen;
                self.eq.save();
                self.invalidate_page();
                cx.notify();
            }
            Err(message) => self.show_toast(message, cx),
        }
    }

    fn render_eq_stereo(&mut self, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let profile = self.eq.profile_ref();
        let balance = self.render_slider(SliderSpec::new("eq-balance", profile.balance, -100.0, 100.0, |this, v, cx| {
            this.eq.profile().balance = if v.abs() < 4.0 { 0.0 } else { v };
            cx.notify();
        }).origin(0.0).on_end(|this, _| this.eq.save()), cx);
        let mono = switch(&t, "eq-mono", profile.mono, true, cx, |this, cx| {
            let v = !this.eq.profile().mono;
            this.eq.profile().mono = v;
            this.eq.save();
            cx.notify();
        }).into_any_element();
        let crossfeed = switch(&t, "eq-crossfeed", profile.crossfeed, true, cx, |this, cx| {
            let v = !this.eq.profile().crossfeed;
            this.eq.profile().crossfeed = v;
            this.eq.save();
            cx.notify();
        }).into_any_element();
        let mut card = div()
            .flex()
            .flex_col()
            .child(section_title(&t, tr("Stereo")))
            .child(settings_row(&t, tr("Balance"), Some(balance_text(profile.balance).into()), balance, RowPos::First))
            .child(settings_row(&t, tr("Mono"), Some(tr("Plays both channels as one.").into()), mono, RowPos::Middle))
            .child(settings_row(&t, tr("Crossfeed"), Some(tr("Blends a little of each channel into the other, for easier listening on headphones.").into()), crossfeed, if profile.crossfeed { RowPos::Middle } else { RowPos::Last }));
        if profile.crossfeed {
            let amount = self.render_slider(SliderSpec::new("eq-crossfeed-amount", profile.crossfeed_amount, 0.0, 100.0, |this, v, cx| {
                this.eq.profile().crossfeed_amount = v;
                cx.notify();
            }).on_end(|this, _| this.eq.save()), cx);
            card = card.child(settings_row(&t, tr("Crossfeed strength"), Some(format!("{}%", profile.crossfeed_amount.round() as i32).into()), amount, RowPos::Last));
        }
        self.eq_frame(layout, self.eq_inert(card)).into_any_element()
    }

    pub fn eq_script(&mut self, op: &str, cx: &mut Context<Self>) -> bool {
        match op {
            "eq-on" => self.eq.enabled = true,
            "eq-off" => self.eq.enabled = false,
            "eq-parametric" => self.eq.profile().mode = EqMode::Parametric,
            "eq-graphic" => self.eq.profile().mode = EqMode::Graphic,
            "eq-preset" => self.eq_apply_preset(&SharedString::from(tr("Bass Boost")), cx),
            "eq-hp" => self.eq.profile().headphone = tr("Sennheiser HD 650").to_string(),
            "eq-naming" => {
                self.eq_ui.naming = true;
                self.eq_ui.preset_name = tr("My Curve").to_string();
                self.controls.text = Some(TextTarget::EqPresetName);
            }
            "eq-add-filter" => self.eq_add_filter(cx),
            "eq-band" => {
                let p = self.eq.profile();
                p.gains = [6.0, 4.0, 2.0, -3.0, -4.5, 1.0, 3.5, 5.0, 2.0, -1.5];
                p.preset.clear();
                self.eq_ui.held = Some(4);
            }
            other => {
                if let Some(path) = other.strip_prefix("eq-import:") {
                    self.eq_import_path(std::path::Path::new(path), cx);
                } else if let Some(o) = other.strip_prefix("eq-output:") {
                    self.eq.output = o.to_string();
                } else if other == "eq-save" {
                    self.eq.save();
                } else {
                    return false;
                }
            }
        }
        cx.notify();
        true
    }
}

fn name_taken(name: &str) -> bool {
    BUILT_IN_PRESETS.iter().any(|p| p.name.eq_ignore_ascii_case(name))
}

fn plot_fraction(position: Point<Pixels>, bounds: Bounds<Pixels>) -> (f32, f32) {
    let w = f32::from(bounds.size.width).max(1.0);
    let h = f32::from(bounds.size.height).max(1.0);
    (f32::from(position.x - bounds.origin.x).clamp(0.0, w) / w, f32::from(position.y - bounds.origin.y).clamp(0.0, h) / h)
}

/// The dashed legend swatch.
fn dashes(t: &crate::theme::Theme) -> gpui::Div {
    let dash = || div().w(t.s(5.0)).h(px(2.0)).bg(t.t2()).flex_none();
    div().flex().flex_row().gap(t.s(3.0)).w(t.s(22.0)).overflow_hidden().child(dash()).child(dash()).child(dash())
}
