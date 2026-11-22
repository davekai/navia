//! The Genres page and Search's landing page. Both show genre tiles for the top genres, then every other genre as a
//! compact row.


use crate::ui::FullTextTip;
use crate::i18n::{tr, trf, trn};
use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{AnyElement, Context, FontWeight, SharedString, div, px, relative};

use crate::art::{genre_fan_key, genre_tint_key};
use crate::color::tint_fg_dim;
use crate::focus::FocusStop;
use crate::model::GenrePage;
use crate::pages::{Block, CardItem, GenreTiles, PageMetrics, grid_fit};
use crate::shell::{Layout, Shell};
use crate::shell_data::CoverTint;
use crate::theme::size::*;
use crate::view_cards::ArtShape;

/// The Genres page's layout and filter text.
#[derive(Default)]
pub struct GenresView {
    pub page: Arc<GenrePage>,
    pub query: String,
}

impl Shell {
    pub fn build_browse(&mut self, layout: &Layout) -> Vec<Block> {
        let m = PageMetrics::of(layout);
        let page = self.genres_view.page.clone();
        if page.all.is_empty() {
            return vec![Self::no_genres()];
        }
        let tiles = |list: GenreTiles, n: usize| (0..n as u32).map(|i| CardItem::GenreFan(list, i)).collect::<Vec<_>>();
        let mut sections = Vec::new();
        if !page.top.is_empty() {
            sections.push(self.shelf_block(tr("Your Genres"), None, tiles(GenreTiles::Top, page.top.len()), true, 0));
        }
        if !page.browse.is_empty() {
            sections.push(self.shelf_block(tr("Browse by Genre"), None, tiles(GenreTiles::Browse, page.browse.len()), true, 0));
        }
        Shell::sections(m.section_gap, sections)
    }

    /// The tiles, hidden while filtering, then the matching rows several to a line. Each line is its own block, so only
    /// lines near the screen are built.
    pub fn build_genres(&mut self, layout: &Layout) -> Vec<Block> {
        let m = PageMetrics::of(layout);
        let page = self.genres_view.page.clone();
        if page.all.is_empty() {
            return vec![Self::no_genres()];
        }
        let needle = self.genres_view.query.trim().to_lowercase();
        let mut sections = Vec::new();
        if needle.is_empty() {
            let tiles: Vec<CardItem> = (0..page.top.len() as u32).map(|i| CardItem::GenreFan(GenreTiles::Top, i)).collect();
            sections.push(self.shelf_block(tr("Your Top Genres"), None, tiles, true, 0));
        }
        let shown: Vec<u32> = page.all.iter().enumerate().filter(|(_, g)| needle.is_empty() || g.name.to_lowercase().contains(&needle)).map(|(i, _)| i as u32).collect();
        if shown.is_empty() {
            let query = self.genres_view.query.clone();
            sections.push(vec![Shell::plain_block(move |this, _, _, cx| {
                this.empty_state("magnifying-glass-regular", tr("No genres match"), trf("Nothing matches “{query}”.", &[("query", &query)]), true)
                    .child(this.clear_genre_filter_button(cx))
                    .into_any_element()
            })]);
        } else {
            let (columns, _) = grid_fit(self.grid_width(layout), m.genre_chip_w, m.tile_gap);
            let shown = Rc::new(shown);
            let mut blocks = vec![Self::section_heading(tr("All Genres").into())];
            for start in (0..shown.len()).step_by(columns) {
                let shown = shown.clone();
                blocks.push(Shell::plain_block(move |this, layout, _, cx| this.render_genre_line(&shown, start, columns, layout, cx)));
            }
            sections.push(blocks);
        }
        Shell::sections(m.section_gap, sections)
    }

    fn no_genres() -> Block {
        Shell::plain_block(|this, _, _, _| this.library_empty_state("guitar-regular", tr("No genres yet"), tr("Genres from the server's tags show up here."), false).into_any_element())
    }

    fn clear_genre_filter_button(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        crate::ui_controls::empty_action(&t, "clear-genre-search")
            .stop(&t)
            .on_click(cx.listener(|this, _, _, cx| {
                this.genres_view.query.clear();
                this.search_active = false;
                this.invalidate_page();
                cx.notify();
            }))
            .child(tr("Clear Search"))
            .into_any_element()
    }

    /// The album ids of a genre's covers.
    fn genre_cover_ids(&self, covers: &[u32]) -> Vec<SharedString> {
        covers.iter().filter_map(|c| self.albums.get(*c as usize).map(|a| a.id.clone())).collect()
    }

    /// A genre tile: name and album count over the tint of its covers, with the covers fanned in the corner. A genre
    /// without covers gets a plain tile.
    pub fn render_genre_tile(&mut self, list: GenreTiles, index: u32, width: f32, group: SharedString, ix: usize, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let page = &self.genres_view.page;
        let tiles = match list {
            GenreTiles::Top => &page.top,
            GenreTiles::Browse => &page.browse,
        };
        let Some(genre) = tiles.get(index as usize).cloned() else { return div().into_any_element() };
        let ids = self.genre_cover_ids(&genre.covers);
        let height = width / GENRE_TILE_RATIO;
        let device_w = (f32::from(t.s(width)) * layout.scale).round() as u32;
        let px_per_token = f32::from(t.s(1.0)) * layout.scale;
        let (tint, fans) = if ids.is_empty() {
            (None, [None, None])
        } else {
            let tint = match self.playlist_cover_tint(&genre_tint_key(&ids)) {
                CoverTint::Ready(tint) => tint,
                CoverTint::Loading => None,
            };
            (tint, [false, true].map(|hover| self.thumb_faded(&genre_fan_key(hover, px_per_token, &ids), device_w)))
        };
        let fill = crate::ui::tile_fill_bg(&t, tint);
        let count_color: gpui::Hsla = tint.map_or(t.t2().into(), |c| tint_fg_dim(c).hsla(1.0));
        let [rest, spread] = fans;
        let (hover, ticker) = crate::ui::card_hover(&group, crate::theme::motion::MED);
        // the spread fan fades in over the resting one while the pointer is on the tile.
        let fan = |image: Option<(Arc<gpui::RenderImage>, f32)>, shown: f32| {
            image.map(|(image, fade)| div().absolute().top(px(0.0)).left(px(0.0)).size_full().opacity(shown).child(crate::ui::image(image).size_full().opacity(fade)))
        };
        let rest_el = fan(rest, 1.0 - hover);
        let hover_el = fan(spread, hover);
        let name = genre.name.clone();
        let text = div()
            .absolute()
            .top(px(0.0))
            .left(px(0.0))
            .right(px(0.0))
            .flex()
            .flex_col()
            .gap(t.s(2.0))
            .pt(t.s(width * GENRE_TILE_PAD_Y))
            .px(t.s(width * GENRE_TILE_PAD_X))
            .child(div().text_size(t.text(TEXT_FEATURE)).font_weight(FontWeight::SEMIBOLD).line_height(relative(1.15)).text_color(t.t_em()).line_clamp(2).text_ellipsis().id("name").child(genre.name.clone()).full_text_tip(&t, genre.name.clone()))
            .child(div().text_size(t.text(TEXT_SM)).text_color(count_color).child(trn("{count} Album", "{count} Albums", genre.albums as usize)));
        let body = div()
            .relative()
            .size_full()
            .rounded(t.s(R_MD))
            .overflow_hidden()
            .bg(fill)
            .child(ticker)
            .children(rest_el)
            .children(hover_el)
            .child(text);
        crate::ui::press_dim(div().id(("card", ix)))
            .relative()
            .flex_none()
            .w(t.s(width))
            .h(t.s(height))
            .child(body)
            .child(crate::ui::card_track(&group))
            .stop(&t)
            .on_click(cx.listener(move |this, _, _, cx| this.open_genre(name.clone(), cx)))
            .into_any_element()
    }

    /// One line of genre rows, `columns` wide, like an auto-fill grid.
    fn render_genre_line(&mut self, shown: &Rc<Vec<u32>>, start: usize, columns: usize, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let m = PageMetrics::of(layout);
        let (_, cell) = grid_fit(self.grid_width(layout), m.genre_chip_w, m.tile_gap);
        let mut line = div().flex().flex_row().gap(t.s(m.tile_gap)).px(t.s(m.gutter));
        for pos in start..(start + columns).min(shown.len()) {
            line = line.child(self.render_genre_row(shown[pos], cell, layout, cx));
        }
        line.into_any_element()
    }

    /// A compact genre row: one cover, the name on up to two lines, and the album count.
    fn render_genre_row(&mut self, index: u32, width: f32, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let Some(genre) = self.genres_view.page.all.get(index as usize).cloned() else { return div().into_any_element() };
        let cover = self.genre_cover_ids(&genre.covers).into_iter().next();
        let art = self.artwork(cover.as_ref(), "music-notes-fill", THUMB, ArtShape::Rounded(R_XS), layout);
        let name = genre.name.clone();
        let (pad, gap) = (GENRE_ROW_PAD, GENRE_ROW_GAP);
        div()
            .id(("genre-row", index as usize))
            .relative()
            .flex()
            .flex_row()
            .items_center()
            .gap(t.s(gap))
            .w(t.s(width))
            .h(t.s(MINI_ROW_H))
            .px(t.s(pad))
            .rounded(t.s(R_ROW))
            .child(crate::ui::hover_fill(crate::ui::HoverShape::Rounded(t.s(R_ROW)), t.row(), t.press()))
            .child(art)
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .line_height(relative(1.3))
                    .child(div().font_weight(FontWeight::MEDIUM).line_clamp(2).text_ellipsis().id("name").child(genre.name.clone()).full_text_tip(&t, genre.name.clone()))
                    .child(div().truncate().text_size(t.text(TEXT_SM)).text_color(t.t2()).child(trn("{count} Album", "{count} Albums", genre.albums as usize))),
            )
            .child(div().absolute().left(t.s(pad + THUMB + gap)).right(t.s(pad)).bottom(px(0.0)).h(px(1.0)).bg(t.line()))
            .stop(&t)
            .on_click(cx.listener(move |this, _, _, cx| this.open_genre(name.clone(), cx)))
            .into_any_element()
    }
}
