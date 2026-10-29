//! Phone Settings, as data: the pages, their rows, and what each key does.
//!
//! Rows follow FlipCTL's settings conventions: an ON/OFF row toggles with Left,
//! Right or Run; an adjustable row ("< Ring >") steps with Left and Right; a row
//! with a chevron opens a page with Run or Ok. Escape goes up a page.

use phone_api::{BtDevice, ModemStatus, RouteInfo};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Main,
    Audio,
    Bluetooth,
    Blocked,
    Modem,
    About,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    SetRinger(String),
    Set(&'static str, String),
    SetRoute(u8),
    BtPower(bool),
    BtScan,
    BtPair(String),
    BtConnect(String, bool),
    BtForget(String),
    Unblock(String),
    Wipe,
    Refresh(Page),
    Quit,
}

pub const RINGER: [&str; 3] = ["ring", "vibrate", "silent"];
pub const REGIONS: [&str; 12] = ["AU", "NZ", "GB", "IE", "US", "CA", "DE", "FR", "NL", "IN", "SG", "JP"];

/// A main-page row: what it is, so keys and drawing agree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MainRow {
    Ringer,
    Vibrate,
    Audio,
    Bluetooth,
    Blocked,
    Private,
    DeleteFromSim,
    Region,
    Modem,
    Wipe,
    About,
}

pub const MAIN: [MainRow; 11] = [
    MainRow::Ringer,
    MainRow::Vibrate,
    MainRow::Audio,
    MainRow::Bluetooth,
    MainRow::Blocked,
    MainRow::Private,
    MainRow::DeleteFromSim,
    MainRow::Region,
    MainRow::Modem,
    MainRow::Wipe,
    MainRow::About,
];

pub struct Model {
    pub page: Page,
    pub sel: [usize; 6],
    pub ringer: String,
    pub vibrate: bool,
    pub private: bool,
    pub delete_from_sim: bool,
    pub region: String,
    pub routes: Vec<RouteInfo>,
    pub bt_available: bool,
    pub bt_powered: bool,
    pub devices: Vec<BtDevice>,
    pub blocked: Vec<String>,
    pub status: Option<ModemStatus>,
    /// A modal, and what Run does on it.
    pub modal: Option<(Vec<String>, Option<Action>)>,
    pub connected: bool,
}

impl Default for Model {
    fn default() -> Self {
        Self {
            page: Page::Main,
            sel: [0; 6],
            ringer: "ring".into(),
            vibrate: true,
            private: false,
            delete_from_sim: true,
            region: "AU".into(),
            routes: Vec::new(),
            bt_available: false,
            bt_powered: false,
            devices: Vec::new(),
            blocked: Vec::new(),
            status: None,
            modal: None,
            connected: false,
        }
    }
}

fn page_index(p: Page) -> usize {
    p as usize
}

impl Model {
    pub fn sel(&self) -> usize {
        self.sel[page_index(self.page)]
    }

    fn set_sel(&mut self, v: usize) {
        let i = page_index(self.page);
        self.sel[i] = v;
    }

    /// Rows on the current page, for bounding the selection.
    pub fn rows(&self) -> usize {
        match self.page {
            Page::Main => MAIN.len(),
            Page::Audio => self.routes.len().max(1),
            // Power, scan, then the devices.
            Page::Bluetooth => 2 + self.devices.len(),
            Page::Blocked => self.blocked.len().max(1),
            Page::Modem | Page::About => 1,
        }
    }

    pub fn key(&mut self, input: Input) -> Vec<Action> {
        if let Some((_, then)) = self.modal.take() {
            return match (input, then) {
                (Input::Run, Some(a)) => vec![a],
                _ => Vec::new(),
            };
        }
        let n = self.rows();
        match input {
            Input::Up => {
                self.set_sel(self.sel().saturating_sub(1));
                return Vec::new();
            }
            Input::Down => {
                self.set_sel((self.sel() + 1).min(n.saturating_sub(1)));
                return Vec::new();
            }
            Input::Escape => {
                if self.page == Page::Main {
                    return vec![Action::Quit];
                }
                self.page = Page::Main;
                return vec![Action::Refresh(Page::Main)];
            }
            _ => {}
        }
        match self.page {
            Page::Main => self.key_main(input),
            Page::Audio => match input {
                Input::Run | Input::Ok => match self.routes.get(self.sel()) {
                    Some(r) if r.available => vec![Action::SetRoute(r.route)],
                    Some(r) => {
                        self.modal = Some((vec![format!("{} is not connected.", r.name)], None));
                        Vec::new()
                    }
                    None => Vec::new(),
                },
                _ => Vec::new(),
            },
            Page::Bluetooth => self.key_bluetooth(input),
            Page::Blocked => match input {
                Input::Run | Input::Ok | Input::Edit => match self.blocked.get(self.sel()) {
                    Some(n) => {
                        self.modal = Some((vec![format!("Unblock {n}?")], Some(Action::Unblock(n.clone()))));
                        Vec::new()
                    }
                    None => Vec::new(),
                },
                _ => Vec::new(),
            },
            Page::Modem | Page::About => Vec::new(),
        }
    }

    fn key_main(&mut self, input: Input) -> Vec<Action> {
        let row = MAIN[self.sel().min(MAIN.len() - 1)];
        let toggle = matches!(input, Input::Left | Input::Right | Input::Run | Input::Ok);
        let step = |list: &[&str], cur: &str, fwd: bool| -> String {
            let i = list.iter().position(|x| *x == cur).unwrap_or(0);
            let j = if fwd { (i + 1) % list.len() } else { (i + list.len() - 1) % list.len() };
            list[j].to_string()
        };
        let fwd = !matches!(input, Input::Left);
        match row {
            MainRow::Ringer if toggle => {
                self.ringer = step(&RINGER, &self.ringer, fwd);
                vec![Action::SetRinger(self.ringer.clone())]
            }
            MainRow::Vibrate if toggle => {
                self.vibrate = !self.vibrate;
                vec![Action::Set("vibrate", self.vibrate.to_string())]
            }
            MainRow::Private if toggle => {
                self.private = !self.private;
                vec![Action::Set("private_notify", self.private.to_string())]
            }
            MainRow::DeleteFromSim if toggle => {
                self.delete_from_sim = !self.delete_from_sim;
                vec![Action::Set("delete_from_sim", self.delete_from_sim.to_string())]
            }
            MainRow::Region if toggle => {
                self.region = step(&REGIONS, &self.region, fwd);
                vec![Action::Set("region", self.region.clone())]
            }
            MainRow::Audio | MainRow::Bluetooth | MainRow::Blocked | MainRow::Modem | MainRow::About
                if matches!(input, Input::Run | Input::Ok | Input::Right) =>
            {
                self.page = match row {
                    MainRow::Audio => Page::Audio,
                    MainRow::Bluetooth => Page::Bluetooth,
                    MainRow::Blocked => Page::Blocked,
                    MainRow::Modem => Page::Modem,
                    _ => Page::About,
                };
                self.set_sel(0);
                vec![Action::Refresh(self.page)]
            }
            MainRow::Wipe if matches!(input, Input::Run | Input::Ok) => {
                self.modal = Some((
                    vec![
                        "Delete all texts, the call".into(),
                        "log and blocked numbers?".into(),
                        "This cannot be undone.".into(),
                    ],
                    Some(Action::Wipe),
                ));
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn key_bluetooth(&mut self, input: Input) -> Vec<Action> {
        let sel = self.sel();
        match (sel, input) {
            (0, Input::Left | Input::Right | Input::Run | Input::Ok) if self.bt_available => {
                self.bt_powered = !self.bt_powered;
                vec![Action::BtPower(self.bt_powered)]
            }
            (1, Input::Run | Input::Ok) if self.bt_powered => vec![Action::BtScan],
            (i, Input::Run | Input::Ok) if i >= 2 => {
                let Some(d) = self.devices.get(i - 2) else { return Vec::new() };
                if !d.paired {
                    vec![Action::BtPair(d.address.clone())]
                } else {
                    vec![Action::BtConnect(d.address.clone(), !d.connected)]
                }
            }
            (i, Input::Edit) if i >= 2 => {
                let Some(d) = self.devices.get(i - 2) else { return Vec::new() };
                if d.paired {
                    self.modal = Some((vec![format!("Forget {}?", d.name)], Some(Action::BtForget(d.address.clone()))));
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    pub fn passkey(&mut self, name: &str, key: u32) {
        self.modal =
            Some((vec![format!("Pairing with {name}"), format!("Code {key:06}"), "Check it matches.".into()], None));
    }
}

#[cfg(test)]
// Tests build a model and then set the few fields each one is about.
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;

    #[test]
    fn ringer_steps_both_ways() {
        let mut m = Model::default();
        assert_eq!(m.key(Input::Right), vec![Action::SetRinger("vibrate".into())]);
        assert_eq!(m.key(Input::Right), vec![Action::SetRinger("silent".into())]);
        assert_eq!(m.key(Input::Left), vec![Action::SetRinger("vibrate".into())]);
    }

    #[test]
    fn toggles_and_pages() {
        let mut m = Model::default();
        m.key(Input::Down);
        assert_eq!(m.key(Input::Run), vec![Action::Set("vibrate", "false".into())]);
        m.key(Input::Down);
        assert_eq!(m.key(Input::Run), vec![Action::Refresh(Page::Audio)]);
        assert_eq!(m.page, Page::Audio);
        assert_eq!(m.key(Input::Escape), vec![Action::Refresh(Page::Main)]);
        assert_eq!(m.key(Input::Escape), vec![Action::Quit]);
    }

    #[test]
    fn wipe_asks_first() {
        let mut m = Model::default();
        for _ in 0..9 {
            m.key(Input::Down);
        }
        assert!(m.key(Input::Run).is_empty());
        assert!(m.modal.is_some());
        assert_eq!(m.key(Input::Run), vec![Action::Wipe]);
        // Escape on the modal cancels.
        m.key(Input::Run);
        assert!(m.key(Input::Escape).is_empty());
    }

    #[test]
    fn bluetooth_pair_then_connect() {
        let mut m = Model::default();
        m.page = Page::Bluetooth;
        m.bt_available = true;
        m.bt_powered = true;
        m.devices = vec![BtDevice {
            address: "AA".into(),
            name: "Buds".into(),
            paired: false,
            connected: false,
            headset: true,
            speaker: true,
        }];
        m.key(Input::Down);
        m.key(Input::Down);
        assert_eq!(m.key(Input::Run), vec![Action::BtPair("AA".into())]);
        m.devices[0].paired = true;
        assert_eq!(m.key(Input::Run), vec![Action::BtConnect("AA".into(), true)]);
    }
}
