//! Dolphone's Messages, a FlipCTL app.

mod model;
mod view;

use std::cell::RefCell;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use flipctl_app::keyboard::{Exit, TextInput};
use flipctl_app::{Key, StatusSource};
use model::{Action, Input, Model, Writing};
use phone_bus::{Bus, Event};
use slint::{ModelRc, SharedString, VecModel};

slint::include_modules!();

struct App {
    ui: AppWindow,
    model: Model,
    bus: Bus,
    kb: Option<(Writing, TextInput)>,
    frame: u32,
    #[cfg(feature = "device")]
    motor: Option<flipctl_app::Haptic>,
    /// phoned's ringer: on Silent a text arrives without a buzz.
    ringer: String,
    /// The vibration being played, and the timer for its next pulse (running
    /// only while there is one).
    player: phone_bus::haptics::Player,
    haptics: slint::Timer,
    /// When the contacts directory was last looked at, and how it looked.
    contacts_checked: Instant,
    contacts_stamp: Option<(SystemTime, usize)>,
    /// Where the Dialer leaves a number to text, and the change time of the last
    /// one read, so a file that could not be removed is not read again.
    handoff: PathBuf,
    handoff_seen: Option<SystemTime>,
    /// Whether the window had the keyboard at the last tick, and the clock
    /// minute of the last draw (the dates in the lists move on by the minute).
    focused: bool,
    drawn_minute: i64,
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

/// Where the Dialer leaves a number to text.
fn handoff() -> PathBuf {
    phone_bus::files::runtime_dir().join("compose")
}

/// The tick while the keyboard is up and in front (the touchpad types, the
/// caret blinks, the keys animate), and otherwise.
const KEYBOARD_TICK: Duration = Duration::from_millis(100);
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

fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    let region = phone_core::number::DEFAULT_REGION.to_string();
    let book = phone_core::contacts::Book::load(&phone_core::store::contacts_dir(), &region);
    let bus = Bus::start(|e| {
        let _ = slint::invoke_from_event_loop(move || with_app(|app| app.event(e)));
    });
    APP.with(|a| *a.borrow_mut() = Some(App::new(ui.clone_strong(), Model::new(&region, book), bus)));
    with_app(|app| {
        app.reload();
        app.check_handoff();
        app.draw();
    });

    let mut status = StatusSource::new(Duration::from_secs(2));
    flipctl_app::apply_status!(&ui, PanelStatus, status.current());
    #[cfg(feature = "device")]
    let mut pad = flipctl_app::TouchpadSource::open().ok();
    let tick = std::rc::Rc::new(slint::Timer::default());
    let timer = std::rc::Rc::downgrade(&tick);
    tick.start(slint::TimerMode::Repeated, IDLE_TICK, move || {
        with_app(|app| {
            app.frame = app.frame.wrapping_add(1);
            let focused = window_focused(&app.ui);
            let refocused = focused && !app.focused;
            app.focused = focused;
            let typing = focused && app.kb.is_some();
            // The pad is read straight from evdev, so it reports strokes meant for
            // whichever app is in front: they type only into this app's keyboard
            // while it is in front, and are thrown away otherwise rather than
            // saved up for the next time the keyboard opens.
            #[cfg(feature = "device")]
            if let Some(pad) = pad.as_mut() {
                while let Some(t) = pad.poll() {
                    let Some((_, field)) = app.kb.as_mut().filter(|_| typing) else { continue };
                    if field.touch(t) {
                        if let Some(m) = app.motor.as_mut() {
                            m.play(3, 8);
                        }
                    }
                }
            }
            // The rest, about once a second.
            let second = !typing || app.frame % 10 == 0;
            if second {
                app.check_contacts();
            }
            if focused && (second || refocused) {
                app.check_handoff();
                if let Some(now) = status.poll() {
                    flipctl_app::apply_status!(&app.ui, PanelStatus, now);
                }
            }
            // Keys and phoned's events draw as they come. The tick draws only the
            // keyboard, whose caret blinks and whose keys animate, and the dates
            // once a minute, and only while it is in front.
            let keyboard = typing && (app.kb.as_mut().is_some_and(|(_, f)| f.animating()) || app.frame % 5 == 0);
            if refocused || keyboard || focused && phone_core::now() / 60 != app.drawn_minute {
                app.draw();
            }
            let want = if typing { KEYBOARD_TICK } else { IDLE_TICK };
            if let Some(t) = timer.upgrade().filter(|t| t.interval() != want) {
                t.set_interval(want);
            }
        });
    });

    // A key can open the keyboard, which wants the quick tick straight away.
    let pace = std::rc::Rc::downgrade(&tick);
    ui.on_keyed(move |text, down| {
        let mut typing = false;
        with_app(|app| {
            let key = Key::from_slint(text.as_str());
            if let Some(k) = key {
                app.ui.set_pressed_slot(match (down, k.soft_slot()) {
                    (true, Some(slot)) => slot as i32,
                    _ => -1,
                });
            }
            app.key(key, text.as_str(), down);
            typing = app.kb.is_some();
        });
        if let Some(t) = pace.upgrade().filter(|t| typing && t.interval() != KEYBOARD_TICK) {
            t.set_interval(KEYBOARD_TICK);
        }
    });

    ui.run()
}

impl App {
    fn new(ui: AppWindow, model: Model, bus: Bus) -> Self {
        Self {
            ui,
            model,
            bus,
            kb: None,
            frame: 0,
            #[cfg(feature = "device")]
            motor: flipctl_app::Haptic::open().ok(),
            ringer: "ring".into(),
            player: Default::default(),
            haptics: slint::Timer::default(),
            contacts_checked: Instant::now(),
            contacts_stamp: phone_bus::files::stamp(&phone_core::store::contacts_dir()),
            handoff: handoff(),
            handoff_seen: None,
            focused: true,
            drawn_minute: 0,
        }
    }

    fn reload(&mut self) {
        // The lock first: a list refused for it says so too, and either locks.
        self.bus.refresh_lock();
        self.bus.refresh_threads();
        self.bus.run(|c| async move {
            Ok(vec![
                Event::Ringer(c.audio.ringer().await?),
                Event::Blocked(c.settings.blocked().await?),
                Event::Setting("region".into(), c.settings.get("region").await?),
                Event::Missed(c.calls.missed().await?),
            ])
        });
        if !self.model.peer.is_empty() {
            self.bus.load_conversation(self.model.peer.clone());
        }
    }

    /// The book is files anyone may drop in; phoned says when it changes them, and
    /// for the rest a look at the directory (a stat, not a parse) every 20 s says
    /// whether there is anything to read again.
    fn check_contacts(&mut self) {
        if self.contacts_checked.elapsed() < Duration::from_secs(20) {
            return;
        }
        self.contacts_checked = Instant::now();
        let dir = phone_core::store::contacts_dir();
        let stamp = phone_bus::files::stamp(&dir);
        if stamp != self.contacts_stamp {
            self.contacts_stamp = stamp;
            self.model.book = phone_core::contacts::Book::load(&dir, &self.model.region);
        }
    }

    /// A number the Dialer left to text. Looked for with a stat; read only when
    /// there is a file, and one not already read.
    fn check_handoff(&mut self) {
        let Some(changed) = std::fs::metadata(&self.handoff).ok().and_then(|m| m.modified().ok()) else { return };
        if self.handoff_seen == Some(changed) {
            return;
        }
        self.handoff_seen = Some(changed);
        let path = self.handoff.clone();
        if let Ok(peer) = std::fs::read_to_string(&path) {
            let _ = std::fs::remove_file(&path);
            let peer = peer.trim().to_string();
            if !peer.is_empty() {
                self.kb = None;
                let actions = self.model.compose_to(&peer);
                self.act(actions);
                self.draw();
            }
        }
    }

    fn key(&mut self, key: Option<Key>, text: &str, down: bool) {
        if let Some((what, field)) = self.kb.as_mut() {
            let what = what.clone();
            let pin = what == Writing::Pin;
            // Done takes a PIN only once it is one phoned could take.
            let valid = !pin || Model::valid_pin(&field.text);
            match key {
                Some(k) if !down => {
                    field.release(k.flipper());
                }
                Some(k) => match field.key(k.flipper(), valid) {
                    Some(Exit::Save(t)) => {
                        self.kb = None;
                        let a = self.model.written(what, Some(t));
                        self.act(a);
                    }
                    Some(Exit::Cancel) => {
                        self.kb = None;
                        let a = self.model.written(what, None);
                        self.act(a);
                    }
                    None => {}
                },
                // A real keyboard (USB, Bluetooth, the remote view) types straight in.
                None if down => {
                    if let Some(c) = text
                        .chars()
                        .next()
                        .filter(|c| text.chars().count() == 1 && !c.is_control())
                        .filter(|c| !pin || (c.is_ascii_digit() && field.text.len() < 12))
                    {
                        let at =
                            field.text.char_indices().nth(field.cursor).map(|(i, _)| i).unwrap_or(field.text.len());
                        field.text.insert(at, c);
                        field.cursor += 1;
                    }
                }
                None => {}
            }
            self.draw();
            return;
        }
        if !down {
            return;
        }
        let input = match key {
            Some(Key::Up) => Input::Up,
            Some(Key::Down) => Input::Down,
            Some(Key::Left) => Input::Left,
            Some(Key::Right) => Input::Right,
            Some(Key::Ok) => Input::Ok,
            Some(Key::Escape) => Input::Escape,
            Some(Key::View) => Input::View,
            Some(Key::Edit) => Input::Edit,
            Some(Key::Run) => Input::Run,
            _ => return,
        };
        let rows = view::conversation_rows(&self.model, phone_core::now()).len();
        let a = self.model.key(input, rows, view::VISIBLE);
        self.act(a);
        self.draw();
    }

    fn event(&mut self, e: Event) {
        let mut actions = Vec::new();
        match e {
            Event::Connected(up) => {
                self.model.connected = up;
                if up {
                    self.reload();
                }
            }
            Event::Threads(t) => self.model.set_threads(t),
            Event::Conversation(peer, m) => self.model.set_conversation(&peer, m),
            Event::MessageAdded(m) => {
                // Two short, as the Dialer buzzes for a text, and not on Silent.
                if m.direction == 0 && self.ringer != "silent" {
                    self.player.start(phone_bus::haptics::TEXT);
                    self.haptics.stop();
                    self.step_buzz();
                }
                actions = self.model.message_added(m);
            }
            Event::Ringer(r) => self.ringer = r,
            Event::MessageChanged(id, state) => self.model.message_changed(id, state),
            Event::Blocked(b) => self.model.blocked = b,
            Event::Missed(n) => self.model.missed = n,
            Event::Found(q, found) if q == self.model.query => self.model.found = found,
            Event::ContactsChanged | Event::ContactsImported { .. } => {
                let dir = phone_core::store::contacts_dir();
                self.contacts_stamp = phone_bus::files::stamp(&dir);
                self.model.book = phone_core::contacts::Book::load(&dir, &self.model.region);
            }
            Event::Setting(k, v) if k == "region" => self.model.region = v,
            Event::Placed { kind: "sms", placed } if placed.pending != 0 => {
                self.model.notice = Some(vec!["Waiting for you to confirm".into(), "on the Phone screen.".into()]);
            }
            Event::Locked(locked) => {
                if !locked && self.kb.as_ref().is_some_and(|(w, _)| *w == Writing::Pin) {
                    // Opened elsewhere (the desktop): the PIN is not wanted now.
                    self.kb = None;
                }
                actions = self.model.set_locked(locked);
            }
            // A list refused for the lock is the PIN screen, not an error.
            Event::Error(text) if Model::is_lock_error(&text) => actions = self.model.set_locked(true),
            Event::Notice(text) | Event::Error(text) => self.model.notice = Some(wrap_notice(&text)),
            _ => {}
        }
        self.act(actions);
        self.draw();
    }

    fn act(&mut self, actions: Vec<Action>) {
        for a in actions {
            match a {
                Action::LoadThreads => self.bus.refresh_threads(),
                Action::Load(peer) => self.bus.load_conversation(peer),
                Action::MarkRead(peer) => {
                    self.bus.run(move |c| async move { c.messages.mark_read(&peer).await.map(|_| vec![]) })
                }
                Action::Send(to, body) => self.bus.send(to, body),
                Action::Write(what, text) => {
                    let title = match what {
                        Writing::To => "To (number or name)".to_string(),
                        Writing::Search => "Search texts".to_string(),
                        Writing::Body => format!("To {}", self.model.name_for(&self.model.peer)),
                        Writing::Pin => "Messages PIN".to_string(),
                    };
                    let mut field = TextInput::new(&flipctl_app::font::fit(&title, 200), &text);
                    if what == Writing::Pin {
                        numeric(&mut field);
                    }
                    self.kb = Some((what, field));
                }
                Action::Unlock(pin) => self.bus.unlock(pin),
                Action::Call(peer) => call(&peer, &self.bus),
                Action::Block(peer) => self.bus.run(move |c| async move {
                    c.settings.block(&peer).await?;
                    Ok(vec![Event::Blocked(c.settings.blocked().await?)])
                }),
                Action::Unblock(peer) => self.bus.run(move |c| async move {
                    c.settings.unblock(&peer).await?;
                    Ok(vec![Event::Blocked(c.settings.blocked().await?)])
                }),
                Action::Delete(peer) => self.bus.run(move |c| async move {
                    c.messages.delete_thread(&peer).await?;
                    Ok(vec![Event::Threads(c.messages.threads().await?)])
                }),
                Action::Search(q) => self
                    .bus
                    .run(move |c| async move { Ok(vec![Event::Found(q.clone(), c.messages.search(&q, 100).await?)]) }),
                Action::Retry(id) => self.bus.run(move |c| async move { c.messages.retry(id).await.map(|_| vec![]) }),
                Action::Quit => {
                    let _ = slint::quit_event_loop();
                }
            }
        }
    }

    /// Play the pattern's next pulse, and set the timer for the one after, if
    /// there is one.
    fn step_buzz(&mut self) {
        let Some((on, wait)) = self.player.pulse() else { return };
        #[cfg(feature = "device")]
        if let Some(m) = self.motor.as_mut() {
            // Effect 3 is the driver's plain buzz; the length is ours.
            m.play(3, on);
        }
        let _ = on;
        if let Some(wait) = wait {
            self.haptics.start(slint::TimerMode::SingleShot, wait, || with_app(App::step_buzz));
        }
    }

    fn draw(&mut self) {
        self.drawn_minute = phone_core::now() / 60;
        let cursor = self.kb.as_ref().map(|(_, f)| f.cursor_visible()).unwrap_or(false);
        apply(
            &self.ui,
            &view::view(&self.model, phone_core::now()),
            self.kb.as_ref().map(|(w, f)| (f, *w == Writing::Pin)),
            cursor,
            self.frame,
        );
    }
}

/// Calls go through phoned like any outside request would; the Dialer comes up
/// for the call itself.
fn call(peer: &str, bus: &Bus) {
    bus.dial(peer.to_string());
}

/// FlipCTL's keyboard has no number pad; its 123 layout, with the digits on top
/// and the highlight on 1, is the nearest.
fn numeric(field: &mut TextInput) {
    use flipctl_app::FlipperKey;
    field.key(FlipperKey::View, true);
    field.key(FlipperKey::Up, true);
}

/// The field as drawn for a secret: a star for each character, laid out as the
/// keyboard lays out text, so the caret sits where it should.
fn masked(field: &TextInput, v: &mut flipctl_app::keyboard::View) {
    use flipctl_app::keyboard::{field_w, fit_input};
    let stars = "*".repeat(field.text.chars().count());
    let w = field_w(i32::from(flipctl_app::font::TITLE.text_width(&stars)));
    let fitted = fit_input(&stars, field.cursor, w - 2 * flipctl_app::theme::metric::KB_INPUT_PAD);
    v.text = fitted.visible;
    v.field_w = w as f32;
    v.cursor_dx = fitted.cursor_dx as f32;
}

fn wrap_notice(text: &str) -> Vec<String> {
    view::wrap(text, 200).into_iter().take(3).collect()
}

/// Draw. `kb` is the keyboard when it is up, and whether what it takes is secret.
fn apply(ui: &AppWindow, v: &view::View, kb: Option<(&TextInput, bool)>, cursor_on: bool, frame: u32) {
    let strings = |s: &[String]| {
        ModelRc::new(VecModel::from(s.iter().map(|x| SharedString::from(x.as_str())).collect::<Vec<_>>()))
    };
    ui.set_tick(frame as i32);
    if let Some((field, secret)) = kb {
        ui.set_mode(3);
        let mut k = field.view("", cursor_on);
        if secret {
            masked(field, &mut k);
        }
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
        ui.set_buttons(strings(&["Cancel".into(), String::new(), String::new(), String::new(), "Done".into()]));
        ui.set_modal_lines(ModelRc::default());
        return;
    }
    ui.set_buttons(strings(&v.buttons));
    match &v.body {
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
        view::Body::Detail { rows, offset, breadcrumb } => {
            ui.set_mode(2);
            let total = rows.len();
            let rows: Vec<DetailRow> = rows
                .iter()
                .map(|r| DetailRow {
                    kind: r.kind,
                    label: r.label.as_str().into(),
                    value: r.value.as_str().into(),
                    dim: r.dim,
                    ..Default::default()
                })
                .collect();
            ui.set_rows(ModelRc::new(VecModel::from(rows)));
            ui.set_offset(*offset as i32);
            ui.set_at_start(*offset == 0);
            ui.set_at_end(offset + view::VISIBLE >= total);
            ui.set_breadcrumb(breadcrumb.as_str().into());
        }
    }
    match &v.modal {
        Some((lines, left, right)) => {
            ui.set_modal_lines(strings(lines));
            ui.set_modal_left(left.as_str().into());
            ui.set_modal_right(right.as_str().into());
        }
        None => ui.set_modal_lines(ModelRc::default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flipctl_app::theme;
    use flipper_ui::slint_render::{render_frame, FlipperSlintPlatform};
    use phone_api::{MessageInfo, ThreadInfo};
    use slint::platform::software_renderer::MinimalSoftwareWindow;
    use std::rc::Rc;

    fn panel() -> Rc<MinimalSoftwareWindow> {
        thread_local! {
            static WINDOW: Rc<MinimalSoftwareWindow> = FlipperSlintPlatform::install();
        }
        WINDOW.with(Rc::clone)
    }

    fn shot(name: &str, m: &Model, kb: Option<(&TextInput, bool)>) {
        let window = panel();
        let ui = AppWindow::new().expect("window");
        apply(&ui, &view::view(m, 1_793_000_000), kb, true, 0);
        ui.show().expect("show");
        slint::platform::update_timers_and_animations();
        let frame: Vec<u8> = render_frame(&window).expect("frame").iter().map(|p| p.0).collect();
        drop(ui);
        assert!(frame.iter().filter(|p| **p < 128).count() > 200, "{name} draws something");
        if std::env::var_os("DOLPHONE_RENDER").is_some() {
            let dir = std::path::Path::new("target/render");
            std::fs::create_dir_all(dir).unwrap();
            let f = std::fs::File::create(dir.join(format!("messages-{name}.png"))).unwrap();
            let mut e =
                png::Encoder::new(std::io::BufWriter::new(f), u32::from(theme::PANEL_W), u32::from(theme::PANEL_H));
            e.set_color(png::ColorType::Grayscale);
            e.set_depth(png::BitDepth::Eight);
            e.write_header().unwrap().write_image_data(&frame).unwrap();
        }
    }

    fn model() -> Model {
        let cards = "BEGIN:VCARD\nFN:Alex Example\nTEL:0491570156\nEND:VCARD\n";
        let book = phone_core::contacts::Book { contacts: phone_core::contacts::parse(cards, "AU") };
        let mut m = Model::new("AU", book);
        m.connected = true;
        m
    }

    #[test]
    fn screens() {
        let mut m = model();
        m.set_threads(vec![
            ThreadInfo {
                peer: "+61491570156".into(),
                last_body: "Running 10 min late, sorry!".into(),
                last_at: 1_792_999_000,
                last_direction: 0,
                unread: 2,
            },
            ThreadInfo {
                peer: "Telstra".into(),
                last_body: "Your bill is ready".into(),
                last_at: 1_792_800_000,
                last_direction: 0,
                unread: 0,
            },
            ThreadInfo {
                peer: "+61491570199".into(),
                last_body: "see you then".into(),
                last_at: 1_792_500_000,
                last_direction: 1,
                unread: 0,
            },
        ]);
        shot("threads", &m, None);
        m.open("+61491570156");
        m.set_conversation(
            "+61491570156",
            vec![
                MessageInfo {
                    id: 1,
                    peer: "+61491570156".into(),
                    direction: 0,
                    body: "Hey, are we still on for lunch tomorrow at the usual place?".into(),
                    at: 1_792_998_000,
                    state: 0,
                    read: true,
                    sim: String::new(),
                    parts: Vec::new(),
                },
                MessageInfo {
                    id: 2,
                    peer: "+61491570156".into(),
                    direction: 1,
                    body: "Yes! 12:30 works.".into(),
                    at: 1_792_998_100,
                    state: 2,
                    read: true,
                    sim: String::new(),
                    parts: Vec::new(),
                },
                MessageInfo {
                    id: 3,
                    peer: "+61491570156".into(),
                    direction: 0,
                    body: "Running 10 min late, sorry!".into(),
                    at: 1_792_999_000,
                    state: 0,
                    read: true,
                    sim: String::new(),
                    parts: Vec::new(),
                },
            ],
        );
        shot("conversation", &m, None);
        m.written(Writing::Body, Some("No worries, see you soon. I'll grab us a table by the window.".into()));
        shot("preview", &m, None);
        let field = TextInput::new("To Alex Example", "On my way");
        shot("compose", &m, Some((&field, false)));
        m.screen = model::Screen::Options;
        shot("options", &m, None);
        m.screen = model::Screen::QuickReply;
        m.quick_sel = 3;
        shot("quick-reply", &m, None);
        m.messages[1].state = phone_core::model::MessageState::Delivered as u8;
        m.messages.push(MessageInfo {
            id: 4,
            peer: "+61491570156".into(),
            direction: 1,
            body: "Grabbing a table.".into(),
            at: 1_792_999_100,
            state: phone_core::model::MessageState::Failed as u8,
            read: true,
            sim: String::new(),
            parts: Vec::new(),
        });
        m.screen = model::Screen::Conversation;
        m.scroll = None;
        shot("conversation-delivered-failed", &m, None);
        m.screen = model::Screen::Options;
        shot("options-resend", &m, None);
        m.screen = model::Screen::Threads;
        m.missed = 1;
        shot("threads-missed", &m, None);

        // Locked: the PIN on the 123 keyboard, starred, over nothing of the texts.
        m.set_locked(true);
        let mut pin = TextInput::new("Messages PIN", "");
        numeric(&mut pin);
        for _ in 0..4 {
            pin.key(flipctl_app::FlipperKey::Right, true);
            pin.key(flipctl_app::FlipperKey::Ok, true);
        }
        assert_eq!(pin.text, "2345", "the digits are where the highlight starts");
        shot("locked", &m, Some((&pin, true)));
        shot("locked-behind", &m, None);
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
            let model = Model::new("AU", phone_core::contacts::Book::load(&phone_core::store::contacts_dir(), "AU"));
            let app = App::new(AppWindow::new().expect("window"), model, bus);
            // The live test leaves the motor alone.
            #[cfg(feature = "device")]
            let app = App { motor: None, ..app };
            let mut l = Self { app, rx };
            l.until("phoned connects", |a| a.model.connected);
            Some(l)
        }

        fn until(&mut self, what: &str, done: impl Fn(&App) -> bool) {
            let end = Instant::now() + Duration::from_secs(10);
            while !done(&self.app) {
                let left = end.saturating_duration_since(Instant::now());
                assert!(!left.is_zero(), "timed out waiting: {what}");
                if let Ok(e) = self.rx.recv_timeout(left.min(Duration::from_millis(100))) {
                    self.app.event(e);
                }
            }
        }

        fn press(&mut self, k: Key) {
            self.app.key(Some(k), "", true);
            self.app.key(Some(k), "", false);
        }

        /// Type on a real keyboard, as the remote view or a USB keyboard does.
        fn type_text(&mut self, t: &str) {
            for c in t.chars() {
                self.app.key(None, &c.to_string(), true);
            }
        }

        fn shot(&mut self, name: &str) {
            let window = panel();
            self.app.draw();
            self.app.ui.show().expect("show");
            slint::platform::update_timers_and_animations();
            window.request_redraw();
            let frame: Vec<u8> = render_frame(&window).expect("frame").iter().map(|p| p.0).collect();
            assert!(frame.iter().filter(|p| **p < 128).count() > 200, "{name} draws something");
            let dir = std::path::Path::new("target/render/live");
            std::fs::create_dir_all(dir).unwrap();
            let f = std::fs::File::create(dir.join(format!("messages-{name}.png"))).unwrap();
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

    fn last_body(a: &App) -> String {
        a.model.messages.last().map(|m| m.body.clone()).unwrap_or_default()
    }

    #[test]
    #[ignore = "needs phoned and fake-mm: scripts/ui-e2e.sh"]
    fn live_messages() {
        let Some(mut l) = Live::start() else { return };
        use phone_core::model::MessageState;

        // A text arrives and starts a thread, under the contact's name.
        phonectl(&["fake", "sms", "+61491570156", "Lunch at 12:30 tomorrow?"]);
        l.until("the thread appears", |a| a.model.threads.iter().any(|t| t.unread > 0));
        l.shot("01-threads");

        // Open it: the conversation is fetched (the signal carries no text).
        l.press(Key::Run);
        l.until("the conversation loads", |a| last_body(a) == "Lunch at 12:30 tomorrow?");
        l.shot("02-conversation");

        // Answer with a quick reply, through the preview, and it is sent.
        l.press(Key::View);
        l.press(Key::Run);
        for _ in 0..3 {
            l.press(Key::Down);
        }
        l.press(Key::Run);
        l.shot("03-preview");
        l.press(Key::Run);
        l.until("sent", |a| {
            a.model.messages.last().is_some_and(|m| m.body == "On my way." && m.state == MessageState::Sent as u8)
        });
        l.shot("04-sent");

        // The network refuses one: it shows as failed, and Options can resend it.
        phonectl(&["fake", "sendfails", "on"]);
        l.press(Key::Edit);
        l.type_text("Running late");
        l.press(Key::Run);
        l.press(Key::Run);
        l.until("it fails", |a| a.model.messages.last().is_some_and(|m| m.state == MessageState::Failed as u8));
        l.shot("05-failed");
        l.press(Key::Ok);
        phonectl(&["fake", "sendfails", "off"]);
        l.press(Key::View);
        assert_eq!(l.app.model.options()[0], model::Opt::Resend);
        l.shot("06-options-resend");
        l.press(Key::Run);
        l.until("resent", |a| {
            a.model.messages.last().is_some_and(|m| m.body == "Running late" && m.state == MessageState::Sent as u8)
        });

        // A new conversation, written on the keyboard.
        l.press(Key::Escape);
        l.until("back at the list", |a| a.model.screen == model::Screen::Threads);
        l.press(Key::Edit);
        l.type_text("0491570199");
        l.shot("07-new-to");
        l.press(Key::Run);
        l.type_text("Hi! New number, this is Sam.");
        l.press(Key::Run);
        l.shot("08-new-preview");
        l.press(Key::Run);
        l.until("the new thread is there", |a| a.model.threads.iter().any(|t| t.peer == "+61491570199"));
    }
}
