//! Settings > Storage: the song cache limit, disk use per cache, Downloads, and Clear Cache. Also tracks which songs
//! play offline from the cache.

use std::collections::HashSet;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{AnyElement, Context, FontWeight, SharedString, Window, div};
use navia_core::songcache::LIMIT_CHOICES;

use crate::focus::FocusStop;
use crate::i18n::{tr, trf, trn};
use crate::shell::{Route, Shell};
use crate::theme::size::*;
use crate::ui_controls::SelectOption;
use crate::view_downloads::format_size as format_bytes;

/// Sizes as Downloads shows them, with zero as 0 KB rather than rounded up.
fn format_size(bytes: u64) -> String {
    if bytes == 0 { trf("{size} KB", &[("size", "0")]) } else { format_bytes(bytes) }
}

/// A limit choice in whole gigabytes, like "2 GB".
pub fn limit_label(bytes: u64) -> String {
    if bytes == 0 { tr("Off").to_string() } else { trf("{size} GB", &[("size", &crate::i18n::fmt_int((bytes / navia_core::songcache::GB) as i64))]) }
}
use crate::view_settings::{ClickHandler, SettingRow, settings_card};

/// measured off the UI thread.
#[derive(Clone, Default)]
pub struct StorageInfo {
    pub songs: u64,
    pub cached: u64,
    pub limit: u64,
    pub low_disk: bool,
    pub artwork: u64,
    pub lyrics: u64,
    pub downloads: u64,
}

#[derive(Default)]
pub struct StorageState {
    pub info: Option<StorageInfo>,
    measured: Option<Instant>,
    loading: bool,
    /// Songs in the song cache by row id, as of the last check.
    pub cached_rows: HashSet<u32>,
}

/// How often Settings re-measures at most while open.
const MEASURE_EVERY: Duration = Duration::from_secs(5);

impl Shell {
    /// Measures the caches and records which songs are cached, off the UI thread.
    pub fn refresh_storage(&mut self, cx: &mut Context<Self>) {
        let Some(lib) = self.core.as_ref().map(|c| c.lib.clone()) else { return };
        if self.storage.loading {
            return;
        }
        self.storage.loading = true;
        cx.spawn(async move |this, cx| {
            let (info, rows) = cx
                .background_spawn(async move {
                    let cache = lib.downloads.song_cache();
                    let (songs, cached) = cache.as_ref().map_or((0, 0), |c| c.usage());
                    let info = StorageInfo {
                        songs,
                        cached,
                        limit: cache.as_ref().map_or(0, |c| c.limit()),
                        low_disk: cache.as_ref().is_some_and(|c| c.low_disk()),
                        artwork: lib.artwork.usage(),
                        lyrics: lib.lyrics.usage(),
                        downloads: lib.downloads.total_size().unwrap_or(0),
                    };
                    (info, lib.cached_song_rowids())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.storage.loading = false;
                this.storage.info = Some(info);
                this.storage.measured = Some(Instant::now());
                this.storage.cached_rows = rows.into_iter().collect();
                if this.route == Route::Settings || this.is_offline() {
                    this.invalidate_page();
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Whether a song plays without the server, because it's downloaded or cached.
    pub fn plays_offline(&self, ix: u32) -> bool {
        self.downloaded.contains(&ix) || self.storage.cached_rows.contains(&ix)
    }

    /// Like [`Self::plays_offline`], but asks the cache now, for the moment a song is played.
    pub fn plays_offline_now(&self, ix: u32) -> bool {
        self.plays_offline(ix) || self.core.as_ref().is_some_and(|c| c.lib.is_row_cached(ix))
    }

    fn set_song_cache_limit(&mut self, bytes: u64, cx: &mut Context<Self>) {
        let Some(cache) = self.core.as_ref().and_then(|c| c.lib.downloads.song_cache()) else { return };
        // Lowering it deletes files, so not on the UI thread.
        cx.background_spawn(async move {
            let _ = cache.set_limit(bytes);
        })
        .detach();
        if let Some(info) = self.storage.info.as_mut() {
            info.limit = bytes;
        }
        self.storage.measured = None;
        cx.notify();
    }

    /// downloads and the library stay.
    pub fn clear_caches(&mut self, cx: &mut Context<Self>) {
        let Some(lib) = self.core.as_ref().map(|c| c.lib.clone()) else { return };
        cx.spawn(async move |this, cx| {
            cx.background_spawn(async move {
                if let Some(cache) = lib.downloads.song_cache() {
                    cache.clear();
                }
                lib.artwork.clear();
                lib.lyrics.clear();
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                this.storage.measured = None;
                this.refresh_storage(cx);
            });
        })
        .detach();
    }

    pub fn render_settings_storage(&mut self, _main_tokens: f32, _window: &mut Window, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let t = self.theme;
        if self.core.is_some() && self.storage.measured.is_none_or(|at| at.elapsed() > MEASURE_EVERY) {
            self.refresh_storage(cx);
        }
        let info = self.storage.info.clone();
        let value = |text: String| div().flex_none().text_color(t.t2()).child(text).into_any_element();
        let limit = info.as_ref().map_or(navia_core::songcache::DEFAULT_LIMIT, |i| i.limit);
        let options = Rc::new(LIMIT_CHOICES.iter().map(|&b| SelectOption::new(b.to_string(), limit_label(b))).collect::<Vec<_>>());
        let picker = self.select_pill("song-cache-limit", limit.to_string(), options, tr("Choose"), Some(240.0), |this, value, cx| {
            if let Ok(bytes) = value.parse() {
                this.set_song_cache_limit(bytes, cx);
            }
        }, cx);
        let cache_hint: SharedString = tr("Songs you play are saved on this device as they stream, so they start at once and play offline. When it's full, the least recently played go first.").into();
        let (used, used_hint): (AnyElement, SharedString) = match &info {
            None => (value(String::new()), SharedString::default()),
            Some(i) if i.limit == 0 => (value(tr("Off").to_string()), tr("Nothing is kept while the song cache is off.").into()),
            Some(i) => {
                let count = trn("{count} song", "{count} songs", i.cached as usize);
                let hint = if i.low_disk { format!("{count}. {}", tr("Paused: the disk is nearly full.")) } else { count.to_string() };
                (value(trf("{used} of {limit}", &[("used", &format_size(i.songs)), ("limit", &limit_label(i.limit))])), hint.into())
            }
        };
        let open_cached: Option<ClickHandler> = self.core.is_some().then(|| Box::new(cx.listener(|this, _, _, cx| this.navigate(Route::CachedSongs, cx))) as ClickHandler);
        let cached = SettingRow::link(&t, "storage-cached", tr("Cached songs"), used_hint, Some(used), open_cached);
        let size_of = |f: fn(&StorageInfo) -> u64| value(info.as_ref().map(|i| format_size(f(i))).unwrap_or_default());
        let count = self.downloads_count();
        let open_downloads: ClickHandler = Box::new(cx.listener(|this, _, _, cx| this.navigate(Route::Downloads, cx)));
        let downloads = SettingRow::link(&t, "storage-downloads", tr("Downloads"), trn("{count} song on this device", "{count} songs on this device", count).into(), Some(size_of(|i| i.downloads)), Some(open_downloads));
        let clear = div()
            .id("clear-cache")
            .flex()
            .items_center()
            .flex_none()
            .h(t.s(34.0))
            .px(t.s(14.0))
            .rounded_full()
            .bg(t.panel_2())
            .text_size(t.text(TEXT_SM))
            .font_weight(FontWeight::MEDIUM)
            .when(self.core.is_some(), |d| {
                d.child(crate::ui::hover_pill(&t)).stop(&t).on_click(cx.listener(|this, _, _, cx| {
                    this.confirm = Some(crate::view_connect::Confirm::ClearCache);
                    cx.notify();
                }))
            })
            .child(tr("Clear Cache"))
            .when(self.core.is_none(), |d| d.opacity(0.4))
            .into_any_element();
        let rows = vec![
            SettingRow::new(&t, tr("Song cache"), Some(cache_hint), false, Some(picker)),
            cached,
            SettingRow::new(&t, tr("Artwork"), None, false, Some(size_of(|i| i.artwork))),
            SettingRow::new(&t, tr("Lyrics"), None, false, Some(size_of(|i| i.lyrics))),
            downloads,
            SettingRow::new(&t, tr("Clear cache"), Some(tr("Removes cached songs, artwork and lyrics. Downloads and your library stay.").into()), false, Some(clear)),
        ];
        settings_card(&t, &self.settings_words(), tr("Storage"), rows)
    }
}
