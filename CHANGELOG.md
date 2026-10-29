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
  (its debug mode), and `dolphone-modem-access on|off` changes it. Or, without
  debug mode, phoned sends them on a spare AT port that ModemManager is told to
  ignore (`dolphone-modem-access port on`, a udev rule for the RM520N-GL's
  interface 3, the profile's `[at_port]`, the `at_port` setting); `phonectl modem
  setup` says which way they go. Tested against a pty in fake-mm, not a modem.
- Call audio: a SIM7600's serial PCM bridged to PipeWire at 8 kHz (the uConsole's
  4G extension); an earpiece route for the PinePhone and Librem 5; holding and
  swapping between a SIP call and a mobile call.
- Texts: search (FTS5) and export as SMS Backup & Restore XML; receiving texts over SIP; the
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
  off. A program asking to change a setting (`phonectl set`, `phonectl device use`)
  waits for a press on the device, as texts and calls do; Delete phone data removes
  contacts too. The panel apps no longer link the database at all.
- Tests: the end-to-end suite grew to 224 checks, with 10 more against the real KDE
  Connect daemon; the four apps are driven live against phoned
  (`scripts/ui-e2e.sh`); CI runs in Debian 13 containers; the suite also passes
  with every CPU core busy.

Fixed after twenty more reviews (security from a program and from the network,
robustness, concurrency, data, packaging, the modem, audio, emergency calling,
telephony standards, numbers abroad, accessibility, the scripting API, the tests,
onboarding, FlipCTL's conventions, licensing, battery, failure recovery, privacy):

- Security: an AppImage front end must be a real FUSE mount; a SIP peer is a
  contact's number only from the account's provider, and SIP texts run no rules;
  Bluetooth pairing accepts only the device picked; KDE Connect binds approval to
  the asking certificate, caps lines, connections and requests, and times out
  handshakes; the gateway refuses non-loopback addresses without
  `--insecure-listen`, caps connections and has deadlines; mmsd-tng must be a
  system program; notifications escape markup; grant prompts say which numbers,
  how often and for how long; live calls' numbers and others' requests are masked
  for programs that may not read; the SIM's PIN and PUK codes are handled on the
  device instead of sent as USSD.
- Emergency: national numbers for NZ, India, Singapore, France, Russia, Brazil,
  China, South Africa; `+61 000` and the like; an emergency call with the SIM
  locked; Australia's Emergency Alert texts (0444 444 444) never blocked and
  sounded through silent; alerts shown even when they cannot be stored; channel 919.
- Calls and texts: a call adopted after a restart gets its audio; a call that can
  no longer be followed ends instead of making later calls "waiting"; a
  ModemManager restart or a pulled SIM ends the old calls; audio starts and stops in
  order; SIP crashes are noticed; Bluetooth calls switch the headset to its
  hands-free profile; the speaker route and the ringer play where they should;
  caller-ID hiding survives a modem restart; delivery reports wait out temporary
  errors; segment counts match how parts are packed; alphanumeric senders keep
  their names; two different international numbers are never one contact.
- Data: a store is moved aside only when damaged, with its WAL; contact edits keep
  every other line of a card and write atomically; MMS pictures cannot overwrite
  each other and are kept by mmsd-tng until all are saved; texts left sending
  become failed and can be sent again.
- Packaging and licences: the LED udev rule works (it never ran its commands);
  upgrades keep ModemManager running and the owner's debug-mode answer;
  SQLCipher's notice, the desktop app's GPL, the AppImage runtime's notices; the
  source tarball builds offline.
- Battery: the apps redraw and poll only when something changed; contacts are read
  once; phoned wakes less when idle.

The rest of that roadmap:

- Signals with numbers in them go in full only to Dolphone's own apps and
  programs granted reading (they register with `Settings.Register`); everyone
  else gets the number masked.
- A Messages PIN (4 to 12 digits, kept as a PBKDF2 hash): Messages and its
  notifications stay closed without it, it opens for a few minutes at a time, and
  wrong guesses slow down after five.
- MMS pictures go into the encrypted store with the texts (`--encrypt-store` moves
  them in, `--decrypt-store` back out).
- Text-to-911 in the US (47 CFR 9.10(q)); texts to emergency numbers elsewhere are
  still refused.
- `phonectl` exits with a code that says why (2 not allowed, 3 bad argument or not
  supported, 4 rate-limited, 5 phoned not running), and `--json` for `get`,
  `ussd` and what it sends.
- The uplink watch, KDE Connect and the headphone jack wake on changes instead of
  every few seconds.
- A PIN screen in Messages (panel and desktop) and PIN settings in both Settings.
- Emergency alerts queue, the most severe first ("1 of 3"); in the US the
  attention tone is the 10.5 s pattern of 47 CFR 10.520.
- The desktop app attaches pictures through the file-chooser portal.
- Serial call audio keeps the modem's clock and PipeWire's apart with a small
  buffer that drops or pads whole frames, so a long call does not drift.

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
