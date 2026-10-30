# Architecture

## The pieces

| Crate | Runs as | Job |
|---|---|---|
| `phone-core` | library | numbers (canonical form, display, emergency / premium / short classes, prefixes), message text cleaning, SMS segment counting and one-time codes, vCard contacts (import, merge, edit), supplementary-service codes (TS 22.030), the store (SQLite, or SQLCipher when encrypted: messages with their MMS parts, calls, alerts, grants, audit; behind the `db` feature, which the panel apps leave out), the countries' call-progress tones, local time |
| `phone-api` | library | the D-Bus names, wire types and client proxies; a script author can use it alone |
| `phone-bus` | library | a front end's worker thread: subscribes to everything phoned says, runs calls off the UI thread, survives phoned restarting |
| `phoned` | systemd user service | the phone: ModemManager client, call state, texts, MMS through mmsd-tng, emergency alerts, confirmations and grants, call audio (including a serial PCM bridge), ringer, tones and LEDs, Bluetooth and the phonebook copy, jack, SIP through baresip, KDE Connect, device profiles, the modem's AT setup and service codes (on a spare AT port of its own when there is one), the Messages PIN, rules on texts, uplink alerts, sleep and wake, desktop notifications, the store's sealed key |
| `dolphone-gateway` | optional user service | an HTTP SMS gateway on phoned's API, localhost only by default |
| `phonectl` | CLI | everything a front end can do, for scripts; `phonectl fake ...` plays the far end against fake-mm |
| `fake-mm` | dev service | a ModemManager stand-in with one scriptable modem: calls, texts, delivery reports, cell broadcast, a SIM PIN, call waiting, conferences, AT commands as a Quectel answers them (refused without debug mode), a spare AT port as a pty, the cells it hears (GetCellInfo); and `fake-mmsd`, an mmsd-tng stand-in on the session bus |
| `apps/dialer`, `apps/messages`, `apps/settings` | FlipCTL apps | the panel front ends (Wayland clients on flipctl's compositor, drawn with flipctl-app's widgets) |
| `apps/desktop` | desktop app | the desktop-mode front end |

## Rules the design keeps

1. **Add to FlipCTL, never replace it.** No launcher, status bar or keyboard of our
   own. The apps use FlipCTL's widgets, fonts and soft-key roles; phoned raises them
   with `flipctl open`; SIM, APN and data settings stay in Flipper's own 5G Modem
   app. What Dolphone sets on the modem is only what calls need: VoLTE, the carrier
   profile, USB call audio, and refusing 2G when asked.
2. **One owner per thing.** phoned is ModemManager's only client for Voice and
   Messaging (NetworkManager and Flipper's 5G Modem app read the same modem for data
   and signal, untouched; mmsd-tng takes the MMS notices from Messaging and fetches
   the messages for phoned), with one deliberate exception: PipeWire's Bluetooth
   hands-free support follows ModemManager's calls so a headset rings and its button
   answers, which is the only way to get that without reimplementing HFP. phoned is
   the only thing that routes call audio. Front ends never talk to either. The
   package conflicts with GNOME Calls and Chatty, which would race phoned for every
   call and text.
3. **Nothing waits on a UI thread, and nothing slow waits on phoned's async threads.**
   Bus calls run in `phone-bus`'s worker; PipeWire's tools run on blocking threads
   with time limits. (A synchronous `pw-dump` once froze call signals for 25 s.)
4. **Untrusted input is cleaned once, on the way in.** Sender names and bodies lose
   control characters, bidi overrides and zero-width characters, and are capped,
   before they reach the store, a log or a screen.
5. **The Flipper One first; other machines by data.** Everything about the hardware
   around the modem is a device profile (`dist/devices/*.toml`, see DEVICES.md), so
   a new machine is a file, not a branch in the code.
6. **Measured on the panel, in the fonts it draws.** Every position on the panel is
   computed in Rust, text is wrapped and cut with the bitmap font that will draw it,
   and the screens are rendered headlessly in tests (`DOLPHONE_RENDER=1 cargo test`).

## A call, end to end

1. The network rings. ModemManager adds a Call object and emits `Voice.CallAdded`.
2. phoned reads the number, checks the block list, starts tracking the call and
   watching its `StateChanged`, emits `Calls.CallAdded`, starts the ringer
   (PipeWire, notification role), runs `flipctl open <Dialer bundle>` and posts a
   desktop notification if a desktop is running.
3. The Dialer comes up on the panel (FlipCTL starts it or brings it forward) and
   shows the incoming screen, ignoring keys for 400 ms so a press meant for the app
   underneath cannot answer. It vibrates the motor while the call rings.
4. Run answers: Dialer → `Calls.Answer` → phoned → `Call.Accept` (or
   `Voice.HoldAndAccept` when another call is live).
5. ModemManager reports `ACTIVE`; phoned stops the ringer, starts the two PipeWire
   loopbacks for the chosen route (modem ↔ speaker with echo cancellation, jack, or
   Bluetooth), and emits `CallChanged`. Every front end shows the call.
6. End (Run, after a 1.5 s lockout), the far end hanging up, or the headset button:
   `TERMINATED`. phoned logs the call (answered, missed, declined, cancelled,
   blocked), tears the audio down, emits `CallRemoved`, posts a missed-call
   notification if it was one, and has ModemManager delete the Call object.

## A SIP call

phoned runs its own baresip (config and account in `~/.config/dolphone/sip/`, 0600)
and drives it over baresip's D-Bus control module, listening only to the connection
whose process it started. baresip's events (`CALL_INCOMING`, `CALL_ESTABLISHED`,
`CALL_CLOSED` …) move a SIP call through the same states as a cellular one, so it
is the same `Live` call to every front end, with `internet: true`. Audio is
baresip's own, through PipeWire: phoned points it at the route's output and
microphone with `auplay`/`ausrc` as the route changes. Which network a call takes is
decided in `Phone::dial`: a SIP address always SIP, an emergency number never, and
otherwise the owner's choice (always, or only with no mobile signal).

## A text, end to end

Incoming: `Messaging.Added` → phoned waits for a multipart message to finish
arriving, cleans sender and body, drops it if blocked, stores it, emits
`MessageAdded` and notifies, then deletes it from the modem's storage (a full modem
stops accepting texts). Then the owner's rules (`~/.config/dolphone/rules.json`) run for a matching text,
with the text as JSON on stdin and no shell. Outgoing: `Messages.Send` → confirmation
rules (or a standing grant) → stored as Sending → `Messaging.Create` + `Sms.Send` →
Sent or Failed → with delivery reports on, the modem's copy is kept until the network
reports Delivered, then deleted.

## An emergency alert

phoned tells the modem's CellBroadcast interface which channels to listen to (the
3GPP alert ranges). Each `Cbm` is classed by its channel (national, extreme, severe,
Amber, test, information), dropped if the owner turned that class off (national
never can be), stored once however often it is rebroadcast, announced as
`Messages.AlertReceived`, sounded with the alert tone whatever the ringer says, put
full screen on the panel and the desktop, and deleted from the modem.

## Confirmations

Dialling or texting from an outside program (a script, `phonectl`, another app),
any premium number or short code from anyone, an emergency number from an outside
program, and wiping the store all wait as a `Request`. phoned shows it on the panel
(by raising the Dialer) and the desktop; only a press there, or a click in the
desktop session, lets it go. Requests expire after two minutes. Who is "inside" is
decided by the executable behind the D-Bus caller; see SECURITY.md for what that
does and does not protect.

A **standing grant** lets one program text listed numbers (or a prefix) without
asking each time, up to its own hourly limit and until a date; it is asked for with
`Settings.RequestGrant`, allowed once on the device, and revoked in Phone Settings.
Premium, short and emergency numbers can never be granted. Every outside request is
written to the **audit log** (numbers masked), shown in Phone Settings.

## Where things live

| What | Where |
|---|---|
| Messages, call log, alerts, grants, audit, blocks, settings | `~/.local/share/dolphone/dolphone.db` (0600) |
| MMS pictures and other parts | `~/.local/share/dolphone/mms/` (0700); in the store itself when it is encrypted |
| KDE Connect: this device's certificate, paired laptops | `~/.local/share/dolphone/kdeconnect/` |
| Contacts | `~/.local/share/dolphone/contacts/*.vcf` |
| The store's sealed key (when encrypted) | `~/.config/dolphone/store-key.cred` |
| The SMS gateway's token | `~/.config/dolphone/gateway.token` (0600) |
| Device profiles | built in; `/usr/share/dolphone/devices/`; `~/.config/dolphone/devices/` |
| The owner's audio overrides | `~/.config/dolphone/audio.json` |
| Rules on incoming texts | `~/.config/dolphone/rules.json` |
| SIP: baresip's config, account and password | `~/.config/dolphone/sip/` (0700; accounts and password 0600) |
| Ring tones, handoff file, desktop socket, MMS parts being shown from an encrypted store | `$XDG_RUNTIME_DIR/dolphone/` |
| Panel apps | `~/Apps/Phone/dolphone-*-aarch64.fap.AppImage` |
