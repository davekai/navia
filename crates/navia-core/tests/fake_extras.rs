//! Playlists, stars, play counts, durable plays and artwork against the local fake server.

mod common;

use std::sync::Arc;

use common::{FakeServer, State, song};
use navia_core::artwork::{ArtworkCache, ArtworkConfig};
use navia_core::library::sync::{SyncOptions, Syncer};
use navia_core::library::{Library, LibraryConfig, StarTable};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, ResponseTemplate};
use navia_core::reporting::{PlayReporter, ReportSink, TrackPlay};
use serde_json::{Value, json};

fn state() -> State {
    State {
        folders: vec![("1".into(), "Music".into())],
        open_subsonic: true,
        last_scan: Some("2026-01-01T00:00:00Z".into()),
        songs: vec![song("s1", "One", "Art", "Alb", "1"), song("s2", "Two", "Art", "Alb", "1")],
        ..Default::default()
    }
}

fn syncer(server: &FakeServer, library: &Library) -> Syncer {
    Syncer::new(server.client(), library.clone(), SyncOptions::default())
}

fn open_library() -> (tempfile::TempDir, Library) {
    let dir = tempfile::tempdir().unwrap();
    let library = Library::open(&LibraryConfig { path: dir.path().join("l.sqlite") }).unwrap();
    (dir, library)
}

fn entry(id: &str) -> Value {
    json!({"id": id, "title": format!("Song {id}")})
}

fn set(server: &FakeServer, key: &str, body: Value) {
    server.state.lock().unwrap().canned.insert(key.to_string(), body);
}

#[tokio::test(flavor = "multi_thread")]
async fn playlists_are_stored_refetched_only_when_changed_and_removed_when_gone() {
    let server = FakeServer::start(state()).await;
    let (_dir, library) = open_library();
    let sync = syncer(&server, &library);
    sync.full_sync(&|_| {}).await.unwrap();

    set(&server, "getPlaylists", json!({"playlists": {"playlist": [
        {"id": "p1", "name": "Mix", "songCount": 2, "changed": "2026-01-01T00:00:00Z"},
        {"id": "p2", "name": "Smart", "songCount": 3, "changed": "2026-01-02T00:00:00Z", "readonly": true},
    ]}}));
    set(&server, "getPlaylist:p1", json!({"playlist": {"id": "p1", "name": "Mix", "songCount": 2, "changed": "2026-01-01T00:00:00Z", "entry": [entry("s2"), entry("not-in-library")]}}));
    // Fetching a smart playlist moves its stamp, and the list's count differs from the entries.
    set(&server, "getPlaylist:p2", json!({"playlist": {"id": "p2", "name": "Smart", "songCount": 1, "changed": "2026-01-02T12:00:00Z", "readonly": true, "entry": [entry("s1")]}}));

    let first = sync.sync_playlists().await.unwrap();
    assert_eq!((first.listed, first.refetched), (2, 1), "smart playlists aren't fetched in the background");
    assert_eq!(server.calls("getPlaylist"), 1);
    assert!(library.playlist("p2").unwrap().unwrap().entry.is_empty(), "stored from the list alone");
    sync.refresh_playlist("p2").await.unwrap();
    assert_eq!(library.playlist("p2").unwrap().unwrap().entry.len(), 1, "fetched when opened");
    let stored = library.playlist("p1").unwrap().unwrap();
    assert_eq!(stored.entry.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["s2", "not-in-library"]);
    assert_eq!(stored.entry[1].title, "Song not-in-library", "entries missing from the library still show");
    assert_eq!(library.playlists().unwrap().len(), 2);

    let calls_before = server.calls("getPlaylist");
    let second = sync.sync_playlists().await.unwrap();
    assert_eq!(second.refetched, 0, "unchanged playlists aren't fetched again");
    assert_eq!(server.calls("getPlaylist"), calls_before);

    // The server changes p1 and drops p2.
    set(&server, "getPlaylists", json!({"playlists": {"playlist": [{"id": "p1", "name": "Mix", "songCount": 1, "changed": "2026-02-01T00:00:00Z"}]}}));
    set(&server, "getPlaylist:p1", json!({"playlist": {"id": "p1", "name": "Mix", "songCount": 1, "changed": "2026-02-01T00:00:00Z", "entry": [entry("s1")]}}));
    let third = sync.sync_playlists().await.unwrap();
    assert_eq!((third.refetched, third.removed), (1, 1));
    assert_eq!(library.playlist("p1").unwrap().unwrap().entry.len(), 1);
    assert!(library.playlist("p2").unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn stars_and_play_counts_refresh_without_a_catalog_sync() {
    let server = FakeServer::start(state()).await;
    let (_dir, library) = open_library();
    let sync = syncer(&server, &library);
    sync.full_sync(&|_| {}).await.unwrap();
    let search_calls = server.calls("search3");

    set(&server, "getStarred2", json!({"starred2": {"song": [{"id": "s1", "title": "One", "starred": "2026-03-01T00:00:00Z"}], "album": [{"id": "al-Alb", "name": "Alb", "starred": "2026-03-02T00:00:00Z"}]}}));
    set(&server, "getAlbumList2", json!({"albumList2": {"album": [{"id": "al-Alb", "name": "Alb", "playCount": 4, "played": "2026-03-05T00:00:00Z"}]}}));
    set(&server, "getAlbum:al-Alb", json!({"album": {"id": "al-Alb", "name": "Alb", "playCount": 4, "played": "2026-03-05T00:00:00Z", "song": [
        {"id": "s1", "title": "One", "playCount": 3, "played": "2026-03-05T00:00:00Z"},
        {"id": "s2", "title": "Two", "playCount": 1, "played": "2026-03-04T00:00:00Z"},
    ]}}));

    let first = sync.refresh_annotations().await.unwrap();
    assert_eq!(server.calls("search3"), search_calls, "no catalog sync");
    assert_eq!(first.albums_refetched, 1);
    assert_eq!(first.songs_changed, 2);
    assert_eq!(first.stars_changed, 2);
    let s1 = library.song("s1").unwrap().unwrap();
    assert_eq!(s1.play_count, Some(3));
    assert_eq!(s1.played.as_deref(), Some("2026-03-05T00:00:00Z"));
    assert!(s1.starred.is_some());
    assert_eq!(library.album("al-Alb").unwrap().unwrap().play_count, Some(4));

    // Nothing moved, so only the two list calls and no per-album fetch.
    let album_calls = server.calls("getAlbum");
    let second = sync.refresh_annotations().await.unwrap();
    assert_eq!((second.albums_refetched, second.stars_changed), (0, 0));
    assert_eq!(server.calls("getAlbum"), album_calls);

    // An unstar on the server clears the local star.
    set(&server, "getStarred2", json!({"starred2": {}}));
    let third = sync.refresh_annotations().await.unwrap();
    assert_eq!(third.stars_changed, 2);
    assert!(library.song("s1").unwrap().unwrap().starred.is_none());
    assert!(library.album("al-Alb").unwrap().unwrap().starred.is_none());
}

struct OfflineSink;

#[async_trait::async_trait]
impl ReportSink for OfflineSink {
    async fn now_playing(&self, _: &str) -> navia_core::Result<()> {
        Ok(())
    }
    async fn submit(&self, _: &str, _: u64) -> navia_core::Result<()> {
        Err(navia_core::Error::Network("offline".into()))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn unsent_plays_survive_reopening_the_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("l.sqlite");
    {
        let library = Library::open(&LibraryConfig { path: path.clone() }).unwrap();
        let reporter = PlayReporter::with_store(Arc::new(OfflineSink), Arc::new(library.clone()));
        reporter.track_started(TrackPlay { song_id: "s9".into(), duration: std::time::Duration::from_secs(200), started_at_ms: 1_700_000_000_000 }).await;
        reporter.advance(std::time::Duration::from_secs(150)).await;
        assert_eq!(reporter.queued(), 1);
    }
    // A fresh process opens the same file after a "crash" and sends it.
    let library = Library::open(&LibraryConfig { path }).unwrap();
    assert_eq!(library.unsent_plays().unwrap().len(), 1);
    let server = FakeServer::start(state()).await;
    let reporter = PlayReporter::with_store(Arc::new(server.client()), Arc::new(library.clone()));
    reporter.flush().await.unwrap();
    assert!(library.unsent_plays().unwrap().is_empty());
    let scrobbles = server.state.lock().unwrap().scrobbles.clone();
    assert_eq!(scrobbles, vec![("s9".to_string(), "true".to_string(), Some("1700000000000".to_string()))]);
}

#[tokio::test(flavor = "multi_thread")]
async fn play_threshold_is_saved_with_the_account() {
    use navia_core::reporting::PlayThreshold;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("l.sqlite");
    {
        let library = Library::open(&LibraryConfig { path: path.clone() }).unwrap();
        let reporter = PlayReporter::with_store(Arc::new(OfflineSink), Arc::new(library.clone()));
        assert_eq!(reporter.play_threshold(), PlayThreshold::default(), "60% with the cap when never set");
        reporter.set_play_threshold(PlayThreshold::new(75, false).unwrap()).unwrap();
    }
    let library = Library::open(&LibraryConfig { path }).unwrap();
    let reporter = PlayReporter::with_store(Arc::new(OfflineSink), Arc::new(library.clone()));
    assert_eq!(reporter.play_threshold(), PlayThreshold { percent: 75, cap_at_four_minutes: false });
    // It's the rule used. 75% of 400 s is 300 s, past four minutes, since the cap is off.
    reporter.track_started(TrackPlay { song_id: "s1".into(), duration: std::time::Duration::from_secs(400), started_at_ms: 1 }).await;
    reporter.advance(std::time::Duration::from_secs(299)).await;
    assert_eq!(reporter.queued(), 0);
    reporter.advance(std::time::Duration::from_secs(1)).await;
    assert_eq!(reporter.queued(), 1);
}

fn png(width: u32, height: u32, rgb: [u8; 3]) -> Vec<u8> {
    let image = image::ImageBuffer::from_pixel(width, height, image::Rgba([rgb[0], rgb[1], rgb[2], 255]));
    let mut bytes = Vec::new();
    image.write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png).unwrap();
    bytes
}

#[tokio::test(flavor = "multi_thread")]
async fn artwork_is_fetched_once_cached_tinted_and_glowed() {
    let server = FakeServer::start(State { cover_png: Some(png(300, 300, [200, 60, 40])), ..state() }).await;
    let dir = tempfile::tempdir().unwrap();
    let cache = ArtworkCache::new(server.client(), ArtworkConfig { dir: dir.path().join("art"), max_bytes: u64::MAX }).unwrap();

    let cover = cache.cover("al-1", Some(300)).await.unwrap();
    assert_eq!((cover.width, cover.height), (300, 300));
    assert!(cover.tint[0] > 150 && cover.tint[1] < 90, "tint {:?}", cover.tint);
    assert!(cover.path.exists());

    let again = cache.cover("al-1", Some(300)).await.unwrap();
    assert_eq!(again.path, cover.path);
    assert_eq!(server.calls("getCoverArt"), 1, "served from cache");

    let glow = cache.glow("al-1", 400, 400).await.unwrap();
    assert!(glow.path.exists());
    let decoded = image::open(&glow.path).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (400, 400));
    assert_eq!(server.calls("getCoverArt"), 1, "the glow reuses the cached cover");
}

fn sidecars(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect()
}

/// an image marked `no-store` (Navidrome's no-cover placeholder) is reused for a while but never
/// cached for good, so a cover added later on the server shows after a relaunch.
#[tokio::test(flavor = "multi_thread")]
async fn a_no_store_placeholder_isnt_kept_for_the_cover() {
    let server = FakeServer::start(State { cover_png: Some(png(64, 64, [90, 90, 90])), cover_no_store: true, ..state() }).await;
    let dir = tempfile::tempdir().unwrap();
    let config = ArtworkConfig { dir: dir.path().join("art"), max_bytes: u64::MAX };
    let cache = ArtworkCache::new(server.client(), config.clone()).unwrap();
    let placeholder = cache.cover("al-1", Some(300)).await.unwrap();
    assert!(placeholder.path.exists());
    assert_eq!(cache.cover("al-1", Some(300)).await.unwrap().path, placeholder.path);
    assert_eq!(server.calls("getCoverArt"), 1, "reused while fresh");
    let glow = cache.glow("al-1", 40, 40).await.unwrap();
    assert!(glow.path.starts_with(placeholder.path.parent().unwrap()));
    assert!(sidecars(&config.dir).is_empty(), "nothing cached under the cover's own name");

    {
        let mut st = server.state.lock().unwrap();
        st.cover_png = Some(png(64, 64, [200, 60, 40]));
        st.cover_no_store = false;
    }
    let relaunched = ArtworkCache::new(server.client(), config).unwrap();
    let real = relaunched.cover("al-1", Some(300)).await.unwrap();
    assert!(real.tint[0] > 150, "the real cover, tint {:?}", real.tint);
}

/// a placeholder cached before it was recognised is dropped once the same image comes marked
/// `no-store`.
#[tokio::test(flavor = "multi_thread")]
async fn an_old_cached_placeholder_is_dropped_once_recognised() {
    let server = FakeServer::start(State { cover_png: Some(png(64, 64, [90, 90, 90])), ..state() }).await;
    let dir = tempfile::tempdir().unwrap();
    let config = ArtworkConfig { dir: dir.path().join("art"), max_bytes: u64::MAX };
    let cache = ArtworkCache::new(server.client(), config.clone()).unwrap();
    cache.cover("al-old", Some(300)).await.unwrap();
    assert_eq!(sidecars(&config.dir).len(), 1);

    server.state.lock().unwrap().cover_no_store = true;
    cache.cover("al-new", Some(300)).await.unwrap();
    let relaunched = ArtworkCache::new(server.client(), config.clone()).unwrap();
    relaunched.cover("al-old", Some(300)).await.unwrap();
    assert_eq!(server.calls("getCoverArt"), 3, "asked again instead of served from the old copy");
    assert!(sidecars(&config.dir).is_empty());
}

/// the image's path is derived from the cache's own file name, whatever a sidecar says.
#[tokio::test(flavor = "multi_thread")]
async fn a_sidecar_cant_point_the_cover_elsewhere() {
    let server = FakeServer::start(State { cover_png: Some(png(32, 32, [200, 60, 40])), ..state() }).await;
    let dir = tempfile::tempdir().unwrap();
    let config = ArtworkConfig { dir: dir.path().join("art"), max_bytes: u64::MAX };
    let cache = ArtworkCache::new(server.client(), config.clone()).unwrap();
    let cover = cache.cover("al-1", Some(300)).await.unwrap();
    let sidecar = sidecars(&config.dir).pop().unwrap();
    let mut json: Value = serde_json::from_str(&std::fs::read_to_string(&sidecar).unwrap()).unwrap();
    json["path"] = json!("/etc/hostname");
    std::fs::write(&sidecar, json.to_string()).unwrap();
    let again = cache.cover("al-1", Some(300)).await.unwrap();
    assert_eq!(again.path, cover.path);
    assert!(again.path.starts_with(&config.dir));
    assert_eq!(server.calls("getCoverArt"), 1);
}


#[tokio::test(flavor = "multi_thread")]
async fn local_stars_update_and_clear() {
    let server = FakeServer::start(state()).await;
    let (_dir, library) = open_library();
    let sync = syncer(&server, &library);
    sync.full_sync(&|_| {}).await.unwrap();
    library.set_starred(StarTable::Song, "s1", true).unwrap();
    assert!(library.song("s1").unwrap().unwrap().starred.is_some());
    assert_eq!(library.starred_song_rowids().unwrap().len(), 1);
    library.set_starred(StarTable::Song, "s1", false).unwrap();
    assert!(library.song("s1").unwrap().unwrap().starred.is_none());
    assert!(library.starred_song_rowids().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn smart_playlist_rules_come_from_the_native_api() {
    let server = FakeServer::start(state()).await;
    Mock::given(method("POST")).and(path("/auth/login")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"token": "abc", "id": "u"}))).mount(&server.server).await;
    Mock::given(method("GET")).and(path("/api/playlist/p2")).and(header("x-nd-authorization", "Bearer abc")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "p2", "rules": {"all": [{"is": {"loved": true}}]}}))).mount(&server.server).await;
    Mock::given(method("GET")).and(path("/api/playlist/p1")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "p1", "rules": null}))).mount(&server.server).await;
    let client = server.client();
    assert_eq!(client.playlist_rules("p2").await.unwrap().as_deref(), Some(r#"{"all":[{"is":{"loved":true}}]}"#));
    assert_eq!(client.playlist_rules("p1").await.unwrap(), None);
}

#[tokio::test(flavor = "multi_thread")]
async fn album_info_notes_are_read_and_absent_info_is_empty() {
    let server = FakeServer::start(state()).await;
    set(&server, "getAlbumInfo2", json!({"albumInfo": {"notes": "Liner notes."}}));
    let client = server.client();
    assert_eq!(client.album_info2("a1").await.unwrap().notes.as_deref(), Some("Liner notes."));
    set(&server, "getAlbumInfo2", json!({}));
    assert!(client.album_info2("a1").await.unwrap().notes.is_none());
}

#[tokio::test]
async fn create_playlist_is_refused_without_writes() {
    let server = FakeServer::start(state()).await;
    let library = open_library().1;
    let syncer = Syncer::new(server.read_only_client(), library, SyncOptions::default());
    assert!(matches!(syncer.create_playlist("New").await, Err(navia_core::Error::WritesDisabled { .. })));
    assert_eq!(server.calls("createPlaylist"), 0);
}

/// Navidrome's native song list for album `al-Alb`, as `/api/song?album_id=` answers it: out of
/// track order, one song untagged, one with two values.
async fn mount_album_songs(server: &FakeServer, status: u16) {
    Mock::given(method("POST")).and(path("/auth/login")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"token": "abc", "id": "u"}))).mount(&server.server).await;
    let rows = json!([
        {"id": "s3", "discNumber": 2, "trackNumber": 1, "tags": {"copyright": ["© 2001 Late Disc"]}},
        {"id": "s2", "discNumber": 1, "trackNumber": 2, "tags": {"genre": ["Rock"]}},
        {"id": "s1", "discNumber": 1, "trackNumber": 1, "tags": {"copyright": ["(P) 2001 Label", "© 2001 Label"]}},
        {"id": "s4", "discNumber": 1, "trackNumber": 3, "tags": {"copyright": ["(P) 2001 Label"]}}
    ]);
    Mock::given(method("GET"))
        .and(path("/api/song"))
        .and(wiremock::matchers::query_param("album_id", "al-Alb"))
        .and(header("x-nd-authorization", "Bearer abc"))
        .respond_with(ResponseTemplate::new(status).set_body_json(rows).insert_header("x-total-count", "4"))
        .mount(&server.server)
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn album_copyrights_come_from_the_native_songs_in_track_order() {
    use navia_core::library::copyright::{AlbumFooterSource, album_footer};
    let mut st = state();
    st.album_extra.insert("al-Alb".into(), json!({"recordLabels": [{"name": "Fallback Records"}]}));
    let server = FakeServer::start(st).await;
    mount_album_songs(&server, 200).await;
    let (_dir, library) = open_library();
    syncer(&server, &library).full_sync(&|_| {}).await.unwrap();
    let client = server.client();
    assert_eq!(client.album_copyrights("al-Alb").await.unwrap(), ["(P) 2001 Label", "© 2001 Label", "(P) 2001 Label", "© 2001 Late Disc"]);
    let footer = album_footer(&client, &library, "al-Alb", true).await.unwrap().unwrap();
    assert_eq!(footer.source, AlbumFooterSource::Copyright);
    assert_eq!(footer.lines, ["℗ 2001 Label", "© 2001 Label", "© 2001 Late Disc"]);
    let local = album_footer(&client, &library, "al-Alb", false).await.unwrap().unwrap();
    assert_eq!((local.source, local.lines), (AlbumFooterSource::RecordLabel, vec!["Fallback Records".to_string()]));
    assert!(client.album_copyrights("../x").await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_native_read_falls_back_to_the_labels_and_nothing_hides() {
    use navia_core::library::copyright::{AlbumFooterSource, album_footer};
    let mut st = state();
    st.album_extra.insert("al-Alb".into(), json!({"recordLabels": [{"name": "Fallback Records"}]}));
    let server = FakeServer::start(st).await;
    mount_album_songs(&server, 500).await;
    let (_dir, library) = open_library();
    syncer(&server, &library).full_sync(&|_| {}).await.unwrap();
    let client = server.client();
    let footer = album_footer(&client, &library, "al-Alb", true).await.unwrap().unwrap();
    assert_eq!((footer.source, footer.lines), (AlbumFooterSource::RecordLabel, vec!["Fallback Records".to_string()]));
    assert!(album_footer(&client, &library, "unknown", true).await.unwrap().is_none());
}
