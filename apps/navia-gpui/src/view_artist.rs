//! The Artist page: photo and name, top songs and albums. The biography is in the About sheet (`view_about`).

use crate::ui::FullTextTip;
use crate::i18n::{tr, trf};
use gpui::prelude::*;
use crate::focus::FocusStop;
use gpui::{AnyElement, Context, FontWeight, SharedString, div, px, relative};

use crate::model::{Artist, ReleaseSection};
use crate::pages::{Block, CardItem, PageMetrics, Route, RowVariant};
use crate::song_links::LinkTarget;
use crate::shell::{Layout, Shell};
use crate::theme::size::*;
use crate::ui::icon_button;
use crate::view_album::pill_group;
use crate::view_cards::{ArtShape, song_count, songs_and_length};
use crate::view_menu::MenuRow;

#[derive(Default)]
pub struct ArtistDetail {
    pub query: String,
}

impl Shell {
    pub fn build_artist(&mut self, id: &SharedString, layout: &Layout) -> Vec<Block> {
        let Some(artist) = self.artists.iter().find(|a| &a.id == id).cloned() else { return vec![Self::not_found(tr("This artist isn't here"))] };
        let m = PageMetrics::of(layout);
        let needle = self.artist_detail.query.trim().to_lowercase();
        // None while the server's list is loading, so no albums and no empty state yet.
        let discography = self.artist_discography(id);
        let loading = discography.is_none();
        let discography = discography.unwrap_or_default();
        let albums = self.albums.clone();
        let top = self.top_songs(id);
        let all_songs = self.library.as_ref().map(|l| l.artist_songs(id)).unwrap_or_default();
        let shown_sections: Vec<(ReleaseSection, Vec<u32>)> = discography
            .iter()
            .map(|(section, list)| (*section, list.iter().filter(|i| needle.is_empty() || albums[**i as usize].name.to_lowercase().contains(&needle)).copied().collect::<Vec<u32>>()))
            .filter(|(_, list)| !list.is_empty())
            .collect();
        let shown_songs: Vec<u32> = if needle.is_empty() {
            top.iter().take(6).copied().collect()
        } else {
            all_songs
                .iter()
                .filter(|i| self.song_peek(**i).is_some_and(|s| format!("{} {}", s.title, s.album).to_lowercase().contains(&needle)))
                .take(12)
                .copied()
                .collect()
        };
        let playable: Vec<u32> = if !top.is_empty() {
            top.clone()
        } else if !all_songs.is_empty() {
            all_songs
        } else {
            Vec::new()
        };
        let list = self.add_list(playable);
        let songs_list = self.add_list(shown_songs.clone());
        // The newest of the artist's own releases. Each section is sorted newest first.
        let latest = if needle.is_empty() {
            discography.iter().filter(|(s, _)| *s != ReleaseSection::AppearsOn).filter_map(|(_, list)| list.first().copied()).max_by_key(|i| (albums[*i as usize].year, std::cmp::Reverse(*i)))
        } else {
            None
        };
        // Unknown counts as available. Asking the server is slow, so it waits for a click.
        let has_radio = self.library.as_ref().and_then(|l| l.similar_known(id)).is_none_or(|s| !s.is_empty());
        let artist_for_head = artist.clone();
        let head = Shell::plain_block(move |this, layout, _, cx| this.render_artist_head(&artist_for_head, list, has_radio, layout, cx));
        let mut sections: Vec<Vec<Block>> = Vec::new();
        if !shown_songs.is_empty() || latest.is_some() {
            let (title, link) = if needle.is_empty() { (tr("Top Songs"), Some(Route::ArtistTop(id.clone()))) } else { (tr("Songs"), None) };
            sections.push(vec![Shell::plain_block(move |this, layout, _, cx| this.render_artist_columns(songs_list, title, link.clone(), latest, layout, cx))]);
        }
        if needle.is_empty() {
            let upcoming = self.upcoming_artist_cards(id);
            sections.push(self.shelf_block(tr("Upcoming"), None, upcoming, false, 1));
        }
        // One row per release type, in page order, each with "See all".
        for (section, list) in &shown_sections {
            let cards: Vec<CardItem> = list.iter().map(|index| CardItem::Album { index: *index, by_artist: false }).collect();
            let link = needle.is_empty().then(|| Route::Albums(crate::pages::AlbumScope::Artist(id.clone(), *section)));
            sections.push(self.shelf_block_sorted(section_title(*section), link, Some(crate::view_albums::AlbumSort::Year), cards, false, 2));
        }
        // Similar Artists last, once the server and Deezer (if enabled) have answered. Hidden when empty.
        if needle.is_empty() {
            let similar: Vec<CardItem> = self.library.as_ref().map(|l| l.similar_artists(id)).unwrap_or_default().into_iter().map(CardItem::Artist).collect();
            sections.push(self.shelf_block(tr("Similar Artists"), None, similar, false, 0));
        }
        let any_albums = !discography.is_empty();
        let empty_search = !needle.is_empty() && shown_songs.is_empty() && shown_sections.is_empty() && !loading;
        let empty_artist = needle.is_empty() && top.is_empty() && !any_albums && !loading;
        let mut page = vec![head, Block::Spacer(m.section_gap)];
        page.extend(Shell::sections(m.section_gap, sections));
        if empty_search || empty_artist {
            let name = artist.name.clone();
            page.push(Shell::plain_block(move |this, _, _, _| {
                if empty_search {
                    this.empty_state("magnifying-glass-regular", tr("No matches"), trf("Nothing by {name} matches your search.", &[("name", &name)]), true).into_any_element()
                } else {
                    this.empty_state("microphone-stage-regular", tr("Nothing here yet"), tr("The server has no songs or albums for this artist."), true).into_any_element()
                }
            }));
        }
        page
    }

    /// The header: the uncropped photo, name, actions and artist search.
    fn render_artist_head(&mut self, artist: &Artist, list: usize, has_radio: bool, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let m = PageMetrics::of(layout);
        let starred = self.artist_starred(&artist.id);
        let id = artist.id.clone();
        let songs = self.page.lists.get(list).cloned().unwrap_or_default();
        let has_songs = !songs.is_empty();
        let radio_id = artist.id.clone();
        let info_id = artist.id.clone();
        let mut actions = vec![
            icon_button(&t, "artist-star", tr("Favorite"), if starred { "heart-fill" } else { "heart-regular" }, starred)
                .stop(&t).on_click(cx.listener(move |this, _, _, cx| {
                    if !starred {
                        crate::ui::pop("artist-star");
                    }
                    this.toggle_artist_star(&id, cx)
                }))
                .into_any_element(),
        ];
        if self.artist_about(&artist.id).is_some() {
            let about_id = artist.id.clone();
            actions.push(icon_button(&t, "artist-about", trf("About {name}", &[("name", &artist.name)]), "info-regular", false).stop(&t).on_click(cx.listener(move |this, _, _, cx| this.open_artist_about(about_id.clone(), cx))).into_any_element());
        }
        actions.push(
            icon_button(&t, "artist-more", tr("More"), "dots-three-bold", false)
                .stop(&t).on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                    let radio_id = radio_id.clone();
                    let mut rows = Vec::new();
                    if has_radio {
                        rows.push(MenuRow::item(tr("Artist Radio"), "radio-regular", move |this, cx| this.start_artist_radio(&radio_id, cx)));
                    }
                    rows.push(this.share_row(navia_core::shares::ShareKind::Artist, info_id.clone()));
                    rows.push(this.info_row(crate::view_info::InfoTarget::Artist(info_id.clone())));
                    this.open_toolbar_menu(rows, event, cx);
                }))
                .into_any_element(),
        );
        let main_tokens = f32::from(layout.main_width) / t.token_scale();
        let art = self.artist_art(artist.image.as_ref(), m.hero_art, layout, cx);
        let bar = div()
            .flex()
            .flex_row()
            .items_center()
            .when(main_tokens <= 900.0, |d| d.flex_wrap())
            .gap(t.s(10.0))
            .child(self.render_play_actions(list, has_songs, cx))
            .child(pill_group(&t, actions))
            .when(main_tokens > 900.0, |d| d.child(div().flex_1()))
            .child(self.search_pill(tr("Search Artist"), self.artist_detail.query.clone(), cx));
        let name = div().text_size(t.text(m.title)).text_color(t.t_em()).font_weight(FontWeight::BOLD).line_height(relative(1.1)).line_clamp(2).text_ellipsis().min_w(px(0.0)).id("artist-name").child(artist.name.clone()).full_text_tip(&t, artist.name.clone());
        let info = div().flex().flex_col().flex_1().min_w(px(0.0)).gap(t.s(ARTIST_HEAD_TITLE_GAP)).child(name).child(bar);
        let stack = main_tokens <= 620.0;
        let mut head = div().flex().gap(t.s(ARTIST_HEAD_GAP)).px(t.s(m.gutter)).w(layout.main_width);
        head = if stack { head.flex_col().items_start() } else { head.flex_row().items_end() };
        head.child(art).child(info).into_any_element()
    }

    /// The artist's photo, uncropped and `size` tall, or the square placeholder while it loads.
    fn artist_art(&mut self, image: Option<&SharedString>, size: f32, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let h = t.s(size);
        let device_h = (f32::from(h) * layout.scale).round() as u32;
        let device_w = (device_h as f32 * ARTIST_ART_MAX_RATIO).round() as u32;
        let baked = image.filter(|i| !i.is_empty()).cloned().and_then(|i| self.bake_for(crate::art::BakeKind::ArtistArt, &i, device_w, device_h, cx));
        let art = match baked {
            Some(img) => {
                let shape = img.size(0);
                let w = h * (shape.width.0 as f32 / shape.height.0.max(1) as f32);
                crate::ui::image(img).w(w).h(h).rounded(t.s(R_LG)).into_any_element()
            }
            None => self.artwork_el(None, "user-fill", size, ArtShape::Rounded(R_LG)),
        };
        crate::ui::hero_art_frame(&t, R_LG, art).into_any_element()
    }

    fn render_artist_columns(&mut self, list: usize, title: &'static str, link: Option<Route>, latest: Option<u32>, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let m = PageMetrics::of(layout);
        let main = f32::from(layout.main_width) / t.token_scale();
        let count = self.page.lists.get(list).map_or(0, |l| l.len());
        let two_columns = main > 900.0 && count > 0 && latest.is_some();
        let songs_width = if two_columns { (main - ARTIST_COLUMNS_GAP - m.gutter * 2.0) / 2.0 } else { main - m.gutter * 2.0 };
        let heading = self.section_title(title.into(), link, cx);
        let mut songs = div().flex().flex_col().min_w(px(0.0)).child(div().mb(t.s(12.0)).child(heading));
        for pos in 0..count {
            songs = songs.child(self.render_mini_row_at(list, pos, songs_width, cx));
        }
        let latest_view = latest.and_then(|ix| self.albums.get(ix as usize).cloned()).map(|album| {
            let art = self.artwork(Some(&album.id), "music-notes-fill", 200.0, ArtShape::Rounded(R_MD), layout);
            let route = Route::Album(album.id.clone());
            let title = self.section_title(tr("Latest Release").into(), None, cx);
            div()
                .flex()
                .flex_col()
                .min_w(px(0.0))
                .child(title)
                .child(
                    crate::ui::press_dim(div().id("latest-release"))
                        .flex()
                        .flex_row()
                        .gap(t.s(25.0))
                        .mt(t.s(14.0))
                        .stop(&t).on_click(cx.listener(move |this, _, _, cx| this.navigate(route.clone(), cx)))
                        .child(art)
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .min_w(px(0.0))
                                .pt(t.s(2.0))
                                .line_height(relative(1.35))
                                .when(album.year > 0, |d| d.child(div().mb(t.s(8.0)).font_weight(FontWeight::SEMIBOLD).text_color(t.t2()).child(album.year.to_string())))
                                .child(div().font_weight(FontWeight::MEDIUM).line_clamp(2).child(album.name.clone()))
                                .child(div().text_color(t.t2()).child(song_count(album.song_count))),
                        ),
                )
        });
        let mut row = div().flex().px(t.s(m.gutter)).w(layout.main_width).gap(t.s(ARTIST_COLUMNS_GAP));
        row = if two_columns { row.flex_row() } else { row.flex_col().gap(t.s(ARTIST_COLUMNS_STACK_GAP)) };
        row = row.child(div().flex_1().min_w(px(0.0)).when(count > 0, |d| d.child(songs)));
        if let Some(view) = latest_view {
            row = row.child(div().flex_1().min_w(px(0.0)).child(view));
        }
        row.into_any_element()
    }
}

/// The heading of a discography section.
pub fn section_title(section: ReleaseSection) -> &'static str {
    match section {
        ReleaseSection::Albums => tr("Albums"),
        ReleaseSection::SinglesAndEps => tr("Singles & EPs"),
        ReleaseSection::Compilations => tr("Compilations"),
        ReleaseSection::AppearsOn => tr("Appears On"),
    }
}

impl Shell {
    /// the server's top songs in its own order, so the table doesn't sort.
    pub fn build_artist_top(&mut self, id: &SharedString, _layout: &Layout) -> Vec<Block> {
        let Some(artist) = self.artists.iter().find(|a| &a.id == id).cloned() else { return vec![Self::not_found(tr("This artist isn't here"))] };
        let top = self.top_songs(id);
        self.core_preload_rows(&top);
        let count = top.len();
        let list = self.add_list(top);
        let head = Shell::plain_block(move |this, layout, _, cx| this.render_artist_top_head(&artist, list, layout, cx));
        let actions = Shell::plain_block(move |this, layout, _, cx| {
            let t = this.theme;
            let m = PageMetrics::of(layout);
            div().flex().flex_row().items_center().px(t.s(m.gutter)).w(layout.main_width).child(this.render_play_actions(list, count > 0, cx)).into_any_element()
        });
        let mut blocks = vec![head, Block::Spacer(28.0), actions];
        if count > 0 {
            blocks.push(Block::Spacer(32.0));
            blocks.push(Shell::plain_block(|this, layout, _, cx| this.render_table_header(layout, None, None, cx)));
            blocks.extend((0..count).map(|pos| Block::Table { list, pos, variant: RowVariant::Full }));
        }
        blocks
    }

    fn render_artist_top_head(&mut self, artist: &Artist, list: usize, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let m = PageMetrics::of(layout);
        let songs = self.page.lists.get(list).cloned().unwrap_or_default();
        let seconds: Option<u32> = songs.iter().map(|s| self.song_peek(*s).map(|song| song.duration_s)).sum();
        let summary = match seconds {
            Some(seconds) => songs_and_length(songs.len() as u32, seconds),
            None => song_count(songs.len() as u32).to_string(),
        };
        let name = self.link(("artist-top-name", 0), artist.name.clone(), Some(LinkTarget::Artist(artist.id.clone())), t.t1().into(), cx);
        let meta = div().mt(t.s(14.0)).flex().flex_row().items_center().min_w(px(0.0)).child(name).child(format!(" • {summary}")).into_any_element();
        let art = self.artwork(artist.image.as_ref(), "user-fill", m.hero_art, ArtShape::Rounded(R_MD), layout);
        self.collection_head(art, tr("Top Songs").into(), meta, layout)
    }
}
