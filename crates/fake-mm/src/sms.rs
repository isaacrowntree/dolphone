//! An SMS object, as ModemManager publishes one per stored, received or outgoing text.

use std::time::Duration;

use zbus::{interface, Connection};

use crate::world::{sms_state, Shared};

pub struct Sms {
    pub world: Shared,
    pub path: String,
    pub state: u32,
    /// MMSmsPduType: 1 deliver (received), 2 submit (outgoing).
    pub pdu_type: u32,
    pub number: String,
    pub text: String,
    pub timestamp: String,
    /// The sender asked for a delivery report, and how delivery stands.
    pub delivery_report: bool,
    pub delivery: u32,
}

#[interface(name = "org.freedesktop.ModemManager1.Sms")]
impl Sms {
    async fn send(&mut self, #[zbus(connection)] conn: &Connection) -> zbus::fdo::Result<()> {
        if self.pdu_type != 2 {
            return Err(zbus::fdo::Error::Failed("only an outgoing message can be sent".into()));
        }
        if self.world.lock().await.send_fails {
            return Err(zbus::fdo::Error::Failed("network rejected the message (fake)".into()));
        }
        // ModemManager returns from Send once the network has the message, so the
        // reply is the success; the state is set before it for a client that reads it.
        self.state = sms_state::SENDING;
        let (conn, path) = (conn.clone(), self.path.clone());
        tokio::time::sleep(Duration::from_millis(150)).await;
        self.state = sms_state::SENT;
        let report = self.delivery_report;
        tokio::spawn(async move {
            if let Ok(iref) = conn.object_server().interface::<_, Sms>(path.as_str()).await {
                let em = iref.signal_emitter().clone();
                let _ = iref.get().await.state_changed(&em).await;
            }
            // The network's delivery report, a moment later, when one was asked for.
            if report {
                tokio::time::sleep(Duration::from_millis(700)).await;
                if let Ok(iref) = conn.object_server().interface::<_, Sms>(path.as_str()).await {
                    let em = iref.signal_emitter().clone();
                    let mut sms = iref.get_mut().await;
                    sms.delivery = crate::world::DELIVERED;
                    let _ = sms.delivery_state_changed(&em).await;
                }
            }
        });
        tracing::info!(sms = %self.path, to = %self.number, "sent");
        Ok(())
    }

    fn store(&mut self, _storage: u32) -> zbus::fdo::Result<()> {
        self.state = sms_state::STORED;
        Ok(())
    }

    #[zbus(property)]
    fn state(&self) -> u32 {
        self.state
    }

    #[zbus(property)]
    fn pdu_type(&self) -> u32 {
        self.pdu_type
    }

    #[zbus(property)]
    fn number(&self) -> String {
        self.number.clone()
    }

    #[zbus(property)]
    fn text(&self) -> String {
        self.text.clone()
    }

    #[zbus(property)]
    fn timestamp(&self) -> String {
        self.timestamp.clone()
    }

    /// MMSmsStorage: 2 is ME (the modem's own memory).
    #[zbus(property)]
    fn storage(&self) -> u32 {
        2
    }

    /// -1: not a class-0 ("flash") message.
    #[zbus(property)]
    fn class(&self) -> i32 {
        -1
    }

    #[zbus(property)]
    fn delivery_report_request(&self) -> bool {
        self.delivery_report
    }

    #[zbus(property)]
    fn message_reference(&self) -> u32 {
        0
    }

    #[zbus(property)]
    fn smsc(&self) -> String {
        "+61418706700".into()
    }

    #[zbus(property)]
    fn validity(&self) -> (u32, zbus::wire::OwnedVariant) {
        (0, zbus::wire::Variant::from(0u32).try_into().expect("u32 value"))
    }

    #[zbus(property)]
    fn data(&self) -> Vec<u8> {
        Vec::new()
    }

    #[zbus(property)]
    fn service_category(&self) -> u32 {
        0
    }

    #[zbus(property)]
    fn teleservice_id(&self) -> u32 {
        0
    }

    #[zbus(property)]
    fn delivery_state(&self) -> u32 {
        self.delivery
    }

    #[zbus(property)]
    fn discharge_timestamp(&self) -> String {
        String::new()
    }
}

/// An ISO 8601 timestamp for now, in UTC, as ModemManager formats them.
pub fn timestamp_now() -> String {
    let secs =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil-from-days, after Howard Hinnant.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}+00:00", rem / 3600, rem % 3600 / 60, rem % 60)
}
