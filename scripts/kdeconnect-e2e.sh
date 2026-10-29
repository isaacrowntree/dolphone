#!/usr/bin/env bash
# Dolphone against the real KDE Connect daemon (kdeconnectd), on one machine:
# discovery, TLS and protocol 8, pairing confirmed on the phone, a text sent from
# the laptop, the conversation list read by the laptop, and a call ringing on it.
# Skipped where KDE Connect is not installed (apt install kdeconnect).
#
#     scripts/kdeconnect-e2e.sh
set -uo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
BIN=${BIN:-${CARGO_TARGET_DIR:-$ROOT/target}/debug}
command -v kdeconnectd >/dev/null || { echo "  skip  KDE Connect is not installed"; exit 0; }
fails=0
pass() { printf '  ok    %s\n' "$1"; }
fail() { printf '  FAIL  %s\n        got: %s\n' "$1" "$2"; fails=$((fails + 1)); }
expect() { if grep -qF -- "$3" <<<"$2"; then pass "$1"; else fail "$1" "$2"; fi; }

if [ -z "${DOLPHONE_KDC_INNER:-}" ]; then
    pgrep -x phoned >/dev/null && { echo "another phoned is running" >&2; exit 2; }
    pkill -x fake-mm 2>/dev/null; pkill -x kdeconnectd 2>/dev/null; sleep 0.3
    "$BIN/fake-mm" >/tmp/dolphone-kdc-fake-mm.log 2>&1 &
    FAKE=$!
    trap 'kill $FAKE 2>/dev/null || true' EXIT
    sleep 0.8
    export XDG_DATA_HOME XDG_CONFIG_HOME XDG_RUNTIME_DIR
    XDG_DATA_HOME=$(mktemp -d); XDG_CONFIG_HOME=$(mktemp -d); XDG_RUNTIME_DIR=$(mktemp -d)
    chmod 700 "$XDG_RUNTIME_DIR"
    DOLPHONE_KDC_INNER=1 BIN="$BIN" dbus-run-session -- "$0"
    exit $?
fi

export PATH="$BIN:$PATH"
# Headless, and for any kdeconnectd D-Bus activation starts as well.
export QT_QPA_PLATFORM=offscreen QT_LOGGING_RULES="kdeconnect.*.debug=true"
DOLPHONE_TRUST_ALL=1 phoned >/tmp/dolphone-kdc-phoned.log 2>&1 &
PHONED=$!
sleep 1
phonectl set kdeconnect_name "Test Phone"
phonectl kdeconnect on
kdeconnectd >/tmp/dolphone-kdc-kde.log 2>&1 &
KDE=$!
for _ in $(seq 1 30); do
    busctl --user status org.kde.kdeconnect >/dev/null 2>&1 && break
    sleep 0.2
done
trap 'kill $KDE $PHONED 2>/dev/null; wait $PHONED 2>/dev/null' EXIT

echo "discovery and TLS"
ID=
for _ in $(seq 1 30); do
    ID=$(kdeconnect-cli -l --id-only 2>/dev/null | head -1)
    [ -n "$ID" ] && break
    sleep 0.5
done
[ -n "$ID" ] && pass "KDE Connect finds the phone and connects" || fail "found by KDE Connect" "$(kdeconnect-cli -l 2>&1)"
expect "under the name it was given" "$(kdeconnect-cli -l 2>&1)" "Test Phone"
expect "and the phone sees the laptop, unpaired" "$(phonectl kdeconnect)" "unpaired"

echo "pairing"
kdeconnect-cli --pair -d "$ID" >/dev/null 2>&1
sleep 1.5
out=$(phonectl pending)
expect "a pair request waits for the device" "$out" "kdeconnect"
phonectl confirm "$(awk '{print $1; exit}' <<<"$out")" >/dev/null
sleep 1.5
expect "allowed there, the laptop is paired" "$(kdeconnect-cli -l 2>&1)" "paired and reachable"
# The second column is "paired" or "unpaired": compare it whole.
state=$(phonectl kdeconnect | awk 'NR==1 {print $2}')
[ "$state" = paired ] && pass "on both sides" || fail "on both sides" "$(phonectl kdeconnect)"

echo "texts"
kdeconnect-cli --send-sms "Hello from the laptop" --destination 0491570196 -d "$ID" >/dev/null 2>&1
sleep 1.5
expect "a text written on the laptop is sent by the phone" "$(phonectl thread 0491570196)" "> Hello from the laptop"
phonectl fake sms 0491570197 "Text for the laptop" >/dev/null
sleep 1
DEV=/modules/kdeconnect/devices/$ID
busctl --user call org.kde.kdeconnect "$DEV" org.kde.kdeconnect.device.conversations requestAllConversationThreads >/dev/null 2>&1
sleep 2
out=$(busctl --user call org.kde.kdeconnect "$DEV" org.kde.kdeconnect.device.conversations activeConversations 2>&1)
expect "the laptop reads the conversation list" "$out" "Text for the laptop"

echo "calls"
phonectl fake ring 0491570198 >/dev/null
sleep 1.5
expect "a ringing call reaches the laptop's telephony plugin" "$(cat /tmp/dolphone-kdc-kde.log)" 'notification with type: "callReceived"'
phonectl fake hangup >/dev/null

echo "unpairing"
phonectl kdeconnect unpair "$(phonectl kdeconnect | awk '{print $1; exit}')"
sleep 1
refute_paired=$(kdeconnect-cli -l 2>&1 | grep -c "paired and")
[ "$refute_paired" = 0 ] && pass "unpairing from the phone unpairs the laptop" || fail "unpaired" "$(kdeconnect-cli -l 2>&1)"

echo
if [ "$fails" -eq 0 ]; then echo "all passed"; else echo "$fails failed"; exit 1; fi
