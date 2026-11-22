//! For You's mixes, Essentials and the change check against a fake server: how many asks, in
//! what order, how many at once, and what's kept when a build is cut short.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use common::{FakeServer, FakeSong, State, song};
use navia_core::client::native::ScrobbleEntry;
use navia_core::library::essentials::{self, ESSENTIALS};
use navia_core::library::for_you::{self, ARTIST_MIXES};
use navia_core::library::sync::{Changes, SyncOptions, Syncer};
use navia_core::library::{Library, LibraryConfig};
use serde_json::{Value, json};
use wiremock::matchers::path_regex;
use wiremock::{Mock, Request, Respond, ResponseTemplate};

const DAY: i64 = 86_400;
const ARTISTS: [&str; 6] = ["A", "B", "C", "D", "E", "F"];
const SLOW: Duration = Duration::from_millis(300);

fn songs() -> Vec<FakeSong> {
    ARTISTS.iter().flat_map(|a| (1..=12).map(move |i| song(&format!("{}{i}", a.to_lowercase()), &format!("{a} song {i}"), a, &format!("{a} album {}", i % 3), "1"))).collect()
}

fn song_json(s: &FakeSong) -> Value {
    json!({"id": s.id, "title": s.title, "artist": s.artist, "artistId": s.artist_id, "album": s.album, "albumId": s.album_id, "duration": 200})
}

fn wrap(body: Value) -> ResponseTemplate {
    let mut r = json!({"status": "ok", "version": "1.16.1", "openSubsonic": true});
    r.as_object_mut().unwrap().extend(body.as_object().unwrap().clone());
    ResponseTemplate::new(200).set_body_json(json!({"subsonic-response": r})).set_delay(SLOW)
}

/// every library song for any seed, except C's, which the server knows nothing about. counts the
/// asks in flight at once.
struct Similar {
    songs: Vec<Value>,
    asked: Arc<std::sync::Mutex<Vec<String>>>,
}

impl Respond for Similar {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let id = request.url.query_pairs().find(|(k, _)| k == "id").map(|(_, v)| v.into_owned()).unwrap_or_default();
        self.asked.lock().unwrap().push(id.clone());
        let list: Vec<Value> = if id.starts_with('c') { Vec::new() } else { self.songs.clone() };
        wrap(json!({"similarSongs2": {"song": list}}))
    }
}

/// ten songs by the artist named, a slow answer each.
struct Top {
    songs: Vec<FakeSong>,
    asked: Arc<std::sync::Mutex<Vec<String>>>,
}

impl Respond for Top {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let name = request.url.query_pairs().find(|(k, _)| k == "artist").map(|(_, v)| v.into_owned()).unwrap_or_default();
        self.asked.lock().unwrap().push(name.clone());
        let list: Vec<Value> = self.songs.iter().filter(|s| s.artist == name).take(10).map(song_json).collect();
        wrap(json!({"topSongs": {"song": list}}))
    }
}

struct Setup {
    _dir: tempfile::TempDir,
    server: FakeServer,
    library: Library,
    similar: Arc<std::sync::Mutex<Vec<String>>>,
    top: Arc<std::sync::Mutex<Vec<String>>>,
}

async fn setup() -> Setup {
    let all = songs();
    let state = State { folders: vec![("1".into(), "Music".into())], open_subsonic: true, last_scan: Some("2026-01-01T00:00:00Z".into()), songs: all.clone(), ..Default::default() };
    let server = FakeServer::start(state).await;
    let (similar, top) = (Arc::default(), Arc::default());
    Mock::given(path_regex("getSimilarSongs2")).respond_with(Similar { songs: all.iter().map(song_json).collect(), asked: Arc::clone(&similar) }).with_priority(1).mount(&server.server).await;
    Mock::given(path_regex("getTopSongs")).respond_with(Top { songs: all.clone(), asked: Arc::clone(&top) }).with_priority(1).mount(&server.server).await;
    let dir = tempfile::tempdir().unwrap();
    let library = Library::open(&LibraryConfig { path: dir.path().join("l.sqlite") }).unwrap();
    Syncer::new(server.client(), library.clone(), SyncOptions::default()).full_sync(&|_| {}).await.unwrap();
    // A is played most, then B, and so on.
    let now = navia_core::library::taste::now_unix();
    let mut plays = Vec::new();
    for (rank, a) in ARTISTS.iter().enumerate() {
        for i in 1..=3 {
            for n in 0..(12 - 2 * rank as i64) {
                plays.push(ScrobbleEntry { id: plays.len() as i64, song_id: format!("{}{i}", a.to_lowercase()), played_at: now - DAY * (n + 1) });
            }
        }
    }
    library.replace_recent_plays(&plays).unwrap();
    Setup { _dir: dir, server, library, similar, top }
}

fn mix_artists(library: &Library, day: i64) -> Vec<String> {
    let mut names: Vec<String> = library.mixes(day).unwrap().into_iter().map(|m| m.name).collect();
    names.sort();
    names
}

#[tokio::test]
async fn mixes_are_made_three_at_once_kept_as_they_land_and_the_same_as_one_at_a_time() {
    let s = setup().await;
    let day = 20_000;
    let landed = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = Arc::clone(&landed);
    let library = s.library.clone();
    let started = Instant::now();
    for_you::refresh_for_you_with(&s.server.client(), &s.library, day, &move || seen.lock().unwrap().push(library.mixes(day).unwrap().len())).await.unwrap();
    let took = started.elapsed();
    // C has no similar songs, so the four are the first four that work out, in taste order.
    assert_eq!(mix_artists(&s.library, day), ["A", "B", "D", "E"]);
    assert!(s.library.mixes(day).unwrap().len() == ARTIST_MIXES);
    let counts = landed.lock().unwrap().clone();
    assert_eq!(counts.iter().filter(|n| **n > 0).take(4).copied().collect::<Vec<_>>(), [1, 2, 3, 4], "each mix shows as it lands: {counts:?}");
    // one at a time would be five slow rounds (A, B, C, D, E); three at once is two.
    assert!(took < SLOW * 4, "took {took:?}");
    let page = s.library.for_you(day).unwrap();
    assert!(!page.needs_refresh && !page.building);
    // done for the day: nothing more is asked.
    let asked = s.similar.lock().unwrap().len();
    for_you::refresh_for_you(&s.server.client(), &s.library, day).await.unwrap();
    assert_eq!(s.similar.lock().unwrap().len(), asked);
}

#[tokio::test]
async fn a_build_cut_short_keeps_its_mixes_and_picks_up_where_it_stopped() {
    let s = setup().await;
    let day = 20_001;
    // the first round (A, B, C) lands; the second is cut off.
    let cut = tokio::time::timeout(SLOW + SLOW / 2, for_you::refresh_for_you(&s.server.client(), &s.library, day)).await;
    assert!(cut.is_err());
    assert_eq!(mix_artists(&s.library, day), ["A", "B"], "kept as they landed");
    assert!(s.library.for_you(day).unwrap().needs_refresh);
    let asked_a = |s: &Setup| s.similar.lock().unwrap().iter().filter(|id| id.starts_with('a')).count();
    let a_before = asked_a(&s);
    for_you::refresh_for_you(&s.server.client(), &s.library, day).await.unwrap();
    assert_eq!(mix_artists(&s.library, day), ["A", "B", "D", "E"]);
    assert_eq!(asked_a(&s), a_before, "A's mix isn't made again");
}

#[tokio::test]
async fn a_second_build_waits_for_the_first_instead_of_asking_again() {
    let s = setup().await;
    let day = 20_002;
    let client = s.server.client();
    let (a, b) = tokio::join!(for_you::refresh_for_you(&client, &s.library, day), async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(s.library.for_you_building());
        for_you::refresh_for_you(&client, &s.library, day).await
    });
    a.unwrap();
    b.unwrap();
    let alone = setup().await;
    for_you::refresh_for_you(&alone.server.client(), &alone.library, day).await.unwrap();
    assert_eq!(s.similar.lock().unwrap().len(), alone.similar.lock().unwrap().len());
}

#[tokio::test]
async fn keep_mixes_daily_makes_them_without_a_visit() {
    let s = setup().await;
    let day = navia_core::library::taste::local_day(navia_core::library::taste::now_unix());
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let task = tokio::spawn(for_you::keep_mixes_daily(s.server.client(), s.library.clone(), Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    })));
    let deadline = Instant::now() + Duration::from_secs(10);
    while s.library.for_you(day).unwrap().needs_refresh && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    task.abort();
    assert_eq!(s.library.mixes(day).unwrap().len(), ARTIST_MIXES);
    assert!(calls.load(Ordering::SeqCst) >= ARTIST_MIXES);
}

#[tokio::test]
async fn essentials_ask_a_few_at_once_and_keep_answers_for_the_day() {
    let s = setup().await;
    let day = 20_003;
    let client = s.server.client();
    assert!(s.library.essentials(day).unwrap().is_empty(), "nothing stored yet");
    let started = Instant::now();
    let found = essentials::refresh_essentials(&client, &s.library, false, day).await.unwrap();
    let took = started.elapsed();
    let ids: Vec<&str> = found.iter().map(|e| e.artist_id.as_str()).collect();
    assert_eq!(ids, ["ar-A", "ar-B", "ar-C", "ar-D", "ar-E", "ar-F"]);
    assert!(found.len() <= ESSENTIALS && found.iter().all(|e| e.songs.len() == 10));
    // six artists, three at once: two slow rounds rather than six.
    assert!(took < SLOW * 4, "took {took:?}");
    assert_eq!(s.top.lock().unwrap().len(), 6);
    assert_eq!(s.library.essentials(day).unwrap(), found, "the same from what's stored");
    essentials::refresh_essentials(&client, &s.library, false, day).await.unwrap();
    assert_eq!(s.top.lock().unwrap().len(), 6, "kept for the day");
    essentials::refresh_essentials(&client, &s.library, false, day + 1).await.unwrap();
    assert_eq!(s.top.lock().unwrap().len(), 12, "asked again the next day");
}

#[tokio::test]
async fn an_artist_page_asks_again_after_an_empty_answer_and_keeps_a_full_one() {
    let s = setup().await;
    let day = 20_003;
    let client = s.server.client();
    s.library.store_top_songs("ar-A", day, &[]).unwrap();
    let ids = essentials::top_songs(&client, &s.library, "ar-A", "A", false, day).await.unwrap();
    assert_eq!(ids.len(), 10, "an empty answer kept earlier is asked again");
    essentials::top_songs(&client, &s.library, "ar-A", "A", false, day).await.unwrap();
    assert_eq!(s.top.lock().unwrap().len(), 1, "a full answer is kept for the day");
}

#[tokio::test]
async fn a_change_check_with_nothing_changed_asks_little_and_finds_nothing() {
    let s = setup().await;
    {
        let mut st = s.server.state.lock().unwrap();
        st.canned.insert("getStarred2".into(), json!({"starred2": {}}));
        st.canned.insert("getAlbumList2".into(), json!({"albumList2": {"album": []}}));
        st.canned.insert("getPlaylists".into(), json!({"playlists": {"playlist": []}}));
    }
    let syncer = Syncer::new(s.server.client(), s.library.clone(), SyncOptions::default());
    syncer.check_for_changes(&|_| {}).await.unwrap();
    s.server.state.lock().unwrap().calls.clear();
    assert_eq!(syncer.check_for_changes(&|_| {}).await.unwrap(), Changes::default());
    let mut calls = s.server.state.lock().unwrap().calls.clone();
    calls.sort();
    assert_eq!(calls, ["getAlbumList2", "getPlaylists", "getScanStatus", "getStarred2"]);

    // a star and a new scan on the server.
    {
        let mut st = s.server.state.lock().unwrap();
        let mut starred = song_json(&st.songs[0]);
        starred["starred"] = json!("2026-02-01T00:00:00Z");
        st.canned.insert("getStarred2".into(), json!({"starred2": {"song": [starred]}}));
        st.last_scan = Some("2026-02-01T00:00:00Z".into());
    }
    let changes = syncer.check_for_changes(&|_| {}).await.unwrap();
    assert!(changes.library && changes.stars && !changes.playlists, "{changes:?}");
    assert!(changes.any());
}
