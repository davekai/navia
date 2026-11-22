//! The OpenSubsonic extensions Navia uses, each against a fake server with and without the extension.

mod common;

use std::sync::Arc;

use serde_json::json;

use common::{FakeServer, State};
use navia_core::capabilities::ServerFeatures;
use navia_core::client::{Client, Extension};
use navia_core::library::{Library, LibraryConfig};
use navia_core::lyrics::lyrics_for;
use navia_core::queue_sync::{QueueState, QueueSync};
use navia_core::reporting::{LiveEvent, PlayReporter};
use navia_core::audio::player::PlayerStatus;

fn features(names: &[(&str, &[i32])]) -> ServerFeatures {
    ServerFeatures::from_extensions(&names.iter().map(|(n, v)| Extension { name: n.to_string(), versions: v.to_vec() }).collect::<Vec<_>>())
}

/// Every request to `endpoint`, as its query and form parameters together.
async fn requests(fake: &FakeServer, endpoint: &str) -> Vec<Vec<(String, String)>> {
    fake.server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.url.path().ends_with(&format!("/{endpoint}")))
        .map(|r| {
            let mut pairs: Vec<(String, String)> = r.url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
            pairs.extend(url::form_urlencoded::parse(&r.body).map(|(k, v)| (k.into_owned(), v.into_owned())));
            pairs
        })
        .collect()
}

fn get<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn all<'a>(pairs: &'a [(String, String)], key: &str) -> Vec<&'a str> {
    pairs.iter().filter(|(k, _)| k == key).map(|(_, v)| v.as_str()).collect()
}

// playbackReport

async fn play_through(reporter: &PlayReporter) {
    // Restored paused at launch, so nothing until it plays.
    reporter.live_event(LiveEvent::Status(PlayerStatus::Paused)).await;
    reporter.live_event(LiveEvent::Track(Some("s1".into()))).await;
    reporter.live_event(LiveEvent::Position(1000)).await;
    reporter.live_event(LiveEvent::Status(PlayerStatus::Playing)).await;
    reporter.live_event(LiveEvent::Position(1100)).await;
    // A seek.
    reporter.live_event(LiveEvent::Position(60_000)).await;
    reporter.live_event(LiveEvent::Status(PlayerStatus::Paused)).await;
    reporter.live_event(LiveEvent::Status(PlayerStatus::Playing)).await;
    // Gapless into the next song.
    reporter.live_event(LiveEvent::Track(Some("s2".into()))).await;
    reporter.live_event(LiveEvent::Ended).await;
}

#[tokio::test]
async fn playback_report_sends_states_alongside_scrobbles() {
    let fake = FakeServer::start(State { open_subsonic: true, extensions: vec![("playbackReport".into(), vec![1])], ..Default::default() }).await;
    let client = fake.client();
    let reporter = PlayReporter::new(Arc::new(client.clone()));
    reporter.set_live_reports(ServerFeatures::detect(&client).await.unwrap().playback_report);
    play_through(&reporter).await;

    let sent = requests(&fake, "reportPlayback").await;
    let states: Vec<(String, String, String)> = sent.iter().map(|p| (get(p, "mediaId").unwrap().into(), get(p, "state").unwrap().into(), get(p, "positionMs").unwrap().into())).collect();
    let want = [("s1", "starting", "1000"), ("s1", "playing", "1000"), ("s1", "playing", "60000"), ("s1", "paused", "60000"), ("s1", "playing", "60000"), ("s2", "starting", "0"), ("s2", "playing", "0"), ("s2", "stopped", "0")];
    assert_eq!(states, want.map(|(a, b, c)| (a.to_string(), b.to_string(), c.to_string())));
    for p in &sent {
        assert_eq!(get(p, "ignoreScrobble"), Some("true"), "plays are counted by scrobble, never twice");
        assert_eq!(get(p, "mediaType"), Some("song"));
    }

    // Private mode ends the server's entry once and then sends nothing.
    reporter.live_event(LiveEvent::Track(Some("s3".into()))).await;
    reporter.live_event(LiveEvent::Status(PlayerStatus::Playing)).await;
    reporter.set_private(true).await;
    reporter.live_event(LiveEvent::Status(PlayerStatus::Paused)).await;
    let sent = requests(&fake, "reportPlayback").await;
    let tail: Vec<&str> = sent[want.len()..].iter().map(|p| get(p, "state").unwrap()).collect();
    assert_eq!(tail, ["starting", "playing", "stopped"]);
}

#[tokio::test]
async fn playback_report_absent_or_read_only_sends_nothing() {
    let fake = FakeServer::start(State { open_subsonic: true, extensions: vec![("songLyrics".into(), vec![1])], ..Default::default() }).await;
    let client = fake.client();
    let reporter = PlayReporter::new(Arc::new(client.clone()));
    reporter.set_live_reports(ServerFeatures::detect(&client).await.unwrap().playback_report);
    play_through(&reporter).await;
    assert!(requests(&fake, "reportPlayback").await.is_empty());

    // With the extension but a client that can't write.
    let reporter = PlayReporter::new(Arc::new(fake.read_only_client()));
    reporter.set_live_reports(true);
    play_through(&reporter).await;
    assert!(requests(&fake, "reportPlayback").await.is_empty());
}

// indexBasedQueue

fn library() -> (tempfile::TempDir, Library) {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::open(&LibraryConfig { path: dir.path().join("library.sqlite") }).unwrap();
    (dir, lib)
}

fn queue(ids: &[&str], index: usize, position_ms: u64) -> QueueState {
    QueueState { song_ids: ids.iter().map(|s| s.to_string()).collect(), index, position_ms }
}

fn sync(client: &Client, lib: &Library, reporter: Arc<PlayReporter>, f: ServerFeatures) -> QueueSync {
    let s = QueueSync::new(client.clone(), lib.clone(), reporter, tokio::runtime::Handle::current());
    s.set_features(f);
    s
}

#[tokio::test]
async fn queue_saves_by_index_and_offers_another_devices_queue() {
    let fake = FakeServer::start(State { open_subsonic: true, ..Default::default() }).await;
    let (_dir, lib) = library();
    let client = fake.client();
    let reporter = Arc::new(PlayReporter::new(Arc::new(client.clone())));
    let qs = sync(&client, &lib, reporter.clone(), features(&[("indexBasedQueue", &[1]), ("formPost", &[1])]));

    // At launch nothing is saved until the server's queue has been looked at.
    qs.note(queue(&["a", "b", "a"], 2, 5000));
    qs.flush().await.unwrap();
    assert!(requests(&fake, "savePlayQueueByIndex").await.is_empty());
    assert!(qs.remote_offer().await.unwrap().is_none(), "the server has none");
    // The same song twice, where only an index can say which one is playing.
    qs.flush().await.unwrap();
    // Unchanged, so no second call. A small position move alone isn't one either.
    qs.note(queue(&["a", "b", "a"], 2, 5500));
    qs.flush().await.unwrap();
    let saved = requests(&fake, "savePlayQueueByIndex").await;
    assert_eq!(saved.len(), 1);
    assert_eq!(all(&saved[0], "id"), ["a", "b", "a"]);
    assert_eq!((get(&saved[0], "currentIndex"), get(&saved[0], "position")), (Some("2"), Some("5000")));
    assert!(requests(&fake, "savePlayQueue").await.is_empty());

    // Private mode stops saving.
    reporter.set_private(true).await;
    qs.note(queue(&["c"], 0, 0));
    qs.flush().await.unwrap();
    assert_eq!(requests(&fake, "savePlayQueueByIndex").await.len(), 1);

    // What this device saved itself is never offered back.
    let entries = |ids: &[&str]| ids.iter().map(|id| json!({"id": id, "title": id})).collect::<Vec<_>>();
    fake.state.lock().unwrap().canned.insert("getPlayQueueByIndex".into(), json!({"playQueueByIndex": {"currentIndex": 2, "position": 5000, "changed": "2099-01-01T00:00:00Z", "changedBy": "navia", "entry": entries(&["a", "b", "a"])}}));
    assert!(qs.remote_offer().await.unwrap().is_none());

    // Another client's newer queue is offered, once.
    fake.state.lock().unwrap().canned.insert("getPlayQueueByIndex".into(), json!({"playQueueByIndex": {"currentIndex": 1, "position": 42000, "changed": "2099-01-01T00:00:00Z", "changedBy": "Other Client", "entry": entries(&["x", "y", "z"])}}));
    let offer = qs.remote_offer().await.unwrap().expect("offered");
    assert_eq!((offer.changed_by.as_str(), offer.index, offer.position_ms, offer.songs.len()), ("Other Client", 1, 42000, 3));
    qs.answered(&offer);
    assert!(qs.remote_offer().await.unwrap().is_none());

    // An older queue than this device's own isn't.
    fake.state.lock().unwrap().canned.insert("getPlayQueueByIndex".into(), json!({"playQueueByIndex": {"currentIndex": 0, "changed": "2001-01-01T00:00:00Z", "changedBy": "Other Client", "entry": entries(&["q"])}}));
    assert!(qs.remote_offer().await.unwrap().is_none());
    // Nor an empty one with Navidrome's zero time.
    fake.state.lock().unwrap().canned.insert("getPlayQueueByIndex".into(), json!({"playQueueByIndex": {"changed": "0001-01-01T00:00:00Z", "changedBy": ""}}));
    assert!(qs.remote_offer().await.unwrap().is_none());
}

#[tokio::test]
async fn an_offered_queue_is_not_overwritten_until_answered() {
    let fake = FakeServer::start(State { open_subsonic: true, ..Default::default() }).await;
    let (_dir, lib) = library();
    let client = fake.client();
    let qs = sync(&client, &lib, Arc::new(PlayReporter::new(Arc::new(client.clone()))), features(&[("indexBasedQueue", &[1])]));
    fake.state.lock().unwrap().canned.insert("getPlayQueueByIndex".into(), json!({"playQueueByIndex": {"currentIndex": 0, "position": 9000, "changed": "2099-01-01T00:00:00Z", "changedBy": "Other Client", "entry": [{"id": "x", "title": "x"}]}}));
    // The restored local queue, then the offer showing: nothing goes to the server.
    qs.note(queue(&["a", "b"], 0, 1000));
    let offer = qs.remote_offer().await.unwrap().expect("offered");
    qs.note(queue(&["a", "b"], 0, 3000));
    qs.flush().await.unwrap();
    assert!(requests(&fake, "savePlayQueueByIndex").await.is_empty());
    // Continue loads that queue, which the server already has, so nothing is sent again.
    qs.answered(&offer);
    qs.note(queue(&["x"], 0, 9000));
    qs.flush().await.unwrap();
    assert!(requests(&fake, "savePlayQueueByIndex").await.is_empty());
    // The listener's next change is saved.
    qs.note(queue(&["x", "y"], 1, 0));
    qs.flush().await.unwrap();
    assert_eq!(all(&requests(&fake, "savePlayQueueByIndex").await[0], "id"), ["x", "y"]);
}

#[tokio::test]
async fn queue_without_index_extension_uses_save_play_queue() {
    let fake = FakeServer::start(State { open_subsonic: true, ..Default::default() }).await;
    let (_dir, lib) = library();
    let client = fake.client();
    let reporter = Arc::new(PlayReporter::new(Arc::new(client.clone())));
    let qs = sync(&client, &lib, reporter, features(&[]));
    assert!(qs.remote_offer().await.unwrap().is_none());
    qs.note(queue(&["a", "b"], 1, 1234));
    qs.flush().await.unwrap();
    let saved = requests(&fake, "savePlayQueue").await;
    assert_eq!(saved.len(), 1);
    assert_eq!((all(&saved[0], "id"), get(&saved[0], "current"), get(&saved[0], "position")), (vec!["a", "b"], Some("b"), Some("1234")));
    assert!(requests(&fake, "savePlayQueueByIndex").await.is_empty());

    // The offer reads getPlayQueue instead, and finds the current song by id.
    fake.state.lock().unwrap().canned.insert("getPlayQueue".into(), json!({"playQueue": {"current": "y", "position": 7, "changed": "2099-01-01T00:00:00Z", "changedBy": "other-client", "entry": [{"id": "x", "title": "x"}, {"id": "y", "title": "y"}]}}));
    let offer = qs.remote_offer().await.unwrap().expect("offered");
    assert_eq!((offer.index, offer.changed_by.as_str()), (1, "other-client"));
    assert_eq!(fake.calls("getPlayQueueByIndex"), 0);

    // A read-only client saves nothing.
    let qs = sync(&fake.read_only_client(), &lib, Arc::new(PlayReporter::new(Arc::new(fake.read_only_client()))), features(&[("indexBasedQueue", &[1])]));
    fake.state.lock().unwrap().canned.remove("getPlayQueue");
    assert!(qs.remote_offer().await.unwrap().is_none());
    qs.note(queue(&["z"], 0, 0));
    qs.flush().await.unwrap();
    assert!(requests(&fake, "savePlayQueueByIndex").await.is_empty());
}

// songLyrics v2

fn enhanced_lyrics() -> serde_json::Value {
    json!({"lyricsList": {"structuredLyrics": [
        {"kind": "translation", "lang": "eng", "synced": true, "line": [{"start": 1000, "value": "Translated"}]},
        {"kind": "main", "lang": "ko", "synced": true, "offset": 0,
         "line": [{"start": 1000, "end": 3000, "value": "눈을 뜬"}, {"start": 3000, "value": "no words here"}],
         "cueLine": [{"index": 0, "start": 1000, "end": 3000, "value": "눈을 뜬", "cue": [
            {"start": 1000, "end": 1500, "value": "눈", "byteStart": 0, "byteEnd": 2},
            {"start": 1500, "end": 2000, "value": "을", "byteStart": 3, "byteEnd": 5},
            {"start": 2000, "end": 2200, "value": " ", "byteStart": 6, "byteEnd": 6},
            {"start": 2200, "end": 3000, "value": "뜬", "byteStart": 7, "byteEnd": 9}]}]}
    ]}})
}

#[tokio::test]
async fn lyrics_v2_carry_word_timing_and_v1_does_not_ask() {
    let mut state = State { open_subsonic: true, ..Default::default() };
    state.canned.insert("getLyricsBySongId".into(), enhanced_lyrics());
    let fake = FakeServer::start(state).await;
    let client = fake.client();

    let v2 = features(&[("songLyrics", &[1, 2])]);
    let lyrics = lyrics_for(&client, None, "s1", None, Some(v2)).await.unwrap();
    assert!(lyrics.synced);
    assert_eq!(lyrics.lines[0].text, "눈을 뜬", "the main layer, never the translation");
    let words: Vec<(u32, u32, u32)> = lyrics.lines[0].words.iter().map(|w| (w.start, w.end, w.start_ms)).collect();
    assert_eq!(words, [(0, 3, 1000), (3, 6, 1500), (6, 7, 2000), (7, 10, 2200)]);
    assert_eq!(lyrics.lines[0].end_ms, Some(3000));
    // Halfway through the second syllable, so 1.5 of 4 characters.
    assert_eq!(lyrics.lines[0].sung_fraction(1750), Some(1.5 / 4.0));
    assert_eq!(lyrics.lines[0].sung_fraction(500), Some(0.0));
    assert_eq!(lyrics.lines[0].sung_fraction(3000), Some(1.0));
    assert!(lyrics.lines[1].words.is_empty() && lyrics.lines[1].sung_fraction(3500).is_none(), "estimated fill for lines without timing");
    let asked = requests(&fake, "getLyricsBySongId").await;
    assert_eq!(get(&asked[0], "enhanced"), Some("true"));

    // With version 1 only, there's no `enhanced` and nothing word-timed comes back.
    let v1 = features(&[("songLyrics", &[1])]);
    lyrics_for(&client, None, "s1", None, Some(v1)).await.unwrap();
    let asked = requests(&fake, "getLyricsBySongId").await;
    assert_eq!(get(&asked[1], "enhanced"), None);
}

// topSongsByArtistId

#[tokio::test]
async fn top_songs_by_artist_id_when_offered() {
    let fake = FakeServer::start(State { open_subsonic: true, ..Default::default() }).await;
    let client = fake.client();
    let songs = client.artist_top_songs("ar-1", "Same Name", features(&[("topSongsByArtistId", &[1])]).top_songs_by_id, 50).await.unwrap();
    assert_eq!(songs.len(), 1);
    client.artist_top_songs("ar-1", "Same Name", features(&[]).top_songs_by_id, 50).await.unwrap();
    let asked = requests(&fake, "getTopSongs").await;
    assert_eq!((get(&asked[0], "id"), get(&asked[0], "artist")), (Some("ar-1"), None));
    assert_eq!((get(&asked[1], "id"), get(&asked[1], "artist")), (None, Some("Same Name")));
}

// Real server, read-only.

/// Read-only calls against the real server. `NAVIA_PROBE_SONG` and `NAVIA_PROBE_ARTIST` name the song
/// and artist. Pick an artist whose external info was refreshed recently, so the server doesn't refresh it.
#[tokio::test]
#[ignore = "talks to the real server; run explicitly"]
async fn real_server_read_only() {
    let env: std::collections::HashMap<String, String> = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env.local"))
        .unwrap()
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().trim_matches('"').to_string()))
        .collect();
    let mut cfg = navia_core::client::ClientConfig::new(env["NAVIDROME_URL"].clone(), navia_core::client::Credentials::new(env["NAVIDROME_USER"].clone(), env["NAVIDROME_PASSWORD"].clone()));
    cfg.write_policy = navia_core::client::WritePolicy::ReadOnly;
    let client = Client::new(cfg).unwrap();
    let f = ServerFeatures::detect(&client).await.unwrap();
    println!("features: {f:?}");
    let song = std::env::var("NAVIA_PROBE_SONG").unwrap();
    let raw = client.lyrics_by_song_id(&song, true).await.unwrap();
    println!("v2 lists: {:?}", raw.iter().map(|l| (l.kind.clone(), l.synced, l.line.len(), l.cue_line.len(), l.line.iter().filter(|x| x.end.is_some()).count())).collect::<Vec<_>>());
    let lyrics = lyrics_for(&client, None, &song, None, Some(f)).await.unwrap();
    println!("lyrics: synced {} lines {} with words {}", lyrics.synced, lyrics.lines.len(), lyrics.lines.iter().filter(|l| !l.words.is_empty()).count());
    let artist = std::env::var("NAVIA_PROBE_ARTIST").unwrap();
    let top = client.artist_top_songs(&artist, "", f.top_songs_by_id, 50).await.unwrap();
    println!("top by id: {} songs, artists {:?}", top.len(), top.iter().map(|s| s.artist.clone().unwrap_or_default()).collect::<std::collections::BTreeSet<_>>());
    let q = client.play_queue_by_index().await.unwrap();
    println!("queue by index: {:?}", q.map(|q| (q.entry.len(), q.current_index, q.position, q.changed, q.changed_by)));
}

/// The one deliberate write round against the real server, run only with `NAVIA_ALLOW_REAL_WRITE=1`.
/// Saves a test queue, reads it back and restores the previous one, then sends one live report.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "writes the user's queue and now-playing on the real server; run only with consent"]
async fn real_server_one_write_round() {
    assert_eq!(std::env::var("NAVIA_ALLOW_REAL_WRITE").as_deref(), Ok("1"));
    let env: std::collections::HashMap<String, String> = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env.local"))
        .unwrap()
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().trim_matches('"').to_string()))
        .collect();
    let client = Client::new(navia_core::client::ClientConfig::new(env["NAVIDROME_URL"].clone(), navia_core::client::Credentials::new(env["NAVIDROME_USER"].clone(), env["NAVIDROME_PASSWORD"].clone()))).unwrap();
    let f = ServerFeatures::detect(&client).await.unwrap();
    assert!(f.index_queue && f.playback_report);
    let song = std::env::var("NAVIA_PROBE_SONG").unwrap();

    let before = client.play_queue_by_index().await.unwrap().expect("a saved queue to put back");
    let before_ids: Vec<String> = before.entry.iter().map(|s| s.id.clone()).collect();
    println!("before: {} songs, index {:?}, position {:?}, by {:?} at {:?}", before_ids.len(), before.current_index, before.position, before.changed_by, before.changed);

    let (_dir, lib) = library();
    let reporter = Arc::new(PlayReporter::new(Arc::new(client.clone())));
    let qs = sync(&client, &lib, reporter.clone(), f);
    // Saving starts once the server's queue has been looked at and set aside.
    if let Some(offer) = qs.remote_offer().await.unwrap() {
        qs.answered(&offer);
    }
    let test_ids = [song.as_str(), before_ids[0].as_str(), song.as_str()];
    qs.note(queue(&test_ids, 2, 1234));
    qs.flush().await.unwrap();
    let saved = client.play_queue_by_index().await.unwrap().unwrap();
    let saved_ids: Vec<&str> = saved.entry.iter().map(|s| s.id.as_str()).collect();
    println!("saved: {:?} index {:?} position {:?} by {:?}", saved_ids, saved.current_index, saved.position, saved.changed_by);
    assert_eq!((saved_ids, saved.current_index, saved.position), (test_ids.to_vec(), Some(2), Some(1234)));

    let now = client.play_queue_by_index().await.unwrap().unwrap();
    if now.changed == saved.changed {
        client.save_play_queue_by_index(&before_ids, before.current_index.unwrap_or(0) as usize, before.position.unwrap_or(0) as u64, f.form_post).await.unwrap();
    } else {
        println!("something else saved meanwhile; not restoring over it");
    }
    let after = client.play_queue_by_index().await.unwrap().unwrap();
    let after_ids: Vec<String> = after.entry.iter().map(|s| s.id.clone()).collect();
    println!("restored: {} songs, index {:?}, position {:?}, by {:?}", after_ids.len(), after.current_index, after.position, after.changed_by);
    assert_eq!((after_ids, after.current_index, after.position.unwrap_or(0)), (before_ids, before.current_index, before.position.unwrap_or(0)));

    reporter.set_live_reports(true);
    reporter.live_event(LiveEvent::Track(Some(song.clone()))).await;
    reporter.live_event(LiveEvent::Status(PlayerStatus::Playing)).await;
    let mine = |entries: Vec<serde_json::Value>| entries.into_iter().find(|e| e["id"] == song.as_str() && e["playerName"].as_str().is_some_and(navia_core::client::is_navia_client));
    let seen = mine(client.now_playing().await.unwrap());
    println!("now playing: {:?}", seen.as_ref().map(|e| (e["state"].clone(), e["positionMs"].clone(), e["playerName"].clone())));
    reporter.stop_live().await;
    let gone = mine(client.now_playing().await.unwrap());
    println!("after stopped: {:?}", gone.as_ref().map(|e| e["state"].clone()));
    assert!(seen.is_some_and(|e| e["state"] == "playing") && gone.is_none());
}

// Listen-along sharing.

/// "Share what I'm playing" changes only the client name of playback reports, adding a zero-width space.
/// Turning it on mid-song moves the session to the marked name.
#[tokio::test]
async fn sharing_marks_only_playback_reports() {
    use navia_core::client::CLIENT_NAME;
    use navia_core::listen_along::SHARE_MARK;
    use navia_core::reporting::TrackPlay;
    let fake = FakeServer::start(State { open_subsonic: true, extensions: vec![("playbackReport".into(), vec![1])], ..Default::default() }).await;
    let client = fake.client();
    let reporter = PlayReporter::new(Arc::new(client.clone()));
    reporter.set_live_reports(true);
    reporter.live_event(LiveEvent::Track(Some("s1".into()))).await;
    reporter.live_event(LiveEvent::Status(PlayerStatus::Playing)).await;
    reporter.set_sharing(true).await;
    reporter.track_started(TrackPlay { song_id: "s2".into(), duration: std::time::Duration::from_secs(200), started_at_ms: 1_700_000_000_000 }).await;
    reporter.advance(std::time::Duration::from_secs(150)).await;
    let marked = format!("{CLIENT_NAME}{SHARE_MARK}");
    let reports: Vec<(String, String)> = requests(&fake, "reportPlayback").await.iter().map(|p| (get(p, "state").unwrap().to_string(), get(p, "c").unwrap().to_string())).collect();
    // the marked session starts before the plain one stops, so a follower never sees it gone
    let want = [("starting", CLIENT_NAME), ("playing", CLIENT_NAME), ("starting", marked.as_str()), ("playing", marked.as_str()), ("stopped", CLIENT_NAME)];
    assert_eq!(reports, want.map(|(a, b)| (a.to_string(), b.to_string())));
    let scrobbles: Vec<(String, String)> = requests(&fake, "scrobble").await.iter().map(|p| (get(p, "submission").unwrap().to_string(), get(p, "c").unwrap().to_string())).collect();
    assert_eq!(scrobbles, vec![("false".to_string(), marked.clone()), ("true".to_string(), CLIENT_NAME.to_string())], "now-playing marked, the play itself not");
    assert!(client.sharing());
    client.set_sharing(false);
    assert_eq!(client.report_client_name(), CLIENT_NAME);
}
