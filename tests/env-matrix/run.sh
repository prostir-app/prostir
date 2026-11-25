#!/usr/bin/env bash
# Runs one step of the environment matrix in a rootless podman container.
#
#   tests/env-matrix/run.sh image  NAME                 build the image images/NAME.Containerfile
#   tests/env-matrix/run.sh build  NAME                 build Prostir (release) inside it
#   tests/env-matrix/run.sh check  NAME COMPOSITOR [MODE] [BUILD-NAME]
#                                                       run the compositor headless, Prostir and
#                                                       a fake headset; write a report
#   tests/env-matrix/run.sh shell  NAME                 a shell in the image (with the same mounts)
#
# COMPOSITOR: kwin | hyprland | mutter | sway
# MODE (KWin only): desktopfile (default) | plain | none | nocheck
#   desktopfile  the desktop file of Prostir is in the service cache (KWin <= 6.7 rule)
#   plain        the shipped desktop file, only Exec made absolute (no X-KDE-Wayland-Interfaces)
#   none         no desktop file
#   nocheck      KWIN_WAYLAND_NO_PERMISSION_CHECKS=1
# BUILD-NAME: run the binaries built in another image (the glibc floor test).
#
# Nothing of the host is shared except the source (read-only), a cargo cache, the
# Rust toolchain (read-only), the target directory and the report directory.
# The check runs with no network and its own D-Bus, PipeWire and runtime directory.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
SRC="${ENVMATRIX_SRC:-$ROOT}"
HERE="$ROOT/tests/env-matrix"
CACHE="${ENVMATRIX_CACHE:-$HOME/.cache/prostir-build}"
TARGETS="$CACHE/target-envmatrix"
CARGO="$CACHE/cargo-envmatrix"
OUT="${ENVMATRIX_OUT:-$CACHE/envmatrix-out}"
CHANNEL="$(grep -oP 'channel = "\K[^"]+' "$ROOT/rust-toolchain.toml")"
TOOLCHAIN="${RUST_TOOLCHAIN_DIR:-$HOME/.rustup/toolchains/$CHANNEL-x86_64-unknown-linux-gnu}"

step="${1:?usage: run.sh image|build|check|shell NAME ...}"
name="${2:?image name}"
image="localhost/prostir-em-$name"
mkdir -p "$TARGETS/${ENVMATRIX_TARGET:-$name}" "$CARGO" "$OUT"

common=(--rm --security-opt label=disable
    -v "$SRC:/src:ro" -v "$CARGO:/cargo" -v "$TOOLCHAIN:/rust:ro"
    -v "$TARGETS/${ENVMATRIX_TARGET:-$name}:/target")

case "$step" in
image)
    nice -n 10 podman build -t "$image" -f "$HERE/images/$name.Containerfile" "$HERE/images"
    ;;
build)
    nice -n 10 podman run "${common[@]}" --memory 14g \
        -v "$HERE/inside:/inside:ro" "$image" bash /inside/build.sh
    ;;
check)
    comp="${3:?compositor}"
    mode="${4:-desktopfile}"
    built="${5:-$name}"
    dest="$OUT/$name-$comp-$mode"
    [ "$built" = "$name" ] || dest="$dest-built-on-$built"
    rm -rf "$dest"
    mkdir -p "$dest"
    nice -n 10 podman run --rm --security-opt label=disable --network=none --memory 6g ${ENVMATRIX_RENDER_NODE:+--device /dev/dri/renderD128} \
        -v "$SRC:/src:ro" -v "$HERE/inside:/inside:ro" -v "$TARGETS/$built:/target:ro" -v "$dest:/out" \
        -e "BIN=/target/release" ${EM_SOFTWARE_GL:+-e LIBGL_ALWAYS_SOFTWARE=1 -e GALLIUM_DRIVER=llvmpipe -e MESA_LOADER_DRIVER_OVERRIDE=kms_swrast} "$image" bash /inside/check.sh "$comp" "$mode" || true
    echo "report: $dest/report.txt"
    ;;
shell)
    nice -n 10 podman run -it "${common[@]}" -v "$HERE/inside:/inside:ro" "$image" bash
    ;;
*)
    echo "unknown step $step" >&2
    exit 2
    ;;
esac
