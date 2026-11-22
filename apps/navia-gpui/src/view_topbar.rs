//! The top bar: back and forward, title, center pill, and the Songs page controls.

use crate::i18n::{tr, trn};
use gpui::prelude::*;
use crate::focus::FocusStop;
use gpui::{AnyElement, Context, FontWeight, Pixels, SharedString, canvas, div, px};

use crate::persist::PlayerMode;
use crate::shell::{Layout, Route, Shell, SortKey};
use crate::theme::size::*;
use crate::ui::{divider, glass_pill, icon, icon_button};
use crate::view_window_buttons::Side;

/// Distance from the window edge to right-hand window buttons, in tokens.
const CORNER_GAP: f32 = 12.0;

impl Shell {
    pub fn render_topbar(&mut self, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let tokens_w = layout.tokens_w;
        let main_tokens = f32::from(layout.main_width) / t.token_scale();
        // Narrow windows give the left group its real width so the center pill can't cover it.
        let tight = main_tokens <= 620.0;
        let crowded = self.search_crowded(main_tokens);
        let mut left = div().flex().flex_row().items_center().gap(t.s(10.0)).min_w(px(0.0)).when(!tight && !crowded, |d| d.flex_1()).when(tight || crowded, |d| d.flex_none());
        if !layout.sidebar_in {
            let controls = self.render_window_buttons(Side::Left, false);
            left = left.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .flex_none()
                    .mr(t.s(8.0))
                    .child(controls)
                    .child(icon_button(&t, "show-sidebar", tr("Show sidebar"), "sidebar-simple-regular", false).occlude().stop(&t).on_click(cx.listener(move |this, _, _, cx| this.toggle_sidebar(tokens_w, cx)))),
            );
        }
        if !self.nav_pill_in_sidebar(layout) {
            left = left.child(self.render_nav_pill(cx));
        }
        let songs_page = self.route == Route::Songs;
        let settings_page = self.route == Route::Settings;
        // the page's title and its detail: whole or not at all, since a cut title says nothing
        let heading: Option<(SharedString, Option<SharedString>)> = if main_tokens <= 620.0 {
            None
        } else if settings_page {
            Some((tr("Settings").into(), None))
        } else if songs_page {
            let count = if self.query.is_empty() && !self.song_filters.is_filtering() {
                trn("{count} song", "{count} songs", self.total)
            } else {
                trn("{count} match", "{count} matches", self.order.len())
            };
            Some((tr("Songs").into(), (main_tokens > 900.0).then(|| count.into())))
        } else {
            self.page_title().map(|title| (title, self.page_title_detail().filter(|_| main_tokens > 900.0)))
        };
        if let Some((title, detail)) = heading {
            let size = t.text(TEXT_MD);
            let need = crate::ui::text_width_app(cx, &title, size, FontWeight::SEMIBOLD).ceil()
                + detail.as_ref().map_or(px(0.0), |d| t.s(TITLE_DETAIL_GAP) + crate::ui::text_width_app(cx, d, size, FontWeight::NORMAL).ceil());
            let store = self.bounds_store("page-title");
            let room = store.borrow().size.width;
            let fits = room + px(0.5) >= need;
            let mut slot = div().relative().flex().flex_row().flex_1().min_w(px(0.0)).h_full().items_center().child(crate::ui::watch_fit(store, need, fits));
            if fits {
                slot = slot
                    .child(div().font_weight(FontWeight::SEMIBOLD).text_color(t.t_em()).flex_none().whitespace_nowrap().child(title))
                    .children(detail.map(|d| div().ml(t.s(TITLE_DETAIL_GAP)).text_color(t.t2()).flex_none().whitespace_nowrap().child(d)));
            }
            left = left.child(slot);
        }

        let center: Option<AnyElement> = if songs_page || settings_page { None } else { self.render_page_center(main_tokens, tokens_w, cx) };
        // crowded, the field fills the middle and both sides keep their own width
        let center = center.map(|c| if crowded { div().flex().flex_1().min_w(px(0.0)).child(c).into_any_element() } else { c });

        let right: AnyElement = if songs_page {
            let search_w = crate::ui::pill_search_width(main_tokens);
            let compact = self.compact;
            let query = self.query.clone();
            let search = self.fitted_search(["search-pill", "search-pill-clear", "songs-query"], tr("Search Songs"), query, search_w, cx);
            div()
                .flex()
                .flex_row()
                .items_center()
                .flex_shrink(1.0)
                .gap(t.s(10.0))
                .child(
                    glass_pill(&t)
                        .occlude()
                        .child(icon_button(&t, "sort", tr("Sort"), "arrows-down-up-regular", false).stop(&t).on_click(cx.listener(|this, event: &gpui::ClickEvent, _, cx| {
                            let position = event.position() + gpui::point(px(0.0), this.theme.s(14.0));
                            this.open_sort_menu(position, cx);
                        }))),
                )
                .when(main_tokens > 620.0, |d| d.child(
                    glass_pill(&t)
                        .occlude()
                        .child(icon_button(&t, "comfortable", tr("Comfortable rows"), if compact { "list-bullets-regular" } else { "list-bullets-fill" }, !compact).stop(&t).on_click(cx.listener(|this, _, _, cx| {
                            this.compact = false;
                            cx.notify();
                        })))
                        .child(icon_button(&t, "compact", tr("Compact rows"), if compact { "rows-fill" } else { "rows-regular" }, compact).stop(&t).on_click(cx.listener(|this, _, _, cx| {
                            this.compact = true;
                            cx.notify();
                        }))),
                ))
                .child(search)
                .into_any_element()
        } else {
            self.render_page_tools(main_tokens, cx)
        };

        let mut bar = div()
            .id("topbar")
            .absolute()
            .top(px(0.0))
            .left(px(0.0))
            .right(px(0.0))
            .h(t.s(TOPBAR_H))
            .pl(t.s(if layout.sidebar_in || self.has_window_buttons(Side::Left) { GUTTER } else { CORNER_GAP }))
            .pr(t.s(GUTTER))
            .flex()
            .flex_row()
            .items_center()
            .gap(t.s(12.0))
            // The bar's empty space belongs to the window, not to what has scrolled under it.
            .block_mouse_except_scroll()
            .map(|d| Shell::drag_area(d, cx))
            .child(left);
        let centred = center.is_some();
        if let Some(c) = center {
            bar = bar.child(c);
        }
        // The account button ends the right group, just left of any corner window buttons.
        let refresh = self.render_refresh(cx);
        let account = self.render_account_button(crate::view_account::AccountPlace::TopBar, cx);
        let right = div().flex().flex_row().items_center().flex_shrink(1.0).gap(t.s(10.0)).child(right).children(refresh).child(account);
        // the bar's full height, so a clip on a tight bar never cuts a focus ring
        let group = div().flex().flex_row().justify_end().items_center().h_full().child(right);
        // with something centred both sides take half; without, the controls come first and the left takes what's left
        let mut group = if crowded { group.flex_none() } else if centred { group.flex_1().min_w(px(0.0)).when(tight, |d| d.overflow_hidden()) } else { group.flex_shrink(1.0).when(tight, |d| d.overflow_hidden()) };
        // with no dock drawn, the top bar carries the corner window buttons and pushes the pills in.
        let corner_buttons = (layout.mode != PlayerMode::Dock || !layout.player) && self.has_window_buttons(Side::Right);
        if corner_buttons {
            bar = bar.pr(t.s(self.window_buttons_width(Side::Right) + CORNER_GAP * 2.0));
            group = group.pr(t.s(4.0));
        }
        let mut bar = bar.child(group);
        if corner_buttons {
            bar = bar.child(div().absolute().top(px(0.0)).bottom(px(0.0)).right(t.s(CORNER_GAP)).flex().items_center().child(self.render_window_buttons(Side::Right, false)));
        }
        bar.into_any_element()
    }

    /// Whether the nav pill sits in the sidebar header's left corner. That happens when the sidebar shows and no window
    /// buttons take that corner. Otherwise it stays in the top bar.
    pub fn nav_pill_in_sidebar(&self, layout: &Layout) -> bool {
        layout.sidebar_in && !self.has_window_buttons(Side::Left)
    }

    pub fn render_nav_pill(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        glass_pill(&t)
            .occlude()
            .child(icon_button(&t, "back", tr("Back"), "caret-left-bold", false).stop(&t).on_click(cx.listener(|this, _, _, cx| this.go_back(cx))))
            .child(divider(&t))
            .child(icon_button(&t, "forward", tr("Forward"), "caret-right-bold", false).stop(&t).on_click(cx.listener(|this, _, _, cx| this.go_forward(cx))))
            .into_any_element()
    }

    pub fn go_back(&mut self, cx: &mut Context<Self>) {
        if self.history_pos > 0 {
            self.history_pos -= 1;
            self.route = self.history[self.history_pos].clone();
            self.page_entered(-1.0);
            self.focus_arrival();
            self.route_arrival(cx);
            cx.notify();
        }
    }

    pub fn go_forward(&mut self, cx: &mut Context<Self>) {
        if self.history_pos + 1 < self.history.len() {
            self.history_pos += 1;
            self.route = self.history[self.history_pos].clone();
            self.page_entered(1.0);
            self.focus_arrival();
            self.route_arrival(cx);
            cx.notify();
        }
    }

    pub fn navigate(&mut self, route: Route, cx: &mut Context<Self>) {
        if self.route != route {
            self.history.truncate(self.history_pos + 1);
            self.history.push(route.clone());
            self.history_pos = self.history.len() - 1;
            self.route = route;
            self.reset_page_state();
            self.page_entered(1.0);
            self.focus_arrival();
            self.route_arrival(cx);
            cx.notify();
        }
    }

    fn page_entered(&mut self, direction: f32) {
        self.page_entry = (self.page_entry.0 + 1, direction);
    }

    /// Pages read fresh each time they're opened.
    fn route_arrival(&mut self, cx: &mut Context<Self>) {
        match &self.route {
            Route::CachedSongs => self.refresh_cached(cx),
            Route::Album(id) => {
                if let Some(library) = &self.library {
                    library.reask_missing_heat(id);
                }
            }
            _ => {}
        }
    }

    /// Browse and Search open with focus in their field.
    fn focus_arrival(&mut self) {
        self.search_active = matches!(self.route, Route::Browse | Route::Search);
    }

    pub fn open_sort_menu(&mut self, position: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        let current = self.sort;
        let rows = SortKey::ALL
            .into_iter()
            .map(|(key, label)| {
                crate::view_menu::MenuRow::check(tr(label), current == key, move |this, cx| {
                    this.sort = key;
                    this.sort_desc = false;
                    this.songs_added_head = false;
                    this.rebuild_order();
                    cx.notify();
                })
            })
            .collect();
        self.open_menu(rows, position, true, cx);
    }

    pub fn render_icon_nav_public(&mut self, main_tokens: f32, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let width = if main_tokens <= 620.0 {
            42.0
        } else if main_tokens <= 900.0 {
            54.0
        } else {
            75.0
        };
        let items: [(&str, &str, Route); 5] = [
            ("house-regular", "house-fill", Route::Home),
            ("flame-regular", "flame-fill", Route::Discover(crate::pages::DiscoverTab::New)),
            ("star-four-regular", "star-four-fill", Route::Discover(crate::pages::DiscoverTab::ForYou)),
            ("nv-library", "nv-library", Route::Albums(crate::pages::AlbumScope::All)),
            ("magnifying-glass-regular", "magnifying-glass-regular", Route::Browse),
        ];
        let mut pill = div()
            .flex()
            .flex_row()
            .items_center()
            .flex_none()
            .h(t.s(PILL_H))
            .p(t.s(4.0))
            .rounded_full()
            .map(|d| crate::ui::glass_layers(d, &t, t.s(PILL_H / 2.0), GLASS_BLUR, GLASS_SATURATION, t.glass(), t.glass_fill(), crate::ui::edge_and(crate::ui::shadow_pill(&t))))
            .occlude();
        for (i, (regular, fill, route)) in items.into_iter().enumerate() {
            let active = match regular {
                "nv-library" => matches!(self.route, Route::Albums(_) | Route::Album(_) | Route::Songs | Route::Liked | Route::Artists | Route::Artist(_) | Route::ArtistTop(_) | Route::Playlists | Route::Playlist(_)),
                "magnifying-glass-regular" => matches!(self.route, Route::Browse | Route::Search),
                "star-four-regular" => matches!(self.route, Route::Discover(tab) if tab != crate::pages::DiscoverTab::New),
                _ => self.route == route,
            };
            let name = if active { fill } else { regular };
            let (segment, ink) = crate::ui::segment(&t, ("nav", i), active, t.t2());
            pill = pill.child(
                segment
                    .w(t.s(width))
                    .h(t.s(48.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .stop(&t)
                    .on_click(cx.listener(move |this, _, _, cx| this.navigate(route.clone(), cx)))
                    .child(icon(&t, name, 24.0, ink)),
            );
        }
        pill.into_any_element()
    }

    pub fn render_bar_glow(&mut self, layout: &Layout) -> AnyElement {
        let t = self.theme;
        let Some((_, _, _, image)) = self.bar_glow.clone() else { return div().into_any_element() };
        crate::ui::image(image).absolute().left(px(0.0)).right(px(0.0)).bottom(px(0.0)).w(layout.main_width).h(t.s(150.0)).opacity(layout.bar_shown).into_any_element()
    }
}


/// Records an element's window bounds so mouse x can be turned into a fraction.
pub fn track_bounds(store: std::rc::Rc<std::cell::RefCell<gpui::Bounds<Pixels>>>) -> impl IntoElement {
    canvas(
        move |bounds, _, _| {
            *store.borrow_mut() = bounds;
        },
        |_, _, _, _| {},
    )
    .absolute()
    .inset_0()
}
