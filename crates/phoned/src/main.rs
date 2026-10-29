//! phoned: Dolphone's phone daemon.
//!
//! One process per user session that owns everything a phone does: it is
//! ModemManager's only client for calls and texts, the only thing that routes call
//! audio, and the keeper of the message store and the call log. Front ends (the
//! panel apps, the desktop app, phonectl, scripts) talk to it on the session bus
//! through the interfaces in phone-api.
//!
//! It starts with no modem and keeps looking: a modem in the M.2 slot comes and goes
//! with resets, SIM switches and power states, and phoned follows it.

mod api;
mod audio;
mod bt;
mod caller;
mod contacts;
mod devices;
mod jack;
mod kdeconnect;
mod launch;
mod leds;
mod media;
mod mm;
mod mms;
mod notify;
mod phone;
mod power;
mod quectel;
mod rules;
mod sip;
mod storekey;
mod uplink;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use futures_util::StreamExt;
use phone_api::{PATH, SERVICE};
use tracing_subscriber::EnvFilter;

use crate::phone::Phone;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use std::io::IsTerminal;
    tracing_subscriber::fmt()
        // Plain text for the journal and log files; colour only on a terminal.
        .with_ansi(std::io::stderr().is_terminal())
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "phoned=info".into()))
        .init();

    let store_path = std::env::args()
        .skip_while(|a| a != "--store")
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(phone_core::store::default_path);
    let encrypt = std::env::args().any(|a| a == "--encrypt-store");
    if encrypt || std::env::args().any(|a| a == "--decrypt-store") {
        // The store must not be open while it is rewritten.
        let session = zbus::Connection::session().await.context("connecting to the session bus")?;
        let running = zbus::fdo::DBusProxy::new(&session)
            .await?
            .name_has_owner(SERVICE.try_into().expect("name"))
            .await
            .unwrap_or(false);
        anyhow::ensure!(!running, "stop phoned first: systemctl --user stop phoned");
        return storekey::convert(&store_path, encrypt);
    }
    let opened = if phone_core::store::is_encrypted(&store_path) {
        match storekey::load() {
            Ok(key) => phone_core::store::Store::open_keyed(&store_path, Some(&key)),
            Err(e) => {
                // Never move an encrypted store aside: it is fine, only locked.
                tracing::error!("the store is encrypted and its key could not be unsealed ({e}); running without it");
                storekey::set_locked();
                phone_core::store::Store::in_memory()
            }
        }
    } else {
        phone_core::store::Store::open(&store_path)
    };
    let store = match opened {
        Ok(s) => s,
        Err(e) => {
            // A store that cannot be opened must not keep the phone from ringing.
            // Move it aside, keep it for recovery, and start a fresh one.
            let aside = store_path.with_extension(format!("db.broken-{}", phone_core::now()));
            tracing::error!("store {} unusable ({e}); moved to {}", store_path.display(), aside.display());
            let _ = std::fs::rename(&store_path, &aside);
            phone_core::store::Store::open(&store_path).with_context(|| format!("opening {}", store_path.display()))?
        }
    };
    if let Ok(Some(g)) = store.setting(phone_api::keys::READ_GRANTS.0) {
        caller::set_grants(&g);
    }
    if let Ok(Some(t)) = store.setting(phone_api::keys::TRUST_APPIMAGES.0) {
        caller::set_trust_appimages(t == "true");
    }

    let system = zbus::Connection::system().await.context("connecting to the system bus")?;
    // Connect first and claim the name only once every interface is served: a
    // client that finds the name must find the objects too, or its call is never
    // answered.
    let session = zbus::Connection::session().await.context("connecting to the session bus")?;

    let phone = Phone::new(session.clone(), system.clone(), store);
    let bt = bt::Bt::new(system.clone(), session.clone());
    let server = session.object_server();
    server.at(PATH, api::Calls(phone.clone())).await?;
    server.at(PATH, api::Messages(phone.clone())).await?;
    server.at(PATH, api::Confirm(phone.clone())).await?;
    server.at(PATH, api::Audio(phone.clone())).await?;
    server.at(PATH, api::Bluetooth(bt.clone())).await?;
    server.at(PATH, api::Modem(phone.clone())).await?;
    server.at(PATH, api::Settings(phone.clone())).await?;
    server.at(PATH, contacts::Contacts(phone.clone())).await?;
    session
        .request_name(SERVICE)
        .await
        .context("claiming io.github.isaacrowntree.Dolphone on the session bus (is phoned already running?)")?;
    tracing::info!(store = %store_path.display(), encrypted = phone_core::store::is_encrypted(&store_path), "phoned up");
    if storekey::locked() {
        let p = phone.clone();
        tokio::spawn(async move {
            p.notify
                .warning(
                    "Messages are locked",
                    "The message store's key could not be unsealed. New texts stay on the SIM until it is.",
                )
                .await
        });
    }

    tokio::spawn(follow_modem(phone.clone()));
    tokio::spawn(power::follow_sleep(phone.clone()));
    tokio::spawn(sip::run(phone.clone(), phone.sip_changed.clone()));
    tokio::spawn(uplink::run(phone.clone()));
    tokio::spawn(kdeconnect::run(phone.clone()));
    tokio::spawn(mms::run(phone.clone()));
    {
        // The audio module asks for a SIP call's devices from its own threads.
        let weak = Arc::downgrade(&phone);
        let rt = tokio::runtime::Handle::current();
        phone.audio.set_sip_hook(move |a| {
            if let Some(p) = weak.upgrade() {
                rt.spawn(async move { p.sip_audio(a).await });
            }
        });
    }
    tokio::spawn(follow_audio(phone.clone()));
    tokio::spawn(notify::listen(phone.clone()));
    bt.register_agent().await;
    jack::watch(phone.clone());

    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
    phone.stop_ringing();
    phone.audio.stop_call();
    phone.sip_lost().await;
    phone.sip.stop();
    // Let the kill land before the process (and the child's handle) goes.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    Ok(())
}

/// Wake `poke` whenever ModemManager says anything changed: a modem or one of its
/// interfaces added or removed (a reset, a SIM unlocked), or a property changing
/// (registration, signal, state). Bursts are folded together by the reader.
async fn modem_events(system: zbus::Connection, poke: Arc<tokio::sync::Notify>) {
    fn rule(member: &'static str, iface: &'static str) -> zbus::Result<zbus::MatchRule<'static>> {
        zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(mm::SERVICE)
            .interface(iface)
            .member(member)
            .build()
    }
    let rules = [
        rule("InterfacesAdded", "org.freedesktop.DBus.ObjectManager"),
        rule("InterfacesRemoved", "org.freedesktop.DBus.ObjectManager"),
        rule("PropertiesChanged", "org.freedesktop.DBus.Properties"),
    ];
    let mut streams = Vec::new();
    for r in rules.into_iter().flatten() {
        match zbus::MessageStream::for_match_rule(r, &system, std::num::NonZeroUsize::new(64)).await {
            Ok(s) => streams.push(s),
            Err(e) => tracing::warn!("not following ModemManager's signals ({e}); polling instead"),
        }
    }
    let mut all = futures_util::stream::select_all(streams);
    while all.next().await.is_some() {
        poke.notify_one();
    }
}

/// Find the modem, and keep finding it. When it appears (or reappears under a new
/// path after a reset), subscribe to its calls, texts and alerts and pick up any it
/// already holds. Driven by ModemManager's signals, with a slow look as a fallback.
async fn follow_modem(phone: Arc<Phone>) {
    let mut current: Option<mm::Found> = None;
    let mut watchers: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    let poke = Arc::new(tokio::sync::Notify::new());
    tokio::spawn(modem_events(phone.system.clone(), poke.clone()));
    loop {
        phone.refresh_status().await;
        let found = phone.state.lock().await.modem.clone();
        // Watch again when the modem changes, gains Voice or Messaging (a SIM
        // unlocked), or a watcher stopped (a subscription failed, or the bus dropped).
        let stalled = !watchers.is_empty() && watchers.iter().any(|w| w.is_finished());
        if found != current || stalled {
            for w in watchers.drain(..) {
                w.abort();
            }
            let lost = current.as_ref().is_some_and(|c| found.as_ref().is_none_or(|f| f.path != c.path));
            if lost {
                phone.lost_modem().await;
            }
            match &found {
                Some(f) => {
                    tracing::info!(modem = %f.path, voice = f.voice, messaging = f.messaging, "modem found");
                    if f.voice {
                        watchers.push(tokio::spawn(watch_calls(phone.clone(), f.path.clone())));
                    }
                    if f.messaging {
                        watchers.push(tokio::spawn(watch_texts(phone.clone(), f.path.clone())));
                    }
                    if f.broadcast {
                        watchers.push(tokio::spawn(watch_broadcasts(phone.clone(), f.path.clone())));
                    }
                }
                None if current.is_some() => tracing::warn!("modem gone"),
                None => {}
            }
            current = found;
        }
        // Wait for ModemManager to say something, then let the burst settle (a
        // reset changes a dozen properties at once).
        let _ = tokio::time::timeout(Duration::from_secs(30), poke.notified()).await;
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Watch the audio outputs: a Bluetooth headset connecting takes the call, and
/// going away gives it back; either way the front ends are told the routes changed.
/// Polled, because PipeWire's own change stream needs its client library and a
/// look every few seconds is plenty for a headset someone is putting on.
async fn follow_audio(phone: Arc<Phone>) {
    let mut last: Option<(bool, Vec<String>)> = None;
    loop {
        let nodes = tokio::task::spawn_blocking(audio::nodes).await.unwrap_or_default();
        let bt = audio::bluetooth_sink(&nodes).is_some();
        let names: Vec<String> = nodes.iter().map(|n| n.name.clone()).collect();
        if let Some((was_bt, was_names)) = &last {
            if *was_bt != bt {
                let p = phone.clone();
                tokio::task::spawn_blocking(move || p.audio.bluetooth_changed(bt));
            }
            if *was_names != names {
                api::emit_routes_changed(&phone.session).await;
            }
        }
        last = Some((bt, names));
        // Quick during a call, when a headset going on or off should move the call
        // at once; relaxed otherwise, which is most of the time.
        let busy = !phone.state.lock().await.calls.is_empty();
        tokio::time::sleep(Duration::from_secs(if busy { 2 } else { 10 })).await;
    }
}

async fn watch_calls(phone: Arc<Phone>, modem: zbus::wire::OwnedObjectPath) {
    let Ok(voice) = mm::VoiceProxy::builder(&phone.system).path(modem).build().await else { return };
    let Ok(mut added) = voice.receive_call_added().await else { return };
    // A call that was ringing before phoned started is still worth answering.
    if let Ok(existing) = voice.list_calls().await {
        for path in existing {
            phone.incoming(path).await;
        }
    }
    while let Some(sig) = added.next().await {
        if let Ok(args) = sig.args() {
            let phone = phone.clone();
            let path = args.path.clone();
            tokio::spawn(async move { phone.incoming(path).await });
        }
    }
}

async fn watch_broadcasts(phone: Arc<Phone>, modem: zbus::wire::OwnedObjectPath) {
    let Ok(cb) = mm::CellBroadcastProxy::builder(&phone.system).path(modem).build().await else { return };
    let Ok(mut added) = cb.receive_added().await else { return };
    if let Ok(existing) = cb.list().await {
        for path in existing {
            phone.alert_added(path).await;
        }
    }
    while let Some(sig) = added.next().await {
        if let Ok(args) = sig.args() {
            let phone = phone.clone();
            let path = args.path.clone();
            tokio::spawn(async move { phone.alert_added(path).await });
        }
    }
}

async fn watch_texts(phone: Arc<Phone>, modem: zbus::wire::OwnedObjectPath) {
    let Ok(messaging) = mm::MessagingProxy::builder(&phone.system).path(modem).build().await else { return };
    let Ok(mut added) = messaging.receive_added().await else { return };
    // Texts that arrived while phoned was not running wait on the modem.
    if let Ok(existing) = messaging.list().await {
        for path in existing {
            phone.received(path).await;
        }
    }
    while let Some(sig) = added.next().await {
        if let Ok(args) = sig.args() {
            if args.received {
                let phone = phone.clone();
                let path = args.path.clone();
                tokio::spawn(async move { phone.received(path).await });
            }
        }
    }
}
