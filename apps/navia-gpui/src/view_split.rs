//! The split view for Albums and Artists: the list on the left and the chosen item's page beside it. Each page
//! remembers whether split is on.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, Context, FontWeight, ListAlignment, ListState, MouseButton, SharedString, div, list, px, relative, uniform_list};

use crate::ui::FullTextTip;
use crate::focus::FocusStop;
use crate::pages::Block;
use crate::shell::{Layout, Shell};
use crate::theme::size::*;
use crate::ui::{glass_pill, icon_button};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SplitPage {
    Albums,
    Artists,
}

/// One page's split state: the chosen item and the right side's blocks.
pub struct SplitPane {
    /// The chosen row's id. None picks the first.
    pub selected: Option<SharedString>,
    pub state: ListState,
    pub blocks: Rc<Vec<Block>>,
    /// What `blocks` shows, so rebuilding the same item keeps its scroll.
    shown: Option<SharedString>,
}

impl SplitPane {
    /// The item the page beside the list shows.
    pub fn shown(&self) -> Option<&SharedString> {
        self.shown.as_ref()
    }
}

impl Default for SplitPane {
    fn default() -> Self {
        Self { selected: None, state: ListState::new(0, ListAlignment::Top, px(300.0)), blocks: Default::default(), shown: None }
    }
}

pub type SplitRows = Rc<dyn Fn(&mut Shell, usize, &Layout, &mut Context<Shell>) -> AnyElement>;

impl Shell {
    /// Whether split is chosen. Below `SPLIT_MIN_W` only the detail page shows (`split_narrow`).
    pub fn split_on(&self, page: SplitPage) -> bool {
        match page {
            SplitPage::Albums => self.prefs.album_split,
            SplitPage::Artists => self.prefs.artist_split,
        }
    }

    pub fn split_pane(&mut self, page: SplitPage) -> &mut SplitPane {
        match page {
            SplitPage::Albums => &mut self.albums_view.pane,
            SplitPage::Artists => &mut self.artists_view.pane,
        }
    }

    fn split_narrow(&self, layout: &Layout) -> bool {
        f32::from(layout.main_width) / self.theme.token_scale() <= SPLIT_MIN_W
    }

    fn set_split(&mut self, page: SplitPage, on: bool, cx: &mut Context<Self>) {
        match page {
            SplitPage::Albums => self.prefs.album_split = on,
            SplitPage::Artists => self.prefs.artist_split = on,
        }
        self.prefs.save();
        self.invalidate_page();
        cx.notify();
    }

    /// The top bar's Split view and Grid view buttons.
    pub fn layout_toggle(&mut self, page: SplitPage, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let split = self.split_on(page);
        let key = match page {
            SplitPage::Albums => "albums",
            SplitPage::Artists => "artists",
        };
        let split_btn = icon_button(&t, SharedString::from(format!("{key}-split-view")), crate::i18n::tr("Split view"), if split { "list-bullets-fill" } else { "list-bullets-regular" }, split)
            .stop(&t)
            .on_click(cx.listener(move |this, _, _, cx| this.set_split(page, true, cx)));
        let grid_btn = icon_button(&t, SharedString::from(format!("{key}-grid-view")), crate::i18n::tr("Grid view"), if split { "grid-four-regular" } else { "grid-four-fill" }, !split)
            .stop(&t)
            .on_click(cx.listener(move |this, _, _, cx| this.set_split(page, false, cx)));
        glass_pill(&t).occlude().child(split_btn).child(grid_btn).into_any_element()
    }

    /// The layout for the right side: what the list leaves, minus the room kept at its end.
    pub fn split_detail_layout(&self, layout: &Layout) -> Layout {
        let list = if self.split_narrow(layout) { 0.0 } else { GUTTER + SPLIT_LIST_W };
        layout.clone_for_width(layout.main_width - self.theme.s(list + SPLIT_END))
    }

    /// The split as one block `above` tokens from the window top: `count` list rows drawn by `row`, and `detail` for
    /// the item `shown`. The right side keeps its scroll while showing the same item.
    pub fn split_block(&mut self, page: SplitPage, shown: Option<SharedString>, mut detail: Vec<Block>, count: usize, above: f32, row: SplitRows, layout: &Layout) -> Block {
        detail.push(Block::Spacer(self.page_end(layout)));
        let pane = self.split_pane(page);
        let top = pane.state.logical_scroll_top();
        let same = pane.shown == shown;
        pane.state.reset(detail.len());
        if same {
            pane.state.scroll_to(gpui::ListOffset { item_ix: top.item_ix.min(detail.len().saturating_sub(1)), offset_in_item: top.offset_in_item });
        }
        pane.blocks = Rc::new(detail);
        pane.shown = shown;
        let id: &'static str = match page {
            SplitPage::Albums => "albums-split",
            SplitPage::Artists => "artists-split",
        };
        Shell::plain_block(move |this, layout, _, cx| {
            let t = this.theme;
            let view_h = layout.viewport.height - t.s(above);
            let row = row.clone();
            let left = uniform_list(id, count, cx.processor(move |this, range: std::ops::Range<usize>, window, cx| {
                let layout = this.layout_ref(window);
                range.map(|ix| row(this, ix, &layout, cx)).collect()
            }))
            .h(view_h)
            .w(t.s(SPLIT_LIST_W))
            .pr(t.s(SPLIT_LIST_PAD))
            .border_r_1()
            .border_color(t.line());
            let detail_w = this.split_detail_layout(layout).main_width;
            let pane = this.split_pane(page);
            let blocks = pane.blocks.clone();
            let right = list(pane.state.clone(), cx.processor(move |this, ix: usize, window, cx| {
                let saved = this.layout_override.replace(detail_w);
                let el = this.render_blocks_at(&blocks, ix, window, cx);
                this.layout_override = saved;
                el
            }))
            .h(view_h)
            .w(detail_w);
            let narrow = this.split_narrow(layout);
            div().flex().flex_row().when(!narrow, |d| d.pl(t.s(GUTTER)).child(left)).h(view_h).w(layout.main_width).child(right).into_any_element()
        })
    }

    /// One row of the split list: a picture, a title and a quiet line. `on` while its page is shown.
    #[allow(clippy::too_many_arguments)]
    pub fn split_row(&mut self, id: &'static str, ix: usize, on: bool, art: AnyElement, title: SharedString, sub: SharedString, cx: &mut Context<Self>, action: impl Fn(&mut Shell, &mut Context<Shell>) + 'static) -> AnyElement {
        let t = self.theme;
        div()
            .id((id, ix))
            .relative()
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(t.s(SPLIT_ROW_GAP))
            .h(t.s(SPLIT_ROW_H))
            .px(t.s(SPLIT_ROW_PAD))
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| action(this, cx)))
            .child(div().absolute().left(px(0.0)).right(px(0.0)).top(t.s(SPLIT_ROW_INSET)).bottom(t.s(SPLIT_ROW_INSET)).rounded(t.s(R_MD)).when(on, |d| d.bg(t.hover())))
            .child(div().relative().flex_none().child(art))
            .child(div().relative().min_w(px(0.0)).line_height(relative(1.32)).child(div().id("name").truncate().font_weight(FontWeight::MEDIUM).child(title.clone()).full_text_tip(&t, title)).child(div().id("sub").truncate().text_color(t.t2()).child(sub.clone()).full_text_tip(&t, sub)))
            .child(div().absolute().left(t.s(SPLIT_LINE_FROM)).right(px(0.0)).bottom(px(0.0)).h(px(1.0)).bg(t.line()))
            .into_any_element()
    }

    /// Shows a row's item beside the list.
    pub fn split_select(&mut self, page: SplitPage, id: SharedString, cx: &mut Context<Self>) {
        self.split_pane(page).selected = Some(id);
        // The detail page's search belonged to the previous item.
        self.album_detail = Default::default();
        self.artist_detail = Default::default();
        self.invalidate_page();
        cx.notify();
    }
}

/// The All Albums row's picture, a glyph in place of art.
pub fn split_glyph(t: &crate::theme::Theme, name: &str) -> AnyElement {
    div().size(t.s(SPLIT_ART)).flex().items_center().justify_center().child(crate::ui::icon(t, name, SPLIT_GLYPH, t.t1())).into_any_element()
}
