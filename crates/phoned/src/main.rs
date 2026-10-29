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
mod jack;
mod launch;
mod mm;
mod notify;
mod phone;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use futures_util::StreamExt;
use phone_api::{PATH, SERVICE};
use tracing_subscriber::EnvFilter;

use crate::phone::Phone;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "phoned=info".into()))
        .init();

    let store_path = std::env::args()
        .skip_while(|a| a != "--store")
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(phone_core::store::default_path);
    let store = match phone_core::store::Store::open(&store_path) {
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

    let system = zbus::Connection::system().await.context("connecting to the system bus")?;
    let session = zbus::connection::Builder::session()
        .name(SERVICE)
        .build()
        .await
        .context("claiming io.github.isaacrowntree.Dolphone on the session bus (is phoned already running?)")?;

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
    tracing::info!(store = %store_path.display(), "phoned up");

    tokio::spawn(follow_modem(phone.clone()));
    tokio::spawn(follow_audio(phone.clone()));
    tokio::spawn(notify::listen(phone.clone()));
    bt.register_agent().await;
    jack::watch(phone.clone());

    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
    phone.audio.stop_ringing();
    phone.audio.stop_call();
    Ok(())
}

/// Find the modem, and keep finding it. When it appears (or reappears under a new
/// path after a reset), subscribe to its calls and texts and pick up any it already
/// holds.
async fn follow_modem(phone: Arc<Phone>) {
    let mut current: Option<mm::Found> = None;
    let mut watchers: Vec<tokio::task::JoinHandle<()>> = Vec::new();
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
                }
                None if current.is_some() => tracing::warn!("modem gone"),
                None => {}
            }
            current = found;
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
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
        tokio::time::sleep(Duration::from_secs(3)).await;
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
