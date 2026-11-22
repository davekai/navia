//! edge analysis for smart transitions: the first and last seconds of a song, measured from a
//! loudness envelope of short RMS windows, done once per song and kept in the library.

use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, OnceLock};
use std::thread;

use serde::{Deserialize, Serialize};

use crate::audio::decode::Decoder;
use crate::audio::engine::{OUTPUT_CHANNELS, TrackOpener};
use crate::audio::transition::{SongTraits, TraitsSlot};
use crate::error::Result;
use crate::library::Library;

/// how much of each end of a song is analyzed.
pub const EDGE_WINDOW_MS: u64 = 30_000;
/// bump when a measurement changes meaning, so kept results are measured again.
pub const EDGES_VERSION: i64 = 5;
/// below this a window counts as silence, in dBFS of mean square.
pub const SILENCE_DBFS: f32 = -55.0;
/// the outro starts where the smoothed level last sits within this of the song's body level.
pub const OUTRO_DROP_DB: f32 = 3.0;
/// the outro has decayed once the smoothed level is this far under the body level.
pub const DECAYED_DB: f32 = 15.0;
/// an edge that goes between silence and the body level in less than this is a hard edge.
pub const HARD_EDGE_MS: u32 = 300;
/// the level curves have one step this long.
pub const CURVE_STEP_MS: u32 = 250;

const HOP_MS: u64 = 5;
const SMOOTH_MS: u64 = 500;
/// the body level is this percentile of the smoothed level over the non-silent windows.
const BODY_PERCENTILE: f32 = 0.9;
/// kick drums sit under this; a beat's accent is the kick band's rise on it.
const KICK_LOWPASS_HZ: f64 = 150.0;
/// a beat's accent compares the kick band just after it with just before it.
const ACCENT_AFTER_MS: u64 = 50;
const ACCENT_BEFORE_MS: u64 = 150;
/// an aubio onset this close to a beat adds its strength (0 to 1) times this to the accent.
const ACCENT_ONSET_MS: f32 = 30.0;
const ACCENT_ONSET_DB: f32 = 6.0;
/// a beat grid needs at least this many tracked beats.
const GRID_MIN_BEATS: usize = 8;
/// a tail decoded from an estimated length starts this much early, in case the estimate is short.
const TAIL_MARGIN_MS: u64 = 2_000;
/// onsets kept per edge for the own estimate, the strongest first.
pub const ONSETS_KEPT: usize = 48;
/// tempos the own estimator looks for, BPM. double and half time match in transitions anyway.
pub const TEMPO_MIN_BPM: f32 = 60.0;
pub const TEMPO_MAX_BPM: f32 = 200.0;
/// onset strength is the rise over this many windows.
const ONSET_LAG: usize = 2;
/// two onsets are at least this far apart.
const ONSET_SPACING_MS: u64 = 60;
/// an onset stands this many standard deviations over the mean rise.
const ONSET_THRESHOLD_SD: f32 = 1.5;
/// the own estimator needs at least this many of its longest beats.
const TEMPO_MIN_BEATS: usize = 4;
/// beats compared per candidate tempo, so a period whose multiples also repeat wins.
const TEMPO_COMB: usize = 4;
/// onset strength is summed over this many windows before the tempo search.
const TEMPO_SMOOTH_HOPS: usize = 4;

/// a tracked beat: when it falls, in ms (from the start for a head, before the end for a tail),
/// and its accent, dB: how much the kick band rises on it, more where aubio also hears an onset.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Beat(pub u32, pub f32);

impl Beat {
    pub fn ms(self) -> u32 {
        self.0
    }

    pub fn accent(self) -> f32 {
        self.1
    }
}

/// a rise in the envelope: when it happens, in ms (from the start for a head, before the end for
/// a tail), and its strength relative to the edge's strongest, 0 to 1.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Onset(pub u32, pub f32);

impl Onset {
    pub fn ms(self) -> u32 {
        self.0
    }

    pub fn strength(self) -> f32 {
        self.1
    }
}

/// an edge's tempo and the estimator's confidence in it: from aubio, BPM fitted to its tracked
/// beats with aubio's confidence; from the own estimator, the mean autocorrelation it chose by.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tempo {
    pub bpm: f32,
    pub confidence: f32,
}

/// how a song begins. times are ms from the start.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Head {
    /// where the leading silence ends.
    pub intro_ms: u32,
    /// where the level first reaches the body level.
    pub body_ms: u32,
    /// body level of the head, dBFS.
    pub level_db: f32,
    /// starts at full level straight out of silence.
    pub hard: bool,
    pub beats: Vec<Beat>,
    pub tempo: Option<Tempo>,
    /// the own estimator's onsets and tempo, a cross-check on aubio's.
    pub onsets: Vec<Onset>,
    pub own_tempo: Option<Tempo>,
    /// level per [`CURVE_STEP_MS`] step from the start. full band and kick band, tenths of a dB.
    pub curve: Vec<[i16; 2]>,
}

/// how a song ends. times are ms before the end.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tail {
    /// length of the trailing silence.
    pub silence_ms: u32,
    /// where the outro starts fading.
    pub outro_ms: u32,
    /// where the outro has decayed.
    pub decayed_ms: u32,
    /// body level of the tail, dBFS.
    pub level_db: f32,
    /// stops from full level (a cold end).
    pub hard: bool,
    pub beats: Vec<Beat>,
    pub tempo: Option<Tempo>,
    /// the own estimator's onsets and tempo, a cross-check on aubio's.
    pub onsets: Vec<Onset>,
    pub own_tempo: Option<Tempo>,
    /// level per [`CURVE_STEP_MS`] step, last step first: step `i` covers `i` to `i + 1` steps
    /// before the end. full band and kick band, tenths of a dB.
    pub curve: Vec<[i16; 2]>,
}

/// mean levels (full band, kick band) of `curve` over steps covering `from_ms..to_ms`, counted
/// the curve's own way (from the start for a head, back from the end for a tail).
pub fn mean_level(curve: &[[i16; 2]], from_ms: f64, to_ms: f64) -> Option<(f32, f32)> {
    let step = f64::from(CURVE_STEP_MS);
    let (lo, hi) = ((from_ms.min(to_ms) / step).floor().max(0.0) as usize, ((from_ms.max(to_ms) / step).ceil().max(0.0) as usize).min(curve.len()));
    if hi <= lo {
        return None;
    }
    let n = (hi - lo) as f32;
    let (full, kick) = curve[lo..hi].iter().fold((0.0f32, 0.0f32), |(f, k), c| (f + f32::from(c[0]), k + f32::from(c[1])));
    Some((full / n / 10.0, kick / n / 10.0))
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Edges {
    pub head: Head,
    pub tail: Tail,
}

/// a regular beat grid fitted to beat times (ms, increasing): (time of beat 0, period), by least
/// squares over the beats numbered by the median spacing. `None` with too few beats.
pub fn fit_grid(times: &[f64]) -> Option<(f64, f64)> {
    if times.len() < GRID_MIN_BEATS {
        return None;
    }
    let mut gaps: Vec<f64> = times.windows(2).map(|w| w[1] - w[0]).filter(|g| *g > 0.0).collect();
    gaps.sort_by(f64::total_cmp);
    let rough = *gaps.get(gaps.len() / 2)?;
    let points: Vec<(f64, f64)> = times.iter().map(|t| (((t - times[0]) / rough).round(), *t)).collect();
    let n = points.len() as f64;
    let (sk, st) = points.iter().fold((0.0, 0.0), |(a, b), (k, t)| (a + k, b + t));
    let (mk, mt) = (sk / n, st / n);
    let (num, den) = points.iter().fold((0.0, 0.0), |(a, b), (k, t)| (a + (k - mk) * (t - mt), b + (k - mk) * (k - mk)));
    if den <= 0.0 {
        return None;
    }
    let period = num / den;
    (period > 0.0).then_some((mt - period * mk, period))
}

fn to_db(power: f64) -> f32 {
    (10.0 * power.max(1e-12).log10()) as f32
}

/// short-window levels of a mono signal: full band, the kick band, and a smoothed full band.
struct Envelope {
    hop: usize,
    rate: u32,
    frames: usize,
    db: Vec<f32>,
    low_db: Vec<f32>,
    smooth_db: Vec<f32>,
}

impl Envelope {
    fn new(mono: &[f32], rate: u32) -> Self {
        let hop = ((u64::from(rate) * HOP_MS / 1000) as usize).max(1);
        let k = 1.0 - (-std::f64::consts::TAU * KICK_LOWPASS_HZ / f64::from(rate)).exp();
        let mut low = 0.0f64;
        let mut power = Vec::with_capacity(mono.len() / hop + 1);
        let mut low_db = Vec::with_capacity(mono.len() / hop + 1);
        for chunk in mono.chunks(hop) {
            let (mut sum, mut low_sum) = (0.0f64, 0.0f64);
            for &x in chunk {
                let x = f64::from(x);
                low += k * (x - low);
                sum += x * x;
                low_sum += low * low;
            }
            power.push(sum / chunk.len() as f64);
            low_db.push(to_db(low_sum / chunk.len() as f64));
        }
        let radius = (SMOOTH_MS / HOP_MS / 2) as usize;
        let mut prefix = Vec::with_capacity(power.len() + 1);
        prefix.push(0.0f64);
        for p in &power {
            prefix.push(prefix.last().copied().unwrap_or(0.0) + p);
        }
        let n = power.len();
        let smooth_db = (0..n)
            .map(|i| {
                let (lo, hi) = (i.saturating_sub(radius), (i + radius + 1).min(n));
                to_db((prefix[hi] - prefix[lo]) / (hi - lo) as f64)
            })
            .collect();
        Self { hop, rate, frames: mono.len(), db: power.into_iter().map(to_db).collect(), low_db, smooth_db }
    }

    fn len(&self) -> usize {
        self.db.len()
    }

    fn ms_from_start(&self, i: usize) -> u32 {
        ((i * self.hop).min(self.frames) as u64 * 1000 / u64::from(self.rate)) as u32
    }

    fn ms_before_end(&self, i: usize) -> u32 {
        (self.frames.saturating_sub(i * self.hop) as u64 * 1000 / u64::from(self.rate)) as u32
    }

    fn window_at(&self, ms: f64) -> usize {
        ((ms * f64::from(self.rate) / 1000.0) as usize / self.hop).min(self.len().saturating_sub(1))
    }

    fn step_mean(&self, lo: usize, hi: usize) -> [i16; 2] {
        let mean_db = |v: &[f32]| to_db(v.iter().map(|d| 10f64.powf(f64::from(*d) / 10.0)).sum::<f64>() / v.len().max(1) as f64);
        let tenths = |d: f32| (d.max(SILENCE_DBFS * 2.0) * 10.0).round() as i16;
        [tenths(mean_db(&self.db[lo..hi])), tenths(mean_db(&self.low_db[lo..hi]))]
    }

    /// mean levels per [`CURVE_STEP_MS`] step from the start.
    fn curve_from_start(&self) -> Vec<[i16; 2]> {
        let per = ((u64::from(CURVE_STEP_MS) / HOP_MS) as usize).max(1);
        let n = self.len();
        (0..n.div_ceil(per)).map(|i| self.step_mean(i * per, ((i + 1) * per).min(n))).collect()
    }

    /// mean levels per [`CURVE_STEP_MS`] step counted back from the end.
    fn curve_from_end(&self) -> Vec<[i16; 2]> {
        let per = ((u64::from(CURVE_STEP_MS) / HOP_MS) as usize).max(1);
        let n = self.len();
        (0..n.div_ceil(per)).map(|i| self.step_mean(n.saturating_sub((i + 1) * per), n - i * per)).collect()
    }

    fn body_level(&self) -> f32 {
        let mut loud: Vec<f32> = self.db.iter().zip(&self.smooth_db).filter(|(d, _)| **d > SILENCE_DBFS).map(|(_, s)| *s).collect();
        if loud.is_empty() {
            return SILENCE_DBFS;
        }
        loud.sort_by(f32::total_cmp);
        loud[((loud.len() - 1) as f32 * BODY_PERCENTILE) as usize]
    }

    /// onset strength per window over `from..to`: the rise in dB, full band plus kick band.
    fn flux(&self, from: usize, to: usize) -> Vec<f32> {
        let floor = |d: f32| d.max(SILENCE_DBFS);
        (from..to)
            .map(|i| {
                if i < ONSET_LAG {
                    return 0.0;
                }
                (floor(self.db[i]) - floor(self.db[i - ONSET_LAG])).max(0.0) + (floor(self.low_db[i]) - floor(self.low_db[i - ONSET_LAG])).max(0.0)
            })
            .collect()
    }

    /// the own tempo estimate over `from..to`.
    fn own_tempo(&self, from: usize, to: usize) -> Option<Tempo> {
        estimate_tempo(&self.flux(from, to), self.hop as f64 * 1000.0 / f64::from(self.rate))
    }

    /// the strongest rises in `from..to`, as (window, strength 0..1), in time order.
    fn onsets(&self, from: usize, to: usize) -> Vec<(usize, f32)> {
        if to <= from + ONSET_LAG {
            return Vec::new();
        }
        let flux = self.flux(from, to);
        let mean = flux.iter().sum::<f32>() / flux.len() as f32;
        let sd = (flux.iter().map(|f| (f - mean).powi(2)).sum::<f32>() / flux.len() as f32).sqrt();
        let threshold = mean + ONSET_THRESHOLD_SD * sd;
        let radius = (ONSET_SPACING_MS / HOP_MS) as usize;
        let mut peaks: Vec<(usize, f32)> = Vec::new();
        for (j, &f) in flux.iter().enumerate() {
            if f <= threshold || f <= 0.0 {
                continue;
            }
            let (lo, hi) = (j.saturating_sub(radius), (j + radius + 1).min(flux.len()));
            if flux[lo..j].iter().all(|g| *g < f) && flux[j + 1..hi].iter().all(|g| *g <= f) {
                peaks.push((from + j, f));
            }
        }
        peaks.sort_by(|a, b| b.1.total_cmp(&a.1));
        peaks.truncate(ONSETS_KEPT);
        let strongest = peaks.first().map_or(1.0, |p| p.1);
        peaks.iter_mut().for_each(|p| p.1 /= strongest);
        peaks.sort_by_key(|p| p.0);
        peaks
    }

    /// the kick band's rise on a beat at `ms` from the start, dB.
    fn kick_rise(&self, ms: f64) -> f32 {
        let (at, after, before) = (self.window_at(ms), self.window_at(ms + ACCENT_AFTER_MS as f64), self.window_at((ms - ACCENT_BEFORE_MS as f64).max(0.0)));
        let peak = self.low_db[at..=after.max(at)].iter().copied().fold(f32::MIN, f32::max);
        let base = self.low_db[before..at.max(before + 1).min(self.len())].iter().sum::<f32>() / (at.max(before + 1) - before) as f32;
        (peak - base).max(0.0)
    }
}

/// the tempo of an onset-strength curve sampled every `hop_ms`. the curve is smoothed a little,
/// then each beat length between [`TEMPO_MIN_BPM`] and [`TEMPO_MAX_BPM`] is scored by how well
/// the curve matches itself one to [`TEMPO_COMB`] beats later; the best is refined on its
/// furthest multiple. confidence is that mean match. `None` when the curve is too short.
pub fn estimate_tempo(flux: &[f32], hop_ms: f64) -> Option<Tempo> {
    let (min_lag, max_lag) = ((60_000.0 / f64::from(TEMPO_MAX_BPM) / hop_ms).floor() as usize, (60_000.0 / f64::from(TEMPO_MIN_BPM) / hop_ms).ceil() as usize);
    let longest = (max_lag + 1) * TEMPO_COMB + TEMPO_COMB;
    if min_lag < 2 || flux.len() < longest + max_lag * TEMPO_MIN_BEATS {
        return None;
    }
    let smooth: Vec<f64> = (0..flux.len()).map(|i| flux[i.saturating_sub(TEMPO_SMOOTH_HOPS - 1)..=i].iter().map(|f| f64::from(*f)).sum::<f64>()).collect();
    let mean = smooth.iter().sum::<f64>() / smooth.len() as f64;
    let x: Vec<f64> = smooth.iter().map(|v| v - mean).collect();
    let energy = x.iter().map(|v| v * v).sum::<f64>() / x.len() as f64;
    if energy <= 0.0 {
        return None;
    }
    let acf: Vec<f64> = (0..=longest).map(|lag| x.iter().zip(&x[lag..]).map(|(a, b)| a * b).sum::<f64>() / (x.len() - lag) as f64 / energy).collect();
    let score = |lag: usize| (1..=TEMPO_COMB).map(|k| acf[k * lag]).sum::<f64>() / TEMPO_COMB as f64;
    let best = (min_lag..=max_lag).max_by(|&a, &b| score(a).total_cmp(&score(b)))?;
    let confidence = score(best);
    if confidence <= 0.0 {
        return None;
    }
    let far = TEMPO_COMB * best;
    let peak = (far - TEMPO_COMB..=far + TEMPO_COMB).max_by(|&a, &b| acf[a].total_cmp(&acf[b]))?;
    let (left, top, right) = (acf[peak - 1], acf[peak], acf[(peak + 1).min(longest)]);
    let curve = left - 2.0 * top + right;
    let shift = if curve < 0.0 { (0.5 * (left - right) / curve).clamp(-0.5, 0.5) } else { 0.0 };
    let lag = (peak as f64 + shift) / TEMPO_COMB as f64;
    Some(Tempo { bpm: (60_000.0 / (lag * hop_ms)) as f32, confidence: confidence.min(1.0) as f32 })
}

/// aubio's beats and tempo for an edge, as times from its start, each with its accent.
fn rhythm(mono: &[f32], rate: u32, env: &Envelope) -> (Vec<(f64, f32)>, Option<Tempo>) {
    let Some(r) = navia_aubio::rhythm(mono, rate) else { return (Vec::new(), None) };
    let strongest = r.onsets.iter().map(|o| o.1).fold(0.0f32, f32::max).max(f32::MIN_POSITIVE);
    let beats: Vec<(f64, f32)> = r
        .beats_ms
        .iter()
        .map(|&b| {
            let onset = r.onsets.iter().filter(|o| (o.0 - b).abs() <= ACCENT_ONSET_MS).map(|o| o.1 / strongest).fold(0.0f32, f32::max);
            (f64::from(b), env.kick_rise(f64::from(b)) + onset * ACCENT_ONSET_DB)
        })
        .collect();
    let times: Vec<f64> = beats.iter().map(|b| b.0).collect();
    let tempo = fit_grid(&times).map(|(_, period)| Tempo { bpm: (60_000.0 / period) as f32, confidence: r.confidence });
    (beats, tempo)
}

/// measure how a song starts from its first seconds, as mono samples.
pub fn analyze_head(mono: &[f32], rate: u32) -> Head {
    let env = Envelope::new(mono, rate);
    let n = env.len();
    let curve = env.curve_from_start();
    let Some(first) = env.db.iter().position(|d| *d > SILENCE_DBFS) else {
        let all = env.ms_from_start(n);
        return Head { intro_ms: all, body_ms: all, level_db: SILENCE_DBFS, hard: false, beats: Vec::new(), tempo: None, onsets: Vec::new(), own_tempo: None, curve };
    };
    let level = env.body_level();
    let body = (first..n).find(|&i| env.smooth_db[i] >= level - OUTRO_DROP_DB).unwrap_or(first);
    let intro_ms = env.ms_from_start(first);
    let body_ms = env.ms_from_start(body);
    let (beats, tempo) = rhythm(mono, rate, &env);
    let beats = beats.into_iter().map(|(t, a)| Beat(t.round() as u32, a)).collect();
    let onsets = env.onsets(first, n).into_iter().map(|(i, s)| Onset(env.ms_from_start(i), s)).collect();
    Head { intro_ms, body_ms, level_db: level, hard: body_ms - intro_ms <= HARD_EDGE_MS, beats, tempo, onsets, own_tempo: env.own_tempo(first, n), curve }
}

/// measure how a song ends from its last seconds, as mono samples.
pub fn analyze_tail(mono: &[f32], rate: u32) -> Tail {
    let env = Envelope::new(mono, rate);
    let curve = env.curve_from_end();
    let Some(last) = env.db.iter().rposition(|d| *d > SILENCE_DBFS) else {
        let all = env.ms_before_end(0);
        return Tail { silence_ms: all, outro_ms: all, decayed_ms: all, level_db: SILENCE_DBFS, hard: false, beats: Vec::new(), tempo: None, onsets: Vec::new(), own_tempo: None, curve };
    };
    let level = env.body_level();
    let outro = (0..=last).rev().find(|&i| env.smooth_db[i] >= level - OUTRO_DROP_DB).unwrap_or(last);
    let decayed = (outro..=last).find(|&i| env.smooth_db[i] < level - DECAYED_DB).unwrap_or(last + 1);
    let silence_ms = env.ms_before_end(last + 1);
    let outro_ms = env.ms_before_end(outro);
    let length_ms = mono.len() as f64 * 1000.0 / f64::from(rate);
    let (beats, tempo) = rhythm(mono, rate, &env);
    let beats = beats.into_iter().map(|(t, a)| Beat((length_ms - t).max(0.0).round() as u32, a)).rev().collect();
    let onsets = env.onsets(0, last + 1).into_iter().map(|(i, s)| Onset(env.ms_before_end(i), s)).collect();
    let own_tempo = env.own_tempo(0, last + 1);
    Tail { silence_ms, outro_ms, decayed_ms: env.ms_before_end(decayed), level_db: level, hard: outro_ms - silence_ms <= HARD_EDGE_MS, beats, tempo, onsets, own_tempo, curve }
}

/// decode until `limit` mono samples are in `out` or the song ends.
fn read_mono(decoder: &mut Decoder, out: &mut Vec<f32>, limit: usize) -> Result<()> {
    let mut block = Vec::new();
    while out.len() < limit && decoder.next_block(&mut block)? {
        out.extend(block.chunks_exact(OUTPUT_CHANNELS).map(|f| (f[0] + f[1]) * 0.5));
    }
    Ok(())
}

/// decode only the first and last [`EDGE_WINDOW_MS`] of a song and measure them. gain from
/// leveling isn't applied: these are the song's own levels.
pub fn analyze(opener: &dyn TrackOpener, duration_ms: Option<u64>) -> Result<Edges> {
    let (mut decoder, _) = opener.open(0)?;
    let rate = decoder.sample_rate;
    let window = (EDGE_WINDOW_MS * u64::from(rate) / 1000) as usize;
    let total_ms = decoder.total_frames().map(|f| f * 1000 / u64::from(rate)).or(duration_ms);
    let mut head = Vec::with_capacity(window);
    read_mono(&mut decoder, &mut head, window)?;
    let far = total_ms.and_then(|t| t.checked_sub(EDGE_WINDOW_MS + TAIL_MARGIN_MS)).filter(|at| *at > EDGE_WINDOW_MS);
    let mut tail = Vec::new();
    match far {
        Some(at) => {
            if decoder.seekable {
                decoder.seek_ms(at)?;
            } else {
                decoder = opener.open(at)?.0;
            }
            read_mono(&mut decoder, &mut tail, usize::MAX)?;
        }
        None => {
            tail.extend_from_slice(&head);
            read_mono(&mut decoder, &mut tail, usize::MAX)?;
        }
    }
    head.truncate(window);
    let tail = &tail[tail.len().saturating_sub(window)..];
    Ok(Edges { head: analyze_head(&head, rate), tail: analyze_tail(tail, rate) })
}

/// what the library knows about a song that a transition uses besides its edges.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SongFacts {
    /// a BPM to trust: the song's tag, or Deezer's.
    pub bpm: Option<f32>,
    /// ReplayGain track gain, dB.
    pub track_gain_db: Option<f32>,
}

/// where edge analyses are kept, and where a song's tempo and ReplayGain come from.
pub trait EdgeStore: Send + Sync {
    fn facts(&self, song_id: &str) -> SongFacts;
    fn edges(&self, song_id: &str) -> Option<Edges>;
    fn keep(&self, song_id: &str, edges: &Edges);
}

/// keeps analyses in the library's `song_edges`, tied to the song's file size so a changed file
/// is measured again. BPM is the tag, else Deezer's (asked through the shared client while the
/// Deezer switch is on, and kept), else nothing: the edges' own estimate is used later.
pub struct LibraryEdgeStore {
    library: Library,
    /// for the Deezer lookup; its own thread, since the analyzer isn't on a runtime.
    runtime: OnceLock<Option<tokio::runtime::Runtime>>,
}

impl LibraryEdgeStore {
    pub fn new(library: Library) -> Self {
        Self { library, runtime: OnceLock::new() }
    }

    fn size(&self, song_id: &str) -> Option<i64> {
        self.library.song(song_id).ok().flatten().and_then(|s| s.size)
    }

    fn deezer_bpm(&self, song_id: &str) -> Option<u32> {
        if !crate::deezer::gate(&self.library).is_open() {
            return None;
        }
        let runtime = self.runtime.get_or_init(|| tokio::runtime::Builder::new_multi_thread().worker_threads(1).thread_name("navia-edges-net").enable_all().build().ok()).as_ref()?;
        runtime.block_on(crate::autoplay::look_up_bpm(&self.library, song_id))
    }
}

impl EdgeStore for LibraryEdgeStore {
    fn facts(&self, song_id: &str) -> SongFacts {
        let Some(song) = self.library.song(song_id).ok().flatten() else { return SongFacts::default() };
        let tagged = song.bpm.filter(|b| *b > 0).map(|b| b as f32);
        let bpm = tagged.or_else(|| self.deezer_bpm(song_id).map(|b| b as f32));
        SongFacts { bpm, track_gain_db: song.replay_gain.and_then(|rg| rg.track_gain).map(|g| g as f32) }
    }

    fn edges(&self, song_id: &str) -> Option<Edges> {
        let size = self.size(song_id);
        let row: Option<(i64, Option<i64>, String)> = self
            .library
            .read(|c| {
                use rusqlite::OptionalExtension;
                Ok(c.query_row("SELECT version, size, data FROM song_edges WHERE song_id = ?1", [song_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?)
            })
            .ok()
            .flatten();
        let (version, kept_size, data) = row?;
        if version != EDGES_VERSION || kept_size != size {
            return None;
        }
        serde_json::from_str(&data).ok()
    }

    fn keep(&self, song_id: &str, edges: &Edges) {
        let Ok(data) = serde_json::to_string(edges) else { return };
        let size = self.size(song_id);
        let _ = self.library.write_quiet(|tx| {
            tx.execute(
                "INSERT INTO song_edges(song_id, version, size, data) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(song_id) DO UPDATE SET version = excluded.version, size = excluded.size, data = excluded.data",
                rusqlite::params![song_id, EDGES_VERSION, size, data],
            )?;
            Ok(())
        });
    }
}

struct Job {
    song_id: String,
    opener: Arc<dyn TrackOpener>,
    duration_ms: Option<u64>,
    slot: TraitsSlot,
}

/// measures songs one at a time on a background thread, kept results first, and fills their slots.
#[derive(Clone)]
pub struct Analyzer {
    tx: Sender<Job>,
}

impl Analyzer {
    pub fn spawn(store: Arc<dyn EdgeStore>) -> Self {
        let (tx, rx) = channel::<Job>();
        // if the thread can't start, asks go nowhere and joins fall back to the plain crossfade.
        let _ = thread::Builder::new().name("navia-edges".into()).spawn(move || {
            for job in rx {
                if job.slot.get().is_some() {
                    continue;
                }
                let facts = store.facts(&job.song_id);
                let edges = store.edges(&job.song_id).or_else(|| match analyze(job.opener.as_ref(), job.duration_ms) {
                    Ok(edges) => {
                        store.keep(&job.song_id, &edges);
                        Some(edges)
                    }
                    Err(e) => {
                        tracing::info!("edge analysis of {} failed: {e}", job.song_id);
                        None
                    }
                });
                job.slot.set(SongTraits { song_id: job.song_id, edges, facts });
            }
        });
        Self { tx }
    }

    /// measure a song unless its slot was asked for before. `opener` is only used when nothing is kept.
    pub fn ask(&self, song_id: &str, opener: impl FnOnce() -> Arc<dyn TrackOpener>, duration_ms: Option<u64>, slot: &TraitsSlot) {
        if slot.ask() {
            let _ = self.tx.send(Job { song_id: song_id.to_string(), opener: opener(), duration_ms, slot: slot.clone() });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn secs(s: f64) -> usize {
        (s * f64::from(RATE)) as usize
    }

    /// noise at `amp` with a kick every `beat` seconds from `first_beat`, for `len` seconds.
    fn music(len: f64, amp: f32, beat: f64, first_beat: f64, seed: u32) -> Vec<f32> {
        let mut state = seed.wrapping_mul(2_654_435_761).max(1);
        (0..secs(len))
            .map(|n| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                let noise = (state as f32 / u32::MAX as f32 - 0.5) * amp;
                let t = n as f64 / f64::from(RATE);
                let since = (t - first_beat).rem_euclid(beat);
                let on = t >= first_beat;
                let kick = if on && since < 0.08 { (since * 60.0 * std::f64::consts::TAU).sin() as f32 * amp * 2.0 * (1.0 - since as f32 / 0.08) } else { 0.0 };
                // the beater's click, so the kick is heard across the spectrum, as a real one is.
                let click = if on && since < 0.006 { noise * 6.0 } else { 0.0 };
                noise + kick + click
            })
            .collect()
    }

    fn near(actual: u32, expected: u32, within: u32) -> bool {
        actual.abs_diff(expected) <= within
    }

    #[test]
    fn a_linear_fade_out_starts_the_outro_and_the_silence_is_measured() {
        // 20 s of music, a 6 s linear fade to nothing, then 1.5 s of silence.
        let mut mono = music(20.0, 0.5, 0.5, 0.0, 1);
        let fade = music(6.0, 0.5, 0.5, 0.0, 2);
        let n = fade.len();
        mono.extend(fade.iter().enumerate().map(|(i, x)| x * (1.0 - i as f32 / n as f32)));
        mono.extend(std::iter::repeat_n(0.0, secs(1.5)));
        let tail = analyze_tail(&mono, RATE);
        // the fade's last moments are under the silence threshold too.
        assert!(near(tail.silence_ms, 1_550, 50), "{tail:?}");
        // -3 dB of a linear fade is 29% of the way in: 4.26 s of fade left, plus the silence.
        assert!(near(tail.outro_ms, 5_760, 300), "{tail:?}");
        // -15 dB of a linear fade is 82% of the way in: 1.1 s of fade left, plus the silence.
        assert!(near(tail.decayed_ms, 2_600, 500), "{tail:?}");
        assert!(!tail.hard);
    }

    #[test]
    fn a_cold_end_is_hard_and_its_outro_is_at_the_end() {
        let mut mono = music(25.0, 0.5, 0.5, 0.0, 3);
        mono.extend(std::iter::repeat_n(0.0, secs(0.4)));
        let tail = analyze_tail(&mono, RATE);
        assert!(tail.hard, "{tail:?}");
        assert!(near(tail.silence_ms, 400, 20), "{tail:?}");
        assert!(tail.outro_ms - tail.silence_ms <= HARD_EDGE_MS, "{tail:?}");
    }

    #[test]
    fn leading_silence_and_a_slow_fade_in_are_not_a_hard_start() {
        let mut mono = vec![0.0; secs(0.8)];
        let rise = music(4.0, 0.5, 0.5, 0.0, 4);
        let n = rise.len();
        mono.extend(rise.iter().enumerate().map(|(i, x)| x * i as f32 / n as f32));
        mono.extend(music(15.0, 0.5, 0.5, 0.0, 5));
        let head = analyze_head(&mono, RATE);
        assert!(near(head.intro_ms, 800, 30), "{head:?}");
        assert!(!head.hard, "{head:?}");
        assert!(head.body_ms > 3_000, "{head:?}");

        let mut hard = vec![0.0; secs(0.2)];
        hard.extend(music(10.0, 0.5, 0.5, 0.0, 6));
        let head = analyze_head(&hard, RATE);
        assert!(head.hard && near(head.intro_ms, 200, 10), "{head:?}");
    }

    #[test]
    fn aubio_finds_the_tempo_of_steady_kicks() {
        for bpm in [72.0, 98.0, 120.0, 128.0, 174.0] {
            let beat = 60.0 / bpm;
            let head = analyze_head(&music(30.0, 0.2, beat, 0.37, 21), RATE);
            let t = head.tempo.expect("a tempo");
            let ratio = [1.0, 2.0, 0.5].iter().map(|m| (f64::from(t.bpm) * m / bpm - 1.0).abs()).fold(f64::MAX, f64::min);
            assert!(ratio < 0.015, "{bpm} BPM measured as {t:?}");
        }
    }

    #[test]
    fn the_own_estimate_finds_steady_kicks_and_not_noise() {
        for bpm in [72.0, 98.0, 120.0, 128.0, 174.0] {
            let head = analyze_head(&music(30.0, 0.2, 60.0 / bpm, 0.37, 21), RATE);
            let t = head.own_tempo.expect("a tempo");
            let ratio = [1.0, 2.0, 0.5].iter().map(|m| (f64::from(t.bpm) * m / bpm - 1.0).abs()).fold(f64::MAX, f64::min);
            assert!(ratio < 0.005 && t.confidence >= crate::audio::transition::OWN_CONFIDENT, "{bpm} BPM measured as {t:?}");
        }
        let noise = analyze_head(&music(30.0, 0.4, 1e9, 1e9, 22), RATE);
        assert!(noise.own_tempo.is_none_or(|t| t.confidence < crate::audio::transition::OWN_CONFIDENT), "{:?}", noise.own_tempo);
    }

    #[test]
    fn beats_fall_on_the_kicks_and_the_kicks_carry_the_accent() {
        // 120 BPM: a kick every 0.5 s from 1.0 s on, over quiet noise.
        let head = analyze_head(&music(20.0, 0.05, 0.5, 1.0, 7), RATE);
        let late: Vec<&Beat> = head.beats.iter().filter(|b| b.ms() > 5_000 && b.ms() < 19_500).collect();
        assert!(late.len() >= 25, "{:?}", head.beats);
        for b in &late {
            let off = (i64::from(b.ms()) - 1_000).rem_euclid(500);
            assert!(off <= 25 || off >= 475, "beat at {} ms is off the kicks", b.ms());
            assert!(b.accent() > 3.0, "{b:?}");
        }
    }

    #[test]
    fn a_tail_keeps_its_beats_and_level_curve_counted_from_the_end() {
        let mut mono = music(20.0, 0.2, 0.5, 0.0, 8);
        mono.extend(std::iter::repeat_n(0.0, secs(2.0)));
        let tail = analyze_tail(&mono, RATE);
        assert!(tail.beats.iter().all(|b| b.ms() >= 1_900), "no beats in the silence: {:?}", &tail.beats[..3.min(tail.beats.len())]);
        assert_eq!(tail.curve.len(), 88);
        assert!(tail.curve[0][0] < -500 && tail.curve[20][0] > -300, "{:?}", &tail.curve[..24]);
        assert!(mean_level(&tail.curve, 0.0, 2_000.0).unwrap().0 < -50.0);
    }
}
