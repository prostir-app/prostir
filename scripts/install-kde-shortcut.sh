#!/usr/bin/env bash
# For running Prostir from a build folder (target/release/prostir) on KDE Plasma 6.0 to
# 6.7: installs a hidden application shortcut for that program in ~/.local/share/applications
# and refreshes KDE's application cache. Up to Plasma 6.7, KWin gives a program its
# screencast interface only when a shortcut names the program's real file; Plasma 6.8
# and newer need none, and a Prostir installed by a package or by scripts/install.sh
# already has one.
#
#   scripts/install-kde-shortcut.sh                         for target/release/prostir
#   scripts/install-kde-shortcut.sh target/debug/prostir    for another build
#   scripts/install-kde-shortcut.sh --remove                remove the shortcut
#
# One shortcut at a time: running it for another path replaces the first. Start Prostir
# again afterwards. If KWin still refuses, log out and in once (KWin may hold an old
# cache); `kwin_wayland --version` and the "kwin_core" log lines say more. As a last
# resort for development only, KWIN_WAYLAND_NO_PERMISSION_CHECKS=1 in the environment of
# kwin_wayland (for example in ~/.config/plasma-workspace/env/prostir.sh, then log in
# again) lets every program use the interface: it turns the check off for all of them.
set -euo pipefail

usage() { sed -n '2,19s/^# \{0,1\}//p' "$0"; }

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
APP_ID="io.github.prostir_app.Prostir"
SHORTCUT="$DATA/applications/$APP_ID.Dev.desktop"
TEMPLATE="$ROOT/packaging/$APP_ID.desktop"

remove=0 program=""
for arg in "$@"; do
    case "$arg" in
        --remove) remove=1 ;;
        -h|--help) usage; exit 0 ;;
        -*) echo "Unknown option: $arg" >&2; usage >&2; exit 2 ;;
        *) program="$arg" ;;
    esac
done

# The cache file's name depends on the language and on XDG_DATA_DIRS, which a terminal
# may have other than the session (and so KWin): the old files go, so that the one built
# here is the only one, and KWin, which builds a missing one itself, reads this.
refresh() {
    rm -f "${XDG_CACHE_HOME:-$HOME/.cache}"/ksycoca6_*
    if command -v kbuildsycoca6 >/dev/null; then
        kbuildsycoca6 --noincremental >/dev/null 2>&1 || echo "kbuildsycoca6 failed: log out and in to refresh the cache." >&2
    else
        echo "kbuildsycoca6 not found: log out and in to refresh the cache." >&2
    fi
}

if (( remove )); then
    rm -f "$SHORTCUT"
    refresh
    echo "Removed $SHORTCUT"
    exit 0
fi

program="${program:-$ROOT/target/release/prostir}"
if [[ ! -x "$program" ]]; then
    echo "$program is not a program: build it first (cargo build --release -p prostir) or give its path." >&2
    exit 1
fi
if [[ ! -f "$TEMPLATE" ]]; then
    echo "$TEMPLATE is missing: run this script from a source checkout." >&2
    exit 1
fi
# KWin compares the file the process runs (/proc/<pid>/exe), which has no symbolic links.
exe="$(readlink -f "$program")"
# The path goes into a line of the entry and a command others copy: no character that
# either would read otherwise.
case "$exe" in
    *[\$\`\"\\%\']*)
        echo "$exe has a character of \$ \` \" \\ % ' in it: move or link the build to a plain path." >&2
        exit 1 ;;
esac

# The program's path as one argument of the entry's Exec line: quoted where it has a
# character the line would read otherwise, escaped for the quoting and then for the
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
# The text $1 as the replacement of a sed `s|...|...|`: its `\`, `&` and `|` escaped.
sed_text() { printf '%s' "$1" | sed -e 's/[\\&|]/\\&/g'; }

# The packaged entry, its Exec on this build; hidden, and without the window class, so
# it neither shows in the menu nor takes the settings window from the installed entry.
install -Dm644 "$TEMPLATE" "$SHORTCUT"
sed -i -e "s|^Exec=.*|Exec=$(sed_text "$(desktop_arg "$exe")")|" -e '/^StartupWMClass=/d' \
    -e '/^\[Desktop Entry\]/a NoDisplay=true' "$SHORTCUT"
refresh

echo "Installed $SHORTCUT"
echo "  for $exe"
echo "Start Prostir again. Remove it with: scripts/install-kde-shortcut.sh --remove"
