//! Uploading and removing a playlist's own image. The server stores it, so it shows on every device. Only offered where
//! the server allows it, on playlists the user owns.

use crate::i18n::{tr, trf};
use std::path::PathBuf;

use gpui::{Context, SharedString};

use crate::shell::Shell;
use crate::view_menu::MenuRow;

impl Shell {
    /// The Upload and Remove rows, or none when the server can't do this or in sample mode.
    pub fn playlist_image_rows(&self, id: &SharedString) -> Vec<MenuRow> {
        if !self.core.as_ref().is_some_and(|core| core.lib.can_change_playlist_images()) {
            return Vec::new();
        }
        let (upload_id, remove_id) = (id.clone(), id.clone());
        vec![
            MenuRow::item(tr("Upload Image"), "upload-regular", move |this, cx| this.pick_playlist_image(upload_id.to_string(), cx)),
            MenuRow::item(tr("Remove Image"), "trash-regular", move |this, cx| this.remove_playlist_image(remove_id.to_string(), cx)),
        ]
    }

    /// Opens the system file picker. `NAVIA_TEST_COVER_FILE` stands in for it in sessions without a portal, but only in
    /// a read-only run where the write gate still applies.
    pub fn pick_playlist_image(&mut self, id: String, cx: &mut Context<Self>) {
        if crate::core_library::read_only_run()
            && let Some(file) = std::env::var_os("NAVIA_TEST_COVER_FILE").map(PathBuf::from)
        {
            self.upload_playlist_image(id, file, cx);
            return;
        }
        let receiver = cx.prompt_for_paths(gpui::PathPromptOptions { files: true, directories: false, multiple: false, prompt: None });
        cx.spawn(async move |this, cx| {
            let path = match receiver.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Err(error)) => {
                    let _ = this.update(cx, |this, cx| this.show_toast(trf("Couldn't open a file picker: {error}", &[("error", &error.to_string())]), cx));
                    None
                }
                _ => None,
            };
            let Some(path) = path else { return };
            let _ = this.update(cx, |this, cx| this.upload_playlist_image(id, path, cx));
        })
        .detach();
    }

    fn upload_playlist_image(&mut self, id: String, file: PathBuf, cx: &mut Context<Self>) {
        if let Some(core) = self.core.as_ref() {
            core.lib.change_playlist_image(id, Some(file));
            self.show_toast(tr("Uploading image"), cx);
        }
    }

    pub fn remove_playlist_image(&mut self, id: String, cx: &mut Context<Self>) {
        if let Some(core) = self.core.as_ref() {
            core.lib.change_playlist_image(id, None);
            self.show_toast(tr("Removing image"), cx);
        }
    }

    /// The server has a new cover for this playlist. Drops the decoded copies so they reload. The core has already
    /// stored the new cover id.
    pub fn on_playlist_image_changed(&mut self, id: String, removed: bool, cx: &mut Context<Self>) {
        self.thumbs.forget(&format!("pl:{id}"));
        self.forget_playlist_tint(&format!("pl:{id}"));
        if let Some(library) = self.library.clone() {
            self.playlists = library.playlists();
            self.playlists_full = library.playlist_details();
        }
        self.invalidate_page();
        if self.take_cover_upload(Some(&id)) {
            cx.notify();
            return;
        }
        self.show_toast(if removed { tr("Removed the image") } else { tr("Updated the cover") }, cx);
    }
}
