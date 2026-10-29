//! A stand-in for the part of NetworkManager phoned's uplink watch reads: the
//! overall connectivity, and one device's state by interface name. Values are
//! NetworkManager's (NMConnectivityState, NMDeviceState). It only claims
//! NetworkManager's name when nothing else has it, so it never displaces a real one.

use std::sync::Arc;

use tokio::sync::Mutex;
use zbus::wire::OwnedObjectPath;
use zbus::{interface, Connection};

pub const SERVICE: &str = "org.freedesktop.NetworkManager";
pub const PATH: &str = "/org/freedesktop/NetworkManager";
pub const DEVICE: &str = "/org/freedesktop/NetworkManager/Devices/1";
/// The one interface the fake has.
pub const IFACE: &str = "eth0";

pub struct NetState {
    /// NMConnectivityState: 4 is full.
    pub connectivity: u32,
    /// NMDeviceState of eth0: 100 is activated.
    pub device: u32,
}

pub type Shared = Arc<Mutex<NetState>>;

pub fn shared() -> Shared {
    Arc::new(Mutex::new(NetState { connectivity: 4, device: 100 }))
}

pub struct Manager(pub Shared);

#[interface(name = "org.freedesktop.NetworkManager")]
impl Manager {
    #[zbus(property)]
    async fn connectivity(&self) -> u32 {
        self.0.lock().await.connectivity
    }

    async fn get_device_by_ip_iface(&self, iface: &str) -> zbus::fdo::Result<OwnedObjectPath> {
        if iface == IFACE {
            Ok(OwnedObjectPath::try_from(DEVICE).expect("path"))
        } else {
            Err(zbus::fdo::Error::Failed(format!("no device for {iface}")))
        }
    }
}

pub struct Device(pub Shared);

#[interface(name = "org.freedesktop.NetworkManager.Device")]
impl Device {
    #[zbus(property)]
    async fn state(&self) -> u32 {
        self.0.lock().await.device
    }
    #[zbus(property)]
    fn interface(&self) -> String {
        IFACE.into()
    }
}

/// Serve the fake on its own connection, if NetworkManager's name is free.
pub async fn serve(session: bool, net: Shared) -> Option<Connection> {
    let builder = if session { zbus::connection::Builder::session() } else { zbus::connection::Builder::system() };
    let conn = builder
        .name(SERVICE)
        .serve_at(PATH, Manager(net.clone()))
        .serve_at(DEVICE, Device(net))
        .build()
        .await
        .map_err(|e| tracing::info!("not standing in for NetworkManager: {e}"))
        .ok()?;
    tracing::info!("standing in for NetworkManager ({IFACE})");
    Some(conn)
}

/// Change what the fake reports, and say so as NetworkManager would.
pub async fn set(conn: &Connection, net: &Shared, connectivity: u32, device: u32) {
    {
        let mut n = net.lock().await;
        n.connectivity = connectivity;
        n.device = device;
    }
    if let Ok(i) = conn.object_server().interface::<_, Manager>(PATH).await {
        let _ = i.get().await.connectivity_changed(i.signal_emitter()).await;
    }
    if let Ok(i) = conn.object_server().interface::<_, Device>(DEVICE).await {
        let _ = i.get().await.state_changed(i.signal_emitter()).await;
    }
}
