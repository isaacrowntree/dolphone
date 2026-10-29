//! A call object, as ModemManager publishes one for every call it knows of.

use std::time::Duration;

use zbus::object_server::SignalEmitter;
use zbus::{interface, Connection};

use crate::world::{call_state as cs, reason, Shared};

pub struct Call {
    pub world: Shared,
    pub path: String,
    pub state: i32,
    pub reason: i32,
    /// MMCallDirection: 1 incoming, 2 outgoing.
    pub direction: i32,
    pub number: String,
    pub dtmf: String,
}

#[interface(name = "org.freedesktop.ModemManager1.Call")]
impl Call {
    async fn start(&mut self, #[zbus(connection)] conn: &Connection) -> zbus::fdo::Result<()> {
        if self.direction != 2 || self.state != cs::UNKNOWN {
            return Err(zbus::fdo::Error::Failed("call cannot be started in this state".into()));
        }
        let (path, world) = (self.path.clone(), self.world.clone());
        let conn = conn.clone();
        tokio::spawn(async move {
            set_state(&conn, &path, cs::DIALING, reason::OUTGOING_STARTED).await;
            tokio::time::sleep(Duration::from_millis(600)).await;
            set_state_if(&conn, &path, cs::DIALING, cs::RINGING_OUT, reason::OUTGOING_STARTED).await;
            let ms = world.lock().await.auto_answer_ms;
            if ms > 0 {
                tokio::time::sleep(Duration::from_millis(ms)).await;
                set_state_if(&conn, &path, cs::RINGING_OUT, cs::ACTIVE, reason::ACCEPTED).await;
            }
        });
        Ok(())
    }

    async fn accept(&mut self, #[zbus(signal_emitter)] em: SignalEmitter<'_>) -> zbus::fdo::Result<()> {
        if !matches!(self.state, cs::RINGING_IN | cs::WAITING) {
            return Err(zbus::fdo::Error::Failed("call is not ringing".into()));
        }
        self.transition(&em, cs::ACTIVE, reason::ACCEPTED).await;
        Ok(())
    }

    async fn hangup(&mut self, #[zbus(signal_emitter)] em: SignalEmitter<'_>) -> zbus::fdo::Result<()> {
        if self.state == cs::TERMINATED {
            return Ok(());
        }
        self.transition(&em, cs::TERMINATED, reason::TERMINATED).await;
        Ok(())
    }

    fn deflect(&self, _number: &str) -> zbus::fdo::Result<()> {
        Err(zbus::fdo::Error::NotSupported("deflect".into()))
    }

    fn join_multiparty(&self) -> zbus::fdo::Result<()> {
        Err(zbus::fdo::Error::NotSupported("multiparty".into()))
    }

    fn leave_multiparty(&self) -> zbus::fdo::Result<()> {
        Err(zbus::fdo::Error::NotSupported("multiparty".into()))
    }

    async fn send_dtmf(&mut self, dtmf: &str) -> zbus::fdo::Result<()> {
        if self.state != cs::ACTIVE {
            return Err(zbus::fdo::Error::Failed("DTMF needs an active call".into()));
        }
        self.dtmf.push_str(dtmf);
        tracing::info!(call = %self.path, dtmf, "DTMF sent");
        Ok(())
    }

    // Named apart from the State property's own `state_changed` helper, which zbus
    // generates and which this signal's Rust name would otherwise collide with.
    #[zbus(signal, name = "StateChanged")]
    pub async fn call_state_changed(em: &SignalEmitter<'_>, old: i32, new: i32, reason: i32) -> zbus::Result<()>;

    #[zbus(signal)]
    pub async fn dtmf_received(em: &SignalEmitter<'_>, dtmf: &str) -> zbus::Result<()>;

    #[zbus(property)]
    fn state(&self) -> i32 {
        self.state
    }

    #[zbus(property)]
    fn state_reason(&self) -> i32 {
        self.reason
    }

    #[zbus(property)]
    fn direction(&self) -> i32 {
        self.direction
    }

    #[zbus(property)]
    fn number(&self) -> String {
        self.number.clone()
    }

    #[zbus(property)]
    fn multiparty(&self) -> bool {
        false
    }

    /// Where a real modem's audio appears: a USB audio card on the RM520N-GL, in
    /// UAC mode. Empty here, which phoned reads as "no modem audio to bridge".
    #[zbus(property)]
    fn audio_port(&self) -> String {
        String::new()
    }
}

impl Call {
    pub async fn transition(&mut self, em: &SignalEmitter<'_>, new: i32, why: i32) {
        let old = self.state;
        self.state = new;
        self.reason = why;
        let _ = Self::call_state_changed(em, old, new, why).await;
        let _ = self.state_changed(em).await;
        let _ = self.state_reason_changed(em).await;
        tracing::info!(call = %self.path, old, new, "call state");
    }
}

/// Move a call to `new`, whatever it was.
pub async fn set_state(conn: &Connection, path: &str, new: i32, why: i32) {
    let Ok(iref) = conn.object_server().interface::<_, Call>(path).await else { return };
    let em = iref.signal_emitter().clone();
    iref.get_mut().await.transition(&em, new, why).await;
}

/// Move a call to `new` only if it is still `from`: a timer firing after the user
/// hung up must not bring the call back.
pub async fn set_state_if(conn: &Connection, path: &str, from: i32, new: i32, why: i32) {
    let Ok(iref) = conn.object_server().interface::<_, Call>(path).await else { return };
    let em = iref.signal_emitter().clone();
    let mut call = iref.get_mut().await;
    if call.state == from {
        call.transition(&em, new, why).await;
    }
}

pub async fn state_of(conn: &Connection, path: &str) -> Option<(i32, i32)> {
    let iref = conn.object_server().interface::<_, Call>(path).await.ok()?;
    let c = iref.get().await;
    Some((c.state, c.direction))
}
