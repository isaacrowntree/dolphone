//! dolphone-desktop: the phone in desktop mode.
//!
//! When the Flipper One drives an external display, this is the phone on it: dial
//! pad and call log, conversations with a real keyboard, contacts, settings, and the
//! call card over everything while a call rings or is live. It is a front end to
//! phoned like the panel apps, so a call answered here is answered on the panel too.
//!
//! It is also what notifications and links open:
//!
//!     dolphone-desktop                      the window
//!     dolphone-desktop tel:+61491570156     the dial pad with that number (not dialled)
//!     dolphone-desktop sms:0491570156?body=hi
//!     dolphone-desktop --thread <number> | --compose <number> | --call | --recents
//!
//! A second start hands its arguments to the window already open and exits.

mod state;
mod tray;

use std::cell::RefCell;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::Duration;

use phone_bus::{Bus, Event};

slint::include_modules!();

thread_local! {
    static APP: RefCell<Option<state::App>> = const { RefCell::new(None) };
}

pub fn with_app(f: impl FnOnce(&mut state::App)) {
    APP.with(|a| {
        if let Some(app) = a.borrow_mut().as_mut() {
            f(app);
        }
    });
}

/// Where a second start finds the window already open: in Dolphone's private
/// runtime directory, never a shared /tmp where another user could answer.
fn socket_path() -> PathBuf {
    phone_bus::files::runtime_dir().join("desktop.sock")
}

fn main() -> Result<(), slint::PlatformError> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!(
            "dolphone-desktop [tel:NUMBER | sms:NUMBER[?body=TEXT] | --thread N | --compose N | --call | --recents]"
        );
        return Ok(());
    }
    // Already open: pass the arguments over and let that window act on them.
    let sock = socket_path();
    if let Ok(mut s) = UnixStream::connect(&sock) {
        let _ = s.write_all(args.join("\n").as_bytes());
        return Ok(());
    }
    let _ = std::fs::remove_file(&sock);
    if let Ok(listener) = UnixListener::bind(&sock) {
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut text = String::new();
                let _ = stream.read_to_string(&mut text);
                let args: Vec<String> = text.lines().map(str::to_string).filter(|l| !l.is_empty()).collect();
                let _ = slint::invoke_from_event_loop(move || {
                    with_app(|app| {
                        app.handle_args(&args);
                        app.ui.window().show().ok();
                    })
                });
            }
        });
    }

    let ui = AppWindow::new()?;
    let bus = Bus::start(|e: Event| {
        let _ = slint::invoke_from_event_loop(move || with_app(|app| app.event(e)));
    });
    APP.with(|a| *a.borrow_mut() = Some(state::App::new(ui.clone_strong(), bus)));
    with_app(|app| {
        app.reload();
        app.handle_args(&args);
        app.render();
    });
    state::wire(&ui);
    // The tray icon brings the window back; a middle click goes to Messages.
    let tray = tray::Tray::start(|messages| {
        let _ = slint::invoke_from_event_loop(move || {
            with_app(|app| {
                if messages {
                    app.ui.set_page(1);
                }
                app.ui.window().show().ok();
                app.render();
            })
        });
    });
    with_app(|app| app.tray = Some(tray));

    let tick = slint::Timer::default();
    tick.start(slint::TimerMode::Repeated, Duration::from_secs(1), || with_app(|app| app.tick()));
    let result = ui.run();
    let _ = std::fs::remove_file(socket_path());
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType, Rgb565Pixel};
    use slint::platform::{Platform, WindowAdapter};
    use std::rc::Rc;

    struct TestPlatform(Rc<MinimalSoftwareWindow>);

    impl Platform for TestPlatform {
        fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
            Ok(self.0.clone())
        }
    }

    const W: u32 = 980;
    const H: u32 = 660;

    fn window() -> Rc<MinimalSoftwareWindow> {
        thread_local! {
            static WINDOW: Rc<MinimalSoftwareWindow> = {
                let w = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
                slint::platform::set_platform(Box::new(TestPlatform(w.clone()))).expect("platform");
                w.set_size(slint::PhysicalSize::new(W, H));
                w
            };
        }
        WINDOW.with(Rc::clone)
    }

    fn shot(name: &str, setup: impl FnOnce(&mut state::App)) {
        let w = window();
        let ui = AppWindow::new().expect("window");
        let mut app = state::App::headless(ui.clone_strong());
        setup(&mut app);
        app.render();
        ui.show().expect("show");
        w.request_redraw();
        let mut buf = vec![Rgb565Pixel::default(); (W * H) as usize];
        w.draw_if_needed(|r| {
            r.render(&mut buf, W as usize);
        });
        let rgb: Vec<u8> = buf
            .iter()
            .flat_map(|p| {
                let v = p.0;
                let (r, g, b) = ((v >> 11) & 0x1f, (v >> 5) & 0x3f, v & 0x1f);
                [(r << 3 | r >> 2) as u8, (g << 2 | g >> 4) as u8, (b << 3 | b >> 2) as u8]
            })
            .collect();
        drop(ui);
        let distinct = rgb.chunks(3).step_by(97).collect::<std::collections::HashSet<_>>().len();
        assert!(distinct > 2, "{name} draws something");
        if std::env::var_os("DOLPHONE_RENDER").is_some() {
            let dir = std::path::Path::new("target/render");
            std::fs::create_dir_all(dir).unwrap();
            let f = std::fs::File::create(dir.join(format!("desktop-{name}.png"))).unwrap();
            let mut e = png::Encoder::new(std::io::BufWriter::new(f), W, H);
            e.set_color(png::ColorType::Rgb);
            e.set_depth(png::BitDepth::Eight);
            e.write_header().unwrap().write_image_data(&rgb).unwrap();
        }
    }

    #[test]
    fn screens() {
        shot("calls", |a| state::sample(a, 0));
        shot("messages", |a| state::sample(a, 1));
        shot("settings", |a| state::sample(a, 3));
        shot("incoming", |a| {
            state::sample(a, 0);
            state::sample_call(a, true);
        });
        shot("in-call", |a| {
            state::sample(a, 1);
            state::sample_call(a, false);
        });
        shot("confirm", |a| {
            state::sample(a, 1);
            state::sample_confirm(a);
        });
        shot("one-time-code", |a| {
            state::sample(a, 1);
            a.messages.push(phone_api::MessageInfo {
                id: 99,
                peer: a.peer.clone(),
                direction: 0,
                body: "Your Example Bank code is 482913. Never share it.".into(),
                at: phone_core::now() - 30,
                state: 0,
                read: true,
                sim: String::new(),
                parts: Vec::new(),
            });
        });
        shot("alert", |a| {
            state::sample(a, 0);
            a.alert = Some(phone_api::AlertInfo {
                id: 1,
                channel: 4371,
                level: phone_core::model::AlertLevel::Extreme as u8,
                text: "Emergency Alert: Bushfire. Leave now if you are in Example Valley. Follow police and fire service directions. Monitor ABC radio.".into(),
                at: phone_core::now() - 120,
            });
        });
        shot("settings-grants", |a| {
            state::sample(a, 3);
            a.voicemail = "101".into();
            a.ui.set_voicemail("101".into());
            a.grants = vec![phone_api::Grant {
                id: 1,
                program: "wan-watch".into(),
                numbers: "+61491570158".into(),
                per_hour: 4,
                until: 0,
            }];
            a.audit = vec![phone_api::AuditEntry {
                at: phone_core::now() - 60,
                program: "wan-watch".into(),
                action: "send".into(),
                target: "+61…158".into(),
                result: "sent under a grant".into(),
            }];
        });
    }

    #[test]
    fn one_time_codes_get_a_copy_button() {
        let _w = window();
        let ui = AppWindow::new().expect("window");
        let mut app = state::App::headless(ui.clone_strong());
        state::sample(&mut app, 1);
        app.messages = vec![phone_api::MessageInfo {
            id: 1,
            peer: app.peer.clone(),
            direction: 0,
            body: "G-735102 is your verification code".into(),
            at: 1,
            state: 0,
            read: true,
            sim: String::new(),
            parts: Vec::new(),
        }];
        app.render();
        use slint::Model;
        assert_eq!(ui.get_bubbles().row_data(0).unwrap().code.as_str(), "735102");
    }

    #[test]
    fn bubbles_say_who_and_keep_a_lost_picture() {
        let _w = window();
        let ui = AppWindow::new().expect("window");
        let mut app = state::App::headless(ui.clone_strong());
        state::sample(&mut app, 1);
        app.messages.truncate(2);
        app.messages[0].body = "Look".into();
        app.messages[0].parts = vec![("image/jpeg".into(), "/nonexistent/picture.jpg".into())];
        app.messages[1].state = phone_core::model::MessageState::Delivered as u8;
        app.render();
        use slint::Model;
        let b = ui.get_bubbles();
        let (theirs, mine) = (b.row_data(0).unwrap(), b.row_data(1).unwrap());
        assert_eq!(theirs.parts.as_str(), "[Picture]", "a picture that will not load is still named");
        assert!(
            theirs.label.starts_with("Alex Example, ") && theirs.label.ends_with(": Look [Picture]"),
            "{}",
            theirs.label
        );
        assert!(
            mine.label.starts_with("You, ") && mine.label.ends_with(", Delivered: Yes! 12:30 works."),
            "{}",
            mine.label
        );
    }

    /// Keys as the window gets them, for the keyboard tests.
    fn press(ui: &AppWindow, key: impl Into<slint::SharedString>) {
        let text = key.into();
        ui.window().dispatch_event(slint::platform::WindowEvent::KeyPressed { text: text.clone() });
        ui.window().dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
    }

    #[test]
    fn the_call_card_keeps_the_keyboard() {
        use slint::platform::Key;
        let _w = window();
        let ui = AppWindow::new().expect("window");
        let mut app = state::App::headless(ui.clone_strong());
        state::sample(&mut app, 0);
        state::sample_call(&mut app, true);
        app.render();
        ui.show().expect("show");
        let said = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let s = said.clone();
        ui.on_answer(move || s.borrow_mut().push("answer".into()));
        let s = said.clone();
        ui.on_decline(move || s.borrow_mut().push("decline".into()));
        let s = said.clone();
        ui.on_reply(move |t| s.borrow_mut().push(t.to_string()));
        slint::platform::update_timers_and_animations();

        // Answer has the focus as the card opens.
        press(&ui, Key::Return);
        // Tab runs along the replies, then round to Decline rather than out to the
        // page; Shift+Tab goes back round to the last reply.
        press(&ui, Key::Tab);
        press(&ui, " ");
        for _ in 0..3 {
            press(&ui, Key::Tab);
        }
        press(&ui, Key::Return);
        press(&ui, Key::Backtab);
        press(&ui, Key::Return);
        assert_eq!(*said.borrow(), ["answer", "Can't talk now", "decline", "Call you back"]);
    }

    #[test]
    fn lists_walk_with_the_arrows() {
        use slint::platform::Key;
        let _w = window();
        let ui = AppWindow::new().expect("window");
        let mut app = state::App::headless(ui.clone_strong());
        state::sample(&mut app, 2);
        app.render();
        ui.show().expect("show");
        let called = std::rc::Rc::new(std::cell::Cell::new(-1));
        let c = called.clone();
        ui.on_contact_call(move |i| c.set(i));
        slint::platform::update_timers_and_animations();

        // Tab past the search field to the first row; Space chooses it.
        let mut tabs = 0;
        while ui.get_contact_selected() < 0 {
            assert!(tabs < 5, "Tab reaches the list");
            press(&ui, Key::Tab);
            tabs += 1;
            press(&ui, " ");
        }
        assert_eq!(ui.get_contact_selected(), 0);
        // Down twice and Space chooses the third; Up and Enter calls the second.
        press(&ui, Key::DownArrow);
        press(&ui, Key::DownArrow);
        press(&ui, " ");
        assert_eq!(ui.get_contact_selected(), 2);
        press(&ui, Key::UpArrow);
        press(&ui, Key::Return);
        assert_eq!(called.get(), 1);
    }

    #[test]
    fn links_open_the_right_place() {
        let w = window();
        let _ = w;
        let ui = AppWindow::new().expect("window");
        let mut app = state::App::headless(ui.clone_strong());
        app.handle_args(&["tel:+61%20491%20570%20156".to_string()]);
        assert_eq!(app.ui.get_page(), 0);
        assert_eq!(app.ui.get_number().as_str(), "+61 491 570 156");
        app.handle_args(&["sms:0491570156?body=hello%20there".to_string()]);
        assert_eq!(app.ui.get_page(), 1);
        assert_eq!(app.ui.get_draft().as_str(), "hello there");
        assert_eq!(app.peer, "+61491570156");
    }

    // Live: the desktop app against a running phoned and fake-mm (scripts/ui-e2e.sh).
    // Its buttons are Slint callbacks, invoked here as a click would.

    fn phonectl(args: &[&str]) -> String {
        let out = std::process::Command::new("phonectl").args(args).output().expect("phonectl on PATH");
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
    }

    fn pump(rx: &std::sync::mpsc::Receiver<phone_bus::Event>, what: &str, done: impl Fn(&state::App) -> bool) {
        let end = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let mut ok = false;
            with_app(|a| ok = done(a));
            if ok {
                return;
            }
            let left = end.saturating_duration_since(std::time::Instant::now());
            assert!(!left.is_zero(), "timed out waiting: {what}");
            if let Ok(e) = rx.recv_timeout(left.min(Duration::from_millis(100))) {
                with_app(|a| a.event(e));
            }
        }
    }

    fn live_shot(ui: &AppWindow, name: &str) {
        let w = window();
        with_app(|a| a.render());
        ui.show().expect("show");
        w.request_redraw();
        let mut buf = vec![Rgb565Pixel::default(); (W * H) as usize];
        w.draw_if_needed(|r| {
            r.render(&mut buf, W as usize);
        });
        let rgb: Vec<u8> = buf
            .iter()
            .flat_map(|p| {
                let v = p.0;
                let (r, g, b) = ((v >> 11) & 0x1f, (v >> 5) & 0x3f, v & 0x1f);
                [(r << 3 | r >> 2) as u8, (g << 2 | g >> 4) as u8, (b << 3 | b >> 2) as u8]
            })
            .collect();
        let dir = std::path::Path::new("target/render/live");
        std::fs::create_dir_all(dir).unwrap();
        let f = std::fs::File::create(dir.join(format!("desktop-{name}.png"))).unwrap();
        let mut e = png::Encoder::new(std::io::BufWriter::new(f), W, H);
        e.set_color(png::ColorType::Rgb);
        e.set_depth(png::BitDepth::Eight);
        e.write_header().unwrap().write_image_data(&rgb).unwrap();
    }

    #[test]
    #[ignore = "needs phoned and fake-mm: scripts/ui-e2e.sh"]
    fn live_desktop() {
        if std::env::var_os("DOLPHONE_LIVE").is_none() {
            return;
        }
        let _w = window();
        let ui = AppWindow::new().expect("window");
        let (tx, rx) = std::sync::mpsc::channel();
        let bus = Bus::start(move |e| {
            let _ = tx.send(e);
        });
        APP.with(|a| *a.borrow_mut() = Some(state::App::new(ui.clone_strong(), bus)));
        state::wire(&ui);
        pump(&rx, "phoned connects", |a| a.connected && a.status.is_some());

        // A call rings: the call card comes up, Answer connects, End ends.
        phonectl(&["fake", "ring", "+61491570156"]);
        pump(&rx, "the card shows", |a| a.ui.get_call_visible() && a.ui.get_call_ringing());
        live_shot(&ui, "01-incoming");
        ui.invoke_answer();
        pump(&rx, "answered", |a| {
            a.ui.get_call_visible() && !a.ui.get_call_ringing() && !a.ui.get_call_timer().is_empty()
        });
        live_shot(&ui, "02-in-call");
        ui.invoke_hangup();
        pump(&rx, "ended", |a| !a.ui.get_call_visible());

        // A one-time code arrives: the conversation offers to copy it.
        phonectl(&["fake", "sms", "+61491570156", "Your Example Bank code is 482913. Never share it."]);
        ui.invoke_navigate(1);
        // This text's thread, on top: earlier live tests left threads of their own,
        // and the first one may still be one of those until this text arrives.
        pump(&rx, "the thread is listed", |a| {
            a.threads.first().is_some_and(|t| t.peer == "+61491570156" && t.last_body.contains("482913"))
        });
        ui.invoke_open_thread(0);
        pump(&rx, "the code is found", |a| {
            use slint::Model;
            let b = a.ui.get_bubbles();
            (0..b.row_count()).any(|i| b.row_data(i).is_some_and(|x| x.code == "482913"))
        });
        live_shot(&ui, "03-one-time-code");

        // Writing back.
        ui.set_draft("Thanks, got it.".into());
        ui.invoke_send("Thanks, got it.".into());
        pump(&rx, "sent", |a| a.messages.last().is_some_and(|m| m.body == "Thanks, got it." && m.state == 2));
        live_shot(&ui, "04-replied");

        // A program wants to call a premium number: the desktop asks too.
        let placed = phonectl(&["dial", "1900123456"]);
        assert!(placed.contains("waiting for confirmation"), "{placed}");
        pump(&rx, "the confirmation shows", |a| a.ui.get_confirm_visible());
        live_shot(&ui, "05-confirm");
        ui.invoke_deny();
        pump(&rx, "denied", |a| !a.ui.get_confirm_visible());

        // An emergency alert covers the window until dismissed.
        phonectl(&["fake", "broadcast", "4371", "Extreme weather warning: shelter indoors. This is a live UI test."]);
        pump(&rx, "the alert shows", |a| a.ui.get_alert_visible());
        live_shot(&ui, "06-alert");
        ui.invoke_dismiss_alert();
        pump(&rx, "dismissed", |a| !a.ui.get_alert_visible());

        // Settings, as phoned reports them.
        ui.invoke_navigate(3);
        pump(&rx, "settings", |a| a.ui.get_page() == 3);
        live_shot(&ui, "07-settings");
    }

    /// A stand-in tray host: records who registered.
    struct Watcher(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

    #[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
    impl Watcher {
        fn register_status_notifier_item(&self, service: &str) {
            self.0.lock().unwrap().push(service.to_string());
        }
    }

    #[test]
    #[ignore = "needs a session bus: scripts/ui-e2e.sh"]
    fn live_tray() {
        if std::env::var_os("DOLPHONE_LIVE").is_none() {
            return;
        }
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let _host = zbus::connection::Builder::session()
                .name("org.kde.StatusNotifierWatcher")
                .serve_at("/StatusNotifierWatcher", Watcher(seen.clone()))
                .build()
                .await
                .expect("the stand-in watcher");
            let tray = tray::Tray::start(|_| {});
            let mut name = None;
            for _ in 0..50 {
                tokio::time::sleep(Duration::from_millis(100)).await;
                if let Some(n) = seen.lock().unwrap().first().cloned() {
                    name = Some(n);
                    break;
                }
            }
            let name = name.expect("the tray icon registers with the watcher");
            let conn = zbus::Connection::session().await.unwrap();
            let item = zbus::Proxy::new(&conn, name.as_str(), "/StatusNotifierItem", "org.kde.StatusNotifierItem")
                .await
                .unwrap();
            let status: String = item.get_property("Status").await.unwrap();
            assert_eq!(status, "Active");
            tray.set(tray::Counts { missed: 1, unread: 2 });
            tokio::time::sleep(Duration::from_millis(300)).await;
            // A fresh proxy reads the properties as they are now.
            let item = zbus::Proxy::new(&conn, name.as_str(), "/StatusNotifierItem", "org.kde.StatusNotifierItem")
                .await
                .unwrap();
            let status: String = item.get_property("Status").await.unwrap();
            assert_eq!(status, "NeedsAttention", "missed calls ask for attention");
            let tip: tray::ToolTip = item.get_property("ToolTip").await.unwrap();
            assert_eq!(tip.3, "1 missed call, 2 new texts");
        });
    }
}
