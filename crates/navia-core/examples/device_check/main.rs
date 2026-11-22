//! Plays through a real output device. Run it with `PIPEWIRE_NODE` set to a private null sink.
//! Modes are `tone <file.wav>`, `silence <seconds>`, `watch <seconds>` and `streamed <id,...>`.
//! `NAVIA_DEVICE` picks a device and `STALL_MS=<ms>` stalls the callback once a second.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use navia_core::audio::player::{FileResolver, OutputPolicy, Player, PlayerConfig, PlayerStatus, QueueItem, ServerResolver};
use navia_core::client::{Client, ClientConfig, Credentials, WritePolicy};

fn usage() -> ! {
    eprintln!("usage: device_check tone FILE [FILE...] | silence SECONDS | follow RATE[,RATE...] SECONDS | watch SECONDS | seek FILE MS | streamed ID[,ID...] | list");
    std::process::exit(2);
}

fn snapshot_line(p: &Player) -> String {
    let s = p.snapshot();
    format!("status={:?} index={:?} pos_ms={} rate={} underrun_frames={} rebuffer_frames={} xruns={}", s.status, s.index, s.position_ms, s.output_rate, s.underrun_frames, s.rebuffer_frames, s.xruns)
}

fn wait_until(p: &Player, status: PlayerStatus, limit: Duration) -> bool {
    p.sync();
    let start = Instant::now();
    while start.elapsed() < limit {
        if p.snapshot().status == status {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn env_client() -> Client {
    let path = std::env::var("NAVIA_ENV_FILE").unwrap_or_else(|_| {
        let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.pop();
        p.pop();
        p.push(".env.local");
        p.to_string_lossy().into_owned()
    });
    let env: HashMap<String, String> = std::fs::read_to_string(path)
        .expect(".env.local")
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().trim_matches('"').to_string()))
        .collect();
    let mut cfg = ClientConfig::new(env["NAVIDROME_URL"].clone(), Credentials::new(env["NAVIDROME_USER"].clone(), env["NAVIDROME_PASSWORD"].clone()));
    cfg.write_policy = WritePolicy::ReadOnly;
    Client::new(cfg).expect("client")
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(mode) = args.first() else { usage() };
    if let Some(ms) = std::env::var("STALL_MS").ok().and_then(|v| v.parse().ok()) {
        navia_core::audio::output::inject_output_stalls(ms);
    }
    match mode.as_str() {
        "list" => {
            for d in Player::list_devices().expect("devices") {
                println!("{}\t{}\t{}", if d.is_default { "*" } else { " " }, d.id, d.name);
            }
        }
        "silence" => {
            let secs: u64 = args[1].parse().unwrap_or_else(|_| usage());
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("silence.wav");
            write_silence(&path, secs, 48_000);
            play_files(vec![path], None);
        }
        "follow" => {
            let rates: Vec<u32> = args.get(1).unwrap_or_else(|| usage()).split(',').map(|r| r.parse().unwrap_or_else(|_| usage())).collect();
            let secs: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(4);
            follow_rates(&rates, secs);
        }
        // Prints the device list each time the sound server's outputs change.
        "watch" => {
            let secs: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(10);
            let _watch = navia_core::audio::device_watch::watch(|| {
                let names: Vec<String> = Player::list_devices().unwrap_or_default().into_iter().map(|d| format!("{}{}", if d.is_default { "*" } else { "" }, d.id)).collect();
                println!("changed: {}", names.join(" "));
            });
            println!("watching: {}", _watch.is_some());
            std::thread::sleep(Duration::from_secs(secs));
        }
        "tone" => play_files(args[1..].iter().map(PathBuf::from).collect(), None),
        // A +6 dB peak at 1 kHz with a -2 dB preamp.
        "tone-eq" => play_files_eq(args[1..].iter().map(PathBuf::from).collect()),
        // The server's first radio station, for a few seconds. The URL is not printed.
        "radio-first" => radio_first(args.get(1).and_then(|s| s.parse().ok()).unwrap_or(8)),
        "seek" => play_files(vec![PathBuf::from(&args[1])], Some(args[2].parse().unwrap_or_else(|_| usage()))),
        "streamed" => streamed(&args[1]),
        _ => usage(),
    }
}

fn write_silence(path: &std::path::Path, secs: u64, rate: u32) {
    let frames = secs * u64::from(rate);
    let mut f = Vec::with_capacity(44 + frames as usize * 4);
    let data_len = (frames * 4) as u32;
    f.extend_from_slice(b"RIFF");
    f.extend_from_slice(&(36 + data_len).to_le_bytes());
    f.extend_from_slice(b"WAVEfmt ");
    f.extend_from_slice(&16u32.to_le_bytes());
    f.extend_from_slice(&1u16.to_le_bytes());
    f.extend_from_slice(&2u16.to_le_bytes());
    f.extend_from_slice(&rate.to_le_bytes());
    f.extend_from_slice(&(rate * 4).to_le_bytes());
    f.extend_from_slice(&4u16.to_le_bytes());
    f.extend_from_slice(&16u16.to_le_bytes());
    f.extend_from_slice(b"data");
    f.extend_from_slice(&data_len.to_le_bytes());
    f.resize(44 + frames as usize * 4, 0);
    std::fs::write(path, f).unwrap();
}

fn play_files(files: Vec<PathBuf>, seek_ms: Option<u64>) {
    let map: HashMap<String, PathBuf> = files.iter().enumerate().map(|(i, p)| (format!("{i}"), p.clone())).collect();
    let items: Vec<QueueItem> = (0..files.len()).map(|i| QueueItem::new(format!("{i}"), None)).collect();
    let player = Player::new(Arc::new(FileResolver(map)), PlayerConfig { policy: OutputPolicy::FollowTrack, ..Default::default() }).expect("player");
    if let Ok(device) = std::env::var("NAVIA_DEVICE") {
        player.set_output_device(Some(device));
    }
    let names: HashMap<String, String> = files.iter().enumerate().map(|(i, p)| (format!("{i}"), format!("{} \u{2013} device_check", p.file_stem().unwrap_or_default().to_string_lossy()))).collect();
    player.set_title_lookup(Arc::new(move |id: &str| names.get(id).cloned()));
    player.add_listener(|e| match e {
        navia_core::audio::player::PlayerEvent::OutputChanged { rate, device, .. } => println!("output {rate} {device:?}"),
        navia_core::audio::player::PlayerEvent::Error(text) => println!("error {text}"),
        _ => {}
    });
    player.set_queue(items, 0, 0, false);
    if let Some(ms) = seek_ms {
        assert!(wait_until(&player, PlayerStatus::Playing, Duration::from_secs(10)));
        std::thread::sleep(Duration::from_millis(1500));
        println!("before seek: {}", snapshot_line(&player));
        player.seek_ms(ms);
        std::thread::sleep(Duration::from_secs(3));
        println!("after seek: {}", snapshot_line(&player));
        player.stop();
    }
    assert!(wait_until(&player, PlayerStatus::Stopped, Duration::from_secs(600)));
    // Let the device play out its buffer before exit.
    std::thread::sleep(Duration::from_millis(500));
    println!("{}", snapshot_line(&player));
}

fn play_files_eq(files: Vec<PathBuf>) {
    use navia_core::audio::processing::{EqBand, EqSettings, FilterShape};
    let map: HashMap<String, PathBuf> = files.iter().enumerate().map(|(i, p)| (format!("{i}"), p.clone())).collect();
    let items: Vec<QueueItem> = (0..files.len()).map(|i| QueueItem::new(format!("{i}"), None)).collect();
    let player = Player::new(Arc::new(FileResolver(map)), PlayerConfig { policy: OutputPolicy::FollowTrack, ..Default::default() }).expect("player");
    player.set_eq(Some(&EqSettings { preamp_db: -2.0, bands: vec![EqBand { shape: FilterShape::Peaking, freq_hz: 1000.0, gain_db: 6.0, q: 1.0 }], limiter: true, ..Default::default() }));
    player.set_queue(items, 0, 0, false);
    assert!(wait_until(&player, PlayerStatus::Stopped, Duration::from_secs(600)));
    std::thread::sleep(Duration::from_millis(500));
    println!("{}", snapshot_line(&player));
}

fn radio_first(seconds: u64) {
    let client = env_client();
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let station = rt.block_on(client.internet_radio_stations()).expect("stations").into_iter().next().expect("no station");
    let url = station.stream_url.clone().expect("no stream url");
    println!("station: {}", station.name);
    let player = Player::new(Arc::new(ServerResolver::new(client).expect("resolver")), PlayerConfig { policy: OutputPolicy::FollowTrack, ..Default::default() }).expect("player");
    player.add_listener(|e| if matches!(e, navia_core::audio::player::PlayerEvent::Error(_)) { println!("{e:?}") });
    let started = Instant::now();
    player.play_live(QueueItem::new(station.id, None), url);
    let playing = wait_until(&player, PlayerStatus::Playing, Duration::from_secs(25));
    println!("playing={playing} after {:?}", started.elapsed());
    std::thread::sleep(Duration::from_secs(seconds));
    println!("title: {:?}", player.stream_title());
    println!("{}", snapshot_line(&player));
    player.stop();
}

fn streamed(ids: &str) {
    let client = env_client();
    assert_eq!(client.write_policy(), WritePolicy::ReadOnly);
    let resolver = Arc::new(ServerResolver::new(client).expect("resolver"));
    let items: Vec<QueueItem> = ids.split(',').map(|id| QueueItem::new(id.to_string(), None)).collect();
    let player = Player::new(resolver, PlayerConfig { policy: OutputPolicy::FollowTrack, ..Default::default() }).expect("player");
    player.add_listener(|e| println!("{e:?}"));
    player.set_queue(items, 0, 0, false);
    let started = Instant::now();
    assert!(wait_until(&player, PlayerStatus::Playing, Duration::from_secs(30)));
    println!("started after {:?}: {}", started.elapsed(), snapshot_line(&player));
    assert!(wait_until(&player, PlayerStatus::Stopped, Duration::from_secs(1800)));
    std::thread::sleep(Duration::from_millis(500));
    println!("finished after {:?}: {}", started.elapsed(), snapshot_line(&player));
}

fn follow_rates(rates: &[u32], secs: u64) {
    let dir = tempfile::tempdir().unwrap();
    let map: HashMap<String, PathBuf> = rates
        .iter()
        .map(|&rate| {
            let path = dir.path().join(format!("{rate}.wav"));
            write_silence(&path, secs + 20, rate);
            (rate.to_string(), path)
        })
        .collect();
    let player = Player::new(Arc::new(FileResolver(map)), PlayerConfig { policy: OutputPolicy::FollowTrack, ..Default::default() }).expect("player");
    player.set_volume(0.0);
    player.set_muted(true);
    if let Some(ms) = std::env::var("CROSSFADE_MS").ok().and_then(|v| v.parse().ok()) {
        player.set_crossfade_ms(ms);
        player.set_smart_crossfade(true);
    }
    player.add_listener(|e| match e {
        navia_core::audio::player::PlayerEvent::OutputChanged { rate, device_rate, .. } => println!("output {rate} device {device_rate:?}"),
        navia_core::audio::player::PlayerEvent::Error(text) => println!("error {text}"),
        _ => {}
    });
    for rate in rates {
        player.set_queue(vec![QueueItem::new(rate.to_string(), None)], 0, 0, false);
        std::thread::sleep(Duration::from_millis(300));
        assert!(wait_until(&player, PlayerStatus::Playing, Duration::from_secs(10)), "{rate} never played");
        println!("playing {rate}: {}", snapshot_line(&player));
        std::thread::sleep(Duration::from_secs(secs));
    }
    player.stop();
    std::thread::sleep(Duration::from_millis(300));
}
