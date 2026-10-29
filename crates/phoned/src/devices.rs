//! Device profiles: what differs between the machines Dolphone runs on.
//!
//! Everything about calls and texts is the same on every Linux machine with
//! ModemManager, PipeWire and BlueZ. What is not is the hardware around the modem:
//! the codec's mixer controls for speaker and jack, whether the modem's audio
//! comes over USB or is wired to the codec, an echo canceller, which front ends
//! make sense. A profile is one TOML file that says those things and how to
//! recognise the machine:
//!
//! ```toml
//! id = "my-board"
//! name = "My Board"
//! status = "untested"            # tested, expected or untested
//! source = "where the values came from"
//! front_ends = ["desktop"]       # and/or "flipctl"
//!
//! [match]                         # any one of these picks the profile
//! dt_compatible = ["vendor,board"]
//! dt_model = ["My Board"]         # substrings of /proc/device-tree/model
//! dmi_vendor = []                 # substrings, on machines with DMI (x86)
//! dmi_product = []
//! dmi_family = []
//! dmi_version = []                # a ThinkPad's model name lives here
//! dmi_chassis = [9, 10]           # SMBIOS chassis types (9 laptop, 10 notebook)
//! file_contains = [["/etc/hostname", "myboard"]]
//! priority = 50                   # higher wins when several match
//!
//! ring_leds = ["white:status"]    # LEDs that flash while a call rings
//! modem_at = ["AT+QPCMV=1,2"]     # sent each time the modem appears
//! call_at_start = []               # AT commands as a mobile call connects
//! call_at_end = []                 # and as it ends (ModemManager's SIMCom
//!                                  # plugin already sends AT+CPCMREG itself)
//!
//! [at_port]                       # a spare AT port phoned may use itself, once
//! usb_vendor = "2c7c"             # ModemManager is told to leave it alone
//! usb_products = ["0801"]         # (see crates/phoned/src/atport.rs)
//! usb_interface = "03"            # bInterfaceNumber, as sysfs writes it
//!
//! [audio]                         # any audio.json key; see dist/audio.example.json
//! speaker_commands = [["amixer", "-q", "sset", "Speaker", "unmute"]]
//! ```
//!
//! Profiles are read from the ones built in, then `/usr/share/dolphone/devices`,
//! then `~/.config/dolphone/devices` (a later file with the same id replaces an
//! earlier one). The owner's `device_profile` setting picks one by id; otherwise the
//! best match is used, and the generic profile when nothing matches. The owner's
//! own `~/.config/dolphone/audio.json` still has the last word on audio.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The profiles that ship inside phoned, so it works from a plain build.
const BUILT_IN: &[(&str, &str)] = &[
    ("flipper-one.toml", include_str!("../../../dist/devices/flipper-one.toml")),
    ("clockworkpi-uconsole.toml", include_str!("../../../dist/devices/clockworkpi-uconsole.toml")),
    ("raspberry-pi.toml", include_str!("../../../dist/devices/raspberry-pi.toml")),
    ("pinephone.toml", include_str!("../../../dist/devices/pinephone.toml")),
    ("librem5.toml", include_str!("../../../dist/devices/librem5.toml")),
    ("laptop.toml", include_str!("../../../dist/devices/laptop.toml")),
    ("generic.toml", include_str!("../../../dist/devices/generic.toml")),
];

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Match {
    pub dt_compatible: Vec<String>,
    pub dt_model: Vec<String>,
    pub dmi_vendor: Vec<String>,
    pub dmi_product: Vec<String>,
    pub dmi_family: Vec<String>,
    pub dmi_version: Vec<String>,
    /// SMBIOS chassis types (System Enclosure, type 3): 9 laptop, 10 notebook,
    /// 14 sub-notebook, 31 convertible, 32 detachable.
    pub dmi_chassis: Vec<u32>,
    /// [path, substring] pairs.
    pub file_contains: Vec<(String, String)>,
    pub priority: i32,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub status: String,
    pub source: String,
    pub notes: String,
    pub front_ends: Vec<String>,
    #[serde(rename = "match")]
    pub matching: Match,
    /// LEDs (names under /sys/class/leds) to flash while a call rings.
    pub ring_leds: Vec<String>,
    /// AT commands sent each time the modem appears (through ModemManager, or the
    /// spare AT port), for settings the modem forgets on a restart, such as
    /// `AT+QPCMV=1,2`.
    pub modem_at: Vec<String>,
    /// AT commands sent when a mobile call connects and when it ends, for modems
    /// that must be told to start call audio (a SIM7600's `AT+CPCMREG=1`).
    pub call_at_start: Vec<String>,
    pub call_at_end: Vec<String>,
    /// audio.json keys, laid over the defaults.
    pub audio: toml::Table,
    /// The modem's spare AT port, for AT commands without ModemManager's debug
    /// mode (crate::atport). Empty: none.
    pub at_port: AtPort,
}

/// A modem's AT port by its place on USB, as sysfs has it: the vendor and product
/// ids in hex, and the interface's number.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct AtPort {
    pub usb_vendor: String,
    pub usb_products: Vec<String>,
    pub usb_interface: String,
}

/// What the machine says about itself.
#[derive(Clone, Debug, Default)]
pub struct Facts {
    pub dt_compatible: Vec<String>,
    pub dt_model: String,
    pub dmi_vendor: String,
    pub dmi_product: String,
    pub dmi_family: String,
    pub dmi_version: String,
    pub dmi_chassis: Option<u32>,
}

impl Facts {
    pub fn read() -> Self {
        let read = |p: &str| std::fs::read_to_string(p).unwrap_or_default().trim_matches(['\0', '\n', ' ']).to_string();
        Self {
            dt_compatible: std::fs::read("/proc/device-tree/compatible")
                .unwrap_or_default()
                .split(|b| *b == 0)
                .filter(|s| !s.is_empty())
                .map(|s| String::from_utf8_lossy(s).into_owned())
                .collect(),
            dt_model: read("/proc/device-tree/model"),
            dmi_vendor: read("/sys/class/dmi/id/sys_vendor"),
            dmi_product: read("/sys/class/dmi/id/product_name"),
            dmi_family: read("/sys/class/dmi/id/product_family"),
            dmi_version: read("/sys/class/dmi/id/product_version"),
            dmi_chassis: read("/sys/class/dmi/id/chassis_type").parse().ok(),
        }
    }
}

impl Profile {
    /// Why this profile fits the machine, or None.
    pub fn matches(&self, f: &Facts) -> Option<String> {
        let m = &self.matching;
        let has = |hay: &str, needles: &[String]| {
            needles.iter().find(|n| !n.is_empty() && hay.to_lowercase().contains(&n.to_lowercase())).cloned()
        };
        if let Some(c) = m.dt_compatible.iter().find(|c| f.dt_compatible.iter().any(|x| x == *c)) {
            return Some(format!("device tree is compatible with {c}"));
        }
        if let Some(n) = has(&f.dt_model, &m.dt_model) {
            return Some(format!("device tree model has \"{n}\""));
        }
        if let Some(n) = has(&f.dmi_vendor, &m.dmi_vendor) {
            return Some(format!("made by {n}"));
        }
        if let Some(n) = has(&f.dmi_product, &m.dmi_product) {
            return Some(format!("product is {n}"));
        }
        if let Some(n) = has(&f.dmi_family, &m.dmi_family) {
            return Some(format!("product family is {n}"));
        }
        if let Some(n) = has(&f.dmi_version, &m.dmi_version) {
            return Some(format!("product is a {n}"));
        }
        if let Some(c) = f.dmi_chassis.filter(|c| m.dmi_chassis.contains(c)) {
            return Some(format!("the chassis is type {c} (a portable)"));
        }
        for (path, needle) in &m.file_contains {
            if std::fs::read_to_string(path).is_ok_and(|t| t.contains(needle.as_str())) {
                return Some(format!("{path} has \"{needle}\""));
            }
        }
        None
    }
}

fn user_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("/nonexistent"))
        .join("dolphone/devices")
}

fn parse(name: &str, text: &str) -> Option<Profile> {
    match toml::from_str::<Profile>(text) {
        Ok(p) if !p.id.is_empty() => Some(p),
        Ok(_) => {
            tracing::warn!("device profile {name}: no id");
            None
        }
        Err(e) => {
            tracing::warn!("device profile {name}: {e}");
            None
        }
    }
}

fn read_dir(dir: &Path) -> Vec<Profile> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut paths: Vec<PathBuf> =
        rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|e| e == "toml")).collect();
    paths.sort();
    paths.iter().filter_map(|p| parse(&p.display().to_string(), &std::fs::read_to_string(p).ok()?)).collect()
}

pub fn built_in() -> Vec<Profile> {
    BUILT_IN.iter().filter_map(|(n, t)| parse(n, t)).collect()
}

/// Every profile known, built-in first; a later one with the same id replaces it.
pub fn all() -> Vec<Profile> {
    let mut out = built_in();
    for p in read_dir(Path::new("/usr/share/dolphone/devices")).into_iter().chain(read_dir(&user_dir())) {
        match out.iter_mut().find(|x| x.id == p.id) {
            Some(slot) => *slot = p,
            None => out.push(p),
        }
    }
    out
}

/// The profile to use, and why.
pub fn choose(profiles: &[Profile], facts: &Facts, chosen: &str) -> (Profile, String) {
    if !chosen.is_empty() && chosen != "auto" {
        if let Some(p) = profiles.iter().find(|p| p.id == chosen) {
            return (p.clone(), "chosen in Phone Settings".into());
        }
        tracing::warn!(profile = chosen, "no such device profile; detecting instead");
    }
    let best =
        profiles.iter().filter_map(|p| p.matches(facts).map(|why| (p, why))).max_by_key(|(p, _)| p.matching.priority);
    match best {
        Some((p, why)) => (p.clone(), why),
        None => (
            profiles.iter().find(|p| p.id == "generic").cloned().unwrap_or_default(),
            "nothing more specific matched".into(),
        ),
    }
}

/// The audio config for a profile: the defaults, then the profile's, then the
/// owner's audio.json.
pub fn audio_config(p: &Profile) -> crate::audio::Config {
    let mut v = serde_json::to_value(crate::audio::Config::default()).unwrap_or_default();
    let overlay = |v: &mut serde_json::Value, o: serde_json::Value| {
        if let (Some(base), Some(top)) = (v.as_object_mut(), o.as_object()) {
            for (k, val) in top {
                if !k.starts_with('_') {
                    base.insert(k.clone(), val.clone());
                }
            }
        }
    };
    overlay(&mut v, serde_json::to_value(&p.audio).unwrap_or_default());
    if let Some(own) = crate::audio::Config::user_file()
        .and_then(|f| std::fs::read_to_string(f).ok())
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).map_err(|e| tracing::warn!("audio.json: {e}")).ok())
    {
        overlay(&mut v, own);
    }
    serde_json::from_value(v).unwrap_or_else(|e| {
        tracing::warn!("device profile {}: audio: {e}", p.id);
        crate::audio::Config::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_built_in_profile_parses_and_has_a_way_to_match() {
        let all = built_in();
        assert_eq!(all.len(), BUILT_IN.len(), "each built-in file is a profile");
        for p in &all {
            assert!(!p.name.is_empty() && !p.status.is_empty() && !p.source.is_empty(), "{} is described", p.id);
            assert!(["tested", "expected", "untested"].contains(&p.status.as_str()), "{}: status", p.id);
            let m = &p.matching;
            let can_match = !(m.dt_compatible.is_empty()
                && m.dt_model.is_empty()
                && m.dmi_vendor.is_empty()
                && m.dmi_product.is_empty()
                && m.dmi_family.is_empty()
                && m.dmi_version.is_empty()
                && m.dmi_chassis.is_empty()
                && m.file_contains.is_empty());
            assert!(can_match || p.id == "generic", "{} can be detected", p.id);
            audio_config(p);
        }
    }

    #[test]
    fn detection_picks_the_most_specific() {
        let all = built_in();
        let pick = |f: Facts| choose(&all, &f, "").0.id;
        let dt = |c: &[&str], model: &str| Facts {
            dt_compatible: c.iter().map(|s| s.to_string()).collect(),
            dt_model: model.into(),
            ..Default::default()
        };
        assert_eq!(
            pick(dt(&["pine64,pinephone-1.2", "pine64,pinephone", "allwinner,sun50i-a64"], "Pine64 PinePhone")),
            "pinephone"
        );
        assert_eq!(pick(dt(&["purism,librem5r4", "purism,librem5", "fsl,imx8mq"], "Purism Librem 5r4")), "librem5");
        assert_eq!(
            pick(dt(&["raspberrypi,5-model-b", "brcm,bcm2712"], "Raspberry Pi 5 Model B Rev 1.0")),
            "raspberry-pi"
        );
        assert_eq!(
            pick(Facts {
                dmi_vendor: "LENOVO".into(),
                dmi_version: "ThinkPad X1 Carbon Gen 11".into(),
                ..Default::default()
            }),
            "laptop"
        );
        assert_eq!(pick(Facts { dmi_chassis: Some(10), ..Default::default() }), "laptop", "any notebook");
        assert_eq!(pick(Facts { dmi_chassis: Some(3), ..Default::default() }), "generic", "a desktop tower is not");
        assert_eq!(pick(Facts::default()), "generic");
        assert_eq!(choose(&all, &Facts::default(), "pinephone").1, "chosen in Phone Settings");
    }

    #[test]
    fn profiles_and_audio_json_layer() {
        let p = built_in().into_iter().find(|p| p.id == "flipper-one").unwrap();
        let c = audio_config(&p);
        assert!(!c.speaker_commands.is_empty(), "the Flipper One switches its codec with amixer");
        assert_eq!(p.at_port.usb_interface, "03", "the RM520N's modem port, not ModemManager's primary");
        let g = built_in().into_iter().find(|p| p.id == "generic").unwrap();
        assert!(audio_config(&g).speaker_commands.is_empty(), "generic touches no mixer controls");
    }
}
