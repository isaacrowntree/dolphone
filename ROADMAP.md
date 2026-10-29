# Roadmap

Ten reviews of Dolphone (as an everyday phone, for Flipper power users, by Linux
phone veterans, for accessibility, messaging, telephony, desktop integration,
reliability, privacy, and "does it feel like a phone on this machine") produced a
long list. All of it that can be built without the hardware is built: see
CHANGELOG.md. This is what is left, and why.

## 0. A real call on a Flipper One

Everything has been tested against stand-ins: fake-mm for ModemManager, fake-mmsd
for mmsd-tng, a second baresip for SIP, a pty for a SIM7600's serial audio.
[docs/HARDWARE.md](https://isaacrowntree.com/dolphone/HARDWARE.html) is the go/no-go test on the hardware; its
deciding question is whether the RM520N-GL's USB audio gives Linux two-way call
audio. What only a board can confirm:

- **The Quectel commands.** `phonectl modem setup` checks USB audio
  (`AT+QCFG="usbcfg"`), VoLTE (`AT+QCFG="ims"`) and the carrier profile
  (`AT+QMBNCFG="AutoSel"`), and the Flipper One profile sends `AT+QPCMV=1,2` at
  every start. They come from Quectel's UAC application note for its LTE modules and
  from Quectel's forum, not from the RM520N's own AT manual, so their answers on real
  firmware are the first thing to record.
- **The Flipper One itself**: its device-tree strings (the profile assumes them;
  `phonectl device use flipper-one` overrides), the names of its four LEDs, and how
  its RP2350 reports the headset jack.
- **MMS on a carrier**: mmsd-tng fetches over mobile data with the carrier's MMSC
  and APN, which it mostly finds by itself; nothing here has met a real MMSC.

## 1. Needs someone else first

- **FlipCTL**: a status-bar badge for missed calls and texts, a toast, and a home
  tile, so the news reaches the panel outside Dolphone's apps; a large-text mode and
  a fallback font beyond Latin and Cyrillic (Dolphone draws with FlipCTL's fonts, and
  shows the number where a name cannot be drawn). Worth raising with Flipper.
- **AT commands without ModemManager's debug mode.** ModemManager passes a client's
  AT commands only with `--debug` (or when built with `at_command_via_dbus`), which
  also raises its log level. Dolphone asks at install and makes it one command to
  change, but a ModemManager API for the few commands it needs, or a spare AT port
  it leaves alone, would be better. Both need upstream ModemManager or a real board.
- No RCS: there is no route to it from Linux. Bridges to Matrix and others belong
  outside Dolphone, on its D-Bus API.

## 2. Not built yet

- **Attaching a picture from the apps.** Pictures that arrive are shown (in the
  desktop app; named on the panel), and replies to a group go to everyone, but
  sending a picture is `phonectl mms` or the D-Bus API. The desktop app needs a file
  picker (a portal), and the panel has nowhere to pick from.
- **MMS pictures encrypted with the store.** The store's messages are encrypted when
  the owner asks (`phoned --encrypt-store`); the picture files beside it are not, and
  rely on full-disk encryption.
- **From the second round of reviews, still to do:**
  - Signals carry who is calling or texting (no text), which every program on the
    bus sees; sending them only to Dolphone's own apps needs a change to how front
    ends subscribe.
  - A pending request exits phonectl with 0; distinct exit codes, and `--json`
    everywhere, would make scripts simpler (`phonectl wait` covers the waiting).
  - A newer emergency alert replaces one not yet dismissed; they should queue, the
    most severe on top. The alert tone is 4 s (the US asks for about 10.5 s).
  - Text-to-911 where carriers support it (texts to emergency numbers are refused).
  - Serial call audio has no buffer for the two clocks drifting apart over a long
    call; a ring buffer that drops or pads would.
  - Some idle polling remains (the jack every 5 s, the uplink every 10 s, KDE
    Connect's discovery); events would be cheaper.
- **A Messages PIN.** The store's key is sealed by systemd to this user on this
  machine, which keeps a copy of the home directory unreadable; a PIN would also
  keep out someone holding the unlocked device. Same-user code can still read the
  screen and the bus either way.
