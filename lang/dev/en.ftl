# The headset's "For development" part, in its preview build only
# (prostir-lang with the feature `preview` adds this file to lang/en.ftl).

## Settings: for development

headset-section-dev = For development
headset-dev-size = Monitor size
headset-dev-size-standard = Standard
headset-dev-size-one = 1:1
headset-dev-size-main = 1:1 main
headset-dev-size-85 = 0.85
headset-dev-reset = Reset size
headset-dev-unknown = This headset model is not known: only the standard size.
headset-dev-super-sampling = Layer smoothing
headset-dev-sharpening = Layer sharpening
headset-dev-auto = Auto
headset-dev-auto-line = Auto: k at least
headset-dev-line-85 = 0.85
headset-dev-line-90 = 0.9
headset-dev-line-100 = 1.0
headset-dev-metrics = Numbers by the monitor
# $rate (Number): megabits a second
headset-dev-mbit = { $rate } Mbit/s
# $scale (String): headset pixels per monitor pixel, such as 0.98
headset-dev-metrics-scale = { $scale } headset px per monitor px
# $rate (String): megabits a second, such as 31.5
headset-dev-metrics-rate = { $rate } Mbit/s
# $size (Number): kilobytes
headset-dev-metrics-keyframe = keyframe { $size } KB

## Settings: hands, a trial of step 8

headset-section-dev-hands = Hands (trial)
headset-dev-monitors = Monitors
headset-dev-monitors-on = Touch and click
headset-dev-monitors-note = With touch and click, monitors take a fingertip's touch, a pinch and the trigger, and each hand switches between touch and its ray by how near the fingertip is. Clicks, touches and scrolls reach a PC that takes input from the headset; on any other only the pointer follows the ray.
headset-dev-take = Take a monitor with
headset-dev-take-fist = Fist
headset-dev-take-middle = Middle pinch
headset-dev-take-hold = Held pinch
headset-dev-move = A monitor in the hand moves
headset-dev-move-as-now = Along the ray
# $power (Number): the power of the reach (1, 2, 3)
headset-dev-move-sphere = Sphere, p = { $power }
headset-dev-hand-look = Hands look
headset-dev-hand-solid = Solid
# $percent (Number): how opaque, in percent
headset-dev-hand-percent = { $percent } %
headset-dev-hand-tips = Fingertips
headset-dev-fx = Effects
headset-dev-fx-thin = Thin
headset-dev-fx-standard = Standard
headset-dev-fx-bold = Bold
headset-dev-record = Record the input, 30 s
# $seconds (Number): seconds of the recording left
headset-dev-recording = Recording: { $seconds } s
headset-dev-read-feel = Read feel.txt again
headset-dev-feel-none = No feel.txt: the standard numbers.
# $changed (Number): numbers that differ from the standard ones; $skipped (Number): lines not taken
headset-dev-feel-read = feel.txt: { $changed } numbers changed, { $skipped } lines not taken.
