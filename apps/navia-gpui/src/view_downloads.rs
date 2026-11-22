//! Downloads: what's on this device, with real file sizes, per-song remove and Clear All.

use crate::ui::FullTextTip;
use crate::i18n::{tr, trf, trn};
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, Context, FontWeight, SharedString, div, px};

use crate::focus::FocusStop;
use crate::pages::Block;
use crate::shell::{Layout, Route, Shell};
use crate::theme::size::*;
use crate::ui::icon;
use crate::ui_controls::glass_action;
use crate::view_cards::ArtShape;
use crate::view_connect::Confirm;
use navia_core::library::collections::CollectionKind;

#[derive(Clone)]
pub struct DownloadEntry {
    id: SharedString,
    rowid: Option<u32>,
    title: SharedString,
    artist: SharedString,
    album: SharedString,
    album_id: SharedString,
    size: u64,
    /// Downloaded by itself, not only as part of a kept playlist or album.
    single: bool,
}

/// see [`Shell::list_song_row`].
#[derive(Clone)]
pub struct ListSong {
    pub rowid: Option<u32>,
    pub title: SharedString,
    pub artist: SharedString,
    pub album: SharedString,
    pub album_id: SharedString,
    pub size: u64,
}

pub type NameClick = Box<dyn Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App)>;

#[derive(Default)]
pub struct DownloadsView {
    pub rows: Rc<Vec<DownloadEntry>>,
    pub total: u64,
    pub loaded: bool,
    pub generation: u64,
}

/// A file size as file browsers show it, a little finer than `format_size`, like "34.2 MB".
pub fn file_size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e9 {
        trf("{n} GB", &[("n", &crate::i18n::fmt_dec(b / 1e9, 2))])
    } else if b >= 1e6 {
        trf("{n} MB", &[("n", &crate::i18n::fmt_dec(b / 1e6, 1))])
    } else {
        trf("{n} KB", &[("n", &crate::i18n::fmt_int(((b / 1e3).round() as i64).max(1)))])
    }
}

pub fn format_size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e9 {
        trf("{size} GB", &[("size", &crate::i18n::fmt_dec(b / 1e9, 1))])
    } else if b >= 1e6 {
        trf("{size} MB", &[("size", &crate::i18n::fmt_int((b / 1e6).round() as i64))])
    } else {
        trf("{size} KB", &[("size", &crate::i18n::fmt_int(((b / 1e3).round() as i64).max(1)))])
    }
}

/// "1 song" or "n songs".
pub fn songs_on_device(count: usize) -> String {
    trn("{count} song", "{count} songs", count)
}

impl Shell {
    /// Reloads the finished downloads off the UI thread.
    pub fn refresh_downloads(&mut self, cx: &mut Context<Self>) {
        let Some(core) = self.core.as_ref() else { return };
        let lib = core.lib.clone();
        self.downloads_view.generation += 1;
        let generation = self.downloads_view.generation;
        cx.spawn(async move |this, cx| {
            let rows = cx.background_spawn(async move {
                let singles: std::collections::HashSet<String> = lib.downloads.single_download_ids().unwrap_or_default().into_iter().collect();
                let mut rows: Vec<DownloadEntry> = lib.downloads.entries().unwrap_or_default().into_iter().map(|(id, size)| {
                    let rowid = lib.rowid_for_song(&id);
                    let found = rowid.and_then(|r| lib.rows(&[r]).into_iter().next());
                    let (title, artist, album, album_id) = match found {
                        Some((_, s)) => (s.title, s.artist, s.album, s.album_id),
                        // Downloaded, but the server no longer lists it.
                        None => (tr("Song no longer on the server").into(), SharedString::default(), SharedString::default(), SharedString::default()),
                    };
                    let single = singles.contains(&id);
                    DownloadEntry { id: id.into(), rowid, title, artist, album, album_id, size, single }
                }).collect();
                rows.sort_by_cached_key(|r| r.title.to_lowercase());
                rows
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                if this.downloads_view.generation != generation {
                    return;
                }
                this.downloads_view.total = rows.iter().map(|r| r.size).sum();
                this.downloads_view.rows = Rc::new(rows);
                this.downloads_view.loaded = true;
                if this.route == Route::Downloads {
                    this.invalidate_page();
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn downloads_count(&self) -> usize {
        if self.downloads_view.loaded { self.downloads_view.rows.len() } else { self.downloaded.len() }
    }

    /// Kept playlists and albums first, then the songs downloaded on their own.
    pub fn build_downloads(&mut self, _layout: &Layout) -> Vec<Block> {
        let rows = self.downloads_view.rows.clone();
        let collections = self.synced.rows.len();
        if rows.is_empty() && collections == 0 {
            return vec![Shell::plain_block(|this, layout, _, _| {
                let empty = this.empty_state("arrow-down-regular", tr("No downloads yet"), tr("Songs, albums and playlists you download are saved on this device and play without the server."), false);
                this.list_page(layout, empty.into_any_element())
            })];
        }
        let mut blocks = vec![Shell::plain_block(|this, layout, _, cx| this.render_downloads_summary(layout, cx)), Block::Spacer(LIST_PAGE_GAP)];
        if collections > 0 {
            blocks.push(Shell::plain_block(|this, layout, _, _| this.downloads_heading(tr("Playlists and Albums"), Some(tr("Kept in step with the server: songs added download by themselves, and songs taken out are removed unless another download holds them.")), layout)));
            blocks.extend((0..collections).map(|i| Shell::plain_block(move |this, layout, _, cx| this.render_collection_row(i, layout, cx))));
        }
        let singles: Vec<usize> = (0..rows.len()).filter(|i| rows[*i].single).collect();
        if !singles.is_empty() {
            if collections > 0 {
                blocks.push(Block::Spacer(LIST_PAGE_GAP));
                blocks.push(Shell::plain_block(|this, layout, _, _| this.downloads_heading(tr("Songs"), None, layout)));
            }
            blocks.extend(singles.into_iter().map(|i| Shell::plain_block(move |this, layout, _, cx| this.render_download_row(i, layout, cx))));
        }
        blocks
    }

    /// A section's title and note.
    fn downloads_heading(&self, title: &'static str, note: Option<&'static str>, layout: &Layout) -> AnyElement {
        let t = self.theme;
        let head = div()
            .flex()
            .flex_col()
            .gap(t.s(6.0))
            .pb(t.s(6.0))
            .child(div().text_size(t.text(TEXT_SECTION)).font_weight(FontWeight::SEMIBOLD).text_color(t.t1()).child(title))
            .children(note.map(|n| div().mb(t.s(4.0)).text_size(t.text(TEXT_SM)).text_color(t.t2()).child(n)));
        self.download_frame(layout, head)
    }

    /// A kept playlist or album: cover, kind, size, progress and its switch. The X asks first.
    fn render_collection_row(&mut self, ix: usize, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let main_tokens = f32::from(layout.main_width) / t.token_scale();
        let Some(row) = self.synced.rows.get(ix).cloned() else { return div().into_any_element() };
        let (name, sub, cover, route) = match row.kind {
            CollectionKind::Playlist => {
                let Some(p) = self.playlists_full.iter().find(|p| p.id == row.id).cloned() else { return div().into_any_element() };
                let kind = if p.smart { tr("Smart Playlist") } else { tr("Playlist") };
                let key = self.playlist_art_key(&p.id);
                (p.name.clone(), format!("{kind} • {}", songs_on_device(p.song_count as usize)), self.artwork(Some(&key), "playlist-regular", 42.0, ArtShape::Rounded(R_SM), layout), Route::Playlist(p.id.clone()))
            }
            CollectionKind::Album => {
                let Some(a) = self.albums.iter().find(|a| a.id == row.id).cloned() else { return div().into_any_element() };
                (a.name.clone(), format!("{} • {} • {}", tr("Album"), a.artist, songs_on_device(a.song_count as usize)), self.artwork(Some(&a.id), "music-notes-fill", 42.0, ArtShape::Rounded(R_SM), layout), Route::Album(a.id.clone()))
            }
        };
        let status = &row.status;
        let state = div()
            .flex_grow(1.3)
            .flex_basis(gpui::relative(0.0))
            .min_w(px(0.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(t.s(DOWNLOAD_MARK_GAP))
            .text_size(t.text(TEXT_SM))
            .text_color(t.t2())
            .when(status.phase != navia_core::downloads::CollectionPhase::Done, |d| {
                d.child(crate::ui::download_glyph(&t, crate::collection_download::glyph_for(status), crate::collection_download::progress_of(status), DOWNLOAD_MARK, t.t2().into()))
            })
            .child(div().min_w(px(0.0)).truncate().child(crate::collection_download::status_text(status)));
        let open = route.clone();
        let label = div()
            .id(("collection-name", ix))
            .flex()
            .flex_col()
            .min_w(px(0.0))
            .when(main_tokens <= 620.0, |d| d.flex_1())
            .when(main_tokens > 620.0, |d| d.flex_grow(2.0).flex_basis(gpui::relative(0.0)))
            .line_height(gpui::relative(1.3))
            .child(div().id("name").font_weight(FontWeight::MEDIUM).truncate().child(name.clone()).full_text_tip(&t, name.clone()))
            .child(div().id("sub").text_size(t.text(TEXT_SM)).text_color(t.t2()).truncate().child(sub.clone()).full_text_tip(&t, sub))
            .stop(&t)
            .on_click(cx.listener(move |this, _, _, cx| this.navigate(open.clone(), cx)));
        let (kind, id) = (row.kind, row.id.clone());
        let remove = crate::ui::ink_button(&t, ("collection-remove", ix), tr("Remove Download"), "x-regular", 17.0, t.t2().into(), t.hover_ink())
            .stop(&t)
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_collection_download(kind, id.clone(), name.clone(), cx)));
        let line = div()
            .id(("collection-row", ix))
            .flex()
            .flex_row()
            .items_center()
            .gap(t.s(14.0))
            .h(t.s(LIST_ROW_H))
            .px(t.s(LIST_ROW_PAD_X))
            .rounded(t.s(R_ROW))
            .hover(|s| s.bg(t.row()))
            .child(div().flex_none().child(cover))
            .child(label)
            .when(main_tokens > 620.0, |d| d.child(state))
            .child(div().flex_none().w(t.s(72.0)).text_right().text_size(t.text(TEXT_SM)).text_color(t.t2()).child(format_size(status.bytes)))
            .child(remove);
        self.download_frame(layout, line)
    }

    fn download_frame(&self, layout: &Layout, content: impl IntoElement) -> AnyElement {
        self.list_page(layout, content.into_any_element())
    }

    fn render_downloads_summary(&mut self, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let count = self.downloads_view.rows.len();
        let total = self.downloads_view.total;
        let summary = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(t.s(16.0))
            .text_color(t.t2())
            .child(div().flex().flex_row().child(div().font_weight(FontWeight::MEDIUM).text_color(t.t1()).child(songs_on_device(count))).child(div().child(format!(" \u{b7} {}", trf("{size} on this device", &[("size", &format_size(total))])))))
            .child(glass_action(&t, "downloads-clear").child(icon(&t, "trash-regular", 16.0, t.t1())).child(tr("Clear All")).stop(&t).on_click(cx.listener(move |this, _, _, cx| {
                let collections = this.synced.rows.len();
                this.confirm = Some(Confirm::ClearDownloads { count, total, collections });
                cx.notify();
            })));
        self.download_frame(layout, summary)
    }

    fn render_download_row(&mut self, ix: usize, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let Some(entry) = self.downloads_view.rows.get(ix).cloned() else { return div().into_any_element() };
        let song = ListSong { rowid: entry.rowid, title: entry.title.clone(), artist: entry.artist.clone(), album: entry.album.clone(), album_id: entry.album_id.clone(), size: entry.size };
        let play: Option<NameClick> = entry.rowid.is_some().then(|| Box::new(cx.listener(move |this, _: &gpui::ClickEvent, _, cx| this.play_download(ix, cx))) as NameClick);
        let id = entry.id.clone();
        let remove = crate::ui::ink_button(&t, "remove", tr("Remove Download"), "x-regular", 17.0, t.t2().into(), t.hover_ink())
            .stop(&t)
            .on_click(cx.listener(move |this, _, _, cx| this.remove_download(id.clone(), cx)))
            .into_any_element();
        let row = self.list_song_row(("download-row", ix), &song, false, play, vec![remove], layout);
        self.download_frame(layout, row)
    }

    /// A song row on a list page like Downloads or Cached Songs. `on_name` is the click that plays it when the list has
    /// no selection. `selected` gives the selected fill.
    pub fn list_song_row(&mut self, id: impl Into<gpui::ElementId>, song: &ListSong, selected: bool, on_name: Option<NameClick>, buttons: Vec<AnyElement>, layout: &Layout) -> gpui::Stateful<gpui::Div> {
        let t = self.theme;
        let main_tokens = f32::from(layout.main_width) / t.token_scale();
        let playing = song.rowid.is_some_and(|r| self.player.song_ix == Some(r as usize));
        let art = self.artwork(Some(&song.album_id), "music-note-regular", THUMB, ArtShape::Rounded(R_SM), layout);
        let name = div()
            .id("list-song-name")
            .flex()
            .flex_col()
            .min_w(px(0.0))
            .when(main_tokens <= 620.0, |d| d.flex_1())
            .when(main_tokens > 620.0, |d| d.flex_grow(2.0).flex_basis(gpui::relative(0.0)))
            .line_height(gpui::relative(1.3))
            .child(div().id("title").font_weight(FontWeight::MEDIUM).truncate().when(playing, |d| d.text_color(t.accent_soft_hsla())).child(song.title.clone()).full_text_tip(&t, song.title.clone()))
            .child(div().id("artist").text_size(t.text(TEXT_SM)).text_color(t.t2()).truncate().child(song.artist.clone()).full_text_tip(&t, song.artist.clone()))
            .when_some(on_name, |d, play| d.stop(&t).on_click(move |e, w, cx| play(e, w, cx)));
        let (rest, hover) = crate::view_rows::row_fills(&t, selected, false);
        div()
            .id(id)
            .flex()
            .flex_row()
            .items_center()
            .gap(t.s(14.0))
            .h(t.s(LIST_ROW_H))
            .px(t.s(LIST_ROW_PAD_X))
            .rounded(t.s(R_ROW))
            .when_some(rest, |d, bg| d.bg(bg))
            .hover(move |s| s.bg(hover))
            .child(div().flex_none().child(art))
            .child(name)
            .when(main_tokens > 620.0, |d| d.child(div().flex_grow(1.3).flex_basis(gpui::relative(0.0)).min_w(px(0.0)).text_size(t.text(TEXT_SM)).text_color(t.t2()).truncate().id("album").child(song.album.clone()).full_text_tip(&t, song.album.clone())))
            .child(div().flex_none().w(t.s(72.0)).text_right().text_size(t.text(TEXT_SM)).text_color(t.t2()).child(format_size(song.size)))
            .children(buttons)
    }

    fn play_download(&mut self, ix: usize, cx: &mut Context<Self>) {
        let rows: Vec<u32> = self.downloads_view.rows.iter().filter_map(|r| r.rowid).collect();
        let at = self.downloads_view.rows.iter().take(ix).filter(|r| r.rowid.is_some()).count();
        self.core_play_list(rows, at, false, cx);
    }

    fn remove_download(&mut self, id: SharedString, cx: &mut Context<Self>) {
        let Some(core) = self.core.as_ref() else { return };
        let (lib, ids) = (core.lib.clone(), vec![id.to_string()]);
        cx.background_executor().spawn(async move {
            let _ = lib.downloads.remove(&ids);
        }).detach();
    }

    /// Clears all downloads after the dialog, and cancels anything still fetching.
    pub fn clear_all_downloads(&mut self, cx: &mut Context<Self>) {
        let Some(core) = self.core.as_ref() else { return };
        let lib = core.lib.clone();
        let ids: Vec<String> = self.downloads_view.rows.iter().map(|r| r.id.to_string()).collect();
        self.download_progress.clear();
        let _ = ids;
        // Kept playlists and albums stop too, so nothing downloads again.
        cx.background_executor().spawn(async move {
            let _ = lib.downloads.clear_all();
        }).detach();
        cx.notify();
    }
}
