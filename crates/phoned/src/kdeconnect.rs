//! KDE Connect: the phone's calls and texts on a laptop, through the KDE Connect
//! (or GSConnect, or Valent) already on it. No new client: Dolphone speaks the
//! protocol a paired Android phone speaks, for the parts a phone provides.
//!
//! The protocol, as KDE Connect's own code has it (core/backends/lan):
//!
//! - Discovery: an identity packet broadcast on UDP 1716, carrying `tcpPort`. The
//!   side that hears one opens TCP to that port and sends its own identity in the
//!   clear; then the side that opened TCP is the TLS *server* and the side that
//!   accepted is the TLS client. TCP ports are 1716 to 1764.
//! - Protocol version 8 (the minimum): once encrypted, each side sends its identity
//!   again, and the device id must match the first one and the certificate's
//!   common name.
//! - Certificates are self-signed, with the device id as the common name; a paired
//!   device's certificate is remembered and must match next time.
//! - Packets are one JSON object per line: `{"id", "type", "body"}`.
//! - Pairing is `kdeconnect.pair` with `{"pair": true|false}`; nothing else is
//!   handled before it.
//!
//! What Dolphone offers: texts (`kdeconnect.sms.*`: the conversation list, one
//! conversation, sending), calls ringing, answered and missed
//! (`kdeconnect.telephony`), and muting the ringer from the laptop. Pairing is
//! always confirmed on the device, like any outside request; a paired laptop then
//! texts as the owner (premium numbers and short codes still ask).

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, SignatureScheme};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::mpsc;

use crate::caller::Caller;
use crate::phone::Phone;

pub const UDP_PORT: u16 = 1716;
pub const MIN_TCP_PORT: u16 = 1716;
pub const MAX_TCP_PORT: u16 = 1764;
pub const PROTOCOL_VERSION: u64 = 8;
/// Unpaired devices allowed connected at once (they can only ask to pair).
const MAX_UNPAIRED: usize = 8;
/// Connections at once, in any state (handshaking, unpaired, paired), and from
/// any one address: anyone on the network can open them, before any pairing.
const MAX_CONNECTIONS: usize = 32;
const MAX_PER_IP: usize = 4;
/// How long a TLS handshake may take.
const HANDSHAKE: Duration = Duration::from_secs(10);
/// How long an unpaired link may sit silent. Longer than a pairing request waits
/// for the owner, so a laptop asking is not dropped while they decide.
const UNPAIRED_IDLE: Duration = Duration::from_secs(180);
/// The longest line read: an identity (before TLS, or just after), any packet
/// from an unpaired device (it can only ask to pair), and any from a paired one.
const MAX_IDENTITY: usize = 16 * 1024;
const MAX_UNPAIRED_LINE: usize = 64 * 1024;
const MAX_PAIRED_LINE: usize = 1024 * 1024;
/// A pairing request is good for as long as phoned's own requests are.
const PAIR_TTL: Duration = Duration::from_secs(120);
/// After a pairing request, the same device or address waits this long before
/// asking again, doubling with each ask up to the cap; an hour quiet forgives.
const PAIR_BACKOFF: Duration = Duration::from_secs(30);
const PAIR_BACKOFF_MAX: Duration = Duration::from_secs(30 * 60);
const PAIR_FORGIVE: Duration = Duration::from_secs(60 * 60);

const INCOMING: &[&str] = &[
    "kdeconnect.sms.request",
    "kdeconnect.sms.request_conversations",
    "kdeconnect.sms.request_conversation",
    "kdeconnect.telephony.request_mute",
    "kdeconnect.ping",
];
const OUTGOING: &[&str] = &["kdeconnect.sms.messages", "kdeconnect.telephony", "kdeconnect.ping"];

fn dir() -> PathBuf {
    phone_core::store::data_dir().join("kdeconnect")
}

/// KDE Connect's own rule for a device id.
pub fn valid_device_id(id: &str) -> bool {
    (32..=38).contains(&id.len()) && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// KDE Connect's rule for a name: no quotes or punctuation, 32 characters at most.
pub fn filter_name(name: &str) -> String {
    name.chars().filter(|c| !"\"',;:.!?()[]<>".contains(*c)).take(32).collect()
}

/// This device's id: made once, in the form KDE Connect makes them.
fn device_id() -> anyhow::Result<String> {
    let path = dir().join("device_id");
    if let Ok(id) = std::fs::read_to_string(&path) {
        if valid_device_id(id.trim()) {
            return Ok(id.trim().to_string());
        }
    }
    let mut b = [0u8; 16];
    std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom")?, &mut b)?;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    let id = format!("{}_{}_{}_{}_{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..]);
    write_private(&path, id.as_bytes())?;
    Ok(id)
}

fn write_private(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
        std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700))?;
    }
    std::fs::write(path, data)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

/// This device's certificate and key: self-signed, the device id as the common
/// name (KDE Connect checks it), made once.
fn certificate(id: &str) -> anyhow::Result<(CertificateDer<'static>, PrivateKeyDer<'static>)> {
    let (cert_path, key_path) = (dir().join("certificate.der"), dir().join("private_key.der"));
    if let (Ok(c), Ok(k)) = (std::fs::read(&cert_path), std::fs::read(&key_path)) {
        if common_name(&c).as_deref() == Some(id) {
            return Ok((CertificateDer::from(c), PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(k))));
        }
    }
    let key = rcgen::KeyPair::generate()?;
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new())?;
    let mut dn = rcgen::DistinguishedName::new();
    dn.push(rcgen::DnType::CommonName, id);
    dn.push(rcgen::DnType::OrganizationName, "KDE");
    dn.push(rcgen::DnType::OrganizationalUnitName, "KDE Connect");
    params.distinguished_name = dn;
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(365);
    params.not_after = now + time::Duration::days(3650);
    let cert = params.self_signed(&key)?;
    write_private(&cert_path, cert.der())?;
    write_private(&key_path, &key.serialize_der())?;
    Ok((cert.der().clone(), PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()))))
}

/// The common name of a certificate, which for KDE Connect is the device id.
pub fn common_name(der: &[u8]) -> Option<String> {
    let (_, cert) = x509_parser::parse_x509_certificate(der).ok()?;
    let cn = cert.subject().iter_common_name().next()?.as_str().ok()?.to_string();
    Some(cn)
}

/// Paired devices, with the certificate each paired with.
#[derive(Default, Serialize, Deserialize)]
struct Trusted {
    devices: HashMap<String, TrustedDevice>,
}

#[derive(Clone, Serialize, Deserialize)]
struct TrustedDevice {
    name: String,
    /// The certificate, DER, hex.
    certificate: String,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

impl Trusted {
    fn path() -> PathBuf {
        dir().join("trusted.json")
    }
    fn load() -> Self {
        std::fs::read_to_string(Self::path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }
    fn save(&self) {
        if let Ok(t) = serde_json::to_string_pretty(self) {
            let _ = write_private(&Self::path(), t.as_bytes());
        }
    }
}

/// Accepts any certificate in the handshake (KDE Connect's are all self-signed),
/// still checking the handshake's signatures; who it is, is checked afterwards
/// against the common name and, once paired, the remembered certificate.
#[derive(Debug)]
struct AnyCertificate(Arc<CryptoProvider>);

impl ServerCertVerifier for AnyCertificate {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

impl ClientCertVerifier for AnyCertificate {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }
    fn verify_client_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        Ok(ClientCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// One line on the wire.
pub fn packet(kind: &str, body: Value) -> String {
    let id = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    format!("{}\n", json!({"id": id as u64, "type": kind, "body": body}))
}

struct Me {
    id: String,
    name: String,
    tcp_port: u16,
    server: Arc<rustls::ServerConfig>,
    client: Arc<rustls::ClientConfig>,
}

impl Me {
    fn identity(&self, with_port: bool) -> String {
        let mut body = json!({
            "deviceId": self.id,
            "deviceName": self.name,
            "deviceType": "phone",
            "protocolVersion": PROTOCOL_VERSION,
            "incomingCapabilities": INCOMING,
            "outgoingCapabilities": OUTGOING,
        });
        if with_port {
            body["tcpPort"] = json!(self.tcp_port);
        }
        packet("kdeconnect.identity", body)
    }
}

/// A connected device.
struct Link {
    /// The laptop's name, as it gave it.
    name: String,
    /// The certificate it linked with, DER, hex: a pairing answer goes to the link
    /// that asked, not to whatever link holds the same device id by then.
    cert: String,
    paired: bool,
    tx: mpsc::UnboundedSender<String>,
}

/// A pairing request waiting for the owner.
#[derive(Clone, Debug)]
struct Ask {
    name: String,
    cert: String,
    ip: IpAddr,
    at: Instant,
}

/// Pairing requests waiting, one per device id and one per address, and when
/// each device id and address may ask again.
#[derive(Default)]
struct Asking {
    pending: HashMap<String, Ask>,
    /// "id:<device>" or "ip:<address>": (may ask again at, asks so far).
    backoff: HashMap<String, (Instant, u32)>,
}

impl Asking {
    fn expire(&mut self, now: Instant) {
        self.pending.retain(|_, a| now.duration_since(a.at) < PAIR_TTL);
        self.backoff.retain(|_, (next, _)| now.saturating_duration_since(*next) < PAIR_FORGIVE);
    }

    /// Note a request, if this device and this address may ask now.
    fn request(&mut self, device: &str, name: &str, cert: &str, ip: IpAddr, now: Instant) -> bool {
        self.expire(now);
        if self.pending.contains_key(device) || self.pending.values().any(|a| a.ip == ip) {
            return false;
        }
        let keys = [format!("id:{device}"), format!("ip:{ip}")];
        if keys.iter().any(|k| self.backoff.get(k).is_some_and(|(next, _)| *next > now)) {
            return false;
        }
        for k in keys {
            let e = self.backoff.entry(k).or_insert((now, 0));
            e.1 += 1;
            let wait = PAIR_BACKOFF.saturating_mul(1u32 << (e.1 - 1).min(16)).min(PAIR_BACKOFF_MAX);
            e.0 = now + wait;
        }
        self.pending.insert(device.to_string(), Ask { name: name.to_string(), cert: cert.to_string(), ip, at: now });
        true
    }

    /// The request a device has waiting, if it has not expired.
    fn take(&mut self, device: &str, now: Instant) -> Option<Ask> {
        self.expire(now);
        self.pending.remove(device)
    }
}

/// Connections open now, in all and by address.
#[derive(Default)]
struct Conns {
    total: usize,
    by_ip: HashMap<IpAddr, usize>,
}

impl Conns {
    fn admit(&mut self, ip: IpAddr) -> bool {
        let here = self.by_ip.get(&ip).copied().unwrap_or(0);
        if self.total >= MAX_CONNECTIONS || here >= MAX_PER_IP {
            return false;
        }
        self.total += 1;
        self.by_ip.insert(ip, here + 1);
        true
    }
    fn release(&mut self, ip: IpAddr) {
        self.total = self.total.saturating_sub(1);
        if let Some(n) = self.by_ip.get_mut(&ip) {
            *n -= 1;
            if *n == 0 {
                self.by_ip.remove(&ip);
            }
        }
    }
}

/// One connection's place in [`Conns`], given back when it ends.
struct Slot {
    conns: Arc<Mutex<Conns>>,
    ip: IpAddr,
}

impl Drop for Slot {
    fn drop(&mut self) {
        if let Ok(mut c) = self.conns.lock() {
            c.release(self.ip);
        }
    }
}

/// A device being connected to, so one heard again (it announces every few
/// seconds, and anyone can send identity packets) does not start another.
struct Connecting {
    set: Arc<Mutex<HashSet<String>>>,
    peer: String,
}

impl Drop for Connecting {
    fn drop(&mut self) {
        if let Ok(mut s) = self.set.lock() {
            s.remove(&self.peer);
        }
    }
}

/// The KDE Connect side of phoned.
#[derive(Default)]
pub struct KdeConnect {
    links: Mutex<HashMap<String, Link>>,
    /// Pairing requests waiting for the owner's answer.
    asking: Mutex<Asking>,
    conns: Arc<Mutex<Conns>>,
    connecting: Arc<Mutex<HashSet<String>>>,
    /// The owner's "show message text in notifications" (private notifications
    /// off), kept here from the settings; off until read, which is the private side.
    show_bodies: AtomicBool,
}

impl KdeConnect {
    fn slot(&self, ip: IpAddr) -> Option<Slot> {
        let ok = self.conns.lock().map(|mut c| c.admit(ip)).unwrap_or(false);
        ok.then(|| Slot { conns: self.conns.clone(), ip })
    }

    fn start_connecting(&self, peer: &str) -> Option<Connecting> {
        let fresh = self.connecting.lock().map(|mut s| s.insert(peer.to_string())).unwrap_or(false);
        fresh.then(|| Connecting { set: self.connecting.clone(), peer: peer.to_string() })
    }

    fn send_paired(&self, line: &str) {
        if let Ok(links) = self.links.lock() {
            for l in links.values().filter(|l| l.paired) {
                let _ = l.tx.send(line.to_string());
            }
        }
    }

    /// A call is ringing, connected, or stopped ringing.
    pub fn ringing(&self, number: &str, name: &str) {
        self.send_paired(&packet(
            "kdeconnect.telephony",
            json!({"event": "ringing", "phoneNumber": number, "contactName": name}),
        ));
    }
    pub fn talking(&self, number: &str, name: &str) {
        self.send_paired(&packet(
            "kdeconnect.telephony",
            json!({"event": "talking", "phoneNumber": number, "contactName": name}),
        ));
    }
    pub fn missed(&self, number: &str, name: &str) {
        self.send_paired(&packet(
            "kdeconnect.telephony",
            json!({"event": "missedCall", "phoneNumber": number, "contactName": name}),
        ));
    }
    pub fn stopped_ringing(&self, number: &str) {
        self.send_paired(&packet(
            "kdeconnect.telephony",
            json!({"event": "ringing", "phoneNumber": number, "isCancel": true}),
        ));
    }

    /// A text arrived or was sent: push it, as a phone does.
    ///
    /// With private notifications on, a text that arrived is not pushed: a laptop
    /// shows a pushed text as it comes, on a screen anyone nearby may see, and
    /// one-time codes arrive by text. Pushing it without its body is no better,
    /// as KDE Connect keeps a message by its id and would show it empty from then
    /// on. The laptop is paired, so the conversation (text and all) is still
    /// there when it asks for it, as it does when its messages window opens.
    pub fn message(&self, m: &phone_core::model::Message) {
        if !pushes(m, self.show_bodies.load(Ordering::Relaxed)) {
            return;
        }
        self.send_paired(&packet("kdeconnect.sms.messages", json!({"messages": [message_json(m)], "version": 2})));
    }

    /// The owner said yes or no to a pairing request.
    ///
    /// The answer is for the certificate that asked: only a link with that one is
    /// told, or paired, and it is the one remembered. A request older than
    /// [`PAIR_TTL`] has lapsed and is not answered.
    pub fn answer_pair(&self, device: &str, yes: bool) {
        let Some(ask) = self.asking.lock().ok().and_then(|mut a| a.take(device, Instant::now())) else { return };
        if let Ok(mut links) = self.links.lock() {
            if let Some(l) = links.get_mut(device).filter(|l| l.cert == ask.cert) {
                let _ = l.tx.send(packet("kdeconnect.pair", json!({"pair": yes})));
                l.paired = yes;
            }
        }
        if yes {
            if let Ok(mut a) = self.asking.lock() {
                a.backoff.remove(&format!("id:{device}"));
                a.backoff.remove(&format!("ip:{}", ask.ip));
            }
            let mut t = Trusted::load();
            t.devices.insert(device.to_string(), TrustedDevice { name: ask.name, certificate: ask.cert });
            t.save();
            tracing::info!("KDE Connect: paired");
        }
    }

    /// The devices linked now: (id, name, paired).
    pub fn devices(&self) -> Vec<(String, String, bool)> {
        self.links
            .lock()
            .map(|l| l.iter().map(|(id, l)| (id.clone(), l.name.clone(), l.paired)).collect())
            .unwrap_or_default()
    }

    /// Forget a paired device.
    pub fn unpair(&self, device: &str) {
        if let Ok(mut a) = self.asking.lock() {
            a.pending.remove(device);
        }
        let mut t = Trusted::load();
        t.devices.remove(device);
        t.save();
        if let Ok(mut links) = self.links.lock() {
            if let Some(l) = links.get_mut(device) {
                let _ = l.tx.send(packet("kdeconnect.pair", json!({"pair": false})));
                l.paired = false;
            }
        }
    }
}

/// Whether a text is pushed live (see [`KdeConnect::message`]).
fn pushes(m: &phone_core::model::Message, show_bodies: bool) -> bool {
    show_bodies || m.direction == phone_core::model::Direction::Outgoing
}

/// Read one line, up to and including its newline, into `buf`, failing once it
/// passes `max` bytes rather than after the whole line is held: the peer decides
/// how long a line is, and before pairing the peer is anyone.
async fn read_line_capped<R: AsyncBufRead + Unpin>(r: &mut R, buf: &mut Vec<u8>, max: usize) -> std::io::Result<usize> {
    let start = buf.len();
    loop {
        let available = r.fill_buf().await?;
        if available.is_empty() {
            return Ok(buf.len() - start);
        }
        let (take, done) = match available.iter().position(|&b| b == b'\n') {
            Some(i) => (i + 1, true),
            None => (available.len(), false),
        };
        if buf.len() - start + take > max {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "line too long"));
        }
        buf.extend_from_slice(&available[..take]);
        r.consume(take);
        if done {
            return Ok(buf.len() - start);
        }
    }
}

/// A thread id KDE Connect can hold (a positive 63-bit number), stable for a peer.
pub fn thread_id(peer: &str) -> i64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in peer.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    (h >> 1) as i64
}

/// A message as KDE Connect's ConversationMessage reads it.
pub fn message_json(m: &phone_core::model::Message) -> Value {
    use phone_core::model::{Direction, MessageState};
    // Android's message types: 1 inbox, 2 sent, 5 failed, 6 queued.
    let kind = match (m.direction, m.state) {
        (Direction::Incoming, _) => 1,
        (Direction::Outgoing, MessageState::Failed) => 5,
        (Direction::Outgoing, MessageState::Sending) => 6,
        (Direction::Outgoing, _) => 2,
    };
    json!({
        "event": 1,
        "body": m.body,
        "addresses": [{"address": m.peer}],
        "date": m.at * 1000,
        "type": kind,
        "read": i32::from(m.read),
        "thread_id": thread_id(&m.peer),
        "_id": m.id,
        "sub_id": -1,
    })
}

/// Run while the owner has KDE Connect on.
pub async fn run(phone: Arc<Phone>) {
    loop {
        if !phone.flag(phone_api::keys::KDECONNECT) {
            tokio::time::sleep(Duration::from_secs(3)).await;
            continue;
        }
        note_privacy(&phone);
        if let Err(e) = serve(&phone).await {
            tracing::warn!("KDE Connect: {e}");
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    }
}

fn note_privacy(phone: &Phone) {
    phone.kdeconnect.show_bodies.store(!phone.private_notify(), Ordering::Relaxed);
}

async fn serve(phone: &Arc<Phone>) -> anyhow::Result<()> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let id = device_id()?;
    let (cert, key) = certificate(&id)?;
    let verifier = Arc::new(AnyCertificate(provider.clone()));
    let server = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()?
        .with_client_cert_verifier(verifier.clone())
        .with_single_cert(vec![cert.clone()], key.clone_key())?;
    let client = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(vec![cert], key)?;
    let mut listener = None;
    for port in MIN_TCP_PORT..=MAX_TCP_PORT {
        if let Ok(l) = TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).await {
            listener = Some((l, port));
            break;
        }
    }
    let (listener, tcp_port) = listener.ok_or_else(|| anyhow::anyhow!("no free TCP port in 1716-1764"))?;
    let name = filter_name(&phone.setting(phone_api::keys::KDECONNECT_NAME));
    let me = Arc::new(Me {
        id,
        name: if name.is_empty() { "Dolphone".into() } else { name },
        tcp_port,
        server: Arc::new(server),
        client: Arc::new(client),
    });
    let udp = discovery_socket()?;
    tracing::info!(tcp_port, "KDE Connect on");
    let mut announce = tokio::time::interval(Duration::from_secs(60));
    let mut buf = vec![0u8; 8192];
    loop {
        note_privacy(phone);
        if !phone.flag(phone_api::keys::KDECONNECT) {
            tracing::info!("KDE Connect off");
            if let Ok(mut links) = phone.kdeconnect.links.lock() {
                links.clear();
            }
            return Ok(());
        }
        tokio::select! {
            _ = announce.tick() => {
                let _ = udp.send_to(me.identity(true).as_bytes(), (Ipv4Addr::BROADCAST, UDP_PORT)).await;
            }
            Ok((n, from)) = udp.recv_from(&mut buf) => {
                if let Some((peer, port)) = discovered(&buf[..n], &me.id) {
                    let linked = phone.kdeconnect.links.lock().map(|l| l.contains_key(&peer)).unwrap_or(true);
                    let started = if linked { None } else { phone.kdeconnect.start_connecting(&peer) };
                    if let Some(started) = started {
                        if let Some(slot) = phone.kdeconnect.slot(from.ip()) {
                            let (phone, me) = (phone.clone(), me.clone());
                            tokio::spawn(async move {
                                let addr = SocketAddr::new(from.ip(), port);
                                if let Err(e) = connect(&phone, &me, addr, &peer, started).await {
                                    tracing::debug!("KDE Connect: {e}");
                                }
                                drop(slot);
                            });
                        }
                    }
                }
            }
            Ok((s, from)) = listener.accept() => {
                // Over the limits, the connection is closed at once.
                if let Some(slot) = phone.kdeconnect.slot(from.ip()) {
                    let (phone, me) = (phone.clone(), me.clone());
                    tokio::spawn(async move {
                        if let Err(e) = accepted(&phone, &me, s, from.ip()).await {
                            tracing::debug!("KDE Connect: {e}");
                        }
                        drop(slot);
                    });
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(3)) => {}
        }
    }
}

/// UDP 1716, shared: another KDE Connect on the same machine (a desktop session's
/// own) may hold it too, and with SO_REUSEADDR every socket on the port hears the
/// broadcasts.
fn discovery_socket() -> anyhow::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    s.set_reuse_address(true)?;
    s.set_broadcast(true)?;
    s.set_nonblocking(true)?;
    s.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, UDP_PORT)).into())?;
    Ok(UdpSocket::from_std(s.into())?)
}

/// A UDP identity worth connecting to: (device id, TCP port).
pub fn discovered(datagram: &[u8], me: &str) -> Option<(String, u16)> {
    let v: Value = serde_json::from_slice(datagram.strip_suffix(b"\n").unwrap_or(datagram)).ok()?;
    if v["type"] != "kdeconnect.identity" {
        return None;
    }
    let b = &v["body"];
    let id = b["deviceId"].as_str()?;
    let port = b["tcpPort"].as_u64()?;
    let version = b["protocolVersion"].as_u64()?;
    (valid_device_id(id)
        && id != me
        && version >= PROTOCOL_VERSION
        && (u64::from(MIN_TCP_PORT)..=u64::from(MAX_TCP_PORT)).contains(&port))
    .then(|| (id.to_string(), port as u16))
}

/// We heard a device: open TCP, say who we are, then be the TLS server.
/// `started` marks the device as being connected to until it is linked (or not).
async fn connect(
    phone: &Arc<Phone>,
    me: &Arc<Me>,
    addr: SocketAddr,
    peer: &str,
    started: Connecting,
) -> anyhow::Result<()> {
    let mut s = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr)).await??;
    tokio::time::timeout(Duration::from_secs(5), s.write_all(me.identity(false).as_bytes())).await??;
    let tls = tokio::time::timeout(HANDSHAKE, tokio_rustls::TlsAcceptor::from(me.server.clone()).accept(s)).await??;
    let cert = tls.get_ref().1.peer_certificates().and_then(|c| c.first()).map(|c| c.to_vec());
    link(phone, me, tls, peer, addr.ip(), cert, Some(started)).await
}

/// A device connected to us: read its identity, then be the TLS client.
async fn accepted(phone: &Arc<Phone>, me: &Arc<Me>, s: TcpStream, ip: IpAddr) -> anyhow::Result<()> {
    let mut r = BufReader::new(s);
    let mut line = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), read_line_capped(&mut r, &mut line, MAX_IDENTITY)).await??;
    let v: Value = serde_json::from_slice(&line)?;
    anyhow::ensure!(v["type"] == "kdeconnect.identity", "not an identity");
    let peer = v["body"]["deviceId"].as_str().unwrap_or("").to_string();
    anyhow::ensure!(valid_device_id(&peer) && peer != me.id, "bad device id");
    anyhow::ensure!(v["body"]["protocolVersion"].as_u64().unwrap_or(0) >= PROTOCOL_VERSION, "protocol too old");
    let s = r.into_inner();
    let tls = tokio::time::timeout(
        HANDSHAKE,
        tokio_rustls::TlsConnector::from(me.client.clone()).connect(ServerName::IpAddress(ip.into()), s),
    )
    .await??;
    let cert = tls.get_ref().1.peer_certificates().and_then(|c| c.first()).map(|c| c.to_vec());
    link(phone, me, tls, &peer, ip, cert, None).await
}

/// After TLS: exchange identities again, check who it is, and serve packets.
async fn link<S>(
    phone: &Arc<Phone>,
    me: &Arc<Me>,
    tls: S,
    peer: &str,
    ip: IpAddr,
    cert: Option<Vec<u8>>,
    started: Option<Connecting>,
) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let cert = cert.ok_or_else(|| anyhow::anyhow!("no certificate"))?;
    anyhow::ensure!(common_name(&cert).as_deref() == Some(peer), "certificate is not the device's");
    let (r, mut w) = tokio::io::split(tls);
    tokio::time::timeout(Duration::from_secs(5), w.write_all(me.identity(false).as_bytes())).await??;
    let mut r = BufReader::new(r);
    let mut line = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), read_line_capped(&mut r, &mut line, MAX_IDENTITY)).await??;
    let v: Value = serde_json::from_slice(&line)?;
    anyhow::ensure!(
        v["type"] == "kdeconnect.identity" && v["body"]["deviceId"] == peer,
        "identity changed mid-handshake"
    );
    let name = filter_name(v["body"]["deviceName"].as_str().unwrap_or(""));
    let trusted = Trusted::load();
    let paired = match trusted.devices.get(peer) {
        Some(t) if t.certificate == hex(&cert) => true,
        Some(_) => anyhow::bail!("a paired device's certificate changed"),
        None => false,
    };
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    {
        let mut links = phone.kdeconnect.links.lock().map_err(|_| anyhow::anyhow!("poisoned"))?;
        anyhow::ensure!(!links.contains_key(peer), "already linked");
        anyhow::ensure!(paired || links.values().filter(|l| !l.paired).count() < MAX_UNPAIRED, "too many unpaired");
        links.insert(peer.to_string(), Link { name: name.clone(), cert: hex(&cert), paired, tx: tx.clone() });
    }
    // Linked: the device is no longer being connected to (and a second link to it
    // is refused while this one lasts).
    drop(started);
    tracing::info!(device = %name, paired, "KDE Connect: linked");
    let writer = tokio::spawn(async move {
        while let Some(line) = rx.recv().await {
            if w.write_all(line.as_bytes()).await.is_err() {
                break;
            }
        }
    });
    let mut line = Vec::new();
    loop {
        line.clear();
        let paired = phone.kdeconnect.links.lock().map(|l| l.get(peer).is_some_and(|l| l.paired)).unwrap_or(false);
        // Unpaired, a device can only ask to pair: it gets short lines, and is
        // dropped when it goes quiet.
        let read = if paired {
            read_line_capped(&mut r, &mut line, MAX_PAIRED_LINE).await
        } else {
            match tokio::time::timeout(UNPAIRED_IDLE, read_line_capped(&mut r, &mut line, MAX_UNPAIRED_LINE)).await {
                Ok(read) => read,
                // Paired while it waited (the owner said yes): it stays.
                Err(_) if phone.kdeconnect.links.lock().is_ok_and(|l| l.get(peer).is_some_and(|l| l.paired)) => {
                    continue
                }
                Err(_) => break,
            }
        };
        match read {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let Ok(p) = serde_json::from_slice::<Value>(&line) else { continue };
        handle(phone, peer, &name, &cert, ip, &p, &tx).await;
    }
    writer.abort();
    if let Ok(mut links) = phone.kdeconnect.links.lock() {
        links.remove(peer);
    }
    tracing::info!(device = %name, "KDE Connect: gone");
    Ok(())
}

async fn handle(
    phone: &Arc<Phone>,
    peer: &str,
    name: &str,
    cert: &[u8],
    ip: IpAddr,
    p: &Value,
    tx: &mpsc::UnboundedSender<String>,
) {
    let kind = p["type"].as_str().unwrap_or("");
    let body = &p["body"];
    let paired = phone.kdeconnect.links.lock().map(|l| l.get(peer).is_some_and(|l| l.paired)).unwrap_or(false);
    if kind == "kdeconnect.pair" {
        match (body["pair"].as_bool(), paired) {
            (Some(true), true) => {
                let _ = tx.send(packet("kdeconnect.pair", json!({"pair": true})));
            }
            (Some(true), false) => {
                // One request at a time from a device or an address, and fewer
                // as they repeat: each one puts a question in front of the owner.
                let ask = phone
                    .kdeconnect
                    .asking
                    .lock()
                    .map(|mut a| a.request(peer, name, &hex(cert), ip, Instant::now()))
                    .unwrap_or(false);
                if ask {
                    phone.park_kdeconnect_pair(peer, name).await;
                } else {
                    tracing::debug!(device = %name, "KDE Connect: a pairing request was not passed on");
                }
            }
            (Some(false), _) => {
                phone.kdeconnect.unpair(peer);
            }
            _ => {}
        }
        return;
    }
    // Nothing else before pairing.
    if !paired {
        return;
    }
    match kind {
        "kdeconnect.sms.request_conversations" => {
            let latest: Vec<Value> = phone
                .store_read(|s| s.threads())
                .unwrap_or_default()
                .iter()
                .filter_map(|t| phone.store_read(|s| s.conversation(&t.peer)).ok()?.last().map(message_json))
                .collect();
            let _ = tx.send(packet("kdeconnect.sms.messages", json!({"messages": latest, "version": 2})));
        }
        "kdeconnect.sms.request_conversation" => {
            let want = body["threadID"].as_i64().unwrap_or(-1);
            let before = body["rangeStartTimestamp"].as_i64();
            let count = body["numberToRequest"].as_u64().map(|n| n as usize);
            let threads = phone.store_read(|s| s.threads()).unwrap_or_default();
            let Some(t) = threads.iter().find(|t| thread_id(&t.peer) == want) else { return };
            let mut msgs = phone.store_read(|s| s.conversation(&t.peer)).unwrap_or_default();
            if let Some(ts) = before {
                msgs.retain(|m| m.at * 1000 < ts);
            }
            if let Some(n) = count {
                let skip = msgs.len().saturating_sub(n);
                msgs.drain(..skip);
            }
            let list: Vec<Value> = msgs.iter().map(message_json).collect();
            let _ = tx.send(packet("kdeconnect.sms.messages", json!({"messages": list, "version": 2})));
        }
        "kdeconnect.sms.request" => {
            let text = body["messageBody"].as_str().unwrap_or("");
            // One text each: Dolphone has no group (MMS) messages yet.
            let to: Vec<String> = body["addresses"]
                .as_array()
                .map(|a| a.iter().filter_map(|x| x["address"].as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            let as_owner = Caller { name: format!("KDE Connect ({name})"), trusted: true, reader: false };
            for n in to.iter().take(10) {
                if let Err(e) = phone.send(n, text, &as_owner, false).await {
                    tracing::warn!("KDE Connect: a text was not sent: {e}");
                }
            }
        }
        "kdeconnect.telephony.request_mute" => phone.stop_ringing(),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tests that point XDG_DATA_HOME somewhere take turns: it is one for the
    /// whole process.
    static DATA_HOME: Mutex<()> = Mutex::new(());

    #[test]
    fn ids_names_and_discovery() {
        assert!(valid_device_id("740bd4b9_b418_4ee4_97d6_caf1da8151be"));
        assert!(!valid_device_id("short"));
        assert!(!valid_device_id("740bd4b9 b418 4ee4 97d6 caf1da8151be"));
        assert_eq!(filter_name("Isaac's \"laptop\" (work)!"), "Isaacs laptop work");
        let me = "aaaaaaaa_bbbb_cccc_dddd_eeeeeeeeeeee";
        let them = json!({"id": 1, "type": "kdeconnect.identity", "body": {
            "deviceId": "740bd4b9_b418_4ee4_97d6_caf1da8151be", "deviceName": "laptop",
            "protocolVersion": 8, "tcpPort": 1716}});
        assert_eq!(
            discovered(them.to_string().as_bytes(), me),
            Some(("740bd4b9_b418_4ee4_97d6_caf1da8151be".into(), 1716))
        );
        let mut old = them.clone();
        old["body"]["protocolVersion"] = json!(7);
        assert_eq!(discovered(old.to_string().as_bytes(), me), None, "protocol 8 or newer");
        let mut far = them.clone();
        far["body"]["tcpPort"] = json!(22);
        assert_eq!(discovered(far.to_string().as_bytes(), me), None, "only KDE Connect's ports");
    }

    #[test]
    fn messages_are_shaped_as_kde_connect_reads_them() {
        use phone_core::model::{Direction, Message, MessageState};
        let m = Message {
            id: 7,
            peer: "+61491570156".into(),
            direction: Direction::Outgoing,
            body: "On my way".into(),
            at: 1_700_000_000,
            state: MessageState::Sent,
            read: true,
            sim: String::new(),
            parts: Vec::new(),
        };
        let v = message_json(&m);
        assert_eq!(v["type"], 2);
        assert_eq!(v["date"], 1_700_000_000_000i64);
        assert_eq!(v["addresses"][0]["address"], "+61491570156");
        assert_eq!(v["thread_id"], thread_id("+61491570156"));
        assert!(thread_id("+61491570156") > 0);
    }

    #[tokio::test]
    async fn lines_are_capped_as_they_are_read() {
        let mut line = Vec::new();
        let mut r: &[u8] = b"{\"a\":1}\nrest";
        assert_eq!(read_line_capped(&mut r, &mut line, 16).await.unwrap(), 8);
        assert_eq!(line, b"{\"a\":1}\n");
        // A line with no end, longer than the cap, fails without being held whole.
        let endless = vec![b'x'; 100_000];
        let mut r = BufReader::with_capacity(1024, &endless[..]);
        let mut line = Vec::new();
        assert!(read_line_capped(&mut r, &mut line, 4096).await.is_err());
        assert!(line.len() <= 4096);
        // The end of the stream ends the line.
        let mut r: &[u8] = b"";
        assert_eq!(read_line_capped(&mut r, &mut Vec::new(), 16).await.unwrap(), 0);
    }

    #[test]
    fn connections_are_capped_overall_and_per_address() {
        let mut c = Conns::default();
        let a: IpAddr = "192.0.2.1".parse().unwrap();
        for _ in 0..MAX_PER_IP {
            assert!(c.admit(a));
        }
        assert!(!c.admit(a), "one address is capped");
        c.release(a);
        assert!(c.admit(a));
        let mut c = Conns::default();
        for i in 0..MAX_CONNECTIONS {
            assert!(c.admit(IpAddr::from([10, 0, (i / 200) as u8, (i % 200) as u8])));
        }
        assert!(!c.admit("192.0.2.9".parse().unwrap()), "and so are all of them");
    }

    #[test]
    fn outbound_connects_are_not_doubled() {
        let k = KdeConnect::default();
        let first = k.start_connecting("dev").expect("first");
        assert!(k.start_connecting("dev").is_none(), "one in flight per device");
        drop(first);
        assert!(k.start_connecting("dev").is_some());
    }

    #[test]
    fn pairing_requests_are_one_at_a_time_and_back_off() {
        let now = Instant::now();
        let (ip, other_ip): (IpAddr, IpAddr) = ("192.0.2.1".parse().unwrap(), "192.0.2.2".parse().unwrap());
        let mut a = Asking::default();
        assert!(a.request("dev1", "laptop", "c1", ip, now));
        assert!(!a.request("dev1", "laptop", "c1", other_ip, now), "one per device");
        assert!(!a.request("dev2", "laptop", "c2", ip, now), "one per address");
        assert!(a.request("dev3", "other", "c3", other_ip, now));
        // Expired after the TTL, and then the same device waits out its backoff.
        assert!(a.take("dev1", now + PAIR_TTL).is_none(), "expired");
        assert!(a.request("dev1", "laptop", "c1", ip, now + PAIR_TTL), "30 s passed");
        assert!(a.take("dev1", now + PAIR_TTL).is_some());
        assert!(!a.request("dev1", "laptop", "c1", ip, now + PAIR_TTL + Duration::from_secs(30)), "now 60 s");
        assert!(a.request("dev1", "laptop", "c1", ip, now + PAIR_TTL + Duration::from_secs(61)));
    }

    #[test]
    fn a_pairing_answer_goes_to_the_certificate_that_asked() {
        let _env = DATA_HOME.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_DATA_HOME", dir.path());
        let k = KdeConnect::default();
        let ip: IpAddr = "192.0.2.1".parse().unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        k.links
            .lock()
            .unwrap()
            .insert("dev".into(), Link { name: "laptop".into(), cert: "other".into(), paired: false, tx });
        assert!(k.asking.lock().unwrap().request("dev", "laptop", "asker", ip, Instant::now()));
        k.answer_pair("dev", true);
        assert!(!k.links.lock().unwrap()["dev"].paired, "a link with another certificate is not paired");
        assert!(rx.try_recv().is_err());
        assert_eq!(Trusted::load().devices["dev"].certificate, "asker", "the asker's certificate is trusted");
    }

    #[test]
    fn private_notifications_keep_incoming_texts_off_the_laptop() {
        use phone_core::model::{Direction, Message, MessageState};
        let mut m = Message {
            id: 1,
            peer: "+61491570156".into(),
            direction: Direction::Incoming,
            body: "Your code is 123456".into(),
            at: 1,
            state: MessageState::Received,
            read: false,
            sim: String::new(),
            parts: Vec::new(),
        };
        assert!(!pushes(&m, false));
        assert!(pushes(&m, true));
        m.direction = Direction::Outgoing;
        assert!(pushes(&m, false), "the owner's own texts still show as sent");
    }

    #[test]
    fn certificates_name_the_device() {
        let _env = DATA_HOME.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_DATA_HOME", dir.path());
        let id = device_id().unwrap();
        assert!(valid_device_id(&id));
        let (cert, _) = certificate(&id).unwrap();
        assert_eq!(common_name(&cert).as_deref(), Some(id.as_str()));
        let (again, _) = certificate(&id).unwrap();
        assert_eq!(cert, again, "made once, then kept");
    }
}
