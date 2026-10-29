//! The Dolphone API, served on the session bus. Each interface here matches a proxy
//! in phone-api, name for name.

use std::sync::Arc;

use phone_api::{
    BtDevice, CallInfo, CallRecordInfo, MessageInfo, ModemStatus, Placed, Request, RouteInfo, ThreadInfo, PATH,
};
use zbus::message::Header;
use zbus::object_server::SignalEmitter;
use zbus::{interface, Connection};

use crate::caller::{self, Caller};
use crate::phone::{to_info, Phone};

/// The caller, if it is one of Dolphone's own front ends.
async fn front_end(conn: &Connection, hdr: &Header<'_>, what: &str) -> zbus::fdo::Result<Caller> {
    let c = caller::identify(conn, hdr).await;
    if c.trusted {
        Ok(c)
    } else {
        Err(zbus::fdo::Error::AccessDenied(format!("only Dolphone's own apps can {what}")))
    }
}

/// The caller, if it may read messages and the call log.
async fn reader(conn: &Connection, hdr: &Header<'_>) -> zbus::fdo::Result<Caller> {
    let c = caller::identify(conn, hdr).await;
    if c.may_read() {
        Ok(c)
    } else {
        Err(zbus::fdo::Error::AccessDenied(format!(
            "{} may not read messages or calls; grant it in Phone Settings (read_grants)",
            c.name
        )))
    }
}

pub struct Calls(pub Arc<Phone>);

#[interface(name = "io.github.isaacrowntree.Dolphone1.Calls")]
impl Calls {
    async fn dial(
        &self,
        number: &str,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<Placed> {
        let who = caller::identify(conn, &hdr).await;
        self.0.dial(number, &who, false).await
    }

    async fn answer(
        &self,
        id: u32,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<()> {
        // Answering is not harmless: auto-answering a call the attacker placed makes
        // the device a room microphone.
        front_end(conn, &hdr, "answer calls").await?;
        self.0.answer(id).await
    }

    /// Anyone may hang up: ending a call can only ever stop something.
    async fn hangup(&self, id: u32) -> zbus::fdo::Result<()> {
        self.0.hangup(id).await
    }

    async fn swap(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<()> {
        front_end(conn, &hdr, "control calls").await?;
        self.0.swap().await
    }

    async fn send_dtmf(
        &self,
        id: u32,
        tones: &str,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<()> {
        front_end(conn, &hdr, "send keypad tones").await?;
        self.0.send_dtmf(id, tones).await
    }

    async fn set_muted(
        &self,
        muted: bool,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<()> {
        front_end(conn, &hdr, "mute calls").await?;
        self.0.set_muted(muted).await;
        Ok(())
    }

    async fn silence(&self) {
        self.0.audio.stop_ringing();
    }

    async fn calls(&self) -> Vec<CallInfo> {
        self.0.calls().await
    }

    async fn recents(
        &self,
        limit: u32,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<Vec<CallRecordInfo>> {
        reader(conn, &hdr).await?;
        let rows = self.0.store_read(|s| s.recents(limit.clamp(1, 500)))?;
        Ok(rows
            .into_iter()
            .map(|r| CallRecordInfo {
                id: r.id,
                peer: r.peer,
                direction: r.direction as u8,
                started: r.started,
                duration: r.duration,
                outcome: r.outcome as u8,
                seen: r.seen,
            })
            .collect())
    }

    async fn mark_calls_seen(
        &self,
        #[zbus(signal_emitter)] em: SignalEmitter<'_>,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<()> {
        front_end(conn, &hdr, "change the call log").await?;
        self.0.store_read(|s| s.mark_calls_seen())?;
        let _ = self.missed_changed(&em).await;
        Ok(())
    }

    #[zbus(property)]
    async fn missed(&self) -> u32 {
        self.0.store_read(|s| s.missed_calls()).unwrap_or(0)
    }

    #[zbus(signal)]
    async fn call_added(em: &SignalEmitter<'_>, call: CallInfo) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn call_changed(em: &SignalEmitter<'_>, call: CallInfo) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn call_removed(em: &SignalEmitter<'_>, id: u32) -> zbus::Result<()>;
}

pub struct Messages(pub Arc<Phone>);

#[interface(name = "io.github.isaacrowntree.Dolphone1.Messages")]
impl Messages {
    async fn send(
        &self,
        to: &str,
        body: &str,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<Placed> {
        let who = caller::identify(conn, &hdr).await;
        self.0.send(to, body, &who, false).await
    }

    async fn threads(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<Vec<ThreadInfo>> {
        reader(conn, &hdr).await?;
        let rows = self.0.store_read(|s| s.threads())?;
        Ok(rows
            .into_iter()
            .map(|t| ThreadInfo {
                peer: t.peer,
                last_body: t.last_body,
                last_at: t.last_at,
                last_direction: t.last_direction as u8,
                unread: t.unread,
            })
            .collect())
    }

    async fn conversation(
        &self,
        peer: &str,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<Vec<MessageInfo>> {
        reader(conn, &hdr).await?;
        Ok(self.0.store_read(|s| s.conversation(peer))?.iter().map(to_info).collect())
    }

    async fn mark_read(
        &self,
        peer: &str,
        #[zbus(signal_emitter)] em: SignalEmitter<'_>,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<()> {
        reader(conn, &hdr).await?;
        self.0.store_read(|s| s.mark_read(peer))?;
        let _ = self.unread_changed(&em).await;
        Ok(())
    }

    async fn delete_thread(
        &self,
        peer: &str,
        #[zbus(signal_emitter)] em: SignalEmitter<'_>,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<()> {
        front_end(conn, &hdr, "delete messages").await?;
        self.0.store_read(|s| s.delete_thread(peer))?;
        let _ = self.unread_changed(&em).await;
        Ok(())
    }

    #[zbus(property)]
    async fn unread(&self) -> u32 {
        self.0.store_read(|s| s.unread_messages()).unwrap_or(0)
    }

    #[zbus(signal)]
    async fn message_added(em: &SignalEmitter<'_>, message: MessageInfo) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn message_changed(em: &SignalEmitter<'_>, id: i64, state: u8) -> zbus::Result<()>;
}

pub struct Confirm(pub Arc<Phone>);

#[interface(name = "io.github.isaacrowntree.Dolphone1.Confirm")]
impl Confirm {
    /// What is waiting, with the text of a waiting message only for our own apps.
    async fn pending(&self, #[zbus(header)] hdr: Header<'_>, #[zbus(connection)] conn: &Connection) -> Vec<Request> {
        let c = caller::identify(conn, &hdr).await;
        let mut p = self.0.pending().await;
        if !c.trusted {
            for r in &mut p {
                r.body.clear();
            }
        }
        p
    }

    async fn confirm(
        &self,
        id: u32,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<Placed> {
        let who = caller::identify(conn, &hdr).await;
        self.0.confirm(id, &who).await
    }

    async fn deny(&self, id: u32) -> zbus::fdo::Result<()> {
        self.0.deny(id).await
    }

    #[zbus(signal)]
    async fn requested(em: &SignalEmitter<'_>, request: Request) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn resolved(em: &SignalEmitter<'_>, id: u32, confirmed: bool) -> zbus::Result<()>;
}

pub struct Audio(pub Arc<Phone>);

#[interface(name = "io.github.isaacrowntree.Dolphone1.Audio")]
impl Audio {
    async fn routes(&self) -> Vec<RouteInfo> {
        let p = self.0.clone();
        tokio::task::spawn_blocking(move || p.audio.routes()).await.unwrap_or_default()
    }

    async fn set_route(
        &self,
        route: u8,
        #[zbus(signal_emitter)] em: SignalEmitter<'_>,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<()> {
        front_end(conn, &hdr, "route call audio").await?;
        let p = self.0.clone();
        tokio::task::spawn_blocking(move || p.audio.set_route(phone_core::model::Route::from_u8(route)))
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?
            .map_err(zbus::fdo::Error::Failed)?;
        let _ = self.route_changed(&em).await;
        Ok(())
    }

    async fn cycle_route(
        &self,
        #[zbus(signal_emitter)] em: SignalEmitter<'_>,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<u8> {
        front_end(conn, &hdr, "route call audio").await?;
        let p = self.0.clone();
        let r = tokio::task::spawn_blocking(move || p.audio.cycle_route() as u8).await.unwrap_or(0);
        let _ = self.route_changed(&em).await;
        Ok(r)
    }

    #[zbus(property)]
    async fn route(&self) -> u8 {
        self.0.audio.route() as u8
    }

    #[zbus(property)]
    async fn ringer(&self) -> String {
        self.0.setting(("ringer", "ring"))
    }

    #[zbus(property)]
    async fn set_ringer(
        &mut self,
        mode: String,
        #[zbus(header)] hdr: Option<Header<'_>>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<()> {
        // A property write carries its message header when it comes over the bus.
        let hdr = hdr.ok_or_else(|| zbus::fdo::Error::AccessDenied("no caller".into()))?;
        front_end(conn, &hdr, "change the ringer").await?;
        if !matches!(mode.as_str(), "ring" | "vibrate" | "silent") {
            return Err(zbus::fdo::Error::InvalidArgs("ring, vibrate or silent".into()));
        }
        self.0.store_read(|s| s.set_setting("ringer", &mode))?;
        Ok(())
    }

    #[zbus(signal)]
    async fn routes_changed(em: &SignalEmitter<'_>) -> zbus::Result<()>;
}

pub struct Bluetooth(pub crate::bt::Bt);

#[interface(name = "io.github.isaacrowntree.Dolphone1.Bluetooth")]
impl Bluetooth {
    async fn devices(&self) -> Vec<BtDevice> {
        self.0.devices().await
    }

    async fn start_scan(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<()> {
        front_end(conn, &hdr, "scan for Bluetooth devices").await?;
        self.0.scan(true).await
    }

    async fn stop_scan(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<()> {
        front_end(conn, &hdr, "scan for Bluetooth devices").await?;
        self.0.scan(false).await
    }

    async fn pair(
        &self,
        address: &str,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<()> {
        front_end(conn, &hdr, "pair Bluetooth devices").await?;
        self.0.pair(address).await
    }

    async fn connect(
        &self,
        address: &str,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<()> {
        front_end(conn, &hdr, "connect Bluetooth devices").await?;
        self.0.connect(address, true).await
    }

    async fn disconnect(
        &self,
        address: &str,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<()> {
        front_end(conn, &hdr, "disconnect Bluetooth devices").await?;
        self.0.connect(address, false).await
    }

    async fn forget(
        &self,
        address: &str,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<()> {
        front_end(conn, &hdr, "forget Bluetooth devices").await?;
        self.0.forget(address).await
    }

    #[zbus(property)]
    async fn powered(&self) -> bool {
        self.0.powered().await
    }

    #[zbus(property)]
    async fn set_powered(
        &mut self,
        on: bool,
        #[zbus(header)] hdr: Option<Header<'_>>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<()> {
        // A property write carries its message header when it comes over the bus.
        let hdr = hdr.ok_or_else(|| zbus::fdo::Error::AccessDenied("no caller".into()))?;
        front_end(conn, &hdr, "switch Bluetooth").await?;
        self.0.set_powered(on).await
    }

    #[zbus(property)]
    async fn available(&self) -> bool {
        self.0.available().await
    }

    #[zbus(signal)]
    async fn devices_changed(em: &SignalEmitter<'_>) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn passkey(em: &SignalEmitter<'_>, address: &str, passkey: u32) -> zbus::Result<()>;
}

pub struct Modem(pub Arc<Phone>);

#[interface(name = "io.github.isaacrowntree.Dolphone1.Modem")]
impl Modem {
    async fn status(&self) -> ModemStatus {
        self.0.state.lock().await.status.clone()
    }

    async fn ussd(
        &self,
        code: &str,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<String> {
        let who = caller::identify(conn, &hdr).await;
        self.0.ussd(code, &who).await
    }

    async fn ussd_respond(
        &self,
        reply: &str,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<String> {
        let who = caller::identify(conn, &hdr).await;
        self.0.ussd_respond(reply, &who).await
    }

    async fn ussd_cancel(&self) -> zbus::fdo::Result<()> {
        self.0.ussd_cancel().await
    }

    #[zbus(signal)]
    async fn status_changed(em: &SignalEmitter<'_>, status: ModemStatus) -> zbus::Result<()>;
}

pub struct Settings(pub Arc<Phone>);

#[interface(name = "io.github.isaacrowntree.Dolphone1.Settings")]
impl Settings {
    async fn get(&self, key: &str) -> zbus::fdo::Result<String> {
        let default = phone_api::keys::ALL
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, d)| *d)
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs(format!("unknown setting {key}")))?;
        Ok(self.0.setting((key, default)))
    }

    async fn set(
        &self,
        key: &str,
        value: &str,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<()> {
        front_end(conn, &hdr, "change settings").await?;
        if key == phone_api::keys::READ_GRANTS.0 {
            caller::set_grants(value);
        }
        if !phone_api::keys::ALL.iter().any(|(k, _)| *k == key) {
            return Err(zbus::fdo::Error::InvalidArgs(format!("unknown setting {key}")));
        }
        self.0.store_read(|s| s.set_setting(key, value))
    }

    async fn block(
        &self,
        number: &str,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<()> {
        front_end(conn, &hdr, "change the block list").await?;
        let n = phone_core::number::canonical(number, &self.0.region());
        self.0.store_read(|s| s.block(&n))
    }

    async fn unblock(
        &self,
        number: &str,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<()> {
        front_end(conn, &hdr, "change the block list").await?;
        let n = phone_core::number::canonical(number, &self.0.region());
        self.0.store_read(|s| s.unblock(&n))
    }

    async fn blocked(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<Vec<String>> {
        reader(conn, &hdr).await?;
        self.0.store_read(|s| s.blocked())
    }

    async fn wipe(&self, #[zbus(header)] hdr: Header<'_>, #[zbus(connection)] conn: &Connection) -> Placed {
        // Always confirmed on the device, even from our own Settings app: there is
        // no undo.
        let who = caller::identify(conn, &hdr).await;
        self.0.request_wipe(&who).await
    }
}

// Emitting, for the parts of phoned that are not inside a method call.

fn emitter(conn: &Connection) -> Option<SignalEmitter<'static>> {
    SignalEmitter::new(conn, PATH).ok().map(|e| e.into_owned())
}

pub async fn emit_call_added(conn: &Connection, call: CallInfo) {
    if let Some(em) = emitter(conn) {
        let _ = Calls::call_added(&em, call).await;
    }
}

pub async fn emit_call_changed(conn: &Connection, call: CallInfo) {
    if let Some(em) = emitter(conn) {
        let _ = Calls::call_changed(&em, call).await;
    }
}

pub async fn emit_call_removed(conn: &Connection, id: u32) {
    if let Some(em) = emitter(conn) {
        let _ = Calls::call_removed(&em, id).await;
    }
}

/// Signals go to everything on the session bus, so a new message is announced
/// without its text (one-time codes arrive by text). Front ends fetch the
/// conversation, which only readers may.
pub async fn emit_message_added(conn: &Connection, mut m: MessageInfo) {
    m.body.clear();
    if let Some(em) = emitter(conn) {
        let _ = Messages::message_added(&em, m).await;
    }
}

pub async fn emit_message_changed(conn: &Connection, id: i64, state: u8) {
    if let Some(em) = emitter(conn) {
        let _ = Messages::message_changed(&em, id, state).await;
    }
}

pub async fn emit_requested(conn: &Connection, mut r: Request) {
    r.body.clear();
    if let Some(em) = emitter(conn) {
        let _ = Confirm::requested(&em, r).await;
    }
}

pub async fn emit_resolved(conn: &Connection, id: u32, confirmed: bool) {
    if let Some(em) = emitter(conn) {
        let _ = Confirm::resolved(&em, id, confirmed).await;
    }
}

pub async fn emit_status_changed(conn: &Connection, s: ModemStatus) {
    if let Some(em) = emitter(conn) {
        let _ = Modem::status_changed(&em, s).await;
    }
}

pub async fn emit_devices_changed(conn: &Connection) {
    if let Some(em) = emitter(conn) {
        let _ = Bluetooth::devices_changed(&em).await;
    }
}

pub async fn emit_passkey(conn: &Connection, address: &str, passkey: u32) {
    if let Some(em) = emitter(conn) {
        let _ = Bluetooth::passkey(&em, address, passkey).await;
    }
}

pub async fn emit_routes_changed(conn: &Connection) {
    if let Some(em) = emitter(conn) {
        let _ = Audio::routes_changed(&em).await;
    }
}

pub async fn emit_missed_changed(phone: &Arc<Phone>) {
    if let Ok(iref) = phone.session.object_server().interface::<_, Calls>(PATH).await {
        let em = iref.signal_emitter().clone();
        let calls = iref.get().await;
        let _ = calls.missed_changed(&em).await;
    }
}

pub async fn emit_unread_changed(phone: &Arc<Phone>) {
    if let Ok(iref) = phone.session.object_server().interface::<_, Messages>(PATH).await {
        let em = iref.signal_emitter().clone();
        let messages = iref.get().await;
        let _ = messages.unread_changed(&em).await;
    }
}
