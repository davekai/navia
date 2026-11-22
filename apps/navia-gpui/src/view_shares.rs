//! Share links: Share… in item menus, its dialog, and the Share Links page with Copy, Edit and Delete. The server work
//! is in `navia_core::shares`.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, ClipboardItem, Context, FontWeight, SharedString, div, px, relative};
use navia_core::shares::{DAY_MS, EXPIRY_PRESETS, ShareChoice, ShareKind, ShareLink, ShareTarget};

use crate::focus::FocusStop;
use crate::i18n::{fmt_date, tr, trf, trn};
use crate::pages::{Block, Route};
use crate::shell::{Layout, Shell};
use crate::theme::size::*;
use crate::ui::icon;
use crate::ui_controls::{DialogButton, SelectOption, TextTarget, dialog_button, switch};
use crate::view_cards::ArtShape;
use crate::view_menu::MenuRow;

/// What a share holds, for its dialogs and row: a title, its kind and artist or album, and its cover.
#[derive(Clone)]
pub struct ShareSubject {
    pub title: SharedString,
    pub kind: &'static str,
    pub detail: SharedString,
    /// The cover's art key. A playlist's comes from `Shell::playlist_art_key`.
    pub art: Option<SharedString>,
    pub shape: ArtShape,
}

/// When a link expires, as the dialogs offer it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Expiry {
    /// The server's default on a new link, or the current date on an existing one.
    First,
    /// One of `EXPIRY_PRESETS`.
    Preset(usize),
    /// YYYY-MM-DD.
    Custom,
}

pub struct ShareDialog {
    /// The link being edited, or None for Share….
    pub edit: Option<ShareLink>,
    pub target: ShareTarget,
    pub subject: ShareSubject,
    pub description: String,
    pub expiry: Expiry,
    pub custom: String,
    pub downloadable: bool,
    pub busy: bool,
    pub error: Option<SharedString>,
    /// The link just created. The dialog shows it with Copy Link and Done.
    pub made: Option<ShareLink>,
}

#[derive(Default)]
pub struct SharesState {
    /// The user's links, once loaded.
    pub links: Option<Vec<ShareLink>>,
    pub error: Option<SharedString>,
    pub dialog: Option<ShareDialog>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn day(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    fmt_date(secs + navia_core::library::taste::utc_offset(secs))
}

/// A date typed as YYYY-MM-DD, as the end of that local day.
fn typed_day(text: &str) -> Option<u64> {
    let mut parts = text.trim().splitn(3, '-');
    let (y, m, d) = (parts.next()?.parse().ok()?, parts.next()?.parse().ok()?, parts.next()?.parse().ok()?);
    let now = (now_ms() / 1000) as i64;
    navia_core::shares::end_of_day_ms(y, m, d, navia_core::library::taste::utc_offset(now))
}

/// YYYY-MM-DD, for the custom date's starting value.
fn day_text(days: u64) -> String {
    let secs = ((now_ms() + days * DAY_MS) / 1000) as i64;
    let local = secs + navia_core::library::taste::utc_offset(secs);
    let (y, m, d) = crate::i18n::civil_from_days(local.div_euclid(86_400));
    format!("{y:04}-{m:02}-{d:02}")
}

impl ShareDialog {
    /// The chosen expiry. None means the server's default, or the current one when editing.
    fn expires(&self) -> Option<u64> {
        match self.expiry {
            Expiry::First => None,
            Expiry::Preset(i) => Some(now_ms() + EXPIRY_PRESETS[i].1 * DAY_MS),
            Expiry::Custom => typed_day(&self.custom),
        }
    }

    /// missing, unreadable or not after today.
    fn invalid(&self) -> bool {
        self.expiry == Expiry::Custom && self.expires().is_none_or(|ms| ms < now_ms())
    }

    fn choice(&self) -> ShareChoice {
        ShareChoice { description: self.description.trim().to_string(), expires_ms: self.expires(), downloadable: self.downloadable }
    }
}

impl Shell {
    /// Whether the server lets this user share, judged by getShares answering.
    pub fn sharing_on(&self) -> bool {
        self.library.as_ref().is_some_and(|l| l.can_share())
    }

    fn share_downloads_on(&self) -> bool {
        self.core.as_ref().is_some_and(|c| c.lib.share_downloads())
    }

    /// "Share…" (or `label`) for `target`. Disabled with the reason when sharing is off on the server.
    pub fn share_row_as(&self, label: impl Into<SharedString>, target: ShareTarget, subject: ShareSubject) -> MenuRow {
        if !self.sharing_on() || self.core.is_none() {
            return MenuRow::disabled(label, "share-network-regular", tr("Sharing is turned off on this server"));
        }
        MenuRow::item(label, "share-network-regular", move |this, cx| this.open_share(target.clone(), subject.clone(), cx))
    }

    pub fn share_row(&self, kind: ShareKind, id: SharedString) -> MenuRow {
        let target = ShareTarget { kind, ids: vec![id.to_string()] };
        let subject = self.share_subject(&target, &[]);
        self.share_row_as(tr("Share\u{2026}"), target, subject)
    }

    /// "Share {n} Songs…" for several.
    pub fn share_songs_row(&self, songs: &[u32]) -> MenuRow {
        let ids: Vec<String> = songs.iter().filter_map(|s| self.song_peek(*s)).map(|s| s.id.to_string()).collect();
        let label: SharedString = if songs.len() > 1 { trf("Share {n} Songs\u{2026}", &[("n", &crate::i18n::fmt_int(songs.len() as i64))]).into() } else { tr("Share\u{2026}").into() };
        let subject = self.song_subject(songs);
        self.share_row_as(label, ShareTarget { kind: ShareKind::Song, ids }, subject)
    }

    fn song_subject(&self, songs: &[u32]) -> ShareSubject {
        let list: Vec<_> = songs.iter().filter_map(|s| self.song_peek(*s)).collect();
        let first = list.first();
        let art = first.map(|s| s.album_id.clone()).filter(|a| !a.is_empty());
        if list.len() > 1 {
            let one_album = list.iter().all(|s| Some(&s.album) == first.map(|f| &f.album));
            let one_artist = list.iter().all(|s| Some(&s.artist) == first.map(|f| &f.artist));
            let detail = first.map(|f| if one_album { f.album.clone() } else if one_artist { f.artist.clone() } else { SharedString::default() }).unwrap_or_default();
            return ShareSubject { title: trn("{count} Song", "{count} Songs", list.len()).into(), kind: tr("Songs"), detail, art, shape: ArtShape::Rounded(R_SM) };
        }
        ShareSubject { title: first.map(|s| s.title.clone()).unwrap_or_default(), kind: tr("Song"), detail: first.map(|s| s.artist.clone()).unwrap_or_default(), art, shape: ArtShape::Rounded(R_SM) }
    }

    /// What a share holds, from this library or from the server's entries for songs and albums it doesn't know.
    pub fn share_subject(&self, target: &ShareTarget, entries: &[navia_core::client::ShareEntry]) -> ShareSubject {
        let rounded = ArtShape::Rounded(R_SM);
        let first_id = target.ids.first().map(String::as_str).unwrap_or_default();
        match target.kind {
            ShareKind::Album => {
                let album = self.albums.iter().find(|a| a.id.as_ref() == first_id);
                let entry = entries.first();
                let title: SharedString = if target.ids.len() > 1 {
                    trn("{count} Album", "{count} Albums", target.ids.len()).into()
                } else {
                    album.map(|a| a.name.clone()).or_else(|| entry.and_then(|e| e.title.clone()).map(Into::into)).unwrap_or_default()
                };
                let detail = album.map(|a| a.artist.clone()).or_else(|| entry.and_then(|e| e.artist.clone()).map(Into::into)).unwrap_or_default();
                ShareSubject { title, kind: tr("Album"), detail, art: Some(first_id.to_string().into()), shape: rounded }
            }
            ShareKind::Playlist => {
                let name = self.playlists_full.iter().find(|p| p.id.as_ref() == first_id).map(|p| p.name.clone()).unwrap_or_default();
                ShareSubject { title: name, kind: tr("Playlist"), detail: SharedString::default(), art: Some(self.playlist_art_key(first_id)), shape: rounded }
            }
            ShareKind::Artist => {
                let artist = self.artists.iter().find(|a| a.id.as_ref() == first_id);
                ShareSubject { title: artist.map(|a| a.name.clone()).unwrap_or_default(), kind: tr("Artist"), detail: SharedString::default(), art: artist.and_then(|a| a.image.clone()), shape: ArtShape::Circle }
            }
            ShareKind::Song => {
                let first = entries.first();
                let art = first.and_then(|e| e.album_id.clone()).map(Into::into);
                if target.ids.len().max(entries.len()) > 1 {
                    let n = target.ids.len().max(entries.len());
                    let one_album = entries.iter().all(|e| e.album == first.and_then(|f| f.album.clone()));
                    let one_artist = entries.iter().all(|e| e.artist == first.and_then(|f| f.artist.clone()));
                    let detail = first.and_then(|f| if one_album { f.album.clone() } else if one_artist { f.artist.clone() } else { None }).unwrap_or_default();
                    return ShareSubject { title: trn("{count} Song", "{count} Songs", n).into(), kind: tr("Songs"), detail: detail.into(), art, shape: rounded };
                }
                ShareSubject {
                    title: first.and_then(|e| e.title.clone()).unwrap_or_default().into(),
                    kind: tr("Song"),
                    detail: first.and_then(|e| e.artist.clone()).unwrap_or_default().into(),
                    art,
                    shape: rounded,
                }
            }
        }
    }

    pub fn open_share(&mut self, target: ShareTarget, subject: ShareSubject, cx: &mut Context<Self>) {
        self.shares.dialog = Some(ShareDialog { edit: None, target, subject, description: String::new(), expiry: Expiry::First, custom: day_text(30), downloadable: false, busy: false, error: None, made: None });
        self.focus_field(TextTarget::ShareDescription, cx);
        cx.notify();
    }

    pub fn open_share_edit(&mut self, link: ShareLink, cx: &mut Context<Self>) {
        let subject = self.share_subject(&link.target, &link.entries);
        let description = if navia_core::shares::is_server_label(&link.description, &subject.title) { String::new() } else { link.description.clone() };
        let downloadable = link.downloadable.unwrap_or(false);
        self.shares.dialog = Some(ShareDialog { target: link.target.clone(), edit: Some(link), subject, description, expiry: Expiry::First, custom: day_text(30), downloadable, busy: false, error: None, made: None });
        self.focus_field(TextTarget::ShareDescription, cx);
        cx.notify();
    }

    pub fn close_share_dialog(&mut self, cx: &mut Context<Self>) -> bool {
        if self.shares.dialog.take().is_none() {
            return false;
        }
        self.controls.text = None;
        cx.notify();
        true
    }

    pub fn share_text(&self, target: &TextTarget) -> String {
        let Some(dialog) = self.shares.dialog.as_ref() else { return String::new() };
        if *target == TextTarget::ShareDate { dialog.custom.clone() } else { dialog.description.clone() }
    }

    pub fn set_share_text(&mut self, target: &TextTarget, value: String, cx: &mut Context<Self>) {
        if let Some(dialog) = self.shares.dialog.as_mut() {
            if *target == TextTarget::ShareDate {
                dialog.custom = value.chars().filter(|c| c.is_ascii_digit() || *c == '-').take(10).collect();
            } else {
                dialog.description = value;
            }
            dialog.error = None;
            cx.notify();
        }
    }

    /// or Save while editing.
    pub fn share_submit(&mut self, cx: &mut Context<Self>) {
        let Some(core) = self.core.as_ref() else { return };
        let Some(dialog) = self.shares.dialog.as_mut() else { return };
        if dialog.busy || dialog.invalid() || dialog.made.is_some() {
            return;
        }
        dialog.busy = true;
        dialog.error = None;
        match dialog.edit.clone() {
            Some(link) => core.lib.update_share(link, dialog.choice()),
            None => core.lib.create_share(dialog.target.clone(), dialog.choice()),
        }
        cx.notify();
    }

    pub fn copy_share_link(&mut self, link: &ShareLink, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(link.url.clone()));
        self.show_toast(tr("Link copied"), cx);
    }

    pub fn delete_share(&mut self, id: String, cx: &mut Context<Self>) {
        self.confirm = None;
        if let Some(core) = self.core.as_ref() {
            core.lib.delete_share(id);
        }
        cx.notify();
    }

    pub fn on_shares(&mut self, result: Result<Vec<ShareLink>, String>, cx: &mut Context<Self>) {
        match result {
            Ok(links) => {
                self.shares.links = Some(links);
                self.shares.error = None;
            }
            Err(message) => self.shares.error = Some(message.into()),
        }
        if self.route == Route::Shares {
            self.invalidate_page();
        }
        cx.notify();
    }

    pub fn on_share_created(&mut self, result: Result<ShareLink, String>, cx: &mut Context<Self>) {
        if let Some(dialog) = self.shares.dialog.as_mut() {
            dialog.busy = false;
            match result {
                Ok(link) => dialog.made = Some(link),
                Err(message) => dialog.error = Some(message.into()),
            }
        }
        self.controls.text = None;
        self.refresh_shares();
        cx.notify();
    }

    pub fn on_share_saved(&mut self, result: Result<(), String>, cx: &mut Context<Self>) {
        match result {
            Ok(()) => {
                self.shares.dialog = None;
                self.controls.text = None;
            }
            Err(message) => {
                if let Some(dialog) = self.shares.dialog.as_mut() {
                    dialog.busy = false;
                    dialog.error = Some(message.into());
                }
            }
        }
        self.refresh_shares();
        cx.notify();
    }

    pub fn on_share_deleted(&mut self, result: Result<String, String>, cx: &mut Context<Self>) {
        match result {
            Ok(_) => self.show_toast(tr("Link deleted"), cx),
            Err(message) => self.show_toast(message, cx),
        }
        self.refresh_shares();
        cx.notify();
    }

    /// Reloads the links for the page and the Settings count.
    pub fn refresh_shares(&self) {
        if let Some(core) = self.core.as_ref() {
            core.lib.load_shares();
        }
    }

    fn share_subject_line(&mut self, subject: &ShareSubject, note: Option<SharedString>, layout: &Layout) -> AnyElement {
        let t = self.theme;
        let art = self.artwork(subject.art.as_ref(), if subject.shape == ArtShape::Circle { "user-fill" } else { "music-notes-fill" }, THUMB, subject.shape, layout);
        let kind_line: SharedString = if subject.detail.is_empty() { subject.kind.into() } else { format!("{} \u{2022} {}", subject.kind, subject.detail).into() };
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(t.s(SHARE_SUBJECT_GAP))
            .min_w(px(0.0))
            .child(div().flex_none().child(art))
            .child(
                div()
                    .min_w(px(0.0))
                    .line_height(relative(1.35))
                    .child(div().truncate().font_weight(FontWeight::MEDIUM).child(subject.title.clone()))
                    .child(div().truncate().text_size(t.text(TEXT_SM)).text_color(t.t2()).child(kind_line))
                    .children(note.filter(|n| !n.is_empty()).map(|n| div().truncate().text_size(t.text(TEXT_SM)).text_color(t.t2()).child(trf("\u{201c}{text}\u{201d}", &[("text", &n)])))),
            )
            .into_any_element()
    }

    /// What anyone with the link gets, and until when.
    fn link_note(&self, link: &ShareLink) -> String {
        let ends = if link.expired(now_ms()) { trf("Expired on {date}.", &[("date", &day(link.expires_ms))]) } else { trf("Works until {date}.", &[("date", &day(link.expires_ms))]) };
        let what = if link.downloadable == Some(true) && self.share_downloads_on() { tr("Anyone with it can play and download.") } else { tr("Anyone with it can play, without signing in.") };
        format!("{ends} {what}")
    }

    pub fn render_share_dialog(&mut self, layout: &Layout, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = self.theme;
        let dialog = self.shares.dialog.as_ref()?;
        let (edit, made, subject, busy, error, invalid) = (dialog.edit.clone(), dialog.made.clone(), dialog.subject.clone(), dialog.busy, dialog.error.clone(), dialog.invalid());
        let (expiry, description, custom, downloadable) = (dialog.expiry, dialog.description.clone(), dialog.custom.clone(), dialog.downloadable);
        let target = dialog.target.clone();
        let title: SharedString = match (&edit, target.kind) {
            (Some(_), _) => tr("Edit Link").into(),
            (None, ShareKind::Song) if target.ids.len() > 1 => trf("Share {n} Songs", &[("n", &crate::i18n::fmt_int(target.ids.len() as i64))]).into(),
            (None, ShareKind::Song) => tr("Share Song").into(),
            (None, ShareKind::Album) => tr("Share Album").into(),
            (None, ShareKind::Playlist) => tr("Share Playlist").into(),
            (None, ShareKind::Artist) => tr("Share Artist").into(),
        };
        let close = |this: &mut Shell, cx: &mut Context<Shell>| {
            this.close_share_dialog(cx);
        };
        let subject_line = self.share_subject_line(&subject, None, layout);
        let mut panel = crate::ui::dialog_panel(&t, "share").child(crate::ui::dialog_title(&t, title)).child(subject_line);
        let mut actions = crate::ui::dialog_actions(&t);
        if let Some(link) = made {
            let url = div().truncate().text_color(t.t1()).child(link.url.clone());
            let field = crate::ui::dialog_field(&t, "share-link", false, None, url);
            panel = panel.child(crate::ui::dialog_block(&t, tr("Link"), field)).child(crate::ui::dialog_note(&t, self.link_note(&link)));
            let done = dialog_button(&t, "share-done", DialogButton::Ghost, true).child(tr("Done")).stop_top(&t).on_click(cx.listener(move |this, _, _, cx| close(this, cx)));
            let copy = dialog_button(&t, "share-copy", DialogButton::Primary, true)
                .child(icon(&t, "copy-regular", 15.0, t.on_accent.hsla(1.0)))
                .child(tr("Copy Link"))
                .stop_top(&t)
                .on_click(cx.listener(move |this, _, _, cx| this.copy_share_link(&link, cx)));
            actions = actions.child(done).child(copy);
        } else {
            let line = self.field_line(&TextTarget::ShareDescription, &description, tr("Optional"), "share-description", t.text(TEXT_MD));
            let focused = self.focused_target() == Some(TextTarget::ShareDescription);
            let field = crate::ui::dialog_field(&t, "share-description-field", focused, None, line).on_click(cx.listener(|this, _, _, cx| this.focus_field(TextTarget::ShareDescription, cx)));
            panel = panel.child(crate::ui::dialog_block(&t, tr("Description"), field));
            let first_label: SharedString = match &edit {
                Some(link) if link.expired(now_ms()) => trf("Expired on {date}", &[("date", &day(link.expires_ms))]).into(),
                Some(link) => day(link.expires_ms).into(),
                None => tr("Server default").into(),
            };
            let preset_labels = [tr("1 day"), tr("1 week"), tr("1 month")];
            let mut options = vec![SelectOption::new("first", first_label)];
            options.extend(EXPIRY_PRESETS.iter().zip(preset_labels).map(|((id, _), label)| SelectOption::new(*id, label)));
            options.push(SelectOption::new("custom", tr("Custom date\u{2026}")));
            let value = match expiry {
                Expiry::First => "first",
                Expiry::Preset(i) => EXPIRY_PRESETS[i].0,
                Expiry::Custom => "custom",
            };
            let pill = self.select_pill("share-expires", value, Rc::new(options), tr("Choose"), None, |this, value, cx| {
                if let Some(dialog) = this.shares.dialog.as_mut() {
                    dialog.expiry = match value.as_ref() {
                        "first" => Expiry::First,
                        "custom" => Expiry::Custom,
                        id => EXPIRY_PRESETS.iter().position(|(p, _)| *p == id).map_or(Expiry::First, Expiry::Preset),
                    };
                    cx.notify();
                }
            }, cx);
            panel = panel.child(crate::ui::dialog_block(&t, tr("Expires"), pill));
            if expiry == Expiry::Custom {
                let line = self.field_line(&TextTarget::ShareDate, &custom, tr("Date"), "share-date", t.text(TEXT_MD));
                let focused = self.focused_target() == Some(TextTarget::ShareDate);
                let field = crate::ui::dialog_field(&t, "share-date-field", focused, Some("calendar-blank-regular"), line).on_click(cx.listener(|this, _, _, cx| this.focus_field(TextTarget::ShareDate, cx)));
                panel = panel.child(crate::ui::dialog_block(&t, tr("Date"), field));
            }
            match (&edit, expiry) {
                (None, Expiry::First) => panel = panel.child(crate::ui::dialog_note(&t, tr("The server sets the date: a year from now unless its admin chose another."))),
                (Some(link), Expiry::First) if link.expired(now_ms()) => panel = panel.child(crate::ui::dialog_note(&t, tr("Pick a new date to open the link again."))),
                _ => {}
            }
            if self.share_downloads_on() {
                let toggle = switch(&t, "share-downloads", downloadable, true, cx, |this, cx| {
                    if let Some(dialog) = this.shares.dialog.as_mut() {
                        dialog.downloadable = !dialog.downloadable;
                        cx.notify();
                    }
                });
                panel = panel.child(crate::ui::dialog_switch(&t, tr("Allow downloads"), tr("People with the link can save the songs, not only play them."), toggle));
            }
            let cancel = dialog_button(&t, "share-cancel", DialogButton::Ghost, true).child(tr("Cancel")).stop_top(&t).on_click(cx.listener(move |this, _, _, cx| close(this, cx)));
            let label = match (&edit, busy) {
                (_, true) => tr("Saving\u{2026}"),
                (Some(_), false) => tr("Save"),
                (None, false) => tr("Create Link"),
            };
            let submit = dialog_button(&t, "share-submit", DialogButton::Primary, !invalid && !busy)
                .when(edit.is_none(), |d| d.child(icon(&t, "check-bold", 15.0, t.on_accent.hsla(1.0))))
                .child(label)
                .stop_top(&t)
                .on_click(cx.listener(|this, _, _, cx| this.share_submit(cx)));
            let submit = submit.when(invalid, |d| d.tooltip(crate::ui::tip(&t, tr("Pick a day after today"))));
            actions = actions.child(cancel).child(submit);
        }
        Some(
            crate::ui::dialog_scrim(&t, "share-scrim")
                .on_mouse_down(gpui::MouseButton::Left, cx.listener(move |this, _, _, cx| close(this, cx)))
                .child(panel.children(error.map(|e| crate::ui::error_text(&t, e))).child(actions))
                .into_any_element(),
        )
    }

    /// newest first.
    pub fn build_shares(&mut self, _layout: &Layout) -> Vec<Block> {
        if self.shares.links.is_none() && self.shares.error.is_none() {
            self.refresh_shares();
        }
        if !self.sharing_on() {
            return vec![Shell::plain_block(|this, layout, _, _| {
                let empty = this.empty_state("link-simple-regular", tr("Sharing is turned off on this server"), tr("Links can be made once the server\u{2019}s admin turns sharing on."), false);
                this.list_page(layout, empty.into_any_element())
            })];
        }
        let links = self.shares.links.clone().unwrap_or_default();
        if let Some(error) = self.shares.error.clone().filter(|_| links.is_empty()) {
            return vec![Shell::plain_block(move |this, layout, _, _| {
                let empty = this.empty_state("link-simple-regular", tr("Share Links"), error.clone(), false);
                this.list_page(layout, empty.into_any_element())
            })];
        }
        if links.is_empty() {
            return vec![Shell::plain_block(|this, layout, _, _| {
                let empty = this.empty_state("link-simple-regular", tr("No share links yet"), tr("Choose Share\u{2026} in the menu of a song, album or playlist to make a link anyone can open, without an account."), false);
                this.list_page(layout, empty.into_any_element())
            })];
        }
        let mut blocks = vec![
            Shell::plain_block(|this, layout, _, _| {
                let t = this.theme;
                let summary = div().text_color(t.t2()).child(tr("Anyone with one of these links can play what it shares, without an account, until its date."));
                this.list_page(layout, summary.into_any_element())
            }),
            Block::Spacer(LIST_PAGE_GAP),
        ];
        blocks.extend(links.into_iter().map(|link| Shell::plain_block(move |this, layout, _, cx| this.render_share_row(&link, layout, cx))));
        blocks
    }

    /// A centered list page column, used by Downloads and Share Links.
    pub fn list_page(&self, layout: &Layout, content: AnyElement) -> AnyElement {
        let t = self.theme;
        div().w(layout.main_width).child(div().w_full().max_w(t.s(LIST_PAGE_W)).mx_auto().px(t.s(GUTTER)).child(content)).into_any_element()
    }

    fn render_share_row(&mut self, link: &ShareLink, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let ended = link.expired(now_ms());
        let subject = self.share_subject(&link.target, &link.entries);
        let note = (!navia_core::shares::is_server_label(&link.description, &subject.title)).then(|| SharedString::from(link.description.clone()));
        let what = self.share_subject_line(&subject, note, layout);
        let width = f32::from(layout.main_width) / t.token_scale();
        let (show_ends, show_visits) = (width > 560.0, width > 760.0);
        let facts = |first: SharedString, second: Option<SharedString>, dim_first: bool| {
            div()
                .flex()
                .flex_col()
                .min_w(px(0.0))
                .line_height(relative(1.35))
                .text_size(t.text(TEXT_SM))
                .text_color(t.t2())
                .child(div().truncate().when(dim_first, |d| d.text_color(t.t3())).child(first))
                .children(second.map(|s| div().truncate().text_color(t.t3()).child(s)))
        };
        let ends: SharedString = if ended { trf("Expired on {date}", &[("date", &day(link.expires_ms))]).into() } else { trf("Until {date}", &[("date", &day(link.expires_ms))]).into() };
        let downloads = (link.downloadable == Some(true) && self.share_downloads_on()).then(|| SharedString::from(tr("Downloads allowed")));
        let visits: SharedString = if link.visits > 0 { trn("Opened {count} time", "Opened {count} times", link.visits as usize).into() } else { tr("Not opened yet").into() };
        let last = link.last_visited_ms.map(|ms| SharedString::from(trf("Last on {date}", &[("date", &day(ms))])));
        let id = link.id.clone();
        let key = |part: &str| gpui::ElementId::Name(format!("share-{part}-{id}").into());
        let (copy_link, edit_link, title) = (link.clone(), link.clone(), subject.title.clone());
        let copy = crate::ui::icon_button(&t, key("copy"), if ended { tr("This link has expired") } else { tr("Copy Link") }, "copy-regular", false)
            .when(ended, |d| d.opacity(0.35))
            .when(!ended, |d| d.stop(&t).on_click(cx.listener(move |this, _, _, cx| this.copy_share_link(&copy_link, cx))));
        let edit = crate::ui::icon_button(&t, key("edit"), tr("Edit"), "pencil-simple-regular", false)
            .stop(&t)
            .on_click(cx.listener(move |this, _, _, cx| this.open_share_edit(edit_link.clone(), cx)));
        let delete_id = link.id.clone();
        let delete = crate::ui::icon_button(&t, key("delete"), tr("Delete"), "trash-regular", false)
            .stop(&t)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.confirm = Some(crate::view_connect::Confirm::DeleteShare { id: delete_id.clone(), title: title.clone() });
                cx.notify();
            }));
        let row = div()
            .id(key("row"))
            .flex()
            .flex_row()
            .items_center()
            .gap(t.s(SHARE_SUBJECT_GAP))
            .min_h(t.s(ROW_H))
            .px(t.s(LIST_ROW_PAD_X))
            .rounded(t.s(R_ROW))
            .hover(|s| s.bg(t.row()))
            .child(div().flex_1().min_w(px(0.0)).child(what))
            .when(show_ends, |d| d.child(facts(ends, downloads, ended).flex_none().w(t.s(SHARE_ENDS_W))))
            .when(show_visits, |d| d.child(facts(visits, last, false).flex_none().w(t.s(SHARE_VISITS_W))))
            .child(div().flex().flex_row().gap(t.s(SHARE_ACTIONS_GAP)).flex_none().text_color(t.t2()).child(copy).child(edit).child(delete));
        self.list_page(layout, row.into_any_element())
    }
}
