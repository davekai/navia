//! Sample-accurate gapless checks on generated sources, so they need no music library.
//! Tests that need encoding use ffmpeg and print a skip message without it. Nothing produces sound.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use navia_core::audio::capture::{self, Capture};
use navia_core::audio::engine::{AlbumSpot, EngineConfig, EngineEvent, OutputMode, crossfade_gains, spawn, spawn_with_albums};
use navia_core::audio::processing::{ProcessingChain, Processor};
use navia_core::audio::{Decoder, GaplessSource};

/// Non-repeating test tones, as 16-bit stereo. Each frame is unique, so any lost, added or
/// shifted frame shows up.
fn synth(frames: usize, rate: u32, seed: f64) -> Vec<i16> {
    let mut out = Vec::with_capacity(frames * 2);
    for n in 0..frames {
        let t = n as f64 / rate as f64;
        let f = 220.0 + 180.0 * (t * 0.37 + seed).sin();
        let l = 0.35 * (2.0 * std::f64::consts::PI * f * t).sin() + 0.1 * (2.0 * std::f64::consts::PI * 1733.0 * t + seed).sin();
        let r = 0.30 * (2.0 * std::f64::consts::PI * (f * 1.5) * t + 0.7).sin() + 0.1 * (2.0 * std::f64::consts::PI * 997.0 * t).sin();
        out.push((l * 32767.0) as i16);
        out.push((r * 32767.0) as i16);
    }
    out
}

fn write_wav(path: &Path, pcm: &[i16], rate: u32) {
    let mut f = Vec::with_capacity(44 + pcm.len() * 2);
    let data_len = (pcm.len() * 2) as u32;
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
    for s in pcm {
        f.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, f).unwrap();
}

fn to_f32(pcm: &[i16]) -> Vec<f32> {
    pcm.iter().map(|s| *s as f32 / 32768.0).collect()
}

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg").arg("-version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

fn ffmpeg(args: &[&str]) {
    let status = Command::new("ffmpeg").args(["-v", "error", "-nostdin", "-y"]).args(args).status().expect("ffmpeg");
    assert!(status.success(), "ffmpeg failed: {args:?}");
}

fn ffmpeg_decode(path: &Path) -> Vec<f32> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-map", "0:a:0", "-ac", "2", "-f", "f32le", "-acodec", "pcm_f32le", "-"])
        .output()
        .expect("ffmpeg");
    out.stdout.chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect()
}

fn config() -> EngineConfig {
    EngineConfig::new(OutputMode::FollowTrack)
}

fn play(queue: Vec<PathBuf>, cfg: EngineConfig) -> Capture {
    capture::run(spawn(queue, cfg))
}

fn track_starts(c: &Capture) -> Vec<(usize, u64)> {
    c.events.iter().filter_map(|e| if let EngineEvent::TrackStart { track, output_frame, .. } = e { Some((*track, *output_frame)) } else { None }).collect()
}

fn no_errors(c: &Capture) {
    let errs: Vec<_> = c.events.iter().filter(|e| matches!(e, EngineEvent::Error { .. })).collect();
    assert!(errs.is_empty(), "{errs:?}");
}

#[test]
fn wav_queue_is_bit_exact_with_exact_join_positions() {
    let dir = tempfile::tempdir().unwrap();
    let a = synth(44_100 * 3 + 137, 44_100, 0.2);
    let b = synth(44_100 * 2 + 971, 44_100, 1.7);
    let (pa, pb) = (dir.path().join("a.wav"), dir.path().join("b.wav"));
    write_wav(&pa, &a, 44_100);
    write_wav(&pb, &b, 44_100);

    let c = play(vec![pa, pb], config());
    no_errors(&c);
    let expected: Vec<f32> = to_f32(&a).into_iter().chain(to_f32(&b)).collect();
    assert_eq!(c.samples.len(), expected.len(), "frame count");
    assert!(c.samples.iter().zip(&expected).all(|(x, y)| x.to_bits() == y.to_bits()), "samples differ");
    assert_eq!(track_starts(&c), vec![(0, 0), (1, (a.len() / 2) as u64)]);
}

/// A stage used only to prove the chain runs and keeps bit-exact bypass when empty.
struct Halve {
    configured_rate: std::sync::Arc<std::sync::Mutex<Option<u32>>>,
}
impl Processor for Halve {
    fn configure(&mut self, sample_rate: u32, _channels: usize) {
        *self.configured_rate.lock().unwrap() = Some(sample_rate);
    }
    fn process(&mut self, interleaved: &mut [f32]) {
        interleaved.iter_mut().for_each(|s| *s *= 0.5);
    }
    fn reset(&mut self) {}
}

#[test]
fn processing_chain_is_applied_and_bypass_stays_bit_exact() {
    let dir = tempfile::tempdir().unwrap();
    let pcm = synth(20_000, 48_000, 0.5);
    let path = dir.path().join("a.wav");
    write_wav(&path, &pcm, 48_000);
    let expected = to_f32(&pcm);

    let empty = ProcessingChain::new();
    assert!(empty.is_bypassed());
    let bypass = play(vec![path.clone()], config());
    assert!(bypass.samples.iter().zip(&expected).all(|(x, y)| x.to_bits() == y.to_bits()));

    let rate = std::sync::Arc::new(std::sync::Mutex::new(None));
    let mut chain = ProcessingChain::new();
    chain.push(Box::new(Halve { configured_rate: rate.clone() }));
    let mut cfg = config();
    cfg.processing = chain;
    let processed = play(vec![path], cfg);
    assert_eq!(processed.samples.len(), expected.len());
    assert!(processed.samples.iter().zip(&expected).all(|(x, y)| (x - y * 0.5).abs() < 1e-7));
    assert_eq!(*rate.lock().unwrap(), Some(48_000));
}

#[test]
fn follow_track_reopens_at_rate_changes_and_stays_exact() {
    let dir = tempfile::tempdir().unwrap();
    let a = synth(22_050, 44_100, 0.1);
    let b = synth(24_000, 48_000, 0.9);
    let (pa, pb) = (dir.path().join("a.wav"), dir.path().join("b.wav"));
    write_wav(&pa, &a, 44_100);
    write_wav(&pb, &b, 48_000);

    let c = play(vec![pa, pb], config());
    no_errors(&c);
    let expected: Vec<f32> = to_f32(&a).into_iter().chain(to_f32(&b)).collect();
    assert_eq!(c.samples.len(), expected.len());
    assert!(c.samples.iter().zip(&expected).all(|(x, y)| x.to_bits() == y.to_bits()));
    let starts: Vec<(u32, bool)> = c.events.iter().filter_map(|e| if let EngineEvent::TrackStart { output_rate, reopen, .. } = e { Some((*output_rate, *reopen)) } else { None }).collect();
    assert_eq!(starts, vec![(44_100, false), (48_000, true)]);
}

#[test]
fn fixed_rate_resampling_delivers_the_exact_expected_length() {
    let dir = tempfile::tempdir().unwrap();
    let a = synth(44_100, 44_100, 0.1);
    let b = synth(30_000, 44_100, 0.3);
    let (pa, pb) = (dir.path().join("a.wav"), dir.path().join("b.wav"));
    write_wav(&pa, &a, 44_100);
    write_wav(&pb, &b, 44_100);
    let c = play(vec![pa, pb], EngineConfig::new(OutputMode::FixedRate(48_000)));
    no_errors(&c);
    let total = (a.len() / 2 + b.len() / 2) as u64;
    assert_eq!(c.samples.len() as u64 / 2, (total * 48_000).div_ceil(44_100), "one continuous resampled run");
    assert!(c.events.iter().all(|e| !matches!(e, EngineEvent::TrackStart { reopen: true, .. })), "one device rate, no reopen");
}

#[test]
fn crossfade_overlap_matches_an_independent_mix() {
    let dir = tempfile::tempdir().unwrap();
    let a = synth(48_000, 48_000, 0.2);
    let b = synth(48_000, 48_000, 1.1);
    let (pa, pb) = (dir.path().join("a.wav"), dir.path().join("b.wav"));
    write_wav(&pa, &a, 48_000);
    write_wav(&pb, &b, 48_000);
    let xf = 12_000usize;
    let mut cfg = config();
    cfg.crossfade_frames = xf;
    let c = play(vec![pa, pb], cfg);
    no_errors(&c);
    assert_eq!(c.samples.len() / 2, 96_000 - xf);
    let (fa, fb) = (to_f32(&a), to_f32(&b));
    let start = 48_000 - xf;
    let mut max_err = 0f32;
    for k in 0..xf {
        let (gout, gin) = crossfade_gains((k as f32 + 0.5) / xf as f32);
        for ch in 0..2 {
            let want = fa[(start + k) * 2 + ch] * gout + fb[k * 2 + ch] * gin;
            max_err = max_err.max((c.samples[(start + k) * 2 + ch] - want).abs());
        }
    }
    assert!(max_err < 1e-6, "max error {max_err}");
}

/// FLAC joins must be bit-exact and match STREAMINFO lengths.
#[test]
fn flac_album_is_bit_exact_through_joins() {
    if !have_ffmpeg() {
        eprintln!("skipped: ffmpeg not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let parts: Vec<Vec<i16>> = (0..3).map(|i| synth(30_000 + i * 11, 44_100, i as f64)).collect();
    let mut queue = Vec::new();
    for (i, pcm) in parts.iter().enumerate() {
        let wav = dir.path().join(format!("{i}.wav"));
        let flac = dir.path().join(format!("{i}.flac"));
        write_wav(&wav, pcm, 44_100);
        ffmpeg(&["-i", wav.to_str().unwrap(), "-c:a", "flac", flac.to_str().unwrap()]);
        queue.push(flac);
    }
    assert_eq!(Decoder::open(&queue[0]).unwrap().gapless, GaplessSource::Lossless);
    let c = play(queue, config());
    no_errors(&c);
    let expected: Vec<f32> = parts.iter().flat_map(|p| to_f32(p)).collect();
    assert_eq!(c.samples.len(), expected.len());
    assert!(c.samples.iter().zip(&expected).all(|(x, y)| x.to_bits() == y.to_bits()));
}

#[test]
fn opus_decodes_through_libopus_with_exact_length_and_pre_skip_removed() {
    if !have_ffmpeg() {
        eprintln!("skipped: ffmpeg not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let pcm = synth(48_000 * 4 + 333, 48_000, 0.4);
    let wav = dir.path().join("a.wav");
    let ogg = dir.path().join("a.opus");
    write_wav(&wav, &pcm, 48_000);
    ffmpeg(&["-i", wav.to_str().unwrap(), "-c:a", "libopus", "-b:a", "128k", ogg.to_str().unwrap()]);

    let d = Decoder::open(&ogg).unwrap();
    assert_eq!(d.codec, "opus");
    assert_eq!(d.gapless, GaplessSource::OpusPreSkip);
    assert_eq!(d.sample_rate, 48_000);
    assert!(d.delay.unwrap_or(0) > 0, "pre-skip reported");

    let c = play(vec![ogg.clone()], config());
    no_errors(&c);
    let reference = ffmpeg_decode(&ogg);
    assert_eq!(c.samples.len(), reference.len(), "same frame count as ffmpeg's decode");
    // Pre-skip removed, so the audio lines up with the source at offset zero.
    let snr = snr_db(&c.samples, &to_f32(&pcm), 48_000 * 2);
    assert!(snr > 15.0, "alignment with source: {snr:.1} dB");
}

#[test]
fn opus_queue_has_the_sum_of_lengths() {
    if !have_ffmpeg() {
        eprintln!("skipped: ffmpeg not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mut queue = Vec::new();
    let mut expected = 0usize;
    for i in 0..2u32 {
        let pcm = synth(48_000 * 2 + i as usize * 77, 48_000, i as f64);
        let wav = dir.path().join(format!("{i}.wav"));
        let ogg = dir.path().join(format!("{i}.opus"));
        write_wav(&wav, &pcm, 48_000);
        ffmpeg(&["-i", wav.to_str().unwrap(), "-c:a", "libopus", "-b:a", "128k", ogg.to_str().unwrap()]);
        expected += ffmpeg_decode(&ogg).len() / 2;
        queue.push(ogg);
    }
    let c = play(queue, config());
    no_errors(&c);
    assert_eq!(c.samples.len() / 2, expected);
    assert_eq!(track_starts(&c).len(), 2);
}

fn snr_db(a: &[f32], b: &[f32], window_frames: usize) -> f64 {
    let frames = window_frames.min(a.len().min(b.len()) / 2);
    let (mut sig, mut err) = (0f64, 0f64);
    for i in 0..frames * 2 {
        let (x, y) = (a[i] as f64, b[i] as f64);
        sig += y * y;
        err += (x - y) * (x - y);
    }
    10.0 * (sig / err.max(1e-30)).log10()
}

/// Playing two encoded parts should reproduce the continuous original without a discontinuity.
/// Only as exact as the encoder allows.
fn lossy_join(codec_args: &[&str], ext: &str, want_source: GaplessSource, tolerance: i64) {
    let dir = tempfile::tempdir().unwrap();
    let whole = synth(44_100 * 10, 44_100, 0.9);
    let wav = dir.path().join("whole.wav");
    write_wav(&wav, &whole, 44_100);
    let cut = 44_100 * 6 + 12_345;
    let parts = [dir.path().join(format!("p0.{ext}")), dir.path().join(format!("p1.{ext}"))];
    // asetpts keeps the second part's timestamps at zero, or the muxer writes a shifted edit list.
    let filters = [format!("atrim=end_sample={cut},asetpts=N/SR/TB"), format!("atrim=start_sample={cut},asetpts=N/SR/TB")];
    for (part, filter) in parts.iter().zip(&filters) {
        let mut args = vec!["-i", wav.to_str().unwrap(), "-af", filter];
        args.extend_from_slice(codec_args);
        args.push(part.to_str().unwrap());
        ffmpeg(&args);
    }
    let d = Decoder::open(&parts[0]).unwrap();
    assert_eq!(d.gapless, want_source, "codec {} gapless source", d.codec);

    let c = play(parts.to_vec(), config());
    no_errors(&c);
    let (got, want) = (c.samples.len() as i64 / 2, whole.len() as i64 / 2);
    assert!((got - want).abs() <= tolerance, "length {got} vs {want}");

    let join = track_starts(&c)[1].1 as usize;
    let span = 4096;
    let (p0, p1) = (ffmpeg_decode(&parts[0]), ffmpeg_decode(&parts[1]));
    let rms = |ours: &[f32], theirs: &[f32]| (ours.iter().zip(theirs).map(|(a, b)| (*a as f64 - *b as f64).powi(2)).sum::<f64>() / ours.len() as f64).sqrt();
    let tail = rms(&c.samples[(join - span) * 2..join * 2], &p0[p0.len() - span * 2..]);
    let head = rms(&c.samples[join * 2..(join + span) * 2], &p1[..span * 2]);
    assert!(tail < 1e-4 && head < 1e-4, "edges against the reference decode: end of first {tail:.6}, start of second {head:.6}");

    // the reference decode could share a misread edit list, so the join must also line up with the original
    let original = to_f32(&whole);
    let best_lag = |at: usize| {
        let err = |lag: i64| (0..span).map(|i| (c.samples[(at + i) * 2] as f64 - original[((at + i) as i64 + lag) as usize * 2] as f64).powi(2)).sum::<f64>();
        (-2048i64..=2048).min_by(|a, b| err(*a).total_cmp(&err(*b))).unwrap()
    };
    assert_eq!((best_lag(join - span), best_lag(join)), (0, 0), "lag against the original before and after the join");
}

#[test]
fn aac_edit_list_join_stays_continuous() {
    if !have_ffmpeg() {
        eprintln!("skipped: ffmpeg not installed");
        return;
    }
    lossy_join(&["-c:a", "aac", "-b:a", "192k", "-f", "ipod"], "m4a", GaplessSource::EditList, 2048);
}

#[test]
fn mp3_with_lame_tag_keeps_length_and_join() {
    if !have_ffmpeg() || !ffmpeg_has_encoder("libmp3lame") {
        eprintln!("skipped: ffmpeg with libmp3lame not installed");
        return;
    }
    lossy_join(&["-c:a", "libmp3lame", "-q:a", "2"], "mp3", GaplessSource::CodecDelay, 1152);
}

fn ffmpeg_has_encoder(name: &str) -> bool {
    Command::new("ffmpeg").args(["-hide_banner", "-encoders"]).output().is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(name))
}

fn spot(album: &str, disc: Option<i32>, track: Option<i32>) -> Option<AlbumSpot> {
    Some(AlbumSpot { album_id: album.to_string(), disc, track })
}

/// Three generated 1 s tracks at 48 kHz played with a 0.25 s crossfade and Smart on.
/// Returns the capture and the tracks' samples.
fn smart_run(spots: Vec<Option<AlbumSpot>>, smart: bool) -> (Capture, Vec<Vec<f32>>) {
    let dir = tempfile::tempdir().unwrap();
    let mut queue = Vec::new();
    let mut pcm = Vec::new();
    for (i, s) in spots.into_iter().enumerate() {
        let p = synth(48_000 + i * 7, 48_000, 0.3 + i as f64);
        let path = dir.path().join(format!("{i}.wav"));
        write_wav(&path, &p, 48_000);
        pcm.push(to_f32(&p));
        queue.push((path, s));
    }
    let mut cfg = config();
    cfg.crossfade_frames = SMART_XF;
    cfg.smart_joins = smart;
    let c = capture::run(spawn_with_albums(queue, cfg));
    no_errors(&c);
    (c, pcm)
}

const SMART_XF: usize = 12_000;

fn frames(pcm: &[Vec<f32>]) -> usize {
    pcm.iter().map(|p| p.len() / 2).sum()
}

/// Bit-exact equality of a stretch of the capture with a track's samples.
fn exact_at(c: &Capture, at_frame: usize, track: &[f32]) -> bool {
    c.samples[at_frame * 2..at_frame * 2 + track.len()].iter().zip(track).all(|(x, y)| x.to_bits() == y.to_bits())
}

#[test]
fn smart_crossfade_plays_an_album_in_order_gapless() {
    // Track 1 and 2 of disc 1, then the first track of disc 2.
    let (c, pcm) = smart_run(vec![spot("al", Some(1), Some(1)), spot("al", Some(1), Some(2)), spot("al", Some(2), Some(1))], true);
    let expected: Vec<f32> = pcm.concat();
    assert_eq!(c.samples.len(), expected.len(), "no overlap anywhere");
    assert!(c.samples.iter().zip(&expected).all(|(x, y)| x.to_bits() == y.to_bits()), "bit-exact through both joins");
}

#[test]
fn smart_crossfade_fades_when_shuffle_breaks_album_order() {
    // Track 3 then track 1 of the same album, then track 2, so neither join continues in order.
    let (c, pcm) = smart_run(vec![spot("al", Some(1), Some(3)), spot("al", Some(1), Some(1)), spot("al", Some(1), Some(3))], true);
    assert_eq!(c.samples.len() / 2, frames(&pcm) - 2 * SMART_XF, "two crossfades");
}

#[test]
fn smart_crossfade_fades_around_a_queued_song_between_album_tracks() {
    // Album track 1, a queued song from another album, then album track 2.
    let (c, pcm) = smart_run(vec![spot("al", Some(1), Some(1)), spot("other", Some(1), Some(5)), spot("al", Some(1), Some(2))], true);
    assert_eq!(c.samples.len() / 2, frames(&pcm) - 2 * SMART_XF, "both joins fade");
}

#[test]
fn smart_crossfade_fades_when_track_numbers_are_missing() {
    let (c, pcm) = smart_run(vec![spot("al", Some(1), None), spot("al", Some(1), None), None], true);
    assert_eq!(c.samples.len() / 2, frames(&pcm) - 2 * SMART_XF);
}

#[test]
fn smart_crossfade_mixes_gapless_and_faded_joins() {
    // Album tracks 1 and 2 join gapless, then another album's song fades in.
    let (c, pcm) = smart_run(vec![spot("al", None, Some(1)), spot("al", None, Some(2)), spot("b", None, Some(1))], true);
    assert_eq!(c.samples.len() / 2, frames(&pcm) - SMART_XF, "one crossfade");
    let first = pcm[0].len() / 2;
    // Track 1 and track 2 up to where the fade into the third begins are untouched.
    assert!(exact_at(&c, 0, &pcm[0]));
    assert!(exact_at(&c, first, &pcm[1][..pcm[1].len() - SMART_XF * 2]));
}

#[test]
fn plain_crossfade_still_fades_an_album_in_order() {
    let (c, pcm) = smart_run(vec![spot("al", Some(1), Some(1)), spot("al", Some(1), Some(2)), spot("al", Some(1), Some(3))], false);
    assert_eq!(c.samples.len() / 2, frames(&pcm) - 2 * SMART_XF);
}
