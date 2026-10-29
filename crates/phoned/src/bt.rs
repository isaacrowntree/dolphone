//! Bluetooth headsets and speakers, through BlueZ.
//!
//! phoned does only what a phone's Bluetooth screen does: power, scan, pair, connect,
//! forget. The audio side is PipeWire's: once a headset is connected its sink and
//! source appear and the call audio follows (see audio.rs). Call control from the
//! headset's own button arrives as an input event (see jack.rs), or through
//! PipeWire's ModemManager integration when the image enables it.
//!
//! Pairing needs an agent. phoned registers one with the "DisplayYesNo" capability
//! and accepts numeric comparison only for a pairing the owner started from a
//! Dolphone front end in the last minute, showing the passkey there as it does.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use phone_api::BtDevice;
use zbus::wire::{OwnedObjectPath, OwnedVariant};
use zbus::{interface, proxy};

pub const SERVICE: &str = "org.bluez";
pub const AGENT_PATH: &str = "/io/github/isaacrowntree/Dolphone/agent";

// The service UUIDs that say what a device can do.
const HFP_HF: &str = "0000111e-0000-1000-8000-00805f9b34fb";
const HSP_HS: &str = "00001108-0000-1000-8000-00805f9b34fb";
const A2DP_SINK: &str = "0000110b-0000-1000-8000-00805f9b34fb";

#[proxy(interface = "org.bluez.Adapter1", default_service = "org.bluez")]
pub trait Adapter1 {
    fn start_discovery(&self) -> zbus::Result<()>;
    fn stop_discovery(&self) -> zbus::Result<()>;
    fn remove_device(&self, device: &zbus::wire::ObjectPath<'_>) -> zbus::Result<()>;
    #[zbus(property)]
    fn powered(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn set_powered(&self, on: bool) -> zbus::Result<()>;
}

#[proxy(interface = "org.bluez.Device1", default_service = "org.bluez")]
pub trait Device1 {
    fn pair(&self) -> zbus::Result<()>;
    fn connect(&self) -> zbus::Result<()>;
    fn disconnect(&self) -> zbus::Result<()>;
    #[zbus(property)]
    fn set_trusted(&self, trusted: bool) -> zbus::Result<()>;
}

#[proxy(interface = "org.bluez.AgentManager1", default_service = "org.bluez", default_path = "/org/bluez")]
pub trait AgentManager1 {
    fn register_agent(&self, agent: &zbus::wire::ObjectPath<'_>, capability: &str) -> zbus::Result<()>;
    fn request_default_agent(&self, agent: &zbus::wire::ObjectPath<'_>) -> zbus::Result<()>;
}

#[derive(Clone)]
pub struct Bt {
    system: zbus::Connection,
    session: zbus::Connection,
    /// When the owner last pressed Pair, so the agent knows a request is wanted.
    pairing: Arc<Mutex<Option<Instant>>>,
}

type Objects = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedVariant>>>;

impl Bt {
    pub fn new(system: zbus::Connection, session: zbus::Connection) -> Self {
        Self { system, session, pairing: Arc::new(Mutex::new(None)) }
    }

    async fn objects(&self) -> Option<Objects> {
        let om =
            zbus::fdo::ObjectManagerProxy::builder(&self.system).destination(SERVICE).path("/").build().await.ok()?;
        let objs = om.get_managed_objects().await.ok()?;
        Some(
            objs.into_iter()
                .map(|(p, ifaces)| {
                    let ifaces = ifaces
                        .into_iter()
                        .map(|(name, props)| {
                            (name.to_string(), props.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
                        })
                        .collect();
                    (p, ifaces)
                })
                .collect(),
        )
    }

    async fn adapter(&self) -> Option<OwnedObjectPath> {
        let objs = self.objects().await?;
        let mut adapters: Vec<OwnedObjectPath> =
            objs.into_iter().filter(|(_, i)| i.contains_key("org.bluez.Adapter1")).map(|(p, _)| p).collect();
        adapters.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        adapters.into_iter().next()
    }

    async fn device_path(&self, address: &str) -> zbus::fdo::Result<OwnedObjectPath> {
        let objs = self.objects().await.ok_or_else(unavailable)?;
        objs.into_iter()
            .find(|(_, i)| {
                i.get("org.bluez.Device1")
                    .and_then(|p| p.get("Address"))
                    .and_then(|v| String::try_from(v.clone()).ok())
                    .is_some_and(|a| a.eq_ignore_ascii_case(address))
            })
            .map(|(p, _)| p)
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs(format!("no device {address}")))
    }

    pub async fn available(&self) -> bool {
        self.adapter().await.is_some()
    }

    pub async fn powered(&self) -> bool {
        let Some(a) = self.adapter().await else { return false };
        match Adapter1Proxy::builder(&self.system).path(a).build().await {
            Ok(p) => p.powered().await.unwrap_or(false),
            Err(_) => false,
        }
    }

    pub async fn set_powered(&self, on: bool) -> zbus::fdo::Result<()> {
        let a = self.adapter().await.ok_or_else(unavailable)?;
        Adapter1Proxy::builder(&self.system).path(a).build().await?.set_powered(on).await?;
        Ok(())
    }

    pub async fn scan(&self, on: bool) -> zbus::fdo::Result<()> {
        let a = self.adapter().await.ok_or_else(unavailable)?;
        let p = Adapter1Proxy::builder(&self.system).path(a).build().await?;
        if on {
            p.start_discovery().await?;
            // Discovery is not left running: it slows Wi-Fi on a shared radio.
            let p2 = p.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(30)).await;
                let _ = p2.stop_discovery().await;
            });
        } else {
            p.stop_discovery().await?;
        }
        Ok(())
    }

    /// Every device BlueZ knows: paired ones first, then what a scan has seen.
    pub async fn devices(&self) -> Vec<BtDevice> {
        let Some(objs) = self.objects().await else { return Vec::new() };
        let mut out: Vec<BtDevice> = objs
            .values()
            .filter_map(|i| i.get("org.bluez.Device1"))
            .map(|p| {
                let s = |k: &str| p.get(k).and_then(|v| String::try_from(v.clone()).ok()).unwrap_or_default();
                let b = |k: &str| p.get(k).and_then(|v| bool::try_from(v.clone()).ok()).unwrap_or(false);
                let uuids: Vec<String> =
                    p.get("UUIDs").and_then(|v| Vec::<String>::try_from(v.clone()).ok()).unwrap_or_default();
                let alias = s("Alias");
                BtDevice {
                    address: s("Address"),
                    name: if alias.is_empty() { s("Name") } else { alias },
                    paired: b("Paired"),
                    connected: b("Connected"),
                    headset: uuids.iter().any(|u| u == HFP_HF || u == HSP_HS),
                    speaker: uuids.iter().any(|u| u == A2DP_SINK),
                }
            })
            // A phone's list is audio devices; a keyboard is the desktop's business.
            .filter(|d| d.headset || d.speaker || !d.paired)
            .collect();
        out.sort_by(|a, b| b.paired.cmp(&a.paired).then(b.connected.cmp(&a.connected)).then(a.name.cmp(&b.name)));
        out
    }

    pub async fn pair(&self, address: &str) -> zbus::fdo::Result<()> {
        let path = self.device_path(address).await?;
        if let Ok(mut p) = self.pairing.lock() {
            *p = Some(Instant::now());
        }
        let d = Device1Proxy::builder(&self.system).path(path).build().await?;
        d.pair().await?;
        let _ = d.set_trusted(true).await;
        let _ = d.connect().await;
        crate::api::emit_devices_changed(&self.session).await;
        Ok(())
    }

    pub async fn connect(&self, address: &str, on: bool) -> zbus::fdo::Result<()> {
        let path = self.device_path(address).await?;
        let d = Device1Proxy::builder(&self.system).path(path).build().await?;
        if on {
            d.connect().await?;
        } else {
            d.disconnect().await?;
        }
        crate::api::emit_devices_changed(&self.session).await;
        Ok(())
    }

    pub async fn forget(&self, address: &str) -> zbus::fdo::Result<()> {
        let path = self.device_path(address).await?;
        let a = self.adapter().await.ok_or_else(unavailable)?;
        Adapter1Proxy::builder(&self.system).path(a).build().await?.remove_device(&path.as_ref()).await?;
        crate::api::emit_devices_changed(&self.session).await;
        Ok(())
    }

    fn pairing_wanted(&self) -> bool {
        self.pairing.lock().ok().and_then(|p| *p).is_some_and(|t| t.elapsed() < Duration::from_secs(60))
    }

    /// Register the pairing agent, if BlueZ is running. Harmless to call when not.
    pub async fn register_agent(&self) {
        let agent = Agent { bt: self.clone() };
        if self.system.object_server().at(AGENT_PATH, agent).await.is_err() {
            return;
        }
        let Ok(m) = AgentManager1Proxy::new(&self.system).await else { return };
        let path = zbus::wire::ObjectPath::try_from(AGENT_PATH).expect("agent path");
        match m.register_agent(&path, "DisplayYesNo").await {
            Ok(()) => {
                let _ = m.request_default_agent(&path).await;
                tracing::info!("Bluetooth pairing agent registered");
            }
            Err(e) => tracing::debug!("no BlueZ agent manager: {e}"),
        }
    }
}

fn unavailable() -> zbus::fdo::Error {
    zbus::fdo::Error::NotSupported("no Bluetooth adapter".into())
}

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.bluez.Error")]
pub enum AgentError {
    Rejected(String),
    Canceled(String),
    #[zbus(error)]
    ZBus(zbus::Error),
}

struct Agent {
    bt: Bt,
}

#[interface(name = "org.bluez.Agent1")]
impl Agent {
    fn release(&self) {}

    fn request_pin_code(&self, _device: OwnedObjectPath) -> Result<String, AgentError> {
        if self.bt.pairing_wanted() {
            // Headsets that ask for a PIN almost all use 0000.
            Ok("0000".into())
        } else {
            Err(AgentError::Rejected("not pairing".into()))
        }
    }

    fn display_pin_code(&self, _device: OwnedObjectPath, _pincode: &str) {}

    fn request_passkey(&self, _device: OwnedObjectPath) -> Result<u32, AgentError> {
        Err(AgentError::Rejected("no keyboard on the device side".into()))
    }

    async fn display_passkey(&self, device: OwnedObjectPath, passkey: u32, _entered: u16) {
        crate::api::emit_passkey(&self.bt.session, device.as_str(), passkey).await;
    }

    async fn request_confirmation(&self, device: OwnedObjectPath, passkey: u32) -> Result<(), AgentError> {
        crate::api::emit_passkey(&self.bt.session, device.as_str(), passkey).await;
        if self.bt.pairing_wanted() {
            Ok(())
        } else {
            Err(AgentError::Rejected("pairing was not started from Dolphone".into()))
        }
    }

    fn request_authorization(&self, _device: OwnedObjectPath) -> Result<(), AgentError> {
        if self.bt.pairing_wanted() {
            Ok(())
        } else {
            Err(AgentError::Rejected("not pairing".into()))
        }
    }

    /// Services on already-paired, trusted devices are authorised by BlueZ without
    /// asking; anything else is a device the owner has not chosen.
    fn authorize_service(&self, _device: OwnedObjectPath, _uuid: &str) -> Result<(), AgentError> {
        if self.bt.pairing_wanted() {
            Ok(())
        } else {
            Err(AgentError::Rejected("not pairing".into()))
        }
    }

    fn cancel(&self) {}
}
