# Debian 13: Sway (recorded only), PipeWire, GStreamer.
FROM docker.io/library/debian:13
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
    sway dbus dbus-daemon \
    pipewire pipewire-bin pipewire-pulse wireplumber pulseaudio-utils \
    gstreamer1.0-tools gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
    gstreamer1.0-plugins-ugly gstreamer1.0-pipewire \
    libgl1-mesa-dri libegl1 libgbm1 wayland-utils procps iproute2 curl ca-certificates file binutils \
 && rm -rf /var/lib/apt/lists/*
