//! Keeps the queue model and the audio player in step. Every queue change goes through the
//! model, which returns edits for the player. A worker thread applies them in order, and
//! `Player`'s queue fields mirror the model for the views.

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Instant;

use gpui::Context;
use navia_core::audio::player::{Player as CorePlayer, QueueItem};
use navia_core::play_queue::{Edit, PlayQueue};
use navia_core::queue_order::{self, ShuffleStyle};

use crate::core_library::CoreLibrary;
use crate::shell::Shell;

/// in the order it was made.
pub(crate) enum EditJob {
    /// Edits. A `SetQueue` among them starts at `start_ms`, paused or not.
    Edits { edits: Vec<Edit<u32>>, start_ms: u64, paused: bool },
    /// `Player::ack_left`, sent after the edits made before the model knew the song left.
    Ack(u64),
}

/// it ends when the sender is dropped.
pub(crate) fn spawn_edit_worker(lib: Arc<CoreLibrary>, player: Arc<CorePlayer>) -> mpsc::Sender<EditJob> {
    let (tx, rx) = mpsc::channel::<EditJob>();
    std::thread::Builder::new()
        .name("navia-queue-edits".into())
        .spawn(move || {
            for job in rx {
                match job {
                    EditJob::Edits { edits, start_ms, paused } => {
                        for edit in edits {
                            apply(&lib, &player, edit, start_ms, paused);
                        }
                    }
                    EditJob::Ack(seq) => player.ack_left(seq),
                }
            }
        })
        .expect("spawn queue edit worker");
    tx
}

fn apply(lib: &CoreLibrary, player: &CorePlayer, edit: Edit<u32>, start_ms: u64, paused: bool) {
    match edit {
        Edit::SetQueue { items, start } => player.set_queue(player_items(lib, &items), start, start_ms, paused),
        Edit::InsertAt { index, items } => player.insert_at(index, player_items(lib, &items)),
        Edit::Remove(positions) => player.remove_items(positions),
        Edit::ReplaceUpcoming(items) => player.replace_upcoming(player_items(lib, &items)),
        Edit::Move { positions, before } => player.move_items(positions, before),
        Edit::SetQueued { positions, queued } => player.set_queued(positions, queued),
        Edit::ShuffleQueued(seed) => player.shuffle_queued(seed),
        Edit::JumpTo(index) => player.jump_to(index),
    }
}

/// Player items for library rows, one per row so indexes match the model's. A row the library
/// no longer has becomes an item the player skips like any failed song.
fn player_items(lib: &CoreLibrary, rows: &[(u32, bool)]) -> Vec<QueueItem> {
    let ids: Vec<u32> = rows.iter().map(|(r, _)| *r).collect();
    let found: std::collections::HashMap<u32, (String, Option<u64>)> = lib.queue_ids(&ids).into_iter().map(|(r, id, d)| (r, (id, d))).collect();
    rows.iter()
        .map(|(r, queued)| {
            let (id, duration) = found.get(r).cloned().unwrap_or_default();
            QueueItem::new(id, duration).queued(*queued)
        })
        .collect()
}

impl Shell {
    /// when there's a real player.
    pub(crate) fn pq(&self) -> Option<&PlayQueue<u32>> {
        self.core.as_ref().map(|c| &c.pq)
    }

    /// Sends edits the model made to the player and mirrors the model in `Player`.
    pub(crate) fn queue_edits(&mut self, edits: Vec<Edit<u32>>) {
        self.queue_edits_at(edits, 0, false);
    }

    /// Like `queue_edits`, with a `SetQueue` starting at `start_ms`. While a station or listen-along
    /// plays, nothing is sent, since the queue is restored whole when it ends.
    pub(crate) fn queue_edits_at(&mut self, edits: Vec<Edit<u32>>, start_ms: u64, paused: bool) {
        let holding = self.player.station.is_none() && self.listen.following.is_none();
        if let Some(core) = self.core.as_ref()
            && holding
            && !edits.is_empty()
        {
            let _ = core.edits.send(EditJob::Edits { edits, start_ms, paused });
        }
        self.mirror_queue();
    }

    pub(crate) fn mirror_queue(&mut self) {
        let Some(pq) = self.pq() else { return };
        let (queue, pos, run, current_queued, tail, shuffle) = (pq.songs(), pq.pos(), pq.queued_run(), pq.current_queued(), pq.other_tail(), pq.shuffle());
        self.player.queue = queue;
        self.player.queue_pos = pos;
        self.player.queued_run = run;
        self.player.current_queued = current_queued;
        self.player.auto_tail = tail;
        self.player.shuffle = shuffle;
    }

    /// Computes the pending shuffle off the UI thread, since reading recency is slow on big
    /// queues, then applies it. Retries if the queue changed meanwhile.
    pub(crate) fn queue_mix(&mut self, cx: &mut Context<Self>) {
        let Some(core) = self.core.as_ref() else { return };
        let Some(request) = core.pq.mix_request() else { return };
        let style = self.prefs.shuffle_style;
        let heard = self.autoplay.lock().unwrap().heard_ids().clone();
        let lib = core.lib.clone();
        let started = Instant::now();
        cx.spawn(async move |this, cx| {
            let (request, order) = cx
                .background_executor()
                .spawn(async move {
                    let recency = if style == ShuffleStyle::FewerRepeats { lib.recency(&request.songs, &heard) } else { vec![0.0; request.songs.len()] };
                    let seed = queue_order::random_seed();
                    let order = match request.current {
                        Some(c) => queue_order::shuffled_whole_order(&recency, c, 0, style, seed),
                        None => queue_order::mixed_order(&recency, style, seed),
                    };
                    (request, order)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let applying = Instant::now();
                let Some(core) = this.core.as_mut() else { return };
                match core.pq.apply_mix(&request, &order) {
                    Some(edits) => {
                        this.queue_edits(edits);
                        this.autoplay_update(None, cx);
                        this.core_save_session();
                        cx.notify();
                        // Scripted runs report the timing.
                        if std::env::var_os("NAVIA_SCRIPT").is_some() {
                            eprintln!("navia: shuffle mixed in {:.1} ms, applied on the UI thread in {:.1} ms", started.elapsed().as_secs_f64() * 1000.0, applying.elapsed().as_secs_f64() * 1000.0);
                        }
                    }
                    None => this.queue_mix(cx),
                }
            });
        })
        .detach();
    }
}
