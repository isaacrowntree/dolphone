# Roadmap

Ten reviews of Dolphone (as an everyday phone, for Flipper power users, by Linux
phone veterans, for accessibility, messaging, telephony, desktop integration,
reliability, privacy, and "does it feel like a phone on this machine") produced this
list. The security holes and outright bugs they found are already fixed (see
CHANGELOG.md); what follows is what is still missing, in the order it matters.

## 0. Before anything else: a real call

Everything here has been tested against fake-mm. The go/no-go hardware test in
[docs/HARDWARE.md](docs/HARDWARE.md) decides whether the rest is worth doing. Its
deciding question is whether the RM520N-GL's USB audio gives Linux two-way call audio.

## 1. It must always ring

- **Suspend and wake.** Take a logind inhibitor while ringing and in a call, react to
  `PrepareForSleep`, re-read calls and texts on resume, and set the modem's USB
  remote wakeup (or the M.2 ring-indicator line) so a call wakes the board. Be plain
  that MCU mode (Linux off) means no phone.
- **Events, not polling.** phoned polls the modem every 5 s, PipeWire every 3 s and
  input devices every 5 s. Move to ModemManager's `InterfacesAdded` and
  `PropertiesChanged`, `pw-mon` or PipeWire's registry, and udev.
- **Adopt calls after a restart**, whatever their direction and state, so a phoned
  restart mid-call keeps the call on screen and its audio running.
- **SIM PIN entry** on the panel (today a locked SIM is reported and sent to Flipper's
  5G Modem app).
- **Monotonic time** for call durations, rate limits and request expiry.
- **Quectel setup** modelled on eg25-manager: URC port, IMS, and carrier profile checks.

## 2. Feels like a phone on the panel

- **Missed calls and texts reach the panel**: a distinct vibration pattern, and a strip
  under the status bar in every Dolphone app ("1 missed · 2 texts"). Upstream: ask
  FlipCTL for a status-bar badge, a toast and a home tile.
- **A "Today" screen** when the Dialer opens: missed calls, unread threads, favourites.
- **The touchpad as a keypad**: tap the 3×4 grid directly instead of walking the D-pad.
- **Smart dialing**: T9 letters on the keypad match names ("539" finds Jordan).
- **Speed dial and favourites**: hold 2-9; hold 1 for voicemail.
- **Quick replies in conversations**, not only for a ringing call.
- **A haptic vocabulary**: text, missed call, call waiting, call dropped, send failed.
- **Ringer quick switch**: hold PTT on the keypad cycles Ring / Vibrate / Silent.

## 3. Telephony

- **Voicemail**: a number in Settings (carrier defaults), Hold-1, and the message-waiting
  indicator read from the SIM.
- **Call waiting on/off** (`Voice.CallWaitingSetup`, already in ModemManager).
- **Conference calls** (`Call.JoinMultiparty`).
- **Call forwarding and caller-ID** codes (`**21*…#`, `#31#`), through QMI or AT, since
  ModemManager has no API. (Today they are refused rather than misread as USSD.)
- **Region from the SIM** (MCC) instead of a setting, and a roaming warning before dialing.

## 4. Messaging

- **Emergency alerts** through ModemManager's CellBroadcast interface: a full-screen
  alert that overrides silent. High value, medium effort.
- **MMS** with mmsd-tng: group threads keyed on participants; photos on the desktop.
  Today MMS notifications show as a placeholder.
- **Delivery reports** and **retry** for a failed text.
- **Search and export** (SQLite FTS5; SMS Backup & Restore XML for moving to Android).
- **Emoji joiners** kept for the desktop app (the cleaner removes U+200D today).
- No RCS: there is no route to it from Linux. Bridges to Matrix and others belong
  outside Dolphone, on its D-Bus API.

## 5. For scripts and the Flipper crowd

- **Standing grants**: "allow this program to text these numbers, N an hour, until
  date", approved once on the device and revocable in Settings. Premium, short and
  emergency numbers can never be granted.
- **Rules on incoming texts**: sender and pattern run a command, with JSON on stdin and
  no shell; remote commands need an allowlisted sender and a shared secret.
- **Uplink-down alerts** (WAN watch → text), the network-multitool feature.
- `phonectl --json`, NDJSON `watch`, `send --wait`, `phonectl otp --wait 60`.
- **Cell info logging** and a 2G-downgrade warning; a "Disable 2G" switch.
- **SIM-aware records** (ICCID) for multi-SIM and eSIM profile switches.
- An optional, separate **SMS gateway** (HTTP/MQTT), localhost-only by default.

## 6. Desktop mode and the laptop

- **KDE Connect protocol** in phoned, so GSConnect / KDE Connect on a laptop gets texts
  and call alerts with no new client.
- **Copy** from message bubbles and a "Copy code" button for one-time codes.
- **Contacts from Evolution Data Server**, and a vdirsyncer recipe for CardDAV.
- **Pause media on ring** (MPRIS) and **inhibit idle** during calls.
- **Keyboard shortcuts**, a tray icon and launcher badges.

## 7. Accessibility

- **Spoken caller ID and dialing** (espeak-ng or Piper through PipeWire), opt-in.
- **A large-text panel mode** (three-row menus, the title face for names and messages).
- **Visual ringing** (backlight and LEDs) and distinct, stronger vibration.
- **TTY/relay guidance** when 106 is dialed (the National Relay Service), and texting as
  the first-class channel for Deaf users.
- **A fallback bitmap font** for scripts beyond Latin and Cyrillic (names already fall
  back to the number).
- Adjustable long-press time; a "steady hands" mode where Decline is a hold.

## 8. Privacy

- **Encrypted store** (SQLCipher) with a Messages PIN, honest about what it does and
  does not stop.
- **Spam rules**: blocking by prefix and pattern; "silence unknown callers".
- **An audit log** of who used the API and what for, shown in Settings.
- **Root-owned front ends only**: once the panel apps can be installed system-wide,
  trust only `/usr/bin`.
