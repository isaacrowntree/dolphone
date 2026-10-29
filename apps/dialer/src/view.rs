//! The Dialer's screens, laid out: every position on the panel is decided here.

use flipctl_app::font;
use flipctl_app::paint::{Corners, Surface};
use flipctl_app::theme::color;
use phone_core::model::{CallState, Outcome, Route};
use phone_core::number::{self, Class};
use phone_core::when;

use crate::model::{Browse, Model, TodayItem, Top, CANNED, KEYS};

/// One string on the canvas. `font`: 0 title, 1 row, 2 bold row. `align`: 0
/// centred on x, -1 starting at x, 1 ending at x.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Text {
    pub x: i32,
    pub y: i32,
    pub text: String,
    pub font: i32,
    pub align: i32,
    pub white: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub label: String,
    pub tag: String,
    pub status: String,
    pub dim: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// DetailRow kinds: 0 label/value, 1 divider, 3 full-width text, 5 bold label.
    pub kind: i32,
    pub label: String,
    pub value: String,
}

pub enum Body {
    Canvas { surface: Surface, texts: Vec<Text> },
    Menu { items: Vec<Item>, selected: usize, scroll: usize, breadcrumb: String },
    Detail { rows: Vec<Row>, breadcrumb: String },
}

pub struct Modal {
    pub lines: Vec<String>,
    pub left: String,
    pub right: String,
}

pub struct View {
    pub body: Body,
    pub buttons: [String; 5],
    pub modal: Option<Modal>,
}

const W: i32 = 256;
const CX: i32 = W / 2;
/// Rows a menu shows at once.
const MENU_ROWS: usize = 5;

// Keypad geometry: four rows of three under the number and the match line, ending
// clear of the soft strip at y130.
const PAD_Y: i32 = 41;
const PAD_PITCH: i32 = 22;
const CELL_H: i32 = 21;
const CELL_W: i32 = 82;
const CELL_X0: i32 = 4;
const CELL_GAP: i32 = 2;

// The keypad ends clear of the soft strip at y130 and spans the panel to x254,
// checked when this compiles rather than when a test runs.
const _: () = assert!(PAD_Y + 3 * PAD_PITCH + CELL_H <= 130);
const _: () = assert!(CELL_X0 + 3 * CELL_W + 2 * CELL_GAP == 254);

fn t(x: i32, y: i32, text: impl Into<String>, font: i32, align: i32) -> Text {
    Text { x, y, text: text.into(), font, align, white: false }
}

fn buttons(b: [&str; 5]) -> [String; 5] {
    b.map(String::from)
}

/// Width in the face detail rows draw text in.
fn row_w(s: &str) -> i32 {
    i32::from(font::ROW.text_width(s))
}

/// Cut to `budget` in the bold face a selected menu row uses (the widest a label
/// is ever drawn), ending in ".." when it had to.
fn fit(s: &str, budget: i32) -> String {
    let s = font::ascii(s);
    let w = |t: &str| i32::from(font::ROW_ACTIVE.text_width(t));
    if w(&s) <= budget {
        return s;
    }
    let mut out = String::new();
    for c in s.chars() {
        out.push(c);
        if w(&format!("{out}..")) > budget {
            out.pop();
            break;
        }
    }
    format!("{}..", out.trim_end())
}

pub fn view(m: &Model, now: i64, frame: u32) -> View {
    let mut v = match m.top() {
        Top::Alert(a) => return alert(&a, m.alerts.position(), now),
        Top::Confirm(_) | Top::Browse => browse(m, now),
        Top::Incoming(c) => incoming(m, &c, frame),
        Top::InCall => in_call(m, now),
    };
    if let Top::Confirm(r) = m.top() {
        let who = fit(&r.requester, 200);
        let target = m.name_for(&r.target);
        let line2 = match r.kind.as_str() {
            "call" => format!("call {}", fit(&target, 170)),
            "sms" => format!("text {}", fit(&target, 170)),
            "wipe" => "delete all calls and texts".into(),
            "modem" => "set up the modem".into(),
            "grant" => fit(&format!("text {} unasked", r.target), 230),
            "contact" => fit(&r.target, 230),
            "setting" => fit(&format!("set {}", r.target), 230),
            other => other.to_string(),
        };
        let why = match r.reason.as_str() {
            "premium" => "Premium number: charges apply",
            "short" => "Short code: may cost money",
            "international" => "International number",
            "emergency" => "EMERGENCY number",
            "wipe" => "This cannot be undone",
            "modem" => "Saved in the modem; it restarts",
            "grant" => "",
            "contact" => "Contacts decide caller names",
            "setting" => "",
            _ => "Asked by another program",
        };
        // A setting shows the value asked for, which is the decision.
        let why = match r.reason.as_str() {
            "setting" => fit(&format!("to {}", r.body), 230),
            // How often and for how long is the decision, not a footnote.
            "grant" => fit(&phone_api::grant_terms(&r.body), 230),
            _ => why.to_string(),
        };
        let first = match r.kind.as_str() {
            "wipe" => "Delete everything?".to_string(),
            "contact" => format!("{who} wants to change contacts:"),
            _ => format!("{who} wants to"),
        };
        v.modal = Some(Modal { lines: vec![first, line2, why], left: "Deny".into(), right: "Allow".into() });
    } else if let Some(n) = &m.notice {
        v.modal = Some(Modal { lines: n.clone(), left: String::new(), right: "Ok".into() });
    }
    v
}

fn keypad(s: &mut Surface, texts: &mut Vec<Text>, sel: (usize, usize)) {
    for (r, row) in KEYS.iter().enumerate() {
        for (c, (digit, letters)) in row.iter().enumerate() {
            let x = CELL_X0 + c as i32 * (CELL_W + CELL_GAP);
            let y = PAD_Y + r as i32 * PAD_PITCH;
            let selected = sel == (r, c);
            if selected {
                s.selector_frame(x, y, CELL_W, CELL_H, 3, Corners::default(), Some(color::BLACK), color::BLACK);
            } else {
                s.round_frame(x, y, CELL_W, CELL_H, 3, color::DIVIDER);
            }
            let ty = y + 5;
            let mut d = t(x + 24, ty, digit.to_string(), 2, 0);
            d.white = selected;
            texts.push(d);
            if !letters.is_empty() {
                let mut l = t(x + 56, ty, *letters, 1, 0);
                l.white = selected;
                texts.push(l);
            }
        }
    }
}

fn browse(m: &Model, now: i64) -> View {
    match &m.browse {
        Browse::Keypad => {
            let mut s = Surface::panel();
            s.clear(color::WHITE);
            let mut texts = Vec::new();
            let typed = number::display(&m.number, &m.region);
            if m.number.is_empty() {
                let line = if !m.connected {
                    "Phone service not running".to_string()
                } else if !m.status.present && m.status.sip == "registered" {
                    "Calls over SIP (no modem)".to_string()
                } else if !m.status.present {
                    "No modem".to_string()
                } else if m.locked() {
                    "SIM locked: type its PIN, then Unlock".to_string()
                } else if m.status.state == "disabled" {
                    "Modem is off".to_string()
                } else if m.status.emergency_only && m.status.sip == "registered" {
                    "No mobile signal; calls over SIP".to_string()
                } else if m.status.emergency_only {
                    "Emergency calls only".to_string()
                } else if m.ussd_replying {
                    "Type your reply".to_string()
                } else {
                    let op = if m.status.operator.is_empty() { "Searching" } else { &m.status.operator };
                    let tech = if m.status.tech == "2G" { " - 2G" } else { "" };
                    format!("{op}{tech}{}", if m.status.roaming { " (roaming)" } else { "" })
                };
                // What is waiting goes beside the operator: "Fakecom AU - 1 missed".
                let line = match waiting_strip(m.missed, m.unread) {
                    Some(w) if m.connected && m.status.present && !m.locked() => format!("{line} - {w}"),
                    _ => line,
                };
                texts.push(t(CX, 18, fit(&line, 240), 1, 0));
            } else if m.locked() && m.classify_typed() != Class::Emergency {
                // A PIN is not shown, only how many digits have been typed.
                texts.push(t(CX, 16, "*".repeat(m.number.chars().count()), 2, 0));
            } else {
                // The end of a long number is the part being typed, so it is the end
                // that stays when it no longer fits.
                texts.push(t(
                    CX,
                    16,
                    tail_fit(if typed.len() > m.number.len() { &typed } else { &m.number }, 236),
                    2,
                    0,
                ));
            }
            let note = if m.number.is_empty() {
                String::new()
            } else if m.locked() && m.classify_typed() != Class::Emergency {
                "SIM PIN".into()
            } else if m.is_ussd() {
                "Service code".into()
            } else if let Some(h) = m.relay_hint() {
                h.into()
            } else {
                match m.classify_typed() {
                    Class::Emergency => "Emergency call".into(),
                    Class::Premium => "Premium number".into(),
                    _ => match (m.typed_match(), m.t9_match()) {
                        (Some(c), _) => c.name.clone(),
                        (None, Some(c)) => format!("Call {}?", c.name),
                        _ if m.status.roaming => "Roaming: charges may apply".into(),
                        _ => String::new(),
                    },
                }
            };
            if !note.is_empty() {
                texts.push(t(CX, 28, fit(&note, 240), 1, 0));
            }
            keypad(&mut s, &mut texts, m.sel);
            let run = if m.locked() && m.classify_typed() != Class::Emergency {
                if m.number.is_empty() {
                    ""
                } else {
                    "Unlock"
                }
            } else if m.ussd_replying {
                "Reply"
            } else if m.is_ussd() {
                "Send"
            } else if m.number.is_empty() {
                "Redial"
            } else {
                "Call"
            };
            let esc = if m.number.is_empty() { "Exit" } else { "Clear" };
            View {
                body: Body::Canvas { surface: s, texts },
                buttons: buttons([esc, "Recents", "", "Delete", run]),
                modal: None,
            }
        }
        Browse::Recents => {
            let items: Vec<Item> = m
                .recent_peers()
                .iter()
                .map(|r| {
                    let outcome = Outcome::from_u8(r.outcome);
                    let tag = match (outcome, r.direction) {
                        (Outcome::Missed, _) => "missed",
                        (Outcome::Declined, _) => "declined",
                        (Outcome::Blocked, _) => "blocked",
                        (Outcome::Silenced, _) => "silenced",
                        (Outcome::Cancelled | Outcome::Failed, _) => "no answer",
                        (Outcome::Answered, 0) => "in",
                        (Outcome::Answered, _) => "out",
                    };
                    Item {
                        label: fit(&m.name_for(&r.peer), 130),
                        tag: tag.into(),
                        status: when::short(r.started, now),
                        dim: false,
                    }
                })
                .collect();
            let empty = items.is_empty();
            let items = if empty {
                vec![Item { label: "No calls yet".into(), tag: String::new(), status: String::new(), dim: true }]
            } else {
                items
            };
            let (selected, scroll) = window(m.recents_sel, items.len());
            View {
                body: Body::Menu { items, selected, scroll, breadcrumb: "> Recents".into() },
                buttons: buttons([
                    "Back",
                    "Contacts",
                    "",
                    if empty { "" } else { "Text" },
                    if empty { "" } else { "Call" },
                ]),
                modal: None,
            }
        }
        Browse::Contacts => {
            let items: Vec<Item> = m
                .book
                .contacts
                .iter()
                .map(|c| Item {
                    label: fit(&c.name, 130),
                    tag: speed_key(m, c).map(|k| format!("[{k}]")).unwrap_or_default(),
                    status: c.numbers.first().map(|p| number::display(&p.number, &m.region)).unwrap_or_default(),
                    dim: true,
                })
                .collect();
            let empty = items.is_empty();
            let items = if empty {
                vec![Item {
                    label: "No contacts".into(),
                    tag: String::new(),
                    status: "add .vcf files".into(),
                    dim: true,
                }]
            } else {
                items
            };
            let (selected, scroll) = window(m.contacts_sel, items.len());
            View {
                body: Body::Menu { items, selected, scroll, breadcrumb: "> Contacts".into() },
                buttons: buttons([
                    "Back",
                    "Keypad",
                    "",
                    if empty { "" } else { "Text" },
                    if empty { "" } else { "Call" },
                ]),
                modal: None,
            }
        }
        Browse::Today(sel) => {
            let today = m.today();
            let mut items: Vec<Item> = today
                .iter()
                .map(|i| match i {
                    TodayItem::Missed { peer, count, at } => Item {
                        label: fit(&m.name_for(peer), 120),
                        tag: if *count > 1 { format!("{count} missed") } else { "missed".into() },
                        status: when::short(*at, now),
                        dim: false,
                    },
                    TodayItem::Unread { peer, count, at } => Item {
                        label: fit(&m.name_for(peer), 120),
                        tag: format!("{count} new"),
                        status: when::short(*at, now),
                        dim: false,
                    },
                    TodayItem::Favourite { key, number } => Item {
                        label: fit(&m.name_for(number), 120),
                        tag: format!("[{key}]"),
                        status: String::new(),
                        dim: true,
                    },
                })
                .collect();
            let empty = items.is_empty();
            if empty {
                items.push(Item {
                    label: "Nothing waiting".into(),
                    tag: String::new(),
                    status: String::new(),
                    dim: true,
                });
            }
            let (selected, scroll) = window(*sel, items.len());
            let run = match today.get(*sel) {
                Some(TodayItem::Unread { .. }) => "Open",
                Some(_) => "Call",
                None => "",
            };
            View {
                body: Body::Menu { items, selected, scroll, breadcrumb: "> Today".into() },
                buttons: buttons(["Keypad", "Keypad", "", if empty { "" } else { "Text" }, run]),
                modal: None,
            }
        }
        Browse::SpeedPick(sel) => {
            let who = m.book.contacts.get(m.contacts_sel).map(|c| c.name.clone()).unwrap_or_default();
            let items: Vec<Item> = (0..8)
                .map(|i| {
                    let on = &m.speed[i];
                    Item {
                        label: format!("Hold {}", i + 2),
                        tag: String::new(),
                        status: if on.is_empty() { "free".into() } else { fit(&m.name_for(on), 110) },
                        dim: on.is_empty(),
                    }
                })
                .collect();
            let (selected, scroll) = window(*sel, items.len());
            let clear = if m.speed[*sel].is_empty() { "" } else { "Clear" };
            View {
                body: Body::Menu { items, selected, scroll, breadcrumb: format!("> Speed dial: {}", fit(&who, 120)) },
                buttons: buttons(["Back", "", "", clear, "Set"]),
                modal: None,
            }
        }
        Browse::Ussd { text, awaiting } => {
            let rows = wrap(text, 240).into_iter().map(|l| Row { kind: 3, label: l, value: String::new() }).collect();
            View {
                body: Body::Detail { rows, breadcrumb: "> Service code".into() },
                buttons: buttons(["Close", "", "", if *awaiting { "Reply" } else { "" }, ""]),
                modal: None,
            }
        }
    }
}

fn incoming(m: &Model, c: &phone_api::CallInfo, frame: u32) -> View {
    let mut s = Surface::panel();
    s.clear(color::WHITE);
    let mut texts = Vec::new();
    if let Some(sel) = m.replies {
        texts.push(t(CX, 17, "Reply with a text", 0, 0));
        for (i, r) in CANNED.iter().enumerate() {
            let y = 32 + i as i32 * 23;
            let selected = i == sel;
            if selected {
                s.selector_frame(4, y, 248, 21, 3, Corners::default(), Some(color::BLACK), color::BLACK);
            }
            let mut line = t(10, y + 5, fit(r, 236), if selected { 2 } else { 1 }, -1);
            line.white = selected;
            texts.push(line);
        }
        return View {
            body: Body::Canvas { surface: s, texts },
            buttons: buttons(["Back", "", "", "", "Send"]),
            modal: None,
        };
    }
    let waiting = CallState::from_u8(c.state) == CallState::Waiting;
    texts.push(t(CX, 18, if waiting { "Call waiting" } else { "Incoming call" }, 0, 0));
    let name = m.name_for(&c.number);
    // The ring: a frame round the caller that breathes, two frames on, one off.
    if frame % 3 != 2 {
        s.round_frame(14, 38, 228, 44, 4, color::BLACK);
    }
    if frame.is_multiple_of(3) {
        s.round_frame(10, 34, 236, 52, 4, color::DIVIDER);
    }
    texts.push(t(CX, 46, fit(&name, 210), 2, 0));
    let second = if c.number.is_empty() {
        "Number withheld".to_string()
    } else if m.info {
        let class = match number::classify(&c.number, &m.region) {
            Class::International => "international",
            Class::Short => "short code",
            Class::Premium => "premium",
            _ => "mobile or landline",
        };
        format!("{} ({class})", number::display(&c.number, &m.region))
    } else if name != number::display(&c.number, &m.region) {
        number::display(&c.number, &m.region)
    } else {
        String::new()
    };
    if !second.is_empty() {
        texts.push(t(CX, 62, fit(&second, 210), 1, 0));
    }
    if waiting {
        if let Some(cur) = m.current() {
            texts.push(t(CX, 96, fit(&format!("On a call with {}", m.name_for(&cur.number)), 240), 1, 0));
            texts.push(t(CX, 108, "Answer holds it", 1, 0));
        }
    } else {
        texts.push(t(CX, 100, m.hint.unwrap_or("PTT to silence"), 1, 0));
    }
    let reply = if c.number.is_empty() { "" } else { "Reply" };
    View {
        body: Body::Canvas { surface: s, texts },
        buttons: buttons([if m.steady_decline { "Hold:Decl" } else { "Decline" }, "Info", "", reply, "Answer"]),
        modal: None,
    }
}

fn in_call(m: &Model, now: i64) -> View {
    let mut s = Surface::panel();
    s.clear(color::WHITE);
    let mut texts = Vec::new();
    let Some(c) = m.current() else {
        return View { body: Body::Canvas { surface: s, texts }, buttons: buttons(["", "", "", "", ""]), modal: None };
    };
    let state = CallState::from_u8(c.state);
    let route_name = m
        .routes
        .iter()
        .find(|r| r.route == m.route as u8)
        .map(|r| r.name.clone())
        .unwrap_or_else(|| m.route.label().to_string());
    let route_key = match m.route {
        Route::Speaker => "Speaker",
        Route::Wired => "Headset",
        Route::Bluetooth => "BT",
        Route::Earpiece => "Ear",
    };
    if m.dtmf_open {
        texts.push(t(CX, 16, if m.dtmf.is_empty() { "Keypad".to_string() } else { tail_fit(&m.dtmf, 236) }, 2, 0));
        texts.push(t(CX, 28, fit(&format!("{} - {}", m.name_for(&c.number), timer(c, now)), 240), 1, 0));
        keypad(&mut s, &mut texts, m.dtmf_sel);
        return View {
            body: Body::Canvas { surface: s, texts },
            buttons: buttons(["Close", "", "", route_key, "End"]),
            modal: None,
        };
    }
    let title = if c.emergency {
        format!("Emergency {}", timer(c, now))
    } else {
        match state {
            CallState::Dialing if c.internet => "Calling over SIP...".to_string(),
            CallState::Dialing => "Calling...".to_string(),
            CallState::RingingOut => "Ringing...".to_string(),
            CallState::Held => "On hold".to_string(),
            _ => timer(c, now),
        }
    };
    texts.push(t(CX, 18, title, 0, 0));
    let name = m.name_for(&c.number);
    texts.push(t(CX, 40, fit(&name, 230), 2, 0));
    let shown = number::display(&c.number, &m.region);
    if name != shown && !c.number.is_empty() {
        texts.push(t(CX, 54, fit(&shown, 230), 1, 0));
    }
    if let Some(o) = m.other() {
        s.round_frame(4, 70, 248, 17, 3, color::DIVIDER);
        texts.push(t(CX, 73, fit(&format!("Held: {} - Ok swap, hold Ok merge", m.name_for(&o.number)), 240), 1, 0));
    }
    let audio = if c.muted { format!("{route_name} - muted") } else { route_name };
    let audio = if c.internet { format!("SIP - {audio}") } else { audio };
    texts.push(t(CX, 96, fit(&audio, 240), 1, 0));
    texts.push(t(CX, 108, if c.muted { "PTT to unmute" } else { "PTT to mute" }, 1, 0));
    View {
        body: Body::Canvas { surface: s, texts },
        buttons: buttons(["", "Keypad", "", route_key, "End"]),
        modal: None,
    }
}

/// An emergency alert, full screen: the level as the title, the whole text.
/// `at` is which of how many are waiting: more than one shows "1 of 3", and
/// Run says Next until the last.
fn alert(a: &phone_api::AlertInfo, at: (usize, usize), now: i64) -> View {
    let level = phone_core::model::AlertLevel::from_u8(a.level);
    let mut rows = vec![
        Row { kind: 5, label: level.label().to_uppercase(), value: when::short(a.at, now) },
        Row { kind: 1, label: String::new(), value: String::new() },
    ];
    rows.extend(wrap(&a.text, 240).into_iter().map(|l| Row { kind: 3, label: l, value: String::new() }));
    View {
        body: Body::Detail {
            rows,
            breadcrumb: match at {
                (_, 0 | 1) => "> Emergency alert".into(),
                (n, of) => format!("> Emergency alert {n} of {of}"),
            },
        },
        buttons: buttons(["", "", "", "", if at.0 < at.1 { "Next" } else { "Dismiss" }]),
        modal: None,
    }
}

/// Which speed-dial key a contact is on, if any.
fn speed_key(m: &Model, c: &phone_core::contacts::Contact) -> Option<usize> {
    m.speed
        .iter()
        .position(|s| !s.is_empty() && c.numbers.iter().any(|p| number::same_party(&p.number, s)))
        .map(|i| i + 2)
}

/// "1 missed - 2 texts", or nothing when nothing is waiting.
pub fn waiting_strip(missed: u32, unread: u32) -> Option<String> {
    let plural = |n: u32, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    match (missed, unread) {
        (0, 0) => None,
        (m, 0) => Some(plural(m, "missed call", "missed calls")),
        (0, u) => Some(plural(u, "new text", "new texts")),
        (m, u) => Some(format!("{m} missed - {}", plural(u, "text", "texts"))),
    }
}

fn timer(c: &phone_api::CallInfo, now: i64) -> String {
    if c.connected_at > 0 {
        when::duration(now - c.connected_at)
    } else {
        String::new()
    }
}

/// The selected row and the first visible row for a five-row menu.
pub fn window(sel: usize, total: usize) -> (usize, usize) {
    let sel = sel.min(total.saturating_sub(1));
    let scroll = if sel < MENU_ROWS { 0 } else { sel + 1 - MENU_ROWS };
    (sel, scroll)
}

/// The end of `s` that fits in `budget` in the bold face the number is drawn in,
/// with ".." in front when cut.
pub fn tail_fit(s: &str, budget: i32) -> String {
    let w = |t: &str| i32::from(font::ROW_ACTIVE.text_width(t));
    if w(s) <= budget {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    for start in 1..chars.len() {
        let tail: String = chars[start..].iter().collect();
        let candidate = format!("..{tail}");
        if w(&candidate) <= budget {
            return candidate;
        }
    }
    String::new()
}

/// Wrap text to lines no wider than `budget`, in the face detail rows use.
pub fn wrap(text: &str, budget: i32) -> Vec<String> {
    let mut out = Vec::new();
    for para in font::ascii(text).lines() {
        let mut line = String::new();
        for word in para.split_whitespace() {
            let candidate = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
            if row_w(&candidate) <= budget || line.is_empty() {
                line = candidate;
            } else {
                out.push(std::mem::take(&mut line));
                line = word.to_string();
            }
        }
        out.push(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_window_keeps_selection_visible() {
        assert_eq!(window(0, 10), (0, 0));
        assert_eq!(window(4, 10), (4, 0));
        assert_eq!(window(5, 10), (5, 1));
        assert_eq!(window(20, 10), (9, 5));
    }

    #[test]
    fn tail_fit_keeps_the_end() {
        let long = "0412345678901234567890123456789012345";
        let fitted = tail_fit(long, 100);
        assert!(fitted.starts_with(".."));
        assert!(long.ends_with(&fitted[2..]));
        assert!(i32::from(font::ROW_ACTIVE.text_width(&fitted)) <= 100);
    }

    #[test]
    fn wraps_words() {
        let lines = wrap("Your balance is $12.34. Expires 30/11. Reply 1 for more options or 2 to recharge now.", 120);
        assert!(lines.len() > 2);
        assert!(lines.iter().all(|l| row_w(l) <= 120 || !l.contains(' ')));
    }
}
