//! The player over HTTP against a local fake of Navidrome's `/stream`, into an in-memory capture device.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::audio::{encode, have_ffmpeg, synth, to_f32, wav_bytes, write_wav};
use common::http::{Faults, Library, TestServer};
use navia_core::audio::output::{CaptureBackend, CaptureBuffer};
use navia_core::audio::player::{OutputPolicy, Player, PlayerConfig, PlayerEvent, PlayerStatus, QueueItem, ServerResolver};
use navia_core::client::{Client, ClientConfig, Credentials};

fn client(url: &str) -> Client {
    Client::new(ClientConfig::new(url.to_string(), Credentials::new("t".to_string(), "t".to_string()))).unwrap()
}

fn items(ids: &[(&str, u64)]) -> Vec<QueueItem> {
    ids.iter().map(|(id, ms)| QueueItem::new(id.to_string(), Some(*ms))).collect()
}

fn player(url: &str, speed: f64, rates: (u32, u32), policy: OutputPolicy) -> (Player, Arc<CaptureBuffer>) {
    let backend = CaptureBackend { speed, rates, ..Default::default() };
    let capture = backend.capture.clone();
    let resolver = ServerResolver::new(client(url)).unwrap();
    let player = Player::with_backend(Arc::new(resolver), Box::new(backend), PlayerConfig { policy, ..Default::default() }).unwrap();
    (player, capture)
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

fn wait_status(p: &Player, status: PlayerStatus, limit: Duration) {
    p.sync();
    assert!(wait_for(limit, || p.snapshot().status == status), "never reached {status:?}, snapshot {:?}", p.snapshot());
}

/// Drops the silence the capture recorded before the first track began.
fn audio_from_first(samples: &[f32]) -> &[f32] {
    let first = samples.chunks_exact(2).position(|f| f[0] != 0.0 || f[1] != 0.0).unwrap_or(0);
    &samples[first * 2..]
}

fn assert_contains_exact(haystack: &[f32], needle: &[f32], what: &str) {
    assert!(haystack.len() >= needle.len(), "{what}: captured {} samples, expected at least {}", haystack.len(), needle.len());
    let bad = haystack.iter().zip(needle).position(|(a, b)| a.to_bits() != b.to_bits());
    assert_eq!(bad, None, "{what}: differs at sample {bad:?}");
}

#[test]
fn streamed_wav_tracks_join_bit_exact_with_no_gap() {
    let a = synth(44_100 * 3 + 137, 44_100, 0.2);
    let b = synth(44_100 * 2 + 971, 44_100, 1.7);
    let mut library = Library::default();
    library.raw.insert("a".into(), wav_bytes(&a, 44_100));
    library.raw.insert("b".into(), wav_bytes(&b, 44_100));
    let server = TestServer::start(library, Faults::default());

    let (p, capture) = player(&server.url, 25.0, (8_000, 192_000), OutputPolicy::FollowTrack);
    p.set_queue(items(&[("a", 3003), ("b", 2022)]), 0, 0, false);
    wait_status(&p, PlayerStatus::Stopped, Duration::from_secs(30));
    let snap = p.snapshot();
    assert_eq!(snap.underrun_frames, 0, "underruns");

    let samples = capture.samples();
    let audio = audio_from_first(&samples);
    let expected: Vec<f32> = to_f32(&a).into_iter().chain(to_f32(&b)).collect();
    assert_contains_exact(audio, &expected, "gapless join");
    assert!(audio[expected.len()..].iter().all(|s| *s == 0.0), "silence after the queue ends");
}

#[test]
fn slow_throttled_server_still_has_no_underruns_thanks_to_buffering() {
    if !have_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mut library = Library::default();
    let mut expected: Vec<f32> = Vec::new();
    let mut queue = Vec::new();
    for (i, seed) in [0.5, 1.1, 2.3].iter().enumerate() {
        let pcm = synth(44_100 * 4 + i * 91, 44_100, *seed);
        let wav = dir.path().join(format!("{i}.wav"));
        write_wav(&wav, &pcm, 44_100);
        let flac = dir.path().join(format!("{i}.flac"));
        encode(&wav, &flac, &["-c:a", "flac"]);
        library.raw.insert(format!("{i}"), std::fs::read(&flac).unwrap());
        expected.extend(to_f32(&pcm));
        queue.push((format!("{i}"), 4000u64));
    }
    // Comfortably faster than FLAC's bitrate, but stalls before every response.
    let server = TestServer::start(library, Faults { throttle: 2_000_000, stall: Duration::from_millis(150), ..Default::default() });

    let (p, capture) = player(&server.url, 8.0, (8_000, 192_000), OutputPolicy::FollowTrack);
    let refs: Vec<(&str, u64)> = queue.iter().map(|(i, m)| (i.as_str(), *m)).collect();
    p.set_queue(items(&refs), 0, 0, false);
    wait_status(&p, PlayerStatus::Stopped, Duration::from_secs(60));

    let snap = p.snapshot();
    println!("underrun frames {}, rebuffer frames {}, requests {}", snap.underrun_frames, snap.rebuffer_frames, server.requests().len());
    let samples = capture.samples();
    let audio = audio_from_first(&samples);
    assert_contains_exact(audio, &expected, "throttled gapless");
    assert_eq!(snap.underrun_frames, 0);
}

#[test]
fn seek_during_streaming_resumes_at_the_exact_frame() {
    let a = synth(44_100 * 20, 44_100, 0.4);
    let mut library = Library::default();
    library.raw.insert("a".into(), wav_bytes(&a, 44_100));
    let server = TestServer::start(library, Faults::default());

    let (p, capture) = player(&server.url, 1.0, (8_000, 192_000), OutputPolicy::FollowTrack);
    p.set_queue(items(&[("a", 20_000)]), 0, 0, false);
    wait_status(&p, PlayerStatus::Playing, Duration::from_secs(10));
    assert!(wait_for(Duration::from_secs(5), || capture.len() > 44_100 * 2), "no audio yet");

    let target_ms = 12_345u64;
    p.seek_ms(target_ms);
    let target_frame = target_ms as usize * 44_100 / 1000;
    let reference = to_f32(&a);
    let probe = &reference[target_frame * 2..(target_frame + 8820) * 2];

    assert!(wait_for(Duration::from_secs(10), || p.snapshot().position_ms >= target_ms + 1200), "position never passed the seek");
    let snap = p.snapshot();
    assert!(snap.position_ms.abs_diff(target_ms) < 1500, "position after seek {}", snap.position_ms);

    let captured = capture.samples();
    // Look for the first probe frame exactly, then compare 0.2 s of audio.
    let start = captured.chunks_exact(2).enumerate().find(|(_, f)| f[0].to_bits() == probe[0].to_bits() && f[1].to_bits() == probe[1].to_bits()).map(|(i, _)| i).expect("seek target not found in capture");
    assert_contains_exact(&captured[start * 2..], probe, "after seek");
    p.stop();
}

#[test]
fn undecodable_originals_fall_back_to_a_server_transcode() {
    if !have_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let wav = dir.path().join("a.wav");
    let pcm = synth(44_100 * 3, 44_100, 0.8);
    write_wav(&wav, &pcm, 44_100);
    let mp3 = dir.path().join("a.mp3");
    encode(&wav, &mp3, &["-c:a", "libmp3lame", "-b:a", "192k"]);

    let mut library = Library::default();
    library.raw.insert("a".into(), b"this is not audio, but a format we can't read, padded out".iter().cycle().take(32 * 1024).copied().collect());
    library.transcoded.insert(("a".into(), "mp3".into()), std::fs::read(&mp3).unwrap());
    let server = TestServer::start(library, Faults::default());

    let (p, capture) = player(&server.url, 6.0, (8_000, 192_000), OutputPolicy::FollowTrack);
    p.set_queue(items(&[("a", 3000)]), 0, 0, false);
    wait_status(&p, PlayerStatus::Stopped, Duration::from_secs(30));

    let formats: Vec<Option<String>> = server.requests().iter().map(|r| r.format.clone()).collect();
    assert_eq!(formats.first().and_then(|f| f.clone()).as_deref(), Some("raw"), "tries the original first");
    assert!(formats.iter().any(|f| f.as_deref() == Some("mp3")), "falls back to mp3: {formats:?}");

    let mut d = navia_core::audio::Decoder::open(&mp3).unwrap();
    let mut reference = Vec::new();
    let mut block = Vec::new();
    while d.next_block(&mut block).unwrap() {
        reference.extend_from_slice(&block);
    }
    let captured = capture.samples();
    let audio = audio_from_first(&captured);
    assert_contains_exact(audio, &reference[..reference.len().min(audio.len())], "transcoded audio");
    assert!(audio.len() / 2 >= reference.len() / 2 - 1152, "played the whole transcode");
}

#[test]
fn seeking_a_transcode_restarts_it_at_a_time_offset() {
    if !have_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let wav = dir.path().join("a.wav");
    write_wav(&wav, &synth(44_100 * 10, 44_100, 0.6), 44_100);
    let mp3 = dir.path().join("a.mp3");
    encode(&wav, &mp3, &["-c:a", "libmp3lame", "-b:a", "192k"]);
    let mut library = Library::default();
    library.raw.insert("a".into(), vec![0u8; 8192]);
    library.transcoded.insert(("a".into(), "mp3".into()), std::fs::read(&mp3).unwrap());
    let server = TestServer::start(library, Faults::default());

    let (p, _capture) = player(&server.url, 1.0, (8_000, 192_000), OutputPolicy::FollowTrack);
    p.set_queue(items(&[("a", 10_000)]), 0, 0, false);
    wait_status(&p, PlayerStatus::Playing, Duration::from_secs(10));
    p.seek_ms(4000);
    assert!(wait_for(Duration::from_secs(10), || server.requests().iter().any(|r| r.time_offset == Some(4))), "no timeOffset request: {:?}", server.requests());
    assert!(wait_for(Duration::from_secs(5), || p.snapshot().position_ms >= 3500), "position after transcoded seek {}", p.snapshot().position_ms);
    p.stop();
}

#[test]
fn failed_requests_are_retried() {
    let a = synth(44_100 * 2, 44_100, 0.9);
    let mut library = Library::default();
    library.raw.insert("a".into(), wav_bytes(&a, 44_100));
    let server = TestServer::start(library, Faults { fail_first: 3, ..Default::default() });

    let (p, capture) = player(&server.url, 8.0, (8_000, 192_000), OutputPolicy::FollowTrack);
    p.set_queue(items(&[("a", 2000)]), 0, 0, false);
    wait_status(&p, PlayerStatus::Stopped, Duration::from_secs(30));

    let samples = capture.samples();
    let audio = audio_from_first(&samples);
    assert_contains_exact(audio, &to_f32(&a), "after retries");
}

#[test]
fn transport_controls_work() {
    let a = synth(44_100 * 8, 44_100, 0.2);
    let b = synth(44_100 * 8, 44_100, 1.2);
    let c = synth(44_100 * 8, 44_100, 2.2);
    let mut library = Library::default();
    for (id, pcm) in [("a", &a), ("b", &b), ("c", &c)] {
        library.raw.insert(id.into(), wav_bytes(pcm, 44_100));
    }
    let server = TestServer::start(library, Faults::default());
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));

    let (p, _capture) = player(&server.url, 3.0, (8_000, 192_000), OutputPolicy::FollowTrack);
    let sink = events.clone();
    p.add_listener(move |e| {
        if matches!(e, PlayerEvent::TrackChanged { .. } | PlayerEvent::StatusChanged(_) | PlayerEvent::QueueEnded) {
            sink.lock().unwrap().push(format!("{e:?}"));
        }
    });

    p.set_queue(items(&[("a", 8000), ("b", 8000), ("c", 8000)]), 0, 0, false);
    wait_status(&p, PlayerStatus::Playing, Duration::from_secs(10));

    p.pause();
    wait_status(&p, PlayerStatus::Paused, Duration::from_secs(2));
    let position = p.snapshot().position_ms;
    std::thread::sleep(Duration::from_millis(300));
    assert!(p.snapshot().position_ms.abs_diff(position) < 50, "position moved while paused");
    p.play();
    wait_status(&p, PlayerStatus::Playing, Duration::from_secs(2));

    p.next();
    assert!(wait_for(Duration::from_secs(5), || p.snapshot().item.as_ref().is_some_and(|i| i.id == "b")), "next didn't reach b");
    p.append(items(&[("c", 8000)]));
    p.sync();
    assert_eq!(p.snapshot().queue.len(), 4);
    p.remove(3);
    p.jump_to(2);
    assert!(wait_for(Duration::from_secs(5), || p.snapshot().item.as_ref().is_some_and(|i| i.id == "c")), "jump didn't reach c");

    p.previous();
    assert!(wait_for(Duration::from_secs(5), || p.snapshot().item.as_ref().is_some_and(|i| i.id == "b")), "previous didn't reach b");

    p.set_volume(0.25);
    p.set_muted(true);
    p.set_muted(false);
    wait_for(Duration::from_millis(500), || (p.snapshot().volume - 0.25).abs() < 1e-3);
    assert!((p.snapshot().volume - 0.25).abs() < 1e-3);

    p.stop();
    wait_status(&p, PlayerStatus::Stopped, Duration::from_secs(2));
    let log = events.lock().unwrap().clone();
    assert!(log.iter().any(|e| e.contains("TrackChanged { index: 1")), "events: {log:?}");
}

#[test]
fn volume_scales_samples_exactly() {
    let a = synth(44_100 * 2, 44_100, 0.3);
    let mut library = Library::default();
    library.raw.insert("a".into(), wav_bytes(&a, 44_100));
    let server = TestServer::start(library, Faults::default());

    let (p, capture) = player(&server.url, 12.0, (8_000, 192_000), OutputPolicy::FollowTrack);
    p.set_queue(items(&[("a", 2000)]), 0, 0, true);
    wait_status(&p, PlayerStatus::Paused, Duration::from_secs(5));
    p.set_volume(0.5);
    std::thread::sleep(Duration::from_millis(100));
    p.play();
    wait_status(&p, PlayerStatus::Stopped, Duration::from_secs(20));

    let samples = capture.samples();
    let audio = audio_from_first(&samples);
    let reference = to_f32(&a);
    // The first frames ramp, and after that every sample is exactly half.
    let skip = 2 * 2048;
    for (x, y) in audio[skip..].iter().zip(&reference[skip..]) {
        assert!((x - y * 0.5).abs() < 1e-6, "{x} vs {}", y * 0.5);
    }
}

#[test]
fn rate_change_reopens_the_device_between_tracks() {
    let a = synth(44_100 * 2 + 44, 44_100, 0.2);
    let b = synth(48_000 * 2, 48_000, 1.2);
    let mut library = Library::default();
    library.raw.insert("a".into(), wav_bytes(&a, 44_100));
    library.raw.insert("b".into(), wav_bytes(&b, 48_000));
    let server = TestServer::start(library, Faults::default());

    let backend = CaptureBackend { speed: 4.0, rates: (8_000, 192_000), open_delay: Duration::from_millis(30), ..Default::default() };
    let capture = backend.capture.clone();
    let p = Player::with_backend(Arc::new(ServerResolver::new(client(&server.url)).unwrap()), Box::new(backend), PlayerConfig::default()).unwrap();
    p.set_queue(items(&[("a", 2001), ("b", 2000)]), 0, 0, false);
    wait_status(&p, PlayerStatus::Stopped, Duration::from_secs(20));

    let opens = capture.opens();
    assert_eq!(opens.iter().map(|o| o.0).collect::<Vec<_>>(), vec![44_100, 48_000], "device reopened at the new rate");
    let captured = capture.samples();
    let a_f = to_f32(&a);
    let b_f = to_f32(&b);
    let audio = audio_from_first(&captured);
    assert_contains_exact(audio, &a_f, "first track before the reopen");
    // After the reopen, the second track appears intact.
    let b_start = captured.chunks_exact(2).position(|f| f[0].to_bits() == b_f[0].to_bits() && f[1].to_bits() == b_f[1].to_bits()).expect("second track present");
    assert_contains_exact(&captured[b_start * 2..], &b_f, "second track after the reopen");
    let gap_frames = b_start - (captured.chunks_exact(2).position(|f| f[0] != 0.0 || f[1] != 0.0).unwrap() + a_f.len() / 2);
    println!("reopen gap: {gap_frames} frames, opens {opens:?}, underruns {}", p.snapshot().underrun_frames);
    assert_eq!(p.snapshot().underrun_frames, 0);
}

#[test]
fn crossfade_through_the_resampler_matches_an_independent_mix() {
    use navia_core::audio::engine::crossfade_gains;
    use navia_core::audio::resample::StreamResampler;

    let a = synth(44_100 * 3, 44_100, 0.2);
    let b = synth(48_000 * 2, 48_000, 1.2);
    let mut library = Library::default();
    library.raw.insert("a".into(), wav_bytes(&a, 44_100));
    library.raw.insert("b".into(), wav_bytes(&b, 48_000));
    let server = TestServer::start(library, Faults::default());

    let backend = CaptureBackend { speed: 2.0, rates: (48_000, 48_000), ..Default::default() };
    let capture = backend.capture.clone();
    let p = Player::with_backend(Arc::new(ServerResolver::new(client(&server.url)).unwrap()), Box::new(backend), PlayerConfig { policy: OutputPolicy::Fixed(48_000), ..Default::default() }).unwrap();
    p.set_crossfade_ms(300);
    p.set_queue(items(&[("a", 3000), ("b", 2000)]), 0, 0, false);
    wait_status(&p, PlayerStatus::Stopped, Duration::from_secs(20));

    // Compute the expected output independently by resampling A and mixing its tail with B's head.
    let mut r = StreamResampler::new(44_100, 48_000).unwrap();
    let mut a_out = Vec::new();
    r.push(&to_f32(&a), &mut a_out);
    r.finish(&mut a_out);
    let b_out = to_f32(&b);
    let overlap = 14_400usize;
    let mut expected = a_out[..a_out.len() - overlap * 2].to_vec();
    let tail = &a_out[a_out.len() - overlap * 2..];
    for k in 0..overlap {
        let t = (k as f32 + 0.5) / overlap as f32;
        let (o, i) = crossfade_gains(t);
        expected.push(tail[k * 2] * o + b_out[k * 2] * i);
        expected.push(tail[k * 2 + 1] * o + b_out[k * 2 + 1] * i);
    }
    expected.extend_from_slice(&b_out[overlap * 2..]);

    let captured = capture.samples();
    let audio = audio_from_first(&captured);
    assert!(audio.len() >= expected.len(), "captured {} expected {}", audio.len(), expected.len());
    let worst = audio.iter().zip(&expected).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max);
    println!("crossfade through resampler: {} frames, worst difference {worst:e}", expected.len() / 2);
    assert!(worst < 1e-4, "worst difference {worst}");
    assert_eq!(p.snapshot().underrun_frames, 0, "{:?}", p.snapshot());
}

/// a stream whose reader took the last byte and let go before the worker noticed the file was whole
/// still hands every byte over: the file is all on disk, so it's kept.
#[test]
fn a_whole_stream_dropped_right_after_its_last_byte_is_still_kept() {
    use navia_core::audio::stream::{CompleteSink, HttpFetcher, HttpStream, StreamConfig};
    use navia_core::client::StreamOptions;
    use navia_core::songcache::Spool;
    use std::io::Read;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Count(AtomicU64);
    impl CompleteSink for Count {
        fn complete(&self, spool: &dyn Spool, len: u64, _: Option<&str>) {
            let mut last = [0u8; 1];
            spool.read_at(len - 1, &mut last).unwrap();
            self.0.store(len, Ordering::Release);
        }
    }

    let data: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    let mut files = Library::default();
    files.raw.insert("s1".into(), data.clone());
    let server = TestServer::start(files, Faults::default());
    let client = Client::new(ClientConfig::new(server.url.clone(), Credentials::new("t".to_string(), "t".to_string()))).unwrap();
    let fetcher = HttpFetcher::new().unwrap();
    let url = client.stream_url("s1", &StreamOptions { format: Some("raw".into()), ..Default::default() }).unwrap();
    // busy threads preempt the stream's worker between its last write and its last look
    let busy = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let burners: Vec<_> = (0..std::thread::available_parallelism().map_or(4, |n| n.get()))
        .map(|_| {
            let busy = busy.clone();
            std::thread::spawn(move || {
                while busy.load(Ordering::Relaxed) {
                    std::hint::spin_loop();
                }
            })
        })
        .collect();
    for round in 0..300 {
        let stream = HttpStream::open(&fetcher, url.clone(), StreamConfig::default()).unwrap();
        let sink = Arc::new(Count(AtomicU64::new(0)));
        stream.keep_when_complete(sink.clone());
        let mut got = Vec::new();
        stream.reader().read_to_end(&mut got).unwrap();
        assert_eq!(got.len(), data.len());
        drop(stream);
        let start = Instant::now();
        while sink.0.load(Ordering::Acquire) == 0 && start.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(1));
        }
        let kept = sink.0.load(Ordering::Acquire);
        if kept != data.len() as u64 {
            busy.store(false, Ordering::Relaxed);
            panic!("round {round}: a whole file wasn't kept ({kept})");
        }
    }
    busy.store(false, Ordering::Relaxed);
    for b in burners {
        b.join().unwrap();
    }
}
