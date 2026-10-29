//! Where Dolphone keeps the owner's data, for everything that needs to find it
//! (the apps read contacts from here without opening the store).

use std::path::PathBuf;

/// Where the store lives for the current user.
pub fn default_path() -> PathBuf {
    data_dir().join("dolphone.db")
}

/// `$XDG_DATA_HOME/dolphone`, or `~/.local/share/dolphone`.
pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("dolphone")
}

/// Contacts sit beside the store.
pub fn contacts_dir() -> PathBuf {
    data_dir().join("contacts")
}
