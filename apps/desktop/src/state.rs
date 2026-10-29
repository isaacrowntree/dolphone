//! The desktop app's state, and how it is drawn and changed.

use std::time::{Duration, Instant};

use phone_api::{
    keys, AlertInfo, AuditEntry, BtDevice, CallInfo, CallRecordInfo, Grant, MessageInfo, ModemStatus, Request,
    RouteInfo, ThreadInfo,
};
use phone_bus::{Bus, Event};
use phone_core::contacts::Book;
use phone_core::model::{CallState, MessageState, Outcome, Route};
use phone_core::{number, text, when};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::{with_app, AppWindow, Bubble, Choice, Row, Toggle};

/// The ON/OFF settings the desktop shows, by group: (key, default, label).
type Flag = ((&'static str, &'static str), &'static str);
const CALL_FLAGS: [Flag; 8] = [
    (keys::VIBRATE, "Vibrate when ringing"),
    (keys::CALL_WAITING, "Call waiting"),
    (keys::SILENCE_UNKNOWN, "Silence callers who are not in contacts"),
    (keys::SPEAK_CALLER, "Say who is calling (needs espeak-ng)"),
    (keys::STEADY_DECLINE, "On the panel, hold Decline to decline"),
    (keys::LOCAL_RINGBACK, "Play the ringing tone on this phone"),
    (keys::DISABLE_2G, "Keep the modem off 2G (weak encryption; less coverage)"),
    (keys::WARN_2G, "Warn when the modem falls back to 2G"),
];
const TEXT_FLAGS: [Flag; 4] = [
    (keys::DELIVERY_REPORTS, "Ask for delivery reports"),
    (keys::TEXT_TONE, "Play a tone when a text arrives"),
    (keys::PRIVATE_NOTIFY, "Hide message text in notifications"),
    (keys::DELETE_FROM_SIM, "Clear texts from the SIM once saved"),
];
const ALERT_FLAGS: [Flag; 4] = [
    (keys::ALERTS_EXTREME, "Extreme alerts"),
    (keys::ALERTS_SEVERE, "Severe alerts"),
    (keys::ALERTS_AMBER, "Amber alerts (missing children)"),
    (keys::ALERTS_TEST, "Test alerts"),
];

/// The panel's hold times, in milliseconds, as the desktop's menu lists them.
const HOLDS: [&str; 5] = ["400", "600", "800", "1000", "1500"];

fn all_flags() -> impl Iterator<Item = &'static Flag> {
    CALL_FLAGS.iter().chain(TEXT_FLAGS.iter()).chain(ALERT_FLAGS.iter())
}

pub struct App {
    pub ui: AppWindow,
    bus: Option<Bus>,
    pub region: String,
    pub book: Book,
    pub connected: bool,
    pub status: Option<ModemStatus>,
    pub calls: Vec<CallInfo>,
    pub recents: Vec<CallRecordInfo>,
    pub threads: Vec<ThreadInfo>,
    /// The open conversation, canonical, or empty.
    pub peer: String,
    pub messages: Vec<MessageInfo>,
    pub pending: Vec<Request>,
    pub routes: Vec<RouteInfo>,
    pub route: u8,
    pub ringer: String,
    pub flags: Vec<(&'static str, bool)>,
    pub voicemail: String,
    pub sip_password_set: bool,
    pub hold_ms: String,
    pub sip_always: bool,
    pub grants: Vec<Grant>,
    pub audit: Vec<AuditEntry>,
    pub alerts: Vec<AlertInfo>,
    /// What the last search found.
    pub found: Vec<MessageInfo>,
    /// Laptops over KDE Connect: (id, name, paired).
    pub laptops: Vec<(String, String, bool)>,
    /// The tray icon and launcher badge, when running for real.
    pub tray: Option<crate::tray::Tray>,
    /// The alert on screen, until dismissed.
    pub alert: Option<AlertInfo>,
    pub bt: (bool, bool),
    pub devices: Vec<BtDevice>,
    pub blocked: Vec<String>,
    pub unread: u32,
    pub missed: u32,
    toast: Option<(String, Instant)>,
    contacts_view: Vec<usize>,
    contacts_loaded: Instant,
}

impl App {
    pub fn new(ui: AppWindow, bus: Bus) -> Self {
        let mut a = Self::headless(ui);
        a.bus = Some(bus);
        a
    }

    /// Without a bus, for the screenshot tests.
    pub fn headless(ui: AppWindow) -> Self {
        let region = number::DEFAULT_REGION.to_string();
        let book = Book::load(&phone_core::store::contacts_dir(), &region);
        Self {
            ui,
            bus: None,
            region,
            book,
            connected: false,
            status: None,
            calls: Vec::new(),
            recents: Vec::new(),
            threads: Vec::new(),
            peer: String::new(),
            messages: Vec::new(),
            pending: Vec::new(),
            routes: Vec::new(),
            route: 0,
            ringer: "ring".into(),
            flags: all_flags().map(|((k, d), _)| (*k, *d == "true")).collect(),
            voicemail: String::new(),
            sip_password_set: false,
            hold_ms: "600".into(),
            sip_always: false,
            grants: Vec::new(),
            audit: Vec::new(),
            alerts: Vec::new(),
            found: Vec::new(),
            tray: None,
            laptops: Vec::new(),
            alert: None,
            bt: (false, false),
            devices: Vec::new(),
            blocked: Vec::new(),
            unread: 0,
            missed: 0,
            toast: None,
            contacts_view: Vec::new(),
            contacts_loaded: Instant::now(),
        }
    }

    fn bus(&self) -> Option<&Bus> {
        self.bus.as_ref()
    }

    fn ringing_id(&self) -> Option<u32> {
        self.calls.iter().find(|c| CallState::from_u8(c.state).ringing_in()).map(|c| c.id)
    }

    fn current_id(&self) -> Option<u32> {
        self.calls
            .iter()
            .find(|c| CallState::from_u8(c.state) == CallState::Active)
            .or_else(|| self.calls.iter().find(|c| CallState::from_u8(c.state).live()))
            .map(|c| c.id)
    }

    pub fn reload(&self) {
        let Some(b) = self.bus() else { return };
        b.refresh_calls();
        b.refresh_recents(200);
        b.refresh_threads();
        b.refresh_pending();
        b.refresh_status();
        b.refresh_routes();
        b.run(|c| async move {
            let mut out = vec![Event::Ringer(c.audio.ringer().await?), Event::Blocked(c.settings.blocked().await?)];
            let extra = [
                "region",
                "voicemail",
                "sip_enabled",
                "sip_account",
                "sip_proxy",
                "sip_password",
                "sip_use",
                "long_press_ms",
                "uplink_alert_to",
                "uplink_interface",
            ];
            for k in all_flags().map(|f| f.0 .0).chain(extra) {
                out.push(Event::Setting(k.into(), c.settings.get(k).await?));
            }
            out.push(Event::Alerts(c.messages.alerts(20).await?));
            // Grants and the audit log are for Dolphone's own apps; a desktop
            // installed somewhere phoned does not trust just goes without them.
            if let Ok(g) = c.settings.grants().await {
                out.push(Event::Grants(g));
            }
            if let Ok(a) = c.settings.audit(100).await {
                out.push(Event::Audit(a));
            }
            if let Ok(l) = c.settings.kdeconnect_devices().await {
                out.push(Event::Laptops(l));
            }
            out.push(Event::Setting("kdeconnect".into(), c.settings.get("kdeconnect").await?));
            let available = c.bluetooth.available().await.unwrap_or(false);
            out.push(Event::Bluetooth {
                available,
                powered: available && c.bluetooth.powered().await.unwrap_or(false),
            });
            if available {
                out.push(Event::Devices(c.bluetooth.devices().await?));
            }
            Ok(out)
        });
        if !self.peer.is_empty() {
            b.load_conversation(self.peer.clone());
        }
    }

    /// Put text on the clipboard: wl-copy on Wayland, xclip on X. A one-time code
    /// is cleared again after a minute, so it does not sit there to be pasted
    /// somewhere by mistake.
    pub fn copy(&mut self, text: &str, secret: bool) {
        match copy_to_clipboard(text) {
            Ok(()) if secret => {
                let t = text.to_string();
                slint::Timer::single_shot(Duration::from_secs(60), move || clear_clipboard_if(&t));
                self.say("Code copied; the clipboard clears in a minute");
            }
            Ok(()) => self.say("Copied"),
            Err(e) => self.say(format!("Could not copy: {e}")),
        }
    }

    pub fn say(&mut self, text: impl Into<String>) {
        self.toast = Some((text.into(), Instant::now()));
        self.render();
    }

    pub fn tick(&mut self) {
        if self.toast.as_ref().is_some_and(|(_, t)| t.elapsed() > Duration::from_secs(4)) {
            self.toast = None;
        }
        if self.contacts_loaded.elapsed() > Duration::from_secs(20) {
            self.book = Book::load(&phone_core::store::contacts_dir(), &self.region);
            self.contacts_loaded = Instant::now();
        }
        self.render();
    }

    pub fn name_for(&self, n: &str) -> String {
        self.book.name_for(n, &self.region)
    }

    // Arguments: links and notification actions.

    pub fn handle_args(&mut self, args: &[String]) {
        let mut it = args.iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--call" | "--recents" => self.ui.set_page(0),
                "--thread" | "--compose" => {
                    if let Some(n) = it.next() {
                        self.open_peer(n);
                    }
                }
                s if s.starts_with("tel:") => {
                    let n = decode(s.trim_start_matches("tel:").split(';').next().unwrap_or(""), false);
                    self.ui.set_page(0);
                    self.ui.set_number(n.into());
                    self.number_typed();
                }
                s if s.starts_with("sms:") => {
                    let rest = s.trim_start_matches("sms:");
                    let (n, query) = rest.split_once('?').unwrap_or((rest, ""));
                    let body = query
                        .split('&')
                        .find_map(|kv| kv.strip_prefix("body="))
                        .map(|v| decode(v, true))
                        .unwrap_or_default();
                    self.open_peer(&decode(n, false));
                    self.ui.set_draft(body.into());
                }
                _ => {}
            }
        }
        self.render();
    }

    fn open_peer(&mut self, raw: &str) {
        self.peer = number::canonical(raw, &self.region);
        self.messages.clear();
        self.ui.set_page(1);
        self.ui.set_thread_selected(
            self.threads.iter().position(|t| number::same_party(&t.peer, &self.peer)).map(|i| i as i32).unwrap_or(-1),
        );
        if let Some(b) = self.bus() {
            b.load_conversation(self.peer.clone());
            let p = self.peer.clone();
            b.run(move |c| async move { c.messages.mark_read(&p).await.map(|_| vec![]) });
        }
    }

    fn number_typed(&mut self) {
        let n = self.ui.get_number().to_string();
        let hint = if n.trim().is_empty() {
            String::new()
        } else if n.starts_with(['*', '#']) && n.ends_with('#') {
            "Service code".into()
        } else {
            match number::classify(&n, &self.region) {
                number::Class::Emergency => "Emergency call".into(),
                number::Class::Premium => "Premium number: charges apply".into(),
                _ => self.book.search(&n).first().map(|c| c.name.clone()).unwrap_or_default(),
            }
        };
        self.ui.set_number_hint(hint.into());
    }

    // Events from phoned.

    pub fn event(&mut self, e: Event) {
        match e {
            Event::Connected(up) => {
                self.connected = up;
                if up {
                    self.reload();
                } else {
                    self.calls.clear();
                }
            }
            Event::Calls(c) => self.calls = c,
            Event::CallAdded(c) | Event::CallChanged(c) => match self.calls.iter_mut().find(|x| x.id == c.id) {
                Some(slot) => *slot = c,
                None => {
                    self.calls.push(c);
                    // A ringing call brings the window forward.
                    self.ui.window().show().ok();
                }
            },
            Event::CallRemoved(id) => {
                self.calls.retain(|c| c.id != id);
                if self.calls.is_empty() {
                    self.ui.set_call_keypad(false);
                }
                if let Some(b) = self.bus() {
                    b.refresh_recents(200);
                }
            }
            Event::Recents(r) => self.recents = r,
            Event::Missed(n) => self.missed = n,
            Event::Threads(t) => self.threads = t,
            Event::Unread(n) => self.unread = n,
            Event::Conversation(p, m) if number::same_party(&p, &self.peer) => self.messages = m,
            Event::MessageAdded(m) => {
                if number::same_party(&m.peer, &self.peer) {
                    // The signal carries no text; fetch the conversation.
                    if let Some(b) = self.bus() {
                        b.load_conversation(self.peer.clone());
                    }
                    if m.direction == 0 && self.ui.get_page() == 1 {
                        if let Some(b) = self.bus() {
                            let p = self.peer.clone();
                            b.run(move |c| async move { c.messages.mark_read(&p).await.map(|_| vec![]) });
                        }
                    }
                }
                if let Some(b) = self.bus() {
                    b.refresh_threads();
                }
            }
            Event::MessageChanged(id, st) => {
                if let Some(m) = self.messages.iter_mut().find(|m| m.id == id) {
                    m.state = st;
                }
                if MessageState::from_u8(st) == MessageState::Failed {
                    self.say("A text was not sent: the network refused it");
                }
            }
            Event::Pending(p) => self.pending = p,
            Event::Requested(r) => {
                if !self.pending.iter().any(|x| x.id == r.id) {
                    self.pending.push(r);
                }
                self.ui.window().show().ok();
            }
            Event::Resolved(id, _) => self.pending.retain(|r| r.id != id),
            Event::Status(s) => self.status = Some(s),
            Event::Routes(r) => {
                if let Some(a) = r.iter().find(|r| r.active) {
                    self.route = a.route;
                }
                self.routes = r;
            }
            Event::Route(r) => self.route = r,
            Event::Ringer(r) => self.ringer = r,
            Event::Setting(k, v) => {
                if k == "region" {
                    self.region = v;
                } else if k == "voicemail" {
                    self.voicemail = v.clone();
                    self.ui.set_voicemail(v.into());
                } else if k == "sip_enabled" {
                    self.ui.set_sip_enabled(v == "true");
                } else if k == "sip_account" {
                    self.ui.set_sip_account(v.into());
                } else if k == "sip_proxy" {
                    self.ui.set_sip_proxy(v.into());
                } else if k == "sip_password" {
                    self.sip_password_set = v == "set";
                } else if k == "kdeconnect" {
                    self.ui.set_kdeconnect(v == "true");
                } else if k == "uplink_alert_to" {
                    self.ui.set_uplink_to(v.into());
                } else if k == "uplink_interface" {
                    self.ui.set_uplink_iface(v.into());
                } else if k == "long_press_ms" {
                    self.hold_ms = v;
                } else if k == "sip_use" {
                    self.sip_always = v == "always";
                } else if let Some(f) = self.flags.iter_mut().find(|(n, _)| *n == k) {
                    f.1 = v == "true";
                }
            }
            Event::Alerts(a) => self.alerts = a,
            Event::Laptops(l) => self.laptops = l,
            Event::Found(q, found) if q == self.ui.get_search_text().trim() => self.found = found,
            Event::ContactsChanged => {
                self.book = Book::load(&phone_core::store::contacts_dir(), &self.region);
            }
            Event::ContactsImported { added, merged, unchanged, error } => {
                self.book = Book::load(&phone_core::store::contacts_dir(), &self.region);
                if error.is_empty() {
                    self.say(format!("Contacts copied: {added} new, {merged} updated, {unchanged} already here"));
                } else {
                    self.say(format!("Contacts not copied: {error}"));
                }
            }
            Event::Grants(g) => self.grants = g,
            Event::Audit(a) => self.audit = a,
            Event::Alert(a) => {
                if a.level != phone_core::model::AlertLevel::Info as u8 {
                    self.alert = Some(a.clone());
                    self.ui.window().show().ok();
                }
                self.alerts.insert(0, a);
            }
            Event::Bluetooth { available, powered } => self.bt = (available, powered),
            Event::Devices(d) => self.devices = d,
            Event::Passkey(_, key) => self.say(format!("Bluetooth pairing code {key:06}: check it matches the device")),
            Event::Blocked(b) => self.blocked = b,
            Event::Placed { kind, placed } if placed.pending != 0 => self.say(format!(
                "{} is waiting for you to allow it",
                if kind == "call" { "The call" } else { "The text" }
            )),
            Event::Ussd(t) | Event::Notice(t) => self.say(t),
            Event::Error(t) => self.say(t),
            _ => {}
        }
        self.render();
    }

    // Drawing.

    pub fn render(&mut self) {
        let ui = &self.ui;
        let now = phone_core::now();
        ui.set_connected(self.connected);
        match &self.status {
            Some(s) if s.present => {
                ui.set_operator(if s.operator.is_empty() { "Searching...".into() } else { s.operator.as_str().into() });
                let bars = (s.quality as usize).div_ceil(25).min(4);
                ui.set_signal(
                    format!(
                        "{} {}{}{}",
                        s.tech,
                        "\u{25AE}".repeat(bars),
                        "\u{25AF}".repeat(4 - bars),
                        if s.roaming { "  roaming" } else { "" }
                    )
                    .into(),
                );
            }
            Some(_) => {
                ui.set_operator("No modem".into());
                ui.set_signal("".into());
            }
            None => {}
        }
        ui.set_unread(self.unread as i32);
        ui.set_missed(self.missed as i32);
        if let Some(t) = &self.tray {
            t.set(crate::tray::Counts { missed: self.missed, unread: self.unread });
        }

        let recents: Vec<Row> = self
            .recents
            .iter()
            .map(|r| {
                let o = Outcome::from_u8(r.outcome);
                let what = match (o, r.direction) {
                    (Outcome::Missed, _) => "Missed".to_string(),
                    (Outcome::Declined, _) => "Declined".into(),
                    (Outcome::Blocked, _) => "Blocked".into(),
                    (Outcome::Silenced, _) => "Unknown caller, silenced".into(),
                    (Outcome::Cancelled | Outcome::Failed, _) => "No answer".into(),
                    (Outcome::Answered, 0) => format!("Incoming, {}", when::duration(r.duration)),
                    (Outcome::Answered, _) => format!("Outgoing, {}", when::duration(r.duration)),
                };
                Row {
                    title: self.name_for(&r.peer).into(),
                    subtitle: what.into(),
                    aside: when::short(r.started, now).into(),
                    mark: match o {
                        Outcome::Missed if !r.seen => 1,
                        Outcome::Blocked => 2,
                        _ => 0,
                    },
                    key: r.peer.as_str().into(),
                }
            })
            .collect();
        ui.set_recents(ModelRc::new(VecModel::from(recents)));

        let threads: Vec<Row> = self
            .threads
            .iter()
            .map(|t| Row {
                title: self.name_for(&t.peer).into(),
                subtitle: format!(
                    "{}{}",
                    if t.last_direction == 1 { "You: " } else { "" },
                    t.last_body.lines().next().unwrap_or("")
                )
                .into(),
                aside: when::short(t.last_at, now).into(),
                mark: if t.unread > 0 { 1 } else { 0 },
                key: t.peer.as_str().into(),
            })
            .collect();
        ui.set_threads(ModelRc::new(VecModel::from(threads)));
        let found: Vec<Row> = self
            .found
            .iter()
            .map(|m| Row {
                title: self.name_for(&m.peer).into(),
                subtitle: format!(
                    "{}{}",
                    if m.direction == 1 { "You: " } else { "" },
                    m.body.lines().next().unwrap_or("")
                )
                .into(),
                aside: when::short(m.at, now).into(),
                mark: 0,
                key: m.peer.as_str().into(),
            })
            .collect();
        ui.set_found(ModelRc::new(VecModel::from(found)));

        if self.peer.is_empty() {
            ui.set_thread_title("".into());
        } else {
            ui.set_thread_title(self.name_for(&self.peer).into());
            ui.set_thread_number(number::display_peer(&self.peer, &self.region).into());
        }
        let bubbles: Vec<Bubble> = self
            .messages
            .iter()
            .map(|m| Bubble {
                text: m.body.as_str().into(),
                time: when::short(m.at, now).into(),
                mine: m.direction == 1,
                state: match MessageState::from_u8(m.state) {
                    MessageState::Sending => "Sending".into(),
                    MessageState::Failed => "Not sent".into(),
                    MessageState::Delivered => "Delivered".into(),
                    _ => SharedString::new(),
                },
                code: if m.direction == 0 {
                    text::one_time_code(&m.body).unwrap_or_default().into()
                } else {
                    "".into()
                },
                // The first picture is shown; anything else is named.
                picture: m
                    .parts
                    .iter()
                    .find(|(mime, _)| mime.starts_with("image/"))
                    .and_then(|(_, path)| slint::Image::load_from_path(std::path::Path::new(path)).ok())
                    .unwrap_or_default(),
                parts: {
                    let first_pic = m.parts.iter().position(|(mime, _)| mime.starts_with("image/"));
                    m.parts
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| Some(*i) != first_pic)
                        .map(|(_, (mime, _))| format!("[{}]", phone_core::model::kind_of(mime)))
                        .collect::<Vec<_>>()
                        .join(" ")
                        .into()
                },
            })
            .collect();
        ui.set_bubbles(ModelRc::new(VecModel::from(bubbles)));
        let draft = ui.get_draft().to_string();
        ui.set_draft_cost(if draft.is_empty() {
            "".into()
        } else {
            let s = text::segments(&draft);
            if s.count == 1 { format!("{} left", s.remaining) } else { format!("{} texts", s.count) }.into()
        });

        let filter = ui.get_contact_filter().to_string();
        self.contacts_view = if filter.trim().is_empty() {
            (0..self.book.contacts.len()).collect()
        } else {
            let hits = self.book.search(&filter);
            (0..self.book.contacts.len())
                .filter(|i| hits.iter().any(|h| std::ptr::eq(*h, &self.book.contacts[*i])))
                .collect()
        };
        let contacts: Vec<Row> = self
            .contacts_view
            .iter()
            .map(|&i| {
                let c = &self.book.contacts[i];
                Row {
                    title: c.name.as_str().into(),
                    subtitle: c
                        .numbers
                        .iter()
                        .map(|p| format!("{} {}", p.kind, number::display(&p.number, &self.region)))
                        .collect::<Vec<_>>()
                        .join("   ")
                        .into(),
                    aside: SharedString::new(),
                    mark: 0,
                    key: c.numbers.first().map(|p| p.number.as_str()).unwrap_or("").into(),
                }
            })
            .collect();
        ui.set_contacts(ModelRc::new(VecModel::from(contacts)));
        let phones: Vec<Row> = self
            .devices
            .iter()
            .filter(|d| d.paired && !d.headset && !d.speaker)
            .map(|d| Row {
                title: d.name.as_str().into(),
                subtitle: SharedString::new(),
                aside: SharedString::new(),
                mark: 0,
                key: d.address.as_str().into(),
            })
            .collect();
        ui.set_phones(ModelRc::new(VecModel::from(phones)));

        // Settings.
        // The routes phoned offers, in its order (an earpiece only on phones).
        let offered: Vec<Route> = if self.routes.is_empty() {
            vec![Route::Speaker, Route::Wired, Route::Bluetooth]
        } else {
            self.routes.iter().map(|r| Route::from_u8(r.route)).collect()
        };
        let choices: Vec<Choice> = offered
            .iter()
            .map(|r| {
                let info = self.routes.iter().find(|x| x.route == *r as u8);
                let label = match info {
                    Some(i) if !i.available => format!("{} (not connected)", i.name),
                    Some(i) => i.name.clone(),
                    None => r.label().to_string(),
                };
                Choice { label: label.into(), value: (*r as u8).to_string().into() }
            })
            .collect();
        let names: Vec<SharedString> = choices.iter().map(|c| c.label.clone()).collect();
        ui.set_route_names(ModelRc::new(VecModel::from(names)));
        ui.set_route_index(offered.iter().position(|r| *r as u8 == self.route).unwrap_or(0) as i32);
        ui.set_routes(ModelRc::new(VecModel::from(choices)));
        ui.set_ringer_index(match self.ringer.as_str() {
            "vibrate" => 1,
            "silent" => 2,
            _ => 0,
        });
        let toggles = |group: &[Flag]| {
            let t: Vec<Toggle> = group
                .iter()
                .map(|((k, d), label)| Toggle {
                    key: (*k).into(),
                    label: (*label).into(),
                    on: self.flags.iter().find(|(n, _)| n == k).map(|f| f.1).unwrap_or(*d == "true"),
                })
                .collect();
            ModelRc::new(VecModel::from(t))
        };
        ui.set_call_toggles(toggles(&CALL_FLAGS));
        ui.set_text_toggles(toggles(&TEXT_FLAGS));
        ui.set_alert_toggles(toggles(&ALERT_FLAGS));
        let grants: Vec<Row> = self
            .grants
            .iter()
            .map(|g| Row {
                title: g.program.as_str().into(),
                subtitle: format!(
                    "{} - {} an hour, {}",
                    g.numbers.split(',').map(|n| number::display(n, &self.region)).collect::<Vec<_>>().join(", "),
                    g.per_hour,
                    if g.until == 0 { "no end".to_string() } else { format!("until {}", when::short(g.until, now)) }
                )
                .into(),
                aside: SharedString::new(),
                mark: 0,
                key: g.id.to_string().into(),
            })
            .collect();
        ui.set_grants(ModelRc::new(VecModel::from(grants)));
        let laptops: Vec<Row> = self
            .laptops
            .iter()
            .map(|(id, name, paired)| Row {
                title: name.as_str().into(),
                subtitle: if *paired {
                    "Paired".into()
                } else {
                    "Not paired: pair from the laptop, then allow it here".into()
                },
                aside: if *paired { "Unpair".into() } else { SharedString::new() },
                mark: 0,
                key: id.as_str().into(),
            })
            .collect();
        ui.set_laptops(ModelRc::new(VecModel::from(laptops)));
        let audit: Vec<Row> = self
            .audit
            .iter()
            .map(|e| Row {
                title: format!("{} {} {}", e.program, e.action, e.target).trim_end().to_string().into(),
                subtitle: e.result.as_str().into(),
                aside: when::short(e.at, now).into(),
                mark: if matches!(e.result.as_str(), "denied" | "refused" | "over its grant's limit") { 2 } else { 0 },
                key: SharedString::new(),
            })
            .collect();
        ui.set_audit(ModelRc::new(VecModel::from(audit)));
        let alerts: Vec<Row> = self
            .alerts
            .iter()
            .map(|a| Row {
                title: phone_core::model::AlertLevel::from_u8(a.level).label().into(),
                subtitle: a.text.as_str().into(),
                aside: when::short(a.at, now).into(),
                mark: 2,
                key: a.id.to_string().into(),
            })
            .collect();
        ui.set_alerts(ModelRc::new(VecModel::from(alerts)));
        ui.set_sip_password_set(self.sip_password_set);
        ui.set_hold_index(HOLDS.iter().position(|h| *h == self.hold_ms).unwrap_or(1) as i32);
        ui.set_sip_always(self.sip_always);
        ui.set_sip_status(self.status.as_ref().map(|s| s.sip.as_str()).unwrap_or("").into());
        match &self.alert {
            Some(a) => {
                ui.set_alert_visible(true);
                ui.set_alert_title(phone_core::model::AlertLevel::from_u8(a.level).label().to_uppercase().into());
                ui.set_alert_text(a.text.as_str().into());
                ui.set_alert_time(when::short(a.at, now).into());
            }
            None => ui.set_alert_visible(false),
        }
        ui.set_bt_available(self.bt.0);
        ui.set_bt_powered(self.bt.1);
        let devices: Vec<Row> = self
            .devices
            .iter()
            .map(|d| Row {
                title: d.name.as_str().into(),
                subtitle: format!(
                    "{}{}",
                    if d.headset {
                        "Headset"
                    } else if d.speaker {
                        "Speaker"
                    } else {
                        "Device"
                    },
                    if d.connected {
                        ", connected"
                    } else if d.paired {
                        ", paired"
                    } else {
                        ""
                    }
                )
                .into(),
                aside: if !d.paired {
                    "Pair"
                } else if d.connected {
                    "Disconnect"
                } else {
                    "Connect"
                }
                .into(),
                mark: 0,
                key: d.address.as_str().into(),
            })
            .collect();
        ui.set_bt_devices(ModelRc::new(VecModel::from(devices)));
        let blocked: Vec<Row> = self
            .blocked
            .iter()
            .map(|n| {
                let shown = number::display(n, &self.region);
                let name = self.name_for(n);
                Row {
                    subtitle: if name == shown { SharedString::new() } else { name.into() },
                    title: shown.into(),
                    aside: SharedString::new(),
                    mark: 2,
                    key: n.as_str().into(),
                }
            })
            .collect();
        ui.set_blocked(ModelRc::new(VecModel::from(blocked)));
        ui.set_modem_text(
            match &self.status {
                Some(s) if s.present => format!(
                    "{} on {}, signal {}%, {}{}. Voice calls {}. Number {}.",
                    if s.operator.is_empty() { "No operator" } else { &s.operator },
                    s.tech,
                    s.quality,
                    if s.registered { "registered" } else { "not registered" },
                    if s.roaming { " (roaming)" } else { "" },
                    if s.voice { "supported" } else { "not supported" },
                    if s.own_number.is_empty() {
                        "unknown".to_string()
                    } else {
                        number::display(&s.own_number, &self.region)
                    }
                ),
                _ => "No modem. Fit one in the M.2 slot.".into(),
            }
            .into(),
        );

        // The call card.
        let ringing = self.calls.iter().find(|c| CallState::from_u8(c.state).ringing_in());
        let current = self.calls.iter().find(|c| CallState::from_u8(c.state) == CallState::Active).or_else(|| {
            self.calls.iter().find(|c| CallState::from_u8(c.state).live() && !CallState::from_u8(c.state).ringing_in())
        });
        let shown = ringing.or(current);
        ui.set_call_visible(shown.is_some());
        if let Some(c) = shown {
            let name = if c.number.is_empty() { "Withheld".to_string() } else { self.name_for(&c.number) };
            let shown_number = number::display(&c.number, &self.region);
            ui.set_call_ringing(ringing.is_some());
            ui.set_call_waiting(CallState::from_u8(c.state) == CallState::Waiting);
            ui.set_call_detail(if name == shown_number { SharedString::new() } else { shown_number.into() });
            ui.set_call_name(name.into());
            ui.set_call_muted(c.muted);
            ui.set_call_timer(match CallState::from_u8(c.state) {
                CallState::Active if c.connected_at > 0 => when::duration(now - c.connected_at).into(),
                CallState::Held => "On hold".into(),
                CallState::RingingOut => "Ringing...".into(),
                _ => SharedString::new(),
            });
            let other = self.calls.iter().find(|x| {
                x.id != c.id && CallState::from_u8(x.state).live() && !CallState::from_u8(x.state).ringing_in()
            });
            ui.set_call_two(other.is_some());
            ui.set_call_held_name(other.map(|o| self.name_for(&o.number)).unwrap_or_default().into());
        }

        match self.pending.first() {
            Some(r) => {
                ui.set_confirm_visible(true);
                let target = self.name_for(&r.target);
                ui.set_confirm_title(
                    match r.kind.as_str() {
                        "call" => format!("{} wants to call {target}", r.requester),
                        "sms" => format!("{} wants to text {target}", r.requester),
                        "wipe" => "Delete all texts, calls and blocked numbers?".into(),
                        "modem" => format!("{} wants to set up the modem (USB audio, VoLTE)", r.requester),
                        "grant" => format!("{} wants to text {target} without asking each time", r.requester),
                        "contact" => format!("{} wants to {} in your contacts", r.requester, r.target),
                        "setting" => format!("{} wants to set {} to {}", r.requester, r.target, r.body),
                        _ => format!("{} wants to use the phone", r.requester),
                    }
                    .into(),
                );
                // Only a text's body is for reading; others carry the change itself.
                ui.set_confirm_body(if r.kind == "sms" { r.body.as_str().into() } else { SharedString::new() });
                ui.set_confirm_reason(
                    match r.reason.as_str() {
                        "premium" => "Premium number: charges apply",
                        "short" => "Short code: may cost money",
                        "emergency" => "This is an emergency number",
                        "wipe" => "This cannot be undone",
                        "modem" => "Saved in the modem, which restarts",
                        "grant" => "You can revoke it in Settings",
                        "contact" => "A contact decides who a caller shows as",
                        "setting" => "Change it back in Settings",
                        _ => "Asked by another program",
                    }
                    .into(),
                );
            }
            None => ui.set_confirm_visible(false),
        }
        ui.set_toast(self.toast.as_ref().map(|(t, _)| t.as_str()).unwrap_or("").into());
    }
}

fn clipboard_tool() -> (&'static str, &'static [&'static str], &'static [&'static str]) {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        ("wl-copy", &[], &["--clear"])
    } else {
        ("xclip", &["-selection", "clipboard"], &[])
    }
}

fn copy_to_clipboard(text: &str) -> Result<(), String> {
    use std::io::Write;
    let (tool, args, _) = clipboard_tool();
    let mut child = std::process::Command::new(tool)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|_| format!("{tool} is not installed"))?;
    child.stdin.take().ok_or("no stdin")?.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    child.wait().map_err(|e| e.to_string())?;
    Ok(())
}

/// Clear the clipboard, but only if it still holds what we put there.
fn clear_clipboard_if(expected: &str) {
    let (tool, _, clear) = clipboard_tool();
    let current = if tool == "wl-copy" {
        std::process::Command::new("wl-paste").arg("--no-newline").output()
    } else {
        std::process::Command::new("xclip").args(["-selection", "clipboard", "-o"]).output()
    };
    if current.ok().is_some_and(|o| o.stdout == expected.as_bytes()) {
        if tool == "wl-copy" {
            let _ = std::process::Command::new(tool).args(clear).status();
        } else {
            let _ = copy_to_clipboard("");
        }
    }
}

/// Where an export goes: the Documents folder (XDG's, or ~/Documents), dated.
fn export_path() -> std::path::PathBuf {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let docs = std::process::Command::new("xdg-user-dir")
        .arg("DOCUMENTS")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|d| !d.is_empty() && std::path::Path::new(d).is_dir())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| home.join("Documents"));
    let _ = std::fs::create_dir_all(&docs);
    let t = when::local(phone_core::now());
    docs.join(format!("dolphone-texts-{}-{:02}-{:02}.xml", t.year, t.month + 1, t.day))
}

/// Percent-decoding for tel: and sms: links. `+` is a space only in a query value;
/// in a number it is the international prefix.
fn decode(s: &str, plus_is_space: bool) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if b[i] == b'+' && plus_is_space { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Connect the window's callbacks to the app.
pub fn wire(ui: &AppWindow) {
    ui.on_navigate(|p| {
        with_app(|a| {
            a.ui.set_page(p);
            if let Some(b) = a.bus() {
                match p {
                    0 => b.run(|c| async move { c.calls.mark_calls_seen().await.map(|_| vec![]) }),
                    1 => b.refresh_threads(),
                    _ => {}
                }
            }
            a.render();
        })
    });
    ui.on_number_typed(|_| with_app(|a| a.number_typed()));
    ui.on_dial(|n| {
        with_app(|a| {
            let n = n.trim().to_string();
            if n.is_empty() {
                return;
            }
            let Some(b) = a.bus() else { return };
            if (n.starts_with('*') || n.starts_with('#')) && n.ends_with('#') {
                b.run(move |c| async move { Ok(vec![Event::Ussd(c.modem.ussd(&n).await?)]) });
            } else {
                b.dial(n);
            }
            a.ui.set_number("".into());
            a.number_typed();
        })
    });
    ui.on_recent_call(|i| {
        with_app(|a| {
            if let (Some(r), Some(b)) = (a.recents.get(i as usize), a.bus()) {
                b.dial(r.peer.clone());
            }
        })
    });
    ui.on_recent_text(|i| {
        with_app(|a| {
            if let Some(p) = a.recents.get(i as usize).map(|r| r.peer.clone()) {
                a.open_peer(&p);
                a.render();
            }
        })
    });
    ui.on_open_thread(|i| {
        with_app(|a| {
            if let Some(p) = a.threads.get(i as usize).map(|t| t.peer.clone()) {
                a.open_peer(&p);
                a.render();
            }
        })
    });
    ui.on_search(|q| {
        with_app(|a| {
            let q = q.trim().to_string();
            if q.is_empty() {
                a.found.clear();
                a.render();
                return;
            }
            if let Some(b) = a.bus() {
                b.run(move |c| async move { Ok(vec![Event::Found(q.clone(), c.messages.search(&q, 100).await?)]) });
            }
        })
    });
    ui.on_open_found(|i| {
        with_app(|a| {
            if let Some(p) = a.found.get(i as usize).map(|m| m.peer.clone()) {
                a.ui.set_search_text("".into());
                a.found.clear();
                a.open_peer(&p);
                a.render();
            }
        })
    });
    ui.on_export_texts(|| {
        with_app(|a| {
            if let Some(b) = a.bus() {
                b.run(|c| async move {
                    let xml = c.messages.export().await?;
                    let path = export_path();
                    let msg = match std::fs::write(&path, xml) {
                        Ok(()) => format!("Texts saved to {}", path.display()),
                        Err(e) => format!("Could not save the texts: {e}"),
                    };
                    Ok(vec![Event::Notice(msg)])
                });
            }
        })
    });
    ui.on_new_message(|_| {
        with_app(|a| {
            let n = a.ui.get_number().to_string();
            if number::digits(&n).len() >= 3 {
                a.open_peer(&n);
                a.ui.set_number("".into());
                a.render();
            } else {
                a.say("Type the number on the Calls dial pad, then press New");
            }
        })
    });
    ui.on_draft_edited(|_| with_app(|a| a.render()));
    ui.on_send(|t| {
        with_app(|a| {
            let body = t.trim().to_string();
            if body.is_empty() || a.peer.is_empty() {
                return;
            }
            if let Some(b) = a.bus() {
                b.send(a.peer.clone(), body);
            }
            a.ui.set_draft("".into());
            a.render();
        })
    });
    ui.on_thread_call(|| {
        with_app(|a| {
            if let Some(b) = a.bus() {
                b.dial(a.peer.clone());
            }
        })
    });
    ui.on_thread_block(|| {
        with_app(|a| {
            let p = a.peer.clone();
            if let Some(b) = a.bus() {
                b.run(move |c| async move {
                    c.settings.block(&p).await?;
                    Ok(vec![Event::Blocked(c.settings.blocked().await?)])
                });
            }
            a.say("Blocked: calls are rejected and texts dropped");
        })
    });
    ui.on_thread_delete(|| {
        with_app(|a| {
            let p = std::mem::take(&mut a.peer);
            a.messages.clear();
            if let Some(b) = a.bus() {
                b.run(move |c| async move {
                    c.messages.delete_thread(&p).await?;
                    Ok(vec![Event::Threads(c.messages.threads().await?)])
                });
            }
            a.render();
        })
    });
    ui.on_contacts_filtered(|_| with_app(|a| a.render()));
    ui.on_contact_call(|i| {
        with_app(|a| {
            let n = a
                .contacts_view
                .get(i as usize)
                .and_then(|&j| a.book.contacts[j].numbers.first())
                .map(|p| p.number.clone());
            if let (Some(n), Some(b)) = (n, a.bus()) {
                b.dial(n);
            }
        })
    });
    ui.on_contact_text(|i| {
        with_app(|a| {
            let n = a
                .contacts_view
                .get(i as usize)
                .and_then(|&j| a.book.contacts[j].numbers.first())
                .map(|p| p.number.clone());
            if let Some(n) = n {
                a.open_peer(&n);
                a.render();
            }
        })
    });
    ui.on_answer(|| {
        with_app(|a| {
            if let (Some(id), Some(b)) = (a.ringing_id(), a.bus()) {
                b.run(move |c| async move { c.calls.answer(id).await.map(|_| vec![]) });
            }
        })
    });
    ui.on_decline(|| {
        with_app(|a| {
            if let (Some(id), Some(b)) = (a.ringing_id(), a.bus()) {
                b.run(move |c| async move { c.calls.hangup(id).await.map(|_| vec![]) });
            }
        })
    });
    ui.on_hangup(|| {
        with_app(|a| {
            if let (Some(id), Some(b)) = (a.current_id(), a.bus()) {
                b.run(move |c| async move { c.calls.hangup(id).await.map(|_| vec![]) });
            }
        })
    });
    ui.on_mute(|m| {
        with_app(|a| {
            if let Some(b) = a.bus() {
                b.run(move |c| async move { c.calls.set_muted(m).await.map(|_| vec![]) });
            }
        })
    });
    ui.on_swap(|| {
        with_app(|a| {
            if let Some(b) = a.bus() {
                b.run(|c| async move { c.calls.swap().await.map(|_| vec![]) });
            }
        })
    });
    ui.on_route(|i| {
        with_app(|a| {
            if let Some(b) = a.bus() {
                // The menu lists phoned's routes in its order; pick by that.
                let Some(r) = a.routes.get(i.max(0) as usize).map(|r| r.route) else { return };
                b.run(move |c| async move {
                    c.audio.set_route(r).await?;
                    Ok(vec![Event::Routes(c.audio.routes().await?)])
                });
            }
        })
    });
    ui.on_dtmf(|d| {
        with_app(|a| {
            let id = a.calls.iter().find(|c| CallState::from_u8(c.state) == CallState::Active).map(|c| c.id);
            if let (Some(id), Some(b)) = (id, a.bus()) {
                let d = d.to_string();
                b.run(move |c| async move { c.calls.send_dtmf(id, &d).await.map(|_| vec![]) });
            }
        })
    });
    ui.on_reply(|t| {
        with_app(|a| {
            if let (Some(c), Some(b)) =
                (a.calls.iter().find(|c| CallState::from_u8(c.state).ringing_in()).cloned(), a.bus())
            {
                b.send(c.number.clone(), t.to_string());
                let id = c.id;
                b.run(move |c| async move { c.calls.hangup(id).await.map(|_| vec![]) });
            }
        })
    });
    ui.on_allow(|| {
        with_app(|a| {
            if let (Some(r), Some(b)) = (a.pending.first().cloned(), a.bus()) {
                b.run(move |c| async move { c.confirm.confirm(r.id).await.map(|_| vec![]) });
            }
        })
    });
    ui.on_deny(|| {
        with_app(|a| {
            if let (Some(r), Some(b)) = (a.pending.first().cloned(), a.bus()) {
                b.run(move |c| async move { c.confirm.deny(r.id).await.map(|_| vec![]) });
            }
        })
    });
    ui.on_set_ringer(|i| {
        with_app(|a| {
            let mode = ["ring", "vibrate", "silent"][i.clamp(0, 2) as usize].to_string();
            a.ringer = mode.clone();
            if let Some(b) = a.bus() {
                b.run(move |c| async move { c.audio.set_ringer(&mode).await.map(|_| vec![]) });
            }
        })
    });
    ui.on_set_flag(|k, on| {
        with_app(|a| {
            let k = k.to_string();
            if let Some(f) = a.flags.iter_mut().find(|(n, _)| *n == k) {
                f.1 = on;
            }
            if let Some(b) = a.bus() {
                b.run(move |c| async move { c.settings.set(&k, &on.to_string()).await.map(|_| vec![]) });
            }
        })
    });
    ui.on_bt_power(|on| {
        with_app(|a| {
            if let Some(b) = a.bus() {
                b.run(move |c| async move {
                    c.bluetooth.set_powered(on).await?;
                    Ok(vec![Event::Bluetooth { available: true, powered: on }])
                });
            }
        })
    });
    ui.on_bt_scan(|| {
        with_app(|a| {
            if let Some(b) = a.bus() {
                b.run(|c| async move { c.bluetooth.start_scan().await.map(|_| vec![]) });
            }
            // The scan runs for 30 s in phoned; read the list back as it fills.
            for k in 1..=6u64 {
                slint::Timer::single_shot(Duration::from_secs(5 * k), || {
                    with_app(|a| {
                        if let Some(b) = a.bus() {
                            b.run(|c| async move { Ok(vec![Event::Devices(c.bluetooth.devices().await?)]) });
                        }
                    })
                });
            }
            a.say("Scanning for 30 seconds: put the headset in pairing mode");
        })
    });
    ui.on_bt_action(|i| {
        with_app(|a| {
            let Some(d) = a.devices.get(i as usize).cloned() else { return };
            if let Some(b) = a.bus() {
                b.run(move |c| async move {
                    if !d.paired {
                        c.bluetooth.pair(&d.address).await?;
                    } else if d.connected {
                        c.bluetooth.disconnect(&d.address).await?;
                    } else {
                        c.bluetooth.connect(&d.address).await?;
                    }
                    Ok(vec![Event::Devices(c.bluetooth.devices().await?)])
                });
            }
        })
    });
    ui.on_unblock(|i| {
        with_app(|a| {
            if let (Some(n), Some(b)) = (a.blocked.get(i as usize).cloned(), a.bus()) {
                b.run(move |c| async move {
                    c.settings.unblock(&n).await?;
                    Ok(vec![Event::Blocked(c.settings.blocked().await?)])
                });
            }
        })
    });
    ui.on_copy(|t, secret| with_app(|a| a.copy(&t, secret)));
    ui.on_set_voicemail(|v| {
        with_app(|a| {
            let n: String = v.chars().filter(|c| c.is_ascii_digit() || *c == '+').collect();
            a.voicemail = n.clone();
            a.ui.set_voicemail(n.as_str().into());
            if let Some(b) = a.bus() {
                b.run(move |c| async move { c.settings.set("voicemail", &n).await.map(|_| vec![]) });
            }
            a.say("Voicemail number saved");
        })
    });
    ui.on_save_uplink(|to, iface| {
        with_app(|a| {
            let (to, iface) = (to.trim().to_string(), iface.trim().to_string());
            if let Some(b) = a.bus() {
                b.run(move |c| async move {
                    c.settings.set("uplink_alert_to", &to).await?;
                    c.settings.set("uplink_interface", &iface).await?;
                    Ok(vec![Event::Notice("Uplink watch saved".into())])
                });
            }
        })
    });
    ui.on_set_hold(|i| {
        with_app(|a| {
            let ms = HOLDS[i.clamp(0, 4) as usize].to_string();
            a.hold_ms = ms.clone();
            if let Some(b) = a.bus() {
                b.run(move |c| async move { c.settings.set("long_press_ms", &ms).await.map(|_| vec![]) });
            }
        })
    });
    ui.on_save_sip(|account, password, proxy, always| {
        with_app(|a| {
            let (account, password, proxy) =
                (account.trim().to_string(), password.to_string(), proxy.trim().to_string());
            a.sip_always = always;
            a.ui.set_sip_password("".into());
            if !password.is_empty() {
                a.sip_password_set = true;
            }
            if let Some(b) = a.bus() {
                b.run(move |c| async move {
                    if !password.is_empty() {
                        c.settings.set("sip_password", &password).await?;
                    }
                    c.settings.set("sip_proxy", &proxy).await?;
                    c.settings.set("sip_use", if always { "always" } else { "fallback" }).await?;
                    c.settings.set("sip_account", &account).await?;
                    Ok(vec![Event::Notice("SIP account saved".into())])
                });
            }
        })
    });
    ui.on_unpair_laptop(|i| {
        with_app(|a| {
            if let (Some((id, _, true)), Some(b)) = (a.laptops.get(i as usize).cloned(), a.bus()) {
                b.run(move |c| async move {
                    c.settings.kdeconnect_unpair(&id).await?;
                    Ok(vec![Event::Laptops(c.settings.kdeconnect_devices().await?)])
                });
            }
        })
    });
    ui.on_revoke(|i| {
        with_app(|a| {
            if let (Some(g), Some(b)) = (a.grants.get(i as usize).cloned(), a.bus()) {
                b.run(move |c| async move {
                    c.settings.revoke_grant(g.id).await?;
                    Ok(vec![Event::Grants(c.settings.grants().await?)])
                });
            }
        })
    });
    ui.on_add_contact(|name, num| {
        with_app(|a| {
            let (name, num) = (name.to_string(), num.to_string());
            if let Some(b) = a.bus() {
                b.run(move |c| async move {
                    c.contacts.add(&name, vec![("mobile".into(), num)]).await?;
                    Ok(vec![Event::ContactsChanged])
                });
            }
            a.ui.set_new_contact_name("".into());
            a.ui.set_new_contact_number("".into());
        })
    });
    ui.on_remove_contact(|i| {
        with_app(|a| {
            let Some(&idx) = a.contacts_view.get(i as usize) else { return };
            let id = a.book.contacts[idx].id();
            if let Some(b) = a.bus() {
                b.run(move |c| async move {
                    c.contacts.remove(&id).await?;
                    Ok(vec![Event::ContactsChanged])
                });
            }
            a.ui.set_contact_selected(-1);
        })
    });
    ui.on_import_from_phone(|i| {
        with_app(|a| {
            let phones: Vec<_> = a.devices.iter().filter(|d| d.paired && !d.headset && !d.speaker).collect();
            let Some(d) = phones.get(i as usize) else { return };
            let (addr, name) = (d.address.clone(), d.name.clone());
            if let Some(b) = a.bus() {
                b.run(move |c| async move { c.contacts.import_from_phone(&addr).await.map(|_| vec![]) });
            }
            a.say(format!("Copying contacts from {name}: allow it on the phone"));
        })
    });
    ui.on_dismiss_alert(|| {
        with_app(|a| {
            a.alert = None;
            a.render();
        })
    });
    ui.on_wipe(|| {
        with_app(|a| {
            // phoned asks first, with a confirmation that shows here (and on the panel).
            if let Some(b) = a.bus() {
                b.run(|c| async move { Ok(vec![Event::Placed { kind: "wipe", placed: c.settings.wipe().await? }]) });
            }
        })
    });
}

// Sample data for the screenshot tests.

#[cfg(test)]
pub fn sample(a: &mut App, page: i32) {
    a.connected = true;
    a.book = Book {
        contacts: phone_core::contacts::parse(
            "BEGIN:VCARD\nFN:Alex Example\nTEL;TYPE=CELL:0491570156\nEND:VCARD\nBEGIN:VCARD\nFN:Sam Sample\nTEL;TYPE=HOME:0491570157\nEND:VCARD\nBEGIN:VCARD\nFN:Jordan Doe\nTEL;TYPE=WORK:+61293744000\nEND:VCARD\n",
            "AU",
        ),
    };
    a.status = Some(ModemStatus {
        present: true,
        registered: true,
        roaming: false,
        operator: "Fakecom AU".into(),
        quality: 72,
        tech: "LTE".into(),
        voice: true,
        own_number: "+61491570110".into(),
        emergency_only: false,
        state: "ready".into(),
        sip: String::new(),
    });
    let now = phone_core::now();
    a.recents = vec![
        CallRecordInfo {
            id: 3,
            peer: "+61491570156".into(),
            direction: 0,
            started: now - 600,
            duration: 0,
            outcome: 1,
            seen: false,
            sim: String::new(),
        },
        CallRecordInfo {
            id: 2,
            peer: "+61491570199".into(),
            direction: 1,
            started: now - 90_000,
            duration: 125,
            outcome: 0,
            seen: true,
            sim: String::new(),
        },
        CallRecordInfo {
            id: 1,
            peer: "+61293744000".into(),
            direction: 0,
            started: now - 400_000,
            duration: 40,
            outcome: 0,
            seen: true,
            sim: String::new(),
        },
    ];
    a.missed = 1;
    a.threads = vec![
        ThreadInfo {
            peer: "+61491570156".into(),
            last_body: "Running 10 min late, sorry!".into(),
            last_at: now - 300,
            last_direction: 0,
            unread: 2,
        },
        ThreadInfo {
            peer: "Telstra".into(),
            last_body: "Your bill is ready".into(),
            last_at: now - 200_000,
            last_direction: 0,
            unread: 0,
        },
    ];
    a.unread = 2;
    a.routes = vec![
        RouteInfo { route: 0, name: "Speaker".into(), available: true, active: false },
        RouteInfo { route: 1, name: "Headset".into(), available: false, active: false },
        RouteInfo { route: 2, name: "Pixel Buds".into(), available: true, active: true },
    ];
    a.route = 2;
    a.bt = (true, true);
    a.devices = vec![
        BtDevice {
            address: "AA".into(),
            name: "Pixel Buds".into(),
            paired: true,
            connected: true,
            headset: true,
            speaker: true,
        },
        BtDevice {
            address: "BB".into(),
            name: "JBL Flip 6".into(),
            paired: false,
            connected: false,
            headset: false,
            speaker: true,
        },
    ];
    a.blocked = vec!["+61400000000".into()];
    if page == 1 {
        a.peer = "+61491570156".into();
        a.messages = vec![
            MessageInfo {
                id: 1,
                peer: a.peer.clone(),
                direction: 0,
                body: "Hey, are we still on for lunch tomorrow at the usual place?".into(),
                at: now - 3000,
                state: 0,
                read: true,
                sim: String::new(),
                parts: Vec::new(),
            },
            MessageInfo {
                id: 2,
                peer: a.peer.clone(),
                direction: 1,
                body: "Yes! 12:30 works.".into(),
                at: now - 2900,
                state: 2,
                read: true,
                sim: String::new(),
                parts: Vec::new(),
            },
            MessageInfo {
                id: 3,
                peer: a.peer.clone(),
                direction: 0,
                body: "Running 10 min late, sorry!".into(),
                at: now - 300,
                state: 0,
                read: true,
                sim: String::new(),
                parts: Vec::new(),
            },
        ];
        a.ui.set_thread_selected(0);
        a.ui.set_draft("No worries, see you soon".into());
    }
    a.ui.set_page(page);
    if page == 0 {
        a.ui.set_number("0491 570 1".into());
        a.number_typed();
        a.ui.set_recent_selected(0);
    }
}

#[cfg(test)]
pub fn sample_call(a: &mut App, ringing: bool) {
    let now = phone_core::now();
    a.calls = vec![CallInfo {
        id: 1,
        number: "+61491570156".into(),
        direction: 0,
        state: if ringing { CallState::RingingIn as u8 } else { CallState::Active as u8 },
        connected_at: if ringing { 0 } else { now - 83 },
        muted: false,
        emergency: false,
        internet: false,
    }];
}

#[cfg(test)]
pub fn sample_confirm(a: &mut App) {
    a.pending = vec![Request {
        id: 1,
        kind: "sms".into(),
        target: "1900123456".into(),
        body: "WIN a prize, reply YES".into(),
        requester: "python3".into(),
        reason: "premium".into(),
    }];
}
