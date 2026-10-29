//! Emergency alerts waiting to be read, for the apps that show them over
//! everything (the Dialer and the desktop).
//!
//! A newer alert used to replace one not yet dismissed, so a warning could be gone
//! before anyone read it. They queue instead: the most severe first, alerts of the
//! same level in the order they came, and dismissing one shows the next.

use phone_api::AlertInfo;

#[derive(Clone, Debug, Default)]
pub struct AlertQueue {
    /// Most severe first. The first is the one on screen.
    waiting: Vec<AlertInfo>,
    /// How many were dismissed since the queue was last empty, so the count reads
    /// "2 of 3" after the first is read rather than starting again at 1.
    dismissed: usize,
}

impl AlertQueue {
    /// Add an alert. An alert already waiting (the same id, heard twice) is not
    /// added again; returns whether this one was new.
    ///
    /// The level is phone_core's AlertLevel as a number, which counts up from the
    /// most severe (National 0, Extreme, Severe, Amber, Test, Info 5), so a lower
    /// number goes first.
    pub fn push(&mut self, a: AlertInfo) -> bool {
        if self.waiting.iter().any(|w| w.id == a.id) {
            return false;
        }
        let at = self.waiting.iter().position(|w| w.level > a.level).unwrap_or(self.waiting.len());
        self.waiting.insert(at, a);
        true
    }

    /// The alert to show now.
    pub fn current(&self) -> Option<&AlertInfo> {
        self.waiting.first()
    }

    /// The one on screen was read: the next takes its place.
    pub fn dismiss(&mut self) {
        if self.waiting.is_empty() {
            return;
        }
        self.waiting.remove(0);
        self.dismissed = if self.waiting.is_empty() { 0 } else { self.dismissed + 1 };
    }

    pub fn is_empty(&self) -> bool {
        self.waiting.is_empty()
    }

    /// Which this is and of how many, counting those read since the queue was last
    /// empty: (1, 1) for one alone.
    pub fn position(&self) -> (usize, usize) {
        if self.waiting.is_empty() {
            return (0, 0);
        }
        (self.dismissed + 1, self.dismissed + self.waiting.len())
    }

    /// "1 of 3", or nothing for an alert on its own.
    pub fn count(&self) -> String {
        match self.position() {
            (_, 0 | 1) => String::new(),
            (n, of) => format!("{n} of {of}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alert(id: i64, level: u8) -> AlertInfo {
        AlertInfo { id, channel: 4370, level, text: format!("alert {id}"), at: id }
    }

    #[test]
    fn the_most_severe_first_then_in_order() {
        let mut q = AlertQueue::default();
        assert!(q.push(alert(1, 2)));
        assert_eq!(q.count(), "", "one alone has no count");
        q.push(alert(2, 4));
        q.push(alert(3, 0));
        q.push(alert(4, 2));
        assert!(!q.push(alert(3, 0)), "the same alert twice is one");
        let order: Vec<i64> = std::iter::from_fn(|| {
            let id = q.current()?.id;
            let shown = q.count();
            q.dismiss();
            Some((id, shown))
        })
        .map(|(id, shown)| {
            assert!(shown.ends_with("of 4"), "{shown}");
            id
        })
        .collect();
        assert_eq!(order, [3, 1, 4, 2], "National, the two Severe as they came, then Test");
        assert!(q.is_empty());
        assert_eq!(q.position(), (0, 0));
        q.push(alert(5, 1));
        assert_eq!(q.position(), (1, 1), "the count starts again once all were read");
    }

    #[test]
    fn a_worse_alert_goes_in_front_of_the_one_showing() {
        let mut q = AlertQueue::default();
        q.push(alert(1, 3));
        q.push(alert(2, 3));
        q.dismiss();
        assert_eq!(q.count(), "2 of 2", "the last of two still says it is the second");
        q.push(alert(3, 0));
        assert_eq!(q.current().map(|a| a.id), Some(3), "National takes the screen at once");
        assert_eq!(q.count(), "2 of 3");
        q.dismiss();
        assert_eq!((q.current().map(|a| a.id), q.count().as_str()), (Some(2), "3 of 3"));
    }
}
