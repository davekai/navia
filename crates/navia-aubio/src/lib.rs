//! aubio's beat tracker, tempo estimate and onset detector over a buffer of mono samples. aubio is
//! built from the vendored source in `aubio/` (see `build.rs`), single precision.

use std::ffi::{CStr, c_char, c_uint};

/// analysis window and step. half aubio's default step, so its beat period is finer than 1%.
pub const WINDOW: usize = 1024;
pub const HOP: usize = 256;
const METHOD: &CStr = c"specflux";

#[repr(C)]
struct FVec {
    length: c_uint,
    data: *mut f32,
}

#[repr(C)]
struct AubioTempo {
    _private: [u8; 0],
}

#[repr(C)]
struct AubioOnset {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn new_fvec(length: c_uint) -> *mut FVec;
    fn del_fvec(v: *mut FVec);
    fn new_aubio_tempo(method: *const c_char, buf_size: c_uint, hop_size: c_uint, samplerate: c_uint) -> *mut AubioTempo;
    fn aubio_tempo_do(o: *mut AubioTempo, input: *const FVec, out: *mut FVec);
    fn aubio_tempo_get_last_ms(o: *mut AubioTempo) -> f32;
    fn aubio_tempo_get_bpm(o: *mut AubioTempo) -> f32;
    fn aubio_tempo_get_confidence(o: *mut AubioTempo) -> f32;
    fn del_aubio_tempo(o: *mut AubioTempo);
    fn new_aubio_onset(method: *const c_char, buf_size: c_uint, hop_size: c_uint, samplerate: c_uint) -> *mut AubioOnset;
    fn aubio_onset_do(o: *mut AubioOnset, input: *const FVec, out: *mut FVec);
    fn aubio_onset_get_last_ms(o: *const AubioOnset) -> f32;
    fn aubio_onset_get_descriptor(o: *const AubioOnset) -> f32;
    fn del_aubio_onset(o: *mut AubioOnset);
}

struct Owned<T>(*mut T, unsafe extern "C" fn(*mut T));

impl<T> Drop for Owned<T> {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from aubio's matching constructor and is freed once.
            unsafe { (self.1)(self.0) }
        }
    }
}

/// what aubio found in a stretch of audio. times are ms from its start.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rhythm {
    pub beats_ms: Vec<f32>,
    /// aubio's tempo estimate at the end, BPM, 0 when it has none.
    pub bpm: f32,
    pub confidence: f32,
    /// onsets, with the onset detection function's value at each (its strength).
    pub onsets: Vec<(f32, f32)>,
}

/// track beats and onsets in `mono` at `rate`. `None` when aubio can't be set up for this rate.
pub fn rhythm(mono: &[f32], rate: u32) -> Option<Rhythm> {
    // SAFETY: every object is created, used and freed here; vectors are sized as aubio expects
    // (input HOP, outputs 1 and 1) and only HOP samples are ever written into the input.
    unsafe {
        let tempo = Owned(new_aubio_tempo(METHOD.as_ptr(), WINDOW as c_uint, HOP as c_uint, rate), del_aubio_tempo);
        let onset = Owned(new_aubio_onset(METHOD.as_ptr(), WINDOW as c_uint, HOP as c_uint, rate), del_aubio_onset);
        let input = Owned(new_fvec(HOP as c_uint), del_fvec);
        let beat = Owned(new_fvec(1), del_fvec);
        let hit = Owned(new_fvec(1), del_fvec);
        if tempo.0.is_null() || onset.0.is_null() || input.0.is_null() || beat.0.is_null() || hit.0.is_null() {
            return None;
        }
        let mut out = Rhythm::default();
        let samples = std::slice::from_raw_parts_mut((*input.0).data, HOP);
        for chunk in mono.chunks(HOP) {
            samples[..chunk.len()].copy_from_slice(chunk);
            samples[chunk.len()..].fill(0.0);
            aubio_tempo_do(tempo.0, input.0, beat.0);
            if *(*beat.0).data != 0.0 {
                out.beats_ms.push(aubio_tempo_get_last_ms(tempo.0));
            }
            aubio_onset_do(onset.0, input.0, hit.0);
            if *(*hit.0).data != 0.0 {
                out.onsets.push((aubio_onset_get_last_ms(onset.0), aubio_onset_get_descriptor(onset.0)));
            }
        }
        out.bpm = aubio_tempo_get_bpm(tempo.0);
        out.confidence = aubio_tempo_get_confidence(tempo.0);
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_click_track_is_tracked_at_its_tempo() {
        let rate = 44_100u32;
        let beat = 0.5;
        // clicks over a quiet hiss: aubio reports no beats in digital silence.
        let mut state = 0x9e37_79b9u32;
        let mono: Vec<f32> = (0..rate as usize * 20)
            .map(|n| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                let hiss = (state as f32 / u32::MAX as f32 - 0.5) * 0.01;
                let t = n as f64 / f64::from(rate);
                let since = t.rem_euclid(beat);
                let click = if since < 0.02 { ((since * 1000.0 * std::f64::consts::TAU).sin() * (1.0 - since / 0.02)) as f32 * 0.8 } else { 0.0 };
                click + hiss
            })
            .collect();
        let r = rhythm(&mono, rate).unwrap();
        // aubio's own estimate is quantised to whole analysis steps (about 1% here).
        assert!((r.bpm - 120.0).abs() < 3.0, "{} BPM", r.bpm);
        assert!(r.beats_ms.len() > 30, "{} beats", r.beats_ms.len());
        let late: Vec<f32> = r.beats_ms.iter().filter(|b| **b > 5_000.0).map(|b| (b % 500.0).min(500.0 - b % 500.0)).collect();
        assert!(late.iter().all(|off| *off < 30.0), "beats off the clicks: {late:?}");
        assert!(r.onsets.len() > 30, "{} onsets", r.onsets.len());
    }
}
