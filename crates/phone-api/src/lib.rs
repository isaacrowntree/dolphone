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
//! | `Modem` | network status and USSD |
//! | `Settings` | blocked numbers, preferences, wiping |
//!
//! Enums travel as `u8` and convert to and from [`phone_core::model`]'s types on
//! either side (this crate keeps no dependency on phone-core so a script author can
//! use it alone).

use serde::{Deserialize, Serialize};
use zbus::{proxy, Type};

pub const SERVICE: &str = "io.github.isaacrowntree.Dolphone";
pub const PATH: &str = "/io/github/isaacrowntree/Dolphone";

pub mod iface {
    pub const CALLS: &str = "io.github.isaacrowntree.Dolphone1.Calls";
    pub const MESSAGES: &str = "io.github.isaacrowntree.Dolphone1.Messages";
    pub const CONFIRM: &str = "io.github.isaacrowntree.Dolphone1.Confirm";
    pub const AUDIO: &str = "io.github.isaacrowntree.Dolphone1.Audio";
    pub const BLUETOOTH: &str = "io.github.isaacrowntree.Dolphone1.Bluetooth";
    pub const MODEM: &str = "io.github.isaacrowntree.Dolphone1.Modem";
    pub const SETTINGS: &str = "io.github.isaacrowntree.Dolphone1.Settings";
}

/// Error names, as `phoned` raises them.
pub mod error {
    pub const RATE_LIMITED: &str = "io.github.isaacrowntree.Dolphone1.Error.RateLimited";
    pub const NO_MODEM: &str = "io.github.isaacrowntree.Dolphone1.Error.NoModem";
    pub const NO_SUCH_CALL: &str = "io.github.isaacrowntree.Dolphone1.Error.NoSuchCall";
    pub const INVALID: &str = "io.github.isaacrowntree.Dolphone1.Error.Invalid";
    pub const DENIED: &str = "io.github.isaacrowntree.Dolphone1.Error.Denied";
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
    /// "call", "sms" or "ussd".
    pub kind: String,
    pub target: String,
    /// The text, for an SMS.
    pub body: String,
    /// Who asked: the program's name as the daemon saw it.
    pub requester: String,
    /// Why it needs confirming: "external", "premium", "short", "international",
    /// "emergency".
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
    fn send(&self, to: &str, body: &str) -> zbus::Result<Placed>;
    fn threads(&self) -> zbus::Result<Vec<ThreadInfo>>;
    fn conversation(&self, peer: &str) -> zbus::Result<Vec<MessageInfo>>;
    fn mark_read(&self, peer: &str) -> zbus::Result<()>;
    fn delete_thread(&self, peer: &str) -> zbus::Result<()>;
    #[zbus(property)]
    fn unread(&self) -> zbus::Result<u32>;

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

    #[zbus(signal)]
    fn status_changed(&self, status: ModemStatus) -> zbus::Result<()>;
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
    /// Delete every message, call and setting. Asks for confirmation on the device
    /// unless it came from Dolphone's own Settings app.
    fn wipe(&self) -> zbus::Result<Placed>;
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
    /// The voicemail number Hold-1 dials; empty means ask the carrier's default.
    pub const VOICEMAIL: (&str, &str) = ("voicemail", "");
    pub const ALL: &[(&str, &str)] =
        &[REGION, DELETE_FROM_SIM, VIBRATE, DESKTOP_NOTIFY, PRIVATE_NOTIFY, READ_GRANTS, VOICEMAIL];
}
