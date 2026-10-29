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

## 1. Optional, later

- **The panel outside Dolphone's apps**: a status-bar badge for missed calls and
  texts, a toast, a home tile; a large-text mode and a font beyond Latin and
  Cyrillic (Dolphone draws with FlipCTL's fonts, and shows the number where a name
  cannot be drawn). FlipCTL has none of these today; Dolphone can add them as
  options of its own later, without waiting for FlipCTL.
- **AT commands without ModemManager's debug mode.** ModemManager passes a client's
  AT commands only with `--debug` (or when built with `at_command_via_dbus`), which
  also raises its log level. Dolphone asks at install and makes it one command to
  change, but a ModemManager API for the few commands it needs, or a spare AT port
  it leaves alone, would be better. Both need upstream ModemManager or a real board.
- No RCS: there is no route to it from Linux. Bridges to Matrix and others belong
  outside Dolphone, on its D-Bus API.

## 2. Not built yet

Nothing else that can be built without the hardware.

