//! The sidebar's playlist rows, in the order the user dragged them into.
//!
//! Navidrome has no playlist order, so it's stored on this device per server and user, keyed
//! by playlist id. Own playlists and other people's public ones reorder separately but share
//! one saved list.

use gpui::prelude::*;
use gpui::{AnyElement, Context, FontWeight, KeyDownEvent, MouseButton, MouseDownEvent, Pixels, Point, SharedString, Window, div, point, px};

use crate::i18n::{tr, trf};
use crate::ui::FullTextTip;
use crate::focus::FocusStop;
use crate::shell::{Drag, Layout, Route, Shell};
use crate::theme::size::*;
use crate::model::Section;
use crate::view_cards::ArtShape;

const ROW_H: f32 = 45.0;
/// Pixels of movement before a press counts as a drag, so a plain click still navigates.
const DRAG_THRESHOLD: f32 = 4.0;
/// How close to the list's edge, in pixels, auto-scroll starts.
const SCROLL_EDGE: f32 = 40.0;
const SCROLL_MAX_PER_FRAME: f32 = 14.0;
/// Rows built past the visible ones each way, so a fast scroll doesn't show gaps.
const OVERSCAN: usize = 4;

/// The display order of `current`, as indexes into it. Saved ids that still exist come first
/// in saved order, then new ones in default order.
pub fn merged_order(saved: &[String], current: &[&str]) -> Vec<usize> {
    let mut placed = vec![false; current.len()];
    let mut out = Vec::with_capacity(current.len());
    for id in saved {
        if let Some(i) = current.iter().position(|c| c == id)
            && !placed[i]
        {
            placed[i] = true;
            out.push(i);
        }
    }
    out.extend((0..current.len()).filter(|i| !placed[*i]));
    out
}

/// Moves the item at `from` to the insertion point `gap`, where 0 is before the first item.
/// Returns false when nothing would change.
pub fn move_to_gap<T>(items: &mut Vec<T>, from: usize, gap: usize) -> bool {
    if from >= items.len() || gap > items.len() || gap == from || gap == from + 1 {
        return false;
    }
    let item = items.remove(from);
    items.insert(if gap > from { gap - 1 } else { gap }, item);
    true
}

/// Which sidebar group a playlist belongs to, by its owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarGroup {
    Mine,
    Shared,
}

impl SidebarGroup {
    fn rows_key(self) -> &'static str {
        match self {
            SidebarGroup::Mine => "sidebar-rows",
            SidebarGroup::Shared => "sidebar-rows-shared",
        }
    }

    fn id(self) -> &'static str {
        match self {
            SidebarGroup::Mine => "playlist",
            SidebarGroup::Shared => "playlist-shared",
        }
    }

    fn cover_id(self) -> &'static str {
        match self {
            SidebarGroup::Mine => "playlist-cover",
            SidebarGroup::Shared => "playlist-shared-cover",
        }
    }
}

/// A press on a playlist row, which may become a drag.
pub struct SidebarDrag {
    id: SharedString,
    group: SidebarGroup,
    start: Point<Pixels>,
    pointer: Point<Pixels>,
    /// Where in the row the pointer grabbed it, from the row's top.
    grab: f32,
    moved: bool,
    /// The gap the row would land in, updated each frame.
    gap: usize,
}

impl Shell {
    pub(crate) fn order_key(&self) -> String {
        self.library.as_ref().map_or_else(|| "local".to_string(), |l| l.account_key())
    }

    /// Rebuilds the order whenever the playlist list changes.
    pub fn refresh_sidebar_order(&mut self) {
        let saved = self.prefs.sidebar_orders.get(&self.order_key()).map_or(&[][..], Vec::as_slice);
        let pinned: Vec<bool> = self.playlists.iter().map(|p| self.sidebar_pinned(p)).collect();
        let group = |section: Section| {
            let members: Vec<usize> = (0..self.playlists.len()).filter(|i| self.playlists[*i].section() == section && pinned[*i]).collect();
            let ids: Vec<&str> = members.iter().map(|i| self.playlists[*i].id.as_ref()).collect();
            merged_order(saved, &ids).into_iter().map(|k| members[k]).collect::<Vec<usize>>()
        };
        self.sidebar_order = group(Section::Mine);
        self.sidebar_shared = group(Section::Shared);
    }

    pub fn group_order(&self, group: SidebarGroup) -> &Vec<usize> {
        match group {
            SidebarGroup::Mine => &self.sidebar_order,
            SidebarGroup::Shared => &self.sidebar_shared,
        }
    }

    fn sidebar_reorder(&mut self, group: SidebarGroup, from: usize, gap: usize, cx: &mut Context<Self>) -> bool {
        let mut order = self.group_order(group).clone();
        if !move_to_gap(&mut order, from, gap) {
            return false;
        }
        match group {
            SidebarGroup::Mine => self.sidebar_order = order,
            SidebarGroup::Shared => self.sidebar_shared = order,
        }
        let mut ids: Vec<String> = self.sidebar_order.iter().chain(&self.sidebar_shared).filter_map(|i| self.playlists.get(*i)).map(|p| p.id.to_string()).collect();
        // unpinned playlists keep their place in the saved order.
        let saved = self.prefs.sidebar_orders.get(&self.order_key()).map_or(&[][..], Vec::as_slice);
        let all: Vec<&str> = self.playlists.iter().map(|p| p.id.as_ref()).collect();
        let rest: Vec<String> = merged_order(saved, &all).into_iter().map(|k| all[k].to_string()).filter(|id| !ids.contains(id)).collect();
        ids.extend(rest);
        let key = self.order_key();
        self.prefs.sidebar_orders.insert(key, ids);
        self.prefs.save();
        cx.notify();
        true
    }

    /// own playlists unless unpinned, others' only once pinned.
    pub fn sidebar_pinned(&self, p: &crate::model::PlaylistSummary) -> bool {
        self.prefs.sidebar_pins.get(&self.order_key()).map_or(p.section() == Section::Mine, |pins| pins.is_pinned(&p.id, p.section() == Section::Mine))
    }

    pub fn set_sidebar_pinned(&mut self, id: &SharedString, on: bool, cx: &mut Context<Self>) {
        let Some(mine) = self.playlists.iter().find(|p| &p.id == id).map(|p| p.section() == Section::Mine) else { return };
        let key = self.order_key();
        self.prefs.sidebar_pins.entry(key).or_default().set(id, on, mine);
        self.prefs.save();
        self.refresh_sidebar_order();
        cx.notify();
    }

    /// "Pin to Sidebar" or "Unpin from Sidebar" in a playlist's menus. others' private playlists, which only an admin sees, have no sidebar section.
    pub fn pin_row(&self, id: &SharedString) -> Option<crate::view_menu::MenuRow> {
        use crate::view_menu::MenuRow;
        let p = self.playlists.iter().find(|p| &p.id == id)?;
        if p.section() == Section::Other {
            return None;
        }
        let id = id.clone();
        Some(if self.sidebar_pinned(p) {
            MenuRow::item(tr("Unpin from Sidebar"), "push-pin-slash-regular", move |this, cx| this.set_sidebar_pinned(&id, false, cx))
        } else {
            MenuRow::item(tr("Pin to Sidebar"), "push-pin-regular", move |this, cx| this.set_sidebar_pinned(&id, true, cx))
        })
    }

    /// over a hovered cover, plays the playlist from the start; on the playlist that's playing it pauses and resumes.
    fn sidebar_cover_play(&self, group: SidebarGroup, stable: usize, id: &SharedString, name: &SharedString, cover: AnyElement, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let source = navia_core::queue_order::SourceLink::Playlist(id.to_string());
        let current = self.player.source.as_ref().and_then(|s| s.link.as_ref()) == Some(&source);
        let has_songs = self.playlists_full.iter().find(|p| &p.id == id).is_some_and(|p| p.song_count > 0);
        let wrap = div().relative().flex_none().size(t.s(SIDEBAR_COVER)).child(cover);
        if !current && !has_songs {
            return wrap.into_any_element();
        }
        let pause = current && self.player.playing;
        let label = if pause { trf("Pause {name}", &[("name", name)]) } else { trf("Play {name}", &[("name", name)]) };
        let key = gpui::ElementId::from((group.cover_id(), stable));
        let id = id.clone();
        wrap.id(key.clone())
            .child(crate::ui::hover_track(key.clone()))
            .children(crate::ui::art_overlay(&t, key, t.s(SIDEBAR_COVER_R), if pause { "pause-fill" } else { "play-fill" }, SIDEBAR_COVER_GLYPH))
            .tooltip(crate::ui::tip(&t, label))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                if current {
                    this.toggle_play(cx);
                } else {
                    this.play_playlist(id.clone(), false, cx);
                }
            }))
            .into_any_element()
    }

    fn sidebar_row_press(&mut self, id: SharedString, group: SidebarGroup, position: Point<Pixels>, grab: f32) {
        self.suppress_row_click = false;
        self.sidebar_drag = Some(SidebarDrag { id, group, start: position, pointer: position, grab, moved: false, gap: 0 });
        self.dragging = Some(Drag::SidebarPlaylist);
    }

    pub fn sidebar_drag_move(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(drag) = self.sidebar_drag.as_mut() else { return };
        drag.pointer = position;
        if !drag.moved && (f32::from(position.y - drag.start.y).abs() >= DRAG_THRESHOLD || f32::from(position.x - drag.start.x).abs() >= DRAG_THRESHOLD) {
            drag.moved = true;
        }
        if drag.moved {
            cx.notify();
        }
    }

    pub fn sidebar_drag_end(&mut self, cx: &mut Context<Self>) {
        self.sidebar_autoscrolling = false;
        let Some(drag) = self.sidebar_drag.take() else { return };
        if !drag.moved {
            return;
        }
        // The release would also count as a click on the starting row.
        self.suppress_row_click = true;
        if let Some(from) = self.sidebar_position(drag.group, &drag.id) {
            self.sidebar_reorder(drag.group, from, drag.gap, cx);
        }
    }

    fn sidebar_position(&self, group: SidebarGroup, id: &str) -> Option<usize> {
        self.group_order(group).iter().position(|i| self.playlists.get(*i).is_some_and(|p| p.id.as_ref() == id))
    }

    /// Handles Alt+Up and Alt+Down on a focused row.
    fn sidebar_key(&mut self, group: SidebarGroup, id: &SharedString, event: &KeyDownEvent, row_px: f32, cx: &mut Context<Self>) {
        let keys = &event.keystroke;
        if !keys.modifiers.alt || keys.modifiers.control || keys.modifiers.shift || keys.modifiers.platform {
            return;
        }
        let Some(from) = self.sidebar_position(group, id) else { return };
        let gap = match keys.key.as_str() {
            "up" if from > 0 => from - 1,
            "down" if from + 1 < self.group_order(group).len() => from + 2,
            _ => return,
        };
        if self.sidebar_reorder(group, from, gap, cx) {
            let moved_to = if gap > from { gap - 1 } else { gap };
            self.sidebar_reveal(group, moved_to, row_px);
            cx.stop_propagation();
        }
    }

    fn sidebar_reveal(&mut self, group: SidebarGroup, position: usize, row_px: f32) {
        let store = self.bounds_store(group.rows_key());
        let rows = *store.borrow();
        let view = self.sidebar_scroll.bounds();
        if rows.size.height <= px(0.0) || view.size.height <= px(0.0) {
            return;
        }
        let top = f32::from(rows.top()) + position as f32 * row_px;
        let offset = self.sidebar_scroll.offset();
        let delta = if top < f32::from(view.top()) {
            f32::from(view.top()) - top
        } else if top + row_px > f32::from(view.bottom()) {
            f32::from(view.bottom()) - top - row_px
        } else {
            return;
        };
        let max = f32::from(self.sidebar_scroll.max_offset().y);
        self.sidebar_scroll.set_offset(point(offset.x, px((f32::from(offset.y) + delta).clamp(-max, 0.0))));
    }

    /// Auto-scrolls near the list's edges and finds the gap under the dragged row.
    fn sidebar_drag_frame(&mut self, group: SidebarGroup, rows_top: f32, row_px: f32) {
        if !self.sidebar_drag.as_ref().is_some_and(|d| d.group == group) {
            return;
        }
        self.sidebar_autoscrolling = false;
        let view = self.sidebar_scroll.bounds();
        let count = self.group_order(group).len();
        let Some(drag) = self.sidebar_drag.as_mut().filter(|d| d.moved) else { return };
        let y = f32::from(drag.pointer.y);
        let (above, below) = (y - f32::from(view.top()), f32::from(view.bottom()) - y);
        let speed = if above < SCROLL_EDGE {
            SCROLL_MAX_PER_FRAME * ((SCROLL_EDGE - above) / SCROLL_EDGE).min(1.0)
        } else if below < SCROLL_EDGE {
            -SCROLL_MAX_PER_FRAME * ((SCROLL_EDGE - below) / SCROLL_EDGE).min(1.0)
        } else {
            0.0
        };
        let mut rows_top = rows_top;
        if speed != 0.0 {
            let offset = self.sidebar_scroll.offset();
            let max = f32::from(self.sidebar_scroll.max_offset().y);
            let next = (f32::from(offset.y) + speed).clamp(-max, 0.0);
            let moved = next - f32::from(offset.y);
            if moved != 0.0 {
                self.sidebar_scroll.set_offset(point(offset.x, px(next)));
                rows_top += moved;
                self.sidebar_autoscrolling = true;
            }
        }
        drag.gap = ((y - drag.grab - rows_top) / row_px).round().clamp(0.0, count as f32) as usize;
    }

    /// One group's playlist rows, building only those near the visible part.
    pub fn render_sidebar_playlists(&mut self, group: SidebarGroup, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let row_h = t.s(ROW_H);
        let row_px = f32::from(row_h);
        let count = self.group_order(group).len();
        let store = self.bounds_store(group.rows_key());
        let rows = *store.borrow();
        let view = self.sidebar_scroll.bounds();
        let measured = rows.size.height > px(0.0) && view.size.height > px(0.0);
        self.sidebar_drag_frame(group, f32::from(rows.top()), row_px);
        let (first, last) = if measured {
            let first = ((f32::from(view.top() - rows.top()) / row_px).floor().max(0.0) as usize).saturating_sub(OVERSCAN).min(count);
            let last = (first + (f32::from(view.size.height) / row_px).ceil() as usize + OVERSCAN * 2 + 1).min(count);
            (first, last)
        } else {
            (0, count.min(24))
        };
        let dragged = self.sidebar_drag.as_ref().filter(|d| d.moved && d.group == group).map(|d| (d.id.clone(), d.gap));
        let line = dragged.as_ref().and_then(|(id, gap)| self.sidebar_position(group, id).filter(|from| *gap != *from && *gap != *from + 1).map(|_| *gap));

        let mut column = div().relative().flex().flex_col().h(px(row_px * count as f32)).child(crate::ui_controls::track_bounds(store)).child(div().h(px(row_px * first as f32)));
        for position in first..last {
            let Some(summary) = self.group_order(group).get(position).and_then(|i| self.playlists.get(*i).map(|p| (*i, p.id.clone(), p.name.clone(), p.public))) else { continue };
            let (stable, playlist_id, name, public) = summary;
            // Your own public playlists get a quiet mark, since others on the server can see them.
            let mark = (group == SidebarGroup::Mine && public).then(|| crate::ui::icon(&t, "users-regular", 14.0, t.t3()));
            let download = self.download_mark(navia_core::library::collections::CollectionKind::Playlist, &playlist_id);
            let lifted = dragged.as_ref().is_some_and(|(id, _)| *id == playlist_id);
            let cover_id = self.playlist_art_key(&playlist_id);
            let cover = self.sidebar_cover(&cover_id, layout);
            let cover = self.sidebar_cover_play(group, stable, &playlist_id, &name, cover, cx);
            let route = Route::Playlist(playlist_id.clone());
            let on = self.route == route;
            let (press_id, key_id, menu_id) = (playlist_id.clone(), playlist_id.clone(), playlist_id.clone());
            // Songs carried here are added to your own regular playlists; the rest refuse them.
            let drop = self.drop_mark(format!("sidebar:{playlist_id}"), self.playlist_drop(&playlist_id, &name), R_MD, 0.0);
            column = column.child(
                div()
                    .id((group.id(), stable))
                    .relative()
                    .flex()
                    .flex_row()
                    .flex_none()
                    .items_center()
                    .gap(t.s(12.0))
                    .h(row_h)
                    .px(t.s(SIDEBAR_ITEM_PAD_X))
                    .rounded(t.s(R_MD))
                    .when(lifted, |d| d.opacity(0.35))
                    .when(on, |d| d.bg(t.hover()))
                    .when(!on, |d| d.hover(|s| s.bg(t.row())))
                    .on_mouse_down(MouseButton::Left, cx.listener(move |this, event: &MouseDownEvent, _, _| {
                        let store = this.bounds_store(group.rows_key());
                        let grab = (f32::from(event.position.y) - f32::from(store.borrow().top())).rem_euclid(row_px);
                        this.sidebar_row_press(press_id.clone(), group, event.position, grab);
                    }))
                    .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &MouseDownEvent, _, cx| this.open_playlist_menu(menu_id.clone(), event.position, cx)))
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _: &mut Window, cx| this.sidebar_key(group, &key_id, event, row_px, cx)))
                    .stop(&t)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if std::mem::take(&mut this.suppress_row_click) {
                            return;
                        }
                        this.navigate(route.clone(), cx);
                        this.sidebar_drawer = false;
                    }))
                    .children(drop)
                    .child(cover)
                    .child(div().id("name").flex_1().min_w(px(0.0)).truncate().text_color(if on { t.t_em() } else { t.t_nav() }).when(on, |d| d.font_weight(SIDEBAR_ACTIVE_WEIGHT)).child(name.clone()).full_text_tip(&t, name))
                    .child(div().flex_none().flex().flex_row().items_center().gap(t.s(8.0)).children(download).children(mark)),
            );
        }
        if let Some(gap) = line {
            column = column.child(div().absolute().left(t.s(8.0)).right(t.s(8.0)).top(px(row_px * gap as f32 - 1.0)).h(px(2.0)).rounded_full().bg(t.accent_hsla()));
        }
        column.into_any_element()
    }

    /// a playlist row's cover in the sidebar.
    pub fn sidebar_cover(&mut self, key: &SharedString, layout: &Layout) -> AnyElement {
        self.artwork(Some(key), "playlist-regular", SIDEBAR_COVER, ArtShape::Rounded(SIDEBAR_COVER_R), layout)
    }

    /// the row following the pointer, in the same box as the scrolling list.
    pub fn render_sidebar_lifted(&mut self, layout: &Layout) -> Option<AnyElement> {
        let t = self.theme;
        let (id, y, grab) = self.sidebar_drag.as_ref().filter(|d| d.moved).map(|d| (d.id.clone(), f32::from(d.pointer.y), d.grab))?;
        let name = self.playlists.iter().find(|p| p.id == id)?.name.clone();
        let cover_id = self.playlist_art_key(&id);
        let cover = self.sidebar_cover(&cover_id, layout);
        let top = y - grab - f32::from(self.sidebar_scroll.bounds().top());
        Some(
            div()
                .absolute()
                .left(t.s(12.0))
                .right(t.s(12.0))
                .top(px(top))
                .h(t.s(ROW_H))
                .flex()
                .flex_row()
                .items_center()
                .gap(t.s(12.0))
                .px(t.s(SIDEBAR_ITEM_PAD_X))
                .rounded(t.s(R_MD))
                .bg(gpui::rgb(crate::theme::PANEL_2))
                .font_weight(FontWeight::MEDIUM)
                .shadow(vec![crate::ui::shadow_ghost(&t)])
                .child(cover)
                .child(div().truncate().child(name))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{merged_order, move_to_gap};

    fn saved(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_saved_order_keeps_the_default() {
        assert_eq!(merged_order(&[], &["a", "b", "c"]), vec![0, 1, 2]);
    }

    #[test]
    fn new_go_last_and_deleted_drop_out() {
        assert_eq!(merged_order(&saved(&["c", "gone", "a"]), &["a", "b", "c", "d"]), vec![2, 0, 1, 3]);
    }

    #[test]
    fn move_to_gap_lands_where_the_line_was() {
        let mut v = vec![0, 1, 2, 3];
        assert!(move_to_gap(&mut v, 0, 3));
        assert_eq!(v, [1, 2, 0, 3]);
        let mut v = vec![0, 1, 2, 3];
        assert!(move_to_gap(&mut v, 3, 1));
        assert_eq!(v, [0, 3, 1, 2]);
        let mut v = vec![0, 1, 2];
        assert!(!move_to_gap(&mut v, 1, 1));
        assert!(!move_to_gap(&mut v, 1, 2));
        assert!(move_to_gap(&mut v, 1, 3));
        assert_eq!(v, [0, 2, 1]);
    }
}
