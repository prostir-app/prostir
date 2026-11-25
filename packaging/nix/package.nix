# The PC app (`prostir`, with its settings window: `prostir window`), for Nix and
# NixOS: `nix build`, `nix run`, or the flake's package in a NixOS or Home Manager
# configuration.
{
  lib,
  stdenv,
  rustPlatform,
  pkg-config,
  makeWrapper,
  gst_all_1,
  libgbm,
  pipewire,
  pulseaudio,
  kdePackages,
  android-tools,
  libnotify,
  curl,
  xdg-user-dirs,
  grim,
  zenity,
  intel-media-driver,
  mesa,
  libGL,
  libxkbcommon,
  wayland,
  xorg,
}:

let
  root = ../..;
  version = (lib.importTOML (root + "/Cargo.toml")).workspace.package.version;
  # GStreamer plugins Prostir builds its pipelines from: va (Intel and AMD encoding,
  # in -bad), nvcodec (NVIDIA, in -bad), x264 (CPU, in -ugly), pipewiresrc, pulse.
  plugins = with gst_all_1; [
    gstreamer
    gst-plugins-base
    gst-plugins-good
    gst-plugins-bad
    gst-plugins-ugly
    pipewire
  ];
  # Programs it runs; the system's own come first.
  tools = [
    pipewire
    pulseaudio
    kdePackages.libkscreen
    kdePackages.qttools
    android-tools
    libnotify
    curl
    xdg-user-dirs
    grim
    zenity
  ];
  # VA-API drivers for encoding on the graphics card. On NixOS the system's own (in
  # /run/opengl-driver) match the hardware; elsewhere Nix's libva cannot load the
  # distribution's drivers, so Intel's and Mesa's (AMD) come along. NVIDIA's encoder
  # comes from the system driver (NixOS: hardware.nvidia).
  vaDrivers = [
    "/run/opengl-driver/lib/dri"
  ]
  # Intel graphics only come with x86 machines.
  ++ lib.optional stdenv.hostPlatform.isx86_64 "${intel-media-driver}/lib/dri"
  ++ [ "${mesa}/lib/dri" ];
  # Libraries the program loads at run time (dlopen) rather than linking them, so they
  # go on its RUNPATH: libwayland-client for the service's own Wayland connections
  # (KDE's virtual outputs, Hyprland's screen capture) and the window's; EGL, the
  # keyboard and X11 (in an X11 session) for the settings window (`prostir window`).
  # The RUNPATH is the program's own: the programs it runs do not inherit it.
  runtimeLibs = [
    libGL
    libxkbcommon
    wayland
    xorg.libX11
    xorg.libXcursor
    xorg.libXi
    xorg.libXrandr
    xorg.libXrender
    xorg.libxcb
  ];
in
rustPlatform.buildRustPackage {
  pname = "prostir";
  inherit version;

  src = lib.fileset.toSource {
    inherit root;
    fileset = lib.fileset.unions [
      (root + "/Cargo.toml")
      (root + "/Cargo.lock")
      (root + "/.cargo")
      # Every workspace member, the headset app and the fake headset for tests too:
      # cargo needs them present.
      (root + "/crates")
      # The texts of every program, one file per language, built into the binaries.
      (root + "/lang")
      (root + "/packaging/io.github.prostir_app.Prostir.desktop")
      (root + "/packaging/prostir.svg")
      (root + "/packaging/systemd")
      (root + "/packaging/udev")
    ];
  };

  cargoLock.lockFile = root + "/Cargo.lock";
  cargoBuildFlags = [
    "-p"
    "prostir"
    "--bin"
    "prostir"
  ];
  # The tests need a desktop session and the headset's toolchain; CI runs them.
  doCheck = false;

  nativeBuildInputs = [
    pkg-config
    makeWrapper
  ];
  buildInputs = [
    gst_all_1.gstreamer
    gst_all_1.gst-plugins-base
    gst_all_1.gst-plugins-bad
    libgbm
  ];

  postInstall = ''
    # The menu entry starts the wrapper, which sets up the environment the program needs.
    install -Dm644 packaging/io.github.prostir_app.Prostir.desktop \
      $out/share/applications/io.github.prostir_app.Prostir.desktop
    substituteInPlace $out/share/applications/io.github.prostir_app.Prostir.desktop \
      --replace-fail Exec=/usr/bin/prostir Exec=$out/bin/prostir
    # Up to Plasma 6.7, KWin lets a program use its restricted screencast interface
    # (X-KDE-Wayland-Interfaces) only if some entry's Exec is the file the process runs
    # (/proc/<pid>/exe), not a script that starts it. The wrapper starts
    # `.prostir-wrapped`, so that is the file, and the entry above (the wrapper) never
    # matches. This second entry names the real file and is hidden from the menus. Its
    # window class is dropped: the first entry owns the window. Plasma 6.8 and newer
    # do not look at either.
    substitute packaging/io.github.prostir_app.Prostir.desktop \
      $out/share/applications/io.github.prostir_app.Prostir.Access.desktop \
      --replace-fail Exec=/usr/bin/prostir Exec=$out/bin/.prostir-wrapped
    sed -i -e '/^StartupWMClass=/d' -e '/^\[Desktop Entry\]/a NoDisplay=true' \
      $out/share/applications/io.github.prostir_app.Prostir.Access.desktop
    install -Dm644 packaging/prostir.svg $out/share/icons/hicolor/scalable/apps/prostir.svg
    # Not enabled here: Prostir's first start enables the socket for its user (on
    # NixOS, `systemd.packages = [ prostir ];` makes the units and the preset known);
    # the tray turns it off for good.
    install -Dm644 packaging/systemd/prostir.socket $out/lib/systemd/user/prostir.socket
    install -Dm644 packaging/systemd/prostir.service $out/lib/systemd/user/prostir.service
    install -Dm644 packaging/systemd/80-prostir.preset \
      $out/lib/systemd/user-preset/80-prostir.preset
    substituteInPlace $out/lib/systemd/user/prostir.service \
      --replace-fail /usr/bin/prostir $out/bin/prostir
    # A Quest plugged in starts it where the socket is on (`services.udev.packages`).
    install -Dm644 packaging/udev/70-prostir-quest.rules \
      $out/lib/udev/rules.d/70-prostir-quest.rules
    # PROSTIR_EXE: the service starts the window, and the window the service, through
    # this wrapper.
    wrapProgram $out/bin/prostir \
      --prefix GST_PLUGIN_SYSTEM_PATH_1_0 : "${lib.makeSearchPathOutput "lib" "lib/gstreamer-1.0" plugins}" \
      --set-default LIBVA_DRIVERS_PATH "${lib.concatStringsSep ":" vaDrivers}" \
      --suffix PATH : "${lib.makeBinPath tools}" \
      --set PROSTIR_EXE "$out/bin/prostir"
  '';

  # After the fixup, which shrinks the RUNPATH to the libraries the program links:
  # those it loads at run time are not among them.
  postFixup = ''
    patchelf --add-rpath "${lib.makeLibraryPath runtimeLibs}" $out/bin/.prostir-wrapped
  '';

  meta = {
    description = "Extra monitors for a Linux PC (KDE Plasma 6 or Hyprland) in a Meta Quest 3";
    homepage = "https://prostir.ishawyha.dev";
    license = lib.licenses.gpl3Plus;
    mainProgram = "prostir";
    platforms = lib.platforms.linux;
  };
}
