//! The right dock: baked background and artwork, Featured Artists, and transport controls.

use crate::ui::FullTextTip;
use crate::i18n::{tr, trf};
use gpui::prelude::*;
use crate::focus::FocusStop;
use gpui::{AnyElement, Context, FontWeight, MouseButton, SharedString, div, point, px, relative};

use crate::color::{Srgb, player_fg, player_fg_dim, player_vivid};
use crate::persist::{DockView, PlayerMode};
use crate::shell::{Drag, Layout, Shell};
use crate::theme::size::*;
use crate::theme::motion;
use crate::ui::{clock, icon};
use crate::view_topbar::track_bounds;
use crate::view_window_buttons::Side;

/// The seek bar's forms. `Stacked` (dock, expanded) has the times under it, `Inline` (bar) beside it, and `Thin` (mini
/// player) is a hairline with a time chip over the playhead on hover.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ScrubVariant {
    Stacked,
    Inline,
    Thin,
}

impl Shell {
    /// for internet radio.
    fn render_live_scrub(&self, variant: ScrubVariant, rest: gpui::Rgba, color: gpui::Hsla) -> AnyElement {
        let t = self.theme;
        let line = || div().h(t.s(4.0)).my(t.s(5.0)).rounded(px(2.0)).bg(rest);
        let tag = div().font_weight(FontWeight::SEMIBOLD).text_size(t.text(TEXT_SM)).text_color(color).child(tr("Live"));
        match variant {
            ScrubVariant::Stacked => div()
                .w_full()
                .child(line())
                .child(div().flex().flex_row().justify_between().items_center().mt(t.s(4.0)).text_size(t.text(TEXT_SM)).text_color(color).child(tag).when_some(self.private_badge(), |d, b| d.child(b)).child(div().child(clock(self.player.position as u32))))
                .into_any_element(),
            ScrubVariant::Inline => div().flex().flex_row().items_center().gap(t.s(12.0)).w_full().child(tag).child(line().flex_1().min_w(px(0.0)).my(px(0.0))).into_any_element(),
            ScrubVariant::Thin => div().w_full().h(t.s(14.0)).flex().items_center().child(div().w_full().h(t.s(SCRUB_THIN)).rounded(t.s(SCRUB_THIN / 2.0)).bg(rest)).into_any_element(),
        }
    }

    /// The quiet mark while Private mode is on.
    pub fn private_badge(&self) -> Option<AnyElement> {
        if !self.private_mode {
            return None;
        }
        let t = self.theme;
        Some(div().flex().flex_row().items_center().gap(t.s(4.0)).text_size(t.text(TEXT_XS)).opacity(0.8).child(icon(&t, "eye-slash-regular", 14.0, t.t_on_art())).child(tr("Private")).into_any_element())
    }

    /// `color` fills the played part (`--player-fg`); `text` is the times (`--player-fg-dim`, opaque).
    pub fn render_scrubber(&mut self, variant: ScrubVariant, rest: gpui::Rgba, color: gpui::Hsla, text: gpui::Hsla, feel: crate::ui::Feel, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        if self.player.station.is_some() {
            return self.render_live_scrub(variant, rest, text);
        }
        let (pos, dur) = match self.current_song() {
            Some(s) => (self.player.position, s.duration_s.max(1) as f32),
            None => (0.0, 1.0),
        };
        let frac = (pos / dur).clamp(0.0, 1.0);
        let waiting = self.player.waiting_for_server && self.player.status == navia_core::audio::player::PlayerStatus::Buffering;
        let waiting_label = move || div().flex_none().font_weight(FontWeight::MEDIUM).text_size(t.text(TEXT_SM)).child(tr("Waiting for server"));
        let thin = variant == ScrubVariant::Thin;
        // The thumb, and the thin bar's time chip, fade in on hover and stay while dragging.
        let dragging = matches!(self.dragging, Some(Drag::Seek));
        let hover = crate::ui::ink(if thin { "seek-thin" } else { "seek" }).amount.max(if dragging { 1.0 } else { 0.0 });
        let (rest_h, hover_h, shown) = match variant {
            ScrubVariant::Thin => (SCRUB_THIN, SCRUB_THIN_HOVER, hover),
            ScrubVariant::Inline => (BAR_SCRUB_REST, BAR_SCRUB_HOVER, self.bar_raise.shown.max(if dragging { 1.0 } else { 0.0 })),
            ScrubVariant::Stacked => (SCRUB_LINE, SCRUB_LINE_HOVER, hover),
        };
        let h = rest_h + (hover_h - rest_h) * shown;
        let (bar_h, radius) = (t.s(h), t.s(h / 2.0));
        let track = div()
            .id(if thin { "seek-thin" } else { "seek" })
            .group("seek")
            .relative()
            .h(t.s(SCRUB_SLOT))
            .flex()
            .items_center()
            .flex_1()
            .min_w(px(0.0))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                this.dragging = Some(Drag::Seek);
                this.seek_to(event.position.x, cx);
            }))
            .when(thin, |d| d.occlude().child(crate::ui::hover_track("seek-thin")))
            .when(!thin, |d| d.child(crate::ui::hover_track("seek")))
            .child(track_bounds(self.seek_bounds.clone()))
            .child(
                div()
                    .relative()
                    .h(bar_h)
                    .w_full()
                    .rounded(radius)
                    .bg(rest)
                    .child(div().absolute().left(px(0.0)).top(px(0.0)).bottom(px(0.0)).w(relative(frac)).rounded(radius).bg(color)),
            )
            .when(thin && hover > 0.0, |d| {
                // The times in a floating chip above the playhead, kept clear of the bar's ends.
                let width = f32::from(self.seek_bounds.borrow().size.width);
                let edge = f32::from(t.s(SCRUB_CHIP_EDGE)).min(width / 2.0);
                let at = (frac * width).clamp(edge, (width - edge).max(edge));
                let chip_w = t.s(SCRUB_CHIP_EDGE * 2.0);
                d.child(
                        div()
                            .absolute()
                            .bottom(t.s(SCRUB_SLOT) - (t.s(SCRUB_SLOT) - bar_h) / 2.0 + px(2.0))
                            .left(px(at) - chip_w / 2.0)
                            .w(chip_w)
                            .flex()
                            .justify_center()
                            .opacity(hover)
                            .child(
                                crate::ui::float_chip(&t)
                                    .child(format!("{} / {}", clock(pos as u32), self.time_end(pos, dur))),
                            ),
                    )
            });
        match variant {
            ScrubVariant::Thin => track.into_any_element(),
            ScrubVariant::Stacked => {
                // Between the times: the Private mode mark and the quality badge.
                let middle: AnyElement = if waiting {
                    waiting_label().into_any_element()
                } else {
                    div().flex().flex_row().items_center().gap(t.s(10.0)).children(self.private_badge()).children(self.quality_badge(false, false, text, cx)).into_any_element()
                };
                div()
                    .w_full()
                    .child(track)
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .justify_between()
                            .items_center()
                            .mt(t.s(4.0))
                            .text_size(t.text(TEXT_SM))
                            .text_color(text)
                            .child(div().child(clock(pos as u32)))
                            .child(middle)
                            .child(self.time_toggle("time-end", pos, dur, feel, cx)),
                    )
                    .into_any_element()
            }
            // the bar's times appear over the title while hovered (`bar_times`).
            ScrubVariant::Inline => div().flex().w_full().child(track).into_any_element(),
        }
    }

    /// the length, or what's left (`-1:23`) once the user chose that.
    pub fn time_end(&self, pos: f32, dur: f32) -> SharedString {
        if self.prefs.time_remaining { format!("\u{2212}{}", clock((dur as u32).saturating_sub(pos as u32))).into() } else { clock(dur as u32) }
    }

    /// `reveal` is 0 to 1. the caller places the row.
    pub fn bar_times(&mut self, reveal: f32, text: gpui::Hsla, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let t = self.theme;
        if reveal <= 0.0 || self.player.station.is_some() {
            return None;
        }
        let (pos, dur) = match self.current_song() {
            Some(s) => (self.player.position, s.duration_s.max(1) as f32),
            None => (0.0, 1.0),
        };
        let waiting = self.player.waiting_for_server && self.player.status == navia_core::audio::player::PlayerStatus::Buffering;
        let middle: Option<AnyElement> = if waiting {
            Some(div().font_weight(FontWeight::MEDIUM).child(tr("Waiting for server")).into_any_element())
        } else {
            self.quality_badge(true, false, text, cx)
        };
        Some(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .text_size(t.text(TEXT_SM))
                .text_color(text)
                .opacity(reveal)
                .child(div().child(clock(pos as u32)))
                .children(middle)
                .child(self.time_toggle("bar-time-end", pos, dur, crate::ui::Feel::BAR, cx)),
        )
    }

    /// a click switches length and time left, remembered.
    fn time_toggle(&mut self, id: &'static str, pos: f32, dur: f32, feel: crate::ui::Feel, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let t = self.theme;
        let label = self.time_end(pos, dur);
        let ink = crate::ui::ink(id);
        crate::ui::pressable_in(div().id(id).relative().child(crate::ui::active_pill(&t, ink, feel)), id, feel).stop(&t).on_click(cx.listener(|this, _, _, cx| {
            this.prefs.time_remaining = !this.prefs.time_remaining;
            this.prefs.save();
            cx.notify();
        })).child(label)
    }

    /// links, or plain text for a station. `sizes` are in tokens, `line` a line-height factor.
    #[allow(clippy::too_many_arguments)]
    pub fn player_names(&mut self, song: &crate::model::Song, keys: (&'static str, &'static str), host: crate::marquee::Host, sizes: (f32, f32), line: f32, colors: (gpui::Hsla, gpui::Hsla), opacity: f32, cx: &mut Context<Self>) -> (AnyElement, AnyElement) {
        let (title_color, artist_color) = colors;
        use crate::marquee::Spec;
        let t = self.theme;
        let station = self.player.station.is_some();
        let title_target = (!station && !song.album_id.is_empty()).then(|| crate::song_links::LinkTarget::Album(song.album_id.clone()));
        let mut artists: Vec<(SharedString, Option<crate::song_links::LinkTarget>)> = Vec::new();
        let names: Vec<SharedString> = if station || song.artists.len() <= 1 { vec![song.artist.clone()] } else { song.artists.clone() };
        for (k, name) in names.into_iter().enumerate() {
            if k > 0 {
                artists.push((", ".into(), None));
            }
            let target = (!station).then(|| self.artist_named(&name).map(|a| crate::song_links::LinkTarget::Artist(a.id.clone()))).flatten();
            artists.push((name, target));
        }
        let spec = |pieces, size: f32, weight, color| Spec { pieces, color, size: t.text(size), weight, line_height: t.text(size) * line };
        let title = self.marquee(keys.0, host, spec(vec![(song.title.clone(), title_target)], sizes.0, FontWeight::MEDIUM, title_color), opacity, cx);
        let artists = self.marquee(keys.1, host, spec(artists, sizes.1, FontWeight::NORMAL, artist_color), opacity, cx);
        (title, artists)
    }

    /// the player's song as a cross-fade key (`SONG_FADE`): the one it replaces is kept whole to draw while it fades out.
    pub fn song_swap(&self, id: &'static str, song: &crate::model::Song) -> (crate::ui::Swapping<SongById>, AnyElement) {
        crate::ui::swap(id, &SongById(song.clone()), motion::SONG_FADE)
    }

    /// a leaving song's title and artists, plain, for the moment it fades out over the new one's.
    pub fn leaving_names(&self, song: &crate::model::Song, sizes: (f32, f32), line: f32, colors: (gpui::Hsla, gpui::Hsla), opacity: f32) -> AnyElement {
        let t = self.theme;
        let line_of = |text: SharedString, size: f32, weight, color| div().min_w(px(0.0)).truncate().text_size(t.text(size)).line_height(t.text(size) * line).font_weight(weight).text_color(color).child(text);
        div()
            .absolute()
            .inset_0()
            .flex()
            .flex_col()
            .justify_center()
            .opacity(opacity)
            .child(line_of(song.title.clone(), sizes.0, FontWeight::MEDIUM, colors.0))
            .child(line_of(song.artist.clone(), sizes.1, FontWeight::NORMAL, colors.1))
            .into_any_element()
    }

    pub fn volume_icon(&self) -> &'static str {
        let volume = self.volume_percent();
        // four steps, so the waves follow the level; the button cross-fades between them
        if self.prefs.muted || volume == 0.0 {
            "speaker-slash-regular"
        } else if volume < 100.0 / 3.0 {
            "speaker-none-regular"
        } else if volume < 200.0 / 3.0 {
            "speaker-low-regular"
        } else {
            "speaker-high-regular"
        }
    }

    /// `color` is the icons (`--player-fg-dim`), `fill` the slider's level (`--player-fg`).
    pub fn render_volume_row(&mut self, color: gpui::Hsla, fill: gpui::Hsla, feel: crate::ui::Feel, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let name = self.volume_icon();
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(t.s(6.0))
            .mt(t.s(PLAYER_VOLUME_GAP))
            .mx(t.s(-8.0))
            .child(
                crate::ui::player_button_fading(&t, "mute", if self.prefs.muted { tr("Unmute") } else { tr("Mute") }, name, 19.0, color, fill, feel).stop(&t).on_click(cx.listener(|this, _, _, cx| {
                    this.toggle_mute(cx);
                })),
            )
            .child(self.render_volume_slider(fill, t.player_track(), cx))
            // Output sits at the end of the volume row.
            .child(
                // in the accent once it plays.
                {
                    let (glyph, accent) = self.output_glyph();
                    let (rest, hover) = if accent { (t.accent_hsla(), t.accent_hsla()) } else { (color, fill) };
                    crate::ui::menu_on_press(crate::ui::player_menu_button(&t, "output", tr("Output"), glyph, 19.0, rest, hover, feel).stop(&t), cx, |this, at, cx| this.open_output_menu(at - point(px(0.0), this.theme.s(70.0)), cx))
                },
            )
            .into_any_element()
    }

    /// The volume slider. Drag, click or scroll on it. `color` fills it over `rest`. Only one is on screen at a time.
    pub fn render_volume_slider(&mut self, color: gpui::Hsla, rest: gpui::Rgba, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let frac = if self.prefs.muted { 0.0 } else { self.volume_percent() / 100.0 };
        let hover = crate::ui::ink("volume").amount.max(if matches!(self.dragging, Some(Drag::Volume)) { 1.0 } else { 0.0 });
        let h = SCRUB_LINE + (SCRUB_LINE_HOVER - SCRUB_LINE) * hover;
        // on AirPlay it drives the receiver's volume, and says so.
        let receiver_tip = self.active_airplay().map(|o| SharedString::from(crate::i18n::trf("Volume on {device}", &[("device", o.name.as_str())])));
        div()
                    .id("volume")
                    .when_some(receiver_tip, |d, tip| d.tooltip(crate::ui::tip(&t, tip)))
                    .relative()
                    .flex_1()
                    .h(t.s(SCRUB_SLOT))
                    .flex()
                    .items_center()
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                        this.dragging = Some(Drag::Volume);
                        this.set_volume_at(event.position.x, cx);
                    }))
                    .on_scroll(cx.listener(|this, event: &gpui::ScrollEvent, window, cx| {
                        let dy = f32::from(event.delta.pixel_delta(window.line_height()).y);
                        if dy != 0.0 {
                            this.change_volume(if dy > 0.0 { 5.0 } else { -5.0 }, cx);
                        }
                    }))
                    .child(crate::ui::hover_track("volume"))
                    .child(track_bounds(self.volume_bounds.clone()))
                    .child(
                        div()
                            .relative()
                            .h(t.s(h))
                            .w_full()
                            .rounded(t.s(h / 2.0))
                            .bg(rest)
                            .child(div().absolute().left(px(0.0)).top(px(0.0)).bottom(px(0.0)).w(relative(frac)).rounded(t.s(h / 2.0)).bg(color)),
                    )
            .into_any_element()
    }
}

/// The player's plain icon buttons: shuffle and repeat, plus the queue headings' one-shot actions.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ModeButton {
    Shuffle,
    Repeat,
    /// Shuffles Next in Queue once. Disabled with fewer than two songs.
    ShuffleQueue,
    /// Clear Queue (Next in Queue) and Clear <source> (Next from).
    ClearQueue,
    ClearList,
}

impl Shell {
    /// shared by the dock and the expanded player.
    pub fn render_transport(&mut self, icon_color: gpui::Hsla, feel: crate::ui::Feel, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let playing = self.player.playing;
        let transport_button = |id: &'static str, label: &'static str, name: &'static str, size: f32, enabled: bool| crate::ui::transport_button(&t, id, if enabled { tr(label) } else { tr(crate::ui::STATION_NO_SKIP) }, name, size, icon_color, enabled, feel);
        let station = self.player.station.is_some();
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .mt(t.s(PLAYER_TRANSPORT_GAP))
            .mx(t.s(-8.0))
            .child(self.render_mode_button(ModeButton::Shuffle, "shuffle", 24.0, icon_color, feel, cx))
            .child(transport_button("prev", "Previous", "rewind-fill", 32.0, !station).when(!station, |d| d.stop(&t).on_click(cx.listener(|this, _, _, cx| this.previous(cx)))).when(station, |d| crate::ui::locked(d).on_click(cx.listener(|this, _, _, cx| this.show_toast(tr(crate::ui::STATION_NO_SKIP), cx)))))
            .child(
                crate::ui::play_button(&t, "play", playing, 44.0, icon_color, feel).stop(&t).on_click(cx.listener(|this, _, _, cx| {
                    this.toggle_play(cx);
                })),
            )
            .child(transport_button("next", "Next", "fast-forward-fill", 32.0, !station).when(!station, |d| d.stop(&t).on_click(cx.listener(|this, _, _, cx| this.next(cx)))).when(station, |d| crate::ui::locked(d).on_click(cx.listener(|this, _, _, cx| this.show_toast(tr(crate::ui::STATION_NO_SKIP), cx)))))
            .child(self.render_mode_button(ModeButton::Repeat, "repeat", 24.0, icon_color, feel, cx))
            .into_any_element()
    }

    /// shuffle toggles, repeat cycles off, all, one; the queue's actions run once.
    pub fn render_mode_button(&mut self, which: ModeButton, id: &'static str, glyph: f32, color: gpui::Hsla, feel: crate::ui::Feel, cx: &mut Context<Self>) -> AnyElement {
        use navia_core::audio::player::Repeat;
        let t = self.theme;
        let station = self.player.station.is_some();
        let (on, one, name) = match which {
            ModeButton::Shuffle => (self.player.shuffle, false, "shuffle-bold"),
            ModeButton::Repeat => (self.player.repeat != Repeat::Off, self.player.repeat == Repeat::One, "repeat-bold"),
            ModeButton::ShuffleQueue => (false, false, "shuffle-bold"),
            ModeButton::ClearQueue | ModeButton::ClearList => (false, false, "broom-bold"),
        };
        let modes = matches!(which, ModeButton::Shuffle | ModeButton::Repeat);
        let disabled = which == ModeButton::ShuffleQueue && self.player.queued_run.min(self.up_next_len()) < 2;
        // The tooltip is the label, or why a station disables it.
        let label: SharedString = match which {
            _ if station && modes => tr(crate::ui::STATION_NO_MODES).into(),
            ModeButton::Shuffle => tr("Shuffle").into(),
            ModeButton::Repeat => tr(match self.player.repeat {
                Repeat::Off => "Repeat",
                Repeat::All => "Repeat all",
                Repeat::One => "Repeat one",
            })
            .into(),
            ModeButton::ShuffleQueue => tr("Shuffle Queue").into(),
            ModeButton::ClearQueue => tr("Clear Queue").into(),
            ModeButton::ClearList => match self.player.source.as_ref() {
                Some(s) => trf("Clear {name}", &[("name", &if s.radio { trf("{name} Radio", &[("name", &s.name)]) } else { s.name.clone() })]).into(),
                None => tr("Clear Up Next").into(),
            },
        };
        let box_size = glyph + 12.0;
        let (shown, ticker) = crate::ui::toggle_ease(id, on);
        let tint = crate::ui::mix_hsla(color, t.accent_soft_hsla(), shown);
        let ink = crate::ui::ink(id);
        let live = !(station && modes) && !disabled;
        div()
            .id(id)
            .child(ticker)
            .tooltip(crate::ui::tip(&t, label))
            .relative()
            .flex_none()
            .size(t.s(box_size))
            .flex()
            .items_center()
            .justify_center()
            .when(station && modes, |d| d.opacity(MODE_DISABLED).on_click(cx.listener(|this, _, _, cx| this.show_toast(tr(crate::ui::STATION_NO_MODES), cx))))
            .when(disabled, |d| d.opacity(MODE_DISABLED))
            .when(live, |d| {
                d.child(crate::ui::press_track(id, feel))
                    .opacity(ink.opacity(MODE_REST + (1.0 - MODE_REST) * shown))
                    .paint_scale(ink.press_scale_in(feel))
                    .stop(&t)
                    .on_click(cx.listener(move |this, _, _, cx| match which {
                        ModeButton::Shuffle => this.toggle_shuffle(cx),
                        ModeButton::Repeat => this.toggle_repeat(cx),
                        ModeButton::ShuffleQueue => this.shuffle_queued(cx),
                        ModeButton::ClearQueue => this.clear_queued(cx),
                        ModeButton::ClearList => this.clear_list(cx),
                    }))
            })
            .when(live, |d| d.child(crate::ui::active_fill(&t, ink, feel, (box_size, box_size), glyph)))
            .child(icon(&t, name, glyph, tint))
            .when(shown > 0.0, |d| d.child(div().absolute().bottom(px(0.0)).left(t.s(box_size / 2.0 - 2.0)).size(t.s(4.0)).rounded_full().bg(tint.opacity(shown))))
            .when(one, |d| {
                d.child(
                    div()
                        .absolute()
                        .top(t.s(1.0))
                        .right(px(0.0))
                        .min_w(t.s(13.0))
                        .h(t.s(13.0))
                        .px(t.s(2.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(t.s(7.0))
                        .bg(t.accent_soft_hsla())
                        .text_color(t.on_accent.hsla(1.0))
                        .text_size(t.s(9.5))
                        .font_weight(FontWeight::BOLD)
                        .line_height(relative(1.0))
                        .child("1"),
                )
            })
            .into_any_element()
    }

    /// The view switcher pill. `item_w` is one button's width in tokens, 50 unless a narrow dock needs less. `ink` is
    /// the icon color for the surface it sits on.
    pub fn render_view_switcher(&mut self, item_w: f32, ink: gpui::Hsla, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let shown = self.shown_view();
        let has_lyrics = self.may_have_lyrics();
        let items = [
            (DockView::Lyrics, "Lyrics", "nv-lyrics", "nv-lyrics"),
            (DockView::Artwork, "Artwork", "music-note-regular", "music-note-fill"),
            (DockView::Queue, "Queue", "list-bullets-regular", "list-bullets-regular"),
        ];
        let mut pill = div()
            .relative()
            .flex()
            .flex_row()
            .p(t.s(4.0))
            .rounded_full()
            .map(|d| crate::ui::glass_layers(d, &t, t.s(SWITCHER_H / 2.0), GLASS_BLUR, GLASS_SATURATION, t.switcher_glass(), t.switcher_glass(), vec![crate::ui::inset_ring(t.switcher_edge())]));
        for (i, (view, label, regular, filled)) in items.into_iter().enumerate() {
            let on = shown == view;
            let disabled = view == DockView::Lyrics && !has_lyrics;
            let (lit, ticker) = crate::ui::toggle_ease(("dock-view-on", i), on);
            let ink_state = crate::ui::ink(("dock-view", i));
            let fill = t.switcher_on();
            // they join the end of Next in Queue.
            let drop = (view == DockView::Queue).then(|| self.drop_mark("queue-tab", crate::song_drop::SongTarget::QueueEnd, R_MD, 0.0)).flatten();
            pill = pill.child(
                div()
                    .id(("dock-view", i))
                    .tooltip(crate::ui::tip(&t, if disabled { tr("No lyrics for this song") } else { tr(label) }))
                    .relative()
                    .children(drop)
                    .w(t.s(item_w))
                    .h(t.s(40.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .child(ticker)
                    .when(lit > 0.0, |d| d.bg(gpui::Rgba { a: fill.a * lit, ..fill }))
                    .opacity(if disabled { 0.3 } else { let rest = ink_state.opacity(SWITCHER_REST); rest + (1.0 - rest) * lit })
                    .when(!disabled, |d| d.child(crate::ui::hover_track(("dock-view", i))).paint_scale(ink_state.press_scale()))
                    .when(!disabled, |d| {
                        d.stop(&t).on_click(cx.listener(move |this, _, _, cx| this.set_view(view, cx)))
                    })
                    .child(icon(&t, if on { filled } else { regular }, 21.0, crate::ui::mix_hsla(ink, t.t_em(), lit))),
            );
        }
        pill.into_any_element()
    }

    pub fn render_dock(&mut self, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let Some(song) = self.current_song().cloned() else {
            return div().w(layout.dock_width).into_any_element();
        };
        let tint = self.tint();
        let vivid = player_vivid(tint);
        let fg: gpui::Hsla = player_fg(tint).hsla(1.0);
        let dim: gpui::Hsla = player_fg_dim(tint).hsla(1.0);
        let geometry = self.dock_geometry(layout);
        let radius = t.s(R_LG);
        let bake = self.dock_bake.clone().filter(|(id, _)| *id == song.album_id);
        let scale = layout.scale;
        let mut art_logical = px(geometry.art_h as f32 / scale);
        let mut stack_logical = px((geometry.reflect_top + geometry.reflect_h).max(geometry.art_h) as f32 / scale);
        // during a drag the last bake keeps its proportions instead of stretching.
        if let Some((_, baked)) = bake.as_ref().filter(|_| self.dock_resizing()) {
            let k = geometry.width as f32 / baked.geometry.width.max(1) as f32;
            art_logical = px(baked.geometry.art_h as f32 * k / scale);
            stack_logical = px((baked.geometry.reflect_top + baked.geometry.reflect_h).max(baked.geometry.art_h) as f32 * k / scale);
        }

        // The baked gradient, or a fallback fill until the first bake arrives.
        let background: AnyElement = match bake.as_ref() {
            Some((_, b)) => crate::ui::image(b.background.clone()).absolute().left(px(0.0)).top(px(0.0)).w(px(geometry.width as f32 / scale)).h(px(geometry.height as f32 / scale)).into_any_element(),
            None => div()
                .absolute()
                .inset_0()
                .bg(gpui::linear_gradient(
                    180.0,
                    gpui::linear_color_stop(vivid.hsla(1.0), 0.35),
                    gpui::linear_color_stop(vivid.mix(Srgb::hex(crate::theme::DOCK_SHADE), crate::theme::PLAYER_SHADE_MIX).hsla(1.0), 1.0),
                ))
                .into_any_element(),
        };

        // The head floats over the art with its own dark gradient so its icons read on any cover. With window buttons
        // in the top-right corner, the bar switch moves beside Expand.
        let corner_buttons = self.has_window_buttons(Side::Right);
        let head = div()
            .absolute()
            .top(px(0.0))
            .left(px(0.0))
            .right(px(0.0))
            .h(t.s(76.0))
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .px(t.s(12.0))
            .map(|d| Shell::drag_area(d, cx))
            .child(
                div()
                    .absolute()
                    .left(px(0.0))
                    .right(px(0.0))
                    .top(px(0.0))
                    .h(t.s(100.0))
                    .bg(gpui::linear_gradient(180.0, gpui::linear_color_stop(gpui::rgba(crate::theme::DOCK_HEAD_SHADE), 0.0), gpui::linear_color_stop(gpui::transparent_black(), 1.0))),
            )
            ;
        let expand = crate::ui::player_button(&t, "expand", tr("Expand player"), "nv-expand", 22.0, t.t_on_art(), t.t_on_art(), crate::ui::Feel::PLAYER)
            .relative()
            .occlude()
            .stop(&t).on_click(cx.listener(|this, _, _, cx| {
                this.expanded = true;
                cx.notify();
            }));
        let mini = crate::ui::player_button(&t, "dock-mini", tr("Mini Player"), "picture-in-picture-regular", 22.0, t.t_on_art(), t.t_on_art(), crate::ui::Feel::PLAYER)
            .relative()
            .occlude()
            .stop(&t).on_click(cx.listener(|this, _, _, cx| this.open_mini_player(cx)));
        let to_bar = crate::ui::player_button(&t, "to-bar", tr("Move player to the bottom"), "nv-bar", 22.0, t.t_on_art(), t.t_on_art(), crate::ui::Feel::PLAYER)
            .relative()
            .occlude()
            .stop(&t).on_click(cx.listener(|this, _, _, cx| this.set_mode(PlayerMode::Bar, cx)));
        // Expand and the mini player on the left. The bar switch goes on the right, or beside them when window buttons
        // take the corner.
        let left = div().relative().flex().flex_row().items_center().gap(t.s(4.0)).child(expand).child(mini);
        let head = if corner_buttons {
            head.child(left.child(to_bar)).child(self.render_window_buttons(Side::Right, true))
        } else {
            head.child(left).child(to_bar)
        };
        // Centered in the dock when it fits. On a narrow dock with window buttons in the corner, it sits between the
        // two groups with narrower buttons.
        let dock_tokens = f32::from(layout.dock_width) / t.token_scale();
        let (left_group, right_group) = if corner_buttons { (12.0 + ICON_W * 3.0 + 4.0 * 2.0, 12.0 + self.window_buttons_width(Side::Right)) } else { (12.0 + ICON_W * 2.0 + 4.0, 12.0 + ICON_W) };
        const SWITCHER_FULL: f32 = 50.0 * 3.0 + 8.0;
        const SWITCHER_MARGIN: f32 = 8.0;
        let fits_centered = dock_tokens / 2.0 - left_group.max(right_group) - SWITCHER_MARGIN >= SWITCHER_FULL / 2.0;
        let room = dock_tokens - left_group - right_group - SWITCHER_MARGIN * 2.0;
        let item_w = if fits_centered { 50.0 } else { ((room - 8.0) / 3.0).clamp(30.0, 50.0) };
        let switcher = self.render_view_switcher(item_w, t.t_on_art().into(), cx);
        let total = item_w * 3.0 + 8.0;
        let left_tokens = if fits_centered { (dock_tokens - total) / 2.0 } else { left_group + SWITCHER_MARGIN + (room - total).max(0.0) / 2.0 };
        let head = head.child(div().absolute().top(px(0.0)).bottom(px(0.0)).left(t.s(left_tokens)).flex().items_center().child(div().occlude().child(switcher)));

        let art_stack = bake.as_ref().map(|(_, b)| b.art_stack.clone());
        let mut body_of = |this: &mut Self, view: DockView| -> AnyElement {
            match view {
                DockView::Artwork => this.render_dock_artwork(art_stack.clone(), art_logical, stack_logical, geometry.width as f32 / scale, &song, layout, cx),
                // under the expanded player only one of them may lay out the lyrics and queue.
                DockView::Lyrics | DockView::Queue if this.expanded => div().into_any_element(),
                DockView::Lyrics => this.render_lyrics_panel(false, layout.dock_width, t.s(76.0), layout, cx),
                DockView::Queue => this.render_queue_panel(t.s(76.0), layout.scale, cx),
            }
        };
        let view = self.shown_view();
        let (swap, swap_ticker) = crate::ui::swap("dock-view", &view, motion::VIEW_FADE);
        let rise = t.s(motion::VIEW_RISE);
        let mut body = div().relative().flex_1().min_h(px(0.0)).child(swap_ticker);
        if let Some(old) = swap.leaving {
            body = body.child(crate::ui::swap_layer(body_of(self, old), swap.shown, rise, false));
        }
        body = body.child(crate::ui::swap_layer(body_of(self, view), swap.shown, rise, true));

        let foot = div()
            .relative()
            .flex_none()
            .pt(t.s(12.0))
            .px(t.s(32.0))
            .pb(t.s(28.0))
            .when_some(self.render_player_chip(false, dim, fg, cx), |d, offer| d.child(offer))
            .child(self.render_meta_row(&song, fg, dim, crate::ui::Feel::PLAYER, cx))
            .child(self.render_scrubber(crate::view_dock::ScrubVariant::Stacked, t.player_track(), fg, dim, crate::ui::Feel::PLAYER, cx))
            .child(self.render_transport(fg, crate::ui::Feel::PLAYER, cx))
            .child(self.render_volume_row(dim, fg, crate::ui::Feel::PLAYER, cx));

        div()
            .id("dock")
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .w(layout.dock_width)
            .mr(t.s(WINDOW_INSET))
            .my(t.s(WINDOW_INSET))
            .rounded(radius)
            .overflow_hidden()
            .text_color(fg)
            .shadow(vec![crate::ui::shadow_dock(&t)])
            // Songs dragged over the dock leave their list. The dock's queue and queue tab accept them.
            .children(self.drop_mark("dock", crate::song_drop::SongTarget::Surface, R_LG, 0.0))
            .child(background)
            .child(body)
            .child(foot)
            .child(head)
            .child(
                // The 1px light edge goes on top of the artwork because inset shadows draw under children.
                div().absolute().inset_0().rounded(radius).shadow(vec![crate::ui::inset_ring(gpui::rgba(crate::theme::GLASS_EDGE))]),
            )
            .into_any_element()
    }

    /// The strip on the dock's left edge that resizes it. It doesn't occlude, so scrolling still reaches what's under
    /// it.
    pub fn render_dock_grab(&mut self, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let width = t.s(DOCK_GRAB_W);
        let dock_left = layout.viewport.width - t.s(WINDOW_INSET) - layout.dock_width;
        div()
            .id("dock-grab")
            .absolute()
            .left(dock_left - width / 2.0)
            .top(t.s(WINDOW_INSET))
            .bottom(t.s(WINDOW_INSET))
            .w(width)
            .cursor(gpui::CursorStyle::ResizeColumn)
            .on_mouse_down(MouseButton::Left, cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                cx.stop_propagation();
                if event.click_count >= 2 {
                    this.dragging = None;
                    this.reset_dock_width(cx);
                } else {
                    this.begin_dock_drag(event.position.x - this.edge.left, this.edge.content_size(window.viewport_size()), window.scale_factor());
                    cx.notify();
                }
            }))
            .into_any_element()
    }

    /// Edge fades on the Featured Artists row where there's more to scroll, and the shelf arrows on hover.
    fn featured_edges(&mut self, people: gpui::Stateful<gpui::Div>, handle: &gpui::ScrollHandle, color: Srgb, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let offset = -f32::from(handle.offset().x);
        let max = f32::from(handle.max_offset().x);
        let (more_left, more_right) = (offset > 2.0, offset < max - 2.0);
        // 10 tokens fully covered at the edge, then 30 to fade out.
        let fade = |left: bool| {
            let (solid, ramp) = (t.s(10.0), t.s(30.0));
            let gradient = |from: f32, to: f32| gpui::linear_gradient(90.0, gpui::linear_color_stop(color.hsla(from), 0.0), gpui::linear_color_stop(color.hsla(to), 1.0));
            let row = div().absolute().top(px(0.0)).bottom(px(0.0)).flex().flex_row();
            if left {
                row.left(px(0.0)).child(div().h_full().w(solid).bg(color.hsla(1.0))).child(div().h_full().w(ramp).bg(gradient(1.0, 0.0)))
            } else {
                row.right(px(0.0)).child(div().h_full().w(ramp).bg(gradient(0.0, 1.0))).child(div().h_full().w(solid).bg(color.hsla(1.0)))
            }
        };
        let arrow = |name: &'static str, dir: f32, cx: &mut Context<Self>| {
            let h = handle.clone();
            div()
                .id(name)
                .absolute()
                .top(t.s(40.0))
                .size(t.s(CONTROL_H))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .map(|d| crate::ui::glass_layers(d, &t, t.s(CONTROL_H / 2.0), GLASS_BLUR, GLASS_SATURATION, t.glass(), t.glass_fill(), crate::ui::glass_edge()))
                .opacity(0.0)
                .group_hover("featured-row", |s| s.opacity(1.0))
                .hover(|s| s.bg(t.hover()).opacity(1.0))
                .block_mouse_except_scroll()
                .on_click(cx.listener(move |_, _, window, _| {
                    let current = h.offset();
                    let max = h.max_offset().x;
                    let page = h.bounds().size.width * 0.8;
                    h.set_offset(point((current.x - page * dir).clamp(-max, px(0.0)), current.y));
                    window.refresh();
                }))
                .tooltip(crate::ui::tip(&t, if dir < 0.0 { tr("Scroll left") } else { tr("Scroll right") }))
                .child(icon(&t, if dir < 0.0 { "caret-left-bold" } else { "caret-right-bold" }, 18.0, t.t1()))
        };
        div()
            .relative()
            .group("featured-row")
            .child(people)
            .when(more_left, |d| d.child(fade(true)))
            .when(more_right, |d| d.child(fade(false)))
            .when(more_left, |d| d.child(arrow("featured-left", -1.0, cx).left(t.s(10.0))))
            .when(more_right, |d| d.child(arrow("featured-right", 1.0, cx).right(t.s(10.0))))
            .into_any_element()
    }

    fn render_dock_artwork(&mut self, art: Option<std::sync::Arc<gpui::RenderImage>>, art_h: gpui::Pixels, stack_h: gpui::Pixels, width: f32, song: &crate::model::Song, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let radius = t.s(R_LG);
        // the artwork cross-fades as the next song's arrives; the old one stays in the last bake until then.
        let shown_album = art.as_ref().map(|_| song.album_id.clone());
        let (swap, swap_ticker) = crate::ui::swap("dock-art", &shown_album, motion::SONG_FADE);
        let old_art = swap.leaving.flatten().and_then(|old| self.dock_bake.as_ref().filter(|(id, _)| *id == old).map(|(_, b)| b.art_stack.clone()));
        let layer = |image: std::sync::Arc<gpui::RenderImage>, opacity: f32| crate::ui::image(image).absolute().left(px(0.0)).top(px(0.0)).w(px(width)).h(stack_h).rounded_t(radius).opacity(opacity);
        let stack = div()
            .relative()
            .h(art_h)
            .w_full()
            .child(swap_ticker)
            .when_some(old_art, |d, image| d.child(layer(image, 1.0 - swap.shown)))
            .when_some(art, |d, image| d.child(layer(image, swap.shown)));

        // A vertical scroll moves the row while it can, then the dock takes over.
        let row_handle = self.featured_scroll.clone();
        let wheel_handle = row_handle.clone();
        let mut people = div()
            .id("featured")
            .flex()
            .flex_row()
            .gap(t.s(20.0))
            .px(t.s(32.0))
            .overflow_x_scroll()
            .restrict_scroll_to_axis()
            .track_scroll(&row_handle)
            .pb(t.s(4.0))
            .on_scroll(move |event: &gpui::ScrollWheelEvent, window, cx| {
                let delta = event.delta.pixel_delta(window.line_height());
                if delta.y.abs() <= delta.x.abs() {
                    return;
                }
                let offset = wheel_handle.offset();
                let max = wheel_handle.max_offset().x;
                let next = (offset.x + delta.y).clamp(-max, px(0.0));
                if next != offset.x {
                    wheel_handle.set_offset(point(next, offset.y));
                    cx.stop_propagation();
                    window.refresh();
                }
            });
        let avatar_px = (f32::from(t.s(116.0)) * layout.scale).round() as u32;
        let avatar_image = self.thumb_faded(&song.album_id, avatar_px);
        for (i, artist) in song.artists.iter().enumerate() {
            // The artist's photo when the server has one, otherwise the album cover.
            let image = match song.artist_art.get(i).cloned() {
                Some(art) => self.thumb_faded(&art, avatar_px).or_else(|| avatar_image.clone()),
                None => avatar_image.clone(),
            };
            let photo: AnyElement = match image {
                Some((image, fade)) => crate::ui::fade_in(crate::ui::image(image).size(t.s(116.0)).rounded_full(), div().size(t.s(116.0)).rounded_full().bg(t.on_art_placeholder()), fade),
                None => div().size(t.s(116.0)).rounded_full().bg(t.on_art_placeholder()).into_any_element(),
            };
            // Photo and name together open the artist, when the library knows them.
            let known = self.artist_named(artist).map(|a| a.id.clone());
            let group = gpui::SharedString::from(format!("featured-{i}"));
            let person = crate::ui::press_dim(div().id(("featured", i)))
                .group(group.clone())
                .flex()
                .flex_col()
                .items_center()
                .flex_none()
                .gap(t.s(8.0))
                .w(t.s(116.0))
                .text_color(t.t_on_art())
                .text_center()
                .line_height(relative(1.25))
                .child(photo)
                .child(div().id("name").w_full().line_clamp(2).when(known.is_some(), |d| d.group_hover(group, |s| s.underline())).child(artist.clone()).full_text_tip(&t, artist.clone()));
            people = people.child(match known {
                Some(id) => person.on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.navigate(crate::pages::Route::Artist(id.clone()), cx);
                })),
                None => person,
            });
        }
        let station_playing = self.player.station.clone();
        // The dock color around the row, for its edge fades. GPUI has no masks.
        let vivid = player_vivid(self.tint());
        let end = vivid.mix(Srgb::hex(crate::theme::DOCK_SHADE), crate::theme::PLAYER_SHADE_MIX);
        let dock_h = f32::from(layout.viewport.height) - f32::from(t.s(WINDOW_INSET)) * 2.0;
        let at = ((1.0 - f32::from(t.s(252.0)) / dock_h - 0.35) / 0.65).clamp(0.0, 1.0);
        let fade_color = end.mix(vivid, at);
        let people = self.featured_edges(people, &row_handle, fade_color, cx);
        let featured = div()
            .relative()
            .pb(t.s(44.0))
            .child(
                div()
                    .px(t.s(32.0))
                    .mb(t.s(14.0))
                    .text_size(t.text(TEXT_SECTION))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.t_on_art())
                    .line_height(relative(1.2))
                    .child(tr("Featured Artists")),
            )
            .child(people);

        // GPUI has no masks, so the bottom fades to the dock color sampled where the scroller ends. Once scrolled, the
        // top fades to the dock's top color too.
        let scrolled = f32::from(self.dock_art_scroll.offset().y) < -0.5;
        div()
            .absolute()
            .inset_0()
            .child(div().id("dock-art-scroll").absolute().inset_0().overflow_y_scroll().track_scroll(&self.dock_art_scroll).child(stack).when(station_playing.is_none(), |d| d.child(div().mt(t.s(-8.0)).child(featured))))
            .child(self.render_scrollbar("dock-art", crate::ui_scroll::ScrollSource::Handle(self.dock_art_scroll.clone()), t.s(76.0), t.s(8.0), cx))
            .child(
                div().absolute().left(px(0.0)).right(px(0.0)).bottom(px(0.0)).h(t.s(28.0)).bg(gpui::linear_gradient(
                    180.0,
                    gpui::linear_color_stop(fade_color.hsla(0.0), 0.0),
                    gpui::linear_color_stop(fade_color.hsla(1.0), 1.0),
                )),
            )
            .when(scrolled, |d| {
                d.child(div().absolute().left(px(0.0)).right(px(0.0)).top(px(0.0)).h(t.s(28.0)).bg(gpui::linear_gradient(
                    180.0,
                    gpui::linear_color_stop(vivid.hsla(1.0), 0.0),
                    gpui::linear_color_stop(vivid.hsla(0.0), 1.0),
                )))
            })
            .into_any_element()
    }

    /// The title, the artists in `dim`, then the heart and More in `fg`.
    pub fn render_meta_row(&mut self, song: &crate::model::Song, fg: gpui::Hsla, dim: gpui::Hsla, feel: crate::ui::Feel, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let song_ix = self.player.song_ix.unwrap_or(0) as u32;
        let starred = self.is_starred(song_ix);
        let station = self.player.station.is_some();
        // too long, they scroll.
        let host = self.marquees.current.unwrap_or(crate::marquee::Host::Player);
        let keys = if host == crate::marquee::Host::Expanded { ("expanded-title", "expanded-artist") } else { ("dock-title", "dock-artist") };
        // the side player's title cross-fades on a song change; full screen it just changes.
        let (swap, swap_ticker) = if host == crate::marquee::Host::Expanded {
            (crate::ui::Swapping { shown: 1.0, leaving: None }, div().into_any_element())
        } else {
            self.song_swap("dock-song", song)
        };
        let (title, artists) = self.player_names(song, keys, host, (TEXT_LG, TEXT_MD), 1.4, (t.t_on_art().into(), dim), swap.shown, cx);
        let leaving = swap.leaving.map(|old| self.leaving_names(&old.0, (TEXT_LG, TEXT_MD), 1.4, (t.t_on_art().into(), dim), 1.0 - swap.shown));
        div()
            .flex()
            .flex_row()
            .items_center()
            .mb(t.s(PLAYER_META_GAP))
            .child(
                div()
                    .id("meta-title")
                    .relative()
                    .child(swap_ticker)
                    .children(leaving)
                    .flex_1()
                    .min_w(px(0.0))
                    .line_height(relative(1.4))
                    // Right-click on the song opens the same menu as More.
                    .on_mouse_down(MouseButton::Right, cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| this.open_player_menu(event.position, false, cx)))
                    .child(crate::ui::titled(&t, div().flex().min_w(px(0.0)).overflow_hidden().text_size(t.text(TEXT_LG)).font_weight(FontWeight::MEDIUM).text_color(t.t_on_art()).child(title), song.explicit, TEXT_LG).opacity(swap.shown))
                    .child(div().flex().min_w(px(0.0)).overflow_hidden().child(artists))
                    .children(self.output_badge(false, false, dim)),
            )
            .child(
                crate::ui::player_button(&t, "meta-star", if station { tr(crate::ui::STATION_NO_STAR) } else { tr("Favorite") }, if starred { "heart-fill" } else { "heart-regular" }, 20.0, fg, fg, feel)
                    .when(!station, |d| {
                        d.stop(&t).on_click(cx.listener(move |this, _, _, cx| {
                            if !starred {
                                crate::ui::pop("meta-star");
                            }
                            this.toggle_song_star(song_ix, cx);
                        }))
                    })
                    .when(station, |d| crate::ui::locked(d).on_click(cx.listener(|this, _, _, cx| this.show_toast(tr(crate::ui::STATION_NO_STAR), cx)))),
            )
            .child(
                crate::ui::menu_on_press(crate::ui::player_menu_button(&t, "meta-more", tr("More"), "dots-three-bold", 20.0, fg, fg, feel).stop(&t), cx, |this, at, cx| this.open_player_menu(at + point(px(0.0), this.theme.s(14.0)), true, cx)),
            )
            .into_any_element()
    }
}

/// a song compared by id, so a cross-fade runs only when the song itself changes.
#[derive(Clone)]
pub struct SongById(pub crate::model::Song);

impl PartialEq for SongById {
    fn eq(&self, other: &Self) -> bool {
        self.0.id == other.0.id
    }
}
