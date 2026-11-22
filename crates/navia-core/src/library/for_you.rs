//! Discover's For You page. Every row comes from server data, hides when empty, and nothing is
//! created on the server. Mixes come from the Autoplay engine and are made once per local day.
//! No album shows in two rows or in a row Home already shows.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};

use rusqlite::params;
use serde::{Deserialize, Serialize};

use super::Library;
use super::taste::{self, Taste};
use crate::autoplay::{main_artist, song_version};
use crate::client::{Album, Client, Song};
use crate::error::Result;

/// A mix's length, and the fewest songs that still make one.
pub const MIX_SIZE: usize = 30;
pub const MIX_MIN: usize = 10;
pub const ARTIST_MIXES: usize = 4;
pub const GENRE_MIXES: usize = 2;
/// At most this many songs by one artist in a mix, not counting an artist mix's own artist.
pub const PER_ARTIST: usize = MIX_SIZE / 5;
/// Seeds of a genre mix: the listener's top songs in the genre lately.
pub const GENRE_SEEDS: usize = 5;
/// Most anchors an artist mix has: the listener's top songs by the artist.
pub const ARTIST_ANCHORS: usize = 3;
/// Songs taken from the engine's plan for a mix, and genre songs asked for to fill one.
const PLAN_LEN: usize = 120;
const GENRE_FILL: u32 = 60;
/// How many top artists the album rows look at.
pub const TOP_ARTISTS: usize = 10;
/// Album rows show at most this many.
pub const ROW_LIMIT: usize = 20;
pub const FORGOTTEN_DAYS: i64 = 183;
pub const FORGOTTEN_LIMIT: usize = 8;
pub const STALE_DAYS: i64 = 90;
pub const STALE_MIN_PLAYS: u32 = 3;
/// How many Home's Recently Added and Most Played show. For You leaves those albums out.
const HOME_SHELF: u32 = 16;
const DAY: i64 = 86_400;
const META_MIXES: &str = "for_you_mixes";
/// mixes made at once. each asks the server for similar songs for all its anchors at once (up to
/// [`GENRE_SEEDS`]), and those answers can take seconds on the server's side, so this stays low on
/// a shared server.
pub const MIXES_AT_ONCE: usize = 3;
/// how often [`keep_mixes_daily`] looks whether the local day turned.
const DAY_CHECK: std::time::Duration = std::time::Duration::from_secs(600);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MixKind {
    Artist,
    Genre,
}

/// A mix as made for the day: its song ids in order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Mix {
    /// `artist:<artist id>` or `genre:<genre name>`, the mix page's address.
    pub id: String,
    pub kind: MixKind,
    /// The artist's or genre's name. The title is "<name> Mix".
    pub name: String,
    pub songs: Vec<String>,
}

/// A mix with its library rows, as the apps show it.
#[derive(Clone, Debug, PartialEq)]
pub struct MixRows {
    pub id: String,
    pub kind: MixKind,
    pub name: String,
    pub songs: Vec<u32>,
    /// Main artists in order of first appearance, for the "A, B, C and more" line.
    pub artists: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct StoredMixes {
    day: i64,
    /// what the page shows: the mixes made so far, ranked.
    mixes: Vec<Mix>,
    /// false while the day's mixes are still being made.
    #[serde(default = "complete_before")]
    complete: bool,
    /// while not complete: each mix made so far with its score, so a build that was cut short
    /// picks up where it stopped.
    #[serde(default)]
    built: Vec<Built>,
}

/// stored before this field existed, which was only ever a finished day.
fn complete_before() -> bool {
    true
}

#[derive(Clone, Serialize, Deserialize)]
struct Built {
    mix: Mix,
    score: f64,
}

/// The page's rows: album rows for album rows, song rows for songs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ForYou {
    pub mixes: Vec<MixRows>,
    pub new_to_you: Vec<u32>,
    pub unplayed: Vec<u32>,
    pub forgotten: Vec<u32>,
    pub on_this_day: Vec<u32>,
    pub not_played_recently: Vec<u32>,
    /// today's mixes and similar artists haven't all been asked for yet ([`refresh_for_you`]).
    pub needs_refresh: bool,
    /// today's mixes are being made right now; more may land.
    pub building: bool,
}

/// A string hash: `h * 31 + code unit`, then murmur3's finalizer so a new day reorders the whole
/// list rather than shifting it.
fn hash(s: &str) -> u32 {
    let mut h = s.encode_utf16().fold(0u32, |h, c| h.wrapping_mul(31).wrapping_add(u32::from(c)));
    h ^= h >> 16;
    h = h.wrapping_mul(0x85eb_ca6b);
    h ^= h >> 13;
    h = h.wrapping_mul(0xc2b2_ae35);
    h ^ (h >> 16)
}

/// The same order all day, a new one tomorrow.
pub fn daily<T: Clone>(day: i64, key: &str, list: &[T], id: impl Fn(&T) -> &str) -> Vec<T> {
    let mut keyed: Vec<(u32, T)> = list.iter().map(|t| (hash(&format!("{day}|{key}|{}", id(t))), t.clone())).collect();
    keyed.sort_by_key(|(h, _)| *h);
    keyed.into_iter().map(|(_, t)| t).collect()
}

/// Drops repeats and other versions of a song already in the list.
pub fn distinct(list: Vec<Song>) -> Vec<Song> {
    let mut seen = HashSet::new();
    list.into_iter().filter(|s| seen.insert(song_version(s))).collect()
}

/// At most [`PER_ARTIST`] songs per main artist, except for `except`.
pub fn capped(list: Vec<Song>, except: Option<&str>) -> Vec<Song> {
    let mut n: HashMap<String, usize> = HashMap::new();
    list.into_iter()
        .filter(|s| {
            let a = main_artist(s).to_string();
            if Some(a.as_str()) == except {
                return true;
            }
            let c = n.entry(a).or_default();
            *c += 1;
            *c <= PER_ARTIST
        })
        .collect()
}

fn credits(song: &Song, artist_id: &str) -> bool {
    song.artist_id.as_deref() == Some(artist_id) || song.artists.iter().any(|a| a.id == artist_id)
}

/// An artist mix: a third the artist's own songs, led by a different anchor each day, the rest the
/// plan's other artists in order. `None` when the plan has no other artist or is too short.
pub fn artist_mix(day: i64, artist_id: &str, name: &str, anchors: &[Song], plan: &[Song]) -> Option<Mix> {
    let id = format!("artist:{artist_id}");
    let others: Vec<Song> = plan.iter().filter(|s| !credits(s, artist_id)).cloned().collect();
    if others.is_empty() {
        return None;
    }
    let lead = (!anchors.is_empty()).then(|| day.rem_euclid(anchors.len() as i64) as usize);
    let own: Vec<Song> = lead
        .map(|l| anchors[l].clone())
        .into_iter()
        .chain(plan.iter().filter(|s| credits(s, artist_id)).cloned())
        .chain(anchors.iter().enumerate().filter(|(i, _)| Some(*i) != lead).map(|(_, s)| s.clone()))
        .take(MIX_SIZE.div_ceil(3))
        .collect();
    let (mut o, mut s) = (own.into_iter(), others.into_iter());
    let mut mixed = Vec::new();
    for i in 0.. {
        let next = if i % 3 == 0 { o.next().or_else(|| s.next()) } else { s.next().or_else(|| o.next()) };
        match next {
            Some(t) => mixed.push(t),
            None => break,
        }
    }
    let songs: Vec<String> = capped(distinct(mixed), Some(name)).into_iter().take(MIX_SIZE).map(|t| t.id).collect();
    (songs.len() >= MIX_MIN).then(|| Mix { id, kind: MixKind::Artist, name: name.to_string(), songs })
}

/// Whether a song carries the genre, as the server tags it.
pub fn in_genre(song: &Song, genre: &str) -> bool {
    if song.genres.is_empty() { song.genre.as_deref() == Some(genre) } else { song.genres.iter().any(|g| g.name == genre) }
}

/// A genre mix: a seed first, a different one each day, then the other seeds, the plan's songs in
/// the genre and the genre's other songs, without repeats and capped per artist.
pub fn genre_mix(day: i64, genre: &str, seeds: &[Song], plan: &[Song], fill: &[Song]) -> Option<Mix> {
    if seeds.is_empty() {
        return None;
    }
    let id = format!("genre:{genre}");
    let seed_ids: HashSet<&str> = seeds.iter().map(|s| s.id.as_str()).collect();
    let rest = plan.iter().cloned().chain(daily(day, &id, fill, |s| &s.id)).filter(|s| in_genre(s, genre) && !seed_ids.contains(s.id.as_str()));
    let first = seeds[day.rem_euclid(seeds.len() as i64) as usize].clone();
    let list: Vec<Song> = std::iter::once(first).chain(daily(day, &id, seeds, |s| &s.id).into_iter().take(4)).chain(rest).collect();
    let songs: Vec<String> = capped(distinct(list), None).into_iter().take(MIX_SIZE).map(|t| t.id).collect();
    (songs.len() >= MIX_MIN).then(|| Mix { id, kind: MixKind::Genre, name: genre.to_string(), songs })
}

/// Orders artist and genre mixes by score, each against the best of its kind, since a genre sums
/// many artists. Ties keep artists first.
pub fn rank_mixes(artists: Vec<(Mix, f64)>, genres: Vec<(Mix, f64)>) -> Vec<Mix> {
    let scaled = |list: Vec<(Mix, f64)>| {
        let best = list.first().map(|(_, s)| *s).filter(|s| *s > 0.0).unwrap_or(1.0);
        list.into_iter().map(move |(m, s)| (m, s / best))
    };
    let mut all: Vec<(Mix, f64)> = scaled(artists).chain(scaled(genres)).collect();
    all.sort_by(|a, b| b.1.total_cmp(&a.1));
    all.into_iter().map(|(m, _)| m).collect()
}

/// Brings forward songs from albums no earlier tile's cover used, so mixes that share artists
/// don't share covers. A cover is the first four albums in the list.
pub fn fresh_covers(mixes: Vec<Mix>, album_of: impl Fn(&str) -> Option<String>) -> Vec<Mix> {
    let mut used: HashSet<String> = HashSet::new();
    mixes
        .into_iter()
        .map(|mut mix| {
            let mut front = Vec::new();
            let mut albums = HashSet::new();
            for id in &mix.songs {
                if front.len() == 4 {
                    break;
                }
                if let Some(al) = album_of(id)
                    && !used.contains(&al)
                    && albums.insert(al)
                {
                    front.push(id.clone());
                }
            }
            used.extend(albums);
            let rest: Vec<String> = mix.songs.iter().filter(|s| !front.contains(s)).cloned().collect();
            mix.songs = front.into_iter().chain(rest).collect();
            mix
        })
        .collect()
}

/// Takes from each list in turn, so one artist with many albums doesn't fill a row.
pub fn round_robin(lists: &[Vec<u32>], skip: &mut HashSet<u32>, limit: usize) -> Vec<u32> {
    let mut out = Vec::new();
    let longest = lists.iter().map(Vec::len).max().unwrap_or(0);
    for i in 0..longest {
        for l in lists {
            if out.len() == limit {
                return out;
            }
            if let Some(a) = l.get(i)
                && skip.insert(*a)
            {
                out.push(*a);
            }
        }
    }
    out
}

/// The civil date of a day count since 1970-01-01 (proleptic Gregorian).
pub fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// An album's full first-release date: `originalReleaseDate`, else `releaseDate`.
pub fn first_release(album: &Album) -> Option<(i32, i32, i32)> {
    [&album.original_release_date, &album.release_date].into_iter().flatten().find_map(|d| Some((d.year?, d.month?, d.day?)).filter(|(y, m, d)| *y > 0 && *m > 0 && *d > 0))
}

impl Library {
    /// The page from what's stored. Fast enough to call per visit. Mixes and similar artists come
    /// from the day's [`refresh_for_you`].
    pub fn for_you(&self, local_day: i64) -> Result<ForYou> {
        self.for_you_at(local_day, taste::now_unix())
    }

    pub fn for_you_at(&self, local_day: i64, now: i64) -> Result<ForYou> {
        let taste = self.taste()?;
        let mut out = ForYou { mixes: self.mixes(local_day)?, ..Default::default() };
        let top: Vec<&taste::Scored> = taste.artists.iter().take(TOP_ARTISTS).collect();
        let stored_mixes = self.stored_mixes()?.is_some_and(|s| s.day == local_day && s.complete);
        let mut similar_known = !top.is_empty();
        let mut used = self.home_album_rowids(local_day, &taste)?;

        // New to You.
        let top_ids: HashSet<&str> = top.iter().map(|a| a.id.as_str()).collect();
        let mut similar: Vec<String> = Vec::new();
        for a in &top {
            match crate::similar_artists::stored_for_day(self, &a.id, local_day) {
                Some(ids) => similar.extend(ids.into_iter().filter(|i| !top_ids.contains(i.as_str()))),
                None => similar_known = false,
            }
        }
        let mut seen = HashSet::new();
        similar.retain(|i| seen.insert(i.clone()));
        let lists: Vec<Vec<u32>> = similar.iter().map(|id| self.unplayed_albums(id)).collect::<Result<_>>()?;
        out.new_to_you = round_robin(&lists, &mut used, ROW_LIMIT);

        // Unplayed from Your Top Artists.
        let lists: Vec<Vec<u32>> = top.iter().map(|a| self.unplayed_albums(&a.id)).collect::<Result<_>>()?;
        out.unplayed = round_robin(&lists, &mut used, ROW_LIMIT);

        out.forgotten = self.forgotten_favorites(now)?;

        // On This Day.
        let (year, month, day) = civil(local_day);
        let days: Vec<u32> = self.released_on(year as i32, month as i32, day as i32)?;
        out.on_this_day = round_robin(&[days], &mut used, ROW_LIMIT);

        // Not Played Recently.
        let stale = self.stale_albums(now)?;
        out.not_played_recently = round_robin(&[stale], &mut used, ROW_LIMIT);

        out.needs_refresh = !top.is_empty() && (!stored_mixes || !similar_known);
        out.building = self.for_you_building();
        Ok(out)
    }

    /// Today's mixes with their library rows. Missing songs are left out, and so is a mix they
    /// leave too short. Empty before today's are made.
    pub fn mixes(&self, local_day: i64) -> Result<Vec<MixRows>> {
        let Some(stored) = self.stored_mixes()?.filter(|s| s.day == local_day) else { return Ok(Vec::new()) };
        let mut out = Vec::new();
        for mix in stored.mixes {
            let rows = self.song_rowids_in_order(&mix.songs)?;
            if rows.len() < MIX_MIN {
                continue;
            }
            let songs = self.songs_by_rowid(&rows)?;
            let mut artists: Vec<String> = Vec::new();
            for (_, s) in &songs {
                let a = main_artist(s).to_string();
                if !a.is_empty() && !artists.contains(&a) {
                    artists.push(a);
                }
            }
            out.push(MixRows { id: mix.id, kind: mix.kind, name: mix.name, songs: rows, artists });
        }
        Ok(out)
    }

    /// One of today's mixes by its address.
    pub fn mix(&self, local_day: i64, id: &str) -> Result<Option<MixRows>> {
        Ok(self.mixes(local_day)?.into_iter().find(|m| m.id == id))
    }

    fn stored_mixes(&self) -> Result<Option<StoredMixes>> {
        Ok(self.meta(META_MIXES)?.and_then(|v| serde_json::from_str(&v).ok()))
    }

    /// keeps the day's mixes, all made. nothing is derived from them, so the taste cache stays.
    pub fn store_mixes(&self, local_day: i64, mixes: &[Mix]) -> Result<()> {
        self.put_mixes(&StoredMixes { day: local_day, mixes: mixes.to_vec(), complete: true, built: Vec::new() })
    }

    fn put_mixes(&self, stored: &StoredMixes) -> Result<()> {
        let json = serde_json::to_string(stored).unwrap_or_default();
        self.write_quiet(|tx| {
            tx.execute("INSERT INTO meta(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value", params![META_MIXES, json])?;
            Ok(())
        })
    }

    fn song_rowids_in_order(&self, ids: &[String]) -> Result<Vec<u32>> {
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            out.extend(self.song_rowids(std::slice::from_ref(id))?);
        }
        Ok(out)
    }

    /// Albums Home shows: this turn's World of, Recently Added and Most Played.
    fn home_album_rowids(&self, local_day: i64, taste: &Taste) -> Result<HashSet<u32>> {
        let mut out: HashSet<u32> = taste.albums.iter().take(HOME_SHELF as usize).map(|a| a.rowid).collect();
        let world = taste.world_of(self.home_turn(local_day)?).map(|w| w.id.clone());
        self.read(|c| {
            let mut st = c.prepare_cached("SELECT rowid FROM album ORDER BY created DESC, sort_name LIMIT ?1")?;
            for r in st.query_map([HOME_SHELF], |r| r.get::<_, i64>(0))? {
                out.insert(r? as u32);
            }
            if let Some(w) = &world {
                let mut st = c.prepare_cached("SELECT rowid FROM album WHERE artist_id = ?1")?;
                for r in st.query_map([w], |r| r.get::<_, i64>(0))? {
                    out.insert(r? as u32);
                }
            }
            Ok(out)
        })
    }

    /// An artist's own albums nobody here has played, newest first.
    fn unplayed_albums(&self, artist_id: &str) -> Result<Vec<u32>> {
        self.read(|c| {
            let mut st = c.prepare_cached("SELECT rowid FROM album WHERE artist_id = ?1 AND play_count = 0 AND played IS NULL ORDER BY year DESC, sort_name")?;
            let rows = st.query_map([artist_id], |r| r.get::<_, i64>(0))?;
            Ok(rows.map(|r| r.map(|v| v as u32)).collect::<std::result::Result<_, _>>()?)
        })
    }

    /// Hearted songs last played over [`FORGOTTEN_DAYS`] ago, most played first, one per song and
    /// its other versions. Songs never played aren't forgotten.
    fn forgotten_favorites(&self, now: i64) -> Result<Vec<u32>> {
        let rows: Vec<(u32, String)> = self.read(|c| {
            let mut st = c.prepare_cached(
                "SELECT rowid, data FROM song WHERE starred IS NOT NULL AND played IS NOT NULL AND unixepoch(played) < ?1
                 ORDER BY play_count DESC, sort_name LIMIT 400",
            )?;
            let rows = st.query_map([now - FORGOTTEN_DAYS * DAY], |r| Ok((r.get::<_, i64>(0)? as u32, r.get::<_, String>(1)?)))?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })?;
        let mut seen = HashSet::new();
        Ok(rows
            .into_iter()
            .filter_map(|(r, data)| serde_json::from_str::<Song>(&data).ok().map(|s| (r, s)))
            .filter(|(_, s)| seen.insert(song_version(s)))
            .take(FORGOTTEN_LIMIT)
            .map(|(r, _)| r)
            .collect())
    }

    /// Albums first released on this month and day before this year, the most played first.
    fn released_on(&self, year: i32, month: i32, day: i32) -> Result<Vec<u32>> {
        let rows: Vec<(u32, i64, String)> = self.read(|c| {
            let mut st = c.prepare_cached(
                "SELECT rowid, play_count, data FROM album WHERE
                 coalesce(json_extract(data, '$.originalReleaseDate.month'), json_extract(data, '$.releaseDate.month')) IS NOT NULL",
            )?;
            let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)? as u32, r.get::<_, i64>(1)?, r.get::<_, String>(2)?)))?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })?;
        let mut hits: Vec<(u32, i64, (i32, i32, i32))> = rows
            .into_iter()
            .filter_map(|(r, plays, data)| {
                let album: Album = serde_json::from_str(&data).ok()?;
                let d = first_release(&album)?;
                (d.1 == month && d.2 == day && d.0 < year).then_some((r, plays, d))
            })
            .collect();
        hits.sort_by(|a, b| b.1.cmp(&a.1).then(a.2.cmp(&b.2)).then(a.0.cmp(&b.0)));
        Ok(hits.into_iter().map(|(r, _, _)| r).collect())
    }

    /// Albums played [`STALE_MIN_PLAYS`]+ times but not in the last [`STALE_DAYS`], longest ago first.
    fn stale_albums(&self, now: i64) -> Result<Vec<u32>> {
        self.read(|c| {
            let mut st = c.prepare_cached("SELECT rowid FROM album WHERE play_count >= ?1 AND played IS NOT NULL AND unixepoch(played) < ?2 ORDER BY unixepoch(played) LIMIT 200")?;
            let rows = st.query_map(params![STALE_MIN_PLAYS, now - STALE_DAYS * DAY], |r| r.get::<_, i64>(0))?;
            Ok(rows.map(|r| r.map(|v| v as u32)).collect::<std::result::Result<_, _>>()?)
        })
    }

    /// An artist mix's anchors: the listener's top songs by the artist lately, at most
    /// [`ARTIST_ANCHORS`], else the artist's most played song.
    fn artist_anchors(&self, taste: &Taste, artist_id: &str) -> Result<Vec<Song>> {
        let rows: Vec<u32> = taste.songs.iter().take(600).map(|s| s.rowid).collect();
        let mine: Vec<Song> = distinct(self.songs_by_rowid(&rows)?.into_iter().map(|(_, s)| s).filter(|s| credits(s, artist_id)).collect());
        if !mine.is_empty() {
            return Ok(mine.into_iter().take(ARTIST_ANCHORS).collect());
        }
        let mut own: Vec<Song> = self.songs_by_rowid(&self.artist_album_rowids(artist_id)?)?.into_iter().map(|(_, s)| s).filter(|s| credits(s, artist_id)).collect();
        own.sort_by(|a, b| b.play_count.unwrap_or(0).cmp(&a.play_count.unwrap_or(0)).then(a.id.cmp(&b.id)));
        Ok(own.into_iter().take(1).collect())
    }

    fn genre_seeds(&self, taste: &Taste, genre: &str) -> Result<Vec<Song>> {
        let rows: Vec<u32> = taste.songs.iter().take(600).map(|s| s.rowid).collect();
        let songs = self.songs_by_rowid(&rows)?;
        Ok(distinct(songs.into_iter().map(|(_, s)| s).filter(|s| in_genre(s, genre)).collect()).into_iter().take(GENRE_SEEDS).collect())
    }
}

/// Asks the server, read-only, for what today's page still needs: the top artists' similar
/// artists and today's mixes. A failed ask counts as nothing found, so it waits until tomorrow.
pub async fn refresh_for_you(client: &Client, library: &Library, local_day: i64) -> Result<()> {
    refresh_for_you_with(client, library, local_day, &|| {}).await
}

/// [`refresh_for_you`], calling `changed` each time something new is stored for the page (the
/// similar artists, then each mix as it lands).
pub async fn refresh_for_you_with(client: &Client, library: &Library, local_day: i64, changed: &(dyn Fn() + Send + Sync)) -> Result<()> {
    let _gate = library.for_you_gate().await;
    let taste = library.taste()?;
    let mut asked = false;
    for a in taste.artists.iter().take(TOP_ARTISTS) {
        if library.similar_for_day(&a.id, local_day)?.is_some() {
            continue;
        }
        let ids: Vec<String> = match client.artist_info2(&a.id, Some(30)).await {
            Ok(info) => info.similar_artist.into_iter().map(|x| x.id).filter(|id| !id.is_empty()).collect(),
            Err(e) => {
                tracing::warn!("similar artists for {}: {e}", a.id);
                Vec::new()
            }
        };
        library.store_similar(&a.id, local_day, &ids)?;
        asked = true;
    }
    // Deezer's related artists for New to You, while the listener has Deezer on. They're cached
    // for a week, so most are already there.
    for a in taste.artists.iter().take(TOP_ARTISTS) {
        if let Some(artist) = library.artist(&a.id)? {
            crate::similar_artists::deezer_similar(library, crate::deezer::Deezer::shared(), &artist.name).await;
        }
    }
    if asked {
        changed();
    }
    let stored = library.stored_mixes()?.filter(|s| s.day == local_day);
    if stored.as_ref().is_some_and(|s| s.complete) {
        return Ok(());
    }
    let earlier: HashMap<String, Built> = stored.map(|s| s.built.into_iter().map(|b| (b.mix.id.clone(), b)).collect()).unwrap_or_default();
    make_mixes(client, library, &taste, local_day, &earlier, changed).await
}

/// makes today's mixes in the background: now, then again whenever the local day turns. runs
/// until dropped; `changed` is called as in [`refresh_for_you_with`].
pub async fn keep_mixes_daily(client: Client, library: Library, changed: std::sync::Arc<dyn Fn() + Send + Sync>) {
    let mut made: Option<i64> = None;
    loop {
        let day = taste::local_day(taste::now_unix());
        if made != Some(day) {
            match refresh_for_you_with(&client, &library, day, &*changed).await {
                Ok(()) => made = Some(day),
                Err(e) => tracing::warn!("For You: {e}"),
            }
        }
        tokio::time::sleep(DAY_CHECK).await;
    }
}

/// The engine's list around `anchors`: the server's similar songs and, with Deezer on, Deezer's
/// sources.
async fn engine_plan(client: &Client, library: &Library, anchors: &[Song]) -> Vec<Song> {
    let ids: Vec<String> = anchors.iter().map(|s| s.id.clone()).collect();
    let answers = crate::autoplay::fetch_answers_with(client, library, crate::autoplay::Externals::shared(), &ids, crate::engine::AutoplayMode::Navia).await;
    crate::engine::list_from(Some(library), anchors, answers, PLAN_LEN)
}

/// a mix that could be made today, in the order they're tried.
enum Candidate {
    Artist { id: String, name: String, anchors: Vec<Song>, score: f64 },
    Genre { name: String, seeds: Vec<Song>, score: f64 },
}

impl Candidate {
    fn id(&self) -> String {
        match self {
            Candidate::Artist { id, .. } => format!("artist:{id}"),
            Candidate::Genre { name, .. } => format!("genre:{name}"),
        }
    }

    fn kind(&self) -> MixKind {
        match self {
            Candidate::Artist { .. } => MixKind::Artist,
            Candidate::Genre { .. } => MixKind::Genre,
        }
    }

    async fn make(&self, client: &Client, library: &Library, day: i64) -> Option<Built> {
        let (mix, score) = match self {
            Candidate::Artist { id, name, anchors, score } => {
                let plan = engine_plan(client, library, anchors).await;
                (artist_mix(day, id, name, anchors, &plan)?, *score)
            }
            Candidate::Genre { name, seeds, score } => {
                let (plan, fill) = tokio::join!(engine_plan(client, library, seeds), client.random_songs(GENRE_FILL, Some(name)));
                (genre_mix(day, name, seeds, &plan, &fill.unwrap_or_default())?, *score)
            }
        };
        library_only(library, mix).ok().flatten().map(|mix| Built { mix, score })
    }
}

/// the mix with only songs the library has, so the stored list is what the page shows; `None`
/// when that leaves it too short.
fn library_only(library: &Library, mut mix: Mix) -> Result<Option<Mix>> {
    let rows = library.song_rowids_in_order(&mix.songs)?;
    let have: HashSet<String> = library.songs_by_rowid(&rows)?.into_iter().map(|(_, s)| s.id).collect();
    mix.songs.retain(|id| have.contains(id));
    Ok((mix.songs.len() >= MIX_MIN).then_some(mix))
}

fn candidates(library: &Library, taste: &Taste) -> Result<Vec<Candidate>> {
    let mut out = Vec::new();
    for a in taste.artists.iter().take(ARTIST_MIXES * 3) {
        let Some(artist) = library.artist(&a.id)? else { continue };
        let anchors = library.artist_anchors(taste, &a.id)?;
        if !anchors.is_empty() {
            out.push(Candidate::Artist { id: a.id.clone(), name: artist.name, anchors, score: a.score.max(a.fallback) });
        }
    }
    for g in library.genre_overview()?.iter().filter(|g| g.played).take(GENRE_MIXES * 3) {
        let seeds = library.genre_seeds(taste, &g.name)?;
        if !seeds.is_empty() {
            out.push(Candidate::Genre { name: g.name.clone(), seeds, score: if g.taste > 0.0 { g.taste } else { g.taste_fallback } });
        }
    }
    Ok(out)
}

/// the page's order for the mixes made so far: ranked, with fresh covers.
fn ranked(library: &Library, artists: &[Built], genres: &[Built]) -> Result<Vec<Mix>> {
    let pairs = |list: &[Built]| list.iter().map(|b| (b.mix.clone(), b.score)).collect::<Vec<_>>();
    let ranked = rank_mixes(pairs(artists), pairs(genres));
    let album_of: HashMap<String, String> = {
        let ids: Vec<String> = ranked.iter().flat_map(|m| m.songs.clone()).collect();
        let rows = library.song_rowids(&ids)?;
        library.songs_by_rowid(&rows)?.into_iter().filter_map(|(_, s)| Some((s.id.clone(), s.album_id.clone()?))).collect()
    };
    Ok(fresh_covers(ranked, |id| album_of.get(id).cloned()))
}

fn wanted(kind: MixKind) -> usize {
    if kind == MixKind::Artist { ARTIST_MIXES } else { GENRE_MIXES }
}

fn kept_of(kept: &[AtomicUsize; 2], kind: MixKind) -> &AtomicUsize {
    &kept[usize::from(kind == MixKind::Genre)]
}

async fn try_candidate(c: &Candidate, kept: &[AtomicUsize; 2], earlier: &HashMap<String, Built>, client: &Client, library: &Library, day: i64) -> Option<Built> {
    if kept_of(kept, c.kind()).load(Ordering::SeqCst) >= wanted(c.kind()) {
        return None;
    }
    match earlier.get(&c.id()) {
        Some(b) => Some(b.clone()),
        None => c.make(client, library, day).await,
    }
}

/// makes the day's mixes, [`MIXES_AT_ONCE`] at a time, keeping the first [`ARTIST_MIXES`] and
/// [`GENRE_MIXES`] that work out in the candidates' order (the same ones as one at a time). each is
/// stored as it lands; mixes in `earlier` (from a build cut short today) aren't asked again.
async fn make_mixes(client: &Client, library: &Library, taste: &Taste, day: i64, earlier: &HashMap<String, Built>, changed: &(dyn Fn() + Send + Sync)) -> Result<()> {
    use futures_util::StreamExt;

    let list = candidates(library, taste)?;
    // kept so far by kind. answers come back in the candidates' order, so a candidate whose kind is
    // already full when it starts would never be kept, and isn't asked for.
    let kept = [AtomicUsize::new(0), AtomicUsize::new(0)];
    let futures: Vec<_> = list.iter().map(|c| try_candidate(c, &kept, earlier, client, library, day)).collect();
    let mut answers = futures_util::stream::iter(futures).buffered(MIXES_AT_ONCE);
    let (mut artists, mut genres): (Vec<Built>, Vec<Built>) = (Vec::new(), Vec::new());
    let mut next = list.iter();
    while let Some(made) = answers.next().await {
        let Some(kind) = next.next().map(Candidate::kind) else { break };
        let Some(b) = made else { continue };
        if kept_of(&kept, kind).load(Ordering::SeqCst) >= wanted(kind) {
            continue;
        }
        kept_of(&kept, kind).fetch_add(1, Ordering::SeqCst);
        if kind == MixKind::Artist { artists.push(b) } else { genres.push(b) }
        let built: Vec<Built> = artists.iter().chain(&genres).cloned().collect();
        library.put_mixes(&StoredMixes { day, mixes: ranked(library, &artists, &genres)?, complete: false, built })?;
        changed();
    }
    drop(answers);
    library.put_mixes(&StoredMixes { day, mixes: ranked(library, &artists, &genres)?, complete: true, built: Vec::new() })?;
    changed();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{ArtistRef, ItemDate, NamedRef};

    fn song(id: &str, artist: &str, genre: &str) -> Song {
        Song {
            id: id.into(),
            title: format!("Song {id}"),
            artist: Some(artist.into()),
            artist_id: Some(artist.to_lowercase()),
            artists: vec![ArtistRef { id: artist.to_lowercase(), name: artist.into() }],
            album_id: Some(format!("al-{id}")),
            genres: vec![NamedRef { name: genre.into() }],
            ..Default::default()
        }
    }

    fn many(prefix: &str, artists: &[&str], n: usize, genre: &str) -> Vec<Song> {
        (0..n).map(|i| song(&format!("{prefix}{i}"), artists[i % artists.len()], genre)).collect()
    }

    #[test]
    fn hash_matches_the_reference_values() {
        // Pinned values, so a day's order stays the same across builds.
        assert_eq!(hash("ab"), 1_868_774_452);
        assert_eq!(hash(""), 0);
    }

    #[test]
    fn daily_order_is_stable_for_a_day_and_changes_the_next() {
        let list = many("s", &["A"], 20, "Pop");
        let a = daily(100, "k", &list, |s| &s.id);
        assert_eq!(a, daily(100, "k", &list, |s| &s.id));
        assert_ne!(a, daily(101, "k", &list, |s| &s.id));
    }

    #[test]
    fn artist_mix_is_a_third_own_starts_with_an_anchor_keeps_the_plan_order_and_caps_others() {
        let anchors = many("a", &["Seed"], 3, "Pop");
        let others = many("x", &["B", "C", "D", "E", "F"], 60, "Pop");
        let mut plan = others.clone();
        plan.extend(many("p", &["Seed"], 20, "Pop"));
        let mix = artist_mix(7, "seed", "Seed", &anchors, &plan).unwrap();
        assert_eq!(mix.id, "artist:seed");
        assert_eq!(mix.songs.len(), MIX_SIZE);
        assert_eq!(mix.songs[0], anchors[7 % 3].id, "today's anchor leads");
        assert_eq!(mix.songs.iter().filter(|s| s.starts_with('a') || s.starts_with('p')).count(), 10);
        let picked: Vec<&String> = mix.songs.iter().filter(|s| s.starts_with('x')).collect();
        let order: Vec<&String> = others.iter().map(|s| &s.id).filter(|id| picked.contains(id)).collect();
        assert_eq!(picked, order, "other artists keep the engine's order");
        for a in ["B", "C", "D", "E", "F"] {
            let n = mix.songs.iter().filter(|id| others.iter().any(|s| &s.id == *id && s.artist.as_deref() == Some(a))).count();
            assert!(n <= PER_ARTIST, "{a}: {n}");
        }
    }

    #[test]
    fn artist_mix_needs_other_artists_and_ten_songs() {
        let anchors = many("a", &["Seed"], 3, "Pop");
        // A plan of the artist's own songs only: no similar data anywhere.
        assert!(artist_mix(1, "seed", "Seed", &anchors, &many("p", &["Seed"], 20, "Pop")).is_none());
        // Too few in all.
        assert!(artist_mix(1, "seed", "Seed", &anchors[..1], &many("x", &["B"], 3, "Pop")).is_none());
    }

    #[test]
    fn genre_mix_keeps_the_genre_and_the_plan_order_and_fills_from_it() {
        let seeds = many("s", &["A"], 3, "Jazz");
        let mut plan = many("x", &["B", "C"], 6, "Jazz");
        plan.extend(many("r", &["D"], 6, "Rock"));
        let fill = many("f", &["E", "F", "G", "H"], 30, "Jazz");
        let mix = genre_mix(3, "Jazz", &seeds, &plan, &fill).unwrap();
        assert_eq!(mix.songs.len(), MIX_SIZE);
        assert!(mix.songs[0].starts_with('s'));
        assert!(!mix.songs.iter().any(|s| s.starts_with('r')));
        let from_plan: Vec<&String> = mix.songs.iter().filter(|s| s.starts_with('x')).collect();
        let order: Vec<&String> = plan.iter().map(|s| &s.id).filter(|id| from_plan.contains(id)).collect();
        assert_eq!(from_plan, order);
        assert!(genre_mix(3, "Jazz", &[], &plan, &fill).is_none());
    }

    #[test]
    fn mixes_rank_against_their_own_kind() {
        let m = |id: &str| Mix { id: id.into(), kind: MixKind::Artist, name: id.into(), songs: vec![] };
        let ranked = rank_mixes(vec![(m("a1"), 10.0), (m("a2"), 4.0)], vec![(m("g1"), 300.0), (m("g2"), 240.0)]);
        let ids: Vec<&str> = ranked.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["a1", "g1", "g2", "a2"]);
    }

    #[test]
    fn covers_prefer_albums_earlier_tiles_did_not_use() {
        let m = |id: &str, songs: &[&str]| Mix { id: id.into(), kind: MixKind::Artist, name: id.into(), songs: songs.iter().map(|s| s.to_string()).collect() };
        let album = |s: &str| Some(s[..1].to_string());
        let out = fresh_covers(vec![m("1", &["a1", "b1", "c1", "d1", "e1"]), m("2", &["a2", "b2", "e2", "f2", "g2", "h2"])], album);
        assert_eq!(out[0].songs[..4], ["a1", "b1", "c1", "d1"]);
        assert_eq!(out[1].songs[..4], ["e2", "f2", "g2", "h2"]);
        assert_eq!(out[1].songs.len(), 6);
    }

    #[test]
    fn round_robin_alternates_and_skips_used() {
        let mut used: HashSet<u32> = [2].into();
        assert_eq!(round_robin(&[vec![1, 2, 3], vec![4, 5]], &mut used, 4), [1, 4, 5, 3]);
        assert!(used.contains(&4));
    }

    #[test]
    fn civil_dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_403), (2025, 11, 11));
        assert_eq!(civil(-1), (1969, 12, 31));
        assert_eq!(civil(11_016), (2000, 2, 29));
    }

    #[test]
    fn first_release_prefers_the_original_and_needs_a_day() {
        let d = |y, m, dd| Some(ItemDate { year: Some(y), month: Some(m), day: dd });
        let a = Album { original_release_date: d(1978, 11, Some(10)), release_date: d(2014, 11, Some(4)), ..Default::default() };
        assert_eq!(first_release(&a), Some((1978, 11, 10)));
        let b = Album { original_release_date: d(1978, 11, None), release_date: d(2014, 11, Some(4)), ..Default::default() };
        assert_eq!(first_release(&b), Some((2014, 11, 4)));
        assert_eq!(first_release(&Album::default()), None);
    }
}
