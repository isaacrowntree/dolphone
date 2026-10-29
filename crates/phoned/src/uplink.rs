//! Uplink watch: text the owner when the network link goes down, and again when it
//! comes back. The network-multitool use of a phone: a Flipper One on a router or a
//! remote site reports its own outage over the mobile network, which still works.
//!
//! What "the uplink" is comes from NetworkManager: one interface's device state
//! (`uplink_interface`, say eth0), or with none named, NetworkManager's overall
//! connectivity. It must be down for `uplink_grace_s` seconds (60 by default) before
//! a text goes, so a blip does not page anyone. The text goes as the owner (the
//! number is the owner's own setting), through the same limits as any text.

use std::sync::Arc;
use std::time::Duration;

use zbus::proxy;

use crate::caller::Caller;
use crate::phone::Phone;

#[proxy(
    interface = "org.freedesktop.NetworkManager",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager"
)]
trait NetworkManager {
    /// NMConnectivityState: 1 none, 2 portal, 3 limited, 4 full.
    #[zbus(property)]
    fn connectivity(&self) -> zbus::Result<u32>;
    fn get_device_by_ip_iface(&self, iface: &str) -> zbus::Result<zbus::wire::OwnedObjectPath>;
}

#[proxy(interface = "org.freedesktop.NetworkManager.Device", default_service = "org.freedesktop.NetworkManager")]
trait Device {
    /// NMDeviceState: 100 activated.
    #[zbus(property)]
    fn state(&self) -> zbus::Result<u32>;
}

/// What a watch saw happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Down since this time, for longer than the grace.
    Down { since: i64 },
    /// Back up after a reported outage that began then.
    Up { since: i64 },
}

/// The watch's memory: when the link went down, and whether that was reported.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Watch {
    down_since: Option<i64>,
    reported: bool,
}

impl Watch {
    /// Feed one look at the link. None when nothing is worth a text.
    pub fn step(&mut self, up: bool, now: i64, grace: i64) -> Option<Change> {
        match (up, self.down_since) {
            (true, Some(since)) => {
                let was = self.reported;
                *self = Self::default();
                was.then_some(Change::Up { since })
            }
            (true, None) => None,
            (false, None) => {
                self.down_since = Some(now);
                None
            }
            (false, Some(since)) if !self.reported && now - since >= grace => {
                self.reported = true;
                Some(Change::Down { since })
            }
            (false, Some(_)) => None,
        }
    }
}

/// Is the link up? None when NetworkManager cannot say (not running, no such
/// interface), which counts as no news rather than an outage.
async fn link_up(system: &zbus::Connection, iface: &str) -> Option<bool> {
    let nm =
        NetworkManagerProxy::builder(system).cache_properties(zbus::proxy::CacheProperties::No).build().await.ok()?;
    if iface.is_empty() {
        return match nm.connectivity().await.ok()? {
            0 => None,
            c => Some(c == 4),
        };
    }
    let path = nm.get_device_by_ip_iface(iface).await.ok()?;
    let dev = DeviceProxy::builder(system)
        .path(path)
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
        .ok()?;
    Some(dev.state().await.ok()? == 100)
}

fn text_for(change: Change, what: &str, now: i64) -> String {
    let at = |t: i64| {
        let l = phone_core::when::local(t);
        format!("{}:{:02}", l.hour, l.minute)
    };
    match change {
        Change::Down { since } => format!("Dolphone: {what} is down (since {}).", at(since)),
        Change::Up { since } => {
            format!("Dolphone: {what} is back up, after {}.", phone_core::when::duration(now - since))
        }
    }
}

/// Watch for phoned's lifetime. Looks every ten seconds; does nothing while no
/// number is set.
pub async fn run(phone: Arc<Phone>) {
    let mut watch = Watch::default();
    loop {
        tokio::time::sleep(Duration::from_secs(if cfg!(debug_assertions) { 2 } else { 10 })).await;
        let to = phone.setting(phone_api::keys::UPLINK_ALERT_TO);
        if to.trim().is_empty() {
            watch = Watch::default();
            continue;
        }
        let iface = phone.setting(phone_api::keys::UPLINK_INTERFACE);
        let grace = phone.setting(phone_api::keys::UPLINK_GRACE_S).parse::<i64>().unwrap_or(60).clamp(0, 3600);
        let Some(up) = link_up(&phone.system, iface.trim()).await else { continue };
        let now = phone_core::now();
        if let Some(change) = watch.step(up, now, grace) {
            let what = if iface.trim().is_empty() { "the network".to_string() } else { iface.trim().to_string() };
            let body = text_for(change, &what, now);
            tracing::warn!(?change, "uplink");
            let owner = Caller { name: "uplink watch".into(), trusted: true, reader: false };
            if let Err(e) = phone.send(&to, &body, &owner, false).await {
                tracing::warn!("the uplink text was not sent: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blip_is_not_an_outage() {
        let mut w = Watch::default();
        assert_eq!(w.step(false, 100, 60), None);
        assert_eq!(w.step(true, 130, 60), None, "back within the grace: nothing to say");
        assert_eq!(w.step(false, 200, 60), None);
        assert_eq!(w.step(false, 259, 60), None);
        assert_eq!(w.step(false, 260, 60), Some(Change::Down { since: 200 }));
        assert_eq!(w.step(false, 400, 60), None, "said once");
        assert_eq!(w.step(true, 500, 60), Some(Change::Up { since: 200 }));
        assert_eq!(w.step(true, 510, 60), None);
    }

    #[test]
    fn the_texts_say_what_and_when() {
        assert!(text_for(Change::Down { since: 0 }, "eth0", 0).starts_with("Dolphone: eth0 is down (since"));
        assert!(text_for(Change::Up { since: 0 }, "the network", 125).contains("is back up, after"));
    }
}
