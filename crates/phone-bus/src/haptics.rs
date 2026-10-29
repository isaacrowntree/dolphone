//! The vibration vocabulary every front end with a motor speaks.
//!
//! Each thing the phone tells you has its own feel, so it can be told apart in a
//! pocket, and the same thing has the same feel whichever app noticed it. A
//! pattern is on and off times in milliseconds, on first (docs/BUTTONS.md lists
//! them in words).

use std::time::Duration;

/// A key press.
pub const TICK: &[u16] = &[10];
/// A call connecting or ending.
pub const THUMP: &[u16] = &[80];
/// A text arrived: two short.
pub const TEXT: &[u16] = &[60, 120, 60];
/// A call was missed: three short.
pub const MISSED: &[u16] = &[60, 100, 60, 100, 60];
/// A second call is waiting: two long.
pub const WAITING: &[u16] = &[300, 200, 300];
/// A call dropped that nobody here ended: one long.
pub const DROPPED: &[u16] = &[600];
/// A text failed to send: short, short, long.
pub const FAILED: &[u16] = &[60, 100, 60, 100, 400];
/// An emergency alert: the alert tone's cadence, 2 s, 1 s, 1 s, twice, with 0.5 s
/// between (47 CFR 10.530; phone_core::tones::ALERT_ATTENTION).
pub const ALERT: &[u16] = &[2000, 500, 1000, 500, 1000, 500, 2000, 500, 1000, 500, 1000];
/// A press that was ignored on purpose (a screen that just appeared, End right
/// after answering, Decline that needs a hold): a quick stutter, unlike a tick,
/// so a press that did nothing does not feel like one that did.
pub const REFUSED: &[u16] = &[20, 40, 20, 40, 20];

/// A pattern being played. The caller owns the motor and the timer: it plays the
/// pulse [`Player::pulse`] hands it and comes back after the wait, and stops
/// asking once the pattern is done, so nothing ticks while the motor is still.
#[derive(Clone, Debug, Default)]
pub struct Player {
    steps: Vec<u16>,
}

impl Player {
    /// Start `pattern`, dropping whatever was playing.
    pub fn start(&mut self, pattern: &[u16]) {
        self.steps = pattern.to_vec();
    }

    /// The next pulse's length, and how long to wait before asking again (None
    /// when this was the last pulse). None when nothing is left.
    pub fn pulse(&mut self) -> Option<(u16, Option<Duration>)> {
        if self.steps.is_empty() {
            return None;
        }
        let on = self.steps.remove(0);
        if self.steps.is_empty() {
            return Some((on, None));
        }
        let off = self.steps.remove(0);
        // A trailing pause with no pulse after it is nothing to wait for.
        let wait = (!self.steps.is_empty()).then(|| Duration::from_millis(u64::from(on) + u64::from(off)));
        Some((on, wait))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pattern_plays_in_pulses_then_stops() {
        let mut p = Player::default();
        p.start(TEXT);
        assert_eq!(p.pulse(), Some((60, Some(Duration::from_millis(180)))));
        assert_eq!(p.pulse(), Some((60, None)));
        assert_eq!(p.pulse(), None);
        p.start(THUMP);
        assert_eq!(p.pulse(), Some((80, None)));
        assert_eq!(p.pulse(), None);
    }

    #[test]
    fn every_pattern_is_its_own() {
        let all = [TICK, THUMP, TEXT, MISSED, WAITING, DROPPED, FAILED, ALERT, REFUSED];
        for (i, a) in all.iter().enumerate() {
            assert!(all[i + 1..].iter().all(|b| a != b));
        }
    }
}
