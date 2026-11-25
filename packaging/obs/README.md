# Open Build Service packaging

The openSUSE Build Service (OBS) builds Prostir's PC part as rpm and deb packages and
publishes them as signed repositories, so users get updates with their system. Project:
[home:iShawyha:prostir](https://build.opensuse.org/project/show/home:iShawyha:prostir),
package `prostir`, download root
`https://download.opensuse.org/repositories/home:/iShawyha:/prostir/`.

## How a release gets there

1. A release is published on GitHub (or the OBS workflow is started by hand with a tag).
2. `.github/workflows/obs.yml` runs `make-sources.sh` for the tag and attaches four
   files to the GitHub release:

   | Release asset | Name on OBS | What it is |
   |---|---|---|
   | `prostir-vendored.tar.gz` | same | the tagged source, every crate from `Cargo.lock` in `vendor/`, and the release's `THIRD-PARTY-LICENSES.txt`; unpacks to `prostir-<version>/` |
   | `prostir-obs.spec` | `prostir.spec` | `prostir.spec` with the version filled in |
   | `prostir-obs.dsc` | `prostir.dsc` | `prostir.dsc` with the version and the build dependencies of `debian/control` |
   | `prostir-obs-debian.tar.gz` | `debian.tar.gz` | the `debian/` folder with the version in `debian/changelog` |

3. The workflow calls `https://api.opensuse.org/trigger/runservice` with the token in
   the `OBS_TOKEN` secret.
4. OBS runs `_service` in the package: `download_url` fetches the four files from
   `https://github.com/prostir-app/prostir/releases/latest/download/`, and every
   repository rebuilds. The rpm repositories build `prostir.spec`; the deb ones build
   `prostir.dsc`, which OBS's debtransform turns into a Debian source package from
   `prostir-vendored.tar.gz` and `debian.tar.gz`. `DEBTRANSFORM-RELEASE` adds the OBS
   build number to the Debian version, so every rebuild is an upgrade.

The builds run without network. Cargo uses only `vendor/` (`--frozen` and a source
replacement given with `--config`), in both `prostir.spec` and `debian/rules`.

### Why it is built this way

- **Vendoring in the workflow, not on OBS.** The openSUSE services (`cargo_vendor` from
  obs-service-cargo) make a vendor tarball that the rpm spec can use, but Debian builds
  on OBS only see the `.dsc`, the orig tarball and `debian.tar.*`, so a separate vendor
  tarball would not reach them. One tarball with `vendor/` inside works for both. It is
  made by `cargo vendor --locked` on the GitHub runner, from the tag's `Cargo.lock`,
  and is reproducible: the same tag gives the same bytes. It can also be checked and
  built locally, as below.
- **A runservice token, not osc commit.** OBS tokens come in four kinds: rebuild,
  release, runservice and workflow. None of them can commit files, and
  `api.opensuse.org` takes only the account password for that. A runservice token bound
  to this one package can only make OBS re-run its `_service`, which downloads from the
  latest GitHub release, so a leaked token cannot change what gets built. The SCM/CI
  workflow integration (webhook plus `.obs/workflows.yml`) branches a new package per
  tag instead of updating this one, and would need a GitHub token stored on OBS.
- **The latest release only.** `_service` downloads from `releases/latest`, so the
  workflow refuses a tag that is not GitHub's latest release, rather than publishing
  older sources than it says.

## Repositories

The project needs these repositories, each with the architectures listed. aarch64 is
experimental: it builds, but nobody has tried it on ARM hardware yet. The path matters:
Fedora and Ubuntu need their update repositories, because the Rust of their first
release is too old (see below).

| Repository name | Path (project / repository) | Architectures | Rust there |
|---|---|---|---|
| `openSUSE_Tumbleweed` | openSUSE:Factory / snapshot | x86_64 | 1.99 |
| `openSUSE_Factory_ARM` | openSUSE:Factory:ARM / standard | aarch64 | 1.99 |
| `openSUSE_Slowroll` | openSUSE:Slowroll / standard | x86_64 | 1.99 |
| `16.0` | openSUSE:Leap:16.0 / standard | x86_64, aarch64 | 1.98 |
| `Fedora_45` | Fedora:45 / **update** | x86_64, aarch64 | 1.99 (1.98 at release) |
| `Fedora_44` | Fedora:44 / **update** | x86_64, aarch64 | 1.99 (1.94 at release: too old) |
| `Debian_13` | Debian:13 / update | x86_64, aarch64 | `rustc` 1.85, `rustc-web` 1.96 |
| `xUbuntu_26.04` | Ubuntu:26.04 / **universe-update** | x86_64, aarch64 | `rustc` 1.93, `rustc-1.97` in updates |
| `xUbuntu_26.10` | Ubuntu:26.10 / **universe-update** | x86_64, aarch64 | see the note on 26.10 below |

The same as project meta (Meta tab of the project), to paste in place of its
`<repository>` elements:

```xml
  <repository name="openSUSE_Tumbleweed">
    <path project="openSUSE:Factory" repository="snapshot"/>
    <arch>x86_64</arch>
  </repository>
  <repository name="openSUSE_Factory_ARM">
    <path project="openSUSE:Factory:ARM" repository="standard"/>
    <arch>aarch64</arch>
  </repository>
  <repository name="openSUSE_Slowroll">
    <path project="openSUSE:Slowroll" repository="standard"/>
    <arch>x86_64</arch>
  </repository>
  <repository name="16.0">
    <path project="openSUSE:Leap:16.0" repository="standard"/>
    <arch>x86_64</arch>
    <arch>aarch64</arch>
  </repository>
  <repository name="Fedora_45">
    <path project="Fedora:45" repository="update"/>
    <arch>x86_64</arch>
    <arch>aarch64</arch>
  </repository>
  <repository name="Fedora_44">
    <path project="Fedora:44" repository="update"/>
    <arch>x86_64</arch>
    <arch>aarch64</arch>
  </repository>
  <repository name="Debian_13">
    <path project="Debian:13" repository="update"/>
    <arch>x86_64</arch>
    <arch>aarch64</arch>
  </repository>
  <repository name="xUbuntu_26.04">
    <path project="Ubuntu:26.04" repository="universe-update"/>
    <arch>x86_64</arch>
    <arch>aarch64</arch>
  </repository>
  <repository name="xUbuntu_26.10">
    <path project="Ubuntu:26.10" repository="universe-update"/>
    <arch>x86_64</arch>
    <arch>aarch64</arch>
  </repository>
```

### Rust

`rust-version` in `Cargo.toml` is 1.96, because the crates in `Cargo.lock` need it:
for v0.1.3, the gstreamer crates need 1.92 and `kstring` 2.0.5 (a gstreamer
dependency) needs 1.96. `make-sources.sh` therefore works out the newest
`rust-version` among the crates the PC part is built from and writes it into the
build dependencies (`cargo >= 1.96` for v0.1.3). A distribution with an older Rust then
shows "unresolvable" on OBS with the reason, instead of failing halfway through the
compile. Nothing pins an old Rust, and no build downloads a toolchain.

- **openSUSE**: Tumbleweed and Slowroll have the newest Rust. Leap 16.0 has `rust`
  and `cargo` meta packages that follow the newest Rust SUSE ships for SLES 16.
- **Fedora**: Rust is updated in each release's updates. Fedora 44 came with 1.94, so
  its repository must use `Fedora:44 / update`, not `standard`.
- **Debian 13**: `rustc` and `cargo` are 1.85. The main archive also has `rustc-web`
  and `cargo-web` 1.96 (kept current for Firefox and Chromium), which provide
  `cargo (= 1.96)`, so `cargo (>= 1.96)` installs them. trixie-backports has
  `rustc` 1.96 as well. When a future release needs a newer Rust than `cargo-web`
  has, Debian 13 builds stop as unresolvable until Debian updates it.
- **Ubuntu**: 26.04 has `rustc` 1.93, but its updates carry versioned toolchains
  (`cargo-1.97` and others, installed in `/usr/lib/rust-1.NN/bin`). The Debian
  build dependencies accept `cargo (>= N)` or `cargo-1.NN` from N up to N+8, newest
  first, and `debian/rules` puts the newest `/usr/lib/rust-1.*/bin` first on `PATH`.
  This needs the `universe-update` path. Ubuntu 26.10 has `rustc` 1.97.

Notes on the distributions (November 2026):

- Only distributions with KDE Plasma 6 or Hyprland. Debian 12 and Ubuntu 24.04 have
  Plasma 5. Ubuntu 25.10 reached its end of life in July 2026, so it is left out.
- Fedora 45 came out in late October 2026 and `Fedora:45` exists on OBS, but the web
  UI's list of distributions still stops at Fedora 44. Add it through the project meta
  above, or Repositories > Add from a project. Fedora 43 is supported until about
  December 2026; a `Fedora_43` repository (`Fedora:43 / update`) can stay until then.
- OBS's `Ubuntu:26.10` project currently downloads from `resolute` (26.04) instead
  of `stonking` (26.10), so `xUbuntu_26.10` builds are really 26.04 builds, with
  26.04's versioned toolchain. They install on 26.10 all the same. Once OBS fixes
  the project, nothing here needs to change.
- `openSUSE_Tumbleweed` and `openSUSE_Slowroll` have no aarch64 on OBS. Tumbleweed for
  ARM is the separate `openSUSE:Factory:ARM` repository ("openSUSE Factory 64bit ARM"
  in the list of distributions); Slowroll has no ARM port.

## Dependencies

| | openSUSE | Fedora | Debian, Ubuntu |
|---|---|---|---|
| Build | `cargo`, `rust`, `gcc`, `pkgconfig(gbm)`, `pkgconfig(gstreamer-1.0)`, `-app-`, `-video-`, `-allocators-`, `pkgconfig(libpipewire-0.3)`, `clang-devel` (spec) | same names as openSUSE | `cargo` or `cargo-1.NN`, `pkgconf`, `libgbm-dev`, `libgstreamer1.0-dev`, `libgstreamer-plugins-base1.0-dev`, `libpipewire-0.3-dev`, `libclang-dev` |
| VA encoders, h264parse, h265parse | `gstreamer-plugins-bad` | `gstreamer1-plugins-bad-free` | `gstreamer1.0-plugins-bad`; the va plugin is in `gstreamer1.0-plugins-extra` on Ubuntu 26.04 and newer |
| pipewiresrc | `gstreamer-plugin-pipewire` | `pipewire-gstreamer` | `gstreamer1.0-pipewire` |
| pulsesrc | `gstreamer-plugins-good` | `gstreamer1-plugins-good` | `gstreamer1.0-plugins-good` |
| pw-cli, pw-loopback | `pipewire-tools` | `pipewire-utils` | `pipewire-bin` |
| pactl, pacat | `pulseaudio-utils` | `pulseaudio-utils` | `pulseaudio-utils` |
| x264enc (CPU encoding), recommended | `gstreamer-plugins-ugly` (x264 only in the Packman build) | `gstreamer1-plugins-ugly` (RPM Fusion; skipped without it) | `gstreamer1.0-plugins-ugly` |
| VA drivers, recommended | `Mesa-libva` or `intel-media-driver` | `mesa-va-drivers`, `libva-intel-media-driver` or `intel-media-driver` | `mesa-va-drivers`, `intel-media-va-driver(-non-free)` |
| kscreen-doctor, qdbus6 (KDE only) | `libkscreen6-plugin`, `qt6-tools-qdbus`, recommended if `plasma6-workspace` is installed | `libkscreen`, `qt6-qttools`, recommended if `plasma-workspace` is installed | `libkscreen-bin`, `qdbus-qt6`, recommended unless `hyprland` is installed |

The libraries the program links (GStreamer, GLib, libgbm) come from rpm's automatic
requirements and `${shlibs:Depends}`. The Mesa in Fedora and openSUSE has no H.264 or
H.265 encoding; the main README explains RPM Fusion and Packman. Without a working GPU
encoder Prostir falls back to x264 on the CPU (`encoder::pick` in
`crates/prostir/src/encoder.rs`), which is also what most ARM machines get.

## Setting up the project (once)

The project `home:iShawyha:prostir` exists. In its web UI:

1. **Repositories.** Make the list match the table above: the names, the paths
   (`update` for Fedora and Debian, `universe-update` for Ubuntu) and the
   architectures (x86_64 everywhere, aarch64 too where the table lists it). The
   quickest way is the project's Meta tab: replace the `<repository>` elements with
   the XML above and save. In the Repositories tab instead, a repository added from
   the list of distributions gets the `standard` or `universe` path; edit it to the
   one in the table.
   Then in the Project Config tab add `Prefer: libunwind-dev`: on Debian 13 two
   packages provide `libunwind-dev` (which `libgstreamer1.0-dev` needs), and without
   a preference the build stays unresolvable ("have choice for libunwind-dev").
2. **Package.** Create a package named `prostir` (Create Package). Add the file
   `_service` from this folder to it (Add File, or `osc add _service && osc commit`).
   That is the only file the package keeps. The first service run fails until a
   release has the OBS files attached (step 5).
3. **Token.** Your profile (top right) > Manage Your Tokens > Create Token. Type: Run
   services (runservice). Project: `home:iShawyha:prostir`. Package: `prostir`. Copy the
   token string; OBS shows it only once. With osc this is
   `osc token --create --operation runservice home:iShawyha:prostir prostir`.
4. **GitHub secret.** In the GitHub repository: Settings > Secrets and variables >
   Actions > New repository secret, name `OBS_TOKEN`, value the token string.
5. **First run.** Once `.github/workflows/obs.yml` is on the default branch on
   GitHub: Actions > OBS > Run workflow, tag `v0.1.3`. It attaches the
   four files to the v0.1.3 release and starts the service. Then watch
   https://build.opensuse.org/package/show/home:iShawyha:prostir/prostir: each
   repository should go from "scheduled" to "succeeded" in about 10 to 30 minutes,
   and the packages appear under
   https://download.opensuse.org/repositories/home:/iShawyha:/prostir/ some minutes
   after that.

From then on every published release does this by itself. Publishing (the repository
metadata and its signature) is on by default for a home project; OBS signs the
repositories with the project's key, which users fetch as `Release.key` or `repomd.xml.key`.

To rebuild without a new release: the package page > Trigger Services, or start the
workflow again for the latest tag.

## Building locally

Make the sources, the same way the workflow does:

```sh
curl -fLO https://github.com/prostir-app/prostir/releases/download/v0.1.3/THIRD-PARTY-LICENSES.txt
packaging/obs/make-sources.sh v0.1.3 THIRD-PARTY-LICENSES.txt /tmp/obs
```

rpm, for example in an openSUSE Tumbleweed or Fedora container. Install the build
dependencies with network, then build without it:

```sh
# zypper install rpm-build $(rpmspec -q --buildrequires prostir.spec | sed 's/ //g')
# dnf builddep prostir.spec
mkdir -p ~/rpmbuild/SOURCES && cp /tmp/obs/prostir-vendored.tar.gz ~/rpmbuild/SOURCES/
rpmbuild --define "_topdir $HOME/rpmbuild" -ba /tmp/obs/prostir.spec
```

deb, in a Debian or Ubuntu container, with OBS's own `debtransform` (from
https://github.com/openSUSE/obs-build):

```sh
perl debtransform --release 1.1 /tmp/obs /tmp/obs/prostir.dsc /tmp/dsc
dpkg-source -x /tmp/dsc/prostir_*.dsc /tmp/build
apt-get build-dep /tmp/build
cd /tmp/build && dpkg-buildpackage -b -us -uc
```

`osc build` against the OBS project does the same in OBS's own build root.

An arm64 image runs under qemu-user on an x86_64 PC (`podman run --arch arm64`): the
build works, slowly, but two tests that start programs (`run_stops_a_program_that_hangs`,
`kwin_scripts_parse`) fail there, because qemu-user reports a program that cannot be
started as one that exited with 127. Add `DEB_BUILD_OPTIONS=nocheck` (deb) or
`--nocheck` (rpmbuild) for such a build; OBS builds aarch64 on ARM machines.

## Files

- `make-sources.sh`: makes the four files for a tag.
- `prostir.spec`: rpm spec for openSUSE (`%if 0%{?suse_version}`) and Fedora.
- `prostir.dsc`: the Debian source description debtransform reads.
- `debian/`: `control`, `rules`, `changelog` (template), `copyright`, `source/format`,
  `prostir.docs`.
- `_service`: the one file kept on OBS.

`make-sources.sh` fills in `@VERSION@`, `@RUST_MIN@`, `@DATE@`, `@BUILD_DEPENDS@` and
the cargo line of `debian/control`; the files here are templates and do not build as
they are.
