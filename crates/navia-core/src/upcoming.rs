//! Deezer's announced and just-released albums by library artists, only when [`crate::deezer::gate`]
//! is open. only ids, names, dates and lengths are kept, never images.
//!
//! the server has a release when an album by the same artist has its title without edition notes,
//! or was added around the release with most of its songs, or carries its barcode (only
//! Navidrome's native API has that tag, and asking logs in, so it comes last).

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::time::Instant;

use crate::client::{Album, Client, Song};
use crate::deezer::{Deezer, gate, store, stored, touch};
use crate::lanyard::matching::{album_key, find_named, find_named_or_bare, key, names, title_key};
use crate::library::Library;
use crate::library::taste::{local_day, now_unix, utc_offset};
use crate::smart_editor::preview::days_from_iso;

/// an album the server never gets leaves this many days after its release date.
pub const OUT_NOW_DAYS: i64 = 14;
/// an album on the server counts as new this many days from its release date.
pub const NEW_RELEASE_DAYS: i64 = 7;
/// top artists by recent taste whose releases home shows.
pub const HOME_ARTISTS: u32 = 10;

const DAY_MS: i64 = 86_400_000;
const DAY_SECS: i64 = 86_400;
const CHECK_KEEP_MS: i64 = DAY_MS;
/// Deezer lists the newest first.
const RELEASES_LIMIT: u32 = 25;
/// Deezer's other kinds are "single" and "compile".
const RECORD_TYPES: &[&str] = &["album", "ep"];
const VARIOUS_ARTISTS: &str = "various artists";
/// library songs from a release that show it belongs to the artist, when others perform it.
const OWNED_SONGS: usize = 2;
const REQUEST_TIME: Duration = Duration::from_secs(20);
/// early leaks and time zones.
const ADDED_BEFORE_SECS: i64 = 7 * DAY_SECS;
const COVER_CDN: &str = "https://cdn-images.dzcdn.net/images/cover";

const ARTIST_PREFIX: &str = "upcoming:artist:";
const ALBUM_PREFIX: &str = "upcoming:album:";
const SERVER_PREFIX: &str = "upcoming:server:";

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct AnnouncedTrack {
    pub id: u64,
    pub title: String,
    pub artist: String,
    pub duration_s: u64,
    pub explicit: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct UpcomingAlbum {
    /// Deezer's album id.
    pub id: u64,
    pub title: String,
    pub artist: String,
    /// the library artist it was found for.
    pub artist_id: String,
    pub cover: String,
    /// `YYYY-MM-DD`, local.
    pub release_date: String,
    /// songs announced in all; `tracks` lists those named so far.
    pub total: u32,
    pub upc: Option<String>,
    pub explicit: bool,
    pub record_type: String,
    pub tracks: Vec<AnnouncedTrack>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PreviewTrack {
    pub track: AnnouncedTrack,
    pub song: Option<(u32, Song)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum UpcomingPage {
    Upcoming { album: UpcomingAlbum, tracks: Vec<PreviewTrack> },
    /// the server's copy.
    OnServer(String),
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
struct Release {
    id: u64,
    title: String,
    release_date: String,
    record_type: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
struct ArtistReleases {
    releases: Vec<Release>,
}

/// local midnight of the release day, Unix seconds.
pub fn release_moment(date: &str) -> Option<i64> {
    let utc_midnight = days_from_iso(date)? * DAY_SECS;
    Some(utc_midnight - utc_offset(utc_midnight))
}

/// `today` in days since 1970; negative before the release.
pub fn days_since_release(date: &str, today: i64) -> Option<i64> {
    Some(today - days_from_iso(date)?)
}

pub fn in_window(date: &str, today: i64) -> bool {
    days_since_release(date, today).is_some_and(|d| d <= OUT_NOW_DAYS)
}

/// by the first-release date, so a reissue isn't new.
pub fn is_new_release(album: &Album, today: i64) -> bool {
    release_day(album).is_some_and(|day| (0..NEW_RELEASE_DAYS).contains(&(today - day)))
}

/// days since 1970, full dates only.
pub fn release_day(album: &Album) -> Option<i64> {
    let (y, m, d) = [&album.original_release_date, &album.release_date].into_iter().flatten().find_map(|d| Some((d.year?, d.month?, d.day?)).filter(|(y, m, d)| *y > 0 && *m > 0 && *d > 0))?;
    days_from_iso(&format!("{y:04}-{m:02}-{d:02}"))
}

fn today() -> i64 {
    local_day(now_unix())
}

fn now_ms() -> i64 {
    crate::engine::playlog::now_ms()
}

/// barcodes compared as digits, so a 12-digit UPC equals its 13-digit EAN.
fn same_barcode(a: &str, b: &str) -> bool {
    let digits = |s: &str| s.chars().filter(char::is_ascii_digit).collect::<String>().trim_start_matches('0').to_string();
    let (a, b) = (digits(a), digits(b));
    !a.is_empty() && a == b
}

fn kept<T: for<'de> Deserialize<'de>>(db: &Library, key: &str) -> Option<(i64, T)> {
    stored(db, key).and_then(|(at, v)| serde_json::from_str(&v).ok().map(|t| (at, t)))
}

fn deadline() -> Instant {
    Instant::now() + REQUEST_TIME
}

/// a failing Deezer serves what was kept.
async fn releases(db: &Library, deezer: &Deezer, artist_id: &str, name: &str, ask: bool) -> Vec<Release> {
    let k = format!("{ARTIST_PREFIX}{artist_id}");
    let now = now_ms();
    let old = kept::<ArtistReleases>(db, &k);
    if let Some((at, found)) = &old
        && (now - at < CHECK_KEEP_MS || !ask)
    {
        touch(db, &k, now);
        return found.releases.clone();
    }
    if !ask {
        return Vec::new();
    }
    let Some(deezer_artist) = crate::engine::deezer_src::artist(db, deezer, name, deadline()).await else {
        return old.map(|(_, f)| f.releases).unwrap_or_default();
    };
    let query = [("limit", RELEASES_LIMIT.to_string())];
    match deezer.get(&format!("/artist/{deezer_artist}/albums"), &query, deadline()).await {
        Ok(body) => {
            let releases: Vec<Release> = data(&body);
            store(db, &k, &serde_json::to_string(&ArtistReleases { releases: releases.clone() }).unwrap_or_default(), now);
            releases
        }
        Err(_) => old.map(|(_, f)| f.releases).unwrap_or_default(),
    }
}

fn data<T: for<'de> Deserialize<'de>>(body: &Value) -> Vec<T> {
    body.get("data").and_then(Value::as_array).map(|items| items.iter().filter_map(|v| serde_json::from_value(v.clone()).ok()).collect()).unwrap_or_default()
}

fn parse_album(body: &Value, artist_id: &str) -> Option<UpcomingAlbum> {
    let text = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    let id = body.get("id").and_then(Value::as_u64).filter(|id| *id > 0)?;
    let tracks = body.pointer("/tracks/data").and_then(Value::as_array).cloned().unwrap_or_default();
    let tracks = tracks
        .iter()
        .filter_map(|t| {
            Some(AnnouncedTrack {
                id: t.get("id").and_then(Value::as_u64).filter(|id| *id > 0)?,
                title: text(t, "title"),
                artist: t.get("artist").map(|a| text(a, "name")).unwrap_or_default(),
                duration_s: t.get("duration").and_then(Value::as_u64).unwrap_or_default(),
                explicit: t.get("explicit_lyrics").and_then(Value::as_bool).unwrap_or(false),
            })
        })
        .collect();
    Some(UpcomingAlbum {
        id,
        title: text(body, "title"),
        artist: body.get("artist").map(|a| text(a, "name")).unwrap_or_default(),
        artist_id: artist_id.to_string(),
        cover: text(body, "md5_image"),
        release_date: text(body, "release_date"),
        total: body.get("nb_tracks").and_then(Value::as_u64).unwrap_or_default() as u32,
        upc: body.get("upc").and_then(Value::as_str).map(str::trim).filter(|u| !u.is_empty()).map(str::to_string),
        explicit: body.get("explicit_lyrics").and_then(Value::as_bool).unwrap_or(false),
        record_type: text(body, "record_type"),
        tracks,
    })
}

/// asked again daily: more songs get named as the date nears.
async fn album_details(db: &Library, deezer: &Deezer, id: u64, artist_id: &str, ask: bool) -> Option<UpcomingAlbum> {
    let k = format!("{ALBUM_PREFIX}{id}");
    let now = now_ms();
    let old = kept::<UpcomingAlbum>(db, &k);
    if let Some((at, album)) = &old
        && (now - at < CHECK_KEEP_MS || !ask)
    {
        touch(db, &k, now);
        return Some(album.clone());
    }
    if !ask {
        return None;
    }
    let artist_id = old.as_ref().map(|(_, a)| a.artist_id.clone()).filter(|a| !a.is_empty()).unwrap_or_else(|| artist_id.to_string());
    match deezer.get(&format!("/album/{id}"), &[], deadline()).await.ok().and_then(|body| parse_album(&body, &artist_id)) {
        Some(album) => {
            // a page opened before its artist's list knows no artist: kept, it would hide the album there
            if !album.artist_id.is_empty() {
                store(db, &k, &serde_json::to_string(&album).unwrap_or_default(), now);
            }
            Some(album)
        }
        None => old.map(|(_, a)| a),
    }
}

struct Candidate {
    id: String,
    name: String,
    added: Option<i64>,
}

fn same_artist_albums(db: &Library, album: &UpcomingAlbum) -> Vec<Candidate> {
    let wanted = key(&album.artist);
    type Row = (String, String, Option<String>, Option<String>, Option<String>, Option<String>);
    let rows: Vec<Row> = db
        .read(|c| {
            let mut st = c.prepare_cached("SELECT id, name, json_extract(data, '$.version'), artist, artist_id, created FROM album WHERE artist_id = ?1 OR artist = ?2 COLLATE NOCASE")?;
            let rows = st.query_map([album.artist_id.as_str(), album.artist.as_str()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .unwrap_or_default();
    rows.into_iter()
        .filter(|(_, _, _, artist, artist_id, _)| artist_id.as_deref() == Some(album.artist_id.as_str()) || artist.as_deref().is_some_and(|a| key(a) == wanted))
        // Navidrome appends the version to the name.
        .map(|(id, name, version, _, _, created)| Candidate { id, name: crate::display::album_name(&name, version.as_deref()).into_owned(), added: created.and_then(|c| crate::queue_sync::parse_iso8601_ms(&c)).map(|ms| (ms / 1000) as i64) })
        .collect()
}

fn barcodes() -> &'static Mutex<HashMap<String, Option<String>>> {
    static KNOWN: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    KNOWN.get_or_init(Mutex::default)
}

async fn barcode(client: &Client, album_id: &str) -> Option<String> {
    if let Some(known) = barcodes().lock().unwrap().get(album_id) {
        return known.clone();
    }
    let found = client.album_barcode(album_id).await.ok()?;
    barcodes().lock().unwrap().insert(album_id.to_string(), found.clone());
    found
}

fn covers_tracks(album: &UpcomingAlbum, songs: &[Song]) -> bool {
    let wanted: HashSet<String> = album.tracks.iter().map(|t| title_key(&t.title)).filter(|t| !t.is_empty()).collect();
    if wanted.len() < 2 {
        return false;
    }
    let have: HashSet<String> = songs.iter().map(|s| title_key(&s.title)).collect();
    wanted.iter().filter(|t| have.contains(*t)).count() * 2 > wanted.len()
}

/// `client` is for the barcode step; without it the other two decide.
pub async fn server_copy(client: Option<&Client>, db: &Library, album: &UpcomingAlbum) -> Option<String> {
    let k = format!("{SERVER_PREFIX}{}", album.id);
    if let Some((_, id)) = stored(db, &k)
        && db.album(&id).ok().flatten().is_some()
    {
        return Some(id);
    }
    let found = local_copy(db, album);
    let found = match (found, client, &album.upc) {
        (Some(id), _, _) => Some(id),
        (None, Some(client), Some(upc)) => {
            let mut hit = None;
            for c in recent(db, album) {
                if barcode(client, &c.id).await.is_some_and(|b| same_barcode(&b, upc)) {
                    hit = Some(c.id);
                    break;
                }
            }
            hit
        }
        _ => None,
    };
    if let Some(id) = &found {
        store(db, &k, id, now_ms());
    }
    found
}

fn recent(db: &Library, album: &UpcomingAlbum) -> Vec<Candidate> {
    let from = release_moment(&album.release_date).map_or(i64::MAX, |m| m - ADDED_BEFORE_SECS);
    same_artist_albums(db, album).into_iter().filter(|c| c.added.is_some_and(|a| a >= from)).collect()
}

fn local_copy(db: &Library, album: &UpcomingAlbum) -> Option<String> {
    let title = album_key(&album.title);
    let candidates = same_artist_albums(db, album);
    if let Some(c) = candidates.iter().find(|c| !title.is_empty() && album_key(&c.name) == title) {
        return Some(c.id.clone());
    }
    let from = release_moment(&album.release_date).map_or(i64::MAX, |m| m - ADDED_BEFORE_SECS);
    candidates.into_iter().filter(|c| c.added.is_some_and(|a| a >= from)).find(|c| covers_tracks(album, &db.album_songs(&c.id).unwrap_or_default())).map(|c| c.id)
}

fn known_copy(db: &Library, album: &UpcomingAlbum) -> bool {
    stored(db, &format!("{SERVER_PREFIX}{}", album.id)).is_some_and(|(_, id)| db.album(&id).ok().flatten().is_some()) || local_copy(db, album).is_some()
}

/// an album or EP filed under the library artist.
fn filed_under(album: &UpcomingAlbum, library_artist: &str) -> bool {
    let album_artist = key(&album.artist);
    if (!album.record_type.is_empty() && !RECORD_TYPES.contains(&album.record_type.as_str())) || album_artist == key(VARIOUS_ARTISTS) {
        return false;
    }
    album_artist.is_empty() || album_artist == key(library_artist)
}

/// Deezer files some label sets under a composer as their albums; songs each by someone else give them away.
fn performs(album: &UpcomingAlbum) -> bool {
    let album_artist = key(&album.artist);
    let by: Vec<String> = album.tracks.iter().map(|t| key(&t.artist)).filter(|a| !a.is_empty()).collect();
    let others: HashSet<&String> = by.iter().filter(|a| **a != album_artist).collect();
    by.len() < 2 || by.iter().any(|a| *a == album_artist) || others.len() * 2 <= by.len()
}

/// a soundtrack is performed by others too; the library crediting its songs to the artist tells it from a label's set.
fn has_songs_from(db: &Library, album: &UpcomingAlbum, library_artist: &str) -> bool {
    let artist = key(library_artist);
    let credited = |song: &Song| song.artist.as_deref().is_some_and(|credit| names(credit).contains(&artist));
    let found = album.tracks.iter().filter(|t| !t.artist.is_empty()).filter(|t| {
        find_named_or_bare(db, &t.title, &[t.artist.as_str()], &album.title, Some(t.duration_s * 1000).filter(|ms| *ms > 0)).is_some_and(|(_, song)| credited(&song))
    });
    found.take(OWNED_SONGS).count() == OWNED_SONGS
}

fn artists_own(db: &Library, album: &UpcomingAlbum, library_artist: &str) -> bool {
    filed_under(album, library_artist) && (performs(album) || has_songs_from(db, album, library_artist))
}

/// `ask`: false only reads what's kept (test runs).
pub async fn for_artist(client: Option<&Client>, db: &Library, deezer: &Deezer, artist_id: &str, ask: bool) -> Vec<UpcomingAlbum> {
    if !gate(db).is_open() {
        return Vec::new();
    }
    let Some(artist) = db.artist(artist_id).ok().flatten() else { return Vec::new() };
    let today = today();
    let mut out = Vec::new();
    for release in releases(db, deezer, artist_id, &artist.name, ask).await {
        if !RECORD_TYPES.contains(&release.record_type.as_str()) || !in_window(&release.release_date, today) {
            continue;
        }
        let Some(album) = album_details(db, deezer, release.id, artist_id, ask).await else { continue };
        if in_window(&album.release_date, today) && artists_own(db, &album, &artist.name) && server_copy(client, db, &album).await.is_none() {
            out.push(album);
        }
    }
    out.sort_by(|a, b| a.release_date.cmp(&b.release_date).then(a.id.cmp(&b.id)));
    out
}

pub async fn for_top_artists(client: Option<&Client>, db: &Library, deezer: &Deezer, ask: bool) -> Vec<UpcomingAlbum> {
    if !gate(db).is_open() {
        return Vec::new();
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for ranked in db.taste_artists(Some(HOME_ARTISTS)).unwrap_or_default() {
        for album in for_artist(client, db, deezer, &ranked.artist.id, ask).await {
            if seen.insert(album.id) {
                out.push(album);
            }
        }
    }
    out.sort_by(|a, b| a.release_date.cmp(&b.release_date).then(a.id.cmp(&b.id)));
    out
}

/// never asks Deezer, so a query isn't sent anywhere.
pub fn kept_matching(db: &Library, query: &str) -> Vec<UpcomingAlbum> {
    let words: Vec<String> = key(query).split(' ').filter(|w| !w.is_empty()).map(str::to_string).collect();
    if words.is_empty() || !gate(db).is_open() {
        return Vec::new();
    }
    let today = today();
    // a key range, so the primary key's index finds them.
    let (from, to) = (ALBUM_PREFIX.to_string(), format!("{}\u{10FFFF}", ALBUM_PREFIX));
    let values: Vec<String> = db
        .read(|c| {
            let mut st = c.prepare_cached("SELECT value FROM deezer_cache WHERE key >= ?1 AND key < ?2")?;
            let rows = st.query_map([from.as_str(), to.as_str()], |r| r.get(0))?;
            Ok(rows.collect::<rusqlite::Result<Vec<String>>>()?)
        })
        .unwrap_or_default();
    let mut out: Vec<UpcomingAlbum> = values
        .iter()
        .filter_map(|v| serde_json::from_str::<UpcomingAlbum>(v).ok())
        .filter(|a| in_window(&a.release_date, today))
        .filter(|a| db.artist(&a.artist_id).ok().flatten().is_some_and(|artist| artists_own(db, a, &artist.name)))
        .filter(|a| {
            let text = format!(" {} ", key(&format!("{} {}", a.title, a.artist)));
            words.iter().all(|w| text.contains(&format!(" {w}")))
        })
        .filter(|a| !known_copy(db, a))
        .collect();
    out.sort_by(|a, b| a.release_date.cmp(&b.release_date).then(a.id.cmp(&b.id)));
    out
}

pub fn preview_tracks(db: &Library, album: &UpcomingAlbum) -> Vec<PreviewTrack> {
    album
        .tracks
        .iter()
        .map(|t| {
            let artists = [t.artist.as_str(), album.artist.as_str()];
            let song = find_named(db, &t.title, &artists[..if t.artist.is_empty() { 2 } else { 1 }], &album.title, Some(t.duration_s * 1000).filter(|ms| *ms > 0));
            PreviewTrack { track: t.clone(), song }
        })
        .collect()
}

pub async fn page(client: Option<&Client>, db: &Library, deezer: &Deezer, id: u64, ask: bool) -> Option<UpcomingPage> {
    if !gate(db).is_open() {
        return None;
    }
    let album = album_details(db, deezer, id, "", ask).await?;
    if let Some(copy) = server_copy(client, db, &album).await {
        return Some(UpcomingPage::OnServer(copy));
    }
    let tracks = preview_tracks(db, &album);
    Some(UpcomingPage::Upcoming { album, tracks })
}

pub fn kept_album(db: &Library, id: u64) -> Option<UpcomingAlbum> {
    kept::<UpcomingAlbum>(db, &format!("{ALBUM_PREFIX}{id}")).map(|(_, a)| a)
}

pub fn cover_url(image: &str, px: u32) -> Option<String> {
    if image.is_empty() || !image.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let size = [250u32, 500, 1000].into_iter().find(|s| *s >= px).unwrap_or(1000);
    Some(format!("{COVER_CDN}/{image}/{size}x{size}-000000-80-0-0.jpg"))
}

/// never stored (Deezer's terms).
pub async fn cover(db: &Library, deezer: &Deezer, image: &str, px: u32) -> Option<Vec<u8>> {
    let url = cover_url(image, px).filter(|_| gate(db).is_open())?;
    let response = tokio::time::timeout(REQUEST_TIME, deezer.http().get(url).send()).await.ok()?.ok()?;
    if !response.status().is_success() {
        return None;
    }
    response.bytes().await.ok().map(|b| b.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{ArtistRef, ItemDate, MusicFolder};
    use crate::library::LibraryConfig;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn iso_day(day: i64) -> String {
        crate::smart_editor::preview::iso_from_unix(day * DAY_SECS)
    }

    fn song(id: &str, title: &str, artist: &str, album: &str, secs: i64) -> Song {
        Song {
            id: id.into(),
            title: title.into(),
            artist: Some(artist.into()),
            artists: vec![ArtistRef { id: "ar-1".into(), name: artist.into() }],
            artist_id: Some("ar-1".into()),
            album_id: Some(album.into()),
            album: Some(album.into()),
            duration: Some(secs),
            ..Default::default()
        }
    }

    fn library(albums: &[Album], songs: &[Song]) -> (tempfile::TempDir, Library) {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open(&LibraryConfig { path: dir.path().join("lib.sqlite") }).unwrap();
        lib.replace_libraries(&[MusicFolder { id: "1".into(), name: "L".into() }]).unwrap();
        let cur = lib.begin_sync("1", None, false).unwrap();
        lib.apply_artists("1", &cur, 1, &[crate::client::Artist { id: "ar-1".into(), name: "Seed Band".into(), ..Default::default() }]).unwrap();
        lib.apply_albums("1", &cur, albums.len() as u32, albums).unwrap();
        lib.apply_songs("1", &cur, songs.len() as u32, songs).unwrap();
        crate::deezer::set_setting(&lib, Some(true)).unwrap();
        (dir, lib)
    }

    fn server_album(id: &str, name: &str, created: &str) -> Album {
        Album { id: id.into(), name: name.into(), artist: Some("Seed Band".into()), artist_id: Some("ar-1".into()), created: Some(created.into()), ..Default::default() }
    }

    fn announced(date: &str) -> UpcomingAlbum {
        UpcomingAlbum {
            id: 77,
            title: "Night Drive (Deluxe)".into(),
            artist: "Seed Band".into(),
            artist_id: "ar-1".into(),
            cover: "abc123".into(),
            release_date: date.into(),
            total: 4,
            upc: Some("075679556431".into()),
            explicit: false,
            record_type: "album".into(),
            tracks: vec![
                AnnouncedTrack { id: 1, title: "Alpha".into(), artist: "Seed Band".into(), duration_s: 200, explicit: false },
                AnnouncedTrack { id: 2, title: "Beta (feat. Guest)".into(), artist: "Seed Band".into(), duration_s: 210, explicit: true },
                AnnouncedTrack { id: 3, title: "Gamma".into(), artist: "Seed Band".into(), duration_s: 220, explicit: false },
            ],
        }
    }

    #[test]
    fn the_window_runs_until_out_now_days_after_the_release() {
        let today = 20_000;
        assert!(in_window(&iso_day(today + 30), today), "announced");
        assert!(in_window(&iso_day(today), today), "out today");
        assert!(in_window(&iso_day(today - OUT_NOW_DAYS), today), "the last day");
        assert!(!in_window(&iso_day(today - OUT_NOW_DAYS - 1), today), "gone");
        assert!(!in_window("not a date", today));
        assert_eq!(days_since_release(&iso_day(today - 3), today), Some(3));
    }

    #[test]
    fn new_release_for_seven_days_from_the_first_release() {
        let today = 20_000;
        let on = |day: i64| {
            let (y, m, d) = { let s = iso_day(day); (s[..4].parse().unwrap(), s[5..7].parse().unwrap(), s[8..10].parse().unwrap()) };
            Some(ItemDate { year: Some(y), month: Some(m), day: Some(d) })
        };
        let album = |released: i64| Album { release_date: on(released), ..Default::default() };
        assert!(is_new_release(&album(today), today));
        assert!(is_new_release(&album(today - NEW_RELEASE_DAYS + 1), today));
        assert!(!is_new_release(&album(today - NEW_RELEASE_DAYS), today));
        assert!(!is_new_release(&album(today + 1), today), "not before its date");
        let reissue = Album { original_release_date: on(today - 9000), release_date: on(today), ..Default::default() };
        assert!(!is_new_release(&reissue, today), "a reissue isn't new");
        let year_only = Album { release_date: Some(ItemDate { year: Some(2026), month: None, day: None }), ..Default::default() };
        assert!(!is_new_release(&year_only, today));
    }

    #[test]
    fn release_moment_is_local_midnight() {
        let at = release_moment("2026-11-20").unwrap();
        assert_eq!((at + utc_offset(at)).rem_euclid(DAY_SECS), 0);
    }

    #[test]
    fn compilations_and_other_artists_albums_are_left_out() {
        let (_d, db) = library(&[], &[]);
        let own = announced("2026-11-20");
        assert!(artists_own(&db, &own, "Seed Band"));
        assert!(artists_own(&db, &own, "The Seed Band"), "names compared as keys");
        assert!(!artists_own(&db, &UpcomingAlbum { record_type: "compile".into(), ..own.clone() }, "Seed Band"));
        assert!(!artists_own(&db, &UpcomingAlbum { record_type: "single".into(), ..own.clone() }, "Seed Band"));
        assert!(!artists_own(&db, &UpcomingAlbum { artist: "Various Artists".into(), ..own.clone() }, "Seed Band"));
        assert!(!artists_own(&db, &UpcomingAlbum { artist: "Someone Else".into(), ..own.clone() }, "Seed Band"));
        let study_set = UpcomingAlbum { tracks: (1..=6).map(|i| performer(i, &format!("Pianist {i}"))).collect(), ..own.clone() };
        assert!(!artists_own(&db, &study_set, "Seed Band"), "a label's set filed under the artist: every song by someone else");
        let recording = UpcomingAlbum { tracks: (1..=6).map(|i| performer(i, "Chamber Orchestra")).collect(), ..own.clone() };
        assert!(artists_own(&db, &recording, "Seed Band"), "one performer throughout is still the artist's album");
    }

    fn performer(id: u64, artist: &str) -> AnnouncedTrack {
        AnnouncedTrack { id, title: format!("Piece {id}"), artist: artist.into(), duration_s: 30, explicit: false }
    }

    #[test]
    fn a_soundtrack_shows_once_the_library_credits_two_of_its_songs_to_the_artist() {
        let soundtrack = UpcomingAlbum { tracks: (1..=6).map(|i| performer(i, &format!("Singer {i}"))).collect(), ..announced("2026-11-20") };
        let single = |i: u64, credit: &str| song(&format!("s{i}"), &format!("Piece {i} (from Night Drive)"), credit, "al", 30);
        let album = server_album("al", "Earlier", "2020-01-01T00:00:00Z");
        let (_d, two) = library(&[album.clone()], &[single(1, "Singer 1 • Seed Band"), single(2, "Singer 2 • Seed Band")]);
        assert!(artists_own(&two, &soundtrack, "Seed Band"));
        let (_d, one) = library(&[album.clone()], &[single(1, "Singer 1 • Seed Band")]);
        assert!(!artists_own(&one, &soundtrack, "Seed Band"), "one song isn't enough");
        let (_d, uncredited) = library(&[album.clone()], &[single(1, "Singer 1"), single(2, "Singer 2")]);
        assert!(!artists_own(&uncredited, &soundtrack, "Seed Band"), "the same songs without the artist's credit");
        let (_d, other_takes) = library(&[album], &[single(1, "Seed Band"), single(2, "Seed Band")]);
        assert!(!artists_own(&other_takes, &soundtrack, "Seed Band"), "the same titles by another performer");
    }

    #[test]
    fn barcodes_match_as_digits() {
        assert!(same_barcode("075679556431", "0075679556431"));
        assert!(same_barcode(" 75679556431", "075679556431"));
        assert!(!same_barcode("075679556431", "075679556432"));
        assert!(!same_barcode("", ""));
    }

    #[test]
    fn the_server_copy_by_name_without_edition_notes() {
        let (_d, db) = library(&[server_album("old", "First Light", "2020-01-01T00:00:00Z"), server_album("new", "Night Drive", "2026-11-19T10:00:00Z")], &[]);
        assert_eq!(local_copy(&db, &announced("2026-11-20")).as_deref(), Some("new"));
        let single = UpcomingAlbum { title: "Night Drive - Single".into(), ..announced("2026-11-20") };
        assert_eq!(local_copy(&db, &single).as_deref(), Some("new"));
        let reissue = UpcomingAlbum { title: "Night Drive (25th Anniversary Edition)".into(), ..announced("2026-11-20") };
        assert_eq!(local_copy(&db, &reissue).as_deref(), Some("new"));
        let explicit = UpcomingAlbum { title: "Night Drive [Explicit]".into(), ..announced("2026-11-20") };
        assert_eq!(local_copy(&db, &explicit).as_deref(), Some("new"));
        let other = UpcomingAlbum { title: "Morning Walk".into(), ..announced("2026-11-20") };
        assert_eq!(local_copy(&db, &other), None);
    }

    #[test]
    fn the_server_copy_by_its_shown_name() {
        let hires = Album { version: Some("192kHz/24bit".into()), ..server_album("hires", "Night Drive (192kHz/24bit)", "2020-01-01T00:00:00Z") };
        let (_d, db) = library(&[hires], &[]);
        assert_eq!(local_copy(&db, &announced("2026-11-20")).as_deref(), Some("hires"), "without the version Navidrome appends");
    }

    #[test]
    fn the_server_copy_by_tracks_only_when_added_around_the_release() {
        let tracks = |album: &str| vec![song(&format!("{album}-a"), "Alpha", "Seed Band", album, 200), song(&format!("{album}-b"), "Beta", "Seed Band", album, 210), song(&format!("{album}-x"), "Other", "Seed Band", album, 180)];
        let mut songs = tracks("renamed");
        songs.extend(tracks("older"));
        let albums = [server_album("renamed", "ND (Tour Edition) Vol. 1", "2026-11-18T09:00:00Z"), server_album("older", "Greatest", "2019-05-01T00:00:00Z")];
        let (_d, db) = library(&albums, &songs);
        assert_eq!(local_copy(&db, &announced("2026-11-20")).as_deref(), Some("renamed"), "two of three titles, the feature credit ignored");
        let (_d2, db2) = library(&albums[1..], &songs[3..]);
        assert_eq!(local_copy(&db2, &announced("2026-11-20")), None, "added years before: not the release");
    }

    #[test]
    fn preview_songs_land_on_the_library() {
        let (_d, db) = library(&[server_album("al", "Earlier", "2020-01-01T00:00:00Z")], &[song("alpha", "Alpha", "Seed Band", "al", 201), song("beta", "Beta", "Seed Band", "al", 210)]);
        let preview = preview_tracks(&db, &announced("2026-11-20"));
        let owned: Vec<Option<&str>> = preview.iter().map(|p| p.song.as_ref().map(|(_, s)| s.id.as_str())).collect();
        assert_eq!(owned, [Some("alpha"), Some("beta"), None]);
    }

    async fn deezer(release_date: &str, past: &str) -> MockServer {
        let server = MockServer::start().await;
        let album = |id: u64, title: &str, date: &str, kind: &str| json!({"id": id, "title": title, "release_date": date, "record_type": kind});
        Mock::given(method("GET")).and(path("/search/artist")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [{"id": 1, "name": "Seed Band", "nb_fan": 10}]}))).mount(&server).await;
        Mock::given(method("GET"))
            .and(path("/artist/1/albums"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [album(77, "Night Drive", release_date, "album"), album(78, "Lead Single", release_date, "single"), album(80, "Label Hits", release_date, "compile"), album(79, "Long Gone", past, "album")]})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/album/77"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 77, "title": "Night Drive", "upc": "075679556431", "md5_image": "abc123", "release_date": release_date, "nb_tracks": 4, "explicit_lyrics": true,
                "artist": {"id": 1, "name": "Seed Band"},
                "tracks": {"data": [{"id": 1, "title": "Alpha", "duration": 200, "artist": {"name": "Seed Band"}}, {"id": 2, "title": "Unheard", "duration": 190, "explicit_lyrics": true, "artist": {"name": "Seed Band"}}]}
            })))
            .mount(&server)
            .await;
        server
    }

    fn client(server: &MockServer) -> Deezer {
        Deezer::new(server.uri()).with_timing(Duration::from_millis(1), Duration::from_secs(5))
    }

    async fn paths(server: &MockServer) -> Vec<String> {
        server.received_requests().await.unwrap().iter().map(|r| r.url.path().to_string()).collect()
    }

    #[tokio::test]
    async fn an_artists_albums_once_a_day_and_singles_and_old_ones_left_out() {
        let (_d, db) = library(&[server_album("al", "Earlier", "2020-01-01T00:00:00Z")], &[song("alpha", "Alpha", "Seed Band", "al", 200)]);
        let today = today();
        let server = deezer(&iso_day(today + 5), &iso_day(today - OUT_NOW_DAYS - 1)).await;
        let dz = client(&server);
        let found = for_artist(None, &db, &dz, "ar-1", true).await;
        assert_eq!(found.iter().map(|a| a.id).collect::<Vec<_>>(), [77]);
        assert_eq!((found[0].total, found[0].tracks.len(), found[0].artist_id.as_str()), (4, 2, "ar-1"));
        let asked = paths(&server).await.len();
        assert_eq!(asked, 3, "artist, releases, one album: the single and the compilation aren't asked for");
        assert_eq!(for_artist(None, &db, &dz, "ar-1", true).await, found);
        assert_eq!(paths(&server).await.len(), asked, "kept for a day");
        assert!(stored(&db, "upcoming:album:77").is_some());
        assert_eq!(kept_matching(&db, "night se").iter().map(|a| a.id).collect::<Vec<_>>(), [77], "title and artist words");
        assert!(kept_matching(&db, "nothing like it").is_empty());
        match page(None, &db, &dz, 77, false).await {
            Some(UpcomingPage::Upcoming { tracks, .. }) => assert_eq!(tracks.iter().map(|t| t.song.is_some()).collect::<Vec<_>>(), [true, false]),
            other => panic!("{other:?}"),
        }
        crate::deezer::set_setting(&db, Some(false)).unwrap();
        assert!(for_artist(None, &db, &dz, "ar-1", true).await.is_empty(), "nothing with the switch off");
        assert!(kept_matching(&db, "night").is_empty());
        assert_eq!(page(None, &db, &dz, 77, true).await, None);
    }

    #[tokio::test]
    async fn a_page_opened_first_keeps_nothing_without_the_artist() {
        let (_d, db) = library(&[], &[]);
        let today = today();
        let server = deezer(&iso_day(today + 5), &iso_day(today - OUT_NOW_DAYS - 1)).await;
        let dz = client(&server);
        assert!(matches!(page(None, &db, &dz, 77, true).await, Some(UpcomingPage::Upcoming { .. })));
        assert!(stored(&db, "upcoming:album:77").is_none());
        let found = for_artist(None, &db, &dz, "ar-1", true).await;
        assert_eq!(found.iter().map(|a| (a.id, a.artist_id.as_str())).collect::<Vec<_>>(), [(77, "ar-1")]);
    }

    #[tokio::test]
    async fn the_shelf_drops_an_album_once_the_server_has_it() {
        let (_d, db) = library(&[server_album("copy", "Night Drive", "2026-11-19T10:00:00Z")], &[]);
        let today = today();
        let server = deezer(&iso_day(today - 1), &iso_day(today - 30)).await;
        let dz = client(&server);
        assert!(for_artist(None, &db, &dz, "ar-1", true).await.is_empty());
        assert_eq!(page(None, &db, &dz, 77, false).await, Some(UpcomingPage::OnServer("copy".into())));
        assert!(kept_matching(&db, "night").is_empty());
    }

    #[tokio::test]
    async fn without_asking_only_kept_answers() {
        let (_d, db) = library(&[], &[]);
        let server = deezer(&iso_day(today() + 5), "2000-01-01").await;
        let dz = client(&server);
        assert!(for_artist(None, &db, &dz, "ar-1", false).await.is_empty());
        assert!(paths(&server).await.is_empty());
    }
}
