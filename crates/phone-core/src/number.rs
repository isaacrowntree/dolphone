//! Phone numbers: one canonical form for storage and matching, a readable form for
//! the panel, and the classes the daemon treats specially.
//!
//! Everything is stored in E.164 (`+61412345678`) when the number parses, and as the
//! digits the network sent otherwise: short codes and alphanumeric senders ("Telstra",
//! "AusPost") have no E.164 form, and a message from one must still land in a thread.

use phonenumber::country::Id;
use phonenumber::Mode;

/// The region a number without a country code is read in. Australia until the
/// settings say otherwise, because that is where this is being built and tested.
pub const DEFAULT_REGION: &str = "AU";

/// What a dialled or received number is, as far as the daemon's rules care.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// An emergency number. Never blocked, never sent over SIP, never delayed by a
    /// confirmation when the user dialled it on the keypad.
    Emergency,
    /// A premium-rate number: confirmed on the device every time, whoever asks.
    Premium,
    /// A short code (fewer digits than any subscriber number), which in practice is
    /// a carrier or premium SMS service: confirmed like a premium number.
    Short,
    /// A number in another country from the home region.
    International,
    /// Anything else.
    Ordinary,
}

/// Emergency numbers that work on every GSM/LTE network regardless of region, plus
/// the common national ones, so a visitor's habit still reaches help. The network
/// decides what actually answers; the rule here is only that nothing in this stack
/// gets in the way.
const EMERGENCY_ANYWHERE: &[&str] = &["112", "911", "000", "999", "08", "110", "118", "119"];

/// Region-specific additions: Australia's text-relay and TTY emergency number.
fn emergency_extra(region: &str) -> &'static [&'static str] {
    match region {
        "AU" => &["106"],
        _ => &[],
    }
}

fn region_id(region: &str) -> Id {
    region.parse().unwrap_or(Id::AU)
}

/// The digits and a leading `+`, nothing else: what a keypad or a contact card
/// produces once spaces, dashes and brackets are gone.
pub fn digits(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for (i, c) in raw.trim().chars().enumerate() {
        if c.is_ascii_digit() || (c == '+' && i == 0) || c == '*' || c == '#' {
            out.push(c);
        }
    }
    out
}

/// The canonical form: E.164 when the number parses as a full number in `region`,
/// otherwise the bare digits (or the sender name, for an alphanumeric sender).
pub fn canonical(raw: &str, region: &str) -> String {
    let d = digits(raw);
    if d.is_empty() {
        // An alphanumeric sender: keep it as the network named it, trimmed.
        return raw.trim().to_string();
    }
    if is_emergency(&d, region) || d.len() < 6 || d.contains(['*', '#']) {
        return d;
    }
    match phonenumber::parse(Some(region_id(region)), &d) {
        Ok(n) if phonenumber::is_valid(&n) => n.format().mode(Mode::E164).to_string(),
        _ => d,
    }
}

/// How the panel shows a number: national format for the home region, international
/// for any other, the raw form for anything that does not parse.
pub fn display(stored: &str, region: &str) -> String {
    let d = digits(stored);
    if d.len() < 6 {
        return stored.to_string();
    }
    match phonenumber::parse(Some(region_id(region)), &d) {
        Ok(n) if phonenumber::is_valid(&n) => {
            let home = n.country().id() == Some(region_id(region));
            let mode = if home { Mode::National } else { Mode::International };
            n.format().mode(mode).to_string()
        }
        _ => stored.to_string(),
    }
}

pub fn is_emergency(raw: &str, region: &str) -> bool {
    let d = digits(raw);
    EMERGENCY_ANYWHERE.contains(&d.as_str()) || emergency_extra(region).contains(&d.as_str())
}

/// Classify a number for the daemon's rules. Emergency wins over everything, so a
/// rule further down can never catch 000.
pub fn classify(raw: &str, region: &str) -> Class {
    let d = digits(raw);
    if is_emergency(&d, region) {
        return Class::Emergency;
    }
    // USSD and supplementary-service strings are not numbers to call; the dialer
    // sends them to the USSD path instead, and the daemon treats them as short.
    if d.contains(['*', '#']) {
        return Class::Short;
    }
    let national = national_digits(&d, region);
    if region == "AU" && national.starts_with("19") {
        return Class::Premium;
    }
    if national.len() < 8 && !d.starts_with('+') {
        return Class::Short;
    }
    match phonenumber::parse(Some(region_id(region)), &d) {
        Ok(n) if n.country().id() != Some(region_id(region)) => Class::International,
        _ => Class::Ordinary,
    }
}

/// The national significant number with any trunk prefix dropped, or the digits as
/// they are when the number does not parse.
fn national_digits(d: &str, region: &str) -> String {
    match phonenumber::parse(Some(region_id(region)), d) {
        Ok(n) => n.national().value().to_string(),
        Err(_) => d.trim_start_matches('0').to_string(),
    }
}

/// A supplementary-service code (3GPP TS 22.030): call forwarding (`**21*n#`),
/// waiting (`*43#`), caller-ID (`#31#`) and the like. Not USSD, though it looks like
/// it: USSD service codes are any other `*…#`.
pub fn is_supplementary(code: &str) -> bool {
    const SERVICES: &[&str] = &[
        "21", "61", "62", "67", "002", "004", "30", "31", "33", "330", "331", "332", "333", "35", "351", "43", "76",
        "77",
    ];
    let c = code.trim();
    let body = c
        .strip_prefix("**")
        .or_else(|| c.strip_prefix("##"))
        .or_else(|| c.strip_prefix("*#"))
        .or_else(|| c.strip_prefix('*'))
        .or_else(|| c.strip_prefix('#'));
    let Some(body) = body else { return false };
    let sc: String = body.chars().take_while(|ch| ch.is_ascii_digit()).collect();
    SERVICES.contains(&sc.as_str()) && body[sc.len()..].starts_with(['*', '#'])
}

/// A number as it may appear in a log: the country code and the last three digits,
/// so two log lines can be told apart without the log being a contact list.
pub fn mask(raw: &str) -> String {
    let d = digits(raw);
    if d.len() <= 4 {
        return d;
    }
    let tail = &d[d.len() - 3..];
    let head = if d.starts_with('+') { &d[..3.min(d.len())] } else { "" };
    format!("{head}…{tail}")
}

/// Whether two stored numbers are the same party: equal canonical forms, or the
/// same last nine digits, which is how a contact saved as `0412…` matches a message
/// from `+61412…` when one side failed to parse.
pub fn same_party(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let (da, db) = (digits(a), digits(b));
    da.len() >= 9 && db.len() >= 9 && da[da.len() - 9..] == db[db.len() - 9..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_au_mobile() {
        assert_eq!(canonical("0412 345 678", "AU"), "+61412345678");
        assert_eq!(canonical("+61 412-345-678", "AU"), "+61412345678");
        assert_eq!(canonical("(02) 9374 4000", "AU"), "+61293744000");
    }

    #[test]
    fn canonical_keeps_what_is_not_a_number() {
        assert_eq!(canonical("Telstra", "AU"), "Telstra");
        assert_eq!(canonical("000", "AU"), "000");
        assert_eq!(canonical("*100#", "AU"), "*100#");
        assert_eq!(canonical("1234", "AU"), "1234");
    }

    #[test]
    fn display_national_and_international() {
        assert_eq!(display("+61412345678", "AU"), "0412 345 678");
        assert_eq!(display("+442079460000", "AU"), "+44 20 7946 0000");
        assert_eq!(display("Telstra", "AU"), "Telstra");
    }

    #[test]
    fn emergency_always_wins() {
        for n in ["000", "112", "911", "106", " 0 0 0 "] {
            assert_eq!(classify(n, "AU"), Class::Emergency, "{n}");
        }
        assert_eq!(classify("106", "GB"), Class::Short);
        assert_eq!(classify("999", "GB"), Class::Emergency);
    }

    #[test]
    fn premium_short_international() {
        assert_eq!(classify("1900 123 456", "AU"), Class::Premium);
        assert_eq!(classify("19 12 34", "AU"), Class::Premium);
        assert_eq!(classify("1234", "AU"), Class::Short);
        assert_eq!(classify("*100#", "AU"), Class::Short);
        assert_eq!(classify("+44 20 7946 0000", "AU"), Class::International);
        assert_eq!(classify("0412 345 678", "AU"), Class::Ordinary);
        assert_eq!(classify("13 13 13", "AU"), Class::Short);
        assert_eq!(classify("1800 123 456", "AU"), Class::Ordinary);
    }

    #[test]
    fn supplementary_is_not_ussd() {
        for c in ["**21*0412345678#", "##002#", "*43#", "#31#", "*#21#", "##21#"] {
            assert!(is_supplementary(c), "{c}");
        }
        for c in ["*100#", "*123#", "#100#", "*1#", "*101*1#"] {
            assert!(!is_supplementary(c), "{c}");
        }
    }

    #[test]
    fn masking_hides_the_middle() {
        assert_eq!(mask("+61412345678"), "+61…678");
        assert_eq!(mask("0412345678"), "…678");
        assert_eq!(mask("000"), "000");
    }

    #[test]
    fn same_party_across_forms() {
        assert!(same_party("+61412345678", "0412345678"));
        assert!(same_party("+61412345678", "+61412345678"));
        assert!(!same_party("+61412345678", "+61412345679"));
        assert!(!same_party("1234", "01234"));
    }
}
