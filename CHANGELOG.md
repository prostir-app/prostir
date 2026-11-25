# Changelog

## 0.2.0 (not released yet)

### Sound, menu and video while the picture changes a lot

- Sound, the microphone, focus and the menu's answers no longer wait while the PC sends
  a big frame. Frames go out in pieces of 16 KiB, and whatever is more urgent goes
  between them; the monitors take turns, so one monitor's keyframe no longer holds up
  the others. The PC also spreads what it sends over time, a little under what the
  network carries, instead of pushing a whole keyframe into the Wi-Fi router's queue at
  once: it watches the round trip, which grows as that queue fills, and never sends
  slower than the monitors need. A fast link is not slowed down. On a 100 Mbit/s link with
  keyframes on three monitors together, sound now arrives within about 10 ms (95 % of
  it) and at most about 20 ms late (before: 26 to 32 ms, and up to 90 ms), without the clicks
  of pauses over 30 ms (13 to 23 in 15 s before).
- A frame that waits too long is still dropped, and a new keyframe now also drops the
  older frames of its monitor still waiting, so a slow moment costs less.

### Monitors and the desktop

- KDE Plasma 6.0 to 6.7: KWin hides its screencast interface from a program that no
  application shortcut names, so Prostir found no way to add monitors. The shortcut
  now lists the interface and names the program by its real path in every package
  (the Nix package adds a hidden entry for the unwrapped program). When KWin still
  refuses, the settings window, the tray and the headset say what to do (install from
  the package, update to Plasma 6.8, or for a build from the source folder run
  `scripts/install-kde-shortcut.sh`) instead of failing without a reason.
- Monitors in the headset no longer overlap, however many there are. When a row would
  reach behind your back (past 210 degrees), the circle grows a little; past 1.5 m the
  monitors make a second row above the main one, then a third. Three 2560×1440
  monitors stay as they were; six big ones (three ultrawide) make two rows at about
  1.35 m.
- Your arrangement stays yours. A layout is applied only when you pick it (and on the
  very first start). A new monitor takes a free place at the end of a row, beside the
  others: of the longest row with room, at the end nearer to where you look; over or
  under the rows only when no row has room, and the next one then joins it there.
  Nothing else moves; a removed one leaves the others where they are. Two rows stay two rows after a restart
  of the PC or of the headset app, a reconnect, taking the headset off, Line up and a
  profile switch.
- Line up (the A button) takes the crookedness out of each row and nothing more: a
  monitor turned or hanging crooked, gaps and small steps between neighbours. Every row
  stays where it was and keeps how it stands: tilted back for a reclined pose,
  overhead for lying down (Line up now works there too), or turned a little as a
  whole. Two monitors placed as mirror images stay mirror images. Rows never merge, and
  a gap you left between two rows stays. A monitor the magnet glued over or under
  another stays glued: Line up closes the middle of the seam as you see it instead of
  lifting the monitor clear and leaving a gap (a wide flat monitor over a curved row
  overlaps the row a little at its ends, the nearer monitor in front, never by more
  than 12 % of a monitor's height; where it would, the monitor is lifted clear). Where
  the two overlap, the picture no longer flickers between them as you move your head.
- A monitor stays in its row while you move it about. A portrait monitor between two
  others no longer jumps into a row of its own when one of them is nudged; a monitor
  leaves its row only when you clearly move it out.
- A profile keeps where its monitors stood, so switching to it and back brings your
  rows back. Profiles saved before keep working and are laid out by their layout.
- A profile also keeps where you sat and which way you faced when you saved it, the
  circle its monitors stand on and which of them share a row. It comes back around you
  the way it stood, in front of where your monitors were last brought. So Line up and
  In front after switching to a profile do what they did before you saved it: monitors
  stacked one over another no longer fly apart (monitors side by side held together),
  and the PC desktop is the one the profile had. A profile saved by an earlier 0.2.0
  build has its monitors where they were in the room, as before, and its rows start
  afresh instead of following the profile before: save it again once it stands as you
  like.
- The PC desktop matches the headset in every direction: the mouse, and a window you
  drag across, go from one monitor to the next where the two meet in the headset,
  sideways and up or down. A monitor you hung higher on purpose is just as much higher
  on the desktop; a step the hand leaves between neighbours in a row, up to a tenth of
  the monitor's height (7 cm on a 1440-pixel one), is lined up. Neighbours always
  share enough of an edge for the mouse to cross, and the desktop no longer spreads out
  into a staircase. The map in the menu shows exactly the desktop the PC makes.
- A monitor whose picture cannot start on the PC keeps its place and is tried again at
  the next connection; the headset says why. Before, it was dropped from the saved set.
- A display larger than the headset can show (a 5120-pixel-wide monitor of the PC
  where the headset decodes H.264 up to 4096) is shown as failed, saying why, instead
  of ending the connection again and again; the others stay. Switching to a codec in
  which a monitor would be too large is refused, and nothing changes.
- Each display of the PC itself (the laptop's panel, a monitor on a cable) is now one
  of its own: it is recognised by its make and model, keeps its own place in the
  headset, and comes back there after the lid was closed or the cable pulled out.
  Another monitor plugged into the same connector gets a place of its own instead of
  taking the old one's. The eight seen last are remembered while not connected; older
  ones are forgotten, so a laptop that met many projectors does not gather them. A profile can show or hide each of them, and make one of them
  the primary display.
- A display keeps its number for good: a removed monitor's number is never given to
  another one, so after a restart or a change of monitors nothing lands in another
  monitor's place.
- The sizes to add a monitor come from the PC, with 5120×1440 when the headset decodes
  HEVC.
- "No limits" in the PC tray allows up to ten monitors and four of the PC's own
  displays, for when the headset copes with more than the safe six.
- Switching to a profile puts its monitors in their places as soon as the PC has made
  them, instead of watching for up to 8 s.
- A monitor you add shows at once where it will appear, as a frame saying it is being
  added, and its picture takes that place; a monitor you remove leaves the view at
  once, and comes back if the PC cannot remove it.
- When something the headset asked for does not happen, the menu says why right under
  the button that asked, in the headset's language (for example "The limit is reached" or
  "No active window on the PC"), with the PC's own detail under it. A request the PC
  does not answer within 30 s says so instead of waiting for ever.
- Every button of the open menu answers right under itself, for a few seconds: a
  profile saved, a monitor added or removed (or why not), a new layout, where the
  microphone goes and whether the headset may use it, the menu's language, a PC paired
  or forgotten, a screenshot saved. A wait for the PC shows on the menu's bottom line
  instead. Pressing the note (or its ×) or another button takes it away, and so does
  another page. A separate message in front of you is left for news that comes while
  the menu is closed and for the palm menu and the PC's commands, which before also
  showed behind an open menu.
- A desktop that thinks or hangs (KScreen, KWin, a graphics driver starting or
  stopping an encoder) no longer holds anything else up: the headset's requests are
  answered at once, sound, the microphone, the menu and the tray go on, and the
  monitors follow as soon as the desktop is ready. The same for a sound system that
  is slow to make the headset's sound device or microphone. Switching the codec no longer
  freezes the PC for seconds; a monitor that cannot encode with the new codec goes on
  with the one it had. A desktop that does not respond for 15 s is said in the
  headset, in the tray and by `prostir ctl status`; after 2 min Prostir quits with an
  error instead of hanging, so that whatever started it can start it again.
- Sharper text: on a Quest 3, Quest 3S, Quest 2 or Quest Pro a new layout makes each
  monitor as large as the headset's own pixels, one pixel of the desktop on one of the
  headset's (a 1920×1080 monitor was shown 30 % smaller than that). The monitors stay
  at the same distance and get larger. Where a layout does not fit around you that
  way, or a row over the main one would be out of view, all of them get the largest
  size that fits. A monitor added later is as large, for its pixels, as the one beside
  it. What you placed or resized yourself stays as it is. On other headsets the size
  is as before.
- Each monitor's picture is smoothed by the headset only while it is shown smaller than
  the headset's pixels, and sharpened always; before, it was smoothed always, which
  blurs a picture shown at full size.

### Settings

- The PC keeps its settings in `~/.config/prostir/settings.txt` and the monitors with
  their rows in `screens.txt`, plain text. The first start moves them over from
  `monitors.txt`, which stays for an older version to read.
- Options on the command line (`--monitor`, `--bitrate`, `--codec`, `--software`) hold
  for that run only and no longer replace what was saved. What you change while it
  runs is saved, also in the tray while no headset is connected.
- Monitors keep their numbers between runs, so after a restart each one comes back
  where you put it in the headset.
- The codec you choose in the headset stays chosen when the graphics card has to use
  another one.

### Counting users only with your yes

- Official builds count this PC among Prostir's users only if you agree. The first
  time the settings window opens, it asks "Count this PC among Prostir's users?", says
  what would be sent (once a day one request with only the date in it, no ID, PC name
  or settings), links to the privacy policy and offers Yes and No, neither chosen.
  Nothing is sent unless you answer Yes, so a PC where nobody answered (one that only
  runs `prostir serve`, a server, CI) never counts. The rest of the window works meanwhile.
- 0.1 counted until turned off and said so in a notification on the first start; that
  notification is gone. After an update from 0.1 the window asks once too, and nothing
  is sent until you answer; counting turned off in 0.1 stays off and is not asked
  about.
- Change the answer at any time in the settings window, the tray ("Count this PC among
  users") or with `prostir ctl count yes|no` (`prostir ctl count status` shows it). An
  AI agent over MCP can say no but never yes. `PROSTIR_NO_COUNT=1` still turns it
  off.
- As every build asks first, every release build may count, also one built from
  source: the key is in the source (`crates/prostir/count.key`), and
  `PROSTIR_COUNT_TOKEN` at build time still takes its place. Debug builds and the tests
  never count. A distribution that does not want counting builds without the `count`
  feature (`--no-default-features --features desktop`), and the window never asks. The answer is kept in `settings.txt`
  (`count unasked`, `yes` or `no`) and is in the local API's state (`counting`).

### Nothing runs while the headset is off

- Prostir starts with the system by default: its first start turns that on, and
  from then on it is kept as you choose it. Turn it off in the tray ("Start with the
  system"), the settings window or with `prostir ctl autostart off`; it stays off,
  also after updates. An update from 0.1 keeps what 0.1 did: on with its autostart
  entry (or systemd's socket), off without.
- On KDE Plasma (and other desktops with systemd) nothing of Prostir runs until a
  headset comes: systemd waits for it (`prostir.socket`) and starts Prostir when the
  headset connects, asks for the PC or is plugged in by USB. Turning it on enables
  the socket and removes the autostart entry of 0.1. Packages install the units and a
  preset that enables the socket for new users.
- Without systemd, and on Hyprland (`exec-once = prostir serve`), `prostir serve` sleeps in a
  small watcher that wakes for nothing (about 1 MB of its own memory) and starts the
  rest when a headset comes. A Quest plugged in by USB is noticed without waking for
  other devices. When another program holds Prostir's port, a notification and
  `prostir ctl status` say so. A crash of the rest no longer loses the port: the next headset
  starts it again.
- Once no headset needs it (none connected, no monitors waiting, no settings window,
  no pairing), Prostir leaves after 10 s, and the headset's next visit starts it
  again. `prostir serve --stay` keeps it running, as before.
- A headset can find its PC by asking, and the PC answers every headset only until it
  is paired, then only its own, so others nearby do not see it. A headset the PC
  forgot can no longer find it. A headset can also ask
  a PC it knows straight at its address, through a VPN such as Tailscale or WireGuard
  too, with no port opened on the router. The PC still announces itself while it
  runs, for headsets that do not ask yet.
- "Pair a new headset" in the tray, the settings window or `prostir ctl pair` lets a
  new headset find the PC for 3 minutes, until it pairs or "Stop pairing". When a new
  headset asks for a paired PC, the PC shows "New headset nearby. Pair it?" with a
  button that does the same.
- A headset that shows its pairing code while the settings window is open has the
  code typed in the window, not in a separate dialog.

### Control from the PC

- `prostir ctl` asks the running Prostir from a terminal, a script or a hot key:
  `prostir ctl recenter`, `prostir ctl profile Work`, `prostir ctl monitor add
  2560x1440@125%`, `prostir ctl status`, `prostir ctl watch` (the headset connecting,
  taken off, put on, leaving), each with `--json` and exit codes for scripts.
- The same on D-Bus for other programs and hot keys (`io.github.prostir_app.Prostir`
  on the session bus, see `LOCAL-API.md`).
- `prostir mcp` offers the same to an AI agent over the Model Context Protocol
  (`claude mcp add prostir -- prostir mcp` for Claude Code): the state, monitors and
  their arrangement, headset commands, a picture of a monitor, and waiting for the state
  to show a change. `prostir ctl screenshot ID` saves a picture of a monitor.
- One program, `prostir`, with one menu entry: `prostir` opens the settings window and
  starts Prostir in the background if it is not running; the tray has "Settings…" too.
  `prostir serve` runs it in the background, as autostart and systemd start it: on
  Hyprland, change `exec-once = prostir` to `exec-once = prostir serve`. The window is
  a process of its own, so the background part never loads its graphics libraries (EGL, X11)
  and a window that crashes takes nothing else down. 0.1's options without a command
  (`prostir -m 2560x1440`) still run it in the background, with a warning in the log.
  A bare `prostir` that the session starts (0.1's autostart entry, or Hyprland's
  `exec-once` right after login) also runs in the background, not the window; the
  autostart entry is changed to `prostir serve`, and on Hyprland a notification says to
  change the line. `install.sh` changes the entry too and points out such lines.

### Sound

- The PC tray says when the headset's microphone has no permission to record, so a
  silent microphone is no longer a mystery.
- The PC no longer keeps a microphone setting of its own: its microphone follows the
  switch in the headset.
- The PC no longer asks the sound system twice a second whether a program records
  from the headset microphone: the sound system tells it, so the headset microphone
  starts within a few hundredths of a second of a program starting to record (before:
  up to half a second), and nothing on the PC wakes while the sound is idle.
- The headset microphone starts as soon as it is allowed ("Ask again" in the menu, or
  the system's settings) while a PC program records; before, it stayed off until the
  program started recording again. A refusal in the system's prompt shows at once, not
  after a minute.
- The PC's sound no longer stays silent for the rest of a session when the headset's
  sound output does not open for a moment: it is tried again until it plays.

### Connection

- A PC picked in the menu, or paired with over USB, no longer holds the headset to
  that address: once its session is open, the headset finds the PC again over Wi-Fi
  when the cable is pulled, or at its new address when it gets one.
- When the PC cannot pair (no window for the code, another headset pairing, too many
  wrong codes), the headset shows what the PC said and asks again ("Pairing did not
  start on the PC", with "the other way round"), instead of counting wrong codes and
  trying again and again.
- A PC that forgot the headset is offered as such ("The PC no longer knows this
  headset"), not as a new PC, and pairing with it again says "Paired with".
- Forgetting a PC while the headset is connecting to it opens no session with it; a
  Forget that cannot be saved changes nothing.
- "Blocked by the firewall" is said only after the PC refused the connection twice in a
  row, and goes away once a connection opens.
- Leaving a PC, or a dropped connection, no longer leaves monitors with a frozen
  picture; a video decoder that fails is started again, and a background picture too
  big for the headset no longer stops the app.
- A dropped connection, a PC gone to sleep or a headset switched off is noticed within
  5 s on both sides (it took about 10 s).
- A headset put to sleep (its power button) gives the desktop back to the laptop
  within about a second; one taken off while awake after 5 s (it was 10 s). Taken
  off, the headset tells the PC within a second where it has a presence sensor
  (`XR_EXT_user_presence`), instead of when the system put it to sleep 15 s later.
- The headset says why a session ended: "The PC app was closed", "The PC ended the
  connection", or "No connection to the PC" when it fell silent. It looks for the PC
  again as before.
- "Forget paired headsets" in the PC tray ends the session of the forgotten headset at
  once; the headset then offers to pair again.
- Leaving a PC in the headset (another PC, forgetting it, closing the app) tells the
  PC why, and the PC writes it in its log. Both sides log the round trip of the
  connection every 10 s.
- What the tray asks of the headset (bring the monitors in front, No limits) waits for
  the headset's answer; a headset that does not answer within 30 s is written in the
  log instead of being waited for.
- A second headset can be given fewer rights with a line in `settings.txt`, for
  example only moving the mouse; the main headset can do everything, as before. A
  mistyped line gives that headset no rights rather than all of them.

## 0.1.3 (2026-11-20)

Fixes for the PC part. The headset app is the same as in 0.1.2.

### Monitors and windows

- On KDE, sharing one of Prostir's monitors in another app (Discord, OBS) no longer
  leaves it shrunk: Prostir puts its size back within a second.

### Sound

- The headset microphone could record only silence on the PC: the sound system kept
  a mute or a near-zero volume on Prostir's internal sound device from an earlier
  run. It now always stays at full volume.
- A recording no longer starts with old sound or loses its first moments, and sound
  held up by a Wi-Fi hiccup is dropped instead of coming out late: what you say comes
  out about 20 ms later.

### Packages

- The AUR has a source package, `prostir`, next to the prebuilt `prostir-bin`; either
  one replaces the other.

## 0.1.2 (2026-11-19)

A fix for the headset app. The PC part is the same as in 0.1.1.

### Monitors and windows

- Tapping a monitor on the map no longer closes the app when monitors overlap or
  stand far apart.

## 0.1.1 (2026-11-19)

Fixes for things that came up in the first days of use. Update both the PC part and
the headset app.

### Monitors and windows

- Take the headset off and put it on again, or let it reconnect after a short drop,
  and the monitors show their picture straight away. On KDE the picture used to stay
  frozen, or the monitors did not come back until the headset app was restarted.
- Windows that cross from one monitor to the next continue where they do in the
  headset. The desktop is now arranged from where the monitors' edges meet in space,
  so it no longer depends on where your head was when the arrangement was centred.
- Line up (the A button) stands every monitor upright again and joins the laptop
  screen after the monitors, so a monitor never ends up hanging off the laptop screen
  instead of beside the main one.
- Moving all monitors together keeps the arrangement in one piece while it moves.
- The map in the Monitors tab shows the monitors as the desktop has them, each at its
  own size.
- Cinema mode no longer loses your arrangement: opening the Meta menu, resizing with
  both hands, follow mode or a profile switch while in cinema used to save the cinema
  screen over it. Started in cinema mode, the app shows the cinema screen.
- After the PC part restarts, the desktop is arranged the way the monitors stood in
  the headset straight away, above and below included.
- A monitor that comes back with another resolution (a profile, a rotated monitor)
  keeps the same size of text as its neighbours, instead of the width the old
  resolution had. After a restart, "left", "middle" and "right" in the menu match
  where the monitors stand.

### In the headset

- Monitors stay visible under the Meta system menu instead of going dark.
- The app no longer closes when you come back to the headset, and it connects again
  after Horizon OS restarts it.
- With hands: a quick pinch on a menu button presses it when you let go; pinch and
  hold takes the menu, on a button too. Something taken within reach follows your
  hand, so you can pull it closer.

### PC

- A codec or encoder switch that fails leaves every monitor working with the previous
  settings, instead of a broken set that could later crash the PC part.
- Reconnecting starts each monitor's picture over once, not twice.
- Monitors given with `-m` no longer reset the sound and microphone choices, and
  quality changes in the tray are kept when no headset is connected.
- A hung `kscreen-doctor`, `qdbus`, `pactl` or screenshot tool can no longer hang
  Prostir.
- The PC part is split into a shared core and a Linux layer, the groundwork for
  GNOME and other systems. Nothing changes in use.
- The AUR package is published automatically, and release builds retry downloading
  the OpenXR loader.

## 0.1.0 (2026-11-18)

First public version.

### Monitors

- Virtual monitors that are real outputs of the desktop: KWin virtual outputs on KDE
  Plasma 6, headless outputs captured with wlr-screencopy straight into GPU buffers on
  Hyprland. Windows, panels and the mouse treat them like plugged-in screens.
- Up to six, at 1920×1080, 2560×1440, 3840×2160, 3440×1440 or 1440×2560, added,
  removed and made primary from the headset, on a map of the monitors as seen from
  where you sit.
- Shown as OpenXR compositor layers, flat or curved, so text stays sharp.
- Layouts: Around me, Wall and Lying down. Move, turn, bring nearer and resize any
  monitor with a controller or your hands, one at a time or all together.
- Line up (the A button, the Monitors tab or the palm menu) closes the seams in your
  own arrangement: neighbours join edge to edge, curving around you at the sides and
  tilting towards you above or below. Monitors put clearly apart stay.
- Stick together: a monitor let go almost against another joins it edge to edge; the
  edge lights up while you drag.
- The desktop is arranged the way you see it, so the mouse crosses where the monitors
  meet in the headset, sideways and up or down.
- The laptop screen in the headset, as part of the arrangement; it hides when the lid
  is closed.
- Profiles Work, Cinema and Lying down, each of which can be overwritten with your own
  setup. Monitors of the same size stay when you switch, so their windows do not move.
- Cinema mode (one big screen, and you pick which), focus mode (other monitors dim) and
  follow my head.
- Detach up to three PC windows into floating panels and attach them back; move the
  active window to the monitor in front of you; save a screenshot of that monitor to a
  Prostir folder in your Pictures folder (on KDE taken from that monitor's own stream).
- Monitor brightness for all monitors at once (100, 80, 60 or 40 %).

### In the headset

- A menu with Quick, Monitors and Settings tabs, which you can touch with a finger when
  your hands are in view.
- A round palm menu: hold an open palm to your face for In front, Focus, Cinema,
  Mouse?, Screenshot, Line up and Menu, with the time, the date and the headset's
  battery in the middle. Left or right hand; 24- or 12-hour clock, the date in
  Ukrainian, English or numbers. The round menu button by the monitor can be hidden.
- Hands shown, and if you like used for pointing: pinch to click, pinch and hold to
  drag.
- Backgrounds: black, your room through the passthrough cameras (plain or dimmed),
  four built-in panoramas or a picture of your own. A keyboard window shows the real
  desk.
- The "Mouse?" button shows where the pointer is, with rings; on Hyprland the mouse can
  follow your gaze.
- Optional PC notifications in the headset and rest reminders; a 72 Hz display rate to
  save battery.
- First-run tips, and a start screen that explains where to get the PC part.
- Ukrainian and English, in the headset and on the PC.

### Sound

- PC sound plays in the headset, and the headset microphone becomes the PC's
  microphone. The headset records only while a PC app listens and the microphone switch
  in the headset is on.
- Optional sound from the screen: the sound comes from the side of the monitor whose
  window is playing it.

### PC

- Encoding on the GPU through VA-API (tested on Intel; AMD should work but is
  untested) or NVENC on NVIDIA (experimental and untested), and on the CPU with x264
  everywhere else. HEVC or H.264, video quality and sharper text, set from the headset
  or the tray.
- Tray icon: connection status, headset battery with a warning at 15 %, monitors in
  front of you, video quality, autostart (KDE), the usage counter, forgetting paired headsets.
- Taking the headset off pauses streaming; after 10 s the virtual monitors go away and
  windows, the panel, sound and microphone return to the laptop until you put it on
  again.
- When the headset and the PC run versions that cannot talk, both say which one to
  update.
- Packages: `prostir-bin` on the AUR, a `.deb` and an `.rpm` (Fedora), a tarball with
  `install.sh` (with `--autostart` and `--uninstall --purge`), and a Nix flake
  (`nix run github:prostir-app/prostir`, NixOS and `nix develop`). Every release
  ships `THIRD-PARTY-LICENSES.txt`.
- Official builds count how many PCs use Prostir: one anonymous request a day with only
  the date in it, announced on the first start; off in the tray or with
  `PROSTIR_NO_COUNT=1`. Builds from source never count.

### Connection and security

- The headset finds the PC over Wi-Fi by itself, or you pick it from a list or type its
  IP address. In developer mode a USB cable works too (`adb reverse`).
- Encrypted connections (Noise XX) and one-time pairing with a 6-digit code (SPAKE2),
  confirmed in the headset first and typed on either side. The PC slows down repeated
  failed attempts.
- The headset lists the paired PCs, and you can forget any of them.
- The protocol between the headset and the PC is documented in `PROTOCOL.md`.
- Website with downloads and the privacy policy: prostir.ishawyha.dev.
