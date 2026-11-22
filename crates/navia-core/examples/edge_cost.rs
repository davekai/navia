//! measures the cost of smart transitions' edge analysis on real songs, streamed read-only from
//! the server, prints what it found and the join planned between each song and the next.
//! `edge_cost <song id[@bpm]...>`; login from `NAVIDROME_URL`, `NAVIDROME_USER`, `NAVIDROME_PASSWORD`.
//! with `NAVIA_JOIN_CHECK=1` each song is also played into the next through the engine into
//! memory, and the capture is compared sample by sample with each song decoded on its own.

use std::sync::Arc;
use std::time::Instant;

use navia_core::audio::edges::{SongFacts, analyze};
use navia_core::audio::capture;
use navia_core::audio::engine::{EngineConfig, EngineEvent, OutputMode, TrackFeed, TrackOpener, TrackRequest, output_from, spawn_feed};
use navia_core::audio::transition::{Join, SongTraits, TraitsCell, Transition, grid, head_boundaries, hold_ms, plan, tail_boundaries};
use navia_core::audio::player::{QueueItem, ServerResolver, TrackResolver};
use navia_core::client::{Client, ClientConfig, Credentials};

fn cpu_secs(clock: libc::clockid_t) -> f64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `ts` is a valid timespec for the call to write.
    unsafe { libc::clock_gettime(clock, &mut ts) };
    ts.tv_sec as f64 + ts.tv_nsec as f64 / 1e9
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let ids: Vec<String> = args.iter().map(|a| a.split('@').next().unwrap_or_default().to_string()).collect();
    let bpms: Vec<Option<f32>> = args.iter().map(|a| a.split('@').nth(1).and_then(|b| b.parse().ok())).collect();
    let mut songs: Vec<SongTraits> = Vec::new();
    let env = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"));
    let client = Client::new(ClientConfig::new(env("NAVIDROME_URL"), Credentials::new(env("NAVIDROME_USER"), env("NAVIDROME_PASSWORD"))).read_only()).expect("client");
    let resolver = ServerResolver::new(client).expect("resolver");
    let (mut thread_total, mut process_total, mut wall_total) = (0.0, 0.0, 0.0);
    for id in &ids {
        let opener = resolver.opener(&QueueItem::new(id.clone(), None));
        let (t0, p0, w0) = (cpu_secs(libc::CLOCK_THREAD_CPUTIME_ID), cpu_secs(libc::CLOCK_PROCESS_CPUTIME_ID), Instant::now());
        let result = analyze(opener.as_ref(), None);
        let (thread, process, wall) = (cpu_secs(libc::CLOCK_THREAD_CPUTIME_ID) - t0, cpu_secs(libc::CLOCK_PROCESS_CPUTIME_ID) - p0, w0.elapsed().as_secs_f64());
        thread_total += thread;
        process_total += process;
        wall_total += wall;
        let bpm = bpms[songs.len()];
        songs.push(SongTraits { song_id: id.clone(), edges: result.as_ref().ok().cloned(), facts: SongFacts { bpm, track_gain_db: None } });
        match result {
            Ok(e) => {
                let show = |t: Option<navia_core::audio::edges::Tempo>| t.map_or("none".to_string(), |t| format!("{:.2} BPM (confidence {:.2})", t.bpm, t.confidence));
                println!("{id}: cpu {:.0} ms (all threads {:.0} ms), wall {:.0} ms, {} B kept", thread * 1000.0, process * 1000.0, wall * 1000.0, serde_json::to_string(&e).map_or(0, |s| s.len()));
                println!("  tag {bpm:?} | start: {} beats, {} | end: {} beats, {}", e.head.beats.len(), show(e.head.tempo), e.tail.beats.len(), show(e.tail.tempo));
                println!("  own estimate | start: {} | end: {}", show(e.head.own_tempo), show(e.tail.own_tempo));
                println!("  start: first sound {} ms, body {} ms | end: last sound {} ms before the end, outro {} ms, decayed {} ms", e.head.intro_ms, e.head.body_ms, e.tail.silence_ms, e.tail.outro_ms, e.tail.decayed_ms);
                if let Some(g) = grid(&e.tail.beats, -1.0) {
                    println!("  end grid: {:.2} BPM, period {:.1} ms, downbeat offset {}", g.bpm(), g.period, g.downbeat);
                    let list: Vec<String> = tail_boundaries(&e.tail, &g).iter().map(|b| format!("{:.2}s{}{}({:.1} dB)", f64::from(b.at_ms - e.tail.silence_ms.min(b.at_ms)) / 1000.0, if b.strong { "*" } else { "" }, b.bars, b.novelty_db)).collect();
                    println!("  phrase starts before the last sound (* strong, bars, novelty): {}", list.join(" "));
                }
                if let Some(g) = grid(&e.head.beats, 1.0) {
                    let list: Vec<String> = head_boundaries(&e.head, &g).iter().map(|b| format!("{:.2}s{}{}", f64::from(b.at_ms) / 1000.0, if b.strong { "*" } else { "" }, b.bars)).collect();
                    println!("  start grid: {:.2} BPM, first downbeat {:.0} ms; phrase starts: {}", g.bpm(), g.downbeat_from(f64::from(e.head.intro_ms) - 25.0), list.join(" "));
                }
            }
            Err(e) => println!("{id}: failed: {e}"),
        }
    }
    if std::env::var("NAVIA_JOIN_CHECK").is_ok_and(|v| v == "1") {
        for i in 1..songs.len() {
            join_check(&resolver, &ids[i - 1..=i], &songs[i - 1..=i]);
        }
    }
    for pair in songs.windows(2) {
        let held_ms = pair[0].edges.as_ref().map_or(6_000, hold_ms);
        let t = plan(Join { out: Some(&pair[0]), inc: Some(&pair[1]), out_gain: 1.0, in_gain: 1.0, continues: false, plain_ms: 6_000, held_ms });
        println!("{} -> {}: {t}", pair[0].song_id, pair[1].song_id);
    }
    let n = ids.len().max(1) as f64;
    println!("mean per song: cpu {:.0} ms, all threads {:.0} ms, wall {:.0} ms", thread_total / n * 1000.0, process_total / n * 1000.0, wall_total / n * 1000.0);
}

const CHECK_CROSSFADE_MS: u64 = 6_000;

struct Next(Option<TrackRequest>);

impl TrackFeed for Next {
    fn peek(&mut self) -> Option<TrackRequest> {
        self.0.clone()
    }

    fn advance(&mut self) -> Option<TrackRequest> {
        self.0.take()
    }
}

fn decode_all(opener: &dyn TrackOpener) -> (Vec<f32>, u32) {
    let (mut d, _) = opener.open(0).expect("open");
    let (mut all, mut block) = (Vec::new(), Vec::new());
    while d.next_block(&mut block).expect("decode") {
        all.extend_from_slice(&block);
    }
    (all, d.sample_rate)
}

/// play song 0 into song 1 with smart transitions and check the capture against each song alone.
fn join_check(resolver: &ServerResolver, ids: &[String], songs: &[SongTraits]) {
    let reqs: Vec<TrackRequest> = ids
        .iter()
        .zip(songs)
        .enumerate()
        .map(|(i, (id, traits))| {
            let slot = Arc::new(TraitsCell::default());
            slot.set(traits.clone());
            TrackRequest { token: i, opener: resolver.opener(&QueueItem::new(id.clone(), None)), start_ms: 0, duration_ms: None, album: None, traits: Some(slot) }
        })
        .collect();
    let (a, rate) = decode_all(reqs[0].opener.as_ref());
    let (b, rate_b) = decode_all(reqs[1].opener.as_ref());
    assert_eq!(rate, rate_b, "pick two songs at the same rate");
    let mut cfg = EngineConfig::new(OutputMode::FollowTrack);
    cfg.crossfade_frames = (CHECK_CROSSFADE_MS * u64::from(rate) / 1000) as usize;
    cfg.smart_transitions = true;
    let first = reqs[0].clone();
    let c = capture::run(output_from(spawn_feed(Box::new(Next(Some(reqs[1].clone()))), cfg, Some(first), true)));
    let held_ms = songs[0].edges.as_ref().map_or(0, hold_ms).max(CHECK_CROSSFADE_MS as u32);
    let p: Transition = plan(Join { out: Some(&songs[0]), inc: Some(&songs[1]), out_gain: 1.0, in_gain: 1.0, continues: false, plain_ms: CHECK_CROSSFADE_MS as u32, held_ms });
    let frames = |ms: u32| (u64::from(ms) * u64::from(rate) / 1000) as usize;
    let (start, overlap, skip) = (frames(p.start_before_ms), frames(p.overlap_ms), frames(p.skip_ms));
    let joined = c.events.iter().find_map(|e| match e {
        EngineEvent::TrackStart { track: 1, output_frame, overlap, .. } => Some((*output_frame as usize, *overlap)),
        _ => None,
    });
    let (a_len, b_len) = (a.len() / 2, b.len() / 2);
    let exact = |x: &[f32], y: &[f32]| x.len() == y.len() && x.iter().zip(y).all(|(p, q)| p.to_bits() == q.to_bits());
    let at = a_len - start;
    let rms_db = |x: &[f32]| 10.0 * (x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len().max(1) as f64).max(1e-12).log10();
    let silence_ms = songs[0].edges.as_ref().map_or(0, |e| e.tail.silence_ms);
    println!("join check {} -> {}: {p}", ids[0], ids[1]);
    println!("  overlap starts {:.2} s before the last sound ({:.2} s before the file's end)", f64::from(p.start_before_ms - silence_ms) / 1000.0, f64::from(p.start_before_ms) / 1000.0);
    let dropped = &a[((at + overlap) * 2).min(a.len())..];
    println!("  dropped from A: {:.2} s at {:.1} dBFS RMS; skipped from B: {:.2} s at {:.1} dBFS RMS (silence is under {} dBFS)", dropped.len() as f64 / 2.0 / f64::from(rate), rms_db(dropped), skip as f64 / f64::from(rate), rms_db(&b[..skip * 2]), navia_core::audio::edges::SILENCE_DBFS);
    println!("  B starts at frame {joined:?}, expected ({at}, {overlap})");
    println!("  length {} frames, expected {}", c.samples.len() / 2, at + b_len - skip);
    println!("  A bit-exact up to the overlap: {}", exact(&c.samples[..at * 2], &a[..at * 2]));
    println!("  B bit-exact after the overlap: {}", c.samples.len() >= (at + overlap) * 2 && exact(&c.samples[(at + overlap) * 2..], &b[(skip + overlap) * 2..]));
}
