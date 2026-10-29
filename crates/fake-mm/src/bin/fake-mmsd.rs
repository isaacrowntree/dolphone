//! fake-mmsd: a stand-in for mmsd-tng on the session bus, for testing phoned's MMS.
//!
//! It serves the part of mmsd-tng's API phoned uses (doc/manager-api.txt,
//! service-api.txt and message-api.txt in https://gitlab.com/kop316/mmsd): the
//! Manager at `/org/ofono/mms`, one Service at `/org/ofono/mms/modemmanager`, and
//! a Message object per message. Received messages are kept as mmsd-tng keeps them,
//! one file per message (here the parts one after another, where mmsd-tng keeps
//! the encoded PDU) with each attachment's offset and length, in DOLPHONE_MMS_DIR.
//!
//! `io.github.isaacrowntree.Dolphone1.FakeMmsd` on the manager object is the far
//! end, which `phonectl fake mms ...` calls: deliver a message, list what was sent,
//! make sending fail.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Mutex;
use zbus::object_server::SignalEmitter;
use zbus::wire::{OwnedObjectPath, OwnedVariant, Variant};
use zbus::{interface, Connection};

const SERVICE_PATH: &str = "/org/ofono/mms/modemmanager";
const OWN_NUMBER: &str = "+61491570110";

type Props = HashMap<String, OwnedVariant>;

#[derive(Default)]
struct World {
    next: u32,
    /// Messages by path: their properties.
    messages: HashMap<String, Props>,
    /// What phoned sent: recipients, then each attachment's type and contents.
    sent: Vec<String>,
    send_fails: bool,
}

type Shared = Arc<Mutex<World>>;

fn dir() -> PathBuf {
    std::env::var_os("DOLPHONE_MMS_DIR").map(PathBuf::from).unwrap_or_else(|| std::env::temp_dir().join("fake-mmsd"))
}

fn ov(v: Variant<'_>) -> OwnedVariant {
    OwnedVariant::try_from(v).expect("value")
}

fn props_clone(p: &Props) -> Props {
    p.iter().filter_map(|(k, v)| v.try_clone().ok().map(|v| (k.clone(), v))).collect()
}

struct Manager;

#[interface(name = "org.ofono.mms.Manager")]
impl Manager {
    fn get_services(&self) -> Vec<(OwnedObjectPath, Props)> {
        vec![(OwnedObjectPath::try_from(SERVICE_PATH).expect("path"), Props::new())]
    }
}

struct Service {
    world: Shared,
}

#[interface(name = "org.ofono.mms.Service")]
impl Service {
    async fn get_messages(&self) -> Vec<(OwnedObjectPath, Props)> {
        let w = self.world.lock().await;
        w.messages.iter().map(|(p, v)| (OwnedObjectPath::try_from(p.as_str()).expect("path"), props_clone(v))).collect()
    }

    fn get_properties(&self) -> Props {
        Props::from([
            ("UseDeliveryReports".into(), ov(Variant::from(false))),
            ("AutoCreateSMIL".into(), ov(Variant::from(true))),
            ("TotalMaxAttachmentSize".into(), ov(Variant::from(1_100_000u32))),
            ("MaxAttachments".into(), ov(Variant::from(25u32))),
        ])
    }

    /// As mmsd-tng: the message is a draft at once, and its Status changes when
    /// the MMSC has it.
    async fn send_message(
        &self,
        recipients: Vec<String>,
        _options: HashMap<String, OwnedVariant>,
        attachments: Vec<(String, String, String)>,
        #[zbus(connection)] conn: &Connection,
        #[zbus(signal_emitter)] em: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        let mut line = format!("to={}", recipients.join(","));
        for (_, mime, file) in &attachments {
            let bytes =
                std::fs::read(file).map_err(|e| zbus::fdo::Error::InvalidArgs(format!("cannot read {file}: {e}")))?;
            if mime.starts_with("text/plain") {
                line.push_str(&format!(" text={:?}", String::from_utf8_lossy(&bytes)));
            } else {
                line.push_str(&format!(" {mime}={}b", bytes.len()));
            }
        }
        let (path, fails) = {
            let mut w = self.world.lock().await;
            w.next += 1;
            let path = format!("{SERVICE_PATH}/out{}", w.next);
            w.sent.push(line);
            let props = Props::from([
                ("Status".into(), ov(Variant::from("draft"))),
                ("Recipients".into(), ov(Variant::from(recipients.clone()))),
            ]);
            w.messages.insert(path.clone(), props);
            (path, w.send_fails)
        };
        conn.object_server().at(path.as_str(), Message { world: self.world.clone() }).await?;
        let p = OwnedObjectPath::try_from(path.clone()).expect("path");
        let props = Props::from([("Status".into(), ov(Variant::from("draft")))]);
        Service::message_added(&em, p.clone(), props).await?;
        let conn = conn.clone();
        let world = self.world.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            let status = if fails { "sending_failed" } else { "sent" };
            if let Some(m) = world.lock().await.messages.get_mut(&path) {
                m.insert("Status".into(), ov(Variant::from(status)));
            }
            if let Ok(em) = SignalEmitter::new(&conn, path.as_str()) {
                let _ = Message::property_changed(&em, "Status", Variant::from(status)).await;
            }
        });
        Ok(p)
    }

    #[zbus(signal)]
    async fn message_added(em: &SignalEmitter<'_>, path: OwnedObjectPath, properties: Props) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn message_removed(em: &SignalEmitter<'_>, path: OwnedObjectPath) -> zbus::Result<()>;
}

struct Message {
    world: Shared,
}

#[interface(name = "org.ofono.mms.Message")]
impl Message {
    async fn delete(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<()> {
        let path = hdr.path().map(|p| p.to_string()).unwrap_or_default();
        self.world.lock().await.messages.remove(&path);
        if let Some(uuid) = path.rsplit('/').next() {
            let _ = std::fs::remove_file(dir().join(uuid));
        }
        if let Ok(em) = SignalEmitter::new(conn, SERVICE_PATH) {
            let _ = Service::message_removed(&em, OwnedObjectPath::try_from(path.clone()).expect("path")).await;
        }
        let conn = conn.clone();
        tokio::spawn(async move {
            let _ = conn.object_server().remove::<Message, _>(path.as_str()).await;
        });
        Ok(())
    }

    fn mark_read(&self) {}

    #[zbus(signal)]
    async fn property_changed(em: &SignalEmitter<'_>, name: &str, value: Variant<'_>) -> zbus::Result<()>;
}

struct Control {
    world: Shared,
}

#[interface(name = "io.github.isaacrowntree.Dolphone1.FakeMmsd")]
impl Control {
    /// Someone sends this phone an MMS: a text, and files (by path) as parts.
    /// `others` are the group's other members, beside this phone.
    async fn receive(
        &self,
        from: &str,
        others: Vec<String>,
        text: &str,
        files: Vec<String>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        let mut data: Vec<u8> = b"<smil><body><par/></body></smil>".to_vec();
        let mut atts: Vec<(String, String, String, u64, u64)> =
            vec![("smil".into(), "application/smil".into(), "smil.xml".into(), 0, data.len() as u64)];
        if !text.is_empty() {
            atts.push((
                "text".into(),
                "text/plain;charset=utf-8".into(),
                "text.txt".into(),
                data.len() as u64,
                text.len() as u64,
            ));
            data.extend_from_slice(text.as_bytes());
        }
        for (i, f) in files.iter().enumerate() {
            let bytes = std::fs::read(f).map_err(|e| zbus::fdo::Error::InvalidArgs(format!("{f}: {e}")))?;
            let mime = if f.ends_with(".png") { "image/png" } else { "image/jpeg" };
            atts.push((format!("part{i}"), mime.into(), format!("part{i}"), data.len() as u64, bytes.len() as u64));
            data.extend_from_slice(&bytes);
        }
        let (path, props) = {
            let mut w = self.world.lock().await;
            w.next += 1;
            let uuid = format!("in{:04}abcdef", w.next);
            std::fs::create_dir_all(dir()).map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            std::fs::write(dir().join(&uuid), &data).map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            let mut recipients = vec![OWN_NUMBER.to_string()];
            recipients.extend(others);
            let props = Props::from([
                ("Status".into(), ov(Variant::from("received"))),
                ("Date".into(), ov(Variant::from("2026-10-29T10:20:48+1100"))),
                ("Sender".into(), ov(Variant::from(from))),
                ("Recipients".into(), ov(Variant::from(recipients))),
                ("Modem Number".into(), ov(Variant::from(OWN_NUMBER))),
                ("Delivery Report".into(), ov(Variant::from(false))),
                ("Attachments".into(), ov(Variant::from(atts))),
            ]);
            let path = format!("{SERVICE_PATH}/{uuid}");
            w.messages.insert(path.clone(), props_clone(&props));
            (path, props)
        };
        conn.object_server().at(path.as_str(), Message { world: self.world.clone() }).await?;
        let p = OwnedObjectPath::try_from(path).expect("path");
        if let Ok(em) = SignalEmitter::new(conn, SERVICE_PATH) {
            Service::message_added(&em, p.clone(), props).await?;
        }
        Ok(p)
    }

    /// What was sent, a line each, and how many messages mmsd-tng still holds.
    async fn dump(&self) -> String {
        let w = self.world.lock().await;
        let mut out: String = w.sent.iter().map(|l| format!("sent {l}\n")).collect();
        out.push_str(&format!("held {}\n", w.messages.len()));
        out
    }

    async fn set_send_fails(&self, fails: bool) {
        self.world.lock().await.send_fails = fails;
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let world: Shared = Default::default();
    let _conn = zbus::connection::Builder::session()
        .name("org.ofono.mms")
        .serve_at("/org/ofono/mms", Manager)
        .serve_at("/org/ofono/mms", Control { world: world.clone() })
        .serve_at(SERVICE_PATH, Service { world: world.clone() })
        .build()
        .await?;
    eprintln!("fake mmsd-tng up at {SERVICE_PATH}");
    tokio::signal::ctrl_c().await?;
    Ok(())
}
