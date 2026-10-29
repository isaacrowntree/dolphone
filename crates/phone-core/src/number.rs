//! Phone numbers: one canonical form for storage and matching, a readable form for
//! the panel, and the classes the daemon treats specially.
//!
//! Everything is stored in E.164 (`+61412345678`) when the number parses, and as the
//! digits the network sent otherwise: short codes and alphanumeric senders ("Telstra",
//! "AusPost") have no E.164 form, and a message from one must still land in a thread.

use phonenumber::country::Id;
use phonenumber::metadata::DATABASE;
use phonenumber::{Mode, PhoneNumber, Type};

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

/// National numbers by region, beyond the ones that work anywhere. Sources, checked
/// October 2026: Wikipedia's "List of emergency telephone numbers" for New Zealand,
/// India, Singapore, Russia, Brazil, China and South Africa; for India also the
/// Lok Sabha answers listing 100, 101, 102 and the reserved 108; for France,
/// service-public.fr ("What are the emergency numbers to call", F33954), which lists
/// 15, 17, 18, 114 (texts for deaf callers) and 115 (Samu social).
fn emergency_extra(region: &str) -> &'static [&'static str] {
    match region {
        // Australia's text-relay and TTY emergency number.
        "AU" => &["106"],
        "NZ" => &["111"],
        "IN" => &["100", "101", "102", "108"],
        "SG" => &["995"],
        "FR" => &["15", "17", "18", "114", "115"],
        "RU" => &["101", "102", "103"],
        "BR" => &["190", "192", "193"],
        "CN" => &["120"],
        "ZA" => &["10111", "10177"],
        _ => &[],
    }
}

fn listed(d: &str, region: &str) -> bool {
    EMERGENCY_ANYWHERE.contains(&d) || emergency_extra(region).contains(&d)
}

fn region_id(region: &str) -> Id {
    region.parse().unwrap_or(Id::AU)
}

/// The country calling code of `region` (61 for AU), with the same fallback as
/// [`region_id`].
fn region_code(region: &str) -> u16 {
    DATABASE.by_id(region_id(region).as_ref()).map(|m| m.country_code()).unwrap_or(61)
}

/// The part of the text that is the number: a `tel:` scheme dropped, and anything
/// from an extension or post-dial marker on cut off ("ext 23", "x23", ";ext=23",
/// ",23"), so the extension is not read as more digits of the number.
fn number_part(raw: &str) -> &str {
    let s = raw.trim();
    let s = match s.get(..4) {
        Some(p) if p.eq_ignore_ascii_case("tel:") => s[4..].trim_start(),
        _ => s,
    };
    let mut seen_digit = false;
    let mut last: Option<char> = None;
    for (i, c) in s.char_indices() {
        if seen_digit {
            if c == ',' || c == ';' {
                return &s[..i];
            }
            // "x" or "ext" straight after a digit (spaces and dots aside), so a
            // sender like "AMEX365" is not cut at its x.
            let after_digit = last.is_some_and(|p| p.is_ascii_digit());
            let rest = &s[i..];
            let ext = rest.get(..3).is_some_and(|w| w.eq_ignore_ascii_case("ext"));
            if after_digit && (c.eq_ignore_ascii_case(&'x') || ext) {
                return &s[..i];
            }
        }
        if c.is_ascii_digit() {
            seen_digit = true;
        }
        if !c.is_whitespace() && c != '.' {
            last = Some(c);
        }
    }
    s
}

/// Whether the text is a name rather than a number: an alphanumeric sender such as
/// "O2", "AMEX365" or "Info112". Its digits are part of the name, so it is neither
/// reduced to them nor classed by them.
fn is_alphanumeric(raw: &str) -> bool {
    number_part(raw).chars().any(char::is_alphabetic)
}

/// The digits and a leading `+`, nothing else: what a keypad or a contact card
/// produces once spaces, dashes, brackets and any extension are gone. The `+` is
/// kept wherever it comes before the first digit, as in "(+44) 20 7946 0000".
pub fn digits(raw: &str) -> String {
    let s = number_part(raw);
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_digit() || c == '*' || c == '#' || (c == '+' && out.is_empty()) {
            out.push(c);
        }
    }
    out
}

/// The canonical form: E.164 when the number parses as a full number in `region`,
/// otherwise the bare digits (or the sender name, for an alphanumeric sender).
pub fn canonical(raw: &str, region: &str) -> String {
    if is_alphanumeric(raw) {
        // An alphanumeric sender: keep it as the network named it, trimmed.
        return raw.trim().to_string();
    }
    let d = digits(raw);
    if d.is_empty() {
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

/// A number prefix ("0491 57" for every number starting that way) in the stored
/// form, with one full number it covers: the prefix is padded with zeros until it
/// reads as a valid number, so its national trunk prefix and country code come out
/// right. None when no number starts that way.
pub fn canonical_prefix(stem: &str, region: &str) -> Option<(String, String)> {
    let d = digits(stem);
    if d.is_empty() || d.contains(['*', '#']) || is_alphanumeric(stem) {
        return None;
    }
    for pad in 0..=12 {
        let full = canonical(&format!("{d}{}", "0".repeat(pad)), region);
        if full.starts_with('+') && full.len() > pad {
            return Some((full[..full.len() - pad].to_string(), full));
        }
    }
    None
}

/// How the panel shows a number: national format for the home region, international
/// for any other, the raw form for anything that does not parse.
/// A thread's peer: one number, or for a group text every member's, canonical,
/// sorted and joined with commas (as phoned stores them).
pub fn canonical_peer(raw: &str, region: &str) -> String {
    if !raw.contains(',') {
        return canonical(raw, region);
    }
    let mut members: Vec<String> = raw.split(',').map(|n| canonical(n, region)).filter(|n| !n.is_empty()).collect();
    members.sort();
    members.dedup();
    members.join(",")
}

/// A peer for showing: each member of a group in national form, comma separated.
pub fn display_peer(stored: &str, region: &str) -> String {
    stored.split(',').map(|n| display(n, region)).collect::<Vec<_>>().join(", ")
}

pub fn display(stored: &str, region: &str) -> String {
    let d = digits(stored);
    if d.len() < 6 || is_alphanumeric(stored) {
        return stored.to_string();
    }
    match phonenumber::parse(Some(region_id(region)), &d) {
        Ok(n) if phonenumber::is_valid(&n) => {
            // By calling code, not region: a Jersey number is national in GB.
            let home = n.code().value() == region_code(region);
            let mode = if home { Mode::National } else { Mode::International };
            n.format().mode(mode).to_string()
        }
        _ => stored.to_string(),
    }
}

/// Whether a number reaches the emergency services: one of the numbers in
/// [`EMERGENCY_ANYWHERE`] or the region's own, bare or after a `+` and a country
/// code ("+61 000", "+1 911", "+112"), which is how a number copied from a contact
/// card or a web page can arrive. The prefixed form counts only when it is too
/// short to be a real number, so a subscriber number ending in 112 is not caught.
pub fn is_emergency(raw: &str, region: &str) -> bool {
    if is_alphanumeric(raw) {
        return false;
    }
    let d = digits(raw);
    if listed(&d, region) {
        return true;
    }
    let Some(rest) = d.strip_prefix('+') else { return false };
    if rest.is_empty() || rest.len() > 8 || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    if phonenumber::parse(None, &d).is_ok_and(|n| n.is_valid()) {
        return false;
    }
    (0..=3.min(rest.len() - 1)).any(|cc_len| {
        let (cc, num) = rest.split_at(cc_len);
        if cc_len == 0 {
            return listed(num, region);
        }
        let Ok(code) = cc.parse::<u16>() else { return false };
        let Some(regions) = DATABASE.region(&code) else { return false };
        // The number as the home network would read it, or as that country would.
        listed(num, region) || regions.iter().any(|r| listed(num, r))
    })
}

/// Classify a number for the daemon's rules. Emergency wins over everything, so a
/// rule further down can never catch 000.
pub fn classify(raw: &str, region: &str) -> Class {
    // An alphanumeric sender cannot be dialled, and its digits are part of a name:
    // "Info112" is neither an emergency number nor a short code.
    if is_alphanumeric(raw) {
        return Class::Ordinary;
    }
    let d = digits(raw);
    if is_emergency(&d, region) {
        return Class::Emergency;
    }
    // USSD and supplementary-service strings are not numbers to call; the dialer
    // sends them to the USSD path instead, and the daemon treats them as short.
    if d.contains(['*', '#']) {
        return Class::Short;
    }
    let home = region_code(region);
    let parsed = phonenumber::parse(Some(region_id(region)), &d).ok();
    let valid = parsed.as_ref().filter(|n| n.is_valid());
    // Australia's 19 numbers are premium whatever their length (1900 123 456 and the
    // six-digit 19 12 34 alike), but only Australia's: "+44 1902 …" is not one.
    let code = parsed.as_ref().map_or(home, |n| n.code().value());
    if code == 61 && national_digits(&d, region).starts_with("19") {
        return Class::Premium;
    }
    if valid.is_some_and(|n| n.number_type(&DATABASE) == Type::PremiumRate) {
        return Class::Premium;
    }
    if d.trim_start_matches('+').len() < 6 {
        return Class::Short;
    }
    match valid {
        // Too short to be a subscriber number and not a valid one: a short code.
        None if !d.starts_with('+') && national_digits(&d, region).len() < 8 => Class::Short,
        None => Class::Ordinary,
        Some(n) if n.code().value() != home => Class::International,
        // A valid six-digit service number (Australia's 13 13 13) is a short code
        // too; a six-digit fixed or mobile number somewhere is just a number.
        Some(n) if is_service_short(n) => Class::Short,
        _ => Class::Ordinary,
    }
}

fn is_service_short(n: &PhoneNumber) -> bool {
    n.national().value().to_string().len() <= 6
        && !matches!(n.number_type(&DATABASE), Type::FixedLine | Type::Mobile | Type::FixedLineOrMobile)
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
/// it: USSD service codes are any other `*…#`. The SIM's PIN and PUK codes
/// (`**04*`, `**042*`, `**05*`, `**052*`), the barring password (`**03*`) and the
/// IMEI request (`*#06#`) are in the list too: sent as USSD they would hand a PIN
/// to the network in clear, so the daemon handles them itself (see [`crate::ss`]).
pub fn is_supplementary(code: &str) -> bool {
    const SERVICES: &[&str] = &[
        "21", "61", "62", "67", "002", "004", "30", "31", "33", "330", "331", "332", "333", "35", "351", "353", "43",
        "76", "77", "03", "04", "042", "05", "052",
    ];
    let c = code.trim();
    if c == "*#06#" {
        return true;
    }
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
/// from `+61412…` when one side failed to parse. Two full international numbers
/// are compared whole, because +1 212 555 0123 and +1 312 555 0123 share nine
/// digits; and a sender name or a group matches only itself.
pub fn same_party(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    // A group's members are joined with commas, where digits() stops reading.
    if is_alphanumeric(a) || is_alphanumeric(b) || a.contains(',') || b.contains(',') {
        return false;
    }
    let (da, db) = (digits(a), digits(b));
    if da.starts_with('+') && db.starts_with('+') {
        return da == db;
    }
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

    #[test]
    fn prefixes_read_like_numbers() {
        let (p, full) = canonical_prefix("0491 571", "AU").unwrap();
        assert_eq!(p, "+61491571");
        assert!(full.starts_with("+61491571") && full.len() == 12, "{full}");
        assert_eq!(canonical_prefix("+44 7700", "AU").unwrap().0, "+447700");
        assert_eq!(canonical_prefix("1900", "AU").map(|(_, f)| classify(&f, "AU")), Some(Class::Premium));
    }

    #[test]
    fn emergency_by_region() {
        for (n, r) in [
            ("111", "NZ"),
            ("100", "IN"),
            ("101", "IN"),
            ("102", "IN"),
            ("108", "IN"),
            ("995", "SG"),
            ("15", "FR"),
            ("17", "FR"),
            ("18", "FR"),
            ("115", "FR"),
            ("101", "RU"),
            ("103", "RU"),
            ("190", "BR"),
            ("193", "BR"),
            ("120", "CN"),
            ("10111", "ZA"),
            ("10177", "ZA"),
        ] {
            assert_eq!(classify(n, r), Class::Emergency, "{n} in {r}");
        }
        assert_ne!(classify("111", "AU"), Class::Emergency);
        assert_ne!(classify("15", "AU"), Class::Emergency);
    }

    #[test]
    fn emergency_after_a_country_code() {
        for n in ["+61 000", "+112", "+1 911", "+44 999", "+64 111", "+27 10111"] {
            assert!(is_emergency(n, "AU"), "{n}");
            assert_eq!(classify(n, "AU"), Class::Emergency, "{n}");
        }
        assert_eq!(canonical("+61 000", "AU"), "+61000");
        // Real numbers that happen to end in an emergency number.
        for n in ["+61 2 9374 4112", "+44 20 7946 0112", "+1 212 555 0911"] {
            assert!(!is_emergency(n, "AU"), "{n}");
        }
        assert!(!is_emergency("+", "AU"));
    }

    #[test]
    fn alphanumeric_senders_keep_their_names() {
        for n in ["O2", "AMEX365", "Info112", " Vodafone 4U "] {
            assert_eq!(canonical(n, "AU"), n.trim(), "{n}");
            assert_eq!(classify(n, "AU"), Class::Ordinary, "{n}");
            assert!(!is_emergency(n, "AU"), "{n}");
        }
        assert!(!same_party("Info112", "112"));
        assert!(!same_party("O2", "02"));
        assert_eq!(display("AMEX365123", "AU"), "AMEX365123");
        assert_eq!(canonical("tel:+61412345678", "AU"), "+61412345678");
        assert_eq!(canonical_prefix("AMEX", "AU"), None);
    }

    #[test]
    fn digits_keep_the_plus_and_drop_the_extension() {
        assert_eq!(digits("(+44) 20 7946 0000"), "+442079460000");
        assert_eq!(digits("+1 555 555 0100 ext 23"), "+15555550100");
        assert_eq!(digits("+1 555 555 0100 x23"), "+15555550100");
        assert_eq!(digits("+1 555 555 0100 X 23"), "+15555550100");
        assert_eq!(digits("tel:+15555550100;ext=23"), "+15555550100");
        assert_eq!(digits("0412 345 678,123"), "0412345678");
        assert_eq!(digits("0412 345 678;123"), "0412345678");
        assert_eq!(digits("1+2"), "12");
        assert_eq!(digits("*100#"), "*100#");
        assert_eq!(canonical("+1 650 253 0000 ext 23", "AU"), "+16502530000");
        assert_eq!(classify("+1 555 555 0100 ext 23", "AU"), classify("+1 555 555 0100", "AU"));
    }

    #[test]
    fn same_party_compares_full_numbers_whole() {
        assert!(!same_party("+12125550123", "+13125550123"));
        assert!(same_party("+12125550123", "2125550123"));
        assert!(same_party("+61412345678", "+61 412 345 678"));
        assert!(!same_party("+61411111111,+61422222222", "+61411111111,+61433333333"), "groups compare whole");
    }

    #[test]
    fn premium_by_country() {
        assert_eq!(classify("+44 1902 123456", "AU"), Class::International, "Wolverhampton, not an AU 19 number");
        assert_eq!(classify("09001 234567", "DE"), Class::Premium);
        assert_eq!(classify("0909 879 0000", "GB"), Class::Premium);
        assert_eq!(classify("1 900 555 0100", "US"), Class::Premium);
        assert_eq!(classify("0900 12345", "NZ"), Class::Premium);
        assert_eq!(classify("+61 1900 123 456", "GB"), Class::Premium);
    }

    #[test]
    fn short_only_when_not_a_number() {
        assert_eq!(classify("030 12345", "DE"), Class::Ordinary);
        assert_eq!(classify("030 1234567", "DE"), Class::Ordinary);
        assert_eq!(classify("13 13 13", "AU"), Class::Short);
        assert_eq!(classify("106", "GB"), Class::Short);
        assert_eq!(classify("12345", "DE"), Class::Short);
        assert_eq!(classify("0412 345", "AU"), Class::Short, "too short for a mobile");
    }

    #[test]
    fn international_by_calling_code() {
        assert_eq!(classify("+44 1534 456789", "GB"), Class::Ordinary, "Jersey shares +44");
        assert_eq!(classify("+44 1624 456789", "GB"), Class::Ordinary, "Isle of Man shares +44");
        assert_eq!(classify("+61 8 9162 1234", "AU"), Class::Ordinary, "Cocos shares +61");
        assert_eq!(classify("+44 20 7946 000", "AU"), Class::Ordinary, "invalid, so not known to be abroad");
        assert_eq!(classify("+1 650 253 0000", "CA"), Class::Ordinary, "NANP");
        assert_eq!(display("+441534456789", "GB"), "01534 456789");
    }

    #[test]
    fn sim_codes_are_not_ussd() {
        for c in ["**04*1234*5678*5678#", "**042*1234*5678*5678#", "**05*12345678*1234*1234#", "**052*1*2*2#", "*#06#"]
        {
            assert!(is_supplementary(c), "{c}");
        }
        for c in ["*#353#", "**03*330*1234*5678*5678#"] {
            assert!(is_supplementary(c), "{c}");
        }
    }
}
