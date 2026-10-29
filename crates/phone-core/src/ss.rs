//! Supplementary-service codes typed on the keypad (3GPP TS 22.030 §6.5), turned
//! into what the modem understands (3GPP TS 27.007: `+CCFC`, `+CLIR`), and the
//! modem's answers turned back into a sentence.
//!
//! A code is `<procedure><SC>[*SIA[*SIB[*SIC]]]#`: the procedure is `*` activate,
//! `#` deactivate, `*#` interrogate, `**` register, `##` erase; SC the service
//! (21 forward everything, 67 when busy, 61 when unanswered, 62 when unreachable,
//! 002 all, 004 all conditional, 43 call waiting, 31 caller ID); SIA the number to
//! forward to; SIB a basic service group; SIC, for 61, the seconds to ring first.
//!
//! A few codes are not network services at all: the SIM's PIN and PUK changes
//! (`**04*`, `**042*`, `**05*`, `**052*`) and the IMEI (`*#06#`). They come out of
//! [`parse_code`] as their own variants so the daemon can handle them on the device;
//! sent to the network as USSD they would carry a PIN in clear.

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

/// Any keypad code: a network service, or one of the codes the device answers
/// itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Code {
    Service(Ss),
    /// Change a PIN: `**04*old*new*new#` for PIN1 (`which` 1), `**042*…#` for PIN2
    /// (`which` 2). The repeated new PIN has already been checked.
    Pin {
        which: u8,
        old: String,
        new: String,
    },
    /// Unblock a PIN with its PUK: `**05*puk*new*new#` (`which` 1) or `**052*…#`
    /// (`which` 2).
    Puk {
        which: u8,
        puk: String,
        new: String,
    },
    /// `*#06#`: show the IMEI.
    Imei,
}

/// Why a code that is recognisably a service code cannot be carried out, in words
/// for the keypad's reply screen.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Invalid {
    #[error("the ring time before forwarding is 5 to 30 seconds, in steps of 5")]
    NoReplyTime,
    #[error("the two new PINs are not the same")]
    PinMismatch,
    #[error("a PIN is 4 to 8 digits")]
    PinLength,
    #[error("a PUK is 8 digits")]
    PukLength,
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

/// The `+CCFC` reasons that take a no-reply time (27.007 §7.11: "when <reason>=2
/// (no reply), 4 (all call forwarding) or 5 (all conditional call forwarding)").
fn takes_time(reason: u8) -> bool {
    matches!(reason, 2 | 4 | 5)
}

/// Barring service codes (22.030 Annex B), and 03 for changing the barring password.
const BARRING: &[&str] = &["33", "330", "331", "332", "333", "35", "351", "353", "03"];

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
///
/// The codes the device handles itself (PIN, PUK, IMEI) come back as
/// [`Ss::Unsupported`] here, and a code with a bad value (a ring time of 7 s) as
/// None, so a caller of this older form never sends either to the network; use
/// [`parse_code`] to handle them.
pub fn parse(code: &str) -> Option<Ss> {
    match parse_code(code)? {
        Ok(Code::Service(s)) => Some(s),
        Ok(Code::Pin { .. }) => Some(Ss::Unsupported("PIN change")),
        Ok(Code::Puk { .. }) => Some(Ss::Unsupported("PIN unblocking")),
        Ok(Code::Imei) => Some(Ss::Unsupported("IMEI")),
        Err(_) => None,
    }
}

/// Parse a keypad code: None when it is not a service code at all (so USSD, or a
/// number), an error when it is one with a value that cannot be right.
pub fn parse_code(code: &str) -> Option<Result<Code, Invalid>> {
    let c = code.trim();
    if c == "*#06#" {
        return Some(Ok(Code::Imei));
    }
    let (procedure, rest) = ["**", "##", "*#", "*", "#"].iter().find_map(|p| c.strip_prefix(p).map(|r| (*p, r)))?;
    let body = rest.strip_suffix('#')?;
    let parts: Vec<&str> = body.split('*').collect();
    let sc = parts[0];
    if sc.is_empty() || !sc.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    if let Some(which) = match sc {
        "04" | "05" => Some(1),
        "042" | "052" => Some(2),
        _ => None,
    } {
        return sim_code(procedure, sc.starts_with("04"), which, &parts[1..]);
    }
    if BARRING.contains(&sc) {
        // Barring takes the network password as SIA, so any number of parts.
        return Some(Ok(Code::Service(Ss::Unsupported("call barring"))));
    }
    if parts.len() > 4 {
        return None;
    }
    let sia = parts.get(1).copied().filter(|s| !s.is_empty());
    let sic = parts.get(3).copied().filter(|s| !s.is_empty());
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
        // The ring time means something only for the no-reply reasons; given to
        // another it is ignored, as the network would. A bad one is an error
        // rather than quietly forwarding on the network's default.
        let time = match sic {
            Some(t) if takes_time(*reason) => match t.parse::<u8>() {
                Ok(t) if (5..=30).contains(&t) && t % 5 == 0 => Some(t),
                _ => return Some(Err(Invalid::NoReplyTime)),
            },
            _ => None,
        };
        return Some(Ok(Code::Service(Ss::Forward { reason: *reason, mode, number, time })));
    }
    if sia.is_some() {
        // `#31#number` hides the caller ID for one call; that is a call, not this.
        return None;
    }
    Some(Ok(Code::Service(match (sc, procedure) {
        ("43", "*") => Ss::Waiting(Some(true)),
        ("43", "#") => Ss::Waiting(Some(false)),
        ("43", "*#") => Ss::Waiting(None),
        // SC 31 is CLIR, caller-ID *restriction*: activating it (`*31#`) hides the
        // number, deactivating it (`#31#`) shows it, as Android's GsmMmiCode does.
        ("31", "*") => Ss::CallerId(Some(false)),
        ("31", "#") => Ss::CallerId(Some(true)),
        ("31", "*#") => Ss::CallerId(None),
        ("30" | "76" | "77", _) => Ss::Unsupported("caller and connected-line presentation"),
        _ => return None,
    })))
}

/// `**04*old*new*new#` and its PIN2 and PUK siblings (22.030 §6.6). Only the
/// register procedure is defined; any other is not a code.
fn sim_code(procedure: &str, change: bool, which: u8, args: &[&str]) -> Option<Result<Code, Invalid>> {
    let [first, new, again] = args else { return None };
    if procedure != "**" || ![first, new, again].iter().all(|a| a.chars().all(|ch| ch.is_ascii_digit())) {
        return None;
    }
    let pin_ok = |p: &str| (4..=8).contains(&p.len());
    Some(if new != again {
        Err(Invalid::PinMismatch)
    } else if !pin_ok(new) || (change && !pin_ok(first)) {
        Err(Invalid::PinLength)
    } else if !change && first.len() != 8 {
        Err(Invalid::PukLength)
    } else if change {
        Ok(Code::Pin { which, old: first.to_string(), new: new.to_string() })
    } else {
        Ok(Code::Puk { which, puk: first.to_string(), new: new.to_string() })
    })
}

/// A number dialled with a one-call caller-ID prefix (22.030 Annex B): `#31#number`
/// hides the caller ID for this call, `*31#number` shows it. The answer is whether
/// to hide it (Some(true) for `#31#`, Some(false) for `*31#`, None without a
/// prefix) and the number to dial. Note the per-call meaning is the reverse of the
/// standing setting in [`parse`]: Android's GsmMmiCode maps a temporary
/// activation (`*31#number`) to CLIR suppression and a deactivation (`#31#number`)
/// to CLIR invocation.
pub fn strip_clir_prefix(s: &str) -> (Option<bool>, &str) {
    let t = s.trim_start();
    for (prefix, hide) in [("#31#", true), ("*31#", false)] {
        if let Some(rest) = t.strip_prefix(prefix) {
            if rest.trim_start().starts_with(|c: char| c.is_ascii_digit() || c == '+') {
                return (Some(hide), rest.trim_start());
            }
        }
    }
    (None, s)
}

/// The `+CCFC` command for a forwarding code. Class 1 is voice, so data and fax
/// forwarding are left as they are; `<type>` is 145 for a number with `+`. The
/// time goes out only for the reasons that take one.
pub fn forward_command(reason: u8, mode: u8, number: Option<&str>, time: Option<u8>) -> String {
    let time = time.filter(|_| takes_time(reason));
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
                Some(t) if takes_time(reason) => format!("Forwarding {what}: on, to {n} after {t} s"),
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
        assert_eq!(parse("*33*1234#"), Some(Ss::Unsupported("call barring")), "with the password");
        assert_eq!(parse("*#33#"), Some(Ss::Unsupported("call barring")));
        assert_eq!(parse("*#353#"), Some(Ss::Unsupported("call barring")));
        assert_eq!(parse("**03*330*1234*5678*5678#"), Some(Ss::Unsupported("call barring")));
    }

    #[test]
    fn sim_codes_stay_on_the_device() {
        let pin = |which, old: &str, new: &str| Some(Ok(Code::Pin { which, old: old.into(), new: new.into() }));
        assert_eq!(parse_code("**04*1234*5678*5678#"), pin(1, "1234", "5678"));
        assert_eq!(parse_code("**042*1234*87654321*87654321#"), pin(2, "1234", "87654321"));
        assert_eq!(
            parse_code("**05*12345678*1234*1234#"),
            Some(Ok(Code::Puk { which: 1, puk: "12345678".into(), new: "1234".into() }))
        );
        assert_eq!(
            parse_code("**052*12345678*1234*1234#"),
            Some(Ok(Code::Puk { which: 2, puk: "12345678".into(), new: "1234".into() }))
        );
        assert_eq!(parse_code("*#06#"), Some(Ok(Code::Imei)));
        assert_eq!(parse_code("**04*1234*5678*5679#"), Some(Err(Invalid::PinMismatch)));
        assert_eq!(parse_code("**04*1234*567*567#"), Some(Err(Invalid::PinLength)));
        assert_eq!(parse_code("**05*1234*5678*5678#"), Some(Err(Invalid::PukLength)));
        assert_eq!(parse_code("**04*1234#"), None);
        // The older entry point never lets one through as something to send.
        assert_eq!(parse("**04*1234*5678*5678#"), Some(Ss::Unsupported("PIN change")));
        assert_eq!(parse("**05*12345678*1234*1234#"), Some(Ss::Unsupported("PIN unblocking")));
        assert_eq!(parse("*#06#"), Some(Ss::Unsupported("IMEI")));
    }

    #[test]
    fn no_reply_time() {
        assert_eq!(parse_code("**61*0412345678**7#"), Some(Err(Invalid::NoReplyTime)));
        assert_eq!(parse_code("**61*0412345678**35#"), Some(Err(Invalid::NoReplyTime)));
        assert_eq!(parse_code("**61*0412345678**x#"), Some(Err(Invalid::NoReplyTime)));
        assert_eq!(parse("**61*0412345678**7#"), None);
        assert_eq!(
            parse("**004*0412345678**30#"),
            Some(Ss::Forward { reason: 5, mode: mode::REGISTER, number: Some("0412345678".into()), time: Some(30) })
        );
        // Forwarding everything has no ring time to give.
        assert_eq!(
            parse("**21*0412345678**20#"),
            Some(Ss::Forward { reason: 0, mode: mode::REGISTER, number: Some("0412345678".into()), time: None })
        );
        assert_eq!(forward_command(0, 3, Some("0412"), Some(20)), "AT+CCFC=0,3,\"0412\",129,1");
        assert_eq!(forward_command(4, 3, Some("0412"), Some(20)), "AT+CCFC=4,3,\"0412\",129,1,,,20");
    }

    #[test]
    fn one_call_caller_id() {
        assert_eq!(strip_clir_prefix("#31#0412345678"), (Some(true), "0412345678"));
        assert_eq!(strip_clir_prefix("*31#+61412345678"), (Some(false), "+61412345678"));
        assert_eq!(strip_clir_prefix("0412345678"), (None, "0412345678"));
        assert_eq!(strip_clir_prefix("#31#"), (None, "#31#"), "the standing setting, not a call");
        assert_eq!(strip_clir_prefix("*31#*100#"), (None, "*31#*100#"));
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
