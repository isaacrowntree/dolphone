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
/// isolates (which can make a link or a number read backwards), and cap the length.
pub fn clean(input: &str, max: usize) -> String {
    input.chars().filter(|&c| !is_hostile(c)).map(|c| if c == '\t' || c == '\r' { ' ' } else { c }).take(max).collect()
}

fn is_hostile(c: char) -> bool {
    match c {
        '\n' | '\t' | '\r' => false,
        // C0, DEL and C1 controls.
        '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}' => true,
        // Bidi embedding, override and isolate controls, and the marks.
        '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' => true,
        // Zero-width characters used to hide text inside text.
        '\u{200b}'..='\u{200d}' | '\u{2060}' | '\u{feff}' => true,
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
    let gsm_units: Option<usize> = body.chars().try_fold(0usize, |n, c| {
        if GSM7_BASIC.contains(c) {
            Some(n + 1)
        } else if GSM7_EXTENSION.contains(c) {
            Some(n + 2)
        } else {
            None
        }
    });
    let (units, single, multi, gsm7) = match gsm_units {
        Some(u) => (u, 160, 153, true),
        None => (body.encode_utf16().count(), 70, 67, false),
    };
    if units <= single {
        return Segments { gsm7, count: 1, remaining: single - units };
    }
    let count = units.div_ceil(multi);
    Segments { gsm7, count, remaining: count * multi - units }
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
        assert_eq!(clean("long", 2), "lo");
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
}
