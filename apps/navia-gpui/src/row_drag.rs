//! Press-and-hold reordering of song rows, in the queue's Up Next and on editable playlists.
//!
//! Holding a row still for [`HOLD`] or moving it [`MOVE_PX`] lifts it. Quick clicks, double
//! clicks, modifier clicks and right clicks keep their usual meaning. Selected rows move together
//! as one card with a count, the other rows open a soft slot where they'd land, and the list
//! scrolls near its edges. Esc cancels. Rows can also be carried out onto a playlist or the
//! queue (`song_drop.rs`), and lists that can't be reordered only drag out.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{AnyElement, Bounds, Context, FontWeight, ListState, MouseDownEvent, Pixels, Point, anchored, deferred, div, px, relative};

use crate::song_drop::{QueueTake, SongTarget};

use crate::i18n::trn;
use crate::pages::Block;
use crate::shell::{Drag, Layout, ListId, Shell};
use crate::theme::motion;
use crate::theme::size::*;
use crate::ui::icon;

pub const HOLD: Duration = Duration::from_millis(180);
/// Moving this far while pressed lifts the row at once. Anything less is still a click.
const MOVE_PX: f32 = 6.0;
/// Distance from the list's top or bottom edge where scrolling starts, and the top speed per frame.
const EDGE_PX: f32 = 56.0;
const EDGE_SPEED: f32 = 18.0;

/// How far a list scrolls per frame with the pointer at `y` between `top` and `bottom`. Negative
/// near the top, positive near the bottom, faster nearer the edge.
pub fn edge_speed(y: f32, top: f32, bottom: f32) -> f32 {
    let (above, below) = (y - top, bottom - y);
    if above < EDGE_PX {
        -EDGE_SPEED * (1.0 - above.max(0.0) / EDGE_PX)
    } else if below < EDGE_PX {
        EDGE_SPEED * (1.0 - below.max(0.0) / EDGE_PX)
    } else {
        0.0
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Slide {
    from: f32,
    to: f32,
    start: Instant,
}

impl Slide {
    pub fn fixed(y: f32) -> Self {
        Self { from: y, to: y, start: Instant::now() }
    }

    fn progress(&self, now: Instant) -> f32 {
        motion::progress(now.saturating_duration_since(self.start).as_secs_f32(), motion::DRAG_SLIDE)
    }

    fn value(&self, now: Instant) -> f32 {
        // Ease out, approximating cubic-bezier(.2, .7, .3, 1).
        let t = 1.0 - (1.0 - self.progress(now)).powi(3);
        self.from + (self.to - self.from) * t
    }

    pub fn moving(&self, now: Instant) -> bool {
        self.from != self.to && self.progress(now) < 1.0
    }

    pub fn retarget(&mut self, to: f32, now: Instant) {
        if to != self.to {
            *self = Self { from: self.value(now), to, start: now };
        }
    }
}

/// Which list a drag reorders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DragList {
    /// The queue's Up Next. Positions count from the first song after the playing one.
    Queue,
    /// A page's song table, by its song list and the block of its first row. Reorders only on an
    /// editable playlist.
    Page { list: usize, first_block: usize },
    /// A page's compact rows, such as Search or Top Songs, which only drag out.
    Mini { list: usize },
    /// only drags out.
    Songs,
}

/// becomes a drag once held or moved.
pub struct RowDrag {
    pub list: DragList,
    pub pressed: usize,
    /// in list order; set when the row lifts.
    pub rows: Vec<usize>,
    pub start: Point<Pixels>,
    pub pointer: Point<Pixels>,
    /// Where in the pressed row the pointer grabbed it, from its top.
    pub grab: Pixels,
    pub lifted: bool,
    /// The gap the rows would land before, where the row count means the end, and where its empty
    /// slot starts on screen.
    pub before: Option<usize>,
    pub slot_y: Option<Pixels>,
    /// Each row's offset from its place, by position, plus the offset of whatever sits between the
    /// droppable rows and the rest. Rows not seen yet are at rest.
    shifts: HashMap<usize, Slide>,
    follow: Option<Slide>,
    /// The first frame after lifting, when visible rows slide from their own places.
    fresh: bool,
    /// for the floating copy's width.
    pub item: Bounds<Pixels>,
    pub count: usize,
    /// Rows from here on can be lifted but not dropped among, such as the queue's Autoplay section.
    pub limit: usize,
    /// Where Next from starts, when both it and Next in Queue show. Its heading parts above or below
    /// the gap and decides which section a drop joins.
    split: Option<usize>,
    split_shift: Option<Slide>,
    /// The drop joins Next in Queue. With one section showing, that section.
    pub into_first: bool,
    /// Whether the rows can be reordered in their list. Otherwise they only drag out.
    pub reorder: bool,
    /// The pointer is outside the list. The rows are back in place, dimmed, and a card follows it.
    pub out: bool,
    /// What the songs would drop on outside the list.
    pub target: Option<SongTarget>,
    /// Over the queue from another list, where the gap opens there.
    pub take: Option<QueueTake>,
    /// The playlists that take songs, read when the rows lift.
    pub takers: std::collections::HashSet<gpui::SharedString>,
    seq: u64,
}

impl Shell {
    fn drag_state(&self, list: DragList) -> ListState {
        match list {
            DragList::Queue => self.queue_list.clone(),
            DragList::Page { .. } | DragList::Mini { .. } | DragList::Songs => self.page.state.clone(),
        }
    }

    fn drag_item(&self, list: DragList, pos: usize) -> Option<usize> {
        match list {
            DragList::Queue => Some(self.queue_item_ix(pos)),
            DragList::Page { first_block, .. } => Some(first_block + pos),
            DragList::Mini { .. } | DragList::Songs => None,
        }
    }

    pub(crate) fn drag_songs(&self, list: DragList) -> Vec<u32> {
        match list {
            DragList::Queue => self.queue_view_list().0.as_ref().clone(),
            DragList::Page { list, .. } | DragList::Mini { list } => self.page.lists.get(list).map(|l| l.as_ref().clone()).unwrap_or_default(),
            DragList::Songs => self.order.clone(),
        }
    }

    pub(crate) fn drag_song(&self, list: DragList, pos: usize) -> Option<u32> {
        match list {
            DragList::Queue => self.queue_view_list().0.get(pos).copied(),
            DragList::Page { list, .. } | DragList::Mini { list } => self.page.lists.get(list).and_then(|l| l.get(pos).copied()),
            DragList::Songs => self.order.get(pos).copied(),
        }
    }

    fn drag_list_id(list: DragList) -> ListId {
        match list {
            DragList::Queue => ListId::Queue,
            DragList::Page { list, .. } | DragList::Mini { list } => ListId::Page(list),
            DragList::Songs => ListId::Songs,
        }
    }

    /// The drag a page table's rows start. Every table drags out, and editable playlists also
    /// reorder ([`Shell::page_reorders`]).
    pub fn page_drag_list(&self, list: usize) -> Option<DragList> {
        let first_block = self.page.blocks.iter().position(|b| matches!(b, Block::Table { list: l, pos: 0, .. } if *l == list))?;
        Some(DragList::Page { list, first_block })
    }

    /// Whether this page's table can be reordered now, as an editable playlist in its own order.
    pub fn page_reorders(&self, list: usize) -> bool {
        self.reorder_list == Some(list)
    }

    /// (lifted, carried) while inside their list.
    pub fn row_drag_marks(&self, list: DragList, pos: usize) -> (bool, bool) {
        match self.row_drag.as_ref().filter(|d| d.lifted && !d.out && d.list == list) {
            Some(d) => (d.pressed == pos, d.pressed != pos && d.rows.contains(&pos)),
            None => (false, false),
        }
    }

    /// How far a row is drawn from its place while rows are lifted. Rows close up over the moving
    /// ones and part at the gap. Queue rows also part for songs from another list.
    pub fn row_drag_shift(&self, list: DragList, pos: usize) -> Pixels {
        let now = Instant::now();
        let Some(d) = self.row_drag.as_ref().filter(|d| d.lifted) else { return px(0.0) };
        let slide = if d.list == list { d.shifts.get(&pos) } else { d.take.as_ref().filter(|_| list == DragList::Queue).and_then(|t| t.shifts.get(&pos)) };
        slide.map_or(px(0.0), |s| px(s.value(now)))
    }

    /// The same for whatever sits between the droppable rows and the rest, like the queue's
    /// Autoplay heading or the empty note above it.
    pub fn row_drag_follow_shift(&self, list: DragList) -> Pixels {
        let now = Instant::now();
        let Some(d) = self.row_drag.as_ref().filter(|d| d.lifted) else { return px(0.0) };
        let slide = if d.list == list { d.follow } else { d.take.as_ref().filter(|_| list == DragList::Queue).and_then(|t| t.follow) };
        slide.map_or(px(0.0), |s| px(s.value(now)))
    }

    /// The same for the Next from heading between the queue's sections.
    pub fn row_drag_split_shift(&self, list: DragList) -> Pixels {
        let now = Instant::now();
        let Some(d) = self.row_drag.as_ref().filter(|d| d.lifted) else { return px(0.0) };
        let slide = if d.list == list { d.split_shift } else { d.take.as_ref().filter(|_| list == DragList::Queue).and_then(|t| t.split) };
        slide.map_or(px(0.0), |s| px(s.value(now)))
    }

    pub fn row_dragging(&self) -> bool {
        self.row_drag.as_ref().is_some_and(|d| d.lifted)
    }

    /// modifier presses keep their meaning.
    pub fn row_press(&mut self, list: DragList, pos: usize, count: usize, event: &MouseDownEvent, cx: &mut Context<Self>) {
        self.suppress_row_click = false;
        // A press on an artist, album or genre link belongs to the link.
        if self.link_pressed.take().is_some_and(|at| at.elapsed() < Duration::from_millis(100)) {
            return;
        }
        let m = event.modifiers;
        if m.shift || m.control || m.platform || m.alt || count == 0 || event.click_count > 1 {
            return;
        }
        // rows that only drag out have no place to keep.
        let item = match self.drag_item(list, pos) {
            Some(ix) => match self.drag_state(list).bounds_for_item(ix) {
                Some(b) => b,
                None => return,
            },
            None => Bounds { origin: event.position, size: gpui::Size::default() },
        };
        self.row_drag_seq += 1;
        let seq = self.row_drag_seq;
        let (limit, split, into_first, reorder) = match list {
            DragList::Queue => {
                let s = self.queue_sections();
                // With one section showing, drops land in it. With neither, Autoplay songs become queued.
                (s.run + s.rest, (s.run > 0 && s.rest > 0).then_some(s.run), s.rest == 0, count > 1)
            }
            DragList::Page { list, .. } => (count, None, false, count > 1 && self.page_reorders(list)),
            DragList::Mini { .. } | DragList::Songs => (count, None, false, false),
        };
        self.row_drag = Some(RowDrag { list, pressed: pos, rows: vec![pos], start: event.position, pointer: event.position, grab: event.position.y - item.top(), lifted: false, before: None, slot_y: None, shifts: HashMap::new(), follow: None, fresh: true, item, count, limit, split, split_shift: None, into_first, reorder, out: false, target: None, take: None, takers: Default::default(), seq });
        self.dragging = Some(Drag::Rows);
        cx.spawn(async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
            cx.background_executor().timer(HOLD).await;
            let _ = this.update(cx, |this, cx| {
                if this.row_drag.as_ref().is_some_and(|d| d.seq == seq && !d.lifted) {
                    this.lift_rows(cx);
                }
            });
        })
        .detach();
    }

    fn lift_rows(&mut self, cx: &mut Context<Self>) {
        let Some(list) = self.row_drag.as_ref().map(|d| d.list) else { return };
        let songs = self.drag_songs(list);
        let id = Self::drag_list_id(list);
        let Some(drag) = self.row_drag.as_mut() else { return };
        let pressed = drag.pressed;
        let selected: Vec<usize> = if self.selection_list == id { (0..songs.len()).filter(|i| self.selection.contains(songs[*i])).collect() } else { Vec::new() };
        drag.rows = if selected.len() > 1 && selected.contains(&pressed) { selected } else { vec![pressed] };
        drag.lifted = true;
        drag.fresh = true;
        // Lists that can't be reordered start outside their list.
        drag.out = !drag.reorder;
        self.drop_marks.borrow_mut().clear();
        let takers = self.song_takers();
        let Some(drag) = self.row_drag.as_mut() else { return };
        drag.takers = takers;
        if drag.rows.len() == 1 {
            self.select_in(id);
            self.selection.click(pressed, &songs, false, false);
        }
        cx.notify();
    }

    pub fn rows_drag_move(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(drag) = self.row_drag.as_mut() else { return };
        drag.pointer = position;
        let moved = f32::from(position.x - drag.start.x).hypot(f32::from(position.y - drag.start.y));
        if !drag.lifted && moved >= MOVE_PX {
            self.lift_rows(cx);
        } else if drag.lifted {
            cx.notify();
        }
    }

    /// Lands the rows in the gap on release. The click that follows belongs to the drag.
    pub fn rows_drag_end(&mut self, cx: &mut Context<Self>) {
        self.rows_autoscrolling = false;
        let Some(drag) = self.row_drag.take() else { return };
        self.drop_marks.borrow_mut().clear();
        if !drag.lifted {
            return;
        }
        self.swallow_click(cx);
        cx.notify();
        if drag.out {
            let songs = self.drag_songs(drag.list);
            let carried: Vec<u32> = drag.rows.iter().filter_map(|r| songs.get(*r).copied()).collect();
            let take = drag.take.as_ref().filter(|t| t.slot_y.is_some()).map(|t| (t.at, t.queued));
            self.drop_songs_out(carried, drag.target, take, cx);
            return;
        }
        let Some(before) = drag.before else { return };
        let insert = before - drag.rows.iter().filter(|r| **r < before).count();
        if drag.rows.iter().enumerate().all(|(k, r)| *r == insert + k) {
            return;
        }
        match drag.list {
            DragList::Queue => self.queue_drop(&drag.rows, before, drag.into_first, cx),
            DragList::Page { list, .. } => self.move_playlist_rows(list, &drag.rows, before, cx),
            DragList::Mini { .. } | DragList::Songs => {}
        }
    }

    /// Esc while a row is lifted puts it back.
    pub fn rows_drag_cancel(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.row_dragging() {
            return false;
        }
        self.row_drag = None;
        self.drop_marks.borrow_mut().clear();
        self.rows_autoscrolling = false;
        self.dragging = None;
        self.swallow_click(cx);
        cx.notify();
        true
    }

    /// Swallows the click the release makes. It comes in the same dispatch, so the flag is cleared
    /// right after and a later click isn't lost.
    fn swallow_click(&mut self, cx: &mut Context<Self>) {
        self.suppress_row_click = true;
        let this = cx.entity().downgrade();
        cx.defer(move |cx| {
            let _ = this.update(cx, |this, _| this.suppress_row_click = false);
        });
    }

    /// Runs every frame while lifted. Scrolls near the list's edges and finds the gap under the
    /// pointer from the last row layout.
    pub fn row_drag_frame(&mut self) {
        self.rows_autoscrolling = false;
        let Some((list, pointer)) = self.row_drag.as_ref().filter(|d| d.lifted).map(|d| (d.list, d.pointer)) else { return };
        let state = self.drag_state(list);
        let view = state.viewport_bounds();
        // Inside the list it reorders. Over a target or anywhere else, the songs are outside it.
        let target = self.song_target_at(pointer, list);
        if let Some(drag) = self.row_drag.as_mut() {
            drag.out = !(drag.reorder && target.is_none() && view.contains(&pointer));
            drag.target = target;
        }
        if self.row_drag.as_ref().is_some_and(|d| d.out) {
            self.row_drag_out_frame();
            return;
        }
        // Each position's list item, since the queue's section headings sit between its rows.
        let sections = match list {
            DragList::Queue => Some(self.queue_sections()),
            DragList::Page { .. } | DragList::Mini { .. } | DragList::Songs => None,
        };
        let first_item = self.drag_item(list, 0).unwrap_or(0);
        let item_of = move |pos: usize| sections.map_or(first_item + pos, |s| s.item(pos));
        let split_item = sections.and_then(|s| s.from_heading);
        let split_bounds = split_item.and_then(|ix| state.bounds_for_item(ix));
        let Some(drag) = self.row_drag.as_mut() else { return };
        let y = f32::from(drag.pointer.y);
        let speed = edge_speed(y, f32::from(view.top()), f32::from(view.bottom()));
        if speed != 0.0 && view.size.height > px(0.0) {
            let before = state.logical_scroll_top();
            state.scroll_by(px(speed));
            self.rows_autoscrolling = state.logical_scroll_top() != before;
        }
        // Positions come from the list's layout, which the slides don't change, so sliding rows never
        // move the gap. Every row has the pressed row's height.
        let h = f32::from(drag.item.size.height);
        let moving_h = h * drag.rows.len() as f32;
        let moved_above = |pos: usize, rows: &[usize]| rows.iter().filter(|r| **r < pos).count() as f32 * h;
        // The gap goes before the first resting row whose middle, with the moving rows taken out, is
        // below the lifted card's middle.
        let center = y - f32::from(drag.grab) + h / 2.0;
        let top_item = state.logical_scroll_top().item_ix;
        let start = (0..drag.count).find(|p| item_of(*p) >= top_item).unwrap_or(drag.count).saturating_sub(1);
        let mut seen: Vec<(usize, f32)> = Vec::new();
        for pos in start..drag.count {
            let Some(b) = state.bounds_for_item(item_of(pos)) else {
                if !seen.is_empty() {
                    break;
                }
                continue;
            };
            seen.push((pos, f32::from(b.top())));
            if b.top() > view.bottom() {
                break;
            }
        }
        let mut before = None;
        for &(pos, top) in seen.iter().filter(|(p, _)| *p < drag.limit && !drag.rows.contains(p)) {
            if center < top - moved_above(pos, &drag.rows) + h / 2.0 {
                before = Some(pos);
                break;
            }
        }
        let last_seen = seen.iter().filter(|(p, _)| *p < drag.limit).map(|(p, _)| *p).max();
        let mut before = before.unwrap_or_else(|| last_seen.map_or(drag.limit, |p| if p + 1 >= drag.limit { drag.limit } else { p + 1 }));
        // Between sections, the heading's middle decides which section the drop joins, and the gap
        // stays on that side of it.
        if let Some(split) = drag.split {
            if let Some(b) = split_bounds {
                drag.into_first = center < f32::from(b.top()) - moved_above(split, &drag.rows) + f32::from(b.size.height) / 2.0;
            } else {
                // The heading is off screen, so use the gap's side of it.
                drag.into_first = before < split;
            }
            before = if drag.into_first { before.min(split) } else { before.max(split) };
        }
        drag.before = Some(before);
        // Retarget every visible row.
        let now = Instant::now();
        let fresh = std::mem::take(&mut drag.fresh);
        for &(pos, _) in &seen {
            if drag.rows.contains(&pos) {
                continue;
            }
            let target = -moved_above(pos, &drag.rows) + if pos >= before { moving_h } else { 0.0 };
            drag.shifts.entry(pos).or_insert_with(|| Slide::fixed(if fresh { 0.0 } else { target })).retarget(target, now);
        }
        let follow = moving_h - moved_above(drag.limit, &drag.rows);
        drag.follow.get_or_insert_with(|| Slide::fixed(if fresh { 0.0 } else { follow })).retarget(follow, now);
        if let Some(split) = drag.split {
            let target = -moved_above(split, &drag.rows) + if drag.into_first { moving_h } else { 0.0 };
            drag.split_shift.get_or_insert_with(|| Slide::fixed(if fresh { 0.0 } else { target })).retarget(target, now);
        }
        // The slot is the space the rows have opened, above the row at the gap as drawn now or under
        // the last droppable row.
        let drawn_top = |pos: usize| seen.iter().find(|(p, _)| *p == pos).map(|(_, top)| top + drag.shifts.get(&pos).map_or(0.0, |s| s.value(now)));
        let last_staying = seen.iter().filter(|(p, _)| *p < drag.limit && !drag.rows.contains(p)).map(|(p, _)| *p).max();
        let at_split = drag.split.is_some_and(|sp| drag.into_first && before == sp);
        let mut slot = if at_split {
            // The gap closes Next in Queue, above the Next from heading as drawn now.
            let split = drag.split.unwrap_or(0);
            let staying = seen.iter().filter(|(p, _)| *p < split && !drag.rows.contains(p)).map(|(p, _)| *p).max();
            match staying.and_then(|p| drawn_top(p)) {
                Some(top) => Some(top + h),
                None => split_bounds.map(|b| f32::from(b.top()) + drag.split_shift.map_or(0.0, |s| s.value(now)) - moving_h),
            }
        } else if before < drag.limit {
            drawn_top(before).map(|top| top - moving_h)
        } else {
            None
        };
        if slot.is_none() && before >= drag.limit {
            slot = last_staying.filter(|p| *p < before).and_then(|p| drawn_top(p)).map(|top| top + h);
        }
        if slot.is_none() {
            // Every droppable row is moving, or there are none. Use where the first of them was, or
            // under the heading above an empty list.
            slot = match drag.rows.first().filter(|r| **r < drag.limit) {
                Some(r) => seen.iter().find(|(p, _)| p == r).map(|(_, top)| *top),
                // The queue's note under its Up Next heading.
                None => state.bounds_for_item(self.queue_rows_first).map(|b| f32::from(b.top())),
            };
        }
        drag.slot_y = slot.map(px);
        let sliding = drag.shifts.values().any(|s| s.moving(now)) || drag.follow.is_some_and(|s| s.moving(now)) || drag.split_shift.is_some_and(|s| s.moving(now));
        self.rows_autoscrolling |= sliding;
    }

    /// Runs while outside the list. The rows slide back and the gap closes while the songs look
    /// for a target.
    fn row_drag_out_frame(&mut self) {
        let now = Instant::now();
        if let Some(drag) = self.row_drag.as_mut() {
            drag.before = None;
            drag.slot_y = None;
            drag.fresh = false;
            drag.shifts.values_mut().for_each(|s| s.retarget(0.0, now));
            for s in [&mut drag.follow, &mut drag.split_shift].into_iter().flatten() {
                s.retarget(0.0, now);
            }
        }
        let moving = self.song_drag_out_frame();
        let sliding = self.row_drag.as_ref().is_some_and(|d| d.shifts.values().any(|s| s.moving(now)) || d.follow.is_some_and(|s| s.moving(now)) || d.split_shift.is_some_and(|s| s.moving(now)));
        self.rows_autoscrolling = moving || sliding;
    }

    /// The empty slot and floating card, drawn above the list while a row is lifted and clipped to
    /// the list's bounds.
    pub fn render_row_drag_overlay(&mut self, layout: &Layout) -> Option<AnyElement> {
        let t = self.theme;
        if self.row_drag.as_ref().is_some_and(|d| d.lifted && d.out) {
            return self.render_song_drag_out(layout);
        }
        let drag = self.row_drag.as_ref().filter(|d| d.lifted)?;
        let (list, pressed, rows, item, pointer, grab, slot_y) = (drag.list, drag.pressed, drag.rows.len(), drag.item, drag.pointer, drag.grab, drag.slot_y);
        let view = self.drag_state(list).viewport_bounds();
        let (left, width, inset_y, card_h) = match list {
            DragList::Queue => (item.left() + t.s(18.0), item.size.width - t.s(36.0), t.s(2.0), t.s(58.0)),
            DragList::Page { .. } | DragList::Mini { .. } | DragList::Songs => {
                let m = crate::pages::PageMetrics::of(layout);
                let under = self.sidebar_under(layout);
                (item.left() + under + t.s(m.gutter - 6.0), px(f32::from(layout.main_width) - f32::from(t.s(m.gutter - 6.0)) * 2.0), t.s(9.0), item.size.height - t.s(18.0))
            }
        };
        let song = self.drag_song(list, pressed)?;
        let floating = self.render_drag_float(list, song, width, layout);
        // Everything is placed from the list's top left and clipped to it.
        let (x, y0) = (left - view.left(), view.top());
        let max_top = (view.size.height - item.size.height).max(px(0.0));
        let card_top = (pointer.y - grab - y0).clamp(px(0.0), max_top) + (item.size.height - card_h) / 2.0;
        let mut layer = div().relative().w(view.size.width).h(view.size.height).overflow_hidden();
        if let Some(slot) = slot_y {
            let height = item.size.height * rows as f32 - inset_y * 2.0;
            layer = layer.child(div().absolute().left(x).top(slot - y0 + inset_y).w(width).h(height).rounded(t.s(R_MD)).bg(t.drag_slot()).border_1().border_color(t.drag_slot_edge()));
        }
        if rows > 1 {
            // A second card peeks out under the first when several rows move together.
            let under: gpui::Hsla = match list {
                DragList::Queue => crate::color::tint_vivid(self.tint()).mix(crate::color::Srgb::hex(0xffffff), 0.96).hsla(0.88),
                DragList::Page { .. } | DragList::Mini { .. } | DragList::Songs => t.drag_card_under().into(),
            };
            layer = layer.child(div().absolute().left(x + t.s(8.0)).top(card_top + t.s(6.0)).w(width - t.s(16.0)).h(card_h).rounded(t.s(R_MD)).bg(under).border_1().border_color(t.drag_card_under_edge()));
        }
        // The table's float is the whole row with its card inset. The queue's float is the card itself.
        let float_top = match list {
            DragList::Queue => card_top,
            DragList::Page { .. } | DragList::Mini { .. } | DragList::Songs => card_top - (item.size.height - card_h) / 2.0,
        };
        layer = layer.child(div().absolute().left(x).top(float_top).child(floating));
        if rows > 1 {
            layer = layer.child(crate::ui::count_badge(&t, trn("{count} song", "{count} songs", rows)).absolute().right(view.right() - left - width + t.s(44.0)).top(card_top - t.s(8.0)));
        }
        Some(deferred(anchored().position(view.origin).child(layer)).with_priority(5).into_any_element())
    }

    fn render_drag_float(&mut self, list: DragList, song_ix: u32, width: Pixels, layout: &Layout) -> AnyElement {
        let t = self.theme;
        let song = match list {
            DragList::Queue => self.song_by_rowid(song_ix),
            DragList::Page { .. } | DragList::Mini { .. } | DragList::Songs => self.song_at(song_ix).unwrap_or_default(),
        };
        if let DragList::Page { .. } | DragList::Mini { .. } | DragList::Songs = list {
            return self.render_lifted_table_row(song_ix, width, layout);
        }
        let thumb_size = 42.0;
        let thumb_px = (f32::from(t.s(thumb_size)) * layout.scale).round() as u32;
        let thumb: AnyElement = match self.thumb_faded(&song.album_id, thumb_px) {
            Some((image, _)) => crate::ui::image(image).size(t.s(thumb_size)).rounded(t.s(R_SM)).into_any_element(),
            None => div().size(t.s(thumb_size)).rounded(t.s(R_SM)).bg(gpui::rgb(crate::theme::PANEL_2)).into_any_element(),
        };
        let base = div().w(width).text_size(t.text(TEXT_MD)).flex().flex_row().items_center().rounded(t.s(R_MD)).shadow(vec![crate::ui::shadow_lift(&t)]).border_1().border_color(t.drag_card_edge()).cursor(gpui::CursorStyle::ClosedHand);
        base
            .h(t.s(58.0))
            .px(t.s(8.0))
            .gap(t.s(11.0))
            // The vivid tint with a little white at 88% opacity, a lighter version of the dock's color.
            .bg(crate::color::tint_vivid(self.tint()).mix(crate::color::Srgb::hex(0xffffff), 0.932).hsla(0.88))
            .text_color(t.t_on_art())
            .child(div().flex_none().child(thumb))
            .child(div().flex_1().min_w(px(0.0)).line_height(relative(1.3)).child(div().truncate().font_weight(FontWeight::MEDIUM).child(song.title.clone())).child(div().truncate().text_size(t.text(TEXT_SM)).opacity(0.7).child(song.artist.clone())))
            .child(icon(&t, "dots-six-vertical-bold", 18.0, gpui::Hsla::from(t.t_on_art()).alpha(0.9)))
            .into_any_element()
    }

    /// The lifted table row, its cells at the table's column widths over the drag card. The card
    /// sits 9 tokens inside the row's height and 6 past its sides.
    fn render_lifted_table_row(&mut self, song_ix: u32, width: Pixels, layout: &Layout) -> AnyElement {
        let t = self.theme;
        let copy = self.lifted_row_copy(song_ix, layout);
        div()
            .relative()
            .w(width)
            .h(t.s(ROW_H))
            .text_size(t.text(TEXT_MD))
            .cursor(gpui::CursorStyle::ClosedHand)
            .child(div().absolute().left(px(0.0)).right(px(0.0)).top(t.s(9.0)).bottom(t.s(9.0)).rounded(t.s(R_MD)).bg(t.drag_card()).shadow(vec![crate::ui::shadow_lift(&t)]).border_1().border_color(t.drag_card_edge()))
            .child(div().absolute().left(t.s(6.0)).top(px(0.0)).child(copy))
            .into_any_element()
    }
}
