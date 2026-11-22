//! Library sync against the local fake server only.

mod common;

use std::sync::atomic::Ordering;
use std::time::Duration;

use common::{FakeServer, State, song};
use navia_core::Error;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use navia_core::library::sync::{RefreshOutcome, SyncMeter, SyncOptions, SyncOutcome, SyncPass, SyncStage, SyncStatus, SyncWatch, Syncer};
use navia_core::library::{Library, LibraryConfig};

fn state(songs: Vec<common::FakeSong>, folders: &[(&str, &str)]) -> State {
    State {
        folders: folders.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect(),
        songs,
        last_scan: Some("2026-01-01T00:00:00Z".into()),
        open_subsonic: true,
        ..Default::default()
    }
}

fn many(n: usize, library: &str, prefix: &str) -> Vec<common::FakeSong> {
    (0..n)
        .map(|i| song(&format!("{prefix}{i}"), &format!("Song {i}"), &format!("Artist {}", i % 7), &format!("Album {}", i / 10), library))
        .collect()
}

struct Fixture {
    _dir: tempfile::TempDir,
    library: Library,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let library = Library::open(&LibraryConfig { path: dir.path().join("library.sqlite") }).unwrap();
    Fixture { _dir: dir, library }
}

fn syncer(server: &FakeServer, library: &Library) -> Syncer {
    Syncer::new(server.read_only_client(), library.clone(), SyncOptions { page_size: 25, page_delay: Duration::ZERO, ..Default::default() })
}

#[tokio::test(flavor = "multi_thread")]
async fn full_sync_covers_every_library_and_page() {
    let mut songs = many(120, "1", "a");
    songs.extend(many(30, "2", "b"));
    let server = FakeServer::start(state(songs, &[("1", "Main"), ("2", "Other")])).await;
    let fx = fixture();
    let outcome = syncer(&server, &fx.library).full_sync(&|_| {}).await.unwrap();
    assert_eq!(outcome.status, SyncStatus::Complete);
    assert_eq!(outcome.counts.songs, 150);
    // "song 5" matches the title Song 5, plus songs by Artist 5 (5, 12, 19 and 26).
    assert_eq!(fx.library.search("song 5", 10, Some(&["2".to_string()])).unwrap().songs.len(), 4);
    assert!(fx.library.search("song", 500, Some(&["1".to_string()])).unwrap().songs.iter().all(|s| s.id.starts_with('a')));
    assert_eq!(fx.library.libraries().unwrap().len(), 2);
}

/// The progress a bar shows, with stages in order and counts across both libraries.
/// The song total comes from `getScanStatus`, and artists and albums have none.
#[tokio::test(flavor = "multi_thread")]
async fn full_sync_reports_real_progress_across_libraries() {
    let mut songs = many(120, "1", "a");
    songs.extend(many(30, "2", "b"));
    let server = FakeServer::start(state(songs, &[("1", "Main"), ("2", "Other")])).await;
    let fx = fixture();
    let seen = std::sync::Mutex::new(Vec::<SyncMeter>::new());
    let sync = syncer(&server, &fx.library);
    sync.full_sync(&|p| seen.lock().unwrap().push(p.meter)).await.unwrap();
    let seen = seen.into_inner().unwrap();
    let songs: Vec<&SyncMeter> = seen.iter().filter(|m| m.stage == SyncStage::Songs).collect();
    assert!(songs.iter().all(|m| m.total == Some(150)), "{songs:?}");
    assert!(songs.windows(2).all(|w| w[0].done <= w[1].done), "songs never go backwards: {songs:?}");
    assert_eq!(songs.last().unwrap().done, 150);
    assert_eq!(songs.last().unwrap().fraction(), Some(1.0));
    assert!(seen.iter().filter(|m| m.stage != SyncStage::Songs).all(|m| m.total.is_none()));
    // 7 artists and 12 + 3 albums over the two libraries.
    assert_eq!(seen.iter().filter(|m| m.stage == SyncStage::Albums).map(|m| m.done).max(), Some(15));

    let lists = std::sync::Mutex::new(Vec::<SyncMeter>::new());
    sync.sync_playlists_with(&|p| lists.lock().unwrap().push(p.meter)).await.unwrap();
    assert!(lists.into_inner().unwrap().iter().all(|m| m.stage == SyncStage::Playlists));
}

/// Answers Navidrome's native login and the `/api/{artist,album,song}` counts the sync bar asks for.
/// Counts the logins it saw.
async fn mount_native_totals(server: &FakeServer, per_library: &[(&str, [u32; 3])]) -> Arc<AtomicUsize> {
    use wiremock::matchers::{method, path, path_regex};
    use wiremock::{Mock, Request, ResponseTemplate};
    let logins = Arc::new(AtomicUsize::new(0));
    let seen = logins.clone();
    Mock::given(method("POST")).and(path("/auth/login")).respond_with(move |_: &Request| {
        seen.fetch_add(1, Ordering::Relaxed);
        ResponseTemplate::new(200).set_body_json(serde_json::json!({"token": "jwt"}))
    }).mount(&server.server).await;
    let libs: Vec<(String, [u32; 3])> = per_library.iter().map(|(id, c)| (id.to_string(), *c)).collect();
    Mock::given(method("GET")).and(path_regex(r"^/api/(artist|album|song)$")).respond_with(move |r: &Request| {
        if r.headers.get("x-nd-authorization").and_then(|v| v.to_str().ok()) != Some("Bearer jwt") {
            return ResponseTemplate::new(401);
        }
        let q: std::collections::HashMap<String, String> = r.url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        assert_eq!((q.get("_start").map(String::as_str), q.get("_end").map(String::as_str), q.get("missing").map(String::as_str)), (Some("0"), Some("1"), Some("false")));
        let i = match r.url.path() { "/api/artist" => 0, "/api/album" => 1, _ => 2 };
        let total: u32 = libs.iter().filter(|(id, _)| q.get("library_id").is_none_or(|l| l == id)).map(|(_, c)| c[i]).sum();
        ResponseTemplate::new(200).insert_header("x-total-count", total.to_string().as_str()).set_body_json(serde_json::json!([{"id": "x"}]))
    }).mount(&server.server).await;
    logins
}

fn watched(server: &FakeServer, library: &Library, server_totals: bool) -> Syncer {
    let watch = Arc::new(SyncWatch::default());
    Syncer::new(server.read_only_client(), library.clone(), SyncOptions { page_size: 25, page_delay: Duration::ZERO, watch: Some(watch), server_totals, ..Default::default() })
}

/// With the native API's counts every stage has a total, and one bar runs across artists, albums and songs.
#[tokio::test(flavor = "multi_thread")]
async fn server_totals_make_every_stage_determinate() {
    let mut songs = many(120, "1", "a");
    songs.extend(many(30, "2", "b"));
    let server = FakeServer::start(state(songs, &[("1", "Main"), ("2", "Other")])).await;
    // 7 artists in each library, 12 and 3 albums, and 120 and 30 songs.
    let logins = mount_native_totals(&server, &[("1", [7, 12, 120]), ("2", [7, 3, 30])]).await;
    let fx = fixture();
    let seen = std::sync::Mutex::new(Vec::<SyncMeter>::new());
    watched(&server, &fx.library, true).full_sync(&|p| seen.lock().unwrap().push(p.meter)).await.unwrap();
    let seen = seen.into_inner().unwrap();
    let total = |stage| seen.iter().filter(|m| m.stage == stage).map(|m| m.total).collect::<std::collections::HashSet<_>>();
    assert_eq!(total(SyncStage::Artists), [Some(14)].into());
    assert_eq!(total(SyncStage::Albums), [Some(15)].into());
    assert_eq!(total(SyncStage::Songs), [Some(150)].into());
    assert!(seen.iter().all(|m| m.overall.is_some_and(|(_, t)| t == 179)), "{seen:?}");
    assert!(seen.windows(2).all(|w| w[0].overall.unwrap().0 <= w[1].overall.unwrap().0), "the bar never goes backwards: {seen:?}");
    assert_eq!(seen.last().unwrap().overall, Some((179, 179)));
    assert_eq!(seen.first().unwrap().fraction(), Some(0.0));
    assert_eq!(logins.load(Ordering::Relaxed), 1, "one login, the token is kept");
}

/// Without the native API, songs count against the scan total and the rest count up.
#[tokio::test(flavor = "multi_thread")]
async fn without_server_totals_the_bar_falls_back() {
    let server = FakeServer::start(state(many(60, "1", "a"), &[("1", "Main")])).await;
    let fx = fixture();
    let seen = std::sync::Mutex::new(Vec::<SyncMeter>::new());
    let outcome = watched(&server, &fx.library, true).full_sync(&|p| seen.lock().unwrap().push(p.meter)).await.unwrap();
    assert_eq!(outcome.status, SyncStatus::Complete);
    let seen = seen.into_inner().unwrap();
    assert!(seen.iter().all(|m| m.overall.is_none()));
    assert!(seen.iter().filter(|m| m.stage != SyncStage::Songs).all(|m| m.total.is_none() && m.fraction().is_none()));
    assert!(seen.iter().filter(|m| m.stage == SyncStage::Songs).all(|m| m.total == Some(60)));

    // When off, as in test runs, no native request is made even where the server has the API.
    let server = FakeServer::start(state(many(60, "1", "a"), &[("1", "Main")])).await;
    let logins = mount_native_totals(&server, &[("1", [7, 6, 60])]).await;
    let fx = fixture();
    let seen = std::sync::Mutex::new(Vec::<SyncMeter>::new());
    watched(&server, &fx.library, false).full_sync(&|p| seen.lock().unwrap().push(p.meter)).await.unwrap();
    assert!(seen.into_inner().unwrap().iter().all(|m| m.overall.is_none()));
    assert_eq!(logins.load(Ordering::Relaxed), 0);
    assert!(server.server.received_requests().await.unwrap().iter().all(|r| r.url.path().starts_with("/rest/")));
}

/// A resumed sync counts what the interrupted run already stored, so the bar picks up where it was.
#[tokio::test(flavor = "multi_thread")]
async fn a_resumed_sync_keeps_its_place_on_the_bar() {
    let mut st = state(many(100, "1", "a"), &[("1", "Main")]);
    // Artists and albums to their end (two calls each), one page of songs, then the server fails.
    st.fail_search_after = Some(5);
    let server = FakeServer::start(st).await;
    mount_native_totals(&server, &[("1", [7, 10, 100])]).await;
    let fx = fixture();
    let sync = watched(&server, &fx.library, true);
    assert!(sync.full_sync(&|_| {}).await.is_err());
    server.state.lock().unwrap().fail_search_after = None;
    let seen = std::sync::Mutex::new(Vec::<SyncMeter>::new());
    sync.full_sync(&|p| seen.lock().unwrap().push(p.meter)).await.unwrap();
    let seen = seen.into_inner().unwrap();
    let first = seen.first().unwrap();
    assert_eq!(first.stage, SyncStage::Songs);
    assert_eq!(first.overall, Some((7 + 10 + first.done, 117)), "{seen:?}");
    assert!(first.done > 0);
    assert_eq!(seen.last().unwrap().overall, Some((117, 117)));
}

/// The watch the apps read. A first sync shows from the start, while a later refresh pass
/// isn't first, and nothing shows once it ends.
#[tokio::test(flavor = "multi_thread")]
async fn the_watch_follows_a_first_sync_through_playlists() {
    let server = FakeServer::start(state(many(60, "1", "a"), &[("1", "Main")])).await;
    let fx = fixture();
    let heard = Arc::new(AtomicUsize::new(0));
    let watch = Arc::new(SyncWatch::new({ let heard = heard.clone(); move || { heard.fetch_add(1, Ordering::Relaxed); } }));
    let sync = Syncer::new(server.read_only_client(), fx.library.clone(), SyncOptions { page_size: 25, page_delay: Duration::ZERO, watch: Some(watch.clone()), ..Default::default() });
    let firsts = std::sync::Mutex::new(Vec::new());
    sync.full_sync(&|_| firsts.lock().unwrap().push(watch.current().map(|s| s.first))).await.unwrap();
    assert!(firsts.into_inner().unwrap().iter().all(|f| *f == Some(true)));
    let after = watch.current().expect("still showing until the playlists are in");
    assert_eq!(after.meter.map(|m| m.stage), Some(SyncStage::Playlists));
    sync.sync_playlists().await.unwrap();
    assert!(watch.current().is_none());
    assert!(heard.load(Ordering::Relaxed) >= 4);

    // A second pass isn't first, so `visible` waits for it to run long enough.
    let later = std::sync::Mutex::new(Vec::new());
    sync.full_sync(&|_| later.lock().unwrap().push((watch.current().map(|s| s.first), watch.visible(Duration::from_secs(10)).is_some()))).await.unwrap();
    assert!(later.into_inner().unwrap().iter().all(|(f, v)| *f == Some(false) && !v));
    assert!(watch.current().is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn sync_survives_a_silent_server_page_cap() {
    let mut st = state(many(103, "1", "a"), &[("1", "Main")]);
    st.page_cap = Some(10);
    let server = FakeServer::start(st).await;
    let fx = fixture();
    let outcome = syncer(&server, &fx.library).full_sync(&|_| {}).await.unwrap();
    assert_eq!(outcome.counts.songs, 103);
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupted_sync_resumes_without_refetching_and_removes_nothing_early() {
    let mut st = state(many(100, "1", "a"), &[("1", "Main")]);
    st.fail_search_after = Some(5);
    let server = FakeServer::start(st).await;
    let fx = fixture();
    let s = syncer(&server, &fx.library);

    let err = s.full_sync(&|_| {}).await.unwrap_err();
    assert!(matches!(err, Error::HttpStatus { status: 500, .. }));
    let partial = fx.library.counts().unwrap();
    assert!(partial.songs < 100, "got {partial:?}");
    let calls_before_resume = server.calls("search3");

    {
        let mut st = server.state.lock().unwrap();
        st.fail_search_after = None;
    }
    let outcome = s.full_sync(&|_| {}).await.unwrap();
    assert_eq!(outcome.counts.songs, 100);
    assert_eq!(outcome.status, SyncStatus::Complete);
    // A fresh run would page artists and albums again from the start, while a resume continues.
    let after = server.calls("search3") - calls_before_resume;
    assert!(after < 12, "resume fetched {after} pages");
}

#[tokio::test(flavor = "multi_thread")]
async fn cancellation_stops_between_pages_and_resumes() {
    let server = FakeServer::start(state(many(200, "1", "a"), &[("1", "Main")])).await;
    let fx = fixture();
    let s = syncer(&server, &fx.library);
    let flag = s.cancel_flag();
    let outcome = s.full_sync(&|p| if p.offset >= 25 { flag.store(true, Ordering::Release) }).await.unwrap();
    assert_eq!(outcome.status, SyncStatus::Cancelled);
    assert!(outcome.counts.songs < 200);
    s.cancel_flag().store(false, Ordering::Release);
    assert_eq!(s.full_sync(&|_| {}).await.unwrap().status, SyncStatus::Complete);
    assert_eq!(fx.library.counts().unwrap().songs, 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn refresh_handles_additions_and_removals() {
    let server = FakeServer::start(state(many(60, "1", "a"), &[("1", "Main")])).await;
    let fx = fixture();
    let s = syncer(&server, &fx.library);
    s.full_sync(&|_| {}).await.unwrap();

    {
        let mut st = server.state.lock().unwrap();
        st.songs.retain(|x| x.id != "a0" && x.id != "a1");
        st.songs.push(song("new1", "Brand New", "Newcomer", "Fresh", "1"));
        st.last_scan = Some("2026-02-02T00:00:00Z".into());
    }
    match s.refresh(false, &|_| {}).await.unwrap() {
        RefreshOutcome::Synced(o) => {
            assert_eq!(o.pass, SyncPass::Changes);
            assert_eq!(o.counts.songs, 59);
            assert_eq!(o.libraries[0].removed.songs, 2);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(fx.library.search("brand", 5, None).unwrap().songs[0].id, "new1");
    assert!(fx.library.song("a0").unwrap().is_none());
    assert!(fx.library.search("Song 0", 5, None).unwrap().songs.iter().all(|s| s.id != "a0"));
    // Nothing changed since, so a second refresh costs only one scan-status call.
    let before = server.calls("search3");
    assert!(matches!(s.refresh(false, &|_| {}).await.unwrap(), RefreshOutcome::Unchanged));
    assert_eq!(server.calls("search3"), before);
}

fn new_scan(server: &FakeServer, marker: &str) {
    server.state.lock().unwrap().last_scan = Some(marker.into());
}

async fn refreshed(s: &Syncer) -> SyncOutcome {
    match s.refresh(false, &|_| {}).await.unwrap() {
        RefreshOutcome::Synced(o) => o,
        other => panic!("{other:?}"),
    }
}

fn album_calls(server: &FakeServer) -> Vec<String> {
    let mut calls = server.state.lock().unwrap().album_calls.clone();
    calls.sort();
    calls
}

/// after a scan only the album list is read, then `getAlbum` for the albums that are new or whose
/// entry moved; no song pages. songs dropped from a changed album and albums that left the list go.
#[tokio::test(flavor = "multi_thread")]
async fn refresh_after_a_scan_fetches_only_the_albums_that_changed() {
    let server = FakeServer::start(state(many(60, "1", "a"), &[("1", "Main")])).await;
    let fx = fixture();
    let s = syncer(&server, &fx.library);
    s.full_sync(&|_| {}).await.unwrap();
    let song_pages = server.state.lock().unwrap().song_pages;
    {
        let mut st = server.state.lock().unwrap();
        st.songs.retain(|x| x.id != "a0" && x.album != "Album 5");
        let after_album_2 = st.songs.iter().rposition(|x| x.album == "Album 2").unwrap() + 1;
        st.songs.insert(after_album_2, song("a2x", "Extra Track", "Artist 2", "Album 2", "1"));
        st.songs.push(song("new1", "Brand New", "Newcomer", "Fresh", "1"));
    }
    new_scan(&server, "2026-02-02T00:00:00Z");

    let o = refreshed(&s).await;
    assert_eq!(o.pass, SyncPass::Changes);
    assert_eq!(album_calls(&server), ["al-Album 0", "al-Album 2", "al-Fresh"]);
    assert_eq!(server.state.lock().unwrap().song_pages, song_pages, "no song pages");
    assert_eq!(o.counts.songs, 51);
    assert_eq!((o.libraries[0].removed.songs, o.libraries[0].removed.albums), (11, 1));
    assert!(fx.library.song("a0").unwrap().is_none());
    assert!(fx.library.album("al-Album 5").unwrap().is_none());
    assert_eq!(fx.library.album_songs("al-Album 2").unwrap().len(), 11);
    assert_eq!(fx.library.search("brand", 5, None).unwrap().songs[0].id, "new1");
    assert!(fx.library.artist("ar-Newcomer").unwrap().is_some());
    assert!(matches!(s.refresh(false, &|_| {}).await.unwrap(), RefreshOutcome::Unchanged));
}

/// a song moved to another album is re-pointed in place, so apps holding its row still find it.
#[tokio::test(flavor = "multi_thread")]
async fn a_song_that_moves_album_keeps_its_row() {
    let server = FakeServer::start(state(many(60, "1", "a"), &[("1", "Main")])).await;
    let fx = fixture();
    let s = syncer(&server, &fx.library);
    s.full_sync(&|_| {}).await.unwrap();
    let before = fx.library.song_rowids(&["a9".to_string()]).unwrap();
    {
        let mut st = server.state.lock().unwrap();
        st.songs[9].album = "Album 0b".into();
        st.songs[9].album_id = "al-Album 0b".into();
    }
    new_scan(&server, "2026-02-02T00:00:00Z");
    let o = refreshed(&s).await;
    assert_eq!(o.pass, SyncPass::Changes);
    assert_eq!(o.libraries[0].removed.songs, 0);
    assert_eq!(fx.library.song_rowids(&["a9".to_string()]).unwrap(), before);
    assert_eq!(fx.library.song("a9").unwrap().unwrap().album_id.as_deref(), Some("al-Album 0b"));
    assert_eq!(fx.library.album_songs("al-Album 0").unwrap().len(), 9);
}

/// plays and stars on an album entry aren't a catalog change: the row is updated, nothing is fetched.
#[tokio::test(flavor = "multi_thread")]
async fn plays_alone_dont_fetch_an_album() {
    let server = FakeServer::start(state(many(60, "1", "a"), &[("1", "Main")])).await;
    let fx = fixture();
    let s = syncer(&server, &fx.library);
    s.full_sync(&|_| {}).await.unwrap();
    server.state.lock().unwrap().album_extra.insert("al-Album 1".into(), serde_json::json!({"playCount": 3, "played": "2026-02-01T00:00:00Z"}));
    new_scan(&server, "2026-02-02T00:00:00Z");
    let o = refreshed(&s).await;
    assert_eq!(o.pass, SyncPass::Changes);
    assert!(album_calls(&server).is_empty());
    assert_eq!(fx.library.album("al-Album 1").unwrap().unwrap().play_count, Some(3));
}

/// what the album list can't show (a song retagged in place) waits for the next full sync.
#[tokio::test(flavor = "multi_thread")]
async fn a_song_retagged_in_place_is_caught_by_the_full_sync() {
    let server = FakeServer::start(state(many(60, "1", "a"), &[("1", "Main")])).await;
    let fx = fixture();
    let s = syncer(&server, &fx.library);
    s.full_sync(&|_| {}).await.unwrap();
    server.state.lock().unwrap().songs[3].title = "Renamed".into();
    new_scan(&server, "2026-02-02T00:00:00Z");
    assert_eq!(refreshed(&s).await.pass, SyncPass::Changes);
    assert_eq!(fx.library.song("a3").unwrap().unwrap().title, "Song 3");
    s.full_sync(&|_| {}).await.unwrap();
    assert_eq!(fx.library.song("a3").unwrap().unwrap().title, "Renamed");
}

/// most albums gone at once looks like a server problem: nothing is removed, and the next refresh
/// is a full sync, with its own two-pass confirmation.
#[tokio::test(flavor = "multi_thread")]
async fn a_mass_vanishing_holds_removal_and_the_next_refresh_is_full() {
    let server = FakeServer::start(state(many(600, "1", "a"), &[("1", "Main")])).await;
    let fx = fixture();
    let s = syncer(&server, &fx.library);
    s.full_sync(&|_| {}).await.unwrap();
    server.state.lock().unwrap().songs.truncate(50);
    new_scan(&server, "2026-02-02T00:00:00Z");
    let o = refreshed(&s).await;
    assert_eq!(o.pass, SyncPass::Changes);
    assert!(o.libraries[0].removal_held.is_some());
    assert_eq!(fx.library.counts().unwrap().songs, 600);
    assert_eq!(refreshed(&s).await.pass, SyncPass::Full);
}

/// a change pass stopped part-way keeps what it stored. a changed album's row is only written at the
/// end, so the next pass still sees it changed and fetches it again, and nothing is lost.
#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_change_pass_leaves_the_rest_for_the_next() {
    let server = FakeServer::start(state(many(60, "1", "a"), &[("1", "Main")])).await;
    let fx = fixture();
    let s = syncer(&server, &fx.library);
    s.full_sync(&|_| {}).await.unwrap();
    {
        let mut st = server.state.lock().unwrap();
        for k in (0..6).rev() {
            st.songs.insert((k + 1) * 10, song(&format!("x{k}"), "Added", &format!("Artist {k}"), &format!("Album {k}"), "1"));
        }
    }
    new_scan(&server, "2026-02-02T00:00:00Z");
    let flag = s.cancel_flag();
    let first = s.refresh(false, &|p| if p.phase == navia_core::library::sync::SyncPhase::Songs && p.offset > 0 { flag.store(true, Ordering::Release) }).await.unwrap();
    let RefreshOutcome::Synced(first) = first else { panic!("{first:?}") };
    assert_eq!(first.status, SyncStatus::Cancelled);
    let fetched = album_calls(&server).len();
    assert!(fetched < 6, "{fetched}");

    let o = refreshed(&s).await;
    assert_eq!((o.pass, o.status), (SyncPass::Changes, SyncStatus::Complete));
    assert_eq!(album_calls(&server).len(), fetched + 6);
    assert_eq!(o.counts.songs, 66);
    assert!(matches!(s.refresh(false, &|_| {}).await.unwrap(), RefreshOutcome::Unchanged));
}

/// songs as Navidrome's native `/api/song` lists them: (id, album id, library id, updatedAt).
type NativeSongs = Arc<std::sync::Mutex<Vec<(String, String, String, String)>>>;

/// answers the native login and `/api/song?library_id=&_sort=updated_at&_order=DESC` paging, from `songs`.
async fn mount_song_updates(server: &FakeServer, songs: NativeSongs) -> Arc<AtomicUsize> {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, Request, ResponseTemplate};
    let logins = Arc::new(AtomicUsize::new(0));
    let seen = logins.clone();
    Mock::given(method("POST")).and(path("/auth/login")).respond_with(move |_: &Request| {
        seen.fetch_add(1, Ordering::Relaxed);
        ResponseTemplate::new(200).set_body_json(serde_json::json!({"token": "jwt"}))
    }).mount(&server.server).await;
    Mock::given(method("GET")).and(path("/api/song")).respond_with(move |r: &Request| {
        if r.headers.get("x-nd-authorization").and_then(|v| v.to_str().ok()) != Some("Bearer jwt") {
            return ResponseTemplate::new(401);
        }
        let q: std::collections::HashMap<String, String> = r.url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        assert_eq!((q.get("_sort").map(String::as_str), q.get("_order").map(String::as_str)), (Some("updated_at"), Some("DESC")));
        let (start, end): (usize, usize) = (q["_start"].parse().unwrap(), q["_end"].parse().unwrap());
        let mut rows: Vec<(String, String, String, String)> = songs.lock().unwrap().iter().filter(|s| q.get("library_id").is_none_or(|l| *l == s.2)).cloned().collect();
        rows.sort_by(|a, b| b.3.cmp(&a.3));
        let page: Vec<serde_json::Value> = rows.into_iter().skip(start).take(end - start).map(|(id, album, library, at)| serde_json::json!({"id": id, "albumId": album, "libraryId": library.parse::<i64>().unwrap(), "updatedAt": at})).collect();
        ResponseTemplate::new(200).set_body_json(page)
    }).mount(&server.server).await;
    logins
}

fn with_song_updates(server: &FakeServer, library: &Library) -> Syncer {
    Syncer::new(server.read_only_client(), library.clone(), SyncOptions { page_size: 25, page_delay: Duration::ZERO, song_updates: true, ..Default::default() })
}

fn native_songs(songs: &[common::FakeSong], at: &str) -> NativeSongs {
    Arc::new(std::sync::Mutex::new(songs.iter().map(|s| (s.id.clone(), s.album_id.clone(), s.library.clone(), at.to_string())).collect()))
}

/// on Navidrome a song retagged in place moves its `updatedAt`, so a change pass refetches just
/// its album, with one login for the whole run.
#[tokio::test(flavor = "multi_thread")]
async fn a_song_retagged_in_place_is_picked_up_from_its_updated_time() {
    let songs = many(60, "1", "a");
    let native = native_songs(&songs, "2026-01-01T00:00:00Z");
    let server = FakeServer::start(state(songs, &[("1", "Main")])).await;
    let logins = mount_song_updates(&server, native.clone()).await;
    let fx = fixture();
    let s = with_song_updates(&server, &fx.library);
    s.full_sync(&|_| {}).await.unwrap();

    server.state.lock().unwrap().songs[13].title = "Renamed".into();
    native.lock().unwrap()[13].3 = "2026-02-02T10:00:00.5Z".into();
    new_scan(&server, "2026-02-02T00:00:00Z");
    let o = refreshed(&s).await;
    assert_eq!(o.pass, SyncPass::Changes);
    assert_eq!(album_calls(&server), ["al-Album 1"]);
    assert_eq!(fx.library.song("a13").unwrap().unwrap().title, "Renamed");
    assert_eq!(o.counts.songs, 60);

    // the mark moved: nothing updated since, so the next pass fetches no album.
    new_scan(&server, "2026-02-03T00:00:00Z");
    refreshed(&s).await;
    assert_eq!(album_calls(&server).len(), 1);
    assert_eq!(logins.load(Ordering::Relaxed), 1, "one login, the token is kept");
}

/// a song the scan marked missing moves its `updatedAt` too; its album no longer lists it, so it goes.
#[tokio::test(flavor = "multi_thread")]
async fn a_song_marked_missing_is_dropped_from_its_album() {
    let songs = many(60, "1", "a");
    let native = native_songs(&songs, "2026-01-01T00:00:00Z");
    let server = FakeServer::start(state(songs, &[("1", "Main")])).await;
    mount_song_updates(&server, native.clone()).await;
    let fx = fixture();
    let s = with_song_updates(&server, &fx.library);
    s.full_sync(&|_| {}).await.unwrap();
    // replaced by another file under the same album entry: same count, one id gone, one new.
    {
        let mut st = server.state.lock().unwrap();
        st.songs[24] = song("a24-new", "Song 24", "Artist 3", "Album 2", "1");
    }
    {
        let mut n = native.lock().unwrap();
        n[24].3 = "2026-02-02T10:00:00Z".into();
        n.push(("a24-new".into(), "al-Album 2".into(), "1".into(), "2026-02-02T10:00:01Z".into()));
    }
    new_scan(&server, "2026-02-02T00:00:00Z");
    let o = refreshed(&s).await;
    assert_eq!(album_calls(&server), ["al-Album 2"]);
    assert!(fx.library.song("a24").unwrap().is_none());
    assert!(fx.library.song("a24-new").unwrap().is_some());
    assert_eq!(o.libraries[0].removed.songs, 1);
}

/// without the native API (another server, or the login refused) the change pass still runs, and
/// the full sync stays the only way to see a retag.
#[tokio::test(flavor = "multi_thread")]
async fn without_the_native_api_song_updates_fall_back_to_the_full_sync() {
    let server = FakeServer::start(state(many(60, "1", "a"), &[("1", "Main")])).await;
    let fx = fixture();
    let s = with_song_updates(&server, &fx.library);
    s.full_sync(&|_| {}).await.unwrap();
    server.state.lock().unwrap().songs[3].title = "Renamed".into();
    new_scan(&server, "2026-02-02T00:00:00Z");
    let o = refreshed(&s).await;
    assert_eq!((o.pass, o.status), (SyncPass::Changes, SyncStatus::Complete));
    assert!(album_calls(&server).is_empty());
    assert_eq!(fx.library.song("a3").unwrap().unwrap().title, "Song 3");
    s.full_sync(&|_| {}).await.unwrap();
    assert_eq!(fx.library.song("a3").unwrap().unwrap().title, "Renamed");
}

/// more updated songs than a pass follows: nothing is guessed, and the next refresh is a full sync.
#[tokio::test(flavor = "multi_thread")]
async fn too_many_updated_songs_make_the_next_refresh_full() {
    let songs = many(60, "1", "a");
    let native = native_songs(&songs, "2026-01-01T00:00:00Z");
    let server = FakeServer::start(state(songs, &[("1", "Main")])).await;
    mount_song_updates(&server, native.clone()).await;
    let fx = fixture();
    let s = with_song_updates(&server, &fx.library);
    s.full_sync(&|_| {}).await.unwrap();
    {
        let mut n = native.lock().unwrap();
        for i in 0..5001 {
            n.push((format!("ghost{i}"), "al-Album 0".into(), "1".into(), format!("2026-02-02T10:00:00.{i:04}Z")));
        }
    }
    new_scan(&server, "2026-02-02T00:00:00Z");
    assert_eq!(refreshed(&s).await.pass, SyncPass::Changes);
    assert!(album_calls(&server).is_empty());
    // no new scan needed: the scan the pass gave up on is still owed
    assert_eq!(refreshed(&s).await.pass, SyncPass::Full);
    assert!(matches!(s.refresh(false, &|_| {}).await.unwrap(), RefreshOutcome::Unchanged));
}

#[tokio::test(flavor = "multi_thread")]
async fn refresh_waits_while_the_server_scans() {
    let server = FakeServer::start(state(many(60, "1", "a"), &[("1", "Main")])).await;
    let fx = fixture();
    let s = syncer(&server, &fx.library);
    s.full_sync(&|_| {}).await.unwrap();
    server.state.lock().unwrap().scanning = true;
    assert!(matches!(s.refresh(false, &|_| {}).await.unwrap(), RefreshOutcome::ServerScanning));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_scan_during_sync_holds_removal_until_the_next_sync() {
    let server = FakeServer::start(state(many(60, "1", "a"), &[("1", "Main")])).await;
    let fx = fixture();
    let s = syncer(&server, &fx.library);
    s.full_sync(&|_| {}).await.unwrap();

    // Remove a song while a scan finishes in the middle of the next sync.
    let flip = server.state.clone();
    let once = std::sync::atomic::AtomicBool::new(false);
    let outcome = s
        .full_sync(&|p| {
            if p.offset >= 25 && !once.swap(true, Ordering::AcqRel) {
                let mut st = flip.lock().unwrap();
                st.songs.retain(|x| x.id != "a59");
                st.last_scan = Some("2026-03-03T00:00:00Z".into());
            }
        })
        .await
        .unwrap();
    assert!(outcome.libraries[0].removal_held.is_some());
    assert_eq!(fx.library.counts().unwrap().songs, 60, "kept until confirmed");

    let outcome = s.full_sync(&|_| {}).await.unwrap();
    assert!(outcome.libraries[0].removal_held.is_none());
    assert_eq!(outcome.counts.songs, 59);
}

#[tokio::test(flavor = "multi_thread")]
async fn sudden_mass_shrink_needs_a_second_sync_to_confirm() {
    let server = FakeServer::start(state(many(200, "1", "a"), &[("1", "Main")])).await;
    let fx = fixture();
    let s = syncer(&server, &fx.library);
    s.full_sync(&|_| {}).await.unwrap();
    {
        let mut st = server.state.lock().unwrap();
        st.songs.truncate(40);
    }
    let outcome = s.full_sync(&|_| {}).await.unwrap();
    assert!(outcome.libraries[0].removal_held.is_some());
    assert_eq!(fx.library.counts().unwrap().songs, 200);
    let outcome = s.full_sync(&|_| {}).await.unwrap();
    assert!(outcome.libraries[0].removal_held.is_none());
    assert_eq!(outcome.counts.songs, 40);
}

#[tokio::test(flavor = "multi_thread")]
async fn losing_access_to_a_library_removes_its_rows_but_keeps_shared_artists() {
    let mut songs = many(30, "1", "a");
    songs.push(song("b0", "Only In Two", "Artist 0", "B", "2"));
    songs.push(song("b1", "Only In Two Also", "Solo Two", "C", "2"));
    let server = FakeServer::start(state(songs, &[("1", "Main"), ("2", "Other")])).await;
    let fx = fixture();
    let s = syncer(&server, &fx.library);
    s.full_sync(&|_| {}).await.unwrap();
    assert_eq!(fx.library.counts().unwrap().songs, 32);

    {
        let mut st = server.state.lock().unwrap();
        st.folders.retain(|(id, _)| id == "1");
        st.songs.retain(|x| x.library == "1");
        st.last_scan = Some("2026-04-04T00:00:00Z".into());
    }
    let outcome = s.full_sync(&|_| {}).await.unwrap();
    assert_eq!(outcome.counts.songs, 30);
    assert!(fx.library.artist("ar-Artist 0").unwrap().is_some(), "still in library 1");
    assert!(fx.library.artist("ar-Solo Two").unwrap().is_none(), "was only in library 2");
    assert!(fx.library.search("solo two", 5, None).unwrap().artists.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_with_no_visible_libraries_never_wipes_the_local_copy() {
    let server = FakeServer::start(state(many(20, "1", "a"), &[("1", "Main")])).await;
    let fx = fixture();
    let s = syncer(&server, &fx.library);
    s.full_sync(&|_| {}).await.unwrap();
    server.state.lock().unwrap().folders.clear();
    assert!(matches!(s.full_sync(&|_| {}).await.unwrap_err(), Error::Sync(_)));
    assert_eq!(fx.library.counts().unwrap().songs, 20);
}

#[tokio::test(flavor = "multi_thread")]
async fn session_connects_syncs_and_refreshes_in_background() {
    use navia_core::client::{Credentials, WritePolicy};
    use navia_core::session::{Session, SessionConfig};

    let server = FakeServer::start(state(many(40, "1", "a"), &[("1", "Main")])).await;
    let dir = tempfile::tempdir().unwrap();
    let session = Session::connect(SessionConfig {
        server_url: server.server.uri(),
        credentials: Credentials::new("tester", "letmein"),
        write_policy: WritePolicy::ReadOnly,
        library_path: Some(dir.path().join("s.sqlite")),
        artwork_dir: Some(dir.path().join("art")),
    })
    .await
    .unwrap();
    assert!(session.capabilities().libraries.len() == 1);
    assert_eq!(session.sync(&|_| {}).await.unwrap().counts.songs, 40);

    server.state.lock().unwrap().songs.push(song("x1", "Late Arrival", "Newcomer", "Later", "1"));
    server.state.lock().unwrap().last_scan = Some("2026-05-05T00:00:00Z".into());
    let mut bg = session.refresh_in_background(Duration::from_millis(50));
    let outcome = tokio::time::timeout(Duration::from_secs(5), bg.results.recv()).await.unwrap().unwrap().unwrap();
    assert!(matches!(outcome, RefreshOutcome::Synced(_)));
    assert_eq!(session.library().search("late arrival", 5, None).unwrap().songs.len(), 1);
    bg.stop().await;
}
