//! Album, artist, playlist and tile cards, the shelves that hold them, and grid rows.

use crate::ui::FullTextTip;
use crate::i18n::{tr, trf, trn};
use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::*;
use crate::ui::LinkCursor;
use crate::focus::FocusStop;
use gpui::{AnyElement, BoxShadow, Context, FontWeight, RenderImage, ScrollHandle, SharedString, div, point, px, relative};

use crate::color::Srgb;
use crate::pages::{Block, CardItem, PageMetrics, Route, Shelf};
use crate::shell::{Layout, Shell};
use crate::theme::motion;
use crate::theme::size::*;
use crate::ui::{glass_edge, icon};

/// A square artwork, or a placeholder when there's none.
#[derive(Clone, Copy, PartialEq)]
pub enum ArtShape {
    Rounded(f32),
    Circle,
}

impl Shell {
    pub fn artwork(&mut self, id: Option<&SharedString>, kind: &'static str, size: f32, shape: ArtShape, layout: &Layout) -> AnyElement {
        let t = self.theme;
        let device = (f32::from(t.s(size)) * layout.scale).round() as u32;
        let image = id.filter(|i| !i.is_empty()).and_then(|i| self.thumb_faded(i, device));
        self.artwork_el(image, kind, size, shape)
    }

    /// large artwork (a page's header): the still cover, replaced by its frames when it moves.
    pub fn hero_artwork(&mut self, id: &SharedString, kind: &'static str, size: f32, radius: f32, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let device = (f32::from(t.s(size)) * layout.scale).round() as u32;
        match self.moving_cover(id, device, "hero", Some(t.s(radius)), cx) {
            Some(view) => div().size(t.s(size)).flex_none().child(view).into_any_element(),
            None => self.artwork(Some(id), kind, size, ArtShape::Rounded(radius), layout),
        }
    }

    pub fn artwork_el(&self, image: Option<(Arc<RenderImage>, f32)>, kind: &'static str, size: f32, shape: ArtShape) -> AnyElement {
        let t = self.theme;
        let dim = t.s(size);
        let round = |d: gpui::Div| match shape {
            ArtShape::Circle => d.rounded_full(),
            ArtShape::Rounded(r) => d.rounded(t.s(r)),
        };
        let placeholder = || round(div().size(dim).flex_none().bg(gpui::rgb(crate::theme::PANEL_2)).flex().items_center().justify_center()).child(icon(&t, kind, size * ART_GLYPH, t.t3()));
        match image {
            Some((image, fade)) => {
                let el = crate::ui::image(image).size(dim);
                let el = match shape {
                    ArtShape::Circle => el.rounded_full(),
                    ArtShape::Rounded(r) => el.rounded(t.s(r)),
                };
                crate::ui::fade_in(el, placeholder(), fade)
            }
            None => placeholder().into_any_element(),
        }
    }

    pub fn card_text_lines(subtitle: bool, sub_lines: u32) -> f32 {
        let title = CARD_TEXT_GAP + TEXT_MD * 1.3;
        if subtitle { title + TEXT_MD * 1.3 * sub_lines as f32 } else { title }
    }

    /// A card's column: its art, then whatever the caller adds. `under` is drawn beneath the whole card, for a drop
    /// highlight.
    fn card_shell(&self, id: (&'static str, usize), width: f32, group: SharedString, under: Option<AnyElement>, children: impl IntoElement) -> gpui::Stateful<gpui::Div> {
        let t = self.theme;
        crate::ui::press_dim(div().id(id))
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .w(t.s(width))
            .line_height(relative(1.3))
            .children(under)
            .child(children)
            .child(crate::ui::card_track(&group))
    }

    /// the artwork of a card; it stays still on hover.
    fn lift(&self, _group: SharedString, art: AnyElement) -> AnyElement {
        art
    }

    pub fn render_card(&mut self, item: &CardItem, width: f32, ix: usize, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let group = SharedString::from(format!("card-{ix}"));
        match item {
            CardItem::Album { index, by_artist } => {
                let Some(album) = self.albums.get(*index as usize).cloned() else { return div().into_any_element() };
                let art = self.artwork(Some(&album.id), "music-notes-fill", width, ArtShape::Rounded(R_MD), layout);
                let today = navia_core::library::taste::local_day(navia_core::library::taste::now_unix());
                let art = if album.is_new_release(today) { labelled_art(&t, art, tr("New||release")) } else { art };
                let art = self.lift(group.clone(), art);
                // Artist names are links. A click anywhere else opens the album.
                let download = self.download_mark(navia_core::library::collections::CollectionKind::Album, &album.id).map(|m| div().flex_none().mr(t.s(DOWNLOAD_MARK_GAP)).child(m));
                let sub: AnyElement = if *by_artist {
                    // Two albums with the same name in one discography are told apart by edition.
                    let edition = album.edition.clone().filter(|_| album.edition_on_card).map(|e| e.to_string());
                    let text = div().min_w(px(0.0)).line_clamp(2).text_ellipsis().child([(album.year > 0).then(|| album.year.to_string()), edition, Some(song_count(album.song_count).to_string())].into_iter().flatten().collect::<Vec<_>>().join(" • "));
                    div().flex().flex_row().items_center().text_color(t.t2()).children(download).child(text).into_any_element()
                } else {
                    let names = self.artist_name_links(("card-artist", ix), &album.artist, t.t2().into(), cx);
                    let edition = album.edition.clone().filter(|_| album.edition_on_card);
                    div().flex().flex_row().items_center().min_w(px(0.0)).overflow_hidden().whitespace_nowrap().text_color(t.t2()).children(download).child(names).when(album.year > 0, |d| d.child(div().flex_none().child(format!("\u{00a0}• {}", album.year)))).when_some(edition, |d, e| d.child(div().flex_none().child(format!("\u{00a0}• {e}")))).into_any_element()
                };
                let route = Route::Album(album.id.clone());
                self.card_shell(("card", ix), width, group, None, art)
                    .child(crate::ui::titled(&t, div().id("name").truncate().font_weight(FontWeight::MEDIUM).child(album.name.clone()).full_text_tip(&t, album.name.clone()), album.explicit, TEXT_MD).mt(t.s(CARD_TEXT_GAP)))
                    .child(sub)
                    .stop(&t).on_click(cx.listener(move |this, _, _, cx| this.navigate(route.clone(), cx)))
                    .into_any_element()
            }
            CardItem::Artist(index) => {
                let Some(artist) = self.artists.get(*index as usize).cloned() else { return div().into_any_element() };
                let art = self.artwork(artist.image.as_ref(), "user-fill", width, ArtShape::Circle, layout);
                let art = self.lift(group.clone(), art);
                let route = Route::Artist(artist.id.clone());
                self.card_shell(("card", ix), width, group, None, art)
                    .items_center()
                    .child(div().id("name").mt(t.s(CARD_TEXT_GAP)).w_full().text_center().truncate().font_weight(FontWeight::MEDIUM).child(artist.name.clone()).full_text_tip(&t, artist.name.clone()))
                    .stop(&t).on_click(cx.listener(move |this, _, _, cx| this.navigate(route.clone(), cx)))
                    .into_any_element()
            }
            CardItem::Playlist(index) => {
                let Some(p) = self.playlists_full.get(*index as usize).cloned() else { return div().into_any_element() };
                let cover_id = self.playlist_art_key(&p.id);
                let art = self.artwork(Some(&cover_id), "playlist-regular", width, ArtShape::Rounded(R_MD), layout);
                let art = self.lift(group.clone(), art);
                let route = Route::Playlist(p.id.clone());
                let menu_id = p.id.clone();
                // Songs carried onto your own regular playlist's card are added to it.
                let drop = self.drop_mark(format!("card:{}", p.id), self.playlist_drop(&p.id, &p.name), R_MD, DROP_PAD_CARD);
                self.card_shell(("card", ix), width, group, drop, art)
                    .child(div().id("name").mt(t.s(CARD_TEXT_GAP)).truncate().font_weight(FontWeight::MEDIUM).child(p.name.clone()).full_text_tip(&t, p.name.clone()))
                    .child(div().flex().flex_row().items_center().min_w(px(0.0)).text_color(t.t2()).children(self.download_mark(navia_core::library::collections::CollectionKind::Playlist, &p.id).map(|m| div().flex_none().mr(t.s(DOWNLOAD_MARK_GAP)).child(m))).child(div().min_w(px(0.0)).truncate().child(playlist_subtitle(&p))))
                    .stop(&t).on_click(cx.listener(move |this, _, _, cx| this.navigate(route.clone(), cx)))
                    .on_mouse_down(gpui::MouseButton::Right, cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| this.open_playlist_menu(menu_id.clone(), event.position, cx)))
                    .into_any_element()
            }
            CardItem::Essentials(index) | CardItem::SongRadio(index) => {
                let essentials = matches!(item, CardItem::Essentials(_));
                let Some(artist) = self.artists.get(*index as usize).cloned() else { return div().into_any_element() };
                let band = if essentials { tr("Essentials") } else { tr("Song Radio") };
                let tint = self.artist_tint(&artist.id);
                let artist_id = artist.id.clone();
                let art = self.render_band_tile(&artist.name, artist.image.as_ref(), band, tint, width, layout);
                let art = self.lift(group.clone(), art);
                let (title, sub) = if essentials {
                    let genre = self.top_genre(&artist.id);
                    (SharedString::from(trf("{name} Essentials", &[("name", &artist.name)])), if genre.is_empty() { SharedString::from(tr("Music")) } else { genre })
                } else {
                    (SharedString::from(trf("{name} Radio", &[("name", &artist.name)])), SharedString::from(tr("Similar artists")))
                };
                self.card_shell(("card", ix), width, group, None, art)
                    .child(div().id("name").mt(t.s(CARD_TEXT_GAP)).truncate().font_weight(FontWeight::MEDIUM).child(title.clone()).full_text_tip(&t, title))
                    .child(div().id("sub").text_color(t.t2()).truncate().child(sub.clone()).full_text_tip(&t, sub))
                    .stop(&t).on_click(cx.listener(move |this, _, _, cx| {
                        if essentials {
                            this.play_artist_top(&artist_id, cx);
                        } else {
                            this.start_artist_radio(&artist_id, cx);
                        }
                    }))
                    .into_any_element()
            }
            CardItem::GenreFan(list, index) => self.render_genre_tile(*list, *index, width, group, ix, layout, cx),
            CardItem::Mix(index) => self.render_mix_card(*index, width, group, ix, layout, cx),
            CardItem::ArtistStation { seed, neighbours } => self.render_artist_station_card(*seed, neighbours, width, group, ix, layout, cx),
            CardItem::Upcoming { album, tag } => {
                self.page_countdown = true;
                let left = crate::view_upcoming::TimeLeft::of(album);
                let label = if *tag { left.tag() } else { left.short() };
                let art = self.artwork(Some(&album.cover), "music-notes-fill", width, ArtShape::Rounded(R_MD), layout);
                let art = self.lift(group.clone(), labelled_art(&t, art, label));
                let route = Route::Upcoming(album.id);
                let sub = crate::view_upcoming::release_line(album, &left, true);
                self.card_shell(("card", ix), width, group, None, art)
                    .child(crate::ui::titled(&t, div().id("name").truncate().font_weight(FontWeight::MEDIUM).child(album.title.clone()).full_text_tip(&t, album.title.clone()), album.explicit, TEXT_MD).mt(t.s(CARD_TEXT_GAP)))
                    .child(div().id("sub").text_color(t.t2()).truncate().child(sub))
                    .stop(&t).on_click(cx.listener(move |this, _, _, cx| this.navigate(route.clone(), cx)))
                    .into_any_element()
            }
        }
    }

    /// An Artist Radio tile: the seed artist's round photo in front of up to two neighbors, the name, and a "With A, B,
    /// C and more" line. Clicking starts the station.
    #[allow(clippy::too_many_arguments)]
    fn render_artist_station_card(&mut self, seed: u32, neighbours: &Rc<Vec<u32>>, width: f32, group: SharedString, ix: usize, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let Some(artist) = self.artists.get(seed as usize).cloned() else { return div().into_any_element() };
        let named: Vec<crate::model::Artist> = neighbours.iter().filter_map(|i| self.artists.get(*i as usize).cloned()).collect();
        let tint = match &artist.image {
            Some(image) => match self.playlist_cover_tint(&crate::art::genre_tint_key(std::slice::from_ref(image))) {
                crate::shell_data::CoverTint::Ready(tint) => tint,
                crate::shell_data::CoverTint::Loading => None,
            },
            None => None,
        };
        let (hover, ticker) = crate::ui::card_hover(&group, motion::MED);
        let inset = t.s(width * STATION_INSET) * hover;
        let radius = t.s(R_MD) + (t.s(width * STATION_INSET) - t.s(R_MD)) * hover;
        let framed = |d: gpui::Stateful<gpui::Div>| d.absolute().top(inset).bottom(inset).left(inset).right(inset).rounded(radius);
        let device = (f32::from(t.s(width)) * layout.scale).round() as u32;
        let image = artist.image.as_ref().filter(|i| !i.is_empty()).and_then(|i| self.thumb_faded(i, device));
        let placeholder = div().id("station-blank").size_full().rounded(radius).bg(gpui::rgb(crate::theme::PANEL_2)).flex().items_center().justify_center().child(icon(&t, "user-fill", width * ART_GLYPH, t.t3()));
        let mut photo = framed(div().id("station-photo")).child(placeholder);
        if let Some((image, fade)) = image {
            photo = photo.child(crate::ui::image(image).id("station-image").absolute().inset_0().size_full().rounded(radius).opacity(fade));
        }
        // `--station-shade-ink`: the tile's own top colour, mixed toward black, fading out up the tile.
        let ink = tint.map_or(Srgb::hex(crate::theme::PANEL_2), |c| crate::color::tile_fill(c).0).mix(Srgb::hex(0x000000), STATION_SHADE);
        let shade = framed(div().id("station-shade")).bg(gpui::linear_gradient(0.0, gpui::linear_color_stop(ink.hsla(1.0), 0.0), gpui::linear_color_stop(ink.hsla(0.0), STATION_SHADE_END)));
        let mark_shade = framed(div().id("station-mark-shade")).bg(gpui::linear_gradient(180.0, gpui::linear_color_stop(t.scrim(), 0.0), gpui::linear_color_stop(gpui::Rgba { a: 0.0, ..t.scrim() }, STATION_MARK_SHADE_END)));
        let face = width * STATION_FACE;
        let grown = face * STATION_FACE_HOVER;
        // a filled circle one ring wider than the face: a zero-blur shadow drew far thicker than its spread.
        let ring = t.s(width * STATION_RING);
        // each face grows from its bottom-left corner; a later one also shifts right by its place times the spread.
        let mut faces = div().absolute().left(t.s(width * STATION_PAD)).bottom(t.s(width * STATION_FACES_BOTTOM)).h(t.s(face));
        for (k, n) in named.iter().take(3).enumerate() {
            let rest = k as f32 * (face + width * STATION_FACE_GAP);
            let spread = rest + k as f32 * width * STATION_FACE_SPREAD;
            let device = (f32::from(t.s(grown)) * layout.scale).round() as u32;
            let image = n.image.as_ref().filter(|i| !i.is_empty()).and_then(|i| self.thumb_faded(i, device));
            let size = t.s(face + (grown - face) * hover);
            let mut el = div()
                .relative()
                .size(size)
                .rounded_full()
                .bg(gpui::rgb(crate::theme::PANEL_2))
                .flex()
                .items_center()
                .justify_center()
                .child(icon(&t, "user-fill", face * ART_GLYPH, t.t3()));
            if let Some((image, fade)) = image {
                el = el.child(crate::ui::image(image).absolute().inset_0().size_full().rounded_full().opacity(fade));
            }
            faces = faces.child(
                div()
                    .id(("station-face", k))
                    .absolute()
                    .bottom(-ring)
                    .left(t.s(rest + (spread - rest) * hover) - ring)
                    .p(ring)
                    .rounded_full()
                    .bg(t.station_ring_ink())
                    .child(el),
            );
        }
        let pad = t.s(width * STATION_PAD);
        let name = div()
            .absolute()
            .left(pad)
            .right(pad)
            .bottom(t.s(width * STATION_NAME_BOTTOM))
            .text_size(t.s(width * STATION_NAME))
            .font_weight(FontWeight::BOLD)
            .line_height(relative(1.05))
            .text_color(t.t_em())
            .truncate()
            .id("station-name")
            .child(artist.name.clone())
            .full_text_tip(&t, artist.name.clone());
        let tile = div()
            .relative()
            .size(t.s(width))
            .rounded(t.s(R_MD))
            .overflow_hidden()
            .bg(crate::ui::tile_fill_bg(&t, tint))
            .child(ticker)
            .child(photo)
            .child(shade)
            .child(mark_shade)
            .child(crate::ui::cover_label(&t, tr("Radio"), crate::ui::LabelAt::Top, crate::ui::LabelStyle::Mark { glyph: "broadcast-regular", width }))
            .when(!named.is_empty(), |d| d.child(faces))
            .child(name);
        let tile = self.lift(group.clone(), tile.into_any_element());
        let line = station_line(&named);
        let artist_id = artist.id.clone();
        self.card_shell(("card", ix), width, group, None, tile)
            .when(!line.is_empty(), |d| d.child(div().id("sub").mt(t.s(CARD_TEXT_GAP)).text_color(t.t2()).line_clamp(STATION_LINES as usize).text_ellipsis().child(line.clone()).full_text_tip(&t, line)))
            .stop(&t)
            .on_click(cx.listener(move |this, _, _, cx| this.start_artist_radio(&artist_id, cx)))
            .into_any_element()
    }

    /// The Essentials and Song Radio tile: a tinted band with the name over the artist's photo.
    fn render_band_tile(&mut self, name: &SharedString, photo: Option<&SharedString>, band: &'static str, tint: Srgb, width: f32, layout: &Layout) -> AnyElement {
        let t = self.theme;
        let device = (f32::from(t.s(width)) * layout.scale).round() as u32;
        let image = photo.filter(|p| !p.is_empty()).and_then(|p| self.thumb_faded(p, device));
        let band_color = tint.mix(t.essentials_paper(), 0.32);
        let _ = name;
        div()
            .relative()
            .size(t.s(width))
            .rounded(t.s(R_MD))
            .overflow_hidden()
            .bg(tint.hsla(1.0))
            .when_some(image, |d, (image, fade)| {
                d.child(crate::ui::image(image).absolute().left(px(0.0)).top(relative(0.31)).w_full().h(relative(0.69)).opacity(fade))
            })
            .child(
                div()
                    .absolute()
                    .left(px(0.0))
                    .right(px(0.0))
                    .top(px(0.0))
                    .h(relative(0.31))
                    .flex()
                    .items_end()
                    .pl(relative(0.036))
                    .pb(relative(0.03))
                    .bg(band_color.hsla(1.0))
                    .text_size(t.s(width * 0.136))
                    .font_weight(FontWeight::SEMIBOLD)
                    .line_height(relative(1.0))
                    .text_color(t.essentials_ink())
                    .child(band),
            )
            .into_any_element()
    }

    pub fn shelf_block(&mut self, title: impl Into<SharedString>, link: Option<Route>, items: Vec<CardItem>, tiles: bool, sub_lines: u32) -> Vec<Block> {
        self.shelf_block_sorted(title, link, None, items, tiles, sub_lines)
    }

    /// A shelf whose title link opens Albums on a given sort.
    pub fn shelf_block_sorted(&mut self, title: impl Into<SharedString>, link: Option<Route>, link_sort: Option<crate::view_albums::AlbumSort>, items: Vec<CardItem>, tiles: bool, sub_lines: u32) -> Vec<Block> {
        if items.is_empty() {
            return Vec::new();
        }
        let id = self.next_shelf_id();
        vec![Block::Shelf(Rc::new(Shelf { id, title: title.into(), link, link_sort, items: Rc::new(items), tiles, scroll: ScrollHandle::new(), sub_lines, note: None, more: None }))]
    }

    /// A shelf with a plain title and a `more` button that opens `link`.
    pub fn shelf_block_more(&mut self, title: impl Into<SharedString>, more: &'static str, link: Route, link_sort: Option<crate::view_albums::AlbumSort>, items: Vec<CardItem>, sub_lines: u32) -> Vec<Block> {
        let mut blocks = self.shelf_block_sorted(title, Some(link), link_sort, items, false, sub_lines);
        if let Some(Block::Shelf(shelf)) = blocks.first_mut()
            && let Some(shelf) = Rc::get_mut(shelf)
        {
            shelf.more = Some(more);
        }
        blocks
    }

    pub fn section_title(&mut self, title: SharedString, link: Option<Route>, cx: &mut Context<Self>) -> AnyElement {
        self.section_title_sorted(title, link, None, cx)
    }

    /// A section title whose link opens Albums on a given sort.
    pub fn section_title_sorted(&mut self, title: SharedString, link: Option<Route>, sort: Option<crate::view_albums::AlbumSort>, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let row = div().flex().flex_row().items_center().gap(t.s(6.0)).text_size(t.text(TEXT_SECTION)).font_weight(FontWeight::SEMIBOLD).line_height(relative(1.2)).child(title.clone());
        match link {
            Some(route) => row
                .id(SharedString::from(format!("section-{title}")))
                .link_cursor()
                .child(icon(&t, "caret-right-bold", 19.0, t.t2()))
                .stop(&t).on_click(cx.listener(move |this, _, _, cx| {
                    this.albums_sort_on_open = sort;
                    this.navigate(route.clone(), cx);
                    this.albums_sort_on_open = None;
                }))
                .into_any_element(),
            None => row.into_any_element(),
        }
    }

    pub fn render_shelf(&mut self, shelf: &Rc<Shelf>, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let m = PageMetrics::of(layout);
        let (card, gap, height) = Self::shelf_card(shelf, &m);
        let main_tokens = f32::from(layout.main_width) / t.token_scale();
        // the row stops just before the dock and fades out.
        let (fade, fade_gap) = (SHELF_FADE * layout.dock_shown, SHELF_GAP * layout.dock_shown);
        // the row reaches under the sidebar so a card scrolled off the left is really there for the sidebar to frost.
        let under_px = self.sidebar_under(layout);
        let under = f32::from(under_px) / t.token_scale();
        let left_pad = under + m.gutter;
        let right_pad = m.gutter + fade;
        let count = shelf.items.len();
        let content = left_pad + right_pad + count as f32 * card + count.saturating_sub(1) as f32 * gap;
        let offset = (-f32::from(shelf.scroll.offset().x)) / t.token_scale();
        let view = main_tokens + under;
        // Only cards near the viewport are built, so long shelves cost the same.
        let step = card + gap;
        let first = (((offset - left_pad) / step).floor() as isize - 1).max(0) as usize;
        let last = (((offset + view - left_pad) / step).ceil() as usize + 1).min(count);
        let mut track = div().relative().h(t.s(height)).w(t.s(content)).flex_none();
        for i in first..last {
            let x = left_pad + i as f32 * step;
            let el = self.render_card(&shelf.items[i], card, shelf.id * 100_000 + i, layout, cx);
            track = track.child(div().absolute().top(px(0.0)).left(t.s(x)).child(el));
        }
        let scroll_h = t.s(height);
        let handle = shelf.scroll.clone();
        let more_left = offset > 2.0;
        let more_right = offset + view < content - 2.0;
        let scroller = div()
            .id(("shelf", shelf.id))
            .w_full()
            .h(scroll_h)
            .overflow_x_scroll()
            .restrict_scroll_to_axis()
            .track_scroll(&shelf.scroll)
            .child(track);
        let group = format!("shelf-{}", shelf.id);
        let bar_top = t.s(height * 0.42 - CONTROL_H / 2.0);
        let arrow = |this: &mut Self, name: &'static str, dir: f32, cx: &mut Context<Self>| {
            let h = handle.clone();
            let page = t.s(main_tokens * 0.8);
            let arrow_id = SharedString::from(format!("{group}-{name}"));
            div()
                .id(arrow_id)
                .absolute()
                .top(bar_top)
                .size(t.s(CONTROL_H))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .map(|d| crate::ui::glass_layers(d, &t, t.s(CONTROL_H / 2.0), GLASS_BLUR, GLASS_SATURATION, t.glass(), t.glass_fill(), glass_edge()))
                .opacity(0.0)
                .group_hover(SharedString::from(group.clone()), |s| s.opacity(1.0))
                // Hovering the arrow blocks the row's group hover, so the arrow stays visible.
                .hover(|s| s.bg(t.hover()).opacity(1.0))
                // Otherwise the card under the arrow gets the hover and the click. Scroll still passes through.
                .block_mouse_except_scroll()
                .stop(&t).on_click(cx.listener(move |this, _, _, cx| {
                    let current = h.offset();
                    let max = h.max_offset().x;
                    h.set_offset(point((current.x - page * dir).clamp(-max, px(0.0)), current.y));
                    this.invalidate_nothing(cx);
                }))
                .tooltip(crate::ui::tip(&t, if dir < 0.0 { tr("Scroll left") } else { tr("Scroll right") }))
                .child(icon(&this.theme, name, 18.0, t.t1()))
        };
        // the clip stays at 0 too, so the row keeps its shape while the dock slides.
        let mut row = div().relative().w(layout.main_width + under_px).child(crate::ui::fade_right(scroller.into_any_element(), t.s(fade), t.s(fade_gap), SHELF_FADE_STOPS));
        if more_left {
            let a = arrow(self, "caret-left-bold", -1.0, cx).left(t.s(10.0 + under));
            row = row.child(a);
        }
        if more_right {
            let a = arrow(self, "caret-right-bold", 1.0, cx).right(t.s(10.0 + fade_gap));
            row = row.child(a);
        }
        let title_link = if shelf.more.is_some() { None } else { shelf.link.clone() };
        let title = self.section_title_sorted(shelf.title.clone(), title_link, shelf.link_sort, cx);
        let more = shelf.more.zip(shelf.link.clone()).map(|(label, route)| {
            let sort = shelf.link_sort;
            crate::ui_controls::small_glass_action(&t, ("shelf-more", shelf.id)).flex_none().child(label).stop(&t).on_click(cx.listener(move |this, _, _, cx| {
                this.albums_sort_on_open = sort;
                this.navigate(route.clone(), cx);
                this.albums_sort_on_open = None;
            }))
        });
        let head = div().flex().flex_row().items_center().justify_between().gap(t.s(SHELF_HEAD_GAP)).child(div().min_w(px(0.0)).child(title)).children(more);
        div()
            .group(SharedString::from(group.clone()))
            .w(layout.main_width + under_px)
            .child(div().pl(t.s(m.gutter + under)).pr(t.s(m.gutter)).mb(t.s(SHELF_HEAD_GAP)).child(head).when_some(shelf.note.clone(), |d, note| d.child(div().mt(t.s(2.0)).text_size(t.text(TEXT_SM)).text_color(t.t2()).child(note))))
            .child(row)
            .into_any_element()
    }

    /// A shelf's card width, gap and row height in tokens. Genre tiles carry their text inside.
    fn shelf_card(shelf: &Shelf, m: &PageMetrics) -> (f32, f32, f32) {
        if matches!(shelf.items.first(), Some(CardItem::GenreFan(..))) {
            return (m.grid_card, m.tile_gap, m.grid_card / GENRE_TILE_RATIO);
        }
        // An Artist Radio tile has only the "With …" line under it.
        if matches!(shelf.items.first(), Some(CardItem::ArtistStation { .. })) {
            return (m.card_w, m.card_gap, m.card_w + CARD_TEXT_GAP + TEXT_MD * 1.3 * STATION_LINES as f32);
        }
        let gap = if shelf.tiles { m.tile_gap } else { m.card_gap };
        let card = if shelf.tiles { m.grid_card.min(m.card_w) } else { m.card_w };
        (card, gap, card + Shell::card_text_lines(shelf.sub_lines > 0 || !matches!(shelf.items.first(), Some(CardItem::Artist(_))), shelf.sub_lines.max(1)))
    }

    /// a row of a card grid: cards stretched to fill the width; height follows the tallest card.
    pub fn render_grid_row(&mut self, items: &Rc<Vec<CardItem>>, start: usize, count: usize, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let m = PageMetrics::of(layout);
        let (_, card) = crate::pages::grid_fit(self.grid_width(layout), m.grid_card, m.card_gap);
        let mut row = div().flex().flex_row().gap(t.s(m.card_gap)).pl(t.s(m.gutter)).pb(t.s(34.0)).w(layout.main_width);
        for i in start..(start + count).min(items.len()) {
            row = row.child(self.render_card(&items[i], card, i, layout, cx));
        }
        row.into_any_element()
    }

    /// The shared empty state for pages built from the whole library. Before the first sync finishes the library is
    /// unknown, not empty.
    pub fn library_empty_state(&self, icon_name: &'static str, title: impl Into<SharedString>, hint: impl Into<SharedString>, compact: bool) -> gpui::Div {
        use crate::shell::LibraryState::*;
        let (icon_name, title, hint): (&'static str, SharedString, SharedString) = match self.library_state {
            Ready => (icon_name, title.into(), hint.into()),
            Opening => ("music-notes-regular", tr("Opening library").into(), SharedString::default()),
            Syncing => ("music-notes-regular", tr("Syncing library").into(), tr("Music shows up here as soon as it arrives.").into()),
            NoServer => ("music-notes-regular", tr("No server connected").into(), tr("Navia plays music from a Navidrome server.").into()),
            Unreachable => ("warning-fill", tr("Can't reach the server").into(), tr("Check the address and your connection. The music shows up once Navia can connect.").into()),
            SyncFailed => ("warning-fill", tr("Couldn't finish syncing").into(), tr("Part of your library may be missing. Navia will try again.").into()),
        };
        let state = self.empty_state(icon_name, title, hint, compact);
        match self.library_state {
            Syncing => state.children(self.render_sync_meter()),
            _ => state,
        }
    }

    pub fn empty_state(&self, icon_name: &'static str, title: impl Into<SharedString>, hint: impl Into<SharedString>, compact: bool) -> gpui::Div {
        let t = self.theme;
        let (title, hint): (SharedString, SharedString) = (title.into(), hint.into());
        div()
            .w_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(t.s(8.0))
            .py(t.s(if compact { 36.0 } else { 80.0 }))
            .px(t.s(GUTTER))
            .text_center()
            .text_color(t.t2())
            .child(icon(&t, icon_name, if compact { 26.0 } else { 34.0 }, t.t3()))
            .child(div().text_size(t.text(TEXT_LG)).font_weight(FontWeight::SEMIBOLD).text_color(t.t1()).child(title))
            .when(!hint.is_empty(), |d| d.child(div().max_w(t.s(440.0)).line_height(relative(1.45)).child(hint)))
    }

    /// Soft light behind the sidebar from cards under it, for renderers without backdrop blur.
    pub fn render_shelf_glows(&mut self, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        if !self.page_uses_list() || !layout.sidebar_visible || layout.sidebar_overlay {
            return div().into_any_element();
        }
        let m = PageMetrics::of(layout);
        let blocks = self.page.blocks.clone();
        let mut layer = div().absolute().inset_0();
        let _ = cx;
        for (ix, block) in blocks.iter().enumerate() {
            let Block::Shelf(shelf) = block else { continue };
            let Some(bounds) = self.page.state.bounds_for_item(ix) else { continue };
            let (card, gap, _) = Self::shelf_card(shelf, &m);
            let offset = (-f32::from(shelf.scroll.offset().x)) / t.token_scale();
            if offset <= 0.0 {
                continue;
            }
            let step = card + gap;
            let first = (((offset - m.gutter - f32::from(layout.sidebar_width) / t.token_scale() - card) / step).floor() as isize).max(0) as usize;
            let last = (((offset - m.gutter) / step).ceil() as usize).min(shelf.items.len());
            let top = bounds.origin.y - self.edge.top + t.s(TEXT_SECTION * 1.2 + 16.0);
            for i in first..last {
                let x = m.gutter + i as f32 * step - offset;
                let color = self.card_tint(&shelf.items[i]);
                layer = layer.child(
                    div()
                        .absolute()
                        .left(t.s(x))
                        .top(top)
                        .size(t.s(card))
                        .rounded(t.s(R_MD))
                        .shadow(vec![BoxShadow { color: color.hsla(0.13), offset: point(px(0.0), px(0.0)), blur_radius: t.s(46.0), spread_radius: t.s(-6.0), inset: false }]),
                );
            }
        }
        layer.into_any_element()
    }

    pub fn card_tint(&mut self, item: &CardItem) -> Srgb {
        match item {
            CardItem::Album { index, .. } => self.albums.get(*index as usize).cloned().map_or(Srgb::hex(crate::theme::TINT_FALLBACK), |a| self.album_tint(&a.id)),
            CardItem::Artist(index) | CardItem::Essentials(index) | CardItem::SongRadio(index) => self.artists.get(*index as usize).cloned().map_or(Srgb::hex(crate::theme::TINT_FALLBACK), |a| self.artist_tint(&a.id)),
            _ => Srgb::hex(crate::theme::TINT_FALLBACK),
        }
    }
}

pub fn song_count(n: u32) -> SharedString {
    trn("{count} Song", "{count} Songs", n as usize).into()
}

pub fn group_thousands(n: usize) -> String {
    crate::i18n::fmt_int(n as i64)
}

pub fn long_duration(seconds: u32) -> String {
    let m = (seconds as f32 / 60.0).round() as u32;
    let m = m.max(u32::from(seconds > 0));
    if m >= 60 {
        trf("{hours} hr {minutes} min", &[("hours", &(m / 60).to_string()), ("minutes", &(m % 60).to_string())])
    } else {
        trn("{count} minute", "{count} minutes", m as usize)
    }
}

pub fn songs_and_length(n: u32, seconds: u32) -> String {
    if n == 0 { tr("No songs").to_string() } else { format!("{}, {}", song_count(n), long_duration(seconds)) }
}

/// A playlist's song count, plus its owner when it isn't yours.
pub fn playlist_subtitle(p: &crate::model::PlaylistDetails) -> SharedString {
    if p.mine || p.owner.is_empty() {
        song_count(p.song_count)
    } else {
        format!("{} • {}", song_count(p.song_count), crate::i18n::trf("By {owner}", &[("owner", &p.owner)])).into()
    }
}

/// Maximum lines for the "With …" under an Artist Radio tile.
const STATION_LINES: u32 = 2;

/// An Artist Radio station's neighbors by name, at most three, or empty with none.
fn station_line(neighbours: &[crate::model::Artist]) -> String {
    if neighbours.is_empty() {
        return String::new();
    }
    let names = neighbours.iter().take(3).map(|a| a.name.as_ref()).collect::<Vec<_>>().join(", ");
    if neighbours.len() > 3 { trf("With {names} and more", &[("names", &names)]) } else { trf("With {names}", &[("names", &names)]) }
}

fn labelled_art(t: &crate::theme::Theme, art: AnyElement, label: impl Into<SharedString>) -> AnyElement {
    div().relative().child(art).child(crate::ui::cover_label(t, label, crate::ui::LabelAt::Bottom, crate::ui::LabelStyle::Chip).font_features(crate::ui::tabular_figures())).into_any_element()
}
