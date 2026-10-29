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
    pub const CELL_BROADCAST: &str = "org.freedesktop.ModemManager1.Modem.CellBroadcast";
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

/// MMModemLock for the SIM PIN, the key of Modem.UnlockRetries.
pub const LOCK_SIM_PIN: u32 = 2;

/// MMCallStateReason, for why a call ended.
pub mod call_reason {
    pub const REFUSED_OR_BUSY: i32 = 5;
    pub const ERROR: i32 = 6;
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
    fn sim(&self) -> zbus::Result<OwnedObjectPath>;
    #[zbus(property)]
    fn unlock_retries(&self) -> zbus::Result<HashMap<u32, u32>>;
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
    #[zbus(property)]
    fn manufacturer(&self) -> zbus::Result<String>;
    /// An AT command, answered without the final OK. Only in debug mode.
    fn command(&self, cmd: &str, timeout: u32) -> zbus::Result<String>;
    /// (allowed, preferred) MMModemMode pairs the modem accepts.
    #[zbus(property)]
    fn supported_modes(&self) -> zbus::Result<Vec<(u32, u32)>>;
    #[zbus(property)]
    fn current_modes(&self) -> zbus::Result<(u32, u32)>;
    fn set_current_modes(&self, modes: &(u32, u32)) -> zbus::Result<()>;
}

/// MMModemMode bits (ModemManager-enums.h).
pub mod mode {
    pub const MODE_2G: u32 = 1 << 1;
    pub const MODE_4G: u32 = 1 << 3;
    pub const MODE_5G: u32 = 1 << 4;
}

/// The supported (allowed, preferred) pair to use: without 2G when `no_2g`, and
/// otherwise the one allowing the most; among equals, one that prefers the fastest.
pub fn choose_modes(supported: &[(u32, u32)], no_2g: bool) -> Option<(u32, u32)> {
    supported.iter().copied().filter(|(allowed, _)| !no_2g || allowed & mode::MODE_2G == 0).max_by_key(
        |(allowed, preferred)| (allowed.count_ones(), *allowed & (mode::MODE_4G | mode::MODE_5G), *preferred),
    )
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
    fn call_waiting_setup(&self, enable: bool) -> zbus::Result<()>;
    fn call_waiting_query(&self) -> zbus::Result<bool>;
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
    fn join_multiparty(&self) -> zbus::Result<()>;
    #[zbus(property)]
    fn multiparty(&self) -> zbus::Result<bool>;
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
    /// MMSmsDeliveryState: 0 delivered; 0x100 not known yet; others are failures.
    #[zbus(property)]
    fn delivery_state(&self) -> zbus::Result<u32>;
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

#[proxy(interface = "org.freedesktop.ModemManager1.Sim", default_service = "org.freedesktop.ModemManager1")]
pub trait Sim {
    fn send_pin(&self, pin: &str) -> zbus::Result<()>;
    /// MCC and MNC of the SIM's own network, "50501".
    #[zbus(property)]
    fn operator_identifier(&self) -> zbus::Result<String>;
    /// The ICCID.
    #[zbus(property)]
    fn sim_identifier(&self) -> zbus::Result<String>;
}

/// Cell broadcast, in its own module: its Added signal would otherwise share
/// generated type names with Messaging's.
pub mod cb {
    use zbus::proxy;
    use zbus::wire::{ObjectPath, OwnedObjectPath};

    #[proxy(
        interface = "org.freedesktop.ModemManager1.Modem.CellBroadcast",
        default_service = "org.freedesktop.ModemManager1"
    )]
    pub trait CellBroadcast {
        fn list(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
        fn delete(&self, path: &ObjectPath<'_>) -> zbus::Result<()>;
        fn set_channels(&self, channels: &[(u32, u32)]) -> zbus::Result<()>;
        #[zbus(signal)]
        fn added(&self, path: OwnedObjectPath) -> zbus::Result<()>;
    }

    #[proxy(interface = "org.freedesktop.ModemManager1.Cbm", default_service = "org.freedesktop.ModemManager1")]
    pub trait Cbm {
        #[zbus(property)]
        fn text(&self) -> zbus::Result<String>;
        #[zbus(property)]
        fn channel(&self) -> zbus::Result<u32>;
        /// The alert's serial: a rebroadcast of the same alert carries the same one.
        #[zbus(property)]
        fn message_code(&self) -> zbus::Result<u32>;
    }
}
pub use cb::{CbmProxy, CellBroadcastProxy};

/// A modem, and which of the interfaces phoned needs it has right now. A modem
/// whose SIM is locked, or that is disabled, has the Modem interface but not Voice or
/// Messaging; phoned still reports it, so the owner is told why it will not ring.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub path: OwnedObjectPath,
    pub voice: bool,
    pub messaging: bool,
    pub ussd: bool,
    pub broadcast: bool,
}

/// The first modem with Voice or Messaging, or None when ModemManager has none (or
/// is not running).
pub async fn find(system: &zbus::Connection) -> Option<Found> {
    lookup(system).await.ok().flatten()
}

/// The modem, or None when ModemManager has none or is not running; an error when
/// the question itself failed (a timeout on a busy bus), which says nothing about
/// the modem and must not be taken as it leaving: that would end its calls.
pub async fn lookup(system: &zbus::Connection) -> zbus::Result<Option<Found>> {
    let om = zbus::fdo::ObjectManagerProxy::builder(system).destination(SERVICE).path(ROOT).build().await?;
    let objects = match om.get_managed_objects().await {
        Ok(o) => o,
        Err(zbus::fdo::Error::ServiceUnknown(_) | zbus::fdo::Error::NameHasNoOwner(_)) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut found: Vec<Found> = objects
        .into_iter()
        .filter(|(_, ifaces)| ifaces.keys().any(|k| k.as_str() == iface::MODEM))
        .map(|(path, ifaces)| {
            let has = |name: &str| ifaces.keys().any(|k| k.as_str() == name);
            Found {
                path,
                voice: has(iface::VOICE),
                messaging: has(iface::MESSAGING),
                ussd: has(iface::USSD),
                broadcast: has(iface::CELL_BROADCAST),
            }
        })
        .collect();
    // A modem that can do the job first, then by path for a stable choice.
    found.sort_by(|a, b| {
        (b.voice || b.messaging).cmp(&(a.voice || a.messaging)).then(a.path.as_str().cmp(b.path.as_str()))
    });
    Ok(found.into_iter().next())
}

/// The emergency-alert channels a phone listens to (3GPP TS 23.041): national
/// through public-safety alerts, their language variants, and the EU-Alert and
/// Australian ranges, which share them.
pub const ALERT_CHANNELS: &[(u32, u32)] = &[(4370, 4400), (919, 919)];

/// The home region for a SIM's mobile country code, for reading numbers written
/// without a country code. A short list of the countries Dolphone is likeliest to
/// meet; anything else keeps the setting.
pub fn region_for_mcc(mcc: &str) -> Option<&'static str> {
    Some(match mcc {
        "505" => "AU",
        "530" => "NZ",
        "234" | "235" => "GB",
        "272" => "IE",
        "310" | "311" | "312" | "313" | "314" | "315" | "316" => "US",
        "302" => "CA",
        "262" => "DE",
        "208" => "FR",
        "204" => "NL",
        "214" => "ES",
        "222" => "IT",
        "404" | "405" => "IN",
        "525" => "SG",
        "440" | "441" => "JP",
        "450" => "KR",
        _ => return None,
    })
}

/// The carrier's voicemail number for a SIM's MCC+MNC, where it is well known.
pub fn voicemail_for(mccmnc: &str) -> Option<&'static str> {
    Some(match mccmnc {
        "50501" | "50571" | "50572" => "101", // Telstra
        "50502" | "50590" => "321",           // Optus
        "50503" | "50506" => "121",           // Vodafone AU
        "53001" => "+6421000",                // One NZ
        "23410" => "901",                     // O2 UK
        "23415" => "121",                     // Vodafone UK
        "23430" | "23433" => "123",           // EE
        _ => return None,
    })
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

/// Why an AT command did not go through, in words an owner can act on.
pub fn explain_at(e: &zbus::Error) -> String {
    let s = e.to_string();
    if s.contains("Unauthorized") || s.contains("debug mode") {
        "ModemManager passes AT commands only in debug mode: sudo dolphone-modem-access on".into()
    } else {
        s
    }
}

/// Send one AT command through ModemManager and return the modem's answer
/// (ModemManager drops the final OK and turns ERROR into an error). It allows this
/// only when it runs with `--debug` (mm-iface-modem.c, handle_command);
/// docs/HARDWARE.md has the drop-in.
pub async fn at(system: &zbus::Connection, modem: &OwnedObjectPath, cmd: &str) -> zbus::Result<String> {
    let m = ModemProxy::builder(system).path(modem.clone()).build().await?;
    let out = m.command(cmd, 10).await?;
    tracing::info!(cmd, answer = %out.trim(), "AT");
    Ok(out)
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

    #[test]
    fn modes_without_2g() {
        let supported = [(2 | 4 | 8, 8), (4 | 8, 8), (8, 0), (2 | 4, 4)];
        assert_eq!(super::choose_modes(&supported, false), Some((2 | 4 | 8, 8)), "everything, 4G preferred");
        assert_eq!(super::choose_modes(&supported, true), Some((4 | 8, 8)), "3G and 4G, not 2G alone");
        assert_eq!(super::choose_modes(&[(2, 0)], true), None, "a 2G-only modem has no such choice");
    }
}
