//! The library filter row on Songs and Artists. Chips stay on one line, and those that don't fit fold into More Filters
//! (`ui::fold_chips`). The catalogs and matching live in the core (`navia_core::song_filters`,
//! `navia_core::artist_filters`).

use crate::i18n::{fmt_int, tr, trf};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{AnyElement, Bounds, Context, Pixels, ScrollHandle, SharedString, Window, anchored, deferred, div, point, px};
use navia_core::song_filters::{CHIPS_UP_TO, FilterId, FilterKind, FilterOption, FilterSource, FilterState, FilterValue, OptionLabel, QuickRange, range_problem, spec};

use crate::focus::FocusStop;
use crate::pages::Route;
use crate::shell::{Layout, Shell};
use crate::theme::size::*;
use crate::ui::{chip_glyph, chip_value, filter_chip, filter_chip_pair, filter_chip_remove};
use crate::ui_controls::{TextTarget, track_bounds};
use crate::view_menu::MenuRow;

/// Gap between a popover and its chip, also kept from the window edges.
const POP_OFFSET: f32 = 6.0;
const EDGE: f32 = 10.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterPage {
    Songs,
    Artists,
}

/// One page's filters and what they show.
#[derive(Default)]
pub struct LibraryFilters {
    pub state: FilterState,
    /// Filters added from More Filters stay as chips for the visit, even once cleared.
    added: Vec<FilterId>,
    /// What the filters know about every song or artist, from the core. None until loaded, and in sample mode.
    index: Option<Arc<dyn FilterSource>>,
    /// The Artists index was requested and is loading.
    pub requested: bool,
    /// The filters this library has data for, in chip order.
    available: Vec<FilterId>,
    /// A list filter's choices with song counts, read once per index.
    options: RefCell<HashMap<FilterId, Rc<Vec<FilterOption>>>>,
    /// The open popover's filter and its find field.
    open: Option<FilterId>,
    find: String,
    list_scroll: ScrollHandle,
    /// The chips folded into More Filters as last drawn, and whether Clear All and Save went in too.
    folded: Vec<FilterId>,
    end_in_more: bool,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// Whether a filter is in the page's main set. Date Added is main on Songs but under More on Artists, so this reads the
/// page's own catalog.
fn main_on(page: FilterPage, id: FilterId) -> bool {
    let catalog: &[navia_core::song_filters::FilterSpec] = match page {
        FilterPage::Songs => &navia_core::song_filters::FILTERS,
        FilterPage::Artists => &navia_core::artist_filters::ARTIST_FILTERS,
    };
    catalog.iter().any(|f| f.id == id && f.main)
}

fn chip_key(id: FilterId) -> String {
    format!("filter-chip-{id:?}")
}

fn option_label(o: &FilterOption) -> SharedString {
    match &o.label {
        OptionLabel::Text(text) => text.clone().into(),
        OptionLabel::Key(key) => tr(key).into(),
    }
}

impl LibraryFilters {
    /// Opening Songs from a genre starts with that genre picked.
    pub fn for_genre(genre: Option<&str>) -> FilterState {
        let mut state = FilterState::default();
        if let Some(g) = genre {
            state.set(FilterId::Genre, Some(FilterValue::List(vec![g.to_string()])));
        }
        state
    }

    /// The genre when it's the only filter. It names what Songs plays, like "Rock" instead of "Songs".
    pub fn only_genre(&self) -> Option<String> {
        match (self.state.active().as_slice(), self.state.get(FilterId::Genre)) {
            ([FilterId::Genre], Some(FilterValue::List(list))) if list.len() == 1 => list.first().cloned(),
            _ => None,
        }
    }

    pub fn is_filtering(&self) -> bool {
        !self.state.is_empty()
    }

    /// Whether the page draws a filter row, meaning the library has data for at least one filter.
    pub fn has_row(&self) -> bool {
        !self.available.is_empty()
    }

    /// The index hasn't been read or asked for yet.
    pub fn wants_index(&self) -> bool {
        self.index.is_none() && !self.requested
    }

    fn options(&self, id: FilterId) -> Rc<Vec<FilterOption>> {
        let Some(index) = &self.index else { return Rc::default() };
        self.options.borrow_mut().entry(id).or_insert_with(|| Rc::new(index.options(id, now_ms()))).clone()
    }

    fn quick(&self, id: FilterId) -> Vec<QuickRange> {
        self.index.as_ref().map(|i| i.quick_ranges(id)).unwrap_or_default()
    }

    /// What an active chip shows after its name, like "Rock +2" or "1990 – 1999".
    fn summary(&self, id: FilterId, value: &FilterValue) -> String {
        match value {
            FilterValue::On => tr(spec(id).label).to_string(),
            FilterValue::Range { from, to } => match (from.is_empty(), to.is_empty()) {
                (false, false) if from == to => from.clone(),
                (false, false) => format!("{from} \u{2013} {to}"),
                (false, true) => trf("From {value}", &[("value", from)]),
                _ => trf("Up to {value}", &[("value", to)]),
            },
            FilterValue::List(list) => {
                let first = list.first().map(String::as_str).unwrap_or("");
                let options = self.options(id);
                let label = options.iter().find(|o| o.value == first).map_or_else(|| first.to_string(), |o| option_label(o).to_string());
                if list.len() > 1 { format!("{label} +{}", list.len() - 1) } else { label }
            }
        }
    }

    /// The chip row's height, or None when there's no row.
    pub fn row_height(&self, t: &crate::theme::Theme) -> Option<Pixels> {
        (!self.available.is_empty()).then(|| t.s(CHIP_H))
    }
}

impl Shell {
    pub fn filters(&self, page: FilterPage) -> &LibraryFilters {
        match page {
            FilterPage::Songs => &self.song_filters,
            FilterPage::Artists => &self.artist_filters,
        }
    }

    pub fn filters_mut(&mut self, page: FilterPage) -> &mut LibraryFilters {
        match page {
            FilterPage::Songs => &mut self.song_filters,
            FilterPage::Artists => &mut self.artist_filters,
        }
    }

    /// The page on screen when it has a filter row.
    pub fn filter_page(&self) -> Option<FilterPage> {
        match self.route {
            Route::Songs => Some(FilterPage::Songs),
            Route::Artists => Some(FilterPage::Artists),
            _ => None,
        }
    }

    /// The core read the library's filter facts. A new index replaces the old choices.
    pub fn set_filter_index(&mut self, page: FilterPage, index: Arc<dyn FilterSource>, cx: &mut Context<Self>) {
        let f = self.filters_mut(page);
        f.requested = false;
        if f.index.as_ref().is_some_and(|i| std::ptr::addr_eq(Arc::as_ptr(i), Arc::as_ptr(&index))) {
            return;
        }
        f.available = index.available();
        f.index = Some(index);
        f.options.borrow_mut().clear();
        if page == FilterPage::Artists {
            self.invalidate_page();
        }
        cx.notify();
    }

    /// Sets or clears one filter and narrows the list again.
    pub fn set_page_filter(&mut self, page: FilterPage, id: FilterId, value: Option<FilterValue>, cx: &mut Context<Self>) {
        self.filters_mut(page).state.set(id, value);
        self.page_filters_changed(page, cx);
    }

    fn page_filters_changed(&mut self, page: FilterPage, cx: &mut Context<Self>) {
        match page {
            FilterPage::Songs => {
                self.rebuild_order();
                self.list_scroll.0.borrow().base_handle.set_offset(point(px(0.0), px(0.0)));
            }
            FilterPage::Artists => self.artists_query_changed(),
        }
        cx.notify();
    }

    pub fn clear_page_filters(&mut self, page: FilterPage, cx: &mut Context<Self>) {
        self.filters_mut(page).state.clear();
        self.page_filters_changed(page, cx);
    }

    /// The empty state's Clear All, which clears the search text too.
    fn clear_search_and_filters(&mut self, page: FilterPage, cx: &mut Context<Self>) {
        match page {
            FilterPage::Songs => self.query.clear(),
            FilterPage::Artists => self.artists_view.query.clear(),
        }
        self.search_active = false;
        self.clear_page_filters(page, cx);
    }

    fn toggle_filter_pop(&mut self, page: FilterPage, id: FilterId, cx: &mut Context<Self>) {
        let f = self.filters_mut(page);
        f.open = if f.open == Some(id) { None } else { Some(id) };
        f.find.clear();
        f.list_scroll.set_offset(point(px(0.0), px(0.0)));
        self.leave_filter_fields();
        cx.notify();
    }

    /// Closes the open filter popover. Returns true if one was open.
    pub fn close_filter_pop(&mut self, cx: &mut Context<Self>) -> bool {
        let open = [FilterPage::Songs, FilterPage::Artists].into_iter().any(|p| self.filters_mut(p).open.take().is_some());
        if !open {
            return false;
        }
        self.leave_filter_fields();
        cx.notify();
        true
    }

    fn leave_filter_fields(&mut self) {
        if matches!(self.controls.text, Some(TextTarget::FilterFind | TextTarget::FilterFrom | TextTarget::FilterTo)) {
            self.controls.text = None;
        }
    }

    pub fn filter_text(&self, target: &TextTarget) -> String {
        let Some(page) = self.filter_page() else { return String::new() };
        let f = self.filters(page);
        let range = f.open.and_then(|id| match f.state.get(id) {
            Some(FilterValue::Range { from, to }) => Some((from.clone(), to.clone())),
            _ => None,
        });
        match target {
            TextTarget::FilterFind => f.find.clone(),
            TextTarget::FilterFrom => range.map(|r| r.0).unwrap_or_default(),
            TextTarget::FilterTo => range.map(|r| r.1).unwrap_or_default(),
            _ => String::new(),
        }
    }

    pub fn filter_text_typed(&mut self, target: &TextTarget, value: String, cx: &mut Context<Self>) {
        let Some(page) = self.filter_page() else { return };
        let Some(id) = self.filters(page).open else { return };
        if *target == TextTarget::FilterFind {
            let f = self.filters_mut(page);
            f.find = value;
            f.list_scroll.set_offset(point(px(0.0), px(0.0)));
            cx.notify();
            return;
        }
        let clean = navia_core::song_filters::clean_range_input(id, &value);
        let (mut from, mut to) = match self.filters(page).state.get(id) {
            Some(FilterValue::Range { from, to }) => (from.clone(), to.clone()),
            _ => (String::new(), String::new()),
        };
        if *target == TextTarget::FilterFrom {
            from = clean;
        } else {
            to = clean;
        }
        self.set_page_filter(page, id, Some(FilterValue::Range { from, to }), cx);
    }

    fn pick_filter_choice(&mut self, page: FilterPage, id: FilterId, value: &str, cx: &mut Context<Self>) {
        self.filters_mut(page).state.toggle_choice(id, value);
        self.page_filters_changed(page, cx);
    }

    /// Opens More Filters under its chip. It lists the folded chips, then filters not shown yet, then Clear All and
    /// Save if the line had no room. Picking one adds it to the line and opens it.
    fn open_more_filters(&mut self, page: FilterPage, chip: Bounds<Pixels>, viewport: gpui::Size<Pixels>, can_save: bool, cx: &mut Context<Self>) {
        // Below the chip, from its left. Near the window's right edge it aligns to the chip's right instead, since the
        // menu's own snapping only reaches past the window shadow.
        let right_limit = viewport.width - self.float_margin().right;
        let end = chip.left() + self.theme.s(POP_W) > right_limit;
        let position = point(if end { chip.right().min(right_limit) } else { chip.left() }, chip.bottom() + self.theme.s(POP_OFFSET));
        let f = self.filters(page);
        let rest: Vec<FilterId> = f.available.iter().copied().filter(|id| !main_on(page, *id) && !f.added.contains(id) && !f.state.is_set(*id)).collect();
        let mut rows: Vec<MenuRow> = Vec::new();
        for id in f.folded.iter().copied().chain(rest) {
            let value = f.state.get(id).cloned();
            let label = match &value {
                Some(v) if spec(id).kind != FilterKind::Toggle => format!("{}: {}", tr(spec(id).label), f.summary(id, v)),
                _ => tr(spec(id).label).to_string(),
            };
            let action = move |this: &mut Shell, cx: &mut Context<Shell>| {
                let f = this.filters_mut(page);
                if !main_on(page, id) && !f.added.contains(&id) {
                    f.added.push(id);
                }
                if spec(id).kind == FilterKind::Toggle {
                    let on = f.state.is_set(id);
                    this.set_page_filter(page, id, (!on).then_some(FilterValue::On), cx);
                } else {
                    this.toggle_filter_pop(page, id, cx);
                }
                cx.notify();
            };
            rows.push(if value.is_some() { MenuRow::check(label, true, action) } else { MenuRow::plain(label, action) });
        }
        if f.end_in_more && f.is_filtering() {
            rows.push(MenuRow::Divider);
            rows.push(MenuRow::plain(tr("Clear All"), move |this, cx| this.clear_page_filters(page, cx)));
            if page == FilterPage::Songs {
                rows.push(if can_save { MenuRow::plain(tr("Save as Smart Playlist"), |this, cx| this.save_filters_as_smart(cx)) } else { MenuRow::plain_off(tr("Save as Smart Playlist")) });
            }
        }
        self.open_menu(rows, position, end, cx);
    }

    /// Opens the smart playlist editor prefilled with the filters as rules and the list's sort.
    fn save_filters_as_smart(&mut self, cx: &mut Context<Self>) {
        let (sort, descending) = navia_core::song_filters::smart_sort(crate::core_library::song_order_of(self.sort));
        let (json, dropped) = navia_core::song_filters::to_rules(&self.song_filters.state, sort, descending);
        let dropped = (!dropped.is_empty()).then(|| dropped.iter().map(|id| tr(spec(*id).label)).collect::<Vec<_>>().join(", "));
        self.smart_prefill = Some((json, dropped));
        self.navigate(Route::SmartEditor(None), cx);
    }

    /// The chip line. It starts at the page gutter and never wraps or overflows, folding what doesn't fit into More
    /// Filters. Songs places it over its table.
    pub fn render_filter_row(&mut self, page: FilterPage, layout: &Layout, window: &mut Window, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let t = self.theme;
        if self.filters(page).available.is_empty() {
            return None;
        }
        let room = layout.main_width - t.s(GUTTER) * 2.0;
        let f = self.filters(page);
        let candidates: Vec<FilterId> = f.available.iter().copied().filter(|id| main_on(page, *id) || f.added.contains(id) || f.state.is_set(*id)).collect();
        let rest = f.available.iter().any(|id| !candidates.contains(id));
        let any = f.is_filtering();
        let can_save = any && !f.state.unfinished();
        let open_id = f.open;
        let saves = page == FilterPage::Songs;

        // Measure each chip up front so the line can fold what doesn't fit.
        let mut widths = Vec::with_capacity(candidates.len());
        let mut keep = Vec::with_capacity(candidates.len());
        for id in &candidates {
            let sp = spec(*id);
            let label = tr(sp.label);
            let value = self.filters(page).state.get(*id).cloned();
            let w = match (&value, sp.kind) {
                (_, FilterKind::Toggle) => crate::ui::filter_chip_width(&t, window, label, false),
                (Some(v), _) => {
                    let summary = self.filters(page).summary(*id, v);
                    crate::ui::filter_chip_pair_width(&t, window, &format!("{label}:"), &summary)
                }
                (None, _) => crate::ui::filter_chip_width(&t, window, label, true),
            };
            widths.push(f32::from(w));
            keep.push(value.is_some());
        }
        let pin = candidates.iter().position(|id| open_id == Some(*id));
        let more_label = tr("More Filters");
        // The More chip is a plus, a label and a caret.
        let more_w = crate::ui::filter_chip_width(&t, window, more_label, true) + t.s(CHIP_GLYPH + CHIP_GAP);
        let end_w = if !any {
            px(0.0)
        } else if saves {
            crate::view_album::text_button_width(&t, window, tr("Clear All")) + t.s(CHIP_GAP) + crate::ui_controls::small_filled_action_width(&t, window, tr("Save as Smart Playlist"), true)
        } else {
            crate::view_album::text_button_width(&t, window, tr("Clear All"))
        };
        let fold = crate::ui::fold_chips(&widths, &keep, pin, f32::from(more_w), f32::from(end_w), f32::from(t.s(CHIP_GAP)), f32::from(room), rest);
        let f = self.filters_mut(page);
        f.folded = candidates.iter().zip(&fold.shown).filter(|(_, s)| !**s).map(|(id, _)| *id).collect();
        f.end_in_more = fold.end_in_more;

        let mut row = div().ml(t.s(GUTTER)).w(room).h(t.s(CHIP_H)).flex().flex_row().flex_nowrap().items_center().gap(t.s(CHIP_GAP));
        for (id, shown) in candidates.into_iter().zip(fold.shown) {
            let store = self.bounds_store(&chip_key(id));
            if !shown {
                // Not drawn this frame. A popover anchored to it waits until it's placed again.
                *store.borrow_mut() = Bounds::default();
                continue;
            }
            let sp = spec(id);
            let value = self.filters(page).state.get(id).cloned();
            let on = value.is_some();
            let label = tr(sp.label);
            if sp.kind == FilterKind::Toggle {
                row = row.child(filter_chip(&t, ("filter-chip", id as usize), on).block_mouse_except_scroll().stop(&t).child(label).on_click(cx.listener(move |this, _, _, cx| {
                    this.set_page_filter(page, id, (!on).then_some(FilterValue::On), cx);
                })));
                continue;
            }
            let open = cx.listener(move |this, _: &gpui::ClickEvent, _, cx| this.toggle_filter_pop(page, id, cx));
            match value {
                Some(value) => {
                    let summary = self.filters(page).summary(id, &value);
                    let main = filter_chip(&t, ("filter-chip", id as usize), true)
                        .relative()
                        .child(track_bounds(store))
                        .child(div().opacity(0.6).child(format!("{label}:")))
                        .child(chip_value(&t, summary))
                        .stop(&t)
                        .on_click(open);
                    let remove = filter_chip_remove(&t, ("filter-chip-x", id as usize)).tooltip(crate::ui::tip(&t, trf("Clear {filter}", &[("filter", label)]))).stop(&t).on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.set_page_filter(page, id, None, cx);
                    }));
                    row = row.child(filter_chip_pair(&t, main, remove).block_mouse_except_scroll());
                }
                None => {
                    row = row.child(filter_chip(&t, ("filter-chip", id as usize), false).block_mouse_except_scroll().child(track_bounds(store)).child(label).child(chip_glyph(&t, "caret-down-bold", false).opacity(0.7)).stop(&t).on_click(open));
                }
            }
        }
        if fold.more {
            let store = self.bounds_store("filter-chip-more");
            row = row.child(
                filter_chip(&t, "filter-chip-more", false)
                    .block_mouse_except_scroll()
                    .child(track_bounds(store.clone()))
                    .child(chip_glyph(&t, "plus-bold", false))
                    .child(more_label)
                    .child(chip_glyph(&t, "caret-down-bold", false).opacity(0.7))
                    .stop(&t)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let b = *store.borrow();
                        this.open_more_filters(page, b, window.viewport_size(), can_save, cx);
                    })),
            );
        }
        if any && !fold.end_in_more {
            let clear = crate::view_album::text_button_on(&t, "filters-clear", None, tr("Clear All"), true, t.t1().into()).block_mouse_except_scroll().stop(&t).on_click(cx.listener(move |this, _, _, cx| this.clear_page_filters(page, cx)));
            let mut end = div().ml_auto().flex().flex_row().flex_none().items_center().gap(t.s(CHIP_GAP)).child(clear);
            if saves {
                let glyph = crate::ui::icon(&t, "lightning-fill", CHIP_GLYPH, if can_save { t.on_accent.hsla(1.0) } else { t.t2().into() });
                let save = crate::ui_controls::small_action(&t, crate::ui_controls::primary_action(&t, "filters-save", can_save))
                    .block_mouse_except_scroll()
                    .child(glyph)
                    .child(tr("Save as Smart Playlist"))
                    .when(can_save, |d| d.stop(&t).on_click(cx.listener(|this, _, _, cx| this.save_filters_as_smart(cx))));
                end = end.child(save);
            }
            row = row.child(end);
        }
        Some(row)
    }

    /// The empty state when nothing matches, with a Clear All for the search and filters.
    pub fn render_filters_empty(&mut self, page: FilterPage, cx: &mut Context<Self>) -> gpui::Div {
        let t = self.theme;
        let (query, title, none) = match page {
            FilterPage::Songs => (self.query.clone(), tr("No songs match"), tr("No songs fit these filters.")),
            FilterPage::Artists => (self.artists_view.query.clone(), tr("No artists match"), tr("No artists fit these filters.")),
        };
        let hint = if query.is_empty() { none.to_string() } else { trf("Nothing matches \u{201c}{query}\u{201d} with these filters.", &[("query", &query)]) };
        self.empty_state("magnifying-glass-regular", title, hint, false)
            .child(crate::ui_controls::empty_action(&t, "filters-empty-clear").child(tr("Clear All")).stop(&t).on_click(cx.listener(move |this, _, _, cx| this.clear_search_and_filters(page, cx))))
    }

    /// The open filter's popover under its chip, with its editor and a Clear once it's set.
    pub fn render_filter_pop(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let page = self.filter_page()?;
        let id = self.filters(page).open?;
        let t = self.theme;
        let anchor = *self.bounds_store(&chip_key(id)).borrow();
        // Not drawn yet, for example just added from More Filters. Placed on the next frame.
        if anchor.size.width <= px(0.0) {
            window.request_animation_frame();
            return None;
        }
        let viewport = window.viewport_size();
        let gap = t.s(POP_OFFSET);
        let below = viewport.height - self.edge.bottom - px(EDGE) - anchor.bottom() - gap;
        let width = t.s(FILTER_POP_W);
        let x = anchor.left().min(viewport.width - self.edge.right - px(EDGE) - width).max(self.edge.left + px(EDGE));
        let body = match spec(id).kind {
            FilterKind::List { find, .. } => self.render_list_editor(page, id, find, window, cx),
            FilterKind::Range { .. } => self.render_range_editor(page, id, window, cx),
            FilterKind::Toggle => return None,
        };
        let set = self.filters(page).state.is_set(id);
        let foot = set.then(|| {
            div().flex().flex_row().justify_end().flex_none().p(t.s(CHIP_GAP)).border_t_1().border_color(t.line()).child(
                crate::view_album::text_button_on(&t, "filter-pop-clear", None, tr("Clear"), true, t.t1().into()).stop(&t).on_click(cx.listener(move |this, _, _, cx| this.set_page_filter(page, id, None, cx))),
            )
        });
        let panel = crate::ui::float_panel(&t)
            .id("filter-pop")
            .w(width)
            .max_h(t.s(POP_MAX_H).min(below.max(t.s(CHIP_H * 4.0))))
            .flex()
            .flex_col()
            .overflow_hidden()
            .text_color(t.t1())
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().flex().flex_col().min_h(px(0.0)).p(t.s(DIALOG_GAP)).child(body))
            .children(foot);
        let scrim = div()
            .id("filter-pop-scrim")
            .absolute()
            .inset_0()
            .block_mouse_except_scroll()
            .on_mouse_down(gpui::MouseButton::Left, cx.listener(|this, _, _, cx| {
                this.close_filter_pop(cx);
            }))
            .on_scroll(cx.listener(|this, _: &gpui::ScrollEvent, _, cx| {
                this.close_filter_pop(cx);
            }))
            .child(anchored().position(point(x, anchor.bottom() + gap)).child(panel));
        Some(deferred(scrim).with_priority(10).into_any_element())
    }

    /// Short lists show as chips. Long ones get a find field over a checked list with song counts, picked ones first.
    fn render_list_editor(&mut self, page: FilterPage, id: FilterId, find: Option<&'static str>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let options = self.filters(page).options(id);
        let picked: Vec<String> = match self.filters(page).state.get(id) {
            Some(FilterValue::List(v)) => v.clone(),
            _ => Vec::new(),
        };
        if find.is_none() && options.len() <= CHIPS_UP_TO {
            let mut chips = div().flex().flex_row().flex_wrap().gap(t.s(CHIP_GAP));
            for (i, o) in options.iter().enumerate() {
                let on = picked.contains(&o.value);
                let value = o.value.clone();
                chips = chips.child(filter_chip(&t, ("filter-choice", i), on).child(option_label(o)).stop(&t).on_click(cx.listener(move |this, _, _, cx| this.pick_filter_choice(page, id, &value, cx))));
            }
            return chips.into_any_element();
        }
        let placeholder = find.unwrap_or(crate::i18n::n_("Find"));
        let query = self.filters(page).find.clone();
        let needle = query.trim().to_lowercase();
        let line = self.text_line_sized(&TextTarget::FilterFind, &query, tr(placeholder), "filter-find-text", t.text(MENU_TEXT), window);
        let clear = self.search_clear(TextTarget::FilterFind, "filter-find-clear", &query, SEARCH_CLEAR_SM, cx);
        let field = crate::ui::float_filter(&t, "filter-find", line, clear).stop_field(&t, TextTarget::FilterFind).on_click(cx.listener(|this, _, _, cx| this.focus_field(TextTarget::FilterFind, cx)));
        // Picked ones stay on top so they can be unpicked without scrolling.
        let shown: Vec<&FilterOption> = options.iter().filter(|o| picked.contains(&o.value)).chain(options.iter().filter(|o| !picked.contains(&o.value) && (needle.is_empty() || option_label(o).to_lowercase().contains(&needle)))).collect();
        let mut list = div().id("filter-list").flex().flex_col().max_h(t.s(FILTER_LIST_MAX_H)).overflow_y_scroll().track_scroll(&self.filters(page).list_scroll);
        if shown.is_empty() {
            list = list.child(div().px(t.s(MENU_ROW_PAD_X)).py(t.s(MENU_EMPTY_PAD_Y)).text_size(t.text(MENU_TEXT)).text_color(t.t2()).child(tr("No matches")));
        }
        for (i, o) in shown.into_iter().enumerate() {
            let value = o.value.clone();
            let row = MenuRow::check(option_label(o), picked.contains(&o.value), |_, _| {}).with_count(fmt_int(o.count as i64));
            if let Some(el) = self.menu_row(("filter-option", i), &row, false, false) {
                list = list.child(el.stop(&t).on_click(cx.listener(move |this, _, _, cx| this.pick_filter_choice(page, id, &value, cx))));
            }
        }
        div().flex().flex_col().min_h(px(0.0)).child(field).child(list).into_any_element()
    }

    /// Decade quick picks as chips, then From and To, with an error while the range is invalid.
    fn render_range_editor(&mut self, page: FilterPage, id: FilterId, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let (from, to) = match self.filters(page).state.get(id) {
            Some(FilterValue::Range { from, to }) => (from.clone(), to.clone()),
            _ => (String::new(), String::new()),
        };
        let problem = range_problem(id, &from, &to);
        let mut col = div().flex().flex_col().gap(t.s(SHEET_LABEL_GAP));
        let quick = self.filters(page).quick(id);
        if !quick.is_empty() {
            let mut chips = div().flex().flex_row().flex_wrap().gap(t.s(CHIP_GAP));
            for (i, q) in quick.into_iter().enumerate() {
                let on = from == q.from && to == q.to;
                chips = chips.child(filter_chip(&t, ("filter-quick", i), on).child(q.label.clone()).stop(&t).on_click(cx.listener(move |this, _, _, cx| {
                    let value = (!on).then(|| FilterValue::Range { from: q.from.clone(), to: q.to.clone() });
                    this.set_page_filter(page, id, value, cx);
                })));
            }
            col = col.child(chips);
        }
        let field = |this: &mut Shell, target: TextTarget, value: &str, placeholder: &'static str, key: &'static str, window: &mut Window, cx: &mut Context<Shell>| {
            let focused = this.controls.text.as_ref() == Some(&target);
            let line = this.text_line_sized(&target, value, placeholder, key, t.text(TEXT_SM), window);
            let focus = target.clone();
            crate::ui::number_field(&t, key, focused, problem.is_some() && !value.is_empty(), line).stop_field(&t, target).on_mouse_down(gpui::MouseButton::Left, cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.focus_field(focus.clone(), cx);
            }))
        };
        let from_el = field(self, TextTarget::FilterFrom, &from, tr("From"), "filter-from", window, cx);
        let to_el = field(self, TextTarget::FilterTo, &to, tr("To"), "filter-to", window, cx);
        col = col.child(div().flex().flex_row().items_center().gap(t.s(SHEET_LABEL_GAP)).text_size(t.text(TEXT_SM)).text_color(t.t2()).child(from_el).child(tr("to")).child(to_el));
        if let Some(problem) = problem {
            col = col.child(div().text_size(t.text(TEXT_XS)).text_color(t.t2()).child(tr(problem)));
        }
        col.into_any_element()
    }

    /// Screenshot steps for the filter row (`page=filter-…` in `NAVIA_SCRIPT`). `pick+Genre+Pop` picks a choice,
    /// `on+Favorite` switches one on, `pop+Albums` opens a popover, `more` opens More Filters, and `query+text` types
    /// into the Artists search.
    pub fn filter_script(&mut self, op: &str, viewport: gpui::Size<Pixels>, cx: &mut Context<Self>) {
        let Some(page) = self.filter_page() else { return };
        let mut parts = op.split('+');
        let verb = parts.next().unwrap_or("");
        let id = parts.next().and_then(|name| navia_core::song_filters::FILTERS.iter().chain(navia_core::artist_filters::ARTIST_FILTERS.iter()).map(|f| f.id).find(|id| format!("{id:?}") == name));
        let rest: Vec<&str> = parts.collect();
        match (verb, id) {
            ("pick", Some(id)) => self.pick_filter_choice(page, id, &rest.join("+"), cx),
            ("on", Some(id)) => self.set_page_filter(page, id, Some(FilterValue::On), cx),
            ("pop", Some(id)) => {
                let f = self.filters_mut(page);
                if !main_on(page, id) && !f.added.contains(&id) {
                    f.added.push(id);
                }
                self.toggle_filter_pop(page, id, cx);
            }
            ("more", _) => {
                let b = *self.bounds_store("filter-chip-more").borrow();
                let can_save = self.filters(page).is_filtering() && !self.filters(page).state.unfinished();
                self.open_more_filters(page, b, viewport, can_save, cx);
            }
            ("query", _) if page == FilterPage::Artists => {
                self.artists_view.query = rest.join(" ");
                self.artists_query_changed();
            }
            _ => {}
        }
        self.invalidate_page();
        cx.notify();
    }
}
