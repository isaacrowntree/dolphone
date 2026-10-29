# Developing

## The machine

Dolphone builds and runs on arm64 Debian 13 (trixie), which is what Flipper OS is
built on. On an Apple Silicon Mac, OrbStack runs that natively:

```sh
scripts/dev-setup.sh        # creates `f1`, installs packages and Rust, builds FlipCTL
                            # (tracking its main branch), swaps in fake-mm
```

Your Mac's home is visible inside the machine at the same path, so edit on the Mac
and build inside. Keep build output inside the machine (`CARGO_TARGET_DIR` under
`~`): building on the shared folder is slower. After edits that make files longer,
the machine can briefly see them cut short ("unclosed delimiter" at the end of a
file); `scripts/run-dev.sh` drops the page cache first, or run
`sync; echo 3 | sudo tee /proc/sys/vm/drop_caches`. Never run `cargo fmt` inside
the machine right after a big edit on the Mac for the same reason.

## Running everything

```sh
orb -m f1                     # a shell in the machine, at the same directory
scripts/run-dev.sh            # build, install to ~/.local, (re)start fake-mm, phoned, FlipCTL
```

Then open `http://f1.orb.local:8899/`: FlipCTL's browser view, with the device's
buttons. Apps > Phone has the three apps.

```sh
phonectl fake ring 0491570156          # the phone rings; the Dialer comes up by itself
phonectl fake sms 0491570156 "hello"
phonectl fake answer | hangup | busy   # the far end
phonectl fake signal 30 5G             # FlipCTL's own status bar follows
phonectl watch                         # every event phoned sends
journalctl --user -u phoned -f
```

fake-mm is close enough to ModemManager that `mmcli -m 0`, `mmcli -m 0
--voice-list-calls` and FlipCTL's status bar all read it.

## Tests

```sh
cargo test --workspace                       # core, daemon, CLI, desktop
scripts/e2e.sh                               # 48 checks: fake-mm + phoned + phonectl on real buses
cd apps/dialer && DOLPHONE_RENDER=1 cargo test  # behaviour, and every screen to target/render/
python3 tools/showcase.py                    # README pictures from the renders
```

CI runs all of it on GitHub's arm64 runners, and a nightly job rebuilds the panel
apps against flipctl-slint's newest commit.

## Panel bundles and the package

```sh
scripts/bundle.sh             # target/bundles/dolphone-*-aarch64.fap.AppImage (upstream's own tooling)
INSTALL=1 scripts/bundle.sh   # and into ~/Apps/Phone
scripts/package-deb.sh        # target/deb/dolphone_<version>_arm64.deb
scripts/source-tarball.sh     # the GPL corresponding source for a release
scripts/gen-third-party-licenses.sh   # after any dependency change
```

## Upstream

The panel apps depend on `flipctl-app` from flipctl-slint's main branch by git. Move
to upstream's newest with `cargo update -p flipctl-app -p flipper-ui` in each app;
the nightly job does this every night. A release records the exact upstream commit
it was built against.

## Conventions

Comments explain why, in full sentences. Every panel position is computed in Rust
and text is measured in the font that draws it. Nothing blocking runs on an async
thread. A user-facing string says what to do, not what went wrong inside.
