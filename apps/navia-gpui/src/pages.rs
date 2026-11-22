//! Routes and the page frame. Every page is a list of blocks virtualized by one `gpui::list`,
//! so a page costs no more than what's on screen.

use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{AnyElement, Context, ListAlignment, ListState, Pixels, ScrollHandle, SharedString, Window, div, list, px};

use crate::shell::{Layout, ListId, Shell};
use crate::theme::size::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DiscoverTab {
    ForYou,
    New,
    Charts,
    Radio,
}

#[derive(Clone, PartialEq, Debug)]
pub enum AlbumScope {
    All,
    /// One section of an artist's discography, from the artist page's "See all".
    Artist(SharedString, crate::model::ReleaseSection),
}

#[derive(Clone, PartialEq, Debug)]
pub enum Route {
    Home,
    Discover(DiscoverTab),
    Albums(AlbumScope),
    Album(SharedString),
    Artists,
    Artist(SharedString),
    /// the full list of an artist's top songs.
    ArtistTop(SharedString),
    Playlists,
    Playlist(SharedString),
    Songs,
    /// The built-in list of this user's favorited songs.
    Liked,
    Browse,
    Genres,
    Search,
    Settings,
    Downloads,
    CachedSongs,
    /// Settings > Library > Share Links.
    Shares,
    Equalizer,
    /// The smart playlist editor, for a new playlist or an existing one by id.
    SmartEditor(Option<SharedString>),
    /// One of today's For You mixes, by its id.
    Mix(SharedString),
    /// an upcoming album from Deezer, by Deezer's album id (`navia_core::upcoming`).
    Upcoming(u64),
}

pub type BlockFn = Rc<dyn Fn(&mut Shell, &Layout, &mut Window, &mut Context<Shell>) -> AnyElement>;

/// One thing in a page's list, measured and drawn only when near the viewport.
#[derive(Clone)]
pub enum Block {
    Spacer(f32),
    View(BlockFn),
    Shelf(Rc<Shelf>),
    Grid { items: Rc<Vec<CardItem>>, start: usize, count: usize },
    Table { list: usize, pos: usize, variant: RowVariant },
    Mini { list: usize, pos: usize, columns: bool },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RowVariant {
    Full,
    Album,
}

/// Which of `GenrePage`'s tile lists a genre tile comes from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GenreTiles {
    /// The user's top genres, on the Genres page and in Search.
    Top,
    /// Search's "Browse by Genre".
    Browse,
}

#[derive(Clone, Debug)]
pub enum CardItem {
    Album { index: u32, by_artist: bool },
    Artist(u32),
    Playlist(u32),
    Essentials(u32),
    SongRadio(u32),
    /// A genre tile, by its place in one of the genre page's tile lists.
    GenreFan(GenreTiles, u32),
    /// A For You mix, by its place in the page's mixes.
    Mix(u32),
    /// An Artist Radio tile on Home. The seed artist and its neighbors, as indexes into `artists`.
    ArtistStation { seed: u32, neighbours: Rc<Vec<u32>> },
    /// an upcoming album; `tag` names its state ("Upcoming", "Out now") instead of counting down, in lists it shares with library albums.
    Upcoming { album: Rc<crate::model::Upcoming>, tag: bool },
}

pub struct Shelf {
    pub id: usize,
    pub title: SharedString,
    pub link: Option<Route>,
    /// The Albums sort the link opens with, such as date added for Recently Added.
    pub link_sort: Option<crate::view_albums::AlbumSort>,
    pub items: Rc<Vec<CardItem>>,
    pub tiles: bool,
    pub scroll: ScrollHandle,
    /// Lines of subtitle under the title, for the row's fixed height.
    pub sub_lines: u32,
    /// A quiet line under the title, such as On This Day's date.
    pub note: Option<SharedString>,
    /// A text button at the heading's right that opens `link` instead of the title, such as an
    /// album's "See Discography".
    pub more: Option<&'static str>,
}

/// Page sizes, which shrink when the window is narrow.
#[derive(Clone, Copy)]
pub struct PageMetrics {
    pub gutter: f32,
    pub card_w: f32,
    pub card_gap: f32,
    pub tile_gap: f32,
    pub grid_card: f32,
    pub section_gap: f32,
    pub section_title: f32,
    pub title: f32,
    pub hero_art: f32,
    /// The genre rows' narrowest width.
    pub genre_chip_w: f32,
}

impl PageMetrics {
    pub fn of(layout: &Layout) -> Self {
        // A narrow window shrinks the sizes, except the gutter. The page beside the split list uses
        // the split's gutter.
        let mut m = if f32::from(layout.viewport.width) <= NARROW_WINDOW {
            Self { gutter: GUTTER, card_w: NARROW_CARD_W, card_gap: NARROW_CARD_GAP, tile_gap: NARROW_TILE_GAP, grid_card: NARROW_GRID_CARD, section_gap: NARROW_SECTION_GAP, section_title: NARROW_TEXT_SECTION, title: NARROW_TEXT_TITLE, hero_art: NARROW_HERO_ART, genre_chip_w: NARROW_GENRE_CHIP_W }
        } else {
            Self { gutter: GUTTER, card_w: CARD_W, card_gap: CARD_GAP, tile_gap: TILE_GAP, grid_card: GRID_CARD, section_gap: SECTION_GAP, section_title: TEXT_SECTION, title: TEXT_TITLE, hero_art: HERO_ART, genre_chip_w: GENRE_CHIP_W }
        };
        if layout.embedded {
            m.gutter = SPLIT_GUTTER;
        }
        m
    }
}

pub struct Page {
    pub route: Option<Route>,
    pub blocks: Rc<Vec<Block>>,
    pub state: ListState,
    /// Song lists behind `Block::Table` and `Block::Mini` rows.
    pub lists: Vec<Arc<Vec<u32>>>,
    /// What each list plays from when it isn't the page itself, such as a shelf.
    pub list_sources: Vec<Option<navia_core::queue_order::QueueSource>>,
    pub tint: Option<crate::color::Srgb>,
    width_bucket: i32,
    data: u64,
}

impl Default for Page {
    fn default() -> Self {
        Self { route: None, blocks: Rc::new(Vec::new()), state: ListState::new(0, ListAlignment::Top, px(crate::theme::size::NEAR_VIEW_MARGIN)), lists: Vec::new(), list_sources: Vec::new(), tint: None, width_bucket: 0, data: 0 }
    }
}

/// at most this many blocks, a page is still waiting for its content; more arriving fades in (`LAND_FADE`).
const LAND_WAITING_BLOCKS: usize = 3;

impl Shell {
    /// the page's content width in tokens, between its gutters.
    pub fn grid_width(&self, layout: &Layout) -> f32 {
        f32::from(layout.settled_width) / self.theme.token_scale() - PageMetrics::of(layout).gutter * 2.0
    }

    pub fn page_uses_list(&self) -> bool {
        !matches!(self.route, Route::Songs)
    }

    /// Bumps the page version so the current page rebuilds. Filters, sorts and search use it.
    pub fn invalidate_page(&mut self) {
        self.page_data += 1;
    }

    /// Rebuilds the blocks when the page's content or its column count could have changed.
    pub fn ensure_page(&mut self, layout: &Layout) {
        let bucket = (f32::from(layout.settled_width) / 8.0) as i32 + (f32::from(layout.viewport.height) / 40.0) as i32 * 1000 + ((self.theme.zoom * 100.0) as i32) * 1_000_000;
        let same_route = self.page.route.as_ref() == Some(&self.route);
        if same_route && self.page.data == self.page_data && self.page.width_bucket == bucket {
            return;
        }
        let keep_scroll = same_route;
        // the top spacer and at most a header and a placeholder: the page was waiting for its content.
        let before = self.page.blocks.len();
        let was_waiting = before <= LAND_WAITING_BLOCKS;
        self.page.lists.clear();
        let blocks = self.build_page(layout);
        let count = blocks.len();
        if !same_route {
            self.page_land = None;
        } else if was_waiting && count > before + 1 && !crate::theme::motion::reduced() {
            self.page_land = Some(std::time::Instant::now());
        }
        self.page.blocks = Rc::new(blocks);
        self.page.route = Some(self.route.clone());
        self.page.data = self.page_data;
        self.page.width_bucket = bucket;
        if keep_scroll {
            let top = self.page.state.logical_scroll_top();
            self.page.state.reset(count);
            self.page.state.scroll_to(gpui::ListOffset { item_ix: top.item_ix.min(count.saturating_sub(1)), offset_in_item: top.offset_in_item });
        } else {
            self.page.state.reset(count);
        }
    }

    fn build_page(&mut self, layout: &Layout) -> Vec<Block> {
        self.shelf_counter = 0;
        self.page_countdown = false;
        let m = PageMetrics::of(layout);
        let mut blocks = Vec::new();
        // A filter row sits `CHIP_GAP` under the top bar, in place of the content top.
        let filter_row = self.route == Route::Artists && !self.artists.is_empty() && self.artist_filters.has_row();
        blocks.push(Block::Spacer(TOPBAR_H + if filter_row { CHIP_GAP } else { CONTENT_TOP }));
        let content = match self.route.clone() {
            Route::Home => self.build_home(layout),
            Route::Discover(tab) => self.build_discover(tab, layout),
            Route::Albums(scope) => self.build_albums(scope, layout),
            Route::Album(id) => self.build_album(&id, layout),
            Route::Artists => self.build_artists(layout),
            Route::Artist(id) => self.build_artist(&id, layout),
            Route::ArtistTop(id) => self.build_artist_top(&id, layout),
            Route::Playlists => self.build_playlists(layout),
            Route::Liked => self.build_liked(layout),
            Route::Playlist(id) => self.build_playlist(&id, layout),
            Route::Browse => self.build_browse(layout),
            Route::Genres => self.build_genres(layout),
            Route::Search => self.build_search(layout),
            Route::Settings => self.build_settings(layout),
            Route::Downloads => self.build_downloads(layout),
            Route::CachedSongs => self.build_cached(layout),
            Route::Shares => self.build_shares(layout),
            Route::Equalizer => self.build_equalizer(layout),
            Route::SmartEditor(id) => self.build_smart_editor(id, layout),
            Route::Mix(id) => self.build_mix(&id, layout),
            Route::Upcoming(id) => self.build_upcoming(id, layout),
            Route::Songs => Vec::new(),
        };
        blocks.extend(content);
        // A split fills the window, and each side keeps its own end padding.
        let split = match self.route {
            Route::Albums(_) => self.split_on(crate::view_split::SplitPage::Albums),
            Route::Artists => self.split_on(crate::view_split::SplitPage::Artists),
            _ => false,
        };
        if !split {
            blocks.push(Block::Spacer(self.page_end(layout)));
        }
        let _ = m;
        blocks
    }

    /// The space after a page's last block, clearing the bottom bar when it shows.
    pub fn page_end(&self, layout: &Layout) -> f32 {
        48.0 + self.bottom_padding(layout)
    }

    /// Blocks separated by the section gap.
    pub fn sections(gap: f32, sections: Vec<Vec<Block>>) -> Vec<Block> {
        let mut out = Vec::new();
        for (i, section) in sections.into_iter().filter(|s| !s.is_empty()).enumerate() {
            if i > 0 {
                out.push(Block::Spacer(gap));
            }
            out.extend(section);
        }
        out
    }

    pub fn add_list(&mut self, songs: Vec<u32>) -> usize {
        self.page.lists.push(Arc::new(songs));
        self.page.list_sources.push(None);
        self.page.lists.len() - 1
    }

    pub fn next_shelf_id(&mut self) -> usize {
        self.shelf_counter += 1;
        self.shelf_counter
    }

    /// How far left of the main column the page list reaches, so cards can pass under the sidebar.
    /// Zero for a page embedded in another, such as the Albums split's right side.
    pub fn sidebar_under(&self, layout: &Layout) -> Pixels {
        if crate::ui::has_backdrop_blur() && !layout.embedded && layout.sidebar_visible && !layout.sidebar_overlay && self.page_uses_list() {
            layout.sidebar_width * layout.sidebar_shown
        } else {
            px(0.0)
        }
    }

    /// Song rows laid out in the page's viewport. That's the table on the Songs page, the Table and
    /// Mini blocks on a block page, or the right side in the Albums split.
    pub fn visible_song_rows(&self) -> usize {
        let count = |state: &gpui::ListState, blocks: &[Block]| {
            let view = state.viewport_bounds();
            blocks
                .iter()
                .enumerate()
                .filter(|(_, b)| matches!(b, Block::Table { .. } | Block::Mini { .. }))
                .filter(|(ix, _)| state.bounds_for_item(*ix).is_some_and(|b| b.intersects(&view)))
                .count()
        };
        if !self.page_uses_list() {
            // The song table's rows that fit its laid-out height.
            let height = f32::from(self.list_scroll.0.borrow().base_handle.bounds().size.height);
            let row = f32::from(self.theme.s(crate::theme::size::ROW_H)).max(1.0);
            return if self.order.is_empty() { 0 } else { (height / row).floor().max(0.0) as usize };
        }
        count(&self.page.state, &self.page.blocks) + [&self.albums_view.pane, &self.artists_view.pane].iter().map(|p| count(&p.state, &p.blocks)).sum::<usize>()
    }

    pub fn render_page(&mut self, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        self.ensure_page(layout);
        let under = self.sidebar_under(layout);
        div()
            .absolute()
            .top(px(0.0))
            .left(-under)
            .bottom(px(0.0))
            .w(layout.main_width + under)
            .child(list(self.page.state.clone(), cx.processor(|this, ix: usize, window, cx| this.render_block(ix, window, cx))).size_full())
            .into_any_element()
    }

    pub fn render_block(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let blocks = self.page.blocks.clone();
        self.render_blocks_at(&blocks, ix, window, cx)
    }

    pub fn render_blocks_at(&mut self, blocks: &Rc<Vec<Block>>, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let layout = self.layout_ref(window);
        let t = self.theme;
        let Some(block) = blocks.get(ix).cloned() else { return div().into_any_element() };
        let under = self.sidebar_under(&layout);
        // Always wrap in a plain block. A list item's root is sized to its content, so a flex row with
        // something pushed right only spans the page inside one.
        let keep_clear = |el: AnyElement| div().pl(under).child(el).into_any_element();
        let landing = self.land_opacity(ix);
        let el = match block {
            Block::Spacer(h) => div().h(t.s(h)).into_any_element(),
            Block::View(render) => keep_clear(render(self, &layout, window, cx)),
            Block::Shelf(shelf) => self.render_shelf(&shelf, &layout, cx),
            Block::Grid { items, start, count } => keep_clear(self.render_grid_row(&items, start, count, &layout, cx)),
            Block::Table { list, pos, variant } => {
                let row = self.render_table_row(list, pos, variant, &layout, cx);
                // A row sliding aside for a drag is offset inside its block, so the list's own layout, where
                // the gap is found, stays put.
                keep_clear(if self.row_dragging() { div().child(row).into_any_element() } else { row })
            }
            Block::Mini { list, pos, columns } => keep_clear(self.render_mini_pair(list, pos, columns, &layout, cx)),
        };
        match landing {
            Some(opacity) => div().opacity(opacity).child(el).into_any_element(),
            None => el,
        }
    }

    /// how far block `ix` has faded in while late content lands: each a `LAND_STAGGER` after the one before (the top
    /// spacer is block 0), those past `LAND_STAGGER_MAX` together; None once in.
    fn land_opacity(&self, ix: usize) -> Option<f32> {
        use crate::theme::motion;
        let since = self.page_land?;
        let slow = crate::testing::slow_motion();
        let delay = ix.saturating_sub(1).min(motion::LAND_STAGGER_MAX) as f32 * motion::LAND_STAGGER * slow;
        let p = motion::progress(since.elapsed().as_secs_f32() - delay, motion::LAND_FADE * slow);
        (p < 1.0).then(|| motion::cubic_bezier(motion::EASE, p.max(0.0)))
    }

    /// frames while landing blocks fade in; a child of the main column.
    pub fn land_ticker(&self) -> Option<AnyElement> {
        use crate::theme::motion;
        let since = self.page_land?;
        let slow = crate::testing::slow_motion();
        let end = since + std::time::Duration::from_secs_f32((motion::LAND_STAGGER * motion::LAND_STAGGER_MAX as f32 + motion::LAND_FADE) * slow);
        (std::time::Instant::now() < end).then(|| {
            gpui::canvas(|_, _, _| {}, move |_, _, window, _| {
                if std::time::Instant::now() < end {
                    crate::animation::request_frame(window);
                }
            })
            .absolute()
            .size_0()
            .into_any_element()
        })
    }

    /// The song list for a page row, after confirming the row still exists.
    pub fn list_song(&self, list: usize, pos: usize) -> Option<(u32, Arc<Vec<u32>>)> {
        let songs = self.page.lists.get(list)?.clone();
        Some((*songs.get(pos)?, songs))
    }

    pub fn page_list_id(list: usize) -> ListId {
        ListId::Page(list)
    }

    /// Whether the main area has moved from its resting position.
    pub fn page_scrolled(&self) -> bool {
        self.page_uses_list() && self.page.route.is_some() && f32::from(self.page.state.scroll_px_offset_for_scrollbar().y) < -0.5
    }

    pub fn page_scroll_y(&self) -> Pixels {
        self.page.state.scroll_px_offset_for_scrollbar().y
    }
}

/// as many columns at least `cell` wide as fit `width` with `gap` between, and the width each stretches to.
pub fn grid_fit(width: f32, cell: f32, gap: f32) -> (usize, f32) {
    let columns = (((width + gap) / (cell + gap)).floor() as usize).max(1);
    (columns, (width - gap * (columns - 1) as f32) / columns as f32)
}
