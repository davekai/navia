//! Recommended under an editable playlist: ten of the server's similar songs, ranked by `navia_core::recommend`, each
//! with Add. Requested only once the section scrolls near view, and hidden until there's a non-empty answer.

use std::sync::{Arc, Mutex};

use gpui::prelude::*;
use gpui::{AnyElement, Context, FontWeight, SharedString, div, px, relative};

use crate::focus::FocusStop;
use crate::i18n::{tr, trf};
use crate::pages::{Block, PageMetrics, RowVariant};
use crate::shell::{Layout, Shell};
use crate::theme::size::*;
use crate::ui_controls::small_glass_action;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RecPhase {
    /// Not requested yet. Requested when the section's place renders.
    Waiting,
    /// Requesting the first batch.
    Asking,
    Shown,
    /// Requesting the next batch while the current one stays, dimmed.
    Refreshing,
    /// Nothing to suggest, so no section.
    Empty,
}

/// One visit's Recommended section.
pub struct Recommended {
    pub playlist: SharedString,
    pub name: SharedString,
    pub state: Arc<Mutex<Option<navia_core::recommend::Recommender>>>,
    pub rows: Vec<u32>,
    /// Added and on their way to the server. Hidden, and shown again if the server refuses.
    pub adding: Vec<u32>,
    pub phase: RecPhase,
    pub generation: u64,
}

impl Shell {
    /// The section's blocks. At first just a marker that requests suggestions once rendered, then the heading and rows.
    /// `held` is the playlist's songs, which drop out of the suggestions once added.
    pub fn recommended_blocks(&mut self, id: &SharedString, name: &SharedString, held: &[u32], new_visit: bool, layout: &Layout) -> Vec<Block> {
        self.recommended_list = None;
        let generation = self.recommended.as_ref().map_or(0, |r| r.generation + 1);
        if new_visit || self.recommended.as_ref().is_none_or(|r| &r.playlist != id) {
            self.recommended = Some(Recommended { playlist: id.clone(), name: name.clone(), state: Default::default(), rows: Vec::new(), adding: Vec::new(), phase: RecPhase::Waiting, generation });
        }
        let Some(rec) = self.recommended.as_mut() else { return Vec::new() };
        rec.name = name.clone();
        rec.rows.retain(|r| !held.contains(r));
        let rows: Vec<u32> = rec.rows.iter().copied().filter(|r| !rec.adding.contains(r)).collect();
        if rows.is_empty() {
            // First visit, or every suggestion added. Fetch the next batch once the place is near view.
            if rec.phase == RecPhase::Shown {
                rec.phase = RecPhase::Waiting;
            }
            if rec.phase != RecPhase::Waiting {
                return Vec::new();
            }
            return vec![Shell::plain_block(|this, _, _, cx| {
                if this.recommended.as_ref().is_some_and(|r| r.phase == RecPhase::Waiting) {
                    this.recommended_fetch(cx);
                }
                div().into_any_element()
            })];
        }
        let source = trf("Recommended for {name}", &[("name", name)]);
        let m = PageMetrics::of(layout);
        let list = self.add_list(rows.clone());
        self.page.list_sources[list] = Some(navia_core::queue_order::QueueSource { name: source, radio: false, link: Some(navia_core::queue_order::SourceLink::Playlist(id.to_string())) });
        self.recommended_list = Some(list);
        let mut blocks = vec![Block::Spacer(m.section_gap), Shell::plain_block(|this, layout, _, cx| this.render_recommended_head(layout, cx))];
        blocks.extend((0..rows.len()).map(|pos| Block::Table { list, pos, variant: RowVariant::Full }));
        blocks
    }

    /// Fetches the next batch off the UI thread.
    pub fn recommended_fetch(&mut self, cx: &mut Context<Self>) {
        let Some(core) = self.core.as_ref() else { return };
        let Some(rec) = self.recommended.as_mut() else { return };
        if matches!(rec.phase, RecPhase::Asking | RecPhase::Refreshing) {
            return;
        }
        let refreshing = !rec.rows.is_empty();
        rec.phase = if refreshing { RecPhase::Refreshing } else { RecPhase::Asking };
        rec.generation += 1;
        let (generation, id, state, lib) = (rec.generation, rec.playlist.to_string(), rec.state.clone(), core.lib.clone());
        let task = cx.background_executor().spawn(async move { lib.recommended_batch(&id, &state) });
        cx.spawn(async move |this, cx| {
            let rows = task.await;
            let _ = this.update(cx, |this, cx| this.on_recommended(generation, rows, cx));
        })
        .detach();
        cx.notify();
    }

    fn on_recommended(&mut self, generation: u64, rows: Vec<u32>, cx: &mut Context<Self>) {
        let Some(rec) = self.recommended.as_mut().filter(|r| r.generation == generation) else { return };
        if rows.is_empty() {
            if rec.phase == RecPhase::Refreshing {
                rec.phase = RecPhase::Shown;
                self.show_toast(tr("No other suggestions right now"), cx);
            } else {
                rec.phase = RecPhase::Empty;
            }
        } else {
            rec.rows = rows;
            rec.phase = RecPhase::Shown;
        }
        self.invalidate_page();
        cx.notify();
    }

    /// Whether the rows are waiting for Refresh's answer, so they dim.
    pub fn recommended_refreshing(&self, list: usize) -> bool {
        self.recommended_list == Some(list) && self.recommended.as_ref().is_some_and(|r| r.phase == RecPhase::Refreshing)
    }

    /// Adds the song to the end of the playlist, with the usual toast, and removes it from the suggestions.
    pub fn recommended_add(&mut self, song_ix: u32, cx: &mut Context<Self>) {
        let Some(rec) = self.recommended.as_mut() else { return };
        rec.adding.push(song_ix);
        let (id, name) = (rec.playlist.clone(), rec.name.clone());
        self.add_songs_to_playlist(id, name, vec![song_ix], cx);
        self.invalidate_page();
        cx.notify();
    }

    /// The playlist save finished. Added suggestions leave for good, and refused ones come back.
    pub fn recommended_add_done(&mut self, playlist: &str, saved: bool) {
        if let Some(rec) = self.recommended.as_mut().filter(|r| r.playlist.as_ref() == playlist) {
            if saved {
                let adding = std::mem::take(&mut rec.adding);
                rec.rows.retain(|r| !adding.contains(r));
            } else {
                rec.adding.clear();
            }
        }
    }

    /// "Recommended" with its hint, and Refresh at the right.
    fn render_recommended_head(&mut self, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let m = PageMetrics::of(layout);
        let busy = self.recommended.as_ref().is_some_and(|r| r.phase == RecPhase::Refreshing);
        let title = self.section_title(tr("Recommended").into(), None, cx);
        let refresh = small_glass_action(&t, "recommended-refresh")
            .child(if busy { tr("Refreshing…") } else { tr("Refresh") })
            .when(busy, |d| d.opacity(DISABLED_OPACITY))
            .when(!busy, |d| d.stop(&t).on_click(cx.listener(|this, _, _, cx| this.recommended_fetch(cx))));
        div()
            .px(t.s(m.gutter))
            .pb(t.s(SHELF_HEAD_GAP))
            .flex()
            .flex_row()
            .items_end()
            .gap(t.s(SHELF_HEAD_GAP))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .child(title)
                    .child(div().mt(t.s(2.0)).text_size(t.text(TEXT_SM)).text_color(t.t2()).line_height(relative(1.3)).font_weight(FontWeight::NORMAL).child(tr("Based on the songs in this playlist"))),
            )
            .child(refresh)
            .into_any_element()
    }

    /// A suggestion's actions: Add instead of Download and Favorite, then More.
    pub fn recommended_actions(&mut self, pos: usize, song_ix: u32, list: usize, width: gpui::Pixels, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let (color, hover): (gpui::Hsla, gpui::Hsla) = (t.t1().into(), t.hover_ink());
        div()
            .w(width)
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .justify_end()
            .pr(t.s(9.0))
            .child(small_glass_action(&t, ("rec-add", pos)).child(tr("Add")).stop(&t).on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.recommended_add(song_ix, cx);
            })))
            .child(crate::ui::icon_button_colored(&t, ("rec-more", pos), tr("More"), "dots-three-bold", false, color, hover).w(t.s(ACTION_W)).stop(&t).on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                cx.stop_propagation();
                this.open_list_song_menu(list, vec![pos], event.position(), true, cx);
            })))
            .into_any_element()
    }
}
