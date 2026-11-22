//! Same-server listen-along, opt-in on both sides. The rules live in `navia_core::listen_along`.
//! This keeps "Listening Now" fresh, plays the followed song in place of the queue, which waits
//! untouched, and restores the queue when following ends.

use std::time::{Duration, Instant};

use gpui::Context;
use navia_core::audio::player::{PlayerStatus, QueueItem};
use navia_core::lanyard::{self as ly, FriendNow};
use navia_core::listen_along::{self as la, FollowStep, Followed, NowPlaying};

use crate::i18n::trf;
use crate::shell::Shell;

#[derive(Default)]
pub struct ListenState {
    pub listing: Vec<NowPlaying>,
    /// When `listing` was last read from the server. It's dropped once older than `la::LISTING_KEEP`.
    pub listing_at: Option<Instant>,
    pub following: Option<Following>,
    /// "<user> stopped listening", shown for a few seconds after following ends on its own.
    pub ended: Option<String>,
    ended_generation: u64,
    next_check: Option<Instant>,
    checking: bool,
    /// Where the listener's own song resumes after following, as its row, queue place and position.
    resume: Option<(u32, usize, u64)>,
    /// Spotify friends as Lanyard last reported them, with songs matched in the library.
    pub friends: Vec<FriendNow>,
    /// The watch generation `friends` was read at.
    friends_generation: Option<u64>,
    friends_checking: bool,
    /// The friend's song last skipped as missing from the library, so the notice shows once.
    skipped: Option<String>,
    /// The Lanyard address given to the watch, so an edited one applies once typing is done.
    applied_address: Option<String>,
    /// The Spotify friend being added in Settings.
    pub draft_id: String,
    pub draft_name: String,
    /// People listening along with this Navia at the last check, kept only while listeners may
    /// control playback, and when this user last changed playback.
    pub listeners: Vec<la::Listener>,
    pub host_change_ms: u64,
    /// the listener whose song or place this host took, while it plays it.
    led: Option<la::Led>,
    /// (control, following, session song queued).
    marks: Option<(bool, bool, bool)>,
    /// from asking the player to start a song mid-way to it playing, measured on the last start.
    start_latency_ms: u64,
    starting_since: Option<Instant>,
    /// the listening session's hand-over of queued songs at a song's end.
    pub handover: la::Handover,
    /// the player ends after the playing song: the other side's session song comes next.
    holding: bool,
}

/// The person followed and their song as it plays here.
pub struct Following {
    pub followed: Followed,
    pub song: NowPlaying,
    /// The song's row in this library, if any. It plays either way.
    pub rowid: Option<u32>,
    /// Where the listener's own song was, so it resumes from there.
    pub back_ms: u64,
    /// Following a Spotify friend whose song isn't in the library. Nothing plays until they play
    /// one that is.
    pub waiting: bool,
    /// The listener paused it, so it stays paused and following until they play again.
    pub held: bool,
    /// When the listener last changed playback while following, and the followed player's state at
    /// the last check.
    pub own_change_ms: Option<u64>,
    pub seen: Option<la::Seen>,
    /// the listener chose this song or place; the host comes to it.
    pub lead: bool,
    /// "Sending to <user>…" while the host hasn't taken the listener's change.
    pub sending: bool,
    pub two_way_since: Option<u64>,
}

impl Shell {
    /// Whether "Let listeners control playback" applies, needing sharing and the setting on.
    pub fn listeners_control(&self) -> bool {
        self.listen_active() && self.prefs.listeners_control
    }

    /// Whether the followed person lets listeners control playback.
    pub fn along_controls(&self) -> bool {
        self.listen.following.as_ref().is_some_and(|f| ly::friend_of(&f.followed.player).is_none() && f.song.allows_control())
    }

    /// Updates the reports' marks for the setting and whether this Navia follows someone.
    pub fn apply_listen_marks(&mut self) {
        let control = self.listeners_control();
        let following = self.listen.following.as_ref().is_some_and(|f| ly::friend_of(&f.followed.player).is_none());
        let queued = self.session_on() && self.session_mine();
        if self.listen.marks == Some((control, following, queued)) {
            return;
        }
        self.listen.marks = Some((control, following, queued));
        if let Some(core) = self.core.as_ref() {
            let lib = core.lib.clone();
            std::thread::spawn(move || lib.set_listen_marks(control, following, queued));
        }
    }

    /// where a song started mid-way should start so it lands where the other side is by then.
    fn catch_up_ms(&self, position_ms: u64, playing: bool) -> u64 {
        if playing { position_ms + self.listen.start_latency_ms } else { position_ms }
    }

    /// measures how long a mid-way start takes to play, for the next one.
    fn note_start(&mut self) {
        self.listen.starting_since = Some(Instant::now());
    }

    pub fn note_started(&mut self, status: PlayerStatus) {
        if status == PlayerStatus::Playing
            && let Some(at) = self.listen.starting_since.take()
        {
            self.listen.start_latency_ms = (at.elapsed().as_millis() as u64).min(la::RESYNC_DRIFT.as_millis() as u64);
        }
    }

    /// the listener's change waits for the host that lets listeners control it.
    fn mark_own_change(&mut self, lead: bool) {
        let controls = self.along_controls();
        if let Some(f) = self.listen.following.as_mut() {
            f.own_change_ms = controls.then(ly::now_ms);
            f.sending = controls;
            f.lead = controls && (lead || f.lead);
        }
    }

    /// a pause holds; play catches up at once. a host that lets listeners control mirrors it.
    pub fn along_toggle(&mut self, play: Option<bool>, cx: &mut Context<Self>) -> bool {
        let Some(f) = self.listen.following.as_mut() else { return false };
        let play = play.unwrap_or(!self.player.playing);
        f.held = !play;
        // without control, play catches up with the host at once
        self.mark_own_change(false);
        if let Some(core) = self.core.as_ref() {
            if play { core.player.play() } else { core.player.pause() }
        }
        self.player.playing = play;
        if play {
            self.check_listening(cx);
        }
        cx.notify();
        true
    }

    /// Seeks while following a host that lets listeners control playback, and their Navia mirrors
    /// it. Returns false otherwise, and seeking ends following.
    pub fn along_seek(&mut self, ms: u64, cx: &mut Context<Self>) -> bool {
        if !self.along_controls() {
            return false;
        }
        self.mark_own_change(true);
        if let Some(core) = self.core.as_ref() {
            core.player.seek_ms(ms);
        }
        self.player.position = ms as f32 / 1000.0;
        cx.notify();
        true
    }

    /// Plays a song while following a host that lets listeners control playback, and their Navia
    /// plays it too. Returns false otherwise, and playing something else ends following.
    pub fn along_play_song(&mut self, rowid: u32, cx: &mut Context<Self>) -> bool {
        if !self.along_controls() {
            return false;
        }
        let Some(core) = self.core.as_ref() else { return false };
        let Some((_, song)) = core.lib.db.songs_by_rowid(&[rowid]).ok().and_then(|r| r.into_iter().next()) else { return false };
        let Some(f) = self.listen.following.as_mut() else { return false };
        f.held = false;
        let entry = NowPlaying { song, state: Some(la::SessionState::Playing), position_ms: Some(0), ..f.song.clone() };
        f.song = entry.clone();
        self.mark_own_change(true);
        self.load_followed(&entry, cx);
        true
    }

    /// Next or Previous while following a host that allows control. Next plays the listener's own
    /// waiting song on both sides, Previous restarts the song.
    pub fn along_skip(&mut self, forward: bool, cx: &mut Context<Self>) -> bool {
        if !self.along_controls() {
            return false;
        }
        if !forward {
            return self.along_seek(0, cx);
        }
        let Some(&rowid) = self.player.queue.get(self.player.queue_pos) else { return false };
        if !self.along_play_song(rowid, cx) {
            return false;
        }
        // The queue waits under the followed song, and its place moves past the song played.
        if let Some(core) = self.core.as_mut() {
            core.pq.step_waiting();
        }
        self.mirror_queue();
        true
    }

    /// The host side when not following anyone. Tracks who listens along and, if allowed, mirrors
    /// their changes here.
    fn host_check(&mut self, entries: &[NowPlaying], cx: &mut Context<Self>) {
        if !self.listen_active() || self.listen.following.is_some() {
            self.listen.listeners.clear();
            return;
        }
        let Some(core) = self.core.as_ref() else { return };
        let Some(song_id) = self.player.current.as_ref().map(|s| s.id.to_string()) else {
            self.listen.listeners.clear();
            return;
        };
        let viewer = core.lib.username().to_string();
        let led = self.listen.led.clone().filter(|l| l.song_id == song_id);
        let host = la::Host { song_id, position_ms: (self.player.position.max(0.0) * 1000.0) as u64, playing: self.player.playing, own_change_ms: self.listen.host_change_ms, led };
        let now = la::listeners(entries, &viewer, &host, &self.listen.listeners, ly::now_ms());
        let mirror = la::host_mirror(&host, &self.listen.listeners, &now, self.listeners_control());
        self.listen.listeners = now;
        let Some(mirror) = mirror else { return };
        self.listen.led = mirror.led(&host, ly::now_ms());
        let player = core.player.clone();
        let seek = |this: &mut Self, ms: u64| {
            player.seek_ms(ms);
            this.player.position = ms as f32 / 1000.0;
        };
        match mirror.action {
            la::HostAction::Pause(place) => {
                place.into_iter().for_each(|ms| seek(self, ms));
                player.pause();
            }
            la::HostAction::Play(place) => {
                place.into_iter().for_each(|ms| seek(self, ms));
                player.play();
            }
            la::HostAction::Seek(ms) => seek(self, ms),
            la::HostAction::Song { song_id, position_ms, playing } => {
                let Some(rowid) = core.lib.rowid_for_song(&song_id) else { return };
                let clash = self.listen.handover.theirs_arrived(ly::now_ms());
                self.set_session_hold(false);
                // as a queued song; the host's queue carries on after it
                self.queue_songs_at(&[rowid], 0, true, cx);
                let at = self.catch_up_ms(position_ms, playing);
                self.note_start();
                let Some(core) = self.core.as_ref() else { return };
                core.player.next();
                if at > 0 {
                    core.player.seek_ms(at);
                }
                if playing { core.player.play() } else { core.player.pause() }
                // both sides started a session song at once: the listener's plays, the host's waits next
                if let Some(own) = clash {
                    self.session_put_back(&own, cx);
                }
            }
        }
        cx.notify();
    }

    /// a listening session: following someone whose Navia lets listeners control it, or hosting
    /// listeners with that setting on.
    pub fn session_on(&self) -> bool {
        if !self.listen_active() {
            return false;
        }
        match self.listen.following.as_ref() {
            Some(f) => ly::friend_of(&f.followed.player).is_none() && f.song.allows_control(),
            None => self.listeners_control() && !self.listen.listeners.is_empty(),
        }
    }

    /// this side's session songs are its Next in Queue (the listener's waiting queue's while following).
    pub fn session_mine(&self) -> bool {
        self.core.as_ref().is_some_and(|c| c.pq.queued_run() > 0)
    }

    /// who on the other side has songs queued for the session, by name.
    pub fn session_theirs(&self) -> Vec<String> {
        match self.listen.following.as_ref() {
            Some(f) => f.song.has_session_queue().then(|| f.followed.user.clone()).into_iter().collect(),
            None => self.listen.listeners.iter().filter(|l| l.queued).map(|l| l.user.clone()).collect(),
        }
    }

    fn session_left_ms(&self) -> Option<u64> {
        let song = self.player.current.as_ref().filter(|s| s.duration_s > 0)?;
        Some((song.duration_s as u64 * 1000).saturating_sub((self.player.position.max(0.0) * 1000.0) as u64))
    }

    /// each check: the turn for the coming end, the host's hold while it's the other side's, and
    /// the bounded wait at the end.
    fn session_check(&mut self) {
        if !self.session_on() {
            self.set_session_hold(false);
            self.listen.handover = la::Handover::default();
            return;
        }
        let host = self.listen.following.is_none();
        let (mine, theirs) = (self.session_mine(), !self.session_theirs().is_empty());
        if self.listen.handover.waiting_since.is_none() {
            let turn = self.listen.handover.plan(mine, theirs, host);
            if host {
                self.set_session_hold(turn == la::Turn::Theirs);
            }
        }
        if self.listen.handover.wait_over(ly::now_ms()) && host {
            // their song didn't come: the host's own queue carries on
            self.set_session_hold(false);
            if let Some(core) = self.core.as_ref() {
                core.player.next();
            }
        }
    }

    fn set_session_hold(&mut self, on: bool) {
        if self.listen.holding == on {
            return;
        }
        self.listen.holding = on;
        if let Some(core) = self.core.as_ref() {
            core.player.set_hold_after_current(on);
        }
    }

    /// the host's song ended while holding for the other side's song: wait for it (bounded), or
    /// carry on when nobody has one any more.
    pub fn session_queue_ended(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.listen.holding {
            return false;
        }
        let (mine, theirs) = (self.session_mine(), !self.session_theirs().is_empty());
        if self.session_on() && self.listen.handover.at_end(mine, theirs, true, ly::now_ms()) == la::Turn::Theirs {
            self.check_listening(cx);
            return true;
        }
        self.set_session_hold(false);
        if let Some(core) = self.core.as_ref() {
            core.player.next();
        }
        true
    }

    /// the host moved to the next song: its own session song, when that was the turn.
    pub fn session_track_changed(&mut self) {
        if self.listen.following.is_some() || !self.session_on() {
            return;
        }
        self.set_session_hold(false);
        let queued = self.core.as_ref().is_some_and(|c| c.pq.current_queued());
        let id = self.player.current.as_ref().map(|s| s.id.to_string()).unwrap_or_default();
        if queued && self.listen.handover.turn == Some(la::Turn::Mine) {
            self.listen.handover.started_mine(&id, ly::now_ms());
        } else if self.listen.handover.waiting_since.is_none() {
            self.listen.handover.song_changed();
        }
    }

    /// the followed song ended here: in a session, this side's turn plays its first queued song.
    pub fn session_followed_ended(&mut self, cx: &mut Context<Self>) {
        if !self.session_on() {
            return;
        }
        let (mine, theirs) = (self.session_mine(), !self.session_theirs().is_empty());
        if self.listen.handover.at_end(mine, theirs, false, ly::now_ms()) != la::Turn::Mine {
            return;
        }
        let Some(rowid) = self.core.as_mut().and_then(|c| c.pq.take_queued_next()) else { return };
        self.mirror_queue();
        if self.along_play_song(rowid, cx) {
            let id = self.player.current.as_ref().map(|s| s.id.to_string()).unwrap_or_default();
            self.listen.handover.started_mine(&id, ly::now_ms());
        }
    }

    /// a session song of this side's that didn't get played goes back to the front of its queue.
    fn session_put_back(&mut self, song_id: &str, cx: &mut Context<Self>) {
        if let Some(rowid) = self.core.as_ref().and_then(|c| c.lib.rowid_for_song(song_id)) {
            self.queue_songs_at(&[rowid], 0, true, cx);
        }
    }

    /// for last-change-wins with listeners.
    pub fn note_host_change(&mut self) {
        self.listen.host_change_ms = ly::now_ms();
        self.listen.led = None;
    }
    /// Others are listed only while this user shares too, with sharing on, Private mode off and a
    /// connection.
    pub fn listen_active(&self) -> bool {
        self.core.is_some() && self.prefs.share_listening && !self.private_mode && !self.is_offline()
    }

    /// Spotify friends show whenever there's a library and the server is reachable. Private mode
    /// doesn't hide them, since following them reveals nothing.
    pub fn friends_active(&self) -> bool {
        self.core.is_some() && !self.is_offline()
    }

    /// The Spotify friends whose activity shows a song.
    pub fn listening_friends(&self) -> Vec<FriendNow> {
        if !self.friends_active() {
            return Vec::new();
        }
        self.listen.friends.iter().filter(|f| f.listening()).cloned().collect()
    }

    /// Adds the typed Discord id and optional name as a Spotify friend. Returns whether it was added.
    pub fn add_friend(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(id) = ly::parse_friend_id(&self.listen.draft_id) else { return false };
        if self.prefs.spotify_friends.iter().any(|f| f.id == id) {
            return false;
        }
        let name = Some(self.listen.draft_name.trim().to_string()).filter(|n| !n.is_empty());
        self.prefs.spotify_friends.push(ly::Friend { id, name });
        self.prefs.save();
        self.listen.draft_id.clear();
        self.listen.draft_name.clear();
        self.controls.text = None;
        self.apply_friends(cx);
        true
    }

    /// Removes a Spotify friend.
    pub fn remove_friend(&mut self, id: &str, cx: &mut Context<Self>) {
        self.prefs.spotify_friends.retain(|f| f.id != id);
        self.prefs.save();
        self.listen.friends.retain(|f| f.friend.id != id);
        self.apply_friends(cx);
    }

    /// The followed person, if it's a Spotify friend.
    pub fn following_friend(&self) -> Option<&str> {
        self.listen.following.as_ref().and_then(|f| ly::friend_of(&f.followed.player))
    }

    pub fn following_friend_id(&self, id: &str) -> bool {
        self.following_friend() == Some(id)
    }

    /// Applies changed Spotify friend settings or a newly opened account to the watch. A friend
    /// removed while followed stops being followed.
    pub fn apply_friends(&mut self, cx: &mut Context<Self>) {
        if let Some(core) = self.core.as_ref() {
            core.lib.configure_friends(&self.prefs.lanyard_address, self.prefs.spotify_friends.clone());
            self.listen.applied_address = Some(self.prefs.lanyard_address.clone());
        }
        if let Some(id) = self.following_friend().map(str::to_string)
            && !self.prefs.spotify_friends.iter().any(|f| f.id == id)
        {
            self.stop_listening(false, cx);
        }
        self.listen.friends_generation = None;
        cx.notify();
    }

    /// Applies a change to sharing or Private mode. The reports' mark follows the setting, and
    /// turning sharing off ends following.
    pub fn apply_sharing(&mut self, cx: &mut Context<Self>) {
        if let Some(core) = self.core.as_ref() {
            let (lib, on) = (core.lib.clone(), self.prefs.share_listening);
            cx.background_executor().spawn(async move { lib.set_sharing(on) }).detach();
        }
        if !self.listen_active() {
            if self.listen.following.is_some() && self.following_friend().is_none() {
                self.stop_listening(false, cx);
            }
            self.listen.listing.clear();
        } else {
            self.listen.next_check = None;
        }
        cx.notify();
    }

    /// Starts the checks for an account. They end with it.
    pub fn start_listen_checks(&mut self, generation: u64, cx: &mut Context<Self>) {
        self.listen = ListenState::default();
        self.apply_friends(cx);
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let alive = this.update(cx, |shell, cx| {
                if shell.core_generation != generation {
                    return false;
                }
                shell.listen_tick(cx);
                true
            });
            if !matches!(alive, Ok(true)) {
                return;
            }
        })
        .detach();
    }

    fn listen_tick(&mut self, cx: &mut Context<Self>) {
        self.friends_tick(cx);
        self.apply_listen_marks();
        if !self.listen_active() {
            self.session_check();
            if !self.listen.listing.is_empty() {
                self.listen.listing.clear();
                cx.notify();
            }
            return;
        }
        // so failing checks leave nothing that looks current.
        if !self.listen.listing.is_empty() && self.listen.listing_at.is_none_or(|at| at.elapsed() > la::LISTING_KEEP) {
            self.listen.listing.clear();
            cx.notify();
        }
        if self.listen.next_check.is_none_or(|at| Instant::now() >= at) {
            self.check_listening(cx);
        }
    }

    /// Rereads Spotify friends whenever Lanyard reports something new, and every second while
    /// following one. The watch holds presences and matching reads the local library, so there's no
    /// network call here.
    fn friends_tick(&mut self, cx: &mut Context<Self>) {
        let typing = self.focused_target() == Some(crate::ui_controls::TextTarget::LanyardAddress);
        if !typing && self.listen.applied_address.as_deref() != Some(self.prefs.lanyard_address.as_str()) {
            self.apply_friends(cx);
        }
        let Some(core) = self.core.as_ref() else { return };
        let following = self.following_friend().is_some();
        core.lib.friends.set_following(following);
        let generation = core.lib.friends.generation();
        if self.listen.friends_checking || (!following && self.listen.friends_generation == Some(generation)) {
            return;
        }
        self.refresh_friends(cx);
    }

    /// takes one step while following one.
    fn refresh_friends(&mut self, cx: &mut Context<Self>) {
        let Some(core) = self.core.as_ref() else { return };
        if self.listen.friends_checking {
            return;
        }
        self.listen.friends_checking = true;
        let lib = core.lib.clone();
        let core_generation = self.core_generation;
        cx.spawn(async move |this, cx| {
            // Read the generation first, so a change landing during the read is picked up next tick.
            let (generation, friends) = cx.background_executor().spawn(async move { (lib.friends.generation(), lib.spotify_friends()) }).await;
            let _ = this.update(cx, |shell, cx| {
                shell.listen.friends_checking = false;
                if shell.core_generation != core_generation {
                    return;
                }
                let changed = shell.listen.friends != friends;
                shell.listen.friends = friends;
                shell.listen.friends_generation = Some(generation);
                if let Some(f) = shell.listen.following.as_ref().filter(|f| ly::friend_of(&f.followed.player).is_some()) {
                    let local = shell.local_along(f);
                    let step = ly::friend_step(&f.followed, &shell.listen.friends, &local, ly::now_ms());
                    shell.apply_follow_step(step, cx);
                }
                if changed {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Where the listener's own song is, to resume when following ends. Switching from following
    /// someone else keeps the position saved then.
    fn own_position_ms(&mut self) -> u64 {
        if let Some(prior) = self.listen.following.take() {
            return prior.back_ms;
        }
        if self.player.station.is_some() { 0 } else { (self.player.position.max(0.0) * 1000.0) as u64 }
    }

    /// The listener's player as following sees it.
    fn local_along(&self, f: &Following) -> la::Local {
        la::Local {
            song_id: f.song.song.id.clone(),
            position_ms: (self.player.position.max(0.0) * 1000.0) as u64,
            playing: self.player.playing,
            held: f.held,
            own_change_ms: f.own_change_ms,
            lead: f.lead,
            seen: f.seen.clone(),
            ended: self.player.status == PlayerStatus::Stopped,
            viewer: self.core.as_ref().map(|c| c.lib.username().to_string()).unwrap_or_default(),
            two_way_since: f.two_way_since,
            now_ms: ly::now_ms(),
        }
    }

    /// Asks the server now. The list refreshes every 15 s, every 5 s while following, and at once
    /// when the followed song ends. When following a Spotify friend, reads the friends instead.
    pub fn check_listening(&mut self, cx: &mut Context<Self>) {
        if self.following_friend().is_some() {
            self.refresh_friends(cx);
            return;
        }
        let Some(core) = self.core.as_ref() else { return };
        if self.listen.checking {
            return;
        }
        self.listen.checking = true;
        let lib = core.lib.clone();
        let generation = self.core_generation;
        cx.spawn(async move |this, cx| {
            let found = cx.background_executor().spawn(async move { lib.now_playing() }).await;
            let _ = this.update(cx, |shell, cx| {
                shell.listen.checking = false;
                if shell.core_generation != generation {
                    return;
                }
                // often enough to notice listeners join and act, and faster at a session song's end
                let session = la::check_every(shell.session_on(), shell.session_left_ms(), shell.listen.handover.waiting_since.is_some());
                let every = if shell.listen.following.is_some() || shell.listeners_control() { session } else { la::LIST_POLL };
                shell.listen.next_check = Some(Instant::now() + every);
                if let Some(entries) = found {
                    shell.on_now_playing(entries, cx);
                }
            });
        })
        .detach();
    }

    fn on_now_playing(&mut self, entries: Vec<NowPlaying>, cx: &mut Context<Self>) {
        let viewer = self.core.as_ref().map(|c| c.lib.username().to_string()).unwrap_or_default();
        self.listen.listing = la::people(&entries, &viewer, self.listen_active());
        self.listen.listing_at = Some(Instant::now());
        if let Some(f) = self.listen.following.as_ref().filter(|f| ly::friend_of(&f.followed.player).is_none()) {
            let now = ly::now_ms();
            let step = la::follow_step(&f.followed, &entries, &self.local_along(f));
            let seen = la::Seen::of(&f.followed, &entries, now);
            let two_way = la::two_way(&f.followed, &entries, &viewer);
            self.apply_follow_step(step, cx);
            if let Some(f) = self.listen.following.as_mut() {
                f.seen = seen;
                f.two_way_since = if two_way { f.two_way_since.or(Some(now)) } else { None };
            }
        }
        if let Some(f) = self.listen.following.as_mut()
            && let Some(e) = la::followed_entry(&f.followed, &entries)
        {
            f.song.player = e.player.clone();
        }
        self.host_check(&entries, cx);
        self.session_check();
        self.apply_listen_marks();
        cx.notify();
    }

    /// Applies what one follow check asks of the player.
    fn apply_follow_step(&mut self, step: FollowStep, cx: &mut Context<Self>) {
        match step {
            FollowStep::Ended => self.listen_gone(cx),
            FollowStep::Keep { player, .. } => {
                if let Some(f) = self.listen.following.as_mut() {
                    f.followed.player = player;
                    f.own_change_ms = None;
                    f.sending = false;
                }
            }
            FollowStep::Sending { player } => {
                if let Some(f) = self.listen.following.as_mut() {
                    f.followed.player = player;
                    f.sending = true;
                }
            }
            FollowStep::Resync { entry, player } => self.follow_to(entry, player, cx),
            FollowStep::NotTaken { entry, player } => {
                let user = entry.user.clone();
                let mine = self.listen.handover.started.take().map(|(id, _)| id).filter(|id| self.player.current.as_ref().is_some_and(|s| s.id.as_ref() == id.as_str()));
                self.follow_to(entry, player, cx);
                if let Some(id) = mine {
                    self.session_put_back(&id, cx);
                }
                self.show_toast(trf("{user} didn’t take your change", &[("user", &user)]), cx);
            }
            FollowStep::Lead { user } => {
                // they listen along with this Navia now; it keeps playing as their host
                self.stop_listening(false, cx);
                self.show_toast(trf("{user} is listening along with you", &[("user", &user)]), cx);
            }
            FollowStep::NotInLibrary { key, song, artist } => self.skip_missing(key, &song, &artist, cx),
        }
    }

    /// Handles a followed friend playing a song that isn't in the library. The player pauses, a
    /// toast says so once, and following continues to their next song.
    fn skip_missing(&mut self, key: String, song: &str, artist: &str, cx: &mut Context<Self>) {
        if self.player.playing
            && let Some(core) = self.core.as_ref()
        {
            core.player.pause();
            self.player.playing = false;
            self.player.status = PlayerStatus::Paused;
        }
        if let Some(f) = self.listen.following.as_mut() {
            f.waiting = true;
        }
        if self.listen.skipped.as_deref() != Some(key.as_str()) {
            self.listen.skipped = Some(key);
            self.show_toast(trf("“{song}” by {artist} isn’t in your library, so it was skipped.", &[("song", song), ("artist", artist)]), cx);
        }
        cx.notify();
    }

    /// Starts following a Spotify friend from the confirm dialog, at their position if the song is
    /// in the library. Otherwise following starts waiting, with the listener's song paused, until
    /// they play a song that is.
    pub fn listen_along_friend(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(friend) = self.listen.friends.iter().find(|f| f.friend.id == id).cloned() else { return };
        let Some(track) = friend.track.clone() else { return };
        self.listen.skipped = None;
        if let Some(entry) = friend.session(ly::now_ms()) {
            self.listen_along(entry, cx);
            return;
        }
        if self.core.is_none() {
            return;
        }
        self.listen.ended = None;
        let back_ms = self.own_position_ms();
        let song = navia_core::client::Song { title: track.song.clone(), ..Default::default() };
        let entry = NowPlaying { user: friend.display_name(), player: ly::friend_player(id), song, state: Some(la::SessionState::Playing), position_ms: Some(0), minutes_ago: 0 };
        let followed = Followed { user: entry.user.clone(), player: entry.player.clone() };
        self.listen.following = Some(Following { followed, song: entry, rowid: None, back_ms, waiting: true, held: false, own_change_ms: None, seen: None, lead: false, sending: false, two_way_since: None });
        // the confirm already said so, so no toast.
        self.listen.skipped = Some(track.key());
        self.skip_missing(track.key(), &track.song, track.first_artist(), cx);
    }

    /// Starts following `entry`'s user after the confirm. Their song plays here from the same
    /// moment, in place of the queue.
    pub fn listen_along(&mut self, entry: NowPlaying, cx: &mut Context<Self>) {
        if !entry.can_listen_along() || self.core.is_none() {
            return;
        }
        self.listen.ended = None;
        // The listener's own song resumes from here when following ends, or from the earlier spot
        // when switching from following someone else.
        let back_ms = self.own_position_ms();
        if self.player.station.is_some() {
            self.player.station = None;
        }
        let followed = Followed { user: entry.user.clone(), player: entry.player.clone() };
        self.listen.following = Some(Following { followed, song: entry.clone(), rowid: None, back_ms, waiting: false, held: false, own_change_ms: None, seen: None, lead: false, sending: false, two_way_since: None });
        self.load_followed(&entry, cx);
        self.listen.next_check = Some(Instant::now() + la::FOLLOW_POLL);
    }

    /// Follows a change found by a check. A new song, a pause or resume, or too much drift.
    fn follow_to(&mut self, entry: NowPlaying, player: String, cx: &mut Context<Self>) {
        let Some(f) = self.listen.following.as_mut() else { return };
        f.followed.player = player;
        f.waiting = false;
        f.held = false;
        f.own_change_ms = None;
        f.lead = false;
        f.sending = false;
        let same_song = f.song.song.id == entry.song.id;
        f.song = entry.clone();
        let Some(core) = self.core.as_ref() else { return };
        let playing = entry.playing();
        let at = self.catch_up_ms(entry.position_ms.unwrap_or(0), playing);
        if !same_song {
            self.listen.handover.theirs_arrived(ly::now_ms());
            self.load_followed(&entry, cx);
            return;
        }
        // The same song, so catch up and match play or pause. These go straight to the player, not
        // through the controls that would end following.
        if ((self.player.position.max(0.0) * 1000.0) as u64).abs_diff(at) > la::RESYNC_DRIFT.as_millis() as u64 {
            core.player.seek_ms(at);
            self.player.position = at as f32 / 1000.0;
        }
        if playing != self.player.playing {
            if playing { core.player.play() } else { core.player.pause() }
        }
    }

    /// Plays the followed song alone in the player. The listener's queue stays as it is.
    fn load_followed(&mut self, entry: &NowPlaying, cx: &mut Context<Self>) {
        if self.core.is_none() {
            return;
        }
        let rowid = self.core.as_ref().and_then(|c| c.lib.rowid_for_song(&entry.song.id));
        let duration = entry.song.duration.map(|d| d.max(0) as u64 * 1000);
        let playing = entry.playing();
        let at = if entry.position_ms.unwrap_or(0) > 0 { self.catch_up_ms(entry.position_ms.unwrap_or(0), playing) } else { 0 };
        self.note_start();
        let Some(core) = self.core.as_mut() else { return };
        // no crossfade.
        core.player.set_following(true);
        core.player.set_queue(vec![QueueItem::new(entry.song.id.clone(), duration)], 0, at, !playing);
        let song = crate::core_library::song_from_core(entry.song.clone());
        if let Some(f) = self.listen.following.as_mut() {
            f.rowid = rowid;
        }
        self.player.current = Some(song);
        self.player.song_ix = Some(rowid.map_or(crate::core_player::LIVE_ROW, |r| r as usize));
        self.player.position = at as f32 / 1000.0;
        self.player.playing = playing;
        self.player.status = if playing { PlayerStatus::Buffering } else { PlayerStatus::Paused };
        self.lyrics.clear();
        self.lyrics_song = None;
        self.request_tint(cx);
        self.publish_integrations(true);
        cx.notify();
    }

    /// The listener took control, so following ends with a toast. The followed song stays in the
    /// queue just before their own song. Returns true when following was on.
    pub fn take_control_from_along(&mut self, cx: &mut Context<Self>) -> bool {
        self.stop_listening(true, cx)
    }

    /// Something else starts playing, so following ends with a toast and the followed song isn't kept.
    pub fn drop_along(&mut self, cx: &mut Context<Self>) {
        let Some(f) = self.listen.following.take() else { return };
        if let Some(core) = self.core.as_ref() {
            core.player.set_following(false);
        }
        self.listen.resume = None;
        self.show_toast(trf("Stopped listening along with {user}", &[("user", &f.followed.user)]), cx);
    }

    /// They stopped or went quiet. The song stays, paused, and the player says so for a moment.
    fn listen_gone(&mut self, cx: &mut Context<Self>) {
        let Some(user) = self.listen.following.as_ref().map(|f| f.followed.user.clone()) else { return };
        self.settle_followed(true, cx);
        self.listen.ended = Some(user);
        self.listen.ended_generation += 1;
        let generation = self.listen.ended_generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(la::ENDED_NOTICE).await;
            let _ = this.update(cx, |shell, cx| {
                if shell.listen.ended_generation == generation {
                    shell.listen.ended = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The X on "<user> stopped listening".
    pub fn dismiss_listen_ended(&mut self, cx: &mut Context<Self>) {
        self.listen.ended = None;
        cx.notify();
    }

    /// Ends following. `toast` is set when the listener took control. Returns whether following was on.
    pub fn stop_listening(&mut self, toast: bool, cx: &mut Context<Self>) -> bool {
        let Some(user) = self.listen.following.as_ref().map(|f| f.followed.user.clone()) else { return false };
        self.settle_followed(false, cx);
        if toast {
            self.show_toast(trf("Stopped listening along with {user}", &[("user", &user)]), cx);
        }
        true
    }

    /// Makes the followed song the current queue entry, just before the listener's own song, which
    /// resumes where it was. The player keeps playing without a break. `pause` stops it there.
    fn settle_followed(&mut self, pause: bool, cx: &mut Context<Self>) {
        let Some(f) = self.listen.following.take() else { return };
        let Some(core) = self.core.as_ref() else { return };
        let player = core.player.clone();
        // The listener's own queue crossfades again. The followed song still ends with a cut.
        player.set_following(false);
        if pause {
            player.pause();
            self.player.playing = false;
            self.player.status = PlayerStatus::Paused;
        }
        let Some(rowid) = f.rowid else {
            // Not in this library, so the queue comes back as it was, paused at its song.
            self.listen.resume = None;
            if !self.player.queue.is_empty() {
                let at = self.player.queue_pos.min(self.player.queue.len() - 1);
                let edits = core.pq.load_edits();
                self.queue_edits_at(edits, f.back_ms, true);
                self.player.song_ix = Some(self.player.queue[at] as usize);
                self.player.current = self.song_peek(self.player.queue[at]);
                self.player.position = f.back_ms as f32 / 1000.0;
                self.player.playing = false;
                self.player.status = PlayerStatus::Paused;
            }
            cx.notify();
            return;
        };
        let pos = self.player.queue_pos.min(self.player.queue.len());
        let had_own = pos < self.player.queue.len();
        let own = self.player.queue.get(pos).copied();
        // The player holds only the followed song, so the queue is restored around it. Sent now, in
        // order, so a skip or seek that ended following reaches the player afterward.
        let Some(core) = self.core.as_mut() else { return };
        let edits = core.pq.keep_in_front(rowid);
        self.queue_edits(edits);
        self.player.song_ix = Some(rowid as usize);
        self.listen.resume = had_own.then(|| (own.unwrap_or_default(), pos + 1, f.back_ms));
        self.core_save_session();
        cx.notify();
    }

    /// Resumes the listener's own song from where it was after following.
    pub fn resume_after_along(&mut self, index: usize, rowid: u32) {
        let Some((row, at, ms)) = self.listen.resume else { return };
        if index != at || row != rowid {
            self.listen.resume = None;
            return;
        }
        self.listen.resume = None;
        if ms > 0
            && let Some(core) = self.core.as_ref()
        {
            core.player.seek_ms(ms);
            self.player.position = ms as f32 / 1000.0;
        }
    }

    pub fn following_user(&self, user: &str) -> bool {
        self.listen.following.as_ref().is_some_and(|f| ly::friend_of(&f.followed.player).is_none() && f.followed.user.eq_ignore_ascii_case(user))
    }
}
