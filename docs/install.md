# Install

Dolphone installs on Flipper OS, or on any arm64 Debian 13 (trixie) machine, from the
Debian package attached to each [release](https://github.com/isaacrowntree/dolphone/releases).

```sh
sudo apt install ./dolphone_0.1.0_arm64.deb
dolphone-install-apps            # as yourself: Apps > Phone on the panel
```

The package brings:

| | |
|---|---|
| `phoned` | the phone daemon, started in every user session (systemd user unit, D-Bus activated) |
| `phonectl` | the command line, for scripts |
| `dolphone-desktop` | the desktop-mode app, with `tel:` and `sms:` links |
| `dolphone-install-apps` | copies the panel apps (Phone, Messages, Phone Settings) into `~/Apps/Phone` |
| PipeWire and WirePlumber config | an echo canceller for speakerphone; Bluetooth hands-free |
| a polkit rule | lets your active local session use the modem's calls and texts |

It needs ModemManager, PipeWire and WirePlumber (pulled in by apt), and conflicts
with GNOME Calls and Chatty, which would race it for every call and text.

## After installing

- **Contacts**: copy `.vcf` files into `~/.local/share/dolphone/contacts/`.
- **Board audio**: if the speaker or the jack stays silent, the mixer control names
  differ on your board; copy `/usr/share/doc/dolphone/audio.example.json` to
  `~/.config/dolphone/audio.json` and adjust (see [Audio](AUDIO.md)).
- **Scripts that read messages** (an OTP reader, say) need a grant: Phone Settings,
  or `read_grants` set to the program's name.

## Without a Flipper One

The whole stack runs against **fake-mm**, a scriptable stand-in modem, on an Apple
Silicon Mac with OrbStack or any arm64 Debian 13 machine. See [Developing](DEVELOPING.md).
