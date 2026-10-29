//! The fake network's state, shared by every interface.

use std::sync::Arc;

use tokio::sync::Mutex;

/// ModemManager's MMCallState values.
pub mod call_state {
    pub const UNKNOWN: i32 = 0;
    pub const DIALING: i32 = 1;
    pub const RINGING_OUT: i32 = 2;
    pub const RINGING_IN: i32 = 3;
    pub const ACTIVE: i32 = 4;
    pub const HELD: i32 = 5;
    pub const WAITING: i32 = 6;
    pub const TERMINATED: i32 = 7;
}

/// MMCallStateReason values used here.
pub mod reason {
    pub const UNKNOWN: i32 = 0;
    pub const OUTGOING_STARTED: i32 = 1;
    pub const INCOMING_NEW: i32 = 2;
    pub const ACCEPTED: i32 = 3;
    pub const TERMINATED: i32 = 4;
    pub const REFUSED_OR_BUSY: i32 = 5;
}

/// MMSmsState values.
pub mod sms_state {
    pub const UNKNOWN: u32 = 0;
    pub const STORED: u32 = 1;
    pub const RECEIVED: u32 = 3;
    pub const SENDING: u32 = 4;
    pub const SENT: u32 = 5;
}

/// MMModemAccessTechnology bits.
pub mod tech {
    pub const UMTS: u32 = 1 << 5;
    pub const LTE: u32 = 1 << 14;
    pub const NR5G: u32 = 1 << 15;

    pub fn from_label(label: &str) -> u32 {
        match label.to_ascii_lowercase().as_str() {
            "5g" | "5gnr" | "nr" => NR5G | LTE,
            "lte" | "4g" => LTE,
            "3g" | "umts" => UMTS,
            _ => 0,
        }
    }
}

pub struct World {
    pub conn: Option<zbus::Connection>,
    pub next_call: u32,
    pub next_sms: u32,
    /// Live call object paths, in creation order.
    pub calls: Vec<String>,
    /// SMS object paths held by the modem.
    pub sms: Vec<String>,
    pub quality: u32,
    pub tech: u32,
    pub registered: bool,
    pub roaming: bool,
    pub operator: String,
    pub own_number: String,
    /// Outgoing calls are answered by the far end after this many milliseconds, or
    /// left ringing when 0.
    pub auto_answer_ms: u64,
    /// Outgoing texts fail instead of sending.
    pub send_fails: bool,
    pub ussd_reply: String,
    pub ussd_state: u32,
}

pub type Shared = Arc<Mutex<World>>;

impl World {
    pub fn shared() -> Shared {
        Arc::new(Mutex::new(Self {
            conn: None,
            next_call: 1,
            next_sms: 1,
            calls: Vec::new(),
            sms: Vec::new(),
            quality: 72,
            tech: tech::LTE,
            registered: true,
            roaming: false,
            operator: "Fakecom AU".into(),
            // From the ACMA's range reserved for drama and examples.
            own_number: "+61491570110".into(),
            auto_answer_ms: 2500,
            send_fails: false,
            ussd_reply: "Your balance is $12.34. Expires 30/11.".into(),
            ussd_state: 1,
        }))
    }

    pub fn call_path(&mut self) -> String {
        let p = format!("/org/freedesktop/ModemManager1/Call/{}", self.next_call);
        self.next_call += 1;
        p
    }

    pub fn sms_path(&mut self) -> String {
        let p = format!("/org/freedesktop/ModemManager1/SMS/{}", self.next_sms);
        self.next_sms += 1;
        p
    }
}

/// The modem's State: registered (8) or searching (7).
pub fn modem_state(w: &World) -> i32 {
    if w.registered {
        8
    } else {
        7
    }
}
