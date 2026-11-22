//! the offline switch: while it's on nothing reaches the server, downloads still play, and turning
//! it off reaches the server again. local fakes only.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::audio::{synth, to_f32, wav_bytes};
use common::http::{Faults, Library as ServerFiles, TestServer};
use common::{FakeServer, State};
use navia_core::artwork::{ArtworkCache, ArtworkConfig};
use navia_core::audio::output::CaptureBackend;
use navia_core::audio::player::{OutputPolicy, Player, PlayerConfig, PlayerStatus, QueueItem, ServerResolver};
use navia_core::audio::stream::{HttpFetcher, HttpStream, StreamConfig};
use navia_core::client::{Client, ClientConfig, Credentials, PlaybackState, StreamOptions, WritePolicy};
use navia_core::downloads::{DownloadManager, DownloadRequest};
use navia_core::library::sync::Syncer;
use navia_core::library::{Library, LibraryConfig};
use navia_core::reporting::{PlayReporter, TrackPlay};

fn wait_for(limit: Duration, mut check: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        if check() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    check()
}

fn offline_error(e: &navia_core::Error) -> bool {
    e.is_outage() && e.to_string().contains(navia_core::client::OFFLINE)
}

#[test]
fn downloads_play_and_nothing_is_streamed_or_downloaded_while_on() {
    let pcm = synth(48_000 * 2, 48_000, 0.4);
    let mut files = ServerFiles::default();
    files.raw.insert("a".into(), wav_bytes(&pcm, 48_000));
    files.raw.insert("b".into(), wav_bytes(&synth(48_000, 48_000, 0.6), 48_000));
    let server = TestServer::start(files, Faults::default());
    let dir = tempfile::tempdir().unwrap();
    let client = Client::new(ClientConfig::new(server.url.clone(), Credentials::new("t".to_string(), "t".to_string()))).unwrap();
    let library = Library::open(&LibraryConfig { path: dir.path().join("lib.sqlite") }).unwrap();
    let downloads = Arc::new(DownloadManager::new(library, client.clone(), dir.path().join("downloads")).unwrap());
    let request = |id: &str| DownloadRequest { id: id.to_string(), suffix: Some("wav".to_string()), size: None };
    downloads.enqueue(vec![request("a")]);
    assert!(wait_for(Duration::from_secs(5), || downloads.local_path("a").is_some()));
    // built while on line, used after the switch: a stream holds its URL
    let url_before = client.stream_url("b", &StreamOptions::default()).unwrap();

    client.set_offline(true);
    downloads.set_online(false);
    let seen = server.seen();

    // paced: a capture pulling as fast as it can records silence whenever the decoder falls behind
    let backend = CaptureBackend { speed: 25.0, ..Default::default() };
    let capture = backend.capture.clone();
    let resolver = ServerResolver::new(client.clone()).unwrap().with_downloads(downloads.clone());
    let player = Player::with_backend(Arc::new(resolver), Box::new(backend), PlayerConfig { policy: OutputPolicy::FollowTrack, ..Default::default() }).unwrap();
    player.set_queue(vec![QueueItem::new("a".into(), Some(2000))], 0, 0, false);
    assert!(wait_for(Duration::from_secs(5), || player.snapshot().status == PlayerStatus::Stopped));
    assert_eq!(player.snapshot().underrun_frames, 0, "underruns");
    let samples = capture.samples();
    let first = samples.chunks_exact(2).position(|f| f[0] != 0.0 || f[1] != 0.0).unwrap_or(0);
    let expected = to_f32(&pcm);
    assert_eq!(&samples[first * 2..first * 2 + expected.len()], &expected[..], "the download plays bit-exact");

    // a song that isn't on this device waits, as with the server unreachable
    player.set_queue(vec![QueueItem::new("b".into(), Some(1000))], 0, 0, false);
    std::thread::sleep(Duration::from_millis(400));
    assert!(matches!(client.stream_url("b", &StreamOptions::default()), Err(ref e) if offline_error(e)));
    let fetcher = HttpFetcher::new().unwrap().with_offline(client.offline_switch());
    assert!(matches!(HttpStream::open(&fetcher, url_before, StreamConfig::default()), Err(ref e) if offline_error(e)));
    downloads.enqueue(vec![request("b")]);
    std::thread::sleep(Duration::from_millis(400));
    assert!(downloads.local_path("b").is_none());

    assert_eq!(server.seen(), seen, "nothing reached the server while the switch was on");
    drop(player);

    client.set_offline(false);
    downloads.set_online(true);
    assert!(wait_for(Duration::from_secs(5), || downloads.local_path("b").is_some()), "off: the waiting download goes ahead");
    assert!(server.seen() > seen);
}

#[tokio::test(flavor = "multi_thread")]
async fn no_request_of_any_kind_reaches_the_server_while_on() {
    let server = FakeServer::start(State::default()).await;
    let client = server.client();
    let copy = client.copy_with_policy(WritePolicy::Allowed).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let library = Library::open(&LibraryConfig { path: dir.path().join("lib.sqlite") }).unwrap();
    let artwork = ArtworkCache::new(client.clone(), ArtworkConfig { dir: dir.path().join("art"), max_bytes: u64::MAX }).unwrap();
    let reporter = PlayReporter::with_store(Arc::new(client.clone()), Arc::new(library.clone()));

    client.set_offline(true);
    assert!(copy.is_offline(), "copies share the switch");

    let at = Instant::now();
    assert!(offline_error(&client.ping().await.unwrap_err()));
    assert!(at.elapsed() < Duration::from_millis(100), "no retries while on");
    assert!(client.artists(None).await.is_err());
    assert!(client.lyrics_by_song_id("s1", true).await.is_err());
    assert!(copy.scrobble("s1", true, Some(1)).await.is_err());
    assert!(copy.report_playback("s1", PlaybackState::Playing, 0).await.is_err());
    assert!(copy.save_play_queue(&["s1".to_string()], Some("s1"), Some(0)).await.is_err());
    assert!(copy.star(navia_core::client::StarTarget::Song("s1")).await.is_err());
    assert!(client.library_totals(None).await.is_err());
    assert!(client.public_config().await.is_none());
    assert!(client.cover_art_url("al-1", Some(300)).is_err());
    assert!(artwork.cover("al-1", Some(300)).await.is_err());
    assert!(navia_core::listen_along::fetch(&client, true).await.is_err());
    assert!(Syncer::new(client.clone(), library.clone(), Default::default()).full_sync(&|_| {}).await.is_err());

    // a play that counts is kept for later, as when the server can't be reached
    reporter.track_started(TrackPlay { song_id: "s1".into(), duration: Duration::from_secs(200), started_at_ms: 1_700_000_000_000 }).await;
    reporter.advance(Duration::from_secs(150)).await;
    assert_eq!(reporter.queued(), 1);
    assert!(reporter.flush().await.is_err());

    assert!(server.server.received_requests().await.unwrap_or_default().is_empty(), "nothing reached the server");

    client.set_offline(false);
    assert!(client.ping().await.is_ok());
    reporter.flush().await.unwrap();
    assert_eq!(reporter.queued(), 0, "off: the kept play goes out");
    assert_eq!(server.state.lock().unwrap().scrobbles.len(), 1);
}
