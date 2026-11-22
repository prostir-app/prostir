#!/usr/bin/env bash
# Installs the Prostir PC program for the current user, without root:
#   ~/.local/bin/prostir, the icon, the application menu entry and the systemd
#   user units (Prostir's first start enables them).
#
#   ./install.sh                       install (or update: run it again)
#   ./install.sh --autostart           start Prostir when you log in now, not at its first start
#   ./install.sh --uninstall           remove everything this script installed
#   ./install.sh --uninstall --purge   also remove settings and pairings (~/.config/prostir)
#
# In a source checkout it builds the program first (cargo build --release).
set -euo pipefail

usage() { sed -n '2,11s/^# \{0,1\}//p' "$0"; }

autostart=0 uninstall=0 purge=0
for arg in "$@"; do
    case "$arg" in
        --autostart) autostart=1 ;;
        --uninstall) uninstall=1 ;;
        --purge) purge=1 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "Unknown option: $arg" >&2; usage >&2; exit 2 ;;
    esac
done
if (( purge && !uninstall )); then
    echo "--purge only works together with --uninstall." >&2
    exit 2
fi

HERE="$(cd "$(dirname "$0")" && pwd)"
# Release archive: install.sh sits next to the binary. Source tree: in scripts/.
if [[ -x "$HERE/prostir" ]]; then ROOT="$HERE"; else ROOT="$(cd "$HERE/.." && pwd)"; fi
BIN="$HOME/.local/bin"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
CONFIG="${XDG_CONFIG_HOME:-$HOME/.config}"
ICON="$DATA/icons/hicolor/scalable/apps/prostir.svg"
APP_ID="io.github.prostir_app.Prostir"
DESKTOP="$DATA/applications/$APP_ID.desktop"
# What earlier versions installed: the menu entry of 0.1, the settings window's own
# program and entry.
OLD=("$DATA/applications/prostir.desktop" "$DATA/applications/$APP_ID.Settings.desktop"
     "$BIN/prostir-settings")
AUTOSTART="$CONFIG/autostart/prostir.desktop"
UNITS="$DATA/systemd/user"
DOCS="$DATA/doc/prostir"
SETTINGS="$CONFIG/prostir"

refresh_menu() {
    if command -v update-desktop-database >/dev/null; then
        update-desktop-database "$DATA/applications" 2>/dev/null || true
    fi
}

# The text $1 as the replacement of a sed `s|...|...|`: its `\`, `&` and `|` escaped.
sed_text() { printf '%s' "$1" | sed -e 's/[\\&|]/\\&/g'; }

# A program's path as one argument of a desktop entry's Exec line: quoted where it has
# a character the line would read otherwise, escaped for the quoting and then for the
# string value; `%` doubled.
desktop_arg() {
    local s=${1//%/%%}
    if [[ $s =~ ^[A-Za-z0-9/._+-]+$ ]]; then printf '%s' "$s"; return; fi
    s=${s//\\/\\\\\\\\}
    s=${s//\"/\\\\\"}
    s=${s//\`/\\\\\`}
    s=${s//\$/\\\\\$}
    printf '"%s"' "$s"
}

# A program's path as one argument of a systemd unit's ExecStart (quoted where needed,
# `%` and `$` doubled).
unit_arg() {
    local s=${1//%/%%}
    s=${s//\$/\$\$}
    if [[ $s =~ ^[A-Za-z0-9/._+-]+$ ]]; then printf '%s' "$s"; return; fi
    s=${s//\\/\\\\}
    s=${s//\"/\\\"}
    printf '"%s"' "$s"
}

# The user's systemd runs (its private socket is there).
user_systemd() {
    [[ -S "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/systemd/private" ]] && command -v systemctl >/dev/null
}

hyprland_lines() {
    echo "   On Hyprland, start it with the session by adding one line to your config:"
    echo "     hyprland.lua:   hl.on(\"hyprland.start\", function () hl.exec_cmd(\"$BIN/prostir serve\") end)"
    echo "     hyprland.conf:  exec-once = $BIN/prostir serve"
    echo "   With ecosystem:enforce_permissions on, also allow screen capture:"
    echo "     hyprland.lua:   hl.permission(\"$BIN/prostir\", \"screencopy\", \"allow\")"
    echo "     hyprland.conf:  permission = $BIN/prostir, screencopy, allow"
}

if (( uninstall )); then
    if pgrep -x -u "$(id -u)" prostir >/dev/null 2>&1; then
        echo "Note: Prostir is still running. Quit it from the tray; removing it does not stop it."
    fi
    if user_systemd && [[ -f "$UNITS/prostir.socket" ]]; then
        systemctl --user disable --now prostir.socket 2>/dev/null || true
    fi
    rm -f "$BIN/prostir" "$ICON" "$DESKTOP" "$AUTOSTART" "${OLD[@]}"
    rm -f "$UNITS/prostir.socket" "$UNITS/prostir.service"
    if user_systemd; then systemctl --user daemon-reload || true; fi
    rm -rf "$DOCS"
    refresh_menu
    echo ">> Prostir removed from $BIN, the application menu and autostart."
    if (( purge )); then
        rm -rf "$SETTINGS"
        echo "   Settings and paired headsets removed ($SETTINGS)."
    elif [[ -d "$SETTINGS" ]]; then
        echo "   Settings and paired headsets are kept in $SETTINGS (--uninstall --purge removes them)."
    fi
    if [[ -n "${HYPRLAND_INSTANCE_SIGNATURE:-}" ]]; then
        echo "   If you added Prostir to your Hyprland config, remove those lines too."
    fi
    echo "   The headset app stays on the headset. To remove it: adb uninstall dev.prostir.client"
    exit 0
fi

if [[ -x "$ROOT/prostir" ]]; then
    built="$ROOT"
else
    echo ">> building"
    (cd "$ROOT" && cargo build --release --locked -p prostir --bin prostir)
    built="$ROOT/target/release"
fi
assets="$ROOT/packaging"

rm -f "${OLD[@]}"
install -Dm755 "$built/prostir" "$BIN/prostir"
install -Dm644 "$assets/prostir.svg" "$ICON"
install -Dm644 "$assets/$APP_ID.desktop" "$DESKTOP"
sed -i "s|^Exec=.*|Exec=$(sed_text "$(desktop_arg "$BIN/prostir")")|" "$DESKTOP"
# Archives made before Prostir could sleep until the headset comes have no units.
if [[ -f "$assets/systemd/prostir.socket" ]]; then
    install -Dm644 "$assets/systemd/prostir.socket" "$UNITS/prostir.socket"
    install -Dm644 "$assets/systemd/prostir.service" "$UNITS/prostir.service"
    sed -i "s|^ExecStart=.*|ExecStart=$(sed_text "$(unit_arg "$BIN/prostir") serve")|" "$UNITS/prostir.service"
    if user_systemd; then systemctl --user daemon-reload || true; fi
fi
for f in LICENSE LICENSE-EXCEPTION.md THIRD-PARTY-LICENSES.txt; do
    if [[ -f "$ROOT/$f" ]]; then install -Dm644 "$ROOT/$f" "$DOCS/$f"; fi
done
refresh_menu

echo ">> installed $BIN/prostir"
echo "   Start it from the application menu (Prostir) or run: $BIN/prostir"
case ":$PATH:" in
    *":$BIN:"*) ;;
    *) echo "   Note: $BIN is not on your PATH, so the bare command \"prostir\" will not be found."
       echo "   Use the full path above, or add $BIN to PATH in your shell profile." ;;
esac

# Up to 0.1 a bare `prostir` started the background part; now it opens the window. An
# autostart entry of then runs `prostir serve` from now on.
if [[ -f "$AUTOSTART" ]] && ! grep -qE '^Exec=.* serve( |$)' "$AUTOSTART"; then
    sed -i "s|^Exec=.*|Exec=$(sed_text "$(desktop_arg "$BIN/prostir") serve")|" "$AUTOSTART"
    echo "   The autostart entry now runs \"prostir serve\" ($AUTOSTART)."
fi
# Hyprland's config is the user's own: what to change is only said.
HYPR="$CONFIG/hypr"
if [[ -d "$HYPR" ]]; then
    old_lines="$(grep -rnE --include='*.conf' --include='*.lua' \
        '(exec-once *=.*|exec_cmd\(["'\''])[^"'\''#]*prostir *(["'\'']|$)' "$HYPR" 2>/dev/null || true)"
    if [[ -n "$old_lines" ]]; then
        echo "   Your Hyprland config starts \"prostir\" without \"serve\", which now opens the window at login:"
        printf '     %s\n' "${old_lines//$'\n'/$'\n'     }"
        echo "   Change it to \"prostir serve\"."
    fi
fi

if [[ -n "${HYPRLAND_INSTANCE_SIGNATURE:-}" ]]; then
    # Hyprland ignores XDG autostart files unless it runs under uwsm, so print the
    # config lines instead of writing one.
    hyprland_lines
elif (( autostart )) && user_systemd && [[ -f "$UNITS/prostir.socket" ]]; then
    # Nothing runs until the headset comes: systemd waits for it.
    rm -f "$AUTOSTART"
    systemctl --user enable prostir.socket
    echo "   From the next login it starts when the headset comes. Turn that off in the tray: \"Start with the system\"."
elif (( autostart )); then
    mkdir -p "$(dirname "$AUTOSTART")"
    printf '%s\n' "[Desktop Entry]" "Type=Application" "Name=Prostir" \
        "Comment=Virtual monitors for Meta Quest" \
        "Exec=$(desktop_arg "$BIN/prostir") serve" "Icon=prostir" "Terminal=false" \
        "X-KDE-autostart-phase=2" > "$AUTOSTART"
    echo "   It starts when you log in. Turn that off in the tray: \"Start with the system\"."
elif [[ -f "$AUTOSTART" ]] || { user_systemd && systemctl --user -q is-enabled prostir.socket 2>/dev/null; }; then
    echo "   It starts when you log in (tray: \"Start with the system\")."
else
    echo "   Its first start sets it to start when you log in (turn that off in the tray: \"Start with the system\");"
    echo "   --autostart does it now."
fi
