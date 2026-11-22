//! The Navia mode's plan for one context. Candidates come from collaborative sources around the
//! anchor songs, are scored by relatedness, then interleaved by source with spacing.
//!
//! Relatedness is a noisy-OR over the lists naming a song, `1 − Π(1 − w·e)`. Tags never qualify a
//! song alone, they only widen the plan when fewer than [`QUALIFY_MIN`] qualify. Genre and era are
//! mild multipliers, and plays and stars only order songs within one [`BAND`]. Ties hash the anchor,
//! day and song, so one anchor gives the same list all day.

use std::collections::{HashMap, HashSet};

use super::deezer_src::DeezerAnswers;
use super::sequence::{Item, SPREAD_WINDOW, Spacer, Strict};
use crate::autoplay::song_version;
use crate::client::Song;
use crate::library::Library;
use crate::recommend::{artist_key, genre_match};

/// Similar songs asked per anchor.
pub const SIMILAR_ASK: u32 = 60;
/// The rank at which a list's evidence halves.
pub const RANK_HALF: f32 = 20.0;
/// The anchor artist's songs taken, top first.
pub const ARTIST_SONGS: usize = 30;
/// Neighbor artists taken, and songs of each.
pub const NEIGHBOURS: usize = 20;
pub const NEIGHBOUR_SONGS: usize = 5;
/// ...and of each when too few songs by other artists than the anchors' qualify.
pub const NEIGHBOUR_SONGS_SHORT: usize = 15;
/// Below this many qualifying songs the plan widens.
pub const QUALIFY_MIN: usize = 15;
/// The neighbors' neighbors taken when widening, and songs of each.
pub const WIDEN_ARTISTS: usize = 20;
pub const WIDEN_SONGS: usize = 3;
/// The tag tier: songs within this many years of the anchor.
pub const TAG_YEARS: i32 = 5;
pub const TAG_SONGS: usize = 60;
/// The lowest genre and era multiplier.
pub const GENRE_FLOOR: f32 = 0.85;
pub const ERA_FLOOR: f32 = 0.8;
/// Years apart at which the era multiplier reaches its floor.
pub const ERA_SPAN: f32 = 25.0;
/// Scores within this ratio of each other are a band, ordered by plays and stars.
pub const BAND: f32 = 1.1;
/// The multiplier for a song offered for the same anchor on an earlier day.
pub const RECENT_PENALTY: f32 = 0.85;
/// How far down a source's queue the interleave looks for a song that fits the spacing.
const LOOK: usize = 12;
/// Songs below this score skip the interleave and follow it in rank order.
pub const INTERLEAVE_FLOOR: f32 = 0.3;
/// How many songs on either side of the anchor judge a playlist's coherence.
const PLAYLIST_NEAR: usize = 20;

/// Where a candidate comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Source {
    Similar,
    Artist,
    ListenBrainz,
    DeezerRadio,
    Neighbour,
    Playlist,
    Widened,
    Tags,
}

impl Source {
    /// How much a list's evidence counts, and the source's share of the interleave.
    pub fn weight(self) -> f32 {
        match self {
            Source::Similar => 0.9,
            Source::Artist => 0.85,
            Source::ListenBrainz => 0.85,
            Source::DeezerRadio => 0.7,
            Source::Neighbour => 0.5,
            Source::Playlist => 0.6,
            Source::Widened => 0.3,
            Source::Tags => 0.1,
        }
    }

    /// Fallback tiers come after everything else.
    fn phase(self) -> u8 {
        match self {
            Source::Widened => 1,
            Source::Tags => 2,
            _ => 0,
        }
    }
}

/// Weight of a neighbor artist named by both the server and Deezer. One only gets [`NEIGHBOUR_ONE`].
pub const NEIGHBOUR_BOTH: f32 = 0.6;
pub const NEIGHBOUR_ONE: f32 = 0.4;
/// Relatedness of tag-tier songs, below anything with evidence.
const TAG_RELATED: f32 = 0.02;

/// One list of candidates with its source, weight and each song's evidence from 0 to 1.
#[derive(Clone, Debug)]
pub struct List {
    pub source: Source,
    pub weight: f32,
    pub songs: Vec<(Song, f32)>,
}

/// The evidence of the song at `rank`: 1, halving every [`RANK_HALF`] places.
pub fn rank_weight(rank: usize) -> f32 {
    1.0 / (1.0 + rank as f32 / RANK_HALF)
}

/// What a plan is built from.
pub struct Inputs<'a> {
    /// The anchor songs, newest first.
    pub anchors: &'a [Song],
    /// The server's similar songs for each anchor.
    pub similar: &'a [Vec<Song>],
    /// Deezer's answers, when the listener has it turned on.
    pub deezer: Option<&'a DeezerAnswers>,
    /// ListenBrainz's similar recordings per anchor as library songs, when the switch is on.
    pub listenbrainz: &'a [(String, Vec<Song>)],
    /// Songs offered for the first anchor on an earlier day.
    pub recent: &'a HashSet<String>,
    /// Score multipliers for songs the listener is tired of, by id.
    pub fatigue: &'a HashMap<String, f32>,
    /// Set before all answers are in. Uses only the server's answers and the anchor artist's
    /// songs, never the fallback tiers.
    pub provisional: bool,
    /// The day, for tie-breaks.
    pub day: i64,
}

/// A song of the plan with why it's there.
#[derive(Clone, Debug)]
pub struct Pick {
    pub song: Song,
    pub related: f32,
    pub score: f32,
    pub tier: Source,
}

/// An artist's library songs, most popular first by Deezer rank, then server plays.
fn artist_songs(db: &Library, artist_id: &str, popular: &HashMap<String, usize>) -> Vec<Song> {
    let rows = db.artist_album_rowids(artist_id).unwrap_or_default();
    let mut songs: Vec<Song> = db.songs_by_rowid(&rows).unwrap_or_default().into_iter().map(|(_, s)| s).filter(|s| artist_key(s) == artist_id || s.artists.iter().any(|a| a.id == artist_id)).collect();
    songs.sort_by(|a, b| {
        let place = |s: &Song| popular.get(&s.id).copied().unwrap_or(usize::MAX);
        place(a).cmp(&place(b)).then(b.play_count.unwrap_or(0).cmp(&a.play_count.unwrap_or(0))).then(a.id.cmp(&b.id))
    });
    let mut seen = HashSet::new();
    songs.retain(|s| seen.insert(song_version(s)));
    songs
}

/// The anchors' artists, each once, in anchor order.
fn anchor_artists(anchors: &[Song]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for a in anchors {
        let k = artist_key(a);
        if !k.is_empty() && !out.contains(&k) {
            out.push(k);
        }
    }
    out
}

/// Neighbor artists of the anchors' artists with their weights, from the server's similar
/// artists and Deezer's related ones. Those named by both come first.
pub fn neighbours(db: Option<&Library>, anchors: &[Song], deezer: Option<&DeezerAnswers>) -> Vec<(String, f32)> {
    let own = anchor_artists(anchors);
    let server: Vec<String> = db.map(|db| db.similar_stored(&own).unwrap_or_default()).map(|m| own.iter().flat_map(|a| m.get(a).cloned().unwrap_or_default()).collect()).unwrap_or_default();
    let dz: Vec<String> = deezer.map(|d| own.iter().flat_map(|a| d.related.get(a).cloned().unwrap_or_default()).collect()).unwrap_or_default();
    let mut out: Vec<(String, f32)> = Vec::new();
    for a in server.iter().filter(|a| dz.contains(a)) {
        out.push((a.clone(), NEIGHBOUR_BOTH));
    }
    for a in server.iter().chain(dz.iter()) {
        if !out.iter().any(|(o, _)| o == a) {
            out.push((a.clone(), NEIGHBOUR_ONE));
        }
    }
    out.retain(|(a, _)| !own.contains(a));
    out.truncate(NEIGHBOURS);
    out
}

/// Lists from the listener's playlists that hold an anchor, nearest songs first. Each counts by
/// how many of its songs near the anchor are by `hood` artists, so a mood playlist counts fully
/// and a catch-all barely. Only `hood` artists' songs are taken.
fn playlist_lists(db: &Library, anchors: &[Song], hood: &HashSet<String>) -> Vec<List> {
    let own: HashSet<String> = db.playlist_targets().unwrap_or_default().into_iter().map(|p| p.id).collect();
    let mut out = Vec::new();
    let mut done: HashSet<String> = HashSet::new();
    for anchor in anchors {
        let held: Vec<(String, i64)> = db
            .read(|c| {
                let mut st = c.prepare_cached("SELECT playlist_id, position FROM playlist_song WHERE song_id = ?1")?;
                let rows = st.query_map([&anchor.id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .unwrap_or_default();
        for (id, pos) in held {
            if !own.contains(&id) || !done.insert(id.clone()) {
                continue;
            }
            let Some(p) = db.playlist(&id).ok().flatten() else { continue };
            let mut songs: Vec<(Song, f32)> = p.entry.into_iter().enumerate().map(|(i, s)| (s, 1.0 / (1.0 + (i as i64 - pos).unsigned_abs() as f32 / 10.0))).collect();
            songs.sort_by(|a, b| b.1.total_cmp(&a.1));
            let near: Vec<&Song> = songs.iter().filter(|(s, _)| s.id != anchor.id).take(PLAYLIST_NEAR).map(|(s, _)| s).collect();
            let coherent = if near.is_empty() { 0.0 } else { near.iter().filter(|s| hood.contains(&artist_key(s))).count() as f32 / near.len() as f32 };
            // A playlist confirms within the neighborhood. It doesn't bring in favorites from elsewhere.
            songs.retain(|(s, _)| hood.contains(&artist_key(s)));
            out.push(List { source: Source::Playlist, weight: Source::Playlist.weight() * coherent, songs });
        }
    }
    out
}

/// Every collaborative list for the inputs.
pub fn lists(db: Option<&Library>, inp: &Inputs) -> Vec<List> {
    lists_with(db, inp, NEIGHBOUR_SONGS)
}

/// [`lists`] with `per_neighbour` songs of each neighbour artist.
fn lists_with(db: Option<&Library>, inp: &Inputs, per_neighbour: usize) -> Vec<List> {
    let mut out: Vec<List> = inp.similar.iter().map(|answer| List { source: Source::Similar, weight: Source::Similar.weight(), songs: answer.iter().enumerate().map(|(i, s)| (s.clone(), rank_weight(i))).collect() }).collect();
    let empty = HashMap::new();
    let popular = inp.deezer.map(|d| &d.popular).unwrap_or(&empty);
    if let Some(db) = db {
        for artist in anchor_artists(inp.anchors) {
            let songs = artist_songs(db, &artist, popular).into_iter().take(ARTIST_SONGS).enumerate().map(|(i, s)| (s, rank_weight(i))).collect();
            out.push(List { source: Source::Artist, weight: Source::Artist.weight(), songs });
        }
    }
    if inp.provisional {
        return out;
    }
    for (_, songs) in inp.listenbrainz {
        out.push(List { source: Source::ListenBrainz, weight: Source::ListenBrainz.weight(), songs: songs.iter().enumerate().map(|(i, s)| (s.clone(), rank_weight(i))).collect() });
    }
    if let Some(d) = inp.deezer {
        for (_, songs) in &d.radio {
            out.push(List { source: Source::DeezerRadio, weight: Source::DeezerRadio.weight(), songs: songs.iter().enumerate().map(|(i, s)| (s.clone(), rank_weight(i))).collect() });
        }
    }
    if let Some(db) = db {
        let near = neighbours(Some(db), inp.anchors, inp.deezer);
        let mut hood: HashSet<String> = anchor_artists(inp.anchors).into_iter().collect();
        hood.extend(near.iter().map(|(a, _)| a.clone()));
        hood.extend(out.iter().filter(|l| matches!(l.source, Source::Similar | Source::ListenBrainz | Source::DeezerRadio)).flat_map(|l| l.songs.iter().map(|(s, _)| artist_key(s))));
        for (artist, w) in near {
            let songs = artist_songs(db, &artist, popular).into_iter().take(per_neighbour).enumerate().map(|(i, s)| (s, 1.0 / (1.0 + i as f32 / 2.0))).collect();
            out.push(List { source: Source::Neighbour, weight: w, songs });
        }
        out.extend(playlist_lists(db, inp.anchors, &hood));
    }
    out
}

/// The neighbors' neighbors, from the server's stored similar artists.
fn widened(db: &Library, inp: &Inputs) -> Vec<List> {
    let near: Vec<String> = neighbours(Some(db), inp.anchors, inp.deezer).into_iter().map(|(a, _)| a).collect();
    let own = anchor_artists(inp.anchors);
    let second = db.similar_stored(&near).unwrap_or_default();
    let mut artists: Vec<String> = Vec::new();
    for a in &near {
        for b in second.get(a).into_iter().flatten() {
            if !near.contains(b) && !own.contains(b) && !artists.contains(b) {
                artists.push(b.clone());
            }
        }
    }
    let empty = HashMap::new();
    let popular = inp.deezer.map(|d| &d.popular).unwrap_or(&empty);
    artists
        .into_iter()
        .take(WIDEN_ARTISTS)
        .map(|a| List { source: Source::Widened, weight: Source::Widened.weight(), songs: artist_songs(db, &a, popular).into_iter().take(WIDEN_SONGS).enumerate().map(|(i, s)| (s, 1.0 / (1.0 + i as f32))).collect() })
        .collect()
}

/// The tag tier: songs from the anchor's era by plays, same genre first. Era leads because genre
/// tags are unreliable.
fn tag_list(db: &Library, anchor: &Song) -> Option<List> {
    let year = anchor.year.filter(|y| *y > 0)?;
    let ids: Vec<String> = db
        .read(|c| {
            let mut st = c.prepare_cached("SELECT id FROM song WHERE year BETWEEN ?1 AND ?2 ORDER BY play_count DESC, id LIMIT 400")?;
            let rows = st.query_map([year - TAG_YEARS, year + TAG_YEARS], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .unwrap_or_default();
    let genre = super::genre_of(anchor);
    let mut songs: Vec<Song> = ids.iter().filter_map(|id| db.song(id).ok().flatten()).collect();
    songs.sort_by(|a, b| {
        let g = |s: &Song| if genre.is_empty() { 0.0 } else { genre_match(genre, super::genre_of(s)) };
        g(b).total_cmp(&g(a))
    });
    Some(List { source: Source::Tags, weight: 1.0, songs: songs.into_iter().take(TAG_SONGS).map(|s| (s, TAG_RELATED)).collect() })
}

/// The genre multiplier against the anchor, only when both have a genre.
pub fn genre_factor(anchor: &Song, song: &Song) -> f32 {
    let (a, b) = (super::genre_of(anchor), super::genre_of(song));
    if a.is_empty() || b.is_empty() { 1.0 } else { GENRE_FLOOR + (1.0 - GENRE_FLOOR) * genre_match(a, b) }
}

/// The era multiplier against the anchor, only when both have a year. Bottoms out at
/// [`ERA_FLOOR`] once [`ERA_SPAN`] years apart.
pub fn era_factor(anchor: &Song, song: &Song) -> f32 {
    match (anchor.year.filter(|y| *y > 0), song.year.filter(|y| *y > 0)) {
        (Some(a), Some(b)) => 1.0 - (1.0 - ERA_FLOOR) * ((a - b).abs() as f32 / ERA_SPAN).min(1.0),
        _ => 1.0,
    }
}

/// Plays and a star, only used to order songs within a band.
fn appeal(song: &Song) -> f32 {
    ((1.0 + song.play_count.unwrap_or(0).max(0) as f32).ln()).min(1.5) + if song.starred.is_some() { 1.0 } else { 0.0 }
}

/// A stable hash for ties.
fn tie(anchor: &str, day: i64, id: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in anchor.bytes().chain(day.to_le_bytes()).chain(id.bytes()) {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Merges the lists into scored candidates, best first. `skip(song, version)` leaves songs out.
pub fn score(lists: &[List], inp: &Inputs, skip: &dyn Fn(&Song, &str) -> bool) -> Vec<Pick> {
    struct Cand {
        song: Song,
        /// Per list naming it: (source, w·e).
        parts: Vec<(usize, Source, f32)>,
    }
    let anchor_ids: HashSet<&str> = inp.anchors.iter().map(|a| a.id.as_str()).collect();
    let anchor_versions: HashSet<String> = inp.anchors.iter().map(song_version).collect();
    let mut cands: Vec<Cand> = Vec::new();
    let mut by_version: HashMap<String, usize> = HashMap::new();
    for (li, list) in lists.iter().enumerate() {
        for (song, e) in &list.songs {
            let version = song_version(song);
            if anchor_ids.contains(song.id.as_str()) || anchor_versions.contains(&version) || skip(song, &version) {
                continue;
            }
            let part = (li, list.source, (list.weight * e).clamp(0.0, 1.0));
            match by_version.get(&version) {
                Some(&i) => {
                    if !cands[i].parts.iter().any(|p| p.0 == li) {
                        cands[i].parts.push(part);
                    }
                }
                None => {
                    by_version.insert(version, cands.len());
                    cands.push(Cand { song: song.clone(), parts: vec![part] });
                }
            }
        }
    }
    let anchor = inp.anchors.first();
    let lead = anchor.map(|a| a.id.as_str()).unwrap_or("");
    let mut out: Vec<(Pick, i32, f32, u64)> = cands
        .into_iter()
        .map(|c| {
            let related = 1.0 - c.parts.iter().map(|p| 1.0 - p.2).product::<f32>();
            let tier = c.parts.iter().max_by(|a, b| a.2.total_cmp(&b.2)).map(|p| p.1).unwrap_or(Source::Tags);
            let mut score = related;
            if let Some(a) = anchor {
                score *= genre_factor(a, &c.song) * era_factor(a, &c.song);
            }
            if inp.recent.contains(&c.song.id) {
                score *= RECENT_PENALTY;
            }
            // Skipped lately or played a lot: "not now", about one band down.
            score *= inp.fatigue.get(&c.song.id).copied().unwrap_or(1.0).max(super::fatigue::FLOOR);
            let band = (score.max(1e-6).ln() / BAND.ln()).floor() as i32;
            let (a, t) = (appeal(&c.song), tie(lead, inp.day, &c.song.id));
            (Pick { song: c.song, related, score, tier }, band, a, t)
        })
        .collect();
    out.sort_by(|x, y| y.1.cmp(&x.1).then(y.2.total_cmp(&x.2)).then(x.3.cmp(&y.3)));
    out.into_iter().map(|x| x.0).collect()
}

/// All candidates, best first, with the widened and tag tiers after them when fewer than
/// [`QUALIFY_MIN`] qualify.
pub fn candidates(db: Option<&Library>, inp: &Inputs, skip: &dyn Fn(&Song, &str) -> bool) -> Vec<Pick> {
    let mut all = lists(db, inp);
    let mut picks = score(&all, inp, skip);
    // the seed artist's own songs don't count: they can fill only a few of the first places.
    let own = anchor_artists(inp.anchors);
    let others = |picks: &[Pick]| picks.iter().filter(|p| !own.contains(&artist_key(&p.song))).count();
    if let Some(db) = db
        && others(&picks) < QUALIFY_MIN
        && !inp.provisional
    {
        // short: more of each neighbour artist's songs first, then their neighbours, then tags.
        all = lists_with(Some(db), inp, NEIGHBOUR_SONGS_SHORT);
        picks = score(&all, inp, skip);
    }
    if let Some(db) = db
        && others(&picks) < QUALIFY_MIN
        && !inp.provisional
    {
        all.extend(widened(db, inp));
        picks = score(&all, inp, skip);
        if others(&picks) < QUALIFY_MIN
            && let Some(tags) = inp.anchors.first().and_then(|a| tag_list(db, a))
        {
            all.push(tags);
            picks = score(&all, inp, skip);
        }
    }
    picks
}

/// The play order as indexes into `picks`. Sources take turns by weight times best remaining
/// score, each giving its best song that fits the spacing. Every candidate appears once.
pub fn sequence(picks: &[Pick], history: &[Item], seed_artists: &[String]) -> Vec<usize> {
    // Songs above the floor take turns by source. The rest follow in rank order, then the fallback tiers.
    let mut queues: Vec<(Source, u8, Vec<usize>)> = Vec::new();
    for (i, p) in picks.iter().enumerate() {
        let (tier, phase) = if p.tier.phase() > 0 { (p.tier, p.tier.phase() + 1) } else if p.score >= INTERLEAVE_FLOOR { (p.tier, 0) } else { (Source::Similar, 1) };
        match queues.iter_mut().find(|q| q.0 == tier && q.1 == phase) {
            Some(q) => q.2.push(i),
            None => queues.push((tier, phase, vec![i])),
        }
    }
    queues.sort_by_key(|q| (q.1, q.0));
    let items: Vec<Item> = picks.iter().map(|p| Item::of(&p.song)).collect();
    let mut spacer = Spacer::new(history, seed_artists, SPREAD_WINDOW);
    let mut credit: Vec<f32> = vec![0.0; queues.len()];
    let mut out = Vec::with_capacity(picks.len());
    while out.len() < picks.len() {
        let phase = queues.iter().filter(|q| !q.2.is_empty()).map(|q| q.1).min().unwrap_or(0);
        let active: Vec<usize> = (0..queues.len()).filter(|&t| !queues[t].2.is_empty() && queues[t].1 == phase).collect();
        // Weight times best remaining score, so a source held back by era and genre plays less.
        let share = |t: usize| queues[t].0.weight() * picks[queues[t].2[0]].score;
        let total: f32 = active.iter().map(|&t| share(t)).sum();
        for &t in &active {
            credit[t] += share(t);
        }
        let mut order = active.clone();
        order.sort_by(|a, b| credit[*b].total_cmp(&credit[*a]).then(a.cmp(b)));
        let fitting = |among: &[usize], look: usize, strict: Strict| among.iter().find_map(|&t| queues[t].2.iter().take(look).position(|&i| spacer.fits(&items[i], strict)).map(|k| (t, k)));
        // when the turn's sources have nothing that fits, a later song that does (a neighbour's,
        // the continuation, a fallback tier) comes before a rule gives way
        let all: Vec<usize> = (0..queues.len()).filter(|&t| !queues[t].2.is_empty()).collect();
        // once an artist may come more often, the song whose artist the window holds least, so
        // the loosened run stays spread and doesn't use up the others early
        let fewest = |strict: Strict| {
            let mut best: Option<(usize, usize, usize)> = None;
            for &t in order.iter().chain(all.iter().filter(|t| !order.contains(t))) {
                for (k, &i) in queues[t].2.iter().enumerate() {
                    if spacer.fits(&items[i], strict) {
                        let n = spacer.in_window(&items[i]);
                        if best.is_none_or(|b| n < b.0) {
                            best = Some((n, t, k));
                        }
                    }
                }
            }
            best.map(|(_, t, k)| (t, k))
        };
        let found = fitting(&order, LOOK, Strict::All)
            .or_else(|| fitting(&all, usize::MAX, Strict::All))
            .or_else(|| [Strict::Third, Strict::Gap].into_iter().find_map(fewest))
            .or_else(|| fitting(&all, usize::MAX, Strict::Seed));
        let (t, k) = found.unwrap_or((order[0], 0));
        let i = queues[t].2.remove(k);
        credit[t] -= total;
        spacer.place(&items[i]);
        out.push(i);
    }
    out
}

/// The plan: the candidates in play order.
pub fn build(db: Option<&Library>, inp: &Inputs, skip: &dyn Fn(&Song, &str) -> bool, history: &[Item]) -> Vec<Pick> {
    let picks = candidates(db, inp, skip);
    let seed_artists = anchor_artists(inp.anchors);
    sequence(&picks, history, &seed_artists).into_iter().map(|i| picks[i].clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::ArtistRef;

    fn song(id: &str, artist: &str, genre: &str, year: i32) -> Song {
        Song {
            id: id.into(),
            title: id.into(),
            artist: Some(artist.into()),
            artists: vec![ArtistRef { id: format!("ar-{artist}"), name: artist.into() }],
            album_id: Some(format!("al-{id}")),
            genre: (!genre.is_empty()).then(|| genre.into()),
            year: (year > 0).then_some(year),
            ..Default::default()
        }
    }

    static NO_FATIGUE: std::sync::LazyLock<HashMap<String, f32>> = std::sync::LazyLock::new(HashMap::new);

    fn inputs<'a>(anchors: &'a [Song], similar: &'a [Vec<Song>], deezer: Option<&'a DeezerAnswers>, recent: &'a HashSet<String>) -> Inputs<'a> {
        Inputs { anchors, similar, deezer, listenbrainz: &[], recent, fatigue: &NO_FATIGUE, day: 1, provisional: false }
    }

    #[test]
    fn noisy_or_rewards_agreement_and_tags_never_qualify() {
        let anchor = [song("a", "A", "Soul", 2020)];
        let similar = [vec![song("x", "X", "Soul", 2020), song("both", "B", "Soul", 2020)]];
        let mut dz = DeezerAnswers::default();
        dz.radio.push(("a".into(), vec![song("both", "B", "Soul", 2020)]));
        let none = HashSet::new();
        let picks = candidates(None, &inputs(&anchor, &similar, Some(&dz), &none), &|_, _| false);
        let ids: Vec<&str> = picks.iter().map(|p| p.song.id.as_str()).collect();
        assert_eq!(ids, ["both", "x"], "named by two sources beats one");
        assert!(picks[0].related > 0.9 && picks[1].related < 0.91);
        // Only the anchor and no library: no candidates.
        assert!(candidates(None, &inputs(&anchor, &[vec![]], None, &none), &|_, _| false).is_empty());
    }

    #[test]
    fn genre_is_mild_and_only_counts_when_both_have_one() {
        let a = song("a", "A", "Soul", 2000);
        assert_eq!(genre_factor(&a, &song("b", "B", "", 2000)), 1.0);
        assert_eq!(genre_factor(&a, &song("b", "B", "Metal", 2000)), GENRE_FLOOR);
        assert_eq!(genre_factor(&song("a", "A", "", 2000), &song("b", "B", "Metal", 2000)), 1.0);
        assert!(era_factor(&a, &song("b", "B", "", 1960)) >= ERA_FLOOR);
        // Strong evidence with a mismatched genre still beats weak evidence with a match.
        let similar = [vec![song("strong", "S", "Metal", 2000)], (0..40).map(|i| song(&format!("f{i}"), "F", "", 0)).chain([song("weak", "W", "Soul", 2000)]).collect()];
        let anchors = [a.clone(), song("a2", "A", "Soul", 2000)];
        let none = HashSet::new();
        let picks = candidates(None, &inputs(&anchors, &similar, None, &none), &|_, _| false);
        let pos = |id: &str| picks.iter().position(|p| p.song.id == id).unwrap();
        assert!(pos("strong") < pos("weak"));
    }

    #[test]
    fn the_plan_is_deterministic_and_bands_order_by_plays() {
        let anchor = [song("a", "A", "", 0)];
        let mut answer: Vec<Song> = (0..30).map(|i| song(&format!("s{i}"), &format!("Art{i}"), "", 0)).collect();
        answer[1].play_count = Some(50);
        let similar = [answer];
        let none = HashSet::new();
        let one = build(None, &inputs(&anchor, &similar, None, &none), &|_, _| false, &[]);
        let two = build(None, &inputs(&anchor, &similar, None, &none), &|_, _| false, &[]);
        let ids = |p: &[Pick]| p.iter().map(|p| p.song.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&one), ids(&two));
        assert_eq!(one[0].song.id, "s1", "a played song within the top band goes first");
        // Offered for this anchor on an earlier day, so further down.
        let recent: HashSet<String> = HashSet::from(["s1".to_string(), "s0".to_string()]);
        let three = build(None, &inputs(&anchor, &similar, None, &recent), &|_, _| false, &[]);
        assert!(three.iter().position(|p| p.song.id == "s0").unwrap() > 0);
    }

    #[test]
    fn the_seed_artist_fills_at_most_two_of_the_first_ten_even_with_few_candidates() {
        let pick = |id: String, artist: &str, tier: Source, score: f32| Pick { song: song(&id, artist, "", 0), related: score, score, tier };
        // many strong songs by the seed artist, a few weak neighbours (under the interleave floor).
        let mut picks: Vec<Pick> = (0..30).map(|i| pick(format!("own{i}"), "Seed", Source::Artist, 0.8)).collect();
        picks.extend((0..8).map(|i| pick(format!("n{i}"), &format!("N{i}"), Source::Neighbour, 0.2)));
        let order = sequence(&picks, &[], &["ar-Seed".to_string()]);
        let first10: Vec<&str> = order.iter().take(10).map(|&i| picks[i].song.id.as_str()).collect();
        assert!(first10.iter().filter(|id| id.starts_with("own")).count() <= 2, "{first10:?}");
        assert!(first10.iter().any(|id| id.starts_with("own")), "allowed, not left out: {first10:?}");
        assert_eq!(order.len(), picks.len());
    }

    fn spread_pick(id: String, artist: &str, tier: Source, score: f32) -> Pick {
        Pick { song: song(&id, artist, "", 0), related: score, score, tier }
    }

    fn artists_in_order<'a>(picks: &'a [Pick], order: &[usize]) -> Vec<&'a str> {
        order.iter().map(|&i| picks[i].song.artist.as_deref().unwrap_or("")).collect()
    }

    fn most_by_one(artists: &[&str]) -> usize {
        artists.iter().map(|a| artists.iter().filter(|b| *b == a).count()).max().unwrap_or(0)
    }

    #[test]
    fn the_shown_songs_have_at_most_two_by_an_artist_and_never_two_in_a_row() {
        // strong artists with many songs each lead every source, a long tail of others follows
        let mut picks: Vec<Pick> = Vec::new();
        for i in 0..8 {
            picks.extend((0..6).map(|k| spread_pick(format!("s{i}-{k}"), &format!("Strong{i}"), Source::Similar, 0.9 - k as f32 * 0.01)));
        }
        picks.extend((0..80).map(|i| spread_pick(format!("n{i}"), &format!("Other{i}"), Source::Neighbour, 0.5)));
        picks.extend((0..10).map(|i| spread_pick(format!("own{i}"), "Seed", Source::Artist, 0.85)));
        let order = sequence(&picks, &[], &["ar-Seed".to_string()]);
        assert_eq!(order.len(), picks.len(), "every candidate stays in the plan");
        let artists = artists_in_order(&picks, &order);
        // while the pool lasts, every run of 25 the queue shows as the plan is walked
        for start in 0..=60 {
            let shown = &artists[start..start + SPREAD_WINDOW];
            let others: Vec<&str> = shown.iter().copied().filter(|a| *a != "Seed").collect();
            assert!(most_by_one(&others) <= 2, "{shown:?}");
        }
        assert!(artists.windows(2).all(|w| w[0] != w[1]), "{artists:?}");
        let first10 = &artists[..10];
        assert!(first10.iter().filter(|a| **a == "Seed").count() <= 2, "the seed artist's own cap: {first10:?}");
        assert!(first10.contains(&"Seed"), "the seed artist is allowed: {first10:?}");
    }

    #[test]
    fn a_small_pool_loosens_the_spread_instead_of_shortening_the_list() {
        // ten artists of three songs: a third song each is needed to fill 25, never back to back
        let picks: Vec<Pick> = (0..10).flat_map(|i| (0..3).map(move |k| spread_pick(format!("a{i}-{k}"), &format!("A{i}"), Source::Similar, 0.9)).collect::<Vec<_>>()).collect();
        let order = sequence(&picks, &[], &[]);
        assert_eq!(order.len(), picks.len());
        let shown = artists_in_order(&picks, &order[..SPREAD_WINDOW]);
        assert_eq!(most_by_one(&shown), 3, "{shown:?}");
        assert!(shown.windows(2).all(|w| w[0] != w[1]), "{shown:?}");
        // three artists only: any number of songs each, still never back to back
        let picks: Vec<Pick> = (0..3).flat_map(|i| (0..10).map(move |k| spread_pick(format!("b{i}-{k}"), &format!("B{i}"), Source::Similar, 0.9)).collect::<Vec<_>>()).collect();
        let order = sequence(&picks, &[], &[]);
        assert_eq!(order.len(), picks.len());
        let shown = artists_in_order(&picks, &order[..SPREAD_WINDOW]);
        assert!(shown.windows(2).all(|w| w[0] != w[1]), "{shown:?}");
        // one artist and a few others: back to back only once nothing else is left
        let mut picks: Vec<Pick> = (0..20).map(|k| spread_pick(format!("c{k}"), "C", Source::Similar, 0.9)).collect();
        picks.extend((0..3).map(|i| spread_pick(format!("d{i}"), &format!("D{i}"), Source::Similar, 0.5)));
        let order = sequence(&picks, &[], &[]);
        assert_eq!(order.len(), picks.len());
        let artists = artists_in_order(&picks, &order);
        let last_other = artists.iter().rposition(|a| *a != "C").unwrap();
        let first_pair = artists.windows(2).position(|w| w[0] == w[1]).unwrap();
        assert!(first_pair > last_other, "{artists:?}");
    }

    #[test]
    fn sources_take_turns() {
        let anchor = [song("a", "A", "", 0)];
        let similar = [(0..20).map(|i| song(&format!("s{i}"), &format!("S{i}"), "", 0)).collect::<Vec<_>>()];
        let mut dz = DeezerAnswers::default();
        dz.radio.push(("a".into(), (0..20).map(|i| song(&format!("d{i}"), &format!("D{i}"), "", 0)).collect()));
        let none = HashSet::new();
        let plan = build(None, &inputs(&anchor, &similar, Some(&dz), &none), &|_, _| false, &[]);
        let first10: Vec<&str> = plan.iter().take(10).map(|p| p.song.id.as_str()).collect();
        assert!(first10.iter().filter(|i| i.starts_with('d')).count() >= 3, "{first10:?}");
        assert!(first10.iter().filter(|i| i.starts_with('s')).count() >= 4, "{first10:?}");
    }
}
