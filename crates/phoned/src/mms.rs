//! MMS (pictures, and texts to more than one person) through mmsd-tng.
//!
//! An MMS is not carried in the text itself: the network sends a short binary
//! push (WAP) saying one is waiting, and the phone fetches it over mobile data
//! from the carrier's server (the MMSC). mmsd-tng does all of that against
//! ModemManager: it reads the push from ModemManager itself (AutoProcessSMSWAP),
//! fetches and decodes the message, and serves it on the session bus. phoned is its
//! client, as GNOME's Chatty is, and keeps what arrives in its own store.
//!
//! The API, from mmsd-tng's doc/manager-api.txt, service-api.txt and
//! message-api.txt (https://gitlab.com/kop316/mmsd):
//!
//! - `org.ofono.mms.Manager` at `/org/ofono/mms`: `GetServices() -> a(oa{sv})`.
//! - `org.ofono.mms.Service` (for ModemManager, `/org/ofono/mms/modemmanager`):
//!   `GetMessages() -> a(oa{sv})`, `SendMessage(as recipients, a{sv} options,
//!   a(sss) attachments) -> o`, and the `MessageAdded(o, a{sv})` signal.
//! - `org.ofono.mms.Message` at `<service>/<uuid>`: `Delete()`, and
//!   `PropertyChanged(s, v)` as its Status moves to `sent` or `sending_failed`.
//!
//! A message's `Attachments` are `a(ssstt)`: id, content type, file name, and the
//! offset and length of the part inside the file mmsd-tng keeps at
//! `~/.mms/modemmanager/<uuid>` (doc/storage.txt; plugins/modemmanager.c names the
//! service "modemmanager"). phoned copies the parts out, then deletes mmsd-tng's
//! copy: one store, which is the one the owner can wipe.
//!
//! The carrier's MMS settings (MMSC, proxy, APN) are mmsd-tng's, in
//! `~/.mms/modemmanager/mms`; it looks most of them up by itself.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use zbus::proxy;
use zbus::wire::{OwnedObjectPath, OwnedVariant, Variant};

pub const SERVICE: &str = "org.ofono.mms";

#[proxy(interface = "org.ofono.mms.Manager", default_service = "org.ofono.mms", default_path = "/org/ofono/mms")]
pub trait Manager {
    fn get_services(&self) -> zbus::Result<Vec<(OwnedObjectPath, HashMap<String, OwnedVariant>)>>;
}

#[proxy(interface = "org.ofono.mms.Service", default_service = "org.ofono.mms")]
pub trait Service {
    fn get_messages(&self) -> zbus::Result<Vec<(OwnedObjectPath, HashMap<String, OwnedVariant>)>>;
    fn send_message(
        &self,
        recipients: &[&str],
        options: HashMap<&str, Variant<'_>>,
        attachments: &[(&str, &str, &str)],
    ) -> zbus::Result<OwnedObjectPath>;
    #[zbus(signal)]
    fn message_added(&self, path: OwnedObjectPath, properties: HashMap<String, OwnedVariant>) -> zbus::Result<()>;
}

#[proxy(interface = "org.ofono.mms.Message", default_service = "org.ofono.mms")]
pub trait Message {
    fn delete(&self) -> zbus::Result<()>;
    #[zbus(signal)]
    fn property_changed(&self, name: String, value: OwnedVariant) -> zbus::Result<()>;
}

/// The most phoned keeps of one message's attachments.
pub const MAX_BYTES: u64 = phone_api::MMS_MAX_BYTES;

/// Whether mmsd-tng is on the session bus, and is mmsd-tng: any program can claim
/// its name, and would then be handed every outgoing MMS (recipients, text and
/// files) and could feed in fake ones. So the name's owner must be a system program
/// (installed under /usr, owned by root), as for the notification server. A debug
/// build testing against fake-mmsd (DOLPHONE_MMS_DIR set) skips that check.
pub async fn available(session: &zbus::Connection) -> bool {
    let Ok(d) = zbus::fdo::DBusProxy::new(session).await else { return false };
    let Ok(owner) = d.get_name_owner(SERVICE.try_into().expect("name")).await else { return false };
    if cfg!(debug_assertions) && std::env::var_os("DOLPHONE_MMS_DIR").is_some() {
        return true;
    }
    match crate::caller::exe_of(session, owner.as_str()).await {
        Some(exe) if crate::caller::system_program(&exe) => true,
        Some(exe) => {
            tracing::warn!(program = %exe.display(), "the MMS service is not a system program; not using it");
            false
        }
        None => false,
    }
}

/// The service phoned uses: the first mmsd-tng offers (it serves one per modem).
pub async fn service(session: &zbus::Connection) -> zbus::Result<ServiceProxy<'static>> {
    let services = ManagerProxy::new(session).await?.get_services().await?;
    let (path, _) =
        services.into_iter().next().ok_or_else(|| zbus::Error::Failure("mmsd-tng has no service".into()))?;
    ServiceProxy::builder(session).path(path).build().await
}

/// Where mmsd-tng keeps a message's file: `~/.mms/modemmanager/<uuid>`, or in a
/// test (debug builds) under DOLPHONE_MMS_DIR.
pub fn file_of(message: &str) -> Option<PathBuf> {
    let uuid = message.rsplit('/').next().filter(|u| !u.is_empty() && u.chars().all(|c| c.is_ascii_alphanumeric()))?;
    let dir = match std::env::var_os("DOLPHONE_MMS_DIR").filter(|_| cfg!(debug_assertions)) {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(std::env::var_os("HOME")?).join(".mms/modemmanager"),
    };
    Some(dir.join(uuid))
}

fn get<T: TryFrom<OwnedVariant>>(props: &HashMap<String, OwnedVariant>, key: &str) -> Option<T> {
    props.get(key).and_then(|v| v.try_clone().ok()).and_then(|v| T::try_from(v).ok())
}

/// One received MMS, as mmsd-tng describes it.
#[derive(Debug, Default)]
pub struct Received {
    pub sender: String,
    /// Everyone it went to, which in a group includes this phone.
    pub recipients: Vec<String>,
    /// This phone's own number, to leave out of a group.
    pub own: String,
    pub date: String,
    pub subject: String,
    pub attachments: Vec<(String, String, String, u64, u64)>,
}

impl Received {
    /// A received message from its properties; None for anything else (our own
    /// sent ones, drafts).
    pub fn from_props(props: &HashMap<String, OwnedVariant>) -> Option<Self> {
        let status: String = get(props, "Status")?;
        if status != "received" && status != "read" {
            return None;
        }
        Some(Self {
            sender: get(props, "Sender").unwrap_or_default(),
            recipients: get(props, "Recipients").unwrap_or_default(),
            own: get(props, "Modem Number").unwrap_or_default(),
            date: get(props, "Date").unwrap_or_default(),
            subject: get(props, "Subject").unwrap_or_default(),
            attachments: get(props, "Attachments").unwrap_or_default(),
        })
    }
}

/// The text of a message and its other parts (MIME type, bytes), read out of
/// mmsd-tng's file. The presentation (SMIL) is left out: phoned shows a thread,
/// not a slide show.
pub fn read_parts(file: &Path, attachments: &[(String, String, String, u64, u64)]) -> (String, Vec<(String, Vec<u8>)>) {
    use std::io::{Read, Seek, SeekFrom};
    let mut text = String::new();
    let mut parts = Vec::new();
    let Ok(mut f) = std::fs::File::open(file) else { return (text, parts) };
    let mut total = 0u64;
    for (_, content_type, _, offset, len) in attachments {
        let mime = content_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
        if mime == "application/smil" || total + len > MAX_BYTES {
            continue;
        }
        let mut buf = vec![0; *len as usize];
        if f.seek(SeekFrom::Start(*offset)).is_err() || f.read_exact(&mut buf).is_err() {
            continue;
        }
        total += len;
        if mime == "text/plain" {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&String::from_utf8_lossy(&buf));
        } else {
            parts.push((mime, buf));
        }
    }
    (text, parts)
}

/// Who a message is between, as a thread: the sender alone, or for a group every
/// number on it but this phone's, sorted and joined with commas.
pub fn peer_of(sender: &str, recipients: &[String], own: &str, canonical: impl Fn(&str) -> String) -> String {
    let own = canonical(own);
    let mut members: Vec<String> = std::iter::once(sender)
        .chain(recipients.iter().map(String::as_str))
        .map(&canonical)
        .filter(|n| !n.is_empty() && *n != own)
        .collect();
    members.sort();
    members.dedup();
    members.join(",")
}

/// A file name ending for a part, so a file manager opens it.
pub fn extension(mime: &str) -> &'static str {
    match mime {
        "image/jpeg" | "image/jpg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/heic" => "heic",
        "video/mp4" => "mp4",
        "video/3gpp" => "3gp",
        "audio/amr" => "amr",
        "audio/mpeg" => "mp3",
        "audio/mp4" | "audio/aac" => "m4a",
        "audio/ogg" => "ogg",
        "text/vcard" | "text/x-vcard" => "vcf",
        _ => "bin",
    }
}

/// The MIME type of a file to send, from its name.
pub fn mime_of(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "mp4" => "video/mp4",
        "3gp" => "video/3gpp",
        "amr" => "audio/amr",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "ogg" => "audio/ogg",
        "vcf" => "text/vcard",
        "txt" => "text/plain",
        _ => "application/octet-stream",
    }
}

/// Send an MMS: the text as a text/plain part, then the files. Returns mmsd-tng's
/// message, whose Status says when it has gone.
pub async fn send(
    session: &zbus::Connection,
    recipients: &[String],
    text_file: Option<&Path>,
    files: &[(String, String)],
    report: bool,
) -> zbus::Result<OwnedObjectPath> {
    let service = service(session).await?;
    let mut attachments: Vec<(String, String, String)> = Vec::new();
    if let Some(t) = text_file {
        attachments.push(("text".into(), "text/plain;charset=utf-8".into(), t.display().to_string()));
    }
    for (i, (mime, path)) in files.iter().enumerate() {
        attachments.push((format!("part{i}"), mime.clone(), path.clone()));
    }
    let refs: Vec<(&str, &str, &str)> =
        attachments.iter().map(|(a, b, c)| (a.as_str(), b.as_str(), c.as_str())).collect();
    let to: Vec<&str> = recipients.iter().map(String::as_str).collect();
    let options = HashMap::from([("DeliveryReport", Variant::from(report))]);
    service.send_message(&to, options, &refs).await
}

/// Wait for a sent message's Status to settle: true when sent. mmsd-tng retries by
/// itself while the data connection is down, so this waits a while.
pub async fn follow(session: &zbus::Connection, path: &OwnedObjectPath) -> bool {
    use futures_util::StreamExt;
    let Ok(msg) = MessageProxy::builder(session).path(path.clone()).build().await else { return false };
    let Ok(mut changes) = msg.receive_property_changed().await else { return false };
    let wait = async {
        while let Some(c) = changes.next().await {
            let Ok(args) = c.args() else { continue };
            if args.name() != "Status" {
                continue;
            }
            match <&str>::try_from(args.value()) {
                Ok("sent") => return true,
                Ok("sending_failed") => return false,
                _ => {}
            }
        }
        false
    };
    let sent = tokio::time::timeout(std::time::Duration::from_secs(600), wait).await.unwrap_or(false);
    // Ours is in the store; mmsd-tng's copy is not needed either way.
    let _ = msg.delete().await;
    sent
}

/// Delete mmsd-tng's copy of a message phoned has taken.
pub async fn forget(session: &zbus::Connection, path: &OwnedObjectPath) {
    if crate::storekey::locked() {
        return; // mmsd-tng keeps it until phoned can.
    }
    if let Ok(m) = MessageProxy::builder(session).path(path.clone()).build().await {
        let _ = m.delete().await;
    }
}

/// Follow mmsd-tng while it runs: take every message it has received, then each
/// one as it arrives. It may start after phoned, or restart; phoned follows it.
pub async fn run(phone: std::sync::Arc<crate::phone::Phone>) {
    use futures_util::StreamExt;
    let session = phone.session.clone();
    let Ok(bus) = zbus::fdo::DBusProxy::new(&session).await else { return };
    let Ok(mut owners) = bus.receive_name_owner_changed_with_args(&[(0, SERVICE)]).await else { return };
    loop {
        if !available(&session).await {
            // Until it starts (or once an hour, in case a signal was missed).
            let _ = tokio::time::timeout(std::time::Duration::from_secs(3600), owners.next()).await;
            continue;
        }
        let service = match service(&session).await {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("mmsd-tng: {e}");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
        };
        let Ok(mut added) = service.receive_message_added().await else { continue };
        tracing::info!("MMS through mmsd-tng");
        for (path, props) in service.get_messages().await.unwrap_or_default() {
            phone.mms_received(path, &props).await;
        }
        loop {
            tokio::select! {
                m = added.next() => {
                    let Some(m) = m else { break };
                    if let Ok(a) = m.args() {
                        phone.mms_received(a.path().clone(), a.properties()).await;
                    }
                }
                // Gone, or started again: look again either way.
                _ = owners.next() => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_leave_out_this_phone() {
        let canon = |s: &str| s.replace(' ', "");
        let to = vec!["+61491570110".to_string(), "+61491570157".to_string()];
        assert_eq!(peer_of("+61491570156", &to, "+61491570110", canon), "+61491570156,+61491570157");
        assert_eq!(peer_of("+61491570156", &["+61491570110".into()], "+61491570110", canon), "+61491570156");
    }

    #[test]
    fn parts_come_out_of_the_file_by_offset() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("abc123");
        std::fs::write(&f, b"<smil/>Hello\xff\xd8jpeg").unwrap();
        let atts = vec![
            ("smil".into(), "application/smil".into(), "s.smil".into(), 0, 7),
            ("t".into(), "text/plain;charset=utf-8".into(), "t.txt".into(), 7, 5),
            ("p".into(), "image/jpeg".into(), "p.jpg".into(), 12, 6),
        ];
        let (text, parts) = read_parts(&f, &atts);
        assert_eq!(text, "Hello");
        assert_eq!(parts, vec![("image/jpeg".to_string(), b"\xff\xd8jpeg".to_vec())]);
        assert!(file_of("/org/ofono/mms/modemmanager/a.b").is_none(), "a uuid is letters and digits");
        assert_eq!(extension("image/jpeg"), "jpg");
        assert_eq!(mime_of("/tmp/a.JPG"), "image/jpeg");
    }
}
