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
}

impl Route {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Wired,
            2 => Self::Bluetooth,
            _ => Self::Speaker,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Speaker => "Speaker",
            Self::Wired => "Headset",
            Self::Bluetooth => "Bluetooth",
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
}

impl Outcome {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Answered,
            1 => Self::Missed,
            2 => Self::Declined,
            4 => Self::Cancelled,
            5 => Self::Blocked,
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
}

impl MessageState {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Received,
            1 => Self::Sending,
            2 => Self::Sent,
            _ => Self::Failed,
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
}
