//! Replays real listening sessions through the Navia mode and reports how often the next song
//! was in its top 20. Server answers are cached in a JSON file so reruns ask nothing again.
//!
//! Plays and last-played dates are cleared from the answers so held-out sessions don't leak into
//! the ranking. With `deezer` last, it turns Deezer on in the database, so pass a copy.
//!
//! `NAVIDROME_URL=... NAVIDROME_USER=... NAVIDROME_PASSWORD=... cargo run -p navia-core --example
//! autoplay_replay -- <copy of library.sqlite> <answers.json> [sessions] [deezer]`

use std::collections::{HashMap, HashSet};

use navia_core::client::{Client, ClientConfig, Credentials, Song, WritePolicy};
use navia_core::engine::eval::{Moment, TOP, replay};
use navia_core::engine::playlog::{Ended, Origin, PlayEvent, SESSION_GAP_MS};
use navia_core::engine::{deezer_src, junk, plan, sequence};
use navia_core::library::{Library, LibraryConfig};

/// The most seed songs asked of the server in one run.
const MAX_ASKS: usize = 80;
/// The plan is deterministic, so one run is enough.
const RUNS: u64 = 1;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("usage: autoplay_replay <library.sqlite> <answers.json> [sessions]")?;
    let cache_path = std::env::args().nth(2).ok_or("usage: autoplay_replay <library.sqlite> <answers.json> [sessions]")?;
    let held: usize = std::env::args().nth(3).and_then(|n| n.parse().ok()).unwrap_or(6);
    let env = |k: &str| std::env::var(k).map_err(|_| format!("{k} is not set"));
    let mut cfg = ClientConfig::new(env("NAVIDROME_URL")?, Credentials::new(env("NAVIDROME_USER")?, env("NAVIDROME_PASSWORD")?));
    cfg.write_policy = WritePolicy::ReadOnly;
    let client = Client::new(cfg)?;
    let rt = tokio::runtime::Runtime::new()?;
    let db = Library::open(&LibraryConfig { path: path.clone().into() })?;
    let with_deezer = std::env::args().nth(4).as_deref() == Some("deezer");
    if with_deezer {
        navia_core::deezer::set_setting(&db, Some(true))?;
        let gate = navia_core::deezer::gate(&db);
        println!("Deezer gate: {gate:?}");
        if !gate.is_open() {
            return Err("the Deezer gate is closed".into());
        }
    }
    let dz = navia_core::deezer::Deezer::shared();
    let clean = |mut s: Song| {
        s.played = None;
        s.play_count = None;
        s
    };

    let plays: Vec<(String, i64)> = {
        let c = rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut st = c.prepare("SELECT r.song_id, r.played_at FROM recent_play r JOIN song s ON s.id = r.song_id ORDER BY r.played_at, r.id")?;
        st.query_map([], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? * 1000)))?.collect::<Result<_, _>>()?
    };
    let mut events: Vec<PlayEvent> = Vec::new();
    let (mut session, mut last) = (0i64, i64::MIN / 2);
    for (i, (song, at)) in plays.iter().enumerate() {
        if *at - last > SESSION_GAP_MS {
            session += 1;
        }
        last = *at;
        events.push(PlayEvent { id: i as i64, song_id: song.clone(), artist_id: None, session, started_ms: *at, ended_ms: Some(*at), heard_ms: Some(200_000), duration_ms: Some(200_000), ended: Some(Ended::Finished), origin: Origin::User });
    }
    let mut sizes: HashMap<i64, usize> = HashMap::new();
    for e in &events {
        *sizes.entry(e.session).or_default() += 1;
    }
    let mut eligible: Vec<i64> = sizes.iter().filter(|(_, n)| **n >= 6).map(|(s, _)| *s).collect();
    eligible.sort();
    let chosen: HashSet<i64> = eligible.iter().rev().take(held).copied().collect();
    println!("{} plays in {session} sessions; held out {} sessions of 6+ plays ({} plays)", events.len(), chosen.len(), chosen.iter().map(|s| sizes[s]).sum::<usize>());

    let mut answers: HashMap<String, Vec<Song>> = std::fs::read_to_string(&cache_path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let mut asked = 0;
    let mut hits = Vec::new();
    let mut report = None;
    for run in 0..RUNS {
        let _ = run;
        let r = replay(&events, |s| chosen.contains(&s), |m: &Moment| {
            let Some(anchor_id) = m.seeds.first() else { return Vec::new() };
            if !answers.contains_key(anchor_id) && asked < MAX_ASKS {
                asked += 1;
                let got = rt.block_on(client.similar_songs2(anchor_id, plan::SIMILAR_ASK)).unwrap_or_default();
                answers.insert(anchor_id.clone(), got);
                let _ = std::fs::write(&cache_path, serde_json::to_string(&answers).unwrap_or_default());
            }
            let Some(anchor) = db.song(anchor_id).ok().flatten() else { return Vec::new() };
            // No knowledge of later plays.
            let similar: Vec<Vec<Song>> = vec![answers.get(anchor_id).map(|a| a.iter().take(plan::SIMILAR_ASK as usize).cloned().map(clean).collect()).unwrap_or_default()];
            let found = with_deezer.then(|| {
                let seeds = vec![anchor.clone()];
                let mut found = rt.block_on(deezer_src::seed_sources(&db, dz, &seeds));
                let names = vec![navia_core::autoplay::main_artist(&anchor).to_string()];
                found.popular = rt.block_on(deezer_src::top_places(&db, dz, &names, tokio::time::Instant::now() + std::time::Duration::from_secs(6)));
                found
            });
            let heard: HashSet<&str> = m.before.iter().map(|e| e.song_id.as_str()).collect();
            let before: Vec<Song> = m.before.iter().rev().take(10).rev().filter_map(|e| db.song(&e.song_id).ok().flatten()).collect();
            let filter = junk::Filter::after(&before[before.len().saturating_sub(3)..]);
            let history: Vec<sequence::Item> = before.iter().map(sequence::Item::of).collect();
            let anchors = [anchor];
            let recent = HashSet::new();
            let day = m.before.last().map_or(0, |e| e.started_ms.div_euclid(86_400_000));
            let inputs = plan::Inputs { anchors: &anchors, similar: &similar, deezer: found.as_ref(), listenbrainz: &[], recent: &recent, fatigue: &Default::default(), day, provisional: false };
            let picks = plan::build(Some(&db), &inputs, &|s, _| heard.contains(s.id.as_str()) || filter.leaves_out(s), &history);
            picks.into_iter().take(TOP).map(|p| p.song.id).collect()
        });
        hits.push(r.hit_rate());
        report = Some(r);
    }
    let r = report.unwrap_or_default();
    let mean = hits.iter().sum::<f64>() / hits.len().max(1) as f64;
    println!("asks {} (finished next {}), server asks this run {asked}", r.asks, r.finished_next);
    println!("next song in the top {TOP}: {:.1}% (runs {})", mean * 100.0, hits.iter().map(|h| format!("{:.1}", h * 100.0)).collect::<Vec<_>>().join(", "));
    Ok(())
}
