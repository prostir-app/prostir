# Prostir texts, English: one file for the PC app, the settings window and the headset
# app. Every other language falls back to this file message by message, so it has every
# message.
#
# A message id starts with the program that shows it: `pc-` (the PC app), `window-`
# (the settings window), `headset-` (the headset app). A comment `# $name` above a
# message says what its placeholders are. Plural forms follow the language's own rules
# ({ $count -> [one] ... *[other] ... }); NUMBER() is there for a number formatted
# another way.
#
# A new language: copy this file as <code>.ftl (for example de.ftl), translate the text
# after each "=", and put it in the folder of the program that should use it:
#   settings window: ~/.local/share/prostir/lang/settings/
#   headset app:     Android/data/dev.prostir.client/files/lang/
# A file can keep only the messages of that program.

language-name = English

### PC app

## Texts with a count in them: the word takes the plural form the count needs

pc-limit-monitors = At most { $n } { $n ->
    [one] monitor
   *[other] monitors
}
pc-limit-windows = At most { $n } { $n ->
    [one] window
   *[other] windows
}
pc-limit-physical = At most { $n } { $n ->
    [one] display of the PC
   *[other] displays of the PC
}
pc-quality-custom = Custom: { $n } Mbit/s
pc-limit-streams = At most { $n } { $n ->
    [one] display
   *[other] displays
} in all

## src/ctl.rs

pc-ctl-waiting-for-the-headset-asleep = Waiting for the headset (asleep)
pc-ctl-not-running-port = Not running: port
pc-ctl-is-taken-by-another-program = is taken by another program
pc-ctl-not-running = Not running
pc-ctl-no-session-bus = No session bus
pc-ctl-prostir-is-not-running = Prostir is not running
pc-ctl-no-answer-in-time = No answer in time
pc-ctl-whole-desktop = { " (whole desktop)" }
pc-ctl-saved = Saved:
pc-ctl-added-id = Added, id
pc-ctl-pairing-is-open-for = Pairing is open for
pc-ctl-s = s
pc-ctl-done = Done
pc-ctl-yes = yes
pc-ctl-no = no
pc-ctl-not-asked-yet = not asked yet
pc-ctl-unknown = unknown
pc-ctl-not-available = not available
pc-ctl-on = on
pc-ctl-off = off
pc-ctl-taken-off = , taken off
pc-ctl-connected = Connected:
pc-ctl-monitors = monitors
pc-ctl-waiting-for-the-headset-monitors = Waiting for the headset, monitors kept
pc-ctl-waiting-for-a-headset = Waiting for a headset
pc-ctl-power-saving-is-on-for-this = Power saving is on for this PC's Wi-Fi
pc-ctl-it-can-hold-the-picture-back = it can hold the picture back by tens of ms. A cable helps, or `nmcli connection modify <connection> 802-11-wireless.powersave 2`
pc-ctl-monitor = monitor
pc-ctl-pc-display = PC display
pc-ctl-window = window
pc-ctl-starting = starting
pc-ctl-running = running
pc-ctl-failed = failed
pc-ctl-not-connected = not connected
pc-ctl-kept-for-the-headset = kept for the headset
pc-ctl-shown = , shown
pc-ctl-hidden = , hidden
pc-ctl-primary = , primary
pc-ctl-frames-s = frames/s
pc-ctl-repeated = repeated
pc-ctl-mbit-s = Mbit/s
pc-ctl-encoder = encoder
pc-ctl-ms = ms
pc-ctl-max = max
pc-ctl-guest = guest
pc-ctl-main = main
pc-ctl-connected-2 = , connected
pc-ctl-prostir-is-not-running-waiting = Prostir is not running, waiting
pc-ctl-prostir-is-running = Prostir is running
pc-ctl-headset-connected = Headset connected:
pc-ctl-headset-disconnected = Headset disconnected:
pc-ctl-headset-put-on = Headset put on:
pc-ctl-headset-taken-off = Headset taken off:
pc-ctl-prostir-did-not-come-on-the = Prostir did not come on the bus in time
pc-ctl-prostir-ended-before-it-started = Prostir ended before it started

## src/control.rs

pc-control-no-headset-is-connected = No headset is connected

## src/main.rs

pc-main-prostir-in-the-hyprland-config = Prostir in the Hyprland config
pc-main-change-exec-once-prostir-to-exec = Change exec-once = prostir to exec-once = prostir serve: a bare prostir now opens the settings window.
pc-main-prostir-is-not-running = Prostir is not running
pc-main-port = Port
pc-main-is-taken-by-another-program = is taken by another program.

## src/platform/linux/tray.rs

pc-tray-on = on
pc-tray-off = off
pc-tray-headset-taken-off-streaming-paused = Headset taken off, streaming paused
pc-tray-headset-connected = Headset connected
pc-tray-waiting-for-the-headset = Waiting for the headset…
pc-tray-a-pc-display-in-the-headset = , a PC display in the headset
pc-tray-no-virtual-monitors = No virtual monitors
pc-tray-virtual-monitors = Virtual monitors
pc-tray-pairing-code = Pairing code
pc-tray-other-headsets = Other headsets
pc-tray-the-desktop-does-not-respond = The desktop does not respond
pc-tray-headset-microphone-no-permission = Headset microphone: no permission
pc-tray-headset-battery = Headset battery:
pc-tray-charging = , charging
pc-tray-bring-the-monitors-in-front = Bring the monitors in front
pc-tray-no-limits = No limits
pc-tray-video-quality = Video quality
pc-tray-sharper-text-about-2-ms-more = Sharper text (about 2 ms more encoding)
pc-tray-count-this-pc-among-users = Count this PC among users
pc-tray-start-with-the-system = Start with the system
pc-tray-settings = Settings…
pc-tray-stop-pairing = Stop pairing
pc-tray-pair-a-new-headset = Pair a new headset
pc-tray-forget-paired-headsets = Forget paired headsets
pc-tray-quit = Quit
pc-tray-quality-economy = Economy: 15 Mbit/s
pc-tray-quality-normal = Normal: 30 Mbit/s
pc-tray-quality-high = High: 50 Mbit/s

## src/platform/linux/dialogs.rs

pc-dialogs-a-headset-wants-to-connect-type =
    A headset wants to connect.
    Type the code it shows:
pc-dialogs-code-from-the-headset = { "Code from the headset: " }

## src/platform/linux/autostart.rs

pc-autostart-starting-with-hyprland = Starting with Hyprland
pc-autostart-for-prostir-to-wait-for-the = For Prostir to wait for the headset after login, add this line to the Hyprland config: exec-once = prostir serve

## src/platform/local_api.rs

pc-local-api-prostir-is-quitting = Prostir is quitting

## src/local.rs

pc-local-a-profile-s-name-is-too = A profile's name is too long, bytes at most:
pc-local-the-command-does-not-fit = The command does not fit
pc-local-it-did-not-work = It did not work
pc-local-no-headset-is-connected = No headset is connected
pc-local-commands-from-the-pc-go-to = Commands from the PC go to the main headset; a guest is connected
pc-local-the-code-six-digits = The code: six digits
pc-local-no-headset-is-waiting-for-a = No headset is waiting for a code
pc-local-a-screenshot-is-taken-while-the = A screenshot is taken while the monitors are on the desktop
pc-local-the-monitors-left-the-desktop-before = The monitors left the desktop before the screenshot
pc-local-quality-5-to-100-mbit-s = Quality: 5 to 100 Mbit/s
pc-local-starting-with-the-session-is-not = Starting with the session is not set from here on this PC
pc-local-the-headset-microphone-is-switched = The headset microphone is switched in the headset only
pc-local-monitors-are-placed-while-they-are = Monitors are placed while they are on the desktop
pc-local-the-headset-s-volume-is-changed = The headset's volume is changed while it is connected
pc-local-this-headset-s-volume-is-not = This headset's volume is not changed from the PC, only with its buttons
pc-local-no-paired-headset = No paired headset
pc-local-several-headsets-have-this-name-give = Several headsets have this name: give its key
pc-local-the-headset-s-system-does-not = The headset's system does not let it record its microphone
pc-local-process = process
pc-local-prostir-is-already-running = Prostir is already running
pc-local-but-not-on-this-session-bus = , but not on this session bus: its window opens from the session it was started in
pc-local-this-prostir-was-started-by-another = this Prostir was started by another one, which found none running: it starts no other

## src/pairing.rs

pc-pairing-wrong-pairing-codes = Wrong pairing codes
pc-pairing-someone-typed-wrong-codes-several = Someone typed wrong codes several times. New attempts now have to wait.
pc-pairing-too-many-wrong-codes-try-again = Too many wrong codes. Try again in
pc-pairing-min = min.
pc-pairing-another-headset-is-pairing-with-this = Another headset is pairing with this PC right now. Try again in a minute.
pc-pairing-no-code-was-typed-on-the = No code was typed on the PC
pc-pairing-the-pc-has-no-dialog-for = The PC has no dialog for the code. Choose "The other way: code on the PC, type it here".
pc-pairing-headset = Headset
pc-pairing-headset-paired = Headset paired
pc-pairing-can-connect-now = can connect now.
pc-pairing-pairing-a-headset = Pairing a headset
pc-pairing-type-this-code-in-the-headset = Type this code in the headset:
pc-pairing-older = older
pc-pairing-update-prostir-on-the-pc = Update Prostir on the PC
pc-pairing-the-headset-app-is-newer = The headset app is newer:
pc-pairing-the-pc-has = the PC has
pc-pairing-update-prostir-in-the-headset = Update Prostir in the headset
pc-pairing-the-headset-app-is-older = The headset app is older:
pc-pairing-the-headset-and-pc-builds-are = The headset and PC builds are from different versions of the branch
pc-pairing-the-headset-has = The headset has
pc-pairing-update-both = Update both.

## src/sessions.rs

pc-sessions-this-pc-no-longer-trusts-this = This PC no longer trusts this headset. Pair them again.
pc-sessions-another-headset-is-already-connected = Another headset is already connected to this PC.

## src/session/mod.rs

pc-session-headset-battery = Headset battery:
pc-session-the-headset-battery-is-low = The headset battery is low
pc-session-the-headset-takes-no-commands = The headset takes no commands
pc-session-the-headset-does-not-answer = The headset does not answer
pc-session-the-headset-disconnected = The headset disconnected
pc-session-the-desktop-does-not-respond = The desktop does not respond

## src/wake.rs

pc-wake-new-headset-nearby = New headset nearby
pc-wake-pair-it = Pair it?
pc-wake-pair = Pair
pc-wake-to-pair-it-choose-pair-a = To pair it, choose "Pair a new headset" in the Prostir menu.

## src/errors.rs

pc-errors-the-pc-cannot-do-this = The PC cannot do this
pc-errors-this-headset-is-not-allowed-to = This headset is not allowed to do this
pc-errors-no-such-display = No such display
pc-errors-the-pc-does-not-make-a = The PC does not make a monitor of
pc-errors-the-last-monitor-cannot-be-removed = The last monitor cannot be removed
pc-errors-no-active-window-on-the-pc = No active window on the PC
pc-errors-the-pc-s-desktop-did-not = The PC's desktop did not do it
pc-errors-the-encoder-did-not-start = The encoder did not start
pc-errors-the-picture-could-not-be-taken = The picture could not be taken
pc-errors-the-sound-could-not-be-switched = The sound could not be switched
pc-errors-the-mouse-is-on-none-of = The mouse is on none of the displays
pc-errors-a-value-out-of-range = A value out of range

## src/displays/arrange.rs

pc-displays-arrange-the-order-does-not-match-the = The order does not match the monitors

## src/displays/encoding.rs

pc-displays-encoding-more = more
pc-displays-encoding-is-larger-than-the-headset-decodes = is larger than the headset decodes in

## src/displays/parts.rs

pc-displays-parts-more-streams-than-the-headset-takes = more streams than the headset takes

## src/displays/sound.rs

pc-displays-sound-the-microphone-is-not-ready-yet = The microphone is not ready yet

## src/encoder/choice.rs

pc-encoder-choice-already-has = already has
pc-encoder-choice-sessions-open-the-most-it-opens = sessions open, the most it opens
pc-encoder-choice-did-not-start = did not start
pc-encoder-choice-encoding-on-the-cpu = encoding on the CPU
pc-encoder-choice-frames-go-through-the-cpu = frames go through the CPU

## src/encoder/probe.rs

pc-encoder-probe-no-video-encoder-on-the-pc = No video encoder on the PC. Intel or AMD: install the GStreamer VA plugin (gst-plugin-va or gst-plugins-bad) and a VA driver. NVIDIA: the NVIDIA driver and nvcodec from gst-plugins-bad. Any graphics: gst-plugins-ugly (x264, encoding on the CPU).

## src/worker/run/encoders.rs

pc-worker-run-encoders-encoders-do-not-respond-no-more = encoders do not respond: no more are started
pc-worker-run-encoders-did-not-respond-times = did not respond, times:
pc-worker-run-encoders-the-source-of-the-frames-fails = the source of the frames fails
pc-worker-run-encoders-no-encoder-for-this-output = no encoder for this output
pc-worker-run-encoders-another-program-keeps-changing-the = Another program keeps changing the size of monitor
pc-worker-run-encoders-share-an-app-window-in-it = Share an app window in it instead of the monitor, then take the headset off and put it on again.
pc-worker-run-encoders-monitor-size = Monitor size
pc-worker-run-encoders-kept = kept
pc-worker-run-encoders-did-not-start = did not start

## src/worker/run/outputs.rs

pc-worker-run-outputs-its-output-on-the-desktop-is = its output on the desktop is gone
pc-worker-run-outputs-a-screen-sharing-program-changed-the = A screen sharing program changed the size of monitor
pc-worker-run-outputs-its-size-is-back-but-the = Its size is back, but the share may jump. To avoid that, share an app window instead of the monitor.
pc-worker-run-outputs-screen-sharing = Screen sharing

## src/worker/run/tests.rs

pc-worker-run-tests-kept-h-264-hevc-did-not = kept H.264: HEVC did not start

## src/platform/linux/audio.rs

pc-audio-prostir-headset = Prostir headset
pc-audio-prostir-internal = Prostir (internal)
pc-audio-prostir-headset-microphone = Prostir headset microphone

## src/platform/sound.rs

# $name: the driver, $url: where to get it, $device: the device to choose
pc-sound-needs-driver = Needs the { $name } driver: { $url }. Once it is installed, choose “{ $device }” as the microphone in programs.

### Settings window

## Pages

window-nav-overview = Overview
window-nav-monitors = Monitors
window-nav-headsets = Headsets
window-nav-sound = Sound
window-nav-quality = Quality
window-nav-look = Look
window-quit-prostir = Quit Prostir
window-close-note-title = Prostir keeps running
window-close-note = Prostir keeps running in the tray. To open it again: the Prostir icon in the menu or in the tray.
window-nav-first-run = First run

## The status line under the pages

window-side-loading = Connecting to Prostir
window-side-connected = { $name } connected
window-side-taken-off = { $name } taken off
window-side-connecting = Connecting { $name }
window-side-waiting = Waiting for a headset
window-side-failed = A monitor was not added
window-side-not-running = Prostir is not running
window-side-silent = Prostir does not answer
window-side-old = Prostir is older than this window
window-side-no-headsets = No headsets yet

## Overview

window-ov-title = Overview
window-ov-connected-title = Headset connected
window-ov-connected-wifi = { $name } over Wi-Fi. Monitors in the headset: { $count }.
window-ov-connected-usb = { $name } over a USB cable. Monitors in the headset: { $count }.
window-ov-wifi-power-save = Power saving is on for this PC's Wi-Fi ({ $interface }). It can hold the picture back by tens of milliseconds. A cable helps, or turning power saving off for this connection.
window-ov-taken-off-title = Headset taken off
window-ov-taken-off-body = { $name } is paused: the monitors wait until the headset is on again.
window-ov-battery = headset battery
window-ov-charging = charging
window-ov-fact-sound = PC sound
window-ov-fact-mic = Headset microphone
window-ov-fact-profile = Profile
window-ov-profile-none = None
window-ov-connecting-title = Connecting { $name }
window-ov-connecting-body = The headset found this PC. Setting up an encrypted connection.
window-ov-waiting-title = Waiting for a headset
window-ov-waiting-body = Open Prostir in the headset near this PC. The headset finds the PC by itself when both are on the same network.
window-ov-failed-title = A monitor was not added
window-ov-failed-body = { $name }: { $reason } The other monitors work.
window-ov-failed-no-reason = { $name } was not added. The other monitors work.
window-ov-retry = Try again
window-ov-open-monitors = Go to monitors
window-ov-no-headsets-title = Pair the first headset
window-ov-no-headsets-body = This PC does not know any headset yet. First run takes you through it in four steps.
window-ov-no-headsets-go = Start first run
window-ov-pair-new = Pair a new headset
window-ov-problems = Needs attention
window-ov-monitors = Monitors in the headset
window-ov-monitors-empty = Monitors appear here once a headset connects.
window-ov-pc = This PC
window-ov-pc-name = Name on the network
window-ov-pc-desktop = Desktop
window-ov-autostart = Start with the system
window-ov-autostart-desc = Prostir waits for the headset after you log in. No window opens then.
window-ov-autostart-here-not = Set up with the desktop on this PC.
window-ov-counting = Count this PC among users
window-ov-counting-desc = Once a day one request that says only that Prostir was used: no name, no address, no settings.

## The question about counting, until it is answered

window-count-ask-title = Count this PC among Prostir's users?
window-count-ask-body = Prostir is free and has no ads. The number of PCs it runs on is the only way to know whether it is worth developing further. Only the date is sent, once a day (and once, that Prostir was installed), with nothing about you.
window-count-ask-later = Sent through GoatCounter. Nothing is sent unless you answer Yes. The answer can be changed at any time on the Overview page or in the tray.
window-count-ask-yes = Yes
window-count-ask-no = No
window-count-ask-yes-name = Yes, count this PC
window-count-ask-no-name = No, do not count this PC
window-count-ask-privacy = Privacy policy
window-count-ask-privacy-url = https://prostir.ishawyha.dev/privacy.html#counting

## How a monitor is doing

window-state-streaming = Streaming
window-state-starting = Starting
window-state-failed = Not added
window-state-absent = Not connected
window-state-kept = Waiting for the headset
window-state-shown = Shown in the headset
window-state-hidden = Hidden

## Short values

window-sound-in-headset = In the headset
window-sound-on-pc = On the PC
window-on = On
window-off = Off
window-percent = { $n }%
window-decimal-separator = .

## Prostir is not running

window-loading-title = Connecting to Prostir
window-nr-title = Prostir is not running
window-nr-body = The settings come from Prostir running in the background. Start it, and this window updates by itself.
window-nr-start = Start Prostir
window-nr-starting = Starting Prostir
window-nr-failed-title = Prostir did not start
window-nr-failed-body = Run prostir in a terminal to see what stops it.
window-nr-no-program = The prostir program was not found. Check that Prostir is installed.
window-nr-old-title = This Prostir is older than the window
window-nr-old-body = Prostir is running, but this version does not work with the settings window. Update Prostir, then try again.
window-nr-retry = Try again
window-nr-silent-title = Prostir does not answer
window-nr-silent-body = Prostir is running but did not answer this window. The window asks again by itself.
window-nr-no-bus = There is no D-Bus session bus, so this window cannot reach Prostir.

## Monitors

window-mon-title = Monitors
window-mon-sub = As the PC's desktop sees them: the mouse moves between monitors where they touch.
window-mon-flat = Flat
window-mon-arc = Around you
window-mon-hint-flat = Drag a monitor, or select it and move it with the arrow keys. It sticks to its neighbour.
window-mon-hint-arc = How the monitors stand around you. Drag an empty spot to turn the view.
window-mon-empty = No monitors to show yet.
window-mon-selected = Selected monitor
window-mon-none = Select a monitor on the map.
window-mon-size = { $w } × { $h }
window-mon-size-scaled = { $w } × { $h }, scale { $scale }%
window-mon-size-at-scale = { $w } × { $h } · { $scale }%
window-mon-scale = Scale
window-mon-main = Main monitor
window-mon-main-desc = The panel and new windows go here.
window-mon-remove = Remove monitor
window-mon-remove-named = Remove { $name }
window-mon-in-headset = In the headset
window-mon-shown = Shown
window-mon-hidden = Hidden
window-mon-laptop = Laptop screen
window-mon-pc-screen = PC screen
window-mon-add = Add a monitor
window-mon-add-desc = A new monitor appears next to the others.
window-mon-placing = { $name }: being placed…
window-mon-add-full = No more monitors fit: { $max } are in the headset already.
window-mon-add-offline = The monitor appears in the headset when it connects.
window-mon-failed = Not added: { $reason }
window-mon-kept = Waiting for the headset
window-mon-kept-desc = Kept for the next headset: they appear when it connects.

## Headsets

window-hs-title = Headsets
window-hs-sub = Headsets this PC trusts. A new headset needs a code only once.
# $address (String): the PC's addresses, separated by commas
window-hs-address = This PC's address, for a headset that does not find it by itself: { $address }
window-hs-now = Connected now
window-hs-last = Last connected { $when }
window-hs-never = Last connected: not known
window-hs-app = Prostir { $app }, key { $key }
window-hs-key = Key { $key }
window-ago-now = just now
window-ago-minutes = { $n ->
    [one] { $n } minute ago
   *[other] { $n } minutes ago
}
window-ago-hours = { $n ->
    [one] { $n } hour ago
   *[other] { $n } hours ago
}
window-ago-days = { $n ->
    [one] { $n } day ago
   *[other] { $n } days ago
}
window-hs-forget = Forget
window-hs-forget-named = Forget { $name }
window-hs-forget-ask = Forget { $name }? To connect again, it will need a code.
window-hs-cancel = Cancel
window-hs-empty-title = No paired headsets yet
window-hs-empty-body = Open Prostir in the headset near this PC and pair them with a code.
window-hs-pair-hint = To pair a new headset, open Prostir in it near this PC: the PC shows a code here to type in the headset.
window-hs-guest-name = { $name } (guest)
window-hs-profiles = Profiles in { $name }
window-hs-profile-n = Profile { $n }
window-hs-profile-in-use = In use now
window-hs-profile-rename = Rename
window-hs-profile-rename-named = Rename { $name }
window-hs-profile-name = New name for { $name }
window-hs-profile-save = Save
window-hs-profile-number = A name cannot be a number only: a number is the profile's place.
window-hs-profiles-note = The name shows in the headset's menu. An empty name brings back the number.

## Pairing

window-pair-title = Pairing is open
window-pair-left = { $time } left
window-pair-wait = Open Prostir in the new headset near this PC. The headset finds the PC and either shows a code or asks for one from here.
window-pair-waiting = Waiting for the new headset
window-pair-type = { $name } shows a code. Type it here:
window-pair-pc-code = Type this code in the headset:
window-pair-code = Code from the headset
window-pair-go = Pair
window-pair-checking = Checking the code
window-pair-stop = Stop pairing
window-pair-wrong = The code did not match. Check the digits on the headset and type it again.
window-pair-done = { $name } is paired. It connects by itself from now on.
window-pair-expired = Pairing time is over. Open it again when the headset is near.
window-pair-reopen = Open again
window-pair-close = Close
# $address (String): the PC's addresses, separated by commas
window-pair-address = If the headset does not find the PC by itself, type the PC's address in it: { $address }
window-pair-firewall = The headset does not see the PC? Allow ports 47830/tcp and 47831/udp in the firewall.

## Sound

window-snd-title = Sound
window-snd-sub = The PC's sound and the headset microphone.
window-snd-where = Where the PC's sound plays
window-snd-where-desc = In the headset: the PC's speakers are quiet while the headset is connected.
window-snd-volume = Volume
window-snd-volume-desc = The same as on the headset's buttons and in the system.
window-snd-volume-off = The volume can be changed while a headset is connected.
window-snd-volume-headset = Changed with the headset's buttons.
window-snd-mic = Headset microphone on the PC
window-snd-mic-desc = Programs on the PC see it as the "Prostir" microphone.
window-snd-mic-ready = The "Prostir" microphone is ready
window-snd-mic-off = Off: programs on the PC do not see the headset microphone
window-snd-mic-in-headset = Switched on and off in the headset.
window-snd-mic-waiting = The microphone appears once a headset connects.
window-snd-mic-not-allowed = The headset has not allowed the microphone. Allow it for Prostir in the headset's settings, among the app permissions.
window-snd-mic-failed = The microphone could not be made: { $reason }

## Quality

window-q-title = Quality
window-q-sub = How the monitors are encoded for the headset.
window-q-bitrate = Bitrate
window-q-bitrate-desc = { $n } Mbit/s for each monitor now. More is sharper and needs a faster network.
window-q-mbit = { $n }
window-q-sharp = Sharper text
window-q-sharp-desc = A slower encoder setting that keeps small text clearer.
window-q-codec = Codec
window-q-codec-desc = HEVC where the PC and the headset can, else H.264.
window-q-cpu = Encode on the processor
window-q-cpu-desc = Even where the graphics card could.
window-q-using-gpu = Now: { $codec } on the graphics card
window-q-using-cpu = Now: { $codec } on the processor
window-q-using-none = Starts with the monitors.

## Look

window-look-title = Look
window-look-sub = Theme, palette and language of this window.
window-look-theme = Theme
window-look-system = As in the system
window-look-light = Light
window-look-dark = Dark
window-look-now-light = The system is light now.
window-look-now-dark = The system is dark now.
window-look-now-none = The system does not say which it prefers: dark is shown.
window-look-palette = Palette
window-look-palette-ink = Prostir
window-look-palette-graphite = Neutral
window-look-palette-desc = Prostir has a touch of blue. Neutral is grey only, so nothing tints the monitors.
window-look-lang = Language
window-look-lang-more = More languages: put a .ftl file into { $dir }
window-look-local = Prostir is not running: the choice holds while this window is open.
window-look-file = Theme file
window-look-file-ok = In use. Changes to the file show at once, no restart needed.
window-look-file-none = There is no file: the built-in theme is in use. Create one to change colours or sizes.
window-look-file-issues = These values were not read; the built-in ones are used instead:
window-look-file-contrast = Too little contrast (text, and the edges of controls):
window-look-create = Create the file
window-look-open = Open the folder
window-issue-not-toml = The file does not read as TOML: { $why }
window-issue-format = format = { $value }: this window reads format 1 only, so none of the file is used.
window-issue-bad = { $at } = { $value } does not fit here
window-issue-unknown = { $at } is not a known key
window-issue-font = { $at }: the font cannot be read ({ $why })
window-issue-unreadable = The file cannot be read: { $why }
window-contrast-line = { $scheme }: { $fg } on { $bg } is { $ratio }:1, needs { $need }:1
window-scheme-light = Light
window-scheme-dark = Dark

## First run

window-fr-title = First run
window-fr-step = Step { $n } of { $total }
window-fr-back = Back
window-fr-next = Next
window-fr-done = Done
window-fr-1-title = Install Prostir in the headset
window-fr-1-body = Turn on developer mode in the headset, connect it with a USB cable and install prostir.apk from the same release as Prostir on this PC.
window-fr-1-link = Open the releases page
window-fr-2-title = Open Prostir in the headset near this PC
window-fr-2-body = The headset and the PC need to be on the same Wi-Fi network. The headset finds this PC by itself.
window-fr-2-looking = Looking for the headset on the network
window-fr-2-found = { $name } sees this PC
window-fr-2-closed = Pairing is closed, so a new headset cannot find this PC.
window-fr-2-asks = The headset asks for a code as soon as it finds this PC.
window-fr-3-title = Pair with a code
window-fr-3-body = The code is needed once: after that, this headset connects by itself.
window-fr-3-wait = When the headset asks for a code, the PC shows it here.
window-fr-4-title = Start Prostir with the system?
window-fr-4-body = Then the headset finds the PC right after you log in. No windows and no extra work: while there is no headset, Prostir only waits.

## Messages after an action

window-notice-failed = Did not work: { $why }
window-notice-timeout = Prostir did not answer in time. Try again.
window-notice-not-running = Prostir is not running, so the change was not saved.
window-notice-broken = Prostir answered something this window does not understand: { $why }
window-notice-close = Close the message

### Headset app

## Words used in many places

headset-off = Off
headset-on = On
headset-no = Off
headset-yes = On
headset-show = Show
headset-hide = Hide
headset-cancel = Cancel
headset-connect = Connect
headset-menu = Menu
headset-focus = Focus
headset-cinema = Cinema
headset-line-up = Line up
# $value (Number): a percentage, 40 to 100
headset-percent = { $value } %

## The header and the tabs

headset-status-update-needed = Update needed
headset-status-pairing = Pairing…
# $pc (String): the PC's name
headset-status-connected = Connected to { $pc }
headset-status-searching = Looking for a PC…
headset-change-pc = Change PC
headset-tab-quick = Quick
headset-tab-monitors = Monitors
headset-tab-profiles = Profiles
headset-tab-settings = Settings

## The palm menu

headset-wrist-in-front = In front
headset-wrist-mouse = Mouse?
headset-wrist-screenshot = Screenshot
# $percent (Number): the headset's battery, 0 to 100
headset-battery-level = { $percent } %
# $percent (Number): the headset's battery, 0 to 100
headset-battery-charging = { $percent } % · charging

## Before a PC: the welcome and the PCs found

headset-welcome-title = Prostir shows your PC's monitors
# $site (String): Prostir's site, where the PCs it runs on are listed
headset-welcome-pc-app = Which PCs work: { $site }
headset-welcome-install = Install Prostir on the PC
headset-welcome-start = Start it
headset-welcome-start-note = The Prostir icon appears in the tray.
headset-welcome-finds = The headset finds the PC
headset-welcome-finds-wifi = On the same Wi‑Fi network.
headset-welcome-finds-wifi-usb = On the same Wi‑Fi network, or over a USB cable.
headset-connect-searching = Looking for PCs on the network…
headset-connect-found = Found
headset-found-cable = cable
headset-connect-blocked = is on the network, but the connection is blocked.
headset-connect-allow-port = Allow TCP port 47830 in the PC's firewall.
headset-connect-type-ip = Type an IP address
headset-connect-new-title = No PC with Prostir found
headset-connect-new-pair = On the PC, open the Prostir menu and choose "Pair a new headset".
headset-connect-or-type = Or type the PC's address: the Prostir window on the PC shows it.
headset-connect-or-usb = Or connect the headset to the PC with a USB cable.
headset-connect-silent-title = The PC does not answer
headset-connect-silent-check = Check that Prostir runs on the PC and that the PC is on the same network.
# Shown after a PC's name in the list of PCs found
headset-connect-pairing-open = pairing open
headset-key-delete = Delete
headset-key-done = Done

## Pairing

# $pc (String): the PC's name
headset-pairing-with = Pairing with { $pc }
headset-pairing-failed = The code did not match. Try again
headset-pairing-type-on-pc = Type this code into the Prostir window on the PC:
headset-pairing-once = This is needed only once per PC.
headset-pairing-other-way-type-here = The other way: code on the PC, type it here
headset-pairing-type-pc-code = Type the code the PC shows
headset-pairing-where-pc-code = (a notification or the Prostir tray icon):
headset-pairing-other-way-type-on-pc = The other way: code in the headset, type it on the PC
headset-offer-new = A new PC
headset-offer-wrong-codes = The code did not match several times
headset-offer-forgotten = The PC no longer knows this headset
headset-offer-pc-refused = Pairing did not start on the PC
headset-offer-own-pc = Pair the headset only with your own PC.
headset-offer-code = You will type a code from one device into the other.
headset-offer-pair = Pair
headset-offer-try-again = Try again
headset-offer-pair-again = Pair again
headset-offer-not-now = Not now

## Versions that cannot connect

headset-update-headset = Update the app in the headset
headset-update-pc = Update Prostir on the PC
# $pc (String): the PC app's version; $headset (String): this app's version
headset-update-versions = The PC has { $pc }, the headset has { $headset }.
headset-update-versions-differ = Different versions cannot connect.
headset-update-pc-from = Download the new PC version from
headset-update-connects-by-itself = The headset connects by itself once the versions match.
headset-store-update-library = Update Prostir in the headset's library:
headset-store-update-apk = Install the new APK from
# $store (String): the name of the store this build comes from
headset-store-not-confirmed = Could not confirm Prostir came from { $store }

## The first-run tips

headset-intro-title = Your monitors are here. A few tips:
headset-intro-menu = Menu
headset-intro-menu-text = The round button under the main monitor, or the menu or X button on the left controller.
headset-intro-move = Move a monitor
headset-intro-move-text = Hold grip and point. With hands: pinch and hold for half a second.
headset-intro-line-up = Line up
headset-intro-line-up-text = The A button closes the seams between monitors. One let go next to another sticks to it.
headset-intro-finger = With a finger
headset-intro-finger-text = With hands in view, just touch the menu like a tablet.
headset-intro-off = Headset off
headset-intro-off-text = After 10 s, windows, sound and microphone go back to the PC.
headset-intro-done = Got it

## Quick

headset-profiles = Profiles
headset-profile-n = Profile { $n }
headset-profiles-all = All profiles
headset-profiles-none-short = No profiles yet
headset-layout-lying = Lying down
headset-quick-in-front = In front of me
headset-quick-in-front-state = Turn everything to me
headset-keyboard-window = Keyboard window
headset-pc-sound = PC sound
headset-sound-in-headset = In the headset
headset-sound-on-pc = On the PC
headset-microphone = Microphone
headset-quick-mic-headset = The headset's
headset-quick-mic-pc = The PC's
headset-quick-detach = Detach window
headset-quick-detach-full = Three already: close one
headset-quick-detach-state = The PC's active window apart
headset-quick-window-here = Window here
headset-quick-window-here-state = To the monitor in front
headset-quick-screenshot = Screenshot
headset-quick-screenshot-state = Of the monitor in front

## Monitors

# $n (String): the monitor's number, as the PC's display settings show it
headset-monitor-n = Monitor { $n }
# $n (Number): the window panel's number, from 1
headset-window-n = Window { $n }
headset-place-left = left
headset-place-middle = middle
headset-place-right = right
headset-place-above = above
headset-place-below = below
headset-monitor-remove-confirm = Remove? Press again
headset-monitor-primary = Primary
headset-monitor-make-primary = Make primary
headset-pc-display = PC display
# $name (String): the PC's name for its display (HDMI-A-1)
headset-pc-display-named = Display { $name }
headset-laptop-screen = Laptop screen
headset-pc-display-not-connected = not connected
headset-pc-display-shown = Shown
headset-pc-display-hidden = Hidden
headset-pc-displays = PC displays
headset-pc-display-limit = the PC's limit reached
# $page (Number): the page shown; $pages (Number): how many pages there are
headset-page-of = Page { $page } of { $pages }
headset-scale = Scale
headset-monitors-line-up = Line up: close the seams (A button)
headset-add-monitor = Add a monitor
# $count (Number): how many monitors the PC makes at most
headset-monitors-at-most =
    { $count ->
        [one] At most { $count } monitor
       *[other] At most { $count } monitors
    }
headset-layout = Layout
headset-layout-around = Around me
headset-layout-wall = Wall
headset-curve = Curve
headset-curve-flat = Flat
headset-curve-gentle = Gentle
headset-curve-strong = Strong
headset-follow-head = Follow my head
headset-magnet = Stick together
headset-drag = Drag
headset-drag-one = One monitor
headset-drag-all = All together
headset-cinema-screen = Cinema screen
headset-profile-replace = Replace?

## Profiles

headset-profile-new = New profile
headset-profile-save-here = Save here
headset-profile-delete-confirm = Press again to delete
headset-profiles-none = Arrange the monitors and press "New profile": the arrangement is kept, one press away.
# $count (Number): how many profiles there can be
headset-profiles-at-most =
    { $count ->
        [one] At most { $count } profile
       *[other] At most { $count } profiles
    }
headset-profiles-rename-hint = A profile's name can be changed in the Prostir window on the PC.
# $name (String): the profile, as the menu named it ("Profile 2")
headset-note-profile-deleted = Profile "{ $name }" deleted
# $count (Number): how many profiles there can be
headset-note-profiles-full =
    { $count ->
        [one] Already { $count } profile: delete one first
       *[other] Already { $count } profiles: delete one first
    }

## Settings

headset-section-look = Look
headset-section-controls = Controls
headset-section-sound = Sound
headset-section-language = Language
headset-section-pc = PC
headset-section-paired = Paired PCs
headset-section-help = Help

## Settings: look

headset-background = Background
headset-background-black = Black
headset-background-room = Room
headset-background-dimmed = Dimmed
headset-background-own = Own photo
headset-background-own-where = Own photo: a 360° picture saved as
headset-sky-night = Night
headset-sky-sunset = Sunset
headset-sky-space = Space
headset-sky-aurora = Aurora
headset-focus-dim-others = Dim the others
headset-focus-note = Monitors without the mouse get darker.
headset-keyboard-window-note = Shows the real desk; put it over the keyboard.
headset-rest = Rest reminders
headset-rest-every-20 = Every 20 min
headset-rest-hourly = Hourly
headset-brightness = Monitor brightness

## Settings: controls

headset-hands = Hands
headset-hands-use = Show and use
headset-gaze-mouse = Mouse follows the gaze
headset-gaze-mouse-note = Look at another monitor and the mouse moves there.
headset-gaze-mouse-unavailable = This PC does not let the mouse be moved.
headset-menu-size = Menu size
headset-menu-size-normal = Normal
headset-menu-size-large = Large
headset-palm-menu = Palm menu
headset-palm-menu-left = Left hand
headset-palm-menu-right = Right hand
headset-display-rate = Headset display rate
headset-display-rate-normal = { $hz } Hz
headset-display-rate-saver = { $hz } Hz, lasts longer
headset-display-rate-note = The video runs at 60 frames a second, so { $hz } Hz keeps all of it.
headset-display-rate-now = The display shows { $hz } Hz now.
headset-menu-button = Round menu button by the monitor

## Settings: sound

headset-sound-plays = PC sound plays
headset-sound-apps-hear = PC apps hear
headset-sound-mic-headset = The headset's mic
headset-sound-mic-pc = The PC's mic
headset-sound-mic-denied-1 = Prostir may not use the microphone. Allow it in the
headset-sound-mic-denied-2 = headset settings (app permissions).
headset-sound-ask-again = Ask again
headset-sound-from-screen = Sound from the screen
headset-sound-from-screen-note = Sound comes from the monitor with the window playing it.
headset-sound-about-1 = While the headset is connected, PC sound plays in it
headset-sound-about-2 = and PC apps hear the headset microphone.
headset-sound-about-3 = The microphone turns on only while an app records.
headset-sound-about-4 = Take the headset off and both return to the PC.

## Settings: language, time and date

headset-language-title = Menu language · Мова
headset-language-like-pc = Same as PC
headset-language-like-system = Same as the system
headset-language-more = More…
# $language (String): the language's code (de, pt-BR)
headset-language-problems = Some lines of the { $language } language file could not be read: they show in English
# $language (String): the language's code (de, pt-BR)
headset-language-missing = No file for the language { $language }: the menu stays in English
headset-time = Time
headset-date = Date
headset-date-like-menu = As the menu

## Settings: the PC

headset-pc-not-connected = Available while a PC is connected.
headset-pc-quality = Video quality
headset-pc-quality-economy = Economy
headset-pc-quality-normal = Normal
headset-pc-quality-high = High
headset-pc-encoding-on = Encoding on
headset-pc-gpu = Graphics card
headset-pc-cpu = Processor
headset-pc-codec = Codec
headset-pc-codec-note = HEVC: the sharpest text. H.264: less memory on the PC.
headset-pc-sharp = Sharper text (+2 ms)
headset-pc-notifications = PC notifications in the headset
headset-no-limits = No limits
headset-no-limits-note = More monitors and displays than Prostir advises. Frames may be skipped, and the headset may get warm and run down faster.
headset-paired-pcs = Paired PCs
headset-paired-none = None
headset-paired-forget-confirm = Forget? Again
headset-paired-forget = Forget
# $count (Number): how many more paired PCs there are
headset-paired-more = And { $count }

## Settings: help

headset-hint-finger = Finger: touch a button, or drag the menu by an empty spot
headset-hint-hand = Hand from afar: pinch to press, pinch and hold to move
headset-hint-controller = Controller: trigger presses, grip moves
headset-hint-stick = Stick while holding: distance and size. A: line up
headset-help-show-tips = Show the tips again

## Around the monitors

headset-attach = Attach
headset-adding-monitor = Adding a monitor…

## Messages

headset-ask-waiting = Working…
headset-rest-eyes = Rest your eyes: look into the distance for 20 seconds
headset-rest-hour = An hour in the headset: time for a short break
# $path (String): where the PC saved the picture
headset-toast-screenshot-saved = Screenshot saved: { $path }
# $name (String): the profile, as the menu names it ("Profile 2")
headset-toast-profile-saved = Saved to "{ $name }"
# $name (String): the profile, as the menu names it ("Profile 2")
headset-toast-profile-switched = Switched to "{ $name }"
# $error (String): what went wrong, as the system says it
headset-error-send = Could not send to the PC: { $error }
headset-error-not-connected = Not connected to a PC
# $name (String): the display's name; $size (String): its size, 5120×1440;
# $codec (String): H264 or H265
headset-error-too-big = { $name }: { $size } ({ $codec }) is too large for this headset to show
# $error (String): what went wrong, as the system says it
headset-error-cameras = Cameras unavailable: { $error }
headset-error-cameras-lost = The cameras stopped working: "Room" and "Dimmed" come back once the app starts again.
headset-error-keyboard-window = The keyboard window cannot be shown now: it is tried again when the app starts next.
## A monitor's picture, under its name in the menu
headset-video-trouble = No picture, recovering…
# $codec (String): the video's codec, H.264 or H.265
headset-video-no-decoder = this headset has no { $codec } decoder
# $codec (String): H.264 or H.265; $size (String): the picture's size, 7680 × 2160
headset-video-bad-size = this headset does not decode { $codec } at { $size }
headset-video-no-surface = the headset could not show this picture
headset-video-stopped = the video decoder stopped
headset-video-undecodable = the headset could not decode this picture
# $reason (String): why, one of the lines above
headset-video-not-shown = Not shown: { $reason }
# $name (String): the monitor, as the menu names it ("Monitor 2"); $reason (String): why, one of the lines above
headset-video-failed = { $name } is not shown: { $reason }
# $typed (String): what was typed
headset-error-not-ip = «{ $typed }»: is not an IP address
# $error (String): what went wrong, as the system says it
headset-error-save-settings = Could not save the settings: { $error }
# $error (String): what went wrong, as the system says it
headset-error-save-desk = Could not save the arrangement: { $error }
# $error (String): what went wrong, as the system says it
headset-error-save-profiles = Could not save the profiles: { $error }
# $error (String): what went wrong, as the system says it
headset-error-save-keyboard = Could not save where the keyboard window lies: { $error }
# $error (String): what went wrong, as the system says it
headset-error-save-files = Could not save the changes: { $error }
headset-pc-usb = PC (USB)
headset-bye-pc-quit = The PC app was closed
headset-bye-forgotten = The PC no longer knows this headset. Pair them again
headset-bye-pc-ended = The PC ended the connection
headset-bye-silence = No connection to the PC

## Under the button pressed in the menu

headset-note-monitor-added = Monitor added
headset-note-monitor-removed = Monitor removed
headset-note-window-closed = The window is back on the monitors
headset-note-primary = Now the primary monitor
headset-note-display-shown = The PC display is shown
headset-note-display-hidden = The PC display is hidden
headset-note-scale = Scale changed
headset-note-no-limits-on = More monitors and displays allowed
headset-note-no-limits-off = Recommended limits back on
headset-note-sound-headset = PC sound plays in the headset
headset-note-sound-pc = PC sound plays on the PC
headset-note-encoding = Video settings applied
headset-note-window-detached = Window detached
headset-note-window-here = Window moved here
headset-note-layout = Monitors laid out again
headset-note-lined-up = Seams closed
headset-note-in-front = Everything is in front of you
headset-note-mic-headset = PC apps hear the headset microphone
headset-note-mic-pc = PC apps hear the PC microphone
headset-note-mic-ask = The system will ask for microphone access
headset-note-mic-allowed = Microphone access allowed
headset-note-mic-denied = No access to the microphone
headset-note-language = Menu language changed
# $pc (String): the PC's name
headset-note-pc-forgotten = { $pc } removed from paired PCs
# $pc (String): the PC's name
headset-note-paired = Paired with { $pc }
headset-note-no-monitor-in-front = No monitor in front of you

## What the PC answered when a request failed

headset-failure-not-connected = No connection to the PC
headset-failure-disconnected = The connection to the PC broke
headset-failure-timed-out = The PC does not answer
headset-failure-unsupported = The PC cannot do this
headset-failure-not-allowed = Not allowed for this headset
headset-failure-unknown-display = That display is gone
headset-failure-limit = The limit is reached
headset-failure-bad-size = A size the PC does not make or the headset cannot show
headset-failure-last-display = The last monitor stays
headset-failure-no-window = No active window on the PC
headset-failure-desktop = The PC's desktop did not do it
headset-failure-encoder = The PC's encoder did not start
headset-failure-capture = The picture could not be taken
headset-failure-sound = The sound could not be switched
headset-failure-not-on-displays = The mouse is not on the monitors
headset-failure-no-such-profile = No such profile
headset-failure-bad-argument = A value out of range
headset-failure-other = It did not work

## The palm menu's date and the 12-hour clock

# $weekday (String): weekday-0 to weekday-6; $day (Number): the day of the month;
# $month (String): month-0 to month-11
headset-date-format = { $weekday }, { $month } { $day }
headset-weekday-0 = Sun
headset-weekday-1 = Mon
headset-weekday-2 = Tue
headset-weekday-3 = Wed
headset-weekday-4 = Thu
headset-weekday-5 = Fri
headset-weekday-6 = Sat
headset-month-0 = Jan
headset-month-1 = Feb
headset-month-2 = Mar
headset-month-3 = Apr
headset-month-4 = May
headset-month-5 = Jun
headset-month-6 = Jul
headset-month-7 = Aug
headset-month-8 = Sep
headset-month-9 = Oct
headset-month-10 = Nov
headset-month-11 = Dec
# $time (String): 2:05; $half: am or pm
headset-time-12h =
    { $half ->
        [am] { $time } AM
       *[pm] { $time } PM
    }
