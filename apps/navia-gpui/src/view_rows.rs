//! Song rows on pages: the table on playlists and albums, and compact rows on artists, search and discover.

use crate::ui::FullTextTip;
use crate::i18n::tr;
use gpui::prelude::*;
use crate::ui::LinkCursor;
use crate::focus::FocusStop;
use gpui::{AnyElement, Context, FontWeight, Hsla, MouseButton, Pixels, Window, div, px, relative};

use crate::pages::{PageMetrics, RowVariant};
use crate::shell::{Layout, ListId, Shell};
use crate::theme::size::*;
use crate::ui::{clock, icon, icon_button_colored};

/// The column a song list is sorted by, when it has a header.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SortColumn {
    Title,
    Time,
    Artist,
    Album,
    Genre,
    Added,
    Plays,
}

/// A song table column the listener can show or hide from the header's context menu. Song Name and Time always show.
/// Declaration order is display order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OptCol {
    Artist,
    Album,
    Genre,
    Added,
    Plays,
}

impl OptCol {
    pub const ALL: [OptCol; 5] = [OptCol::Artist, OptCol::Album, OptCol::Genre, OptCol::Added, OptCol::Plays];

    /// The name saved in prefs.
    pub fn key(self) -> &'static str {
        match self {
            OptCol::Artist => "artist",
            OptCol::Album => "album",
            OptCol::Genre => "genre",
            OptCol::Added => "added",
            OptCol::Plays => "plays",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            OptCol::Artist => tr("Artist"),
            OptCol::Album => tr("Album"),
            OptCol::Genre => tr("Genre"),
            OptCol::Added => tr("Date Added"),
            OptCol::Plays => tr("Plays"),
        }
    }

    pub fn head(self) -> &'static str {
        match self {
            OptCol::Artist => tr("Artist name"),
            OptCol::Album => tr("Album name"),
            OptCol::Genre => tr("Genre"),
            OptCol::Added => tr("Date Added"),
            OptCol::Plays => tr("Plays"),
        }
    }

    pub fn sort(self) -> SortColumn {
        match self {
            OptCol::Artist => SortColumn::Artist,
            OptCol::Album => SortColumn::Album,
            OptCol::Genre => SortColumn::Genre,
            OptCol::Added => SortColumn::Added,
            OptCol::Plays => SortColumn::Plays,
        }
    }
}

/// a column an album table can show, from its Columns button or the header's right-click; saved apart from the other
/// tables' (`Prefs::album_columns`). Albums have no sorting, and Heat is offered only with the Deezer switch on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AlbumCol {
    Time,
    Plays,
    Heat,
}

impl AlbumCol {
    pub const ALL: [AlbumCol; 3] = [AlbumCol::Time, AlbumCol::Plays, AlbumCol::Heat];

    pub fn key(self) -> &'static str {
        match self {
            AlbumCol::Time => "time",
            AlbumCol::Plays => "plays",
            AlbumCol::Heat => "heat",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AlbumCol::Time => tr("Time"),
            AlbumCol::Plays => tr("Plays"),
            AlbumCol::Heat => tr("Heat"),
        }
    }
}

/// widths of a full song table's columns: narrow tables drop Genre, Date Added and Plays, then Album, then Artist.
#[derive(Clone, Copy)]
pub struct TableCols {
    pub title: Pixels,
    pub time: Pixels,
    pub actions: Pixels,
    pub cols: [Option<Pixels>; 5],
}

pub fn table_cols(theme: &crate::theme::Theme, width: f32, lead: f32, chosen: [bool; 5]) -> TableCols {
    let tokens = width / theme.token_scale();
    let on = |c: OptCol| chosen[c as usize];
    // Column, then fr share or fixed tokens.
    enum W {
        Fr(f32),
        Fixed(f32),
    }
    let (title_fr, time, actions, cols): (f32, f32, f32, Vec<(OptCol, W)>) = if tokens <= 500.0 {
        (1.0, 58.0, 124.0, vec![])
    } else if tokens <= 700.0 {
        (1.0, 62.0, 124.0, [OptCol::Artist].into_iter().filter(|c| on(*c)).map(|c| (c, W::Fr(1.0))).collect())
    } else if tokens <= 900.0 {
        (2.0, 70.0, 132.0, [(OptCol::Artist, 1.2), (OptCol::Album, 1.4)].into_iter().filter(|(c, _)| on(*c)).map(|(c, f)| (c, W::Fr(f))).collect())
    } else {
        let all = [(OptCol::Artist, W::Fr(172.0)), (OptCol::Album, W::Fr(228.0)), (OptCol::Genre, W::Fr(95.0)), (OptCol::Added, W::Fixed(122.0)), (OptCol::Plays, W::Fixed(76.0))];
        (298.0, 78.0, 132.0, all.into_iter().filter(|(c, _)| on(*c)).collect())
    };
    let fixed: f32 = 5.0 + lead + time + actions + cols.iter().map(|(_, w)| if let W::Fixed(t) = w { *t } else { 0.0 }).sum::<f32>();
    let frs: f32 = title_fr + cols.iter().map(|(_, w)| if let W::Fr(f) = w { *f } else { 0.0 }).sum::<f32>();
    let unit = ((width - fixed * theme.token_scale()) / frs).max(0.0);
    let mut out = [None; 5];
    for (c, w) in cols {
        out[c as usize] = Some(match w {
            W::Fr(f) => px(f * unit),
            W::Fixed(t) => theme.s(t),
        });
    }
    TableCols { title: px(title_fr * unit), time: theme.s(time), actions: theme.s(actions), cols: out }
}

/// A column sort. Clicking a header cycles ascending, descending, then the list's own order, which is None.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ColumnSort {
    pub column: SortColumn,
    pub ascending: bool,
}

impl ColumnSort {
    pub fn asc(column: SortColumn) -> Option<Self> {
        Some(Self { column, ascending: true })
    }

    /// What a click on `column`'s header does to the current sort.
    pub fn after_click(current: Option<Self>, column: SortColumn) -> Option<Self> {
        match current {
            Some(c) if c.column == column && c.ascending => Some(Self { column, ascending: false }),
            Some(c) if c.column == column => None,
            _ => Self::asc(column),
        }
    }

    /// Saved per playlist as "title:asc".
    pub fn key(self) -> String {
        let column = match self.column {
            SortColumn::Title => "title",
            SortColumn::Time => "time",
            SortColumn::Artist => "artist",
            SortColumn::Album => "album",
            SortColumn::Genre => "genre",
            SortColumn::Added => "added",
            SortColumn::Plays => "plays",
        };
        format!("{column}:{}", if self.ascending { "asc" } else { "desc" })
    }

    pub fn from_key(key: &str) -> Option<Self> {
        let (column, dir) = key.split_once(':')?;
        let column = match column {
            "title" => SortColumn::Title,
            "time" => SortColumn::Time,
            "artist" => SortColumn::Artist,
            "album" => SortColumn::Album,
            "genre" => SortColumn::Genre,
            "added" => SortColumn::Added,
            "plays" => SortColumn::Plays,
            _ => return None,
        };
        Some(Self { column, ascending: dir != "desc" })
    }
}

/// The header click handler, given the clicked column.
pub type OnSort = std::rc::Rc<dyn Fn(&mut Shell, SortColumn, &mut Context<Shell>)>;

#[derive(Clone)]
pub struct UnavailableRow {
    pub number: u32,
    pub title: gpui::SharedString,
    pub artist: gpui::SharedString,
    pub explicit: bool,
    pub duration_s: u32,
    pub rule_above: bool,
    pub last: bool,
}

#[derive(Clone, Copy)]
struct RowCols {
    width: Pixels,
    lead: Pixels,
    title: Pixels,
    /// last, beside the row's buttons; an album table can hide it.
    time: Option<Pixels>,
    cols: [Option<Pixels>; 5],
    heat: Option<Pixels>,
    actions: Pixels,
    gutter: Pixels,
}

impl Shell {
    /// the album table's columns the listener chose and can have now: Heat only with the Deezer switch on.
    pub fn album_columns_offered(&self) -> Vec<AlbumCol> {
        AlbumCol::ALL.into_iter().filter(|c| *c != AlbumCol::Heat || self.deezer.choice == Some(true)).collect()
    }

    fn album_column_on(&self, col: AlbumCol) -> bool {
        self.prefs.album_columns[col as usize] && self.album_columns_offered().contains(&col)
    }

    /// narrow album tables keep only Time.
    fn row_columns(&self, variant: RowVariant, layout: &Layout) -> RowCols {
        let t = self.theme;
        let m = PageMetrics::of(layout);
        let width = f32::from(layout.main_width) - f32::from(t.s(m.gutter)) * 2.0;
        let tokens = width / t.token_scale();
        match variant {
            RowVariant::Album => {
                let narrow = tokens <= 500.0;
                let (lead, time_w, actions) = if narrow { (40.0, 58.0, 124.0) } else { (55.0, TIME_COL_W, 132.0) };
                let wide = tokens > 700.0;
                let time = self.album_column_on(AlbumCol::Time).then_some(time_w);
                let plays = (wide && self.album_column_on(AlbumCol::Plays)).then_some(PLAYS_COL_W);
                let heat = (wide && self.album_column_on(AlbumCol::Heat)).then_some(POPULARITY_COL_W);
                let fixed = 5.0 + lead + actions + time.unwrap_or(0.0) + plays.unwrap_or(0.0) + heat.unwrap_or(0.0);
                let mut cols = [None; 5];
                cols[OptCol::Plays as usize] = plays.map(|w| t.s(w));
                RowCols { width: px(width), lead: t.s(lead), title: px((width - fixed * t.token_scale()).max(0.0)), time: time.map(|w| t.s(w)), cols, heat: heat.map(|w| t.s(w)), actions: t.s(actions), gutter: t.s(m.gutter) }
            }
            RowVariant::Full => {
                let c = table_cols(&t, width, 55.0, self.prefs.columns);
                RowCols { width: px(width), lead: t.s(55.0), title: c.title, time: Some(c.time), cols: c.cols, heat: None, actions: c.actions, gutter: t.s(m.gutter) }
            }
        }
    }

    /// album numbers need their labels; Time alone reads without one.
    pub fn album_header_shown(&self, layout: &Layout) -> bool {
        let c = self.row_columns(RowVariant::Album, layout);
        c.heat.is_some() || c.cols.iter().any(Option::is_some)
    }

    /// Sorts song rows by a column, keeping ties in their current order.
    pub fn sort_rows(&self, rows: &mut [u32], sort: ColumnSort) {
        self.sort_by_song(rows, sort, |s| *s);
    }

    /// Sorts rows that carry their place in a list along with the song.
    pub fn sort_places(&self, rows: &mut [(usize, u32)], sort: ColumnSort) {
        self.sort_by_song(rows, sort, |(_, s)| *s);
    }

    fn sort_by_song<R>(&self, rows: &mut [R], sort: ColumnSort, song_of: impl Fn(&R) -> u32) {
        let key = |r: &R| -> (String, i64) {
            let Some(song) = self.song_peek(song_of(r)) else { return (String::new(), 0) };
            match sort.column {
                SortColumn::Title => (song.title.to_lowercase(), 0),
                SortColumn::Artist => (song.artist.to_lowercase(), 0),
                SortColumn::Album => (song.album.to_lowercase(), 0),
                SortColumn::Genre => (song.genre.to_lowercase(), 0),
                SortColumn::Time => (String::new(), i64::from(song.duration_s)),
                // Oldest and fewest first, like A to Z on the other columns.
                SortColumn::Added => (String::new(), song.added),
                SortColumn::Plays => (String::new(), i64::from(song.plays)),
            }
        };
        rows.sort_by_cached_key(key);
        if !sort.ascending {
            rows.reverse();
        }
    }

    /// Every header starts at its column's left edge, and Song Name spans the artwork column too. With `on_sort`,
    /// clicking a header sorts by it and the sorted one shows ▲ or ▼.
    pub fn render_table_header(&mut self, layout: &Layout, sorted: Option<ColumnSort>, on_sort: Option<OnSort>, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let c = self.row_columns(RowVariant::Full, layout);
        let label = |w: Pixels, text: &'static str, column: SortColumn| {
            let on = sorted.filter(|s| s.column == column);
            let mut inner = sort_head(&t, ("table-head", column as usize), text, on.map(|s| s.ascending));
            if let Some(on_sort) = on_sort.clone() {
                inner = inner.link_cursor().hover(|s| s.text_color(t.t1())).on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    on_sort(this, column, cx);
                }));
            }
            head_cell(&t, w, is_number(column)).child(inner)
        };
        let mut row = table_head_row(&t, t.s(ROW_H), c.width, c.gutter, t.s(5.0)).child(label(c.lead + c.title, tr("Song Name"), SortColumn::Title));
        for col in OptCol::ALL {
            if let Some(w) = c.cols[col as usize] {
                row = row.child(label(w, col.head(), col.sort()));
            }
        }
        if let Some(w) = c.time {
            row = row.child(label(w, tr("Time"), SortColumn::Time));
        }
        row.id("table-header")
            .on_mouse_down(MouseButton::Right, cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| this.open_column_chooser(event.position, cx)))
            .child(div().absolute().left(px(0.0)).right(px(0.0)).bottom(px(0.0)).h(px(1.0)).bg(t.line()))
            .into_any_element()
    }

    /// an album table's labels, shown only with a column other than Time; no sorting. right-click opens its chooser.
    pub fn render_album_header(&mut self, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let c = self.row_columns(RowVariant::Album, layout);
        let label = |w: Pixels, text: &'static str, number: bool| head_cell(&t, w, number).child(div().truncate().child(text));
        let mut row = table_head_row(&t, t.s(MINI_ROW_H), c.width, c.gutter, t.s(5.0)).child(label(c.lead + c.title, tr("Song Name"), false));
        if let Some(w) = c.cols[OptCol::Plays as usize] {
            row = row.child(label(w, tr("Plays"), true));
        }
        if let Some(w) = c.heat {
            row = row.child(label(w, tr("Heat"), false));
        }
        if let Some(w) = c.time {
            row = row.child(label(w, tr("Time"), true));
        }
        row.id("album-table-header")
            .on_mouse_down(MouseButton::Right, cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                let rows = this.album_column_rows();
                this.open_menu(rows, event.position, false, cx);
            }))
            .child(div().absolute().left(px(0.0)).right(px(0.0)).bottom(px(0.0)).h(px(1.0)).bg(t.line()))
            .into_any_element()
    }

    /// right-click on a song table's header: the optional columns, remembered and shared by every table.
    pub fn open_column_chooser(&mut self, position: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        let rows = OptCol::ALL
            .into_iter()
            .map(|col| {
                crate::view_menu::MenuRow::check(col.label(), self.prefs.columns[col as usize], move |this, cx| {
                    this.prefs.columns[col as usize] = !this.prefs.columns[col as usize];
                    this.prefs.save();
                    this.invalidate_page();
                    cx.notify();
                })
            })
            .collect();
        self.open_menu(rows, position, false, cx);
    }

    /// the album table's chooser, for its header's right-click and the page's Columns button.
    pub fn album_column_rows(&self) -> Vec<crate::view_menu::MenuRow> {
        self.album_columns_offered()
            .into_iter()
            .map(|col| {
                crate::view_menu::MenuRow::check(col.label(), self.prefs.album_columns[col as usize], move |this, cx| {
                    this.prefs.album_columns[col as usize] = !this.prefs.album_columns[col as usize];
                    this.prefs.save();
                    this.invalidate_page();
                    cx.notify();
                })
            })
            .collect()
    }

    /// One optional column's cell in a song row: names are links (`song_links.rs`).
    pub fn optional_cell(&mut self, col: OptCol, key: usize, song: &crate::model::Song, color: Hsla, cx: &mut Context<Self>) -> AnyElement {
        match col {
            OptCol::Artist => self.artist_links(("row-artist", key), song, color, cx),
            OptCol::Album => self.album_link(("row-album", key), song, color, cx),
            OptCol::Genre => self.genre_link(("row-genre", key), song, color, cx),
            OptCol::Added => div().truncate().text_color(color).child(crate::i18n::fmt_date(song.added)).into_any_element(),
            OptCol::Plays => div().w_full().flex().justify_end().text_color(color).child(if song.plays > 0 { crate::i18n::fmt_int(i64::from(song.plays)) } else { String::new() }).into_any_element(),
        }
    }

    pub fn render_table_row(&mut self, list: usize, pos: usize, variant: RowVariant, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let Some((song_ix, songs)) = self.list_song(list, pos) else { return div().into_any_element() };
        let Some(song) = self.song_at(song_ix) else { return div().into_any_element() };
        let c = self.row_columns(variant, layout);
        let is_now = self.player.song_ix == Some(song_ix as usize);
        let downloaded = self.is_downloaded(song_ix);
        let offline_opacity = self.offline_row_opacity(song_ix);
        let selected = self.selection_list == ListId::Page(list) && self.selection.contains(song_ix);
        let group = SharedGroup::new("page-row", pos);

        let fills = row_fills(&t, selected, downloaded && variant == RowVariant::Full);
        let name_color: Hsla = if is_now {
            t.accent_soft_hsla()
        } else {
            t.t1().into()
        };
        let dim_color: Hsla = t.t2().into();

        let thumb_px = (f32::from(t.s(THUMB)) * layout.scale).round() as u32;
        let hover_play = row_hover_play(&t, ("row-play", pos), group.name()).on_click(cx.listener(move |this, _, _, cx| {
            cx.stop_propagation();
            this.play_page_list(list, pos, false, cx);
        }));
        let lead_content: AnyElement = match variant {
            RowVariant::Album => {
                let number = self.page_numbers.get(pos).copied().flatten().unwrap_or(pos as u32 + 1);
                div().text_color(dim_color).group_hover(group.name(), |s| s.opacity(0.0)).child(number.to_string()).into_any_element()
            }
            RowVariant::Full => row_thumb(&t, self.thumb_faded(&song.album_id, thumb_px)),
        };
        let lead = div().w(c.lead).flex_none().child(div().relative().w(t.s(42.0)).h(t.s(THUMB)).flex().items_center().justify_center().child(lead_content).child(hover_play));

        // No rule under an album's last song or a disc's last song, since the next disc header has its own.
        let next_disc = songs.get(pos + 1).and_then(|s| self.song_peek(*s)).map(|s| s.disc.max(1));
        let last_row = variant == RowVariant::Album && (pos + 1 == songs.len() || next_disc.is_some_and(|d| d != song.disc.max(1)));
        // A grip shows in the gutter on hover for reordering. While rows are lifted, the others slide apart to open a
        // gap. Any table's rows can be dragged out to a playlist or the queue, and editable playlists can also be
        // reordered.
        let drag_list = self.page_drag_list(list);
        let (lifted, carried) = drag_list.map_or((false, false), |l| self.row_drag_marks(l, pos));
        let out = drag_list.is_some_and(|l| self.row_drag_out(l, pos));
        let dragging = self.row_dragging();
        let count = songs.len();
        let gutter_tokens = f32::from(c.gutter) / t.token_scale();
        let grip = drag_list.filter(|_| !dragging && self.page_reorders(list)).map(|_| {
            div()
                .absolute()
                .left(-t.s(26.0f32.min(gutter_tokens)))
                .top(px(0.0))
                .bottom(px(0.0))
                .w(t.s(22.0f32.min(gutter_tokens)))
                .flex()
                .items_center()
                .justify_center()
                .opacity(0.0)
                .group_hover(group.name(), |s| s.opacity(1.0))
                .child(icon(&t, "dots-six-vertical-bold", 16.0, t.t2()))
        });
        let lead_w = c.lead;
        let actions_w = c.actions;
        // Album tracks are two lines, title over artists, at the compact row height.
        let album = variant == RowVariant::Album;
        let (row_h, pill_inset) = if album { (65.0, 6.5) } else { (ROW_H, 13.0) };
        let title_cell = if album {
            let artists = self.artist_links(("album-artist", list * 100_000 + pos), &song, dim_color, cx);
            div()
                .flex()
                .flex_col()
                .min_w(px(0.0))
                .line_height(relative(1.32))
                .child(div().id("title").truncate().text_color(name_color).font_weight(FontWeight::MEDIUM).child(song.title.clone()).full_text_tip(&t, song.title.clone()))
                .child(div().flex().flex_row().items_center().min_w(px(0.0)).overflow_hidden().text_color(dim_color).when(song.explicit, |d| d.child(crate::ui::explicit_mark(&t, TEXT_MD).ml(px(0.0)).mr(t.s(TEXT_MD * 0.3)))).child(artists))
        } else {
            crate::ui::titled(&t, div().id("title").truncate().text_color(name_color).font_weight(FontWeight::MEDIUM).child(song.title.clone()).full_text_tip(&t, song.title.clone()), song.explicit, TEXT_MD)
        };
        let mut row = div()
            .id(("page-row", pos))
            .group(group.name())
            .relative()
            .h(t.s(row_h))
            .flex()
            .flex_row()
            .items_center()
            .w(c.width)
            .ml(c.gutter)
            .pl(t.s(5.0))
            .when(lifted || carried, |d| d.opacity(0.0))
            .when(out, |d| d.opacity(DRAG_OUT_DIM))
            .when_some(drag_list.filter(|_| dragging), |d, l| d.top(self.row_drag_shift(l, pos)))
            .when_some(drag_list, |d, drag_list| {
                d.on_mouse_down(MouseButton::Left, cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                    // The art's play button and the row's own buttons never lift the row.
                    if let Some(b) = this.page.state.bounds_for_item(match drag_list {
                        crate::row_drag::DragList::Page { first_block, .. } => first_block + pos,
                        crate::row_drag::DragList::Queue | crate::row_drag::DragList::Mini { .. } | crate::row_drag::DragList::Songs => return,
                    }) {
                        let layout_w = f32::from(b.size.width);
                        let x = f32::from(event.position.x - b.left());
                        let layout = this.layout_ref(window);
                        let m = PageMetrics::of(&layout);
                        let start = f32::from(this.sidebar_under(&layout)) + f32::from(this.theme.s(m.gutter + 5.0));
                        if x < start + f32::from(lead_w) || x > layout_w - f32::from(this.theme.s(m.gutter)) - f32::from(actions_w) {
                            return;
                        }
                    }
                    this.row_press(drag_list, pos, count, event, cx);
                }))
            })
            .stop_row(&t, t.s(pill_inset)).on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                if std::mem::take(&mut this.suppress_row_click) {
                    return;
                }
                let m = event.modifiers();
                let count = if event.is_keyboard() { 2 } else { event.click_count() };
                this.click_page_row(list, pos, m.secondary(cx), m.shift, count, cx);
            }))
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| this.open_page_row_menu(list, pos, event.position, cx)))
            .child(row_pill(&t, t.s(pill_inset), group.name(), fills))
            .children(self.row_flash(song_ix, t.s(pill_inset), R_ROW))
            .children(grip)
            .child(lead.opacity(offline_opacity))
            .child(div().w(c.title).flex_none().pr(t.s(NAME_CELL_PAD)).flex().min_w(px(0.0)).opacity(offline_opacity).child(title_cell));
        // Artist, album and genre are links (`song_links.rs`).
        let key = list * 100_000 + pos;
        for col in OptCol::ALL {
            if let Some(w) = c.cols[col as usize] {
                let content = self.optional_cell(col, key, &song, dim_color, cx);
                row = row.child(body_cell(&t, w, col == OptCol::Plays).opacity(offline_opacity).child(content));
            }
        }
        if let Some(w) = c.heat {
            let content = self.heat_cell(pos, song_ix, is_now, cx);
            row = row.child(body_cell(&t, w, false).items_center().opacity(offline_opacity).child(content));
            if self.page_heat.scale.lit(song_ix) == Some(crate::heat::BARS) {
                row = row.on_hover(cx.listener(move |this, on: &bool, _, cx| {
                    this.page_heat.hovered = if *on { Some(pos) } else { this.page_heat.hovered.filter(|p| *p != pos) };
                    cx.notify();
                }));
            }
        }
        if let Some(w) = c.time {
            row = row.child(body_cell(&t, w, true).text_color(dim_color).opacity(offline_opacity).child(div().truncate().child(clock(song.duration_s))));
        }
        // Recommended's suggestions end with Add in place of Download and Favorite.
        let actions = if self.recommended_list == Some(list) { self.recommended_actions(pos, song_ix, list, c.actions, cx) } else { self.song_actions(("page", pos), song_ix, Some((list, pos)), c.actions, cx) };
        row = row.child(div().flex_1()).child(actions).when(self.recommended_refreshing(list), |d| d.opacity(DISABLED_OPACITY));
        if last_row {
            row.into_any_element()
        } else {
            row.child(div().absolute().left(px(0.0)).right(px(0.0)).bottom(px(0.0)).h(px(1.0)).bg(t.line())).into_any_element()
        }
    }

    /// `rule_above` stands in for the rule a playable row leaves off when it's the last of its list.
    pub fn render_unavailable_row(&mut self, row: UnavailableRow, layout: &Layout) -> AnyElement {
        let t = self.theme;
        let c = self.row_columns(RowVariant::Album, layout);
        let dim_color: Hsla = t.t2().into();
        let lead = div().w(c.lead).flex_none().child(div().w(t.s(42.0)).h(t.s(THUMB)).flex().items_center().justify_center().text_color(dim_color).child(row.number.to_string()));
        let title_cell = div()
            .flex()
            .flex_col()
            .min_w(px(0.0))
            .line_height(relative(1.32))
            .child(div().truncate().text_color(t.t1()).font_weight(FontWeight::MEDIUM).child(row.title))
            .child(div().flex().flex_row().items_center().min_w(px(0.0)).overflow_hidden().text_color(dim_color).when(row.explicit, |d| d.child(crate::ui::explicit_mark(&t, TEXT_MD).ml(px(0.0)).mr(t.s(TEXT_MD * 0.3)))).child(div().truncate().child(row.artist)));
        let unavailable = ROW_UNAVAILABLE_OPACITY;
        let mut el = div().relative().h(t.s(65.0)).flex().flex_row().items_center().w(c.width).ml(c.gutter).pl(t.s(5.0)).child(lead.opacity(unavailable)).child(div().w(c.title).flex_none().pr(t.s(NAME_CELL_PAD)).flex().min_w(px(0.0)).opacity(unavailable).child(title_cell));
        for col in OptCol::ALL {
            if let Some(w) = c.cols[col as usize] {
                el = el.child(body_cell(&t, w, col == OptCol::Plays));
            }
        }
        if let Some(w) = c.heat {
            el = el.child(body_cell(&t, w, false));
        }
        if let Some(w) = c.time {
            el = el.child(body_cell(&t, w, true).text_color(dim_color).opacity(unavailable).child(div().truncate().child(clock(row.duration_s))));
        }
        el = el.child(div().flex_1()).child(div().w(c.actions).flex_none());
        let rule = || div().absolute().left(px(0.0)).right(px(0.0)).h(px(1.0)).bg(t.line());
        el.when(row.rule_above, |d| d.child(rule().top(px(0.0)))).when(!row.last, |d| d.child(rule().bottom(px(0.0)))).into_any_element()
    }

    /// the lifted row of a table drag, without its pill, rule, links or buttons (they'd share ids with the source row).
    pub fn lifted_row_copy(&mut self, song_ix: u32, layout: &Layout) -> AnyElement {
        let t = self.theme;
        let Some(song) = self.song_at(song_ix) else { return div().into_any_element() };
        let c = self.row_columns(RowVariant::Full, layout);
        let name_color: Hsla = if self.player.song_ix == Some(song_ix as usize) { t.accent_soft_hsla() } else { t.t1().into() };
        let dim_color: Hsla = t.t2().into();
        let thumb_px = (f32::from(t.s(THUMB)) * layout.scale).round() as u32;
        let lead = div().w(c.lead).flex_none().child(div().w(t.s(42.0)).h(t.s(THUMB)).flex().items_center().justify_center().child(row_thumb(&t, self.thumb_faded(&song.album_id, thumb_px))));
        let title = crate::ui::titled(&t, div().id("title").truncate().text_color(name_color).font_weight(FontWeight::MEDIUM).child(song.title.clone()).full_text_tip(&t, song.title.clone()), song.explicit, TEXT_MD);
        let mut row = div()
            .relative()
            .h(t.s(ROW_H))
            .w(c.width)
            .pl(t.s(5.0))
            .flex()
            .flex_row()
            .items_center()
            .child(lead)
            .child(div().w(c.title).flex_none().pr(t.s(NAME_CELL_PAD)).flex().min_w(px(0.0)).child(title));
        for col in OptCol::ALL {
            if let Some(w) = c.cols[col as usize] {
                let text: gpui::SharedString = match col {
                    OptCol::Artist => song.artist.clone(),
                    OptCol::Album => song.album.clone(),
                    OptCol::Genre => song.genre.clone(),
                    OptCol::Added => crate::i18n::fmt_date(song.added).into(),
                    OptCol::Plays => if song.plays > 0 { crate::i18n::fmt_int(i64::from(song.plays)).into() } else { "".into() },
                };
                row = row.child(body_cell(&t, w, col == OptCol::Plays).child(div().truncate().text_color(dim_color).child(text)));
            }
        }
        if let Some(w) = c.time {
            row = row.child(body_cell(&t, w, true).text_color(dim_color).child(div().truncate().child(clock(song.duration_s))));
        }
        let (downloaded, starred) = (self.is_downloaded(song_ix), self.is_starred(song_ix));
        let action = |name: &str| div().w(t.s(ACTION_W)).flex_none().flex().items_center().justify_center().child(icon(&t, name, 18.0, t.t1()));
        let actions = div()
            .w(c.actions)
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .justify_end()
            .pr(t.s(9.0))
            .child(if downloaded { div().w(t.s(ACTION_W)).flex_none() } else { action("arrow-down-regular") })
            .child(action(if starred { "heart-fill" } else { "heart-regular" }))
            .child(action("dots-three-bold"));
        row.child(div().flex_1()).child(actions).into_any_element()
    }

    /// A row's Download, Favorite and More. `place` is the row's list and index, for actions like Remove from This
    /// Playlist.
    pub fn song_actions(&mut self, id: (&'static str, usize), song_ix: u32, place: Option<(usize, usize)>, width: Pixels, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let (color, hover): (Hsla, Hsla) = (t.t1().into(), t.hover_ink());
        let downloaded = self.is_downloaded(song_ix);
        let starred = self.is_starred(song_ix);
        let key = id.1 * 4 + id.0.len() % 4;
        let row = div().w(width).flex_none().flex().flex_row().items_center().justify_end().pr(t.s(9.0)).child(
            if downloaded {
                div().w(t.s(ACTION_W)).flex_none().into_any_element()
            } else if self.page_collection_pending() {
                // The page's playlist or album is kept downloaded and still needs this song, so it's queued or
                // downloading.
                let glyph = if self.download_progress.contains_key(&song_ix) { crate::ui::DownloadGlyph::Fetching } else { crate::ui::DownloadGlyph::Queued };
                div().w(t.s(ACTION_W)).flex_none().flex().justify_center().child(crate::ui::download_glyph(&t, glyph, 0.0, DOWNLOAD_GLYPH, t.t2().into())).into_any_element()
            } else if let Some(fraction) = self.download_progress.get(&song_ix).copied() {
                self.download_progress_badge(("row-dl", key), song_ix, fraction, cx)
            } else {
                icon_button_colored(&t, ("row-dl", key), tr("Download"), "arrow-down-regular", false, color, hover)
                    .w(t.s(ACTION_W))
                    .stop(&t).on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.set_downloaded(&[song_ix], true, cx);
                        cx.notify();
                    }))
                    .into_any_element()
            },
        );
        row.child(
            icon_button_colored(&t, ("row-star", key), tr("Favorite"), if starred { "heart-fill" } else { "heart-regular" }, false, t.t1().into(), hover).w(t.s(ACTION_W)).stop(&t).on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                if !starred {
                    crate::ui::pop(("row-star", key));
                }
                this.toggle_song_star(song_ix, cx);
            })),
        )
        .child(
            icon_button_colored(&t, ("row-more", key), tr("More"), "dots-three-bold", false, color, hover).w(t.s(ACTION_W)).stop(&t).on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                cx.stop_propagation();
                match place {
                    Some((list, pos)) => this.open_list_song_menu(list, vec![pos], event.position(), true, cx),
                    None => this.open_song_menu(vec![song_ix], Some(Self::play_songs_row(&[song_ix])), event.position(), true, cx),
                }
            })),
        )
        .into_any_element()
    }

    /// One or two compact rows side by side, depending on the page's width.
    pub fn render_mini_pair(&mut self, list: usize, pos: usize, columns: bool, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let m = PageMetrics::of(layout);
        let width = f32::from(layout.main_width) / t.token_scale() - m.gutter * 2.0;
        if !columns {
            return div().pl(t.s(m.gutter)).child(self.render_mini_row(list, pos, width, cx)).into_any_element();
        }
        let cell = (width - 38.0) / 2.0;
        let mut row = div().flex().flex_row().gap(t.s(38.0)).pl(t.s(m.gutter)).child(self.render_mini_row(list, pos, cell, cx));
        if self.page.lists.get(list).is_some_and(|s| pos + 1 < s.len()) {
            row = row.child(self.render_mini_row(list, pos + 1, cell, cx));
        }
        row.into_any_element()
    }

    pub fn render_mini_row_at(&mut self, list: usize, pos: usize, width: f32, cx: &mut Context<Self>) -> AnyElement {
        self.render_mini_row(list, pos, width, cx)
    }

    fn render_mini_row(&mut self, list: usize, pos: usize, width: f32, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let Some((song_ix, _)) = self.list_song(list, pos) else { return div().into_any_element() };
        let Some(song) = self.song_at(song_ix) else { return div().into_any_element() };
        let is_now = self.player.song_ix == Some(song_ix as usize);
        let offline_opacity = self.offline_row_opacity(song_ix);
        let selected = self.selection_list == ListId::Page(list) && self.selection.contains(song_ix);
        let mini_artist = self.page_mini_artist;
        // Compact rows can only be dragged out, to a playlist or the queue.
        let drag_list = crate::row_drag::DragList::Mini { list };
        let out = self.row_drag_out(drag_list, pos);
        let dragging = self.row_dragging();
        let name: Hsla = if is_now {
            t.accent_soft_hsla()
        } else {
            t.t1().into()
        };
        let dim: Hsla = t.t2().into();
        let thumb_px = (f32::from(t.s(THUMB)) * self.window_scale).round() as u32;
        let group = SharedGroup::new("mini-row", pos);
        let art = row_thumb(&t, self.thumb_faded(&song.album_id, thumb_px));
        let id_key = list * 100_000 + pos;
        // The line under the title, "Artist, Artist • Album", with each name a link. Artists only show where the page
        // asks for them.
        let artists = self.artist_links(("mini-artist", id_key), &song, dim, cx);
        let sub = if mini_artist || song.album.is_empty() {
            artists
        } else {
            let album = self.album_link(("mini-album", id_key), &song, dim, cx);
            div()
                .flex()
                .flex_row()
                .min_w(px(0.0))
                .overflow_hidden()
                .whitespace_nowrap()
                .text_color(dim)
                // The album shortens first, then the artists.
                .child(div().flex_shrink(0.01).min_w(px(0.0)).max_w(relative(0.65)).overflow_hidden().child(artists))
                .child(div().flex_none().px(t.s(4.0)).child("•"))
                .child(div().flex_shrink(1.0).min_w(px(0.0)).overflow_hidden().child(album))
                .into_any_element()
        };
        div()
            .id(("mini-row", id_key))
            .group(group.name())
            .flex()
            .flex_row()
            .items_center()
            .gap(t.s(12.0))
            .w(t.s(width))
            .h(t.s(65.0))
            .pl(t.s(5.0))
            .pr(t.s(12.0))
            .rounded(t.s(R_MD))
            .relative()
            .children(self.row_flash(song_ix, px(0.0), R_MD))
            .when(selected, |d| d.bg(t.row_selected()))
            .when(!selected && !dragging, |d| d.hover(|s| s.bg(t.row())))
            .when(out, |d| d.opacity(DRAG_OUT_DIM))
            .stop_row(&t, gpui::px(0.0)).on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                if std::mem::take(&mut this.suppress_row_click) {
                    return;
                }
                let m = event.modifiers();
                let count = if event.is_keyboard() { 2 } else { event.click_count() };
                this.click_page_row(list, pos, m.secondary(cx), m.shift, count, cx);
            }))
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| this.open_page_row_menu(list, pos, event.position, cx)))
            .child(
                div().relative().size(t.s(THUMB)).flex_none().opacity(offline_opacity).child(art).child(row_hover_play(&t, ("mini-play", id_key), group.name()).on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.play_page_list(list, pos, false, cx);
                }))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .line_height(relative(1.32))
                    .opacity(offline_opacity)
                    // Pressing the text lifts the row. The art's play button and the actions are buttons.
                    .on_mouse_down(MouseButton::Left, cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                        let count = this.page.lists.get(list).map_or(0, |l| l.len());
                        this.row_press(drag_list, pos, count, event, cx);
                    }))
                    .child(crate::ui::titled(&t, div().id("title").truncate().font_weight(FontWeight::MEDIUM).text_color(name).child(song.title.clone()).full_text_tip(&t, song.title.clone()), song.explicit, TEXT_MD))
                    .child(sub),
            )
            .child(div().mr(t.s(16.0)).flex_none().text_color(dim).child(clock(song.duration_s)))
            .child(self.song_actions(("mini", id_key), song_ix, None, t.s(132.0), cx))
            .into_any_element()
    }

    fn click_page_row(&mut self, list: usize, pos: usize, ctrl: bool, shift: bool, count: usize, cx: &mut Context<Self>) {
        let Some(songs) = self.page.lists.get(list).cloned() else { return };
        self.select_in(ListId::Page(list));
        self.selection.click(pos, &songs, ctrl, shift);
        if count >= 2 && !ctrl && !shift {
            self.play_page_list(list, pos, false, cx);
        }
        cx.notify();
    }

    /// Replaces the queue with a page's list and plays from one position. On Search a song starts a radio instead.
    pub fn play_page_list(&mut self, list: usize, pos: usize, shuffle: bool, cx: &mut Context<Self>) {
        let Some(songs) = self.page.lists.get(list).cloned() else { return };
        if self.route == crate::pages::Route::Search
            && let Some(&song_ix) = songs.get(pos)
        {
            self.play_as_radio(song_ix, cx);
            return;
        }
        if let Some(source) = self.page.list_sources.get(list).cloned().flatten() {
            self.play_source = Some(source);
        }
        self.play_songs(songs.as_ref().clone(), pos, shuffle, cx);
    }

    pub fn play_songs(&mut self, songs: Vec<u32>, pos: usize, shuffle: bool, cx: &mut Context<Self>) {
        if songs.is_empty() {
            return;
        }
        if self.core.is_some() {
            self.lyrics_user_scrolled = false;
            self.core_play_list(songs, pos, shuffle, cx);
            return;
        }
        let mut queue = songs;
        let mut start = pos.min(queue.len() - 1);
        if shuffle {
            // Keep the chosen song first and shuffle the rest.
            let first = queue.remove(start);
            let mut rng = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1u32, |d| d.subsec_nanos() | 1);
            for i in (1..queue.len()).rev() {
                rng ^= rng << 13;
                rng ^= rng >> 17;
                rng ^= rng << 5;
                queue.swap(i, rng as usize % (i + 1));
            }
            queue.insert(0, first);
            start = 0;
        }
        self.player.song_ix = queue.get(start).map(|s| *s as usize);
        self.player.queue = queue;
        self.player.queue_pos = start;
        self.player.position = 0.0;
        self.player.playing = true;
        self.lyrics_user_scrolled = false;
        self.on_song_changed(cx);
    }

    fn open_page_row_menu(&mut self, list: usize, pos: usize, position: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        let Some(songs) = self.page.lists.get(list).cloned() else { return };
        self.select_in(ListId::Page(list));
        self.selection.prepare_context(pos, &songs);
        // Positions rather than songs, since Remove from This Playlist removes by position.
        let places: Vec<usize> = songs.iter().enumerate().filter(|(_, s)| self.selection.contains(**s)).map(|(i, _)| i).collect();
        self.open_list_song_menu(list, places, position, false, cx);
    }

    /// Lays out a page's compact rows in one or two columns by width. `shelf` names what the rows play from when the
    /// page name doesn't.
    pub fn mini_blocks(&mut self, songs: Vec<u32>, layout: &Layout, force_one: bool, shelf: Option<&'static str>) -> Vec<crate::pages::Block> {
        let t = self.theme;
        let main = f32::from(layout.main_width) / t.token_scale();
        let columns = !force_one && main > 1000.0;
        let list = self.add_list(songs);
        if let Some(name) = shelf {
            self.page.list_sources[list] = Some(navia_core::queue_order::QueueSource { name: name.to_string(), radio: false, link: None });
        }
        let count = self.page.lists[list].len();
        let step = if columns { 2 } else { 1 };
        (0..count).step_by(step).map(|pos| crate::pages::Block::Mini { list, pos, columns }).collect()
    }

    pub fn window_layout(&self, window: &Window) -> Layout {
        self.layout_ref(window)
    }
}

impl Shell {
    /// lights the rows showing these songs, fading back over `ROW_FLASH`; under reduced motion they stay lit, then drop.
    pub fn flash_rows(&mut self, songs: &[u32]) {
        self.row_flash = Some((std::rc::Rc::new(songs.iter().copied().collect()), std::time::Instant::now()));
    }

    /// the flash over a song's row, if it's lit: a child for the row, which must be `relative`.
    pub fn row_flash(&self, song_ix: u32, inset: Pixels, radius: f32) -> Option<AnyElement> {
        let (songs, since) = self.row_flash.as_ref()?;
        if !songs.contains(&song_ix) {
            return None;
        }
        let secs = crate::theme::motion::ROW_FLASH * crate::testing::slow_motion();
        let elapsed = since.elapsed().as_secs_f32();
        if elapsed >= secs {
            return None;
        }
        let left = if crate::theme::motion::reduced() { 1.0 } else { 1.0 - crate::theme::motion::cubic_bezier(crate::theme::motion::EASE_OUT, elapsed / secs) };
        let t = self.theme;
        let fill = t.row_flash_fill();
        let end = *since + std::time::Duration::from_secs_f32(secs);
        let ticker = gpui::canvas(|_, _, _| {}, move |_, _, window, _| {
            if std::time::Instant::now() < end {
                crate::animation::request_frame(window);
            }
        })
        .absolute()
        .size_0();
        Some(div().absolute().left(px(0.0)).right(px(0.0)).top(inset).bottom(inset).rounded(t.s(radius)).bg(fill.alpha(fill.a * left)).child(ticker).into_any_element())
    }
}

/// A song row's fills: at rest (the selection, or a downloaded row's `--row`) and under the pointer.
pub fn row_fills(t: &crate::theme::Theme, selected: bool, downloaded: bool) -> (Option<Hsla>, Hsla) {
    if selected {
        (Some(t.row_selected()), t.row_selected())
    } else if downloaded {
        (Some(t.row().into()), t.hover().into())
    } else {
        (None, t.row().into())
    }
}

/// A song row's artwork, over a placeholder until it loads.
pub fn row_thumb(t: &crate::theme::Theme, thumb: Option<(std::sync::Arc<gpui::RenderImage>, f32)>) -> AnyElement {
    let placeholder = || div().size(t.s(THUMB)).rounded(t.s(R_SM)).bg(t.panel_2());
    match thumb {
        Some((image, fade)) => crate::ui::fade_in(crate::ui::image(image).size(t.s(THUMB)).rounded(t.s(R_SM)), placeholder(), fade),
        None => placeholder().into_any_element(),
    }
}

/// A song row's highlight, an inset pill rather than a full-row fill.
pub fn row_pill(t: &crate::theme::Theme, inset: Pixels, group: gpui::SharedString, (rest, hover): (Option<Hsla>, Hsla)) -> gpui::Div {
    div().absolute().left(px(0.0)).right(px(0.0)).top(inset).bottom(inset).rounded(t.s(R_ROW)).when_some(rest, |d, bg| d.bg(bg)).group_hover(group, move |s| s.bg(hover))
}

/// The play glyph over a row's artwork or number, shown on hover. It plays on one click, where a click elsewhere only
/// selects.
pub fn row_hover_play(t: &crate::theme::Theme, id: impl Into<gpui::ElementId>, group: gpui::SharedString) -> gpui::Stateful<gpui::Div> {
    div().id(id).tooltip(crate::ui::tip(t, tr("Play"))).absolute().inset_0().flex().items_center().justify_center().opacity(0.0).group_hover(group, |s| s.opacity(1.0)).child(icon(t, "play-fill", 16.0, t.t_on_art()))
}

/// A song table's column head, emphasized with ▲ or ▼ when it's the sorted column.
pub fn sort_head(t: &crate::theme::Theme, id: impl Into<gpui::ElementId>, text: &'static str, sorted: Option<bool>) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap(t.s(5.0))
        .min_w(px(0.0))
        .when(sorted.is_some(), |d| d.text_color(t.t_em()).font_weight(FontWeight::MEDIUM))
        .child(div().truncate().child(text))
        .when_some(sorted, |d, ascending| d.child(icon(t, if ascending { "caret-up-fill" } else { "caret-down-fill" }, 11.0, t.t_em())))
}

/// A song table's header row: column heads in `--t2` over a 1px line. A right-click picks the columns.
/// Time and Plays are right-aligned, headers and values alike.
pub fn is_number(column: SortColumn) -> bool {
    matches!(column, SortColumn::Time | SortColumn::Plays)
}

/// one row cell's box, padded and aligned as its header (`head_cell`).
pub fn body_cell(t: &crate::theme::Theme, w: Pixels, number: bool) -> gpui::Div {
    div().w(w).flex_none().pr(t.s(if number { NUM_CELL_PAD } else { CELL_PAD })).flex().min_w(px(0.0)).overflow_hidden().when(number, |d| d.justify_end())
}

/// one header label's box: number columns end at `NUM_CELL_PAD`, the others at `CELL_PAD`.
pub fn head_cell(t: &crate::theme::Theme, w: Pixels, number: bool) -> gpui::Div {
    div().w(w).flex_none().pr(t.s(if number { NUM_CELL_PAD } else { CELL_PAD })).flex().flex_row().items_center().when(number, |d| d.justify_end()).min_w(px(0.0))
}

pub fn table_head_row(t: &crate::theme::Theme, height: Pixels, width: Pixels, gutter: Pixels, pad_left: Pixels) -> gpui::Div {
    div().relative().h(height).flex().flex_row().items_center().w(width).ml(gutter).pl(pad_left).text_color(t.t2())
}

struct SharedGroup(gpui::SharedString);

impl SharedGroup {
    fn new(kind: &str, pos: usize) -> Self {
        Self(gpui::SharedString::from(format!("{kind}-{pos}")))
    }
    fn name(&self) -> gpui::SharedString {
        self.0.clone()
    }
}
