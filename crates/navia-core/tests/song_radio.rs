//! Song Radio and Search radio follow the Autoplay mode. The Navia mode runs Autoplay's engine on the song,
//! and the Server mode plays the server's similar songs in order.

mod common;

use std::time::Duration;

use common::{FakeServer, FakeSong, State, song};
use navia_core::autoplay::{Externals, radio_with};
use navia_core::deezer::Deezer;
use navia_core::engine::{self, AutoplayMode};
use navia_core::library::sync::{SyncOptions, Syncer};
use navia_core::library::{Library, LibraryConfig};
use serde_json::{Value, json};
use wiremock::MockServer;

/// (id, title, artist, album, seconds) in the server's similar order, after the seed.
const SIMILAR: &[(&str, &str, &str, &str, u64)] = &[
    ("fa1", "Near One", "Far Away", "Second", 200),
    ("fa2", "Near Two", "Far Away", "Third", 210),
    ("fa3", "Near Three", "Far Away", "Fourth", 220),
    ("inst", "Other Song (Instrumental)", "Band Inst", "Inst Album", 200),
    ("short", "Interlude", "Band Short", "Short Album", 30),
    ("seedv", "Seed Song - Remastered 2011", "Seed Band", "Best Of", 200),
    ("o1", "One", "Act 1", "Album 1", 200),
    ("o2", "Two", "Act 2", "Album 2", 200),
    ("o3", "Three", "Act 3", "Album 3", 200),
    ("o4", "Four", "Act 4", "Album 4", 200),
    ("o5", "Five", "Act 5", "Album 5", 200),
    ("o6", "Six", "Act 6", "Album 6", 200),
    ("o7", "Seven", "Act 7", "Album 7", 200),
    ("o8", "Eight", "Act 8", "Album 8", 200),
];

fn with(mut s: FakeSong, extra: Value) -> FakeSong {
    s.extra = extra;
    s
}

struct Setup {
    server: FakeServer,
    deezer: MockServer,
    library: Library,
    _dir: tempfile::TempDir,
}

async fn setup() -> Setup {
    let mut songs = vec![with(song("seed", "Seed Song", "Seed Band", "First", "1"), json!({"duration": 200, "genre": "Rock", "year": 1990}))];
    let mut similar = Vec::new();
    for (id, title, artist, album, secs) in SIMILAR {
        songs.push(with(song(id, title, artist, album, "1"), json!({"duration": secs, "genre": "Rock", "year": 1991})));
        similar.push(json!({"id": id, "title": title, "artist": artist, "artistId": format!("ar-{artist}"), "artists": [{"id": format!("ar-{artist}"), "name": artist}], "album": album, "albumId": format!("al-{album}"), "duration": secs, "genre": "Rock", "year": 1991}));
    }
    let state = State { folders: vec![("1".into(), "Music".into())], open_subsonic: true, last_scan: Some("2026-01-01T00:00:00Z".into()), songs, ..Default::default() };
    let server = FakeServer::start(state).await;
    let dir = tempfile::tempdir().unwrap();
    let library = Library::open(&LibraryConfig { path: dir.path().join("l.sqlite") }).unwrap();
    Syncer::new(server.client(), library.clone(), SyncOptions::default()).full_sync(&|_| {}).await.unwrap();
    server.state.lock().unwrap().canned.insert("getSimilarSongs2".to_string(), json!({"similarSongs2": {"song": similar}}));
    Setup { server, deezer: MockServer::start().await, library, _dir: dir }
}

fn no_listenbrainz() -> navia_core::listenbrainz::ListenBrainz {
    navia_core::listenbrainz::ListenBrainz::new("http://127.0.0.1:9")
}

fn fast(deezer: &MockServer) -> Deezer {
    Deezer::new(deezer.uri()).with_timing(Duration::from_millis(1), Duration::from_secs(5))
}

fn artist_of(id: &str) -> &'static str {
    if id == "seed" { "Seed Band" } else { SIMILAR.iter().find(|s| s.0 == id).unwrap().2 }
}

#[tokio::test]
async fn a_search_radio_in_the_navia_mode_is_ranked_spaced_and_left_out_like_autoplay() {
    let s = setup().await;
    assert_eq!(engine::mode(&s.library), AutoplayMode::Navia, "Navia is the default");
    let client = s.server.read_only_client();
    for _ in 0..5 {
        let radio = radio_with(&client, &s.library, Externals { deezer: &fast(&s.deezer), listenbrainz: &no_listenbrainz() }, "seed").await;
        let ids: Vec<&str> = radio.iter().map(|s| s.id.as_str()).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(sorted, ["fa1", "fa2", "fa3", "o1", "o2", "o3", "o4", "o5", "o6", "o7", "o8"], "no seed, other version, instrumental or short track: {ids:?}");
        // The seed counts as the song before the radio. Spacing gives way at the end, once only one artist's
        // songs are left, so check the start.
        let order: Vec<&str> = std::iter::once("seed").chain(ids.iter().copied()).take(9).collect();
        for (i, id) in order.iter().enumerate() {
            for later in order.iter().skip(i + 1).take(engine::sequence::ARTIST_GAP) {
                assert_ne!(artist_of(id), artist_of(later), "an artist within {} songs: {order:?}", engine::sequence::ARTIST_GAP);
            }
        }
        assert_ne!(ids[..3], ["fa1", "fa2", "fa3"], "not the server's order");
    }
    assert!(s.deezer.received_requests().await.unwrap().is_empty(), "the Deezer switch is off");
}

#[tokio::test]
async fn a_radio_in_the_server_mode_is_the_servers_order_as_before() {
    let s = setup().await;
    engine::set_mode(&s.library, AutoplayMode::Server).unwrap();
    let radio = radio_with(&s.server.read_only_client(), &s.library, Externals { deezer: &fast(&s.deezer), listenbrainz: &no_listenbrainz() }, "seed").await;
    let ids: Vec<&str> = radio.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["fa1", "fa2", "fa3", "inst", "short", "o1", "o2", "o3", "o4", "o5", "o6", "o7", "o8"]);
    assert!(s.deezer.received_requests().await.unwrap().is_empty());
}
