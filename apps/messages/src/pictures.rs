//! Pictures to attach, as the panel has no file chooser: the newest few in the
//! Pictures and Downloads folders.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// What phoned sends at most in one MMS, in all, and in how many files.
pub const MAX_BYTES: u64 = phone_api::MMS_MAX_BYTES;
pub const MAX_FILES: usize = phone_api::MMS_MAX_FILES;
/// How many the list shows.
pub const SHOWN: usize = 50;
/// Entries looked at in all, so a Downloads folder of thousands stays quick.
const LOOKED_AT: usize = 5000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    pub path: String,
    /// The file's name, as shown.
    pub name: String,
    pub size: u64,
    /// Last changed, in seconds since 1970.
    pub at: i64,
}

/// The kinds phoned names a type for (its mms::mime_of), and so mmsd-tng sends
/// as a picture.
fn is_picture(p: &Path) -> bool {
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    matches!(ext.as_str(), "jpg" | "jpeg" | "png" | "gif" | "webp")
}

/// The folders looked in: XDG's Pictures and Downloads (user-dirs.dirs), or
/// ~/Pictures and ~/Downloads.
pub fn folders() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return Vec::new() };
    let config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config"));
    let dirs = std::fs::read_to_string(config.join("user-dirs.dirs")).unwrap_or_default();
    let mut out = Vec::new();
    for (key, fallback) in [("XDG_PICTURES_DIR", "Pictures"), ("XDG_DOWNLOAD_DIR", "Downloads")] {
        let dir = user_dir(&dirs, key, &home).unwrap_or_else(|| home.join(fallback));
        if !out.contains(&dir) {
            out.push(dir);
        }
    }
    out
}

/// One folder from user-dirs.dirs: `XDG_PICTURES_DIR="$HOME/Pictures"`. One set
/// to the home folder itself is turned off, as xdg-user-dirs takes it.
fn user_dir(dirs: &str, key: &str, home: &Path) -> Option<PathBuf> {
    let line = dirs.lines().map(str::trim).find(|l| l.strip_prefix(key).is_some_and(|r| r.starts_with('=')))?;
    let value = line[key.len() + 1..].trim().trim_matches('"');
    let path = match value.strip_prefix("$HOME") {
        Some(rest) => home.join(rest.trim_start_matches('/')),
        None if value.starts_with('/') => PathBuf::from(value),
        None => return None,
    };
    (path != home && path.as_os_str() != "").then_some(path)
}

/// The pictures in these folders and the folders directly in them (Screenshots),
/// newest first. Links are not followed, so nothing outside them is offered; a
/// file phoned would refuse (too large, empty, unreadable) is left out.
pub fn list(folders: &[PathBuf]) -> Vec<Picture> {
    let mut found = Vec::new();
    let mut budget = LOOKED_AT;
    for dir in folders {
        scan(dir, 1, &mut found, &mut budget);
    }
    found.sort_by(|a: &Picture, b| b.at.cmp(&a.at).then_with(|| a.name.cmp(&b.name)));
    found.dedup_by(|a, b| a.path == b.path);
    let mut out = Vec::new();
    for p in found {
        if out.len() == SHOWN {
            break;
        }
        // Opened, not only listed: phoned reads it as this user.
        if std::fs::File::open(&p.path).is_ok() {
            out.push(p);
        }
    }
    out
}

fn scan(dir: &Path, depth: u8, found: &mut Vec<Picture>, budget: &mut usize) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut sub = Vec::new();
    for e in entries.flatten() {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        let path = e.path();
        if e.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        // The entry's own type, which is a link's, not what it points at.
        let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
        if meta.is_dir() {
            sub.push(path);
            continue;
        }
        if !meta.is_file() || !is_picture(&path) || meta.len() == 0 || meta.len() > MAX_BYTES {
            continue;
        }
        let Some(full) = path.to_str() else { continue };
        let at = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs() as i64);
        found.push(Picture { path: full.to_string(), name: display_name(&path), size: meta.len(), at });
    }
    if depth > 0 {
        for d in sub {
            scan(&d, depth - 1, found, budget);
        }
    }
}

/// A file's name for the panel: without the extension, underscores as spaces.
pub fn display_name(path: &Path) -> String {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let name = stem.replace('_', " ").split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    } else {
        name
    }
}

/// A size as the list shows it: "840 KB", "2.4 MB".
pub fn size(bytes: u64) -> String {
    if bytes < 1024 * 1024 {
        format!("{} KB", bytes.div_ceil(1024))
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_dirs_are_read_as_xdg_writes_them() {
        let home = Path::new("/home/me");
        let dirs = "# written by xdg-user-dirs-update\nXDG_PICTURES_DIR=\"$HOME/Bilder\"\nXDG_DOWNLOAD_DIR=\"$HOME\"\nXDG_MUSIC_DIR=\"/srv/music\"\n";
        assert_eq!(user_dir(dirs, "XDG_PICTURES_DIR", home), Some(PathBuf::from("/home/me/Bilder")));
        assert_eq!(user_dir(dirs, "XDG_DOWNLOAD_DIR", home), None, "the home folder itself is turned off");
        assert_eq!(user_dir(dirs, "XDG_MUSIC_DIR", home), Some(PathBuf::from("/srv/music")));
        assert_eq!(user_dir(dirs, "XDG_VIDEOS_DIR", home), None);
    }

    #[test]
    fn names_and_sizes() {
        assert_eq!(display_name(Path::new("/p/IMG_2026_10_29.jpg")), "IMG 2026 10 29");
        assert_eq!(display_name(Path::new("/p/Beach day.JPEG")), "Beach day");
        assert_eq!(display_name(Path::new("/p/_.png")), "_.png");
        assert_eq!(size(1000), "1 KB");
        assert_eq!(size(860_000), "840 KB");
        assert_eq!(size(2_500_000), "2.4 MB");
    }

    #[test]
    fn newest_pictures_only_and_no_links_out() {
        let root = std::env::temp_dir().join(format!("dolphone-pictures-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let pics = root.join("Pictures");
        let outside = root.join("outside");
        std::fs::create_dir_all(pics.join("Screenshots")).unwrap();
        std::fs::create_dir_all(pics.join("a/b")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let write = |p: &Path, n: usize, age: u64| {
            std::fs::write(p, vec![1u8; n]).unwrap();
            let t = std::time::SystemTime::now() - std::time::Duration::from_secs(age);
            std::fs::File::options().write(true).open(p).unwrap().set_modified(t).unwrap();
        };
        write(&pics.join("old.jpg"), 10, 3000);
        write(&pics.join("new.PNG"), 10, 10);
        write(&pics.join("Screenshots/shot.png"), 10, 100);
        write(&pics.join("a/b/too-deep.jpg"), 10, 1);
        write(&pics.join("notes.txt"), 10, 1);
        write(&pics.join("empty.gif"), 0, 1);
        write(&pics.join(".hidden.jpg"), 10, 1);
        write(&outside.join("secret.jpg"), 10, 1);
        std::os::unix::fs::symlink(outside.join("secret.jpg"), pics.join("link.jpg")).unwrap();
        std::os::unix::fs::symlink(&outside, pics.join("linked-dir")).unwrap();
        let big = std::fs::File::create(pics.join("huge.jpg")).unwrap();
        big.set_len(MAX_BYTES + 1).unwrap();
        let names: Vec<String> = list(std::slice::from_ref(&pics)).into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["new", "shot", "old"]);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
