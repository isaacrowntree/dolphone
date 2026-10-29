#!/usr/bin/env bash
# Build everything, install it into the current user's session, and (re)start it:
# fake-mm, phoned, and FlipCTL headless with its browser view. Run inside the dev
# machine (scripts/dev-setup.sh made it), from the repository root:
#
#     scripts/run-dev.sh              # then open http://f1.orb.local:8899/
#     NO_BUNDLE=1 scripts/run-dev.sh  # daemons only, keep the installed apps
#     REAL_MODEM=1 scripts/run-dev.sh # use the machine's ModemManager, no fake-mm
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
FLIPCTL=${FLIPCTL:-$HOME/flipctl-slint}
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$HOME/dolphone-target}
PORT=${PORT:-8899}
# shellcheck disable=SC1091
. "$HOME/.cargo/env"

# The repository is usually the Mac's folder seen through OrbStack's share, which can
# keep a file's old length cached after an edit on the Mac makes it longer: the build
# then sees it cut short ("unclosed delimiter"). Dropping the page cache refreshes it.
if [ -d /mnt/mac ] || [ -n "${ORBSTACK:-}" ] || systemd-detect-virt -cq 2>/dev/null; then
    sync
    echo 3 | sudo tee /proc/sys/vm/drop_caches >/dev/null 2>&1 || true
fi

echo "== daemons =="
cargo build -q --release --manifest-path "$ROOT/Cargo.toml" -p phoned -p phonectl -p fake-mm
mkdir -p "$HOME/.local/bin" "$HOME/.config/systemd/user/phoned.service.d" "$HOME/.local/share/dbus-1/services"
install -m 0755 "$CARGO_TARGET_DIR"/release/{phoned,phonectl,fake-mm} "$HOME/.local/bin/"
install -m 0644 "$ROOT/dist/systemd/phoned.service" "$ROOT/dist/dev/fake-mm.service" "$HOME/.config/systemd/user/"
install -m 0644 "$ROOT/dist/dbus-1/io.github.isaacrowntree.Dolphone.service" "$HOME/.local/share/dbus-1/services/"
# The unit names /usr/bin/phoned, where a package puts it; a dev install is in ~/.local/bin.
# On the device flipctl is /usr/bin/flipctl; here it is the checkout's build, linked
# where the daemon's PATH will find it for `flipctl open`.
ln -sf "$FLIPCTL/target/release/flipctl" "$HOME/.local/bin/flipctl"
printf '[Service]\nExecStart=\nExecStart=%%h/.local/bin/phoned\nEnvironment=PATH=%%h/.local/bin:/usr/local/bin:/usr/bin:/bin\n' > "$HOME/.config/systemd/user/phoned.service.d/dev.conf"
systemctl --user daemon-reload
if [ -z "${REAL_MODEM:-}" ]; then
    systemctl --user restart fake-mm.service
fi
systemctl --user restart phoned.service
sleep 1
"$HOME/.local/bin/phonectl" status

if [ -z "${NO_BUNDLE:-}" ]; then
    echo "== apps =="
    INSTALL=1 "$ROOT/scripts/bundle.sh" | grep -E "^== [0-9.]+[KM]" || true
fi

echo "== flipctl on :$PORT =="
pkill -x flipctl 2>/dev/null || true
sleep 0.5
# Detached from this shell entirely (its own session, no inherited stdin), so the
# script returns while flipctl keeps running.
(cd "$FLIPCTL" && WLR_RENDERER=pixman setsid nohup ./target/release/flipctl --headless --remote "0.0.0.0:$PORT" </dev/null >/tmp/flipctl.log 2>&1 &)
sleep 1.5
grep -E "remote view|apps " /tmp/flipctl.log || true
echo
echo "FlipCTL: http://$(hostname).orb.local:$PORT/   (Apps > Phone)"
echo "Ring it: phonectl fake ring 0491570156     Text it: phonectl fake sms 0491570156 hello"
