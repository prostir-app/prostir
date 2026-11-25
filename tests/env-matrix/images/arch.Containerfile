# Arch Linux, latest: Plasma 6.x, Hyprland, current PipeWire and GStreamer.
FROM docker.io/library/archlinux:base-devel
RUN pacman -Syu --noconfirm --needed \
    kwin kscreen kservice qt6-tools dbus pipewire pipewire-pulse wireplumber libpulse \
    gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad gst-plugins-ugly gst-plugin-pipewire gst-plugin-va \
    mesa libglvnd libdrm wayland libxkbcommon wayland-utils hyprland clang \
    procps-ng iproute2 curl file \
 && pacman -Scc --noconfirm
# kwin_wayland carries file capabilities that a rootless container cannot grant.
RUN setcap -r /usr/bin/kwin_wayland
