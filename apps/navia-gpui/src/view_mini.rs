//! The mini player's contents. Compact is a strip with the cover on the left, and large is a square cover with controls
//! over a scrim. Drawn from the shell, so it shares the main player's state.

use crate::i18n::tr;
use gpui::prelude::*;
use gpui::{AnyElement, Bounds, Context, FocusHandle, FontWeight, KeyDownEvent, MouseButton, MouseDownEvent, Pixels, StyledText, Window, canvas, div, point, px, relative};

use crate::color::{Srgb, player_fg, player_fg_dim, player_vivid};
use crate::mini_player::MiniKind;
use crate::shell::Shell;
use crate::theme::size::*;
use crate::view_dock::ScrubVariant;

impl Shell {
    pub fn render_mini(&mut self, focus: &FocusHandle, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let kind = self.mini_kind();
        let room = self.mini_room();
        // Not while the hint shows, since the window is then as wide as the hint.
        if kind != MiniKind::Layer && self.mini.surface.is_some() && !self.mini_hint_shown() {
            // A window the desktop resizes. The card is whatever room the window has.
            let view = window.viewport_size();
            let hint = if self.mini_hint_shown() { (*self.mini.hint_h.borrow()).map(|h| h + t.s(MINI_HINT_GAP)).unwrap_or(px(0.0)) } else { px(0.0) };
            self.mini_follow_window(gpui::size(view.width - room * 2.0, view.height - room * 2.0 - hint));
        }
        let large = self.mini_stacked();
        let card = if kind == MiniKind::Topmost && self.mini.surface.is_some() { window.viewport_size() } else { self.mini_card_size() };
        let radius = self.mini_radius();
        let origin = self.mini_card_origin(window);
        let card_box = Bounds { origin, size: card };
        if self.mini.resize.is_some() {
            // The surface is the whole work area until the grip is released.
        } else if kind == MiniKind::Layer || self.mini.surface.is_none() {
            self.resize_mini_surface(window);
        } else {
            self.mini.surface = Some(window.viewport_size());
        }
        match kind {
            // The window's own edge is the card, so the desktop places, snaps and shadows the card.
            MiniKind::Window => {
                window.set_client_inset(room);
                // Only the card is blurred, even where the desktop made the window bigger.
                window.set_blur_area(Some(card_box));
                window.set_client_corner_radii(gpui::Corners::all(radius));
            }
            MiniKind::Layer => {
                // Only the card takes the pointer, so clicks on its shadow pass through. While a grip is held the whole
                // surface does, so a changing region can't drop the drag.
                let region = if self.mini.resize.is_some() { Bounds { origin: point(px(0.0), px(0.0)), size: window.viewport_size() } } else { card_box };
                if self.mini.region != Some(region) {
                    self.mini.region = Some(region);
                    window.set_input_region(Some(&[region]));
                }
                // The desktop's blur goes behind the card only, not its shadow room.
                window.set_blur_area(Some(card_box));
                window.set_client_corner_radii(gpui::Corners::all(radius));
            }
            MiniKind::Topmost => {}
        }

        let root = div()
            .id("mini-root")
            .size_full()
            .relative()
            .font_family(crate::assets::FONT_FAMILY)
            .track_focus(focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| this.mini_key(event, cx)));
        let Some(song) = self.current_song().cloned() else { return root.into_any_element() };

        let tint = self.tint();
        let vivid = player_vivid(tint);
        let shade = vivid.mix(Srgb::hex(crate::theme::DOCK_SHADE), crate::theme::PLAYER_SHADE_MIX);
        let fg: gpui::Hsla = player_fg(tint).hsla(1.0);
        let dim: gpui::Hsla = player_fg_dim(tint).hsla(1.0);
        // Over the desktop blur the glass lets a little through. Without blur it's solid.
        let glass = if self.mini_glass() { MINI_GLASS } else { 1.0 };
        let station = self.player.station.is_some();
        let playing = self.player.playing;
        let hover = crate::ui::ink("mini-card").amount;

        // The cover, a square as tall as the card in compact, or the whole card in large. GPUI clips to rectangles, so
        // it rounds its own outer corners. It needs an explicit aspect ratio or GPUI uses the image's own.
        let art_side = if large { card.width.max(card.height) } else { card.height };
        let art_px = (f32::from(art_side) * window.scale_factor()).round() as u32;
        let (art_w, art_h) = if large { (card.width, card.height) } else { (card.height, card.height) };
        let cover_shape = |d: gpui::Img| if large { d.rounded(radius) } else { d.rounded_l(radius) };
        let placeholder = || {
            let d = div().w(art_w).h(art_h).bg(t.on_art_placeholder());
            if large { d.rounded(radius) } else { d.rounded_l(radius) }
        };
        let cover: AnyElement = match self.thumb_faded(&song.album_id, art_px) {
            Some((image, fade)) => crate::ui::fade_in(cover_shape(crate::ui::image(image).w(art_w).h(art_h).aspect_ratio(f32::from(art_w) / f32::from(art_h))), placeholder(), fade),
            None => placeholder().into_any_element(),
        };
        if self.thumbs_fading {
            crate::animation::request_frame(window);
        }

        // The title and artists link to their pages. The line being sung shows when the song has synced lyrics.
        let title_size = if large { TEXT_LG } else { TEXT_MD };
        let (title, artists) = self.player_names(&song, ("mini-title", "mini-artist"), crate::marquee::Host::Mini, (title_size, TEXT_SM), 1.3, (t.t_on_art().into(), dim), 1.0, cx);
        let title = crate::ui::titled(&t, div().flex().min_w(px(0.0)).overflow_hidden().child(title), song.explicit, title_size)
            .font_weight(FontWeight::MEDIUM)
            .text_size(t.text(title_size))
            .text_color(t.t_on_art())
            .when(large, |d| d.justify_center());
        let lyric = (!station && self.lyrics_synced()).then(|| self.current_lyric_line()).flatten().and_then(|i| self.lyrics.get(i)).map(|l| l.text.clone()).filter(|l| !l.is_empty());
        let info = div()
            .flex()
            .flex_col()
            .min_w(px(0.0))
            .line_height(relative(1.3))
            .when(large, |d| d.items_center())
            .child(title)
            .child(div().flex().min_w(px(0.0)).max_w_full().text_size(t.text(TEXT_SM)).text_color(dim).child(artists))
            .when_some(lyric, |d, line| d.child(div().max_w_full().truncate().text_size(t.text(TEXT_XS)).text_color(dim).child(line)));

        let (skip, play) = if large { (26.0, 38.0) } else { (20.0, 28.0) };
        let button = |b: gpui::Stateful<gpui::Div>| if large { b } else { b.size(t.s(MINI_BUTTON)) };
        let transport = div()
            .flex()
            .flex_row()
            .items_center()
            .flex_none()
            .occlude()
            .when(large, |d| d.gap(t.s(MINI_GAP * 2.0)))
            .child(button(crate::ui::transport_button(&t, "mini-prev", if station { tr(crate::ui::STATION_NO_SKIP) } else { tr("Previous") }, "rewind-fill", skip, fg, !station, crate::ui::Feel::PLAIN)).when(!station, |d| d.on_click(cx.listener(|this, _, _, cx| this.previous(cx)))))
            .child(button(crate::ui::play_button(&t, "mini-play", playing, play, fg, crate::ui::Feel::PLAIN)).on_click(cx.listener(|this, _, _, cx| this.toggle_play(cx))))
            .child(button(crate::ui::transport_button(&t, "mini-next", if station { tr(crate::ui::STATION_NO_SKIP) } else { tr("Next") }, "fast-forward-fill", skip, fg, !station, crate::ui::Feel::PLAIN)).when(!station, |d| d.on_click(cx.listener(|this, _, _, cx| this.next(cx)))));
        let seek = div().w_full().text_color(fg).child(self.render_scrubber(ScrubVariant::Thin, t.player_track(), fg, dim, crate::ui::Feel::PLAIN, cx));

        let body = if large {
            div()
                .absolute()
                .left(px(0.0))
                .right(px(0.0))
                .bottom(px(0.0))
                .flex()
                .flex_col()
                .gap(t.s(MINI_PAD / 2.0))
                .px(t.s(MINI_PAD * 1.5))
                .pb(t.s(MINI_PAD))
                .child(info)
                .child(seek)
                .child(div().flex().justify_center().child(transport))
        } else {
            div()
                .relative()
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .flex_col()
                .pt(t.s(MINI_PAD))
                .pr(t.s(MINI_PAD))
                .pb(t.s(MINI_PAD / 2.0))
                .pl(t.s(MINI_GAP))
                .child(div().flex_1().min_h(px(0.0)).flex().flex_row().gap(t.s(MINI_GAP)).child(div().flex_1().min_w(px(0.0)).flex().items_center().child(info)).child(div().flex().items_end().child(transport)))
                .child(seek)
        };

        // on the large cover they sit on the window buttons' dark fill.
        let chrome_button = |id: &'static str, label: &'static str, name: &'static str, glyph: f32| {
            let b = crate::ui::player_button(&t, id, label, name, glyph, fg, fg, crate::ui::Feel::PLAIN);
            if large {
                let on = crate::ui::ink(id).amount;
                let fill = gpui::rgba(crate::theme::WINDOW_BUTTON_ON_ART);
                let fill_hover = gpui::rgba(crate::theme::WINDOW_BUTTON_ON_ART_HOVER);
                b.size(t.s(CONTROL_H)).bg(crate::ui::Ink { amount: on, ..Default::default() }.color(fill, fill_hover))
            } else {
                b.size(t.s(MINI_CHROME_BUTTON))
            }
        };
        let (resize_icon, resize_glyph, back_glyph) = if large { ("arrows-in-simple-bold", 17.0, 19.0) } else { ("arrows-out-simple-bold", 15.0, 17.0) };
        let chrome_inset = if large { t.s(MINI_PAD) } else { t.s(MINI_PAD / 3.0) };
        let chrome = div()
            .absolute()
            .top(chrome_inset)
            .right(chrome_inset)
            .flex()
            .flex_row()
            .gap(if large { t.s(MINI_GAP / 2.0) } else { px(2.0) })
            .opacity(hover)
            .occlude()
            .child(chrome_button("mini-size", if large { tr("Smaller") } else { tr("Larger") }, resize_icon, resize_glyph).on_click(cx.listener(|this, _, window, cx| this.toggle_mini_size(window, cx))))
            .child(chrome_button("mini-back", tr("Back to Full Window"), "app-window-regular", back_glyph).on_click(cx.listener(|this, _, _, cx| this.close_mini_player(true, cx))));

        let background = gpui::linear_gradient(180.0, gpui::linear_color_stop(vivid.hsla(glass), 0.0), gpui::linear_color_stop(shade.hsla(glass), 1.0));
        let mut card_el = div()
            .id("mini-card")
            .absolute()
            .left(origin.x)
            .top(origin.y)
            .w(card.width)
            .h(card.height)
            .flex()
            .flex_row()
            .rounded(radius)
            .bg(background)
            .shadow(vec![crate::ui::shadow_float(&t)])
            .text_color(fg)
            .text_size(t.text(TEXT_MD))
            // Any empty spot drags it.
            .on_mouse_down(MouseButton::Left, cx.listener(|this, event: &MouseDownEvent, window, cx| this.mini_press(event.position, window, cx)))
            .when(kind == MiniKind::Window, |d| d.on_mouse_down(MouseButton::Right, cx.listener(|_, event: &MouseDownEvent, window, _| window.show_window_menu(event.position))));
        #[cfg(target_os = "windows")]
        {
            card_el = card_el.window_control_area(gpui::WindowControlArea::Drag);
        }
        card_el = if large {
            card_el
                .child(div().absolute().inset_0().child(cover))
                .child(div().absolute().inset_0().rounded(radius).bg(gpui::linear_gradient(
                    180.0,
                    gpui::linear_color_stop(shade.hsla(0.0), MINI_SCRIM_START),
                    gpui::linear_color_stop(shade.hsla(MINI_SCRIM_STRENGTH), 1.0),
                )))
                .child(body)
        } else {
            card_el.child(div().flex_none().child(cover)).child(body)
        };
        // The thin light edge goes over everything, since inset shadows draw under children.
        card_el = card_el.child(div().absolute().inset_0().rounded(radius).shadow(crate::ui::glass_edge())).child(chrome);
        #[cfg(not(target_os = "windows"))]
        {
            card_el = card_el.children(self.mini_grips(card, cx));
        }
        // The transport and grips go last, so none of them hides the chrome under the pointer.
        card_el = card_el.child(crate::ui::reveal_track("mini-card"));

        let mut root = root.child(card_el);
        if self.mini_hint_shown() {
            root = root.child(self.render_mini_hint(room, card, cx));
        }
        if self.mini.grab.is_some() || self.mini.resize.is_some() || self.seek_dragging() {
            root = root.child(self.mini_drag_tracker(cx));
        }
        root.into_any_element()
    }

    /// The resize grips along the card's edges and corners.
    fn mini_grips(&mut self, card: gpui::Size<Pixels>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        use gpui::{CursorStyle as C, ResizeEdge as E};
        let g = self.theme.s(MINI_GRIP);
        let corner = g * 2.0;
        let grips = [
            (E::Top, point(corner, px(0.0)), gpui::size(card.width - corner * 2.0, g), C::ResizeUpDown),
            (E::Bottom, point(corner, card.height - g), gpui::size(card.width - corner * 2.0, g), C::ResizeUpDown),
            (E::Left, point(px(0.0), corner), gpui::size(g, card.height - corner * 2.0), C::ResizeLeftRight),
            (E::Right, point(card.width - g, corner), gpui::size(g, card.height - corner * 2.0), C::ResizeLeftRight),
            (E::TopLeft, point(px(0.0), px(0.0)), gpui::size(corner, corner), C::ResizeUpLeftDownRight),
            (E::BottomRight, point(card.width - corner, card.height - corner), gpui::size(corner, corner), C::ResizeUpLeftDownRight),
            (E::TopRight, point(card.width - corner, px(0.0)), gpui::size(corner, corner), C::ResizeUpRightDownLeft),
            (E::BottomLeft, point(px(0.0), card.height - corner), gpui::size(corner, corner), C::ResizeUpRightDownLeft),
        ];
        grips
            .into_iter()
            .enumerate()
            .map(|(i, (edge, at, size, cursor))| {
                div()
                    .id(("mini-grip", i))
                    .absolute()
                    .left(at.x)
                    .top(at.y)
                    .w(size.width)
                    .h(size.height)
                    .cursor(cursor)
                    .occlude()
                    .on_mouse_down(MouseButton::Left, cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        this.mini_resize_press(edge, event.position, window, cx);
                    }))
                    .into_any_element()
            })
            .collect()
    }

    /// follows the pointer even past the card's edge.
    fn mini_drag_tracker(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let on_move = cx.listener(|this: &mut Self, event: &gpui::MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>| {
            if event.pressed_button != Some(MouseButton::Left) {
                this.mini_drag_end(cx);
                this.mini_resize_end(window, cx);
                if this.seek_dragging() {
                    this.end_drag(cx);
                }
                return;
            }
            if this.mini.resize.is_some() {
                this.mini_resize_move(event.position, window);
            } else if this.mini.grab.is_some() {
                this.mini_drag_move(event.position, window);
            } else {
                this.seek_to(event.position.x, cx);
            }
        });
        let on_up = cx.listener(|this: &mut Self, _: &gpui::MouseUpEvent, window: &mut Window, cx: &mut Context<Self>| {
            this.mini_drag_end(cx);
            this.mini_resize_end(window, cx);
            if this.seek_dragging() {
                this.end_drag(cx);
            }
        });
        canvas(|_, _, _| {}, move |_, _, window, _| {
            window.on_mouse_event(move |event: &gpui::MouseMoveEvent, phase, window, cx| {
                if phase == gpui::DispatchPhase::Capture {
                    on_move(event, window, cx);
                }
            });
            window.on_mouse_event(move |event: &gpui::MouseUpEvent, phase, window, cx| {
                if phase == gpui::DispatchPhase::Capture && event.button == MouseButton::Left {
                    on_up(event, window, cx);
                }
            });
        })
        .absolute()
        .size_0()
        .into_any_element()
    }

    /// GNOME's one-time hint under the card that Always on Top is in the window menu.
    fn render_mini_hint(&mut self, room: Pixels, card: gpui::Size<Pixels>, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let item = tr("Always on Top");
        let text = crate::i18n::trf("To keep the mini player above other windows, right-click an empty spot of it and choose {item}.", &[("item", item)]);
        let bold = text.find(item).map(|start| start..start + item.len());
        let styled = StyledText::new(text.clone()).with_highlights(bold.map(|range| (range, gpui::HighlightStyle { font_weight: Some(FontWeight::SEMIBOLD), color: Some(t.t1().into()), ..Default::default() })));
        let measured = self.mini.hint_h.clone();
        let measure = canvas(
            move |bounds, window, _| {
                let h = bounds.size.height;
                if *measured.borrow() != Some(h) {
                    *measured.borrow_mut() = Some(h);
                    window.refresh();
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .inset_0();
        crate::ui::float_panel(&t)
            .absolute()
            .left(room)
            .top(room + card.height + t.s(MINI_HINT_GAP))
            .w(card.width.max(t.s(MINI_HINT_W)))
            .flex()
            .flex_row()
            .items_center()
            .gap(t.s(14.0))
            .pt(t.s(12.0))
            .pb(t.s(12.0))
            .pr(t.s(12.0))
            .pl(t.s(16.0))
            .text_size(t.text(TEXT_SM))
            .line_height(relative(1.4))
            .text_color(t.t2())
            .child(measure)
            .child(div().flex_1().min_w(px(0.0)).child(styled))
            .child(crate::ui_controls::glass_action(&t, "mini-hint-ok").child(tr("Got It")).on_click(cx.listener(|this, _, window, cx| this.dismiss_mini_hint(window, cx))))
            .into_any_element()
    }

    /// Keys in the mini window. Space toggles play, and the shortcut returns to the full window.
    fn mini_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let k = &event.keystroke;
        if Self::is_mini_shortcut(k) {
            self.close_mini_player(true, cx);
        } else if k.key == "space" && !k.modifiers.modified() {
            self.toggle_play(cx);
        }
    }
}
