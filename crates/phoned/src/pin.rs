//! The Messages PIN: kept as a salted PBKDF2-HMAC-SHA256 hash in the settings
//! (never the PIN itself), and an unlock that lasts a few minutes.
//!
//! What it is for: someone holding the unlocked device, or sitting at its desktop,
//! cannot read the texts without it, and new texts' notifications stop showing
//! what they say. What it is not: protection from programs running as the owner,
//! which can read the store (or, encrypted, unseal its key) as phoned does, and
//! from programs the owner granted reading, which read regardless. SECURITY.md
//! says so.

use std::num::NonZeroU32;
use std::sync::Mutex;

use ring::{digest, pbkdf2, rand::SecureRandom};

/// OWASP's suggestion for PBKDF2-HMAC-SHA256 (2023): 600 000 iterations.
const ITERATIONS: u32 = 600_000;

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    (0..s.len()).step_by(2).map(|i| s.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok())).collect()
}

/// A PIN: 4 to 12 digits.
pub fn valid(pin: &str) -> bool {
    (4..=12).contains(&pin.len()) && pin.chars().all(|c| c.is_ascii_digit())
}

/// The stored form: `pbkdf2-sha256$<iterations>$<salt>$<hash>`, hex.
pub fn hash(pin: &str) -> String {
    let mut salt = [0u8; 16];
    let _ = ring::rand::SystemRandom::new().fill(&mut salt);
    let mut out = [0u8; digest::SHA256_OUTPUT_LEN];
    let n = NonZeroU32::new(ITERATIONS).expect("nonzero");
    pbkdf2::derive(pbkdf2::PBKDF2_HMAC_SHA256, n, &salt, pin.as_bytes(), &mut out);
    format!("pbkdf2-sha256${ITERATIONS}${}${}", hex(&salt), hex(&out))
}

/// Whether `pin` is the one `stored` was made from (in constant time).
pub fn verify(pin: &str, stored: &str) -> bool {
    let mut f = stored.split('$');
    let (Some("pbkdf2-sha256"), Some(n), Some(salt), Some(want)) = (f.next(), f.next(), f.next(), f.next()) else {
        return false;
    };
    let (Some(n), Some(salt), Some(want)) = (n.parse().ok().and_then(NonZeroU32::new), unhex(salt), unhex(want)) else {
        return false;
    };
    pbkdf2::verify(pbkdf2::PBKDF2_HMAC_SHA256, n, &salt, pin.as_bytes(), &want).is_ok()
}

/// Whether Messages is open now, and how wrong guesses are slowed.
#[derive(Default)]
pub struct Lock {
    /// Open until this (phoned's monotonic seconds); 0 when locked.
    open_until: Mutex<i64>,
    /// Wrong guesses in a row, and no more guesses before this.
    wrong: Mutex<(u32, i64)>,
}

impl Lock {
    pub fn is_open(&self, now: i64) -> bool {
        self.open_until.lock().map(|u| now < *u).unwrap_or(false)
    }

    pub fn open(&self, until: i64) {
        if let Ok(mut u) = self.open_until.lock() {
            *u = until;
        }
        if let Ok(mut w) = self.wrong.lock() {
            *w = (0, 0);
        }
    }

    pub fn close(&self) {
        if let Ok(mut u) = self.open_until.lock() {
            *u = 0;
        }
    }

    /// Seconds to wait before another guess, if any.
    pub fn wait(&self, now: i64) -> Option<i64> {
        self.wrong.lock().ok().and_then(|w| (now < w.1).then(|| w.1 - now))
    }

    /// A wrong guess: after five in a row, a pause of 30 s that doubles each time.
    pub fn wrong_guess(&self, now: i64) {
        if let Ok(mut w) = self.wrong.lock() {
            w.0 += 1;
            if w.0 >= 5 {
                w.1 = now + (30i64 << (w.0 - 5).min(8));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_hash_and_verify() {
        let h = hash("4821");
        assert!(h.starts_with("pbkdf2-sha256$600000$"));
        assert!(verify("4821", &h));
        assert!(!verify("4822", &h));
        assert!(!verify("4821", "nonsense"));
        assert_ne!(hash("4821"), h, "salted");
        assert!(valid("4821") && !valid("12") && !valid("12ab") && !valid("1234567890123"));
    }

    #[test]
    fn wrong_guesses_slow_down() {
        let l = Lock::default();
        for _ in 0..4 {
            l.wrong_guess(100);
        }
        assert_eq!(l.wait(100), None, "four are free");
        l.wrong_guess(100);
        assert_eq!(l.wait(100), Some(30));
        l.wrong_guess(100);
        assert_eq!(l.wait(100), Some(60));
        l.open(400);
        assert!(l.is_open(300) && !l.is_open(400));
        assert_eq!(l.wait(100), None, "the right one resets the count");
    }
}
