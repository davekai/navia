//! The player's chip slot, a pill above the title in the dock and expanded player, or above the play controls in the
//! bar. In priority order it shows a lost AirPlay receiver, listen-along status, listeners on the host, a listener who
//! stopped, and the offer to continue another device's queue.

use gpui::prelude::*;
use gpui::{AnyElement, Context, Hsla, SharedString, div, px};

use crate::i18n::{tr, trf};
use crate::shell::Shell;
use crate::theme::size::*;
use crate::ui::{hover_track, icon, ink, ink_button};

/// its end control, and what the label does when clicked.
enum ChipEnd {
    /// A text button, like Stop.
    Text(SharedString),
    /// dismiss, not now: the label is its tip.
    Close(&'static str),
    /// nothing to press.
    None,
    /// Several text buttons, each with its own action. `on_end` isn't used.
    Actions(Vec<(SharedString, ChipAction)>),
}

type ChipAction = std::rc::Rc<dyn Fn(&mut Shell, &mut Context<Shell>)>;

impl Shell {
    /// The player chip, or None when there's nothing to show. `above_bar` is the bar's form.
    pub fn render_player_chip(&mut self, above_bar: bool, rest: Hsla, hover: Hsla, cx: &mut Context<Self>) -> Option<AnyElement> {
        // A lost receiver comes first. Playback stays paused until it's back or the listener plays here.
        if let Some((lost, receiver)) = self.airplay_lost() {
            let here: (SharedString, ChipAction) = (tr("Play Here").into(), std::rc::Rc::new(|this: &mut Shell, cx: &mut Context<Shell>| this.airplay_play_here(cx)));
            let (label, ends) = match lost {
                crate::view_airplay::Lost::Reconnecting => (tr("Connection lost. Reconnecting…"), vec![here]),
                crate::view_airplay::Lost::GaveUp => (tr("Couldn’t reconnect"), vec![(tr("Try Again").into(), std::rc::Rc::new(|this: &mut Shell, cx: &mut Context<Shell>| this.airplay_reconnect(cx)) as ChipAction), here]),
            };
            return Some(self.player_chip("airplay-lost", above_bar, "nv-airplay", label.into(), Some(receiver.name.into()), false, ChipEnd::Actions(ends), rest, hover, cx, |_, _| {}));
        }
        if let Some((user, waiting, held, sending)) = self.listen.following.as_ref().map(|f| (f.followed.user.clone(), f.waiting, f.held, f.sending)) {
            let label = if waiting {
                trf("Waiting for a song you have from {user}", &[("user", &user)])
            } else if sending {
                trf("Sending to {user}…", &[("user", &user)])
            } else if held {
                trf("Listening along with {user} · paused", &[("user", &user)])
            } else {
                trf("Listening along with {user}", &[("user", &user)])
            };
            let tip = trf("Checks every {seconds} seconds and catches up when it drifts more than {drift} seconds.", &[
                ("seconds", &navia_core::listen_along::FOLLOW_POLL.as_secs().to_string()),
                ("drift", &navia_core::listen_along::RESYNC_DRIFT.as_secs().to_string()),
            ]);
            return Some(self.player_chip("listen-chip", above_bar, "headphones-regular", label.into(), Some(tip.into()), false, ChipEnd::Text(tr("Stop").into()), rest, hover, cx, |this, cx| {
                this.stop_listening(false, cx);
            }));
        }
        if let Some(user) = self.listen.ended.clone() {
            let label = trf("{user} stopped listening", &[("user", &user)]);
            return Some(self.player_chip("listen-ended", above_bar, "headphones-regular", label.into(), None, false, ChipEnd::Close("Dismiss"), rest, hover, cx, |this, cx| this.dismiss_listen_ended(cx)));
        }
        // On the host, show who's listening along while they're allowed to control playback.
        if self.listeners_control() && !self.listen.listeners.is_empty() {
            let label = match self.listen.listeners.as_slice() {
                [one] => trf("{user} is listening along", &[("user", &one.user)]),
                many => crate::i18n::trn("{count} person is listening along", "{count} people are listening along", many.len()),
            };
            return Some(self.player_chip("listeners", above_bar, "headphones-regular", label.into(), None, false, ChipEnd::None, rest, hover, cx, |_, _| {}));
        }
        let device = self.core.as_ref()?.queue_offer.as_ref()?.device.clone();
        let label = trf("Continue from {device}?", &[("device", &device)]);
        let offer = self.player_chip("queue-offer", above_bar, "device-mobile-speaker-regular", label.into(), None, true, ChipEnd::Close("Not now"), rest, hover, cx, |this, cx| this.dismiss_queue_offer(cx));
        Some(offer)
    }

    /// One chip: `glyph` and `label`, then `end`, which runs `on_end`. The label accepts the queue offer when
    /// `label_acts`. `tip` is the tooltip.
    #[allow(clippy::too_many_arguments)]
    fn player_chip(
        &mut self,
        key: &'static str,
        above_bar: bool,
        glyph: &'static str,
        label: SharedString,
        tip: Option<SharedString>,
        label_acts: bool,
        end: ChipEnd,
        rest: Hsla,
        hover: Hsla,
        cx: &mut Context<Self>,
        on_end: impl Fn(&mut Shell, &mut Context<Shell>) + 'static,
    ) -> AnyElement {
        let t = self.theme;
        let go = ink(key);
        let content = div()
            .id(key)
            .relative()
            .flex()
            .flex_row()
            .items_center()
            .gap(t.s(6.0))
            .min_w(px(0.0))
            .py(t.s(4.0))
            .pr(t.s(4.0))
            .opacity(if label_acts { go.opacity(0.9) } else { 0.9 })
            .text_color(if label_acts { go.color(rest, hover) } else { rest })
            .child(icon(&t, glyph, 14.0, if label_acts { go.color(rest, hover) } else { rest }))
            .child(div().min_w(px(0.0)).truncate().child(label))
            .when(label_acts, |d| d.child(hover_track(key)).on_click(cx.listener(|this, _, _, cx| this.accept_queue_offer(cx))));
        let end_key: (&'static str, usize) = (key, 1);
        let end_el = match end {
            ChipEnd::Text(text) => {
                let e = ink(end_key);
                div()
                    .id(end_key)
                    .relative()
                    .flex_none()
                    .h(t.s(24.0))
                    .px(t.s(10.0))
                    .flex()
                    .items_center()
                    .rounded_full()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(hover_track(end_key))
                    .text_color(e.color(rest, hover))
                    .child(text)
                    .on_click(cx.listener(move |this, _, _, cx| on_end(this, cx)))
                    .into_any_element()
            }
            ChipEnd::Close(label) => ink_button(&t, end_key, tr(label), "x-bold", 14.0, rest, hover).w(t.s(24.0)).h(t.s(24.0)).on_click(cx.listener(move |this, _, _, cx| on_end(this, cx))).into_any_element(),
            // The same height as the others, without a button.
            ChipEnd::None => div().flex_none().w(t.s(6.0)).h(t.s(24.0)).into_any_element(),
            ChipEnd::Actions(actions) => div()
                .flex()
                .flex_row()
                .flex_none()
                .children(actions.into_iter().enumerate().map(|(i, (text, run))| {
                    let k: (&'static str, usize) = (key, 10 + i);
                    let e = ink(k);
                    div()
                        .id(k)
                        .relative()
                        .flex_none()
                        .h(t.s(24.0))
                        .px(t.s(10.0))
                        .flex()
                        .items_center()
                        .rounded_full()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(hover_track(k))
                        .text_color(e.color(rest, hover))
                        .child(text)
                        .on_click(cx.listener(move |this, _, _, cx| run(this, cx)))
                }))
                .into_any_element(),
        };
        let pill = div()
            .id((key, 2usize))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(2.0))
            .max_w_full()
            .min_w(px(0.0))
            .pl(t.s(10.0))
            .pr(px(2.0))
            .py(px(2.0))
            .rounded_full()
            .text_size(t.text(TEXT_SM))
            .when_some(tip, |d, tip| d.tooltip(crate::ui::tip(&t, tip)))
            .map(|d| {
                if above_bar {
                    crate::ui::glass_layers(d, &t, t.s(14.0), BAR_BLUR, BAR_SATURATION, t.bar_glass(), t.bar_fill(), crate::ui::edge_and(crate::ui::shadow_float(&t)))
                } else {
                    crate::ui::glass_layers(d, &t, t.s(14.0), GLASS_BLUR, GLASS_SATURATION, t.switcher_glass(), t.switcher_glass(), vec![crate::ui::inset_ring(t.switcher_edge())])
                }
            })
            .child(content)
            .child(end_el);
        if above_bar {
            // Centered above the play controls, in a row wider than the chip can be.
            let half = t.s(CHIP_ABOVE_MAX_W / 2.0);
            div().absolute().left(-half).right(-half).bottom(t.s(BAR_H + CHIP_ABOVE_GAP)).flex().flex_row().justify_center().child(div().max_w(t.s(CHIP_ABOVE_MAX_W)).child(pill)).into_any_element()
        } else {
            div().flex().flex_row().justify_center().mb(t.s(12.0)).child(pill).into_any_element()
        }
    }
}
