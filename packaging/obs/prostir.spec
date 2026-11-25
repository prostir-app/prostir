#
# spec file for package prostir (openSUSE and Fedora, built on the Open Build Service)
#
# packaging/obs/make-sources.sh fills in @VERSION@ and makes the source archive: the
# tagged source with every crate in vendor/ and THIRD-PARTY-LICENSES.txt from the
# release, so the build needs no network. See packaging/obs/README.md.
#

Name:           prostir
Version:        @VERSION@
Release:        0%{?dist}
Summary:        Extra KDE Plasma 6 and Hyprland monitors in a Meta Quest 3 or 3S headset
License:        GPL-3.0-or-later
URL:            https://prostir.ishawyha.dev
Source0:        prostir-vendored.tar.gz

# The newest rust-version among the crates the program is built from, which can be
# newer than rust-version in Cargo.toml (make-sources.sh works it out).
BuildRequires:  cargo >= @RUST_MIN@
BuildRequires:  rust >= @RUST_MIN@
BuildRequires:  gcc
BuildRequires:  pkgconfig
BuildRequires:  pkgconfig(gbm)
BuildRequires:  pkgconfig(gstreamer-1.0) >= 1.24
BuildRequires:  pkgconfig(gstreamer-allocators-1.0) >= 1.24
BuildRequires:  pkgconfig(gstreamer-app-1.0) >= 1.24
BuildRequires:  pkgconfig(gstreamer-video-1.0) >= 1.24
BuildRequires:  pkgconfig(libpipewire-0.3)
# The PipeWire bindings are generated from its headers at build time (bindgen).
BuildRequires:  clang-devel

# The libraries the program links come from rpm's automatic requirements. These are
# the GStreamer plugins and the commands Prostir runs, and the libraries the settings
# window loads at run time (EGL, Wayland, the keyboard; libwayland-client the service
# too, for its own Wayland connections).
Requires:       curl
Requires:       pulseaudio-utils
%if 0%{?suse_version}
Requires:       libEGL1
Requires:       libwayland-client0
Requires:       libwayland-cursor0
Requires:       libwayland-egl1
Requires:       libxkbcommon0
# gstreamer-plugins-bad: the VA-API encoders (va plugin), h264parse and h265parse.
Requires:       gstreamer-plugin-pipewire
Requires:       gstreamer-plugins-bad
Requires:       gstreamer-plugins-base
Requires:       gstreamer-plugins-good
Requires:       pipewire-pulseaudio
Requires:       pipewire-tools
# x264enc for encoding on the CPU is only in the Packman build of this package.
Recommends:     gstreamer-plugins-ugly
Recommends:     (Mesa-libva or intel-media-driver)
Recommends:     libnotify-tools
# Only KDE Plasma needs these: kscreen-doctor and qdbus6.
Recommends:     (libkscreen6-plugin if plasma6-workspace)
Recommends:     (qt6-tools-qdbus if plasma6-workspace)
Suggests:       kdialog
Suggests:       zenity
Suggests:       grim
Suggests:       android-tools
# The settings window in an X11 session.
Suggests:       libX11-6
Suggests:       libX11-xcb1
Suggests:       libXcursor1
Suggests:       libXi6
Suggests:       libXrandr2
Suggests:       libXrender1
Suggests:       libxkbcommon-x11-0
%else
Requires:       libglvnd-egl
Requires:       libwayland-client
Requires:       libwayland-cursor
Requires:       libwayland-egl
Requires:       libxkbcommon
# gstreamer1-plugins-bad-free: the VA-API encoders (va plugin), h264parse and h265parse.
Requires:       gstreamer1-plugins-bad-free
Requires:       gstreamer1-plugins-base
Requires:       gstreamer1-plugins-good
Requires:       pipewire-gstreamer
Requires:       pipewire-pulseaudio
Requires:       pipewire-utils
# x264enc for encoding on the CPU comes from RPM Fusion; without it dnf skips this.
Recommends:     gstreamer1-plugins-ugly
Recommends:     (mesa-va-drivers or libva-intel-media-driver or intel-media-driver)
Recommends:     libnotify
# Only KDE Plasma needs these: kscreen-doctor and qdbus-qt6.
Recommends:     (libkscreen if plasma-workspace)
Recommends:     (qt6-qttools if plasma-workspace)
Suggests:       kdialog
Suggests:       zenity
Suggests:       grim
Suggests:       android-tools
# The settings window in an X11 session.
Suggests:       libX11
Suggests:       libX11-xcb
Suggests:       libXcursor
Suggests:       libXi
Suggests:       libXrandr
Suggests:       libXrender
Suggests:       libxkbcommon-x11
%endif

%description
Prostir puts extra monitors for a KDE Plasma 6 or Hyprland desktop into a Meta Quest 3
or 3S headset. They are real outputs of the desktop, streamed to the headset over the
local network. This package is the PC part; the headset app comes as an APK from the
GitHub release.

%prep
%autosetup -n prostir-%{version}

%build
# The crates come from vendor/ in the source archive; --frozen fails instead of
# reaching the network.
export CARGO_HOME="$PWD/.cargo-home"
# The distribution strips and makes the debuginfo package itself: from line tables,
# not from a program the release profile already stripped.
export CARGO_PROFILE_RELEASE_DEBUG=1 CARGO_PROFILE_RELEASE_STRIP=false
cargo build --frozen --release \
    --config 'source.crates-io.replace-with="vendored-sources"' \
    --config 'source.vendored-sources.directory="vendor"' \
    -p prostir --bin prostir

%install
install -Dm755 target/release/prostir %{buildroot}%{_bindir}/prostir
install -Dm644 packaging/io.github.prostir_app.Prostir.desktop %{buildroot}%{_datadir}/applications/io.github.prostir_app.Prostir.desktop
install -Dm644 packaging/prostir.svg %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/prostir.svg
# Not enabled here: Prostir's first start enables the socket for its user (and the
# preset for new users, where presets apply); the tray turns it off for good.
install -Dm644 packaging/systemd/prostir.socket %{buildroot}%{_prefix}/lib/systemd/user/prostir.socket
install -Dm644 packaging/systemd/prostir.service %{buildroot}%{_prefix}/lib/systemd/user/prostir.service
install -Dm644 packaging/systemd/80-prostir.preset %{buildroot}%{_prefix}/lib/systemd/user-preset/80-prostir.preset
install -Dm644 packaging/udev/70-prostir-quest.rules %{buildroot}%{_prefix}/lib/udev/rules.d/70-prostir-quest.rules

%check
export CARGO_HOME="$PWD/.cargo-home"
# The distribution strips and makes the debuginfo package itself: from line tables,
# not from a program the release profile already stripped.
export CARGO_PROFILE_RELEASE_DEBUG=1 CARGO_PROFILE_RELEASE_STRIP=false
# One test at a time: tests that start programs in parallel can hand each other's
# pipes to a child process, which made a_stopped_player_is_noticed fail now and then.
cargo test --frozen --release \
    --config 'source.crates-io.replace-with="vendored-sources"' \
    --config 'source.vendored-sources.directory="vendor"' \
    -p prostir -p prostir-proto -p prostir-lang -- --test-threads=1

%files
%license LICENSE LICENSE-EXCEPTION.md THIRD-PARTY-LICENSES.txt
%doc README.md README.uk.md CHANGELOG.md
%{_bindir}/prostir
%{_datadir}/applications/io.github.prostir_app.Prostir.desktop
%dir %{_datadir}/icons/hicolor
%dir %{_datadir}/icons/hicolor/scalable
%dir %{_datadir}/icons/hicolor/scalable/apps
%{_datadir}/icons/hicolor/scalable/apps/prostir.svg
%dir %{_prefix}/lib/systemd
%dir %{_prefix}/lib/systemd/user
%{_prefix}/lib/systemd/user/prostir.socket
%{_prefix}/lib/systemd/user/prostir.service
%dir %{_prefix}/lib/systemd/user-preset
%{_prefix}/lib/systemd/user-preset/80-prostir.preset
%{_prefix}/lib/udev/rules.d/70-prostir-quest.rules

%changelog
