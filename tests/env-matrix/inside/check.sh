#!/usr/bin/env bash
# Inside the container: a compositor with no screen, PipeWire, Prostir (the real
# desktop backend, software encoder) and the fake headset. Writes /out/report.txt
# (key=value lines) and the logs next to it.
#
#   check.sh COMPOSITOR [MODE]       COMPOSITOR: kwin | hyprland | mutter | sway
#                                    MODE (kwin): desktopfile | none | nocheck
set -u
COMP="${1:?compositor}"
MODE="${2:-desktopfile}"
BIN="${BIN:-/target/release}"
OUT="${OUT:-/out}"
WAIT_SECONDS="${WAIT_SECONDS:-20}"

# Everything lives in a D-Bus session of its own.
if [ "${EM_INNER:-}" != 1 ]; then
    export EM_INNER=1
    exec dbus-run-session -- bash "$0" "$@"
fi

T=/tmp/em
rm -rf "$T"
mkdir -p "$T"/{run,home,pcconf,headhome} "$OUT"
chmod 700 "$T/run"
export XDG_RUNTIME_DIR="$T/run" HOME="$T/home" XDG_CONFIG_HOME="$T/home/.config"
export XDG_DATA_HOME="$T/home/.local/share" XDG_CACHE_HOME="$T/home/.cache"
export XDG_SESSION_TYPE=wayland WAYLAND_DISPLAY=wayland-em
unset DISPLAY
REPORT="$OUT/report.txt"
: >"$REPORT"
PIDS=()

rep() { echo "$1=$2" | tee -a "$REPORT"; }
bg() { # bg NAME COMMAND...: starts a process with its log in /out
    local n="$1"
    shift
    "$@" >"$OUT/$n.log" 2>&1 &
    PIDS+=($!)
    eval "PID_$n=$!"
}
cleanup() {
    cp "$XDG_RUNTIME_DIR"/hypr/*/hyprland.log "$OUT/hyprland-own.log" 2>/dev/null
    cp "$HOME"/.cache/hyprland/hyprlandCrashReport*.txt "$OUT/" 2>/dev/null
    for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null; done
    sleep 1
    for p in "${PIDS[@]}"; do kill -9 "$p" 2>/dev/null; done
}
trap cleanup EXIT
wait_for() { # wait_for SECONDS COMMAND...
    local n="$1"
    shift
    for _ in $(seq $((n * 5))); do
        "$@" >/dev/null 2>&1 && return 0
        sleep 0.2
    done
    return 1
}

# Versions of what is installed (whatever of these exists).
ver() { rep "$1" "$(eval "$2" 2>&1 | head -1 | tr -d '\r')"; }
os="$(. /etc/os-release && echo "$PRETTY_NAME")"
rep os "$os"
rep compositor "$COMP"
rep mode "$MODE"
ver glibc 'ldd --version'
ver pipewire 'pipewire --version | tail -1'
ver wireplumber 'wireplumber --version | tail -1'
ver gstreamer 'gst-inspect-1.0 --gst-version'
ver x264enc 'gst-inspect-1.0 x264enc | grep -m1 "Long-name"'
ver pipewiresrc 'gst-inspect-1.0 pipewiresrc | grep -m1 "Long-name"'
ver vah264enc 'gst-inspect-1.0 vah264enc | grep -m1 "Long-name"'
ver kscreen_doctor 'command -v kscreen-doctor'
case "$COMP" in
kwin|kwin-nested) ver kwin 'kwin_wayland --version 2>&1 | grep -m1 "^kwin"' ;;
hyprland) ver hyprland 'Hyprland --version | head -1' ;;
mutter) ver mutter 'mutter --version' ;;
sway) ver sway 'sway --version' ;;
esac

# PipeWire with its session manager and the PulseAudio layer.
bg pipewire pipewire
wait_for 10 test -S "$XDG_RUNTIME_DIR/pipewire-0" || rep pipewire_up no
bg wireplumber wireplumber
bg pipewire_pulse pipewire-pulse
wait_for 10 pactl info && rep pipewire_up yes

# The desktop file Prostir ships, with the absolute path of the binary the way a
# package installs it: KWin (up to 6.7) finds the program by it.
mkdir -p "$XDG_DATA_HOME/applications"
if [ "$MODE" = plain ]; then
    sed -e "s|^Exec=.*|Exec=$BIN/prostir|" /src/packaging/io.github.prostir_app.Prostir.desktop \
        >"$XDG_DATA_HOME/applications/io.github.prostir_app.Prostir.desktop"
fi
if [ "$MODE" = desktopfile ] || [ "$MODE" = plain ]; then
    [ "$MODE" = plain ] || sed -e "s|^Exec=.*|Exec=$BIN/prostir|" \
        -e '/^Terminal=/a X-KDE-Wayland-Interfaces=zkde_screencast_unstable_v1' \
        /src/packaging/io.github.prostir_app.Prostir.desktop \
        >"$XDG_DATA_HOME/applications/io.github.prostir_app.Prostir.desktop"
    # A session has an applications menu (plasma-workspace); some images do not, and
    # kbuildsycoca then leaves the programs out of its cache.
    if ! ls /etc/xdg/menus/*applications.menu >/dev/null 2>&1; then
        mkdir -p "$XDG_CONFIG_HOME/menus"
        cat >"$XDG_CONFIG_HOME/menus/applications.menu" <<'MENU'
<!DOCTYPE Menu PUBLIC "-//freedesktop//DTD Menu 1.0//EN" "http://www.freedesktop.org/standards/menu-spec/menu-1.0.dtd">
<Menu><Name>Applications</Name><DefaultAppDirs/><DefaultDirectoryDirs/><DefaultMergeDirs/>
<Include><All/></Include></Menu>
MENU
        rep applications_menu "none in the image, a minimal one written"
    fi
    for b in kbuildsycoca6 kbuildsycoca5; do
        if command -v $b >/dev/null; then
            $b >"$OUT/sycoca.log" 2>&1
            rep sycoca "$b exit $?"
            break
        fi
    done
fi

start_compositor() {
    case "$COMP" in
    kwin)
        export XDG_CURRENT_DESKTOP=KDE KDE_FULL_SESSION=true
        export QT_FORCE_STDERR_LOGGING=1 QT_LOGGING_RULES="kwin_*.info=true;kwin_*.warning=true;kwin_screencast.debug=true;kwin_core.debug=true"
        [ "$MODE" = nocheck ] && export KWIN_WAYLAND_NO_PERMISSION_CHECKS=1
        # shellcheck disable=SC2086
        bg compositor kwin_wayland --virtual --no-lockscreen --no-global-shortcuts \
            --socket "$WAYLAND_DISPLAY" --width 1920 --height 1080 ${KWIN_EXTRA:-}
        ;;
    kwin-nested)
        # KWin up to 6.4 makes virtual outputs for a program only on a real or a nested
        # backend, not on --virtual: the inner KWin runs in a window of an outer one.
        export XDG_CURRENT_DESKTOP=KDE KDE_FULL_SESSION=true
        export QT_FORCE_STDERR_LOGGING=1 QT_LOGGING_RULES="kwin_*.info=true;kwin_*.warning=true;kwin_screencast.debug=true;kwin_core.debug=true"
        [ "$MODE" = nocheck ] && export KWIN_WAYLAND_NO_PERMISSION_CHECKS=1
        WAYLAND_DISPLAY=wayland-outer bg outer kwin_wayland --virtual --no-lockscreen --no-global-shortcuts \
            --socket wayland-outer --width 2560 --height 1440
        wait_for "$WAIT_SECONDS" test -S "$XDG_RUNTIME_DIR/wayland-outer"
        bg compositor kwin_wayland --wayland-display wayland-outer --no-lockscreen --no-global-shortcuts \
            --socket "$WAYLAND_DISPLAY" --width 1920 --height 1080
        ;;
    hyprland)
        cat >"$T/hypr.conf" <<'EOF'
misc {
    disable_hyprland_logo = true
    disable_splash_rendering = true
}
EOF
        export WLR_BACKENDS=headless WLR_RENDERER_ALLOW_SOFTWARE=1 WLR_NO_HARDWARE_CURSORS=1
        bg compositor Hyprland --i-am-really-stupid -c "$T/hypr.conf"
        # Hyprland picks its own socket name; Prostir finds the rest by the signature.
        wait_for "$WAIT_SECONDS" bash -c "ls $XDG_RUNTIME_DIR/hypr/*/.socket.sock"
        sig="$(basename "$(dirname "$(ls "$XDG_RUNTIME_DIR"/hypr/*/.socket.sock | head -1)")")"
        export HYPRLAND_INSTANCE_SIGNATURE="$sig"
        wsock="$(ls "$XDG_RUNTIME_DIR" | grep -m1 '^wayland-[0-9]*$')"
        export WAYLAND_DISPLAY="${wsock:-wayland-1}"
        ;;
    mutter)
        export XDG_CURRENT_DESKTOP=GNOME
        bg compositor mutter --headless --wayland --no-x11 --virtual-monitor 1920x1080 \
            --wayland-display "$WAYLAND_DISPLAY"
        ;;
    sway)
        echo 'output HEADLESS-1 resolution 1920x1080' >"$T/sway.conf"
        export WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman
        bg compositor sway -c "$T/sway.conf"
        wait_for "$WAIT_SECONDS" bash -c "ls $XDG_RUNTIME_DIR/wayland-*"
        wsock="$(ls "$XDG_RUNTIME_DIR" | grep -m1 '^wayland-[0-9]*$')"
        export WAYLAND_DISPLAY="${wsock:-wayland-1}"
        ;;
    esac
}
start_compositor
if wait_for "$WAIT_SECONDS" test -S "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY"; then
    rep compositor_up yes
else
    rep compositor_up no
    tail -30 "$OUT/compositor.log"
    exit 1
fi
sleep 3
kill -0 "$PID_compositor" 2>/dev/null && rep compositor_alive yes || rep compositor_alive no
[ -e /dev/dri/renderD128 ] && rep render_node yes || rep render_node no
if [ "$COMP" = kwin ] || [ "$COMP" = kwin-nested ]; then
    for q in qdbus-qt6 qdbus6 qdbus-qt5 qdbus; do
        command -v $q >/dev/null || continue
        $q org.kde.KWin /KWin supportInformation >"$OUT/kwin-support.txt" 2>&1
        rep kwin_compositing "$(grep -m1 'Compositing Type' "$OUT/kwin-support.txt" | sed 's/.*: //')"
        rep kwin_renderer "$(grep -m1 'OpenGL renderer string' "$OUT/kwin-support.txt" | sed 's/.*: //')"
        break
    done
fi

# What a program with no special rights sees in the registry.
if command -v wayland-info >/dev/null; then
    wayland-info >"$OUT/registry.txt" 2>&1
    for p in zkde_screencast_unstable_v1 org_kde_kwin_outputdevice kde_output_device_v2 \
        kde_output_management_v2 zwlr_screencopy_manager_v1 ext_image_copy_capture_manager_v1 \
        zwlr_output_manager_v1 zxdg_output_manager_v1 zwlr_virtual_pointer_manager_v1 \
        org_kde_kwin_fake_input; do
        grep -q "interface: '$p'" "$OUT/registry.txt" && rep "registry_$p" yes || rep "registry_$p" no
    done
fi

# A window that keeps moving, so the monitors have something new to send every frame
# (a still desktop sends almost nothing).
if [ "$COMP" != mutter ] && [ "$COMP" != sway ] && command -v gst-launch-1.0 >/dev/null; then
    bg painter gst-launch-1.0 -q videotestsrc is-live=true pattern=ball ! video/x-raw,width=640,height=360,framerate=30/1 ! videoconvert ! waylandsink
fi

# Prostir and the fake headset, who trust each other beforehand.
if [ "$COMP" = mutter ]; then
    # GNOME's way to a virtual monitor (Breezy Desktop uses it): Mutter's ScreenCast.
    python3 /inside/mutter_screencast.py 2>&1 | tee -a "$REPORT"
fi
[ "$COMP" = mutter ] || [ "$COMP" = sway ] && {
    rep prostir_run skipped_no_backend
    exit 0
}
"$BIN/prostir-trust" "$T/headhome/.config/prostir/probe" "$T/pcconf/prostir" >/dev/null 2>&1 || rep trust failed
pc_env=(env XDG_CONFIG_HOME="$T/pcconf" RUST_LOG="${RUST_LOG:-info,prostir=debug}")
"${pc_env[@]}" bash -c "exec '$BIN/prostir' serve --port 47930 --no-adb --no-discovery --stay --software -m 1920x1080" \
    >"$OUT/prostir.log" 2>&1 &
PIDS+=($!)
PID_prostir=$!
wait_for 15 bash -c "ss -ltn | grep -q 47930" && rep prostir_listening yes || rep prostir_listening no

HOME="$T/headhome" "$BIN/prostir-probe" 127.0.0.1:47930 14 "$OUT" >"$OUT/probe.log" 2>&1 &
PID_probe=$!
sleep 8
# Mid-stream: what the compositor and PipeWire have now.
case "$COMP" in
kwin|kwin-nested) kscreen-doctor -o >"$OUT/outputs.txt" 2>&1 ;;
hyprland) hyprctl monitors >"$OUT/outputs.txt" 2>&1 ;;
esac
pw-cli ls Node >"$OUT/pw-nodes.txt" 2>&1
wait "$PID_probe"
rep probe_exit $?
kill -0 "$PID_prostir" 2>/dev/null && rep prostir_alive_at_end yes || rep prostir_alive_at_end no

# The results.
grep -q "zkde_screencast_unstable_v1" "$OUT/prostir.log" && grep -m1 "zkde_screencast" "$OUT/prostir.log" | sed 's/^/prostir_log_zkde: /' >>"$REPORT"
rep outputs_with_prostir_name "$(grep -a 'Output:' "$OUT/outputs.txt" 2>/dev/null | grep -c Prostir)"
rep prostir_pw_node "$(grep -oP 'PipeWire node \K[0-9]+' "$OUT/prostir.log" | head -1)"
rep compositor_video_stream_nodes "$(grep -c 'Stream/Output/Video' "$OUT/pw-nodes.txt")"
rep prostir_video_stream_nodes "$(grep -c 'Stream/Input/Video' "$OUT/pw-nodes.txt")"
rep prostir_error "$(grep -m1 'ERROR' "$OUT/prostir.log" | sed 's/^[^ ]* ERROR //')"
if [ -s "$OUT/display-0.h264" ]; then
    gst-launch-1.0 -q filesrc location="$OUT/display-0.h264" ! h264parse ! fakesink >/dev/null 2>&1 \
        && rep h264_parses yes || rep h264_parses no
fi
rep probe_connected "$(grep -c '^connected' "$OUT/probe.log")"
rep probe_stream_lines "$(grep '^stream ' "$OUT/probe.log" | tr '\n' ';')"
frames="$(grep -oP '^stream \d+: \K\d+(?= frames)' "$OUT/probe.log" | head -1)"
rep frames "${frames:-0}"
if [ "${frames:-0}" -gt 0 ]; then rep RESULT pass; else rep RESULT fail; fi
