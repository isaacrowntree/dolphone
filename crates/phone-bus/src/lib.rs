//! A front end's connection to phoned.
//!
//! A UI thread must never wait on the bus, so every front end runs one worker
//! thread with its own small runtime. The worker subscribes to everything phoned
//! announces, runs the calls the UI asks for, and hands both back as [`Event`]s
//! through the callback it was started with; a Slint app wraps that callback in
//! `slint::invoke_from_event_loop` and handles events on its own thread.
//!
//! The worker survives phoned restarting: it watches phoned's bus name and sends
//! [`Event::Connected`] each time it comes and goes, which is a front end's cue to
//! reload what it shows.

pub mod files;
pub mod haptics;

use std::future::Future;
use std::pin::Pin;

use futures_util::StreamExt;
use phone_api::*;
use tokio::sync::mpsc;

/// Everything a front end hears.
#[derive(Clone, Debug)]
pub enum Event {
    /// phoned is (or is no longer) on the bus.
    Connected(bool),
    Calls(Vec<CallInfo>),
    CallAdded(CallInfo),
    CallChanged(CallInfo),
    CallRemoved(u32),
    Recents(Vec<CallRecordInfo>),
    Missed(u32),
    Threads(Vec<ThreadInfo>),
    Conversation(String, Vec<MessageInfo>),
    MessageAdded(MessageInfo),
    MessageChanged(i64, u8),
    Unread(u32),
    Pending(Vec<Request>),
    Requested(Request),
    Resolved(u32, bool),
    Status(ModemStatus),
    Routes(Vec<RouteInfo>),
    Route(u8),
    Ringer(String),
    Devices(Vec<BtDevice>),
    Bluetooth {
        available: bool,
        powered: bool,
    },
    Passkey(String, u32),
    Blocked(Vec<String>),
    Setting(String, String),
    /// A dial or send went through (`id`) or is waiting (`pending`).
    Placed {
        kind: &'static str,
        placed: Placed,
    },
    Ussd(String),
    /// An emergency alert arrived.
    Alert(AlertInfo),
    Alerts(Vec<AlertInfo>),
    Grants(Vec<Grant>),
    Audit(Vec<AuditEntry>),
    /// The address book changed (a contact added, edited, removed or imported).
    ContactsChanged,
    /// A contacts import finished: added, merged, already here, and an error if
    /// it failed.
    ContactsImported {
        added: u32,
        merged: u32,
        unchanged: u32,
        error: String,
    },
    /// The SIM PIN was tried: 0 when it unlocked, otherwise the tries left.
    Unlock(u32),
    /// Laptops linked over KDE Connect: (id, name, paired).
    Laptops(Vec<(String, String, bool)>),
    /// Texts found by a search, newest first.
    Found(String, Vec<MessageInfo>),
    /// The device profile phoned is using.
    Device(DeviceInfo),
    /// The modem's one-time setup: (step, state, in words). `applied` when it was
    /// just made rather than checked.
    ModemSetup {
        steps: Vec<(String, String, String)>,
        applied: bool,
    },
    /// Something a job finished, to tell the owner in passing.
    Notice(String),
    /// A call the UI made failed; the text is phoned's message.
    Error(String),
}

/// The proxies a job gets, all on the one connection.
#[derive(Clone)]
pub struct Clients {
    pub calls: CallsProxy<'static>,
    pub messages: MessagesProxy<'static>,
    pub confirm: ConfirmProxy<'static>,
    pub audio: AudioProxy<'static>,
    pub bluetooth: BluetoothProxy<'static>,
    pub modem: ModemProxy<'static>,
    pub settings: SettingsProxy<'static>,
    pub contacts: ContactsProxy<'static>,
}

type Job = Box<dyn FnOnce(Clients) -> Pin<Box<dyn Future<Output = Vec<Event>> + Send>> + Send>;

/// The handle a front end keeps.
#[derive(Clone)]
pub struct Bus {
    jobs: mpsc::UnboundedSender<Job>,
}

impl Bus {
    /// Start the worker. `deliver` is called on the worker thread for every event.
    pub fn start(deliver: impl Fn(Event) + Send + Sync + 'static) -> Self {
        let (tx, rx) = mpsc::unbounded_channel::<Job>();
        std::thread::Builder::new()
            .name("phone-bus".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
                rt.block_on(worker(rx, std::sync::Arc::new(deliver)));
            })
            .expect("spawn phone-bus");
        Self { jobs: tx }
    }

    /// Run `f` against phoned and deliver what it returns. Errors become
    /// [`Event::Error`].
    /// Run blocking work (a child process waiting on a password prompt) off the
    /// bus's own thread, from inside a job.
    pub async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
        tokio::task::spawn_blocking(f).await.ok()
    }

    /// Wait, inside a job.
    pub async fn pause(d: std::time::Duration) {
        tokio::time::sleep(d).await
    }

    pub fn run<F, Fut>(&self, f: F)
    where
        F: FnOnce(Clients) -> Fut + Send + 'static,
        Fut: Future<Output = zbus::Result<Vec<Event>>> + Send + 'static,
    {
        let job: Job = Box::new(move |c| {
            Box::pin(async move {
                match f(c).await {
                    Ok(events) => events,
                    Err(e) => vec![Event::Error(message(&e))],
                }
            })
        });
        let _ = self.jobs.send(job);
    }

    // The calls every front end makes, so none of them repeats the plumbing.

    pub fn refresh_calls(&self) {
        self.run(|c| async move { Ok(vec![Event::Calls(c.calls.calls().await?)]) });
    }

    pub fn refresh_recents(&self, limit: u32) {
        self.run(move |c| async move {
            Ok(vec![Event::Recents(c.calls.recents(limit).await?), Event::Missed(c.calls.missed().await?)])
        });
    }

    pub fn refresh_threads(&self) {
        self.run(|c| async move {
            Ok(vec![Event::Threads(c.messages.threads().await?), Event::Unread(c.messages.unread().await?)])
        });
    }

    pub fn load_conversation(&self, peer: String) {
        self.run(move |c| async move {
            let m = c.messages.conversation(&peer).await?;
            Ok(vec![Event::Conversation(peer, m)])
        });
    }

    pub fn refresh_pending(&self) {
        self.run(|c| async move { Ok(vec![Event::Pending(c.confirm.pending().await?)]) });
    }

    pub fn refresh_status(&self) {
        self.run(|c| async move { Ok(vec![Event::Status(c.modem.status().await?)]) });
    }

    pub fn refresh_routes(&self) {
        self.run(
            |c| async move { Ok(vec![Event::Routes(c.audio.routes().await?), Event::Route(c.audio.route().await?)]) },
        );
    }

    pub fn dial(&self, number: String) {
        self.run(
            move |c| async move { Ok(vec![Event::Placed { kind: "call", placed: c.calls.dial(&number).await? }]) },
        );
    }

    pub fn send(&self, to: String, body: String) {
        self.run(move |c| async move {
            Ok(vec![Event::Placed { kind: "sms", placed: c.messages.send(&to, &body).await? }])
        });
    }
}

async fn worker(mut jobs: mpsc::UnboundedReceiver<Job>, deliver: std::sync::Arc<dyn Fn(Event) + Send + Sync>) {
    let conn = loop {
        match zbus::Connection::session().await {
            Ok(c) => break c,
            Err(_) => tokio::time::sleep(std::time::Duration::from_secs(2)).await,
        }
    };
    let clients = match clients(&conn).await {
        Ok(c) => c,
        Err(e) => {
            deliver(Event::Error(message(&e)));
            return;
        }
    };
    {
        let deliver = deliver.clone();
        let (c, conn) = (clients.clone(), conn.clone());
        tokio::spawn(async move { listen(conn, c, deliver).await });
    }
    while let Some(job) = jobs.recv().await {
        let (c, d) = (clients.clone(), deliver.clone());
        tokio::spawn(async move {
            for e in job(c).await {
                d(e);
            }
        });
    }
}

async fn clients(conn: &zbus::Connection) -> zbus::Result<Clients> {
    // Properties are read when asked, not cached: phoned can restart under us, and a
    // cache would keep answering for the old one.
    macro_rules! proxy {
        ($t:ident) => {
            $t::builder(conn).cache_properties(zbus::proxy::CacheProperties::No).build().await?
        };
    }
    Ok(Clients {
        calls: proxy!(CallsProxy),
        messages: proxy!(MessagesProxy),
        confirm: proxy!(ConfirmProxy),
        audio: proxy!(AudioProxy),
        bluetooth: proxy!(BluetoothProxy),
        modem: proxy!(ModemProxy),
        settings: proxy!(SettingsProxy),
        contacts: proxy!(ContactsProxy),
    })
}

async fn listen(conn: zbus::Connection, c: Clients, deliver: std::sync::Arc<dyn Fn(Event) + Send + Sync>) {
    let Ok(dbus) = zbus::fdo::DBusProxy::new(&conn).await else { return };
    let (
        Ok(mut owner),
        Ok(mut added),
        Ok(mut changed),
        Ok(mut removed),
        Ok(mut text),
        Ok(mut text_state),
        Ok(mut asked),
        Ok(mut answered),
        Ok(mut status),
        Ok(mut routes),
        Ok(mut devices),
        Ok(mut passkey),
    ) = (
        dbus.receive_name_owner_changed_with_args(&[(0, SERVICE)]).await,
        c.calls.receive_call_added().await,
        c.calls.receive_call_changed().await,
        c.calls.receive_call_removed().await,
        c.messages.receive_message_added().await,
        c.messages.receive_message_changed().await,
        c.confirm.receive_requested().await,
        c.confirm.receive_resolved().await,
        c.modem.receive_status_changed().await,
        c.audio.receive_routes_changed().await,
        c.bluetooth.receive_devices_changed().await,
        c.bluetooth.receive_passkey().await,
    )
    else {
        deliver(Event::Error("could not subscribe to phoned".into()));
        return;
    };
    let mut missed = c.calls.receive_missed_changed().await;
    let mut unread = c.messages.receive_unread_changed().await;
    let mut route = c.audio.receive_route_changed().await;
    // The ringer is changed from more than one place (the Dialer's PTT hold, either
    // settings app), and every app that buzzes has to know when it went silent.
    let mut ringer = c.audio.receive_ringer_changed().await;
    let (Ok(mut alerts), Ok(mut book), Ok(mut imported)) = (
        c.messages.receive_alert_received().await,
        c.contacts.receive_changed().await,
        c.contacts.receive_imported().await,
    ) else {
        deliver(Event::Error("could not subscribe to phoned".into()));
        return;
    };

    let up = dbus.name_has_owner(zbus::names::BusName::try_from(SERVICE).expect("name")).await.unwrap_or(false);
    deliver(Event::Connected(up));
    loop {
        let e = tokio::select! {
            Some(s) = owner.next() => match s.args() {
                Ok(a) => Event::Connected(a.new_owner().is_some()),
                Err(_) => continue,
            },
            Some(s) = added.next() => match s.args() { Ok(a) => Event::CallAdded(a.call), Err(_) => continue },
            Some(s) = changed.next() => match s.args() { Ok(a) => Event::CallChanged(a.call), Err(_) => continue },
            Some(s) = removed.next() => match s.args() { Ok(a) => Event::CallRemoved(a.id), Err(_) => continue },
            Some(s) = text.next() => match s.args() { Ok(a) => Event::MessageAdded(a.message), Err(_) => continue },
            Some(s) = text_state.next() => match s.args() { Ok(a) => Event::MessageChanged(a.id, a.state), Err(_) => continue },
            Some(s) = asked.next() => match s.args() { Ok(a) => Event::Requested(a.request), Err(_) => continue },
            Some(s) = answered.next() => match s.args() { Ok(a) => Event::Resolved(a.id, a.confirmed), Err(_) => continue },
            Some(s) = status.next() => match s.args() { Ok(a) => Event::Status(a.status), Err(_) => continue },
            Some(_) = routes.next() => match c.audio.routes().await { Ok(r) => Event::Routes(r), Err(_) => continue },
            Some(_) = devices.next() => match c.bluetooth.devices().await { Ok(d) => Event::Devices(d), Err(_) => continue },
            Some(s) = passkey.next() => match s.args() { Ok(a) => Event::Passkey(a.address.to_string(), a.passkey), Err(_) => continue },
            Some(p) = missed.next() => match p.get().await { Ok(n) => Event::Missed(n), Err(_) => continue },
            Some(p) = unread.next() => match p.get().await { Ok(n) => Event::Unread(n), Err(_) => continue },
            Some(p) = route.next() => match p.get().await { Ok(n) => Event::Route(n), Err(_) => continue },
            Some(p) = ringer.next() => match p.get().await { Ok(r) => Event::Ringer(r), Err(_) => continue },
            Some(s) = alerts.next() => match s.args() { Ok(a) => Event::Alert(a.alert), Err(_) => continue },
            Some(_) = book.next() => Event::ContactsChanged,
            Some(s) = imported.next() => match s.args() {
                Ok(a) => Event::ContactsImported { added: a.added, merged: a.merged, unchanged: a.unchanged, error: a.error.to_string() },
                Err(_) => continue,
            },
            else => break,
        };
        deliver(e);
    }
}

/// phoned's own words for an error, without the D-Bus error name in front.
pub fn message(e: &zbus::Error) -> String {
    match e {
        zbus::Error::MethodError(name, msg, _) => {
            if name.as_str() == "org.freedesktop.DBus.Error.ServiceUnknown" {
                "The phone service is not running".into()
            } else {
                msg.clone().unwrap_or_else(|| name.to_string())
            }
        }
        zbus::Error::FDO(f) => {
            f.to_string().split_once(": ").map(|(_, m)| m.to_string()).unwrap_or_else(|| f.to_string())
        }
        other => other.to_string(),
    }
}
