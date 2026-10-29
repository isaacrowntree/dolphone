//! The manager, the SIM and the modem's interfaces.

use std::collections::HashMap;

use zbus::object_server::SignalEmitter;
use zbus::wire::{OwnedObjectPath, OwnedVariant};
use zbus::{interface, Connection, ObjectServer};

use crate::call::{self, Call};
use crate::sms::{self, Sms};
use crate::world::{call_state as cs, modem_state, reason, sms_state, Shared};
use crate::{MODEM, SIM};

fn path(p: &str) -> OwnedObjectPath {
    OwnedObjectPath::try_from(p.to_string()).expect("valid object path")
}

fn string_arg(props: &HashMap<String, OwnedVariant>, key: &str) -> Option<String> {
    props.get(key).and_then(|v| String::try_from(v.clone()).ok())
}

fn bool_arg(props: &HashMap<String, OwnedVariant>, key: &str) -> bool {
    props.get(key).and_then(|v| bool::try_from(v.clone()).ok()).unwrap_or(false)
}

/// ModemManager's core errors, by their D-Bus names.
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.freedesktop.ModemManager1.Error.Core")]
pub enum CoreError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Failed(String),
    Unauthorized(String),
}

/// `org.freedesktop.ModemManager1` itself.
pub struct Manager;

#[interface(name = "org.freedesktop.ModemManager1")]
impl Manager {
    fn scan_devices(&self) {}
    fn set_logging(&self, _level: &str) {}
    fn report_kernel_event(&self, _props: HashMap<String, OwnedVariant>) {}
    fn inhibit_device(&self, _uid: &str, _inhibit: bool) {}

    #[zbus(property)]
    fn version(&self) -> String {
        "1.24.0 (Dolphone fake-mm)".into()
    }
}

pub struct Sim {
    pub world: Shared,
}

#[interface(name = "org.freedesktop.ModemManager1.Sim")]
impl Sim {
    async fn send_pin(&self, pin: &str, #[zbus(connection)] conn: &Connection) -> zbus::fdo::Result<()> {
        let mut w = self.world.lock().await;
        if !w.locked {
            return Ok(());
        }
        if w.pin_retries == 0 {
            return Err(zbus::fdo::Error::Failed("SIM blocked: PUK required".into()));
        }
        if pin != w.pin {
            w.pin_retries -= 1;
            // As ModemManager: no count in the error; UnlockRetries has it.
            return Err(zbus::fdo::Error::Failed("incorrect password".into()));
        }
        w.locked = false;
        w.pin_retries = 3;
        drop(w);
        unlock_interfaces(conn, &self.world).await;
        tracing::info!("SIM unlocked");
        Ok(())
    }

    async fn change_pin(&self, old: &str, new: &str) -> zbus::fdo::Result<()> {
        let mut w = self.world.lock().await;
        if old != w.pin {
            w.pin_retries = w.pin_retries.saturating_sub(1);
            return Err(zbus::fdo::Error::Failed("incorrect password".into()));
        }
        w.pin = new.to_string();
        tracing::info!("SIM PIN changed");
        Ok(())
    }

    async fn send_puk(&self, puk: &str, pin: &str, #[zbus(connection)] conn: &Connection) -> zbus::fdo::Result<()> {
        if puk != "12345678" {
            return Err(zbus::fdo::Error::Failed("incorrect password".into()));
        }
        let mut w = self.world.lock().await;
        w.pin = pin.to_string();
        w.pin_retries = 3;
        let was_locked = std::mem::replace(&mut w.locked, false);
        drop(w);
        if was_locked {
            unlock_interfaces(conn, &self.world).await;
        }
        Ok(())
    }

    #[zbus(property)]
    fn active(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn sim_identifier(&self) -> String {
        "8961000000000000000F".into()
    }
    #[zbus(property)]
    fn imsi(&self) -> String {
        "505990000000001".into()
    }
    #[zbus(property)]
    fn eid(&self) -> String {
        String::new()
    }
    #[zbus(property)]
    fn operator_identifier(&self) -> String {
        "50599".into()
    }
    #[zbus(property)]
    fn operator_name(&self) -> String {
        "Fakecom AU".into()
    }
    #[zbus(property)]
    fn emergency_numbers(&self) -> Vec<String> {
        vec!["000".into(), "112".into(), "106".into()]
    }
    /// MMSimType: 1 physical.
    #[zbus(property)]
    fn sim_type(&self) -> u32 {
        1
    }
}

pub struct Modem {
    pub world: Shared,
}

#[interface(name = "org.freedesktop.ModemManager1.Modem")]
impl Modem {
    async fn enable(&self, enable: bool, #[zbus(connection)] conn: &Connection) -> zbus::fdo::Result<()> {
        self.world.lock().await.enabled = enable;
        tracing::info!(enable, "modem enable");
        let _ = announce_signal(conn).await;
        Ok(())
    }
    fn list_bearers(&self) -> Vec<OwnedObjectPath> {
        Vec::new()
    }
    fn reset(&self) {}
    fn set_power_state(&self, _state: u32) {}
    /// AT passthrough, answered as an RM520N-GL would. ModemManager refuses it
    /// unless it runs with --debug (mm-iface-modem.c), and so does this.
    async fn command(&self, cmd: &str, _timeout: u32) -> Result<String, CoreError> {
        tracing::info!(cmd, "AT command");
        let mut w = self.world.lock().await;
        if !w.at.debug {
            return Err(CoreError::Unauthorized(
                "Cannot send AT command to modem: operation only allowed in debug mode".into(),
            ));
        }
        w.at.answer(cmd).ok_or_else(|| CoreError::Failed("ERROR".into()))
    }

    #[zbus(property)]
    fn sim(&self) -> OwnedObjectPath {
        path(SIM)
    }
    #[zbus(property)]
    fn sim_slots(&self) -> Vec<OwnedObjectPath> {
        vec![path(SIM)]
    }
    #[zbus(property)]
    fn primary_sim_slot(&self) -> u32 {
        1
    }
    #[zbus(property)]
    fn bearers(&self) -> Vec<OwnedObjectPath> {
        Vec::new()
    }
    /// MMModemCapability: 4 is LTE, 8 is 5GNR.
    #[zbus(property)]
    fn supported_capabilities(&self) -> Vec<u32> {
        vec![4 | 8]
    }
    #[zbus(property)]
    fn current_capabilities(&self) -> u32 {
        4 | 8
    }
    #[zbus(property)]
    fn max_bearers(&self) -> u32 {
        4
    }
    #[zbus(property)]
    fn max_active_bearers(&self) -> u32 {
        4
    }
    #[zbus(property)]
    fn manufacturer(&self) -> String {
        "Quectel".into()
    }
    #[zbus(property)]
    fn model(&self) -> String {
        "fake-mm RM520N-GL".into()
    }
    #[zbus(property)]
    fn revision(&self) -> String {
        "FAKE01A01M4G".into()
    }
    #[zbus(property)]
    fn hardware_revision(&self) -> String {
        "1.0".into()
    }
    #[zbus(property)]
    fn carrier_configuration(&self) -> String {
        "AU-VoLTE-Fakecom".into()
    }
    #[zbus(property)]
    fn carrier_configuration_revision(&self) -> String {
        "0".into()
    }
    #[zbus(property)]
    fn device_identifier(&self) -> String {
        "fakemm0000000000000000000000000000000000".into()
    }
    #[zbus(property)]
    fn device(&self) -> String {
        "/sys/devices/platform/fake-mm".into()
    }
    #[zbus(property)]
    fn physdev(&self) -> String {
        "/sys/devices/platform/fake-mm".into()
    }
    #[zbus(property)]
    fn drivers(&self) -> Vec<String> {
        vec!["qmi_wwan".into(), "option".into()]
    }
    #[zbus(property)]
    fn plugin(&self) -> String {
        "quectel".into()
    }
    #[zbus(property)]
    fn primary_port(&self) -> String {
        "cdc-wdm0".into()
    }
    /// (name, MMModemPortType): 6 QMI, 3 AT, 2 net, 8 audio.
    #[zbus(property)]
    fn ports(&self) -> Vec<(String, u32)> {
        vec![("cdc-wdm0".into(), 6), ("ttyUSB2".into(), 3), ("wwan0".into(), 2)]
    }
    #[zbus(property)]
    fn equipment_identifier(&self) -> String {
        "350000000000001".into()
    }
    /// MMModemLock: 1 none.
    #[zbus(property)]
    async fn unlock_required(&self) -> u32 {
        // MMModemLock: 1 none, 2 SIM PIN.
        if self.world.lock().await.locked {
            2
        } else {
            1
        }
    }
    #[zbus(property)]
    async fn unlock_retries(&self) -> HashMap<u32, u32> {
        HashMap::from([(2, self.world.lock().await.pin_retries)])
    }
    #[zbus(property)]
    async fn state(&self) -> i32 {
        modem_state(&*self.world.lock().await)
    }
    #[zbus(property)]
    fn state_failed_reason(&self) -> u32 {
        0
    }
    #[zbus(property)]
    async fn access_technologies(&self) -> u32 {
        let w = self.world.lock().await;
        if w.registered {
            w.tech
        } else {
            0
        }
    }
    #[zbus(property)]
    async fn signal_quality(&self) -> (u32, bool) {
        let w = self.world.lock().await;
        (if w.registered { w.quality } else { 0 }, true)
    }
    #[zbus(property)]
    async fn own_numbers(&self) -> Vec<String> {
        vec![self.world.lock().await.own_number.clone()]
    }
    /// MMModemPowerState: 3 on.
    #[zbus(property)]
    fn power_state(&self) -> u32 {
        3
    }
    /// (allowed, preferred) MMModemMode pairs (2 is 2G, 4 3G, 8 4G), as a modem
    /// with 2G, 3G and LTE offers them.
    #[zbus(property)]
    fn supported_modes(&self) -> Vec<(u32, u32)> {
        vec![(2 | 4 | 8, 8), (4 | 8, 8), (8, 0), (2 | 4, 4)]
    }
    #[zbus(property)]
    async fn current_modes(&self) -> (u32, u32) {
        self.world.lock().await.modes
    }
    async fn set_current_modes(&self, modes: (u32, u32)) -> zbus::fdo::Result<()> {
        if !self.supported_modes().contains(&modes) {
            return Err(zbus::fdo::Error::InvalidArgs("unsupported modes".into()));
        }
        self.world.lock().await.modes = modes;
        Ok(())
    }
    #[zbus(property)]
    fn supported_bands(&self) -> Vec<u32> {
        vec![31, 33, 37, 38, 57]
    }
    #[zbus(property)]
    fn current_bands(&self) -> Vec<u32> {
        vec![31, 33, 37, 38, 57]
    }
    /// MMBearerIpFamily: 1 v4, 2 v6.
    #[zbus(property)]
    fn supported_ip_families(&self) -> u32 {
        3
    }
}

pub struct Modem3gpp {
    pub world: Shared,
}

#[interface(name = "org.freedesktop.ModemManager1.Modem.Modem3gpp")]
impl Modem3gpp {
    fn register(&self, _operator_id: &str) {}
    fn scan(&self) -> Vec<HashMap<String, OwnedVariant>> {
        Vec::new()
    }

    #[zbus(property)]
    fn imei(&self) -> String {
        "350000000000001".into()
    }
    /// MMModem3gppRegistrationState: 1 home, 2 searching, 5 roaming.
    #[zbus(property)]
    async fn registration_state(&self) -> u32 {
        let w = self.world.lock().await;
        match (w.registered, w.roaming) {
            (false, _) => 2,
            (true, false) => 1,
            (true, true) => 5,
        }
    }
    #[zbus(property)]
    fn operator_code(&self) -> String {
        "50599".into()
    }
    #[zbus(property)]
    async fn operator_name(&self) -> String {
        let w = self.world.lock().await;
        if w.registered {
            w.operator.clone()
        } else {
            String::new()
        }
    }
    #[zbus(property)]
    fn enabled_facility_locks(&self) -> u32 {
        0
    }
    #[zbus(property)]
    fn eps_ue_mode_operation(&self) -> u32 {
        2
    }
    #[zbus(property)]
    fn pco(&self) -> Vec<(u32, bool, Vec<u8>)> {
        Vec::new()
    }
    #[zbus(property)]
    fn initial_eps_bearer(&self) -> OwnedObjectPath {
        path("/")
    }
    #[zbus(property)]
    fn initial_eps_bearer_settings(&self) -> HashMap<String, OwnedVariant> {
        HashMap::new()
    }
    /// MMModem3gppPacketServiceState: 2 attached.
    #[zbus(property)]
    fn packet_service_state(&self) -> u32 {
        2
    }
}

pub struct Voice {
    pub world: Shared,
}

#[interface(name = "org.freedesktop.ModemManager1.Modem.Voice")]
impl Voice {
    async fn list_calls(&self) -> Vec<OwnedObjectPath> {
        self.world.lock().await.calls.iter().map(|p| path(p)).collect()
    }

    async fn create_call(
        &self,
        properties: HashMap<String, OwnedVariant>,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(signal_emitter)] em: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        let number = string_arg(&properties, "number")
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs("number is required".into()))?;
        let p = {
            let mut w = self.world.lock().await;
            let p = w.call_path();
            w.calls.push(p.clone());
            p
        };
        let c = Call {
            world: self.world.clone(),
            path: p.clone(),
            state: cs::UNKNOWN,
            reason: reason::UNKNOWN,
            direction: 2,
            number: number.clone(),
            dtmf: String::new(),
            multiparty: false,
        };
        server.at(p.as_str(), c).await?;
        Self::call_added(&em, path(&p)).await?;
        self.calls_changed(&em).await?;
        tracing::info!(call = %p, number = %number, "outgoing call created");
        Ok(path(&p))
    }

    async fn delete_call(
        &self,
        call: OwnedObjectPath,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(signal_emitter)] em: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        let removed = {
            let mut w = self.world.lock().await;
            let before = w.calls.len();
            w.calls.retain(|p| p.as_str() != call.as_str());
            before != w.calls.len()
        };
        if !removed {
            return Err(zbus::fdo::Error::InvalidArgs("no such call".into()));
        }
        let _ = server.remove::<Call, _>(call.as_str()).await;
        Self::call_deleted(&em, call).await?;
        self.calls_changed(&em).await?;
        Ok(())
    }

    /// Hold the active call and accept the waiting or held one.
    async fn hold_and_accept(&self, #[zbus(connection)] conn: &Connection) -> zbus::fdo::Result<()> {
        let calls = self.world.lock().await.calls.clone();
        let mut states = Vec::new();
        for p in &calls {
            if let Some((s, _)) = call::state_of(conn, p).await {
                states.push((p.clone(), s));
            }
        }
        for (p, s) in &states {
            match *s {
                cs::ACTIVE => call::set_state(conn, p, cs::HELD, reason::UNKNOWN).await,
                cs::WAITING | cs::RINGING_IN | cs::HELD => call::set_state(conn, p, cs::ACTIVE, reason::ACCEPTED).await,
                _ => {}
            }
        }
        Ok(())
    }

    async fn hangup_and_accept(&self, #[zbus(connection)] conn: &Connection) -> zbus::fdo::Result<()> {
        let calls = self.world.lock().await.calls.clone();
        for p in &calls {
            match call::state_of(conn, p).await.map(|s| s.0) {
                Some(cs::ACTIVE) => call::set_state(conn, p, cs::TERMINATED, reason::TERMINATED).await,
                Some(cs::WAITING | cs::HELD) => call::set_state(conn, p, cs::ACTIVE, reason::ACCEPTED).await,
                _ => {}
            }
        }
        Ok(())
    }

    async fn hangup_all(&self, #[zbus(connection)] conn: &Connection) -> zbus::fdo::Result<()> {
        let calls = self.world.lock().await.calls.clone();
        for p in &calls {
            if call::state_of(conn, p).await.is_some_and(|s| s.0 != cs::TERMINATED) {
                call::set_state(conn, p, cs::TERMINATED, reason::TERMINATED).await;
            }
        }
        Ok(())
    }

    fn transfer(&self) -> zbus::fdo::Result<()> {
        Err(zbus::fdo::Error::NotSupported("transfer".into()))
    }

    async fn call_waiting_setup(&self, enable: bool) {
        self.world.lock().await.call_waiting = enable;
        tracing::info!(enable, "call waiting");
    }

    async fn call_waiting_query(&self) -> bool {
        self.world.lock().await.call_waiting
    }

    #[zbus(signal)]
    pub async fn call_added(em: &SignalEmitter<'_>, path: OwnedObjectPath) -> zbus::Result<()>;

    #[zbus(signal)]
    pub async fn call_deleted(em: &SignalEmitter<'_>, path: OwnedObjectPath) -> zbus::Result<()>;

    #[zbus(property)]
    async fn calls(&self) -> Vec<OwnedObjectPath> {
        self.world.lock().await.calls.iter().map(|p| path(p)).collect()
    }

    #[zbus(property)]
    async fn emergency_only(&self) -> bool {
        !self.world.lock().await.registered
    }
}

pub struct Messaging {
    pub world: Shared,
}

#[interface(name = "org.freedesktop.ModemManager1.Modem.Messaging")]
impl Messaging {
    async fn list(&self) -> Vec<OwnedObjectPath> {
        self.world.lock().await.sms.iter().map(|p| path(p)).collect()
    }

    async fn delete(
        &self,
        path_: OwnedObjectPath,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(signal_emitter)] em: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        let removed = {
            let mut w = self.world.lock().await;
            let before = w.sms.len();
            w.sms.retain(|p| p.as_str() != path_.as_str());
            before != w.sms.len()
        };
        if !removed {
            return Err(zbus::fdo::Error::InvalidArgs("no such message".into()));
        }
        let _ = server.remove::<Sms, _>(path_.as_str()).await;
        Self::deleted(&em, path_).await?;
        self.messages_changed(&em).await?;
        Ok(())
    }

    async fn create(
        &self,
        properties: HashMap<String, OwnedVariant>,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(signal_emitter)] em: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        let number = string_arg(&properties, "number")
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs("number is required".into()))?;
        let text =
            string_arg(&properties, "text").ok_or_else(|| zbus::fdo::Error::InvalidArgs("text is required".into()))?;
        let p = {
            let mut w = self.world.lock().await;
            let p = w.sms_path();
            w.sms.push(p.clone());
            p
        };
        let s = Sms {
            world: self.world.clone(),
            path: p.clone(),
            state: sms_state::UNKNOWN,
            pdu_type: 2,
            number,
            text,
            timestamp: String::new(),
            delivery_report: bool_arg(&properties, "delivery-report-request"),
            delivery: crate::world::DELIVERY_UNKNOWN,
        };
        server.at(p.as_str(), s).await?;
        Self::added(&em, path(&p), false).await?;
        self.messages_changed(&em).await?;
        Ok(path(&p))
    }

    #[zbus(signal)]
    pub async fn added(em: &SignalEmitter<'_>, path: OwnedObjectPath, received: bool) -> zbus::Result<()>;

    #[zbus(signal)]
    pub async fn deleted(em: &SignalEmitter<'_>, path: OwnedObjectPath) -> zbus::Result<()>;

    #[zbus(property)]
    async fn messages(&self) -> Vec<OwnedObjectPath> {
        self.world.lock().await.sms.iter().map(|p| path(p)).collect()
    }

    #[zbus(property)]
    fn supported_storages(&self) -> Vec<u32> {
        vec![1, 2]
    }

    #[zbus(property)]
    fn default_storage(&self) -> u32 {
        2
    }
}

pub struct Ussd {
    pub world: Shared,
}

#[interface(name = "org.freedesktop.ModemManager1.Modem.Modem3gpp.Ussd")]
impl Ussd {
    async fn initiate(&self, command: &str) -> zbus::fdo::Result<String> {
        tracing::info!(command, "USSD initiate");
        let w = self.world.lock().await;
        if !w.registered {
            return Err(zbus::fdo::Error::Failed("not registered".into()));
        }
        Ok(w.ussd_reply.clone())
    }

    async fn respond(&self, response: &str) -> String {
        tracing::info!(response, "USSD respond");
        format!("You chose {response}. Thank you.")
    }

    fn cancel(&self) {}

    /// MMModem3gppUssdSessionState: 1 idle.
    #[zbus(property)]
    async fn state(&self) -> u32 {
        self.world.lock().await.ussd_state
    }

    #[zbus(property)]
    fn network_notification(&self) -> String {
        String::new()
    }

    #[zbus(property)]
    fn network_request(&self) -> String {
        String::new()
    }
}

// Announcements made on behalf of the remote side, from the control interface.

pub async fn announce_call(conn: &Connection, p: &str) -> zbus::Result<()> {
    let iref = conn.object_server().interface::<_, Voice>(MODEM).await?;
    let em = iref.signal_emitter().clone();
    Voice::call_added(&em, path(p)).await?;
    let voice = iref.get().await;
    voice.calls_changed(&em).await
}

pub async fn announce_sms(conn: &Connection, p: &str) -> zbus::Result<()> {
    let iref = conn.object_server().interface::<_, Messaging>(MODEM).await?;
    let em = iref.signal_emitter().clone();
    Messaging::added(&em, path(p), true).await?;
    let messaging = iref.get().await;
    messaging.messages_changed(&em).await
}

pub async fn announce_signal(conn: &Connection) -> zbus::Result<()> {
    let iref = conn.object_server().interface::<_, Modem>(MODEM).await?;
    let em = iref.signal_emitter().clone();
    let m = iref.get().await;
    m.signal_quality_changed(&em).await?;
    m.access_technologies_changed(&em).await?;
    m.state_changed(&em).await?;
    let g = conn.object_server().interface::<_, Modem3gpp>(MODEM).await?;
    let gem = g.signal_emitter().clone();
    let g = g.get().await;
    g.registration_state_changed(&gem).await?;
    g.operator_name_changed(&gem).await
}

pub fn new_incoming(world: &Shared, p: &str, number: &str, state: i32) -> Call {
    Call {
        world: world.clone(),
        path: p.to_string(),
        state,
        reason: reason::INCOMING_NEW,
        direction: 1,
        number: number.to_string(),
        dtmf: String::new(),
        multiparty: false,
    }
}

pub fn new_received(world: &Shared, p: &str, number: &str, text: &str) -> Sms {
    Sms {
        world: world.clone(),
        path: p.to_string(),
        state: sms_state::RECEIVED,
        pdu_type: 1,
        number: number.to_string(),
        text: text.to_string(),
        timestamp: sms::timestamp_now(),
        delivery_report: false,
        delivery: crate::world::DELIVERED,
    }
}

/// Emergency alerts: ModemManager's CellBroadcast interface on the modem.
pub struct CellBroadcast {
    pub world: Shared,
}

#[interface(name = "org.freedesktop.ModemManager1.Modem.CellBroadcast")]
impl CellBroadcast {
    async fn list(&self) -> Vec<OwnedObjectPath> {
        self.world.lock().await.cbm.iter().map(|p| path(p)).collect()
    }

    async fn delete(
        &self,
        path_: OwnedObjectPath,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(signal_emitter)] em: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        self.world.lock().await.cbm.retain(|p| p.as_str() != path_.as_str());
        let _ = server.remove::<Cbm, _>(path_.as_str()).await;
        Self::deleted(&em, path_).await?;
        Ok(())
    }

    async fn set_channels(&self, channels: Vec<(u32, u32)>, #[zbus(signal_emitter)] em: SignalEmitter<'_>) {
        tracing::info!(?channels, "cell broadcast channels");
        self.world.lock().await.channels = channels;
        let _ = self.channels_changed(&em).await;
    }

    #[zbus(signal)]
    pub async fn added(em: &SignalEmitter<'_>, path: OwnedObjectPath) -> zbus::Result<()>;
    #[zbus(signal)]
    pub async fn deleted(em: &SignalEmitter<'_>, path: OwnedObjectPath) -> zbus::Result<()>;

    #[zbus(property)]
    async fn cell_broadcasts(&self) -> Vec<OwnedObjectPath> {
        self.world.lock().await.cbm.iter().map(|p| path(p)).collect()
    }

    #[zbus(property)]
    async fn channels(&self) -> Vec<(u32, u32)> {
        self.world.lock().await.channels.clone()
    }
}

/// One cell broadcast message.
pub struct Cbm {
    pub state: u32,
    pub text: String,
    pub channel: u32,
    pub message_code: u32,
    pub update: u32,
}

#[interface(name = "org.freedesktop.ModemManager1.Cbm")]
impl Cbm {
    #[zbus(property)]
    fn state(&self) -> u32 {
        self.state
    }
    #[zbus(property)]
    fn text(&self) -> String {
        self.text.clone()
    }
    #[zbus(property)]
    fn channel(&self) -> u32 {
        self.channel
    }
    #[zbus(property)]
    fn message_code(&self) -> u32 {
        self.message_code
    }
    #[zbus(property)]
    fn update(&self) -> u32 {
        self.update
    }
}

/// The interfaces a locked SIM hides. Not Voice: ModemManager keeps it while the
/// SIM is locked or missing so that emergency calls work (since 1.12, MR 176).
pub async fn lock_interfaces(conn: &Connection) {
    let s = conn.object_server();
    let _ = s.remove::<Messaging, _>(MODEM).await;
    let _ = s.remove::<Ussd, _>(MODEM).await;
    let _ = s.remove::<CellBroadcast, _>(MODEM).await;
    let _ = announce_signal(conn).await;
}

pub async fn unlock_interfaces(conn: &Connection, world: &Shared) {
    let s = conn.object_server();
    let _ = s.at(MODEM, Voice { world: world.clone() }).await;
    let _ = s.at(MODEM, Messaging { world: world.clone() }).await;
    let _ = s.at(MODEM, Ussd { world: world.clone() }).await;
    let _ = s.at(MODEM, CellBroadcast { world: world.clone() }).await;
    let _ = announce_signal(conn).await;
}
