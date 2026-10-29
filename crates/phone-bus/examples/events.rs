//! Print every event a front end would get: `cargo run -p phone-bus --example events`.
fn main() {
    let bus = phone_bus::Bus::start(|e| println!("{e:?}"));
    bus.refresh_calls();
    loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}
