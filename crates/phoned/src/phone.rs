//! The phone: live calls, pending confirmations, and the rules between the API and
//! ModemManager.
//!
//! Everything that changes a call or a message goes through here, so the store, the
//! ringer, the call audio, the notifications and the signals to the front ends never
//! disagree about what is happening.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use phone_api::{CallInfo, MessageInfo, ModemStatus, Placed, Request};
use phone_core::guard::RateLimit;
use phone_core::model::{CallState, Direction, MessageState, Outcome};
use phone_core::number::{self, Class};
use phone_core::store::Store;
use phone_core::text;
use tokio::sync::Mutex;
use zbus::wire::{ObjectPath, OwnedObjectPath, Variant};

use crate::audio::Audio;
use crate::caller::Caller;
use crate::mm::{self, call_state as mcs};
use crate::{api, launch, notify};

pub struct Phone {
    /// Where our API is served and front ends listen.
    pub session: zbus::Connection,
    /// Where ModemManager and BlueZ are.
    pub system: zbus::Connection,
    pub store: std::sync::Mutex<Store>,
    pub state: Mutex<State>,
    pub audio: Audio,
    pub notify: notify::Notifier,
}

pub struct State {
    pub modem: Option<mm::Found>,
    pub calls: BTreeMap<u32, Live>,
    next_call: u32,
    pub requests: BTreeMap<u32, Pending>,
    next_request: u32,
    sms_limit: RateLimit,
    call_limit: RateLimit,
    pub status: ModemStatus,
    pub muted: bool,
}

/// A call phoned is tracking, and how it is going.
pub struct Live {
    pub info: CallInfo,
    pub mm_path: OwnedObjectPath,
    pub started: i64,
    /// We declined it, as opposed to the caller giving up: decides Declined against
    /// Missed in the log.
    pub declined: bool,
    pub ringing: bool,
}

/// An action waiting for a press.
pub struct Pending {
    pub request: Request,
    pub created: i64,
}

/// Pending confirmations expire, so a request nobody saw cannot be confirmed by
/// accident an hour later.
const REQUEST_TTL: i64 = 120;

impl Phone {
    pub fn new(session: zbus::Connection, system: zbus::Connection, store: Store) -> Arc<Self> {
        Arc::new(Self {
            notify: notify::Notifier::new(session.clone()),
            session,
            system,
            store: std::sync::Mutex::new(store),
            state: Mutex::new(State {
                modem: None,
                calls: BTreeMap::new(),
                next_call: 1,
                requests: BTreeMap::new(),
                next_request: 1,
                sms_limit: RateLimit::sms(),
                call_limit: RateLimit::calls(),
                status: no_modem(),
                muted: false,
            }),
            audio: Audio::new(),
        })
    }

    pub fn region(&self) -> String {
        self.setting(phone_api::keys::REGION)
    }

    pub fn setting(&self, key: (&str, &str)) -> String {
        self.store.lock().ok().and_then(|s| s.setting(key.0).ok().flatten()).unwrap_or_else(|| key.1.to_string())
    }

    pub fn flag(&self, key: (&str, &str)) -> bool {
        self.setting(key) == "true"
    }

    fn with_store<T>(&self, f: impl FnOnce(&Store) -> phone_core::store::Result<T>) -> Option<T> {
        let s = self.store.lock().ok()?;
        match f(&s) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::error!("store: {e}");
                None
            }
        }
    }

    async fn modem(&self) -> zbus::fdo::Result<mm::Found> {
        self.state
            .lock()
            .await
            .modem
            .clone()
            .ok_or_else(|| zbus::fdo::Error::Failed("no modem with voice or messaging".into()))
    }

    // Dialling

    /// Dial, or park the request for confirmation. `confirmed` is true when this is
    /// the second step of a confirmation.
    pub async fn dial(self: &Arc<Self>, raw: &str, caller: &Caller, confirmed: bool) -> zbus::fdo::Result<Placed> {
        let region = self.region();
        let canonical = number::canonical(raw, &region);
        if canonical.is_empty() {
            return Err(zbus::fdo::Error::InvalidArgs("empty number".into()));
        }
        let class = number::classify(&canonical, &region);
        if canonical.contains(['*', '#']) && class != Class::Emergency {
            return Err(zbus::fdo::Error::InvalidArgs("USSD codes go through Modem.Ussd".into()));
        }
        if !confirmed {
            if let Some(reason) = needs_confirmation(class, caller) {
                let id = self.park("call", &canonical, "", caller, reason).await;
                return Ok(Placed { id: 0, pending: id });
            }
        }
        if class != Class::Emergency {
            let mut st = self.state.lock().await;
            if let Err(wait) = st.call_limit.take(phone_core::now()) {
                return Err(zbus::fdo::Error::LimitsExceeded(format!("too many calls; try again in {wait}s")));
            }
        } else {
            tracing::warn!("emergency call placed");
        }
        let modem = self.modem().await?;
        if !modem.voice {
            return Err(zbus::fdo::Error::NotSupported("this modem has no voice support".into()));
        }
        let voice = mm::VoiceProxy::builder(&self.system).path(modem.path.clone()).build().await?;
        let path = voice.create_call(HashMap::from([("number", Variant::from(canonical.as_str()))])).await?;
        let id =
            self.track(&path, &canonical, Direction::Outgoing, CallState::Dialing, class == Class::Emergency).await;
        let call = mm::CallProxy::builder(&self.system).path(path.clone()).build().await?;
        if let Err(e) = call.start().await {
            tracing::error!("starting call: {e}");
            self.finish(id, Some(Outcome::Failed)).await;
            return Err(e.into());
        }
        tracing::info!(id, to = %number::mask(&canonical), "dialling");
        Ok(Placed { id: i64::from(id), pending: 0 })
    }

    /// Start tracking a call ModemManager has, and watch its state.
    async fn track(
        self: &Arc<Self>,
        path: &OwnedObjectPath,
        num: &str,
        dir: Direction,
        state: CallState,
        emergency: bool,
    ) -> u32 {
        let info;
        let id;
        {
            let mut st = self.state.lock().await;
            if let Some((&existing, _)) = st.calls.iter().find(|(_, c)| c.mm_path == *path) {
                return existing;
            }
            id = st.next_call;
            st.next_call += 1;
            info = CallInfo {
                id,
                number: num.to_string(),
                direction: dir as u8,
                state: state as u8,
                connected_at: 0,
                muted: false,
                emergency,
            };
            st.calls.insert(
                id,
                Live {
                    info: info.clone(),
                    mm_path: path.clone(),
                    started: phone_core::now(),
                    declined: false,
                    ringing: false,
                },
            );
        }
        api::emit_call_added(&self.session, info).await;
        let me = self.clone();
        let p = path.clone();
        tokio::spawn(async move { me.watch_call(id, p).await });
        id
    }

    async fn watch_call(self: Arc<Self>, id: u32, path: OwnedObjectPath) {
        let Ok(call) = mm::CallProxy::builder(&self.system).path(path.clone()).build().await else { return };
        let Ok(mut changes) = call.receive_call_state_changed().await else { return };
        // The state may have moved between creating the proxy and subscribing.
        if let Ok(now) = call.state().await {
            self.on_state(id, now, &call).await;
        }
        while let Some(sig) = changes.next().await {
            let Ok(args) = sig.args() else { continue };
            if !self.on_state(id, args.new, &call).await {
                break;
            }
        }
    }

    /// Apply a ModemManager call state. Returns false once the call is over.
    async fn on_state(self: &Arc<Self>, id: u32, mm_state: i32, call: &mm::CallProxy<'_>) -> bool {
        let new = match mm_state {
            mcs::DIALING => CallState::Dialing,
            mcs::RINGING_OUT => CallState::RingingOut,
            mcs::RINGING_IN => CallState::RingingIn,
            mcs::ACTIVE => CallState::Active,
            mcs::HELD => CallState::Held,
            mcs::WAITING => CallState::Waiting,
            mcs::TERMINATED => CallState::Terminated,
            _ => return true,
        };
        if new == CallState::Terminated {
            self.finish(id, None).await;
            return false;
        }
        let (info, became_active, any_ringing) = {
            let mut st = self.state.lock().await;
            let Some(live) = st.calls.get_mut(&id) else { return false };
            let was = CallState::from_u8(live.info.state);
            if was == new {
                return true;
            }
            live.info.state = new as u8;
            let became_active = new == CallState::Active && live.info.connected_at == 0;
            if became_active {
                live.info.connected_at = phone_core::now();
            }
            // An outgoing call's audio starts while it rings: VoLTE carries ringback,
            // "this number is disconnected" and carrier menus before anyone answers.
            let early_media = new == CallState::RingingOut && live.info.direction == Direction::Outgoing as u8;
            let became_active = became_active || early_media;
            let info = live.info.clone();
            let any_ringing = st.calls.values().any(|c| CallState::from_u8(c.info.state).ringing_in());
            (info, became_active, any_ringing)
        };
        if !any_ringing {
            self.audio.stop_ringing();
        }
        if became_active {
            let port = call.audio_port().await.unwrap_or_default();
            // Audio is subprocesses (PipeWire's tools), so it runs off the async
            // threads: a slow pw-dump must never hold up call signals.
            let me = self.clone();
            tokio::task::spawn_blocking(move || me.audio.start_call(&port));
            self.notify.withdraw_call(id).await;
        }
        api::emit_call_changed(&self.session, info).await;
        true
    }

    /// A call is over: log it, tear down its audio, tell everyone, and have
    /// ModemManager forget it.
    async fn finish(self: &Arc<Self>, id: u32, forced: Option<Outcome>) {
        let (live, others_active, any_ringing) = {
            let mut st = self.state.lock().await;
            let Some(live) = st.calls.remove(&id) else { return };
            let others = st.calls.values().any(|c| CallState::from_u8(c.info.state) == CallState::Active);
            let ringing = st.calls.values().any(|c| CallState::from_u8(c.info.state).ringing_in());
            (live, others, ringing)
        };
        if !any_ringing {
            self.audio.stop_ringing();
        }
        if !others_active {
            let me = self.clone();
            tokio::task::spawn_blocking(move || me.audio.stop_call());
        }
        let info = &live.info;
        let dir = if info.direction == 0 { Direction::Incoming } else { Direction::Outgoing };
        let connected = info.connected_at > 0;
        let outcome = forced.unwrap_or(match (dir, connected, live.declined) {
            (_, true, _) => Outcome::Answered,
            (Direction::Incoming, false, true) => Outcome::Declined,
            (Direction::Incoming, false, false) => Outcome::Missed,
            (Direction::Outgoing, false, _) => Outcome::Cancelled,
        });
        let duration = if connected { phone_core::now() - info.connected_at } else { 0 };
        self.with_store(|s| s.add_call(&info.number, dir, live.started, duration, outcome));
        tracing::info!(id, ?outcome, duration, "call ended");
        api::emit_call_removed(&self.session, id).await;
        api::emit_missed_changed(self).await;
        self.notify.withdraw_call(id).await;
        if outcome == Outcome::Missed {
            let name = self.name_for(&info.number);
            self.notify.missed_call(&name, &info.number).await;
        }
        if let Some(modem) = self.state.lock().await.modem.clone() {
            if let Ok(voice) = mm::VoiceProxy::builder(&self.system).path(modem.path).build().await {
                let _ = voice.delete_call(&ObjectPath::from(&live.mm_path)).await;
            }
        }
    }

    /// A new call from the network.
    pub async fn incoming(self: &Arc<Self>, path: OwnedObjectPath) {
        let Ok(call) = mm::CallProxy::builder(&self.system).path(path.clone()).build().await else { return };
        let Ok(direction) = call.direction().await else { return };
        if direction != 1 {
            return; // Ours: dial() is already tracking it.
        }
        if call.state().await.unwrap_or(mcs::TERMINATED) == mcs::TERMINATED {
            // A call that rang and ended while phoned was not watching (restarting,
            // or asleep): it was missed, and the owner should hear so. Then clear it
            // from the modem.
            let raw = call.number().await.unwrap_or_default();
            let num = if raw.is_empty() {
                String::new()
            } else {
                number::canonical(&text::clean(&raw, text::MAX_SENDER), &self.region())
            };
            self.with_store(|s| s.add_call(&num, Direction::Incoming, phone_core::now(), 0, Outcome::Missed));
            api::emit_missed_changed(self).await;
            self.notify.missed_call(&self.name_for(&num), &num).await;
            if let Some(modem) = self.state.lock().await.modem.clone() {
                if let Ok(voice) = mm::VoiceProxy::builder(&self.system).path(modem.path).build().await {
                    let _ = voice.delete_call(&ObjectPath::from(&path)).await;
                }
            }
            return;
        }
        let raw = call.number().await.unwrap_or_default();
        let region = self.region();
        let num = if raw.is_empty() {
            String::new()
        } else {
            number::canonical(&text::clean(&raw, text::MAX_SENDER), &region)
        };
        if !num.is_empty() && self.with_store(|s| s.is_blocked(&num)).unwrap_or(false) {
            tracing::info!(from = %number::mask(&num), "blocked caller rejected");
            let _ = call.hangup().await;
            self.with_store(|s| s.add_call(&num, Direction::Incoming, phone_core::now(), 0, Outcome::Blocked));
            return;
        }
        let state = match call.state().await.unwrap_or(mcs::RINGING_IN) {
            mcs::WAITING => CallState::Waiting,
            _ => CallState::RingingIn,
        };
        let id = self.track(&path, &num, Direction::Incoming, state, false).await;
        if let Some(live) = self.state.lock().await.calls.get_mut(&id) {
            live.ringing = true;
        }
        tracing::info!(id, from = %number::mask(&num), waiting = state == CallState::Waiting, "incoming call");
        let ringer = self.setting(("ringer", "ring"));
        let vibrate = self.flag(phone_api::keys::VIBRATE);
        // A waiting call beeps in the ear rather than ringing the room.
        self.audio.start_ringing(&ringer, state == CallState::Waiting);
        let _ = vibrate; // The Dialer vibrates the motor; it reads the same setting.
        launch::dialer();
        let name = self.name_for(&num);
        self.notify.incoming_call(id, &name, &num).await;
    }

    pub async fn answer(self: &Arc<Self>, id: u32) -> zbus::fdo::Result<()> {
        let (path, has_active) = {
            let st = self.state.lock().await;
            let live = st.calls.get(&id).ok_or_else(no_such_call)?;
            let active =
                st.calls.values().any(|c| c.info.id != id && CallState::from_u8(c.info.state) == CallState::Active);
            (live.mm_path.clone(), active)
        };
        self.audio.stop_ringing();
        if has_active {
            // Answering a waiting call holds the active one, as phones do.
            let modem = self.modem().await?;
            let voice = mm::VoiceProxy::builder(&self.system).path(modem.path).build().await?;
            voice.hold_and_accept().await?;
        } else {
            let call = mm::CallProxy::builder(&self.system).path(path).build().await?;
            call.accept().await?;
        }
        Ok(())
    }

    pub async fn hangup(self: &Arc<Self>, id: u32) -> zbus::fdo::Result<()> {
        let path = {
            let mut st = self.state.lock().await;
            let live = st.calls.get_mut(&id).ok_or_else(no_such_call)?;
            if CallState::from_u8(live.info.state).ringing_in() {
                live.declined = true;
            }
            live.mm_path.clone()
        };
        let call = mm::CallProxy::builder(&self.system).path(path).build().await?;
        call.hangup().await?;
        Ok(())
    }

    pub async fn swap(self: &Arc<Self>) -> zbus::fdo::Result<()> {
        let modem = self.modem().await?;
        let voice = mm::VoiceProxy::builder(&self.system).path(modem.path).build().await?;
        voice.hold_and_accept().await?;
        Ok(())
    }

    pub async fn send_dtmf(&self, id: u32, tones: &str) -> zbus::fdo::Result<()> {
        if tones.is_empty() || !tones.chars().all(|c| c.is_ascii_digit() || "*#ABCD".contains(c)) {
            return Err(zbus::fdo::Error::InvalidArgs("DTMF is 0-9, *, #, A-D".into()));
        }
        let path = self.state.lock().await.calls.get(&id).ok_or_else(no_such_call)?.mm_path.clone();
        let call = mm::CallProxy::builder(&self.system).path(path).build().await?;
        call.send_dtmf(tones).await?;
        Ok(())
    }

    pub async fn set_muted(self: &Arc<Self>, muted: bool) {
        let infos: Vec<CallInfo> = {
            let mut st = self.state.lock().await;
            st.muted = muted;
            st.calls
                .values_mut()
                .map(|c| {
                    c.info.muted = muted;
                    c.info.clone()
                })
                .collect()
        };
        let me = self.clone();
        tokio::task::spawn_blocking(move || me.audio.set_muted(muted));
        for info in infos {
            api::emit_call_changed(&self.session, info).await;
        }
    }

    pub async fn calls(&self) -> Vec<CallInfo> {
        self.state.lock().await.calls.values().map(|c| c.info.clone()).collect()
    }

    // Messages

    pub async fn send(
        self: &Arc<Self>,
        to: &str,
        body: &str,
        caller: &Caller,
        confirmed: bool,
    ) -> zbus::fdo::Result<Placed> {
        let region = self.region();
        let peer = number::canonical(to, &region);
        let body = text::clean(body, text::MAX_BODY);
        if peer.is_empty() || body.trim().is_empty() {
            return Err(zbus::fdo::Error::InvalidArgs("a text needs a number and a body".into()));
        }
        let class = number::classify(&peer, &region);
        if class == Class::Emergency {
            return Err(zbus::fdo::Error::NotSupported(
                "texting emergency numbers is not supported; call instead".into(),
            ));
        }
        if !confirmed {
            if let Some(reason) = needs_confirmation(class, caller) {
                let id = self.park("sms", &peer, &body, caller, reason).await;
                return Ok(Placed { id: 0, pending: id });
            }
        }
        {
            let mut st = self.state.lock().await;
            if let Err(wait) = st.sms_limit.take(phone_core::now()) {
                return Err(zbus::fdo::Error::LimitsExceeded(format!("too many texts; try again in {wait}s")));
            }
        }
        let modem = self.modem().await?;
        if !modem.messaging {
            return Err(zbus::fdo::Error::NotSupported("this modem has no messaging support".into()));
        }
        let now = phone_core::now();
        let id = self
            .with_store(|s| s.add_message(&peer, Direction::Outgoing, &body, now, MessageState::Sending))
            .ok_or_else(|| zbus::fdo::Error::Failed("could not store the message".into()))?;
        let stored = self.with_store(|s| s.message(id)).flatten();
        if let Some(m) = stored {
            api::emit_message_added(&self.session, to_info(&m)).await;
        }
        let me = self.clone();
        tokio::spawn(async move {
            let state = match me.transmit(&modem, &peer, &body).await {
                Ok(()) => MessageState::Sent,
                Err(e) => {
                    tracing::error!(to = %number::mask(&peer), "sending failed: {e}");
                    MessageState::Failed
                }
            };
            me.with_store(|s| s.set_message_state(id, state));
            api::emit_message_changed(&me.session, id, state as u8).await;
        });
        Ok(Placed { id, pending: 0 })
    }

    async fn transmit(&self, modem: &mm::Found, peer: &str, body: &str) -> zbus::Result<()> {
        let messaging = mm::MessagingProxy::builder(&self.system).path(modem.path.clone()).build().await?;
        let path =
            messaging.create(HashMap::from([("number", Variant::from(peer)), ("text", Variant::from(body))])).await?;
        let sms = mm::SmsProxy::builder(&self.system).path(path.clone()).build().await?;
        let result = sms.send().await;
        // Sent or not, the modem's copy is not needed: ours is in the store.
        let _ = messaging.delete(&ObjectPath::from(&path)).await;
        result
    }

    /// A text the modem has, from the network. Waits for a multipart message to
    /// finish arriving before reading it.
    pub async fn received(self: &Arc<Self>, path: OwnedObjectPath) {
        let Ok(sms) = mm::SmsProxy::builder(&self.system).path(path.clone()).build().await else { return };
        let modem = self.state.lock().await.modem.clone();
        let forget = |phone: Arc<Self>, modem: Option<mm::Found>, path: OwnedObjectPath| async move {
            if let Some(modem) = modem {
                if let Ok(m) = mm::MessagingProxy::builder(&phone.system).path(modem.path).build().await {
                    let _ = m.delete(&ObjectPath::from(&path)).await;
                }
            }
        };
        match sms.pdu_type().await.unwrap_or(0) {
            2 => return, // Outgoing: one of ours.
            // A delivery report, not a message: nothing to show, so nothing to keep.
            3 => return forget(self.clone(), modem, path).await,
            _ => {}
        }
        // A long text arrives in parts. Wait for the rest, generously: parts can be
        // minutes apart on a busy network, and giving up would leave it on the modem.
        let mut waited = Duration::ZERO;
        loop {
            match sms.state().await {
                Ok(mm::sms_state::RECEIVED) => break,
                Ok(mm::sms_state::RECEIVING) if waited < Duration::from_secs(600) => {
                    let step = if waited < Duration::from_secs(30) { 500 } else { 5000 };
                    tokio::time::sleep(Duration::from_millis(step)).await;
                    waited += Duration::from_millis(step);
                }
                Ok(mm::sms_state::RECEIVING) => {
                    tracing::warn!("a multipart text never completed; keeping what arrived");
                    break;
                }
                _ => return,
            }
        }
        let raw_from = sms.number().await.unwrap_or_default();
        let raw_text = sms.text().await.unwrap_or_default();
        let flash = sms.class().await.unwrap_or(-1) == 0;
        // The network's own time, so a backlog delivered at once keeps its order.
        let at =
            sms.timestamp().await.ok().and_then(|t| phone_core::when::parse_iso(&t)).unwrap_or_else(phone_core::now);
        let region = self.region();
        let from = number::canonical(&text::clean(&raw_from, text::MAX_SENDER), &region);
        let mut body = text::clean(&raw_text, text::MAX_BODY);
        if body.trim().is_empty() {
            // Most often an MMS notification (a picture, or a group chat), which
            // arrives as a binary push with no text. Say so rather than show nothing.
            body = "[Picture or group message: MMS is not supported yet]".into();
        }
        let delete = self.flag(phone_api::keys::DELETE_FROM_SIM);
        let private = self.flag(phone_api::keys::PRIVATE_NOTIFY);

        let handled = if self.with_store(|s| s.is_blocked(&from)).unwrap_or(false) {
            tracing::info!(from = %number::mask(&from), "text from blocked number dropped");
            true
        } else if flash {
            // A flash message is meant to be read at once and not kept.
            self.notify.message(&self.name_for(&from), &from, &body, private).await;
            true
        } else if self.with_store(|s| s.has_message(&from, Direction::Incoming, at, &body)).unwrap_or(false) {
            true // Already ours: the modem kept a copy.
        } else {
            match self.with_store(|s| s.add_message(&from, Direction::Incoming, &body, at, MessageState::Received)) {
                Some(id) => {
                    if let Some(m) = self.with_store(|s| s.message(id)).flatten() {
                        api::emit_message_added(&self.session, to_info(&m)).await;
                    }
                    api::emit_unread_changed(self).await;
                    tracing::info!(from = %number::mask(&from), "text received");
                    self.notify.message(&self.name_for(&from), &from, &body, private).await;
                    true
                }
                // Not stored: leave it on the modem, which still has it.
                None => false,
            }
        };
        // Only once it is safely ours: a modem whose storage fills stops accepting.
        if delete && handled {
            forget(self.clone(), modem, path).await;
        }
    }

    // Confirmations

    async fn park(&self, kind: &str, target: &str, body: &str, caller: &Caller, reason: &str) -> u32 {
        let request;
        {
            let mut st = self.state.lock().await;
            let now = phone_core::now();
            st.requests.retain(|_, p| now - p.created < REQUEST_TTL);
            let id = st.next_request;
            st.next_request += 1;
            request = Request {
                id,
                kind: kind.into(),
                target: target.into(),
                body: body.into(),
                requester: caller.name.clone(),
                reason: reason.into(),
            };
            st.requests.insert(id, Pending { request: request.clone(), created: now });
        }
        tracing::info!(id = request.id, kind, by = %caller.name, reason, "waiting for confirmation");
        api::emit_requested(&self.session, request.clone()).await;
        // Show it where the owner will see it: the panel (the Dialer draws requests)
        // and the desktop, when there is one.
        launch::dialer();
        self.notify.confirmation(&request, &self.name_for(target)).await;
        request.id
    }

    pub async fn pending(&self) -> Vec<Request> {
        let mut st = self.state.lock().await;
        let now = phone_core::now();
        st.requests.retain(|_, p| now - p.created < REQUEST_TTL);
        st.requests.values().map(|p| p.request.clone()).collect()
    }

    pub async fn confirm(self: &Arc<Self>, id: u32, confirmer: &Caller) -> zbus::fdo::Result<Placed> {
        if !confirmer.trusted {
            return Err(zbus::fdo::Error::AccessDenied("only Dolphone's own apps can confirm".into()));
        }
        let pending = {
            let mut st = self.state.lock().await;
            st.requests.remove(&id)
        }
        .ok_or_else(|| zbus::fdo::Error::InvalidArgs("no such request (it may have expired)".into()))?;
        api::emit_resolved(&self.session, id, true).await;
        self.notify.withdraw_request(id).await;
        let as_requester = Caller { name: pending.request.requester.clone(), trusted: false, reader: false };
        match pending.request.kind.as_str() {
            "call" => self.dial(&pending.request.target, &as_requester, true).await,
            "sms" => self.send(&pending.request.target, &pending.request.body, &as_requester, true).await,
            "wipe" => {
                self.with_store(|s| s.wipe());
                api::emit_unread_changed(self).await;
                api::emit_missed_changed(self).await;
                Ok(Placed { id: 1, pending: 0 })
            }
            _ => Err(zbus::fdo::Error::Failed("unknown request kind".into())),
        }
    }

    pub async fn deny(&self, id: u32) -> zbus::fdo::Result<()> {
        let removed = self.state.lock().await.requests.remove(&id);
        if removed.is_some() {
            api::emit_resolved(&self.session, id, false).await;
            self.notify.withdraw_request(id).await;
        }
        Ok(())
    }

    pub async fn request_wipe(&self, caller: &Caller) -> Placed {
        Placed { id: 0, pending: self.park("wipe", "", "", caller, "wipe").await }
    }

    /// The modem went away (reset, unplugged, ModemManager restarted): every call it
    /// had is over, whatever it last said, and its audio with it.
    pub async fn lost_modem(self: &Arc<Self>) {
        let ids: Vec<u32> = self.state.lock().await.calls.keys().copied().collect();
        for id in ids {
            self.finish(id, None).await;
        }
        self.audio.stop_ringing();
    }

    // Modem

    pub async fn refresh_status(self: &Arc<Self>) {
        let found = mm::find(&self.system).await;
        let status = match &found {
            None => no_modem(),
            Some(f) => read_status(&self.system, f).await,
        };
        let changed = {
            let mut st = self.state.lock().await;
            st.modem = found;
            let changed = st.status != status;
            st.status = status.clone();
            changed
        };
        if changed {
            api::emit_status_changed(&self.session, status).await;
        }
    }

    pub async fn ussd(&self, code: &str, caller: &Caller) -> zbus::fdo::Result<String> {
        if !caller.trusted {
            // A USSD code can change settings on the account or the SIM, and has
            // been used to do worse: only what the owner types on our dialer runs.
            return Err(zbus::fdo::Error::AccessDenied("USSD codes are only run from Dolphone's dialer".into()));
        }
        let code = code.trim();
        if code.is_empty() || !code.chars().all(|c| c.is_ascii_digit() || c == '*' || c == '#') {
            return Err(zbus::fdo::Error::InvalidArgs("a USSD code is digits, * and #".into()));
        }
        if number::is_supplementary(code) {
            // **21*number# and friends are not USSD: sent as USSD they would be
            // misread by the network. They need their own path, which is not built.
            return Err(zbus::fdo::Error::NotSupported(
                "call forwarding, waiting and caller-ID codes are not supported yet".into(),
            ));
        }
        let modem = self.modem().await?;
        if !modem.ussd {
            return Err(zbus::fdo::Error::NotSupported("this modem has no USSD support".into()));
        }
        let u = mm::UssdProxy::builder(&self.system).path(modem.path).build().await?;
        Ok(text::clean(&u.initiate(code).await?, text::MAX_BODY))
    }

    pub async fn ussd_respond(&self, reply: &str, caller: &Caller) -> zbus::fdo::Result<String> {
        if !caller.trusted {
            return Err(zbus::fdo::Error::AccessDenied("USSD is only run from Dolphone's dialer".into()));
        }
        let modem = self.modem().await?;
        let u = mm::UssdProxy::builder(&self.system).path(modem.path).build().await?;
        Ok(text::clean(&u.respond(reply).await?, text::MAX_BODY))
    }

    pub async fn ussd_cancel(&self) -> zbus::fdo::Result<()> {
        let modem = self.modem().await?;
        let u = mm::UssdProxy::builder(&self.system).path(modem.path).build().await?;
        u.cancel().await?;
        Ok(())
    }

    // Helpers

    pub fn name_for(&self, num: &str) -> String {
        let region = self.region();
        phone_core::contacts::Book::load(&phone_core::store::contacts_dir(), &region).name_for(num, &region)
    }

    pub fn store_read<T>(&self, f: impl FnOnce(&Store) -> phone_core::store::Result<T>) -> zbus::fdo::Result<T> {
        let s = self.store.lock().map_err(|_| zbus::fdo::Error::Failed("store poisoned".into()))?;
        f(&s).map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }
}

/// Why a request from `caller` for a number of `class` must wait, if it must.
pub fn needs_confirmation(class: Class, caller: &Caller) -> Option<&'static str> {
    match (class, caller.trusted) {
        // The owner typed it on our keypad: never delay an emergency call.
        (Class::Emergency, true) => None,
        (Class::Emergency, false) => Some("emergency"),
        (Class::Premium, _) => Some("premium"),
        (Class::Short, _) => Some("short"),
        (_, false) => Some("external"),
        (Class::International | Class::Ordinary, true) => None,
    }
}

fn no_such_call() -> zbus::fdo::Error {
    zbus::fdo::Error::InvalidArgs("no such call".into())
}

pub fn no_modem() -> ModemStatus {
    ModemStatus {
        present: false,
        registered: false,
        roaming: false,
        operator: String::new(),
        quality: 0,
        tech: "--".into(),
        voice: false,
        own_number: String::new(),
        emergency_only: true,
        state: String::new(),
    }
}

async fn read_status(system: &zbus::Connection, f: &mm::Found) -> ModemStatus {
    let mut s = no_modem();
    s.present = true;
    s.voice = f.voice;
    if let Ok(m) = mm::ModemProxy::builder(system).path(f.path.clone()).build().await {
        s.quality = m.signal_quality().await.map(|q| q.0.min(100) as u8).unwrap_or(0);
        s.tech = mm::tech_label(m.access_technologies().await.unwrap_or(0)).into();
        s.own_number = m.own_numbers().await.ok().and_then(|n| n.into_iter().next()).unwrap_or_default();
        let state = m.state().await.unwrap_or(0);
        s.registered = state >= 8;
        s.state = mm::state_label(state).into();
        if state == 3 {
            // Disabled: nobody else will turn it on, and a disabled modem never rings.
            tracing::info!("enabling the modem");
            let _ = m.enable(true).await;
        }
    }
    if let Ok(g) = mm::Modem3gppProxy::builder(system).path(f.path.clone()).build().await {
        let reg = g.registration_state().await.unwrap_or(0);
        s.roaming = reg == 5;
        s.registered = s.registered || matches!(reg, 1 | 5);
        s.operator = g.operator_name().await.unwrap_or_default();
    }
    if f.voice {
        if let Ok(v) = mm::VoiceProxy::builder(system).path(f.path.clone()).build().await {
            s.emergency_only = v.emergency_only().await.unwrap_or(!s.registered);
        }
    } else {
        s.emergency_only = !s.registered;
    }
    s
}

pub fn to_info(m: &phone_core::model::Message) -> MessageInfo {
    MessageInfo {
        id: m.id,
        peer: m.peer.clone(),
        direction: m.direction as u8,
        body: m.body.clone(),
        at: m.at,
        state: m.state as u8,
        read: m.read,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caller(trusted: bool) -> Caller {
        Caller { name: if trusted { "dolphone-dialer" } else { "python3" }.into(), trusted, reader: false }
    }

    #[test]
    fn confirmation_rules() {
        assert_eq!(needs_confirmation(Class::Emergency, &caller(true)), None);
        assert_eq!(needs_confirmation(Class::Emergency, &caller(false)), Some("emergency"));
        assert_eq!(needs_confirmation(Class::Ordinary, &caller(true)), None);
        assert_eq!(needs_confirmation(Class::International, &caller(true)), None);
        assert_eq!(needs_confirmation(Class::Ordinary, &caller(false)), Some("external"));
        assert_eq!(needs_confirmation(Class::Premium, &caller(true)), Some("premium"));
        assert_eq!(needs_confirmation(Class::Short, &caller(true)), Some("short"));
    }
}
