//! Songs dragged out of their list onto a playlist or the queue.
//!
//! The source list owns the drag (`row_drag.rs`). Once the pointer leaves it, the rows return
//! to their places dimmed and a card follows the pointer. Drop targets leave marks while songs
//! are carried ([`Shell::drop_mark`]), and the mark under the pointer decides the target.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use gpui::prelude::*;
use gpui::{AnyElement, Bounds, Context, FontWeight, Pixels, Point, SharedString, anchored, canvas, deferred, div, point, px, relative};

use crate::focus::FocusStop;
use crate::i18n::{tr, trn, trnf};
use crate::row_drag::{DragList, Slide};
use crate::shell::{Layout, Shell};
use crate::theme::size::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SongTarget {
    /// One of the user's own regular playlists. Songs are added, asking about duplicates.
    Playlist { id: SharedString, name: SharedString },
    /// The queue list. Songs go in at the gap.
    Queue,
    /// The queue tab or the bar's queue button, meaning the end of Next in Queue.
    QueueEnd,
    /// A playlist that can't take songs, such as a smart one or someone else's, or the queue during a station.
    Refused,
    /// Somewhere that is neither the list nor a target, like the sidebar or the dock.
    Surface,
}

impl SongTarget {
    /// Whether the target gets the fill and ring while songs are over it. The queue opens a gap instead.
    fn lights(&self) -> bool {
        matches!(self, SongTarget::Playlist { .. } | SongTarget::QueueEnd)
    }
}

/// Where each drop target was last drawn, clipped to its visible part. Filled while songs are
/// carried and emptied on drop.
pub type DropMarks = std::rc::Rc<std::cell::RefCell<HashMap<SharedString, (Bounds<Pixels>, SongTarget)>>>;

/// Songs from another list over the queue. The gap they'd land in, and how rows and headings
/// part around it.
#[derive(Default)]
pub struct QueueTake {
    /// The position among the songs after the playing one, and whether it falls in Next in Queue
    /// rather than Next from.
    pub at: usize,
    pub queued: bool,
    /// in window coordinates, while over the queue.
    pub slot_y: Option<Pixels>,
    pub gap_h: Pixels,
    pub shifts: HashMap<usize, Slide>,
    pub split: Option<Slide>,
    pub follow: Option<Slide>,
}

impl QueueTake {
    fn sliding(&self, now: Instant) -> bool {
        self.shifts.values().any(|s| s.moving(now)) || self.split.is_some_and(|s| s.moving(now)) || self.follow.is_some_and(|s| s.moving(now))
    }
}

impl Shell {
    /// Registers this element as a drop target while songs are carried, highlighted when they're
    /// over it. The element must be `relative`, and this goes first among its children so the
    /// highlight sits under its content. `pad` extends the highlight and `radius` rounds it.
    pub fn drop_mark(&self, key: impl Into<SharedString>, target: SongTarget, radius: f32, pad: f32) -> Option<AnyElement> {
        let drag = self.row_drag.as_ref().filter(|d| d.lifted)?;
        let t = self.theme;
        let over = drag.out && target.lights() && drag.target.as_ref() == Some(&target);
        let (key, marks) = (key.into(), self.drop_marks.clone());
        let mark = canvas(
            move |bounds, window, _| {
                // Only the visible part counts, so rows scrolled past a list's edge don't.
                let shown = bounds.intersect(&window.content_mask().bounds);
                marks.borrow_mut().insert(key.clone(), (shown, target.clone()));
            },
            |_, _, _, _| {},
        )
        .absolute()
        .inset_0();
        let highlight = over.then(|| {
            let pad = t.s(pad);
            div().absolute().top(-pad).bottom(-pad).left(-pad).right(-pad).rounded(t.s(radius)).bg(t.drop_target()).border(t.s(DROP_RING)).border_color(t.drop_target_edge())
        });
        Some(div().absolute().inset_0().child(mark).children(highlight).into_any_element())
    }

    /// The smallest target under a window point, from where the marks were last drawn. A queue's
    /// own songs don't drop back onto the queue.
    pub(crate) fn song_target_at(&self, p: Point<Pixels>, from: DragList) -> Option<SongTarget> {
        let marks = self.drop_marks.borrow();
        let area = |b: &Bounds<Pixels>| f32::from(b.size.width) * f32::from(b.size.height);
        let found = marks.values().filter(|(b, _)| b.size.width > px(0.0) && b.size.height > px(0.0) && b.contains(&p)).min_by(|a, b| area(&a.0).total_cmp(&area(&b.0)))?;
        match found.1 {
            // the queue's rows reorder inside it, and don't drop on its tab or button.
            SongTarget::Queue if from == DragList::Queue => None,
            SongTarget::QueueEnd if from == DragList::Queue => Some(SongTarget::Surface),
            ref target => Some(target.clone()),
        }
    }

    /// A playlist as a drop target. Only the user's own regular playlists take songs.
    pub fn playlist_drop(&self, id: &SharedString, name: &SharedString) -> SongTarget {
        match self.row_drag.as_ref() {
            Some(d) if d.takers.contains(id) => SongTarget::Playlist { id: id.clone(), name: name.clone() },
            _ => SongTarget::Refused,
        }
    }

    /// read once when songs lift.
    pub fn song_takers(&self) -> HashSet<SharedString> {
        let targets: Vec<(SharedString, SharedString)> = match self.core.as_ref() {
            Some(core) => core.lib.playlist_targets(),
            None => self.playlists_full.iter().filter(|p| p.mine && !p.readonly).map(|p| (p.id.clone(), p.name.clone())).collect(),
        };
        targets.into_iter().map(|(id, _)| id).collect()
    }

    /// The queue list as a drop target. It takes songs while one is playing, but not during a station.
    pub fn queue_drop_target(&self) -> SongTarget {
        if self.player.station.is_some() || self.player.song_ix.is_none() { SongTarget::Refused } else { SongTarget::Queue }
    }

    /// Runs every frame while songs are out of their list. Finds the target, scrolls the sidebar
    /// near its edges and places the queue gap. Returns whether anything still moves.
    pub fn song_drag_out_frame(&mut self) -> bool {
        let Some((pointer, list)) = self.row_drag.as_ref().map(|d| (d.pointer, d.list)) else { return false };
        let mut moving = false;
        // The sidebar scrolls near its top or bottom edge, so every playlist can be reached.
        let view = self.sidebar_scroll.bounds();
        if view.contains(&pointer) {
            let delta = crate::row_drag::edge_speed(f32::from(pointer.y), f32::from(view.top()), f32::from(view.bottom()));
            let offset = self.sidebar_scroll.offset();
            let max = f32::from(self.sidebar_scroll.max_offset().y);
            let next = (f32::from(offset.y) - delta).clamp(-max, 0.0);
            if next != f32::from(offset.y) {
                self.sidebar_scroll.set_offset(point(offset.x, px(next)));
                moving = true;
            }
        }
        let over_queue = self.row_drag.as_ref().is_some_and(|d| d.target == Some(SongTarget::Queue)) && list != DragList::Queue;
        if over_queue {
            moving |= self.queue_take_frame();
        } else if let Some(take) = self.row_drag.as_mut().and_then(|d| d.take.as_mut()) {
            // The pointer left the queue, so its rows slide back before the take ends.
            let now = Instant::now();
            take.slot_y = None;
            take.shifts.values_mut().for_each(|s| s.retarget(0.0, now));
            for s in [&mut take.split, &mut take.follow].into_iter().flatten() {
                s.retarget(0.0, now);
            }
            if take.sliding(now) {
                moving = true;
            } else if let Some(d) = self.row_drag.as_mut() {
                d.take = None;
            }
        }
        moving
    }

    /// Finds the gap among the queue's rows under the pointer. Above Next from's heading it's Next
    /// in Queue, below it Next from. Rows from the gap down part by the height of the carried songs.
    fn queue_take_frame(&mut self) -> bool {
        let s = self.queue_sections();
        let state = self.queue_list.clone();
        let view = state.viewport_bounds();
        let Some(drag) = self.row_drag.as_ref() else { return false };
        let (y, carried) = (f32::from(drag.pointer.y), drag.rows.len());
        let mut moving = false;
        let speed = crate::row_drag::edge_speed(y, f32::from(view.top()), f32::from(view.bottom()));
        if speed != 0.0 && view.size.height > px(0.0) {
            let before = state.logical_scroll_top();
            state.scroll_by(px(speed));
            moving = state.logical_scroll_top() != before;
        }
        let n = s.run + s.rest;
        let h = f32::from(self.theme.s(crate::view_queue::ROW_HEIGHT));
        let head = s.from_heading.filter(|_| s.rest > 0).and_then(|ix| state.bounds_for_item(ix));
        // Visible rows with their tops from the list's layout, which the slides don't change.
        let seen: Vec<(usize, f32)> = (0..n + s.auto_rows).filter_map(|p| state.bounds_for_item(s.item(p)).map(|b| (p, f32::from(b.top())))).collect();
        let mut at = None;
        let mut above_head = false;
        for &(pos, top) in seen.iter().filter(|(p, _)| *p < n) {
            if pos == s.run
                && let Some(b) = head
                && y < f32::from(b.top()) + f32::from(b.size.height) / 2.0
            {
                (at, above_head) = (Some(pos), true);
                break;
            }
            if y < top + h / 2.0 {
                at = Some(pos);
                break;
            }
        }
        let last = seen.iter().filter(|(p, _)| *p < n).next_back().copied();
        let at = at.unwrap_or_else(|| last.map_or(n, |(p, _)| (p + 1).min(n)));
        let queued = head.is_none() && s.rest == 0 || at < s.run || above_head;
        let gap = h * carried as f32;
        let slot = if above_head {
            head.map(|b| f32::from(b.top()))
        } else if at < n {
            seen.iter().find(|(p, _)| *p == at).map(|(_, top)| *top)
        } else {
            match last {
                Some((_, top)) => Some(top + h),
                // Nothing after the playing song, so the gap goes right under it.
                None => state.bounds_for_item(self.queue_rows_first).map(|b| f32::from(b.top())),
            }
        };
        let now = Instant::now();
        let Some(drag) = self.row_drag.as_mut() else { return moving };
        let fresh = drag.take.is_none();
        let take = drag.take.get_or_insert_with(QueueTake::default);
        (take.at, take.queued, take.gap_h, take.slot_y) = (at, queued, px(gap), slot.map(px));
        for &(pos, _) in &seen {
            let target = if pos >= at { gap } else { 0.0 };
            take.shifts.entry(pos).or_insert_with(|| Slide::fixed(if fresh { 0.0 } else { target })).retarget(target, now);
        }
        // Next from's heading moves down when the gap is above it. Autoplay's heading and the empty
        // note always do.
        let split = if at < s.run || above_head { gap } else { 0.0 };
        take.split.get_or_insert_with(|| Slide::fixed(0.0)).retarget(split, now);
        take.follow.get_or_insert_with(|| Slide::fixed(0.0)).retarget(gap, now);
        moving | take.sliding(now)
    }

    /// Drops the songs on the target under the pointer.
    pub fn drop_songs_out(&mut self, songs: Vec<u32>, target: Option<SongTarget>, take: Option<(usize, bool)>, cx: &mut Context<Self>) {
        if songs.is_empty() {
            return;
        }
        match target {
            Some(SongTarget::Playlist { id, name }) => self.add_songs_to_playlist(id, name, songs, cx),
            Some(SongTarget::Queue) => {
                let run = self.player.queued_run;
                let (at, queued) = take.unwrap_or((run, true));
                self.queue_songs_at(&songs, at, queued, cx);
            }
            Some(SongTarget::QueueEnd) => self.add_to_queue(&songs, cx),
            _ => {}
        }
    }

    /// Whether a row is dimmed because it's being carried out of its list.
    pub fn row_drag_out(&self, list: DragList, pos: usize) -> bool {
        self.row_drag.as_ref().is_some_and(|d| d.lifted && d.out && d.list == list && d.rows.contains(&pos))
    }

    /// The card that follows the pointer outside the list, and the queue's slot when over it.
    pub fn render_song_drag_out(&mut self, layout: &Layout) -> Option<AnyElement> {
        let t = self.theme;
        let drag = self.row_drag.as_ref().filter(|d| d.lifted && d.out)?;
        let (list, pressed, count, pointer) = (drag.list, drag.pressed, drag.rows.len(), drag.pointer);
        let slot = drag.take.as_ref().and_then(|take| take.slot_y.map(|y| (y, take.gap_h)));
        let song_ix = self.drag_song(list, pressed)?;
        let song = match list {
            DragList::Queue => self.song_by_rowid(song_ix),
            _ => self.song_at(song_ix).unwrap_or_default(),
        };
        let thumb_px = (f32::from(t.s(THUMB)) * layout.scale).round() as u32;
        let thumb: AnyElement = match self.thumb_faded(&song.album_id, thumb_px) {
            Some((image, _)) => crate::ui::image(image).size(t.s(THUMB)).rounded(t.s(R_SM)).into_any_element(),
            None => div().size(t.s(THUMB)).rounded(t.s(R_SM)).bg(t.drag_card_under()).into_any_element(),
        };
        let (w, h) = (t.s(DRAG_OUT_CARD_W), t.s(THUMB + 16.0));
        let card = div()
            .relative()
            .w(w)
            .h(h)
            .flex()
            .flex_row()
            .items_center()
            .gap(t.s(12.0))
            .pl(t.s(8.0))
            .pr(t.s(12.0))
            .rounded(t.s(R_MD))
            .bg(t.drag_card())
            .border_1()
            .border_color(t.drag_card_edge())
            .shadow(vec![crate::ui::shadow_lift(&t)])
            .text_size(t.text(TEXT_MD))
            .child(div().flex_none().child(thumb))
            .child(div().flex_1().min_w(px(0.0)).line_height(relative(1.3)).child(div().truncate().font_weight(FontWeight::MEDIUM).text_color(t.t1()).child(song.title.clone())).child(div().truncate().text_size(t.text(TEXT_SM)).text_color(t.t2()).child(song.artist.clone())));
        let card = if count > 1 {
            // For several songs, a second card under the first with the count on its corner.
            div()
                .relative()
                .w(w)
                .h(h + t.s(6.0))
                .child(div().absolute().left(px(0.0)).right(px(0.0)).top(t.s(6.0)).h(h).rounded(t.s(R_MD)).bg(t.drag_card_under()).border_1().border_color(t.drag_card_under_edge()))
                .child(div().absolute().top(px(0.0)).left(px(0.0)).child(card))
                .child(crate::ui::count_badge(&t, trn("{count} song", "{count} songs", count)).absolute().top(-t.s(10.0)).right(-t.s(8.0)))
                .into_any_element()
        } else {
            card.into_any_element()
        };
        // Below and right of the pointer like a system drag image, flipping sides near the window's
        // right or bottom edge.
        let (ox, oy) = (t.s(DRAG_OUT_OFFSET_X), t.s(DRAG_OUT_OFFSET_Y));
        let x = if pointer.x + ox + w > layout.viewport.width { pointer.x - ox - w } else { pointer.x + ox };
        let y = if pointer.y + oy + h > layout.viewport.height { pointer.y - oy - h } else { pointer.y + oy };
        let mut layers = vec![deferred(anchored().position(point(x, y)).child(card)).with_priority(6).into_any_element()];
        if let Some((slot_y, gap_h)) = slot {
            let view = self.queue_list.viewport_bounds();
            let inset = t.s(2.0);
            let slot = div().absolute().left(t.s(crate::view_queue::SIDE_PAD)).right(t.s(crate::view_queue::SIDE_PAD)).top(slot_y - view.top() + inset).h(gap_h - inset * 2.0).rounded(t.s(R_MD)).bg(t.drag_slot()).border_1().border_color(t.drag_slot_edge());
            let layer = div().relative().w(view.size.width).h(view.size.height).overflow_hidden().child(slot);
            layers.push(deferred(anchored().position(view.origin).child(layer)).with_priority(5).into_any_element());
        }
        Some(div().children(layers).into_any_element())
    }
}

impl Shell {
    /// Adds songs to a playlist, from the submenu or a drop. Asks first about songs already in it.
    pub fn add_songs_to_playlist(&mut self, id: SharedString, name: SharedString, songs: Vec<u32>, cx: &mut Context<Self>) {
        let Some(core) = self.core.as_ref() else {
            self.show_toast(tr("Playlists can't be changed without the server."), cx);
            return;
        };
        let dups = core.lib.already_in_playlist(&id, &songs);
        if dups.is_empty() {
            core.lib.add_to_playlist(id.to_string(), name.to_string(), songs.clone(), None);
            self.flash_rows(&songs);
            cx.notify();
            return;
        }
        let first = songs.first().and_then(|s| self.song_at(*s)).map(|s| s.title).unwrap_or_default();
        let fresh: Vec<u32> = songs.iter().copied().filter(|s| !dups.contains(s)).collect();
        self.confirm = Some(crate::view_connect::Confirm::Duplicates { id, name, songs, fresh, first });
        cx.notify();
    }

    /// The "Already in <playlist>" prompt. When every song is a duplicate it offers Cancel or Add
    /// Anyway, otherwise Add Anyway or Skip Duplicates.
    pub fn render_duplicates(&mut self, id: SharedString, name: SharedString, songs: Vec<u32>, fresh: Vec<u32>, first: SharedString, cx: &mut Context<Self>) -> AnyElement {
        use crate::ui_controls::{DialogButton, dialog_button};
        let t = self.theme;
        let total = songs.len();
        let text = if total == 1 {
            crate::i18n::trf("“{song}” is already in this playlist.", &[("song", &first)])
        } else if fresh.is_empty() {
            trnf("The song is already in {name}.", "All {count} songs are already in {name}.", total, &[("name", &name)])
        } else {
            trnf("{count} of {total} songs is already in {name}.", "{count} of {total} songs are already in {name}.", total - fresh.len(), &[("total", &crate::i18n::fmt_int(total as i64)), ("name", &name)])
        };
        let title = crate::i18n::trf("Already in {name}", &[("name", &name)]);
        let skip_toast = trnf("Added {count} new song to {name}", "Added {count} new songs to {name}", fresh.len(), &[("name", &name)]);
        // Adds these songs. `toast` replaces "Added to <playlist>" once done.
        let add = move |this: &mut Shell, songs: Vec<u32>, toast: Option<String>, cx: &mut Context<Shell>| {
            this.confirm = None;
            if let Some(core) = this.core.as_ref() {
                core.lib.add_to_playlist(id.to_string(), name.to_string(), songs.clone(), toast);
                this.flash_rows(&songs);
            }
            cx.notify();
        };
        let add_all = {
            let add = add.clone();
            move |this: &mut Shell, _: &gpui::ClickEvent, _: &mut gpui::Window, cx: &mut Context<Shell>| add(this, songs.clone(), None, cx)
        };
        let close = |this: &mut Shell, cx: &mut Context<Shell>| {
            this.confirm = None;
            cx.notify();
        };
        let (left, right) = if fresh.is_empty() {
            let cancel = dialog_button(&t, "duplicates-cancel", DialogButton::Ghost, true).child(tr("Cancel")).stop_top(&t).on_click(cx.listener(move |this, _, _, cx| close(this, cx)));
            let anyway = dialog_button(&t, "duplicates-add", DialogButton::Primary, true).child(tr("Add Anyway")).stop_top(&t).on_click(cx.listener(add_all));
            (cancel, anyway)
        } else {
            let anyway = dialog_button(&t, "duplicates-add", DialogButton::Ghost, true).child(tr("Add Anyway")).stop_top(&t).on_click(cx.listener(add_all));
            let skip = dialog_button(&t, "duplicates-skip", DialogButton::Primary, true).child(tr("Skip Duplicates")).stop_top(&t).on_click(cx.listener(move |this, _, _, cx| add(this, fresh.clone(), Some(skip_toast.clone()), cx)));
            (anyway, skip)
        };
        crate::ui::dialog_scrim(&t, "duplicates-scrim")
            .on_mouse_down(gpui::MouseButton::Left, cx.listener(move |this, _, _, cx| close(this, cx)))
            .child(
                crate::ui::dialog_panel(&t, "duplicates")
                    .child(crate::ui::dialog_title(&t, title))
                    .child(crate::ui::dialog_text(&t, text))
                    .child(crate::ui::dialog_actions(&t).child(left).child(right)),
            )
            .into_any_element()
    }
}
