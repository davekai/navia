//! The account button at the right of the top bar and its menu.

use crate::i18n::tr;
use gpui::prelude::*;
use gpui::{AnyElement, Context, FontWeight, SharedString, div, point};

use crate::focus::FocusStop;
use crate::pages::Route;
use crate::shell::Shell;
use crate::theme::size::*;
use crate::ui::glass_edge;
use crate::view_menu::MenuRow;

/// Where the button is.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AccountPlace {
    TopBar,
}

impl AccountPlace {
    fn key(self) -> &'static str {
        match self {
            AccountPlace::TopBar => "account-button",
        }
    }
}

impl Shell {
    /// The signed-in name and the server host without its scheme, or None before sign-in.
    fn account_identity(&self) -> Option<(SharedString, SharedString)> {
        if self.sample_mode {
            // sample data has no account behind it.
            return Some(("sample".into(), "sample data".into()));
        }
        let lib = self.core.as_ref().map(|c| c.lib.clone())?;
        let host = lib.server_url().split("://").last().unwrap_or(lib.server_url()).trim_end_matches('/').to_string();
        Some((lib.username().to_string().into(), host.into()))
    }

    fn account_initial(name: &str) -> SharedString {
        name.trim().chars().next().map_or_else(|| "?".to_string(), |c| c.to_uppercase().collect()).into()
    }

    pub fn render_account_button(&mut self, place: AccountPlace, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let name = self.account_identity().map(|(n, _)| n).unwrap_or_default();
        let open = self.account_menu_open == Some(place) && self.menu.is_some();
        let bounds = self.bounds_store(place.key());
        div()
            .id(place.key())
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .flex_none()
            .size(t.s(CONTROL_H))
            .rounded_full()
            .map(|d| crate::ui::glass_layers(d, &t, t.s(CONTROL_H / 2.0), GLASS_BLUR, GLASS_SATURATION, t.glass(), t.glass_fill(), glass_edge()))
            .when(open, |d| d.child(div().absolute().inset_0().rounded_full().bg(t.glass_press())))
            .child(crate::view_topbar::track_bounds(bounds))
            .when(!open, |d| d.child(crate::ui::hover_fill(crate::ui::HoverShape::Pill, t.glass_hover(), t.glass_press())))
            .font_weight(FontWeight::SEMIBOLD)
            .text_size(t.text(TEXT_MD))
            .text_color(t.t1())
            .occlude()
            .stop(&t)
            .on_click(cx.listener(move |this, _, _, cx| {
                if this.menu.is_some() && this.account_menu_open == Some(place) {
                    this.close_menu(cx);
                } else {
                    this.open_account_menu(place, cx);
                }
            }))
            .child(Self::account_initial(&name))
            .into_any_element()
    }

    pub fn open_account_menu(&mut self, place: AccountPlace, cx: &mut Context<Self>) {
        let (name, host) = self.account_identity().unwrap_or_default();
        // A developer override or sample data has no account to change.
        let switchable = !self.sample_mode && self.core.as_ref().is_some_and(|c| !c.lib.from_env);
        let reason = if self.sample_mode { tr("Sample data, for testing. The real app shows the server you connect to.") } else { tr("Set by NAVIDROME_URL, a developer override.") };
        let mut rows = vec![
            MenuRow::Header { initial: Self::account_initial(&name), name, host: host.clone() },
            MenuRow::Divider,
            MenuRow::toggle(tr("Private Mode"), "eye-slash-regular", self.private_mode, |this, cx| {
                let on = !this.private_mode;
                this.set_private_mode(on, cx);
            }),
            MenuRow::toggle(tr("Offline"), "cloud-slash-regular", self.offline_switch, |this, cx| {
                let on = !this.offline_switch;
                this.set_offline_switch(on, cx);
            }),
            MenuRow::Divider,
            MenuRow::item(tr("Settings…"), "gear-regular", |this, cx| this.open_settings(cx)).with_shortcut("Ctrl+,"),
        ];
        if switchable {
            rows.push(MenuRow::item(tr("Change Server"), "hard-drives-regular", |this, cx| {
                this.expanded = false;
                this.open_change_server(None, cx);
            }));
            rows.push(MenuRow::item(tr("Sign Out"), "sign-out-regular", move |this, cx| {
                this.confirm = Some(crate::view_connect::Confirm::SignOut { host: host.clone() });
                cx.notify();
            }));
        } else {
            rows.push(MenuRow::disabled(tr("Change Server"), "hard-drives-regular", reason));
            rows.push(MenuRow::disabled(tr("Sign Out"), "sign-out-regular", reason));
        }
        let b = *self.bounds_store(place.key()).borrow();
        let position = point(b.origin.x + b.size.width, b.origin.y + b.size.height + self.theme.s(8.0));
        self.open_menu(rows, position, true, cx);
        self.account_menu_open = Some(place);
    }

    /// Opens Settings. From the expanded player the page would be hidden, so the player collapses first.
    pub fn open_settings(&mut self, cx: &mut Context<Self>) {
        if self.connect.is_some() {
            return;
        }
        self.expanded = false;
        self.menu = None;
        self.sidebar_drawer = false;
        self.navigate(Route::Settings, cx);
        cx.notify();
    }
}
