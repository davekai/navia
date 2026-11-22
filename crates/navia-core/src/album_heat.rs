//! the album page's Heat column: each song's Deezer `rank` on the album's Deezer release, only
//! when [`crate::deezer::gate`] is open. the Deezer album is found by one song's ISRC, else by an
//! album search on the album artist and title, and taken only when it covers at least half the
//! album's songs. kept in the library's `deezer_cache` (ids and ranks only); a kept answer older
//! than its keep time still serves while Deezer fails.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use tokio::time::Instant;

use crate::client::Song;
use crate::deezer::{AlbumTrack, Deezer, DeezerError, gate, store, stored, touch};
use crate::lanyard::matching::key;
use crate::library::Library;

const DAY_MS: i64 = 86_400_000;
const HEAT_KEEP_MS: i64 = 7 * DAY_MS;
/// an album Deezer lacks is asked again after this.
const HEAT_MISS_KEEP_MS: i64 = DAY_MS;
/// nothing waits on the answer, so the lookup takes its turn behind every other Deezer request.
const LOOKUP_TIME: Duration = Duration::from_secs(600);
const KEY_PREFIX: &str = "album_heat:";

fn cache_key(album_id: &str) -> String {
    format!("{KEY_PREFIX}{album_id}")
}

fn parse(value: &str) -> HashMap<String, u64> {
    serde_json::from_str(value).unwrap_or_default()
}

/// library song id → Deezer `rank` for an album's songs, asking Deezer when nothing fresh is kept.
/// songs Deezer doesn't have on the album are absent; empty with the gate closed (no request).
pub async fn album_heat(db: &Library, deezer: &Deezer, album_id: &str) -> HashMap<String, u64> {
    if !gate(db).is_open() {
        return HashMap::new();
    }
    let k = cache_key(album_id);
    let now = crate::engine::playlog::now_ms();
    let stale = match stored(db, &k) {
        Some((at, value)) => {
            let heat = parse(&value);
            let keep = if heat.is_empty() { HEAT_MISS_KEEP_MS } else { HEAT_KEEP_MS };
            if now - at < keep {
                touch(db, &k, now);
                return heat;
            }
            Some(heat)
        }
        None => None,
    };
    let songs = db.album_songs(album_id).unwrap_or_default();
    if songs.is_empty() {
        return stale.unwrap_or_default();
    }
    match look_up(db, deezer, album_id, &songs, Instant::now() + LOOKUP_TIME).await {
        Ok(heat) => {
            store(db, &k, &serde_json::to_string(&heat).unwrap_or_default(), now);
            heat
        }
        Err(_) => stale.unwrap_or_default(),
    }
}

/// what [`album_heat`] kept for an album (any age; no request); `None` when nothing is kept.
pub fn kept_album_heat(db: &Library, album_id: &str) -> Option<HashMap<String, u64>> {
    stored(db, &cache_key(album_id)).map(|(_, value)| parse(&value))
}

async fn look_up(db: &Library, deezer: &Deezer, album_id: &str, songs: &[Song], deadline: Instant) -> Result<HashMap<String, u64>, DeezerError> {
    let mut tried = HashSet::new();
    if let Some(isrc) = songs.iter().find_map(|s| s.isrc.iter().find(|i| !i.trim().is_empty())) {
        match deezer.track_by_isrc(isrc.trim(), deadline).await {
            Ok(body) => {
                if let Some(id) = body.pointer("/album/id").and_then(serde_json::Value::as_u64).filter(|id| *id > 0) {
                    tried.insert(id);
                    let heat = match_tracks(songs, &deezer.album_tracks(id, deadline).await?);
                    if covers(&heat, songs) {
                        return Ok(heat);
                    }
                }
            }
            Err(DeezerError::Api { .. }) => {}
            Err(e) => return Err(e),
        }
    }
    let album = db.album(album_id).ok().flatten();
    let title = album.as_ref().map(|a| a.name.clone()).or_else(|| songs[0].album.clone()).unwrap_or_default();
    let artist = album.and_then(|a| a.artist).or_else(|| songs[0].artist.clone()).unwrap_or_default();
    let (title_key, artist_key) = (key(&title), key(&artist));
    if title_key.is_empty() || artist_key.is_empty() {
        return Ok(HashMap::new());
    }
    let found = deezer.search_album(&artist, &title, deadline).await?;
    let Some(hit) = found.into_iter().find(|a| !tried.contains(&a.id) && key(&a.title) == title_key && key(&a.artist.name) == artist_key) else {
        return Ok(HashMap::new());
    };
    let heat = match_tracks(songs, &deezer.album_tracks(hit.id, deadline).await?);
    Ok(if covers(&heat, songs) { heat } else { HashMap::new() })
}

/// at least half the album's songs found, so a single or another release isn't taken for it.
fn covers(heat: &HashMap<String, u64>, songs: &[Song]) -> bool {
    !heat.is_empty() && heat.len() * 2 >= songs.len()
}

/// library song id → rank: by ISRC first, then by normalized title, ties going to the same disc
/// and track position, then the closest length.
fn match_tracks(songs: &[Song], tracks: &[AlbumTrack]) -> HashMap<String, u64> {
    let mut used = vec![false; tracks.len()];
    let mut heat = HashMap::new();
    let isrc_of = |t: &AlbumTrack| t.isrc.as_deref().map(|i| i.trim().to_ascii_uppercase()).filter(|i| !i.is_empty());
    for song in songs {
        let wanted: Vec<String> = song.isrc.iter().map(|i| i.trim().to_ascii_uppercase()).filter(|i| !i.is_empty()).collect();
        if let Some(at) = tracks.iter().enumerate().position(|(i, t)| !used[i] && isrc_of(t).is_some_and(|x| wanted.contains(&x))) {
            used[at] = true;
            heat.insert(song.id.clone(), tracks[at].rank);
        }
    }
    for song in songs {
        if heat.contains_key(&song.id) {
            continue;
        }
        let title = key(&song.title);
        if title.is_empty() {
            continue;
        }
        let place = (song.disc_number.unwrap_or(1).max(1) as u32, song.track.map(|t| t as u32));
        let best = tracks
            .iter()
            .enumerate()
            .filter(|(i, t)| !used[*i] && (key(&t.title) == title || key(&t.title_short) == title))
            .min_by_key(|(_, t)| {
                let same_place = place.1.is_some() && (t.disk_number.unwrap_or(1).max(1), t.track_position) == place;
                let gap = match (song.duration, t.duration) {
                    (Some(a), Some(b)) => a.abs_diff(b as i64),
                    _ => u64::MAX,
                };
                (!same_place, gap)
            });
        if let Some((at, t)) = best {
            used[at] = true;
            heat.insert(song.id.clone(), t.rank);
        }
    }
    heat
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{Album, MusicFolder};
    use crate::library::LibraryConfig;
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn song(id: &str, title: &str, track: i32, secs: i64, isrc: &[&str]) -> Song {
        Song {
            id: id.into(),
            title: title.into(),
            album: Some("First Light".into()),
            artist: Some("Seed Band".into()),
            album_id: Some("al".into()),
            track: Some(track),
            disc_number: Some(1),
            duration: Some(secs),
            isrc: isrc.iter().map(|i| i.to_string()).collect(),
            ..Default::default()
        }
    }

    fn library(songs: &[Song], open: bool) -> (tempfile::TempDir, Library) {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open(&LibraryConfig { path: dir.path().join("lib.sqlite") }).unwrap();
        lib.replace_libraries(&[MusicFolder { id: "1".into(), name: "L".into() }]).unwrap();
        let cur = lib.begin_sync("1", None, false).unwrap();
        lib.apply_albums("1", &cur, 1, &[Album { id: "al".into(), name: "First Light".into(), artist: Some("Seed Band".into()), ..Default::default() }]).unwrap();
        lib.apply_songs("1", &cur, 2, songs).unwrap();
        crate::deezer::set_setting(&lib, Some(open)).unwrap();
        (dir, lib)
    }

    fn tracks() -> serde_json::Value {
        json!({"data": [
            {"id": 11, "title": "Opening", "title_short": "Opening", "duration": 200, "rank": 500000, "isrc": "USAAA0000001", "track_position": 1, "disk_number": 1},
            {"id": 12, "title": "Middle (Remastered)", "title_short": "Middle", "duration": 210, "rank": 800000, "isrc": "USAAA0000002", "track_position": 2, "disk_number": 1},
            {"id": 13, "title": "Closer", "title_short": "Closer", "duration": 240, "rank": 300000, "isrc": "USAAA0000003", "track_position": 3, "disk_number": 1}
        ], "total": 3})
    }

    async fn deezer(isrc_found: bool) -> MockServer {
        let server = MockServer::start().await;
        let isrc = if isrc_found {
            ResponseTemplate::new(200).set_body_json(json!({"id": 11, "album": {"id": 77, "title": "First Light"}}))
        } else {
            ResponseTemplate::new(200).set_body_json(json!({"error": {"type": "DataException", "message": "no data", "code": 800}}))
        };
        Mock::given(method("GET")).and(path("/track/isrc:USAAA0000001")).respond_with(isrc).mount(&server).await;
        Mock::given(method("GET"))
            .and(path("/search/album"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [{"id": 76, "title": "First Light (Live)", "artist": {"id": 1, "name": "Seed Band"}}, {"id": 77, "title": "First Light", "artist": {"id": 1, "name": "Seed Band"}}]})))
            .mount(&server)
            .await;
        Mock::given(method("GET")).and(path("/album/77/tracks")).and(query_param("index", "0")).respond_with(ResponseTemplate::new(200).set_body_json(tracks())).mount(&server).await;
        server
    }

    fn client(server: &MockServer) -> Deezer {
        Deezer::new(server.uri()).with_timing(Duration::from_millis(1), Duration::from_secs(5))
    }

    async fn paths(server: &MockServer) -> Vec<String> {
        server.received_requests().await.unwrap().iter().map(|r| r.url.path().to_string()).collect()
    }

    #[tokio::test]
    async fn found_by_isrc_and_matched_by_isrc_then_title() {
        let songs = [song("a", "Opening", 1, 200, &["USAAA0000001"]), song("b", "Middle", 2, 211, &[]), song("c", "Closer", 3, 240, &["usaaa0000003"])];
        let (_d, db) = library(&songs, true);
        let server = deezer(true).await;
        let heat = album_heat(&db, &client(&server), "al").await;
        assert_eq!(heat, HashMap::from([("a".to_string(), 500000), ("b".to_string(), 800000), ("c".to_string(), 300000)]));
        assert_eq!(paths(&server).await, ["/track/isrc:USAAA0000001", "/album/77/tracks"]);
        assert_eq!(kept_album_heat(&db, "al"), Some(heat));
    }

    #[tokio::test]
    async fn found_by_album_search_when_no_isrc_answers() {
        let songs = [song("a", "Opening", 1, 200, &["USAAA0000001"]), song("b", "Middle", 2, 210, &[])];
        let (_d, db) = library(&songs, true);
        let server = deezer(false).await;
        let heat = album_heat(&db, &client(&server), "al").await;
        assert_eq!(heat.len(), 2);
        assert_eq!(paths(&server).await, ["/track/isrc:USAAA0000001", "/search/album", "/album/77/tracks"], "the live album isn't taken");
    }

    #[tokio::test]
    async fn a_kept_answer_asks_nothing_again() {
        let songs = [song("a", "Opening", 1, 200, &["USAAA0000001"])];
        let (_d, db) = library(&songs, true);
        let server = deezer(true).await;
        let dz = client(&server);
        let first = album_heat(&db, &dz, "al").await;
        let asked = paths(&server).await.len();
        assert_eq!(album_heat(&db, &dz, "al").await, first);
        assert_eq!(paths(&server).await.len(), asked);
    }

    #[tokio::test]
    async fn a_stale_answer_serves_when_deezer_fails() {
        let songs = [song("a", "Opening", 1, 200, &["USAAA0000001"])];
        let (_d, db) = library(&songs, true);
        let server = deezer(true).await;
        let first = album_heat(&db, &client(&server), "al").await;
        let _ = db.write_quiet(|tx| {
            tx.execute("UPDATE deezer_cache SET fetched_ms = fetched_ms - 30 * 86400000", [])?;
            Ok(())
        });
        let gone = Deezer::new("http://127.0.0.1:9").with_timing(Duration::from_millis(1), Duration::from_secs(2));
        assert_eq!(album_heat(&db, &gone, "al").await, first);
    }

    #[tokio::test]
    async fn the_closed_gate_asks_nothing() {
        let songs = [song("a", "Opening", 1, 200, &["USAAA0000001"])];
        let (_d, db) = library(&songs, false);
        let server = deezer(true).await;
        assert!(album_heat(&db, &client(&server), "al").await.is_empty());
        assert!(paths(&server).await.is_empty());
        assert_eq!(kept_album_heat(&db, "al"), None);
    }

    #[tokio::test]
    async fn unmatched_songs_are_absent_and_a_miss_is_kept() {
        let songs = [song("a", "Opening", 1, 200, &["USAAA0000001"]), song("b", "Middle", 2, 210, &[]), song("x", "Bonus Demo", 4, 180, &[])];
        let (_d, db) = library(&songs, true);
        let server = deezer(true).await;
        let heat = album_heat(&db, &client(&server), "al").await;
        assert_eq!(heat.len(), 2);
        assert!(!heat.contains_key("x"));

        let lone = [song("z", "Nowhere", 1, 100, &[])];
        let (_d2, db2) = library(&lone, true);
        let server2 = deezer(true).await;
        let dz = client(&server2);
        assert!(album_heat(&db2, &dz, "al").await.is_empty());
        let asked = paths(&server2).await.len();
        assert!(album_heat(&db2, &dz, "al").await.is_empty());
        assert_eq!(paths(&server2).await.len(), asked, "the miss is kept");
        assert_eq!(kept_album_heat(&db2, "al"), Some(HashMap::new()));
    }

    #[test]
    fn position_breaks_a_title_tie() {
        let songs = [song("a", "Interlude", 2, 60, &[]), song("b", "Interlude", 4, 60, &[])];
        let tracks: Vec<AlbumTrack> = [(1, 2, 10), (2, 4, 40)].iter().map(|&(id, pos, rank)| AlbumTrack { id, title: "Interlude".into(), title_short: "Interlude".into(), duration: Some(60), rank, track_position: Some(pos), disk_number: Some(1), ..Default::default() }).collect();
        assert_eq!(match_tracks(&songs, &tracks), HashMap::from([("a".to_string(), 10), ("b".to_string(), 40)]));
    }
}
