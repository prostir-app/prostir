# Environment matrix

Runs a real compositor with no screen inside a rootless podman container, then
Prostir (real desktop backend, x264 encoder) and the fake headset from
`prostir-testkit`, and checks that a virtual monitor appears and an encoded
frame arrives. The results are in `plans/env-matrix-2026-11.md`.

Needs only `podman` (rootless) and a Rust toolchain in `~/.rustup` (mounted
read-only into the containers; the version comes from `rust-toolchain.toml`).
Nothing of the host session is shared: no Wayland or PipeWire socket, no GPU,
no network during a check. Each check starts its own D-Bus, PipeWire,
WirePlumber and compositor.

```sh
tests/env-matrix/run.sh image fedora43                  # build the image (images/*.Containerfile)
tests/env-matrix/run.sh build fedora43                  # release build inside it (own glibc)
tests/env-matrix/run.sh check fedora43 kwin             # KWin, desktop file in the cache (<= 6.7 rule)
tests/env-matrix/run.sh check fedora43 kwin none        # KWin, no desktop file
tests/env-matrix/run.sh check fedora43 kwin nocheck     # KWIN_WAYLAND_NO_PERMISSION_CHECKS=1
tests/env-matrix/run.sh check ubuntu2404 kwin desktopfile debian12   # run a binary built elsewhere (glibc floor)
```

KWin 6 needs a DRM render node even with no screen (without one it composes with QPainter
and refuses screencasts), so give it one: `ENVMATRIX_RENDER_NODE=1` passes the host's
`/dev/dri/renderD128` into the check container, and `EM_SOFTWARE_GL=1` makes Mesa draw in
software on it (the host GPU does no drawing). Other knobs: `ENVMATRIX_SRC` (build another
source tree, mounted at `/src`), `ENVMATRIX_TARGET` (another target directory name for a
build). Compositors: `kwin`, `kwin-nested` (KWin inside a KWin, for versions whose `--virtual`
cannot make outputs on request), `hyprland`, `mutter`, `sway`; Mutter and Sway are only
recorded (Prostir has no backend for them).

A check writes `report.txt` (key=value lines: versions, registry, `RESULT=pass|fail`)
and the logs of every process into `~/.cache/prostir-build/envmatrix-out/NAME-COMPOSITOR-MODE/`.
Build output goes to `~/.cache/prostir-build/target-envmatrix/NAME` (delete it
when done); `ENVMATRIX_CACHE` moves both.

`inside/` holds what runs in the container: `build.sh`, `check.sh`.
