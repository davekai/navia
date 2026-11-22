//! Artists: a grid of every artist, or a split view with the list beside the chosen artist. Search and the filter row
//! narrow it through the core's search index.

use std::collections::HashSet;
use std::sync::Arc;

use crate::focus::FocusStop;
use std::rc::Rc;

use crate::i18n::{tr, trf, trn};
use crate::pages::{Block, CardItem, PageMetrics, grid_fit};
use crate::shell::{Layout, Shell};
use crate::theme::size::{CHIP_GAP, CHIP_H, CONTENT_TOP, FILTER_ROW_GAP, SPLIT_ART, TOPBAR_H};
use crate::view_cards::{ArtShape, song_count};
use crate::view_split::{SplitPage, SplitPane, SplitRows};
use crate::view_albums::grid_blocks;
use crate::view_filters::FilterPage;
use gpui::prelude::*;
use gpui::{Context, SharedString};

/// The Artists search text, and the ids of the artists matching it and the filters. Empty while nothing narrows the
/// list.
#[derive(Default)]
pub struct ArtistsView {
    pub query: String,
    matches: Option<HashSet<SharedString>>,
    /// How many artists show while narrowed, for the title.
    shown: usize,
    /// The newest request's number. Older answers are dropped.
    generation: u64,
    pub pane: SplitPane,
    /// Per-artist facts for the split list's album and song counts. None until loaded, and in sample mode.
    counts: Option<Arc<navia_core::artist_filters::ArtistFilterIndex>>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

impl Shell {
    /// The search or filters changed. Asks the core for matches in the background, or matches names by substring with
    /// sample data.
    pub fn artists_query_changed(&mut self) {
        let needle = self.artists_view.query.trim().to_string();
        let filters = self.artist_filters.state.clone();
        let Some(core) = self.core.as_ref() else {
            let needle = needle.to_lowercase();
            self.artists_view.matches = (!needle.is_empty()).then(|| self.artists.iter().filter(|a| a.name.to_lowercase().contains(&needle)).map(|a| a.id.clone()).collect());
            self.count_shown_artists();
            return;
        };
        self.artists_view.generation += 1;
        let (lib, tx, generation) = (core.lib.clone(), core.events.clone(), self.artists_view.generation);
        let want_index = self.artist_filters.wants_index();
        self.artist_filters.requested |= want_index;
        std::thread::spawn(move || {
            let matches = (!needle.is_empty() || !filters.is_empty()).then(|| {
                let query = (!needle.is_empty()).then_some(needle.as_str());
                lib.db.filtered_artists(query, &filters, now_ms()).unwrap_or_default().into_iter().map(|a| SharedString::from(a.artist.id)).collect()
            });
            let index = if want_index { lib.db.artist_filters().ok() } else { None };
            let _ = tx.unbounded_send(crate::core_player::CoreEvent::ArtistMatches { generation, matches, index });
        });
    }

    pub fn on_artist_matches(&mut self, generation: u64, matches: Option<HashSet<SharedString>>, index: Option<Arc<navia_core::artist_filters::ArtistFilterIndex>>, cx: &mut Context<Self>) {
        if let Some(index) = index {
            self.artists_view.counts = Some(index.clone());
            self.set_filter_index(FilterPage::Artists, index, cx);
        }
        if generation != self.artists_view.generation {
            return;
        }
        self.artists_view.matches = matches;
        self.count_shown_artists();
        self.invalidate_page();
        cx.notify();
    }

    fn count_shown_artists(&mut self) {
        self.artists_view.shown = match &self.artists_view.matches {
            Some(ids) => self.artists.iter().filter(|a| ids.contains(&a.id)).count(),
            None => self.artists.len(),
        };
    }

    /// The match count after the title while the list is narrowed.
    pub fn artists_title_detail(&self) -> Option<SharedString> {
        let narrowed = !self.artists_view.query.trim().is_empty() || self.artist_filters.is_filtering();
        narrowed.then(|| crate::i18n::trn("{count} match", "{count} matches", self.artists_view.shown).into())
    }

    pub fn build_artists(&mut self, layout: &Layout) -> Vec<Block> {
        let m = PageMetrics::of(layout);
        if self.artists.is_empty() {
            return vec![Shell::plain_block(|this, _, _, _| this.library_empty_state("microphone-stage-regular", tr("No artists yet"), tr("Artists on the server show up here."), false).into_any_element())];
        }
        // The filter facts are read once the page is first shown.
        if self.artist_filters.wants_index() && self.core.is_some() {
            self.artists_query_changed();
        }
        let mut blocks = Vec::new();
        let filter_row = self.artist_filters.has_row();
        if filter_row {
            blocks.push(Shell::plain_block(|this, layout, window, cx| this.render_filter_row(FilterPage::Artists, layout, window, cx).map_or_else(|| gpui::div().into_any_element(), |row| row.into_any_element())));
            blocks.push(Block::Spacer(FILTER_ROW_GAP));
        }
        let shown: Vec<u32> = match &self.artists_view.matches {
            Some(ids) => self.artists.iter().enumerate().filter(|(_, a)| ids.contains(&a.id)).map(|(i, _)| i as u32).collect(),
            None => (0..self.artists.len() as u32).collect(),
        };
        if shown.is_empty() {
            if self.artist_filters.is_filtering() {
                blocks.push(Shell::plain_block(|this, _, _, cx| this.render_filters_empty(FilterPage::Artists, cx).into_any_element()));
                return blocks;
            }
            let query = self.artists_view.query.clone();
            blocks.push(Shell::plain_block(move |this, _, _, cx| {
                let t = this.theme;
                this.empty_state("magnifying-glass-regular", tr("No artists match"), trf("Nothing here matches \u{201c}{query}\u{201d}.", &[("query", &query)]), false)
                    .child(crate::ui_controls::empty_action(&t, "artists-clear-search").child(tr("Clear Search")).stop(&t).on_click(cx.listener(|this, _, _, cx| {
                        this.artists_view.query.clear();
                        this.artists_query_changed();
                        this.search_active = false;
                        this.invalidate_page();
                        cx.notify();
                    })))
                    .into_any_element()
            }));
            return blocks;
        }
        if self.split_on(SplitPage::Artists) {
            let above = TOPBAR_H + if filter_row { CHIP_GAP + CHIP_H + FILTER_ROW_GAP } else { CONTENT_TOP };
            blocks.push(self.build_artists_split(shown, above, layout));
            return blocks;
        }
        let (columns, _) = grid_fit(self.grid_width(layout), m.grid_card, m.card_gap);
        let items: Vec<CardItem> = shown.into_iter().map(CardItem::Artist).collect();
        blocks.extend(grid_blocks(items, columns));
        blocks
    }

    /// The list beside the chosen artist's page. Shows the first artist when none is chosen or it's filtered out.
    fn build_artists_split(&mut self, shown: Vec<u32>, above: f32, layout: &Layout) -> Block {
        let selected = self.artists_view.pane.selected.clone();
        let current = shown.iter().copied().find(|i| selected.as_ref().is_some_and(|s| *s == self.artists[*i as usize].id)).or(shown.first().copied());
        let id = current.map(|i| self.artists[i as usize].id.clone());
        let detail_layout = self.split_detail_layout(layout);
        let detail = id.as_ref().map(|id| self.build_artist(id, &detail_layout)).unwrap_or_default();
        let count = shown.len();
        let shown = Rc::new(shown);
        let rows: SplitRows = Rc::new(move |this, ix, layout, cx| this.render_artist_split_row(ix, &shown, layout, cx));
        self.split_block(SplitPage::Artists, id, detail, count, above, rows, layout)
    }

    /// An artist row with photo, name, and album and song counts.
    fn render_artist_split_row(&mut self, ix: usize, shown: &[u32], layout: &Layout, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(artist) = shown.get(ix).and_then(|i| self.artists.get(*i as usize)).cloned() else { return gpui::div().into_any_element() };
        let art = self.artwork(artist.image.as_ref(), "user-fill", SPLIT_ART, ArtShape::Circle, layout);
        let sub = self.artists_view.counts.as_ref().and_then(|c| c.counts(&artist.id)).map(|c| format!("{} • {}", trn("{count} Album", "{count} Albums", c.albums as usize), song_count(c.songs))).unwrap_or_default();
        let on = self.artists_view.pane.shown() == Some(&artist.id);
        let id = artist.id.clone();
        self.split_row("artist-split", ix, on, art, artist.name.clone(), sub.into(), cx, move |this, cx| this.split_select(SplitPage::Artists, id.clone(), cx))
    }
}
