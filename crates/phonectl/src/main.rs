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

const USAGE: &str = "usage: phonectl <command>

phone
  status                      modem, signal, operator
  dial <number>               place a call (waits for confirmation on the device)
  answer [id] | hangup [id]   the ringing / live call by default
  swap | mute | unmute | silence
  dtmf <id> <tones>
  calls | recents [n]
  ussd <code>                 e.g. *100# (Dolphone's dialer only, refused here)

messages
  send <to> <text...>         (waits for confirmation on the device)
  threads | thread <peer> | read <peer>

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
  fake autoanswer <ms> | fake sendfails <on|off> | fake ussd <reply...> | fake dump";

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    if words.is_empty() || matches!(words[0], "-h" | "--help" | "help") {
        println!("{USAGE}");
        return Ok(());
    }
    if words[0] == "fake" {
        return fake(&words[1..]).await;
    }
    let conn = zbus::Connection::session().await.context("session bus")?;
    run(&conn, &words).await.map_err(|e| {
        if let Some(zbus::Error::MethodError(name, msg, _)) = e.downcast_ref::<zbus::Error>() {
            if name.as_str() == "org.freedesktop.DBus.Error.ServiceUnknown" {
                return anyhow::anyhow!("phoned is not running");
            }
            return anyhow::anyhow!("{}", msg.clone().unwrap_or_else(|| name.to_string()));
        }
        e
    })
}

async fn run(conn: &zbus::Connection, w: &[&str]) -> Result<()> {
    let calls = CallsProxy::new(conn).await?;
    let messages = MessagesProxy::new(conn).await?;
    let region = SettingsProxy::new(conn).await?.get(keys::REGION.0).await.unwrap_or_else(|_| "AU".into());
    let show = |n: &str| phone_core::number::display(n, &region);
    match w {
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
        ["mute"] => calls.set_muted(true).await?,
        ["unmute"] => calls.set_muted(false).await?,
        ["silence"] => calls.silence().await?,
        ["dtmf", id, tones] => calls.send_dtmf(id.parse()?, tones).await?,
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
                    (Outcome::Answered, 0) => "in",
                    (Outcome::Answered, _) => "out",
                };
                println!("{:<10} {:<16} {}s", what, show(&r.peer), r.duration);
            }
        }
        ["ussd", code] => println!("{}", ModemProxy::new(conn).await?.ussd(code).await?),
        ["send", to, text @ ..] if !text.is_empty() => placed("text", messages.send(to, &text.join(" ")).await?),
        ["threads"] => {
            for t in messages.threads().await? {
                let badge = if t.unread > 0 { format!("({}) ", t.unread) } else { String::new() };
                println!("{badge}{:<16} {}", show(&t.peer), t.last_body.lines().next().unwrap_or(""));
            }
        }
        ["thread", peer] | ["read", peer] => {
            let peer = phone_core::number::canonical(peer, &region);
            for m in messages.conversation(&peer).await? {
                let who = if m.direction == 0 { "<" } else { ">" };
                let state = match MessageState::from_u8(m.state) {
                    MessageState::Sending => " (sending)",
                    MessageState::Failed => " (failed)",
                    _ => "",
                };
                println!("{who} {}{state}", m.body);
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
        ["watch"] => watch(conn).await?,
        _ => bail!("unknown command; phonectl help"),
    }
    Ok(())
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

async fn watch(conn: &zbus::Connection) -> Result<()> {
    let calls = CallsProxy::new(conn).await?;
    let messages = MessagesProxy::new(conn).await?;
    let confirm = ConfirmProxy::new(conn).await?;
    let modem = ModemProxy::new(conn).await?;
    let mut added = calls.receive_call_added().await?;
    let mut changed = calls.receive_call_changed().await?;
    let mut removed = calls.receive_call_removed().await?;
    let mut texts = messages.receive_message_added().await?;
    let mut text_state = messages.receive_message_changed().await?;
    let mut requested = confirm.receive_requested().await?;
    let mut status = modem.receive_status_changed().await?;
    println!("watching; ctrl-c to stop");
    loop {
        let line = tokio::select! {
            Some(s) = added.next() => s.args().map(|a| format!("call+ {} {} {}", a.call.id, CallState::from_u8(a.call.state).label(), a.call.number))?,
            Some(s) = changed.next() => s.args().map(|a| format!("call~ {} {}", a.call.id, CallState::from_u8(a.call.state).label()))?,
            Some(s) = removed.next() => s.args().map(|a| format!("call- {}", a.id))?,
            Some(s) = texts.next() => s.args().map(|a| format!("text{} {} {}", if a.message.direction == 0 { "<" } else { ">" }, a.message.peer, a.message.body))?,
            Some(s) = text_state.next() => s.args().map(|a| format!("text~ {} {:?}", a.id, MessageState::from_u8(a.state)))?,
            Some(s) = requested.next() => s.args().map(|a| format!("confirm? {} {} {} by {}", a.request.id, a.request.kind, a.request.target, a.request.requester))?,
            Some(s) = status.next() => s.args().map(|a| format!("modem {} {} {}%", a.status.operator, a.status.tech, a.status.quality))?,
            _ = tokio::signal::ctrl_c() => return Ok(()),
        };
        println!("{line}");
    }
}

async fn fake(w: &[&str]) -> Result<()> {
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
        ["ussd", reply @ ..] if !reply.is_empty() => drop(p.call_method("SetUssdReply", &(reply.join(" "),)).await?),
        ["dump"] => print!("{}", p.call_method("Dump", &()).await?.body().deserialize::<String>()?),
        _ => bail!("fake ring|sms|answer|hangup|busy|signal|register|roam|autoanswer|sendfails|ussd|dump"),
    }
    Ok(())
}
