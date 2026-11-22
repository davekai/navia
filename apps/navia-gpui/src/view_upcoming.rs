//! upcoming albums (`navia_core::upcoming`), only with the Deezer switch on.

use std::rc::Rc;

use crate::i18n::{tr, trf, trn};
use gpui::prelude::*;
use gpui::{AnyElement, Context, FontWeight, SharedString, div};
use navia_core::library::taste::{local_day, now_unix};

use crate::model::{Upcoming, UpcomingTrack, UpcomingView};
use crate::pages::{Block, CardItem, PageMetrics, RowVariant, Route};
use crate::shell::{Layout, Shell};
use crate::theme::size::*;
use crate::view_album::{album_head, album_info, album_meta_line, album_title};
use crate::view_rows::UnavailableRow;

const MINUTE: i64 = 60;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;

pub struct TimeLeft {
    pub out: bool,
    pub days_ago: i64,
    pub days: i64,
    pub hours: i64,
    pub minutes: i64,
    pub seconds: i64,
}

impl TimeLeft {
    pub fn of(album: &Upcoming) -> Self {
        let now = now_unix();
        let left = (album.release_moment - now).max(0);
        let days_ago = navia_core::upcoming::days_since_release(&album.release_date, local_day(now)).unwrap_or(0);
        TimeLeft { out: left == 0, days_ago, days: left / DAY, hours: left % DAY / HOUR, minutes: left % HOUR / MINUTE, seconds: left % MINUTE }
    }

    pub fn short(&self) -> String {
        if self.out {
            return tr("Out now").to_string();
        }
        let clock = format!("{:02}:{:02}:{:02}", self.hours, self.minutes, self.seconds);
        if self.days > 0 { trf("{days}d {clock}", &[("days", &self.days.to_string()), ("clock", &clock)]) } else { clock }
    }

    /// for lists shared with library albums.
    pub fn tag(&self) -> String {
        (if self.out { tr("Out now") } else { tr("Upcoming") }).to_string()
    }
}

pub fn release_line(album: &Upcoming, left: &TimeLeft, short: bool) -> String {
    let Some(day) = navia_core::smart_editor::preview::days_from_iso(&album.release_date) else { return String::new() };
    let date = if short {
        crate::i18n::fmt_date(day * DAY)
    } else {
        let (y, m, d) = crate::i18n::civil_from_days(day);
        crate::i18n::fmt_release(y, Some(m), Some(d))
    };
    let vars = [("date", date.as_str())];
    match (short, left.out) {
        (true, false) => trf("Releases {date}", &vars),
        (true, true) => trf("Released {date}", &vars),
        (false, false) => trf("Releases on {date}", &vars),
        (false, true) => trf("Released on {date}", &vars),
    }
}

impl Shell {
    /// an album past `OUT_NOW_DAYS` since the answer leaves at once.
    fn upcoming_cards(&self, list: Vec<Upcoming>, tag: bool) -> Vec<CardItem> {
        if self.deezer.choice != Some(true) {
            return Vec::new();
        }
        let today = local_day(now_unix());
        list.into_iter().filter(|u| navia_core::upcoming::in_window(&u.release_date, today)).map(|album| CardItem::Upcoming { album: Rc::new(album), tag }).collect()
    }

    pub fn upcoming_artist_cards(&self, artist_id: &str) -> Vec<CardItem> {
        if self.deezer.choice != Some(true) {
            return Vec::new();
        }
        let list = self.library.as_ref().map(|l| l.upcoming_for_artist(artist_id)).unwrap_or_default();
        self.upcoming_cards(list, false)
    }

    pub fn upcoming_home_cards(&self) -> Vec<CardItem> {
        if self.deezer.choice != Some(true) {
            return Vec::new();
        }
        let list = self.library.as_ref().map(|l| l.upcoming_home()).unwrap_or_default();
        self.upcoming_cards(list, false)
    }

    pub fn upcoming_search_cards(&self, query: &str) -> Vec<CardItem> {
        if self.deezer.choice != Some(true) {
            return Vec::new();
        }
        let list = self.library.as_ref().map(|l| l.upcoming_matching(query)).unwrap_or_default();
        self.upcoming_cards(list, true)
    }

    pub fn build_upcoming(&mut self, id: u64, layout: &Layout) -> Vec<Block> {
        self.upcoming_cover = None;
        if self.deezer.choice != Some(true) {
            return vec![Self::not_found(tr("This album isn't here"))];
        }
        let view = self.library.as_ref().map_or(UpcomingView::Missing, |l| l.upcoming_page(id));
        let (album, tracks) = match view {
            UpcomingView::Loading => return Vec::new(),
            UpcomingView::Missing => return vec![Self::not_found(tr("This album isn't here"))],
            // the album page takes this one's place in the history.
            UpcomingView::OnServer(album_id) if self.albums.iter().any(|a| a.id == album_id) => {
                self.route = Route::Album(album_id.clone());
                if let Some(entry) = self.history.get_mut(self.history_pos) {
                    *entry = self.route.clone();
                }
                return self.build_album(&album_id, layout);
            }
            UpcomingView::OnServer(_) => return Vec::new(),
            UpcomingView::Upcoming(album, tracks) => (album, tracks),
        };
        self.page_countdown = true;
        self.upcoming_cover = Some(album.cover.clone());
        let owned: Vec<u32> = tracks.iter().filter_map(|t| t.song).collect();
        self.core_preload_rows(&owned);
        self.page_numbers = Rc::new(tracks.iter().enumerate().filter(|(_, t)| t.song.is_some()).map(|(i, _)| Some(i as u32 + 1)).collect());
        let list = self.add_list(owned.clone());
        let head_album = album.clone();
        let mut blocks = vec![Shell::plain_block(move |this, layout, _, cx| this.render_upcoming_head(&head_album, layout, cx)), Block::Spacer(ALBUM_SECTION_GAP)];
        let listed = tracks.len();
        let note = [
            trf("{listed} of {total} songs announced so far", &[("listed", &listed.to_string()), ("total", &album.total.max(listed as u32).to_string())]),
            if owned.is_empty() { String::new() } else { trf("{count} in the library", &[("count", &owned.len().to_string())]) },
        ]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" • ");
        blocks.push(Self::section_heading_with_note(tr("Tracklist preview").into(), note.into()));
        blocks.push(Shell::plain_block(|this, layout, _, cx| if this.album_header_shown(layout) { this.render_album_header(layout, cx) } else { div().into_any_element() }));
        let mut pos = 0;
        let mut last_owned = false;
        for (i, track) in tracks.iter().enumerate() {
            let last = i + 1 == tracks.len();
            if track.song.is_some() {
                blocks.push(Block::Table { list, pos, variant: RowVariant::Album });
                pos += 1;
                last_owned = pos == owned.len();
                continue;
            }
            let row = unavailable_row(track, i, last_owned, last);
            last_owned = false;
            blocks.push(Shell::plain_block(move |this, layout, _, _| this.render_unavailable_row(row.clone(), layout)));
        }
        blocks
    }

    fn render_upcoming_head(&mut self, album: &Upcoming, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let m = PageMetrics::of(layout);
        let art = self.artwork(Some(&album.cover), "music-notes-fill", m.hero_art, crate::view_cards::ArtShape::Rounded(R_MD), layout);
        let tint = self.album_tint(&album.cover);
        let tint_fg: gpui::Hsla = crate::color::tint_fg(tint).hsla(1.0);
        let tint_dim: gpui::Hsla = crate::color::tint_fg_dim(tint).hsla(1.0);
        let left = TimeLeft::of(album);
        let kind = div().mb(t.s(COUNTDOWN_LABEL_GAP)).text_color(tint_dim).font_weight(FontWeight::SEMIBOLD).child(tr("Album"));
        let artist = self.album_artist_line(&album.artist, tint_fg, cx);
        let meta = album_meta_line(&t, tint_dim).child(release_line(album, &left, false));
        let info = album_info(&t).child(kind).child(album_title(&t, &m, album.title.clone(), album.explicit)).child(artist).child(meta).child(div().flex().child(countdown(&t, &left, tint_dim)));
        album_head(&t, &m, art, info, layout)
    }
}

fn unavailable_row(track: &UpcomingTrack, index: usize, rule_above: bool, last: bool) -> UnavailableRow {
    UnavailableRow { number: index as u32 + 1, title: track.title.clone(), artist: track.artist.clone(), explicit: track.explicit, duration_s: track.duration_s, rule_above, last }
}

fn countdown(t: &crate::theme::Theme, left: &TimeLeft, label_color: gpui::Hsla) -> AnyElement {
    let cell = |number: String, label: SharedString, first: bool| {
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(t.s(COUNTDOWN_LABEL_GAP))
            .when(!first, |d| d.border_l_1().border_color(t.countdown_divider()))
            .child(div().text_size(t.text(COUNTDOWN_NUMBER)).font_weight(FontWeight::SEMIBOLD).line_height(gpui::relative(1.0)).text_color(t.t_em()).whitespace_nowrap().font_features(crate::ui::tabular_figures()).child(number))
            .child(div().text_size(t.text(COUNTDOWN_LABEL)).font_weight(FontWeight::MEDIUM).text_color(label_color).child(label))
    };
    let panel = div().flex().flex_row().mt(t.s(COUNTDOWN_GAP)).py(t.s(COUNTDOWN_PAD_Y)).rounded(t.s(COUNTDOWN_R)).bg(t.countdown_fill()).shadow(crate::ui::glass_edge());
    if left.out {
        let ago: SharedString = if left.days_ago > 0 { trn("Released {count} day ago", "Released {count} days ago", left.days_ago as usize).into() } else { tr("Released today").into() };
        return panel.px(t.s(COUNTDOWN_PAD_X)).child(cell(tr("Out now").to_string(), ago, true)).into_any_element();
    }
    let parts = [(left.days, tr("Days")), (left.hours, tr("Hours")), (left.minutes, tr("Minutes")), (left.seconds, tr("Seconds"))];
    panel.children(parts.into_iter().enumerate().map(|(i, (n, label))| cell(format!("{n:02}"), label.into(), i == 0).w(t.s(COUNTDOWN_CELL_W)))).into_any_element()
}
