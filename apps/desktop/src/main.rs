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

fn socket_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("dolphone/desktop.sock")
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
    if let Some(dir) = sock.parent() {
        let _ = std::fs::create_dir_all(dir);
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
}
