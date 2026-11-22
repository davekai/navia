//! the server's similar songs for the Navia mode, without its filler.
//!
//! the server answers `getSimilarSongs2` with an agent's songs (Last.fm, ListenBrainz, a plugin),
//! which it caches, so they come back the same on every call; when they don't fill the count it
//! tops the answer up with draws that change on every call (random songs of the seed's genre, or
//! a weighted pick of similar artists' top songs). a client can't tell the two apart in one
//! answer, so each anchor is asked twice at once and only the head both answers hold as the same
//! set is kept, in the first answer's order. songs the two draws share by chance further down are
//! left out with the rest.
//!
//! the cleaned answer is kept in memory for the session ([`super::Shared`]), never on disk.

use std::collections::HashSet;

use crate::client::{Client, Song};
use crate::library::Library;

/// anchors whose cleaned answer is kept for the session.
pub const KEEP_ANCHORS: usize = 500;

/// the longest head both answers hold as the same set of songs, in `first`'s order. agents' songs
/// come first in every answer, in any order; the top-up after them differs between calls.
pub fn stable_head(first: &[Song], second: &[Song]) -> Vec<Song> {
    let first = distinct(first);
    let second = distinct(second);
    // +1 for a song in `first`'s head only, -1 for one in `second`'s only.
    let mut balance: std::collections::HashMap<&str, i32> = std::collections::HashMap::new();
    let mut uneven = 0usize;
    let mut head = 0;
    for (i, (x, y)) in first.iter().zip(&second).enumerate() {
        for (id, step) in [(x.id.as_str(), 1), (y.id.as_str(), -1)] {
            let b = balance.entry(id).or_insert(0);
            let was = *b != 0;
            *b += step;
            match (was, *b != 0) {
                (false, true) => uneven += 1,
                (true, false) => uneven -= 1,
                _ => {}
            }
        }
        if uneven == 0 {
            head = i + 1;
        }
    }
    first[..head].iter().map(|s| (*s).clone()).collect()
}

fn distinct(songs: &[Song]) -> Vec<&Song> {
    let mut seen = HashSet::new();
    songs.iter().filter(|s| seen.insert(s.id.as_str())).collect()
}

/// the server's cleaned similar songs for each seed ([`stable_head`] of two asks of `count`), from
/// the session's memory when known. both asks for every seed go at once. a seed whose ask failed
/// gives an empty list and is asked again next time.
pub async fn similar(client: &Client, db: &Library, seeds: &[String], count: u32) -> Vec<Vec<Song>> {
    let shared = db.engine_shared();
    let mut out: Vec<Option<Vec<Song>>> = seeds.iter().map(|s| shared.cleaned(s)).collect();
    let mut set = tokio::task::JoinSet::new();
    for (i, seed) in seeds.iter().enumerate().filter(|(i, _)| out[*i].is_none()) {
        for call in 0..2 {
            let (client, seed) = (client.clone(), seed.clone());
            set.spawn(async move { (i, call, client.similar_songs2(&seed, count).await) });
        }
    }
    let mut got: Vec<[Option<Vec<Song>>; 2]> = vec![[None, None]; seeds.len()];
    while let Some(done) = set.join_next().await {
        if let Ok((i, call, Ok(songs))) = done {
            got[i][call] = Some(songs);
        }
    }
    for (i, [first, second]) in got.into_iter().enumerate() {
        if out[i].is_some() {
            continue;
        }
        let (Some(first), Some(second)) = (first, second) else {
            out[i] = Some(Vec::new());
            continue;
        };
        let head = stable_head(&first, &second);
        shared.keep_cleaned(&seeds[i], head.clone());
        out[i] = Some(head);
    }
    out.into_iter().map(Option::unwrap_or_default).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn songs(ids: &[&str]) -> Vec<Song> {
        ids.iter().map(|id| Song { id: (*id).into(), ..Default::default() }).collect()
    }

    fn ids(songs: &[Song]) -> Vec<&str> {
        songs.iter().map(|s| s.id.as_str()).collect()
    }

    #[test]
    fn a_stable_answer_is_kept_whole_in_the_first_order() {
        let a = songs(&["a", "b", "c", "d"]);
        let b = songs(&["b", "a", "d", "c"]);
        assert_eq!(ids(&stable_head(&a, &b)), ["a", "b", "c", "d"]);
        assert_eq!(ids(&stable_head(&a, &a)), ["a", "b", "c", "d"]);
    }

    #[test]
    fn a_random_answer_leaves_nothing_even_where_draws_share_songs() {
        let a = songs(&["r1", "r2", "x", "r3", "y"]);
        let b = songs(&["q1", "x", "q2", "y", "q3"]);
        assert!(stable_head(&a, &b).is_empty());
    }

    #[test]
    fn real_songs_padded_with_random_keep_only_the_real_head() {
        // three agent songs in another order, then draws that share "p2" by chance.
        let a = songs(&["s1", "s2", "s3", "p1", "p2", "p3"]);
        let b = songs(&["s3", "s1", "s2", "p2", "p4", "p5"]);
        assert_eq!(ids(&stable_head(&a, &b)), ["s1", "s2", "s3"]);
        // answers of different lengths: only what both hold.
        assert_eq!(ids(&stable_head(&songs(&["s1", "s2"]), &songs(&["s2", "s1", "p9"]))), ["s1", "s2"]);
        assert!(stable_head(&[], &a).is_empty());
    }
}
