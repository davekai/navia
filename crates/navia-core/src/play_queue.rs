//! The play queue as one model: the list it plays from, Next in Queue, Autoplay's songs and the player's entries.
//! Every change comes back as [`Edit`]s for the player, so an app, the player and a saved session can't drift apart.
//! The randomized test at the bottom checks the invariants.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::queue_order;

/// How many list songs the player holds at once. The rest feeds in as the queue gets there.
pub const WINDOW: usize = 2000;
/// The rest feeds in when fewer list songs than this are left in the player.
pub const FEED_AHEAD: usize = 32;

/// What an entry is to the queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Kind {
    /// A song of the list the queue plays from.
    List,
    /// A song the listener queued with Next in Queue. It leaves once it has played.
    Queued,
    /// A song from outside the list, such as Autoplay's or one played on its own.
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry<T> {
    /// Unique within a queue. A list entry's id is its list item's id.
    pub id: u64,
    pub song: T,
    pub kind: Kind,
}

/// A change for the audio player in its own terms, applied in order. `bool` is the player's queued flag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edit<T> {
    SetQueue { items: Vec<(T, bool)>, start: usize },
    InsertAt { index: usize, items: Vec<(T, bool)> },
    Remove(Vec<usize>),
    /// Everything after the song being heard.
    ReplaceUpcoming(Vec<(T, bool)>),
    Move { positions: Vec<usize>, before: usize },
    SetQueued { positions: Vec<usize>, queued: bool },
    /// `queue_order::shuffled_queued_order` with this seed, as the player works it out.
    ShuffleQueued(u64),
    JumpTo(usize),
}

/// What a shuffle mixes, so the mix can be worked out off the UI thread.
/// The playing song stays first if it's in the list.
#[derive(Clone, Debug)]
pub struct MixRequest<T> {
    pub revision: u64,
    pub songs: Vec<T>,
    pub current: Option<usize>,
}

/// The Undo for Clear <source>, valid while nothing has changed since.
#[derive(Clone, Debug)]
pub struct ListUndo<T> {
    revision: u64,
    before: Box<PlayQueue<T>>,
    songs: Vec<T>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayQueue<T> {
    /// What the player holds, in play order.
    entries: Vec<Entry<T>>,
    /// The playing entry. Meaningless while `entries` is empty.
    pos: usize,
    /// The list in its own order, as (id, song).
    list: Vec<(u64, T)>,
    /// The list's ids in play order.
    play: Vec<u64>,
    /// `play[..fed]` has reached the player.
    fed: usize,
    shuffle: bool,
    /// A shuffle's mix is being worked out and the list waits for it.
    mixing: bool,
    /// The list song that played last. Queued and other songs don't move it.
    /// With shuffle off, play carries on in list order right after it.
    #[serde(default)]
    last_list: Option<u64>,
    /// Changes when the listener starts something or the play order is rebuilt, never when a song merely plays.
    /// That's why Autoplay's own songs can't start a context.
    #[serde(default)]
    context: u64,
    next_id: u64,
    #[serde(default = "default_window")]
    window: usize,
    #[serde(default = "default_feed_ahead")]
    feed_ahead: usize,
    /// Moves with every change, so answers worked out meanwhile can be checked for staleness.
    #[serde(skip)]
    revision: u64,
}

fn default_window() -> usize {
    WINDOW
}

fn default_feed_ahead() -> usize {
    FEED_AHEAD
}

impl<T: Clone + Eq + std::hash::Hash> Default for PlayQueue<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Clone + Eq + std::hash::Hash> PlayQueue<T> {
    pub fn new() -> Self {
        Self::with_window(WINDOW, FEED_AHEAD)
    }

    /// A queue holding `window` list songs in the player, feeding more when fewer than `feed_ahead` are left.
    pub fn with_window(window: usize, feed_ahead: usize) -> Self {
        Self { entries: Vec::new(), pos: 0, list: Vec::new(), play: Vec::new(), fed: 0, shuffle: false, mixing: false, last_list: None, context: queue_order::random_seed(), next_id: 0, window: window.max(2), feed_ahead, revision: 0 }
    }

    /// A queue known only as it stands, such as an old session or another device's queue.
    /// Songs at `queued` are Next in Queue. `shuffle` only sets the switch, since there's no other order to go back to.
    pub fn from_queue(songs: Vec<T>, pos: usize, queued: std::ops::Range<usize>, shuffle: bool) -> Self {
        let mut q = Self::new();
        q.shuffle = shuffle;
        for (i, song) in songs.into_iter().enumerate() {
            let id = q.fresh_id();
            let kind = if queued.contains(&i) { Kind::Queued } else { Kind::List };
            if kind == Kind::List {
                q.list.push((id, song.clone()));
                q.play.push(id);
            }
            q.entries.push(Entry { id, song, kind });
        }
        q.fed = q.play.len();
        q.pos = pos.min(q.entries.len().saturating_sub(1));
        q.changed();
        q
    }

    // Reading.

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> &[Entry<T>] {
        &self.entries
    }

    pub fn songs(&self) -> Vec<T> {
        self.entries.iter().map(|e| e.song.clone()).collect()
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn current(&self) -> Option<&Entry<T>> {
        self.entries.get(self.pos)
    }

    pub fn shuffle(&self) -> bool {
        self.shuffle
    }

    pub fn mixing(&self) -> bool {
        self.mixing
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The list's songs in its own order.
    pub fn list_songs(&self) -> Vec<T> {
        self.list.iter().map(|(_, s)| s.clone()).collect()
    }

    /// The list's songs in play order.
    pub fn play_songs(&self) -> Vec<T> {
        let by_id: HashMap<u64, &T> = self.list.iter().map(|(i, s)| (*i, s)).collect();
        self.play.iter().filter_map(|i| by_id.get(i).map(|s| (*s).clone())).collect()
    }

    /// How many queued entries follow the playing one.
    pub fn queued_run(&self) -> usize {
        if self.entries.is_empty() {
            return 0;
        }
        self.entries[self.pos + 1..].iter().take_while(|e| e.kind == Kind::Queued).count()
    }

    pub fn current_queued(&self) -> bool {
        self.current().is_some_and(|e| e.kind == Kind::Queued)
    }

    /// How many Autoplay songs sit at the end of the player, after the playing one.
    pub fn other_tail(&self) -> usize {
        if self.entries.is_empty() {
            return 0;
        }
        self.entries[self.pos + 1..].iter().rev().take_while(|e| e.kind == Kind::Other).count()
    }

    /// How many entries follow the playing one, before Autoplay's.
    pub fn up_next_len(&self) -> usize {
        if self.entries.is_empty() {
            return 0;
        }
        self.entries.len() - self.pos - 1 - self.other_tail()
    }

    /// List songs not in the player yet.
    pub fn unfed(&self) -> usize {
        self.play.len() - self.fed
    }

    /// How many list songs are still to come after the playing one, fed or not.
    pub fn list_left(&self) -> usize {
        self.play.len().saturating_sub(self.next_play())
    }

    /// The last `n` songs of the list in play order.
    pub fn play_tail(&self, n: usize) -> Vec<T> {
        self.play[self.play.len().saturating_sub(n)..].iter().filter_map(|id| self.list.iter().rev().find(|(i, _)| i == id).map(|(_, s)| s.clone())).collect()
    }

    /// The context the listener started. Autoplay plans once per context.
    pub fn context(&self) -> u64 {
        self.context
    }

    /// The songs Autoplay's context is anchored on, oldest first.
    /// The last `n` of the list in play order, else the queued songs, never a song from outside the list.
    pub fn anchor_tail(&self, n: usize) -> Vec<T> {
        if !self.play.is_empty() {
            return self.play_tail(n);
        }
        let queued: Vec<&Entry<T>> = self.entries.iter().filter(|e| e.kind == Kind::Queued).collect();
        queued[queued.len().saturating_sub(n)..].iter().map(|e| e.song.clone()).collect()
    }

    fn new_context(&mut self) {
        self.context = queue_order::random_seed();
    }

    /// Where the next list song is in the play order, counted from the playing song or the list song before it.
    pub fn next_play(&self) -> usize {
        let Some(cur) = self.current() else { return 0 };
        if cur.kind == Kind::List {
            return self.play_index(cur.id).map_or(self.fed, |i| i + 1);
        }
        if let Some(i) = self.entries[..self.pos].iter().rev().find(|e| e.kind == Kind::List).and_then(|e| self.play_index(e.id)) {
            return i + 1;
        }
        self.entries[self.pos + 1..].iter().find(|e| e.kind == Kind::List).and_then(|e| self.play_index(e.id)).unwrap_or(self.fed)
    }

    fn play_index(&self, id: u64) -> Option<usize> {
        self.play.iter().position(|i| *i == id)
    }

    fn own_index(&self, id: u64) -> Option<usize> {
        self.list.iter().position(|(i, _)| *i == id)
    }

    /// List entries for these ids, looked up once since lists can be long.
    fn list_entries(&self, ids: &[u64]) -> Vec<Entry<T>> {
        let by_id: HashMap<u64, &T> = self.list.iter().map(|(i, s)| (*i, s)).collect();
        ids.iter().filter_map(|i| by_id.get(i).map(|s| Entry { id: *i, song: (*s).clone(), kind: Kind::List })).collect()
    }

    fn items(entries: &[Entry<T>]) -> Vec<(T, bool)> {
        entries.iter().map(|e| (e.song.clone(), e.kind == Kind::Queued)).collect()
    }

    fn fresh_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn changed(&mut self) {
        self.revision += 1;
        if let Some(e) = self.current().filter(|e| e.kind == Kind::List) {
            self.last_list = Some(e.id);
        }
    }

    fn play_is_own(&self) -> bool {
        self.play.len() == self.list.len() && self.play.iter().zip(&self.list).all(|(p, (i, _))| p == i)
    }

    // The player's events.

    /// The player moved to `index`, and `left` is where a played queued song was taken out.
    /// Returns what to feed in.
    pub fn track_changed(&mut self, index: usize, left: Option<usize>) -> Vec<Edit<T>> {
        // Only if it was the playing entry here. Otherwise this queue isn't the player's yet.
        if let Some(l) = left.filter(|l| *l == self.pos && self.entries.get(*l).is_some_and(|e| e.kind == Kind::Queued)) {
            self.entries.remove(l);
        }
        if !self.entries.is_empty() {
            self.pos = index.min(self.entries.len() - 1);
        }
        self.changed();
        self.feed()
    }

    /// Feeds in more of the list once the player holds fewer than `feed_ahead` of its songs to come.
    pub fn feed(&mut self) -> Vec<Edit<T>> {
        if self.mixing || self.entries.is_empty() || self.fed >= self.play.len() {
            return Vec::new();
        }
        let ahead = self.entries[self.pos + 1..].iter().filter(|e| e.kind == Kind::List).count();
        if ahead >= self.feed_ahead {
            return Vec::new();
        }
        let n = self.window.min(self.play.len() - self.fed);
        let ids: Vec<u64> = self.play[self.fed..self.fed + n].to_vec();
        let new = self.list_entries(&ids);
        self.fed += n;
        let at = self.entries.len() - self.other_tail();
        let items = Self::items(&new);
        self.entries.splice(at..at, new);
        self.changed();
        vec![Edit::InsertAt { index: at, items }]
    }

    // Starting.

    /// A new list starts at `index`. Next in Queue stays next and everything else is replaced.
    /// When shuffled, only the chosen song is in the player until the mix arrives.
    pub fn start(&mut self, songs: Vec<T>, index: usize, shuffled: bool) -> Vec<Edit<T>> {
        // Nothing to play leaves the queue as it is.
        if songs.is_empty() {
            return Vec::new();
        }
        let kept: Vec<Entry<T>> = if self.entries.is_empty() { Vec::new() } else { self.entries[self.pos + 1..self.pos + 1 + self.queued_run()].to_vec() };
        self.shuffle = shuffled;
        self.list = songs.into_iter().map(|s| (self.fresh_id(), s)).collect();
        self.entries.clear();
        self.pos = 0;
        self.mixing = false;
        let index = index.min(self.list.len() - 1);
        let list_entry = |q: &Self, k: usize| Entry { id: q.list[k].0, song: q.list[k].1.clone(), kind: Kind::List };
        if shuffled {
            let first = self.list[index].0;
            self.play = std::iter::once(first).chain(self.list.iter().map(|(i, _)| *i).filter(|i| *i != first)).collect();
            self.fed = 1;
            self.entries.push(list_entry(self, index));
            self.mixing = self.list.len() > 1;
        } else {
            self.play = self.list.iter().map(|(i, _)| *i).collect();
            let start = index.saturating_sub(self.window / 4);
            let end = (start + self.window).min(self.list.len()).max(index + 1);
            self.entries = (start..end).map(|k| list_entry(self, k)).collect();
            self.pos = index - start;
            self.fed = end;
        }
        let at = self.pos + 1;
        self.entries.splice(at..at, kept);
        self.new_context();
        self.changed();
        vec![Edit::SetQueue { items: Self::items(&self.entries), start: self.pos }]
    }

    /// A saved queue goes back into the player, paused at its place.
    pub fn load_edits(&self) -> Vec<Edit<T>> {
        if self.entries.is_empty() {
            return Vec::new();
        }
        vec![Edit::SetQueue { items: Self::items(&self.entries), start: self.pos }]
    }

    // Shuffle.

    /// Turns shuffle on or off. Next in Queue and Autoplay's songs stay as they are.
    /// Off restores the list's order at once. On asks for a mix, unless an Autoplay song is playing.
    pub fn set_shuffle(&mut self, on: bool) -> Vec<Edit<T>> {
        if on == self.shuffle {
            return Vec::new();
        }
        self.shuffle = on;
        self.changed();
        if on {
            self.mixing = self.can_mix();
            return Vec::new();
        }
        self.mixing = false;
        if self.play_is_own() {
            return self.feed();
        }
        self.rebuild_in_order()
    }

    /// Whether a mix makes sense, meaning the list has songs and the playing song belongs to it.
    fn can_mix(&self) -> bool {
        let Some(cur) = self.current() else { return false };
        match cur.kind {
            Kind::List => self.list.len() > 1,
            Kind::Queued => !self.list.is_empty() && self.next_play() < self.play.len(),
            Kind::Other => false,
        }
    }

    /// What to mix while `mixing`.
    pub fn mix_request(&self) -> Option<MixRequest<T>> {
        if !self.mixing {
            return None;
        }
        let current = self.current().filter(|e| e.kind == Kind::List).and_then(|e| self.own_index(e.id));
        Some(MixRequest { revision: self.revision, songs: self.list_songs(), current })
    }

    /// Applies the mix for `request`, given as list positions in play order.
    /// Returns `None` if the queue changed meanwhile, so the caller asks again.
    pub fn apply_mix(&mut self, request: &MixRequest<T>, order: &[usize]) -> Option<Vec<Edit<T>>> {
        if !self.mixing || request.revision != self.revision || order.len() != self.list.len() {
            return None;
        }
        let mut seen = vec![false; order.len()];
        if order.iter().any(|&i| i >= seen.len() || std::mem::replace(&mut seen[i], true)) || request.current.is_some_and(|c| order.first() != Some(&c)) {
            return None;
        }
        let cur = self.entries[self.pos].clone();
        let queued: Vec<Entry<T>> = self.entries[self.pos + 1..self.pos + 1 + self.queued_run()].to_vec();
        let tail: Vec<Entry<T>> = self.entries[self.entries.len() - self.other_tail()..].to_vec();
        self.play = order.iter().map(|&i| self.list[i].0).collect();
        let first = usize::from(cur.kind == Kind::List);
        self.fed = self.play.len().min(self.window.max(first + 1));
        let up = self.list_entries(&self.play[first..self.fed]);
        let mut after = queued;
        after.extend(up);
        if self.fed == self.play.len() {
            after.extend(tail);
        }
        let mut edits = Vec::new();
        if self.pos > 0 {
            edits.push(Edit::Remove((0..self.pos).collect()));
        }
        edits.push(Edit::ReplaceUpcoming(Self::items(&after)));
        self.entries = std::iter::once(cur).chain(after).collect();
        self.pos = 0;
        self.mixing = false;
        self.new_context();
        self.changed();
        edits.extend(self.feed());
        Some(edits)
    }

    /// Back to the list's own order, with a window around the playing song.
    fn rebuild_in_order(&mut self) -> Vec<Edit<T>> {
        self.play = self.list.iter().map(|(i, _)| *i).collect();
        if self.entries.is_empty() {
            self.fed = 0;
            return Vec::new();
        }
        let cur = self.entries[self.pos].clone();
        let is_list = cur.kind == Kind::List;
        // For a queued or Autoplay song, resume right after the list song that played last.
        // Songs heard in the shuffled order come again.
        let at = if is_list {
            self.own_index(cur.id).unwrap_or(self.list.len())
        } else if let Some(a) = self.last_list.and_then(|id| self.own_index(id)) {
            a + 1
        } else if let Some(a) = self.entries[..self.pos].iter().rev().find(|e| e.kind == Kind::List).and_then(|e| self.own_index(e.id)) {
            a + 1
        } else {
            self.entries[self.pos + 1..].iter().find(|e| e.kind == Kind::List).and_then(|e| self.own_index(e.id)).unwrap_or(self.list.len())
        };
        let start = at.saturating_sub(self.window / 4);
        let end = (start + self.window).min(self.list.len()).max((at + usize::from(is_list)).min(self.list.len()));
        let head = self.list_entries(&self.play[start..at]);
        let up = self.list_entries(&self.play[(at + usize::from(is_list)).min(end)..end]);
        let queued: Vec<Entry<T>> = self.entries[self.pos + 1..self.pos + 1 + self.queued_run()].to_vec();
        let tail: Vec<Entry<T>> = self.entries[self.entries.len() - self.other_tail()..].to_vec();
        self.fed = end;
        let mut after = queued;
        after.extend(up);
        if end == self.play.len() {
            after.extend(tail);
        }
        let mut edits = Vec::new();
        if self.pos > 0 {
            edits.push(Edit::Remove((0..self.pos).collect()));
        }
        if !head.is_empty() {
            edits.push(Edit::InsertAt { index: 0, items: Self::items(&head) });
        }
        edits.push(Edit::ReplaceUpcoming(Self::items(&after)));
        self.pos = head.len();
        self.entries = head.into_iter().chain(std::iter::once(cur)).chain(after).collect();
        self.new_context();
        self.changed();
        edits.extend(self.feed());
        edits
    }

    // Next in Queue and Next from.

    /// Puts songs at `offset` among Up Next, into Next in Queue or into Next from.
    /// Songs put in Next from join the list. Returns the edits and where the songs went.
    pub fn insert(&mut self, songs: Vec<T>, offset: usize, queued: bool) -> (Vec<Edit<T>>, usize) {
        if songs.is_empty() {
            return (Vec::new(), self.pos + 1);
        }
        if self.entries.is_empty() {
            // With nothing playing, the songs become the queue.
            let kind = if queued { Kind::Queued } else { Kind::List };
            if !queued {
                self.list = songs.iter().map(|s| (self.fresh_id(), s.clone())).collect();
                self.play = self.list.iter().map(|(i, _)| *i).collect();
                self.fed = self.play.len();
                self.entries = self.list.iter().map(|(i, s)| Entry { id: *i, song: s.clone(), kind }).collect();
            } else {
                self.entries = songs.into_iter().map(|song| Entry { id: self.fresh_id(), song, kind }).collect();
            }
            self.pos = 0;
            self.changed();
            return (vec![Edit::SetQueue { items: Self::items(&self.entries), start: 0 }], 0);
        }
        let first = self.pos + 1;
        let run = self.queued_run();
        let up_next = self.up_next_len();
        let offset = if queued { offset.min(run) } else { offset.clamp(run, up_next) };
        let at = first + offset;
        let new: Vec<Entry<T>> = songs.into_iter().map(|song| Entry { id: self.fresh_id(), song, kind: if queued { Kind::Queued } else { Kind::List } }).collect();
        if !queued {
            let ids: Vec<u64> = new.iter().map(|e| e.id).collect();
            let (play_at, own_at) = self.list_place(at);
            self.play.splice(play_at..play_at, ids.iter().copied());
            self.list.splice(own_at..own_at, new.iter().map(|e| (e.id, e.song.clone())));
            self.fed += ids.len();
        }
        let items = Self::items(&new);
        self.entries.splice(at..at, new);
        self.changed();
        (vec![Edit::InsertAt { index: at, items }], at)
    }

    /// Where list songs placed before entry `at` go, in play order and in list order.
    fn list_place(&self, at: usize) -> (usize, usize) {
        if let Some(e) = self.entries[..at].iter().rev().find(|e| e.kind == Kind::List) {
            return (self.play_index(e.id).map_or(self.fed, |i| i + 1), self.own_index(e.id).map_or(self.list.len(), |i| i + 1));
        }
        if let Some(e) = self.entries[at..].iter().find(|e| e.kind == Kind::List) {
            return (self.play_index(e.id).unwrap_or(self.fed), self.own_index(e.id).unwrap_or(self.list.len()));
        }
        // No list song in the player, so go before the unfed part.
        let own = self.play.get(self.fed).and_then(|i| self.own_index(*i)).unwrap_or(self.list.len());
        (self.fed, own)
    }

    /// Adds songs to the end of the list, such as a Search song's radio.
    /// They go before Autoplay's songs in the player when the whole list is there.
    pub fn append_to_list(&mut self, songs: Vec<T>) -> (Vec<Edit<T>>, usize) {
        let new: Vec<(u64, T)> = songs.into_iter().map(|s| (self.fresh_id(), s)).collect();
        let all_fed = self.fed == self.play.len();
        self.play.extend(new.iter().map(|(i, _)| *i));
        self.list.extend(new.iter().cloned());
        if !new.is_empty() {
            self.new_context();
        }
        self.changed();
        let at = self.entries.len() - self.other_tail();
        if !all_fed || self.mixing || self.entries.is_empty() {
            return (self.feed(), at);
        }
        self.fed = self.play.len();
        let entries: Vec<Entry<T>> = new.into_iter().map(|(id, song)| Entry { id, song, kind: Kind::List }).collect();
        let items = Self::items(&entries);
        self.entries.splice(at..at, entries);
        (vec![Edit::InsertAt { index: at, items }], at)
    }

    /// Removes Up Next entries at these offsets after the playing one.
    /// List songs leave the list too, so turning shuffle off doesn't bring them back.
    pub fn remove(&mut self, offsets: &[usize]) -> Vec<Edit<T>> {
        let first = self.pos + 1;
        let mut gone: Vec<usize> = offsets.iter().map(|o| first + o).filter(|i| *i < self.entries.len()).collect();
        gone.sort_unstable();
        gone.dedup();
        if gone.is_empty() {
            return Vec::new();
        }
        for &i in gone.iter().rev() {
            let e = self.entries.remove(i);
            if e.kind == Kind::List {
                self.forget_list_id(e.id);
            }
        }
        self.changed();
        let mut edits = vec![Edit::Remove(gone)];
        edits.extend(self.feed());
        edits
    }

    fn forget_list_id(&mut self, id: u64) {
        if let Some(p) = self.play_index(id) {
            self.play.remove(p);
            if p < self.fed {
                self.fed -= 1;
            }
        }
        if let Some(o) = self.own_index(id) {
            self.list.remove(o);
        }
    }

    /// Removes Autoplay's songs at the end.
    pub fn drop_other_tail(&mut self) -> Vec<Edit<T>> {
        let tail = self.other_tail();
        if tail == 0 {
            return Vec::new();
        }
        let len = self.entries.len();
        self.entries.truncate(len - tail);
        self.changed();
        vec![Edit::Remove((len - tail..len).collect())]
    }

    /// Clear Queue removes Next in Queue. Returns the songs for Undo through `insert(songs, 0, true)`.
    pub fn clear_queued(&mut self) -> (Vec<Edit<T>>, Vec<T>) {
        let run = self.queued_run();
        if run == 0 {
            return (Vec::new(), Vec::new());
        }
        let first = self.pos + 1;
        let songs: Vec<T> = self.entries.drain(first..first + run).map(|e| e.song).collect();
        self.changed();
        (vec![Edit::Remove((first..first + run).collect())], songs)
    }

    /// Shuffle Queue mixes Next in Queue once, as the player does with the same seed.
    pub fn shuffle_queued(&mut self, seed: u64) -> Vec<Edit<T>> {
        let flags: Vec<bool> = self.entries.iter().map(|e| e.kind == Kind::Queued).collect();
        let Some(order) = queue_order::shuffled_queued_order(&flags, Some(self.pos), seed) else { return Vec::new() };
        self.entries = queue_order::apply_order(&self.entries, &order);
        self.changed();
        vec![Edit::ShuffleQueued(seed)]
    }

    /// Clear <source> removes the rest of the list after Next in Queue. Returns its Undo.
    pub fn clear_list(&mut self) -> Option<(Vec<Edit<T>>, ListUndo<T>)> {
        if self.entries.is_empty() {
            return None;
        }
        let next = self.next_play();
        if next >= self.play.len() {
            return None;
        }
        let before = Box::new(self.clone());
        let songs: Vec<T> = self.list_entries(&self.play[next..]).into_iter().map(|e| e.song).collect();
        let gone_ids: std::collections::HashSet<u64> = self.play[next..].iter().copied().collect();
        let first = self.pos + 1;
        let gone: Vec<usize> = (first..self.entries.len()).filter(|i| self.entries[*i].kind == Kind::List && gone_ids.contains(&self.entries[*i].id)).collect();
        for &i in gone.iter().rev() {
            self.entries.remove(i);
        }
        self.play.truncate(next);
        self.list.retain(|(i, _)| !gone_ids.contains(i));
        self.fed = self.play.len();
        self.new_context();
        self.changed();
        let edits = if gone.is_empty() { Vec::new() } else { vec![Edit::Remove(gone)] };
        Some((edits, ListUndo { revision: self.revision, before, songs }))
    }

    /// Undoes Clear <source>, exactly if nothing changed since.
    /// Otherwise the songs come back right after Next in Queue as list songs.
    pub fn undo_clear_list(&mut self, undo: ListUndo<T>) -> Vec<Edit<T>> {
        if undo.revision == self.revision {
            let before = *undo.before;
            // The player gets back each entry it lost at its old place, in order.
            let now: std::collections::HashSet<u64> = self.entries.iter().map(|e| e.id).collect();
            let edits = before.entries.iter().enumerate().filter(|(_, e)| !now.contains(&e.id)).map(|(i, e)| Edit::InsertAt { index: i, items: Self::items(std::slice::from_ref(e)) }).collect();
            let revision = self.revision;
            *self = before;
            self.revision = revision;
            self.changed();
            return edits;
        }
        let run = self.queued_run();
        self.insert(undo.songs, run, false).0
    }

    /// A drag within the queue's sections. Entries at `offsets` land before Up Next position `before`.
    /// Moved songs take the section they land in, joining or leaving the list.
    pub fn move_entries(&mut self, offsets: &[usize], before: usize, into_queued: bool) -> Vec<Edit<T>> {
        if self.entries.is_empty() {
            return Vec::new();
        }
        let first = self.pos + 1;
        let run = self.queued_run();
        let up_next = self.up_next_len();
        let before = before.min(up_next);
        let at = if into_queued { before.min(run) } else { before.max(run) };
        let abs: Vec<usize> = offsets.iter().map(|o| first + o).filter(|i| *i < self.entries.len()).collect();
        if abs.is_empty() {
            return Vec::new();
        }
        let flags: Vec<bool> = self.entries.iter().map(|e| e.kind == Kind::Queued).collect();
        let (order, _) = queue_order::section_move(&flags, self.pos, &abs, at + first, into_queued);
        let moved_ids: std::collections::HashSet<u64> = abs.iter().map(|i| self.entries[*i].id).collect();
        let ws = self.entries.iter().find(|e| e.kind == Kind::List).and_then(|e| self.play_index(e.id)).unwrap_or(self.fed);
        let mut entries = order.as_ref().map_or_else(|| self.entries.clone(), |o| queue_order::apply_order(&self.entries, o));
        let mut joined = Vec::new();
        let mut next_id = self.next_id;
        for e in entries.iter_mut().filter(|e| moved_ids.contains(&e.id)) {
            let kind = if into_queued { Kind::Queued } else { Kind::List };
            if e.kind == Kind::List && kind == Kind::Queued {
                // Leaving the list. The entry keeps an id that no list item has anymore.
                if let Some(o) = self.list.iter().position(|(i, _)| *i == e.id) {
                    self.list.remove(o);
                }
            } else if e.kind != Kind::List && kind == Kind::List {
                // A song joining the list is a new member of it.
                next_id += 1;
                e.id = next_id;
                joined.push(e.id);
            }
            e.kind = kind;
        }
        self.next_id = next_id;
        // The play order's part in the player follows the player's list entries.
        let in_player: Vec<u64> = entries.iter().filter(|e| e.kind == Kind::List).map(|e| e.id).collect();
        let in_set: std::collections::HashSet<u64> = in_player.iter().copied().collect();
        let head: Vec<u64> = self.play[..ws.min(self.fed)].iter().copied().filter(|i| !in_set.contains(i)).collect();
        let rest: Vec<u64> = self.play[self.fed..].to_vec();
        self.fed = head.len() + in_player.len();
        self.play = head.into_iter().chain(in_player.iter().copied()).chain(rest).collect();
        // The list's own order: songs joining go after the list song before them.
        for id in &joined {
            let k = entries.iter().position(|e| e.id == *id).unwrap_or(0);
            let song = entries[k].song.clone();
            let own_at = entries[..k].iter().rev().find(|e| e.kind == Kind::List && !joined.contains(&e.id)).and_then(|e| self.own_index(e.id)).map(|i| i + 1).or_else(|| entries[k + 1..].iter().find(|e| e.kind == Kind::List && !joined.contains(&e.id)).and_then(|e| self.own_index(e.id))).unwrap_or(self.list.len());
            self.list.insert(own_at, (*id, song));
        }
        if !self.shuffle {
            // In order, the list's own order is the play order.
            let by_id: HashMap<u64, T> = self.list.drain(..).collect();
            self.list = self.play.iter().filter_map(|i| by_id.get(i).map(|s| (*i, s.clone()))).collect();
        }
        std::mem::swap(&mut self.entries, &mut entries);
        self.changed();
        let mut edits = Vec::new();
        if order.is_some() {
            edits.push(Edit::Move { positions: abs, before: at + first });
        }
        let (queued, plain): (Vec<usize>, Vec<usize>) = (first..self.entries.len()).partition(|i| self.entries[*i].kind == Kind::Queued);
        edits.push(Edit::SetQueued { positions: plain, queued: false });
        if !queued.is_empty() {
            edits.push(Edit::SetQueued { positions: queued, queued: true });
        }
        edits
    }

    /// A double-click on an Up Next entry. Picking a queued song drops the queued songs above it.
    /// The player's `TrackChanged` follows.
    pub fn jump(&mut self, index: usize) -> Vec<Edit<T>> {
        if index <= self.pos || index >= self.entries.len() {
            return Vec::new();
        }
        let flags: Vec<bool> = self.entries.iter().map(|e| e.kind == Kind::Queued).collect();
        let jump = queue_order::queue_jump(&flags, self.pos, index);
        let mut edits = Vec::new();
        if !jump.dropped.is_empty() {
            for &i in jump.dropped.iter().rev() {
                self.entries.remove(i);
            }
            edits.push(Edit::Remove(jump.dropped.clone()));
        } else if !jump.moved.is_empty() {
            let order = queue_order::queue_jump_order(self.entries.len(), &jump);
            self.entries = queue_order::apply_order(&self.entries, &order);
            edits.push(Edit::Move { positions: jump.moved.clone(), before: jump.before });
        }
        self.changed();
        edits.push(Edit::JumpTo(jump.at));
        edits
    }

    // Autoplay.

    /// Autoplay's next song goes into the player at the end, so the join stays gapless.
    pub fn hand_over(&mut self, song: T) -> Vec<Edit<T>> {
        let at = self.entries.len();
        let entry = Entry { id: self.fresh_id(), song, kind: Kind::Other };
        let items = Self::items(std::slice::from_ref(&entry));
        self.entries.push(entry);
        self.changed();
        vec![Edit::InsertAt { index: at, items }]
    }

    /// Plays a song on its own right after the playing one, leaving Up Next as it was.
    pub fn play_other(&mut self, song: T) -> Vec<Edit<T>> {
        let at = if self.entries.is_empty() { 0 } else { self.pos + 1 };
        let entry = Entry { id: self.fresh_id(), song, kind: Kind::Other };
        let items = Self::items(std::slice::from_ref(&entry));
        self.entries.insert(at, entry);
        self.changed();
        vec![Edit::InsertAt { index: at, items }, Edit::JumpTo(at)]
    }

    // While the player holds something else, like a station or someone followed.

    /// Restarts the queue from entry `index` after the player held something else.
    /// The player gets the whole queue back.
    pub fn restart_at(&mut self, index: usize) -> Vec<Edit<T>> {
        if self.entries.is_empty() {
            return Vec::new();
        }
        let index = index.min(self.entries.len() - 1);
        let mut at = index;
        if index > self.pos
            && let Some(Edit::JumpTo(a)) = self.jump(index).last()
        {
            at = *a;
        }
        if at != self.pos && self.entries[self.pos].kind == Kind::Queued {
            self.entries.remove(self.pos);
            if at > self.pos {
                at -= 1;
            }
        }
        self.pos = at.min(self.entries.len() - 1);
        self.changed();
        self.load_edits()
    }

    /// Moves the queue's place on by one while following someone, as the player would.
    pub fn step_waiting(&mut self) {
        if self.pos + 1 >= self.entries.len() {
            return;
        }
        if self.entries[self.pos].kind == Kind::Queued {
            self.entries.remove(self.pos);
        } else {
            self.pos += 1;
        }
        self.changed();
    }

    /// while following someone, the first song of Next in Queue leaves the waiting queue to be
    /// played through them (a listening session's turn). the player doesn't hold this queue then.
    pub fn take_queued_next(&mut self) -> Option<T> {
        if self.queued_run() == 0 {
            return None;
        }
        let entry = self.entries.remove(self.pos + 1);
        self.changed();
        Some(entry.song)
    }

    /// The song followed (listen along) stays as the playing entry, right before the listener's
    /// own song, when following ends. The player holds only that song: the edits put the queue
    /// back before and after it.
    pub fn keep_in_front(&mut self, song: T) -> Vec<Edit<T>> {
        let at = self.pos.min(self.entries.len());
        let id = self.fresh_id();
        self.entries.insert(at, Entry { id, song, kind: Kind::Other });
        self.pos = at;
        self.changed();
        let head = Self::items(&self.entries[..at]);
        let tail = Self::items(&self.entries[at + 1..]);
        let mut edits = Vec::new();
        if !head.is_empty() {
            edits.push(Edit::InsertAt { index: 0, items: head });
        }
        if !tail.is_empty() {
            edits.push(Edit::InsertAt { index: at + 1, items: tail });
        }
        edits
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// The audio player's queue as `audio::player::Player` keeps it, for checking the edits.
    #[derive(Clone, Debug)]
    struct Sim {
        entries: Vec<(u32, bool)>,
        audible: usize,
        repeat_all: bool,
        repeat_one: bool,
        /// Repeat wrapped around since the list started, so earlier songs can play again.
        wrapped: bool,
    }

    /// The player's new index and where a played queued song was taken out.
    type Moved = Option<(usize, Option<usize>)>;

    impl Sim {
        fn apply(&mut self, edits: &[Edit<u32>]) -> Moved {
            let mut moved = None;
            for e in edits {
                match e {
                    Edit::SetQueue { items, start } => {
                        self.entries = items.clone();
                        self.audible = *start;
                    }
                    Edit::InsertAt { index, items } => {
                        let at = (*index).min(self.entries.len());
                        self.entries.splice(at..at, items.iter().cloned());
                        if at <= self.audible && !self.entries.is_empty() && self.entries.len() > items.len() {
                            self.audible += items.len();
                        }
                    }
                    Edit::Remove(gone) => {
                        let audible_gone = gone.contains(&self.audible);
                        let before = gone.iter().filter(|g| **g < self.audible).count();
                        let mut k = 0;
                        self.entries.retain(|_| {
                            k += 1;
                            !gone.contains(&(k - 1))
                        });
                        self.audible -= before;
                        assert!(!audible_gone, "the model never removes the song being heard");
                    }
                    Edit::ReplaceUpcoming(items) => {
                        self.entries.truncate(self.audible + 1);
                        self.entries.extend(items.iter().cloned());
                    }
                    Edit::Move { positions, before } => {
                        let audible = self.audible;
                        let mut tagged: Vec<(usize, (u32, bool))> = self.entries.iter().cloned().enumerate().collect();
                        queue_order::move_items(&mut tagged, positions, *before);
                        self.entries = tagged.iter().map(|(_, e)| *e).collect();
                        self.audible = tagged.iter().position(|(i, _)| *i == audible).unwrap();
                    }
                    Edit::SetQueued { positions, queued } => {
                        for p in positions {
                            if let Some(e) = self.entries.get_mut(*p) {
                                e.1 = *queued;
                            }
                        }
                    }
                    Edit::ShuffleQueued(seed) => {
                        let flags: Vec<bool> = self.entries.iter().map(|e| e.1).collect();
                        if let Some(order) = queue_order::shuffled_queued_order(&flags, Some(self.audible), *seed) {
                            self.entries = queue_order::apply_order(&self.entries, &order);
                        }
                    }
                    Edit::JumpTo(i) => moved = self.go(*i),
                }
            }
            moved
        }

        /// The audible entry changes to `to`, and a heard queued song leaves.
        fn go(&mut self, to: usize) -> Moved {
            if to >= self.entries.len() || (to == self.audible && !self.repeat_one) {
                return None;
            }
            if to == self.audible {
                return Some((to, None));
            }
            let mut to = to;
            let mut left = None;
            if self.entries[self.audible].1 {
                left = Some(self.audible);
                self.entries.remove(self.audible);
                if to > self.audible {
                    to -= 1;
                }
            }
            self.audible = to;
            Some((to, left))
        }

        fn next(&mut self) -> Moved {
            let len = self.entries.len();
            if self.audible + 1 < len {
                self.go(self.audible + 1)
            } else if self.repeat_all && len > 1 {
                self.wrapped = true;
                self.go(0)
            } else {
                None
            }
        }

        fn previous(&mut self) -> Moved {
            if self.audible > 0 {
                self.go(self.audible - 1)
            } else if self.repeat_all && self.entries.len() > 1 {
                self.wrapped = true;
                self.go(self.entries.len() - 1)
            } else {
                None
            }
        }
    }

    struct Fuzz {
        rng: u64,
        q: PlayQueue<u32>,
        sim: Sim,
        next_song: u32,
        /// Songs that may be in the queue: the list's, songs added since and the queued songs kept at the start.
        allowed: HashSet<u32>,
        /// Autoplay's songs, which are never in the list.
        others: HashSet<u32>,
        /// Autoplay's songs that have played, which Previous can bring back.
        heard: HashSet<u32>,
        /// The list's own order as it started or as an in-order drag last left it.
        origin: Vec<u64>,
        pending_mix: Option<MixRequest<u32>>,
        list_undo: Option<ListUndo<u32>>,
        queued_undo: Vec<u32>,
        log: Vec<String>,
        /// The player holds a station (`None`) or a followed song.
        hold: Option<Option<u32>>,
        session: u64,
        /// The list song that played last, tracked apart from the model.
        cursor: Option<u64>,
    }

    impl Fuzz {
        fn new(seed: u64) -> Self {
            Self {
                rng: seed,
                q: PlayQueue::with_window(6, 3),
                sim: Sim { entries: Vec::new(), audible: 0, repeat_all: false, repeat_one: false, wrapped: false },
                next_song: 1,
                allowed: HashSet::new(),
                others: HashSet::new(),
                heard: HashSet::new(),
                origin: Vec::new(),
                pending_mix: None,
                list_undo: None,
                queued_undo: Vec::new(),
                log: Vec::new(),
                hold: None,
                session: 0,
                cursor: None,
            }
        }

        fn rand(&mut self, n: usize) -> usize {
            self.rng = self.rng.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = self.rng;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            ((z ^ (z >> 31)) % n.max(1) as u64) as usize
        }

        fn songs(&mut self, n: usize) -> Vec<u32> {
            let first = self.next_song;
            self.next_song += n as u32;
            // Sometimes a song twice, as playlists can have.
            let mut v: Vec<u32> = (first..first + n as u32).collect();
            if n > 3 && self.rand(4) == 0 {
                let k = self.rand(n);
                v[k] = v[0];
            }
            v
        }

        fn edits(&mut self, edits: Vec<Edit<u32>>) {
            // While the player holds a station or someone followed, the app sends nothing.
            // The queue goes back whole afterwards.
            if self.hold.is_some() {
                return;
            }
            if let Some((index, left)) = self.sim.apply(&edits) {
                self.moved(index, left);
            }
        }

        fn moved(&mut self, index: usize, left: Option<usize>) {
            let feed = self.q.track_changed(index, left);
            if let Some(e) = self.q.current() {
                self.heard.insert(e.song);
                if e.kind == Kind::List {
                    self.cursor = Some(e.id);
                }
            }
            self.edits(feed);
        }

        fn engine(&mut self, moved: Moved) {
            if let Some((index, left)) = moved {
                self.moved(index, left);
            }
        }

        fn mix(&mut self, request: &MixRequest<u32>) -> Vec<usize> {
            let recency: Vec<f32> = (0..request.songs.len()).map(|_| if self.rand(3) == 0 { 0.9 } else { 0.0 }).collect();
            let style = if self.rand(2) == 0 { queue_order::ShuffleStyle::FewerRepeats } else { queue_order::ShuffleStyle::Standard };
            let seed = self.rand(1 << 30) as u64;
            match request.current {
                Some(c) => queue_order::shuffled_whole_order(&recency, c, 0, style, seed),
                None => queue_order::mixed_order(&recency, style, seed),
            }
        }

        fn step(&mut self) {
            let up_next = self.q.up_next_len();
            let op = self.rand(29);
            if self.hold.is_some() && matches!(op, 2..=6 | 22 | 24..=28) {
                return self.while_held(op);
            }
            let before_queued: Vec<u32> = {
                let p = self.q.pos();
                let run = self.q.queued_run();
                if self.q.is_empty() { Vec::new() } else { self.q.entries()[p + 1..p + 1 + run].iter().map(|e| e.song).collect() }
            };
            let mut shuffle_step = false;
            let mut mixed_now = false;
            let members = |q: &PlayQueue<u32>| {
                let mut v: Vec<u64> = q.list.iter().map(|(i, _)| *i).collect();
                v.sort_unstable();
                v
            };
            let list_before = members(&self.q);
            let mixed_before = !self.q.play_is_own();
            // Ops that never add songs to the list or remove them.
            let keeps_list = matches!(op, 2..=12 | 15 | 21 | 22) || (op == 16);
            match op {
                0 | 1 => {
                    let n = self.rand(14);
                    let songs = self.songs(n);
                    let at = self.rand(n.max(1));
                    let shuffled = self.rand(2) == 0;
                    self.log.push(format!("start {n} at {at} shuffled {shuffled}"));
                    if n > 0 {
                        // Playing something ends a station or following, and the followed song isn't kept.
                        self.hold = None;
                        self.sim.wrapped = false;
                        let kept: Vec<u32> = before_queued.clone();
                        self.allowed = songs.iter().copied().chain(kept).collect();
                        self.list_undo = None;
                        self.queued_undo.clear();
                        self.pending_mix = None;
                    }
                    let e = self.q.start(songs, at, shuffled);
                    self.edits(e);
                    if n > 0 {
                        self.origin = self.q.list.iter().map(|(i, _)| *i).collect();
                    }
                    if self.q.mixing() {
                        self.pending_mix = self.q.mix_request();
                    }
                }
                2..=4 => {
                    self.log.push("next".into());
                    let m = self.sim.next();
                    self.engine(m);
                }
                5 => {
                    self.log.push("previous".into());
                    let m = self.sim.previous();
                    self.engine(m);
                }
                6 => {
                    if up_next + self.q.other_tail() > 0 {
                        let target = self.q.pos() + 1 + self.rand(up_next + self.q.other_tail());
                        self.log.push(format!("jump {target}"));
                        let song = self.q.entries()[target].song;
                        let e = self.q.jump(target);
                        self.edits(e);
                        assert_eq!(self.q.current().map(|e| e.song), Some(song), "a jump plays the song clicked");
                    }
                }
                7 | 8 => {
                    let on = !self.q.shuffle();
                    self.log.push(format!("shuffle {on}"));
                    shuffle_step = true;
                    let e = self.q.set_shuffle(on);
                    self.edits(e);
                    self.pending_mix = self.q.mix_request();
                }
                9 | 10 => {
                    // The mix arrives, maybe after other changes that make it ask again.
                    if let Some(req) = self.pending_mix.take() {
                        let order = self.mix(&req);
                        self.log.push("mix arrives".into());
                        shuffle_step = true;
                        match self.q.apply_mix(&req, &order) {
                            Some(e) => {
                                self.edits(e);
                                mixed_now = true;
                            }
                            None => self.pending_mix = self.q.mix_request(),
                        }
                    }
                }
                11 => {
                    if !self.q.is_empty() {
                        let song = self.songs(1)[0];
                        self.allowed.insert(song);
                        self.others.insert(song);
                        self.log.push(format!("autoplay hands over {song}"));
                        let e = self.q.hand_over(song);
                        self.edits(e);
                    }
                }
                12 => {
                    let song = self.songs(1)[0];
                    self.allowed.insert(song);
                    self.others.insert(song);
                    self.log.push(format!("play autoplay song {song}"));
                    let e = self.q.play_other(song);
                    if self.hold.take().is_some() {
                        // It ends a station or following, and the queue comes back.
                        let _ = e;
                        let at = self.q.pos() + 1;
                        let e = self.q.restart_at(at);
                        self.edits(e);
                    } else {
                        self.edits(e);
                    }
                }
                13 | 14 => {
                    let n = 1 + self.rand(3);
                    let songs = self.songs(n);
                    self.allowed.extend(&songs);
                    let queued = self.rand(2) == 0;
                    let offset = self.rand(up_next + 2);
                    self.log.push(format!("insert {songs:?} at {offset} queued {queued}"));
                    let was_empty = self.q.is_empty();
                    let (e, at) = self.q.insert(songs.clone(), offset, queued);
                    self.edits(e);
                    if !was_empty && self.q.pos() < at {
                        let got: Vec<(u32, Kind)> = self.q.entries()[at..at + songs.len()].iter().map(|e| (e.song, e.kind)).collect();
                        let want: Vec<(u32, Kind)> = songs.iter().map(|s| (*s, if queued { Kind::Queued } else { Kind::List })).collect();
                        assert_eq!(got, want, "inserted songs land where asked");
                    }
                }
                15 => {
                    self.log.push("shuffle queue".into());
                    let seed = self.rand(1 << 30) as u64;
                    let e = self.q.shuffle_queued(seed);
                    self.edits(e);
                }
                16 => {
                    if !self.queued_undo.is_empty() && self.rand(2) == 0 {
                        self.log.push("undo clear queue".into());
                        let songs = std::mem::take(&mut self.queued_undo);
                        let (e, _) = self.q.insert(songs, 0, true);
                        self.edits(e);
                    } else {
                        self.log.push("clear queue".into());
                        let (e, songs) = self.q.clear_queued();
                        self.edits(e);
                        if !songs.is_empty() {
                            self.queued_undo = songs;
                        }
                    }
                }
                17 => {
                    if let Some(undo) = self.list_undo.take().filter(|_| self.rand(2) == 0) {
                        self.log.push("undo clear list".into());
                        let e = self.q.undo_clear_list(undo);
                        self.edits(e);
                    } else if let Some((e, undo)) = self.q.clear_list() {
                        self.log.push("clear list".into());
                        self.edits(e);
                        self.list_undo = Some(undo);
                    }
                }
                18 => {
                    let n = up_next + self.q.other_tail();
                    if n > 0 {
                        let k = 1 + self.rand(n.min(3));
                        let offsets: Vec<usize> = (0..k).map(|_| self.rand(n)).collect();
                        self.log.push(format!("remove {offsets:?}"));
                        let e = self.q.remove(&offsets);
                        self.edits(e);
                    }
                }
                19 | 20 => {
                    let n = up_next + self.q.other_tail();
                    if n > 0 {
                        let k = 1 + self.rand(n.min(3));
                        let mut offsets: Vec<usize> = (0..k).map(|_| self.rand(n)).collect();
                        offsets.sort_unstable();
                        offsets.dedup();
                        let before = self.rand(up_next + 1);
                        let into_queued = self.rand(2) == 0;
                        self.log.push(format!("drag {offsets:?} before {before} queued {into_queued}"));
                        // Dropped songs take the section they land in, so Autoplay's become the listener's.
                        let p = self.q.pos();
                        for o in &offsets {
                            if let Some(e) = self.q.entries().get(p + 1 + o) {
                                self.others.remove(&e.song);
                            }
                        }
                        let e = self.q.move_entries(&offsets, before, into_queued);
                        self.edits(e);
                        if !self.q.shuffle() {
                            self.origin = self.q.list.iter().map(|(i, _)| *i).collect();
                        }
                    }
                }
                21 => {
                    let mode = self.rand(3);
                    self.sim.repeat_all = mode == 1;
                    self.sim.repeat_one = mode == 2;
                    self.log.push(format!("repeat {mode}"));
                    if mode != 0 {
                        let e = self.q.drop_other_tail();
                        self.edits(e);
                    }
                }
                22 => {
                    // The app quits and starts again.
                    self.log.push("save and restore".into());
                    let saved = serde_json::to_string(&self.q).unwrap();
                    let back: PlayQueue<u32> = serde_json::from_str(&saved).unwrap();
                    let mut a = self.q.clone();
                    a.revision = 0;
                    assert_eq!(back, a, "a saved queue comes back the same");
                    self.q = back;
                    self.sim.entries.clear();
                    let e = self.q.load_edits();
                    self.edits(e);
                    self.pending_mix = self.q.mix_request();
                    self.list_undo = None;
                }
                24 | 25 => {
                    // A station plays in place of the queue, or someone is followed.
                    if !self.q.is_empty() {
                        let song = if op == 24 { None } else { Some(self.songs(1)[0]) };
                        self.log.push(format!("player holds {song:?}"));
                        if let Some(s) = song {
                            self.allowed.insert(s);
                            self.heard.insert(s);
                        }
                        self.sim.entries = vec![(song.unwrap_or(0), false)];
                        self.sim.audible = 0;
                        self.hold = Some(song);
                    }
                }
                26 => {
                    // Another device's queue, continued here.
                    let n = 1 + self.rand(10);
                    let songs = self.songs(n);
                    let at = self.rand(n);
                    self.log.push(format!("continue another device's queue of {n} at {at}"));
                    self.hold = None;
                    self.sim.wrapped = false;
                    self.allowed = songs.iter().copied().collect();
                    self.list_undo = None;
                    self.queued_undo.clear();
                    self.q = PlayQueue::from_queue(songs, at, 0..0, false);
                    self.q.window = 6;
                    self.q.feed_ahead = 3;
                    self.origin = self.q.list.iter().map(|(i, _)| *i).collect();
                    self.pending_mix = None;
                    let e = self.q.load_edits();
                    self.edits(e);
                }
                _ => {
                    let n = 1 + self.rand(4);
                    let songs = self.songs(n);
                    self.allowed.extend(&songs);
                    self.log.push(format!("radio adds {songs:?}"));
                    let (e, _) = self.q.append_to_list(songs);
                    self.edits(e);
                }
            }
            // Whatever plays now has been heard, since a restart plays without a track change here.
            if let Some(e) = self.q.current() {
                self.heard.insert(e.song);
                if e.kind == Kind::List {
                    self.cursor = Some(e.id);
                }
            }
            // Shuffle off from a mix while a queued or Autoplay song plays. The list carries on after the
            // list song that played last, including songs heard in the mix.
            if (op == 7 || op == 8)
                && mixed_before
                && !self.q.shuffle()
                && self.hold.is_none()
                && self.q.current().is_some_and(|c| c.kind != Kind::List)
                && let Some(k) = self.cursor.and_then(|id| self.q.own_index(id))
            {
                let next = self.q.entries()[self.q.pos() + 1..].iter().find(|e| e.kind == Kind::List).map(|e| e.id);
                assert_eq!(next, self.q.list.get(k + 1).map(|(i, _)| *i), "shuffle off goes on after the last list song played, {:?}", &self.log[self.log.len().saturating_sub(12)..]);
            }
            if keeps_list {
                assert_eq!(members(&self.q), list_before, "no list song lost or added, after {:?}", &self.log[self.log.len().saturating_sub(12)..]);
            }
            self.check(shuffle_step, mixed_now, &before_queued);
        }

        /// Next, Previous, a jump or the end while the player holds a station or a followed song.
        fn while_held(&mut self, op: usize) {
            let Some(hold) = self.hold else { return };
            match (op, hold) {
                (2..=5, None) => self.log.push("next/previous do nothing on a station".into()),
                (2..=4, Some(_)) => {
                    self.log.push("next while following: the own next song plays through them".into());
                    self.q.step_waiting();
                }
                (6, None) => {
                    // The queue view shows the playing song and what comes after it.
                    let n = self.q.entries().len() - self.q.pos();
                    let target = self.q.pos() + self.rand(n);
                    self.log.push(format!("station gives way to a click on {target}"));
                    self.hold = None;
                    let e = self.q.restart_at(target);
                    self.edits(e);
                }
                (6, Some(song)) => {
                    // Taking control keeps the followed song in front, then jumps.
                    self.log.push("taking control from following, then a jump".into());
                    self.hold = None;
                    let e = self.q.keep_in_front(song);
                    self.edits(e);
                    let n = self.q.up_next_len() + self.q.other_tail();
                    if n > 0 {
                        let target = self.q.pos() + 1 + self.rand(n);
                        let e = self.q.jump(target);
                        self.edits(e);
                    }
                }
                (22, _) => {
                    self.hold = None;
                    self.sim.entries.clear();
                    self.log.push("restart while held".into());
                    let saved = serde_json::to_string(&self.q).unwrap();
                    self.q = serde_json::from_str(&saved).unwrap();
                    let e = self.q.load_edits();
                    self.edits(e);
                    self.pending_mix = self.q.mix_request();
                    self.list_undo = None;
                }
                (_, None) => {
                    self.log.push("station ends".into());
                    self.hold = None;
                    let e = self.q.load_edits();
                    self.edits(e);
                }
                (_, Some(song)) => {
                    self.log.push("following ends".into());
                    self.hold = None;
                    let e = self.q.keep_in_front(song);
                    self.edits(e);
                }
            }
            if let Some(e) = self.q.current() {
                self.heard.insert(e.song);
                if e.kind == Kind::List {
                    self.cursor = Some(e.id);
                }
            }
            self.check(false, false, &[]);
        }

        fn check(&self, shuffle_step: bool, mixed_now: bool, before_queued: &[u32]) {
            let q = &self.q;
            let ctx = || format!("session {}, after {:?}\nqueue {:?} at {}", self.session, &self.log[self.log.len().saturating_sub(12)..], q.entries.iter().map(|e| (e.song, e.kind)).collect::<Vec<_>>(), q.pos);
            // The player holds what the model says, and plays the same entry.
            if self.hold.is_none() {
                let mine: Vec<(u32, bool)> = q.entries.iter().map(|e| (e.song, e.kind == Kind::Queued)).collect();
                assert_eq!(mine, self.sim.entries, "player and model agree, {}", ctx());
                if !q.is_empty() {
                    assert_eq!(q.pos, self.sim.audible, "same playing entry, {}", ctx());
                }
            }
            // 1. Nothing from an older list or queue.
            for s in q.entries.iter().map(|e| e.song).chain(q.list.iter().map(|(_, s)| *s)) {
                assert!(self.allowed.contains(&s), "song {s} is from an older queue, {}", ctx());
            }
            // Ids are unique, and the play order is exactly the list.
            let ids: HashSet<u64> = q.entries.iter().map(|e| e.id).collect();
            assert_eq!(ids.len(), q.entries.len(), "entry ids unique, {}", ctx());
            let mut own: Vec<u64> = q.list.iter().map(|(i, _)| *i).collect();
            let mut play = q.play.clone();
            own.sort_unstable();
            play.sort_unstable();
            assert_eq!(own, play, "3. the play order holds every list song once, {}", ctx());
            assert!(q.fed <= q.play.len());
            // The list's songs never change places except by an in-order drag.
            let place: HashMap<u64, usize> = self.origin.iter().enumerate().map(|(k, i)| (*i, k)).collect();
            let kept: Vec<usize> = q.list.iter().filter_map(|(i, _)| place.get(i).copied()).collect();
            assert!(kept.windows(2).all(|w| w[0] < w[1]), "the list's own order changed, {}", ctx());
            // 2. With shuffle off, the play order is the list's own order.
            if !q.shuffle {
                assert!(q.play_is_own(), "2. shuffle off plays the list's own order, {}", ctx());
                assert!(!q.mixing);
            }
            // The player's list entries are a run of the play order, ending where feeding got to.
            let held: Vec<u64> = q.entries.iter().filter(|e| e.kind == Kind::List).map(|e| e.id).collect();
            if !held.is_empty() {
                let ws = q.play_index(held[0]).expect("held list songs are in the list");
                assert_eq!(&q.play[ws..q.fed], &held[..], "the player's list songs are play[ws..fed], {}", ctx());
            }
            // Next from is the list in play order after the playing song.
            if !q.is_empty() && !q.mixing {
                let next = q.next_play();
                let upcoming: Vec<u64> = q.entries[q.pos + 1..].iter().filter(|e| e.kind == Kind::List).map(|e| e.id).collect();
                assert_eq!(&q.play[next..q.fed], &upcoming[..], "Next from is the list after the playing song, {}", ctx());
            }
            // 3. Right after a mix, Next from and the rest are the whole list but the playing song.
            if mixed_now {
                let cur = q.current().unwrap();
                if cur.kind == Kind::List {
                    assert_eq!(q.play[0], cur.id, "the playing song stays first, {}", ctx());
                }
                assert_eq!(q.next_play(), usize::from(cur.kind == Kind::List), "the whole list comes next, {}", ctx());
            }
            // 4. Shuffle leaves Next in Queue alone.
            if shuffle_step && !q.is_empty() {
                let run = q.queued_run();
                let now: Vec<u32> = q.entries[q.pos + 1..q.pos + 1 + run].iter().map(|e| e.song).collect();
                assert_eq!(now, before_queued, "4. shuffle doesn't touch Next in Queue, {}", ctx());
            }
            // 5. Autoplay's songs come after the list's and never join it.
            for (_, s) in &q.list {
                assert!(!self.others.contains(s), "5. an Autoplay song joined the list, {}", ctx());
            }
            if !q.is_empty() && !self.sim.wrapped {
                let after = &q.entries[q.pos + 1..];
                let last_own = after.iter().rposition(|e| e.kind != Kind::Other).map_or(0, |i| i + 1);
                assert!(after[..last_own].iter().all(|e| e.kind != Kind::Other || self.heard.contains(&e.song)), "5. Autoplay's songs come last, {}", ctx());
            }
        }
    }

    #[test]
    fn shuffle_while_autoplay_plays_after_a_restart_brings_nothing_back() {
        // After a restart with Autoplay's songs playing, Shuffle on must keep Autoplay's songs
        // and must not bring back an older queue.
        let mut q: PlayQueue<u32> = PlayQueue::new();
        let mut sim = Sim { entries: Vec::new(), audible: 0, repeat_all: false, repeat_one: false, wrapped: false };
        let go = |q: &mut PlayQueue<u32>, sim: &mut Sim, edits: Vec<Edit<u32>>| {
            if let Some((i, l)) = sim.apply(&edits) {
                let feed = q.track_changed(i, l);
                assert!(sim.apply(&feed).is_none());
            }
        };
        let e = q.start((0..5).collect(), 0, false);
        go(&mut q, &mut sim, e);
        let e = q.start((100..110).collect(), 0, false);
        go(&mut q, &mut sim, e);
        for _ in 0..9 {
            let (i, l) = sim.next().unwrap();
            let e = q.track_changed(i, l);
            go(&mut q, &mut sim, e);
        }
        let e = q.hand_over(500);
        go(&mut q, &mut sim, e);
        let (i, l) = sim.next().unwrap();
        let e = q.track_changed(i, l);
        go(&mut q, &mut sim, e);
        let e = q.hand_over(501);
        go(&mut q, &mut sim, e);
        assert_eq!(q.current().unwrap().song, 500);
        // Restart.
        let saved = serde_json::to_string(&q).unwrap();
        q = serde_json::from_str(&saved).unwrap();
        sim.entries.clear();
        let e = q.load_edits();
        go(&mut q, &mut sim, e);
        let before = q.songs();
        // Shuffle on while Autoplay plays changes nothing, since the list is over.
        let e = q.set_shuffle(true);
        assert!(e.is_empty() && !q.mixing());
        assert_eq!(q.songs(), before);
        assert_eq!(q.other_tail(), 1, "Autoplay's next song stays");
        assert!(q.songs().iter().all(|s| *s >= 100), "nothing from the older list");
        // Back on a list song, the mix is of that list only, and Autoplay's song stays last.
        let e = q.set_shuffle(false);
        go(&mut q, &mut sim, e);
        let (i, l) = sim.previous().unwrap();
        let e = q.track_changed(i, l);
        go(&mut q, &mut sim, e);
        assert_eq!(q.current().unwrap().song, 109);
        let e = q.set_shuffle(true);
        go(&mut q, &mut sim, e);
        let request = q.mix_request().unwrap();
        let order = queue_order::shuffled_whole_order(&vec![0.0; request.songs.len()], request.current.unwrap(), 0, queue_order::ShuffleStyle::Standard, 5);
        let e = q.apply_mix(&request, &order).unwrap();
        go(&mut q, &mut sim, e);
        let songs = q.songs();
        assert_eq!(songs[0], 109);
        let mut mixed = songs[1..10].to_vec();
        mixed.sort_unstable();
        assert_eq!(mixed, (100..109).collect::<Vec<u32>>());
        // Autoplay's songs still come after the list.
        assert_eq!(&songs[10..], &[500, 501]);
    }

    #[test]
    fn shuffle_off_during_a_queued_song_goes_on_after_the_last_list_song() {
        // The list cursor ignores queued and Autoplay songs.
        let mut q: PlayQueue<u32> = PlayQueue::new();
        let mut sim = Sim { entries: Vec::new(), audible: 0, repeat_all: false, repeat_one: false, wrapped: false };
        let go = |q: &mut PlayQueue<u32>, sim: &mut Sim, edits: Vec<Edit<u32>>| {
            if let Some((i, l)) = sim.apply(&edits) {
                let feed = q.track_changed(i, l);
                assert!(sim.apply(&feed).is_none());
            }
        };
        let next = |q: &mut PlayQueue<u32>, sim: &mut Sim| {
            let (i, l) = sim.next().unwrap();
            let e = q.track_changed(i, l);
            assert!(sim.apply(&e).is_none());
        };
        let up_list = |q: &PlayQueue<u32>| q.entries()[q.pos() + 1..].iter().filter(|e| e.kind == Kind::List).map(|e| e.song).collect::<Vec<_>>();
        // Shuffled, 0, 8 and 3 play, then a queued song.
        let e = q.start((0..10).collect(), 0, true);
        go(&mut q, &mut sim, e);
        let r = q.mix_request().unwrap();
        let e = q.apply_mix(&r, &[0, 8, 3, 1, 9, 2, 7, 4, 6, 5]).unwrap();
        go(&mut q, &mut sim, e);
        next(&mut q, &mut sim);
        next(&mut q, &mut sim);
        assert_eq!(q.current().unwrap().song, 3);
        let (e, _) = q.insert(vec![100], 0, true);
        go(&mut q, &mut sim, e);
        next(&mut q, &mut sim);
        assert_eq!(q.current().unwrap().song, 100);
        // Shuffle off resumes right after 3 in list order, and 8, heard in the mix, comes again.
        let e = q.set_shuffle(false);
        go(&mut q, &mut sim, e);
        assert_eq!(up_list(&q), vec![4, 5, 6, 7, 8, 9]);

        // In order, 0, 1 and 2 play, then a queued song starts and shuffle goes on and off while it plays.
        // The list goes on after 2, not after the queued song's place in the mix.
        let e = q.start((10..20).collect(), 0, false);
        go(&mut q, &mut sim, e);
        next(&mut q, &mut sim);
        next(&mut q, &mut sim);
        let (e, _) = q.insert(vec![200], 0, true);
        go(&mut q, &mut sim, e);
        next(&mut q, &mut sim);
        assert_eq!(q.current().unwrap().song, 200);
        q.set_shuffle(true);
        let r = q.mix_request().unwrap();
        let e = q.apply_mix(&r, &[19, 15, 11, 13, 10, 17, 12, 18, 14, 16].map(|s| s - 10)).unwrap();
        go(&mut q, &mut sim, e);
        assert_eq!(up_list(&q)[0], 19);
        let e = q.set_shuffle(false);
        go(&mut q, &mut sim, e);
        assert_eq!(up_list(&q), (13..20).collect::<Vec<u32>>());
        // A restart keeps the cursor.
        let saved = serde_json::to_string(&q).unwrap();
        let back: PlayQueue<u32> = serde_json::from_str(&saved).unwrap();
        assert_eq!(back.last_list, q.last_list);
    }

    #[test]
    fn long_random_sessions_keep_the_queue_whole() {
        // Seeded, so a failure replays. NAVIA_QUEUE_FUZZ=<n> runs more sessions.
        let sessions = std::env::var("NAVIA_QUEUE_FUZZ").ok().and_then(|v| v.parse().ok()).unwrap_or(300u64);
        // NAVIA_QUEUE_FUZZ_ONLY=<session> replays one session, printing every step.
        let only: Option<u64> = std::env::var("NAVIA_QUEUE_FUZZ_ONLY").ok().and_then(|v| v.parse().ok());
        for seed in only.map_or(0..sessions, |s| s..s + 1) {
            let mut f = Fuzz::new(seed.wrapping_mul(0x2545_f491_4f6c_dd1d) ^ 0xa5a5);
            f.session = seed;
            for _ in 0..400 {
                f.step();
                if only.is_some() {
                    eprintln!("{:<60} {:?} at {}", f.log.last().cloned().unwrap_or_default(), f.q.entries.iter().map(|e| (e.song, format!("{:?}", e.kind).chars().next().unwrap())).collect::<Vec<_>>(), f.q.pos);
                }
            }
        }
    }

    #[test]
    fn a_session_turn_takes_the_first_queued_song_of_the_waiting_queue() {
        let mut q = PlayQueue::new();
        q.start(vec![1, 2, 3], 0, false);
        q.insert(vec![8, 9], 0, true);
        assert_eq!(q.take_queued_next(), Some(8));
        assert_eq!((q.songs(), q.pos(), q.queued_run()), (vec![1, 9, 2, 3], 0, 1));
        assert_eq!(q.take_queued_next(), Some(9));
        assert_eq!(q.take_queued_next(), None, "the list's own songs aren't taken");
        assert_eq!(q.songs(), vec![1, 2, 3]);
    }
}
