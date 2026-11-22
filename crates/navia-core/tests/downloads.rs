//! Offline downloads against a local fake of Navidrome's `/stream`.

mod common;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::audio::{synth, to_f32, wav_bytes};
use common::http::{Faults, Library as ServerFiles, TestServer};
use navia_core::audio::output::CaptureBackend;
use navia_core::audio::player::{OutputPolicy, Player, PlayerConfig, PlayerStatus, QueueItem, ServerResolver};
use navia_core::client::{Client, ClientConfig, Credentials};
use navia_core::downloads::{DownloadEvent, DownloadManager, DownloadRequest};
use navia_core::library::{Library, LibraryConfig};

fn client(url: &str) -> Client {
    Client::new(ClientConfig::new(url.to_string(), Credentials::new("t".to_string(), "t".to_string()))).unwrap()
}

fn manager(url: &str, dir: &Path) -> (Library, Arc<DownloadManager>, Arc<Mutex<Vec<DownloadEvent>>>) {
    let library = Library::open(&LibraryConfig { path: dir.join("lib.sqlite") }).unwrap();
    let manager = Arc::new(DownloadManager::new(library.clone(), client(url), dir.join("downloads")).unwrap());
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    manager.add_listener(move |e| sink.lock().unwrap().push(e.clone()));
    (library, manager, events)
}

fn req(id: &str) -> DownloadRequest {
    DownloadRequest { id: id.to_string(), suffix: Some("wav".to_string()), size: None }
}

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

fn wav(seed: f64) -> Vec<u8> {
    wav_bytes(&synth(48_000, 48_000, seed), 48_000)
}

#[test]
fn downloads_the_original_bytes_and_survives_a_restart() {
    let data = wav(0.3);
    let mut files = ServerFiles::default();
    files.raw.insert("a".into(), data.clone());
    let server = TestServer::start(files, Faults::default());
    let dir = tempfile::tempdir().unwrap();

    let (library, downloads, events) = manager(&server.url, dir.path());
    assert_eq!(downloads.enqueue(vec![req("a")]), 1);
    assert!(wait_for(Duration::from_secs(5), || downloads.local_path("a").is_some()), "{:?}", events.lock().unwrap());
    let path = downloads.local_path("a").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), data, "exact original bytes");
    assert!(events.lock().unwrap().contains(&DownloadEvent::Finished { id: "a".into(), size: data.len() as u64 }));
    assert_eq!(server.requests().iter().find(|r| r.id == "a").unwrap().format.as_deref(), Some("raw"));
    assert_eq!(downloads.total_size().unwrap(), data.len() as u64);

    // Queueing it again does nothing, and a new manager finds the same file.
    assert_eq!(downloads.enqueue(vec![req("a")]), 0);
    drop(downloads);
    assert!(wait_for(Duration::from_secs(2), || Arc::strong_count(&Arc::new(())) >= 1));
    let again = DownloadManager::new(library, client(&server.url), dir.path().join("downloads")).unwrap();
    assert_eq!(again.local_path("a"), Some(path));
    assert_eq!(again.downloaded_ids().unwrap(), vec!["a".to_string()]);
}

#[test]
fn a_transfer_cut_short_is_not_kept() {
    let data = wav(0.4);
    let mut files = ServerFiles::default();
    files.raw.insert("a".into(), data);
    let server = TestServer::start(files, Faults::default());
    // Serve a body shorter than the Content-Length it claims.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut buf = [0u8; 2048];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: audio/flac\r\nContent-Length: 500000\r\n\r\n");
            let _ = stream.write_all(&[0u8; 1000]);
            // Dropped here, so the transfer ends early.
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let (_library, downloads, events) = manager(&url, dir.path());
    downloads.enqueue(vec![req("a")]);
    assert!(wait_for(Duration::from_secs(20), || events.lock().unwrap().iter().any(|e| matches!(e, DownloadEvent::Failed { .. }))), "{:?}", events.lock().unwrap());
    assert!(downloads.local_path("a").is_none());
    let leftover = walk(&dir.path().join("downloads"));
    assert!(leftover.is_empty(), "left behind: {leftover:?}");
    let _ = server;
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if e.path().is_dir() {
                out.extend(walk(&e.path()));
            } else {
                out.push(e.path());
            }
        }
    }
    out
}

#[test]
fn cancelling_stops_at_once_and_removes_a_finished_download() {
    let data = vec![0x41u8; 40_000_000];
    let small = wav(0.9);
    let mut files = ServerFiles::default();
    files.raw.insert("big".into(), data);
    files.raw.insert("small".into(), small.clone());
    let server = TestServer::start(files, Faults { throttle: 500_000, ..Faults::default() });
    let dir = tempfile::tempdir().unwrap();
    let (_library, downloads, events) = manager(&server.url, dir.path());

    downloads.enqueue(vec![req("big")]);
    assert!(wait_for(Duration::from_secs(5), || events.lock().unwrap().iter().any(|e| matches!(e, DownloadEvent::Progress { id, .. } if id == "big"))), "{:?}", events.lock().unwrap());
    let cancelled_at = Instant::now();
    downloads.cancel(&["big".to_string()]);
    assert!(wait_for(Duration::from_secs(3), || downloads.in_progress().is_empty()), "cancel didn't stop the worker");
    assert!(cancelled_at.elapsed() < Duration::from_secs(2), "took {:?} to stop", cancelled_at.elapsed());
    assert!(downloads.local_path("big").is_none(), "cancelled download kept");
    assert!(walk(&dir.path().join("downloads")).iter().all(|p| p.extension().is_none_or(|e| e != "part")), "partial left behind");

    downloads.enqueue(vec![req("small")]);
    assert!(wait_for(Duration::from_secs(5), || downloads.local_path("small").is_some()));
    let path = downloads.local_path("small").unwrap();
    downloads.remove(&["small".to_string()]).unwrap();
    assert!(!path.exists(), "the file is deleted");
    assert_eq!(downloads.local_path("small"), None);
    assert_eq!(downloads.total_size().unwrap(), 0);
    assert!(events.lock().unwrap().contains(&DownloadEvent::Removed { id: "small".into() }));
}

#[test]
fn only_a_couple_of_downloads_run_at_once() {
    // A server that counts how many responses it is sending at the same time.
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (now, peak) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let body = Arc::new(wav(1.0));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (n2, p2) = (now.clone(), peak.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = match stream {
                Ok(s) => s,
                Err(_) => return,
            };
            let (now, peak, body) = (n2.clone(), p2.clone(), body.clone());
            std::thread::spawn(move || {
                let mut req = [0u8; 2048];
                let _ = stream.read(&mut req);
                let active = now.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(active, Ordering::SeqCst);
                let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: audio/flac\r\nConnection: close\r\nContent-Length: {}\r\n\r\n", body.len());
                let _ = stream.write_all(&body[..body.len() / 2]);
                let _ = stream.flush();
                std::thread::sleep(Duration::from_millis(200));
                let _ = stream.write_all(&body[body.len() / 2..]);
                now.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let (_library, downloads, _events) = manager(&url, dir.path());
    downloads.enqueue((0..6).map(|i| req(&format!("s{i}"))).collect());
    assert!(wait_for(Duration::from_secs(20), || downloads.downloaded_ids().unwrap().len() == 6), "not all finished: {:?}", downloads.in_progress());
    assert_eq!(peak.load(Ordering::SeqCst), 2, "exactly two ran at once");
}

#[test]
fn downloaded_songs_play_with_the_server_unreachable_and_stay_exact() {
    let pcm = synth(48_000 * 2, 48_000, 0.7);
    let mut files = ServerFiles::default();
    files.raw.insert("a".into(), wav_bytes(&pcm, 48_000));
    let server = TestServer::start(files, Faults::default());
    let dir = tempfile::tempdir().unwrap();
    let (library, downloads, events) = manager(&server.url, dir.path());
    downloads.enqueue(vec![req("a")]);
    assert!(wait_for(Duration::from_secs(5), || downloads.local_path("a").is_some()), "{:?}", events.lock().unwrap());
    drop(downloads);

    // The same data now, but the client can't reach any server.
    let offline = client("http://127.0.0.1:9");
    let downloads = Arc::new(DownloadManager::new(library, offline.clone(), dir.path().join("downloads")).unwrap());
    // paced: a capture pulling as fast as it can records silence whenever the decoder falls behind
    let backend = CaptureBackend { speed: 25.0, ..Default::default() };
    let capture = backend.capture.clone();
    let resolver = ServerResolver::new(offline).unwrap().with_downloads(downloads);
    let player = Player::with_backend(Arc::new(resolver), Box::new(backend), PlayerConfig { policy: OutputPolicy::FollowTrack, ..Default::default() }).unwrap();
    player.set_queue(vec![QueueItem::new("a".into(), Some(2000))], 0, 0, false);
    assert!(wait_for(Duration::from_secs(5), || player.snapshot().status == PlayerStatus::Stopped));
    assert_eq!(player.snapshot().underrun_frames, 0, "underruns");
    let samples = capture.samples();
    let first = samples.chunks_exact(2).position(|f| f[0] != 0.0 || f[1] != 0.0).unwrap_or(0);
    let expected = to_f32(&pcm);
    assert_eq!(&samples[first * 2..first * 2 + expected.len()], &expected[..], "bit-exact from the file on disk");
}
