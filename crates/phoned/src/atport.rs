//! AT commands on a port of phoned's own, without ModemManager's debug mode.
//!
//! ModemManager passes a client's AT commands (Modem.Command) only when it runs
//! with `--debug` (handle_command_auth_ready in src/mm-iface-modem.c). The other
//! way is a spare AT port that ModemManager is told to leave alone. The udev
//! property `ID_MM_PORT_IGNORE` does that: "a port-specific tag that allows
//! explicitly ignoring a given port in a device", for every port type, since 1.10
//! (include/ModemManager-tags.h). It wins over a plugin's port-type hints such as
//! `ID_MM_PORT_TYPE_AT_SECONDARY`: mm-port-probe.c loads the hints, then
//! "regardless of the type, the port may be ignored". The hints do not need
//! changing. dist/udev/70-dolphone-at-port.rules sets it, once the owner has run
//! `sudo dolphone-modem-access port on`.
//!
//! Which port: the device profile's `[at_port]`, found in sysfs by USB vendor,
//! product and interface number. For the RM520N-GL (2c7c:0801) Quectel's UMTS/LTE/5G
//! Linux USB Driver User Guide V3.1 (Table 1) and ModemManager's
//! 77-mm-quectel-port-types.rules agree: DIAG on interface 0, GNSS on 1, AT on 2
//! (ModemManager's primary port) and the modem port on 3, which takes AT commands
//! too (ModemManager's secondary). The profiles take 3, so ModemManager keeps its
//! primary port. Which port the modem sends unsolicited messages to
//! (`AT+QURCCFG="urcport"`) is not in any manual found for the RM520N, and none of
//! this has met a real one yet.
//!
//! phoned uses a profile's port only when udev says ModemManager ignores it: two
//! programs on one AT port garble each other's answers. The `at_port` setting can
//! name a port outright (`/dev/...`), or say `modemmanager` to never use one.

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use crate::devices::AtPort;

/// How long a command may take, as phoned gives ModemManager's Command.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Where AT commands go.
#[derive(Clone, Debug, PartialEq)]
pub enum Route {
    /// phoned's own port, and how it was found.
    Direct(String, String),
    /// ModemManager's Command, and why not a port of phoned's own.
    ModemManager(String),
}

impl Route {
    /// In words, for `phonectl modem setup`. (Never "change": a setup that has
    /// nothing to do says nothing of the kind.)
    pub fn describe(&self) -> String {
        match self {
            Route::Direct(path, why) => format!("on phoned's own port {path} ({why})"),
            Route::ModemManager(why) => format!("through ModemManager, which needs its debug mode ({why})"),
        }
    }
}

#[derive(Default)]
struct State {
    setting: String,
    profile: AtPort,
    open: Option<(String, File)>,
    last: Option<Route>,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(Default::default);

/// What the owner's `at_port` setting and the device profile say. Called when
/// either changes; the port is looked for at each command, since the modem's tty
/// comes back under another name after a restart.
pub fn configure(setting: &str, profile: &AtPort) {
    if let Ok(mut s) = STATE.lock() {
        s.setting = setting.trim().to_string();
        s.profile = profile.clone();
        s.open = None;
    }
}

/// Whether `value` is something the `at_port` setting takes.
pub fn valid_setting(value: &str) -> bool {
    matches!(value, "auto" | "modemmanager") || (value.starts_with("/dev/") && !value.contains(".."))
}

/// Where AT commands would go now.
pub async fn route() -> Route {
    tokio::task::spawn_blocking(|| match STATE.lock() {
        Ok(s) => find_route(&s.setting, &s.profile, Path::new("/sys")),
        Err(_) => Route::ModemManager("phoned's AT port is unusable".into()),
    })
    .await
    .unwrap_or_else(|_| Route::ModemManager("phoned's AT port is unusable".into()))
}

/// Send one command on phoned's own port, if there is one: None means it goes
/// through ModemManager. The answer is as ModemManager gives it, without the final
/// OK; ERROR and the +CME and +CMS errors are errors.
pub async fn command(cmd: &str) -> Option<Result<String, String>> {
    let cmd = cmd.to_string();
    tokio::task::spawn_blocking(move || {
        let mut s = STATE.lock().ok()?;
        let route = find_route(&s.setting, &s.profile, Path::new("/sys"));
        if s.last.as_ref() != Some(&route) {
            tracing::info!(route = %route.describe(), "AT commands");
            s.last = Some(route.clone());
        }
        let Route::Direct(path, _) = route else { return None };
        // The lock is held to the end, so commands go one at a time.
        Some(on_port(&mut s, &path, &cmd))
    })
    .await
    .unwrap_or_else(|_| Some(Err("phoned's AT port is unusable".into())))
}

fn on_port(s: &mut State, path: &str, cmd: &str) -> Result<String, String> {
    for attempt in 0..2 {
        if s.open.as_ref().is_none_or(|(p, _)| p != path) {
            s.open = None;
            let f = open(path).map_err(|e| {
                format!("the modem's AT port {path} cannot be opened: {e} (is the owner in the dolphone group?)")
            })?;
            s.open = Some((path.to_string(), f));
        }
        let Some((_, f)) = s.open.as_mut() else { continue };
        match exchange(f, cmd, TIMEOUT) {
            Ok(answer) => return answer.map_err(|e| format!("the modem answered {e}")),
            // A modem that restarted leaves a dead descriptor: open it again, once.
            Err(e) => {
                s.open = None;
                if attempt == 1 || e.kind() == std::io::ErrorKind::TimedOut {
                    return Err(format!("no answer on the modem's AT port {path}: {e}"));
                }
            }
        }
    }
    Err(format!("no answer on the modem's AT port {path}"))
}

fn find_route(setting: &str, profile: &AtPort, sys: &Path) -> Route {
    match setting {
        "modemmanager" => return Route::ModemManager("the at_port setting says so".into()),
        p if p.starts_with('/') => return Route::Direct(p.to_string(), "the at_port setting names it".into()),
        _ => {}
    }
    if profile.usb_vendor.is_empty() {
        return Route::ModemManager("the device profile names no spare AT port".into());
    }
    let Some(dev) = find(sys, profile) else {
        return Route::ModemManager(format!(
            "the profile's AT port, USB {}:{} interface {}, is not here",
            profile.usb_vendor,
            profile.usb_products.join("/"),
            profile.usb_interface
        ));
    };
    let props = std::process::Command::new("udevadm")
        .args(["info", "--query=property", "--name", &dev])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    if ignored(&props) {
        Route::Direct(dev, "ModemManager leaves it alone".into())
    } else {
        Route::ModemManager(format!("{dev} is ModemManager's; sudo dolphone-modem-access port on gives it to phoned"))
    }
}

/// Whether udev's properties for a port tell ModemManager to ignore it. Its
/// booleans are "1", "true" or "yes" (mm_common_get_boolean_from_string).
fn ignored(props: &str) -> bool {
    props.lines().filter_map(|l| l.trim().strip_prefix("ID_MM_PORT_IGNORE=")).any(|v| {
        let v = v.trim().to_ascii_lowercase();
        matches!(v.as_str(), "1" | "true" | "yes")
    })
}

/// The tty a profile's `[at_port]` names, as /dev/<name>: the one whose USB
/// interface has that number, on a device with that vendor and one of those
/// products.
fn find(sys: &Path, want: &AtPort) -> Option<String> {
    let read = |p: PathBuf| std::fs::read_to_string(p).ok().map(|s| s.trim().to_ascii_lowercase());
    let mut names: Vec<String> = std::fs::read_dir(sys.join("class/tty"))
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect();
    names.sort();
    let iface_want = want.usb_interface.trim().to_ascii_lowercase();
    names.into_iter().find_map(|name| {
        let dev = std::fs::canonicalize(sys.join("class/tty").join(&name).join("device")).ok()?;
        // The interface is the device itself (cdc-acm) or above it (the option
        // driver's ttyUSB port).
        let iface = dev.ancestors().find(|a| a.join("bInterfaceNumber").exists())?;
        let usb = iface.parent()?;
        let ok = read(iface.join("bInterfaceNumber"))? == iface_want
            && read(usb.join("idVendor"))? == want.usb_vendor.to_ascii_lowercase()
            && want
                .usb_products
                .iter()
                .any(|p| read(usb.join("idProduct")).as_deref() == Some(&p.to_ascii_lowercase()));
        ok.then(|| format!("/dev/{name}"))
    })
}

/// Open a port raw at 115200 baud (a USB modem ignores the rate), with reads that
/// give up after 0.1 s so a command can time out, and turn echo off.
fn open(path: &str) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    // O_NOCTTY: never phoned's controlling terminal. O_NONBLOCK only for the
    // open, which can otherwise wait for a carrier.
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
        .open(path)?;
    let fd = f.as_raw_fd();
    let fail = || Err(std::io::Error::last_os_error());
    // SAFETY: termios calls on a descriptor this function owns.
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(fd, &mut t) != 0 {
            return fail();
        }
        libc::cfmakeraw(&mut t);
        t.c_cflag |= libc::CLOCAL | libc::CREAD;
        t.c_cc[libc::VMIN] = 0;
        t.c_cc[libc::VTIME] = 1;
        libc::cfsetispeed(&mut t, libc::B115200);
        libc::cfsetospeed(&mut t, libc::B115200);
        if libc::tcsetattr(fd, libc::TCSANOW, &t) != 0 {
            return fail();
        }
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) != 0 {
            return fail();
        }
    }
    let mut f = f;
    // Echo off, so answers are only answers; its own answer does not matter.
    let _ = exchange(&mut f, "ATE0", Duration::from_secs(2))?;
    Ok(f)
}

/// Write a command and read until its final result code. Err is the port failing
/// (or no answer in time); Ok(Err) is the modem refusing.
fn exchange(f: &mut File, cmd: &str, timeout: Duration) -> std::io::Result<Result<String, String>> {
    // What came before (unsolicited messages nobody reads here) is not the answer.
    // SAFETY: tcflush on an open descriptor.
    unsafe { libc::tcflush(f.as_raw_fd(), libc::TCIFLUSH) };
    f.write_all(format!("{cmd}\r").as_bytes())?;
    let mut answer = Answer::new(cmd);
    let end = Instant::now() + timeout;
    let mut buf = [0u8; 512];
    while Instant::now() < end {
        let n = f.read(&mut buf)?;
        if let Some(done) = answer.feed(&buf[..n]) {
            return Ok(done);
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::TimedOut, format!("{cmd} had no answer in {}s", timeout.as_secs())))
}

/// Unsolicited messages a modem may send in the middle of an answer, by prefix
/// (3GPP TS 27.007 and 27.005, and Quectel's own). A line that starts with the
/// command's own name is the answer, even if it looks like one of these.
const URCS: &[&str] = &[
    "RING",
    "NO CARRIER",
    "RDY",
    "POWERED DOWN",
    "+CRING:",
    "+CLIP:",
    "+CCWA:",
    "+CLCC:",
    "+CMTI:",
    "+CMT:",
    "+CDS:",
    "+CDSI:",
    "+CBM:",
    "+CBMI:",
    "+CUSD:",
    "+CREG:",
    "+CGREG:",
    "+CEREG:",
    "+C5GREG:",
    "+CGEV:",
    "+CIEV:",
    "+CTZV:",
    "+CTZE:",
    "+CPIN:",
    "+QIND:",
    "+QUSIM:",
    "+QIURC:",
    "+QNETDEVSTATUS:",
    "+QSIMSTAT:",
    "+QGPSURC:",
    "^",
];

/// One command's answer, read as it comes.
pub struct Answer {
    cmd: String,
    /// The command's own name (`+QCFG`), whose lines are always the answer.
    own: Option<String>,
    partial: Vec<u8>,
    lines: Vec<String>,
}

impl Answer {
    pub fn new(cmd: &str) -> Self {
        let cmd = cmd.trim().to_string();
        let upper = cmd.to_ascii_uppercase();
        let own = upper
            .strip_prefix("AT")
            .filter(|r| r.starts_with(['+', '^', '$']))
            .map(|r| r.split(['=', '?']).next().unwrap_or(r).to_string());
        Self { cmd, own, partial: Vec::new(), lines: Vec::new() }
    }

    /// Take bytes from the port; Some once the final result code has come.
    pub fn feed(&mut self, bytes: &[u8]) -> Option<Result<String, String>> {
        self.partial.extend_from_slice(bytes);
        while let Some(i) = self.partial.iter().position(|b| *b == b'\r' || *b == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=i).collect();
            let line = String::from_utf8_lossy(&line[..line.len() - 1]).trim().to_string();
            if let Some(done) = self.line(line) {
                return Some(done);
            }
        }
        None
    }

    fn line(&mut self, line: String) -> Option<Result<String, String>> {
        if line.is_empty() || line.eq_ignore_ascii_case(&self.cmd) {
            return None;
        }
        let upper = line.to_ascii_uppercase();
        if upper == "OK" {
            return Some(Ok(self.lines.join("\n")));
        }
        if upper == "ERROR" || upper.starts_with("+CME ERROR") || upper.starts_with("+CMS ERROR") {
            return Some(Err(line));
        }
        let own = self.own.as_deref().is_some_and(|o| upper.starts_with(o));
        if !own && URCS.iter().any(|u| upper.starts_with(u)) {
            tracing::debug!(urc = %line, "unsolicited, on the AT port");
            return None;
        }
        self.lines.push(line);
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(cmd: &str, wire: &str) -> Option<Result<String, String>> {
        Answer::new(cmd).feed(wire.as_bytes())
    }

    #[test]
    fn answers() {
        assert_eq!(
            answer("AT+QCFG=\"ims\"", "\r\n+QCFG: \"ims\",1,1\r\n\r\nOK\r\n"),
            Some(Ok("+QCFG: \"ims\",1,1".into()))
        );
        assert_eq!(answer("AT+QPCMV=1,2", "\r\nOK\r\n"), Some(Ok(String::new())));
        assert_eq!(answer("AT+QFOO", "\r\nERROR\r\n"), Some(Err("ERROR".into())));
        assert_eq!(answer("AT+CCFC=0,2", "\r\n+CME ERROR: 30\r\n"), Some(Err("+CME ERROR: 30".into())));
        assert_eq!(answer("AT+CMGS=1", "+CMS ERROR: 500\r\n"), Some(Err("+CMS ERROR: 500".into())));
        assert_eq!(answer("AT+CLIR?", "\r\n+CLIR: 0,4\r\n"), None, "not finished without a result code");
        let multi = "\r\n+CCFC: 1,1,\"0412\",129\r\n+CCFC: 1,4,\"0412\",129\r\n\r\nOK\r\n";
        assert_eq!(answer("AT+CCFC=4,2", multi), Some(Ok("+CCFC: 1,1,\"0412\",129\n+CCFC: 1,4,\"0412\",129".into())));
    }

    #[test]
    fn echo_and_unsolicited_messages_are_not_the_answer() {
        let wire =
            "AT+QCFG=\"ims\"\r\r\n+QIND: \"csq\",20,99\r\n\r\nRING\r\n+QCFG: \"ims\",0,1\r\n+CMTI: \"ME\",3\r\nOK\r\n";
        assert_eq!(answer("AT+QCFG=\"ims\"", wire), Some(Ok("+QCFG: \"ims\",0,1".into())));
        // A query of something that is also an unsolicited message keeps its answer.
        assert_eq!(answer("AT+CREG?", "\r\n+CREG: 0,1\r\n\r\nOK\r\n"), Some(Ok("+CREG: 0,1".into())));
        assert_eq!(answer("AT+CPIN?", "+CREG: 1\r\n+CPIN: READY\r\nOK\r\n"), Some(Ok("+CPIN: READY".into())));
    }

    #[test]
    fn answers_split_across_reads() {
        let mut a = Answer::new("AT+QMBNCFG=\"AutoSel\"");
        assert_eq!(a.feed(b"\r\n+QMBNCFG: \"Auto"), None);
        assert_eq!(a.feed(b"Sel\",1\r\n\r\nO"), None);
        assert_eq!(a.feed(b"K\r\n"), Some(Ok("+QMBNCFG: \"AutoSel\",1".into())));
    }

    #[test]
    fn udev_properties() {
        assert!(ignored("DEVNAME=/dev/ttyUSB3\nID_MM_CANDIDATE=1\nID_MM_PORT_IGNORE=1\n"));
        assert!(ignored("ID_MM_PORT_IGNORE=true"));
        assert!(!ignored("ID_MM_CANDIDATE=1\nID_MM_PORT_TYPE_AT_SECONDARY=1\n"));
        assert!(!ignored("ID_MM_PORT_IGNORE=0"));
        assert!(valid_setting("auto") && valid_setting("modemmanager") && valid_setting("/dev/ttyUSB3"));
        assert!(!valid_setting("ttyUSB3") && !valid_setting("/dev/../etc/passwd") && !valid_setting(""));
    }

    /// A sysfs tree as the option driver makes it: /sys/class/tty/ttyUSBn/device
    /// points at the port, under its USB interface, under the USB device.
    #[test]
    fn finds_the_port_in_sysfs() {
        let sys = tempfile::tempdir().unwrap();
        let usb = sys.path().join("devices/platform/usb1/1-1");
        let w = |p: PathBuf, s: &str| {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, s).unwrap();
        };
        w(usb.join("idVendor"), "2c7c\n");
        w(usb.join("idProduct"), "0801\n");
        std::fs::create_dir_all(sys.path().join("class/tty")).unwrap();
        for i in 0..4 {
            let iface = usb.join(format!("1-1:1.{i}"));
            w(iface.join("bInterfaceNumber"), &format!("0{i}\n"));
            let port = iface.join(format!("ttyUSB{i}"));
            std::fs::create_dir_all(&port).unwrap();
            let class = sys.path().join(format!("class/tty/ttyUSB{i}"));
            std::fs::create_dir_all(&class).unwrap();
            std::os::unix::fs::symlink(&port, class.join("device")).unwrap();
        }
        // A console with no device, as most ttys are.
        std::fs::create_dir_all(sys.path().join("class/tty/tty0")).unwrap();
        let rm520 = AtPort { usb_vendor: "2c7c".into(), usb_products: vec!["0801".into()], usb_interface: "03".into() };
        assert_eq!(find(sys.path(), &rm520).as_deref(), Some("/dev/ttyUSB3"));
        let other = AtPort { usb_products: vec!["0125".into()], ..rm520.clone() };
        assert_eq!(find(sys.path(), &other), None);
        assert!(matches!(find_route("", &other, sys.path()), Route::ModemManager(w) if w.contains("is not here")));
        assert!(matches!(find_route("auto", &AtPort::default(), sys.path()), Route::ModemManager(_)));
        assert_eq!(
            find_route("/dev/pts/9", &rm520, sys.path()),
            Route::Direct("/dev/pts/9".into(), "the at_port setting names it".into())
        );
        assert!(matches!(find_route("modemmanager", &rm520, sys.path()), Route::ModemManager(_)));
    }

    /// The whole exchange over a pty, with a responder that echoes until ATE0 and
    /// sends an unsolicited message before each answer.
    #[test]
    fn talks_to_a_pty() {
        use std::os::fd::FromRawFd;
        // SAFETY: plain libc calls on descriptors this test owns.
        let (mut master, slave) = unsafe {
            let fd = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
            assert!(fd >= 0 && libc::grantpt(fd) == 0 && libc::unlockpt(fd) == 0);
            let mut name = [0 as libc::c_char; 64];
            assert_eq!(libc::ptsname_r(fd, name.as_mut_ptr(), name.len()), 0);
            let slave = std::ffi::CStr::from_ptr(name.as_ptr()).to_string_lossy().into_owned();
            (File::from_raw_fd(fd), slave)
        };
        let responder = std::thread::spawn(move || {
            let mut echo = true;
            let mut line = Vec::new();
            let mut buf = [0u8; 256];
            let mut seen = Vec::new();
            while seen.len() < 3 {
                let n = master.read(&mut buf).unwrap_or(0);
                for &b in &buf[..n] {
                    if b != b'\r' {
                        line.push(b);
                        continue;
                    }
                    let cmd = String::from_utf8_lossy(&line).to_string();
                    line.clear();
                    let mut out = String::new();
                    if echo {
                        out.push_str(&cmd);
                        out.push('\r');
                    }
                    out.push_str("\r\n+QIND: \"csq\",20,99\r\n");
                    match cmd.as_str() {
                        "ATE0" => echo = false,
                        "AT+QCFG=\"ims\"" => out.push_str("\r\n+QCFG: \"ims\",1,1\r\n"),
                        _ => {}
                    }
                    out.push_str(if cmd == "AT+NOPE" { "\r\nERROR\r\n" } else { "\r\nOK\r\n" });
                    master.write_all(out.as_bytes()).unwrap();
                    seen.push(cmd);
                }
            }
            seen
        });
        let mut s = State::default();
        assert_eq!(on_port(&mut s, &slave, "AT+QCFG=\"ims\""), Ok("+QCFG: \"ims\",1,1".into()));
        assert_eq!(on_port(&mut s, &slave, "AT+NOPE"), Err("the modem answered ERROR".into()));
        assert_eq!(responder.join().unwrap(), ["ATE0", "AT+QCFG=\"ims\"", "AT+NOPE"]);
    }
}
