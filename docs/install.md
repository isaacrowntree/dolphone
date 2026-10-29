# Install

## On the Flipper One

Dolphone installs on Flipper OS (Debian 13, arm64) from the Debian package attached
to each [release](https://github.com/isaacrowntree/dolphone/releases) (until the first one,
build it with `scripts/package-deb.sh` on the device or any arm64 Debian 13 machine):

```sh
sudo apt install ./dolphone_0.1.0_arm64.deb
dolphone-install-apps            # as yourself: Apps > Phone on the panel
phonectl device                  # should say Flipper One
```

The package brings:

| | |
|---|---|
| `phoned` | the phone daemon, started in every user session (systemd user unit, D-Bus activated) |
| `phonectl` | the command line, for scripts |
| `dolphone-desktop` | the desktop-mode app, with `tel:` and `sms:` links |
| `dolphone-install-apps` | copies the panel apps (Phone, Messages, Phone Settings) into `~/Apps/Phone` |
| device profiles | `/usr/share/dolphone/devices/`: how each machine's audio is wired ([Devices](DEVICES.md)) |
| PipeWire and WirePlumber config | an echo canceller for speakerphone; Bluetooth hands-free |
| udev rules | let the modem wake the board for a call or a text, the `dolphone` group flash the Flipper One's LEDs, and (when chosen) give that group a spare AT port of the modem's |
| `dolphone-modem-access` | turns ModemManager's debug mode on or off, or gives Dolphone a spare AT port instead (see below) |
| a polkit rule | lets your active local session use the modem's calls, texts, USSD and device control (which, in debug mode, includes AT commands) |

It needs ModemManager, PipeWire and WirePlumber (pulled in by apt), and conflicts
with GNOME Calls and Chatty, which would race it for every call and text. It
recommends, and apt installs unless told not to:

| Package | For |
|---|---|
| `baresip-core` | calls over the internet (SIP) |
| `bluez`, `bluez-obexd` | Bluetooth headsets, and copying contacts from your phone |
| `mmsd-tng` | picture messages and group texts (MMS) |
| `pipewire-pulse` | SIP call audio (baresip plays through PipeWire's Pulse server) |
| `alsa-utils` | the mixer commands device profiles run |
| `wl-clipboard` or `xclip` | Copy in the desktop app |
| `espeak-ng` (suggested) | saying who is calling |

### The one question

The install asks: **Let Dolphone send setup commands to the modem?** Dolphone sets
the modem up for calls, tells it where call audio goes each time it starts, and runs
call forwarding and caller-ID codes, all as AT commands, which ModemManager passes on
only in its debug mode. Debug mode also makes ModemManager's log more detailed, and
with an AT modem that log can hold numbers and texts. It also lets any program
running as you send the modem AT commands through ModemManager (`mmcli --command`),
around Dolphone's confirmations. On a Flipper One call audio needs it, so yes is the
default there; elsewhere it is no. Change it any time:

```sh
sudo dolphone-modem-access on        # or off, or status
sudo dpkg-reconfigure dolphone       # ask again
```

On a Quectel RM520N-GL or RM500Q there is another way, not yet tried on a real modem: a spare
AT port that ModemManager is told to leave alone, which phoned uses itself, so debug
mode can stay off. It is not part of the question, since it depends on the modem:

```sh
sudo dolphone-modem-access port on   # or port off
sudo dolphone-modem-access off       # debug mode, if it was on
sudo adduser $USER dolphone          # the port's group; then log in again
phonectl modem setup                 # says which way the commands go
```

[Devices](DEVICES.md) has the details, and how to do it for another modem.

Phone Settings > Modem shows **Turn on** when it is off, with a password prompt.

## After installing

- **Contacts**: in Phone Settings > Bluetooth, pair your current phone and press
  View on it to copy its contacts (allow it on the phone); or
  `phonectl contacts import contacts.vcf` with an export from contacts.google.com or
  icloud.com; or drop `.vcf` files into `~/.local/share/dolphone/contacts/`. Only
  names and numbers are kept.
- **Voicemail**: Phone Settings > Calls > Voicemail number (some carriers' numbers
  are known and filled in from the SIM).
- **Calls over the internet**: Phone Settings > Calls over the internet, with your
  SIP provider's address, password and (if they give one) proxy.
- **Board audio**: if the speaker or the jack stays silent, the profile's mixer
  controls do not match your board; see [Devices](DEVICES.md) and [Audio](AUDIO.md).
- **Modem setup**: Phone Settings > Modem says whether the modem is set up for calls
  (USB audio, VoLTE, the carrier profile); **Set up** makes the changes and restarts
  it. `phonectl modem setup` is the same from a shell.
- **Picture messages**: with `mmsd-tng` installed, MMS just arrives. If it does not,
  the carrier's MMS settings (MMSC, proxy, APN) are in `~/.mms/modemmanager/mms`
  (stop mmsd-tng before editing it).
- **LEDs for a ringing call**: `sudo adduser $USER dolphone` and log in again; they
  flash while a call rings (the `flash_on_ring` setting, on by default).
- **An encrypted message store**: `systemctl --user stop phoned`, then
  `phoned --encrypt-store`, then `systemctl --user start phoned`. The key is sealed
  by systemd to you on this machine; `phoned --decrypt-store` undoes it. See
  [Security](https://isaacrowntree.com/dolphone/security.html) for what it does and does not protect.
- **Scripts that read messages** (an OTP reader, say) need a read grant:
  `phonectl set read_grants phonectl`, allowed on the device. Grants go by program
  name, so a grant to `phonectl` is a grant to every script that uses it. **Scripts that text** without asking each
  time need a standing grant: `phonectl grant <numbers> <per-hour> <days>`, allowed
  once on the device.

## On other machines

The same package (arm64), or the amd64 one for a laptop, installs on any Debian 13
machine; the panel apps are only in the arm64 package and only useful under FlipCTL.
`phonectl device` says which profile phoned picked; [Devices](DEVICES.md) has what
works where and how to add a machine.

## Without any hardware

The whole stack runs against **fake-mm**, a scriptable stand-in modem, on an Apple
Silicon Mac with OrbStack or any arm64 Debian 13 machine. See [Developing](DEVELOPING.md).
