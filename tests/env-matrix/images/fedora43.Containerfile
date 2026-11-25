# Fedora 43: KWin (Plasma 6.x), PipeWire, GStreamer, and the build tools for Prostir.
FROM registry.fedoraproject.org/fedora:43
RUN dnf -y install https://mirrors.rpmfusion.org/free/fedora/rpmfusion-free-release-43.noarch.rpm \
 && dnf -y --setopt=install_weak_deps=False install \
    kwin-wayland kwin kscreen kf6-kservice qt6-qttools dbus-daemon dbus-tools \
    pipewire pipewire-utils pipewire-pulseaudio pipewire-gstreamer wireplumber pulseaudio-utils \
    gstreamer1 gstreamer1-plugins-base gstreamer1-plugins-good gstreamer1-plugins-bad-free \
    gstreamer1-plugins-ugly gstreamer1-vaapi \
    mesa-dri-drivers mesa-libEGL mesa-libgbm libglvnd-egl \
    libwayland-client libwayland-cursor libwayland-egl libxkbcommon wayland-utils \
    gcc gcc-c++ make pkgconf-pkg-config clang \
    gstreamer1-devel gstreamer1-plugins-base-devel glib2-devel mesa-libgbm-devel libdrm-devel \
    wayland-devel libxkbcommon-devel dbus-devel \
    procps-ng iproute util-linux findutils which file curl \
 && dnf clean all
# kwin_wayland carries file capabilities (cap_sys_nice) that a rootless container cannot grant.
RUN dnf -y install libcap && for f in /usr/bin/kwin_wayland*; do setcap -r "$f" 2>/dev/null || true; done
# Only to record what GNOME's Mutter does headless (Prostir has no GNOME backend yet).
RUN dnf -y install --setopt=install_weak_deps=False mutter python3-gobject-base && dnf clean all
