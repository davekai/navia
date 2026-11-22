//! Discover: For You, New, Charts and Radio, as the tabs in the top pill.

use crate::ui::FullTextTip;
use crate::i18n::tr;
use gpui::prelude::*;
use crate::focus::FocusStop;
use gpui::{AnyElement, Context, FontWeight, SharedString, div, px, relative};

use crate::pages::{Block, CardItem, DiscoverTab, PageMetrics, Route};
use crate::shell::{Layout, Shell};
use crate::theme::size::*;
use crate::ui::icon;

impl Shell {
    pub fn build_discover(&mut self, tab: DiscoverTab, layout: &Layout) -> Vec<Block> {
        let m = PageMetrics::of(layout);
        match tab {
            DiscoverTab::ForYou => self.build_for_you(m, layout),
            DiscoverTab::New => self.build_new(m, layout),
            DiscoverTab::Charts => self.build_charts(m, layout),
            DiscoverTab::Radio => self.build_radio(m, layout),
        }
    }

    fn build_new(&mut self, m: PageMetrics, layout: &Layout) -> Vec<Block> {
        let albums = self.albums.clone();
        let mut recent: Vec<u32> = (0..albums.len() as u32).collect();
        recent.sort_by_key(|i| std::cmp::Reverse(albums[*i as usize].added));
        if recent.is_empty() {
            return vec![Shell::plain_block(|this, _, _, _| this.library_empty_state("clock-regular", tr("Nothing new yet"), tr("Albums added to the server show up here."), false).into_any_element())];
        }
        let features: Vec<u32> = recent.iter().take(3).copied().collect();
        let mut sections: Vec<Vec<Block>> = vec![vec![Shell::plain_block(move |this, layout, _, cx| this.render_features(&features, layout, cx))]];
        let latest: Vec<u32> = recent.iter().take(40).filter_map(|i| self.album_tracks_cached(&albums[*i as usize].id).first().copied()).take(8).collect();
        if !latest.is_empty() {
            self.page_mini_artist = true;
            let mut block = vec![Self::section_heading(tr("Latest Songs").into())];
            block.extend(self.mini_blocks(latest, layout, false, Some(tr("Latest Songs"))));
            sections.push(block);
        }
        let rest: Vec<CardItem> = recent.iter().skip(3).take(19).map(|index| CardItem::Album { index: *index, by_artist: false }).collect();
        sections.push(self.shelf_block(tr("Latest Albums"), None, rest, false, 2));
        Shell::sections(m.section_gap, sections)
    }

    pub fn section_heading(title: SharedString) -> Block {
        Shell::plain_block(move |this, layout, _, cx| {
            let t = this.theme;
            let m = PageMetrics::of(layout);
            let heading = this.section_title(title.clone(), None, cx);
            div().px(t.s(m.gutter)).pb(t.s(SHELF_HEAD_GAP)).child(heading).into_any_element()
        })
    }

    pub fn section_heading_with_note(title: SharedString, note: SharedString) -> Block {
        Shell::plain_block(move |this, layout, _, cx| {
            let t = this.theme;
            let m = PageMetrics::of(layout);
            let heading = this.section_title(title.clone(), None, cx);
            div().px(t.s(m.gutter)).pb(t.s(SHELF_HEAD_GAP)).child(heading).child(div().mt(t.s(2.0)).text_size(t.text(TEXT_SM)).text_color(t.t2()).child(note.clone())).into_any_element()
        })
    }

    fn render_features(&mut self, features: &[u32], layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let m = PageMetrics::of(layout);
        let main = f32::from(layout.main_width) / t.token_scale();
        let single = main <= 760.0;
        let mut row = div().flex().w_full().px(t.s(m.gutter)).gap(t.s(13.0));
        row = if single { row.flex_col() } else { row.flex_row() };
        for (slot, ix) in features.iter().enumerate() {
            let Some(album) = self.albums.get(*ix as usize).cloned() else { continue };
            let tint = self.album_tint(&album.id);
            let cover_px = (f32::from(t.s(240.0)) * layout.scale).round() as u32;
            let cover = self.thumb_faded(&album.id, cover_px);
            // A tiny stretched thumbnail stands in for a blurred wash.
            let wash = self.thumb_faded(&album.id, 24);
            let route = Route::Album(album.id.clone());
            let group = format!("feature-{slot}");
            row = row.child(
                crate::ui::press_dim(div().id(("feature", slot)))
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.0))
                    .line_height(relative(1.3))
                    .stop(&t).on_click(cx.listener(move |this, _, _, cx| this.navigate(route.clone(), cx)))
                    .child(div().text_size(t.text(TEXT_SM)).font_weight(FontWeight::SEMIBOLD).text_color(t.t2()).child(tr("NEW ALBUM")))
                    .child(div().mt(t.s(4.0)).text_size(t.text(TEXT_FEATURE)).font_weight(FontWeight::MEDIUM).truncate().id("name").child(album.name.clone()).full_text_tip(&t, album.name.clone()))
                    .child(div().mb(t.s(14.0)).text_size(t.text(TEXT_FEATURE)).font_weight(FontWeight::SEMIBOLD).text_color(t.t2()).truncate().id("artist").child(album.artist.clone()).full_text_tip(&t, album.artist.clone()))
                    .child(
                        div()
                            .relative()
                            .h(t.s(284.0))
                            .rounded(t.s(R_MD))
                            .overflow_hidden()
                            .bg(tint.hsla(1.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .when_some(wash, |d, (wash, fade)| d.child(crate::ui::image(wash).absolute().inset_0().size_full().opacity(HERO_GLOW_OPACITY * fade)))
                            .when_some(cover, |d, (cover, fade)| {
                                d.child(crate::ui::hero_art_frame(&t, R_SM, crate::ui::image(cover).size(t.s(284.0 * 0.8)).rounded(t.s(R_SM))).opacity(fade))
                            }),
                    )
                    .child(crate::ui::card_track(&group)),
            );
        }
        row.into_any_element()
    }

    fn build_charts(&mut self, m: PageMetrics, layout: &Layout) -> Vec<Block> {
        let songs = self.top_rows(16);
        let mut sections: Vec<Vec<Block>> = Vec::new();
        if !songs.is_empty() {
            self.page_mini_artist = false;
            let mut block = vec![Self::section_heading_with_note(tr("Your Top Songs").into(), tr("Last 6 months").into())];
            block.extend(self.mini_blocks(songs, layout, false, Some(tr("Your Top Songs"))));
            sections.push(block);
        }
        let taste = self.library.as_ref().map(|l| l.home_taste()).unwrap_or_default();
        let albums = taste.albums.iter().take(16).map(|index| CardItem::Album { index: *index, by_artist: false }).collect();
        sections.push(self.shelf_block(tr("Your Top Albums"), None, albums, false, 2));
        let artists = taste.artists.iter().take(32).map(|index| CardItem::Artist(*index)).collect();
        sections.push(self.shelf_block(tr("Your Top Artists"), None, artists, false, 0));
        let page = Shell::sections(m.section_gap, sections);
        if page.is_empty() {
            return vec![Shell::plain_block(|this, _, _, _| this.library_empty_state("clock-regular", tr("No play history yet"), tr("Once you've played some music, your most played shows up here."), false).into_any_element())];
        }
        page
    }

    fn build_radio(&mut self, m: PageMetrics, _layout: &Layout) -> Vec<Block> {
        let stations = self.library.as_ref().map(|l| l.radio_stations()).unwrap_or_default();
        let mut sections: Vec<Vec<Block>> = Vec::new();
        if !stations.is_empty() {
            let mut block = vec![Self::section_heading(tr("Stations").into())];
            block.push(Shell::plain_block(move |this, layout, _, cx| this.render_stations(&stations, layout, cx)));
            sections.push(block);
        }
        let artists = self.artists.clone();
        let mut radio = Vec::new();
        // Artists played lately first, then everyone else in the library's order.
        let taste = self.library.as_ref().map(|l| l.home_taste()).unwrap_or_default();
        let first: std::collections::HashSet<u32> = taste.artists.iter().copied().collect();
        let order = taste.artists.iter().copied().chain((0..artists.len() as u32).filter(|i| !first.contains(i)));
        for i in order.take(120) {
            let Some(a) = artists.get(i as usize) else { continue };
            // Shown until the server says it has nothing. It's only asked when a tile is played.
            let known_empty = self.library.as_ref().and_then(|l| l.similar_known(&a.id)).is_some_and(|s| s.is_empty());
            if !known_empty {
                radio.push(CardItem::SongRadio(i));
                if radio.len() == 14 {
                    break;
                }
            }
        }
        sections.push(self.shelf_block(tr("Song Radio"), None, radio, false, 1));
        let page = Shell::sections(m.section_gap, sections);
        if page.is_empty() {
            return vec![Shell::plain_block(|this, _, _, _| this.library_empty_state("broadcast-regular", tr("No radio yet"), tr("Internet stations on the server show up here. Song radio needs similar-artist data from the server."), false).into_any_element())];
        }
        page
    }

    fn render_stations(&mut self, stations: &[crate::model::RadioStation], layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let m = PageMetrics::of(layout);
        let mut wrap = div().flex().flex_wrap().gap(t.s(m.card_gap)).px(t.s(m.gutter));
        for (i, station) in stations.iter().enumerate() {
            let selected = station.clone();
            let label: SharedString = match self.player.station.as_ref().filter(|s| s.id == station.id) {
                Some(_) if self.player.playing => tr("Playing now").into(),
                Some(_) => tr("Paused").into(),
                None => tr("Internet radio").into(),
            };
            let group = format!("station-{i}");
            let (hover, ticker) = crate::ui::card_hover(&group, crate::theme::motion::FAST);
            wrap = wrap.child(
                crate::ui::press_dim(div().id(("station", i)))
                    .relative()
                    .flex()
                    .flex_col()
                    .justify_end()
                    .w(t.s(300.0))
                    .h(t.s(150.0))
                    .p(t.s(16.0))
                    .rounded(t.s(R_MD))
                    .bg(gpui::linear_gradient(135.0, gpui::linear_color_stop(t.accent.hsla(1.0), 0.0), gpui::linear_color_stop(t.accent.mix(crate::color::Srgb::hex(0), 0.4).hsla(1.0), 1.0)))
                    .stop(&t).on_click(cx.listener(move |this, _, _, cx| this.core_play_or_toggle_station(selected.clone(), cx)))
                    .child(div().text_size(t.text(TEXT_FEATURE)).font_weight(FontWeight::SEMIBOLD).truncate().id("name").child(station.name.clone()).full_text_tip(&t, station.name.clone()))
                    .child(div().text_color(t.t2()).child(label))
                    .child(
                        div()
                            .absolute()
                            .top(t.s(14.0))
                            .right(t.s(14.0))
                            .size(t.s(34.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .bg(t.hover())
                            .opacity(hover)
                            .child(icon(&t, "play-fill", 16.0, t.t_on_art())),
                    )
                    .child(ticker)
                    .child(crate::ui::card_track(&group)),
            );
        }
        wrap.into_any_element()
    }
}
