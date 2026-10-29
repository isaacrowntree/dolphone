//! The few files the front ends share with each other.

use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Dolphone's own runtime directory: where the Dialer leaves a number for
/// Messages and the desktop app listens for a second start. Under
/// `$XDG_RUNTIME_DIR` when the session has one; otherwise under `~/.cache`, never
/// a shared /tmp, where anyone could plant the handoff file or the socket. Made
/// owner-only (0700) either way.
pub fn runtime_dir() -> PathBuf {
    let dir = match std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        Some(d) => PathBuf::from(d).join("dolphone"),
        None => std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(|h| PathBuf::from(h).join(".cache/dolphone"))
            // No home either: still somewhere only this user can read, by name.
            .unwrap_or_else(|| {
                let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
                std::env::temp_dir().join(format!("dolphone-{user}"))
            }),
    };
    let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir);
    // A directory left from before (made with the umask's mode) is tightened too.
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    dir
}

/// How a directory of files looks from outside: the newest change to it or to
/// anything in it, and how many entries it has. A stat each and no reads, so a
/// front end can notice files others drop in (the contacts) without parsing them
/// again every time it looks.
pub fn stamp(dir: &Path) -> Option<(SystemTime, usize)> {
    let mut newest = std::fs::metadata(dir).ok()?.modified().ok()?;
    let mut count = 0;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        count += 1;
        if let Some(t) = e.metadata().ok().and_then(|m| m.modified().ok()) {
            newest = newest.max(t);
        }
    }
    Some((newest, count))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stamp_changes_when_a_file_does() {
        let dir = std::env::temp_dir().join(format!("phone-bus-stamp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(stamp(&dir), None, "no directory, no stamp");
        std::fs::create_dir_all(&dir).unwrap();
        let empty = stamp(&dir).unwrap();
        std::fs::write(dir.join("a.vcf"), "x").unwrap();
        let one = stamp(&dir).unwrap();
        assert_ne!(empty, one, "a new file counts");
        assert_eq!(stamp(&dir), Some(one), "nothing changed, nothing to do");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
