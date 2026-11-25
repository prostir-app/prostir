#!/usr/bin/env bash
# Inside the container: builds the PC app and the fake headset (release), so the
# binaries link against this container's glibc.
set -euo pipefail
export PATH=/rust/bin:$PATH CARGO_HOME=/cargo CARGO_TARGET_DIR=/target
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-12}"
cd /src
{
    echo "rustc: $(rustc --version)"
    echo "glibc: $(ldd --version | head -1)"
    echo "gstreamer-dev: $(pkg-config --modversion gstreamer-1.0 2>&1)"
} | tee /target/build-info.txt
cargo build --release --locked -p prostir -p prostir-testkit --bins 2>&1 | tail -40
ls -l /target/release/prostir /target/release/prostir-probe /target/release/prostir-trust
objdump -T /target/release/prostir | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1 | sed 's/^/highest glibc symbol needed: /' | tee -a /target/build-info.txt
