//! A playlist's Edit Details dialog and the Public toggle in its share menu. Both use `updatePlaylist`. The toggle
//! updates at once and reverts with a toast if the server refuses.

use std::collections::HashMap;
use std::sync::Arc;

use crate::i18n::tr;
use gpui::prelude::*;
use gpui::{AnyElement, Context, SharedString};

use crate::focus::FocusStop;
use crate::shell::Shell;
use crate::theme::size::*;
use crate::ui_controls::{DialogButton, TextTarget, dialog_button, switch};
use crate::view_new_playlist::CoverRow;

pub struct EditPlaylist {
    pub id: SharedString,
    pub name: String,
    pub description: String,
    pub public: bool,
    /// The values the dialog opened with, so only changed fields are sent.
    original: (String, String, bool),
    pub saving: bool,
    pub error: Option<SharedString>,
    /// the cover row, starting on the cover as it is. `None` where the image can't be changed.
    pub covers: Option<CoverRow>,
}

/// Public toggles sent but not answered yet, with the value to revert to if the server refuses.
#[derive(Default)]
pub struct PublicPending(pub HashMap<SharedString, bool>);

impl Shell {
    pub fn open_edit_playlist(&mut self, id: SharedString, cx: &mut Context<Self>) {
        let Some(p) = self.playlists_full.iter().find(|p| p.id == id) else { return };
        let (name, description, public) = (p.name.to_string(), p.description.to_string(), p.public);
        let covers = self.core.as_ref().is_some_and(|core| core.lib.can_change_playlist_images()).then(|| CoverRow::new(Some(self.playlist_art_key(&id))));
        self.edit_playlist = Some(EditPlaylist { id, name: name.clone(), description: description.clone(), public, original: (name, description, public), saving: false, error: None, covers });
        self.focus_field(TextTarget::PlaylistName, cx);
        cx.notify();
    }

    pub fn close_edit_playlist(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(dialog) = self.edit_playlist.take() else { return false };
        if let Some(row) = dialog.covers {
            self.lyrics_view.garbage.extend(row.images());
        }
        self.controls.text = None;
        cx.notify();
        true
    }

    pub fn edit_playlist_text(&self, target: &TextTarget) -> String {
        let Some(d) = self.edit_playlist.as_ref() else { return String::new() };
        if *target == TextTarget::PlaylistName { d.name.clone() } else { d.description.clone() }
    }

    pub fn set_edit_playlist_text(&mut self, target: &TextTarget, value: String, cx: &mut Context<Self>) {
        let Some(d) = self.edit_playlist.as_mut() else { return };
        if *target == TextTarget::PlaylistName {
            d.name = value;
        } else {
            d.description = value;
        }
        d.error = None;
        cx.notify();
    }

    pub fn edit_playlist_submit(&mut self, cx: &mut Context<Self>) {
        let Some(d) = self.edit_playlist.as_mut() else { return };
        let (name, description) = (d.name.trim().to_string(), d.description.trim().to_string());
        if name.is_empty() || d.saving {
            return;
        }
        let details = navia_core::client::PlaylistDetails {
            name: (name != d.original.0).then_some(name),
            comment: (description != d.original.1).then_some(description),
            public: (d.public != d.original.2).then_some(d.public),
        };
        // the image changes only when another cover than the one it has was chosen.
        let cover = d.covers.as_ref().and_then(|row| row.pick());
        let unchanged = details == navia_core::client::PlaylistDetails::default();
        if unchanged && cover.is_none() {
            self.close_edit_playlist(cx);
            return;
        }
        let Some(lib) = self.core.as_ref().map(|core| core.lib.clone()) else {
            d.error = Some(tr("Editing a playlist needs the server.").into());
            cx.notify();
            return;
        };
        let id = d.id.to_string();
        if !unchanged {
            d.saving = true;
            d.error = None;
            lib.update_playlist_details(id.clone(), details);
        }
        if let Some(pick) = cover {
            let title = d.name.trim().to_string();
            self.upload_cover_pick(&id, &title, pick, false);
        }
        if unchanged {
            self.close_edit_playlist(cx);
        }
        cx.notify();
    }

    /// Toggles Public from the share menu. Shown at once, then sent to the server.
    pub fn toggle_playlist_public(&mut self, id: SharedString, cx: &mut Context<Self>) {
        let Some(lib) = self.core.as_ref().map(|c| c.lib.clone()) else { return };
        let Some(was) = self.playlists_full.iter().find(|p| p.id == id).map(|p| p.public) else { return };
        self.set_shown_public(&id, !was);
        self.public_pending.0.entry(id.clone()).or_insert(was);
        lib.update_playlist_details(id.to_string(), navia_core::client::PlaylistDetails { public: Some(!was), ..Default::default() });
        self.invalidate_page();
        cx.notify();
    }

    fn set_shown_public(&mut self, id: &SharedString, public: bool) {
        let mut list = self.playlists_full.as_ref().clone();
        if let Some(p) = list.iter_mut().find(|p| &p.id == id) {
            p.public = public;
        }
        self.playlists_full = Arc::new(list);
    }

    pub fn on_playlist_details_saved(&mut self, id: String, result: Result<(), String>, cx: &mut Context<Self>) {
        let id = SharedString::from(id);
        let revert = self.public_pending.0.remove(&id);
        let dialog_open = self.edit_playlist.as_ref().is_some_and(|d| d.id == id && d.saving);
        match result {
            Ok(()) => {
                if dialog_open {
                    self.close_edit_playlist(cx);
                }
                if let Some(library) = self.library.clone() {
                    self.playlists = library.playlists();
                    self.playlists_full = library.playlist_details();
                    self.refresh_sidebar_order();
                }
            }
            Err(message) => {
                if dialog_open && let Some(d) = self.edit_playlist.as_mut() {
                    d.saving = false;
                    d.error = Some(message.into());
                } else {
                    if let Some(was) = revert {
                        self.set_shown_public(&id, was);
                    }
                    self.show_toast(message, cx);
                }
            }
        }
        self.invalidate_page();
        cx.notify();
    }

    pub fn render_edit_playlist(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = self.theme;
        let cover_row = if self.edit_playlist.is_some() { self.render_cover_row(cx) } else { None };
        let d = self.edit_playlist.as_ref()?;
        let (name, description, public, saving, error) = (d.name.clone(), d.description.clone(), d.public, d.saving, d.error.clone());
        let empty = name.trim().is_empty();
        let name_line = self.field_line(&TextTarget::PlaylistName, &name, "", "edit-playlist-name", t.text(TEXT_MD));
        let name_focused = self.focused_target() == Some(TextTarget::PlaylistName);
        let name_field = crate::ui::dialog_field(&t, "edit-playlist-name-field", name_focused, None, name_line).on_click(cx.listener(|this, _, _, cx| this.focus_field(TextTarget::PlaylistName, cx)));
        let description_line = self.field_line(&TextTarget::PlaylistDescription, &description, tr("Optional"), "edit-playlist-description", t.text(TEXT_MD));
        let description_focused = self.focused_target() == Some(TextTarget::PlaylistDescription);
        let description_field = crate::ui::dialog_field(&t, "edit-playlist-description-field", description_focused, None, description_line).on_click(cx.listener(|this, _, _, cx| this.focus_field(TextTarget::PlaylistDescription, cx)));
        let toggle = switch(&t, "edit-playlist-public", public, true, cx, |this, cx| {
            if let Some(d) = this.edit_playlist.as_mut() {
                d.public = !d.public;
                cx.notify();
            }
        });
        let close = |this: &mut Shell, cx: &mut Context<Shell>| {
            this.close_edit_playlist(cx);
        };
        let cancel = dialog_button(&t, "edit-playlist-cancel", DialogButton::Ghost, true).child(tr("Cancel")).stop_top(&t).on_click(cx.listener(move |this, _, _, cx| close(this, cx)));
        let save = dialog_button(&t, "edit-playlist-save", DialogButton::Primary, !empty && !saving)
            .child(if saving { tr("Saving\u{2026}") } else { tr("Save") })
            .stop_top(&t)
            .on_click(cx.listener(|this, _, _, cx| this.edit_playlist_submit(cx)));
        Some(
            crate::ui::dialog_scrim(&t, "edit-playlist-scrim")
                .on_mouse_down(gpui::MouseButton::Left, cx.listener(move |this, _, _, cx| close(this, cx)))
                .child(
                    crate::ui::dialog_panel(&t, "edit-playlist")
                        .child(crate::ui::dialog_title(&t, tr("Edit Details")))
                        .children(cover_row)
                        .child(crate::ui::dialog_block(&t, tr("Name"), name_field))
                        .child(crate::ui::dialog_block(&t, tr("Description"), description_field))
                        .child(crate::ui::dialog_switch(&t, tr("Public"), tr("Other people on this server can see it."), toggle))
                        .children(error.map(|e| crate::ui::error_text(&t, e)))
                        .child(crate::ui::dialog_actions(&t).child(cancel).child(save)),
                )
                .into_any_element(),
        )
    }
}
