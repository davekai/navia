//! Get Info for a song, album, playlist or artist. Each row shows only when the server sent that field, and empty
//! sections are left out.

use gpui::prelude::*;
use gpui::{AnyElement, ClipboardItem, Context, FontWeight, MouseButton, ScrollHandle, SharedString, Window, div, px, relative};
use navia_core::library::info::{AlbumInfo, ArtistInfo, Credit, PlaylistInfo, ReplayGainInfo, SongInfo};

use crate::ui::FullTextTip;
use crate::focus::FocusStop;
use crate::i18n::{fmt_dec, fmt_int, fmt_release, tr, trf};
use crate::pages::Route;
use crate::shell::{Layout, ListId, Shell};
use crate::song_links::LinkTarget;
use crate::theme::size::*;
use crate::ui::icon;
use crate::ui_controls::{DialogButton, dialog_button};
use crate::view_cards::{ArtShape, long_duration};
use crate::view_downloads::file_size;
use crate::view_menu::MenuRow;

/// What a sheet describes, by server id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InfoTarget {
    Song(SharedString),
    Album(SharedString),
    Playlist(SharedString),
    Artist(SharedString),
}

/// The open sheet's item and scroll state.
pub struct InfoSheet {
    pub target: InfoTarget,
    scroll: ScrollHandle,
    /// Whether the About or Biography text is expanded.
    more: bool,
}

/// One row: its label, value, and the text its copy button copies.
struct Field {
    label: SharedString,
    value: AnyElement,
    copy: Option<String>,
}

// In the usual credits order. Unknown roles from the server follow in the server's order.
const ROLES: [(&str, &str, &str); 13] = [
    ("artist", "Artist", "Artists"),
    ("albumartist", "Album Artist", "Album Artists"),
    ("composer", "Composer", "Composers"),
    ("lyricist", "Lyricist", "Lyricists"),
    ("producer", "Producer", "Producers"),
    ("performer", "Performer", "Performers"),
    ("conductor", "Conductor", "Conductors"),
    ("arranger", "Arranger", "Arrangers"),
    ("director", "Director", "Directors"),
    ("engineer", "Engineer", "Engineers"),
    ("mixer", "Mixer", "Mixers"),
    ("remixer", "Remixer", "Remixers"),
    ("djmixer", "DJ Mixer", "DJ Mixers"),
];

/// How long a copy button shows its check.
const COPIED_FOR: std::time::Duration = std::time::Duration::from_millis(1500);

pub(crate) fn role_rank(role: &str) -> usize {
    ROLES.iter().position(|(r, _, _)| *r == role).unwrap_or(ROLES.len())
}

/// A role in words, plural when `n` > 1. Unknown roles show as sent, capitalized.
pub(crate) fn role_name(role: &str, n: usize) -> String {
    match ROLES.iter().find(|(r, _, _)| *r == role) {
        Some((_, one, many)) => tr(if n > 1 { many } else { one }).to_string(),
        None => capitalised(role),
    }
}

fn capitalised(s: &str) -> String {
    let mut c = s.chars();
    c.next().map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
}

fn signed(n: f64) -> String {
    let sign = if n > 0.0 { "+" } else if n < 0.0 { "−" } else { "" };
    format!("{sign}{}", fmt_dec(n.abs(), 2))
}

fn gain(g: Option<f64>, peak: Option<f64>) -> Option<String> {
    let g = g?;
    Some(match peak {
        Some(p) => trf("{gain} dB · peak {peak}", &[("gain", &signed(g)), ("peak", &fmt_dec(p, 3))]),
        None => trf("{gain} dB", &[("gain", &signed(g))]),
    })
}

/// An ISO 8601 date from the server, as a day.
fn day(iso: &str) -> Option<String> {
    navia_core::queue_sync::parse_iso8601_ms(iso).map(|ms| crate::i18n::fmt_date((ms / 1000) as i64))
}

/// The same with local time, for Created, Changed and Songs Up to Date Until.
fn moment(iso: &str) -> Option<String> {
    navia_core::queue_sync::parse_iso8601_ms(iso).map(|ms| crate::i18n::fmt_date_time((ms / 1000) as i64))
}

fn yes_no(on: bool) -> &'static str {
    if on { tr("Yes") } else { tr("No") }
}

fn release(d: &navia_core::client::ItemDate) -> Option<String> {
    let y = d.year.filter(|y| *y > 0)?;
    Some(fmt_release(y as i64, d.month.map(|m| m as u32), d.day.map(|d| d as u32)))
}

impl Shell {
    /// The "Get Info" menu row with its shortcut.
    pub fn info_row(&self, target: InfoTarget) -> MenuRow {
        MenuRow::item(tr("Get Info"), "info-regular", move |this, cx| this.open_info(target.clone(), cx)).with_shortcut("Ctrl+I")
    }

    pub fn open_info(&mut self, target: InfoTarget, cx: &mut Context<Self>) {
        if self.core.is_none() {
            return;
        }
        self.close_menu(cx);
        self.info = Some(InfoSheet { target, scroll: ScrollHandle::new(), more: false });
        cx.notify();
    }

    pub fn close_info(&mut self, cx: &mut Context<Self>) -> bool {
        let open = self.info.take().is_some();
        if open {
            cx.notify();
        }
        open
    }

    /// Ctrl+I. Closes an open sheet, or opens one for the selected song, the page's item, or the playing song.
    pub fn info_shortcut(&mut self, cx: &mut Context<Self>) {
        if self.close_info(cx) || self.about.is_some() || self.confirm.is_some() || self.new_playlist.is_some() || self.shares.dialog.is_some() || self.connect.is_some() {
            return;
        }
        let selected = self.selected_song();
        let target = selected
            .and_then(|s| self.song_peek(s))
            .map(|s| InfoTarget::Song(s.id))
            .or_else(|| match &self.route {
                Route::Album(id) => Some(InfoTarget::Album(id.clone())),
                Route::Playlist(id) => Some(InfoTarget::Playlist(id.clone())),
                Route::Artist(id) => Some(InfoTarget::Artist(id.clone())),
                _ => None,
            })
            .or_else(|| self.player.station.is_none().then(|| self.player.current.as_ref().map(|s| InfoTarget::Song(s.id.clone()))).flatten());
        if let Some(target) = target {
            self.open_info(target, cx);
        }
    }

    /// The first selected song of the list that holds the selection.
    fn selected_song(&self) -> Option<u32> {
        let songs: std::sync::Arc<Vec<u32>> = match self.selection_list {
            ListId::Page(list) => self.page.lists.get(list).cloned()?,
            ListId::Queue => self.queue_view_list().0,
            ListId::Songs => std::sync::Arc::new(self.order.clone()),
            ListId::Cached => std::sync::Arc::new(self.cached_view.library_rows()),
        };
        self.selection.first_in(&songs).and_then(|i| songs.get(i).copied())
    }

    pub fn render_info(&mut self, layout: &Layout, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let target = self.info.as_ref()?.target.clone();
        let scroll = self.info.as_ref()?.scroll.clone();
        let t = self.theme;
        let db = self.core.as_ref()?.lib.clone();
        let sub_ink: gpui::Hsla = t.t2().into();
        let (head, body): (AnyElement, Vec<AnyElement>) = match &target {
            InfoTarget::Song(id) => {
                let info = db.db.song_info(id).ok().flatten()?;
                let art = self.artwork(info.album_id.clone().map(SharedString::from).as_ref(), "music-notes-fill", INFO_ART, ArtShape::Rounded(R_MD), layout);
                let mut sub = div().flex().flex_row().flex_wrap().child(self.info_text_link(info.artist.clone().unwrap_or_default(), info.artist_id.clone().map(|a| LinkTarget::Artist(a.into())), sub_ink, cx));
                if let Some(album) = info.album.as_deref().map(|a| navia_core::display::song_album_name(info.album_id.as_deref(), a)) {
                    sub = sub.child(" — ").child(self.info_text_link(album, info.album_id.clone().map(|a| LinkTarget::Album(a.into())), sub_ink, cx));
                }
                (self.info_head(art, tr("Song"), info.title.clone(), Some(sub.into_any_element())), self.song_sections(&info, cx))
            }
            InfoTarget::Album(id) => {
                let info = db.db.album_info(id).ok().flatten()?;
                let art = self.artwork(Some(id), "music-notes-fill", INFO_ART, ArtShape::Rounded(R_MD), layout);
                let sub = self.info_text_link(info.artist.clone().unwrap_or_default(), info.artist_id.clone().map(|a| LinkTarget::Artist(a.into())), sub_ink, cx);
                let name = navia_core::display::album_name(&info.name, info.version.as_deref()).into_owned();
                (self.info_head(art, tr("Album"), name, Some(sub)), self.album_sections(&info, cx))
            }
            InfoTarget::Playlist(id) => {
                let info = db.db.playlist_info(id).ok().flatten()?;
                let key = self.playlist_art_key(id);
                let art = self.artwork(Some(&key), "playlist-regular", INFO_ART, ArtShape::Rounded(R_MD), layout);
                let kind = if info.smart { tr("Smart Playlist") } else { tr("Playlist") };
                let sub = info.owner.clone().map(|o| div().child(trf("By {name}", &[("name", &o)])).into_any_element());
                (self.info_head(art, kind, info.name.clone(), sub), self.playlist_sections(&info, cx))
            }
            InfoTarget::Artist(id) => {
                let info = db.db.artist_info(id).ok().flatten()?;
                let image = self.artists.iter().find(|a| &a.id == id).and_then(|a| a.image.clone());
                let art = self.artwork(image.as_ref(), "user-fill", INFO_ART, ArtShape::Circle, layout);
                (self.info_head(art, tr("Artist"), info.name.clone(), None), self.artist_sections(&info, cx))
            }
        };
        let done = dialog_button(&t, "info-done", DialogButton::Primary, true)
            .child(tr("Done"))
            .stop_top(&t)
            .on_click(cx.listener(|this, _, _, cx| {
                this.close_info(cx);
            }));
        let body = div()
            .id("info-body")
            .flex_1()
            .min_h(px(0.0))
            .mx(-t.s(DIALOG_PAD))
            .px(t.s(DIALOG_PAD))
            .pb(t.s(DIALOG_GAP))
            .overflow_y_scroll()
            .track_scroll(&scroll)
            .flex()
            .flex_col()
            .gap(t.s(INFO_SECTION_GAP))
            .children(body);
        Some(
            crate::ui::dialog_scrim(&t, "info-scrim")
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                    this.close_info(cx);
                }))
                .child(crate::ui::dialog_sheet(&t, "info", window.viewport_size().height).child(head).child(body).child(crate::ui::dialog_sheet_actions(&t).child(done)))
                .into_any_element(),
        )
    }

    /// The sheet's head: art, kind, title on up to three lines, and credit.
    fn info_head(&self, art: AnyElement, kind: &'static str, title: String, sub: Option<AnyElement>) -> AnyElement {
        let t = self.theme;
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(t.s(16.0))
            .min_w(px(0.0))
            .child(div().flex_none().size(t.s(INFO_ART)).child(art))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(t.s(2.0))
                    .min_w(px(0.0))
                    .child(div().text_size(t.text(TEXT_XS)).font_weight(FontWeight::SEMIBOLD).text_color(t.t_section()).child(kind))
                    .child(div().text_size(t.text(TEXT_FEATURE)).font_weight(FontWeight::BOLD).line_height(relative(1.2)).text_color(t.t_em()).line_clamp(3).text_ellipsis().id("info-title").child(title.clone()).full_text_tip(&t, title))
                    .when_some(sub, |d, s| d.child(div().text_color(t.t2()).line_height(relative(1.3)).child(s))),
            )
            .into_any_element()
    }

    /// Text that links to a page when there is one.
    fn info_text_link(&mut self, text: String, target: Option<LinkTarget>, color: gpui::Hsla, cx: &mut Context<Self>) -> AnyElement {
        self.info_links += 1;
        let key = self.info_links;
        div().child(self.link(("info-link", key), text.into(), target, color, cx)).into_any_element()
    }

    /// Comma-separated names, each a link when it has a page.
    fn info_link_list(&mut self, items: Vec<(String, Option<LinkTarget>)>, cx: &mut Context<Self>) -> AnyElement {
        let mut row = div().flex().flex_row().flex_wrap();
        for (i, (text, target)) in items.into_iter().enumerate() {
            if i > 0 {
                row = row.child(div().mr(self.theme.s(4.0)).child(","));
            }
            row = row.child(self.info_text_link(text, target, self.theme.t1().into(), cx));
        }
        row.into_any_element()
    }

    fn text(s: impl Into<SharedString>) -> AnyElement {
        div().child(s.into()).into_any_element()
    }

    fn code(&self, s: impl Into<SharedString>) -> AnyElement {
        div().text_size(self.theme.text(TEXT_SM)).child(s.into()).into_any_element()
    }

    /// A value with a quiet note under it.
    fn noted(&self, value: impl Into<SharedString>, note: &'static str) -> AnyElement {
        let t = self.theme;
        div().child(value.into()).child(div().mt(t.s(1.0)).text_size(t.text(TEXT_SM)).text_color(t.t2()).child(note)).into_any_element()
    }

    fn genre_links(&mut self, genres: &[String], cx: &mut Context<Self>) -> Option<Field> {
        if genres.is_empty() {
            return None;
        }
        let label = if genres.len() > 1 { tr("Genres") } else { tr("Genre") };
        let value = self.info_link_list(genres.iter().map(|g| (g.clone(), Some(LinkTarget::Genre(g.clone().into())))).collect(), cx);
        Some(Field { label: label.into(), value, copy: None })
    }

    fn song_sections(&mut self, s: &SongInfo, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut out = Vec::new();
        let album = s.album_id.as_deref().and_then(|id| self.core.as_ref()?.lib.db.album_info(id).ok().flatten());
        let mut song: Vec<Option<Field>> = vec![Some(Field { label: tr("Title").into(), value: Self::text(s.title.clone()), copy: None })];
        if !s.artists.is_empty() {
            let label = if s.artists.len() > 1 { tr("Artists") } else { tr("Artist") };
            let value = self.info_link_list(s.artists.iter().map(|a| (a.name.clone(), a.artist_id.clone().map(|i| LinkTarget::Artist(i.into())))).collect(), cx);
            song.push(Some(Field { label: label.into(), value, copy: None }));
        } else if let Some(artist) = s.artist.clone() {
            let value = self.info_link_list(vec![(artist, s.artist_id.clone().map(|i| LinkTarget::Artist(i.into())))], cx);
            song.push(Some(Field { label: tr("Artist").into(), value, copy: None }));
        }
        let mut credits: Vec<&Credit> = s.credits.iter().filter(|c| c.role != "maincredit").collect();
        credits.sort_by_key(|c| role_rank(&c.role));
        for c in credits {
            let t = self.theme;
            let mut row = div().flex().flex_row().flex_wrap();
            for (i, n) in c.names.iter().enumerate() {
                if i > 0 {
                    row = row.child(div().mr(t.s(4.0)).child(","));
                }
                row = row.child(self.info_text_link(n.name.clone(), n.artist_id.clone().map(|i| LinkTarget::Artist(i.into())), t.t1().into(), cx));
                if let Some(sub) = n.sub_role.clone() {
                    row = row.child(div().ml(t.s(4.0)).text_color(t.t2()).child(format!("({sub})")));
                }
            }
            song.push(Some(Field { label: role_name(&c.role, c.names.len()).into(), value: row.into_any_element(), copy: None }));
        }
        if let (Some(name), Some(id)) = (s.album.as_deref().map(|a| navia_core::display::song_album_name(s.album_id.as_deref(), a)), s.album_id.clone()) {
            let value = self.info_link_list(vec![(name, Some(LinkTarget::Album(id.into())))], cx);
            song.push(Some(Field { label: tr("Album").into(), value, copy: None }));
        }
        song.push(s.track.filter(|n| *n > 0).map(|n| Field { label: tr("Track").into(), value: Self::text(fmt_int(n as i64)), copy: None }));
        let discs = album.as_ref().and_then(|a| a.disc_count);
        // Only on multi-disc albums. With the disc count unknown, any disc past the first is shown.
        song.push(s.disc.filter(|d| *d > 0 && discs.map_or(*d > 1, |n| n > 1)).map(|d| {
            let value = match discs {
                Some(total) => trf("{n} of {total}", &[("n", &fmt_int(d as i64)), ("total", &fmt_int(total as i64))]),
                None => fmt_int(d as i64),
            };
            Field { label: tr("Disc").into(), value: Self::text(value), copy: None }
        }));
        let released = album.as_ref().and_then(|a| a.release_date.as_ref()).and_then(release);
        let original = album.as_ref().and_then(|a| a.original_release_date.as_ref()).and_then(release);
        song.push(released.clone().map(|r| Field { label: tr("Released").into(), value: Self::text(r), copy: None }));
        song.push(original.clone().filter(|o| Some(o) != released.as_ref()).map(|o| Field { label: tr("Original Release").into(), value: Self::text(o), copy: None }));
        if released.is_none() && original.is_none() {
            song.push(s.year.filter(|y| *y > 0).map(|y| Field { label: tr("Year").into(), value: Self::text(y.to_string()), copy: None }));
        }
        song.push(self.genre_links(&s.genres, cx));
        song.push((s.explicit_status.as_deref() == Some("explicit")).then(|| Field { label: tr("Explicit").into(), value: Self::text(tr("Yes")), copy: None }));
        out.extend(self.info_section(tr("Song"), song, None, cx));

        // File facts come from the shared quality logic, the same as the badge.
        let facts = s.suffix.clone().filter(|x| !x.trim().is_empty()).map(|suffix| navia_core::audio_quality::FileFacts { suffix, kbps: s.bit_rate_kbps, bit_depth: s.bit_depth, sample_rate: s.sampling_rate, channels: s.channel_count });
        if let Some(f) = facts {
            let format = f.format_name();
            let format = if format.to_lowercase() != f.suffix.to_lowercase() { format!("{format} ({})", f.suffix.to_uppercase()) } else { format };
            let file = vec![
                Some(Field { label: tr("Format").into(), value: Self::text(format), copy: None }),
                f.kbps.map(|k| Field { label: tr("Bitrate").into(), value: Self::text(trf("{n} kbps", &[("n", &fmt_int(k as i64))])), copy: None }),
                f.bit_depth.map(|b| Field { label: tr("Bit Depth").into(), value: Self::text(trf("{n}-bit", &[("n", &b.to_string())])), copy: None }),
                f.sample_rate.map(|r| Field { label: tr("Sample Rate").into(), value: Self::text(crate::quality_badge::khz(r)), copy: None }),
                f.channels.map(|c| Field { label: tr("Channels").into(), value: Self::text(crate::quality_badge::channels(c)), copy: None }),
                s.size.map(|b| Field { label: tr("Size").into(), value: Self::text(file_size(b)), copy: None }),
                s.duration_s.map(|d| Field { label: tr("Length").into(), value: Self::text(crate::ui::clock(d)), copy: None }),
                s.path.clone().map(|p| Field { label: tr("Path").into(), value: self.code(p.clone()), copy: Some(p) }),
            ];
            out.extend(self.info_section(tr("File"), file, None, cx));
        }

        // Only for the song playing now, from the badge's own data.
        let playing = self.player.station.is_none() && self.player.current.as_ref().is_some_and(|c| c.id.as_ref() == s.id);
        if playing && let Some(q) = self.quality_info() {
            let plays_as = match q.transcode() {
                Some((format, kbps)) => self.noted(kbps.map_or(format.clone(), |k| format!("{format} · {}", crate::quality_badge::kbps(k))), tr("Transcoded by the server")),
                None => Self::text(tr("The original file")),
            };
            let from = q.delivery.as_ref().map(|d| Field { label: tr("From||where a song plays from").into(), value: Self::text(crate::quality_badge::source_label(d.source)), copy: None });
            out.extend(self.info_section(tr("Playing Now"), vec![Some(Field { label: tr("Plays As").into(), value: plays_as, copy: None }), from], None, cx));
        }

        let rg = s.replay_gain.clone().unwrap_or(ReplayGainInfo { track_gain: None, album_gain: None, track_peak: None, album_peak: None });
        let isrc = (!s.isrc.is_empty()).then(|| s.isrc.join(", "));
        let details = vec![
            s.bpm.filter(|b| *b > 0).map(|b| Field { label: tr("BPM").into(), value: Self::text(fmt_int(b as i64)), copy: None }),
            gain(rg.track_gain, rg.track_peak).map(|g| Field { label: tr("Track Gain").into(), value: Self::text(g), copy: None }),
            gain(rg.album_gain, rg.album_peak).map(|g| Field { label: tr("Album Gain").into(), value: Self::text(g), copy: None }),
            isrc.map(|i| Field { label: "ISRC".into(), value: self.code(i.clone()), copy: Some(i) }),
            s.comment.clone().map(|c| Field { label: tr("Comment").into(), value: Self::text(c), copy: None }),
            s.music_brainz_id.clone().map(|m| Field { label: tr("MusicBrainz Recording").into(), value: self.code(m.clone()), copy: Some(m) }),
        ];
        out.extend(self.info_section(tr("Details"), details, None, cx));
        out.extend(self.listening(s.play_count, s.played.as_deref(), s.created.as_deref(), s.starred.is_some(), cx));
        out
    }

    /// "Your Listening", the signed-in user's own plays.
    fn listening(&mut self, plays: Option<u64>, played: Option<&str>, added: Option<&str>, favorite: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        let fields = vec![
            plays.map(|p| Field { label: tr("Plays").into(), value: Self::text(if p > 0 { fmt_int(p as i64) } else { tr("Never played").to_string() }), copy: None }),
            played.and_then(day).map(|d| Field { label: tr("Last Played").into(), value: Self::text(d), copy: None }),
            added.and_then(day).map(|d| Field { label: tr("Added").into(), value: Self::text(d), copy: None }),
            Some(Field { label: tr("Favorite").into(), value: Self::text(yes_no(favorite)), copy: None }),
        ];
        self.info_section(tr("Your Listening"), fields, None, cx)
    }

    fn album_sections(&mut self, a: &AlbumInfo, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut out = Vec::new();
        let released = a.release_date.as_ref().and_then(release);
        let original = a.original_release_date.as_ref().and_then(release);
        let mut album: Vec<Option<Field>> = Vec::new();
        if let Some(artist) = a.artist.clone() {
            let value = self.info_link_list(vec![(artist, a.artist_id.clone().map(|i| LinkTarget::Artist(i.into())))], cx);
            album.push(Some(Field { label: tr("Artist").into(), value, copy: None }));
        }
        let types: Vec<String> = a.release_types.iter().map(|r| capitalised(r)).collect();
        album.push((!types.is_empty()).then(|| Field { label: if types.len() > 1 { tr("Release Types") } else { tr("Release Type") }.into(), value: Self::text(types.join(", ")), copy: None }));
        album.push(navia_core::display::edition(a.version.as_deref()).map(|v| Field { label: tr("Version").into(), value: Self::text(v), copy: None }));
        album.push((!a.record_labels.is_empty()).then(|| Field { label: if a.record_labels.len() > 1 { tr("Record Labels") } else { tr("Record Label") }.into(), value: Self::text(a.record_labels.join(", ")), copy: None }));
        album.push(released.clone().map(|r| Field { label: tr("Released").into(), value: Self::text(r), copy: None }));
        album.push(original.clone().filter(|o| Some(o) != released.as_ref()).map(|o| Field { label: tr("Original Release").into(), value: Self::text(o), copy: None }));
        if released.is_none() && original.is_none() {
            album.push(a.year.filter(|y| *y > 0).map(|y| Field { label: tr("Year").into(), value: Self::text(y.to_string()), copy: None }));
        }
        album.push(self.genre_links(&a.genres, cx));
        album.push((a.explicit_status.as_deref() == Some("explicit")).then(|| Field { label: tr("Explicit").into(), value: Self::text(tr("Yes")), copy: None }));
        album.push((a.is_compilation == Some(true)).then(|| Field { label: tr("Compilation").into(), value: Self::text(tr("Yes")), copy: None }));
        out.extend(self.info_section(tr("Album"), album, None, cx));
        let contents = vec![
            a.song_count.map(|n| Field { label: tr("Songs").into(), value: Self::text(fmt_int(n as i64)), copy: None }),
            a.disc_count.filter(|d| *d > 1).map(|d| Field { label: tr("Discs").into(), value: Self::text(fmt_int(d as i64)), copy: None }),
            a.duration_s.filter(|d| *d > 0).map(|d| Field { label: tr("Length").into(), value: Self::text(long_duration(d as u32)), copy: None }),
            a.size.map(|b| Field { label: tr("Size").into(), value: Self::text(file_size(b)), copy: None }),
        ];
        out.extend(self.info_section(tr("Contents"), contents, None, cx));
        out.extend(self.listening(a.play_count, a.played.as_deref(), a.created.as_deref(), a.starred.is_some(), cx));
        let about = self.library.as_ref().map(|l| l.album_description(&a.id)).unwrap_or_default();
        if !about.is_empty() {
            let more = self.info_more(about, cx);
            out.extend(self.info_section(tr("About"), Vec::new(), Some(more), cx));
        }
        let ids = vec![a.music_brainz_id.clone().map(|m| Field { label: tr("MusicBrainz Release").into(), value: self.code(m.clone()), copy: Some(m) })];
        out.extend(self.info_section(tr("Identifiers"), ids, None, cx));
        out
    }

    fn playlist_sections(&mut self, p: &PlaylistInfo, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let t = self.theme;
        let mut out = Vec::new();
        let fields = vec![
            p.owner.clone().map(|o| Field { label: tr("Owner").into(), value: Self::text(o), copy: None }),
            p.public.map(|on| Field { label: tr("Visibility").into(), value: Self::text(if on { tr("Public") } else { tr("Private") }), copy: None }),
            p.created.as_deref().and_then(moment).map(|d| Field { label: tr("Created").into(), value: Self::text(d), copy: None }),
            p.changed.as_deref().and_then(moment).map(|d| Field { label: tr("Changed").into(), value: Self::text(d), copy: None }),
            p.song_count.map(|n| Field { label: tr("Songs").into(), value: Self::text(fmt_int(n as i64)), copy: None }),
            p.duration_s.filter(|d| *d > 0).map(|d| Field { label: tr("Length").into(), value: Self::text(long_duration(d as u32)), copy: None }),
            p.size.map(|b| Field { label: tr("Size").into(), value: Self::text(file_size(b)), copy: None }),
            p.comment.clone().filter(|c| !c.is_empty()).map(|c| Field { label: tr("Comment").into(), value: Self::text(c), copy: None }),
        ];
        out.extend(self.info_section(tr("Playlist"), fields, None, cx));
        if p.smart {
            // `validUntil`: the server reruns the rules the next time the playlist is read after this.
            let until = p.valid_until.as_deref().and_then(|v| {
                let ms = navia_core::queue_sync::parse_iso8601_ms(v)?;
                let text = crate::i18n::fmt_date_time((ms / 1000) as i64);
                let past = ms < std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
                Some(Field { label: tr("Songs Up to Date Until").into(), value: if past { self.noted(text, tr("The server updates them the next time the playlist is opened.")) } else { Self::text(text) }, copy: None })
            });
            let words = self.smart_words();
            let rules = self.playlists_full.iter().find(|x| x.id.as_ref() == p.id).and_then(|x| x.rules.clone());
            let summary = rules.as_deref().and_then(|raw| crate::smart_model::summarize_criteria(raw, crate::smart_model::Say(&words))).filter(|s| !s.rules.is_empty());
            let block = match summary {
                Some(s) => {
                    let lines: Vec<SharedString> = s.rules.iter().map(|r| SharedString::from(r.clone())).collect();
                    crate::view_menu::rule_list(&t, (!s.sentence.is_empty()).then(|| s.sentence.clone().into()), (!s.match_word.is_empty()).then(|| s.match_word.clone().into()), &lines, (!s.order.is_empty()).then(|| s.order.join(" · ").into())).into_any_element()
                }
                None => div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(t.s(5.0))
                    .text_color(t.t2())
                    .child(icon(&t, "lightning-fill", 13.0, t.accent_hsla()))
                    .child(tr("The rules come from the server and aren’t available here."))
                    .into_any_element(),
            };
            out.extend(self.info_section(tr("Smart Playlist"), vec![until], Some(block), cx));
        }
        out
    }

    fn artist_sections(&mut self, a: &ArtistInfo, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut out = Vec::new();
        let mut roles: Vec<&String> = a.roles.iter().filter(|r| r.as_str() != "maincredit").collect();
        roles.sort_by_key(|r| role_rank(r));
        let roles: Vec<String> = roles.into_iter().map(|r| role_name(r, 1)).collect();
        let fields = vec![
            a.album_count.map(|n| Field { label: tr("Albums").into(), value: Self::text(fmt_int(n as i64)), copy: None }),
            (a.song_count > 0).then(|| Field { label: tr("Songs").into(), value: Self::text(fmt_int(a.song_count as i64)), copy: None }),
            (!roles.is_empty()).then(|| Field { label: if roles.len() > 1 { tr("Roles") } else { tr("Role") }.into(), value: Self::text(roles.join(", ")), copy: None }),
            (a.play_count > 0).then(|| Field { label: tr("Plays").into(), value: Self::text(fmt_int(a.play_count as i64)), copy: None }),
            Some(Field { label: tr("Favorite").into(), value: Self::text(yes_no(a.starred.is_some())), copy: None }),
            a.music_brainz_id.clone().map(|m| Field { label: tr("MusicBrainz Artist").into(), value: self.code(m.clone()), copy: Some(m) }),
        ];
        out.extend(self.info_section(tr("Artist"), fields, None, cx));
        // Similar artists come from the server's artist info. The biography stays in the About sheet.
        let extra = self.library.as_ref().map(|l| l.artist_info(&a.id));
        if let Some(extra) = extra {
            if !extra.similar.is_empty() {
                let items = extra.similar.iter().map(|name| (name.to_string(), self.artist_named(name).map(|x| LinkTarget::Artist(x.id.clone())))).collect();
                let list = self.info_link_list(items, cx);
                out.extend(self.info_section(tr("Similar Artists"), Vec::new(), Some(list), cx));
            }
        }
        out
    }

    /// A long paragraph clamped to three lines until MORE opens it.
    fn info_more(&mut self, text: SharedString, cx: &mut Context<Self>) -> AnyElement {
        let open = self.info.as_ref().is_some_and(|i| i.more);
        self.more_text("info-more", text, open, true, cx, |this, cx| {
            if let Some(info) = this.info.as_mut() {
                info.more = !info.more;
            }
            cx.notify();
        })
    }

    /// A titled group of rows with an optional padded block under them. None when there's nothing to show.
    fn info_section(&mut self, title: &'static str, fields: Vec<Option<Field>>, block: Option<AnyElement>, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = self.theme;
        let fields: Vec<Field> = fields.into_iter().flatten().collect();
        if fields.is_empty() && block.is_none() {
            return None;
        }
        let has_rows = !fields.is_empty();
        let mut group = div().rounded(t.s(R_MD)).bg(t.row()).overflow_hidden().flex().flex_col();
        for (i, f) in fields.into_iter().enumerate() {
            let copy = f.copy.map(|text| self.copy_button(text, f.label.clone(), cx));
            group = group.child(
                div()
                    .flex()
                    .flex_row()
                    .gap(t.s(14.0))
                    .px(t.s(INFO_ROW_PAD_X))
                    .py(t.s(INFO_ROW_PAD_Y))
                    .line_height(relative(1.35))
                    .when(i > 0, |d| d.border_t_1().border_color(t.line()))
                    .child(div().flex_none().w(t.s(INFO_LABEL_W)).text_color(t.t2()).child(f.label))
                    .child(div().flex_1().min_w(px(0.0)).flex().flex_row().items_start().gap(t.s(8.0)).text_color(t.t1()).child(div().flex_1().min_w(px(0.0)).child(f.value)).children(copy)),
            );
        }
        if let Some(block) = block {
            group = group.child(div().px(t.s(INFO_ROW_PAD_X)).py(t.s(INFO_ROW_PAD_Y)).when(has_rows, |d| d.border_t_1().border_color(t.line())).child(block));
        }
        Some(
            div()
                .flex()
                .flex_col()
                .child(div().ml(t.s(INFO_ROW_PAD_X)).mb(t.s(7.0)).text_size(t.text(TEXT_SM)).font_weight(FontWeight::SEMIBOLD).text_color(t.t_section()).child(title))
                .child(group)
                .into_any_element(),
        )
    }

    /// The copy button at a value's end. It shows a check for a moment after copying.
    fn copy_button(&mut self, text: String, label: SharedString, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        self.info_links += 1;
        let done = self.info_copied.as_ref().is_some_and(|(l, at)| *l == label && at.elapsed() < COPIED_FOR);
        div()
            .id(("info-copy", self.info_links))
            .flex_none()
            .size(t.s(26.0))
            .my(-t.s(3.0))
            .mr(-t.s(6.0))
            .flex()
            .items_center()
            .justify_center()
            .child(crate::ui::hover_fill(crate::ui::HoverShape::Rounded(t.s(R_XS)), t.hover(), t.press()))
            .child(icon(&t, if done { "check-bold" } else { "copy-regular" }, 16.0, t.t2()))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                this.info_copied = Some((label.clone(), std::time::Instant::now()));
                this.show_toast(trf("Copied {name}", &[("name", &label)]), cx);
                // Switch the check back to the copy icon.
                cx.spawn(async move |this: gpui::WeakEntity<Shell>, cx: &mut gpui::AsyncApp| {
                    cx.background_executor().timer(COPIED_FOR).await;
                    let _ = this.update(cx, |_, cx| cx.notify());
                })
                .detach();
            }))
            .into_any_element()
    }
}
