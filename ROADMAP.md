# Roadmap

Everything that can be built without the hardware is built: see CHANGELOG.md.
This is what is left, and why.

## 0. A real call on a Flipper One

Everything has been tested against stand-ins: fake-mm for ModemManager, fake-mmsd
for mmsd-tng, a second baresip for SIP, a pty for a SIM7600's serial audio.
[docs/HARDWARE.md](https://isaacrowntree.com/dolphone/HARDWARE.html) is the go/no-go test on the hardware; its
deciding question is whether a modem that can call gives Linux two-way call audio.
Quectel says the RM520N-GL is data-only, so it can test texts and data but not calls
([docs/MODEMS.md](https://isaacrowntree.com/dolphone/MODEMS.html) lists the
candidates for calls). What only a board can confirm:

- **A call over the M.2 slot's audio pins.** The two modems Telstra certifies for
  VoLTE that fit the slot as they are, the Quectel EM12-G and the Fibocom FM160-EAU,
  put call audio on pins 20, 22, 24 and 28. Flipper's device tree joins those to the
  SoC's SAI0 ("Serial audio for M.2 WWAN") but builds no sound card on it;
  `dist/dts/rk3576-flipper-one-m2-pcm.dtso` adds one. A board must show that it
  loads (it needs `CONFIG_SND_SOC_BT_SCO`, not in Flipper's kernel today), that the
  pins are 1.8 V at the SoC as the pinout says, and that audio passes both ways.
  It applies cleanly to Flipper's F0B1C2 device tree (`fdtoverlay`). Until then the
  SIMCom SIM7600G-H-M2, whose call audio comes over USB, is the safe M.2 choice.

- **The Quectel commands.** `phonectl modem setup` checks USB audio
  (`AT+QCFG="usbcfg"`), VoLTE (`AT+QCFG="ims"`) and the carrier profile
  (`AT+QMBNCFG="AutoSel"`), and the Flipper One profile sends `AT+QPCMV=1,2` at
  every start. They come from Quectel's UAC application note for its LTE modules and
  from Quectel's forum, not from the RM520N's own AT manual, so their answers on real
  firmware are the first thing to record.
- **The Flipper One itself.** Its device-tree strings, LED names, sound card and
  headset jack now come from Flipper's kernel (branch `flipper-devel`), so the
  profile no longer guesses them. A board must confirm that ALSA calls the card
  `NAU8822` (the mixer commands use it), that the pin switches route speaker, jack
  and both microphones as expected, and that the network LEDs show their links
  again after a ring.
- **MMS on a carrier**: mmsd-tng fetches over mobile data with the carrier's MMSC
  and APN, which it mostly finds by itself; nothing here has met a real MMSC.
- **AT commands without ModemManager's debug mode.** Built: a spare AT port that
  ModemManager is told to ignore (`ID_MM_PORT_IGNORE`), which phoned talks to itself
  (`sudo dolphone-modem-access port on`; the Flipper One profile names the
  RM520N-GL's interface 3). Flipper's own RM520N-GL test confirms interface 3 is the
  secondary AT port, ignores it the same way, and finds ModemManager loses "almost
  nothing" without it. Dolphone's use of it is tested only against a pty, and the
  RM520N cannot call, so whether ModemManager still sees `RING` with it gone waits
  for a modem that can (which port gets the unsolicited messages,
  `AT+QURCCFG="urcport"`, is not in any manual found).

## 1. Optional, later

- **The panel outside Dolphone's apps**: a status-bar badge for missed calls and
  texts, a toast, a home tile; a large-text mode and a font beyond Latin and
  Cyrillic (Dolphone draws with FlipCTL's fonts, and shows the number where a name
  cannot be drawn). FlipCTL has none of these today. They belong in
  [FlipCTL](https://github.com/flipperdevices/flipctl-slint) itself, for every app, but Dolphone could also add
  them as options of its own; it could go either way.
- No RCS: there is no route to it from Linux. Bridges to Matrix and others belong
  outside Dolphone, on its D-Bus API.

## 2. Not built yet

Nothing else that can be built without the hardware.

