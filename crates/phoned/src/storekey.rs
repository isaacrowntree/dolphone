//! The key of an encrypted message store, sealed by systemd for this user on this
//! machine.
//!
//! `phoned --encrypt-store` makes a random 32-byte key, seals it with
//! `systemd-creds --user encrypt` into `~/.config/dolphone/store-key.cred`, and
//! rewrites the store with SQLCipher under it. At every start phoned unseals it with
//! `systemd-creds --user decrypt`, which an unprivileged user may run (systemd 256 and
//! later, through its io.systemd.Credentials service).
//!
//! What sealing protects against (systemd-creds(1)): the key is bound to the user's
//! UID and name and the machine's ID, and encrypted with the machine's credential
//! secret, which only root can read (`/var/lib/systemd/credential.secret`), and with
//! the TPM2 chip when there is one. So a copy of the home directory, a backup, or
//! another account on the machine cannot read the messages. Someone with the whole
//! disk can, unless a TPM holds the secret or the disk itself is encrypted: full-disk
//! encryption is still the stronger layer.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

const NAME: &str = "dolphone-store-key";

/// The store is encrypted but its key could not be had: phoned runs on a store in
/// memory, and keeps nothing it cannot keep (texts stay on the SIM, MMS in mmsd-tng).
static LOCKED: AtomicBool = AtomicBool::new(false);

pub fn locked() -> bool {
    LOCKED.load(Ordering::Relaxed)
}

pub fn set_locked() {
    LOCKED.store(true, Ordering::Relaxed);
}

pub fn path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("dolphone/store-key.cred")
}

/// The key, unsealed.
pub fn load() -> anyhow::Result<Vec<u8>> {
    let out = Command::new("systemd-creds")
        .args(["--user", "decrypt", &format!("--name={NAME}")])
        .arg(path())
        .arg("-")
        .stderr(Stdio::piped())
        .output()?;
    anyhow::ensure!(out.status.success(), "systemd-creds: {}", String::from_utf8_lossy(&out.stderr).trim());
    anyhow::ensure!(out.stdout.len() == 32, "the sealed key is not 32 bytes");
    Ok(out.stdout)
}

/// A new random key, sealed to `path()`.
pub fn create() -> anyhow::Result<Vec<u8>> {
    use std::io::{Read, Write};
    let mut key = vec![0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut key)?;
    if let Some(dir) = path().parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut child = Command::new("systemd-creds")
        .args(["--user", "encrypt", &format!("--name={NAME}"), "-"])
        .arg(path())
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child.stdin.take().expect("stdin").write_all(&key)?;
    let out = child.wait_with_output()?;
    anyhow::ensure!(out.status.success(), "systemd-creds: {}", String::from_utf8_lossy(&out.stderr).trim());
    Ok(key)
}

/// `phoned --encrypt-store` and `--decrypt-store`, run by the owner with phoned
/// stopped.
pub fn convert(store: &std::path::Path, encrypt: bool) -> anyhow::Result<()> {
    let encrypted = phone_core::store::is_encrypted(store);
    if encrypt {
        anyhow::ensure!(!encrypted, "{} is already encrypted", store.display());
        let key = create()?;
        if let Err(e) = phone_core::store::rekey_file(store, None, Some(&key)) {
            let _ = std::fs::remove_file(path());
            anyhow::bail!("encrypting the store: {e}");
        }
        // The pictures go into the encrypted store with the texts.
        let s = phone_core::store::Store::open_keyed(store, Some(&key))?;
        let moved = crate::phone::move_parts(&s, true)?;
        println!("encrypted {}, with {moved} MMS parts; its key is sealed in {}", store.display(), path().display());
    } else {
        anyhow::ensure!(encrypted, "{} is not encrypted", store.display());
        let key = load()?;
        phone_core::store::rekey_file(store, Some(&key), None)?;
        let s = phone_core::store::Store::open(store)?;
        let moved = crate::phone::move_parts(&s, false)?;
        std::fs::remove_file(path())?;
        println!("decrypted {}, with {moved} MMS parts back as files", store.display());
    }
    Ok(())
}
