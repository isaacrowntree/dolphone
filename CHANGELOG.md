# Changelog

## 0.1.0 (unreleased)

First version, built for the Flipper One, tested against fake-mm, a SIP far end and
in the real FlipCTL; not yet on a real modem.

The roadmap from the reviews, done:

- It rings: a logind inhibitor while a call exists; catching up after resume; the
  modem allowed to wake the board (udev); ModemManager's signals instead of polling;
  calls adopted after a restart in any state; SIM PIN entry on the keypad; a
  monotonic clock for durations, limits and expiry.
- The panel: missed calls and new texts beside the operator; the touchpad as a
  keypad; T9 name matching; speed dial (hold 2–9, assigned by holding Ok on a
  contact); voicemail (hold 1, with carrier numbers from the SIM); quick replies in
  conversations; a vibration for each kind of news; hold PTT to switch the ringer;
  "hold to decline"; the TTY relay hint for 106; a roaming note.
- Telephony: call waiting on and off; conference (hold Ok to merge); the region from
  the SIM; the country's busy, congestion and ringback tones.
- Calls over the internet: SIP through baresip, with the same screens, routes and log.
- Messaging: emergency alerts (cell broadcast) full screen; delivery reports;
  resending a failed text; emoji joiners kept.
- Contacts: copied from a paired phone over Bluetooth (PBAP) or imported from a vCard
  export, merged rather than doubled, names and numbers only; managed from the
  desktop app and `phonectl contacts`.
- Scripts: standing grants; rules on incoming texts; an audit log;
  `phonectl --json`, `watch` as NDJSON, `send --wait`, `otp --wait`.
- The desktop: Copy and "Copy code" for one-time codes (cleared after a minute);
  keyboard shortcuts; the alert card; every new setting.
- Privacy: prefix blocking; silence for unknown callers.
- Device profiles: the Flipper One first, and the ClockworkPi uConsole, a Raspberry
  Pi, the PinePhone, the Librem 5, laptops and a generic fallback; an amd64 package.
- Picture messages and group texts: MMS through mmsd-tng (pictures kept beside the
  store, shown in the desktop app, named on the panel; a group is one thread and a
  reply goes to everyone); `phonectl mms` and `SendMms` to send, waiting for the
  device like a text when a program asks; fake-mmsd to test against.
- Carrier services: call forwarding, caller-ID and call-waiting codes from the
  keypad (3GPP TS 22.030, sent as `AT+CCFC` and `AT+CLIR`); barring codes refused.
- The modem: `phonectl modem setup` and Phone Settings > Modem check a Quectel
  modem's USB call audio, VoLTE and carrier profile and set them with one press
  (it restarts); a device profile can send AT commands when the modem appears and
  when a call starts or ends. The install asks whether ModemManager may pass them
  (its debug mode), and `dolphone-modem-access on|off` changes it.
- Call audio: a SIM7600's serial PCM bridged to PipeWire at 8 kHz (the uConsole's
  4G extension); an earpiece route for the PinePhone and Librem 5; holding and
  swapping between a SIP call and a mobile call.
- Texts: search (FTS5) and export as SMS Backup & Restore XML; texts over SIP; the
  SIM each text and call went through.
- The panel: a Today screen of missed calls, unread threads and favourites; an
  adjustable long-press time; the LEDs flash for a ringing call (a udev rule gives
  the `dolphone` group the Flipper One's LEDs).
- Cell information: a log of the networks the modem was on, a warning when it is
  forced onto 2G, and a switch to refuse 2G.
- The laptop: KDE Connect (texts and call alerts in GSConnect, replies from it),
  a tray icon and launcher badges for the desktop app.
- Scripts: an optional SMS gateway (HTTP, localhost by default); a text when the
  uplink goes down.
- Privacy: the message store can be encrypted with SQLCipher (`phoned
  --encrypt-store`), its key sealed by `systemd-creds --user`; without the key it
  stays locked and texts stay on the SIM. Trusting AppImage front ends can be turned
  off. The panel apps no longer link the database at all.
- Tests: the end-to-end suite grew to 212 checks, with 11 more against the real KDE
  Connect daemon; the four apps are driven live against phoned
  (`scripts/ui-e2e.sh`); CI runs in Debian 13 containers.

- phoned: calls (dial, answer, decline, hold/swap, waiting, DTMF, mute), texts
  (multipart, cleaning, deletion from the modem once stored), confirmations for
  outside requests and premium numbers, rate limits, blocking, USSD, call log,
  PipeWire call audio with echo cancellation, ringer, jack and headset buttons,
  Bluetooth (BlueZ agent), desktop notifications.
- Panel apps for FlipCTL: Phone, Messages, Phone Settings.
- dolphone-desktop for desktop mode, with tel: and sms: links.
- phonectl, fake-mm, an end-to-end suite, a Debian package and CI.

Fixed after ten reviews, before release:

- Security: reading messages and the call log, answering, keypad tones, audio
  routing, Bluetooth, the block list and settings are limited to Dolphone's own apps
  (and, for reading, to granted programs); signals carry no message text; trust is
  by full executable path; the trust-everything test switch is gone from release
  builds; the notification server is verified and notifications cannot allow a
  request; private notifications are the default.
- Texts: the network's timestamp is kept; duplicates are not stored twice; a text is
  deleted from the modem only once it is stored; delivery reports are not stored as
  texts; MMS notifications are left to mmsd-tng (a placeholder without it); flash texts are shown; multipart texts
  are waited for up to ten minutes.
- Calls: audio starts while an outgoing call rings (early media); a call that ended
  while phoned was down is logged as missed; every call ends cleanly when the modem
  goes away; the ringer stops after 90 s; supplementary-service codes are run as
  such instead of sent as USSD.
- Modem: a locked SIM or disabled modem is reported (and a disabled modem enabled);
  watchers restart when a subscription fails; SIGTERM is handled; an unreadable store
  is moved aside instead of stopping phoned; the store uses WAL with full sync.
- Reliability, found by running the suites with every core busy: a ModemManager
  lookup that times out is no longer taken as the modem leaving (which ended its
  calls); commands to baresip go one at a time, so "callfind" and the command after
  it cannot be split by another call's; a BlueZ that is installed but not running
  is not woken on every Bluetooth question (each waited out the bus's activation
  timeout, stalling Phone Settings); `otp --wait` sees a code that arrives while it
  is still connecting; baresip is restarted at once after a crash; the tests wait
  for states instead of sleeping, and their SIP audio uses ALSA's null device
  rather than baresip 1.1.0's aufile player, whose thread can outlive the player
  it plays for.
- Panel: names the panel cannot draw show as the number.
- Desktop: screen-reader roles and labels on every custom control; contrast raised to
  WCAG AA for secondary text and your own bubbles.
