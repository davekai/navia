//! The top bar's page-specific parts: titles, the center pill, and page toolbars.

use crate::i18n::tr;
use gpui::prelude::*;
use crate::focus::FocusStop;
use gpui::{AnyElement, Context, FontWeight, SharedString, div, px};

use crate::pages::{AlbumScope, DiscoverTab, Route};
use crate::shell::Shell;
use crate::theme::size::*;
use crate::ui::{divider, glass_pill, icon, icon_button};
use crate::ui_controls::TextTarget;
use crate::view_menu::MenuRow;
use crate::view_search::{SearchScope, SearchSort};

impl Shell {
    pub fn page_title(&self) -> Option<SharedString> {
        match &self.route {
            Route::Albums(AlbumScope::All) => Some(tr("Albums").into()),
            Route::Albums(AlbumScope::Artist(_, section)) => Some(crate::view_artist::section_title(*section).into()),
            Route::Artists => Some(tr("Artists").into()),
            Route::Playlists => Some(tr("Playlists").into()),
            Route::Genres => Some(tr("Genres").into()),
            Route::Equalizer => Some(tr("Equalizer").into()),
            Route::Downloads => Some(tr("Downloads").into()),
            Route::CachedSongs => Some(tr("Cached Songs").into()),
            Route::Shares => Some(tr("Share Links").into()),
            Route::SmartEditor(id) => Some(self.smart_page_title(id)),
            _ => None,
        }
    }

    /// Secondary text after the title, such as whose albums a "See all" page shows.
    pub fn page_title_detail(&self) -> Option<SharedString> {
        match &self.route {
            Route::Albums(AlbumScope::Artist(id, _)) => self.artists.iter().find(|a| &a.id == id).map(|a| a.name.clone()),
            Route::Artists => self.artists_title_detail(),
            _ => None,
        }
    }

    /// the Search page with its tools and too little room for both at full width: the field takes what the tools leave.
    pub fn search_crowded(&self, main_tokens: f32) -> bool {
        self.route == Route::Search && main_tokens <= SEARCH_FIELD_CROWDED
    }

    pub fn render_page_center(&mut self, main_tokens: f32, tokens_w: f32, cx: &mut Context<Self>) -> Option<AnyElement> {
        match self.route.clone() {
            Route::Discover(tab) => Some(self.render_discover_tabs(tab, main_tokens, cx)),
            Route::Browse | Route::Search => Some(self.render_search_field(main_tokens, tokens_w, cx)),
            Route::Albums(_) | Route::Artists | Route::Playlists | Route::Genres | Route::Equalizer | Route::Downloads | Route::CachedSongs | Route::Shares | Route::SmartEditor(_) => None,
            _ => Some(self.render_icon_nav_public(main_tokens, cx)),
        }
    }

    fn render_discover_tabs(&mut self, active: DiscoverTab, main_tokens: f32, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let pad = if main_tokens <= 620.0 {
            10.0
        } else if main_tokens <= 900.0 {
            14.0
        } else {
            24.0
        };
        let tabs = [(DiscoverTab::ForYou, tr("For You")), (DiscoverTab::New, tr("New")), (DiscoverTab::Charts, tr("Charts")), (DiscoverTab::Radio, tr("Radio"))];
        let mut pill = div().flex().flex_row().items_center().flex_none().h(t.s(53.0)).p(t.s(4.0)).rounded_full().map(|d| crate::ui::glass_layers(d, &t, t.s(53.0 / 2.0), GLASS_BLUR, GLASS_SATURATION, t.glass(), t.glass_fill(), crate::ui::edge_and(crate::ui::shadow_pill(&t)))).occlude();
        for (i, (tab, label)) in tabs.into_iter().enumerate() {
            let (segment, ink) = crate::ui::segment(&t, ("tab", i), tab == active, t.t2());
            pill = pill.child(
                segment
                    .flex()
                    .items_center()
                    .justify_center()
                    .h(t.s(44.0))
                    .px(t.s(pad))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(ink)
                    .stop(&t).on_click(cx.listener(move |this, _, _, cx| this.navigate(Route::Discover(tab), cx)))
                    .child(label),
            );
        }
        pill.into_any_element()
    }

    fn render_search_field(&mut self, main_tokens: f32, tokens_w: f32, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let width = if self.search_crowded(main_tokens) {
            let max = if main_tokens <= 620.0 { SEARCH_FIELD_W_MID } else { tokens_w * SEARCH_FIELD_MAX_SHARE };
            crate::ui::SearchWidth::Fill { min: SEARCH_FIELD_MIN_W, max }
        } else if main_tokens <= 620.0 {
            crate::ui::SearchWidth::Fixed(SEARCH_FIELD_W_MID.min(main_tokens * 0.45))
        } else if main_tokens <= 1000.0 {
            crate::ui::SearchWidth::Fixed(SEARCH_FIELD_W_MID)
        } else {
            crate::ui::SearchWidth::Fixed(SEARCH_FIELD_W.min(tokens_w * SEARCH_FIELD_MAX_SHARE))
        };
        let query = self.search_query.clone();
        let active = self.search_active;
        let text = self.field_line(&TextTarget::Page, &query, tr("Search..."), "search-query", t.text(TEXT_LG));
        let clear = self.search_clear(TextTarget::Page, "search-field-clear", &query, crate::ui::SearchSize::Large.clear(), cx);
        crate::ui::search_field(&t, "search-field", crate::ui::SearchSize::Large, width, active, text, clear)
            .on_click(cx.listener(|this, _, _, cx| this.focus_field(TextTarget::Page, cx)))
            .into_any_element()
    }

    pub fn render_page_tools(&mut self, main_tokens: f32, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let search_w = crate::ui::pill_search_width(main_tokens);
        match self.route.clone() {
            Route::Albums(_) => {
                let query = self.albums_view.query.clone();
                let sort = icon_button(&t, "albums-sort", tr("Sort"), "arrows-down-up-regular", false).stop(&t).on_click(cx.listener(|this, event: &gpui::ClickEvent, _, cx| {
                    let rows = this.albums_sort_rows();
                    this.open_toolbar_menu(rows, event, cx);
                }));
                let filter = icon_button(&t, "albums-filter", tr("Filter"), "funnel-simple-regular", self.albums_view.genre.is_some()).stop(&t).on_click(cx.listener(|this, event: &gpui::ClickEvent, _, cx| {
                    let rows = this.albums_filter_rows();
                    this.open_toolbar_menu(rows, event, cx);
                }));
                let views = self.layout_toggle(crate::view_split::SplitPage::Albums, cx);
                let search = self.toolbar_search(tr("Search Albums"), query, search_w, cx);
                self.toolbar(vec![glass_pill(&t).occlude().child(sort).child(divider(&t)).child(filter).into_any_element(), views, search])
            }
            Route::Playlists => {
                let query = self.playlists_view.query.clone();
                let list_mode = self.playlists_view.list;
                let sort = icon_button(&t, "playlists-sort", tr("Sort"), "arrows-down-up-regular", false).stop(&t).on_click(cx.listener(|this, event: &gpui::ClickEvent, _, cx| {
                    let rows = this.playlists_sort_rows();
                    this.open_toolbar_menu(rows, event, cx);
                }));
                let filter = icon_button(&t, "playlists-filter", tr("Filter"), "funnel-simple-regular", self.playlists_view.kind != crate::view_playlists::PlaylistKind::All).stop(&t).on_click(cx.listener(|this, event: &gpui::ClickEvent, _, cx| {
                    let rows = this.playlists_kind_rows();
                    this.open_toolbar_menu(rows, event, cx);
                }));
                let list_btn = icon_button(&t, "playlists-list-view", tr("List view"), if list_mode { "list-bullets-fill" } else { "list-bullets-regular" }, list_mode).stop(&t).on_click(cx.listener(|this, _, _, cx| {
                    this.playlists_view.list = true;
                    this.invalidate_page();
                    cx.notify();
                }));
                let grid_btn = icon_button(&t, "playlists-grid-view", tr("Grid view"), if list_mode { "grid-four-regular" } else { "grid-four-fill" }, !list_mode).stop(&t).on_click(cx.listener(|this, _, _, cx| {
                    this.playlists_view.list = false;
                    this.invalidate_page();
                    cx.notify();
                }));
                let search = self.toolbar_search(tr("Search Playlists"), query, search_w, cx);
                let new_button = crate::ui_controls::round_button(&t, "playlists-new", tr("New"), "plus-regular").occlude().stop(&t).on_click(cx.listener(|this, event: &gpui::ClickEvent, _, cx| {
                    let rows = this.new_playlist_rows();
                    this.open_toolbar_menu(rows, event, cx);
                }));
                self.toolbar(vec![new_button.into_any_element(), glass_pill(&t).occlude().child(sort).child(divider(&t)).child(filter).into_any_element(), glass_pill(&t).occlude().child(list_btn).child(grid_btn).into_any_element(), search])
            }
            Route::Search => {
                let v = &self.search_view;
                let (sort, favorites, scope) = (v.sort, v.favorites, v.scope);
                let sort_btn = icon_button(&t, "search-sort", tr("Sort"), "arrows-down-up-regular", false).stop(&t).on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                    let rows = [(SearchSort::Relevance, tr("Relevance")), (SearchSort::Name, tr("Name")), (SearchSort::Year, tr("Year"))]
                        .into_iter()
                        .map(|(s, label)| MenuRow::check(label, sort == s, move |this, cx| {
                            this.search_view.sort = s;
                            this.invalidate_page();
                            cx.notify();
                        }))
                        .collect();
                    this.open_toolbar_menu(rows, event, cx);
                }));
                let filter_btn = icon_button(&t, "search-filter", tr("Filter"), "funnel-simple-regular", favorites).stop(&t).on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                    let rows = [(false, tr("Everything")), (true, tr("Favorites"))]
                        .into_iter()
                        .map(|(f, label)| MenuRow::check(label, favorites == f, move |this, cx| {
                            this.search_view.favorites = f;
                            this.invalidate_page();
                            cx.notify();
                        }))
                        .collect();
                    this.open_toolbar_menu(rows, event, cx);
                }));
                let scope_btn = div()
                    .id("search-scope")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(t.s(8.0))
                    .h(t.s(34.0))
                    .px(t.s(12.0))
                    .rounded_full()
                    .font_weight(FontWeight::MEDIUM)
                    .child(crate::ui::hover_pill(&t))
                    .child(scope.label())
                    .child(icon(&t, "caret-up-down-bold", 15.0, t.t1()))
                    .stop(&t).on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                        let rows = [SearchScope::All, SearchScope::Artists, SearchScope::Albums, SearchScope::Songs]
                            .into_iter()
                            .map(|s| MenuRow::check(s.label(), scope == s, move |this, cx| {
                                this.search_view.scope = s;
                                this.invalidate_page();
                                cx.notify();
                            }))
                            .collect();
                        this.open_toolbar_menu(rows, event, cx);
                    }));
                self.toolbar(vec![glass_pill(&t).occlude().child(sort_btn).child(divider(&t)).child(filter_btn).into_any_element(), glass_pill(&t).occlude().child(scope_btn).into_any_element()])
            }
            Route::Artists if !self.artists.is_empty() => {
                let query = self.artists_view.query.clone();
                let views = self.layout_toggle(crate::view_split::SplitPage::Artists, cx);
                let search = self.toolbar_search(tr("Search Artists"), query, search_w, cx);
                self.toolbar(vec![views, search])
            }
            Route::Genres if !self.genres_view.page.all.is_empty() => {
                let query = self.genres_view.query.clone();
                self.toolbar_search(tr("Search Genres"), query, search_w, cx)
            }
            Route::SmartEditor(_) => self.render_smart_tools(cx),
            Route::Settings => {
                let query = self.settings_query.clone();
                self.toolbar_search(tr("Search Settings"), query, search_w, cx)
            }
            _ => div().into_any_element(),
        }
    }

    /// the page's refresh, after its other controls: on library pages it runs the library sync now and turns with
    /// any sync; on Home, New, Charts and Radio it reloads their rows and turns until they're in.
    pub fn render_refresh(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let library_page = match self.route {
            Route::Genres | Route::Albums(_) | Route::Artists | Route::Songs => true,
            Route::Home | Route::Discover(DiscoverTab::New | DiscoverTab::Charts | DiscoverTab::Radio) => false,
            _ => return None,
        };
        let t = self.theme;
        let busy = self.core.as_ref().is_some_and(|c| c.lib.is_refreshing(library_page));
        let refresh = crate::ui::refresh_button(&t, busy, cx.listener(|this, _, _, cx| {
            if let Some(core) = this.core.as_ref() {
                core.lib.refresh_now();
            }
            cx.notify();
        }));
        Some(refresh.into_any_element())
    }

    fn toolbar(&mut self, children: Vec<AnyElement>) -> AnyElement {
        let t = self.theme;
        div().flex().flex_row().items_center().flex_shrink(1.0).gap(t.s(10.0)).children(children).into_any_element()
    }

    fn toolbar_search(&mut self, placeholder: &'static str, query: String, step: f32, cx: &mut Context<Self>) -> AnyElement {
        self.fitted_search(["toolbar-search", "toolbar-search-clear", "toolbar-query"], placeholder, query, step, cx)
    }

    /// a page's search field ([field, clear button, text line] ids): as wide as its placeholder, at least `step`;
    /// narrowed by a full bar, it says plain "Search" rather than a cut placeholder.
    pub fn fitted_search(&mut self, ids: [&'static str; 3], placeholder: &'static str, query: String, step: f32, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let size = crate::ui::SearchSize::Toolbar;
        let fit = size.fit(&t, cx, placeholder).max(t.s(step));
        let store = self.bounds_store(ids[0]);
        let room = store.borrow().size.width;
        let fits = room <= px(0.0) || room + px(0.5) >= fit;
        let line = self.field_line(&TextTarget::Page, &query, if fits { placeholder } else { tr("Search") }, ids[2], t.text(TEXT_MD));
        let clear = self.search_clear(TextTarget::Page, ids[1], &query, size.clear(), cx);
        crate::ui::search_field(&t, ids[0], size, crate::ui::SearchWidth::Fit { step, fit }, self.search_active, line, clear)
            .child(crate::ui::watch_fit(store, fit, fits))
            .on_click(cx.listener(|this, _, _, cx| this.focus_field(TextTarget::Page, cx)))
            .into_any_element()
    }
}
