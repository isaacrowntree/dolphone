//! Visual ringing: the machine's LEDs flash while a call rings, for someone who
//! cannot hear the ringer, or a ringer set to silent on a desk.
//!
//! Which LEDs is the device profile's `ring_leds` (names under /sys/class/leds).
//! They flash with the kernel's own timer trigger (delay_on and delay_off, in
//! milliseconds, per Documentation/leds/leds-class), so nothing in phoned has to
//! keep time. When the ringing stops each LED gets back what it had: on the
//! Flipper One the four LEDs show the network interfaces' carrier, through the
//! netdev trigger that Flipper's image sets up in udev (99-network-leds.rules), and
//! must go on doing so. Changing an LED's trigger drops the old trigger's settings
//! (netdev's interface and which events light it), so those are kept and written
//! back after the trigger, as is the brightness of an LED with no trigger.
//! Writing those files needs permission: dist/udev/70-dolphone-leds.rules gives the
//! dolphone group the LED's trigger, delay, brightness and netdev files, as
//! DEVICES.md shows.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// What a flashing LED had before, to give back.
#[derive(Clone, Debug, PartialEq)]
struct Before {
    trigger: String,
    /// The trigger's own files and their values, in the order to write them back.
    settings: Vec<(String, String)>,
    brightness: String,
}

static BEFORE: Mutex<Option<HashMap<String, Before>>> = Mutex::new(None);

/// The netdev trigger's files (Documentation/ABI/testing/sysfs-class-led-trigger-netdev).
/// device_name goes first: the mode files apply to the interface it names.
const NETDEV: &[&str] = &[
    "device_name",
    "interval",
    "link",
    "link_10",
    "link_100",
    "link_1000",
    "link_2500",
    "link_5000",
    "link_10000",
    "half_duplex",
    "full_duplex",
    "rx",
    "tx",
    "rx_err",
    "tx_err",
];

/// A trigger's own settings, as they are now.
fn settings(dir: &Path, trigger: &str) -> Vec<(String, String)> {
    let files: &[&str] = if trigger == "netdev" { NETDEV } else { &[] };
    files
        .iter()
        .filter_map(|f| Some((f.to_string(), std::fs::read_to_string(dir.join(f)).ok()?.trim_end().to_string())))
        .collect()
}

/// Write a trigger's settings back. Its files are new with the trigger and udev
/// opens them to the group a moment later, so what fails is tried again for a
/// second. Unwritable values the kernel only shows (such as an empty
/// device_name) are skipped.
fn restore_settings(dir: PathBuf, settings: Vec<(String, String)>) {
    let pending: Vec<_> = settings.into_iter().filter(|(_, v)| !v.is_empty()).collect();
    let left: Vec<_> = pending.iter().filter(|(f, v)| !write(&dir, f, v)).cloned().collect();
    if left.is_empty() {
        return;
    }
    // Write them all again, in order: a mode written before device_name is lost.
    std::thread::spawn(move || {
        for _ in 0..10 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if pending.iter().all(|(f, v)| write(&dir, f, v)) {
                return;
            }
        }
        tracing::warn!(led = %dir.display(), "could not restore the LED's trigger settings (see DEVICES.md)");
    });
}

/// The Flipper One's MCU scales its RGB LEDs by a group brightness that can be 0,
/// set in FlipCTL's Settings or the MCU's own menu (Documentation/ABI/testing/
/// sysfs-class-led-driver-flipper-one); then nothing it does can light them.
fn group_dark(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join("device/link_brightness")).is_ok_and(|v| v.trim() == "0")
}

/// The selected trigger, which the kernel shows in [brackets] among the others.
fn current_trigger(dir: &Path) -> Option<String> {
    let all = std::fs::read_to_string(dir.join("trigger")).ok()?;
    let t = all.split_whitespace().find(|t| t.starts_with('['))?;
    Some(t.trim_matches(|c| c == '[' || c == ']').to_string())
}

/// Where LEDs are: /sys/class/leds, or for tests (debug builds) DOLPHONE_LEDS_DIR.
fn base() -> PathBuf {
    if cfg!(debug_assertions) {
        if let Some(d) = std::env::var_os("DOLPHONE_LEDS_DIR") {
            return PathBuf::from(d);
        }
    }
    PathBuf::from("/sys/class/leds")
}

/// An LED name as the kernel names them: no path tricks.
fn valid(name: &str) -> bool {
    !name.is_empty() && !name.contains('/') && name != "." && name != ".."
}

fn write(dir: &Path, file: &str, value: &str) -> bool {
    match std::fs::write(dir.join(file), value) {
        Ok(()) => true,
        Err(e) => {
            tracing::debug!(led = %dir.display(), file, "LED: {e}");
            false
        }
    }
}

/// Flash these LEDs: on 250 ms, off 250 ms, at full brightness.
pub fn flash(names: &[String]) {
    for n in names.iter().filter(|n| valid(n)) {
        let dir = base().join(n);
        let max = std::fs::read_to_string(dir.join("max_brightness")).unwrap_or_else(|_| "1".into());
        if group_dark(&dir) {
            tracing::warn!(led = %n, "the LED's group brightness is 0 (FlipCTL: Settings > LED Brightness > Link), so it cannot flash");
        }
        if let (Some(t), Ok(mut before)) = (current_trigger(&dir), BEFORE.lock()) {
            if t != "timer" {
                let brightness = std::fs::read_to_string(dir.join("brightness")).unwrap_or_default().trim().to_string();
                let settings = settings(&dir, &t);
                before.get_or_insert_with(HashMap::new).entry(n.clone()).or_insert(Before {
                    trigger: t,
                    settings,
                    brightness,
                });
            }
        }
        if write(&dir, "trigger", "timer") {
            write(&dir, "brightness", max.trim());
            if !(write(&dir, "delay_on", "250") && write(&dir, "delay_off", "250")) {
                // The timer's delay files are new with the trigger, and udev opens
                // them to the group a moment later; till then it blinks at its own
                // default rate.
                std::thread::spawn(move || {
                    for _ in 0..10 {
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        if write(&dir, "delay_on", "250") && write(&dir, "delay_off", "250") {
                            break;
                        }
                    }
                });
            }
        } else {
            tracing::warn!(led = %n, "cannot flash this LED (no permission? see DEVICES.md)");
        }
    }
}

/// Stop flashing: each LED gets back the trigger it had with its settings, or with
/// no trigger the brightness it had; one never seen before goes off.
pub fn stop(names: &[String]) {
    let mut before = BEFORE.lock().ok().and_then(|mut b| b.take()).unwrap_or_default();
    for n in names.iter().filter(|n| valid(n)) {
        let dir = base().join(n);
        match before.remove(n.as_str()) {
            Some(b) if b.trigger != "none" => {
                if write(&dir, "trigger", &b.trigger) {
                    restore_settings(dir, b.settings);
                }
            }
            Some(b) => {
                write(&dir, "trigger", "none");
                write(&dir, "brightness", if b.brightness.is_empty() { "0" } else { &b.brightness });
            }
            None => {
                write(&dir, "trigger", "none");
                write(&dir, "brightness", "0");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flashes_with_the_timer_trigger_and_stops() {
        let d = tempfile::tempdir().unwrap();
        let led = d.path().join("white:status");
        std::fs::create_dir_all(&led).unwrap();
        std::fs::write(led.join("max_brightness"), "255\n").unwrap();
        std::fs::write(led.join("trigger"), "none timer [netdev] mmc0\n").unwrap();
        std::env::set_var("DOLPHONE_LEDS_DIR", d.path());
        let names = vec!["white:status".to_string(), "../../etc".to_string()];
        flash(&names);
        assert_eq!(std::fs::read_to_string(led.join("trigger")).unwrap(), "timer");
        assert_eq!(std::fs::read_to_string(led.join("delay_on")).unwrap(), "250");
        assert_eq!(std::fs::read_to_string(led.join("brightness")).unwrap(), "255");
        stop(&names);
        assert_eq!(std::fs::read_to_string(led.join("trigger")).unwrap(), "netdev", "its old trigger comes back");
        assert!(!d.path().join("etc").exists(), "names cannot leave the LED directory");
    }

    /// Flipper's image points each network LED at its interface (netdev, link only);
    /// the kernel forgets that when the trigger changes, so it is written back.
    #[test]
    fn netdev_settings_come_back() {
        let d = tempfile::tempdir().unwrap();
        let led = d.path().join("flipper-one:rgb:eth0");
        std::fs::create_dir_all(led.join("device")).unwrap();
        for (f, v) in [
            ("max_brightness", "255\n"),
            ("brightness", "255\n"),
            ("trigger", "none timer [netdev]\n"),
            ("device_name", "end0\n"),
            ("link", "1\n"),
            ("tx", "0\n"),
            ("rx", "0\n"),
            ("interval", "50\n"),
        ] {
            std::fs::write(led.join(f), v).unwrap();
        }
        assert!(!group_dark(&led));
        std::fs::write(led.join("device/link_brightness"), "0\n").unwrap();
        assert!(group_dark(&led));
        let before = settings(&led, "netdev");
        assert_eq!(before[0], ("device_name".to_string(), "end0".to_string()), "the interface is written first");
        // As the kernel would on a trigger change: the old settings are gone.
        std::fs::write(led.join("device_name"), "\n").unwrap();
        std::fs::write(led.join("link"), "0\n").unwrap();
        restore_settings(led.clone(), before);
        assert_eq!(std::fs::read_to_string(led.join("device_name")).unwrap(), "end0");
        assert_eq!(std::fs::read_to_string(led.join("link")).unwrap(), "1");
        assert_eq!(std::fs::read_to_string(led.join("interval")).unwrap(), "50");
        assert!(settings(&led, "timer").is_empty());
    }
}
