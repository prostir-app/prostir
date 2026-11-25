# Ubuntu 25.10: Hyprland 0.41.2 (the older, wlroots based one), PipeWire, GStreamer.
FROM docker.io/library/ubuntu:25.10
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
    hyprland dbus dbus-daemon \
    pipewire pipewire-bin pipewire-pulse wireplumber pulseaudio-utils \
    gstreamer1.0-tools gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
    gstreamer1.0-plugins-ugly gstreamer1.0-pipewire \
    libgl1-mesa-dri libegl1 libgbm1 libwayland-client0 libwayland-cursor0 libwayland-egl1 libxkbcommon0 \
    wayland-utils procps iproute2 curl ca-certificates file binutils \
 && rm -rf /var/lib/apt/lists/*
# Plasma 6.4 (KWin) too: the Plasma 6.0-6.7 rule on a second version.
RUN apt-get update && apt-get install -y --no-install-recommends kwin-wayland kwin-common kwin-data kscreen libkf6service-bin qt6-wayland qdbus-qt6 libcap2-bin && rm -rf /var/lib/apt/lists/* && setcap -r /usr/bin/kwin_wayland
