//! The single text field implementation. Each field is a `TextTarget` whose string lives in
//! `Shell`. This keeps caret, selection and IME state for the focused field, draws the line,
//! and turns keys, mouse and clipboard into edits. Typed text comes through the platform's
//! input handler, so IME composition works the same way.

use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    AnyElement, Bounds, ClipboardItem, Context, ElementInputHandler, EntityInputHandler, Hsla, KeyDownEvent, MouseButton, MouseDownEvent, Pixels, Point, ShapedLine,
    SharedString, TextAlign, TextRun, UTF16Selection, UnderlineStyle, Window, canvas, div, fill, point, px, size,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::focus::FocusStop;
use crate::i18n::tr;
use crate::shell::Shell;
use crate::ui_controls::{PointerDrag, TextTarget, track_bounds};

/// What was last painted for one field, enough to map a click to a caret position.
pub struct FieldLine {
    line: ShapedLine,
    origin_x: Pixels,
    scroll: Pixels,
}

/// Caret and selection for the field with the keyboard. Byte offsets into its text.
#[derive(Default)]
pub struct FieldEdit {
    target: Option<TextTarget>,
    anchor: usize,
    caret: usize,
    marked: Option<Range<usize>>,
    scroll: Pixels,
    lines: HashMap<String, FieldLine>,
    /// The line painted for the focused field, for input method queries.
    current_line: Option<String>,
    history: History,
}

/// A field's text and caret, as undo and redo restore them.
#[derive(Clone, Debug, PartialEq, Default)]
struct EditState {
    value: String,
    caret: usize,
    anchor: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum EditKind {
    Typing,
    Deleting,
    Other,
    /// Composing with an input method, already recorded when composition started.
    Skip,
}

/// Undo and redo for the focused field. Runs of typing or deleting undo together. Cleared when
/// focus moves to another field.
#[derive(Default, Debug)]
struct History {
    undo: Vec<EditState>,
    redo: Vec<EditState>,
    last: Option<(EditKind, Instant)>,
}

const HISTORY_LIMIT: usize = 200;
const COALESCE: Duration = Duration::from_millis(800);

impl History {
    fn clear(&mut self) {
        *self = History::default();
    }

    /// Records the state an edit is about to change. Call before the edit.
    fn record(&mut self, before: EditState, kind: EditKind, now: Instant) {
        debug_assert!(kind != EditKind::Skip);
        let merge = kind != EditKind::Other && self.last.is_some_and(|(k, at)| k == kind && now.duration_since(at) < COALESCE) && !self.undo.is_empty();
        if !merge {
            self.undo.push(before);
            if self.undo.len() > HISTORY_LIMIT {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.last = Some((kind, now));
    }

    fn undo(&mut self, current: EditState) -> Option<EditState> {
        let state = self.undo.pop()?;
        self.redo.push(current);
        self.last = None;
        Some(state)
    }

    fn redo(&mut self, current: EditState) -> Option<EditState> {
        let state = self.redo.pop()?;
        self.undo.push(current);
        self.last = None;
        Some(state)
    }
}

impl FieldEdit {
    fn selection(&self) -> Range<usize> {
        self.anchor.min(self.caret)..self.anchor.max(self.caret)
    }

    fn collapse(&mut self, at: usize) {
        self.anchor = at;
        self.caret = at;
        self.marked = None;
    }
}

/// Drawn for each character of a hidden password.
const MASK: &str = "\u{2022}";

/// Maps a byte offset in the real text to the same place in the dots.
fn to_masked(value: &str, at: usize) -> usize {
    value[..floor_boundary(value, at)].chars().count() * MASK.len()
}

/// Maps a place in the dots to a byte offset in the real text.
fn from_masked(value: &str, at: usize) -> usize {
    let wanted = at / MASK.len();
    value.char_indices().map(|(i, _)| i).nth(wanted).unwrap_or(value.len())
}

fn floor_boundary(value: &str, mut at: usize) -> usize {
    at = at.min(value.len());
    while !value.is_char_boundary(at) {
        at -= 1;
    }
    at
}

fn previous_grapheme(value: &str, at: usize) -> usize {
    value.grapheme_indices(true).rev().find_map(|(i, _)| (i < at).then_some(i)).unwrap_or(0)
}

fn next_grapheme(value: &str, at: usize) -> usize {
    value.grapheme_indices(true).find_map(|(i, _)| (i > at).then_some(i)).unwrap_or(value.len())
}

/// The start of the word before `at`, skipping the spaces right before it.
fn word_left(value: &str, at: usize) -> usize {
    let head = value[..at].trim_end();
    match head.char_indices().rev().find(|(_, c)| c.is_whitespace()) {
        Some((i, c)) => i + c.len_utf8(),
        None => 0,
    }
}

/// The end of the word after `at`, skipping the spaces right after it.
fn word_right(value: &str, at: usize) -> usize {
    let tail = &value[at..];
    let word = tail.trim_start();
    let spaces = tail.len() - word.len();
    at + spaces + word.find(char::is_whitespace).unwrap_or(word.len())
}

fn utf16_to_utf8(value: &str, utf16: usize) -> usize {
    let (mut units, mut bytes) = (0, 0);
    for c in value.chars() {
        if units >= utf16 {
            break;
        }
        units += c.len_utf16();
        bytes += c.len_utf8();
    }
    bytes
}

fn utf8_to_utf16(value: &str, utf8: usize) -> usize {
    value[..utf8].encode_utf16().count()
}

fn range_to_utf8(value: &str, r: &Range<usize>) -> Range<usize> {
    utf16_to_utf8(value, r.start)..utf16_to_utf8(value, r.end)
}

fn range_to_utf16(value: &str, r: &Range<usize>) -> Range<usize> {
    utf8_to_utf16(value, r.start)..utf8_to_utf16(value, r.end)
}

impl Shell {
    /// The field typing goes to. The focused settings field, the open dialog's, or the page's.
    pub fn focused_target(&self) -> Option<TextTarget> {
        if let Some(target) = &self.controls.text {
            return Some(target.clone());
        }
        if self.search_active && self.page_has_field() {
            return Some(TextTarget::Page);
        }
        None
    }

    /// Whether the focused field's caret is at its start with nothing selected.
    pub fn field_caret_at_start(&self) -> bool {
        let e = self.controls.edit.borrow();
        e.caret == 0 && e.anchor == 0
    }

    /// Resets edit state when focus has moved to a different field.
    pub fn edit_sync(&mut self) {
        let target = self.focused_target();
        let edit = self.controls.edit.clone();
        let mut edit = edit.borrow_mut();
        if edit.target != target {
            let len = target.as_ref().map_or(0, |t| self.text_value(t).len());
            edit.target = target;
            edit.collapse(len);
            edit.scroll = px(0.0);
            edit.history.clear();
        }
    }

    pub fn focus_field(&mut self, target: TextTarget, cx: &mut Context<Self>) {
        match target {
            TextTarget::Page => {
                self.controls.text = None;
                self.search_active = true;
            }
            other => {
                self.controls.text = Some(other);
                self.search_active = false;
            }
        }
        self.edit_sync();
        cx.notify();
    }

    fn field_replace(&mut self, target: &TextTarget, range: Range<usize>, text: &str, kind: EditKind, cx: &mut Context<Self>) {
        let mut value = self.text_value(target);
        if kind != EditKind::Skip {
            let mut e = self.controls.edit.borrow_mut();
            let (caret, anchor) = (e.caret, e.anchor);
            e.history.record(EditState { value: value.clone(), caret, anchor }, kind, Instant::now());
        }
        let start = floor_boundary(&value, range.start);
        let end = floor_boundary(&value, range.end.max(start));
        value.replace_range(start..end, text);
        self.set_text_value(target, value, cx);
        // Some fields filter input, so read back what they kept.
        let after = self.text_value(target);
        let caret = floor_boundary(&after, start + text.len());
        self.controls.edit.borrow_mut().collapse(caret);
        cx.notify();
    }

    /// Editing keys. Plain characters come through `replace_text_in_range` instead.
    pub fn field_key(&mut self, target: &TextTarget, event: &KeyDownEvent, cx: &mut Context<Self>) {
        self.edit_sync();
        let value = self.text_value(target);
        let key = event.keystroke.key.as_str();
        let mods = event.keystroke.modifiers;
        let ctrl = mods.control || mods.platform;
        let edit = self.controls.edit.clone();
        let (caret, selection) = {
            let mut e = edit.borrow_mut();
            e.caret = floor_boundary(&value, e.caret);
            e.anchor = floor_boundary(&value, e.anchor);
            (e.caret, e.selection())
        };
        let move_to = |at: usize, cx: &mut Context<Self>| {
            let mut e = edit.borrow_mut();
            e.caret = at;
            if !mods.shift {
                e.anchor = at;
            }
            e.marked = None;
            cx.notify();
        };
        match key {
            "a" if ctrl => {
                let mut e = edit.borrow_mut();
                e.anchor = 0;
                e.caret = value.len();
                cx.notify();
            }
            "c" if ctrl => {
                if !selection.is_empty() && !self.field_masked(target) {
                    cx.write_to_clipboard(ClipboardItem::new_string(value[selection].to_string()));
                }
            }
            "x" if ctrl => {
                if !selection.is_empty() && !self.field_masked(target) {
                    cx.write_to_clipboard(ClipboardItem::new_string(value[selection.clone()].to_string()));
                    self.field_replace(target, selection, "", EditKind::Other, cx);
                }
            }
            "z" if ctrl => {
                if mods.shift {
                    self.field_redo(target, cx);
                } else {
                    self.field_undo(target, cx);
                }
            }
            "y" if ctrl => self.field_redo(target, cx),
            "v" if ctrl => {
                if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
                    let line = text.replace(['\n', '\r'], " ");
                    if !line.is_empty() {
                        self.field_replace(target, selection, &line, EditKind::Other, cx);
                    }
                }
            }
            "left" if ctrl => move_to(word_left(&value, caret), cx),
            "right" if ctrl => move_to(word_right(&value, caret), cx),
            "left" if !selection.is_empty() && !mods.shift => move_to(selection.start, cx),
            "right" if !selection.is_empty() && !mods.shift => move_to(selection.end, cx),
            "left" => move_to(previous_grapheme(&value, caret), cx),
            "right" => move_to(next_grapheme(&value, caret), cx),
            "home" => move_to(0, cx),
            "end" => move_to(value.len(), cx),
            "backspace" | "delete" => {
                let forward = key == "delete";
                let range = if !selection.is_empty() {
                    selection
                } else if forward {
                    caret..if ctrl { word_right(&value, caret) } else { next_grapheme(&value, caret) }
                } else {
                    (if ctrl { word_left(&value, caret) } else { previous_grapheme(&value, caret) })..caret
                };
                if !range.is_empty() {
                    self.field_replace(target, range, "", EditKind::Deleting, cx);
                }
            }
            _ => {}
        }
    }

    fn field_undo(&mut self, target: &TextTarget, cx: &mut Context<Self>) {
        self.field_restore(target, false, cx);
    }

    fn field_redo(&mut self, target: &TextTarget, cx: &mut Context<Self>) {
        self.field_restore(target, true, cx);
    }

    fn field_restore(&mut self, target: &TextTarget, redo: bool, cx: &mut Context<Self>) {
        let value = self.text_value(target);
        let restored = {
            let mut e = self.controls.edit.borrow_mut();
            let current = EditState { value, caret: e.caret, anchor: e.anchor };
            if redo { e.history.redo(current) } else { e.history.undo(current) }
        };
        let Some(state) = restored else { return };
        self.set_text_value(target, state.value.clone(), cx);
        let after = self.text_value(target);
        let mut e = self.controls.edit.borrow_mut();
        e.anchor = floor_boundary(&after, state.anchor);
        e.caret = floor_boundary(&after, state.caret);
        e.marked = None;
        cx.notify();
    }

    /// Where a press at `x` lands in the field's real text, even when dots are drawn for it.
    fn field_index_at(&self, target: &TextTarget, key: &str, x: Pixels) -> usize {
        let drawn = self.controls.edit.borrow().lines.get(key).map_or(0, |l| l.line.closest_index_for_x(x - l.origin_x + l.scroll));
        if self.field_masked(target) { from_masked(&self.text_value(target), drawn) } else { drawn }
    }

    /// Handles a press on the text. Places the caret, or selects a word or the whole line.
    fn field_mouse_down(&mut self, target: &TextTarget, key: &str, event: &MouseDownEvent, cx: &mut Context<Self>) {
        cx.stop_propagation();
        self.focus_field(target.clone(), cx);
        let value = self.text_value(target);
        let index = floor_boundary(&value, self.field_index_at(target, key, event.position.x));
        {
            let edit = self.controls.edit.clone();
            let mut e = edit.borrow_mut();
            match event.click_count {
                2 => {
                    e.anchor = word_left(&value, index.min(next_grapheme(&value, index)));
                    e.caret = word_right(&value, index);
                }
                n if n >= 3 => {
                    e.anchor = 0;
                    e.caret = value.len();
                }
                _ => {
                    e.caret = index;
                    if !event.modifiers.shift {
                        e.anchor = index;
                    }
                }
            }
            e.marked = None;
        }
        if event.click_count == 1 {
            let (target, key) = (target.clone(), key.to_string());
            let on_move = Rc::new(move |this: &mut Shell, position: Point<Pixels>, cx: &mut Context<Shell>| {
                if this.focused_target().as_ref() != Some(&target) {
                    return;
                }
                let value = this.text_value(&target);
                let index = floor_boundary(&value, this.field_index_at(&target, &key, position.x));
                this.controls.edit.borrow_mut().caret = index;
                cx.notify();
            });
            self.controls.pointer = Some(PointerDrag { on_move, on_end: None });
            self.dragging = Some(crate::shell::Drag::Pointer);
        }
        cx.notify();
    }

    /// the ✕ at a search field's end, only while it has text. it empties the field as typing would, and the field keeps the keyboard.
    pub fn search_clear(&mut self, target: TextTarget, id: &'static str, value: &str, size: f32, cx: &mut Context<Self>) -> Option<AnyElement> {
        if value.is_empty() {
            return None;
        }
        let t = self.theme;
        let ink = crate::ui::ink(id);
        let button = div()
            .id(id)
            .flex_none()
            .size(t.s(size))
            .child(crate::ui::hover_track(id))
            .opacity(ink.opacity(1.0))
            .child(crate::ui::icon(&t, "x-circle-fill", size, ink.color(t.t3(), t.t1())))
            .tooltip(crate::ui::tip(&t, tr("Clear Search")))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.clear_field(target.clone(), cx);
            }));
        Some(button.into_any_element())
    }

    pub fn clear_field(&mut self, target: TextTarget, cx: &mut Context<Self>) {
        self.focus_field(target.clone(), cx);
        let len = self.text_value(&target).len();
        self.field_replace(&target, 0..len, "", EditKind::Other, cx);
    }

    /// The text of a field, its caret and selection. Keeps the caret in view when the text is long.
    pub fn field_line(&mut self, target: &TextTarget, value: &str, placeholder: &'static str, key: &str, size_px: Pixels) -> AnyElement {
        self.edit_sync();
        let t = self.theme;
        let focused = self.focused_target().as_ref() == Some(target);
        let store = self.bounds_store(key);
        let edit = self.controls.edit.clone();
        let entity = self.this.upgrade();
        let focus_handle = self.focus.clone();
        let accent: Hsla = t.accent_hsla();
        let masked = self.field_masked(target);
        let real = value.to_string();
        let text = if masked { MASK.repeat(value.chars().count()) } else { value.to_string() };
        let (paint_key, down_key, down_target) = (key.to_string(), key.to_string(), target.clone());
        let weak = self.this.clone();
        let mut line = div()
            .id(gpui::ElementId::Name(format!("field:{key}").into()))
            .stop_field(&t, target.clone())
            .relative()
            .flex_1()
            .min_w(px(0.0))
            .h(size_px * 1.5)
            .flex()
            .items_center()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_size(size_px)
            .cursor_text()
            .child(track_bounds(store))
            .on_mouse_down(MouseButton::Left, move |event, _, cx| {
                let (target, key) = (down_target.clone(), down_key.clone());
                let _ = weak.update(cx, |this, cx| this.field_mouse_down(&target, &key, event, cx));
            });
        if value.is_empty() {
            line = line.child(div().flex_none().text_color(t.t3()).child(placeholder));
        }
        let field_stop = crate::focus::handle_for_field(target);
        let painter = canvas(
            |_, _, _| {},
            move |bounds: Bounds<Pixels>, _, window, cx| {
                let style = window.text_style();
                let font_size = style.font_size.to_pixels(window.rem_size());
                let line_height = window.line_height();
                let mut edit = edit.borrow_mut();
                if focused {
                    edit.current_line = Some(paint_key.clone());
                }
                let (caret, selection, marked) = if focused {
                    let map = |at: usize| if masked { to_masked(&real, at) } else { at };
                    let selection = edit.selection();
                    (floor_boundary(&text, map(edit.caret)), map(selection.start)..map(selection.end), edit.marked.clone().map(|m| map(m.start)..map(m.end)))
                } else {
                    (0, 0..0, None)
                };
                let run = |len: usize, underline: Option<UnderlineStyle>| TextRun { len, font: style.font(), color: style.color, background_color: None, underline, strikethrough: None };
                let runs = match &marked {
                    Some(m) if m.start <= m.end && m.end <= text.len() => {
                        let mark = Some(UnderlineStyle { color: Some(style.color), thickness: px(1.0), wavy: false });
                        [run(m.start, None), run(m.end - m.start, mark), run(text.len() - m.end, None)].into_iter().filter(|r| r.len > 0).collect()
                    }
                    _ => vec![run(text.len(), None)],
                };
                let shaped = window.text_system().shape_line(SharedString::from(text.clone()), font_size, &runs, None);
                // Keeps the caret inside the field. Otherwise the end of the text shows.
                let room = (bounds.size.width - px(2.0)).max(px(0.0));
                let scroll = if focused {
                    let caret_x = shaped.x_for_index(caret);
                    let mut scroll = edit.scroll;
                    if caret_x < scroll {
                        scroll = caret_x;
                    } else if caret_x > scroll + room {
                        scroll = caret_x - room;
                    }
                    scroll = scroll.min((shaped.width - room).max(px(0.0))).max(px(0.0));
                    edit.scroll = scroll;
                    scroll
                } else {
                    px(0.0)
                };
                let origin = point(bounds.origin.x - scroll, bounds.origin.y + (bounds.size.height - line_height) / 2.0);
                if focused && !selection.is_empty() {
                    let (from, to) = (shaped.x_for_index(selection.start.min(text.len())), shaped.x_for_index(selection.end.min(text.len())));
                    window.paint_quad(fill(Bounds::new(point(origin.x + from, origin.y), size(to - from, line_height)), accent.alpha(0.35)));
                }
                if !text.is_empty() {
                    shaped.paint(origin, line_height, TextAlign::Left, None, window, cx).ok();
                }
                if focused && selection.is_empty() {
                    let caret_h = font_size * 1.2;
                    let x = origin.x + shaped.x_for_index(caret);
                    window.paint_quad(fill(Bounds::new(point(x, origin.y + (line_height - caret_h) / 2.0), size(px(1.5), caret_h)), accent));
                }
                edit.lines.insert(paint_key.clone(), FieldLine { line: shaped, origin_x: bounds.origin.x, scroll });
                drop(edit);
                if focused && let Some(entity) = entity.clone() {
                    // A tab stop keeps focus. Otherwise typing comes through the root.
                    let input_focus = match &field_stop {
                        Some(stop) if stop.is_focused(window) => stop.clone(),
                        _ => {
                            if !focus_handle.is_focused(window) {
                                window.focus(&focus_handle, cx);
                            }
                            focus_handle.clone()
                        }
                    };
                    window.handle_input(&input_focus, ElementInputHandler::new(bounds, entity), cx);
                }
            },
        )
        .absolute()
        .left(px(0.0))
        .top(px(0.0))
        .size_full();
        line = line.child(painter);
        line.into_any_element()
    }

    pub fn text_line(&mut self, target: &TextTarget, value: &str, placeholder: &'static str, key: &str, _window: &mut Window) -> AnyElement {
        let size = self.theme.text(crate::theme::size::TEXT_MD);
        self.field_line(target, value, placeholder, key, size)
    }

    pub fn text_line_sized(&mut self, target: &TextTarget, value: &str, placeholder: &'static str, key: &str, size: Pixels, _window: &mut Window) -> AnyElement {
        self.field_line(target, value, placeholder, key, size)
    }

    fn field_for_input(&self) -> Option<(TextTarget, String)> {
        // An open menu takes the keyboard, except its submenu's filter.
        if self.menu.is_some() && !self.menu_find_focused() {
            return None;
        }
        let target = self.focused_target()?;
        let value = self.text_value(&target);
        Some((target, value))
    }

    fn input_bounds(&self, key_bounds: Bounds<Pixels>, value: &str, range: &Range<usize>) -> Option<Bounds<Pixels>> {
        let edit = self.controls.edit.borrow();
        let line = edit.current_line.as_ref().and_then(|k| edit.lines.get(k))?;
        let (start, end) = (range.start.min(value.len()), range.end.min(value.len()));
        let origin = point(key_bounds.origin.x + line.line.x_for_index(start) - line.scroll, key_bounds.origin.y);
        let width = line.line.x_for_index(end) - line.line.x_for_index(start);
        Some(Bounds::new(origin, size(width, key_bounds.size.height)))
    }
}

impl EntityInputHandler for Shell {
    fn text_for_range(&mut self, range_utf16: Range<usize>, adjusted: &mut Option<Range<usize>>, _: &mut Window, _: &mut Context<Self>) -> Option<String> {
        let (_, value) = self.field_for_input()?;
        let range = range_to_utf8(&value, &range_utf16);
        *adjusted = Some(range_to_utf16(&value, &range));
        Some(value[range].to_string())
    }

    fn selected_text_range(&mut self, _ignore_disabled: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        let (_, value) = self.field_for_input()?;
        let edit = self.controls.edit.borrow();
        let range = edit.selection();
        let range = floor_boundary(&value, range.start)..floor_boundary(&value, range.end);
        Some(UTF16Selection { range: range_to_utf16(&value, &range), reversed: edit.caret < edit.anchor })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        let (_, value) = self.field_for_input()?;
        let edit = self.controls.edit.borrow();
        edit.marked.as_ref().map(|m| range_to_utf16(&value, m))
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.controls.edit.borrow_mut().marked = None;
        cx.notify();
    }

    fn replace_text_in_range(&mut self, range_utf16: Option<Range<usize>>, text: &str, _: &mut Window, cx: &mut Context<Self>) {
        let Some((target, value)) = self.field_for_input() else { return };
        // Control characters arrive with their key press, which `field_key` already handled. An
        // empty string cancels a composition and must still clear the marked text.
        if !text.is_empty() && text.chars().all(char::is_control) {
            return;
        }
        self.edit_sync();
        let (marked, selection) = {
            let e = self.controls.edit.borrow();
            (e.marked.clone(), e.selection())
        };
        let kind = if marked.is_some() { EditKind::Skip } else { EditKind::Typing };
        let range = range_utf16.as_ref().map(|r| range_to_utf8(&value, r)).or(marked).unwrap_or(selection);
        self.field_replace(&target, range, text, kind, cx);
    }

    fn replace_and_mark_text_in_range(&mut self, range_utf16: Option<Range<usize>>, text: &str, new_selected: Option<Range<usize>>, _: &mut Window, cx: &mut Context<Self>) {
        let Some((target, value)) = self.field_for_input() else { return };
        self.edit_sync();
        let (marked, selection) = {
            let e = self.controls.edit.borrow();
            (e.marked.clone(), e.selection())
        };
        // A whole composition is one undo step, recorded when it starts.
        let kind = if marked.is_some() { EditKind::Skip } else { EditKind::Other };
        let range = range_utf16.as_ref().map(|r| range_to_utf8(&value, r)).or(marked).unwrap_or(selection);
        let start = range.start;
        self.field_replace(&target, range, text, kind, cx);
        let edit = self.controls.edit.clone();
        let mut edit = edit.borrow_mut();
        if !text.is_empty() {
            edit.marked = Some(start..start + text.len());
        } else {
            edit.marked = None;
        }
        if let Some(selected) = new_selected {
            let local = range_to_utf8(text, &selected);
            edit.anchor = start + local.start;
            edit.caret = start + local.end;
        }
    }

    fn bounds_for_range(&mut self, range_utf16: Range<usize>, element_bounds: Bounds<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        let (_, value) = self.field_for_input()?;
        let range = range_to_utf8(&value, &range_utf16);
        self.input_bounds(element_bounds, &value, &range)
    }

    fn character_index_for_point(&mut self, point: Point<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        let (_, value) = self.field_for_input()?;
        let edit = self.controls.edit.borrow();
        let line = edit.current_line.as_ref().and_then(|k| edit.lines.get(k))?;
        let index = line.line.closest_index_for_x(point.x - line.origin_x + line.scroll).min(value.len());
        Some(utf8_to_utf16(&value, floor_boundary(&value, index)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(value: &str) -> EditState {
        EditState { value: value.into(), caret: value.len(), anchor: value.len() }
    }

    #[test]
    fn typing_runs_undo_together() {
        let mut h = History::default();
        let t0 = Instant::now();
        h.record(state(""), EditKind::Typing, t0);
        h.record(state("a"), EditKind::Typing, t0 + Duration::from_millis(100));
        h.record(state("ab"), EditKind::Typing, t0 + Duration::from_millis(200));
        assert_eq!(h.undo.len(), 1);
        assert_eq!(h.undo(state("abc")), Some(state("")));
        assert_eq!(h.redo(state("")), Some(state("abc")));
    }

    #[test]
    fn a_pause_or_different_kind_starts_a_new_step() {
        let mut h = History::default();
        let t0 = Instant::now();
        h.record(state(""), EditKind::Typing, t0);
        h.record(state("a"), EditKind::Typing, t0 + Duration::from_secs(2));
        h.record(state("ab"), EditKind::Deleting, t0 + Duration::from_secs(2));
        h.record(state("a"), EditKind::Other, t0 + Duration::from_secs(2));
        assert_eq!(h.undo.len(), 4);
    }

    #[test]
    fn a_new_edit_clears_redo() {
        let mut h = History::default();
        h.record(state(""), EditKind::Other, Instant::now());
        h.undo(state("x"));
        assert_eq!(h.redo.len(), 1);
        h.record(state(""), EditKind::Other, Instant::now());
        assert!(h.redo.is_empty());
    }

    #[test]
    fn undo_on_an_empty_history_does_nothing() {
        let mut h = History::default();
        assert_eq!(h.undo(state("a")), None);
        assert_eq!(h.redo(state("a")), None);
    }
}
