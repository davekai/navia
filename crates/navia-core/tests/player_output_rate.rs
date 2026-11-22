//! A device change picks the rate fresh for the new device, so a default that only takes 48 and 96 kHz
//! is never left silent at 44.1 kHz. A device that can't start is its own event, not a song error.

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::audio::{synth, write_wav};
use navia_core::audio::output::{CaptureBackend, DeviceCaps, DeviceInfo, OutputBackend, OutputHandle};
use navia_core::audio::player::{FileResolver, OutputPolicy, Player, PlayerConfig, PlayerEvent, QueueItem};
use navia_core::audio::render::RenderCore;
use navia_core::{Error, Result};

const RECEIVER: &str = "airplay:tv";

/// Two outputs, a receiver at 44.1 kHz only and a default at 48 or 96 kHz only.
/// Opens at any other rate fail, as cpal's do. Every open is logged.
struct TwoDevices {
    capture: CaptureBackend,
    opens: Arc<Mutex<Vec<(Option<String>, u32)>>>,
}

fn rates(device: Option<&str>) -> Vec<(u32, u32)> {
    match device {
        Some(_) => vec![(44_100, 44_100)],
        None => vec![(48_000, 48_000), (96_000, 96_000)],
    }
}

impl OutputBackend for TwoDevices {
    fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        Ok(vec![
            DeviceInfo { id: "earphones".into(), name: "Earphones".into(), is_default: true },
            DeviceInfo { id: RECEIVER.into(), name: "Living Room TV".into(), is_default: false },
        ])
    }
    fn caps(&self, device: Option<&str>) -> Result<DeviceCaps> {
        let rates = rates(device);
        Ok(DeviceCaps { default_rate: rates[0].0, rates })
    }
    fn open(&mut self, device: Option<&str>, rate: u32, core: Arc<Mutex<RenderCore>>) -> Result<Box<dyn OutputHandle>> {
        self.opens.lock().unwrap().push((device.map(str::to_string), rate));
        if !rates(device).iter().any(|(lo, hi)| (*lo..=*hi).contains(&rate)) {
            return Err(Error::Audio(format!("device doesn't support {rate} Hz")));
        }
        self.capture.open(device, rate, core)
    }
}

struct Run {
    player: Player,
    opens: Arc<Mutex<Vec<(Option<String>, u32)>>>,
    events: Arc<Mutex<Vec<PlayerEvent>>>,
    /// Songs heard, in order, and the rate the output ran at for each.
    heard: Arc<Mutex<Vec<String>>>,
}

fn start(crossfade_ms: u32, policy: OutputPolicy, dir: &std::path::Path) -> Run {
    // a 44.1 kHz song, as on the receiver, a 48 kHz one and a 96 kHz one.
    write_wav(&dir.join("a.wav"), &synth(44_100 * 6, 44_100, 0.1), 44_100);
    write_wav(&dir.join("b.wav"), &synth(48_000 * 6, 48_000, 0.7), 48_000);
    write_wav(&dir.join("c.wav"), &synth(96_000 * 6, 96_000, 0.4), 96_000);
    let files = [("a".to_string(), dir.join("a.wav")), ("b".to_string(), dir.join("b.wav")), ("c".to_string(), dir.join("c.wav"))].into();
    let opens = Arc::new(Mutex::new(Vec::new()));
    // Real time, so the songs are still playing while the steps run.
    let backend = TwoDevices { capture: CaptureBackend { speed: 1.0, rates: (8_000, 384_000), default_rate: 48_000, ..Default::default() }, opens: opens.clone() };
    let config = PlayerConfig { policy, ..Default::default() };
    let player = Player::with_backend(Arc::new(FileResolver(files)), Box::new(backend), config).unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let heard = Arc::new(Mutex::new(Vec::new()));
    let (sink, songs) = (events.clone(), heard.clone());
    player.add_listener(move |e| match e {
        PlayerEvent::Error(_) | PlayerEvent::OutputError { .. } => sink.lock().unwrap().push(e.clone()),
        PlayerEvent::TrackChanged { item, .. } => songs.lock().unwrap().push(item.id.clone()),
        _ => {}
    });
    player.set_crossfade_ms(crossfade_ms);
    Run { player, opens, events, heard }
}

fn wait_for(limit: Duration, mut check: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        if check() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

fn tempdir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("navia-output-rate-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Each open on the default asks for 48 kHz without error, with and without a crossfade.
fn back_to_the_default_opens_at_its_rate(crossfade_ms: u32) {
    let dir = tempdir(&format!("back{crossfade_ms}"));
    let run = start(crossfade_ms, OutputPolicy::DeviceDefault, &dir);
    run.player.set_output_device(Some(RECEIVER.into()));
    run.player.set_queue(vec![QueueItem::new("a".into(), None), QueueItem::new("b".into(), None)], 0, 1_000, false);
    assert!(wait_for(Duration::from_secs(5), || run.opens.lock().unwrap().iter().any(|(d, _)| d.is_some())), "the receiver never opened");

    run.player.set_output_device(None);
    assert!(
        wait_for(Duration::from_secs(5), || run.opens.lock().unwrap().iter().any(|(d, r)| d.is_none() && *r == 48_000)),
        "the default never opened at its rate: {:?} {:?}",
        run.opens.lock().unwrap(),
        run.events.lock().unwrap()
    );
    // A different song from the list, as from a click on a row, heard on the default as it is.
    run.heard.lock().unwrap().clear();
    run.player.set_queue(vec![QueueItem::new("b".into(), None)], 0, 0, false);
    assert!(wait_for(Duration::from_secs(5), || run.heard.lock().unwrap().iter().any(|id| id == "b")), "b was never heard");
    assert!(wait_for(Duration::from_secs(2), || run.player.snapshot().position_ms > 300), "b didn't play");
    run.player.sync();
    assert_eq!(run.player.snapshot().output_rate, 48_000);

    let opens = run.opens.lock().unwrap().clone();
    assert_eq!(opens[0], (Some(RECEIVER.to_string()), 44_100), "{opens:?}");
    let on_default: Vec<u32> = opens.iter().filter(|(d, _)| d.is_none()).map(|(_, r)| *r).collect();
    assert!(on_default.iter().all(|r| *r == 48_000), "crossfade {crossfade_ms} ms: the default was asked {on_default:?}");
    let events = run.events.lock().unwrap().clone();
    assert!(events.is_empty(), "crossfade {crossfade_ms} ms: {events:?}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_device_change_takes_the_new_devices_rate_with_a_crossfade() {
    back_to_the_default_opens_at_its_rate(5_000);
}

#[test]
fn a_device_change_takes_the_new_devices_rate_without_a_crossfade() {
    back_to_the_default_opens_at_its_rate(0);
}

/// A device that won't start is an `OutputError` naming it, not a song error.
#[test]
fn a_device_that_cannot_start_is_its_own_error() {
    let dir = tempdir("refuse");
    let run = start(0, OutputPolicy::DeviceDefault, &dir);
    // A fixed 44.1 kHz policy asks the default for a rate it refuses.
    run.player.set_output_policy(OutputPolicy::Fixed(44_100));
    run.player.set_queue(vec![QueueItem::new("a".into(), None)], 0, 0, false);
    assert!(wait_for(Duration::from_secs(5), || !run.events.lock().unwrap().is_empty()), "no error came");
    let events = run.events.lock().unwrap().clone();
    match &events[0] {
        PlayerEvent::OutputError { device, message } => {
            assert_eq!(device.as_deref(), Some("Earphones"));
            assert!(message.contains("44100"), "{message}");
        }
        other => panic!("expected an output error, got {other:?}"),
    }
    assert!(!events.iter().any(|e| matches!(e, PlayerEvent::Error(_))), "{events:?}");
    let _ = std::fs::remove_dir_all(dir);
}

/// following the song's rate on a device that takes 48 and 96 kHz: a click on a 96 kHz song while
/// a 48 kHz one plays reopens the output at 96 kHz, and a click back on the 48 kHz song returns to
/// 48 kHz, with and without a crossfade (a crossfade only holds the rate while it mixes two songs).
fn a_click_follows_the_songs_rate(crossfade_ms: u32) {
    let dir = tempdir(&format!("follow{crossfade_ms}"));
    let run = start(crossfade_ms, OutputPolicy::FollowTrack, &dir);
    let play = |id: &str, rate: u32| {
        run.heard.lock().unwrap().clear();
        run.player.set_queue(vec![QueueItem::new(id.into(), None)], 0, 0, false);
        assert!(wait_for(Duration::from_secs(5), || run.heard.lock().unwrap().iter().any(|h| h == id)), "{id} was never heard");
        assert!(wait_for(Duration::from_secs(2), || run.player.snapshot().position_ms > 300), "{id} didn't play");
        run.player.sync();
        assert_eq!(run.player.snapshot().output_rate, rate, "crossfade {crossfade_ms} ms, {id}: {:?}", run.opens.lock().unwrap());
    };
    play("b", 48_000);
    play("c", 96_000);
    play("b", 48_000);
    let opens: Vec<u32> = run.opens.lock().unwrap().iter().map(|(_, r)| *r).collect();
    assert_eq!(opens, [48_000, 96_000, 48_000], "crossfade {crossfade_ms} ms");
    let events = run.events.lock().unwrap().clone();
    assert!(events.is_empty(), "crossfade {crossfade_ms} ms: {events:?}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_click_follows_the_songs_rate_with_a_crossfade() {
    a_click_follows_the_songs_rate(5_000);
}

#[test]
fn a_click_follows_the_songs_rate_without_a_crossfade() {
    a_click_follows_the_songs_rate(0);
}
