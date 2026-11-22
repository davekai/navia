//! the album table's Heat column: Deezer's `rank` as five thin bars against the
//! table's own range, the hottest song full and tinted, with a light passing across it while hovered or playing.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{AnyElement, Bounds, Context, Hsla, Pixels, Window, canvas, div, fill, linear_color_stop, linear_gradient, point, px, size};

use crate::i18n::{tr, trf};
use crate::shell::Shell;
use crate::theme::Theme;
use crate::theme::motion;
use crate::theme::size::{POPULARITY_BAR_GAP, POPULARITY_BAR_H, POPULARITY_BAR_W};

pub const BARS: usize = 5;
/// Deezer's `rank` tops out near a million.
const RANK_MAX: f64 = 1_000_000.0;
/// the table's least popular song keeps a bar, so it doesn't read as unknown.
const FLOOR: f64 = 0.15;

/// the ranks of a table's songs (song index to rank): its lowest and highest, and each song's place (ties share one).
#[derive(Clone, Default)]
pub struct HeatScale {
    ranks: HashMap<u32, u64>,
    low: u64,
    top: u64,
}

impl HeatScale {
    pub fn new(ranks: HashMap<u32, u64>) -> Self {
        let low = ranks.values().copied().min().unwrap_or(0);
        let top = ranks.values().copied().max().unwrap_or(0);
        Self { ranks, low, top }
    }

    pub fn is_empty(&self) -> bool {
        self.ranks.is_empty()
    }

    /// bars lit for a song. all five are kept for the table's most popular songs, so a full meter always means the hottest.
    pub fn lit(&self, song: u32) -> Option<usize> {
        let rank = *self.ranks.get(&song)?;
        if rank >= self.top {
            return Some(BARS);
        }
        // Deezer's ranks bunch near the top on a popular album, so the table's own range is stretched over the meter.
        let share = if self.top > self.low { FLOOR + (1.0 - FLOOR) * ((rank - self.low) as f64 / (self.top - self.low) as f64) } else { 1.0 };
        Some(((share * BARS as f64).ceil() as usize).clamp(1, BARS - 1))
    }

    /// "#2 of 10 on this album · Very popular on Deezer".
    pub fn tip(&self, song: u32) -> Option<String> {
        let rank = *self.ranks.get(&song)?;
        let place = 1 + self.ranks.values().filter(|r| **r > rank).count();
        Some(trf("#{place} of {count} on this album · {level}", &[("place", &place.to_string()), ("count", &self.ranks.len().to_string()), ("level", level(rank))]))
    }
}

/// how popular a song is on Deezer as a whole, in five steps.
fn level(rank: u64) -> &'static str {
    match (rank as f64 / RANK_MAX * BARS as f64).ceil() as usize {
        5.. => tr("Very popular on Deezer"),
        4 => tr("Popular on Deezer"),
        3 => tr("Fairly popular on Deezer"),
        2 => tr("Less popular on Deezer"),
        _ => tr("Little known on Deezer"),
    }
}

/// how the sweep stands `elapsed` seconds after it started: each bar's light (0 to 1), and how long until it next changes.
pub fn sweep(elapsed: f32) -> ([f32; BARS], f32) {
    let period = motion::POPULARITY_HEAT_SWEEP;
    let peak = motion::HEAT_FLASH_PEAK * period;
    let end = motion::HEAT_FLASH_END * period;
    let mut light = [0.0; BARS];
    let mut wake = f32::MAX;
    for (i, l) in light.iter_mut().enumerate() {
        let delay = i as f32 * motion::POPULARITY_HEAT_SWEEP_STEP;
        let at = (elapsed - delay).max(0.0);
        let phase = at % period;
        if elapsed < delay {
            wake = wake.min(delay - elapsed);
        } else if phase < peak {
            *l = motion::cubic_bezier(motion::EASE, phase / peak);
            wake = 0.0;
        } else if phase < end {
            *l = 1.0 - motion::cubic_bezier(motion::EASE, (phase - peak) / (end - peak));
            wake = 0.0;
        } else {
            wake = wake.min(period - phase);
        }
    }
    (light, wake)
}

/// how the meter is drawn: lit bars, the hottest song's tint (its bars run from it to white), the playing row, and the sweep's light per bar.
#[derive(Clone, Copy)]
pub struct Meter {
    pub lit: usize,
    pub hot: Option<Hsla>,
    pub now: bool,
    pub light: [f32; BARS],
}

/// every bar, gap and the origin on whole device pixels at the window's scale, so the 2 px bars stay crisp at any zoom.
pub fn meter(t: &Theme, m: Meter) -> impl IntoElement {
    let (bar_w, gap, bar_h) = (t.s(POPULARITY_BAR_W), t.s(POPULARITY_BAR_GAP), t.s(POPULARITY_BAR_H));
    let (on, now_on, off, top) = (t.popularity_on(), t.popularity_on_now(), t.popularity_off(), t.popularity_heat_top());
    let width = (bar_w + gap) * BARS as f32 - gap;
    canvas(|_, _, _| {}, move |bounds: Bounds<Pixels>, _, window: &mut Window, _| {
        let scale = window.scale_factor();
        let device = |v: Pixels| (f32::from(v) * scale).round().max(1.0);
        let (w, g, h) = (device(bar_w), device(gap), device(bar_h));
        let x0 = (f32::from(bounds.origin.x) * scale).round();
        let y0 = ((f32::from(bounds.origin.y) + (f32::from(bounds.size.height) - h / scale) / 2.0) * scale).round();
        let lit_color = if m.now { now_on } else { on };
        for i in 0..BARS {
            let rect = Bounds { origin: point(px((x0 + i as f32 * (w + g)) / scale), px(y0 / scale)), size: size(px(w / scale), px(h / scale)) };
            let radius = px(w / 2.0 / scale);
            let quad = match m.hot {
                Some(foot) => fill(rect, linear_gradient(0.0, linear_color_stop(foot, 0.0), linear_color_stop(top, 1.0))),
                None => fill(rect, if i < m.lit { lit_color } else { off }),
            };
            window.paint_quad(quad.corner_radii(radius));
            if m.light[i] > 0.0 {
                window.paint_quad(fill(rect, Hsla { a: top.a * m.light[i], ..top }).corner_radii(radius));
            }
        }
    })
    .w(width)
    .h(bar_h)
}

/// the album page's Heat: its table's scale and cover tint, which rows are hovered, and each hot row's sweep.
#[derive(Default)]
pub struct PageHeat {
    pub scale: HeatScale,
    pub foot: Option<Hsla>,
    pub hovered: Option<usize>,
    sweeps: HashMap<usize, Instant>,
    wake: Option<Instant>,
}

impl PageHeat {
    pub fn new(scale: HeatScale, foot: Hsla) -> Self {
        Self { scale, foot: Some(foot), ..Default::default() }
    }
}

/// a redraw while a light is passing.
const FRAME: f32 = 1.0 / 60.0;

impl Shell {
    /// a row's Heat meter with its tooltip, or nothing for a song Deezer has no match for.
    pub fn heat_cell(&mut self, pos: usize, song: u32, now: bool, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let heat = &mut self.page_heat;
        let (Some(lit), Some(tip)) = (heat.scale.lit(song), heat.scale.tip(song)) else { return div().into_any_element() };
        let hot = lit == BARS;
        let sweeping = hot && !motion::reduced() && self.window_shown && (now || heat.hovered == Some(pos));
        let mut light = [0.0; BARS];
        if sweeping {
            let started = *heat.sweeps.entry(pos).or_insert_with(Instant::now);
            let (l, wake) = sweep(started.elapsed().as_secs_f32());
            light = l;
            self.wake_heat(wake.max(FRAME), cx);
        } else {
            heat.sweeps.remove(&pos);
        }
        let foot = self.page_heat.foot.unwrap_or(t.popularity_heat_foot());
        div()
            .id(("heat", pos))
            .flex()
            .items_center()
            .tooltip(crate::ui::tip(&t, tip))
            .child(meter(&t, Meter { lit, hot: hot.then_some(foot), now, light }))
            .into_any_element()
    }

    /// one redraw when the next light starts or moves, never sooner than one already asked for.
    fn wake_heat(&mut self, after: f32, cx: &mut Context<Self>) {
        let at = Instant::now() + Duration::from_secs_f32(after);
        if self.page_heat.wake.is_some_and(|w| w <= at) {
            return;
        }
        self.page_heat.wake = Some(at);
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs_f32(after)).await;
            let _ = this.update(cx, |this, cx| {
                if this.page_heat.wake == Some(at) {
                    this.page_heat.wake = None;
                    this.notify_visual(cx);
                }
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_top_songs_fill_the_meter() {
        let scale = HeatScale::new(HashMap::from([(1, 960_000), (2, 960_000), (3, 950_000), (4, 750_000)]));
        assert_eq!((scale.lit(1), scale.lit(2)), (Some(BARS), Some(BARS)), "ties share the top");
        assert_eq!(scale.lit(3), Some(BARS - 1), "close to the top still stops at four");
        assert_eq!(scale.lit(4), Some(1), "the album's least popular keeps a bar");
        assert_eq!(scale.lit(9), None, "no match, no meter");
        assert!(scale.tip(3).unwrap().starts_with("#3 of 4 on this album"));
        assert!(scale.tip(1).unwrap().starts_with("#1 of 4"));
    }

    #[test]
    fn the_light_passes_bar_by_bar_then_rests_until_the_next_period() {
        let (light, wake) = sweep(motion::HEAT_FLASH_PEAK * motion::POPULARITY_HEAT_SWEEP);
        assert!(light[0] > 0.99 && light[BARS - 1] < light[0]);
        assert_eq!(wake, 0.0);
        let rest = motion::POPULARITY_HEAT_SWEEP / 2.0;
        let (light, wake) = sweep(rest);
        assert!(light.iter().all(|l| *l == 0.0));
        assert!(wake > 0.0 && wake <= motion::POPULARITY_HEAT_SWEEP - rest + 0.001);
    }
}
