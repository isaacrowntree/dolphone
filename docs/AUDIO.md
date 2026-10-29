# Audio

phoned owns call audio. FlipCTL pins a hosted app's sound to the panel speaker and
keeps it away from the sound card, so no app could route a call even if it wanted to.

## Routes

| Route | Output | Microphone | Echo cancelled |
|---|---|---|---|
| Speaker | the codec, speaker side (`dolphone_ec_sink`) | the device mic (`dolphone_ec_source`) | yes, WebRTC in PipeWire |
| Headset (3.5 mm) | the codec, headphone side | the headset's mic (TRRS) | no |
| Bluetooth | the headset's sink, in its hands-free profile | the headset's mic | the headset's own |

A call is two `pw-loopback`s phoned starts when the call connects:

```
dolphone-downlink:  modem capture  ─▶ output for the route
dolphone-uplink:    mic for route  ─▶ modem playback
```

Mute stops the uplink only. Changing route mid-call restarts both. They stop when
the last call ends.

## What switches routes

- **Edit** in a call cycles Speaker → Headset → Bluetooth, skipping what is not
  connected (and the desktop app's route menu sets one).
- A headset plugged into the jack takes the call; unplugging it falls back to the
  speaker. phoned reads `SW_HEADPHONE_INSERT` from whichever input device reports it.
- A Bluetooth headset connecting takes the call; it disconnecting falls back to the
  jack or the speaker.
- A route the user picked by hand stays until the call ends, except that a device
  disappearing always falls back.

## Board configuration

The NAU8822 driver has no UCM profile and no jack detection, so speaker and
headphone switching is mixer commands, and which mixer controls depends on the board.
`~/.config/dolphone/audio.json` (example in `dist/audio.example.json`) holds them,
along with the sink and source to use and the names that identify the modem's card.
The defaults use the NAU8822's own control names and are **unverified on the Flipper
One** until its driver is in hand.

## Ringing

phoned synthesises the ring (Australia's 400 ms on, 200 off, 400 on, 2 s off, at
400+450 Hz with a little of the octave so it carries from a small speaker) and the
call-waiting pips into `$XDG_RUNTIME_DIR/dolphone/`, and plays them with `pw-play` in
the Notification role. The Dialer runs the vibration motor alongside. PTT silences.

## Bluetooth headsets

`dist/wireplumber/51-dolphone-bluetooth.conf` turns on the hands-free Audio Gateway
roles and PipeWire's ModemManager integration, so a headset rings and its button
works. Pairing goes through phoned's BlueZ agent, which only accepts a pairing the
owner started from a Dolphone screen in the last minute, and shows the passkey there.

## Lessons kept

- PipeWire clients in a systemd **user** unit with any sandboxing option
  (ProtectSystem, NoNewPrivileges, PrivateTmp) stall for seconds to 25 s. phoned's
  unit has none.
- `pw-dump` into a pipe can take seconds where the same dump to a file takes
  milliseconds; phoned dumps to a file, with a time limit.
- In containers where RTKit cannot run (OrbStack), every PipeWire client waits out
  RTKit's D-Bus timeout; the dev setup masks `rtkit-daemon`.
