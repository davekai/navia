//! Home. Rows ranked by plays use the core's recent-taste score, and each row hides when empty.

use crate::i18n::{tr, trf};
use crate::pages::{AlbumScope, Block, CardItem, PageMetrics, Route};
use crate::shell::{Layout, Shell};

impl Shell {
    pub fn build_home(&mut self, layout: &Layout) -> Vec<Block> {
        let m = PageMetrics::of(layout);
        let taste = self.library.as_ref().map(|l| l.home_taste()).unwrap_or_default();
        let artists = self.artists.clone();
        let essentials: Vec<CardItem> = taste.essentials.iter().map(|i| CardItem::Essentials(*i)).collect();
        let world = taste.world_of.and_then(|i| artists.get(i as usize).cloned());
        let albums = self.albums.clone();
        let mut recent: Vec<u32> = (0..albums.len() as u32).collect();
        recent.sort_by_key(|i| std::cmp::Reverse(albums[*i as usize].added));
        let recent: Vec<CardItem> = recent.into_iter().take(16).map(|index| CardItem::Album { index, by_artist: false }).collect();
        let top: Vec<CardItem> = taste.albums.iter().take(16).map(|index| CardItem::Album { index: *index, by_artist: false }).collect();
        let artist_cards = |list: &[u32]| list.iter().map(|i| CardItem::Artist(*i)).collect::<Vec<_>>();
        let mut sections = Vec::new();
        let stations: Vec<CardItem> = taste.stations.iter().map(|(seed, list)| CardItem::ArtistStation { seed: *seed, neighbours: std::rc::Rc::new(list.clone()) }).collect();
        let mut radio = self.shelf_block(tr("Artist Radio"), None, stations, false, 0);
        if let Some(Block::Shelf(shelf)) = radio.first_mut()
            && let Some(shelf) = std::rc::Rc::get_mut(shelf)
        {
            shelf.note = Some(tr("Non-stop music from the artists you've been playing").into());
        }
        sections.push(radio);
        if !taste.on_repeat.is_empty() {
            self.page_mini_artist = false;
            let mut block = vec![Self::section_heading_with_note(tr("On Repeat").into(), tr("Last 4 weeks").into())];
            block.extend(self.mini_blocks(taste.on_repeat.clone(), layout, false, Some(tr("On Repeat"))));
            sections.push(block);
        }
        if let Some(world) = &world {
            let list: Vec<CardItem> = self.artist_albums(&world.id).into_iter().map(|index| CardItem::Album { index, by_artist: false }).collect();
            sections.push(self.shelf_block(trf("World of {name}", &[("name", &world.name)]), Some(Route::Artist(world.id.clone())), list, false, 2));
        }
        if let Some((seed, similar)) = &taste.because
            && let Some(seed) = artists.get(*seed as usize)
        {
            sections.push(self.shelf_block(trf("Because you've been playing {name}", &[("name", &seed.name)]), Some(Route::Artist(seed.id.clone())), artist_cards(similar), false, 0));
        }
        // The link opens Albums sorted by date added.
        let mut recent = self.shelf_block(tr("Recently Added"), Some(Route::Albums(AlbumScope::All)), recent, false, 2);
        if let Some(Block::Shelf(shelf)) = recent.first_mut()
            && let Some(shelf) = std::rc::Rc::get_mut(shelf)
        {
            shelf.link_sort = Some(crate::view_albums::AlbumSort::Added);
        }
        sections.push(recent);
        let upcoming = self.upcoming_home_cards();
        sections.push(self.shelf_block(tr("Upcoming"), None, upcoming, false, 1));
        sections.push(self.shelf_block(tr("Essentials"), Some(Route::Artists), essentials, false, 1));
        sections.push(self.shelf_block(tr("Most Played"), Some(Route::Albums(AlbumScope::All)), top, false, 2));
        sections.push(self.shelf_block(tr("Back in Rotation"), None, artist_cards(&taste.back_in_rotation), false, 0));
        let page = Shell::sections(m.section_gap, sections);
        if page.is_empty() {
            return vec![Shell::plain_block(|this, _, _, _| gpui::IntoElement::into_any_element(this.library_empty_state("music-notes-regular", tr("Nothing here yet"), tr("Home fills in once the server has music."), false)))];
        }
        page
    }
}
