//! Listen-along views: Listening Now in the sidebar, the live mark, the Listen Along dialogs, and the queue while
//! following.

use gpui::prelude::*;
use gpui::{Animation, AnimationExt, AnyElement, Context, FontWeight, SharedString, div, px, relative};
use navia_core::lanyard::FriendNow;
use navia_core::listen_along::{NowPlaying, SessionState};

use crate::i18n::{tr, trf};
use crate::pages::Route;
use crate::shell::{Layout, Shell};
use crate::theme::Theme;
use crate::theme::size::*;
use crate::ui::{icon, icon_button};
use crate::view_cards::ArtShape;
use crate::view_queue::SIDE_PAD;
use crate::focus::FocusStop;

/// One live-mark bar's cycle, from 30% to full height and back.
const LIVE_BAR_SECS: f32 = 1.1;

/// Three moving bars when another person's player is playing, or a pause glyph when paused. None when the server
/// doesn't report playback.
pub fn live_mark(t: &Theme, key: &str, state: Option<SessionState>) -> Option<AnyElement> {
    match state? {
        SessionState::Paused => Some(div().flex_none().h(t.s(LIVE_MARK_H)).flex().items_center().child(icon(t, "pause-fill", LIVE_MARK_H, t.t3())).into_any_element()),
        SessionState::Starting | SessionState::Playing => {
            let mut bars = div().flex_none().flex().flex_row().items_end().gap(t.s(LIVE_BAR_GAP)).h(t.s(LIVE_MARK_H));
            // Each bar starts at its own point of the cycle.
            for (k, offset) in [0.4f32, 0.9, 0.2].into_iter().enumerate() {
                let h = t.s(LIVE_MARK_H);
                let bar = div().w(t.s(LIVE_BAR_W)).rounded(t.s(LIVE_BAR_W / 2.0)).bg(t.accent_hsla()).h(h);
                let id = SharedString::from(format!("live-{key}-{k}"));
                bars = bars.child(bar.with_animation(id, Animation::new(std::time::Duration::from_secs_f32(LIVE_BAR_SECS)).repeat(), move |el, delta| {
                    // Up and back down, eased and shifted by this bar's offset.
                    let phase = (delta + offset) % 1.0;
                    let wave = 1.0 - (2.0 * phase - 1.0).abs();
                    let eased = wave * wave * (3.0 - 2.0 * wave);
                    el.h(h * (0.3 + 0.7 * eased))
                }));
            }
            Some(bars.into_any_element())
        }
    }
}

impl Shell {
    /// "Listening Now": others on the server who share, then Spotify friends who are listening. Hidden when there's
    /// nobody.
    pub fn render_listening_now(&mut self, layout: &Layout, cx: &mut Context<Self>) -> Option<AnyElement> {
        let people = if self.listen_active() { self.listen.listing.clone() } else { Vec::new() };
        let friends = self.listening_friends();
        if people.is_empty() && friends.is_empty() {
            return None;
        }
        let mut group = div().flex().flex_col();
        for (k, e) in people.into_iter().enumerate() {
            group = group.child(self.person_row(k, e, layout, cx));
        }
        for (k, f) in friends.into_iter().enumerate() {
            group = group.child(self.friend_row(k, f, layout, cx));
        }
        Some(div().flex().flex_col().child(self.heading_line(tr("Listening Now"))).child(group).into_any_element())
    }

    /// A Spotify friend's avatar, their Discord picture or initials, with the friend mark on its corner.
    fn friend_avatar(&mut self, f: &FriendNow, layout: &Layout) -> AnyElement {
        let t = self.theme;
        let device = (f32::from(t.s(PERSON_ART)) * layout.scale).round() as u32;
        let id = f.avatar_url().map(|url| SharedString::from(format!("{}{url}", crate::art::AVATAR_PREFIX)));
        let image = id.and_then(|id| self.thumb_faded(&id, device));
        let initials = || {
            div()
                .size(t.s(PERSON_ART))
                .flex_none()
                .rounded_full()
                .bg(t.panel_2())
                .flex()
                .items_center()
                .justify_center()
                .text_size(t.text(FRIEND_INITIALS))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.t2())
                .child(navia_core::lanyard::initials(&f.display_name()))
        };
        let face: AnyElement = match image {
            Some((image, fade)) => crate::ui::fade_in(crate::ui::image(image).size(t.s(PERSON_ART)).rounded_full(), initials(), fade),
            None => initials().into_any_element(),
        };
        let mark = div()
            .absolute()
            .right(t.s(-2.0))
            .bottom(t.s(-2.0))
            .size(t.s(FRIEND_MARK))
            .rounded_full()
            .bg(t.accent_hsla())
            .flex()
            .items_center()
            .justify_center()
            .child(icon(&t, "link-simple-regular", FRIEND_MARK_GLYPH, t.on_accent.hsla(1.0)));
        div().relative().flex_none().size(t.s(PERSON_ART)).child(face).child(mark).into_any_element()
    }

    /// One Spotify friend's row: avatar, name, and "Title – Artist". The row opens the album when the song is in the
    /// library, and the headphones button asks before following.
    fn friend_row(&mut self, k: usize, f: FriendNow, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let Some(track) = f.track.clone() else { return div().into_any_element() };
        let name = f.display_name();
        let on = self.following_friend_id(&f.friend.id);
        let avatar = self.friend_avatar(&f, layout);
        let album: SharedString = f.song.as_ref().and_then(|s| s.album_id.clone()).unwrap_or_default().into();
        let artist = track.artist_line();
        let song_line = format!("{} – {}", track.song, artist);
        let label = if f.song.is_some() {
            trf("{user} on Spotify: {song} by {artist}", &[("user", &name), ("song", &track.song), ("artist", &artist)])
        } else {
            trf("{user} on Spotify: {song} by {artist}. Not in your library.", &[("user", &name), ("song", &track.song), ("artist", &artist)])
        };
        let in_library = f.song.is_some();
        // Put "Not in your library" before the song so it shows even when the line truncates.
        let sub_line = if in_library { song_line } else { format!("{} · {song_line}", tr("Not in your library")) };
        let group_name = SharedString::from(format!("friend-{k}"));
        let row = div()
            .id(("friend-row", k))
            .group(group_name.clone())
            .flex()
            .flex_row()
            .items_center()
            .gap(t.s(6.0))
            .h(t.s(45.0))
            .pl(t.s(14.0))
            .pr(t.s(6.0))
            .rounded(t.s(R_MD))
            .hover(|s| s.bg(t.row()))
            .child(
                div()
                    .id(("friend-link", k))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(t.s(12.0))
                    .flex_1()
                    .min_w(px(0.0))
                    .h_full()
                    .on_click(cx.listener({
                        let album = album.clone();
                        move |this, _, _, cx| {
                            if !album.is_empty() {
                                this.navigate(Route::Album(album.clone()), cx);
                                this.sidebar_drawer = false;
                            }
                        }
                    }))
                    .child(avatar)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w(px(0.0))
                            .line_height(relative(1.25))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(t.s(6.0))
                                    .min_w(px(0.0))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(t.t_nav())
                                    .child(div().truncate().child(name.clone()))
                                    .children(live_mark(&t, &format!("f{k}"), in_library.then_some(SessionState::Playing))),
                            )
                            .child(div().text_size(t.text(TEXT_XS)).text_color(t.t3()).truncate().child(sub_line)),
                    ),
            )
            .tooltip(crate::ui::tip(&t, label));
        let tip: SharedString = if on { tr("Stop listening along").into() } else { trf("Listen along with {user}", &[("user", &name)]).into() };
        let id = f.friend.id.clone();
        let button = icon_button(&t, ("friend-listen", k), tip, "headphones-regular", on)
            .flex_none()
            .on_click(cx.listener(move |this, _, _, cx| {
                if this.following_friend_id(&id) {
                    this.stop_listening(false, cx);
                } else {
                    this.confirm = Some(crate::view_connect::Confirm::ListenAlongFriend { id: id.clone() });
                    cx.notify();
                }
            }));
        row.child(div().flex_none().when(!on, |d| d.invisible().group_hover(group_name, |s| s.visible())).child(button)).into_any_element()
    }

    /// The confirmation before following a Spotify friend. It says what they play, whether it's in the listener's
    /// library, and that the queue waits.
    pub fn render_friend_confirm(&mut self, id: String, cx: &mut Context<Self>) -> Option<AnyElement> {
        use crate::ui_controls::{DialogButton, dialog_button};
        let t = self.theme;
        let friend = self.listen.friends.iter().find(|f| f.friend.id == id).cloned()?;
        let track = friend.track.clone()?;
        let name = friend.display_name();
        let artist = track.artist_line();
        let vars = [("user", name.as_str()), ("song", track.song.as_str()), ("artist", artist.as_str())];
        let what = if friend.song.is_some() {
            trf("{user} is playing “{song}” by {artist} on Spotify. You’ll hear it from your library, from the same moment, and follow along as they change songs.", &vars)
        } else {
            trf("{user} is playing “{song}” by {artist} on Spotify, which isn’t in your library. You’ll hear their next song that is, from the same moment.", &vars)
        };
        let own = if self.listen.following.is_some() || self.player.station.is_some() { None } else { self.player.current.as_ref().map(|s| s.title.clone()) };
        let waits = match own {
            Some(song) => trf("Your queue waits. When you stop, it carries on from “{song}”.", &[("song", &song)]),
            None => tr("Your queue isn’t changed.").to_string(),
        };
        let close = |this: &mut Shell, cx: &mut Context<Shell>| {
            this.confirm = None;
            cx.notify();
        };
        let cancel = dialog_button(&t, "listen-cancel", DialogButton::Ghost, true).child(tr("Cancel")).stop_top(&t).on_click(cx.listener(move |this, _, _, cx| close(this, cx)));
        let action = if friend.song.is_some() { tr("Listen Along") } else { tr("Listen Along Anyway") };
        let go = dialog_button(&t, "listen-go", DialogButton::Primary, true).child(action).stop_top(&t).on_click(cx.listener(move |this, _, _, cx| {
            this.confirm = None;
            this.listen_along_friend(&id, cx);
        }));
        Some(
            crate::ui::dialog_scrim(&t, "listen-scrim")
                .on_mouse_down(gpui::MouseButton::Left, cx.listener(move |this, _, _, cx| close(this, cx)))
                .child(
                    crate::ui::dialog_panel(&t, "listen-confirm")
                        .child(crate::ui::dialog_title(&t, trf("Listen along with {user}?", &[("user", &name)])))
                        .child(crate::ui::dialog_text(&t, what))
                        .child(crate::ui::dialog_text(&t, format!("{waits} {}", tr("Playing, pausing or skipping yourself stops listening along."))))
                        .child(crate::ui::dialog_note(&t, tr("Songs that aren’t in your library are skipped. Navia reads only what your friend shares through Lanyard, and nothing you play is shared with them.")))
                        .child(crate::ui::dialog_actions(&t).child(cancel).child(go)),
                )
                .into_any_element(),
        )
    }

    /// One person's row: cover, name with live mark, and song. The row opens the album, and the headphones button
    /// starts or stops following.
    fn person_row(&mut self, k: usize, e: NowPlaying, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let on = self.following_user(&e.user);
        let album: SharedString = e.song.album_id.clone().unwrap_or_default().into();
        let cover = self.artwork(Some(&album), "music-note-regular", PERSON_ART, ArtShape::Rounded(R_SM), layout);
        let artist = e.song.artist_line().unwrap_or_default();
        let song_line = format!("{} – {}", e.song.title, artist);
        let label = trf("{user}: {song} by {artist}", &[("user", &e.user), ("song", &e.song.title), ("artist", &artist)]);
        let row_key = ("person-row", k);
        let mut row = div()
            .id(row_key)
            .group(SharedString::from(format!("person-{k}")))
            .flex()
            .flex_row()
            .items_center()
            .gap(t.s(6.0))
            .h(t.s(45.0))
            .pl(t.s(14.0))
            .pr(t.s(6.0))
            .rounded(t.s(R_MD))
            .hover(|s| s.bg(t.row()))
            .child(
                div()
                    .id(("person-link", k))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(t.s(12.0))
                    .flex_1()
                    .min_w(px(0.0))
                    .h_full()
                    .on_click(cx.listener({
                        let album = album.clone();
                        move |this, _, _, cx| {
                            if !album.is_empty() {
                                this.navigate(Route::Album(album.clone()), cx);
                                this.sidebar_drawer = false;
                            }
                        }
                    }))
                    .child(div().flex_none().child(cover))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w(px(0.0))
                            .line_height(relative(1.25))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(t.s(6.0))
                                    .min_w(px(0.0))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(t.t_nav())
                                    .child(div().truncate().child(e.user.clone()))
                                    .children(live_mark(&t, &format!("p{k}"), e.state)),
                            )
                            .child(div().text_size(t.text(TEXT_XS)).text_color(t.t3()).truncate().child(song_line)),
                    ),
            )
            .tooltip(crate::ui::tip(&t, label));
        if e.can_listen_along() {
            let tip: SharedString = if on { tr("Stop listening along").into() } else { trf("Listen along with {user}", &[("user", &e.user)]).into() };
            let user = e.user.clone();
            let button = icon_button(&t, ("person-listen", k), tip, "headphones-regular", on)
                .flex_none()
                .on_click(cx.listener(move |this, _, _, cx| {
                    if this.following_user(&user) {
                        this.stop_listening(false, cx);
                    } else {
                        this.confirm = Some(crate::view_connect::Confirm::ListenAlong { user: user.clone() });
                        cx.notify();
                    }
                }));
            let group_name = SharedString::from(format!("person-{k}"));
            row = row.child(div().flex_none().when(!on, |d| d.invisible().group_hover(group_name, |s| s.visible())).child(button));
        }
        row.into_any_element()
    }

    /// The confirmation before following someone. It says what they're playing and that the listener's queue waits
    /// until following ends.
    pub fn render_listen_confirm(&mut self, user: String, cx: &mut Context<Self>) -> Option<AnyElement> {
        use crate::ui_controls::{DialogButton, dialog_button};
        let t = self.theme;
        let entry = self.listen.listing.iter().find(|e| e.user == user).cloned()?;
        let close = |this: &mut Shell, cx: &mut Context<Shell>| {
            this.confirm = None;
            cx.notify();
        };
        // following them back would make two people follow each other
        if navia_core::listen_along::listens_along(&self.listen.listeners, &user) {
            let done = dialog_button(&t, "listen-done", DialogButton::Primary, true).child(tr("Done")).stop_top(&t).on_click(cx.listener(move |this, _, _, cx| close(this, cx)));
            return Some(
                crate::ui::dialog_scrim(&t, "listen-scrim")
                    .on_mouse_down(gpui::MouseButton::Left, cx.listener(move |this, _, _, cx| close(this, cx)))
                    .child(
                        crate::ui::dialog_panel(&t, "listen-confirm")
                            .child(crate::ui::dialog_title(&t, trf("{user} is listening along with you", &[("user", &user)])))
                            .child(crate::ui::dialog_text(&t, trf("{user} already hears what you play, so you’re listening together.", &[("user", &user)])))
                            .child(crate::ui::dialog_actions(&t).child(done)),
                    )
                    .into_any_element(),
            );
        }
        let artist = entry.song.artist_line().unwrap_or_default();
        let vars = [("user", user.as_str()), ("song", entry.song.title.as_str()), ("artist", artist.as_str())];
        let what = if entry.state == Some(SessionState::Paused) {
            trf("{user} has paused “{song}” by {artist}. You’ll hear what {user} plays, from the same moment, and follow along as they skip, pause or seek.", &vars)
        } else {
            trf("{user} is playing “{song}” by {artist}. You’ll hear what {user} plays, from the same moment, and follow along as they skip, pause or seek.", &vars)
        };
        let own = if self.listen.following.is_some() || self.player.station.is_some() { None } else { self.player.current.as_ref().map(|s| s.title.clone()) };
        let waits = match own {
            Some(song) => trf("Your queue waits. When you stop, it carries on from “{song}”.", &[("song", &song)]),
            None => tr("Your queue isn’t changed.").to_string(),
        };
        let cancel = dialog_button(&t, "listen-cancel", DialogButton::Ghost, true).child(tr("Cancel")).stop_top(&t).on_click(cx.listener(move |this, _, _, cx| close(this, cx)));
        let go = dialog_button(&t, "listen-go", DialogButton::Primary, true).child(tr("Listen Along")).stop_top(&t).on_click(cx.listener(move |this, _, _, cx| {
            this.confirm = None;
            this.listen_along(entry.clone(), cx);
        }));
        Some(
            crate::ui::dialog_scrim(&t, "listen-scrim")
                .on_mouse_down(gpui::MouseButton::Left, cx.listener(move |this, _, _, cx| close(this, cx)))
                .child(
                    crate::ui::dialog_panel(&t, "listen-confirm")
                        .child(crate::ui::dialog_title(&t, trf("Listen along with {user}?", &[("user", &user)])))
                        .child(crate::ui::dialog_text(&t, what))
                        .child(crate::ui::dialog_text(&t, format!("{waits} {}", tr("Playing, pausing or skipping yourself stops listening along."))))
                        .child(crate::ui::dialog_note(&t, tr("You share what you play too, so others using Navia on this server can see it and listen along.")))
                        .child(crate::ui::dialog_actions(&t).child(cancel).child(go)),
                )
                .into_any_element(),
        )
    }

    /// The queue while following: their song as Now Playing, and a note that your own queue waits.
    pub fn render_queue_following(&mut self, scale: f32, _cx: &mut Context<Self>) -> Option<AnyElement> {
        let user = self.listen.following.as_ref()?.followed.user.clone();
        let t = self.theme;
        let song = self.player.current.clone()?;
        let thumb_px = (f32::from(t.s(42.0)) * scale).round() as u32;
        let thumb = self.thumb_faded(&song.album_id, thumb_px);
        let thumb_el: AnyElement = match thumb {
            Some((image, fade)) => crate::ui::fade_in(crate::ui::image(image).size(t.s(42.0)).rounded(t.s(R_XS)), div().size(t.s(42.0)).rounded(t.s(R_XS)).bg(t.on_art_placeholder()), fade),
            None => div().size(t.s(42.0)).rounded(t.s(R_XS)).bg(t.on_art_placeholder()).into_any_element(),
        };
        let own = self.player.queue.get(self.player.queue_pos).copied().map(|r| self.song_by_rowid(r).title).filter(|t| !t.is_empty());
        let hint = match own {
            Some(song) => trf("Your queue waits. When you stop, it carries on from “{song}”.", &[("song", &song)]),
            None => trf("Songs come from what {user} plays.", &[("user", &user)]),
        };
        Some(
            div()
                .flex()
                .flex_col()
                .child(div().pt(t.s(14.0)).pb(t.s(10.0)).pl(t.s(SIDE_PAD + 8.0)).text_size(t.text(TEXT_LG)).font_weight(FontWeight::SEMIBOLD).text_color(t.t_on_art()).child(tr("Now Playing")))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(t.s(12.0))
                        .px(t.s(SIDE_PAD + 8.0))
                        .py(t.s(8.0))
                        .child(thumb_el)
                        .child(div().flex_1().min_w(px(0.0)).line_height(relative(1.3)).child(div().truncate().font_weight(FontWeight::MEDIUM).text_color(t.t_on_art()).child(song.title.clone())).child(div().truncate().text_size(t.text(TEXT_SM)).text_color(gpui::Hsla::from(t.t_on_art()).alpha(0.7)).child(song.artist.clone())))
                        .child(div().flex_none().text_size(t.text(TEXT_SM)).text_color(t.t_on_art()).opacity(0.65).child(crate::ui::clock(song.duration_s))),
                )
                .child(
                    div()
                        .px(t.s(SIDE_PAD + 8.0))
                        .py(t.s(24.0))
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(t.s(8.0))
                        .text_center()
                        .text_color(t.t2())
                        .child(icon(&t, "headphones-regular", 26.0, t.t3()))
                        .child(div().text_size(t.text(TEXT_LG)).font_weight(FontWeight::SEMIBOLD).text_color(t.t1()).child(trf("Listening along with {user}", &[("user", &user)])))
                        .child(hint),
                )
                .into_any_element(),
        )
    }
}
