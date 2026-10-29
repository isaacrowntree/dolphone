//! The AT commands phoned sends through Modem.Command, answered as an RM520N-GL
//! would (formats from Quectel's RM5x0N AT manual and UAC application note), with
//! ModemManager's habit of dropping the final OK.

use std::collections::BTreeMap;

pub struct At {
    /// ModemManager runs with --debug; without it, Command is refused.
    pub debug: bool,
    /// The UAC function in AT+QCFG="usbcfg" (saved; counts from the next restart).
    pub uac: bool,
    /// AT+QCFG="ims": 0 carrier profile, 1 on, 2 off.
    pub ims: u8,
    pub autosel: bool,
    /// AT+QPCMV, forgotten at a restart.
    pub qpcmv: String,
    /// Call forwarding by +CCFC reason: (active, number).
    pub ccfc: BTreeMap<u8, (bool, String)>,
    /// AT+CLIR's <n>.
    pub clir: u8,
    pub restarts: u32,
    /// AT+CPCMREG: a SIM7600's serial PCM, on or off.
    pub cpcmreg: u8,
    /// The spare AT port's pty, once one is asked for (crate::atport).
    pub port: Option<String>,
    /// The last command that came on it.
    pub port_last: String,
}

impl Default for At {
    fn default() -> Self {
        Self {
            debug: true,
            uac: false,
            ims: 0,
            autosel: false,
            qpcmv: "0".into(),
            ccfc: BTreeMap::new(),
            clir: 0,
            restarts: 0,
            cpcmreg: 0,
            port: None,
            port_last: String::new(),
        }
    }
}

impl At {
    /// The answer to a command, or None for ERROR.
    pub fn answer(&mut self, cmd: &str) -> Option<String> {
        let c = cmd.trim();
        let upper = c.to_ascii_uppercase();
        let args = |prefix: &str| {
            upper.strip_prefix(prefix).map(|a| a.split(',').map(|x| x.trim().to_string()).collect::<Vec<_>>())
        };
        if upper == "AT+QCFG=\"USBCFG\"" {
            return Some(format!("+QCFG: \"usbcfg\",0x2C7C,0x0801,1,1,1,1,1,0,{}", self.uac as u8));
        }
        if let Some(a) = args("AT+QCFG=\"USBCFG\",") {
            // vid, pid and seven functions; only the last (UAC) is honoured here.
            if a.len() != 9 {
                return None;
            }
            self.uac = a[8] == "1";
            return Some(String::new());
        }
        if upper == "AT+QCFG=\"IMS\"" {
            let registered = self.ims == 1 || (self.ims == 0 && self.autosel);
            return Some(format!("+QCFG: \"ims\",{},{}", self.ims, registered as u8));
        }
        if let Some(a) = args("AT+QCFG=\"IMS\",") {
            self.ims = a.first()?.parse().ok().filter(|v| *v <= 2)?;
            return Some(String::new());
        }
        if upper == "AT+QMBNCFG=\"AUTOSEL\"" {
            return Some(format!("+QMBNCFG: \"AutoSel\",{}", self.autosel as u8));
        }
        if let Some(a) = args("AT+QMBNCFG=\"AUTOSEL\",") {
            self.autosel = a.first()? == "1";
            return Some(String::new());
        }
        if upper == "AT+QPCMV?" {
            return Some(format!("+QPCMV: {}", self.qpcmv));
        }
        if let Some(v) = upper.strip_prefix("AT+QPCMV=") {
            self.qpcmv = v.to_string();
            return Some(String::new());
        }
        if upper == "AT+CFUN=1,1" {
            self.restarts += 1;
            self.qpcmv = "0".into();
            return Some(String::new());
        }
        if let Some(a) = args("AT+CCFC=") {
            let reason: u8 = a.first()?.parse().ok().filter(|r| *r <= 5)?;
            let mode: u8 = a.get(1)?.parse().ok()?;
            let number = a.get(2).map(|n| n.trim_matches('"').to_string()).filter(|n| !n.is_empty());
            // 4 and 5 are groups: all, and the conditional ones.
            let targets: Vec<u8> = match reason {
                4 => vec![0, 1, 2, 3],
                5 => vec![1, 2, 3],
                r => vec![r],
            };
            match mode {
                2 => {
                    return Some(match self.ccfc.get(&reason) {
                        Some((true, n)) => {
                            let ty = if n.starts_with('+') { 145 } else { 129 };
                            format!("+CCFC: 1,1,\"{n}\",{ty}")
                        }
                        _ => "+CCFC: 0,7".into(),
                    })
                }
                3 => {
                    let n = number?;
                    for t in targets {
                        self.ccfc.insert(t, (true, n.clone()));
                    }
                }
                1 | 0 => {
                    for t in targets {
                        if let Some(e) = self.ccfc.get_mut(&t) {
                            e.0 = mode == 1;
                        }
                    }
                }
                4 => {
                    for t in targets {
                        self.ccfc.remove(&t);
                    }
                }
                _ => return None,
            }
            return Some(String::new());
        }
        if let Some(v) = upper.strip_prefix("AT+CPCMREG=") {
            self.cpcmreg = v.parse().ok().filter(|n| *n <= 1)?;
            return Some(String::new());
        }
        if upper == "AT+CLIR?" {
            return Some(format!("+CLIR: {},4", self.clir));
        }
        if let Some(v) = upper.strip_prefix("AT+CLIR=") {
            self.clir = v.parse().ok().filter(|n| *n <= 2)?;
            return Some(String::new());
        }
        if upper.starts_with("AT") {
            return Some(String::new());
        }
        None
    }

    /// One line for the fake's dump.
    pub fn dump(&self) -> String {
        let fwd: Vec<String> =
            self.ccfc.iter().map(|(r, (on, n))| format!("{r}:{}{n}", if *on { "" } else { "off:" })).collect();
        format!(
            "at uac={} ims={} autosel={} qpcmv={} clir={} cpcmreg={} restarts={} forward=[{}] port_last={}\n",
            self.uac as u8,
            self.ims,
            self.autosel as u8,
            self.qpcmv,
            self.clir,
            self.cpcmreg,
            self.restarts,
            fwd.join(" "),
            self.port_last
        )
    }
}

#[cfg(test)]
mod tests {
    use super::At;

    #[test]
    fn forwarding() {
        let mut a = At::default();
        assert_eq!(a.answer("AT+CCFC=0,3,\"0412345678\",129,1").as_deref(), Some(""));
        assert_eq!(a.answer("AT+CCFC=0,2").unwrap(), "+CCFC: 1,1,\"0412345678\",129");
        a.answer("AT+CCFC=4,4,,,1");
        assert_eq!(a.answer("AT+CCFC=0,2").unwrap(), "+CCFC: 0,7");
        assert!(a.answer("nonsense").is_none());
    }
}
