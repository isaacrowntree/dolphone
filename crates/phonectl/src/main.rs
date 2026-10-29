//! phonectl: Dolphone from a shell.
//!
//! Everything a front end can do, as commands, for scripts and for testing; plus
//! `phonectl fake ...`, which plays the other end of the network against fake-mm.
//! A call or text started here is an outside request like any script's: phoned
//! holds it until it is confirmed on the device (`phonectl pending`, then the
//! Dialer's confirm screen).

use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use phone_api::*;
use phone_core::model::{CallState, MessageState, Outcome, Route};

const USAGE: &str = "usage: phonectl [--json] <command>

--json prints lists and status as JSON, and watch as one JSON object per line.

phone
  status                      modem, signal, operator
  dial <number>               place a call (waits for confirmation on the device)
  answer [id] | hangup [id]   the ringing / live call by default
  swap | merge | mute | unmute | silence
  dtmf <id> <tones>
  calls | recents [n]
  ussd <code>                 e.g. *100#, or **21*number# to forward calls
                              (Dolphone's dialer only, refused here)
  modem setup [apply]         check the modem's one-time setup (USB audio, VoLTE,
                              carrier profile); apply saves it and restarts the modem

messages
  send [--wait [s]] <to> <text...>
                              (waits for confirmation on the device; --wait
                              exits once it is sent, or 1 if it failed)
  mms <to[,to...]> [--file <f>]... [text...]
                              a picture message or a group text (needs mmsd-tng);
                              to several numbers, send works too
  retry <id>                  send a failed text again (Dolphone's apps only)
  threads | thread <peer> | read <peer>
  otp [--from <n>] [--wait <s>]
                              wait for a text with a one-time code, print the code
  alerts [n]                  emergency alerts received
  search <words...>           texts with all of those words
  export > texts.xml          every text, as an SMS Backup & Restore file (Android)

contacts (vCard files in ~/.local/share/dolphone/contacts; changes from here
          wait for a press on the device, like calls and texts)
  contacts [list]               reading needs a grant, as messages do
  contacts add <name> <number> [kind]
  contacts edit <id> <name> <number> [kind]
  contacts remove <id>
  contacts import <file.vcf | ->     an export from Google Contacts, iCloud, a phone
                                     (names and numbers only; matches merge)

device (what Dolphone takes this machine to be; see docs/DEVICES.md)
  device                      the profile in use, and why
  device list                 every profile known
  device use <id|auto>        choose one, or detect again

KDE Connect (a laptop's KDE Connect or GSConnect sees the phone's calls and texts)
  kdeconnect on | off         listen on the local network (UDP and TCP 1716)
  kdeconnect [devices]        laptops connected, and whether paired
  kdeconnect unpair <id>

modem
  cells [n]                   the networks the modem has been on (needs a read grant)
  unlock <pin>                the SIM's PIN (Dolphone's apps only)

grants (texting without asking each time, approved once on the device)
  grant <numbers> [per-hour] [days]   numbers comma separated, 0491* for a prefix
  grants | revoke <id> | audit [n]

confirmations
  pending | deny <id>         (allowing is done on the device or in the desktop app;
                              confirm <id> works only from Dolphone's own apps)

reading messages and calls needs a grant: Phone Settings, read_grants = phonectl

audio and Bluetooth
  routes | route <speaker|headset|bluetooth> | ringer [ring|vibrate|silent]
  bt [devices|scan|pair <addr>|connect <addr>|disconnect <addr>|forget <addr>|on|off]

settings
  get <key> | set <key> <value> | block <n> | unblock <n> | blocked | wipe

watch                         print every event as it happens

fake modem (the far end, against fake-mm)
  fake ring <number> | fake sms <from> <text...> | fake answer | fake hangup | fake busy
  fake signal <0-100> <5G|LTE|3G|none> | fake register <on|off> [operator] | fake roam <on|off>
  fake autoanswer <ms> | fake sendfails <on|off> | fake atdebug <on|off> | fake ussd <reply...> | fake dump
  fake broadcast <channel> <text...> | fake lock <on|off> | fake enable <on|off>
  fake mms <from> [--to <n,n>] [--file <f>]... [text...] | fake mms dump | fake mms fails <on|off>
                              (against fake-mmsd, mmsd-tng's stand-in)
  fake uplink <on|off>        the network link (fake-mm stands in for NetworkManager)

fake SIP far end (a second baresip, with ctrl_tcp on 127.0.0.1:4444; see e2e.sh)
  fake sip ring <your sip address> | fake sip answer | fake sip hangup
  fake sip autoanswer <on|off> | fake sip message <text...>";

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let json = args.iter().any(|a| a == "--json");
    let words: Vec<&str> = args.iter().map(String::as_str).filter(|a| *a != "--json").collect();
    if words.is_empty() || matches!(words[0], "-h" | "--help" | "help") {
        println!("{USAGE}");
        return Ok(());
    }
    if words[0] == "fake" {
        return fake(&words[1..]).await;
    }
    let conn = zbus::Connection::session().await.context("session bus")?;
    run(&conn, &words, json).await.map_err(|e| {
        if let Some(zbus::Error::MethodError(name, msg, _)) = e.downcast_ref::<zbus::Error>() {
            if name.as_str() == "org.freedesktop.DBus.Error.ServiceUnknown" {
                return anyhow::anyhow!("phoned is not running");
            }
            return anyhow::anyhow!("{}", msg.clone().unwrap_or_else(|| name.to_string()));
        }
        e
    })
}

fn state_word(s: MessageState) -> &'static str {
    match s {
        MessageState::Received => "received",
        MessageState::Sending => "sending",
        MessageState::Sent => "sent",
        MessageState::Failed => "failed",
        MessageState::Delivered => "delivered",
    }
}

/// Print as JSON.
fn out<T: serde::Serialize>(v: &T) -> Result<()> {
    println!("{}", serde_json::to_string(v)?);
    Ok(())
}

async fn run(conn: &zbus::Connection, w: &[&str], json: bool) -> Result<()> {
    let calls = CallsProxy::new(conn).await?;
    let messages = MessagesProxy::new(conn).await?;
    let region = SettingsProxy::new(conn).await?.get(keys::REGION.0).await.unwrap_or_else(|_| "AU".into());
    let show = |n: &str| phone_core::number::display_peer(n, &region);
    match w {
        ["status"] if json => {
            let s = ModemProxy::new(conn).await?.status().await?;
            let v = serde_json::json!({
                "modem": s,
                "unread": messages.unread().await?,
                "missed": calls.missed().await?,
            });
            out(&v)?;
        }
        ["status"] => {
            let s = ModemProxy::new(conn).await?.status().await?;
            if !s.present {
                println!("no modem");
            } else {
                println!(
                    "{} {} {}% {}{}{}",
                    if s.operator.is_empty() { "(no operator)" } else { &s.operator },
                    s.tech,
                    s.quality,
                    if s.registered { "registered" } else { "not registered" },
                    if s.roaming { ", roaming" } else { "" },
                    if s.voice { "" } else { ", no voice" },
                );
                if !s.sip.is_empty() {
                    println!("SIP {}", s.sip);
                }
                if s.state == "locked" {
                    println!("SIM locked: phonectl unlock <pin>, or enter it on the Dialer");
                }
                if !s.own_number.is_empty() {
                    println!("number {}", show(&s.own_number));
                }
            }
            println!("unread {}  missed {}", messages.unread().await?, calls.missed().await?);
        }
        ["dial", rest @ ..] if !rest.is_empty() => placed("call", calls.dial(&rest.join(" ")).await?),
        ["answer"] => calls.answer(pick(&calls, true).await?).await?,
        ["answer", id] => calls.answer(id.parse()?).await?,
        ["hangup"] => calls.hangup(pick(&calls, false).await?).await?,
        ["hangup", id] => calls.hangup(id.parse()?).await?,
        ["swap"] => calls.swap().await?,
        ["merge"] => calls.merge().await?,
        ["mute"] => calls.set_muted(true).await?,
        ["unmute"] => calls.set_muted(false).await?,
        ["silence"] => calls.silence().await?,
        ["dtmf", id, tones] => calls.send_dtmf(id.parse()?, tones).await?,
        ["calls"] if json => out(&calls.calls().await?)?,
        ["recents"] | ["recents", _] if json => {
            out(&calls.recents(w.get(1).and_then(|n| n.parse().ok()).unwrap_or(20)).await?)?
        }
        ["calls"] => {
            for c in calls.calls().await? {
                println!("{:>3}  {:<14} {}", c.id, CallState::from_u8(c.state).label(), show(&c.number));
            }
        }
        ["recents"] | ["recents", _] => {
            let n = w.get(1).and_then(|n| n.parse().ok()).unwrap_or(20);
            for r in calls.recents(n).await? {
                let what = match (Outcome::from_u8(r.outcome), r.direction) {
                    (Outcome::Missed, _) => "missed",
                    (Outcome::Declined, _) => "declined",
                    (Outcome::Blocked, _) => "blocked",
                    (Outcome::Cancelled, _) => "cancelled",
                    (Outcome::Failed, _) => "failed",
                    (Outcome::Silenced, _) => "silenced",
                    (Outcome::Answered, 0) => "in",
                    (Outcome::Answered, _) => "out",
                };
                println!("{:<10} {:<16} {}s", what, show(&r.peer), r.duration);
            }
        }
        ["ussd", code] => println!("{}", ModemProxy::new(conn).await?.ussd(code).await?),
        ["modem", "setup"] => print_steps(&ModemProxy::new(conn).await?.setup().await?, json),
        ["modem", "setup", "apply"] => {
            let (p, steps) = ModemProxy::new(conn).await?.apply_setup().await?;
            if p.pending != 0 {
                placed("modem setup", p);
            } else {
                print_steps(&steps, json);
            }
        }
        ["send", "--wait", rest @ ..] => {
            let (secs, rest) = match rest {
                [n, more @ ..] if n.parse::<u64>().is_ok() && more.len() >= 2 => (n.parse()?, more),
                _ => (60, rest),
            };
            let [to, text @ ..] = rest else { bail!("send --wait [seconds] <to> <text...>") };
            if text.is_empty() {
                bail!("send --wait [seconds] <to> <text...>");
            }
            send_and_wait(conn, to, &text.join(" "), secs, json).await?
        }
        ["send", to, text @ ..] if !text.is_empty() => {
            let p = messages.send(to, &text.join(" ")).await?;
            if json {
                out(&p)?
            } else {
                placed("text", p)
            }
        }
        ["mms", to, rest @ ..] => {
            let mut files = Vec::new();
            let mut text = Vec::new();
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match *a {
                    "--file" => {
                        files.push(std::fs::canonicalize(it.next().context("--file <path>")?)?.display().to_string())
                    }
                    t => text.push(t),
                }
            }
            let to: Vec<&str> = to.split(',').collect();
            let files: Vec<&str> = files.iter().map(String::as_str).collect();
            let p = messages.send_mms(&to, &text.join(" "), &files).await?;
            if json {
                out(&p)?
            } else {
                placed("message", p)
            }
        }
        ["retry", id] => messages.retry(id.parse()?).await?,
        ["otp", rest @ ..] => otp(conn, rest, &region, json).await?,
        ["search", words @ ..] if !words.is_empty() => {
            let found = messages.search(&words.join(" "), 50).await?;
            if json {
                return out(&found);
            }
            for m in found {
                let who = if m.direction == 0 { show(&m.peer) } else { format!("you to {}", show(&m.peer)) };
                println!(
                    "{}  {who}: {}",
                    phone_core::when::short(m.at, phone_core::now()),
                    m.body.lines().next().unwrap_or("")
                );
            }
        }
        ["export"] => print!("{}", messages.export().await?),
        ["alerts"] | ["alerts", _] => {
            let list = messages.alerts(w.get(1).and_then(|n| n.parse().ok()).unwrap_or(20)).await?;
            if json {
                out(&list)?;
            }
            for a in list.iter().filter(|_| !json) {
                let level = phone_core::model::AlertLevel::from_u8(a.level).label();
                println!(
                    "{}  {level} (channel {}): {}",
                    phone_core::when::short(a.at, phone_core::now()),
                    a.channel,
                    a.text
                );
            }
        }
        ["contacts", rest @ ..] => contacts(conn, rest, &region, json).await?,
        ["cells"] | ["cells", _] => {
            let list = ModemProxy::new(conn).await?.cells(w.get(1).and_then(|n| n.parse().ok()).unwrap_or(20)).await?;
            if json {
                return out(&list);
            }
            for (at, tech, op, reg) in list {
                let state = if reg { "" } else { "  (not registered)" };
                println!("{}  {tech:<4} {op}{state}", phone_core::when::short(at, phone_core::now()));
            }
        }
        ["kdeconnect", "on"] => SettingsProxy::new(conn).await?.set("kdeconnect", "true").await?,
        ["kdeconnect", "off"] => SettingsProxy::new(conn).await?.set("kdeconnect", "false").await?,
        ["kdeconnect"] | ["kdeconnect", "devices"] => {
            let list = SettingsProxy::new(conn).await?.kdeconnect_devices().await?;
            if json {
                return out(&list);
            }
            for (id, name, paired) in list {
                println!("{id}  {:<9} {name}", if paired { "paired" } else { "unpaired" });
            }
        }
        ["kdeconnect", "unpair", id] => SettingsProxy::new(conn).await?.kdeconnect_unpair(id).await?,
        ["device"] => {
            let d = SettingsProxy::new(conn).await?.device().await?;
            if json {
                return out(&d);
            }
            println!("{} ({}, {})", d.name, d.id, d.status);
            println!("why     {}", d.why);
            println!("apps    {}", d.front_ends.join(", "));
            println!("source  {}", d.source);
            if !d.notes.is_empty() {
                println!("notes   {}", d.notes);
            }
        }
        ["device", "list"] => {
            let all = SettingsProxy::new(conn).await?.devices().await?;
            if json {
                return out(&all);
            }
            for d in all {
                println!("{:<22} {:<10} {}", d.id, d.status, d.name);
            }
        }
        ["device", "use", id] => SettingsProxy::new(conn).await?.set("device_profile", id).await?,
        ["unlock", pin] => match ModemProxy::new(conn).await?.unlock(pin).await? {
            0 => println!("SIM unlocked"),
            n => bail!("wrong PIN; {n} {} left", if n == 1 { "try" } else { "tries" }),
        },
        ["grant", numbers, rest @ ..] if rest.len() <= 2 => {
            let per_hour = rest.first().map(|n| n.parse()).transpose()?.unwrap_or(10);
            let days = rest.get(1).map(|n| n.parse()).transpose()?.unwrap_or(30);
            let p = SettingsProxy::new(conn).await?.request_grant(numbers, per_hour, days).await?;
            if json {
                out(&p)?
            } else {
                placed("grant", p)
            }
        }
        ["grants"] => {
            let list = SettingsProxy::new(conn).await?.grants().await?;
            if json {
                out(&list)?;
            }
            for g in list.iter().filter(|_| !json) {
                let until =
                    if g.until == 0 { "no end".into() } else { phone_core::when::short(g.until, phone_core::now()) };
                println!("{:>3}  {:<16} {} at {}/h, until {until}", g.id, g.program, g.numbers, g.per_hour);
            }
        }
        ["revoke", id] => SettingsProxy::new(conn).await?.revoke_grant(id.parse()?).await?,
        ["audit"] | ["audit", _] => {
            let list =
                SettingsProxy::new(conn).await?.audit(w.get(1).and_then(|n| n.parse().ok()).unwrap_or(50)).await?;
            if json {
                out(&list)?;
            }
            for a in list.iter().filter(|_| !json) {
                println!(
                    "{}  {:<16} {:<8} {:<14} {}",
                    phone_core::when::short(a.at, phone_core::now()),
                    a.program,
                    a.action,
                    a.target,
                    a.result
                );
            }
        }
        ["threads"] if json => out(&messages.threads().await?)?,
        ["thread", peer] if json => {
            out(&messages.conversation(&phone_core::number::canonical_peer(peer, &region)).await?)?
        }
        ["pending"] if json => out(&ConfirmProxy::new(conn).await?.pending().await?)?,
        ["routes"] if json => out(&AudioProxy::new(conn).await?.routes().await?)?,
        ["blocked"] if json => out(&SettingsProxy::new(conn).await?.blocked().await?)?,
        ["threads"] => {
            for t in messages.threads().await? {
                let badge = if t.unread > 0 { format!("({}) ", t.unread) } else { String::new() };
                println!("{badge}{:<16} {}", show(&t.peer), t.last_body.lines().next().unwrap_or(""));
            }
        }
        ["thread", peer] | ["read", peer] => {
            let peer = phone_core::number::canonical_peer(peer, &region);
            for m in messages.conversation(&peer).await? {
                let who = if m.direction == 0 { "<" } else { ">" };
                let sim = if m.sim.is_empty() { String::new() } else { format!(" [SIM …{}]", m.sim) };
                let state = match MessageState::from_u8(m.state) {
                    MessageState::Sending => " (sending)",
                    MessageState::Failed => " (failed)",
                    MessageState::Delivered => " (delivered)",
                    _ => "",
                };
                let parts: String = m
                    .parts
                    .iter()
                    .map(|(mime, path)| format!(" [{}: {path}]", phone_core::model::kind_of(mime)))
                    .collect();
                println!("{who} {}{parts}{state}{sim}", m.body);
            }
            if w[0] == "read" {
                messages.mark_read(&peer).await?;
            }
        }
        ["pending"] => {
            for r in ConfirmProxy::new(conn).await?.pending().await? {
                println!("{:>3}  {} {} by {} ({})", r.id, r.kind, show(&r.target), r.requester, r.reason);
            }
        }
        ["confirm", id] => placed("request", ConfirmProxy::new(conn).await?.confirm(id.parse()?).await?),
        ["deny", id] => ConfirmProxy::new(conn).await?.deny(id.parse()?).await?,
        ["routes"] => {
            for r in AudioProxy::new(conn).await?.routes().await? {
                let mark = if r.active {
                    "*"
                } else if r.available {
                    " "
                } else {
                    "-"
                };
                println!("{mark} {}", r.name);
            }
        }
        ["route", which] => {
            let r = match *which {
                "speaker" => Route::Speaker,
                "headset" | "wired" | "jack" => Route::Wired,
                "bluetooth" | "bt" => Route::Bluetooth,
                _ => bail!("speaker, headset or bluetooth"),
            };
            AudioProxy::new(conn).await?.set_route(r as u8).await?;
        }
        ["ringer"] => println!("{}", AudioProxy::new(conn).await?.ringer().await?),
        ["ringer", mode] => AudioProxy::new(conn).await?.set_ringer(mode).await?,
        ["bt", rest @ ..] => bluetooth(conn, rest).await?,
        ["get", key] => println!("{}", SettingsProxy::new(conn).await?.get(key).await?),
        ["set", key, value] => SettingsProxy::new(conn).await?.set(key, value).await?,
        ["block", n] => SettingsProxy::new(conn).await?.block(n).await?,
        ["unblock", n] => SettingsProxy::new(conn).await?.unblock(n).await?,
        ["blocked"] => {
            for n in SettingsProxy::new(conn).await?.blocked().await? {
                println!("{}", show(&n));
            }
        }
        ["wipe"] => placed("wipe", SettingsProxy::new(conn).await?.wipe().await?),
        ["watch"] => watch(conn, json).await?,
        _ => bail!("unknown command; phonectl help"),
    }
    Ok(())
}

async fn contacts(conn: &zbus::Connection, w: &[&str], region: &str, json: bool) -> Result<()> {
    let book = ContactsProxy::new(conn).await?;
    let show = |n: &str| phone_core::number::display_peer(n, region);
    let done = |p: Placed, what: &str| {
        if json {
            out(&p)
        } else {
            placed(what, p);
            Ok(())
        }
    };
    match w {
        [] | ["list"] => {
            let all = book.list().await?;
            if json {
                return out(&all);
            }
            for c in all {
                let numbers: Vec<String> = c.numbers.iter().map(|(k, n)| format!("{k} {}", show(n))).collect();
                println!("{:<24} {:<24} {}", c.id, c.name, numbers.join(", "));
            }
        }
        ["add", name, n] | ["add", name, n, _] => {
            let kind = w.get(3).unwrap_or(&"mobile").to_string();
            done(book.add(name, vec![(kind, n.to_string())]).await?, "contact")?;
        }
        ["edit", id, name, n] | ["edit", id, name, n, _] => {
            let kind = w.get(4).unwrap_or(&"mobile").to_string();
            done(book.update(id, name, vec![(kind, n.to_string())]).await?, "contact")?;
        }
        ["remove", id] => done(book.remove(id).await?, "contact")?,
        ["import", file] => {
            let text = if *file == "-" {
                let mut s = String::new();
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)?;
                s
            } else {
                std::fs::read_to_string(file).with_context(|| format!("reading {file}"))?
            };
            let p = book.import(&text).await?;
            if json {
                out(&p)?;
            } else if p.pending != 0 {
                placed("import", p);
            } else {
                println!("{} contacts added or updated", p.id);
            }
        }
        _ => bail!("contacts [list] | add <name> <number> [kind] | edit <id> <name> <number> [kind] | remove <id> | import <file|->"),
    }
    Ok(())
}

fn print_steps(steps: &[(String, String, String)], json: bool) {
    for (name, state, detail) in steps {
        if json {
            println!("{}", serde_json::json!({ "step": name, "state": state, "detail": detail }));
        } else {
            println!("{name:<17} {state:<12} {detail}");
        }
    }
}

fn placed(what: &str, p: Placed) {
    if p.pending != 0 {
        println!("{what} waiting for confirmation on the device (request {})", p.pending);
    } else {
        println!("{what} {}", p.id);
    }
}

/// The call a bare `answer` or `hangup` means.
async fn pick(calls: &CallsProxy<'_>, ringing: bool) -> Result<u32> {
    let all = calls.calls().await?;
    let found = if ringing {
        all.iter().find(|c| CallState::from_u8(c.state).ringing_in())
    } else {
        all.iter()
            .find(|c| CallState::from_u8(c.state) == CallState::Active)
            .or_else(|| all.iter().find(|c| CallState::from_u8(c.state).live()))
    };
    found.map(|c| c.id).context(if ringing { "nothing is ringing" } else { "no call" })
}

async fn bluetooth(conn: &zbus::Connection, w: &[&str]) -> Result<()> {
    let bt = BluetoothProxy::new(conn).await?;
    if !bt.available().await? {
        bail!("no Bluetooth adapter");
    }
    match w {
        [] | ["devices"] => {
            println!("powered {}", if bt.powered().await? { "on" } else { "off" });
            for d in bt.devices().await? {
                let kind = match (d.headset, d.speaker) {
                    (true, _) => "headset",
                    (false, true) => "speaker",
                    _ => "device",
                };
                let state = if d.connected {
                    "connected"
                } else if d.paired {
                    "paired"
                } else {
                    "seen"
                };
                println!("{}  {:<9} {:<8} {}", d.address, state, kind, d.name);
            }
        }
        ["scan"] => {
            bt.start_scan().await?;
            println!("scanning for 30 s; phonectl bt devices to see what turns up");
        }
        ["pair", a] => bt.pair(a).await?,
        ["connect", a] => bt.connect(a).await?,
        ["disconnect", a] => bt.disconnect(a).await?,
        ["forget", a] => bt.forget(a).await?,
        ["on"] => bt.set_powered(true).await?,
        ["off"] => bt.set_powered(false).await?,
        _ => bail!("bt devices|scan|pair|connect|disconnect|forget|on|off"),
    }
    Ok(())
}

async fn watch(conn: &zbus::Connection, json: bool) -> Result<()> {
    use serde_json::json;
    let calls = CallsProxy::new(conn).await?;
    let messages = MessagesProxy::new(conn).await?;
    let confirm = ConfirmProxy::new(conn).await?;
    let modem = ModemProxy::new(conn).await?;
    let mut added = calls.receive_call_added().await?;
    let mut changed = calls.receive_call_changed().await?;
    let mut removed = calls.receive_call_removed().await?;
    let mut texts = messages.receive_message_added().await?;
    let mut text_state = messages.receive_message_changed().await?;
    let mut alerts = messages.receive_alert_received().await?;
    let mut requested = confirm.receive_requested().await?;
    let mut resolved = confirm.receive_resolved().await?;
    let mut status = modem.receive_status_changed().await?;
    if !json {
        println!("watching; ctrl-c to stop");
    }
    loop {
        // Each event as a line of text, and as a JSON object for --json.
        let (line, obj) = tokio::select! {
            Some(s) = added.next() => s.args().map(|a| (
                format!("call+ {} {} {}", a.call.id, CallState::from_u8(a.call.state).label(), a.call.number),
                json!({"event": "call_added", "call": a.call}),
            ))?,
            Some(s) = changed.next() => s.args().map(|a| (
                format!("call~ {} {}", a.call.id, CallState::from_u8(a.call.state).label()),
                json!({"event": "call_changed", "call": a.call}),
            ))?,
            Some(s) = removed.next() => s.args().map(|a| (format!("call- {}", a.id), json!({"event": "call_removed", "id": a.id})))?,
            Some(s) = texts.next() => s.args().map(|a| (
                format!("text{} {} #{}", if a.message.direction == 0 { "<" } else { ">" }, a.message.peer, a.message.id),
                json!({"event": "message_added", "message": a.message}),
            ))?,
            Some(s) = text_state.next() => s.args().map(|a| (
                format!("text~ {} {:?}", a.id, MessageState::from_u8(a.state)),
                json!({"event": "message_changed", "id": a.id, "state": state_word(MessageState::from_u8(a.state))}),
            ))?,
            Some(s) = alerts.next() => s.args().map(|a| (
                format!("alert! {} {}", phone_core::model::AlertLevel::from_u8(a.alert.level).label(), a.alert.text),
                json!({"event": "alert", "alert": a.alert}),
            ))?,
            Some(s) = requested.next() => s.args().map(|a| (
                format!("confirm? {} {} {} by {}", a.request.id, a.request.kind, a.request.target, a.request.requester),
                json!({"event": "requested", "request": a.request}),
            ))?,
            Some(s) = resolved.next() => s.args().map(|a| (
                format!("confirm{} {}", if a.confirmed { "+" } else { "-" }, a.id),
                json!({"event": "resolved", "id": a.id, "confirmed": a.confirmed}),
            ))?,
            Some(s) = status.next() => s.args().map(|a| (
                format!("modem {} {} {}%", a.status.operator, a.status.tech, a.status.quality),
                json!({"event": "status", "status": a.status}),
            ))?,
            _ = tokio::signal::ctrl_c() => return Ok(()),
        };
        if json {
            println!("{obj}");
        } else {
            println!("{line}");
        }
    }
}

/// Send, and wait for the outcome: through a confirmation on the device if one is
/// needed, then until the modem has sent it (or failed to).
async fn send_and_wait(conn: &zbus::Connection, to: &str, text: &str, secs: u64, json: bool) -> Result<()> {
    let messages = MessagesProxy::new(conn).await?;
    let confirm = ConfirmProxy::new(conn).await?;
    // Subscribe first, so nothing that happens quickly is missed.
    let mut changes = messages.receive_message_changed().await?;
    let mut added = messages.receive_message_added().await?;
    let mut resolved = confirm.receive_resolved().await?;
    let placed = messages.send(to, text).await?;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(secs);
    let mut id = placed.id;
    if placed.pending != 0 {
        if !json {
            eprintln!("waiting for confirmation on the device (request {})", placed.pending);
        }
        loop {
            let s =
                tokio::time::timeout_at(deadline, resolved.next()).await.context("timed out")?.context("bus closed")?;
            let a = s.args()?;
            if a.id == placed.pending {
                if !a.confirmed {
                    bail!("denied on the device");
                }
                break;
            }
        }
        // Confirmed: the text is stored and announced, then sent.
        loop {
            let s =
                tokio::time::timeout_at(deadline, added.next()).await.context("timed out")?.context("bus closed")?;
            let m = s.args()?.message;
            if m.direction == 1 {
                id = m.id;
                break;
            }
        }
    }
    loop {
        let s = tokio::time::timeout_at(deadline, changes.next()).await.context("timed out waiting for the modem")?;
        let a = s.context("bus closed")?.args().map(|a| (a.id, MessageState::from_u8(a.state)))?;
        if a.0 != id {
            continue;
        }
        match a.1 {
            MessageState::Sent | MessageState::Delivered => {
                if json {
                    out(&serde_json::json!({"id": id, "state": state_word(a.1)}))?;
                } else {
                    println!("sent {id}");
                }
                return Ok(());
            }
            MessageState::Failed => bail!("sending failed (phonectl retry {id})"),
            _ => {}
        }
    }
}

/// Wait for a one-time code. Reading the code needs a read grant, as reading any
/// text does.
async fn otp(conn: &zbus::Connection, w: &[&str], region: &str, json: bool) -> Result<()> {
    let mut from = None;
    let mut secs = 60u64;
    let mut it = w.iter();
    while let Some(a) = it.next() {
        match *a {
            "--from" => from = Some(phone_core::number::canonical(it.next().context("--from <number>")?, region)),
            "--wait" => secs = it.next().context("--wait <seconds>")?.parse()?,
            _ => bail!("otp [--from <number>] [--wait <seconds>]"),
        }
    }
    let messages = MessagesProxy::new(conn).await?;
    let mut added = messages.receive_message_added().await?;
    // Fail now, not after a minute, if this program may not read texts.
    messages.threads().await?;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        let s =
            tokio::time::timeout_at(deadline, added.next()).await.context("no code arrived")?.context("bus closed")?;
        let m = s.args()?.message;
        if m.direction != 0 || from.as_deref().is_some_and(|f| !phone_core::number::same_party(f, &m.peer)) {
            continue;
        }
        let body = messages.conversation(&m.peer).await?.into_iter().find(|x| x.id == m.id).map(|x| x.body);
        if let Some(code) = body.as_deref().and_then(phone_core::text::one_time_code) {
            if json {
                out(&serde_json::json!({"code": code, "from": m.peer, "id": m.id}))?;
            } else {
                println!("{code}");
            }
            return Ok(());
        }
    }
}

/// One command to the far end's baresip, over its ctrl_tcp netstring protocol.
/// Events stream on the same socket; the reply is the object marked a response.
/// The far end of MMS, against fake-mmsd on the session bus.
async fn fake_mms(w: &[&str]) -> Result<()> {
    let conn = zbus::Connection::session().await.context("session bus")?;
    let p = zbus::Proxy::new(&conn, "org.ofono.mms", "/org/ofono/mms", "io.github.isaacrowntree.Dolphone1.FakeMmsd")
        .await
        .context("fake-mmsd")?;
    match w {
        ["dump"] => print!("{}", p.call_method("Dump", &()).await?.body().deserialize::<String>()?),
        ["fails", s] => drop(p.call_method("SetSendFails", &(matches!(*s, "on" | "true" | "yes" | "1"),)).await?),
        [from, rest @ ..] => {
            let (mut others, mut files, mut text) = (Vec::<String>::new(), Vec::<String>::new(), Vec::<&str>::new());
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match *a {
                    "--to" => others.extend(it.next().context("--to <n,n>")?.split(',').map(String::from)),
                    "--file" => {
                        files.push(std::fs::canonicalize(it.next().context("--file <path>")?)?.display().to_string())
                    }
                    t => text.push(t),
                }
            }
            let path = p.call_method("Receive", &(*from, others, text.join(" "), files)).await?;
            println!("{}", path.body().deserialize::<zbus::wire::OwnedObjectPath>()?);
        }
        _ => bail!("fake mms <from> [--to <n,n>] [--file <f>]... [text...] | fake mms dump | fake mms fails <on|off>"),
    }
    Ok(())
}

fn fake_sip(command: &str, params: &str) -> Result<String> {
    use std::io::{Read, Write};
    let addr = std::env::var("DOLPHONE_FAKE_SIP").unwrap_or_else(|_| "127.0.0.1:4444".into());
    let mut s = std::net::TcpStream::connect(&addr).with_context(|| format!("the fake SIP far end ({addr})"))?;
    s.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
    let msg = serde_json::json!({"command": command, "params": params, "token": "phonectl"}).to_string();
    s.write_all(format!("{}:{msg},", msg.len()).as_bytes())?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = s.read(&mut chunk)?;
        if n == 0 {
            bail!("the far end closed the connection");
        }
        buf.extend_from_slice(&chunk[..n]);
        // Take whole netstrings off the front until the response is among them.
        while let Some(colon) = buf.iter().position(|b| *b == b':') {
            let Ok(len) = std::str::from_utf8(&buf[..colon]).unwrap_or("").parse::<usize>() else {
                bail!("not a netstring from the far end");
            };
            if buf.len() < colon + 1 + len + 1 {
                break;
            }
            let body: serde_json::Value = serde_json::from_slice(&buf[colon + 1..colon + 1 + len])?;
            buf.drain(..colon + 1 + len + 1);
            if body.get("response").and_then(|r| r.as_bool()) == Some(true) {
                return Ok(body.get("data").and_then(|d| d.as_str()).unwrap_or("").to_string());
            }
        }
    }
}

async fn fake(w: &[&str]) -> Result<()> {
    match w {
        ["sip", "ring", to] => {
            println!("{}", fake_sip("dial", to)?.trim());
            return Ok(());
        }
        ["sip", "answer"] => return fake_sip("accept", "").map(drop),
        ["sip", "hangup"] => return fake_sip("hangup", "").map(drop),
        ["sip", "message", text @ ..] if !text.is_empty() => return fake_sip("message", &text.join(" ")).map(drop),
        ["sip", "autoanswer", s] => {
            let mode = if matches!(*s, "on" | "true" | "yes" | "1") { "auto" } else { "manual" };
            return fake_sip("answermode", mode).map(drop);
        }
        ["sip", ..] => bail!("fake sip ring <address> | answer | hangup | autoanswer <on|off>"),
        ["mms", rest @ ..] => return fake_mms(rest).await,
        _ => {}
    }
    let conn = zbus::Connection::system().await.context("system bus")?;
    let p = zbus::Proxy::new(
        &conn,
        "org.freedesktop.ModemManager1",
        "/org/freedesktop/ModemManager1",
        "io.github.isaacrowntree.Dolphone1.FakeModem",
    )
    .await?;
    let on = |s: &str| matches!(s, "on" | "true" | "yes" | "1");
    match w {
        ["ring", n] => {
            println!("{}", p.call_method("Ring", &(*n,)).await?.body().deserialize::<zbus::wire::OwnedObjectPath>()?)
        }
        ["sms", from, text @ ..] if !text.is_empty() => {
            println!(
                "{}",
                p.call_method("Deliver", &(*from, text.join(" ")))
                    .await?
                    .body()
                    .deserialize::<zbus::wire::OwnedObjectPath>()?
            )
        }
        ["answer"] => drop(p.call_method("RemoteAnswer", &()).await?),
        ["hangup"] => drop(p.call_method("RemoteHangup", &()).await?),
        ["busy"] => drop(p.call_method("RemoteBusy", &()).await?),
        ["signal", q, tech] => drop(p.call_method("SetSignal", &(q.parse::<u32>()?, *tech)).await?),
        ["register", s] => drop(p.call_method("SetRegistration", &(on(s), false, "")).await?),
        ["register", s, op @ ..] => drop(p.call_method("SetRegistration", &(on(s), false, op.join(" "))).await?),
        ["roam", s] => drop(p.call_method("SetRegistration", &(true, on(s), "")).await?),
        ["autoanswer", ms] => drop(p.call_method("SetAutoAnswer", &(ms.parse::<u64>()?,)).await?),
        ["sendfails", s] => drop(p.call_method("SetSendFails", &(on(s),)).await?),
        ["atdebug", s] => drop(p.call_method("SetAtDebug", &(on(s),)).await?),
        ["ussd", reply @ ..] if !reply.is_empty() => drop(p.call_method("SetUssdReply", &(reply.join(" "),)).await?),
        ["dump"] => print!("{}", p.call_method("Dump", &()).await?.body().deserialize::<String>()?),
        ["broadcast", ch, text @ ..] if !text.is_empty() => {
            // The serial comes from the text, so broadcasting the same text again is
            // a rebroadcast of the same alert, as networks repeat them.
            let text = text.join(" ");
            let serial = text.bytes().fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b))) & 0xffff;
            let reply = p.call_method("Broadcast", &(ch.parse::<u32>()?, text, serial)).await?;
            println!("{}", reply.body().deserialize::<zbus::wire::OwnedObjectPath>()?)
        }
        ["lock", s] => drop(p.call_method("SetLocked", &(on(s),)).await?),
        ["uplink", s] => drop(p.call_method("SetUplink", &(on(s),)).await?),
        ["enable", s] => drop(p.call_method("SetEnabled", &(on(s),)).await?),
        _ => bail!(
            "fake ring|sms|answer|hangup|busy|signal|register|roam|autoanswer|sendfails|atdebug|ussd|broadcast|lock|enable|dump"
        ),
    }
    Ok(())
}
