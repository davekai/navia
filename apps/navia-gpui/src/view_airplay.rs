//! AirPlay in the player: the Output menu section, the PIN dialog, the Linux firewall prompt, and the lost-receiver
//! chip. The service and pairing calls are in `airplay.rs`.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{AnyElement, Context, Hsla, SharedString, div, px};
use navia_core::audio::airplay::firewall::{self, FirewallKind};
use navia_core::audio::airplay::{AirPlayOutput, DEFAULT_PORTS, ID_PREFIX, OutputState, Support};

use crate::i18n::{tr, trf};
use crate::shell::Shell;
use crate::theme::size::*;
use crate::ui_controls::{DialogButton, TextTarget, dialog_button, small_glass_action};
use crate::view_menu::MenuRow;

/// How long the AirPlay heading shows its spinner after the menu opens.
const SEARCH_SHOWN: Duration = Duration::from_millis(2500);
const PIN_LENGTH: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PinState {
    /// Asking the receiver to show a code.
    Asking,
    Waiting,
    Checking,
    /// The code was wrong, and the receiver is showing a new one.
    Wrong,
    /// The receiver didn't answer or dropped the code, so a new one is needed.
    Expired,
}

pub struct PinDialog {
    pub id: String,
    pub name: SharedString,
    pub code: String,
    pub state: PinState,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FirewallState {
    Blocked,
    /// The system password prompt is up, or the rule is being added.
    Allowing,
    /// The rule is in. Waiting to see whether the receiver connects back.
    Retrying,
    Still,
}

pub struct FirewallDialog {
    pub id: String,
    pub name: SharedString,
    /// The detected firewall, if it's one Navia knows (ufw or firewalld).
    pub kind: Option<FirewallKind>,
    /// The receiver's local network. The rule opens the ports to it.
    pub net: String,
    pub state: FirewallState,
    pub open: bool,
    pub copied: bool,
}

#[derive(Default)]
pub struct AirPlayUi {
    pub pin: Option<PinDialog>,
    pub firewall: Option<FirewallDialog>,
    /// The menu's heading spins until then.
    pub searching_until: Option<Instant>,
    /// The receiver that has played since it was picked, so losing it shows the chip.
    pub has_played: Option<String>,
    /// Playback was paused because that receiver was lost. Set once per loss.
    pub paused_for_loss: bool,
    /// Receivers the firewall prompt was already shown for this session.
    pub firewall_asked: HashSet<String>,
    /// Sample receivers for script previews (`page=airplay-…`).
    pub sample: Option<Vec<AirPlayOutput>>,
}

/// What the player says about a lost receiver.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Lost {
    Reconnecting,
    GaveUp,
}

impl Shell {
    /// Receivers as the menu lists them, from the service or a script preview.
    pub fn airplay_list(&self) -> Vec<AirPlayOutput> {
        match &self.airplay_ui.sample {
            Some(list) => list.clone(),
            None => self.airplay_outputs(),
        }
    }

    /// The receiver chosen as the output, if the output is one.
    pub fn active_airplay(&self) -> Option<AirPlayOutput> {
        let id = self.prefs.output_device.as_deref().filter(|d| d.starts_with(ID_PREFIX))?;
        self.airplay_list().into_iter().find(|o| o.id == id)
    }

    /// The Output button's glyph, and whether it uses the accent because a receiver is playing.
    pub fn output_glyph(&self) -> (&'static str, bool) {
        match self.active_airplay() {
            Some(o) => ("nv-airplay", o.state == Some(OutputState::Playing)),
            None => ("laptop-regular", false),
        }
    }

    /// The menu opened. The heading spins for a moment while discovery runs.
    pub fn airplay_menu_opened(&mut self, cx: &mut Context<Self>) {
        if self.airplay_ui.sample.is_some() {
            return;
        }
        if let Some(airplay) = self.core.as_ref().and_then(|c| c.airplay.as_ref()) {
            airplay.rescan();
        }
        self.airplay_ui.searching_until = Some(Instant::now() + SEARCH_SHOWN);
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_SHOWN).await;
            let _ = this.update(cx, |this, cx| {
                this.airplay_ui.searching_until = None;
                this.refresh_output_menu(cx);
            });
        })
        .detach();
    }

    fn airplay_searching(&self) -> bool {
        self.airplay_ui.searching_until.is_some_and(|until| Instant::now() < until)
    }

    /// The Output menu's AirPlay rows. Ready receivers come first, then sleeping ones, then unusable ones with the
    /// reason.
    pub fn airplay_menu_rows(&self) -> Vec<MenuRow> {
        let mut list = self.airplay_list();
        // A service that couldn't start lists nothing at all.
        if self.airplay_ui.sample.is_none() && self.core.as_ref().is_none_or(|c| c.airplay.is_none()) {
            return Vec::new();
        }
        let rank = |o: &AirPlayOutput| if unsupported(&o.support).is_some() { 2 } else if !o.found { 1 } else { 0 };
        list.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        let searching = self.airplay_searching();
        let mut rows = vec![MenuRow::Divider, MenuRow::Heading { label: tr("AirPlay").into(), icon: Some("nv-airplay"), busy: searching && !list.is_empty() }];
        if list.is_empty() {
            rows.push(if searching {
                MenuRow::detailed(tr("Looking for AirPlay devices…"), None, false, true, None)
            } else {
                MenuRow::detailed(tr("No AirPlay devices found"), Some(tr("Devices on the same network show up here.").into()), false, false, None)
            });
            return rows;
        }
        let active = self.prefs.output_device.clone();
        for o in list {
            if let Some(why) = unsupported(&o.support) {
                rows.push(MenuRow::detailed(o.name.clone(), Some(tr(why).into()), false, false, None));
                continue;
            }
            if !o.found {
                rows.push(MenuRow::detailed(o.name.clone(), Some(tr("Not found. It may be asleep.").into()), false, false, None));
                continue;
            }
            let chosen = active.as_deref() == Some(o.id.as_str());
            let state = if chosen { o.state.clone() } else { None };
            let detail: SharedString = match &state {
                Some(OutputState::Connecting) if self.airplay_ui.has_played.as_deref() == Some(o.id.as_str()) => tr("Reconnecting…").into(),
                Some(OutputState::Connecting) => tr("Connecting…").into(),
                Some(OutputState::Failed(_)) => tr("Couldn’t reconnect").into(),
                _ if o.needs_pin && !o.paired => trf("{model} · Asks for a code once", &[("model", o.model.as_str())]).into(),
                _ => o.model.clone().into(),
            };
            let busy = matches!(state, Some(OutputState::Connecting));
            let playing = matches!(state, Some(OutputState::Playing | OutputState::FirewallSuspected));
            let id = o.id.clone();
            let action: crate::view_menu::MenuAction = std::rc::Rc::new(move |this: &mut Shell, cx: &mut Context<Shell>| this.pick_receiver(id.clone(), cx));
            rows.push(MenuRow::detailed(o.name.clone(), (!detail.is_empty()).then_some(detail), playing, busy, Some(action)));
        }
        rows
    }

    /// Picks a receiver. An unpaired one asks for its code first.
    pub fn pick_receiver(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(o) = self.airplay_list().into_iter().find(|o| o.id == id) else { return };
        if self.prefs.output_device.as_deref() == Some(id.as_str()) && o.state == Some(OutputState::Playing) {
            return;
        }
        if o.needs_pin && !o.paired {
            self.open_airplay_pin(id, o.name.into(), cx);
            return;
        }
        self.set_output_device(Some(id), cx);
    }

    /// The output changed, so forget the old receiver's loss.
    pub fn airplay_output_changed(&mut self) {
        self.airplay_ui.has_played = None;
        self.airplay_ui.paused_for_loss = false;
    }

    // MARK: the PIN dialog

    fn open_airplay_pin(&mut self, id: String, name: SharedString, cx: &mut Context<Self>) {
        self.airplay_ui.pin = Some(PinDialog { id, name, code: String::new(), state: PinState::Asking });
        self.focus_field(TextTarget::AirPlayPin, cx);
        self.ask_for_code(cx);
    }

    /// Asks the receiver to show a new code.
    fn ask_for_code(&mut self, cx: &mut Context<Self>) {
        let Some(pin) = self.airplay_ui.pin.as_mut() else { return };
        pin.state = if pin.state == PinState::Wrong { PinState::Wrong } else { PinState::Asking };
        let id = pin.id.clone();
        if self.airplay_ui.sample.is_some() {
            self.pin_asked(&id, Ok(()), cx);
            return;
        }
        self.airplay_pair_begin(id.clone(), cx, move |this, result, cx| this.pin_asked(&id, result, cx));
        cx.notify();
    }

    fn pin_asked(&mut self, id: &str, result: Result<(), String>, cx: &mut Context<Self>) {
        let Some(pin) = self.airplay_ui.pin.as_mut().filter(|p| p.id == id) else { return };
        pin.state = match (result, pin.state) {
            // A wrong code's message stays until the next digit.
            (Ok(()), PinState::Wrong) => PinState::Wrong,
            (Ok(()), _) => PinState::Waiting,
            (Err(_), _) => PinState::Expired,
        };
        cx.notify();
    }

    pub fn airplay_pin_text(&self) -> String {
        self.airplay_ui.pin.as_ref().map(|p| p.code.clone()).unwrap_or_default()
    }

    pub fn airplay_pin_typed(&mut self, value: String, cx: &mut Context<Self>) {
        let Some(pin) = self.airplay_ui.pin.as_mut() else { return };
        if matches!(pin.state, PinState::Checking | PinState::Expired | PinState::Asking) {
            return;
        }
        let digits: String = value.chars().filter(char::is_ascii_digit).collect();
        // After a wrong code the next digit starts a new one.
        pin.code = if pin.state == PinState::Wrong { digits.chars().last().map(String::from).unwrap_or_default() } else { digits.chars().take(PIN_LENGTH).collect() };
        pin.state = PinState::Waiting;
        cx.notify();
    }

    pub fn airplay_pin_submit(&mut self, cx: &mut Context<Self>) {
        let Some(pin) = self.airplay_ui.pin.as_mut() else { return };
        if pin.code.len() != PIN_LENGTH || pin.state != PinState::Waiting {
            return;
        }
        pin.state = PinState::Checking;
        let (id, code) = (pin.id.clone(), pin.code.clone());
        cx.notify();
        if self.airplay_ui.sample.is_some() {
            let result = if code == "0000" { Err("wrong PIN".to_string()) } else { Ok(()) };
            self.pin_checked(id, result, cx);
            return;
        }
        let back = id.clone();
        self.airplay_pair_finish(id, code, cx, move |this, result, cx| this.pin_checked(back.clone(), result, cx));
    }

    fn pin_checked(&mut self, id: String, result: Result<(), String>, cx: &mut Context<Self>) {
        let Some(pin) = self.airplay_ui.pin.as_mut().filter(|p| p.id == id) else { return };
        match result {
            Ok(()) => {
                self.airplay_ui.pin = None;
                self.controls.text = None;
                if let Some(list) = self.airplay_ui.sample.as_mut() {
                    list.iter_mut().filter(|o| o.id == id).for_each(|o| o.paired = true);
                }
                self.set_output_device(Some(id), cx);
            }
            // The receiver drops the code after a wrong one, so ask for another.
            Err(e) if e.contains("wrong PIN") => {
                pin.state = PinState::Wrong;
                self.ask_for_code(cx);
            }
            Err(_) => pin.state = PinState::Expired,
        }
        cx.notify();
    }

    fn airplay_pin_new_code(&mut self, cx: &mut Context<Self>) {
        if let Some(pin) = self.airplay_ui.pin.as_mut() {
            pin.code.clear();
            pin.state = PinState::Asking;
        }
        self.focus_field(TextTarget::AirPlayPin, cx);
        self.ask_for_code(cx);
    }

    /// Closes the open dialog. Returns true if one was open.
    pub fn close_airplay_dialog(&mut self, cx: &mut Context<Self>) -> bool {
        if self.airplay_ui.pin.take().is_some() {
            self.controls.text = None;
            cx.notify();
            return true;
        }
        if self.airplay_ui.firewall.is_some() {
            self.airplay_play_here(cx);
            return true;
        }
        false
    }

    pub fn airplay_dialog_open(&self) -> bool {
        self.airplay_ui.pin.is_some() || self.airplay_ui.firewall.is_some()
    }

    // MARK: changes from the service

    /// Receivers changed. Updates the open menu, pauses when the active receiver is lost, and on Linux may ask about
    /// the firewall.
    pub fn on_airplay_changed(&mut self, cx: &mut Context<Self>) {
        self.refresh_output_menu(cx);
        let Some(active) = self.active_airplay() else {
            cx.notify();
            return;
        };
        match &active.state {
            Some(OutputState::Playing) => {
                self.airplay_ui.has_played = Some(active.id.clone());
                self.airplay_ui.paused_for_loss = false;
                if let Some(fw) = self.airplay_ui.firewall.as_ref().filter(|f| f.id == active.id && f.state == FirewallState::Retrying) {
                    let _ = fw;
                    self.airplay_ui.firewall = None;
                }
            }
            Some(OutputState::FirewallSuspected) => self.firewall_suspected(&active, cx),
            Some(OutputState::Connecting | OutputState::Failed(_)) if self.airplay_lost().is_some() && !self.airplay_ui.paused_for_loss => {
                // The receiver dropped. Pause rather than keep playing unheard, once per loss.
                self.airplay_ui.paused_for_loss = true;
                if self.player.playing
                    && let Some(core) = self.core.as_ref()
                {
                    core.player.pause();
                }
            }
            _ => {}
        }
        cx.notify();
    }

    /// Whether the chosen receiver was lost after it played.
    pub fn airplay_lost(&self) -> Option<(Lost, AirPlayOutput)> {
        let active = self.active_airplay()?;
        if self.airplay_ui.has_played.as_deref() != Some(active.id.as_str()) {
            return None;
        }
        match active.state {
            Some(OutputState::Connecting) => Some((Lost::Reconnecting, active)),
            Some(OutputState::Failed(_)) => Some((Lost::GaveUp, active)),
            _ => None,
        }
    }

    /// The chip's Try Again. Opens the same receiver again.
    pub fn airplay_reconnect(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.prefs.output_device.clone() else { return };
        let played = self.airplay_ui.has_played.clone();
        self.set_output_device(Some(id), cx);
        // Keep the lost receiver so the chip says "Reconnecting…".
        self.airplay_ui.has_played = played;
        self.airplay_ui.paused_for_loss = true;
    }

    /// Play Here. Switches back to this computer's default output.
    pub fn airplay_play_here(&mut self, cx: &mut Context<Self>) {
        self.airplay_ui.firewall = None;
        self.set_output_device(None, cx);
    }

    // MARK: the firewall prompt (Linux)

    fn firewall_suspected(&mut self, active: &AirPlayOutput, cx: &mut Context<Self>) {
        if !cfg!(target_os = "linux") {
            return;
        }
        if let Some(fw) = self.airplay_ui.firewall.as_mut().filter(|f| f.id == active.id) {
            // The rule is in and the receiver still doesn't connect back.
            if fw.state == FirewallState::Retrying {
                fw.state = FirewallState::Still;
                fw.open = true;
            }
            return;
        }
        if !self.airplay_ui.firewall_asked.insert(active.id.clone()) {
            return;
        }
        let net = match active.address {
            Some(std::net::IpAddr::V4(ip)) => firewall::local_network(ip),
            Some(other) => other.to_string(),
            None => String::new(),
        };
        let kind = if self.airplay_ui.sample.is_some() { Some(FirewallKind::Ufw) } else { firewall::detect() };
        self.airplay_ui.firewall = Some(FirewallDialog { id: active.id.clone(), name: active.name.clone().into(), kind, net, state: FirewallState::Blocked, open: false, copied: false });
        cx.notify();
    }

    /// Adds the firewall rule through the system password prompt, then reopens the receiver. Without a known firewall,
    /// or if the prompt fails, it reports the receiver as still blocked.
    fn firewall_allow(&mut self, cx: &mut Context<Self>) {
        let Some(fw) = self.airplay_ui.firewall.as_mut() else { return };
        let Some(kind) = fw.kind.filter(|_| !fw.net.is_empty()) else {
            fw.state = FirewallState::Still;
            cx.notify();
            return;
        };
        fw.state = FirewallState::Allowing;
        let net = fw.net.clone();
        cx.notify();
        if self.airplay_ui.sample.is_some() {
            return;
        }
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { firewall::allow(kind, &net, &DEFAULT_PORTS) }).await;
            let _ = this.update(cx, |this, cx| {
                let Some(fw) = this.airplay_ui.firewall.as_mut() else { return };
                match result {
                    Ok(()) => {
                        fw.state = FirewallState::Retrying;
                        let id = fw.id.clone();
                        let played = this.airplay_ui.has_played.clone();
                        this.set_output_device(Some(id), cx);
                        this.airplay_ui.has_played = played;
                    }
                    Err(e) => {
                        eprintln!("navia: firewall rule: {e}");
                        fw.state = FirewallState::Still;
                        fw.open = true;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    // MARK: drawing

    /// Where the music plays while on AirPlay. `compact` is the bar's title row. Otherwise it's a line under the
    /// artist.
    pub fn output_badge(&self, compact: bool, narrow: bool, color: Hsla) -> Option<AnyElement> {
        let t = self.theme;
        let o = self.active_airplay()?;
        let playing = o.state == Some(OutputState::Playing);
        let text: SharedString = match &o.state {
            Some(OutputState::Connecting) if self.airplay_lost().is_none() => trf("Connecting to {device}…", &[("device", o.name.as_str())]).into(),
            Some(OutputState::Playing) if !compact => trf("Playing on {device}", &[("device", o.name.as_str())]).into(),
            _ => o.name.clone().into(),
        };
        let glyph = crate::ui::icon(&t, "nv-airplay", if compact { 14.0 } else { 15.0 }, if playing { t.accent_hsla() } else { color });
        let badge = div()
            .id(if compact { "output-badge-bar" } else { "output-badge" })
            .flex()
            .flex_row()
            .items_center()
            .gap(t.s(OUTPUT_BADGE_GAP))
            .min_w(px(0.0))
            .text_size(t.text(if compact { TEXT_XS } else { TEXT_SM }))
            .text_color(color)
            .tooltip(crate::ui::tip(&t, text.clone()))
            .child(div().flex_none().child(glyph))
            .when(!(compact && narrow), |d| d.child(div().min_w(px(0.0)).truncate().child(text)));
        Some(if compact { badge.flex_none().max_w(t.s(OUTPUT_BADGE_MAX_W)).into_any_element() } else { badge.mt(t.s(2.0)).into_any_element() })
    }

    /// The PIN dialog or the firewall prompt, when one is open.
    pub fn render_airplay_dialog(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.airplay_ui.pin.is_some() {
            return Some(self.render_airplay_pin(cx));
        }
        if self.airplay_ui.firewall.is_some() {
            return Some(self.render_airplay_firewall(cx));
        }
        None
    }

    fn render_airplay_pin(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let pin = self.airplay_ui.pin.as_ref().expect("pin dialog");
        let (name, code, state) = (pin.name.clone(), pin.code.clone(), pin.state);
        let focused = self.focused_target() == Some(TextTarget::AirPlayPin);
        let shown = if state == PinState::Expired { String::new() } else { code.clone() };
        let boxes = crate::ui::dialog_pin(&t, "airplay-pin", &shown, PIN_LENGTH, focused, state == PinState::Wrong, matches!(state, PinState::Checking | PinState::Expired | PinState::Asking))
            .on_click(cx.listener(|this, _, _, cx| this.focus_field(TextTarget::AirPlayPin, cx)));
        // The keyboard needs a real field, so a hidden one sits over the boxes.
        let field = self.field_line(&TextTarget::AirPlayPin, &code, "", "airplay-pin-field", t.text(TEXT_MD));
        let boxes = div().relative().child(boxes).child(div().absolute().inset_0().opacity(0.0).child(field));
        let note = match state {
            PinState::Wrong => Some(crate::ui::dialog_note_error(&t, trf("That code didn’t work. {device} is showing a new one.", &[("device", name.as_ref())]))),
            PinState::Expired => Some(crate::ui::dialog_note_error(&t, tr("The code timed out."))),
            _ => None,
        };
        let cancel = dialog_button(&t, "airplay-pin-cancel", DialogButton::Ghost, true).child(tr("Cancel")).on_click(cx.listener(|this, _, _, cx| {
            this.close_airplay_dialog(cx);
        }));
        let action = if state == PinState::Expired {
            dialog_button(&t, "airplay-pin-new", DialogButton::Primary, true).child(tr("Show a New Code")).on_click(cx.listener(|this, _, _, cx| this.airplay_pin_new_code(cx)))
        } else {
            let ready = code.len() == PIN_LENGTH && state == PinState::Waiting;
            let label: AnyElement = if state == PinState::Checking {
                div().flex().flex_row().items_center().gap(t.s(8.0)).child(crate::ui::spinner(&t, "airplay-pin-spin", 15.0, t.on_accent.hsla(1.0))).child(tr("Pairing…")).into_any_element()
            } else {
                div().child(tr("Pair")).into_any_element()
            };
            dialog_button(&t, "airplay-pin-pair", DialogButton::Primary, ready).child(label).on_click(cx.listener(|this, _, _, cx| this.airplay_pin_submit(cx)))
        };
        crate::ui::dialog_scrim(&t, "airplay-pin-scrim")
            .on_mouse_down(gpui::MouseButton::Left, cx.listener(|this, _, _, cx| {
                this.close_airplay_dialog(cx);
            }))
            .child(
                crate::ui::dialog_panel(&t, "airplay-pin")
                    .child(crate::ui::dialog_title(&t, trf("Enter the code shown on {device}", &[("device", name.as_ref())])))
                    .child(crate::ui::dialog_text(&t, tr("Navia remembers this device, so the code is needed only once.")))
                    .child(boxes)
                    .children(note)
                    .child(crate::ui::dialog_actions(&t).child(cancel).child(action)),
            )
            .into_any_element()
    }

    fn render_airplay_firewall(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let fw = self.airplay_ui.firewall.as_ref().expect("firewall dialog");
        let (name, kind, state, open, copied) = (fw.name.clone(), fw.kind, fw.state, fw.open, fw.copied);
        let still = state == FirewallState::Still;
        let command = kind.map(|k| firewall::rule_text(k, &fw.net, &DEFAULT_PORTS));
        let mut panel = crate::ui::dialog_panel(&t, "airplay-firewall")
            .child(crate::ui::dialog_title(&t, tr("Your firewall is blocking AirPlay")))
            .child(crate::ui::dialog_text(&t, trf("{device} can’t connect back to Navia, so nothing plays. Allow AirPlay from devices on this network? The system asks for your password.", &[("device", name.as_ref())])));
        if still {
            panel = panel.child(crate::ui::dialog_note_error(
                &t,
                if command.is_some() { tr("Still blocked. Another rule or firewall may be in the way. The command below is what Navia ran.") } else { tr("Navia found neither ufw nor firewalld. Allow UDP 51310–51312 and TCP 51313 from this network in your firewall.") },
            ));
        }
        if let (Some(kind), Some(command)) = (kind, command) {
            let label = match kind {
                FirewallKind::Ufw => tr("Show the ufw command"),
                FirewallKind::Firewalld => tr("Show the firewalld command"),
            };
            panel = panel.child(crate::ui::dialog_disclosure(&t, "airplay-firewall-more", label, open).on_click(cx.listener(|this, _, _, cx| {
                if let Some(fw) = this.airplay_ui.firewall.as_mut() {
                    fw.open = !fw.open;
                }
                cx.notify();
            })));
            if open {
                let copy_text = command.clone();
                let copy = small_glass_action(&t, "airplay-firewall-copy")
                    .child(crate::ui::icon(&t, if copied { "check-bold" } else { "copy-regular" }, 14.0, t.t1()))
                    .child(if copied { tr("Copied") } else { tr("Copy") })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy_text.clone()));
                        if let Some(fw) = this.airplay_ui.firewall.as_mut() {
                            fw.copied = true;
                        }
                        cx.notify();
                    }));
                panel = panel.child(crate::ui::dialog_code(&t, command, copy));
            }
        }
        let here = dialog_button(&t, "airplay-firewall-here", DialogButton::Ghost, true).child(tr("Play Here")).on_click(cx.listener(|this, _, _, cx| this.airplay_play_here(cx)));
        let busy = matches!(state, FirewallState::Allowing | FirewallState::Retrying);
        let action = if still {
            dialog_button(&t, "airplay-firewall-retry", DialogButton::Primary, true).child(tr("Try Again")).on_click(cx.listener(|this, _, _, cx| this.firewall_allow(cx)))
        } else {
            let label: AnyElement = if busy {
                div().flex().flex_row().items_center().gap(t.s(8.0)).child(crate::ui::spinner(&t, "airplay-firewall-spin", 15.0, t.on_accent.hsla(1.0))).child(tr("Allowing…")).into_any_element()
            } else {
                div().child(tr("Allow")).into_any_element()
            };
            dialog_button(&t, "airplay-firewall-allow", DialogButton::Primary, !busy).child(label).on_click(cx.listener(|this, _, _, cx| this.firewall_allow(cx)))
        };
        crate::ui::dialog_scrim(&t, "airplay-firewall-scrim")
            .on_mouse_down(gpui::MouseButton::Left, cx.listener(|this, _, _, cx| {
                this.close_airplay_dialog(cx);
            }))
            .child(panel.child(crate::ui::dialog_actions(&t).child(here).child(action)))
            .into_any_element()
    }

    // MARK: script previews

    /// Fills the menu with sample receivers in the given state for `page=airplay-<state>`. No service or network is
    /// used.
    pub fn airplay_preview(&mut self, state: &str, cx: &mut Context<Self>) {
        let row = |id: &str, name: &str, model: &str, support: Support, paired: bool, needs_pin: bool, found: bool| AirPlayOutput {
            id: format!("{ID_PREFIX}{id}"),
            name: name.into(),
            model: model.into(),
            support,
            paired,
            needs_pin,
            state: None,
            found,
            address: Some(std::net::IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 40))),
        };
        let ready = |pairing| Support::Ready(navia_core::audio::airplay::Route { pairing, pcm: false });
        use navia_core::audio::airplay::Pairing;
        let mut list = vec![
            row("tv", "Living Room TV (sample)", "Sample TV", ready(Pairing::Pin), false, true, true),
            row("speaker", "Kitchen Speaker (sample)", "Sample speaker", ready(Pairing::Transient), false, false, true),
            row("bedroom", "Bedroom TV (sample)", "", ready(Pairing::Pin), true, true, false),
            row("laptop", "Office Laptop (sample)", "Sample laptop", Support::Restricted, false, false, true),
            row("den", "Den (sample)", "Sample receiver", Support::NeedsPassword, false, false, true),
            row("hall", "Hall (sample)", "Sample speaker", Support::NeedsPtp, false, false, true),
            row("old", "Old Receiver (sample)", "Sample receiver", Support::AirPlay1Only, false, false, true),
        ];
        let tv = format!("{ID_PREFIX}tv");
        let set_tv = |list: &mut Vec<AirPlayOutput>, state: Option<OutputState>| {
            if let Some(o) = list.iter_mut().find(|o| o.id == tv) {
                o.paired = true;
                o.state = state;
            }
        };
        self.airplay_ui = AirPlayUi { firewall_asked: HashSet::new(), ..AirPlayUi::default() };
        self.prefs.output_device = None;
        let mut open_menu = false;
        match state {
            "menu" => open_menu = true,
            "searching" | "empty" => {
                list.clear();
                self.airplay_ui.searching_until = (state == "searching").then(|| Instant::now() + Duration::from_secs(3600));
                open_menu = true;
            }
            "connecting" => {
                set_tv(&mut list, Some(OutputState::Connecting));
                self.prefs.output_device = Some(tv.clone());
                open_menu = true;
            }
            "playing" | "playing-menu" => {
                set_tv(&mut list, Some(OutputState::Playing));
                self.prefs.output_device = Some(tv.clone());
                open_menu = state == "playing-menu";
            }
            "lost" | "lost-gaveup" => {
                set_tv(&mut list, Some(if state == "lost" { OutputState::Connecting } else { OutputState::Failed("the receiver stopped answering".into()) }));
                self.prefs.output_device = Some(tv.clone());
                self.airplay_ui.has_played = Some(tv.clone());
                self.airplay_ui.paused_for_loss = true;
            }
            "firewall" | "firewall-still" => {
                set_tv(&mut list, Some(OutputState::FirewallSuspected));
                self.prefs.output_device = Some(tv.clone());
            }
            _ => {}
        }
        self.airplay_ui.sample = Some(list);
        match state {
            "pin" | "pin-wrong" | "pin-expired" => {
                self.open_airplay_pin(tv.clone(), "Living Room TV (sample)".into(), cx);
                if let Some(pin) = self.airplay_ui.pin.as_mut() {
                    pin.state = match state {
                        "pin-wrong" => PinState::Wrong,
                        "pin-expired" => PinState::Expired,
                        _ => PinState::Waiting,
                    };
                    if state == "pin-wrong" {
                        pin.code = "4821".into();
                    }
                }
            }
            "firewall" | "firewall-still" => {
                self.on_airplay_changed(cx);
                if let Some(fw) = self.airplay_ui.firewall.as_mut()
                    && state == "firewall-still"
                {
                    fw.state = FirewallState::Still;
                    fw.open = true;
                }
            }
            _ => {}
        }
        if open_menu {
            self.open_output_menu(gpui::point(px(1200.0), px(560.0)), cx);
        }
        cx.notify();
    }
}

/// Why a receiver can't be used, short enough for the menu row's second line.
fn unsupported(support: &Support) -> Option<&'static str> {
    match support {
        Support::Ready(_) => None,
        Support::Restricted => Some(crate::i18n::n_("Only accepts its owner’s devices")),
        Support::NeedsPtp => Some(crate::i18n::n_("Not supported yet: needs PTP timing")),
        Support::AirPlay1Only => Some(crate::i18n::n_("Not supported yet: AirPlay 1 only")),
        Support::NeedsPassword => Some(crate::i18n::n_("Not supported yet: needs a password")),
    }
}
