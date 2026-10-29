#!/usr/bin/env bash
# The apps themselves against a live phoned and fake-mm: each app's `live_*` test
# drives its real input path, waits on phoned's signals, and saves what the screen
# shows at each step to target/render/live/. Run inside the dev machine (see
# scripts/dev-setup.sh), from the repository root:
#
#     scripts/ui-e2e.sh
#
# Like scripts/e2e.sh it needs the fake modem to be the only ModemManager on the
# system bus, and no other phoned running.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
TARGET=${CARGO_TARGET_DIR:-$ROOT/target}
APPS_TARGET=${APPS_TARGET:-$HOME/dolphone-apps-target}
BIN="$TARGET/debug"

if [ -z "${DOLPHONE_UI_INNER:-}" ]; then
    if pgrep -x phoned >/dev/null; then
        echo "another phoned is running; stop it first (systemctl --user stop phoned; pkill -x phoned)" >&2
        exit 2
    fi
    (cd "$ROOT" && cargo build -q -p phoned -p phonectl -p fake-mm)
    pkill -x fake-mm 2>/dev/null || true
    sleep 0.3
    "$BIN/fake-mm" >/tmp/dolphone-ui-fake-mm.log 2>&1 &
    FAKE=$!
    trap 'kill $FAKE 2>/dev/null || true' EXIT
    sleep 0.8
    kill -0 "$FAKE" 2>/dev/null || { cat /tmp/dolphone-ui-fake-mm.log >&2; exit 2; }
    export XDG_DATA_HOME XDG_RUNTIME_DIR XDG_CONFIG_HOME
    XDG_DATA_HOME=$(mktemp -d)
    XDG_CONFIG_HOME=$(mktemp -d)
    XDG_RUNTIME_DIR=$(mktemp -d)
    # A contact, so names show where a phone would show them.
    mkdir -p "$XDG_DATA_HOME/dolphone/contacts"
    printf 'BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Alex Example\r\nTEL;TYPE=CELL:+61491570156\r\nEND:VCARD\r\n' \
        >"$XDG_DATA_HOME/dolphone/contacts/alex.vcf"
    DOLPHONE_UI_INNER=1 dbus-run-session -- "$0"
    exit $?
fi

export PATH="$BIN:$PATH"
# The apps run as Dolphone's own front ends would (debug builds only).
DOLPHONE_TRUST_ALL=1 "$BIN/phoned" >/tmp/dolphone-ui-phoned.log 2>&1 &
PHONED=$!
trap 'kill $PHONED 2>/dev/null; wait $PHONED 2>/dev/null || true' EXIT
for _ in $(seq 1 30); do phonectl status >/dev/null 2>&1 && break; sleep 0.1; done
sleep 0.6

fails=0
live() { # name, directory, cargo target, extra cargo arguments
    local name=$1 dir=$2 target=$3
    shift 3
    echo "== $name"
    local log="/tmp/dolphone-ui-$name.log"
    (cd "$dir" && DOLPHONE_LIVE=1 CARGO_TARGET_DIR="$target" cargo test "$@" live_ -- --ignored --test-threads=1) \
        >"$log" 2>&1 || true
    grep -E "^test .* (ok|FAILED)$|panicked at|timed out" "$log" | sed 's/^/  /' || true
    # Passed only when a live test ran and none failed.
    if ! grep -qE "test result: ok\. [1-9]" "$log"; then
        fails=$((fails + 1))
        echo "  (log: $log)"
    fi
}
live dialer "$ROOT/apps/dialer" "$APPS_TARGET"
live messages "$ROOT/apps/messages" "$APPS_TARGET"
live settings "$ROOT/apps/settings" "$APPS_TARGET"
live desktop "$ROOT" "$TARGET" -p dolphone-desktop
echo
for d in "$ROOT"/apps/*/target/render/live; do
    [ -d "$d" ] && find "$d" -name '*.png' | sort | sed "s|$ROOT/|  |"
done
if [ "$fails" -eq 0 ]; then echo "all live UI tests passed"; else echo "$fails app(s) failed"; fi
exit "$fails"
