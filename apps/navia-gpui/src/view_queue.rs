//! The queue, virtualized. Used by the dock and the expanded player.

use crate::ui::FullTextTip;
use crate::i18n::tr;
use gpui::prelude::*;
use crate::focus::FocusStop;
use gpui::{AnyElement, Context, FontWeight, Hsla, MouseButton, Pixels, Window, div, list, px, relative};

use crate::shell::{Layout, Shell};
use crate::theme::size::*;
use crate::row_drag::DragList;
use crate::ui::{clock, icon};
use crate::i18n::trf;
use crate::pages::Route;
use crate::song_links::LinkTarget;
use gpui::SharedString;
use navia_core::queue_order::{QueueSource, SourceLink};
use crate::view_dock::ModeButton;
use crate::ui_controls::{glass_action, primary_action, small_action};

pub(crate) const ROW_HEIGHT: f32 = 58.0;
pub(crate) const SIDE_PAD: f32 = 18.0;

impl Shell {
    /// The queue's song order and the index of the first upcoming song.
    pub fn upcoming_queue(&self) -> (std::sync::Arc<Vec<u32>>, usize) {
        if self.player.queue.is_empty() {
            let pos = self.player.song_ix.and_then(|s| self.order.iter().position(|i| *i as usize == s)).map_or(0, |p| p + 1);
            (std::sync::Arc::new(self.order.clone()), pos)
        } else {
            (std::sync::Arc::new(self.player.queue.clone()), self.player.queue_pos + 1)
        }
    }

    /// The queue view's list: Up Next, then the Autoplay section, and how many are Up Next. Selection, dragging and row
    /// positions all index this list.
    pub fn queue_view_list(&self) -> (std::sync::Arc<Vec<u32>>, usize) {
        let (queue, first) = self.upcoming_queue();
        if self.player.queue.is_empty() {
            let rest = queue.get(first..).map(<[u32]>::to_vec).unwrap_or_default();
            let n = rest.len();
            return (std::sync::Arc::new(rest), n);
        }
        let len = queue.len();
        let own_end = (len - self.player.auto_tail.min(len)).max(first.min(len));
        let mut list: Vec<u32> = queue[first.min(len)..own_end].to_vec();
        let up_next = list.len();
        if self.player.station.is_none() {
            list.extend(self.autoplay_section());
        }
        (std::sync::Arc::new(list), up_next)
    }

    /// Whether the queue shows an Autoplay heading: with songs, while finding them, or for the note while Repeat is on.
    fn queue_autoplay_heading(&self, auto_rows: usize) -> bool {
        match self.autoplay_state() {
            navia_core::autoplay::AutoplayState::On => auto_rows > 0,
            navia_core::autoplay::AutoplayState::Finding => self.queue_current().is_some(),
            navia_core::autoplay::AutoplayState::Repeat => self.queue_current().is_some(),
            _ => false,
        }
    }

    /// Where each part of the queue view sits in its list: Now Playing, Next in Queue, Next from, then Autoplay. The
    /// empty message shows only when nothing follows at all.
    pub fn queue_sections(&self) -> QueueSections {
        let (list, up_next) = self.queue_view_list();
        let current = self.queue_current().is_some();
        let run = if self.player.queue.is_empty() { 0 } else { self.player.queued_run.min(up_next) };
        let auto_rows = list.len() - up_next;
        let auto_heading = self.queue_autoplay_heading(auto_rows);
        let state = self.autoplay_state();
        let finding = if auto_heading && state == navia_core::autoplay::AutoplayState::Finding { QUEUE_FINDING_ROWS } else { 0 };
        let follows = auto_heading && state != navia_core::autoplay::AutoplayState::Repeat;
        let mut i = if current { 2 } else { 0 };
        // in a listening session Next in Queue is the session's, shown even while empty
        let queued_heading = (run > 0 || self.session_on()).then_some(i);
        if queued_heading.is_some() {
            i += 1 + run;
        }
        let rest = up_next - run;
        let from_heading = (rest > 0).then_some(i);
        if rest > 0 {
            i += 1 + rest;
        }
        let empty = (up_next == 0 && !follows).then_some(i);
        if empty.is_some() {
            i += 1;
        }
        let auto_heading_ix = auto_heading.then_some(i);
        if auto_heading {
            i += 1;
        }
        QueueSections { current, run, rest, auto_rows, finding, queued_heading, from_heading, empty, auto_heading: auto_heading_ix, auto_first: i, count: i + auto_rows + finding + 1 }
    }

    /// The list item a row of [`Shell::queue_view_list`] is drawn as.
    pub fn queue_item_ix(&self, pos: usize) -> usize {
        self.queue_sections().item(pos)
    }

    /// except during a station. while following in a listening session, their song.
    fn queue_current(&self) -> Option<u32> {
        if self.player.station.is_some() {
            return None;
        }
        if let Some(f) = self.listen.following.as_ref() {
            return f.rowid.filter(|_| self.session_on());
        }
        let (queue, first) = self.upcoming_queue();
        first.checked_sub(1).and_then(|i| queue.get(i).copied()).filter(|s| self.player.song_ix == Some(*s as usize))
    }

    /// What a list started from the current page plays from. Other pages have none, and the queue says "Up Next".
    pub fn route_source(&self) -> Option<QueueSource> {
        let named = |name: SharedString, link: SourceLink| Some(QueueSource { name: name.to_string(), radio: false, link: Some(link) });
        match &self.route {
            Route::Album(id) => self.albums.iter().find(|a| &a.id == id).and_then(|a| named(a.name.clone(), SourceLink::Album(id.to_string()))),
            Route::Playlist(id) => self.playlists.iter().find(|p| &p.id == id).and_then(|p| named(p.name.clone(), SourceLink::Playlist(id.to_string()))),
            Route::Artist(id) => self.artists.iter().find(|a| &a.id == id).and_then(|a| named(a.name.clone(), SourceLink::Artist(id.to_string()))),
            Route::ArtistTop(id) => self.artists.iter().find(|a| &a.id == id).and_then(|a| named(trf("{artist} Top Songs", &[("artist", &a.name)]).into(), SourceLink::ArtistTop(id.to_string()))),
            Route::Mix(id) => self.for_you.mixes.iter().find(|m| &m.id == id).and_then(|m| named(crate::view_for_you::mix_title(m), SourceLink::Mix(id.to_string()))),
            Route::Liked => named(tr("Liked Songs").into(), SourceLink::Liked),
            Route::Songs => match self.song_filters.only_genre() {
                Some(genre) => named(genre.clone().into(), SourceLink::Genre(genre)),
                None => named(tr("Songs").into(), SourceLink::Songs),
            },
            _ => None,
        }
    }

    pub fn render_queue_panel(&mut self, top_inset: Pixels, scale: f32, cx: &mut Context<Self>) -> AnyElement {
        // following: their song, and the listener's queue waiting; in a session the queue shows, to edit
        if !self.session_on()
            && let Some(following) = self.render_queue_following(scale, cx)
        {
            return div().absolute().left(px(0.0)).right(px(0.0)).top(top_inset).bottom(px(0.0)).child(following).into_any_element();
        }
        self.autoplay_view_shown(cx);
        let sections = self.queue_sections();
        self.queue_rows_first = sections.item(0).min(sections.empty.unwrap_or(usize::MAX)).min(sections.auto_heading.unwrap_or(usize::MAX));
        let session_rows = if sections.queued_heading.is_some() && sections.run == 0 { 1 + self.session_theirs().len() } else { 0 };
        let key = (sections.run, sections.rest, (sections.auto_rows * (QUEUE_FINDING_ROWS + 1) + sections.finding) * 2 + usize::from(sections.auto_heading.is_some()), sections.current, self.autoplay_asks_deezer(), session_rows);
        if self.queue_key != key {
            self.queue_key = key;
            self.queue_list.reset(sections.count);
        }
        // Songs dragged in from another list drop into the queue at a gap.
        let mark = self.drop_mark("queue", self.queue_drop_target(), R_MD, 0.0);
        div()
            .absolute()
            .left(px(0.0))
            .right(px(0.0))
            .top(top_inset)
            .bottom(px(0.0))
            .children(mark)
            .child(list(self.queue_list.clone(), cx.processor(|this, ix: usize, window, cx| this.render_queue_item(ix, window, cx))).size_full())
            .child(self.render_scrollbar("queue", crate::ui_scroll::ScrollSource::List(self.queue_list.clone()), px(0.0), px(0.0), cx))
            .into_any_element()
    }

    /// A section heading with an optional control on its right. Headings after Now Playing get the section gap above.
    fn queue_heading(&mut self, title: AnyElement, gap: bool, right: Option<AnyElement>) -> AnyElement {
        let t = self.theme;
        div()
            .w_full()
            .pt(t.s(if gap { QUEUE_SECTION_GAP + 6.0 } else { 14.0 }))
            .pb(t.s(10.0))
            .pl(t.s(SIDE_PAD + 8.0))
            .pr(t.s(SIDE_PAD + 6.0))
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(t.s(8.0))
            .text_size(t.text(TEXT_LG))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(t.t_on_art())
            .child(div().flex_1().min_w(px(0.0)).flex().flex_row().items_center().overflow_hidden().child(title))
            .when_some(right, |d, r| d.child(div().flex_none().child(r)))
            .into_any_element()
    }

    /// A queue heading's Clear and Shuffle buttons at its right edge.
    fn queue_head_buttons(&mut self, clear: ModeButton, shuffle: ModeButton, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let ink = t.t_on_art();
        let feel = crate::ui::Feel::PLAYER;
        let (clear_id, shuffle_id) = if clear == ModeButton::ClearQueue { ("queue-clear", "queue-shuffle-queued") } else { ("queue-clear-list", "queue-shuffle") };
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(t.s(QUEUE_HEAD_GAP))
            .child(self.render_mode_button(clear, clear_id, QUEUE_HEAD_ICON, ink, feel, cx))
            .child(self.render_mode_button(shuffle, shuffle_id, QUEUE_HEAD_ICON, ink, feel, cx))
            .into_any_element()
    }

    /// "Next from: <source>" with the source linked, or "Up Next" when unknown.
    fn next_from_title(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        if self.listen.following.is_some() {
            let own = self.player.queue.get(self.player.queue_pos).copied().map(|r| self.song_by_rowid(r).title).filter(|t| !t.is_empty());
            let title = match own {
                Some(song) => trf("Your queue waits at “{song}”", &[("song", &song)]),
                None => tr("Your queue waits").to_string(),
            };
            return div().truncate().child(title).into_any_element();
        }
        let Some(source) = self.player.source.clone() else { return div().truncate().child(tr("Up Next")).into_any_element() };
        let name: SharedString = if source.radio { trf("{name} Radio", &[("name", &source.name)]).into() } else { source.name.clone().into() };
        let link = self.link(("queue-source", 0), name, source.link.as_ref().map(LinkTarget::from), t.t_on_art().into(), cx);
        div().flex().flex_row().items_center().min_w(px(0.0)).gap(t.s(5.0)).child(div().flex_none().child(tr("Next from:"))).child(link).into_any_element()
    }

    fn render_queue_item(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let (list, upcoming) = self.queue_view_list();
        let s = self.queue_sections();
        let current = self.queue_current();
        let station = self.player.station.is_some();
        if let Some(song) = current {
            if ix == 0 {
                return self.queue_heading(div().child(tr("Now Playing")).into_any_element(), false, None);
            }
            if ix == 1 {
                return self.queue_row(None, song, window, cx);
            }
        }
        if s.queued_heading == Some(ix) {
            let buttons = self.queue_head_buttons(ModeButton::ClearQueue, ModeButton::ShuffleQueue, cx);
            if !self.session_on() {
                return self.queue_heading(div().truncate().child(tr("Next in Queue")).into_any_element(), s.current, Some(buttons));
            }
            let heading = self.queue_heading(div().truncate().child(tr("Session Queue")).into_any_element(), s.current, Some(buttons));
            // the other side's songs can't be listed here, only that they have some
            let mut notes: Vec<SharedString> = self.session_theirs().iter().map(|user| trf("{user} has songs queued", &[("user", user)]).into()).collect();
            if s.run == 0 && notes.is_empty() {
                notes.push(tr("Songs you add to the queue play in turns with the other side’s.").into());
            }
            let note_ink = Hsla::from(t.t_on_art()).alpha(0.7);
            return div()
                .flex()
                .flex_col()
                .child(heading)
                .children(notes.into_iter().map(|n| div().px(t.s(SIDE_PAD + 8.0)).pb(t.s(10.0)).text_size(t.text(TEXT_SM)).text_color(note_ink).child(n)))
                .into_any_element();
        }
        if s.from_heading == Some(ix) {
            let shift = self.row_drag_split_shift(DragList::Queue);
            let title = self.next_from_title(cx);
            let buttons = (!station).then(|| self.queue_head_buttons(ModeButton::ClearList, ModeButton::Shuffle, cx));
            let heading = self.queue_heading(title, s.current || s.queued_heading.is_some(), buttons);
            return div().child(div().relative().top(shift).child(heading)).into_any_element();
        }
        if ix == s.count - 1 {
            return div().h(t.s(24.0)).into_any_element();
        }
        if s.auto_heading == Some(ix) {
            let shift = self.row_drag_follow_shift(DragList::Queue);
            let card = self.autoplay_deezer_card(cx);
            return div().child(div().relative().top(shift).child(self.autoplay_heading()).children(card)).into_any_element();
        }
        if ix >= s.auto_first + s.auto_rows {
            return self.queue_placeholder_row();
        }
        if ix >= s.auto_first {
            let pos = upcoming + (ix - s.auto_first);
            return self.queue_row(Some(pos), list[pos], window, cx);
        }
        if s.empty == Some(ix) {
            let off = !self.prefs.autoplay && !station;
            let shift = self.row_drag_follow_shift(DragList::Queue);
            return div()
                .relative()
                .top(shift)
                .px(t.s(SIDE_PAD + 8.0))
                .py(t.s(24.0))
                .flex()
                .flex_col()
                .items_center()
                .gap(t.s(8.0))
                .text_center()
                .text_color(t.t2())
                .child(icon(&t, "queue-regular", 26.0, t.t3()))
                .child(div().text_size(t.text(TEXT_LG)).font_weight(FontWeight::SEMIBOLD).text_color(t.t1()).child(tr("Nothing queued")))
                .child(tr("Add songs from any menu, or start an album or playlist."))
                .when(off, |d| d.child(div().text_size(t.text(TEXT_SM)).text_color(t.t3()).child(tr("Autoplay is off."))))
                .into_any_element();
        }
        let pos = match s.queued_heading {
            Some(h) if ix <= h + s.run => ix - h - 1,
            _ => s.run + ix - s.from_heading.map_or(ix, |h| h + 1),
        };
        self.queue_row(Some(pos), list[pos], window, cx)
    }

    /// Autoplay's heading: what its songs are based on, that they're being found, or the note while Repeat is on.
    fn autoplay_heading(&mut self) -> AnyElement {
        use navia_core::autoplay::AutoplayState;
        let t = self.theme;
        let state = self.autoplay_state();
        let repeat = state == AutoplayState::Repeat;
        let note: Option<SharedString> = match state {
            AutoplayState::Repeat => Some(tr("Off while Repeat is on, because the list never ends.").into()),
            AutoplayState::Finding => Some(tr("Finding similar songs").into()),
            _ => self.autoplay_based_on().map(|name| trf("Based on {name}", &[("name", &name)]).into()),
        };
        div()
            .w_full()
            .pt(t.s(18.0))
            .pb(t.s(10.0))
            .px(t.s(SIDE_PAD + 8.0))
            .text_color(t.t_on_art())
            .child(div().text_size(t.text(TEXT_LG)).font_weight(FontWeight::SEMIBOLD).when(repeat, |d| d.opacity(0.6)).child(tr("Autoplay")))
            .when_some(note, |d, note| d.child(div().mt(t.s(2.0)).text_size(t.text(TEXT_SM)).opacity(0.6).truncate().child(note)))
            .into_any_element()
    }

    /// Whether Autoplay opens with the Deezer question, for an account that never answered it and only while Autoplay
    /// runs.
    fn autoplay_asks_deezer(&self) -> bool {
        self.core.is_some() && self.deezer.choice.is_none() && self.autoplay_state() != navia_core::autoplay::AutoplayState::Repeat
    }

    /// The Deezer question at the top of Autoplay. Either answer saves the choice, so it never returns.
    fn autoplay_deezer_card(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.autoplay_asks_deezer() {
            return None;
        }
        let t = self.theme;
        let not_now = small_action(&t, glass_action(&t, "queue-deezer-not-now")).flex_1().child(tr("Not Now")).stop(&t).on_click(cx.listener(|this, _, _, cx| this.set_deezer_choice(false, cx)));
        let turn_on = small_action(&t, primary_action(&t, "queue-deezer-turn-on", true)).flex_1().child(tr("Turn On")).stop(&t).on_click(cx.listener(|this, _, _, cx| this.set_deezer_choice(true, cx)));
        let card = crate::ui::prompt_card(
            &t,
            tr("Better Autoplay with Deezer?").into(),
            "paper-plane-tilt-regular",
            tr("Sends artist and song names to Deezer and ListenBrainz from this computer.").into(),
            vec![not_now.into_any_element(), turn_on.into_any_element()],
        );
        Some(div().px(t.s(SIDE_PAD)).pb(t.s(PROMPT_CARD_GAP)).child(card).into_any_element())
    }

    /// A placeholder row while Autoplay finds songs, at a song row's size so nothing jumps.
    fn queue_placeholder_row(&self) -> AnyElement {
        let t = self.theme;
        let block = t.on_art_placeholder();
        div()
            .h(t.s(ROW_HEIGHT))
            .px(t.s(SIDE_PAD))
            .child(
                div()
                    .h_full()
                    .px(t.s(8.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(t.s(11.0))
                    .child(div().flex_none().size(t.s(42.0)).rounded(t.s(R_XS)).bg(block))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap(t.s(ROW_PLACEHOLDER_GAP))
                            .child(div().h(t.s(ROW_PLACEHOLDER_LINE)).w(relative(ROW_PLACEHOLDER_TITLE_W)).rounded(t.s(R_XS)).bg(block))
                            .child(div().h(t.s(ROW_PLACEHOLDER_LINE)).w(relative(ROW_PLACEHOLDER_ARTIST_W)).rounded(t.s(R_XS)).bg(block)),
                    ),
            )
            .into_any_element()
    }

    /// One queue row. `pos` is its place in Up Next, or None for the Now Playing row, which can't be selected, played
    /// or moved.
    fn queue_row(&mut self, pos: Option<usize>, song_ix: u32, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let song = self.song_by_rowid(song_ix);
        let thumb_px = (f32::from(t.s(42.0)) * window.scale_factor()).round() as u32;
        let thumb = self.thumb_faded(&song.album_id, thumb_px);
        let thumb_el: AnyElement = match thumb {
            Some((image, fade)) => crate::ui::fade_in(crate::ui::image(image).size(t.s(42.0)).rounded(t.s(R_XS)), div().size(t.s(42.0)).rounded(t.s(R_XS)).bg(t.on_art_placeholder()), fade),
            None => div().size(t.s(42.0)).rounded(t.s(R_XS)).bg(t.on_art_placeholder()).into_any_element(),
        };
        let Some(pos) = pos else {
            return div()
                .px(t.s(SIDE_PAD))
                .child(
                    div()
                        .id("queue-now")
                        // the song's menu, without anything that removes or moves it.
                        .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| this.open_song_menu(vec![song_ix], None, event.position, false, cx)))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(t.s(11.0))
                        .h(t.s(ROW_HEIGHT))
                        .px(t.s(8.0))
                        .child(div().size(t.s(42.0)).flex_none().child(thumb_el))
                        .child(div().flex_1().min_w(px(0.0)).line_height(relative(1.3)).child(crate::ui::titled(&t, div().id("title").truncate().font_weight(FontWeight::MEDIUM).text_color(t.t_on_art()).child(song.title.clone()).full_text_tip(&t, song.title.clone()), song.explicit, TEXT_MD)).child(div().text_size(t.text(TEXT_SM)).child(self.artist_links(("queue-now-artist", 0), &song, gpui::Hsla::from(t.t_on_art()).alpha(0.7), cx))))
                        .child(div().flex_none().text_size(t.text(TEXT_SM)).text_color(t.t_on_art()).opacity(0.65).child(clock(song.duration_s))),
                )
                .into_any_element();
        };
        let selected = self.selection.contains(song_ix) && self.selection_list == crate::shell::ListId::Queue;
        let artist_line = self.artist_links(("queue-artist", pos), &song, gpui::Hsla::from(t.t_on_art()).alpha(0.7), cx);
        let is_now = self.player.song_ix == Some(song_ix as usize);
        let (lifted, carried) = self.row_drag_marks(DragList::Queue, pos);
        let dragging = self.row_dragging();
        let (list, upcoming) = self.queue_view_list();
        let auto = pos >= upcoming;
        let draggable = self.player.station.is_none() && (upcoming > 1 || list.len() > upcoming);
        // Any row can be dragged out to a playlist, even where the queue can't be reordered.
        let pressable = self.player.station.is_none();
        let out = self.row_drag_out(DragList::Queue, pos);
        let row_bg: Option<Hsla> = selected.then(|| t.on_art_selected().into());
        let title_color: Hsla = if is_now { t.accent_soft_hsla() } else { t.t_on_art() };
        let group = "queue-row";
        // Autoplay's rows are dimmed until hovered or selected.
        let dim = |d: gpui::Div| if auto && !selected && !lifted { d.opacity(0.6).group_hover(group, |s| s.opacity(1.0)) } else { d };
        let removable = self.player.station.is_none() && !dragging;
        // A grip replaces the time on hover where rows can be moved.
        let time = div().flex_none().text_size(t.text(TEXT_SM)).text_color(t.t_on_art()).opacity(0.65).child(clock(song.duration_s));
        let grip = div().absolute().right(px(0.0)).top(px(0.0)).bottom(px(0.0)).flex().items_center().opacity(0.0).child(icon(&t, "dots-six-vertical-bold", 18.0, gpui::Hsla::from(t.t_on_art()).alpha(0.6)));
        let end = div()
            .relative()
            .flex_none()
            .child(if (draggable || removable) && !dragging { time.group_hover(group, |s| s.opacity(0.0)) } else { time })
            .when(draggable && !dragging, |d| d.child(grip.group_hover(group, |s| s.opacity(1.0))));
        // Every row but the playing one shows an X in the time's place on hover, left of the grip.
        let remove = removable.then(|| {
            div()
                .absolute()
                .right(t.s(if draggable { 22.0 } else { 0.0 }))
                .top(px(0.0))
                .bottom(px(0.0))
                .flex()
                .items_center()
                .opacity(0.0)
                .group_hover(group, |s| s.opacity(1.0))
                .child(
                    crate::ui::ink_button(&t, ("queue-remove", pos), if auto { tr("Remove from Autoplay") } else { tr("Remove from Queue") }, "x-bold", 14.0, t.t_on_art(), t.t_on_art())
                        .w(t.s(QUEUE_REMOVE_W))
                        .h(t.s(QUEUE_REMOVE_W))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.queue_remove(&[pos], cx);
                        })),
                )
        });
        let end = end.when_some(remove, |d, r| d.child(r));
        // While rows are lifted, the others slide apart to open a gap.
        let shift = self.row_drag_shift(DragList::Queue, pos);
        div().child(div()
            .relative()
            .top(shift)
            .px(t.s(SIDE_PAD))
            .child(
                div()
                    .id(("queue", pos))
                    .group(group)
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(t.s(11.0))
                    .h(t.s(ROW_HEIGHT))
                    .px(t.s(8.0))
                    .rounded(t.s(R_MD))
                    .relative()
                    .children(self.row_flash(song_ix, px(0.0), R_MD))
                    .when_some(row_bg, |d, bg| d.bg(bg))
                    .when(!dragging, |d| d.hover(|s| s.bg(t.on_art_hover())))
                    .when(lifted || carried, |d| d.opacity(0.0))
                    .when(out, |d| d.opacity(DRAG_OUT_DIM))
                    .when(pressable, |d| {
                        d.on_mouse_down(MouseButton::Left, cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                            // The thumbnail's play button never lifts the row.
                            let count = this.queue_view_list().0.len();
                            if let Some(b) = this.queue_list.bounds_for_item(this.queue_item_ix(pos))
                                && f32::from(event.position.x - b.left()) < f32::from(this.theme.s(SIDE_PAD + 8.0 + 42.0))
                            {
                                return;
                            }
                            this.row_press(DragList::Queue, pos, count, event, cx);
                        }))
                    })
                    .stop_row(&t, gpui::px(0.0)).on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                        if std::mem::take(&mut this.suppress_row_click) {
                            return;
                        }
                        let m = event.modifiers();
                        let count = if event.is_keyboard() { 2 } else { event.click_count() };
                        this.click_queue_row(pos, m.secondary(cx), m.shift, count, cx);
                    }))
                    .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| this.open_queue_row_menu(pos, event.position, window, cx)))
                    .child(
                        dim(div()
                            .relative()
                            .size(t.s(42.0))
                            .flex_none())
                            .child(thumb_el)
                            .child(
                                div()
                                    .id(("queue-play", pos))
                                    .tooltip(crate::ui::tip(&t, tr("Play")))
                                    .absolute()
                                    .inset_0()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .opacity(0.0)
                                    .when(!dragging, |d| d.group_hover(group, |s| s.opacity(1.0)))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        if std::mem::take(&mut this.suppress_row_click) {
                                            return;
                                        }
                                        this.jump_in_queue(pos, cx);
                                    }))
                                    .child(icon(&t, "play-fill", 16.0, t.t_on_art())),
                            ),
                    )
                    .child(
                        dim(div()
                            .flex_1()
                            .min_w(px(0.0))
                            .line_height(relative(1.3)))
                            .child(crate::ui::titled(&t, div().id("title").truncate().font_weight(FontWeight::MEDIUM).text_color(title_color).child(song.title.clone()).full_text_tip(&t, song.title.clone()), song.explicit, TEXT_MD))
                            .child(div().text_size(t.text(TEXT_SM)).child(artist_line)),
                    )
                    .child(dim(end)),
            ))
            .into_any_element()
    }

    pub(crate) fn jump_in_queue(&mut self, pos: usize, cx: &mut Context<Self>) {
        let (queue, first) = self.upcoming_queue();
        // A song Autoplay offers but hasn't handed over yet plays right after the current one.
        let (list, up_next) = self.queue_view_list();
        if self.core.is_some() && pos >= up_next + self.player.auto_tail && pos < list.len() {
            self.autoplay_play(list[pos], cx);
            return;
        }
        if self.core.is_some() {
            if self.player.queue.is_empty() {
                self.core_play_row(first + pos, cx);
            } else {
                self.core_jump_queue(first + pos, cx);
            }
            return;
        }
        self.player.queue = queue.to_vec();
        self.player.queue_pos = first + pos;
        self.player.song_ix = self.player.queue.get(first + pos).map(|s| *s as usize);
        self.player.position = 0.0;
        self.player.playing = true;
        self.on_song_changed(cx);
    }

    fn click_queue_row(&mut self, pos: usize, ctrl: bool, shift: bool, count: usize, cx: &mut Context<Self>) {
        let (list, _) = self.queue_view_list();
        self.select_in(crate::shell::ListId::Queue);
        self.selection.click(pos, &list, ctrl, shift);
        if count >= 2 && !ctrl && !shift {
            self.jump_in_queue(pos, cx);
        }
        cx.notify();
    }

    fn open_queue_row_menu(&mut self, pos: usize, position: gpui::Point<Pixels>, _window: &mut Window, cx: &mut Context<Self>) {
        let (list, _) = self.queue_view_list();
        self.select_in(crate::shell::ListId::Queue);
        self.selection.prepare_context(pos, &list);
        let positions: Vec<usize> = (0..list.len()).filter(|p| self.selection.contains(list[*p])).collect();
        let selected: Vec<u32> = positions.iter().map(|p| list[*p]).collect();
        let play = match positions[..] {
            [pos] => Self::play_row_item(move |this, cx| this.jump_in_queue(pos, cx)),
            _ => Self::play_songs_row(&selected),
        };
        let remove = (self.player.station.is_none() && !self.player.queue.is_empty()).then(|| self.queue_remove_row(positions));
        let rows = self.song_rows(&selected, Some(play), remove);
        self.open_menu(rows, position, false, cx);
        cx.notify();
    }

    pub fn layout_ref(&self, window: &Window) -> Layout {
        let mut layout = self.layout(self.edge.content_size(window.viewport_size()), window.scale_factor());
        if let Some(width) = self.layout_override {
            layout.main_width = width;
            layout.settled_width = width;
            layout.embedded = true;
        }
        layout
    }
}

/// Item positions of the queue view's parts.
#[derive(Clone, Copy, Debug)]
pub struct QueueSections {
    pub current: bool,
    /// Songs in Next in Queue and Next from.
    pub run: usize,
    pub rest: usize,
    pub auto_rows: usize,
    /// Placeholder rows after Autoplay's songs, while more are being found.
    pub finding: usize,
    pub queued_heading: Option<usize>,
    pub from_heading: Option<usize>,
    pub empty: Option<usize>,
    pub auto_heading: Option<usize>,
    pub auto_first: usize,
    pub count: usize,
}

impl QueueSections {
    /// The item a row of the queue list is drawn as.
    pub fn item(&self, pos: usize) -> usize {
        if pos < self.run {
            return self.queued_heading.map_or(0, |h| h + 1) + pos;
        }
        if pos < self.run + self.rest {
            return self.from_heading.map_or(0, |h| h + 1) + pos - self.run;
        }
        self.auto_first + pos - self.run - self.rest
    }
}
