//! with the `formPost` extension, every request Navia sends carries its parameters, credentials
//! included, in a POST form and none in the URL; without it, requests stay GET. URLs built for
//! anything outside Navia keep their query. local fakes only.

mod common;

use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::http::{Faults, Library as ServerFiles, TestServer};
use common::{FakeServer, State, song};
use navia_core::artwork::{ArtworkCache, ArtworkConfig};
use navia_core::audio::stream::{HttpFetcher, HttpStream, StreamConfig};
use navia_core::client::{Client, ClientConfig, Credentials, StreamOptions};
use navia_core::downloads::{DownloadManager, DownloadRequest};
use navia_core::library::{Library, LibraryConfig};

fn png() -> Vec<u8> {
    let image = image::ImageBuffer::from_pixel(32, 32, image::Rgba([200u8, 60, 40, 255]));
    let mut bytes = Vec::new();
    image.write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png).unwrap();
    bytes
}

fn state(form_post: bool) -> State {
    let mut extensions = vec![("songLyrics".to_string(), vec![1])];
    if form_post {
        extensions.push(("formPost".to_string(), vec![1]));
    }
    State { open_subsonic: true, extensions, songs: vec![song("s1", "One", "Artist", "Album", "1")], folders: vec![("1".into(), "Music".into())], cover_png: Some(png()), ..Default::default() }
}

fn query_keys(url: &str) -> Vec<String> {
    url::Url::parse(url).unwrap().query_pairs().map(|(k, _)| k.into_owned()).collect()
}

async fn exercise(server: &FakeServer, client: &Client) {
    let dir = tempfile::tempdir().unwrap();
    client.ping().await.unwrap();
    client.album("al-Album").await.unwrap();
    client.open_subsonic_extensions().await.unwrap();
    let artwork = ArtworkCache::new(client.clone(), ArtworkConfig { dir: dir.path().join("art"), max_bytes: u64::MAX }).unwrap();
    artwork.cover("al-Album", Some(64)).await.unwrap();
    assert!(server.calls("getCoverArt") >= 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn with_form_post_every_request_carries_its_credentials_in_the_body() {
    let server = FakeServer::start(state(true)).await;
    let client = server.client();
    exercise(&server, &client).await;
    assert_eq!(client.form_post().known(), Some(true));

    let sent = server.sent();
    // the first ask of the extension list goes out with no credentials at all
    let (ask, rest) = sent.split_first().unwrap();
    assert_eq!((ask.method.as_str(), ask.endpoint.as_str()), ("GET", "getOpenSubsonicExtensions"));
    assert!(!ask.credentials_in_url() && !ask.credentials_in_body());
    assert!(rest.len() >= 4, "{sent:?}");
    for r in rest {
        assert_eq!(r.method, "POST", "{r:?}");
        assert!(r.query.is_empty(), "nothing in the URL: {r:?}");
        assert!(r.credentials_in_body(), "{r:?}");
        assert_eq!(r.content_type.as_deref(), Some("application/x-www-form-urlencoded"));
        for key in ["v", "c", "f"] {
            assert!(r.body.iter().any(|k| k == key), "{key} in {r:?}");
        }
    }
    let album = rest.iter().find(|r| r.endpoint == "getAlbum").unwrap();
    assert!(album.body.iter().any(|k| k == "id"), "the endpoint's own parameters too");
    let cover = rest.iter().find(|r| r.endpoint == "getCoverArt").unwrap();
    assert!(cover.body.iter().any(|k| k == "size"));
}

#[tokio::test(flavor = "multi_thread")]
async fn without_form_post_requests_stay_get() {
    let server = FakeServer::start(state(false)).await;
    let client = server.client();
    exercise(&server, &client).await;
    assert_eq!(client.form_post().known(), Some(false));

    let sent = server.sent();
    assert!(sent.iter().all(|r| r.method == "GET"), "{sent:?}");
    for r in sent.iter().skip(1) {
        assert!(r.credentials_in_url(), "{r:?}");
        assert!(r.body.is_empty());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_server_is_asked_once() {
    let server = FakeServer::start(state(false)).await;
    let client = server.client();
    let copy = client.copy_with_policy(navia_core::client::WritePolicy::ReadOnly).unwrap();
    client.ping().await.unwrap();
    copy.ping().await.unwrap();
    client.album("al-Album").await.unwrap();
    assert_eq!(server.calls("getOpenSubsonicExtensions"), 1, "copies share the answer");
}

#[tokio::test(flavor = "multi_thread")]
async fn urls_for_outside_navia_keep_their_credentials() {
    let server = FakeServer::start(state(true)).await;
    let client = server.client();
    client.ping().await.unwrap();
    assert_eq!(client.form_post().known(), Some(true));
    let cover = client.cover_art_url("al-Album", Some(300)).unwrap();
    let stream = client.stream_url("s1", &StreamOptions::default()).unwrap();
    for url in [cover, stream] {
        let keys = query_keys(&url);
        for key in ["u", "t", "s", "v", "c", "id"] {
            assert!(keys.iter().any(|k| k == key), "{key} stays in a handed-out URL");
        }
    }
}

#[test]
fn a_ranged_stream_seeks_over_post() {
    let data: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    let mut files = ServerFiles::default();
    files.raw.insert("s1".into(), data.clone());
    let server = TestServer::start(files, Faults::default());
    server.state.form_post.store(true, std::sync::atomic::Ordering::Release);
    let client = Client::new(ClientConfig::new(server.url.clone(), Credentials::new("tester", "letmein"))).unwrap();
    let fetcher = HttpFetcher::new().unwrap().with_form_post(client.form_post());
    let url = client.stream_url("s1", &StreamOptions { format: Some("raw".into()), ..Default::default() }).unwrap();
    let config = StreamConfig { initial_chunk: 64 * 1024, max_chunk: 128 * 1024, read_ahead: 256 * 1024, max_retries: 2 };
    let stream = HttpStream::open(&fetcher, url, config).unwrap();
    assert!(stream.is_seekable());
    let mut reader = stream.reader();
    let mut head = vec![0u8; 1000];
    reader.read_exact(&mut head).unwrap();
    assert_eq!(head, data[..1000]);
    let at = 2_500_000u64;
    reader.seek(SeekFrom::Start(at)).unwrap();
    let mut middle = vec![0u8; 50_000];
    reader.read_exact(&mut middle).unwrap();
    assert_eq!(middle, data[at as usize..at as usize + 50_000]);

    let requests: Vec<_> = server.requests().into_iter().filter(|r| r.endpoint == "stream").collect();
    assert!(requests.len() >= 2, "a first chunk and a ranged fetch at the seek: {requests:?}");
    for r in &requests {
        assert_eq!(r.method, "POST", "{r:?}");
        assert!(r.range.is_some(), "{r:?}");
        assert!(r.query_keys.is_empty(), "{r:?}");
        assert!(["u", "t", "s", "id", "format"].iter().all(|k| r.body_keys.iter().any(|b| b == k)), "{r:?}");
    }
    let covers_seek = |range: &str| {
        let (a, b) = range.trim_start_matches("bytes=").split_once('-').unwrap();
        a.parse::<u64>().unwrap() <= at && b.parse::<u64>().unwrap() >= at
    };
    assert!(requests.iter().any(|r| r.range.as_deref().is_some_and(covers_seek)), "{requests:?}");
}

#[test]
fn downloads_go_as_post_too() {
    let mut files = ServerFiles::default();
    files.raw.insert("s1".into(), vec![7u8; 4096]);
    let server = TestServer::start(files, Faults::default());
    server.state.form_post.store(true, std::sync::atomic::Ordering::Release);
    let dir = tempfile::tempdir().unwrap();
    let client = Client::new(ClientConfig::new(server.url.clone(), Credentials::new("tester", "letmein"))).unwrap();
    let library = Library::open(&LibraryConfig { path: dir.path().join("lib.sqlite") }).unwrap();
    let downloads = Arc::new(DownloadManager::new(library, client, dir.path().join("downloads")).unwrap());
    downloads.enqueue(vec![DownloadRequest { id: "s1".into(), suffix: Some("bin".into()), size: None }]);
    let start = Instant::now();
    while server.requests().iter().all(|r| r.endpoint != "stream") && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(10));
    }
    let fetch = server.requests().into_iter().find(|r| r.endpoint == "stream").expect("the download was fetched");
    assert_eq!(fetch.method, "POST");
    assert!(fetch.query_keys.is_empty() && fetch.body_keys.iter().any(|k| k == "t"));
}
