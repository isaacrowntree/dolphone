//! Dolphone's Dialer, a FlipCTL app.
//!
//! The keypad, recent calls, contacts, the incoming and in-call screens, and the
//! confirmations phoned asks for. phoned raises it with `flipctl open` when a call
//! rings or an outside program asks to dial; the owner opens it from Apps > Phone.

mod model;
mod view;

use std::cell::RefCell;
use std::time::{Duration, Instant, SystemTime};

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
    /// A vibration pattern being played, and the timer that plays its next pulse.
    /// The timer runs only while a pattern does.
    player: phone_bus::haptics::Player,
    haptics: slint::Timer,
    last_ring_buzz: Instant,
    /// When the contacts directory was last looked at, and how it looked.
    contacts_checked: Instant,
    contacts_stamp: Option<(SystemTime, usize)>,
    /// The clock second and minute the screen was last drawn at, so the tick
    /// draws only when what it shows has moved on.
    drawn_at: i64,
    /// Whether the window had the keyboard at the last tick.
    focused: bool,
    /// The first call log and conversations have arrived (Today is chosen once
    /// both are in), or None once the first screen is chosen.
    first_load: Option<(bool, bool)>,
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
        player: Default::default(),
        haptics: slint::Timer::default(),
        last_ring_buzz: Instant::now(),
        contacts_checked: Instant::now(),
        contacts_stamp: phone_bus::files::stamp(&phone_core::store::contacts_dir()),
        drawn_at: 0,
        focused: true,
        first_load: Some((false, false)),
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
    let tick = std::rc::Rc::new(slint::Timer::default());
    let timer = std::rc::Rc::downgrade(&tick);
    tick.start(slint::TimerMode::Repeated, BUSY_TICK, move || {
        with_app(|app| {
            app.frame = app.frame.wrapping_add(1);
            app.tick();
            if app.focused {
                if let Some(now) = status.poll() {
                    flipctl_app::apply_status!(&app.ui, PanelStatus, now);
                }
            }
            // The pad is read straight from evdev, so it reports strokes meant for
            // whichever app is in front: only the one with the keyboard acts on
            // them, and the rest are thrown away rather than saved for later.
            #[cfg(feature = "device")]
            if app.focused {
                for input in pad.poll(app.model.keypad_showing()) {
                    app.input(input, true);
                    app.input(input, false);
                }
            } else {
                pad.discard();
            }
            // Four times a second while it is looked at or a call needs it (the
            // ring, the pad), once a second otherwise.
            let want = if app.focused || app.model.top() != Top::Browse { BUSY_TICK } else { IDLE_TICK };
            if let Some(t) = timer.upgrade().filter(|t| t.interval() != want) {
                t.set_interval(want);
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
                        // Back leaves the app (flipctl keeps it). The middle soft
                        // key (silkscreened "power", KEY_C) does reach us, but its
                        // slot is left empty on purpose: a press meant for the
                        // phone's power is never taken as Answer or End.
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
        self.bus.refresh_threads();
        self.bus.refresh_status();
        self.bus.refresh_routes();
        self.bus.run(|c| async move {
            let mut out = vec![
                Event::Ringer(c.audio.ringer().await?),
                Event::Missed(c.calls.missed().await?),
                Event::Unread(c.messages.unread().await?),
            ];
            let keys = ["vibrate", "region", "voicemail", "steady_decline", "long_press_ms"];
            for k in keys.into_iter().chain(phone_api::keys::SPEED.iter().map(|s| s.0)) {
                out.push(Event::Setting(k.into(), c.settings.get(k).await?));
            }
            Ok(out)
        });
    }

    fn input(&mut self, input: Input, down: bool) {
        let actions = self.model.key(input, down, Instant::now());
        self.act(actions);
        self.draw();
    }

    fn tick(&mut self) {
        let focused = window_focused(&self.ui);
        let refocused = focused && !self.focused;
        self.focused = focused;
        // The book is files anyone may drop in; phoned says when it changes them,
        // and for the rest a look at the directory (a stat, not a parse) now and
        // then says whether there is anything to read again.
        if self.contacts_checked.elapsed() > Duration::from_secs(20) {
            self.contacts_checked = Instant::now();
            let dir = phone_core::store::contacts_dir();
            let stamp = phone_bus::files::stamp(&dir);
            if stamp != self.contacts_stamp {
                self.contacts_stamp = stamp;
                self.model.book = phone_core::contacts::Book::load(&dir, &self.model.region);
            }
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
        // Keys and phoned's events draw as they come; the tick only draws what
        // moves by itself: the ring's frame, a call's timer, and the dates in the
        // lists once a minute. Nothing at all while another app is in front.
        let now = phone_core::now();
        let moving = match self.model.top() {
            Top::Incoming(_) => true,
            Top::InCall => now != self.drawn_at,
            _ => now / 60 != self.drawn_at / 60,
        };
        if refocused || (focused && moving) {
            self.draw();
        }
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
            Event::Recents(r) => {
                self.model.set_recents(r);
                self.first_screen(true, false);
            }
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
            Event::Ringer(r) => {
                self.model.ringer = r.clone();
                self.ringer = r;
            }
            Event::Setting(k, v) if k == "vibrate" => self.vibrate = v == "true",
            Event::Setting(k, v) => self.model.setting(&k, v),
            Event::Missed(n) => {
                actions = self.model.missed_changed(n);
                self.bus.refresh_recents(100);
            }
            Event::Threads(t) => {
                self.model.threads = t;
                self.first_screen(false, true);
            }
            Event::Unread(n) => {
                self.model.unread = n;
                self.bus.refresh_threads();
            }
            Event::MessageAdded(m) if m.direction == 0 => actions = vec![Action::Buzz(Buzz::Text)],
            Event::MessageChanged(_, s) if s == phone_core::model::MessageState::Failed as u8 => {
                actions = vec![Action::Buzz(Buzz::Failed)]
            }
            Event::Alert(a) => actions = self.model.alert_received(a),
            Event::ContactsChanged | Event::ContactsImported { .. } => {
                let dir = phone_core::store::contacts_dir();
                self.contacts_stamp = phone_bus::files::stamp(&dir);
                self.model.book = phone_core::contacts::Book::load(&dir, &self.model.region);
            }
            Event::Unlock(left) => self.model.unlocked(left),
            Event::Ussd(text) => self.model.ussd_reply(text),
            Event::Error(text) => self.model.error(&text),
            Event::Placed { kind: "call", placed } if placed.pending == 0 => self.model.browse = model::Browse::Keypad,
            _ => {}
        }
        self.act(actions);
        self.draw();
    }

    /// Open on Today when something is waiting, once there is enough to tell.
    fn first_screen(&mut self, recents: bool, threads: bool) {
        let Some((r, t)) = self.first_load.as_mut() else { return };
        *r |= recents;
        *t |= threads;
        if *r && *t {
            self.first_load = None;
            if self.model.number.is_empty() && self.model.browse == model::Browse::Keypad {
                self.model.opened();
            }
        }
    }

    fn act(&mut self, actions: Vec<Action>) {
        for a in actions {
            match a {
                Action::Dial(n) => self.bus.dial(n),
                Action::Answer(id) => self.bus.run(move |c| async move { c.calls.answer(id).await.map(|_| vec![]) }),
                Action::Hangup(id) => self.bus.run(move |c| async move { c.calls.hangup(id).await.map(|_| vec![]) }),
                Action::Swap => self.bus.run(|c| async move { c.calls.swap().await.map(|_| vec![]) }),
                Action::Merge => self.bus.run(|c| async move { c.calls.merge().await.map(|_| vec![]) }),
                Action::Unlock(pin) => {
                    self.bus.run(move |c| async move { Ok(vec![Event::Unlock(c.modem.unlock(&pin).await?)]) })
                }
                Action::SetSetting(k, v) => {
                    self.bus.run(move |c| async move { c.settings.set(&k, &v).await.map(|_| vec![]) })
                }
                Action::SetRinger(r) => {
                    self.ringer = r.clone();
                    self.bus.run(move |c| async move { c.audio.set_ringer(&r).await.map(|_| vec![]) })
                }
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
                Action::Buzz(b) => self.buzz(b),
            }
        }
    }

    /// Play a pattern. Key ticks always play; anything announcing an event obeys
    /// a silent ringer, except an emergency alert.
    fn buzz(&mut self, b: Buzz) {
        let quiet = self.ringer == "silent" && !matches!(b, Buzz::Tick | Buzz::Thump | Buzz::Refused | Buzz::Alert);
        if quiet {
            return;
        }
        self.player.start(b.pattern());
        self.haptics.stop();
        self.step_buzz();
    }

    /// Play the pattern's next pulse, and set the timer for the one after, if
    /// there is one.
    fn step_buzz(&mut self) {
        let Some((on, wait)) = self.player.pulse() else { return };
        self.buzz_ms(on);
        if let Some(wait) = wait {
            self.haptics.start(slint::TimerMode::SingleShot, wait, || with_app(App::step_buzz));
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
        let now = phone_core::now();
        self.drawn_at = now;
        apply(&self.ui, &view::view(&self.model, now, self.frame), self.frame);
    }
}

/// The tick while the Dialer is looked at or a call is up, and while it is not.
const BUSY_TICK: Duration = Duration::from_millis(250);
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
    let _ = std::fs::write(phone_bus::files::runtime_dir().join("compose"), peer);
    if let Some(bundle) = find_bundle("dolphone-messages") {
        if let Ok(mut child) = std::process::Command::new("flipctl").arg("open").arg(bundle).spawn() {
            // Waited for off the UI thread, so it does not linger as a zombie.
            std::thread::spawn(move || child.wait());
        }
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

/// The touchpad: a stroke moves the selection once per stride, and on a keypad a
/// tap (a touch that barely moved) presses the cell under the finger, once the
/// pad's range has been learnt.
#[cfg(feature = "device")]
struct Swipe {
    pad: Option<flipctl_app::TouchpadSource>,
    origin: Option<(i32, i32)>,
    moved: bool,
    map: model::PadMap,
}

#[cfg(feature = "device")]
impl Swipe {
    const STRIDE: i32 = 48;

    fn new() -> Self {
        Self { pad: flipctl_app::TouchpadSource::open().ok(), origin: None, moved: false, map: Default::default() }
    }

    /// Throw away what the pad reported while another app was in front, and
    /// forget any stroke half made.
    fn discard(&mut self) {
        if let Some(pad) = self.pad.as_mut() {
            while pad.poll().is_some() {}
        }
        self.origin = None;
        self.moved = false;
    }

    fn poll(&mut self, keypad: bool) -> Vec<Input> {
        let mut out = Vec::new();
        let Some(pad) = self.pad.as_mut() else { return out };
        while let Some(t) = pad.poll() {
            self.map.observe(t.x, t.y);
            if !t.down {
                if let (Some((ox, oy)), false, true) = (self.origin, self.moved, keypad) {
                    if let Some((r, c)) = self.map.cell(ox, oy, Self::STRIDE * 3) {
                        out.push(Input::Tap(r, c));
                    }
                }
                self.origin = None;
                self.moved = false;
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
                self.moved = true;
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
            internet: false,
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
                sim: String::new(),
            },
            CallRecordInfo {
                id: 2,
                peer: "+61491570199".into(),
                direction: 1,
                started: 1_792_900_000,
                duration: 125,
                outcome: 0,
                seen: true,
                sim: String::new(),
            },
            CallRecordInfo {
                id: 1,
                peer: "+61293744000".into(),
                direction: 0,
                started: 1_792_500_000,
                duration: 40,
                outcome: 0,
                seen: true,
                sim: String::new(),
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
        // 1 on an empty keypad types on release (a hold would be voicemail).
        assert_eq!(tap(&mut m, Input::Ok, t), vec![Action::Buzz(Buzz::Tick)]);
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
        // A press the instant it appears is dropped, and says so with a stutter.
        assert_eq!(m.key(Input::Run, true, Instant::now()), vec![Action::Buzz(Buzz::Refused)]);
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
        assert_eq!(
            m.key(Input::Run, true, Instant::now()),
            vec![Action::Buzz(Buzz::Refused)],
            "End right after answering is ignored, and felt to be"
        );
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

    fn alert_info(level: phone_core::model::AlertLevel) -> phone_api::AlertInfo {
        phone_api::AlertInfo {
            id: 1,
            channel: 4370,
            level: level as u8,
            text: "Emergency Alert: Bushfire. Leave now if you are in Example Valley. Follow police directions.".into(),
            at: 1_792_999_000,
        }
    }

    #[test]
    fn new_screens() {
        let mut m = model();
        m.missed = 1;
        m.unread = 2;
        shot("keypad-waiting-strip", &m);
        let mut m = model();
        m.alert_received(alert_info(phone_core::model::AlertLevel::Extreme));
        assert!(ink(&shot("alert", &m)) > 500);
        let mut m = model();
        m.status.state = "locked".into();
        m.number = "12".into();
        shot("sim-pin", &m);
        let mut m = model();
        m.browse = model::Browse::Contacts;
        m.speed[0] = "+61491570156".into();
        shot("contacts-speed", &m);
        m.browse = model::Browse::SpeedPick(1);
        shot("speed-pick", &m);
        let mut m = model();
        m.number = "567".into();
        shot("keypad-t9", &m);
        let mut m = model();
        m.number = "106".into();
        shot("keypad-relay", &m);
    }

    fn hold(m: &mut Model, input: Input, t: Instant) -> Vec<Action> {
        let mut a = m.key(input, true, t);
        a.extend(m.key(input, false, t + model::HOLD));
        a
    }

    fn tap(m: &mut Model, input: Input, t: Instant) -> Vec<Action> {
        let mut a = m.key(input, true, t);
        a.extend(m.key(input, false, t + Duration::from_millis(100)));
        a
    }

    #[test]
    fn hold_for_voicemail_and_speed_dial() {
        let mut m = model();
        let t = Instant::now();
        m.voicemail = "101".into();
        m.speed[1] = "+61491570157".into();
        m.sel = (0, 0);
        assert!(hold(&mut m, Input::Ok, t).contains(&Action::Dial("101".into())), "hold 1 is voicemail");
        m.sel = (0, 2);
        assert!(hold(&mut m, Input::Ok, t).contains(&Action::Dial("+61491570157".into())), "hold 3");
        m.sel = (0, 1);
        assert!(hold(&mut m, Input::Ok, t).iter().all(|a| !matches!(a, Action::Dial(_))));
        assert!(m.notice.is_some(), "an empty key says how to fill it");
        m.notice = None;
        m.sel = (0, 2);
        tap(&mut m, Input::Ok, t);
        assert_eq!(m.number, "3", "a short press still types");
        tap(&mut m, Input::Ok, t);
        assert_eq!(m.number, "33", "and types at once after the first digit");
    }

    #[test]
    fn assigning_speed_dial_from_contacts() {
        let mut m = model();
        let t = Instant::now();
        m.browse = model::Browse::Contacts;
        hold(&mut m, Input::Ok, t);
        assert_eq!(m.browse, model::Browse::SpeedPick(0));
        m.key(Input::Down, true, t);
        let a = m.key(Input::Run, true, t);
        assert_eq!(a, vec![Action::SetSetting("speed_3".into(), "+61491570156".into())]);
        assert_eq!(m.speed[1], "+61491570156");
        assert_eq!(m.browse, model::Browse::Contacts);
        assert_eq!(tap(&mut m, Input::Ok, t), vec![Action::Dial("+61491570156".into())], "a short Ok calls");
    }

    #[test]
    fn t9_finds_names() {
        let mut m = model();
        m.number = "567".into();
        assert_eq!(m.t9_match().map(|c| c.name.as_str()), Some("Jordan Doe"));
        assert_eq!(m.key(Input::Run, true, Instant::now()), vec![Action::Dial("+61293744000".into())]);
        m.number = "000".into();
        assert!(m.t9_match().is_none(), "never instead of an emergency number");
        m.number = "567890".into();
        assert!(m.t9_match().is_none(), "six digits is a number");
    }

    #[test]
    fn sim_pin_on_the_keypad() {
        let mut m = model();
        m.status.state = "locked".into();
        m.number = "1234".into();
        assert_eq!(m.key(Input::Run, true, Instant::now()), vec![Action::Unlock("1234".into())]);
        m.number = "000".into();
        assert_eq!(m.key(Input::Run, true, Instant::now()), vec![Action::Dial("000".into())], "000 works locked");
        m.unlocked(2);
        assert_eq!(m.notice.as_ref().unwrap()[1], "2 tries left");
    }

    #[test]
    fn alerts_take_the_screen_until_dismissed() {
        let mut m = model();
        assert_eq!(
            m.alert_received(alert_info(phone_core::model::AlertLevel::National)),
            vec![Action::Buzz(Buzz::Alert)]
        );
        assert!(matches!(m.top(), Top::Alert(_)));
        m.set_calls(vec![call(1, "+61491570156", CallState::RingingIn, 0)]);
        assert!(matches!(m.top(), Top::Incoming(_)), "a ringing call can still be answered");
        m.set_calls(Vec::new());
        std::thread::sleep(model::FRESH);
        m.key(Input::Run, true, Instant::now());
        assert_eq!(m.top(), Top::Browse);
        assert!(m.alert_received(alert_info(phone_core::model::AlertLevel::Info)).is_empty(), "info is quiet");
    }

    #[test]
    fn swap_merge_and_steady_decline() {
        let mut m = model();
        m.set_calls(vec![call(1, "+61491570156", CallState::Active, 1), call(2, "+61491570157", CallState::Held, 0)]);
        std::thread::sleep(model::FRESH);
        let t = Instant::now();
        assert_eq!(tap(&mut m, Input::Ok, t), vec![Action::Swap]);
        assert_eq!(hold(&mut m, Input::Ok, t), vec![Action::Merge]);

        let mut m = model();
        m.steady_decline = true;
        m.set_calls(vec![call(3, "+61491570156", CallState::RingingIn, 0)]);
        std::thread::sleep(model::FRESH);
        let t = Instant::now();
        assert_eq!(tap(&mut m, Input::Escape, t), vec![Action::Buzz(Buzz::Refused)], "a press does not decline");
        assert_eq!(m.hint, Some("Hold Decline to decline"));
        assert_eq!(hold(&mut m, Input::Escape, t), vec![Action::Hangup(3)]);
    }

    #[test]
    fn ptt_held_cycles_the_ringer() {
        let mut m = model();
        let t = Instant::now();
        assert!(hold(&mut m, Input::Ptt, t).contains(&Action::SetRinger("vibrate".into())));
        assert!(hold(&mut m, Input::Ptt, t).contains(&Action::SetRinger("silent".into())));
        assert!(hold(&mut m, Input::Ptt, t).contains(&Action::SetRinger("ring".into())));
    }

    #[test]
    fn haptics_tell_events_apart() {
        let mut m = model();
        m.set_calls(vec![call(1, "+61491570156", CallState::Active, 1)]);
        assert_eq!(m.call_removed(1), vec![Action::Buzz(Buzz::Dropped)], "the far end hanging up");
        m.set_calls(vec![call(2, "+61491570156", CallState::Active, 1)]);
        std::thread::sleep(model::FRESH);
        m.key(Input::Run, true, Instant::now() + model::END_LOCKOUT);
        assert_eq!(m.call_removed(2), vec![Action::Buzz(Buzz::Thump)], "ending it here");
        assert_eq!(m.missed_changed(1), vec![Action::Buzz(Buzz::Missed)]);
        assert!(m.missed_changed(0).is_empty());
        let patterns: Vec<_> =
            [Buzz::Text, Buzz::Missed, Buzz::Waiting, Buzz::Dropped, Buzz::Failed, Buzz::Alert, Buzz::Refused]
                .iter()
                .map(|b| b.pattern())
                .collect();
        for (i, a) in patterns.iter().enumerate() {
            assert!(patterns[i + 1..].iter().all(|b| a != b), "every pattern is its own");
        }
    }

    #[test]
    fn touchpad_taps_land_on_cells() {
        let mut map = model::PadMap::default();
        assert_eq!(map.cell(10, 10, 100), None, "not before the range is known");
        map.observe(0, 0);
        map.observe(299, 399);
        assert_eq!(map.cell(0, 0, 100), Some((0, 0)));
        assert_eq!(map.cell(299, 399, 100), Some((3, 2)));
        assert_eq!(map.cell(150, 250, 100), Some((2, 1)), "the 8");
        let mut m = model();
        assert_eq!(m.key(Input::Tap(2, 1), true, Instant::now()), vec![Action::Buzz(Buzz::Tick)]);
        assert_eq!(m.number, "8");
    }

    #[test]
    fn today_lists_what_is_waiting_and_opens_on_it() {
        let mut m = model();
        m.set_recents(vec![
            CallRecordInfo {
                id: 2,
                peer: "+61491570156".into(),
                direction: 0,
                started: 1_792_999_000,
                duration: 0,
                outcome: 1,
                seen: false,
                sim: String::new(),
            },
            CallRecordInfo {
                id: 1,
                peer: "+61491570156".into(),
                direction: 0,
                started: 1_792_998_000,
                duration: 0,
                outcome: 1,
                seen: false,
                sim: String::new(),
            },
        ]);
        m.threads = vec![phone_api::ThreadInfo {
            peer: "+61491570157".into(),
            last_body: "x".into(),
            last_at: 1_792_999_500,
            last_direction: 0,
            unread: 2,
        }];
        m.speed[0] = "+61293744000".into();
        let t = m.today();
        assert_eq!(t.len(), 3, "two missed calls from one caller are one line");
        assert!(matches!(t[0], model::TodayItem::Missed { count: 2, .. }));
        m.opened();
        assert_eq!(m.browse, model::Browse::Today(0));
        shot("today", &m);
        assert_eq!(m.key(Input::Run, true, Instant::now()), vec![Action::Dial("+61491570156".into())]);
        m.key(Input::Down, true, Instant::now());
        assert_eq!(m.key(Input::Run, true, Instant::now()), vec![Action::Text("+61491570157".into())]);
        m.key(Input::Escape, true, Instant::now());
        assert_eq!(m.browse, model::Browse::Keypad);

        let mut quiet = model();
        quiet.opened();
        assert_eq!(quiet.browse, model::Browse::Keypad, "nothing waiting: the keypad");
    }

    #[test]
    fn the_hold_time_is_the_owners() {
        let mut m = model();
        m.setting("long_press_ms", "1000".into());
        m.sel = (0, 1);
        m.speed[0] = "+61491570157".into();
        let t = Instant::now();
        m.key(Input::Ok, true, t);
        assert!(m.key(Input::Ok, false, t + Duration::from_millis(700)).iter().all(|a| !matches!(a, Action::Dial(_))));
        assert_eq!(m.number, "2", "0.7 s is a press when holds are 1 s");
        m.number.clear();
        m.key(Input::Ok, true, t);
        assert!(m
            .key(Input::Ok, false, t + Duration::from_millis(1000))
            .contains(&Action::Dial("+61491570157".into())));
        m.setting("long_press_ms", "50".into());
        assert_eq!(m.hold, Duration::from_millis(300), "kept within reason");
        m.setting("long_press_ms", "3000".into());
        assert_eq!(m.hold, Duration::from_millis(3000), "three seconds, for slow hands");
        m.setting("long_press_ms", "9000".into());
        assert_eq!(m.hold, Duration::from_millis(3000));
    }

    // Live: the real app against a running phoned and fake-mm (scripts/ui-e2e.sh
    // starts them). Keys go in through the app's own input path, phoned's signals
    // come back through its bus, and each step is rendered from the app's window.

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
            let ui = AppWindow::new().expect("window");
            let app = App {
                ui,
                model: Model::new("AU", phone_core::contacts::Book::load(&phone_core::store::contacts_dir(), "AU")),
                bus,
                frame: 0,
                vibrate: true,
                ringer: "ring".into(),
                #[cfg(feature = "device")]
                motor: None,
                player: Default::default(),
                haptics: slint::Timer::default(),
                last_ring_buzz: Instant::now(),
                contacts_checked: Instant::now(),
                contacts_stamp: None,
                drawn_at: 0,
                focused: true,
                // The live test starts on the keypad.
                first_load: None,
            };
            let mut live = Self { app, rx };
            live.until("phoned connects", |m| m.connected && m.status.present);
            Some(live)
        }

        /// Pass phoned's events to the app until `done`, or fail after 10 s.
        fn until(&mut self, what: &str, done: impl Fn(&Model) -> bool) {
            let end = Instant::now() + Duration::from_secs(10);
            while !done(&self.app.model) {
                let left = end.saturating_duration_since(Instant::now());
                assert!(!left.is_zero(), "timed out waiting: {what}");
                if let Ok(e) = self.rx.recv_timeout(left.min(Duration::from_millis(100))) {
                    self.app.event(e);
                }
            }
        }

        /// Let events settle (for steps whose effect is elsewhere).
        fn settle(&mut self, ms: u64) {
            let end = Instant::now() + Duration::from_millis(ms);
            while Instant::now() < end {
                if let Ok(e) = self.rx.recv_timeout(Duration::from_millis(20)) {
                    self.app.event(e);
                }
            }
        }

        fn press(&mut self, i: Input) {
            self.app.input(i, true);
            self.app.input(i, false);
        }

        fn shot(&mut self, name: &str) -> Vec<u8> {
            let window = panel();
            self.app.draw();
            self.app.ui.show().expect("show");
            slint::platform::update_timers_and_animations();
            window.request_redraw();
            let frame: Vec<u8> = render_frame(&window).expect("frame").iter().map(|p| p.0).collect();
            let dir = std::path::Path::new("target/render/live");
            std::fs::create_dir_all(dir).unwrap();
            let f = std::fs::File::create(dir.join(format!("dialer-{name}.png"))).unwrap();
            let mut e =
                png::Encoder::new(std::io::BufWriter::new(f), u32::from(theme::PANEL_W), u32::from(theme::PANEL_H));
            e.set_color(png::ColorType::Grayscale);
            e.set_depth(png::BitDepth::Eight);
            e.write_header().unwrap().write_image_data(&frame).unwrap();
            frame
        }
    }

    fn phonectl(args: &[&str]) -> String {
        let out = std::process::Command::new("phonectl").args(args).output().expect("phonectl on PATH");
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
    }

    #[test]
    #[ignore = "needs phoned and fake-mm: scripts/ui-e2e.sh"]
    fn live_dialer() {
        let Some(mut l) = Live::start() else { return };
        l.shot("01-keypad");

        // A call rings: the Dialer shows it, and Run answers it.
        phonectl(&["fake", "ring", "+61491570156"]);
        l.until("the call rings", |m| matches!(m.top(), Top::Incoming(_)));
        assert!(ink(&l.shot("02-incoming")) > 500);
        std::thread::sleep(model::FRESH);
        l.press(Input::Run);
        l.until("answered", |m| m.current().is_some_and(|c| CallState::from_u8(c.state) == CallState::Active));
        l.shot("03-in-call");

        // The in-call keypad sends tones to the network. Keys are ignored for a
        // moment after a screen appears by itself, as they would be for a person.
        std::thread::sleep(model::FRESH);
        l.press(Input::View);
        l.press(Input::Ok);
        l.settle(300);
        l.shot("04-in-call-keypad");
        let dump = phonectl(&["fake", "dump"]);
        assert!(dump.contains("dtmf=1"), "the tone reached the modem: {dump}");
        l.press(Input::Escape);

        // End, once the lockout after answering has passed.
        std::thread::sleep(model::END_LOCKOUT);
        l.press(Input::Run);
        l.until("the call ends", |m| m.calls.is_empty());
        l.until("the call is in Recents", |m| !m.recents.is_empty());
        l.press(Input::View);
        l.shot("05-recents");
        l.press(Input::Escape);

        // Typing a number and calling it: the far end answers.
        phonectl(&["fake", "autoanswer", "800"]);
        for c in "0491570157".chars() {
            l.app.input(Input::Char(c), true);
        }
        l.shot("06-typed");
        l.press(Input::Run);
        l.until("it connects", |m| m.current().is_some_and(|c| CallState::from_u8(c.state) == CallState::Active));
        std::thread::sleep(model::END_LOCKOUT);
        l.press(Input::Run);
        l.until("hung up", |m| m.calls.is_empty());
        phonectl(&["fake", "autoanswer", "0"]);

        // Another program wants to call a premium number: the Dialer asks.
        let placed = phonectl(&["dial", "1900123456"]);
        assert!(placed.contains("waiting for confirmation"), "{placed}");
        l.until("the confirmation shows", |m| matches!(m.top(), Top::Confirm(_)));
        l.shot("07-confirm");
        std::thread::sleep(model::FRESH);
        l.press(Input::Escape);
        l.until("denied", |m| m.pending.is_empty());

        // An emergency alert takes the screen until it is dismissed.
        phonectl(&["fake", "broadcast", "4370", "Emergency Alert: this is a test of the live UI."]);
        l.until("the alert shows", |m| matches!(m.top(), Top::Alert(_)));
        l.shot("08-alert");
        std::thread::sleep(model::FRESH);
        l.press(Input::Run);
        assert_eq!(l.app.model.top(), Top::Browse);

        // A locked SIM: the keypad takes the PIN.
        phonectl(&["fake", "lock", "on"]);
        l.until("locked", |m| m.locked());
        for c in "1234".chars() {
            l.app.input(Input::Char(c), true);
        }
        l.shot("09-sim-pin");
        l.press(Input::Run);
        l.until("unlocked", |m| !m.locked());
    }
}
