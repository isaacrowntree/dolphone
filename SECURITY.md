# Security

A phone can spend money, leak one-time codes and be used to harass. Dolphone runs on a
device whose owners install a lot of third-party tools, so it assumes some of what
runs beside it is careless or hostile.

## Threat model

| Threat | What Dolphone does |
|---|---|
| A program as the same user texts or dials (premium numbers, toll fraud, spam) | Anything not from Dolphone's own front ends waits for a press on the device or in the desktop app. Premium numbers (AU 19xx) and short codes wait for everyone. Texts are limited to 30 an hour and calls to 20. |
| A program reads your one-time codes | Messages, the call log and the block list are only served to Dolphone's own apps and to programs the owner grants reading (`read_grants`). The new-message signal, which everything on the bus sees, carries no text. **Same-user code can still read the store file directly**; see Limits. |
| A program answers a call it placed (the device as a room microphone), sends keypad tones, reroutes audio, pairs a Bluetooth headset or changes settings | Only Dolphone's own apps may. Anyone may hang up. |
| A program poses as the desktop's notification server (to read notifications or press their buttons) | phoned only uses a notification server that is a system program (under /usr, owned by root), only acts on button signals from that server, and no notification can allow a waiting request: Review opens the desktop app, and only Deny acts directly. |
| Someone on the network uses FlipCTL's browser view | That view has no login upstream: anyone who can reach it sees the screen and can press keys, including Allow. Keep it off, or bound to localhost, on untrusted networks; it is worth raising with Flipper on Discord. |
| A hostile text (control characters, bidi overrides that make a link read backwards, zero-width text, huge bodies) | Cleaned once on arrival, before the store, logs or screens; capped at 4 KB; drawn with ASCII fonts. Flash (class 0) texts are shown, not stored. |
| USSD codes that change account settings | Only run when typed on Dolphone's own dialer; never from D-Bus callers, never from links. |
| Emergency calls | Never blocked or delayed when dialled on the keypad; never sent over the internet; routed to the modem as normal calls, which the network treats as emergency calls. An outside program asking to call 000 must be confirmed. |
| Personal data outliving a reset | The store and contacts live under `~/.local/share/dolphone` (0700/0600). `/home` survives a Flipper OS factory reset, so Settings has **Delete phone data**, confirmed on the device. |
| Logs | Numbers are masked (`+61…156`); message bodies are never logged. |
| Bluetooth | The pairing agent only accepts a pairing started from a Dolphone screen in the last minute. |

## Limits

"Dolphone's own front end" is decided by the executable behind a D-Bus caller, by
full path: `/usr/bin/dolphone-*` owned by root, or the same name inside an
AppImage's mount (the panel apps run from `~/Apps`). The `DOLPHONE_TRUST_ALL` and
`DOLPHONE_READ_GRANTS` test switches exist only in debug builds. This stops scripts
and tools that use the API, and makes every such action visible, but it is a speed
bump: code running as your user can fake an AppImage mount under `/tmp`, read the
SQLite file, ptrace the apps, or call ModemManager directly (polkit allows the
active local session). Separating users, or a sandbox around third-party tools, is
the real fix; ROADMAP.md lists the next steps.

## Reporting

Use GitHub's private vulnerability reporting: the repository's **Security** tab,
then **Report a vulnerability**. Please do not open a public issue for something
that lets a program text, dial or read messages without confirmation.
