# Audio

phoned owns call audio. FlipCTL pins a hosted app's sound to the panel speaker and
keeps it away from the sound card, so no app could route a call even if it wanted to.

## Routes

| Route | Output | Microphone | Echo cancelled |
|---|---|---|---|
| Speaker | the codec, speaker side (`dolphone_ec_sink`) | the device mic (`dolphone_ec_source`) | yes, WebRTC in PipeWire |
| Headset (3.5 mm) | the codec, headphone side | the headset's mic (TRRS) | no |
| Bluetooth | the headset's sink, in its hands-free profile | the headset's mic | the headset's own |

A mobile call is two `pw-loopback`s phoned starts when the call connects:

```
dolphone-downlink:  modem capture  ─▶ output for the route
dolphone-uplink:    mic for route  ─▶ modem playback
```

Mute stops the uplink only. Changing route mid-call restarts both. They stop when
the last call ends. An outgoing call's audio starts while it rings out, for the
network's own ringback and announcements.

A **SIP call**'s audio is baresip's: it plays and records through PipeWire itself,
and phoned points it at the route's output and microphone (`auplay`, `ausrc`) when
the call connects and whenever the route changes. Mute is baresip's mute.

## What switches routes

- **Edit** in a call cycles Speaker → Headset → Bluetooth, skipping what is not
  connected (and the desktop app's route menu sets one).
- A headset plugged into the jack takes the call; unplugging it falls back to the
  speaker. phoned reads `SW_HEADPHONE_INSERT` from whichever input device reports it,
  and notices a device that goes away and comes back.
- A Bluetooth headset connecting takes the call; it disconnecting falls back to the
  jack or the speaker.
- A route the user picked by hand stays until the call ends, except that a device
  disappearing always falls back.

## The machine's own wiring: device profiles

Which mixer controls switch speaker and headphones, which sink and source to use,
how to spot the modem's audio card, and anything to run as a call starts and ends
are the **device profile**'s ([DEVICES.md](DEVICES.md)). On the **Flipper One** that
is the NAU8822's `Speaker Playback Switch` and `Headphone Playback Switch`, switched
with `amixer`, because the driver has no UCM profile; its jack and headset button are
watched by the RP2350, which passes them on. A phone whose modem is wired to the
codec (the PinePhone) needs no loopbacks, only its UCM Voice Call verb, which its
profile runs with `call_start_commands`.

`~/.config/dolphone/audio.json` (example in `dist/audio.example.json`) overrides any
key of the profile, for the machine in front of you.

Some modems send call audio over a serial port instead of a sound card: the
SIMCom SIM7600 in the uConsole's 4G extension sends 8 kHz mono 16-bit PCM on its
audio port once `AT+CPCMREG=1` is sent (SIMCom's USB AUDIO Application Note V1.03,
§2). A profile's `serial_pcm` names the port (or ModemManager's AudioPort is used),
its `call_at_start` and `call_at_end` turn the stream on and off, and phoned bridges
it with two `pw-cat` processes, one each way, as nodes `dolphone-downlink` and
`dolphone-uplink`. The port needs the `dialout` group.

## Ringing and tones

phoned synthesises its sounds into `$XDG_RUNTIME_DIR/dolphone/` and plays them with
`pw-play`:

- the **ring** (400 ms on, 200 off, 400 on, 2 s off, at 400+450 Hz with a little of
  the octave so it carries from a small speaker) and the call-waiting pips, in the
  Notification role; the Dialer runs the motor alongside, and PTT silences;
- the **text tone**, when the ringer is on;
- the **alert tone** for emergency alerts (853 + 960 Hz, the attention signal alert
  systems use), in the Alarm role, whatever the ringer says;
- the **call-progress tones** of the country the SIM (or the owner's setting) says
  you are in: busy and congestion after the network turns a call away, and, for
  networks that send silence while ringing out, ringback (Phone Settings > Calls >
  Ringback on this phone). The cadences are ITU-T E.180's, country by country, in
  `phone-core/src/tones.rs`;
- the caller's name, spoken with espeak-ng when "Say who is calling" is on.

Music playing on the machine (anything speaking MPRIS) pauses when a call rings and
resumes when the last call ends.

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
