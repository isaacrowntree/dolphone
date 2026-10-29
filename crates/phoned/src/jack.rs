//! The headset jack and headset buttons.
//!
//! The kernel reports a jack as a switch (`SW_HEADPHONE_INSERT`,
//! `SW_MICROPHONE_INSERT`) on some input device, and a TRRS headset's button as a key
//! (`KEY_MEDIA`, `KEY_PLAYPAUSE`, `KEY_PHONE`). Bluetooth headsets' buttons arrive
//! the same way, through BlueZ's AVRCP input device. Which device that is depends
//! on the machine (on the Flipper One the RP2350 microcontroller watches the jack
//! and the headset button and passes them on, so it is whatever input device that
//! appears as), so this scans every input device for those capabilities and
//! rescans when devices come and go.
//!
//! The buttons only act while a call is ringing or live, so a media key on a
//! desktop keyboard still plays and pauses music the rest of the time.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use evdev::{Device, EventSummary, KeyCode, SwitchCode};
use phone_core::model::CallState;

use crate::phone::Phone;

const BUTTONS: &[KeyCode] = &[KeyCode::KEY_MEDIA, KeyCode::KEY_PLAYPAUSE, KeyCode::KEY_PHONE];

fn interesting(d: &Device) -> bool {
    let jack = d
        .supported_switches()
        .is_some_and(|s| s.contains(SwitchCode::SW_HEADPHONE_INSERT) || s.contains(SwitchCode::SW_MICROPHONE_INSERT));
    // A keyboard has media keys too, but it also has letters: only devices that are
    // nothing but a few buttons are taken as headsets.
    let headset = d.supported_keys().is_some_and(|k| {
        BUTTONS.iter().any(|b| k.contains(*b)) && !k.contains(KeyCode::KEY_A) && k.iter().count() < 16
    });
    jack || headset
}

/// Watch for the jack and headset buttons for phoned's lifetime.
pub fn watch(phone: Arc<Phone>) {
    let rt = tokio::runtime::Handle::current();
    std::thread::spawn(move || {
        let watching: Arc<Mutex<HashSet<PathBuf>>> = Arc::default();
        loop {
            for (path, dev) in evdev::enumerate() {
                let seen = watching.lock().map(|w| w.contains(&path)).unwrap_or(true);
                if seen || !interesting(&dev) {
                    continue;
                }
                tracing::info!(device = dev.name().unwrap_or("?"), "watching for headset events");
                if let Ok(mut w) = watching.lock() {
                    w.insert(path.clone());
                }
                let (phone, rt, watching) = (phone.clone(), rt.clone(), watching.clone());
                let is_jack = dev.supported_switches().is_some_and(|s| s.contains(SwitchCode::SW_HEADPHONE_INSERT));
                std::thread::spawn(move || {
                    read(dev, &phone, &rt);
                    tracing::debug!("input device {path:?} went away");
                    // Forget it, so the same device coming back (a Bluetooth headset
                    // reconnecting, a USB headset replugged) is watched again; and a
                    // jack that vanished has nothing plugged into it.
                    if let Ok(mut w) = watching.lock() {
                        w.remove(&path);
                    }
                    if is_jack {
                        phone.audio.set_wired(false);
                    }
                });
            }
            std::thread::sleep(Duration::from_secs(5));
        }
    });
}

fn read(mut dev: Device, phone: &Arc<Phone>, rt: &tokio::runtime::Handle) {
    if let Ok(state) = dev.get_switch_state() {
        phone.audio.set_wired(state.contains(SwitchCode::SW_HEADPHONE_INSERT));
    }
    loop {
        let Ok(events) = dev.fetch_events() else { return };
        for ev in events {
            match ev.destructure() {
                EventSummary::Switch(_, SwitchCode::SW_HEADPHONE_INSERT, v) => phone.audio.set_wired(v != 0),
                EventSummary::Key(_, code, 1) if BUTTONS.contains(&code) => {
                    let phone = phone.clone();
                    rt.spawn(async move { button(&phone).await });
                }
                _ => {}
            }
        }
    }
}

/// One press: answer a ringing call, otherwise hang up the live one.
async fn button(phone: &Arc<Phone>) {
    let calls = phone.calls().await;
    if let Some(c) = calls.iter().find(|c| CallState::from_u8(c.state).ringing_in()) {
        let _ = phone.answer(c.id).await;
    } else if let Some(c) = calls.iter().find(|c| CallState::from_u8(c.state) != CallState::Terminated) {
        let _ = phone.hangup(c.id).await;
    }
}
