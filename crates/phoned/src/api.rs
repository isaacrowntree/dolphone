//! The Dolphone API, served on the session bus. Each interface here matches a proxy
//! in phone-api, name for name.

use std::sync::Arc;

use phone_api::{
    AlertInfo, AuditEntry, BtDevice, CallInfo, CallRecordInfo, DeviceInfo, Grant, MessageInfo, ModemStatus, Placed,
    Request, RouteInfo, ThreadInfo, PATH,
};
use zbus::message::Header;
use zbus::object_server::SignalEmitter;
use zbus::{interface, Connection};

use phone_core::number;

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
/// A reader of messages, with the Messages PIN applied: while it is set and
/// Messages is not open, Dolphone's own apps are refused too (the PIN is about the
/// person holding the device). Programs granted reading are not asked.
async fn messages_reader(phone: &Phone, conn: &Connection, hdr: &Header<'_>) -> zbus::fdo::Result<Caller> {
    let c = reader(conn, hdr).await?;
    if c.trusted && phone.messages_locked() {
        return Err(zbus::fdo::Error::AccessDenied("Messages is locked: enter the PIN".into()));
    }
    Ok(c)
}

async fn reader(conn: &Connection, hdr: &Header<'_>) -> zbus::fdo::Result<Caller> {
    let c = caller::identify(conn, hdr).await;
    if c.may_read() {
        Ok(c)
    } else {
        Err(zbus::fdo::Error::AccessDenied(format!(
            "{} may not read messages or calls; ask with phonectl set read_grants {0} (allowed on the device)",
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
        let r = self.0.dial(number, &who, false).await;
        let said =
            if r.as_ref().is_ok_and(|p| p.pending > 0) { "waiting for confirmation".into() } else { outcome(&r) };
        self.0.audit(&who, "dial", number, &said);
        r
    }

    async fn answer(
        &self,
        id: u32,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<()> {
        // Answering is not harmless: auto-answering a call the attacker placed makes
        // the device a room microphone.
        let who = caller::identify(conn, &hdr).await;
        if !who.trusted {
            self.0.audit(&who, "answer", "", "refused");
        }
        front_end(conn, &hdr, "answer calls").await?;
        self.0.answer(id).await
    }

    /// Anyone may hang up: ending a call can only ever stop something.
    async fn hangup(
        &self,
        id: u32,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<()> {
        let who = caller::identify(conn, &hdr).await;
        let r = self.0.hangup(id).await;
        self.0.audit(&who, "hang up", "", &outcome(&r));
        r
    }

    /// Join the active and held calls into a conference.
    async fn merge(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<()> {
        front_end(conn, &hdr, "control calls").await?;
        self.0.merge().await
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
        self.0.stop_ringing();
    }

    /// The calls now. Who is calling is a contact's number: shown only to callers
    /// that may read the call log; others see that there is a call, and its state.
    async fn calls(&self, #[zbus(header)] hdr: Header<'_>, #[zbus(connection)] conn: &Connection) -> Vec<CallInfo> {
        let c = caller::identify(conn, &hdr).await;
        let mut calls = self.0.calls().await;
        if !c.may_read() {
            for call in &mut calls {
                call.number = number::mask(&call.number);
            }
        }
        calls
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
                sim: r.sim,
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
        let r = self.0.send(to, body, &who, false).await;
        if r.is_err() {
            self.0.audit(&who, "send", to, &outcome(&r));
        }
        r
    }

    /// An MMS: to several numbers (a group text), with files, or both.
    async fn send_mms(
        &self,
        to: Vec<String>,
        body: &str,
        files: Vec<String>,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<Placed> {
        let who = caller::identify(conn, &hdr).await;
        let r = self.0.send_mms(&to, body, &files, &who, false).await;
        if r.is_err() {
            self.0.audit(&who, "send", &to.join(","), &outcome(&r));
        }
        r
    }

    async fn retry(
        &self,
        id: i64,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<()> {
        front_end(conn, &hdr, "resend messages").await?;
        self.0.retry(id).await
    }

    async fn search(
        &self,
        query: &str,
        limit: u32,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<Vec<MessageInfo>> {
        let who = messages_reader(&self.0, conn, &hdr).await?;
        self.0.audit(&who, "search", "", "allowed");
        Ok(self.0.store_read(|s| s.search(query, limit.clamp(1, 500)))?.iter().map(to_info).collect())
    }

    /// The whole history, for moving it to another phone: a reader's, and logged.
    async fn export(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<String> {
        let who = messages_reader(&self.0, conn, &hdr).await?;
        self.0.audit(&who, "export", "", "allowed");
        let region = self.0.region();
        let book = phone_core::contacts::Book::load(&phone_core::store::contacts_dir(), &region);
        self.0.store_read(|s| {
            s.export_xml(|n| book.lookup(n).map(|c| c.name.clone()).unwrap_or_else(|| "(Unknown)".into()))
        })
    }

    /// Emergency alerts are public broadcasts: anyone may read them.
    async fn alerts(&self, limit: u32) -> zbus::fdo::Result<Vec<AlertInfo>> {
        self.0.alerts(limit)
    }

    async fn unlock(
        &self,
        pin: &str,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<bool> {
        front_end(conn, &hdr, "open Messages").await?;
        self.0.unlock_messages(pin).await
    }

    async fn lock(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<()> {
        front_end(conn, &hdr, "close Messages").await?;
        self.0.lock_messages().await;
        Ok(())
    }

    async fn locked(&self) -> bool {
        self.0.messages_locked()
    }

    #[zbus(signal)]
    async fn lock_changed(em: &SignalEmitter<'_>, locked: bool) -> zbus::Result<()>;

    async fn threads(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<Vec<ThreadInfo>> {
        messages_reader(&self.0, conn, &hdr).await?;
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
        let who = messages_reader(&self.0, conn, &hdr).await?;
        self.0.audit(&who, "read", peer, "allowed");
        let messages = self.0.store_read(|s| s.conversation(peer))?;
        self.0.show_parts(&messages);
        Ok(messages.iter().map(to_info).collect())
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
        // An MMS's pictures go with it.
        if let Ok(gone) = self.0.store_read(|s| s.conversation(peer)) {
            let _ = self.0.store_read(|s| {
                crate::phone::remove_parts(Some(s), &gone);
                Ok(())
            });
        }
        self.0.store_read(|s| s.delete_thread(peer))?;
        let _ = self.unread_changed(&em).await;
        Ok(())
    }

    #[zbus(property)]
    async fn unread(&self) -> u32 {
        self.0.store_read(|s| s.unread_messages()).unwrap_or(0)
    }

    #[zbus(signal)]
    async fn alert_received(em: &SignalEmitter<'_>, alert: AlertInfo) -> zbus::Result<()>;
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
                // Who is being called or texted is the owner's business; a program
                // sees its own requests in full.
                if r.requester != c.name && !c.may_read() {
                    r.target = number::mask(&r.target);
                }
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

    async fn cells(
        &self,
        limit: u32,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<Vec<(i64, String, String, bool)>> {
        let who = reader(conn, &hdr).await?;
        self.0.audit(&who, "cells", "", "allowed");
        self.0.store_read(|s| s.cells(limit.clamp(1, 2000)))
    }

    async fn unlock(
        &self,
        pin: &str,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<u32> {
        front_end(conn, &hdr, "unlock the SIM").await?;
        self.0.unlock(pin).await
    }

    async fn ussd_cancel(&self) -> zbus::fdo::Result<()> {
        self.0.ussd_cancel().await
    }

    /// The modem's one-time setup (USB audio, VoLTE, carrier profile), checked:
    /// (step, "ok"/"change"/"unsupported"/"error", in words). Reading it changes
    /// nothing, so anyone may.
    async fn setup(&self) -> Vec<(String, String, String)> {
        self.0.modem_setup(false).await
    }

    /// Make the setup's changes, which are saved in the modem and restart it. A
    /// front end does it now and gets the steps back; anyone else waits for a press.
    async fn apply_setup(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> (Placed, Vec<(String, String, String)>) {
        let who = caller::identify(conn, &hdr).await;
        if who.trusted {
            let steps = self.0.modem_setup(true).await;
            self.0.audit(&who, "modem setup", "", "done");
            (Placed { id: 1, pending: 0 }, steps)
        } else {
            (self.0.request_modem_setup(&who).await, Vec::new())
        }
    }

    #[zbus(signal)]
    async fn status_changed(em: &SignalEmitter<'_>, status: ModemStatus) -> zbus::Result<()>;
}

pub struct Settings(pub Arc<Phone>);

#[interface(name = "io.github.isaacrowntree.Dolphone1.Settings")]
impl Settings {
    async fn get(
        &self,
        key: &str,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<String> {
        use phone_api::keys;
        // Settings that name people (speed dial, who uplink alerts go to) or say
        // who may read: for callers that may read, as messages are.
        let personal =
            key == keys::READ_GRANTS.0 || key == keys::UPLINK_ALERT_TO.0 || keys::SPEED.iter().any(|k| k.0 == key);
        if personal {
            reader(conn, &hdr).await?;
        }
        if key == keys::REGION.0 {
            // The region phoned reads numbers in (the owner's, or the SIM's), so a
            // front end reads them the same way.
            return Ok(self.0.region());
        }
        let default = phone_api::keys::ALL
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, d)| *d)
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs(format!("unknown setting {key}")))?;
        if key == phone_api::keys::VOICEMAIL.0 {
            // The owner's number, or the SIM carrier's well-known one.
            return Ok(self.0.voicemail());
        }
        if key == phone_api::keys::MESSAGES_PIN.0 {
            // Never the hash: whether there is a PIN.
            return Ok(if self.0.setting((key, "")).is_empty() { String::new() } else { "set".into() });
        }
        if key == phone_api::keys::SIP_PASSWORD.0 {
            // Never the password itself: only whether there is one.
            return Ok(if crate::sip::has_password() { "set".into() } else { String::new() });
        }
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
        self.0.apply_setting(key, value).await
    }

    /// A setting changed for another program: it waits for the owner, as a text
    /// or a call does. Dolphone's own apps get it at once. Never the SIP password.
    async fn request_setting(
        &self,
        key: &str,
        value: &str,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<Placed> {
        let who = caller::identify(conn, &hdr).await;
        self.0.request_setting(key, value, &who).await
    }

    /// What the machine is taken to be. Anyone may ask: it is about the hardware.
    async fn device(&self) -> DeviceInfo {
        let (p, why) = self.0.device.lock().map(|d| d.clone()).unwrap_or_default();
        device_info(&p, &why)
    }

    async fn kdeconnect_devices(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<Vec<(String, String, bool)>> {
        front_end(conn, &hdr, "list KDE Connect devices").await?;
        Ok(self.0.kdeconnect.devices())
    }

    /// Anyone may unpair, as anyone may hang up: it only takes away.
    async fn kdeconnect_unpair(&self, id: &str) {
        self.0.kdeconnect.unpair(id);
    }

    async fn devices(&self) -> Vec<DeviceInfo> {
        crate::devices::all().iter().map(|p| device_info(p, "")).collect()
    }

    async fn request_grant(
        &self,
        numbers: &str,
        per_hour: u32,
        days: u32,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<Placed> {
        let who = caller::identify(conn, &hdr).await;
        self.0.request_grant(numbers, per_hour, days, &who).await
    }

    async fn grants(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<Vec<Grant>> {
        front_end(conn, &hdr, "list grants").await?;
        self.0.grants()
    }

    /// Anyone may give up a grant, as anyone may hang up: it only takes away.
    async fn revoke_grant(&self, id: i64) -> zbus::fdo::Result<()> {
        self.0.revoke_grant(id).await
    }

    async fn audit(
        &self,
        limit: u32,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<Vec<AuditEntry>> {
        front_end(conn, &hdr, "read the audit log").await?;
        self.0.audit_log(limit)
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

    async fn register(&self, #[zbus(header)] hdr: Header<'_>, #[zbus(connection)] conn: &Connection) -> bool {
        let c = caller::identify(conn, &hdr).await;
        let Some(sender) = hdr.sender() else { return false };
        if !c.may_read() {
            return false;
        }
        if let Ok(mut l) = LISTENERS.lock() {
            l.insert(sender.to_string());
        }
        true
    }

    async fn wipe(&self, #[zbus(header)] hdr: Header<'_>, #[zbus(connection)] conn: &Connection) -> Placed {
        // Always confirmed on the device, even from our own Settings app: there is
        // no undo.
        let who = caller::identify(conn, &hdr).await;
        self.0.request_wipe(&who).await
    }
}

fn device_info(p: &crate::devices::Profile, why: &str) -> DeviceInfo {
    DeviceInfo {
        id: p.id.clone(),
        name: p.name.clone(),
        status: p.status.clone(),
        why: why.to_string(),
        source: p.source.clone(),
        notes: p.notes.clone(),
        front_ends: p.front_ends.clone(),
    }
}

/// A zbus error in phoned's own words, without the D-Bus name in front.
pub fn short(e: &zbus::Error) -> String {
    match e {
        zbus::Error::MethodError(name, msg, _) => msg.clone().unwrap_or_else(|| name.to_string()),
        zbus::Error::FDO(f) => {
            f.to_string().split_once(": ").map(|(_, m)| m.to_string()).unwrap_or_else(|| f.to_string())
        }
        other => other.to_string(),
    }
}

/// How a request went, in a word for the audit log.
fn outcome<T>(r: &zbus::fdo::Result<T>) -> String {
    match r {
        Ok(_) => "done".into(),
        Err(zbus::fdo::Error::AccessDenied(_)) => "refused".into(),
        Err(zbus::fdo::Error::LimitsExceeded(_)) => "over the limit".into(),
        Err(_) => "failed".into(),
    }
}

// Emitting, for the parts of phoned that are not inside a method call.

/// The connections that registered for signals in full (Dolphone's own apps, and
/// programs that may read): by unique bus name. Pruned when one leaves the bus.
static LISTENERS: std::sync::Mutex<std::collections::BTreeSet<String>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

pub fn forget_listener(name: &str) {
    if let Ok(mut l) = LISTENERS.lock() {
        l.remove(name);
    }
}

fn listeners() -> Vec<String> {
    LISTENERS.lock().map(|l| l.iter().cloned().collect()).unwrap_or_default()
}

fn emitter(conn: &Connection) -> Option<SignalEmitter<'static>> {
    SignalEmitter::new(conn, PATH).ok().map(|e| e.into_owned())
}

/// A signal sent twice: in full to each registered listener, and to everyone else
/// as `masked` makes it.
macro_rules! private_signal {
    ($conn:expr, $value:expr, $mask:expr, |$em:ident, $v:ident| $send:expr) => {{
        let full = $value;
        for name in listeners() {
            if let (Some(e), Ok(dest)) = (emitter($conn), zbus::names::BusName::try_from(name.as_str())) {
                let $em = e.set_destination(dest.into_owned());
                let $v = full.clone();
                let _ = $send.await;
            }
        }
        if let Some($em) = emitter($conn) {
            let mut masked = full;
            $mask(&mut masked);
            let $v = masked;
            let _ = $send.await;
        }
    }};
}

pub async fn emit_call_added(conn: &Connection, call: CallInfo) {
    private_signal!(conn, call, |c: &mut CallInfo| c.number = number::mask(&c.number), |em, v| Calls::call_added(
        &em, v
    ));
}

pub async fn emit_call_changed(conn: &Connection, call: CallInfo) {
    private_signal!(conn, call, |c: &mut CallInfo| c.number = number::mask(&c.number), |em, v| {
        Calls::call_changed(&em, v)
    });
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
    private_signal!(
        conn,
        m,
        |m: &mut MessageInfo| {
            m.peer = m.peer.split(',').map(number::mask).collect::<Vec<_>>().join(",");
            m.parts.iter_mut().for_each(|p| p.1.clear());
        },
        |em, v| Messages::message_added(&em, v)
    );
}

pub async fn emit_alert_received(conn: &Connection, a: AlertInfo) {
    if let Some(em) = emitter(conn) {
        let _ = Messages::alert_received(&em, a).await;
    }
}

pub async fn emit_message_changed(conn: &Connection, id: i64, state: u8) {
    if let Some(em) = emitter(conn) {
        let _ = Messages::message_changed(&em, id, state).await;
    }
}

pub async fn emit_requested(conn: &Connection, mut r: Request) {
    r.body.clear();
    private_signal!(conn, r, |r: &mut Request| r.target = number::mask(&r.target), |em, v| Confirm::requested(&em, v));
}

pub async fn emit_resolved(conn: &Connection, id: u32, confirmed: bool) {
    if let Some(em) = emitter(conn) {
        let _ = Confirm::resolved(&em, id, confirmed).await;
    }
}

pub async fn emit_status_changed(conn: &Connection, s: ModemStatus) {
    private_signal!(conn, s, |s: &mut ModemStatus| s.own_number.clear(), |em, v| Modem::status_changed(&em, v));
}

pub async fn emit_lock_changed(conn: &Connection, locked: bool) {
    if let Some(em) = emitter(conn) {
        let _ = Messages::lock_changed(&em, locked).await;
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
