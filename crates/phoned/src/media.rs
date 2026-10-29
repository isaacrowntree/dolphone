//! Pausing music for a call.
//!
//! When a call rings, any media player that is playing (anything speaking MPRIS on
//! the session bus: a browser tab, a podcast app, the panel's radio) is paused, and
//! the ones phoned paused are resumed when the last call ends. Players the owner
//! paused themselves stay paused.

use std::sync::Mutex;

use zbus::proxy;

#[proxy(interface = "org.mpris.MediaPlayer2.Player", default_path = "/org/mpris/MediaPlayer2")]
pub trait Player {
    fn pause(&self) -> zbus::Result<()>;
    fn play(&self) -> zbus::Result<()>;
    #[zbus(property)]
    fn playback_status(&self) -> zbus::Result<String>;
}

#[derive(Default)]
pub struct Media {
    paused: Mutex<Vec<String>>,
}

impl Media {
    /// Pause everything that is playing, remembering what.
    pub async fn pause_all(&self, session: &zbus::Connection) {
        let Ok(dbus) = zbus::fdo::DBusProxy::new(session).await else { return };
        let Ok(names) = dbus.list_names().await else { return };
        for name in names.iter().map(|n| n.to_string()).filter(|n| n.starts_with("org.mpris.MediaPlayer2.")) {
            let Ok(p) = PlayerProxy::builder(session)
                .destination(name.clone())
                .cache_properties(zbus::proxy::CacheProperties::No)
                .build()
                .await
            else {
                continue;
            };
            if p.playback_status().await.is_ok_and(|s| s == "Playing") && p.pause().await.is_ok() {
                tracing::info!(player = %name, "paused for a call");
                if let Ok(mut v) = self.paused.lock() {
                    v.push(name);
                }
            }
        }
    }

    /// Resume what pause_all paused.
    pub async fn resume(&self, session: &zbus::Connection) {
        let names: Vec<String> = self.paused.lock().map(|mut v| std::mem::take(&mut *v)).unwrap_or_default();
        for name in names {
            if let Ok(p) = PlayerProxy::builder(session).destination(name.clone()).build().await {
                let _ = p.play().await;
            }
        }
    }
}
