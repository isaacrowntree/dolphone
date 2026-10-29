# Changelog

## 0.1.0 (unreleased)

First version, tested against fake-mm and in the real FlipCTL; not yet on a real modem.

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
  texts; MMS notifications show a placeholder; flash texts are shown; multipart texts
  are waited for up to ten minutes.
- Calls: audio starts while an outgoing call rings (early media); a call that ended
  while phoned was down is logged as missed; every call ends cleanly when the modem
  goes away; the ringer stops after 90 s; supplementary-service codes are refused
  instead of sent as USSD.
- Modem: a locked SIM or disabled modem is reported (and a disabled modem enabled);
  watchers restart when a subscription fails; SIGTERM is handled; an unreadable store
  is moved aside instead of stopping phoned; the store uses WAL with full sync.
- Panel: names the panel cannot draw show as the number.
- Desktop: screen-reader roles and labels on every custom control; contrast raised to
  WCAG AA for secondary text and your own bubbles.
