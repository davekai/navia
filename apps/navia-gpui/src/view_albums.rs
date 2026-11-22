//! Albums: a grid or a split list.

use crate::i18n::{tr, trn};
use gpui::prelude::*;
use crate::focus::FocusStop;
use std::rc::Rc;

use gpui::{AnyElement, Context, SharedString, div};

use crate::pages::{AlbumScope, Block, CardItem, PageMetrics, Route, grid_fit};
use crate::persist::PlayerMode;
use crate::shell::{Layout, Shell};
use crate::theme::size::*;
use crate::view_split::{SplitPage, SplitPane, SplitRows, split_glyph};
use crate::view_cards::{ArtShape, song_count};
use crate::view_menu::MenuRow;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AlbumSort {
    MostPlayed,
    /// Newest first, by the server's date added.
    Added,
    Name,
    Year,
}

pub struct AlbumsView {
    pub sort: AlbumSort,
    pub genre: Option<SharedString>,
    pub query: String,
    pub pane: SplitPane,
}

impl Default for AlbumsView {
    fn default() -> Self {
        Self { sort: AlbumSort::MostPlayed, genre: None, query: String::new(), pane: SplitPane::default() }
    }
}

const ALL: &str = "__all";

impl Shell {
    pub fn albums_sort_rows(&self) -> Vec<MenuRow> {
        let sorts: Vec<(AlbumSort, &str)> = vec![(AlbumSort::MostPlayed, tr("Most Played")), (AlbumSort::Added, tr("Date Added")), (AlbumSort::Name, tr("Name")), (AlbumSort::Year, tr("Year"))];
        sorts.into_iter().map(|(sort, label)| MenuRow::check(label, self.albums_view.sort == sort, move |this, cx| {
            this.albums_view.sort = sort;
            this.invalidate_page();
            cx.notify();
        })).collect()
    }

    pub fn albums_filter_rows(&self) -> Vec<MenuRow> {
        let mut rows = vec![MenuRow::check(tr("All Genres"), self.albums_view.genre.is_none(), |this, cx| {
            this.albums_view.genre = None;
            this.invalidate_page();
            cx.notify();
        })];
        for genre in self.genres.iter().take(10).map(|g| g.name.clone()) {
            let on = self.albums_view.genre.as_ref() == Some(&genre);
            rows.push(MenuRow::check(genre.clone(), on, move |this, cx| {
                this.albums_view.genre = Some(genre.clone());
                this.invalidate_page();
                cx.notify();
            }));
        }
        rows
    }

    pub fn route_scope(&self) -> AlbumScope {
        if let Route::Albums(scope) = &self.route { scope.clone() } else { AlbumScope::All }
    }

    fn album_list(&mut self, scope: &AlbumScope) -> Vec<u32> {
        let albums = self.albums.clone();
        let base: Vec<u32> = match scope {
            AlbumScope::Artist(id, section) => self.artist_section(id, *section),
            AlbumScope::All => (0..albums.len() as u32).collect(),
        };
        let needle = self.albums_view.query.trim().to_lowercase();
        let genre = self.albums_view.genre.clone();
        let mut found: Vec<u32> = base
            .into_iter()
            .filter(|i| {
                let a = &albums[*i as usize];
                genre.as_ref().is_none_or(|g| &a.genre == g) && (needle.is_empty() || a.name.to_lowercase().contains(&needle) || a.artist.to_lowercase().contains(&needle))
            })
            .collect();
        match self.albums_view.sort {
            AlbumSort::Name => found.sort_by_cached_key(|i| albums[*i as usize].name.to_lowercase()),
            AlbumSort::Year => found.sort_by_key(|i| std::cmp::Reverse(albums[*i as usize].year)),
            AlbumSort::Added => found.sort_by_key(|i| std::cmp::Reverse(albums[*i as usize].added)),
            AlbumSort::MostPlayed => found.sort_by_key(|i| std::cmp::Reverse(albums[*i as usize].plays)),
        }
        found
    }

    pub fn build_albums(&mut self, scope: AlbumScope, layout: &Layout) -> Vec<Block> {
        let m = PageMetrics::of(layout);
        let found = self.album_list(&scope);
        // Cards name the album artist, except in an artist's own sections.
        let by_artist = matches!(scope, AlbumScope::Artist(_, section) if section != crate::model::ReleaseSection::AppearsOn);
        if found.is_empty() {
            let filtered = !self.albums_view.query.trim().is_empty() || self.albums_view.genre.is_some();
            return vec![Shell::plain_block(move |this, _, _, cx| {
                let view = if filtered {
                    this.empty_state("magnifying-glass-regular", tr("No albums match"), tr("Nothing here matches your search or filter."), false).child(this.clear_filters_button(cx))
                } else {
                    this.library_empty_state("stack-regular", tr("No albums"), tr("Albums on the server show up here."), false)
                };
                view.into_any_element()
            })];
        }
        if self.split_on(SplitPage::Albums) {
            return vec![self.build_albums_split(found, layout)];
        }
        let (columns, _) = grid_fit(self.grid_width(layout), m.grid_card, m.card_gap);
        let items: Vec<CardItem> = found.into_iter().map(|index| CardItem::Album { index, by_artist }).collect();
        grid_blocks(items, columns)
    }

    pub fn clear_filters_button(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        crate::ui_controls::empty_action(&t, "clear-filters")
            .stop(&t).on_click(cx.listener(|this, _, _, cx| {
                this.albums_view.query.clear();
                this.albums_view.genre = None;
                this.search_active = false;
                this.invalidate_page();
                cx.notify();
            }))
            .child(tr("Clear Filters"))
            .into_any_element()
    }

    /// The album list beside the chosen album, or every listed album's songs.
    fn build_albums_split(&mut self, found: Vec<u32>, layout: &Layout) -> Block {
        let selected = self.albums_view.pane.selected.clone();
        let all = selected.as_deref() == Some(ALL);
        let current = if all { None } else { found.iter().copied().find(|i| selected.as_ref().is_none_or(|s| *s == self.albums[*i as usize].id)).or(found.first().copied()) };
        let shown = Some(current.map_or_else(|| SharedString::from(ALL), |i| self.albums[i as usize].id.clone()));
        let detail_layout = self.split_detail_layout(layout);
        let detail = match current {
            Some(i) => {
                let id = self.albums[i as usize].id.clone();
                self.build_album(&id, &detail_layout)
            }
            None => self.build_all_songs_table(&found, &detail_layout),
        };
        let total: u32 = found.iter().filter_map(|i| self.albums.get(*i as usize)).map(|a| a.song_count).sum();
        let count = found.len() + 1;
        let found = Rc::new(found);
        let rows: SplitRows = Rc::new(move |this, ix, layout, cx| this.render_album_split_row(ix, &found, total, layout, cx));
        self.split_block(SplitPage::Albums, shown, detail, count, TOPBAR_H + CONTENT_TOP, rows, layout)
    }

    fn render_album_split_row(&mut self, ix: usize, found: &[u32], total: u32, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let shown = self.albums_view.pane.shown().cloned();
        if ix == 0 {
            let label = format!("{} • {}", trn("{count} Album", "{count} Albums", found.len()), song_count(total));
            return self.split_row("all", 0, shown.as_deref() == Some(ALL), split_glyph(&t, "stack-regular"), tr("All Albums").into(), label.into(), cx, |this, cx| this.split_select(SplitPage::Albums, ALL.into(), cx));
        }
        let Some(album) = found.get(ix - 1).and_then(|i| self.albums.get(*i as usize)).cloned() else { return div().into_any_element() };
        let art = self.artwork(Some(&album.id), "music-notes-fill", SPLIT_ART, ArtShape::Rounded(R_SM), layout);
        let sub = [(album.year > 0).then(|| album.year.to_string()), Some(song_count(album.song_count).to_string())].into_iter().flatten().collect::<Vec<_>>().join(" • ");
        let id = album.id.clone();
        self.split_row("split", ix, shown.as_ref() == Some(&album.id), art, album.name.clone(), sub.into(), cx, move |this, cx| this.split_select(SplitPage::Albums, id.clone(), cx))
    }

    fn build_all_songs_table(&mut self, found: &[u32], layout: &Layout) -> Vec<Block> {
        let albums = self.albums.clone();
        let mut songs = Vec::new();
        for ix in found {
            songs.extend(self.album_tracks_cached(&albums[*ix as usize].id));
        }
        let list = self.add_list(songs);
        let mut blocks = vec![Shell::plain_block(|this, layout, _, cx| this.render_table_header(layout, None, None, cx))];
        let count = self.page.lists[list].len();
        let _ = layout;
        blocks.extend((0..count).map(|pos| Block::Table { list, pos, variant: crate::pages::RowVariant::Full }));
        blocks
    }

    pub fn bottom_padding(&self, layout: &Layout) -> f32 {
        if layout.mode == PlayerMode::Bar && layout.player { BAR_H + WINDOW_INSET + 24.0 } else { 0.0 }
    }
}

pub fn grid_blocks(items: Vec<CardItem>, columns: usize) -> Vec<Block> {
    let total = items.len();
    let items = std::rc::Rc::new(items);
    (0..total).step_by(columns).map(|start| Block::Grid { items: items.clone(), start, count: columns }).collect()
}
