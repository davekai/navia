//! The sign-in screen for first run and Change Server, including the one-time Deezer step. It replaces the whole shell.
//! Also the confirmation dialogs shared by Settings and Downloads.

use crate::i18n::{tr, trf, trn, trnf, tr_dyn};
use std::time::Duration;

use gpui::prelude::*;
use gpui::{AnyElement, Context, FontWeight, MouseButton, SharedString, div, px, relative};
use navia_core::connect::{Probe, parse_address, probe};

use crate::account::{self, NO_KEYRING, SavedAccount, SignInPrompt};
use crate::core_library::{Connection, CoreLibrary};
use crate::focus::FocusStop;
use crate::shell::{Layout, Shell};
use crate::theme::size::*;
use crate::ui::icon;
use crate::ui_controls::{DialogButton, TextTarget, dialog_button, glass_action, primary_action};
use crate::view_downloads::format_size;
use crate::view_window_buttons::Side;

/// Which field a problem is about. Its message shows under that field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Address,
    Account,
    /// Neither field. Shown above the buttons.
    Form,
}

#[derive(Clone, Debug)]
pub enum Outcome {
    /// `test` is true for Test, false for Sign In.
    Checking { test: bool },
    /// The server answered and the login worked. `line` is "<server> · <API>".
    Connected { line: SharedString },
    /// with no login to check.
    Found { line: SharedString },
    Problem { field: Field, title: SharedString, detail: SharedString },
    /// Why this screen is up, when that isn't an error.
    Note { title: SharedString, detail: SharedString },
}

pub struct ConnectState {
    pub address: String,
    pub user: String,
    pub password: String,
    pub reveal: bool,
    pub outcome: Option<Outcome>,
    pub from_settings: bool,
    /// The saved login was refused. Cancel then reads Browse Offline.
    pub rejected: bool,
    /// with the library already open behind it.
    pub deezer_step: bool,
    /// until the address changes.
    pub server_line: Option<SharedString>,
    generation: u64,
    busy: bool,
}

impl ConnectState {
    fn checking(&self) -> bool {
        self.busy || matches!(self.outcome, Some(Outcome::Checking { .. }))
    }

    fn complete(&self) -> bool {
        !self.address.trim().is_empty() && !self.user.trim().is_empty() && !self.password.is_empty()
    }

    fn insecure(&self) -> bool {
        parse_address(&self.address).is_some_and(|a| a.unencrypted)
    }
}

#[derive(Clone)]
pub enum Confirm {
    SignOut { host: SharedString },
    ClearDownloads { count: usize, total: u64, collections: usize },
    /// Cached songs, artwork and lyrics.
    ClearCache,
    /// Clearing `count` cached songs taking `total` bytes.
    ClearSongCache { count: usize, total: u64 },
    /// Turning off a playlist's or album's download.
    StopDownload { kind: navia_core::library::collections::CollectionKind, id: SharedString, name: SharedString, plan: navia_core::downloads::RemovalPlan },
    /// Adding songs to a playlist that already holds some of them. `fresh` are the new ones and `first` is the first
    /// song's title.
    Duplicates { id: SharedString, name: SharedString, songs: Vec<u32>, fresh: Vec<u32>, first: SharedString },
    /// Before following someone on the server.
    ListenAlong { user: String },
    /// Before following a Spotify friend, by Discord id.
    ListenAlongFriend { id: String },
    /// Deleting a share link. `title` is what it shares.
    DeleteShare { id: String, title: SharedString },
    /// `removed` downloaded songs leave the device with it.
    DeletePlaylist { id: SharedString, name: SharedString, removed: usize },
}

fn describe(result: Probe) -> Outcome {
    let address = |title: &str, detail: String| Outcome::Problem { field: Field::Address, title: title.to_string().into(), detail: detail.into() };
    let account = |title: &str, detail: String| Outcome::Problem { field: Field::Account, title: title.to_string().into(), detail: detail.into() };
    let problem = address;
    match result {
        Probe::Connected { server, api, .. } => Outcome::Connected { line: format!("{server} \u{b7} {api}").into() },
        Probe::Found { server, api } => Outcome::Found { line: format!("{server} \u{b7} {api}").into() },
        Probe::BadAddress => problem(tr("That isn't a web address"), tr("Use the full address, like https://music.example.org or http://192.168.1.20:4533.").to_string()),
        Probe::Refused => problem(tr("Can't reach this server"), tr("The connection was refused. Check the address and port, and that the server is running.").to_string()),
        Probe::TimedOut => problem(tr("Can't reach this server"), tr("It did not answer within 10 seconds. It may be down, or a firewall may be in the way.").to_string()),
        Probe::UnknownHost => problem(tr("No server found at this name"), tr("The name could not be resolved. Check the spelling, or your DNS or VPN.").to_string()),
        Probe::BadCertificate => problem(tr("This server's certificate isn't valid"), tr("The certificate is expired or doesn't match this name, so the connection was not made.").to_string()),
        Probe::NotSecure => problem(tr("Couldn't make a secure connection"), tr("This address didn't answer over HTTPS. If it uses plain HTTP, start the address with http://.").to_string()),
        Probe::Unreachable { detail } => problem(tr("Can't reach this server"), unreachable_detail(detail)),
        Probe::WrongLogin => account(tr("Wrong username or password"), tr("The server rejected this login (error 40).").to_string()),
        Probe::TokenUnsupported => account(tr("This server refused the sign-in method"), tr("It doesn't accept token sign-in (error 41), which is the only way Navia signs in.").to_string()),
        Probe::NotNavidrome { name } => problem(tr("This is not a Navidrome server"), trf("It answered as {name}. Navia only works with Navidrome.", &[("name", &name)])),
        Probe::NotSubsonic => problem(tr("This address didn't answer like a server"), tr("A web page came back, not the Subsonic API. If it sits behind a proxy or login page, point to Navidrome itself.").to_string()),
        Probe::ServerError { code, message } => Outcome::Problem { field: Field::Form, title: tr("The server reported a problem").into(), detail: trf("Error {code}: {message}", &[("code", &code.to_string()), ("message", &message)]).into() },
    }
}

/// Checks the server on a short-lived runtime, since no library exists yet on first run.
fn run_probe(address: String, user: String, password: String) -> Probe {
    match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt.block_on(probe(&address, &user, &password)),
        Err(_) => Probe::Unreachable { detail: tr("Navia couldn't start the connection check.").to_string() },
    }
}

/// `Probe::Unreachable` carries the core's English. Its formatted message gets the status pulled back out, and the
/// fixed ones are catalog keys.
fn unreachable_detail(detail: String) -> String {
    if let Some(status) = detail.strip_prefix("The server answered with HTTP ").and_then(|r| r.strip_suffix('.')) {
        return trf("The server answered with HTTP {status}.", &[("status", status)]);
    }
    tr_dyn(&detail)
}

impl Shell {
    pub fn show_sign_in(&mut self, prompt: SignInPrompt, cx: &mut Context<Self>) {
        let (address, user) = prompt.account.map_or((String::new(), String::new()), |a| (a.url, a.user));
        let outcome = prompt.note.map(|(title, detail)| Outcome::Note { title: title.into(), detail: detail.into() });
        let focus = if address.is_empty() { TextTarget::ConnectAddress } else { TextTarget::ConnectPassword };
        self.library_state = crate::shell::LibraryState::NoServer;
        self.connect = Some(ConnectState { address, user, password: String::new(), reveal: false, outcome, from_settings: false, rejected: false, deezer_step: false, server_line: None, generation: 0, busy: false });
        self.focus_field(focus, cx);
        cx.notify();
    }

    /// Opens Change Server, from Settings or after the server refused the saved login.
    pub fn open_change_server(&mut self, outcome: Option<Outcome>, cx: &mut Context<Self>) {
        let (address, user) = self.core.as_ref().map_or((String::new(), String::new()), |c| (c.lib.server_url().to_string(), c.lib.username().to_string()));
        self.connect = Some(ConnectState { address, user, password: String::new(), reveal: false, outcome, from_settings: true, rejected: false, deezer_step: false, server_line: None, generation: 0, busy: false });
        self.clear_toast();
        self.focus_field(TextTarget::ConnectPassword, cx);
        cx.notify();
    }

    /// The server refused the saved login.
    pub fn on_login_rejected(&mut self, cx: &mut Context<Self>) {
        if self.connect.is_some() {
            return;
        }
        let outcome = Outcome::Problem { field: Field::Account, title: tr("Wrong username or password").into(), detail: tr("The server rejected the saved login (error 40). Sign in again, or keep browsing what's on this device.").into() };
        self.open_change_server(Some(outcome), cx);
        if let Some(c) = self.connect.as_mut() {
            c.rejected = true;
        }
    }

    pub fn connect_field(&self, target: &TextTarget) -> String {
        let Some(c) = self.connect.as_ref() else { return String::new() };
        match target {
            TextTarget::ConnectAddress => c.address.clone(),
            TextTarget::ConnectUser => c.user.clone(),
            TextTarget::ConnectPassword => c.password.clone(),
            _ => String::new(),
        }
    }

    pub fn connect_field_typed(&mut self, target: &TextTarget, value: String, cx: &mut Context<Self>) {
        let Some(c) = self.connect.as_mut() else { return };
        match target {
            TextTarget::ConnectAddress => {
                c.address = value;
                c.server_line = None;
            }
            TextTarget::ConnectUser => c.user = value,
            TextTarget::ConnectPassword => c.password = value,
            _ => return,
        }
        // Results for the old text no longer apply.
        c.generation += 1;
        if !c.busy {
            c.outcome = None;
        }
        cx.notify();
    }

    pub fn field_masked(&self, target: &TextTarget) -> bool {
        *target == TextTarget::ConnectPassword && self.connect.as_ref().is_some_and(|c| !c.reveal)
    }

    pub fn connect_test(&mut self, cx: &mut Context<Self>) {
        let Some(c) = self.connect.as_ref() else { return };
        if c.address.trim().is_empty() || c.checking() {
            return;
        }
        self.start_probe(false, cx);
    }

    /// Enter in a field, or the Connect button.
    pub fn connect_submit(&mut self, cx: &mut Context<Self>) {
        let Some(c) = self.connect.as_ref() else { return };
        if !c.complete() || c.checking() {
            return;
        }
        self.start_probe(true, cx);
    }

    fn start_probe(&mut self, then_connect: bool, cx: &mut Context<Self>) {
        let Some(c) = self.connect.as_mut() else { return };
        c.generation += 1;
        c.outcome = Some(Outcome::Checking { test: !then_connect });
        let (generation, address, user, password) = (c.generation, c.address.clone(), c.user.clone(), c.password.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn({
                let (address, user, password) = (address.clone(), user.clone(), password.clone());
                async move { run_probe(address, user, password) }
            })
            .await;
            let _ = this.update(cx, |this, cx| this.on_probe(result, then_connect, generation, address, user, password, cx));
        })
        .detach();
    }

    #[allow(clippy::too_many_arguments)]
    fn on_probe(&mut self, result: Probe, then_connect: bool, generation: u64, address: String, user: String, password: String, cx: &mut Context<Self>) {
        let Some(c) = self.connect.as_mut() else { return };
        if c.generation != generation {
            return;
        }
        let connected = result.is_connected();
        let outcome = describe(result);
        if let Outcome::Found { line } | Outcome::Connected { line } = &outcome {
            c.server_line = Some(line.clone());
        }
        c.outcome = Some(outcome);
        if connected && then_connect {
            self.finish_connect(address, user, password, cx);
        }
        cx.notify();
    }

    /// replaces the current account, if any.
    fn finish_connect(&mut self, address: String, user: String, password: String, cx: &mut Context<Self>) {
        let Some(url) = parse_address(&address).map(|a| a.url) else { return };
        let user = user.trim().to_string();
        let previous = account::load_saved();
        if let Some(c) = self.connect.as_mut() {
            c.busy = true;
        }
        self.detach_core(cx);
        account::save_account(&url, &user);
        let keep_form_user = user.clone();
        let first = self.connect.as_ref().is_some_and(|c| !c.from_settings);
        cx.spawn(async move |this, cx| {
            let opened = cx.background_spawn(async move {
                if let Some(old) = previous.filter(|o| !(o.url == url && o.user == user)) {
                    account::forget_secret(&old.url, &old.user);
                }
                let saved = account::write_secret(&url, &user, &password);
                let opened = CoreLibrary::open(Connection::saved(url, user, password));
                // Show the Deezer step after a first sign-in if this account hasn't chosen yet.
                let ask = first && opened.as_ref().is_ok_and(|lib| lib.deezer_recommendations().is_none());
                (saved, opened, ask)
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                let (saved, opened, ask) = opened;
                match opened {
                    Ok(lib) => {
                        if ask {
                            if let Some(c) = this.connect.as_mut() {
                                c.busy = false;
                                c.outcome = None;
                                c.deezer_step = true;
                            }
                        } else {
                            this.connect = None;
                        }
                        this.controls.text = None;
                        let library: std::sync::Arc<dyn crate::model::Library> = lib.clone();
                        this.on_library(library, Some(lib), cx);
                        if saved.is_err() {
                            this.keyring_note = true;
                            this.show_toast_for(tr(NO_KEYRING), Duration::from_secs(6), cx);
                        }
                    }
                    Err(e) => {
                        eprintln!("navia: couldn't open the library for {keep_form_user}: {e}");
                        if let Some(c) = this.connect.as_mut() {
                            c.busy = false;
                            c.outcome = Some(Outcome::Problem { field: Field::Form, title: tr("Couldn't open the library on this device").into(), detail: e.to_string().into() });
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Signs out. Forgets the password and returns to sign-in. The account's library, queue and downloads stay on disk.
    pub fn sign_out(&mut self, cx: &mut Context<Self>) {
        let account = self.core.as_ref().map(|c| SavedAccount { url: c.lib.server_url().to_string(), user: c.lib.username().to_string() });
        self.confirm = None;
        self.detach_core(cx);
        account::clear_saved();
        if let Some(a) = account {
            std::thread::spawn(move || account::forget_secret(&a.url, &a.user));
        }
        self.keyring_note = false;
        self.show_sign_in(SignInPrompt::default(), cx);
    }

    /// A labeled form field. Clicking anywhere in it focuses it.
    #[allow(clippy::too_many_arguments)]
    fn connect_field_view(&mut self, label: &'static str, target: TextTarget, placeholder: &'static str, key: &str, invalid: bool, trailing: Option<AnyElement>, message: Option<AnyElement>, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let value = self.connect_field(&target);
        let line = self.field_line(&target, &value, placeholder, key, t.text(TEXT_MD));
        let focused = self.focused_target().as_ref() == Some(&target);
        let press_target = target.clone();
        let field = crate::ui::text_field_box(&t, gpui::SharedString::from(format!("connect-box-{key}")), focused, invalid)
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.focus_field(press_target.clone(), cx);
            }))
            .child(div().flex_1().min_w(px(0.0)).pr(t.s(FIELD_PAD - FIELD_INSET)).child(line))
            .children(trailing);
        crate::ui::text_field_block(&t, label, field, message).into_any_element()
    }

    /// A result or note in a box, with problems in the danger fill.
    fn connect_status(&self, kind: StatusKind, title: SharedString, detail: Option<SharedString>) -> AnyElement {
        let t = self.theme;
        let (name, color): (&str, gpui::Hsla) = match kind {
            StatusKind::Bad => ("warning-circle-fill", crate::theme::danger_ink()),
            StatusKind::Warn => ("lock-key-open-fill", t.t2().into()),
            StatusKind::Note => ("warning-circle-fill", t.t2().into()),
        };
        div()
            .flex()
            .flex_row()
            .gap(t.s(STATUS_GAP))
            .px(t.s(STATUS_PAD_X))
            .py(t.s(STATUS_PAD_Y))
            .rounded(t.s(R_MD))
            .line_height(relative(1.35))
            .when(kind == StatusKind::Bad, |d| d.bg(crate::theme::danger_fill()))
            .when(kind != StatusKind::Bad, |d| d.bg(gpui::rgba(crate::theme::STATUS_FILL)))
            .child(div().mt(t.s(1.0)).flex_none().child(icon(&t, name, 20.0, color)))
            .child(div().flex().flex_col().min_w(px(0.0)).child(div().font_weight(FontWeight::MEDIUM).child(title)).when_some(detail, |d, detail| d.child(div().mt(t.s(2.0)).text_size(t.text(TEXT_SM)).text_color(t.t2()).child(detail))))
            .into_any_element()
    }

    fn connect_spinner(&self, size: f32, color: gpui::Hsla) -> AnyElement {
        crate::ui::spinner(&self.theme, "connect-spin", size, color)
    }

    /// The card's heading and lead.
    fn connect_heading(&self, title: SharedString, lead: SharedString) -> gpui::Div {
        let t = self.theme;
        div()
            .flex()
            .flex_col()
            .gap(t.s(CONNECT_HEADING_GAP))
            .mb(t.s(CONNECT_HEADING_GAP))
            .child(div().text_size(t.text(TEXT_SECTION)).font_weight(FontWeight::SEMIBOLD).text_color(t.t_em()).child(title))
            .child(div().text_color(t.t2()).line_height(relative(1.4)).child(lead))
    }

    /// Saves the Deezer step choice for this account, then opens the app.
    fn connect_deezer_choice(&mut self, on: bool, cx: &mut Context<Self>) {
        self.set_deezer_choice(on, cx);
        self.connect = None;
        self.controls.text = None;
        cx.notify();
    }

    /// The one-time Deezer step.
    fn render_deezer_step(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let t = self.theme;
        let fact = |glyph: &str, text: &'static str| div().flex().flex_row().gap(t.s(STATUS_GAP)).child(div().flex_none().child(icon(&t, glyph, 20.0, t.t2()))).child(div().flex_1().min_w(px(0.0)).child(tr(text)));
        let not_now = glass_action(&t, "deezer-not-now").flex_1().child(tr("Not Now")).stop(&t).on_click(cx.listener(|this, _, _, cx| this.connect_deezer_choice(false, cx)));
        let turn_on = primary_action(&t, "deezer-turn-on", true).flex_1().child(tr("Turn On")).stop(&t).on_click(cx.listener(|this, _, _, cx| this.connect_deezer_choice(true, cx)));
        div()
            .flex()
            .flex_col()
            .gap(t.s(CONNECT_GAP))
            .child(self.connect_heading(tr("Use Deezer for recommendations?").into(), tr("Better Autoplay and Artist Radio, with suggestions from Deezer and ListenBrainz added to the server\u{2019}s, and a Heat column showing each album\u{2019}s most popular songs.").into()))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(t.s(STATUS_GAP))
                    .p(t.s(STATUS_PAD_X))
                    .rounded(t.s(R_MD))
                    .bg(gpui::rgba(crate::theme::STATUS_FILL))
                    .line_height(relative(1.35))
                    .child(fact("paper-plane-tilt-regular", "Sends artist and song names to Deezer and ListenBrainz from this computer."))
                    .child(fact("key-regular", "No Deezer account needed. Your login is never sent.")),
            )
            .child(div().text_size(t.text(TEXT_SM)).text_color(t.t3()).line_height(relative(1.4)).child(tr("You can change this any time in Settings > Playback.")))
            .child(div().flex().flex_row().gap(t.s(DIALOG_ACTIONS_GAP)).mt(t.s(DIALOG_ACTIONS_TOP)).child(not_now).child(turn_on))
    }

    pub fn render_connect(&mut self, _layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let Some(state) = self.connect.as_ref() else { return div().into_any_element() };
        let (from_settings, rejected, checking, complete, insecure, deezer_step) = (state.from_settings, state.rejected, state.checking(), state.complete(), state.insecure(), state.deezer_step);
        let address_empty = state.address.trim().is_empty();
        let reveal_on = state.reveal;
        let outcome = state.outcome.clone();
        let server_line = state.server_line.clone();
        let testing = matches!(outcome, Some(Outcome::Checking { test: true }));
        let signing_in = state.busy || matches!(outcome, Some(Outcome::Checking { test: false }));
        let problem = |want: Field| match &outcome {
            Some(Outcome::Problem { field, title, detail }) if *field == want => Some((title.clone(), detail.clone())),
            _ => None,
        };
        let (address_problem, account_problem) = (problem(Field::Address), problem(Field::Account));
        let form_note = match &outcome {
            Some(Outcome::Problem { field: Field::Form, title, detail }) => Some(self.connect_status(StatusKind::Bad, title.clone(), Some(detail.clone()))),
            Some(Outcome::Note { title, detail }) => Some(self.connect_status(StatusKind::Note, title.clone(), Some(detail.clone()))),
            _ => None,
        };

        let card_body = if deezer_step {
            self.render_deezer_step(cx)
        } else {
            let test_enabled = !address_empty && !checking;
            let test = crate::ui::field_action(&t, "connect-test", test_enabled || testing)
                .occlude()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(if testing { self.connect_spinner(18.0, t.t2().into()) } else { div().child(tr("Test")).into_any_element() })
                .when(test_enabled, |d| d.stop(&t).on_click(cx.listener(|this, _, _, cx| this.connect_test(cx))))
                .into_any_element();
            let reveal = crate::ui::field_action(&t, "connect-reveal", true)
                .tooltip(crate::ui::tip(&t, if reveal_on { tr("Hide password") } else { tr("Show password") }))
                .occlude()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(icon(&t, if reveal_on { "eye-slash-regular" } else { "eye-regular" }, 18.0, crate::ui::ink("connect-reveal").color(gpui::Hsla::from(t.t2()), t.hover_ink())))
                .stop(&t)
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(c) = this.connect.as_mut() {
                        c.reveal = !c.reveal;
                    }
                    cx.notify();
                }))
                .into_any_element();
            let address_message = if let Some((title, detail)) = address_problem.clone() {
                Some(self.connect_status(StatusKind::Bad, title, Some(detail)))
            } else if insecure {
                Some(self.connect_status(StatusKind::Warn, tr("This connection isn't encrypted").into(), Some(tr("Your password and music travel in the clear over plain HTTP. That's fine at home, but use HTTPS if this server is on the internet.").into())))
            } else {
                server_line.map(|line| {
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(t.s(CONNECT_HEADING_GAP))
                        .pl(t.s(FIELD_PAD))
                        .text_size(t.text(TEXT_SM))
                        .text_color(t.t2())
                        .child(icon(&t, "check-circle-fill", 18.0, t.accent_soft_hsla()))
                        .child(div().flex_1().min_w(px(0.0)).child(format!("{} \u{b7} {line}", tr("Server found"))))
                        .into_any_element()
                })
            };
            let account_message = account_problem.clone().map(|(title, detail)| self.connect_status(StatusKind::Bad, title, Some(detail)));
            let address = self.connect_field_view(tr("Server address"), TextTarget::ConnectAddress, "https://music.example.org", "connect-address", address_problem.is_some(), Some(test), address_message, cx);
            let user = self.connect_field_view(tr("Username"), TextTarget::ConnectUser, "", "connect-user", account_problem.is_some(), None, None, cx);
            let password = self.connect_field_view(tr("Password"), TextTarget::ConnectPassword, "", "connect-password", account_problem.is_some(), Some(reveal), account_message, cx);
            let submit_enabled = complete && !checking;
            let label = if signing_in { tr("Signing In\u{2026}") } else if from_settings { tr("Save and Reconnect") } else { tr("Sign In") };
            let submit = primary_action(&t, "connect-submit", submit_enabled || signing_in)
                .flex_1()
                .gap(t.s(CONNECT_HEADING_GAP))
                .when(signing_in, |d| d.child(self.connect_spinner(18.0, t.on_accent.hsla(1.0))))
                .child(label)
                .when(submit_enabled, |d| d.stop(&t).on_click(cx.listener(|this, _, _, cx| this.connect_submit(cx))));
            // Cancel returns to the app. After a refused login that means browsing offline.
            let cancel = from_settings.then(|| {
                glass_action(&t, "connect-cancel").flex_1().child(if rejected { tr("Browse Offline") } else { tr("Cancel") }).stop(&t).on_click(cx.listener(|this, _, _, cx| {
                    this.connect = None;
                    this.controls.text = None;
                    cx.notify();
                }))
            });
            div()
                .flex()
                .flex_col()
                .gap(t.s(CONNECT_GAP))
                .child(self.connect_heading(if from_settings { tr("Change Server") } else { tr("Sign in to a server") }.into(), tr("Enter the Navidrome server\u{2019}s address and the account you listen with.").into()))
                .child(address)
                .child(user)
                .child(password)
                .children(form_note)
                .child(div().flex().flex_row().gap(t.s(DIALOG_ACTIONS_GAP)).mt(t.s(DIALOG_ACTIONS_TOP)).children(cancel).child(submit))
        };
        let card = crate::ui::glass_card(&t).p(t.s(CONNECT_CARD_PAD)).child(card_body);
        let footer = div()
            .flex()
            .flex_row()
            .items_center()
            .self_center()
            .gap(t.s(CONNECT_HEADING_GAP))
            .mt(t.s(-SECTION_GAP / 2.0))
            .text_size(t.text(TEXT_SM))
            .text_color(t.t3())
            .child(icon(&t, "navidrome-logo", TEXT_LG, t.t3()))
            .child(tr("A Navidrome client"));
        let column = div()
            .w(t.s(CONNECT_W))
            .max_w_full()
            .flex()
            .flex_col()
            .gap(t.s(SECTION_GAP))
            .child(div().flex().justify_center().child(crate::ui::brand_lockup(&t, CONNECT_MARK, t.t_em())))
            .child(card)
            .child(footer);
        let drag = div().id("connect-drag").absolute().top(px(0.0)).left(px(0.0)).right(px(0.0)).h(t.s(TOPBAR_H)).map(|d| Shell::drag_area(d, cx));
        let left = self.render_window_buttons(Side::Left, false);
        let right = self.render_window_buttons(Side::Right, false);
        div().absolute().inset_0()
            .id("connect-screen")
            .bg(gpui::linear_gradient(180.0, gpui::linear_color_stop(gpui::rgb(crate::theme::BRAND_TOP), 0.0), gpui::linear_color_stop(gpui::rgb(crate::theme::BRAND_BOTTOM), 1.0)))
            .overflow_y_scroll()
            .child(drag)
            .child(div().absolute().top(t.s(WINDOW_CORNER_GAP)).left(t.s(WINDOW_CORNER_GAP)).child(left))
            .child(div().absolute().top(t.s(WINDOW_CORNER_GAP)).right(t.s(WINDOW_CORNER_GAP)).child(right))
            .child(div().relative().min_h_full().flex().flex_col().items_center().justify_center().px(t.s(GUTTER)).py(t.s(SECTION_GAP)).child(column))
            .into_any_element()
    }

    pub fn render_confirm(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = self.theme;
        let confirm = self.confirm.as_ref()?;
        if let Confirm::StopDownload { kind, id, name, plan } = confirm.clone() {
            return Some(self.render_stop_download(kind, id, name, plan, cx));
        }
        if let Confirm::Duplicates { id, name, songs, fresh, first } = confirm.clone() {
            return Some(self.render_duplicates(id, name, songs, fresh, first, cx));
        }
        if let Confirm::ListenAlong { user } = confirm.clone() {
            return self.render_listen_confirm(user, cx);
        }
        if let Confirm::ListenAlongFriend { id } = confirm.clone() {
            return self.render_friend_confirm(id, cx);
        }
        let (title, body, action): (SharedString, String, &'static str) = match confirm {
            Confirm::SignOut { host } => (tr("Sign out?").into(), trf("You'll need to sign in to {host} again. Your downloads stay on this device.", &[("host", host)]), tr("Sign Out")),
            Confirm::ClearDownloads { count, total, collections } => {
                let mut body = trnf("{count} song and {size} will be removed from this device. Nothing changes on the server.", "{count} songs and {size} will be removed from this device. Nothing changes on the server.", *count, &[("size", &format_size(*total))]);
                if *collections > 0 {
                    body.push(' ');
                    body.push_str(&trn("{count} playlist or album stops downloading.", "{count} playlists and albums stop downloading.", *collections));
                }
                (tr("Clear all downloads?").into(), body, tr("Clear All"))
            }
            Confirm::StopDownload { .. } | Confirm::Duplicates { .. } | Confirm::ListenAlong { .. } | Confirm::ListenAlongFriend { .. } => return None,
            Confirm::DeletePlaylist { name, removed, .. } => {
                let (title, body) = Shell::delete_playlist_text(name, *removed);
                (title.into(), body, tr("Delete"))
            }
            Confirm::DeleteShare { title, .. } => (tr("Delete this link?").into(), trf("The link to \u{201c}{title}\u{201d} stops working for everyone who has it.", &[("title", title)]), tr("Delete")),
            Confirm::ClearSongCache { count, total } => (
                tr("Clear cached songs?").into(),
                trnf("{count} song and {size} will be removed from this device. Downloads stay, and the song streams from the server the next time it plays.", "{count} songs and {size} will be removed from this device. Downloads stay, and the songs stream from the server the next time they play.", *count, &[("size", &format_size(*total))]),
                tr("Clear Cache"),
            ),
            Confirm::ClearCache => (tr("Clear the cache?").into(), tr("Cached songs, artwork and lyrics are removed from this device. Downloads and your library stay, and anything removed comes from the server again when it's needed.").to_string(), tr("Clear Cache")),
        };
        let clear_cache = matches!(confirm, Confirm::ClearCache);
        let clear_songs = matches!(confirm, Confirm::ClearSongCache { .. });
        let sign_out = matches!(confirm, Confirm::SignOut { .. });
        let delete_share = if let Confirm::DeleteShare { id, .. } = confirm { Some(id.clone()) } else { None };
        let delete_playlist = if let Confirm::DeletePlaylist { id, .. } = confirm { Some(id.clone()) } else { None };
        let cancel = dialog_button(&t, "confirm-cancel", DialogButton::Ghost, true)
            .child(tr("Cancel"))
            .stop_top(&t)
            .on_click(cx.listener(|this, _, _, cx| {
                this.confirm = None;
                cx.notify();
            }));
        let danger = dialog_button(&t, "confirm-action", DialogButton::Danger, true)
            .child(action)
            .stop_top(&t)
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(id) = delete_share.clone() {
                    this.delete_share(id, cx);
                } else if let Some(id) = delete_playlist.clone() {
                    this.delete_playlist(id, cx);
                } else if clear_songs {
                    this.confirm = None;
                    this.clear_song_cache(cx);
                } else if clear_cache {
                    this.confirm = None;
                    this.clear_caches(cx);
                } else if sign_out {
                    this.sign_out(cx);
                } else {
                    this.confirm = None;
                    this.clear_all_downloads(cx);
                }
            }));
        Some(
            crate::ui::dialog_scrim(&t, "confirm-scrim")
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                    this.confirm = None;
                    cx.notify();
                }))
                .child(
                    crate::ui::dialog_panel(&t, "confirm")
                        .child(crate::ui::dialog_title(&t, title))
                        .child(crate::ui::dialog_text(&t, body))
                        .child(crate::ui::dialog_actions(&t).child(cancel).child(danger)),
                )
                .into_any_element(),
        )
    }
}

impl Shell {
    /// "Stop downloading <name>?" with Cancel, Keep Songs when songs would be removed, and Remove or Stop Syncing.
    fn render_stop_download(&mut self, kind: navia_core::library::collections::CollectionKind, id: SharedString, name: SharedString, plan: navia_core::downloads::RemovalPlan, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let (body, note) = Shell::stop_download_text(&plan);
        let removes = plan.remove > 0;
        let cancel = dialog_button(&t, "stop-download-cancel", DialogButton::Ghost, true).child(tr("Cancel")).stop_top(&t).on_click(cx.listener(|this, _, _, cx| {
            this.confirm = None;
            cx.notify();
        }));
        let keep_id = id.clone();
        let keep = removes.then(|| {
            dialog_button(&t, "stop-download-keep", DialogButton::Ghost, true).child(tr("Keep Songs")).stop_top(&t).on_click(cx.listener(move |this, _, _, cx| this.stop_collection_download(kind, keep_id.clone(), true, cx)))
        });
        let action = dialog_button(&t, "stop-download-remove", if removes { DialogButton::Danger } else { DialogButton::Primary }, true)
            .child(if removes { tr("Remove") } else { tr("Stop Syncing") })
            .stop_top(&t)
            .on_click(cx.listener(move |this, _, _, cx| this.stop_collection_download(kind, id.clone(), false, cx)));
        crate::ui::dialog_scrim(&t, "stop-download-scrim")
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                this.confirm = None;
                cx.notify();
            }))
            .child(
                crate::ui::dialog_panel(&t, "stop-download")
                    .child(crate::ui::dialog_title(&t, trf("Stop downloading “{name}”?", &[("name", &name)])))
                    .child(crate::ui::dialog_text(&t, body))
                    .children(note.map(|n| crate::ui::dialog_note(&t, n)))
                    .child(crate::ui::dialog_actions(&t).child(cancel).children(keep).child(action)),
            )
            .into_any_element()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StatusKind {
    Bad,
    Warn,
    Note,
}
