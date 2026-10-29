# Hardware bring-up: the go/no-go test

Everything in Dolphone runs against fake-mm. Whether it can be a phone on real
hardware comes down to five gates. Run them on an **RK3576 board from Flipper's
supported list** (or any arm64 Debian 13 machine) with a **Quectel RM520N-GL** in an
M.2 Key-B to USB adapter and a SIM with VoLTE. Until a Flipper One exists, that board
running Flipper OS is the target.

| Gate | Question | Pass |
|---|---|---|
| **G1 Call setup** | Does ModemManager place and receive VoLTE calls on your carrier? | `mmcli` calls connect both ways |
| **G2 Call audio** (decides it) | Can Linux hear and speak a call? | two-way audio through a USB audio card the modem exposes (UAC), or a wired PCM path on a modem that has one |
| **G3 Texts** | Do texts work over IMS on the same SIM? | send and receive |
| **G4 Carrier safety** | Will the carrier accept the device? | the IMEI is not blocked for lacking VoLTE emergency calling (Australia blocks such devices since the 3G shutdown: check with your carrier or ACMA) |
| **G5 Wake** | Does an incoming call wake the board from suspend? | (can wait for the Flipper One itself) |

**GO** when G1-G3 pass on at least one modem. **NO-GO** when G2 fails on every
candidate: publish what was measured and stop.

## G0: the modem is there

```sh
sudo apt install modemmanager libqmi-utils
mmcli -L
mmcli -m any                       # model, firmware, state, signal
mmcli -m any --command='AT+QGMR' 2>/dev/null || sudo qmicli -d /dev/cdc-wdm0 --dms-get-revision
```

Write down the firmware revision: Quectel asks for it.

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

## G2: call audio (the deciding one)

The RM520N-GL has no PCM pins (they are reserved on the GL variant; only the -EU
variant has them), so call audio has to come over USB as a sound card.

1. Ask Quectel for the "RM520N-GL UAC Application Note" (forum or distributor; it
   is not public), with your firmware revision.
2. Enable UAC as the note says. It is an `AT+QCFG="USBCFG",...` setting and a modem
   reset; the exact values are in the note.
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
   Both people should hear each other. If the modem needs voice routed to USB per
   call, Quectel's note names the command (`AT+QPCMV` on related modules).
5. Then let phoned do it: `scripts/run-dev.sh` with `REAL_MODEM=1`, call, and check
   `pgrep -a pw-loopback` shows the two loopbacks it starts.

If the card appears but carries silence, try the other UAC mode the note lists,
check `AT+QAUDMOD`, and capture `pw-top` during the call. Record what you tried.

## G3: texts

```sh
mmcli -m any --messaging-create-sms="number=0491570156,text='Dolphone test'"
mmcli -s <sms> --send
mmcli -m any --messaging-list-sms        # after texting the SIM from another phone
```

On Australian 4G-only networks texts travel over IMS too: if G1 needed a carrier
profile, G3 does as well.

## Then: Dolphone on real hardware

```sh
scripts/dev-setup.sh --here              # without the fake modem: FORCE_FAKE_MODEM is not set
REAL_MODEM=1 scripts/run-dev.sh
phonectl status                          # your operator, signal, number
```

Install the polkit rule (`dist/polkit/50-dolphone.rules`, the .deb does) so phoned
may use Voice and Messaging from your session.

## Report back

Open an issue titled `Hardware test: <modem> on <board>` with: modem and firmware,
carrier and country, which gates passed, the commands that worked, and for G2 the
card names and what you heard. A NO-GO report is as useful as a GO.
