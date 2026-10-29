#!/usr/bin/env bash
# Provision a development machine for Dolphone: an arm64 Debian 13 (trixie) box,
# which is what Flipper OS is built on. Run from a Mac with OrbStack:
#
#     scripts/dev-setup.sh            # creates the `f1` machine and provisions it
#     MACHINE=other scripts/dev-setup.sh
#
# or, already inside any arm64 trixie machine (a board, a VM, CI):
#
#     scripts/dev-setup.sh --here
#
# It installs the toolchain and the audio stack, clones and builds flipctl-slint
# (tracking its main branch: upstream moves and so do we), fetches its pinned
# AppImage tools, and swaps the real ModemManager for fake-mm. It never touches a
# machine with a real modem unless asked to, because of that last step.
set -euo pipefail

MACHINE=${MACHINE:-f1}
HERE=$(cd "$(dirname "$0")/.." && pwd)

if [ "${1:-}" != --here ]; then
    command -v orb >/dev/null || { echo "OrbStack's orb is not installed; run with --here inside the machine" >&2; exit 1; }
    if ! orb list 2>/dev/null | awk '{print $1}' | grep -qx "$MACHINE"; then
        echo "== creating $MACHINE (debian:trixie, arm64) =="
        orb create debian:trixie "$MACHINE"
    fi
    # The Mac's home is visible inside the machine at the same path.
    exec orb -m "$MACHINE" bash -lc "cd '$HERE' && scripts/dev-setup.sh --here"
fi

if [ -e /dev/cdc-wdm0 ] || ls /dev/wwan* >/dev/null 2>&1; then
    if [ -z "${FORCE_FAKE_MODEM:-}" ]; then
        echo "a real modem is attached; refusing to mask ModemManager (FORCE_FAKE_MODEM=1 to insist)" >&2
        exit 1
    fi
fi

echo "== packages =="
sudo env DEBIAN_FRONTEND=noninteractive apt-get update -qq
sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq \
    build-essential git curl pkg-config libssl-dev file desktop-file-utils squashfs-tools \
    sway fuse3 dbus-user-session modemmanager \
    pipewire pipewire-pulse wireplumber pipewire-bin \
    bluez baresip python3 >/dev/null

if ! command -v cargo >/dev/null && [ ! -x "$HOME/.cargo/bin/cargo" ]; then
    echo "== rust =="
    curl -sSf https://sh.rustup.rs | sh -s -- -y -q
fi
# shellcheck disable=SC1091
. "$HOME/.cargo/env"
rustup component add rustfmt clippy >/dev/null 2>&1 || true

echo "== flipctl-slint (main) =="
if [ ! -d "$HOME/flipctl-slint/.git" ]; then
    git clone -q https://github.com/flipperdevices/flipctl-slint "$HOME/flipctl-slint"
else
    git -C "$HOME/flipctl-slint" pull -q --ff-only
fi
(cd "$HOME/flipctl-slint" &&
    CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 \
    cargo build -q --release -p flipctl --features device,slint,remote,wayland)
# Building one upstream app with the bundler fetches its pinned tools, which
# scripts/bundle.sh then uses for ours.
[ -x "$HOME/flipctl-slint/target/appimage/tools/appimagetool-aarch64" ] ||
    (cd "$HOME/flipctl-slint" && tools/appimage/build.sh --native apps/sysmon >/dev/null)

echo "== fake modem in place of ModemManager =="
sudo systemctl stop ModemManager 2>/dev/null || true
sudo systemctl mask ModemManager >/dev/null 2>&1 || true
sudo install -m 0644 "$HERE/dist/dev/dolphone-fake-mm.conf" /etc/dbus-1/system.d/
sudo systemctl reload dbus

# RTKit cannot run inside an OrbStack (LXC) machine, and while D-Bus keeps trying to
# start it every PipeWire client waits out a 25 s timeout. Masked, the request fails
# at once and PipeWire runs without real-time priority, which a test box can spare.
if systemd-detect-virt -cq 2>/dev/null; then
    sudo systemctl mask rtkit-daemon >/dev/null 2>&1 || true
fi

# A user manager that outlives the shell, so phoned and fake-mm keep running.
sudo loginctl enable-linger "$USER"

echo
echo "ready. Next: scripts/run-dev.sh (inside the machine) to build, install and start everything,"
echo "then open http://$(hostname).orb.local:8899/ for FlipCTL's browser view."
