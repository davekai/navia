//! held by song, so a selection survives sorting and search.

use std::collections::HashSet;

#[derive(Default)]
pub struct Selection {
    songs: HashSet<u32>,
    /// the last row clicked without Shift.
    anchor: Option<u32>,
}

impl Selection {
    pub fn clear(&mut self) {
        self.songs.clear();
        self.anchor = None;
    }

    pub fn contains(&self, song: u32) -> bool {
        self.songs.contains(&song)
    }

    /// Handles a click on a row. `order` is the song at each visible row.
    pub fn click(&mut self, row: usize, order: &[u32], ctrl: bool, shift: bool) {
        let Some(&song) = order.get(row) else { return };
        let anchor_row = self.anchor.and_then(|a| order.iter().position(|s| *s == a));
        match (shift, anchor_row) {
            (true, Some(from)) => {
                let (lo, hi) = (from.min(row), from.max(row));
                if !ctrl {
                    self.songs.clear();
                }
                self.songs.extend(&order[lo..=hi]);
            }
            _ if ctrl => {
                if !self.songs.remove(&song) {
                    self.songs.insert(song);
                }
                self.anchor = Some(song);
            }
            _ => {
                self.songs.clear();
                self.songs.insert(song);
                self.anchor = Some(song);
            }
        }
    }

    /// Prepares the selection before a row menu opens. A selected row keeps it, any other row replaces it.
    pub fn prepare_context(&mut self, row: usize, order: &[u32]) {
        if order.get(row).is_some_and(|song| !self.songs.contains(song)) {
            self.click(row, order, false, false);
        }
    }

    /// in the current order.
    pub fn first_in(&self, order: &[u32]) -> Option<usize> {
        order.iter().position(|s| self.songs.contains(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORDER: [u32; 6] = [10, 11, 12, 13, 14, 15];

    fn selected(s: &Selection) -> Vec<u32> {
        let mut v: Vec<u32> = s.songs.iter().copied().collect();
        v.sort();
        v
    }

    #[test]
    fn plain_click_replaces() {
        let mut s = Selection::default();
        s.click(1, &ORDER, false, false);
        s.click(3, &ORDER, false, false);
        assert_eq!(selected(&s), [13]);
    }

    #[test]
    fn ctrl_click_toggles() {
        let mut s = Selection::default();
        s.click(1, &ORDER, false, false);
        s.click(3, &ORDER, true, false);
        assert_eq!(selected(&s), [11, 13]);
        s.click(1, &ORDER, true, false);
        assert_eq!(selected(&s), [13]);
    }

    #[test]
    fn shift_click_ranges_from_the_anchor_in_either_direction() {
        let mut s = Selection::default();
        s.click(2, &ORDER, false, false);
        s.click(4, &ORDER, false, true);
        assert_eq!(selected(&s), [12, 13, 14]);
        s.click(0, &ORDER, false, true);
        assert_eq!(selected(&s), [10, 11, 12]);
    }

    #[test]
    fn shift_click_without_an_anchor_selects_the_row() {
        let mut s = Selection::default();
        s.click(2, &ORDER, false, true);
        assert_eq!(selected(&s), [12]);
    }

    #[test]
    fn selection_follows_songs_through_a_new_order() {
        let mut s = Selection::default();
        s.click(2, &ORDER, false, false);
        let resorted = [15, 14, 13, 12, 11, 10];
        assert_eq!(s.first_in(&resorted), Some(3));
    }

    #[test]
    fn context_click_keeps_a_selection_it_lands_in() {
        let mut s = Selection::default();
        s.click(1, &ORDER, false, false);
        s.click(3, &ORDER, true, false);
        s.prepare_context(3, &ORDER);
        assert_eq!(selected(&s), [11, 13]);
        s.prepare_context(5, &ORDER);
        assert_eq!(selected(&s), [15]);
    }
}
