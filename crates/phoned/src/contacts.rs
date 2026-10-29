//! The address book over the bus, and copying a phonebook from another phone.
//!
//! Contacts are `.vcf` files (see phone_core::contacts), so this is a thin layer:
//! list them for readers, and change them for Dolphone's own apps directly and for
//! anything else after a press on the device. A contact is not harmless data: its
//! name is what a caller shows as, and with "silence unknown callers" on it decides
//! who rings, so a script must not be able to make a scammer's number read "Bank".
//!
//! Import takes vCards from anywhere (an export from Google Contacts or iCloud) or
//! straight from a paired phone over Bluetooth with PBAP, the profile car kits use
//! to show your contacts. Both keep names and numbers only. There is no account,
//! no sign-in and nothing kept syncing: import again whenever you like, and
//! matches merge rather than double.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use phone_api::{ContactInfo, Placed};
use phone_core::contacts::{self, Book, Contact, Imported, Phone as Number};
use phone_core::number;
use zbus::message::Header;
use zbus::object_server::SignalEmitter;
use zbus::wire::{OwnedObjectPath, OwnedVariant, Variant};
use zbus::{interface, proxy, Connection};

use crate::caller::{self, Caller};
use crate::phone::Phone;

#[proxy(interface = "org.bluez.obex.Client1", default_service = "org.bluez.obex", default_path = "/org/bluez/obex")]
trait ObexClient {
    fn create_session(&self, destination: &str, args: HashMap<&str, Variant<'_>>) -> zbus::Result<OwnedObjectPath>;
    fn remove_session(&self, session: &zbus::wire::ObjectPath<'_>) -> zbus::Result<()>;
}

#[proxy(interface = "org.bluez.obex.PhonebookAccess1", default_service = "org.bluez.obex")]
trait PhonebookAccess {
    fn select(&self, location: &str, phonebook: &str) -> zbus::Result<()>;
    fn pull_all(
        &self,
        targetfile: &str,
        filters: HashMap<&str, Variant<'_>>,
    ) -> zbus::Result<(OwnedObjectPath, HashMap<String, OwnedVariant>)>;
}

#[proxy(interface = "org.bluez.obex.Transfer1", default_service = "org.bluez.obex")]
trait Transfer {
    #[zbus(property)]
    fn status(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn filename(&self) -> zbus::Result<String>;
}

/// The most a phonebook copied over Bluetooth may be, as for an import over the
/// bus: the file is the other phone's to write, as large as it likes.
const MAX_PHONEBOOK: u64 = 8 * 1024 * 1024;

/// Changes to the book are made one at a time: each reads the files, checks the
/// card is still the one asked about, and writes, with nothing in between.
static APPLYING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A Bluetooth address, as BlueZ writes it.
fn valid_address(a: &str) -> bool {
    let parts: Vec<&str> = a.split(':').collect();
    parts.len() == 6 && parts.iter().all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Pull the phone's own phonebook ("int", "pb") as vCard 3.0 with names and
/// numbers only, and return the cards' text.
async fn pull_phonebook(session_bus: &Connection, address: &str) -> Result<String, String> {
    let client = ObexClientProxy::new(session_bus)
        .await
        .map_err(|_| "Bluetooth file transfer (obexd) is not running; install bluez-obexd".to_string())?;
    let session = client
        .create_session(address, HashMap::from([("Target", Variant::from("pbap"))]))
        .await
        .map_err(|e| format!("the phone did not accept a contacts connection ({}); allow contact sharing for this device on the phone", crate::api::short(&e)))?;
    let result = async {
        let pb = PhonebookAccessProxy::builder(session_bus).path(session.clone()).build().await?;
        pb.select("int", "pb").await?;
        let fields = Variant::from(vec!["VERSION", "FN", "N", "TEL"]);
        let (transfer, props) =
            pb.pull_all("", HashMap::from([("Format", Variant::from("vcard30")), ("Fields", fields)])).await?;
        let t = TransferProxy::builder(session_bus)
            .path(transfer)
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await?;
        // A few hundred contacts take seconds; give a big phonebook two minutes.
        for _ in 0..240 {
            match t.status().await.as_deref() {
                Ok("complete") => break,
                Ok("error") => return Err(zbus::Error::Failure("the phone stopped the transfer".into())),
                // The transfer object goes away as it completes.
                Err(_) => break,
                _ => tokio::time::sleep(Duration::from_millis(500)).await,
            }
        }
        let file = props
            .get("Filename")
            .and_then(|v| String::try_from(v.clone()).ok())
            .ok_or_else(|| zbus::Error::Failure("no file from the transfer".into()))?;
        let text = read_capped(std::path::Path::new(&file), MAX_PHONEBOOK);
        let _ = std::fs::remove_file(&file);
        text.map_err(zbus::Error::Failure)
    }
    .await;
    let _ = client.remove_session(&session.as_ref()).await;
    result.map_err(|e| crate::api::short(&e))
}

/// A file's text, refusing one over `max` bytes (checked as it is read, so a file
/// still growing cannot slip past).
fn read_capped(path: &std::path::Path, max: u64) -> Result<String, String> {
    use std::io::Read;
    let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    f.take(max + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > max {
        return Err(format!("the phonebook is more than {} MB", max / (1024 * 1024)));
    }
    String::from_utf8(bytes).map_err(|_| "the phonebook is not text".to_string())
}

fn to_info(c: &Contact) -> ContactInfo {
    ContactInfo {
        id: c.id(),
        name: c.name.clone(),
        numbers: c.numbers.iter().map(|p| (p.kind.clone(), p.number.clone())).collect(),
    }
}

/// Clean a contact as given over the bus.
fn checked(name: &str, numbers: &[(String, String)], region: &str) -> zbus::fdo::Result<(String, Vec<Number>)> {
    let name = phone_core::text::clean(name, 100).trim().to_string();
    if name.is_empty() {
        return Err(zbus::fdo::Error::InvalidArgs("a contact needs a name".into()));
    }
    if numbers.is_empty() || numbers.len() > 20 {
        return Err(zbus::fdo::Error::InvalidArgs("a contact has 1 to 20 numbers".into()));
    }
    let mut out = Vec::new();
    for (kind, n) in numbers {
        let c = number::canonical(n, region);
        if number::digits(&c).len() < 3 {
            return Err(zbus::fdo::Error::InvalidArgs(format!("{n} is not a phone number")));
        }
        let kind = kind.trim().to_lowercase();
        let kind = if kind.is_empty() || kind.len() > 20 || !kind.chars().all(|c| c.is_ascii_alphabetic()) {
            "mobile".to_string()
        } else {
            kind
        };
        out.push(Number { number: c, kind });
    }
    Ok((name, out))
}

pub struct Contacts(pub Arc<Phone>);

impl Contacts {
    fn book(&self) -> Book {
        Book::load(&phone_core::store::contacts_dir(), &self.0.region())
    }

    /// Do it now for our own apps; hold it for a press for anything else.
    async fn change(&self, who: &Caller, what: &str, op: Op) -> zbus::fdo::Result<Placed> {
        if !who.trusted {
            let body = serde_json::to_string(&op).map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            let placed = self.0.park_contact(what, &body, who).await;
            self.0.audit(who, "contacts", what, "waiting for confirmation");
            return Ok(placed);
        }
        apply(&self.0, op).await
    }
}

/// A change to the book, as it waits in a confirmation.
///
/// An id is a file and a place in it, and places move when a card before it goes,
/// so a change to one contact also carries what that contact was when asked
/// (`was`): once confirmed it is made only if the id still names that contact,
/// never to whichever card has come to hold the place.
#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub enum Op {
    Add {
        name: String,
        numbers: Vec<(String, String)>,
    },
    Update {
        id: String,
        name: String,
        numbers: Vec<(String, String)>,
        #[serde(default)]
        was: Option<Was>,
    },
    Remove {
        id: String,
        #[serde(default)]
        was: Option<Was>,
    },
    Import {
        vcards: String,
    },
}

/// A contact as it was when a change to it was asked for.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Was {
    name: String,
    numbers: Vec<(String, String)>,
}

impl Was {
    fn of(c: &Contact) -> Self {
        Self { name: c.name.clone(), numbers: to_info(c).numbers }
    }
}

/// The contact `id` names, checked against what it was when asked about.
fn still<'a>(book: &'a Book, id: &str, was: &Option<Was>) -> zbus::fdo::Result<&'a Contact> {
    let c = book.get(id).ok_or_else(no_such)?;
    if was.as_ref().is_some_and(|w| *w != Was::of(c)) {
        return Err(zbus::fdo::Error::Failed(
            "that contact has changed since this was asked for; nothing was done, ask again".into(),
        ));
    }
    Ok(c)
}

/// Carry out a change, now or once confirmed.
pub async fn apply(phone: &Arc<Phone>, op: Op) -> zbus::fdo::Result<Placed> {
    let _one_at_a_time = APPLYING.lock().await;
    let dir = phone_core::store::contacts_dir();
    let region = phone.region();
    let mut book = Book::load(&dir, &region);
    let io = |e: std::io::Error| zbus::fdo::Error::Failed(e.to_string());
    let placed = match op {
        Op::Add { name, numbers } => {
            let (name, numbers) = checked(&name, &numbers, &region)?;
            book.add(&dir, Contact { name, numbers, source: None, card: 0 }).map_err(io)?;
            Placed { id: 1, pending: 0 }
        }
        Op::Update { id, name, numbers, was } => {
            let (name, numbers) = checked(&name, &numbers, &region)?;
            still(&book, &id, &was)?;
            book.update(&id, &name, numbers, &region).map_err(io)?;
            Placed { id: 1, pending: 0 }
        }
        Op::Remove { id, was } => {
            still(&book, &id, &was)?;
            book.remove(&id, &region).map_err(io)?;
            Placed { id: 1, pending: 0 }
        }
        Op::Import { vcards } => {
            let r = book.import(&dir, contacts::parse(&vcards, &region), &region).map_err(io)?;
            emit_imported(&phone.session, r, "").await;
            Placed { id: i64::from(r.added + r.merged), pending: 0 }
        }
    };
    emit_changed(&phone.session).await;
    Ok(placed)
}

#[interface(name = "io.github.isaacrowntree.Dolphone1.Contacts")]
impl Contacts {
    async fn list(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<Vec<ContactInfo>> {
        let who = caller::identify(conn, &hdr).await;
        if !who.may_read() {
            return Err(zbus::fdo::Error::AccessDenied(format!(
                "{} may not read contacts; grant it in Phone Settings (read_grants)",
                who.name
            )));
        }
        self.0.audit(&who, "contacts", "", "read");
        Ok(self.book().contacts.iter().map(to_info).collect())
    }

    async fn add(
        &self,
        name: &str,
        numbers: Vec<(String, String)>,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<Placed> {
        let who = caller::identify(conn, &hdr).await;
        checked(name, &numbers, &self.0.region())?;
        self.change(&who, &format!("add {name}"), Op::Add { name: name.into(), numbers }).await
    }

    async fn update(
        &self,
        id: &str,
        name: &str,
        numbers: Vec<(String, String)>,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<Placed> {
        let who = caller::identify(conn, &hdr).await;
        checked(name, &numbers, &self.0.region())?;
        let was = self.book().get(id).map(Was::of).ok_or_else(no_such)?;
        let what = format!("change {}", was.name);
        self.change(&who, &what, Op::Update { id: id.into(), name: name.into(), numbers, was: Some(was) }).await
    }

    async fn remove(
        &self,
        id: &str,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<Placed> {
        let who = caller::identify(conn, &hdr).await;
        let was = self.book().get(id).map(Was::of).ok_or_else(no_such)?;
        let what = format!("delete {}", was.name);
        self.change(&who, &what, Op::Remove { id: id.into(), was: Some(was) }).await
    }

    async fn import(
        &self,
        vcards: &str,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<Placed> {
        let who = caller::identify(conn, &hdr).await;
        if vcards.len() > 8 * 1024 * 1024 {
            return Err(zbus::fdo::Error::InvalidArgs("that is more than 8 MB of contacts".into()));
        }
        let n = contacts::parse(vcards, &self.0.region()).len().min(contacts::IMPORT_MAX);
        if n == 0 {
            return Err(zbus::fdo::Error::InvalidArgs("no contacts with a name or a number in that".into()));
        }
        self.change(&who, &format!("import {n} contacts"), Op::Import { vcards: vcards.into() }).await
    }

    /// Only Dolphone's own apps start a phonebook copy: it is a Bluetooth
    /// connection to someone's phone, which the owner asks for on the device.
    async fn import_from_phone(
        &self,
        address: &str,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<()> {
        let who = caller::identify(conn, &hdr).await;
        if !who.trusted {
            return Err(zbus::fdo::Error::AccessDenied(
                "only Dolphone's own apps can copy contacts from a phone (Phone Settings > Bluetooth)".into(),
            ));
        }
        let address = address.trim().to_ascii_uppercase();
        if !valid_address(&address) {
            return Err(zbus::fdo::Error::InvalidArgs("a Bluetooth address is six pairs, AA:BB:CC:DD:EE:FF".into()));
        }
        let phone = self.0.clone();
        tokio::spawn(async move {
            tracing::info!("copying contacts from a phone over Bluetooth");
            match pull_phonebook(&phone.session, &address).await {
                Ok(text) => {
                    if let Err(e) = apply(&phone, Op::Import { vcards: text }).await {
                        emit_imported(&phone.session, Imported::default(), &e.to_string()).await;
                    }
                }
                Err(e) => {
                    tracing::warn!("phonebook copy failed: {e}");
                    emit_imported(&phone.session, Imported::default(), &e).await;
                }
            }
        });
        Ok(())
    }

    #[zbus(signal)]
    async fn changed(em: &SignalEmitter<'_>) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn imported(em: &SignalEmitter<'_>, added: u32, merged: u32, unchanged: u32, error: &str)
        -> zbus::Result<()>;
}

fn no_such() -> zbus::fdo::Error {
    zbus::fdo::Error::InvalidArgs("no such contact".into())
}

async fn emit_changed(conn: &Connection) {
    if let Ok(em) = SignalEmitter::new(conn, phone_api::PATH) {
        let _ = Contacts::changed(&em).await;
    }
}

async fn emit_imported(conn: &Connection, r: Imported, error: &str) {
    if let Ok(em) = SignalEmitter::new(conn, phone_api::PATH) {
        let _ = Contacts::imported(&em, r.added, r.merged, r.unchanged, error).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_and_numbers_are_checked() {
        assert!(valid_address("AA:BB:CC:00:11:22"));
        assert!(!valid_address("AA:BB:CC:00:11"));
        assert!(!valid_address("../../etc"));
        let (name, n) = checked(" Alex ", &[("Cell".into(), "0491 570 156".into())], "AU").unwrap();
        assert_eq!(name, "Alex");
        assert_eq!(n[0].number, "+61491570156");
        assert_eq!(n[0].kind, "cell");
        assert!(checked("", &[("".into(), "0491570156".into())], "AU").is_err());
        assert!(checked("x", &[("".into(), "hi".into())], "AU").is_err());
        let (_, n) = checked("x", &[("<script>".into(), "0491570156".into())], "AU").unwrap();
        assert_eq!(n[0].kind, "mobile", "odd kinds become mobile");
    }

    #[test]
    fn a_confirmed_change_is_for_the_contact_that_was_asked_about() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("all.vcf"),
            "BEGIN:VCARD\nFN:Alex\nTEL:0491570156\nEND:VCARD\nBEGIN:VCARD\nFN:Sam\nTEL:0491570157\nEND:VCARD\n",
        )
        .unwrap();
        let book = Book::load(dir.path(), "AU");
        let sam = Some(Was::of(book.get("all#1").unwrap()));
        assert!(still(&book, "all#1", &sam).is_ok());
        // Alex goes; the place Sam was asked about is gone, and "all" is Sam now.
        std::fs::write(dir.path().join("all.vcf"), "BEGIN:VCARD\nFN:Sam\nTEL:0491570157\nEND:VCARD\n").unwrap();
        let book = Book::load(dir.path(), "AU");
        assert!(still(&book, "all#1", &sam).is_err());
        let alex = Some(Was { name: "Alex".into(), numbers: vec![("phone".into(), "+61491570156".into())] });
        assert!(still(&book, "all", &alex).is_err(), "Sam is not changed in Alex's place");
        assert!(still(&book, "all", &None).is_ok(), "an older request with nothing to check");
    }

    #[test]
    fn copied_phonebooks_are_capped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pb.vcf");
        std::fs::write(&path, vec![b'x'; 2048]).unwrap();
        assert!(read_capped(&path, 1024).is_err());
        assert_eq!(read_capped(&path, 4096).unwrap().len(), 2048);
    }
}
