//! The Navia Autoplay mode with Deezer's sources, against fakes of Navidrome and Deezer.

mod common;

use std::time::Duration;

use common::{FakeServer, FakeSong, State, song};
use navia_core::autoplay::{Autoplay, Externals, fetch_answers_with};
use navia_core::deezer::{self, Deezer};
use navia_core::engine::AutoplayMode;
use navia_core::library::sync::{SyncOptions, Syncer};
use navia_core::library::{Library, LibraryConfig};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn with(mut s: FakeSong, extra: Value) -> FakeSong {
    s.extra = extra;
    s
}

fn track(id: u64, title: &str, artist: &str, secs: u64) -> Value {
    json!({"id": id, "title": title, "duration": secs, "artist": {"id": 1, "name": artist}, "album": {"title": "Any"}})
}

struct Setup {
    server: FakeServer,
    deezer: MockServer,
    library: Library,
    _dir: tempfile::TempDir,
}

async fn setup(opted_in: bool) -> Setup {
    let songs = vec![
        with(song("seed", "Seed Song", "Seed Band", "First", "1"), json!({"duration": 200, "genre": "Rock", "year": 1990})),
        with(song("srv", "Server Pick", "Far Away", "Second", "1"), json!({"duration": 200, "genre": "Rock", "year": 1991})),
        with(song("dz", "Deezer Pick", "Other Act", "Third", "1"), json!({"duration": 230, "genre": "Rock", "year": 1990})),
    ];
    let state = State { folders: vec![("1".into(), "Music".into())], open_subsonic: true, last_scan: Some("2026-01-01T00:00:00Z".into()), songs, ..Default::default() };
    let server = FakeServer::start(state).await;
    let dir = tempfile::tempdir().unwrap();
    let library = Library::open(&LibraryConfig { path: dir.path().join("l.sqlite") }).unwrap();
    Syncer::new(server.client(), library.clone(), SyncOptions::default()).full_sync(&|_| {}).await.unwrap();
    deezer::set_setting(&library, Some(opted_in)).unwrap();
    server.state.lock().unwrap().canned.insert("getSimilarSongs2".to_string(), json!({"similarSongs2": {"song": [{"id": "srv", "title": "Server Pick", "artist": "Far Away", "duration": 200, "genre": "Rock", "year": 1991}]}}));
    let deezer = MockServer::start().await;
    Mock::given(method("GET")).and(path("/search/artist")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [{"id": 1, "name": "Seed Band", "nb_fan": 10}]}))).mount(&deezer).await;
    Mock::given(method("GET")).and(path("/artist/1/radio")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [track(9002, "Deezer Pick", "Other Act", 231)]}))).mount(&deezer).await;
    Mock::given(method("GET")).and(path("/artist/1/related")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [{"id": 5, "name": "Other Act"}]}))).mount(&deezer).await;
    Mock::given(method("GET")).and(path("/artist/1/top")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [track(9001, "Seed Song", "Seed Band", 200)]}))).mount(&deezer).await;
    Setup { server, deezer, library, _dir: dir }
}

/// ListenBrainz shares the switch; here it's unreachable, so only Deezer answers.
static NO_LISTENBRAINZ: std::sync::LazyLock<navia_core::listenbrainz::ListenBrainz> = std::sync::LazyLock::new(|| navia_core::listenbrainz::ListenBrainz::new("http://127.0.0.1:9"));

fn ext(deezer: &Deezer) -> Externals<'_> {
    Externals { deezer, listenbrainz: &NO_LISTENBRAINZ }
}

fn fast(deezer: &MockServer) -> Deezer {
    Deezer::new(deezer.uri()).with_timing(Duration::from_millis(1), Duration::from_secs(5))
}

#[tokio::test]
async fn deezer_radio_songs_join_the_offer_when_the_gate_is_open() {
    let s = setup(true).await;
    let dz = fast(&s.deezer);
    let mut a = Autoplay::new();
    a.attach(s.library.clone());
    a.set_chosen(["seed"].into_iter());
    let seeds = a.wanted_fetch().unwrap();
    a.begin_fetch(&seeds);
    let answers = fetch_answers_with(&s.server.read_only_client(), &s.library, ext(&dz), &seeds, AutoplayMode::Navia).await;
    let found = answers.deezer.clone().expect("Deezer was asked");
    assert_eq!(found.radio[0].1.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["dz"]);
    assert_eq!(found.related.values().next().unwrap(), &vec!["ar-Other Act".to_string()]);
    assert_eq!(found.popular.get("seed"), Some(&0));
    a.finish_answers(seeds, answers);
    let mut offered: Vec<String> = a.songs(&[], 10).into_iter().map(|s| s.id).collect();
    offered.sort();
    assert_eq!(offered, ["dz", "srv"], "the server's pick and Deezer's");
}

#[tokio::test]
async fn deezer_is_never_asked_with_the_gate_closed_or_in_server_mode() {
    let s = setup(false).await;
    let dz = fast(&s.deezer);
    let client = s.server.read_only_client();
    let answers = fetch_answers_with(&client, &s.library, ext(&dz), &["seed".to_string()], AutoplayMode::Navia).await;
    assert!(answers.deezer.is_none());
    assert_eq!(answers.similar[0].len(), 1);
    deezer::set_setting(&s.library, Some(true)).unwrap();
    let server_mode = fetch_answers_with(&client, &s.library, ext(&dz), &["seed".to_string()], AutoplayMode::Server).await;
    assert!(server_mode.deezer.is_none());
    assert!(s.deezer.received_requests().await.unwrap().is_empty());
}
