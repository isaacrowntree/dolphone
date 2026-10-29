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
//! The output is chosen per route. Bluetooth is a `bluez_output.*` sink, with the
//! headset switched to its hands-free profile for the call so its microphone is
//! there; the jack and the speaker share the codec's sink and are switched with the
//! mixer commands of the machine's device profile (see devices.rs): the Flipper
//! One's NAU8822 driver has no UCM profile to do it. A phone whose modem is wired to
//! the codec (PinePhone, Librem 5) needs no bridge at all, only its UCM voice-call
//! verb, which the profile's call_start_commands set.
//! Everything is a subprocess (`pw-dump`, `pw-loopback`, `pw-play`, `pw-cli`,
//! `amixer`), so this works with whatever PipeWire the image ships and is easy to
//! follow in `ps`. Each one is bounded in time, since PipeWire's tools can stall.
//!
//! Starting and stopping a call's audio is slow (a dump, a profile switch, child
//! processes), so phoned asks for it with `request_start_call` and
//! `request_stop_call`, which return at once and are carried out in the order they
//! were asked on one worker thread. A short call's stop can then never overtake its
//! start.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use phone_api::RouteInfo;
use phone_core::model::Route;
use phone_core::tones::{self, Progress};
use serde::{Deserialize, Serialize};

/// What differs between machines: from the device profile, then the owner's
/// `$XDG_CONFIG_HOME/dolphone/audio.json`.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    /// Commands that send the codec's output to the built-in speaker. They also run
    /// while the phone rings and after a call, so the ring is heard.
    pub speaker_commands: Vec<Vec<String>>,
    /// Commands that send it to the 3.5 mm jack.
    pub wired_commands: Vec<Vec<String>>,
    /// Commands that send it to the earpiece, on a phone that has one. A machine
    /// with these offers the Earpiece route, and calls start there.
    pub earpiece_commands: Vec<Vec<String>>,
    /// The codec's sink, or empty for the default sink. The ringer plays here, and
    /// the echo canceller's output is pointed here.
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
    pub fn user_file() -> Option<PathBuf> {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .map(|d| d.join("dolphone/audio.json"))
    }
}

/// One PipeWire node, as `pw-dump` describes it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Node {
    pub name: String,
    pub description: String,
    pub class: String,
    /// The node's global id.
    pub id: u32,
    /// The device (card) it belongs to, if any.
    pub device: Option<u32>,
    /// Shared by the nodes of one filter, such as the echo canceller's four.
    pub link_group: String,
    /// WirePlumber's stand-in Bluetooth source (`bluez5.loopback`), which exists
    /// in every profile and switches the headset to hands-free when used.
    pub bt_loopback: bool,
}

/// One device's profile, from its EnumProfile params.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    pub index: u32,
    pub name: String,
    pub priority: i64,
    pub available: bool,
}

/// A PipeWire device (an ALSA card, a Bluetooth headset).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Device {
    pub id: u32,
    pub name: String,
    /// The index of the profile in use.
    pub profile: Option<u32>,
    pub profiles: Vec<Profile>,
    /// The profiles that have an input route: the ones with a microphone.
    pub input_profiles: Vec<u32>,
}

/// What phoned needs from one `pw-dump`.
#[derive(Clone, Debug, Default)]
pub struct Dump {
    /// Audio sinks and sources.
    pub nodes: Vec<Node>,
    /// Audio streams (`Stream/*`), such as the echo canceller's own.
    pub streams: Vec<Node>,
    pub devices: Vec<Device>,
}

/// Every audio sink and source PipeWire has.
pub fn nodes() -> Vec<Node> {
    read_dump().nodes
}

/// A dump of PipeWire's graph.
///
/// pw-dump writes to a file, not a pipe: into a pipe it takes seconds (measured: 8 s
/// against 18 ms for the same dump to a file), which is long enough to hold up a
/// call connecting. It is bounded as well, since PipeWire's tools can stall.
pub fn read_dump() -> Dump {
    let Some(dir) = runtime_dir() else { return Dump::default() };
    let path = dir.join(format!("pw-dump-{}.json", std::process::id()));
    let Ok(file) = std::fs::File::create(&path) else { return Dump::default() };
    let mut cmd = Command::new("pw-dump");
    cmd.stdin(Stdio::null()).stdout(file).stderr(Stdio::null());
    let ok = run_within(cmd, Duration::from_secs(3));
    let text = if ok { std::fs::read_to_string(&path).unwrap_or_default() } else { String::new() };
    let _ = std::fs::remove_file(&path);
    parse_dump(&text)
}

/// phoned's private directory for tones, speech and dumps: `$XDG_RUNTIME_DIR/dolphone`,
/// or `~/.cache/dolphone` when there is no runtime directory. Never a shared
/// directory such as /tmp, where another user could plant or read the files. It
/// must be phoned's own and is kept at 0700.
fn runtime_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|d| !d.is_empty())
        .map(|d| PathBuf::from(d).join("dolphone"))
        .or_else(|| {
            std::env::var_os("HOME").filter(|h| !h.is_empty()).map(|h| PathBuf::from(h).join(".cache/dolphone"))
        })?;
    private_dir(&base)
}

fn private_dir(dir: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir);
    let meta = std::fs::symlink_metadata(dir).ok()?;
    if !meta.is_dir() || meta.uid() != uid() {
        tracing::warn!(dir = %dir.display(), "not phoned's own directory; not using it");
        return None;
    }
    if meta.mode() & 0o077 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).ok()?;
    }
    Some(dir.to_path_buf())
}

// Two libc calls std does not wrap. std links libc already, so declaring them is
// enough.
extern "C" {
    fn getuid() -> u32;
    fn tcflush(fd: i32, queue: i32) -> i32;
}

fn uid() -> u32 {
    // SAFETY: getuid takes nothing, cannot fail and touches no memory of ours.
    unsafe { getuid() }
}

/// Run a command to completion, or kill it once it overruns `limit`.
fn run_within(mut cmd: Command, limit: Duration) -> bool {
    match cmd.spawn() {
        Ok(child) => wait_within(child, limit, &format!("{:?}", cmd.get_program())),
        Err(e) => {
            tracing::debug!("{:?}: {e}", cmd.get_program());
            false
        }
    }
}

fn wait_within(mut child: Child, limit: Duration, what: &str) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                tracing::warn!("{what} took longer than {limit:?}; skipped");
                return false;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => return false,
        }
    }
}

pub fn parse_dump(json: &str) -> Dump {
    let mut dump = Dump::default();
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else { return dump };
    let Some(items) = v.as_array() else { return dump };
    let num = |v: Option<&serde_json::Value>| v.and_then(|v| v.as_u64()).and_then(|n| u32::try_from(n).ok());
    let text = |v: Option<&serde_json::Value>| v.and_then(|v| v.as_str()).unwrap_or_default().to_string();
    for o in items {
        let id = num(o.get("id")).unwrap_or_default();
        let Some(info) = o.get("info") else { continue };
        let Some(props) = info.get("props") else { continue };
        let kind = o.get("type").and_then(|t| t.as_str()).unwrap_or_default();
        if let Some(class) = props.get("media.class").and_then(|c| c.as_str()) {
            let Some(name) = props.get("node.name").and_then(|n| n.as_str()) else { continue };
            let node = Node {
                name: name.to_string(),
                description: text(props.get("node.description")),
                class: class.to_string(),
                id,
                device: num(props.get("device.id")),
                link_group: text(props.get("node.link-group")),
                bt_loopback: props.get("bluez5.loopback").is_some_and(|b| b.as_bool() == Some(true) || *b == "true"),
            };
            if class.starts_with("Audio/") {
                dump.nodes.push(node);
            } else if class.starts_with("Stream/") && class.contains("Audio") {
                dump.streams.push(node);
            }
        } else if kind.ends_with("Device") {
            let params = info.get("params");
            let list =
                |key: &str| params.and_then(|p| p.get(key)).and_then(|l| l.as_array()).cloned().unwrap_or_default();
            let profiles = list("EnumProfile")
                .iter()
                .filter_map(|p| {
                    Some(Profile {
                        index: num(p.get("index"))?,
                        name: text(p.get("name")),
                        priority: p.get("priority").and_then(|n| n.as_i64()).unwrap_or_default(),
                        available: p.get("available").and_then(|a| a.as_str()) != Some("no"),
                    })
                })
                .collect();
            let input_profiles = list("EnumRoute")
                .iter()
                .filter(|r| r.get("direction").and_then(|d| d.as_str()) == Some("Input"))
                .flat_map(|r| r.get("profiles").and_then(|p| p.as_array()).cloned().unwrap_or_default())
                .filter_map(|p| p.as_u64().and_then(|n| u32::try_from(n).ok()))
                .collect();
            dump.devices.push(Device {
                id,
                name: text(props.get("device.name")),
                profile: list("Profile").first().and_then(|p| num(p.get("index"))),
                profiles,
                input_profiles,
            });
        }
    }
    dump
}

// Classes are matched by prefix: virtual and duplex variants ("Audio/Source/Virtual")
// carry audio the same way.
fn is_sink(n: &Node) -> bool {
    n.class.starts_with("Audio/Sink")
}

fn is_source(n: &Node) -> bool {
    n.class.starts_with("Audio/Source")
}

/// A Bluetooth headset's own microphone: a source of the same device that is not
/// WirePlumber's stand-in. In the hands-free profile that is the SCO source, named
/// `bluez_input.<address>.<n>`, or `bluez_input_internal.*` when WirePlumber's
/// stand-in is in front of it (wireplumber's monitors/bluez.lua).
fn bluetooth_mic<'a>(nodes: &'a [Node], sink: &Node) -> Option<&'a Node> {
    let same = |n: &&Node| is_source(n) && sink.device.is_some() && n.device == sink.device;
    nodes.iter().filter(same).find(|n| !n.bt_loopback)
}

/// The Bluetooth sink, if a device is connected: preferring one whose device has
/// its microphone up (a headset in its hands-free profile).
pub fn bluetooth_sink(nodes: &[Node]) -> Option<&Node> {
    let bt: Vec<&Node> = nodes.iter().filter(|n| is_sink(n) && n.name.starts_with("bluez_output.")).collect();
    bt.iter()
        .find(|n| bluetooth_mic(nodes, n).is_some() || n.name.contains("headset") || n.name.contains("handsfree"))
        .copied()
        .or(bt.first().copied())
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
    let bt = bluetooth_sink(nodes);
    let output = match route {
        Route::Bluetooth => bt.map(|n| n.name.clone()).unwrap_or_default(),
        _ if ec => cfg.echo_cancel_sink.clone(),
        _ => cfg.codec_sink.clone(),
    };
    let mic = match route {
        // The headset's own microphone; failing that, WirePlumber's stand-in for
        // it, which asks WirePlumber to switch the headset to hands-free itself.
        Route::Bluetooth => bt
            .and_then(|sink| {
                bluetooth_mic(nodes, sink)
                    .or_else(|| nodes.iter().find(|n| is_source(n) && n.device == sink.device && n.device.is_some()))
                    .map(|n| n.name.clone())
                    .or_else(|| Some(sink.name.replace("bluez_output.", "bluez_input.")).filter(|s| has(s)))
            })
            .unwrap_or_default(),
        _ if ec => cfg.echo_cancel_source.clone(),
        _ => cfg.mic_source.clone(),
    };
    (output, mic)
}

/// Stream properties that keep a call's stream on its device: when the device
/// goes (a headset out of range, PipeWire restarting) WirePlumber destroys the
/// stream rather than moving it to the default device, and one whose device is
/// not there yet is not linked to the default instead (wireplumber's
/// linking/prepare-link.lua and find-defined-target.lua). The child then exits and
/// check_call starts it again where it belongs.
const STAY: &str = "node.dont-reconnect=true node.dont-fallback=true";

/// The two loopbacks for a call, as pw-loopback argument lists.
pub fn call_plan(route: Route, modem: (&Node, &Node), nodes: &[Node], cfg: &Config) -> Vec<Vec<String>> {
    let (modem_src, modem_sink) = modem;
    let (output, mic) = endpoints(route, nodes, cfg);
    let loopback = |capture: &str, playback: &str, name: &str| {
        let mut a = vec!["pw-loopback".to_string(), "--name".into(), name.into()];
        if !capture.is_empty() {
            a.push(format!("--capture={capture}"));
            a.push(format!("--capture-props={STAY}"));
        }
        if !playback.is_empty() {
            a.push(format!("--playback={playback}"));
        }
        // Call audio, so policy treats it as a call and nothing ducks it.
        let stay = if playback.is_empty() { String::new() } else { format!(" {STAY}") };
        a.push(format!("--playback-props=media.role=Communication{stay}"));
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

/// The serial PCM rate: what ModemManager reports in the call's AudioFormat, or
/// 8 kHz, SIMCom's default (USB AUDIO Application Note V1.03, §2.3). Its SIMCom
/// plugin switches the modem to 16 kHz (AT+CPCMFRM=1) where the firmware has it.
pub fn pcm_rate(reported: u32) -> u32 {
    if reported == 16_000 {
        16_000
    } else {
        8_000
    }
}

/// The two pw-cat legs of a serial PCM call: the downlink plays what the modem
/// sends, the uplink records the microphone for it. 16-bit linear, mono, at `rate`.
pub fn serial_plan(route: Route, nodes: &[Node], cfg: &Config, rate: u32) -> Vec<Vec<String>> {
    let (output, mic) = endpoints(route, nodes, cfg);
    let rate = pcm_rate(rate).to_string();
    let cat = |mode: &str, target: &str, name: &str| {
        // --raw: bare samples, not a sound file with a header.
        let mut a: Vec<String> =
            ["pw-cat", mode, "--raw", "--rate", &rate, "--channels", "1", "--format", "s16"].map(String::from).to_vec();
        let stay = if target.is_empty() { String::new() } else { format!(" {}", STAY.replace('=', " = ")) };
        a.extend([
            "--media-role".into(),
            "Communication".into(),
            "-P".into(),
            format!("{{ node.name = {name}{stay} }}"),
        ]);
        if !target.is_empty() {
            a.extend(["--target".into(), target.into()]);
        }
        a.push("-".into());
        a
    };
    vec![cat("--playback", &output, "dolphone-downlink"), cat("--record", &mic, "dolphone-uplink")]
}

/// The profile to switch a Bluetooth headset to for a call, or None when the one
/// it is in already has a microphone (or none does). As WirePlumber's own
/// autoswitch picks it (device/autoswitch-bluetooth-profile.lua): the highest
/// priority profile with an input route, which for a headset is
/// `headset-head-unit` (HFP/HSP, mSBC where both ends can) or, for an LE Audio one,
/// `bap-duplex`. A dump without routes falls back to those names.
pub fn headset_profile(dev: &Device) -> Option<u32> {
    let with_mic = |p: &Profile| {
        if dev.input_profiles.is_empty() {
            p.name.starts_with("headset-head-unit") || p.name.starts_with("bap-duplex")
        } else {
            dev.input_profiles.contains(&p.index)
        }
    };
    let current = dev.profiles.iter().find(|p| Some(p.index) == dev.profile);
    if current.is_some_and(with_mic) {
        return None;
    }
    dev.profiles.iter().filter(|p| p.available && with_mic(p)).max_by_key(|p| p.priority).map(|p| p.index)
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
    /// The route, readable without the lock, which a call's audio starting can hold
    /// for seconds.
    shown: Arc<AtomicU8>,
    /// Chosen by the user during this call, so a jack event does not override it.
    pinned: bool,
    wired: bool,
    /// The downlink and the uplink.
    legs: [Option<Child>; 2],
    /// The plan in effect: what each leg runs, to restart one that died or the
    /// uplink after unmuting, and to leave alone a leg a route change does not move.
    plan: Vec<Vec<String>>,
    in_call: bool,
    port: String,
    /// The serial PCM rate the call's audio runs at, when it is serial.
    rate: u32,
    /// The serial port the call's audio is bridged over, when it is.
    serial: Option<String>,
    /// Where the call's audio plays, for the tones that belong in the ear.
    output: String,
    /// A Bluetooth headset phoned moved to its hands-free profile for the call, and
    /// the profile it was in: (device name, profile index).
    bt_restore: Option<(String, u32)>,
    muted: bool,
    warned_no_modem_audio: bool,
    sip: Option<SipHook>,
}

impl Inner {
    fn set_route(&mut self, route: Route) {
        self.route = route;
        self.shown.store(route as u8, Ordering::SeqCst);
        tracing::info!(route = route.label(), "audio route");
    }
}

/// A sound that loops on its own thread until stopped. Each start gets a ticket,
/// so a thread that ends on its own clears only its own run, never a newer one.
#[derive(Default)]
struct Looper {
    current: AtomicU64,
    issued: AtomicU64,
}

impl Looper {
    /// A ticket for a new run, or None when one is already going.
    fn begin(&self) -> Option<u64> {
        let ticket = self.issued.fetch_add(1, Ordering::SeqCst) + 1;
        self.current.compare_exchange(0, ticket, Ordering::SeqCst, Ordering::SeqCst).ok().map(|_| ticket)
    }

    fn live(&self, ticket: u64) -> bool {
        self.current.load(Ordering::SeqCst) == ticket
    }

    /// The run's thread is done, whatever the reason.
    fn end(&self, ticket: u64) {
        let _ = self.current.compare_exchange(ticket, 0, Ordering::SeqCst, Ordering::SeqCst);
    }

    fn stop(&self) {
        self.current.store(0, Ordering::SeqCst);
    }

    #[cfg(test)]
    fn running(&self) -> bool {
        self.current.load(Ordering::SeqCst) != 0
    }
}

struct Shared {
    inner: Mutex<Inner>,
    ring: Looper,
    /// A call-progress tone (ringback) is looping.
    progress: Looper,
    /// Bumped by every request to start or stop a call's audio, so a start that a
    /// later request has overtaken is skipped rather than run late.
    requests: AtomicU64,
}

enum Job {
    Start {
        port: String,
        rate: u32,
        request: u64,
    },
    Stop,
    Check,
    #[cfg(test)]
    Flush(mpsc::Sender<()>),
}

pub struct Audio {
    s: Arc<Shared>,
    jobs: mpsc::Sender<Job>,
    route_shown: Arc<AtomicU8>,
}

impl Audio {
    pub fn new(cfg: Config) -> Self {
        let s = Arc::new(Shared {
            inner: Mutex::new(Inner {
                cfg,
                route: Route::Speaker,
                shown: Arc::new(AtomicU8::new(Route::Speaker as u8)),
                pinned: false,
                wired: false,
                legs: [None, None],
                plan: Vec::new(),
                in_call: false,
                port: String::new(),
                rate: 8_000,
                serial: None,
                output: String::new(),
                bt_restore: None,
                muted: false,
                warned_no_modem_audio: false,
                sip: None,
            }),
            ring: Looper::default(),
            progress: Looper::default(),
            requests: AtomicU64::new(0),
        });
        let route_shown = s.inner.lock().map(|i| i.shown.clone()).unwrap_or_default();
        let (jobs, rx) = mpsc::channel();
        let worker = s.clone();
        // One thread, so requests run in the order they were made. It ends when the
        // Audio does.
        let _ = std::thread::Builder::new().name("call-audio".into()).spawn(move || {
            for job in rx {
                match job {
                    Job::Start { port, rate, request } => {
                        if worker.requests.load(Ordering::SeqCst) == request {
                            start_call(&worker, &port, rate);
                        }
                    }
                    Job::Stop => stop_call(&worker),
                    Job::Check => check_call(&worker),
                    #[cfg(test)]
                    Job::Flush(done) => {
                        let _ = done.send(());
                    }
                }
            }
        });
        Self { s, jobs, route_shown }
    }

    fn lock(&self) -> Option<std::sync::MutexGuard<'_, Inner>> {
        self.s.inner.lock().ok()
    }

    pub fn route(&self) -> Route {
        Route::from_u8(self.route_shown.load(Ordering::SeqCst))
    }

    pub fn routes(&self) -> Vec<RouteInfo> {
        let nodes = nodes();
        self.lock().map(|i| route_infos(&i, &nodes)).unwrap_or_default()
    }

    pub fn set_route(&self, route: Route) -> Result<(), String> {
        let dump = read_dump();
        let mut i = self.s.inner.lock().map_err(|_| "audio state poisoned".to_string())?;
        if !route_infos(&i, &dump.nodes).iter().any(|r| r.route == route as u8 && r.available) {
            return Err(format!("{} is not connected", route.label()));
        }
        i.pinned = true;
        apply_route(&mut i, route, Some(dump));
        Ok(())
    }

    pub fn cycle_route(&self) -> Route {
        let dump = read_dump();
        let bt = bluetooth_sink(&dump.nodes).is_some();
        let Some(mut i) = self.lock() else { return Route::Speaker };
        let wired = i.wired || i.cfg.assume_wired;
        let next = next_route(i.route, wired, bt, !i.cfg.earpiece_commands.is_empty());
        i.pinned = true;
        apply_route(&mut i, next, Some(dump));
        next
    }

    /// The jack changed. Calls follow a headset in and fall back to the speaker when
    /// it comes out, unless the user picked a route themselves.
    pub fn set_wired(&self, present: bool) {
        let Some(mut i) = self.lock() else { return };
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
            apply_route(&mut i, to, None);
        }
    }

    /// A Bluetooth audio device connected or went away.
    pub fn bluetooth_changed(&self, connected: bool) {
        let dump = read_dump();
        // A headset changing profile (phoned's own doing, at a call's start) drops
        // its sink for a moment; one that is back is still connected.
        let connected = connected || bluetooth_sink(&dump.nodes).is_some();
        let Some(mut i) = self.lock() else { return };
        if connected && !i.pinned {
            apply_route(&mut i, Route::Bluetooth, Some(dump));
        } else if !connected && i.route == Route::Bluetooth {
            // Looked at again with the lock held: a call's start may have been
            // switching the headset's profile when the first look was taken.
            let dump = read_dump();
            if bluetooth_sink(&dump.nodes).is_some() {
                return;
            }
            let to = if i.wired { Route::Wired } else { Route::Speaker };
            apply_route(&mut i, to, Some(dump));
        }
    }

    /// Use another device profile's audio from the next call on.
    pub fn set_config(&self, cfg: Config) {
        if let Some(mut i) = self.lock() {
            i.cfg = cfg;
        }
    }

    /// Start a call's audio, without waiting: it is done on the audio thread, in
    /// order with every other request. `pcm_rate` is the rate ModemManager reports
    /// in the call's AudioFormat, or 0 when it reports none (see pcm_rate).
    pub fn request_start_call(&self, audio_port: &str, pcm_rate: u32) {
        let request = self.s.requests.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = self.jobs.send(Job::Start { port: audio_port.to_string(), rate: pcm_rate, request });
    }

    /// Stop a call's audio, without waiting, after any start asked for before it.
    pub fn request_stop_call(&self) {
        self.s.requests.fetch_add(1, Ordering::SeqCst);
        let _ = self.jobs.send(Job::Stop);
    }

    /// Restart a call's loopbacks if they have died (PipeWire restarted, a device
    /// went away under them). Cheap and does not wait: call it every few seconds
    /// during a call.
    pub fn check_call(&self) {
        let _ = self.jobs.send(Job::Check);
    }

    /// Stop a call's audio now, on this thread.
    pub fn stop_call(&self) {
        self.s.requests.fetch_add(1, Ordering::SeqCst);
        stop_call(&self.s);
    }

    #[cfg(test)]
    fn flush(&self) {
        let (tx, rx) = mpsc::channel();
        let _ = self.jobs.send(Job::Flush(tx));
        let _ = rx.recv_timeout(Duration::from_secs(10));
    }

    /// How to reach baresip for a SIP call's audio.
    pub fn set_sip_hook(&self, hook: impl Fn(SipAudio) + Send + Sync + 'static) {
        if let Some(mut i) = self.lock() {
            i.sip = Some(Arc::new(hook));
        }
    }

    pub fn set_muted(&self, muted: bool) {
        let Some(mut i) = self.lock() else { return };
        i.muted = muted;
        if i.port == SIP_PORT {
            if let Some(h) = i.sip.clone() {
                h(SipAudio::Mute(muted));
            }
            return;
        }
        if muted {
            kill(i.legs[1].take());
        } else if i.in_call && i.legs[1].is_none() {
            if let Some(args) = i.plan.get(1).cloned() {
                i.legs[1] = spawn_leg(&args, i.serial.as_deref(), false);
            }
        }
    }

    /// Ring until stopped. `mode` is the ringer setting; a waiting call gets a short
    /// beep every few seconds in the ear instead of the full ring.
    pub fn start_ringing(&self, mode: &str, waiting: bool) {
        if mode != "ring" {
            return;
        }
        let Some(ticket) = self.s.ring.begin() else { return };
        let Some(file) = tone_file(if waiting { Tone::Waiting } else { Tone::Ring }) else {
            self.s.ring.end(ticket);
            return;
        };
        let s = self.s.clone();
        std::thread::spawn(move || {
            // The full ring is for the room: the codec's speaker, switched on for it
            // (a profile whose jack route mutes the speaker would otherwise ring
            // silently with a headset in). The waiting beep goes where the call is.
            let nodes = if waiting { nodes() } else { Vec::new() };
            let (target, switched) = match s.inner.lock() {
                Ok(i) if waiting => (call_output(&i, &nodes), false),
                Ok(i) => {
                    let switch = !i.in_call;
                    if switch {
                        run_all(&i.cfg.speaker_commands);
                    }
                    (i.cfg.codec_sink.clone(), switch)
                }
                Err(_) => (String::new(), false),
            };
            ring(&s.ring, ticket, &file, &target, waiting);
            s.ring.end(ticket);
            // Back to where the codec plays outside a call, unless a call has
            // started meanwhile and put it where the call is.
            if switched {
                if let Ok(i) = s.inner.lock() {
                    if !i.in_call {
                        run_all(idle_commands(&i));
                    }
                }
            }
        });
    }

    pub fn stop_ringing(&self) {
        self.s.ring.stop();
    }

    /// Play the country's call-progress tone to the call's output (or, after the
    /// call, to where the route's output was). Ringback loops until stop_progress
    /// (or two minutes); busy and congestion play for about four seconds, as a phone
    /// does after the call is released.
    pub fn start_progress(&self, region: &str, p: Progress) {
        let looping = p == Progress::Ringback;
        let ticket = if looping {
            match self.s.progress.begin() {
                Some(t) => Some(t),
                None => return,
            }
        } else {
            None
        };
        let Some(file) = progress_file(region, p) else {
            if let Some(t) = ticket {
                self.s.progress.end(t);
            }
            return;
        };
        tracing::info!(tone = p.label(), region, "call-progress tone");
        let s = self.s.clone();
        std::thread::spawn(move || {
            let nodes = nodes();
            let target = s.inner.lock().map(|i| call_output(&i, &nodes)).unwrap_or_default();
            let live = || ticket.is_none_or(|t| s.progress.live(t));
            let started = Instant::now();
            loop {
                let Some(mut child) = pw_play("Communication", &target, &file) else { break };
                loop {
                    if !live() {
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                    if let Ok(Some(_)) = child.try_wait() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                if ticket.is_none() || !live() || started.elapsed() > Duration::from_secs(120) {
                    break;
                }
            }
            if let Some(t) = ticket {
                s.progress.end(t);
            }
        });
    }

    pub fn stop_progress(&self) {
        self.s.progress.stop();
    }

    /// Play a tone once, in the background. An alert plays whatever the ringer says;
    /// that is what an emergency alert is for.
    pub fn play(&self, tone: Tone) {
        let Some(file) = tone_file(tone) else { return };
        let role = if matches!(tone, Tone::Alert) { "Alarm" } else { "Notification" };
        std::thread::spawn(move || {
            if let Some(child) = pw_play(role, "", &file) {
                wait_within(child, Duration::from_secs(30), "pw-play");
            }
        });
    }

    /// Say something (a caller's name), when espeak-ng is installed, on the ringer's
    /// speaker. The words go on espeak's stdin, not its command line, which any
    /// user can read in /proc; the speech is deleted once played.
    pub fn speak(&self, words: &str) {
        static SPOKEN: AtomicU64 = AtomicU64::new(0);
        let words = words.to_string();
        let s = self.s.clone();
        std::thread::spawn(move || {
            let target = s.inner.lock().map(|i| i.cfg.codec_sink.clone()).unwrap_or_default();
            let Some(dir) = runtime_dir() else { return };
            let wav = dir.join(format!("speech-{}.wav", SPOKEN.fetch_add(1, Ordering::SeqCst)));
            let made = speak_to(&words, &wav);
            if made {
                if let Some(child) = pw_play("Notification", &target, &wav) {
                    wait_within(child, Duration::from_secs(30), "pw-play");
                }
            } else {
                tracing::debug!("espeak-ng is not installed; not speaking the caller");
            }
            let _ = std::fs::remove_file(&wav);
        });
    }
}

impl Default for Audio {
    fn default() -> Self {
        Self::new(Config::default())
    }
}

fn speak_to(words: &str, wav: &Path) -> bool {
    use std::io::Write;
    let child = Command::new("espeak-ng")
        .arg("--stdin")
        .arg("-w")
        .arg(wav)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else { return false };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(words.as_bytes());
        // Dropping stdin ends the text.
    }
    wait_within(child, Duration::from_secs(10), "espeak-ng")
}

/// Play the ring (or the waiting beep) until the run is stopped, the ringer's
/// limit passes or pw-play fails.
fn ring(looper: &Looper, ticket: u64, file: &Path, target: &str, waiting: bool) {
    // A ringer that is never stopped (a call phoned lost track of) must not
    // ring forever; networks give up on an unanswered call within a minute.
    let started = Instant::now();
    while looper.live(ticket) && started.elapsed() < Duration::from_secs(90) {
        let role = if waiting { "Communication" } else { "Notification" };
        let Some(mut child) = pw_play(role, target, file) else {
            tracing::warn!("pw-play is missing: ringing silently");
            return;
        };
        // Poll so a stop takes effect within a beat, not at the end of the tone.
        loop {
            if !looper.live(ticket) {
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
}

fn pw_play(role: &str, target: &str, file: &Path) -> Option<Child> {
    let mut cmd = Command::new("pw-play");
    cmd.arg(format!("--media-role={role}"));
    if !target.is_empty() {
        cmd.arg("--target").arg(target);
    }
    cmd.arg(file).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().ok()
}

/// Where tones for the ear go: the live call's output, or the route's output when
/// no call is up (a busy tone after the network turned a call away).
fn call_output(i: &Inner, nodes: &[Node]) -> String {
    if i.in_call && !i.output.is_empty() {
        i.output.clone()
    } else {
        endpoints(i.route, nodes, &i.cfg).0
    }
}

/// Where the codec plays outside a call: the jack when a headset is in it, the
/// speaker otherwise.
fn idle_commands(i: &Inner) -> &[Vec<String>] {
    if i.wired && !i.cfg.wired_commands.is_empty() {
        &i.cfg.wired_commands
    } else {
        &i.cfg.speaker_commands
    }
}

fn route_commands(cfg: &Config, route: Route) -> &[Vec<String>] {
    match route {
        Route::Speaker => &cfg.speaker_commands,
        Route::Wired => &cfg.wired_commands,
        Route::Bluetooth => &[],
        Route::Earpiece => &cfg.earpiece_commands,
    }
}

fn route_infos(i: &Inner, nodes: &[Node]) -> Vec<RouteInfo> {
    let bt = bluetooth_sink(nodes);
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

fn start_call(s: &Shared, audio_port: &str, rate: u32) {
    // Read the graph before taking the lock, so nothing waits on pw-dump.
    let dump = read_dump();
    let Ok(mut i) = s.inner.lock() else { return };
    let rate = pcm_rate(rate);
    if i.in_call {
        // The same call again (an outgoing call's early media, then its answer):
        // only bring back what has died. Another port: the call moved.
        if audio_port.is_empty() || (audio_port == i.port && rate == i.rate) {
            sync_legs(&mut i, dump);
            return;
        }
        kill_legs(&mut i);
    } else {
        i.in_call = true;
        for c in i.cfg.call_start_commands.clone() {
            run(&c);
        }
        // A phone with an earpiece starts a call there, as phones do, unless a
        // headset or the owner's choice says otherwise.
        if !i.cfg.earpiece_commands.is_empty() && i.route == Route::Speaker && !i.pinned {
            i.set_route(Route::Earpiece);
        }
        // The ringer may have switched the codec to the speaker: put it where the
        // call is.
        run_all(route_commands(&i.cfg, i.route));
    }
    i.port = audio_port.to_string();
    i.rate = rate;
    sync_legs(&mut i, dump);
}

fn stop_call(s: &Shared) {
    let Ok(mut i) = s.inner.lock() else { return };
    let was = i.in_call;
    if was {
        run_all(&i.cfg.call_stop_commands);
    }
    kill_legs(&mut i);
    restore_bluetooth(&mut i);
    i.in_call = false;
    i.pinned = false;
    i.muted = false;
    if was {
        // Leave the codec where it plays outside a call, so the next ring and
        // whatever else plays is heard.
        run_all(idle_commands(&i));
    }
}

fn check_call(s: &Shared) {
    let dead = {
        let Ok(mut i) = s.inner.lock() else { return };
        if !i.in_call || i.port == SIP_PORT {
            return;
        }
        let muted = i.muted;
        let planned = i.plan.len();
        (0..2).any(|leg| {
            if leg == 1 && muted {
                return false;
            }
            match i.legs[leg].as_mut() {
                Some(c) => !matches!(c.try_wait(), Ok(None)),
                // Nothing was started: no modem audio yet (PipeWire coming up), or
                // a spawn that failed.
                None => planned == 0 || leg < planned,
            }
        })
    };
    if !dead {
        return;
    }
    let dump = read_dump();
    let Ok(mut i) = s.inner.lock() else { return };
    if i.in_call {
        tracing::debug!("call audio is not running; starting it again");
        sync_legs(&mut i, dump);
    }
}

fn apply_route(i: &mut Inner, route: Route, dump: Option<Dump>) {
    run_all(route_commands(&i.cfg, route));
    let changed = i.route != route;
    i.set_route(route);
    if changed && i.in_call {
        sync_legs(i, dump.unwrap_or_else(read_dump));
    }
}

/// Bring the call's audio to what the route wants: switch a Bluetooth headset's
/// profile, aim the echo canceller, and start each leg that is not running what it
/// should. A leg whose ends did not change (the microphone, when only the output
/// moved) carries on untouched.
fn sync_legs(i: &mut Inner, dump: Dump) {
    if !i.in_call {
        return;
    }
    let route = i.route;
    let dump = if route == Route::Bluetooth {
        bluetooth_headset_profile(i, dump)
    } else {
        restore_bluetooth(i);
        dump
    };
    let (output, mic) = endpoints(route, &dump.nodes, &i.cfg);
    let cancelling = route == Route::Speaker && output == i.cfg.echo_cancel_sink;
    if cancelling && i.output != output {
        aim_echo_canceller(&dump, &i.cfg);
    }
    i.output = output.clone();
    let port = i.port.clone();
    if port == SIP_PORT {
        if let Some(h) = i.sip.clone() {
            h(SipAudio::Route(output, mic));
            if i.muted {
                h(SipAudio::Mute(true));
            }
        }
        return;
    }
    let (plan, tty) = if let Some(tty) = serial_port(&port, &i.cfg) {
        (serial_plan(route, &dump.nodes, &i.cfg, i.rate), Some(tty))
    } else if let Some(modem) = modem_nodes(&dump.nodes, &port, &i.cfg) {
        (call_plan(route, modem, &dump.nodes, &i.cfg), None)
    } else {
        if !i.warned_no_modem_audio {
            tracing::info!("no modem audio card: the call's audio stays inside the modem");
            i.warned_no_modem_audio = true;
        }
        kill_legs(i);
        return;
    };
    if tty != i.serial {
        kill_legs(i);
        if let Some(t) = &tty {
            // Raw bytes both ways: no echo, no newline translation.
            run(&["stty".into(), "-F".into(), t.clone(), "raw".into(), "-echo".into()]);
            tracing::info!(port = %t, rate = i.rate, "call audio over serial PCM");
        }
        i.serial = tty;
    }
    let mut restarted = false;
    for leg in 0..2 {
        let want = plan.get(leg);
        let same = i.plan.get(leg) == want;
        let alive = i.legs[leg].as_mut().is_some_and(|c| matches!(c.try_wait(), Ok(None)));
        if same && alive {
            continue;
        }
        restarted |= i.legs[leg].is_some();
        kill(i.legs[leg].take());
        if leg == 1 && i.muted {
            continue;
        }
        i.legs[leg] = want.and_then(|a| spawn_leg(a, i.serial.as_deref(), leg == 0));
    }
    i.plan = plan;
    // A leg that died may mean PipeWire restarted, and the canceller's streams
    // with it, with new ids and no target.
    if cancelling && restarted {
        aim_echo_canceller(&dump, &i.cfg);
    }
}

/// Put the call's Bluetooth headset in its hands-free profile, where its
/// microphone is, and wait for the microphone to appear. A headset in A2DP has a
/// sink only; the profile it was in comes back when the call ends.
///
/// Switched with `pw-cli set-param <device> Profile { index, save: false }`, so
/// WirePlumber does not remember hands-free as the headset's profile for next
/// time; WirePlumber's own autoswitch leaves a profile it did not set alone.
fn bluetooth_headset_profile(i: &mut Inner, dump: Dump) -> Dump {
    let Some(sink) = bluetooth_sink(&dump.nodes) else { return dump };
    let Some(dev) = dump.devices.iter().find(|d| Some(d.id) == sink.device) else { return dump };
    let Some(to) = headset_profile(dev) else { return dump };
    let (id, name, from) = (dev.id, dev.name.clone(), dev.profile);
    let to_name = dev.profiles.iter().find(|p| p.index == to).map(|p| p.name.clone()).unwrap_or_default();
    tracing::info!(device = %name, profile = %to_name, "Bluetooth headset to hands-free for the call");
    if !set_profile(id, to) {
        return dump;
    }
    if i.bt_restore.as_ref().is_none_or(|(n, _)| *n != name) {
        i.bt_restore = from.map(|f| (name.clone(), f));
    }
    // The A2DP nodes go and the hands-free ones come, typically within a second.
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        std::thread::sleep(Duration::from_millis(150));
        let now = read_dump();
        let ready = now.nodes.iter().any(|n| is_sink(n) && n.device == Some(id))
            && now.nodes.iter().any(|n| is_source(n) && n.device == Some(id) && !n.bt_loopback);
        if ready {
            return now;
        }
        if Instant::now() >= deadline {
            tracing::warn!(device = %name, "the headset's microphone did not appear");
            return now;
        }
    }
}

/// Put back the profile a Bluetooth headset was in before the call.
fn restore_bluetooth(i: &mut Inner) {
    let Some((name, index)) = i.bt_restore.take() else { return };
    let dump = read_dump();
    if let Some(dev) = dump.devices.iter().find(|d| d.name == name) {
        if dev.profile != Some(index) {
            set_profile(dev.id, index);
        }
    }
}

fn set_profile(device: u32, index: u32) -> bool {
    run(&[
        "pw-cli".into(),
        "set-param".into(),
        device.to_string(),
        "Profile".into(),
        format!("{{ index: {index}, save: false }}"),
    ])
}

/// Point the echo canceller's own streams at the codec: its capture at the
/// microphone and its playback at the speaker's sink. Left alone they follow the
/// default sink and source, which can be a Bluetooth headset or the modem's card.
/// Set as `target.object` in the default metadata, which WirePlumber honours for
/// any stream it may move (linking/find-defined-target.lua).
fn aim_echo_canceller(dump: &Dump, cfg: &Config) {
    let Some(ec) = dump.nodes.iter().find(|n| n.name == cfg.echo_cancel_sink) else { return };
    if ec.link_group.is_empty() {
        return;
    }
    for stream in dump.streams.iter().filter(|s| s.link_group == ec.link_group) {
        let target = if stream.class.starts_with("Stream/Input") {
            &cfg.mic_source
        } else if stream.class.starts_with("Stream/Output") {
            &cfg.codec_sink
        } else {
            continue;
        };
        if target.is_empty() || !dump.nodes.iter().any(|n| &n.name == target) {
            continue;
        }
        run(&[
            "pw-metadata".into(),
            "-n".into(),
            "default".into(),
            stream.id.to_string(),
            "target.object".into(),
            target.clone(),
        ]);
    }
}

fn kill(child: Option<Child>) {
    if let Some(mut c) = child {
        let _ = c.kill();
        let _ = c.wait();
    }
}

fn kill_legs(i: &mut Inner) {
    for leg in &mut i.legs {
        kill(leg.take());
    }
    i.plan.clear();
    i.serial = None;
}

fn spawn_leg(args: &[String], serial: Option<&str>, downlink: bool) -> Option<Child> {
    match serial {
        Some(tty) => spawn_serial(args, tty, downlink),
        None => spawn(args),
    }
}

fn spawn(args: &[String]) -> Option<Child> {
    let (cmd, rest) = args.split_first()?;
    match Command::new(cmd).args(rest).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
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
    use std::os::fd::AsRawFd;
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
    // Drop what is queued from before (a previous call, a leg that died): stale
    // bytes would play as noise, and an odd one would misalign every sample after.
    const TCIFLUSH: i32 = 0;
    const TCOFLUSH: i32 = 1;
    // SAFETY: the descriptor is open and ours for the duration of the call.
    unsafe { tcflush(file.as_raw_fd(), if downlink { TCIFLUSH } else { TCOFLUSH }) };
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

/// How long a profile's command (amixer, alsaucm, wpctl) may take. They run with
/// the audio state locked, so one that hangs must not hold up the call.
const COMMAND_LIMIT: Duration = Duration::from_secs(5);

fn run(args: &[String]) -> bool {
    let Some((cmd, rest)) = args.split_first() else { return false };
    let mut c = Command::new(cmd);
    c.args(rest).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    run_within(c, COMMAND_LIMIT)
}

fn run_all(commands: &[Vec<String>]) {
    for c in commands {
        run(c);
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
fn tone_file(tone: Tone) -> Option<PathBuf> {
    let dir = runtime_dir()?;
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
fn progress_file(region: &str, p: Progress) -> Option<PathBuf> {
    let region: String = region.chars().filter(char::is_ascii_alphabetic).take(2).collect();
    let dir = runtime_dir()?;
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
                // Some countries' tones alternate levels (Australia's congestion).
                let level = c.level(i / 2);
                out.push((s * 0.3 * level * edge * i16::MAX as f32) as i16);
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
        let n = parse_dump(DUMP).nodes;
        let mut cfg = Config::default();
        assert_eq!(serial_port("", &cfg), None, "no port, no profile: not serial");
        assert_eq!(serial_port("ttyUSB4", &cfg).as_deref(), Some("/dev/ttyUSB4"), "ModemManager's AudioPort");
        assert_eq!(serial_port("card2", &cfg), None);
        cfg.serial_pcm = "/dev/ttyUSB4".into();
        assert_eq!(serial_port("", &cfg).as_deref(), Some("/dev/ttyUSB4"), "the profile's port");
        assert_eq!(serial_port(SIP_PORT, &cfg), None, "never for a SIP call");
        let plan = serial_plan(Route::Speaker, &n, &cfg, 0);
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
        let n = parse_dump(DUMP).nodes;
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
        let n = parse_dump(DUMP).nodes;
        let cfg = Config::default();
        let m = modem_nodes(&n, "", &cfg).unwrap();
        let plan = call_plan(Route::Speaker, m, &n, &cfg);
        assert!(plan[0].contains(&"--playback=dolphone_ec_sink".to_string()));
        assert!(plan[1].contains(&"--capture=dolphone_ec_source".to_string()));
        assert!(plan[1].iter().any(|a| a.starts_with("--playback=alsa_output.usb-Quectel")));
    }

    #[test]
    fn bluetooth_plan_uses_headset_mic() {
        let n = parse_dump(DUMP).nodes;
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

    #[test]
    fn serial_rate_follows_modemmanager() {
        let n = parse_dump(DUMP).nodes;
        let cfg = Config::default();
        assert_eq!(serial_plan(Route::Speaker, &n, &cfg, 16_000)[0][4], "16000", "AT+CPCMFRM=1");
        assert_eq!(serial_plan(Route::Speaker, &n, &cfg, 0)[1][4], "8000", "nothing reported");
        assert_eq!(pcm_rate(44_100), 8_000, "not a rate the modem has");
        let plan = serial_plan(Route::Speaker, &n, &cfg, 8_000);
        assert!(plan[0].iter().any(|a| a.contains("node.dont-reconnect = true")), "a targeted leg stays put");
    }

    #[test]
    fn loopbacks_do_not_fall_back() {
        let n = parse_dump(DUMP).nodes;
        let cfg = Config::default();
        let m = modem_nodes(&n, "", &cfg).unwrap();
        let plan = call_plan(Route::Speaker, m, &n, &cfg);
        for leg in &plan {
            assert!(leg.iter().any(|a| a.starts_with("--capture-props=") && a.contains("node.dont-reconnect=true")));
            assert!(leg.iter().any(|a| a == &format!("--playback-props=media.role=Communication {STAY}")));
        }
        // The default microphone: nothing to stay on, so nothing is pinned.
        let cfg = Config { echo_cancel_sink: String::new(), ..Config::default() };
        let plan = call_plan(Route::Wired, m, &n, &cfg);
        assert!(!plan[1].iter().any(|a| a.starts_with("--capture")), "{:?}", plan[1]);
        assert!(!plan[0].iter().any(|a| a.starts_with("--playback=")));
        assert!(plan[0].contains(&"--playback-props=media.role=Communication".to_string()));
    }

    // A headset as PipeWire 1.x and WirePlumber 0.5 show it, in A2DP with
    // WirePlumber's stand-in source, and then in hands-free.
    fn headset(profile: u32) -> String {
        let hfp = profile == 2;
        let sco = if hfp {
            r#",{"id": 72, "type": "PipeWire:Interface:Node", "info": {"props": {"media.class": "Audio/Source", "node.name": "bluez_input_internal.AA_BB_CC_DD_EE_FF.0", "device.id": 70, "bluez5.loopback": false}}}"#
        } else {
            ""
        };
        format!(
            r#"[
          {{"id": 70, "type": "PipeWire:Interface:Device", "info": {{"props": {{"device.name": "bluez_card.AA_BB_CC_DD_EE_FF"}}, "params": {{
            "EnumProfile": [
              {{"index": 0, "name": "off", "priority": 0, "available": "yes"}},
              {{"index": 1, "name": "a2dp-sink", "priority": 40, "available": "yes"}},
              {{"index": 2, "name": "headset-head-unit", "priority": 30, "available": "yes"}},
              {{"index": 3, "name": "headset-head-unit-cvsd", "priority": 20, "available": "yes"}}],
            "Profile": [{{"index": {profile}, "name": "x"}}],
            "EnumRoute": [
              {{"index": 0, "direction": "Input", "profiles": [2, 3]}},
              {{"index": 1, "direction": "Output", "profiles": [1, 2, 3]}}]}}}}}},
          {{"id": 71, "type": "PipeWire:Interface:Node", "info": {{"props": {{"media.class": "Audio/Sink", "node.name": "bluez_output.AA_BB_CC_DD_EE_FF.1", "node.description": "Pixel Buds", "device.id": 70}}}}}},
          {{"id": 73, "type": "PipeWire:Interface:Node", "info": {{"props": {{"media.class": "Audio/Source", "node.name": "bluez_input.AA_BB_CC_DD_EE_FF", "device.id": 70, "bluez5.loopback": true}}}}}}
          {sco}
        ]"#
        )
    }

    #[test]
    fn bluetooth_call_switches_to_hands_free() {
        let a2dp = parse_dump(&headset(1));
        assert_eq!(a2dp.devices.len(), 1);
        assert_eq!(a2dp.devices[0].input_profiles, vec![2, 3]);
        assert_eq!(headset_profile(&a2dp.devices[0]), Some(2), "the best profile with a microphone");
        // Until it switches, only WirePlumber's stand-in is there.
        let (out, mic) = endpoints(Route::Bluetooth, &a2dp.nodes, &Config::default());
        assert_eq!(out, "bluez_output.AA_BB_CC_DD_EE_FF.1");
        assert_eq!(mic, "bluez_input.AA_BB_CC_DD_EE_FF");

        let hfp = parse_dump(&headset(2));
        assert_eq!(headset_profile(&hfp.devices[0]), None, "already hands-free");
        let (_, mic) = endpoints(Route::Bluetooth, &hfp.nodes, &Config::default());
        assert_eq!(mic, "bluez_input_internal.AA_BB_CC_DD_EE_FF.0", "the headset's own microphone");

        // Without routes in the dump, by name.
        let mut dev = a2dp.devices[0].clone();
        dev.input_profiles.clear();
        dev.profiles[2].available = false;
        assert_eq!(headset_profile(&dev), Some(3));
    }

    #[test]
    fn echo_canceller_streams_are_found() {
        let d = parse_dump(
            r#"[
          {"id": 36, "type": "PipeWire:Interface:Node", "info": {"props": {"media.class": "Stream/Input/Audio", "node.name": "dolphone_ec_capture", "node.link-group": "echo-cancel-1"}}},
          {"id": 38, "type": "PipeWire:Interface:Node", "info": {"props": {"media.class": "Audio/Sink", "node.name": "dolphone_ec_sink", "node.link-group": "echo-cancel-1"}}},
          {"id": 39, "type": "PipeWire:Interface:Node", "info": {"props": {"media.class": "Stream/Output/Audio", "node.name": "dolphone_ec_playback", "node.link-group": "echo-cancel-1"}}}
        ]"#,
        );
        assert_eq!(d.nodes.len(), 1, "streams are not sinks or sources");
        assert_eq!(d.streams.iter().map(|s| s.id).collect::<Vec<_>>(), vec![36, 39]);
        assert!(d.streams.iter().all(|s| s.link_group == d.nodes[0].link_group));
    }

    #[test]
    fn a_ring_that_ends_clears_only_itself() {
        let l = Looper::default();
        let first = l.begin().unwrap();
        assert!(l.begin().is_none(), "one at a time");
        l.end(first);
        assert!(!l.running(), "the 90 s cap or a pw-play failure leaves it free to ring again");
        let second = l.begin().unwrap();
        l.stop();
        let third = l.begin().unwrap();
        l.end(second);
        assert!(l.live(third), "an old ring's thread ending does not stop a new one");
    }

    #[test]
    fn call_audio_requests_run_in_order() {
        let audio = Audio::default();
        audio.request_start_call("", 0);
        audio.request_stop_call();
        audio.flush();
        assert!(!audio.lock().unwrap().in_call, "a stop asked for after a start wins");
        audio.request_start_call("", 0);
        audio.flush();
        assert!(audio.lock().unwrap().in_call);
        audio.request_stop_call();
        audio.request_start_call("", 0);
        audio.flush();
        assert!(audio.lock().unwrap().in_call, "and a start after a stop");
        audio.stop_call();
        assert!(!audio.lock().unwrap().in_call);
    }

    #[test]
    fn private_dir_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("dolphone");
        std::fs::create_dir(&d).unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(private_dir(&d).as_deref(), Some(d.as_path()));
        assert_eq!(std::fs::metadata(&d).unwrap().permissions().mode() & 0o777, 0o700);
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&d, &link).unwrap();
        assert_eq!(private_dir(&link), None, "not through a symlink");
    }
}
