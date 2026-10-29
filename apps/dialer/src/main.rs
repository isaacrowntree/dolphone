//! Dolphone's Dialer, a FlipCTL app.
//!
//! The keypad, recent calls, contacts, the incoming and in-call screens, and the
//! confirmations phoned asks for. phoned raises it with `flipctl open` when a call
//! rings or an outside program asks to dial; the owner opens it from Apps > Phone.

mod model;
mod view;

use std::cell::RefCell;
use std::time::{Duration, Instant};

use flipctl_app::{Key, StatusSource};
use model::{Action, Buzz, Input, Model, Top};
use phone_bus::{Bus, Event};
use slint::{ModelRc, SharedString, VecModel};

slint::include_modules!();

struct App {
    ui: AppWindow,
    model: Model,
    bus: Bus,
    frame: u32,
    vibrate: bool,
    ringer: String,
    #[cfg(feature = "device")]
    motor: Option<flipctl_app::Haptic>,
    last_ring_buzz: Instant,
    contacts_loaded: Instant,
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
    let region = phone_core::number::DEFAULT_REGION.to_string();
    let book = phone_core::contacts::Book::load(&phone_core::store::contacts_dir(), &region);
    let bus = Bus::start(|e| {
        let _ = slint::invoke_from_event_loop(move || with_app(|app| app.event(e)));
    });
    let app = App {
        ui: ui.clone_strong(),
        model: Model::new(&region, book),
        bus,
        frame: 0,
        vibrate: true,
        ringer: "ring".into(),
        #[cfg(feature = "device")]
        motor: flipctl_app::Haptic::open().ok(),
        last_ring_buzz: Instant::now(),
        contacts_loaded: Instant::now(),
    };
    APP.with(|a| *a.borrow_mut() = Some(app));
    with_app(|app| {
        app.reload();
        app.draw();
    });

    let mut status = StatusSource::new(Duration::from_secs(2));
    flipctl_app::apply_status!(&ui, PanelStatus, status.current());
    #[cfg(feature = "device")]
    let mut pad = Swipe::new();
    let tick = slint::Timer::default();
    tick.start(slint::TimerMode::Repeated, Duration::from_millis(250), move || {
        with_app(|app| {
            app.frame = app.frame.wrapping_add(1);
            app.tick();
            if let Some(now) = status.poll() {
                flipctl_app::apply_status!(&app.ui, PanelStatus, now);
            }
            #[cfg(feature = "device")]
            for input in pad.poll() {
                app.input(input, true);
                app.input(input, false);
            }
        });
    });

    ui.on_keyed(move |text, down| {
        with_app(|app| {
            let input = match Key::from_slint(text.as_str()) {
                Some(k) => {
                    app.ui.set_pressed_slot(match (down, k.soft_slot()) {
                        (true, Some(slot)) => slot as i32,
                        _ => -1,
                    });
                    match k {
                        Key::Up => Input::Up,
                        Key::Down => Input::Down,
                        Key::Left => Input::Left,
                        Key::Right => Input::Right,
                        Key::Ok => Input::Ok,
                        Key::Escape => Input::Escape,
                        Key::View => Input::View,
                        Key::Edit => Input::Edit,
                        Key::Run => Input::Run,
                        Key::Ptt => Input::Ptt,
                        // Back leaves the app (flipctl's), Power is the system's.
                        Key::Back | Key::Power => return,
                    }
                }
                None => match text.chars().next() {
                    Some(c) if text.chars().count() == 1 && (c.is_ascii_digit() || "*#+".contains(c)) => Input::Char(c),
                    _ => return,
                },
            };
            app.input(input, down);
        });
    });

    ui.run()
}

impl App {
    fn reload(&mut self) {
        self.bus.refresh_calls();
        self.bus.refresh_pending();
        self.bus.refresh_recents(100);
        self.bus.refresh_status();
        self.bus.refresh_routes();
        self.bus.run(|c| async move {
            Ok(vec![
                Event::Ringer(c.audio.ringer().await?),
                Event::Setting("vibrate".into(), c.settings.get("vibrate").await?),
                Event::Setting("region".into(), c.settings.get("region").await?),
            ])
        });
    }

    fn input(&mut self, input: Input, down: bool) {
        let actions = self.model.key(input, down, Instant::now());
        self.act(actions);
        self.draw();
    }

    fn tick(&mut self) {
        // The book is files anyone may drop in; look again now and then.
        if self.contacts_loaded.elapsed() > Duration::from_secs(20) {
            self.model.book = phone_core::contacts::Book::load(&phone_core::store::contacts_dir(), &self.model.region);
            self.contacts_loaded = Instant::now();
        }
        // While a call rings, the motor rings too (unless the ringer is silent).
        if matches!(self.model.top(), Top::Incoming(_))
            && self.ringer != "silent"
            && (self.vibrate || self.ringer == "vibrate")
            && self.last_ring_buzz.elapsed() >= Duration::from_millis(1500)
        {
            self.last_ring_buzz = Instant::now();
            self.buzz_ms(500);
        }
        self.draw();
    }

    fn event(&mut self, e: Event) {
        let mut actions = Vec::new();
        match e {
            Event::Connected(up) => {
                self.model.connected = up;
                if up {
                    self.reload();
                } else {
                    self.model.set_calls(Vec::new());
                    self.model.set_pending(Vec::new());
                }
            }
            Event::Calls(c) => self.model.set_calls(c),
            Event::CallAdded(c) | Event::CallChanged(c) => actions = self.model.call_changed(c),
            Event::CallRemoved(id) => {
                actions = self.model.call_removed(id);
                self.bus.refresh_recents(100);
            }
            Event::Recents(r) => self.model.set_recents(r),
            Event::Pending(p) => self.model.set_pending(p),
            Event::Requested(r) => self.model.requested(r),
            Event::Resolved(id, _) => self.model.resolved(id),
            Event::Status(s) => self.model.status = s,
            Event::Routes(r) => {
                if let Some(a) = r.iter().find(|r| r.active) {
                    self.model.route = phone_core::model::Route::from_u8(a.route);
                }
                self.model.routes = r;
            }
            Event::Route(r) => self.model.route = phone_core::model::Route::from_u8(r),
            Event::Ringer(r) => self.ringer = r,
            Event::Setting(k, v) if k == "vibrate" => self.vibrate = v == "true",
            Event::Setting(k, v) if k == "region" => self.model.region = v,
            Event::Ussd(text) => self.model.ussd_reply(text),
            Event::Error(text) => self.model.error(&text),
            Event::Placed { kind: "call", placed } if placed.pending == 0 => self.model.browse = model::Browse::Keypad,
            _ => {}
        }
        self.act(actions);
        self.draw();
    }

    fn act(&mut self, actions: Vec<Action>) {
        for a in actions {
            match a {
                Action::Dial(n) => self.bus.dial(n),
                Action::Answer(id) => self.bus.run(move |c| async move { c.calls.answer(id).await.map(|_| vec![]) }),
                Action::Hangup(id) => self.bus.run(move |c| async move { c.calls.hangup(id).await.map(|_| vec![]) }),
                Action::Swap => self.bus.run(|c| async move { c.calls.swap().await.map(|_| vec![]) }),
                Action::Dtmf(id, d) => {
                    self.bus.run(move |c| async move { c.calls.send_dtmf(id, &d).await.map(|_| vec![]) })
                }
                Action::Mute(m) => self.bus.run(move |c| async move { c.calls.set_muted(m).await.map(|_| vec![]) }),
                Action::Silence => self.bus.run(|c| async move { c.calls.silence().await.map(|_| vec![]) }),
                Action::CycleRoute => self.bus.run(|c| async move {
                    let r = c.audio.cycle_route().await?;
                    Ok(vec![Event::Route(r), Event::Routes(c.audio.routes().await?)])
                }),
                Action::Confirm(id) => {
                    self.bus.run(move |c| async move { c.confirm.confirm(id).await.map(|_| vec![]) })
                }
                Action::Deny(id) => self.bus.run(move |c| async move { c.confirm.deny(id).await.map(|_| vec![]) }),
                Action::Ussd(code) => {
                    self.bus.run(move |c| async move { Ok(vec![Event::Ussd(c.modem.ussd(&code).await?)]) })
                }
                Action::UssdRespond(r) => {
                    self.bus.run(move |c| async move { Ok(vec![Event::Ussd(c.modem.ussd_respond(&r).await?)]) })
                }
                Action::UssdCancel => self.bus.run(|c| async move { c.modem.ussd_cancel().await.map(|_| vec![]) }),
                Action::Text(peer) => open_messages(&peer),
                Action::Reply(to, body) => self.bus.send(to, body),
                Action::MarkSeen => self.bus.run(|c| async move { c.calls.mark_calls_seen().await.map(|_| vec![]) }),
                Action::Quit => {
                    let _ = slint::quit_event_loop();
                }
                Action::Buzz(Buzz::Tick) => self.buzz_ms(10),
                Action::Buzz(Buzz::Thump) => self.buzz_ms(80),
            }
        }
    }

    fn buzz_ms(&mut self, ms: u16) {
        #[cfg(feature = "device")]
        if let Some(m) = self.motor.as_mut() {
            // Effect 3 is the driver's plain buzz; the length is ours.
            m.play(3, ms);
        }
        let _ = ms;
    }

    fn draw(&mut self) {
        apply(&self.ui, &view::view(&self.model, phone_core::now(), self.frame), self.frame);
    }
}

fn apply(ui: &AppWindow, v: &view::View, frame: u32) {
    let strings = |s: &[String]| {
        ModelRc::new(VecModel::from(s.iter().map(|x| SharedString::from(x.as_str())).collect::<Vec<_>>()))
    };
    ui.set_buttons(strings(&v.buttons));
    ui.set_tick(frame as i32);
    match &v.body {
        view::Body::Canvas { surface, texts } => {
            ui.set_mode(0);
            ui.set_picture(flipctl_app::picture(surface));
            let texts: Vec<CanvasText> = texts
                .iter()
                .map(|t| CanvasText {
                    x: t.x as f32,
                    y: t.y as f32,
                    text: t.text.as_str().into(),
                    font: t.font,
                    align: t.align,
                    white: t.white,
                })
                .collect();
            ui.set_texts(ModelRc::new(VecModel::from(texts)));
        }
        view::Body::Menu { items, selected, scroll, breadcrumb } => {
            ui.set_mode(1);
            let items: Vec<ListItem> = items
                .iter()
                .map(|i| ListItem {
                    label: i.label.as_str().into(),
                    tag: i.tag.as_str().into(),
                    status: i.status.as_str().into(),
                    dim: i.dim,
                    frames: 1,
                    ..Default::default()
                })
                .collect();
            ui.set_total(items.len() as i32);
            ui.set_items(ModelRc::new(VecModel::from(items)));
            ui.set_selected(*selected as i32);
            ui.set_scroll(*scroll as i32);
            ui.set_breadcrumb(breadcrumb.as_str().into());
        }
        view::Body::Detail { rows, breadcrumb } => {
            ui.set_mode(2);
            let rows: Vec<DetailRow> = rows
                .iter()
                .map(|r| DetailRow {
                    kind: r.kind,
                    label: r.label.as_str().into(),
                    value: r.value.as_str().into(),
                    ..Default::default()
                })
                .collect();
            ui.set_rows(ModelRc::new(VecModel::from(rows)));
            ui.set_breadcrumb(breadcrumb.as_str().into());
        }
    }
    match &v.modal {
        Some(m) => {
            ui.set_modal_lines(strings(&m.lines));
            ui.set_modal_left(m.left.as_str().into());
            ui.set_modal_right(m.right.as_str().into());
        }
        None => ui.set_modal_lines(ModelRc::default()),
    }
}

/// Hand a number to Messages: leave it where Messages looks, and bring Messages up.
fn open_messages(peer: &str) {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("dolphone");
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join("compose"), peer);
    if let Some(bundle) = find_bundle("dolphone-messages") {
        let _ = std::process::Command::new("flipctl").arg("open").arg(bundle).spawn();
    }
}

fn find_bundle(stem: &str) -> Option<std::path::PathBuf> {
    let apps = std::path::PathBuf::from(std::env::var_os("HOME")?).join("Apps");
    let mut dirs = vec![apps.clone()];
    if let Ok(rd) = std::fs::read_dir(&apps) {
        dirs.extend(rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.is_dir()));
    }
    dirs.iter().find_map(|d| {
        std::fs::read_dir(d).ok()?.filter_map(|e| e.ok().map(|e| e.path())).find(|p| {
            p.file_name().is_some_and(|n| {
                let n = n.to_string_lossy();
                n.starts_with(stem) && n.ends_with(".AppImage")
            })
        })
    })
}

/// The touchpad as D-pad steps: a stroke moves the selection once per stride.
#[cfg(feature = "device")]
struct Swipe {
    pad: Option<flipctl_app::TouchpadSource>,
    origin: Option<(i32, i32)>,
}

#[cfg(feature = "device")]
impl Swipe {
    const STRIDE: i32 = 48;

    fn new() -> Self {
        Self { pad: flipctl_app::TouchpadSource::open().ok(), origin: None }
    }

    fn poll(&mut self) -> Vec<Input> {
        let mut out = Vec::new();
        let Some(pad) = self.pad.as_mut() else { return out };
        while let Some(t) = pad.poll() {
            if !t.down {
                self.origin = None;
                continue;
            }
            let (ox, oy) = *self.origin.get_or_insert((t.x, t.y));
            let (dx, dy) = (t.x - ox, t.y - oy);
            if dx.abs() >= Self::STRIDE || dy.abs() >= Self::STRIDE {
                out.push(if dx.abs() > dy.abs() {
                    if dx > 0 {
                        Input::Right
                    } else {
                        Input::Left
                    }
                } else if dy > 0 {
                    Input::Down
                } else {
                    Input::Up
                });
                self.origin = Some((t.x, t.y));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flipctl_app::theme;
    use flipper_ui::slint_render::{render_frame, FlipperSlintPlatform};
    use phone_api::{CallInfo, CallRecordInfo, Request, RouteInfo};
    use slint::platform::software_renderer::MinimalSoftwareWindow;
    use std::rc::Rc;

    fn panel() -> Rc<MinimalSoftwareWindow> {
        thread_local! {
            static WINDOW: Rc<MinimalSoftwareWindow> = FlipperSlintPlatform::install();
        }
        WINDOW.with(Rc::clone)
    }

    /// The screen as the panel receives it. `DOLPHONE_RENDER=1 cargo test` leaves the
    /// frames in target/render.
    fn shot(name: &str, m: &Model) -> Vec<u8> {
        let window = panel();
        let ui = AppWindow::new().expect("window");
        apply(&ui, &view::view(m, 1_793_000_000, 0), 0);
        ui.show().expect("show");
        slint::platform::update_timers_and_animations();
        let frame: Vec<u8> = render_frame(&window).expect("frame").iter().map(|p| p.0).collect();
        drop(ui);
        if std::env::var_os("DOLPHONE_RENDER").is_some() {
            let dir = std::path::Path::new("target/render");
            std::fs::create_dir_all(dir).unwrap();
            let f = std::fs::File::create(dir.join(format!("dialer-{name}.png"))).unwrap();
            let mut e =
                png::Encoder::new(std::io::BufWriter::new(f), u32::from(theme::PANEL_W), u32::from(theme::PANEL_H));
            e.set_color(png::ColorType::Grayscale);
            e.set_depth(png::BitDepth::Eight);
            e.write_header().unwrap().write_image_data(&frame).unwrap();
        }
        frame
    }

    fn book() -> phone_core::contacts::Book {
        let cards = "BEGIN:VCARD\nFN:Alex Example\nTEL;TYPE=CELL:0491570156\nEND:VCARD\nBEGIN:VCARD\nFN:Sam Sample\nTEL:0491570157\nEND:VCARD\nBEGIN:VCARD\nFN:Jordan Doe\nTEL:+61293744000\nEND:VCARD\n";
        let mut b = phone_core::contacts::Book { contacts: phone_core::contacts::parse(cards, "AU") };
        b.contacts.sort_by_key(|c| c.name.clone());
        b
    }

    fn model() -> Model {
        let mut m = Model::new("AU", book());
        m.connected = true;
        m.status.present = true;
        m.status.registered = true;
        m.status.operator = "Fakecom AU".into();
        m.routes = vec![RouteInfo { route: 0, name: "Speaker".into(), available: true, active: true }];
        m
    }

    fn call(id: u32, number: &str, state: CallState, dir: u8) -> CallInfo {
        CallInfo {
            id,
            number: number.into(),
            direction: dir,
            state: state as u8,
            connected_at: if state == CallState::Active { 1_792_999_917 } else { 0 },
            muted: false,
            emergency: false,
        }
    }

    use phone_core::model::CallState;

    fn ink(frame: &[u8]) -> usize {
        frame.iter().filter(|p| **p < 128).count()
    }

    #[test]
    fn keypad_with_a_number() {
        let mut m = model();
        m.number = "0491570156".into();
        m.sel = (1, 1);
        let f = shot("keypad", &m);
        assert!(ink(&f) > 2000, "the keypad draws");
    }

    #[test]
    fn keypad_idle_and_emergency() {
        let m = model();
        shot("keypad-idle", &m);
        let mut m = model();
        m.number = "000".into();
        shot("keypad-emergency", &m);
    }

    #[test]
    fn incoming_and_waiting() {
        let mut m = model();
        m.set_calls(vec![call(1, "+61491570156", CallState::RingingIn, 0)]);
        assert!(ink(&shot("incoming", &m)) > 500);
        let mut m = model();
        m.set_calls(vec![
            call(1, "+61491570157", CallState::Active, 0),
            call(2, "+61400000000", CallState::Waiting, 0),
        ]);
        shot("waiting", &m);
    }

    #[test]
    fn in_call_and_two_calls() {
        let mut m = model();
        m.set_calls(vec![call(1, "+61491570156", CallState::Active, 1)]);
        shot("in-call", &m);
        let mut m = model();
        m.set_calls(vec![call(1, "+61491570156", CallState::Active, 1), call(2, "+61491570157", CallState::Held, 0)]);
        shot("two-calls", &m);
        let mut m = model();
        m.set_calls(vec![call(1, "+61491570156", CallState::Active, 1)]);
        m.dtmf_open = true;
        m.dtmf = "1234#".into();
        shot("dtmf", &m);
    }

    #[test]
    fn confirm_modal() {
        let mut m = model();
        m.set_pending(vec![Request {
            id: 1,
            kind: "call".into(),
            target: "1900123456".into(),
            body: String::new(),
            requester: "python3".into(),
            reason: "premium".into(),
        }]);
        shot("confirm", &m);
    }

    #[test]
    fn recents_and_contacts() {
        let mut m = model();
        m.set_recents(vec![
            CallRecordInfo {
                id: 3,
                peer: "+61491570156".into(),
                direction: 0,
                started: 1_792_990_000,
                duration: 0,
                outcome: 1,
                seen: false,
            },
            CallRecordInfo {
                id: 2,
                peer: "+61491570199".into(),
                direction: 1,
                started: 1_792_900_000,
                duration: 125,
                outcome: 0,
                seen: true,
            },
            CallRecordInfo {
                id: 1,
                peer: "+61293744000".into(),
                direction: 0,
                started: 1_792_500_000,
                duration: 40,
                outcome: 0,
                seen: true,
            },
        ]);
        m.browse = model::Browse::Recents;
        shot("recents", &m);
        m.browse = model::Browse::Contacts;
        shot("contacts", &m);
        m.browse = model::Browse::Ussd {
            text: "Your balance is $12.34. Expires 30/11. 1. Recharge 2. Data packs".into(),
            awaiting: true,
        };
        shot("ussd", &m);
    }

    // Behaviour.

    #[test]
    fn typing_and_calling() {
        let mut m = model();
        let t = Instant::now();
        m.sel = (0, 0);
        assert_eq!(m.key(Input::Ok, true, t), vec![Action::Buzz(Buzz::Tick)]);
        m.key(Input::Char('2'), true, t);
        assert_eq!(m.number, "12");
        m.key(Input::Edit, true, t);
        assert_eq!(m.number, "1");
        let a = m.key(Input::Run, true, t);
        assert_eq!(a, vec![Action::Dial("1".into())]);
        assert!(m.number.is_empty());
    }

    #[test]
    fn long_zero_is_plus() {
        let mut m = model();
        m.sel = (3, 1);
        let t = Instant::now();
        m.key(Input::Ok, true, t);
        m.key(Input::Ok, false, t + Duration::from_millis(700));
        assert_eq!(m.number, "+");
        m.key(Input::Ok, true, t + Duration::from_secs(1));
        m.key(Input::Ok, false, t + Duration::from_millis(1100));
        assert_eq!(m.number, "+0");
    }

    #[test]
    fn ussd_codes_are_sent_not_dialled() {
        let mut m = model();
        m.number = "*100#".into();
        assert_eq!(m.key(Input::Run, true, Instant::now()), vec![Action::Ussd("*100#".into())]);
    }

    #[test]
    fn incoming_keys_and_fresh_guard() {
        let mut m = model();
        m.set_calls(vec![call(7, "+61491570156", CallState::RingingIn, 0)]);
        // A press the instant it appears is dropped.
        assert!(m.key(Input::Run, true, Instant::now()).is_empty());
        std::thread::sleep(model::FRESH);
        assert_eq!(m.key(Input::Run, true, Instant::now()), vec![Action::Answer(7)]);
        assert_eq!(m.key(Input::Escape, true, Instant::now()), vec![Action::Hangup(7)]);
        assert_eq!(m.key(Input::Ptt, true, Instant::now()), vec![Action::Silence]);
    }

    #[test]
    fn end_is_locked_out_after_answering() {
        let mut m = model();
        m.set_calls(vec![call(7, "+61491570156", CallState::RingingIn, 0)]);
        m.call_changed(call(7, "+61491570156", CallState::Active, 0));
        std::thread::sleep(model::FRESH);
        assert!(m.key(Input::Run, true, Instant::now()).is_empty(), "End right after answering is ignored");
        assert_eq!(m.key(Input::Run, true, Instant::now() + model::END_LOCKOUT), vec![Action::Hangup(7)]);
    }

    #[test]
    fn confirm_keys() {
        let mut m = model();
        m.set_pending(vec![Request {
            id: 4,
            kind: "sms".into(),
            target: "x".into(),
            body: "b".into(),
            requester: "r".into(),
            reason: "external".into(),
        }]);
        std::thread::sleep(model::FRESH);
        assert_eq!(m.key(Input::Run, true, Instant::now()), vec![Action::Confirm(4)]);
        assert_eq!(m.key(Input::Escape, true, Instant::now()), vec![Action::Deny(4)]);
    }

    #[test]
    fn canned_reply_sends_and_declines() {
        let mut m = model();
        m.set_calls(vec![call(9, "+61491570156", CallState::RingingIn, 0)]);
        std::thread::sleep(model::FRESH);
        m.key(Input::Edit, true, Instant::now());
        m.key(Input::Down, true, Instant::now());
        let a = m.key(Input::Run, true, Instant::now());
        assert_eq!(a, vec![Action::Reply("+61491570156".into(), model::CANNED[1].into()), Action::Hangup(9)]);
    }

    #[test]
    fn contacts_letter_jump() {
        let mut m = model();
        m.browse = model::Browse::Contacts;
        m.key(Input::Right, true, Instant::now());
        assert_eq!(m.book.contacts[m.contacts_sel].name, "Jordan Doe");
        m.key(Input::Right, true, Instant::now());
        assert_eq!(m.book.contacts[m.contacts_sel].name, "Sam Sample");
        m.key(Input::Left, true, Instant::now());
        assert_eq!(m.book.contacts[m.contacts_sel].name, "Jordan Doe");
    }
}
