#!/usr/bin/env bash
# Inside an env-matrix container: a compositor with no screen, whether it offers
# pointer sessions (ext-image-copy-capture), and, with the unit tests of the PC app
# mounted at /t/prostir-test, the pointer session of `hyprland/pointer.rs` against it
# while a pointer moves (what it tells, and whether the frames would leave it out).
#   cursor.sh hyprland|sway
set -u
COMP="$1"
if [ "${EM_INNER:-}" != 1 ]; then
    export EM_INNER=1
    exec dbus-run-session -- bash "$0" "$@"
fi
T=/tmp/em
mkdir -p "$T/run" "$T/home"
chmod 700 "$T/run"
export XDG_RUNTIME_DIR="$T/run" HOME="$T/home" XDG_SESSION_TYPE=wayland
case "$COMP" in
hyprland)
    echo "version: $(Hyprland --version | head -1)"
    printf 'misc {\n disable_hyprland_logo = true\n disable_splash_rendering = true\n}\n' >"$T/hypr.conf"
    export WLR_BACKENDS=headless WLR_RENDERER_ALLOW_SOFTWARE=1 WLR_NO_HARDWARE_CURSORS=1
    Hyprland --i-am-really-stupid -c "$T/hypr.conf" >"$T/comp.log" 2>&1 &
    for _ in $(seq 100); do ls "$XDG_RUNTIME_DIR"/hypr/*/.socket.sock >/dev/null 2>&1 && break; sleep 0.2; done
    sig="$(basename "$(dirname "$(ls "$XDG_RUNTIME_DIR"/hypr/*/.socket.sock | head -1)")")"
    export HYPRLAND_INSTANCE_SIGNATURE="$sig"
    sleep 2
    export WAYLAND_DISPLAY="$(ls "$XDG_RUNTIME_DIR" | grep -m1 '^wayland-[0-9]*$')"
    hyprctl monitors | head -3
    out="$(hyprctl -j monitors | grep -m1 '"name"' | sed 's/.*: "\(.*\)".*/\1/')"
    ;;
sway)
    echo "version: $(sway --version)"
    printf 'output HEADLESS-1 resolution 1920x1080 position 0 0\noutput HEADLESS-2 resolution 1920x1080 position 1920 0\n' >"$T/sway.conf"
    export WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman
    sway -c "$T/sway.conf" >"$T/comp.log" 2>&1 &
    for _ in $(seq 100); do ls "$XDG_RUNTIME_DIR"/wayland-* >/dev/null 2>&1 && break; sleep 0.2; done
    sleep 2
    export WAYLAND_DISPLAY="$(ls "$XDG_RUNTIME_DIR" | grep -m1 '^wayland-[0-9]*$')"
    # A second output to the right, for the pointer to go over the edge.
    export SWAYSOCK="$(ls "$XDG_RUNTIME_DIR"/sway-ipc.*.sock | head -1)"
    swaymsg create_output >/dev/null
    sleep 1
    out=HEADLESS-1,HEADLESS-2
    ;;
esac
wayland-info >"$T/registry.txt" 2>&1
for p in zwlr_screencopy_manager_v1 ext_image_copy_capture_manager_v1 \
    ext_output_image_capture_source_manager_v1 wl_seat; do
    grep -q "interface: '$p'" "$T/registry.txt" && echo "registry_$p=yes" || echo "registry_$p=no"
done
grep -A3 "interface: 'wl_seat'" "$T/registry.txt" | grep -i capab
if [ -x /t/prostir-test ]; then
    [ "$COMP" = sway ] && export PROSTIR_POINTER_MOVE=1
    (
        sleep 2
        for i in $(seq 1 20); do
            case "$COMP" in
            hyprland) hyprctl dispatch movecursor $((100 + i * 20)) 200 >/dev/null ;;
            sway) : ;;
            esac
            sleep 0.2
        done
    ) &
    PROSTIR_POINTER_OUTPUTS="$out" timeout 30 /t/prostir-test --ignored --nocapture --exact \
        platform::linux::hyprland::pointer::tests::pointer_session_live 2>&1 | grep -v "^running\|^$" | head -60
fi
tail -5 "$T/comp.log"
