//! The play queue model (`navia_core::play_queue::PlayQueue`) for the Mac, kept in step with its player. Every change
//! goes through the model, whose edits reach the player in order under one lock, so the app, player and saved session
//! can't drift apart. Durations come from the items the app passes in, or else the library.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use navia_core::audio::player::QueueItem as CoreQueueItem;
use navia_core::play_queue::{Edit, Kind, ListUndo, PlayQueue};
use navia_core::queue_order;

use crate::{NaviaAutoplay, NaviaCore, NaviaPlayer, QueueItem, ShuffleStyle};

/// Durations are cached for songs the queue has seen, and cleared past this many.
const DURATIONS_MAX: usize = 100_000;
/// A mix is recomputed when the queue changed meanwhile, up to this many times.
const MIX_TRIES: usize = 5;

#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueKind {
    /// A song from the list the queue plays from.
    List,
    /// Queued by the listener in Next in Queue.
    Queued,
    /// From outside the list, like Autoplay's songs, a followed song, or one played on its own.
    Other,
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct QueueEntryRow {
    /// Unique within the queue.
    pub id: u64,
    pub song_id: String,
    pub kind: QueueKind,
}

/// The queue as the player holds it, for views.
#[derive(uniffi::Record, Clone, Debug)]
pub struct QueueView {
    pub entries: Vec<QueueEntryRow>,
    pub pos: u64,
    /// Next in Queue: queued entries right after the playing one.
    pub queued_run: u64,
    pub current_queued: bool,
    /// Autoplay's songs at the end, already handed to the player.
    pub other_tail: u64,
    /// List songs not yet in the player.
    pub unfed: u64,
    /// The list's songs still to come after the playing one, whether in the player or not.
    pub list_left: u64,
    pub shuffle: bool,
    pub mixing: bool,
    pub revision: u64,
}

struct State {
    pq: PlayQueue<String>,
    /// A station or a followed song holds the player. Edits wait, and the whole queue is restored when it ends.
    holding: bool,
    durations: HashMap<String, u64>,
    undo: Option<ListUndo<String>>,
    saved_revision: Option<u64>,
}

#[derive(uniffi::Object)]
pub struct NaviaQueue {
    core: Arc<NaviaCore>,
    player: Arc<NaviaPlayer>,
    autoplay: Option<Arc<NaviaAutoplay>>,
    state: Mutex<State>,
}

#[uniffi::export]
impl NaviaCore {
    /// The queue model for `player`. `autoplay` gives Fewer Repeats this session's plays.
    pub fn make_queue(self: Arc<Self>, player: Arc<NaviaPlayer>, autoplay: Option<Arc<NaviaAutoplay>>) -> Arc<NaviaQueue> {
        Arc::new(NaviaQueue { core: self, player, autoplay, state: Mutex::new(State { pq: PlayQueue::new(), holding: false, durations: HashMap::new(), undo: None, saved_revision: None }) })
    }
}

impl NaviaQueue {
    fn note(st: &mut State, items: &[QueueItem]) -> Vec<String> {
        if st.durations.len() > DURATIONS_MAX {
            st.durations.clear();
        }
        items
            .iter()
            .map(|i| {
                if let Some(d) = i.duration_ms.filter(|d| *d > 0) {
                    st.durations.insert(i.id.clone(), d);
                }
                i.id.clone()
            })
            .collect()
    }

    /// Player items, one per song so the player's indexes match the model's.
    fn items(&self, st: &mut State, songs: &[(String, bool)]) -> Vec<CoreQueueItem> {
        let missing: Vec<String> = songs.iter().filter(|(id, _)| !st.durations.contains_key(id)).map(|(id, _)| id.clone()).collect();
        if !missing.is_empty()
            && let Ok(found) = self.core.db.durations_by_id(&missing)
        {
            st.durations.extend(found);
        }
        songs.iter().map(|(id, queued)| CoreQueueItem::new(id.clone(), st.durations.get(id).copied()).queued(*queued)).collect()
    }

    /// Sends the model's edits to the player in order. A `SetQueue` among them starts at `start_ms`, paused or not.
    /// Nothing is sent while something else holds the player.
    fn apply(&self, st: &mut State, edits: Vec<Edit<String>>, start_ms: u64, paused: bool) {
        if st.holding {
            return;
        }
        let player = &self.player.inner;
        for edit in edits {
            match edit {
                Edit::SetQueue { items, start } => {
                    let items = self.items(st, &items);
                    player.set_queue(items, start, start_ms, paused);
                }
                Edit::InsertAt { index, items } => {
                    let items = self.items(st, &items);
                    player.insert_at(index, items);
                }
                Edit::Remove(positions) => player.remove_items(positions),
                Edit::ReplaceUpcoming(items) => {
                    let items = self.items(st, &items);
                    player.replace_upcoming(items);
                }
                Edit::Move { positions, before } => player.move_items(positions, before),
                Edit::SetQueued { positions, queued } => player.set_queued(positions, queued),
                Edit::ShuffleQueued(seed) => player.shuffle_queued(seed),
                Edit::JumpTo(index) => player.jump_to(index),
            }
        }
    }

    fn edit(&self, f: impl FnOnce(&mut PlayQueue<String>) -> Vec<Edit<String>>) {
        let mut st = self.state.lock().unwrap();
        let edits = f(&mut st.pq);
        self.apply(&mut st, edits, 0, false);
    }
}

#[uniffi::export]
impl NaviaQueue {
    pub fn view(&self) -> QueueView {
        let st = self.state.lock().unwrap();
        let q = &st.pq;
        let entries = q
            .entries()
            .iter()
            .map(|e| QueueEntryRow {
                id: e.id,
                song_id: e.song.clone(),
                kind: match e.kind {
                    Kind::List => QueueKind::List,
                    Kind::Queued => QueueKind::Queued,
                    Kind::Other => QueueKind::Other,
                },
            })
            .collect();
        QueueView { entries, pos: q.pos() as u64, queued_run: q.queued_run() as u64, current_queued: q.current_queued(), other_tail: q.other_tail() as u64, unfed: q.unfed() as u64, list_left: q.list_left() as u64, shuffle: q.shuffle(), mixing: q.mixing(), revision: q.revision() }
    }

    /// While a station plays or someone is followed, edits wait for the queue to come back.
    pub fn set_holding(&self, on: bool) {
        self.state.lock().unwrap().holding = on;
    }

    /// Starts a new list at `index`. Next in Queue stays next. When shuffled, the chosen song plays at once and `mix`
    /// orders the rest.
    pub fn start(&self, songs: Vec<QueueItem>, index: u64, shuffled: bool) {
        let mut st = self.state.lock().unwrap();
        let ids = Self::note(&mut st, &songs);
        let edits = st.pq.start(ids, index as usize, shuffled);
        self.apply(&mut st, edits, 0, false);
    }

    /// `start` for a list of library rows. Returns false when the chosen row isn't in the library.
    pub fn start_rowids(&self, rowids: Vec<u32>, index: u64, shuffled: bool) -> bool {
        let Some(&chosen) = rowids.get(index as usize) else { return false };
        let Ok(ids) = self.core.db.song_ids_by_rowid(&rowids) else { return false };
        // Rows the library no longer has are left out, and the chosen one keeps its place.
        let at = if ids.len() == rowids.len() {
            index as usize
        } else {
            let Ok(chosen_id) = self.core.db.song_ids_by_rowid(&[chosen]) else { return false };
            let Some(chosen_id) = chosen_id.into_iter().next() else { return false };
            let before = self.core.db.song_ids_by_rowid(&rowids[..index as usize]).map_or(0, |b| b.len());
            if ids.get(before) != Some(&chosen_id) {
                return false;
            }
            before
        };
        let mut st = self.state.lock().unwrap();
        let edits = st.pq.start(ids, at, shuffled);
        self.apply(&mut st, edits, 0, false);
        true
    }

    /// The player moved to `index`, and a queued song that played may have left at `left_index`. The rest of the list
    /// feeds in when it runs low.
    pub fn track_changed(&self, index: u64, left_index: Option<u64>, left_seq: Option<u64>) {
        let mut st = self.state.lock().unwrap();
        let edits = st.pq.track_changed(index as usize, left_index.map(|l| l as usize));
        if let Some(seq) = left_seq {
            self.player.inner.ack_left(seq);
        }
        self.apply(&mut st, edits, 0, false);
    }

    /// Turns shuffle on or off. Off applies at once, and on waits for `mix`.
    pub fn set_shuffle(&self, on: bool) {
        self.edit(|q| q.set_shuffle(on));
    }

    /// Computes and applies the pending shuffle. It reads every song's recency, so call it off the main thread. Returns
    /// true when a mix was applied.
    pub fn mix(&self, style: ShuffleStyle) -> bool {
        let style = match style {
            ShuffleStyle::FewerRepeats => queue_order::ShuffleStyle::FewerRepeats,
            ShuffleStyle::Standard => queue_order::ShuffleStyle::Standard,
        };
        for _ in 0..MIX_TRIES {
            let Some(request) = self.state.lock().unwrap().pq.mix_request() else { return false };
            let recency = if style == queue_order::ShuffleStyle::FewerRepeats {
                let heard = self.autoplay.as_ref().map(|a| a.state.lock().unwrap().heard_ids().clone()).unwrap_or_default();
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
                self.core.db.recency_by_id(&request.songs, &heard, now).unwrap_or_else(|_| vec![0.0; request.songs.len()])
            } else {
                vec![0.0; request.songs.len()]
            };
            let seed = queue_order::random_seed();
            let order = match request.current {
                Some(c) => queue_order::shuffled_whole_order(&recency, c, 0, style, seed),
                None => queue_order::mixed_order(&recency, style, seed),
            };
            let mut st = self.state.lock().unwrap();
            if let Some(edits) = st.pq.apply_mix(&request, &order) {
                self.apply(&mut st, edits, 0, false);
                return true;
            }
        }
        false
    }

    /// Inserts songs at `offset` in Up Next, into Next in Queue (`queued`) or Next from. Returns the queue index they
    /// went to. Into an empty queue they become the queue, paused.
    pub fn insert(&self, songs: Vec<QueueItem>, offset: u64, queued: bool) -> u64 {
        let mut st = self.state.lock().unwrap();
        let empty = st.pq.is_empty();
        let ids = Self::note(&mut st, &songs);
        let (edits, at) = st.pq.insert(ids, offset as usize, queued);
        self.apply(&mut st, edits, 0, empty);
        at as u64
    }

    /// Appends songs to the end of the list, like a Search song's radio. With `play`, the queue had ended, so the first
    /// one plays.
    pub fn append_to_list(&self, songs: Vec<QueueItem>, play: bool) {
        let mut st = self.state.lock().unwrap();
        let ids = Self::note(&mut st, &songs);
        let (mut edits, at) = st.pq.append_to_list(ids);
        if play && !edits.is_empty() {
            edits.push(Edit::JumpTo(at));
        }
        self.apply(&mut st, edits, 0, false);
    }

    /// Removes Up Next entries by offset after the playing one. Autoplay's entries at the end count.
    pub fn remove(&self, offsets: Vec<u64>) {
        let offsets: Vec<usize> = offsets.into_iter().map(|o| o as usize).collect();
        self.edit(|q| q.remove(&offsets));
    }

    /// Drops Autoplay's songs at the end, when Repeat comes on or Autoplay is off.
    pub fn drop_other_tail(&self) {
        self.edit(|q| q.drop_other_tail());
    }

    /// Clear Queue. Removes Next in Queue's songs and returns their ids for Undo, which inserts them at 0 as queued.
    pub fn clear_queued(&self) -> Vec<String> {
        let mut st = self.state.lock().unwrap();
        let (edits, songs) = st.pq.clear_queued();
        self.apply(&mut st, edits, 0, false);
        songs
    }

    /// Shuffle Queue. Mixes Next in Queue once.
    pub fn shuffle_queued(&self) {
        self.edit(|q| q.shuffle_queued(queue_order::random_seed()));
    }

    /// Clear <source>. Removes the rest of the list after Next in Queue. Returns false when there's none.
    /// `undo_clear_list` restores it.
    pub fn clear_list(&self) -> bool {
        let mut st = self.state.lock().unwrap();
        let Some((edits, undo)) = st.pq.clear_list() else { return false };
        st.undo = Some(undo);
        self.apply(&mut st, edits, 0, false);
        true
    }

    pub fn undo_clear_list(&self) {
        let mut st = self.state.lock().unwrap();
        let Some(undo) = st.undo.take() else { return };
        let edits = st.pq.undo_clear_list(undo);
        self.apply(&mut st, edits, 0, false);
    }

    /// A drag within the queue's sections (`PlayQueue::move_entries`).
    pub fn move_entries(&self, offsets: Vec<u64>, before: u64, into_queued: bool) {
        let offsets: Vec<usize> = offsets.into_iter().map(|o| o as usize).collect();
        self.edit(|q| q.move_entries(&offsets, before as usize, into_queued));
    }

    /// A double-click on the queue entry at `index`, following the jump rules.
    pub fn jump(&self, index: u64) {
        self.edit(|q| q.jump(index as usize));
    }

    /// Adds Autoplay's next song at the end. With `play`, the queue had ended, so it plays now. Only accepts songs for
    /// the current context, so a pick made before the listener started something else is dropped.
    pub fn hand_over(&self, song: QueueItem, play: bool) {
        if self.autoplay.as_ref().is_some_and(|a| !a.state.lock().unwrap().offers(&song.id)) {
            return;
        }
        let mut st = self.state.lock().unwrap();
        let id = Self::note(&mut st, std::slice::from_ref(&song)).remove(0);
        let at = st.pq.entries().len();
        let mut edits = st.pq.hand_over(id);
        if play {
            edits.push(Edit::JumpTo(at));
        }
        self.apply(&mut st, edits, 0, false);
    }

    /// Plays a song on its own right after the playing one, like one Autoplay offers.
    pub fn play_other(&self, song: QueueItem) {
        let mut st = self.state.lock().unwrap();
        let id = Self::note(&mut st, std::slice::from_ref(&song)).remove(0);
        let edits = st.pq.play_other(id);
        self.apply(&mut st, edits, 0, false);
    }

    /// Restores the queue from entry `index` after a station or following held the player. Ends holding.
    pub fn restart_at(&self, index: u64) {
        let mut st = self.state.lock().unwrap();
        st.holding = false;
        let edits = st.pq.restart_at(index as usize);
        self.apply(&mut st, edits, 0, false);
    }

    /// While following, the listener's own next song played through the followed person, so the queue moves on.
    pub fn step_waiting(&self) {
        self.state.lock().unwrap().pq.step_waiting();
    }

    /// while following, the first song of Next in Queue leaves the waiting queue to play through
    /// the followed person (a listening session's turn).
    pub fn take_queued_next(&self) -> Option<String> {
        self.state.lock().unwrap().pq.take_queued_next()
    }

    /// Following ended with the followed song kept: it becomes the playing entry, and the queue
    /// goes back into the player around it. Ends holding.
    pub fn keep_in_front(&self, song: QueueItem) {
        let mut st = self.state.lock().unwrap();
        st.holding = false;
        let id = Self::note(&mut st, std::slice::from_ref(&song)).remove(0);
        let edits = st.pq.keep_in_front(id);
        self.apply(&mut st, edits, 0, false);
    }

    /// Restores the whole queue to the player at its place, for a restored session or the end of a station or
    /// following. Ends holding.
    pub fn load(&self, start_ms: u64, paused: bool) {
        let mut st = self.state.lock().unwrap();
        st.holding = false;
        let edits = st.pq.load_edits();
        self.apply(&mut st, edits, start_ms, paused);
    }

    /// Replaces the queue from a flat list, like an older session or another device's queue. Songs at
    /// `queued_start..queued_end` are Next in Queue. Not sent to the player until `load`.
    pub fn replace_plain(&self, songs: Vec<QueueItem>, pos: u64, queued_start: u64, queued_end: u64, shuffle: bool) {
        let mut st = self.state.lock().unwrap();
        let ids = Self::note(&mut st, &songs);
        st.pq = PlayQueue::from_queue(ids, pos as usize, queued_start as usize..queued_end as usize, shuffle);
        st.undo = None;
    }

    /// The context the listener started (`PlayQueue::context`). It changes when a list starts or its play order is
    /// rebuilt, never when a song merely plays.
    pub fn context(&self) -> u64 {
        self.state.lock().unwrap().pq.context()
    }

    /// The songs Autoplay's context is anchored on, oldest first (`PlayQueue::anchor_tail`). These are the end of the
    /// list in play order, never songs Autoplay added.
    pub fn anchor_songs(&self, count: u64) -> Vec<String> {
        self.state.lock().unwrap().pq.anchor_tail(count as usize)
    }

    /// Writes the model to `path` if it changed since the last save. Returns true when written.
    pub fn save(&self, path: String) -> bool {
        let json = {
            let mut st = self.state.lock().unwrap();
            if st.pq.is_empty() || st.saved_revision == Some(st.pq.revision()) {
                return false;
            }
            st.saved_revision = Some(st.pq.revision());
            match serde_json::to_vec(&st.pq) {
                Ok(json) => json,
                Err(_) => return false,
            }
        };
        let path = std::path::PathBuf::from(path);
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json).and_then(|_| std::fs::rename(&tmp, &path)).is_ok()
    }

    /// Reads a model saved with `save` if its playing song is `current_id`. Not sent to the player until `load`.
    pub fn restore(&self, path: String, current_id: String) -> bool {
        let Ok(data) = std::fs::read(&path) else { return false };
        let Ok(pq) = serde_json::from_slice::<PlayQueue<String>>(&data) else { return false };
        if pq.current().is_none_or(|e| e.song != current_id) {
            return false;
        }
        let mut st = self.state.lock().unwrap();
        st.pq = pq;
        st.undo = None;
        st.saved_revision = Some(st.pq.revision());
        true
    }
}
