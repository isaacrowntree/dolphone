//! The phone: live calls, pending confirmations, and the rules between the API and
//! ModemManager.
//!
//! Everything that changes a call or a message goes through here, so the store, the
//! ringer, the call audio, the notifications and the signals to the front ends never
//! disagree about what is happening.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use phone_api::{AlertInfo, CallInfo, MessageInfo, ModemStatus, Placed, Request};
use phone_core::guard::RateLimit;
use phone_core::model::{AlertLevel, CallState, Direction, MessageState, Outcome};
use phone_core::number::{self, Class};
use phone_core::store::Store;
use phone_core::text;
use phone_core::tones::Progress;
use tokio::sync::Mutex;
use zbus::wire::{ObjectPath, OwnedObjectPath, Variant};

use crate::audio::{Audio, SipAudio, Tone, SIP_PORT};
use crate::caller::Caller;
use crate::mm::{self, call_state as mcs};
use crate::{api, launch, media::Media, notify, power::Awake, rules, sip};

/// Seconds on a clock that only moves forward, for rate limits and expiry: the wall
/// clock can jump when the network or NTP sets it, and a jump must not lock out
/// calls or let a request outlive its time.
pub fn mono() -> i64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs() as i64
}

/// ModemManager's call state as ours, or None for one we do not show.
fn call_state_of(mm_state: i32) -> Option<CallState> {
    Some(match mm_state {
        mcs::DIALING => CallState::Dialing,
        mcs::RINGING_OUT => CallState::RingingOut,
        mcs::RINGING_IN => CallState::RingingIn,
        mcs::ACTIVE => CallState::Active,
        mcs::HELD => CallState::Held,
        mcs::WAITING => CallState::Waiting,
        mcs::TERMINATED => CallState::Terminated,
        _ => return None,
    })
}

pub struct Phone {
    /// Where our API is served and front ends listen.
    pub session: zbus::Connection,
    /// Where ModemManager and BlueZ are.
    pub system: zbus::Connection,
    pub store: std::sync::Mutex<Store>,
    pub state: Mutex<State>,
    pub audio: Audio,
    pub notify: notify::Notifier,
    /// Sleep is held off while any call exists.
    pub awake: Awake,
    /// Music paused for a call, to resume after.
    pub media: Media,
    /// The SIM's own network (MCC+MNC), once known: region and voicemail defaults.
    pub sim_network: std::sync::Mutex<String>,
    /// Calls over the internet, when an account is set up.
    pub sip: sip::Sip,
    /// Poked when the SIP settings change, so baresip restarts with them.
    pub sip_changed: Arc<tokio::sync::Notify>,
    /// Whether the SIP call's microphone is muted, as baresip has it (its mute
    /// command toggles).
    sip_muted: Mutex<bool>,
    /// Laptops paired over KDE Connect.
    pub kdeconnect: crate::kdeconnect::KdeConnect,
    /// The device profile in use, and why it was chosen.
    pub device: std::sync::Mutex<(crate::devices::Profile, String)>,
    /// The contacts, read once and again only when their directory changes: a
    /// ringing call or an arriving text should not wait on reading every card.
    book: std::sync::Mutex<Option<BookCache>>,
    /// Held while the modem's status is read, so reads do not overlap.
    refreshing: Mutex<()>,
}

/// The contacts as last read: for which region, and the directory's modification
/// time then (files are replaced by rename, which changes it).
struct BookCache {
    region: String,
    mtime: Option<std::time::SystemTime>,
    book: Arc<phone_core::contacts::Book>,
}

pub struct State {
    pub modem: Option<mm::Found>,
    pub calls: BTreeMap<u32, Live>,
    next_call: u32,
    pub requests: BTreeMap<u32, Pending>,
    next_request: u32,
    sms_limit: RateLimit,
    call_limit: RateLimit,
    /// Each standing grant's own hourly limit.
    grant_limits: HashMap<i64, RateLimit>,
    pub status: ModemStatus,
    pub muted: bool,
    /// Calls just ended, by ModemManager path, for a minute: a late signal or a
    /// resync for one of them must not log it again as missed.
    finished: HashMap<OwnedObjectPath, Instant>,
    /// baresip's events for a SIP call not tracked yet, by its id, for a while.
    sip_early: HashMap<String, (Instant, Vec<sip::CallEvent>)>,
    /// Texts over SIP come from anyone who can reach baresip: a limit on them.
    sip_text_limit: RateLimit,
    /// Texts being read now, by path: the same text reported twice (a resync while
    /// a multipart one is still arriving) is handled once.
    texts_in_flight: std::collections::HashSet<OwnedObjectPath>,
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
    /// When it connected, on the monotonic clock, for an honest duration.
    pub connected: Option<Instant>,
    /// It rang silently (an unknown caller with silencing on).
    pub silenced: bool,
    /// baresip's id, for a call over SIP; None for a cellular call.
    pub sip: Option<String>,
    /// Where its audio is (the modem's audio port, or SIP_PORT), once connected,
    /// so a swap between networks can move the audio with it.
    pub port: String,
    /// The sample rate of its serial audio, from the call's AudioFormat (0: not
    /// said, 8 kHz): ModemManager may switch a SIM7600 to 16 kHz.
    pub rate: u32,
}

/// An action waiting for a press.
pub struct Pending {
    pub request: Request,
    pub created: i64,
    /// For an MMS: its text and files, as asked (the request's body is what the
    /// owner is shown).
    pub mms: Option<(String, Vec<String>)>,
}

/// Pending confirmations expire, so a request nobody saw cannot be confirmed by
/// accident an hour later.
const REQUEST_TTL: i64 = 120;

impl Phone {
    pub fn new(session: zbus::Connection, system: zbus::Connection, store: Store) -> Arc<Self> {
        let chosen = store.setting(phone_api::keys::DEVICE_PROFILE.0).ok().flatten().unwrap_or_default();
        let (profile, why) = crate::devices::choose(&crate::devices::all(), &crate::devices::Facts::read(), &chosen);
        tracing::info!(device = %profile.id, status = %profile.status, why = %why, "device profile");
        let audio = Audio::new(crate::devices::audio_config(&profile));
        Arc::new(Self {
            notify: notify::Notifier::new(session.clone()),
            sip: sip::Sip::new(session.clone()),
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
                grant_limits: HashMap::new(),
                status: no_modem(),
                muted: false,
                finished: HashMap::new(),
                sip_early: HashMap::new(),
                sip_text_limit: RateLimit::new(30, 3600),
                texts_in_flight: Default::default(),
            }),
            audio,
            device: std::sync::Mutex::new((profile, why)),
            kdeconnect: Default::default(),
            awake: Awake::default(),
            media: Media::default(),
            sim_network: std::sync::Mutex::new(String::new()),
            sip_changed: Arc::new(tokio::sync::Notify::new()),
            sip_muted: Mutex::new(false),
            book: std::sync::Mutex::new(None),
            refreshing: Mutex::new(()),
        })
    }

    /// The region numbers are read in: the owner's setting when they chose one,
    /// otherwise the SIM's own country, otherwise the default.
    pub fn region(&self) -> String {
        let chosen = self.store.lock().ok().and_then(|s| s.setting(phone_api::keys::REGION.0).ok().flatten());
        if let Some(r) = chosen {
            return r;
        }
        let net = self.sim_network.lock().map(|n| n.clone()).unwrap_or_default();
        mm::region_for_mcc(net.get(..3).unwrap_or("")).unwrap_or(phone_api::keys::REGION.1).to_string()
    }

    /// The voicemail number: the owner's, or the SIM carrier's well-known one.
    pub fn voicemail(&self) -> String {
        let chosen = self.setting(phone_api::keys::VOICEMAIL);
        if !chosen.is_empty() {
            return chosen;
        }
        let net = self.sim_network.lock().map(|n| n.clone()).unwrap_or_default();
        mm::voicemail_for(&net).unwrap_or_default().to_string()
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
        // "#31#0412…" hides the number for this one call, "*31#…" shows it: the
        // number itself is what is classified and dialled (000 behind it is still
        // an emergency call).
        let (clir_once, raw) = phone_core::ss::strip_clir_prefix(raw);
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
            if let Err(wait) = st.call_limit.take(mono()) {
                return Err(zbus::fdo::Error::LimitsExceeded(format!("too many calls; try again in {wait}s")));
            }
        } else {
            tracing::warn!("emergency call placed");
        }
        if self.use_sip(raw, class).await {
            return self.dial_sip(raw, &canonical).await;
        }
        let modem = self.modem().await?;
        if !modem.voice {
            return Err(zbus::fdo::Error::NotSupported("this modem has no voice support".into()));
        }
        if let Some(hide) = clir_once.filter(|_| class != Class::Emergency) {
            self.clir_for_one_call(&modem, hide).await;
        }
        let voice = mm::VoiceProxy::builder(&self.system).path(modem.path.clone()).build().await?;
        let path = voice.create_call(HashMap::from([("number", Variant::from(canonical.as_str()))])).await?;
        let id = self
            .track(&path, &canonical, Direction::Outgoing, CallState::Dialing, class == Class::Emergency, None)
            .await;
        self.media.pause_all(&self.session).await;
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
        sip: Option<String>,
    ) -> u32 {
        let info;
        let id;
        {
            let mut st = self.state.lock().await;
            let same = |c: &Live| match (&sip, &c.sip) {
                (Some(a), Some(b)) => a == b,
                (None, None) => c.mm_path == *path,
                _ => false,
            };
            if let Some((&existing, _)) = st.calls.iter().find(|(_, c)| same(c)) {
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
                internet: sip.is_some(),
            };
            st.calls.insert(
                id,
                Live {
                    info: info.clone(),
                    mm_path: path.clone(),
                    started: phone_core::now(),
                    declined: false,
                    ringing: false,
                    connected: None,
                    silenced: false,
                    sip: sip.clone(),
                    port: String::new(),
                    rate: 0,
                },
            );
        }
        self.awake.hold(&self.system).await;
        api::emit_call_added(&self.session, info).await;
        // A SIP call's states come as baresip's events, not from ModemManager.
        if sip.is_none() {
            let me = self.clone();
            let p = path.clone();
            tokio::spawn(async move { me.watch_call(id, p).await });
        }
        id
    }

    async fn watch_call(self: Arc<Self>, id: u32, path: OwnedObjectPath) {
        if !self.follow_call(id, &path).await {
            // The call can no longer be followed (its object is gone, or the bus
            // dropped): a call left in the list would hold the board awake and
            // make every later call "waiting" instead of ringing.
            tracing::warn!(id, "lost track of a call; ending it here");
            self.finish(id, Some(Outcome::Failed)).await;
        }
    }

    /// Follow one call's states. True when it ended normally.
    async fn follow_call(self: &Arc<Self>, id: u32, path: &OwnedObjectPath) -> bool {
        let Ok(call) = mm::CallProxy::builder(&self.system).path(path.clone()).build().await else { return false };
        let Ok(mut changes) = call.receive_call_state_changed().await else { return false };
        // The state may have moved between creating the proxy and subscribing.
        if let Ok(now) = call.state().await {
            if !self.on_state(id, now, 0, &call).await {
                return true;
            }
        }
        while let Some(sig) = changes.next().await {
            let Ok(args) = sig.args() else { continue };
            if !self.on_state(id, args.new, args.reason, &call).await {
                return true;
            }
        }
        false
    }

    /// Apply a ModemManager call state. Returns false once the call is over.
    async fn on_state(self: &Arc<Self>, id: u32, mm_state: i32, reason: i32, call: &mm::CallProxy<'_>) -> bool {
        let Some(new) = call_state_of(mm_state) else { return true };
        let refused = match reason {
            mm::call_reason::REFUSED_OR_BUSY => Some(Progress::Busy),
            mm::call_reason::ERROR => Some(Progress::Congestion),
            _ => None,
        };
        let port = if matches!(new, CallState::Active | CallState::RingingOut) {
            call.audio_port().await.unwrap_or_default()
        } else {
            String::new()
        };
        if new == CallState::Active {
            let rate = mm::audio_rate(call).await;
            if let Some(live) = self.state.lock().await.calls.get_mut(&id) {
                live.rate = rate;
            }
            if port.is_empty() {
                // ModemManager fills in AudioPort only once its own in-call setup
                // is done, just after the call goes active: look again shortly.
                let me = self.clone();
                let path = call.inner().path().to_owned();
                tokio::spawn(async move { me.recheck_port(id, path.into()).await });
            }
        }
        self.set_state(id, new, refused, port).await
    }

    /// Read a connected call's audio port again, and move its audio there when it
    /// turns up.
    async fn recheck_port(self: &Arc<Self>, id: u32, path: OwnedObjectPath) {
        for wait in [500, 1500] {
            tokio::time::sleep(Duration::from_millis(wait)).await;
            let Ok(call) = mm::CallProxy::builder(&self.system)
                .path(path.clone())
                .cache_properties(zbus::proxy::CacheProperties::No)
                .build()
                .await
            else {
                return;
            };
            let port = call.audio_port().await.unwrap_or_default();
            let rate = mm::audio_rate(&call).await;
            let moved = {
                let mut st = self.state.lock().await;
                let Some(live) = st.calls.get_mut(&id) else { return };
                let moved = !port.is_empty() && live.port != port;
                if moved {
                    live.port = port.clone();
                    live.rate = rate;
                }
                moved
            };
            if moved {
                tracing::info!(id, "the call's audio port appeared; moving its audio there");
                self.audio.request_start_call(&port, rate);
                return;
            }
        }
    }

    /// A call moved to `new`, whichever network it is on. `refused` is how the
    /// network turned an unanswered call away, for the tone to play; `port` the
    /// audio to bridge once it connects. Returns false once the call is over.
    async fn set_state(self: &Arc<Self>, id: u32, new: CallState, refused: Option<Progress>, port: String) -> bool {
        if new == CallState::Terminated {
            self.audio.stop_progress();
            let unanswered_outgoing = self
                .state
                .lock()
                .await
                .calls
                .get(&id)
                .is_some_and(|c| c.info.direction == Direction::Outgoing as u8 && c.info.connected_at == 0);
            self.finish(id, None).await;
            // The network turned the call away: tell the caller the way their
            // country's phones do, in the ear, after the line has gone.
            if let (true, Some(t)) = (unanswered_outgoing, refused) {
                self.audio.start_progress(&self.region(), t);
            }
            return false;
        }
        if new == CallState::RingingOut && self.flag(phone_api::keys::LOCAL_RINGBACK) {
            self.audio.start_progress(&self.region(), Progress::Ringback);
        } else if new == CallState::Active {
            self.audio.stop_progress();
        }
        let (info, became_active, any_ringing, rate) = {
            let mut st = self.state.lock().await;
            let Some(live) = st.calls.get_mut(&id) else { return false };
            let was = CallState::from_u8(live.info.state);
            // A call phoned marked waiting (another network's call is live) stays
            // waiting when the modem, which cannot see that call, says ringing.
            if was == new || (was == CallState::Waiting && new == CallState::RingingIn) {
                return true;
            }
            // A connected call never goes back to ringing or dialling: a late or
            // replayed progress event (SIP's 180 after its 200) must not undo it.
            if matches!(was, CallState::Active | CallState::Held)
                && matches!(new, CallState::Dialing | CallState::RingingOut | CallState::RingingIn)
            {
                return true;
            }
            live.info.state = new as u8;
            let became_active = new == CallState::Active && live.info.connected_at == 0;
            // A call first seen on hold (adopted after a restart) was connected.
            if (became_active || new == CallState::Held) && live.info.connected_at == 0 {
                live.info.connected_at = phone_core::now();
                live.connected = Some(Instant::now());
            }
            if !port.is_empty() {
                live.port = port.clone();
            }
            // An outgoing call's audio starts while it rings: VoLTE carries ringback,
            // "this number is disconnected" and carrier menus before anyone answers.
            let early_media = new == CallState::RingingOut && live.info.direction == Direction::Outgoing as u8;
            let became_active = became_active || early_media;
            let info = live.info.clone();
            let rate = live.rate;
            let any_ringing = st.calls.values().any(|c| CallState::from_u8(c.info.state).ringing_in());
            (info, became_active, any_ringing, rate)
        };
        if !any_ringing {
            self.stop_ringing();
        }
        if became_active {
            // Audio is subprocesses (PipeWire's tools), so it runs off the async
            // threads: a slow pw-dump must never hold up call signals.
            let sip = port == SIP_PORT;
            // In order with any stop that follows: Audio runs its requests on one
            // worker, so a call that ends at once cannot leave its audio running.
            self.audio.request_start_call(&port, rate);
            self.notify.withdraw_call(id).await;
            if new == CallState::Active {
                self.kdeconnect.talking(&info.number, &self.name_for(&info.number));
                if !sip {
                    self.call_at(true);
                }
            }
        }
        api::emit_call_changed(&self.session, info).await;
        true
    }

    /// A call is over: log it, tear down its audio, tell everyone, and have
    /// ModemManager forget it.
    async fn finish(self: &Arc<Self>, id: u32, forced: Option<Outcome>) {
        let (live, others_active, any_ringing, none_left) = {
            let mut st = self.state.lock().await;
            let Some(live) = st.calls.remove(&id) else { return };
            st.finished.retain(|_, at| at.elapsed() < Duration::from_secs(60));
            st.finished.insert(live.mm_path.clone(), Instant::now());
            let others = st.calls.values().any(|c| CallState::from_u8(c.info.state) == CallState::Active);
            let ringing = st.calls.values().any(|c| CallState::from_u8(c.info.state).ringing_in());
            (live, others, ringing, st.calls.is_empty())
        };
        if none_left {
            self.awake.release();
            self.media.resume(&self.session).await;
        }
        if !any_ringing {
            self.stop_ringing();
        }
        if !others_active {
            self.audio.request_stop_call();
            if live.sip.is_none() && live.info.connected_at > 0 {
                self.call_at(false);
            }
        }
        let info = &live.info;
        let dir = if info.direction == 0 { Direction::Incoming } else { Direction::Outgoing };
        let connected = info.connected_at > 0;
        let outcome = forced.unwrap_or(match (dir, connected, live.declined) {
            (_, true, _) => Outcome::Answered,
            (Direction::Incoming, false, true) => Outcome::Declined,
            (Direction::Incoming, false, false) if live.silenced => Outcome::Silenced,
            (Direction::Incoming, false, false) => Outcome::Missed,
            (Direction::Outgoing, false, _) => Outcome::Cancelled,
        });
        let duration = live.connected.map(|t| t.elapsed().as_secs() as i64).unwrap_or(0);
        self.with_store(|s| s.add_call(&info.number, dir, live.started, duration, outcome));
        tracing::info!(id, ?outcome, duration, "call ended");
        api::emit_call_removed(&self.session, id).await;
        api::emit_missed_changed(self).await;
        self.notify.withdraw_call(id).await;
        if outcome == Outcome::Missed {
            let name = self.name_for(&info.number);
            self.notify.missed_call(&name, &info.number).await;
            self.kdeconnect.missed(&info.number, &name);
        } else if dir == Direction::Incoming && !connected {
            self.kdeconnect.stopped_ringing(&info.number);
        }
        if live.sip.is_some() {
            return;
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
        let mm_state = call.state().await.unwrap_or(mcs::TERMINATED);
        if direction != 1 {
            // Outgoing. Usually dial() is tracking it already; if phoned restarted
            // mid-call, adopt it so it stays on screen with its audio.
            let tracked = self.state.lock().await.calls.values().any(|c| c.mm_path == path);
            if !tracked && mm_state != mcs::TERMINATED && call_state_of(mm_state).is_some() {
                let num = number::canonical(&call.number().await.unwrap_or_default(), &self.region());
                tracing::info!("adopting an outgoing call phoned was not watching");
                // As dialling: its watcher then applies the real state, which starts
                // the audio and the clock as for any call that connects.
                self.track(&path, &num, Direction::Outgoing, CallState::Dialing, false, None).await;
            }
            return;
        }
        if self.state.lock().await.finished.contains_key(&path) {
            return; // Ours, just ended and logged.
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
        if matches!(mm_state, mcs::ACTIVE | mcs::HELD) {
            // Already answered before phoned saw it (a restart mid-call): adopt it
            // as dialling, so its watcher's first state starts audio and the clock.
            tracing::info!("adopting a live incoming call phoned was not watching");
            self.track(&path, &num, Direction::Incoming, CallState::Dialing, false, None).await;
            return;
        }
        // A call during another is waiting, whichever network the other is on (the
        // modem cannot see a SIP call, so it says ringing).
        let busy = self.state.lock().await.calls.values().any(|c| CallState::from_u8(c.info.state).live());
        let state = if mm_state == mcs::WAITING || busy { CallState::Waiting } else { CallState::RingingIn };
        self.ring(&path, &num, state, None).await;
    }

    /// Ring for a new incoming call, whichever network it came on.
    async fn ring(self: &Arc<Self>, path: &OwnedObjectPath, num: &str, state: CallState, sip: Option<String>) {
        let num = num.to_string();
        let known = !num.is_empty() && self.contact_for(&num);
        let silence = self.flag(phone_api::keys::SILENCE_UNKNOWN) && !known;
        let id = self.track(path, &num, Direction::Incoming, state, false, sip).await;
        if let Some(live) = self.state.lock().await.calls.get_mut(&id) {
            live.ringing = !silence;
            live.silenced = silence;
        }
        tracing::info!(id, from = %number::mask(&num), waiting = state == CallState::Waiting, silence, "incoming call");
        if silence {
            // Not in contacts, and the owner silences unknown callers: no ring, no
            // screen. It is in the log afterwards to call back.
            return;
        }
        let ringer = self.setting(("ringer", "ring"));
        // Ring first, then pause the music: a slow player must not delay the ring.
        // The caller may have hung up already (finish ran while this was tracking);
        // then there is nothing to ring for.
        if !self.state.lock().await.calls.contains_key(&id) {
            return;
        }
        // A waiting call beeps in the ear rather than ringing the room. The Dialer
        // vibrates the motor; it reads the same settings.
        self.audio.start_ringing(&ringer, state == CallState::Waiting);
        self.media.pause_all(&self.session).await;
        if !self.state.lock().await.calls.contains_key(&id) {
            // Ended while the music paused: finish has already stopped the ringer
            // and resumed the music, or will.
            self.stop_ringing_if_idle().await;
            return;
        }
        if self.flag(phone_api::keys::FLASH_ON_RING) {
            crate::leds::flash(&self.ring_leds());
        }
        launch::dialer();
        let name = self.name_for(&num);
        if self.flag(phone_api::keys::SPEAK_CALLER) && ringer != "silent" {
            // A number with no name is read out digit by digit: as one figure it is
            // "sixty-one billion...".
            let who = if num.is_empty() {
                "a withheld number".to_string()
            } else if name.chars().all(|c| c.is_ascii_digit() || c == ' ' || c == '+') {
                name.chars().filter(char::is_ascii_digit).map(String::from).collect::<Vec<_>>().join(" ")
            } else {
                name.clone()
            };
            self.audio.speak(&format!("Call from {who}"));
        }
        self.notify.incoming_call(id, &name, &num).await;
        self.kdeconnect.ringing(&num, &name);
    }

    /// Merge the active call and the held one into a conference.
    pub async fn merge(self: &Arc<Self>) -> zbus::fdo::Result<()> {
        let held = {
            let st = self.state.lock().await;
            if st.calls.values().any(|c| c.sip.is_some()) {
                return Err(zbus::fdo::Error::NotSupported("conference calls over SIP are not supported".into()));
            }
            let held = st.calls.values().find(|c| CallState::from_u8(c.info.state) == CallState::Held);
            let active = st.calls.values().any(|c| CallState::from_u8(c.info.state) == CallState::Active);
            match (held, active) {
                (Some(h), true) => h.mm_path.clone(),
                _ => return Err(zbus::fdo::Error::Failed("merging needs one call active and one on hold".into())),
            }
        };
        let call = mm::CallProxy::builder(&self.system).path(held).build().await?;
        call.join_multiparty().await?;
        Ok(())
    }

    pub async fn answer(self: &Arc<Self>, id: u32) -> zbus::fdo::Result<()> {
        let (path, has_active, sip, sip_active) = {
            let st = self.state.lock().await;
            let live = st.calls.get(&id).ok_or_else(no_such_call)?;
            let others: Vec<&Live> = st
                .calls
                .values()
                .filter(|c| c.info.id != id && CallState::from_u8(c.info.state) == CallState::Active)
                .collect();
            let sip_active = others.iter().find_map(|c| c.sip.clone());
            (live.mm_path.clone(), !others.is_empty(), live.sip.clone(), sip_active)
        };
        self.stop_ringing();
        if let Some(sid) = sip {
            if has_active {
                // A SIP call waiting during a mobile call: hold the mobile call
                // (HoldAndAccept with nothing waiting on the modem holds it).
                let modem = self.modem().await?;
                let voice = mm::VoiceProxy::builder(&self.system).path(modem.path).build().await?;
                voice.hold_and_accept().await?;
                self.audio_handover().await;
            }
            self.sip.on_call(&sid, "accept").await?;
            return Ok(());
        }
        if let Some(sid) = sip_active {
            // A mobile call waiting during a SIP call: hold the SIP call, then answer.
            self.sip_hold(&sid, true).await?;
            let call = mm::CallProxy::builder(&self.system).path(path).build().await?;
            call.accept().await?;
            self.audio_handover().await;
            return Ok(());
        }
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
        let (path, sip) = {
            let mut st = self.state.lock().await;
            let live = st.calls.get_mut(&id).ok_or_else(no_such_call)?;
            if CallState::from_u8(live.info.state).ringing_in() {
                live.declined = true;
            }
            (live.mm_path.clone(), live.sip.clone())
        };
        if let Some(sid) = sip {
            self.sip.on_call(&sid, "hangup").await?;
            return Ok(());
        }
        let call = mm::CallProxy::builder(&self.system).path(path).build().await?;
        call.hangup().await?;
        Ok(())
    }

    pub async fn swap(self: &Arc<Self>) -> zbus::fdo::Result<()> {
        let (sip_call, mobile) = {
            let st = self.state.lock().await;
            let sip = st.calls.values().find_map(|c| c.sip.clone().map(|s| (s, CallState::from_u8(c.info.state))));
            let mobile = st.calls.values().any(|c| c.sip.is_none() && CallState::from_u8(c.info.state).live());
            (sip, mobile)
        };
        let voice = || async {
            let modem = self.modem().await?;
            Ok::<_, zbus::fdo::Error>(mm::VoiceProxy::builder(&self.system).path(modem.path).build().await?)
        };
        match sip_call {
            // Two mobile calls, or one held: the modem swaps them.
            None => {
                voice().await?.hold_and_accept().await?;
            }
            // SIP on, mobile held: hold SIP, bring the mobile call back.
            Some((sid, CallState::Active)) => {
                self.sip_hold(&sid, true).await?;
                if mobile {
                    voice().await?.hold_and_accept().await?;
                }
            }
            // SIP held: hold the mobile call (if any), bring SIP back.
            Some((sid, CallState::Held)) => {
                if mobile {
                    voice().await?.hold_and_accept().await?;
                }
                self.sip_hold(&sid, false).await?;
            }
            Some(_) => return Err(zbus::fdo::Error::Failed("nothing to swap yet".into())),
        }
        self.audio_handover().await;
        Ok(())
    }

    /// Hold or resume the SIP call. baresip does it; phoned keeps the state, which
    /// baresip's events do not report.
    async fn sip_hold(self: &Arc<Self>, sid: &str, hold: bool) -> zbus::fdo::Result<()> {
        self.sip.on_call(sid, if hold { "hold" } else { "resume" }).await?;
        let id = self.state.lock().await.calls.iter().find(|(_, c)| c.sip.as_deref() == Some(sid)).map(|(i, _)| *i);
        if let Some(id) = id {
            let state = if hold { CallState::Held } else { CallState::Active };
            self.set_state(id, state, None, String::new()).await;
        }
        Ok(())
    }

    /// After a swap between networks, move the audio to the call that is now live.
    async fn audio_handover(self: &Arc<Self>) {
        // The modem's state changes arrive as signals; give them a moment.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let port = self
            .state
            .lock()
            .await
            .calls
            .values()
            .find(|c| CallState::from_u8(c.info.state) == CallState::Active)
            .map(|c| if c.sip.is_some() { SIP_PORT.to_string() } else { c.port.clone() });
        self.audio.request_stop_call();
        if let Some(p) = port {
            self.audio.request_start_call(&p, 0);
        }
    }

    pub async fn send_dtmf(&self, id: u32, tones: &str) -> zbus::fdo::Result<()> {
        if tones.is_empty() || !tones.chars().all(|c| c.is_ascii_digit() || "*#ABCD".contains(c)) {
            return Err(zbus::fdo::Error::InvalidArgs("DTMF is 0-9, *, #, A-D".into()));
        }
        let (path, sip) = {
            let st = self.state.lock().await;
            let live = st.calls.get(&id).ok_or_else(no_such_call)?;
            (live.mm_path.clone(), live.sip.clone())
        };
        if let Some(sid) = sip {
            for t in tones.chars() {
                self.sip.on_call(&sid, &format!("sndcode {t}")).await?;
            }
            return Ok(());
        }
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
        if to.trim().starts_with("sip:") || to.trim().starts_with("sips:") {
            return Err(zbus::fdo::Error::NotSupported(
                "texts to SIP addresses are not supported: baresip 1.1.0 cannot message a given address".into(),
            ));
        }
        if to.contains(',') {
            // More than one person: a group text, which only MMS carries.
            let to: Vec<String> = to.split(',').map(String::from).collect();
            return self.send_mms(&to, body, &[], caller, confirmed).await;
        }
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
                // A standing grant the owner approved lets this program text this
                // number without asking again, up to the grant's own hourly limit.
                // Never for premium numbers or short codes, whatever the grant says.
                let grant = if reason == "external" {
                    self.with_store(|s| s.grant_for(&caller.name, &peer, phone_core::now())).flatten()
                } else {
                    None
                };
                match grant {
                    Some((gid, per_hour)) => {
                        let mut st = self.state.lock().await;
                        let limit =
                            st.grant_limits.entry(gid).or_insert_with(|| RateLimit::new(per_hour as usize, 3600));
                        if let Err(wait) = limit.take(mono()) {
                            drop(st);
                            self.audit(caller, "send", &peer, "over its grant's limit");
                            return Err(zbus::fdo::Error::LimitsExceeded(format!(
                                "this program's grant allows {per_hour} an hour; try again in {wait}s"
                            )));
                        }
                        drop(st);
                        self.audit(caller, "send", &peer, "sent under a grant");
                    }
                    None => {
                        let id = self.park("sms", &peer, &body, caller, reason).await;
                        self.audit(caller, "send", &peer, "waiting for confirmation");
                        return Ok(Placed { id: 0, pending: id });
                    }
                }
            }
        }
        {
            let mut st = self.state.lock().await;
            if let Err(wait) = st.sms_limit.take(mono()) {
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
            self.kdeconnect.message(&m);
        }
        self.spawn_transmit(modem, id, peer, body);
        Ok(Placed { id, pending: 0 })
    }

    /// Send a stored outgoing message in the background, and follow it to Sent,
    /// Failed, and (with delivery reports on) Delivered.
    fn spawn_transmit(self: &Arc<Self>, modem: mm::Found, id: i64, peer: String, body: String) {
        let me = self.clone();
        let report = self.flag(phone_api::keys::DELIVERY_REPORTS);
        tokio::spawn(async move {
            let (state, kept) = match me.transmit(&modem, &peer, &body, report).await {
                Ok(kept) => (MessageState::Sent, kept),
                Err(e) => {
                    tracing::error!(to = %number::mask(&peer), "sending failed: {e}");
                    (MessageState::Failed, None)
                }
            };
            me.with_store(|s| s.set_message_state(id, state));
            api::emit_message_changed(&me.session, id, state as u8).await;
            if let Some(path) = kept {
                me.follow_delivery(&modem, id, path).await;
            }
        });
    }

    /// Create and send the text. With a delivery report asked for, the modem's copy
    /// is kept (the report updates it) and returned; otherwise it is deleted at once.
    async fn transmit(
        &self,
        modem: &mm::Found,
        peer: &str,
        body: &str,
        report: bool,
    ) -> zbus::Result<Option<OwnedObjectPath>> {
        let messaging = mm::MessagingProxy::builder(&self.system).path(modem.path.clone()).build().await?;
        let mut props = HashMap::from([("number", Variant::from(peer)), ("text", Variant::from(body))]);
        if report {
            props.insert("delivery-report-request", Variant::from(true));
        }
        let path = messaging.create(props).await?;
        let sms = mm::SmsProxy::builder(&self.system).path(path.clone()).build().await?;
        let result = sms.send().await;
        if result.is_err() || !report {
            // Sent or not, the modem's copy is not needed: ours is in the store.
            let _ = messaging.delete(&ObjectPath::from(&path)).await;
            return result.map(|_| None);
        }
        Ok(Some(path))
    }

    /// Wait for the network's delivery report, for up to two days, then let the
    /// modem forget the message.
    async fn follow_delivery(self: &Arc<Self>, modem: &mm::Found, id: i64, path: OwnedObjectPath) {
        let Ok(sms) = mm::SmsProxy::builder(&self.system).path(path.clone()).build().await else { return };
        let mut changes = sms.receive_delivery_state_changed().await;
        let wait = async {
            if sms.delivery_state().await.is_ok_and(|d| d <= 2) {
                return true;
            }
            while let Some(c) = changes.next().await {
                match c.get().await {
                    // 0x00 delivered, 0x01 forwarded (not confirmed) and 0x02 replaced
                    // by the service centre all mean it arrived (TS 23.040 §9.2.3.15).
                    Ok(0..=2) => return true,
                    // 0x20-0x2F: a temporary error, and the centre is still trying:
                    // keep waiting for its next report. 0x100 is "not known yet".
                    Ok(0x20..=0x2F) | Ok(0x100..) => {}
                    // Anything else is final, and not delivered.
                    Ok(_) => return false,
                    _ => {}
                }
            }
            false
        };
        let delivered = tokio::time::timeout(Duration::from_secs(48 * 3600), wait).await.unwrap_or(false);
        if delivered {
            self.with_store(|s| s.set_message_state(id, MessageState::Delivered));
            api::emit_message_changed(&self.session, id, MessageState::Delivered as u8).await;
        }
        if let Ok(m) = mm::MessagingProxy::builder(&self.system).path(modem.path.clone()).build().await {
            let _ = m.delete(&ObjectPath::from(&path)).await;
        }
    }

    /// Send a failed text again.
    pub async fn retry(self: &Arc<Self>, id: i64) -> zbus::fdo::Result<()> {
        let m = self
            .with_store(|s| s.message(id))
            .flatten()
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs("no such message".into()))?;
        if m.direction != Direction::Outgoing || m.state != MessageState::Failed {
            return Err(zbus::fdo::Error::InvalidArgs("only a failed text can be sent again".into()));
        }
        if let Err(wait) = self.state.lock().await.sms_limit.take(mono()) {
            return Err(zbus::fdo::Error::LimitsExceeded(format!("too many texts; try again in {wait}s")));
        }
        let modem = self.modem().await?;
        self.with_store(|s| s.set_message_state(id, MessageState::Sending));
        api::emit_message_changed(&self.session, id, MessageState::Sending as u8).await;
        self.spawn_transmit(modem, id, m.peer, m.body);
        Ok(())
    }

    /// Record what an outside program asked for. Dolphone's own apps are the owner
    /// acting, and are not logged.
    pub fn audit(&self, caller: &Caller, action: &str, target: &str, result: &str) {
        if caller.trusted {
            return;
        }
        let shown = number::mask(target);
        self.with_store(|s| s.audit(&caller.name, action, &shown, result, phone_core::now()));
    }

    /// Whether a number belongs to a contact.
    pub fn contact_for(&self, num: &str) -> bool {
        self.book().lookup(num).is_some()
    }

    /// The contacts, from the cache unless the directory or the region changed.
    pub fn book(&self) -> Arc<phone_core::contacts::Book> {
        let region = self.region();
        let dir = phone_core::store::contacts_dir();
        let mtime = std::fs::metadata(&dir).and_then(|m| m.modified()).ok();
        let mut cache = self.book.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(c) = cache.as_ref().filter(|c| c.region == region && c.mtime == mtime && mtime.is_some()) {
            return c.book.clone();
        }
        let book = Arc::new(phone_core::contacts::Book::load(&dir, &region));
        *cache = Some(BookCache { region, mtime, book: book.clone() });
        book
    }

    /// A text the modem has, from the network. Waits for a multipart message to
    /// finish arriving before reading it.
    pub async fn received(self: &Arc<Self>, path: OwnedObjectPath) {
        if !self.state.lock().await.texts_in_flight.insert(path.clone()) {
            return; // Already being read: one copy, one notification, one reply.
        }
        self.received_once(path.clone()).await;
        self.state.lock().await.texts_in_flight.remove(&path);
    }

    async fn received_once(self: &Arc<Self>, path: OwnedObjectPath) {
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
            // arrives as a binary push with no text. mmsd-tng takes those from
            // ModemManager itself and fetches the message (mms.rs); without it, say
            // so rather than show nothing.
            if crate::mms::available(&self.session).await {
                return;
            }
            body = "[Picture or group message: install mmsd-tng to receive MMS]".into();
        }
        // With the store locked, nothing here outlives phoned: the SIM keeps it.
        let delete = self.flag(phone_api::keys::DELETE_FROM_SIM) && !crate::storekey::locked();
        let private = self.flag(phone_api::keys::PRIVATE_NOTIFY);

        // Australia's Emergency Alert texts come from 0444 444 444, the number the
        // national system sends from (ACT ESA, Queensland's disaster centre: "do
        // not block it"). Until cell broadcast takes over (AusAlert, from 2027) they
        // are the country's warnings: never blocked, and sounded through silent.
        let emergency_alert = from == "+61444444444";
        if emergency_alert {
            tracing::warn!("an Emergency Alert text");
            self.audio.play(Tone::Alert);
            launch::dialer();
            self.notify.alert("Emergency Alert", &body).await;
        }
        let handled = if !emergency_alert && self.with_store(|s| s.is_blocked(&from)).unwrap_or(false) {
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
                        self.kdeconnect.message(&m);
                    }
                    api::emit_unread_changed(self).await;
                    tracing::info!(from = %number::mask(&from), "text received");
                    // An Emergency Alert text has had its alert already.
                    if !emergency_alert {
                        self.notify.message(&self.name_for(&from), &from, &body, private).await;
                        if self.flag(phone_api::keys::TEXT_TONE) && self.setting(("ringer", "ring")) == "ring" {
                            self.audio.play(Tone::Text);
                        }
                    }
                    self.run_rules(&from, &body, at);
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

    async fn park(self: &Arc<Self>, kind: &str, target: &str, body: &str, caller: &Caller, reason: &str) -> u32 {
        let request;
        {
            let mut st = self.state.lock().await;
            // The monotonic clock: a wall clock set back must not keep a request
            // alive for longer than its two minutes.
            let now = mono();
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
            st.requests.insert(id, Pending { request: request.clone(), created: now, mms: None });
        }
        tracing::info!(id = request.id, kind, by = %caller.name, reason, "waiting for confirmation");
        api::emit_requested(&self.session, request.clone()).await;
        // Show it where the owner will see it: the panel (the Dialer draws requests)
        // and the desktop, when there is one.
        launch::dialer();
        self.notify.confirmation(&request, &self.name_for(target)).await;
        // When it expires unanswered, say so: a script waiting on it learns it
        // was not allowed, and the screens stop showing it.
        let me = self.clone();
        let id = request.id;
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(REQUEST_TTL as u64 + 1)).await;
            let gone = me.state.lock().await.requests.remove(&id);
            if let Some(p) = gone {
                let who = Caller { name: p.request.requester.clone(), trusted: false, reader: false };
                me.audit(&who, &p.request.kind, &p.request.target, "expired");
                api::emit_resolved(&me.session, id, false).await;
                me.notify.withdraw_request(id).await;
            }
        });
        request.id
    }

    pub async fn pending(&self) -> Vec<Request> {
        let mut st = self.state.lock().await;
        let now = mono();
        st.requests.retain(|_, p| now - p.created < REQUEST_TTL);
        st.requests.values().map(|p| p.request.clone()).collect()
    }

    pub async fn confirm(self: &Arc<Self>, id: u32, confirmer: &Caller) -> zbus::fdo::Result<Placed> {
        if !confirmer.trusted {
            return Err(zbus::fdo::Error::AccessDenied("only Dolphone's own apps can confirm".into()));
        }
        let pending = {
            let mut st = self.state.lock().await;
            st.requests.remove(&id).filter(|p| mono() - p.created < REQUEST_TTL)
        }
        .ok_or_else(|| zbus::fdo::Error::InvalidArgs("no such request (it may have expired)".into()))?;
        self.notify.withdraw_request(id).await;
        let as_requester = Caller { name: pending.request.requester.clone(), trusted: false, reader: false };
        // Resolved goes out once the action has run, saying whether it worked, so a
        // script waiting on its request learns the outcome, not just the press.
        let r = match pending.request.kind.as_str() {
            "call" => self.dial(&pending.request.target, &as_requester, true).await,
            "sms" => match &pending.mms {
                Some((body, files)) => {
                    let to: Vec<String> = pending.request.target.split(',').map(String::from).collect();
                    self.send_mms(&to, body, files, &as_requester, true).await
                }
                None => self.send(&pending.request.target, &pending.request.body, &as_requester, true).await,
            },
            "contact" => {
                let op: crate::contacts::Op = serde_json::from_str(&pending.request.body)
                    .map_err(|_| zbus::fdo::Error::Failed("the waiting change could not be read".into()))?;
                let r = crate::contacts::apply(self, op).await;
                self.audit(
                    &as_requester,
                    "contacts",
                    &pending.request.target,
                    if r.is_ok() { "allowed" } else { "failed" },
                );
                r
            }
            "kdeconnect" => {
                self.kdeconnect.answer_pair(&pending.request.target, true);
                Ok(Placed { id: 1, pending: 0 })
            }
            "grant" => {
                let (rate, days) = pending.request.body.split_once("/h|").unwrap_or(("1", "1"));
                let per_hour: u32 = rate.parse().unwrap_or(1);
                let days: i64 = days.parse().unwrap_or(1);
                let until = if days == 0 { 0 } else { phone_core::now() + days * 86400 };
                let gid = self
                    .with_store(|s| s.add_grant(&as_requester.name, &pending.request.target, per_hour, until))
                    .ok_or_else(|| zbus::fdo::Error::Failed("could not store the grant".into()))?;
                self.audit(&as_requester, "grant", &pending.request.target, "granted");
                Ok(Placed { id: gid, pending: 0 })
            }
            "setting" => {
                let r = self.apply_setting(&pending.request.target, &pending.request.body).await;
                self.audit(
                    &as_requester,
                    "setting",
                    &pending.request.target,
                    if r.is_ok() { "allowed" } else { "failed" },
                );
                r.map(|_| Placed { id: 1, pending: 0 })
            }
            "modem" => {
                let me = self.clone();
                tokio::spawn(async move {
                    for (name, state, detail) in me.modem_setup(true).await {
                        tracing::info!(name, state, detail, "modem setup");
                    }
                });
                self.audit(&as_requester, "modem setup", "", "allowed");
                Ok(Placed { id: 1, pending: 0 })
            }
            "wipe" => {
                self.with_store(|s| s.wipe());
                let _ = std::fs::remove_dir_all(parts_dir());
                // Contacts are phone data too; and the settings just deleted from
                // the store stop counting now, not at the next start.
                let _ = std::fs::remove_dir_all(phone_core::store::contacts_dir());
                crate::caller::set_grants("");
                crate::caller::set_trust_appimages(true);
                api::emit_unread_changed(self).await;
                api::emit_missed_changed(self).await;
                Ok(Placed { id: 1, pending: 0 })
            }
            _ => Err(zbus::fdo::Error::Failed("unknown request kind".into())),
        };
        api::emit_resolved(&self.session, id, r.is_ok()).await;
        r
    }

    pub async fn deny(&self, id: u32) -> zbus::fdo::Result<()> {
        let removed = self.state.lock().await.requests.remove(&id);
        if let Some(p) = removed.as_ref().filter(|p| p.request.kind == "kdeconnect") {
            self.kdeconnect.answer_pair(&p.request.target, false);
        }
        if let Some(p) = &removed {
            let who = Caller { name: p.request.requester.clone(), trusted: false, reader: false };
            self.audit(&who, &p.request.kind, &p.request.target, "denied");
        }
        if removed.is_none() {
            return Err(zbus::fdo::Error::InvalidArgs("no such request (it may have expired)".into()));
        }
        api::emit_resolved(&self.session, id, false).await;
        self.notify.withdraw_request(id).await;
        Ok(())
    }

    /// A change to the address book from outside, held for a press. `what` is
    /// what the owner reads ("add Alex Example"); `op` the change, as JSON.
    pub async fn park_contact(self: &Arc<Self>, what: &str, op: &str, caller: &Caller) -> Placed {
        Placed { id: 0, pending: self.park("contact", what, op, caller, "contact").await }
    }

    /// A laptop asks to pair over KDE Connect: the owner decides, on the device.
    pub async fn park_kdeconnect_pair(self: &Arc<Self>, device: &str, name: &str) {
        let who = Caller { name: format!("KDE Connect: {name}"), trusted: false, reader: false };
        self.park("kdeconnect", device, name, &who, "pair").await;
    }

    pub async fn request_wipe(self: &Arc<Self>, caller: &Caller) -> Placed {
        Placed { id: 0, pending: self.park("wipe", "", "", caller, "wipe").await }
    }

    /// The modem went away (reset, unplugged, ModemManager restarted): every call it
    /// had is over, whatever it last said, and its audio with it.
    pub async fn lost_modem(self: &Arc<Self>) {
        let ids: Vec<u32> =
            self.state.lock().await.calls.iter().filter(|(_, c)| c.sip.is_none()).map(|(id, _)| *id).collect();
        for id in ids {
            self.finish(id, None).await;
        }
        self.stop_ringing();
    }

    // Modem

    pub async fn refresh_status(self: &Arc<Self>) {
        // One at a time: an older look finishing after a newer one would put back
        // what the newer one saw changed.
        let _one = self.refreshing.lock().await;
        let found = match mm::lookup(&self.system).await {
            Ok(f) => f,
            Err(e) => {
                // Not an answer about the modem: keep what is known, and look again.
                tracing::debug!("could not ask ModemManager for its modem: {e}");
                return;
            }
        };
        let mut status = match &found {
            None => no_modem(),
            Some(f) => read_status(&self.system, f).await,
        };
        status.sip = self.sip.status();
        if let Some(f) = &found {
            if let Some((net, iccid)) = sim_network(&self.system, f).await {
                if let Ok(mut n) = self.sim_network.lock() {
                    *n = net;
                }
                // Records note which SIM they went through.
                if let Ok(s) = self.store.lock() {
                    s.set_sim(&iccid);
                }
            }
        }
        let previous = self.state.lock().await.status.clone();
        let (changed, appeared) = {
            let mut st = self.state.lock().await;
            // A modem that can now do the job (just plugged in, unlocked or enabled)
            // gets the owner's network settings.
            let could = st.modem.as_ref().is_some_and(|m| m.voice || m.messaging);
            let can = found.as_ref().is_some_and(|m| m.voice || m.messaging);
            // Also when alerts or texts become possible (after the SIM PIN: Voice
            // can be there while the SIM is locked, for emergency calls), and for a
            // ModemManager that restarted, which forgot what it was told.
            let gained = match (st.modem.as_ref(), found.as_ref()) {
                (Some(old), Some(new)) => {
                    (new.broadcast && !old.broadcast) || (new.messaging && !old.messaging) || new.owner != old.owner
                }
                _ => false,
            };
            let appeared = ((can && !could) || gained).then(|| found.clone()).flatten();
            st.modem = found;
            let changed = st.status != status;
            st.status = status.clone();
            (changed, appeared)
        };
        if changed {
            self.network_changed(&previous, &status).await;
            api::emit_status_changed(&self.session, status).await;
        }
        if let Some(m) = appeared {
            let me = self.clone();
            tokio::spawn(async move { me.apply_modem_settings(&m).await });
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
            // misread by the network, so they go to the modem as what they are.
            return self.supplementary(code).await;
        }
        let modem = self.modem().await?;
        if !modem.ussd {
            return Err(zbus::fdo::Error::NotSupported("this modem has no USSD support".into()));
        }
        let u = mm::UssdProxy::builder(&self.system).path(modem.path).build().await?;
        Ok(text::clean(&u.initiate(code).await?, text::MAX_BODY))
    }

    /// A supplementary-service code from the dialer: call waiting through
    /// ModemManager, forwarding and caller ID as the 3GPP AT commands.
    async fn supplementary(&self, code: &str) -> zbus::fdo::Result<String> {
        use phone_core::ss::{self, Code, Ss};
        let parsed = ss::parse_code(code)
            .ok_or_else(|| zbus::fdo::Error::NotSupported("Dolphone does not know this service code".into()))?
            .map_err(|why| zbus::fdo::Error::InvalidArgs(why.to_string()))?;
        let modem = self.modem().await?;
        // The SIM's codes are the phone's, not the network's: sent as USSD they
        // would carry the PIN and PUK off the device (TS 22.030 §6.6).
        let sim = || async {
            let m = mm::ModemProxy::builder(&self.system).path(modem.path.clone()).build().await?;
            mm::SimProxy::builder(&self.system).path(m.sim().await?).build().await
        };
        let s = match parsed {
            Code::Service(s) => s,
            Code::Imei => {
                let m = mm::ModemProxy::builder(&self.system).path(modem.path.clone()).build().await?;
                return Ok(format!("IMEI: {}", m.equipment_identifier().await?));
            }
            Code::Pin { which: 1, old, new } => {
                sim().await?.change_pin(&old, &new).await?;
                return Ok("SIM PIN changed".into());
            }
            Code::Puk { which: 1, puk, new } => {
                sim().await?.send_puk(&puk, &new).await?;
                return Ok("SIM unblocked; the new PIN is set".into());
            }
            Code::Pin { .. } | Code::Puk { .. } => {
                return Err(zbus::fdo::Error::NotSupported(
                    "PIN2 is not reachable through ModemManager; change it in another phone".into(),
                ))
            }
        };
        let at = |cmd: String| {
            let path = modem.path.clone();
            async move { mm::at(&self.system, &path, &cmd).await.map_err(|e| zbus::fdo::Error::Failed(mm::explain_at(&e))) }
        };
        match &s {
            Ss::Unsupported(what) => Err(zbus::fdo::Error::NotSupported(format!("{what} codes are not supported"))),
            Ss::Waiting(want) => {
                let v = mm::VoiceProxy::builder(&self.system).path(modem.path.clone()).build().await?;
                match want {
                    Some(on) => {
                        v.call_waiting_setup(*on).await?;
                        // The setting follows, or the next modem start would undo it.
                        let value = if *on { "true" } else { "false" };
                        self.store_read(|st| st.set_setting(phone_api::keys::CALL_WAITING.0, value))?;
                        Ok(ss::describe_change(&s))
                    }
                    None => Ok(format!("Call waiting: {}", if v.call_waiting_query().await? { "on" } else { "off" })),
                }
            }
            Ss::Forward { reason, mode, number, time } => {
                let out = at(ss::forward_command(*reason, *mode, number.as_deref(), *time)).await?;
                Ok(if *mode == ss::mode::QUERY {
                    ss::describe_forwarding(*reason, &out)
                } else {
                    ss::describe_change(&s)
                })
            }
            Ss::CallerId(show) => {
                let out = at(ss::clir_command(*show)).await?;
                if let Some(show) = show {
                    // The modem forgets it at a restart; apply_modem_settings puts
                    // it back, so "hidden" stays hidden.
                    let n = if *show { "2" } else { "1" };
                    self.store_read(|st| st.set_setting(phone_api::keys::CLIR.0, n))?;
                }
                Ok(if show.is_none() { ss::describe_clir(&out) } else { ss::describe_change(&s) })
            }
        }
    }

    /// Caller ID for the next call only (`#31#number`): ModemManager's CreateCall
    /// has no such option, so CLIR is set for the call and the owner's own setting
    /// put back once the network has the call.
    async fn clir_for_one_call(self: &Arc<Self>, modem: &mm::Found, hide: bool) {
        let cmd = phone_core::ss::clir_command(Some(!hide));
        if let Err(e) = mm::at(&self.system, &modem.path, &cmd).await {
            tracing::warn!("caller ID for this call not set: {}", mm::explain_at(&e));
            return;
        }
        let me = self.clone();
        let path = modem.path.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let n = me.setting(phone_api::keys::CLIR);
            let _ = mm::at(&me.system, &path, &format!("AT+CLIR={n}")).await;
        });
    }

    /// The profile's AT commands for a mobile call connecting or ending. A
    /// SIM7600 answers ERROR to `AT+CPCMREG=1` until it has reported the call
    /// begun (ModemManager issue 797), so a refusal is tried again shortly.
    fn call_at(self: &Arc<Self>, start: bool) {
        let cmds = self
            .device
            .lock()
            .map(|d| if start { d.0.call_at_start.clone() } else { d.0.call_at_end.clone() })
            .unwrap_or_default();
        if cmds.is_empty() {
            return;
        }
        let me = self.clone();
        tokio::spawn(async move {
            let Some(modem) = me.state.lock().await.modem.clone() else { return };
            for cmd in cmds {
                let mut tries = 0;
                while let Err(e) = mm::at(&me.system, &modem.path, &cmd).await {
                    tries += 1;
                    if tries == 3 || mm::explain_at(&e).contains("debug mode") {
                        tracing::warn!(cmd, "the modem did not take a call command: {}", mm::explain_at(&e));
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        });
    }

    /// Check the modem's one-time setup, or make it.
    pub async fn modem_setup(&self, apply: bool) -> Vec<crate::quectel::Step> {
        let usb_audio =
            self.device.lock().map(|d| !crate::devices::audio_config(&d.0).modem_match.is_empty()).unwrap_or(true);
        crate::quectel::setup(&self.system, apply, usb_audio).await
    }

    /// An outside program asks to change the modem's setup: the owner decides.
    pub async fn request_modem_setup(self: &Arc<Self>, caller: &Caller) -> Placed {
        Placed { id: 0, pending: self.park("modem", "", "", caller, "modem").await }
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

    /// Run the owner's rules for an incoming text, off the async threads. A rule
    /// that replies texts its sender back, as the owner (the rules file is theirs),
    /// but still within the hourly limit.
    fn run_rules(self: &Arc<Self>, from: &str, body: &str, at: i64) {
        let matching: Vec<rules::Rule> = rules::load().into_iter().filter(|r| r.matches(from, body)).collect();
        for rule in matching {
            let me = self.clone();
            let (from, body) = (from.to_string(), body.to_string());
            tokio::spawn(async move {
                let r = rule.clone();
                let (f, b) = (from.clone(), body.clone());
                let out = tokio::task::spawn_blocking(move || rules::run(&r, &f, &b, at)).await.ok().flatten();
                let Some(reply) = out.filter(|o| !o.is_empty() && rule.may_reply()) else { return };
                let owner = Caller { name: "rules.json".into(), trusted: true, reader: false };
                if let Err(e) = me.send(&from, &reply, &owner, false).await {
                    tracing::warn!("a rule's reply was not sent: {e}");
                }
            });
        }
    }

    /// Unlock the SIM. Returns 0 when unlocked, or the tries left.
    pub async fn unlock(self: &Arc<Self>, pin: &str) -> zbus::fdo::Result<u32> {
        if pin.len() < 4 || pin.len() > 8 || !pin.chars().all(|c| c.is_ascii_digit()) {
            return Err(zbus::fdo::Error::InvalidArgs("a SIM PIN is 4 to 8 digits".into()));
        }
        let path = mm::find(&self.system).await.ok_or_else(|| zbus::fdo::Error::Failed("no modem".into()))?.path;
        let modem = mm::ModemProxy::builder(&self.system).path(path.clone()).build().await?;
        let sim = mm::SimProxy::builder(&self.system).path(modem.sim().await?).build().await?;
        let result = sim.send_pin(pin).await;
        let left = || async {
            mm::ModemProxy::builder(&self.system)
                .path(path.clone())
                .cache_properties(zbus::proxy::CacheProperties::No)
                .build()
                .await
                .ok()?
                .unlock_retries()
                .await
                .ok()?
                .get(&mm::LOCK_SIM_PIN)
                .copied()
        };
        match result {
            Ok(()) => {
                tracing::info!("SIM unlocked");
                // ModemManager enables the modem itself after an unlock; read it
                // again so Voice and Messaging are picked up.
                let me = self.clone();
                tokio::spawn(async move {
                    for _ in 0..10 {
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        me.refresh_status().await;
                        if me.state.lock().await.status.state == "ready" {
                            break;
                        }
                    }
                });
                Ok(0)
            }
            Err(e) => match left().await {
                Some(n) if n > 0 => Ok(n),
                _ => Err(zbus::fdo::Error::Failed(format!("the SIM is blocked and needs its PUK ({e})"))),
            },
        }
    }

    /// Catch up after a sleep or a reconnect: re-read the modem, its calls and its
    /// texts. Anything already tracked or stored is skipped.
    pub async fn resync(self: &Arc<Self>) {
        self.refresh_status().await;
        let Some(modem) = self.state.lock().await.modem.clone() else { return };
        if modem.voice {
            if let Ok(v) = mm::VoiceProxy::builder(&self.system).path(modem.path.clone()).build().await {
                for p in v.list_calls().await.unwrap_or_default() {
                    self.incoming(p).await;
                }
            }
        }
        if modem.messaging {
            if let Ok(m) = mm::MessagingProxy::builder(&self.system).path(modem.path.clone()).build().await {
                for p in m.list().await.unwrap_or_default() {
                    let me = self.clone();
                    tokio::spawn(async move { me.received(p).await });
                }
            }
        }
        if modem.broadcast {
            if let Ok(cb) = mm::CellBroadcastProxy::builder(&self.system).path(modem.path.clone()).build().await {
                for p in cb.list().await.unwrap_or_default() {
                    self.alert_added(p).await;
                }
            }
        }
    }

    /// Put the owner's network settings on a modem that has just appeared: call
    /// waiting, and the emergency-alert channels.
    pub async fn apply_modem_settings(self: &Arc<Self>, modem: &mm::Found) {
        let at_start = self.device.lock().map(|d| d.0.modem_at.clone()).unwrap_or_default();
        if !at_start.is_empty() {
            crate::quectel::on_appeared(&self.system, &modem.path, &at_start).await;
        }
        if let Ok(m) = mm::ModemProxy::builder(&self.system)
            .path(modem.path.clone())
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await
        {
            let no_2g = self.flag(phone_api::keys::DISABLE_2G);
            let supported = m.supported_modes().await.unwrap_or_default();
            if let Some(want) = mm::choose_modes(&supported, no_2g) {
                if m.current_modes().await.ok() != Some(want) {
                    match m.set_current_modes(&want).await {
                        Ok(()) => tracing::info!(no_2g, allowed = want.0, "network modes set"),
                        Err(e) => tracing::warn!("network modes not set: {e}"),
                    }
                }
            } else if no_2g {
                tracing::warn!("this modem offers no way to leave 2G out");
            }
        }
        // Caller ID as the owner set it: only when it is not the network's default,
        // since it needs ModemManager's AT passthrough.
        let clir = self.setting(phone_api::keys::CLIR);
        if clir != "0" {
            if let Err(e) = mm::at(&self.system, &modem.path, &format!("AT+CLIR={clir}")).await {
                tracing::warn!("caller ID setting not applied: {}", mm::explain_at(&e));
            }
        }
        if modem.voice {
            if let Ok(v) = mm::VoiceProxy::builder(&self.system).path(modem.path.clone()).build().await {
                let want = self.flag(phone_api::keys::CALL_WAITING);
                if v.call_waiting_query().await.ok() != Some(want) {
                    match v.call_waiting_setup(want).await {
                        Ok(()) => tracing::info!(on = want, "call waiting set"),
                        Err(e) => tracing::warn!("call waiting not set: {e}"),
                    }
                }
            }
        }
        if modem.broadcast {
            if let Ok(cb) = mm::CellBroadcastProxy::builder(&self.system).path(modem.path.clone()).build().await {
                if let Err(e) = cb.set_channels(mm::ALERT_CHANNELS).await {
                    tracing::warn!("emergency alert channels not set: {e}");
                }
            }
        }
    }

    /// A cell-broadcast message: if it is an emergency alert the owner wants,
    /// store it, sound it whatever the ringer says, and put it on every screen.
    pub async fn alert_added(self: &Arc<Self>, path: OwnedObjectPath) {
        let Ok(cbm) = mm::CbmProxy::builder(&self.system).path(path.clone()).build().await else { return };
        let channel = cbm.channel().await.unwrap_or(0);
        let serial = cbm.message_code().await.unwrap_or(0);
        let body = text::clean(&cbm.text().await.unwrap_or_default(), text::MAX_BODY);
        let forget = || async {
            if let Some(m) = self.state.lock().await.modem.clone() {
                if let Ok(cb) = mm::CellBroadcastProxy::builder(&self.system).path(m.path).build().await {
                    let _ = cb.delete(&ObjectPath::from(&path)).await;
                }
            }
        };
        let Some(level) = AlertLevel::of_channel(channel) else { return forget().await };
        let wanted = match level {
            AlertLevel::National => true,
            AlertLevel::Extreme => self.flag(phone_api::keys::ALERTS_EXTREME),
            AlertLevel::Severe => self.flag(phone_api::keys::ALERTS_SEVERE),
            AlertLevel::Amber => self.flag(phone_api::keys::ALERTS_AMBER),
            AlertLevel::Test => self.flag(phone_api::keys::ALERTS_TEST),
            AlertLevel::Info => true,
        };
        if !wanted || body.trim().is_empty() {
            return forget().await;
        }
        let now = phone_core::now();
        // A rebroadcast of an alert already shown is stored once and shown once. An
        // alert that cannot be stored (a full disk) is still shown and sounded: it
        // matters more than the record of it.
        let id = match self.with_store(|s| s.add_alert(channel, level as u8, &body, now, serial)) {
            Some(Some(id)) => id,
            Some(None) => return forget().await,
            None => {
                tracing::error!(channel, "an emergency alert could not be stored; showing it anyway");
                0
            }
        };
        tracing::warn!(channel, level = level.label(), "emergency alert");
        let info = AlertInfo { id, channel, level: level as u8, text: body.clone(), at: now };
        api::emit_alert_received(&self.session, info).await;
        if level != AlertLevel::Info {
            self.audio.play(Tone::Alert);
            launch::dialer();
        }
        self.notify.alert(level.label(), &body).await;
        forget().await;
    }

    pub fn alerts(&self, limit: u32) -> zbus::fdo::Result<Vec<AlertInfo>> {
        Ok(self
            .store_read(|s| s.alerts(limit.clamp(1, 200)))?
            .into_iter()
            .map(|(id, channel, level, text, at)| AlertInfo { id, channel, level, text, at })
            .collect())
    }

    /// An outside program asks for a standing grant. The owner sees what, for how
    /// long and how often, and confirms on the device. Premium, short and emergency
    /// numbers can never be granted.
    pub async fn request_grant(
        self: &Arc<Self>,
        numbers: &str,
        per_hour: u32,
        days: u32,
        caller: &Caller,
    ) -> zbus::fdo::Result<Placed> {
        let region = self.region();
        let mut list = Vec::new();
        for n in numbers.split(',').map(str::trim).filter(|n| !n.is_empty()) {
            let (stem, prefix) = match n.strip_suffix('*') {
                Some(s) => (s, true),
                None => (n, false),
            };
            let (canon, probe) = if prefix {
                number::canonical_prefix(stem, &region)
                    .ok_or_else(|| zbus::fdo::Error::InvalidArgs(format!("{n}: no number starts that way")))?
            } else {
                let c = number::canonical(stem, &region);
                (c.clone(), c)
            };
            if canon.len() < if prefix { 6 } else { 3 } {
                return Err(zbus::fdo::Error::InvalidArgs(format!("{n}: too short to grant")));
            }
            let class = number::classify(&probe, &region);
            if matches!(class, Class::Premium | Class::Short | Class::Emergency) {
                return Err(zbus::fdo::Error::AccessDenied(format!(
                    "{n}: premium, short and emergency numbers cannot be granted"
                )));
            }
            list.push(if prefix { format!("{canon}*") } else { canon });
        }
        if list.is_empty() || list.len() > 20 {
            return Err(zbus::fdo::Error::InvalidArgs("grant 1 to 20 numbers".into()));
        }
        if !(1..=60).contains(&per_hour) {
            return Err(zbus::fdo::Error::InvalidArgs("per_hour is 1 to 60".into()));
        }
        if days > 366 {
            return Err(zbus::fdo::Error::InvalidArgs("a grant lasts at most a year (or 0 for no end)".into()));
        }
        let body = format!("{per_hour}/h|{days}");
        let target = list.join(",");
        let id = self.park("grant", &target, &body, caller, "grant").await;
        self.audit(caller, "grant", &target, "waiting for confirmation");
        Ok(Placed { id: 0, pending: id })
    }

    pub fn grants(&self) -> zbus::fdo::Result<Vec<phone_api::Grant>> {
        Ok(self
            .store_read(|s| s.grants())?
            .into_iter()
            .map(|(id, program, numbers, per_hour, until)| phone_api::Grant { id, program, numbers, per_hour, until })
            .collect())
    }

    pub async fn revoke_grant(&self, id: i64) -> zbus::fdo::Result<()> {
        self.store_read(|s| s.revoke_grant(id))?;
        self.state.lock().await.grant_limits.remove(&id);
        Ok(())
    }

    pub fn audit_log(&self, limit: u32) -> zbus::fdo::Result<Vec<phone_api::AuditEntry>> {
        Ok(self
            .store_read(|s| s.audit_log(limit.clamp(1, 1000)))?
            .into_iter()
            .map(|(at, program, action, target, result)| phone_api::AuditEntry { at, program, action, target, result })
            .collect())
    }

    /// Stop the ringer and the LEDs.
    pub fn stop_ringing(&self) {
        self.audio.stop_ringing();
        crate::leds::stop(&self.ring_leds());
    }

    /// Stop the ringer and give the music back when no call needs them any more:
    /// for a ring that raced its call's end.
    async fn stop_ringing_if_idle(self: &Arc<Self>) {
        let (ringing, none) = {
            let st = self.state.lock().await;
            (st.calls.values().any(|c| CallState::from_u8(c.info.state).ringing_in()), st.calls.is_empty())
        };
        if !ringing {
            self.stop_ringing();
        }
        if none {
            self.media.resume(&self.session).await;
        }
    }

    /// The LEDs this machine flashes for a ringing call.
    pub fn ring_leds(&self) -> Vec<String> {
        self.device.lock().map(|d| d.0.ring_leds.clone()).unwrap_or_default()
    }

    /// Use the profile the owner picked ("auto" to detect again).
    pub fn choose_device(&self, id: &str) {
        let (profile, why) = crate::devices::choose(&crate::devices::all(), &crate::devices::Facts::read(), id);
        tracing::info!(device = %profile.id, why = %why, "device profile");
        self.audio.set_config(crate::devices::audio_config(&profile));
        if let Ok(mut d) = self.device.lock() {
            *d = (profile, why);
        }
    }

    /// The modem moved to another technology or operator: note it, and warn of a
    /// fall back to 2G, which a fake base station can force.
    async fn network_changed(&self, was: &ModemStatus, now: &ModemStatus) {
        if !now.present || (was.tech == now.tech && was.operator == now.operator && was.registered == now.registered) {
            return;
        }
        self.with_store(|s| s.add_cell(phone_core::now(), &now.tech, &now.operator, now.registered));
        let downgraded = now.tech == "2G" && matches!(was.tech.as_str(), "3G" | "LTE" | "5G");
        if downgraded && self.flag(phone_api::keys::WARN_2G) {
            tracing::warn!(operator = %now.operator, "the modem fell back to 2G");
            self.notify
                .warning(
                    "Now on 2G",
                    "The modem fell back to 2G, whose encryption is weak. Calls and texts may be readable \
                     nearby. You can keep the modem off 2G in Phone Settings > Calls.",
                )
                .await;
        }
    }

    // SIP

    /// Whether this call goes over SIP: a SIP address always does; an emergency
    /// call never does; anything else by the owner's choice, "always" or only when
    /// there is no mobile network to place it on.
    async fn use_sip(&self, raw: &str, class: Class) -> bool {
        let address = raw.trim().starts_with("sip:") || raw.trim().starts_with("sips:");
        if class == Class::Emergency && !address {
            return false;
        }
        if address {
            return true;
        }
        if !self.sip.registered() {
            return false;
        }
        match self.setting(phone_api::keys::SIP_USE).as_str() {
            "always" => true,
            _ => {
                let st = self.state.lock().await;
                let cellular = st.modem.as_ref().is_some_and(|m| m.voice) && st.status.registered;
                !cellular
            }
        }
    }

    async fn dial_sip(self: &Arc<Self>, raw: &str, canonical: &str) -> zbus::fdo::Result<Placed> {
        if !self.sip.registered() {
            return Err(zbus::fdo::Error::Failed(match self.sip.status().as_str() {
                "" => "SIP calling is off (Phone Settings > Calls over the internet)".into(),
                s => format!("SIP is not ready ({s})"),
            }));
        }
        let account = self.setting(phone_api::keys::SIP_ACCOUNT);
        let target = if raw.trim().starts_with("sip") { raw.trim() } else { canonical };
        let uri = sip::uri_for(target, &account);
        if !sip::valid_account(&uri) {
            return Err(zbus::fdo::Error::InvalidArgs("that is not a SIP address".into()));
        }
        let sid = self.sip.dial(&uri).await?;
        let shown = sip::number_of(&uri);
        let shown = if shown.starts_with("sip:") { shown } else { number::canonical(&shown, &self.region()) };
        let id = self
            .track(
                &OwnedObjectPath::from(ObjectPath::from_static_str_unchecked("/")),
                &shown,
                Direction::Outgoing,
                CallState::Dialing,
                false,
                Some(sid.clone()),
            )
            .await;
        // Whatever baresip said about the call before it was tracked.
        let early = self.state.lock().await.sip_early.remove(&sid).map(|(_, v)| v).unwrap_or_default();
        for e in early {
            self.sip_event(e).await;
        }
        self.media.pause_all(&self.session).await;
        tracing::info!(id, "dialling over SIP");
        Ok(Placed { id: i64::from(id), pending: 0 })
    }

    /// One of baresip's events.
    pub async fn sip_event(self: &Arc<Self>, e: sip::CallEvent) {
        if e.id.is_empty() {
            return;
        }
        let id = {
            let mut st = self.state.lock().await;
            let id = st.calls.iter().find(|(_, c)| c.sip.as_deref() == Some(&e.id)).map(|(i, _)| *i);
            if id.is_none()
                && matches!(e.kind.as_str(), "CALL_RINGING" | "CALL_PROGRESS" | "CALL_ESTABLISHED" | "CALL_CLOSED")
            {
                // About a call dial_sip has not tracked yet (baresip answered its
                // dial command after sending this): kept for it to replay.
                st.sip_early.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(30));
                st.sip_early.entry(e.id.clone()).or_insert_with(|| (Instant::now(), Vec::new())).1.push(e);
                return;
            }
            id
        };
        match (e.kind.as_str(), id) {
            ("CALL_INCOMING", None) => self.sip_incoming(e).await,
            ("CALL_RINGING" | "CALL_PROGRESS", Some(id)) => {
                self.set_state(id, CallState::RingingOut, None, SIP_PORT.into()).await;
            }
            ("CALL_ESTABLISHED", Some(id)) => {
                self.set_state(id, CallState::Active, None, SIP_PORT.into()).await;
            }
            ("CALL_CLOSED", Some(id)) => {
                self.set_state(id, CallState::Terminated, sip::refusal(&e.param), String::new()).await;
            }
            _ => {}
        }
    }

    async fn sip_incoming(self: &Arc<Self>, e: sip::CallEvent) {
        let raw = sip::number_from(
            &e.peer,
            &self.setting(phone_api::keys::SIP_ACCOUNT),
            &self.setting(phone_api::keys::SIP_PROXY),
        );
        let num = if raw.starts_with("sip:") {
            text::clean(&raw, text::MAX_SENDER)
        } else {
            number::canonical(&text::clean(&raw, text::MAX_SENDER), &self.region())
        };
        if !num.is_empty() && self.with_store(|s| s.is_blocked(&num)).unwrap_or(false) {
            tracing::info!(from = %number::mask(&num), "blocked SIP caller rejected");
            let _ = self.sip.on_call(&e.id, "hangup").await;
            self.with_store(|s| s.add_call(&num, Direction::Incoming, phone_core::now(), 0, Outcome::Blocked));
            return;
        }
        // baresip carries one SIP call at a time here: a second is turned away
        // busy, and is in the log as missed to call back. During a mobile call it
        // waits, as a second mobile call would.
        let (any, sip_busy) = {
            let st = self.state.lock().await;
            (!st.calls.is_empty(), st.calls.values().any(|c| c.sip.is_some()))
        };
        if sip_busy {
            let _ = self.sip.on_call(&e.id, "hangup").await;
            self.with_store(|s| s.add_call(&num, Direction::Incoming, phone_core::now(), 0, Outcome::Missed));
            api::emit_missed_changed(self).await;
            return;
        }
        tracing::info!(from = %number::mask(&num), waiting = any, "incoming SIP call");
        self.ring(
            &OwnedObjectPath::from(ObjectPath::from_static_str_unchecked("/")),
            &num,
            if any { CallState::Waiting } else { CallState::RingingIn },
            Some(e.id),
        )
        .await;
    }

    /// A text that came as a SIP MESSAGE: kept with the rest, as from the sender's
    /// number (or SIP address), and announced like any text.
    /// Change a setting, checked, and follow it where it acts at once (the modem,
    /// SIP, who may read). For Dolphone's own apps, and for a request the owner
    /// allowed.
    pub async fn apply_setting(self: &Arc<Self>, key: &str, value: &str) -> zbus::fdo::Result<()> {
        // read_grants is a list: "+name" adds one program and "-name" removes one,
        // so asking for a grant cannot drop another program's.
        let edited;
        let value = if key == phone_api::keys::READ_GRANTS.0 && value.starts_with(['+', '-']) {
            let mut list: Vec<String> = self
                .setting(phone_api::keys::READ_GRANTS)
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            let name = value[1..].trim().to_string();
            list.retain(|n| *n != name);
            if value.starts_with('+') && !name.is_empty() {
                list.push(name);
            }
            edited = list.join(",");
            edited.as_str()
        } else {
            value
        };
        if key == phone_api::keys::READ_GRANTS.0 {
            crate::caller::set_grants(value);
        }
        if key == phone_api::keys::TRUST_APPIMAGES.0 {
            crate::caller::set_trust_appimages(value == "true");
        }
        if !phone_api::keys::ALL.iter().any(|(k, _)| *k == key) {
            return Err(zbus::fdo::Error::InvalidArgs(format!("unknown setting {key}")));
        }
        use phone_api::keys;
        if key == keys::SIP_PASSWORD.0 {
            // Kept in a file only the owner can read, not in the settings table
            // that Get reads back.
            crate::sip::set_password(value).map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            self.sip_changed.notify_one();
            return Ok(());
        }
        if key == keys::SIP_ACCOUNT.0 && !value.is_empty() && !crate::sip::valid_account(value) {
            return Err(zbus::fdo::Error::InvalidArgs(
                "a SIP account looks like sip:user@provider.example (optionally ;transport=tls)".into(),
            ));
        }
        if key == keys::SIP_PROXY.0 && !value.is_empty() && !crate::sip::valid_proxy(value) {
            return Err(zbus::fdo::Error::InvalidArgs(
                "a proxy looks like sip:proxy.example (optionally :port and ;transport=tls)".into(),
            ));
        }
        if key == keys::DEVICE_PROFILE.0 && value != "auto" && !crate::devices::all().iter().any(|p| p.id == value) {
            return Err(zbus::fdo::Error::InvalidArgs(format!("no device profile {value}; see phonectl device list")));
        }
        if key == keys::SIP_USE.0 && !matches!(value, "fallback" | "always") {
            return Err(zbus::fdo::Error::InvalidArgs("fallback or always".into()));
        }
        self.store_read(|s| s.set_setting(key, value))?;
        if key == keys::DEVICE_PROFILE.0 {
            self.choose_device(value);
        }
        // The modem follows these at once; a new profile may have start-up commands.
        if key == keys::DISABLE_2G.0 || key == keys::DEVICE_PROFILE.0 {
            if let Some(m) = self.state.lock().await.modem.clone() {
                self.apply_modem_settings(&m).await;
            }
        }
        if key == keys::SIP_ENABLED.0 || key == keys::SIP_ACCOUNT.0 || key == keys::SIP_PROXY.0 {
            self.sip_changed.notify_one();
        }
        if key == phone_api::keys::CALL_WAITING.0 {
            if let Some(m) = self.state.lock().await.modem.clone() {
                self.apply_modem_settings(&m).await;
            }
        }
        Ok(())
    }

    /// A setting another program asked for: unknown keys and the SIP password are
    /// refused, the rest wait for the device.
    pub async fn request_setting(
        self: &Arc<Self>,
        key: &str,
        value: &str,
        caller: &Caller,
    ) -> zbus::fdo::Result<Placed> {
        if !phone_api::keys::ALL.iter().any(|(k, _)| *k == key) {
            return Err(zbus::fdo::Error::InvalidArgs(format!("unknown setting {key}")));
        }
        if caller.trusted {
            self.apply_setting(key, value).await?;
            return Ok(Placed { id: 1, pending: 0 });
        }
        if key == phone_api::keys::SIP_PASSWORD.0 {
            return Err(zbus::fdo::Error::AccessDenied("the SIP password is only set in Phone Settings".into()));
        }
        let value = text::clean(value, 500);
        let id = self.park("setting", key, &value, caller, "setting").await;
        self.audit(caller, "setting", key, "waiting for confirmation");
        Ok(Placed { id: 0, pending: id })
    }

    /// An MMS mmsd-tng has fetched: copy it into the store, with its parts as files
    /// beside it, then delete mmsd-tng's copy.
    pub async fn mms_received(
        self: &Arc<Self>,
        path: OwnedObjectPath,
        props: &HashMap<String, zbus::wire::OwnedVariant>,
    ) {
        let Some(r) = crate::mms::Received::from_props(props) else { return };
        let Some(file) = crate::mms::file_of(path.as_str()) else { return };
        let region = self.region();
        let canon = |n: &str| number::canonical(&text::clean(n, text::MAX_SENDER), &region);
        let sender = canon(&r.sender);
        // This phone's own number, to leave it out of the thread: mmsd-tng's, or the
        // modem's when the SIM does not say. Unknown, a message to one recipient
        // (this phone) is between the sender and us.
        let own =
            if r.own.trim().is_empty() { self.state.lock().await.status.own_number.clone() } else { r.own.clone() };
        let recipients: &[String] = if own.trim().is_empty() && r.recipients.len() <= 1 { &[] } else { &r.recipients };
        let peer = crate::mms::peer_of(&r.sender, recipients, &own, canon);
        let (text_part, parts) = crate::mms::read_parts(&file, &r.attachments);
        let mut body = text::clean(&text_part, text::MAX_BODY);
        if !r.subject.trim().is_empty() && !body.contains(r.subject.trim()) {
            body = format!("{}\n{body}", text::clean(&r.subject, 200)).trim().to_string();
        }
        let at = phone_core::when::parse_iso(&r.date).unwrap_or_else(phone_core::now);
        if peer.is_empty() || self.with_store(|s| s.is_blocked(&sender)).unwrap_or(false) {
            crate::mms::forget(&self.session, &path).await;
            return;
        }
        if self.with_store(|s| s.has_message(&peer, Direction::Incoming, at, &body)).unwrap_or(false) {
            crate::mms::forget(&self.session, &path).await;
            return;
        }
        let Some(id) =
            self.with_store(|s| s.add_message(&peer, Direction::Incoming, &body, at, MessageState::Received))
        else {
            return; // Not stored: mmsd-tng keeps it, and phoned tries again next start.
        };
        // Without a store that lasts, the parts are not written (they would outlive
        // the row that names them); mmsd-tng keeps the message for next time.
        let saved = if crate::storekey::locked() {
            Vec::new()
        } else {
            save_parts(id, parts.iter().map(|(m, b)| (m.as_str(), b.as_slice())))
        };
        self.with_store(|s| s.set_parts(id, &saved));
        // mmsd-tng's copy goes only once every part is safely ours.
        if saved.len() == parts.len() {
            crate::mms::forget(&self.session, &path).await;
        } else {
            tracing::warn!(
                saved = saved.len(),
                of = parts.len(),
                "not all of an MMS's parts were saved; mmsd-tng keeps it"
            );
        }
        tracing::info!(from = %number::mask(&sender), parts = saved.len(), "MMS received");
        let shown = if body.is_empty() {
            saved.first().map(|p| format!("[{}]", p.kind())).unwrap_or_default()
        } else {
            body.clone()
        };
        self.announce_incoming(id, &peer, &sender, &shown, at).await;
    }

    /// Tell everyone about a stored incoming message: front ends, KDE Connect, a
    /// notification, the text tone, the owner's rules.
    async fn announce_incoming(self: &Arc<Self>, id: i64, peer: &str, sender: &str, shown: &str, at: i64) {
        if let Some(m) = self.with_store(|s| s.message(id)).flatten() {
            api::emit_message_added(&self.session, to_info(&m)).await;
            self.kdeconnect.message(&m);
        }
        api::emit_unread_changed(self).await;
        let private = self.flag(phone_api::keys::PRIVATE_NOTIFY);
        self.notify.message(&self.name_for(peer), peer, shown, private).await;
        if self.flag(phone_api::keys::TEXT_TONE) && self.setting(("ringer", "ring")) == "ring" {
            self.audio.play(Tone::Text);
        }
        self.run_rules(sender, shown, at);
    }

    /// Send an MMS: to more than one person, or with files (pictures, audio, a
    /// contact card). Asked about as a text is, and the files are read only once
    /// it is sending.
    pub async fn send_mms(
        self: &Arc<Self>,
        to: &[String],
        body: &str,
        files: &[String],
        caller: &Caller,
        confirmed: bool,
    ) -> zbus::fdo::Result<Placed> {
        let region = self.region();
        let mut members: Vec<String> =
            to.iter().map(|n| number::canonical(n, &region)).filter(|n| !n.is_empty()).collect();
        members.sort();
        members.dedup();
        let body = text::clean(body, text::MAX_BODY);
        if members.is_empty() || members.len() > 20 || (body.trim().is_empty() && files.is_empty()) {
            return Err(zbus::fdo::Error::InvalidArgs("an MMS needs 1 to 20 numbers, and a text or a file".into()));
        }
        if files.len() > 10 {
            return Err(zbus::fdo::Error::InvalidArgs("at most 10 files".into()));
        }
        let classes: Vec<Class> = members.iter().map(|m| number::classify(m, &region)).collect();
        if classes.contains(&Class::Emergency) {
            return Err(zbus::fdo::Error::NotSupported(
                "texting emergency numbers is not supported; call instead".into(),
            ));
        }
        let mut size = 0u64;
        for f in files {
            let meta = std::fs::metadata(f)
                .ok()
                .filter(|m| m.is_file())
                .ok_or_else(|| zbus::fdo::Error::InvalidArgs(format!("not a file: {f}")))?;
            size += meta.len();
        }
        if size > crate::mms::MAX_BYTES {
            return Err(zbus::fdo::Error::InvalidArgs("the files are too large for an MMS".into()));
        }
        let peer = members.join(",");
        if !confirmed {
            // Grants name single numbers: an MMS from a program is always asked about.
            // The costliest member decides what the owner is warned of: a premium
            // number in a group must not hide behind an ordinary one.
            let rank = |r: &str| match r {
                "emergency" => 0,
                "premium" => 1,
                "short" => 2,
                "international" => 3,
                _ => 4,
            };
            let worst = classes.iter().filter_map(|c| needs_confirmation(*c, caller)).min_by_key(|r| rank(r));
            if let Some(reason) = worst {
                let names: Vec<&str> = files.iter().map(|f| f.rsplit('/').next().unwrap_or(f)).collect();
                let shown = if names.is_empty() { body.clone() } else { format!("{body}\n+ {}", names.join(", ")) };
                let id = self.park("sms", &peer, shown.trim(), caller, reason).await;
                if let Some(p) = self.state.lock().await.requests.get_mut(&id) {
                    p.mms = Some((body.clone(), files.to_vec()));
                }
                self.audit(caller, "send", &peer, "waiting for confirmation");
                return Ok(Placed { id: 0, pending: id });
            }
        }
        if let Err(wait) = self.state.lock().await.sms_limit.take(mono()) {
            return Err(zbus::fdo::Error::LimitsExceeded(format!("too many texts; try again in {wait}s")));
        }
        if !crate::mms::available(&self.session).await {
            return Err(zbus::fdo::Error::NotSupported(
                "MMS needs mmsd-tng (sudo apt install mmsd-tng), and mobile data".into(),
            ));
        }
        let now = phone_core::now();
        let id = self
            .with_store(|s| s.add_message(&peer, Direction::Outgoing, &body, now, MessageState::Sending))
            .ok_or_else(|| zbus::fdo::Error::Failed("could not store the message".into()))?;
        // Our own copy of each file, so the thread keeps it whatever happens to the
        // original; mmsd-tng reads these.
        let read: Vec<(String, Vec<u8>)> = files
            .iter()
            .filter_map(|f| std::fs::read(f).ok().map(|b| (crate::mms::mime_of(f).to_string(), b)))
            .collect();
        let saved = save_parts(id, read.iter().map(|(m, b)| (m.as_str(), b.as_slice())));
        self.with_store(|s| s.set_parts(id, &saved));
        if let Some(m) = self.with_store(|s| s.message(id)).flatten() {
            api::emit_message_added(&self.session, to_info(&m)).await;
        }
        let me = self.clone();
        let report = self.flag(phone_api::keys::DELIVERY_REPORTS);
        tokio::spawn(async move {
            let text_file =
                (!body.is_empty()).then(|| write_private(&format!("mms-{id}.txt"), body.as_bytes())).flatten();
            let files: Vec<(String, String)> = saved.iter().map(|p| (p.mime.clone(), p.path.clone())).collect();
            let sent = match crate::mms::send(&me.session, &members, text_file.as_deref(), &files, report).await {
                Ok(path) => crate::mms::follow(&me.session, &path).await,
                Err(e) => {
                    tracing::error!(to = %number::mask(&peer), "MMS not sent: {e}");
                    false
                }
            };
            if let Some(t) = text_file {
                let _ = std::fs::remove_file(t);
            }
            let state = if sent { MessageState::Sent } else { MessageState::Failed };
            me.with_store(|s| s.set_message_state(id, state));
            api::emit_message_changed(&me.session, id, state as u8).await;
        });
        Ok(Placed { id, pending: 0 })
    }

    pub async fn sip_text(self: &Arc<Self>, peer: &str, body: &str) {
        let raw = sip::number_from(
            peer,
            &self.setting(phone_api::keys::SIP_ACCOUNT),
            &self.setting(phone_api::keys::SIP_PROXY),
        );
        if let Err(wait) = self.state.lock().await.sip_text_limit.take(mono()) {
            tracing::warn!(wait, "too many texts over SIP; dropping one");
            return;
        }
        let from = if raw.starts_with("sip:") {
            text::clean(&raw, text::MAX_SENDER)
        } else {
            number::canonical(&text::clean(&raw, text::MAX_SENDER), &self.region())
        };
        let body = text::clean(body, text::MAX_BODY);
        if from.is_empty() || body.trim().is_empty() || self.with_store(|s| s.is_blocked(&from)).unwrap_or(false) {
            return;
        }
        let at = phone_core::now();
        let Some(id) =
            self.with_store(|s| s.add_message(&from, Direction::Incoming, &body, at, MessageState::Received))
        else {
            return;
        };
        if let Some(m) = self.with_store(|s| s.message(id)).flatten() {
            api::emit_message_added(&self.session, to_info(&m)).await;
            self.kdeconnect.message(&m);
        }
        api::emit_unread_changed(self).await;
        tracing::info!(from = %number::mask(&from), "text received over SIP");
        let private = self.flag(phone_api::keys::PRIVATE_NOTIFY);
        self.notify.message(&self.name_for(&from), &from, &body, private).await;
        if self.flag(phone_api::keys::TEXT_TONE) && self.setting(("ringer", "ring")) == "ring" {
            self.audio.play(Tone::Text);
        }
        // No rules: a SIP sender is not authenticated the way a mobile network's
        // is, and a rule's reply goes out as an SMS on the owner's account.
        let _ = at;
    }

    /// baresip went away: its calls are over.
    pub async fn sip_lost(self: &Arc<Self>) {
        let ids: Vec<u32> =
            self.state.lock().await.calls.iter().filter(|(_, c)| c.sip.is_some()).map(|(id, _)| *id).collect();
        for id in ids {
            self.finish(id, None).await;
        }
        *self.sip_muted.lock().await = false;
    }

    /// The audio module's requests for a SIP call.
    pub async fn sip_audio(self: &Arc<Self>, a: SipAudio) {
        let Some(sid) = self.state.lock().await.calls.values().find_map(|c| c.sip.clone()) else { return };
        match a {
            SipAudio::Route(_, _) if sip::test_audio() => {}
            SipAudio::Route(out, mic) => {
                let _ = self.sip.on_call(&sid, &format!("auplay pulse,{out}")).await;
                let _ = self.sip.on_call(&sid, &format!("ausrc pulse,{mic}")).await;
            }
            SipAudio::Mute(m) => {
                // baresip's mute toggles: read, toggle and record under one lock,
                // or two requests at once would both toggle and invert it.
                let mut muted = self.sip_muted.lock().await;
                if *muted != m && self.sip.on_call(&sid, "mute").await.is_ok() {
                    *muted = m;
                }
            }
        }
    }

    /// Read the status again and tell the front ends if it changed.
    pub async fn status_changed(self: &Arc<Self>) {
        self.refresh_status().await;
    }

    // Helpers

    pub fn name_for(&self, num: &str) -> String {
        let region = self.region();
        self.book().name_for(num, &region)
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

/// The SIM's own network (MCC+MNC), which says which country it is from, and its
/// ICCID, which tells one SIM from another.
async fn sim_network(system: &zbus::Connection, f: &mm::Found) -> Option<(String, String)> {
    let m = mm::ModemProxy::builder(system).path(f.path.clone()).build().await.ok()?;
    let sim = m.sim().await.ok().filter(|p| p.as_str() != "/")?;
    let proxy = mm::SimProxy::builder(system).path(sim).build().await.ok()?;
    let id = proxy.operator_identifier().await.ok()?;
    let iccid = proxy.sim_identifier().await.unwrap_or_default();
    (id.len() >= 5 && id.chars().all(|c| c.is_ascii_digit())).then_some((id, iccid))
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
        sip: String::new(),
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

/// Where an MMS's parts are kept: beside the store, private like it.
pub fn parts_dir() -> std::path::PathBuf {
    phone_core::store::data_dir().join("mms")
}

/// Write a message's parts as `<id>-<n>.<ext>` in the parts directory.
fn save_parts<'a>(id: i64, parts: impl Iterator<Item = (&'a str, &'a [u8])>) -> Vec<phone_core::model::Part> {
    let dir = parts_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return Vec::new();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    // A random part in each name: ids start again in a store opened fresh (or in
    // memory), and must never overwrite a picture another store still points to.
    let tag = random_hex(4);
    let mut out = Vec::new();
    for (n, (mime, bytes)) in parts.enumerate() {
        let path = dir.join(format!("{id}-{n}-{tag}.{}", crate::mms::extension(mime)));
        if write_file_private(&path, bytes).is_ok() {
            out.push(phone_core::model::Part { mime: mime.to_string(), path: path.display().to_string() });
        }
    }
    out
}

/// `n` random bytes as hex, from the kernel.
fn random_hex(n: usize) -> String {
    use std::io::Read;
    let mut b = vec![0u8; n];
    let _ = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b));
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A file only the owner can read.
fn write_file_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o.open(path)?.write_all(bytes)
}

/// A file in the runtime directory, for mmsd-tng to read; its path.
fn write_private(name: &str, bytes: &[u8]) -> Option<std::path::PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from)?.join("dolphone");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(name);
    write_file_private(&path, bytes).ok()?;
    Some(path)
}

/// Remove the files of these messages' parts.
pub fn remove_parts(messages: &[phone_core::model::Message]) {
    let dir = parts_dir();
    for p in messages.iter().flat_map(|m| &m.parts) {
        // Only files phoned wrote.
        if std::path::Path::new(&p.path).parent() == Some(dir.as_path()) {
            let _ = std::fs::remove_file(&p.path);
        }
    }
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
        sim: m.sim.clone(),
        parts: m.parts.iter().map(|p| (p.mime.clone(), p.path.clone())).collect(),
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
