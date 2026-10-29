//! Who is asking: the program behind a D-Bus caller.
//!
//! The daemon lets Dolphone's own front ends do everything, and everything else
//! only what does no harm: an outside program may ask to dial or text (and waits
//! for a press on the device), may hang up, and may read the modem's status. It may
//! not read messages or the call log, answer, route audio, pair Bluetooth or change
//! settings, unless the owner has granted it reading.
//!
//! A front end is recognised by the executable behind its connection, by full
//! path: `/usr/bin/dolphone-*` owned by root (the Debian package), or the same name
//! inside an AppImage's mount or extraction (the panel apps, from `~/Apps`). This is
//! a speed bump, not a wall: code running as the same user can still read the store
//! file, ptrace the apps or call ModemManager itself. What it stops is the common
//! case, a script or tool doing it quietly, and SECURITY.md says so.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use zbus::message::Header;

/// Whether front ends in an AppImage count (the panel apps run that way). An owner
/// who uses only the desktop app can turn this off, so only root-owned /usr/bin
/// front ends are trusted and a user-writable AppImage mount cannot pose as one.
static TRUST_APPIMAGES: AtomicBool = AtomicBool::new(true);

pub fn set_trust_appimages(on: bool) {
    TRUST_APPIMAGES.store(on, Ordering::SeqCst);
}

/// Our front ends, by executable name.
const FRONT_ENDS: &[&str] = &["dolphone-dialer", "dolphone-messages", "dolphone-settings", "dolphone-desktop"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caller {
    /// The executable's file name, or the bus name when it cannot be read.
    pub name: String,
    /// One of Dolphone's own front ends.
    pub trusted: bool,
    /// Granted reading (messages, the call log) without being a front end.
    pub reader: bool,
}

impl Caller {
    pub fn may_read(&self) -> bool {
        self.trusted || self.reader
    }
}

/// Whether `exe` is one of our front ends, installed where only the package or an
/// AppImage puts it.
pub fn is_front_end(exe: &Path) -> bool {
    front_end_with(exe, TRUST_APPIMAGES.load(Ordering::SeqCst))
}

/// The same, with the trust_appimages setting given rather than read.
fn front_end_with(exe: &Path, trust_appimages: bool) -> bool {
    let Some(name) = exe.file_name().and_then(|n| n.to_str()) else { return false };
    if !FRONT_ENDS.contains(&name) {
        return false;
    }
    let Some(dir) = exe.parent() else { return false };
    if !dir.ends_with("usr/bin") {
        return false;
    }
    let s = exe.to_string_lossy();
    if s.starts_with("/usr/bin/") {
        return root_owned(exe);
    }
    if !trust_appimages {
        return false;
    }
    // An AppImage runs from its squashfs, mounted at /tmp/.mount_<random>, or from a
    // directory it was extracted to when FUSE is missing.
    let in_mount = dir.ancestors().any(|a| a.file_name().is_some_and(|n| n.to_string_lossy().starts_with(".mount_")));
    let extracted = dir.ancestors().any(|a| {
        a.file_name().is_some_and(|n| {
            let n = n.to_string_lossy();
            n.starts_with("appimage_extracted_") || n == "squashfs-root"
        })
    });
    in_mount || extracted
}

/// A program installed with the system: under /usr and owned by root.
pub fn system_program(p: &Path) -> bool {
    p.starts_with("/usr") && root_owned(p)
}

#[cfg(unix)]
fn root_owned(p: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(p).map(|m| m.uid() == 0).unwrap_or(false)
}

#[cfg(not(unix))]
fn root_owned(_: &Path) -> bool {
    false
}

/// The program behind a bus name, or None.
pub async fn exe_of(conn: &zbus::Connection, name: &str) -> Option<std::path::PathBuf> {
    let dbus = zbus::fdo::DBusProxy::new(conn).await.ok()?;
    let bus = zbus::names::BusName::try_from(name).ok()?;
    let pid = dbus.get_connection_unix_process_id(bus).await.ok()?;
    std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}

pub async fn identify(conn: &zbus::Connection, header: &Header<'_>) -> Caller {
    let Some(sender) = header.sender() else {
        return Caller { name: "unknown".into(), trusted: false, reader: false };
    };
    let exe = exe_of(conn, sender.as_str()).await;
    let name = exe
        .as_deref()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| sender.to_string());
    let trusted = exe.as_deref().is_some_and(is_front_end) || test_trust_all();
    let reader = !trusted && readers().contains(&name);
    Caller { name, trusted, reader }
}

/// Programs the owner has allowed to read messages and the call log, by executable
/// name, from the `read_grants` setting (comma separated). Set from Phone Settings.
fn readers() -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    if let Ok(g) = std::env::var("DOLPHONE_READ_GRANTS") {
        // Tests only: a debug build reads grants from the environment as well.
        if cfg!(debug_assertions) {
            v.extend(g.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()));
        }
    }
    if let Some(stored) = GRANTS.get() {
        if let Ok(g) = stored.lock() {
            v.extend(g.iter().cloned());
        }
    }
    v
}

/// The stored grants, kept here so identify() need not open the store.
pub static GRANTS: std::sync::OnceLock<std::sync::Mutex<Vec<String>>> = std::sync::OnceLock::new();

pub fn set_grants(list: &str) {
    let parsed: Vec<String> = list.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    let cell = GRANTS.get_or_init(|| std::sync::Mutex::new(Vec::new()));
    if let Ok(mut g) = cell.lock() {
        *g = parsed;
    }
}

/// Tests and development only: trust every caller. Compiled out of release builds,
/// so a packaged phoned cannot be talked into it by its environment.
fn test_trust_all() -> bool {
    cfg!(debug_assertions) && std::env::var_os("DOLPHONE_TRUST_ALL").is_some()
}

#[cfg(test)]
mod tests {
    use super::is_front_end;
    use std::path::Path;

    #[test]
    fn front_ends_by_path() {
        assert!(is_front_end(Path::new("/tmp/.mount_dolphoXYZ/usr/bin/dolphone-dialer")));
        assert!(is_front_end(Path::new("/tmp/appimage_extracted_ab12/usr/bin/dolphone-messages")));
        assert!(!is_front_end(Path::new("/home/me/dolphone-dialer")), "a copy in home is not ours");
        assert!(!is_front_end(Path::new("/home/me/usr/bin/dolphone-dialer")));
        assert!(!is_front_end(Path::new("/tmp/.mount_x/usr/bin/python3")));
        // /usr/bin must be root-owned; a missing file is not.
        assert!(!is_front_end(Path::new("/usr/bin/dolphone-nonexistent")));
    }

    #[test]
    fn appimages_can_be_left_untrusted() {
        // The setting is given, not set: the global it lives in is shared with
        // the test above, which runs alongside.
        let mount = Path::new("/tmp/.mount_dolphoXYZ/usr/bin/dolphone-dialer");
        assert!(!super::front_end_with(mount, false), "with AppImages off, only /usr/bin counts");
        assert!(super::front_end_with(mount, true));
    }
}
