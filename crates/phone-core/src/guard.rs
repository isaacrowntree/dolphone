//! The send and dial rate limit.
//!
//! A sliding window, per kind: an app or script that loops on SendMessage costs
//! the owner money and may get the SIM barred, and the limit is what turns that into
//! an error instead. Emergency calls are never counted against it.

use std::collections::VecDeque;

pub struct RateLimit {
    window: i64,
    max: usize,
    hits: VecDeque<i64>,
}

impl RateLimit {
    pub fn new(max: usize, window_secs: i64) -> Self {
        Self { window: window_secs, max, hits: VecDeque::new() }
    }

    /// Texts: 30 an hour is far above what a person types and far below what a
    /// runaway loop sends.
    pub fn sms() -> Self {
        Self::new(30, 3600)
    }

    /// Calls placed: 20 an hour.
    pub fn calls() -> Self {
        Self::new(20, 3600)
    }

    /// Record an attempt at `now`, or refuse it. Returns seconds until the oldest
    /// hit leaves the window when refused.
    pub fn take(&mut self, now: i64) -> Result<(), i64> {
        while self.hits.front().is_some_and(|&t| now - t >= self.window) {
            self.hits.pop_front();
        }
        if self.hits.len() >= self.max {
            let oldest = *self.hits.front().unwrap_or(&now);
            return Err(self.window - (now - oldest));
        }
        self.hits.push_back(now);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_past_the_limit_then_recovers() {
        let mut r = RateLimit::new(2, 60);
        assert!(r.take(0).is_ok());
        assert!(r.take(10).is_ok());
        assert_eq!(r.take(20), Err(40));
        assert!(r.take(60).is_ok());
    }
}
