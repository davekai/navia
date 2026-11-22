//! Processing in the audio chain.
//!
//! [`ProcessingChain`] runs on the final mix before the ring buffer, and is bypassed with no stages.
//! [`Equalizer`] runs after the ring buffer in [`crate::audio::render`], so changes are heard at once.
//! It is real-time safe, adds no latency, and smooths every parameter. While off it does nothing,
//! so the path stays bit-exact.

/// One stage of the [`ProcessingChain`]. None exist yet.
pub trait Processor: Send {
    /// Called before the first block and whenever the output rate changes.
    fn configure(&mut self, sample_rate: u32, channels: usize);
    /// Process interleaved samples in place.
    fn process(&mut self, interleaved: &mut [f32]);
    /// Discard any internal state, such as after a seek.
    fn reset(&mut self);
}

#[derive(Default)]
pub struct ProcessingChain {
    stages: Vec<Box<dyn Processor>>,
    configured_rate: Option<u32>,
    scratch: Vec<f32>,
}

impl ProcessingChain {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, stage: Box<dyn Processor>) {
        self.stages.push(stage);
    }

    /// True when no stage is active, so samples pass through unchanged.
    pub fn is_bypassed(&self) -> bool {
        self.stages.is_empty()
    }

    /// Clear stage state after a seek or skip, keeping the rate configuration.
    pub(crate) fn reset(&mut self) {
        for stage in &mut self.stages {
            stage.reset();
        }
    }

    pub(crate) fn set_output(&mut self, sample_rate: u32, channels: usize) {
        if self.configured_rate != Some(sample_rate) {
            for stage in &mut self.stages {
                stage.configure(sample_rate, channels);
            }
            self.configured_rate = Some(sample_rate);
        }
    }

    /// Run the chain over a copy of `data`, or return `data` unchanged when bypassed.
    pub(crate) fn apply<'a>(&'a mut self, data: &'a [f32]) -> &'a [f32] {
        if self.stages.is_empty() {
            return data;
        }
        self.scratch.clear();
        self.scratch.extend_from_slice(data);
        for stage in &mut self.stages {
            stage.process(&mut self.scratch);
        }
        &self.scratch
    }
}


// ---------------------------------------------------------------------------------------------
// Equalizer
// ---------------------------------------------------------------------------------------------

/// Most bands the equalizer runs. Parametric filters plus a headphone correction fit well inside.
pub const MAX_EQ_BANDS: usize = 64;

/// Frames processed between control updates. Coefficients are interpolated inside each block.
const BLOCK_FRAMES: usize = 64;
/// How fast parameters reach a new value.
const SMOOTH_TAU_S: f64 = 0.015;
/// Limiter release time constant.
const LIMITER_RELEASE_S: f64 = 0.15;
/// Peaks above this start to be limited (about -1 dB).
const LIMITER_KNEE: f64 = 0.891_250_938;
/// Longest crossfeed delay, in frames.
const CROSSFEED_BUF: usize = 256;
const CROSSFEED_DELAY_S: f64 = 0.000_3;
const CROSSFEED_CUTOFF_HZ: f64 = 700.0;
/// Share of the low-passed side difference fed across at crossfeed amount 1.0.
const CROSSFEED_MAX: f64 = 0.45;
/// State below this is inaudible, so the filter may stop.
const SETTLED: f64 = 1e-6;

/// The shape of one filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterShape {
    Peaking,
    LowShelf,
    HighShelf,
}

/// One filter, in the terms of the RBJ cookbook formulas.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EqBand {
    pub shape: FilterShape,
    pub freq_hz: f32,
    pub gain_db: f32,
    pub q: f32,
}

/// Everything the equalizer does, in run order: preamp, filters, mono, crossfeed, balance,
/// then the limiter last so its ceiling holds. Volume comes after all of it.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct EqSettings {
    pub preamp_db: f32,
    pub bands: Vec<EqBand>,
    pub limiter: bool,
    /// -1 is full left, 1 is full right.
    pub balance: f32,
    pub mono: bool,
    /// 0 to 1, or `None` when off.
    pub crossfeed: Option<f32>,
}

impl EqSettings {
    /// True when nothing would change the samples, so the output stays bit-exact.
    pub fn is_identity(&self) -> bool {
        self.preamp_db == 0.0 && self.bands.iter().all(|b| b.gain_db == 0.0) && self.balance == 0.0 && !self.mono && self.crossfeed.is_none_or(|c| c <= 0.0)
    }
}

/// A plan with no heap storage, for handing to the audio callback.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct EqTarget {
    preamp_db: f64,
    bands: [EqBand; MAX_EQ_BANDS],
    count: usize,
    limiter: bool,
    balance: f64,
    mono: bool,
    crossfeed: f64,
}

const IDLE_BAND: EqBand = EqBand { shape: FilterShape::Peaking, freq_hz: 1000.0, gain_db: 0.0, q: 1.0 };

impl EqTarget {
    pub(crate) const fn off() -> Self {
        Self { preamp_db: 0.0, bands: [IDLE_BAND; MAX_EQ_BANDS], count: 0, limiter: false, balance: 0.0, mono: false, crossfeed: 0.0 }
    }

    pub(crate) fn from_settings(settings: Option<&EqSettings>) -> Self {
        let mut out = Self::off();
        let Some(s) = settings else { return out };
        if s.is_identity() {
            return out;
        }
        out.preamp_db = f64::from(s.preamp_db).clamp(-48.0, 24.0);
        out.count = s.bands.len().min(MAX_EQ_BANDS);
        if s.bands.len() > MAX_EQ_BANDS {
            tracing::warn!("equalizer: using the first {MAX_EQ_BANDS} of {} filters", s.bands.len());
        }
        for (slot, band) in out.bands.iter_mut().zip(&s.bands) {
            *slot = EqBand { shape: band.shape, freq_hz: band.freq_hz.clamp(1.0, 192_000.0), gain_db: band.gain_db.clamp(-30.0, 30.0), q: band.q.max(0.01) };
        }
        out.limiter = s.limiter;
        out.balance = f64::from(s.balance).clamp(-1.0, 1.0);
        out.mono = s.mono;
        out.crossfeed = s.crossfeed.map_or(0.0, |c| f64::from(c).clamp(0.0, 1.0) * CROSSFEED_MAX);
        out
    }
}

/// One biquad's coefficients, normalized so a0 is 1.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Coefs {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Coefs {
    pub(crate) fn identity() -> Self {
        Self { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0 }
    }

    /// one sample through transposed direct form II with state `z`.
    pub(crate) fn run(&self, z: &mut [f64; 2], x: f64) -> f64 {
        let y = self.b0 * x + z[0];
        z[0] = self.b1 * x - self.a1 * y + z[1];
        z[1] = self.b2 * x - self.a2 * y;
        y
    }
}

/// The cookbook biquads, the same formulas as `filter_db` in the app's curve.
pub(crate) fn design(shape: FilterShape, freq_hz: f64, gain_db: f64, q: f64, rate: f64) -> Coefs {
    // Above Nyquist a filter can't act on anything in the signal.
    if gain_db == 0.0 || freq_hz >= rate * 0.49 {
        return if gain_db == 0.0 { unity_at(freq_hz, q, rate) } else { Coefs::identity() };
    }
    let a = 10f64.powf(gain_db / 40.0);
    let w0 = std::f64::consts::TAU * freq_hz / rate;
    let (sin, cos) = w0.sin_cos();
    let alpha = sin / (2.0 * q.max(0.01));
    let (b0, b1, b2, a0, a1, a2) = match shape {
        FilterShape::Peaking => (1.0 + alpha * a, -2.0 * cos, 1.0 - alpha * a, 1.0 + alpha / a, -2.0 * cos, 1.0 - alpha / a),
        FilterShape::LowShelf => {
            let s = 2.0 * a.sqrt() * alpha;
            (a * ((a + 1.0) - (a - 1.0) * cos + s), 2.0 * a * ((a - 1.0) - (a + 1.0) * cos), a * ((a + 1.0) - (a - 1.0) * cos - s), (a + 1.0) + (a - 1.0) * cos + s, -2.0 * ((a - 1.0) + (a + 1.0) * cos), (a + 1.0) + (a - 1.0) * cos - s)
        }
        FilterShape::HighShelf => {
            let s = 2.0 * a.sqrt() * alpha;
            (a * ((a + 1.0) + (a - 1.0) * cos + s), -2.0 * a * ((a - 1.0) + (a + 1.0) * cos), a * ((a + 1.0) + (a - 1.0) * cos - s), (a + 1.0) - (a - 1.0) * cos + s, 2.0 * ((a - 1.0) - (a + 1.0) * cos), (a + 1.0) - (a - 1.0) * cos - s)
        }
    };
    Coefs { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0 }
}

/// Unity gain with the filter's poles kept, so b equals a. A filter ramped to 0 dB then decays
/// on its own and the output never jumps.
fn unity_at(freq_hz: f64, q: f64, rate: f64) -> Coefs {
    let w0 = std::f64::consts::TAU * freq_hz.min(rate * 0.45) / rate;
    let (sin, cos) = w0.sin_cos();
    let alpha = sin / (2.0 * q.max(0.01));
    let (a0, a1, a2) = (1.0 + alpha, -2.0 * cos, 1.0 - alpha);
    Coefs { b0: 1.0, b1: a1 / a0, b2: a2 / a0, a1: a1 / a0, a2: a2 / a0 }
}

fn approach(current: &mut f64, target: f64, k: f64, snap: f64) -> bool {
    if *current == target {
        return false;
    }
    *current += (target - *current) * k;
    if (*current - target).abs() <= snap {
        *current = target;
    }
    true
}

/// One running filter: smoothed parameters, the block's coefficient ramp, and per-channel state.
#[derive(Clone, Copy)]
struct Slot {
    shape: FilterShape,
    freq_ln: f64,
    q_ln: f64,
    gain_db: f64,
    target: EqBand,
    /// A new shape waits for the gain to reach 0 dB, where every shape is the same.
    pending_shape: bool,
    coefs: Coefs,
    step: Coefs,
    moving: bool,
    z: [[f64; 2]; 2],
}

impl Slot {
    fn new() -> Self {
        Self { shape: FilterShape::Peaking, freq_ln: 1000f64.ln(), q_ln: 0.0, gain_db: 0.0, target: IDLE_BAND, pending_shape: false, coefs: Coefs::identity(), step: Coefs::default(), moving: false, z: [[0.0; 2]; 2] }
    }

    fn settled(&self) -> bool {
        !self.moving && !self.pending_shape && self.gain_db == 0.0 && self.target.gain_db == 0.0 && self.z.iter().flatten().all(|v| v.abs() < SETTLED)
    }

    fn snap_to_target(&mut self) {
        self.shape = self.target.shape;
        self.freq_ln = f64::from(self.target.freq_hz).ln();
        self.q_ln = f64::from(self.target.q).ln();
        self.gain_db = f64::from(self.target.gain_db);
        self.pending_shape = false;
    }

    fn design(&self, rate: f64) -> Coefs {
        design(self.shape, self.freq_ln.exp(), self.gain_db, self.q_ln.exp(), rate)
    }

    fn at_target(&self) -> bool {
        !self.pending_shape && self.gain_db == f64::from(self.target.gain_db) && self.freq_ln == f64::from(self.target.freq_hz).ln() && self.q_ln == f64::from(self.target.q).ln()
    }

    /// Moves toward the target and sets the coefficient ramp for a block of `frames`.
    fn advance(&mut self, k: f64, rate: f64, frames: usize) {
        if self.at_target() {
            if self.moving {
                self.moving = false;
                self.step = Coefs::default();
            }
            return;
        }
        if self.pending_shape {
            approach(&mut self.gain_db, 0.0, k, 0.01);
            if self.gain_db == 0.0 {
                self.shape = self.target.shape;
                self.freq_ln = f64::from(self.target.freq_hz).ln();
                self.q_ln = f64::from(self.target.q).ln();
                self.pending_shape = false;
            }
        } else {
            approach(&mut self.gain_db, f64::from(self.target.gain_db), k, 0.005);
            approach(&mut self.freq_ln, f64::from(self.target.freq_hz).ln(), k, 0.0002);
            approach(&mut self.q_ln, f64::from(self.target.q).ln(), k, 0.0002);
        }
        let end = self.design(rate);
        let n = frames as f64;
        self.step = Coefs { b0: (end.b0 - self.coefs.b0) / n, b1: (end.b1 - self.coefs.b1) / n, b2: (end.b2 - self.coefs.b2) / n, a1: (end.a1 - self.coefs.a1) / n, a2: (end.a2 - self.coefs.a2) / n };
        self.moving = true;
    }

    /// Runs the block in place in transposed direct form 2, which stays clean under coefficient changes.
    fn run(&mut self, block: &mut [f64], frames: usize, end: &Coefs) {
        let (mut b0, mut b1, mut b2, mut a1, mut a2) = (self.coefs.b0, self.coefs.b1, self.coefs.b2, self.coefs.a1, self.coefs.a2);
        let step = self.step;
        let mut z = self.z;
        for frame in 0..frames {
            b0 += step.b0;
            b1 += step.b1;
            b2 += step.b2;
            a1 += step.a1;
            a2 += step.a2;
            for ch in 0..2 {
                let x = block[frame * 2 + ch];
                let y = b0 * x + z[ch][0];
                z[ch][0] = b1 * x - a1 * y + z[ch][1];
                z[ch][1] = b2 * x - a2 * y;
                block[frame * 2 + ch] = y;
            }
        }
        for v in z.iter_mut().flatten() {
            if v.abs() < 1e-30 {
                *v = 0.0;
            } else if !v.is_finite() {
                *v = 0.0;
            }
        }
        self.z = z;
        self.coefs = *end;
    }
}

/// The output-side equalizer. Change it only through [`Self::set_target`] and [`Self::configure`].
pub struct Equalizer {
    rate: f64,
    target: EqTarget,
    slots: [Slot; MAX_EQ_BANDS],
    preamp_db: f64,
    balance: f64,
    mono: f64,
    crossfeed: f64,
    limiter_on: bool,
    limiter_gain: f64,
    limiter_step: f64,
    delay: [[f32; CROSSFEED_BUF]; 2],
    delay_write: usize,
    delay_frames: usize,
    lp_diff: f64,
    lp_coef: f64,
    bypassed: bool,
    block: Box<[f64; BLOCK_FRAMES * 2]>,
}

impl Default for Equalizer {
    fn default() -> Self {
        Self::new()
    }
}

impl Equalizer {
    pub fn new() -> Self {
        Self {
            rate: 0.0,
            target: EqTarget::off(),
            slots: [Slot::new(); MAX_EQ_BANDS],
            preamp_db: 0.0,
            balance: 0.0,
            mono: 0.0,
            crossfeed: 0.0,
            limiter_on: false,
            limiter_gain: 1.0,
            limiter_step: 0.0,
            delay: [[0.0; CROSSFEED_BUF]; 2],
            delay_write: 0,
            delay_frames: 0,
            lp_diff: 0.0,
            lp_coef: 0.0,
            bypassed: true,
            block: Box::new([0.0; BLOCK_FRAMES * 2]),
        }
    }

    /// True when the path is idle and samples pass through untouched.
    pub fn is_bypassed(&self) -> bool {
        self.bypassed
    }

    /// Sets the target. Safe to call often, since values move there smoothly.
    pub fn set_settings(&mut self, settings: Option<&EqSettings>) {
        self.set_target(EqTarget::from_settings(settings));
    }

    pub(crate) fn set_target(&mut self, target: EqTarget) {
        self.target = target;
        for (i, slot) in self.slots.iter_mut().enumerate() {
            let band = if i < target.count { target.bands[i] } else { EqBand { gain_db: 0.0, ..slot.target } };
            slot.target = band;
            if slot.gain_db == 0.0 && !slot.moving {
                // Silent, so shape and frequency can change freely.
                slot.shape = band.shape;
                slot.freq_ln = f64::from(band.freq_hz).ln();
                slot.q_ln = f64::from(band.q).ln();
                if slot.gain_db != f64::from(band.gain_db) {
                    slot.moving = true;
                }
                slot.pending_shape = false;
            } else if band.shape != slot.shape {
                slot.pending_shape = true;
            }
        }
        if self.rate > 0.0 {
            self.bypassed = false;
            self.check_bypass();
        }
    }

    /// Sets the output rate. Filters are rebuilt and state is cleared, since the stream restarts anyway.
    pub fn configure(&mut self, rate: u32) {
        let rate = rate.max(1);
        if self.rate == f64::from(rate) {
            return;
        }
        self.rate = f64::from(rate);
        self.limiter_step = 1.0 - (-1.0 / (self.rate * LIMITER_RELEASE_S)).exp();
        self.delay_frames = ((self.rate * CROSSFEED_DELAY_S).round() as usize).clamp(1, CROSSFEED_BUF - 1);
        self.lp_coef = (-std::f64::consts::TAU * CROSSFEED_CUTOFF_HZ / self.rate).exp();
        let rate = self.rate;
        self.snap_to_target();
        for slot in self.slots.iter_mut() {
            slot.coefs = slot.design(rate);
            slot.z = [[0.0; 2]; 2];
        }
        self.delay = [[0.0; CROSSFEED_BUF]; 2];
        self.delay_write = 0;
        self.lp_diff = 0.0;
        self.limiter_gain = 1.0;
        self.bypassed = false;
        self.check_bypass();
    }

    fn snap_to_target(&mut self) {
        let t = self.target;
        self.preamp_db = t.preamp_db;
        self.balance = t.balance;
        self.mono = if t.mono { 1.0 } else { 0.0 };
        self.crossfeed = t.crossfeed;
        self.limiter_on = t.limiter;
        for (i, slot) in self.slots.iter_mut().enumerate() {
            let band = if i < t.count { t.bands[i] } else { EqBand { gain_db: 0.0, ..slot.target } };
            slot.target = band;
            slot.snap_to_target();
            slot.moving = false;
            slot.step = Coefs::default();
        }
    }

    fn check_bypass(&mut self) {
        let t = &self.target;
        let idle = t.count == 0 && t.preamp_db == 0.0 && t.balance == 0.0 && !t.mono && t.crossfeed == 0.0;
        self.bypassed = idle && self.preamp_db == 0.0 && self.balance == 0.0 && self.mono == 0.0 && self.crossfeed == 0.0 && self.limiter_gain == 1.0 && self.slots.iter().all(Slot::settled);
    }

    /// Clears filter history after a seek or skip, keeping parameters.
    pub fn reset(&mut self) {
        for slot in self.slots.iter_mut() {
            slot.z = [[0.0; 2]; 2];
        }
        self.delay = [[0.0; CROSSFEED_BUF]; 2];
        self.lp_diff = 0.0;
        self.limiter_gain = 1.0;
    }

    /// Processes interleaved stereo in place. Does nothing at all while bypassed.
    pub fn process(&mut self, samples: &mut [f32]) {
        if self.bypassed || self.rate == 0.0 {
            return;
        }
        for chunk in samples.chunks_mut(BLOCK_FRAMES * 2) {
            if self.bypassed {
                return;
            }
            self.process_block(chunk);
            self.check_bypass();
        }
    }

    fn process_block(&mut self, chunk: &mut [f32]) {
        let frames = chunk.len() / 2;
        if frames == 0 {
            return;
        }
        let rate = self.rate;
        let k = 1.0 - (-(frames as f64) / (rate * SMOOTH_TAU_S)).exp();

        // Read each scalar's start value for the block, then move it toward the target.
        let (pre_start, bal_start, mono_start, cross_start) = (self.preamp_db, self.balance, self.mono, self.crossfeed);
        let target = self.target;
        approach(&mut self.preamp_db, target.preamp_db, k, 0.0005);
        approach(&mut self.balance, target.balance, k, 0.0002);
        approach(&mut self.mono, if target.mono { 1.0 } else { 0.0 }, k, 0.0005);
        approach(&mut self.crossfeed, target.crossfeed, k, 0.0002);
        let pre_a = 10f64.powf(pre_start / 20.0);
        let pre_b = 10f64.powf(self.preamp_db / 20.0);

        for (i, s) in chunk.iter().enumerate() {
            self.block[i] = f64::from(*s);
        }
        let n = frames as f64;
        let pre_step = (pre_b - pre_a) / n;
        for f in 0..frames {
            let g = pre_a + pre_step * (f + 1) as f64;
            self.block[f * 2] *= g;
            self.block[f * 2 + 1] *= g;
        }

        let block_ptr = self.block.as_mut_slice();
        for slot in self.slots.iter_mut() {
            if slot.settled() && slot.target.gain_db == 0.0 && slot.gain_db == 0.0 {
                continue;
            }
            slot.advance(k, rate, frames);
            let end = if slot.moving { slot.design(rate) } else { slot.coefs };
            slot.run(block_ptr, frames, &end);
        }

        self.post_process(frames, bal_start, mono_start, cross_start, n);

        for (s, v) in chunk.iter_mut().zip(self.block.iter()) {
            *s = *v as f32;
        }
    }

    fn post_process(&mut self, frames: usize, bal_start: f64, mono_start: f64, cross_start: f64, n: f64) {
        let bal_step = (self.balance - bal_start) / n;
        let mono_step = (self.mono - mono_start) / n;
        let cross_step = (self.crossfeed - cross_start) / n;
        let run_cross = cross_start > 0.0 || self.crossfeed > 0.0 || self.target.crossfeed > 0.0;
        let lim_step = self.limiter_step;
        let limiter_on = self.target.limiter;

        for f in 0..frames {
            let progress = (f + 1) as f64;
            let (mut l, mut r) = (self.block[f * 2], self.block[f * 2 + 1]);

            let m = mono_start + mono_step * progress;
            if m != 0.0 {
                let (ll, rr) = (l * (1.0 - m * 0.5) + r * (m * 0.5), r * (1.0 - m * 0.5) + l * (m * 0.5));
                l = ll;
                r = rr;
            }

            if run_cross {
                let g = cross_start + cross_step * progress;
                let write = self.delay_write;
                let read = (write + CROSSFEED_BUF - self.delay_frames) % CROSSFEED_BUF;
                let (dl, dr) = (f64::from(self.delay[0][read]), f64::from(self.delay[1][read]));
                self.delay[0][write] = l as f32;
                self.delay[1][write] = r as f32;
                self.delay_write = (write + 1) % CROSSFEED_BUF;
                // Feed the delayed side difference across, low-passed. A centered signal has no
                // difference and stays flat, while a hard-panned one reaches the far ear.
                let c = self.lp_coef;
                self.lp_diff = (1.0 - c) * (dr - dl) + c * self.lp_diff;
                l += g * self.lp_diff;
                r -= g * self.lp_diff;
            }

            let b = bal_start + bal_step * progress;
            if b > 0.0 {
                l *= 1.0 - b;
            } else if b < 0.0 {
                r *= 1.0 + b;
            }

            if limiter_on || self.limiter_gain != 1.0 {
                let want = if limiter_on { limit_gain(l.abs().max(r.abs())) } else { 1.0 };
                let mut g = self.limiter_gain;
                g = if want < g { want } else { (g + (1.0 - g) * lim_step).min(want) };
                if g > 0.999_999 {
                    g = 1.0;
                }
                self.limiter_gain = g;
                l *= g;
                r *= g;
            }

            self.block[f * 2] = l;
            self.block[f * 2 + 1] = r;
        }
        self.limiter_on = limiter_on;
    }
}

/// Gain that brings a peak under the soft knee's curve, never past 1.0.
fn limit_gain(peak: f64) -> f64 {
    if peak <= LIMITER_KNEE {
        return 1.0;
    }
    let room = 1.0 - LIMITER_KNEE;
    let shaped = LIMITER_KNEE + room * ((peak - LIMITER_KNEE) / room).tanh();
    shaped / peak
}
