# Ubuntu 24.04 LTS: Plasma 5.27, PipeWire 1.0, GStreamer 1.24, glibc 2.39.
FROM docker.io/library/ubuntu:24.04
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
    kwin-wayland kwin-x11 kscreen libkf5service-bin qdbus-qt5 dbus dbus-daemon dbus-user-session \
    pipewire pipewire-bin pipewire-pulse wireplumber pulseaudio-utils \
    gstreamer1.0-tools gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
    gstreamer1.0-plugins-ugly gstreamer1.0-pipewire gstreamer1.0-vaapi \
    libgl1-mesa-dri libegl1 libgbm1 libwayland-client0 libwayland-cursor0 libwayland-egl1 libxkbcommon0 wayland-utils \
    build-essential pkg-config clang libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
    libglib2.0-dev libgbm-dev libdrm-dev libwayland-dev libxkbcommon-dev libdbus-1-dev \
    procps iproute2 curl ca-certificates file binutils \
 && rm -rf /var/lib/apt/lists/*
# kwin_wayland carries file capabilities that a rootless container cannot grant.
RUN apt-get update && apt-get install -y --no-install-recommends libcap2-bin && rm -rf /var/lib/apt/lists/* && setcap -r /usr/bin/kwin_wayland
# Qt platform plugin for kscreen-doctor on Plasma 5.
RUN apt-get update && apt-get install -y --no-install-recommends qtwayland5 && rm -rf /var/lib/apt/lists/*
