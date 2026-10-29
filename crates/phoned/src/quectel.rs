//! AT commands through ModemManager, and the one-time setup a Quectel modem needs
//! before it can carry calls on a machine like the Flipper One.
//!
//! What the setup checks, and where each step comes from:
//!
//! - **USB audio (UAC).** Call audio reaches Linux only if the modem offers a USB
//!   sound card. `AT+QCFG="usbcfg"` lists the USB functions; the last is UAC, and
//!   setting it to 1 is saved in the modem and takes effect after a restart. A
//!   firmware listing only six functions cannot do it by AT command. (Quectel LTE
//!   Standard UAC Application Note V1.0, §4.1; the same steps are reported on the
//!   RM520N on Quectel's forum.) Checked only when the device profile bridges the
//!   modem's USB audio (`modem_match`): a PinePhone's modem is wired to the codec.
//! - **VoLTE (IMS).** `AT+QCFG="ims"` answers `<setting>,<state>`: 0 leaves it to
//!   the carrier profile, 1 forces it on, 2 off. Most networks have switched 2G and
//!   3G off, so without IMS there are no calls at all.
//! - **The carrier profile (MBN).** `AT+QMBNCFG="AutoSel",1` lets the modem pick its
//!   carrier profile from the SIM, which is what turns IMS on for that carrier.
//!   (Both from Quectel's forum, where staff give these steps for the EC25 and the
//!   RM520N-GL; neither command is in the RM5x0N AT manual V1.0.)
//! - **Where call audio goes**, `AT+QPCMV=1,2`, is forgotten at every restart (UAC
//!   note §3), so it is not a setup step: the profile's `modem_at` sends it each
//!   time the modem appears.
//!
//! The commands go through ModemManager ([`mm::at`]), which passes them only in
//! debug mode.

use zbus::Connection;

use crate::mm;

/// One line of the setup: what, how it stands ("ok", "change", "changed",
/// "unsupported", "error"), and in words.
pub type Step = (String, String, String);

fn step(name: &str, state: &str, detail: impl Into<String>) -> Step {
    (name.into(), state.into(), detail.into())
}

/// Reads a query's answer: the step, and the command that would change it.
type Judge = fn(&str) -> (Step, Option<String>);

pub fn is_quectel(manufacturer: &str, model: &str) -> bool {
    let both = format!("{manufacturer} {model}").to_lowercase();
    both.contains("quectel")
        || ["rm5", "rg5", "eg25", "ec2", "em1", "eg1"].iter().any(|p| model.to_lowercase().starts_with(p))
}

/// The fields after `+QCFG: "<name>",` in an answer.
fn qcfg_fields<'a>(answer: &'a str, name: &str) -> Option<Vec<&'a str>> {
    let line = answer.lines().map(str::trim).find(|l| {
        l.strip_prefix("+QCFG:").is_some_and(|r| r.trim_start().to_lowercase().starts_with(&format!("\"{name}\"")))
    })?;
    let (_, rest) = line.split_once(',')?;
    Some(rest.split(',').map(str::trim).collect())
}

/// The USB-audio step from a `+QCFG: "usbcfg",<vid>,<pid>,<f1>,…` answer, and the
/// command that turns it on.
pub fn uac_step(answer: &str) -> (Step, Option<String>) {
    let Some(f) = qcfg_fields(answer, "usbcfg").filter(|f| f.len() >= 3) else {
        return (step("usb audio", "error", format!("unexpected answer: {}", answer.trim())), None);
    };
    let functions = &f[2..];
    if functions.len() < 7 {
        return (step("usb audio", "unsupported", "this firmware cannot turn on USB audio by AT command"), None);
    }
    if functions.last() == Some(&"1") {
        return (step("usb audio", "ok", "the modem offers a USB sound card for calls"), None);
    }
    let mut on: Vec<&str> = f.clone();
    *on.last_mut().unwrap_or(&mut "1") = "1";
    let set = format!("AT+QCFG=\"usbcfg\",{}", on.join(","));
    (step("usb audio", "change", "turn on the modem's USB sound card (saved in the modem; it restarts)"), Some(set))
}

/// The VoLTE step from a `+QCFG: "ims",<setting>[,<state>]` answer.
pub fn ims_step(answer: &str) -> (Step, Option<String>) {
    let Some(f) = qcfg_fields(answer, "ims") else {
        return (step("volte", "error", format!("unexpected answer: {}", answer.trim())), None);
    };
    let registered = f.get(1) == Some(&"1");
    match f.first().copied() {
        Some("1") => (
            step(
                "volte",
                "ok",
                if registered { "on, and registered" } else { "on (not registered with the network yet)" },
            ),
            None,
        ),
        // 0 leaves it to the carrier profile; with it already working, keep it.
        Some("0") if registered => (step("volte", "ok", "on, as the carrier profile says"), None),
        _ => (
            step("volte", "change", "turn on VoLTE, needed for calls where 2G and 3G are off (it restarts)"),
            Some("AT+QCFG=\"ims\",1".into()),
        ),
    }
}

/// The carrier-profile step from a `+QMBNCFG: "AutoSel",<0|1>` answer.
pub fn autosel_step(answer: &str) -> (Step, Option<String>) {
    let v = answer
        .lines()
        .find_map(|l| l.trim().strip_prefix("+QMBNCFG:"))
        .and_then(|r| r.split(',').nth(1))
        .map(str::trim);
    match v {
        Some("1") => (step("carrier profile", "ok", "chosen from the SIM"), None),
        Some(_) => (
            step("carrier profile", "change", "let the modem choose its carrier profile from the SIM (it restarts)"),
            Some("AT+QMBNCFG=\"AutoSel\",1".into()),
        ),
        None => (step("carrier profile", "error", format!("unexpected answer: {}", answer.trim())), None),
    }
}

/// Check the setup, and with `apply` make the changes (restarting the modem when a
/// saved setting changed). `usb_audio` is whether this machine bridges the modem's
/// USB audio.
pub async fn setup(system: &Connection, apply: bool, usb_audio: bool) -> Vec<Step> {
    let Some(found) = mm::find(system).await else {
        return vec![step("modem", "error", "no modem")];
    };
    let path = found.path.clone();
    let (manufacturer, model) = match mm::ModemProxy::builder(system).path(path.clone()).build().await {
        Ok(m) => (m.manufacturer().await.unwrap_or_default(), m.model().await.unwrap_or_default()),
        Err(_) => Default::default(),
    };
    if !is_quectel(&manufacturer, &model) {
        return vec![step(
            "modem",
            "unsupported",
            format!("only Quectel modems have a setup here; this is {manufacturer} {model}").trim().to_string(),
        )];
    }
    let mut checks: Vec<(&str, Judge)> = Vec::new();
    if usb_audio {
        checks.push(("AT+QCFG=\"usbcfg\"", uac_step));
    }
    checks.push(("AT+QCFG=\"ims\"", ims_step));
    checks.push(("AT+QMBNCFG=\"AutoSel\"", autosel_step));
    let mut out = vec![step("modem", "ok", format!("{manufacturer} {model}"))];
    let mut changed = false;
    for (query, judge) in checks {
        let answer = match mm::at(system, &path, query).await {
            Ok(a) => a,
            Err(e) => {
                out.push(step("modem", "error", mm::explain_at(&e)));
                return out;
            }
        };
        let (mut s, set) = judge(&answer);
        if let (true, Some(cmd)) = (apply, set) {
            match mm::at(system, &path, &cmd).await {
                Ok(_) => {
                    s.1 = "changed".into();
                    changed = true;
                }
                Err(e) => {
                    s.1 = "error".into();
                    s.2 = mm::explain_at(&e);
                }
            }
        }
        out.push(s);
    }
    if changed {
        // The saved settings take effect when the modem starts again; it comes
        // back by itself, and phoned follows it as it does after any reset.
        match mm::at(system, &path, "AT+CFUN=1,1").await {
            Ok(_) => out.push(step("restart", "changed", "the modem is restarting; it is back in about half a minute")),
            Err(e) => out.push(step("restart", "error", format!("restart it by hand: {}", mm::explain_at(&e)))),
        }
    }
    out
}

/// Send the profile's start-up commands to a modem that has just appeared.
pub async fn on_appeared(system: &Connection, modem: &zbus::wire::OwnedObjectPath, commands: &[String]) {
    for cmd in commands {
        if let Err(e) = mm::at(system, modem, cmd).await {
            tracing::warn!(cmd, "the modem did not take a start-up command: {}", mm::explain_at(&e));
            if mm::explain_at(&e).contains("debug mode") {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usb_audio() {
        // The UAC note's example, before and after.
        let (s, set) = uac_step("+QCFG: \"USBCFG\",0x2C7C,0x0125,1,1,1,1,1,1,0");
        assert_eq!(s.1, "change");
        assert_eq!(set.as_deref(), Some("AT+QCFG=\"usbcfg\",0x2C7C,0x0125,1,1,1,1,1,1,1"));
        assert_eq!(uac_step("+QCFG: \"usbcfg\",0x2C7C,0x0801,1,1,1,1,1,0,1").0 .1, "ok");
        assert_eq!(uac_step("+QCFG: \"usbcfg\",0x2C7C,0x0125,1,1,1,1,1,1").0 .1, "unsupported");
        assert_eq!(uac_step("ERROR").0 .1, "error");
    }

    #[test]
    fn volte_and_profile() {
        assert_eq!(ims_step("+QCFG: \"ims\",1,1").0 .2, "on, and registered");
        assert_eq!(ims_step("+QCFG: \"ims\",0,1").0 .1, "ok");
        assert_eq!(ims_step("+QCFG: \"ims\",0,0").1.as_deref(), Some("AT+QCFG=\"ims\",1"));
        assert_eq!(autosel_step("+QMBNCFG: \"AutoSel\",1").0 .1, "ok");
        assert_eq!(autosel_step("+QMBNCFG: \"AutoSel\",0").1.as_deref(), Some("AT+QMBNCFG=\"AutoSel\",1"));
        assert!(is_quectel("Quectel", "RM520N-GL"));
        assert!(is_quectel("", "EG25"));
        assert!(!is_quectel("SIMCOM INCORPORATED", "SIMCOM_SIM7600G-H"));
    }

    #[test]
    fn debug_mode_is_explained() {
        let e = zbus::Error::Failure(
            "org.freedesktop.ModemManager1.Error.Core.Unauthorized: Cannot send AT command to modem: operation only allowed in debug mode".into(),
        );
        assert!(mm::explain_at(&e).contains("dolphone-modem-access on"));
    }
}
