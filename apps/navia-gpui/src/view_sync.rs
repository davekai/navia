//! Sync progress (`navia_core::library::sync::SyncWatch`). Shown as a bar under the empty state while the page is
//! empty, otherwise as a card at the sidebar's foot. Refreshes after the first sync only show after
//! `motion::SYNC_SHOW_AFTER`, so quick ones stay silent.

use gpui::prelude::*;
use gpui::{Animation, AnimationExt, AnyElement, Div, FontWeight, div, px, relative};
use navia_core::library::sync::{SyncMeter, SyncShown, SyncStage};

use crate::i18n::{fmt_int, tr, trnf};
use crate::shell::Shell;
use crate::theme::size::*;
use crate::theme::{Theme, motion};

/// "1,240 of 3,812 albums", or a running count when the server gave no total. None before the first page.
pub fn sync_detail(meter: &SyncMeter) -> Option<String> {
    let done = fmt_int(meter.done as i64);
    match (meter.stage, meter.total) {
        (SyncStage::Artists, Some(total)) => Some(trnf("{done} of {count} artist", "{done} of {count} artists", total as usize, &[("done", &done)])),
        (SyncStage::Albums, Some(total)) => Some(trnf("{done} of {count} album", "{done} of {count} albums", total as usize, &[("done", &done)])),
        (SyncStage::Songs, Some(total)) => Some(trnf("{done} of {count} song", "{done} of {count} songs", total as usize, &[("done", &done)])),
        (SyncStage::Playlists, Some(total)) => Some(trnf("{done} of {count} playlist", "{done} of {count} playlists", total as usize, &[("done", &done)])),
        // No total from the server, so count up.
        _ if meter.done == 0 => None,
        (SyncStage::Artists, _) => Some(trnf("{count} artist", "{count} artists", meter.done as usize, &[])),
        (SyncStage::Albums, _) => Some(trnf("{count} album", "{count} albums", meter.done as usize, &[])),
        (SyncStage::Songs, None) => Some(trnf("{count} song", "{count} songs", meter.done as usize, &[])),
        (SyncStage::Playlists, None) => Some(trnf("{count} playlist", "{count} playlists", meter.done as usize, &[])),
    }
}

/// The progress bar, filled to `fraction`, or a sweeping segment when there's none. It sweeps even under reduced
/// motion.
pub fn sync_bar(t: &Theme, id: &'static str, fraction: Option<f32>) -> AnyElement {
    let track = div().relative().w_full().h(t.s(SYNC_BAR_H)).rounded_full().overflow_hidden().bg(t.sync_track());
    let fill = div().absolute().top_0().bottom_0().rounded_full().bg(t.sync_fill());
    match fraction {
        Some(f) => track.child(fill.left_0().w(relative(f))).into_any_element(),
        None if motion::reduced() => track.child(fill.left(relative((1.0 - motion::SYNC_SWEEP_SPAN) / 2.0)).w(relative(motion::SYNC_SWEEP_SPAN))).into_any_element(),
        None => {
            let span = motion::SYNC_SWEEP_SPAN;
            let sweep = fill.w(relative(span)).with_animation(id, Animation::new(motion::duration(motion::SYNC_SWEEP)).repeat(), move |el, delta| el.left(relative(-span + delta * (1.0 + span))));
            track.child(sweep).into_any_element()
        }
    }
}

impl Shell {
    /// The sync to show, if any. See the module docs.
    pub fn sync_shown(&self) -> Option<SyncShown> {
        self.core.as_ref()?.lib.sync_watch.visible(motion::duration(motion::SYNC_SHOW_AFTER))
    }

    /// The count and bar under the empty state's "Syncing your library".
    pub fn render_sync_meter(&self) -> Option<Div> {
        let t = self.theme;
        let shown = self.sync_shown()?;
        let meter = shown.meter?;
        self.sync_meter_now.set(true);
        Some(
            div()
                .w(t.s(SYNC_METER_W))
                .max_w_full()
                .mt(t.s(8.0))
                .flex()
                .flex_col()
                .gap(t.s(8.0))
                .text_size(t.text(TEXT_XS))
                .text_color(t.t2())
                .children(sync_detail(&meter))
                .child(sync_bar(&t, "sync-meter-sweep", meter.fraction())),
        )
    }

    /// The card at the sidebar's foot, whenever the page isn't showing the meter.
    pub fn render_sync_card(&self) -> Option<AnyElement> {
        let t = self.theme;
        let shown = self.sync_shown()?;
        if self.sync_meter_last.get() || self.sync_meter_now.get() {
            return None;
        }
        let meter = shown.meter;
        Some(
            div()
                .flex()
                .flex_col()
                .gap(t.s(8.0))
                .mx(t.s(12.0))
                .mt(t.s(6.0))
                .mb(t.s(12.0))
                .px(t.s(12.0))
                .py(t.s(10.0))
                .rounded(t.s(R_MD))
                .bg(t.row())
                .child(
                    div()
                        .min_w(px(0.0))
                        .line_height(relative(1.25))
                        .child(div().font_weight(FontWeight::MEDIUM).truncate().child(tr("Syncing library")))
                        .children(meter.as_ref().and_then(sync_detail).map(|d| div().text_size(t.text(TEXT_XS)).text_color(t.t2()).truncate().child(d))),
                )
                .child(sync_bar(&t, "sync-card-sweep", meter.and_then(|m| m.fraction())))
                .into_any_element(),
        )
    }
}
