//! Settings > Storage > Cached Songs, built from the Downloads page's parts. The cache only holds complete files, so
//! nothing here is partial.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, Context, FontWeight, MouseButton, SharedString, div};

use crate::focus::FocusStop;
use crate::i18n::{tr, trf, trn};
use crate::pages::Block;
use crate::shell::{Layout, ListId, Route, Shell};
use crate::theme::size::*;
use crate::ui::icon;
use crate::ui_controls::{PillWidth, SelectOption, glass_action};
use crate::view_connect::Confirm;
use crate::view_downloads::{ListSong, format_size};
use crate::view_menu::MenuRow;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum CacheSort {
    /// Most recently played or cached first. This is the reverse of eviction order.
    #[default]
    Recent,
    Size,
}

impl CacheSort {
    const ALL: [(CacheSort, &'static str); 2] = [(CacheSort::Recent, "recent"), (CacheSort::Size, "size")];

    fn label(self) -> &'static str {
        match self {
            CacheSort::Recent => tr("Recently Played"),
            CacheSort::Size => tr("Size"),
        }
    }
}

#[derive(Clone)]
struct CachedEntry {
    id: SharedString,
    rowid: Option<u32>,
    /// The selection key: the library row, or a key past every row for a song the library no longer lists.
    key: u32,
    song: ListSong,
    played: i64,
}

#[derive(Default)]
pub struct CachedView {
    rows: Rc<Vec<CachedEntry>>,
    pub loaded: bool,
    generation: u64,
    pub sort: CacheSort,
}

impl CachedView {
    fn sort_rows(&mut self) {
        let mut rows = self.rows.as_ref().clone();
        match self.sort {
            CacheSort::Recent => rows.sort_by(|a, b| b.played.cmp(&a.played)),
            CacheSort::Size => rows.sort_by(|a, b| b.song.size.cmp(&a.song.size).then(b.played.cmp(&a.played))),
        }
        self.rows = Rc::new(rows);
    }

    /// The library rows of the listed songs, in page order.
    pub fn library_rows(&self) -> Vec<u32> {
        self.rows.iter().filter_map(|r| r.rowid).collect()
    }

    fn keys(&self) -> Vec<u32> {
        self.rows.iter().map(|r| r.key).collect()
    }
}

impl Shell {
    /// Reloads the cache index off the UI thread.
    pub fn refresh_cached(&mut self, cx: &mut Context<Self>) {
        let Some(lib) = self.core.as_ref().map(|c| c.lib.clone()) else { return };
        self.cached_view.generation += 1;
        let generation = self.cached_view.generation;
        cx.spawn(async move |this, cx| {
            let rows = cx
                .background_spawn(async move {
                    let songs = lib.downloads.song_cache().map(|c| c.songs()).unwrap_or_default();
                    songs
                        .into_iter()
                        .enumerate()
                        .map(|(i, s)| {
                            let rowid = lib.rowid_for_song(&s.song_id);
                            let found = rowid.and_then(|r| lib.rows(&[r]).into_iter().next());
                            let song = match found {
                                Some((_, f)) => ListSong { rowid, title: f.title, artist: f.artist, album: f.album, album_id: f.album_id, size: s.bytes },
                                None => ListSong { rowid: None, title: tr("Song no longer on the server").into(), artist: SharedString::default(), album: SharedString::default(), album_id: SharedString::default(), size: s.bytes },
                            };
                            CachedEntry { id: s.song_id.into(), rowid, key: rowid.unwrap_or(u32::MAX - i as u32), song, played: s.last_played_ms }
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.cached_view.generation != generation {
                    return;
                }
                this.cached_view.rows = Rc::new(rows);
                this.cached_view.sort_rows();
                this.cached_view.loaded = true;
                if this.route == Route::CachedSongs {
                    this.invalidate_page();
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn build_cached(&mut self, _layout: &Layout) -> Vec<Block> {
        if !self.cached_view.loaded {
            return Vec::new();
        }
        let count = self.cached_view.rows.len();
        if count == 0 {
            return vec![Shell::plain_block(|this, layout, _, _| {
                let off = this.storage.info.as_ref().is_some_and(|i| i.limit == 0);
                let hint = if off { tr("Nothing is kept while the song cache is off.") } else { tr("Songs you play are kept here as they stream, so they start at once and play offline.") };
                let empty = this.empty_state("music-note-regular", tr("No cached songs"), hint, false);
                this.list_page(layout, empty.into_any_element())
            })];
        }
        let mut blocks = vec![Shell::plain_block(|this, layout, _, cx| this.render_cached_summary(layout, cx)), Block::Spacer(LIST_PAGE_GAP)];
        blocks.extend((0..count).map(|i| Shell::plain_block(move |this, layout, _, cx| this.render_cached_row(i, layout, cx))));
        blocks
    }

    fn cached_selected(&self) -> Vec<CachedEntry> {
        if self.selection_list != ListId::Cached {
            return Vec::new();
        }
        self.cached_view.rows.iter().filter(|r| self.selection.contains(r.key)).cloned().collect()
    }

    fn render_cached_summary(&mut self, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let rows = self.cached_view.rows.clone();
        let total: u64 = rows.iter().map(|r| r.song.size).sum();
        let limit = self.storage.info.as_ref().map(|i| i.limit).filter(|l| *l > 0);
        let selected = self.cached_selected();
        let used = match limit {
            Some(limit) => trf("{used} of {limit}", &[("used", &format_size(total)), ("limit", &crate::view_storage::limit_label(limit))]),
            None => trf("{size} on this device", &[("size", &format_size(total))]),
        };
        let lead: AnyElement = if selected.is_empty() {
            div().flex().flex_row().min_w(gpui::px(0.0)).child(div().font_weight(FontWeight::MEDIUM).text_color(t.t1()).child(trn("{count} song", "{count} songs", rows.len()))).child(div().truncate().child(format!(" \u{b7} {used}"))).into_any_element()
        } else {
            div().font_weight(FontWeight::MEDIUM).text_color(t.t1()).child(trn("{count} song selected", "{count} songs selected", selected.len())).into_any_element()
        };
        let options = Rc::new(CacheSort::ALL.iter().map(|(s, key)| SelectOption::new(*key, s.label())).collect::<Vec<_>>());
        let current = CacheSort::ALL.iter().find(|(s, _)| *s == self.cached_view.sort).map_or("recent", |(_, k)| k);
        let sort = self.select_pill_sized("cached-sort", current, options, tr("Sort"), PillWidth::Fit, |this, value, cx| {
            if let Some((sort, _)) = CacheSort::ALL.iter().find(|(_, k)| *k == value.as_ref()) {
                this.cached_view.sort = *sort;
                this.cached_view.sort_rows();
                this.invalidate_page();
                cx.notify();
            }
        }, cx);
        let actions: Vec<AnyElement> = if selected.is_empty() {
            let count = rows.len();
            vec![glass_action(&t, "cached-clear").child(icon(&t, "trash-regular", 16.0, t.t1())).child(tr("Clear Cache")).stop(&t).on_click(cx.listener(move |this, _, _, cx| {
                this.confirm = Some(Confirm::ClearSongCache { count, total });
                cx.notify();
            })).into_any_element()]
        } else {
            vec![
                glass_action(&t, "cached-keep").child(icon(&t, "arrow-down-regular", 16.0, t.t1())).child(tr("Keep Offline")).stop(&t).on_click(cx.listener(|this, _, _, cx| {
                    let rows = this.cached_selected();
                    this.cached_keep_offline(&rows, cx);
                })).into_any_element(),
                glass_action(&t, "cached-remove").child(icon(&t, "x-regular", 16.0, t.t1())).child(tr("Remove from Cache")).stop(&t).on_click(cx.listener(|this, _, _, cx| {
                    let rows = this.cached_selected();
                    this.cached_remove(&rows, cx);
                })).into_any_element(),
            ]
        };
        let summary = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap(t.s(16.0))
            .text_color(t.t2())
            .child(lead)
            .child(div().flex().flex_row().flex_wrap().items_center().gap(t.s(BUTTON_GAP)).child(sort).children(actions));
        self.list_page(layout, summary.into_any_element())
    }

    fn render_cached_row(&mut self, ix: usize, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let Some(entry) = self.cached_view.rows.get(ix).cloned() else { return div().into_any_element() };
        let selected = self.selection_list == ListId::Cached && self.selection.contains(entry.key);
        let keep: AnyElement = match entry.rowid.and_then(|r| self.download_progress.get(&r).copied().map(|f| (r, f))) {
            Some((rowid, fraction)) => self.download_progress_badge(("cached-dl", ix), rowid, fraction, cx),
            None if entry.rowid.is_some_and(|r| !self.downloaded.contains(&r)) => {
                let keep_entry = entry.clone();
                crate::ui::ink_button(&t, "keep", tr("Keep Offline"), "arrow-down-regular", 17.0, t.t2().into(), t.hover_ink())
                    .stop(&t)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.cached_keep_offline(std::slice::from_ref(&keep_entry), cx);
                    }))
                    .into_any_element()
            }
            // Already downloaded, or no longer in the library.
            None => div().flex_none().w(t.s(ACTION_W)).into_any_element(),
        };
        let remove_entry = entry.clone();
        let remove = crate::ui::ink_button(&t, "remove", tr("Remove from Cache"), "x-regular", 17.0, t.t2().into(), t.hover_ink())
            .stop(&t)
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.cached_remove(std::slice::from_ref(&remove_entry), cx);
            }))
            .into_any_element();
        let row = self
            .list_song_row(("cached-row", ix), &entry.song, selected, None, vec![keep, remove], layout)
            .stop_row(&t, gpui::px(0.0))
            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                let m = event.modifiers();
                let count = if event.is_keyboard() { 2 } else { event.click_count() };
                this.click_cached_row(ix, m.secondary(cx), m.shift, count, cx);
            }))
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| this.open_cached_menu(ix, event.position, cx)));
        self.list_page(layout, row.into_any_element())
    }

    fn click_cached_row(&mut self, ix: usize, ctrl: bool, shift: bool, count: usize, cx: &mut Context<Self>) {
        if count >= 2 && !ctrl && !shift {
            self.play_cached(ix, cx);
            return;
        }
        self.select_in(ListId::Cached);
        let keys = self.cached_view.keys();
        self.selection.click(ix, &keys, ctrl, shift);
        self.invalidate_page();
        cx.notify();
    }

    /// Plays the list from this song, using the songs the library still has.
    fn play_cached(&mut self, ix: usize, cx: &mut Context<Self>) {
        let rows = &self.cached_view.rows;
        if rows.get(ix).is_none_or(|r| r.rowid.is_none()) {
            return;
        }
        let at = rows.iter().take(ix).filter(|r| r.rowid.is_some()).count();
        let songs: Vec<u32> = rows.iter().filter_map(|r| r.rowid).collect();
        self.core_play_list(songs, at, false, cx);
    }

    fn open_cached_menu(&mut self, ix: usize, position: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
        self.select_in(ListId::Cached);
        let keys = self.cached_view.keys();
        self.selection.prepare_context(ix, &keys);
        let chosen = self.cached_selected();
        let n = chosen.len();
        let mut rows = Vec::new();
        if n == 1 && chosen[0].rowid.is_some() {
            rows.push(MenuRow::item(tr("Play"), "play-regular", move |this, cx| this.play_cached(ix, cx)));
        }
        if chosen.iter().any(|r| r.rowid.is_some()) {
            let keep = chosen.clone();
            let label: SharedString = if n == 1 { tr("Keep Offline").into() } else { trn("Keep {count} Song Offline", "Keep {count} Songs Offline", n).into() };
            rows.push(MenuRow::item(label, "arrow-down-regular", move |this, cx| this.cached_keep_offline(&keep, cx)));
        }
        let label: SharedString = if n == 1 { tr("Remove from Cache").into() } else { trn("Remove {count} Song from Cache", "Remove {count} Songs from Cache", n).into() };
        rows.push(MenuRow::item(label, "x-regular", move |this, cx| this.cached_remove(&chosen, cx)));
        self.invalidate_page();
        self.open_menu(rows, position, false, cx);
    }

    /// Turns cached songs into downloads. A cached copy at the download quality is moved, and the rest are fetched.
    /// Either way they leave the cache.
    fn cached_keep_offline(&mut self, rows: &[CachedEntry], cx: &mut Context<Self>) {
        let songs: Vec<u32> = rows.iter().filter_map(|r| r.rowid).filter(|r| !self.downloaded.contains(r)).collect();
        if songs.is_empty() {
            return;
        }
        self.set_downloaded(&songs, true, cx);
        self.selection.clear();
        self.invalidate_page();
        cx.notify();
    }

    fn cached_remove(&mut self, rows: &[CachedEntry], cx: &mut Context<Self>) {
        let Some(lib) = self.core.as_ref().map(|c| c.lib.clone()) else { return };
        let ids: Vec<String> = rows.iter().map(|r| r.id.to_string()).collect();
        if ids.is_empty() {
            return;
        }
        // Remove from the page at once and delete the files on a background thread.
        let gone: std::collections::HashSet<&str> = ids.iter().map(String::as_str).collect();
        let kept: Vec<CachedEntry> = self.cached_view.rows.iter().filter(|r| !gone.contains(r.id.as_ref())).cloned().collect();
        self.cached_view.rows = Rc::new(kept);
        for r in rows.iter().filter_map(|r| r.rowid) {
            self.storage.cached_rows.remove(&r);
        }
        self.selection.clear();
        self.invalidate_page();
        cx.spawn(async move |this, cx| {
            cx.background_spawn(async move {
                if let Some(cache) = lib.downloads.song_cache() {
                    cache.remove_songs(&ids);
                }
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                this.refresh_storage(cx);
                this.refresh_cached(cx);
            });
        })
        .detach();
        cx.notify();
    }

    /// Delete or Backspace with cached songs selected.
    pub fn cached_remove_selected(&mut self, cx: &mut Context<Self>) -> bool {
        let rows = self.cached_selected();
        if self.route != Route::CachedSongs || rows.is_empty() {
            return false;
        }
        self.cached_remove(&rows, cx);
        true
    }

    /// Clears cached songs only, after the confirmation dialog. Artwork and lyrics stay.
    pub fn clear_song_cache(&mut self, cx: &mut Context<Self>) {
        let Some(lib) = self.core.as_ref().map(|c| c.lib.clone()) else { return };
        self.cached_view.rows = Rc::new(Vec::new());
        self.storage.cached_rows.clear();
        self.selection.clear();
        self.invalidate_page();
        cx.spawn(async move |this, cx| {
            cx.background_spawn(async move {
                if let Some(cache) = lib.downloads.song_cache() {
                    cache.clear();
                }
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                this.refresh_storage(cx);
                this.refresh_cached(cx);
            });
        })
        .detach();
        cx.notify();
    }
}
