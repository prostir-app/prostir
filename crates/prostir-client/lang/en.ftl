# The headset's texts, in English: complete, and behind every other language.
# Another language is a file <code>.ftl like this one in the app's folder
# Android/data/dev.prostir.client/files/lang/ (its own words where it has them,
# these where it does not).
#
# A comment `# $name` above a message says what its placeholders are. Plural forms
# follow the language's own rules ({ $count -> [one] ... *[other] ... }); NUMBER()
# is there for a number formatted another way.

language-name = English

## Words used in many places

off = Off
on = On
no = Off
yes = On
show = Show
hide = Hide
cancel = Cancel
connect = Connect
menu = Menu
focus = Focus
cinema = Cinema
line-up = Line up
# $value (Number): a percentage, 40 to 100
percent = { $value } %

## The header and the tabs

status-update-needed = Update needed
status-pairing = Pairing…
# $pc (String): the PC's name
status-connected = Connected to { $pc }
status-searching = Looking for a PC…
change-pc = Change PC
tab-quick = Quick
tab-monitors = Monitors
tab-profiles = Profiles
tab-settings = Settings

## The palm menu

wrist-in-front = In front
wrist-mouse = Mouse?
wrist-screenshot = Screenshot
# $percent (Number): the headset's battery, 0 to 100
battery-level = { $percent } %
# $percent (Number): the headset's battery, 0 to 100
battery-charging = { $percent } % · charging

## Before a PC: the welcome and the PCs found

welcome-title = Prostir shows your PC's monitors
# $site (String): Prostir's site, where the PCs it runs on are listed
welcome-pc-app = Which PCs work: { $site }
welcome-install = Install Prostir on the PC
welcome-start = Start it
welcome-start-note = The Prostir icon appears in the tray.
welcome-finds = The headset finds the PC
welcome-finds-wifi = On the same Wi‑Fi network.
welcome-finds-wifi-usb = On the same Wi‑Fi network, or over a USB cable.
connect-searching = Looking for PCs on the network…
connect-found = Found
connect-blocked = is on the network, but the connection is blocked.
connect-allow-port = Allow TCP port 47830 in the PC's firewall.
connect-type-ip = Type an IP address
connect-new-title = No PC with Prostir found
connect-new-pair = On the PC, open the Prostir menu and choose "Pair a new headset".
connect-or-type = Or type the PC's address: the Prostir window on the PC shows it.
connect-or-usb = Or connect the headset to the PC with a USB cable.
connect-silent-title = The PC does not answer
connect-silent-check = Check that Prostir runs on the PC and that the PC is on the same network.
# Shown after a PC's name in the list of PCs found
connect-pairing-open = pairing open
key-delete = Delete
key-done = Done

## Pairing

# $pc (String): the PC's name
pairing-with = Pairing with { $pc }
pairing-failed = The code did not match. Try again
pairing-type-on-pc = Type this code into the Prostir window on the PC:
pairing-once = This is needed only once per PC.
pairing-other-way-type-here = The other way: code on the PC, type it here
pairing-type-pc-code = Type the code the PC shows
pairing-where-pc-code = (a notification or the Prostir tray icon):
pairing-other-way-type-on-pc = The other way: code in the headset, type it on the PC
offer-new = A new PC
offer-wrong-codes = The code did not match several times
offer-forgotten = The PC no longer knows this headset
offer-pc-refused = Pairing did not start on the PC
offer-own-pc = Pair the headset only with your own PC.
offer-code = You will type a code from one device into the other.
offer-pair = Pair
offer-try-again = Try again
offer-pair-again = Pair again
offer-not-now = Not now

## Versions that cannot connect

update-headset = Update the app in the headset
update-pc = Update Prostir on the PC
# $pc (String): the PC app's version; $headset (String): this app's version
update-versions = The PC has { $pc }, the headset has { $headset }.
update-versions-differ = Different versions cannot connect.
update-pc-from = Download the new PC version from
update-connects-by-itself = The headset connects by itself once the versions match.
store-update-library = Update Prostir in the headset's library:
store-update-apk = Install the new APK from
# $store (String): the name of the store this build comes from
store-not-confirmed = Could not confirm Prostir came from { $store }

## The first-run tips

intro-title = Your monitors are here. A few tips:
intro-menu = Menu
intro-menu-text = The round button under the main monitor, or the menu or X button on the left controller.
intro-move = Move a monitor
intro-move-text = Hold grip and point. With hands: pinch and hold for half a second.
intro-line-up = Line up
intro-line-up-text = The A button closes the seams between monitors. One let go next to another sticks to it.
intro-finger = With a finger
intro-finger-text = With hands in view, just touch the menu like a tablet.
intro-off = Headset off
intro-off-text = After 10 s, windows, sound and microphone go back to the PC.
intro-done = Got it

## Quick

profiles = Profiles
profile-n = Profile { $n }
profiles-all = All profiles
profiles-none-short = No profiles yet
layout-lying = Lying down
quick-in-front = In front of me
quick-in-front-state = Turn everything to me
keyboard-window = Keyboard window
pc-sound = PC sound
sound-in-headset = In the headset
sound-on-pc = On the PC
microphone = Microphone
quick-mic-headset = The headset's
quick-mic-pc = The PC's
quick-detach = Detach window
quick-detach-full = Three already: close one
quick-detach-state = The PC's active window apart
quick-window-here = Window here
quick-window-here-state = To the monitor in front
quick-screenshot = Screenshot
quick-screenshot-state = Of the monitor in front

## Monitors

# $n (String): the monitor's number, as the PC's display settings show it
monitor-n = Monitor { $n }
# $n (Number): the window panel's number, from 1
window-n = Window { $n }
place-left = left
place-middle = middle
place-right = right
place-above = above
place-below = below
monitor-remove-confirm = Remove? Press again
monitor-primary = Primary
monitor-make-primary = Make primary
pc-display = PC display
# $name (String): the PC's name for its display (HDMI-A-1)
pc-display-named = Display { $name }
laptop-screen = Laptop screen
pc-display-not-connected = not connected
pc-display-shown = Shown
pc-display-hidden = Hidden
pc-displays = PC displays
pc-display-limit = the PC's limit reached
# $page (Number): the page shown; $pages (Number): how many pages there are
page-of = Page { $page } of { $pages }
scale = Scale
monitors-line-up = Line up: close the seams (A button)
add-monitor = Add a monitor
# $count (Number): how many monitors the PC makes at most
monitors-at-most =
    { $count ->
        [one] At most { $count } monitor
       *[other] At most { $count } monitors
    }
layout = Layout
layout-around = Around me
layout-wall = Wall
curve = Curve
curve-flat = Flat
curve-gentle = Gentle
curve-strong = Strong
follow-head = Follow my head
magnet = Stick together
drag = Drag
drag-one = One monitor
drag-all = All together
cinema-screen = Cinema screen
profile-replace = Replace?

## Profiles

profile-new = New profile
profile-save-here = Save here
profile-delete-confirm = Press again to delete
profiles-none = Arrange the monitors and press "New profile": the arrangement is kept, one press away.
# $count (Number): how many profiles there can be
profiles-at-most =
    { $count ->
        [one] At most { $count } profile
       *[other] At most { $count } profiles
    }
profiles-rename-hint = A profile's name can be changed in the Prostir window on the PC.
# $name (String): the profile, as the menu named it ("Profile 2")
note-profile-deleted = Profile "{ $name }" deleted
# $count (Number): how many profiles there can be
note-profiles-full =
    { $count ->
        [one] Already { $count } profile: delete one first
       *[other] Already { $count } profiles: delete one first
    }

## Settings

section-look = Look
section-controls = Controls
section-sound = Sound
section-language = Language
section-pc = PC
section-paired = Paired PCs
section-help = Help

## Settings: look

background = Background
background-black = Black
background-room = Room
background-dimmed = Dimmed
background-own = Own photo
background-own-where = Own photo: a 360° picture saved as
sky-night = Night
sky-sunset = Sunset
sky-space = Space
sky-aurora = Aurora
focus-dim-others = Dim the others
focus-note = Monitors without the mouse get darker.
keyboard-window-note = Shows the real desk; put it over the keyboard.
rest = Rest reminders
rest-every-20 = Every 20 min
rest-hourly = Hourly
brightness = Monitor brightness

## Settings: controls

hands = Hands
hands-use = Show and use
gaze-mouse = Mouse follows the gaze
gaze-mouse-note = Look at another monitor and the mouse moves there.
gaze-mouse-unavailable = This PC does not let the mouse be moved.
menu-size = Menu size
menu-size-normal = Normal
menu-size-large = Large
palm-menu = Palm menu
palm-menu-left = Left hand
palm-menu-right = Right hand
display-rate = Headset display rate
display-rate-normal = { $hz } Hz
display-rate-saver = { $hz } Hz, lasts longer
display-rate-note = The video runs at 60 frames a second, so { $hz } Hz keeps all of it.
display-rate-now = The display shows { $hz } Hz now.
menu-button = Round menu button by the monitor

## Settings: sound

sound-plays = PC sound plays
sound-apps-hear = PC apps hear
sound-mic-headset = The headset's mic
sound-mic-pc = The PC's mic
sound-mic-denied-1 = Prostir may not use the microphone. Allow it in the
sound-mic-denied-2 = headset settings (app permissions).
sound-ask-again = Ask again
sound-from-screen = Sound from the screen
sound-from-screen-note = Sound comes from the monitor with the window playing it.
sound-about-1 = While the headset is connected, PC sound plays in it
sound-about-2 = and PC apps hear the headset microphone.
sound-about-3 = The microphone turns on only while an app records.
sound-about-4 = Take the headset off and both return to the PC.

## Settings: language, time and date

language-title = Menu language · Мова
language-like-pc = Same as PC
language-like-system = Same as the system
language-more = More…
# $language (String): the language's code (de, pt-BR)
language-problems = Some lines of the { $language } language file could not be read: they show in English
# $language (String): the language's code (de, pt-BR)
language-missing = No file for the language { $language }: the menu stays in English
time = Time
date = Date
date-like-menu = As the menu

## Settings: the PC

pc-not-connected = Available while a PC is connected.
pc-quality = Video quality
pc-quality-economy = Economy
pc-quality-normal = Normal
pc-quality-high = High
pc-encoding-on = Encoding on
pc-gpu = Graphics card
pc-cpu = Processor
pc-codec = Codec
pc-codec-note = HEVC: the sharpest text. H.264: less memory on the PC.
pc-sharp = Sharper text (+2 ms)
pc-notifications = PC notifications in the headset
no-limits = No limits
no-limits-note = More monitors and displays than Prostir advises. Frames may be skipped, and the headset may get warm and run down faster.
paired-pcs = Paired PCs
paired-none = None
paired-forget-confirm = Forget? Again
paired-forget = Forget
# $count (Number): how many more paired PCs there are
paired-more = And { $count }

## Settings: help

hint-finger = Finger: touch a button, or drag the menu by an empty spot
hint-hand = Hand from afar: pinch to press, pinch and hold to move
hint-controller = Controller: trigger presses, grip moves
hint-stick = Stick while holding: distance and size. A: line up
help-show-tips = Show the tips again

## Around the monitors

attach = Attach
adding-monitor = Adding a monitor…

## Messages

ask-waiting = Working…
rest-eyes = Rest your eyes: look into the distance for 20 seconds
rest-hour = An hour in the headset: time for a short break
# $path (String): where the PC saved the picture
toast-screenshot-saved = Screenshot saved: { $path }
# $name (String): the profile, as the menu names it ("Profile 2")
toast-profile-saved = Saved to "{ $name }"
# $name (String): the profile, as the menu names it ("Profile 2")
toast-profile-switched = Switched to "{ $name }"
# $error (String): what went wrong, as the system says it
error-send = Could not send to the PC: { $error }
error-not-connected = Not connected to a PC
# $name (String): the display's name; $size (String): its size, 5120×1440;
# $codec (String): H264 or H265
error-too-big = { $name }: { $size } ({ $codec }) is too large for this headset to show
# $error (String): what went wrong, as the system says it
error-cameras = Cameras unavailable: { $error }
error-cameras-lost = The cameras stopped working: "Room" and "Dimmed" come back once the app starts again.
# $typed (String): what was typed
error-not-ip = «{ $typed }»: is not an IP address
# $error (String): what went wrong, as the system says it
error-save-settings = Could not save the settings: { $error }
# $error (String): what went wrong, as the system says it
error-save-desk = Could not save the arrangement: { $error }
# $error (String): what went wrong, as the system says it
error-save-profiles = Could not save the profiles: { $error }
# $error (String): what went wrong, as the system says it
error-save-keyboard = Could not save where the keyboard window lies: { $error }
# $error (String): what went wrong, as the system says it
error-save-files = Could not save the changes: { $error }
pc-usb = PC (USB)
bye-pc-quit = The PC app was closed
bye-forgotten = The PC no longer knows this headset. Pair them again
bye-pc-ended = The PC ended the connection
bye-silence = No connection to the PC

## Under the button pressed in the menu

note-monitor-added = Monitor added
note-monitor-removed = Monitor removed
note-window-closed = The window is back on the monitors
note-primary = Now the primary monitor
note-display-shown = The PC display is shown
note-display-hidden = The PC display is hidden
note-scale = Scale changed
note-no-limits-on = More monitors and displays allowed
note-no-limits-off = Recommended limits back on
note-sound-headset = PC sound plays in the headset
note-sound-pc = PC sound plays on the PC
note-encoding = Video settings applied
note-window-detached = Window detached
note-window-here = Window moved here
note-layout = Monitors laid out again
note-lined-up = Seams closed
note-in-front = Everything is in front of you
note-mic-headset = PC apps hear the headset microphone
note-mic-pc = PC apps hear the PC microphone
note-mic-ask = The system will ask for microphone access
note-mic-allowed = Microphone access allowed
note-mic-denied = No access to the microphone
note-language = Menu language changed
# $pc (String): the PC's name
note-pc-forgotten = { $pc } removed from paired PCs
# $pc (String): the PC's name
note-paired = Paired with { $pc }
note-no-monitor-in-front = No monitor in front of you

## What the PC answered when a request failed

failure-not-connected = No connection to the PC
failure-disconnected = The connection to the PC broke
failure-timed-out = The PC does not answer
failure-unsupported = The PC cannot do this
failure-not-allowed = Not allowed for this headset
failure-unknown-display = That display is gone
failure-limit = The limit is reached
failure-bad-size = A size the PC does not make or the headset cannot show
failure-last-display = The last monitor stays
failure-no-window = No active window on the PC
failure-desktop = The PC's desktop did not do it
failure-encoder = The PC's encoder did not start
failure-capture = The picture could not be taken
failure-sound = The sound could not be switched
failure-not-on-displays = The mouse is not on the monitors
failure-no-such-profile = No such profile
failure-bad-argument = A value out of range
failure-other = It did not work

## The palm menu's date and the 12-hour clock

# $weekday (String): weekday-0 to weekday-6; $day (Number): the day of the month;
# $month (String): month-0 to month-11
date-format = { $weekday }, { $month } { $day }
weekday-0 = Sun
weekday-1 = Mon
weekday-2 = Tue
weekday-3 = Wed
weekday-4 = Thu
weekday-5 = Fri
weekday-6 = Sat
month-0 = Jan
month-1 = Feb
month-2 = Mar
month-3 = Apr
month-4 = May
month-5 = Jun
month-6 = Jul
month-7 = Aug
month-8 = Sep
month-9 = Oct
month-10 = Nov
month-11 = Dec
# $time (String): 2:05; $half: am or pm
time-12h =
    { $half ->
        [am] { $time } AM
       *[pm] { $time } PM
    }
