//! Messages' screens, laid out.

use flipctl_app::font;
use phone_core::model::MessageState;
use phone_core::{number, text, when};

use crate::model::{Model, Opt, Screen, QUICK};
use crate::pictures;

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
    pub dim: bool,
}

pub enum Body {
    Menu { items: Vec<Item>, selected: usize, scroll: usize, breadcrumb: String },
    Detail { rows: Vec<Row>, offset: usize, breadcrumb: String },
}

pub struct View {
    pub body: Body,
    pub buttons: [String; 5],
    pub modal: Option<(Vec<String>, String, String)>,
}

/// Detail rows visible under a breadcrumb.
pub const VISIBLE: usize = 8;
const MENU_ROWS: usize = 5;
/// Text width inside a detail screen, clear of the scrollbar.
const TEXT_W: i32 = 236;

fn b(x: [&str; 5]) -> [String; 5] {
    x.map(String::from)
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

pub fn window(sel: usize, total: usize) -> (usize, usize) {
    let sel = sel.min(total.saturating_sub(1));
    (sel, if sel < MENU_ROWS { 0 } else { sel + 1 - MENU_ROWS })
}

/// The conversation as detail rows: a bold line naming the sender and the time,
/// the text wrapped under it, and a rule between messages.
pub fn conversation_rows(m: &Model, now: i64) -> Vec<Row> {
    let them = m.name_for(&m.peer);
    let mut rows = Vec::new();
    for (i, msg) in m.messages.iter().enumerate() {
        if i > 0 {
            rows.push(Row { kind: 1, label: String::new(), value: String::new(), dim: false });
        }
        let who = if msg.direction == 0 { fit(&them, 150) } else { "You".into() };
        let state = match MessageState::from_u8(msg.state) {
            MessageState::Sending => " sending",
            MessageState::Failed => " FAILED",
            MessageState::Delivered => " delivered",
            _ => "",
        };
        rows.push(Row { kind: 5, label: who, value: format!("{}{state}", when::short(msg.at, now)), dim: false });
        for line in wrap(&msg.body, TEXT_W) {
            rows.push(Row { kind: 3, label: line, value: String::new(), dim: false });
        }
        // The panel's screen is too small for pictures: name what came.
        for (mime, _) in &msg.parts {
            let kind = phone_core::model::kind_of(mime);
            rows.push(Row { kind: 3, label: format!("[{kind}]"), value: String::new(), dim: true });
        }
    }
    if rows.is_empty() {
        rows.push(Row { kind: 3, label: "No messages yet. Edit to write.".into(), value: String::new(), dim: true });
    }
    rows
}

pub fn view(m: &Model, now: i64) -> View {
    let modal = m.notice.as_ref().map(|lines| {
        if m.confirm_delete {
            (lines.clone(), "Keep".to_string(), "Delete".to_string())
        } else {
            (lines.clone(), String::new(), "Ok".to_string())
        }
    });
    let (body, buttons) = match m.screen {
        Screen::Threads => {
            let mut items: Vec<Item> = m
                .threads
                .iter()
                .map(|t| {
                    let preview = t.last_body.lines().next().unwrap_or("");
                    let lead = if t.last_direction == 1 { "You: " } else { "" };
                    Item {
                        label: fit(&m.name_for(&t.peer), 110),
                        tag: if t.unread > 0 {
                            format!("{} new", t.unread)
                        } else {
                            fit(&format!("{lead}{preview}"), 70)
                        },
                        status: when::short(t.last_at, now),
                        dim: t.unread == 0,
                    }
                })
                .collect();
            let empty = items.is_empty();
            if empty {
                items.push(Item {
                    label: if m.connected { "No messages".into() } else { "Phone service not running".into() },
                    tag: String::new(),
                    status: String::new(),
                    dim: true,
                });
            }
            let (selected, scroll) = window(m.thread_sel, items.len());
            // What else is waiting, as in every Dolphone app.
            let crumb = match m.missed {
                0 => String::new(),
                1 => "> 1 missed call".into(),
                n => format!("> {n} missed calls"),
            };
            (
                Body::Menu { items, selected, scroll, breadcrumb: crumb },
                b(["Exit", "Search", "", "New", if empty { "" } else { "Open" }]),
            )
        }
        Screen::Conversation => {
            let rows = conversation_rows(m, now);
            let last = rows.len().saturating_sub(VISIBLE);
            let offset = m.scroll.unwrap_or(last).min(last);
            let crumb = format!("> {}", fit(&m.name_for(&m.peer), 200));
            (Body::Detail { rows, offset, breadcrumb: crumb }, b(["Back", "Options", "", "Write", "Call"]))
        }
        Screen::Options => {
            let items = m
                .options()
                .into_iter()
                .map(|o| Item {
                    label: match o {
                        Opt::QuickReply => "Quick reply",
                        Opt::Attach => "Attach a picture",
                        Opt::Resend => "Resend failed text",
                        Opt::Call => "Call",
                        Opt::Block if m.is_blocked() => "Unblock number",
                        Opt::Block => "Block number",
                        Opt::Delete => "Delete conversation",
                    }
                    .to_string(),
                    tag: String::new(),
                    status: String::new(),
                    dim: false,
                })
                .collect::<Vec<_>>();
            let (selected, scroll) = window(m.option_sel, items.len());
            (
                Body::Menu { items, selected, scroll, breadcrumb: format!("> {}", fit(&m.name_for(&m.peer), 200)) },
                b(["Back", "", "", "", "Select"]),
            )
        }
        Screen::Found => {
            let mut items: Vec<Item> = m
                .found
                .iter()
                .map(|f| Item {
                    label: fit(&m.name_for(&f.peer), 100),
                    tag: fit(f.body.lines().next().unwrap_or(""), 90),
                    status: when::short(f.at, now),
                    dim: false,
                })
                .collect();
            let empty = items.is_empty();
            if empty {
                items.push(Item {
                    label: "Nothing found".into(),
                    tag: String::new(),
                    status: String::new(),
                    dim: true,
                });
            }
            let (selected, scroll) = window(m.found_sel, items.len());
            (
                Body::Menu { items, selected, scroll, breadcrumb: format!("> \"{}\"", fit(&m.query, 180)) },
                b(["Back", "Search", "", "", if empty { "" } else { "Open" }]),
            )
        }
        Screen::QuickReply => {
            let items = QUICK
                .iter()
                .map(|q| Item { label: fit(q, 230), tag: String::new(), status: String::new(), dim: false })
                .collect::<Vec<_>>();
            let (selected, scroll) = window(m.quick_sel, items.len());
            (
                Body::Menu { items, selected, scroll, breadcrumb: "> Quick reply".into() },
                b(["Back", "", "", "Edit", "Use"]),
            )
        }
        Screen::Attach => {
            let mut items: Vec<Item> = m
                .pictures
                .iter()
                .map(|p| {
                    let on = m.attached.iter().any(|a| a.path == p.path);
                    Item {
                        label: fit(&p.name, 120),
                        tag: if on { "Added".into() } else { pictures::size(p.size) },
                        status: when::short(p.at, now),
                        dim: !on,
                    }
                })
                .collect();
            let empty = items.is_empty();
            if empty {
                items.push(Item {
                    label: "No pictures to attach".into(),
                    tag: String::new(),
                    status: String::new(),
                    dim: true,
                });
            }
            let run = match m.pictures.get(m.picture_sel) {
                Some(p) if m.attached.iter().any(|a| a.path == p.path) => "Remove",
                Some(_) => "Add",
                None => "",
            };
            let (selected, scroll) = window(m.picture_sel, items.len());
            let crumb = match m.attached.len() {
                0 => "> Attach a picture".to_string(),
                n => format!("> Attach a picture ({n} added)"),
            };
            (Body::Menu { items, selected, scroll, breadcrumb: crumb }, b(["Back", "", "", "", run]))
        }
        Screen::Locked => {
            let rows = vec![
                Row { kind: 5, label: "Messages is locked".into(), value: String::new(), dim: false },
                Row { kind: 1, label: String::new(), value: String::new(), dim: false },
                Row { kind: 3, label: "Unlock and type the PIN set".into(), value: String::new(), dim: false },
                Row { kind: 3, label: "in Settings > Privacy.".into(), value: String::new(), dim: false },
            ];
            (Body::Detail { rows, offset: 0, breadcrumb: "> Locked".into() }, b(["Exit", "", "", "", "Unlock"]))
        }
        Screen::Preview => {
            let seg = text::segments(&m.draft);
            let mut rows = vec![Row { kind: 0, label: "To".into(), value: fit(&m.name_for(&m.peer), 160), dim: false }];
            if m.name_for(&m.peer) != number::display_peer(&m.peer, &m.region) {
                rows.push(Row {
                    kind: 0,
                    label: String::new(),
                    value: number::display_peer(&m.peer, &m.region),
                    dim: true,
                });
            }
            rows.push(Row { kind: 1, label: String::new(), value: String::new(), dim: false });
            // What goes with the text, above it, as the desktop shows it. No rule
            // between them, so the cost stays on the screen.
            // Two by name, and the rest counted, so the text stays on the screen.
            let named = if m.attached.len() > 3 { 2 } else { m.attached.len() };
            for (i, p) in m.attached.iter().take(named).enumerate() {
                let label = if i == 0 { "Attached" } else { "" };
                rows.push(Row { kind: 0, label: label.into(), value: fit(&p.name, 150), dim: false });
            }
            if m.attached.len() > named {
                let more = format!("and {} more", m.attached.len() - named);
                rows.push(Row { kind: 0, label: String::new(), value: more, dim: true });
            }
            if !m.draft.trim().is_empty() {
                for line in wrap(&m.draft, TEXT_W) {
                    rows.push(Row { kind: 3, label: line, value: String::new(), dim: false });
                }
            }
            rows.push(Row { kind: 1, label: String::new(), value: String::new(), dim: false });
            let note = if !m.attached.is_empty() {
                "1 picture message (MMS)".to_string()
            } else {
                let cost = if seg.count == 1 { "1 text".to_string() } else { format!("{} texts", seg.count) };
                if seg.gsm7 {
                    format!("{cost}, {} left", seg.remaining)
                } else {
                    format!("{cost} (emoji or accents: 70 per text)")
                }
            };
            rows.push(Row { kind: 0, label: "Cost".into(), value: note, dim: true });
            let view = if m.attached.is_empty() { "Attach" } else { "Pictures" };
            (Body::Detail { rows, offset: 0, breadcrumb: "> Send?".into() }, b(["Edit", view, "", "", "Send"]))
        }
    };
    View { body, buttons, modal }
}

/// Wrap text to lines no wider than `budget`, breaking long words.
pub fn wrap(text: &str, budget: i32) -> Vec<String> {
    let mut out = Vec::new();
    for para in font::ascii(text).lines() {
        let mut line = String::new();
        for word in para.split_whitespace() {
            let candidate = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
            if row_w(&candidate) <= budget {
                line = candidate;
                continue;
            }
            if !line.is_empty() {
                out.push(std::mem::take(&mut line));
            }
            // A word wider than the line (a URL) is broken where it must be.
            let mut piece = String::new();
            for ch in word.chars() {
                piece.push(ch);
                if row_w(&piece) > budget {
                    piece.pop();
                    out.push(std::mem::take(&mut piece));
                    piece.push(ch);
                }
            }
            line = piece;
        }
        out.push(line);
    }
    while out.last().is_some_and(|l| l.is_empty()) && out.len() > 1 {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pictures::Picture;

    #[test]
    fn attached_pictures_leave_the_cost_on_screen() {
        let mut m = Model::new("AU", phone_core::contacts::Book::default());
        m.open("0491570156");
        let pic = |n: &str| Picture { path: format!("/p/{n}.jpg"), name: n.into(), size: 1, at: 1 };
        m.pictures = vec![pic("Screenshot from 2026-10-29 09-14-02 at the very long name"), pic("Beach")];
        m.screen = Screen::Attach;
        let Body::Menu { items, .. } = view(&m, 100).body else { panic!() };
        assert!(
            items.iter().all(|i| i32::from(font::ROW_ACTIVE.text_width(&i.label)) <= 120),
            "names fit beside the size"
        );
        m.attached = m.pictures.clone();
        m.draft = "Here's the beach.".into();
        m.screen = Screen::Preview;
        let v = view(&m, 100);
        let Body::Detail { rows, .. } = v.body else { panic!() };
        assert!(rows.len() <= VISIBLE, "{rows:?}");
        assert!(rows.iter().any(|r| r.label == "Attached"));
        assert_eq!(rows.last().unwrap().value, "1 picture message (MMS)");
        assert_eq!(v.buttons[1], "Pictures");
        m.attached = (0..6).map(|i| pic(&format!("p{i}"))).collect();
        let Body::Detail { rows, .. } = view(&m, 100).body else { panic!() };
        assert!(rows.iter().any(|r| r.value == "and 4 more"));
        assert!(rows.len() <= VISIBLE);
    }

    #[test]
    fn wrap_breaks_long_words() {
        let lines = wrap("see https://example.com/a/really/long/path/that/does/not/fit/on/one/line ok", 100);
        assert!(lines.len() >= 3);
        assert!(lines.iter().all(|l| row_w(l) <= 100));
    }
}
