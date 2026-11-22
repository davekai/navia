//! Synced downloads of playlists and albums. The core keeps them in step with the server.
//! This holds the app's copy of their state, the Download switch, status text, the small
//! download marks and the turn-off dialog.

use std::collections::HashMap;

use gpui::prelude::*;
use gpui::{AnyElement, Context, SharedString, div};
use navia_core::downloads::{CollectionPhase, CollectionStatus, RemovalPlan};
use navia_core::library::collections::CollectionKind;

use crate::focus::FocusStop;
use crate::i18n::{fmt_int, tr, trf, trn};
use crate::shell::Shell;
use crate::theme::size::*;
use crate::ui::{DownloadGlyph, download_glyph, icon_button, icon_button_with};
use crate::view_connect::Confirm;

/// A downloaded collection as the Downloads page lists it, newest first.
#[derive(Clone, Debug)]
pub struct SyncedRow {
    pub kind: CollectionKind,
    pub id: SharedString,
    pub status: CollectionStatus,
}

/// The app's copy of the kept collections, reloaded when the core reports a change.
#[derive(Default)]
pub struct Synced {
    pub rows: Vec<SyncedRow>,
    by_key: HashMap<(CollectionKind, SharedString), usize>,
    pub generation: u64,
}

impl Synced {
    pub fn status(&self, kind: CollectionKind, id: &str) -> Option<&CollectionStatus> {
        self.by_key.get(&(kind, SharedString::from(id.to_string()))).and_then(|i| self.rows.get(*i)).map(|r| &r.status)
    }
}

/// The glyph for a status. Filled when done, gray while paused, a filling ring otherwise.
pub fn glyph_for(s: &CollectionStatus) -> DownloadGlyph {
    match s.phase {
        CollectionPhase::Done => DownloadGlyph::Done,
        CollectionPhase::Metered | CollectionPhase::Offline | CollectionPhase::Waiting => DownloadGlyph::Paused,
        CollectionPhase::Downloading | CollectionPhase::Syncing => DownloadGlyph::Running,
    }
}

/// How far along a collection is, counting songs in progress by their progress.
pub fn progress_of(s: &CollectionStatus) -> f32 {
    if s.total == 0 { 0.0 } else { (s.progress as f32).clamp(0.0, 1.0) }
}

/// The short line in a collection's header, after its song count.
pub fn status_text(s: &CollectionStatus) -> String {
    let progress = trf("{have} of {total}", &[("have", &fmt_int(s.have as i64)), ("total", &fmt_int(s.total as i64))]);
    match s.phase {
        CollectionPhase::Done => tr("Downloaded").to_string(),
        CollectionPhase::Syncing => trn("Downloading {count} new song", "Downloading {count} new songs", s.missing as usize),
        CollectionPhase::Downloading => trf("Downloading {progress}", &[("progress", &progress)]),
        CollectionPhase::Metered => trf("Paused on a metered connection, {progress}", &[("progress", &progress)]),
        CollectionPhase::Offline => trf("Waiting for the server, {progress}", &[("progress", &progress)]),
        CollectionPhase::Waiting => tr("Waiting to download").to_string(),
    }
}

impl Shell {
    /// Reads the kept collections and their status again, off the UI thread.
    pub fn refresh_synced(&mut self, cx: &mut Context<Self>) {
        let Some(core) = self.core.as_ref() else { return };
        let lib = core.lib.clone();
        self.synced.generation += 1;
        let generation = self.synced.generation;
        cx.spawn(async move |this, cx| {
            let rows = cx
                .background_spawn(async move {
                    let kept = lib.downloads.synced_collections().unwrap_or_default();
                    kept.into_iter()
                        .filter_map(|c| {
                            let status = lib.downloads.collection_status(c.kind, &c.id).ok().flatten()?;
                            Some(SyncedRow { kind: c.kind, id: c.id.into(), status })
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.synced.generation != generation {
                    return;
                }
                this.synced.by_key = rows.iter().enumerate().map(|(i, r)| ((r.kind, r.id.clone()), i)).collect();
                this.synced.rows = rows;
                this.invalidate_page();
                cx.notify();
            });
        })
        .detach();
    }

    pub fn collection_status(&self, kind: CollectionKind, id: &str) -> Option<&CollectionStatus> {
        self.synced.status(kind, id)
    }

    /// Whether the playlist or album on screen is kept downloaded and still missing songs.
    pub fn page_collection_pending(&self) -> bool {
        let status = match &self.route {
            crate::pages::Route::Album(id) => self.collection_status(CollectionKind::Album, id),
            crate::pages::Route::Playlist(id) => self.collection_status(CollectionKind::Playlist, id),
            _ => None,
        };
        status.is_some_and(|s| s.phase != CollectionPhase::Done)
    }

    /// Toggles the Download switch. Turning it off asks first when songs are on the device.
    pub fn toggle_collection_download(&mut self, kind: CollectionKind, id: SharedString, name: SharedString, cx: &mut Context<Self>) {
        let Some(core) = self.core.as_ref() else { return };
        let lib = core.lib.clone();
        if self.collection_status(kind, &id).is_none() {
            let key = id.to_string();
            cx.background_executor()
                .spawn(async move {
                    // A playlist whose songs this device hasn't fetched yet is fetched first.
                    if kind == CollectionKind::Playlist && lib.db.collection_needs_fetch(kind, &key).unwrap_or(false) {
                        lib.fetch_playlist_songs(&key);
                    }
                    if let Err(e) = lib.downloads.collection_download_on(kind, &key) {
                        eprintln!("navia: couldn't turn on a download: {e}");
                    }
                })
                .detach();
            return;
        }
        let plan = lib.downloads.removal_plan(kind, &id).unwrap_or_default();
        if plan.remove == 0 && plan.keep == 0 {
            self.stop_collection_download(kind, id, false, cx);
            return;
        }
        self.confirm = Some(Confirm::StopDownload { kind, id, name, plan });
        cx.notify();
    }

    /// Stops a collection download. Keep Songs leaves them as single downloads, Remove deletes
    /// what nothing else holds.
    pub fn stop_collection_download(&mut self, kind: CollectionKind, id: SharedString, keep_songs: bool, cx: &mut Context<Self>) {
        self.confirm = None;
        let Some(core) = self.core.as_ref() else { return };
        let lib = core.lib.clone();
        cx.background_executor()
            .spawn(async move {
                if let Err(e) = lib.downloads.collection_download_off(kind, &id, keep_songs) {
                    eprintln!("navia: couldn't turn off a download: {e}");
                }
            })
            .detach();
        cx.notify();
    }

    /// The Download switch in a playlist's or album's action pill.
    pub fn download_toggle(&mut self, key: &'static str, kind: CollectionKind, id: SharedString, name: SharedString, has_songs: bool, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let status = self.collection_status(kind, &id).cloned();
        let label = if !has_songs { tr("No songs to download") } else if status.is_some() { tr("Remove Download") } else { tr("Download") };
        let button = match &status {
            Some(s) => icon_button_with(&t, key, label, true, download_glyph(&t, glyph_for(s), progress_of(s), DOWNLOAD_GLYPH, t.t1().into())),
            None => icon_button(&t, key, label, "arrow-down-regular", false),
        };
        if !has_songs {
            return button.opacity(DISABLED_OPACITY).into_any_element();
        }
        button
            .stop(&t)
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_collection_download(kind, id.clone(), name.clone(), cx)))
            .into_any_element()
    }

    /// The download part of a collection's header line, like " • Downloaded". `None` while off.
    pub fn download_status_line(&self, kind: CollectionKind, id: &str) -> Option<String> {
        self.collection_status(kind, id).map(|s| format!(" • {}", status_text(s)))
    }

    /// The small mark on a sidebar row, card or list row while a collection is downloaded or downloading.
    /// Callers place it `DOWNLOAD_MARK_GAP` from the text.
    pub fn download_mark(&self, kind: CollectionKind, id: &str) -> Option<AnyElement> {
        let t = self.theme;
        let s = self.collection_status(kind, id)?;
        Some(div().flex_none().child(download_glyph(&t, glyph_for(s), progress_of(s), DOWNLOAD_MARK, t.t2().into())).into_any_element())
    }

    /// The turn-off dialog's text, saying what goes and what stays.
    pub fn stop_download_text(plan: &RemovalPlan) -> (String, Option<&'static str>) {
        let size = crate::view_downloads::format_size(plan.remove_bytes);
        let mut body = if plan.remove > 0 {
            crate::i18n::trnf("{count} song and {size} will be removed from this device.", "{count} songs and {size} will be removed from this device.", plan.remove as usize, &[("size", &size)])
        } else if plan.keep > 0 {
            tr("Every song in it is also kept by another download, so nothing is removed.").to_string()
        } else {
            tr("None of its songs are on this device yet.").to_string()
        };
        if plan.keep > 0 && plan.remove > 0 {
            body.push(' ');
            body.push_str(&trn("{count} song stays, because another download includes it.", "{count} songs stay, because another download includes them.", plan.keep as usize));
        }
        let note = (plan.remove > 0).then_some(tr("Keep Songs leaves them on this device as single songs. Either way, it stops staying in step with the server."));
        (body, note)
    }
}
