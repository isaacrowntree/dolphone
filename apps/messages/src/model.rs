//! What Messages shows and what its keys do, with no UI and no bus in it.
//!
//! Keys keep FlipCTL's roles: Escape backs out one level, View shows options or
//! details, Edit writes, Run does the main thing (open, send, call).

use phone_api::{MessageInfo, ThreadInfo};
use phone_core::contacts::Book;
use phone_core::model::MessageState;
use phone_core::{number, text};

use crate::pictures::{self, Picture};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    Up,
    Down,
    Left,
    Right,
    Ok,
    Escape,
    View,
    Edit,
    Run,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Screen {
    Threads,
    Conversation,
    Options,
    /// Ready-made replies, for answering without the keyboard.
    QuickReply,
    /// Texts found by a search.
    Found,
    /// The text, ready to go, with its cost shown.
    Preview,
    /// Pictures to attach, from the Pictures and Downloads folders.
    Attach,
    /// Messages wants its PIN: nothing of the texts is shown.
    Locked,
}

/// What the keyboard is for when it is up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Writing {
    /// The number for a new conversation.
    To,
    /// The message itself.
    Body,
    /// Words to search for.
    Search,
    /// The Messages PIN.
    Pin,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    LoadThreads,
    Load(String),
    MarkRead(String),
    Send(String, String),
    /// A picture message: to everyone on the thread, the text, the files.
    SendMms(Vec<String>, String, Vec<String>),
    /// Look for pictures to attach.
    Pictures,
    /// Open the keyboard for this, starting from this text.
    Write(Writing, String),
    Call(String),
    Block(String),
    Unblock(String),
    Delete(String),
    /// Send a failed text again.
    Retry(i64),
    Search(String),
    /// Try the Messages PIN.
    Unlock(String),
    Quit,
}

/// What a conversation's Options offer. Resend is there only when a text in it
/// failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opt {
    QuickReply,
    Attach,
    Resend,
    Call,
    Block,
    Delete,
}

/// Replies that need no typing on a five-key panel.
pub const QUICK: [&str; 8] = [
    "OK",
    "Yes",
    "No",
    "On my way.",
    "Running late, sorry!",
    "Can't talk now, I'll call you back.",
    "Thanks!",
    "Call me when you can.",
];

pub struct Model {
    pub region: String,
    pub book: Book,
    pub screen: Screen,
    pub threads: Vec<ThreadInfo>,
    pub thread_sel: usize,
    /// The conversation's peer, canonical.
    pub peer: String,
    pub messages: Vec<MessageInfo>,
    /// First visible row of the conversation; None follows the newest message.
    pub scroll: Option<usize>,
    pub option_sel: usize,
    pub draft: String,
    pub blocked: Vec<String>,
    /// A modal: lines, and whether Run confirms a pending deletion.
    pub notice: Option<Vec<String>>,
    pub confirm_delete: bool,
    pub connected: bool,
    pub quick_sel: usize,
    pub missed: u32,
    /// The last search and what it found.
    pub query: String,
    pub found: Vec<MessageInfo>,
    pub found_sel: usize,
    /// Messages wants its PIN, as phoned last said.
    pub locked: bool,
    /// A number the Dialer asked to text while Messages was locked, to open once
    /// the PIN is in.
    pub waiting_peer: Option<String>,
    /// Pictures that could be attached, newest first, and the ones that are.
    pub pictures: Vec<Picture>,
    pub picture_sel: usize,
    pub attached: Vec<Picture>,
}

impl Model {
    pub fn new(region: &str, book: Book) -> Self {
        Self {
            region: region.into(),
            book,
            screen: Screen::Threads,
            threads: Vec::new(),
            thread_sel: 0,
            peer: String::new(),
            messages: Vec::new(),
            scroll: None,
            option_sel: 0,
            draft: String::new(),
            blocked: Vec::new(),
            notice: None,
            confirm_delete: false,
            connected: false,
            quick_sel: 0,
            missed: 0,
            query: String::new(),
            found: Vec::new(),
            found_sel: 0,
            locked: false,
            waiting_peer: None,
            pictures: Vec::new(),
            picture_sel: 0,
            attached: Vec::new(),
        }
    }

    pub fn name_for(&self, n: &str) -> String {
        // A name the panel cannot draw (Chinese, Arabic, emoji) shows as the number.
        let name = self.book.name_for(n, &self.region);
        let shown = flipctl_app::font::ascii(&name);
        let unknown = shown.chars().filter(|c| *c == '?').count();
        if unknown * 3 >= shown.chars().count().max(1) {
            number::display(n, &self.region)
        } else {
            name
        }
    }

    /// The newest failed text in the open conversation.
    pub fn failed(&self) -> Option<&MessageInfo> {
        self.messages.iter().rev().find(|m| m.direction == 1 && MessageState::from_u8(m.state) == MessageState::Failed)
    }

    pub fn options(&self) -> Vec<Opt> {
        let mut o = vec![Opt::QuickReply, Opt::Attach];
        if self.failed().is_some() {
            o.insert(0, Opt::Resend);
        }
        o.extend([Opt::Call, Opt::Block, Opt::Delete]);
        o
    }

    pub fn is_blocked(&self) -> bool {
        self.blocked.iter().any(|b| number::same_party(b, &self.peer))
    }

    /// Open a conversation, from the list or from a handoff.
    pub fn open(&mut self, peer: &str) -> Vec<Action> {
        // A group stays a group: canonical() alone would keep only its first number.
        self.peer = number::canonical_peer(peer, &self.region);
        self.messages.clear();
        self.attached.clear();
        self.scroll = None;
        self.screen = Screen::Conversation;
        vec![Action::Load(self.peer.clone()), Action::MarkRead(self.peer.clone())]
    }

    /// The Dialer (or the desktop) asked to text this number.
    pub fn compose_to(&mut self, peer: &str) -> Vec<Action> {
        if self.locked {
            // Kept for after the PIN; the keyboard is the PIN's until then.
            self.waiting_peer = Some(peer.to_string());
            return Vec::new();
        }
        let mut a = self.open(peer);
        a.push(Action::Write(Writing::Body, String::new()));
        a
    }

    /// The texts' lock: phoned said Messages is (or is no longer) locked, or a
    /// load was refused because it is. Locking forgets what was on screen, so
    /// nothing of it shows behind the PIN; unlocking loads the list again.
    pub fn set_locked(&mut self, locked: bool) -> Vec<Action> {
        if locked == self.locked {
            return Vec::new();
        }
        self.locked = locked;
        self.notice = None;
        self.confirm_delete = false;
        if locked {
            self.screen = Screen::Locked;
            self.threads.clear();
            self.messages.clear();
            self.found.clear();
            self.draft.clear();
            self.peer.clear();
            self.pictures.clear();
            self.attached.clear();
            return vec![Action::Write(Writing::Pin, String::new())];
        }
        self.screen = Screen::Threads;
        let mut a = vec![Action::LoadThreads];
        if let Some(peer) = self.waiting_peer.take() {
            a.extend(self.compose_to(&peer));
        }
        a
    }

    /// What the PIN keyboard lets through: 4 to 12 digits, as phoned takes.
    pub fn valid_pin(t: &str) -> bool {
        (4..=12).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_digit())
    }

    /// Whether phoned's error means only that Messages is locked.
    pub fn is_lock_error(text: &str) -> bool {
        text.starts_with("Messages is locked")
    }

    // Events.

    pub fn set_pictures(&mut self, p: Vec<Picture>) {
        self.pictures = p;
        self.picture_sel = self.picture_sel.min(self.pictures.len().saturating_sub(1));
    }

    pub fn set_threads(&mut self, t: Vec<ThreadInfo>) {
        self.threads = t;
        self.thread_sel = self.thread_sel.min(self.threads.len().saturating_sub(1));
    }

    pub fn set_conversation(&mut self, peer: &str, m: Vec<MessageInfo>) {
        if peer == self.peer {
            self.messages = m;
        }
    }

    /// A new message from phoned. Returns what to do about it (reload the list;
    /// mark read when it lands in the open conversation).
    pub fn message_added(&mut self, m: MessageInfo) -> Vec<Action> {
        if self.locked {
            return Vec::new();
        }
        let mut a = vec![Action::LoadThreads];
        if number::same_party(&m.peer, &self.peer)
            && matches!(self.screen, Screen::Conversation | Screen::Preview | Screen::Options | Screen::Attach)
        {
            // The signal carries no text (it goes to everything on the bus), so the
            // conversation is fetched again, which only readers may.
            a.push(Action::Load(self.peer.clone()));
            self.scroll = None;
            if m.direction == 0 {
                a.push(Action::MarkRead(self.peer.clone()));
            }
        }
        a
    }

    pub fn message_changed(&mut self, id: i64, state: u8) {
        if let Some(m) = self.messages.iter_mut().find(|m| m.id == id) {
            m.state = state;
        }
        if MessageState::from_u8(state) == MessageState::Failed {
            self.notice = Some(vec!["Not sent.".into(), "Options > Resend to try again.".into()]);
        }
    }

    /// The keyboard finished.
    pub fn written(&mut self, what: Writing, text: Option<String>) -> Vec<Action> {
        if what == Writing::Pin {
            // Leaving the PIN leaves Messages: there is nothing else to show.
            return match text {
                Some(pin) if !pin.is_empty() => vec![Action::Unlock(pin)],
                Some(_) => Vec::new(),
                None => vec![Action::Quit],
            };
        }
        let Some(text) = text else {
            // Cancelled: back to where the keyboard was opened from.
            if what == Writing::To || (what == Writing::Search && self.screen != Screen::Found) {
                self.screen = Screen::Threads;
            }
            return Vec::new();
        };
        match what {
            Writing::To => {
                // Digits are a number; letters are a contact's name.
                let peer = if number::digits(&text).len() >= 3 {
                    number::canonical(&text, &self.region)
                } else {
                    match self.book.search(text.trim()).first().and_then(|c| c.numbers.first()) {
                        Some(p) => p.number.clone(),
                        None => {
                            self.notice =
                                Some(vec!["No number or contact".into(), format!("called \"{}\".", text.trim())]);
                            return Vec::new();
                        }
                    }
                };
                self.compose_to(&peer)
            }
            Writing::Search => {
                let q = text.trim().to_string();
                if q.is_empty() {
                    return Vec::new();
                }
                self.query = q.clone();
                self.found.clear();
                self.found_sel = 0;
                self.screen = Screen::Found;
                vec![Action::Search(q)]
            }
            Writing::Pin => Vec::new(),
            Writing::Body => {
                let body = text::clean(&text, text::MAX_BODY);
                if body.trim().is_empty() {
                    // A picture can go without a text.
                    if !self.attached.is_empty() {
                        self.draft.clear();
                        self.screen = Screen::Preview;
                    }
                    return Vec::new();
                }
                self.draft = body;
                self.screen = Screen::Preview;
                Vec::new()
            }
        }
    }

    // Keys.

    pub fn key(&mut self, input: Input, rows: usize, visible: usize) -> Vec<Action> {
        if self.notice.take().is_some() {
            if self.confirm_delete {
                self.confirm_delete = false;
                if input == Input::Run {
                    self.screen = Screen::Threads;
                    return vec![Action::Delete(self.peer.clone()), Action::LoadThreads];
                }
            }
            return Vec::new();
        }
        match self.screen {
            Screen::Threads => self.key_threads(input),
            Screen::Locked => match input {
                Input::Run | Input::Ok | Input::Edit => vec![Action::Write(Writing::Pin, String::new())],
                Input::Escape => vec![Action::Quit],
                _ => Vec::new(),
            },
            Screen::Conversation => self.key_conversation(input, rows, visible),
            Screen::Options => self.key_options(input),
            Screen::Found => match input {
                Input::Up => {
                    self.found_sel = self.found_sel.saturating_sub(1);
                    Vec::new()
                }
                Input::Down => {
                    self.found_sel = (self.found_sel + 1).min(self.found.len().saturating_sub(1));
                    Vec::new()
                }
                Input::Run | Input::Ok => match self.found.get(self.found_sel) {
                    Some(m) => {
                        let peer = m.peer.clone();
                        self.open(&peer)
                    }
                    None => Vec::new(),
                },
                Input::Edit | Input::View => vec![Action::Write(Writing::Search, self.query.clone())],
                Input::Escape => {
                    self.screen = Screen::Threads;
                    Vec::new()
                }
                _ => Vec::new(),
            },
            Screen::QuickReply => match input {
                Input::Up => {
                    self.quick_sel = self.quick_sel.saturating_sub(1);
                    Vec::new()
                }
                Input::Down => {
                    self.quick_sel = (self.quick_sel + 1).min(QUICK.len() - 1);
                    Vec::new()
                }
                // Through the preview, which says what it costs, like any text.
                Input::Run | Input::Ok => {
                    self.draft = QUICK[self.quick_sel].to_string();
                    self.screen = Screen::Preview;
                    Vec::new()
                }
                Input::Edit => vec![Action::Write(Writing::Body, QUICK[self.quick_sel].to_string())],
                Input::Escape => {
                    self.screen = Screen::Options;
                    Vec::new()
                }
                _ => Vec::new(),
            },
            Screen::Preview => match input {
                Input::Run => {
                    let body = std::mem::take(&mut self.draft);
                    self.screen = Screen::Conversation;
                    self.scroll = None;
                    if self.attached.is_empty() {
                        return vec![Action::Send(self.peer.clone(), body)];
                    }
                    // A group's peer is its numbers with commas, as the desktop splits it.
                    let to = self.peer.split(',').map(str::trim).filter(|p| !p.is_empty()).map(String::from).collect();
                    let files = std::mem::take(&mut self.attached).into_iter().map(|p| p.path).collect();
                    vec![Action::SendMms(to, body, files)]
                }
                Input::Escape | Input::Edit => vec![Action::Write(Writing::Body, self.draft.clone())],
                Input::View => self.show_pictures(),
                _ => Vec::new(),
            },
            Screen::Attach => self.key_attach(input),
        }
    }

    fn show_pictures(&mut self) -> Vec<Action> {
        self.screen = Screen::Attach;
        self.picture_sel = 0;
        vec![Action::Pictures]
    }

    /// Where the pictures go back to: the preview while there is something to
    /// send, else the conversation.
    fn after_pictures(&mut self) {
        self.screen = if self.attached.is_empty() && self.draft.trim().is_empty() {
            Screen::Conversation
        } else {
            Screen::Preview
        };
    }

    /// Run adds the picture, or takes it off if it is on already, and goes back.
    fn key_attach(&mut self, input: Input) -> Vec<Action> {
        match input {
            Input::Up => self.picture_sel = self.picture_sel.saturating_sub(1),
            Input::Down => self.picture_sel = (self.picture_sel + 1).min(self.pictures.len().saturating_sub(1)),
            Input::Run | Input::Ok => {
                let Some(p) = self.pictures.get(self.picture_sel).cloned() else { return Vec::new() };
                if let Some(i) = self.attached.iter().position(|a| a.path == p.path) {
                    self.attached.remove(i);
                } else if self.attached.len() >= pictures::MAX_FILES {
                    self.notice =
                        Some(vec![format!("{} pictures at most", pictures::MAX_FILES), "in one message.".into()]);
                    return Vec::new();
                } else if self.attached.iter().map(|a| a.size).sum::<u64>() + p.size > pictures::MAX_BYTES {
                    self.notice = Some(vec![
                        "Too large to send together:".into(),
                        format!("{} MB in all at most.", pictures::MAX_BYTES >> 20),
                    ]);
                    return Vec::new();
                } else {
                    self.attached.push(p);
                }
                self.after_pictures();
            }
            Input::Escape => self.after_pictures(),
            _ => {}
        }
        Vec::new()
    }

    fn key_threads(&mut self, input: Input) -> Vec<Action> {
        let n = self.threads.len();
        match input {
            Input::Up => self.thread_sel = self.thread_sel.saturating_sub(1),
            Input::Down if n > 0 => self.thread_sel = (self.thread_sel + 1).min(n - 1),
            Input::Run | Input::Ok => {
                if let Some(t) = self.threads.get(self.thread_sel) {
                    let peer = t.peer.clone();
                    return self.open(&peer);
                }
            }
            Input::Edit => return vec![Action::Write(Writing::To, String::new())],
            Input::View => return vec![Action::Write(Writing::Search, self.query.clone())],
            Input::Escape => return vec![Action::Quit],
            _ => {}
        }
        Vec::new()
    }

    fn key_conversation(&mut self, input: Input, rows: usize, visible: usize) -> Vec<Action> {
        let last = rows.saturating_sub(visible);
        let at = self.scroll.unwrap_or(last).min(last);
        match input {
            Input::Up => self.scroll = Some(at.saturating_sub(1)),
            Input::Down => {
                let next = (at + 1).min(last);
                self.scroll = if next >= last { None } else { Some(next) };
            }
            Input::Left => self.scroll = Some(at.saturating_sub(visible)),
            Input::Right => {
                let next = (at + visible).min(last);
                self.scroll = if next >= last { None } else { Some(next) };
            }
            Input::Edit | Input::Ok => return vec![Action::Write(Writing::Body, String::new())],
            Input::Run => return vec![Action::Call(self.peer.clone())],
            Input::View => {
                self.screen = Screen::Options;
                self.option_sel = 0;
            }
            Input::Escape => {
                self.screen = Screen::Threads;
                self.peer.clear();
                return vec![Action::LoadThreads];
            }
        }
        Vec::new()
    }

    fn key_options(&mut self, input: Input) -> Vec<Action> {
        let opts = self.options();
        match input {
            Input::Up => self.option_sel = self.option_sel.saturating_sub(1),
            Input::Down => self.option_sel = (self.option_sel + 1).min(opts.len() - 1),
            Input::Escape | Input::View => self.screen = Screen::Conversation,
            Input::Run | Input::Ok => {
                self.screen = Screen::Conversation;
                return match opts[self.option_sel.min(opts.len() - 1)] {
                    Opt::QuickReply => {
                        self.screen = Screen::QuickReply;
                        self.quick_sel = 0;
                        Vec::new()
                    }
                    Opt::Attach => self.show_pictures(),
                    Opt::Resend => self.failed().map(|m| vec![Action::Retry(m.id)]).unwrap_or_default(),
                    Opt::Call => vec![Action::Call(self.peer.clone())],
                    Opt::Block if self.is_blocked() => vec![Action::Unblock(self.peer.clone())],
                    Opt::Block => vec![Action::Block(self.peer.clone())],
                    Opt::Delete => {
                        self.screen = Screen::Options;
                        self.confirm_delete = true;
                        self.notice = Some(vec!["Delete this conversation?".into(), "This cannot be undone.".into()]);
                        Vec::new()
                    }
                };
            }
            _ => {}
        }
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(id: i64, peer: &str, dir: u8, body: &str) -> MessageInfo {
        MessageInfo {
            id,
            peer: peer.into(),
            direction: dir,
            body: body.into(),
            at: 100 + id,
            state: 2,
            read: true,
            sim: String::new(),
            parts: Vec::new(),
        }
    }

    #[test]
    fn opening_and_replying() {
        let mut m = Model::new("AU", Book::default());
        m.set_threads(vec![ThreadInfo {
            peer: "+61491570156".into(),
            last_body: "hi".into(),
            last_at: 1,
            last_direction: 0,
            unread: 1,
        }]);
        let a = m.key(Input::Run, 0, 8);
        assert_eq!(a, vec![Action::Load("+61491570156".into()), Action::MarkRead("+61491570156".into())]);
        assert_eq!(m.screen, Screen::Conversation);
        assert_eq!(m.key(Input::Edit, 0, 8), vec![Action::Write(Writing::Body, String::new())]);
        m.written(Writing::Body, Some("On my way".into()));
        assert_eq!(m.screen, Screen::Preview);
        assert_eq!(m.key(Input::Run, 0, 8), vec![Action::Send("+61491570156".into(), "On my way".into())]);
    }

    #[test]
    fn preview_escape_goes_back_to_editing() {
        let mut m = Model::new("AU", Book::default());
        m.open("0491570156");
        m.written(Writing::Body, Some("draft".into()));
        assert_eq!(m.key(Input::Escape, 0, 8), vec![Action::Write(Writing::Body, "draft".into())]);
    }

    #[test]
    fn new_conversation_from_a_number() {
        let mut m = Model::new("AU", Book::default());
        assert_eq!(m.key(Input::Edit, 0, 8), vec![Action::Write(Writing::To, String::new())]);
        let a = m.written(Writing::To, Some("0491 570 157".into()));
        assert!(a.contains(&Action::Write(Writing::Body, String::new())));
        assert_eq!(m.peer, "+61491570157");
        let mut m = Model::new("AU", Book::default());
        m.written(Writing::To, Some("x".into()));
        assert!(m.notice.is_some());
    }

    #[test]
    fn scroll_follows_newest_until_moved() {
        let mut m = Model::new("AU", Book::default());
        m.open("0491570156");
        m.key(Input::Up, 20, 8);
        assert_eq!(m.scroll, Some(11));
        m.key(Input::Down, 20, 8);
        assert_eq!(m.scroll, None, "back at the bottom follows new messages again");
    }

    #[test]
    fn incoming_in_open_thread_is_marked_read() {
        let mut m = Model::new("AU", Book::default());
        m.open("0491570156");
        let a = m.message_added(msg(5, "+61491570156", 0, ""));
        assert!(a.contains(&Action::MarkRead("+61491570156".into())));
        assert!(a.contains(&Action::Load("+61491570156".into())), "the text is fetched, not taken from the signal");
        let a = m.message_added(msg(6, "+61400000000", 0, "other"));
        assert_eq!(a, vec![Action::LoadThreads]);
    }

    #[test]
    fn quick_replies_go_through_the_preview() {
        let mut m = Model::new("AU", Book::default());
        m.open("0491570156");
        m.key(Input::View, 0, 8);
        m.key(Input::Run, 0, 8);
        assert_eq!(m.screen, Screen::QuickReply);
        m.key(Input::Down, 0, 8);
        m.key(Input::Down, 0, 8);
        m.key(Input::Down, 0, 8);
        m.key(Input::Run, 0, 8);
        assert_eq!(m.screen, Screen::Preview);
        assert_eq!(m.key(Input::Run, 0, 8), vec![Action::Send("+61491570156".into(), "On my way.".into())]);
    }

    #[test]
    fn a_failed_text_can_be_resent() {
        let mut m = Model::new("AU", Book::default());
        m.open("0491570156");
        assert!(!m.options().contains(&Opt::Resend));
        let mut f = msg(9, "+61491570156", 1, "lost");
        f.state = MessageState::Failed as u8;
        m.set_conversation("+61491570156", vec![msg(8, "+61491570156", 0, "hi"), f]);
        assert_eq!(m.options()[0], Opt::Resend, "first, when there is something to resend");
        m.key(Input::View, 0, 8);
        assert_eq!(m.key(Input::Run, 0, 8), vec![Action::Retry(9)]);
    }

    fn pic(name: &str, size: u64) -> Picture {
        Picture { path: format!("/home/me/Pictures/{name}.jpg"), name: name.into(), size, at: 1 }
    }

    #[test]
    fn a_picture_from_options_goes_as_mms() {
        let mut m = Model::new("AU", Book::default());
        m.open("0491570156");
        m.key(Input::View, 0, 8);
        m.key(Input::Down, 0, 8);
        assert_eq!(m.key(Input::Run, 0, 8), vec![Action::Pictures], "Options > Attach looks for pictures");
        assert_eq!(m.screen, Screen::Attach);
        m.set_pictures(vec![pic("Beach", 1000), pic("Sunset", 2000)]);
        m.key(Input::Down, 0, 8);
        m.key(Input::Run, 0, 8);
        assert_eq!(m.screen, Screen::Preview, "picked, it is shown ready to go");
        assert_eq!(m.attached, vec![pic("Sunset", 2000)]);
        // With no text it still goes; a text written to go with it goes too.
        m.key(Input::Edit, 0, 8);
        m.written(Writing::Body, Some("  ".into()));
        assert_eq!(m.screen, Screen::Preview);
        m.written(Writing::Body, Some("Look".into()));
        let a = m.key(Input::Run, 0, 8);
        assert_eq!(
            a,
            vec![Action::SendMms(
                vec!["+61491570156".into()],
                "Look".into(),
                vec!["/home/me/Pictures/Sunset.jpg".into()]
            )]
        );
        assert!(m.attached.is_empty());
        // The next text is a text again.
        m.written(Writing::Body, Some("And that".into()));
        assert_eq!(m.key(Input::Run, 0, 8), vec![Action::Send("+61491570156".into(), "And that".into())]);
    }

    #[test]
    fn pictures_come_off_again_and_have_limits() {
        let mut m = Model::new("AU", Book::default());
        m.open("+61491570156,+61491570157");
        m.written(Writing::Body, Some("Both of you".into()));
        // From the preview, as when writing a new message.
        assert_eq!(m.key(Input::View, 0, 8), vec![Action::Pictures]);
        m.set_pictures(vec![pic("Beach", 5 << 20), pic("Sunset", 4 << 20)]);
        m.key(Input::Run, 0, 8);
        m.key(Input::View, 0, 8);
        m.key(Input::Down, 0, 8);
        m.key(Input::Run, 0, 8);
        assert!(m.notice.is_some(), "over phoned's 8 MB together");
        assert_eq!(m.attached.len(), 1);
        m.key(Input::Ok, 0, 8);
        m.key(Input::Up, 0, 8);
        m.key(Input::Run, 0, 8);
        assert!(m.attached.is_empty(), "Run on one already on takes it off");
        assert_eq!(m.screen, Screen::Preview, "the text is still there to send");
        m.key(Input::View, 0, 8);
        m.key(Input::Run, 0, 8);
        match m.key(Input::Run, 0, 8).as_slice() {
            [Action::SendMms(to, _, files)] => {
                assert_eq!(to, &["+61491570156".to_string(), "+61491570157".into()], "a group goes to everyone");
                assert_eq!(files.len(), 1);
            }
            a => panic!("{a:?}"),
        }
        // Nothing to send: the pictures go back to the conversation.
        m.key(Input::View, 0, 8);
        m.key(Input::Down, 0, 8);
        m.key(Input::Run, 0, 8);
        assert_eq!(m.screen, Screen::Attach);
        m.key(Input::Escape, 0, 8);
        assert_eq!(m.screen, Screen::Conversation);
        m.key(Input::View, 0, 8);
        m.key(Input::Down, 0, 8);
        m.key(Input::Run, 0, 8);
        m.key(Input::Run, 0, 8);
        assert_eq!(m.attached.len(), 1);
        m.key(Input::View, 0, 8);
        m.key(Input::Run, 0, 8);
        assert_eq!(m.screen, Screen::Conversation);
        // Another conversation starts with nothing on.
        m.key(Input::View, 0, 8);
        m.key(Input::Down, 0, 8);
        m.key(Input::Run, 0, 8);
        m.key(Input::Run, 0, 8);
        m.open("0491570158");
        assert!(m.attached.is_empty());
    }

    #[test]
    fn delete_asks_first() {
        let mut m = Model::new("AU", Book::default());
        m.open("0491570156");
        m.key(Input::View, 0, 8);
        for _ in 0..4 {
            m.key(Input::Down, 0, 8);
        }
        assert_eq!(m.options()[m.option_sel], Opt::Delete);
        assert!(m.key(Input::Run, 0, 8).is_empty());
        assert!(m.notice.is_some());
        let a = m.key(Input::Run, 0, 8);
        assert_eq!(a, vec![Action::Delete("+61491570156".into()), Action::LoadThreads]);
    }

    #[test]
    fn locked_messages_ask_for_the_pin() {
        let mut m = Model::new("AU", Book::default());
        m.set_threads(vec![ThreadInfo {
            peer: "+61491570156".into(),
            last_body: "hi".into(),
            last_at: 1,
            last_direction: 0,
            unread: 1,
        }]);
        m.open("0491570156");
        m.set_conversation("+61491570156", vec![msg(1, "+61491570156", 0, "secret")]);
        // A load refused for the lock locks, as the signal does.
        assert!(Model::is_lock_error("Messages is locked: enter the PIN"));
        assert_eq!(m.set_locked(true), vec![Action::Write(Writing::Pin, String::new())]);
        assert_eq!(m.screen, Screen::Locked);
        assert!(m.threads.is_empty() && m.messages.is_empty(), "nothing is left behind the PIN");
        assert!(m.set_locked(true).is_empty(), "a second word of it changes nothing");
        // Texts arriving while locked fetch nothing.
        assert!(m.message_added(msg(2, "+61491570156", 0, "")).is_empty());
        // The Dialer's handoff waits for the PIN.
        assert!(m.compose_to("0491570157").is_empty());
        assert_eq!(m.written(Writing::Pin, Some("1234".into())), vec![Action::Unlock("1234".into())]);
        // A wrong PIN leaves it locked; Run types it again, Escape leaves.
        assert_eq!(m.key(Input::Run, 0, 8), vec![Action::Write(Writing::Pin, String::new())]);
        assert_eq!(m.written(Writing::Pin, None), vec![Action::Quit]);
        assert_eq!(m.key(Input::Escape, 0, 8), vec![Action::Quit]);
        // Right: the list loads, and the waiting conversation opens to write in.
        let a = m.set_locked(false);
        assert_eq!(a[0], Action::LoadThreads);
        assert!(a.contains(&Action::Write(Writing::Body, String::new())));
        assert_eq!(m.peer, "+61491570157");
        assert!(Model::valid_pin("0000") && !Model::valid_pin("123") && !Model::valid_pin("12a4"));
    }

    #[test]
    fn search_from_the_thread_list() {
        let mut m = Model::new("AU", Book::default());
        assert_eq!(m.key(Input::View, 0, 8), vec![Action::Write(Writing::Search, String::new())]);
        assert_eq!(m.written(Writing::Search, Some(" parcel ".into())), vec![Action::Search("parcel".into())]);
        assert_eq!(m.screen, Screen::Found);
        m.found = vec![msg(3, "+61491570156", 0, "Your parcel is at the post office")];
        let a = m.key(Input::Run, 0, 8);
        assert_eq!(a[0], Action::Load("+61491570156".into()), "a result opens its conversation");
    }
}
