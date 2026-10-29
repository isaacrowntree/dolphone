//! Call and message state, as the daemon reports it and the apps draw it.
//!
//! These mirror ModemManager's own enums closely enough to convert both ways, but
//! they are ours: the apps never see a ModemManager value, so the fake modem, a
//! real modem and (later) SIP all arrive as the same thing.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum Direction {
    Incoming = 0,
    Outgoing = 1,
}

/// A call's state. `Waiting` is a second incoming call while one is active.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum CallState {
    Dialing = 0,
    RingingOut = 1,
    RingingIn = 2,
    Active = 3,
    Held = 4,
    Waiting = 5,
    Terminated = 6,
}

impl CallState {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Dialing,
            1 => Self::RingingOut,
            2 => Self::RingingIn,
            3 => Self::Active,
            4 => Self::Held,
            5 => Self::Waiting,
            _ => Self::Terminated,
        }
    }

    /// Whether the call is still something the user can act on.
    pub fn live(self) -> bool {
        self != Self::Terminated
    }

    /// Whether this call is asking to be answered.
    pub fn ringing_in(self) -> bool {
        matches!(self, Self::RingingIn | Self::Waiting)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Dialing => "Calling",
            Self::RingingOut => "Ringing",
            Self::RingingIn => "Incoming call",
            Self::Active => "Connected",
            Self::Held => "On hold",
            Self::Waiting => "Call waiting",
            Self::Terminated => "Call ended",
        }
    }
}

/// Where call audio goes. The daemon owns the choice; the Dialer only asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum Route {
    Speaker = 0,
    /// The 3.5 mm jack: a TRRS headset, or headphones/speakers with no mic.
    Wired = 1,
    Bluetooth = 2,
    /// The earpiece, on a phone that has one (held to the ear).
    Earpiece = 3,
}

impl Route {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Wired,
            2 => Self::Bluetooth,
            3 => Self::Earpiece,
            _ => Self::Speaker,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Speaker => "Speaker",
            Self::Wired => "Headset",
            Self::Bluetooth => "Bluetooth",
            Self::Earpiece => "Earpiece",
        }
    }
}

/// How a finished call went, for the call log.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum Outcome {
    Answered = 0,
    Missed = 1,
    Declined = 2,
    Failed = 3,
    /// Outgoing, hung up before the other side answered.
    Cancelled = 4,
    /// Incoming from a blocked number: rejected without ringing.
    Blocked = 5,
    /// Incoming from a number not in contacts, with "silence unknown callers" on:
    /// it did not ring, and is in the log to call back.
    Silenced = 6,
}

impl Outcome {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Answered,
            1 => Self::Missed,
            2 => Self::Declined,
            4 => Self::Cancelled,
            5 => Self::Blocked,
            6 => Self::Silenced,
            _ => Self::Failed,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum MessageState {
    Received = 0,
    Sending = 1,
    Sent = 2,
    Failed = 3,
    /// The network reported it delivered (when delivery reports are on).
    Delivered = 4,
}

impl MessageState {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Received,
            1 => Self::Sending,
            2 => Self::Sent,
            4 => Self::Delivered,
            _ => Self::Failed,
        }
    }
}

/// How serious a cell-broadcast emergency alert is, from its channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum AlertLevel {
    /// National / presidential: always shown, cannot be turned off.
    National = 0,
    Extreme = 1,
    Severe = 2,
    /// A missing child.
    Amber = 3,
    /// Exercises and tests, off unless asked for.
    Test = 4,
    /// Public safety and other information.
    Info = 5,
}

impl AlertLevel {
    /// The 3GPP TS 23.041 channel ranges used by WEA (US), EU-Alert and Australia's
    /// Emergency Alert, and their language variants 12 channels up.
    pub fn of_channel(ch: u32) -> Option<Self> {
        let c = if (4383..=4395).contains(&ch) { ch - 13 } else { ch };
        Some(match c {
            // 919 is the Netherlands' NL-Alert channel from before the 4370
            // range, which the Dutch NL-Alert regulation still names as one a
            // phone must listen on.
            4370 | 919 => Self::National,
            4371 | 4372 => Self::Extreme,
            4373..=4378 => Self::Severe,
            4379 => Self::Amber,
            4380..=4382 => Self::Test,
            4396..=4400 => Self::Info,
            _ => return None,
        })
    }

    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::National,
            1 => Self::Extreme,
            2 => Self::Severe,
            3 => Self::Amber,
            4 => Self::Test,
            _ => Self::Info,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::National => "National alert",
            Self::Extreme => "Extreme alert",
            Self::Severe => "Severe alert",
            Self::Amber => "Amber alert",
            Self::Test => "Test alert",
            Self::Info => "Public information",
        }
    }
}

/// One live call, as the Dialer draws it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Call {
    pub id: u32,
    /// Canonical number, or empty for a withheld caller.
    pub number: String,
    pub direction: Direction,
    pub state: CallState,
    /// When the call became active, or 0 if it never has.
    pub connected_at: i64,
    pub muted: bool,
    pub emergency: bool,
}

/// One stored message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: i64,
    pub peer: String,
    pub direction: Direction,
    pub body: String,
    pub at: i64,
    pub state: MessageState,
    pub read: bool,
    /// Which SIM it went through: the last four digits of its ICCID, or empty
    /// when it is not known (records from before SIMs were noted).
    pub sim: String,
    /// What an MMS carried besides its text: pictures, audio, a contact card.
    pub parts: Vec<Part>,
}

/// One attachment of an MMS, kept as a file beside the store.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Part {
    /// Its MIME type, as the sender gave it: image/jpeg, audio/amr, text/vcard.
    pub mime: String,
    pub path: String,
}

impl Part {
    /// "Picture", "Audio", ... for a line in a thread.
    pub fn kind(&self) -> &'static str {
        kind_of(&self.mime)
    }
}

pub fn kind_of(mime: &str) -> &'static str {
    match mime.split('/').next().unwrap_or("") {
        "image" => "Picture",
        "video" => "Video",
        "audio" => "Audio",
        _ if mime.contains("vcard") => "Contact",
        _ => "Attachment",
    }
}

/// One conversation in the thread list: the last message and how many are unread.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thread {
    pub peer: String,
    pub last_body: String,
    pub last_at: i64,
    pub last_direction: Direction,
    pub unread: u32,
}

/// One finished call, for Recents.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallRecord {
    pub id: i64,
    pub peer: String,
    pub direction: Direction,
    pub started: i64,
    pub duration: i64,
    pub outcome: Outcome,
    pub seen: bool,
    /// Which SIM, as for a message.
    pub sim: String,
}
