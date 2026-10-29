#!/usr/bin/env bash
# End-to-end: fake-mm on the system bus, phoned on a private session bus, and
# phonectl driving both, with assertions. Run inside the dev machine (see
# scripts/dev-setup.sh), from the repository root:
#
#     scripts/e2e.sh                 # uses target/debug
#     BIN=/path/to/bin scripts/e2e.sh
#
# The fake modem must be the only ModemManager on the system bus: the dev setup
# masks the real one and installs dist/dev/dolphone-fake-mm.conf.
set -euo pipefail

BIN=${BIN:-${CARGO_TARGET_DIR:-target}/debug}
P="$BIN/phonectl"
fails=0

pass() { printf '  ok    %s\n' "$1"; }
fail() { printf '  FAIL  %s\n        got: %s\n' "$1" "$2"; fails=$((fails + 1)); }
expect() { # name, haystack, needle
    if grep -qF -- "$3" <<<"$2"; then pass "$1"; else fail "$1" "$2"; fi
}
refute() {
    if grep -qF -- "$3" <<<"$2"; then fail "$1" "$2"; else pass "$1"; fi
}

if [ -z "${DOLPHONE_E2E_INNER:-}" ]; then
    # Another phoned (the dev install's, or one left by an interrupted run) watches
    # the same modem and would answer, end and delete the test's calls.
    if pgrep -x phoned >/dev/null; then
        echo "another phoned is running; stop it first (systemctl --user stop phoned; pkill -x phoned)" >&2
        exit 2
    fi
    # A fresh fake modem, so state from an earlier run cannot leak in.
    pkill -x fake-mm 2>/dev/null || true
    sleep 0.3
    "$BIN/fake-mm" >/tmp/dolphone-e2e-fake-mm.log 2>&1 &
    FAKE=$!
    trap 'kill $FAKE 2>/dev/null || true' EXIT
    sleep 0.8
    export XDG_DATA_HOME XDG_RUNTIME_DIR
    XDG_DATA_HOME=$(mktemp -d)
    XDG_RUNTIME_DIR=$(mktemp -d)
    DOLPHONE_E2E_INNER=1 BIN="$BIN" dbus-run-session -- "$0"
    exit $?
fi

start_phoned() {
    "$BIN/phoned" >>/tmp/dolphone-e2e-phoned.log 2>&1 &
    PHONED=$!
    for _ in $(seq 1 30); do
        "$P" status >/dev/null 2>&1 && return
        sleep 0.1
    done
    echo "phoned did not come up" >&2
    exit 1
}
: >/tmp/dolphone-e2e-phoned.log
# Phase 1: phoned trusts every caller, as if phonectl were Dolphone's own Dialer.
# (Only a debug build honours DOLPHONE_TRUST_ALL.) Phases 2 and 3 take it away.
DOLPHONE_TRUST_ALL=1 start_phoned
sleep 0.6 # first modem scan

echo "status"
out=$("$P" status)
expect "modem is found and registered" "$out" "Fakecom AU LTE 72% registered"
expect "own number in national format" "$out" "0491 570 110"

echo "incoming call, answered"
# Front ends live on phoned's signals: watch them for the whole call.
"$P" watch >/tmp/dolphone-e2e-watch.log 2>&1 &
WATCH=$!
sleep 0.3
"$P" fake ring +61491570156 >/dev/null
sleep 0.4
out=$("$P" calls)
expect "the call rings" "$out" "Incoming call"
"$P" answer
sleep 0.3
out=$("$P" calls)
expect "answering connects it" "$out" "Connected"
id=$(awk '{print $1; exit}' <<<"$out")
"$P" dtmf "$id" 42 && pass "DTMF on the live call" || fail "DTMF" "error"
"$P" fake hangup
sleep 0.4
out=$("$P" calls)
refute "the far end hanging up ends it" "$out" "Connected"
out=$("$P" recents 1)
expect "logged as answered" "$out" "in "
kill "$WATCH" 2>/dev/null || true
out=$(cat /tmp/dolphone-e2e-watch.log)
expect "signal: call added" "$out" "call+ "
expect "signal: call connected (promptly, not after a timeout)" "$out" "Connected"
expect "signal: call removed" "$out" "call- "

echo "incoming call, missed and declined"
"$P" fake ring 0491570157 >/dev/null
sleep 0.3
"$P" fake hangup
sleep 0.4
out=$("$P" recents 1)
expect "the caller giving up is a missed call" "$out" "missed"
out=$("$P" status)
expect "missed count" "$out" "missed 1"
"$P" fake ring 0491570157 >/dev/null
sleep 0.3
"$P" hangup
sleep 0.4
out=$("$P" recents 1)
expect "hanging up a ringing call declines it" "$out" "declined"

echo "call waiting"
"$P" fake ring 0491570161 >/dev/null
sleep 0.3
"$P" answer
sleep 0.2
"$P" fake ring 0491570162 >/dev/null
sleep 0.4
out=$("$P" calls)
expect "a second call while one is active waits" "$out" "Call waiting"
"$P" answer "$(awk '/Call waiting/ {print $1}' <<<"$out")"
sleep 0.4
out=$("$P" calls)
expect "answering it holds the first" "$out" "On hold"
"$P" fake hangup
sleep 0.3
"$P" fake hangup
sleep 0.4
out=$("$P" calls)
[ -z "$out" ] && pass "both calls end" || fail "both calls end" "$out"

echo "texts"
"$P" fake sms +61491570158 "Hey, running 10 min late" >/dev/null
sleep 0.5
out=$("$P" threads)
expect "an incoming text starts a thread" "$out" "Hey, running 10 min late"
expect "and is unread" "$out" "(1)"
out=$("$P" read 0491570158)
expect "the conversation has it" "$out" "< Hey, running 10 min late"
out=$("$P" status)
expect "reading clears unread" "$out" "unread 0"
"$P" fake sms Telstra "Your bill is ready" >/dev/null
sleep 0.5
out=$("$P" threads)
expect "an alphanumeric sender keeps its name" "$out" "Telstra"
"$P" fake sms +61491570158 $'evil‮txt.exe' >/dev/null
sleep 0.5
out=$("$P" thread +61491570158)
expect "bidi overrides are stripped" "$out" "eviltxt.exe"
out=$("$P" fake dump)
refute "received texts are deleted from the modem once stored" "$out" "sms "

echo "blocked numbers"
"$P" block 0491570160
"$P" fake ring +61491570160 >/dev/null
sleep 0.5
out=$("$P" calls)
[ -z "$out" ] && pass "a blocked caller never rings" || fail "a blocked caller never rings" "$out"
out=$("$P" recents 1)
expect "and is logged as blocked" "$out" "blocked"
"$P" fake sms 0491570160 "spam" >/dev/null
sleep 0.5
out=$("$P" threads)
refute "a blocked sender's text is dropped" "$out" "spam"

kill "$PHONED"
wait "$PHONED" 2>/dev/null || true

echo "as Dolphone's own app (trusted)"
DOLPHONE_TRUST_ALL=1 start_phoned
sleep 0.6
out=$("$P" dial 0491570163)
expect "our dialer's call goes straight out" "$out" "call "
sleep 0.9
out=$("$P" calls)
expect "it rings out" "$out" "Ringing"
sleep 2.3
out=$("$P" calls)
expect "the far end answers" "$out" "Connected"
"$P" hangup
sleep 0.4
out=$("$P" recents 1)
expect "logged as outgoing" "$out" "out "
out=$("$P" send 0491570158 "No worries, see you soon")
expect "our app's text goes straight out" "$out" "text "
sleep 0.6
out=$("$P" thread 0491570158)
expect "and is sent" "$out" "> No worries, see you soon"
refute "not stuck sending" "$out" "(sending)"
"$P" fake sendfails on
"$P" send 0491570158 "this one fails" >/dev/null
sleep 0.6
out=$("$P" thread 0491570158)
expect "a network failure is shown" "$out" "this one fails (failed)"
"$P" fake sendfails off
out=$("$P" dial 1900123456)
expect "premium numbers are held even for our app" "$out" "waiting for confirmation"
req=$(awk '{print $NF}' <<<"$out" | tr -d ')')
out=$("$P" confirm "$req")
expect "and go out once confirmed" "$out" "request "
sleep 0.2
out=$("$P" calls)
expect "the confirmed call is placed" "$out" "1900 123 456"
sleep 0.3
"$P" hangup || true
out=$("$P" ussd '*100#')
expect "USSD from our dialer" "$out" "Your balance is"
"$P" fake signal 30 5G
sleep 5.5
out=$("$P" status)
expect "signal changes reach the status" "$out" "5G 30%"
out=$("$P" wipe)
expect "wiping asks first" "$out" "waiting for confirmation"
"$P" confirm "$(awk '{print $NF}' <<<"$out" | tr -d ')')" >/dev/null
out=$("$P" threads)
[ -z "$out" ] && pass "wipe clears messages" || fail "wipe clears messages" "$out"

kill "$PHONED"
wait "$PHONED" 2>/dev/null || true

echo "phase 2: a script, not trusted, no grants"
start_phoned
sleep 0.6
echo "outside requests wait for the device"
out=$("$P" send 0491570158 No worries)
expect "a text from a script is held" "$out" "waiting for confirmation"
out=$("$P" dial 0491570159)
expect "a call from a script is held" "$out" "waiting for confirmation"
out=$("$P" pending)
expect "both are pending" "$out" "call 0491 570 159 by phonectl (external)"
out=$("$P" confirm 1 2>&1 || true)
expect "a script cannot confirm its own request" "$out" "only Dolphone's own apps can confirm"
"$P" deny 1
"$P" deny 2
out=$("$P" pending)
[ -z "$out" ] && pass "denied requests are gone" || fail "denied requests are gone" "$out"
out=$("$P" ussd '*100#' 2>&1 || true)
expect "USSD is refused from a script" "$out" "only run from Dolphone's dialer"
out=$("$P" dial 000)
expect "an emergency call from a script is held, not placed" "$out" "waiting for confirmation"

out=$("$P" threads 2>&1 || true)
expect "a script cannot read messages" "$out" "may not read messages"
out=$("$P" recents 2>&1 || true)
expect "or the call log" "$out" "may not read messages"
out=$("$P" block 0491570170 2>&1 || true)
expect "a script cannot change the block list" "$out" "only Dolphone's own apps"
"$P" fake ring 0491570171 >/dev/null
sleep 0.4
out=$("$P" answer 2>&1 || true)
expect "a script cannot answer (no room-bugging)" "$out" "only Dolphone's own apps can answer"
"$P" hangup && pass "but anyone may hang up" || fail "anyone may hang up" "error"
sleep 0.3
"$P" watch >/tmp/dolphone-e2e-watch2.log 2>&1 &
WATCH=$!
sleep 0.3
"$P" fake sms 0491570172 "Your code is 482913" >/dev/null
sleep 0.6
kill "$WATCH" 2>/dev/null || true
out=$(cat /tmp/dolphone-e2e-watch2.log)
expect "the new-message signal still arrives" "$out" "text< "
refute "but without the text (one-time codes stay private)" "$out" "482913"
kill "$PHONED"
wait "$PHONED" 2>/dev/null || true

echo "phase 3: a script the owner granted reading"
DOLPHONE_READ_GRANTS=phonectl start_phoned
sleep 0.6
out=$("$P" thread 0491570172)
expect "a granted script can read" "$out" "Your code is 482913"
out=$("$P" block 0491570173 2>&1 || true)
expect "but still cannot change settings" "$out" "only Dolphone's own apps"
kill "$PHONED"
echo
if [ "$fails" -eq 0 ]; then echo "all passed"; else echo "$fails failed (log: /tmp/dolphone-e2e-phoned.log)"; exit 1; fi
