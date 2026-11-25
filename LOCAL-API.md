# Prostir local API

Programs of the same user on the PC (`prostir ctl`, the settings window
`prostir window`, scripts, hot keys, an AI agent through
[`prostir mcp`](#mcp-prostir-mcp)) talk to the running Prostir through this API. The
headset protocol is a separate thing (`PROTOCOL.md`).

The requests, answers, events and the state are Rust types in the crate
`prostir-api` (`crates/prostir-api/src/lib.rs`), shared by the service (`prostir serve`) and every
client, so the two sides cannot drift apart. They travel as JSON. This document is the
contract for clients written in any language; the tests of `prostir-api` pin the JSON.

## Transport and access

| | |
|---|---|
| Bus | the **session bus** (D-Bus) |
| Name | `io.github.prostir_app.Prostir` |
| Object | `/io/github/prostir_app/Prostir` |
| Interface | `io.github.prostir_app.Prostir1` |
| Errors | `io.github.prostir_app.Prostir1.Error.<Name>` |

Only programs of the same user reach the session bus, which is the whole access rule:
the same trust as the tray icon, which shows the same things (the pairing code among
them). There is no check per request beyond that. The person at the PC asks as its
owner, with every right; see [Commands for the headset](#commands-for-the-headset) for
what goes to a headset.

The digit in the interface name is its version: an incompatible change adds
`io.github.prostir_app.Prostir2` next to it. If the project gets a domain of its own,
a new bus name is added and this one stays as a second name until the next major
version, so scripts keep working.

On Windows (later) the same JSON goes over a named pipe `\\.\pipe\prostir-<SID>`, one
object a line; nothing else of this document changes.

Prostir is not D-Bus activatable: when it is not running, calls fail with
`org.freedesktop.DBus.Error.ServiceUnknown` at once. `prostir` starts it.

Prostir is on the bus only while something needs it. Asleep, nothing of it runs but
what holds its sockets: systemd's `prostir.socket`, or without systemd its small
sockets' watcher (`prostir serve` itself), which is not on the bus. A headset that
comes (connects, or asks for the PC), `prostir`, the tray's notice of a new
headset start it; it leaves again 10 s after nothing needs it: no headset connected
and no monitors waiting for one, no settings window on the bus, no pairing window
open. A client follows it coming and going with `NameOwnerChanged` (below).

If the session bus goes away while Prostir runs (dbus-daemon restarted), Prostir
takes its name again once the bus is back, trying after 1 s, then after pauses that
double up to a minute; meanwhile signals are not sent. Prostir connects the bus's unix
socket itself, so an idle Prostir does not wake at all; with a bus address that is no
unix socket (`tcp:`, which a desktop session does not use) zbus connects instead, a
thread of its own then wakes twice a second while idle, and the log says so once.
Without a session bus at the start, Prostir runs without the local API (the log says
so) and does not look for one later.

## Methods

| Method | In | Out | What it does |
|---|---|---|---|
| `Call` | `s request_json` | `s answer_json` | any [request](#requests), at most 64 KiB; the window and `ctl` use it |
| `GetState` | | `u revision, s state_json` | the whole [state](#state) |
| `Recenter`, `LineUp`, `ShowMouse`, `ResetArrangement` | | | commands for the headset |
| `Profile` | `s name` | | `name`: the profile's name, or its number from 1 as the headset's menu lists them (`headsets[].session.profiles`) |
| `RenameProfile` | `s profile, s name` | | gives `profile` (its name or number) the name `name`; an empty one: called by its number again |
| `Cinema`, `Focus`, `Follow`, `NoLimits`, `SetMic` | `b on` | | `SetMic` is refused, see below |
| `Preset` | `y preset` | | |
| `AddMonitor` | `u w, u h, q scale_percent` | `u id` | |
| `RemoveMonitor` | `u id` | | |
| `ShowPhysical` | `u id, b shown` | | a display of the PC in the headset or not |
| `SetQuality` | `u mbit` | | 5 to 100 |
| `PairNewHeadset` | | `u seconds_left` | opens the pairing window |
| `StopPairing`, `ShowSettings`, `Quit` | | | |
| `ForgetHeadset` | `s fingerprint_or_name` | | |

The typed methods are for hot keys and scripts (`busctl`, `gdbus`, `qdbus6`); each is
the same as the request of the same name through `Call`. A failure is a D-Bus error
(below), with a text in the PC's language.

**Timeouts.** A command for the headset waits up to 30 s for the headset; the service
then answers `TimedOut`. `busctl` and `gdbus` give up after 25 s by default, so give
them more:

```
busctl --user --timeout=35 call io.github.prostir_app.Prostir \
    /io/github/prostir_app/Prostir io.github.prostir_app.Prostir1 Recenter
gdbus call --session --timeout 35 --dest io.github.prostir_app.Prostir \
    --object-path /io/github/prostir_app/Prostir \
    --method io.github.prostir_app.Prostir1.Profile 2
```

`prostir ctl` and the settings window wait 35 s.

## Properties

| Property | Type | |
|---|---|---|
| `Version` | `s` | the PC app's version (`0.2.0`, `0.2.0-dev+p5`) |
| `StateVersion` | `u` | the `v` of `state_json` |

## Signals

| Signal | Arguments | When |
|---|---|---|
| `StateChanged` | `u revision, s state_json` | the state changed; the whole state, never a delta; at most 10 a second |
| `HeadsetConnected` | `s headset, s name, s via` | a session opened; `via` is `wifi` or `usb` |
| `HeadsetDisconnected` | `s headset, s reason` | a session ended; `reason`: `quit`, `taken-off`, `silence`, `forgotten`, `other` |
| `HeadsetWorn` | `s headset, b worn` | put on or taken off during a session |

`headset` is the headset's `key` (see the state). A headset that reconnects before
the PC noticed the old connection was lost gets `HeadsetDisconnected` (`other`) and
`HeadsetConnected` right after.

**Order.** An answer goes out after the `StateChanged` that shows what it changed: a
client that has the answer has the state with the change too (its revision is the
answer's `revision` or later). That holds for a command for the headset too: what the
headset changed and said just before it answered (its "No limits", say) is in that
state. While a `StateChanged` waits for its turn under the limit of 10 a second, the
answers and headset signals after it wait behind it, so the order and the limit both
hold. `GetState` always has the latest state, also while a `StateChanged` waits.

**Revisions** count from 1 each time Prostir starts: after it restarts (the name
vanished and came back) a smaller revision is a new state, not an old one.

To follow the service coming and going, watch `NameOwnerChanged` for the name: subscribe
to the signals by path and interface (not by sender), so the subscription holds before
Prostir appears and after it restarts. After it appears, call `GetState`.

## Errors

The D-Bus error name is `io.github.prostir_app.Prostir1.Error.` and one of:

| Name | |
|---|---|
| `Unsupported` | this PC or headset does not do it (or not yet), or a request this version does not know |
| `NotAllowed` | not allowed (a command while only a guest headset is connected) |
| `UnknownDisplay` | no such display |
| `Limit` | as many as allowed already |
| `BadSize` | a size or scale the PC does not make |
| `LastDisplay` | the last monitor stays |
| `NoWindow` | no active window on the PC |
| `Desktop` | the desktop did not do it |
| `Encoder` | the encoder did not start |
| `Capture` | the picture could not be taken |
| `Sound` | the sound could not be switched |
| `NotOnDisplays` | the mouse is on none of the displays |
| `NoSuchProfile` | the headset has no such profile |
| `BadArgument` | a value out of range, or a request that does not read |
| `NotConnected` | no headset is connected (for a command for the headset) |
| `TimedOut` | the headset did not answer within 30 s |
| `Disconnected` | the headset left before it answered |
| `Other` | anything else, also "Prostir is quitting" for a request that comes while it quits; a client reads a name it does not know as this |

The first fourteen are the headset protocol's error codes (`PROTOCOL.md`), by name.

## Requests

A request is a JSON object whose `request` names it; the other fields are its
arguments. The service reads requests strictly: a `request` it does not know is
`Unsupported`, a missing or wrong argument `BadArgument`. Fields it does not know are
ignored.

```json
{"request": "add_monitor", "w": 2560, "h": 1440, "scale": 125}
```

### Commands for the headset

Answered when the headset answers (30 s at most). Arguments the headset protocol
cannot carry (a profile's name over 64 bytes) are refused with `BadArgument` before
anything goes to the headset. They go to the **main** headset. A
headset whose line in `settings.txt` makes it a guest is someone else's headset: while
only guests are connected, commands are refused with `NotAllowed`. Without a headset:
`NotConnected`. A headset that does not take commands: `Unsupported`.

| `request` | Arguments |
|---|---|
| `recenter`, `line_up`, `show_mouse`, `reset_arrangement` | |
| `profile` | `name` (string): the profile's name, or its number from 1 as the headset's menu lists them |
| `rename_profile` | `profile` (string, its name or number), `name` (string, 64 bytes at most; empty: called by its number again; a number only is `BadArgument`): the new name shows in `headsets[].session.profiles` before the answer |
| `cinema`, `focus`, `follow`, `no_limits` | `on` (bool) |
| `preset` | `preset` (0 to 255) |

### The PC's own state

Answered in the same pass of Prostir's control loop that made the change, right after
the `StateChanged` that shows it. While no headset is connected and no monitors
exist, monitor requests change the set kept for the next headset (its monitors stand
`kept` in the state).

| `request` | Arguments | |
|---|---|---|
| `add_monitor` | `w`, `h` (pixels), `scale` (percent, default 100) | answers `id`; the monitor stands `starting` until it runs, `failed` if it could not start (it keeps its place) |
| `remove_monitor` | `id` | `LastDisplay` for the last one |
| `retry_monitor` | `id` | a failed monitor tried again now; `UnknownDisplay` for no such monitor |
| `show_physical` | `id`, `shown` (bool) | a display of the PC in the headset |
| `set_scale` | `id`, `scale` (percent) | another scale for a monitor: its output is made anew under the same name and id (it stands `starting` meanwhile); `BadSize` for a scale the desktop does not give its size. A scale given to a monitor in the desktop's own display settings is taken as well, within about a second: the state and the set kept for next time follow it, the output stays as it is. Not taken: a scale the desktop does not give that size (it stays on the desktop, the state keeps the one before), and a scale the desktop never had the monitor at after it was made (one it could not set) |
| `place` | `id`, `x`, `y` | moves a display on the desktop to about `x`, `y` (its top-left corner, logical pixels): into the row it is then in, between its neighbours left and right, or a row of its own above or under the others; the desktop's arrangement then lines it up. Only while the monitors are on the desktop (`Unsupported` otherwise). The main headset's next arrangement (when the person moves a monitor in the headset) replaces it; the headset's own 3D arrangement is not changed |
| `set_primary` | `id` | the desktop's primary display |
| `screenshot` | `id` | a picture of a virtual monitor or a window panel, saved as a PNG as the headset's screenshots are (`Prostir/<date and time to the millisecond>.png` in the Pictures folder, a name no other file there has); answers `path` (the whole path) and `whole`. Answered once the desktop took it (20 s at most, else `Desktop`): with `grim -o` on Hyprland; on KDE Plasma from the monitor's own stream while it streams to a headset, else Spectacle's picture of the whole desktop (`whole`: true). `UnknownDisplay` for no such monitor or a display of the PC, `Capture` when it could not be taken, `Unsupported` while no monitors are on the desktop, or when they leave it before the picture |
| `set_quality` | `mbit` (5 to 100) | per monitor |
| `set_sharp` | `on` | the slower encoder preset for clearer small text |
| `set_codec` | `codec` (`h264`, `h265`, `auto`), `cpu` (bool, default false) | `auto` is the same as `h265` (HEVC where the PC and every headset can, else H.264), and the state shows it as `h265` |
| `sound_to_headset` | `on` | the PC's sound in the headset, or on the PC |
| `volume` | `level` (0 to 100) | the main headset's volume, as a slider of it on the PC: the headset's sound device goes to that level, unmuted (as moving a volume slider in Plasma unmutes), and the headset is asked for it, as when the device's own slider moves; `sound.volume` then shows the level the headset took (its nearest step). `BadArgument` above 100; `Unsupported` while `sound.volume` is `null` (no headset connected, or one that does not tell its volume) |
| `set_counting` | `counting` (`yes`, `no`, `unasked`) | the person's answer to "Count this PC among users?" (once a day one request with only the date, see the privacy policy), kept in `settings.txt` (`count yes`, `no` or `unasked`). Nothing is counted until it is `yes`; `unasked` puts the question back in the settings window. Set it only as the person says. `BadArgument` for anything else |
| `autostart` | `on` | start with the session: with systemd, enables `prostir.socket` (and removes an autostart entry of 0.1), else an XDG autostart entry (`prostir serve`); `Unsupported` where it is not set from here (Hyprland, whose config starts it). On by default: the first start of Prostir for a user turns it on; the choice is kept in `settings.txt` (`autostart on` or `off`), so once turned off (here, the tray, `prostir ctl autostart off`) no later start or update turns it on again |
| `pair_new_headset` | | opens the pairing window for 3 minutes (again from now if it is open), answers `seconds_left`: while it is open the PC answers every headset that asks for it, and the program stays running. It closes at its time, with `stop_pairing`, or once a headset pairs. A headset can pair over a connection (a typed address, USB) whether it is open or not; its code protects that |
| `stop_pairing` | | closes it |
| `pairing_code` | `code` (six digits, spaces allowed) | the code a headset shows, typed in the settings window: while the window is on the bus, a headset that shows its code waits for it here (`pairing.code_for`) instead of a dialog. Answered once that attempt ended, with the state showing how (`pairing.last`, or the headset among `headsets`); `NotAllowed` when no headset waits for a code, `BadArgument` for anything but six digits |
| `window_look` | `theme` (`system`, `light`, `dark`), `palette` (a word: `ink`, `graphite`), `language` (a code or `null`) | the settings window's look, kept in `settings.txt` |
| `forget_headset` | `headset`: its `key`, its whole key (64 hex digits) or its name | its session ends |
| `forget_all` | | every paired headset |
| `show_settings` | | starts the settings window, `prostir window`, a process of its own (it keeps one window per user itself) |
| `quit` | | Prostir quits after answering, as the tray's "Quit": nothing waits for the headset afterwards (the sockets' watcher quits too; under systemd the socket stops until the next login) |

### Not from the PC

This reads, and is answered `Unsupported` with a text saying why:

| `request` | Arguments | |
|---|---|---|
| `set_mic` | `on` | always: the headset microphone's switch is the headset user's, changed only in the headset; the state shows it (`session.mic_on`) |

## Answers

`Call` answers one JSON object:

```json
{"revision": 7}
{"id": 103, "revision": 8}
{"seconds_left": 180, "revision": 9}
{"path": "/home/me/Pictures/Prostir/2026-11-21 14-05-09.042.png", "whole": false, "revision": 9}
```

`revision` is that of the state that already has the change; `id` comes with
`add_monitor`, `seconds_left` with `pair_new_headset`, `path` and `whole` with
`screenshot`. On D-Bus a failure is a D-Bus
error, not an answer. A transport without errors of its own (the named pipe) answers
`{"error": "NotConnected", "text": "No headset is connected", "revision": 7}` instead.

## State

`GetState` and `StateChanged` carry the whole state as one JSON object with its
revision, a number that grows with every change. Every field is always present (`null`
where there is nothing). Clients read it leniently: fields they do not know are
ignored, a missing field takes its default, and an unknown value of a closed set
(`kind`, `state`, `via`, an error's name) reads as "other". `v` grows only for a
change older clients would misread.

```json
{
  "v": 1,
  "pc": {"name": "desk", "app": "0.2.0", "desktop": "KDE Plasma", "phase": "live", "addresses": ["192.168.1.10"]},
  "window": {"theme": "system", "palette": "ink", "language": null},
  "autostart": true,
  "headsets": [
    {
      "key": "a1b2c3d4e5f6", "name": "Quest 3", "model": "Quest 3", "app": "0.2.0",
      "last_seen": 1795000000,
      "role": "main", "rights": ["input", "mic"],
      "session": {
        "via": "wifi", "worn": true, "battery": {"percent": 80, "charging": false},
        "profile": "Work", "profiles": ["", "Work"], "mic_allowed": true, "mic_on": false,
        "main": true, "commands": true, "no_limits": false
      }
    }
  ],
  "connecting": null,
  "displays": [
    {
      "id": 103, "kind": "virtual", "name": "Prostir-1", "output": "Virtual-Prostir-1",
      "builtin": false, "shown": true, "mode": {"w": 2560, "h": 1440, "scale": 125},
      "desk": {"x": 0, "y": 0, "w": 2048, "h": 1152},
      "state": "failed", "failure": {"code": "Desktop", "text": "KScreen"}
    }
  ],
  "primary": 103,
  "limits": {
    "sizes": [[1920, 1080]], "scales": [100, 125], "displays_max": 6,
    "physical_max": 2, "streams_max": 8, "min": [640, 360], "max": [3840, 2160]
  },
  "encoding": {
    "bitrate": 30, "large": null, "sharp": true, "sharp_by_size": true,
    "codec": "h265", "cpu": false,
    "using": {"codec": "h264", "hardware": true},
    "outputs": [
      {"display": 103, "encoder": "VA-API", "codec": "h264", "hardware": true,
       "copies": false, "note": null,
       "stats": {"window_ms": 10000, "frames": 600, "repeats": 5, "bytes": 30000000,
                 "keyframes": 1, "keyframe_max": 750000, "encode_us_avg": 4300,
                 "encode_us_max": 9100}}
    ]
  },
  "sound": {
    "to_headset": true, "volume": {"level": 40, "muted": false}, "mic_on": false,
    "mic": "off", "mic_failure": null
  },
  "pairing": null,
  "nearby": [],
  "counting": "yes",
  "problems": [{"code": "Sound", "text": "The sound could not be switched"}],
  "wifi": {"interface": "wlan0", "power_save": true, "freq_mhz": 5180, "width_mhz": 80}
}
```

| Field | |
|---|---|
| `v` | the state's version, now 1 |
| `pc.name` | the name headsets see |
| `pc.app` | the PC app's version |
| `pc.desktop` | `KDE Plasma`, `Hyprland`; empty while there are no monitors |
| `pc.phase` | `idle` (waiting, no monitors), `live` (a headset connected), `lingering` (the monitors wait for the headset to come back) |
| `pc.addresses` | the PC's IPv4 addresses on its networks (a VPN's too, not those of containers or virtual machines), as a person types one into a headset that does not find the PC by itself; empty from a PC before 0.2.0's step 7.9 |
| `window` | the settings window's look (`window_look`) |
| `autostart` | starting with the session; `null` where it is not set from here |
| `headsets` | every paired headset, those connected first (in the order they connected) |
| `headsets[].key` | the fingerprint of its key, 12 hex digits: how requests and events name it |
| `headsets[].name` | as it was paired |
| `headsets[].model` | what it says it is, while connected |
| `headsets[].app` | its Prostir app's version as it last connected (`0.2.0`), empty where not known: two apps on one headset (a release and a test build) are two headsets, each with its own key |
| `headsets[].last_seen` | when it last connected or left, Unix seconds; `null` where not known (it has not connected since the PC began to keep this) |
| `headsets[].role`, `rights` | from its line in `settings.txt` (`main` with every right without one); rights: `input`, `mic`, `camera`, `clipboard`, `changes`, `private` |
| `headsets[].session` | while connected: `via` (`wifi`, `usb`; from 127.0.0.1 counts as USB), `worn`, `battery`, `profiles` (the profiles saved in the headset, in its menu's order: the name each was given, empty for one called by its number; empty where the headset does not say), `profile` (the one in use: its name, else its number; `null`: none, or not said), `mic_allowed` (whether the headset's system lets it record; `null`: not known), `mic_on` (its switch), `main` (the main headset now: its arrangement is the desktop's, commands go to it), `commands` (it takes commands), `no_limits` (where it takes commands) |
| `connecting` | a headset being connected now, by name (not yet) |
| `displays` | in the order of the rows, then the rest |
| `displays[].kind` | `virtual` (a monitor made for the headset), `physical` (a display of the PC), `window` (a window panel) |
| `displays[].name` | `Prostir-2`, or a display of the PC's label |
| `displays[].output` | the desktop's name for the output, while it has one |
| `displays[].builtin`, `shown` | a display of the PC: the laptop's panel; shown in the headset |
| `displays[].mode` | the stream's size and the scale in percent |
| `displays[].desk` | where it is on the desktop, in logical pixels. `null` when it is not there: a monitor kept for the next headset (no monitors on the desktop), one whose output could not be made, a display of the PC that is not connected; and for a monitor just added while the main headset places the monitors (it has sent where they stand this session), until the desktop has it where the headset put it, at most 4 s and no longer once no headset is connected, so a map never shows it at a stand-in place |
| `displays[].placing` | `true` for a monitor just added that the headset is placing (its `desk` is `null` meanwhile, see above); left out when false |
| `displays[].state` | `starting`, `running`, `failed` (see `failure`; it keeps its place and is tried again), `absent` (a display of the PC that is not connected now: the lid closed, the cable out), `kept` (a monitor of the set kept for the next headset, while no monitors are on the desktop) |
| `primary` | the desktop's primary display |
| `limits` | for the main headset while one is connected, else what any headset gets: `sizes` to offer `[w, h]`, `scales` (percent), `displays_max` (virtual monitors), `physical_max`, `streams_max`, `min` and `max` size |
| `encoding` | as chosen: `bitrate` (Mbit/s per monitor), `large` (Mbit/s for each monitor of 2560x1440 or more now where another: over USB while the quality is not chosen; `null` otherwise), `sharp`, `sharp_by_size` (`sharp` is not chosen: on only for monitors smaller than 2560x1440), `codec` (`h264`, `h265`, `av1`), `cpu`; `using`: what it encodes with now, while there are monitors; `outputs`: how each display that streams encodes (each has an encoder of its own, picked for where its frames are): `display`, `encoder` (`VA-API`, `NVENC`, `x264`), `codec`, `hardware`, `copies` (its frames go through the CPU on their way to the encoder), `note` (why it is not on the best encoder this PC has for it, for example a session limit reached or an encoder that did not start; `null` when it is), `stats` (what its encoder made over the last window of the PC's report, about 10 s while a headset is connected; `null` before one ended: `window_ms`, `frames`, `repeats` (frames that only repeat the picture before, a still screen's keepalive), `bytes`, `keyframes`, `keyframe_max` in bytes, `encode_us_avg` and `encode_us_max`, from a frame entering the encoder to leaving it, in microseconds) |
| `sound` | `to_headset`; `volume`: the main headset's volume (`level` 0 to 100, `muted`), while the PC's volume device follows it (one volume for the PC's slider and the headset's buttons), else `null`; `mic_on` (the switch of the headset whose microphone the PC takes, read only: it is changed in the headset); `mic` (`off`, `ready`, `failed`: the PC's end, the virtual microphone programs record from) and `mic_failure` |
| `pairing` | while the pairing window is open, the PC shows a code, or a headset waits for its code: `seconds_left`, `seconds_total` (how long the pairing window was opened for; `0`: no pairing window, only a code), `pc_code` (the code to type in the headset, while the PC shows one), `code_for` (a headset waiting for its code to be typed here, by name; only while the settings window is on the bus), `last` (how the last attempt since the window opened ended: `{"paired": {"name": …}}`, `"wrong"`; anything else reads as other) |
| `nearby` | headsets that are not this PC's and asked for it in the last minute looking for a PC to pair with, by the name they gave (a query without a name is not listed, from step 7.9), the latest last |
| `counting` | the answer to "Count this PC among users?": `unasked` (the settings window asks, at the top of its overview and first run; nothing is counted meanwhile, so a PC where nobody answered, such as one that only runs `prostir serve`, never counts), `yes`, `no`; `null` where this build or run never counts (a debug build, one without the `count` feature or with no key, `PROSTIR_NO_COUNT=1`), so there is nothing to ask. An update from 0.1 asks again unless 0.1 was turned off |
| `problems` | what is wrong now: `code` (an error's name) and `text` in the PC's language; `Desktop` while the desktop does not respond (a call into it has hung for 15 s) |
| `wifi` | the PC's own Wi-Fi when it reaches its network over Wi-Fi (its default route goes through a wireless interface), as found when the last headset connected over the network; `null` on a cable or before: `interface`, `power_save` (the card's power saving; on, it can hold back what comes for the PC by tens of milliseconds; `null` when the card does not say), `freq_mhz` and `width_mhz` of the channel (0 when not known). Prostir only reports it and changes nothing of the network |

The pairing code is on the bus because the tray shows it to the same user anyway; it
is never written to the log.

## Events

`prostir ctl watch --json` and the named pipe give the headset signals as JSON:

```json
{"event": "headset_connected", "headset": "a1b2c3d4e5f6", "name": "Quest 3", "via": "wifi"}
{"event": "headset_disconnected", "headset": "a1b2c3d4e5f6", "reason": "taken-off"}
{"event": "headset_worn", "headset": "a1b2c3d4e5f6", "worn": false}
```

`watch --json` also prints `{"event": "state_changed", "revision": 8, "state": {…}}`
for each `StateChanged`, and `{"event": "vanished"}` / `{"event": "appeared"}` as
Prostir leaves and comes back on the bus (it keeps watching).

## `prostir ctl`

```
prostir ctl status [--json]
prostir ctl watch [--json]                 # what happens, a line each, until Ctrl+C
prostir ctl recenter | lineup | showmouse | reset
prostir ctl profile N|NAME                 # a profile of the headset: its number, or its name
prostir ctl profile rename N NEW NAME      # profile N (a number or a one-word name) named NEW NAME
prostir ctl cinema|focus|follow|mic|nolimits on|off
prostir ctl preset N
prostir ctl monitor list | add WxH[@1.25|@125%] | remove ID | retry ID | primary ID
prostir ctl screen show|hide ID            # the PC's own displays
prostir ctl screenshot ID                  # a picture of a monitor: where it was saved
prostir ctl quality MBIT | sharp on|off | codec h264|hevc|auto [--cpu]
prostir ctl sound headset|pc
prostir ctl pair | pair stop
prostir ctl headsets | forget HEADSET | forget --all
prostir ctl autostart on|off|status
prostir ctl count yes|no|unasked|status   # count this PC among users (unasked: ask again)
prostir ctl settings
prostir ctl quit
```

Without `--json`: one line for people (in the PC's language). With `--json`: one JSON
object, the answer, the state (`status`), the list (`monitor list`, `headsets`), or a
failure, always as `{"error": NAME, "text": …}`: the error names above, and
`NotRunning` and `NoBus` of `ctl` itself; `watch` one object a line. `watch` takes
signals only from the program that has the name, and gives the whole state again each
time Prostir comes back on the bus.

| Exit code | |
|---|---|
| 0 | done |
| 1 | it did not happen (the error's name is in the text or the JSON) |
| 2 | Prostir is not running, or no headset is connected for a command for the headset |
| 3 | no answer in time (the headset did not answer within 30 s, or Prostir not within 35 s) |
| 64 | wrong arguments |

`ctl status` when Prostir is not running does not start it: it says `Not running`
(`{"error": "NotRunning", "text": …, "running": false}`), or `Waiting for the
headset (asleep)` (`"asleep": true` too) when the sockets' watcher or systemd's socket
waits for a headset, or that port 47830 is taken by another program (`"port_taken":
true`), and exits with 2.

## MCP (`prostir mcp`)

`prostir mcp` is this API for an AI agent, as a server of the Model Context Protocol:
the agent's program starts it and talks to it over its stdin and stdout (JSON-RPC 2.0,
one message a line; MCP versions `2025-11-25`, `2025-06-18`, `2025-03-26` and
`2024-11-05`). Only those messages go to stdout; its log goes to stderr. For Claude
Code:

```
claude mcp add prostir -- prostir mcp
```

It needs the session bus (`DBUS_SESSION_BUS_ADDRESS`); `start` without a systemd user
manager also needs the desktop session's `XDG_RUNTIME_DIR` and `WAYLAND_DISPLAY` (or
`DISPLAY`), and says so when they are missing. An agent started from the desktop
session has them. One started elsewhere (over SSH, by a service) gets them in the
server's environment: `claude mcp add prostir -e
DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus -e XDG_RUNTIME_DIR=/run/user/1000
-e WAYLAND_DISPLAY=wayland-0 -- prostir mcp`, or `"env"` next to `"command"` in the
client's MCP configuration. It connects to the bus when a tool first needs it, and
again after the connection broke.

It is a client of the session bus like `prostir ctl`, with the same rights (those of
the PC's owner) and no other way in: no network port, only the stdio of the process the
user's own agent started. Each tool is one request (or `GetState`); the agent's program
can ask the person before the tools marked `destructiveHint` (`remove_monitor`,
`forget_headset`, `forget_all`, `quit`, `call`); only `get_state`, `list_monitors` and
`wait_for_state` are marked `readOnlyHint`. What the agent sees, its model sees too: the
state has the pairing code while one is shown, and a screenshot shows what is on that
monitor. On KDE Plasma while the monitor does not stream to a headset, the screenshot
is of the whole desktop: every screen, and every window on them. The tools' schemas are
closed (`additionalProperties: false`, with the bounds of each number), and an argument
a tool does not take is refused with `BadArgument`.

| Tool | Arguments | |
|---|---|---|
| `get_state` | | `{"revision": …, "state": {…}}`, the [state](#state) |
| `list_monitors` | | `{"revision", "primary", "displays"}` from the state |
| `add_monitor` | `w`, `h`, `scale` (100) | `add_monitor`; the answer as `Call` gives it (`{"id": …, "revision": …}`) |
| `remove_monitor`, `retry_monitor`, `set_primary` | `id` | the requests of the same name |
| `place_monitor` | `id`, `x`, `y` | `place` |
| `set_scale` | `id`, `scale` | |
| `show_pc_display` | `id`, `shown` | `show_physical` |
| `recenter`, `line_up`, `show_mouse`, `reset_arrangement`, `profile`, `rename_profile`, `cinema`, `focus`, `follow`, `no_limits`, `preset` | as the requests | commands for the headset |
| `set_quality`, `set_sharp`, `set_codec`, `sound_to_headset`, `autostart` | as the requests | |
| `set_counting` | `counting`: `no` or `unasked` | as the request, but never `yes`: counting needs the person's own yes (the settings window, the tray, `prostir ctl count yes`), so `yes` is `BadArgument`, also through `call` |
| `pair_new_headset`, `stop_pairing`, `forget_headset`, `forget_all`, `show_settings`, `quit` | as the requests | |
| `call` | `request`: a request object | any request as it is, also those no tool covers |
| `screenshot_monitor` | `id`, `keep` (false) | `screenshot`; the picture as MCP `image` content (`image/png`), after a text `{"id", "whole", "bytes", "revision", "path"}`; the file is removed afterwards unless `keep` (then `path` names it). A picture over 5 MB is not sent: its file is kept and `path` names it. Only a picture Prostir saved is read: a regular file (no link) ending in `.png`, right in `Prostir` of the Pictures folder as this program finds it, that begins as a PNG; anything else is `Capture`, and the file is left as it is |
| `wait_for_state` | `pointer`, one of `equals`, `contains`, `exists` (`null` as left out); `timeout` (seconds, 10 by default, 50 at most) | waits until the value at the JSON Pointer `pointer` (RFC 6901, `""` the whole state) is `equals`, has `contains` in it (an object those fields, an array such an element, a string that text) or `exists` (true or false); without a condition, until the state changes (Prostir coming back on the bus counts). Answers `{"revision", "value", "waited_ms"}` |
| `start` | | starts Prostir as `prostir` does (the settings window opens too, and keeps it running while open), waiting 30 s at most for it on the bus; a new Prostir starts with `systemd-run --user` (its log in the journal) where there is a systemd user manager, else on its own with nothing on the agent's stdio. Its errors: `NoBus`, `NotRunning` (with why), `TimedOut`. Nothing else starts it |

For example, after `add_monitor` answered `{"id": 105, …}`, `wait_for_state` with
`{"pointer": "/displays", "contains": {"id": 105, "state": "running"}, "timeout": 30}`
returns once monitor 105 runs. A new monitor can stand `running` with `desk: null` for a
few seconds while the headset places it; `{"pointer": "/displays", "contains": {"id":
105, "desk": {}}}` waits until it is on the desktop. A wait looks at `GetState` first,
then again on each `StateChanged` and at least once a second.

A tool that did not do what it was asked answers its result with `isError: true` and
a text `{"error": NAME, "text": …}`, NAME as `ctl --json` gives it: the
[errors](#errors) above, `NotRunning` and `NoBus`, `BadArgument` for arguments that do
not read, `TimedOut` for a wait whose condition did not hold in time (with `revision`
and `value` as they were last). While Prostir is not running every tool says
`NotRunning` at once. JSON-RPC errors are left for what is not MCP: `-32700` (not JSON),
`-32600` (not a request), `-32601` (no such method), `-32602` (no such tool); a line
that is no JSON (not even UTF-8) gets `-32700`, and the server reads on.

Requests are handled one after the other: a command for the headset or a wait holds
the next ones until it is answered. A `ping` is answered at once, also meanwhile.
`notifications/cancelled` ends a wait soon, and a cancelled request gets no answer
(a command for the headset or a screenshot already asked of Prostir still happens).
With `_meta.progressToken` in its request, a wait sends `notifications/progress` about
once a second: `progress` the seconds waited, `total` its timeout.

## `prostir` and the settings window

One program, `prostir`, in two processes. `prostir` without a command (the menu entry,
`io.github.prostir_app.Prostir.desktop`) opens the settings window. `prostir serve`
is the service: the sockets' watcher, which sleeps until a headset comes and then
starts the full program (`prostir serve --stay` runs the full program all along
instead, with its sockets opened itself). Autostart entries, systemd's
`prostir.service` and Hyprland's `exec-once = prostir serve` start it so.

When Prostir runs, `prostir` asks it for the settings window (`ShowSettings`). When it
sleeps, it wakes it: the watcher (SIGUSR1), which starts it with the window, or systemd
(`systemctl --user start prostir.service`), and then asks for the window once it is on
the bus, 30 s at most. When nothing runs, it starts `prostir serve --open-settings` on
its own (its log goes where `prostir`'s errors go: the journal, when the desktop
started it), which opens the window once it is on the bus; `prostir` waits for that
30 s at most (exit code 3 after that), and says so when the new Prostir ends first
(exit code 2). Prostir takes its name
on the bus before it starts GStreamer, so this is quick. `prostir serve` started again
while Prostir runs opens the window of the running one too, and so does a click on the
tray icon; the very first start of Prostir for a user opens it as well. The tray has
the same item ("Settings…").

The window is a process of its own: the service starts its own program again as
`prostir window` (a hidden command), so a window that crashes takes nothing else down
and the service never loads the window's graphics libraries (EGL, X11). The
window's app id is `io.github.prostir_app.Prostir`, the desktop entry's name.

The settings window owns `io.github.prostir_app.Prostir.Settings` on the session
bus (one window per user: a second start calls `Present` on
`io.github.prostir_app.Prostir.Settings1` of the first and quits). Prostir stays
running while that name is on the bus, and a headset showing its pairing code then
waits for the code to be typed in the window (`pairing.code_for`, `pairing_code`)
instead of a dialog. `ctl watch` does not keep it running.
