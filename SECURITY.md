# Security

A phone can spend money, leak one-time codes and be used to harass. Dolphone runs on a
device whose owners install a lot of third-party tools, so it assumes some of what
runs beside it is careless or hostile.

## Threat model

| Threat | What Dolphone does |
|---|---|
| A program as the same user texts or dials (premium numbers, toll fraud, spam) | Anything not from Dolphone's own front ends waits for a press on the device or in the desktop app. Premium numbers (AU 19xx) and short codes wait for everyone. Texts are limited to 30 an hour and calls to 20. |
| A program texts without asking, once allowed | A **standing grant** is for one program, listed numbers or a prefix, a per-hour limit (1–60) and an end date (up to a year, or none), allowed on the device. A grant names a program by its executable's name, not its path: a grant to `phonectl` covers every script that runs phonectl. Premium, short and emergency numbers can never be granted. Anyone may revoke a grant; only Dolphone's own apps may list them. |
| A program changes your contacts (to make a scammer's number read "Bank", or to get past "silence unknown callers") | Adding, editing, removing or importing contacts from anything but Dolphone's own apps waits for a press on the device. Reading contacts needs the same grant as reading messages. A phonebook copy over Bluetooth is only started from Phone Settings. Imports keep names and numbers only. |
| A program reads your one-time codes | Messages, the call log and the block list are only served to Dolphone's own apps and to programs the owner grants reading (`read_grants`). The new-message signal, which everything on the bus sees, carries no text. **Same-user code can still read the store file directly**; see Limits. |
| A program answers a call it placed (the device as a room microphone), sends keypad tones, reroutes audio, pairs a Bluetooth headset or changes settings | Only Dolphone's own apps may. Anyone may hang up. |
| A program poses as the desktop's notification server (to read notifications or press their buttons) | phoned only uses a notification server that is a system program (under /usr, owned by root), only acts on button signals from that server, and no notification can allow a waiting request: Review opens the desktop app, and only Deny acts directly. |
| Someone on the network uses FlipCTL's browser view | That view has no login upstream: anyone who can reach it sees the screen and can press keys, including Allow. Keep it off, or bound to localhost, on untrusted networks; it is worth raising with Flipper on Discord. |
| A hostile text (control characters, bidi overrides that make a link read backwards, zero-width text, huge bodies) | Cleaned once on arrival, before the store, logs or screens; capped at 4 KB; drawn with ASCII fonts. Flash (class 0) texts are shown, not stored. |
| USSD codes that change account settings | Only run when typed on Dolphone's own dialer; never from D-Bus callers, never from links. |
| Emergency calls | Never blocked or delayed when dialled on the keypad; never sent over the internet; routed to the modem as normal calls, which the network treats as emergency calls. An outside program asking to call 000 must be confirmed. |
| Personal data outliving a reset | The store, contacts and MMS pictures live under `~/.local/share/dolphone` (0700/0600). `/home` survives a Flipper OS factory reset, so Settings has **Delete phone data**, confirmed on the device. It removes messages, calls, alerts, grants, the audit log, blocked numbers, settings, MMS pictures and contacts. It leaves the SIP account (`~/.config/dolphone/sip/`), rules, KDE Connect pairings, the gateway token and the store's key; delete `~/.config/dolphone` for those. |
| A copy of the home directory (a backup, another account, a disk read elsewhere) | `phoned --encrypt-store` encrypts the message store with SQLCipher under a random key that `systemd-creds --user` seals to this user on this machine (with the machine's root-only credential secret, and the TPM2 chip when there is one). A copy of the home directory alone cannot open it. Someone with the whole disk can, unless a TPM holds the secret or the disk is encrypted. If the key cannot be unsealed phoned leaves the store as it is and runs on an empty one in memory: new texts stay on the SIM, but until the key is back the block list, grants, speed dial and other settings are at their defaults, and calls are not logged. Contacts (`.vcf` files) and MMS pictures are not encrypted with it. |
| A program sends your files by MMS | `SendMms` from anything but Dolphone's own apps waits for a press on the device, which shows the files' names; grants never cover MMS. Files are read only once allowed, and at most 8 MB. |
| ModemManager's debug mode | Dolphone's AT commands (the modem's setup, call forwarding codes, the Flipper One's call-audio command) pass only in this mode, which also logs at debug level: ModemManager hides numbers and texts in its QMI and MBIM log lines, but not in AT traffic. It also lets **any program in your session send the modem AT commands** through ModemManager (`mmcli --command`; the polkit rule allows device control to the active local session), which goes around every confirmation here: dialling, texting, forwarding. The install asks (yes by default only on a Flipper One, where call audio needs it); `dolphone-modem-access off` turns it off, and removing the package does. Through Dolphone, call forwarding codes still only run from its own dialer. |
| A program changes a setting (who may read messages, the device profile, 2G) | `Settings.Set` is for Dolphone's own apps; anything else asks with `RequestSetting`, which waits for a press on the device showing the setting and its new value. The SIP password is never set that way. |
| A laptop over KDE Connect | It listens on the local network (UDP and TCP 1716–1764). Pairing is always confirmed on the device, and the approval pairs only the connection whose certificate asked; that certificate is remembered and must match. Requests are limited per device and per address, lines and connections are capped, and handshakes time out. A paired laptop reads your texts and texts as you (premium numbers and short codes still ask); with private notifications on, new texts are not pushed to it. Unpair it in Phone Settings > Laptops. |
| The SMS gateway | Off unless started (`systemctl --user enable --now dolphone-gateway`). It listens on 127.0.0.1; any other address needs `--insecure-listen`, since it speaks plain HTTP and the token would cross the network in clear. Every request needs the token in `~/.config/dolphone/gateway.token` (0600), requests and connections are limited, and it reads only with its own read grant. |
| Logs | Numbers are masked (`+61…156`); message bodies are never logged. |
| Knowing what programs did | Every outside request (dial, text, read, contacts, grants, answer attempts) goes in an **audit log** in the store, numbers masked, capped at 1000 entries, readable only from Dolphone's own apps (Phone Settings > Privacy). |
| A rule on incoming texts as a way in | Rules live in `~/.config/dolphone/rules.json`; unknown fields make the file invalid rather than quietly ignored. A rule runs a program directly (never a shell) with the text as JSON on stdin, for at most 30 s; it may reply only to a sender it names, never to "anyone", and a rule can require a shared secret in the text. Texts that arrive over SIP never run rules: a SIP sender is not authenticated the way a mobile network's is. |
| Someone who can reach baresip directly (the local network, by default) | A SIP peer counts as a phone number, and so as a contact, only when it comes from the account's own domain or proxy; from anywhere else it stays a SIP address, in a thread of its own. Texts over SIP are limited to 30 an hour. |
| A program poses as mmsd-tng | phoned uses the MMS service only when the program owning its bus name is a system program (under /usr, owned by root), as for the notification server. |
| The SIP account | The password is written to `~/.config/dolphone/sip/` (0700, file 0600) and never returned over D-Bus (`Get` answers "set"). Only Dolphone's own apps can set it. The account and proxy are checked so they cannot add options to baresip's account line. phoned listens only to the baresip process it started, so another program claiming baresip's bus name cannot fake calls. Media is encrypted (SRTP) when the provider offers it. Emergency calls never go over SIP. |
| Emergency alerts | National alerts cannot be turned off. Alerts are shown and sounded whatever the ringer says, and their text is always shown (they are public). |
| Bluetooth | The pairing agent accepts only the device picked on a Dolphone screen, for a minute; any other device asking then, a keyboard included, is refused. |

## Limits

"Dolphone's own front end" is decided by the executable behind a D-Bus caller, by
full path: `/usr/bin/dolphone-*` owned by root, or the same name inside an
AppImage's FUSE mount (the panel apps run from `~/Apps`). The `DOLPHONE_TRUST_ALL`
and `DOLPHONE_READ_GRANTS` test switches exist only in debug builds. This stops
scripts and tools that use the API, and makes every such action visible, but it is
a speed bump. Code running as your user can still:

- mount a FUSE filesystem of its own holding a program named like a front end
  (unless the `trust_appimages` setting is off, which trusts only `/usr/bin`);
- connect to the bus and then run a real front end on that connection (the bus
  records the process that connected, and after exec it is the real program);
- watch the whole session bus (`dbus-monitor`), which shows message text on its
  way to the apps, notification bodies and pairing passkeys;
- write Dolphone's own files: the store (grants, settings), `rules.json`, KDE
  Connect's paired certificates, contact cards;
- read the store file (or, with it encrypted, unseal its key the way phoned does),
  ptrace the apps, or call ModemManager directly (polkit allows the active local
  session).

Signals, which every program on the bus receives, carry no message text, but they
do carry who: the number on a new call or text, and the requests waiting. Separating
users, or a sandbox around third-party tools, is the real fix; ROADMAP.md lists the
next steps.

## Reporting

Use GitHub's private vulnerability reporting: the repository's **Security** tab,
then **Report a vulnerability**. Please do not open a public issue for something
that lets a program text, dial or read messages without confirmation.
