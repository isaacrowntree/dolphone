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
# Run a command until its output has the needle, for up to 10 s, and print its
# last output: a check waits for the state it expects instead of guessing how long
# phoned takes, which on a busy machine is longer.
until_has() {
    local needle=$1 o=
    shift
    for _ in $(seq 1 100); do
        o=$("$@" 2>&1) || true
        grep -qF -- "$needle" <<<"$o" && break
        sleep 0.1
    done
    printf '%s\n' "$o"
}
# A SIP command as a person would give it: baresip 1.1.0's ctrl_dbus can crash
# mid-command (phoned says so, and starts it again), and then you try once more.
sip_try() {
    local o
    if o=$("$P" "$@" 2>&1); then printf '%s\n' "$o"; return 0; fi
    if grep -q "try once more" <<<"$o"; then
        echo "  (baresip stopped during: $*; once more)" >&2
        sleep 2
        "$P" "$@"
    else
        printf '%s\n' "$o" >&2
        return 1
    fi
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
    if ! kill -0 "$FAKE" 2>/dev/null; then
        echo "fake-mm did not start (is the real ModemManager running, or the bus policy missing?):" >&2
        cat /tmp/dolphone-e2e-fake-mm.log >&2
        exit 2
    fi
    # The far end for SIP calls: a second baresip on loopback, driven by
    # phonectl fake sip over its TCP control. Skipped when baresip is missing.
    SIPFAR_PID=
    if command -v baresip >/dev/null; then
        SIPFAR=$(mktemp -d)
        cat >"$SIPFAR/config" <<CONF
module_path /usr/lib/baresip/modules
sip_listen 127.0.0.1:5073
module opus.so
module g711.so
module ausine.so
module alsa.so
module_app account.so
module_app contact.so
module_app menu.so
module_app ctrl_tcp.so
ctrl_tcp_listen 127.0.0.1:4444
audio_player alsa,null
audio_source ausine,440
CONF
        # Plain RTP: baresip 1.1.0 crashes on a hold re-INVITE with SRTP restarting.
        # phoned's own baresip offers SRTP, which is optional, so the call falls
        # back to RTP here and the hold path is exercised.
        echo "<sip:far@127.0.0.1:5073>;regint=0;catchall=yes;answermode=manual" >"$SIPFAR/accounts"
        # The far end's one contact, so its "message" command texts phoned.
        echo '"Dolphone" <sip:me@127.0.0.1:5072>' >"$SIPFAR/contacts"
        baresip -f "$SIPFAR" >/tmp/dolphone-e2e-sip.log 2>&1 &
        SIPFAR_PID=$!
        export DOLPHONE_E2E_SIP=1
    fi
    trap 'kill $FAKE ${SIPFAR_PID:-} 2>/dev/null || true' EXIT
    # Where the session's PipeWire is, before the run hides it.
    export DOLPHONE_PW_RUNTIME=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}
    export XDG_DATA_HOME XDG_RUNTIME_DIR XDG_CONFIG_HOME
    XDG_DATA_HOME=$(mktemp -d)
    XDG_CONFIG_HOME=$(mktemp -d)
    XDG_RUNTIME_DIR=$(mktemp -d)
    DOLPHONE_E2E_INNER=1 BIN="$BIN" dbus-run-session -- "$0"
    exit $?
fi

# A run that stops early (set -e) must not leave its phoned for the next one.
trap 'kill "${PHONED:-}" "${MMSD:-}" 2>/dev/null || true' EXIT

start_phoned() {
    "$BIN/phoned" >>/tmp/dolphone-e2e-phoned.log 2>&1 &
    PHONED=$!
    for _ in $(seq 1 100); do
        "$P" status >/dev/null 2>&1 && break
        sleep 0.1
    done
    "$P" status >/dev/null 2>&1 || { echo "phoned did not come up" >&2; exit 1; }
    # And until it has found the modem: a fixed pause was too short on a busy CI
    # runner, and the first ring went unseen.
    for _ in $(seq 1 100); do
        "$P" status 2>/dev/null | grep -q "no modem" || break
        sleep 0.1
    done
}
# phoned's own baresip, for the SIP checks: on loopback, with a tone for a
# microphone (debug builds only), since the test has no sound server.
export DOLPHONE_SIP_LISTEN=127.0.0.1:5072 DOLPHONE_SIP_TEST_AUDIO=1
# Where fake-mmsd keeps messages, as mmsd-tng keeps them in ~/.mms/modemmanager.
DOLPHONE_MMS_DIR=$(mktemp -d)
export DOLPHONE_MMS_DIR
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
out=$(until_has "Incoming call" "$P" calls)
expect "the call rings" "$out" "Incoming call"
"$P" answer
out=$(until_has "Connected" "$P" calls)
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
out=$(until_has "missed" "$P" recents 1)
expect "the caller giving up is a missed call" "$out" "missed"
out=$("$P" status)
expect "missed count" "$out" "missed 1"
"$P" fake ring 0491570157 >/dev/null
sleep 0.3
"$P" hangup
out=$(until_has "declined" "$P" recents 1)
expect "hanging up a ringing call declines it" "$out" "declined"

echo "call waiting"
"$P" fake ring 0491570161 >/dev/null
sleep 0.3
"$P" answer
sleep 0.2
"$P" fake ring 0491570162 >/dev/null
out=$(until_has "Call waiting" "$P" calls)
expect "a second call while one is active waits" "$out" "Call waiting"
"$P" answer "$(awk '/Call waiting/ {print $1}' <<<"$out")"
out=$(until_has "On hold" "$P" calls)
expect "answering it holds the first" "$out" "On hold"
"$P" merge
sleep 0.4
out=$("$P" calls)
refute "merging joins them into one conference" "$out" "On hold"
[ "$(grep -c Connected <<<"$out")" = 2 ] && pass "with both parties connected" || fail "both connected" "$out"
"$P" fake hangup
sleep 0.3
"$P" fake hangup
sleep 0.4
out=$("$P" calls)
[ -z "$out" ] && pass "both calls end" || fail "both calls end" "$out"

echo "call waiting off, and silencing unknown callers"
"$P" set call_waiting false
out=$(until_has "call_waiting=false" "$P" fake dump)
expect "turning call waiting off reaches the modem" "$out" "call_waiting=false"
"$P" fake ring 0491570161 >/dev/null
sleep 0.3
"$P" answer
sleep 0.2
"$P" fake ring 0491570162 >/dev/null
sleep 0.4
out=$("$P" calls)
refute "with it off, a second caller is turned away" "$out" "Call waiting"
"$P" fake hangup
sleep 0.4
"$P" set call_waiting true
"$P" set silence_unknown true
"$P" fake ring 0491570199 >/dev/null
sleep 0.4
out=$(grep "incoming call" /tmp/dolphone-e2e-phoned.log | tail -1)
expect "an unknown caller rings silently" "$out" "silence=true"
"$P" fake hangup
out=$(until_has "silenced" "$P" recents 1)
expect "and is logged as silenced, to call back" "$out" "silenced"
"$P" set silence_unknown false

echo "emergency alerts"
out=$("$P" fake dump)
expect "phoned listens on the alert channels" "$out" "channels=[(4370, 4400)"
"$P" watch >/tmp/dolphone-e2e-watch3.log 2>&1 &
WATCH=$!
sleep 0.3
"$P" fake broadcast 4370 "Emergency Alert: test of the national system" >/dev/null
sleep 0.5
"$P" fake broadcast 4370 "Emergency Alert: test of the national system" >/dev/null
sleep 0.5
"$P" fake broadcast 4380 "Exercise, exercise" >/dev/null
sleep 0.5
kill "$WATCH" 2>/dev/null || true
out=$("$P" alerts)
expect "a national alert is stored" "$out" "National alert (channel 4370): Emergency Alert: test"
[ "$(grep -c 'channel 4370' <<<"$out")" = 1 ] && pass "a rebroadcast is shown once" || fail "rebroadcast once" "$out"
refute "test alerts are off unless asked for" "$out" "Exercise"
expect "alerts reach the front ends" "$(cat /tmp/dolphone-e2e-watch3.log)" "alert! National alert"
out=$("$P" fake dump)
refute "alerts are cleared from the modem" "$out" "cbm "
out=$("$P" --json alerts)
expect "as JSON" "$out" '"channel":4370'

echo "SIM PIN"
"$P" fake lock on
out=$(until_has "SIM locked" "$P" status)
expect "a locked SIM is reported" "$out" "SIM locked"
out=$("$P" dial 000)
expect "an emergency call goes out with the SIM locked" "$out" "call "
"$P" hangup >/dev/null 2>&1 || true
sleep 0.5
out=$("$P" unlock 1111 2>&1 || true)
expect "a wrong PIN says how many tries are left" "$out" "2 tries left"
out=$("$P" unlock 1234)
expect "the right one unlocks" "$out" "SIM unlocked"
sleep 1.2
out=$("$P" status)
refute "and the phone is ready again" "$out" "SIM locked"
expect "registered" "$out" "registered"

echo "texts"
"$P" fake sms +61491570158 "Hey, running 10 min late" >/dev/null
out=$(until_has "Hey, running 10 min late" "$P" threads)
expect "an incoming text starts a thread" "$out" "Hey, running 10 min late"
expect "and is unread" "$out" "(1)"
out=$("$P" read 0491570158)
expect "the conversation has it" "$out" "< Hey, running 10 min late"
out=$("$P" status)
expect "reading clears unread" "$out" "unread 0"
"$P" fake sms Telstra "Your bill is ready" >/dev/null
out=$(until_has "Telstra" "$P" threads)
expect "an alphanumeric sender keeps its name" "$out" "Telstra"
"$P" fake sms +61491570158 $'evil‮txt.exe' >/dev/null
out=$(until_has "eviltxt.exe" "$P" thread +61491570158)
expect "bidi overrides are stripped" "$out" "eviltxt.exe"
out=$("$P" fake dump)
refute "received texts are deleted from the modem once stored" "$out" "sms "

echo "Australia's Emergency Alert texts"
"$P" block 0444444444 >/dev/null
"$P" fake sms 0444444444 "Emergency Alert: test message" >/dev/null
out=$(until_has "Emergency Alert: test" "$P" threads)
expect "a text from 0444 444 444 is never blocked" "$out" "Emergency Alert: test"
expect "and is sounded as an alert" "$(cat /tmp/dolphone-e2e-phoned.log)" "an Emergency Alert text"
"$P" unblock 0444444444 >/dev/null

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
out=$(until_has "call " "$P" dial 0491570163)
expect "our dialer's call goes straight out" "$out" "call "
out=$(until_has "Ringing" "$P" calls)
expect "it rings out" "$out" "Ringing"
out=$(until_has "Connected" "$P" calls)
expect "the far end answers" "$out" "Connected"
"$P" hangup
out=$(until_has "out " "$P" recents 1)
expect "logged as outgoing" "$out" "out "
out=$("$P" send 0491570158 "No worries, see you soon")
expect "our app's text goes straight out" "$out" "text "
sleep 0.6
out=$(until_has "> No worries, see you soon" "$P" thread 0491570158)
expect "and is sent" "$out" "> No worries, see you soon"
refute "not stuck sending" "$out" "(sending)"
"$P" fake sendfails on
failed=$("$P" send 0491570158 "this one fails" | awk '{print $2}')
out=$(until_has "this one fails (failed)" "$P" thread 0491570158)
expect "a network failure is shown" "$out" "this one fails (failed)"
"$P" fake sendfails off
"$P" retry "$failed"
sleep 0.6
out=$("$P" thread 0491570158)
refute "resending a failed text sends it" "$out" "(failed)"
out=$("$P" send --wait 10 0491570158 "On my way")
expect "send --wait waits for the modem" "$out" "sent "
"$P" set delivery_reports true
"$P" send 0491570158 "Delivered?" >/dev/null
out=$(until_has "Delivered? (delivered)" "$P" thread 0491570158)
expect "a delivery report marks it delivered" "$out" "Delivered? (delivered)"
out=$("$P" fake dump)
refute "and the modem's copy is then deleted" "$out" "sms "
"$P" set delivery_reports false

echo "busy, and call-progress tones"
"$P" dial 0491570164 >/dev/null
sleep 0.3
"$P" fake busy
sleep 0.5
out=$(grep "call-progress tone" /tmp/dolphone-e2e-phoned.log | tail -1)
expect "a busy line plays the country's busy tone" "$out" 'tone="busy" region="AU"'
"$P" set local_ringback true
"$P" dial 0491570165 >/dev/null
sleep 1
out=$(grep "call-progress tone" /tmp/dolphone-e2e-phoned.log | tail -1)
expect "ringback can be played locally" "$out" 'tone="ringback"'
"$P" hangup || true
"$P" set local_ringback false
sleep 0.3

echo "rules on incoming texts"
mkdir -p "$XDG_CONFIG_HOME/dolphone"
cat >"$XDG_CONFIG_HOME/status.sh" <<'SH'
#!/bin/sh
cat >/dev/null
echo "uplink ok"
SH
chmod +x "$XDG_CONFIG_HOME/status.sh"
printf '[{"from": "0491570166", "contains": "status", "secret": "4821", "run": ["%s"], "reply": true}]' \
    "$XDG_CONFIG_HOME/status.sh" >"$XDG_CONFIG_HOME/dolphone/rules.json"
"$P" fake sms +61491570166 "STATUS 4821" >/dev/null
out=$(until_has "> uplink ok" "$P" thread 0491570166)
expect "a matching text runs the rule, which replies" "$out" "> uplink ok"
"$P" fake sms +61491570166 "STATUS" >/dev/null
sleep 1
out=$("$P" thread 0491570166)
[ "$(grep -c 'uplink ok' <<<"$out")" = 1 ] && pass "without the secret, nothing runs" || fail "secret needed" "$out"
rm "$XDG_CONFIG_HOME/dolphone/rules.json"

echo "uplink watch"
"$P" set uplink_alert_to 0491570194
"$P" set uplink_interface eth0
"$P" set uplink_grace_s 2
"$P" fake uplink off
out=$(until_has "eth0 is down" "$P" thread 0491570194)
expect "an outage longer than the grace is texted" "$out" "eth0 is down"
"$P" fake uplink on
out=$(until_has "eth0 is back up" "$P" thread 0491570194)
expect "and so is the link coming back" "$out" "eth0 is back up"
"$P" set uplink_alert_to ""

echo "2G"
"$P" fake signal 40 2G
sleep 1
expect "a fall back to 2G is warned of" "$(tail -20 /tmp/dolphone-e2e-phoned.log)" "fell back to 2G"
out=$("$P" cells)
expect "and is in the network history" "$out" "2G   Fakecom AU"
"$P" set disable_2g true
sleep 0.5
expect "Disable 2G leaves 2G out of the modem's modes" "$("$P" fake dump)" "modes=(12, 8)"
"$P" set disable_2g false
sleep 0.5
expect "and puts it back" "$("$P" fake dump)" "modes=(14, 8)"
"$P" fake signal 72 LTE
sleep 1

echo "search, export, and which SIM"
out=$("$P" search running late)
expect "search finds a text by its words" "$out" "Hey, running 10 min late"
out=$("$P" search 'late"; DROP TABLE messages; --' 2>&1) && pass "a query with SQL in it is still just a search" \
    || fail "a query with SQL in it is still just a search" "$out"
refute "and its SQL is not run as SQL" "$out" "no such table"
expect "and the messages are all still there" "$("$P" search running late)" "Hey, running 10 min late"
out=$("$P" export)
expect "export writes an SMS Backup & Restore file" "$out" "<smses count="
expect "with each text" "$out" 'body="Hey, running 10 min late"'
out=$("$P" thread 0491570158)
expect "texts note the SIM they came through" "$out" "[SIM …"

echo "contacts"
out=$("$P" contacts add "Alex Example" 0491570156)
expect "our app adds a contact straight away" "$out" "contact "
cat >"$XDG_CONFIG_HOME/export.vcf" <<'VCF'
BEGIN:VCARD
VERSION:3.0
FN:Alex Example
TEL;TYPE=WORK:+61 2 9374 4000
EMAIL:alex@example.com
END:VCARD
BEGIN:VCARD
VERSION:3.0
FN:Sam Sample
TEL;TYPE=CELL:0491 570 157
PHOTO;ENCODING=b:aGVsbG8=
END:VCARD
VCF
out=$("$P" contacts import "$XDG_CONFIG_HOME/export.vcf")
expect "an export imports" "$out" "2 contacts added or updated"
out=$("$P" contacts)
expect "a match merges instead of doubling" "$out" "mobile 0491 570 156, work (02) 9374 4000"
expect "and a new card is added" "$out" "Sam Sample"
refute "only names and numbers are kept" "$(cat "$XDG_DATA_HOME"/dolphone/contacts/*.vcf)" "EMAIL"
"$P" contacts import "$XDG_CONFIG_HOME/export.vcf" >/dev/null
[ "$("$P" contacts | grep -c 'Alex Example')" = 1 ] && pass "importing twice changes nothing" || fail "import twice" "$("$P" contacts)"
id=$("$P" contacts | awk '/Sam Sample/ {print $1}')
"$P" contacts remove "$id" >/dev/null
refute "a contact can be removed" "$("$P" contacts)" "Sam Sample"
out=$("$P" --json contacts)
expect "contacts as JSON" "$out" '"name":"Alex Example"'
out=$("$P" contacts import-phone AA:BB 2>&1 || true)
expect "a bad command is explained" "$out" "contacts [list]"

echo "calls over SIP"
if [ -n "${DOLPHONE_E2E_SIP:-}" ]; then
    "$P" set sip_password "not-a-real-password"
    "$P" set sip_account sip:me@127.0.0.1:5072
    # Calls go out through the far end, as through a provider's proxy.
    "$P" set sip_proxy sip:127.0.0.1:5073
    "$P" set sip_enabled true
    out=$(until_has "SIP registered" "$P" status)
    expect "phoned starts baresip with the account" "$out" "SIP registered"
    out=$("$P" get sip_password)
    [ "$out" = "set" ] && pass "the password is never read back" || fail "password hidden" "$out"
    out=$(stat -c %a "$XDG_CONFIG_HOME/dolphone/sip/accounts")
    [ "$out" = 600 ] && pass "and its file is the owner's alone" || fail "accounts file mode" "$out"
    out=$("$P" set sip_account 'sip:x@example.com>;outbound=evil' 2>&1 || true)
    expect "an account cannot smuggle in options" "$out" "a SIP account looks like"
    "$P" fake sip ring sip:me@127.0.0.1:5072 >/dev/null
    out=$(until_has "Incoming call" "$P" calls)
    expect "a SIP call rings like any other" "$out" "Incoming call"
    expect "showing the caller's address" "$out" "sip:far@127.0.0.1:5073"
    "$P" answer
    out=$(until_has "Connected" "$P" calls)
    expect "answering connects it" "$out" "Connected"
    "$P" dtmf "$(awk '{print $1; exit}' <<<"$out")" 5 && pass "keypad tones over SIP" || fail "SIP DTMF" "error"
    "$P" fake sip hangup
    sleep 1
    out=$("$P" calls)
    [ -z "$out" ] && pass "the far end hanging up ends it" || fail "SIP far end hangs up" "$out"
    out=$("$P" recents 1)
    expect "and it is in the log" "$out" "in "
    "$P" set sip_use always
    "$P" fake sip autoanswer on
    out=$(sip_try dial 0491570190)
    expect "with SIP set to always, a call goes over SIP" "$out" "call "
    out=$(until_has "Connected" "$P" calls)
    expect "and connects" "$out" "Connected"
    "$P" hangup
    out=$(until_has "out " "$P" recents 1)
    expect "logged as outgoing" "$out" "out "
    "$P" dial 000 >/dev/null
    sleep 0.5
    refute "an emergency call never goes over SIP" "$(tail -3 /tmp/dolphone-e2e-phoned.log)" "dialling over SIP"
    "$P" hangup >/dev/null 2>&1 || true
    sleep 0.5
    "$P" fake sip autoanswer off
    sip_try dial 0491570191 >/dev/null
    sleep 1
    "$P" fake sip hangup
    sleep 1
    out=$(grep "call-progress tone" /tmp/dolphone-e2e-phoned.log | tail -1)
    expect "a SIP call refused busy plays the busy tone" "$out" 'tone="busy"'
    "$P" fake sip message "Hello over SIP" >/dev/null
    sleep 1
    expect "a SIP text arrives as a text" "$("$P" threads)" "Hello over SIP"
    out=$("$P" send sip:far@127.0.0.1:5073 "hi" 2>&1 || true)
    expect "texting a SIP address says why it cannot" "$out" "baresip 1.1.0 cannot message"
    echo "  (holding across SIP and mobile)"
    "$P" set sip_use fallback
    "$P" fake sip ring sip:me@127.0.0.1:5072 >/dev/null
    sleep 1
    "$P" answer
    sleep 1
    "$P" fake ring 0491570192 >/dev/null
    out=$(until_has "Call waiting" "$P" calls)
    expect "a mobile call during a SIP call waits" "$out" "Call waiting"
    "$P" answer "$(awk '/Call waiting/ {print $1}' <<<"$out")"
    sleep 1
    out=$("$P" calls)
    expect "answering it holds the SIP call" "$(grep 'sip:far' <<<"$out")" "On hold"
    expect "and connects the mobile one" "$(grep '0491 570 192' <<<"$out")" "Connected"
    "$P" swap
    sleep 1
    out=$("$P" calls)
    expect "swap brings SIP back" "$(grep 'sip:far' <<<"$out")" "Connected"
    expect "and holds the mobile call" "$(grep '0491 570 192' <<<"$out")" "On hold"
    "$P" fake hangup
    "$P" fake sip hangup
    sleep 1
    out=$("$P" calls)
    [ -z "$out" ] && pass "both end" || fail "both end" "$out"
    "$P" fake ring 0491570193 >/dev/null
    sleep 0.4
    "$P" answer
    sleep 0.5
    "$P" fake sip ring sip:me@127.0.0.1:5072 >/dev/null
    sleep 1
    out=$("$P" calls)
    expect "a SIP call during a mobile call waits" "$(grep 'sip:far' <<<"$out")" "Call waiting"
    "$P" answer "$(awk '/sip:far/ {print $1}' <<<"$out")"
    sleep 1.5
    out=$("$P" calls)
    expect "answering it holds the mobile call" "$(grep '0491 570 193' <<<"$out")" "On hold"
    expect "and connects SIP" "$(grep 'sip:far' <<<"$out")" "Connected"
    "$P" fake sip hangup
    sleep 0.6
    "$P" fake hangup
    sleep 0.6
    "$P" set sip_enabled false
    sleep 1
    refute "turning SIP off stops baresip" "$("$P" status)" "SIP "
else
    echo "  skip  baresip is not installed"
fi

echo "device profiles"
out=$("$P" device)
expect "a profile is chosen, and says why" "$out" "why "
out=$("$P" device list)
expect "the Flipper One profile is built in" "$out" "flipper-one"
expect "and the others" "$out" "pinephone"
"$P" device use flipper-one
expect "one can be chosen by hand" "$("$P" device)" "chosen in Phone Settings"
out=$("$P" device use nonsense 2>&1 || true)
expect "an unknown one is refused" "$out" "no device profile nonsense"
"$P" device use auto
refute "and detection comes back" "$("$P" device)" "chosen in Phone Settings"

echo "JSON"
out=$("$P" --json status)
expect "status as JSON" "$out" '"present":true'
"$P" --json watch >/tmp/dolphone-e2e-watch4.log 2>&1 &
WATCH=$!
sleep 0.3
"$P" fake sms 0491570167 "json" >/dev/null
sleep 0.5
kill "$WATCH" 2>/dev/null || true
expect "watch --json is one object per event" "$(cat /tmp/dolphone-e2e-watch4.log)" '{"event":"message_added"'

out=$("$P" dial 1900123456)
expect "premium numbers are held even for our app" "$out" "waiting for confirmation"
req=$(awk '{print $NF}' <<<"$out" | tr -d ')')
out=$("$P" confirm "$req")
expect "and go out once confirmed" "$out" "request "
out=$(until_has "1900 123 456" "$P" calls)
expect "the confirmed call is placed" "$out" "1900 123 456"
sleep 0.3
"$P" hangup || true
out=$("$P" ussd '*100#')
expect "USSD from our dialer" "$out" "Your balance is"

echo "MMS: pictures and group texts"
"$P" fake sms 0491570164 "" >/dev/null
out=$(until_has "install mmsd-tng" "$P" threads)
expect "without mmsd-tng, an MMS notice says what is missing" "$out" "install mmsd-tng"
out=$("$P" mms 0491570162 Hello 2>&1 || true)
expect "and sending one says what it needs" "$out" "needs mmsd-tng"
"$BIN/fake-mmsd" >/tmp/dolphone-e2e-mmsd.log 2>&1 &
MMSD=$!
sleep 0.6
printf '\xff\xd8\xff\xe0 a small jpeg' >"$DOLPHONE_MMS_DIR/cat.jpg"
"$P" fake mms +61491570162 --file "$DOLPHONE_MMS_DIR/cat.jpg" "Look at this" >/dev/null
out=$(until_has "Look at this" "$P" threads)
expect "a picture message arrives as a text" "$out" "Look at this"
out=$("$P" read 0491570162)
expect "with its picture" "$out" "< Look at this [Picture: "
pic=$(grep -o '/[^]]*\.jpg' <<<"$out" | head -1)
cmp -s "$pic" "$DOLPHONE_MMS_DIR/cat.jpg" && pass "the picture is kept whole, beside the store" || fail "picture kept" "$pic"
out=$("$P" fake mms dump)
expect "and mmsd-tng's copy is deleted once stored" "$out" "held 0"
"$P" fake mms +61491570163 --to +61491570162 "Dinner at 7?" >/dev/null
out=$(until_has "0491 570 162, 0491 570 163" "$P" threads)
expect "a group text is one thread with everyone on it" "$out" "0491 570 162, 0491 570 163"
"$P" send 0491570163,0491570162 "Yes!" >/dev/null
out=$(until_has 'sent to=+61491570162,+61491570163 text="Yes!"' "$P" fake mms dump)
expect "a reply to the group goes to everyone, as MMS" "$out" 'sent to=+61491570162,+61491570163 text="Yes!"'
out=$("$P" thread 0491570162,0491570163)
expect "and is in the group's thread, sent" "$out" "> Yes!"
refute "not still sending" "$out" "(sending)"
"$P" mms 0491570162 --file "$DOLPHONE_MMS_DIR/cat.jpg" "Mine" >/dev/null
out=$(until_has 'text="Mine" image/jpeg=17b' "$P" fake mms dump)
expect "a picture goes out as a part of its own" "$out" 'text="Mine" image/jpeg=17b'
"$P" fake mms fails on
"$P" mms 0491570162 "Will fail" >/dev/null
out=$(until_has "> Will fail (failed)" "$P" thread 0491570162)
expect "a failed MMS shows as failed" "$out" "> Will fail (failed)"
"$P" fake mms fails off
"$P" fake sms 0491570165 "" >/dev/null
sleep 0.4
out=$("$P" threads)
refute "with mmsd-tng running, phoned leaves the MMS notice to it" "$out" "0491 570 165"

echo "service codes and the modem's setup"
out=$("$P" ussd '*#06#')
expect "*#06# shows the IMEI, on the device" "$out" "IMEI: 350000000000001"
out=$("$P" ussd '**04*1234*4321*4321#')
expect "**04*old*new*new# changes the SIM PIN through ModemManager, not as USSD" "$out" "SIM PIN changed"
"$P" ussd '**04*4321*1234*1234#' >/dev/null
out=$("$P" ussd '**04*1234*4321*9999#' 2>&1 || true)
expect "and two different new PINs are refused before anything is sent" "$out" "not the same"
"$P" ussd '*31#' >/dev/null
out=$("$P" get clir)
expect "hiding caller ID is remembered, to put back after a modem restart" "$out" "1"
"$P" ussd '#31#' >/dev/null
out=$("$P" ussd '**21*0491570199#')
expect "**21*n# forwards calls, through +CCFC" "$out" "Forwarding all calls to 0491570199"
out=$("$P" ussd '*#21#')
expect "*#21# asks the network" "$out" "Forwarding all calls: on, to 0491570199"
out=$("$P" ussd '##002#')
expect "##002# clears every forward" "$out" "cleared"
out=$("$P" fake dump)
expect "and the modem has none left" "$out" "forward=[]"
out=$("$P" ussd '#31#')
expect "#31# shows the caller ID" "$out" "Caller ID: shown"
out=$("$P" ussd '*#43#')
expect "*#43# asks about call waiting, through ModemManager" "$out" "Call waiting: on"
out=$("$P" ussd '*#33#' 2>&1 || true)
expect "barring codes are refused, not sent as USSD" "$out" "call barring codes are not supported"
out=$("$P" modem setup)
expect "the setup names the modem" "$out" "Quectel fake-mm RM520N-GL"
expect "and finds VoLTE off" "$out" "volte             change"
"$P" device use flipper-one
out=$(until_has "usb audio         change" "$P" modem setup)
expect "on the Flipper One it checks USB audio" "$out" "usb audio         change"
out=$("$P" fake dump)
expect "the profile's start-up command reached the modem" "$out" "qpcmv=1,2"
out=$("$P" modem setup apply)
expect "applying changes VoLTE" "$out" "volte             changed"
expect "and restarts the modem" "$out" "restart           changed"
out=$("$P" fake dump)
expect "the modem has USB audio, VoLTE and its carrier profile" "$out" "uac=1 ims=1 autosel=1"
out=$("$P" modem setup)
refute "and a second check finds nothing to do" "$out" "change"
"$P" fake atdebug off
out=$("$P" modem setup)
expect "without ModemManager's debug mode, it says so" "$out" "only in debug mode"
"$P" fake atdebug on
"$P" device use auto

"$P" fake signal 30 5G
out=$(until_has "5G 30%" "$P" status)
expect "signal changes reach the status, without waiting for a poll" "$out" "5G 30%"
out=$("$P" wipe)
expect "wiping asks first" "$out" "waiting for confirmation"
"$P" confirm "$(awk '{print $NF}' <<<"$out" | tr -d ')')" >/dev/null
out=$("$P" threads)
[ -z "$out" ] && pass "wipe clears messages" || fail "wipe clears messages" "$out"
[ ! -e "$XDG_DATA_HOME/dolphone/mms" ] && pass "and MMS pictures" || fail "wipe clears MMS pictures" "$(ls "$XDG_DATA_HOME/dolphone/mms")"

echo "standing grants"
out=$("$P" grant 1900123456 2>&1 || true)
expect "premium numbers can never be granted" "$out" "cannot be granted"
out=$("$P" grant '0491570158,0491571*' 2 1)
expect "a grant waits for the device" "$out" "grant waiting for confirmation"
out=$("$P" confirm "$(awk '{print $NF}' <<<"$out" | tr -d ')')")
expect "and is stored once confirmed" "$out" "request "
out=$("$P" grants)
expect "it is listed with its limit" "$out" "phonectl         +61491570158,+61491571* at 2/h"

kill "$PHONED"
wait "$PHONED" 2>/dev/null || true

echo "phase 2: a script, not trusted, no read grant"
start_phoned
sleep 0.6
echo "a standing grant lets a script text without asking"
out=$("$P" send 0491570158 "From cron")
refute "a granted number goes straight out" "$out" "waiting"
out=$("$P" send 0491571234 "From cron")
refute "and so does a granted prefix" "$out" "waiting"
out=$("$P" send 0491571235 "Over the limit" 2>&1 || true)
expect "but only as often as the grant says" "$out" "allows 2 an hour"
out=$("$P" grants 2>&1 || true)
expect "a script cannot list grants" "$out" "only Dolphone's own apps"
out=$("$P" audit 2>&1 || true)
expect "or read the audit log" "$out" "only Dolphone's own apps"
echo "outside requests wait for the device"
out=$("$P" send 0491570177 No worries)
expect "a text from a script is held" "$out" "waiting for confirmation"
out=$("$P" dial 0491570159)
expect "a call from a script is held" "$out" "waiting for confirmation"
out=$("$P" mms 0491570162 --file "$DOLPHONE_MMS_DIR/cat.jpg" "From a script")
expect "an MMS from a script is held too" "$out" "message waiting for confirmation"
out=$("$P" pending)
expect "both are pending" "$out" "call 0491 570 159 by phonectl (external)"
out=$("$P" confirm 1 2>&1 || true)
expect "a script cannot confirm its own request" "$out" "only Dolphone's own apps can confirm"
for id in $("$P" pending | awk '{print $1}'); do "$P" deny "$id"; done
out=$("$P" pending)
[ -z "$out" ] && pass "denied requests are gone" || fail "denied requests are gone" "$out"
out=$("$P" ussd '*100#' 2>&1 || true)
expect "USSD is refused from a script" "$out" "only run from Dolphone's dialer"
out=$("$P" ussd '**21*0491570198#' 2>&1 || true)
expect "so is forwarding calls (it would hand them to someone else)" "$out" "only run from Dolphone's dialer"
out=$("$P" modem setup)
expect "a script may check the modem's setup" "$out" "Quectel"
out=$("$P" modem setup apply)
expect "but changing it waits for the device" "$out" "modem setup waiting for confirmation"
"$P" deny "$(awk '{print $NF}' <<<"$out" | tr -d ')')"
out=$("$P" set read_grants phonectl)
expect "a script asking to read messages waits for the device" "$out" "setting waiting for confirmation"
"$P" deny "$(awk '{print $NF}' <<<"$out" | tr -d ')')"
out=$("$P" device use generic)
expect "so does choosing a device profile" "$out" "setting waiting for confirmation"
req=$(awk '{print $NF}' <<<"$out" | tr -d ')')
out=$("$P" pending)
expect "and the request names the setting" "$out" "setting device_profile by phonectl"
"$P" deny "$req"
out=$("$P" set sip_password hunter2 2>&1 || true)
expect "the SIP password is never set from a script" "$out" "only set in Phone Settings"
out=$("$P" dial 000)
expect "an emergency call from a script is held, not placed" "$out" "waiting for confirmation"
out=$("$P" contacts add "Your Bank" 0491570180)
expect "a script adding a contact waits for the device" "$out" "contact waiting for confirmation"
out=$("$P" contacts 2>&1 || true)
expect "and cannot read contacts without a grant" "$out" "may not read contacts"

out=$("$P" threads 2>&1 || true)
expect "a script cannot read messages" "$out" "may not read messages"
out=$("$P" recents 2>&1 || true)
expect "or the call log" "$out" "may not read messages"
out=$("$P" block 0491570170 2>&1 || true)
expect "a script cannot change the block list" "$out" "only Dolphone's own apps"
"$P" fake ring 0491570171 >/dev/null
out=$(until_has "only Dolphone's own apps can answer" "$P" answer)
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
"$P" otp --from 0491570174 --wait 5 >/tmp/dolphone-e2e-otp.log 2>&1 &
OTP=$!
sleep 0.4
"$P" fake sms 0491570175 "Your code is 111111 (someone else)" >/dev/null
"$P" fake sms 0491570174 "G-735102 is your verification code" >/dev/null
wait "$OTP" && pass "otp --wait exits once the code arrives" || fail "otp exits" "$(cat /tmp/dolphone-e2e-otp.log)"
out=$(cat /tmp/dolphone-e2e-otp.log)
expect "with the code from the sender asked for" "$out" "735102"
out=$("$P" thread 0491570172)
expect "a granted script can read" "$out" "Your code is 482913"
echo "the HTTP gateway"
if command -v curl >/dev/null; then
    "$BIN/dolphone-gateway" --listen 127.0.0.1:18475 >/tmp/dolphone-e2e-gateway.log 2>&1 &
    GW=$!
    sleep 0.5
    TOKEN=$("$BIN/dolphone-gateway" --print-token)
    out=$(curl -s -o /dev/null -w "%{http_code}" http://127.0.0.1:18475/v1/status)
    [ "$out" = 401 ] && pass "the gateway wants its token" || fail "gateway auth" "$out"
    out=$(curl -s -H "Authorization: Bearer $TOKEN" http://127.0.0.1:18475/v1/status)
    expect "and with it reports the modem" "$out" '"operator":"Fakecom AU"'
    out=$(curl -s -H "Authorization: Bearer $TOKEN" -d '{"to":"0491570195","body":"From HTTP"}' http://127.0.0.1:18475/v1/messages)
    expect "a text through it waits for the device, as any program's does" "$out" '"pending"'
    out=$(curl -s -H "Authorization: Bearer $TOKEN" http://127.0.0.1:18475/v1/threads)
    expect "reading needs the read grant too" "$out" "may not read messages"
    kill $GW 2>/dev/null || true
else
    echo "  skip  curl is not installed"
fi
out=$("$P" block 0491570173 2>&1 || true)
expect "but still cannot change settings" "$out" "only Dolphone's own apps"
kill "$PHONED"
wait "$PHONED" 2>/dev/null || true

echo "the audit log, from Dolphone's own Settings"
DOLPHONE_TRUST_ALL=1 start_phoned
sleep 0.4
out=$(until_has "sent under a grant" "$P" audit)
expect "sends under a grant are logged" "$out" "sent under a grant"
expect "so are refusals" "$out" "over its grant's limit"
expect "and denials" "$out" "denied"
expect "and reads by a granted script" "$out" "read "
# Stored numbers are canonical (+61491570158): look for the digits that would
# show through if masking failed, whatever the prefix.
refute "numbers in the log are masked" "$out" "491570158"
kill "$PHONED"
# Let it go (and take its baresip with it) before the next run looks for one.
wait "$PHONED" 2>/dev/null || true
echo "an encrypted store"
DB="$XDG_DATA_HOME/dolphone/dolphone.db"
if echo probe | systemd-creds --user encrypt --name=probe - - >/dev/null 2>&1; then
    DOLPHONE_TRUST_ALL=1 start_phoned
    "$P" fake sms 0491570190 "Meet at the usual place" >/dev/null
    out=$(until_has "stop phoned first" "$BIN/phoned" --encrypt-store)
    expect "it is not rewritten while phoned has it open" "$out" "stop phoned first"
    kill "$PHONED"
    wait "$PHONED" 2>/dev/null || true
    out=$("$BIN/phoned" --encrypt-store 2>&1)
    expect "phoned --encrypt-store encrypts it" "$out" "encrypted "
    head -c 15 "$DB" | grep -qa "SQLite format" && fail "the file is no longer plain SQLite" "$(head -c 15 "$DB")" \
        || pass "the file is no longer plain SQLite"
    grep -qa "usual place" "$DB" && fail "no message text in it" "found" || pass "no message text in it"
    DOLPHONE_TRUST_ALL=1 start_phoned
    out=$("$P" thread 0491570190)
    expect "phoned unseals the key and reads it" "$out" "Meet at the usual place"
    kill "$PHONED"
    wait "$PHONED" 2>/dev/null || true
    mv "$XDG_CONFIG_HOME/dolphone/store-key.cred" "$XDG_CONFIG_HOME/dolphone/key.away"
    DOLPHONE_TRUST_ALL=1 start_phoned
    out=$("$P" threads)
    refute "without its key the store stays locked" "$out" "usual place"
    "$P" fake sms 0491570191 "While locked" >/dev/null
    out=$(until_has "sms " "$P" fake dump)
    expect "and a text that arrives stays on the SIM" "$out" "sms "
    kill "$PHONED"
    wait "$PHONED" 2>/dev/null || true
    [ -e "$DB" ] && ! head -c 15 "$DB" | grep -qa "SQLite format" && pass "the locked store is left as it was" \
        || fail "the locked store is left as it was" "$(ls "$XDG_DATA_HOME/dolphone")"
    mv "$XDG_CONFIG_HOME/dolphone/key.away" "$XDG_CONFIG_HOME/dolphone/store-key.cred"
    out=$("$BIN/phoned" --decrypt-store 2>&1)
    expect "and --decrypt-store turns it back" "$out" "decrypted "
    head -c 15 "$DB" | grep -qa "SQLite format" && pass "to plain SQLite" || fail "to plain SQLite" "$(head -c 15 "$DB")"
else
    echo "  skip  needs systemd-creds --user (systemd 256 or later)"
fi
echo "serial PCM call audio (a SIM7600, as in the uConsole)"
if command -v python3 >/dev/null && command -v pw-cat >/dev/null && [ -S "${DOLPHONE_PW_RUNTIME:-}/pipewire-0" ]; then
    # The rest of the run keeps PipeWire out of reach; this needs the real one.
    PIPEWIRE_RUNTIME_DIR=$DOLPHONE_PW_RUNTIME DOLPHONE_TRUST_ALL=1 start_phoned
    sleep 0.4
    # A pty stands in for the modem's audio port: whatever phoned bridges into it
    # is read back out and counted.
    PTYLOG=$(mktemp)
    python3 -c '
import os, pty, sys, time
m, s = pty.openpty()
print(os.ttyname(s), flush=True)
# 20 ms of silence every 20 ms, as the modem would send; a pty holds only a few
# KB, so what was written beyond that is what the downlink read.
written, heard, end = 0, 0, time.time() + 12
os.set_blocking(m, False)
while time.time() < end:
    try:
        written += os.write(m, bytes(320))
    except BlockingIOError:
        pass
    try:
        heard += len(os.read(m, 4096))
    except BlockingIOError:
        pass
    time.sleep(0.02)
print(written, heard, flush=True)
' >"$PTYLOG" &
    PTY=$!
    sleep 0.3
    TTY=$(head -1 "$PTYLOG")
    printf '{"serial_pcm": "%s"}' "$TTY" >"$XDG_CONFIG_HOME/dolphone/audio.json"
    "$P" device use clockworkpi-uconsole
    "$P" fake ring 0491570156 >/dev/null
    sleep 0.4
    "$P" answer
    sleep 2
    expect "the call's audio is bridged over the serial port" "$(cat /tmp/dolphone-e2e-phoned.log)" "call audio over serial PCM port=$TTY"
    out=$(ps -o stat=,args= -C pw-cat | grep -v '^Z' || true)
    expect "with pw-cat at 8 kHz, raw, into the call" "$out" "pw-cat --playback --raw --rate 8000 --channels 1 --format s16"
    expect "and from the microphone" "$out" "pw-cat --record --raw --rate 8000"
    out=$("$P" fake dump)
    # ModemManager's SIMCom plugin sends AT+CPCMREG=1 and =0 itself
    # (mm-shared-simtech.c); a second one from phoned would only fail.
    refute "phoned leaves AT+CPCMREG to ModemManager's SIMCom plugin" "$out" "cpcmreg=1"
    "$P" hangup
    for _ in $(seq 1 50); do pgrep -x pw-cat >/dev/null || break; sleep 0.1; done
    out=$(pgrep -a pw-cat || true)
    [ -z "$out" ] && pass "the bridge stops with the call" || fail "the bridge stops with the call" "$out"
    wait "$PTY" 2>/dev/null || true
    read -r written heard < <(sed -n 2p "$PTYLOG")
    # About 2.5 s of the call at 16 kB/s; the pty alone would hold 4 kB.
    [ "${written:-0}" -gt 20000 ] && pass "the modem's PCM is played as it arrives (${written} bytes)" \
        || fail "the modem's PCM is played as it arrives" "${written:-none} bytes written"
    [ "${heard:-0}" -gt 20000 ] && pass "and the microphone's goes to the modem (${heard} bytes)" \
        || fail "and the microphone's goes to the modem" "${heard:-none} bytes"
    rm -f "$XDG_CONFIG_HOME/dolphone/audio.json" "$PTYLOG"
    "$P" device use auto
    kill "$PHONED"
    wait "$PHONED" 2>/dev/null || true
else
    echo "  skip  needs python3, pw-cat and a running PipeWire"
fi
kill "${MMSD:-}" 2>/dev/null || true
echo
if [ "$fails" -eq 0 ]; then echo "all passed"; else echo "$fails failed (log: /tmp/dolphone-e2e-phoned.log)"; exit 1; fi
