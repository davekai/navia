//! The Playlists page's New menu, the New Playlist dialog, and the cover rows in a playlist's Edit menu. Covers only
//! come from the server, either an upload or Navidrome's album grid.

use crate::i18n::{tr, trf};
use std::path::PathBuf;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{Animation, AnimationExt, AnyElement, Context, RenderImage, SharedString, div};
use navia_core::covers::{COVER_EXPORT_PX, COVER_STYLES};

use crate::focus::FocusStop;
use crate::pages::Route;
use crate::shell::Shell;
use crate::theme::motion;
use crate::theme::size::*;
use crate::ui::icon;
use crate::ui_controls::{DialogButton, TextTarget, dialog_button, round_button, switch};
use crate::view_cards::ArtShape;
use crate::view_menu::MenuRow;

pub struct NewPlaylist {
    pub name: String,
    /// Set from Add to Playlist. The new playlist holds these songs and the app stays where it is.
    pub songs: Option<Vec<u32>>,
    /// Set from Save As in the smart playlist editor. These rules, in Navidrome's JSON, become a new smart playlist
    /// owned by the user.
    pub smart: Option<String>,
    pub creating: bool,
    pub error: Option<SharedString>,
    pub description: String,
    pub public: bool,
    /// only where this server takes playlist images; `None` is the plain name dialog.
    pub covers: Option<CoverRow>,
}

/// slots: the cover as it is (Edit Details only), the user's own picture, then the styles.
pub struct CoverRow {
    /// comes first and starts chosen.
    current: Option<SharedString>,
    pub slot: usize,
    /// and a count that restarts the slide.
    from: usize,
    moves: usize,
    /// the user's own picture and its preview.
    image: Option<(PathBuf, Arc<RenderImage>)>,
    /// drawn with `title` at `px` pixels.
    art: Vec<Arc<RenderImage>>,
    title: String,
    px: u32,
}

impl CoverRow {
    /// with `current`, it starts on the cover as it is; otherwise on the first style.
    pub fn new(current: Option<SharedString>) -> Self {
        let slot = if current.is_some() { 0 } else { 1 };
        Self { current, slot, from: slot, moves: 0, image: None, art: Vec::new(), title: String::new(), px: 0 }
    }

    fn upload_slot(&self) -> usize {
        usize::from(self.current.is_some())
    }

    fn slots(&self) -> usize {
        self.upload_slot() + 1 + COVER_STYLES.len()
    }

    /// none for the cover as it is, or the upload tile with no picture.
    pub fn pick(&self) -> Option<CoverPick> {
        let upload = self.upload_slot();
        match self.slot {
            slot if slot < upload => None,
            slot if slot == upload => self.image.as_ref().map(|(file, _)| CoverPick::File(file.clone())),
            slot => COVER_STYLES.get(slot - upload - 1).map(|style| CoverPick::Style(style.id)),
        }
    }

    pub fn images(self) -> Vec<Arc<RenderImage>> {
        self.art.into_iter().chain(self.image.map(|(_, image)| image)).collect()
    }
}

/// once the server has made it.
pub struct AfterCreate {
    description: String,
    public: bool,
    cover: Option<CoverPick>,
}

pub enum CoverPick {
    Style(&'static str),
    File(PathBuf),
}

/// `quiet`: its arrival needs no toast (a new playlist's first cover).
pub struct CoverUpload {
    id: String,
    file: Option<PathBuf>,
    quiet: bool,
}

impl Shell {
    /// in the shared thumbnail path.
    pub fn playlist_art_key(&self, id: &str) -> SharedString {
        SharedString::from(format!("pl:{id}"))
    }

    pub fn playlist_cover_rows(&self, id: &SharedString) -> Vec<MenuRow> {
        self.playlist_image_rows(id)
    }

    pub fn new_playlist_rows(&self) -> Vec<MenuRow> {
        vec![
            MenuRow::item(tr("New Playlist"), "playlist-regular", |this, cx| this.open_new_playlist(cx)),
            MenuRow::item(tr("New Smart Playlist"), "lightning-fill", |this, cx| this.navigate(Route::SmartEditor(None), cx)),
        ]
    }

    pub fn open_new_playlist(&mut self, cx: &mut Context<Self>) {
        self.open_new_playlist_with(None, String::new(), cx);
    }

    /// New Playlist from Add to Playlist, holding `songs` and named `name` to start.
    pub fn open_new_playlist_with(&mut self, songs: Option<Vec<u32>>, name: String, cx: &mut Context<Self>) {
        let covers = self.core.as_ref().is_some_and(|core| core.lib.can_change_playlist_images()).then(|| CoverRow::new(None));
        self.new_playlist = Some(NewPlaylist { name, songs, smart: None, creating: false, error: None, description: String::new(), public: false, covers });
        self.focus_field(TextTarget::NewPlaylistName, cx);
        cx.notify();
    }

    /// Save As. Names a copy of the editor's rules, saves it as a new smart playlist, and opens it.
    pub fn open_smart_save_as(&mut self, name: String, criteria: String, cx: &mut Context<Self>) {
        self.new_playlist = Some(NewPlaylist { name, songs: None, smart: Some(criteria), creating: false, error: None, description: String::new(), public: false, covers: None });
        self.focus_field(TextTarget::NewPlaylistName, cx);
        cx.notify();
    }

    pub fn new_playlist_submit(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.new_playlist.as_mut() else { return };
        let name = dialog.name.trim().to_string();
        if name.is_empty() || dialog.creating {
            return;
        }
        match self.core.as_ref() {
            Some(core) => {
                dialog.creating = true;
                dialog.error = None;
                self.new_playlist_after = dialog.covers.as_ref().map(|row| AfterCreate {
                    description: dialog.description.trim().to_string(),
                    public: dialog.public,
                    cover: row.pick(),
                });
                match (dialog.smart.clone(), dialog.songs.clone()) {
                    (Some(criteria), _) => core.lib.save_smart_playlist(None, name, criteria),
                    (None, Some(songs)) => core.lib.create_playlist_with(name, songs),
                    (None, None) => core.lib.create_playlist(name),
                }
            }
            None => dialog.error = Some(tr("Creating a playlist needs the server.").into()),
        }
        cx.notify();
    }

    /// its pictures are freed with the next frame's garbage.
    pub fn close_new_playlist(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(dialog) = self.new_playlist.take() else { return false };
        if let Some(row) = dialog.covers {
            self.lyrics_view.garbage.extend(row.images());
        }
        self.controls.text = None;
        cx.notify();
        true
    }

    /// sent now that the playlist exists.
    fn finish_new_playlist(&mut self, id: &str, name: &str) {
        let (Some(after), Some(lib)) = (self.new_playlist_after.take(), self.core.as_ref().map(|core| core.lib.clone())) else { return };
        if id.is_empty() {
            return;
        }
        let details = navia_core::client::PlaylistDetails { name: None, comment: (!after.description.is_empty()).then_some(after.description), public: after.public.then_some(true) };
        if details != navia_core::client::PlaylistDetails::default() {
            lib.update_playlist_details(id.to_string(), details);
        }
        if let Some(pick) = after.cover {
            self.upload_cover_pick(id, name, pick, true);
        }
    }

    /// the picked file, or a style drawn with `name` at the upload size.
    pub fn upload_cover_pick(&mut self, id: &str, name: &str, pick: CoverPick, quiet: bool) {
        let Some(lib) = self.core.as_ref().map(|core| core.lib.clone()) else { return };
        match pick {
            CoverPick::File(file) => {
                self.cover_upload = Some(CoverUpload { id: id.to_string(), file: None, quiet });
                lib.change_playlist_image(id.to_string(), Some(file));
            }
            CoverPick::Style(style) => {
                let file = std::env::temp_dir().join(format!("navia-cover-{}-{id}.png", std::process::id()));
                self.cover_upload = Some(CoverUpload { id: id.to_string(), file: Some(file.clone()), quiet });
                let (id, name) = (id.to_string(), name.to_string());
                // drawing and encoding takes a moment, so off the window's thread.
                std::thread::spawn(move || {
                    if let Some(png) = navia_core::covers::render_cover_png(style, &name, COVER_EXPORT_PX)
                        && std::fs::write(&file, png).is_ok()
                    {
                        lib.change_playlist_image(id, Some(file));
                    }
                });
            }
        }
    }

    /// removes the drawn file; true when its arrival needs no toast. without `id`, whichever is under way.
    pub fn take_cover_upload(&mut self, id: Option<&str>) -> bool {
        if self.cover_upload.as_ref().is_none_or(|upload| id.is_some_and(|id| upload.id != id)) {
            return false;
        }
        let Some(upload) = self.cover_upload.take() else { return false };
        if let Some(file) = upload.file {
            let _ = std::fs::remove_file(file);
        }
        upload.quiet
    }

    fn cover_row(&self) -> Option<&CoverRow> {
        self.new_playlist.as_ref().and_then(|d| d.covers.as_ref()).or_else(|| self.edit_playlist.as_ref().and_then(|d| d.covers.as_ref()))
    }

    fn cover_row_mut(&mut self) -> Option<&mut CoverRow> {
        match self.new_playlist.as_mut() {
            Some(dialog) => dialog.covers.as_mut(),
            None => self.edit_playlist.as_mut().and_then(|d| d.covers.as_mut()),
        }
    }

    pub fn on_playlist_created(&mut self, id: String, name: String, with_songs: bool, cx: &mut Context<Self>) {
        self.close_new_playlist(cx);
        self.finish_new_playlist(&id, &name);
        if let Some(library) = self.library.clone() {
            self.playlists = library.playlists();
            self.refresh_sidebar_order();
            self.load_catalog(&library);
        }
        if std::mem::take(&mut self.saving_mix) {
            self.show_toast(trf("Saved as {name}", &[("name", &name)]), cx);
            cx.notify();
            return;
        }
        if with_songs {
            self.show_toast(trf("Added to {name}", &[("name", &name)]), cx);
            cx.notify();
            return;
        }
        self.show_toast(trf("Created {name}", &[("name", &name)]), cx);
        if !id.is_empty() {
            self.navigate(Route::Playlist(id.into()), cx);
        }
        cx.notify();
    }

    pub fn on_playlist_create_failed(&mut self, message: String, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.saving_mix) {
            self.show_toast(message.clone(), cx);
        }
        self.new_playlist_after = None;
        if let Some(dialog) = self.new_playlist.as_mut() {
            dialog.creating = false;
            dialog.error = Some(message.into());
        }
        cx.notify();
    }

    fn set_cover_slot(&mut self, slot: usize, cx: &mut Context<Self>) {
        let Some(row) = self.cover_row_mut() else { return };
        let slot = slot.min(row.slots() - 1);
        if slot != row.slot {
            row.from = row.slot;
            row.slot = slot;
            row.moves += 1;
            cx.notify();
        }
    }

    /// the upload tile's slot presses it.
    pub fn script_cover_slot(&mut self, slot: usize, cx: &mut Context<Self>) {
        if self.cover_row().is_some_and(|row| slot == row.upload_slot()) { self.pick_cover_row_image(cx) } else { self.set_cover_slot(slot, cx) }
    }

    /// while no field has the keyboard.
    pub fn cover_row_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let Some((slot, slots)) = self.cover_row().map(|row| (row.slot, row.slots())) else { return false };
        let to = match key {
            "left" | "up" => slot.saturating_sub(1),
            "right" | "down" => slot + 1,
            "home" => 0,
            "end" => slots - 1,
            _ => return false,
        };
        self.set_cover_slot(to, cx);
        true
    }

    /// opens the file picker when it's already chosen or still empty.
    fn pick_cover_row_image(&mut self, cx: &mut Context<Self>) {
        let Some(row) = self.cover_row() else { return };
        let upload = row.upload_slot();
        let open = row.slot == upload || row.image.is_none();
        self.set_cover_slot(upload, cx);
        if !open {
            return;
        }
        // a named file stands in for the picker in a read-only run.
        if crate::core_library::read_only_run()
            && let Some(file) = std::env::var_os("NAVIA_TEST_COVER_FILE").map(PathBuf::from)
        {
            self.set_cover_row_image(file, cx);
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
            let _ = this.update(cx, |this, cx| this.set_cover_row_image(path, cx));
        })
        .detach();
    }

    fn set_cover_row_image(&mut self, file: PathBuf, cx: &mut Context<Self>) {
        let px = self.cover_pick_px();
        let Ok(picture) = image::open(&file) else {
            self.show_toast(tr("That file isn't a picture Navia can read."), cx);
            return;
        };
        let preview = crate::art::thumbnail(&picture.to_rgba8(), px);
        let Some(row) = self.cover_row_mut() else { return };
        let upload = row.upload_slot();
        if let Some((_, old)) = row.image.replace((file, preview)) {
            self.lyrics_view.garbage.push(old);
        }
        self.set_cover_slot(upload, cx);
        cx.notify();
    }

    /// in device pixels.
    fn cover_pick_px(&self) -> u32 {
        (f32::from(self.theme.s(COVER_PICK_SIZE)) * self.window_scale).round().max(1.0) as u32
    }

    /// when the title or the size changed.
    fn refresh_cover_art(&mut self) {
        let px = self.cover_pick_px();
        let name = match (self.new_playlist.as_ref(), self.edit_playlist.as_ref()) {
            (Some(dialog), _) => dialog.name.trim(),
            (None, Some(dialog)) => dialog.name.trim(),
            (None, None) => return,
        };
        let title = if name.is_empty() { tr("Playlist Name").to_string() } else { name.to_string() };
        let Some(row) = self.cover_row_mut() else { return };
        if row.px == px && row.title == title && !row.art.is_empty() {
            return;
        }
        let art = COVER_STYLES
            .iter()
            .filter_map(|style| navia_core::covers::render_cover_rgba(style.id, &title, px))
            .filter_map(|rgba| image::RgbaImage::from_raw(px, px, rgba))
            .map(crate::art::into_render_image)
            .collect();
        let old = std::mem::replace(&mut row.art, art);
        row.title = title;
        row.px = px;
        self.lyrics_view.garbage.extend(old);
    }

    /// with its arrow buttons and the pager under it.
    pub fn render_cover_row(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.refresh_cover_art();
        let t = self.theme;
        let current = self.cover_row()?.current.clone();
        // the cover as the sidebar and the playlist's page show it.
        let px = self.cover_pick_px();
        let current = current.map(|key| {
            let image = self.thumb_faded(&key, px);
            self.artwork_el(image, "playlist-regular", COVER_PICK_SIZE, ArtShape::Rounded(R_MD))
        });
        let row = self.cover_row()?;
        let (slot, from, moves, upload_slot, slots) = (row.slot, row.from, row.moves, row.upload_slot(), row.slots());
        let (side, gap, ring) = (t.s(COVER_PICK_SIZE), t.s(COVER_PICK_GAP), t.s(COVER_PICK_RING_W + COVER_PICK_RING_GAP));
        let radius = t.s(R_MD);
        let chosen = |el: gpui::Stateful<gpui::Div>, on: bool| {
            el.when(on, |el| el.child(div().absolute().top(-ring).left(-ring).size(side + ring * 2.0).rounded(radius + ring).border(t.s(COVER_PICK_RING_W)).border_color(t.t1())))
        };
        let upload = match row.image.as_ref() {
            Some((_, preview)) => div().id("cover-upload").child(crate::ui::image(preview.clone()).size(side).rounded(radius)),
            None => div()
                .id("cover-upload")
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(t.s(COVER_PICK_UPLOAD_GAP))
                .bg(t.glass_fill())
                .shadow(crate::ui::glass_edge())
                .text_size(t.text(TEXT_SM))
                .text_color(t.t2())
                .child(icon(&t, "image-regular", COVER_PICK_UPLOAD_ICON, t.t2()))
                .child(tr("Upload Image")),
        };
        let upload = chosen(upload.relative().flex_none().size(side).rounded(radius).cursor_pointer(), slot == upload_slot)
            .on_click(cx.listener(|this, _, _, cx| this.pick_cover_row_image(cx)));
        let mut covers = div().absolute().top(ring).flex().flex_row().gap(gap);
        if let Some(art) = current {
            let cover = div().id("cover-current").relative().flex_none().size(side).rounded(radius).cursor_pointer().child(art);
            covers = covers.child(chosen(cover, slot == 0).on_click(cx.listener(|this, _, _, cx| {
                this.controls.text = None;
                this.set_cover_slot(0, cx);
            })));
        }
        covers = covers.child(upload);
        for (i, art) in row.art.iter().enumerate() {
            let at = upload_slot + 1 + i;
            let cover = div().id(("cover-style", i)).relative().flex_none().size(side).rounded(radius).cursor_pointer().child(crate::ui::image(art.clone()).size(side).rounded(radius));
            covers = covers.child(chosen(cover, slot == at).on_click(cx.listener(move |this, _, _, cx| {
                this.controls.text = None;
                this.set_cover_slot(at, cx);
            })));
        }
        // the chosen cover sits in the middle; the row slides there.
        let left = |at: usize| (t.s(DIALOG_W) - side) / 2.0 - (side + gap) * at as f32;
        let (start, end) = (left(from), left(slot));
        let covers = covers.with_animation(("cover-row", moves), Animation::new(motion::duration(motion::TOGGLE_FADE)).with_easing(gpui::ease_in_out), move |el, delta| el.left(start + (end - start) * delta));
        let stage_h = side + ring * 2.0;
        let arrow = |name: &'static str, id: &'static str, label: &'static str, to: usize, cx: &mut Context<Self>| {
            round_button(&t, id, tr(label), name).absolute().top((stage_h - t.s(CONTROL_H)) / 2.0).opacity(0.0).group_hover("cover-stage", |s| s.opacity(1.0)).on_click(cx.listener(move |this, _, _, cx| {
                this.controls.text = None;
                this.set_cover_slot(to, cx);
            }))
        };
        let mut stage = div().group("cover-stage").relative().h(stage_h).mx(-t.s(DIALOG_PAD)).overflow_hidden().child(covers);
        if slot > 0 {
            stage = stage.child(arrow("caret-left-bold", "cover-prev", "Previous cover", slot - 1, cx).left(t.s(COVER_PICK_ARROW_INSET)));
        }
        if slot < slots - 1 {
            stage = stage.child(arrow("caret-right-bold", "cover-next", "Next cover", slot + 1, cx).right(t.s(COVER_PICK_ARROW_INSET)));
        }
        let mut pager = div().flex().flex_row().items_center().justify_center().gap(t.s(COVER_PICK_DOT_GAP));
        for i in 0..slots {
            let ink = if i == slot { t.t1() } else { t.t3() };
            let dot = if i == upload_slot { div().id(("cover-dot", i)).child(icon(&t, "image-regular", COVER_PICK_PAGER_ICON, ink)) } else { div().id(("cover-dot", i)).size(t.s(COVER_PICK_DOT)).rounded_full().bg(ink) };
            pager = pager.child(dot.flex_none().cursor_pointer().on_click(cx.listener(move |this, _, _, cx| if i == upload_slot { this.pick_cover_row_image(cx) } else { this.set_cover_slot(i, cx) })));
        }
        Some(div().flex().flex_col().gap(t.s(COVER_PICK_PAGER_TOP)).child(stage).child(pager).into_any_element())
    }

    pub fn render_new_playlist(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = self.theme;
        let cover_row = self.render_cover_row(cx);
        let dialog = self.new_playlist.as_ref()?;
        let (name, creating, error, smart) = (dialog.name.clone(), dialog.creating, dialog.error.clone(), dialog.smart.is_some());
        let (description, public, covers) = (dialog.description.clone(), dialog.public, dialog.covers.is_some());
        let can_create = !name.trim().is_empty() && !creating;
        let line = self.field_line(&TextTarget::NewPlaylistName, &name, tr("Playlist Name"), "new-playlist-name", t.text(TEXT_MD));
        let focused = self.focused_target() == Some(TextTarget::NewPlaylistName);
        let field = crate::ui::dialog_field(&t, "new-playlist-field", focused, None, line)
            .on_click(cx.listener(|this, _, _, cx| this.focus_field(TextTarget::NewPlaylistName, cx)));
        let description_line = self.field_line(&TextTarget::NewPlaylistDescription, &description, tr("Optional"), "new-playlist-description", t.text(TEXT_MD));
        let description_focused = self.focused_target() == Some(TextTarget::NewPlaylistDescription);
        let description_field = crate::ui::dialog_field(&t, "new-playlist-description-field", description_focused, None, description_line)
            .on_click(cx.listener(|this, _, _, cx| this.focus_field(TextTarget::NewPlaylistDescription, cx)));
        let toggle = switch(&t, "new-playlist-public", public, true, cx, |this, cx| {
            if let Some(dialog) = this.new_playlist.as_mut() {
                dialog.public = !dialog.public;
                cx.notify();
            }
        });
        let close = |this: &mut Shell, cx: &mut Context<Shell>| {
            this.close_new_playlist(cx);
        };
        let cancel = dialog_button(&t, "new-playlist-cancel", DialogButton::Ghost, true).child(tr("Cancel")).stop_top(&t).on_click(cx.listener(move |this, _, _, cx| close(this, cx)));
        let create = dialog_button(&t, "new-playlist-create", DialogButton::Primary, can_create).child(match (smart, creating) {
            (true, true) => tr("Saving\u{2026}"),
            (true, false) => tr("Save"),
            (false, true) => tr("Creating\u{2026}"),
            (false, false) => tr("Create"),
        }).stop_top(&t).on_click(cx.listener(|this, _, _, cx| this.new_playlist_submit(cx)));
        Some(
            crate::ui::dialog_scrim(&t, "new-playlist-scrim")
                .on_mouse_down(gpui::MouseButton::Left, cx.listener(move |this, _, _, cx| close(this, cx)))
                .child(
                    crate::ui::dialog_panel(&t, "new-playlist")
                        .child(crate::ui::dialog_title(&t, if smart { tr("Save As") } else { tr("New Playlist") }))
                        .when(smart, |d| d.child(crate::ui::dialog_text(&t, tr("Saves these rules as a new smart playlist of your own. The original stays as it is."))))
                        .children(cover_row)
                        .child(crate::ui::dialog_block(&t, tr("Name"), field))
                        .when(covers, |d| d.child(crate::ui::dialog_block(&t, tr("Description"), description_field)).child(crate::ui::dialog_switch(&t, tr("Public"), tr("Other people on this server can see it."), toggle)))
                        .children(error.map(|e| crate::ui::error_text(&t, e)))
                        .child(crate::ui::dialog_actions(&t).child(cancel).child(create)),
                )
                .into_any_element(),
        )
    }
}
