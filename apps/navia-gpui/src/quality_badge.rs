//! The audio quality badge beside the time in the players, and its details popover. It shows
//! Lossless, Hi-Res Lossless, or a lossy format and bitrate. When the server transcodes, it
//! shows what is actually playing with a small conversion mark.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{AnyElement, Bounds, Context, FontWeight, Hsla, Pixels, Window, anchored, deferred, div, point, px, relative, svg};
use navia_core::audio_quality::{Delivery, FileFacts, Source, Tier};

use crate::i18n::{fmt_int, tr, trf};
use crate::shell::Shell;
use crate::theme::size::*;
use crate::ui::icon;
use crate::ui_controls::track_bounds;

/// How long the pointer rests on the badge before the details open, or away before they close.
const HOVER_OPEN: Duration = Duration::from_millis(350);
const HOVER_CLOSE: Duration = Duration::from_millis(180);

/// The badge's popover, opened by hover or pinned by a click.
#[derive(Default)]
pub struct QualityPopover {
    hovered: bool,
    pinned: bool,
    generation: u64,
    badge: Rc<RefCell<Bounds<Pixels>>>,
    facts: Option<(usize, Option<FileFacts>)>,
}

pub struct QualityInfo {
    pub facts: FileFacts,
    pub delivery: Option<Delivery>,
    /// the output device's own rate, where the output can tell
    pub device_rate: Option<u32>,
}

pub(crate) fn khz(hz: u32) -> String {
    let k = hz as f64 / 1000.0;
    let text = if (k - k.round()).abs() < 0.05 { format!("{}", k.round() as u32) } else { format!("{k:.1}") };
    trf("{n} kHz", &[("n", &text)])
}

pub(crate) fn channels(n: u32) -> String {
    match n {
        1 => tr("Mono").to_string(),
        2 => tr("Stereo").to_string(),
        6 => "5.1".to_string(),
        8 => "7.1".to_string(),
        n => trf("{n} channels", &[("n", &fmt_int(n as i64))]),
    }
}

/// Where the audible song comes from, in words, for the popover and Get Info.
pub fn source_label(source: Source) -> &'static str {
    match source {
        Source::Download => tr("Downloaded copy"),
        Source::Cache => tr("Song cache on this device"),
        Source::Stream => tr("Streaming from the server"),
    }
}

pub(crate) fn kbps(n: u32) -> String {
    trf("{n} kbps", &[("n", &fmt_int(n as i64))])
}

/// the file in one line, like "FLAC · 24-bit · 96 kHz · Stereo"; the bitrate only for lossy files
pub fn detail_line(f: &FileFacts) -> String {
    let mut parts = vec![f.format_name()];
    parts.extend(f.bit_depth.map(|b| trf("{n}-bit", &[("n", &b.to_string())])));
    parts.extend(f.sample_rate.map(khz));
    parts.extend(f.kbps.filter(|_| !f.is_lossless()).map(kbps));
    parts.extend(f.channels.map(channels));
    parts.join(" · ")
}

impl QualityInfo {
    /// the format and bitrate the server converts to; a bitrate of 0 is one it didn't say
    pub(crate) fn transcode(&self) -> Option<(String, Option<u32>)> {
        self.delivery.as_ref().and_then(|d| d.transcode).map(|(codec, kbps)| (codec.label().to_string(), (kbps > 0).then_some(kbps)))
    }

    /// what's heard, like "Original · 48 kHz output"
    pub fn playing_line(&self) -> String {
        let mut parts = vec![match self.transcode() {
            Some((format, Some(n))) => trf("Converted to {format} {bitrate}", &[("format", &format), ("bitrate", &kbps(n))]),
            Some((format, None)) => trf("Converted to {format}", &[("format", &format)]),
            None => tr("Original").to_string(),
        }];
        parts.extend(self.device_rate.map(|r| trf("{rate} output", &[("rate", &khz(r))])));
        parts.join(" · ")
    }

    /// The badge's text. The bar says Hi-Res for Hi-Res Lossless, to save room.
    pub fn label(&self, short: bool) -> String {
        if let Some((format, kbps)) = self.transcode() {
            return kbps.map_or(format.clone(), |k| format!("{format} {k}"));
        }
        match self.facts.tier() {
            Tier::HiRes if short => tr("Hi-Res").to_string(),
            Tier::HiRes => tr("Hi-Res Lossless").to_string(),
            Tier::Lossless => tr("Lossless").to_string(),
            Tier::Lossy => match self.facts.kbps {
                Some(k) => format!("{} {k}", self.facts.format_name()),
                None => self.facts.format_name(),
            },
        }
    }
}

impl Shell {
    /// What the badge shows for the current song. `None` for a station, offline, or when the
    /// server didn't report the file type.
    pub fn quality_info(&mut self) -> Option<QualityInfo> {
        if self.player.station.is_some() {
            return None;
        }
        let song_ix = self.player.song_ix?;
        let core = self.core.as_ref()?;
        let facts = match &self.quality_pop.facts {
            Some((ix, facts)) if *ix == song_ix => facts.clone(),
            _ => {
                let facts = core.lib.audio_file(song_ix as u32);
                self.quality_pop.facts = Some((song_ix, facts.clone()));
                facts
            }
        }?;
        let delivery = core.player.delivered().map(|d| Delivery::of(&d));
        Some(QualityInfo { facts, delivery, device_rate: self.player.device_rate })
    }

    fn quality_hover(&mut self, on: bool, cx: &mut Context<Self>) {
        self.quality_pop.generation += 1;
        let generation = self.quality_pop.generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(if on { HOVER_OPEN } else { HOVER_CLOSE }).await;
            let _ = this.update(cx, |this, cx| {
                if this.quality_pop.generation == generation && this.quality_pop.hovered != on {
                    this.quality_pop.hovered = on;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub fn quality_pop_pinned(&self) -> bool {
        self.quality_pop.pinned
    }

    /// as a click on the badge does
    pub fn pin_quality_popover(&mut self, cx: &mut Context<Self>) {
        self.quality_pop.pinned = !self.quality_pop.pinned;
        self.quality_pop.hovered = false;
        self.quality_pop.generation += 1;
        cx.notify();
    }

    pub fn quality_pop_open(&self) -> bool {
        self.quality_pop.pinned || self.quality_pop.hovered
    }

    /// Esc and a click elsewhere close the details. True when they were open.
    pub fn close_quality_popover(&mut self, cx: &mut Context<Self>) -> bool {
        let open = self.quality_pop.pinned || self.quality_pop.hovered;
        self.quality_pop.pinned = false;
        self.quality_pop.hovered = false;
        self.quality_pop.generation += 1;
        if open {
            cx.notify();
        }
        open
    }

    /// `bar` is the bar's smaller, dimmer badge; `ink` is the time row's own color, which it takes.
    /// `narrow` keeps only the lossless wave where there is one.
    pub fn quality_badge(&mut self, bar: bool, narrow: bool, ink: Hsla, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = self.theme;
        let info = self.quality_info()?;
        let transcoded = info.transcode().is_some();
        let text = if bar { 11.5 } else { 12.0 };
        let waves = match info.facts.tier() {
            _ if transcoded => 0,
            Tier::HiRes => 2,
            Tier::Lossless => 1,
            Tier::Lossy => 0,
        };
        let open = self.quality_pop.pinned || self.quality_pop.hovered;
        let color: Hsla = if bar { if open { t.t1().into() } else { t.t2().into() } } else { ink };
        // The outline is the text color at 34%, lighter while transcoding.
        let outline = Hsla { a: color.a * if transcoded { 0.18 } else { 0.34 }, ..color };
        let store = self.quality_pop.badge.clone();
        let el = div()
            .id(if bar { "quality-badge-bar" } else { "quality-badge" })
            .relative()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap(t.s(4.0))
            .h(t.s(if bar { 17.0 } else { 18.0 }))
            .px(t.s(if bar { 5.0 } else { 6.0 }))
            .rounded(t.s(5.0))
            .border_1()
            .border_color(outline)
            .text_size(t.text(text))
            .font_weight(FontWeight::MEDIUM)
            .line_height(relative(1.0))
            .text_color(color)
            .opacity(if bar || open { 1.0 } else { 0.85 })
            .when(open, |d| d.bg(t.hover()))
            .hover(|s| s.bg(t.hover()).opacity(1.0))
            .child(track_bounds(store))
            .when(transcoded, |d| d.child(icon(&t, "arrows-clockwise-bold", 10.0, Hsla { a: color.a * 0.75, ..color })))
            .when(waves > 0, |d| d.child(lossless_glyph(&t, text, waves, color)))
            .when(!(narrow && waves > 0), |d| d.child(info.label(bar)))
            .on_hover(cx.listener(|this, on: &bool, _, cx| {
                if !this.quality_pop.pinned {
                    this.quality_hover(*on, cx);
                }
            }))
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                this.pin_quality_popover(cx);
            }));
        Some(el.into_any_element())
    }

    /// The details popover above the badge, drawn in the shared float layer.
    pub fn render_quality_popover(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !(self.quality_pop.pinned || self.quality_pop.hovered) {
            return None;
        }
        let t = self.theme;
        let Some(info) = self.quality_info() else {
            self.quality_pop.pinned = false;
            self.quality_pop.hovered = false;
            return None;
        };
        let badge = *self.quality_pop.badge.borrow();
        if badge.size.width <= px(0.0) {
            return None;
        }
        // a label column as wide as its longest label, then the values; one line each keeps the rows level
        let mut rows: Vec<(&'static str, String)> = vec![(tr("File"), detail_line(&info.facts)), (tr("Playing"), info.playing_line())];
        rows.extend(info.delivery.as_ref().map(|d| (tr("From||where a song plays from"), source_label(d.source).to_string())));
        let column = |cells: Vec<String>, dim: bool| div().flex().flex_col().flex_none().gap(t.s(QUALITY_POP_ROW_GAP)).whitespace_nowrap().when(dim, |d| d.text_color(t.t2())).children(cells);
        let rows = div()
            .flex()
            .flex_row()
            .gap(t.s(QUALITY_POP_COL_GAP))
            .child(column(rows.iter().map(|(l, _)| l.to_string()).collect(), true))
            .child(column(rows.into_iter().map(|(_, v)| v).collect(), false));
        let panel = crate::ui::float_panel(&t)
            .id("quality-popover")
            .max_w(t.s(QUALITY_POP_MAX_W))
            .pt(t.s(QUALITY_POP_PAD_TOP))
            .px(t.s(QUALITY_POP_PAD_X))
            .pb(t.s(QUALITY_POP_PAD_BOTTOM))
            .text_size(t.text(TEXT_XS))
            .line_height(relative(1.35))
            .text_color(t.t1())
            .child(rows)
            .on_hover(cx.listener(|this, on: &bool, _, cx| {
                if !this.quality_pop.pinned {
                    this.quality_hover(*on, cx);
                }
            }))
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                if this.quality_pop.pinned {
                    this.close_quality_popover(cx);
                }
            }));
        // Centered above the badge, shifted to stay inside the window.
        let center = badge.center().x;
        let top = badge.top() - t.s(QUALITY_POP_GAP);
        let _ = window;
        Some(
            deferred(anchored().position(point(center, top)).anchor(gpui::Anchor::BottomCenter).snap_to_window_with_margin(self.float_margin_by(t.s(QUALITY_POP_EDGE))).child(panel))
                .with_priority(9)
                .into_any_element(),
        )
    }
}

/// the lossless wave, once for a lossless file and twice, stacked, for a hi-res one
fn lossless_glyph(t: &crate::theme::Theme, text: f32, waves: usize, color: Hsla) -> AnyElement {
    let size = t.text(text * QUALITY_GLYPH);
    let gap = t.text(text * QUALITY_GLYPH_TWIN_GAP);
    let wave = |dy: Pixels| svg().path(crate::assets::icon("wave-sine-bold.svg")).absolute().left_0().top(dy).size(size).text_color(color);
    let mut glyph = div().relative().flex_none().size(size);
    glyph = if waves > 1 { glyph.child(wave(gap * -0.5)).child(wave(gap * 0.5)) } else { glyph.child(wave(px(0.0))) };
    glyph.into_any_element()
}
