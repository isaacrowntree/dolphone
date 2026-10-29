//! Visual ringing: the machine's LEDs flash while a call rings, for someone who
//! cannot hear the ringer, or a ringer set to silent on a desk.
//!
//! Which LEDs is the device profile's `ring_leds` (names under /sys/class/leds).
//! They flash with the kernel's own timer trigger (delay_on and delay_off, in
//! milliseconds, per Documentation/leds/leds-class), so nothing in phoned has to
//! keep time. When the ringing stops each LED gets back the trigger it had: on the
//! Flipper One the four LEDs show the network interfaces' carrier, and must go on
//! doing so.
//! Writing those files needs permission: dist/udev/70-dolphone-leds.rules gives the
//! dolphone group the LED's trigger, delay and brightness files, as DEVICES.md shows.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Each flashing LED's trigger from before, to give back.
static BEFORE: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

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
        if let (Some(t), Ok(mut before)) = (current_trigger(&dir), BEFORE.lock()) {
            if t != "timer" {
                before.get_or_insert_with(HashMap::new).entry(n.clone()).or_insert(t);
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

/// Stop flashing: each LED gets back the trigger it had, or goes off.
pub fn stop(names: &[String]) {
    let mut before = BEFORE.lock().ok().and_then(|mut b| b.take()).unwrap_or_default();
    for n in names.iter().filter(|n| valid(n)) {
        let dir = base().join(n);
        match before.remove(n.as_str()).filter(|t| t != "none") {
            Some(t) => {
                write(&dir, "trigger", &t);
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
}
