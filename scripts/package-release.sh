#!/usr/bin/env bash
# Builds release artifacts into dist/:
#   prostir-<version>-x86_64-linux.tar.gz   PC part with install.sh
#   prostir-<version>.apk                   headset app
#   THIRD-PARTY-LICENSES.txt                licenses of everything bundled in both
# Signing of the APK: see scripts/build-apk.sh (KEYSTORE…), debug key otherwise.
# The .deb and .rpm are made afterwards from target/release/prostir (release
# workflow). The release profile strips it.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
# The same archives from the same commit: every file dated by the commit (the programs
# themselves embed no time).
SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct)}"
export SOURCE_DATE_EPOCH
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
DIST="$ROOT/dist"
NAME="prostir-$VERSION-x86_64-linux"
rm -rf "$DIST/$NAME" && mkdir -p "$DIST/$NAME"

# The APK first: build-apk.sh also writes build/THIRD-PARTY-LICENSES.txt, which every
# package below ships.
PROFILE=release ./scripts/build-apk.sh
cp build/prostir.apk "$DIST/prostir-$VERSION.apk"
cp build/THIRD-PARTY-LICENSES.txt "$DIST/"

cargo build --release --locked -p prostir --bin prostir
# Where cargo builds: its own CARGO_TARGET_DIR when set, else target/ here.
TARGET_DIR="${CARGO_TARGET_DIR:-target}"
cp "$TARGET_DIR/release/prostir" "$DIST/$NAME/"
mkdir -p "$DIST/$NAME/packaging"
cp packaging/prostir.svg packaging/io.github.prostir_app.Prostir.desktop "$DIST/$NAME/packaging/"
cp -r packaging/systemd packaging/udev "$DIST/$NAME/packaging/"
cp scripts/install.sh LICENSE LICENSE-EXCEPTION.md build/THIRD-PARTY-LICENSES.txt \
    README.md README.uk.md CHANGELOG.md "$DIST/$NAME/"
# The modes too: the same whatever the umask of the machine that made the files.
tar --sort=name --owner=0 --group=0 --numeric-owner --mtime="@$SOURCE_DATE_EPOCH" \
    --mode=u+rw,go-w,a+rX \
    -C "$DIST" -cf - "$NAME" | gzip -9n > "$DIST/$NAME.tar.gz"
rm -rf "$DIST/$NAME"

(cd "$DIST" && sha256sum "$NAME.tar.gz" "prostir-$VERSION.apk" THIRD-PARTY-LICENSES.txt > SHA256SUMS)
echo ">> $DIST"
ls -la "$DIST"
