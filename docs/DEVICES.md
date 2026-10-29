# Devices

Dolphone is built for the **Flipper One**: its panel apps are FlipCTL apps, its
buttons are the Flipper's, and [HARDWARE.md](HARDWARE.md) is the Flipper One's
bring-up. Underneath, though, phoned only needs what any Linux machine with a modem
has: ModemManager for the modem, PipeWire for audio, BlueZ for Bluetooth, logind
and D-Bus. So the daemon, `phonectl` and the desktop app run elsewhere too.

What differs between machines is the hardware around the modem, and that lives in
a **device profile**: one TOML file that says how to recognise the machine and how
its audio is wired.

## Which one am I using?

```sh
phonectl device          # the profile in use, and why it was chosen
phonectl device list     # every profile phoned knows
phonectl device use pinephone     # choose one by hand (auto to detect again)
```

Phone Settings > About shows it on the panel too.

## The profiles that ship

| Profile | Recognised by | Status | Notes |
|---|---|---|---|
| `flipper-one` | device tree model "Flipper One" | expected | The reference. NAU8822 codec switched with amixer (`Speaker`, `Headphone`, from the kernel driver's controls); the modem's USB audio bridged by phoned. The board's device-tree strings are not published yet, so detection is assumed: choose it by hand if it misses. |
| `clockworkpi-uconsole` | `dtoverlay=clockworkpi-uconsole` in config.txt | untested | The 4G extension is a SIMCom SIM7600G-H, which sends call audio as serial PCM: phoned turns it on with `AT+CPCMREG=1` when a call connects and bridges `/dev/ttyUSB4` to PipeWire at 8 kHz (SIMCom USB AUDIO Application Note V1.03). Needs ModemManager in debug mode for the AT command, and you in the `dialout` group for the port. |
| `raspberry-pi` | device tree model "Raspberry Pi" | untested | Any modem ModemManager supports. Mobile call audio needs a modem with a USB sound card; use a USB headset for the speaker and microphone. |
| `pinephone` | `pine64,pinephone` | untested | The EG25-G is wired to the codec; ALSA UCM's Voice Call verb connects it (`call_start_commands`). No earpiece route yet. |
| `librem5` | `purism,librem5` | untested | The BM818 is its own ALSA card ("Modem"), bridged to the codec by phoned, as on the Flipper One. No earpiece route yet. |
| `laptop` | SMBIOS chassis type (laptop, notebook, convertible), or ThinkPad | untested | The desktop app. Whether the WWAN modem can call and text depends on it: `mmcli -m 0` lists Voice and Messaging when it can. SIP works regardless. |
| `generic` | nothing else matched | expected | Default devices, no mixer controls touched. |

"Untested" means the values come from the sources each file cites (kernel device
trees, alsa-ucm-conf, vendor documentation), but nobody has yet run the bring-up
checks on that machine. Running them and reporting back, pass or fail, is the most
useful contribution there is.

On the phones (PinePhone, Librem 5), stop callaudiod and GNOME Calls first: one
program should own the modem's calls and texts, and the package conflicts with
GNOME Calls and Chatty for that reason.

## Adding a device

Copy `/usr/share/dolphone/devices/generic.toml` to `~/.config/dolphone/devices/`,
give it an id, and fill in what you know:

```toml
id = "my-board"
name = "My Board"
status = "untested"              # tested once docs/HARDWARE.md passes on it
source = "where the values came from"
notes = "what works, what does not"
front_ends = ["desktop"]         # "flipctl" too, if it runs FlipCTL

[match]                          # any one of these picks the profile
dt_compatible = ["vendor,board"]      # exact strings from /proc/device-tree/compatible
dt_model = ["My Board"]               # substrings of /proc/device-tree/model
dmi_vendor = []                       # substrings, on machines with DMI (x86)
dmi_product = []
dmi_family = []
dmi_version = []
dmi_chassis = []                      # SMBIOS chassis types: 9 laptop, 10 notebook...
file_contains = [["/etc/hostname", "myboard"]]
priority = 50                         # higher wins when several match

[audio]                          # any key of dist/audio.example.json
codec_sink = ""                       # PipeWire node names; empty for the default
mic_source = ""
speaker_commands = [["amixer", "-q", "sset", "Speaker", "unmute"]]
wired_commands = [["amixer", "-q", "sset", "Headphone", "unmute"]]
modem_match = ["Quectel"]             # how to spot the modem's USB audio nodes
call_start_commands = []              # a UCM verb, an AT command, an amplifier
call_stop_commands = []
assume_wired = false
```

Restart phoned (or `phonectl device use my-board`), place a call, and check with
`phonectl device` and `journalctl --user -u phoned`. When it works, a pull request
adding the file to `dist/devices/` (and to the list in `crates/phoned/src/devices.rs`)
brings it to everyone; the unit tests check that every profile parses, is described,
and can be detected.

### Flashing LEDs for a ringing call

A profile's `ring_leds` names LEDs under `/sys/class/leds` to flash while a call
rings (the `flash_on_ring` setting, on by default). Their files belong to root, so the package's
`/usr/lib/udev/rules.d/70-dolphone-leds.rules` gives them to the `dolphone` group,
for the Flipper One's LEDs (`flipper-one:*`). Join the group once:

```sh
sudo adduser $USER dolphone      # then log in again
```

On another machine, copy the rule to `/etc/udev/rules.d/`, change its `KERNEL`
match to your LEDs' names (`ls /sys/class/leds`), and run
`sudo udevadm control --reload-rules && sudo udevadm trigger --subsystem-match=leds`.

### Where the facts are

- `cat /proc/device-tree/compatible | tr '\0' '\n'` and `/proc/device-tree/model`
  on ARM boards; `/sys/class/dmi/id/*` on x86.
- `aplay -l` and `wpctl status` for the sound cards and PipeWire's nodes.
- `amixer scontrols` for the mixer controls, or the codec's driver in the kernel
  (`sound/soc/codecs/`).
- `/usr/share/alsa/ucm2/` for a UCM profile and its verbs.
- `mmcli -m 0` for what the modem offers, and `lsusb` for its USB functions (a
  "USB Audio" interface is what phoned can bridge).

### Order of precedence

Built-in profiles, then `/usr/share/dolphone/devices/`, then
`~/.config/dolphone/devices/` (a later file with the same id replaces an earlier
one). The owner's `device_profile` setting picks one by id; otherwise the best match
is used. The owner's own `~/.config/dolphone/audio.json` still has the last word on
any audio key.
