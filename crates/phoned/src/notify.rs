//! Desktop notifications, for desktop mode.
//!
//! When a notification server is on the session bus (a desktop is running on the
//! external display), phoned posts the phone's events there with the actions a
//! phone has: Answer and Decline on a call, Reply on a text, Allow and Deny on a
//! confirmation. On the bare panel there is no server and every call here is a
//! no-op, so nothing else in phoned has to know which mode it is in.

use std::collections::HashMap;
use std::sync::Arc;

use futures_util::StreamExt;
use tokio::sync::Mutex;
use zbus::proxy;
use zbus::wire::Variant;

use crate::caller::Caller;
use crate::phone::Phone;

#[proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
pub trait Notifications {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: HashMap<&str, Variant<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;
    fn close_notification(&self, id: u32) -> zbus::Result<()>;
    fn get_capabilities(&self) -> zbus::Result<Vec<String>>;
    #[zbus(signal)]
    fn action_invoked(&self, id: u32, action_key: String) -> zbus::Result<()>;
    /// KDE's inline reply: the text typed into the notification itself.
    #[zbus(signal)]
    fn notification_replied(&self, id: u32, text: String) -> zbus::Result<()>;
}

/// What a notification is about, to act on its buttons.
#[derive(Clone, Debug, PartialEq, Eq)]
enum About {
    Call(u32),
    Message(String),
    Missed(String),
    Request(u32),
    Alert(String),
}

pub struct Notifier {
    conn: zbus::Connection,
    posted: Mutex<HashMap<u32, About>>,
}

const APP: &str = "Dolphone";

/// The notification server's unique bus name, if it is a program installed with the
/// system (under /usr, owned by root).
///
/// Anything on the session bus may claim `org.freedesktop.Notifications`, and on a
/// bare panel nothing else does. An impostor would be sent every notification phoned
/// posts and could press its buttons, so phoned only talks to, and only listens to, a
/// server it can vouch for.
async fn verified_server(conn: &zbus::Connection) -> Option<String> {
    let dbus = zbus::fdo::DBusProxy::new(conn).await.ok()?;
    let name = zbus::names::BusName::try_from("org.freedesktop.Notifications").ok()?;
    let owner = dbus.get_name_owner(name).await.ok()?.to_string();
    let exe = crate::caller::exe_of(conn, &owner).await?;
    if crate::caller::system_program(&exe) {
        Some(owner)
    } else {
        tracing::warn!(server = %exe.display(), "notification server is not a system program; not using it");
        None
    }
}

/// How loud a notification is: its urgency (2 critical), its category, and how long
/// it stays (0 until dealt with, -1 the server's default).
#[derive(Clone, Copy)]
struct Style {
    urgency: u8,
    category: &'static str,
    timeout: i32,
    icon: &'static str,
}

// Categories are the Desktop Notifications spec's own, so desktops style them as
// calls and messages; the confirmation has none, so it gets a vendor one.
const RINGING: Style = Style { urgency: 2, category: "call.incoming", timeout: 0, icon: "call-start" };
const MISSED: Style = Style { urgency: 1, category: "call.unanswered", timeout: -1, icon: "call-missed" };
const MESSAGE: Style = Style { urgency: 1, category: "im.received", timeout: -1, icon: "mail-message-new" };
const ALERT: Style = Style { urgency: 2, category: "x-dolphone.alert", timeout: 0, icon: "dialog-warning" };
const CONFIRM: Style = Style { urgency: 2, category: "x-dolphone.confirm", timeout: 0, icon: "dialog-warning" };

impl Notifier {
    pub fn new(conn: zbus::Connection) -> Self {
        Self { conn, posted: Mutex::new(HashMap::new()) }
    }

    async fn proxy(&self) -> Option<NotificationsProxy<'static>> {
        let owner = verified_server(&self.conn).await?;
        NotificationsProxy::builder(&self.conn).destination(owner).build().await.ok()
    }

    async fn post(&self, about: About, style: Style, summary: &str, body: &str, actions: &[&str]) {
        let Style { urgency, category, timeout, icon } = style;
        let Some(p) = self.proxy().await else { return };
        let replaces = self.posted.lock().await.iter().find(|(_, a)| **a == about).map(|(id, _)| *id).unwrap_or(0);
        let hints = HashMap::from([
            ("urgency", Variant::from(urgency)),
            ("category", Variant::from(category)),
            ("desktop-entry", Variant::from("io.github.isaacrowntree.Dolphone")),
            // A ringing call stays until it is dealt with.
            ("resident", Variant::from(urgency == 2)),
        ]);
        match p.notify(APP, replaces, icon, summary, body, actions, hints, timeout).await {
            Ok(id) => {
                self.posted.lock().await.insert(id, about);
            }
            Err(e) => tracing::debug!("notify: {e}"),
        }
    }

    async fn withdraw(&self, about: About) {
        let ids: Vec<u32> = {
            let mut posted = self.posted.lock().await;
            let ids: Vec<u32> = posted.iter().filter(|(_, a)| **a == about).map(|(id, _)| *id).collect();
            for id in &ids {
                posted.remove(id);
            }
            ids
        };
        if let Some(p) = self.proxy().await {
            for id in ids {
                let _ = p.close_notification(id).await;
            }
        }
    }

    pub async fn incoming_call(&self, id: u32, name: &str, number: &str) {
        let body = if name == number || number.is_empty() { String::new() } else { number.to_string() };
        let actions = ["answer", "Answer", "decline", "Decline", "default", "Open"];
        self.post(About::Call(id), RINGING, &format!("Incoming call: {name}"), &body, &actions).await;
    }

    pub async fn withdraw_call(&self, id: u32) {
        self.withdraw(About::Call(id)).await;
    }

    pub async fn missed_call(&self, name: &str, number: &str) {
        let actions = ["callback", "Call back", "text", "Text", "default", "Open"];
        self.post(About::Missed(number.to_string()), MISSED, &format!("Missed call: {name}"), "", &actions).await;
    }

    pub async fn message(&self, name: &str, from: &str, body: &str, private: bool) {
        let Some(p) = self.proxy().await else { return };
        let inline = p.get_capabilities().await.unwrap_or_default().iter().any(|c| c == "inline-reply");
        let mut actions = vec!["default", "Open"];
        if inline {
            actions.extend(["inline-reply", "Reply"]);
        } else {
            actions.extend(["reply", "Reply"]);
        }
        let body = if private { "New message" } else { body };
        self.post(About::Message(from.to_string()), MESSAGE, name, body, &actions).await;
    }

    pub async fn confirmation(&self, r: &phone_api::Request, name: &str) {
        let what = match r.kind.as_str() {
            "call" => format!("{} wants to call {name}", r.requester),
            "sms" => format!("{} wants to text {name}", r.requester),
            "wipe" => "Delete all messages and calls?".to_string(),
            "modem" => format!("{} wants to set up the modem", r.requester),
            "grant" => format!("{} wants to text {} without asking", r.requester, r.target),
            "contact" => format!("{} wants to {} in your contacts", r.requester, r.target),
            "setting" => format!("{} wants to set {}", r.requester, r.target),
            _ => format!("{} wants to use the phone", r.requester),
        };
        let body = match r.kind.as_str() {
            "sms" => r.body.clone(),
            "contact" => "A contact decides who a caller shows as".into(),
            "setting" => format!("to {}", r.body),
            _ => r.reason.clone(),
        };
        // No Allow button: a notification is a weaker channel than the device (any
        // server could fake a press), so allowing happens on the panel or in the
        // desktop app, which Review opens.
        self.post(About::Request(r.id), CONFIRM, &what, &body, &["review", "Review", "deny", "Deny"]).await;
    }

    /// A warning about the phone itself (the network fell back to 2G).
    pub async fn warning(&self, summary: &str, body: &str) {
        self.post(About::Alert(summary.to_string()), MISSED, summary, body, &["default", "Open"]).await;
    }

    /// An emergency alert. Its text is always shown: it is public, and it matters.
    pub async fn alert(&self, label: &str, body: &str) {
        self.post(About::Alert(body.to_string()), ALERT, label, body, &["default", "Open"]).await;
    }

    pub async fn withdraw_request(&self, id: u32) {
        self.withdraw(About::Request(id)).await;
    }

    async fn about(&self, id: u32) -> Option<About> {
        self.posted.lock().await.get(&id).cloned()
    }
}

/// Act on notification buttons. Runs for phoned's lifetime; a desktop that starts
/// later is picked up because the match is on the signal, not on a proxy to a
/// particular owner.
pub async fn listen(phone: Arc<Phone>) {
    let Ok(p) =
        NotificationsProxy::builder(&phone.session).cache_properties(zbus::proxy::CacheProperties::No).build().await
    else {
        return;
    };
    let (Ok(mut actions), Ok(mut replies)) = (p.receive_action_invoked().await, p.receive_notification_replied().await)
    else {
        return;
    };
    // The owner pressing a button in their own desktop session is the owner
    // confirming, so it carries our own front end's trust.
    let owner = Caller { name: "dolphone-desktop".into(), trusted: true, reader: true };
    let from_server = |sender: Option<String>, conn: zbus::Connection| async move {
        let Some(sender) = sender else { return false };
        verified_server(&conn).await.is_some_and(|v| v == sender)
    };
    loop {
        tokio::select! {
            Some(sig) = actions.next() => {
                let sender = sig.message().header().sender().map(|s| s.to_string());
                if !from_server(sender, phone.session.clone()).await {
                    continue;
                }
                let Ok(a) = sig.args() else { continue };
                let Some(about) = phone.notify.about(a.id).await else { continue };
                act(&phone, about, &a.action_key, &owner).await;
            }
            Some(sig) = replies.next() => {
                let sender = sig.message().header().sender().map(|s| s.to_string());
                if !from_server(sender, phone.session.clone()).await {
                    continue;
                }
                let Ok(a) = sig.args() else { continue };
                if let Some(About::Message(peer)) = phone.notify.about(a.id).await {
                    if let Err(e) = phone.send(&peer, &a.text, &owner, false).await {
                        tracing::warn!("inline reply: {e}");
                    }
                }
            }
            else => break,
        }
    }
}

async fn act(phone: &Arc<Phone>, about: About, action: &str, owner: &Caller) {
    match (about, action) {
        (About::Call(id), "answer") => {
            let _ = phone.answer(id).await;
            crate::launch::desktop(&["--call"]);
        }
        (About::Call(id), "decline") => {
            let _ = phone.hangup(id).await;
        }
        (About::Call(_), _) => crate::launch::desktop(&["--call"]),
        (About::Missed(n), "callback") => {
            let _ = phone.dial(&n, owner, false).await;
            crate::launch::desktop(&["--call"]);
        }
        (About::Missed(n), "text") | (About::Message(n), "reply") => crate::launch::desktop(&["--compose", &n]),
        (About::Missed(_), _) => crate::launch::desktop(&["--recents"]),
        (About::Message(n), _) => crate::launch::desktop(&["--thread", &n]),
        (About::Request(id), "deny") => {
            let _ = phone.deny(id).await;
        }
        (About::Request(_), _) => crate::launch::desktop(&["--call"]),
        (About::Alert(_), _) => crate::launch::desktop(&["--alerts"]),
    }
}
