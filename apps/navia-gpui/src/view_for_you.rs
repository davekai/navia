//! Discover's For You rows and the mix page, from the core's `library::for_you`. Rows hide when the server has no data
//! for them. Nothing is created on the server until Save as Playlist.

use crate::ui::FullTextTip;
use crate::i18n::{tr, trf};
use gpui::prelude::*;
use crate::focus::FocusStop;
use gpui::{AnyElement, Context, FontWeight, SharedString, div, px, relative};

use crate::model::MixInfo;
use crate::pages::{Block, CardItem, PageMetrics, RowVariant, Route};
use crate::shell::{Layout, Shell};
use crate::theme::size::*;
use crate::ui::glass_pill;
use crate::view_cards::{ArtShape, songs_and_length};

/// "<Artist> Mix", "Jazz Mix".
pub fn mix_title(mix: &MixInfo) -> SharedString {
    trf("{name} Mix", &[("name", &mix.name)]).into()
}

/// The artists a mix holds: "A, B, C and more".
pub fn mix_artists_line(mix: &MixInfo) -> SharedString {
    let names = mix.artists.iter().take(3).map(|a| a.as_ref()).collect::<Vec<_>>().join(", ");
    if mix.artists.len() > 3 { trf("{names} and more", &[("names", &names)]).into() } else { names.into() }
}

/// The art key for a mix's cover, a grid of its albums (`art::make_album_grid`).
pub fn mix_art_key(mix: &MixInfo) -> SharedString {
    format!("mix:{}", mix.covers.iter().map(|c| c.as_ref()).collect::<Vec<_>>().join("|")).into()
}

impl Shell {
    pub fn build_for_you(&mut self, m: PageMetrics, layout: &Layout) -> Vec<Block> {
        let page = self.library.as_ref().map(|l| l.for_you()).unwrap_or_default();
        self.for_you = page.clone();
        let albums = |list: &[u32]| list.iter().map(|index| CardItem::Album { index: *index, by_artist: false }).collect::<Vec<_>>();
        let mut sections = vec![
            self.shelf_block(tr("Your Mixes"), None, (0..page.mixes.len() as u32).map(CardItem::Mix).collect(), false, 2),
            self.shelf_block(tr("New to You"), None, albums(&page.new_to_you), false, 2),
            self.shelf_block(tr("Unplayed from Your Top Artists"), None, albums(&page.unplayed), false, 2),
        ];
        if !page.forgotten.is_empty() {
            self.page_mini_artist = false;
            let mut block = vec![Self::section_heading_with_note(tr("Forgotten Favorites").into(), tr("Not played in 6 months").into())];
            block.extend(self.mini_blocks(page.forgotten.clone(), layout, false, Some(tr("Forgotten Favorites"))));
            sections.push(block);
        }
        let mut today = self.shelf_block(tr("On This Day"), None, albums(&page.on_this_day), false, 2);
        if let Some(Block::Shelf(shelf)) = today.first_mut()
            && let Some(shelf) = std::rc::Rc::get_mut(shelf)
        {
            shelf.note = Some(crate::i18n::fmt_day_month(navia_core::library::taste::local_day(navia_core::library::taste::now_unix())).into());
        }
        sections.push(today);
        sections.push(self.shelf_block(tr("Not Played Recently"), None, albums(&page.not_played_recently), false, 2));
        let loading = self.for_you.loading;
        let page = Shell::sections(m.section_gap, sections);
        if page.is_empty() && !loading {
            return vec![Shell::plain_block(|this, _, _, _| this.library_empty_state("clock-regular", tr("Nothing here yet"), tr("These rows fill in once you've built up some play history."), false).into_any_element())];
        }
        page
    }

    /// The day changed while running, so rows that follow the local day are rebuilt.
    pub fn new_local_day(&mut self, cx: &mut Context<Self>) {
        self.invalidate_page();
        cx.notify();
    }

    fn mix_by_id(&mut self, id: &str) -> Option<MixInfo> {
        if !self.for_you.mixes.iter().any(|m| m.id.as_ref() == id) {
            self.for_you = self.library.as_ref().map(|l| l.for_you()).unwrap_or_default();
        }
        self.for_you.mixes.iter().find(|m| m.id.as_ref() == id).cloned()
    }

    /// A mix's tile, like a playlist card with a "Mix" chip on the cover. No hover play.
    pub fn render_mix_card(&mut self, index: u32, width: f32, group: SharedString, ix: usize, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let Some(mix) = self.for_you.mixes.get(index as usize).cloned() else { return div().into_any_element() };
        let art = self.mix_cover(&mix, width, layout);
        let route = Route::Mix(mix.id.clone());
        crate::ui::press_dim(div().id(("card", ix)))
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .w(t.s(width))
            .line_height(relative(1.3))
            .child(art)
            .child(div().id("name").mt(t.s(CARD_TEXT_GAP)).truncate().font_weight(FontWeight::MEDIUM).child(mix_title(&mix)).full_text_tip(&t, mix_title(&mix)))
            .child(div().id("sub").text_color(t.t2()).line_clamp(2).text_ellipsis().child(mix_artists_line(&mix)).full_text_tip(&t, mix_artists_line(&mix)))
            .child(crate::ui::card_track(&group))
            .stop(&t)
            .on_click(cx.listener(move |this, _, _, cx| this.navigate(route.clone(), cx)))
            .into_any_element()
    }

    /// A mix's cover, a grid of its albums, with the "Mix" chip.
    fn mix_cover(&mut self, mix: &MixInfo, size: f32, layout: &Layout) -> AnyElement {
        let key = mix_art_key(mix);
        let art = self.artwork(Some(&key), "playlist-regular", size, ArtShape::Rounded(R_MD), layout);
        div().relative().size(self.theme.s(size)).child(art).child(crate::ui::cover_label(&self.theme, tr("Mix"), crate::ui::LabelAt::Bottom, crate::ui::LabelStyle::Chip)).into_any_element()
    }

    pub fn build_mix(&mut self, id: &SharedString, _layout: &Layout) -> Vec<Block> {
        let Some(mix) = self.mix_by_id(id) else { return vec![Self::not_found(tr("This mix isn't here today"))] };
        let mut shown: Vec<(usize, u32)> = mix.songs.iter().copied().enumerate().collect();
        if let Some(sort) = self.mix_sort {
            self.sort_places(&mut shown, sort);
        }
        let shown: Vec<u32> = shown.into_iter().map(|(_, s)| s).collect();
        self.core_preload_rows(&mix.songs);
        let list = self.add_list(mix.songs.clone());
        let table = self.add_list(shown.clone());
        let head_mix = mix.clone();
        let head = Shell::plain_block(move |this, layout, _, cx| this.render_mix_head(&head_mix, layout, cx));
        let actions_mix = mix.clone();
        let actions = Shell::plain_block(move |this, layout, _, cx| this.render_mix_actions(&actions_mix, list, layout, cx));
        let sort = self.mix_sort;
        let on_sort: crate::view_rows::OnSort = std::rc::Rc::new(move |this: &mut Shell, column, cx: &mut Context<Shell>| {
            this.mix_sort = crate::view_rows::ColumnSort::after_click(this.mix_sort, column);
            this.invalidate_page();
            cx.notify();
        });
        let mut blocks = vec![head, Block::Spacer(28.0), actions, Block::Spacer(32.0)];
        blocks.push(Shell::plain_block(move |this, layout, _, cx| this.render_table_header(layout, sort, Some(on_sort.clone()), cx)));
        blocks.extend((0..shown.len()).map(|pos| Block::Table { list: table, pos, variant: RowVariant::Full }));
        blocks
    }

    fn render_mix_head(&mut self, mix: &MixInfo, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let m = PageMetrics::of(layout);
        let cover = crate::ui::hero_art_frame(&t, R_MD, self.mix_cover(mix, m.hero_art, layout));
        let meta = format!("{} • {}", songs_and_length(mix.songs.len() as u32, mix.duration_s), tr("Updated today"));
        let info = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.0))
            .child(div().text_size(t.text(m.title)).text_color(t.t_em()).font_weight(FontWeight::BOLD).line_height(relative(1.1)).line_clamp(3).text_ellipsis().id("mix-title").child(mix_title(mix)).full_text_tip(&t, mix_title(mix)))
            .child(div().mt(t.s(14.0)).child(meta))
            .child(self.header_more_text("mix-artists", crate::view_about::AboutText { title: mix_title(mix), subtitle: Some(tr("Mix").into()), art: Some(mix_art_key(mix)), text: mix_artists_line(mix) }, cx));
        let main = f32::from(layout.main_width) / t.token_scale();
        let mut head = div().flex().gap(t.s(25.0)).px(t.s(m.gutter)).w(layout.main_width).items_center();
        head = if main <= 620.0 { head.flex_col().items_start() } else { head.flex_row() };
        head.child(cover).child(info).into_any_element()
    }

    fn render_mix_actions(&mut self, mix: &MixInfo, list: usize, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let m = PageMetrics::of(layout);
        let (name, songs) = (mix_title(mix).to_string(), mix.songs.clone());
        let save = crate::view_album::text_button(&t, "mix-save", "plus-fill", tr("Save as Playlist"), !self.saving_mix)
            .stop(&t)
            .on_click(cx.listener(move |this, _, _, cx| this.save_mix(name.clone(), songs.clone(), cx)));
        div()
            .flex()
            .flex_row()
            .items_center()
            .flex_wrap()
            .gap(t.s(8.0))
            .px(t.s(m.gutter))
            .w(layout.main_width)
            .child(self.render_play_actions(list, !mix.songs.is_empty(), cx))
            .child(glass_pill(&t).child(save))
            .into_any_element()
    }

    /// Saves today's songs as a regular playlist through the core's write gate. A read-only run is refused and says so.
    fn save_mix(&mut self, name: String, songs: Vec<u32>, cx: &mut Context<Self>) {
        if self.saving_mix {
            return;
        }
        match self.core.as_ref() {
            Some(core) => {
                self.saving_mix = true;
                core.lib.create_playlist_with(name, songs);
            }
            None => self.show_toast(tr("Creating a playlist needs the server."), cx),
        }
        cx.notify();
    }
}
