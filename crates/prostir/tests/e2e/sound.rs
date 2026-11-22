//! One volume for the PC's slider and the headset's buttons, the headset's microphone
//! as an up stream, and the sound system followed by its events (never asked on a
//! clock), also when they stop for a while.

use std::time::{Duration, Instant};

use prostir_proto::ClientMsg;
use prostir_proto::request::{Outcome, Request};
use prostir_proto::state::{UpKind, UpState, permission};
use prostir_testkit::{FIRST_VOLUME, FakeHeadset, View, quest_step};

use crate::pc::{Options, Pc, TestDir};
use crate::{SOON, session};

/// The levels the PC put its sound device at, in order (from the log).
fn written(pc: &Pc) -> Vec<String> {
    pc.lines("fake desktop: volume set to ")
        .iter()
        .filter_map(|l| l.split("volume set to ").nth(1).map(str::to_owned))
        .collect()
}

/// Takes what the PC sends until the device was last set to `level`, for up to `SOON`.
pub(crate) fn wait_written(pc: &Pc, headset: &mut FakeHeadset, level: u8) {
    let until = Instant::now() + SOON;
    while written(pc).last().map(String::as_str) != Some(&level.to_string()) {
        assert!(
            Instant::now() < until,
            "the device at {level}: {:?}",
            written(pc)
        );
        headset.take_for(Duration::from_millis(50)).unwrap();
    }
}

/// Takes what the PC sends until the session took the headset's new sound device in:
/// its first level is the headset's whatever it says, so a slider moved before would
/// be put back, not asked about. The device is set at start too, before the session
/// follows it, so the level alone does not tell. Read before the PC set it, the device
/// needs nothing more; else the PC puts the headset's level on it, and that is waited
/// for too.
pub(crate) fn wait_first_volume(pc: &mut Pc, headset: &mut FakeHeadset) {
    const NEW_DEVICE: &str = "volume: a new device for the headset";
    const SET: &str = "fake desktop: volume set to";
    let line = pc.wait_log(SOON, NEW_DEVICE);
    if line.contains("it takes the headset's") {
        let log = pc.lines("");
        let new_device = log.iter().position(|l| l.contains(NEW_DEVICE)).unwrap();
        let before = log[..new_device].iter().filter(|l| l.contains(SET)).count();
        pc.wait_log_after(SOON, SET, before);
    }
    wait_written(pc, headset, FIRST_VOLUME);
}

/// The headset's level comes in `Open` and goes to the device, its buttons too, and
/// nothing comes back for them; the slider asks the headset once and ends on its step;
/// dragged, the device is not set until it rests, and then once; quiet after.
#[test]
fn one_volume_for_the_slider_and_the_buttons() {
    let (_dir, mut pc, _keys, mut headset) = session("volume", &Options::default());
    wait_first_volume(&mut pc, &mut headset);
    headset.press_volume(73).unwrap();
    wait_written(&pc, &mut headset, 73);
    headset.take_for(Duration::from_millis(500)).unwrap();
    assert!(
        headset.view().volume_asks.is_empty(),
        "nothing asked for the headset's own changes"
    );

    // The slider.
    pc.desktop("volume 21");
    wait_written(&pc, &mut headset, quest_step(21));
    assert_eq!(headset.volume(), Some(quest_step(21)));
    assert_eq!(headset.view().volume_asks.len(), 1);

    // Dragged from 0 to 100 in a second: no level written while it moves, once when
    // it rests. Only where the drag itself stood still as long as the PC waits for a
    // slider to rest (a busy test machine held the fake slider up) may a level be
    // written on the way; the slider then never goes back.
    let before = written(&pc).len();
    let asks_before = headset.view().volume_asks.len();
    pc.desktop("drag 0 100 1000");
    wait_written(&pc, &mut headset, 100);
    let done = pc.wait_log(SOON, "fake desktop: drag done, longest pause");
    let longest: u64 = done
        .rsplit("longest pause ")
        .next()
        .and_then(|t| t.trim_end_matches(" ms").trim().parse().ok())
        .unwrap();
    let during: Vec<u8> = written(&pc)[before..]
        .iter()
        .map(|l| l.parse().unwrap())
        .collect();
    if longest < prostir_proto::volume::ASK_EVERY.as_millis() as u64 {
        assert_eq!(during, [100], "written once, when it rested");
    } else {
        eprintln!("the drag stood still for {longest} ms: levels on the way allowed");
        assert!(
            during.windows(2).all(|w| w[0] <= w[1]),
            "the slider went back: {during:?}"
        );
        assert_eq!(during.last(), Some(&100));
    }
    let asks = headset.view().volume_asks.len() - asks_before;
    assert!((1..=25).contains(&asks), "{asks} requests");
    assert_eq!(headset.volume(), Some(100));
    // Quiet once it rests.
    let asks = headset.view().volume_asks.len();
    headset.take_for(Duration::from_secs(2)).unwrap();
    assert_eq!(headset.view().volume_asks.len(), asks);
    assert_eq!(written(&pc).len(), before + during.len());
}

/// The slider and the buttons at the same moment (the PC's request and the headset's
/// change cross on the way): the headset's level wins.
#[test]
fn at_the_same_moment_the_headset_wins() {
    let (_dir, mut pc, _keys, mut headset) = session("volume-race", &Options::default());
    wait_first_volume(&mut pc, &mut headset);
    // The headset reads nothing for a moment: the PC's request waits on the way.
    headset.stall(Duration::from_secs(1));
    pc.desktop("volume 90");
    pc.wait_log(SOON, "fake desktop: volume moved to 90");
    // The PC's request is on its way (the headset reads nothing yet) when the
    // headset's own change goes.
    pc.wait_log(SOON, "volume: asking the headset for 90");
    headset.press_volume(13).unwrap();
    wait_written(&pc, &mut headset, 13);
    headset.take_for(Duration::from_secs(1)).unwrap();
    assert_eq!(headset.volume(), Some(13));
    assert_eq!(written(&pc).last().map(String::as_str), Some("13"));
}

/// While a program on the PC records, the PC asks for the microphone, and the
/// headset's stream reaches the PC's end of it; a microphone the system does not let
/// record shows on the PC.
#[test]
fn the_microphone_streams_while_a_program_records() {
    let (_dir, mut pc, _keys, mut headset) = session("microphone", &Options::default());
    let wanted = |h: &FakeHeadset| h.view().pc.up_wanted.get(&UpKind::Mic).map(|u| u.on);
    assert_eq!(wanted(&headset), Some(false));
    pc.desktop("record on");
    headset
        .wait(SOON, "the microphone wanted", |v| {
            v.pc.up_wanted.get(&UpKind::Mic).is_some_and(|u| u.on)
        })
        .unwrap();
    headset.send_mic(150).unwrap();
    pc.wait_log(SOON, "fake desktop: the microphone got");
    pc.wait_log(SOON, "the headset's microphone stopped");
    pc.desktop("record off");
    headset
        .wait(SOON, "the microphone no longer wanted", |v| {
            v.pc.up_wanted.get(&UpKind::Mic).is_some_and(|u| !u.on)
        })
        .unwrap();

    let denied = UpState {
        kind: UpKind::Mic,
        switch: true,
        permission: permission::DENIED,
    };
    headset.send(&ClientMsg::UpState { part: denied }).unwrap();
    pc.wait_log(SOON, "the headset's microphone has no permission");
}

/// The PC keeps up with the headset however many small messages it sends: with three
/// seconds of microphone sound arriving at once (and a sound system as slow to ask as
/// the real one), a request after them is answered at once, not after the PC worked
/// through the pile one pass at a time.
#[test]
fn a_request_is_answered_at_once_while_the_microphone_streams() {
    let (_dir, pc, _keys, mut headset) = session("busy", &Options::default());
    pc.desktop("record on");
    headset
        .wait(SOON, "the microphone wanted", |v| {
            v.pc.up_wanted.get(&UpKind::Mic).is_some_and(|u| u.on)
        })
        .unwrap();
    headset.send_mic(300).unwrap();
    let asked = Instant::now();
    let req = headset
        .request(Request::SetSound { to_headset: true })
        .unwrap();
    assert_eq!(headset.reply(req, SOON).unwrap(), Outcome::Done);
    let took = asked.elapsed();
    assert!(took < Duration::from_secs(1), "answered after {took:?}");
}

/// The microphone switched off and on again quickly: the end of the first recording
/// comes after the start of the second, under its own number, and ends nothing of the
/// second, which goes on reaching the PC.
#[test]
fn a_quick_off_and_on_keeps_the_new_recording() {
    use prostir_proto::state::UpFormat;
    let (_dir, mut pc, _keys, mut headset) = session("mic-again", &Options::default());
    pc.desktop("record on");
    headset
        .wait(SOON, "the microphone wanted", |v| {
            v.pc.up_wanted.get(&UpKind::Mic).is_some_and(|u| u.on)
        })
        .unwrap();
    let start = |up| ClientMsg::UpStart {
        up,
        kind: UpKind::Mic,
        format: UpFormat::Pcm16 {
            rate: prostir_proto::AUDIO_RATE,
            channels: 1,
        },
    };
    let piece = |up| ClientMsg::Up {
        up,
        data: vec![0; 960],
    };
    headset.send(&start(0)).unwrap();
    headset.send(&piece(0)).unwrap();
    headset.send(&start(1)).unwrap();
    headset.send(&ClientMsg::UpEnd { up: 0 }).unwrap();
    // A second of sound in the new recording.
    for _ in 0..100 {
        headset.send(&piece(1)).unwrap();
    }
    pc.wait_log(SOON, "fake desktop: the microphone got");
    assert!(
        pc.lines("the headset's microphone stopped").is_empty(),
        "the old end ended the new recording"
    );
    headset.send(&ClientMsg::UpEnd { up: 1 }).unwrap();
    pc.wait_log(SOON, "the headset's microphone stopped (up stream 1)");
}

/// How many times the PC's thread that follows the sound system woke since it started.
/// The kernel counts a wake when the thread goes to sleep again, so a thread that has
/// just been woken (and, on a busy machine, not run yet) is counted only later.
fn watcher_woke(pc: &Pc) -> Option<u64> {
    pc.wakeups()
        .into_values()
        .find(|(name, _)| name == "sound-events")
        .map(|(_, n)| n)
}

/// The count of [`watcher_woke`] once it stays the same for a while: the thread is
/// asleep again after whatever woke it.
fn watcher_asleep(pc: &Pc) -> u64 {
    let until = Instant::now() + SOON;
    let mut last = watcher_woke(pc).expect("no sound-events thread");
    loop {
        std::thread::sleep(Duration::from_millis(300));
        let now = watcher_woke(pc).expect("the sound-events thread went");
        if now == last {
            return now;
        }
        assert!(
            Instant::now() < until,
            "the sound-events thread never sleeps"
        );
        last = now;
    }
}

/// Whether the headset is asked for its microphone.
fn mic_wanted(view: &View) -> bool {
    view.pc.up_wanted.get(&UpKind::Mic).is_some_and(|u| u.on)
}

/// Everything the sound system said so far has been looked at: the slider moved to
/// `level`, the PC asked the headset to take it. The PC reads the sound system's lines
/// in order, so its looks at the lines before are in the log by then.
fn sound_looked_at(pc: &mut Pc, level: u8) {
    const ASKING: &str = "volume: asking the headset for";
    let before = pc.count_log(&format!("{ASKING} {level}"));
    pc.desktop(&format!("volume {level}"));
    pc.wait_log_after(SOON, &format!("{ASKING} {level}"), before);
}

/// The PC learns that a program records from the sound system's event at once, not
/// on a clock: nothing asks the sound system while nothing changes, and a recording
/// that begins or ends is answered within a moment, with one look each (the lines of
/// one change come together).
#[test]
fn a_recording_is_seen_from_the_sound_systems_event() {
    let (_dir, mut pc, _keys, mut headset) = session("recording-event", &Options::default());
    const ASKED: &str = "fake desktop: asked whether a program records";
    pc.wait_log(SOON, "sound: following the sound system");
    pc.wait_log(SOON, "headset microphone ready");
    wait_first_volume(&mut pc, &mut headset);
    sound_looked_at(&mut pc, 55);
    let asked = pc.count_log(ASKED);
    headset.take_for(Duration::from_secs(2)).unwrap();
    assert_eq!(pc.count_log(ASKED), asked, "asked while nothing changed");

    for (on, what, level) in [(true, "record on", 65), (false, "record off", 75)] {
        let asked = pc.count_log(ASKED);
        let started = Instant::now();
        pc.desktop(what);
        headset
            .wait(SOON, what, |v| {
                v.pc.up_wanted.get(&UpKind::Mic).is_some_and(|u| u.on == on)
            })
            .unwrap();
        let took = started.elapsed();
        // Through a file the fake desktop watches, the sound system and the network:
        // within the second a request is answered in.
        assert!(took < Duration::from_secs(1), "{what} seen after {took:?}");
        sound_looked_at(&mut pc, level);
        assert_eq!(pc.count_log(ASKED), asked + 1, "{what}: one look");
    }
    let after = pc.count_log(ASKED);
    headset.take_for(Duration::from_secs(1)).unwrap();
    assert_eq!(pc.count_log(ASKED), after, "asked while nothing changed");
}

/// While the sound is idle, the thread that follows the sound system sleeps: it wakes
/// only when the sound system says something, and is asleep again after.
#[test]
fn the_sound_watcher_sleeps_while_the_sound_is_idle() {
    let (_dir, mut pc, _keys, mut headset) = session("sound-idle", &Options::default());
    pc.wait_log(SOON, "sound: following the sound system");
    // The first volume and its echo.
    headset.take_for(Duration::from_secs(1)).unwrap();
    for round in 0..2 {
        let before = watcher_asleep(&pc);
        headset.take_for(Duration::from_secs(3)).unwrap();
        let after = watcher_woke(&pc).expect("the sound-events thread went");
        assert_eq!(
            after,
            before,
            "round {round}: woke {} times",
            after - before
        );
        // A change wakes it, and the headset is asked once.
        let asks = headset.view().volume_asks.len();
        pc.desktop(&format!("volume {}", 30 + round * 20));
        headset
            .wait(SOON, "a request for the volume", |v| {
                v.volume_asks.len() > asks
            })
            .unwrap();
        // The headset may be asked before the thread is asleep again, and only then
        // is its wake counted.
        let until = Instant::now() + SOON;
        while watcher_woke(&pc).unwrap() <= after {
            assert!(Instant::now() < until, "the change did not wake it");
            std::thread::sleep(Duration::from_millis(20));
        }
        headset.take_for(Duration::from_millis(500)).unwrap();
        assert_eq!(headset.view().volume_asks.len(), asks + 1);
    }
    // With the headset gone, so are the sound devices and the thread that follows them
    // (the monitors stay for a while).
    headset.disconnect();
    let until = Instant::now() + SOON;
    while watcher_woke(&pc).is_some() {
        assert!(
            Instant::now() < until,
            "the sound-events thread outlived the sound devices"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The sound system's events end (as when `pactl subscribe` dies): the PC follows it
/// again after a pause, and what changed meanwhile comes then: the slider asks the
/// headset, a program that began recording gets the microphone.
#[test]
fn the_sound_watcher_starts_again_and_catches_up() {
    let (_dir, mut pc, _keys, mut headset) = session("sound-again", &Options::default());
    pc.wait_log(SOON, "sound: following the sound system");
    wait_first_volume(&mut pc, &mut headset);
    let asks = headset.view().volume_asks.len();
    pc.desktop("sound-events stop");
    pc.wait_log(SOON, "sound: the sound system's events stopped");
    // Meanwhile nothing is said: the slider moves, a program records.
    pc.desktop("volume 60\nrecord on");
    pc.wait_log(SOON, "fake desktop: a program records: on");
    headset.take_for(Duration::from_millis(300)).unwrap();
    assert_eq!(
        headset.view().volume_asks.len(),
        asks,
        "asked without events"
    );
    assert!(!mic_wanted(headset.view()), "wanted without events");
    // Again after the first pause (a second), with what changed.
    pc.wait_log(SOON, "sound: following the sound system again");
    headset
        .wait(SOON, "the microphone wanted", mic_wanted)
        .unwrap();
    wait_written(&pc, &mut headset, quest_step(60));
    assert_eq!(headset.view().volume_asks.len(), asks + 1);
    assert_eq!(headset.volume(), Some(quest_step(60)));
    // And it follows on.
    pc.desktop("record off");
    headset
        .wait(SOON, "the microphone no longer wanted", |v| {
            v.pc.up_wanted.get(&UpKind::Mic).is_some_and(|u| !u.on)
        })
        .unwrap();
    assert_eq!(
        pc.count_log("fake desktop: the sound system's events start"),
        2
    );
}

/// The sound device cannot be made and the microphone is off: the PC keeps trying to
/// make the device, and keeps one subscription to the sound system meanwhile instead
/// of starting `pactl` again with every try.
#[test]
fn the_sound_watcher_stays_while_the_device_cannot_be_made() {
    let options = Options {
        env: vec![("PROSTIR_FAKE_NO_SOUND".into(), "1".into())],
        ..Options::default()
    };
    let (_dir, mut pc, _keys, mut headset) = session("sound-none", &options);
    let off = UpState {
        kind: UpKind::Mic,
        switch: false,
        permission: permission::GRANTED,
    };
    headset.send(&ClientMsg::UpState { part: off }).unwrap();
    pc.wait_log(SOON, "no sound in the headset");
    pc.wait_log(SOON, "sound: following the sound system");
    // Through several of the PC's half-second rounds and a try to make the device.
    headset.take_for(Duration::from_secs(5)).unwrap();
    assert!(
        pc.count_log("no sound in the headset") >= 2,
        "tried once only"
    );
    assert_eq!(
        pc.count_log("fake desktop: the sound system's events start"),
        1,
        "the watcher came and went"
    );
    assert!(watcher_woke(&pc).is_some(), "no watcher");
}

/// The sound system went away in the middle of a session (PipeWire restarted) and took
/// the headset's sound device and microphone with it, saying nothing but that they
/// are gone: the PC notices within a few seconds, says the sound is not in the
/// headset meanwhile, and makes both again, so its sound does not stay on its own
/// speakers. The session goes on.
#[test]
fn sound_and_microphone_come_back_after_the_sound_system_went() {
    const DEVICE: &str = "fake desktop: the headset's sound device made";
    const MIC: &str = "fake desktop: the headset microphone made";
    const OFF: &str = "sound: PC sound stays on the PC";
    const ON: &str = "sound: PC sound HeadsetOnly";
    let (_dir, mut pc, _keys, mut headset) = session("sound-gone", &Options::default());
    pc.wait_log(SOON, "headset microphone ready");
    let (devices, mics) = (pc.count_log(DEVICE), pc.count_log(MIC));
    let (offs, ons) = (pc.count_log(OFF), pc.count_log(ON));
    pc.desktop("sound-gone");
    pc.wait_log(
        SOON,
        "fake desktop: the sound system lost Prostir's devices",
    );
    let gone = Instant::now();
    pc.wait_log_after(SOON, OFF, offs);
    pc.wait_log_after(SOON, DEVICE, devices);
    pc.wait_log_after(SOON, MIC, mics);
    let back = pc.wait_log_after(SOON, ON, ons);
    eprintln!(
        "sound back in the headset {:?} after it went",
        gone.elapsed()
    );
    assert!(
        pc.lines(OFF)[offs..]
            .iter()
            .any(|l| l.contains("microphone is not ready")),
        "the microphone said gone meanwhile: {:?}",
        pc.lines("sound: ")
    );
    // The status ends where it was.
    let until = Instant::now() + SOON;
    while !pc
        .lines("sound: ")
        .last()
        .is_some_and(|l| l.contains("HeadsetOnly") && l.contains("headset microphone ready"))
    {
        assert!(Instant::now() < until, "{back}: {:?}", pc.lines("sound: "));
        headset.take_for(Duration::from_millis(100)).unwrap();
    }
    assert_eq!(headset.view().goodbye, None, "still in the session");
}

/// The sound switched off and on (or the headset taken off and put on) makes the
/// headset's sound device anew, at the sound system's level for a new device: the PC
/// puts the headset's level on it, and asks the headset nothing.
#[test]
fn a_new_sound_device_takes_the_headsets_level() {
    let (_dir, mut pc, _keys, mut headset) = session("sound-anew", &Options::default());
    wait_first_volume(&mut pc, &mut headset);
    headset.take_for(Duration::from_millis(500)).unwrap();
    let asks = headset.view().volume_asks.len();
    let before = written(&pc).len();
    // Off, and on once the device is gone: the sound's worker takes the latest wish,
    // so both at once would leave the device as it is.
    let removed = pc.count_log("fake desktop: the headset's sound device removed");
    for on in [false, true] {
        let req = headset
            .request(Request::SetSound { to_headset: on })
            .unwrap();
        assert_eq!(headset.reply(req, SOON).unwrap(), Outcome::Done);
        if !on {
            pc.wait_log_after(
                SOON,
                "fake desktop: the headset's sound device removed",
                removed,
            );
        }
    }
    pc.wait_log_after(SOON, "fake desktop: the headset's sound device made", 1);
    let until = Instant::now() + SOON;
    while written(&pc).len() == before {
        assert!(Instant::now() < until, "the new device was not set");
        headset.take_for(Duration::from_millis(50)).unwrap();
    }
    headset.take_for(Duration::from_secs(1)).unwrap();
    assert_eq!(
        written(&pc)[before..],
        [FIRST_VOLUME.to_string()],
        "the headset's level, once"
    );
    assert_eq!(
        headset.view().volume_asks.len(),
        asks,
        "the headset was asked"
    );
    assert_eq!(headset.volume(), Some(FIRST_VOLUME));
}

/// A stand-in `pactl` (`fake-pactl/pactl`, first on the PC app's `PATH`) whose sound
/// system is files in `dir`: the test moves the slider and starts recordings there.
struct FakePactl {
    dir: TestDir,
    index: u32,
}

impl FakePactl {
    fn new(name: &str) -> Self {
        let dir = TestDir::new(name);
        let index = 50;
        let at = |f: &str| dir.path().join(f);
        std::fs::write(at("sink"), format!("{index} prostir_headset\n")).unwrap();
        std::fs::write(at("volume"), "100\n").unwrap();
        std::fs::write(at("mute"), "no\n").unwrap();
        std::fs::write(at("mic"), "51 prostir_mic IDLE\n").unwrap();
        std::fs::write(at("events"), "").unwrap();
        Self { dir, index }
    }

    /// The PC app's environment for it.
    fn env(&self) -> Vec<(String, String)> {
        let tools = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/e2e/fake-pactl");
        let path = std::env::var("PATH").unwrap_or_default();
        vec![
            ("PROSTIR_FAKE_PACTL".into(), "1".into()),
            (
                "FAKE_PACTL_DIR".into(),
                self.dir.path().to_string_lossy().into_owned(),
            ),
            ("PATH".into(), format!("{tools}:{path}")),
        ]
    }

    fn read(&self, file: &str) -> String {
        std::fs::read_to_string(self.dir.path().join(file))
            .unwrap_or_default()
            .trim()
            .to_owned()
    }

    fn write(&self, file: &str, text: &str) {
        std::fs::write(self.dir.path().join(file), format!("{text}\n")).unwrap();
    }

    /// What `pactl subscribe` says next.
    fn say(&self, line: &str) {
        use std::io::Write;
        let mut events = std::fs::OpenOptions::new()
            .append(true)
            .open(self.dir.path().join("events"))
            .unwrap();
        writeln!(events, "{line}").unwrap();
    }

    /// The slider moved to `level`.
    fn slide(&self, level: u8) {
        self.write("volume", &level.to_string());
        self.say(&format!("Event 'change' on sink #{}", self.index));
    }

    /// How many times `pactl subscribe` was started.
    fn subscriptions(&self) -> usize {
        self.read("calls")
            .lines()
            .filter(|l| *l == "subscribe")
            .count()
    }
}

/// The PC's own `pactl` code, as on Linux, against a stand-in `pactl`: the headset's
/// level goes to the device on a thread of its own, the slider asks the headset, a
/// program recording gets the microphone, and a `pactl subscribe` that dies starts
/// again and catches up with what changed meanwhile.
#[test]
fn the_pc_follows_pactl() {
    let pactl = FakePactl::new("pactl-state");
    let options = Options {
        env: pactl.env(),
        ..Options::default()
    };
    let (_dir, mut pc, _keys, mut headset) = session("pactl", &options);
    let set = |level: u8| format!("volume: the headset's device at {level}");
    pc.wait_log(SOON, &set(FIRST_VOLUME));
    assert_eq!(pactl.read("volume"), FIRST_VOLUME.to_string());
    let pc_caps = headset.view().pc.caps.clone().unwrap();
    assert!(pc_caps.has(prostir_proto::state::caps::pc::VOLUME));
    headset.take_for(Duration::from_millis(500)).unwrap();
    let asks = headset.view().volume_asks.len();

    // The slider: one request, and the headset's step on the device.
    pactl.slide(21);
    pc.wait_log(SOON, &set(quest_step(21)));
    assert_eq!(pactl.read("volume"), quest_step(21).to_string());
    headset.take_for(Duration::from_millis(500)).unwrap();
    assert_eq!(headset.view().volume_asks.len(), asks + 1);

    // A program records, then stops.
    pactl.write("mic", "51 prostir_mic RUNNING");
    pactl.say("Event 'new' on source-output #77");
    headset
        .wait(SOON, "the microphone wanted", mic_wanted)
        .unwrap();
    pactl.write("mic", "51 prostir_mic IDLE");
    pactl.say("Event 'remove' on source-output #77");
    headset
        .wait(SOON, "the microphone no longer wanted", |v| !mic_wanted(v))
        .unwrap();

    // `pactl subscribe` dies; the slider moves meanwhile.
    let pid: libc::pid_t = pactl.read("subscribe.pid").parse().unwrap();
    // SAFETY: a signal to the stand-in this test started through the PC app.
    unsafe { libc::kill(pid, libc::SIGKILL) };
    pc.wait_log(SOON, "sound: the sound system's events stopped");
    pactl.write("volume", "60");
    pc.wait_log(SOON, "sound: following the sound system again");
    pc.wait_log(SOON, &set(quest_step(60)));
    headset.take_for(Duration::from_millis(500)).unwrap();
    assert_eq!(headset.view().volume_asks.len(), asks + 2);
    assert_eq!(headset.volume(), Some(quest_step(60)));
    assert_eq!(pactl.subscriptions(), 2);
}

/// A `pactl` too old to read a device's level (before PulseAudio 14): the PC does not
/// offer the headset its volume, and never sets the device.
#[test]
fn an_old_pactl_shares_no_volume() {
    let pactl = FakePactl::new("pactl-old-state");
    let mut env = pactl.env();
    env.push(("FAKE_PACTL_VERSION".into(), "13.99.1".into()));
    let options = Options {
        env,
        ..Options::default()
    };
    let (_dir, mut pc, _keys, mut headset) = session("pactl-old", &options);
    pc.wait_log(SOON, "volume: not shared with the headset");
    headset.take_for(Duration::from_secs(1)).unwrap();
    let pc_caps = headset.view().pc.caps.clone().unwrap();
    assert!(!pc_caps.has(prostir_proto::state::caps::pc::VOLUME));
    assert!(!pactl.read("calls").contains("set-sink-volume"));
    assert_eq!(pactl.read("volume"), "100");
}

/// "On the PC" from the PC (the settings window, `prostir ctl sound pc`) keeps the
/// PC's sound on the PC, "In the headset" sends it to the headset: the request says
/// which in the log, and the state follows.
#[test]
fn the_sound_goes_where_the_pc_says() {
    let (_dir, mut pc, _keys, _headset) = session("sound-route", &Options::default());
    pc.wait_log(SOON, "sound: PC sound HeadsetOnly");
    let Some(on_pc) = pc.ctl(&["--json", "sound", "pc"]) else {
        return;
    };
    assert_eq!(on_pc.code, 0, "{on_pc:?}");
    let asked = pc.wait_log(SOON, "sound_to_headset");
    assert!(
        asked.contains("sound_to_headset") && asked.contains(r#""on":false"#),
        "{asked}"
    );
    pc.wait_log(SOON, "sound: PC sound stays on the PC");
    let state = pc.ctl(&["--json", "status"]).unwrap().json();
    assert_eq!(state["sound"]["to_headset"], false, "{state}");
    let back = pc.count_log("sound: PC sound HeadsetOnly");
    let to_headset = pc.ctl(&["--json", "sound", "headset"]).unwrap();
    assert_eq!(to_headset.code, 0, "{to_headset:?}");
    let asked = pc.wait_log_after(SOON, "sound_to_headset", 1);
    assert!(asked.contains(r#""on":true"#), "{asked}");
    pc.wait_log_after(SOON, "sound: PC sound HeadsetOnly", back);
    let state = pc.ctl(&["--json", "status"]).unwrap().json();
    assert_eq!(state["sound"]["to_headset"], true, "{state}");
}
