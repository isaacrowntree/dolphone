//! Message text: making what the network delivers safe to store and draw, and
//! counting what an outgoing text will cost.
//!
//! Everything in a message body and a sender name is chosen by whoever sent it, so
//! it is cleaned once, on the way in, before it reaches the store or a log. The
//! panel's fonts are ASCII and the apps transliterate for drawing; this layer only
//! removes what no font should ever be handed.

/// Longest body kept. A concatenated SMS tops out near 39 KB in theory; nothing a
/// person sends is close, and a hostile one should not be able to fill the disk or
/// stall a redraw.
pub const MAX_BODY: usize = 4096;

/// Longest sender kept. Alphanumeric senders are at most 11 characters; numbers are
/// at most 15 digits plus a `+`.
pub const MAX_SENDER: usize = 32;

/// Remove control characters (keeping newline and tab), directional overrides and
/// isolates (which can make a link or a number read backwards), invisible
/// characters, and cap the length.
pub fn clean(input: &str, max: usize) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::new();
    let mut n = 0;
    for (i, &c) in chars.iter().enumerate() {
        if n >= max {
            break;
        }
        // A joiner between two emoji (families, flags, skin tones) is part of the
        // emoji; anywhere else it only hides things.
        let joins_emoji =
            c == '\u{200d}' && i > 0 && i + 1 < chars.len() && is_emoji(chars[i - 1]) && is_emoji(chars[i + 1]);
        if is_hostile(c) && !joins_emoji {
            continue;
        }
        out.push(if c == '\t' || c == '\r' { ' ' } else { c });
        n += 1;
    }
    out
}

fn is_emoji(c: char) -> bool {
    matches!(c as u32, 0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0xFE0F)
}

fn is_hostile(c: char) -> bool {
    match c {
        '\n' | '\t' | '\r' => false,
        // C0, DEL and C1 controls.
        '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}' => true,
        // Bidi embedding, override and isolate controls, and the marks (the Arabic
        // letter mark among them).
        '\u{200e}' | '\u{200f}' | '\u{61c}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' => true,
        // Zero-width characters used to hide text inside text, the soft hyphen
        // (invisible unless a line breaks at it), and the tag characters, which
        // draw as nothing and can smuggle a whole hidden string.
        '\u{200b}'..='\u{200d}' | '\u{2060}' | '\u{feff}' | '\u{ad}' | '\u{e0000}'..='\u{e007f}' => true,
        // Line and paragraph separators: a line break the store and the panel
        // would each treat differently.
        '\u{2028}' | '\u{2029}' => true,
        _ => false,
    }
}

/// How an outgoing text is encoded and how many network messages it takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segments {
    /// GSM 7-bit when every character is in the default alphabet or its extension
    /// table; UCS-2 otherwise, which is what one emoji does to a whole message.
    pub gsm7: bool,
    pub count: usize,
    /// Characters (in the encoding's units) left before another segment starts.
    pub remaining: usize,
}

const GSM7_BASIC: &str = "@£$¥èéùìòÇ\nØø\rÅåΔ_ΦΓΛΩΠΨΣΘΞÆæßÉ !\"#¤%&'()*+,-./0123456789:;<=>?¡ABCDEFGHIJKLMNOPQRSTUVWXYZÄÖÑÜ§¿abcdefghijklmnopqrstuvwxyzäöñüà";
const GSM7_EXTENSION: &str = "^{}\\[~]|€\u{c}";

/// The segment count the network will bill, and what is left in the last one.
pub fn segments(body: &str) -> Segments {
    let gsm_units: Option<Vec<usize>> = body
        .chars()
        .map(|c| {
            if GSM7_BASIC.contains(c) {
                Some(1)
            } else if GSM7_EXTENSION.contains(c) {
                Some(2)
            } else {
                None
            }
        })
        .collect();
    let (units, single, multi, gsm7) = match gsm_units {
        Some(u) => (u, 160, 153, true),
        None => (body.chars().map(char::len_utf16).collect(), 70, 67, false),
    };
    let total: usize = units.iter().sum();
    if total <= single {
        return Segments { gsm7, count: 1, remaining: single - total };
    }
    // Packed part by part, because a character is never split between parts: an
    // extension character's escape and code, or a surrogate pair, that would
    // straddle a boundary moves whole to the next part, leaving one unit unused.
    let (mut count, mut used) = (1, 0);
    for u in units {
        if used + u > multi {
            count += 1;
            used = 0;
        }
        used += u;
    }
    Segments { gsm7, count, remaining: multi - used }
}

/// The one-time code in a text, if it has one: a run of 4 to 8 digits (a dash or
/// space may split it, as in "123-456"), preferring one near the word "code".
/// Phone numbers, amounts and times are not codes.
pub fn one_time_code(body: &str) -> Option<String> {
    let lower = body.to_lowercase();
    let hint = ["code", "otp", "pin", "passcode", "verification", "verify", "password"];
    if !hint.iter().any(|h| lower.contains(h)) {
        return None;
    }
    let chars: Vec<char> = body.chars().collect();
    let mut found: Vec<(usize, String)> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !chars[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        let mut digits = String::new();
        while i < chars.len() {
            if chars[i].is_ascii_digit() {
                digits.push(chars[i]);
                i += 1;
            } else if (chars[i] == '-' || chars[i] == ' ')
                && i + 1 < chars.len()
                && chars[i + 1].is_ascii_digit()
                && digits.len() == 3
            {
                i += 1;
            } else {
                break;
            }
        }
        let before = if start > 0 { chars[start - 1] } else { ' ' };
        let after = chars.get(i).copied().unwrap_or(' ');
        let money = matches!(before, '$' | '€' | '£' | '¥') || after == '%';
        let part_of_more = before == '+'
            || before == '.'
            || before == ','
            || after == ':'
            || after == '.' && chars.get(i + 1).is_some_and(char::is_ascii_digit);
        if (4..=8).contains(&digits.len()) && !money && !part_of_more {
            found.push((start, digits));
        }
    }
    let at = lower.find("code").or_else(|| hint.iter().find_map(|h| lower.find(h))).unwrap_or(0);
    // Byte offsets and char offsets differ only for non-ASCII text, where "near"
    // is close enough.
    found.into_iter().min_by_key(|(pos, _)| pos.abs_diff(at)).map(|(_, d)| d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_removes_controls_and_bidi() {
        assert_eq!(clean("hi\u{7}there", 100), "hithere");
        assert_eq!(clean("pay \u{202e}gnp.exe", 100), "pay gnp.exe");
        assert_eq!(clean("a\tb\r\nc", 100), "a b \nc");
        assert_eq!(clean("zero\u{200b}width", 100), "zerowidth");
        assert_eq!(clean("a\u{200d}b", 100), "ab", "a joiner between letters is hidden text");
        let family = "\u{1F468}\u{200d}\u{1F469}\u{200d}\u{1F467}";
        assert_eq!(clean(family, 100), family, "a joiner between emoji is kept");
        assert_eq!(clean("long", 2), "lo");
        assert_eq!(clean("a\u{61c}b\u{2028}c\u{2029}d", 100), "abcd");
        assert_eq!(clean("pay\u{ad}pal", 100), "paypal");
        assert_eq!(clean("hi\u{e0041}\u{e0042}\u{e007f}", 100), "hi", "tag characters hide text");
    }

    #[test]
    fn characters_are_not_split_across_parts() {
        // 152 plain septets and a euro: the escape would end part one and the code
        // start part two, so the euro moves whole and part one carries 152.
        let s = segments(&format!("{}€{}", "a".repeat(152), "a".repeat(10)));
        assert_eq!((s.count, s.remaining), (2, 153 - 12));
        let s = segments(&format!("{}€{}", "a".repeat(152), "a".repeat(151)));
        assert_eq!((s.count, s.remaining), (2, 0));
        let s = segments(&format!("{}€{}", "a".repeat(152), "a".repeat(152)));
        assert_eq!(s.count, 3, "without the rule this would still fit two parts");
        // UCS-2: 66 units and an emoji, a surrogate pair, which moves to part two.
        let s = segments(&format!("{}👍{}", "ę".repeat(66), "ę".repeat(3)));
        assert_eq!((s.count, s.remaining), (2, 67 - 5));
        let s = segments(&format!("{}👍{}", "ę".repeat(66), "ę".repeat(66)));
        assert_eq!(s.count, 3);
    }

    #[test]
    fn single_gsm_segment() {
        let s = segments("Running late, see you at 7?");
        assert!(s.gsm7);
        assert_eq!(s.count, 1);
        assert_eq!(s.remaining, 160 - 27);
    }

    #[test]
    fn multipart_gsm() {
        let body = "a".repeat(161);
        let s = segments(&body);
        assert_eq!((s.count, s.remaining), (2, 306 - 161));
    }

    #[test]
    fn extension_characters_cost_two() {
        let s = segments("€");
        assert!(s.gsm7);
        assert_eq!(s.remaining, 158);
    }

    #[test]
    fn emoji_forces_ucs2() {
        let s = segments("ok 👍");
        assert!(!s.gsm7);
        assert_eq!(s.count, 1);
        assert_eq!(s.remaining, 70 - 5);
        assert_eq!(segments(&"é".repeat(10).replace('é', "ę")).count, 1);
        assert_eq!(segments(&"ę".repeat(71)).count, 2);
    }

    #[test]
    fn finds_one_time_codes() {
        assert_eq!(one_time_code("Your Example code is 482913. Don't share it.").as_deref(), Some("482913"));
        assert_eq!(one_time_code("G-123456 is your verification code").as_deref(), Some("123456"));
        assert_eq!(one_time_code("Your code: 123-456").as_deref(), Some("123456"));
        assert_eq!(one_time_code("Use 7781 as your PIN, valid until 10:45").as_deref(), Some("7781"));
        assert_eq!(one_time_code("Running 10 min late, sorry!"), None, "no hint, no code");
        assert_eq!(one_time_code("Code for the gate? It costs $1500"), None, "money is not a code");
        assert_eq!(one_time_code("Call 0491570156 for your code"), None, "a phone number is not a code");
    }
}
