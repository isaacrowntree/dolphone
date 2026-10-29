//! Choosing pictures to send, through the desktop's own file chooser.
//!
//! The XDG desktop portal's FileChooser shows the chooser the desktop already has
//! (GNOME's, KDE's, or the GTK one), works the same inside a sandbox, and hands
//! back only the files picked. OpenFile answers at once with a request handle; the
//! choice comes later as that request's Response signal: 0 with the picked files'
//! `uris`, 1 when cancelled, 2 when the chooser failed.

use std::collections::HashMap;

use futures_util::StreamExt;
use zbus::{Connection, OwnedObjectPath, OwnedVariant, Variant};

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";

/// What came of asking.
#[derive(Debug, PartialEq, Eq)]
pub enum Picked {
    Files(Vec<String>),
    Cancelled,
    /// No portal on this desktop, or it failed; the words are for a toast.
    Unavailable(String),
}

/// Ask the desktop's file chooser for pictures, and wait for the answer.
pub async fn pick_pictures(title: &str) -> Picked {
    match ask(title).await {
        Ok(p) => p,
        Err(zbus::Error::MethodError(name, _, _))
            if matches!(
                name.as_str(),
                "org.freedesktop.DBus.Error.ServiceUnknown" | "org.freedesktop.DBus.Error.UnknownMethod"
            ) || name.as_str().starts_with("org.freedesktop.DBus.Error.UnknownInterface") =>
        {
            Picked::Unavailable("No file chooser: this desktop has no XDG desktop portal".into())
        }
        Err(e) => Picked::Unavailable(format!("The file chooser failed: {}", phone_bus::message(&e))),
    }
}

async fn ask(title: &str) -> zbus::Result<Picked> {
    let conn = Connection::session().await?;
    // The request's path is known before asking, from our name and a token of our
    // own, so its Response is listened for before it can be sent.
    // A token per ask, so a second chooser opened over the first answers apart.
    static ASKED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = ASKED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let token = format!("dolphone{}_{n}", std::process::id());
    let sender = conn.unique_name().map(|n| n.trim_start_matches(':').replace('.', "_")).unwrap_or_default();
    let expected = format!("{PORTAL_PATH}/request/{sender}/{token}");
    let mut responses = request(&conn, &expected).await?.receive_signal("Response").await?;

    let filters: Vec<(&str, Vec<(u32, &str)>)> = vec![(
        "Pictures",
        // 1 is a MIME type, which the chooser matches however the file is named.
        vec![(1, "image/jpeg"), (1, "image/png"), (1, "image/gif"), (1, "image/webp"), (1, "image/heic")],
    )];
    let mut options: HashMap<&str, Variant> = HashMap::new();
    options.insert("handle_token", Variant::from(token.as_str()));
    options.insert("modal", Variant::from(true));
    options.insert("multiple", Variant::from(true));
    options.insert("filters", Variant::from(filters));
    let chooser = zbus::Proxy::new(&conn, PORTAL, PORTAL_PATH, "org.freedesktop.portal.FileChooser").await?;
    let handle: OwnedObjectPath = chooser.call("OpenFile", &("", title, options)).await?;
    // A portal older than handle_token picks its own path: listen there instead.
    if handle.as_str() != expected {
        responses = request(&conn, handle.as_str()).await?.receive_signal("Response").await?;
    }
    let Some(reply) = responses.next().await else {
        return Ok(Picked::Unavailable("The file chooser went away".into()));
    };
    let (code, mut results): (u32, HashMap<String, OwnedVariant>) = reply.body().deserialize()?;
    let uris = results.remove("uris").and_then(|v| Vec::<String>::try_from(v).ok()).unwrap_or_default();
    Ok(response(code, &uris))
}

async fn request<'a>(conn: &Connection, path: &'a str) -> zbus::Result<zbus::Proxy<'a>> {
    zbus::Proxy::new(conn, PORTAL, path, "org.freedesktop.portal.Request").await
}

/// The Response signal's answer: what was picked, as paths on this machine.
pub fn response(code: u32, uris: &[String]) -> Picked {
    match code {
        0 => {
            let files = paths(uris);
            if files.is_empty() {
                Picked::Cancelled
            } else {
                Picked::Files(files)
            }
        }
        1 => Picked::Cancelled,
        _ => Picked::Unavailable("The file chooser could not open".into()),
    }
}

/// `file://` URIs as paths: percent-decoded, and with a `localhost` host allowed.
/// Anything not a local file (a network share the chooser offered) is left out,
/// since phoned reads the files itself.
pub fn paths(uris: &[String]) -> Vec<String> {
    uris.iter()
        .filter_map(|u| u.strip_prefix("file://"))
        .map(|rest| rest.strip_prefix("localhost").unwrap_or(rest))
        .filter(|p| p.starts_with('/'))
        .map(percent_decode)
        .collect()
}

/// A URI path's %XX escapes, as bytes; a path is bytes, not necessarily UTF-8, but
/// D-Bus carries it as a string, so anything else is replaced.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            if let Some(v) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A file's name, for showing what is attached.
pub fn file_name(path: &str) -> String {
    std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_become_paths() {
        let uris = [
            "file:///home/me/Pictures/Beach%20day%20%231.jpg".to_string(),
            "file://localhost/tmp/caf%C3%A9.png".into(),
            "sftp://host/photo.jpg".into(),
            "file:///home/me/100%.png".into(),
        ];
        assert_eq!(
            paths(&uris),
            ["/home/me/Pictures/Beach day #1.jpg", "/tmp/café.png", "/home/me/100%.png"],
            "decoded, localhost allowed, a remote file left out, a stray % kept"
        );
        assert_eq!(file_name("/home/me/Pictures/Beach day #1.jpg"), "Beach day #1.jpg");
    }

    #[test]
    fn responses() {
        let one = ["file:///p/a.jpg".to_string()];
        assert_eq!(response(0, &one), Picked::Files(vec!["/p/a.jpg".into()]));
        assert_eq!(response(1, &[]), Picked::Cancelled);
        assert_eq!(response(0, &[]), Picked::Cancelled, "nothing usable picked is nothing picked");
        assert!(matches!(response(2, &[]), Picked::Unavailable(_)));
    }
}
