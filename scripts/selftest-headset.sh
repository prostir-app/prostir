#!/usr/bin/env bash
# Runs the headset app's selftest over adb and prints its report (key=value lines),
# exiting with its code: 0 passed, 1 a check failed (fail= says which), 2 it could not
# run, 3 it timed out (or no report came).
#
# The app runs a fixed scenario against a fake PC in its own process (its own data,
# nothing of the person's touched, no real PC involved): two monitors from the clips
# in the APK, a monitor moved, the menu, a frame the decoder cannot take, a stream
# started over, one started over without its keyframe (the first ask for one ignored),
# the connection dropped and back. Every build has the selftest but the
# store's, which ignores it (no report comes: exit 3).
#
#   scripts/selftest-headset.sh [--seconds N] [--relaunch] [--decoder-rate max]
#                                [--caps minimal] [--perf low|high]
#                                [--pc] [--pc-port PORT] [--package NAME] [--out DIR]
#
#   --seconds N    how long the calm part, where frame times are measured, lasts (30);
#                  3600 makes an hour's run for battery and heat
#   --relaunch     the first run ends without leaving the process, a second start runs
#                  in the same one: its report must have the same pid and run=2
#   --decoder-rate max  the decoders are asked to run as fast as they can
#                  (operating-rate), which the app never asks otherwise: an A/B of
#                  decode_ms against battery.* with a run without it
#   --caps minimal the app turns on only the OpenXR extensions it cannot do without, so
#                  it runs as on a headset without the others (the menu hides what needs
#                  them; caps= in the report says minimal)
#   --perf L       asks the runtime for performance level L (low or high; the app
#                  itself asks none): frame times and battery.* to compare with a run
#                  without it
#   --pc           against a real PC app instead (pc=usb): this script builds and
#                  starts `prostir serve` with the fake desktop and its own settings,
#                  bus and runtime directory (`--no-adb --no-discovery --time-code`, on
#                  host port PORT, 47930 by default), so a prostir already running
#                  here is left alone; makes both sides trust each other (keys pushed to
#                  files/selftest-usb/ of the app), points the headset's 127.0.0.1:47830
#                  at it with `adb reverse` (a running prostir started without --no-adb
#                  may point it back at itself when USB changes: run that one with
#                  --no-adb meanwhile), serves the PC's clock on 47931 for the latency
#                  measurement, runs `prostir ctl recenter` and starts the PC app again
#                  when the app asks. Every reverse is put back as it was on the way out
#   --package      dev.prostir.client.preview (the preview build, the default when it
#                  is installed) or dev.prostir.client (the default otherwise)
#   --out DIR      where the reports and the app's log go (a new directory in /tmp)
#
# ANDROID_SERIAL picks the headset when adb sees several.
set -euo pipefail

SECONDS_CALM=30
RELAUNCH=0
DECODER_RATE=default
CAPS=full
PERF=default
PC=0
PC_PORT=47930
CLOCK_PORT=47931
PACKAGE=""
OUT=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --seconds) SECONDS_CALM="$2"; shift 2 ;;
        --relaunch) RELAUNCH=1; shift ;;
        --decoder-rate) DECODER_RATE="$2"; shift 2 ;;
        --package) PACKAGE="$2"; shift 2 ;;
        --out) OUT="$2"; shift 2 ;;
        --pc) PC=1; shift ;;
        --pc-port) PC_PORT="$2"; shift 2 ;;
        --caps) CAPS="$2"; shift 2 ;;
        --perf) PERF="$2"; shift 2 ;;
        -h | --help) sed -n '2,45p' "$0"; exit 0 ;;
        *) echo "!! unknown option $1 (see --help)" >&2; exit 2 ;;
    esac
done
[[ "$SECONDS_CALM" =~ ^[0-9]+$ ]] && (( SECONDS_CALM >= 1 && SECONDS_CALM <= 7200 )) \
    || { echo "!! --seconds: 1 to 7200" >&2; exit 2; }
case "$PACKAGE" in
    "" | dev.prostir.client.preview | dev.prostir.client) ;;
    *) echo "!! --package: dev.prostir.client.preview or dev.prostir.client" >&2; exit 2 ;;
esac
(( PC && RELAUNCH )) && { echo "!! --pc and --relaunch: one at a time" >&2; exit 2; }
[[ "$PC_PORT" =~ ^[0-9]+$ ]] && (( PC_PORT != 47830 )) \
    || { echo "!! --pc-port: a port of its own (not 47830, which a running prostir may hold)" >&2; exit 2; }
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
    if (( PC )); then
        pc_restore
    fi
}

# ---- Against a real PC app (--pc) ----
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PC_DIR="" PC_PID="" BUS_PID="" BUS_DIR="" CLOCK_PID="" REVERSE_BEFORE="" PC_ASKED=""
# Which of the headset's ports this script pointed elsewhere (and so puts back).
REVERSED_USB=0 REVERSED_CLOCK=0
# The test PC app's environment: nothing of the person's (settings, bus, desktop).
pc_env() {
    env -u WAYLAND_DISPLAY -u DISPLAY -u HYPRLAND_INSTANCE_SIGNATURE -u DESKTOP_AUTOSTART_ID \
        XDG_CONFIG_HOME="$PC_DIR/config" XDG_RUNTIME_DIR="$PC_DIR/run" \
        XDG_CACHE_HOME="$PC_DIR/cache" HOME="$PC_DIR/home" \
        DBUS_SESSION_BUS_ADDRESS="${PC_BUS:-unix:path=$PC_DIR/run/no-bus}" \
        PROSTIR_FAKE_DESKTOP=1 PROSTIR_NO_COUNT=1 PROSTIR_FAKE_FPS=30 \
        PROSTIR_FAKE_PICTURE=bands LANGUAGE=en RUST_LOG=info "$@"
}
pc_start() {
    # Only what this start logs counts (a start before it said the same).
    local from
    from=$(( $(wc -l <"$OUT/pc.log") + 1 ))
    pc_env "$PROSTIR" serve --port "$PC_PORT" --no-adb --no-discovery --stay --time-code \
        >>"$OUT/pc.log" 2>&1 &
    PC_PID=$!
    local until=$(( $(date +%s) + 60 ))
    while ! tail -n +"$from" "$OUT/pc.log" | grep -q "waiting for the headset on port $PC_PORT"; do
        kill -0 "$PC_PID" 2>/dev/null || { echo "!! the test PC app ended (see $OUT/pc.log)" >&2; return 1; }
        (( $(date +%s) < until )) || { echo "!! the test PC app did not start in 60 s" >&2; return 1; }
        sleep 0.5
    done
}
pc_stop() {
    [[ -n "$PC_PID" ]] || return 0
    kill -TERM "$PC_PID" 2>/dev/null || true
    for _ in $(seq 1 100); do kill -0 "$PC_PID" 2>/dev/null || break; sleep 0.1; done
    kill -KILL "$PC_PID" 2>/dev/null || true
    wait "$PC_PID" 2>/dev/null || true
    PC_PID=""
}
pc_setup() {
    command -v python3 >/dev/null || { echo "!! --pc needs python3 (for the PC's clock)" >&2; exit 2; }
    echo ">> building the PC app with the fake desktop, and prostir-trust"
    # A target directory of its own: the fake desktop's build never replaces the
    # prostir built for the desktop in the usual one.
    local target="${PROSTIR_SELFTEST_TARGET:-$ROOT/target/selftest-pc}"
    (cd "$ROOT" && CARGO_TARGET_DIR="$target" cargo build -q -p prostir \
        --no-default-features --features fake-desktop \
        && CARGO_TARGET_DIR="$target" cargo build -q -p prostir-testkit --bin prostir-trust) || exit 2
    PROSTIR="$target/debug/prostir"
    PC_DIR="$OUT/pc"
    mkdir -p "$PC_DIR"/{config/prostir,run,home,cache,headset}
    chmod 700 "$PC_DIR/run"
    if command -v dbus-daemon >/dev/null; then
        # A socket's path is short (108 bytes): its own directory, whatever --out is.
        BUS_DIR="$(mktemp -d "${TMPDIR:-/tmp}/prostir-bus.XXXXXX")"
        dbus-daemon --session --nofork --print-address --address="unix:path=$BUS_DIR/bus" \
            >"$PC_DIR/bus.address" 2>/dev/null &
        BUS_PID=$!
        for _ in $(seq 1 50); do [[ -s "$PC_DIR/bus.address" ]] && break; sleep 0.1; done
        PC_BUS="$(head -1 "$PC_DIR/bus.address")"
    else
        echo ">> no dbus-daemon: \`prostir ctl recenter\` cannot run (command_ms stays none)"
    fi
    "$target/debug/prostir-trust" "$PC_DIR/headset" "$PC_DIR/config/prostir" >/dev/null || exit 2
    local keys="/sdcard/Android/data/$PACKAGE/files/selftest-usb"
    shell rm -rf "$keys"
    shell mkdir -p "$keys"
    adb push "$PC_DIR/headset/identity" "$PC_DIR/headset/paired_hosts" "$keys/" >/dev/null || exit 2
    # The PC's clock, for the offset of the two clocks: CLOCK_MONOTONIC in µs a line.
    python3 -I -c '
import socket, sys, time, threading
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", int(sys.argv[1]))); s.listen(4)
def serve(c):
    with c:
        c.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        for _ in c.makefile("rb"):
            c.sendall(b"%d\n" % (time.clock_gettime_ns(time.CLOCK_MONOTONIC) // 1000))
while True:
    threading.Thread(target=serve, args=(s.accept()[0],), daemon=True).start()
' "$CLOCK_PORT" &
    CLOCK_PID=$!
    local listening=0
    for _ in $(seq 1 50); do
        kill -0 "$CLOCK_PID" 2>/dev/null || break
        if python3 -I -c 'import socket, sys; socket.create_connection(("127.0.0.1", int(sys.argv[1])), 1).close()' \
            "$CLOCK_PORT" 2>/dev/null; then
            listening=1
            break
        fi
        sleep 0.1
    done
    (( listening )) || { echo "!! the PC's clock does not listen on $CLOCK_PORT (taken?)" >&2; exit 2; }
    # How the headset's ports pointed before, so only what this script changed is put
    # back, and as it was.
    REVERSE_BEFORE="$(adb reverse --list 2>/dev/null | tr -d '\r')" || exit 2
    if grep -q "tcp:47830" <<<"$REVERSE_BEFORE"; then
        echo ">> the headset's 127.0.0.1:47830 pointed at a prostir here; it points at the test one" \
            "for now and back afterwards (a prostir without --no-adb may take it back meanwhile)"
    fi
    # Marked just before each change: a reverse that half went through (or the
    # script stopped right after it) is put back too; putting back is harmless when
    # nothing changed, as the state before is known.
    REVERSED_USB=1
    adb reverse tcp:47830 "tcp:$PC_PORT" >/dev/null || exit 2
    REVERSED_CLOCK=1
    adb reverse "tcp:$CLOCK_PORT" "tcp:$CLOCK_PORT" >/dev/null || exit 2
    : >"$OUT/pc.log"
    pc_start || exit 2
}
# What the app asks (files/selftest-ask.txt), done once each.
pc_tick() {
    local ask
    ask="$(shell cat "/sdcard/Android/data/$PACKAGE/files/selftest-ask.txt" 2>/dev/null || true)"
    [[ -n "$ask" && "$ask" != "$PC_ASKED" ]] || return 0
    PC_ASKED="$ask"
    case "$ask" in
        command)
            echo ">> the app asks for a command from the PC: prostir ctl recenter"
            pc_env "$PROSTIR" ctl recenter >>"$OUT/pc-ctl.log" 2>&1 || true ;;
        restart)
            echo ">> the app asks for the PC app to start again"
            pc_stop
            pc_start || true ;;
    esac
}
pc_restore() {
    pc_stop
    if [[ -n "$CLOCK_PID" ]]; then kill "$CLOCK_PID" 2>/dev/null || true; fi
    if [[ -n "$BUS_PID" ]]; then kill "$BUS_PID" 2>/dev/null || true; fi
    if [[ -n "$BUS_DIR" ]]; then rm -rf "$BUS_DIR" || true; fi
    # Only a port this script pointed elsewhere is put back, as it was before (a
    # reverse of someone else's is never removed).
    local remote local_ port
    for port in 47830 "$CLOCK_PORT"; do
        if [[ "$port" == 47830 ]]; then (( REVERSED_USB )) || continue; else (( REVERSED_CLOCK )) || continue; fi
        adb reverse --remove "tcp:$port" >/dev/null 2>&1 || true
        while read -r _ remote local_; do
            [[ "$remote" == "tcp:$port" ]] || continue
            adb reverse "$remote" "$local_" >/dev/null 2>&1 || true
        done <<<"$REVERSE_BEFORE"
    done
    shell rm -rf "/sdcard/Android/data/$PACKAGE/files/selftest-usb" \
        "/sdcard/Android/data/$PACKAGE/files/selftest-ask.txt" >/dev/null 2>&1 || true
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
# Against a real PC the app waits for this script for a few things.
(( PC )) && WAIT=$(( WAIT + 150 ))

# Waits for the report of a run (it ends with `exit=`) and copies it to $1.
fetch() {
    local into="$1" until=$(( $(date +%s) + WAIT ))
    while (( $(date +%s) < until )); do
        if shell cat "$REPORT" 2>/dev/null | grep -q '^exit='; then
            shell cat "$REPORT" > "$into"
            return 0
        fi
        if (( PC )); then
            pc_tick
            sleep 1
        else
            sleep 2
        fi
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
(( PC )) && pc_setup
OPTIONS="seconds=$SECONDS_CALM"
(( PC )) && OPTIONS+=",pc=usb"
(( RELAUNCH )) && OPTIONS+=",relaunch=1"
[[ $DECODER_RATE != default ]] && OPTIONS+=",decoder_rate=$DECODER_RATE"
[[ $CAPS != full ]] && OPTIONS+=",caps=$CAPS"
[[ $PERF != default ]] && OPTIONS+=",perf=$PERF"
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
if (( PC )); then
    echo "pc.app=$("$PROSTIR" --version 2>/dev/null | head -1)"
    echo "pc.encoder=$(grep -o 'encoder [^,;]*' "$OUT/pc.log" | head -1 || true)"
    echo "pc.log_errors=$(grep -c ' ERROR ' "$OUT/pc.log" || true)"
    echo "pc.log=$OUT/pc.log"
fi
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
