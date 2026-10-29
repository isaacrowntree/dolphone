//! fake-mm: a stand-in for ModemManager with one scriptable modem.
//!
//! It owns `org.freedesktop.ModemManager1` on the bus it is given (the system bus by
//! default, or whatever `DBUS_SYSTEM_BUS_ADDRESS` names) and serves the subset of
//! ModemManager's API that phoned, `mmcli` and FlipCTL's status bar read:
//!
//! - the manager object with its ObjectManager, at `/org/freedesktop/ModemManager1`
//! - one modem at `.../Modem/0` with the Modem, Modem3gpp, Voice, Messaging and
//!   Ussd interfaces, and its SIM at `.../SIM/0`
//! - call objects at `.../Call/N` and SMS objects at `.../SMS/N`, made as they are
//!   needed, as ModemManager makes them
//!
//! The other side of the network is driven through
//! `io.github.isaacrowntree.Dolphone1.FakeModem` on the manager object, which is what
//! `phonectl fake ...` calls: ring the phone, deliver a text, answer or hang up as the
//! remote party, change the signal.
//!
//! Property names, types and enum values follow ModemManager 1.24's introspection
//! XML, so a client written against fake-mm needs no change for a real modem.

mod at;
mod call;
mod control;
mod modem;
mod nm;
mod objmgr;
mod sms;
mod world;

use anyhow::Context;
use tracing_subscriber::EnvFilter;

pub const SERVICE: &str = "org.freedesktop.ModemManager1";
pub const MANAGER: &str = "/org/freedesktop/ModemManager1";
pub const MODEM: &str = "/org/freedesktop/ModemManager1/Modem/0";
pub const SIM: &str = "/org/freedesktop/ModemManager1/SIM/0";

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "fake_mm=info".into()))
        .init();

    let session = std::env::args().any(|a| a == "--session");
    let world = world::World::shared();
    let net = nm::shared();
    let nm_conn = nm::serve(session, net.clone()).await;
    let builder = if session { zbus::connection::Builder::session() } else { zbus::connection::Builder::system() };
    let conn = builder
        .name(SERVICE)
        .serve_at(MANAGER, objmgr::ObjectManager)
        .serve_at(MANAGER, modem::Manager)
        .serve_at(MANAGER, control::Control { world: world.clone() })
        .serve_at(SIM, modem::Sim { world: world.clone() })
        .serve_at(MODEM, modem::Modem { world: world.clone() })
        .serve_at(MODEM, modem::Modem3gpp { world: world.clone() })
        .serve_at(MODEM, modem::Voice { world: world.clone() })
        .serve_at(MODEM, modem::Messaging { world: world.clone() })
        .serve_at(MODEM, modem::Ussd { world: world.clone() })
        .serve_at(MODEM, modem::CellBroadcast { world: world.clone() })
        .build()
        .await
        .context(
            "claiming org.freedesktop.ModemManager1 (is the real ModemManager running, or is the bus policy missing?)",
        )?;

    {
        let mut w = world.lock().await;
        w.conn = Some(conn.clone());
        w.nm = nm_conn.map(|c| (c, net));
    }
    tracing::info!(bus = if session { "session" } else { "system" }, "fake modem up at {MODEM}");

    tokio::signal::ctrl_c().await?;
    Ok(())
}
