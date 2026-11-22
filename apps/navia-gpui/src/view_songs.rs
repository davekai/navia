//! the main column and the Songs table, virtualized with `uniform_list`.

use crate::ui::FullTextTip;
use crate::i18n::tr;
use std::ops::Range;

use crate::view_filters::FilterPage;
use gpui::prelude::*;
use crate::ui::LinkCursor;
use crate::focus::FocusStop;
use gpui::{AnyElement, Context, FontWeight, Hsla, MouseButton, Pixels, Window, div, px, uniform_list};

use crate::persist::PlayerMode;
use crate::shell::{Layout, Route, Shell};
use crate::theme::size::*;
use crate::theme::motion;
use crate::ui::clock;

/// Grid columns of the song table, in pixels.
#[derive(Clone, Copy)]
pub struct Columns {
    width: Pixels,
    lead: Pixels,
    pad_left: Pixels,
    title: Pixels,
    time: Pixels,
    /// Optional columns, indexed by `view_rows::OptCol`.
    cols: [Option<Pixels>; 5],
    actions: Pixels,
}

impl Shell {
    fn columns(&self, layout: &Layout) -> Columns {
        let t = self.theme;
        let gutter = f32::from(t.s(GUTTER));
        let width = f32::from(layout.main_width) - gutter * 2.0;
        let lead = if self.compact { 24.0 } else { 55.0 };
        let c = crate::view_rows::table_cols(&t, width, lead, self.prefs.columns);
        Columns {
            width: px(width),
            pad_left: t.s(5.0),
            lead: t.s(lead),
            title: c.title,
            time: c.time,
            cols: c.cols,
            actions: c.actions,
        }
    }

    /// The main column. `with_margin` is false when the caller leaves room for the sidebar itself.
    pub fn render_main_column(&mut self, layout: &Layout, with_margin: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let content = match self.route {
            Route::Songs => self.render_song_list(layout, window, cx),
            _ => self.render_page(layout, cx),
        };
        let t = self.theme;
        // a new page fades in rising, or falling after Back.
        let (entry, entry_ticker) = crate::ui::swap("page-entry", &self.page_entry.0, motion::PAGE_FADE);
        let content = if entry.leaving.is_some() {
            div().relative().size_full().top(t.s(motion::PAGE_RISE) * self.page_entry.1 * (1.0 - entry.shown)).opacity(entry.shown).child(content).into_any_element()
        } else {
            content
        };
        let sidebar_margin = if with_margin { self.main_column_margin(layout) } else { px(0.0) };
        let mut col = div().relative().flex_1().min_w(px(0.0)).h_full().ml(sidebar_margin).child(entry_ticker).children(self.land_ticker()).child(content);
        if self.content_scrolled() {
            let page = gpui::rgb(crate::theme::BG);
            if crate::ui::has_backdrop_blur() {
                let height = t.s(TOPBAR_H + 20.0);
                let window = self.edge.corners;
                // the band spans the window's whole top so it never ends in an edge beside either panel.
                let left = -self.main_column_margin(layout);
                let right = -layout.dock_room;
                let corners = gpui::Corners { top_left: window.top_left, top_right: window.top_right, bottom_left: px(0.0), bottom_right: px(0.0) };
                col = col
                    .child(div().absolute().top(px(0.0)).left(left).right(right).h(height).child(crate::ui::backdrop_blur(corners, t.s(EDGE_BLUR), EDGE_SATURATION, height * EDGE_FADE)))
                    .child(crate::chrome::round_corners(div().absolute().top(px(0.0)).left(left).right(right).h(height), corners).bg(gpui::linear_gradient(
                        180.0,
                        gpui::linear_color_stop(gpui::Rgba { a: EDGE_TINT, ..page }, 0.3),
                        gpui::linear_color_stop(gpui::Rgba { a: 0.0, ..page }, 1.0),
                    )));
            } else {
            let right = -layout.dock_room;
            col = col.child(crate::chrome::round_corners(div().absolute().top(px(0.0)).left(-self.main_column_margin(layout)).right(right).h(t.s(TOPBAR_H + 12.0)), gpui::Corners { top_left: self.edge.corners.top_left, top_right: self.edge.corners.top_right, bottom_left: px(0.0), bottom_right: px(0.0) }).bg(gpui::linear_gradient(
                180.0,
                gpui::linear_color_stop(page, 0.5),
                gpui::linear_color_stop(gpui::Rgba { a: 0.0, ..page }, 1.0),
            )));
            }
        }
        crate::focus::set_section(crate::focus::SECTION_TOPBAR);
        let topbar = self.render_topbar(layout, cx);
        crate::focus::set_section(crate::focus::SECTION_PAGE);
        let col = if crate::ui::has_backdrop_blur() { col } else { col.child(self.render_shelf_glows(layout, cx)) };
        let mut col = col.child(topbar);
        let page_scroll = match self.route {
            Route::Songs => Some(crate::ui_scroll::ScrollSource::Uniform(self.list_scroll.clone())),
            _ => Some(crate::ui_scroll::ScrollSource::List(self.page.state.clone())),
        };
        if let Some(source) = page_scroll {
            let t = self.theme;
            let bottom = t.s(6.0) + if layout.mode == PlayerMode::Bar && layout.player { t.s(BAR_H) + t.s(WINDOW_INSET) + t.s(12.0) } else { px(0.0) };
            col = col.child(self.render_scrollbar("page", source, t.s(TOPBAR_H), bottom, cx));
        }
        col.into_any_element()
    }

    /// The sidebar's room to the left of the main column.
    pub fn main_column_margin(&self, layout: &Layout) -> Pixels {
        layout.sidebar_room
    }

    /// The main content has moved from its resting position.
    fn content_scrolled(&self) -> bool {
        f32::from(self.list_scroll.0.borrow().base_handle.offset().y) < -0.5 && self.route == Route::Songs || self.page_scrolled()
    }

    fn render_song_list(&mut self, layout: &Layout, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        // The filter chips sit `CHIP_GAP` under the top bar and the table `FILTER_ROW_GAP` under them. Without chips
        // the table starts at the page top.
        let chips_top = t.s(TOPBAR_H) + t.s(CHIP_GAP);
        let top = match self.song_filters.row_height(&t) {
            Some(h) => chips_top + h + t.s(FILTER_ROW_GAP),
            None => t.s(TOPBAR_H) + t.s(CONTENT_TOP),
        };
        let bottom = t.s(48.0) + if layout.mode == PlayerMode::Bar { t.s(BAR_H) + t.s(WINDOW_INSET) + t.s(24.0) } else { px(0.0) };
        // uniform_list renders only `height - padding` of rows and bottom padding doesn't scroll. So the list is made
        // taller by the top padding, clipped by the window, and bottom space comes from blank trailing rows.
        self.trailing_rows = (f32::from(bottom) / f32::from(self.row_height())).ceil() as usize;
        // The chips follow the table's scroll. GPUI adds scroll deltas unclamped and only clamps at layout, after this
        // render, so overscroll at the top would slide the chips over the header. Clamp it here the same way the list
        // will.
        let scrolled = {
            let handle = &self.list_scroll.0.borrow().base_handle;
            handle.offset().y.clamp(-handle.max_offset().y, px(0.0))
        };
        let filtering = self.song_filters.is_filtering();
        let body = if self.total > 0 && self.order.is_empty() && (filtering || !self.query.is_empty()) {
            // Nothing matches. The chips stay put with the empty state under them.
            let empty = self.render_filters_empty(FilterPage::Songs, cx);
            let chips = self.render_filter_row(FilterPage::Songs, layout, window, cx).map(|row| div().absolute().left(px(0.0)).right(px(0.0)).top(chips_top).child(row));
            return div().size_full().relative().child(div().absolute().left(px(0.0)).right(px(0.0)).top(top).child(empty)).children(chips).into_any_element();
        } else if self.order.is_empty() && self.query.is_empty() && !filtering {
            return self.library_empty_state("music-notes-regular", tr("No songs yet"), tr("Songs on the server show up here."), false).size_full().justify_center().into_any_element();
        } else {
            let count = if self.order.is_empty() { 0 } else { self.order.len() + 1 + self.trailing_rows };
            uniform_list("songs-table", count, cx.processor(|this, range: Range<usize>, window, cx| this.render_rows(range, window, cx)))
                .track_scroll(&self.list_scroll)
                .absolute()
                .top(px(0.0))
                .left(px(0.0))
                .right(px(0.0))
                .h(layout.viewport.height + top)
                .pt(top)
                .into_any_element()
        };
        let chips = self.render_filter_row(FilterPage::Songs, layout, window, cx).map(|row| div().absolute().left(px(0.0)).right(px(0.0)).top(chips_top + scrolled).child(row));
        div().size_full().relative().child(body).children(chips).into_any_element()
    }

    fn render_rows(&mut self, range: Range<usize>, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let viewport = self.edge.content_size(window.viewport_size());
        let layout = self.layout(viewport, window.scale_factor());
        let cols = self.columns(&layout);
        let t = self.theme;
        let thumb_px = (f32::from(t.s(THUMB)) * layout.scale).round() as u32;
        range
            .map(|ix| {
                if ix == 0 {
                    self.render_header(cols, cx)
                } else if ix <= self.order.len() {
                    self.render_row(ix - 1, cols, thumb_px, cx)
                } else {
                    div().h(self.row_height()).into_any_element()
                }
            })
            .collect()
    }

    fn row_height(&self) -> Pixels {
        self.theme.s(if self.compact { ROW_COMPACT_H } else { ROW_H })
    }

    fn render_header(&mut self, c: Columns, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        use crate::shell::SortKey;
        use crate::view_rows::SortColumn;
        use crate::view_rows::OptCol;
        // Most Played sorts the Plays column, most first. Date Added is the list's own order and only shows an arrow
        // once its header was clicked.
        let (sorted, ascending) = match self.sort {
            SortKey::Title => (Some(SortColumn::Title), !self.sort_desc),
            SortKey::Artist => (Some(SortColumn::Artist), !self.sort_desc),
            SortKey::Album => (Some(SortColumn::Album), !self.sort_desc),
            SortKey::Length => (Some(SortColumn::Time), !self.sort_desc),
            SortKey::MostPlayed => (Some(SortColumn::Plays), self.sort_desc),
            SortKey::Added if self.songs_added_head => (Some(SortColumn::Added), !self.sort_desc),
            SortKey::Year | SortKey::Added => (None, true),
        };
        // Every header starts at its column's left edge, and only the sorted one is emphasized with its arrow. Clicking
        // cycles ascending, descending, then the list's own order.
        let label = |w: Pixels, text: &'static str, column: SortColumn| {
            let on = sorted == Some(column);
            let mut inner = crate::view_rows::sort_head(&t, ("songs-head", column as usize), text, on.then_some(ascending));
            let key = match column {
                SortColumn::Title => Some(SortKey::Title),
                SortColumn::Artist => Some(SortKey::Artist),
                SortColumn::Album => Some(SortKey::Album),
                SortColumn::Time => Some(SortKey::Length),
                SortColumn::Added => Some(SortKey::Added),
                SortColumn::Plays => Some(SortKey::MostPlayed),
                SortColumn::Genre => None,
            };
            if let Some(key) = key {
                inner = inner.link_cursor().hover(|s| s.text_color(t.t1())).on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    // Ascending, descending, then the list's own order, newest added first.
                    let current = (sorted == Some(column)).then_some(ascending);
                    let next = match current {
                        None => Some(true),
                        Some(true) => Some(false),
                        Some(false) => None,
                    };
                    let (sort, desc, head) = match next {
                        None => (SortKey::Added, false, false),
                        // Most Played's natural direction is most first, so ascending reverses it.
                        Some(asc) => (key, if key == SortKey::MostPlayed { asc } else { !asc }, key == SortKey::Added),
                    };
                    this.sort = sort;
                    this.sort_desc = desc;
                    this.songs_added_head = head;
                    this.rebuild_order();
                    cx.notify();
                }));
            }
            crate::view_rows::head_cell(&t, w, crate::view_rows::is_number(column)).child(inner)
        };
        let mut row = crate::view_rows::table_head_row(&t, self.row_height(), c.width, t.s(GUTTER), c.pad_left).child(label(c.lead + c.title, tr("Song Name"), SortColumn::Title));
        for col in OptCol::ALL {
            if let Some(w) = c.cols[col as usize] {
                row = row.child(label(w, col.head(), col.sort()));
            }
        }
        row = row.child(label(c.time, tr("Time"), SortColumn::Time));
        row.id("songs-header")
            .on_mouse_down(MouseButton::Right, cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| this.open_column_chooser(event.position, cx)))
            .child(div().absolute().left(px(0.0)).right(px(0.0)).bottom(px(0.0)).h(px(1.0)).bg(t.line()))
            .into_any_element()
    }

    fn render_row(&mut self, order_ix: usize, c: Columns, thumb_px: u32, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let song_ix = self.order[order_ix];
        let song = self.song_for_row(order_ix);
        let is_now = self.player.song_ix == Some(song_ix as usize);
        let downloaded = self.is_downloaded(song_ix);
        let offline_opacity = self.offline_row_opacity(song_ix);
        let thumb = if self.compact { None } else { self.thumb_faded(&song.album_id, thumb_px) };

        let group = gpui::SharedString::new_static("song-row");
        let inset = t.s(if self.compact { 3.0 } else { 13.0 });
        let selected = self.selection.contains(song_ix);
        let fills = crate::view_rows::row_fills(&t, selected, downloaded);
        let hover_play = crate::view_rows::row_hover_play(&t, ("row-play", order_ix), group.clone()).on_click(cx.listener(move |this, _, _, cx| {
            cx.stop_propagation();
            this.play_row(order_ix, cx);
        }));
        let lead: AnyElement = if self.compact {
            div().w(c.lead).flex_none().relative().flex().items_center().justify_center().child(hover_play).into_any_element()
        } else {
            div().w(c.lead).flex_none().child(div().relative().w(t.s(42.0)).flex().items_center().justify_center().child(crate::view_rows::row_thumb(&t, thumb)).child(hover_play)).into_any_element()
        };

        let name_color: Hsla = if is_now {
            t.accent_soft_hsla()
        } else {
            t.t1().into()
        };
        let dim_color: Hsla = t.t2().into();

        // Rows can be dragged out to a playlist or the queue.
        let out = self.row_drag_out(crate::row_drag::DragList::Songs, order_ix);
        let mut row = div()
            .id(("song-row", order_ix))
            .group(group.clone())
            .relative()
            .when(out, |d| d.opacity(DRAG_OUT_DIM))
            .h(self.row_height())
            .flex()
            .flex_row()
            .items_center()
            .w(c.width)
            .ml(t.s(GUTTER))
            .pl(c.pad_left)
            .stop_row(&t, inset).on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                if std::mem::take(&mut this.suppress_row_click) {
                    return;
                }
                let modifiers = event.modifiers();
                let count = if event.is_keyboard() { 2 } else { event.click_count() };
                this.click_row(order_ix, modifiers.secondary(cx), modifiers.shift, count, cx);
            }))
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| this.open_row_menu(order_ix, event.position, cx)))
            .child(crate::view_rows::row_pill(&t, inset, group.clone(), fills))
            .children(self.row_flash(song_ix, inset, R_ROW))
            .child(div().opacity(offline_opacity).child(lead))
            .child(div().w(c.title).flex_none().pr(t.s(NAME_CELL_PAD)).flex().min_w(px(0.0)).opacity(offline_opacity).on_mouse_down(MouseButton::Left, cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                // A press on the title lifts the row.
                let count = this.order.len();
                this.row_press(crate::row_drag::DragList::Songs, order_ix, count, event, cx);
            })).child(crate::ui::titled(&t, div().id("title").truncate().text_color(name_color).font_weight(FontWeight::MEDIUM).child(song.title.clone()).full_text_tip(&t, song.title.clone()), song.explicit, TEXT_MD)));
        // Artist, album and genre are links (`song_links.rs`).
        for col in crate::view_rows::OptCol::ALL {
            if let Some(w) = c.cols[col as usize] {
                let content = self.optional_cell(col, 900_000_000 + order_ix, &song, dim_color, cx);
                row = row.child(crate::view_rows::body_cell(&t, w, col == crate::view_rows::OptCol::Plays).opacity(offline_opacity).child(content));
            }
        }
        row = row.child(crate::view_rows::body_cell(&t, c.time, true).text_color(dim_color).opacity(offline_opacity).child(div().truncate().child(clock(song.duration_s))));

        let actions = self.song_actions(("songs", order_ix), song_ix, None, c.actions, cx);
        row = row.child(actions);
        let divider = div().absolute().left(px(0.0)).right(px(0.0)).bottom(px(0.0)).h(px(1.0)).bg(t.line());
        row.child(divider).into_any_element()
    }

}
