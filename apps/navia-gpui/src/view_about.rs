//! An artist's About sheet, opened from the (i) in the artist header. Every part hides when the server has no data for
//! it.

use gpui::prelude::*;
use std::ops::Range;

use gpui::{AnyElement, Context, FontWeight, HighlightStyle, InteractiveText, MouseButton, ScrollHandle, SharedString, StyledText, UnderlineStyle, Window, div, px, relative};

use crate::art::BakeKind;
use crate::focus::FocusStop;
use crate::i18n::{fmt_int, tr};
use crate::shell::{Layout, Shell};
use crate::theme::size::*;
use crate::ui_controls::{round_button, small_glass_action};

/// How many of the artist's most common album genres the sheet shows.
const GENRES_SHOWN: usize = 3;

/// what the sheet is about: an artist's facts and biography, or a page's description.
pub enum AboutOf {
    Artist(SharedString),
    Text(AboutText),
}

/// a header's whole description; `art` is a bake key (`bake_source`).
#[derive(Clone)]
pub struct AboutText {
    pub title: SharedString,
    pub subtitle: Option<SharedString>,
    pub art: Option<SharedString>,
    pub text: SharedString,
}

/// the open sheet: what it shows, its body's scroll and the link under the pointer.
pub struct AboutSheet {
    of: AboutOf,
    scroll: ScrollHandle,
    hovered_link: Option<usize>,
}

pub struct ArtistAbout {
    facts: Vec<(&'static str, String)>,
    genres: Vec<SharedString>,
    bio: SharedString,
}

impl Shell {
    /// The artist's About, or None when the server has nothing for it.
    pub fn artist_about(&mut self, id: &SharedString) -> Option<ArtistAbout> {
        let info = self.core.as_ref().and_then(|c| c.lib.db.artist_info(id).ok().flatten());
        let mut facts = Vec::new();
        if let Some(info) = info {
            if let Some(n) = info.album_count.filter(|n| *n > 0) {
                facts.push((tr("Albums"), fmt_int(n as i64)));
            }
            if info.song_count > 0 {
                facts.push((tr("Songs"), fmt_int(info.song_count as i64)));
            }
            let mut roles: Vec<&String> = info.roles.iter().filter(|r| r.as_str() != "maincredit").collect();
            roles.sort_by_key(|r| crate::view_info::role_rank(r));
            if !roles.is_empty() {
                let names: Vec<String> = roles.iter().map(|r| crate::view_info::role_name(r, 1)).collect();
                facts.push((if roles.len() > 1 { tr("Roles") } else { tr("Role") }, names.join(", ")));
            }
            if info.play_count > 0 {
                facts.push((tr("Plays"), fmt_int(info.play_count as i64)));
            }
        }
        let mut count: Vec<(SharedString, usize)> = Vec::new();
        for i in self.artist_albums(id) {
            let Some(genre) = self.albums.get(i as usize).map(|a| a.genre.trim().to_string()).filter(|g| !g.is_empty()) else { continue };
            match count.iter_mut().find(|(g, _)| g.as_ref() == genre) {
                Some((_, n)) => *n += 1,
                None => count.push((genre.into(), 1)),
            }
        }
        // Most common first. The stable sort keeps album order among equals.
        count.sort_by(|a, b| b.1.cmp(&a.1));
        let genres = count.into_iter().take(GENRES_SHOWN).map(|(g, _)| g).collect::<Vec<_>>();
        let bio = self.library.as_ref().map(|l| l.artist_info(id).biography).unwrap_or_default();
        (!facts.is_empty() || !genres.is_empty() || !bio.is_empty()).then_some(ArtistAbout { facts, genres, bio })
    }

    pub fn open_artist_about(&mut self, artist: SharedString, cx: &mut Context<Self>) {
        self.open_about(AboutOf::Artist(artist), cx);
    }

    /// a header's MORE: the whole description.
    pub fn open_about_text(&mut self, about: AboutText, cx: &mut Context<Self>) {
        self.open_about(AboutOf::Text(about), cx);
    }

    fn open_about(&mut self, of: AboutOf, cx: &mut Context<Self>) {
        self.close_menu(cx);
        self.about = Some(AboutSheet { of, scroll: ScrollHandle::new(), hovered_link: None });
        cx.notify();
    }

    pub fn close_about(&mut self, cx: &mut Context<Self>) -> bool {
        let open = self.about.take().is_some();
        if open {
            cx.notify();
        }
        open
    }

    fn hover_about_link(&mut self, link: Option<usize>, cx: &mut Context<Self>) {
        if let Some(sheet) = self.about.as_mut().filter(|s| s.hovered_link != link) {
            sheet.hovered_link = link;
            cx.notify();
        }
    }

    /// the sheet's close button sits on the same side as the window's close button.
    fn close_on_left(&self) -> bool {
        self.button_layout.left.contains(&Some(gpui::WindowButton::Close))
    }

    /// an artist's facts, genres and biography under its name.
    fn artist_about_parts(&mut self, id: &SharedString, hovered: Option<usize>, cx: &mut Context<Self>) -> Option<(AboutText, Vec<AnyElement>)> {
        let artist = self.artists.iter().find(|a| &a.id == id).cloned()?;
        let about = self.artist_about(id).unwrap_or(ArtistAbout { facts: Vec::new(), genres: Vec::new(), bio: SharedString::default() });
        let t = self.theme;
        let mut parts = Vec::new();
        if !about.facts.is_empty() {
            let mut grid = div().flex().flex_col().gap(t.s(DIALOG_GAP));
            for pair in about.facts.chunks(2) {
                let mut row = div().flex().flex_row().gap(t.s(DIALOG_PAD));
                for (label, value) in pair {
                    row = row.child(div().flex_1().min_w(px(0.0)).child(div().text_size(t.text(TEXT_SM)).text_color(t.t2()).child(*label)).child(div().mt(t.s(2.0)).text_color(t.t1()).child(value.clone())));
                }
                if pair.len() == 1 {
                    row = row.child(div().flex_1());
                }
                grid = grid.child(row);
            }
            parts.push(grid.into_any_element());
        }
        if !about.genres.is_empty() {
            let mut chips = div().flex().flex_row().flex_wrap().gap(t.s(DIALOG_ACTIONS_GAP));
            for (i, genre) in about.genres.iter().enumerate() {
                let g = genre.clone();
                chips = chips.child(small_glass_action(&t, ("about-genre", i)).child(genre.clone()).stop(&t).on_click(cx.listener(move |this, _, _, cx| {
                    this.close_about(cx);
                    this.open_genre(g.clone(), cx);
                })));
            }
            let label = if about.genres.len() > 1 { tr("Genres") } else { tr("Genre") };
            parts.push(div().flex().flex_col().gap(t.s(SHEET_LABEL_GAP)).child(div().text_size(t.text(TEXT_SM)).text_color(t.t2()).child(label)).child(chips).into_any_element());
        }
        if !about.bio.is_empty() {
            let text = self.about_text(about.bio.clone(), hovered, cx);
            parts.push(div().flex().flex_col().gap(t.s(SHEET_LABEL_GAP)).child(div().text_size(t.text(TEXT_SECTION)).font_weight(FontWeight::SEMIBOLD).text_color(t.t1()).child(tr("About"))).child(text).into_any_element());
        }
        let head = AboutText { title: artist.name.clone(), subtitle: None, art: artist.image.clone().filter(|i| !i.is_empty()), text: SharedString::default() };
        Some((head, parts))
    }

    /// running text that keeps its line breaks; web addresses open in the browser.
    fn about_text(&mut self, text: SharedString, hovered: Option<usize>, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let text: SharedString = if text.contains('\r') { text.replace("\r\n", "\n").replace('\r', "\n").into() } else { text };
        let links = web_links(&text);
        let ink = t.link_ink();
        let highlights = links.iter().enumerate().map(|(i, range)| {
            let underline = (hovered == Some(i)).then_some(UnderlineStyle { thickness: px(LINK_UNDERLINE), color: Some(ink), wavy: false });
            (range.clone(), HighlightStyle { color: Some(ink), underline, ..Default::default() })
        });
        let styled = StyledText::new(text.clone()).with_highlights(highlights);
        let urls: Vec<String> = links.iter().map(|r| text[r.clone()].to_string()).collect();
        let shell = cx.entity().downgrade();
        let hover_ranges = links.clone();
        let interactive = InteractiveText::new("about-text-links", styled)
            .on_click(links, move |ix, _, cx| {
                if let Some(url) = urls.get(ix) {
                    cx.open_url(url);
                }
            })
            .on_hover(move |index, _, _, cx| {
                let link = index.and_then(|i| hover_ranges.iter().position(|r| r.contains(&i)));
                shell.update(cx, |this, cx| this.hover_about_link(link, cx)).ok();
            });
        div()
            .id("about-text")
            .line_height(relative(1.55))
            .text_color(t.t1())
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !*hovered {
                    this.hover_about_link(None, cx);
                }
            }))
            .child(interactive)
            .into_any_element()
    }

    pub fn render_about(&mut self, layout: &Layout, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let sheet = self.about.as_ref()?;
        let (scroll, hovered) = (sheet.scroll.clone(), sheet.hovered_link);
        let (head, parts) = match &sheet.of {
            AboutOf::Artist(id) => {
                let id = id.clone();
                self.artist_about_parts(&id, hovered, cx)?
            }
            AboutOf::Text(about) => {
                let about = about.clone();
                let text = self.about_text(about.text.clone(), hovered, cx);
                (about, vec![text])
            }
        };
        let t = self.theme;
        let device = |tokens: f32| (f32::from(t.s(tokens)) * layout.scale).round() as u32;
        let cover = head.art.as_ref().and_then(|key| self.bake_for(BakeKind::CoverFade, key, device(SHEET_W), device(SHEET_COVER_H), cx));
        let has_cover = head.art.is_some();
        let left = self.close_on_left();
        // without a picture the name starts the sheet, beside the close button and clear of it.
        let clear = DIALOG_GAP * 2.0 + CONTROL_H - DIALOG_PAD;
        let title = div()
            .text_size(t.text(TEXT_TITLE))
            .font_weight(FontWeight::BOLD)
            .line_height(relative(1.1))
            .text_color(t.t_em())
            .when(!has_cover, |d| d.flex().items_center().min_h(t.s(CONTROL_H)).when(left, |d| d.pl(t.s(clear))).when(!left, |d| d.pr(t.s(clear))))
            .child(head.title.clone());
        let titles = div()
            .flex()
            .flex_col()
            .gap(t.s(SHEET_LABEL_GAP))
            .child(title)
            .when_some(head.subtitle.clone(), |d, subtitle| d.child(div().text_size(t.text(TEXT_MD)).font_weight(FontWeight::SEMIBOLD).text_color(t.t2()).child(subtitle)));
        let content = div().relative().flex().flex_col().gap(t.s(SHEET_SECTION_GAP)).child(titles).children(parts);
        let body = div()
            .id("about-body")
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .track_scroll(&scroll)
            .px(t.s(DIALOG_PAD))
            .pb(t.s(DIALOG_PAD))
            .when(!has_cover, |d| d.pt(t.s(DIALOG_GAP)))
            .when(has_cover, |d| d.child(crate::ui::dialog_cover_picture(&t, cover)))
            .child(content);
        let close = round_button(&t, "about-close", tr("Close"), "x-regular")
            .absolute()
            .top(t.s(DIALOG_GAP))
            .when(left, |d| d.left(t.s(DIALOG_GAP)))
            .when(!left, |d| d.right(t.s(DIALOG_GAP)))
            .stop_top(&t)
            .on_click(cx.listener(|this, _, _, cx| {
                this.close_about(cx);
            }));
        Some(
            crate::ui::dialog_scrim(&t, "about-scrim")
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                    this.close_about(cx);
                }))
                .child(crate::ui::dialog_cover(&t, "about", window.viewport_size().height).child(body).child(close))
                .into_any_element(),
        )
    }
}

/// web addresses in `text`; trailing punctuation usually ends the sentence, not the address.
pub fn web_links(text: &str) -> Vec<Range<usize>> {
    const STOP: &[char] = &['<', '>', '"'];
    const TRAILING: &[char] = &['.', ',', ';', ':', '!', '?', ')', ']', '\'', '\u{2019}', '\u{201d}', '"'];
    let mut links = Vec::new();
    let mut from = 0;
    while let Some(found) = text[from..].find("http") {
        let start = from + found;
        let rest = &text[start..];
        let scheme = ["https://", "http://"].into_iter().find(|s| rest.starts_with(s)).map(str::len);
        let Some(scheme) = scheme else {
            from = start + "http".len();
            continue;
        };
        let len = rest.find(|c: char| c.is_whitespace() || STOP.contains(&c)).unwrap_or(rest.len());
        let address = rest[..len].trim_end_matches(TRAILING);
        if address.len() > scheme + 1 {
            links.push(start..start + address.len());
        }
        from = start + len.max(scheme);
    }
    links
}

#[cfg(test)]
mod tests {
    use super::web_links;

    fn found(text: &str) -> Vec<&str> {
        web_links(text).into_iter().map(|r| &text[r]).collect()
    }

    #[test]
    fn finds_web_addresses_without_the_sentences_punctuation() {
        assert_eq!(found("see https://example.org/a/b. then"), ["https://example.org/a/b"]);
        assert_eq!(found("(http://x.io/p?q=1), and \u{201c}https://y.io\u{201d}"), ["http://x.io/p?q=1", "https://y.io"]);
        assert_eq!(found("line\nhttps://a.b/c/\nnext"), ["https://a.b/c/"]);
        assert!(found("http only, https:// nothing, ftp://no").is_empty());
    }
}
