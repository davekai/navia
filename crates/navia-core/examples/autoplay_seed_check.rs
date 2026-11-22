//! Checks Autoplay's Navia mode against pass criteria on seeds chosen by rule, with and
//! without Deezer. Read-only on the server, with answers cached in `<answers.json>`.
//! It changes the Deezer setting in the database, so pass a copy. `NAVIA_DEEZER=0|1` runs one setting.
//!
//! `NAVIDROME_URL=... NAVIDROME_USER=... NAVIDROME_PASSWORD=... cargo run -p navia-core --example
//! autoplay_seed_check -- <copy of library.sqlite> <answers.json> [song id...]`

use std::collections::{HashMap, HashSet};

use navia_core::autoplay::{Answers, Autoplay, Taken, main_artist, radio_songs};
use navia_core::client::{Client, ClientConfig, Credentials, Song, WritePolicy};
use navia_core::deezer::{self, Deezer};
use navia_core::engine::{self, AutoplayMode, plan};
use navia_core::library::{Library, LibraryConfig};
use navia_core::play_queue::PlayQueue;
use navia_core::recommend::{artist_key, genre_match};

const TOP_GENRES: usize = 5;

struct Ctx {
    client: Client,
    db: Library,
    cache: HashMap<String, Vec<Song>>,
    cache_path: String,
    rt: tokio::runtime::Runtime,
}

impl Ctx {
    fn similar(&mut self, id: &str) -> Vec<Song> {
        if !self.cache.contains_key(id) {
            let got = self.rt.block_on(self.client.similar_songs2(id, plan::SIMILAR_ASK)).unwrap_or_default();
            self.cache.insert(id.to_string(), got);
            let _ = std::fs::write(&self.cache_path, serde_json::to_vec(&self.cache).unwrap_or_default());
        }
        self.cache[id].iter().take(plan::SIMILAR_ASK as usize).cloned().collect()
    }

    fn deezer(&self, anchors: &[Song]) -> Option<engine::deezer_src::DeezerAnswers> {
        if !deezer::gate(&self.db).is_open() {
            return None;
        }
        let db = &self.db;
        Some(self.rt.block_on(async {
            let dz = Deezer::shared();
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
            let mut found = engine::deezer_src::seed_sources(db, dz, anchors).await;
            let mut names: Vec<String> = anchors.iter().map(|s| main_artist(s).to_string()).collect();
            for (a, _) in plan::neighbours(Some(db), anchors, Some(&found)).into_iter().take(engine::deezer_src::TOP_ARTISTS) {
                if let Some(a) = db.artist(&a).ok().flatten() {
                    names.push(a.name);
                }
            }
            found.popular = engine::deezer_src::top_places(db, dz, &names, deadline).await;
            found
        }))
    }

    /// Gives an Autoplay its context's answers, as the app's fetch does.
    fn answer(&mut self, a: &mut Autoplay) {
        while let Some(seeds) = a.wanted_fetch() {
            a.begin_fetch(&seeds);
            let anchors: Vec<Song> = seeds.iter().filter_map(|id| self.db.song(id).ok().flatten()).collect();
            let similar = seeds.iter().map(|s| self.similar(s)).collect();
            let found = self.deezer(&anchors);
            a.finish_answers(seeds, Answers { similar, deezer: found, ..Default::default() });
        }
    }
}

fn rows(db: &Library, sql: &str) -> Vec<String> {
    let c = rusqlite::Connection::open_with_flags(db.path(), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).expect("read the copy");
    let mut st = c.prepare(sql).expect("query");
    st.query_map([], |r| r.get::<_, String>(0)).expect("rows").filter_map(Result::ok).collect()
}

/// Seeds by rule.
fn pick_seeds(ctx: &mut Ctx, given: &[String]) -> Vec<(String, String)> {
    let db = ctx.db.clone();
    let mut out: Vec<(String, String)> = given.iter().map(|id| (id.clone(), "given".to_string())).collect();
    let genre = "lower(coalesce(json_extract(data, '$.genre'), ''))";
    let genres = rows(&db, &format!("SELECT {genre} g FROM song WHERE g <> '' GROUP BY g ORDER BY count(*) DESC LIMIT {TOP_GENRES}"));
    for g in &genres {
        let g = g.replace('\'', "''");
        // The largest artist's most played song, and a small artist's (2 to 4 songs).
        let large = rows(&db, &format!("SELECT id FROM song WHERE {genre} = '{g}' AND artist_id = (SELECT artist_id FROM song WHERE {genre} = '{g}' GROUP BY artist_id ORDER BY count(*) DESC LIMIT 1) ORDER BY play_count DESC, id LIMIT 1"));
        let small = rows(&db, &format!("SELECT id FROM song WHERE {genre} = '{g}' AND artist_id IN (SELECT artist_id FROM song GROUP BY artist_id HAVING count(*) BETWEEN 2 AND 4) ORDER BY play_count DESC, id LIMIT 1"));
        out.extend(large.into_iter().map(|id| (id, format!("{g}, large artist"))));
        out.extend(small.into_iter().map(|id| (id, format!("{g}, small artist"))));
    }
    out.extend(rows(&db, &format!("SELECT id FROM song WHERE {genre} = '' ORDER BY play_count DESC, id LIMIT 1")).into_iter().map(|id| (id, "no genre".into())));
    // The most played song in a genre unlike its artist's usual one.
    let odd = rows(
        &db,
        &format!(
            "WITH usual AS (SELECT artist_id, {genre} g, count(*) n FROM song WHERE {genre} <> '' GROUP BY artist_id, g),
                  top AS (SELECT artist_id, g FROM usual u WHERE n = (SELECT max(n) FROM usual v WHERE v.artist_id = u.artist_id) AND n >= 5)
             SELECT s.id FROM song s JOIN top t ON t.artist_id = s.artist_id WHERE {genre} <> '' AND {genre} <> t.g ORDER BY s.play_count DESC, s.id LIMIT 1"
        ),
    );
    out.extend(odd.into_iter().map(|id| (id, "genre unlike its artist's".into())));
    // A song the server has nothing similar for.
    for id in rows(&db, "SELECT id FROM song WHERE artist_id IN (SELECT artist_id FROM song GROUP BY artist_id HAVING count(*) = 1) ORDER BY id LIMIT 15") {
        if ctx.similar(&id).is_empty() {
            out.push((id, "no similar data".into()));
            break;
        }
    }
    let mut seen = HashSet::new();
    out.retain(|(id, _)| seen.insert(id.clone()));
    out
}

fn neighbourhood(db: &Library, seed: &Song, similar: &[Song], found: Option<&engine::deezer_src::DeezerAnswers>) -> HashSet<String> {
    let mut artists: HashSet<String> = similar.iter().map(artist_key).collect();
    artists.insert(artist_key(seed));
    if let Some(f) = found {
        artists.extend(f.radio.iter().flat_map(|(_, songs)| songs.iter().map(artist_key)));
    }
    artists.extend(plan::neighbours(Some(db), std::slice::from_ref(seed), found).into_iter().map(|(a, _)| a));
    artists
}

fn share(hood: &HashSet<String>, songs: &[Song]) -> f32 {
    let top = &songs[..songs.len().min(10)];
    if top.is_empty() {
        return 0.0;
    }
    top.iter().filter(|s| hood.contains(&artist_key(s))).count() as f32 / top.len() as f32
}

fn median_year_gap(seed: &Song, songs: &[Song]) -> Option<i32> {
    let y = seed.year.filter(|y| *y > 0)?;
    let mut gaps: Vec<i32> = songs.iter().take(10).filter_map(|s| s.year.filter(|v| *v > 0)).map(|v| (v - y).abs()).collect();
    if gaps.is_empty() {
        return None;
    }
    gaps.sort();
    Some(gaps[gaps.len() / 2])
}

fn line(s: &Song) -> String {
    format!("{} - {} [{} {}]", main_artist(s), s.title, s.genre.as_deref().unwrap_or("-"), s.year.unwrap_or(0))
}

fn taken(db: &Library, ids: &[String]) -> Vec<Taken> {
    ids.iter().filter_map(|id| db.song(id).ok().flatten()).map(|s| Taken::of(&s)).collect()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let usage = "usage: autoplay_seed_check <library.sqlite> <answers.json> [song id...]";
    let path = args.get(1).ok_or(usage)?;
    let cache_path = args.get(2).ok_or(usage)?.clone();
    let env = |k: &str| std::env::var(k).map_err(|_| format!("{k} is not set"));
    let mut cfg = ClientConfig::new(env("NAVIDROME_URL")?, Credentials::new(env("NAVIDROME_USER")?, env("NAVIDROME_PASSWORD")?));
    cfg.write_policy = WritePolicy::ReadOnly;
    let client = Client::new(cfg)?;
    let db = Library::open(&LibraryConfig { path: path.into() })?;
    engine::set_mode(&db, AutoplayMode::Navia)?;
    let cache: HashMap<String, Vec<Song>> = std::fs::read(&cache_path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let mut ctx = Ctx { client, db: db.clone(), cache, cache_path, rt: tokio::runtime::Runtime::new()? };
    let seeds = pick_seeds(&mut ctx, &args[3..]);
    let most_played: HashSet<String> = rows(&db, "SELECT id FROM song ORDER BY play_count DESC LIMIT 100").into_iter().collect();
    let played: Vec<Song> = rows(&db, "SELECT id FROM song ORDER BY play_count DESC LIMIT 400").into_iter().filter_map(|s| db.song(&s).ok().flatten()).collect();
    let genre_of = |s: &Song| engine::genre_of(s).to_lowercase();
    let modes: Vec<bool> = match std::env::var("NAVIA_DEEZER").ok().as_deref() {
        Some("1") => vec![true],
        Some("0") => vec![false],
        _ => vec![false, true],
    };
    let mut failures = 0;
    for with_deezer in modes {
        deezer::set_setting(&db, Some(with_deezer))?;
        println!("\n######## Deezer {}", if with_deezer { "on" } else { "off" });
        let mut tops: Vec<(String, String, Vec<String>, HashMap<String, (plan::Source, f32)>)> = Vec::new();
        for (id, why) in &seeds {
            let Some(seed) = db.song(id)? else { continue };
            let similar = ctx.similar(id);
            let found = ctx.deezer(std::slice::from_ref(&seed));
            let hood = neighbourhood(&db, &seed, &similar, found.as_ref());
            let server = radio_songs(&seed, &[similar.iter().take(navia_core::autoplay::PER_SEED as usize).cloned().collect()]);
            let queue = taken(&db, std::slice::from_ref(id));
            let mut alone = Autoplay::new();
            alone.attach(db.clone());
            alone.set_chosen([id.as_str()].into_iter());
            ctx.answer(&mut alone);
            let navia = alone.songs(&queue, 50);
            let whole: HashSet<String> = alone.songs(&queue, 100_000).into_iter().map(|s| s.id).collect();
            let anchors = [seed.clone()];
            let sims = [similar.clone()];
            let none = HashSet::new();
            let inputs = plan::Inputs { anchors: &anchors, similar: &sims, deezer: found.as_ref(), listenbrainz: &[], recent: &none, fatigue: &Default::default(), day: 0, provisional: false };
            let tiers: HashMap<String, (plan::Source, f32)> = plan::candidates(Some(&db), &inputs, &|_, _| false).into_iter().map(|p| (p.song.id.clone(), (p.tier, p.related))).collect();
            let why_line = |s: &Song| format!("{} {:?} {:.2}", line(s), tiers.get(&s.id).map(|t| t.0), tiers.get(&s.id).map_or(0.0, |t| t.1));
            // After an unrelated session of 5 much-played songs.
            let unrelated: Vec<String> = played.iter().filter(|s| artist_key(s) != artist_key(&seed) && genre_match(&genre_of(s), &genre_of(&seed)) == 0.0 && !hood.contains(&artist_key(s))).take(5).map(|s| s.id.clone()).collect();
            let mut after = Autoplay::new();
            after.attach(db.clone());
            after.set_chosen(unrelated.iter().map(String::as_str));
            ctx.answer(&mut after);
            let _ = after.songs(&taken(&db, &unrelated), 10);
            for u in &unrelated {
                if let Some(s) = db.song(u)? {
                    after.heard(&s);
                }
            }
            // Starting the song clears the queue, so only it is there.
            after.set_chosen([id.as_str()].into_iter());
            ctx.answer(&mut after);
            let navia_after = after.songs(&queue, 50);
            let alone_minus: Vec<&str> = navia.iter().map(|s| s.id.as_str()).filter(|i| !unrelated.iter().any(|u| u == i)).collect();
            let n = alone_minus.len().min(40);
            let same_after = navia_after.iter().map(|s| s.id.as_str()).take(n).collect::<Vec<_>>() == alone_minus[..n];
            // Three quick context changes, then the seed.
            let mut quick = Autoplay::new();
            quick.attach(db.clone());
            let mut pq: PlayQueue<String> = PlayQueue::default();
            let mut old_picks: Vec<String> = Vec::new();
            for other in unrelated.iter().take(3) {
                pq.start(vec![other.clone()], 0, false);
                quick.set_chosen([other.as_str()].into_iter());
                ctx.answer(&mut quick);
                let offer = quick.songs(&taken(&db, std::slice::from_ref(other)), 10);
                if let Some(pick) = offer.first() {
                    pq.hand_over(pick.id.clone());
                }
                old_picks.extend(offer.into_iter().map(|s| s.id));
            }
            pq.start(vec![id.clone()], 0, false);
            quick.set_chosen([id.as_str()].into_iter());
            let before_answer = quick.songs(&queue, 50);
            ctx.answer(&mut quick);
            let after_change = quick.songs(&queue, 50);
            // A pick from an earlier context that the seed's own plan doesn't hold.
            let stale = |s: &str| old_picks.iter().any(|p| p == s) && !whole.contains(s);
            let leftover = before_answer.iter().chain(after_change.iter()).filter(|s| stale(&s.id)).count() + pq.songs().iter().filter(|s| old_picks.contains(s)).count();

            let (ns, ss) = (share(&hood, &navia), share(&hood, &server));
            let (ny, sy) = (median_year_gap(&seed, &navia), median_year_gap(&seed, &server));
            let famous_outside = navia.iter().filter(|s| most_played.contains(&s.id) && !hood.contains(&artist_key(s))).count();
            let year_ok = match (ny, sy) {
                (Some(n), Some(s)) => n <= s,
                _ => true,
            };
            let pass = ns >= 0.8 && ns >= ss && same_after && leftover == 0 && year_ok && famous_outside == 0;
            if !pass {
                failures += 1;
            }
            println!("\n=== {} ({why}) {}", line(&seed), if pass { "PASS" } else { "FAIL" });
            println!(
                "  neighbourhood {:.0}% (Server {:.0}%), alone = after session: {same_after}, leftovers {leftover}, median year gap {} (Server {}), outside & top-100 {famous_outside}, plan {} songs",
                ns * 100.0,
                ss * 100.0,
                ny.map_or("-".into(), |v| v.to_string()),
                sy.map_or("-".into(), |v| v.to_string()),
                navia.len()
            );
            println!("  Server: {}", server.iter().take(10).map(line).collect::<Vec<_>>().join(" | "));
            println!("  Navia:  {}", navia.iter().take(10).map(&why_line).collect::<Vec<_>>().join(" | "));
            for s in navia.iter().filter(|s| most_played.contains(&s.id) && !hood.contains(&artist_key(s))) {
                println!("  outside & top-100: {}", why_line(s));
            }
            for s in navia.iter().take(10).filter(|s| !hood.contains(&artist_key(s))) {
                println!("  outside the neighbourhood: {}", why_line(s));
            }
            if !same_after {
                println!("  after:  {}", navia_after.iter().take(10).map(line).collect::<Vec<_>>().join(" | "));
            }
            tops.push((id.clone(), genre_of(&seed), navia.iter().take(50).map(|s| s.id.clone()).collect(), tiers.clone()));
        }
        for i in 0..tops.len() {
            for j in i + 1..tops.len() {
                let (a, b) = (&tops[i], &tops[j]);
                if a.1.is_empty() || b.1.is_empty() || genre_match(&a.1, &b.1) > 0.0 {
                    continue;
                }
                let shared: Vec<&String> = a.2.iter().filter(|x| b.2.contains(x)).collect();
                if !shared.is_empty() {
                    failures += 1;
                    let names: Vec<String> = shared.iter().filter_map(|id| db.song(id).ok().flatten()).map(|s| format!("{} ({:?} / {:?})", line(&s), a.3.get(&s.id), b.3.get(&s.id))).collect();
                    println!("  FAIL cross-genre: {} ({}) and {} ({}) share {}", a.0, a.1, b.0, b.1, names.join(" | "));
                }
            }
        }
    }
    println!("\nfailures: {failures}");
    Ok(())
}
