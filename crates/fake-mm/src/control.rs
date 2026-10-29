//! The far end of the network, driven by tests and `phonectl fake`.

use zbus::wire::OwnedObjectPath;
use zbus::{interface, Connection, ObjectServer};

use crate::call;
use crate::modem;
use crate::world::{call_state as cs, reason, tech, Shared};

pub struct Control {
    pub world: Shared,
}

#[interface(name = "io.github.isaacrowntree.Dolphone1.FakeModem")]
impl Control {
    /// Someone calls the phone. While another call is active the new one arrives
    /// as call waiting, as a network would deliver it.
    async fn ring(
        &self,
        number: &str,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        let calls = self.world.lock().await.calls.clone();
        let mut busy = false;
        for p in &calls {
            if matches!(call::state_of(conn, p).await.map(|s| s.0), Some(cs::ACTIVE | cs::HELD)) {
                busy = true;
            }
        }
        let p = {
            let mut w = self.world.lock().await;
            let p = w.call_path();
            w.calls.push(p.clone());
            p
        };
        let state = if busy { cs::WAITING } else { cs::RINGING_IN };
        server.at(p.as_str(), modem::new_incoming(&self.world, &p, number, state)).await?;
        modem::announce_call(conn, &p).await?;
        tracing::info!(call = %p, number, waiting = busy, "incoming call");
        Ok(OwnedObjectPath::try_from(p).expect("path"))
    }

    /// The far end of the newest outgoing call picks up.
    async fn remote_answer(&self, #[zbus(connection)] conn: &Connection) -> zbus::fdo::Result<()> {
        let calls = self.world.lock().await.calls.clone();
        for p in calls.iter().rev() {
            if let Some((s, 2)) = call::state_of(conn, p).await {
                if matches!(s, cs::DIALING | cs::RINGING_OUT) {
                    call::set_state(conn, p, cs::ACTIVE, reason::ACCEPTED).await;
                    return Ok(());
                }
            }
        }
        Err(zbus::fdo::Error::Failed("no outgoing call is ringing".into()))
    }

    /// The far end hangs up the newest live call (or, for a ringing incoming call,
    /// gives up, which the phone logs as missed).
    async fn remote_hangup(&self, #[zbus(connection)] conn: &Connection) -> zbus::fdo::Result<()> {
        let calls = self.world.lock().await.calls.clone();
        for p in calls.iter().rev() {
            if call::state_of(conn, p).await.is_some_and(|s| s.0 != cs::TERMINATED) {
                call::set_state(conn, p, cs::TERMINATED, reason::TERMINATED).await;
                return Ok(());
            }
        }
        Err(zbus::fdo::Error::Failed("no live call".into()))
    }

    /// The far end is busy: the newest outgoing call is refused.
    async fn remote_busy(&self, #[zbus(connection)] conn: &Connection) -> zbus::fdo::Result<()> {
        let calls = self.world.lock().await.calls.clone();
        for p in calls.iter().rev() {
            if let Some((s, 2)) = call::state_of(conn, p).await {
                if s != cs::TERMINATED {
                    call::set_state(conn, p, cs::TERMINATED, reason::REFUSED_OR_BUSY).await;
                    return Ok(());
                }
            }
        }
        Err(zbus::fdo::Error::Failed("no outgoing call".into()))
    }

    /// Someone texts the phone.
    async fn deliver(
        &self,
        from: &str,
        text: &str,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        let p = {
            let mut w = self.world.lock().await;
            let p = w.sms_path();
            w.sms.push(p.clone());
            p
        };
        server.at(p.as_str(), modem::new_received(&self.world, &p, from, text)).await?;
        modem::announce_sms(conn, &p).await?;
        tracing::info!(sms = %p, from, "text delivered");
        Ok(OwnedObjectPath::try_from(p).expect("path"))
    }

    /// Signal quality 0..=100 and the technology ("5G", "LTE", "3G", "none").
    async fn set_signal(
        &self,
        quality: u32,
        technology: &str,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<()> {
        {
            let mut w = self.world.lock().await;
            w.quality = quality.min(100);
            w.tech = tech::from_label(technology);
            w.registered = w.tech != 0;
        }
        modem::announce_signal(conn).await?;
        Ok(())
    }

    async fn set_registration(
        &self,
        registered: bool,
        roaming: bool,
        operator: &str,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<()> {
        {
            let mut w = self.world.lock().await;
            w.registered = registered;
            w.roaming = roaming;
            if !operator.is_empty() {
                w.operator = operator.to_string();
            }
        }
        modem::announce_signal(conn).await?;
        Ok(())
    }

    /// How long the far end takes to answer an outgoing call; 0 leaves it ringing.
    async fn set_auto_answer(&self, milliseconds: u64) {
        self.world.lock().await.auto_answer_ms = milliseconds;
    }

    async fn set_send_fails(&self, fails: bool) {
        self.world.lock().await.send_fails = fails;
    }

    async fn set_ussd_reply(&self, reply: &str) {
        self.world.lock().await.ussd_reply = reply.to_string();
    }

    /// The calls and messages the modem holds, one per line, for a test to read.
    async fn dump(&self, #[zbus(connection)] conn: &Connection) -> String {
        let (calls, sms) = {
            let w = self.world.lock().await;
            (w.calls.clone(), w.sms.clone())
        };
        let mut out = String::new();
        for p in calls {
            if let Some((s, d)) = call::state_of(conn, &p).await {
                out.push_str(&format!("call {p} state={s} direction={d}\n"));
            }
        }
        for p in sms {
            out.push_str(&format!("sms {p}\n"));
        }
        out
    }
}
