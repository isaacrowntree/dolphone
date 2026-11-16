# Hardware bring-up: the go/no-go test

Everything in Dolphone runs against fake-mm. Whether it can be a phone on real
hardware comes down to five gates. Run them on an **RK3576 board from Flipper's
supported list** (or any arm64 Debian 13 machine) with a modem in an M.2 Key-B to USB
adapter and a SIM with VoLTE. Until a Flipper One exists, that board running Flipper
OS is the target.

**Which modem decides which gates apply.** Quectel says the **RM520N-GL** is
data-only, with no voice: on it G3 (texts) is the test, and G1, G2 and G4 do not
apply; Dolphone shows "texts only, no calls" and leaves the call setup alone. For
calls, use a modem with voice whose call audio reaches Linux: over USB (the SIMCom
SIM7600G-H-M2), or on the slot's audio pins if Flipper wires them (the Quectel EM12-G,
the Fibocom FM160-EAU) ([MODEMS.md](MODEMS.md)).
The Quectel commands below are for Quectel modems that have voice.

| Gate | Question | Pass |
|---|---|---|
| **G1 Call setup** | Does ModemManager place and receive VoLTE calls on your carrier? | `mmcli` calls connect both ways |
| **G2 Call audio** (decides it) | Can Linux hear and speak a call? | two-way audio through a USB audio card the modem exposes (UAC), or a wired PCM path on a modem that has one |
| **G3 Texts** | Do texts work over IMS on the same SIM? | send and receive |
| **G4 Carrier safety** | Will the carrier accept the device? | the IMEI is not blocked for lacking VoLTE emergency calling (Australia blocks such devices since the 3G shutdown: check with your carrier or ACMA) |
| **G5 Wake** | Does an incoming call wake the board from suspend? | (can wait for the Flipper One itself) |

**GO** when G1-G3 pass on at least one modem. **NO-GO** when G2 fails on every
candidate: publish what was measured and stop.

On another machine (a Raspberry Pi, a uConsole, a PinePhone, a Librem 5, a laptop),
the same gates are how its device profile goes from "untested" to "tested": run them,
note what passed, and send the profile's file back with its `status` and `notes`
updated (see [DEVICES.md](DEVICES.md)). `phonectl device` says which profile is in
use, and why.

## Before G0: prepare the board

Install Dolphone's package on the board first: several gates use `phonectl` and the
modem commands it brings, and the package also installs the polkit rule that lets
your session use the modem, and `dolphone-modem-access`.

```sh
# A release's .deb, or one built on any arm64 Debian 13 machine. Without
# flipctl-slint's tools there, leave the panel bundles out:
SKIP_BUNDLES=1 scripts/package-deb.sh
sudo apt install ./target/deb/dolphone_*_arm64.deb    # answer yes: AT commands
```

Log in again so phoned starts with your session (`systemctl --user status phoned`).
`scripts/dev-setup.sh` is for a machine without a modem: it refuses to run where it
finds one, since it swaps ModemManager for the fake.

## G0: the modem is there

```sh
sudo apt install modemmanager libqmi-utils
mmcli -L
mmcli -m any                       # model, firmware, state, signal
mmcli -m any --command='AT+QGMR' 2>/dev/null || sudo qmicli -p -d /dev/cdc-wdm0 --dms-get-revision
```

Write down the firmware revision: Quectel asks for it.

`mmcli --command` and Dolphone's AT commands (the modem setup, the Flipper One's
`AT+QPCMV=1,2` at every start, call forwarding codes) need ModemManager in **debug
mode**, the only mode in which it passes AT commands from a client, unless Dolphone
has a spare AT port (below). Installing the package asks whether to turn it on (yes
by default on a Flipper One, where call audio needs it). To change it later:

```sh
sudo dolphone-modem-access on      # or off, or status; dpkg-reconfigure dolphone asks again
```

Phone Settings > Modem offers **Turn on** too, with a password prompt.

What it costs: `--debug` also sets ModemManager's log level to DEBUG. ModemManager
hides numbers and message text from its QMI and MBIM log lines unless started
with `--log-personal-info` (since 1.20), but not from AT traffic, so with an AT
modem the journal can hold them. Removing the package turns it off again.

**Or a spare AT port**, with debug mode left off. The RM520N-GL (USB `2c7c:0801`)
has two AT ports: interface 2, which ModemManager makes its primary port, and
interface 3, the "modem" port, which takes AT commands too and which ModemManager
uses only as its secondary (Quectel's UMTS/LTE/5G Linux USB Driver User Guide V3.1,
Table 1; ModemManager's `77-mm-quectel-port-types.rules`). Dolphone can tell
ModemManager to ignore interface 3 (the `ID_MM_PORT_IGNORE` udev tag) and send its
commands there itself. Flipper's own
[RM520N-GL test](https://docs.flipper.net/one/testing/RM520N-GL) (firmware
`RM520NGLAAR03A04M4G`) found the same layout, ignored interface 3 the same way, and
reports that ModemManager loses "almost nothing" without it, keeping interface 2 and
QMI. Dolphone's own use of the port is untested, so check it here:

```sh
ls -d /sys/bus/usb/devices/*/ttyUSB*           # which ttyUSB is which interface (…:1.3 is 3)
sudo dolphone-modem-access port on             # then log in again if you just joined dolphone
udevadm info -q property -n /dev/ttyUSB3 | grep ID_MM_PORT_IGNORE   # =1
mmcli -m any | grep -A4 " ports:"              # ttyUSB3 no longer listed; ttyUSB2 still (at)
ls -l /dev/ttyUSB3                             # crw-rw---- root dolphone
phonectl modem setup                           # "at commands ... on phoned's own port /dev/ttyUSB3"
```

Record: whether ModemManager still finds the modem, places and receives calls
(G1) with the port gone, and still sees `RING` and new texts. Which port the modem
sends those unsolicited messages to (`AT+QURCCFG="urcport"`) is not in any RM520N
manual found; if ModemManager misses them, answer `AT+QURCCFG="urcport"` on
interface 3 and note it. If the interface numbers differ (another USB composition
from `AT+QCFG="usbcfg"`), the rule and the profile's `[at_port]` need the right one
([DEVICES.md](DEVICES.md)).

## G1: calls

```sh
mmcli -m any --voice-create-call="number=0491570156"     # a number you own
mmcli -o <call> --start
mmcli -m any --voice-list-calls                          # watch ringing-out, active
mmcli -o <call> --hangup
# Incoming: ring the SIM from another phone, then
mmcli -m any --voice-list-calls
mmcli -o <call> --accept
```

If calls fail to set up: `mmcli -m any | grep -i "carrier config"` should name your
carrier's VoLTE profile (Quectel calls it MBN). `AT+QMBNCFG="List"` shows the
profiles the firmware has.

Dolphone checks the setup a Quectel modem needs for calls and can make it:

```sh
phonectl modem setup           # USB audio, VoLTE, carrier profile: ok or change
phonectl modem setup apply     # waits for a press on the device; the modem restarts
```

| Step | Checked with | Changed with | Source |
|---|---|---|---|
| USB call audio | `AT+QCFG="usbcfg"` (the last function is UAC) | the same with it set to 1 | Quectel LTE Standard UAC Application Note §4.1 |
| VoLTE | `AT+QCFG="ims"` (`<setting>,<registered>`) | `AT+QCFG="ims",1` | Quectel forum (EC25, RM520N-GL threads) |
| Carrier profile | `AT+QMBNCFG="AutoSel"` | `AT+QMBNCFG="AutoSel",1` | Quectel forum |
| Restart | | `AT+CFUN=1,1` | 3GPP TS 27.007 |

None of the three is in the public RM5x0N AT manual (V1.0), so treat a "change"
on real firmware as something to confirm, and report what it answered. Phone
Settings > Modem shows the same check, with **Set up** when there is something to do.

## G2: call audio (the deciding one)

On a modem that can call, call audio comes over USB, as a sound card (UAC, the
Quectel way below) or as a serial PCM stream (SIMCom's, which ModemManager names
and phoned bridges), or on the M.2 slot's PCM pins (20, 22, 24 and 28, at 1.8 V, on
the Quectel EM12-G). The RM520N-GL has no voice at all (see above).

The Flipper One joins those pins to the RK3576's SAI0: its [device tree](https://github.com/flipperdevices/flipper-linux-kernel/blob/flipper-devel/arch/arm64/boot/dts/rockchip/rk3576-flipper-one.dtsi) enables SAI0 on
its `sai0m2` pins as "Serial audio for M.2 WWAN". It builds no sound card on it,
though, so a modem there has nowhere to send its audio yet.
[`dist/dts/rk3576-flipper-one-m2-pcm.dtso`](https://github.com/isaacrowntree/dolphone/blob/main/dist/dts/rk3576-flipper-one-m2-pcm.dtso)
(installed to `/usr/share/dolphone/dts/`) is an untested overlay that adds one,
"M.2 Modem PCM", with the modem driving the clocks. Before trying it:

- the kernel needs `CONFIG_SND_SOC_BT_SCO`, the generic PCM codec, which Flipper's
  kernel does not enable today;
- the EM12-G's PCM is 1.8 V, and Flipper's pinout and F0B0C1 M.2 schematic give
  GPIO_5 to GPIO_8 as 0/1.8 V too; the SoC side (GPIO1_A0, A1, A7 and B2) is not on
  a published sheet, so measure a pin before trusting it;
- the modem's PCM format (Quectel: `AT+QDAI`) must match the overlay's (DSP A,
  8 kHz, 16-bit mono, the modem as clock master).

Then `aplay -l` shows the card, and phoned bridges it to the codec like a USB modem's
with `~/.config/dolphone/audio.json` holding `"modem_match": ["M.2 Modem PCM"]`, and
the profile's `modem_at` emptied (`AT+QPCMV=1,2` would move a Quectel's audio to USB):
it is a profile setting, so copy `flipper-one.toml` to `~/.config/dolphone/devices/`,
where it replaces the built-in one, and empty it there. The pass is two-way audio through
the NAU8822.

1. Turn on UAC: `phonectl modem setup apply`, or by hand as Quectel's LTE Standard
   UAC Application Note describes: query `AT+QCFG="usbcfg"`, send the same values
   back with the last one set to 1, and restart (`AT+CFUN=1,1`). A firmware that
   lists only six functions cannot do it by AT command; ask Quectel for the
   RM520N-GL note and a firmware that can, with your revision.
2. Send voice to USB: `AT+QPCMV=1,2`. The modem forgets it at every restart; the
   Flipper One profile's `modem_at` has phoned send it each time the modem appears.
3. Check a new card appears:
   ```sh
   aplay -l; arecord -l             # a Quectel card
   pw-dump | grep -i quectel        # its PipeWire nodes
   ```
4. Make a call (G1) and route it by hand:
   ```sh
   pw-loopback --capture=<quectel source> --playback=<speaker or headset sink> &
   pw-loopback --capture=<microphone> --playback=<quectel sink> &
   ```
   Both people should hear each other. The note gives the format as 8 kHz, mono,
   16-bit; PipeWire converts.
5. Then let phoned do it: stop the two loopbacks, call again, and check
   `pgrep -a pw-loopback` shows the two phoned starts (names `dolphone-downlink` and
   `dolphone-uplink`). To find the Quectel card's node names for the step above,
   `wpctl status` or `pw-cli ls Node`.

If the card appears but carries silence, try the other UAC mode the note lists,
check `AT+QAUDMOD`, and capture `pw-top` during the call. Record what you tried.

## Call forwarding and caller ID

ModemManager has no API for these, so the dialer sends them as the standard AT
commands (3GPP TS 27.007), through the spare AT port when there is one, otherwise
ModemManager's debug-mode passthrough:

| Keypad | Does | Command |
|---|---|---|
| `**21*0412345678#` | forward every call | `AT+CCFC=0,3,"0412345678",129,1` |
| `**67*n#`, `**61*n#`, `**62*n#` | forward when busy, unanswered, unreachable | `AT+CCFC=1/2/3,3,...` |
| `**61*n**20#` | unanswered after 20 s | `...,1,,,20` |
| `*#21#` | is it on? | `AT+CCFC=0,2` |
| `#21#`, `##21#`, `##002#` | off, cleared, clear every forward | `AT+CCFC=…,0` / `,4` |
| `*#31#`, `#31#`, `*31#` | caller ID: ask, show, hide | `AT+CLIR?`, `=2`, `=1` |
| `*43#`, `#43#`, `*#43#` | call waiting | ModemManager's Voice API |

Barring codes (`*33*…#`) are refused: they need the network's barring password.
On real hardware, after setting a forward, ring the SIM from another phone and
check where the call goes.

## G3: texts

```sh
mmcli -m any --messaging-create-sms="number=0491570156,text='Dolphone test'"
mmcli -s <sms> --send
mmcli -m any --messaging-list-sms        # after texting the SIM from another phone
```

On Australian 4G-only networks texts travel over IMS too: if G1 needed a carrier
profile, G3 does as well.

Picture messages and group texts come over mobile data (MMS), so they need a data
connection and mmsd-tng:

```sh
sudo apt install mmsd-tng                # it finds most carriers' MMS settings itself
# Send the SIM a picture from another phone, then:
phonectl threads                         # the text, with [Picture]
phonectl mms 0491570156 --file test.jpg "Dolphone MMS test"
```

If nothing arrives, check `~/.mms/modemmanager/mms` against your carrier's MMSC,
proxy and APN, and `journalctl --user -u mmsd-tng`. Record what you changed.

## Then: Dolphone on real hardware

Install the package (see [install](install.md); `scripts/package-deb.sh` builds it on
the board), which brings the polkit rule that lets phoned use Voice and Messaging
from your session, and log in again:

```sh
phonectl status                          # your operator, signal, number
journalctl --user -u phoned -f           # what phoned makes of the modem
```

`scripts/dev-setup.sh` is for a machine without a modem: it refuses to mask
ModemManager where it finds one.

`phonectl cell` (after `phonectl set read_grants +phonectl`, allowed on the device)
lists the cells the firmware reports; whether the RM520N-GL fills in the 5G NR
fields is unconfirmed, so note what it shows.

On the Flipper One, the profile's `ring_leds` are the four RGB LEDs that Flipper's
LED driver names `flipper-one:rgb:link`, `wifi`, `eth1` and `eth0`, which Flipper's
image points at the network interfaces. Ring it and check that they flash, and that
afterwards each shows its interface's link again
(`cat /sys/class/leds/flipper-one:rgb:eth0/device_name` names it). If they stay dark,
FlipCTL's Settings > LED Brightness > Link may be Off.

The mixer commands assume ALSA names the codec's card `NAU8822` (`aplay -l`), and use
its pin switches (`amixer -c NAU8822 contents`); note any that fail in
`journalctl --user -u phoned`, and whether a headset's microphone is heard on a
call through the jack (the profile switches to it when the jack reports one).

## Report back

Open an issue titled `Hardware test: <modem> on <board>` with: modem and firmware,
carrier and country, which gates passed, the commands that worked, and for G2 the
card names and what you heard. A NO-GO report is as useful as a GO.
