//! Call audio and the ringer.
//!
//! phoned is the only thing that routes call audio. A call is two PipeWire
//! loopbacks that phoned starts when the call connects and stops when it ends:
//!
//! ```text
//!   modem capture  ──▶ (echo canceller, on the speaker) ──▶ output (speaker/jack/BT)
//!   microphone     ──▶ (echo canceller, on the speaker) ──▶ modem playback
//! ```
//!
//! The modem's side is its USB audio card (the RM520N-GL in UAC mode), found by
//! the name ModemManager gives as the call's AudioPort or by the vendor in the node
//! name. When there is none (a modem that routes audio over PCM straight to the
//! codec, or the fake modem), phoned bridges nothing and says so once.
//!
//! The output is chosen per route. Bluetooth is a `bluez_output.*` sink; the jack
//! and the speaker share the codec's sink and are switched with the mixer commands
//! in the audio config, because the NAU8822 driver has no UCM profile to do it.
//! Everything is a subprocess (`pw-dump`, `pw-loopback`, `pw-play`, `amixer`), so
//! this works with whatever PipeWire the image ships and is easy to follow in `ps`.

use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use phone_api::RouteInfo;
use phone_core::model::Route;
use serde::Deserialize;

/// What differs between boards, read from `$XDG_CONFIG_HOME/dolphone/audio.json`.
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Commands that send the codec's output to the built-in speaker.
    pub speaker_commands: Vec<Vec<String>>,
    /// Commands that send it to the 3.5 mm jack.
    pub wired_commands: Vec<Vec<String>>,
    /// The codec's sink, or empty for the default sink.
    pub codec_sink: String,
    /// The microphone for calls on the speaker or jack, or empty for the default.
    pub mic_source: String,
    /// Substrings that identify the modem's audio nodes when the call names none.
    pub modem_match: Vec<String>,
    /// PipeWire node names of the echo canceller (see dist/pipewire), used on the
    /// speaker route when present.
    pub echo_cancel_sink: String,
    pub echo_cancel_source: String,
    /// The jack is assumed present when nothing reports it, so a board without jack
    /// detection still offers the route.
    pub assume_wired: bool,
}

impl Default for Config {
    fn default() -> Self {
        let amixer = |args: &[&str]| -> Vec<String> {
            std::iter::once("amixer").chain(args.iter().copied()).map(String::from).collect()
        };
        Self {
            // The NAU8822's own control names. Unverified on the Flipper One until the
            // board's driver is in hand: override in audio.json if they differ.
            speaker_commands: vec![
                amixer(&["-q", "sset", "Speaker", "unmute"]),
                amixer(&["-q", "sset", "Headphone", "mute"]),
            ],
            wired_commands: vec![
                amixer(&["-q", "sset", "Speaker", "mute"]),
                amixer(&["-q", "sset", "Headphone", "unmute"]),
            ],
            codec_sink: String::new(),
            mic_source: String::new(),
            modem_match: vec!["Quectel".into(), "RM520".into(), "RM500".into()],
            echo_cancel_sink: "dolphone_ec_sink".into(),
            echo_cancel_source: "dolphone_ec_source".into(),
            assume_wired: false,
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let path = std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
            .map(|d| d.join("dolphone/audio.json"));
        path.and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).map_err(|e| tracing::warn!("audio.json: {e}")).ok())
            .unwrap_or_default()
    }
}

/// One PipeWire node, as `pw-dump` describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub name: String,
    pub description: String,
    pub class: String,
}

/// Every audio sink and source PipeWire has.
///
/// pw-dump writes to a file, not a pipe: into a pipe it takes seconds (measured: 8 s
/// against 18 ms for the same dump to a file), which is long enough to hold up a
/// call connecting. It is bounded as well, since PipeWire's tools can stall.
pub fn nodes() -> Vec<Node> {
    let dir = runtime_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return Vec::new();
    }
    let path = dir.join(format!("pw-dump-{}.json", std::process::id()));
    let Ok(file) = std::fs::File::create(&path) else { return Vec::new() };
    let mut cmd = Command::new("pw-dump");
    cmd.stdin(Stdio::null()).stdout(file).stderr(Stdio::null());
    let ok = run_within(cmd, Duration::from_secs(3));
    let text = if ok { std::fs::read_to_string(&path).unwrap_or_default() } else { String::new() };
    let _ = std::fs::remove_file(&path);
    parse_nodes(&text)
}

fn runtime_dir() -> std::path::PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("dolphone")
}

/// Run a command to completion, or kill it once it overruns `limit`.
fn run_within(mut cmd: Command, limit: Duration) -> bool {
    let Ok(mut child) = cmd.spawn() else { return false };
    let deadline = std::time::Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                tracing::warn!("{:?} took longer than {limit:?}; skipped", cmd.get_program());
                return false;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => return false,
        }
    }
}

pub fn parse_nodes(json: &str) -> Vec<Node> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else { return Vec::new() };
    let Some(items) = v.as_array() else { return Vec::new() };
    items
        .iter()
        .filter_map(|o| {
            let props = o.get("info")?.get("props")?;
            let class = props.get("media.class")?.as_str()?;
            if !class.starts_with("Audio/") {
                return None;
            }
            Some(Node {
                name: props.get("node.name")?.as_str()?.to_string(),
                description: props.get("node.description").and_then(|d| d.as_str()).unwrap_or_default().to_string(),
                class: class.to_string(),
            })
        })
        .collect()
}

// Classes are matched by prefix: virtual and duplex variants ("Audio/Source/Virtual")
// carry audio the same way.
fn is_sink(n: &Node) -> bool {
    n.class.starts_with("Audio/Sink")
}

fn is_source(n: &Node) -> bool {
    n.class.starts_with("Audio/Source")
}

/// The Bluetooth sink, if a device is connected: preferring one in a headset
/// profile, which is the one that carries a microphone.
pub fn bluetooth_sink(nodes: &[Node]) -> Option<&Node> {
    let bt: Vec<&Node> = nodes.iter().filter(|n| is_sink(n) && n.name.starts_with("bluez_output.")).collect();
    bt.iter().find(|n| n.name.contains("headset") || n.name.contains("handsfree")).copied().or(bt.first().copied())
}

/// The modem's capture and playback nodes.
pub fn modem_nodes<'a>(nodes: &'a [Node], port: &str, cfg: &Config) -> Option<(&'a Node, &'a Node)> {
    let matches = |n: &Node| {
        (!port.is_empty() && (n.name.contains(port) || n.description.contains(port)))
            || cfg.modem_match.iter().any(|m| n.name.contains(m.as_str()) || n.description.contains(m.as_str()))
    };
    let source = nodes.iter().find(|n| is_source(n) && matches(n))?;
    let sink = nodes.iter().find(|n| is_sink(n) && matches(n))?;
    Some((source, sink))
}

/// The two loopbacks for a call, as pw-loopback argument lists.
pub fn call_plan(route: Route, modem: (&Node, &Node), nodes: &[Node], cfg: &Config) -> Vec<Vec<String>> {
    let (modem_src, modem_sink) = modem;
    let has = |name: &str| !name.is_empty() && nodes.iter().any(|n| n.name == name);
    let ec = route == Route::Speaker && has(&cfg.echo_cancel_sink) && has(&cfg.echo_cancel_source);
    let output = match route {
        Route::Bluetooth => bluetooth_sink(nodes).map(|n| n.name.clone()).unwrap_or_default(),
        _ if ec => cfg.echo_cancel_sink.clone(),
        _ => cfg.codec_sink.clone(),
    };
    let mic = match route {
        // A headset-profile Bluetooth device has a matching source.
        Route::Bluetooth => bluetooth_sink(nodes)
            .map(|n| n.name.replace("bluez_output.", "bluez_input."))
            .filter(|s| has(s))
            .unwrap_or_default(),
        _ if ec => cfg.echo_cancel_source.clone(),
        _ => cfg.mic_source.clone(),
    };
    let loopback = |capture: &str, playback: &str, name: &str| {
        let mut a = vec!["pw-loopback".to_string(), "--name".into(), name.into()];
        if !capture.is_empty() {
            a.push(format!("--capture={capture}"));
        }
        if !playback.is_empty() {
            a.push(format!("--playback={playback}"));
        }
        // Call audio, so policy treats it as a call and nothing ducks it.
        a.push("--playback-props=media.role=Communication".into());
        a
    };
    vec![loopback(&modem_src.name, &output, "dolphone-downlink"), loopback(&mic, &modem_sink.name, "dolphone-uplink")]
}

/// The next route the in-call Route key moves to.
pub fn next_route(current: Route, wired: bool, bluetooth: bool) -> Route {
    let order = [Route::Speaker, Route::Wired, Route::Bluetooth];
    let available = |r: Route| match r {
        Route::Speaker => true,
        Route::Wired => wired,
        Route::Bluetooth => bluetooth,
    };
    let at = order.iter().position(|&r| r == current).unwrap_or(0);
    (1..=order.len()).map(|i| order[(at + i) % order.len()]).find(|&r| available(r)).unwrap_or(Route::Speaker)
}

struct Inner {
    cfg: Config,
    route: Route,
    /// Chosen by the user during this call, so a jack event does not override it.
    pinned: bool,
    wired: bool,
    downlink: Option<Child>,
    uplink: Option<Child>,
    /// The plan in effect, to restart the uplink after unmuting.
    plan: Vec<Vec<String>>,
    in_call: bool,
    port: String,
    muted: bool,
    warned_no_modem_audio: bool,
}

pub struct Audio {
    inner: Mutex<Inner>,
    ringing: Arc<AtomicBool>,
}

impl Audio {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                cfg: Config::load(),
                route: Route::Speaker,
                pinned: false,
                wired: false,
                downlink: None,
                uplink: None,
                plan: Vec::new(),
                in_call: false,
                port: String::new(),
                muted: false,
                warned_no_modem_audio: false,
            }),
            ringing: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn route(&self) -> Route {
        self.inner.lock().map(|i| i.route).unwrap_or(Route::Speaker)
    }

    pub fn routes(&self) -> Vec<RouteInfo> {
        let nodes = nodes();
        let Ok(i) = self.inner.lock() else { return Vec::new() };
        let bt = bluetooth_sink(&nodes);
        vec![
            RouteInfo {
                route: Route::Speaker as u8,
                name: "Speaker".into(),
                available: true,
                active: i.route == Route::Speaker,
            },
            RouteInfo {
                route: Route::Wired as u8,
                name: "Headset".into(),
                available: i.wired || i.cfg.assume_wired,
                active: i.route == Route::Wired,
            },
            RouteInfo {
                route: Route::Bluetooth as u8,
                name: bt.map(|n| n.description.clone()).filter(|d| !d.is_empty()).unwrap_or_else(|| "Bluetooth".into()),
                available: bt.is_some(),
                active: i.route == Route::Bluetooth,
            },
        ]
    }

    pub fn set_route(&self, route: Route) -> Result<(), String> {
        let available = self.routes().iter().any(|r| r.route == route as u8 && r.available);
        if !available {
            return Err(format!("{} is not connected", route.label()));
        }
        let mut i = self.inner.lock().map_err(|_| "audio state poisoned".to_string())?;
        i.pinned = true;
        apply_route(&mut i, route);
        Ok(())
    }

    pub fn cycle_route(&self) -> Route {
        let bt = bluetooth_sink(&nodes()).is_some();
        let Ok(mut i) = self.inner.lock() else { return Route::Speaker };
        let wired = i.wired || i.cfg.assume_wired;
        let next = next_route(i.route, wired, bt);
        i.pinned = true;
        apply_route(&mut i, next);
        next
    }

    /// The jack changed. Calls follow a headset in and fall back to the speaker when
    /// it comes out, unless the user picked a route themselves.
    pub fn set_wired(&self, present: bool) {
        let Ok(mut i) = self.inner.lock() else { return };
        if i.wired == present {
            return;
        }
        i.wired = present;
        tracing::info!(present, "headset jack");
        if !i.pinned || !present {
            let to = if present {
                Route::Wired
            } else if i.route == Route::Wired {
                Route::Speaker
            } else {
                i.route
            };
            apply_route(&mut i, to);
        }
    }

    /// A Bluetooth audio device connected or went away.
    pub fn bluetooth_changed(&self, connected: bool) {
        let Ok(mut i) = self.inner.lock() else { return };
        if connected && !i.pinned {
            apply_route(&mut i, Route::Bluetooth);
        } else if !connected && i.route == Route::Bluetooth {
            let to = if i.wired { Route::Wired } else { Route::Speaker };
            apply_route(&mut i, to);
        }
    }

    pub fn start_call(&self, audio_port: &str) {
        let Ok(mut i) = self.inner.lock() else { return };
        if i.in_call {
            return;
        }
        i.in_call = true;
        i.port = audio_port.to_string();
        let route = i.route;
        start_loopbacks(&mut i, route);
    }

    pub fn stop_call(&self) {
        let Ok(mut i) = self.inner.lock() else { return };
        stop_loopbacks(&mut i);
        i.in_call = false;
        i.pinned = false;
        i.muted = false;
    }

    pub fn set_muted(&self, muted: bool) {
        let Ok(mut i) = self.inner.lock() else { return };
        i.muted = muted;
        if muted {
            if let Some(mut c) = i.uplink.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
        } else if i.in_call && i.uplink.is_none() {
            if let Some(args) = i.plan.get(1).cloned() {
                i.uplink = spawn(&args);
            }
        }
    }

    /// Ring until stopped. `mode` is the ringer setting; a waiting call gets a short
    /// beep every few seconds instead of the full ring.
    pub fn start_ringing(&self, mode: &str, waiting: bool) {
        if mode != "ring" || self.ringing.swap(true, Ordering::SeqCst) {
            return;
        }
        let Some(file) = tone_file(if waiting { Tone::Waiting } else { Tone::Ring }) else {
            self.ringing.store(false, Ordering::SeqCst);
            return;
        };
        let flag = self.ringing.clone();
        std::thread::spawn(move || {
            // A ringer that is never stopped (a call phoned lost track of) must not
            // ring forever; networks give up on an unanswered call within a minute.
            let started = std::time::Instant::now();
            while flag.load(Ordering::SeqCst) && started.elapsed() < Duration::from_secs(90) {
                let child = Command::new("pw-play")
                    .arg("--media-role=Notification")
                    .arg(&file)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn();
                let Ok(mut child) = child else {
                    tracing::warn!("pw-play is missing: ringing silently");
                    break;
                };
                // Poll so a stop takes effect within a beat, not at the end of the tone.
                loop {
                    if !flag.load(Ordering::SeqCst) {
                        let _ = child.kill();
                        let _ = child.wait();
                        return;
                    }
                    if let Ok(Some(_)) = child.try_wait() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                if waiting {
                    std::thread::sleep(Duration::from_secs(3));
                }
            }
        });
    }

    pub fn stop_ringing(&self) {
        self.ringing.store(false, Ordering::SeqCst);
    }
}

impl Default for Audio {
    fn default() -> Self {
        Self::new()
    }
}

fn apply_route(i: &mut Inner, route: Route) {
    let commands = match route {
        Route::Speaker => i.cfg.speaker_commands.clone(),
        Route::Wired => i.cfg.wired_commands.clone(),
        Route::Bluetooth => Vec::new(),
    };
    for c in &commands {
        run(c);
    }
    let changed = i.route != route;
    i.route = route;
    tracing::info!(route = route.label(), "audio route");
    if changed && i.in_call {
        stop_loopbacks(i);
        start_loopbacks(i, route);
    }
}

fn start_loopbacks(i: &mut Inner, route: Route) {
    let nodes = nodes();
    let port = i.port.clone();
    let Some(modem) = modem_nodes(&nodes, &port, &i.cfg) else {
        if !i.warned_no_modem_audio {
            tracing::info!("no modem audio card: the call's audio stays inside the modem");
            i.warned_no_modem_audio = true;
        }
        return;
    };
    let plan = call_plan(route, modem, &nodes, &i.cfg);
    i.downlink = plan.first().and_then(|a| spawn(a));
    i.uplink = if i.muted { None } else { plan.get(1).and_then(|a| spawn(a)) };
    i.plan = plan;
}

fn stop_loopbacks(i: &mut Inner) {
    for c in [i.downlink.take(), i.uplink.take()].into_iter().flatten() {
        let mut c = c;
        let _ = c.kill();
        let _ = c.wait();
    }
}

fn spawn(args: &[String]) -> Option<Child> {
    let (cmd, rest) = args.split_first()?;
    match Command::new(cmd).args(rest).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::warn!("{cmd}: {e}");
            None
        }
    }
}

fn run(args: &[String]) {
    if let Some((cmd, rest)) = args.split_first() {
        if let Err(e) = Command::new(cmd).args(rest).stdout(Stdio::null()).stderr(Stdio::null()).status() {
            tracing::debug!("{cmd}: {e}");
        }
    }
}

#[derive(Clone, Copy)]
enum Tone {
    Ring,
    Waiting,
}

/// The tone as a WAV in the runtime directory, written on first use.
fn tone_file(tone: Tone) -> Option<std::path::PathBuf> {
    let dir = runtime_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(match tone {
        Tone::Ring => "ring.wav",
        Tone::Waiting => "waiting.wav",
    });
    if !path.exists() {
        std::fs::write(&path, wav(&synth(tone))).ok()?;
    }
    Some(path)
}

const RATE: u32 = 48_000;

/// Australia's ring cadence (400 ms on, 200 off, 400 on, 2 s off) on 400+450 Hz,
/// brightened with a little of the octave so it carries from a small speaker; the
/// waiting tone is two short 425 Hz pips.
fn synth(tone: Tone) -> Vec<i16> {
    let ms = |m: u32| (RATE * m / 1000) as usize;
    let mut out = Vec::new();
    let mut burst = |len: usize, freqs: &[f32], amp: f32| {
        for n in 0..len {
            let t = n as f32 / RATE as f32;
            // 10 ms fade at each end, so the bursts do not click.
            let edge = (n.min(len - n) as f32 / ms(10) as f32).min(1.0);
            let s: f32 = freqs.iter().map(|f| (t * f * std::f32::consts::TAU).sin()).sum::<f32>() / freqs.len() as f32;
            out.push((s * amp * edge * i16::MAX as f32) as i16);
        }
    };
    match tone {
        Tone::Ring => {
            burst(ms(400), &[400.0, 450.0, 800.0], 0.7);
            burst(ms(200), &[0.0], 0.0);
            burst(ms(400), &[400.0, 450.0, 800.0], 0.7);
            burst(ms(2000), &[0.0], 0.0);
        }
        Tone::Waiting => {
            burst(ms(200), &[425.0], 0.4);
            burst(ms(200), &[0.0], 0.0);
            burst(ms(200), &[425.0], 0.4);
        }
    }
    out
}

fn wav(samples: &[i16]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut b = Vec::with_capacity(44 + data_len as usize);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes()); // PCM
    b.extend_from_slice(&1u16.to_le_bytes()); // mono
    b.extend_from_slice(&RATE.to_le_bytes());
    b.extend_from_slice(&(RATE * 2).to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        b.extend_from_slice(&s.to_le_bytes());
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUMP: &str = r#"[
      {"id": 30, "type": "PipeWire:Interface:Node", "info": {"props": {"media.class": "Audio/Sink", "node.name": "alsa_output.platform-sound.stereo-fallback", "node.description": "NAU8822"}}},
      {"id": 31, "type": "PipeWire:Interface:Node", "info": {"props": {"media.class": "Audio/Source", "node.name": "alsa_input.platform-sound.stereo-fallback", "node.description": "NAU8822 Mic"}}},
      {"id": 40, "type": "PipeWire:Interface:Node", "info": {"props": {"media.class": "Audio/Sink", "node.name": "alsa_output.usb-Quectel_RM520N-GL-00.mono-fallback", "node.description": "RM520N-GL Analog Mono"}}},
      {"id": 41, "type": "PipeWire:Interface:Node", "info": {"props": {"media.class": "Audio/Source", "node.name": "alsa_input.usb-Quectel_RM520N-GL-00.mono-fallback", "node.description": "RM520N-GL Analog Mono"}}},
      {"id": 50, "type": "PipeWire:Interface:Node", "info": {"props": {"media.class": "Audio/Sink", "node.name": "bluez_output.AA_BB_CC_DD_EE_FF.1", "node.description": "Pixel Buds"}}},
      {"id": 51, "type": "PipeWire:Interface:Node", "info": {"props": {"media.class": "Audio/Source", "node.name": "bluez_input.AA_BB_CC_DD_EE_FF.1", "node.description": "Pixel Buds"}}},
      {"id": 60, "type": "PipeWire:Interface:Node", "info": {"props": {"media.class": "Audio/Sink", "node.name": "dolphone_ec_sink"}}},
      {"id": 61, "type": "PipeWire:Interface:Node", "info": {"props": {"media.class": "Audio/Source", "node.name": "dolphone_ec_source"}}},
      {"id": 5, "type": "PipeWire:Interface:Client", "info": {"props": {"application.name": "x"}}}
    ]"#;

    #[test]
    fn parses_and_finds_modem() {
        let n = parse_nodes(DUMP);
        assert_eq!(n.len(), 8);
        let cfg = Config::default();
        let (src, sink) = modem_nodes(&n, "", &cfg).unwrap();
        assert!(src.name.starts_with("alsa_input.usb-Quectel"));
        assert!(sink.name.starts_with("alsa_output.usb-Quectel"));
        assert_eq!(bluetooth_sink(&n).unwrap().description, "Pixel Buds");
    }

    #[test]
    fn speaker_plan_uses_echo_cancel() {
        let n = parse_nodes(DUMP);
        let cfg = Config::default();
        let m = modem_nodes(&n, "", &cfg).unwrap();
        let plan = call_plan(Route::Speaker, m, &n, &cfg);
        assert!(plan[0].contains(&"--playback=dolphone_ec_sink".to_string()));
        assert!(plan[1].contains(&"--capture=dolphone_ec_source".to_string()));
        assert!(plan[1].iter().any(|a| a.starts_with("--playback=alsa_output.usb-Quectel")));
    }

    #[test]
    fn bluetooth_plan_uses_headset_mic() {
        let n = parse_nodes(DUMP);
        let cfg = Config::default();
        let m = modem_nodes(&n, "", &cfg).unwrap();
        let plan = call_plan(Route::Bluetooth, m, &n, &cfg);
        assert!(plan[0].contains(&"--playback=bluez_output.AA_BB_CC_DD_EE_FF.1".to_string()));
        assert!(plan[1].contains(&"--capture=bluez_input.AA_BB_CC_DD_EE_FF.1".to_string()));
    }

    #[test]
    fn route_cycle_skips_missing() {
        assert_eq!(next_route(Route::Speaker, false, false), Route::Speaker);
        assert_eq!(next_route(Route::Speaker, true, false), Route::Wired);
        assert_eq!(next_route(Route::Wired, true, false), Route::Speaker);
        assert_eq!(next_route(Route::Speaker, false, true), Route::Bluetooth);
        assert_eq!(next_route(Route::Bluetooth, true, true), Route::Speaker);
    }

    #[test]
    fn tones_are_valid_wav() {
        let w = wav(&synth(Tone::Ring));
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(&w[8..12], b"WAVE");
        // 3 s of 48 kHz mono 16-bit, plus the header.
        assert_eq!(w.len(), 44 + 2 * 48_000 * 3);
    }
}
