//! Liked Songs, every song this user favorited, newest first. It's built in rather than a playlist, so it can't be
//! edited, and likes change it at once.

use crate::i18n::{tr, trf};
use std::collections::HashSet;

use gpui::prelude::*;
use crate::focus::FocusStop;
use gpui::{AnyElement, Context, div};

use crate::pages::{Block, PageMetrics, RowVariant};
use crate::shell::{Layout, Route, Shell};
use crate::ui::{glass_pill, icon_button};
use crate::view_cards::{songs_and_length, song_count};
use crate::view_menu::MenuRow;
use crate::view_rows::{ColumnSort, SortColumn};

pub struct LikedView {
    /// A column sort, or None for newest like first.
    pub sort: Option<ColumnSort>,
    pub query: String,
}

impl Default for LikedView {
    fn default() -> Self {
        Self { sort: None, query: String::new() }
    }
}

impl Shell {
    /// The page's rows, newest like first. Likes the core hasn't reported yet, or won't in a read-only run, come first
    /// in the order they were made.
    pub fn liked_rows(&self) -> Vec<u32> {
        let mut seen = HashSet::new();
        let mut rows: Vec<u32> = self.liked_order.iter().copied().filter(|s| self.is_starred(*s) && seen.insert(*s)).collect();
        let mut rest: Vec<u32> = self.starred.iter().copied().filter(|s| !seen.contains(s)).collect();
        rest.sort_unstable();
        rows.extend(rest);
        rows
    }

    /// Marks the favorites as changed so an open Liked Songs page rebuilds.
    pub fn liked_changed(&mut self) {
        if self.route == Route::Liked {
            self.invalidate_page();
        }
    }

    /// The top bar's count, plus total length once every row is loaded.
    pub fn liked_summary(&self) -> String {
        let rows = self.liked_rows();
        let mut seconds = 0u64;
        for s in &rows {
            let Some(song) = self.song_peek(*s) else { return song_count(rows.len() as u32).to_string() };
            seconds += song.duration_s as u64;
        }
        songs_and_length(rows.len() as u32, seconds as u32)
    }

    pub fn build_liked(&mut self, _layout: &Layout) -> Vec<Block> {
        let all = self.liked_rows();
        let needle = self.liked_view.query.trim().to_lowercase();
        let mut shown: Vec<u32> = all
            .iter()
            .copied()
            .filter(|s| needle.is_empty() || self.song_peek(*s).is_some_and(|song| format!("{} {} {}", song.title, song.artist, song.album).to_lowercase().contains(&needle)))
            .collect();
        if let Some(sort) = self.liked_view.sort {
            self.sort_rows(&mut shown, sort);
        }
        self.core_preload_rows(&all);
        let has_songs = !all.is_empty();
        let list = self.add_list(all);
        let table = self.add_list(shown.clone());
        let actions = Shell::plain_block(move |this, layout, _, cx| this.render_liked_actions(list, has_songs, layout, cx));
        let head = Shell::plain_block(|this, layout, _, cx| {
            let t = this.theme;
            let about = crate::view_about::AboutText { title: tr("Liked Songs").into(), subtitle: None, art: Some(crate::art::LIKED_COVER.into()), text: tr("Every song you've liked, newest first.").into() };
            let below = div().child(div().mt(t.s(14.0)).child(this.liked_summary())).child(this.header_more_text("liked-desc", about, cx)).into_any_element();
            this.render_collection_head(&crate::art::LIKED_COVER.into(), tr("Liked Songs").into(), below, layout, cx)
        });
        let mut blocks = vec![head, Block::Spacer(28.0), actions];
        if shown.is_empty() {
            if has_songs {
                blocks.push(Block::Spacer(32.0));
                blocks.push(Shell::plain_block(|this, _, _, cx| {
                    let q = this.liked_view.query.clone();
                    this.empty_state("magnifying-glass-regular", tr("No songs match"), trf("Nothing in Liked Songs matches \u{201c}{q}\u{201d}.", &[("q", &q)]), true).child(this.clear_liked_search_button(cx)).into_any_element()
                }));
            } else {
                blocks.push(Block::Spacer(32.0));
                blocks.push(Shell::plain_block(|this, _, _, _| this.library_empty_state("heart-regular", tr("No liked songs yet"), tr("Songs you favorite with the heart show up here, newest first."), false).into_any_element()));
            }
        } else {
            blocks.push(Block::Spacer(32.0));
            let sorted = self.liked_view.sort;
            let on_sort: crate::view_rows::OnSort = std::rc::Rc::new(|this: &mut Shell, column, cx: &mut Context<Shell>| {
                this.liked_view.sort = ColumnSort::after_click(this.liked_view.sort, column);
                this.invalidate_page();
                cx.notify();
            });
            blocks.push(Shell::plain_block(move |this, layout, _, cx| this.render_table_header(layout, sorted, Some(on_sort.clone()), cx)));
            blocks.extend((0..shown.len()).map(|pos| Block::Table { list: table, pos, variant: RowVariant::Full }));
        }
        blocks
    }

    fn clear_liked_search_button(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        crate::ui_controls::empty_action(&t, "clear-liked-search")
            .stop(&t).on_click(cx.listener(|this, _, _, cx| {
                this.liked_view.query.clear();
                this.search_active = false;
                this.invalidate_page();
                cx.notify();
            }))
            .child(tr("Clear"))
            .into_any_element()
    }

    fn render_liked_actions(&mut self, list: usize, has_songs: bool, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let m = PageMetrics::of(layout);
        let sort = self.liked_view.sort.map(|s| s.column);
        div()
            .flex()
            .flex_row()
            .items_center()
            .flex_wrap()
            .gap(t.s(8.0))
            .px(t.s(m.gutter))
            .w(layout.main_width)
            .child(self.render_play_actions(list, has_songs, cx))
            .child(div().flex_1())
            .child(glass_pill(&t).child(icon_button(&t, "liked-sort", tr("Sort"), "arrows-down-up-regular", sort.is_some()).stop(&t).on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                // The list's own order first, then the column sorts (ascending).
                let rows = [(None, tr("Recently Liked")), (Some(SortColumn::Title), tr("Title")), (Some(SortColumn::Artist), tr("Artist")), (Some(SortColumn::Album), tr("Album")), (Some(SortColumn::Time), tr("Length"))]
                    .into_iter()
                    .map(|(o, label)| MenuRow::check(label, sort == o, move |this, cx| {
                        this.liked_view.sort = o.and_then(ColumnSort::asc);
                        this.invalidate_page();
                        cx.notify();
                    }))
                    .collect();
                this.open_toolbar_menu(rows, event, cx);
            }))))
            .child(self.search_pill(tr("Search Songs"), self.liked_view.query.clone(), cx))
            .into_any_element()
    }
}
