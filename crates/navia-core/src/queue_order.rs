//! Pure orderings for a play queue or playlist, so every app moves and shuffles the same way.
//! An order is a list of old positions, `order[new] = old`.

/// Moves the items at `positions`, in their current order, before the item now at `before`.
/// Returns where the first moved item landed, or `None` when nothing would move.
pub fn move_items<T>(items: &mut Vec<T>, positions: &[usize], before: usize) -> Option<usize> {
    let order = moved_order(items.len(), positions, before)?;
    let mut old: Vec<Option<T>> = std::mem::take(items).into_iter().map(Some).collect();
    items.extend(order.iter().map(|&i| old[i].take().expect("each position once")));
    landed(positions, before, old.len())
}

/// The order [`move_items`] gives, or `None` when nothing would move.
/// Positions past the end are ignored and repeats count once.
pub fn moved_order(len: usize, positions: &[usize], before: usize) -> Option<Vec<usize>> {
    let picked = picked(len, positions);
    if picked.is_empty() {
        return None;
    }
    let before = before.min(len);
    let insert = before - picked.iter().filter(|&&i| i < before).count();
    let mut rest: Vec<usize> = (0..len).filter(|i| picked.binary_search(i).is_err()).collect();
    if picked.iter().enumerate().all(|(k, &i)| i == insert + k) {
        return None;
    }
    rest.splice(insert..insert, picked);
    Some(rest)
}

/// Where the first moved item lands for these arguments, as [`move_items`] reports it.
fn landed(positions: &[usize], before: usize, len: usize) -> Option<usize> {
    let picked = picked(len, positions);
    let before = before.min(len);
    Some(before - picked.iter().filter(|&&i| i < before).count())
}

fn picked(len: usize, positions: &[usize]) -> Vec<usize> {
    let mut picked: Vec<usize> = positions.iter().copied().filter(|&i| i < len).collect();
    picked.sort_unstable();
    picked.dedup();
    picked
}

/// An order that shuffles everything after `current` and keeps the rest in place.
/// `None` shuffles the whole list.
pub fn shuffled_order(len: usize, current: Option<usize>, seed: u64) -> Vec<usize> {
    let keep = current.map_or(0, |c| (c + 1).min(len));
    let mut order: Vec<usize> = (0..len).collect();
    let mut rng = SplitMix(seed);
    let tail = &mut order[keep..];
    for i in (1..tail.len()).rev() {
        let j = (rng.next() % (i as u64 + 1)) as usize;
        tail.swap(i, j);
    }
    order
}

/// A seed for [`shuffled_order`] that differs every time.
pub fn random_seed() -> u64 {
    rand::random::<u64>()
}

/// Where songs added to the queue go. Play Next goes right after the current song, and Add to Queue
/// goes after the queued songs already waiting.
pub fn queue_insert_at(queued: &[bool], current: Option<usize>, next: bool) -> usize {
    let start = current.map_or(0, |c| c + 1).min(queued.len());
    if next {
        return start;
    }
    start + queued_run(queued, current)
}

/// How many queued songs wait right after the current one.
pub fn queued_run(queued: &[bool], current: Option<usize>) -> usize {
    let start = current.map_or(0, |c| c + 1).min(queued.len());
    queued[start..].iter().take_while(|q| **q).count()
}

/// What the queue is playing from, for the "Next from: <source>" heading.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct QueueSource {
    pub name: String,
    pub radio: bool,
    /// The page the heading links to, if it has one.
    pub link: Option<SourceLink>,
}

/// A page a [`QueueSource`] links to, by server id or genre name.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SourceLink {
    Album(String),
    Playlist(String),
    Artist(String),
    /// an artist's Top Songs page, by artist id.
    ArtistTop(String),
    Genre(String),
    Mix(String),
    Liked,
    Songs,
}

/// What playing an upcoming queue entry does to the queue.
/// Apply `dropped` or else `moved`, then play the entry at `at`. Positions are indexes before the change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueueJump {
    /// Queued songs the jump passes over. They leave the queue, so Previous never walks back into them.
    pub dropped: Vec<usize>,
    /// Queued songs that stay next when the target is further down. They move to land right after the target.
    pub moved: Vec<usize>,
    pub before: usize,
    /// Where the target plays once that's done.
    pub at: usize,
}

/// The [`QueueJump`] for playing `target` while `current` plays.
pub fn queue_jump(queued: &[bool], current: usize, target: usize) -> QueueJump {
    let run = queued_run(queued, Some(current));
    let first = current + 1;
    let plain = QueueJump { dropped: Vec::new(), moved: Vec::new(), before: target, at: target };
    if target <= first || target >= queued.len() || run == 0 {
        return plain;
    }
    if target < first + run {
        return QueueJump { dropped: (first..target).collect(), at: first, ..plain };
    }
    QueueJump { moved: (first..first + run).collect(), before: target + 1, at: target - run, ..plain }
}

/// The queue's order after a [`QueueJump`], with dropped positions left out.
pub fn queue_jump_order(len: usize, jump: &QueueJump) -> Vec<usize> {
    if !jump.dropped.is_empty() {
        return (0..len).filter(|i| !jump.dropped.contains(i)).collect();
    }
    moved_order(len, &jump.moved, jump.before).unwrap_or_else(|| (0..len).collect())
}

/// The play order for starting a new list while songs wait in Next in Queue.
/// Takes `list` followed by the `kept` queued songs. The chosen song is at `start` and the queued songs follow it.
pub fn start_keeping_queued(list_len: usize, start: usize, kept: usize) -> Vec<usize> {
    let start = start.min(list_len.saturating_sub(1));
    let after = if list_len == 0 { 0 } else { start + 1 };
    (0..after).chain(list_len..list_len + kept).chain(after..list_len).collect()
}

/// Shuffle Queue, which mixes the queued songs after `current` once, never into their current order.
/// Returns `None` with fewer than two queued songs. The same seed gives the same order.
pub fn shuffled_queued_order(queued: &[bool], current: Option<usize>, seed: u64) -> Option<Vec<usize>> {
    let run = queued_run(queued, current);
    if run < 2 {
        return None;
    }
    let first = current.map_or(0, |c| c + 1);
    let mut rng = SplitMix(seed);
    let mut mixed: Vec<usize> = (first..first + run).collect();
    while mixed.iter().enumerate().all(|(k, &i)| i == first + k) {
        for i in (1..mixed.len()).rev() {
            let j = (rng.next() % (i as u64 + 1)) as usize;
            mixed.swap(i, j);
        }
    }
    Some((0..first).chain(mixed).chain(first + run..queued.len()).collect())
}

/// Settings > Playback > Shuffle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ShuffleStyle {
    /// Plays the freshest of several random orders, so recently heard songs come later.
    #[default]
    FewerRepeats,
    /// Plain random.
    Standard,
}

/// How many random orders Fewer Repeats draws. Bounded so shuffling stays fast on any list size.
pub const SHUFFLE_CANDIDATES: usize = 8;
pub const SHUFFLE_SCORED: usize = 200;
/// Days after which a last play counts half as recent.
pub const RECENT_HALF_DAYS: f64 = 14.0;
const DAY_MS: f64 = 86_400_000.0;

/// How recently a song was heard, from 1 for this session toward 0.
/// The server's last-played date fades with age. Without one, only this session's plays count.
pub fn recency(heard_this_session: bool, played_ms: Option<u64>, now_ms: u64) -> f32 {
    if heard_this_session {
        return 1.0;
    }
    match played_ms {
        Some(at) => (1.0 / (1.0 + now_ms.saturating_sub(at) as f64 / DAY_MS / RECENT_HALF_DAYS)) as f32,
        None => 0.0,
    }
}

/// A shuffled order for items with the given recency.
/// Fewer Repeats keeps the freshest of [`SHUFFLE_CANDIDATES`] random orders, penalizing recent songs that play early.
pub fn mixed_order(recency: &[f32], style: ShuffleStyle, seed: u64) -> Vec<usize> {
    let mut rng = SplitMix(seed);
    let draw = |rng: &mut SplitMix| {
        let mut order: Vec<usize> = (0..recency.len()).collect();
        for i in (1..order.len()).rev() {
            let j = (rng.next() % (i as u64 + 1)) as usize;
            order.swap(i, j);
        }
        order
    };
    if style == ShuffleStyle::Standard || !recency.iter().any(|r| *r > 0.0) {
        return draw(&mut rng);
    }
    let scored = recency.len().min(SHUFFLE_SCORED);
    let score = |order: &[usize]| -> f64 {
        (0..scored).map(|i| f64::from(recency[order[i]]) * (1.0 - i as f64 / scored as f64).powi(2)).sum()
    };
    let mut best = draw(&mut rng);
    let mut best_score = score(&best);
    for _ in 1..SHUFFLE_CANDIDATES {
        let order = draw(&mut rng);
        let s = score(&order);
        if s < best_score {
            best = order;
            best_score = s;
        }
    }
    best
}

/// The play order for turning shuffle on. Everything but `current` is mixed, earlier songs included.
/// The `queued` songs right after it stay next, and the current song plays at 0.
pub fn shuffled_whole_order(recency: &[f32], current: usize, queued: usize, style: ShuffleStyle, seed: u64) -> Vec<usize> {
    let len = recency.len();
    if len == 0 {
        return Vec::new();
    }
    let current = current.min(len - 1);
    let keep_end = (current + 1 + queued).min(len);
    let rest: Vec<usize> = (0..current).chain(keep_end..len).collect();
    let weights: Vec<f32> = rest.iter().map(|&i| recency[i]).collect();
    let mixed = mixed_order(&weights, style, seed);
    std::iter::once(current).chain(current + 1..keep_end).chain(mixed.into_iter().map(|k| rest[k])).collect()
}

/// The range Clear <source> removes, after the queued songs and before Autoplay's at `end`.
pub fn list_after_queued(queued: &[bool], current: Option<usize>, end: usize) -> std::ops::Range<usize> {
    let first = current.map_or(0, |c| c + 1).min(queued.len());
    let start = first + queued_run(queued, current);
    start..end.min(queued.len()).max(start)
}

/// A drag within the queue's sections, moving the entries at `positions` before `before`.
/// Moved songs take the section they land in. Returns the order, or `None` if nothing moves, and the new flags.
pub fn section_move(queued: &[bool], current: usize, positions: &[usize], before: usize, into_queued: bool) -> (Option<Vec<usize>>, Vec<bool>) {
    let len = queued.len();
    let first = (current + 1).min(len);
    let run = queued_run(queued, Some(current));
    let positions: Vec<usize> = positions.iter().copied().filter(|&p| p >= first && p < len).collect();
    let before = before.clamp(first, len);
    let at = if into_queued { before.min(first + run) } else { before.max(first + run) };
    let mut flags = queued.to_vec();
    for &p in &positions {
        flags[p] = into_queued;
    }
    // Entries before the playing one keep their flags, so a song played earlier never counts.
    let order = moved_order(len, &positions, at);
    let flags = order.as_ref().map_or(flags.clone(), |o| apply_order(&flags, o));
    (order, flags)
}

/// Puts `items` in `order`. Positions not in the order are dropped.
pub fn apply_order<T: Clone>(items: &[T], order: &[usize]) -> Vec<T> {
    order.iter().filter_map(|&i| items.get(i).cloned()).collect()
}

/// Splitmix64, small and fast and good enough to shuffle a queue.
struct SplitMix(u64);

impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn moved(len: usize, positions: &[usize], before: usize) -> Vec<usize> {
        let mut v: Vec<usize> = (0..len).collect();
        move_items(&mut v, positions, before);
        v
    }

    #[test]
    fn moves_match_the_reference() {
        // One row down, one row up, to the end and to the start.
        assert_eq!(moved(5, &[1], 4), vec![0, 2, 3, 1, 4]);
        assert_eq!(moved(5, &[3], 1), vec![0, 3, 1, 2, 4]);
        assert_eq!(moved(5, &[0], 5), vec![1, 2, 3, 4, 0]);
        assert_eq!(moved(5, &[4], 0), vec![4, 0, 1, 2, 3]);
        // Several rows, scattered, keep their order and land together.
        assert_eq!(moved(7, &[5, 1, 3], 0), vec![1, 3, 5, 0, 2, 4, 6]);
        assert_eq!(moved(7, &[1, 3, 5], 7), vec![0, 2, 4, 6, 1, 3, 5]);
        assert_eq!(moved(7, &[1, 3, 5], 3), vec![0, 2, 1, 3, 5, 4, 6]);
    }

    #[test]
    fn a_move_that_changes_nothing_reports_none() {
        let mut v = vec![0, 1, 2, 3];
        assert_eq!(move_items(&mut v, &[1], 1), None);
        assert_eq!(move_items(&mut v, &[1], 2), None, "before the next row is where it is");
        assert_eq!(move_items(&mut v, &[1, 2], 3), None);
        assert_eq!(move_items(&mut v, &[], 0), None);
        assert_eq!(move_items(&mut v, &[9], 0), None, "past the end is ignored");
        assert_eq!(v, vec![0, 1, 2, 3]);
        assert_eq!(move_items(&mut v, &[2, 2], 0), Some(0), "a repeat counts once");
        assert_eq!(v, vec![2, 0, 1, 3]);
    }

    #[test]
    fn landing_place_is_reported() {
        let mut v: Vec<usize> = (0..6).collect();
        assert_eq!(move_items(&mut v, &[0, 1], 5), Some(3));
        assert_eq!(v, vec![2, 3, 4, 0, 1, 5]);
    }

    #[test]
    fn shuffle_keeps_the_played_part_and_the_current_song() {
        for seed in 0..50 {
            let order = shuffled_order(40, Some(9), seed);
            assert_eq!(&order[..10], &(0..10).collect::<Vec<_>>()[..], "seed {seed}");
            let mut rest = order[10..].to_vec();
            rest.sort_unstable();
            assert_eq!(rest, (10..40).collect::<Vec<_>>(), "every later song once");
        }
        let moved = (0..50).filter(|&s| shuffled_order(40, Some(9), s)[10..] != (10..40).collect::<Vec<_>>()[..]).count();
        assert_eq!(moved, 50, "the rest really is shuffled");
        assert_eq!(shuffled_order(1, Some(0), 3), vec![0]);
        assert_eq!(shuffled_order(0, None, 3), Vec::<usize>::new());
        assert_eq!(shuffled_order(3, Some(7), 3), vec![0, 1, 2], "a current past the end keeps all");
    }

    #[test]
    fn queued_songs_play_before_the_rest_of_the_list() {
        // Playing 0, with 1 and 2 queued and 3.. the list.
        let queued = [false, true, true, false, false];
        assert_eq!(queue_insert_at(&queued, Some(0), true), 1, "Play Next goes first");
        assert_eq!(queue_insert_at(&queued, Some(0), false), 3, "Add to Queue goes after earlier queued songs");
        assert_eq!(queue_insert_at(&[false, false], Some(0), false), 1, "nothing queued: right after the current song");
        assert_eq!(queue_insert_at(&[], None, false), 0);
        assert_eq!(queue_insert_at(&[true, false], None, false), 1);
    }

    #[test]
    fn play_order_is_queued_then_the_list_then_autoplay() {
        use crate::audio::player::Repeat;
        use crate::autoplay::starts_now;
        // A list of 4 playing its first song, shuffled, then two songs queued.
        let mut entries: Vec<(&str, bool)> = vec![("l0", false), ("l1", false), ("l2", false), ("l3", false)];
        let flags = |e: &[(&str, bool)]| e.iter().map(|x| x.1).collect::<Vec<_>>();
        let order = shuffled_whole_order(&vec![0.0; entries.len()], 0, queued_run(&flags(&entries), Some(0)), ShuffleStyle::Standard, 3);
        entries = apply_order(&entries, &order);
        let at = queue_insert_at(&flags(&entries), Some(0), false);
        entries.insert(at, ("q1", true));
        let at = queue_insert_at(&flags(&entries), Some(0), false);
        entries.insert(at, ("q2", true));
        assert_eq!(entries[1].0, "q1");
        assert_eq!(entries[2].0, "q2");
        // Autoplay waits for every song of the shuffled list, then takes over.
        for current in 0..entries.len() {
            let left = entries.len() - current - 1;
            assert_eq!(starts_now(true, Repeat::Off, false, left), current == entries.len() - 1);
            assert!(!starts_now(true, Repeat::All, false, left));
        }
    }

    #[test]
    fn jumping_down_the_list_keeps_queued_songs_next() {
        // Playing 0, queued 1 and 2, then the list 3..6. Jump to 5.
        let queued = [false, true, true, false, false, false, false];
        let jump = queue_jump(&queued, 0, 5);
        assert_eq!(jump, QueueJump { dropped: vec![], moved: vec![1, 2], before: 6, at: 3 });
        let order = queue_jump_order(queued.len(), &jump);
        assert_eq!(order, vec![0, 3, 4, 5, 1, 2, 6]);
        assert_eq!(apply_order(&queued, &order)[jump.at + 1..jump.at + 3], [true, true]);
        // With none waiting, or only queued songs that already played, it plays where it is.
        assert_eq!(queue_jump(&[false; 5], 0, 3).at, 3);
        assert_eq!(queue_jump(&[true, false, false, false], 1, 3), QueueJump { dropped: vec![], moved: vec![], before: 3, at: 3 });
    }

    #[test]
    fn jumping_into_next_in_queue_drops_the_queued_songs_above() {
        // Playing 0, queued 1, 2 and 3, then the list. Play the third queued song.
        let queued = [false, true, true, true, false, false];
        let jump = queue_jump(&queued, 0, 3);
        assert_eq!(jump, QueueJump { dropped: vec![1, 2], moved: vec![], before: 3, at: 1 });
        let order = queue_jump_order(queued.len(), &jump);
        assert_eq!(order, vec![0, 3, 4, 5]);
        assert_eq!(order[jump.at], 3, "the target plays right after the current song");
        // The first queued song is next anyway, so nothing changes.
        assert_eq!(queue_jump(&queued, 0, 1), QueueJump { dropped: vec![], moved: vec![], before: 1, at: 1 });
        // For the last song of the queue, everything queued above it goes.
        let jump = queue_jump(&[false, true, true], 0, 2);
        assert_eq!((jump.dropped, jump.at), (vec![1], 1));
        // Behind the current song, past the end or in an empty queue, it plays where it is.
        assert_eq!(queue_jump(&queued, 3, 0).at, 0);
        assert_eq!(queue_jump(&queued, 0, 9).at, 9);
        assert_eq!(queue_jump(&[], 0, 0).at, 0);
    }

    #[test]
    fn a_new_list_keeps_the_queued_songs_next() {
        // A new list of 5 started at its third song, with two songs waiting in Next in Queue.
        let list = ["l0", "l1", "l2", "l3", "l4"];
        let kept = ["q1", "q2"];
        let all: Vec<&str> = list.iter().chain(kept.iter()).copied().collect();
        let order = start_keeping_queued(list.len(), 2, kept.len());
        assert_eq!(apply_order(&all, &order), ["l0", "l1", "l2", "q1", "q2", "l3", "l4"]);
        let flags: Vec<bool> = order.iter().map(|&i| i >= list.len()).collect();
        assert_eq!(queued_run(&flags, Some(2)), 2, "they're still Next in Queue");
        // Nothing queued keeps the list as it is. At the last song, the queued songs follow it.
        assert_eq!(start_keeping_queued(3, 1, 0), vec![0, 1, 2]);
        assert_eq!(start_keeping_queued(3, 2, 1), vec![0, 1, 2, 3]);
        // With shuffle on, the chosen song comes first and the queued songs still follow it, outside the shuffle.
        let shuffled = apply_order(&list, &shuffled_order(list.len(), None, 5));
        let all: Vec<&str> = shuffled.iter().chain(kept.iter()).copied().collect();
        let order = start_keeping_queued(shuffled.len(), 0, kept.len());
        assert_eq!(&apply_order(&all, &order)[1..3], &["q1", "q2"]);
        // A single song, like a Search song before its radio arrives.
        assert_eq!(start_keeping_queued(1, 0, 2), vec![0, 1, 2]);
        assert_eq!(start_keeping_queued(0, 0, 2), vec![0, 1]);
    }

    #[test]
    fn shuffle_queue_mixes_only_the_queued_songs_once() {
        // Playing 1, with 2..5 queued, then the list.
        let queued = [false, false, true, true, true, true, false, false];
        for seed in 0..40 {
            let order = shuffled_queued_order(&queued, Some(1), seed).unwrap();
            assert_eq!(&order[..2], &[0, 1]);
            assert_eq!(&order[6..], &[6, 7], "the list stays");
            assert_ne!(&order[2..6], &[2, 3, 4, 5], "never the same order");
            let mut mixed = order[2..6].to_vec();
            mixed.sort_unstable();
            assert_eq!(mixed, vec![2, 3, 4, 5]);
            assert_eq!(order, shuffled_queued_order(&queued, Some(1), seed).unwrap(), "same seed, same order");
        }
        // Two queued songs can only swap.
        assert_eq!(shuffled_queued_order(&[false, true, true, false], Some(0), 3), Some(vec![0, 2, 1, 3]));
        // Under two queued songs, or an empty queue, gives nothing.
        assert_eq!(shuffled_queued_order(&[false, true, false], Some(0), 3), None);
        assert_eq!(shuffled_queued_order(&[], None, 3), None);
    }

    #[test]
    fn clearing_the_list_keeps_the_queued_songs_and_autoplay() {
        // Playing 0, with 1 and 2 queued, 3..6 the list and 6 Autoplay's.
        let queued = [false, true, true, false, false, false, false];
        assert_eq!(list_after_queued(&queued, Some(0), 6), 3..6);
        assert_eq!(list_after_queued(&queued, Some(0), queued.len()), 3..7);
        // Nothing queued clears everything after the current song. Nothing after gives an empty range.
        assert_eq!(list_after_queued(&[false, false, false], Some(0), 3), 1..3);
        assert!(list_after_queued(&[false, true], Some(0), 2).is_empty());
        assert!(list_after_queued(&[false], Some(0), 1).is_empty());
        assert!(list_after_queued(&[], None, 0).is_empty());
    }

    #[test]
    fn recency_fades_with_the_last_play() {
        let now = 100 * 86_400_000;
        assert_eq!(recency(true, None, now), 1.0, "this session counts most");
        assert_eq!(recency(false, None, now), 0.0, "never played");
        assert_eq!(recency(false, Some(now), now), 1.0);
        assert!((recency(false, Some(now - 14 * 86_400_000), now) - 0.5).abs() < 1e-6, "half after 14 days");
        assert!(recency(false, Some(now - 60 * 86_400_000), now) < recency(false, Some(now - 30 * 86_400_000), now));
        assert_eq!(recency(false, Some(now + 5), now), 1.0, "a clock ahead counts as now");
    }

    #[test]
    fn fewer_repeats_puts_recent_songs_later_and_drops_none() {
        // 40 songs, the first 10 heard this session.
        let rec: Vec<f32> = (0..40).map(|i| if i < 10 { 1.0 } else { 0.0 }).collect();
        let mean_place = |style| {
            let mut total = 0.0;
            for seed in 0..200 {
                let order = mixed_order(&rec, style, seed);
                let mut sorted = order.clone();
                sorted.sort_unstable();
                assert_eq!(sorted, (0..40).collect::<Vec<_>>(), "every song once");
                total += order.iter().enumerate().filter(|(_, i)| **i < 10).map(|(p, _)| p as f64).sum::<f64>() / 10.0;
            }
            total / 200.0
        };
        let (fewer, standard) = (mean_place(ShuffleStyle::FewerRepeats), mean_place(ShuffleStyle::Standard));
        assert!((standard - 19.5).abs() < 1.5, "standard is plain random: {standard}");
        assert!(fewer > standard + 3.0, "heard songs come later with Fewer Repeats: {fewer} vs {standard}");
    }

    #[test]
    fn standard_and_nothing_recent_are_plain_random() {
        let none = vec![0.0; 30];
        let some: Vec<f32> = (0..30).map(|i| i as f32 / 30.0).collect();
        for seed in 0..20 {
            // With nothing recent, Fewer Repeats draws exactly the order Standard does.
            assert_eq!(mixed_order(&none, ShuffleStyle::FewerRepeats, seed), mixed_order(&none, ShuffleStyle::Standard, seed));
            // Standard ignores recency.
            assert_eq!(mixed_order(&some, ShuffleStyle::Standard, seed), mixed_order(&none, ShuffleStyle::Standard, seed));
        }
        assert_eq!(mixed_order(&[], ShuffleStyle::FewerRepeats, 1), Vec::<usize>::new());
        assert_eq!(mixed_order(&[1.0], ShuffleStyle::FewerRepeats, 1), vec![0]);
    }

    #[test]
    fn whole_list_shuffle_keeps_the_current_song_and_queued_songs_first() {
        // Playing 4 of 10, with 5 and 6 queued.
        let rec = vec![0.0; 10];
        for seed in 0..30 {
            let order = shuffled_whole_order(&rec, 4, 2, ShuffleStyle::FewerRepeats, seed);
            assert_eq!(&order[..3], &[4, 5, 6]);
            let mut rest = order[3..].to_vec();
            rest.sort_unstable();
            assert_eq!(rest, vec![0, 1, 2, 3, 7, 8, 9], "songs before the current one come back into the mix");
        }
        assert_eq!(shuffled_whole_order(&[0.0], 0, 0, ShuffleStyle::Standard, 3), vec![0]);
        assert_eq!(shuffled_whole_order(&[], 0, 0, ShuffleStyle::Standard, 3), Vec::<usize>::new());
        assert_eq!(shuffled_whole_order(&[0.0; 3], 2, 5, ShuffleStyle::Standard, 3)[0], 2, "queued past the end is clamped");
    }

    #[test]
    fn a_huge_list_shuffles_fast() {
        let len = 200_000;
        let rec: Vec<f32> = (0..len).map(|i| if i % 7 == 0 { 0.8 } else { 0.0 }).collect();
        let start = std::time::Instant::now();
        let order = shuffled_whole_order(&rec, 1234, 3, ShuffleStyle::FewerRepeats, 99);
        let took = start.elapsed();
        assert_eq!(order.len(), len);
        let mut seen = vec![false; len];
        for i in &order {
            assert!(!seen[*i], "song {i} twice");
            seen[*i] = true;
        }
        // Debug build on a shared machine, so generous, but it catches anything quadratic.
        assert!(took < std::time::Duration::from_secs(2), "took {took:?}");
    }

    #[test]
    fn section_moves_take_the_section_they_land_in() {
        // Playing 0, queued 1 and 2, then 3..5 from the list.
        let queued = [false, true, true, false, false, false];
        // A list song dragged into Next in Queue, between the queued ones.
        let (order, flags) = section_move(&queued, 0, &[4], 2, true);
        assert_eq!(order.unwrap(), vec![0, 1, 4, 2, 3, 5]);
        assert_eq!(flags, vec![false, true, true, true, false, false]);
        // A queued song dragged below the line becomes part of the list.
        let (order, flags) = section_move(&queued, 0, &[1], 5, false);
        assert_eq!(order.unwrap(), vec![0, 2, 3, 4, 1, 5]);
        assert_eq!(flags, vec![false, true, false, false, false, false]);
        assert_eq!(queued_run(&flags, Some(0)), 1);
        // Dropped into Next from above its start, it lands at the start of Next from.
        let (order, flags) = section_move(&queued, 0, &[1], 1, false);
        assert_eq!(order.unwrap(), vec![0, 2, 1, 3, 4, 5]);
        assert_eq!(flags, vec![false, true, false, false, false, false]);
        // Dropped into Next in Queue below its end, it lands at its end.
        let (order, flags) = section_move(&queued, 0, &[5], 4, true);
        assert_eq!(order.unwrap(), vec![0, 1, 2, 5, 3, 4]);
        assert_eq!(queued_run(&flags, Some(0)), 3);
        // A queued song dropped in place in its own section doesn't move.
        let (order, flags) = section_move(&queued, 0, &[2], 3, true);
        assert!(order.is_none());
        assert_eq!(flags, queued.to_vec());
        // Into an empty Next in Queue right after the playing song, only the flag changes.
        let (order, flags) = section_move(&[false, false, false], 0, &[1], 1, true);
        assert!(order.is_none());
        assert_eq!(flags, vec![false, true, false]);
    }
}
