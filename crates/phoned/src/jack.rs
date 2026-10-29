//! The headset jack and headset buttons.
//!
//! The kernel reports a jack as a switch (`SW_HEADPHONE_INSERT`,
//! `SW_MICROPHONE_INSERT`) on some input device, and a TRRS headset's button as a key
//! (`KEY_MEDIA`, `KEY_PLAYPAUSE`, `KEY_PHONE`). Bluetooth headsets' buttons arrive
//! the same way, through BlueZ's AVRCP input device. Which device that is depends
//! on the board (the NAU8822 driver has no jack support of its own, so on the
//! Flipper One it will be whatever the board wires the detect line to), so this
//! scans every input device for those capabilities and rescans when devices come
//! and go.
//!
//! The buttons only act while a call is ringing or live, so a media key on a
//! desktop keyboard still plays and pauses music the rest of the time.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
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
        let mut watching: HashSet<PathBuf> = HashSet::new();
        loop {
            for (path, dev) in evdev::enumerate() {
                if watching.contains(&path) || !interesting(&dev) {
                    continue;
                }
                tracing::info!(device = dev.name().unwrap_or("?"), "watching for headset events");
                watching.insert(path.clone());
                let (phone, rt) = (phone.clone(), rt.clone());
                std::thread::spawn(move || {
                    read(dev, &phone, &rt);
                    tracing::debug!("input device {path:?} went away");
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
