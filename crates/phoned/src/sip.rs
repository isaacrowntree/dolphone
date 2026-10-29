//! Calls over the internet, with SIP, through baresip.
//!
//! phoned runs its own baresip (a small, long-standing SIP client that Debian
//! packages) with a config it writes into `~/.config/dolphone/sip`, and drives it
//! over baresip's D-Bus control module: commands go in through `invoke`, and call
//! events come back as JSON on its `event` signal. A SIP call is then tracked like
//! a cellular one, so it gets the same screens, buttons, headsets, log and audio
//! routes; only the commands that move it differ.
//!
//! The password lives in the accounts file there, 0600 in a 0700 directory, and is
//! never sent over D-Bus. Anything could claim baresip's bus name, so phoned
//! listens only to the connection whose process is the baresip it started.
//!
//! Texts: SIP MESSAGEs that arrive are kept as texts, from the sender's number (or
//! SIP address). Sending them is not here: baresip 1.1.0 (Debian 13's) can only
//! message its own address book's "current contact", not a given address, so a
//! text to a SIP address is refused with that reason; texts to numbers go over the
//! mobile network. What else is not here: video, and
//! emergency calls, which always go over the mobile network: a SIP provider cannot
//! be relied on to route them or to know where you are.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use zbus::proxy;

use crate::phone::Phone;

#[proxy(interface = "com.github.Baresip", default_service = "com.github.Baresip", default_path = "/baresip")]
pub trait Baresip {
    // baresip's names are lowercase; zbus would otherwise ask for "Invoke".
    #[zbus(name = "invoke")]
    fn invoke(&self, command: &str) -> zbus::Result<String>;
    #[zbus(signal, name = "event")]
    fn event(&self, class: &str, kind: &str, param: &str) -> zbus::Result<()>;
    /// An incoming SIP MESSAGE: the account, the sender, the content type, the body.
    #[zbus(signal, name = "message")]
    fn message(&self, aor: &str, peer: &str, ctype: &str, body: &str) -> zbus::Result<()>;
}

const NAME: &str = "com.github.Baresip";

pub fn dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("/nonexistent"))
        .join("dolphone/sip")
}

fn password_file() -> PathBuf {
    dir().join("password")
}

/// Keep the account's password, readable only by the owner.
pub fn set_password(pw: &str) -> std::io::Result<()> {
    let d = dir();
    std::fs::create_dir_all(&d)?;
    std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700))?;
    let f = password_file();
    std::fs::write(&f, pw)?;
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o600))
}

pub fn has_password() -> bool {
    std::fs::metadata(password_file()).is_ok_and(|m| m.len() > 0)
}

/// A SIP address an owner may give: `sip:` (or `sips:`), a user, `@`, a host and
/// an optional port, and at most a `;transport=udp|tcp|tls`. Anything else (and in
/// particular `>`, `;` other than that one, quotes or line breaks, which would
/// change baresip's account line) is refused.
pub fn valid_account(a: &str) -> bool {
    let (base, transport) = match a.split_once(";transport=") {
        Some((b, t)) => (b, Some(t)),
        None => (a, None),
    };
    if transport.is_some_and(|t| !matches!(t, "udp" | "tcp" | "tls")) {
        return false;
    }
    let Some(rest) = base.strip_prefix("sip:").or_else(|| base.strip_prefix("sips:")) else { return false };
    let Some((user, host)) = rest.split_once('@') else { return false };
    let user_ok =
        !user.is_empty() && user.chars().all(|c| c.is_ascii_alphanumeric() || "+-_.!~*'()&=$,?/%".contains(c));
    let host_ok = !host.is_empty()
        && host.len() <= 253
        && host.chars().all(|c| c.is_ascii_alphanumeric() || ".-:[]".contains(c))
        && !host.starts_with('.');
    user_ok && host_ok
}

/// An outbound proxy: `sip:` and a host, an optional port, and at most a
/// `;transport=`. No user part, and nothing else.
pub fn valid_proxy(p: &str) -> bool {
    let (base, transport) = match p.split_once(";transport=") {
        Some((b, t)) => (b, Some(t)),
        None => (p, None),
    };
    if transport.is_some_and(|t| !matches!(t, "udp" | "tcp" | "tls")) {
        return false;
    }
    let Some(host) = base.strip_prefix("sip:").or_else(|| base.strip_prefix("sips:")) else { return false };
    !host.is_empty() && !host.contains('@') && host.chars().all(|c| c.is_ascii_alphanumeric() || ".-:[]".contains(c))
}

/// The host part of an account, for dialling a number on it.
pub fn domain(account: &str) -> String {
    let base = account.split(';').next().unwrap_or("");
    base.split_once('@').map(|(_, h)| h.to_string()).unwrap_or_default()
}

/// A number from a SIP address: the user part when it is a phone number, the
/// whole address otherwise (a SIP-only contact, `sip:alex@example.com`).
pub fn number_of(uri: &str) -> String {
    let rest = uri.trim_start_matches('<').trim_end_matches('>');
    let rest = rest.strip_prefix("sip:").or_else(|| rest.strip_prefix("sips:")).unwrap_or(rest);
    let user = rest.split(['@', ';']).next().unwrap_or("");
    if !user.is_empty() && user.chars().all(|c| c.is_ascii_digit() || c == '+') {
        user.to_string()
    } else {
        format!("sip:{}", rest.split(';').next().unwrap_or(""))
    }
}

/// What a SIP address dialled from the keypad or a contact should be.
pub fn uri_for(target: &str, account: &str) -> String {
    if target.starts_with("sip:") || target.starts_with("sips:") {
        target.to_string()
    } else {
        format!("sip:{target}@{}", domain(account))
    }
}

/// Tests run without a sound server: a debug build with DOLPHONE_SIP_TEST_AUDIO
/// set plays calls to nowhere and sends a tone.
pub fn test_audio() -> bool {
    cfg!(debug_assertions) && std::env::var_os("DOLPHONE_SIP_TEST_AUDIO").is_some()
}

/// baresip's config, for the account and the audio devices of the moment.
pub fn config(listen: &str) -> String {
    let audio = if test_audio() {
        "module ausine.so\nmodule aufile.so\naudio_player aufile,/dev/null\naudio_source ausine,440\naudio_alert aufile,/dev/null\n"
    } else {
        "audio_player pulse,\naudio_source pulse,\naudio_alert pulse,\n"
    };
    format!(
        "# Written by phoned; changes are overwritten.\n\
         module_path /usr/lib/baresip/modules\n\
         sip_listen {listen}\n\
         module opus.so\n\
         module g722.so\n\
         module g711.so\n\
         module pulse.so\n\
         module stun.so\n\
         module ice.so\n\
         module srtp.so\n\
         module_app account.so\n\
         module_app menu.so\n\
         module_app ctrl_dbus.so\n\
         ctrl_dbus_use session\n\
         {audio}"
    )
}

/// The accounts line: the address, its password, and how to register. Media is
/// encrypted when the provider offers it.
pub fn account_line(account: &str, password: &str, proxy: &str, register: bool) -> String {
    let pw = password.replace(['\n', '\r', ';', '>', '"'], "");
    let mut line = format!("<{account}>;auth_pass={pw};mediaenc=srtp;answermode=manual");
    if valid_proxy(proxy) {
        line.push_str(&format!(";outbound=\"{proxy}\""));
    }
    if !register {
        line.push_str(";regint=0");
    }
    line
}

/// One event from baresip, the parts phoned uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallEvent {
    pub kind: String,
    pub id: String,
    pub peer: String,
    pub incoming: bool,
    pub param: String,
}

pub fn parse_event(json: &str) -> Option<CallEvent> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    Some(CallEvent {
        kind: s("type"),
        id: s("id"),
        peer: s("peeruri"),
        incoming: s("direction") == "incoming",
        param: s("param"),
    })
}

/// How a call that never connected ended, from the SIP reason baresip reports.
pub fn refusal(param: &str) -> Option<phone_core::tones::Progress> {
    use phone_core::tones::Progress;
    let code: u16 = param.split_whitespace().next().and_then(|c| c.parse().ok()).unwrap_or(0);
    match code {
        486 | 600 => Some(Progress::Busy),
        404 | 408 | 410 | 480 | 484 | 488 | 500..=599 | 604 => Some(Progress::Congestion),
        _ => None,
    }
}

/// phoned's handle on its baresip.
pub struct Sip {
    session: zbus::Connection,
    /// The bus connection of the baresip phoned started.
    owner: Mutex<Option<String>>,
    pub status: Mutex<String>,
    child: Mutex<Option<tokio::process::Child>>,
}

impl Sip {
    pub fn new(session: zbus::Connection) -> Self {
        Self { session, owner: Mutex::new(None), status: Mutex::new(String::new()), child: Mutex::new(None) }
    }

    pub fn status(&self) -> String {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn registered(&self) -> bool {
        self.status() == "registered"
    }

    fn set_status(&self, s: &str) {
        if let Ok(mut st) = self.status.lock() {
            *st = s.to_string();
        }
    }

    async fn proxy(&self) -> zbus::Result<BaresipProxy<'static>> {
        let owner = self.owner.lock().ok().and_then(|o| o.clone()).ok_or(zbus::Error::Failure("SIP is off".into()))?;
        BaresipProxy::builder(&self.session).destination(owner).build().await
    }

    pub async fn invoke(&self, cmd: &str) -> zbus::Result<String> {
        self.proxy().await?.invoke(cmd).await
    }

    /// Act on one call: baresip's commands work on the current call, so the call
    /// is chosen first.
    pub async fn on_call(&self, id: &str, cmd: &str) -> zbus::Result<String> {
        self.invoke(&format!("callfind {id}")).await?;
        self.invoke(cmd).await
    }

    /// Dial, and return baresip's id for the call.
    pub async fn dial(&self, uri: &str) -> zbus::Result<String> {
        let out = self.invoke(&format!("dial {uri}")).await?;
        out.lines()
            .find_map(|l| l.trim().strip_prefix("call id: "))
            .map(|s| s.trim().to_string())
            .ok_or_else(|| zbus::Error::Failure(format!("SIP did not place the call: {}", out.trim())))
    }

    pub fn stop(&self) {
        if let Ok(mut c) = self.child.lock() {
            if let Some(mut child) = c.take() {
                let _ = child.start_kill();
            }
        }
        if let Ok(mut o) = self.owner.lock() {
            *o = None;
        }
    }
}

/// Run baresip while SIP is on, restart it when it stops or the account changes,
/// and pass its events to the phone. Runs for phoned's lifetime.
pub async fn run(phone: Arc<Phone>, changed: Arc<tokio::sync::Notify>) {
    loop {
        let enabled = phone.flag(phone_api::keys::SIP_ENABLED);
        let account = phone.setting(phone_api::keys::SIP_ACCOUNT);
        if !enabled || !valid_account(&account) {
            phone.sip.stop();
            let s = if enabled { "failed: no valid account" } else { "" };
            if phone.sip.status() != s {
                phone.sip.set_status(s);
                phone.status_changed().await;
            }
            changed.notified().await;
            continue;
        }
        match start(&phone, &account).await {
            Ok(child_pid) => {
                let settings_changed = tokio::select! {
                    _ = follow(&phone, child_pid) => {
                        tracing::warn!("baresip stopped; starting it again");
                        false
                    }
                    _ = changed.notified() => {
                        tracing::info!("SIP settings changed");
                        true
                    }
                };
                phone.sip.stop();
                phone.sip_lost().await;
                if settings_changed {
                    // Straight on with the new settings (or to off).
                    continue;
                }
            }
            Err(e) => {
                tracing::warn!("SIP: {e}");
                phone.sip.set_status(&format!("failed: {e}"));
                phone.status_changed().await;
            }
        }
        // Do not spin when it keeps failing.
        let _ = tokio::time::timeout(Duration::from_secs(10), changed.notified()).await;
    }
}

async fn start(phone: &Arc<Phone>, account: &str) -> Result<u32, String> {
    let d = dir();
    std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700));
    let password = std::fs::read_to_string(password_file()).unwrap_or_default();
    // A local test account (sip:me@127.0.0.1) has nothing to register with.
    let local = domain(account).starts_with("127.") || domain(account).starts_with("localhost");
    let listen = std::env::var("DOLPHONE_SIP_LISTEN").unwrap_or_else(|_| "0.0.0.0:5060".into());
    std::fs::write(d.join("config"), config(&listen)).map_err(|e| e.to_string())?;
    let accounts = d.join("accounts");
    let proxy = phone.setting(phone_api::keys::SIP_PROXY);
    std::fs::write(&accounts, account_line(account, password.trim(), &proxy, !local)).map_err(|e| e.to_string())?;
    std::fs::set_permissions(&accounts, std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;

    // One left running by a phoned that crashed holds the port and the bus name.
    let _ = std::process::Command::new("pkill").arg("-f").arg(format!("^baresip -f {}$", d.display())).status();
    let child = tokio::process::Command::new("baresip")
        .arg("-f")
        .arg(&d)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| "baresip is not installed".to_string())?;
    let pid = child.id().ok_or("baresip exited at once")?;
    if let Ok(mut c) = phone.sip.child.lock() {
        *c = Some(child);
    }
    phone.sip.set_status(if local { "registered" } else { "registering" });

    // Wait for it on the bus, and make sure it is ours.
    let dbus = zbus::fdo::DBusProxy::new(&phone.session).await.map_err(|e| e.to_string())?;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let Ok(owner) = dbus.get_name_owner(NAME.try_into().expect("name")).await else { continue };
        let theirs = dbus.get_connection_unix_process_id(owner.clone().into()).await.unwrap_or(0);
        if theirs != pid {
            return Err("another program holds baresip's bus name".into());
        }
        if let Ok(mut o) = phone.sip.owner.lock() {
            *o = Some(owner.to_string());
        }
        tracing::info!(account = %account.split('@').nth(1).unwrap_or(""), "SIP running");
        phone.status_changed().await;
        return Ok(pid);
    }
    Err("baresip did not come up on the bus".into())
}

/// Pass baresip's events on until it goes away.
async fn follow(phone: &Arc<Phone>, pid: u32) {
    let p = match phone.sip.proxy().await {
        Ok(p) => p,
        Err(e) => return tracing::warn!("SIP: no proxy for baresip: {e}"),
    };
    let mut events = match p.receive_event().await {
        Ok(ev) => ev,
        Err(e) => return tracing::warn!("SIP: could not follow baresip's events: {e}"),
    };
    tracing::debug!(dest = ?p.inner().destination(), "following baresip's events");
    let mut texts = match p.receive_message().await {
        Ok(m) => m,
        Err(e) => return tracing::warn!("SIP: could not follow baresip's messages: {e}"),
    };
    let mut alive = tokio::time::interval(Duration::from_secs(5));
    loop {
        tokio::select! {
            Some(sig) = events.next() => {
                let Ok(a) = sig.args() else {
                    tracing::debug!("a baresip event that did not parse");
                    continue;
                };
                tracing::debug!(kind = a.kind, "baresip event");
                match a.kind {
                    "REGISTER_OK" => {
                        phone.sip.set_status("registered");
                        phone.status_changed().await;
                    }
                    "REGISTER_FAIL" => {
                        phone.sip.set_status(&format!("failed: {}", a.param.chars().take(60).collect::<String>()));
                        phone.status_changed().await;
                    }
                    _ => {
                        if let Some(e) = parse_event(a.param) {
                            phone.sip_event(e).await;
                        }
                    }
                }
            }
            Some(sig) = texts.next() => {
                if let Ok(a) = sig.args() {
                    if a.ctype.starts_with("text/plain") {
                        phone.sip_text(a.peer, a.body).await;
                    }
                }
            }
            _ = alive.tick() => {
                if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accounts_are_checked() {
        assert!(valid_account("sip:alex@sip.example.com"));
        assert!(valid_account("sip:61491570156@voip.example.net:5061;transport=tls"));
        assert!(!valid_account("sip:alex@example.com>;auth_pass=x"), "nothing that edits the account line");
        assert!(!valid_account("sip:alex@example.com;outbound=evil"));
        assert!(!valid_account("alex@example.com"));
        assert!(!valid_account("sip:@example.com"));
        assert!(!valid_account("sip:alex@exa\nmple.com"));
        assert!(valid_proxy("sip:proxy.example.net:5061;transport=tls"));
        assert!(!valid_proxy("sip:user@proxy.example.net"));
        assert!(!valid_proxy("sip:proxy\";x"));
    }

    #[test]
    fn numbers_and_uris() {
        assert_eq!(number_of("sip:+61491570156@voip.example.net"), "+61491570156");
        assert_eq!(number_of("<sip:0491570156@10.0.0.1;user=phone>"), "0491570156");
        assert_eq!(number_of("sip:alex@example.com"), "sip:alex@example.com");
        assert_eq!(
            uri_for("+61491570156", "sip:me@voip.example.net;transport=tls"),
            "sip:+61491570156@voip.example.net"
        );
        assert_eq!(uri_for("sip:alex@example.com", "sip:me@x"), "sip:alex@example.com");
    }

    #[test]
    fn events_and_refusals() {
        let e = parse_event(r#"{"type":"CALL_INCOMING","class":"call","direction":"incoming","peeruri":"sip:+61491570156@x","id":"ab12","param":"sip:+61491570156@x"}"#).unwrap();
        assert_eq!((e.kind.as_str(), e.id.as_str(), e.incoming), ("CALL_INCOMING", "ab12", true));
        use phone_core::tones::Progress;
        assert_eq!(refusal("486 Busy Here"), Some(Progress::Busy));
        assert_eq!(refusal("404 Not Found"), Some(Progress::Congestion));
        assert_eq!(refusal("Connection reset by peer"), None);
    }

    #[test]
    fn the_password_cannot_change_the_account_line() {
        let l = account_line("sip:me@x", "pw;regint=0>\nsip:evil", "", true);
        assert!(!l.contains('\n'));
        assert_eq!(l.matches(';').count(), 3, "only phoned's own parameters: {l}");
    }
}
