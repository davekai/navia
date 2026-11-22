//! The glass sidebar. Content passing behind it, on shelf pages or in the narrow-window drawer, gets a real backdrop
//! blur. Elsewhere it's just the fill.

use crate::i18n::{tr, trf};
use gpui::prelude::*;
use crate::focus::FocusStop;
use gpui::{AnyElement, Context, FontWeight, div, px};

use crate::pages::{AlbumScope, DiscoverTab};
use crate::shell::{Layout, Route, Shell};
use crate::sidebar_playlists::SidebarGroup;
use crate::theme::size::*;
use crate::ui::{icon, icon_button};
use crate::view_window_buttons::Side;

fn top_items() -> [(&'static str, &'static str, Route); 5] {
    [
        ("magnifying-glass-regular", tr("Search"), Route::Browse),
        ("house-regular", tr("Home"), Route::Home),
        ("star-four-regular", tr("For You"), Route::Discover(DiscoverTab::ForYou)),
        ("squares-four-regular", tr("New"), Route::Discover(DiscoverTab::New)),
        ("broadcast-regular", tr("Radio"), Route::Discover(DiscoverTab::Radio)),
    ]
}

/// Liked Songs comes first. Recently Added is a sort on Albums and Songs, not an item here.
fn library_items() -> [(&'static str, &'static str, Route); 5] {
    [
        ("heart-regular", tr("Liked Songs"), Route::Liked),
        ("music-note-regular", tr("Songs"), Route::Songs),
        ("microphone-stage-regular", tr("Artists"), Route::Artists),
        ("stack-regular", tr("Albums"), Route::Albums(AlbumScope::All)),
        ("guitar-regular", tr("Genres"), Route::Genres),
    ]
}

impl Shell {
    fn nav_item(&mut self, id: (&'static str, usize), name: &str, label: &'static str, route: Route, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let active = self.route == route || (matches!(route, Route::Browse) && self.route == Route::Search);
        let ink = if active { t.t_em() } else { t.t_nav() };
        div()
            .id(id)
            .flex()
            .flex_row()
            .items_center()
            .gap(t.s(12.0))
            .h(t.s(45.0))
            .px(t.s(SIDEBAR_ITEM_PAD_X))
            .rounded(t.s(R_MD))
            .when(active, |d| d.bg(t.hover()))
            .when(!active, |d| d.hover(|s| s.bg(t.row())))
            .stop(&t).on_click(cx.listener(move |this, _, _, cx| {
                this.navigate(route.clone(), cx);
                this.sidebar_drawer = false;
            }))
            .child(icon(&t, name, 22.0, ink))
            .child(div().truncate().text_color(ink).when(active, |d| d.font_weight(SIDEBAR_ACTIVE_WEIGHT)).child(label))
            .into_any_element()
    }

    /// A heading line without a toggle, as used by Listening Now.
    pub(crate) fn heading_line(&self, title: &'static str) -> gpui::Div {
        let t = self.theme;
        div()
            .flex()
            .flex_row()
            .justify_between()
            .items_center()
            .pt(t.s(SIDEBAR_HEADING_TOP))
            .pr(t.s(12.0))
            .pb(t.s(SIDEBAR_HEADING_PAD_Y))
            .pl(t.s(SIDEBAR_ITEM_PAD_X))
            .text_size(t.text(TEXT_SM))
            .text_color(t.t_nav_section())
            .font_weight(FontWeight::SEMIBOLD)
            .child(title)
    }

    /// A group's heading. Its chevron folds and unfolds the group.
    fn heading(&mut self, group: usize, title: &'static str, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let open = !self.sidebar_folded[group];
        let label = trf(if open { "Collapse {section}" } else { "Expand {section}" }, &[("section", title)]);
        let row_group = gpui::SharedString::from(format!("sidebar-fold-{group}"));
        self.heading_line(title)
            .id(("sidebar-fold", group))
            .group(row_group.clone())
            .mt(t.s(SIDEBAR_HEADING_TOP - SIDEBAR_HEADING_PAD_Y))
            .pt(t.s(SIDEBAR_HEADING_PAD_Y))
            .rounded(t.s(R_MD))
            .hover(|s| s.bg(t.row()))
            .tooltip(crate::ui::tip(&t, label))
            .stop(&t)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.sidebar_folded[group] = !this.sidebar_folded[group];
                cx.notify();
            }))
            .child(icon(&t, if open { "caret-down-bold" } else { "caret-right-bold" }, 11.0, t.t_nav_section()).group_hover(row_group, move |s| s.text_color(t.t1())))
            .into_any_element()
    }

    /// The resize strip on the sidebar's right edge. Drag to resize, double-click for the default.
    pub fn render_sidebar_grab(&mut self, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let width = t.s(SIDEBAR_GRAB_W);
        let right = t.s(WINDOW_INSET) + layout.sidebar_width;
        div()
            .id("sidebar-grab")
            .absolute()
            .left(right - width / 2.0)
            .top(t.s(WINDOW_INSET))
            .bottom(t.s(WINDOW_INSET))
            .w(width)
            .cursor(gpui::CursorStyle::ResizeColumn)
            .on_mouse_down(gpui::MouseButton::Left, cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                cx.stop_propagation();
                if event.click_count >= 2 {
                    this.dragging = None;
                    this.reset_sidebar_width(cx);
                } else {
                    this.begin_sidebar_drag(event.position.x - this.edge.left, this.edge.content_size(window.viewport_size()), window.scale_factor());
                    cx.notify();
                }
            }))
            .into_any_element()
    }

    pub fn render_sidebar(&mut self, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let tokens_w = layout.tokens_w;
        let mut scroll = div().id("sidebar-scroll").size_full().overflow_y_scroll().track_scroll(&self.sidebar_scroll).px(t.s(12.0)).pb(t.s(12.0));
        let mut group = div().flex().flex_col();
        for (i, (name, label, route)) in top_items().into_iter().enumerate() {
            group = group.child(self.nav_item(("nav-top", i), name, label, route, cx));
        }
        scroll = scroll.child(group).child(self.heading(0, tr("Library"), cx));
        if !self.sidebar_folded[0] {
            let mut group = div().flex().flex_col();
            for (i, (name, label, route)) in library_items().into_iter().enumerate() {
                group = group.child(self.nav_item(("nav-lib", i), name, label, route, cx));
            }
            scroll = scroll.child(group);
        }
        // Listening Now goes between Library and Playlists while anyone is listed.
        if let Some(listening) = self.render_listening_now(layout, cx) {
            scroll = scroll.child(listening);
        }
        scroll = scroll.child(self.heading(1, tr("Playlists"), cx));
        if !self.sidebar_folded[1] {
            let mut group = div().flex().flex_col();
            group = group.child(self.nav_item(("nav-all-playlists", 0), "playlist-regular", tr("All Playlists"), Route::Playlists, cx));
            group = group.child(self.render_sidebar_playlists(SidebarGroup::Mine, layout, cx));
            scroll = scroll.child(group);
        }
        // Other people's public playlists. Hidden when there are none.
        if !self.sidebar_shared.is_empty() {
            scroll = scroll.child(self.heading(2, tr("Shared||playlists"), cx));
            if !self.sidebar_folded[2] {
                scroll = scroll.child(div().flex().flex_col().child(self.render_sidebar_playlists(SidebarGroup::Shared, layout, cx)));
            }
        }

        let top = div()
            .id("sidebar-top")
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .h(t.s(62.0))
            .pl(t.s(14.0))
            .pr(t.s(12.0))
            .flex_none()
            .map(|d| Shell::drag_area(d, cx))
            .child({
                // When the window buttons are on the right, the back and forward pill takes this corner.
                let mut left = div().flex().flex_row().items_center().child(self.render_window_buttons(Side::Left, false));
                if self.nav_pill_in_sidebar(layout) {
                    left = left.child(self.render_nav_pill(cx));
                }
                left
            })
            .child(icon_button(&t, "hide-sidebar", tr("Hide sidebar"), "sidebar-simple-regular", false).occlude().stop(&t).on_click(cx.listener(move |this, _, _, cx| this.toggle_sidebar(tokens_w, cx))));

        let offline = (self.core.is_some() && self.is_offline()).then(|| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(t.s(10.0))
                .mx(t.s(12.0))
                .mt(t.s(6.0))
                .px(t.s(12.0))
                .py(t.s(10.0))
                .rounded(t.s(R_MD))
                .bg(t.row())
                .child(icon(&t, "cloud-slash-fill", 20.0, t.t_nav()))
                .child(div().flex_1().min_w(px(0.0)).line_height(gpui::relative(1.25)).child(div().font_weight(FontWeight::MEDIUM).child(tr("Offline"))).child(div().text_size(t.text(TEXT_XS)).text_color(t.t_nav()).child(tr("Downloads only"))))
                .mb(t.s(12.0))
                // the switch is the way back, so no Retry while it's on
                .when(!self.offline_switch, |d| d.child(
                    div()
                        .id("offline-retry")
                        .flex_none()
                        .flex()
                        .items_center()
                        .h(t.s(30.0))
                        .px(t.s(12.0))
                        .rounded_full()
                        .bg(t.glass_fill())
                        .shadow(crate::ui::glass_edge())
                        .text_size(t.text(TEXT_XS))
                        .font_weight(FontWeight::MEDIUM)
                        .stop(&t).on_click(cx.listener(|this, _, _, cx| this.core_retry_connection(cx)))
                        .child(tr("Retry")),
                ))
        });

        let inset = t.s(WINDOW_INSET);
        let radius = t.s(R_LG);
        // Shelf pages and the drawer have something bright under the sidebar, so it gets real blur and dark glass to
        // keep labels readable.
        let frosted = crate::ui::has_backdrop_blur() && (layout.sidebar_overlay || self.page.blocks.iter().any(|block| matches!(block, crate::pages::Block::Shelf(_))));
        let glass = frosted || matches!(self.route, Route::Artist(_) | Route::ArtistTop(_) | Route::Album(_) | Route::Playlist(_) | Route::Upcoming(_));
        let fill = if glass { t.sidebar_glass() } else { t.panel_glass() };
        div()
            .id("sidebar")
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .w(layout.sidebar_width)
            .rounded(radius)
            .when(layout.sidebar_overlay, |d| d.ml(inset).my(inset))
            .when(!layout.sidebar_overlay, |d| crate::animation::at_left(d.absolute(), layout.sidebar_room - layout.sidebar_width, inset, layout.scale).top(inset).bottom(inset))
            .occlude()
            .overflow_hidden()
            // Songs dragged over the sidebar leave their list, whether or not a playlist is under them.
            .children(self.drop_mark("sidebar", crate::song_drop::SongTarget::Surface, R_LG, 0.0))
            .when(frosted, |d| d.child(crate::ui::backdrop_blur(gpui::Corners::all(radius), t.s(SIDEBAR_BLUR), SIDEBAR_SATURATION, px(0.0))))
            .child(
                // Inset rings paint under children, so the fill and ring get their own layer over the blur.
                div().absolute().inset_0().rounded(radius).bg(fill).shadow(vec![gpui::BoxShadow {
                    color: t.line().into(),
                    offset: gpui::point(px(0.0), px(0.0)),
                    blur_radius: px(0.0),
                    spread_radius: px(1.0),
                    inset: true,
                }]),
            )
            .child(top)
            .child({
                let bar = self.render_scrollbar("sidebar", crate::ui_scroll::ScrollSource::Handle(self.sidebar_scroll.clone()), px(0.0), px(0.0), cx);
                let lifted = self.render_sidebar_lifted(layout);
                div().relative().flex_1().min_h(px(0.0)).child(scroll).child(bar).children(lifted)
            })
            // Settings is in the account menu, so the playlists end the sidebar.
            .children(self.render_sync_card())
            .children(offline)
            .into_any_element()
    }

    pub fn render_sidebar_overlay(&mut self, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let sidebar = self.render_sidebar(layout, cx);
        div()
            .absolute()
            .inset_0()
            .flex()
            .flex_row()
            .child(
                div().absolute().inset_0().id("drawer-scrim").occlude().bg(self.theme.scrim())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.sidebar_drawer = false;
                        cx.notify();
                    })),
            )
            .child(sidebar)
            .into_any_element()
    }
}
