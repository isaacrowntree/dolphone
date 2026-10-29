//! Dolphone's Phone Settings, a FlipCTL app.

mod model;

use std::cell::RefCell;
use std::time::Duration;

use flipctl_app::{font, Key, StatusSource};
use model::{Action, Input, MainRow, Model, Page, MAIN};
use phone_bus::{Bus, Event};
use slint::{ModelRc, SharedString, VecModel};

slint::include_modules!();

struct App {
    ui: AppWindow,
    model: Model,
    bus: Bus,
    frame: u32,
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
    APP.with(|a| *a.borrow_mut() = Some(App { ui: ui.clone_strong(), model: Model::default(), bus, frame: 0 }));
    with_app(|app| {
        app.refresh(Page::Main);
        app.draw();
    });
    let mut status = StatusSource::new(Duration::from_secs(2));
    flipctl_app::apply_status!(&ui, PanelStatus, status.current());
    let tick = slint::Timer::default();
    tick.start(slint::TimerMode::Repeated, Duration::from_millis(200), move || {
        with_app(|app| {
            app.frame = app.frame.wrapping_add(1);
            if let Some(now) = status.poll() {
                flipctl_app::apply_status!(&app.ui, PanelStatus, now);
            }
            // A Bluetooth scan fills the list as it goes.
            if app.model.page == Page::Bluetooth && app.frame % 10 == 0 {
                app.refresh(Page::Bluetooth);
            }
            app.draw();
        });
    });
    ui.on_keyed(move |text, down| {
        with_app(|app| {
            let Some(k) = Key::from_slint(text.as_str()) else { return };
            app.ui.set_pressed_slot(match (down, k.soft_slot()) {
                (true, Some(slot)) => slot as i32,
                _ => -1,
            });
            app.ui.set_arrow_pressed(match (down, k) {
                (true, Key::Left) => 1,
                (true, Key::Right) => 2,
                _ => 0,
            });
            if !down {
                app.draw();
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
            let actions = app.model.key(input);
            app.act(actions);
            app.draw();
        });
    });
    ui.run()
}

impl App {
    fn refresh(&self, page: Page) {
        match page {
            Page::Main => self.bus.run(|c| async move {
                let mut out = vec![Event::Ringer(c.audio.ringer().await?)];
                for key in ["vibrate", "private_notify", "delete_from_sim", "region"] {
                    out.push(Event::Setting(key.into(), c.settings.get(key).await?));
                }
                out.push(Event::Routes(c.audio.routes().await?));
                out.push(Event::Bluetooth {
                    available: c.bluetooth.available().await?,
                    powered: c.bluetooth.powered().await?,
                });
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
            Page::Modem => self.bus.refresh_status(),
            Page::About => {}
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
            Event::Setting(k, v) => match k.as_str() {
                "vibrate" => m.vibrate = v == "true",
                "private_notify" => m.private = v == "true",
                "delete_from_sim" => m.delete_from_sim = v == "true",
                "region" => m.region = v,
                _ => {}
            },
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
            Event::Error(t) => m.modal = Some((vec![font::fit(&t, 210)], None)),
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
                    MainRow::Vibrate => toggle("Vibrate on ring", m.vibrate),
                    MainRow::Audio => item("Call audio", &font::fit(&route, 90)),
                    MainRow::Bluetooth => item("Bluetooth", &bt),
                    MainRow::Blocked => item("Blocked numbers", &m.blocked.len().to_string()),
                    MainRow::Private => toggle("Hide texts in alerts", m.private),
                    MainRow::DeleteFromSim => toggle("Clear texts from SIM", m.delete_from_sim),
                    MainRow::Region => adjust("Number region", &m.region),
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
            (rows, "> Bluetooth".into(), ["Back", "", "", "Forget", "Select"])
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
    }
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
                row("Voice calls", if s.voice { "Yes" } else { "No" }),
                row("Emergency only", if s.emergency_only { "Yes" } else { "No" }),
            ],
            _ => vec![DetailRow { kind: 3, label: "No modem. Fit one in the M.2 slot.".into(), ..Default::default() }],
        },
        _ => vec![
            row("Dolphone", env!("CARGO_PKG_VERSION")),
            DetailRow { kind: 3, label: "Phone apps for FlipCTL.".into(), ..Default::default() },
            DetailRow { kind: 3, label: "Unofficial; not by Flipper Devices.".into(), ..Default::default() },
            DetailRow { kind: 3, label: "Not for emergency calls.".into(), ..Default::default() },
            DetailRow { kind: 3, label: "MIT source, GPL-3.0 binary.".into(), ..Default::default() },
        ],
    }
}

fn apply(ui: &AppWindow, m: &Model, frame: u32) {
    let strings =
        |s: &[&str]| ModelRc::new(VecModel::from(s.iter().map(|x| SharedString::from(*x)).collect::<Vec<_>>()));
    ui.set_tick(frame as i32);
    if matches!(m.page, Page::Modem | Page::About) {
        ui.set_mode(2);
        ui.set_rows(ModelRc::new(VecModel::from(detail(m))));
        ui.set_breadcrumb(if m.page == Page::Modem { "> Modem" } else { "> About" }.into());
        ui.set_buttons(strings(&["Back", "", "", "", ""]));
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
        });
        shot("main", &m);
        m.page = Page::Audio;
        shot("audio", &m);
        m.page = Page::Bluetooth;
        shot("bluetooth", &m);
        m.page = Page::Modem;
        shot("modem", &m);
        m.page = Page::Main;
        m.sel = [9, 0, 0, 0, 0, 0];
        m.key(Input::Run);
        shot("wipe", &m);
    }
}
