//! a playlist's right-click menu, and Delete Playlist with its dialog.

use gpui::{Context, Pixels, Point, SharedString};
use navia_core::library::collections::CollectionKind;

use crate::i18n::{tr, trf};
use crate::pages::Route;
use crate::shell::Shell;
use crate::view_connect::Confirm;
use crate::view_menu::MenuRow;

impl Shell {
    /// editing and deleting only where the user can edit (admins count).
    pub fn playlist_menu_rows(&self, id: &SharedString) -> Vec<MenuRow> {
        let Some(p) = self.playlists_full.iter().find(|p| &p.id == id).cloned() else { return Vec::new() };
        let has_songs = p.song_count > 0;
        let play = |label: &'static str, icon: &'static str, shuffle: bool| {
            if !has_songs {
                return MenuRow::disabled(label, icon, tr("No songs to play"));
            }
            let id = p.id.clone();
            MenuRow::item(label, icon, move |this, cx| this.play_playlist(id.clone(), shuffle, cx))
        };
        let download = if !has_songs {
            MenuRow::disabled(tr("Download"), "arrow-down-regular", tr("No songs to download"))
        } else {
            let (id, name) = (p.id.clone(), p.name.clone());
            let on = self.collection_status(CollectionKind::Playlist, &p.id).is_some();
            let (label, icon) = if on { (tr("Remove Download"), "x-regular") } else { (tr("Download"), "arrow-down-regular") };
            MenuRow::item(label, icon, move |this, cx| this.toggle_collection_download(CollectionKind::Playlist, id.clone(), name.clone(), cx))
        };
        // stored songs only: a right-click never asks the server for a playlist.
        let songs = self.library.as_ref().map(|l| l.stored_playlist_songs(id)).unwrap_or_default();
        let mut rows = vec![
            play(tr("Play"), "play-regular", false),
            play(tr("Shuffle"), "shuffle-regular", true),
            MenuRow::Divider,
            download,
            self.share_row(navia_core::shares::ShareKind::Playlist, p.id.clone()),
            self.add_to_playlist_row(tr("Add to Playlist"), songs, p.name.clone()),
            self.info_row(crate::view_info::InfoTarget::Playlist(p.id.clone())),
        ];
        rows.extend(self.pin_row(&p.id));
        if p.owned {
            rows.push(MenuRow::Divider);
            rows.push(self.edit_details_row(&p.id));
            rows.push(self.delete_playlist_row(&p.id));
        }
        rows
    }

    pub fn open_playlist_menu(&mut self, id: SharedString, position: Point<Pixels>, cx: &mut Context<Self>) {
        let rows = self.playlist_menu_rows(&id);
        if rows.is_empty() {
            return;
        }
        self.open_menu(rows, position, false, cx);
        if let Some(menu) = self.menu.as_mut() {
            menu.playlist = Some(id.clone());
        }
        // fetched as its page would be so Add to Playlist fills in; not in test and read-only runs, since fetching
        // a smart playlist changes it on the server.
        let Some(lib) = self.core.as_ref().map(|c| c.lib.clone()) else { return };
        if crate::core_library::read_only_run() {
            return;
        }
        let key = id.to_string();
        cx.spawn(async move |this, cx| {
            let fetched = cx
                .background_executor()
                .spawn(async move {
                    let needed = lib.db.collection_needs_fetch(CollectionKind::Playlist, &key).unwrap_or(false);
                    if needed {
                        lib.fetch_playlist_songs(&key);
                    }
                    needed
                })
                .await;
            if fetched {
                let _ = this.update(cx, |this, cx| {
                    if this.menu.as_ref().and_then(|m| m.playlist.as_ref()) == Some(&id) {
                        let rows = this.playlist_menu_rows(&id);
                        this.replace_menu_rows(rows, cx);
                    }
                });
            }
        })
        .detach();
    }

    pub fn edit_details_row(&self, id: &SharedString) -> MenuRow {
        let id = id.clone();
        MenuRow::item(tr("Edit Details\u{2026}"), "pencil-simple-regular", move |this, cx| this.open_edit_playlist(id.clone(), cx))
    }

    pub fn delete_playlist_row(&self, id: &SharedString) -> MenuRow {
        let id = id.clone();
        MenuRow::item(tr("Delete Playlist\u{2026}"), "trash-regular", move |this, cx| this.ask_delete_playlist(id.clone(), cx))
    }

    /// fetches its songs first when this device has none yet.
    pub fn play_playlist(&mut self, id: SharedString, shuffle: bool, cx: &mut Context<Self>) {
        let Some(lib) = self.core.as_ref().map(|c| c.lib.clone()) else { return };
        let key = id.to_string();
        let fetch = !crate::core_library::read_only_run();
        cx.spawn(async move |this, cx| {
            cx.background_executor().spawn(async move {
                if fetch && lib.db.collection_needs_fetch(CollectionKind::Playlist, &key).unwrap_or(false) {
                    lib.fetch_playlist_songs(&key);
                }
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                let Some(name) = this.playlists_full.iter().find(|p| p.id == id).map(|p| p.name.to_string()) else { return };
                let songs = this.library.as_ref().map(|l| l.stored_playlist_songs(&id)).unwrap_or_default();
                if songs.is_empty() {
                    return;
                }
                this.play_source = Some(navia_core::queue_order::QueueSource { name, radio: false, link: Some(navia_core::queue_order::SourceLink::Playlist(id.to_string())) });
                this.play_songs(songs, 0, shuffle, cx);
            });
        })
        .detach();
    }

    fn ask_delete_playlist(&mut self, id: SharedString, cx: &mut Context<Self>) {
        let Some(name) = self.playlists_full.iter().find(|p| p.id == id).map(|p| p.name.clone()) else { return };
        let removed = match (self.collection_status(CollectionKind::Playlist, &id), self.core.as_ref()) {
            (Some(_), Some(core)) => core.lib.downloads.removal_plan(CollectionKind::Playlist, &id).map_or(0, |p| p.remove as usize),
            _ => 0,
        };
        self.close_menu(cx);
        self.confirm = Some(Confirm::DeletePlaylist { id, name, removed });
        cx.notify();
    }

    pub fn delete_playlist(&mut self, id: SharedString, cx: &mut Context<Self>) {
        self.confirm = None;
        if let Some(core) = self.core.as_ref() {
            core.lib.delete_playlist(id.to_string());
        }
        cx.notify();
    }

    /// it leaves the sidebar, the Playlists page, the saved order and the pins; its page goes to Playlists.
    pub fn on_playlist_deleted(&mut self, id: String, result: Result<(), String>, cx: &mut Context<Self>) {
        if let Err(message) = result {
            self.show_toast(message, cx);
            return;
        }
        let mut forgot = false;
        for order in self.prefs.sidebar_orders.values_mut() {
            let before = order.len();
            order.retain(|o| *o != id);
            forgot |= order.len() != before;
        }
        for pins in self.prefs.sidebar_pins.values_mut() {
            let before = pins.pinned.len() + pins.unpinned.len();
            pins.pinned.retain(|p| *p != id);
            pins.unpinned.retain(|p| *p != id);
            forgot |= pins.pinned.len() + pins.unpinned.len() != before;
        }
        if forgot {
            self.prefs.save();
        }
        if let Some(library) = self.library.clone() {
            self.playlists = library.playlists();
            self.playlists_full = library.playlist_details();
            self.refresh_sidebar_order();
        }
        let on_page = matches!(&self.route, Route::Playlist(p) | Route::SmartEditor(Some(p)) if p.as_ref() == id);
        if on_page {
            self.navigate(Route::Playlists, cx);
        }
        self.invalidate_page();
        cx.notify();
    }

    /// what goes, for everyone and from this device.
    pub fn delete_playlist_text(name: &str, removed: usize) -> (String, String) {
        let mut body = tr("It\u{2019}s removed from the server for everyone.").to_string();
        if removed > 0 {
            body.push(' ');
            body.push_str(&crate::i18n::trn("Its {count} downloaded song is removed from this device too.", "Its {count} downloaded songs are removed from this device too.", removed));
        }
        (trf("Delete \u{201c}{name}\u{201d}?", &[("name", name)]), body)
    }
}
