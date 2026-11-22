//! moving covers (animated GIF or WebP) for large artwork. each is its own view, so a frame redraws only its slot.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{AnyElement, Context, Entity, Pixels, RenderImage, SharedString, Task, Window};

use crate::shell::Shell;

/// where a cover is drawn, so the same cover in two places gets two views.
pub type AnimKey = (SharedString, u32, &'static str);

pub enum AnimEntry {
    Loading,
    Still { asked: u64 },
    Moving { view: Entity<AnimCover>, asked: u64 },
}

#[derive(Default)]
pub struct AnimCache {
    map: HashMap<AnimKey, AnimEntry>,
}

pub struct AnimCover {
    frames: Vec<Arc<RenderImage>>,
    delays: Vec<Duration>,
    ix: usize,
    /// `None` is a circle.
    radius: Option<Pixels>,
    drawn: bool,
    timer: Option<Task<()>>,
}

impl AnimCover {
    fn tick(&mut self, cx: &mut Context<Self>) {
        self.timer = None;
        if !std::mem::replace(&mut self.drawn, false) {
            return;
        }
        self.ix = (self.ix + 1) % self.frames.len();
        cx.notify();
    }
}

impl Render for AnimCover {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if crate::theme::motion::reduced() {
            self.ix = 0;
            self.timer = None;
        } else if self.timer.is_none() && self.frames.len() > 1 {
            let delay = self.delays[self.ix];
            self.timer = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(delay).await;
                let _ = this.update(cx, |this, cx| this.tick(cx));
            }));
        }
        self.drawn = true;
        let el = crate::ui::image(self.frames[self.ix].clone()).size_full();
        match self.radius {
            Some(r) => el.rounded(r),
            None => el.rounded_full(),
        }
    }
}

impl Shell {
    /// the moving cover at `px` device pixels, if it moves; until it arrives the caller shows the still image.
    pub fn moving_cover(&mut self, key: &SharedString, px: u32, site: &'static str, radius: Option<Pixels>, cx: &mut Context<Self>) -> Option<AnyElement> {
        if key.is_empty() {
            return None;
        }
        let frame = self.frame;
        let id: AnimKey = (key.clone(), px, site);
        match self.anim.map.get_mut(&id) {
            Some(AnimEntry::Moving { view, asked }) => {
                *asked = frame;
                return Some(view.clone().into_any_element());
            }
            Some(AnimEntry::Still { asked }) => {
                *asked = frame;
                return None;
            }
            Some(AnimEntry::Loading) => return None,
            None => {}
        }
        let library = self.library.clone()?;
        self.anim.map.insert(id.clone(), AnimEntry::Loading);
        let lookup = key.to_string();
        cx.spawn(async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
            let frames = cx
                .background_executor()
                .spawn(async move {
                    let art = library.animated_cover(&lookup, px)?;
                    let delays = art.delays_ms.iter().map(|ms| Duration::from_millis(u64::from(*ms))).collect::<Vec<_>>();
                    let frames = art.frames().ok()?.iter().map(|f| crate::art::thumbnail(f, px)).collect::<Vec<_>>();
                    (frames.len() > 1 && frames.len() == delays.len()).then_some((frames, delays))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let asked = this.frame;
                let entry = match frames {
                    Some((frames, delays)) => AnimEntry::Moving { view: cx.new(|_| AnimCover { frames, delays, ix: 0, radius, drawn: false, timer: None }), asked },
                    None => AnimEntry::Still { asked },
                };
                this.anim.map.insert(id, entry);
                this.notify_visual(cx);
            });
        })
        .detach();
        None
    }

    /// frees the frames of moving covers that weren't drawn in the last full frames.
    pub fn evict_moving_covers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let frame = self.frame;
        self.anim.map.retain(|_, entry| match entry {
            AnimEntry::Moving { view, asked } if *asked + 2 < frame => {
                for image in view.update(cx, |v, _| std::mem::take(&mut v.frames)) {
                    let _ = window.drop_image(image);
                }
                false
            }
            // a still answer is cheap to keep, but not forever.
            AnimEntry::Still { asked } => *asked + 600 >= frame,
            _ => true,
        });
    }
}
