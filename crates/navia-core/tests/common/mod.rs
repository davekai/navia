//! A local fake of the parts of Navidrome the core uses. Tests write only to this, never to the real server.

#![allow(dead_code)]

pub mod audio;
pub mod http;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use wiremock::matchers::path_regex;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use navia_core::client::{Client, ClientConfig, Credentials, WritePolicy};

#[derive(Clone, Debug)]
pub struct FakeSong {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub artist_id: String,
    pub album: String,
    pub album_id: String,
    pub library: String,
    /// More fields for the song's JSON (`isrc`, `duration`…), merged in when it's an object.
    pub extra: Value,
}

#[derive(Default)]
pub struct State {
    pub folders: Vec<(String, String)>,
    pub songs: Vec<FakeSong>,
    pub last_scan: Option<String>,
    pub scanning: bool,
    pub open_subsonic: bool,
    pub extensions: Vec<(String, Vec<i32>)>,
    pub sharing_501: bool,
    /// Cap the page size, as real servers may.
    pub page_cap: Option<usize>,
    /// After this many search3 calls, answer 500 for all further search3 calls.
    pub fail_search_after: Option<usize>,
    pub search_calls: usize,
    /// `search3` calls that asked for songs.
    pub song_pages: usize,
    /// Canned JSON bodies, keyed by "endpoint" or "endpoint:id".
    pub canned: HashMap<String, Value>,
    /// bodies answered in turn, one per call, keyed like `canned` and before it.
    pub turns: HashMap<String, Vec<Value>>,
    pub turn_calls: HashMap<String, usize>,
    /// Song files served by `stream`, by id.
    pub stream: HashMap<String, Vec<u8>>,
    /// Bytes served for getCoverArt.
    pub cover_png: Option<Vec<u8>>,
    /// serve them as `Cache-Control: no-store`, as Navidrome does its placeholder.
    pub cover_no_store: bool,
    /// Every endpoint hit, in order.
    pub calls: Vec<String>,
    /// the album id of every `getAlbum`, in order.
    pub album_calls: Vec<String>,
    /// more fields for an album's entry, by album id.
    pub album_extra: HashMap<String, Value>,
    pub scrobbles: Vec<(String, String, Option<String>)>,
    /// `createPlaylist` calls with a `playlistId`, as (method, playlist id, song ids in order).
    pub playlist_saves: Vec<(String, String, Vec<String>)>,
    /// `updatePlaylist` calls, as (method, playlist id, songs added, positions removed).
    pub playlist_updates: Vec<(String, String, Vec<String>, Vec<usize>)>,
    /// `createPlaylist` calls with a name, as (method, name, song ids).
    pub playlist_creates: Vec<(String, String, Vec<String>)>,
    /// every request in order: how it was sent and where its parameters were.
    pub sent: Vec<Sent>,
}

#[derive(Clone, Debug)]
pub struct Sent {
    pub method: String,
    pub endpoint: String,
    /// the URL's query keys and the form body's keys.
    pub query: Vec<String>,
    pub body: Vec<String>,
    pub content_type: Option<String>,
}

impl Sent {
    /// any of `u`, `t`, `s`, `p` in the URL.
    pub fn credentials_in_url(&self) -> bool {
        self.query.iter().any(|k| matches!(k.as_str(), "u" | "t" | "s" | "p"))
    }

    pub fn credentials_in_body(&self) -> bool {
        ["u", "t", "s"].iter().all(|k| self.body.iter().any(|b| b == k))
    }
}

pub type Shared = Arc<Mutex<State>>;

pub fn song(id: &str, title: &str, artist: &str, album: &str, library: &str) -> FakeSong {
    FakeSong { id: id.into(), title: title.into(), artist: artist.into(), artist_id: format!("ar-{artist}"), album: album.into(), album_id: format!("al-{album}"), library: library.into(), extra: Value::Null }
}

fn ok(body: Value, open_subsonic: bool) -> ResponseTemplate {
    let mut response = json!({ "status": "ok", "version": "1.16.1", "type": "navidrome", "serverVersion": "9.9.9", "openSubsonic": open_subsonic });
    if let (Some(r), Value::Object(m)) = (response.as_object_mut(), body) {
        r.extend(m);
    }
    ResponseTemplate::new(200).set_body_json(json!({ "subsonic-response": response }))
}

/// Parameters from the URL and from a POST form body.
fn all_pairs(request: &Request) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = request.url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
    pairs.extend(url::form_urlencoded::parse(&request.body).map(|(k, v)| (k.into_owned(), v.into_owned())));
    pairs
}

fn song_json(s: &FakeSong) -> Value {
    let mut v = json!({
        "id": s.id, "title": s.title, "album": s.album, "albumId": s.album_id, "artist": s.artist,
        "artistId": s.artist_id, "displayArtist": s.artist, "track": 1, "discNumber": 1
    });
    if let (Some(m), Value::Object(extra)) = (v.as_object_mut(), &s.extra) {
        m.extend(extra.clone());
    }
    v
}

/// an album's list entry as Navidrome sends it: song count and duration follow its songs, plus any
/// `album_extra` for it (a cover hash, plays).
fn album_entry(st: &State, first: &FakeSong) -> Value {
    let songs: Vec<&FakeSong> = st.songs.iter().filter(|s| s.album_id == first.album_id).collect();
    let duration: i64 = songs.iter().map(|s| s.extra.get("duration").and_then(Value::as_i64).unwrap_or(0)).sum();
    let mut v = json!({"id": first.album_id, "name": first.album, "artist": first.artist, "artistId": first.artist_id, "songCount": songs.len(), "duration": duration});
    if let (Some(m), Some(Value::Object(extra))) = (v.as_object_mut(), st.album_extra.get(&first.album_id)) {
        m.extend(extra.clone());
    }
    v
}

struct Fake(Shared);

impl Respond for Fake {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let endpoint = request.url.path().rsplit('/').next().unwrap_or_default().to_string();
        let every = all_pairs(request);
        let param = |k: &str| every.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone());
        let mut st = self.0.lock().unwrap();
        st.calls.push(endpoint.clone());
        st.sent.push(Sent {
            method: request.method.to_string(),
            endpoint: endpoint.clone(),
            query: request.url.query_pairs().map(|(k, _)| k.into_owned()).collect(),
            body: url::form_urlencoded::parse(&request.body).map(|(k, _)| k.into_owned()).collect(),
            content_type: request.headers.get("content-type").and_then(|v| v.to_str().ok()).map(str::to_string),
        });
        let os = st.open_subsonic;
        match endpoint.as_str() {
            "ping" => ok(json!({}), os),
            "getOpenSubsonicExtensions" => {
                let list: Vec<Value> = st.extensions.iter().map(|(n, v)| json!({"name": n, "versions": v})).collect();
                ok(json!({"openSubsonicExtensions": list}), os)
            }
            "getMusicFolders" => {
                let list: Vec<Value> = st.folders.iter().map(|(id, name)| json!({"id": id.parse::<i64>().unwrap_or(0), "name": name})).collect();
                ok(json!({"musicFolders": {"musicFolder": list}}), os)
            }
            "getScanStatus" => {
                let mut s = json!({"scanning": st.scanning, "count": st.songs.len()});
                if let Some(ls) = &st.last_scan {
                    s["lastScan"] = json!(ls);
                }
                ok(json!({"scanStatus": s}), os)
            }
            "getUser" => ok(json!({"user": {"username": "tester", "shareRole": true, "folder": [1]}}), os),
            "getShares" => {
                if st.sharing_501 {
                    ResponseTemplate::new(501).set_body_string("not implemented")
                } else {
                    ok(json!({"shares": {}}), os)
                }
            }
            "getArtistInfo2" => ok(json!({"artistInfo2": {"biography": "Bio."}}), os),
            "getTopSongs" => ok(json!({"topSongs": {"song": [{"id": "x", "title": "T"}]}}), os),
            "search3" => {
                st.search_calls += 1;
                if param("songCount").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1) > 0 {
                    st.song_pages += 1;
                }
                if st.fail_search_after.is_some_and(|n| st.search_calls > n) {
                    return ResponseTemplate::new(500).set_body_string("boom");
                }
                let folder = param("musicFolderId");
                let in_folder = |s: &&FakeSong| folder.as_deref().is_none_or(|f| s.library == f);
                let count = |k: &str, def: usize| param(k).and_then(|v| v.parse().ok()).unwrap_or(def);
                let cap = st.page_cap.unwrap_or(usize::MAX);
                let mut out = json!({});
                let songs: Vec<&FakeSong> = st.songs.iter().filter(in_folder).collect();
                let (sc, so) = (count("songCount", 20).min(cap), count("songOffset", 0));
                if sc > 0 {
                    let page: Vec<Value> = songs.iter().skip(so).take(sc).map(|s| song_json(s)).collect();
                    if !page.is_empty() {
                        out["song"] = json!(page);
                    }
                }
                let mut albums: Vec<&FakeSong> = songs.clone();
                albums.dedup_by_key(|s| s.album_id.clone());
                let (ac, ao) = (count("albumCount", 20).min(cap), count("albumOffset", 0));
                if ac > 0 {
                    let page: Vec<Value> = albums.iter().skip(ao).take(ac).map(|s| album_entry(&st, s)).collect();
                    if !page.is_empty() {
                        out["album"] = json!(page);
                    }
                }
                let mut artists: Vec<&FakeSong> = songs.clone();
                artists.sort_by_key(|s| s.artist_id.clone());
                artists.dedup_by_key(|s| s.artist_id.clone());
                let (rc, ro) = (count("artistCount", 20).min(cap), count("artistOffset", 0));
                if rc > 0 {
                    let page: Vec<Value> = artists.iter().skip(ro).take(rc).map(|s| json!({"id": s.artist_id, "name": s.artist})).collect();
                    if !page.is_empty() {
                        out["artist"] = json!(page);
                    }
                }
                ok(json!({"searchResult3": out}), os)
            }
            "getAlbum" if !st.canned.contains_key(&format!("getAlbum:{}", param("id").unwrap_or_default())) => {
                let id = param("id").unwrap_or_default();
                st.album_calls.push(id.clone());
                let Some(first) = st.songs.iter().find(|s| s.album_id == id) else {
                    return ResponseTemplate::new(200).set_body_json(json!({"subsonic-response": {"status": "failed", "error": {"code": 70, "message": "Album not found"}}}));
                };
                let mut album = album_entry(&st, first);
                let songs: Vec<Value> = st.songs.iter().filter(|s| s.album_id == id).map(song_json).collect();
                album["song"] = json!(songs);
                ok(json!({"album": album}), os)
            }
            "scrobble" => {
                st.scrobbles.push((param("id").unwrap_or_default(), param("submission").unwrap_or_default(), param("time")));
                ok(json!({}), os)
            }
            // With a name, a new playlist of those songs, owned by the tester.
            "createPlaylist" if all_pairs(request).iter().any(|(k, _)| k == "name") => {
                let pairs = all_pairs(request);
                let name = pairs.iter().find(|(k, _)| k == "name").map(|(_, v)| v.clone()).unwrap_or_default();
                let songs: Vec<String> = pairs.iter().filter(|(k, _)| k == "songId").map(|(_, v)| v.clone()).collect();
                st.playlist_creates.push((request.method.to_string(), name.clone(), songs.clone()));
                let id = format!("new-{}", st.playlist_creates.len());
                let entries: Vec<Value> = songs.iter().map(|s| json!({"id": s, "title": format!("Song {s}")})).collect();
                let playlist = json!({"playlist": {"id": id, "name": name, "owner": "tester", "readonly": false, "songCount": songs.len(), "changed": "2026-01-01T00:00:00Z", "entry": entries}});
                st.canned.insert(format!("getPlaylist:{id}"), playlist.clone());
                ok(playlist, os)
            }
            // Appends and removes by position in the list before this call, as Navidrome does.
            "updatePlaylist" => {
                let pairs = all_pairs(request);
                let id = pairs.iter().find(|(k, _)| k == "playlistId").map(|(_, v)| v.clone()).unwrap_or_default();
                let adds: Vec<String> = pairs.iter().filter(|(k, _)| k == "songIdToAdd").map(|(_, v)| v.clone()).collect();
                let removes: Vec<usize> = pairs.iter().filter(|(k, _)| k == "songIndexToRemove").filter_map(|(_, v)| v.parse().ok()).collect();
                st.playlist_updates.push((request.method.to_string(), id.clone(), adds.clone(), removes.clone()));
                let key = format!("getPlaylist:{id}");
                let Some(mut playlist) = st.canned.get(&key).cloned() else { return ResponseTemplate::new(200).set_body_json(json!({"subsonic-response": {"status": "failed", "error": {"code": 70, "message": "not found"}}})) };
                let mut entries: Vec<Value> = playlist["playlist"]["entry"].as_array().cloned().unwrap_or_default();
                entries = entries.into_iter().enumerate().filter(|(i, _)| !removes.contains(i)).map(|(_, e)| e).collect();
                entries.extend(adds.iter().map(|s| json!({"id": s, "title": format!("Song {s}")})));
                playlist["playlist"]["songCount"] = json!(entries.len());
                playlist["playlist"]["entry"] = json!(entries);
                st.canned.insert(key, playlist);
                ok(json!({}), os)
            }
            // With a playlistId, replace that playlist's songs, as Navidrome does, from the URL or a POST form.
            "createPlaylist" if param("playlistId").is_some() || request.method.as_str() == "POST" => {
                let pairs = all_pairs(request);
                let id = pairs.iter().find(|(k, _)| k == "playlistId").map(|(_, v)| v.clone()).unwrap_or_default();
                let songs: Vec<String> = pairs.iter().filter(|(k, _)| k == "songId").map(|(_, v)| v.clone()).collect();
                st.playlist_saves.push((request.method.to_string(), id.clone(), songs.clone()));
                let key = format!("getPlaylist:{id}");
                let Some(mut playlist) = st.canned.get(&key).cloned() else { return ResponseTemplate::new(200).set_body_json(json!({"subsonic-response": {"status": "failed", "error": {"code": 70, "message": "not found"}}})) };
                let entries: Vec<Value> = songs.iter().map(|s| json!({"id": s, "title": format!("Song {s}")})).collect();
                playlist["playlist"]["entry"] = json!(entries);
                playlist["playlist"]["songCount"] = json!(songs.len());
                st.canned.insert(key, playlist.clone());
                ok(playlist, os)
            }
            "stream" => match param("id").and_then(|id| st.stream.get(&id).cloned()) {
                Some(bytes) => ResponseTemplate::new(200).insert_header("Content-Type", "audio/wav").set_body_bytes(bytes),
                None => ResponseTemplate::new(404),
            },
            "getCoverArt" => match &st.cover_png {
                Some(png) if st.cover_no_store => ResponseTemplate::new(200).insert_header("Content-Type", "image/png").insert_header("Cache-Control", "no-store").set_body_bytes(png.clone()),
                Some(png) => ResponseTemplate::new(200).insert_header("Content-Type", "image/png").set_body_bytes(png.clone()),
                None => ResponseTemplate::new(404),
            },
            other => {
                let key = param("id").map(|id| format!("{other}:{id}"));
                let turn = [key.clone(), Some(other.to_string())].into_iter().flatten().find(|k| st.turns.contains_key(k));
                if let Some(k) = turn {
                    let n = st.turn_calls.entry(k.clone()).or_insert(0);
                    let at = *n;
                    *n += 1;
                    let list = &st.turns[&k];
                    return ok(list[at % list.len()].clone(), os);
                }
                let body = key.and_then(|k| st.canned.get(&k).cloned()).or_else(|| st.canned.get(other).cloned()).unwrap_or_else(|| json!({}));
                ok(body, os)
            }
        }
    }
}

pub struct FakeServer {
    pub server: MockServer,
    pub state: Shared,
}

impl FakeServer {
    pub async fn start(state: State) -> Self {
        let state = Arc::new(Mutex::new(state));
        let server = MockServer::start().await;
        Mock::given(path_regex(r"^/rest/.*$")).respond_with(Fake(state.clone())).mount(&server).await;
        Self { server, state }
    }

    pub fn client(&self) -> Client {
        Client::new(ClientConfig::new(self.server.uri(), Credentials::new("tester", "letmein"))).unwrap()
    }

    pub fn read_only_client(&self) -> Client {
        let mut cfg = ClientConfig::new(self.server.uri(), Credentials::new("tester", "letmein"));
        cfg.write_policy = WritePolicy::ReadOnly;
        Client::new(cfg).unwrap()
    }

    pub fn sent(&self) -> Vec<Sent> {
        self.state.lock().unwrap().sent.clone()
    }

    pub fn calls(&self, endpoint: &str) -> usize {
        self.state.lock().unwrap().calls.iter().filter(|c| *c == endpoint).count()
    }
}
