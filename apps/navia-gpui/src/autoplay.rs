//! Autoplay in the queue. The rules live in `navia_core::autoplay`, and this keeps the queue
//! and player in step with them.
//!
//! The queue reads: the playing song, the listener's queued songs, the rest of the list, at
//! most one Autoplay song already handed to the player so the join stays gapless, then what
//! Autoplay offers. The view shows the last two together under "Autoplay".
//!
//! Autoplay plans once per context (`PlayQueue::context`), which only changes when the listener
//! starts something. Playing, skipping to or picking an Autoplay song just walks the plan. Until
//! the plan is final the section shows no songs, so rows never reorder under the listener.

use gpui::Context;
use navia_core::autoplay::{self, AutoplayState};

use crate::core_player::CoreEvent;
use crate::persist::DockView;
use crate::shell::Shell;

/// how many of the context's last songs anchor Autoplay.
const ANCHOR_TAIL: usize = 12;

impl Shell {
    /// Songs after the current one that the listener queued, not Autoplay's.
    pub fn up_next_len(&self) -> usize {
        self.player.queue.len().saturating_sub(self.player.queue_pos + 1).saturating_sub(self.player.auto_tail)
    }

    /// The listener's own songs still to come. Up Next plus the part of the list, or of the
    /// shuffled order, not yet in the queue.
    pub fn own_songs_left(&self) -> usize {
        let unfed = self.pq().map_or(0, |q| q.unfed());
        self.up_next_len() + unfed
    }

    /// Whether a song from Search is waiting for its radio, which comes before Autoplay.
    pub fn radio_waiting(&self) -> bool {
        self.radio_pending == Some(self.plays)
    }

    /// The list's songs still to come after the playing one, excluding the listener's queued
    /// songs. Autoplay shows only at the end of the list.
    pub fn list_songs_left(&self) -> usize {
        self.own_songs_left().saturating_sub(self.player.queued_run.min(self.up_next_len()))
    }

    pub fn autoplay_state(&self) -> AutoplayState {
        let has = !self.autoplay_rows.is_empty() || self.player.auto_tail > 0;
        // The sample library has no real player, so there's nothing to wait for.
        let ready = self.autoplay_ready || self.core.is_none();
        autoplay::state(self.prefs.autoplay, self.player.repeat, self.player.station.is_some(), has, self.list_songs_left(), ready)
    }

    /// The Autoplay section's rows. Songs already handed to the player, then the core's offer.
    /// While the plan is being found, only the former.
    pub fn autoplay_section(&self) -> Vec<u32> {
        let state = self.autoplay_state();
        if !matches!(state, AutoplayState::On | AutoplayState::Finding) {
            return Vec::new();
        }
        let len = self.player.queue.len();
        let mut rows: Vec<u32> = self.player.queue[len - self.player.auto_tail.min(len)..].to_vec();
        if state == AutoplayState::On {
            rows.extend(self.autoplay_rows.iter());
        }
        rows
    }

    /// What Autoplay's songs are based on, for its heading. The album, playlist, artist or radio
    /// the listener started, else the song the context is anchored on.
    pub fn autoplay_based_on(&mut self) -> Option<String> {
        use navia_core::queue_order::SourceLink;
        match self.player.source.clone().filter(|s| !matches!(s.link, Some(SourceLink::Songs) | None) || s.radio) {
            Some(source) if source.radio => Some(crate::i18n::trf("{name} Radio", &[("name", &source.name)])),
            Some(source) => Some(source.name),
            None => {
                let row = self.pq().and_then(|q| q.anchor_tail(1).pop())?;
                Some(self.song_by_rowid(row).title.to_string()).filter(|t| !t.is_empty())
            }
        }
    }

    fn queue_view_showing(&self) -> bool {
        self.prefs.view == DockView::Queue
    }

    /// Saves the account's Autoplay mode. The offer updates at once.
    pub fn set_autoplay_mode(&mut self, mode: navia_core::engine::AutoplayMode, cx: &mut Context<Self>) {
        self.autoplay_mode = mode;
        if let Some(core) = self.core.as_ref()
            && let Err(e) = core.lib.set_autoplay_mode(mode)
        {
            eprintln!("navia: couldn't save the Autoplay mode: {e}");
        }
        self.autoplay_update(None, cx);
        cx.notify();
    }

    /// Recomputes Autoplay's offer off the UI thread. `heard` is a song that just started. The
    /// server is asked only when the seeds changed and the songs may soon be needed, meaning the
    /// queue is showing or two or fewer of the listener's songs are left.
    pub fn autoplay_update(&mut self, heard: Option<u32>, cx: &mut Context<Self>) {
        let Some(core) = self.core.as_ref() else { return };
        self.autoplay_generation += 1;
        let generation = self.autoplay_generation;
        // The context and its anchor songs come from the end of the list, never from Autoplay, so an
        // Autoplay song that plays doesn't start a new context.
        let context = core.pq.context();
        let chosen: Vec<u32> = core.pq.anchor_tail(ANCHOR_TAIL);
        // A new context, so nothing from the last offer may show or be handed over until the new
        // one is ready.
        if context != self.autoplay_context {
            self.autoplay_context = context;
            self.autoplay_rows.clear();
            self.autoplay_ready = false;
        }
        let queue = self.player.queue.clone();
        let may_fetch = self.prefs.autoplay && self.player.station.is_none() && !self.radio_waiting() && (self.queue_view_showing() || self.own_songs_left() <= 2);
        let (lib, tx, state) = (core.lib.clone(), core.events.clone(), self.autoplay.clone());
        cx.background_executor()
            .spawn(async move {
                if let Some(row) = heard
                    && let Some((_, taken)) = lib.autoplay_taken(&[row]).into_iter().next()
                {
                    state.lock().unwrap().heard_taken(taken);
                }
                let found = lib.autoplay_taken(&chosen);
                let ids: Vec<&str> = chosen.iter().filter_map(|r| found.iter().find(|(f, _)| f == r).map(|(_, t)| t.id.as_str())).collect();
                let (fetch, wants, mode) = {
                    let mut s = state.lock().unwrap();
                    // the Navia mode's setting, play log and song details.
                    s.attach(lib.db.clone());
                    s.set_context(context, ids.into_iter());
                    let wanted = s.wanted_fetch();
                    if let Some(seeds) = wanted.as_ref().filter(|_| may_fetch) {
                        s.begin_fetch(seeds);
                    }
                    (wanted.clone().filter(|_| may_fetch), wanted.is_some() && !may_fetch, s.mode())
                };
                if let Some(seeds) = fetch {
                    let answers = lib.autoplay_answers(&seeds, mode);
                    state.lock().unwrap().finish_answers(seeds, answers);
                    let _ = tx.unbounded_send(CoreEvent::Autoplay { generation, rows: Vec::new(), ready: false, fetched: true, wants_fetch: false });
                    return;
                }
                let taken: Vec<_> = lib.autoplay_taken(&queue).into_iter().map(|(_, t)| t).collect();
                let (songs, ready) = {
                    let mut s = state.lock().unwrap();
                    (s.songs(&taken, autoplay::SHOWN), s.ready())
                };
                let ids: Vec<String> = songs.into_iter().map(|s| s.id).collect();
                let rows = lib.rowids_in_order(&ids);
                let _ = tx.unbounded_send(CoreEvent::Autoplay { generation, rows, ready, fetched: false, wants_fetch: wants });
            })
            .detach();
    }

    /// An answer from [`Shell::autoplay_update`].
    pub fn on_autoplay(&mut self, generation: u64, rows: Vec<u32>, ready: bool, fetched: bool, wants_fetch: bool, cx: &mut Context<Self>) {
        if fetched {
            // New server answers, so recompute the offer.
            self.autoplay_update(None, cx);
            return;
        }
        if generation != self.autoplay_generation {
            return;
        }
        self.autoplay_wants_fetch = wants_fetch;
        self.autoplay_rows = rows;
        self.autoplay_ready = ready;
        self.autoplay_maybe_start(cx);
        cx.notify();
    }

    /// asks about seeds it hasn't yet.
    pub fn autoplay_view_shown(&mut self, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.autoplay_wants_fetch) {
            self.autoplay_update(None, cx);
        }
    }

    /// Hands the player Autoplay's first song once the listener's songs run out, so it joins
    /// without a gap. If the queue already ended, it starts playing.
    pub fn autoplay_maybe_start(&mut self, cx: &mut Context<Self>) {
        let left = self.own_songs_left() + self.player.auto_tail;
        if self.player.queue.is_empty() || self.radio_waiting() || !autoplay::starts_now(self.prefs.autoplay, self.player.repeat, self.player.station.is_some(), left) {
            return;
        }
        let Some(row) = self.autoplay_rows.first().copied() else { return };
        let ended = self.player.status == navia_core::audio::player::PlayerStatus::Stopped && self.player.queue_pos + 1 == self.player.queue.len();
        // Before the plan is final its songs are a stand-in from the anchor artist. One plays if the
        // context already ended, but none is lined up ahead of the plan.
        if !self.autoplay_ready && !ended {
            return;
        }
        let Some(core) = self.core.as_mut() else { return };
        self.autoplay_rows.remove(0);
        let mut edits = core.pq.hand_over(row);
        // Fetch its BPM from Deezer when it has no tag, for the flow step later.
        let lib = core.lib.clone();
        cx.background_executor().spawn(async move { lib.autoplay_bpm(row) }).detach();
        if ended {
            edits.push(navia_core::play_queue::Edit::JumpTo(self.player.queue.len()));
        }
        self.queue_edits(edits);
        self.core_save_session();
        cx.notify();
    }

    /// Plays an Autoplay song right after the current one, leaving Next in Queue alone. Autoplay
    /// continues from it, so the songs above it leave the plan. Once played it stays behind for
    /// Previous.
    pub fn autoplay_play(&mut self, row: u32, cx: &mut Context<Self>) {
        if self.core.is_none() {
            return;
        }
        // This ends a station or listen-along, and the queue is restored.
        let held = self.player.station.is_some() || self.listen.following.is_some();
        self.player.station = None;
        self.drop_along(cx);
        let len = self.player.queue.len();
        let mut above: Vec<u32> = self.player.queue[len - self.player.auto_tail.min(len)..].to_vec();
        above.extend(self.autoplay_rows.iter().take_while(|r| **r != row));
        let core = self.core.as_mut().expect("core");
        let mut edits = core.pq.drop_other_tail();
        edits.extend(core.pq.play_other(row));
        if held {
            let at = core.pq.pos() + 1;
            edits = core.pq.restart_at(at);
        }
        self.queue_edits(edits);
        self.autoplay_rows.retain(|r| *r != row && !above.contains(r));
        self.autoplay_forget(above, cx);
        self.selection = Default::default();
        cx.notify();
    }

    /// Removes songs from Autoplay's plan for this session, then updates the offer.
    fn autoplay_forget(&mut self, rows: Vec<u32>, cx: &mut Context<Self>) {
        if rows.is_empty() {
            return;
        }
        let Some(core) = self.core.as_ref() else { return };
        let (lib, state) = (core.lib.clone(), self.autoplay.clone());
        let done = cx.background_executor().spawn(async move {
            for (_, taken) in lib.autoplay_taken(&rows) {
                state.lock().unwrap().remove_taken(taken);
            }
        });
        cx.spawn(async move |this, cx| {
            done.await;
            let _ = this.update(cx, |this, cx| this.autoplay_update(None, cx));
        })
        .detach();
    }

    /// Delete or Backspace in the queue removes the selected rows. Returns false when nothing is selected.
    pub fn queue_remove_selected(&mut self, cx: &mut Context<Self>) -> bool {
        let (list, _) = self.queue_view_list();
        let positions: Vec<usize> = (0..list.len()).filter(|p| self.selection.contains(list[*p])).collect();
        if positions.is_empty() || self.player.station.is_some() {
            return false;
        }
        self.queue_remove(&positions, cx);
        true
    }

    /// Removes queue view rows by position in [`Shell::queue_view_list`]. Up Next songs leave the
    /// queue and Autoplay's leave its offer. The playing song is never among them.
    pub fn queue_remove(&mut self, positions: &[usize], cx: &mut Context<Self>) {
        if self.player.station.is_some() || self.player.queue.is_empty() {
            return;
        }
        let up_next = self.up_next_len();
        let up: Vec<usize> = positions.iter().copied().filter(|p| *p < up_next).collect();
        let offered: Vec<usize> = positions.iter().filter(|p| **p >= up_next).map(|p| p - up_next).collect();
        if !up.is_empty() {
            self.remove_upcoming(&up, cx);
        }
        if !offered.is_empty() {
            self.autoplay_remove(&offered, cx);
        }
    }

    /// Removes Autoplay songs by position in [`Shell::autoplay_section`].
    pub fn autoplay_remove(&mut self, positions: &[usize], cx: &mut Context<Self>) {
        let section = self.autoplay_section();
        let rows: Vec<u32> = positions.iter().filter_map(|p| section.get(*p).copied()).collect();
        if rows.is_empty() {
            return;
        }
        // Songs already handed to the player leave its queue too.
        let tail = self.player.auto_tail;
        let up_next = self.up_next_len();
        let gone: Vec<usize> = positions.iter().filter(|p| **p < tail).map(|p| up_next + p).collect();
        if let Some(core) = self.core.as_mut().filter(|_| !gone.is_empty()) {
            let edits = core.pq.remove(&gone);
            self.queue_edits(edits);
        }
        self.autoplay_rows.retain(|r| !rows.contains(r));
        self.autoplay_forget(rows, cx);
        self.selection = Default::default();
        self.core_save_session();
        cx.notify();
    }

    /// Moves dragged queue rows before Up Next position `before`, into Next in Queue when
    /// `into_queued` or else Next from. Moved songs join the section they land in, and Autoplay
    /// songs become the listener's own.
    pub fn queue_drop(&mut self, positions: &[usize], before: usize, into_queued: bool, cx: &mut Context<Self>) {
        if self.core.is_none() {
            return;
        }
        let up_next = self.up_next_len();
        let tail = self.player.auto_tail;
        let section = self.autoplay_section();
        // Offered songs join the end of the queue first so every moved song is in the queue. Then one
        // move puts them all in place.
        let offered: Vec<u32> = positions.iter().filter(|p| **p >= up_next + tail).filter_map(|p| section.get(p - up_next)).copied().collect();
        let mut edits = Vec::new();
        let core = self.core.as_mut().expect("core");
        for row in &offered {
            edits.extend(core.pq.hand_over(*row));
        }
        self.autoplay_rows.retain(|r| !offered.contains(r));
        let mut next_offered = up_next + tail;
        let moved: Vec<usize> = positions
            .iter()
            .map(|p| {
                if *p < up_next + tail {
                    *p
                } else {
                    next_offered += 1;
                    next_offered - 1
                }
            })
            .collect();
        edits.extend(core.pq.move_entries(&moved, before, into_queued));
        self.queue_edits(edits);
        self.selection = Default::default();
        self.autoplay_update(None, cx);
        self.core_save_session();
        cx.notify();
    }

}
