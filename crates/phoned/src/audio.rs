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
//! of the machine's device profile (see devices.rs): the Flipper One's NAU8822
//! driver has no UCM profile to do it. A phone whose modem is wired to the codec
//! (PinePhone, Librem 5) needs no bridge at all, only its UCM voice-call verb,
//! which the profile's call_start_commands set.
//! Everything is a subprocess (`pw-dump`, `pw-loopback`, `pw-play`, `amixer`), so
//! this works with whatever PipeWire the image ships and is easy to follow in `ps`.

use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use phone_api::RouteInfo;
use phone_core::model::Route;
use phone_core::tones::{self, Progress};
use serde::{Deserialize, Serialize};

/// What differs between machines: from the device profile, then the owner's
/// `$XDG_CONFIG_HOME/dolphone/audio.json`.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    /// Commands that send the codec's output to the built-in speaker.
    pub speaker_commands: Vec<Vec<String>>,
    /// Commands that send it to the 3.5 mm jack.
    pub wired_commands: Vec<Vec<String>>,
    /// Commands that send it to the earpiece, on a phone that has one. A machine
    /// with these offers the Earpiece route, and calls start there.
    pub earpiece_commands: Vec<Vec<String>>,
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
    /// Run as a call's audio starts and after it ends: a phone's UCM voice-call
    /// verb, a modem's AT command, an amplifier's enable line.
    pub call_start_commands: Vec<Vec<String>>,
    pub call_stop_commands: Vec<Vec<String>>,
    /// The serial port a modem sends call audio over as raw PCM, for modems with
    /// no USB sound card (SIMCom SIM7600: its "Audio" port, /dev/ttyUSB4). Used
    /// when ModemManager names no audio port for the call.
    pub serial_pcm: String,
}

/// What any machine gets before its profile: no mixer controls touched, the
/// default devices, the echo canceller when it is installed, and the modem audio
/// cards Linux phones and boards commonly carry.
impl Default for Config {
    fn default() -> Self {
        Self {
            speaker_commands: Vec::new(),
            wired_commands: Vec::new(),
            earpiece_commands: Vec::new(),
            codec_sink: String::new(),
            mic_source: String::new(),
            modem_match: ["Quectel", "RM520", "RM500", "EG25", "EC25", "SIMCOM", "SIM7600"].map(String::from).to_vec(),
            echo_cancel_sink: "dolphone_ec_sink".into(),
            echo_cancel_source: "dolphone_ec_source".into(),
            assume_wired: false,
            call_start_commands: Vec::new(),
            call_stop_commands: Vec::new(),
            serial_pcm: String::new(),
        }
    }
}

impl Config {
    /// The owner's audio.json, which has the last word.
    pub fn user_file() -> Option<std::path::PathBuf> {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
            .map(|d| d.join("dolphone/audio.json"))
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

/// Where a route's call audio plays and where its microphone is: (output, mic) as
/// PipeWire node names, empty for the default device.
pub fn endpoints(route: Route, nodes: &[Node], cfg: &Config) -> (String, String) {
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
    (output, mic)
}

/// The two loopbacks for a call, as pw-loopback argument lists.
pub fn call_plan(route: Route, modem: (&Node, &Node), nodes: &[Node], cfg: &Config) -> Vec<Vec<String>> {
    let (modem_src, modem_sink) = modem;
    let (output, mic) = endpoints(route, nodes, cfg);
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

/// The serial port a mobile call's audio comes over, if it does: the port
/// ModemManager names for the call when it is a tty (its SIMCom plugin sets the
/// call's AudioPort), or else the profile's `serial_pcm`.
pub fn serial_port(port: &str, cfg: &Config) -> Option<String> {
    if port == SIP_PORT {
        return None;
    }
    if port.starts_with("tty") {
        return Some(format!("/dev/{port}"));
    }
    if port.starts_with("/dev/tty") || port.starts_with("/dev/pts/") {
        return Some(port.to_string());
    }
    (port.is_empty() && !cfg.serial_pcm.is_empty()).then(|| cfg.serial_pcm.clone())
}

/// The two pw-cat legs of a serial PCM call: the downlink plays what the modem
/// sends, the uplink records the microphone for it. The format is fixed at 8 kHz,
/// 16-bit linear, mono (SIMCom USB AUDIO Application Note V1.03, §2.3).
pub fn serial_plan(route: Route, nodes: &[Node], cfg: &Config) -> Vec<Vec<String>> {
    let (output, mic) = endpoints(route, nodes, cfg);
    let cat = |mode: &str, target: &str, name: &str| {
        // --raw: bare samples, not a sound file with a header.
        let mut a: Vec<String> = ["pw-cat", mode, "--raw", "--rate", "8000", "--channels", "1", "--format", "s16"]
            .map(String::from)
            .to_vec();
        a.extend(["--media-role".into(), "Communication".into(), "-P".into(), format!("{{ node.name = {name} }}")]);
        if !target.is_empty() {
            a.extend(["--target".into(), target.into()]);
        }
        a.push("-".into());
        a
    };
    vec![cat("--playback", &output, "dolphone-downlink"), cat("--record", &mic, "dolphone-uplink")]
}

/// The next route the in-call Route key moves to.
pub fn next_route(current: Route, wired: bool, bluetooth: bool, earpiece: bool) -> Route {
    let order = [Route::Earpiece, Route::Speaker, Route::Wired, Route::Bluetooth];
    let available = |r: Route| match r {
        Route::Speaker => true,
        Route::Wired => wired,
        Route::Bluetooth => bluetooth,
        Route::Earpiece => earpiece,
    };
    let at = order.iter().position(|&r| r == current).unwrap_or(0);
    (1..=order.len()).map(|i| order[(at + i) % order.len()]).find(|&r| available(r)).unwrap_or(Route::Speaker)
}

/// The audio port a SIP call reports: its audio is baresip's, played and recorded
/// through PipeWire by baresip itself, so phoned tells it where rather than
/// bridging anything.
pub const SIP_PORT: &str = "sip";

/// What a SIP call's audio needs from baresip.
pub enum SipAudio {
    /// Play to this output and record this microphone (node names; empty is the
    /// default device).
    Route(String, String),
    Mute(bool),
}

type SipHook = Arc<dyn Fn(SipAudio) + Send + Sync>;

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
    /// The serial port the call's audio is bridged over, when it is.
    serial: Option<String>,
    muted: bool,
    warned_no_modem_audio: bool,
    sip: Option<SipHook>,
}

pub struct Audio {
    inner: Mutex<Inner>,
    ringing: Arc<AtomicBool>,
    /// A call-progress tone (ringback) is looping.
    progress: Arc<AtomicBool>,
}

impl Audio {
    pub fn new(cfg: Config) -> Self {
        Self {
            inner: Mutex::new(Inner {
                cfg,
                route: Route::Speaker,
                pinned: false,
                wired: false,
                downlink: None,
                uplink: None,
                plan: Vec::new(),
                in_call: false,
                port: String::new(),
                serial: None,
                muted: false,
                warned_no_modem_audio: false,
                sip: None,
            }),
            ringing: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(AtomicBool::new(false)),
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
        .into_iter()
        .chain((!i.cfg.earpiece_commands.is_empty()).then(|| RouteInfo {
            route: Route::Earpiece as u8,
            name: "Earpiece".into(),
            available: true,
            active: i.route == Route::Earpiece,
        }))
        .collect()
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
        let next = next_route(i.route, wired, bt, !i.cfg.earpiece_commands.is_empty());
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

    /// Use another device profile's audio from the next call on.
    pub fn set_config(&self, cfg: Config) {
        if let Ok(mut i) = self.inner.lock() {
            i.cfg = cfg;
        }
    }

    pub fn start_call(&self, audio_port: &str) {
        let Ok(mut i) = self.inner.lock() else { return };
        if i.in_call {
            return;
        }
        i.in_call = true;
        for c in i.cfg.call_start_commands.clone() {
            run(&c);
        }
        // A phone with an earpiece starts a call there, as phones do, unless a
        // headset or the owner's choice says otherwise.
        if !i.cfg.earpiece_commands.is_empty() && i.route == Route::Speaker && !i.pinned {
            apply_route(&mut i, Route::Earpiece);
        }
        i.port = audio_port.to_string();
        let route = i.route;
        start_loopbacks(&mut i, route);
    }

    pub fn stop_call(&self) {
        let Ok(mut i) = self.inner.lock() else { return };
        if i.in_call {
            for c in i.cfg.call_stop_commands.clone() {
                run(&c);
            }
        }
        stop_loopbacks(&mut i);
        i.in_call = false;
        i.pinned = false;
        i.muted = false;
    }

    /// How to reach baresip for a SIP call's audio.
    pub fn set_sip_hook(&self, hook: impl Fn(SipAudio) + Send + Sync + 'static) {
        if let Ok(mut i) = self.inner.lock() {
            i.sip = Some(Arc::new(hook));
        }
    }

    pub fn set_muted(&self, muted: bool) {
        let Ok(mut i) = self.inner.lock() else { return };
        i.muted = muted;
        if i.port == SIP_PORT {
            if let Some(h) = i.sip.clone() {
                h(SipAudio::Mute(muted));
            }
            return;
        }
        if muted {
            if let Some(mut c) = i.uplink.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
        } else if i.in_call && i.uplink.is_none() {
            if let Some(args) = i.plan.get(1).cloned() {
                i.uplink = match i.serial.clone() {
                    Some(tty) => spawn_serial(&args, &tty, false),
                    None => spawn(&args),
                };
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

    /// Play the country's call-progress tone to whoever is on the call's output.
    /// Ringback loops until stop_progress (or two minutes); busy and congestion
    /// play for about four seconds, as a phone does after the call is released.
    pub fn start_progress(&self, region: &str, p: Progress) {
        let looping = p == Progress::Ringback;
        if looping && self.progress.swap(true, Ordering::SeqCst) {
            return;
        }
        let Some(file) = progress_file(region, p) else {
            self.progress.store(false, Ordering::SeqCst);
            return;
        };
        tracing::info!(tone = p.label(), region, "call-progress tone");
        let flag = self.progress.clone();
        std::thread::spawn(move || {
            let started = std::time::Instant::now();
            loop {
                let Ok(mut child) = Command::new("pw-play")
                    .arg("--media-role=Communication")
                    .arg(&file)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                else {
                    break;
                };
                loop {
                    if looping && !flag.load(Ordering::SeqCst) {
                        let _ = child.kill();
                        let _ = child.wait();
                        return;
                    }
                    if let Ok(Some(_)) = child.try_wait() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                if !looping || !flag.load(Ordering::SeqCst) || started.elapsed() > Duration::from_secs(120) {
                    break;
                }
            }
            if looping {
                flag.store(false, Ordering::SeqCst);
            }
        });
    }

    pub fn stop_progress(&self) {
        self.progress.store(false, Ordering::SeqCst);
    }

    /// Play a tone once, in the background. An alert plays whatever the ringer says;
    /// that is what an emergency alert is for.
    pub fn play(&self, tone: Tone) {
        let Some(file) = tone_file(tone) else { return };
        let role = if matches!(tone, Tone::Alert) { "Alarm" } else { "Notification" };
        std::thread::spawn(move || {
            let _ = Command::new("pw-play")
                .arg(format!("--media-role={role}"))
                .arg(&file)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        });
    }

    /// Say something (a caller's name), when espeak-ng is installed.
    pub fn speak(&self, words: &str) {
        let words = words.to_string();
        std::thread::spawn(move || {
            let dir = std::env::var_os("XDG_RUNTIME_DIR")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(std::env::temp_dir)
                .join("dolphone");
            let wav = dir.join("speech.wav");
            let made = Command::new("espeak-ng")
                .args(["-w"])
                .arg(&wav)
                .arg(&words)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            if !made {
                tracing::debug!("espeak-ng is not installed; not speaking the caller");
                return;
            }
            let _ = Command::new("pw-play")
                .arg("--media-role=Notification")
                .arg(&wav)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        });
    }
}

impl Default for Audio {
    fn default() -> Self {
        Self::new(Config::default())
    }
}

fn apply_route(i: &mut Inner, route: Route) {
    let commands = match route {
        Route::Speaker => i.cfg.speaker_commands.clone(),
        Route::Wired => i.cfg.wired_commands.clone(),
        Route::Bluetooth => Vec::new(),
        Route::Earpiece => i.cfg.earpiece_commands.clone(),
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
    if port == SIP_PORT {
        let (out, mic) = endpoints(route, &nodes, &i.cfg);
        if let Some(h) = i.sip.clone() {
            h(SipAudio::Route(out, mic));
            if i.muted {
                h(SipAudio::Mute(true));
            }
        }
        return;
    }
    if let Some(tty) = serial_port(&port, &i.cfg) {
        // Raw bytes both ways: no echo, no newline translation.
        run(&["stty".into(), "-F".into(), tty.clone(), "raw".into(), "-echo".into()]);
        let plan = serial_plan(route, &nodes, &i.cfg);
        tracing::info!(port = %tty, "call audio over serial PCM");
        i.downlink = plan.first().and_then(|a| spawn_serial(a, &tty, true));
        i.uplink = if i.muted { None } else { plan.get(1).and_then(|a| spawn_serial(a, &tty, false)) };
        i.plan = plan;
        i.serial = Some(tty);
        return;
    }
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
    i.serial = None;
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

/// A pw-cat leg wired to the modem's serial port: the downlink reads from it, the
/// uplink writes to it.
fn spawn_serial(args: &[String], tty: &str, downlink: bool) -> Option<Child> {
    use std::os::unix::fs::OpenOptionsExt;
    let (cmd, rest) = args.split_first()?;
    // O_NOCTTY: the port must never become phoned's controlling terminal.
    const O_NOCTTY: i32 = 0o400;
    let file = match std::fs::OpenOptions::new().read(downlink).write(!downlink).custom_flags(O_NOCTTY).open(tty) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(
                port = tty,
                "the modem's audio port cannot be opened: {e} (is the owner in the dialout group?)"
            );
            return None;
        }
    };
    let (stdin, stdout) =
        if downlink { (Stdio::from(file), Stdio::null()) } else { (Stdio::null(), Stdio::from(file)) };
    match Command::new(cmd).args(rest).stdin(stdin).stdout(stdout).stderr(Stdio::null()).spawn() {
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
pub enum Tone {
    Ring,
    Waiting,
    /// A text arrived.
    Text,
    /// An emergency alert: the attention signal alert systems use (853 + 960 Hz).
    Alert,
}

/// The tone as a WAV in the runtime directory, written on first use.
fn tone_file(tone: Tone) -> Option<std::path::PathBuf> {
    let dir = runtime_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(match tone {
        Tone::Ring => "ring.wav",
        Tone::Waiting => "waiting.wav",
        Tone::Text => "text.wav",
        Tone::Alert => "alert.wav",
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
        Tone::Text => {
            burst(ms(90), &[1320.0], 0.45);
            burst(ms(60), &[0.0], 0.0);
            burst(ms(140), &[1760.0], 0.45);
        }
        Tone::Alert => {
            for _ in 0..4 {
                burst(ms(900), &[853.0, 960.0], 0.9);
                burst(ms(150), &[0.0], 0.0);
            }
        }
    }
    out
}

/// A call-progress tone as a WAV: one cycle of ringback (it loops), or about four
/// seconds of busy or congestion.
fn progress_file(region: &str, p: Progress) -> Option<std::path::PathBuf> {
    let region: String = region.chars().filter(char::is_ascii_alphabetic).take(2).collect();
    let dir = runtime_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{}-{}.wav", p.label(), region.to_ascii_lowercase()));
    if !path.exists() {
        let c = tones::cadence(&region, p);
        let cycles = if p == Progress::Ringback { 1 } else { 4000u32.div_ceil(c.cycle_ms()) };
        std::fs::write(&path, wav(&synth_cadence(c, cycles))).ok()?;
    }
    Some(path)
}

/// A cadence's on/off pattern, `cycles` times. Progress tones are quieter than the
/// ringer: they play in the ear.
fn synth_cadence(c: tones::Cadence, cycles: u32) -> Vec<i16> {
    let ms = |m: u32| (RATE * m / 1000) as usize;
    let steps: &[u32] = if c.steps.is_empty() { &[1000, 0] } else { c.steps };
    let mut out = Vec::new();
    for _ in 0..cycles {
        for (i, &len) in steps.iter().enumerate() {
            let len = ms(len);
            let on = i % 2 == 0;
            for n in 0..len {
                if !on {
                    out.push(0);
                    continue;
                }
                let t = n as f32 / RATE as f32;
                let edge = (n.min(len - n) as f32 / ms(5) as f32).min(1.0);
                let s: f32 =
                    c.freqs.iter().map(|f| (t * f * std::f32::consts::TAU).sin()).sum::<f32>() / c.freqs.len() as f32;
                out.push((s * 0.3 * edge * i16::MAX as f32) as i16);
            }
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
    fn serial_pcm_calls() {
        let n = parse_nodes(DUMP);
        let mut cfg = Config::default();
        assert_eq!(serial_port("", &cfg), None, "no port, no profile: not serial");
        assert_eq!(serial_port("ttyUSB4", &cfg).as_deref(), Some("/dev/ttyUSB4"), "ModemManager's AudioPort");
        assert_eq!(serial_port("card2", &cfg), None);
        cfg.serial_pcm = "/dev/ttyUSB4".into();
        assert_eq!(serial_port("", &cfg).as_deref(), Some("/dev/ttyUSB4"), "the profile's port");
        assert_eq!(serial_port(SIP_PORT, &cfg), None, "never for a SIP call");
        let plan = serial_plan(Route::Speaker, &n, &cfg);
        assert_eq!(
            plan[0][..9],
            ["pw-cat", "--playback", "--raw", "--rate", "8000", "--channels", "1", "--format", "s16"]
        );
        assert!(plan[0].contains(&"dolphone_ec_sink".to_string()), "the speaker goes through the echo canceller");
        assert_eq!(plan[1][1], "--record");
        assert_eq!(plan[1].last().map(String::as_str), Some("-"));
    }

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
    fn progress_tones_have_their_cadence() {
        let au = synth_cadence(tones::cadence("AU", Progress::Ringback), 1);
        assert_eq!(au.len(), RATE as usize * 3, "0.4 + 0.2 + 0.4 + 2.0 s");
        assert!(au[RATE as usize..].iter().all(|&s| s == 0), "silent after the double ring");
        let us = synth_cadence(tones::cadence("US", Progress::Busy), 4);
        assert_eq!(us.len(), RATE as usize * 4);
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
        assert_eq!(next_route(Route::Speaker, false, false, false), Route::Speaker);
        assert_eq!(next_route(Route::Speaker, true, false, false), Route::Wired);
        assert_eq!(next_route(Route::Wired, true, false, false), Route::Speaker);
        assert_eq!(next_route(Route::Speaker, false, true, false), Route::Bluetooth);
        assert_eq!(next_route(Route::Bluetooth, true, true, false), Route::Speaker);
        assert_eq!(next_route(Route::Earpiece, false, false, true), Route::Speaker, "a phone: ear, then speaker");
        assert_eq!(next_route(Route::Speaker, false, false, true), Route::Earpiece, "and back");
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
