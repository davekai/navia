//! Smart transitions: how long each join overlaps, where it sits, beat alignment, loudness
//! matching and the bass swap, all from the songs' measured edges ([`crate::audio::edges`]).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use crate::audio::edges::{Beat, CURVE_STEP_MS, Edges, Head, Onset, SongFacts, Tail, fit_grid, mean_level};
use crate::audio::engine::{OUTPUT_CHANNELS, crossfade_gains};
use crate::audio::processing::{Coefs, FilterShape, design};

/// tempos match within this share, also at double or half time.
pub const TEMPO_MATCH: f32 = 0.03;
/// edges whose body levels are within this many dB have similar energy. masters differ by a few
/// dB on their own, so this is wider than a heard difference.
pub const ENERGY_MATCH_DB: f32 = 6.0;
/// the least confidence at which aubio's tempo, and the own estimator's, count.
pub const AUBIO_CONFIDENT: f32 = 0.15;
pub const OWN_CONFIDENT: f32 = 0.2;
/// a cold end gets this long a crossfade, right at its end.
pub const HARD_EDGE_FADE_MS: u32 = 1_000;
/// a phrase start is a candidate when it's this far from the outgoing song's last sound.
pub const BOUNDARY_MIN_MS: u32 = 2_000;
pub const BOUNDARY_MAX_MS: u32 = 24_000;
/// a phrase entry that isn't a blend (tempos differ, or levels differ by more than
/// [`ENERGY_MATCH_DB`]) overlaps at most this long.
pub const MISMATCH_MAX_MS: u32 = 8_000;
/// the incoming song may start this long before its first downbeat (its pickup) at most.
pub const LEAD_MAX_MS: u32 = 4_000;
/// a phrase start whose level changes this much (full band or kick band) is a strong boundary.
pub const NOVELTY_STRONG_DB: f32 = 3.0;
/// bars per phrase; groupings of 8 and 16 bars are marked too.
const PHRASE_BARS: usize = 4;
const BEATS_PER_BAR: usize = 4;
/// novelty compares this many bars on each side of a boundary.
const NOVELTY_BARS: f64 = 2.0;
/// two strong boundaries this close to the outro (in beats) count as equally close, and the
/// earlier wins.
const TIE_BEATS: f64 = 4.0;
/// loudness matching never moves the incoming song by more than this.
pub const MATCH_MAX_DB: f32 = 6.0;
/// the incoming song keeps this much of its leading silence, so its attack isn't clipped.
const INTRO_PREROLL_MS: u32 = 10;
/// an onset this close to a beat counts for it; a downbeat this close before the incoming song's
/// first sound still counts as its first.
const BEAT_TOLERANCE_MS: f64 = 25.0;
const BASS_SWAP_HZ: f64 = 200.0;
const BASS_CUT_DB: f64 = -24.0;
const SHELF_Q: f64 = std::f64::consts::FRAC_1_SQRT_2;
/// the bass swap takes this long, centred on the boundary.
const BASS_SWAP_MS: f64 = 500.0;
/// filter coefficients are recomputed every this many frames during a swap.
const COEF_BLOCK: usize = 64;

/// what a transition knows about one song.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SongTraits {
    /// for the log of each transition.
    pub song_id: String,
    pub edges: Option<Edges>,
    pub facts: SongFacts,
}

/// a song's traits, filled in the background once measured. `asked` keeps it from being
/// queued for analysis twice.
#[derive(Debug, Default)]
pub struct TraitsCell {
    traits: OnceLock<Arc<SongTraits>>,
    asked: AtomicBool,
}

pub type TraitsSlot = Arc<TraitsCell>;

impl TraitsCell {
    pub fn get(&self) -> Option<&Arc<SongTraits>> {
        self.traits.get()
    }

    pub fn set(&self, traits: SongTraits) {
        let _ = self.traits.set(Arc::new(traits));
    }

    /// true the first time only.
    pub fn ask(&self) -> bool {
        !self.asked.swap(true, Ordering::AcqRel)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinKind {
    /// the album continues in order: no overlap at all.
    Gapless,
    /// from a phrase start, tempos matching: beat-aligned, with a bass swap on the boundary.
    Blend,
    /// from a phrase start, tempos differing: the incoming song enters on the boundary.
    Phrase,
    /// a cold end: about a second, right at the end.
    HardEdge,
    /// no trusted tempo or no boundary: the plain crossfade, ending at the last sound.
    Plain,
}

/// why a join was placed where it is, for the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    AlbumInOrder,
    ColdEnd,
    NotMeasured,
    NoTrustedTempo,
    NoBoundary,
    /// the strong boundary nearest the outro.
    StrongBoundary,
    /// no strong boundary in reach: the phrase start nearest the outro.
    PhraseGrid,
    /// the incoming song's intro is shorter than that outro: the latest phrase start it covers.
    IntroLength,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transition {
    pub kind: JoinKind,
    pub reason: Reason,
    /// the overlap runs to the outgoing song's last sound; only its trailing silence is dropped.
    pub overlap_ms: u32,
    /// the overlap starts this long before the outgoing song's end.
    pub start_before_ms: u32,
    /// the chosen boundary, ms before the outgoing song's end: where the incoming song's first
    /// downbeat lands and the bass swaps.
    pub boundary_ms: Option<u32>,
    /// where the lows swap, ms before the outgoing song's end.
    pub swap_ms: Option<u32>,
    /// leading silence of the incoming song skipped.
    pub skip_ms: u32,
    pub bass_swap: bool,
    /// the incoming song's loudness against the outgoing one's, as the gain that would match it.
    /// matching only ever turns down, so nothing can clip: below 0 the incoming song starts this
    /// much down and glides up to its own level; above 0 the outgoing song glides down by it.
    pub match_db: f32,
}

impl Transition {
    /// where the bass swap falls in the overlap, as a share of it.
    pub fn swap_at(&self) -> f64 {
        match self.swap_ms {
            Some(at) if self.overlap_ms > 0 => f64::from(self.start_before_ms.saturating_sub(at)) / f64::from(self.overlap_ms),
            _ => 0.5,
        }
    }
}

impl std::fmt::Display for Transition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = |ms: u32| f64::from(ms) / 1000.0;
        write!(f, "{:?} ({:?}): {:.2} s overlap from {:.2} s before the end", self.kind, self.reason, s(self.overlap_ms), s(self.start_before_ms))?;
        if let Some(b) = self.boundary_ms {
            write!(f, ", boundary {:.2} s before the end", s(b))?;
        }
        if let Some(at) = self.swap_ms {
            write!(f, ", bass swap {:.2} s before the end", s(at))?;
        }
        write!(f, ", intro skip {:.2} s, match {:+.1} dB", s(self.skip_ms), self.match_db)
    }
}

/// the two sides of a join.
#[derive(Clone, Copy, Debug)]
pub struct Join<'a> {
    pub out: Option<&'a SongTraits>,
    pub inc: Option<&'a SongTraits>,
    /// leveling gain already applied to each song (1.0 when off).
    pub out_gain: f32,
    pub in_gain: f32,
    /// the incoming song continues the outgoing one's album in order.
    pub continues: bool,
    /// the plain crossfade's length, for a song not measured yet.
    pub plain_ms: u32,
    /// how much of the outgoing song's end is held, so the overlap can't start earlier.
    pub held_ms: u32,
}

/// true when the tempos match within [`TEMPO_MATCH`], straight or at double or half time.
pub fn tempo_matches(a: Option<f32>, b: Option<f32>) -> bool {
    let (Some(a), Some(b)) = (a, b) else { return false };
    if a <= 0.0 || b <= 0.0 {
        return false;
    }
    [1.0, 2.0, 0.5].iter().any(|m| ((a * m) / b - 1.0).abs() <= TEMPO_MATCH)
}

/// how much of a song's end to hold back for its join, by its measured tail.
pub fn hold_ms(edges: &Edges) -> u32 {
    edges.tail.silence_ms + BOUNDARY_MAX_MS + LEAD_MAX_MS
}

/// a beat grid on an edge's own timeline: ms that grow with the music (from the start for a
/// head, negative before the end for a tail), with which beat of a bar is the downbeat.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grid {
    /// time of a beat, ms.
    pub phase: f64,
    pub period: f64,
    /// beats `phase + (downbeat + 4k) * period` start bars.
    pub downbeat: usize,
    /// built from aubio's tracked beats, so a phrase start can snap to one.
    pub tracked: bool,
}

impl Grid {
    pub fn bpm(&self) -> f32 {
        (60_000.0 / self.period) as f32
    }

    pub fn bar_ms(&self) -> f64 {
        self.period * BEATS_PER_BAR as f64
    }

    /// the first downbeat at or after `x`.
    pub fn downbeat_from(&self, x: f64) -> f64 {
        let first = self.phase + self.downbeat as f64 * self.period;
        first + ((x - first) / self.bar_ms()).ceil() * self.bar_ms()
    }
}

/// the beat of a bar with the most weight on it, for `points` (time, weight) on a grid.
fn downbeat_of(points: &[(f64, f32)], phase: f64, period: f64) -> Option<usize> {
    let mut weight = [0.0f32; BEATS_PER_BAR];
    for &(t, w) in points {
        let k = ((t - phase) / period).round();
        if (t - phase - k * period).abs() <= BEAT_TOLERANCE_MS {
            weight[(k as i64).rem_euclid(BEATS_PER_BAR as i64) as usize] += w;
        }
    }
    (0..BEATS_PER_BAR).max_by(|&a, &b| weight[a].total_cmp(&weight[b]))
}

/// the grid of an edge's tracked beats, `sign` 1 for a head and -1 for a tail. the downbeat is
/// the beat of the bar with the most accent on it.
pub fn grid(beats: &[Beat], sign: f64) -> Option<Grid> {
    let mut points: Vec<(f64, f32)> = beats.iter().map(|b| (sign * f64::from(b.ms()), b.accent())).collect();
    points.sort_by(|a, b| a.0.total_cmp(&b.0));
    let times: Vec<f64> = points.iter().map(|p| p.0).collect();
    let (phase, period) = fit_grid(&times)?;
    // fitted beats sit on the grid by construction, whatever the tolerance.
    let snapped: Vec<(f64, f32)> = points.iter().map(|&(t, a)| (phase + ((t - phase) / period).round() * period, a)).collect();
    Some(Grid { phase, period, downbeat: downbeat_of(&snapped, phase, period)?, tracked: true })
}

/// a grid at `bpm` from the own estimator's onsets: the phase that most onset strength falls on,
/// the downbeat where the strongest onsets are.
pub fn onset_grid(onsets: &[Onset], sign: f64, bpm: f32) -> Option<Grid> {
    let period = 60_000.0 / f64::from(bpm);
    let points: Vec<(f64, f32)> = onsets.iter().map(|o| (sign * f64::from(o.ms()), o.strength())).collect();
    let phase = beat_phase(&points, period)?;
    Some(Grid { phase, period, downbeat: downbeat_of(&points, phase, period)?, tracked: false })
}

/// the phase (ms, in `0..period`) of the beat grid that best fits `beats`, given as times with
/// strengths. `None` without any.
pub fn beat_phase(beats: &[(f64, f32)], period: f64) -> Option<f64> {
    let mut best: Option<(f64, f32)> = None;
    for &(candidate, _) in beats {
        let phase = candidate.rem_euclid(period);
        let score: f32 = beats
            .iter()
            .filter(|(t, _)| {
                let off = (t - phase).rem_euclid(period);
                off.min(period - off) <= BEAT_TOLERANCE_MS
            })
            .map(|(_, s)| s)
            .sum();
        if best.is_none_or(|(_, b)| score > b) {
            best = Some((phase, score));
        }
    }
    best.map(|(p, _)| p)
}

/// how a song's tempo came to be trusted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TempoTrust {
    /// the tag or Deezer's BPM agrees with aubio or the own estimate.
    Tagged,
    /// no tag: aubio and the own estimate agree on this edge.
    Agree,
    /// no tag: the own estimate alone, its start and end agreeing.
    Own,
}

/// an edge's trusted BPM and why: see [`TempoTrust`]. `None` when nothing can be trusted.
pub fn trusted_bpm(song: &SongTraits, tail: bool) -> Option<(f32, TempoTrust)> {
    let edges = song.edges.as_ref()?;
    let pick = |e: &Edges| if tail { (e.tail.tempo, e.tail.own_tempo) } else { (e.head.tempo, e.head.own_tempo) };
    let (aubio, own) = pick(edges);
    if let Some(bpm) = song.facts.bpm {
        let agrees = [aubio, own].into_iter().flatten().any(|t| tempo_matches(Some(bpm), Some(t.bpm)));
        return agrees.then_some((bpm, TempoTrust::Tagged));
    }
    if let (Some(a), Some(o)) = (aubio, own)
        && a.confidence >= AUBIO_CONFIDENT
        && o.confidence >= OWN_CONFIDENT
        && tempo_matches(Some(a.bpm), Some(o.bpm))
    {
        return Some((a.bpm, TempoTrust::Agree));
    }
    let (head, tail_own) = (edges.head.own_tempo?, edges.tail.own_tempo?);
    let sure = head.confidence >= OWN_CONFIDENT && tail_own.confidence >= OWN_CONFIDENT && tempo_matches(Some(head.bpm), Some(tail_own.bpm));
    sure.then(|| own.map(|o| (o.bpm, TempoTrust::Own))).flatten()
}

/// an edge's grid at its trusted tempo: from aubio's beats when they keep that tempo, else from
/// the own estimator's onsets.
fn trusted_grid(song: Option<&SongTraits>, tail: bool) -> Option<Grid> {
    let song = song?;
    let (bpm, _) = trusted_bpm(song, tail)?;
    let edges = song.edges.as_ref()?;
    let (beats, onsets, sign) = if tail { (&edges.tail.beats, &edges.tail.onsets, -1.0) } else { (&edges.head.beats, &edges.head.onsets, 1.0) };
    grid(beats, sign).filter(|g| tempo_matches(Some(bpm), Some(g.bpm()))).or_else(|| onset_grid(onsets, sign, bpm))
}

/// a phrase start: a downbeat where a 4-bar phrase begins.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Boundary {
    /// ms before the end for a tail, from the start for a head.
    pub at_ms: u32,
    /// the largest grouping it starts: 4, 8 or 16 bars.
    pub bars: u32,
    /// the biggest level change across it, full band or kick band, dB.
    pub novelty_db: f32,
    pub strong: bool,
}

/// phrase starts on a grid between `from` and `to` (ms on the grid's timeline): every fourth
/// downbeat, on the offset where the level changes most, then the 8 and 16 bar groupings the
/// same way. `level` gives the mean levels over a stretch of the timeline. in time order.
fn phrase_starts(g: &Grid, from: f64, to: f64, level: &dyn Fn(f64, f64) -> Option<(f32, f32)>) -> Vec<(f64, u32, f32)> {
    let window = NOVELTY_BARS * g.bar_ms();
    let mut downbeats = Vec::new();
    let mut d = g.downbeat_from(from);
    while d < to {
        downbeats.push(d);
        d += g.bar_ms();
    }
    let novelty: Vec<Option<f32>> = downbeats
        .iter()
        .map(|&d| {
            // only what's audible counts: a boundary needs a bar of sound after it.
            let end = (d + window).min(to - f64::from(CURVE_STEP_MS));
            if end - d < g.bar_ms() {
                return None;
            }
            let before = level(d - window, d)?;
            let after = level(d, end)?;
            Some((after.0 - before.0).abs().max((after.1 - before.1).abs()))
        })
        .collect();
    let best_offset = |every: usize, within: &dyn Fn(usize) -> bool| {
        (0..every).max_by(|&a, &b| {
            let sum = |o: usize| (0..downbeats.len()).filter(|i| i % every == o && within(*i)).filter_map(|i| novelty[i]).sum::<f32>();
            sum(a).total_cmp(&sum(b))
        })
    };
    let Some(q4) = best_offset(PHRASE_BARS, &|_| true) else { return Vec::new() };
    let q8 = best_offset(PHRASE_BARS * 2, &|i| i % PHRASE_BARS == q4);
    let q16 = best_offset(PHRASE_BARS * 4, &|i| Some(i % (PHRASE_BARS * 2)) == q8);
    (0..downbeats.len())
        .filter(|i| i % PHRASE_BARS == q4)
        .filter_map(|i| {
            let n = novelty[i]?;
            let bars = if Some(i % (PHRASE_BARS * 4)) == q16 { 16 } else if Some(i % (PHRASE_BARS * 2)) == q8 { 8 } else { 4 };
            Some((downbeats[i], bars, n))
        })
        .collect()
}

/// the tracked beat nearest `ms`, when one is within a quarter beat: the grid is a fit over
/// the whole edge, a tracked beat is where that beat really fell.
fn snap(ms: u32, beats: &[Beat], g: &Grid) -> u32 {
    if !g.tracked {
        return ms;
    }
    beats.iter().map(|b| b.ms()).min_by_key(|b| b.abs_diff(ms)).filter(|b| f64::from(b.abs_diff(ms)) <= g.period / 4.0).unwrap_or(ms)
}

/// phrase starts in a song's measured end, latest first, before its last sound.
pub fn tail_boundaries(tail: &Tail, g: &Grid) -> Vec<Boundary> {
    let span = f64::from(CURVE_STEP_MS) * tail.curve.len() as f64;
    let level = |a: f64, b: f64| mean_level(&tail.curve, -a, -b);
    phrase_starts(g, -span, -f64::from(tail.silence_ms), &level)
        .into_iter()
        .rev()
        .map(|(x, bars, n)| Boundary { at_ms: snap((-x).round() as u32, &tail.beats, g), bars, novelty_db: n, strong: n >= NOVELTY_STRONG_DB })
        .collect()
}

/// phrase starts in a song's measured start, from its first downbeat on, in time order.
pub fn head_boundaries(head: &Head, g: &Grid) -> Vec<Boundary> {
    let span = f64::from(CURVE_STEP_MS) * head.curve.len() as f64;
    let level = |a: f64, b: f64| mean_level(&head.curve, a, b);
    let first = g.downbeat_from(f64::from(head.intro_ms) - BEAT_TOLERANCE_MS);
    phrase_starts(g, first, span, &level).into_iter().map(|(x, bars, n)| Boundary { at_ms: x.round().max(0.0) as u32, bars, novelty_db: n, strong: n >= NOVELTY_STRONG_DB }).collect()
}

/// choose the join between two songs. the outgoing song's outro runs from a phrase start near
/// its natural outro to its last sound, the incoming song's intro from its first downbeat to its
/// first strong phrase start, and the transition is as long as the shorter of the two, so
/// neither is cut. the incoming song's first downbeat lands on the outgoing phrase start.
/// nothing audible is dropped; without a trusted tempo it's the plain crossfade ending at the
/// last sound.
pub fn plan(join: Join<'_>) -> Transition {
    let out_edges = join.out.and_then(|t| t.edges.as_ref());
    let in_edges = join.inc.and_then(|t| t.edges.as_ref());
    let match_db = match_db(&join, out_edges, in_edges);
    let skip_ms = in_edges.map_or(0, |e| e.head.intro_ms.saturating_sub(INTRO_PREROLL_MS));
    let base = Transition { kind: JoinKind::Plain, reason: Reason::NotMeasured, overlap_ms: 0, start_before_ms: 0, boundary_ms: None, swap_ms: None, skip_ms, bass_swap: false, match_db };
    if join.continues {
        return Transition { kind: JoinKind::Gapless, reason: Reason::AlbumInOrder, skip_ms: 0, match_db: 0.0, ..base };
    }
    let Some(out) = out_edges else {
        let overlap = join.plain_ms.min(join.held_ms);
        return Transition { overlap_ms: overlap, start_before_ms: overlap, ..base };
    };
    let tail = &out.tail;
    let at_end = |kind, reason, overlap: u32| {
        let overlap = overlap.min(join.held_ms.saturating_sub(tail.silence_ms));
        Transition { kind, reason, overlap_ms: overlap, start_before_ms: tail.silence_ms + overlap, ..base }
    };
    if tail.hard {
        return at_end(JoinKind::HardEdge, Reason::ColdEnd, HARD_EDGE_FADE_MS);
    }
    let Some(out_grid) = trusted_grid(join.out, true) else { return at_end(JoinKind::Plain, Reason::NoTrustedTempo, join.plain_ms) };
    let in_grid = trusted_grid(join.inc, false);
    // the incoming song starts at its first sound and reaches its first downbeat this much later.
    let downbeat = in_edges.zip(in_grid).map(|(e, g)| snap(g.downbeat_from(f64::from(e.head.intro_ms) - BEAT_TOLERANCE_MS).round().max(0.0) as u32, &e.head.beats, &g));
    let lead = downbeat.map_or(0, |d| d.saturating_sub(skip_ms)).min(LEAD_MAX_MS);
    let intro_ms = in_edges.zip(in_grid).zip(downbeat).and_then(|((e, g), d)| head_boundaries(&e.head, &g).into_iter().find(|b| b.strong && b.at_ms > d).map(|b| b.at_ms - d));
    let rg = |t: Option<&SongTraits>| t.and_then(|t| t.facts.track_gain_db);
    // with ReplayGain on both, levels compare as leveled, so only the music's energy differs.
    let leveled = rg(join.out).zip(rg(join.inc)).map_or(0.0, |(o, i)| o - i);
    let similar = in_edges.is_some_and(|e| (tail.level_db + leveled - e.head.level_db).abs() <= ENERGY_MATCH_DB);
    let blend = in_grid.is_some_and(|g| tempo_matches(Some(out_grid.bpm()), Some(g.bpm()))) && similar;
    let longest = if blend { BOUNDARY_MAX_MS } else { MISMATCH_MAX_MS.saturating_sub(lead) };
    let all = tail_boundaries(tail, &out_grid);
    let reach = |b: &&Boundary| b.at_ms >= tail.silence_ms + BOUNDARY_MIN_MS && b.at_ms <= tail.silence_ms + longest && b.at_ms + lead <= join.held_ms;
    let tie = TIE_BEATS * out_grid.period;
    // nearest the natural outro; an earlier one wins when they're about as near.
    let nearest = |pool: &mut dyn Iterator<Item = &Boundary>| {
        pool.min_by(|a, b| {
            let (da, db) = ((f64::from(a.at_ms) - f64::from(tail.outro_ms)).abs(), (f64::from(b.at_ms) - f64::from(tail.outro_ms)).abs());
            if (da - db).abs() <= tie { b.at_ms.cmp(&a.at_ms) } else { da.total_cmp(&db) }
        })
        .copied()
    };
    let (mut chosen, mut reason) = match nearest(&mut all.iter().filter(reach).filter(|b| b.strong)) {
        Some(b) => (b, Reason::StrongBoundary),
        None => match nearest(&mut all.iter().filter(reach)) {
            Some(b) => (b, Reason::PhraseGrid),
            None => return at_end(JoinKind::Plain, Reason::NoBoundary, join.plain_ms),
        },
    };
    // a shorter intro than outro: start at the latest phrase start whose outro the intro covers.
    if let Some(intro) = intro_ms
        && chosen.at_ms - tail.silence_ms > intro
        && let Some(b) = all.iter().filter(reach).filter(|b| b.at_ms - tail.silence_ms <= intro).max_by_key(|b| b.at_ms)
    {
        chosen = *b;
        reason = Reason::IntroLength;
    }
    let start = chosen.at_ms + lead;
    // the lows swap on the next phrase start inside the overlap, else halfway through it.
    let swap = blend.then(|| all.iter().map(|b| b.at_ms).find(|&at| at < chosen.at_ms && at > tail.silence_ms).unwrap_or((chosen.at_ms + tail.silence_ms) / 2));
    Transition {
        kind: if blend { JoinKind::Blend } else { JoinKind::Phrase },
        reason,
        overlap_ms: start - tail.silence_ms,
        start_before_ms: start,
        boundary_ms: Some(chosen.at_ms),
        swap_ms: swap,
        bass_swap: blend,
        ..base
    }
}

/// the gain that brings the incoming song to the outgoing song's loudness: from ReplayGain when both have it, else from the measured edges, with leveling taken into account.
fn match_db(join: &Join<'_>, out: Option<&Edges>, inc: Option<&Edges>) -> f32 {
    let rg = |t: Option<&SongTraits>| t.and_then(|t| t.facts.track_gain_db);
    let (out_loud, in_loud) = match (rg(join.out), rg(join.inc), out, inc) {
        (Some(a), Some(b), _, _) => (-a, -b),
        (_, _, Some(a), Some(b)) => (a.tail.level_db, b.head.level_db),
        _ => return 0.0,
    };
    let db = |g: f32| 20.0 * g.max(1e-6).log10();
    let diff = (in_loud + db(join.in_gain)) - (out_loud + db(join.out_gain));
    (-diff).clamp(-MATCH_MAX_DB, MATCH_MAX_DB)
}

/// a low shelf whose gain moves during the overlap, one per side.
struct Shelf {
    z: [[f64; 2]; OUTPUT_CHANNELS],
}

impl Shelf {
    fn run(&mut self, coefs: &Coefs, frame: &mut [f32]) {
        for (c, s) in frame.iter_mut().enumerate() {
            *s = coefs.run(&mut self.z[c], f64::from(*s)) as f32;
        }
    }
}

/// mix the outgoing song's overlap with the incoming song's first frames (both interleaved
/// stereo, the same length): equal-power fades, loudness matched by turning one side down per
/// `match_db`, and with `bass_swap` the lows handed from one to the other at `swap_ms`.
pub fn mix(out: &[f32], inc: &[f32], rate: u32, plan: &Transition) -> Vec<f32> {
    let frames = inc.len() / OUTPUT_CHANNELS;
    let mut mixed = Vec::with_capacity(inc.len());
    let (mut out_shelf, mut in_shelf) = (Shelf { z: [[0.0; 2]; OUTPUT_CHANNELS] }, Shelf { z: [[0.0; 2]; OUTPUT_CHANNELS] });
    let half_swap = BASS_SWAP_MS * f64::from(rate) / 1000.0 / 2.0 / frames.max(1) as f64;
    let swap_at = plan.swap_at();
    let (mut out_coefs, mut in_coefs) = (Coefs::identity(), Coefs::identity());
    let mut o = [0.0f32; OUTPUT_CHANNELS];
    let mut i = [0.0f32; OUTPUT_CHANNELS];
    for k in 0..frames {
        let t = (k as f32 + 0.5) / frames as f32;
        let (gain_out, gain_in) = crossfade_gains(t);
        let in_glide = 10f32.powf(plan.match_db.min(0.0) * (1.0 - t) / 20.0);
        let out_glide = 10f32.powf(-plan.match_db.max(0.0) * t / 20.0);
        o.copy_from_slice(&out[k * OUTPUT_CHANNELS..(k + 1) * OUTPUT_CHANNELS]);
        i.copy_from_slice(&inc[k * OUTPUT_CHANNELS..(k + 1) * OUTPUT_CHANNELS]);
        if plan.bass_swap {
            if k % COEF_BLOCK == 0 {
                let swapped = ((f64::from(t) - (swap_at - half_swap)) / (2.0 * half_swap)).clamp(0.0, 1.0);
                out_coefs = design(FilterShape::LowShelf, BASS_SWAP_HZ, BASS_CUT_DB * swapped, SHELF_Q, f64::from(rate));
                in_coefs = design(FilterShape::LowShelf, BASS_SWAP_HZ, BASS_CUT_DB * (1.0 - swapped), SHELF_Q, f64::from(rate));
            }
            out_shelf.run(&out_coefs, &mut o);
            in_shelf.run(&in_coefs, &mut i);
        }
        for c in 0..OUTPUT_CHANNELS {
            mixed.push(o[c] * gain_out * out_glide + i[c] * gain_in * in_glide);
        }
    }
    mixed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::edges::Tempo;

    const BEAT: u32 = 500;

    /// a song's end at 120 BPM: beats every half second up to `silence_ms` before the end, every
    /// fourth one accented (downbeats on whole seconds before the end), the level dropping from
    /// -12 to -20 dB at `drop_ms` before the end.
    fn tail_at(silence_ms: u32, drop_ms: u32, hard: bool) -> Tail {
        let beats = (0..60).map(|k| silence_ms + 250 + k * BEAT).map(|ms| Beat(ms, if (ms - silence_ms - 250) % (4 * BEAT) == 0 { 9.0 } else { 2.0 })).collect();
        let curve = (0..120u32).map(|i| if i * CURVE_STEP_MS < silence_ms { [-900, -900] } else if i * CURVE_STEP_MS < drop_ms { [-200, -260] } else { [-120, -150] }).collect();
        Tail { silence_ms, outro_ms: drop_ms, decayed_ms: silence_ms + 500, level_db: -12.0, hard, beats, tempo: Some(Tempo { bpm: 120.0, confidence: 0.6 }), onsets: Vec::new(), own_tempo: None, curve }
    }

    /// a song's start at 120 BPM after `intro_ms` of silence, first downbeat at `downbeat_ms`,
    /// quiet until `body_ms` and full from there.
    fn head_at(intro_ms: u32, downbeat_ms: u32, body_ms: u32) -> Head {
        let beats = (0..60).map(|k| downbeat_ms + k * BEAT).map(|ms| Beat(ms, if (ms - downbeat_ms) % (4 * BEAT) == 0 { 9.0 } else { 2.0 })).collect();
        let curve = (0..120u32).map(|i| if i * CURVE_STEP_MS < intro_ms { [-900, -900] } else if i * CURVE_STEP_MS < body_ms { [-200, -260] } else { [-120, -150] }).collect();
        Head { intro_ms, body_ms, level_db: -12.0, hard: false, beats, tempo: Some(Tempo { bpm: 120.0, confidence: 0.6 }), onsets: Vec::new(), own_tempo: None, curve }
    }

    fn song(tail: Tail, head: Head, bpm: Option<f32>) -> SongTraits {
        SongTraits { song_id: String::new(), edges: Some(Edges { head, tail }), facts: SongFacts { bpm, track_gain_db: None } }
    }

    fn plain_song(bpm: Option<f32>) -> SongTraits {
        let mut s = song(tail_at(400, 9_000, false), head_at(0, 0, 4_000), bpm);
        let e = s.edges.as_mut().unwrap();
        e.tail.beats.clear();
        e.head.beats.clear();
        s
    }

    fn join<'a>(out: &'a SongTraits, inc: &'a SongTraits, continues: bool) -> Join<'a> {
        Join { out: Some(out), inc: Some(inc), out_gain: 1.0, in_gain: 1.0, continues, plain_ms: 6_000, held_ms: 30_000 }
    }

    #[test]
    fn an_album_in_order_is_gapless_whatever_the_silence() {
        let (a, b) = (song(tail_at(1_500, 8_250, false), head_at(0, 0, 4_000), Some(120.0)), plain_song(None));
        assert_eq!(plan(join(&a, &b, true)).kind, JoinKind::Gapless);
        let none = SongTraits::default();
        assert_eq!(plan(join(&none, &none, true)).kind, JoinKind::Gapless);
    }

    #[test]
    fn a_grid_is_fitted_to_tracked_beats_with_the_accented_downbeat() {
        let t = tail_at(400, 9_000, false);
        let g = grid(&t.beats, -1.0).unwrap();
        assert!((g.bpm() - 120.0).abs() < 0.01, "{g:?}");
        // downbeats fall 650 ms, 2650 ms ... before the end.
        let d = g.downbeat_from(-3_000.0);
        assert!((d + 2_650.0).abs() < 1.0, "{d} {g:?}");
        // jittered beats still fit their period.
        let (phase, period) = fit_grid(&[0.0, 512.0, 990.0, 1_505.0, 2_011.0, 2_494.0, 3_000.0, 3_508.0, 3_990.0]).unwrap();
        assert!((period - 500.0).abs() < 3.0 && phase.abs() < 10.0, "{phase} {period}");
    }

    #[test]
    fn the_overlap_starts_on_the_phrase_where_the_outro_begins() {
        // the level drops at a downbeat 8.65 s before the end: the outro's phrase start.
        let out = song(tail_at(400, 8_650, false), head_at(0, 0, 4_000), Some(120.0));
        let found = tail_boundaries(&out.edges.as_ref().unwrap().tail, &grid(&out.edges.as_ref().unwrap().tail.beats, -1.0).unwrap());
        assert!(found.iter().any(|b| b.at_ms == 8_650 && b.strong), "{found:?}");
        // the incoming song: 300 ms of silence, first downbeat 800 ms in, its intro ending 16 s in.
        let inc = song(tail_at(400, 9_000, false), head_at(300, 800, 16_800), Some(120.0));
        let t = plan(join(&out, &inc, false));
        assert_eq!((t.kind, t.reason, t.boundary_ms), (JoinKind::Blend, Reason::StrongBoundary, Some(8_650)), "{t}");
        assert_eq!(t.skip_ms, 290);
        assert_eq!(t.start_before_ms, 8_650 + 510, "starts so the first downbeat lands on the boundary");
        assert_eq!(t.start_before_ms - t.overlap_ms, 400, "ends at the last sound");
        assert!(t.swap_ms.is_some_and(|at| at < 8_650 && at > 400), "{t}");
    }

    #[test]
    fn a_short_intro_moves_the_start_to_a_later_phrase() {
        // the outro's phrase starts 12.65 s before the end; phrases are 8 s long.
        let out = song(tail_at(400, 12_650, false), head_at(0, 0, 4_000), Some(120.0));
        // the intro lasts 8 s from the first downbeat: shorter than that outro.
        let inc = song(tail_at(400, 9_000, false), head_at(0, 0, 8_000), Some(120.0));
        let t = plan(join(&out, &inc, false));
        assert_eq!((t.reason, t.boundary_ms), (Reason::IntroLength, Some(4_650)), "{t}");
        assert_eq!(t.start_before_ms - t.overlap_ms, 400);
    }

    #[test]
    fn without_a_trusted_tempo_it_is_the_plain_crossfade_at_the_last_sound() {
        let (a, b) = (plain_song(Some(120.0)), plain_song(None));
        let t = plan(join(&a, &b, false));
        assert_eq!((t.kind, t.reason, t.overlap_ms, t.start_before_ms), (JoinKind::Plain, Reason::NoTrustedTempo, 6_000, 6_400));
        // beats that disagree with the tag aren't trusted either.
        let out = song(tail_at(400, 8_650, false), head_at(0, 0, 4_000), Some(97.0));
        assert_eq!(plan(join(&out, &b, false)).reason, Reason::NoTrustedTempo);
        // no tag and only aubio's word for it: not trusted.
        let unsure = song(tail_at(400, 8_650, false), head_at(0, 0, 4_000), None);
        assert_eq!(plan(join(&unsure, &b, false)).reason, Reason::NoTrustedTempo);
        // not measured at all: the plain crossfade at the file's end.
        let none = SongTraits::default();
        assert_eq!(plan(join(&none, &b, false)).start_before_ms, 6_000);
    }

    #[test]
    fn a_tempo_is_trusted_by_a_tag_or_by_two_estimates_agreeing() {
        let own = |bpm: f32, confidence: f32| Some(Tempo { bpm, confidence });
        let mut s = song(tail_at(400, 8_650, false), head_at(0, 0, 4_000), Some(120.0));
        assert_eq!(trusted_bpm(&s, true), Some((120.0, TempoTrust::Tagged)), "the tag agrees with aubio");
        let e = s.edges.as_mut().unwrap();
        e.tail.tempo = Some(Tempo { bpm: 97.0, confidence: 0.6 });
        assert_eq!(trusted_bpm(&s, true), None, "the tag agrees with neither estimate");
        s.edges.as_mut().unwrap().tail.own_tempo = own(60.2, 0.01);
        assert_eq!(trusted_bpm(&s, true), Some((120.0, TempoTrust::Tagged)), "the own estimate agrees at half time");
        // no tag: aubio and the own estimate must agree, each confident enough.
        let mut s = song(tail_at(400, 8_650, false), head_at(0, 0, 4_000), None);
        s.edges.as_mut().unwrap().tail.own_tempo = own(121.0, OWN_CONFIDENT);
        assert_eq!(trusted_bpm(&s, true), Some((120.0, TempoTrust::Agree)));
        s.edges.as_mut().unwrap().tail.own_tempo = own(121.0, OWN_CONFIDENT / 2.0);
        assert_eq!(trusted_bpm(&s, true), None, "the own estimate isn't confident");
        // the own estimate alone, when the song's start and end agree on it.
        let mut s = song(tail_at(400, 8_650, false), head_at(0, 0, 4_000), None);
        let e = s.edges.as_mut().unwrap();
        e.tail.tempo = None;
        e.tail.own_tempo = own(100.0, 0.3);
        e.head.own_tempo = own(101.0, 0.3);
        assert_eq!(trusted_bpm(&s, true), Some((100.0, TempoTrust::Own)));
        s.edges.as_mut().unwrap().head.own_tempo = own(130.0, 0.3);
        assert_eq!(trusted_bpm(&s, true), None);
    }

    #[test]
    fn without_aubio_beats_the_grid_comes_from_the_onsets() {
        // onsets every half second 650 ms + k * 500 before the end, strongest every fourth.
        let onsets: Vec<Onset> = (0..40).map(|k| Onset(650 + k * 500, if k % 4 == 0 { 1.0 } else { 0.4 })).collect();
        let g = onset_grid(&onsets, -1.0, 120.0).unwrap();
        assert!(!g.tracked);
        assert!((g.downbeat_from(-3_000.0) + 2_650.0).abs() < 1.0, "{g:?}");
        let mut s = song(tail_at(400, 8_650, false), head_at(0, 0, 4_000), Some(120.0));
        let e = s.edges.as_mut().unwrap();
        e.tail.beats.clear();
        e.tail.onsets = onsets;
        let inc = song(tail_at(400, 9_000, false), head_at(0, 0, 4_000), Some(120.0));
        let t = plan(join(&s, &inc, false));
        assert_eq!((t.kind, t.boundary_ms), (JoinKind::Blend, Some(8_650)), "{t}");
    }

    #[test]
    fn mismatched_levels_cap_the_phrase_entry() {
        // the outro's phrase starts 14.65 s before the end; phrases are 8 s long.
        let out = song(tail_at(400, 14_650, false), head_at(0, 0, 4_000), Some(120.0));
        let mut inc = song(tail_at(400, 9_000, false), head_at(0, 0, 4_000), Some(120.0));
        assert_eq!(plan(join(&out, &inc, false)).boundary_ms, Some(14_650));
        // a 10 dB quieter incoming song: the phrase start within the cap instead.
        inc.edges.as_mut().unwrap().head.level_db = -22.0;
        let t = plan(join(&out, &inc, false));
        assert_eq!((t.kind, t.boundary_ms), (JoinKind::Phrase, Some(6_650)), "{t}");
        assert!(t.overlap_ms <= MISMATCH_MAX_MS, "{t}");
        assert_eq!(t.start_before_ms - t.overlap_ms, 400, "still ends at the last sound");
    }

    #[test]
    fn mismatched_tempos_cap_the_phrase_entry() {
        let out = song(tail_at(400, 14_650, false), head_at(0, 0, 4_000), Some(120.0));
        let mut inc = song(tail_at(400, 9_000, false), head_at(0, 0, 4_000), Some(97.0));
        let e = inc.edges.as_mut().unwrap();
        e.head.tempo = Some(Tempo { bpm: 97.0, confidence: 0.6 });
        e.head.beats = (0..60).map(|k| Beat((k as f64 * 60_000.0 / 97.0).round() as u32, if k % 4 == 0 { 9.0 } else { 2.0 })).collect();
        let t = plan(join(&out, &inc, false));
        assert_eq!((t.kind, t.boundary_ms), (JoinKind::Phrase, Some(6_650)), "{t}");
        assert!(t.overlap_ms <= MISMATCH_MAX_MS, "{t}");
        assert_eq!(t.start_before_ms - t.overlap_ms, 400, "still ends at the last sound");
    }

    #[test]
    fn a_cold_end_gets_a_second_at_the_last_sound() {
        let (cold, b) = (song(tail_at(200, 300, true), head_at(0, 0, 4_000), Some(120.0)), plain_song(None));
        let t = plan(join(&cold, &b, false));
        assert_eq!((t.kind, t.overlap_ms, t.start_before_ms), (JoinKind::HardEdge, HARD_EDGE_FADE_MS, 1_200));
    }

    #[test]
    fn tempos_match_straight_and_at_double_or_half_time() {
        assert!(tempo_matches(Some(120.0), Some(123.0)));
        assert!(tempo_matches(Some(120.0), Some(241.0)));
        assert!(tempo_matches(Some(60.5), Some(120.0)));
        assert!(!tempo_matches(Some(120.0), Some(126.5)));
        assert!(!tempo_matches(None, Some(120.0)));
    }

    #[test]
    fn loudness_matches_by_replaygain_then_by_measure() {
        let mut a = plain_song(None);
        let mut b = plain_song(None);
        b.edges.as_mut().unwrap().head.level_db = -8.0;
        // measured: the incoming song is 4 dB louder, so it starts 4 dB down.
        assert!((plan(join(&a, &b, false)).match_db + 4.0).abs() < 1e-4);
        // ReplayGain wins: track gains -6 and -9 say the incoming song is 3 dB louder.
        a.facts.track_gain_db = Some(-6.0);
        b.facts.track_gain_db = Some(-9.0);
        assert!((plan(join(&a, &b, false)).match_db + 3.0).abs() < 1e-4);
        // with leveling on, the applied gains already even them out.
        let leveled = Join { out_gain: 10f32.powf(-6.0 / 20.0), in_gain: 10f32.powf(-9.0 / 20.0), ..join(&a, &b, false) };
        assert!(plan(leveled).match_db.abs() < 1e-3);
        // never more than the cap.
        b.facts.track_gain_db = Some(-30.0);
        assert_eq!(plan(join(&a, &b, false)).match_db, -MATCH_MAX_DB);
    }

    #[test]
    fn the_mix_fades_and_swaps_the_bass() {
        let rate = 48_000;
        let frames = 48_000;
        // a 50 Hz tone on both sides: the outgoing side should lose it after the swap, the
        // incoming side gain it.
        let tone: Vec<f32> = (0..frames).flat_map(|n| {
            let s = (std::f64::consts::TAU * 50.0 * n as f64 / f64::from(rate)).sin() as f32 * 0.5;
            [s, s]
        }).collect();
        let silent = vec![0.0f32; tone.len()];
        let p = Transition { kind: JoinKind::Blend, reason: Reason::StrongBoundary, overlap_ms: 1_000, start_before_ms: 1_000, boundary_ms: None, swap_ms: None, skip_ms: 0, bass_swap: true, match_db: 0.0 };
        let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt();
        let out_only = mix(&tone, &silent, rate, &p);
        let in_only = mix(&silent, &tone, rate, &p);
        let quarter = tone.len() / 4;
        let plain = Transition { bass_swap: false, ..p };
        let out_plain = mix(&tone, &silent, rate, &plain);
        let in_plain = mix(&silent, &tone, rate, &plain);
        // before the swap the outgoing lows pass and the incoming lows are cut, after it the reverse.
        assert!(rms(&out_only[..quarter]) > 0.9 * rms(&out_plain[..quarter]));
        assert!(rms(&in_only[..quarter]) < 0.15 * rms(&in_plain[..quarter]));
        assert!(rms(&out_only[3 * quarter..]) < 0.15 * rms(&out_plain[3 * quarter..]));
        assert!(rms(&in_only[3 * quarter..]) > 0.9 * rms(&in_plain[3 * quarter..]));
    }

    #[test]
    fn loudness_matching_only_turns_a_side_down() {
        let frames = 4_800;
        let ones = vec![1.0f32; frames * 2];
        let silent = vec![0.0f32; frames * 2];
        let at = |db: f32, out: &[f32], inc: &[f32], k: usize| {
            let p = Transition { kind: JoinKind::Plain, reason: Reason::NoTrustedTempo, overlap_ms: 100, start_before_ms: 100, boundary_ms: None, swap_ms: None, skip_ms: 0, bass_swap: false, match_db: db };
            mix(out, inc, 48_000, &p)[k * 2]
        };
        let (first, last) = (0, frames - 1);
        let (g_out, _) = crossfade_gains(0.5 / frames as f32);
        let (_, g_in) = crossfade_gains((frames as f32 - 0.5) / frames as f32);
        // a louder incoming song starts 6 dB down and ends at its own level.
        assert!((at(-6.0, &silent, &ones, first) / crossfade_gains(0.5 / frames as f32).1 - 0.5012).abs() < 1e-3);
        assert!((at(-6.0, &silent, &ones, last) / g_in - 1.0).abs() < 1e-3);
        // a quieter one leaves itself alone and the outgoing song glides down instead.
        assert!((at(6.0, &ones, &silent, first) / g_out - 1.0).abs() < 1e-3);
        assert!((at(6.0, &silent, &ones, last) / g_in - 1.0).abs() < 1e-6);
        let (g_out_last, _) = crossfade_gains((frames as f32 - 0.5) / frames as f32);
        assert!((at(6.0, &ones, &silent, last) / g_out_last - 0.5012).abs() < 1e-3);
    }
}
