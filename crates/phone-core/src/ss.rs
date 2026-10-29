//! Supplementary-service codes typed on the keypad (3GPP TS 22.030 §6.5), turned
//! into what the modem understands (3GPP TS 27.007: `+CCFC`, `+CLIR`), and the
//! modem's answers turned back into a sentence.
//!
//! A code is `<procedure><SC>[*SIA[*SIB[*SIC]]]#`: the procedure is `*` activate,
//! `#` deactivate, `*#` interrogate, `**` register, `##` erase; SC the service
//! (21 forward everything, 67 when busy, 61 when unanswered, 62 when unreachable,
//! 002 all, 004 all conditional, 43 call waiting, 31 caller ID); SIA the number to
//! forward to; SIB a basic service group; SIC, for 61, the seconds to ring first.

/// What a code asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ss {
    /// Call forwarding: `+CCFC` with this reason and mode.
    Forward { reason: u8, mode: u8, number: Option<String>, time: Option<u8> },
    /// Call waiting, through ModemManager's own Voice API: on, off, or None to ask.
    Waiting(Option<bool>),
    /// Caller ID on outgoing calls (CLIR): Some(true) to show it, Some(false) to
    /// hide it, None to ask.
    CallerId(Option<bool>),
    /// A service Dolphone does not handle (barring needs the network password;
    /// connected-line codes are rarely provisioned), named for the reply.
    Unsupported(&'static str),
}

/// `+CCFC` modes (27.007 §7.11).
pub mod mode {
    pub const DISABLE: u8 = 0;
    pub const ENABLE: u8 = 1;
    pub const QUERY: u8 = 2;
    pub const REGISTER: u8 = 3;
    pub const ERASE: u8 = 4;
}

/// Service codes for forwarding and their `+CCFC` reason (22.030 Annex B).
const FORWARDS: &[(&str, u8)] = &[("21", 0), ("67", 1), ("61", 2), ("62", 3), ("002", 4), ("004", 5)];

fn reason_name(reason: u8) -> &'static str {
    match reason {
        0 => "all calls",
        1 => "when busy",
        2 => "when unanswered",
        3 => "when unreachable",
        4 => "every kind",
        _ => "all conditions",
    }
}

/// Parse a keypad code, or None when it is not a supplementary-service code.
pub fn parse(code: &str) -> Option<Ss> {
    let c = code.trim();
    let (procedure, rest) = ["**", "##", "*#", "*", "#"].iter().find_map(|p| c.strip_prefix(p).map(|r| (*p, r)))?;
    let body = rest.strip_suffix('#')?;
    let mut parts = body.split('*');
    let sc = parts.next()?;
    let sia = parts.next().filter(|s| !s.is_empty());
    let _sib = parts.next();
    let sic = parts.next().filter(|s| !s.is_empty());
    if parts.next().is_some() || sc.is_empty() || !sc.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    if let Some((_, reason)) = FORWARDS.iter().find(|(s, _)| *s == sc) {
        let number = sia.map(str::to_string);
        if number.as_deref().is_some_and(|n| !n.chars().all(|ch| ch.is_ascii_digit() || ch == '+')) {
            return None;
        }
        let mode = match (procedure, &number) {
            ("**", _) => mode::REGISTER,
            // `*21*number#`: 22.030 §6.5.2 treats a number given to activation as
            // a registration.
            ("*", Some(_)) => mode::REGISTER,
            ("*", None) => mode::ENABLE,
            ("#", _) => mode::DISABLE,
            ("*#", _) => mode::QUERY,
            _ => mode::ERASE,
        };
        if mode == mode::REGISTER && number.is_none() {
            return None;
        }
        let time = sic.and_then(|t| t.parse().ok()).filter(|t| (5..=30).contains(t));
        return Some(Ss::Forward { reason: *reason, mode, number, time });
    }
    if sia.is_some() {
        // `#31#number` hides the caller ID for one call; that is a call, not this.
        return None;
    }
    Some(match (sc, procedure) {
        ("43", "*") => Ss::Waiting(Some(true)),
        ("43", "#") => Ss::Waiting(Some(false)),
        ("43", "*#") => Ss::Waiting(None),
        // SC 31 is CLIR, caller-ID *restriction*: activating it (`*31#`) hides the
        // number, deactivating it (`#31#`) shows it, as Android's GsmMmiCode does.
        ("31", "*") => Ss::CallerId(Some(false)),
        ("31", "#") => Ss::CallerId(Some(true)),
        ("31", "*#") => Ss::CallerId(None),
        ("33" | "330" | "331" | "332" | "333" | "35" | "351", _) => Ss::Unsupported("call barring"),
        ("30" | "76" | "77", _) => Ss::Unsupported("caller and connected-line presentation"),
        _ => return None,
    })
}

/// The `+CCFC` command for a forwarding code. Class 1 is voice, so data and fax
/// forwarding are left as they are; `<type>` is 145 for a number with `+`.
pub fn forward_command(reason: u8, mode: u8, number: Option<&str>, time: Option<u8>) -> String {
    match number {
        Some(n) => {
            let ty = if n.starts_with('+') { 145 } else { 129 };
            match time {
                Some(t) => format!("AT+CCFC={reason},{mode},\"{n}\",{ty},1,,,{t}"),
                None => format!("AT+CCFC={reason},{mode},\"{n}\",{ty},1"),
            }
        }
        None if mode == mode::QUERY => format!("AT+CCFC={reason},{mode}"),
        None => format!("AT+CCFC={reason},{mode},,,1"),
    }
}

/// The `+CLIR` command: `n` 1 hides the number on outgoing calls, 2 shows it.
pub fn clir_command(show: Option<bool>) -> String {
    match show {
        None => "AT+CLIR?".into(),
        Some(true) => "AT+CLIR=2".into(),
        Some(false) => "AT+CLIR=1".into(),
    }
}

/// What a change did, in words for the keypad's reply screen.
pub fn describe_change(ss: &Ss) -> String {
    match ss {
        Ss::Forward { reason, mode, number, .. } => {
            let what = reason_name(*reason);
            match *mode {
                mode::REGISTER => format!("Forwarding {what} to {}", number.as_deref().unwrap_or("")),
                mode::ENABLE => format!("Forwarding {what}: on"),
                mode::DISABLE => format!("Forwarding {what}: off"),
                mode::ERASE => format!("Forwarding {what}: cleared"),
                _ => String::new(),
            }
        }
        Ss::Waiting(Some(on)) => format!("Call waiting: {}", if *on { "on" } else { "off" }),
        Ss::CallerId(Some(show)) => format!("Caller ID: {}", if *show { "shown" } else { "hidden" }),
        _ => String::new(),
    }
}

/// A `+CCFC` interrogation's answer: one line per class, `+CCFC: <status>,<class>
/// [,<number>,<type>[,<subaddr>,<satype>[,<time>]]]`.
pub fn describe_forwarding(reason: u8, response: &str) -> String {
    let what = reason_name(reason);
    let mut out = Vec::new();
    for line in response.lines() {
        let Some(rest) = line.trim().strip_prefix("+CCFC:") else { continue };
        let f: Vec<&str> = rest.split(',').map(str::trim).collect();
        let active = f.first() == Some(&"1");
        let class: u32 = f.get(1).and_then(|c| c.parse().ok()).unwrap_or(0);
        if class != 0 && class & 1 == 0 {
            continue; // data, fax or texts, not calls
        }
        let number = f.get(2).map(|n| n.trim_matches('"')).filter(|n| !n.is_empty());
        let time = f.get(6).and_then(|t| t.parse::<u32>().ok());
        out.push(match (active, number) {
            (true, Some(n)) => match time {
                Some(t) if reason == 2 => format!("Forwarding {what}: on, to {n} after {t} s"),
                _ => format!("Forwarding {what}: on, to {n}"),
            },
            (true, None) => format!("Forwarding {what}: on"),
            (false, _) => format!("Forwarding {what}: off"),
        });
    }
    if out.is_empty() {
        format!("Forwarding {what}: off")
    } else {
        out.dedup();
        out.join("\n")
    }
}

/// A `+CLIR?` answer: `+CLIR: <n>,<m>`, where `m` is what the network has (0 not
/// provisioned, 1 always hidden, 2 unknown, 3 hidden unless shown, 4 shown unless
/// hidden) and `n` what this device asks for (0 the network's choice, 1 hide, 2 show).
pub fn describe_clir(response: &str) -> String {
    let Some(rest) = response.lines().find_map(|l| l.trim().strip_prefix("+CLIR:")) else {
        return "Caller ID: unknown".into();
    };
    let f: Vec<u8> = rest.split(',').filter_map(|x| x.trim().parse().ok()).collect();
    match (f.first(), f.get(1)) {
        (Some(1), _) => "Caller ID: hidden".into(),
        (Some(2), _) => "Caller ID: shown".into(),
        (_, Some(1 | 3)) => "Caller ID: hidden (the network's setting)".into(),
        (_, Some(0 | 4)) => "Caller ID: shown (the network's setting)".into(),
        _ => "Caller ID: the network's setting".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwarding_codes() {
        assert_eq!(
            parse("**21*0412345678#"),
            Some(Ss::Forward { reason: 0, mode: mode::REGISTER, number: Some("0412345678".into()), time: None })
        );
        assert_eq!(
            parse("*21*+61412345678#"),
            Some(Ss::Forward { reason: 0, mode: mode::REGISTER, number: Some("+61412345678".into()), time: None })
        );
        assert_eq!(parse("##002#"), Some(Ss::Forward { reason: 4, mode: mode::ERASE, number: None, time: None }));
        assert_eq!(parse("*#67#"), Some(Ss::Forward { reason: 1, mode: mode::QUERY, number: None, time: None }));
        assert_eq!(parse("#21#"), Some(Ss::Forward { reason: 0, mode: mode::DISABLE, number: None, time: None }));
        assert_eq!(
            parse("**61*0412345678**20#"),
            Some(Ss::Forward { reason: 2, mode: mode::REGISTER, number: Some("0412345678".into()), time: Some(20) })
        );
        assert_eq!(parse("**21#"), None, "registering needs a number");
        assert_eq!(parse("*100#"), None, "USSD");
        assert_eq!(parse("#31#0412345678"), None, "a call, not a code");
    }

    #[test]
    fn other_codes() {
        assert_eq!(parse("*43#"), Some(Ss::Waiting(Some(true))));
        assert_eq!(parse("*#43#"), Some(Ss::Waiting(None)));
        assert_eq!(parse("#31#"), Some(Ss::CallerId(Some(true))));
        assert_eq!(parse("*31#"), Some(Ss::CallerId(Some(false))));
        assert_eq!(parse("*#31#"), Some(Ss::CallerId(None)));
        assert_eq!(parse("*33*1234#"), None);
        assert_eq!(parse("*#33#"), Some(Ss::Unsupported("call barring")));
    }

    #[test]
    fn commands() {
        // The examples in Quectel's RM5x0N AT manual, §10.1.
        assert_eq!(forward_command(0, 3, Some("15021012496"), None), "AT+CCFC=0,3,\"15021012496\",129,1");
        assert_eq!(forward_command(0, 2, None, None), "AT+CCFC=0,2");
        assert_eq!(forward_command(0, 4, None, None), "AT+CCFC=0,4,,,1");
        assert_eq!(forward_command(2, 3, Some("+61412345678"), Some(20)), "AT+CCFC=2,3,\"+61412345678\",145,1,,,20");
        assert_eq!(clir_command(Some(false)), "AT+CLIR=1");
    }

    #[test]
    fn answers() {
        assert_eq!(
            describe_forwarding(0, "+CCFC: 1,1,\"+8615021012496\",145,,,"),
            "Forwarding all calls: on, to +8615021012496"
        );
        assert_eq!(describe_forwarding(0, "+CCFC: 0,7"), "Forwarding all calls: off");
        assert_eq!(
            describe_forwarding(2, "+CCFC: 1,1,\"0412\",129,,,20"),
            "Forwarding when unanswered: on, to 0412 after 20 s"
        );
        assert_eq!(describe_forwarding(1, ""), "Forwarding when busy: off");
        assert_eq!(describe_clir("+CLIR: 0,4"), "Caller ID: shown (the network's setting)");
        assert_eq!(describe_clir("+CLIR: 1,4"), "Caller ID: hidden");
    }
}
