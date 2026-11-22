# Prostir settings window, English. Every other language falls back to this file
# message by message, so it has every message.
# A new language: copy this file to ~/.local/share/prostir/lang/settings/<code>.ftl
# (for example de.ftl), translate the text after each "=", and reopen the window.

language-name = English

## Pages

nav-overview = Overview
nav-monitors = Monitors
nav-headsets = Headsets
nav-sound = Sound
nav-quality = Quality
nav-look = Look
quit-prostir = Quit Prostir
close-note-title = Prostir keeps running
close-note = Prostir keeps running in the tray. To open it again: the Prostir icon in the menu or in the tray.
nav-first-run = First run

## The status line under the pages

side-loading = Connecting to Prostir
side-connected = { $name } connected
side-taken-off = { $name } taken off
side-connecting = Connecting { $name }
side-waiting = Waiting for a headset
side-failed = A monitor was not added
side-not-running = Prostir is not running
side-silent = Prostir does not answer
side-old = Prostir is older than this window
side-no-headsets = No headsets yet

## Overview

ov-title = Overview
ov-connected-title = Headset connected
ov-connected-wifi = { $name } over Wi-Fi. Monitors in the headset: { $count }.
ov-connected-usb = { $name } over a USB cable. Monitors in the headset: { $count }.
ov-wifi-power-save = Power saving is on for this PC's Wi-Fi ({ $interface }). It can hold the picture back by tens of milliseconds. A cable helps, or turning power saving off for this connection.
ov-taken-off-title = Headset taken off
ov-taken-off-body = { $name } is paused: the monitors wait until the headset is on again.
ov-battery = headset battery
ov-charging = charging
ov-fact-sound = PC sound
ov-fact-mic = Headset microphone
ov-fact-profile = Profile
ov-profile-none = None
ov-connecting-title = Connecting { $name }
ov-connecting-body = The headset found this PC. Setting up an encrypted connection.
ov-waiting-title = Waiting for a headset
ov-waiting-body = Open Prostir in the headset near this PC. The headset finds the PC by itself when both are on the same network.
ov-failed-title = A monitor was not added
ov-failed-body = { $name }: { $reason } The other monitors work.
ov-failed-no-reason = { $name } was not added. The other monitors work.
ov-retry = Try again
ov-open-monitors = Go to monitors
ov-no-headsets-title = Pair the first headset
ov-no-headsets-body = This PC does not know any headset yet. First run takes you through it in four steps.
ov-no-headsets-go = Start first run
ov-pair-new = Pair a new headset
ov-problems = Needs attention
ov-monitors = Monitors in the headset
ov-monitors-empty = Monitors appear here once a headset connects.
ov-pc = This PC
ov-pc-name = Name on the network
ov-pc-desktop = Desktop
ov-autostart = Start with the system
ov-autostart-desc = Prostir waits for the headset after you log in. No window opens then.
ov-autostart-here-not = Set up with the desktop on this PC.
ov-counting = Count this PC among users
ov-counting-desc = Once a day one request that says only that Prostir was used: no name, no address, no settings.

## The question about counting, until it is answered

count-ask-title = Count this PC among Prostir's users?
count-ask-body = Prostir is free and has no ads. The number of PCs it runs on is the only way to know whether it is worth developing further. Only the date is sent, once a day (and once, that Prostir was installed), with nothing about you.
count-ask-later = Sent through GoatCounter. Nothing is sent unless you answer Yes. The answer can be changed at any time on the Overview page or in the tray.
count-ask-yes = Yes
count-ask-no = No
count-ask-yes-name = Yes, count this PC
count-ask-no-name = No, do not count this PC
count-ask-privacy = Privacy policy
count-ask-privacy-url = https://prostir.ishawyha.dev/privacy.html#counting

## How a monitor is doing

state-streaming = Streaming
state-starting = Starting
state-failed = Not added
state-absent = Not connected
state-kept = Waiting for the headset
state-shown = Shown in the headset
state-hidden = Hidden

## Short values

sound-in-headset = In the headset
sound-on-pc = On the PC
on = On
off = Off
percent = { $n }%
decimal-separator = .

## Prostir is not running

loading-title = Connecting to Prostir
nr-title = Prostir is not running
nr-body = The settings come from Prostir running in the background. Start it, and this window updates by itself.
nr-start = Start Prostir
nr-starting = Starting Prostir
nr-failed-title = Prostir did not start
nr-failed-body = Run prostir in a terminal to see what stops it.
nr-no-program = The prostir program was not found. Check that Prostir is installed.
nr-old-title = This Prostir is older than the window
nr-old-body = Prostir is running, but this version does not work with the settings window. Update Prostir, then try again.
nr-retry = Try again
nr-silent-title = Prostir does not answer
nr-silent-body = Prostir is running but did not answer this window. The window asks again by itself.
nr-no-bus = There is no D-Bus session bus, so this window cannot reach Prostir.

## Monitors

mon-title = Monitors
mon-sub = As the PC's desktop sees them: the mouse moves between monitors where they touch.
mon-flat = Flat
mon-arc = Around you
mon-hint-flat = Drag a monitor, or select it and move it with the arrow keys. It sticks to its neighbour.
mon-hint-arc = How the monitors stand around you. Drag an empty spot to turn the view.
mon-empty = No monitors to show yet.
mon-selected = Selected monitor
mon-none = Select a monitor on the map.
mon-size = { $w } × { $h }
mon-size-scaled = { $w } × { $h }, scale { $scale }%
mon-size-at-scale = { $w } × { $h } · { $scale }%
mon-scale = Scale
mon-main = Main monitor
mon-main-desc = The panel and new windows go here.
mon-remove = Remove monitor
mon-remove-named = Remove { $name }
mon-in-headset = In the headset
mon-shown = Shown
mon-hidden = Hidden
mon-laptop = Laptop screen
mon-pc-screen = PC screen
mon-add = Add a monitor
mon-add-desc = A new monitor appears next to the others.
mon-placing = { $name }: being placed…
mon-add-full = No more monitors fit: { $max } are in the headset already.
mon-add-offline = The monitor appears in the headset when it connects.
mon-failed = Not added: { $reason }
mon-kept = Waiting for the headset
mon-kept-desc = Kept for the next headset: they appear when it connects.

## Headsets

hs-title = Headsets
hs-sub = Headsets this PC trusts. A new headset needs a code only once.
# $address (String): the PC's addresses, separated by commas
hs-address = This PC's address, for a headset that does not find it by itself: { $address }
hs-now = Connected now
hs-last = Last connected { $when }
hs-never = Last connected: not known
hs-app = Prostir { $app }, key { $key }
hs-key = Key { $key }
ago-now = just now
ago-minutes = { $n ->
    [one] { $n } minute ago
   *[other] { $n } minutes ago
}
ago-hours = { $n ->
    [one] { $n } hour ago
   *[other] { $n } hours ago
}
ago-days = { $n ->
    [one] { $n } day ago
   *[other] { $n } days ago
}
hs-forget = Forget
hs-forget-named = Forget { $name }
hs-forget-ask = Forget { $name }? To connect again, it will need a code.
hs-cancel = Cancel
hs-empty-title = No paired headsets yet
hs-empty-body = Open Prostir in the headset near this PC and pair them with a code.
hs-pair-hint = To pair a new headset, open Prostir in it near this PC: the PC shows a code here to type in the headset.
hs-guest-name = { $name } (guest)
hs-profiles = Profiles in { $name }
hs-profile-n = Profile { $n }
hs-profile-in-use = In use now
hs-profile-rename = Rename
hs-profile-rename-named = Rename { $name }
hs-profile-name = New name for { $name }
hs-profile-save = Save
hs-profiles-note = The name shows in the headset's menu. An empty name brings back the number.

## Pairing

pair-title = Pairing is open
pair-left = { $time } left
pair-wait = Open Prostir in the new headset near this PC. The headset finds the PC and either shows a code or asks for one from here.
pair-waiting = Waiting for the new headset
pair-type = { $name } shows a code. Type it here:
pair-pc-code = Type this code in the headset:
pair-code = Code from the headset
pair-go = Pair
pair-checking = Checking the code
pair-stop = Stop pairing
pair-wrong = The code did not match. Check the digits on the headset and type it again.
pair-done = { $name } is paired. It connects by itself from now on.
pair-expired = Pairing time is over. Open it again when the headset is near.
pair-reopen = Open again
pair-close = Close
# $address (String): the PC's addresses, separated by commas
pair-address = If the headset does not find the PC by itself, type the PC's address in it: { $address }
pair-firewall = The headset does not see the PC? Allow ports 47830/tcp and 47831/udp in the firewall.

## Sound

snd-title = Sound
snd-sub = The PC's sound and the headset microphone.
snd-where = Where the PC's sound plays
snd-where-desc = In the headset: the PC's speakers are quiet while the headset is connected.
snd-volume = Volume
snd-volume-desc = The same as on the headset's buttons and in the system.
snd-volume-off = The volume can be changed while a headset is connected.
snd-volume-headset = Changed with the headset's buttons.
snd-mic = Headset microphone on the PC
snd-mic-desc = Programs on the PC see it as the "Prostir" microphone.
snd-mic-ready = The "Prostir" microphone is ready
snd-mic-off = Off: programs on the PC do not see the headset microphone
snd-mic-in-headset = Switched on and off in the headset.
snd-mic-waiting = The microphone appears once a headset connects.
snd-mic-not-allowed = The headset has not allowed the microphone. Allow it for Prostir in the headset's settings, among the app permissions.
snd-mic-failed = The microphone could not be made: { $reason }

## Quality

q-title = Quality
q-sub = How the monitors are encoded for the headset.
q-bitrate = Bitrate
q-bitrate-desc = { $n } Mbit/s for each monitor now. More is sharper and needs a faster network.
q-mbit = { $n }
q-sharp = Sharper text
q-sharp-desc = A slower encoder setting that keeps small text clearer.
q-codec = Codec
q-codec-desc = HEVC where the PC and the headset can, else H.264.
q-cpu = Encode on the processor
q-cpu-desc = Even where the graphics card could.
q-using-gpu = Now: { $codec } on the graphics card
q-using-cpu = Now: { $codec } on the processor
q-using-none = Starts with the monitors.

## Look

look-title = Look
look-sub = Theme, palette and language of this window.
look-theme = Theme
look-system = As in the system
look-light = Light
look-dark = Dark
look-now-light = The system is light now.
look-now-dark = The system is dark now.
look-now-none = The system does not say which it prefers: dark is shown.
look-palette = Palette
look-palette-ink = Prostir
look-palette-graphite = Neutral
look-palette-desc = Prostir has a touch of blue. Neutral is grey only, so nothing tints the monitors.
look-lang = Language
look-lang-more = More languages: put a .ftl file into { $dir }
look-local = Prostir is not running: the choice holds while this window is open.
look-file = Theme file
look-file-ok = In use. Changes to the file show at once, no restart needed.
look-file-none = There is no file: the built-in theme is in use. Create one to change colours or sizes.
look-file-issues = These values were not read; the built-in ones are used instead:
look-file-contrast = Too little contrast (text, and the edges of controls):
look-create = Create the file
look-open = Open the folder
issue-not-toml = The file does not read as TOML: { $why }
issue-format = format = { $value }: this window reads format 1 only, so none of the file is used.
issue-bad = { $at } = { $value } does not fit here
issue-unknown = { $at } is not a known key
issue-font = { $at }: the font cannot be read ({ $why })
issue-unreadable = The file cannot be read: { $why }
contrast-line = { $scheme }: { $fg } on { $bg } is { $ratio }:1, needs { $need }:1
scheme-light = Light
scheme-dark = Dark

## First run

fr-title = First run
fr-step = Step { $n } of { $total }
fr-back = Back
fr-next = Next
fr-done = Done
fr-1-title = Install Prostir in the headset
fr-1-body = Turn on developer mode in the headset, connect it with a USB cable and install prostir.apk from the same release as Prostir on this PC.
fr-1-link = Open the releases page
fr-2-title = Open Prostir in the headset near this PC
fr-2-body = The headset and the PC need to be on the same Wi-Fi network. The headset finds this PC by itself.
fr-2-looking = Looking for the headset on the network
fr-2-found = { $name } sees this PC
fr-2-closed = Pairing is closed, so a new headset cannot find this PC.
fr-2-asks = The headset asks for a code as soon as it finds this PC.
fr-3-title = Pair with a code
fr-3-body = The code is needed once: after that, this headset connects by itself.
fr-3-wait = When the headset asks for a code, the PC shows it here.
fr-4-title = Start Prostir with the system?
fr-4-body = Then the headset finds the PC right after you log in. No windows and no extra work: while there is no headset, Prostir only waits.

## Messages after an action

notice-failed = Did not work: { $why }
notice-timeout = Prostir did not answer in time. Try again.
notice-not-running = Prostir is not running, so the change was not saved.
notice-broken = Prostir answered something this window does not understand: { $why }
notice-close = Close the message
