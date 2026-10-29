//! Rules for incoming texts: when a matching text arrives, run a command.
//!
//! For the network-multitool uses of a phone: a text that says STATUS gets the
//! uplink's state back, a text with a code reboots a router, a one-time code is
//! handed to a script. Rules live in `~/.config/dolphone/rules.json`, which only the
//! owner can write:
//!
//! ```json
//! [
//!   { "from": "+61491570156", "contains": "STATUS", "secret": "4821",
//!     "run": ["/usr/local/bin/wan-status"], "reply": true },
//!   { "from": "*", "contains": "Your code is", "run": ["/home/me/bin/otp-to-clipboard"] }
//! ]
//! ```
//!
//! A rule matches on the sender (`*` for anyone), on text it must contain (case
//! insensitive), and on a secret the text must also contain. The command runs
//! directly, never through a shell, with the text as JSON on stdin:
//! `{"from": "...", "body": "...", "at": 1793228520}`. It has 30 seconds. With
//! `"reply": true`, what it prints (up to 600 characters) is texted back to the
//! sender, and only to a sender named in the rule: a reply never goes to `*`.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use phone_core::number;
use serde::Deserialize;

/// Unknown fields are an error, not ignored: a misspelled "contains" would
/// otherwise vanish and leave a rule that fires on every text from that sender.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub from: String,
    #[serde(default)]
    pub contains: Option<String>,
    #[serde(default)]
    pub secret: Option<String>,
    pub run: Vec<String>,
    #[serde(default)]
    pub reply: bool,
}

impl Rule {
    pub fn matches(&self, from: &str, body: &str) -> bool {
        let lower = body.to_lowercase();
        (self.from == "*" || number::same_party(&self.from, from))
            && self.contains.as_ref().is_none_or(|c| lower.contains(&c.to_lowercase()))
            && self.secret.as_ref().is_none_or(|s| body.contains(s.as_str()))
            && !self.run.is_empty()
    }

    /// Whether a reply may go back: only to a sender the rule names.
    pub fn may_reply(&self) -> bool {
        self.reply && self.from != "*"
    }
}

pub fn path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("/nonexistent"))
        .join("dolphone/rules.json")
}

/// The rules, read fresh each time (so an edit needs no restart). A broken file is
/// logged and treated as empty, never as "match everything".
pub fn load() -> Vec<Rule> {
    let Ok(text) = std::fs::read_to_string(path()) else { return Vec::new() };
    match serde_json::from_str(&text) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("rules.json: {e}");
            Vec::new()
        }
    }
}

/// Run a rule's command with the text on stdin. Returns what it printed, or None if
/// it failed or overran.
pub fn run(rule: &Rule, from: &str, body: &str, at: i64) -> Option<String> {
    let input = serde_json::json!({ "from": from, "body": body, "at": at }).to_string();
    let mut child = Command::new(&rule.run[0])
        .args(&rule.run[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| tracing::warn!("rule {:?}: {e}", rule.run[0]))
        .ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
    }
    let mut stdout = child.stdout.take()?;
    let mut stderr = child.stderr.take()?;
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let errors = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let out = reader.join().ok()?;
                let err = errors.join().unwrap_or_default();
                tracing::info!(command = %rule.run[0], ok = status.success(), "rule ran");
                if !status.success() && !err.trim().is_empty() {
                    // What the owner's own command said about failing, for them.
                    let said: String = err.trim().chars().take(300).collect();
                    tracing::warn!(command = %rule.run[0], said, "rule failed");
                }
                return status.success().then(|| out.trim().chars().take(600).collect());
            }
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                tracing::warn!(command = %rule.run[0], "rule took longer than 30 s; stopped");
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(from: &str, contains: Option<&str>, secret: Option<&str>, reply: bool) -> Rule {
        Rule {
            from: from.into(),
            contains: contains.map(Into::into),
            secret: secret.map(Into::into),
            run: vec!["/bin/cat".into()],
            reply,
        }
    }

    #[test]
    fn matching() {
        let r = rule("+61491570156", Some("status"), Some("4821"), true);
        assert!(r.matches("0491570156", "STATUS 4821"));
        assert!(!r.matches("0491570156", "STATUS"), "needs the secret");
        assert!(!r.matches("0491570157", "STATUS 4821"), "only that sender");
        assert!(rule("*", None, None, false).matches("anyone", "anything"));
    }

    #[test]
    fn replies_only_to_named_senders() {
        assert!(rule("+61491570156", None, None, true).may_reply());
        assert!(!rule("*", None, None, true).may_reply());
    }

    #[test]
    fn runs_without_a_shell_and_reads_stdout() {
        let r = rule("*", None, None, true);
        let out = run(&r, "+61491570156", "hello; rm -rf /", 1).unwrap();
        assert!(out.contains("hello; rm -rf /"), "the text arrives as data, not as a command");
        assert!(out.contains("\"from\":\"+61491570156\""));
    }
}
