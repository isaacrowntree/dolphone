//! Putting Dolphone's front ends in front of the owner.
//!
//! On the panel that is `flipctl open <bundle>`, which starts the Dialer or brings
//! it to the front. On a desktop it is the desktop app, started from a notification.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Raise the Dialer on the panel. Repeated calls within a second are one call: a
/// ring and a confirmation arriving together should not open it twice.
pub fn dialer() {
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    if let Ok(mut last) = LAST.lock() {
        if last.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
            return;
        }
        *last = Some(Instant::now());
    }
    let Some(bundle) = find_bundle("dolphone-dialer") else {
        tracing::debug!("no Dialer bundle in ~/Apps");
        return;
    };
    std::thread::spawn(move || {
        // flipctl open: 0 on the panel, 1 refused, 2 nothing listening (flipctl
        // restarting). A ring must reach the screen, so try once more after a pause.
        for attempt in 0..2 {
            let run =
                Command::new("flipctl").arg("open").arg(&bundle).stdout(Stdio::null()).stderr(Stdio::null()).status();
            match run.map(|s| s.code()) {
                Ok(Some(0)) => return,
                Ok(code) if attempt == 0 => {
                    tracing::warn!(?code, "flipctl open did not bring the Dialer up; trying again");
                    std::thread::sleep(Duration::from_secs(2));
                }
                Ok(code) => tracing::warn!(?code, "flipctl open did not bring the Dialer up"),
                Err(e) => return tracing::debug!("flipctl open: {e}"),
            }
        }
    });
}

/// Start the desktop app with arguments, when it is installed.
pub fn desktop(args: &[&str]) {
    let spawned = Command::new("dolphone-desktop").args(args).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
    match spawned {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => tracing::debug!("dolphone-desktop: {e}"),
    }
}

/// `~/Apps/<stem>*.AppImage`, or one folder down (the Apps list groups by folder).
pub fn find_bundle(stem: &str) -> Option<PathBuf> {
    let apps = std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Apps"))?;
    search(&apps, stem, 1)
}

fn search(dir: &Path, stem: &str, depth: u32) -> Option<PathBuf> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir).ok()?.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    for p in &entries {
        let name = p.file_name()?.to_string_lossy();
        if p.is_file() && name.starts_with(stem) && name.ends_with(".AppImage") {
            return Some(p.clone());
        }
    }
    if depth > 0 {
        for p in entries.iter().filter(|p| p.is_dir()) {
            if let Some(found) = search(p, stem, depth - 1) {
                return Some(found);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn finds_in_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("Phone")).unwrap();
        std::fs::write(dir.path().join("Phone/dolphone-dialer-aarch64.fap.AppImage"), b"").unwrap();
        let found = super::search(dir.path(), "dolphone-dialer", 1).unwrap();
        assert!(found.ends_with("Phone/dolphone-dialer-aarch64.fap.AppImage"));
        assert!(super::search(dir.path(), "dolphone-dialer", 0).is_none());
    }
}
