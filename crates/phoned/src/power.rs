//! Staying awake for calls, and catching up after sleep.
//!
//! While a call rings or is live, phoned holds a logind inhibitor that blocks sleep
//! and idle, so the board does not suspend mid-call and a desktop does not blank its
//! screen. When the system resumes, phoned re-reads the modem: calls and texts that
//! arrived while it slept are picked up, and a call that rang out while asleep is
//! logged as missed (see Phone::incoming).
//!
//! Waking *for* a call is the modem's job: dist/udev/70-dolphone-modem-wakeup.rules
//! lets the modem's USB interface wake the system, which is how an incoming call or
//! text reaches a sleeping board.

use std::sync::{Arc, Mutex};

use futures_util::StreamExt;
use zbus::proxy;

use crate::phone::Phone;

#[proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
pub trait Login {
    fn inhibit(&self, what: &str, who: &str, why: &str, mode: &str) -> zbus::Result<zbus::wire::OwnedFd>;
    #[zbus(signal)]
    fn prepare_for_sleep(&self, start: bool) -> zbus::Result<()>;
}

/// The inhibitor, held while any call exists. Dropping the descriptor releases it.
#[derive(Default)]
pub struct Awake {
    held: Mutex<Option<zbus::wire::OwnedFd>>,
    /// Bumped by every release, so a hold still waiting for logind when the last
    /// call ends does not keep the inhibitor it then gets.
    released: std::sync::atomic::AtomicU64,
}

impl Awake {
    pub async fn hold(&self, system: &zbus::Connection) {
        if self.held.lock().map(|h| h.is_some()).unwrap_or(true) {
            return;
        }
        let before = self.released.load(std::sync::atomic::Ordering::SeqCst);
        let Ok(login) = LoginProxy::new(system).await else { return };
        match login.inhibit("sleep:idle", "Dolphone", "A call is ringing or in progress", "block").await {
            Ok(fd) => {
                if self.released.load(std::sync::atomic::Ordering::SeqCst) != before {
                    // The call ended meanwhile: dropping the descriptor lets go.
                    return;
                }
                if let Ok(mut h) = self.held.lock() {
                    *h = Some(fd);
                }
                tracing::debug!("holding sleep and idle off for a call");
            }
            Err(e) => tracing::debug!("no logind inhibitor: {e}"),
        }
    }

    pub fn release(&self) {
        self.released.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Ok(mut h) = self.held.lock() {
            if h.take().is_some() {
                tracing::debug!("sleep allowed again");
            }
        }
    }
}

/// Catch up after every resume. Runs for phoned's lifetime.
pub async fn follow_sleep(phone: Arc<Phone>) {
    let Ok(login) = LoginProxy::new(&phone.system).await else { return };
    let Ok(mut sleeps) = login.receive_prepare_for_sleep().await else { return };
    while let Some(sig) = sleeps.next().await {
        let Ok(a) = sig.args() else { continue };
        if !a.start {
            tracing::info!("resumed: re-reading the modem");
            phone.resync().await;
        }
    }
}
