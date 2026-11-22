//! Search results: a top result card, songs, artists and albums.

use crate::ui::FullTextTip;
use crate::i18n::{tr, trf};
use gpui::prelude::*;
use crate::focus::FocusStop;
use gpui::{AnyElement, Context, FontWeight, div, relative};

use crate::model::Artist;
use crate::pages::{Block, CardItem, PageMetrics, Route};
use crate::shell::{Layout, Shell};
use crate::theme::size::*;
use crate::ui::{glass_pill, icon, icon_button};
use crate::view_cards::ArtShape;
use crate::view_menu::MenuRow;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SearchSort {
    Relevance,
    Name,
    Year,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SearchScope {
    All,
    Artists,
    Albums,
    Songs,
}

impl SearchScope {
    pub fn label(self) -> &'static str {
        match self {
            SearchScope::All => tr("All Results"),
            SearchScope::Artists => tr("Artists"),
            SearchScope::Albums => tr("Albums"),
            SearchScope::Songs => tr("Songs"),
        }
    }
}

/// Results requested from the core for each kind. The page shows fewer.
const SEARCH_LIMIT: u32 = 40;

/// The core's search results as rows of the shell's lists.
struct CoreSearch {
    artists: Vec<u32>,
    albums: Vec<u32>,
    songs: Vec<u32>,
    /// An artist is the best match overall, so it becomes the top result.
    artist_leads: bool,
}

pub struct SearchView {
    pub sort: SearchSort,
    pub favorites: bool,
    pub scope: SearchScope,
}

impl Default for SearchView {
    fn default() -> Self {
        Self { sort: SearchSort::Relevance, favorites: false, scope: SearchScope::All }
    }
}

impl Shell {
    /// The core's ranked search as rows of this shell's lists. Artists and albums the shell doesn't list are left out.
    /// None without a library.
    fn core_search(&self, query: &str) -> Option<CoreSearch> {
        let core = self.core.as_ref()?;
        let found = core.lib.db.search(query, SEARCH_LIMIT, None).unwrap_or_default();
        let index_of = |ids: Vec<&str>, all: &[(&str, u32)]| -> Vec<u32> {
            let map: std::collections::HashMap<&str, u32> = all.iter().copied().collect();
            ids.into_iter().filter_map(|id| map.get(id).copied()).collect()
        };
        let artist_ids: Vec<(&str, u32)> = self.artists.iter().enumerate().map(|(i, a)| (a.id.as_ref(), i as u32)).collect();
        let album_ids: Vec<(&str, u32)> = self.albums.iter().enumerate().map(|(i, a)| (a.id.as_ref(), i as u32)).collect();
        let song_ids: Vec<String> = found.songs.iter().map(|s| s.id.clone()).collect();
        Some(CoreSearch {
            artists: index_of(found.artists.iter().map(|a| a.id.as_str()).collect(), &artist_ids),
            albums: index_of(found.albums.iter().map(|a| a.id.as_str()).collect(), &album_ids),
            songs: core.lib.db.song_rowids(&song_ids).unwrap_or_default(),
            artist_leads: found.best == Some(navia_core::library::SearchKind::Artist),
        })
    }

    pub fn build_search(&mut self, layout: &Layout) -> Vec<Block> {
        let m = PageMetrics::of(layout);
        let needle = self.search_query.trim().to_lowercase();
        let v = &self.search_view;
        let (scope, fav_only, sort) = (v.scope, v.favorites, v.sort);
        let show = |what: SearchScope| scope == SearchScope::All || scope == what;
        if needle.is_empty() {
            return vec![Shell::plain_block(|this, _, _, _| this.empty_state("magnifying-glass-regular", tr("Search your library"), tr("Try an artist, album or song name."), false).into_any_element())];
        }
        let artists_data = self.artists.clone();
        let albums_data = self.albums.clone();
        let core = self.core_search(&self.search_query);
        let (artists, mut albums, artist_leads): (Vec<u32>, Vec<u32>, bool) = match &core {
            Some(found) => (found.artists.clone(), found.albums.clone(), found.artist_leads),
            None => (
                (0..artists_data.len() as u32).filter(|i| artists_data[*i as usize].name.to_lowercase().contains(&needle)).collect(),
                (0..albums_data.len() as u32).filter(|i| { let a = &albums_data[*i as usize]; a.name.to_lowercase().contains(&needle) || a.artist.to_lowercase().contains(&needle) }).collect(),
                true,
            ),
        };
        let artists: Vec<u32> = artists.into_iter().filter(|i| !fav_only || artists_data[*i as usize].starred).collect();
        albums.retain(|i| !fav_only || albums_data[*i as usize].starred);
        match sort {
            SearchSort::Name => albums.sort_by_cached_key(|i| albums_data[*i as usize].name.to_lowercase()),
            SearchSort::Year => albums.sort_by_key(|i| std::cmp::Reverse(albums_data[*i as usize].year)),
            SearchSort::Relevance => {}
        }
        albums.truncate(20);
        // The top result is an artist only when one is the best match. "weekend" leads with a song called Weekend, not
        // The Weeknd.
        let top = artists.first().copied().filter(|_| artist_leads);
        let top_tracks: Vec<u32> = top.map(|i| self.top_songs(&artists_data[i as usize].id)).unwrap_or_default();
        let top_in_card: std::collections::HashSet<u32> = if show(SearchScope::Artists) { top_tracks.iter().filter(|s| !fav_only || self.is_starred(**s)).take(6).copied().collect() } else { Default::default() };
        // Without a library, title matches come first so a matching artist's songs can't push them out. The core's
        // search already ranks titles over artists.
        let (matches, title_hits): (Vec<u32>, std::collections::HashSet<u32>) = if let Some(found) = core {
            (found.songs.clone(), found.songs.into_iter().collect())
        } else {
            let songs = self.songs.clone();
            let rows: Vec<u32> = (0..songs.len() as u32).filter(|i| { let s = &songs[*i as usize]; s.title.to_lowercase().contains(&needle) || s.artist.to_lowercase().contains(&needle) }).collect();
            let hits = rows.iter().copied().filter(|i| songs[*i as usize].title.to_lowercase().contains(&needle)).collect();
            (rows, hits)
        };
        let matches: Vec<u32> = matches.into_iter().filter(|i| !top_in_card.contains(i)).filter(|i| !fav_only || self.is_starred(*i)).collect();
        let mut title_first: Vec<u32> = matches.iter().copied().filter(|i| title_hits.contains(i)).collect();
        title_first.extend(matches.iter().copied().filter(|i| !title_hits.contains(i)));
        title_first.truncate(12);
        let show_top = top.is_some() && show(SearchScope::Artists);
        let mut sections: Vec<Vec<Block>> = Vec::new();
        if let Some(i) = top.filter(|_| show_top) {
            let artist = artists_data[i as usize].clone();
            let songs: Vec<u32> = top_tracks.iter().filter(|s| !fav_only || self.is_starred(**s)).take(6).copied().collect();
            let list = self.add_list(songs);
            self.page_mini_artist = false;
            sections.push(vec![Shell::plain_block(move |this, layout, _, cx| this.render_top_result(&artist, list, layout, cx))]);
        }
        if show(SearchScope::Songs) && !title_first.is_empty() {
            self.page_mini_artist = false;
            let mut block = Vec::new();
            if show_top {
                block.push(Self::section_heading(tr("Songs").into()));
            }
            block.extend(self.mini_blocks(title_first, layout, false, None));
            sections.push(block);
        }
        if show(SearchScope::Artists) && !artists.is_empty() {
            let cards: Vec<CardItem> = artists.into_iter().map(CardItem::Artist).collect();
            sections.push(self.shelf_block(tr("Artists"), Some(Route::Artists), cards, false, 0));
        }
        let upcoming = if fav_only { Vec::new() } else { self.upcoming_search_cards(&needle) };
        if show(SearchScope::Albums) && (!albums.is_empty() || !upcoming.is_empty()) {
            let mut cards: Vec<CardItem> = albums.into_iter().map(|index| CardItem::Album { index, by_artist: false }).collect();
            cards.extend(upcoming);
            sections.push(self.shelf_block(tr("Albums"), Some(Route::Albums(crate::pages::AlbumScope::All)), cards, false, 2));
        }
        let page = Shell::sections(m.section_gap, sections);
        if page.is_empty() {
            let (scope_word, q) = (scope, self.search_query.clone());
            return vec![Shell::plain_block(move |this, _, _, _| {
                let title = match (scope_word, fav_only) {
                    (SearchScope::Artists, false) => tr("No artists"),
                    (SearchScope::Artists, true) => tr("No favorite artists"),
                    (SearchScope::Albums, false) => tr("No albums"),
                    (SearchScope::Albums, true) => tr("No favorite albums"),
                    (SearchScope::Songs, false) => tr("No songs"),
                    (SearchScope::Songs, true) => tr("No favorite songs"),
                    (SearchScope::All, false) => tr("No results"),
                    (SearchScope::All, true) => tr("No favorite results"),
                };
                this.empty_state("magnifying-glass-regular", title, trf("Nothing matches \u{201c}{q}\u{201d}. Check the spelling, or try an artist or album name.", &[("q", &q)]), false).into_any_element()
            })];
        }
        page
    }

    fn render_top_result(&mut self, artist: &Artist, list: usize, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let m = PageMetrics::of(layout);
        let photo = self.artwork(artist.image.as_ref(), "user-fill", 54.0, ArtShape::Circle, layout);
        let id = artist.id.clone();
        let id2 = artist.id.clone();
        let count = self.page.lists.get(list).map_or(0, |l| l.len());
        let has_play = !self.artist_playable(&artist.id).is_empty();
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .pl(t.s(16.0))
            .pr(t.s(24.0))
            .pb(t.s(12.0))
            .child(
                crate::ui::press_dim(div().id("top-artist"))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(t.s(14.0))
                    .text_size(t.text(24.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .stop(&t).on_click(cx.listener(move |this, _, _, cx| this.navigate(Route::Artist(id.clone()), cx)))
                    .child(photo)
                    .child(div().id("name").truncate().child(artist.name.clone()).full_text_tip(&t, artist.name.clone())),
            )
            .child(
                div().flex().flex_row().items_center().gap(t.s(12.0)).child(
                    glass_pill(&t).child(
                        div()
                            .id("top-play")
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(t.s(9.0))
                            .h(t.s(34.0))
                            .pl(t.s(13.0))
                            .pr(t.s(12.0))
                            .rounded_full()
                            .font_weight(FontWeight::MEDIUM)
                            .opacity(if has_play { 1.0 } else { 0.4 })
                            .when(has_play, |d| {
                                let artist_id = artist.id.clone();
                                d.child(crate::ui::hover_pill(&t)).stop(&t).on_click(cx.listener(move |this, _, _, cx| {
                                    let songs = this.artist_playable(&artist_id);
                                    this.play_songs(songs, 0, false, cx);
                                }))
                            })
                            .child(icon(&t, "play-fill", 18.0, t.t1()))
                            .child(tr("Play")),
                    ),
                )
                .child(
                    glass_pill(&t).child(icon_button(&t, "top-more", tr("More"), "dots-three-bold", false).stop(&t).on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                        let radio = this.similar_songs(&id2);
                        let (go, radio_id, go_info) = (id2.clone(), id2.clone(), id2.clone());
                        let mut rows = vec![MenuRow::item(tr("Go to Artist"), "microphone-stage-regular", move |this, cx| this.navigate(Route::Artist(go.clone()), cx))];
                        rows.push(if radio.is_empty() {
                            MenuRow::disabled(tr("Artist Radio"), "radio-regular", tr("Needs similar-artist data from the server"))
                        } else {
                            MenuRow::item(tr("Artist Radio"), "radio-regular", move |this, cx| this.start_artist_radio(&radio_id, cx))
                        });
                        rows.push(this.share_row(navia_core::shares::ShareKind::Artist, go_info.clone()));
                        rows.push(this.info_row(crate::view_info::InfoTarget::Artist(go_info.clone())));
                        this.open_toolbar_menu(rows, event, cx);
                    }))),
                ),
            );
        let mut card = div().mx(t.s(m.gutter)).pt(t.s(16.0)).pb(t.s(12.0)).rounded(t.s(R_LG)).bg(gpui::rgb(crate::theme::PANEL_2)).line_height(relative(1.3)).child(header);
        let width = f32::from(layout.main_width) / t.token_scale() - m.gutter * 2.0 - 24.0;
        for pos in 0..count {
            card = card.child(div().pl(t.s(12.0)).child(self.render_mini_row_at(list, pos, width, cx)));
        }
        card.into_any_element()
    }
}
