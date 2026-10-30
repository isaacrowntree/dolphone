//! Dolphone's D-Bus interface: the names, the wire types, and client proxies.
//!
//! `phoned` serves these on the session bus at [`PATH`] under [`SERVICE`]. Every
//! front end (the panel apps, the desktop app, `phonectl`, a user's script) talks to
//! the phone only through them: nothing else touches ModemManager's voice and
//! messaging interfaces, the audio graph or the store.
//!
//! The interfaces are split by area so a front end can watch only what it draws:
//!
//! | Interface | What |
//! |---|---|
//! | `Calls` | live calls, dialling, answering, the call log |
//! | `Messages` | threads, conversations, sending |
//! | `Confirm` | actions waiting for a press on the device |
//! | `Audio` | where call audio goes, the ringer |
//! | `Bluetooth` | pairing and connecting headsets and speakers |
//! | `Modem` | network status, USSD and service codes, the modem's setup |
//! | `Settings` | blocked numbers, preferences, grants, the audit log, wiping |
//! | `Contacts` | reading and changing contacts, copying a phone's phonebook |
//!
//! What a caller may do depends on who it is: Dolphone's own apps may do
//! everything; another program may ask to dial, text, change a setting or a
//! contact (each waits for a press on the device; see [`Request`]), may hang up and
//! read the modem's status, and may read messages, the call log and where the phone
//! has been (the network history, the cells it hears) only with a read grant
//! (`read_grants`). Refusals come back as the standard
//! `org.freedesktop.DBus.Error.AccessDenied`; bad arguments as `InvalidArgs`; rate
//! limits as `LimitsExceeded`; what the modem or network refused as `Failed` or
//! `NotSupported`, with the reason in the message.
//!
//! Signals carry no message text, ever: [`MessageInfo::body`] is empty in
//! `MessageAdded`, and a reader fetches the conversation to read it.
//!
//! Enums travel as `u8` and convert to and from [`phone_core::model`]'s types on
//! either side (this crate keeps no dependency on phone-core so a script author can
//! use it alone).

use serde::{Deserialize, Serialize};
use zbus::{proxy, Type};

pub const SERVICE: &str = "io.github.isaacrowntree.Dolphone";
pub const PATH: &str = "/io/github/isaacrowntree/Dolphone";

/// The most one picture message may carry, in all: carriers cap an MMS at about
/// 1 MB (mmsd-tng's TotalMaxAttachmentSize), so this is only a guard.
pub const MMS_MAX_BYTES: u64 = 8 * 1024 * 1024;
/// And in how many files.
pub const MMS_MAX_FILES: usize = 10;

pub mod iface {
    pub const CALLS: &str = "io.github.isaacrowntree.Dolphone1.Calls";
    pub const MESSAGES: &str = "io.github.isaacrowntree.Dolphone1.Messages";
    pub const CONFIRM: &str = "io.github.isaacrowntree.Dolphone1.Confirm";
    pub const AUDIO: &str = "io.github.isaacrowntree.Dolphone1.Audio";
    pub const BLUETOOTH: &str = "io.github.isaacrowntree.Dolphone1.Bluetooth";
    pub const MODEM: &str = "io.github.isaacrowntree.Dolphone1.Modem";
    pub const SETTINGS: &str = "io.github.isaacrowntree.Dolphone1.Settings";
}

/// The error names `phoned` replies with: the standard D-Bus ones, with the reason
/// in the message.
pub mod error {
    /// Not allowed for this caller (only Dolphone's own apps, or a read grant).
    pub const DENIED: &str = "org.freedesktop.DBus.Error.AccessDenied";
    /// A bad number, an unknown setting, no such call or request.
    pub const INVALID: &str = "org.freedesktop.DBus.Error.InvalidArgs";
    /// Too many texts or calls this hour, or over a grant's limit.
    pub const RATE_LIMITED: &str = "org.freedesktop.DBus.Error.LimitsExceeded";
    /// Something this modem, network or setup cannot do.
    pub const NOT_SUPPORTED: &str = "org.freedesktop.DBus.Error.NotSupported";
    /// It was tried and failed (no modem, the network refused).
    pub const FAILED: &str = "org.freedesktop.DBus.Error.Failed";
}

/// A live call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[zbus(crate = "zbus::wire")]
pub struct CallInfo {
    pub id: u32,
    pub number: String,
    /// 0 incoming, 1 outgoing.
    pub direction: u8,
    /// phone_core::model::CallState as u8.
    pub state: u8,
    /// Unix seconds the call connected, 0 if it has not.
    pub connected_at: i64,
    pub muted: bool,
    pub emergency: bool,
    /// It is over the internet (SIP), not the mobile network.
    pub internet: bool,
}

/// A cell-broadcast emergency alert.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[zbus(crate = "zbus::wire")]
pub struct AlertInfo {
    pub id: i64,
    pub channel: u32,
    /// phone_core::model::AlertLevel as u8.
    pub level: u8,
    pub text: String,
    pub at: i64,
}

/// A standing permission for a program to text without asking each time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[zbus(crate = "zbus::wire")]
pub struct Grant {
    pub id: i64,
    /// The program, by executable name.
    pub program: String,
    /// Numbers it may text, comma separated; a trailing * matches a prefix.
    pub numbers: String,
    pub per_hour: u32,
    /// Unix seconds it lapses, 0 for never.
    pub until: i64,
}

/// A contact: an id (its file, and its place there), a name, and numbers as
/// (kind, canonical number).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[zbus(crate = "zbus::wire")]
pub struct ContactInfo {
    pub id: String,
    pub name: String,
    pub numbers: Vec<(String, String)>,
}

/// A device profile: what Dolphone knows about the machine it runs on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[zbus(crate = "zbus::wire")]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    /// "tested", "expected" or "untested".
    pub status: String,
    /// Why it was chosen (empty in a list of all of them).
    pub why: String,
    pub source: String,
    pub notes: String,
    /// "flipctl" (the panel apps), "desktop".
    pub front_ends: Vec<String>,
}

/// One line of the audit log: what an outside program asked for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[zbus(crate = "zbus::wire")]
pub struct AuditEntry {
    pub at: i64,
    pub program: String,
    pub action: String,
    pub target: String,
    pub result: String,
}

/// What a grant request asks for, in words: "4 an hour, for 30 days" or "4 an
/// hour, with no end", from its body ("4/h|30").
pub fn grant_terms(body: &str) -> String {
    let (rate, days) = body.split_once("/h|").unwrap_or((body, ""));
    match days.trim() {
        "0" => format!("{rate} an hour, with no end"),
        "1" => format!("{rate} an hour, for a day"),
        d => format!("{rate} an hour, for {d} days"),
    }
}

/// A stored message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[zbus(crate = "zbus::wire")]
pub struct MessageInfo {
    pub id: i64,
    pub peer: String,
    pub direction: u8,
    pub body: String,
    pub at: i64,
    /// phone_core::model::MessageState as u8.
    pub state: u8,
    pub read: bool,
    /// The SIM it went through (last four digits of the ICCID), or empty.
    pub sim: String,
    /// An MMS's attachments: (MIME type, file path).
    pub parts: Vec<(String, String)>,
}

/// One conversation in the thread list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[zbus(crate = "zbus::wire")]
pub struct ThreadInfo {
    pub peer: String,
    pub last_body: String,
    pub last_at: i64,
    pub last_direction: u8,
    pub unread: u32,
}

/// One finished call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[zbus(crate = "zbus::wire")]
pub struct CallRecordInfo {
    pub id: i64,
    pub peer: String,
    pub direction: u8,
    pub started: i64,
    pub duration: i64,
    /// phone_core::model::Outcome as u8.
    pub outcome: u8,
    pub seen: bool,
    /// The SIM it went through (last four digits of the ICCID), or empty.
    pub sim: String,
}

/// What came of asking to dial or send: done straight away, or waiting for the
/// owner to confirm on the device. Exactly one of the two ids is non-zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[zbus(crate = "zbus::wire")]
pub struct Placed {
    /// The call id, or the message id, when it went ahead.
    pub id: i64,
    /// The confirmation request id, when it is waiting.
    pub pending: u32,
}

/// An action waiting for a press on the device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[zbus(crate = "zbus::wire")]
pub struct Request {
    pub id: u32,
    /// What it would do: "call", "sms" (a text or an MMS), "contact", "grant",
    /// "setting", "modem" (the modem's setup), "kdeconnect" (pairing a laptop) or
    /// "wipe".
    pub kind: String,
    /// What it is done to: the number (comma separated for a group), the setting's
    /// key, the contact change in words, the laptop.
    pub target: String,
    /// The text for a call or text ("" to programs other than Dolphone's apps);
    /// the new value for a setting; "N/h|days" for a grant (see [`grant_terms`]).
    pub body: String,
    /// Who asked: the program's name as the daemon saw it.
    pub requester: String,
    /// Why it needs confirming: "external" (another program asked), "premium",
    /// "short", "international", "emergency", or the kind itself for the rest.
    pub reason: String,
}

/// An audio output a call can use.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[zbus(crate = "zbus::wire")]
pub struct RouteInfo {
    /// phone_core::model::Route as u8.
    pub route: u8,
    /// What to show: "Speaker", "Headset", or a Bluetooth device's name.
    pub name: String,
    pub available: bool,
    pub active: bool,
}

/// A Bluetooth device the phone knows of or can see.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[zbus(crate = "zbus::wire")]
pub struct BtDevice {
    pub address: String,
    pub name: String,
    pub paired: bool,
    pub connected: bool,
    /// Offers the hands-free profile, so it can carry a call with its microphone.
    pub headset: bool,
    /// Offers A2DP, so it can play the ringer and media.
    pub speaker: bool,
}

/// The cellular side, for front ends that show more than FlipCTL's status bar does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[zbus(crate = "zbus::wire")]
pub struct ModemStatus {
    pub present: bool,
    pub registered: bool,
    pub roaming: bool,
    pub operator: String,
    /// 0..=100.
    pub quality: u8,
    /// "5G", "LTE", "3G", "2G" or "--".
    pub tech: String,
    /// The modem can place voice calls at all (has a Voice interface).
    pub voice: bool,
    pub own_number: String,
    /// The network only allows emergency calls (no SIM, not registered).
    pub emergency_only: bool,
    /// What the modem is doing, in a word a front end can show: "ready",
    /// "locked" (the SIM wants its PIN), "disabled", "searching", "failed", or ""
    /// when there is no modem.
    pub state: String,
    /// The SIP account: "" when off, "registering", "registered", or what went
    /// wrong ("failed: 403 Forbidden").
    pub sip: String,
}

/// A cell the modem hears, serving or a neighbour, as ModemManager's
/// `Modem.GetCellInfo` reports it (ModemManager 1.20; the keys are listed in
/// phoned's `mm::parse_cells`). Only what the firmware filled in is set: a string
/// left out is "", a number [`CellInfo::UNSET`] and a measurement
/// [`CellInfo::UNSET_DB`], the values libmm-glib itself uses for "not given"
/// (mm-cell-info-lte.c); `phonectl --json cell` leaves them out. Every field
/// travels on D-Bus whether set or not: the struct's signature is fixed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
#[zbus(crate = "zbus::wire")]
pub struct CellInfo {
    /// "gsm", "umts", "tdscdma", "lte", "nr" (5G), "cdma", or "unknown" for a type
    /// newer than this.
    pub kind: String,
    /// The cell the modem is on, rather than one it measures nearby.
    pub serving: bool,
    /// The network's MCC and MNC, "50501".
    pub operator_id: String,
    /// The location area (GSM, UMTS: LAC) or tracking area (LTE, NR: TAC), in
    /// upper-case hex as ModemManager gives it (3GPP TS 27.007).
    pub area: String,
    /// The cell identity, in hex.
    pub ci: String,
    /// The physical cell id (LTE, NR), as a number: ModemManager gives it in hex,
    /// and everything else (3GPP TS 36.211, 38.211; other phones' field tests)
    /// counts it in decimal.
    pub pci: u32,
    /// The primary scrambling code (UMTS).
    pub psc: u32,
    /// The channel: ARFCN (GSM), UARFCN (UMTS, TD-SCDMA), EARFCN (LTE) or
    /// NR-ARFCN (NR).
    pub channel: u32,
    /// The received level (GSM), 0..=63.
    pub rx_level: u32,
    /// UMTS and TD-SCDMA: received signal code power in dBm, and Ec/Io in dB.
    pub rscp: f64,
    pub ecio: f64,
    /// LTE and NR: reference signal received power in dBm, and quality in dB.
    pub rsrp: f64,
    pub rsrq: f64,
    /// NR only: ModemManager has no SINR for LTE.
    pub sinr: f64,
    /// The serving cell's timing advance.
    pub timing_advance: u32,
    /// Which serving cell this is (MMServingCellType, ModemManager 1.22): 1 the
    /// primary, 2 a secondary (carrier aggregation), 3 and 4 the primary and a
    /// secondary of the secondary group (5G next to LTE).
    pub serving_cell_type: u32,
    /// The carrier's downlink bandwidth, as the modem gives it (ModemManager 1.22,
    /// which does not say in what unit).
    pub bandwidth: u32,
}

impl CellInfo {
    /// A number the modem did not give (libmm-glib's G_MAXUINT).
    pub const UNSET: u32 = u32::MAX;
    /// A measurement the modem did not give (libmm-glib's -G_MAXDOUBLE).
    pub const UNSET_DB: f64 = f64::MIN;

    /// A cell of this kind with nothing measured yet.
    pub fn new(kind: &str, serving: bool) -> Self {
        Self {
            kind: kind.into(),
            serving,
            operator_id: String::new(),
            area: String::new(),
            ci: String::new(),
            pci: Self::UNSET,
            psc: Self::UNSET,
            channel: Self::UNSET,
            rx_level: Self::UNSET,
            rscp: Self::UNSET_DB,
            ecio: Self::UNSET_DB,
            rsrp: Self::UNSET_DB,
            rsrq: Self::UNSET_DB,
            sinr: Self::UNSET_DB,
            timing_advance: Self::UNSET,
            serving_cell_type: Self::UNSET,
            bandwidth: Self::UNSET,
        }
    }

    /// A number, if the modem gave it.
    pub fn given(v: u32) -> Option<u32> {
        (v != Self::UNSET).then_some(v)
    }

    /// A measurement, if the modem gave it, in words: "-95", "-10.5".
    pub fn db(v: f64) -> Option<String> {
        (v != Self::UNSET_DB && v.is_finite()).then(|| {
            if v.fract() == 0.0 {
                format!("{v:.0}")
            } else {
                format!("{v:.1}")
            }
        })
    }

    /// The technology, as a phone shows it: "LTE", "5G NR", "UMTS".
    pub fn tech(&self) -> &'static str {
        match self.kind.as_str() {
            "gsm" => "GSM",
            "umts" => "UMTS",
            "tdscdma" => "TD-SCDMA",
            "lte" => "LTE",
            "nr" => "5G NR",
            "cdma" => "CDMA",
            _ => "Unknown",
        }
    }

    /// What its channel number is called: "EARFCN".
    pub fn channel_name(&self) -> &'static str {
        match self.kind.as_str() {
            "gsm" => "ARFCN",
            "umts" | "tdscdma" => "UARFCN",
            "lte" => "EARFCN",
            "nr" => "NR-ARFCN",
            _ => "Channel",
        }
    }

    /// What its area is called: "TAC" (LTE, NR) or "LAC".
    pub fn area_name(&self) -> &'static str {
        match self.kind.as_str() {
            "lte" | "nr" => "TAC",
            _ => "LAC",
        }
    }

    /// All the modem gave, on one line: "LTE 50599 TAC 3010 CI 8A1C02 PCI 416
    /// EARFCN 1275 RSRP -95 dBm RSRQ -11 dB".
    pub fn describe(&self) -> String {
        let mut parts = vec![self.tech().to_string()];
        for (name, v) in [("", &self.operator_id), (self.area_name(), &self.area), ("CI", &self.ci)] {
            if !v.is_empty() {
                parts.push(if name.is_empty() { v.clone() } else { format!("{name} {v}") });
            }
        }
        for (name, v) in
            [("PCI", self.pci), ("PSC", self.psc), (self.channel_name(), self.channel), ("RX level", self.rx_level)]
        {
            if let Some(v) = Self::given(v) {
                parts.push(format!("{name} {v}"));
            }
        }
        for (name, v, unit) in [
            ("RSRP", self.rsrp, "dBm"),
            ("RSRQ", self.rsrq, "dB"),
            ("SINR", self.sinr, "dB"),
            ("RSCP", self.rscp, "dBm"),
            ("Ec/Io", self.ecio, "dB"),
        ] {
            if let Some(v) = Self::db(v) {
                parts.push(format!("{name} {v} {unit}"));
            }
        }
        for (name, v) in [("TA", self.timing_advance), ("bandwidth", self.bandwidth)] {
            if let Some(v) = Self::given(v) {
                parts.push(format!("{name} {v}"));
            }
        }
        parts.join(" ")
    }

    /// Its signal in one short phrase, the way each technology measures it:
    /// "-95 dBm" (RSRP, RSCP), or "RX 40" (GSM's level). Empty when not measured.
    pub fn strength(&self) -> String {
        if let Some(p) = Self::db(self.rsrp).or_else(|| Self::db(self.rscp)) {
            format!("{p} dBm")
        } else if let Some(l) = Self::given(self.rx_level) {
            format!("RX {l}")
        } else {
            String::new()
        }
    }
}

#[proxy(
    interface = "io.github.isaacrowntree.Dolphone1.Calls",
    default_service = "io.github.isaacrowntree.Dolphone",
    default_path = "/io/github/isaacrowntree/Dolphone"
)]
pub trait Calls {
    /// Dial a number. Our own Dialer's requests for ordinary and emergency numbers
    /// go straight through; anything else waits for confirmation.
    fn dial(&self, number: &str) -> zbus::Result<Placed>;
    fn answer(&self, id: u32) -> zbus::Result<()>;
    fn hangup(&self, id: u32) -> zbus::Result<()>;
    /// Put the active call on hold and answer the waiting one, or swap two calls.
    fn swap(&self) -> zbus::Result<()>;
    fn send_dtmf(&self, id: u32, tones: &str) -> zbus::Result<()>;
    /// Merge the active and held calls into a conference.
    fn merge(&self) -> zbus::Result<()>;
    fn set_muted(&self, muted: bool) -> zbus::Result<()>;
    /// Stop the ringer for the ringing call without answering it.
    fn silence(&self) -> zbus::Result<()>;
    fn calls(&self) -> zbus::Result<Vec<CallInfo>>;
    fn recents(&self, limit: u32) -> zbus::Result<Vec<CallRecordInfo>>;
    fn mark_calls_seen(&self) -> zbus::Result<()>;
    #[zbus(property)]
    fn missed(&self) -> zbus::Result<u32>;

    #[zbus(signal)]
    fn call_added(&self, call: CallInfo) -> zbus::Result<()>;
    #[zbus(signal)]
    fn call_changed(&self, call: CallInfo) -> zbus::Result<()>;
    #[zbus(signal)]
    fn call_removed(&self, id: u32) -> zbus::Result<()>;
}

#[proxy(
    interface = "io.github.isaacrowntree.Dolphone1.Messages",
    default_service = "io.github.isaacrowntree.Dolphone",
    default_path = "/io/github/isaacrowntree/Dolphone"
)]
pub trait Messages {
    /// Open Messages with its PIN (Dolphone's own apps only): true when right. While
    /// a PIN is set and Messages is not open, threads, conversations, search and
    /// export are refused to Dolphone's own apps too.
    fn unlock(&self, pin: &str) -> zbus::Result<bool>;
    /// Close Messages again, before its time is up.
    fn lock(&self) -> zbus::Result<()>;
    /// Whether Messages needs the PIN now.
    fn locked(&self) -> zbus::Result<bool>;
    #[zbus(signal)]
    fn lock_changed(&self, locked: bool) -> zbus::Result<()>;
    /// A text. Several numbers joined with commas make a group text, sent as MMS.
    fn send(&self, to: &str, body: &str) -> zbus::Result<Placed>;
    /// An MMS (through mmsd-tng): to one or more numbers, with a text, files, or
    /// both. From an outside program it waits for the owner, with the files named.
    fn send_mms(&self, to: &[&str], body: &str, files: &[&str]) -> zbus::Result<Placed>;
    fn threads(&self) -> zbus::Result<Vec<ThreadInfo>>;
    fn conversation(&self, peer: &str) -> zbus::Result<Vec<MessageInfo>>;
    fn mark_read(&self, peer: &str) -> zbus::Result<()>;
    fn delete_thread(&self, peer: &str) -> zbus::Result<()>;
    /// Send a failed message again.
    fn retry(&self, id: i64) -> zbus::Result<()>;
    /// Messages with every word of `query` (as the start of a word), newest first.
    /// Reading, so it needs the same grant as reading conversations.
    fn search(&self, query: &str, limit: u32) -> zbus::Result<Vec<MessageInfo>>;
    /// Every message as an SMS Backup & Restore XML file, for moving to Android.
    fn export(&self) -> zbus::Result<String>;
    /// Emergency alerts received, newest first.
    fn alerts(&self, limit: u32) -> zbus::Result<Vec<AlertInfo>>;
    #[zbus(property)]
    fn unread(&self) -> zbus::Result<u32>;

    #[zbus(signal)]
    fn alert_received(&self, alert: AlertInfo) -> zbus::Result<()>;

    #[zbus(signal)]
    fn message_added(&self, message: MessageInfo) -> zbus::Result<()>;
    #[zbus(signal)]
    fn message_changed(&self, id: i64, state: u8) -> zbus::Result<()>;
}

#[proxy(
    interface = "io.github.isaacrowntree.Dolphone1.Confirm",
    default_service = "io.github.isaacrowntree.Dolphone",
    default_path = "/io/github/isaacrowntree/Dolphone"
)]
pub trait Confirm {
    fn pending(&self) -> zbus::Result<Vec<Request>>;
    fn confirm(&self, id: u32) -> zbus::Result<Placed>;
    fn deny(&self, id: u32) -> zbus::Result<()>;

    #[zbus(signal)]
    fn requested(&self, request: Request) -> zbus::Result<()>;
    #[zbus(signal)]
    fn resolved(&self, id: u32, confirmed: bool) -> zbus::Result<()>;
}

#[proxy(
    interface = "io.github.isaacrowntree.Dolphone1.Audio",
    default_service = "io.github.isaacrowntree.Dolphone",
    default_path = "/io/github/isaacrowntree/Dolphone"
)]
pub trait Audio {
    fn routes(&self) -> zbus::Result<Vec<RouteInfo>>;
    fn set_route(&self, route: u8) -> zbus::Result<()>;
    /// Move to the next available route: what the in-call Route key does.
    fn cycle_route(&self) -> zbus::Result<u8>;
    #[zbus(property)]
    fn route(&self) -> zbus::Result<u8>;
    /// "ring", "vibrate" or "silent".
    #[zbus(property)]
    fn ringer(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn set_ringer(&self, mode: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    fn routes_changed(&self) -> zbus::Result<()>;
}

#[proxy(
    interface = "io.github.isaacrowntree.Dolphone1.Bluetooth",
    default_service = "io.github.isaacrowntree.Dolphone",
    default_path = "/io/github/isaacrowntree/Dolphone"
)]
pub trait Bluetooth {
    fn devices(&self) -> zbus::Result<Vec<BtDevice>>;
    fn start_scan(&self) -> zbus::Result<()>;
    fn stop_scan(&self) -> zbus::Result<()>;
    fn pair(&self, address: &str) -> zbus::Result<()>;
    fn connect(&self, address: &str) -> zbus::Result<()>;
    fn disconnect(&self, address: &str) -> zbus::Result<()>;
    fn forget(&self, address: &str) -> zbus::Result<()>;
    #[zbus(property)]
    fn powered(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn set_powered(&self, on: bool) -> zbus::Result<()>;
    #[zbus(property)]
    fn available(&self) -> zbus::Result<bool>;

    #[zbus(signal)]
    fn devices_changed(&self) -> zbus::Result<()>;
    /// A device asks to confirm a passkey: shown on the panel, Run accepts.
    #[zbus(signal)]
    fn passkey(&self, address: &str, passkey: u32) -> zbus::Result<()>;
}

#[proxy(
    interface = "io.github.isaacrowntree.Dolphone1.Modem",
    default_service = "io.github.isaacrowntree.Dolphone",
    default_path = "/io/github/isaacrowntree/Dolphone"
)]
pub trait Modem {
    fn status(&self) -> zbus::Result<ModemStatus>;
    /// Start a USSD session (`*100#`). Returns the network's reply.
    fn ussd(&self, code: &str) -> zbus::Result<String>;
    fn ussd_respond(&self, reply: &str) -> zbus::Result<String>;
    fn ussd_cancel(&self) -> zbus::Result<()>;
    /// The modem's one-time setup, checked: (step, "ok"/"change"/"unsupported"/
    /// "error", in words).
    fn setup(&self) -> zbus::Result<Vec<(String, String, String)>>;
    /// Make the setup's changes (saved in the modem, which restarts). Programs other
    /// than Dolphone's own wait for a press on the device.
    fn apply_setup(&self) -> zbus::Result<(Placed, Vec<(String, String, String)>)>;
    /// The networks the modem has been on, newest first: (when, "5G"/"LTE"/"3G"/"2G",
    /// operator, registered). Where you have been, roughly, so it needs a read grant.
    fn cells(&self, limit: u32) -> zbus::Result<Vec<(i64, String, String, bool)>>;
    /// The cells the modem hears now, the serving one first, then its neighbours,
    /// asked of ModemManager each time (nothing is polled while nobody asks).
    /// Cell ids and areas place the phone more closely than the operator does, so
    /// this needs a read grant too. `NotSupported` when the modem (or a
    /// ModemManager older than 1.20) does not report cells.
    fn cell_info(&self) -> zbus::Result<Vec<CellInfo>>;
    /// Unlock the SIM with its PIN. Returns 0 once unlocked, or the tries left after
    /// a wrong PIN. Fails once the SIM is blocked and needs its PUK.
    fn unlock(&self, pin: &str) -> zbus::Result<u32>;

    #[zbus(signal)]
    fn status_changed(&self, status: ModemStatus) -> zbus::Result<()>;
}

/// The address book: `.vcf` files in `~/.local/share/dolphone/contacts`, which any
/// tool may also edit directly. Reading needs the same grant as reading messages;
/// a change from anything but Dolphone's own apps waits for a press on the device,
/// because a contact decides what name a caller shows and whether they ring.
#[proxy(
    interface = "io.github.isaacrowntree.Dolphone1.Contacts",
    default_service = "io.github.isaacrowntree.Dolphone",
    default_path = "/io/github/isaacrowntree/Dolphone"
)]
pub trait Contacts {
    fn list(&self) -> zbus::Result<Vec<ContactInfo>>;
    /// Numbers are (kind, number): kind is "mobile", "home", "work" or anything.
    fn add(&self, name: &str, numbers: Vec<(String, String)>) -> zbus::Result<Placed>;
    fn update(&self, id: &str, name: &str, numbers: Vec<(String, String)>) -> zbus::Result<Placed>;
    fn remove(&self, id: &str) -> zbus::Result<Placed>;
    /// Bring in vCards (an export from Google Contacts or iCloud). Names and
    /// numbers are kept, nothing else; matches merge rather than double.
    fn import(&self, vcards: &str) -> zbus::Result<Placed>;
    /// Copy the phonebook from a paired phone over Bluetooth (PBAP, what car kits
    /// use): no account and no sign-in. Runs in the background; `imported` says
    /// how it went.
    fn import_from_phone(&self, address: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    fn changed(&self) -> zbus::Result<()>;
    /// An import finished: how many were added, merged and already here, or why
    /// it failed.
    #[zbus(signal)]
    fn imported(&self, added: u32, merged: u32, unchanged: u32, error: &str) -> zbus::Result<()>;
}

#[proxy(
    interface = "io.github.isaacrowntree.Dolphone1.Settings",
    default_service = "io.github.isaacrowntree.Dolphone",
    default_path = "/io/github/isaacrowntree/Dolphone"
)]
pub trait Settings {
    fn get(&self, key: &str) -> zbus::Result<String>;
    fn set(&self, key: &str, value: &str) -> zbus::Result<()>;
    fn block(&self, number: &str) -> zbus::Result<()>;
    fn unblock(&self, number: &str) -> zbus::Result<()>;
    fn blocked(&self) -> zbus::Result<Vec<String>>;
    /// Ask for signals in full. Who is calling or texting (and the phone's own
    /// number) goes only to the connections that registered and may read: every
    /// other program on the bus gets the same signals with the numbers masked. True
    /// when this connection now gets them in full; it receives both copies, and
    /// keeps the ones addressed to it.
    fn register(&self) -> zbus::Result<bool>;
    /// Delete every message, call and setting. Asks for confirmation on the device
    /// unless it came from Dolphone's own Settings app.
    fn wipe(&self) -> zbus::Result<Placed>;
    /// Change a setting from another program: it waits for a press on the device
    /// (Dolphone's own apps get it at once). Not the SIP password. For
    /// `read_grants`, "+name" adds one program and "-name" removes one.
    fn request_setting(&self, key: &str, value: &str) -> zbus::Result<Placed>;
    /// Ask for a standing grant to text `numbers` (comma separated, `*` suffix for a
    /// prefix) up to `per_hour` times for `days` days (0 for no end). Waits for the
    /// owner's confirmation on the device, like any outside request.
    fn request_grant(&self, numbers: &str, per_hour: u32, days: u32) -> zbus::Result<Placed>;
    fn grants(&self) -> zbus::Result<Vec<Grant>>;
    fn revoke_grant(&self, id: i64) -> zbus::Result<()>;
    /// What outside programs have asked for, newest first.
    fn audit(&self, limit: u32) -> zbus::Result<Vec<AuditEntry>>;
    /// The device profile in use, and why.
    fn device(&self) -> zbus::Result<DeviceInfo>;
    /// Laptops connected over KDE Connect: (device id, name, paired).
    fn kdeconnect_devices(&self) -> zbus::Result<Vec<(String, String, bool)>>;
    /// Forget a paired laptop.
    fn kdeconnect_unpair(&self, id: &str) -> zbus::Result<()>;
    /// Every device profile known (built in, installed, and the owner's own).
    fn devices(&self) -> zbus::Result<Vec<DeviceInfo>>;
}

/// The keys `Settings.Get` and `Settings.Set` understand, with their defaults.
pub mod keys {
    /// ISO region for numbers without a country code.
    pub const REGION: (&str, &str) = ("region", "AU");
    /// Delete a text from the SIM once it is safely stored.
    pub const DELETE_FROM_SIM: (&str, &str) = ("delete_from_sim", "true");
    /// Vibrate when ringing (in addition to sound, unless the ringer is "vibrate").
    pub const VIBRATE: (&str, &str) = ("vibrate", "true");
    /// Desktop notifications when a graphical session is running.
    pub const DESKTOP_NOTIFY: (&str, &str) = ("desktop_notify", "true");
    /// Hide message bodies in notifications.
    /// On by default: a notification is visible to anyone looking at the screen,
    /// and one-time codes arrive by text.
    pub const PRIVATE_NOTIFY: (&str, &str) = ("private_notify", "true");
    /// Programs (by executable name, comma separated) the owner lets read messages
    /// and the call log, for scripts such as an OTP reader. Empty by default.
    pub const READ_GRANTS: (&str, &str) = ("read_grants", "");
    /// The voicemail number Hold-1 dials; empty means the carrier default for the
    /// SIM's network when phoned knows it.
    pub const VOICEMAIL: (&str, &str) = ("voicemail", "");
    /// Call waiting on the network (applied to the modem when it appears).
    pub const CALL_WAITING: (&str, &str) = ("call_waiting", "true");
    /// Caller ID as the owner last set it with `*31#`/`#31#`: "0" the network's
    /// default, "1" hidden, "2" shown (AT+CLIR's n). Applied when the modem starts,
    /// which forgets it.
    pub const CLIR: (&str, &str) = ("clir", "0");
    /// The Messages PIN, as a salted hash (Set takes the PIN, "" clears it; Get
    /// answers "set" or ""). Only Dolphone's own apps may set it.
    pub const MESSAGES_PIN: (&str, &str) = ("messages_pin", "");
    /// How long Messages stays open after the PIN, in seconds.
    pub const MESSAGES_OPEN_FOR: (&str, &str) = ("messages_open_for", "300");
    /// Ask the network to report when a text is delivered.
    pub const DELIVERY_REPORTS: (&str, &str) = ("delivery_reports", "false");
    /// Speak the caller's name when a call rings (needs espeak-ng).
    pub const SPEAK_CALLER: (&str, &str) = ("speak_caller", "false");
    /// Calls from numbers not in contacts do not ring; they are logged to call back.
    pub const SILENCE_UNKNOWN: (&str, &str) = ("silence_unknown", "false");
    /// A short tone when a text arrives (when the ringer is on).
    pub const TEXT_TONE: (&str, &str) = ("text_tone", "true");
    /// Which emergency alerts to show besides national ones (always on).
    pub const ALERTS_EXTREME: (&str, &str) = ("alerts_extreme", "true");
    pub const ALERTS_SEVERE: (&str, &str) = ("alerts_severe", "true");
    pub const ALERTS_AMBER: (&str, &str) = ("alerts_amber", "true");
    pub const ALERTS_TEST: (&str, &str) = ("alerts_test", "false");
    /// How long a press must last to count as a hold, in milliseconds (300 to 1500):
    /// longer for unsteady hands, shorter for quick ones.
    pub const LONG_PRESS_MS: (&str, &str) = ("long_press_ms", "600");
    /// Keep the modem off 2G (GSM, GPRS, EDGE): 2G's encryption is weak, and a fake
    /// base station can force phones down to it. Costs coverage where only 2G is.
    pub const DISABLE_2G: (&str, &str) = ("disable_2g", "false");
    /// Warn when the modem falls back to 2G.
    pub const WARN_2G: (&str, &str) = ("warn_2g", "true");
    /// Uplink watch: the number to text when the network link goes down and comes
    /// back (empty: off).
    pub const UPLINK_ALERT_TO: (&str, &str) = ("uplink_alert_to", "");
    /// The interface to watch (eth0); empty watches NetworkManager's connectivity.
    pub const UPLINK_INTERFACE: (&str, &str) = ("uplink_interface", "");
    /// Seconds the link must stay down before a text goes.
    pub const UPLINK_GRACE_S: (&str, &str) = ("uplink_grace_s", "60");
    /// KDE Connect: the phone's calls and texts on a laptop's KDE Connect or
    /// GSConnect. Off by default: it listens on the local network (UDP and TCP
    /// 1716). Pairing is always confirmed on the device.
    pub const KDECONNECT: (&str, &str) = ("kdeconnect", "false");
    /// The name laptops see.
    pub const KDECONNECT_NAME: (&str, &str) = ("kdeconnect_name", "Dolphone");
    /// Trust Dolphone's front ends when they run from an AppImage (the panel apps
    /// do). Off trusts only root-owned /usr/bin/dolphone-*: for a machine that uses
    /// only the desktop app, it stops a user-writable mount posing as a front end.
    pub const TRUST_APPIMAGES: (&str, &str) = ("trust_appimages", "true");
    /// Flash the device profile's LEDs while a call rings.
    pub const FLASH_ON_RING: (&str, &str) = ("flash_on_ring", "true");
    /// Play the country's ringback tone locally while an outgoing call rings, for
    /// networks or modems that send silence instead. Busy and congestion tones are
    /// always played locally, after the network has released the call.
    pub const LOCAL_RINGBACK: (&str, &str) = ("local_ringback", "false");
    /// Decline needs a hold of Escape rather than a press, for unsteady hands.
    pub const STEADY_DECLINE: (&str, &str) = ("steady_decline", "false");
    /// Calls over the internet (SIP), through baresip.
    pub const SIP_ENABLED: (&str, &str) = ("sip_enabled", "false");
    /// The account, as a SIP address: `sip:user@provider.example`, optionally with
    /// `;transport=tcp` or `;transport=tls`.
    pub const SIP_ACCOUNT: (&str, &str) = ("sip_account", "");
    /// An outbound proxy, when the provider asks for one: `sip:proxy.example`,
    /// optionally with a port and `;transport=`. Empty sends calls to the
    /// account's own domain.
    pub const SIP_PROXY: (&str, &str) = ("sip_proxy", "");
    /// The account's password. Write-only: it is kept in a file only the owner
    /// can read, and Get answers "set" or "".
    pub const SIP_PASSWORD: (&str, &str) = ("sip_password", "");
    /// When calls go over SIP: "fallback" (when there is no mobile signal) or
    /// "always". Emergency calls never do.
    pub const SIP_USE: (&str, &str) = ("sip_use", "fallback");
    /// The device profile by id, or "auto" to detect it (see Settings.Devices).
    pub const DEVICE_PROFILE: (&str, &str) = ("device_profile", "auto");
    /// Where phoned's AT commands go: "auto" (the device profile's spare AT port
    /// once ModemManager is told to ignore it, otherwise ModemManager's Command),
    /// "modemmanager" (always ModemManager), or a port's path (`/dev/ttyUSB3`).
    pub const AT_PORT: (&str, &str) = ("at_port", "auto");
    /// Speed dial: the number Hold-2 .. Hold-9 calls.
    pub const SPEED: [(&str, &str); 8] = [
        ("speed_2", ""),
        ("speed_3", ""),
        ("speed_4", ""),
        ("speed_5", ""),
        ("speed_6", ""),
        ("speed_7", ""),
        ("speed_8", ""),
        ("speed_9", ""),
    ];
    pub const ALL: &[(&str, &str)] = &[
        REGION,
        DELETE_FROM_SIM,
        VIBRATE,
        DESKTOP_NOTIFY,
        PRIVATE_NOTIFY,
        READ_GRANTS,
        VOICEMAIL,
        CALL_WAITING,
        CLIR,
        MESSAGES_PIN,
        MESSAGES_OPEN_FOR,
        DELIVERY_REPORTS,
        SPEAK_CALLER,
        SILENCE_UNKNOWN,
        TEXT_TONE,
        ALERTS_EXTREME,
        ALERTS_SEVERE,
        ALERTS_AMBER,
        ALERTS_TEST,
        STEADY_DECLINE,
        LOCAL_RINGBACK,
        LONG_PRESS_MS,
        DISABLE_2G,
        WARN_2G,
        UPLINK_ALERT_TO,
        UPLINK_INTERFACE,
        UPLINK_GRACE_S,
        KDECONNECT,
        KDECONNECT_NAME,
        FLASH_ON_RING,
        TRUST_APPIMAGES,
        SIP_ENABLED,
        SIP_ACCOUNT,
        SIP_PROXY,
        SIP_PASSWORD,
        SIP_USE,
        DEVICE_PROFILE,
        AT_PORT,
        SPEED[0],
        SPEED[1],
        SPEED[2],
        SPEED[3],
        SPEED[4],
        SPEED[5],
        SPEED[6],
        SPEED[7],
    ];
}
