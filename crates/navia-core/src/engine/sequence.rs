//! Artist and album spacing for Autoplay's plan. The songs already in the queue count too.
//!
//! When nothing fits, as in a one-artist library, the rules relax in steps so Autoplay never stops.

use crate::client::Song;
use crate::recommend::artist_key;

/// Another artist not again within this many songs.
pub const ARTIST_GAP: usize = 2;
/// at most [`ARTIST_MAX`] songs of one artist in any run of this many ([`respace`])
pub const ARTIST_WINDOW: usize = 10;
/// a plan's run: the songs the queue shows
pub const SPREAD_WINDOW: usize = crate::autoplay::SHOWN;
pub const ARTIST_MAX: usize = 2;
/// No album again within this many songs.
pub const ALBUM_GAP: usize = 3;
/// the seed artist: at most [`SEED_FIRST10`] in the first this many picks...
pub const SEED_SPAN: usize = 10;
pub const SEED_FIRST10: usize = 2;
/// ...and [`SEED_WINDOW`] in any run of [`SEED_SPAN`] after
pub const SEED_WINDOW: usize = 3;

/// What spacing looks at.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub artist: String,
    pub album: String,
}

impl Item {
    pub fn of(song: &Song) -> Self {
        let album = song.album_id.clone().filter(|a| !a.is_empty()).or_else(|| song.album.as_ref().map(|a| a.to_lowercase())).unwrap_or_default();
        Self { artist: artist_key(song), album }
    }
}

/// how strictly a song must fit, from every rule to none
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Strict {
    All,
    /// a third song of an artist in the window, never back to back, and the seed artist's caps
    Third,
    /// any number of an artist's songs, never back to back, and the seed artist's caps
    Gap,
    /// only the seed artist's caps.
    Seed,
    Any,
}

/// Spacing state: the songs before, those placed, and which artists are the seed's.
pub struct Spacer<'a> {
    recent: Vec<Item>,
    /// Songs placed, not counting the history.
    placed: usize,
    /// the run of songs another artist's cap counts in
    window: usize,
    seed_artists: &'a [String],
    seed_placed: Vec<bool>,
}

impl Strict {
    /// each level in the order the rules give way
    pub const LOOSENING: [Strict; 5] = [Strict::All, Strict::Third, Strict::Gap, Strict::Seed, Strict::Any];
}

impl<'a> Spacer<'a> {
    pub fn new(history: &[Item], seed_artists: &'a [String], window: usize) -> Self {
        Self { recent: history.to_vec(), placed: 0, window, seed_artists, seed_placed: Vec::new() }
    }

    fn is_seed(&self, artist: &str) -> bool {
        !artist.is_empty() && self.seed_artists.iter().any(|a| a == artist)
    }

    /// Whether `item` may come next.
    pub fn fits(&self, item: &Item, strict: Strict) -> bool {
        if strict == Strict::Any {
            return true;
        }
        let artist = !item.artist.is_empty();
        let last = self.recent.last();
        if strict < Strict::Seed && artist && last.is_some_and(|r| r.artist == item.artist) {
            return false;
        }
        if self.is_seed(&item.artist) {
            let seeds_in = |n: usize| self.seed_placed.iter().rev().take(n).filter(|s| **s).count();
            let (cap, window) = if self.placed < SEED_SPAN { (SEED_FIRST10, SEED_SPAN) } else { (SEED_WINDOW, SEED_SPAN - 1) };
            return seeds_in(window) < cap;
        }
        if strict >= Strict::Gap {
            return true;
        }
        let tail = |n: usize| self.recent.iter().rev().take(n);
        if strict == Strict::All && artist && tail(ARTIST_GAP).any(|r| r.artist == item.artist) {
            return false;
        }
        let cap = if strict == Strict::All { ARTIST_MAX } else { ARTIST_MAX + 1 };
        if artist && self.in_window(item) >= cap {
            return false;
        }
        !(strict == Strict::All && !item.album.is_empty() && tail(ALBUM_GAP).any(|r| r.album == item.album))
    }

    /// how many of `item`'s artist's songs the window holds among those placed
    pub fn in_window(&self, item: &Item) -> usize {
        self.recent.iter().rev().take(self.placed.min(self.window - 1)).filter(|r| r.artist == item.artist).count()
    }

    pub fn place(&mut self, item: &Item) {
        self.seed_placed.push(self.is_seed(&item.artist));
        self.recent.push(item.clone());
        self.placed += 1;
    }
}

/// Indexes of up to `limit` of `items`, each the first that fits after `history` and those
/// taken, ignoring seed-artist rules.
pub fn respace(items: &[Item], history: &[Item], limit: usize) -> Vec<usize> {
    let mut used = vec![false; items.len()];
    let mut spacer = Spacer::new(history, &[], ARTIST_WINDOW);
    let mut out = Vec::new();
    while out.len() < limit.min(items.len()) {
        let pick = Strict::LOOSENING.into_iter().find_map(|strict| (0..items.len()).find(|&i| !used[i] && spacer.fits(&items[i], strict))).expect("an unused item");
        used[pick] = true;
        spacer.place(&items[pick]);
        out.push(pick);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(artist: &str, album: &str) -> Item {
        Item { artist: artist.into(), album: album.into() }
    }

    #[test]
    fn other_artists_two_apart_twice_per_ten_albums_three_apart() {
        let items: Vec<Item> = (0..6).map(|i| item("A", &format!("A{i}"))).chain((0..20).map(|i| item(&format!("O{i}"), &format!("L{i}")))).collect();
        let order = respace(&items, &[], 20);
        let artists: Vec<&str> = order.iter().map(|&i| items[i].artist.as_str()).collect();
        for (i, a) in artists.iter().enumerate() {
            assert!(!artists[i.saturating_sub(ARTIST_GAP)..i].contains(a), "{artists:?}");
            let window = &artists[i.saturating_sub(ARTIST_WINDOW - 1)..=i];
            assert!(window.iter().filter(|x| *x == a).count() <= ARTIST_MAX, "{artists:?}");
        }
        let same_album = vec![item("A", "X"), item("B", "X"), item("C", "Y"), item("D", "Z"), item("E", "W")];
        assert_eq!(respace(&same_album, &[], 5), [0, 2, 3, 4, 1]);
    }

    #[test]
    fn the_seed_artist_is_allowed_but_capped_and_never_back_to_back() {
        let seed = vec!["S".to_string()];
        // Many seed-artist songs ranked first, then others.
        let items: Vec<Item> = (0..10).map(|i| item("S", &format!("S{i}"))).chain((0..20).map(|i| item(&format!("O{i}"), &format!("L{i}")))).collect();
        let mut spacer = Spacer::new(&[item("S", "S-anchor")], &seed, ARTIST_WINDOW);
        let mut used = vec![false; items.len()];
        let mut order = Vec::new();
        for _ in 0..20 {
            let pick = (0..items.len()).find(|&i| !used[i] && spacer.fits(&items[i], Strict::All)).unwrap();
            used[pick] = true;
            spacer.place(&items[pick]);
            order.push(items[pick].artist.as_str());
        }
        assert_ne!(order[0], "S", "not right after the anchor");
        assert!(order[..5].iter().filter(|a| **a == "S").count() >= 1, "allowed early: {order:?}");
        assert!(order[..10].iter().filter(|a| **a == "S").count() <= SEED_FIRST10, "{order:?}");
        assert!(order[10..20].iter().filter(|a| **a == "S").count() <= SEED_WINDOW, "{order:?}");
        assert!(order.windows(2).all(|w| !(w[0] == "S" && w[1] == "S")), "{order:?}");
    }

    #[test]
    fn one_artist_still_plays() {
        let items: Vec<Item> = (0..5).map(|_| item("A", "L")).collect();
        assert_eq!(respace(&items, &[], 3), [0, 1, 2]);
    }
}
