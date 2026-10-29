//! ModemManager, as phoned sees it: the proxies for the interfaces it uses, and
//! finding the modem.
//!
//! phoned is ModemManager's only client for Voice and Messaging. NetworkManager owns
//! the data connection and Flipper's 5G Modem app reads signal and SIM, through the
//! same ModemManager, and neither is disturbed by anything here.

use std::collections::HashMap;

use zbus::proxy;
use zbus::wire::{ObjectPath, OwnedObjectPath, Variant};

pub const SERVICE: &str = "org.freedesktop.ModemManager1";
pub const ROOT: &str = "/org/freedesktop/ModemManager1";

pub mod iface {
    pub const MODEM: &str = "org.freedesktop.ModemManager1.Modem";
    pub const VOICE: &str = "org.freedesktop.ModemManager1.Modem.Voice";
    pub const MESSAGING: &str = "org.freedesktop.ModemManager1.Modem.Messaging";
    pub const USSD: &str = "org.freedesktop.ModemManager1.Modem.Modem3gpp.Ussd";
}

/// MMCallState.
pub mod call_state {
    pub const DIALING: i32 = 1;
    pub const RINGING_OUT: i32 = 2;
    pub const RINGING_IN: i32 = 3;
    pub const ACTIVE: i32 = 4;
    pub const HELD: i32 = 5;
    pub const WAITING: i32 = 6;
    pub const TERMINATED: i32 = 7;
}

/// MMSmsState.
pub mod sms_state {
    pub const RECEIVING: u32 = 2;
    pub const RECEIVED: u32 = 3;
}

#[proxy(interface = "org.freedesktop.ModemManager1.Modem", default_service = "org.freedesktop.ModemManager1")]
pub trait Modem {
    fn enable(&self, enable: bool) -> zbus::Result<()>;
    #[zbus(property)]
    fn state(&self) -> zbus::Result<i32>;
    #[zbus(property)]
    fn signal_quality(&self) -> zbus::Result<(u32, bool)>;
    #[zbus(property)]
    fn access_technologies(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn own_numbers(&self) -> zbus::Result<Vec<String>>;
    #[zbus(property)]
    fn model(&self) -> zbus::Result<String>;
}

#[proxy(interface = "org.freedesktop.ModemManager1.Modem.Modem3gpp", default_service = "org.freedesktop.ModemManager1")]
pub trait Modem3gpp {
    #[zbus(property)]
    fn operator_name(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn registration_state(&self) -> zbus::Result<u32>;
}

#[proxy(interface = "org.freedesktop.ModemManager1.Modem.Voice", default_service = "org.freedesktop.ModemManager1")]
pub trait Voice {
    fn create_call(&self, properties: HashMap<&str, Variant<'_>>) -> zbus::Result<OwnedObjectPath>;
    fn delete_call(&self, path: &ObjectPath<'_>) -> zbus::Result<()>;
    fn list_calls(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
    fn hold_and_accept(&self) -> zbus::Result<()>;
    fn hangup_and_accept(&self) -> zbus::Result<()>;
    fn hangup_all(&self) -> zbus::Result<()>;
    #[zbus(signal)]
    fn call_added(&self, path: OwnedObjectPath) -> zbus::Result<()>;
    #[zbus(signal)]
    fn call_deleted(&self, path: OwnedObjectPath) -> zbus::Result<()>;
    #[zbus(property)]
    fn emergency_only(&self) -> zbus::Result<bool>;
}

#[proxy(interface = "org.freedesktop.ModemManager1.Call", default_service = "org.freedesktop.ModemManager1")]
pub trait Call {
    fn start(&self) -> zbus::Result<()>;
    fn accept(&self) -> zbus::Result<()>;
    fn hangup(&self) -> zbus::Result<()>;
    fn send_dtmf(&self, dtmf: &str) -> zbus::Result<()>;
    // Named apart from the State property's change stream, which zbus also calls
    // `receive_state_changed`.
    #[zbus(signal, name = "StateChanged")]
    fn call_state_changed(&self, old: i32, new: i32, reason: i32) -> zbus::Result<()>;
    #[zbus(property)]
    fn state(&self) -> zbus::Result<i32>;
    #[zbus(property)]
    fn direction(&self) -> zbus::Result<i32>;
    #[zbus(property)]
    fn number(&self) -> zbus::Result<String>;
    /// The modem's audio card, when its audio is exposed to the host (USB audio on
    /// the RM520N-GL). Empty when there is no host-side audio to bridge.
    #[zbus(property)]
    fn audio_port(&self) -> zbus::Result<String>;
}

#[proxy(interface = "org.freedesktop.ModemManager1.Modem.Messaging", default_service = "org.freedesktop.ModemManager1")]
pub trait Messaging {
    fn create(&self, properties: HashMap<&str, Variant<'_>>) -> zbus::Result<OwnedObjectPath>;
    fn delete(&self, path: &ObjectPath<'_>) -> zbus::Result<()>;
    fn list(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
    #[zbus(signal)]
    fn added(&self, path: OwnedObjectPath, received: bool) -> zbus::Result<()>;
}

#[proxy(interface = "org.freedesktop.ModemManager1.Sms", default_service = "org.freedesktop.ModemManager1")]
pub trait Sms {
    fn send(&self) -> zbus::Result<()>;
    #[zbus(property)]
    fn state(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn pdu_type(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn number(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn text(&self) -> zbus::Result<String>;
    /// The network's time for a received text.
    #[zbus(property)]
    fn timestamp(&self) -> zbus::Result<String>;
    /// -1 for an ordinary message, 0 for a "flash" message meant to be shown, not kept.
    #[zbus(property)]
    fn class(&self) -> zbus::Result<i32>;
}

#[proxy(
    interface = "org.freedesktop.ModemManager1.Modem.Modem3gpp.Ussd",
    default_service = "org.freedesktop.ModemManager1"
)]
pub trait Ussd {
    fn initiate(&self, command: &str) -> zbus::Result<String>;
    fn respond(&self, response: &str) -> zbus::Result<String>;
    fn cancel(&self) -> zbus::Result<()>;
}

/// A modem, and which of the interfaces phoned needs it has right now. A modem
/// whose SIM is locked, or that is disabled, has the Modem interface but not Voice or
/// Messaging; phoned still reports it, so the owner is told why it will not ring.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub path: OwnedObjectPath,
    pub voice: bool,
    pub messaging: bool,
    pub ussd: bool,
}

/// The first modem with Voice or Messaging, or None when ModemManager has none (or
/// is not running).
pub async fn find(system: &zbus::Connection) -> Option<Found> {
    let om = zbus::fdo::ObjectManagerProxy::builder(system).destination(SERVICE).path(ROOT).build().await.ok()?;
    let objects = om.get_managed_objects().await.ok()?;
    let mut found: Vec<Found> = objects
        .into_iter()
        .filter(|(_, ifaces)| ifaces.keys().any(|k| k.as_str() == iface::MODEM))
        .map(|(path, ifaces)| {
            let has = |name: &str| ifaces.keys().any(|k| k.as_str() == name);
            Found { path, voice: has(iface::VOICE), messaging: has(iface::MESSAGING), ussd: has(iface::USSD) }
        })
        .collect();
    // A modem that can do the job first, then by path for a stable choice.
    found.sort_by(|a, b| {
        (b.voice || b.messaging).cmp(&(a.voice || a.messaging)).then(a.path.as_str().cmp(b.path.as_str()))
    });
    found.into_iter().next()
}

/// MMModemState as a word for the front ends.
pub fn state_label(state: i32) -> &'static str {
    match state {
        -1 => "failed",
        0 | 1 => "starting",
        2 => "locked",
        3 | 4 => "disabled",
        5 => "starting",
        6 | 7 => "searching",
        _ => "ready",
    }
}

/// The status bar's word for ModemManager's access technology bits.
pub fn tech_label(bits: u32) -> &'static str {
    if bits & (1 << 15) != 0 {
        "5G"
    } else if bits & ((1 << 14) | (1 << 16) | (1 << 17)) != 0 {
        "LTE"
    } else if bits & 0x3e0 != 0 {
        "3G"
    } else if bits & 0x1e != 0 {
        "2G"
    } else {
        "--"
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn tech_labels() {
        assert_eq!(super::tech_label((1 << 15) | (1 << 14)), "5G");
        assert_eq!(super::tech_label(1 << 14), "LTE");
        assert_eq!(super::tech_label(1 << 5), "3G");
        assert_eq!(super::tech_label(0), "--");
    }
}
