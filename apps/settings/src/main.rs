//! Dolphone's Phone Settings, a FlipCTL app.

mod model;

use std::cell::RefCell;
use std::time::{Duration, Instant};

use flipctl_app::keyboard::{Exit, TextInput};
use flipctl_app::{font, Key, StatusSource};
use model::{
    Action, Field, Input, MainRow, Model, Page, PrivacyRow, SipRow, ALERT_FLAGS, CALL_FLAGS, MAIN, PRIVACY, SIP,
    TEXT_FLAGS,
};
use phone_bus::{Bus, Event};
use slint::{ModelRc, SharedString, VecModel};

slint::include_modules!();

struct App {
    ui: AppWindow,
    model: Model,
    bus: Bus,
    frame: u32,
    /// The keyboard, while a setting is being typed.
    kb: Option<(Field, TextInput)>,
    /// Whether the window had the keyboard at the last tick, and when the
    /// Bluetooth list was last asked for.
    focused: bool,
    bt_asked: Instant,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn with_app(f: impl FnOnce(&mut App)) {
    APP.with(|a| {
        if let Some(app) = a.borrow_mut().as_mut() {
            f(app);
        }
    });
}

fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    let bus = Bus::start(|e| {
        let _ = slint::invoke_from_event_loop(move || with_app(|app| app.event(e)));
    });
    APP.with(|a| *a.borrow_mut() = Some(App::new(ui.clone_strong(), bus)));
    with_app(|app| {
        app.refresh(Page::Main);
        app.draw();
    });
    let mut status = StatusSource::new(Duration::from_secs(2));
    flipctl_app::apply_status!(&ui, PanelStatus, status.current());
    let tick = std::rc::Rc::new(slint::Timer::default());
    let timer = std::rc::Rc::downgrade(&tick);
    tick.start(slint::TimerMode::Repeated, KEYBOARD_TICK, move || {
        with_app(|app| {
            app.frame = app.frame.wrapping_add(1);
            let focused = window_focused(&app.ui);
            let refocused = focused && !app.focused;
            app.focused = focused;
            if !focused {
                // In the background: nothing to show and nobody to show it to.
                if let Some(t) = timer.upgrade().filter(|t| t.interval() != IDLE_TICK) {
                    t.set_interval(IDLE_TICK);
                }
                return;
            }
            if let Some(now) = status.poll() {
                flipctl_app::apply_status!(&app.ui, PanelStatus, now);
            }
            // A Bluetooth scan fills the list as it goes.
            if app.model.page == Page::Bluetooth && app.bt_asked.elapsed() >= Duration::from_secs(2) {
                app.bt_asked = Instant::now();
                app.refresh(Page::Bluetooth);
            }
            // Keys and phoned's answers draw as they come; the tick draws only the
            // keyboard, whose caret blinks and whose keys animate, five times a
            // second, and otherwise wakes once a second for the status bar.
            if app.kb.is_some() || refocused {
                app.draw();
            }
            let want = if app.kb.is_some() { KEYBOARD_TICK } else { IDLE_TICK };
            if let Some(t) = timer.upgrade().filter(|t| t.interval() != want) {
                t.set_interval(want);
            }
        });
    });
    // A key can open the keyboard, which wants the quick tick straight away.
    let pace = std::rc::Rc::downgrade(&tick);
    ui.on_keyed(move |text, down| {
        let mut want = false;
        with_app(|app| {
            app.key(text.as_str(), down);
            want = app.kb.is_some();
        });
        if let Some(t) = pace.upgrade().filter(|t| want && t.interval() != KEYBOARD_TICK) {
            t.set_interval(KEYBOARD_TICK);
        }
    });
    ui.run()
}

/// The tick while the keyboard is up (its caret blinks on a 1 s cycle), and
/// otherwise.
const KEYBOARD_TICK: Duration = Duration::from_millis(200);
const IDLE_TICK: Duration = Duration::from_secs(1);

/// Whether this window has the keyboard, which is how flipctl marks the app in
/// front. Slint's public API does not say; the window's own state does, and the
/// lock file pins the Slint it belongs to. A window never once told it is active
/// (a compositor that does not say) counts as in front, so nothing that used to
/// work goes quiet.
fn window_focused(ui: &AppWindow) -> bool {
    use slint::ComponentHandle;
    thread_local! {
        static EVER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    let active = slint::private_unstable_api::re_exports::WindowInner::from_pub(ui.window()).active();
    EVER.with(|ever| {
        ever.set(ever.get() || active);
        active || !ever.get()
    })
}

impl App {
    fn new(ui: AppWindow, bus: Bus) -> Self {
        Self { ui, model: Model::default(), bus, frame: 0, kb: None, focused: true, bt_asked: Instant::now() }
    }

    /// A key from the panel (or a real keyboard, which types into the keyboard).
    fn key(&mut self, text: &str, down: bool) {
        if self.kb.is_some() {
            self.keyboard(Key::from_slint(text), text, down);
            return;
        }
        let Some(k) = Key::from_slint(text) else { return };
        self.ui.set_pressed_slot(match (down, k.soft_slot()) {
            (true, Some(slot)) => slot as i32,
            _ => -1,
        });
        self.ui.set_arrow_pressed(match (down, k) {
            (true, Key::Left) => 1,
            (true, Key::Right) => 2,
            _ => 0,
        });
        if !down {
            self.draw();
            return;
        }
        let input = match k {
            Key::Up => Input::Up,
            Key::Down => Input::Down,
            Key::Left => Input::Left,
            Key::Right => Input::Right,
            Key::Ok => Input::Ok,
            Key::Escape => Input::Escape,
            Key::View => Input::View,
            Key::Edit => Input::Edit,
            Key::Run => Input::Run,
            _ => return,
        };
        let actions = self.model.key(input);
        self.act(actions);
        self.draw();
    }

    fn keyboard(&mut self, key: Option<Key>, text: &str, down: bool) {
        let Some((what, field)) = self.kb.as_mut() else { return };
        let what = *what;
        // Done takes a PIN only once it is one phoned could take.
        let valid = !what.is_pin() || model::valid_pin(&field.text);
        let done = match key {
            Some(k) if !down => {
                field.release(k.flipper());
                None
            }
            Some(k) => match field.key(k.flipper(), valid) {
                Some(Exit::Save(t)) => Some(Some(t)),
                Some(Exit::Cancel) => Some(None),
                None => None,
            },
            None if down => {
                if let Some(c) = text
                    .chars()
                    .next()
                    .filter(|c| text.chars().count() == 1 && !c.is_control())
                    .filter(|c| !what.is_pin() || (c.is_ascii_digit() && field.text.len() < 12))
                {
                    let at = field.text.char_indices().nth(field.cursor).map(|(i, _)| i).unwrap_or(field.text.len());
                    field.text.insert(at, c);
                    field.cursor += 1;
                }
                None
            }
            None => None,
        };
        if let Some(result) = done {
            self.kb = None;
            let a = self.model.written(what, result);
            self.act(a);
        }
        self.draw();
    }

    fn refresh(&self, page: Page) {
        match page {
            Page::Main => self.bus.run(|c| async move {
                let mut out = vec![Event::Ringer(c.audio.ringer().await?)];
                let keys = model::all_flags().map(|k| k.0).chain([
                    "region",
                    "voicemail",
                    "sip_account",
                    "sip_proxy",
                    "sip_password",
                    "sip_use",
                    "long_press_ms",
                    "uplink_alert_to",
                    "uplink_interface",
                    phone_api::keys::MESSAGES_PIN.0,
                    phone_api::keys::MESSAGES_OPEN_FOR.0,
                ]);
                for key in keys {
                    out.push(Event::Setting(key.into(), c.settings.get(key).await?));
                }
                out.push(Event::Routes(c.audio.routes().await?));
                // Powered is only asked of an adapter that is there: with BlueZ
                // running and no adapter it is an error, which would lose the
                // whole page.
                let available = c.bluetooth.available().await?;
                out.push(Event::Bluetooth { available, powered: available && c.bluetooth.powered().await? });
                out.push(Event::Blocked(c.settings.blocked().await?));
                out.push(Event::Status(c.modem.status().await?));
                Ok(out)
            }),
            Page::Audio => self.bus.refresh_routes(),
            Page::Bluetooth => self.bus.run(|c| async move {
                let available = c.bluetooth.available().await?;
                let mut out = vec![Event::Bluetooth { available, powered: available && c.bluetooth.powered().await? }];
                if available {
                    out.push(Event::Devices(c.bluetooth.devices().await?));
                }
                Ok(out)
            }),
            Page::Blocked => self.bus.run(|c| async move { Ok(vec![Event::Blocked(c.settings.blocked().await?)]) }),
            Page::Modem => {
                self.bus.refresh_status();
                self.bus.run(|c| async move {
                    Ok(vec![Event::ModemSetup { steps: c.modem.setup().await?, applied: false }])
                })
            }
            Page::About => self.bus.run(|c| async move { Ok(vec![Event::Device(c.settings.device().await?)]) }),
            Page::Privacy => self.bus.run(|c| async move {
                let mut out = vec![Event::Blocked(c.settings.blocked().await?)];
                for key in [phone_api::keys::MESSAGES_PIN.0, phone_api::keys::MESSAGES_OPEN_FOR.0] {
                    out.push(Event::Setting(key.into(), c.settings.get(key).await?));
                }
                Ok(out)
            }),
            Page::Calls | Page::Texts => {}
            Page::Sip => self.bus.refresh_status(),
            Page::Laptops => {
                self.bus.run(|c| async move { Ok(vec![Event::Laptops(c.settings.kdeconnect_devices().await?)]) })
            }
            Page::Alerts => self.bus.run(|c| async move { Ok(vec![Event::Alerts(c.messages.alerts(20).await?)]) }),
            Page::Grants => self.bus.run(|c| async move { Ok(vec![Event::Grants(c.settings.grants().await?)]) }),
            Page::Audit => self.bus.run(|c| async move { Ok(vec![Event::Audit(c.settings.audit(100).await?)]) }),
        }
    }

    fn event(&mut self, e: Event) {
        let m = &mut self.model;
        match e {
            Event::Connected(up) => {
                m.connected = up;
                if up {
                    self.refresh(self.model.page);
                }
            }
            Event::Ringer(r) => m.ringer = r,
            Event::Setting(k, v) => {
                m.setting(&k, &v);
            }
            Event::Alerts(a) => m.alerts = a,
            Event::Device(d) => m.device = Some(d),
            Event::ModemSetup { steps, applied: true } => m.setup_done(steps),
            Event::ModemSetup { steps, applied: false } => m.setup = steps,
            Event::Laptops(l) => m.laptops = l,
            Event::ContactsImported { added, merged, unchanged, error } => m.imported(added, merged, unchanged, &error),
            Event::Grants(g) => m.grants = g,
            Event::Audit(a) => m.audit = a,
            Event::Routes(r) => m.routes = r,
            Event::Bluetooth { available, powered } => {
                m.bt_available = available;
                m.bt_powered = powered;
            }
            Event::Devices(d) => m.devices = d,
            Event::Passkey(addr, key) => {
                let name = m
                    .devices
                    .iter()
                    .find(|d| addr.ends_with(&d.address.replace(':', "_")))
                    .map(|d| d.name.clone())
                    .unwrap_or_else(|| "device".into());
                m.passkey(&name, key);
            }
            Event::Blocked(b) => m.blocked = b,
            Event::Status(s) => m.status = Some(s),
            Event::Placed { kind: "wipe", placed } if placed.pending != 0 => {
                // Ours to confirm: the owner has just said yes on our own modal.
                let id = placed.pending;
                self.bus.run(move |c| async move { c.confirm.confirm(id).await.map(|_| vec![]) });
                m.modal = Some((vec!["Phone data deleted.".into()], None));
            }
            Event::Notice(t) | Event::Error(t) => m.modal = Some((vec![font::fit(&t, 210)], None)),
            _ => {}
        }
        self.draw();
    }

    fn act(&mut self, actions: Vec<Action>) {
        for a in actions {
            match a {
                Action::SetRinger(r) => {
                    self.bus.run(move |c| async move { c.audio.set_ringer(&r).await.map(|_| vec![]) })
                }
                // The PIN is read back, so the row says what phoned holds, and its
                // change is said in words since nothing on the row shows it.
                Action::Set(k, v) if k == phone_api::keys::MESSAGES_PIN.0 => self.bus.run(move |c| async move {
                    c.settings.set(k, &v).await?;
                    Ok(vec![
                        Event::Setting(k.into(), c.settings.get(k).await?),
                        Event::Notice(if v.is_empty() { "Messages PIN removed." } else { "Messages PIN set." }.into()),
                    ])
                }),
                Action::Set(k, v) => self.bus.run(move |c| async move { c.settings.set(k, &v).await.map(|_| vec![]) }),
                Action::SetRoute(r) => self.bus.run(move |c| async move {
                    c.audio.set_route(r).await?;
                    Ok(vec![Event::Routes(c.audio.routes().await?)])
                }),
                Action::BtPower(on) => {
                    self.bus.run(move |c| async move { c.bluetooth.set_powered(on).await.map(|_| vec![]) })
                }
                Action::BtScan => self.bus.run(|c| async move { c.bluetooth.start_scan().await.map(|_| vec![]) }),
                Action::BtPair(a) => self.bus.run(move |c| async move {
                    c.bluetooth.pair(&a).await?;
                    Ok(vec![Event::Devices(c.bluetooth.devices().await?)])
                }),
                Action::BtConnect(a, on) => self.bus.run(move |c| async move {
                    if on {
                        c.bluetooth.connect(&a).await?;
                    } else {
                        c.bluetooth.disconnect(&a).await?;
                    }
                    Ok(vec![Event::Devices(c.bluetooth.devices().await?)])
                }),
                Action::BtForget(a) => self.bus.run(move |c| async move {
                    c.bluetooth.forget(&a).await?;
                    Ok(vec![Event::Devices(c.bluetooth.devices().await?)])
                }),
                Action::Unblock(n) => self.bus.run(move |c| async move {
                    c.settings.unblock(&n).await?;
                    Ok(vec![Event::Blocked(c.settings.blocked().await?)])
                }),
                Action::ImportContacts(addr) => {
                    self.model.modal =
                        Some((vec!["Copying contacts...".into(), "Allow it on the phone.".into()], None));
                    self.bus.run(move |c| async move { c.contacts.import_from_phone(&addr).await.map(|_| vec![]) })
                }
                Action::ModemAccess => {
                    self.model.modal = Some((vec!["Asking for a password...".into()], None));
                    self.bus.run(|c| async move {
                        // polkit asks (dist/polkit/io.github.isaacrowntree.Dolphone.policy);
                        // ModemManager restarts, so look again once it is back.
                        let ran = phone_bus::Bus::blocking(|| {
                            std::process::Command::new("pkexec").args(["/usr/bin/dolphone-modem-access", "on"]).status()
                        })
                        .await;
                        if !matches!(ran, Some(Ok(s)) if s.success()) {
                            return Ok(vec![Event::Error("Not turned on. sudo dolphone-modem-access on".into())]);
                        }
                        // It finds the modem again in a few seconds; give it half a minute.
                        let mut steps = Vec::new();
                        for _ in 0..15 {
                            phone_bus::Bus::pause(std::time::Duration::from_secs(2)).await;
                            steps = c.modem.setup().await?;
                            if !steps.iter().any(|(n, st, _)| n == "modem" && st == "error") {
                                break;
                            }
                        }
                        Ok(vec![Event::ModemSetup { steps, applied: false }])
                    })
                }
                Action::ApplyModemSetup => {
                    self.model.modal = Some((vec!["Setting up the modem...".into()], None));
                    self.bus.run(|c| async move {
                        let (_, steps) = c.modem.apply_setup().await?;
                        Ok(vec![Event::ModemSetup { steps, applied: true }])
                    })
                }
                Action::LaptopUnpair(id) => self.bus.run(move |c| async move {
                    c.settings.kdeconnect_unpair(&id).await?;
                    Ok(vec![Event::Laptops(c.settings.kdeconnect_devices().await?)])
                }),
                Action::Revoke(id) => self.bus.run(move |c| async move {
                    c.settings.revoke_grant(id).await?;
                    Ok(vec![Event::Grants(c.settings.grants().await?)])
                }),
                Action::EditText(f) => {
                    let mut field = TextInput::new(f.title(), &self.model.field_text(f));
                    if f.is_pin() {
                        numeric(&mut field);
                    }
                    self.kb = Some((f, field));
                }
                Action::Wipe => self
                    .bus
                    .run(|c| async move { Ok(vec![Event::Placed { kind: "wipe", placed: c.settings.wipe().await? }]) }),
                Action::Refresh(p) => self.refresh(p),
                Action::Quit => {
                    let _ = slint::quit_event_loop();
                }
            }
        }
    }

    fn draw(&self) {
        if let Some((what, field)) = &self.kb {
            show_keyboard(&self.ui, field, what.is_pin(), self.frame % 10 < 5);
            return;
        }
        apply(&self.ui, &self.model, self.frame);
    }
}

fn item(label: &str, status: &str) -> ListItem {
    ListItem { label: label.into(), status: status.into(), frames: 1, dim: true, ..Default::default() }
}

fn toggle(label: &str, on: bool) -> ListItem {
    let v = if on { "ON" } else { "OFF" };
    ListItem { label: label.into(), status: v.into(), value: v.into(), chevrons: 1, frames: 1, ..Default::default() }
}

fn adjust(label: &str, value: &str) -> ListItem {
    ListItem {
        label: label.into(),
        status: value.into(),
        value: value.into(),
        chevrons: 2,
        frames: 1,
        ..Default::default()
    }
}

/// "5 min", for the seconds Messages stays open.
fn open_for_label(secs: &str) -> String {
    match secs.parse::<u32>() {
        Ok(s) if s % 60 == 0 => format!("{} min", s / 60),
        Ok(s) => format!("{s} s"),
        Err(_) => secs.to_string(),
    }
}

fn cap(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

pub fn items(m: &Model) -> (Vec<ListItem>, String, [&'static str; 5]) {
    match m.page {
        Page::Main => {
            let route = m.routes.iter().find(|r| r.active).map(|r| r.name.clone()).unwrap_or_else(|| "Speaker".into());
            let bt = if !m.bt_available {
                "None".to_string()
            } else if !m.bt_powered {
                "Off".into()
            } else {
                match m.devices.iter().filter(|d| d.connected).count() {
                    0 => "On".into(),
                    n => format!("{n} connected"),
                }
            };
            let rows = MAIN
                .iter()
                .map(|r| match r {
                    MainRow::Ringer => adjust("Ringer", &cap(&m.ringer)),
                    MainRow::Vibrate => toggle("Vibrate on ring", m.flag(phone_api::keys::VIBRATE)),
                    MainRow::Audio => item("Call audio", &font::fit(&route, 90)),
                    MainRow::Bluetooth => item("Bluetooth", &bt),
                    MainRow::Calls => item("Calls", ""),
                    MainRow::Laptops => {
                        item("Laptops (KDE Connect)", if m.flag(phone_api::keys::KDECONNECT) { "On" } else { "Off" })
                    }
                    MainRow::Sip => item(
                        "Calls over the internet",
                        match m.status.as_ref().map(|s| s.sip.as_str()) {
                            Some("registered") => "On",
                            Some(s) if s.starts_with("failed") => "Problem",
                            Some("registering") => "...",
                            _ => "Off",
                        },
                    ),
                    MainRow::Texts => item("Texts", ""),
                    MainRow::Alerts => item("Emergency alerts", ""),
                    MainRow::Privacy => item("Privacy", ""),
                    MainRow::Region => adjust("Number region", &m.region),
                    MainRow::HoldTime => {
                        adjust("Hold time", &format!("{:.1} s", m.hold_ms.parse::<f32>().unwrap_or(600.0) / 1000.0))
                    }
                    MainRow::Modem => item(
                        "Modem",
                        m.status.as_ref().map(|s| if s.present { s.tech.as_str() } else { "None" }).unwrap_or(""),
                    ),
                    MainRow::Wipe => item("Delete phone data", ""),
                    MainRow::About => item("About", ""),
                })
                .collect();
            (rows, String::new(), ["Exit", "", "", "", "Select"])
        }
        Page::Audio => {
            let rows = if m.routes.is_empty() {
                vec![item("Speaker", "")]
            } else {
                m.routes
                    .iter()
                    .map(|r| {
                        let st = if r.active {
                            "In use"
                        } else if r.available {
                            ""
                        } else {
                            "Not connected"
                        };
                        ListItem { dim: !r.available, ..item(&font::fit(&r.name, 140), st) }
                    })
                    .collect()
            };
            (rows, "> Call audio".into(), ["Back", "", "", "", "Use"])
        }
        Page::Bluetooth => {
            let mut rows = vec![toggle("Bluetooth", m.bt_powered && m.bt_available)];
            rows.push(item(if m.bt_available { "Scan for devices" } else { "No Bluetooth adapter" }, ""));
            for d in &m.devices {
                let kind = if d.headset {
                    "headset"
                } else if d.speaker {
                    "speaker"
                } else {
                    ""
                };
                let st = if d.connected {
                    "Connected"
                } else if d.paired {
                    "Paired"
                } else {
                    "Pair"
                };
                rows.push(ListItem { tag: kind.into(), ..item(&font::fit(&d.name, 110), st) });
            }
            let paired = m.sel() >= 2 && m.devices.get(m.sel() - 2).is_some_and(|d| d.paired);
            (rows, "> Bluetooth".into(), ["Back", if paired { "Contacts" } else { "" }, "", "Forget", "Select"])
        }
        Page::Blocked => {
            let rows = if m.blocked.is_empty() {
                vec![item("Nobody is blocked", "")]
            } else {
                m.blocked.iter().map(|n| item(&phone_core::number::display(n, &m.region), "Unblock")).collect()
            };
            (rows, "> Blocked numbers".into(), ["Back", "", "", "", "Unblock"])
        }
        Page::Modem | Page::About => (Vec::new(), String::new(), ["Back", "", "", "", ""]),
        Page::Calls => {
            let vm = if m.voicemail.is_empty() { "Not set".into() } else { font::fit(&m.voicemail, 90) };
            let mut rows = vec![item("Voicemail number", &vm)];
            rows.extend(CALL_FLAGS.iter().map(|(label, key)| toggle(label, m.flag(*key))));
            (rows, "> Calls".into(), ["Back", "", "", if m.sel() == 0 { "Edit" } else { "" }, "Select"])
        }
        Page::Sip => {
            let status = m.status.as_ref().map(|s| s.sip.clone()).unwrap_or_default();
            let rows = SIP
                .iter()
                .map(|r| match r {
                    SipRow::Enabled => toggle("Calls over SIP", m.flag(phone_api::keys::SIP_ENABLED)),
                    SipRow::Account => item(
                        "Account",
                        &if m.sip_account.is_empty() { "Not set".into() } else { font::fit(&m.sip_account, 110) },
                    ),
                    SipRow::Password => item("Password", if m.sip_password_set { "Set" } else { "Not set" }),
                    SipRow::Proxy => item(
                        "Proxy",
                        &if m.sip_proxy.is_empty() { "None".into() } else { font::fit(&m.sip_proxy, 110) },
                    ),
                    SipRow::Use => adjust("Use it", if m.sip_use == "always" { "Always" } else { "No signal" }),
                    SipRow::Status => ListItem {
                        dim: true,
                        ..item("Status", &font::fit(if status.is_empty() { "Off" } else { &status }, 120))
                    },
                })
                .collect();
            let edit = matches!(SIP[m.sel().min(SIP.len() - 1)], SipRow::Account | SipRow::Password | SipRow::Proxy);
            (rows, "> Calls over the internet".into(), ["Back", "", "", if edit { "Edit" } else { "" }, "Select"])
        }
        Page::Laptops => {
            let mut rows = vec![toggle("Connect to laptops", m.flag(phone_api::keys::KDECONNECT))];
            for (_, name, paired) in &m.laptops {
                rows.push(item(&font::fit(name, 130), if *paired { "Paired" } else { "Not paired" }));
            }
            if m.laptops.is_empty() && m.flag(phone_api::keys::KDECONNECT) {
                rows.push(ListItem { dim: true, ..item("Open KDE Connect on the laptop", "") });
            }
            (rows, "> Laptops".into(), ["Back", "", "", "", "Select"])
        }
        Page::Texts => {
            let mut rows: Vec<ListItem> = TEXT_FLAGS.iter().map(|(label, key)| toggle(label, m.flag(*key))).collect();
            let shown = |v: &str, none: &str| if v.is_empty() { none.to_string() } else { font::fit(v, 100) };
            rows.push(item("Uplink alerts to", &shown(&m.uplink_to, "Off")));
            rows.push(item("Uplink to watch", &shown(&m.uplink_iface, "Any")));
            let edit = if m.sel() >= TEXT_FLAGS.len() { "Edit" } else { "" };
            (rows, "> Texts".into(), ["Back", "", "", edit, "Select"])
        }
        Page::Alerts => {
            let mut rows = vec![ListItem { dim: true, ..toggle("National alerts", true) }];
            rows.extend(ALERT_FLAGS.iter().map(|(label, key)| toggle(label, m.flag(*key))));
            let now = phone_core::now();
            rows.extend(m.alerts.iter().map(|a| {
                let level = phone_core::model::AlertLevel::from_u8(a.level).label();
                item(&font::fit(level, 120), &phone_core::when::short(a.at, now))
            }));
            (rows, "> Emergency alerts".into(), ["Back", "", "", "", "Select"])
        }
        Page::Privacy => {
            let rows = PRIVACY
                .iter()
                .map(|r| match r {
                    PrivacyRow::Blocked => item("Blocked numbers", &m.blocked.len().to_string()),
                    PrivacyRow::Grants => item("Programs allowed to text", ""),
                    PrivacyRow::Audit => item("What programs asked for", ""),
                    PrivacyRow::MessagesPin => item("Messages PIN", if m.messages_pin_set { "On" } else { "Off" }),
                    PrivacyRow::StayOpen => adjust("Messages stay open", &open_for_label(&m.open_for)),
                })
                .collect();
            let buttons = match PRIVACY[m.sel().min(PRIVACY.len() - 1)] {
                PrivacyRow::MessagesPin if m.messages_pin_set => ["Back", "", "", "Remove", "Change"],
                PrivacyRow::MessagesPin => ["Back", "", "", "", "Set"],
                PrivacyRow::StayOpen => ["Back", "", "", "", "Change"],
                _ => ["Back", "", "", "", "Select"],
            };
            (rows, "> Privacy".into(), buttons)
        }
        Page::Grants => {
            let rows = if m.grants.is_empty() {
                vec![item("No program may text", "")]
            } else {
                m.grants
                    .iter()
                    .map(|g| {
                        let to = g.numbers.split(',').next().unwrap_or("");
                        let more = if g.numbers.contains(',') { " +" } else { "" };
                        ListItem {
                            tag: format!("{}/h", g.per_hour).into(),
                            ..item(
                                &font::fit(&g.program, 100),
                                &format!("{}{more}", font::fit(&phone_core::number::display(to, &m.region), 80)),
                            )
                        }
                    })
                    .collect()
            };
            let revoke = if m.grants.is_empty() { "" } else { "Revoke" };
            (rows, "> Allowed to text".into(), ["Back", "", "", "", revoke])
        }
        Page::Audit => {
            let now = phone_core::now();
            let rows = if m.audit.is_empty() {
                vec![item("Nothing asked yet", "")]
            } else {
                m.audit
                    .iter()
                    .map(|a| ListItem {
                        tag: short_result(&a.result).into(),
                        ..item(
                            &font::fit(&format!("{} {}", a.program, a.action), 110),
                            &phone_core::when::short(a.at, now),
                        )
                    })
                    .collect()
            };
            (rows, "> What programs asked".into(), ["Back", "", "", "", ""])
        }
    }
}

/// The modem's setup in one row: "Done", what Set up would change, or why it
/// cannot be checked.
fn setup_row(steps: &[(String, String, String)]) -> Option<DetailRow> {
    if steps.is_empty() {
        return None;
    }
    let short = |name: &str| match name {
        "usb audio" => "USB audio",
        "volte" => "VoLTE",
        "carrier profile" => "Carrier",
        _ => "Modem",
    };
    let todo: Vec<&str> = steps.iter().filter(|(_, st, _)| st == "change").map(|(n, _, _)| short(n)).collect();
    let value = if let Some((_, _, why)) = steps.iter().find(|(_, st, _)| st == "error") {
        if why.contains("debug mode") {
            "Needs MM debug".to_string()
        } else {
            "Unknown".to_string()
        }
    } else if steps.iter().any(|(n, st, _)| n == "modem" && st == "unsupported") {
        "Not needed".to_string()
    } else if todo.is_empty() {
        "Done".to_string()
    } else if todo.len() == 1 {
        format!("{} off", todo[0])
    } else {
        format!("{} to do", todo.len())
    };
    Some(DetailRow { kind: 0, label: "Setup".into(), value: font::fit(&value, 120).into(), ..Default::default() })
}

fn detail(m: &Model) -> Vec<DetailRow> {
    let row = |label: &str, value: &str| DetailRow {
        kind: 0,
        label: label.into(),
        value: value.into(),
        ..Default::default()
    };
    match m.page {
        Page::Modem => match &m.status {
            Some(s) if s.present => vec![
                row("Operator", if s.operator.is_empty() { "-" } else { &s.operator }),
                row("Network", &s.tech),
                DetailRow {
                    kind: 2,
                    label: "Signal".into(),
                    value: format!("{}%", s.quality).into(),
                    percent: i32::from(s.quality),
                    ..Default::default()
                },
                row(
                    "Registered",
                    if s.registered {
                        if s.roaming {
                            "Roaming"
                        } else {
                            "Home"
                        }
                    } else {
                        "No"
                    },
                ),
                row("Number", &phone_core::number::display(&s.own_number, &m.region)),
                row(
                    "Voice calls",
                    if !s.voice {
                        "No"
                    } else if s.emergency_only {
                        "Emergency only"
                    } else {
                        "Yes"
                    },
                ),
            ]
            .into_iter()
            .chain(setup_row(&m.setup))
            .collect(),
            _ => vec![DetailRow { kind: 3, label: "No modem. Fit one in the M.2 slot.".into(), ..Default::default() }],
        },
        _ => vec![
            row("Dolphone", env!("CARGO_PKG_VERSION")),
            row(
                "Device",
                &m.device.as_ref().map(|d| font::fit(&format!("{} ({})", d.name, d.status), 150)).unwrap_or_default(),
            ),
            DetailRow { kind: 3, label: "Phone apps for FlipCTL.".into(), ..Default::default() },
            DetailRow { kind: 3, label: "Unofficial; not by Flipper Devices.".into(), ..Default::default() },
            DetailRow { kind: 3, label: "Not for emergency calls.".into(), ..Default::default() },
            DetailRow { kind: 3, label: "MIT source, GPL-3.0 binary.".into(), ..Default::default() },
        ],
    }
}

/// An audit result in a word, to fit beside the time.
fn short_result(r: &str) -> &'static str {
    match r {
        "sent under a grant" => "granted",
        "waiting for confirmation" => "asked",
        "over its grant's limit" | "over the limit" => "limit",
        "denied" => "denied",
        "refused" => "refused",
        "granted" => "granted",
        "allowed" => "read",
        "failed" => "failed",
        _ => "done",
    }
}

/// FlipCTL's keyboard has no number pad; its 123 layout, with the digits on top
/// and the highlight on 1, is the nearest.
fn numeric(field: &mut TextInput) {
    use flipctl_app::FlipperKey;
    field.key(FlipperKey::View, true);
    field.key(FlipperKey::Up, true);
}

/// The keyboard. A secret shows as a star for each character, laid out as the
/// keyboard lays out text, so the caret sits where it should.
fn show_keyboard(ui: &AppWindow, field: &TextInput, secret: bool, cursor_on: bool) {
    let mut k = field.view("", cursor_on);
    if secret {
        use flipctl_app::keyboard::{field_w, fit_input};
        let stars = "*".repeat(field.text.chars().count());
        let w = field_w(i32::from(font::TITLE.text_width(&stars)));
        let fitted = fit_input(&stars, field.cursor, w - 2 * flipctl_app::theme::metric::KB_INPUT_PAD);
        k.text = fitted.visible;
        k.field_w = w as f32;
        k.cursor_dx = fitted.cursor_dx as f32;
    }
    ui.set_mode(3);
    ui.set_kb_title(k.title.as_str().into());
    ui.set_kb_text(k.text.as_str().into());
    ui.set_kb_field_w(k.field_w);
    ui.set_kb_cursor_dx(k.cursor_dx);
    ui.set_kb_cursor_on(k.cursor_on);
    ui.set_kb_field_focused(k.field_focused);
    ui.set_kb_warning(k.warning.as_str().into());
    let cells: Vec<KbCell> = k
        .cells
        .iter()
        .map(|c| KbCell {
            x: c.x as f32,
            y: c.y as f32,
            w: c.w as f32,
            text: c.text.as_str().into(),
            icon: c.icon,
            icon_w: c.icon_w as f32,
            icon_h: c.icon_h as f32,
            selected: c.selected,
            pressed: c.pressed,
            clip_h: c.clip_h as f32,
        })
        .collect();
    ui.set_kb_cells(ModelRc::new(VecModel::from(cells)));
    ui.set_kb_chrome_x(k.chrome.0);
    ui.set_kb_chrome_y(k.chrome.1);
    ui.set_kb_chrome_w(k.chrome.2);
    ui.set_kb_chrome_h(k.chrome.3);
    ui.set_kb_lang_label(k.lang_label.into());
    ui.set_kb_tab_label(k.tab_label.into());
    ui.set_kb_tab_focus(k.tab_focus);
    ui.set_kb_tab_pressed(k.tab_pressed);
    ui.set_kb_discard(k.discard);
    let b = ["Cancel", "", "", "", "Done"].map(SharedString::from);
    ui.set_buttons(ModelRc::new(VecModel::from(b.to_vec())));
    ui.set_modal_lines(ModelRc::default());
}

fn apply(ui: &AppWindow, m: &Model, frame: u32) {
    let strings =
        |s: &[&str]| ModelRc::new(VecModel::from(s.iter().map(|x| SharedString::from(*x)).collect::<Vec<_>>()));
    ui.set_tick(frame as i32);
    if matches!(m.page, Page::Modem | Page::About) {
        ui.set_mode(2);
        ui.set_rows(ModelRc::new(VecModel::from(detail(m))));
        ui.set_breadcrumb(if m.page == Page::Modem { "> Modem" } else { "> About" }.into());
        let run = match m.page {
            Page::Modem if m.needs_access() => "Turn on",
            Page::Modem if m.setup_needed() => "Set up",
            _ => "",
        };
        ui.set_buttons(strings(&["Back", "", "", "", run]));
    } else {
        ui.set_mode(1);
        let (rows, crumb, buttons) = items(m);
        let sel = m.sel().min(rows.len().saturating_sub(1));
        ui.set_total(rows.len() as i32);
        ui.set_items(ModelRc::new(VecModel::from(rows)));
        ui.set_selected(sel as i32);
        ui.set_scroll(if sel < 5 { 0 } else { (sel - 4) as i32 });
        ui.set_breadcrumb(crumb.as_str().into());
        ui.set_buttons(strings(&buttons));
    }
    match &m.modal {
        Some((lines, then)) => {
            ui.set_modal_lines(ModelRc::new(VecModel::from(
                lines.iter().map(|l| SharedString::from(l.as_str())).collect::<Vec<_>>(),
            )));
            ui.set_modal_left(if then.is_some() { "Cancel" } else { "" }.into());
            ui.set_modal_right(if then.is_some() { "Yes" } else { "Ok" }.into());
        }
        None => ui.set_modal_lines(ModelRc::default()),
    }
}

#[cfg(test)]
// Tests build a model and then set the few fields each one is about.
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use flipctl_app::theme;
    use flipper_ui::slint_render::{render_frame, FlipperSlintPlatform};
    use phone_api::{BtDevice, ModemStatus, RouteInfo};
    use slint::platform::software_renderer::MinimalSoftwareWindow;
    use std::rc::Rc;

    fn panel() -> Rc<MinimalSoftwareWindow> {
        thread_local! {
            static WINDOW: Rc<MinimalSoftwareWindow> = FlipperSlintPlatform::install();
        }
        WINDOW.with(Rc::clone)
    }

    fn shot(name: &str, m: &Model) {
        let window = panel();
        let ui = AppWindow::new().expect("window");
        apply(&ui, m, 0);
        ui.show().expect("show");
        slint::platform::update_timers_and_animations();
        let frame: Vec<u8> = render_frame(&window).expect("frame").iter().map(|p| p.0).collect();
        drop(ui);
        assert!(frame.iter().filter(|p| **p < 128).count() > 200);
        if std::env::var_os("DOLPHONE_RENDER").is_some() {
            let dir = std::path::Path::new("target/render");
            std::fs::create_dir_all(dir).unwrap();
            let f = std::fs::File::create(dir.join(format!("settings-{name}.png"))).unwrap();
            let mut e =
                png::Encoder::new(std::io::BufWriter::new(f), u32::from(theme::PANEL_W), u32::from(theme::PANEL_H));
            e.set_color(png::ColorType::Grayscale);
            e.set_depth(png::BitDepth::Eight);
            e.write_header().unwrap().write_image_data(&frame).unwrap();
        }
    }

    #[test]
    fn screens() {
        let mut m = Model::default();
        m.bt_available = true;
        m.bt_powered = true;
        m.routes = vec![
            RouteInfo { route: 0, name: "Speaker".into(), available: true, active: false },
            RouteInfo { route: 1, name: "Headset".into(), available: false, active: false },
            RouteInfo { route: 2, name: "Pixel Buds".into(), available: true, active: true },
        ];
        m.devices = vec![
            BtDevice {
                address: "AA".into(),
                name: "Pixel Buds".into(),
                paired: true,
                connected: true,
                headset: true,
                speaker: true,
            },
            BtDevice {
                address: "BB".into(),
                name: "JBL Flip".into(),
                paired: false,
                connected: false,
                headset: false,
                speaker: true,
            },
        ];
        m.status = Some(ModemStatus {
            present: true,
            registered: true,
            roaming: false,
            operator: "Fakecom AU".into(),
            quality: 72,
            tech: "LTE".into(),
            voice: true,
            own_number: "+61491570110".into(),
            emergency_only: false,
            state: "ready".into(),
            sip: String::new(),
        });
        shot("main", &m);
        m.page = Page::Audio;
        shot("audio", &m);
        m.page = Page::Bluetooth;
        shot("bluetooth", &m);
        m.page = Page::Modem;
        m.setup = vec![
            ("modem".into(), "ok".into(), "Quectel RM520N-GL".into()),
            ("usb audio".into(), "ok".into(), String::new()),
            ("volte".into(), "change".into(), String::new()),
            ("carrier profile".into(), "change".into(), String::new()),
        ];
        shot("modem", &m);
        m.page = Page::Calls;
        m.voicemail = "101".into();
        shot("calls", &m);
        m.page = Page::Texts;
        shot("texts", &m);
        m.page = Page::Sip;
        m.flags.insert("sip_enabled", true);
        m.sip_account = "sip:me@voip.example.net".into();
        m.sip_password_set = true;
        if let Some(s) = m.status.as_mut() {
            s.sip = "registered".into();
        }
        shot("sip", &m);
        m.page = Page::Alerts;
        m.alerts = vec![phone_api::AlertInfo {
            id: 1,
            channel: 4371,
            level: 1,
            text: "Bushfire: leave now".into(),
            at: phone_core::now() - 3600,
        }];
        shot("alerts", &m);
        m.page = Page::Privacy;
        shot("privacy", &m);
        m.messages_pin_set = true;
        m.open_for = "900".into();
        m.sel[Page::Privacy as usize] = PRIVACY.iter().position(|r| *r == PrivacyRow::MessagesPin).unwrap();
        shot("privacy-pin", &m);
        m.sel[Page::Privacy as usize] = 0;
        m.page = Page::Grants;
        m.grants = vec![phone_api::Grant {
            id: 1,
            program: "wan-watch".into(),
            numbers: "+61491570158,+61491571*".into(),
            per_hour: 4,
            until: 0,
        }];
        shot("grants", &m);
        m.page = Page::Audit;
        m.audit = vec![
            phone_api::AuditEntry {
                at: phone_core::now() - 60,
                program: "wan-watch".into(),
                action: "send".into(),
                target: "+61...158".into(),
                result: "sent under a grant".into(),
            },
            phone_api::AuditEntry {
                at: phone_core::now() - 600,
                program: "python3".into(),
                action: "dial".into(),
                target: "1900...456".into(),
                result: "denied".into(),
            },
        ];
        shot("audit", &m);
        m.page = Page::Main;
        m.sel = [0; 14];
        m.sel[0] = MAIN.iter().position(|r| *r == MainRow::Wipe).unwrap();
        m.key(Input::Run);
        shot("wipe", &m);
    }

    // Live: the real app against a running phoned and fake-mm (scripts/ui-e2e.sh).

    struct Live {
        app: App,
        rx: std::sync::mpsc::Receiver<phone_bus::Event>,
    }

    impl Live {
        fn start() -> Option<Self> {
            std::env::var_os("DOLPHONE_LIVE")?;
            let _ = panel();
            let (tx, rx) = std::sync::mpsc::channel();
            let bus = Bus::start(move |e| {
                let _ = tx.send(e);
            });
            let app = App::new(AppWindow::new().expect("window"), bus);
            let mut l = Self { app, rx };
            // Connecting loads the settings; let all of it land before any key,
            // as a person would see the screen fill before pressing.
            l.until("the settings load", |m| m.connected && m.status.is_some());
            l.settle();
            Some(l)
        }

        fn until(&mut self, what: &str, done: impl Fn(&Model) -> bool) {
            let end = std::time::Instant::now() + Duration::from_secs(10);
            while !done(&self.app.model) {
                let left = end.saturating_duration_since(std::time::Instant::now());
                assert!(!left.is_zero(), "timed out waiting: {what}");
                if let Ok(e) = self.rx.recv_timeout(left.min(Duration::from_millis(100))) {
                    self.app.event(e);
                }
            }
        }

        fn settle(&mut self) {
            let end = std::time::Instant::now() + Duration::from_millis(400);
            while std::time::Instant::now() < end {
                if let Ok(e) = self.rx.recv_timeout(Duration::from_millis(20)) {
                    self.app.event(e);
                }
            }
        }

        /// A key, through the same path the panel's keys take.
        fn press(&mut self, i: Input) {
            let a = self.app.model.key(i);
            self.app.act(a);
        }

        fn select(&mut self, page_rows: usize, row: usize) {
            for _ in 0..page_rows {
                self.press(Input::Up);
            }
            for _ in 0..row {
                self.press(Input::Down);
            }
        }

        fn shot(&mut self, name: &str) {
            let window = panel();
            self.app.draw();
            self.app.ui.show().expect("show");
            slint::platform::update_timers_and_animations();
            window.request_redraw();
            let frame: Vec<u8> = render_frame(&window).expect("frame").iter().map(|p| p.0).collect();
            let dir = std::path::Path::new("target/render/live");
            std::fs::create_dir_all(dir).unwrap();
            let f = std::fs::File::create(dir.join(format!("settings-{name}.png"))).unwrap();
            let mut e =
                png::Encoder::new(std::io::BufWriter::new(f), u32::from(theme::PANEL_W), u32::from(theme::PANEL_H));
            e.set_color(png::ColorType::Grayscale);
            e.set_depth(png::BitDepth::Eight);
            e.write_header().unwrap().write_image_data(&frame).unwrap();
        }
    }

    fn phonectl(args: &[&str]) -> String {
        let out = std::process::Command::new("phonectl").args(args).output().expect("phonectl on PATH");
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
    }

    #[test]
    #[ignore = "needs phoned and fake-mm: scripts/ui-e2e.sh"]
    fn live_settings() {
        let Some(mut l) = Live::start() else { return };
        l.shot("01-main");
        let at = |r: MainRow| MAIN.iter().position(|x| *x == r).unwrap();

        // Calls > Call waiting off reaches the modem.
        l.select(MAIN.len(), at(MainRow::Calls));
        l.press(Input::Run);
        l.press(Input::Down);
        l.press(Input::Run);
        l.settle();
        l.shot("02-calls");
        assert!(phonectl(&["fake", "dump"]).contains("call_waiting=false"), "the modem was told");
        let modal = l.app.model.modal.as_ref().map(|m| m.0.join(" "));
        l.press(Input::Run);
        l.settle();
        assert!(phonectl(&["fake", "dump"]).contains("call_waiting=true"), "modal was {modal:?}");

        // The voicemail number, typed on flipctl's keyboard.
        l.press(Input::Up);
        l.press(Input::Run);
        assert!(l.app.kb.is_some(), "the keyboard is up");
        for c in "101".chars() {
            l.app.keyboard(None, &c.to_string(), true);
        }
        l.shot("03-voicemail-keyboard");
        l.app.keyboard(Some(Key::Run), "", true);
        l.settle();
        assert_eq!(phonectl(&["get", "voicemail"]).trim(), "101");
        l.press(Input::Escape);

        // An emergency alert is listed under Emergency alerts.
        phonectl(&["fake", "broadcast", "4371", "Extreme weather warning for the live test."]);
        l.select(MAIN.len(), at(MainRow::Alerts));
        l.press(Input::Run);
        l.until("the alert is listed", |m| !m.alerts.is_empty());
        l.shot("04-alerts");
        l.press(Input::Escape);

        // What the machine is taken to be.
        l.select(MAIN.len(), at(MainRow::About));
        l.press(Input::Run);
        l.until("the device profile arrives", |m| m.device.is_some());
        l.shot("05-about");
    }
}
