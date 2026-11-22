//! Parts of the window that keep last frame's drawing until the shell changes.
//!
//! GPUI redraws a whole view on every notify, but the lyrics animate for a whole song while the
//! sidebar and main column stay the same. Each of those renders in its own cached view (`Slot`),
//! so a lyrics-only frame replays them instead of rebuilding them.
//!
//! Any `cx.notify()` on the shell redraws every slot, so shell code behaves as if nothing were
//! cached. Only the lyrics animation requests frames through `Ticker`, which redraws the shell's
//! own elements and nothing cached. `Shell::frame` counts full frames only, so per-frame
//! bookkeeping treats cached parts as still drawn.

use gpui::prelude::*;
use gpui::{AnyElement, AnyView, Context, Entity, Pixels, StyleRefinement, Subscription, WeakEntity, Window, div, px};

use crate::shell::{Layout, Shell};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Part {
    Sidebar,
    Main,
    Player,
    Expanded,
}

pub struct Slot {
    shell: WeakEntity<Shell>,
    part: Part,
}

impl Render for Slot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let part = self.part;
        self.shell.update(cx, |shell, cx| shell.render_slot(part, window, cx)).unwrap_or_else(|_| div().into_any_element())
    }
}

pub struct Slots {
    sidebar: Entity<Slot>,
    main: Entity<Slot>,
    player: Entity<Slot>,
    expanded: Entity<Slot>,
    /// The layout of the frame being drawn, for the slots that render after the shell.
    layout: Option<Layout>,
    /// The shell was notified since its last render, so this frame redraws everything.
    full: bool,
    _observer: Subscription,
}

impl Slots {
    pub fn new(cx: &mut Context<Shell>) -> Self {
        let shell = cx.entity().downgrade();
        let sidebar = cx.new(|_| Slot { shell: shell.clone(), part: Part::Sidebar });
        let main = cx.new(|_| Slot { shell: shell.clone(), part: Part::Main });
        let player = cx.new(|_| Slot { shell: shell.clone(), part: Part::Player });
        let expanded = cx.new(|_| Slot { shell, part: Part::Expanded });
        let observer = cx.observe_self(|shell, cx| {
            shell.slots.full = true;
            for slot in [&shell.slots.sidebar, &shell.slots.main, &shell.slots.player, &shell.slots.expanded] {
                slot.update(cx, |_, cx| cx.notify());
            }
        });
        Self { sidebar, main, player, expanded, layout: None, full: true, _observer: observer }
    }

    /// Starts a render of the shell. Returns true for a full frame, when the shell itself changed.
    pub fn begin(&mut self, layout: &Layout) -> bool {
        self.layout = Some(layout.clone());
        std::mem::replace(&mut self.full, false)
    }

    /// the sidebar, cached; it places itself, so the slot covers the shell's root.
    pub fn sidebar_view(&self) -> AnyElement {
        let style = StyleRefinement::default().absolute().inset_0();
        AnyView::from(self.sidebar.clone()).cached(style).into_any_element()
    }

    /// the main column, cached, with the sidebar's and the side player's rooms as its margins.
    pub fn main_view(&self, margin: Pixels, dock_room: Pixels) -> AnyElement {
        let style = StyleRefinement::default().relative().flex_1().min_w(px(0.0)).h_full().ml(margin).mr(dock_room);
        AnyView::from(self.main.clone()).cached(style).into_any_element()
    }

    pub fn player_view(&self, style: StyleRefinement) -> AnyElement {
        AnyView::from(self.player.clone()).cached(style).into_any_element()
    }

    pub fn expanded_view(&self) -> AnyElement {
        AnyView::from(self.expanded.clone()).cached(StyleRefinement::default().absolute().inset_0()).into_any_element()
    }

    pub fn notify_player(&self, cx: &mut gpui::App) {
        self.player.update(cx, |_, cx| cx.notify());
        self.expanded.update(cx, |_, cx| cx.notify());
    }
}

impl Shell {
    /// Reacts to a new playback position. When only the dock or expanded player show it, cached
    /// parts stay as they are. The bottom bar lives in the main column, so then everything redraws.
    /// Skipped while hidden.
    pub fn notify_position(&mut self, cx: &mut Context<Self>) {
        if !self.window_shown {
            return;
        }
        if self.slots.layout.as_ref().is_some_and(|l| l.player) {
            self.slots.notify_player(cx);
        } else {
            cx.notify();
        }
    }

    fn render_slot(&mut self, part: Part, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(layout) = self.slots.layout.clone() else { return div().into_any_element() };
        match part {
            Part::Sidebar => {
                let element = crate::focus::scoped(crate::focus::Scope::Sidebar, crate::focus::SECTION_SIDEBAR, || self.render_sidebar(&layout, cx));
                div().relative().size_full().child(crate::testing::timed(["sidebar.layout", "sidebar.prepaint", "sidebar.paint"], element)).into_any_element()
            }
            Part::Main => {
                let element = crate::focus::scoped(crate::focus::Scope::Main, crate::focus::SECTION_PAGE, || self.render_main_column(&layout, false, window, cx));
                div().relative().size_full().child(crate::testing::timed(["main.layout", "main.prepaint", "main.paint"], element)).into_any_element()
            }
            Part::Player | Part::Expanded => {
                self.marquees_begin(crate::marquee::Host::of(part));
                self.lyrics_animating = false;
                let element = if part == Part::Expanded {
                    crate::focus::scoped(crate::focus::Scope::Expanded, crate::focus::SECTION_PAGE, || self.render_expanded(&layout, cx))
                } else {
                    crate::focus::scoped(crate::focus::Scope::Player, crate::focus::SECTION_PAGE, || self.render_player_forms(&layout, cx))
                };
                self.marquees_end(crate::marquee::Host::of(part), cx);
                // the lyrics glide and sweep with frames for this slot alone.
                if self.window_shown && self.lyrics_animating {
                    window.request_animation_frame();
                }
                element
            }
        }
    }
}
