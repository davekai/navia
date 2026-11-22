//! every animated value in one place: eased or sprung toward a target by key, turned around from wherever it is
//! (keeping its speed under a spring), read where it's drawn so the views apply it at paint (`paint_offset`,
//! `paint_scale`, `opacity`) rather than through layout, and moved on by the display's frame callbacks only while
//! something moves in a window that shows. under reduced motion every value is at its target at once.

use std::{cell::RefCell, collections::HashMap, time::Instant};

use gpui::{AnyElement, ElementId, IntoElement, Styled};

use crate::theme::motion;

/// how a value travels to its target.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Curve {
    /// a CSS `cubic-bezier` over `secs`.
    Ease { secs: f32, bezier: [f32; 4] },
    /// a damped spring with period `secs`; `bounce` 0 is critically damped, towards 1 it rings longer.
    Spring { secs: f32, bounce: f32 },
}

impl Curve {
    pub const fn ease(secs: f32, bezier: [f32; 4]) -> Self {
        Curve::Ease { secs, bezier }
    }

    pub const fn spring(secs: f32, bounce: f32) -> Self {
        Curve::Spring { secs, bounce }
    }

    fn slowed(self) -> Self {
        let slow = crate::testing::slow_motion();
        match self {
            Curve::Ease { secs, bezier } => Curve::Ease { secs: secs * slow, bezier },
            Curve::Spring { secs, bounce } => Curve::Spring { secs: secs * slow, bounce },
        }
    }
}

/// a value read this frame, and whether it's still on its way.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Motion {
    pub value: f32,
    pub moving: bool,
}

#[derive(Clone, Copy, Debug)]
struct Track {
    from: f32,
    to: f32,
    /// units per second at `since`, carried into a spring that turns around mid-way.
    velocity: f32,
    since: Instant,
    curve: Curve,
}

impl Track {
    fn at_rest(value: f32, now: Instant) -> Self {
        Track { from: value, to: value, velocity: 0.0, since: now, curve: Curve::ease(0.0, motion::EASE) }
    }

    /// the value and speed `now`, and whether it still moves.
    fn sample(&self, now: Instant) -> (f32, f32, bool) {
        sample(self.curve, self.from, self.to, self.velocity, now.saturating_duration_since(self.since).as_secs_f32())
    }
}

/// a value set off from `from` at `speed` toward `to` along `curve`, `t` seconds in: the value, its speed, and
/// whether it still moves. for animations that keep their own timing, such as staggered lyric lines.
pub fn sample(curve: Curve, from: f32, to: f32, speed: f32, t: f32) -> (f32, f32, bool) {
    if motion::reduced() || (from == to && speed == 0.0) {
        return (to, 0.0, false);
    }
    match curve {
        Curve::Ease { secs, bezier } => {
            let p = motion::progress(t, secs);
            if p >= 1.0 {
                return (to, 0.0, false);
            }
            let span = to - from;
            let value = from + span * motion::cubic_bezier(bezier, p);
            // a finite difference is enough to hand the speed on to a spring
            let step = 1.0 / 240.0;
            let ahead = from + span * motion::cubic_bezier(bezier, motion::progress(t + step, secs));
            (value, (ahead - value) / step, true)
        }
        Curve::Spring { secs, bounce } => {
            let (offset, speed, moving) = spring(from - to, speed, t, secs, bounce);
            if moving { (to + offset, speed, true) } else { (to, 0.0, false) }
        }
    }
}

/// a damped spring's offset from rest and speed `t` seconds after it was let go at `offset` moving at `speed`. with no
/// speed it's `motion::spring` turned around: the same curve and the same moment it counts as settled.
pub fn spring(offset: f32, speed: f32, t: f32, secs: f32, bounce: f32) -> (f32, f32, bool) {
    let omega = std::f32::consts::TAU / secs.max(0.01);
    let zeta = (1.0 - bounce).clamp(0.05, 1.0);
    let decay = zeta * omega;
    if t >= 1000f32.ln() / decay {
        return (0.0, 0.0, false);
    }
    let fade = (-decay * t).exp();
    if zeta < 1.0 {
        let damped = omega * (1.0 - zeta * zeta).sqrt();
        let (sin, cos) = (damped * t).sin_cos();
        let b = (speed + decay * offset) / damped;
        let x = fade * (offset * cos + b * sin);
        let v = fade * ((b * damped - decay * offset) * cos - (offset * damped + decay * b) * sin);
        (x, v, true)
    } else {
        let b = speed + omega * offset;
        let x = fade * (offset + b * t);
        let v = fade * (b - omega * (offset + b * t));
        (x, v, true)
    }
}

thread_local! {
    static TRACKS: RefCell<HashMap<ElementId, Track>> = Default::default();
    /// windows the app counts as hidden (minimized, on another desktop, suspended), where nothing ticks.
    static HIDDEN: RefCell<Vec<gpui::WindowId>> = Default::default();
}

/// tells the tickers whether `window` can be seen; while it can't they ask for no frames.
pub fn set_window_shown(window: gpui::WindowId, shown: bool) {
    HIDDEN.with(|hidden| {
        let mut hidden = hidden.borrow_mut();
        hidden.retain(|id| *id != window);
        if !shown {
            hidden.push(window);
        }
    });
}

fn window_shows(window: &gpui::Window) -> bool {
    let id = window.window_handle().window_id();
    window.is_visible() && HIDDEN.with(|hidden| !hidden.borrow().contains(&id))
}

pub(crate) fn log_enabled() -> bool {
    static LOG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *LOG.get_or_init(|| std::env::var_os("NAVIA_MOTION_LOG").is_some())
}

fn log(key: &ElementId, value: f32, moving: bool) {
    if log_enabled() && moving {
        eprintln!("navia: motion {key:?} {:.2} ms {value:.5}", crate::testing::since_start_ms());
    }
}

fn log_place(edge: &str, whole: gpui::Pixels, fraction: gpui::Pixels) {
    if log_enabled() && fraction != gpui::px(0.0) {
        eprintln!("navia: place {edge} {:.2} ms {:.4} = {} + {:.4}", crate::testing::since_start_ms(), f32::from(whole + fraction), f32::from(whole), f32::from(fraction));
    }
}

/// `key`'s value on its way to `target` along `curve`. a new key starts at its target; a new target sets off from
/// the value (and, for a spring, the speed) it has now.
pub fn animate(key: impl Into<ElementId>, target: f32, curve: Curve) -> Motion {
    let key = key.into();
    let now = Instant::now();
    let curve = curve.slowed();
    let (value, moving) = TRACKS.with(|tracks| {
        let mut tracks = tracks.borrow_mut();
        let track = tracks.entry(key.clone()).or_insert_with(|| Track::at_rest(target, now));
        if track.to != target {
            let (value, velocity, _) = track.sample(now);
            *track = Track { from: value, to: target, velocity, since: now, curve };
        }
        let (value, _, moving) = track.sample(now);
        (value, moving)
    });
    log(&key, value, moving);
    Motion { value, moving }
}

/// sets `key` off from `from` to `to` along `curve`, whatever it was doing; under reduced motion it's at `to`.
pub fn restart(key: impl Into<ElementId>, from: f32, to: f32, curve: Curve) {
    let now = Instant::now();
    let curve = curve.slowed();
    TRACKS.with(|tracks| {
        tracks.borrow_mut().insert(key.into(), Track { from, to, velocity: 0.0, since: now, curve });
    });
}

/// `key`'s value now without changing where it's going; none for a key never animated or dropped.
pub fn current(key: impl Into<ElementId>) -> Option<Motion> {
    let key = key.into();
    let now = Instant::now();
    let motion = TRACKS.with(|tracks| {
        tracks.borrow().get(&key).map(|track| {
            let (value, _, moving) = track.sample(now);
            Motion { value, moving }
        })
    });
    if let Some(motion) = motion {
        log(&key, motion.value, motion.moving);
    }
    motion
}

/// forgets `key`, so its next [`animate`] starts at rest.
pub fn forget(key: impl Into<ElementId>) {
    let key = key.into();
    TRACKS.with(|tracks| tracks.borrow_mut().remove(&key));
}

fn moving(key: &ElementId) -> bool {
    let now = Instant::now();
    TRACKS.with(|tracks| tracks.borrow().get(key).is_some_and(|track| track.sample(now).2))
}

/// asks for the next frame, unless the window can't be seen: for animations that keep their own timing.
pub fn request_frame(window: &mut gpui::Window) {
    if window_shows(window) {
        window.request_animation_frame();
    }
}

/// asks for the next frame while any of `keys` moves and the window shows; add it to the view that reads them.
pub fn ticker<K: Into<ElementId>>(keys: impl IntoIterator<Item = K>) -> AnyElement {
    let keys: Vec<ElementId> = keys.into_iter().map(Into::into).collect();
    gpui::canvas(|_, _, _| {}, move |_, _, window, _| {
        if window_shows(window) && keys.iter().any(moving) {
            window.request_animation_frame();
        }
    })
    .absolute()
    .size_0()
    .into_any_element()
}

/// `x` as a whole device pixel for layout and the rest as a paint offset, so something moving by fractions of a
/// pixel moves smoothly; hit tests stay on the whole pixel.
pub fn split_px(x: gpui::Pixels, scale: f32) -> (gpui::Pixels, gpui::Pixels) {
    let whole = gpui::px((f32::from(x) * scale).floor() / scale);
    (whole, x - whole)
}

/// `x` on a path shifted so that `rest`, where it settles, lands on the whole device pixel layout would round it
/// to: it moves by fractions and comes to rest exactly where it always did.
fn settled(x: gpui::Pixels, rest: gpui::Pixels, scale: f32) -> gpui::Pixels {
    let rounded = gpui::px((f32::from(rest) * scale).round() / scale);
    x + (rounded - rest)
}

/// an element `left` from its parent's left edge, moving by fractions of a pixel, at rest on `rest`'s pixel.
pub fn at_left<E: Styled>(el: E, left: gpui::Pixels, rest: gpui::Pixels, scale: f32) -> E {
    let (whole, fraction) = split_px(settled(left, rest, scale), scale);
    log_place("left", whole, fraction);
    el.left(whole).paint_offset(gpui::point(fraction, gpui::px(0.0)))
}

/// an element `bottom` above its parent's bottom edge, moving by fractions of a pixel, at rest on `rest`'s pixel.
pub fn at_bottom<E: Styled>(el: E, bottom: gpui::Pixels, rest: gpui::Pixels, scale: f32) -> E {
    let (whole, fraction) = split_px(settled(bottom, rest, scale), scale);
    log_place("bottom", whole, fraction);
    el.bottom(whole).paint_offset(gpui::point(gpui::px(0.0), -fraction))
}

/// a child that does nothing: what a ticker is replaced by once nothing moves.
pub fn idle() -> AnyElement {
    gpui::div().into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// a reference spring in the lyrics' own closed form, to check the shared spring against.
    fn lyric_spring(t: f32, secs: f32, bounce: f32) -> (f32, bool) {
        let omega = std::f32::consts::TAU / secs.max(0.01);
        let zeta = (1.0 - bounce).clamp(0.05, 1.0);
        let decay = zeta * omega;
        if t >= 1000f32.ln() / decay {
            return (1.0, false);
        }
        let x = if zeta < 1.0 {
            let damped = omega * (1.0 - zeta * zeta).sqrt();
            1.0 - (-decay * t).exp() * ((damped * t).cos() + decay / damped * (damped * t).sin())
        } else {
            1.0 - (-omega * t).exp() * (1.0 + omega * t)
        };
        (x, true)
    }

    #[test]
    fn spring_from_rest_is_the_lyric_spring() {
        for &(secs, bounce) in &[(0.6f32, 0.15f32), (0.4, 0.0), (0.8, 0.3)] {
            for i in 0..200 {
                let t = i as f32 * 0.005;
                let (old, old_moving) = lyric_spring(t, secs, bounce);
                let (offset, _, moving) = spring(-1.0, 0.0, t, secs, bounce);
                assert_eq!(old_moving, moving, "t {t}");
                let new = if moving { 1.0 + offset } else { 1.0 };
                assert!((old - new).abs() < 1e-5, "t {t}: {old} vs {new}");
            }
        }
    }

    #[test]
    fn spring_speed_is_the_derivative() {
        for &(offset, speed, bounce) in &[(1.0f32, 0.0f32, 0.2f32), (-0.5, 3.0, 0.0), (0.3, -2.0, 0.5)] {
            for i in 1..100 {
                let t = i as f32 * 0.004;
                let h = 1e-3;
                let (before, _, _) = spring(offset, speed, t - h, 0.5, bounce);
                let (after, _, _) = spring(offset, speed, t + h, 0.5, bounce);
                let (_, v, _) = spring(offset, speed, t, 0.5, bounce);
                assert!(((after - before) / (2.0 * h) - v).abs() < 2e-2 * (1.0 + v.abs()), "t {t}");
            }
        }
    }

    #[test]
    fn ease_matches_the_toggle_curve() {
        let now = Instant::now();
        let track = Track { from: 0.0, to: 1.0, velocity: 0.0, since: now, curve: Curve::ease(0.3, motion::EASE) };
        for i in 0..40 {
            let t = i as f32 * 0.01;
            let (value, _, moving) = track.sample(now + std::time::Duration::from_secs_f32(t));
            let expected = motion::cubic_bezier(motion::EASE, motion::progress(t, 0.3));
            assert!((value - expected).abs() < 1e-6, "t {t}");
            assert_eq!(moving, motion::progress(t, 0.3) < 1.0);
        }
    }

    #[test]
    fn retarget_turns_around_from_the_current_value() {
        let key = ElementId::Name("test-retarget".into());
        let start = animate(key.clone(), 0.0, Curve::ease(0.3, motion::EASE));
        assert_eq!(start, Motion { value: 0.0, moving: false });
        animate(key.clone(), 1.0, Curve::ease(10.0, motion::EASE));
        let mid = current(key.clone()).expect("tracked");
        let back = animate(key.clone(), 0.0, Curve::ease(10.0, motion::EASE));
        assert!((back.value - mid.value).abs() < 1e-3);
        assert!(back.moving);
        forget(key);
    }

    #[test]
    fn split_px_keeps_the_sum() {
        for &(x, scale) in &[(10.3f32, 1.0f32), (10.3, 1.25), (-3.7, 2.0), (0.0, 1.5)] {
            let (whole, rest) = split_px(gpui::px(x), scale);
            assert!((f32::from(whole + rest) - x).abs() < 1e-5);
            assert!(f32::from(rest) >= 0.0 && f32::from(rest) * scale < 1.0 + 1e-5);
            assert!(((f32::from(whole) * scale) - (f32::from(whole) * scale).round()).abs() < 1e-4);
        }
    }
}
