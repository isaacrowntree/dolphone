//! Phone Settings, as data: the pages, their rows, and what each key does.
//!
//! Rows follow FlipCTL's settings conventions: an ON/OFF row toggles with Left,
//! Right or Run; an adjustable row ("< Ring >") steps with Left and Right; a row
//! with a chevron opens a page with Run or Ok. Escape goes up a page.

use std::collections::HashMap;

use phone_api::{keys, AlertInfo, AuditEntry, BtDevice, CellInfo, Grant, ModemStatus, RouteInfo};
use phone_bus::CellReport;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    Up,
    Down,
    Left,
    Right,
    Ok,
    Escape,
    View,
    Edit,
    Run,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Main,
    Audio,
    Bluetooth,
    Blocked,
    Modem,
    About,
    Calls,
    Texts,
    Alerts,
    Privacy,
    Grants,
    Audit,
    /// Calls over the internet.
    Sip,
    /// Laptops, over KDE Connect.
    Laptops,
    /// The cells the modem hears, from the Modem page: a field test.
    Network,
}

pub const PAGES: usize = 15;

/// Rows a detail screen shows below its breadcrumb (flipctl's
/// detail_visible_rows; a test in main.rs holds the two together).
pub const DETAIL_ROWS: usize = 8;

/// A line of the Network screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetRow {
    /// A cell's heading, in the bold face: "LTE serving", and its network.
    Head(String, String),
    Value(String, String),
    Divider,
    Text(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    SetRinger(String),
    Set(&'static str, String),
    SetRoute(u8),
    BtPower(bool),
    BtScan,
    BtPair(String),
    BtConnect(String, bool),
    BtForget(String),
    LaptopUnpair(String),
    /// Copy a paired phone's contacts over Bluetooth.
    ImportContacts(String),
    Unblock(String),
    Revoke(i64),
    /// Open the keyboard for a typed setting.
    EditText(Field),
    Wipe,
    /// Make the modem's one-time setup (it restarts).
    ApplyModemSetup,
    /// Turn on ModemManager's debug mode, which the setup needs (a password).
    ModemAccess,
    Refresh(Page),
    Quit,
}

/// A setting typed on the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Voicemail,
    SipAccount,
    SipPassword,
    SipProxy,
    UplinkTo,
    UplinkIface,
    /// The Messages PIN, new, and then again to be sure of it.
    Pin,
    PinAgain,
}

impl Field {
    pub fn key(self) -> &'static str {
        match self {
            Self::Voicemail => keys::VOICEMAIL.0,
            Self::SipAccount => keys::SIP_ACCOUNT.0,
            Self::SipPassword => keys::SIP_PASSWORD.0,
            Self::SipProxy => keys::SIP_PROXY.0,
            Self::UplinkTo => keys::UPLINK_ALERT_TO.0,
            Self::UplinkIface => keys::UPLINK_INTERFACE.0,
            Self::Pin | Self::PinAgain => keys::MESSAGES_PIN.0,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Voicemail => "Voicemail number",
            Self::SipAccount => "SIP address (sip:you@provider)",
            Self::SipPassword => "SIP password",
            Self::SipProxy => "Proxy (sip:host), or empty",
            Self::UplinkTo => "Text this number when the link drops",
            Self::UplinkIface => "Interface to watch (eth0), or empty",
            Self::Pin => "New Messages PIN, 4 to 12 digits",
            Self::PinAgain => "The new PIN again",
        }
    }

    /// Typed but never shown: the keyboard stars it, and takes only digits.
    pub fn is_pin(self) -> bool {
        matches!(self, Self::Pin | Self::PinAgain)
    }
}

/// What the PIN keyboard lets through: 4 to 12 digits, as phoned takes.
pub fn valid_pin(t: &str) -> bool {
    (4..=12).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_digit())
}

/// How long Messages stays open after its PIN, in seconds: 1, 5 or 15 minutes.
pub const OPEN_FOR: [&str; 3] = ["60", "300", "900"];

/// The SIP page's rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SipRow {
    Enabled,
    Account,
    Password,
    Proxy,
    Use,
    Status,
}

pub const SIP: [SipRow; 6] =
    [SipRow::Enabled, SipRow::Account, SipRow::Password, SipRow::Proxy, SipRow::Use, SipRow::Status];

pub const RINGER: [&str; 3] = ["ring", "vibrate", "silent"];
/// How long a hold is, in milliseconds: up to three seconds, for hands that press
/// slowly or shake.
pub const HOLD_TIMES: [&str; 7] = ["400", "600", "800", "1000", "1500", "2000", "3000"];
pub const REGIONS: [&str; 12] = ["AU", "NZ", "GB", "IE", "US", "CA", "DE", "FR", "NL", "IN", "SG", "JP"];

/// A main-page row: what it is, so keys and drawing agree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MainRow {
    Ringer,
    Vibrate,
    Audio,
    Bluetooth,
    Calls,
    Sip,
    Laptops,
    Texts,
    Alerts,
    Privacy,
    Region,
    HoldTime,
    Modem,
    Wipe,
    About,
}

pub const MAIN: [MainRow; 15] = [
    MainRow::Ringer,
    MainRow::Vibrate,
    MainRow::Audio,
    MainRow::Bluetooth,
    MainRow::Calls,
    MainRow::Sip,
    MainRow::Laptops,
    MainRow::Texts,
    MainRow::Alerts,
    MainRow::Privacy,
    MainRow::Region,
    MainRow::HoldTime,
    MainRow::Modem,
    MainRow::Wipe,
    MainRow::About,
];

/// An ON/OFF row on a sub-page: its label and the setting it is.
pub type Flag = (&'static str, (&'static str, &'static str));

/// Calls: the voicemail row, then these.
pub const CALL_FLAGS: [Flag; 7] = [
    ("Call waiting", keys::CALL_WAITING),
    ("Silence unknown callers", keys::SILENCE_UNKNOWN),
    ("Say who is calling", keys::SPEAK_CALLER),
    ("Hold to decline", keys::STEADY_DECLINE),
    ("Ringback on this phone", keys::LOCAL_RINGBACK),
    ("Keep off 2G", keys::DISABLE_2G),
    ("Warn when on 2G", keys::WARN_2G),
];

pub const TEXT_FLAGS: [Flag; 4] = [
    ("Delivery reports", keys::DELIVERY_REPORTS),
    ("Text tone", keys::TEXT_TONE),
    ("Hide texts in alerts", keys::PRIVATE_NOTIFY),
    ("Clear texts from SIM", keys::DELETE_FROM_SIM),
];

/// Emergency alerts: national alerts are always on (row 0, not a flag), then these,
/// then the alerts received.
pub const ALERT_FLAGS: [Flag; 4] = [
    ("Extreme alerts", keys::ALERTS_EXTREME),
    ("Severe alerts", keys::ALERTS_SEVERE),
    ("Amber alerts", keys::ALERTS_AMBER),
    ("Test alerts", keys::ALERTS_TEST),
];

/// Every flag a page shows, for loading them.
pub fn all_flags() -> impl Iterator<Item = (&'static str, &'static str)> {
    CALL_FLAGS.iter().chain(TEXT_FLAGS.iter()).chain(ALERT_FLAGS.iter()).map(|f| f.1).chain([
        keys::VIBRATE,
        keys::SIP_ENABLED,
        keys::KDECONNECT,
    ])
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrivacyRow {
    Blocked,
    Grants,
    Audit,
    /// Set, change or remove the PIN that keeps texts closed.
    MessagesPin,
    /// How long Messages stays open once the PIN is in.
    StayOpen,
}

pub const PRIVACY: [PrivacyRow; 5] =
    [PrivacyRow::Blocked, PrivacyRow::Grants, PrivacyRow::Audit, PrivacyRow::MessagesPin, PrivacyRow::StayOpen];

pub struct Model {
    pub page: Page,
    pub sel: [usize; PAGES],
    pub ringer: String,
    /// The ON/OFF settings, by key.
    pub flags: HashMap<&'static str, bool>,
    pub voicemail: String,
    pub sip_account: String,
    pub sip_proxy: String,
    pub sip_password_set: bool,
    pub sip_use: String,
    pub hold_ms: String,
    pub uplink_to: String,
    pub uplink_iface: String,
    pub region: String,
    pub alerts: Vec<AlertInfo>,
    pub device: Option<phone_api::DeviceInfo>,
    /// Laptops linked over KDE Connect: (id, name, paired).
    pub laptops: Vec<(String, String, bool)>,
    pub grants: Vec<Grant>,
    pub audit: Vec<AuditEntry>,
    pub routes: Vec<RouteInfo>,
    pub bt_available: bool,
    pub bt_powered: bool,
    pub devices: Vec<BtDevice>,
    pub blocked: Vec<String>,
    pub status: Option<ModemStatus>,
    /// The modem's one-time setup, as last checked.
    pub setup: Vec<(String, String, String)>,
    /// A modal, and what Run does on it.
    pub modal: Option<(Vec<String>, Option<Action>)>,
    pub connected: bool,
    /// Whether a Messages PIN is set (phoned never says what it is).
    pub messages_pin_set: bool,
    /// Seconds Messages stays open after the PIN.
    pub open_for: String,
    /// The new PIN typed once, waiting to be typed again.
    pub new_pin: Option<String>,
    /// The cells, as last asked for; None until the answer to the first comes.
    pub cells: Option<CellReport>,
}

impl Default for Model {
    fn default() -> Self {
        Self {
            page: Page::Main,
            sel: [0; PAGES],
            ringer: "ring".into(),
            flags: all_flags().map(|(k, d)| (k, d == "true")).collect(),
            voicemail: String::new(),
            sip_account: String::new(),
            sip_proxy: String::new(),
            sip_password_set: false,
            sip_use: "fallback".into(),
            hold_ms: "600".into(),
            uplink_to: String::new(),
            uplink_iface: String::new(),
            region: "AU".into(),
            alerts: Vec::new(),
            device: None,
            laptops: Vec::new(),
            grants: Vec::new(),
            audit: Vec::new(),
            routes: Vec::new(),
            bt_available: false,
            bt_powered: false,
            devices: Vec::new(),
            blocked: Vec::new(),
            status: None,
            setup: Vec::new(),
            modal: None,
            connected: false,
            messages_pin_set: false,
            open_for: keys::MESSAGES_OPEN_FOR.1.into(),
            new_pin: None,
            cells: None,
        }
    }
}

fn page_index(p: Page) -> usize {
    p as usize
}

impl Model {
    pub fn flag(&self, key: (&str, &str)) -> bool {
        self.flags.get(key.0).copied().unwrap_or(key.1 == "true")
    }

    /// A setting phoned reported. Returns false for keys this app does not show.
    pub fn setting(&mut self, key: &str, value: &str) -> bool {
        if key == keys::VOICEMAIL.0 {
            self.voicemail = value.to_string();
            return true;
        }
        if key == keys::REGION.0 {
            self.region = value.to_string();
            return true;
        }
        match key {
            k if k == keys::SIP_ACCOUNT.0 => self.sip_account = value.to_string(),
            k if k == keys::SIP_PROXY.0 => self.sip_proxy = value.to_string(),
            k if k == keys::SIP_PASSWORD.0 => self.sip_password_set = value == "set",
            k if k == keys::SIP_USE.0 => self.sip_use = value.to_string(),
            k if k == keys::LONG_PRESS_MS.0 => self.hold_ms = value.to_string(),
            k if k == keys::UPLINK_ALERT_TO.0 => self.uplink_to = value.to_string(),
            k if k == keys::UPLINK_INTERFACE.0 => self.uplink_iface = value.to_string(),
            k if k == keys::MESSAGES_PIN.0 => self.messages_pin_set = value == "set",
            k if k == keys::MESSAGES_OPEN_FOR.0 => self.open_for = value.to_string(),
            _ => {
                return match all_flags().find(|(k, _)| *k == key) {
                    Some((k, _)) => {
                        self.flags.insert(k, value == "true");
                        true
                    }
                    None => false,
                }
            }
        }
        true
    }

    /// The text a typed setting starts from (a password always starts empty).
    pub fn field_text(&self, f: Field) -> String {
        match f {
            Field::Voicemail => self.voicemail.clone(),
            Field::SipAccount => self.sip_account.clone(),
            Field::SipProxy => self.sip_proxy.clone(),
            Field::UplinkTo => self.uplink_to.clone(),
            Field::UplinkIface => self.uplink_iface.clone(),
            Field::SipPassword | Field::Pin | Field::PinAgain => String::new(),
        }
    }

    fn toggle(&mut self, key: (&'static str, &'static str)) -> Vec<Action> {
        let on = !self.flag(key);
        self.flags.insert(key.0, on);
        vec![Action::Set(key.0, on.to_string())]
    }

    /// Whether the modem's setup has something to change.
    pub fn setup_needed(&self) -> bool {
        self.setup.iter().any(|(_, state, _)| state == "change")
    }

    /// Whether the setup could not run because ModemManager passes no AT commands.
    pub fn needs_access(&self) -> bool {
        self.setup.iter().any(|(_, state, why)| state == "error" && why.contains("debug mode"))
    }

    /// What the setup did, for a modal.
    pub fn setup_done(&mut self, steps: Vec<(String, String, String)>) {
        let failed = steps.iter().find(|(_, state, _)| state == "error").map(|(_, _, d)| d.clone());
        self.modal = Some(match failed {
            Some(why) => (vec!["Not set up:".into(), why], None),
            None => (vec!["Modem set up.".into(), "It is restarting;".into(), "back in half a minute.".into()], None),
        });
        self.setup = steps;
    }

    pub fn sel(&self) -> usize {
        self.sel[page_index(self.page)]
    }

    fn set_sel(&mut self, v: usize) {
        let i = page_index(self.page);
        self.sel[i] = v;
    }

    /// Rows on the current page, for bounding the selection.
    pub fn rows(&self) -> usize {
        match self.page {
            Page::Main => MAIN.len(),
            Page::Audio => self.routes.len().max(1),
            // Power, scan, then the devices.
            Page::Bluetooth => 2 + self.devices.len(),
            Page::Blocked => self.blocked.len().max(1),
            Page::Modem | Page::About => 1,
            // Up and Down scroll: one step per row past the screen's eight.
            Page::Network => self.network_rows().len().saturating_sub(DETAIL_ROWS) + 1,
            Page::Calls => 1 + CALL_FLAGS.len(),
            // The flags, then the uplink watch's number and interface.
            Page::Texts => TEXT_FLAGS.len() + 2,
            Page::Alerts => 1 + ALERT_FLAGS.len() + self.alerts.len(),
            Page::Privacy => PRIVACY.len(),
            Page::Grants => self.grants.len().max(1),
            Page::Sip => SIP.len(),
            Page::Laptops => 1 + self.laptops.len(),
            Page::Audit => self.audit.len().max(1),
        }
    }

    pub fn key(&mut self, input: Input) -> Vec<Action> {
        if let Some((_, then)) = self.modal.take() {
            return match (input, then) {
                (Input::Run, Some(a)) => vec![a],
                _ => Vec::new(),
            };
        }
        let n = self.rows();
        match input {
            Input::Up => {
                self.set_sel(self.sel().min(n.saturating_sub(1)).saturating_sub(1));
                return Vec::new();
            }
            Input::Down => {
                self.set_sel((self.sel() + 1).min(n.saturating_sub(1)));
                return Vec::new();
            }
            Input::Escape => {
                let up = match self.page {
                    Page::Main => return vec![Action::Quit],
                    Page::Blocked | Page::Grants | Page::Audit => Page::Privacy,
                    Page::Network => Page::Modem,
                    _ => Page::Main,
                };
                self.page = up;
                return vec![Action::Refresh(up)];
            }
            _ => {}
        }
        match self.page {
            Page::Main => self.key_main(input),
            Page::Audio => match input {
                Input::Run | Input::Ok => match self.routes.get(self.sel()) {
                    Some(r) if r.available => vec![Action::SetRoute(r.route)],
                    Some(r) => {
                        self.modal = Some((vec![format!("{} is not connected.", r.name)], None));
                        Vec::new()
                    }
                    None => Vec::new(),
                },
                _ => Vec::new(),
            },
            Page::Bluetooth => self.key_bluetooth(input),
            Page::Blocked => match input {
                Input::Run | Input::Ok | Input::Edit => match self.blocked.get(self.sel()) {
                    Some(n) => {
                        self.modal = Some((vec![format!("Unblock {n}?")], Some(Action::Unblock(n.clone()))));
                        Vec::new()
                    }
                    None => Vec::new(),
                },
                _ => Vec::new(),
            },
            Page::Modem if input == Input::Run && self.needs_access() => {
                self.modal = Some((
                    vec!["Let Dolphone send".into(), "AT commands? It turns".into(), "on MM debug mode.".into()],
                    Some(Action::ModemAccess),
                ));
                Vec::new()
            }
            Page::Modem if input == Input::Run && self.setup_needed() => {
                self.modal = Some((
                    vec!["Set up the modem?".into(), "USB audio and VoLTE,".into(), "saved in it; it restarts.".into()],
                    Some(Action::ApplyModemSetup),
                ));
                Vec::new()
            }
            Page::Modem if input == Input::View => {
                // What it showed last time may be from somewhere else: ask afresh.
                self.cells = None;
                self.page = Page::Network;
                self.set_sel(0);
                vec![Action::Refresh(Page::Network)]
            }
            Page::Network if matches!(input, Input::Run | Input::Ok) => vec![Action::Refresh(Page::Network)],
            Page::Modem | Page::About | Page::Audit | Page::Network => Vec::new(),
            Page::Calls => {
                let toggle = matches!(input, Input::Left | Input::Right | Input::Run | Input::Ok);
                match self.sel() {
                    0 if matches!(input, Input::Run | Input::Ok | Input::Edit) => {
                        vec![Action::EditText(Field::Voicemail)]
                    }
                    i if toggle && i >= 1 => self.toggle(CALL_FLAGS[(i - 1).min(CALL_FLAGS.len() - 1)].1),
                    _ => Vec::new(),
                }
            }
            Page::Texts => match self.sel() {
                i if i < TEXT_FLAGS.len() && matches!(input, Input::Left | Input::Right | Input::Run | Input::Ok) => {
                    self.toggle(TEXT_FLAGS[i].1)
                }
                i if i == TEXT_FLAGS.len() && matches!(input, Input::Run | Input::Ok | Input::Edit) => {
                    vec![Action::EditText(Field::UplinkTo)]
                }
                i if i == TEXT_FLAGS.len() + 1 && matches!(input, Input::Run | Input::Ok | Input::Edit) => {
                    vec![Action::EditText(Field::UplinkIface)]
                }
                _ => Vec::new(),
            },
            Page::Alerts => {
                let toggle = matches!(input, Input::Left | Input::Right | Input::Run | Input::Ok);
                match self.sel() {
                    0 if toggle => {
                        self.modal = Some((vec!["National alerts are".into(), "always on.".into()], None));
                        Vec::new()
                    }
                    i if toggle && (1..=ALERT_FLAGS.len()).contains(&i) => self.toggle(ALERT_FLAGS[i - 1].1),
                    i if matches!(input, Input::Run | Input::Ok) => {
                        if let Some(a) = self.alerts.get(i - 1 - ALERT_FLAGS.len()) {
                            let mut lines = vec![phone_core::model::AlertLevel::from_u8(a.level).label().to_string()];
                            lines.extend(wrap_modal(&a.text));
                            self.modal = Some((lines, None));
                        }
                        Vec::new()
                    }
                    _ => Vec::new(),
                }
            }
            Page::Privacy => self.key_privacy(input),
            Page::Grants if matches!(input, Input::Run | Input::Ok | Input::Edit) => {
                if let Some(g) = self.grants.get(self.sel()) {
                    self.modal = Some((
                        vec![format!("Revoke {}'s grant?", g.program), "It will have to ask".into(), "again.".into()],
                        Some(Action::Revoke(g.id)),
                    ));
                }
                Vec::new()
            }
            Page::Sip => {
                let toggle = matches!(input, Input::Left | Input::Right | Input::Run | Input::Ok);
                let open = matches!(input, Input::Run | Input::Ok | Input::Edit);
                match SIP[self.sel().min(SIP.len() - 1)] {
                    SipRow::Enabled if toggle => self.toggle(keys::SIP_ENABLED),
                    SipRow::Account if open => vec![Action::EditText(Field::SipAccount)],
                    SipRow::Password if open => vec![Action::EditText(Field::SipPassword)],
                    SipRow::Proxy if open => vec![Action::EditText(Field::SipProxy)],
                    SipRow::Use if toggle => {
                        self.sip_use = if self.sip_use == "always" { "fallback".into() } else { "always".into() };
                        vec![Action::Set(keys::SIP_USE.0, self.sip_use.clone())]
                    }
                    _ => Vec::new(),
                }
            }
            Page::Laptops => match self.sel() {
                0 if matches!(input, Input::Left | Input::Right | Input::Run | Input::Ok) => {
                    self.toggle(keys::KDECONNECT)
                }
                i if i >= 1 && matches!(input, Input::Run | Input::Ok | Input::Edit) => {
                    match self.laptops.get(i - 1) {
                        Some((id, name, true)) => {
                            self.modal =
                                Some((vec![format!("Unpair {name}?")], Some(Action::LaptopUnpair(id.clone()))));
                        }
                        Some((_, name, false)) => {
                            self.modal = Some((
                                vec![
                                    format!("Pair from {name}:"),
                                    "KDE Connect > this phone".into(),
                                    "> Pair, then allow here".into(),
                                ],
                                None,
                            ));
                        }
                        None => {}
                    }
                    Vec::new()
                }
                _ => Vec::new(),
            },
            Page::Grants => Vec::new(),
        }
    }

    fn key_privacy(&mut self, input: Input) -> Vec<Action> {
        let open = matches!(input, Input::Run | Input::Ok | Input::Right);
        let page = match PRIVACY[self.sel().min(PRIVACY.len() - 1)] {
            PrivacyRow::Blocked if open => Page::Blocked,
            PrivacyRow::Grants if open => Page::Grants,
            PrivacyRow::Audit if open => Page::Audit,
            PrivacyRow::MessagesPin if open => return vec![Action::EditText(Field::Pin)],
            PrivacyRow::MessagesPin if input == Input::Edit && self.messages_pin_set => {
                self.modal = Some((
                    vec!["Remove the Messages PIN?".into(), "Texts open without it.".into()],
                    Some(Action::Set(keys::MESSAGES_PIN.0, String::new())),
                ));
                return Vec::new();
            }
            PrivacyRow::StayOpen if matches!(input, Input::Left | Input::Right | Input::Run | Input::Ok) => {
                let i = OPEN_FOR.iter().position(|x| *x == self.open_for).unwrap_or(1);
                let j = if input == Input::Left {
                    (i + OPEN_FOR.len() - 1) % OPEN_FOR.len()
                } else {
                    (i + 1) % OPEN_FOR.len()
                };
                self.open_for = OPEN_FOR[j].to_string();
                return vec![Action::Set(keys::MESSAGES_OPEN_FOR.0, self.open_for.clone())];
            }
            _ => return Vec::new(),
        };
        self.page = page;
        self.set_sel(0);
        vec![Action::Refresh(page)]
    }

    fn key_main(&mut self, input: Input) -> Vec<Action> {
        let row = MAIN[self.sel().min(MAIN.len() - 1)];
        let toggle = matches!(input, Input::Left | Input::Right | Input::Run | Input::Ok);
        let step = |list: &[&str], cur: &str, fwd: bool| -> String {
            let i = list.iter().position(|x| *x == cur).unwrap_or(0);
            let j = if fwd { (i + 1) % list.len() } else { (i + list.len() - 1) % list.len() };
            list[j].to_string()
        };
        let fwd = !matches!(input, Input::Left);
        match row {
            MainRow::Ringer if toggle => {
                self.ringer = step(&RINGER, &self.ringer, fwd);
                vec![Action::SetRinger(self.ringer.clone())]
            }
            MainRow::Vibrate if toggle => self.toggle(keys::VIBRATE),
            MainRow::Region if toggle => {
                self.region = step(&REGIONS, &self.region, fwd);
                vec![Action::Set("region", self.region.clone())]
            }
            MainRow::HoldTime if toggle => {
                self.hold_ms = step(&HOLD_TIMES, &self.hold_ms, fwd);
                vec![Action::Set(keys::LONG_PRESS_MS.0, self.hold_ms.clone())]
            }
            MainRow::Audio
            | MainRow::Bluetooth
            | MainRow::Calls
            | MainRow::Sip
            | MainRow::Laptops
            | MainRow::Texts
            | MainRow::Alerts
            | MainRow::Privacy
            | MainRow::Modem
            | MainRow::About
                if matches!(input, Input::Run | Input::Ok | Input::Right) =>
            {
                self.page = match row {
                    MainRow::Audio => Page::Audio,
                    MainRow::Bluetooth => Page::Bluetooth,
                    MainRow::Calls => Page::Calls,
                    MainRow::Sip => Page::Sip,
                    MainRow::Laptops => Page::Laptops,
                    MainRow::Texts => Page::Texts,
                    MainRow::Alerts => Page::Alerts,
                    MainRow::Privacy => Page::Privacy,
                    MainRow::Modem => Page::Modem,
                    _ => Page::About,
                };
                self.set_sel(0);
                vec![Action::Refresh(self.page)]
            }
            MainRow::Wipe if matches!(input, Input::Run | Input::Ok) => {
                self.modal = Some((
                    vec![
                        "Delete all texts, the call".into(),
                        "log and blocked numbers?".into(),
                        "This cannot be undone.".into(),
                    ],
                    Some(Action::Wipe),
                ));
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn key_bluetooth(&mut self, input: Input) -> Vec<Action> {
        let sel = self.sel();
        match (sel, input) {
            (0, Input::Left | Input::Right | Input::Run | Input::Ok) if self.bt_available => {
                self.bt_powered = !self.bt_powered;
                vec![Action::BtPower(self.bt_powered)]
            }
            (1, Input::Run | Input::Ok) if self.bt_powered => vec![Action::BtScan],
            (i, Input::Run | Input::Ok) if i >= 2 => {
                let Some(d) = self.devices.get(i - 2) else { return Vec::new() };
                if !d.paired {
                    vec![Action::BtPair(d.address.clone())]
                } else {
                    vec![Action::BtConnect(d.address.clone(), !d.connected)]
                }
            }
            (i, Input::View) if i >= 2 => {
                let Some(d) = self.devices.get(i - 2) else { return Vec::new() };
                if !d.paired {
                    return Vec::new();
                }
                self.modal = Some((
                    vec![
                        format!("Copy contacts from {}?", d.name),
                        "Names and numbers only.".into(),
                        "Allow it on the phone.".into(),
                    ],
                    Some(Action::ImportContacts(d.address.clone())),
                ));
                Vec::new()
            }
            (i, Input::Edit) if i >= 2 => {
                let Some(d) = self.devices.get(i - 2) else { return Vec::new() };
                if d.paired {
                    self.modal = Some((vec![format!("Forget {}?", d.name)], Some(Action::BtForget(d.address.clone()))));
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// The keyboard closed on a typed setting.
    pub fn written(&mut self, f: Field, text: Option<String>) -> Vec<Action> {
        let Some(t) = text else {
            self.new_pin = None;
            return Vec::new();
        };
        let t = t.trim().to_string();
        match f {
            Field::Pin => {
                if !valid_pin(&t) {
                    self.modal = Some((vec!["A PIN is 4 to 12 digits.".into()], None));
                    return Vec::new();
                }
                self.new_pin = Some(t);
                vec![Action::EditText(Field::PinAgain)]
            }
            Field::PinAgain => match self.new_pin.take() {
                Some(first) if first == t => {
                    self.messages_pin_set = true;
                    vec![Action::Set(keys::MESSAGES_PIN.0, t)]
                }
                _ => {
                    self.modal = Some((vec!["The two PINs differ.".into(), "Nothing was changed.".into()], None));
                    Vec::new()
                }
            },
            Field::Voicemail => {
                let n: String = t.chars().filter(|c| c.is_ascii_digit() || *c == '+').collect();
                self.voicemail = n.clone();
                vec![Action::Set(keys::VOICEMAIL.0, n)]
            }
            Field::SipAccount => {
                // A bare user@host is a SIP address too.
                let a = if t.is_empty() || t.starts_with("sip") { t } else { format!("sip:{t}") };
                self.sip_account = a.clone();
                vec![Action::Set(keys::SIP_ACCOUNT.0, a)]
            }
            Field::SipProxy => {
                let p = if t.is_empty() || t.starts_with("sip") { t } else { format!("sip:{t}") };
                self.sip_proxy = p.clone();
                vec![Action::Set(keys::SIP_PROXY.0, p)]
            }
            Field::SipPassword => {
                self.sip_password_set = !t.is_empty();
                vec![Action::Set(keys::SIP_PASSWORD.0, t)]
            }
            Field::UplinkTo => {
                let n: String = t.chars().filter(|c| c.is_ascii_digit() || *c == '+').collect();
                self.uplink_to = n.clone();
                vec![Action::Set(keys::UPLINK_ALERT_TO.0, n)]
            }
            Field::UplinkIface => {
                let i: String =
                    t.chars().filter(|c| c.is_ascii_alphanumeric() || "-_.".contains(*c)).take(15).collect();
                self.uplink_iface = i.clone();
                vec![Action::Set(keys::UPLINK_INTERFACE.0, i)]
            }
        }
    }

    /// The Network screen: the serving cell with all the modem gave for it, then
    /// a line for each neighbour. Firmware fills in what it fills in; what it
    /// left out is left off.
    pub fn network_rows(&self) -> Vec<NetRow> {
        let text = |t: &str| NetRow::Text(t.into());
        let cells = match &self.cells {
            None => return vec![text("Asking the modem...")],
            Some(CellReport::Unsupported) => {
                return vec![
                    text("This modem does not report"),
                    text("its cells: ModemManager 1.20+"),
                    text("gets them from QMI and MBIM"),
                    text("modems only."),
                ]
            }
            Some(CellReport::Failed(why)) => {
                let mut rows = vec![text("No cell information:")];
                rows.extend(wrap_modal(why).into_iter().take(3).map(NetRow::Text));
                return rows;
            }
            Some(CellReport::Cells(c)) if c.is_empty() => return vec![text("No cells heard yet.")],
            Some(CellReport::Cells(c)) => c,
        };
        let mut rows = Vec::new();
        for c in cells.iter().filter(|c| c.serving) {
            rows.push(NetRow::Head(format!("{} serving", c.tech()), c.operator_id.clone()));
            let mut value = |label: &str, v: String| {
                if !v.is_empty() {
                    rows.push(NetRow::Value(label.into(), v));
                }
            };
            let n = |v: u32| CellInfo::given(v).map(|v| v.to_string()).unwrap_or_default();
            let db = |v: f64, unit: &str| CellInfo::db(v).map(|v| format!("{v} {unit}")).unwrap_or_default();
            value(c.channel_name(), n(c.channel));
            value("Cell ID", c.ci.clone());
            value(c.area_name(), c.area.clone());
            value("PCI", n(c.pci));
            value("PSC", n(c.psc));
            value("RSRP", db(c.rsrp, "dBm"));
            value("RSRQ", db(c.rsrq, "dB"));
            value("SINR", db(c.sinr, "dB"));
            value("RSCP", db(c.rscp, "dBm"));
            value("Ec/Io", db(c.ecio, "dB"));
            value("RX level", n(c.rx_level));
        }
        let near: Vec<&CellInfo> = cells.iter().filter(|c| !c.serving).collect();
        if !near.is_empty() {
            if !rows.is_empty() {
                rows.push(NetRow::Divider);
            }
            rows.push(NetRow::Head("Neighbours".into(), near.len().to_string()));
            for c in near {
                // "LTE 1275 PCI 37": the channel and the code that tell it apart.
                let mut label = c.tech().to_string();
                if let Some(ch) = CellInfo::given(c.channel) {
                    label += &format!(" {ch}");
                }
                if let Some(p) = CellInfo::given(c.pci) {
                    label += &format!(" PCI {p}");
                } else if let Some(p) = CellInfo::given(c.psc) {
                    label += &format!(" PSC {p}");
                }
                rows.push(NetRow::Value(label, c.strength()));
            }
        }
        rows
    }

    pub fn imported(&mut self, added: u32, merged: u32, unchanged: u32, error: &str) {
        self.modal = Some((
            if error.is_empty() {
                vec![
                    "Contacts copied".into(),
                    format!("{added} new, {merged} updated,"),
                    format!("{unchanged} already here"),
                ]
            } else {
                let mut l = vec!["Contacts not copied".to_string()];
                l.extend(wrap_modal(error).into_iter().take(3));
                l
            },
            None,
        ));
    }

    pub fn passkey(&mut self, name: &str, key: u32) {
        self.modal =
            Some((vec![format!("Pairing with {name}"), format!("Code {key:06}"), "Check it matches.".into()], None));
    }
}

/// Split text into modal lines of about 30 characters, at most four.
fn wrap_modal(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    for w in text.split_whitespace() {
        if !line.is_empty() && line.len() + 1 + w.len() > 30 {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(w);
    }
    if !line.is_empty() {
        out.push(line);
    }
    out.truncate(4);
    out
}

#[cfg(test)]
// Tests build a model and then set the few fields each one is about.
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;

    #[test]
    fn ringer_steps_both_ways() {
        let mut m = Model::default();
        assert_eq!(m.key(Input::Right), vec![Action::SetRinger("vibrate".into())]);
        assert_eq!(m.key(Input::Right), vec![Action::SetRinger("silent".into())]);
        assert_eq!(m.key(Input::Left), vec![Action::SetRinger("vibrate".into())]);
    }

    #[test]
    fn toggles_and_pages() {
        let mut m = Model::default();
        m.key(Input::Down);
        assert_eq!(m.key(Input::Run), vec![Action::Set("vibrate", "false".into())]);
        m.key(Input::Down);
        assert_eq!(m.key(Input::Run), vec![Action::Refresh(Page::Audio)]);
        assert_eq!(m.page, Page::Audio);
        assert_eq!(m.key(Input::Escape), vec![Action::Refresh(Page::Main)]);
        assert_eq!(m.key(Input::Escape), vec![Action::Quit]);
    }

    #[test]
    fn wipe_asks_first() {
        let mut m = Model::default();
        for _ in 0..MAIN.iter().position(|r| *r == MainRow::Wipe).unwrap() {
            m.key(Input::Down);
        }
        assert!(m.key(Input::Run).is_empty());
        assert!(m.modal.is_some());
        assert_eq!(m.key(Input::Run), vec![Action::Wipe]);
        // Escape on the modal cancels.
        m.key(Input::Run);
        assert!(m.key(Input::Escape).is_empty());
    }

    #[test]
    fn contacts_from_a_paired_phone() {
        let mut m = Model::default();
        m.page = Page::Bluetooth;
        m.bt_available = true;
        m.bt_powered = true;
        m.devices = vec![BtDevice {
            address: "AA:BB:CC:00:11:22".into(),
            name: "Pixel 9".into(),
            paired: true,
            connected: false,
            headset: false,
            speaker: false,
        }];
        m.key(Input::Down);
        m.key(Input::Down);
        assert!(m.key(Input::View).is_empty());
        assert_eq!(m.key(Input::Run), vec![Action::ImportContacts("AA:BB:CC:00:11:22".into())]);
        m.imported(120, 3, 0, "");
        assert_eq!(m.modal.as_ref().unwrap().0[1], "120 new, 3 updated,");
    }

    #[test]
    fn bluetooth_pair_then_connect() {
        let mut m = Model::default();
        m.page = Page::Bluetooth;
        m.bt_available = true;
        m.bt_powered = true;
        m.devices = vec![BtDevice {
            address: "AA".into(),
            name: "Buds".into(),
            paired: false,
            connected: false,
            headset: true,
            speaker: true,
        }];
        m.key(Input::Down);
        m.key(Input::Down);
        assert_eq!(m.key(Input::Run), vec![Action::BtPair("AA".into())]);
        m.devices[0].paired = true;
        assert_eq!(m.key(Input::Run), vec![Action::BtConnect("AA".into(), true)]);
    }

    #[test]
    fn call_settings_and_voicemail() {
        let mut m = Model::default();
        for _ in 0..4 {
            m.key(Input::Down);
        }
        assert_eq!(m.key(Input::Run), vec![Action::Refresh(Page::Calls)]);
        assert_eq!(m.key(Input::Run), vec![Action::EditText(Field::Voicemail)]);
        assert_eq!(m.written(Field::Voicemail, Some("1 0 1".into())), vec![Action::Set("voicemail", "101".into())]);
        m.key(Input::Down);
        assert_eq!(m.key(Input::Run), vec![Action::Set("call_waiting", "false".into())]);
        m.key(Input::Down);
        assert_eq!(m.key(Input::Right), vec![Action::Set("silence_unknown", "true".into())]);
        assert_eq!(m.key(Input::Escape), vec![Action::Refresh(Page::Main)]);
    }

    #[test]
    fn national_alerts_cannot_be_turned_off() {
        let mut m = Model::default();
        m.page = Page::Alerts;
        assert!(m.key(Input::Run).is_empty());
        assert!(m.modal.is_some());
        m.key(Input::Escape);
        m.key(Input::Down);
        m.key(Input::Down);
        m.key(Input::Down);
        m.key(Input::Down);
        assert_eq!(m.key(Input::Run), vec![Action::Set("alerts_test", "true".into())]);
    }

    #[test]
    fn grants_are_revoked_after_asking() {
        let mut m = Model::default();
        m.page = Page::Grants;
        m.grants = vec![Grant { id: 3, program: "cron".into(), numbers: "+61491570158".into(), per_hour: 2, until: 0 }];
        assert!(m.key(Input::Run).is_empty());
        assert_eq!(m.key(Input::Run), vec![Action::Revoke(3)]);
        m.page = Page::Grants;
        assert_eq!(m.key(Input::Escape), vec![Action::Refresh(Page::Privacy)], "back to Privacy, not Main");
    }

    #[test]
    fn sip_account_from_the_panel() {
        let mut m = Model::default();
        m.page = Page::Sip;
        assert_eq!(m.key(Input::Run), vec![Action::Set("sip_enabled", "true".into())]);
        m.key(Input::Down);
        assert_eq!(m.key(Input::Run), vec![Action::EditText(Field::SipAccount)]);
        assert_eq!(
            m.written(Field::SipAccount, Some("me@voip.example.net".into())),
            vec![Action::Set("sip_account", "sip:me@voip.example.net".into())]
        );
        assert_eq!(m.field_text(Field::SipPassword), "", "a password is never shown back");
        m.key(Input::Down);
        m.key(Input::Down);
        m.key(Input::Down);
        assert_eq!(m.key(Input::Right), vec![Action::Set("sip_use", "always".into())]);
    }

    #[test]
    fn modem_setup_asks_first() {
        let mut m = Model::default();
        m.page = Page::Modem;
        assert!(m.key(Input::Run).is_empty() && m.modal.is_none(), "nothing to set up, nothing asked");
        m.setup = vec![("volte".into(), "change".into(), String::new())];
        assert!(m.key(Input::Run).is_empty());
        assert!(m.modal.is_some(), "it asks, since the modem restarts");
        assert_eq!(m.key(Input::Run), vec![Action::ApplyModemSetup]);
        m.setup_done(vec![("modem".into(), "error".into(), "no modem".into())]);
        assert_eq!(m.modal.as_ref().map(|(l, _)| l[1].as_str()), Some("no modem"));
    }

    #[test]
    fn messages_pin_is_typed_twice_and_removed_after_asking() {
        let mut m = Model::default();
        m.page = Page::Privacy;
        let at = |r: PrivacyRow| PRIVACY.iter().position(|x| *x == r).unwrap();
        for _ in 0..at(PrivacyRow::MessagesPin) {
            m.key(Input::Down);
        }
        assert!(m.key(Input::Edit).is_empty() && m.modal.is_none(), "nothing to remove yet");
        assert_eq!(m.key(Input::Run), vec![Action::EditText(Field::Pin)]);
        assert!(m.written(Field::Pin, Some("12".into())).is_empty());
        assert!(m.modal.take().is_some(), "too short says so");
        assert_eq!(m.written(Field::Pin, Some("2468".into())), vec![Action::EditText(Field::PinAgain)]);
        assert!(m.written(Field::PinAgain, Some("2469".into())).is_empty());
        assert!(m.modal.take().is_some() && !m.messages_pin_set, "two that differ change nothing");
        m.written(Field::Pin, Some("2468".into()));
        assert_eq!(m.written(Field::PinAgain, Some("2468".into())), vec![Action::Set("messages_pin", "2468".into())]);
        assert!(m.messages_pin_set);
        // Cancelling the second forgets the first.
        m.written(Field::Pin, Some("1111".into()));
        m.written(Field::PinAgain, None);
        assert!(m.new_pin.is_none());
        // Removing asks first.
        assert!(m.key(Input::Edit).is_empty() && m.modal.is_some());
        assert_eq!(m.key(Input::Run), vec![Action::Set("messages_pin", String::new())]);
        // Stay open steps through 1, 5 and 15 minutes.
        m.key(Input::Down);
        assert_eq!(m.key(Input::Right), vec![Action::Set("messages_open_for", "900".into())]);
        assert_eq!(m.key(Input::Right), vec![Action::Set("messages_open_for", "60".into())]);
        assert_eq!(m.key(Input::Left), vec![Action::Set("messages_open_for", "900".into())]);
        assert!(valid_pin("123456789012") && !valid_pin("1234567890123"));
    }

    #[test]
    fn modem_access_asks_first() {
        let mut m = Model::default();
        m.page = Page::Modem;
        m.setup = vec![("modem".into(), "error".into(), "ModemManager passes AT commands only in debug mode".into())];
        assert!(m.needs_access() && !m.setup_needed());
        assert!(m.key(Input::Run).is_empty() && m.modal.is_some());
        assert_eq!(m.key(Input::Run), vec![Action::ModemAccess]);
    }

    fn lte(serving: bool, pci: u32, rsrp: f64) -> CellInfo {
        CellInfo { operator_id: "50599".into(), pci, channel: 1275, rsrp, ..CellInfo::new("lte", serving) }
    }

    #[test]
    fn network_from_the_modem_page() {
        let mut m = Model::default();
        m.page = Page::Modem;
        m.cells = Some(CellReport::Unsupported);
        assert_eq!(m.key(Input::View), vec![Action::Refresh(Page::Network)]);
        assert_eq!((m.page, m.cells.as_ref()), (Page::Network, None), "asked afresh, nothing old shown");
        assert_eq!(m.network_rows(), vec![NetRow::Text("Asking the modem...".into())]);
        assert_eq!(m.key(Input::Run), vec![Action::Refresh(Page::Network)], "Run asks again now");
        assert_eq!(m.key(Input::Escape), vec![Action::Refresh(Page::Modem)], "back to Modem, not Main");
        assert_eq!(m.page, Page::Modem);
    }

    #[test]
    fn network_rows_serving_first_then_neighbours() {
        let mut m = Model::default();
        m.page = Page::Network;
        let serving = CellInfo { ci: "8A1C02".into(), area: "3010".into(), rsrq: -11.0, ..lte(true, 416, -95.0) };
        m.cells = Some(CellReport::Cells(vec![serving, lte(false, 37, -104.0), lte(false, 270, -112.0)]));
        let rows = m.network_rows();
        assert_eq!(rows[0], NetRow::Head("LTE serving".into(), "50599".into()));
        let value = |l: &str, v: &str| NetRow::Value(l.into(), v.into());
        assert_eq!(
            rows[1..7],
            [
                value("EARFCN", "1275"),
                value("Cell ID", "8A1C02"),
                value("TAC", "3010"),
                value("PCI", "416"),
                value("RSRP", "-95 dBm"),
                value("RSRQ", "-11 dB")
            ]
        );
        assert!(!rows.iter().any(|r| matches!(r, NetRow::Value(l, _) if l == "SINR")), "not given, not shown");
        assert_eq!(rows[7], NetRow::Divider);
        assert_eq!(rows[8], NetRow::Head("Neighbours".into(), "2".into()));
        assert_eq!(rows[9], value("LTE 1275 PCI 37", "-104 dBm"));
        // Eleven rows on an eight-row screen: four places to scroll to, no more.
        assert_eq!(m.rows(), 4);
        for _ in 0..10 {
            m.key(Input::Down);
        }
        assert_eq!(m.sel(), 3);
    }

    #[test]
    fn network_without_cells() {
        let mut m = Model::default();
        m.cells = Some(CellReport::Cells(Vec::new()));
        assert_eq!(m.network_rows(), vec![NetRow::Text("No cells heard yet.".into())]);
        m.cells = Some(CellReport::Cells(vec![lte(false, 37, CellInfo::UNSET_DB)]));
        let rows = m.network_rows();
        assert_eq!(rows[0], NetRow::Head("Neighbours".into(), "1".into()), "no divider above nothing");
        assert_eq!(rows[1], NetRow::Value("LTE 1275 PCI 37".into(), String::new()));
        m.cells = Some(CellReport::Failed("Cannot get cell info: device not yet enabled".into()));
        assert_eq!(m.network_rows()[0], NetRow::Text("No cell information:".into()));
    }
}
