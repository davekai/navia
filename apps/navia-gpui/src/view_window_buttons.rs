//! The window buttons, on whichever side and in whatever order the desktop says.

use gpui::prelude::*;
#[cfg(not(target_os = "windows"))]
use crate::focus::FocusStop;
use gpui::{AnyElement, Context, Hsla, MouseButton, MouseDownEvent, Rgba, WindowButton, div, rgb, rgba};

use crate::chrome::{ButtonStyle, DoubleClick};
use crate::shell::Shell;
use crate::theme::{WINDOW_BUTTON_HOVER, WINDOW_BUTTON_ON_ART, WINDOW_BUTTON_ON_ART_HOVER, WINDOW_CLOSE};
use crate::ui::icon;
use crate::i18n::{n_, tr};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

struct Look {
    width: f32,
    height: f32,
    glyph: f32,
    gap: f32,
    /// Resting fill on a panel and over artwork, where a bare glyph can't be read on a bright cover.
    rest: (Option<Rgba>, Option<Rgba>),
    hover: (Rgba, Rgba),
    close_hover: Option<Rgba>,
}

fn look(style: ButtonStyle) -> Look {
    match style {
        // Flat until hovered, and red under the pointer on close.
        ButtonStyle::Breeze => Look { width: 26.0, height: 26.0, glyph: 16.0, gap: 4.0, rest: (None, Some(rgba(WINDOW_BUTTON_ON_ART))), hover: (rgba(WINDOW_BUTTON_HOVER), rgba(WINDOW_BUTTON_ON_ART_HOVER)), close_hover: Some(rgb(WINDOW_CLOSE)) },
        // Round, always a subtle disc, no color on close.
        ButtonStyle::Adwaita => Look { width: 27.0, height: 27.0, glyph: 16.0, gap: 9.0, rest: (Some(rgba(0xffffff1a)), Some(rgba(WINDOW_BUTTON_ON_ART))), hover: (rgba(0xffffff29), rgba(WINDOW_BUTTON_ON_ART_HOVER)), close_hover: None },
        ButtonStyle::Neutral => Look { width: 26.0, height: 26.0, glyph: 15.0, gap: 8.0, rest: (Some(rgba(0xffffff0f)), Some(rgba(0x00000033))), hover: (rgba(0xffffff1a), rgba(0x00000052)), close_hover: None },
    }
}

impl Shell {
    fn side_buttons(&self, side: Side) -> Vec<WindowButton> {
        if !self.csd {
            return Vec::new();
        }
        let controls = self.window_controls;
        let row = if side == Side::Left { self.button_layout.left } else { self.button_layout.right };
        row.into_iter()
            .flatten()
            .filter(|b| match b {
                WindowButton::Minimize => controls.minimize,
                WindowButton::Maximize => controls.maximize,
                WindowButton::Close => true,
            })
            .collect()
    }

    pub fn has_window_buttons(&self, side: Side) -> bool {
        !self.side_buttons(side).is_empty()
    }

    /// in tokens, for reserving room around them.
    pub fn window_buttons_width(&self, side: Side) -> f32 {
        let l = look(self.button_style);
        let count = self.side_buttons(side).len() as f32;
        if count == 0.0 { 0.0 } else { count * l.width + (count - 1.0) * l.gap }
    }

    /// `on_art` is true over the dock's or the expanded player's picture.
    pub fn render_window_buttons(&self, side: Side, on_art: bool) -> AnyElement {
        let t = self.theme;
        let buttons = self.side_buttons(side);
        if buttons.is_empty() {
            return div().into_any_element();
        }
        let l = look(self.button_style);
        let breeze = self.button_style == ButtonStyle::Breeze;
        let glyph_color: Hsla = if on_art { t.t_on_art() } else { gpui::rgba(crate::theme::WINDOW_BUTTON_INK).into() };
        let mut row = div().flex().flex_row().items_center().flex_none().gap(t.s(l.gap)).occlude();
        for button in buttons {
            let glyph = match button {
                WindowButton::Close => "nv-win-close",
                WindowButton::Minimize if breeze => "nv-win-min-chevron",
                WindowButton::Minimize => "nv-win-min-line",
                WindowButton::Maximize if self.maximized && breeze => "nv-win-restore-diamond",
                WindowButton::Maximize if self.maximized => "nv-win-restore-squares",
                WindowButton::Maximize if breeze => "nv-win-max-chevron",
                WindowButton::Maximize => "nv-win-max-square",
            };
            let rest = if on_art { l.rest.1 } else { l.rest.0 };
            let hover = if button == WindowButton::Close { l.close_hover.unwrap_or(if on_art { l.hover.1 } else { l.hover.0 }) } else if on_art { l.hover.1 } else { l.hover.0 };
            let el = div()
                .id((button.id(), side as usize))
                .w(t.s(l.width))
                .h(t.s(l.height))
                .flex()
                .items_center()
                .justify_center()
                .flex_none()
                .rounded_full()
                .occlude()
                .when_some(rest, |d, bg| d.bg(bg))
                .hover(move |s| s.bg(hover))
                .tooltip(crate::ui::tip(&t, tr(match button {
                    WindowButton::Close => n_("Close"),
                    WindowButton::Minimize => n_("Minimize"),
                    WindowButton::Maximize if self.maximized => n_("Restore"),
                    WindowButton::Maximize => n_("Maximize"),
                })))
                .child(icon(&t, glyph, l.glyph, glyph_color));
            // On Windows the buttons are reported to the system as caption buttons (HTMINBUTTON, HTMAXBUTTON, HTCLOSE).
            // Windows then handles presses, shows Snap layouts over Maximize, and closes through the same path as
            // Alt+F4.
            #[cfg(target_os = "windows")]
            let el = el.window_control_area(match button {
                WindowButton::Close => gpui::WindowControlArea::Close,
                WindowButton::Minimize => gpui::WindowControlArea::Min,
                WindowButton::Maximize => gpui::WindowControlArea::Max,
            });
            #[cfg(not(target_os = "windows"))]
            let el = match button {
                // Minimize to the tray or quit, per Settings.
                WindowButton::Close => {
                    let this = self.this.clone();
                    el.stop(&t).on_click(move |_, window, cx| {
                        if this.update(cx, |shell, cx| shell.main_window_closing(window, cx)).unwrap_or(true) {
                            window.remove_window();
                        }
                    })
                }
                WindowButton::Minimize => el.stop(&t).on_click(|_, window, _| window.minimize_window()),
                WindowButton::Maximize => el.stop(&t).on_click(|_, window, _| window.zoom_window()),
            };
            row = row.child(el);
        }
        row.into_any_element()
    }

    /// Makes an area act as a title bar in Navia's own frame. A press moves the window, a double-click does the
    /// desktop's action, and a right-click opens the window menu. Under the desktop's own title bar it does nothing.
    ///
    /// On Windows the area is reported as the caption (HTCAPTION), so Windows handles moving, Aero Snap, double-click
    /// and the system menu.
    pub fn drag_area<E: InteractiveElement>(element: E, cx: &mut Context<Self>) -> E {
        #[cfg(target_os = "windows")]
        let element = element.window_control_area(gpui::WindowControlArea::Drag);
        element
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this: &mut Self, event: &MouseDownEvent, window, cx| {
                    // Windows handles caption presses itself, and taking this one would stop it.
                    if !this.csd || cfg!(target_os = "windows") {
                        return;
                    }
                    // Where drag areas overlap, like the dock head over the band, one press does one thing.
                    cx.stop_propagation();
                    if event.click_count == 2 {
                        match crate::chrome::double_click() {
                            DoubleClick::Maximize => window.zoom_window(),
                            DoubleClick::Minimize => window.minimize_window(),
                            DoubleClick::Menu => window.show_window_menu(event.position),
                            DoubleClick::Nothing => {}
                        }
                    } else {
                        // Start the move on the press, like any title bar. Waiting for the first motion would lose the
                        // distance covered, so the window would trail the pointer. Double-clicks still arrive because
                        // the compositor ends the move on release.
                        window.start_window_move();
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this: &mut Self, event: &MouseDownEvent, window, cx| {
                    if this.csd && this.window_controls.window_menu && cfg!(not(target_os = "windows")) {
                        cx.stop_propagation();
                        window.show_window_menu(event.position);
                    }
                }),
            )
    }
}
