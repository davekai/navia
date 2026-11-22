use crate::ui::FullTextTip;
use crate::i18n::{tr, trf};
use gpui::prelude::*;
use crate::ui::LinkCursor;
use navia_core::library::collections::CollectionKind;
use crate::focus::FocusStop;
use gpui::{AnyElement, Context, FontWeight, SharedString, div, px, relative};

use crate::pages::{AlbumScope, Block, CardItem, PageMetrics, RowVariant, Route};
use crate::shell::{Layout, Shell};
use crate::theme::size::*;
use crate::ui::{divider, glass_pill, icon, icon_button};
use crate::view_cards::songs_and_length;
use crate::view_menu::MenuRow;
use crate::view_about::AboutText;

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum SongFilter {
    #[default]
    All,
    Favorites,
    Downloaded,
}

#[derive(Default)]
pub struct AlbumDetail {
    pub filter: SongFilter,
    pub query: String,
}

impl Shell {
    pub fn build_album(&mut self, id: &SharedString, layout: &Layout) -> Vec<Block> {
        let Some(album) = self.albums.iter().find(|a| &a.id == id).cloned() else { return vec![Self::not_found(tr("This album isn't here"))] };
        let m = PageMetrics::of(layout);
        let tracks = self.library.as_ref().map(|l| l.album_tracks(id)).unwrap_or_default();
        let all: Vec<u32> = tracks.iter().map(|t| t.song).collect();
        let hires = tracks.iter().any(|t| t.hi_res);
        let needle = self.album_detail.query.trim().to_lowercase();
        let filter = self.album_detail.filter;
        let shown: Vec<u32> = all
            .iter()
            .copied()
            .filter(|s| {
                let song = self.song_peek(*s);
                let ok = match filter {
                    SongFilter::All => true,
                    SongFilter::Favorites => self.is_starred(*s),
                    SongFilter::Downloaded => self.is_downloaded(*s),
                };
                ok && (needle.is_empty() || song.is_some_and(|song| song.title.to_lowercase().contains(&needle)))
            })
            .collect();
        self.page_numbers = std::rc::Rc::new(shown.iter().map(|s| tracks.iter().find(|t| t.song == *s).and_then(|t| t.number)).collect());
        let description = self.library.as_ref().map(|l| l.album_description(id)).unwrap_or_default();
        self.core_preload_rows(&all);
        let ranks = if self.deezer.choice == Some(true) { self.library.as_ref().map(|l| l.album_heat(id)).unwrap_or_default() } else { Default::default() };
        let foot = crate::color::tint_fg(self.album_tint(id)).hsla(1.0);
        self.page_heat = crate::heat::PageHeat::new(crate::heat::HeatScale::new(ranks.into_iter().filter(|(s, _)| shown.contains(s)).collect()), foot);
        let list = self.add_list(all.clone());
        let table = self.add_list(shown.clone());
        let album_for_head = album.clone();
        let head = Shell::plain_block(move |this, layout, _, cx| this.render_album_head(&album_for_head, hires, &description, layout, cx));
        let album_for_actions = album.clone();
        let actions = Shell::plain_block(move |this, layout, _, cx| this.render_album_actions(&album_for_actions, list, layout, cx));
        let mut blocks = vec![head, Block::Spacer(ALBUM_SECTION_GAP), actions, Block::Spacer(ALBUM_SECTION_GAP)];
        if shown.is_empty() {
            let empty_all = all.is_empty();
            let filtered = !needle.is_empty();
            blocks.push(Shell::plain_block(move |this, _, _, cx| {
                if empty_all {
                    this.empty_state("music-notes-regular", tr("No songs in this album"), "", true).into_any_element()
                } else {
                    let hint = if filtered { tr("Nothing in this album matches your search.") } else { tr("No songs on this album are in this filter.") };
                    this.empty_state("magnifying-glass-regular", tr("No songs match"), hint, true).child(this.clear_album_filter_button(cx)).into_any_element()
                }
            }));
        } else {
            // Disc headers only for multi-disc albums, using the server's disc titles when present.
            let discs: std::collections::BTreeSet<u32> = tracks.iter().map(|t| t.disc.max(1)).collect();
            let disc_of = |s: u32| tracks.iter().find(|t| t.song == s).map_or(1, |t| t.disc.max(1));
            blocks.push(Shell::plain_block(|this, layout, _, cx| if this.album_header_shown(layout) { this.render_album_header(layout, cx) } else { div().into_any_element() }));
            let mut last = None;
            for (pos, song) in shown.iter().enumerate() {
                let disc = disc_of(*song);
                if discs.len() > 1 && last != Some(disc) {
                    let title = album.disc_titles.iter().find(|(n, _)| *n == disc).map(|(_, name)| name.clone());
                    blocks.push(Shell::plain_block(move |this, layout, _, _| this.render_disc_header(disc, title.clone(), layout)));
                }
                last = Some(disc);
                blocks.push(Block::Table { list: table, pos, variant: RowVariant::Album });
            }
        }
        // "More by <album artist>" goes last. It's hidden for Various Artists or when there's nothing else.
        if let Some(more) = self.library.as_ref().and_then(|l| l.more_by(id))
            && let Some(artist) = self.artists.get(more.artist as usize).cloned()
        {
            let cards: Vec<CardItem> = more.albums.iter().map(|index| CardItem::Album { index: *index, by_artist: true }).collect();
            let link = Route::Albums(AlbumScope::Artist(artist.id.clone(), more.section));
            let shelf = self.shelf_block_more(trf("More by {name}", &[("name", &artist.name)]), tr("See Discography"), link, Some(crate::view_albums::AlbumSort::Year), cards, 2);
            if !shelf.is_empty() {
                blocks.push(Block::Spacer(m.section_gap));
                blocks.extend(shelf);
            }
        }
        blocks
    }

    /// its own name, or "Disc 2".
    fn render_disc_header(&mut self, disc: u32, title: Option<SharedString>, layout: &Layout) -> AnyElement {
        let t = self.theme;
        let m = PageMetrics::of(layout);
        let text: SharedString = title.unwrap_or_else(|| crate::i18n::trf("Disc {number}", &[("number", &disc.to_string())]).into());
        div()
            .relative()
            .mx(t.s(m.gutter))
            .h(t.s(52.0))
            .pt(t.s(14.0))
            .pl(t.s(17.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(t.s(10.0))
            .text_color(t.t2())
            .font_weight(FontWeight::MEDIUM)
            .child(icon(&t, "disc-regular", 18.0, t.t2()))
            .child(div().min_w(px(0.0)).truncate().child(text))
            .child(div().absolute().left(px(0.0)).right(px(0.0)).bottom(px(0.0)).h(px(1.0)).bg(t.line()))
            .into_any_element()
    }

    pub fn not_found(what: &'static str) -> Block {
        Shell::plain_block(move |this, _, _, _| this.empty_state("magnifying-glass-regular", what, tr("It may have been removed from the server."), false).into_any_element())
    }

    fn clear_album_filter_button(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        crate::ui_controls::empty_action(&t, "clear-album-filter")
            .stop(&t).on_click(cx.listener(|this, _, _, cx| {
                this.album_detail.query.clear();
                this.album_detail.filter = SongFilter::All;
                this.search_active = false;
                this.invalidate_page();
                cx.notify();
            }))
            .child(tr("Clear"))
            .into_any_element()
    }

    fn render_album_head(&mut self, album: &crate::model::Album, hires: bool, description: &SharedString, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let m = PageMetrics::of(layout);
        let art = self.hero_artwork(&album.id, "music-notes-fill", m.hero_art, R_MD, layout, cx);
        let tint = self.album_tint(&album.id);
        let tint_fg: gpui::Hsla = crate::color::tint_fg(tint).hsla(1.0);
        let tint_dim: gpui::Hsla = crate::color::tint_fg_dim(tint).hsla(1.0);
        let artist = self.album_artist_line(&album.artist, tint_fg, cx);
        let mut meta = album_meta_line(&t, tint_dim);
        if !album.genre.is_empty() {
            let genre = self.link(("album-genre", 0), album.genre.clone(), Some(crate::song_links::LinkTarget::Genre(album.genre.clone())), tint_fg, cx);
            meta = meta.child(div().font_weight(FontWeight::SEMIBOLD).child(genre)).child(div().px(t.s(5.0)).child("•"));
        }
        if album.year > 0 {
            meta = meta.child(format!("{} • ", album.year));
        }
        // The edition is its own meta item rather than part of the name.
        if let Some(edition) = album.edition.clone() {
            meta = meta.child(format!("{edition} • "));
        }
        meta = meta.child(songs_and_length(album.song_count, album.duration_s));
        if let Some(status) = self.download_status_line(CollectionKind::Album, &album.id) {
            meta = meta.child(status);
        }
        if hires {
            meta = meta.child(div().flex().flex_row().items_center().gap(t.s(5.0)).child(" • ").child(icon(&t, "waveform-regular", 16.0, tint_dim)).child(tr("Hi-res Lossless")));
        }
        let title = album_title(&t, &m, album.name.clone(), album.explicit);
        let mut info = album_info(&t).child(title).child(artist).child(meta);
        if !description.is_empty() {
            let about = AboutText { title: album.name.clone(), subtitle: (!album.artist.is_empty()).then(|| album.artist.clone()), art: Some(album.id.clone()), text: description.clone() };
            info = info.child(self.header_more_text("album-desc", about, cx));
        }
        album_head(&t, &m, art, info, layout)
    }

    pub fn album_artist_line(&mut self, artist: &SharedString, tint_fg: gpui::Hsla, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let known_artist = self.artist_named(artist).map(|a| a.id.clone());
        let name_row = div().flex().flex_row().items_center().gap(t.s(5.0)).mt(t.s(ALBUM_ARTIST_GAP)).max_w_full().text_size(t.text(TEXT_SUBTITLE)).font_weight(FontWeight::SEMIBOLD).text_color(tint_fg).child(div().id("album-artist").min_w(px(0.0)).truncate().child(artist.clone()).full_text_tip(&t, artist.clone()));
        if let Some(id) = known_artist {
            name_row.id("album-artist").link_cursor().child(icon(&t, "caret-right-bold", 21.0, tint_fg)).stop(&t).on_click(cx.listener(move |this, _, _, cx| this.navigate(Route::Artist(id.clone()), cx))).into_any_element()
        } else if crate::song_links::split_credit(artist).iter().any(|(name, is_name)| *is_name && self.artist_named(&SharedString::from(name.clone())).is_some()) {
            // each credited artist the library knows is its own link.
            let names = self.artist_name_links(("album-artists", 0), artist, tint_fg, cx);
            div().flex().flex_row().mt(t.s(ALBUM_ARTIST_GAP)).max_w_full().min_w(px(0.0)).text_size(t.text(TEXT_SUBTITLE)).font_weight(FontWeight::SEMIBOLD).child(names).into_any_element()
        } else {
            name_row.into_any_element()
        }
    }

    /// a page header's description: three lines at most, MORE (only when cut) opens the whole text in the About sheet.
    pub fn header_more_text(&mut self, id: &'static str, about: AboutText, cx: &mut Context<Self>) -> AnyElement {
        let text = about.text.clone();
        self.more_text(id, text, false, false, cx, move |this, cx| this.open_about_text(about.clone(), cx))
    }

    /// GPUI can't wrap text around a float, so the third line is drawn alone, ending in an ellipsis before MORE.
    /// `inset`: in a padded group, in the body ink, with no room above or width cap, and MORE opens it in place.
    pub fn more_text(&mut self, id: &'static str, text: SharedString, open: bool, inset: bool, cx: &mut Context<Self>, toggle: impl Fn(&mut Shell, &mut Context<Shell>) + 'static) -> AnyElement {
        let t = self.theme;
        let measure = self.more_measures.entry(id).or_default().clone();
        let known = measure.borrow().clone().filter(|m| m.text == text);
        let line_h = t.s(22.5);
        let toggle = std::rc::Rc::new(toggle);
        let label = move |open: bool, toggle: std::rc::Rc<dyn Fn(&mut Shell, &mut Context<Shell>)>, cx: &mut Context<Self>| {
            div()
                .id(id)
                .flex_none()
                .ml(t.s(8.0))
                .h(line_h)
                .flex()
                .items_center()
                .text_color(t.t1())
                .font_weight(FontWeight::SEMIBOLD)
                .link_cursor()
                .hover(|s| s.opacity(0.75))
                .stop(&t)
                .on_click(cx.listener(move |this, _, _, cx| toggle(this, cx)))
                .child(if open { tr("LESS") } else { tr("MORE") })
        };
        // Measure how the text wraps at its width. A change redraws once.
        let probe_text = text.clone();
        let probe = gpui::canvas(
            move |bounds, window, _| {
                let style = window.text_style();
                let font_size = style.font_size.to_pixels(window.rem_size());
                let run = style.to_run(probe_text.len());
                let split = window.text_system().shape_text(probe_text.clone(), font_size, &[run], Some(bounds.size.width), None).ok().and_then(|lines| third_line_start(&lines));
                let next = MoreMeasure { text: probe_text.clone(), width: bounds.size.width, split };
                let changed = measure.borrow().as_ref().is_none_or(|m| m.text != next.text || m.split != next.split || (m.width - next.width).abs() > px(0.5));
                if changed {
                    *measure.borrow_mut() = Some(next);
                    window.refresh();
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top(px(0.0))
        .left(px(0.0))
        .w_full()
        .h(px(1.0));
        let body = div().relative().w_full().line_height(line_h).child(probe);
        let body = if inset { body.text_color(t.t1()) } else { body.mt(t.s(14.0)).max_w(t.s(620.0)).text_color(t.t2()) };
        let split = known.and_then(|m| m.split);
        let body = match (open, split) {
            (false, Some(at)) => {
                let (head, tail) = text.split_at(at);
                let third = third_line(tail);
                body.child(div().child(SharedString::from(head.trim_end().to_string())))
                    .child(div().flex().flex_row().items_center().min_w(px(0.0)).child(div().flex_1().min_w(px(0.0)).truncate().child(third)).child(label(false, toggle, cx)))
            }
            // Not measured yet, or it fits. Up to three lines, no label.
            (false, None) => body.child(div().line_clamp(3).text_ellipsis().child(text)),
            (true, _) => {
                let body = body.child(div().child(text));
                if split.is_some() { body.child(div().flex().flex_row().justify_end().child(label(true, toggle, cx))) } else { body }
            }
        };
        body.into_any_element()
    }

    fn render_album_actions(&mut self, album: &crate::model::Album, list: usize, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let m = PageMetrics::of(layout);
        let songs = self.page.lists.get(list).cloned().unwrap_or_default();
        let has_songs = !songs.is_empty();
        let starred = self.album_starred(&album.id);
        let album_id = album.id.clone();
        let menu_songs = songs.as_ref().clone();
        let album_name = album.name.clone();
        let album_id_info = album.id.clone();
        let actions = vec![
            icon_button(&t, "album-star", tr("Favorite"), if starred { "heart-fill" } else { "heart-regular" }, starred)
                .stop(&t).on_click(cx.listener(move |this, _, _, cx| {
                    if !starred {
                        crate::ui::pop("album-star");
                    }
                    this.toggle_album_star(&album_id, cx);
                }))
                .into_any_element(),
            self.download_toggle("album-download", CollectionKind::Album, album.id.clone(), album.name.clone(), has_songs, cx),
            icon_button(&t, "album-more", tr("More"), "dots-three-bold", false)
                .stop(&t).on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                    let queue_songs = menu_songs.clone();
                    let rows = vec![
                        MenuRow::item(tr("Add to Queue"), "queue-regular", move |this, cx| this.add_to_queue(&queue_songs, cx)),
                        this.add_to_playlist_row(tr("Add to Playlist"), menu_songs.clone(), album_name.clone()),
                        this.share_row(navia_core::shares::ShareKind::Album, album_id_info.clone()),
                        this.info_row(crate::view_info::InfoTarget::Album(album_id_info.clone())),
                    ];
                    this.open_toolbar_menu(rows, event, cx);
                }))
                .into_any_element(),
        ];
        let filter = self.album_detail.filter;
        let row = div()
            .flex()
            .flex_row()
            .items_center()
            .flex_wrap()
            .gap(t.s(8.0))
            .px(t.s(m.gutter))
            .w(layout.main_width)
            .child(self.render_play_actions(list, has_songs, cx))
            .child(pill_group(&t, actions))
            .child(div().flex_1())
            .child(glass_pill(&t).child(icon_button(&t, "album-columns", tr("Columns"), "columns-regular", false).stop(&t).on_click(cx.listener(|this, event: &gpui::ClickEvent, _, cx| {
                let rows = this.album_column_rows();
                this.open_toolbar_menu(rows, event, cx);
            }))))
            .child(glass_pill(&t).child(icon_button(&t, "album-filter", tr("Filter"), "funnel-simple-regular", filter != SongFilter::All).stop(&t).on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                let rows = this.song_filter_rows(filter, |this, f| this.album_detail.filter = f);
                this.open_toolbar_menu(rows, event, cx);
            }))))
            .child(self.search_pill(tr("Search Album"), self.album_detail.query.clone(), cx));
        row.into_any_element()
    }

    pub fn song_filter_rows(&mut self, current: SongFilter, set: impl Fn(&mut Shell, SongFilter) + 'static) -> Vec<MenuRow> {
        let set = std::rc::Rc::new(set);
        [(SongFilter::All, tr("All Songs")), (SongFilter::Favorites, tr("Favorites")), (SongFilter::Downloaded, tr("Downloaded"))]
            .into_iter()
            .map(|(f, label)| {
                let set = set.clone();
                MenuRow::check(label, current == f, move |this, cx| {
                    set(this, f);
                    this.invalidate_page();
                    cx.notify();
                })
            })
            .collect()
    }

    pub fn render_play_actions(&mut self, list: usize, enabled: bool, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let opacity = if enabled { 1.0 } else { 0.4 };
        let play = text_button(&t, ("page-play", list), "play-fill", tr("Play"), enabled)
            .when(enabled, |d| d.stop(&t).on_click(cx.listener(move |this, _, _, cx| this.play_page_list(list, 0, false, cx))));
        let shuffle = crate::ui::ink_button(&t, ("page-shuffle", list), tr("Shuffle"), "shuffle-regular", 20.0, t.t1().into(), t.hover_ink())
            .when(!enabled, |d| d.opacity(opacity))
            .when(enabled, |d| d.stop(&t).on_click(cx.listener(move |this, _, _, cx| {
                let len = this.page.lists.get(list).map_or(0, |l| l.len());
                this.play_page_list(list, this.frame as usize % len.max(1), true, cx);
            })));
        glass_pill(&t).child(play).child(divider(&t)).child(shuffle).into_any_element()
    }

    /// the route decides what it searches.
    pub fn search_pill(&mut self, placeholder: &'static str, query: String, cx: &mut Context<Self>) -> AnyElement {
        self.fitted_search(["page-search", "page-search-clear", "page-query"], placeholder, query, SEARCH_PILL_W, cx)
    }
}

/// A text button inside a glass pill, with a filled icon and a label. Disabled buttons are dimmed and ignore hover.
pub fn text_button(t: &crate::theme::Theme, id: impl Into<gpui::ElementId>, icon_name: &'static str, label: &'static str, enabled: bool) -> gpui::Stateful<gpui::Div> {
    text_button_on(t, id, Some(icon_name), label, enabled, t.t1().into())
}

/// [`text_button`] in another ink, with or without an icon.
pub fn text_button_on(t: &crate::theme::Theme, id: impl Into<gpui::ElementId>, icon_name: Option<&'static str>, label: &'static str, enabled: bool, ink: gpui::Hsla) -> gpui::Stateful<gpui::Div> {
    let id = id.into();
    let button = div()
        .id(id.clone())
        .flex()
        .flex_row()
        .items_center()
        .gap(t.s(9.0))
        .h(t.s(34.0))
        .pl(t.s(13.0))
        .pr(t.s(12.0))
        .rounded_full()
        .font_weight(FontWeight::MEDIUM)
        .opacity(if enabled { 1.0 } else { 0.4 })
        .text_size(t.text(crate::theme::size::TEXT_MD))
        .text_color(ink)
        .when(enabled, |d| d.child(crate::ui::hover_pill(t)))
        .when_some(icon_name, |d, name| d.child(icon(t, name, 18.0, ink)))
        .child(label);
    if enabled { crate::ui::pressable(button, id) } else { button }
}

/// How wide [`text_button_on`] draws a label without an icon.
pub fn text_button_width(t: &crate::theme::Theme, window: &mut gpui::Window, label: &str) -> gpui::Pixels {
    t.s(13.0 + 12.0) + crate::ui::text_width(window, label, t.text(crate::theme::size::TEXT_MD), FontWeight::MEDIUM)
}

pub fn pill_group(t: &crate::theme::Theme, children: Vec<AnyElement>) -> AnyElement {
    glass_pill(t).children(children).into_any_element()
}

/// What `more_text` measured, including where the third line starts when the text needs more than three.
#[derive(Clone, Default)]
pub struct MoreMeasure {
    pub text: SharedString,
    pub width: gpui::Pixels,
    pub split: Option<usize>,
}

/// the collapsed text's last line, from `tail` to its first line break; a line cut there still ends in an ellipsis.
fn third_line(tail: &str) -> SharedString {
    let tail = tail.trim_start();
    match tail.split_once('\n') {
        // a truncated line draws each line break as a line of its own
        Some((line, _)) => format!("{}…", line.trim_end()).into(),
        None => tail.to_string().into(),
    }
}

/// none unless the text wraps to more than three lines.
fn third_line_start(lines: &[gpui::WrappedLine]) -> Option<usize> {
    let mut starts = vec![0usize];
    let mut offset = 0usize;
    for (n, line) in lines.iter().enumerate() {
        if n > 0 {
            starts.push(offset);
        }
        let glyphs: Vec<usize> = line.unwrapped_layout.runs.iter().flat_map(|r| r.glyphs.iter().map(|g| g.index)).collect();
        for boundary in line.wrap_boundaries() {
            let run_start: usize = line.unwrapped_layout.runs[..boundary.run_ix].iter().map(|r| r.glyphs.len()).sum();
            // The boundary glyph is the first one on the new line.
            let first = run_start + boundary.glyph_ix;
            starts.push(offset + glyphs.get(first).copied().unwrap_or(line.len()));
        }
        offset += line.len() + 1;
    }
    (starts.len() > 3).then(|| starts[2])
}

pub fn album_head(t: &crate::theme::Theme, m: &PageMetrics, art: AnyElement, info: gpui::Div, layout: &Layout) -> AnyElement {
    let stack = f32::from(layout.main_width) / t.token_scale() <= 620.0;
    let art = crate::ui::hero_art_frame(t, R_MD, art);
    let head = div().flex().gap(t.s(ALBUM_HEAD_GAP)).px(t.s(m.gutter)).w(layout.main_width);
    // stacked, the text spans the column between the gutters; fitted to its content it ran past the right one
    let (head, info) = if stack { (head.flex_col().items_start(), info.w_full()) } else { (head.flex_row(), info) };
    head.child(art).child(info).into_any_element()
}

pub fn album_info(t: &crate::theme::Theme) -> gpui::Div {
    div().flex().flex_col().flex_1().min_w(px(0.0)).pt(t.s(ALBUM_INFO_PAD_TOP))
}

pub fn album_title(t: &crate::theme::Theme, m: &PageMetrics, name: SharedString, explicit: bool) -> gpui::Div {
    let title = div().text_size(t.text(m.title)).text_color(t.t_em()).font_weight(FontWeight::BOLD).line_height(relative(1.1)).line_clamp(3).text_ellipsis().id("album-title").child(name.clone()).full_text_tip(t, name);
    div().flex().flex_row().items_center().child(title.min_w(px(0.0))).when(explicit, |d| d.child(crate::ui::explicit_mark(t, (m.title * 0.4).max(13.0) / 0.68)))
}

pub fn album_meta_line(t: &crate::theme::Theme, tint_dim: gpui::Hsla) -> gpui::Div {
    div().flex().flex_row().items_center().flex_wrap().mt(t.s(ALBUM_META_GAP)).text_color(tint_dim).font_weight(FontWeight::MEDIUM)
}

#[cfg(test)]
mod tests {
    use super::third_line;

    #[test]
    fn collapsed_text_stops_at_its_third_lines_break() {
        assert_eq!(third_line(" Excluded by rating: 0\r\nExcluded for being recent: 57").as_ref(), "Excluded by rating: 0…");
        assert_eq!(third_line("the last line, wrapped on").as_ref(), "the last line, wrapped on");
    }
}
