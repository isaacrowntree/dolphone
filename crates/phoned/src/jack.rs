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
//! Coming and going is watched with inotify on /dev/input, so nothing wakes while
//! nothing changes: a device node appearing (or udev giving it its permissions
//! a moment later) brings a rescan at once and another a second after. A slow
//! poll stays underneath, every minute, for a machine where inotify cannot watch
//! /dev/input (it does not exist yet, or the watch limit is reached), along with a
//! rescan whenever a watched device goes away.
//!
//! The buttons only act while a call is ringing or live, so a media key on a
//! desktop keyboard still plays and pauses music the rest of the time.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
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

/// The rescan underneath inotify, for when it cannot watch.
const FALLBACK_POLL: Duration = Duration::from_secs(60);

/// A second look after a change: udev sets a new node's permissions just after
/// the kernel creates it, and inotify reports that too, but not every system's
/// udev does it in a way that shows.
const SETTLE: Duration = Duration::from_secs(1);

/// Watch for the jack and headset buttons for phoned's lifetime.
pub fn watch(phone: Arc<Phone>) {
    let rt = tokio::runtime::Handle::current();
    let (wake, woken) = mpsc::channel::<()>();
    watch_dir(wake.clone());
    std::thread::spawn(move || {
        let watching: Arc<Mutex<HashSet<PathBuf>>> = Arc::default();
        let mut wait = FALLBACK_POLL;
        loop {
            scan(&phone, &rt, &watching, &wake);
            match woken.recv_timeout(wait) {
                Ok(()) => {
                    // A plug brings several events (the node, its permissions, the
                    // by-id links): take them as one.
                    std::thread::sleep(Duration::from_millis(100));
                    while woken.try_recv().is_ok() {}
                    wait = SETTLE;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => wait = FALLBACK_POLL,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
    });
}

/// Start watching every input device not yet watched that is a jack or a headset.
fn scan(
    phone: &Arc<Phone>,
    rt: &tokio::runtime::Handle,
    watching: &Arc<Mutex<HashSet<PathBuf>>>,
    wake: &mpsc::Sender<()>,
) {
    for (path, dev) in evdev::enumerate() {
        let seen = watching.lock().map(|w| w.contains(&path)).unwrap_or(true);
        if seen || !interesting(&dev) {
            continue;
        }
        tracing::info!(device = dev.name().unwrap_or("?"), "watching for headset events");
        if let Ok(mut w) = watching.lock() {
            w.insert(path.clone());
        }
        let (phone, rt, watching, wake) = (phone.clone(), rt.clone(), watching.clone(), wake.clone());
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
            // Look again now, not at the next minute's poll: a device that went
            // is often one coming straight back (a driver rebinding).
            let _ = wake.send(());
        });
    }
}

// inotify, which std does not wrap. std links libc already, so declaring it is
// enough (as audio.rs does for tcflush).
extern "C" {
    fn inotify_init1(flags: i32) -> i32;
    fn inotify_add_watch(fd: i32, path: *const std::ffi::c_char, mask: u32) -> i32;
}

const IN_ATTRIB: u32 = 0x4;
const IN_MOVED_TO: u32 = 0x80;
const IN_CREATE: u32 = 0x100;
const IN_DELETE: u32 = 0x200;
const IN_CLOEXEC: i32 = 0o2000000;

/// Send a wake whenever an event device in /dev/input appears, goes, or has its
/// permissions changed. Tries again each minute while /dev/input cannot be
/// watched; the fallback poll covers that time.
fn watch_dir(wake: mpsc::Sender<()>) {
    let _ = std::thread::Builder::new().name("input-watch".into()).spawn(move || loop {
        if let Some(mut f) = inotify("/dev/input") {
            use std::io::Read;
            let mut buf = [0u8; 4096];
            while let Ok(n @ 1..) = f.read(&mut buf) {
                if names_event_device(&buf[..n]) && wake.send(()).is_err() {
                    return;
                }
            }
        }
        std::thread::sleep(FALLBACK_POLL);
    });
}

fn inotify(dir: &str) -> Option<std::fs::File> {
    use std::os::fd::FromRawFd;
    let path = std::ffi::CString::new(dir).ok()?;
    // SAFETY: plain flags; the descriptor returned is ours alone.
    let fd = unsafe { inotify_init1(IN_CLOEXEC) };
    if fd < 0 {
        return None;
    }
    // SAFETY: fd is the inotify descriptor just made, and it is owned from here on.
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    // SAFETY: path is a valid C string for the length of the call.
    let wd = unsafe { inotify_add_watch(fd, path.as_ptr(), IN_CREATE | IN_DELETE | IN_ATTRIB | IN_MOVED_TO) };
    if wd < 0 {
        tracing::debug!("{dir} cannot be watched: {}; polling it every minute", std::io::Error::last_os_error());
        return None;
    }
    Some(file)
}

/// Whether a read of inotify events names an `event*` node (not the `mouse*` and
/// `js*` nodes beside it, nor the by-id and by-path directories). Each event is a
/// struct inotify_event: wd, mask, cookie and len, four 32-bit fields, then `len`
/// bytes of name padded with NULs.
fn names_event_device(buf: &[u8]) -> bool {
    let mut at = 0;
    while at + 16 <= buf.len() {
        let len = u32::from_ne_bytes([buf[at + 12], buf[at + 13], buf[at + 14], buf[at + 15]]) as usize;
        let name = buf.get(at + 16..at + 16 + len).unwrap_or_default();
        if name.starts_with(b"event") {
            return true;
        }
        at += 16 + len;
    }
    false
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

#[cfg(test)]
mod tests {
    use super::*;

    fn event(name: &str) -> Vec<u8> {
        let mut padded = name.as_bytes().to_vec();
        padded.resize(name.len().next_multiple_of(16).max(16), 0);
        let mut b = Vec::new();
        for field in [1u32, IN_CREATE, 0, padded.len() as u32] {
            b.extend(field.to_ne_bytes());
        }
        b.extend(padded);
        b
    }

    #[test]
    fn only_event_nodes_wake_the_scan() {
        assert!(names_event_device(&event("event5")));
        assert!(!names_event_device(&event("mouse0")));
        assert!(!names_event_device(&event("by-path")));
        let mut both = event("js0");
        both.extend(event("event12"));
        assert!(names_event_device(&both), "found past the first event");
        assert!(!names_event_device(&both[..20]), "a short read is not misread");
        assert!(!names_event_device(&[]));
    }
}
