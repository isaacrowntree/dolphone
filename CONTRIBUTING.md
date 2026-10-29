# Contributing

Thanks for wanting to help. The most valuable thing right now is **hardware**: if you
have an RK3576 board and an M.2 modem, run [docs/HARDWARE.md](https://isaacrowntree.com/dolphone/HARDWARE.html) and
report what happened, GO or NO-GO. On another machine with a modem (a Raspberry
Pi, a uConsole, a PinePhone, a Librem 5, a laptop), running the same checks moves its
device profile from "untested" to "tested", and a new machine is one TOML file:
[docs/DEVICES.md](https://isaacrowntree.com/dolphone/DEVICES.html).

## Ground rules

- **Add to FlipCTL, never replace it.** Screens use FlipCTL's widgets, fonts and
  soft-key roles ([docs/BUTTONS.md](https://isaacrowntree.com/dolphone/BUTTONS.html)). If something should change in
  FlipCTL itself, that is a contribution to flipctl-slint, not a workaround here.
- **The Flipper One comes first.** A feature is designed for its panel and its
  buttons; other machines get it through their device profile and the desktop app.
  Hardware facts in a profile cite where they came from.
- **phoned stays the one owner** of calls, texts and call audio. Front ends talk to
  it, never to ModemManager or PipeWire.
- **Tests come with changes.** Logic in `model.rs` files is tested directly; screens
  are rendered (`DOLPHONE_RENDER=1 cargo test`) and looked at; daemon behaviour goes in
  `scripts/e2e.sh`; a new flow through an app goes in its `live_*` test
  (`scripts/ui-e2e.sh`).
- Run `cargo fmt`, `cargo clippy --all-targets -- -D warnings` and the tests before a
  pull request; CI runs the same.

## AI-assisted contributions

Welcome, on the terms Flipper Devices sets for its own repositories: you must
understand, test and be able to defend every line yourself, and answer review
yourself. Pull requests that relay a tool's output unread will be closed.

## Talking to Flipper Devices

Dolphone is unofficial. Please do not open issues on Flipper's repositories about
Dolphone; their contribution rules ask for discussion on Discord and for designs as
comments on their "help wanted" tasks. Changes we want upstream go there as small,
self-contained pull requests.

## Conduct

Be kind and assume good faith; see [CODE_OF_CONDUCT.md](https://github.com/isaacrowntree/dolphone/blob/main/CODE_OF_CONDUCT.md).
