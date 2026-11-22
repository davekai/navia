//! times For You's daily mixes, Essentials and the change check on a copy of a real library
//! against its server. the client is read-only and the play history isn't fetched (it logs in).
//! similar artists stored on any day are moved to today first, so no `getArtistInfo2` is sent
//! (it can refresh an artist's info on the server).
//!
//! `NAVIDROME_URL=... NAVIDROME_USER=... NAVIDROME_PASSWORD=... cargo run --release -p navia-core
//! --example foryou_speed -- <copy of library.sqlite> [cold|warm|essentials|check|check-changed]`
//!
//! - `cold`: forgets today's mixes, then makes them, printing when each lands.
//! - `warm`: the page from what's stored, and a second build (nothing to ask).
//! - `essentials`: forgets today's top songs, then finds Essentials, then again from storage.
//! - `check`: a change check with nothing changed (run it twice; the first may catch up).
//! - `check-changed`: makes the copy older first (one regular playlist's stamp, the newest
//!   played album's plays and the play-count checkpoint), then checks.

use std::time::Instant;

use navia_core::client::{Client, ClientConfig, Credentials, WritePolicy};
use navia_core::library::sync::{SyncOptions, Syncer};
use navia_core::library::{Library, LibraryConfig, essentials, for_you, taste};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("usage: foryou_speed <library.sqlite> <mode>")?;
    let mode = std::env::args().nth(2).unwrap_or_else(|| "cold".into());
    let env = |k: &str| std::env::var(k).map_err(|_| format!("{k} is not set"));
    let mut cfg = ClientConfig::new(env("NAVIDROME_URL")?, Credentials::new(env("NAVIDROME_USER")?, env("NAVIDROME_PASSWORD")?));
    cfg.write_policy = WritePolicy::ReadOnly;
    let client = Client::new(cfg)?;
    let sql = rusqlite::Connection::open(&path)?;
    let library = Library::open(&LibraryConfig { path: path.clone().into() })?;
    let day = taste::local_day(taste::now_unix());
    sql.execute("UPDATE similar_artist SET day = ?1", [day])?;
    match mode.as_str() {
        "cold" => {
            sql.execute("DELETE FROM meta WHERE key = 'for_you_mixes'", [])?;
            let t = Instant::now();
            let lib = library.clone();
            let changed = move || {
                let n = lib.mixes(day).map(|m| m.len()).unwrap_or(0);
                println!("{:>6.2} s  {n} mixes stored", t.elapsed().as_secs_f32());
            };
            for_you::refresh_for_you_with(&client, &library, day, &changed).await?;
            println!("cold build {:.2} s", t.elapsed().as_secs_f32());
            for m in library.mixes(day)? {
                println!("  {} ({:?}) {} songs", m.name, m.kind, m.songs.len());
            }
        }
        "warm" => {
            let t = Instant::now();
            let page = library.for_you(day)?;
            println!("stored page {:.3} s: {} mixes, needs_refresh {}", t.elapsed().as_secs_f32(), page.mixes.len(), page.needs_refresh);
            let t = Instant::now();
            for_you::refresh_for_you(&client, &library, day).await?;
            println!("build when done for the day {:.3} s", t.elapsed().as_secs_f32());
        }
        "essentials" => {
            sql.execute("DELETE FROM top_songs", [])?;
            let t = Instant::now();
            let found = essentials::refresh_essentials(&client, &library, false, day).await?;
            println!("essentials asked {:.2} s: {} tiles", t.elapsed().as_secs_f32(), found.len());
            let t = Instant::now();
            let again = essentials::refresh_essentials(&client, &library, false, day).await?;
            println!("essentials again {:.3} s: {} tiles", t.elapsed().as_secs_f32(), again.len());
            let t = Instant::now();
            let stored = library.essentials(day)?;
            println!("essentials stored {:.3} s: {} tiles", t.elapsed().as_secs_f32(), stored.len());
        }
        "check" | "check-changed" => {
            if mode == "check-changed" {
                let n = sql.execute("UPDATE playlist SET changed = '2000-01-01T00:00:00Z' WHERE rowid = (SELECT rowid FROM playlist WHERE json_extract(data, '$.validUntil') IS NULL AND readonly = 0 LIMIT 1)", [])?;
                println!("playlists made older: {n}");
                let marks: Vec<String> = {
                    let mut st = sql.prepare("SELECT played FROM album WHERE played IS NOT NULL ORDER BY played DESC LIMIT 3")?;
                    st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?
                };
                if let Some(older) = marks.last() {
                    sql.execute("UPDATE meta SET value = ?1 WHERE key = 'annotation_played_mark'", [older])?;
                    let n = sql.execute("UPDATE album SET play_count = play_count - 1 WHERE played = ?1", [&marks[0]])?;
                    println!("albums with plays made older: {n}");
                }
            }
            let syncer = Syncer::new(client.clone(), library.clone(), SyncOptions::default());
            let t = Instant::now();
            let changes = syncer.check_for_changes(&|_| {}).await?;
            println!("check {:.3} s: {changes:?}", t.elapsed().as_secs_f32());
        }
        "probe" => {
            let rows: Vec<u32> = library.taste()?.songs.iter().take(9).map(|s| s.rowid).collect();
            let ids: Vec<String> = library.songs_by_rowid(&rows)?.into_iter().map(|(_, s)| s.id).collect();
            let mut alone = Vec::new();
            for id in &ids {
                let t = Instant::now();
                let n = client.similar_songs2(id, 60).await.map(|s| s.len()).unwrap_or(0);
                alone.push((t.elapsed().as_secs_f32(), n));
            }
            println!("one at a time: {:?}, sum {:.2} s", alone.iter().map(|(t, _)| format!("{t:.2}")).collect::<Vec<_>>(), alone.iter().map(|(t, _)| t).sum::<f32>());
            for width in [3usize, 9] {
                let t = Instant::now();
                for chunk in ids.chunks(width) {
                    navia_core::autoplay::fetch_count(&client, chunk, 60).await;
                }
                println!("{width} at once: {:.2} s", t.elapsed().as_secs_f32());
            }
        }
        other => return Err(format!("unknown mode {other}").into()),
    }
    Ok(())
}
