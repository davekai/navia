//! Playlists: a grid or list, with sort, filter and search in the top bar.

use crate::ui::FullTextTip;
use crate::i18n::tr;
use gpui::prelude::*;
use navia_core::library::collections::CollectionKind;
use crate::focus::FocusStop;
use gpui::{FontWeight, div, px, relative};

use crate::model::{PlaylistDetails, Section};
use crate::ui_controls::segmented_glass;
use crate::pages::{Block, CardItem, PageMetrics, Route, grid_fit};
use crate::shell::{Layout, Shell};
use crate::theme::size::*;
use crate::view_albums::grid_blocks;
use crate::view_cards::long_duration;
use crate::view_menu::MenuRow;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PlaylistSort {
    Library,
    Name,
    Songs,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PlaylistKind {
    All,
    Smart,
    Regular,
}

/// All, Mine (yours, private or public) or Shared (other people's public playlists).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PlaylistScope {
    All,
    Mine,
    Shared,
    /// Playlists with Download on.
    Downloaded,
}

pub struct PlaylistsView {
    pub sort: PlaylistSort,
    pub kind: PlaylistKind,
    pub scope: PlaylistScope,
    pub list: bool,
    pub query: String,
}

impl Default for PlaylistsView {
    fn default() -> Self {
        Self { sort: PlaylistSort::Library, kind: PlaylistKind::All, scope: PlaylistScope::All, list: false, query: String::new() }
    }
}

impl Shell {
    pub fn playlists_sort_rows(&self) -> Vec<MenuRow> {
        [(PlaylistSort::Library, tr("Library Order")), (PlaylistSort::Name, tr("Name")), (PlaylistSort::Songs, tr("Song Count"))]
            .into_iter()
            .map(|(sort, label)| MenuRow::check(label, self.playlists_view.sort == sort, move |this, cx| {
                this.playlists_view.sort = sort;
                this.invalidate_page();
                cx.notify();
            }))
            .collect()
    }

    pub fn playlists_kind_rows(&self) -> Vec<MenuRow> {
        [(PlaylistKind::All, tr("All Playlists")), (PlaylistKind::Smart, tr("Smart")), (PlaylistKind::Regular, tr("Regular"))]
            .into_iter()
            .map(|(kind, label)| MenuRow::check(label, self.playlists_view.kind == kind, move |this, cx| {
                this.playlists_view.kind = kind;
                this.invalidate_page();
                cx.notify();
            }))
            .collect()
    }

    pub fn build_playlists(&mut self, layout: &Layout) -> Vec<Block> {
        let m = PageMetrics::of(layout);
        let all = self.playlists_full.clone();
        let needle = self.playlists_view.query.trim().to_lowercase();
        let kind = self.playlists_view.kind;
        let scope = self.playlists_view.scope;
        let mut found: Vec<u32> = (0..all.len() as u32)
            .filter(|i| {
                let p = &all[*i as usize];
                let of_kind = match kind {
                    PlaylistKind::All => true,
                    PlaylistKind::Smart => p.smart,
                    PlaylistKind::Regular => !p.smart,
                };
                let in_scope = match scope {
                    PlaylistScope::All => true,
                    PlaylistScope::Mine => p.section() == Section::Mine,
                    PlaylistScope::Shared => p.section() == Section::Shared,
                    PlaylistScope::Downloaded => self.collection_status(CollectionKind::Playlist, &p.id).is_some(),
                };
                in_scope && of_kind && (needle.is_empty() || p.name.to_lowercase().contains(&needle))
            })
            .collect();
        match self.playlists_view.sort {
            PlaylistSort::Library => {}
            PlaylistSort::Name => found.sort_by_cached_key(|i| all[*i as usize].name.to_lowercase()),
            PlaylistSort::Songs => found.sort_by_key(|i| std::cmp::Reverse(all[*i as usize].song_count)),
        }
        // When every playlist is yours, Mine and Shared add nothing. Downloaded always can.
        let others = all.iter().any(|p| p.section() != Section::Mine);
        let mut head = vec![Shell::plain_block(move |this, layout, _, cx| this.playlists_scope(layout, scope, others, cx)), Block::Spacer(24.0)];
        if found.is_empty() {
            let filtered = !needle.is_empty() || kind != PlaylistKind::All;
            let nothing_shared = scope == PlaylistScope::Shared && !filtered;
            let nothing_downloaded = scope == PlaylistScope::Downloaded && !filtered;
            let filtered = filtered || scope != PlaylistScope::All;
            head.push(Shell::plain_block(move |this, _, _, _| {
                if nothing_downloaded {
                    this.empty_state("playlist-regular", tr("No downloaded playlists"), tr("Turn on Download on a playlist to keep it on this device and in step with the server."), false).into_any_element()
                } else if nothing_shared {
                    this.empty_state("playlist-regular", tr("Nothing shared yet"), tr("Public playlists other people make on this server show up here."), false).into_any_element()
                } else if filtered {
                    this.empty_state("magnifying-glass-regular", tr("No playlists match"), tr("Nothing matches your search or filter."), false).into_any_element()
                } else {
                    this.library_empty_state("playlist-regular", tr("No playlists yet"), tr("Playlists from the server show up here."), false).into_any_element()
                }
            }));
            return head;
        }
        if self.playlists_view.list {
            let t = self.theme;
            head.extend(found
                .into_iter()
                .map(|index| {
                    let p: PlaylistDetails = all[index as usize].clone();
                    Shell::plain_block(move |this, layout, _, cx| {
                        let m = PageMetrics::of(layout);
                        let cover_id = this.playlist_art_key(&p.id);
                        let thumb = this.artwork(Some(&cover_id), "playlist-regular", 42.0, crate::view_cards::ArtShape::Rounded(R_SM), layout);
                        let route = Route::Playlist(p.id.clone());
                        let menu_id = p.id.clone();
                        let drop = this.drop_mark(format!("row:{}", p.id), this.playlist_drop(&p.id, &p.name), R_MD, 0.0);
                        div()
                            .id(("playlist-row", index as usize))
                            .relative()
                            .children(drop)
                            .flex()
                            .flex_row()
                            .items_center()
                            .h(t.s(ROW_H))
                            .mx(t.s(m.gutter))
                            .pl(t.s(5.0))
                            .text_color(t.t2())
                            .hover(|s| s.bg(t.row()))
                            .stop(&t).on_click(cx.listener(move |this, _, _, cx| this.navigate(route.clone(), cx)))
                            .on_mouse_down(gpui::MouseButton::Right, cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| this.open_playlist_menu(menu_id.clone(), event.position, cx)))
                            .child(div().w(t.s(55.0)).flex_none().child(thumb))
                            .child(div().id("name").flex_1().truncate().min_w(px(0.0)).font_weight(FontWeight::MEDIUM).text_color(t.t1()).child(p.name.clone()).full_text_tip(&t, p.name.clone()))
                            .child(div().w(relative(0.2)).flex_none().flex().flex_row().items_center().gap(t.s(DOWNLOAD_MARK_GAP)).min_w(px(0.0)).children(this.download_mark(CollectionKind::Playlist, &p.id)).child(div().min_w(px(0.0)).truncate().child(crate::view_cards::playlist_subtitle(&p))))
                            .child(div().w(relative(0.2)).flex_none().child(long_duration(p.duration_s)))
                            .child(div().absolute().left(px(0.0)).right(px(0.0)).bottom(px(0.0)).h(px(1.0)).bg(t.line()))
                            .into_any_element()
                    })
                })
            );
            return head;
        }
        let (columns, _) = grid_fit(self.grid_width(layout), m.grid_card, m.card_gap);
        head.extend(grid_blocks(found.into_iter().map(CardItem::Playlist).collect(), columns));
        head
    }

    /// The All, Mine and Shared choice above the grid.
    fn playlists_scope(&mut self, layout: &Layout, scope: PlaylistScope, others: bool, cx: &mut gpui::Context<Self>) -> gpui::AnyElement {
        let t = self.theme;
        let m = PageMetrics::of(layout);
        let mut options = vec![(PlaylistScope::All, tr("All||playlists"))];
        if others {
            options.extend([(PlaylistScope::Mine, tr("Mine||playlists")), (PlaylistScope::Shared, tr("Shared||playlists"))]);
        }
        options.push((PlaylistScope::Downloaded, tr("Downloaded||playlists")));
        div()
            .flex()
            .px(t.s(m.gutter))
            .child(segmented_glass(&t, "playlists-scope", scope, &options, cx, |this, next, cx| {
                this.playlists_view.scope = next;
                this.invalidate_page();
                cx.notify();
            }))
            .into_any_element()
    }
}
