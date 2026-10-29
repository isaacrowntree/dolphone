# Audio

phoned owns call audio. FlipCTL pins a hosted app's sound to the panel speaker and
keeps it away from the sound card, so no app could route a call even if it wanted to.

## Routes

| Route | Output | Microphone | Echo cancelled |
|---|---|---|---|
| Speaker | the codec, speaker side (`dolphone_ec_sink`) | the device mic (`dolphone_ec_source`) | yes, WebRTC in PipeWire |
| Headset (3.5 mm) | the codec, headphone side | the headset's mic (TRRS) | no |
| Bluetooth | the headset's sink, switched to its hands-free profile for the call | the headset's mic | the headset's own |
| Earpiece (phones that have one) | the codec's earpiece, switched by the profile's `earpiece_commands` | the device mic | no |

A mobile call is two `pw-loopback`s phoned starts when the call connects:

```
dolphone-downlink:  modem capture  ─▶ output for the route
dolphone-uplink:    mic for route  ─▶ modem playback
```

Mute stops the uplink only. Changing route mid-call restarts only the loopback
whose end moved (speaker to headset on the codec moves neither: only the mixer
commands run), so the other side of the call is not cut. They stop when the last
call ends. An outgoing call's audio starts while it rings out, for the network's
own ringback and announcements.

Starting and stopping are asked for without waiting and carried out in order on
one audio thread, so a call that ends quickly cannot have its stop run before its
start. Each loopback's streams carry `node.dont-reconnect` and `node.dont-fallback`,
so when their device goes WirePlumber ends them rather than moving them to the
default device (wireplumber's `linking/prepare-link.lua`); during a call phoned
checks every couple of seconds and starts again any loopback that has died, which
also brings a call's audio back after PipeWire restarts.

On the **speaker** the call goes through the echo canceller
(`dist/pipewire/dolphone-echo-cancel.conf`). Its own streams name no device, so
when a speaker call starts phoned points them at the profile's `codec_sink` and
`mic_source` (`target.object` in PipeWire's default metadata, which WirePlumber
honours: `linking/find-defined-target.lua`); otherwise they would follow the
default sink and source.

On **Bluetooth** a headset is usually in A2DP, which has no microphone. When a call
starts there phoned switches it to the highest-priority profile with an input
route (`headset-head-unit`, HFP/HSP, as WirePlumber's own
`device/autoswitch-bluetooth-profile.lua` picks), with
`pw-cli set-param <device> Profile '{ index: N, save: false }'` so the choice is not
remembered, waits for the headset's microphone to appear, and starts the
loopbacks. The old profile comes back when the call ends or leaves Bluetooth.

A **SIP call**'s audio is baresip's: it plays and records through PipeWire itself,
and phoned points it at the route's output and microphone (`auplay`, `ausrc`) when
the call connects and whenever the route changes. Mute is baresip's mute.

## What switches routes

- **Edit** in a call cycles Earpiece → Speaker → Headset → Bluetooth, skipping what
  is not there; a call starts on the earpiece where the profile has one, otherwise
  on the speaker (and the desktop app's route menu sets one).
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
SIMCom SIM7600 in the uConsole's 4G extension sends mono 16-bit PCM on its audio
port once `AT+CPCMREG=1` is sent (SIMCom's USB AUDIO Application Note V1.03, §2).
ModemManager's SIMCom plugin does that itself (`plugins/simtech/mm-shared-simtech.c`):
it sends `AT+CPCMREG=1` and `=0` as a call's audio starts and ends, names the port
as the call's AudioPort, and reports the rate in its AudioFormat, 8 kHz or 16 kHz
where the firmware has `AT+CPCMFRM=1`. So the profile sends nothing, and needs no
ModemManager debug mode. phoned uses the call's AudioPort, or else the profile's
`serial_pcm`, and bridges it with two `pw-cat` processes at that rate (8 kHz when
none is reported), one each way, as nodes `dolphone-downlink` and
`dolphone-uplink`. The port is flushed each time a leg starts, so stale bytes
never play as noise. The port needs the `dialout` group.

The modem's own sound card, where it has one, is given the lowest
`priority.session` by `dist/wireplumber/51-dolphone-bluetooth.conf`, so it never
becomes the default sink or source.

## Ringing and tones

phoned synthesises its sounds into `$XDG_RUNTIME_DIR/dolphone/` (or
`~/.cache/dolphone/` without a runtime directory; never a shared directory, and
always 0700) and plays them with `pw-play`:

- the **ring** (400 ms on, 200 off, 400 on, 2 s off, at 400+450 Hz with a little of
  the octave so it carries from a small speaker), in the Notification role, on the
  profile's `codec_sink` (the default sink may be an idle Bluetooth headset or the
  modem). While it rings the profile's `speaker_commands` run, since a profile
  whose jack route mutes the speaker would otherwise ring silently; afterwards,
  and after a call, the codec goes back to the jack if a headset is in it and to
  the speaker if not. The Dialer runs the motor alongside, and PTT silences;
- the call-waiting pips, in the ear: on the call's output;
- the **text tone**, when the ringer is on;
- the **alert tone** for emergency alerts (853 + 960 Hz, the attention signal alert
  systems use), in the Alarm role, whatever the ringer says;
- the **call-progress tones** of the country the SIM (or the owner's setting) says
  you are in: busy and congestion after the network turns a call away, and, for
  networks that send silence while ringing out, ringback (Phone Settings > Calls >
  Ringback on this phone). The cadences are ITU-T E.180's, country by country, in
  `phone-core/src/tones.rs`. They play on the call's output (after the call, the
  route's), not the default sink;
- the caller's name, spoken with espeak-ng when "Say who is calling" is on. The
  name goes to espeak on its stdin, not its command line (which any user can read
  in `/proc`), and the speech is deleted once played.

Music playing on the machine (anything speaking MPRIS) pauses when a call rings and
resumes when the last call ends.

## Bluetooth headsets

`dist/wireplumber/51-dolphone-bluetooth.conf` turns on the hands-free Audio Gateway
roles and PipeWire's ModemManager integration, so a headset rings and its button
works, and mSBC for wideband (16 kHz) call audio where the headset has it. Pairing goes through phoned's BlueZ agent, which only accepts a pairing the
owner started from a Dolphone screen in the last minute, and shows the passkey there.

## Lessons kept

- PipeWire clients in a systemd **user** unit with any sandboxing option
  (ProtectSystem, NoNewPrivileges, PrivateTmp) stall for seconds to 25 s. phoned's
  unit has none.
- `pw-dump` into a pipe can take seconds where the same dump to a file takes
  milliseconds; phoned dumps to a file, with a time limit.
- Every command phoned runs for audio (mixer commands, `pw-cli`, `pw-dump`) has a
  time limit, since they run with the audio state locked and a stalled one would
  hold up the call.
- In containers where RTKit cannot run (OrbStack), every PipeWire client waits out
  RTKit's D-Bus timeout; the dev setup masks `rtkit-daemon`.
