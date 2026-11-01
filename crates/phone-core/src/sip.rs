//! A SIP account as a provider hands it out: a username, a password and a server.
//!
//! phoned keeps one SIP address (`sip:user@server`), but providers' welcome emails,
//! and Android's own SIP settings before Android 12, give the parts separately, so
//! the apps ask for them that way and join them here.

/// The username and the server of an address (`sip:me@voip.example.net:5061;transport=tls`
/// gives `me` and `voip.example.net:5061;transport=tls`).
pub fn split(account: &str) -> (String, String) {
    let rest = account.trim();
    let rest = rest.strip_prefix("sip:").or_else(|| rest.strip_prefix("sips:")).unwrap_or(rest);
    match rest.split_once('@') {
        Some((user, server)) => (user.to_string(), server.to_string()),
        None => (rest.to_string(), String::new()),
    }
}

/// The address for a username and a server, once both are there. Either may
/// carry a `sip:`, and a whole `user@server` typed as the username is taken
/// whole, so pasting what the provider sent works too.
pub fn join(user: &str, server: &str) -> Option<String> {
    let strip = |s: &str| {
        let s = s.trim();
        s.strip_prefix("sip:").or_else(|| s.strip_prefix("sips:")).unwrap_or(s).to_string()
    };
    let (mut user, mut server) = (strip(user), strip(server));
    if let Some((u, s)) = user.clone().split_once('@') {
        user = u.to_string();
        if !s.is_empty() {
            server = s.to_string();
        }
    }
    (!user.is_empty() && !server.is_empty()).then(|| format!("sip:{user}@{server}"))
}

/// Why registering failed, in words for the owner, from the SIP reply or the
/// error baresip reports.
pub fn explain(reason: &str) -> String {
    let r = reason.trim();
    let code: u16 = r.split_whitespace().next().and_then(|c| c.parse().ok()).unwrap_or(0);
    let lower = r.to_ascii_lowercase();
    match code {
        401 | 403 | 407 => "wrong username or password".into(),
        404 => "the server does not know this username".into(),
        408 | 503 | 504 => "no answer from the server".into(),
        _ if lower.contains("timed out") || lower.contains("unreachable") || lower.contains("refused") => {
            "no answer from the server".into()
        }
        _ if lower.contains("not known") || lower.contains("no such") || lower.contains("dns") => {
            "the server's name was not found".into()
        }
        _ => r.chars().take(60).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_and_back() {
        assert_eq!(split("sip:me@voip.example.net"), ("me".into(), "voip.example.net".into()));
        assert_eq!(split("sip:0291234567@sip.example.net:5061;transport=tls").1, "sip.example.net:5061;transport=tls");
        assert_eq!(split(""), (String::new(), String::new()));
        assert_eq!(join("me", "voip.example.net").as_deref(), Some("sip:me@voip.example.net"));
        assert_eq!(join(" sip:me ", "sip:voip.example.net ").as_deref(), Some("sip:me@voip.example.net"));
        assert_eq!(join("me@voip.example.net", "").as_deref(), Some("sip:me@voip.example.net"), "pasted whole");
        assert_eq!(join("me@new.example.net", "old.example.net").as_deref(), Some("sip:me@new.example.net"));
        assert_eq!(join("me", ""), None);
        assert_eq!(join("", "voip.example.net"), None);
    }

    #[test]
    fn failures_in_words() {
        assert_eq!(explain("401 Unauthorized"), "wrong username or password");
        assert_eq!(explain("403 Forbidden"), "wrong username or password");
        assert_eq!(explain("408 Request Timeout"), "no answer from the server");
        assert_eq!(explain("Connection timed out [110]"), "no answer from the server");
        assert_eq!(explain("Name or service not known"), "the server's name was not found");
        assert_eq!(explain("488 Not Acceptable Here"), "488 Not Acceptable Here");
    }
}
