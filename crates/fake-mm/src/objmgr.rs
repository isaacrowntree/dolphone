//! The manager's ObjectManager, as ModemManager runs it: it lists modems and
//! nothing else.
//!
//! zbus's own `fdo::ObjectManager` reports every object below its path, which here
//! would include the SIM, the calls and the texts. ModemManager's clients
//! (libmm-glib, so `mmcli`) treat every managed object as a modem and warn about
//! each one that is not, so this answers with the modem alone, reading its
//! properties back from the object server the way a client's GetAll would.

use std::collections::HashMap;

use zbus::wire::{OwnedObjectPath, OwnedVariant};
use zbus::{interface, Connection};

use crate::MODEM;

pub struct ObjectManager;

const MODEM_INTERFACES: &[&str] = &[
    "org.freedesktop.ModemManager1.Modem",
    "org.freedesktop.ModemManager1.Modem.Modem3gpp",
    "org.freedesktop.ModemManager1.Modem.Voice",
    "org.freedesktop.ModemManager1.Modem.Messaging",
    "org.freedesktop.ModemManager1.Modem.Modem3gpp.Ussd",
];

type Props = HashMap<String, OwnedVariant>;

#[interface(name = "org.freedesktop.DBus.ObjectManager")]
impl ObjectManager {
    async fn get_managed_objects(
        &self,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<HashMap<OwnedObjectPath, HashMap<String, Props>>> {
        let me = conn.unique_name().map(|n| n.to_string()).unwrap_or_default();
        let props = zbus::fdo::PropertiesProxy::builder(conn).destination(me).path(MODEM).build().await?;
        let mut ifaces = HashMap::new();
        for name in MODEM_INTERFACES {
            let iface = zbus::names::InterfaceName::try_from(*name).expect("interface name");
            let all = props.get_all(iface).await?;
            ifaces.insert(name.to_string(), all.into_iter().collect());
        }
        Ok(HashMap::from([(OwnedObjectPath::try_from(MODEM.to_string()).expect("path"), ifaces)]))
    }
}
