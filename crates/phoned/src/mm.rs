//! ModemManager, as phoned sees it: the proxies for the interfaces it uses, and
//! finding the modem.
//!
//! phoned is ModemManager's only client for Voice and Messaging. NetworkManager owns
//! the data connection and Flipper's 5G Modem app reads signal and SIM, through the
//! same ModemManager, and neither is disturbed by anything here.

use std::collections::HashMap;

use zbus::proxy;
use zbus::wire::{ObjectPath, OwnedObjectPath, OwnedVariant, Variant};

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
    /// The IMEI.
    #[zbus(property)]
    fn equipment_identifier(&self) -> zbus::Result<String>;
    /// An AT command, answered without the final OK. Only in debug mode.
    fn command(&self, cmd: &str, timeout: u32) -> zbus::Result<String>;
    /// (allowed, preferred) MMModemMode pairs the modem accepts.
    #[zbus(property)]
    fn supported_modes(&self) -> zbus::Result<Vec<(u32, u32)>>;
    #[zbus(property)]
    fn current_modes(&self) -> zbus::Result<(u32, u32)>;
    fn set_current_modes(&self, modes: &(u32, u32)) -> zbus::Result<()>;
    /// The serving and neighbouring cells, one dictionary each (ModemManager 1.20;
    /// see [`parse_cells`]). It needs the modem enabled, and a plugin that can ask:
    /// ModemManager's QMI and MBIM modems do (mm-broadband-modem-qmi.c,
    /// mm-broadband-modem-mbim.c); others answer Core.Unsupported.
    fn get_cell_info(&self) -> zbus::Result<Vec<HashMap<String, OwnedVariant>>>;
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
    /// Where a call's audio is, when ModemManager knows: a serial port for a
    /// SIMCom modem (its plugin sets it once the call is up). Empty otherwise,
    /// including a Quectel's USB sound card, which the profile's modem_match finds.
    #[zbus(property)]
    fn audio_port(&self) -> zbus::Result<String>;
    /// The call audio's format, when the modem sends it over a port (a SIM7600:
    /// "rate", 8000 or 16000).
    #[zbus(property)]
    fn audio_format(&self) -> zbus::Result<HashMap<String, zbus::wire::OwnedVariant>>;
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
    fn send_puk(&self, puk: &str, pin: &str) -> zbus::Result<()>;
    fn change_pin(&self, old: &str, new: &str) -> zbus::Result<()>;
    /// MCC and MNC of the SIM's own network, "50501".
    #[zbus(property)]
    fn operator_identifier(&self) -> zbus::Result<String>;
    /// The ICCID.
    #[zbus(property)]
    fn sim_identifier(&self) -> zbus::Result<String>;
}

/// A call's audio sample rate from its AudioFormat property (a{sv} with "rate"),
/// or 0 when the modem says none.
pub async fn audio_rate(call: &CallProxy<'_>) -> u32 {
    match call.audio_format().await {
        Ok(f) => f.get("rate").and_then(|v| u32::try_from(v).ok()).unwrap_or(0),
        Err(_) => 0,
    }
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
    /// ModemManager's connection on the bus. A ModemManager that restarted and
    /// probed the modem again under the same path is a new owner: the calls the
    /// old one had are gone.
    pub owner: String,
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
    let owner = match zbus::fdo::DBusProxy::new(system).await?.get_name_owner(SERVICE.try_into().expect("name")).await {
        Ok(o) => o.to_string(),
        Err(zbus::fdo::Error::NameHasNoOwner(_)) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
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
                owner: owner.clone(),
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

/// MMCellType (ModemManager-enums.h, since 1.20), as `CellInfo::kind`.
pub fn cell_kind(t: u32) -> &'static str {
    match t {
        1 => "cdma",
        2 => "gsm",
        3 => "umts",
        4 => "tdscdma",
        5 => "lte",
        6 => "nr",
        _ => "unknown",
    }
}

/// `Modem.GetCellInfo`'s dictionaries as Dolphone's cells, the serving ones first.
///
/// The keys, from ModemManager's documentation of the method
/// (org.freedesktop.ModemManager1.Modem.xml; libmm-glib's mm-cell-info-*.c write
/// them): every cell has "cell-type" (u, MMCellType) and "serving" (b). Then, all
/// optional, as the firmware fills them:
///
/// - GSM: "operator-id" (s), "lac" and "ci" (s, hex), "arfcn" (u), "rx-level" (u,
///   0..=63), "timing-advance" (u), "base-station-id" (s).
/// - UMTS: "operator-id", "lac", "ci", "uarfcn" (u), "psc" (u), "rscp" and "ecio"
///   (d), "path-loss", "frequency-fdd-ul/-dl", "frequency-tdd" (u).
/// - TD-SCDMA: "operator-id", "lac", "ci", "uarfcn", "cell-parameter-id" (u),
///   "rscp" (d), "timing-advance", "path-loss" (u).
/// - LTE: "operator-id", "tac" and "ci" (s, hex), "physical-ci" (s, hex), "earfcn"
///   (u), "rsrp" and "rsrq" (d), "timing-advance" (u); since 1.22 also
///   "serving-cell-type" and "bandwidth" (u).
/// - 5G NR: as LTE, with "nrarfcn" for "earfcn", and "sinr" (d).
/// - CDMA: "nid", "sid", "base-station-id", "ref-pn" (s), "pilot-strength" (u):
///   kept only as a cell of its kind; Dolphone's modems have no CDMA.
///
/// None of them carries a band. A key of an unexpected type is taken as not
/// given, and a cell type newer than this list as "unknown".
pub fn parse_cells(list: &[HashMap<String, OwnedVariant>]) -> Vec<phone_api::CellInfo> {
    use phone_api::CellInfo;
    let mut cells: Vec<CellInfo> = list
        .iter()
        .map(|d| {
            let s = |k: &str| d.get(k).and_then(|v| String::try_from(v.clone()).ok()).unwrap_or_default();
            let u = |k: &str| d.get(k).and_then(|v| u32::try_from(v).ok()).unwrap_or(CellInfo::UNSET);
            let f = |k: &str| d.get(k).and_then(|v| f64::try_from(v).ok()).unwrap_or(CellInfo::UNSET_DB);
            let kind = cell_kind(d.get("cell-type").and_then(|v| u32::try_from(v).ok()).unwrap_or(0));
            let serving = d.get("serving").and_then(|v| bool::try_from(v).ok()).unwrap_or(false);
            let mut c = CellInfo::new(kind, serving);
            if kind == "unknown" || kind == "cdma" {
                return c;
            }
            c.operator_id = s("operator-id");
            c.ci = s("ci");
            c.timing_advance = u("timing-advance");
            match kind {
                "gsm" => {
                    c.area = s("lac");
                    c.channel = u("arfcn");
                    c.rx_level = u("rx-level");
                }
                "umts" | "tdscdma" => {
                    c.area = s("lac");
                    c.channel = u("uarfcn");
                    c.psc = u("psc");
                    c.rscp = f("rscp");
                    c.ecio = f("ecio");
                }
                _ => {
                    c.area = s("tac");
                    c.channel = u(if kind == "nr" { "nrarfcn" } else { "earfcn" });
                    c.pci = u32::from_str_radix(&s("physical-ci"), 16).unwrap_or(CellInfo::UNSET);
                    c.rsrp = f("rsrp");
                    c.rsrq = f("rsrq");
                    c.sinr = f("sinr");
                    c.serving_cell_type = u("serving-cell-type");
                    c.bandwidth = u("bandwidth");
                }
            }
            c
        })
        .collect();
    // Stable: ModemManager's own order among serving cells (the primary first, as
    // the QMI and MBIM code builds the list) and among neighbours is kept.
    cells.sort_by_key(|c| !c.serving);
    cells
}

/// Whether ModemManager said this modem cannot report cells: its plugin has no
/// way to (Core.Unsupported, mm-iface-modem.c), or it is older than 1.20 and has
/// no such method.
pub fn no_cell_info(e: &zbus::Error) -> bool {
    match e {
        zbus::Error::MethodError(name, _, _) => matches!(
            name.as_str(),
            "org.freedesktop.ModemManager1.Error.Core.Unsupported" | "org.freedesktop.DBus.Error.UnknownMethod"
        ),
        zbus::Error::FDO(f) => matches!(**f, zbus::fdo::Error::UnknownMethod(_) | zbus::fdo::Error::NotSupported(_)),
        _ => false,
    }
}

/// Why an AT command did not go through, in words an owner can act on.
pub fn explain_at(e: &zbus::Error) -> String {
    let s = e.to_string();
    if s.contains("Unauthorized") || s.contains("debug mode") {
        "ModemManager passes AT commands only in debug mode: sudo dolphone-modem-access on \
         (or, where the device profile names a spare AT port, sudo dolphone-modem-access port on)"
            .into()
    } else {
        s
    }
}

/// Send one AT command and return the modem's answer, without the final OK; ERROR
/// is an error. It goes on phoned's own AT port when there is one (crate::atport),
/// and otherwise through ModemManager, which allows it only when it runs with
/// `--debug` (mm-iface-modem.c, handle_command); docs/HARDWARE.md has both.
pub async fn at(system: &zbus::Connection, modem: &OwnedObjectPath, cmd: &str) -> zbus::Result<String> {
    if let Some(answer) = crate::atport::command(cmd).await {
        let out = answer.map_err(zbus::Error::Failure)?;
        tracing::info!(cmd, answer = %out.trim(), "AT (own port)");
        return Ok(out);
    }
    let m = ModemProxy::builder(system).path(modem.clone()).build().await?;
    let out = m.command(cmd, 10).await?;
    tracing::info!(cmd, answer = %out.trim(), "AT");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{parse_cells, HashMap, OwnedVariant};
    use phone_api::CellInfo;
    use zbus::wire::Variant as V;

    #[test]
    fn tech_labels() {
        assert_eq!(super::tech_label((1 << 15) | (1 << 14)), "5G");
        assert_eq!(super::tech_label(1 << 14), "LTE");
        assert_eq!(super::tech_label(1 << 5), "3G");
        assert_eq!(super::tech_label(0), "--");
    }

    fn dict(pairs: &[(&str, zbus::wire::Variant<'static>)]) -> HashMap<String, OwnedVariant> {
        pairs.iter().map(|(k, v)| (k.to_string(), OwnedVariant::try_from(v.clone()).expect("owned"))).collect()
    }

    #[test]
    fn cells_of_every_kind() {
        let list = [
            // A neighbour first: the serving cell still comes out first.
            dict(&[
                ("cell-type", V::from(5u32)),
                ("serving", V::from(false)),
                ("physical-ci", V::from("25")),
                ("earfcn", V::from(1275u32)),
                ("rsrp", V::from(-104.0)),
                ("rsrq", V::from(-14.5)),
            ]),
            dict(&[
                ("cell-type", V::from(5u32)),
                ("serving", V::from(true)),
                ("operator-id", V::from("50501")),
                ("tac", V::from("3010")),
                ("ci", V::from("8A1C02")),
                ("physical-ci", V::from("1A0")),
                ("earfcn", V::from(1275u32)),
                ("rsrp", V::from(-95.0)),
                ("rsrq", V::from(-11.0)),
                ("timing-advance", V::from(3u32)),
                ("serving-cell-type", V::from(1u32)),
                ("bandwidth", V::from(20000u32)),
            ]),
            dict(&[
                ("cell-type", V::from(6u32)),
                ("serving", V::from(true)),
                ("tac", V::from("3010")),
                ("ci", V::from("1B2C3D4E5")),
                ("physical-ci", V::from("3E7")),
                ("nrarfcn", V::from(643334u32)),
                ("rsrp", V::from(-88.0)),
                ("rsrq", V::from(-10.0)),
                ("sinr", V::from(17.0)),
            ]),
            dict(&[
                ("cell-type", V::from(3u32)),
                ("serving", V::from(false)),
                ("operator-id", V::from("50501")),
                ("lac", V::from("84CD")),
                ("ci", V::from("2BAF")),
                ("uarfcn", V::from(10838u32)),
                ("psc", V::from(301u32)),
                ("rscp", V::from(-90.0)),
                ("ecio", V::from(-7.0)),
            ]),
            dict(&[
                ("cell-type", V::from(2u32)),
                ("serving", V::from(false)),
                ("lac", V::from("84CD")),
                ("ci", V::from("2BAF")),
                ("arfcn", V::from(62u32)),
                ("rx-level", V::from(40u32)),
            ]),
        ];
        let cells = parse_cells(&list);
        let lte = &cells[0];
        assert!(lte.serving && lte.kind == "lte", "serving first: {cells:?}");
        assert_eq!((lte.operator_id.as_str(), lte.area.as_str(), lte.ci.as_str()), ("50501", "3010", "8A1C02"));
        assert_eq!((lte.pci, lte.channel, lte.timing_advance), (416, 1275, 3), "the PCI from hex");
        assert_eq!((lte.rsrp, lte.rsrq), (-95.0, -11.0));
        assert_eq!((lte.serving_cell_type, lte.bandwidth), (1, 20000));
        assert_eq!(lte.sinr, CellInfo::UNSET_DB, "ModemManager has no SINR for LTE");
        assert_eq!(lte.strength(), "-95 dBm");
        assert_eq!(
            lte.describe(),
            "LTE 50501 TAC 3010 CI 8A1C02 PCI 416 EARFCN 1275 RSRP -95 dBm RSRQ -11 dB TA 3 bandwidth 20000"
        );
        let nr = &cells[1];
        assert_eq!((nr.kind.as_str(), nr.pci, nr.channel, nr.sinr), ("nr", 999, 643334, 17.0));
        assert_eq!((nr.tech(), nr.channel_name(), nr.area_name()), ("5G NR", "NR-ARFCN", "TAC"));
        let lte_n = &cells[2];
        assert!(!lte_n.serving && lte_n.pci == 37 && lte_n.ci.is_empty());
        assert_eq!(CellInfo::db(lte_n.rsrq).as_deref(), Some("-14.5"));
        let umts = &cells[3];
        assert_eq!(
            (umts.area.as_str(), umts.channel, umts.psc, umts.rscp, umts.ecio),
            ("84CD", 10838, 301, -90.0, -7.0)
        );
        assert_eq!((umts.channel_name(), umts.area_name(), umts.strength().as_str()), ("UARFCN", "LAC", "-90 dBm"));
        assert_eq!(umts.pci, CellInfo::UNSET);
        let gsm = &cells[4];
        assert_eq!((gsm.kind.as_str(), gsm.channel, gsm.rx_level, gsm.strength().as_str()), ("gsm", 62, 40, "RX 40"));
    }

    #[test]
    fn cells_with_little_or_nothing() {
        let cells = parse_cells(&[
            // Only the two keys ModemManager always gives.
            dict(&[("cell-type", V::from(5u32)), ("serving", V::from(true))]),
            // A type newer than Dolphone, with keys it does not know.
            dict(&[("cell-type", V::from(9u32)), ("serving", V::from(false)), ("ci", V::from("1"))]),
            // A key of the wrong type, and no serving flag at all.
            dict(&[("cell-type", V::from(5u32)), ("earfcn", V::from("1275")), ("rsrp", V::from(-95i32))]),
        ]);
        assert_eq!(cells.len(), 3);
        assert_eq!(cells[0], CellInfo::new("lte", true));
        assert_eq!(cells[0].strength(), "");
        assert_eq!(cells[1], CellInfo::new("unknown", false), "kept, with nothing guessed");
        assert_eq!(cells[1].tech(), "Unknown");
        assert_eq!(cells[2], CellInfo::new("lte", false), "wrong types are not given");
        assert!(parse_cells(&[]).is_empty());
    }

    #[test]
    fn cells_travel_on_dbus_whole() {
        // Every field goes on the wire, given or not: a struct that left some out
        // would not match its signature, and the bus would drop phoned for it.
        let cells = vec![CellInfo { rsrp: -95.0, ..CellInfo::new("lte", true) }, CellInfo::new("nr", false)];
        let ctx = zbus::wire::serialized::Context::new(zbus::wire::LE, 0);
        let bytes = zbus::wire::to_bytes(ctx, &cells).unwrap();
        let (back, _): (Vec<CellInfo>, _) = bytes.deserialize().unwrap();
        assert_eq!(back, cells);
    }

    #[test]
    fn no_cell_info_errors() {
        use zbus::Error;
        let named = |n: &'static str| {
            Error::MethodError(n.try_into().unwrap(), Some("Cannot get cell info: operation not supported".into()), {
                zbus::message::Message::method_call("/", "X").build(&()).unwrap()
            })
        };
        assert!(super::no_cell_info(&named("org.freedesktop.ModemManager1.Error.Core.Unsupported")));
        assert!(super::no_cell_info(&named("org.freedesktop.DBus.Error.UnknownMethod")));
        assert!(!super::no_cell_info(&named("org.freedesktop.ModemManager1.Error.Core.WrongState")));
    }

    #[test]
    fn modes_without_2g() {
        let supported = [(2 | 4 | 8, 8), (4 | 8, 8), (8, 0), (2 | 4, 4)];
        assert_eq!(super::choose_modes(&supported, false), Some((2 | 4 | 8, 8)), "everything, 4G preferred");
        assert_eq!(super::choose_modes(&supported, true), Some((4 | 8, 8)), "3G and 4G, not 2G alone");
        assert_eq!(super::choose_modes(&[(2, 0)], true), None, "a 2G-only modem has no such choice");
    }
}
