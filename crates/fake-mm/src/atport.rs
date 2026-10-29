//! A spare AT port, as a pty: the modem's port that ModemManager has been told to
//! ignore (ID_MM_PORT_IGNORE), which phoned talks to directly. It answers from the
//! same state as Modem.Command but does not care about debug mode, as a real port
//! would not. Like a modem it echoes until ATE0, and it sends an unsolicited
//! message before each answer, which phoned must not take for the answer.

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::FromRawFd;

use crate::world::Shared;

/// Open the pty and answer on it from a thread; the slave's path for phoned.
pub fn serve(world: Shared) -> std::io::Result<String> {
    // SAFETY: plain libc calls on descriptors this function owns.
    let (mut master, slave) = unsafe {
        let fd = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
        if fd < 0 || libc::grantpt(fd) != 0 || libc::unlockpt(fd) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut name = [0 as libc::c_char; 64];
        if libc::ptsname_r(fd, name.as_mut_ptr(), name.len()) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        (File::from_raw_fd(fd), std::ffi::CStr::from_ptr(name.as_ptr()).to_string_lossy().into_owned())
    };
    // Held open, so the master does not read EIO between phoned's opens.
    let keep = std::fs::OpenOptions::new().read(true).write(true).open(&slave)?;
    let path = slave.clone();
    std::thread::Builder::new().name("at-port".into()).spawn(move || {
        let _keep = keep;
        let mut echo = true;
        let mut line = Vec::new();
        let mut buf = [0u8; 256];
        loop {
            let n = match master.read(&mut buf) {
                Ok(0) | Err(_) => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    continue;
                }
                Ok(n) => n,
            };
            for &b in &buf[..n] {
                if b == b'\n' {
                    continue;
                }
                if b != b'\r' {
                    line.push(b);
                    continue;
                }
                let cmd = String::from_utf8_lossy(&line).trim().to_string();
                line.clear();
                if cmd.is_empty() {
                    continue;
                }
                tracing::info!(cmd, "AT command on the spare port");
                let mut out = String::new();
                if echo {
                    out.push_str(&cmd);
                    out.push('\r');
                }
                out.push_str("\r\n+QIND: \"csq\",20,99\r\n");
                let upper = cmd.to_ascii_uppercase();
                let answer = if upper == "ATE0" || upper == "ATE1" {
                    echo = upper == "ATE1";
                    Some(String::new())
                } else {
                    let mut w = world.blocking_lock();
                    w.at.port_last = cmd.clone();
                    w.at.answer(&cmd)
                };
                match answer {
                    Some(a) if a.is_empty() => out.push_str("\r\nOK\r\n"),
                    Some(a) => out.push_str(&format!("\r\n{a}\r\n\r\nOK\r\n")),
                    None => out.push_str("\r\nERROR\r\n"),
                }
                let _ = master.write_all(out.as_bytes());
            }
        }
    })?;
    tracing::info!(port = %path, "spare AT port up");
    Ok(path)
}
