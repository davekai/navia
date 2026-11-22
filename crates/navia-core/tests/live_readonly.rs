//! Checks against the real server, read-only because the client uses `WritePolicy::ReadOnly`.
//! Run with `cargo test -p navia-core --test live_readonly -- --ignored --nocapture`.
//! Credentials come from `.env.local` at the repo root and are never printed.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use navia_core::capabilities::{Capabilities, ExternalProbe};
use navia_core::client::{Client, ClientConfig, Credentials, TopSongsBy, WritePolicy};
use navia_core::library::sync::{RefreshOutcome, SyncOptions, SyncStatus, Syncer};
use navia_core::library::{Library, LibraryConfig};

fn load_env() -> HashMap<String, String> {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.pop();
    path.pop();
    path.push(".env.local");
    let text = std::fs::read_to_string(&path).expect(".env.local at the repo root");
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().trim_matches('"').to_string()))
        .collect()
}

fn client() -> Client {
    let env = load_env();
    let mut cfg = ClientConfig::new(env["NAVIDROME_URL"].clone(), Credentials::new(env["NAVIDROME_USER"].clone(), env["NAVIDROME_PASSWORD"].clone()));
    cfg.write_policy = WritePolicy::ReadOnly;
    Client::new(cfg).unwrap()
}

fn percentile(values: &mut [Duration], p: f64) -> Duration {
    values.sort();
    values[((values.len() as f64 - 1.0) * p).round() as usize]
}

/// One full sync, at most one refresh, and search timings. Keeps load modest on purpose.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "talks to the real server; run explicitly"]
async fn foundation_against_real_server() {
    let client = client();
    assert_eq!(client.write_policy(), WritePolicy::ReadOnly);

    let server = client.ping().await.unwrap();
    println!("server: {:?} {:?} api {} opensubsonic {}", server.kind, server.server_version, server.api_version, server.open_subsonic);

    let db_dir = tempfile::tempdir_in(std::env::var("NAVIA_LIVE_DB_DIR").unwrap_or_else(|_| std::env::temp_dir().display().to_string())).unwrap();
    let db_path = db_dir.path().join("library.sqlite");
    let library = Library::open(&LibraryConfig { path: db_path.clone() }).unwrap();

    let syncer = Syncer::new(client.clone(), library.clone(), SyncOptions::default());
    let pages = std::sync::atomic::AtomicUsize::new(0);
    let started = Instant::now();
    let outcome = syncer.full_sync(&|_| { pages.fetch_add(1, std::sync::atomic::Ordering::Relaxed); }).await.unwrap();
    let sync_secs = started.elapsed().as_secs_f64();
    assert_eq!(outcome.status, SyncStatus::Complete);
    let counts = library.counts().unwrap();
    let db_size = std::fs::metadata(&db_path).unwrap().len() + std::fs::metadata(db_path.with_extension("sqlite-wal")).map(|m| m.len()).unwrap_or(0);
    println!("full sync: {:.2} s, {} pages, {counts:?}, db {:.1} MB, libraries {:?}", sync_secs, pages.load(std::sync::atomic::Ordering::Relaxed), db_size as f64 / 1_048_576.0, outcome.libraries);
    let scan = client.scan_status().await.unwrap();
    println!("server scan status count: {:?}, last scan {:?}", scan.count, scan.last_scan);
    assert_eq!(Some(counts.songs), scan.count, "every track accounted for");

    // A library-wide refresh with no change costs one call.
    let refresh = syncer.refresh(false, &|_| {}).await.unwrap();
    assert!(matches!(refresh, RefreshOutcome::Unchanged), "{refresh:?}");

    // Search timings over a spread of real queries.
    let queries = ["a", "al", "the", "beat", "love", "نور", "عمر", "하", "天", "ceremony", "live at", "feat remix", "qqqqqq"];
    let mut timings = Vec::new();
    for _ in 0..30 {
        for q in queries {
            let t = Instant::now();
            let results = library.search(q, 20, None).unwrap();
            timings.push(t.elapsed());
            std::hint::black_box(results);
        }
    }
    println!("search over {} queries: p50 {:?}, p95 {:?}, p99 {:?}, max {:?}", timings.len(), percentile(&mut timings, 0.5), percentile(&mut timings, 0.95), percentile(&mut timings, 0.99), percentile(&mut timings, 1.0));
    let sample = library.search("beat", 3, None).unwrap();
    println!("sample 'beat': {} artists, {} albums, {} songs", sample.artists.len(), sample.albums.len(), sample.songs.len());

    // Capabilities, probing with a real artist that has albums.
    let probe = ExternalProbe::from_library(&library).unwrap();
    let caps = Capabilities::detect(&client, probe.as_ref()).await.unwrap();
    println!("extensions: {:?}", caps.extensions.iter().map(|e| e.name.as_str()).collect::<Vec<_>>());
    println!("libraries: {:?}", caps.libraries);
    println!("probed artist: {:?}", probe.as_ref().map(|p| p.artist_name.as_str()));
    println!("sharing: {:?}", caps.sharing);
    println!("artist info: {:?}", caps.artist_info);
    println!("top songs: {:?}", caps.top_songs);
    println!("star ratings: {:?}", caps.star_ratings);
    println!("favorites: {:?}", caps.favorites);
    println!("synced lyrics: {:?}, top by id: {:?}", caps.synced_lyrics.support, caps.top_songs_by_id.support);

    // Typed calls for the rest of the required surface, one each.
    let song = library.list_songs(navia_core::library::SongSort::Title, 0, 1, None).unwrap().remove(0);
    let album = client.album(song.album_id.as_deref().unwrap()).await.unwrap();
    let artist_id = song.artist_id.clone().unwrap();
    let artist = client.artist(&artist_id).await.unwrap();
    let playlists = client.playlists().await.unwrap();
    let stations = client.internet_radio_stations().await.unwrap();
    let queue = client.play_queue().await.unwrap();
    let lyrics = client.lyrics_by_song_id(&song.id, false).await.unwrap();
    let info = client.artist_info2(&artist_id, Some(3)).await.unwrap();
    let top = client.top_songs(&TopSongsBy::Name(artist.name.clone()), 3).await.unwrap();
    let genres = client.genres().await.unwrap();
    let cover = client.cover_art_url(album.cover_art.as_deref().unwrap_or_default(), Some(256)).unwrap();
    assert!(cover.contains("/rest/getCoverArt"));
    println!("album '{}' has {} songs; artist {} has {} albums; {} playlists, {} radio, queue {} entries, lyrics {} sets, bio {} chars, top {} songs, {} genres", album.name, album.song.len(), artist.name, artist.album.len(), playlists.len(), stations.len(), queue.map(|q| q.entry.len()).unwrap_or(0), lyrics.len(), info.biography.map(|b| b.len()).unwrap_or(0), top.len(), genres.len());
}

/// Playlists, stars, play counts and artwork against the real server, read-only. One full
/// sync, one playlist pass, one annotation pass and a handful of covers.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "talks to the real server; run explicitly"]
async fn extras_against_real_server() {
    use navia_core::artwork::{ArtworkCache, ArtworkConfig};

    let client = client();
    assert_eq!(client.write_policy(), WritePolicy::ReadOnly);
    let dir = tempfile::tempdir_in(std::env::var("NAVIA_LIVE_DB_DIR").unwrap_or_else(|_| std::env::temp_dir().display().to_string())).unwrap();
    let library = Library::open(&LibraryConfig { path: dir.path().join("library.sqlite") }).unwrap();
    let syncer = Syncer::new(client.clone(), library.clone(), SyncOptions::default());
    syncer.full_sync(&|_| {}).await.unwrap();

    let started = Instant::now();
    let first = syncer.sync_playlists().await.unwrap();
    let elapsed = started.elapsed();
    println!("playlists: {first:?} in {elapsed:?}, stored {}", library.playlists().unwrap().len());
    let again = syncer.sync_playlists().await.unwrap();
    assert_eq!(again.refetched, 0, "second pass refetches nothing");

    let started = Instant::now();
    let refresh = syncer.refresh_annotations().await.unwrap();
    println!("annotations: {refresh:?} in {:?}", started.elapsed());
    let second = syncer.refresh_annotations().await.unwrap();
    println!("annotations again: {second:?}");

    let art_dir = dir.path().join("art");
    let cache = ArtworkCache::new(client.clone(), ArtworkConfig { dir: art_dir.clone(), max_bytes: u64::MAX }).unwrap();
    let albums = library.list_albums(navia_core::library::AlbumSort::RecentlyAdded, 0, 12, None).unwrap();
    let mut fetch = Vec::new();
    let mut glow = Vec::new();
    for album in albums.iter().filter(|a| a.cover_art.is_some()) {
        let id = album.cover_art.as_deref().unwrap();
        let t = Instant::now();
        let cover = cache.cover(id, Some(600)).await.unwrap();
        fetch.push(t.elapsed());
        let t = Instant::now();
        let g = cache.glow(id, 640, 640).await.unwrap();
        glow.push(t.elapsed());
        println!("{} {}x{} tint {:?} glow bytes {}", album.name, cover.width, cover.height, cover.tint, std::fs::metadata(&g.path).unwrap().len());
    }
    println!("cover p50 {:?}, glow p50 {:?} ({} covers)", percentile(&mut fetch, 0.5), percentile(&mut glow, 0.5), fetch.len());
    let total: u64 = std::fs::read_dir(&art_dir).unwrap().filter_map(|e| e.ok()?.metadata().ok()).map(|m| m.len()).sum();
    println!("artwork dir {} KB", total / 1024);
}

/// Streaming and seeking against the real server, read-only. Fetches parts of a few tracks.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "talks to the real server; run explicitly"]
async fn streaming_and_seeking_against_real_server() {
    use navia_core::audio::stream::{HttpFetcher, HttpStream, StreamConfig};
    use navia_core::audio::{Decoder, TrackSource};
    use navia_core::client::StreamOptions;

    let client = client();
    assert_eq!(client.write_policy(), WritePolicy::ReadOnly);
    let dir = tempfile::tempdir_in(std::env::var("NAVIA_LIVE_DB_DIR").unwrap_or_else(|_| std::env::temp_dir().display().to_string())).unwrap();
    let library = Library::open(&LibraryConfig { path: dir.path().join("library.sqlite") }).unwrap();
    Syncer::new(client.clone(), library.clone(), SyncOptions::default()).full_sync(&|_| {}).await.unwrap();
    let fetcher = HttpFetcher::new().unwrap();
    let raw = StreamOptions { format: Some("raw".to_string()), ..Default::default() };

    let songs = library.list_songs(navia_core::library::SongSort::Title, 0, 4000, None).unwrap();
    for suffix in ["flac", "mp3", "m4a", "opus"] {
        let Some(song) = songs.iter().find(|s| s.suffix.as_deref() == Some(suffix) && s.duration.is_some_and(|d| (120..400).contains(&d))) else {
            println!("{suffix}: no suitable track");
            continue;
        };
        let (id, url) = (song.id.clone(), client.stream_url(&song.id, &raw).unwrap());
        let (fetcher, url2) = (fetcher.clone(), url.clone());
        tokio::task::spawn_blocking(move || {
            let decode_all = |d: &mut Decoder| {
                let (mut all, mut b) = (Vec::new(), Vec::new());
                while d.next_block(&mut b).unwrap() {
                    all.extend_from_slice(&b);
                }
                all
            };
            let full_stream = HttpStream::open(&fetcher, url, StreamConfig::default()).unwrap();
            let mut full = Decoder::open_source(&TrackSource::Stream(full_stream.clone())).unwrap();
            let rate = full.sample_rate as u64;
            let reference = decode_all(&mut full);
            let frames = reference.len() as u64 / 2;
            let full_stats = full_stream.stats();

            let stream = HttpStream::open(&fetcher, url2, StreamConfig::default()).unwrap();
            let t = Instant::now();
            let mut d = Decoder::open_source(&TrackSource::Stream(stream.clone())).unwrap();
            let open_ms = t.elapsed().as_secs_f64() * 1000.0;
            let mut worst_lag = 0i64;
            let mut exact = true;
            let mut times = Vec::new();
            for fraction in [0.71f64, 0.13, 0.92, 0.42] {
                let target = (frames as f64 * fraction) as u64;
                let t = Instant::now();
                d.seek(target).unwrap();
                let (mut got, mut b) = (Vec::new(), Vec::new());
                while got.len() < 4096 * 2 && d.next_block(&mut b).unwrap() {
                    got.extend_from_slice(&b);
                }
                times.push(t.elapsed());
                got.truncate(4096 * 2);
                let start = target as usize * 2;
                let expected = &reference[start..start + got.len()];
                let identical = got.iter().zip(expected).all(|(a, b)| a.to_bits() == b.to_bits());
                exact &= identical;
                if !identical {
                    let skip = 2048 * 2;
                    let (lag, _) = best_lag(&got[skip..], &expected[skip..], 2500, 1000);
                    worst_lag = worst_lag.max(lag.abs());
                }
            }
            let st = stream.stats();
            println!(
                "{suffix} id {id}: {frames} frames @ {rate}, open {open_ms:.0} ms, 4 seeks bit-exact: {exact}, worst lag {worst_lag}, seek p50 {:?} max {:?}, partial {} requests {} KB vs full {} requests {} KB",
                percentile(&mut times, 0.5), percentile(&mut times, 1.0), st.requests, st.bytes / 1024, full_stats.requests, full_stats.bytes / 1024
            );
            assert_eq!(worst_lag, 0, "{suffix} landed off target");
        })
        .await
        .unwrap();
    }

    // A server transcode, sequential, and restarted at an offset.
    let song = songs.iter().find(|s| s.suffix.as_deref() == Some("flac") && s.duration.is_some_and(|d| (120..400).contains(&d))).unwrap();
    for (label, offset) in [("start", None), ("timeOffset 60", Some(60u32))] {
        let opts = StreamOptions { format: Some("mp3".to_string()), time_offset: offset, ..Default::default() };
        let url = client.stream_url(&song.id, &opts).unwrap();
        let fetcher = fetcher.clone();
        tokio::task::spawn_blocking(move || {
            let t = Instant::now();
            let stream = HttpStream::open(&fetcher, url, StreamConfig::default()).unwrap();
            assert!(!stream.is_seekable(), "live transcodes have no ranges");
            let mut d = Decoder::open_source(&TrackSource::Stream(stream)).unwrap();
            let mut b = Vec::new();
            let mut frames = 0;
            while frames < 44_100 * 3 && d.next_block(&mut b).unwrap() {
                frames += b.len() / 2;
            }
            println!("transcode mp3 ({label}): {} {} Hz, {frames} frames in {:?}", d.codec, d.sample_rate, t.elapsed());
            assert!(frames >= 44_100 * 3);
        })
        .await
        .unwrap();
    }
}

fn best_lag(a: &[f32], b: &[f32], max_shift: i64, window: usize) -> (i64, f32) {
    let mut best = (i64::MAX, f32::MAX, f64::MAX);
    for shift in -max_shift..=max_shift {
        let (mut sum, mut worst, mut count) = (0.0f64, 0.0f32, 0);
        for i in 0..window {
            let ai = (i as i64 + shift) * 2;
            if ai < 0 || ai as usize + 1 >= a.len() || i * 2 + 1 >= b.len() {
                continue;
            }
            let (x, y) = (a[ai as usize] - b[i * 2], a[ai as usize + 1] - b[i * 2 + 1]);
            sum += (x * x + y * y) as f64;
            worst = worst.max(x.abs()).max(y.abs());
            count += 1;
        }
        if count > window / 2 && sum < best.2 {
            best = (shift, worst, sum);
        }
    }
    (best.0, best.1)
}

/// an album's copyright tags from the native API (`NAVIA_PROBE_ALBUM`, an album id). logs in once.
#[tokio::test]
#[ignore]
async fn real_server_album_copyrights() {
    let Ok(album) = std::env::var("NAVIA_PROBE_ALBUM") else { return };
    let values = client().album_copyrights(&album).await.unwrap();
    let lines = navia_core::library::copyright::footer(&values, &[]).map(|f| f.lines).unwrap_or_default();
    println!("{} tag values -> {lines:?}", values.len());
}

/// one animated cover, decoded twice: the first call fetches and decodes, the second reads the
/// cache. `NAVIA_ANIM_COVER=<coverArt id>`; GETs only.
#[tokio::test]
#[ignore]
async fn animated_cover() {
    use navia_core::artwork::{ArtworkCache, ArtworkConfig};
    let id = std::env::var("NAVIA_ANIM_COVER").expect("NAVIA_ANIM_COVER");
    let dir = tempfile::tempdir().unwrap();
    let cache = ArtworkCache::new(client(), ArtworkConfig { dir: dir.path().to_path_buf(), max_bytes: 1 << 30 }).unwrap();
    for size in [600u32, 300] {
        let start = Instant::now();
        let art = cache.animated_artwork(&id, size).await.unwrap().expect("animated");
        let first = start.elapsed();
        let start = Instant::now();
        let again = cache.animated_artwork(&id, size).await.unwrap().unwrap();
        let frames = again.frames().unwrap();
        println!(
            "size {size}: {}x{} {} frames, {} ms loop, opaque {}, file {} KB, decoded {} KB; first {:?}, cached + all frames {:?}",
            art.width,
            art.height,
            art.frame_count(),
            art.delays_ms.iter().sum::<u32>(),
            art.opaque,
            std::fs::metadata(&art.path).unwrap().len() / 1024,
            art.decoded_bytes() / 1024,
            first,
            start.elapsed()
        );
        assert_eq!(frames.len(), art.frame_count());
    }
    assert_eq!(cache.is_animated(&id), Some(true));
    if let Ok(still) = std::env::var("NAVIA_STILL_COVER") {
        let start = Instant::now();
        assert!(cache.animated_artwork(&still, 600).await.unwrap().is_none());
        println!("still cover checked in {:?}", start.elapsed());
        assert_eq!(cache.is_animated(&still), Some(false));
    }
}
