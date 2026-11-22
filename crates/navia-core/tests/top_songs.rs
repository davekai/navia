//! Top Songs' play history against a fake server shaped like Navidrome 0.64.2's `/api/scrobble`.

mod common;

use std::time::Duration;

use common::{FakeServer, State, song};
use navia_core::library::recent::{HISTORY_DAYS, PlayHistoryRefresh, refresh_play_history_at};
use navia_core::library::sync::{BackgroundRefresh, SyncOptions, Syncer};
use navia_core::library::{Library, LibraryConfig};
use serde_json::json;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

const NOW: i64 = 1_800_000_000;
const FROM: i64 = NOW - HISTORY_DAYS * 86_400;
const HOUR: i64 = 3600;
const AUTH: &str = "x-nd-authorization";

fn state() -> State {
    State { folders: vec![("1".into(), "Music".into())], open_subsonic: true, last_scan: Some("2026-01-01T00:00:00Z".into()), songs: vec![song("s1", "One", "Art", "Alb", "1"), song("s2", "Two", "Art", "Alb", "1")], ..Default::default() }
}

fn open_library() -> (tempfile::TempDir, Library) {
    let dir = tempfile::tempdir().unwrap();
    let library = Library::open(&LibraryConfig { path: dir.path().join("l.sqlite") }).unwrap();
    (dir, library)
}

async fn synced(server: &FakeServer) -> (tempfile::TempDir, Library) {
    let (dir, library) = open_library();
    Syncer::new(server.client(), library.clone(), SyncOptions::default()).full_sync(&|_| {}).await.unwrap();
    (dir, library)
}

/// What an annotation refresh needs from the fake server when nothing has changed.
fn no_annotations(server: &FakeServer) {
    let mut state = server.state.lock().unwrap();
    state.canned.insert("getStarred2".to_string(), json!({"starred2": {}}));
    state.canned.insert("getAlbumList2".to_string(), json!({"albumList2": {"album": []}}));
}

fn row(id: i64, song: &str, at: i64) -> serde_json::Value {
    json!({"id": id, "mediaFileId": song, "submissionTime": at})
}

/// Login, the authorized route and the 401 a request without a token gets.
async fn mount_history(server: &FakeServer, body: serde_json::Value) {
    Mock::given(method("POST")).and(path("/auth/login")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"token": "jwt"}))).mount(&server.server).await;
    Mock::given(method("GET")).and(path("/api/scrobble")).and(header(AUTH, "Bearer jwt")).respond_with(ResponseTemplate::new(200).set_body_json(body)).with_priority(1).mount(&server.server).await;
    Mock::given(method("GET")).and(path("/api/scrobble")).respond_with(ResponseTemplate::new(401).set_body_json(json!({"error": "Not authenticated"}))).with_priority(5).mount(&server.server).await;
}

async fn count(server: &FakeServer, wanted_path: &str) -> usize {
    server.server.received_requests().await.unwrap().iter().filter(|r| r.url.path() == wanted_path).count()
}

fn top(library: &Library) -> Vec<String> {
    let rows = library.top_song_rowids_at(16, NOW).unwrap();
    library.songs_by_rowid(&rows).unwrap().into_iter().map(|(_, s)| s.id).collect()
}

#[tokio::test]
async fn real_history_ranks_top_songs_and_asks_for_the_window() {
    let server = FakeServer::start(state()).await;
    let (_d, library) = synced(&server).await;
    mount_history(&server, json!([row(3, "s2", NOW - 10), row(2, "s2", NOW - 20), row(1, "s1", NOW - 30)])).await;

    assert_eq!(refresh_play_history_at(&server.client(), &library, NOW).await.unwrap(), PlayHistoryRefresh::Stored(3));
    assert_eq!(top(&library), ["s2", "s1"]);

    let requests = server.server.received_requests().await.unwrap();
    let asked = requests.iter().find(|r| r.url.path() == "/api/scrobble" && r.headers.contains_key(AUTH)).unwrap();
    let query = asked.url.query().unwrap();
    assert!(query.contains(&format!("from={FROM}")), "{query}");
    assert!(query.contains("_sort=submission_time") && query.contains("_order=DESC"), "{query}");
}

#[tokio::test]
async fn paging_follows_full_pages() {
    let server = FakeServer::start(state()).await;
    let (_d, library) = synced(&server).await;
    let full: Vec<_> = (0..1000).map(|i| row(10_000 - i, "s1", NOW - i)).collect();
    Mock::given(method("POST")).and(path("/auth/login")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"token": "jwt"}))).mount(&server.server).await;
    Mock::given(path("/api/scrobble")).and(header(AUTH, "Bearer jwt")).and(query_param("_start", "0")).respond_with(ResponseTemplate::new(200).set_body_json(full)).with_priority(1).mount(&server.server).await;
    Mock::given(path("/api/scrobble")).and(header(AUTH, "Bearer jwt")).and(query_param("_start", "1000")).respond_with(ResponseTemplate::new(200).set_body_json(json!([row(1, "s2", NOW - 5000)]))).with_priority(1).mount(&server.server).await;
    Mock::given(path("/api/scrobble")).respond_with(ResponseTemplate::new(401)).with_priority(5).mount(&server.server).await;
    assert_eq!(refresh_play_history_at(&server.client(), &library, NOW).await.unwrap(), PlayHistoryRefresh::Stored(1001));
}

#[tokio::test]
async fn a_server_without_the_route_gets_no_login_and_falls_back() {
    let server = FakeServer::start(state()).await;
    let (_d, library) = synced(&server).await;
    Mock::given(path("/api/scrobble")).respond_with(ResponseTemplate::new(404)).mount(&server.server).await;
    assert_eq!(refresh_play_history_at(&server.client(), &library, NOW).await.unwrap(), PlayHistoryRefresh::Unsupported);
    assert_eq!(count(&server, "/auth/login").await, 0);
    assert!(top(&library).is_empty());
}

#[tokio::test]
async fn history_turned_off_on_the_server_means_the_fallback() {
    let server = FakeServer::start(state()).await;
    let (_d, library) = synced(&server).await;
    mount_history(&server, json!([])).await;
    assert_eq!(refresh_play_history_at(&server.client(), &library, NOW).await.unwrap(), PlayHistoryRefresh::Stored(0));
    assert!(top(&library).is_empty(), "nothing played, nothing shown");
}

#[tokio::test]
async fn an_unknown_reply_keeps_what_was_stored() {
    let server = FakeServer::start(state()).await;
    let (_d, library) = synced(&server).await;
    mount_history(&server, json!([row(1, "s1", NOW - 10)])).await;
    refresh_play_history_at(&server.client(), &library, NOW).await.unwrap();
    server.server.reset().await;
    mount_history(&server, json!({"changed": true})).await;
    // Past the wait, and with the plays mark cleared by a newly played song.
    library.record_play("s2", ((NOW - 5) * 1000) as u64).unwrap();
    assert!(refresh_play_history_at(&server.client(), &library, NOW + 7 * HOUR).await.is_err());
    assert_eq!(top(&library), ["s1"]);
}

#[tokio::test]
async fn refetches_only_after_the_wait_and_when_plays_moved() {
    let server = FakeServer::start(state()).await;
    let (_d, library) = synced(&server).await;
    mount_history(&server, json!([row(1, "s1", NOW - 10)])).await;
    let client = server.client();
    refresh_play_history_at(&client, &library, NOW).await.unwrap();
    assert_eq!(count(&server, "/auth/login").await, 1);

    assert_eq!(refresh_play_history_at(&client, &library, NOW + HOUR).await.unwrap(), PlayHistoryRefresh::Skipped, "too soon");
    assert_eq!(refresh_play_history_at(&client, &library, NOW + 7 * HOUR).await.unwrap(), PlayHistoryRefresh::Skipped, "nothing new played");
    assert_eq!(count(&server, "/auth/login").await, 1);

    library.record_play("s2", (NOW * 1000) as u64).unwrap();
    assert_eq!(refresh_play_history_at(&client, &library, NOW + 2 * HOUR).await.unwrap(), PlayHistoryRefresh::Skipped, "still too soon");
    assert_eq!(refresh_play_history_at(&client, &library, NOW + 13 * HOUR).await.unwrap(), PlayHistoryRefresh::Stored(1));
    assert_eq!(count(&server, "/auth/login").await, 1, "the token is kept");
}

#[tokio::test]
async fn a_limited_login_is_asked_again_after_the_wait_not_hours_later() {
    let server = FakeServer::start(state()).await;
    let (_d, library) = synced(&server).await;
    Mock::given(method("POST")).and(path("/auth/login")).respond_with(ResponseTemplate::new(429)).up_to_n_times(1).with_priority(1).mount(&server.server).await;
    mount_history(&server, json!([row(1, "s1", NOW - 10)])).await;
    let client = server.client();
    client.set_native_backoff(Duration::from_millis(200), Duration::from_secs(1));

    assert!(refresh_play_history_at(&client, &library, NOW).await.is_err());
    assert!(matches!(refresh_play_history_at(&client, &library, NOW + 60).await, Err(navia_core::Error::Waiting { .. })));
    assert_eq!(count(&server, "/auth/login").await, 1, "nothing sent during the wait");

    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(refresh_play_history_at(&client, &library, NOW + 120).await.unwrap(), PlayHistoryRefresh::Stored(1));
    assert_eq!(top(&library), ["s1"]);
    assert_eq!(count(&server, "/auth/login").await, 2);
}

#[tokio::test]
async fn a_server_without_history_stays_on_the_slow_schedule() {
    let server = FakeServer::start(state()).await;
    let (_d, library) = synced(&server).await;
    Mock::given(path("/api/scrobble")).respond_with(ResponseTemplate::new(404)).mount(&server.server).await;
    let client = server.client();
    assert_eq!(refresh_play_history_at(&client, &library, NOW).await.unwrap(), PlayHistoryRefresh::Unsupported);
    library.record_play("s2", (NOW * 1000) as u64).unwrap();
    assert_eq!(refresh_play_history_at(&client, &library, NOW + 60).await.unwrap(), PlayHistoryRefresh::Skipped);
    assert_eq!(refresh_play_history_at(&client, &library, NOW + HOUR).await.unwrap(), PlayHistoryRefresh::Skipped);
    assert_eq!(count(&server, "/api/scrobble").await, 1);
    assert_eq!(client.native_retry_in(), None);
}

#[tokio::test]
async fn a_failing_probe_is_not_taken_for_a_server_without_history() {
    let server = FakeServer::start(state()).await;
    let (_d, library) = synced(&server).await;
    Mock::given(path("/api/scrobble")).respond_with(ResponseTemplate::new(503)).up_to_n_times(1).with_priority(1).mount(&server.server).await;
    mount_history(&server, json!([row(1, "s1", NOW - 10)])).await;
    let client = server.client();
    client.set_native_backoff(Duration::from_millis(100), Duration::from_secs(1));
    assert!(refresh_play_history_at(&client, &library, NOW).await.is_err());
    assert!(client.native_retry_in().is_some());
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(refresh_play_history_at(&client, &library, NOW + 60).await.unwrap(), PlayHistoryRefresh::Stored(1), "left for the retry, not the slow schedule");
}

#[tokio::test]
async fn the_background_refresh_asks_for_history_again_once_the_wait_is_over() {
    let server = FakeServer::start(state()).await;
    let (_d, library) = synced(&server).await;
    no_annotations(&server);
    Mock::given(method("POST")).and(path("/auth/login")).respond_with(ResponseTemplate::new(429).insert_header("retry-after", "1")).up_to_n_times(1).with_priority(1).mount(&server.server).await;
    mount_history(&server, json!([row(1, "s1", 4_000_000_000)])).await;
    let client = server.client();
    client.set_native_backoff(Duration::from_millis(100), Duration::from_secs(1));
    let interval = Duration::from_secs(4);
    let syncer = Syncer::new(client, library.clone(), SyncOptions { play_history: true, ..Default::default() });
    let started = std::time::Instant::now();
    let refresh = BackgroundRefresh::spawn_with(syncer, interval, None);

    while count(&server, "/auth/login").await < 2 {
        assert!(started.elapsed() < interval * 2, "no retry before the next tick");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let retried = started.elapsed();
    assert!(retried >= interval + Duration::from_secs(1), "Retry-After kept: {retried:?}");
    assert!(retried < interval * 2, "{retried:?}");
    assert_eq!(server.calls("getStarred2"), 1, "only the history was asked again");
    refresh.stop().await;
}

#[tokio::test]
async fn the_default_sync_never_logs_in() {
    let server = FakeServer::start(state()).await;
    let (_d, library) = synced(&server).await;
    no_annotations(&server);
    mount_history(&server, json!([row(1, "s1", NOW)])).await;
    Syncer::new(server.client(), library.clone(), SyncOptions::default()).refresh_annotations().await.unwrap();
    assert_eq!(count(&server, "/auth/login").await, 0);
    assert_eq!(count(&server, "/api/scrobble").await, 0);
}

#[tokio::test]
async fn annotation_refresh_fetches_history_when_the_app_asks() {
    let server = FakeServer::start(state()).await;
    let (_d, library) = synced(&server).await;
    no_annotations(&server);
    mount_history(&server, json!([row(1, "s1", 4_000_000_000)])).await;
    Syncer::new(server.client(), library.clone(), SyncOptions { play_history: true, ..Default::default() }).refresh_annotations().await.unwrap();
    assert_eq!(count(&server, "/auth/login").await, 1);
}

#[tokio::test]
async fn a_failed_login_keeps_annotations_working() {
    let server = FakeServer::start(state()).await;
    let (_d, library) = synced(&server).await;
    no_annotations(&server);
    Mock::given(path("/api/scrobble")).respond_with(ResponseTemplate::new(401)).mount(&server.server).await;
    Mock::given(path("/auth/login")).respond_with(ResponseTemplate::new(401)).mount(&server.server).await;
    Syncer::new(server.client(), library.clone(), SyncOptions { play_history: true, ..Default::default() }).refresh_annotations().await.unwrap();
}
