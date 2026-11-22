//! the native API's kept login and its shared wait, against a fake Navidrome.

mod common;

use std::time::Duration;

use common::{FakeServer, State};
use navia_core::Error;
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, ResponseTemplate};

const AUTH: &str = "x-nd-authorization";

async fn count(server: &FakeServer, wanted: &str) -> usize {
    server.server.received_requests().await.unwrap().iter().filter(|r| r.url.path() == wanted).count()
}

async fn login_gives(server: &FakeServer, token: &str, times: Option<u64>, priority: u8) {
    let mock = Mock::given(method("POST")).and(path("/auth/login")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"token": token}))).with_priority(priority);
    match times {
        Some(n) => mock.up_to_n_times(n).mount(&server.server).await,
        None => mock.mount(&server.server).await,
    }
}

async fn login_refused(server: &FakeServer, status: u16, retry_after: Option<&str>, times: u64) {
    let mut reply = ResponseTemplate::new(status);
    if let Some(after) = retry_after {
        reply = reply.insert_header("retry-after", after);
    }
    Mock::given(method("POST")).and(path("/auth/login")).respond_with(reply).up_to_n_times(times).with_priority(1).mount(&server.server).await;
}

fn song_row() -> serde_json::Value {
    json!([{"id": "s1", "albumId": "a1", "updatedAt": "2026-11-18T13:16:22Z", "tags": {"barcode": ["0123"]}}])
}

/// `/api/song` for `token`, and 401 for anything else.
async fn songs_for(server: &FakeServer, token: &str) {
    Mock::given(method("GET")).and(path("/api/song")).and(header(AUTH, format!("Bearer {token}").as_str())).respond_with(ResponseTemplate::new(200).set_body_json(song_row())).with_priority(1).mount(&server.server).await;
    Mock::given(method("GET")).and(path("/api/song")).respond_with(ResponseTemplate::new(401)).with_priority(5).mount(&server.server).await;
}

#[tokio::test]
async fn one_login_serves_every_native_call_and_every_copy() {
    let server = FakeServer::start(State::default()).await;
    login_gives(&server, "jwt", None, 1).await;
    songs_for(&server, "jwt").await;
    Mock::given(path("/api/scrobble")).and(header(AUTH, "Bearer jwt")).respond_with(ResponseTemplate::new(200).set_body_json(json!([]))).mount(&server.server).await;
    let client = server.client();

    let (a, b, c) = tokio::join!(client.album_barcode("a1"), client.album_barcode("a2"), client.songs_updated_since("1", None, 0));
    assert_eq!(a.unwrap().as_deref(), Some("0123"));
    assert_eq!(b.unwrap().as_deref(), Some("0123"));
    assert_eq!(c.unwrap().newest.as_deref(), Some("2026-11-18T13:16:22Z"));
    client.scrobbles_since(0, 10).await.unwrap();
    client.writable_copy().unwrap().album_barcode("a3").await.unwrap();

    assert_eq!(count(&server, "/auth/login").await, 1);
    assert_eq!(count(&server, "/api/song").await, 4);
}

#[tokio::test]
async fn the_refreshed_token_in_a_reply_is_used_next() {
    let server = FakeServer::start(State::default()).await;
    login_gives(&server, "jwt", None, 1).await;
    let first = ResponseTemplate::new(200).set_body_json(song_row()).insert_header(AUTH, "Bearer jwt2");
    Mock::given(path("/api/song")).and(header(AUTH, "Bearer jwt")).respond_with(first).mount(&server.server).await;
    Mock::given(path("/api/song")).and(header(AUTH, "Bearer jwt2")).respond_with(ResponseTemplate::new(200).set_body_json(json!([]))).mount(&server.server).await;
    let client = server.client();

    assert_eq!(client.album_barcode("a1").await.unwrap().as_deref(), Some("0123"));
    assert_eq!(client.album_barcode("a2").await.unwrap(), None, "asked with the refreshed token");
    assert_eq!(count(&server, "/auth/login").await, 1);
}

#[tokio::test]
async fn an_expired_token_logs_in_once_more_and_retries_once() {
    let server = FakeServer::start(State::default()).await;
    login_gives(&server, "old", Some(1), 1).await;
    login_gives(&server, "new", None, 2).await;
    songs_for(&server, "new").await;
    let client = server.client();

    assert_eq!(client.album_barcode("a1").await.unwrap().as_deref(), Some("0123"));
    assert_eq!(count(&server, "/auth/login").await, 2);
    assert_eq!(count(&server, "/api/song").await, 2);
}

#[tokio::test]
async fn a_token_the_server_keeps_refusing_is_not_retried_in_a_loop() {
    let server = FakeServer::start(State::default()).await;
    login_gives(&server, "jwt", None, 1).await;
    Mock::given(path("/api/song")).respond_with(ResponseTemplate::new(401)).mount(&server.server).await;
    let client = server.client();

    assert!(matches!(client.album_barcode("a1").await, Err(Error::HttpStatus { status: 401, .. })));
    assert_eq!(count(&server, "/auth/login").await, 2);
    assert_eq!(count(&server, "/api/song").await, 2);
    assert_eq!(client.native_retry_in(), None, "a refusal isn't a reason to wait");
}

#[tokio::test]
async fn retry_after_is_honoured_over_a_shorter_backoff() {
    let server = FakeServer::start(State::default()).await;
    login_refused(&server, 429, Some("1"), 1).await;
    login_gives(&server, "jwt", None, 2).await;
    songs_for(&server, "jwt").await;
    let client = server.client();
    client.set_native_backoff(Duration::from_millis(50), Duration::from_secs(1));

    assert!(matches!(client.album_barcode("a1").await, Err(Error::HttpStatus { status: 429, .. })));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(matches!(client.album_barcode("a1").await, Err(Error::Waiting { .. })), "still inside the server's second");
    assert!(matches!(client.songs_updated_since("1", None, 0).await, Err(Error::Waiting { .. })), "the wait is shared");
    assert_eq!(count(&server, "/auth/login").await, 1);

    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(client.album_barcode("a1").await.unwrap().as_deref(), Some("0123"));
    assert_eq!(count(&server, "/auth/login").await, 2);
    assert_eq!(client.native_retry_in(), None);
}

#[tokio::test]
async fn the_backoff_doubles_to_its_cap_and_ends_with_an_answer() {
    let server = FakeServer::start(State::default()).await;
    login_refused(&server, 429, None, 1).await;
    Mock::given(method("POST")).and(path("/auth/login")).respond_with(ResponseTemplate::new(503)).up_to_n_times(2).with_priority(2).mount(&server.server).await;
    login_gives(&server, "jwt", None, 3).await;
    songs_for(&server, "jwt").await;
    let client = server.client();
    let step = Duration::from_millis(100);
    client.set_native_backoff(step, step * 3);

    let mut waits = Vec::new();
    for _ in 0..3 {
        assert!(client.album_barcode("a1").await.is_err());
        let wait = client.native_retry_in().expect("waiting after a limited or failed login");
        waits.push(wait);
        tokio::time::sleep(wait + Duration::from_millis(20)).await;
    }
    let near = |d: Duration, ms: u64| d <= Duration::from_millis(ms) && d > Duration::from_millis(ms - 60);
    assert!(near(waits[0], 100) && near(waits[1], 200) && near(waits[2], 300), "{waits:?}");

    assert_eq!(client.album_barcode("a1").await.unwrap().as_deref(), Some("0123"));
    assert_eq!(client.native_retry_in(), None);
    assert_eq!(count(&server, "/auth/login").await, 4);
}
