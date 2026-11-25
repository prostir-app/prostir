# driftwm #180: коментар (чернетка)

Куди: https://github.com/malbiruk/driftwm/issues/180 (і коротке посилання в #202).
Публікує власник. Політика driftwm дозволяє текст і код з ШІ (CONTRIBUTING, AGENTS.md).

---

Hi! I'd like to help with this one, and I can send the PR myself.

My use case is a bit different from OBS/Discord, but it needs the same piece. I'm building
[Prostir](https://github.com/prostir-app/prostir), which shows a Linux desktop's monitors in a
VR headset. On KDE and Hyprland it asks the compositor for extra monitors that don't physically
exist, captures them and streams them to the headset. driftwm already has
`ext-image-copy-capture-v1` for outputs, so the only missing part is creating the output itself.
An infinite canvas is a really natural fit for VR, so I'd love driftwm to work with it.

Before writing code, a few design questions:

1. **A real `wl_output` or only a canvas region?** My suggestion is a real output whose view is
   pinned to a canvas rectangle by default. Then the portal and every capture client see it as an
   ordinary monitor, windows can be fullscreened on it, and no new capture code is needed:
   `compose_frame` plus the existing capture path already work without DRM (the headless test
   fixture does the same).
2. **Interface.** Your `virtual-display create -g <geometry>` for slurp, plus a form without a
   geometry for programs: `--size 1920x1080 --refresh 90 --name VR-1`. And `remove <name>`,
   `list`.
3. **View.** Pinned to its rectangle, or movable like a real output (`camera --output`)? For VR
   both are useful; pinned by default seems least surprising.
4. **Lifetime.** Until `remove`, or until the IPC connection that created it closes? The second
   means a crashed client never leaves an invisible monitor behind. Maybe a flag picks one.
5. **Frame pacing.** A timer at the requested refresh that sleeps when nothing is damaged and no
   capture is pending, or a frame only when a capture asks for one?
6. **Scope.** Should this come together with headless (#202), or first only next to the udev and
   winit backends? I'd start small: create/remove/list for udev and winit with tests, then
   `wlr-output-management` and config, then headless as its own PR.

A few things I'd handle from the start: the lock screen is shown on virtual outputs too (so a
capture never shows the desktop while locked), names are stable for `[[outputs]]` rules and
session restore, and the name doesn't clash with the existing placeholder outputs.

Happy to follow whatever shape you prefer.
