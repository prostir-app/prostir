#!/usr/bin/env bash
# Runs the headset app's selftest over adb and prints its report (key=value lines),
# exiting with its code: 0 passed, 1 a check failed (fail= says which), 2 it could not
# run, 3 it timed out (or no report came).
#
# The app runs a fixed scenario against a fake PC in its own process (its own data,
# nothing of the person's touched, no real PC involved): two monitors from the clips
# in the APK, a monitor moved, the menu, a frame the decoder cannot take, a stream
# started over, the connection dropped and back. Every build has the selftest but the
# store's, which ignores it (no report comes: exit 3).
#
#   scripts/selftest-headset.sh [--seconds N] [--relaunch] [--decoder-rate max]
#                                [--package NAME] [--out DIR]
#
#   --seconds N    how long the calm part, where frame times are measured, lasts (30);
#                  3600 makes an hour's run for battery and heat
#   --relaunch     the first run ends without leaving the process, a second start runs
#                  in the same one: its report must have the same pid and run=2
#   --decoder-rate max  the decoders are asked to run as fast as they can
#                  (operating-rate), which the app never asks otherwise: an A/B of
#                  decode_ms against battery.* with a run without it
#   --package      dev.prostir.client.preview (the preview build, the default when it
#                  is installed) or dev.prostir.client (the default otherwise)
#   --out DIR      where the reports and the app's log go (a new directory in /tmp)
#
# ANDROID_SERIAL picks the headset when adb sees several.
set -euo pipefail

SECONDS_CALM=30
RELAUNCH=0
DECODER_RATE=default
PACKAGE=""
OUT=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --seconds) SECONDS_CALM="$2"; shift 2 ;;
        --relaunch) RELAUNCH=1; shift ;;
        --decoder-rate) DECODER_RATE="$2"; shift 2 ;;
        --package) PACKAGE="$2"; shift 2 ;;
        --out) OUT="$2"; shift 2 ;;
        --pc | --pc=*)
            echo "!! the selftest against a real prostir (pc=usb) comes in a later step" >&2
            exit 2 ;;
        -h | --help) sed -n '2,26p' "$0"; exit 0 ;;
        *) echo "!! unknown option $1 (see --help)" >&2; exit 2 ;;
    esac
done
[[ "$SECONDS_CALM" =~ ^[0-9]+$ ]] && (( SECONDS_CALM >= 1 && SECONDS_CALM <= 7200 )) \
    || { echo "!! --seconds: 1 to 7200" >&2; exit 2; }
case "$PACKAGE" in
    "" | dev.prostir.client.preview | dev.prostir.client) ;;
    *) echo "!! --package: dev.prostir.client.preview or dev.prostir.client" >&2; exit 2 ;;
esac
OUT="${OUT:-$(mktemp -d "${TMPDIR:-/tmp}/prostir-selftest.XXXXXX")}"
mkdir -p "$OUT"

shell() { adb shell "$@" | tr -d '\r'; }

adb get-state >/dev/null 2>&1 || { echo "!! no headset over adb (adb devices)" >&2; exit 2; }
installed() { shell pm path "$1" 2>/dev/null | grep -q "^package:"; }
if [[ -z "$PACKAGE" ]]; then
    PACKAGE=dev.prostir.client
    installed dev.prostir.client.preview && PACKAGE=dev.prostir.client.preview
fi
installed "$PACKAGE" \
    || { echo "!! $PACKAGE is not installed on the headset" >&2; exit 2; }
if [[ "$PACKAGE" == dev.prostir.client ]]; then
    echo ">> testing dev.prostir.client, the app used every day: it is stopped for the test" \
        "(its own data stays as it is; the selftest keeps its own)"
fi

# Keeping the headset awake on a table, the way each maker allows: the proximity
# sensor told it is worn. A headset not in this table has to be worn.
MAKER="$(shell getprop ro.product.manufacturer)"
PROX_ON=() PROX_OFF=()
case "$MAKER" in
    Oculus | Meta)
        PROX_ON=(am broadcast -a com.oculus.vrpowermanager.prox_close)
        PROX_OFF=(am broadcast -a com.oculus.vrpowermanager.automation_disable) ;;
    *)
        echo ">> no known way to keep a $MAKER headset awake on a table: put it on, then press Enter"
        read -r ;;
esac
# Started and not finished: a selftest left running (Ctrl-C, no report in time) is
# stopped, so its fake PC does not stream on.
STARTED=0 FINISHED=0
restore() {
    if (( STARTED && !FINISHED )); then
        shell am force-stop "$PACKAGE" >/dev/null || true
    fi
    if [[ ${#PROX_OFF[@]} -gt 0 ]]; then
        shell "${PROX_OFF[@]}" >/dev/null || true
    fi
}
trap restore EXIT

battery() {
    local dump level temp
    dump="$(shell dumpsys battery)"
    level="$(sed -n 's/^ *level: //p' <<<"$dump" | head -1)"
    temp="$(sed -n 's/^ *temperature: //p' <<<"$dump" | head -1)"
    echo "${level:-none} ${temp:+$((temp / 10)).$((temp % 10))}"
}

REPORT="/sdcard/Android/data/$PACKAGE/files/selftest.txt"
WAIT=$(( SECONDS_CALM + 90 ))

# Waits for the report of a run (it ends with `exit=`) and copies it to $1.
fetch() {
    local into="$1" until=$(( $(date +%s) + WAIT ))
    while (( $(date +%s) < until )); do
        if shell cat "$REPORT" 2>/dev/null | grep -q '^exit='; then
            shell cat "$REPORT" > "$into"
            return 0
        fi
        sleep 2
    done
    return 1
}

# The exit code a report gives: its `exit=`, or 3 when that is not one of the codes.
exit_of() {
    local v
    v="$(sed -n 's/^exit=//p' "$1" | tail -1)"
    if [[ "$v" =~ ^[0-3]$ ]]; then echo "$v"; else echo 3; fi
}

start() {
    shell am start -n "$PACKAGE/android.app.NativeActivity" --es selftest "$1" >/dev/null
}

read -r BATTERY_START TEMP_START <<<"$(battery)"
SINCE="$(shell date '+%m-%d %H:%M:%S.000')"
shell rm -f "$REPORT"
# The activity is singleTask: a running app would only come to the front, without
# the extra. Stopped first, the start is a new one that reads it.
shell am force-stop "$PACKAGE"
shell input keyevent KEYCODE_WAKEUP >/dev/null || true
if [[ ${#PROX_ON[@]} -gt 0 ]]; then
    shell "${PROX_ON[@]}" >/dev/null
fi
sleep 1
OPTIONS="seconds=$SECONDS_CALM"
(( RELAUNCH )) && OPTIONS+=",relaunch=1"
[[ $DECODER_RATE != default ]] && OPTIONS+=",decoder_rate=$DECODER_RATE"
echo ">> $PACKAGE on a $MAKER $(shell getprop ro.product.model): selftest $OPTIONS (up to ${WAIT}s)"
STARTED=1
start "$OPTIONS"

code=0
if fetch "$OUT/selftest.txt"; then
    code="$(exit_of "$OUT/selftest.txt")"
else
    echo "!! no report within ${WAIT}s" >&2
    code=3
fi

if (( RELAUNCH )) && [[ -f "$OUT/selftest.txt" ]]; then
    # The same process, started again (no force-stop).
    shell rm -f "$REPORT"
    sleep 3
    echo ">> second start in the same process"
    start "seconds=$SECONDS_CALM"
    if fetch "$OUT/selftest-2.txt"; then
        second="$(exit_of "$OUT/selftest-2.txt")"
        (( second > code )) && code="$second"
        pid1="$(sed -n 's/^pid=//p' "$OUT/selftest.txt")"
        pid2="$(sed -n 's/^pid=//p' "$OUT/selftest-2.txt")"
        run2="$(sed -n 's/^run=//p' "$OUT/selftest-2.txt")"
        if [[ "$pid1" != "$pid2" || "$run2" != 2 ]]; then
            echo "!! the second start was not in the same process (pid $pid1, then $pid2, run $run2)" >&2
            (( code < 1 )) && code=1
        fi
    else
        echo "!! no report of the second start within ${WAIT}s" >&2
        code=3
    fi
fi
(( code < 3 )) && FINISHED=1

read -r BATTERY_END TEMP_END <<<"$(battery)"
adb logcat -d -T "$SINCE" 2>/dev/null | grep -E 'prostir' > "$OUT/logcat.txt" || true

for report in "$OUT"/selftest.txt "$OUT"/selftest-2.txt; do
    [[ -f "$report" ]] || continue
    echo "---- ${report##*/}"
    cat "$report"
done
echo "---- PC side"
echo "pc.device_battery.start=$BATTERY_START"
echo "pc.device_battery.end=$BATTERY_END"
echo "pc.device_temp_c.start=${TEMP_START:-none}"
echo "pc.device_temp_c.end=${TEMP_END:-none}"
# The app does not wait for its threads on exit yet, so threads_left is only written
# down for now; it becomes a check once the app joins them.
echo "pc.threads_left=recorded, not checked"
echo "pc.out=$OUT"
echo "pc.exit=$code"
exit "$code"
