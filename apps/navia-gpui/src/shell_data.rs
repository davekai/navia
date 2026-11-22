//! library data for pages, cached so a page never asks the library twice.

use crate::i18n::tr;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{Animation, AnimationExt, AnyElement, Context, Pixels, RenderImage, SharedString, div, point, px};

use crate::art::{BakeKind, Job};
use crate::color::{Srgb, tint_vivid};
use crate::theme::motion;
use crate::model::{Artist, Library};
use crate::pages::{AlbumScope, DiscoverTab, Route};
use crate::model::ReleaseSection;
use crate::shell::{Layout, Shell};

const BAKE_DEBOUNCE: Duration = Duration::from_millis(140);
/// How many decoded bytes the page bakes may hold before the least recently drawn go. Each is
/// window-sized, around 13 MB on a 3:2 laptop at 150%, so the limit is in bytes.
const HERO_BAKE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Default)]
pub struct BakeCache {
    /// Width, height, the image, and the frame it was last requested in.
    pub items: HashMap<(BakeKind, SharedString), (u32, u32, Arc<RenderImage>, u64)>,
    pub generation: u64,
    pub pending: Option<(BakeKind, SharedString, u32, u32)>,
}

impl BakeCache {
    /// Drops every bake and its pending request while the window is hidden, so the next draw asks again.
    pub fn release(&mut self) -> Vec<Arc<RenderImage>> {
        self.pending = None;
        self.generation += 1;
        self.items.drain().map(|(_, (_, _, image, _))| image).collect()
    }
}

impl Shell {
    pub fn load_catalog(&mut self, library: &Arc<dyn Library>) {
        self.albums = library.albums();
        self.artists = library.artists();
        self.playlists_full = library.playlist_details();
        self.genres = library.genres();
        self.genres_view.page = library.genre_page();
        self.artist_index = self.artists.iter().enumerate().map(|(i, a)| (a.name.clone(), i as u32)).collect();
        self.album_stars = self.albums.iter().filter(|a| a.starred).map(|a| a.id.clone()).collect();
        self.artist_stars = self.artists.iter().filter(|a| a.starred).map(|a| a.id.clone()).collect();
        self.smart_library_changed();
        self.invalidate_page();
    }

    pub fn artist_named(&self, name: &SharedString) -> Option<&Artist> {
        self.artist_index.get(name).and_then(|i| self.artists.get(*i as usize))
    }

    pub fn top_songs(&mut self, artist_id: &SharedString) -> Vec<u32> {
        if let Some(cached) = self.top_song_cache.get(artist_id) {
            return cached.clone();
        }
        let top = self.library.as_ref().map(|l| l.artist_top_songs(artist_id)).unwrap_or_default();
        self.top_song_cache.insert(artist_id.clone(), top.clone());
        top
    }

    pub fn similar_songs(&mut self, artist_id: &SharedString) -> Vec<u32> {
        if let Some(cached) = self.similar_cache.get(artist_id) {
            return cached.clone();
        }
        let found = self.library.as_ref().map(|l| l.similar_songs(artist_id)).unwrap_or_default();
        self.similar_cache.insert(artist_id.clone(), found.clone());
        found
    }

    pub fn artist_albums(&mut self, artist_id: &SharedString) -> Vec<u32> {
        self.library.as_ref().map(|l| l.artist_albums(artist_id)).unwrap_or_default()
    }

    /// The artist's albums by release type. `None` while the answer is on its way.
    pub fn artist_discography(&self, artist_id: &SharedString) -> Option<crate::model::Discography> {
        self.library.as_ref().and_then(|l| l.artist_discography(artist_id))
    }

    /// One section of the artist's albums. Empty while unknown.
    pub fn artist_section(&self, artist_id: &SharedString, section: ReleaseSection) -> Vec<u32> {
        self.artist_discography(artist_id).and_then(|d| d.into_iter().find(|(s, _)| *s == section)).map(|(_, a)| a).unwrap_or_default()
    }

    pub fn artist_album_count(&self, artist_id: &SharedString) -> usize {
        self.library.as_ref().map_or(0, |l| l.artist_albums(artist_id).len())
    }

    pub fn album_tracks_cached(&mut self, album_id: &SharedString) -> Vec<u32> {
        if let Some(cached) = self.album_track_cache.get(album_id) {
            return cached.clone();
        }
        let tracks: Vec<u32> = self.library.as_ref().map(|l| l.album_tracks(album_id).into_iter().map(|t| t.song).collect()).unwrap_or_default();
        self.album_track_cache.insert(album_id.clone(), tracks.clone());
        tracks
    }

    /// Top songs, or anything by the artist when the server has no top songs, which need Last.fm.
    pub fn artist_playable(&mut self, artist_id: &SharedString) -> Vec<u32> {
        let top = self.top_songs(artist_id);
        if !top.is_empty() {
            return top;
        }
        self.library.as_ref().map(|l| l.artist_songs(artist_id)).unwrap_or_default()
    }

    pub fn top_genre(&mut self, artist_id: &SharedString) -> SharedString {
        let top = self.top_songs(artist_id);
        top.first().and_then(|ix| self.song_peek(*ix)).map(|s| s.genre.clone()).unwrap_or_default()
    }

    pub fn play_artist_top(&mut self, artist_id: &SharedString, cx: &mut Context<Self>) {
        let songs = self.artist_playable(artist_id);
        self.play_songs(songs, 0, false, cx);
    }

    pub fn start_artist_radio(&mut self, artist_id: &SharedString, cx: &mut Context<Self>) {
        if self.core.is_some() {
            self.core_start_radio(crate::core_player::RadioSeed::Artist(artist_id.to_string()), cx);
            return;
        }
        let songs = self.similar_songs(artist_id);
        self.play_songs(songs, 0, false, cx);
    }

    /// Starts a queue of similar songs beginning with this one. Hides the option for this song if
    /// the server has nothing.
    pub fn start_song_radio(&mut self, song_ix: u32, cx: &mut Context<Self>) {
        if self.core.is_some() {
            if let Some(song) = self.song_peek(song_ix) {
                self.core_start_radio(crate::core_player::RadioSeed::Song { id: song.id.to_string(), rowid: song_ix }, cx);
            }
            return;
        }
        let artist = self.song_peek(song_ix).and_then(|s| self.artist_named(&s.artist).map(|a| a.id.clone()));
        let similar: Vec<u32> = artist.map(|id| self.similar_songs(&id)).unwrap_or_default();
        let mut queue = vec![song_ix];
        queue.extend(similar.iter().copied().filter(|s| *s != song_ix));
        if queue.len() > 1 {
            self.play_songs(queue, 0, false, cx);
        } else if let Some(song) = self.song_peek(song_ix) {
            self.no_song_radio.insert(song.id);
            self.show_toast(tr("The server has no similar songs for that one"), cx);
        }
    }

    /// Plays a song from Search at once, and fills Up Next with its radio when the server answers.
    /// The radio has its own order, so shuffle turns off.
    pub fn play_as_radio(&mut self, song_ix: u32, cx: &mut Context<Self>) {
        if self.player.shuffle {
            self.toggle_shuffle(cx);
        }
        if self.core.is_none() {
            let artist = self.song_peek(song_ix).and_then(|s| self.artist_named(&s.artist).map(|a| a.id.clone()));
            let mut queue = vec![song_ix];
            queue.extend(artist.map(|id| self.similar_songs(&id)).unwrap_or_default().into_iter().filter(|s| *s != song_ix));
            self.play_songs(queue, 0, false, cx);
            return;
        }
        let Some(song) = self.song_peek(song_ix) else { return };
        self.play_source = Some(navia_core::queue_order::QueueSource { name: song.title.to_string(), radio: true, link: None });
        self.play_songs(vec![song_ix], 0, false, cx);
        // Offline, a song that needs the server doesn't start, and there's no radio either.
        if self.player.song_ix != Some(song_ix as usize) {
            return;
        }
        self.radio_pending = Some(self.plays);
        self.core_start_radio(crate::core_player::RadioSeed::After { id: song.id.to_string(), rowid: song_ix, play: self.plays }, cx);
    }

    pub fn open_genre(&mut self, genre: SharedString, cx: &mut Context<Self>) {
        self.song_filters.state = crate::view_filters::LibraryFilters::for_genre(Some(&genre));
        self.rebuild_order();
        self.navigate(Route::Songs, cx);
    }

    /// Opens Songs with no genre filter.
    pub fn open_all_songs(&mut self, cx: &mut Context<Self>) {
        if self.song_filters.is_filtering() {
            self.song_filters.state.clear();
            self.rebuild_order();
        }
        self.navigate(Route::Songs, cx);
    }

    pub fn album_starred(&self, id: &SharedString) -> bool {
        self.album_stars.contains(id)
    }

    pub fn artist_starred(&self, id: &SharedString) -> bool {
        self.artist_stars.contains(id)
    }

    pub fn toggle_album_star(&mut self, id: &SharedString, cx: &mut Context<Self>) {
        if self.is_offline() {
            self.show_toast(tr(crate::core_player::STARS_NEED_SERVER), cx);
            return;
        }
        let on = !self.album_stars.remove(id);
        if on {
            self.album_stars.insert(id.clone());
        }
        self.core_star_album(id, on);
        cx.notify();
    }

    pub fn toggle_artist_star(&mut self, id: &SharedString, cx: &mut Context<Self>) {
        if self.is_offline() {
            self.show_toast(tr(crate::core_player::STARS_NEED_SERVER), cx);
            return;
        }
        let on = !self.artist_stars.remove(id);
        if on {
            self.artist_stars.insert(id.clone());
        }
        self.core_star_artist(id, on);
        cx.notify();
    }

    pub fn album_tint(&mut self, album_id: &SharedString) -> Srgb {
        if let Some(tint) = self.tints.get(album_id) {
            return *tint;
        }
        if self.tint_requested.insert(album_id.clone()) {
            self.art.request(Job::Tint { album_id: album_id.clone() });
        }
        Srgb::hex(crate::theme::TINT_FALLBACK)
    }

    /// An artist's tint, from their first album.
    pub fn artist_tint(&mut self, artist_id: &SharedString) -> Srgb {
        let first = self.artist_albums(artist_id).first().copied();
        match first.and_then(|i| self.albums.get(i as usize)).map(|a| a.id.clone()) {
            Some(id) => self.album_tint(&id),
            None => Srgb::hex(0x555555),
        }
    }

    /// The tint behind the top of a page, and any glow behind the sidebar.
    pub fn render_page_backdrop(&mut self, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let scroll = if self.page_uses_list() && self.page.route.is_some() { self.page_scroll_y() } else { px(0.0) };
        let width = layout.viewport.width;
        let tint = match self.route.clone() {
            Route::Album(id) => Some(self.album_tint(&id)),
            Route::Upcoming(_) => self.upcoming_cover.clone().map(|cover| self.album_tint(&cover)),
            _ => None,
        };
        let mut layer = div().absolute().inset_0().overflow_hidden();
        if let Route::Artist(id) | Route::ArtistTop(id) = &self.route {
            if let Some(image) = self.artists.iter().find(|a| &a.id == id).and_then(|a| a.image.clone()) {
                let (w, h) = ((f32::from(width) * layout.scale) as u32, (f32::from(layout.viewport.height) * 0.82 * layout.scale) as u32);
                // The glow scrolls with the page.
                if let Some(glow) = self.bake_for(BakeKind::PageGlow, &image, w, h, cx) {
                    layer = layer.child(crate::ui::image(glow).absolute().left(px(0.0)).top(scroll).w(width).h(layout.viewport.height * 0.82));
                }
            }
        }
        let grad = |tint: Srgb| page_tint_gradient(tint, t.s(460.0), width, scroll);
        if let Some(tint) = tint {
            layer = layer.child(grad(tint));
        }
        // A playlist page takes its cover's tint, a mix its album grid's.
        let playlist = match &self.route {
            Route::Playlist(id) => self.playlists_full.iter().find(|p| &p.id == id).map(|_| (id.clone(), self.playlist_art_key(id))),
            Route::Mix(id) => self.for_you.mixes.iter().find(|m| &m.id == id).map(|m| (id.clone(), crate::art::genre_tint_key(&m.covers))),
            Route::Liked => Some((crate::art::LIKED_COVER.into(), crate::art::LIKED_COVER.into())),
            _ => None,
        };
        match playlist {
            Some((id, key)) => {
                let state = self.playlist_cover_tint(&key);
                self.playlist_tints.fade.advance(&id, state);
                let fade = &self.playlist_tints.fade;
                if fade.animating {
                    let animation = || Animation::new(motion::duration(motion::TINT_FADE)).with_easing(gpui::ease_in_out);
                    if let Some(old) = fade.previous {
                        layer = layer.child(grad(old).with_animation(("playlist-tint-out", fade.generation), animation(), |el, delta| el.opacity(1.0 - delta)));
                    }
                    if let Some(new) = fade.shown {
                        layer = layer.child(grad(new).with_animation(("playlist-tint-in", fade.generation), animation(), |el, delta| el.opacity(delta)));
                    }
                } else if let Some(new) = fade.shown {
                    layer = layer.child(grad(new));
                }
            }
            None => self.playlist_tints.fade.owner = None,
        }
        layer.into_any_element()
    }

    /// The state of a playlist cover's tint. Failures and placeholders are retried with backoff,
    /// so a playlist whose songs arrive later still gets its tint.
    pub fn playlist_cover_tint(&mut self, key: &SharedString) -> CoverTint {
        let now = Instant::now();
        let tints = &mut self.playlist_tints;
        let entry = tints.entries.entry(key.clone()).or_insert_with(|| PlaylistTintEntry { tint: None, answered: false, generation: 0, retry_at: now, failures: 0 });
        if entry.tint.is_none() {
            if now >= entry.retry_at {
                tints.next_generation += 1;
                entry.generation = tints.next_generation;
                entry.retry_at = now + if entry.answered { retry_backoff(entry.failures) } else { LOST_TINT_JOB };
                self.art.request(Job::PlaylistTint { key: key.clone(), generation: entry.generation });
            }
            let due = entry.retry_at;
            self.next_thumb_retry = Some(self.next_thumb_retry.map_or(due, |at: Instant| at.min(due)));
        }
        let entry = &self.playlist_tints.entries[key];
        if entry.answered || entry.tint.is_some() { CoverTint::Ready(entry.tint) } else { CoverTint::Loading }
    }

    pub fn apply_playlist_tint(&mut self, key: SharedString, generation: u64, tint: Option<Srgb>) {
        let Some(entry) = self.playlist_tints.entries.get_mut(&key).filter(|e| e.generation == generation) else { return };
        entry.answered = true;
        entry.tint = tint;
        entry.failures = if tint.is_some() { 0 } else { entry.failures.saturating_add(1) };
        entry.retry_at = Instant::now() + retry_backoff(entry.failures);
    }

    /// Forgets the tint after a cover change so it's read again.
    pub fn forget_playlist_tint(&mut self, key: &str) {
        self.playlist_tints.entries.remove(key);
    }

    /// A baked image at this size, or the last one while a new size waits for resizing to settle.
    pub fn bake_for(&mut self, kind: BakeKind, id: &SharedString, w: u32, h: u32, cx: &mut Context<Self>) -> Option<Arc<RenderImage>> {
        let key = (kind, id.clone());
        let frame = self.frame;
        let have = self.hero_bakes.items.get_mut(&key).map(|item| {
            item.3 = frame;
            (item.0, item.1, item.2.clone())
        });
        let matches = have.as_ref().is_some_and(|(hw, hh, _)| *hw == w && *hh == h);
        if !matches && !self.dock_resizing() {
            let want = (kind, id.clone(), w, h);
            if self.hero_bakes.pending.as_ref() != Some(&want) {
                self.hero_bakes.pending = Some(want);
                self.hero_bakes.generation += 1;
                let generation = self.hero_bakes.generation;
                let art = self.art.clone();
                let (id2, debounce) = (id.clone(), have.is_some());
                cx.spawn(async move |this, cx| {
                    if debounce {
                        cx.background_executor().timer(BAKE_DEBOUNCE).await;
                    }
                    let _ = this.update(cx, |this, _| {
                        if this.hero_bakes.generation == generation {
                            art.request(Job::Bake { kind, id: id2, width: w, height: h });
                        }
                    });
                })
                .detach();
            }
        }
        have.map(|(_, _, image)| image)
    }

    pub fn apply_bake(&mut self, kind: BakeKind, id: SharedString, width: u32, height: u32, image: Option<Arc<RenderImage>>) {
        if let Some(image) = image {
            if let Some((_, _, old, _)) = self.hero_bakes.items.insert((kind, id.clone()), (width, height, image, self.frame)) {
                self.lyrics_view.garbage.push(old);
            }
        }
        if self.hero_bakes.pending.as_ref().is_some_and(|p| p.0 == kind && p.1 == id && p.2 == width && p.3 == height) {
            self.hero_bakes.pending = None;
        }
        // Keep a few within HERO_BAKE_BYTES, since users go back and forth between artists. The least
        // recently drawn go first, but never one drawn in the last two frames.
        let bytes = |items: &HashMap<(BakeKind, SharedString), (u32, u32, Arc<RenderImage>, u64)>| items.values().map(|(w, h, _, _)| *w as usize * *h as usize * 4).sum::<usize>();
        while bytes(&self.hero_bakes.items) > HERO_BAKE_BYTES {
            let frame = self.frame;
            let Some(oldest) = self.hero_bakes.items.iter().filter(|(_, item)| item.3 + 2 < frame).min_by_key(|(_, item)| item.3).map(|(key, _)| key.clone()) else { break };
            if let Some((_, _, image, _)) = self.hero_bakes.items.remove(&oldest) {
                self.lyrics_view.garbage.push(image);
            }
        }
    }

    /// Resets filters and searches, which belong to one page visit.
    pub fn reset_page_state(&mut self) {
        self.albums_view.query.clear();
        self.albums_view.genre = None;
        self.albums_view.pane.selected = None;
        self.artists_view.pane.selected = None;
        // A link can ask for a sort, like Home's Recently Added opening Albums by date added.
        self.albums_view.sort = self.albums_sort_on_open.take().unwrap_or(crate::view_albums::AlbumSort::MostPlayed);
        self.album_detail = Default::default();
        self.artist_detail = Default::default();
        self.playlist_detail = Default::default();
        self.liked_view = Default::default();
        self.playlists_view.query.clear();
        self.settings_query.clear();
        self.artists_view = Default::default();
        self.playlists_view.kind = crate::view_playlists::PlaylistKind::All;
        self.playlists_view.scope = crate::view_playlists::PlaylistScope::All;
        self.selection.clear();
        self.clear_control_bounds();
        self.reset_smart_editor();
        self.invalidate_page();
    }

    /// The text field the current page types into.
    fn active_field(&mut self) -> Option<&mut String> {
        match &self.route {
            Route::Songs => Some(&mut self.query),
            Route::Liked => Some(&mut self.liked_view.query),
            Route::Albums(_) => Some(&mut self.albums_view.query),
            Route::Album(_) => Some(&mut self.album_detail.query),
            Route::Artist(_) => Some(&mut self.artist_detail.query),
            Route::Playlists => Some(&mut self.playlists_view.query),
            Route::Genres => Some(&mut self.genres_view.query),
            Route::Artists => Some(&mut self.artists_view.query),
            Route::Playlist(_) => Some(&mut self.playlist_detail.query),
            Route::Browse | Route::Search => Some(&mut self.search_query),
            Route::Settings => Some(&mut self.settings_query),
            _ => None,
        }
    }

    pub fn page_has_field(&self) -> bool {
        matches!(self.route, Route::Songs | Route::Liked | Route::Albums(_) | Route::Album(_) | Route::Artists | Route::Artist(_) | Route::Playlists | Route::Genres | Route::Playlist(_) | Route::Browse | Route::Search | Route::Settings)
    }

    pub fn page_field_value(&self) -> String {
        match &self.route {
            Route::Songs => self.query.clone(),
            Route::Liked => self.liked_view.query.clone(),
            Route::Albums(_) => self.albums_view.query.clone(),
            Route::Album(_) => self.album_detail.query.clone(),
            Route::Artist(_) => self.artist_detail.query.clone(),
            Route::Playlists => self.playlists_view.query.clone(),
            Route::Genres => self.genres_view.query.clone(),
            Route::Artists => self.artists_view.query.clone(),
            Route::Playlist(_) => self.playlist_detail.query.clone(),
            Route::Browse | Route::Search => self.search_query.clone(),
            Route::Settings => self.settings_query.clone(),
            _ => String::new(),
        }
    }

    /// Applies an edit to the current page's search field.
    pub fn set_page_field(&mut self, value: String, cx: &mut Context<Self>) {
        let route = self.route.clone();
        let Some(field) = self.active_field() else { return };
        *field = value;
        let now_empty = field.is_empty();
        if route == Route::Songs {
            self.rebuild_order();
            self.list_scroll.0.borrow().base_handle.set_offset(point(px(0.0), px(0.0)));
            cx.notify();
            return;
        }
        match (route, now_empty) {
            // The first letter is a real history step. Emptying the field goes back to Browse.
            (Route::Browse, false) => self.replace_search_route(Route::Search),
            (Route::Search, true) => self.replace_search_route(Route::Browse),
            (Route::Artists, _) => self.artists_query_changed(),
            _ => {}
        }
        self.invalidate_page();
        cx.notify();
    }

    fn replace_search_route(&mut self, route: Route) {
        self.route = route.clone();
        if let Some(slot) = self.history.get_mut(self.history_pos) {
            *slot = route;
        }
    }

    pub fn page_script(&mut self, op: &str, viewport: gpui::Size<gpui::Pixels>, cx: &mut Context<Self>) {
        match op {
            "albums-split" => self.prefs.album_split = true,
            "albums-grid" => self.prefs.album_split = false,
            "artists-split" => self.prefs.artist_split = true,
            "artists-grid" => self.prefs.artist_split = false,
            "playlists-list" => self.playlists_view.list = true,
            "focus" => self.search_active = true,
            // Dialogs for screenshots, an empty New Playlist and a confirmation.
            "new-playlist" => self.open_new_playlist(cx),
            "confirm" => self.confirm = Some(crate::view_connect::Confirm::ClearCache),
            // The output device menu, as the player's output button opens it.
            "output-menu" => self.open_output_menu(point(px(600.0), px(300.0)), cx),
            // `menu-quick` focuses the quick row, `menu-quick-right` moves along, `menu-quick-open` presses as Enter would.
            "menu-quick" => self.script_menu_quick(cx),
            "menu-quick-right" => self.script_menu_quick_move(1, cx),
            "menu-quick-open" => self.menu_activate(cx),
            // sample receivers, for previews.
            op if op.starts_with("airplay-") => self.airplay_preview(&op["airplay-".len()..], cx),
            // The page's filter row.
            op if op.starts_with("filter-") => self.filter_script(&op["filter-".len()..], viewport, cx),
            // The open artist's About sheet.
            "about" => {
                if let crate::pages::Route::Artist(id) = self.route.clone() {
                    self.open_artist_about(id, cx);
                }
            }
            // the open playlist's description sheet, even when MORE isn't shown.
            "more" => {
                if let crate::pages::Route::Playlist(id) = self.route.clone()
                    && let Some(p) = self.playlists_full.iter().find(|p| p.id == id).cloned()
                {
                    let about = self.playlist_about(&p);
                    self.open_about_text(about, cx);
                }
            }
            // share links: needs `NAVIA_TEST_SHARE=1` in a test run, which otherwise refuses writes.
            "share-new" => {
                if let Some(album) = self.albums.first() {
                    let target = navia_core::shares::ShareTarget { kind: navia_core::shares::ShareKind::Album, ids: vec![album.id.to_string()] };
                    let subject = self.share_subject(&target, &[]);
                    self.open_share(target, subject, cx);
                }
            }
            "share-submit" => self.share_submit(cx),
            // The open playlist's Edit Details dialog and Public toggle. `edit-name:` types a name.
            "edit-details" | "public-toggle" => {
                if let crate::pages::Route::Playlist(id) = self.route.clone() {
                    if op == "edit-details" { self.open_edit_playlist(id, cx) } else { self.toggle_playlist_public(id, cx) }
                }
            }
            "edit-save" => self.edit_playlist_submit(cx),
            "share-delete-made" => {
                if let Some(id) = self.shares.dialog.as_ref().and_then(|d| d.made.as_ref()).map(|l| l.id.clone()) {
                    self.shares.dialog = None;
                    self.delete_share(id, cx);
                }
            }
            // Reports how many song rows are on screen, to check rows after a mode switch.
            "rows-report" => eprintln!("navia: rows visible {} on {:?} in {:?}", self.visible_song_rows(), self.route, self.prefs.mode),
            // Reports the open artist's discography sections, to check grouping against the server.
            "discography-report" => {
                if let Route::Artist(id) = self.route.clone() {
                    match self.artist_discography(&id) {
                        None => eprintln!("navia: discography of {id} still loading"),
                        Some(sections) => {
                            for (section, list) in sections {
                                let names: Vec<String> = list.iter().filter_map(|i| self.albums.get(*i as usize)).map(|a| format!("{} [{}]", a.name, a.artist)).collect();
                                eprintln!("navia: discography {section:?} {}: {}", names.len(), names.join(" | "));
                            }
                        }
                    }
                }
            }
            "menu" => {
                let position = point(px(420.0), px(260.0));
                let songs: Vec<u32> = self.songs.iter().take(1).enumerate().map(|(i, _)| i as u32).collect();
                self.open_song_menu(songs.clone(), Some(Self::play_songs_row(&songs)), position, false, cx);
            }
            other => {
                if self.eq_script(other, cx) || self.smart_script(other, cx) || self.settings_script(other, cx) {
                    self.invalidate_page();
                    cx.notify();
                    return;
                }
                if let Some(name) = other.strip_prefix("new-name:") {
                    // the upload tile's slot presses it.
                    self.set_text_value(&crate::ui_controls::TextTarget::NewPlaylistName, name.replace('_', " "), cx);
                } else if let Some(slot) = other.strip_prefix("cover-slot:").and_then(|v| v.parse::<usize>().ok()) {
                    self.script_cover_slot(slot, cx);
                } else if other == "new-create" {
                    self.new_playlist_submit(cx);
                } else if let Some(name) = other.strip_prefix("edit-name:") {
                    self.set_edit_playlist_text(&crate::ui_controls::TextTarget::PlaylistName, name.to_string(), cx);
                } else if let Some(id) = other.strip_prefix("cover-upload:") {
                    self.pick_playlist_image(id.to_string(), cx);
                } else if let Some(id) = other.strip_prefix("cover-remove:") {
                    self.remove_playlist_image(id.to_string(), cx);
                } else if let Some(y) = other.strip_prefix("pagescroll+").and_then(|v| v.parse::<f32>().ok()) {
                    // for screenshots of rows further down.
                    self.page.state.scroll_by(px(y));
                } else if let Some(q) = other.strip_prefix("search:") {
                    self.search_query = q.replace('_', " ");
                    if self.route == Route::Browse {
                        self.replace_search_route(Route::Search);
                    }
                } else if let Some(rest) = other.strip_prefix("scroll:") {
                    if let Ok(y) = rest.parse::<f32>() {
                        self.page.state.scroll_by(px(y));
                    }
                } else if let Some(rest) = other.strip_prefix("shelf:") {
                    let mut parts = rest.split('+');
                    let (ix, x): (usize, f32) = (parts.next().and_then(|v| v.parse().ok()).unwrap_or(0), parts.next().and_then(|v| v.parse().ok()).unwrap_or(0.0));
                    let shelf = self.page.blocks.iter().filter_map(|b| if let crate::pages::Block::Shelf(s) = b { Some(s.clone()) } else { None }).nth(ix);
                    if let Some(shelf) = shelf {
                        shelf.scroll.set_offset(point(px(-x), px(0.0)));
                    }
                    // A rebuild would drop the shelf's own scroll position.
                    cx.notify();
                    return;
                }
            }
        }
        self.invalidate_page();
        cx.notify();
    }

    pub fn script_route(&self, name: &str) -> Route {
        if let Some(id) = name.strip_prefix("Playlist:") {
            return Route::Playlist(id.to_string().into());
        }
        let first_album = self.albums.first().map(|a| a.id.clone()).unwrap_or_default();
        let first_artist = self.artists.iter().find(|a| a.image.is_some()).or(self.artists.first()).map(|a| a.id.clone()).unwrap_or_default();
        let first_playlist = self.playlists_full.first().map(|p| p.id.clone()).unwrap_or_default();
        if let Some(id) = name.strip_prefix("upcoming:").and_then(|id| id.parse().ok()) {
            return Route::Upcoming(id);
        }
        if let Some(album) = name.strip_prefix("album:").map(|p| p.replace('_', " ")) {
            let id = self.albums.iter().find(|a| a.name.starts_with(album.as_str())).map(|a| a.id.clone()).unwrap_or_default();
            return Route::Album(id);
        }
        // `artist-albums:Name[:singles|compilations|appears]` opens one section of an artist's albums.
        if let Some(rest) = name.strip_prefix("artist-albums:") {
            let (artist, section) = rest.split_once(':').unwrap_or((rest, "albums"));
            let artist = artist.replace('_', " ");
            let section = match section {
                "singles" => ReleaseSection::SinglesAndEps,
                "compilations" => ReleaseSection::Compilations,
                "appears" => ReleaseSection::AppearsOn,
                _ => ReleaseSection::Albums,
            };
            let id = self.artists.iter().find(|a| a.name.starts_with(artist.as_str())).map(|a| a.id.clone()).unwrap_or_default();
            return Route::Albums(AlbumScope::Artist(id, section));
        }
        if let Some(artist) = name.strip_prefix("artist-top:").map(|p| p.replace('_', " ")) {
            let id = self.artists.iter().find(|a| a.name.starts_with(artist.as_str())).map(|a| a.id.clone()).unwrap_or_default();
            return Route::ArtistTop(id);
        }
        if let Some(artist) = name.strip_prefix("artist:").map(|p| p.replace('_', " ")) {
            let id = self.artists.iter().find(|a| a.name.starts_with(artist.as_str())).map(|a| a.id.clone()).unwrap_or_default();
            return Route::Artist(id);
        }
        match name {
            "Home" => Route::Home,
            "New" => Route::Discover(DiscoverTab::New),
            "ForYou" => Route::Discover(DiscoverTab::ForYou),
            "Charts" => Route::Discover(DiscoverTab::Charts),
            "Radio" => Route::Discover(DiscoverTab::Radio),
            "Albums" => Route::Albums(AlbumScope::All),
            "Album" => Route::Album(first_album),
            "Artists" => Route::Artists,
            "Artist" => Route::Artist(first_artist),
            "Liked" => Route::Liked,
            "Playlists" => Route::Playlists,
            "Playlist" => Route::Playlist(first_playlist),
            "Browse" => Route::Browse,
            "Genres" => Route::Genres,
            "Search" => Route::Search,
            "Settings" => Route::Settings,
            "Equalizer" => Route::Equalizer,
            "Downloads" => Route::Downloads,
            "CachedSongs" => Route::CachedSongs,
            "Shares" => Route::Shares,
            "Smart" => Route::SmartEditor(None),
            "SmartOwn" => Route::SmartEditor(self.playlists_full.iter().find(|p| p.smart && p.owned).map(|p| p.id.clone())),
            "SmartOther" => Route::SmartEditor(self.playlists_full.iter().find(|p| p.smart && !p.owned).map(|p| p.id.clone())),
            _ => Route::Songs,
        }
    }
}


/// How long before a tint job counts as lost. The worker queue can drop it when thumbnails flood it.
const LOST_TINT_JOB: Duration = Duration::from_secs(20);

fn retry_backoff(failures: u32) -> Duration {
    Duration::from_secs((2u64 << failures.min(5)).min(60))
}

/// One flat tint with fading alpha over the page, so translucent windows stay translucent. It's
/// `total` tall, scrolls with the page, goes from 56% to 34% over 62% of it, then fades out.
fn page_tint_gradient(tint: Srgb, total: Pixels, width: Pixels, scroll: Pixels) -> gpui::Div {
    let vivid = tint_vivid(tint).mix(tint, 0.5);
    let color = |a: f32| vivid.hsla(a);
    div()
        .absolute()
        .left(px(0.0))
        .w(width)
        .top(scroll)
        .h(total)
        .flex()
        .flex_col()
        .child(div().w_full().h(total * 0.62).bg(gpui::linear_gradient(180.0, gpui::linear_color_stop(color(0.56), 0.0), gpui::linear_color_stop(color(0.34), 1.0))))
        .child(div().w_full().h(total * 0.38).bg(gpui::linear_gradient(180.0, gpui::linear_color_stop(color(0.34), 0.0), gpui::linear_color_stop(color(0.0), 1.0))))
}

/// Tints read from playlist covers, and the fade on the open playlist's page.
#[derive(Default)]
pub struct PlaylistTints {
    entries: HashMap<SharedString, PlaylistTintEntry>,
    next_generation: u64,
    pub fade: TintFade,
}

struct PlaylistTintEntry {
    tint: Option<Srgb>,
    /// Whether a worker has answered at least once, so `tint: None` means no art rather than unknown.
    answered: bool,
    /// so a read from before an upload can't land after it.
    generation: u64,
    retry_at: Instant,
    failures: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CoverTint {
    Loading,
    Ready(Option<Srgb>),
}

/// What the page shows, and what it's fading from.
#[derive(Default, Debug)]
pub struct TintFade {
    pub owner: Option<SharedString>,
    pub shown: Option<Srgb>,
    pub previous: Option<Srgb>,
    pub generation: u64,
    pub animating: bool,
}

impl TintFade {
    /// A new playlist starts from what's known with no fade. Within one playlist a new color fades
    /// in, and `Loading` keeps the last color so a cover change doesn't dip to neutral.
    pub fn advance(&mut self, playlist: &SharedString, state: CoverTint) {
        if self.owner.as_ref() != Some(playlist) {
            self.owner = Some(playlist.clone());
            self.shown = if let CoverTint::Ready(tint) = state { tint } else { None };
            self.previous = None;
            self.animating = false;
            self.generation += 1;
        } else if let CoverTint::Ready(tint) = state
            && tint != self.shown
        {
            self.previous = self.shown;
            self.shown = tint;
            self.animating = true;
            self.generation += 1;
        }
    }
}

#[cfg(test)]
mod tint_fade_tests {
    use super::*;

    const BLUE: Srgb = Srgb { r: 0.1, g: 0.2, b: 0.5 };
    const RED: Srgb = Srgb { r: 0.5, g: 0.1, b: 0.1 };

    #[test]
    fn a_page_opens_neutral_and_fades_in_when_the_tint_is_known() {
        let (mut fade, id) = (TintFade::default(), SharedString::from("a"));
        fade.advance(&id, CoverTint::Loading);
        assert_eq!((fade.shown, fade.animating), (None, false));
        fade.advance(&id, CoverTint::Ready(Some(BLUE)));
        assert_eq!((fade.shown, fade.previous, fade.animating), (Some(BLUE), None, true));
    }

    #[test]
    fn a_cover_change_holds_the_last_color_until_the_new_one_is_read() {
        let (mut fade, id) = (TintFade::default(), SharedString::from("a"));
        fade.advance(&id, CoverTint::Ready(Some(BLUE)));
        assert!(!fade.animating, "known on arrival: no fade");
        let generation = fade.generation;
        fade.advance(&id, CoverTint::Loading);
        assert_eq!((fade.shown, fade.generation), (Some(BLUE), generation));
        fade.advance(&id, CoverTint::Ready(Some(RED)));
        assert_eq!((fade.shown, fade.previous, fade.animating), (Some(RED), Some(BLUE), true));
    }

    #[test]
    fn removing_the_last_art_fades_back_to_neutral() {
        let (mut fade, id) = (TintFade::default(), SharedString::from("a"));
        fade.advance(&id, CoverTint::Ready(Some(BLUE)));
        fade.advance(&id, CoverTint::Ready(None));
        assert_eq!((fade.shown, fade.previous, fade.animating), (None, Some(BLUE), true));
    }

    #[test]
    fn another_playlist_starts_clean() {
        let (mut fade, a, b) = (TintFade::default(), SharedString::from("a"), SharedString::from("b"));
        fade.advance(&a, CoverTint::Ready(Some(BLUE)));
        fade.advance(&b, CoverTint::Loading);
        assert_eq!((fade.shown, fade.previous, fade.animating), (None, None, false));
    }
}
