//! What phoned, phonectl and the apps agree on, with no D-Bus and no UI in it.
//!
//! - [`number`]: parsing, formatting and classifying phone numbers, including the
//!   emergency and premium rules the daemon enforces.
//! - [`text`]: making untrusted message text safe to draw, and counting SMS segments.
//! - [`contacts`]: a small vCard reader and writer, and lookup by number.
//! - [`store`]: messages, calls, blocked numbers and settings, in SQLite (SQLCipher,
//!   encrypted when phoned is given a key); only with the `db` feature.
//! - [`model`]: call and message state as both ends of the bus see it.
//! - [`guard`]: the rate limit on outgoing texts and calls.

pub mod contacts;
pub mod guard;
pub mod model;
pub mod number;
pub mod paths;
pub mod ss;
#[cfg(feature = "db")]
pub mod store;
/// Without the database, only where things are kept.
#[cfg(not(feature = "db"))]
pub mod store {
    pub use crate::paths::*;
}
pub mod text;
pub mod tones;
pub mod when;

/// Seconds since the Unix epoch, which is what every timestamp here is.
pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}
