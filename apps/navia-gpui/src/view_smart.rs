//! The smart playlist editor. Rules, summary and preview come from the core (`navia_core::smart_editor`). Save works
//! for the owner or an admin, and Save As copies anyone's readable rules into a playlist of your own.

use crate::i18n::{tr, trf, trnf, tr_dyn};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::*;
use crate::focus::FocusStop;
use gpui::{AnyElement, Context, FontWeight, MouseButton, SharedString, Window, div, px, relative};

use crate::model::{Album, PlaylistDetails, Song};
use crate::pages::{Block, PageMetrics, Route, RowVariant};
use crate::shell::{Layout, Shell};
use crate::smart_model::{EditorState, Evaluator, FIELDS, FieldKind, Issue, LimitUnit, PlaylistMembers, Preview, PreviewSong, Row, RuleValue, SORT_RANDOM, Say, Shape, SimpleRule, Source, album_hint_at, directions, editor_sentence, field_by_id, flatten, has_rules, iso_from_unix, op_label, ops_for, parse_criteria, playlist_ids, rows_complete, rule_issue, shape_of, to_criteria, today_days, MAX_GROUP_DEPTH};
use crate::theme::size::*;
use crate::ui::{glass_edge, icon};
use crate::ui_controls::{PillWidth, SelectOption, TextTarget, glass_action, pill_shell, primary_action, round_button, segmented_glass, small_glass_action, with_invalid_ring, with_ring};
use crate::view_menu::MenuRow;

/// Most songs the preview lists. The count still covers every match.
const PREVIEW_MAX: usize = 200;
const PREVIEW_DEBOUNCE: Duration = Duration::from_millis(150);

#[derive(Default)]
pub struct SmartEditor {
    for_route: Option<Option<SharedString>>,
    pub name: String,
    pub state: EditorState,
    existing: Option<PlaylistDetails>,
    pub preview: Preview,
    dirty: bool,
    generation: u64,
    loading: bool,
    field_options: Rc<Vec<SelectOption>>,
    /// Sort fields for the first row, with Random first, and for the rows after it.
    sort_options: Rc<Vec<SelectOption>>,
    next_sort_options: Rc<Vec<SelectOption>>,
    /// The name and rules as opened, to detect changes.
    opened: Option<(String, serde_json::Value)>,
    /// A save is in flight.
    saving: bool,
    /// shown under the name until the next edit or save.
    pub error: Option<String>,
    /// Filters that Save as Smart Playlist couldn't carry over because the server has no field for them.
    dropped: Option<String>,
}

/// Rebuilds the editor's option lists after a language change.
impl Shell {
    pub fn refresh_smart_options(&mut self) {
        self.smart.field_options = field_options();
        (self.smart.sort_options, self.smart.next_sort_options) = sort_options();
    }
}

fn field_options() -> Rc<Vec<SelectOption>> {
    Rc::new(FIELDS.iter().map(|f| SelectOption::grouped(f.id, tr(f.label), tr(f.group))).collect())
}

fn sort_options() -> (Rc<Vec<SelectOption>>, Rc<Vec<SelectOption>>) {
    let fields: Vec<SelectOption> = FIELDS.iter().filter(|f| f.kind != FieldKind::Playlist).map(|f| SelectOption::grouped(f.id, tr(f.label), tr(f.group))).collect();
    let mut first = vec![SelectOption::new(SORT_RANDOM, tr("Random"))];
    first.extend(fields.iter().cloned());
    (Rc::new(first), Rc::new(fields))
}

/// A group's header choices, each a whole sentence.
fn match_sentences() -> Rc<Vec<SelectOption>> {
    Rc::new(vec![SelectOption::new("all", tr("All of the following are true")), SelectOption::new("any", tr("Any of the following are true"))])
}

fn preview_from_sample(rowid: u32, s: &Song, starred: bool, album: Option<&Album>) -> PreviewSong {
    PreviewSong {
        rowid,
        id: s.id.to_string(),
        title: s.title.to_string(),
        album: s.album.to_string(),
        artist: s.artist.to_string(),
        album_artist: album.map(|a| a.artist.to_string()).unwrap_or_default(),
        genres: if s.genre.is_empty() { Vec::new() } else { vec![s.genre.to_string()] },
        year: (s.year > 0).then_some(s.year as f64),
        plays: Some(s.plays as f64),
        loved: Some(starred),
        duration: Some(s.duration_s as f64),
        lyrics: Some(s.has_lyrics),
        album_plays: album.map(|a| a.plays as f64),
        album_songs: album.map(|a| a.song_count as f64),
        album_duration: album.map(|a| a.duration_s as f64),
        album_added: album.filter(|a| a.added > 0).map(|a| iso_from_unix(a.added)),
        album_last_played: album.filter(|a| a.last_played > 0).map(|a| iso_from_unix(a.last_played)),
        album_loved: album.map(|a| a.starred),
        ..Default::default()
    }
}

impl Shell {
    pub fn reset_smart_editor(&mut self) {
        let generation = self.smart.generation + 1;
        self.smart = SmartEditor { generation, ..Default::default() };
    }

    fn smart_init(&mut self, id: &Option<SharedString>) {
        if self.smart.for_route.as_ref() == Some(id) {
            return;
        }
        let existing = id.as_ref().and_then(|i| self.playlists_full.iter().find(|p| &p.id == i).cloned());
        let words = self.smart_words();
        // A new playlist from the Songs filters starts with their rules.
        let prefill = if id.is_none() { self.smart_prefill.take() } else { None };
        let rules = existing.as_ref().and_then(|p| p.rules.as_deref()).or(prefill.as_ref().map(|p| p.0.as_str()));
        let state = parse_criteria(rules, Say(&words));
        let dropped = prefill.and_then(|p| p.1);
        let generation = self.smart.generation;
        let name = existing.as_ref().map_or_else(String::new, |p| p.name.to_string());
        let opened = Some((name.trim().to_string(), to_criteria(&state)));
        let (sort_options, next_sort_options) = sort_options();
        self.smart = SmartEditor { for_route: Some(id.clone()), name, state, existing, preview: Preview::default(), dirty: true, generation, loading: true, field_options: field_options(), sort_options, next_sort_options, opened, saving: false, error: None, dropped };
        self.controls.text = None;
    }

    /// Whether the open playlist is someone else's, so it can only be saved as a copy.
    fn smart_is_others(&self) -> bool {
        self.smart.existing.as_ref().is_some_and(|p| !p.owned)
    }

    pub fn build_smart_editor(&mut self, id: Option<SharedString>, layout: &Layout) -> Vec<Block> {
        self.smart_init(&id);
        if id.is_some() && !self.smart.existing.as_ref().is_some_and(|p| p.smart) {
            return vec![Self::not_found(tr("This smart playlist isn't here"))];
        }
        let name_block = Shell::plain_block(|this, layout, window, cx| this.render_smart_name(layout, window, cx));
        let rules_block = Shell::plain_block(|this, layout, window, cx| this.render_smart_rules(layout, window, cx));
        let order_block = Shell::plain_block(|this, layout, window, cx| this.render_smart_order(layout, window, cx));
        let preview_block = Shell::plain_block(|this, layout, _, cx| this.render_smart_preview_head(layout, cx));
        let m = PageMetrics::of(layout);
        let mut blocks = vec![name_block, Block::Spacer(28.0), rules_block, Block::Spacer(28.0), order_block, Block::Spacer(28.0), preview_block, Block::Spacer(20.0)];
        let rowids = self.smart.preview.rowids.clone();
        if rowids.is_empty() {
            blocks.push(Shell::plain_block(move |this, layout, _, _| this.render_smart_empty(m.gutter, layout)));
        } else {
            self.core_preload_rows(&rowids);
            let list = self.add_list(rowids.clone());
            blocks.push(Shell::plain_block(|this, layout, _, cx| this.render_table_header(layout, None, None, cx)));
            blocks.extend((0..rowids.len()).map(|pos| Block::Table { list, pos, variant: RowVariant::Full }));
        }
        blocks
    }

    pub fn smart_leave(&mut self, cx: &mut Context<Self>) {
        if self.history_pos > 0 {
            self.go_back(cx);
        } else {
            self.navigate(Route::Playlists, cx);
        }
    }

    /// Why these rules can't be saved now, or None when they can.
    fn smart_rules_blocked(&self) -> Option<SharedString> {
        let say = |text: &'static str| Some(SharedString::new_static(text));
        if self.smart.saving {
            return say(tr("Saving\u{2026}"));
        }
        if self.core.is_none() {
            return say(tr("Saving needs the server."));
        }
        if self.smart.existing.as_ref().is_some_and(|p| p.rules.is_none()) {
            return say(tr("This playlist's rules haven't loaded from the server yet. Open it from Playlists, then come back here."));
        }
        if !has_rules(&self.smart.state.rows) {
            return say(tr("Add a rule to save"));
        }
        if !rows_complete(&self.smart.state.rows) {
            return say(tr("Finish or remove the unfinished rules to save"));
        }
        // The core checks this again before sending. This catches what the rows can't show.
        if let Some(problem) = navia_core::smart_rules::checked_criteria(&to_criteria(&self.smart.state).to_string()).err() {
            return Some(problem.0.into());
        }
        None
    }

    /// Why Save can't be pressed now, or None when it can.
    pub fn smart_blocked(&self) -> Option<SharedString> {
        if self.smart.name.trim().is_empty() && !self.smart.saving {
            return Some(SharedString::new_static(tr("Add a name to save")));
        }
        if let Some(reason) = self.smart_rules_blocked() {
            return Some(reason);
        }
        if self.smart.opened.as_ref() == Some(&(self.smart.name.trim().to_string(), to_criteria(&self.smart.state))) {
            return Some(SharedString::new_static(tr("Nothing has changed yet")));
        }
        None
    }

    /// Sends the name and rules to the server. The editor stays as is until the answer.
    pub fn smart_save(&mut self, cx: &mut Context<Self>) {
        if let Some(reason) = self.smart_blocked() {
            self.show_toast(reason, cx);
            return;
        }
        let Some(core) = self.core.as_ref() else { return };
        let id = self.smart.existing.as_ref().map(|p| p.id.to_string());
        let criteria = to_criteria(&self.smart.state).to_string();
        self.smart.saving = true;
        self.smart.error = None;
        core.lib.save_smart_playlist(id, self.smart.name.trim().to_string(), criteria);
        cx.notify();
    }

    /// Save As. Asks for a name, then saves the rules as a new smart playlist owned by the user.
    pub fn smart_save_as(&mut self, cx: &mut Context<Self>) {
        if let Some(reason) = self.smart_rules_blocked() {
            self.show_toast(reason, cx);
            return;
        }
        let original = self.smart.existing.as_ref().map(|p| p.name.to_string()).unwrap_or_default();
        let current = if self.smart.name.trim().is_empty() { original } else { self.smart.name.trim().to_string() };
        let name = if self.smart_is_others() { current } else { trf("{name} Copy", &[("name", &current)]) };
        self.open_smart_save_as(name, to_criteria(&self.smart.state).to_string(), cx);
    }

    /// The server saved it. Says so, then opens the copy for Save As, or returns to where the editor was opened.
    pub fn on_smart_saved(&mut self, id: String, name: String, created: bool, cx: &mut Context<Self>) {
        let save_as = self.new_playlist.as_ref().is_some_and(|d| d.smart.is_some());
        if let Some(library) = self.library.clone() {
            self.playlists = library.playlists();
            self.refresh_sidebar_order();
            self.load_catalog(&library);
        }
        self.show_toast(if created { trf("Created {name}", &[("name", &name)]) } else { trf("Saved {name}", &[("name", &name)]) }, cx);
        // The page is rebuilt from the server's copy next time it opens.
        self.reset_smart_editor();
        if save_as {
            self.close_new_playlist(cx);
            self.navigate(Route::Playlist(id.into()), cx);
        } else {
            self.smart_leave(cx);
        }
        cx.notify();
    }

    /// The save failed. Keeps every edit and shows why, under the name or in the Save As dialog.
    pub fn on_smart_save_failed(&mut self, message: String, cx: &mut Context<Self>) {
        if let Some(dialog) = self.new_playlist.as_mut().filter(|d| d.smart.is_some()) {
            dialog.creating = false;
            dialog.error = Some(message.into());
        } else {
            self.smart.saving = false;
            self.smart.error = Some(message);
        }
        cx.notify();
    }

    /// The editor's Cancel, Save As and Save in the top bar. For someone else's playlist, Save As is the main action.
    pub fn render_smart_tools(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let page = match (&self.smart.for_route, &self.smart.existing) {
            (Some(None), _) => Some(false),
            (Some(Some(_)), Some(p)) if p.smart => Some(true),
            _ => None,
        };
        let Some(existing) = page else { return div().into_any_element() };
        let others = self.smart_is_others();
        let cancel = glass_action(&t, "smart-cancel").occlude().child(tr("Cancel")).stop(&t).on_click(cx.listener(|this, _, _, cx| this.smart_leave(cx)));
        let save_as_enabled = self.smart_rules_blocked().is_none();
        let save_as = existing.then(|| {
            let button = if others { primary_action(&t, "smart-save-as", save_as_enabled) } else { glass_action(&t, "smart-save-as").when(!save_as_enabled, |d| d.opacity(DISABLED_OPACITY)) };
            button.occlude().child(tr("Save As\u{2026}")).stop(&t).on_click(cx.listener(|this, _, _, cx| this.smart_save_as(cx)))
        });
        let save = (!others).then(|| {
            let enabled = self.smart_blocked().is_none();
            let label = if self.smart.saving { tr("Saving\u{2026}") } else { tr("Save") };
            primary_action(&t, "smart-save", enabled).occlude().child(label).stop(&t).on_click(cx.listener(|this, _, _, cx| this.smart_save(cx)))
        });
        div().flex().flex_row().items_center().gap(t.s(10.0)).child(cancel).children(save_as).children(save).into_any_element()
    }

    pub fn smart_page_title(&self, id: &Option<SharedString>) -> SharedString {
        match id {
            None => tr("New Smart Playlist").into(),
            Some(_) if self.smart_is_others() => tr("Smart Playlist Rules").into(),
            Some(_) => tr("Edit Smart Playlist").into(),
        }
    }

    /// The library arrived or changed, so an open preview needs rerunning.
    pub fn smart_library_changed(&mut self) {
        self.smart.dirty = true;
    }

    pub fn smart_changed(&mut self, cx: &mut Context<Self>) {
        self.smart.dirty = true;
        self.smart.error = None;
        cx.notify();
    }

    /// runs the preview again.
    fn smart_edit(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut EditorState)) {
        edit(&mut self.smart.state);
        self.smart_changed(cx);
    }

    pub fn smart_value_text(&self, row: u64, side: u8) -> String {
        match self.smart.state.rule(row).map(|r| &r.value) {
            Some(RuleValue::One(v)) => v.clone(),
            Some(RuleValue::Range(a, b)) => if side == 0 { a.clone() } else { b.clone() },
            _ => String::new(),
        }
    }

    pub fn smart_value_typed(&mut self, row: u64, side: u8, value: String, cx: &mut Context<Self>) {
        self.smart_edit(cx, |st| st.set_value_text(row, side, &value));
    }

    /// once typing has paused; newer runs supersede older ones.
    fn smart_run_if_dirty(&mut self, cx: &mut Context<Self>) {
        if !self.smart.dirty {
            return;
        }
        self.smart.dirty = false;
        self.smart.generation += 1;
        let generation = self.smart.generation;
        let debounce = !self.smart.loading;
        self.smart.loading = true;
        let state = self.smart.state.clone();
        let core = self.core.as_ref().map(|c| c.lib.clone());
        let songs = self.songs.clone();
        let albums = self.albums.clone();
        let starred = self.starred.clone();
        let library = self.library.clone();
        cx.spawn(async move |this, cx| {
            if debounce {
                cx.background_executor().timer(PREVIEW_DEBOUNCE).await;
            }
            // A newer run started while this one waited.
            let current = this.read_with(cx, |shell, _| shell.smart.generation).unwrap_or(0);
            if current != generation {
                return;
            }
            let preview = cx
                .background_spawn(async move {
                    if let Some(lib) = core.as_ref() {
                        return navia_core::smart_editor::preview_library(&lib.db, &state, PREVIEW_MAX);
                    }
                    // Sample data only has the common fields and its own playlists.
                    let by_album: HashMap<SharedString, Album> = albums.iter().map(|a| (a.id.clone(), a.clone())).collect();
                    let mut members = PlaylistMembers::new();
                    if let Some(lib) = library.as_ref() {
                        for id in playlist_ids(&state.rows) {
                            let list = lib.playlist_songs(&id).into_iter().filter_map(|i| songs.get(i as usize).map(|s| s.id.to_string())).collect();
                            members.insert(id, list);
                        }
                    }
                    let mut eval = Evaluator::new(&state, Source::Sample, members, today_days());
                    for (i, song) in songs.iter().enumerate() {
                        eval.feed(&preview_from_sample(i as u32, song, starred.contains(&(i as u32)), by_album.get(&song.album_id)));
                    }
                    eval.finish(&state, PREVIEW_MAX)
                })
                .await;
            let _ = this.update(cx, |shell, cx| {
                if shell.smart.generation == generation {
                    shell.smart.preview = preview;
                    shell.smart.loading = false;
                    shell.invalidate_page();
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn smart_frame(&self, layout: &Layout, content: gpui::Div) -> AnyElement {
        let t = self.theme;
        let m = PageMetrics::of(layout);
        div().w(layout.main_width).px(t.s(m.gutter)).child(content).into_any_element()
    }

    /// for someone else's playlist, a note that saving makes a copy.
    fn render_smart_name(&mut self, layout: &Layout, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        self.smart_run_if_dirty(cx);
        let size = t.text(TEXT_TITLE);
        let others = self.smart.existing.clone().filter(|p| !p.owned);
        let title = div().pb(t.s(8.0)).border_b_1().text_size(size).font_weight(FontWeight::BOLD).line_height(relative(1.15));
        let name = match &others {
            Some(p) => title.border_color(t.line()).child(p.name.clone()).into_any_element(),
            None => {
                let focused = self.controls.text == Some(TextTarget::SmartName);
                let value = self.smart.name.clone();
                let line = self.text_line_sized(&TextTarget::SmartName, &value, tr("Playlist Name"), "smart-name-text", size, window);
                title
                    .id("smart-name")
                    .flex()
                    .flex_row()
                    .items_center()
                    .border_color(if focused { t.accent_hsla() } else { t.line().into() })
                    .cursor_text()
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.focus_text(TextTarget::SmartName, cx);
                    }))
                    .child(line)
                    .into_any_element()
            }
        };
        let words = self.smart_words();
        let sentence = editor_sentence(&self.smart.state, Say(&words));
        let empty = sentence.is_empty();
        let summary = div()
            .flex()
            .flex_row()
            .items_start()
            .gap(t.s(8.0))
            .text_size(t.text(TEXT_LG))
            .text_color(if empty { t.t3() } else { t.t1() })
            .child(div().flex_none().h(t.text(TEXT_LG) * 1.3).flex().items_center().child(icon(&t, "lightning-fill", 16.0, t.accent_hsla())))
            .child(div().min_w(px(0.0)).line_height(relative(1.3)).child(if empty { tr("Add rules and they are summed up here.").to_string() } else { sentence }));
        let note = others.map(|p| div().text_size(t.text(TEXT_SM)).text_color(t.t2()).child(trf("This playlist belongs to {owner}. Change the rules and use Save As to keep a copy of your own; the original stays as it is.", &[("owner", &p.owner)])));
        let error = self.smart.error.clone();
        self.smart_frame(layout, div().flex().flex_col().gap(t.s(12.0)).child(name).child(summary).children(note).children(error.map(|e| crate::ui::error_text(&t, e))))
    }

    fn render_smart_rules(&mut self, layout: &Layout, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let main_tokens = f32::from(layout.main_width) / t.token_scale();
        let narrow = main_tokens <= 720.0;
        let match_pill = segmented_glass(&t, "smart-match", self.smart.state.match_any, &[(false, tr("All of these")), (true, tr("Any of these"))], cx, |this, value, cx| this.smart_edit(cx, |st| st.set_any(None, value)));
        let heading = div().flex().flex_row().flex_wrap().items_center().gap_x(t.s(16.0)).gap_y(t.s(8.0)).child(div().text_size(t.text(TEXT_SECTION)).font_weight(FontWeight::BOLD).child(tr("Rules"))).child(match_pill);
        let state = self.smart.state.clone();
        let mut col = div().flex().flex_col().gap(t.s(14.0)).child(heading);
        if state.rows.is_empty() {
            col = col.child(div().px(t.s(16.0)).py(t.s(14.0)).rounded(t.s(R_MD)).bg(t.row()).text_color(t.t2()).child(tr("No rules yet. Add one to start matching songs.")));
        } else {
            let mut list = div().flex().flex_col().gap(t.s(SMART_ROW_GAP));
            for flat in flatten(&state.rows) {
                let indent = t.s(SMART_GROUP_INDENT * flat.depth as f32);
                let row = match flat.row {
                    Row::Group { key, any, rows } => {
                        let head = self.render_group_head(*key, *any, flat.depth + 1, cx);
                        let empty = rows.is_empty().then(|| self.render_empty_group(*key, flat.depth + 1, cx));
                        let mut both = div().flex().flex_col().gap(t.s(SMART_ROW_GAP)).child(head);
                        if let Some(empty) = empty {
                            both = both.child(div().pl(t.s(SMART_GROUP_INDENT)).child(empty));
                        }
                        both.into_any_element()
                    }
                    other => self.render_rule_row(other, narrow, window, cx),
                };
                list = list.child(div().id(("smart-row", flat.row.key())).pl(indent).child(row));
            }
            col = col.child(list);
        }
        let add_rule = glass_action(&t, "smart-add-rule").pl(t.s(13.0)).child(icon(&t, "plus-regular", 16.0, t.t1())).child(tr("Add Rule")).stop(&t).on_click(cx.listener(|this, _, _, cx| this.smart_edit(cx, |st| {
            st.add_rule(None);
        })));
        let add_group = glass_action(&t, "smart-add-group").pl(t.s(13.0)).child(icon(&t, "plus-regular", 16.0, t.t1())).child(tr("Add Group")).stop(&t).on_click(cx.listener(|this, _, _, cx| this.smart_edit(cx, |st| {
            st.add_group(None);
        })));
        let note = self.smart.dropped.clone().map(|what| {
            div().flex().flex_row().items_center().gap(t.s(6.0)).text_size(t.text(TEXT_SM)).text_color(t.t2()).child(icon(&t, "warning-fill", 15.0, t.t2())).child(trf("Not carried over: {what}. The server has no field for it.", &[("what", &what)]))
        });
        col = col.child(div().flex().flex_row().flex_wrap().items_center().gap(t.s(SMART_ROW_GAP)).child(add_rule).child(add_group)).children(note);
        self.smart_frame(layout, col)
    }

    /// A group's header row, as tall as a rule. It shows the All/Any sentence, then add and remove buttons on hover so
    /// they don't repeat down every level.
    fn render_group_head(&mut self, key: u64, any: bool, depth: usize, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let select = self.select_pill_sized(&format!("group-match-{key}"), if any { "any" } else { "all" }, match_sentences(), tr("Match"), PillWidth::Fit, move |this, value, cx| this.smart_edit(cx, |st| st.set_any(Some(key), value.as_ref() == "any")), cx);
        let can_nest = depth + 1 < MAX_GROUP_DEPTH;
        let add = round_button(&t, ("group-add", key), tr("Add to group"), "plus-regular").stop(&t).on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
            let mut rows = vec![MenuRow::item(tr("Add Rule"), "plus-regular", move |this, cx| this.smart_edit(cx, |st| {
                st.add_rule(Some(key));
            }))];
            if can_nest {
                rows.push(MenuRow::item(tr("Add Group"), "plus-regular", move |this, cx| this.smart_edit(cx, |st| {
                    st.add_group(Some(key));
                })));
            }
            this.open_toolbar_menu(rows, event, cx);
        }));
        let remove = round_button(&t, ("group-remove", key), tr("Remove group"), "x-regular").stop(&t).on_click(cx.listener(move |this, _, _, cx| this.smart_edit(cx, |st| st.remove(key))));
        let group = SharedString::from(format!("smart-head-{key}"));
        div()
            .id(("smart-head", key))
            .group(group.clone())
            .flex()
            .flex_row()
            .items_center()
            .gap(t.s(SMART_ROW_GAP))
            .h(t.s(CONTROL_H))
            .child(select)
            .child(div().flex_1())
            .child(div().flex().flex_row().gap(t.s(SMART_ROW_GAP)).opacity(0.0).group_hover(group, |s| s.opacity(1.0)).child(add).child(remove))
            .into_any_element()
    }

    /// An empty group isn't an error, since saving leaves it out. It offers what can go in it.
    fn render_empty_group(&mut self, key: u64, depth: usize, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let button = |id: &'static str, glyph: &'static str, label: &'static str| small_glass_action(&t, (id, key)).gap(t.s(6.0)).child(icon(&t, glyph, 14.0, t.t1())).child(label).stop(&t);
        let add_rule = button("group-empty-rule", "plus-regular", tr("Add Rule")).on_click(cx.listener(move |this, _, _, cx| this.smart_edit(cx, |st| {
            st.add_rule(Some(key));
        })));
        let add_group = (depth + 1 < MAX_GROUP_DEPTH).then(|| {
            button("group-empty-group", "plus-regular", tr("Add Group")).on_click(cx.listener(move |this, _, _, cx| this.smart_edit(cx, |st| {
                st.add_group(Some(key));
            })))
        });
        let remove = button("group-empty-remove", "x-regular", tr("Remove Group")).on_click(cx.listener(move |this, _, _, cx| this.smart_edit(cx, |st| st.remove(key))));
        div().flex().flex_row().flex_wrap().items_center().gap(t.s(SMART_ROW_GAP)).child(add_rule).children(add_group).child(remove).into_any_element()
    }

    fn render_rule_row(&mut self, row: &Row, narrow: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let key = row.key();
        let remove = round_button(&t, ("rule-remove", key), tr("Remove rule"), "x-regular").stop(&t).on_click(cx.listener(move |this, _, _, cx| this.smart_edit(cx, |st| st.remove(key))));
        match row {
            Row::Kept { text, reason, .. } => div()
                .flex()
                .flex_row()
                .items_center()
                .gap(t.s(SMART_ROW_GAP))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .items_baseline()
                        .gap_x(t.s(10.0))
                        .gap_y(t.s(4.0))
                        .flex_1()
                        .min_w(px(0.0))
                        .min_h(t.s(CONTROL_H))
                        .px(t.s(16.0))
                        .py(t.s(9.0))
                        .rounded(t.s(R_MD))
                        .bg(t.row())
                        .text_size(t.text(TEXT_SM))
                        .child(div().flex_none().text_size(t.text(TEXT_XS)).font_weight(FontWeight::SEMIBOLD).text_color(t.t2()).child(tr_dyn(reason)))
                        .child(text.clone()),
                )
                .child(remove)
                .into_any_element(),
            Row::Simple(rule) => {
                let Some(field) = field_by_id(&rule.field) else { return div().into_any_element() };
                let shape = shape_of(field, &rule.op);
                let (field_id, op) = (rule.field.clone(), rule.op.clone());
                let field_select = self.select_pill(&format!("rule-field-{key}"), field_id, self.smart.field_options.clone(), tr("Field"), None, move |this, value, cx| this.smart_edit(cx, |st| st.set_field(key, &value)), cx);
                let ops: Vec<SelectOption> = ops_for(field).into_iter().map(|o| SelectOption::new(o, tr(op_label(o)))).collect();
                let op_select = self.select_pill(&format!("rule-op-{key}"), op, Rc::new(ops), tr("Condition"), None, move |this, value, cx| this.smart_edit(cx, |st| st.set_op(key, &value)), cx);
                let value = self.render_rule_value(rule, shape, field.unit.map(tr), field.hint.map(tr), window, cx);
                let field_cell = div().w(t.s(SMART_FIELD_W)).min_w(px(0.0)).child(field_select);
                let op_cell = div().w(t.s(SMART_OP_W)).min_w(px(0.0)).child(op_select);
                if narrow {
                    let mut stack = div().flex().flex_col().gap(t.s(SMART_ROW_GAP));
                    stack = stack.child(div().flex().flex_row().items_center().gap(t.s(SMART_ROW_GAP)).child(field_cell.flex_1()).child(op_cell.flex_1()).child(remove));
                    if let Some(value) = value {
                        stack = stack.child(div().flex().child(value));
                    }
                    stack.into_any_element()
                } else {
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(t.s(SMART_ROW_GAP))
                        .child(field_cell)
                        .child(op_cell)
                        .child(div().flex().flex_1().min_w(px(0.0)).when_some(value, |d, v| d.child(v)))
                        .child(remove)
                        .into_any_element()
                }
            }
            Row::Group { .. } => div().into_any_element(),
        }
    }

    fn render_rule_value(&mut self, rule: &SimpleRule, shape: Shape, unit: Option<&'static str>, hint: Option<&'static str>, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = self.theme;
        let key = rule.key;
        let issue = rule_issue(rule) == Issue::Invalid;
        let placeholder = tr(shape.placeholder());
        match shape {
            Shape::None => None,
            Shape::Bool => {
                let value = matches!(rule.value, RuleValue::Flag(true));
                Some(segmented_glass(&t, "rule-bool", value, &[(true, tr("Yes")), (false, tr("No"))], cx, move |this, v, cx| this.smart_edit(cx, |st| st.set_flag(key, v))).into_any_element())
            }
            Shape::Genre | Shape::Playlist => {
                let v = if let RuleValue::One(v) = &rule.value { v.clone() } else { String::new() };
                let genre = shape == Shape::Genre;
                let mut options: Vec<SelectOption> = if genre { self.genres.iter().map(|g| SelectOption::new(g.name.clone(), g.name.clone())).collect() } else { self.playlists_full.iter().map(|p| SelectOption::new(p.id.clone(), p.name.clone())).collect() };
                // A saved value can name something not in the picker, like a genre this library lacks.
                if !v.is_empty() && !options.iter().any(|o| o.value == v.as_str()) {
                    let display = if genre { v.clone() } else { trf("Playlist {id}", &[("id", &v.chars().take(8).collect::<String>())]) };
                    options.insert(0, SelectOption::new(v.clone(), display));
                }
                let placeholder = if genre { tr("Choose a genre") } else { tr("Choose a playlist") };
                Some(self.select_pill(&format!("rule-value-{key}"), v, Rc::new(options), placeholder, None, move |this, value, cx| this.smart_edit(cx, |st| st.set_choice(key, &value)), cx))
            }
            _ if shape.is_range() => {
                let (a, b) = if let RuleValue::Range(a, b) = &rule.value { (a.clone(), b.clone()) } else { (String::new(), String::new()) };
                let (target_a, target_b) = (TextTarget::SmartValue { row: key, side: 0 }, TextTarget::SmartValue { row: key, side: 1 });
                let focused = self.controls.text == Some(target_a.clone()) || self.controls.text == Some(target_b.clone());
                let line_a = self.text_line(&target_a, &a, placeholder, &format!("rule-a-{key}"), window);
                let line_b = self.text_line(&target_b, &b, placeholder, &format!("rule-b-{key}"), window);
                let cell = |id: String, side: u8, line: AnyElement, t: &crate::theme::Theme, cx: &mut Context<Self>| {
                    let target = TextTarget::SmartValue { row: key, side };
                    div().id(gpui::ElementId::Name(id.into())).flex().flex_row().items_center().flex_1().min_w(px(0.0)).h_full().pl(t.s(12.0)).cursor_text().on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.focus_text(target.clone(), cx);
                    })).child(line)
                };
                let pair = pill_shell(&t, ("rule-pair", key)).flex_1().w_full().pl(t.s(4.0)).pr(t.s(14.0)).gap(t.s(6.0)).shadow(shadow_for(&t, focused, issue)).child(cell(format!("rule-a-cell-{key}"), 0, line_a, &t, cx)).child(div().flex_none().text_size(t.text(TEXT_SM)).text_color(t.t2()).child(tr("to"))).child(cell(format!("rule-b-cell-{key}"), 1, line_b, &t, cx)).when_some(unit, |d, u| d.child(unit_text(&t, u)));
                Some(pair.into_any_element())
            }
            _ => {
                let v = if let RuleValue::One(v) = &rule.value { v.clone() } else { String::new() };
                let target = TextTarget::SmartValue { row: key, side: 0 };
                let focused = self.controls.text == Some(target.clone());
                let line = self.text_line(&target, &v, hint.unwrap_or(placeholder), &format!("rule-v-{key}"), window);
                let unit = if shape == Shape::Days { Some(tr("days")) } else { unit };
                Some(
                    pill_shell(&t, ("rule-value", key))
                        .w_full()
                        .px(t.s(16.0))
                        .gap(t.s(6.0))
                        .cursor_text()
                        .shadow(shadow_for(&t, focused, issue))
                        .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.focus_text(target.clone(), cx);
                        }))
                        .child(line)
                        .when_some(unit, |d, u| d.child(unit_text(&t, u)))
                        .into_any_element(),
                )
            }
        }
    }

    /// The Order section: sort rows, a hint to sort Album after Date Added, Add Sort Field, then the limit in songs or
    /// percent of matches. Random takes the whole order.
    fn render_smart_order(&mut self, layout: &Layout, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let st = self.smart.state.clone();
        let random = st.sorts.first().is_some_and(|s| s.field == SORT_RANDOM);
        let label = |text: &'static str| div().w(t.s(SMART_SORT_LABEL_W)).flex_none().text_size(t.text(TEXT_SM)).text_color(t.t2()).child(text);
        let mut col = div().flex().flex_col().gap(t.s(14.0)).child(div().text_size(t.text(TEXT_SECTION)).font_weight(FontWeight::BOLD).child(tr("Order")));
        let mut sorts = div().flex().flex_col().gap(t.s(SMART_ROW_GAP));
        for (i, s) in st.sorts.iter().enumerate() {
            let base = if i == 0 { self.smart.sort_options.clone() } else { self.smart.next_sort_options.clone() };
            // A saved sort field outside the catalog, like a custom tag, stays offered under its own name.
            let options = if base.iter().any(|o| o.value == s.field.as_str()) { base } else { Rc::new(std::iter::once(SelectOption::new(s.field.clone(), s.field.clone())).chain(base.iter().cloned()).collect()) };
            let select = self.select_pill(&format!("smart-sort-{}", s.key), s.field.clone(), options, if i == 0 { tr("Sort field") } else { tr("Next sort field") }, Some(SMART_SORT_W), move |this, value, cx| this.smart_edit(cx, |st| st.set_sort_field(i, &value)), cx);
            let mut row = div().id(("sort-row", s.key)).flex().flex_row().flex_wrap().items_center().gap(t.s(SMART_ROW_GAP)).child(label(if i == 0 { tr("Sort by") } else { tr("Then by") })).child(select);
            if s.field != SORT_RANDOM {
                let options: Vec<(bool, &'static str)> = directions(&s.field).iter().map(|d| (d.desc, tr(d.label))).collect();
                row = row.child(segmented_glass(&t, if i == 0 { "smart-dir-0" } else { "smart-dir-n" }, s.desc, &options, cx, move |this, desc, cx| this.smart_edit(cx, |st| st.set_sort_desc(i, desc))));
            }
            if st.sorts.len() > 1 {
                row = row.child(round_button(&t, ("sort-remove", s.key), tr("Remove sort field"), "x-regular").stop(&t).on_click(cx.listener(move |this, _, _, cx| this.smart_edit(cx, |st| st.remove_sort(i)))));
            }
            sorts = sorts.child(row);
        }
        if let Some(at) = album_hint_at(&st.sorts) {
            let add = small_glass_action(&t, "sort-add-album").child(tr("Add Album")).stop(&t).on_click(cx.listener(move |this, _, _, cx| this.smart_edit(cx, |st| st.add_sort(at + 1, Some("album")))));
            sorts = sorts.child(div().flex().flex_row().flex_wrap().items_center().gap(t.s(SMART_ROW_GAP)).text_size(t.text(TEXT_SM)).text_color(t.t2()).child(icon(&t, "lightbulb-fill", 15.0, t.t2())).child(tr("Songs added together share a date. Add Album next to keep albums together.")).child(add));
        }
        if !random {
            let add = glass_action(&t, "sort-add").pl(t.s(13.0)).child(icon(&t, "plus-regular", 16.0, t.t1())).child(tr("Add Sort Field")).stop(&t).on_click(cx.listener(|this, _, _, cx| this.smart_edit(cx, |st| st.add_sort(usize::MAX, None))));
            sorts = sorts.child(div().flex().child(add));
        }
        col = col.child(sorts);
        let limit_target = TextTarget::SmartLimit;
        let focused = self.controls.text == Some(limit_target.clone());
        let line = self.text_line(&limit_target, &st.limit, tr("all"), "smart-limit-text", window);
        let limit_pill = pill_shell(&t, "smart-limit").w(t.s(SMART_LIMIT_W)).px(t.s(16.0)).gap(t.s(6.0)).cursor_text().shadow(if focused { with_ring(glass_edge(), t.accent_hsla()) } else { glass_edge() }).on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
            cx.stop_propagation();
            this.focus_text(TextTarget::SmartLimit, cx);
        })).child(unit_text(&t, tr("Top"))).child(line);
        let unit = segmented_glass(&t, "smart-limit-unit", st.unit, &[(LimitUnit::Songs, tr("songs")), (LimitUnit::Percent, tr("% of matches"))], cx, |this, unit, cx| this.smart_edit(cx, |st| st.set_unit(unit)));
        col = col.child(div().flex().flex_row().flex_wrap().items_center().gap(t.s(SMART_ROW_GAP)).child(label(tr("Limit"))).child(limit_pill).child(unit));
        self.smart_frame(layout, col)
    }

    fn render_smart_preview_head(&mut self, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let _ = cx;
        let p = self.smart.preview.clone();
        let st = &self.smart.state;
        let count: String = if st.rows.is_empty() || !p.checkable {
            String::new()
        } else {
            let shown = p.rowids.len() < p.matched;
            let vars: &[(&str, &str)] = &[("shown", &crate::i18n::fmt_int(p.rowids.len() as i64)), ("total", &crate::i18n::fmt_int(p.sampled as i64))];
            match (self.core.is_some(), shown) {
                (true, false) => trnf("{count} match in your library of {total} songs", "{count} matches in your library of {total} songs", p.matched, vars),
                (false, false) => trnf("{count} match in a sample of {total} songs", "{count} matches in a sample of {total} songs", p.matched, vars),
                (true, true) => trnf("showing {shown} of {count} match in your library of {total} songs", "showing {shown} of {count} matches in your library of {total} songs", p.matched, vars),
                (false, true) => trnf("showing {shown} of {count} match in a sample of {total} songs", "showing {shown} of {count} matches in a sample of {total} songs", p.matched, vars),
            }
        };
        let mut caveat = Vec::new();
        if !p.unchecked.is_empty() {
            let fields = p.unchecked.iter().map(|f| tr_dyn(f)).collect::<Vec<_>>().join(", ");
            caveat.push(if self.core.is_some() { trf("Not stored on this device, so not applied here: {fields}.", &[("fields", &fields)]) } else { trf("Not in the sample, so not applied here: {fields}.", &[("fields", &fields)]) });
        }
        if !p.sort_unchecked.is_empty() && !st.rows.is_empty() {
            let label = p.sort_unchecked.iter().map(|f| tr_dyn(f)).collect::<Vec<_>>().join(", ");
            caveat.push(trf("Sorting by {label} isn't applied here.", &[("label", &label)]));
        }
        let note = |text: String, t: &crate::theme::Theme| div().flex().flex_row().items_start().gap(t.s(8.0)).text_size(t.text(TEXT_SM)).text_color(t.t2()).child(div().flex_none().mt(t.s(2.0)).child(icon(t, "lightning-fill", 14.0, t.t2()))).child(div().min_w(px(0.0)).child(text));
        let mut col = div().flex().flex_col().gap(t.s(14.0)).child(
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .items_center()
                .gap_x(t.s(16.0))
                .gap_y(t.s(8.0))
                .child(div().text_size(t.text(TEXT_SECTION)).font_weight(FontWeight::BOLD).child(tr("Preview")))
                .child(div().text_size(t.text(TEXT_SM)).text_color(t.t2()).child(count)),
        );
        if !caveat.is_empty() {
            col = col.child(note(caveat.join(" "), &t));
        }
        if self.smart.existing.is_some() {
            let rules_loaded = self.smart.existing.as_ref().is_some_and(|p| p.rules.is_some());
            col = if rules_loaded {
                col.child(note(tr("This preview only checks the songs stored on this device, so it can differ from the playlist's real song count.").into(), &t))
            } else {
                col.child(note(tr("This playlist's rules haven't loaded from the server yet. Open it from Playlists, then come back here.").into(), &t))
            };
        }
        self.smart_frame(layout, col)
    }

    fn render_smart_empty(&mut self, gutter: f32, layout: &Layout) -> AnyElement {
        let t = self.theme;
        let st = &self.smart.state;
        let p = &self.smart.preview;
        let text: &'static str = if self.smart.loading {
            tr("Checking your library…")
        } else if !has_rules(&st.rows) {
            tr("Add a rule to see matching songs.")
        } else if !p.checkable && rows_complete(&st.rows) {
            tr("None of these rules can be checked against the local library.")
        } else if !p.checkable {
            tr("Finish a rule to see matching songs.")
        } else {
            tr("No songs here match these rules.")
        };
        div().w(layout.main_width).px(t.s(gutter)).text_color(t.t2()).child(text).into_any_element()
    }

    /// for script steps that add inside it.
    fn smart_last_group(&self) -> Option<u64> {
        self.smart.state.rows.iter().rev().find_map(|r| matches!(r, Row::Group { .. }).then(|| r.key()))
    }

    pub fn smart_script(&mut self, op: &str, cx: &mut Context<Self>) -> bool {
        let Some(rest) = op.strip_prefix("smart-") else { return false };
        if rest == "any" {
            self.smart.state.match_any = true;
        } else if rest == "cancel" {
            self.smart_leave(cx);
        } else if rest == "save" {
            self.smart_save(cx);
            return true;
        } else if let Some(name) = rest.strip_prefix("saveas:") {
            // Save As with this name. Opens the dialog, names the copy and saves it.
            self.smart_save_as(cx);
            if let Some(dialog) = self.new_playlist.as_mut() {
                dialog.name = name.replace('_', " ");
            }
            self.new_playlist_submit(cx);
            return true;
        } else if rest == "saveas-dialog" {
            self.smart_save_as(cx);
            return true;
        } else if let Some(name) = rest.strip_prefix("name:") {
            self.smart.name = name.replace('_', " ");
        } else if let Some(limit) = rest.strip_prefix("limit:") {
            self.smart.state.set_unit(LimitUnit::Songs);
            self.smart.state.set_limit(limit);
        } else if let Some(limit) = rest.strip_prefix("limitpct:") {
            self.smart.state.set_unit(LimitUnit::Percent);
            self.smart.state.set_limit(limit);
        } else if let Some(v) = rest.strip_prefix("sort:") {
            let (field, dir) = v.split_once(':').unwrap_or((v, "asc"));
            self.smart.state.set_sort_field(0, field);
            self.smart.state.set_sort_desc(0, dir == "desc");
        } else if let Some(v) = rest.strip_prefix("thensort:") {
            let (field, dir) = v.split_once(':').unwrap_or((v, "asc"));
            let at = self.smart.state.sorts.len();
            self.smart.state.add_sort(at, Some(field));
            self.smart.state.set_sort_desc(at, dir == "desc");
        } else if rest == "group" {
            // A new group's first rule is removed so the following `grule:` steps fill it.
            if let Some(g) = self.smart.state.add_group(None) {
                if let Some(Row::Group { rows, .. }) = self.smart.state.rows.iter_mut().find(|r| r.key() == g) {
                    rows.clear();
                }
            }
        } else if let Some(v) = rest.strip_prefix("rule:").map(|v| (None, v)).or_else(|| rest.strip_prefix("grule:").map(|v| (Some(()), v))) {
            // For example rule:genre:is:Rock or rule:year:inTheRange:1990:1999. A `grule:` adds to the last group.
            let (in_group, v) = v;
            let group = in_group.and_then(|_| self.smart_last_group());
            let mut parts = v.split(':');
            let (Some(field), Some(rule_op)) = (parts.next(), parts.next()) else { return true };
            let (a, b) = (parts.next().unwrap_or("").replace('_', " "), parts.next().unwrap_or("").to_string());
            let st = &mut self.smart.state;
            let Some(key) = st.add_rule(group) else { return true };
            st.set_field(key, field);
            st.set_op(key, rule_op);
            match field_by_id(field).map(|f| shape_of(f, rule_op)) {
                Some(Shape::Bool) => st.set_flag(key, a == "true"),
                Some(s) if s.is_range() => {
                    st.set_value_text(key, 0, &a);
                    st.set_value_text(key, 1, &b);
                }
                Some(Shape::Genre | Shape::Playlist) => st.set_choice(key, &a),
                _ => st.set_value_text(key, 0, &a),
            }
        } else if rest == "file" {
            if let Some(path) = std::env::var_os("NAVIA_SMART_RULES") {
                if let Ok(text) = std::fs::read_to_string(path) {
                    let words = self.smart_words();
                    self.smart.state = parse_criteria(Some(&text), Say(&words));
                }
            }
        } else if rest == "report" {
            let p = &self.smart.preview;
            let words = self.smart_words();
            eprintln!("navia: smart preview matched={} shown={} sampled={} checkable={} unchecked={:?} sort_unchecked={:?} first={:?}", p.matched, p.rowids.len(), p.sampled, p.checkable, p.unchecked, p.sort_unchecked, p.rowids.first().and_then(|i| self.song_peek(*i)).map(|s| s.title));
            eprintln!("navia: smart summary {:?}", editor_sentence(&self.smart.state, Say(&words)));
            eprintln!("navia: smart criteria {}", to_criteria(&self.smart.state));
            return true;
        } else if let Some(kind) = rest.strip_prefix("fixture:") {
            // A smart playlist for tests that open one, `owned` or `other`.
            let rules = std::env::var_os("NAVIA_SMART_RULES").and_then(|p| std::fs::read_to_string(p).ok());
            let mut list = self.playlists_full.as_ref().clone();
            list.retain(|p| p.id != "fixture-smart");
            list.push(PlaylistDetails { id: "fixture-smart".into(), name: tr("Weekend Mix").into(), song_count: 40, duration_s: 7200, description: "".into(), smart: true, public: true, owned: kind == "owned", mine: kind == "owned", owner: if kind == "owned" { "".into() } else { "someone".into() }, rules, readonly: true });
            self.playlists_full = std::sync::Arc::new(list);
            self.reset_smart_editor();
            self.navigate(Route::SmartEditor(Some("fixture-smart".into())), cx);
            return true;
        } else if rest == "openfield" {
            let Some(rule) = flatten(&self.smart.state.rows).into_iter().find_map(|f| match f.row {
                Row::Simple(s) => Some(s.clone()),
                _ => None,
            }) else {
                return true;
            };
            let key = rule.key;
            let bounds = self.controls.bounds.get(&format!("rule-field-{key}")).map(|b| *b.borrow());
            if let Some(bounds) = bounds {
                let options = self.smart.field_options.clone();
                self.toggle_select_public(format!("rule-field-{key}"), rule.field.into(), options, Rc::new(move |this: &mut Shell, v, cx: &mut Context<Shell>| this.smart_edit(cx, |st| st.set_field(key, &v))), bounds, cx);
            }
        } else {
            return false;
        }
        self.smart_changed(cx);
        true
    }
}

fn shadow_for(t: &crate::theme::Theme, focused: bool, invalid: bool) -> Vec<gpui::BoxShadow> {
    if focused {
        with_ring(glass_edge(), t.accent_hsla())
    } else if invalid {
        with_invalid_ring(glass_edge())
    } else {
        glass_edge()
    }
}

fn unit_text(t: &crate::theme::Theme, unit: &'static str) -> gpui::Div {
    div().flex_none().text_size(t.text(TEXT_SM)).text_color(t.t2()).pr(t.s(2.0)).child(unit)
}
