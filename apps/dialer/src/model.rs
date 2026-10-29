//! What the Dialer shows and what its keys do, with no UI and no bus in it.
//!
//! The screen on top is decided by the phone's state, not by navigation: a
//! confirmation waiting beats a ringing call, a ringing call beats a live one, and a
//! live call beats whatever the user was browsing. When those clear, the user is
//! back where they were.
//!
//! Keys follow FlipCTL's soft-key roles everywhere: Escape backs out, View switches
//! view or shows information, Edit edits, Run does the main thing. Back and the app
//! switcher never arrive here (flipctl keeps them), which is why leaving a call
//! screen with Back leaves the call running.

use std::time::{Duration, Instant};

use phone_api::{AlertInfo, CallInfo, CallRecordInfo, ModemStatus, Request, RouteInfo, ThreadInfo};
use phone_core::contacts::{Book, Contact};
use phone_core::model::{CallState, Route};
use phone_core::number::{self, Class};

/// The panel's keys, as the model sees them: flipctl_app's, plus typed characters
/// from a keyboard (a USB keyboard, the remote view, desktop mode).
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
    Ptt,
    Char(char),
    /// A tap on the touchpad over a keypad cell (row, column).
    Tap(usize, usize),
}

/// What the model asks the app to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Dial(String),
    Answer(u32),
    Hangup(u32),
    Swap,
    /// Join the active and held calls.
    Merge,
    Dtmf(u32, String),
    Mute(bool),
    CycleRoute,
    Silence,
    Confirm(u32),
    Deny(u32),
    Ussd(String),
    UssdRespond(String),
    UssdCancel,
    /// Text this number: hand over to Messages.
    Text(String),
    Reply(String, String),
    MarkSeen,
    Unlock(String),
    SetSetting(String, String),
    SetRinger(String),
    Quit,
    Buzz(Buzz),
}

/// The vibration vocabulary: each thing the phone tells you has its own feel, so
/// it can be told apart in a pocket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Buzz {
    /// A key press.
    Tick,
    /// A call connecting or ending.
    Thump,
    /// A text arrived: two short.
    Text,
    /// A call was missed: three short.
    Missed,
    /// A second call is waiting: two long.
    Waiting,
    /// A call dropped that nobody here ended: one long.
    Dropped,
    /// A text failed to send: short, short, long.
    Failed,
    /// An emergency alert: long, long, long.
    Alert,
    /// A press ignored on purpose: a quick stutter.
    Refused,
}

impl Buzz {
    /// On and off times in milliseconds, on first: phone-bus's, so Messages
    /// buzzes for a text exactly as the Dialer does.
    pub fn pattern(self) -> &'static [u16] {
        use phone_bus::haptics as h;
        match self {
            Self::Tick => h::TICK,
            Self::Thump => h::THUMP,
            Self::Text => h::TEXT,
            Self::Missed => h::MISSED,
            Self::Waiting => h::WAITING,
            Self::Dropped => h::DROPPED,
            Self::Failed => h::FAILED,
            Self::Alert => h::ALERT,
            Self::Refused => h::REFUSED,
        }
    }
}

/// Where the user is when nothing is ringing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Browse {
    Keypad,
    /// What is waiting: missed calls, unread conversations, favourites. Where the
    /// Dialer opens when there is any of that.
    Today(usize),
    Recents,
    Contacts,
    /// A USSD session: the network's last words, and whether it wants a reply.
    Ussd {
        text: String,
        awaiting: bool,
    },
    /// Giving the selected contact a speed-dial key, 2 to 9 (the index is the key
    /// less two).
    SpeedPick(usize),
}

/// What is on top, derived from the state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Top {
    /// An emergency alert, over everything but a ringing call.
    Alert(AlertInfo),
    Confirm(Request),
    Incoming(CallInfo),
    InCall,
    Browse,
}

pub const KEYS: [[(char, &str); 3]; 4] = [
    [('1', ""), ('2', "ABC"), ('3', "DEF")],
    [('4', "GHI"), ('5', "JKL"), ('6', "MNO")],
    [('7', "PQRS"), ('8', "TUV"), ('9', "WXYZ")],
    [('*', ""), ('0', "+"), ('#', "")],
];

/// A long press is at least this, unless the owner chose otherwise.
pub const HOLD: Duration = Duration::from_millis(600);
/// The longest hold the owner may choose: three seconds, for hands that press
/// slowly or shake.
pub const MAX_HOLD_MS: u64 = 3000;

/// One line of the Today screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TodayItem {
    Missed { peer: String, count: u32, at: i64 },
    Unread { peer: String, count: u32, at: i64 },
    Favourite { key: usize, number: String },
}
/// After a call is answered, End is ignored for this long, so the press that
/// answered (or a nervous second one) cannot hang up.
pub const END_LOCKOUT: Duration = Duration::from_millis(1500);
/// After a screen appears unbidden (a call, a confirmation), keys are ignored for
/// this long, so a press meant for what was there before does not land on it.
pub const FRESH: Duration = Duration::from_millis(400);

pub const CANNED: [&str; 4] = ["Can't talk now, I'll call you back.", "On my way.", "In a meeting.", "Text me?"];

pub struct Model {
    pub region: String,
    pub browse: Browse,
    pub number: String,
    pub sel: (usize, usize),
    pub calls: Vec<CallInfo>,
    pub recents: Vec<CallRecordInfo>,
    pub recents_sel: usize,
    pub book: Book,
    pub contacts_sel: usize,
    pub pending: Vec<Request>,
    pub status: ModemStatus,
    pub routes: Vec<RouteInfo>,
    pub route: Route,
    /// The in-call keypad is open, and what has been sent on it.
    pub dtmf_open: bool,
    pub dtmf: String,
    pub dtmf_sel: (usize, usize),
    /// Showing the caller's number and class on the incoming screen.
    pub info: bool,
    /// The canned replies picker, and which is selected.
    pub replies: Option<usize>,
    /// A message for the modal, cleared by any key.
    pub notice: Option<Vec<String>>,
    pub connected: bool,
    /// The keypad is typing a reply to a USSD menu rather than a number.
    pub ussd_replying: bool,
    /// Hold-1 calls this; empty when there is none.
    pub voicemail: String,
    /// Hold-2 .. Hold-9.
    pub speed: [String; 8],
    /// Decline needs a hold, for unsteady hands.
    pub steady_decline: bool,
    pub ringer: String,
    pub missed: u32,
    pub unread: u32,
    /// Emergency alerts not yet dismissed, the most severe on screen.
    pub alerts: phone_bus::alerts::AlertQueue,
    /// Conversations, for the unread ones on Today.
    pub threads: Vec<ThreadInfo>,
    /// How long a hold is.
    pub hold: Duration,
    /// A one-line hint on the incoming screen ("Hold Decline to decline").
    pub hint: Option<&'static str>,
    /// The call the owner just ended here, so its going is not a drop.
    ending: Vec<u32>,
    answered_at: Option<Instant>,
    top_since: (Option<Top>, Instant),
    held: Option<(Input, Instant)>,
}

impl Model {
    pub fn new(region: &str, book: Book) -> Self {
        Self {
            region: region.to_string(),
            browse: Browse::Keypad,
            number: String::new(),
            sel: (0, 0),
            calls: Vec::new(),
            recents: Vec::new(),
            recents_sel: 0,
            book,
            contacts_sel: 0,
            pending: Vec::new(),
            status: phone_api::ModemStatus {
                present: false,
                registered: false,
                roaming: false,
                operator: String::new(),
                quality: 0,
                tech: "--".into(),
                voice: false,
                own_number: String::new(),
                emergency_only: false,
                state: "ready".into(),
                sip: String::new(),
            },
            routes: Vec::new(),
            route: Route::Speaker,
            dtmf_open: false,
            dtmf: String::new(),
            dtmf_sel: (0, 0),
            info: false,
            replies: None,
            notice: None,
            connected: false,
            ussd_replying: false,
            voicemail: String::new(),
            speed: Default::default(),
            steady_decline: false,
            ringer: "ring".into(),
            missed: 0,
            unread: 0,
            alerts: Default::default(),
            threads: Vec::new(),
            hold: HOLD,
            hint: None,
            ending: Vec::new(),
            answered_at: None,
            top_since: (None, Instant::now()),
            held: None,
        }
    }

    pub fn ringing(&self) -> Option<&CallInfo> {
        self.calls.iter().find(|c| CallState::from_u8(c.state).ringing_in())
    }

    /// The call the in-call screen is about: the active one, else any live one.
    pub fn current(&self) -> Option<&CallInfo> {
        self.calls.iter().find(|c| CallState::from_u8(c.state) == CallState::Active).or_else(|| {
            self.calls.iter().find(|c| !CallState::from_u8(c.state).ringing_in() && CallState::from_u8(c.state).live())
        })
    }

    /// The other call, when there are two.
    pub fn other(&self) -> Option<&CallInfo> {
        let cur = self.current()?.id;
        self.calls
            .iter()
            .find(|c| c.id != cur && CallState::from_u8(c.state).live() && !CallState::from_u8(c.state).ringing_in())
    }

    pub fn top(&self) -> Top {
        // A ringing call first: it cannot wait, and answering it does not hide the
        // alert, which is back on top once the call is dealt with.
        let ringing = self.ringing().cloned();
        if let (Some(a), None) = (self.alerts.current(), &ringing) {
            return Top::Alert(a.clone());
        }
        if let Some(r) = self.pending.first() {
            return Top::Confirm(r.clone());
        }
        if let Some(c) = self.ringing() {
            return Top::Incoming(c.clone());
        }
        if self.current().is_some() {
            return Top::InCall;
        }
        Top::Browse
    }

    /// Note what is on top now, for the fresh-screen guard. Call after every state
    /// change and before handling keys.
    pub fn settle(&mut self) {
        let top = self.top();
        let same = match (&self.top_since.0, &top) {
            (Some(Top::Confirm(a)), Top::Confirm(b)) => a.id == b.id,
            (Some(Top::Incoming(a)), Top::Incoming(b)) => a.id == b.id,
            (Some(Top::Alert(a)), Top::Alert(b)) => a.id == b.id,
            (Some(a), b) => std::mem::discriminant(a) == std::mem::discriminant(b),
            (None, _) => false,
        };
        if !same {
            if !matches!(top, Top::Incoming(_)) {
                self.info = false;
                self.replies = None;
                self.hint = None;
            }
            if !matches!(top, Top::InCall) {
                self.dtmf_open = false;
                self.dtmf.clear();
            }
            self.top_since = (Some(top), Instant::now());
        }
    }

    pub fn name_for(&self, n: &str) -> String {
        if n.is_empty() {
            return "Unknown".into();
        }
        readable(self.book.name_for(n, &self.region), n, &self.region)
    }

    /// A contact whose number starts like what has been typed, for the line under
    /// the number.
    pub fn typed_match(&self) -> Option<&Contact> {
        if number::digits(&self.number).len() < 3 {
            return None;
        }
        self.book.search(&self.number).into_iter().next()
    }

    /// A contact whose name the typed digits spell on the keypad's letters ("567"
    /// is Jordan), when no number matches. Run calls it while what is typed is too
    /// short to be a phone number.
    pub fn t9_match(&self) -> Option<&Contact> {
        let d = number::digits(&self.number);
        if self.typed_match().is_some() || d.len() >= T9_MAX || self.number.contains(['*', '#', '+']) {
            return None;
        }
        if self.classify_typed() == Class::Emergency {
            return None;
        }
        self.book.search_t9(&d).into_iter().next()
    }

    /// A keypad is on screen (the dialler's, or the in-call one).
    pub fn keypad_showing(&self) -> bool {
        match self.top() {
            Top::Browse => self.browse == Browse::Keypad,
            Top::InCall => self.dtmf_open,
            _ => false,
        }
    }

    /// What Today lists: missed calls not yet seen (one line a caller), unread
    /// conversations, then the speed-dial favourites.
    pub fn today(&self) -> Vec<TodayItem> {
        let mut out: Vec<TodayItem> = Vec::new();
        for r in self.recents.iter().filter(|r| r.outcome == phone_core::model::Outcome::Missed as u8 && !r.seen) {
            let same = out
                .iter_mut()
                .find(|i| matches!(i, TodayItem::Missed { peer, .. } if number::same_party(peer, &r.peer)));
            match same {
                Some(TodayItem::Missed { count, .. }) => *count += 1,
                _ => out.push(TodayItem::Missed { peer: r.peer.clone(), count: 1, at: r.started }),
            }
        }
        for t in self.threads.iter().filter(|t| t.unread > 0) {
            out.push(TodayItem::Unread { peer: t.peer.clone(), count: t.unread, at: t.last_at });
        }
        for (i, n) in self.speed.iter().enumerate().filter(|(_, n)| !n.is_empty()) {
            out.push(TodayItem::Favourite { key: i + 2, number: n.clone() });
        }
        out
    }

    /// Where to open: Today when something is waiting, the keypad otherwise.
    pub fn opened(&mut self) {
        let waiting = self.today().iter().any(|i| !matches!(i, TodayItem::Favourite { .. }));
        self.browse = if waiting { Browse::Today(0) } else { Browse::Keypad };
    }

    pub fn locked(&self) -> bool {
        self.connected && self.status.present && self.status.state == "locked"
    }

    /// What Hold-N calls, if anything.
    pub fn held_number(&self, d: char) -> Option<String> {
        let n = match d {
            '1' => self.voicemail.clone(),
            '2'..='9' => self.speed[(d as u8 - b'2') as usize].clone(),
            _ => String::new(),
        };
        (!n.is_empty()).then_some(n)
    }

    /// The Australian National Relay Service, for a Deaf caller, a caller with a
    /// speech impairment, or someone calling one.
    pub fn relay_hint(&self) -> Option<&'static str> {
        (self.region == "AU" && number::digits(&self.number) == "106").then_some("TTY relay; or text 0423 677 767")
    }

    pub fn is_ussd(&self) -> bool {
        let n = self.number.trim();
        n.len() > 1 && (n.starts_with('*') || n.starts_with('#')) && n.ends_with('#')
    }

    pub fn classify_typed(&self) -> Class {
        number::classify(&self.number, &self.region)
    }

    pub fn recent_peers(&self) -> &[CallRecordInfo] {
        &self.recents
    }

    // Events from phoned.

    pub fn call_changed(&mut self, c: CallInfo) -> Vec<Action> {
        let was = self.calls.iter().find(|x| x.id == c.id).map(|x| CallState::from_u8(x.state));
        let now = CallState::from_u8(c.state);
        let mut out = Vec::new();
        if now == CallState::Waiting && was.is_none() {
            out.push(Action::Buzz(Buzz::Waiting));
        }
        if now == CallState::Active && was != Some(CallState::Active) && was != Some(CallState::Held) {
            self.answered_at = Some(Instant::now());
            out.push(Action::Buzz(Buzz::Thump));
        }
        match self.calls.iter_mut().find(|x| x.id == c.id) {
            Some(slot) => *slot = c,
            None => self.calls.push(c),
        }
        self.settle();
        out
    }

    pub fn call_removed(&mut self, id: u32) -> Vec<Action> {
        let gone = self.calls.iter().find(|c| c.id == id).map(|c| CallState::from_u8(c.state));
        self.calls.retain(|c| c.id != id);
        let ours = self.ending.contains(&id);
        self.ending.retain(|e| *e != id);
        self.settle();
        match gone {
            // A live call ending that nobody ended here: the far end hung up, or the
            // network lost it. Either way it should feel different from End.
            Some(CallState::Active | CallState::Held) if !ours => vec![Action::Buzz(Buzz::Dropped)],
            Some(_) => vec![Action::Buzz(Buzz::Thump)],
            None => Vec::new(),
        }
    }

    pub fn missed_changed(&mut self, n: u32) -> Vec<Action> {
        let more = n > self.missed;
        self.missed = n;
        if more {
            vec![Action::Buzz(Buzz::Missed)]
        } else {
            Vec::new()
        }
    }

    pub fn alert_received(&mut self, a: AlertInfo) -> Vec<Action> {
        // Public information is kept, but does not take the screen.
        if a.level == phone_core::model::AlertLevel::Info as u8 {
            return Vec::new();
        }
        // One heard twice is not news; one waiting behind another still buzzes.
        if !self.alerts.push(a) {
            return Vec::new();
        }
        self.settle();
        vec![Action::Buzz(Buzz::Alert)]
    }

    pub fn setting(&mut self, key: &str, value: String) {
        match key {
            "region" => self.region = value,
            "voicemail" => self.voicemail = value,
            "steady_decline" => self.steady_decline = value == "true",
            "long_press_ms" => {
                if let Ok(ms) = value.parse::<u64>() {
                    self.hold = Duration::from_millis(ms.clamp(300, MAX_HOLD_MS));
                }
            }
            k if k.starts_with("speed_") => {
                if let Some(i) = k[6..].parse::<usize>().ok().filter(|i| (2..=9).contains(i)) {
                    self.speed[i - 2] = value;
                }
            }
            _ => {}
        }
    }

    pub fn set_calls(&mut self, calls: Vec<CallInfo>) {
        self.calls = calls;
        self.settle();
    }

    pub fn set_pending(&mut self, p: Vec<Request>) {
        self.pending = p;
        self.settle();
    }

    pub fn requested(&mut self, r: Request) {
        if !self.pending.iter().any(|p| p.id == r.id) {
            self.pending.push(r);
        }
        self.settle();
    }

    pub fn resolved(&mut self, id: u32) {
        self.pending.retain(|p| p.id != id);
        self.settle();
    }

    pub fn set_recents(&mut self, r: Vec<CallRecordInfo>) {
        self.recents = r;
        self.recents_sel = self.recents_sel.min(self.recents.len().saturating_sub(1));
    }

    // Keys.

    pub fn key(&mut self, input: Input, down: bool, now: Instant) -> Vec<Action> {
        self.settle();
        if !down {
            return self.release(input, now);
        }
        if self.top_since.1.elapsed() < FRESH && !matches!(self.top(), Top::Browse) {
            // Felt, so a press that did nothing is not mistaken for one that did.
            return vec![Action::Buzz(Buzz::Refused)];
        }
        self.held = Some((input, now));
        if self.notice.take().is_some() {
            return Vec::new();
        }
        match self.top() {
            Top::Alert(_) => {
                if matches!(input, Input::Run | Input::Ok | Input::Escape) {
                    self.alerts.dismiss();
                    self.settle();
                }
                Vec::new()
            }
            Top::Confirm(r) => self.key_confirm(&r, input),
            Top::Incoming(c) => self.key_incoming(&c, input),
            Top::InCall => self.key_in_call(input, now),
            Top::Browse => self.key_browse(input),
        }
    }

    /// Releases are where long presses are told from short ones: 0 or +, a digit or
    /// its speed dial, swap or merge, a pressed or a held Decline, and Edit or PTT
    /// held on the keypad.
    fn release(&mut self, input: Input, now: Instant) -> Vec<Action> {
        let Some((pressed, at)) = self.held.take() else { return Vec::new() };
        if pressed != input {
            return Vec::new();
        }
        let long = now.duration_since(at) >= self.hold;
        match self.top() {
            Top::Incoming(c) if input == Input::Escape && self.steady_decline && self.replies.is_none() => {
                if long {
                    return vec![Action::Hangup(c.id)];
                }
                self.hint = Some("Hold Decline to decline");
                return vec![Action::Buzz(Buzz::Refused)];
            }
            Top::InCall if input == Input::Ok && !self.dtmf_open && self.other().is_some() => {
                return vec![if long { Action::Merge } else { Action::Swap }];
            }
            Top::Browse => {}
            _ => return Vec::new(),
        }
        match (&self.browse, input) {
            (Browse::Contacts, Input::Ok) => {
                let Some(c) = self.book.contacts.get(self.contacts_sel) else { return Vec::new() };
                if long {
                    self.browse = Browse::SpeedPick(0);
                    return vec![Action::Buzz(Buzz::Tick)];
                }
                return c.numbers.first().map(|p| vec![Action::Dial(p.number.clone())]).unwrap_or_default();
            }
            (Browse::Keypad, _) => {}
            _ => return Vec::new(),
        }
        match input {
            Input::Ok => {
                let d = KEYS[self.sel.0][self.sel.1].0;
                self.release_digit(d, long)
            }
            Input::Edit if long => {
                self.number.clear();
                Vec::new()
            }
            // Hold PTT on the keypad: Ring, then Vibrate, then Silent.
            Input::Ptt if long => {
                let next = match self.ringer.as_str() {
                    "ring" => "vibrate",
                    "vibrate" => "silent",
                    _ => "ring",
                };
                self.ringer = next.into();
                let shown = match next {
                    "ring" => "Ringer on",
                    "vibrate" => "Vibrate only",
                    _ => "Silent",
                };
                self.notice = Some(vec![shown.into()]);
                vec![Action::SetRinger(next.into()), Action::Buzz(Buzz::Thump)]
            }
            _ => Vec::new(),
        }
    }

    /// A digit that waited for its release: 0 (held: +), and 1 to 9 on an empty
    /// keypad (held: voicemail and speed dial).
    fn release_digit(&mut self, d: char, long: bool) -> Vec<Action> {
        if d == '0' {
            self.number.push(if long { '+' } else { '0' });
            return vec![Action::Buzz(Buzz::Tick)];
        }
        if !d.is_ascii_digit() || !self.number.is_empty() || self.locked() || self.ussd_replying {
            return Vec::new();
        }
        if !long {
            self.number.push(d);
            return vec![Action::Buzz(Buzz::Tick)];
        }
        match self.held_number(d) {
            Some(n) => vec![Action::Buzz(Buzz::Thump), Action::Dial(n)],
            None if d == '1' => {
                self.notice = Some(wrap_lines("No voicemail number. Set one in Phone Settings."));
                Vec::new()
            }
            None => {
                self.notice = Some(wrap_lines(&format!("Nothing on {d} yet. Hold Ok on a contact to put it there.")));
                Vec::new()
            }
        }
    }

    fn key_confirm(&mut self, r: &Request, input: Input) -> Vec<Action> {
        match input {
            Input::Run => vec![Action::Confirm(r.id)],
            Input::Escape => vec![Action::Deny(r.id)],
            _ => Vec::new(),
        }
    }

    fn key_incoming(&mut self, c: &CallInfo, input: Input) -> Vec<Action> {
        if let Some(sel) = self.replies {
            return match input {
                Input::Up => {
                    self.replies = Some(sel.saturating_sub(1));
                    Vec::new()
                }
                Input::Down => {
                    self.replies = Some((sel + 1).min(CANNED.len() - 1));
                    Vec::new()
                }
                Input::Run | Input::Ok => {
                    self.replies = None;
                    vec![Action::Reply(c.number.clone(), CANNED[sel].to_string()), Action::Hangup(c.id)]
                }
                Input::Escape => {
                    self.replies = None;
                    Vec::new()
                }
                _ => Vec::new(),
            };
        }
        match input {
            Input::Run => vec![Action::Answer(c.id)],
            // With steady decline, the release decides (a hold declines).
            Input::Escape if self.steady_decline => Vec::new(),
            Input::Escape => vec![Action::Hangup(c.id)],
            Input::View => {
                self.info = !self.info;
                Vec::new()
            }
            Input::Edit if !c.number.is_empty() => {
                self.replies = Some(0);
                Vec::new()
            }
            Input::Ptt => vec![Action::Silence],
            _ => Vec::new(),
        }
    }

    fn key_in_call(&mut self, input: Input, now: Instant) -> Vec<Action> {
        let Some(cur) = self.current().cloned() else { return Vec::new() };
        if self.dtmf_open {
            match input {
                Input::Up | Input::Down | Input::Left | Input::Right => {
                    self.dtmf_sel = step(self.dtmf_sel, input);
                    return Vec::new();
                }
                Input::Ok => {
                    let d = KEYS[self.dtmf_sel.0][self.dtmf_sel.1].0;
                    self.dtmf.push(d);
                    return vec![Action::Dtmf(cur.id, d.to_string()), Action::Buzz(Buzz::Tick)];
                }
                Input::Tap(r, c) => {
                    self.dtmf_sel = (r, c);
                    let d = KEYS[r][c].0;
                    self.dtmf.push(d);
                    return vec![Action::Dtmf(cur.id, d.to_string()), Action::Buzz(Buzz::Tick)];
                }
                Input::Char(c) if c.is_ascii_digit() || c == '*' || c == '#' => {
                    self.dtmf.push(c);
                    return vec![Action::Dtmf(cur.id, c.to_string())];
                }
                Input::Escape | Input::View => {
                    self.dtmf_open = false;
                    return Vec::new();
                }
                _ => {}
            }
        }
        match input {
            Input::Run => {
                let locked = self.answered_at.is_some_and(|t| now.duration_since(t) < END_LOCKOUT);
                if locked {
                    vec![Action::Buzz(Buzz::Refused)]
                } else {
                    self.ending.push(cur.id);
                    vec![Action::Hangup(cur.id)]
                }
            }
            Input::View => {
                self.dtmf_open = true;
                Vec::new()
            }
            Input::Edit => vec![Action::CycleRoute],
            Input::Ptt => vec![Action::Mute(!cur.muted)],
            // Two calls: Ok swaps between them, a held Ok merges them (on release).
            Input::Ok if self.other().is_some() => Vec::new(),
            Input::Char(c) if c.is_ascii_digit() || c == '*' || c == '#' => {
                vec![Action::Dtmf(cur.id, c.to_string())]
            }
            _ => Vec::new(),
        }
    }

    fn key_browse(&mut self, input: Input) -> Vec<Action> {
        match self.browse.clone() {
            Browse::Keypad => self.key_keypad(input),
            Browse::Recents => self.key_recents(input),
            Browse::Contacts => self.key_contacts(input),
            Browse::SpeedPick(i) => self.key_speed_pick(i, input),
            Browse::Today(i) => self.key_today(i, input),
            Browse::Ussd { awaiting, .. } => match input {
                Input::Escape => {
                    self.browse = Browse::Keypad;
                    self.number.clear();
                    if awaiting {
                        vec![Action::UssdCancel]
                    } else {
                        Vec::new()
                    }
                }
                Input::Edit if awaiting => {
                    // Reply on the keypad: the number field becomes the reply.
                    self.browse = Browse::Keypad;
                    self.number.clear();
                    self.ussd_replying = true;
                    Vec::new()
                }
                _ => Vec::new(),
            },
        }
    }

    fn key_keypad(&mut self, input: Input) -> Vec<Action> {
        match input {
            Input::Up | Input::Down | Input::Left | Input::Right => {
                self.sel = step(self.sel, input);
                Vec::new()
            }
            Input::Ok => {
                let d = KEYS[self.sel.0][self.sel.1].0;
                // Typed on release when a hold means something else: 0 (or +),
                // and 1 to 9 on an empty keypad (voicemail, speed dial).
                if self.waits_for_release(d) {
                    return Vec::new();
                }
                self.number.push(d);
                vec![Action::Buzz(Buzz::Tick)]
            }
            Input::Tap(r, c) => {
                self.sel = (r, c);
                let d = KEYS[r][c].0;
                self.number.push(d);
                vec![Action::Buzz(Buzz::Tick)]
            }
            Input::Char(c) if c.is_ascii_digit() || c == '*' || c == '#' || c == '+' => {
                self.number.push(c);
                Vec::new()
            }
            Input::Edit => {
                self.number.pop();
                Vec::new()
            }
            Input::Run if self.locked() && !matches!(self.classify_typed(), Class::Emergency) => {
                // A locked SIM: what is typed is its PIN. Emergency numbers still
                // dial, locked or not.
                let pin = std::mem::take(&mut self.number);
                if pin.is_empty() {
                    return Vec::new();
                }
                vec![Action::Unlock(pin)]
            }
            Input::Run => {
                if self.ussd_replying {
                    self.ussd_replying = false;
                    let reply = std::mem::take(&mut self.number);
                    return vec![Action::UssdRespond(reply)];
                }
                if self.number.is_empty() {
                    // An empty Call redials the last number, as phones do.
                    if let Some(last) = self.recents.iter().find(|r| r.direction == 1) {
                        self.number = number::display(&last.peer, &self.region).replace(' ', "");
                    }
                    return Vec::new();
                }
                if self.is_ussd() {
                    let code = std::mem::take(&mut self.number);
                    self.browse = Browse::Ussd { text: "Sending...".into(), awaiting: false };
                    return vec![Action::Ussd(code)];
                }
                if let Some(c) = self.t9_match().and_then(|c| c.numbers.first()) {
                    let n = c.number.clone();
                    self.number.clear();
                    return vec![Action::Dial(n)];
                }
                let n = std::mem::take(&mut self.number);
                vec![Action::Dial(n)]
            }
            Input::View => {
                self.browse = Browse::Recents;
                vec![Action::MarkSeen]
            }
            Input::Escape => {
                if self.number.is_empty() {
                    vec![Action::Quit]
                } else {
                    self.number.clear();
                    Vec::new()
                }
            }
            _ => Vec::new(),
        }
    }

    fn key_recents(&mut self, input: Input) -> Vec<Action> {
        let n = self.recents.len();
        match input {
            Input::Up => self.recents_sel = self.recents_sel.saturating_sub(1),
            Input::Down if n > 0 => self.recents_sel = (self.recents_sel + 1).min(n - 1),
            Input::Run | Input::Ok => {
                if let Some(r) = self.recents.get(self.recents_sel) {
                    return vec![Action::Dial(r.peer.clone())];
                }
            }
            Input::Edit => {
                if let Some(r) = self.recents.get(self.recents_sel) {
                    return vec![Action::Text(r.peer.clone())];
                }
            }
            Input::View => self.browse = Browse::Contacts,
            Input::Escape => self.browse = Browse::Keypad,
            _ => {}
        }
        Vec::new()
    }

    fn key_contacts(&mut self, input: Input) -> Vec<Action> {
        let n = self.book.contacts.len();
        match input {
            Input::Up => self.contacts_sel = self.contacts_sel.saturating_sub(1),
            Input::Down if n > 0 => self.contacts_sel = (self.contacts_sel + 1).min(n - 1),
            // Left and right jump a letter, for a long book on a five-row screen.
            Input::Right => self.contacts_sel = self.letter_jump(true),
            Input::Left => self.contacts_sel = self.letter_jump(false),
            // Ok calls on release; held, it gives the contact a speed-dial key.
            Input::Ok => {}
            Input::Run => {
                if let Some(p) = self.book.contacts.get(self.contacts_sel).and_then(|c| c.numbers.first()) {
                    return vec![Action::Dial(p.number.clone())];
                }
            }
            Input::Edit => {
                if let Some(p) = self.book.contacts.get(self.contacts_sel).and_then(|c| c.numbers.first()) {
                    return vec![Action::Text(p.number.clone())];
                }
            }
            Input::View => self.browse = Browse::Today(0),
            Input::Escape => self.browse = Browse::Keypad,
            _ => {}
        }
        Vec::new()
    }

    fn key_today(&mut self, sel: usize, input: Input) -> Vec<Action> {
        let items = self.today();
        let n = items.len();
        let number_of = |i: &TodayItem| match i {
            TodayItem::Missed { peer, .. } | TodayItem::Unread { peer, .. } => peer.clone(),
            TodayItem::Favourite { number, .. } => number.clone(),
        };
        match input {
            Input::Up => self.browse = Browse::Today(sel.saturating_sub(1)),
            Input::Down if n > 0 => self.browse = Browse::Today((sel + 1).min(n - 1)),
            // A conversation opens in Messages; anything else is called back.
            Input::Run | Input::Ok => match items.get(sel) {
                Some(TodayItem::Unread { peer, .. }) => return vec![Action::Text(peer.clone())],
                Some(i) => return vec![Action::Dial(number_of(i))],
                None => {}
            },
            Input::Edit => {
                if let Some(i) = items.get(sel) {
                    return vec![Action::Text(number_of(i))];
                }
            }
            Input::View | Input::Escape => self.browse = Browse::Keypad,
            _ => {}
        }
        Vec::new()
    }

    fn waits_for_release(&self, d: char) -> bool {
        d == '0' || (d.is_ascii_digit() && self.number.is_empty() && !self.locked() && !self.ussd_replying)
    }

    fn key_speed_pick(&mut self, i: usize, input: Input) -> Vec<Action> {
        match input {
            Input::Up => self.browse = Browse::SpeedPick(i.saturating_sub(1)),
            Input::Down => self.browse = Browse::SpeedPick((i + 1).min(7)),
            Input::Run | Input::Ok => {
                let Some(n) = self.book.contacts.get(self.contacts_sel).and_then(|c| c.numbers.first()) else {
                    return Vec::new();
                };
                let n = n.number.clone();
                self.speed[i] = n.clone();
                self.browse = Browse::Contacts;
                self.notice = Some(vec![format!("Hold {} to call", i + 2)]);
                return vec![Action::SetSetting(format!("speed_{}", i + 2), n)];
            }
            Input::Edit if !self.speed[i].is_empty() => {
                self.speed[i].clear();
                return vec![Action::SetSetting(format!("speed_{}", i + 2), String::new())];
            }
            Input::Escape => self.browse = Browse::Contacts,
            _ => {}
        }
        Vec::new()
    }

    pub fn unlocked(&mut self, left: u32) {
        self.notice = Some(if left == 0 {
            vec!["SIM unlocked".into()]
        } else {
            vec!["Wrong PIN".into(), format!("{left} {} left", if left == 1 { "try" } else { "tries" })]
        });
    }

    fn letter_jump(&self, forward: bool) -> usize {
        let c = &self.book.contacts;
        if c.is_empty() {
            return 0;
        }
        let initial = |i: usize| c[i].name.chars().next().map(|ch| ch.to_ascii_uppercase());
        let here = initial(self.contacts_sel);
        if forward {
            (self.contacts_sel + 1..c.len()).find(|&i| initial(i) != here).unwrap_or(c.len() - 1)
        } else {
            let start = (0..self.contacts_sel).rev().find(|&i| initial(i) != here).unwrap_or(0);
            let target = initial(start);
            (0..=start).find(|&i| initial(i) == target).unwrap_or(0)
        }
    }

    pub fn ussd_reply(&mut self, text: String) {
        // A reply that ends in a question or a numbered menu wants an answer.
        let awaiting = text.trim_end().ends_with('?')
            || text
                .lines()
                .any(|l| l.trim_start().starts_with(|c: char| c.is_ascii_digit()) && l.contains(['.', ')', ':']));
        self.browse = Browse::Ussd { text, awaiting };
    }

    pub fn error(&mut self, text: &str) {
        if matches!(self.browse, Browse::Ussd { .. }) {
            self.browse = Browse::Ussd { text: text.to_string(), awaiting: false };
        } else {
            self.notice = Some(wrap_lines(text));
        }
    }
}

/// Typed digits at or past this are a number, not a name: T9 stops matching.
pub const T9_MAX: usize = 6;

/// The touchpad as an absolute keypad. The pad reports raw positions and not their
/// range, so the range is learnt from the strokes seen; until a fair part of the
/// pad has been touched, taps are not mapped and swipes do the work.
#[derive(Clone, Debug, Default)]
pub struct PadMap {
    min: Option<(i32, i32)>,
    max: Option<(i32, i32)>,
}

impl PadMap {
    pub fn observe(&mut self, x: i32, y: i32) {
        let (a, b) = (self.min.unwrap_or((x, y)), self.max.unwrap_or((x, y)));
        self.min = Some((a.0.min(x), a.1.min(y)));
        self.max = Some((b.0.max(x), b.1.max(y)));
    }

    /// The keypad cell under a tap, once the range is known well enough.
    pub fn cell(&self, x: i32, y: i32, span_needed: i32) -> Option<(usize, usize)> {
        let (lo, hi) = (self.min?, self.max?);
        let (w, h) = (hi.0 - lo.0, hi.1 - lo.1);
        if w < span_needed || h < span_needed {
            return None;
        }
        let c = (((x - lo.0) * 3) / (w + 1)).clamp(0, 2) as usize;
        let r = (((y - lo.1) * 4) / (h + 1)).clamp(0, 3) as usize;
        Some((r, c))
    }
}

/// Move a keypad selection, wrapping at the edges.
pub fn step(sel: (usize, usize), input: Input) -> (usize, usize) {
    let (r, c) = sel;
    match input {
        Input::Up => ((r + 3) % 4, c),
        Input::Down => ((r + 1) % 4, c),
        Input::Left => (r, (c + 2) % 3),
        Input::Right => (r, (c + 1) % 3),
        _ => sel,
    }
}

/// Split a message into modal lines of at most ~30 characters.
pub fn wrap_lines(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.len() + 1 + word.len() > 30 {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines.truncate(3);
    lines
}

/// A name the panel can draw, or the number when it cannot. The panel's fonts are
/// ASCII; Latin with accents and Cyrillic transliterate, but Chinese, Arabic,
/// Hebrew, Devanagari and emoji become `?`, and "??" tells nobody who is calling.
pub fn readable(name: String, number_raw: &str, region: &str) -> String {
    let shown = flipctl_app::font::ascii(&name);
    let unknown = shown.chars().filter(|c| *c == '?').count();
    if unknown * 3 >= shown.chars().count().max(1) && !number_raw.is_empty() {
        number::display(number_raw, region)
    } else {
        name
    }
}

#[cfg(test)]
mod readable_tests {
    #[test]
    fn unreadable_names_fall_back_to_the_number() {
        assert_eq!(super::readable("王芳".into(), "+61491570156", "AU"), "0491 570 156");
        assert_eq!(super::readable("José".into(), "+61491570156", "AU"), "José");
        assert_eq!(super::readable("Алексей".into(), "+61491570156", "AU"), "Алексей");
    }
}
