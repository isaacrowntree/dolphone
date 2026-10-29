# Architecture

## The pieces

| Crate | Runs as | Job |
|---|---|---|
| `phone-core` | library | numbers (canonical form, display, emergency / premium / short classes), message text cleaning and SMS segment counting, vCard contacts, the SQLite store, local time |
| `phone-api` | library | the D-Bus names, wire types and client proxies; a script author can use it alone |
| `phone-bus` | library | a front end's worker thread: subscribes to everything phoned says, runs calls off the UI thread, survives phoned restarting |
| `phoned` | systemd user service | the phone: ModemManager client, call state, texts, confirmations, call audio, ringer, Bluetooth, jack, desktop notifications |
| `phonectl` | CLI | everything a front end can do, for scripts; `phonectl fake ...` plays the far end against fake-mm |
| `fake-mm` | dev service | a ModemManager stand-in with one scriptable modem |
| `apps/dialer`, `apps/messages`, `apps/settings` | FlipCTL apps | the panel front ends (Wayland clients on flipctl's compositor, drawn with flipctl-app's widgets) |
| `apps/desktop` | desktop app | the desktop-mode front end |

## Rules the design keeps

1. **Add to FlipCTL, never replace it.** No launcher, status bar, keyboard or modem
   settings of our own. The apps use FlipCTL's widgets, fonts and soft-key roles;
   phoned raises them with `flipctl open`; SIM and network settings stay in
   Flipper's own 5G Modem app.
2. **One owner per thing.** phoned is ModemManager's only client for Voice and
   Messaging (NetworkManager and Flipper's 5G Modem app read the same modem for data
   and signal, untouched), with one deliberate exception: PipeWire's Bluetooth
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
5. **Measured on the panel, in the fonts it draws.** Every position on the panel is
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

## A text, end to end

Incoming: `Messaging.Added` → phoned waits for a multipart message to finish
arriving, cleans sender and body, drops it if blocked, stores it, emits
`MessageAdded` and notifies, then deletes it from the modem's storage (a full modem
stops accepting texts). Outgoing: `Messages.Send` → confirmation rules → stored as
Sending → `Messaging.Create` + `Sms.Send` → Sent or Failed → the modem's copy is
deleted.

## Confirmations

Dialling or texting from an outside program (a script, `phonectl`, another app),
any premium number or short code from anyone, an emergency number from an outside
program, and wiping the store all wait as a `Request`. phoned shows it on the panel
(by raising the Dialer) and the desktop; only a press there, or a click in the
desktop session, lets it go. Requests expire after two minutes. Who is "inside" is
decided by the executable behind the D-Bus caller; see SECURITY.md for what that
does and does not protect.

## Where things live

| What | Where |
|---|---|
| Messages, call log, blocks, settings | `~/.local/share/dolphone/dolphone.db` (0600) |
| Contacts | `~/.local/share/dolphone/contacts/*.vcf` |
| Audio board config | `~/.config/dolphone/audio.json` |
| Ring tones, handoff file, desktop socket | `$XDG_RUNTIME_DIR/dolphone/` |
| Panel apps | `~/Apps/Phone/dolphone-*-aarch64.fap.AppImage` |
