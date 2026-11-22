//! Client, capability and play-reporting behavior against the local fake server only.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{FakeServer, State};
use navia_core::Error;
use navia_core::capabilities::{Capabilities, ExternalProbe, Support};
use navia_core::client::{Credentials, StarTarget, StreamOptions};
use navia_core::reporting::{PlayReporter, TrackPlay};

fn open_state() -> State {
    State {
        open_subsonic: true,
        folders: vec![("1".into(), "Main".into())],
        extensions: vec![("songLyrics".into(), vec![1, 2]), ("topSongsByArtistId".into(), vec![1])],
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn capabilities_report_what_was_actually_detected() {
    let server = FakeServer::start(open_state()).await;
    let client = server.read_only_client();
    let probe = ExternalProbe { artist_id: "x".into(), artist_name: "Someone".into() };
    let c = Capabilities::detect(&client, Some(&probe)).await.unwrap();
    assert_eq!(c.server.kind.as_deref(), Some("navidrome"));
    assert!(c.has_extension("songLyrics"));
    assert_eq!(c.synced_lyrics.support, Support::Available);
    assert_eq!(c.top_songs_by_id.support, Support::Available);
    assert_eq!(c.sharing.support, Support::Available);
    assert_eq!(c.artist_info.support, Support::Available);
    assert_eq!(c.top_songs.support, Support::Available);
    assert_eq!(c.star_ratings.support, Support::Unknown);
    assert_eq!(c.favorites.support, Support::Unknown);
    assert_eq!(c.libraries.len(), 1);
    assert!(server.calls("scrobble") == 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_features_are_reported_as_missing_not_assumed() {
    let mut st = open_state();
    st.extensions.clear();
    st.sharing_501 = true;
    let server = FakeServer::start(st).await;
    let c = Capabilities::detect(&server.read_only_client(), None).await.unwrap();
    assert_eq!(c.synced_lyrics.support, Support::Unavailable);
    assert_eq!(c.sharing.support, Support::Unavailable);
    assert_eq!(c.artist_info.support, Support::Unknown, "no artist to probe yet");
}

#[tokio::test(flavor = "multi_thread")]
async fn plain_subsonic_server_has_no_extensions_and_does_not_fail() {
    let mut st = open_state();
    st.open_subsonic = false;
    let server = FakeServer::start(st).await;
    let c = Capabilities::detect(&server.read_only_client(), None).await.unwrap();
    assert!(c.extensions.is_empty());
    assert_eq!(c.synced_lyrics.support, Support::Unavailable);
    // only the request style's ask, which carries no credentials
    let asks: Vec<_> = server.sent().into_iter().filter(|r| r.endpoint == "getOpenSubsonicExtensions").collect();
    assert_eq!(asks.len(), 1);
    assert!(!asks[0].credentials_in_url() && !asks[0].credentials_in_body());
}

#[tokio::test(flavor = "multi_thread")]
async fn star_reaches_the_server_and_read_only_clients_refuse_it() {
    let server = FakeServer::start(open_state()).await;
    let client = server.client();
    client.star(StarTarget::Song("s1")).await.unwrap();
    client.unstar(StarTarget::Album("al1")).await.unwrap();
    client.star(StarTarget::Artist("ar1")).await.unwrap();
    assert_eq!(server.calls("star"), 2);
    assert_eq!(server.calls("unstar"), 1);

    let read_only = server.read_only_client();
    assert!(matches!(read_only.star(StarTarget::Song("s1")).await.unwrap_err(), Error::WritesDisabled { .. }));
    assert!(matches!(read_only.unstar(StarTarget::Song("s1")).await.unwrap_err(), Error::WritesDisabled { .. }));
    assert_eq!(server.calls("star"), 2, "nothing was sent");
}

#[tokio::test(flavor = "multi_thread")]
async fn read_only_client_blocks_writes_before_any_request() {
    let server = FakeServer::start(open_state()).await;
    let client = server.read_only_client();
    let err = client.scrobble("x", true, None).await.unwrap_err();
    assert!(matches!(err, Error::WritesDisabled { .. }));
    assert!(matches!(client.save_play_queue(&["x".into()], None, None).await.unwrap_err(), Error::WritesDisabled { .. }));
    assert_eq!(server.calls("scrobble"), 0);
    assert_eq!(server.calls("savePlayQueue"), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn only_submission_true_counts_and_private_mode_silences_everything() {
    let server = FakeServer::start(open_state()).await;
    let reporter = PlayReporter::new(Arc::new(server.client()));
    let play = |id: &str| TrackPlay { song_id: id.into(), duration: Duration::from_secs(100), started_at_ms: 1_700_000_000_000 };

    reporter.track_started(play("listened")).await;
    reporter.advance(Duration::from_secs(60)).await;
    reporter.track_ended();

    reporter.track_started(play("skipped")).await;
    reporter.advance(Duration::from_secs(10)).await;
    reporter.track_ended();

    reporter.set_private(true).await;
    reporter.track_started(play("secret")).await;
    reporter.advance(Duration::from_secs(90)).await;
    reporter.track_ended();
    reporter.set_private(false).await;

    let scrobbles = server.state.lock().unwrap().scrobbles.clone();
    assert_eq!(scrobbles, vec![
        ("listened".to_string(), "false".to_string(), None),
        ("listened".to_string(), "true".to_string(), Some("1700000000000".to_string())),
        ("skipped".to_string(), "false".to_string(), None),
    ]);
}

#[tokio::test(flavor = "multi_thread")]
async fn urls_and_errors_never_expose_the_password() {
    let server = FakeServer::start(open_state()).await;
    let client = server.client();
    let url = client.cover_art_url("al-1", Some(300)).unwrap();
    assert!(url.contains("id=al-1"));
    assert!(url.contains("size=300"));
    assert!(url.contains("t=") && url.contains("s="));
    assert!(!url.contains("letmein"));
    let stream = client.stream_url("s1", &StreamOptions { format: Some("opus".into()), max_bit_rate: Some(192), time_offset: None }).unwrap();
    assert!(stream.contains("format=opus") && stream.contains("maxBitRate=192"));

    // A failed request reports the endpoint but no URL, token or salt.
    let bad = navia_core::client::Client::new(navia_core::client::ClientConfig::new("http://127.0.0.1:1", Credentials::new("u", "letmein"))).unwrap();
    let message = bad.ping().await.unwrap_err().to_string();
    assert!(!message.contains("letmein"));
    assert!(!message.contains("t="), "{message}");
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_credentials_are_a_distinct_error() {
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::path_regex};
    let server = MockServer::start().await;
    Mock::given(path_regex("/rest/ping"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "subsonic-response": {"status":"failed","version":"1.16.1","error":{"code":40,"message":"Wrong username or password"}}
        })))
        .mount(&server)
        .await;
    let client = navia_core::client::Client::new(navia_core::client::ClientConfig::new(server.uri(), Credentials::new("u", "p"))).unwrap();
    let err = client.ping().await.unwrap_err();
    assert!(err.is_auth_failure());
    assert!(!err.is_transient());
}

/// Navidrome lists a player as "<c> [<User-Agent>]" and keys it on both.
/// So the User-Agent leaves out the build number, or every test build would add a player.
#[tokio::test(flavor = "multi_thread")]
async fn requests_carry_the_client_name_and_user_agent() {
    let server = FakeServer::start(open_state()).await;
    navia_core::client::set_app_version("0.1.0+560");
    let client = server.read_only_client();
    client.ping().await.unwrap();
    let requests = server.server.received_requests().await.unwrap();
    let ping = requests.iter().find(|r| r.url.path().contains("ping")).expect("a ping");
    let c = ping.url.query_pairs().find(|(k, _)| k == "c").map(|(_, v)| v.into_owned());
    assert_eq!(c.as_deref(), Some("Navia"));
    let ua = ping.headers.get("user-agent").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
    assert_eq!(ua, format!("Navia/0.1.0 ({})", navia_core::client::platform_name()));
    assert_eq!(navia_core::client::user_agent(), ua);
}
