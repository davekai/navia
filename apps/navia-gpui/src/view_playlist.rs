use crate::ui::FullTextTip;
use crate::i18n::{tr, trf, trn};
use gpui::prelude::*;
use navia_core::library::collections::CollectionKind;
use crate::focus::FocusStop;
use gpui::{AnyElement, Context, FontWeight, ScrollHandle, SharedString, div, point, px, relative};

use crate::model::PlaylistDetails;
use crate::pages::{Block, PageMetrics, RowVariant};
use crate::shell::{Layout, Shell};
use crate::theme::size::*;
use crate::ui::{divider, glass_edge, glass_pill, icon, icon_button};
use crate::view_album::{SongFilter, pill_group};
use crate::view_cards::songs_and_length;
use crate::view_menu::{MenuPanel, MenuRow};
use crate::smart_model::{Say, summarize_criteria};
use crate::ui_controls::track_bounds;
use crate::view_rows::SortColumn;

/// The table's Remove from This Playlist, when the user can remove songs.
#[derive(Clone)]
pub struct PlaylistRemove {
    pub list: usize,
    pub id: SharedString,
    pub name: SharedString,
    /// Each shown row's index in the playlist's stored list of library songs.
    pub places: std::rc::Rc<Vec<usize>>,
}

pub struct PlaylistDetail {
    pub filter: SongFilter,
    pub query: String,
}

impl Default for PlaylistDetail {
    fn default() -> Self {
        Self { filter: SongFilter::All, query: String::new() }
    }
}

impl Shell {
    pub fn build_playlist(&mut self, id: &SharedString, layout: &Layout) -> Vec<Block> {
        let Some(p) = self.playlists_full.iter().find(|p| &p.id == id).cloned() else { return vec![Self::not_found(tr("This playlist isn't here"))] };
        let stored = self.library.as_ref().map(|l| l.playlist_songs(id)).unwrap_or_default();
        let stored_count = self.library.as_ref().map_or(0, |l| l.playlist_entry_count(id));
        let saving = self.playlist_order_override.contains_key(id);
        let all = self.playlist_order_override.get(id).cloned().unwrap_or(stored);
        // Songs can only be moved on playlists this user can edit, in playlist order, unfiltered. Every stored song
        // must be in the library since a move saves the whole list, and no save may be pending.
        let editable = !p.smart && !p.readonly && p.owned;
        let narrowed = !self.playlist_detail.query.trim().is_empty() || self.playlist_detail.filter != SongFilter::All;
        let column_sort = self.playlist_sort(id);
        let sorted = column_sort.is_some();
        let complete = all.len() == stored_count && !saving;
        let needle = self.playlist_detail.query.trim().to_lowercase();
        let filter = self.playlist_detail.filter;
        // Each row keeps its stored index, so removing works while sorted or filtered.
        let mut shown: Vec<(usize, u32)> = all
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, s)| {
                let song = self.song_peek(*s);
                let ok = match filter {
                    SongFilter::All => true,
                    SongFilter::Favorites => self.is_starred(*s),
                    SongFilter::Downloaded => self.is_downloaded(*s),
                };
                ok && (needle.is_empty() || song.is_some_and(|song| format!("{} {} {}", song.title, song.artist, song.album).to_lowercase().contains(&needle)))
            })
            .collect();
        if let Some(sort) = column_sort {
            self.sort_places(&mut shown, sort);
        }
        let (places, shown): (Vec<usize>, Vec<u32>) = shown.into_iter().unzip();
        self.core_preload_rows(&all);
        let list = self.add_list(all.clone());
        let table = self.add_list(shown.clone());
        let can_move = editable && complete && !sorted && !narrowed && shown.len() > 1;
        self.reorder_list = can_move.then_some(table);
        // Rows are removed by index, so not while a new order is still saving.
        self.playlist_remove = (editable && !saving).then(|| PlaylistRemove { list: table, id: p.id.clone(), name: p.name.clone(), places: std::rc::Rc::new(places) });
        let note = (editable && all.len() > 1 && (sorted || narrowed)).then_some(sorted);
        let head_p = p.clone();
        let head = Shell::plain_block(move |this, layout, _, cx| this.render_playlist_head(&head_p, layout, cx));
        let actions_p = p.clone();
        let actions = Shell::plain_block(move |this, layout, _, cx| this.render_playlist_actions(&actions_p, list, layout, cx));
        let mut blocks = vec![head, Block::Spacer(28.0), actions];
        if let Some(sorted) = note {
            // Equal space between the pills and the column labels. The table follows the note directly.
            blocks.push(Block::Spacer(ORDER_NOTE_GAP));
            blocks.push(Shell::plain_block(move |this, layout, _, cx| this.render_order_note(sorted, layout, cx)));
        } else {
            blocks.push(Block::Spacer(32.0));
        }
        if shown.is_empty() {
            let (empty_all, smart, owned, owner) = (all.is_empty(), p.smart, p.owned, p.owner.clone());
            blocks.push(Shell::plain_block(move |this, _, _, cx| {
                if empty_all {
                    let hint = if smart {
                        tr("No songs match its rules right now.").to_string()
                    } else if owned {
                        tr("Add songs from any song menu.").to_string()
                    } else {
                        trf("{owner} hasn't added any songs yet.", &[("owner", &owner)])
                    };
                    this.empty_state("music-notes-regular", tr("This playlist is empty"), hint, true).into_any_element()
                } else {
                    this.empty_state("magnifying-glass-regular", tr("No songs match"), tr("Nothing in this playlist matches your search or filter."), true).child(this.clear_playlist_filter_button(cx)).into_any_element()
                }
            }));
        } else {
            let header_id = id.clone();
            let on_sort: crate::view_rows::OnSort = std::rc::Rc::new(move |this: &mut Shell, column, cx: &mut Context<Shell>| {
                let next = crate::view_rows::ColumnSort::after_click(this.playlist_sort(&header_id), column);
                this.set_playlist_sort(&header_id, next, cx);
            });
            blocks.push(Shell::plain_block(move |this, layout, _, cx| this.render_table_header(layout, column_sort, Some(on_sort.clone()), cx)));
            blocks.extend((0..shown.len()).map(|pos| Block::Table { list: table, pos, variant: RowVariant::Full }));
        }
        // Suggestions only for the user's own regular playlists, and not while searching or filtering.
        self.recommended_list = None;
        if editable && !narrowed && self.core.is_some() {
            let new_visit = self.page.route.as_ref() != Some(&self.route);
            blocks.extend(self.recommended_blocks(id, &p.name, &all, new_visit, layout));
        }
        blocks
    }

    /// Shown on reorderable playlists while sorted or filtered. Says why rows can't move and how to go back.
    fn render_order_note(&mut self, sorted: bool, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let m = PageMetrics::of(layout);
        let sort_name = match self.current_playlist_id().and_then(|id| self.playlist_sort(&id)).map(|s| s.column) {
            None => tr("Playlist Order"),
            Some(SortColumn::Title) => tr("Title"),
            Some(SortColumn::Artist) => tr("Artist"),
            Some(SortColumn::Album) => tr("Album"),
            Some(SortColumn::Time) => tr("Length"),
            Some(SortColumn::Genre) => tr("Genre"),
            Some(SortColumn::Added) => tr("Date Added"),
            Some(SortColumn::Plays) => tr("Plays"),
        };
        let text = if sorted { trf("Sorted by {name}. Songs can only be moved in Playlist Order.", &[("name", sort_name)]) } else { tr("Showing some songs. Clear the search and filter to move songs.").to_string() };
        div()
            .mx(t.s(m.gutter))
            .flex()
            .flex_row()
            .items_center()
            .gap(t.s(8.0))
            .text_size(t.text(TEXT_SM))
            .text_color(t.t2())
            .child(icon(&t, "arrows-down-up-regular", 15.0, t.t2()))
            .child(text)
            .child(
                div()
                    .id("order-note-reset")
                    .ml(t.s(4.0))
                    .px(t.s(10.0))
                    .py(t.s(3.0))
                    .rounded_full()
                    .text_color(t.t1())
                    .font_weight(FontWeight::MEDIUM)
                    .child(crate::ui::hover_pill(&t))
                    .child(if sorted { tr("Use Playlist Order") } else { tr("Show All") })
                    .stop(&t)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(id) = this.current_playlist_id() {
                            this.set_playlist_sort(&id, None, cx);
                        }
                        this.playlist_detail.filter = SongFilter::All;
                        this.playlist_detail.query.clear();
                        this.search_active = false;
                        this.invalidate_page();
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    /// A drop on a playlist's table. The new order shows at once and is saved, and a failed save restores the old order
    /// and says why.
    pub fn move_playlist_rows(&mut self, list: usize, rows: &[usize], before: usize, cx: &mut Context<Self>) {
        let crate::pages::Route::Playlist(id) = self.route.clone() else { return };
        if self.reorder_list != Some(list) {
            return;
        }
        let Some(mut order) = self.page.lists.get(list).map(|l| l.as_ref().clone()) else { return };
        if navia_core::queue_order::move_items(&mut order, rows, before).is_none() {
            return;
        }
        let Some(lib) = self.core.as_ref().map(|c| c.lib.clone()) else {
            self.show_toast(tr("Playlists can't be changed without the server."), cx);
            return;
        };
        self.playlist_order_override.insert(id.clone(), order);
        lib.save_playlist_move(id.to_string(), rows.to_vec(), before);
        self.invalidate_page();
        cx.notify();
    }

    pub fn on_playlist_order_saved(&mut self, id: String, result: Result<(), navia_core::library::sync::PlaylistEditError>, cx: &mut Context<Self>) {
        use navia_core::library::sync::PlaylistEditError;
        self.playlist_order_override.remove(id.as_str());
        match result {
            Ok(()) => {}
            Err(PlaylistEditError::Changed) => self.show_toast(tr("This playlist changed on the server. Showing the latest order."), cx),
            Err(PlaylistEditError::WritesDisabled) => self.show_toast(tr("Test and read-only runs can't change playlists."), cx),
            Err(PlaylistEditError::ReadOnly) => self.show_toast(tr("This playlist can't be changed."), cx),
            Err(e) => self.show_toast(trf("Couldn't save the new order: {error}", &[("error", &e.to_string())]), cx),
        }
        if let Some(library) = self.library.clone() {
            self.playlists_full = library.playlist_details();
        }
        self.invalidate_page();
        cx.notify();
    }

    /// Songs were added to or removed from a playlist on the server, or the change failed.
    pub fn on_playlist_songs_changed(&mut self, id: String, name: String, added: bool, toast: Option<String>, result: Result<(), navia_core::library::sync::PlaylistEditError>, cx: &mut Context<Self>) {
        use navia_core::library::sync::PlaylistEditError;
        if added {
            self.recommended_add_done(&id, result.is_ok());
        }
        let message = match result {
            Ok(()) if added => toast.unwrap_or_else(|| trf("Added to {name}", &[("name", &name)])),
            Ok(()) => trf("Removed from {name}", &[("name", &name)]),
            Err(PlaylistEditError::Changed) => tr("This playlist changed on the server, so nothing was removed. Showing the latest songs.").to_string(),
            Err(PlaylistEditError::WritesDisabled) => tr("Test and read-only runs can't change playlists.").to_string(),
            Err(PlaylistEditError::ReadOnly) => tr("This playlist can't be changed.").to_string(),
            Err(e) if added => trf("Couldn't add to {name}: {error}", &[("name", &name), ("error", &e.to_string())]),
            Err(e) => trf("Couldn't remove from {name}: {error}", &[("name", &name), ("error", &e.to_string())]),
        };
        self.show_toast(message, cx);
        if let Some(library) = self.library.clone() {
            self.playlists_full = library.playlist_details();
        }
        if matches!(&self.route, crate::pages::Route::Playlist(open) if open.as_ref() == id) {
            self.invalidate_page();
        }
        cx.notify();
    }

    /// This playlist's column sort, remembered across visits and launches.
    pub fn playlist_sort(&self, id: &SharedString) -> Option<crate::view_rows::ColumnSort> {
        self.prefs.playlist_sorts.get(id.as_ref()).and_then(|k| crate::view_rows::ColumnSort::from_key(k))
    }

    pub fn set_playlist_sort(&mut self, id: &SharedString, sort: Option<crate::view_rows::ColumnSort>, cx: &mut Context<Self>) {
        match sort {
            Some(s) => self.prefs.playlist_sorts.insert(id.to_string(), s.key()),
            None => self.prefs.playlist_sorts.remove(id.as_ref()),
        };
        self.prefs.save();
        self.invalidate_page();
        cx.notify();
    }

    fn current_playlist_id(&self) -> Option<SharedString> {
        match &self.route {
            crate::pages::Route::Playlist(id) => Some(id.clone()),
            _ => None,
        }
    }

    fn clear_playlist_filter_button(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        crate::ui_controls::empty_action(&t, "clear-playlist-filter")
            .stop(&t).on_click(cx.listener(|this, _, _, cx| {
                this.playlist_detail.query.clear();
                this.playlist_detail.filter = SongFilter::All;
                this.search_active = false;
                this.invalidate_page();
                cx.notify();
            }))
            .child(tr("Clear"))
            .into_any_element()
    }

    fn render_playlist_head(&mut self, p: &PlaylistDetails, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let cover_id = self.playlist_art_key(&p.id);
        // follows the real owner, so an admin still sees whose it is.
        let meta = if p.mine || p.owner.is_empty() {
            format!("{} • {}", songs_and_length(p.song_count, p.duration_s), if p.public { tr("Public") } else { tr("Private") })
        } else {
            format!("{} • {} • {}", songs_and_length(p.song_count, p.duration_s), if p.public { tr("Public") } else { tr("Private") }, trf("By {owner}", &[("owner", &p.owner)]))
        };
        let meta = format!("{meta}{}", self.download_status_line(CollectionKind::Playlist, &p.id).unwrap_or_default());
        let mut info = div().mt(t.s(14.0)).flex().flex_row().flex_wrap().items_center().gap_x(t.s(12.0)).gap_y(t.s(6.0)).child(meta).when(p.smart, |d| d.child(self.smart_rules_pill(p, cx))).into_any_element();
        if !p.description.is_empty() {
            let more = self.header_more_text("playlist-desc", self.playlist_about(p), cx);
            info = div().child(info).child(more).into_any_element();
        }
        self.render_collection_head(&cover_id, p.name.clone(), info, layout, cx)
    }

    /// what the playlist's About sheet shows; whose it is only when it isn't yours.
    pub fn playlist_about(&self, p: &PlaylistDetails) -> crate::view_about::AboutText {
        let by = (!p.mine && !p.owner.is_empty()).then(|| SharedString::from(trf("By {owner}", &[("owner", &p.owner)])));
        crate::view_about::AboutText { title: p.name.clone(), subtitle: by, art: Some(self.playlist_art_key(&p.id)), text: p.description.clone() }
    }

    /// the page header, for playlists and Liked Songs.
    pub(crate) fn render_collection_head(&mut self, cover_key: &SharedString, title: SharedString, below: AnyElement, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let m = PageMetrics::of(layout);
        let art = self.hero_artwork(cover_key, "playlist-regular", m.hero_art, R_MD, layout, cx);
        self.collection_head(art, title, below, layout)
    }

    /// also the artist Top Songs page, around the artist's photo.
    pub(crate) fn collection_head(&mut self, art: AnyElement, title: SharedString, below: AnyElement, layout: &Layout) -> AnyElement {
        let t = self.theme;
        let m = PageMetrics::of(layout);
        let cover = crate::ui::hero_art_frame(&t, R_MD, art);
        let info = div().flex().flex_col().flex_1().min_w(px(0.0)).child(div().text_size(t.text(m.title)).text_color(t.t_em()).font_weight(FontWeight::BOLD).line_height(relative(1.1)).line_clamp(3).text_ellipsis().id("playlist-title").child(title.clone()).full_text_tip(&t, title)).child(below);
        let main = f32::from(layout.main_width) / t.token_scale();
        let mut head = div().flex().gap(t.s(25.0)).px(t.s(m.gutter)).w(layout.main_width).items_center();
        head = if main <= 620.0 { head.flex_col().items_start() } else { head.flex_row() };
        head.child(cover).child(info).into_any_element()
    }

    /// The smart mark on the meta line. With rules loaded it shows how many and opens them in a panel. It only uses
    /// rules already loaded, never a fresh `getPlaylist`.
    fn smart_rules_pill(&mut self, p: &PlaylistDetails, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let words = self.smart_words();
        let summary = p.rules.as_deref().and_then(|raw| summarize_criteria(raw, Say(&words))).filter(|s| !s.rules.is_empty());
        let pill = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(t.s(5.0))
            .h(t.s(26.0))
            .pl(t.s(9.0))
            .pr(t.s(10.0))
            .rounded_full()
            .bg(t.glass())
            .shadow(glass_edge())
            .text_size(t.text(TEXT_SM))
            .font_weight(FontWeight::MEDIUM)
            .text_color(t.t1())
            .child(icon(&t, "lightning-fill", 13.0, t.accent_hsla()))
            .child(tr("Smart"));
        let Some(summary) = summary else { return pill.into_any_element() };
        let store = self.bounds_store("smart-rules-pill");
        let open = self.menu.as_ref().is_some_and(|m| m.panel.is_some());
        let (id, owned) = (p.id.clone(), p.owned);
        pill.id("smart-rules-pill")
            .relative()
            .when(open, |d| d.bg(t.glass_hover()))
            .hover(|s| s.bg(t.glass_hover()))
            .child(div().font_weight(FontWeight::NORMAL).text_color(t.t2()).child(format!("· {}", trn("{count} rule", "{count} rules", summary.rules.len()))))
            .child(icon(&t, "caret-down-bold", 11.0, t.t2()))
            .child(track_bounds(store.clone()))
            .stop(&t)
            .on_click(cx.listener(move |this, _: &gpui::ClickEvent, _, cx| {
                if this.menu.as_ref().is_some_and(|m| m.panel.is_some()) {
                    this.close_menu(cx);
                    return;
                }
                let bounds = *store.borrow();
                let panel = MenuPanel {
                    sentence: (!summary.sentence.is_empty()).then(|| summary.sentence.clone().into()),
                    heading: (!summary.match_word.is_empty()).then(|| summary.match_word.clone().into()),
                    lines: summary.rules.iter().map(|r| SharedString::from(r.clone())).collect(),
                    footnote: (!summary.order.is_empty()).then(|| summary.order.join(" · ").into()),
                    scroll: ScrollHandle::new(),
                };
                let mut rows = Vec::new();
                let id = id.clone();
                // Someone else's rules open in the editor too, to be saved as the user's own copy.
                rows.push(if owned {
                    MenuRow::item(tr("Edit Rules"), "pencil-simple-regular", move |this, cx| this.navigate(crate::pages::Route::SmartEditor(Some(id.clone())), cx))
                } else {
                    MenuRow::item(tr("Edit a Copy"), "copy-simple-regular", move |this, cx| this.navigate(crate::pages::Route::SmartEditor(Some(id.clone())), cx))
                });
                this.open_panel_menu(panel, rows, bounds.bottom_left() + point(px(0.0), this.theme.s(6.0)), cx);
            }))
            .into_any_element()
    }

    fn render_playlist_actions(&mut self, p: &PlaylistDetails, list: usize, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let m = PageMetrics::of(layout);
        let songs = self.page.lists.get(list).cloned().unwrap_or_default();
        let has_songs = !songs.is_empty();
        let menu_songs = songs.as_ref().clone();
        let name = p.name.clone();
        let owned = p.owned;
        let (smart, id) = (p.smart, p.id.clone());
        let share_id = p.id.clone();
        let public = p.public;
        let mut action_list = vec![
            self.download_toggle("playlist-download", CollectionKind::Playlist, p.id.clone(), p.name.clone(), has_songs, cx),
            icon_button(&t, "playlist-share", tr("Share"), "user-plus-regular", false)
                .stop(&t).on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                    let mut rows = Vec::new();
                    if owned {
                        let id = share_id.clone();
                        rows.push(MenuRow::toggle(tr("Public on this server"), "users-regular", public, move |this, cx| this.toggle_playlist_public(id.clone(), cx)));
                    }
                    rows.push(this.share_row(navia_core::shares::ShareKind::Playlist, share_id.clone()));
                    this.open_toolbar_menu(rows, event, cx);
                }))
                .into_any_element(),
        ];
        if owned {
            action_list.push(
                icon_button(&t, "playlist-edit", tr("Edit"), "pencil-simple-regular", false)
                    .stop(&t).on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                        let details_id = id.clone();
                        let mut rows = vec![this.edit_details_row(&details_id)];
                        if smart {
                            let id = id.clone();
                            rows.push(MenuRow::item(tr("Edit Rules"), "lightning-fill", move |this, cx| this.navigate(crate::pages::Route::SmartEditor(Some(id.clone())), cx)));
                        }
                        rows.push(MenuRow::Divider);
                        rows.extend(this.playlist_cover_rows(&id));
                        rows.push(MenuRow::Divider);
                        rows.push(this.delete_playlist_row(&id));
                        this.open_toolbar_menu(rows, event, cx);
                    }))
                    .into_any_element(),
            );
        }
        let info_id = p.id.clone();
        action_list.push(
            icon_button(&t, "playlist-more", tr("More"), "dots-three-bold", false)
                .stop(&t).on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                    let mut rows = vec![this.add_to_playlist_row(tr("Add to Playlist"), menu_songs.clone(), name.clone()), this.info_row(crate::view_info::InfoTarget::Playlist(info_id.clone()))];
                    rows.extend(this.pin_row(&info_id));
                    this.open_toolbar_menu(rows, event, cx);
                }))
                .into_any_element(),
        );
        let sort = self.playlist_sort(&p.id).map(|s| s.column);
        let sort_id = p.id.clone();
        let filter = self.playlist_detail.filter;
        let row = div()
            .flex()
            .flex_row()
            .items_center()
            .flex_wrap()
            .gap(t.s(8.0))
            .px(t.s(m.gutter))
            .w(layout.main_width)
            .child(self.render_play_actions(list, has_songs, cx))
            .child(pill_group(&t, action_list))
            .child(div().flex_1())
            .child(
                glass_pill(&t)
                    .child(icon_button(&t, "playlist-sort", tr("Sort"), "arrows-down-up-regular", sort.is_some()).stop(&t).on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                        // The playlist's own order first, then the column sorts (ascending).
                        let rows = [(None, tr("Playlist Order")), (Some(SortColumn::Title), tr("Title")), (Some(SortColumn::Artist), tr("Artist")), (Some(SortColumn::Album), tr("Album")), (Some(SortColumn::Time), tr("Length"))]
                            .into_iter()
                            .map(|(o, label)| {
                                let id = sort_id.clone();
                                MenuRow::check(label, sort == o, move |this, cx| this.set_playlist_sort(&id, o.and_then(crate::view_rows::ColumnSort::asc), cx))
                            })
                            .collect();
                        this.open_toolbar_menu(rows, event, cx);
                    })))
                    .child(divider(&t))
                    .child(icon_button(&t, "playlist-filter", tr("Filter"), "funnel-simple-regular", filter != SongFilter::All).stop(&t).on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                        let rows = this.song_filter_rows(filter, |this, f| this.playlist_detail.filter = f);
                        this.open_toolbar_menu(rows, event, cx);
                    }))),
            )
            .child(self.search_pill(tr("Search Playlist"), self.playlist_detail.query.clone(), cx));
        row.into_any_element()
    }
}
