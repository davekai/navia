//! the Navia mode's sources against a fake Navidrome, a fake Deezer and a fake ListenBrainz: the
//! server's answer without its filler (asked twice, only the stable head kept), the Server mode
//! unchanged, and ListenBrainz's similar recordings merged in only with the switch on.

mod common;

use std::time::{Duration, Instant};

use common::{FakeServer, FakeSong, State, song};
use navia_core::autoplay::{Autoplay, Externals, fetch_answers_with};
use navia_core::deezer::{self, Deezer};
use navia_core::engine::{self, AutoplayMode};
use navia_core::library::sync::{SyncOptions, Syncer};
use navia_core::library::{Library, LibraryConfig};
use navia_core::listenbrainz::ListenBrainz;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// (id, title, artist) of the library besides the anchors.
const SONGS: &[(&str, &str, &str)] = &[
    ("r1", "Real One", "Real A"),
    ("r2", "Real Two", "Real B"),
    ("r3", "Real Three", "Real C"),
    ("p1", "Pad One", "Pad A"),
    ("p2", "Pad Two", "Pad B"),
    ("p3", "Pad Three", "Pad C"),
    ("p4", "Pad Four", "Pad D"),
    ("p5", "Pad Five", "Pad E"),
    ("both", "Both Song", "Both Act"),
    ("lbonly", "LB Only", "LB Act"),
];

fn entry(id: &str) -> Value {
    let (title, artist) = SONGS.iter().find(|s| s.0 == id).map(|s| (s.1, s.2)).unwrap_or((id, "Anchor Band"));
    json!({"id": id, "title": title, "artist": artist, "artistId": format!("ar-{artist}"), "artists": [{"id": format!("ar-{artist}"), "name": artist}], "album": format!("{title} LP"), "albumId": format!("al-{id}"), "duration": 200, "genre": "Rock", "year": 2000})
}

fn answer(ids: &[&str]) -> Value {
    json!({"similarSongs2": {"song": ids.iter().map(|id| entry(id)).collect::<Vec<_>>()}})
}

struct Setup {
    server: FakeServer,
    deezer: MockServer,
    listenbrainz: MockServer,
    library: Library,
    _dir: tempfile::TempDir,
}

async fn setup() -> Setup {
    let with = |mut s: FakeSong, extra: Value| {
        s.extra = extra;
        s
    };
    let mut songs: Vec<FakeSong> = ["a1", "a2", "a3"].iter().map(|id| with(song(id, &format!("Anchor {id}"), "Anchor Band", &format!("Anchor {id} LP"), "1"), json!({"duration": 200}))).collect();
    for (id, title, artist) in SONGS {
        let extra = if *id == "both" { json!({"duration": 200, "musicBrainzId": "mb-both"}) } else { json!({"duration": 200}) };
        songs.push(with(song(id, title, artist, &format!("{title} LP"), "1"), extra));
    }
    let state = State { folders: vec![("1".into(), "Music".into())], open_subsonic: true, last_scan: Some("2026-01-01T00:00:00Z".into()), songs, ..Default::default() };
    let server = FakeServer::start(state).await;
    let dir = tempfile::tempdir().unwrap();
    let library = Library::open(&LibraryConfig { path: dir.path().join("l.sqlite") }).unwrap();
    Syncer::new(server.client(), library.clone(), SyncOptions::default()).full_sync(&|_| {}).await.unwrap();
    {
        let mut st = server.state.lock().unwrap();
        // an agent's answer, the same every call.
        st.turns.insert("getSimilarSongs2:a1".into(), vec![answer(&["r1", "r2", "r3"]), answer(&["r1", "r2", "r3"])]);
        // the local agent's genre draw: different every call, sharing a song by chance.
        st.turns.insert("getSimilarSongs2:a2".into(), vec![answer(&["p1", "p2", "p3"]), answer(&["p4", "p2", "p5"])]);
        // an agent's songs in another order, topped up with draws.
        st.turns.insert("getSimilarSongs2:a3".into(), vec![answer(&["r1", "r2", "p1", "p2"]), answer(&["r2", "r1", "p3", "p2"])]);
    }
    Setup { server, deezer: MockServer::start().await, listenbrainz: MockServer::start().await, library, _dir: dir }
}

fn ext<'a>(deezer: &'a Deezer, lb: &'a ListenBrainz) -> Externals<'a> {
    Externals { deezer, listenbrainz: lb }
}

fn fakes(s: &Setup) -> (Deezer, ListenBrainz) {
    (Deezer::new(s.deezer.uri()).with_timing(Duration::from_millis(1), Duration::from_secs(2)), ListenBrainz::new(s.listenbrainz.uri()).with_pace(Duration::from_millis(1)))
}

fn ids(songs: &[navia_core::client::Song]) -> Vec<&str> {
    songs.iter().map(|s| s.id.as_str()).collect()
}

#[tokio::test]
async fn the_navia_mode_keeps_only_the_head_two_answers_share() {
    let s = setup().await;
    let (dz, lb) = fakes(&s);
    let client = s.server.read_only_client();
    let seeds: Vec<String> = ["a1", "a2", "a3"].iter().map(|s| s.to_string()).collect();
    let answers = fetch_answers_with(&client, &s.library, ext(&dz, &lb), &seeds, AutoplayMode::Navia).await;
    assert_eq!(ids(&answers.similar[0]), ["r1", "r2", "r3"], "stable: whole, in the server's order");
    assert!(answers.similar[1].is_empty(), "random: nothing, not even the song both draws share");
    assert_eq!(ids(&answers.similar[2]), ["r1", "r2"], "real plus padding: only the real head");
    assert_eq!(s.server.calls("getSimilarSongs2"), 6, "two asks per anchor");
    // kept for the session: asked again, the server isn't.
    let again = fetch_answers_with(&client, &s.library, ext(&dz, &lb), &seeds, AutoplayMode::Navia).await;
    assert_eq!(ids(&again.similar[2]), ["r1", "r2"]);
    assert_eq!(s.server.calls("getSimilarSongs2"), 6);
    // the plan never offers the filler.
    let mut a = Autoplay::new();
    a.attach(s.library.clone());
    a.set_chosen(["a2"].into_iter());
    let wanted = a.wanted_fetch().unwrap();
    a.begin_fetch(&wanted);
    a.finish_answers(wanted, fetch_answers_with(&client, &s.library, ext(&dz, &lb), &["a2".to_string()], AutoplayMode::Navia).await);
    assert!(a.songs(&[], 50).iter().all(|s| !s.id.starts_with('p')), "no filler offered");
}

#[tokio::test]
async fn the_server_mode_is_the_servers_answer_as_it_is() {
    let s = setup().await;
    let (dz, lb) = fakes(&s);
    engine::set_mode(&s.library, AutoplayMode::Server).unwrap();
    let client = s.server.read_only_client();
    let seeds: Vec<String> = ["a2", "a3"].iter().map(|s| s.to_string()).collect();
    let answers = fetch_answers_with(&client, &s.library, ext(&dz, &lb), &seeds, AutoplayMode::Server).await;
    assert_eq!(ids(&answers.similar[0]), ["p1", "p2", "p3"]);
    assert_eq!(ids(&answers.similar[1]), ["r1", "r2", "p1", "p2"]);
    assert_eq!(s.server.calls("getSimilarSongs2"), 2, "one ask per seed");
}

async fn listenbrainz_answers(lb: &MockServer) {
    Mock::given(method("POST")).and(path("/acr-lookup/json")).respond_with(ResponseTemplate::new(200).set_body_json(json!([{"index": 0, "recording_mbid": "mb-a1"}]))).mount(lb).await;
    let similar = json!([
        {"recording_mbid": "mb-both", "recording_name": "Something Else Entirely", "artist_credit_name": "Nobody"},
        {"recording_mbid": "mb-unknown", "recording_name": "LB Only (Remastered)", "artist_credit_name": "LB Act feat. Guest"},
        {"recording_mbid": "mb-missing", "recording_name": "Not Here", "artist_credit_name": "Stranger"}
    ]);
    Mock::given(method("GET")).and(path("/similar-recordings/json")).respond_with(ResponseTemplate::new(200).set_body_json(similar)).mount(lb).await;
}

#[tokio::test]
async fn listenbrainz_merges_with_the_servers_answer_when_the_switch_is_on() {
    let s = setup().await;
    s.server.state.lock().unwrap().turns.insert("getSimilarSongs2:a1".into(), vec![answer(&["r1", "r2", "r3", "both"])]);
    listenbrainz_answers(&s.listenbrainz).await;
    deezer::set_setting(&s.library, Some(true)).unwrap();
    let (dz, lb) = fakes(&s);
    let client = s.server.read_only_client();
    let mut a = Autoplay::new();
    a.attach(s.library.clone());
    a.set_chosen(["a1"].into_iter());
    let seeds = a.wanted_fetch().unwrap();
    a.begin_fetch(&seeds);
    let answers = fetch_answers_with(&client, &s.library, ext(&dz, &lb), &seeds, AutoplayMode::Navia).await;
    assert_eq!(answers.listenbrainz.len(), 1);
    assert_eq!(ids(&answers.listenbrainz[0].1), ["both", "lbonly"], "by MBID, then by the bare name");
    a.finish_answers(seeds, answers);
    assert!(a.ready());
    let offer = a.songs(&[], 50);
    assert_eq!(offer[0].id, "both", "named by the server and ListenBrainz: first ({:?})", ids(&offer));
    assert!(ids(&offer).contains(&"lbonly"));
}

#[tokio::test]
async fn listenbrainz_is_never_asked_with_the_switch_off() {
    let s = setup().await;
    listenbrainz_answers(&s.listenbrainz).await;
    deezer::set_setting(&s.library, Some(false)).unwrap();
    let (dz, lb) = fakes(&s);
    let answers = fetch_answers_with(&s.server.read_only_client(), &s.library, ext(&dz, &lb), &["a1".to_string()], AutoplayMode::Navia).await;
    assert!(answers.listenbrainz.is_empty());
    assert!(s.listenbrainz.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_failing_or_slow_listenbrainz_still_seals_the_plan() {
    for slow in [false, true] {
        let s = setup().await;
        let reply = if slow { ResponseTemplate::new(200).set_body_json(json!([])).set_delay(Duration::from_secs(5)) } else { ResponseTemplate::new(500) };
        Mock::given(wiremock::matchers::any()).respond_with(reply).mount(&s.listenbrainz).await;
        deezer::set_setting(&s.library, Some(true)).unwrap();
        let (dz, lb) = fakes(&s);
        let mut a = Autoplay::new();
        a.attach(s.library.clone());
        a.set_chosen(["a1"].into_iter());
        let seeds = a.wanted_fetch().unwrap();
        a.begin_fetch(&seeds);
        let started = Instant::now();
        let answers = fetch_answers_with(&s.server.read_only_client(), &s.library, ext(&dz, &lb), &seeds, AutoplayMode::Navia).await;
        assert!(started.elapsed() < engine::EXTERNAL_TIMEOUT + Duration::from_secs(1));
        assert!(answers.listenbrainz.is_empty());
        a.finish_answers(seeds, answers);
        assert!(a.ready(), "sealed without it");
        let offer = a.songs(&[], 50);
        assert!(["r1", "r2", "r3"].iter().all(|id| ids(&offer).contains(id)), "the server's songs: {:?}", ids(&offer));
        assert!(!s.listenbrainz.received_requests().await.unwrap().is_empty(), "it was asked");
    }
}
