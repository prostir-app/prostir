//! The local API on a session bus of the test's own (`dbus-daemon --session`; every
//! test here is skipped, saying so, where it is not installed): the state and its
//! changes, requests as JSON and as typed methods, `prostir ctl` and its exit codes,
//! the headsets' events, and what a guest is refused.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use prostir_api::{BUS_NAME, INTERFACE, OBJECT_PATH, State};
use prostir_proto::request::{Asked, Command as HeadsetCommand};
use prostir_testkit::View;
use zbus::blocking::Connection;

use crate::pc::{Bus, Options, Pc, TestDir, ctl_on};
use crate::{SOON, START, TWO_MONITORS, connect, session, worn};

/// Calls `method` of the PC app with `body`.
fn call<B>(bus: &Connection, method: &str, body: &B) -> zbus::Result<zbus::Message>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    bus.call_method(Some(BUS_NAME), OBJECT_PATH, Some(INTERFACE), method, body)
}

/// The state now, and its revision.
fn get_state(bus: &Connection) -> (u32, State) {
    let (revision, json): (u32, String) = call(bus, "GetState", &())
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    (revision, State::from_json(&json).unwrap())
}

/// The name of the D-Bus error `e`, after the prefix of the PC app's errors.
fn error_name(e: &zbus::Error) -> String {
    match e {
        zbus::Error::MethodError(name, _, _) => name
            .as_str()
            .strip_prefix(prostir_api::ERROR_PREFIX)
            .unwrap_or(name.as_str())
            .to_owned(),
        other => panic!("not a method error: {other}"),
    }
}

/// Every `StateChanged` from now on: its revision and state.
fn state_changes(bus: &Connection) -> mpsc::Receiver<(u32, State)> {
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface(INTERFACE)
        .unwrap()
        .member("StateChanged")
        .unwrap()
        .build();
    let messages = zbus::blocking::MessageIterator::for_match_rule(rule, bus, None).unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for msg in messages.flatten() {
            let (revision, json): (u32, String) = msg.body().deserialize().unwrap();
            if tx
                .send((revision, State::from_json(&json).unwrap()))
                .is_err()
            {
                return;
            }
        }
    });
    rx
}

/// A request's answer (as JSON) and the states (revision and state) that came before
/// it.
type Seen = (serde_json::Value, Vec<(u32, State)>);

/// What one connection sees, in order, after it sent `requests` through `Call`
/// without waiting: for each, its answer (as JSON) and the states (revision and
/// state) that came before it.
fn calls_in_order(bus: &Connection, requests: &[String]) -> Vec<Seen> {
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface(INTERFACE)
        .unwrap()
        .member("StateChanged")
        .unwrap()
        .build();
    zbus::blocking::fdo::DBusProxy::new(bus)
        .unwrap()
        .add_match_rule(rule)
        .unwrap();
    // Every message to this connection, in the order the bus sends them.
    let messages = zbus::blocking::MessageIterator::from(bus);
    let mut serials = Vec::new();
    for request in requests {
        let call = zbus::Message::method_call(OBJECT_PATH, "Call")
            .unwrap()
            .destination(BUS_NAME)
            .unwrap()
            .interface(INTERFACE)
            .unwrap()
            .build(&(request.as_str(),))
            .unwrap();
        serials.push(call.primary_header().serial_num());
        bus.send(&call).unwrap();
    }
    let mut states = Vec::new();
    let mut answers: Vec<Option<Seen>> = vec![None; requests.len()];
    for msg in messages {
        let msg = msg.unwrap();
        let header = msg.header();
        match header.message_type() {
            zbus::message::Type::Signal
                if header
                    .member()
                    .is_some_and(|m| m.as_str() == "StateChanged") =>
            {
                let (revision, json): (u32, String) = msg.body().deserialize().unwrap();
                states.push((revision, State::from_json(&json).unwrap()));
            }
            zbus::message::Type::MethodReturn => {
                let Some(i) = serials
                    .iter()
                    .position(|s| Some(*s) == header.reply_serial())
                else {
                    continue;
                };
                let json: String = msg.body().deserialize().unwrap();
                answers[i] = Some((serde_json::from_str(&json).unwrap(), states.clone()));
            }
            zbus::message::Type::Error => panic!("{msg:?}"),
            _ => {}
        }
        if answers.iter().all(Option::is_some) {
            break;
        }
    }
    answers.into_iter().map(Option::unwrap).collect()
}

/// How each display encodes is in the state (`encoding.outputs`), with why one is
/// not on the best encoder for it, and the tray says so too: here a hardware encoder
/// that opens one session, so the second monitor goes on with another.
#[test]
fn the_state_says_how_each_display_encodes() {
    let options = Options {
        software: false,
        encoder: Some("sessions=1"),
        ..Options::default()
    };
    let (_dir, mut pc, _keys, _headset) = session("api-encoders", &options);
    pc.wait_log(START, "fake hardware opens at most 1 sessions at once");
    pc.wait_log(START, "encoding: Prostir-2: ");
    let Some(bus) = &pc.bus else {
        return;
    };
    let bus = bus.connect();
    let until = Instant::now() + SOON;
    loop {
        let (_, state) = get_state(&bus);
        let outputs = &state.encoding.outputs;
        if outputs.len() == 2 && outputs[1].note.is_some() {
            assert_eq!(outputs[0].encoder, "fake hardware", "{outputs:?}");
            assert!(outputs[0].hardware && outputs[0].note.is_none());
            assert_eq!(outputs[0].display, state.displays[0].id);
            assert_ne!(outputs[1].encoder, "fake hardware");
            let note = outputs[1].note.as_deref().unwrap();
            assert!(note.contains("fake hardware"), "{note}");
            return;
        }
        assert!(Instant::now() < until, "{outputs:?}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// An answer goes out after the state with its change, for a request the PC carries
/// out and for a command the headset answers (whose change, its "No limits", comes
/// from the headset just before its answer).
#[test]
fn answers_come_after_their_state() {
    let (_dir, pc, _keys, _headset) = session("api-order", &Options::default());
    let Some(bus) = &pc.bus else {
        return;
    };
    let bus = bus.connect();
    let add = r#"{"request":"add_monitor","w":320,"h":240}"#.to_owned();
    let (answer, before) = calls_in_order(&bus, &[add]).remove(0);
    let id = answer["id"].as_u64().unwrap() as u32;
    let (revision, state) = before.last().expect("a state before the answer");
    assert_eq!(*revision, answer["revision"].as_u64().unwrap() as u32);
    assert!(
        state.display(id).is_some(),
        "the state before it has the monitor"
    );
    for on in [true, false] {
        let request = format!(r#"{{"request":"no_limits","on":{on}}}"#);
        let (answer, before) = calls_in_order(&bus, &[request]).remove(0);
        let (revision, state) = before.last().expect("a state before the answer");
        assert_eq!(*revision, answer["revision"].as_u64().unwrap() as u32);
        let (_, session) = state.connected().unwrap();
        assert_eq!(
            session.no_limits,
            Some(on),
            "the state before it has the change"
        );
    }
}

/// Forty changes asked at once: `StateChanged` goes out ten times a second at most,
/// and each answer still comes after a state with its change.
#[test]
fn state_changes_are_ten_a_second_at_most() {
    let (_dir, pc, _keys, _headset) = session("api-rate", &Options::default());
    let Some(bus) = &pc.bus else {
        return;
    };
    let bus = bus.connect();
    let requests: Vec<String> = (0..40)
        .map(|i| format!(r#"{{"request":"set_quality","mbit":{}}}"#, 10 + i))
        .collect();
    let started = Instant::now();
    let answers = calls_in_order(&bus, &requests);
    let took = started.elapsed();
    let (_, states) = answers.iter().max_by_key(|(_, s)| s.len()).unwrap();
    let allowed = (took.as_secs_f64() * 10.0).ceil() as usize + 1;
    assert!(
        states.len() <= allowed,
        "{} states in {took:?}",
        states.len()
    );
    for (i, (answer, before)) in answers.iter().enumerate() {
        let (revision, state) = before.last().expect("a state before the answer");
        assert!(*revision >= answer["revision"].as_u64().unwrap() as u32);
        assert!(
            state.encoding.bitrate >= 10 + i as u32,
            "answer {i}: {}",
            state.encoding.bitrate
        );
    }
    let (_, last) = answers.last().unwrap().1.last().unwrap();
    assert_eq!(last.encoding.bitrate, 49);
}

/// The whole state, and a request that changes the desktop: its answer comes at once
/// with the new monitor's id, after the `StateChanged` that has the monitor in it.
#[test]
fn the_state_and_its_changes() {
    let (_dir, pc, _keys, mut headset) = session("api-state", &Options::default());
    let Some(bus) = &pc.bus else {
        return;
    };
    let bus = bus.connect();
    // The control loop's pass after the session opened says so.
    let started = Instant::now();
    let (revision, state) = loop {
        let (revision, state) = get_state(&bus);
        if state.pc.phase == prostir_api::Phase::Live || started.elapsed() > SOON {
            break (revision, state);
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(revision > 0);
    assert_eq!(state.v, prostir_api::STATE_VERSION);
    assert_eq!(state.pc.phase, prostir_api::Phase::Live);
    assert_eq!(state.pc.name, "fake-pc");
    // `PROSTIR_NO_COUNT=1`, as every test runs: nothing to ask.
    assert_eq!(state.counting, None);
    let (h, s) = state.connected().expect("the headset's session");
    assert_eq!(h.name, "headset");
    assert!(s.main && s.worn);
    assert_eq!(state.displays.len(), 2);
    assert!(
        state
            .displays
            .iter()
            .all(|d| d.state == prostir_api::DisplayState::Running && !d.output.is_empty()),
        "{:?}",
        state.displays
    );
    let changes = state_changes(&bus);
    let started = Instant::now();
    let reply: String = call(
        &bus,
        "Call",
        &(r#"{"request":"add_monitor","w":320,"h":240}"#,),
    )
    .unwrap()
    .body()
    .deserialize()
    .unwrap();
    let answer: serde_json::Value = serde_json::from_str(&reply).unwrap();
    let id = answer["id"].as_u64().expect("the monitor's id") as u32;
    let answered = answer["revision"].as_u64().unwrap() as u32;
    assert!(answered > revision, "{reply}");
    assert!(started.elapsed() < SOON);
    // The change went out first.
    let (changed, state) = changes.recv_timeout(SOON).expect("StateChanged");
    assert!(changed <= answered);
    let mut state = state;
    while state.display(id).is_none() {
        state = changes.recv_timeout(SOON).expect("StateChanged with it").1;
    }
    let added = state.display(id).unwrap();
    assert_eq!((added.mode.w, added.mode.h), (320, 240));
    assert_ne!(added.state, prostir_api::DisplayState::Failed);
    headset
        .wait(SOON, "the new monitor", |v| v.order().len() == 3)
        .unwrap();
    // What does not read is refused with the reason's name.
    let bad = call(&bus, "Call", &("recenter",)).unwrap_err();
    assert_eq!(error_name(&bad), "BadArgument");
    let unknown = call(&bus, "Call", &(r#"{"request":"fly"}"#,)).unwrap_err();
    assert_eq!(error_name(&unknown), "Unsupported");
    let last = call(&bus, "RemoveMonitor", &(99_999u32,)).unwrap_err();
    assert_eq!(error_name(&last), "UnknownDisplay");
}

/// `prostir ctl` without Prostir: at once, exit 2, nothing started; wrong arguments
/// 64 whatever runs.
#[test]
fn ctl_without_prostir() {
    let dir = TestDir::new("api-none");
    let Some(bus) = Bus::start(dir.path()) else {
        return;
    };
    let started = Instant::now();
    let status = ctl_on(&bus.address, &["status"]);
    assert_eq!(status.code, 2, "{status:?}");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(status.out.trim(), "Not running");
    let json = ctl_on(&bus.address, &["status", "--json"]);
    assert_eq!(json.json()["running"], false);
    assert_eq!(ctl_on(&bus.address, &["recenter"]).code, 2);
    let wrong = ctl_on(&bus.address, &["cinema", "maybe"]);
    assert_eq!(wrong.code, 64, "{wrong:?}");
    assert!(wrong.err.contains("on or off"), "{wrong:?}");
    assert!(
        !bus.connect()
            .call_method(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                Some("org.freedesktop.DBus"),
                "NameHasOwner",
                &(BUS_NAME,),
            )
            .unwrap()
            .body()
            .deserialize::<bool>()
            .unwrap(),
        "nothing took the name"
    );
}

/// The typed methods, as `busctl` calls them from a script or a hot key.
#[test]
fn typed_methods_from_busctl() {
    let (_dir, pc, _keys, mut headset) = session("api-busctl", &Options::default());
    let Some(bus) = &pc.bus else {
        return;
    };
    let busctl = |args: &[&str]| {
        Command::new("busctl")
            .arg(format!("--address={}", bus.address))
            // `--`: what follows is an argument, not an option.
            .args(["call", "--", BUS_NAME, OBJECT_PATH, INTERFACE])
            .args(args)
            .stdin(Stdio::null())
            .output()
    };
    let recenter = match busctl(&["Recenter"]) {
        Ok(r) => r,
        Err(e) if std::env::var_os("CI").is_some() => panic!("no busctl in CI: {e}"),
        Err(e) => {
            eprintln!("\n*** no busctl ({e}): the typed methods are SKIPPED ***\n");
            return;
        }
    };
    assert!(recenter.status.success(), "{recenter:?}");
    let profile = busctl(&["Profile", "s", "Робота"]).unwrap();
    assert!(profile.status.success(), "{profile:?}");
    headset
        .wait(SOON, "the commands", |v| v.commands.len() == 2)
        .unwrap();
    assert_eq!(
        headset.view().commands,
        [
            Asked::Known(HeadsetCommand::Recenter),
            Asked::Known(HeadsetCommand::Profile {
                name: "Робота".into()
            }),
        ]
    );
    let added = busctl(&["AddMonitor", "uuq", "320", "240", "100"]).unwrap();
    assert!(added.status.success(), "{added:?}");
    let out = String::from_utf8_lossy(&added.stdout);
    let id: u32 = out
        .trim()
        .strip_prefix("u ")
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("{out}"));
    headset
        .wait(SOON, "the new monitor", |v| {
            v.order().contains(&prostir_proto::state::DisplayId(id))
        })
        .unwrap();
    let removed = busctl(&["RemoveMonitor", "u", &id.to_string()]).unwrap();
    assert!(removed.status.success(), "{removed:?}");
    let gone = busctl(&["RemoveMonitor", "u", &id.to_string()]).unwrap();
    assert!(!gone.status.success(), "a monitor removed twice");
    let quality = busctl(&["SetQuality", "u", "15"]).unwrap();
    assert!(quality.status.success(), "{quality:?}");
    headset
        .wait(SOON, "the quality", |v| {
            v.encoding().is_some_and(|e| e.wanted.bitrate == 15)
        })
        .unwrap();
}

/// `ctl watch --json` sees the headset connect, come off the head and back, and go.
#[test]
fn ctl_watch_sees_the_headset() {
    let dir = TestDir::new("api-watch");
    dir.save_monitors(TWO_MONITORS);
    let mut keys = dir.pair();
    let pc = Pc::start(&dir, &Options::default());
    let Some(bus) = &pc.bus else {
        return;
    };
    let mut watch = Command::new(env!("CARGO_BIN_EXE_prostir"))
        .args(["ctl", "watch", "--json"])
        .env("DBUS_SESSION_BUS_ADDRESS", &bus.address)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let (tx, lines) = mpsc::channel();
    let out = watch.stdout.take().unwrap();
    std::thread::spawn(move || {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            let value: serde_json::Value = serde_json::from_str(&line).unwrap();
            if tx.send(value).is_err() {
                return;
            }
        }
    });
    let next = |event: &str| loop {
        let line = lines
            .recv_timeout(START)
            .unwrap_or_else(|_| panic!("no {event}"));
        if line["event"] == event {
            return line;
        }
    };
    // It says the state it starts from.
    next("state_changed");
    let mut headset = connect(&pc, &mut keys);
    let connected = next("headset_connected");
    assert_eq!(connected["name"], "headset");
    // From 127.0.0.1: as through `adb reverse`.
    assert_eq!(connected["via"], "usb");
    let key = connected["headset"].as_str().unwrap().to_owned();
    assert_eq!(key.len(), 12);
    headset
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    headset.send(&worn(false)).unwrap();
    let off = next("headset_worn");
    assert_eq!(
        (off["headset"].as_str(), off["worn"].as_bool()),
        (Some(key.as_str()), Some(false))
    );
    headset.send(&worn(true)).unwrap();
    assert_eq!(next("headset_worn")["worn"], true);
    headset.disconnect();
    let gone = next("headset_disconnected");
    assert_eq!(gone["headset"], key.as_str());
    assert_eq!(gone["reason"], "other");
    let _ = watch.kill();
    let _ = watch.wait();
}

/// A guest (a headset its line in `settings.txt` makes one) is someone else's
/// headset: with only a guest connected, commands from the PC are refused, while
/// the PC's own settings still change.
#[test]
fn a_guest_takes_no_commands_from_the_pc() {
    let dir = TestDir::new("api-guest");
    dir.save_monitors(TWO_MONITORS);
    let mut keys = dir.pair();
    let key = prostir_proto::secure::to_hex(&keys.identity.public);
    dir.save_settings(&format!("access {key} guest input\n"));
    let pc = Pc::start(&dir, &Options::default());
    let mut headset = connect(&pc, &mut keys);
    headset
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    let Some(refused) = pc.ctl(&["recenter", "--json"]) else {
        return;
    };
    assert_eq!(refused.code, 1, "{refused:?}");
    assert_eq!(refused.json()["error"], "NotAllowed");
    let bus = pc.bus.as_ref().unwrap().connect();
    let typed = call(&bus, "NoLimits", &(true,)).unwrap_err();
    assert_eq!(error_name(&typed), "NotAllowed");
    headset.take_for(Duration::from_millis(500)).unwrap();
    assert!(headset.view().commands.is_empty(), "no command reached it");
    let (_, state) = get_state(&bus);
    assert_eq!(state.headsets[0].role, prostir_api::Role::Guest);
    assert!(!state.connected().unwrap().1.main);
    let quality = pc.ctl(&["quality", "15"]).unwrap();
    assert_eq!(quality.code, 0, "{quality:?}");
}

/// `ctl quit` is answered, then Prostir says goodbye to the headset and ends.
#[test]
fn ctl_quit_ends_prostir() {
    let (_dir, mut pc, _keys, mut headset) = session("api-quit", &Options::default());
    let Some(quit) = pc.ctl(&["quit"]) else {
        return;
    };
    assert_eq!(quit.code, 0, "{quit:?}");
    headset.wait_closed(SOON).unwrap();
    assert_eq!(
        headset.view().goodbye,
        Some(prostir_proto::PcGoodbye::Quit.code())
    );
    assert!(pc.wait_end(SOON).success());
}

/// A command whose arguments do not fit the protocol (a profile's name over 64 bytes)
/// is refused, and the session goes on; one the headset does not know is refused by
/// the headset.
#[test]
fn a_command_that_does_not_fit_is_refused() {
    let (_dir, pc, _keys, mut headset) = session("api-long", &Options::default());
    let long = "Робота".repeat(10);
    let Some(refused) = pc.ctl(&["profile", &long, "--json"]) else {
        return;
    };
    assert_eq!(refused.code, 1, "{refused:?}");
    assert_eq!(refused.json()["error"], "BadArgument");
    let bus = pc.bus.as_ref().unwrap().connect();
    let typed = call(&bus, "Profile", &(long.as_str(),)).unwrap_err();
    assert_eq!(error_name(&typed), "BadArgument");
    headset.take_for(Duration::from_millis(300)).unwrap();
    assert!(!headset.view().closed, "the session goes on");
    assert!(headset.view().commands.is_empty());
    let done = pc.ctl(&["profile", "Робота"]).unwrap();
    assert_eq!(done.code, 0, "{done:?}");
    headset.refuse_commands(prostir_proto::state::ErrorCode::UNSUPPORTED);
    let unknown = pc.ctl(&["cinema", "on", "--json"]).unwrap();
    assert_eq!(unknown.code, 1, "{unknown:?}");
    assert_eq!(unknown.json()["error"], "Unsupported");
}

/// The headset's profiles in the state, its names and the one in use; one renamed from
/// the PC (`rename_profile`, `prostir ctl profile rename`) shows there before the
/// answer; one the headset does not have is refused.
#[test]
fn a_profile_renamed_from_the_pc() {
    let (_dir, pc, _keys, _headset) = session("api-rename", &Options::default());
    let Some(bus) = &pc.bus else {
        return;
    };
    let bus = bus.connect();
    let started = Instant::now();
    loop {
        let (_, state) = get_state(&bus);
        let (_, session) = state.connected().unwrap();
        if !session.profiles.is_empty() {
            assert_eq!(session.profiles, ["", "Робота"]);
            assert_eq!(session.profile.as_deref(), Some("Робота"));
            break;
        }
        assert!(started.elapsed() < SOON, "the profiles in the state");
        std::thread::sleep(Duration::from_millis(50));
    }
    let request = r#"{"request":"rename_profile","profile":"1","name":"Ігри"}"#.to_owned();
    let (answer, before) = calls_in_order(&bus, &[request]).remove(0);
    let (revision, state) = before.last().expect("a state before the answer");
    assert_eq!(*revision, answer["revision"].as_u64().unwrap() as u32);
    let (_, session) = state.connected().unwrap();
    assert_eq!(
        session.profiles,
        ["Ігри", "Робота"],
        "the state before it has it"
    );
    let Some(done) = pc.ctl(&["profile", "rename", "2", "Робота", "вдома"]) else {
        return;
    };
    assert_eq!(done.code, 0, "{done:?}");
    let missing = pc.ctl(&["profile", "rename", "7", "x", "--json"]).unwrap();
    assert_eq!(missing.code, 1, "{missing:?}");
    assert_eq!(missing.json()["error"], "NoSuchProfile");
    let typed = call(&bus, "RenameProfile", &("1", "")).map(drop);
    assert!(typed.is_ok(), "{typed:?}");
}

/// The session bus restarted: the PC app takes its name again and answers as before.
#[test]
fn the_api_comes_back_after_the_bus_restarts() {
    let (_dir, mut pc, _keys, _headset) = session("api-bus-again", &Options::default());
    if !pc.restart_bus() {
        return;
    }
    pc.wait_log(
        SOON,
        "local API on D-Bus as io.github.prostir_app.Prostir again",
    );
    let status = pc.ctl(&["status", "--json"]).unwrap();
    assert_eq!(status.code, 0, "{status:?}");
    assert_eq!(status.json()["pc"]["phase"], "live");
    let done = pc.ctl(&["recenter"]).unwrap();
    assert_eq!(done.code, 0, "{done:?}");
}

/// Without a headset the set kept for the next one stands `kept`, and is changed there.
#[test]
fn the_kept_set_without_a_headset() {
    let dir = TestDir::new("api-kept");
    dir.save_monitors(TWO_MONITORS);
    let pc = Pc::start(&dir, &Options::default());
    let Some(bus) = &pc.bus else {
        return;
    };
    let bus = bus.connect();
    let (_, state) = get_state(&bus);
    assert_eq!(state.pc.phase, prostir_api::Phase::Idle);
    assert_eq!(state.displays.len(), 2);
    assert!(
        state
            .displays
            .iter()
            .all(|d| d.state == prostir_api::DisplayState::Kept),
        "{:?}",
        state.displays
    );
    let first = state.displays[0].id;
    let scale = *state.limits.scales.iter().find(|s| **s != 100).unwrap();
    let rescaled = pc
        .ctl(&["--json", "monitor", "retry", &first.to_string()])
        .unwrap();
    assert_eq!(rescaled.code, 0, "{rescaled:?}");
    let unknown = pc.ctl(&["--json", "monitor", "retry", "99999"]).unwrap();
    assert_eq!(unknown.json()["error"], "UnknownDisplay");
    let reply: String = call(
        &bus,
        "Call",
        &(format!(
            r#"{{"request":"set_scale","id":{first},"scale":{scale}}}"#
        ),),
    )
    .unwrap()
    .body()
    .deserialize()
    .unwrap();
    assert!(reply.contains("revision"), "{reply}");
    let (_, state) = get_state(&bus);
    assert_eq!(state.display(first).unwrap().mode.scale, scale);
    let place = call(
        &bus,
        "Call",
        &(r#"{"request":"place","id":1,"x":0,"y":0}"#,),
    )
    .unwrap_err();
    assert_eq!(error_name(&place), "Unsupported");
}

/// Monitors placed from the PC (a map in the settings window): another scale keeps
/// the monitor's id, and a monitor moved above the row gets a row of its own there.
#[test]
fn monitors_scaled_and_placed_from_the_pc() {
    let (_dir, pc, _keys, mut headset) = session("api-place", &Options::default());
    let Some(bus) = &pc.bus else {
        return;
    };
    let bus = bus.connect();
    let started = Instant::now();
    let state = loop {
        let (_, state) = get_state(&bus);
        let placed = state.displays.iter().all(|d| d.desk.is_some());
        if (placed && state.displays.len() == 2) || started.elapsed() > SOON {
            break state;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let (a, b) = (state.displays[0].clone(), state.displays[1].clone());
    let (da, db) = (a.desk.unwrap(), b.desk.unwrap());
    assert_eq!(da.y.min(db.y), da.y.min(db.y), "one row");
    // B above A.
    let request = format!(
        r#"{{"request":"place","id":{},"x":{},"y":{}}}"#,
        b.id,
        da.x,
        da.y - 2 * db.h as i32
    );
    let reply: String = call(&bus, "Call", &(request.as_str(),))
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    assert!(reply.contains("revision"), "{reply}");
    let started = Instant::now();
    loop {
        let (_, state) = get_state(&bus);
        let (na, nb) = (
            state.display(a.id).unwrap().desk.unwrap(),
            state.display(b.id).unwrap().desk.unwrap(),
        );
        if nb.y + nb.h as i32 <= na.y {
            break;
        }
        assert!(started.elapsed() < SOON, "not above: {na:?} and {nb:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
    // Another scale: the same monitor, in place.
    let scale = *state
        .limits
        .scales
        .iter()
        .find(|s| **s != a.mode.scale)
        .expect("another scale");
    let request = format!(r#"{{"request":"set_scale","id":{},"scale":{scale}}}"#, a.id);
    call(&bus, "Call", &(request.as_str(),)).unwrap();
    let started = Instant::now();
    loop {
        let (_, state) = get_state(&bus);
        let d = state.display(a.id).expect("the same id");
        if d.mode.scale == scale && d.state == prostir_api::DisplayState::Running {
            break;
        }
        assert!(started.elapsed() < START, "{d:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
    headset
        .wait(SOON, "the new scale", |v| {
            v.display(prostir_proto::state::DisplayId(a.id))
                .is_some_and(|d| d.mode.scale == scale)
        })
        .unwrap();
    let bad = call(
        &bus,
        "Call",
        &(format!(r#"{{"request":"set_scale","id":{},"scale":30}}"#, a.id).as_str(),),
    )
    .unwrap_err();
    assert_eq!(error_name(&bad), "BadSize");
    // A place far off the desktop, at the ends of what the numbers hold: placed at
    // the edge, and the PC goes on.
    for (x, y) in [(i32::MAX, i32::MAX), (i32::MIN, i32::MIN)] {
        let request = format!(r#"{{"request":"place","id":{},"x":{x},"y":{y}}}"#, b.id);
        let reply: String = call(&bus, "Call", &(request.as_str(),))
            .unwrap()
            .body()
            .deserialize()
            .unwrap();
        assert!(reply.contains("revision"), "{reply}");
    }
    let (_, state) = get_state(&bus);
    assert_eq!(state.displays.len(), 2, "{state:?}");
}

/// How long a monitor just added waits for the headset to place it (`PLACE_WAIT` in
/// `displays/placing.rs`).
const PLACE_WAIT: Duration = Duration::from_secs(4);

/// A session whose headset placed its two monitors (640x360 and 480x272 side by side,
/// tops level), the local API showing both on the desktop; with the
/// arrangement sent.
fn placed_session(
    name: &str,
) -> Option<(
    TestDir,
    Pc,
    prostir_testkit::FakeHeadset,
    Connection,
    Vec<prostir_proto::state::Placed>,
)> {
    use prostir_proto::ClientMsg;
    use prostir_proto::state::Row;
    let (dir, pc, _keys, mut headset) = session(name, &Options::default());
    let bus = pc.bus.as_ref()?.connect();
    let order = headset.view().order();
    let entries = vec![at(order[0], 320, 180), at(order[1], 880, 136)];
    headset
        .send(&ClientMsg::Arrange {
            rows: vec![Row {
                entries: entries.clone(),
            }],
        })
        .unwrap();
    let started = Instant::now();
    loop {
        let (_, state) = get_state(&bus);
        if state.displays.iter().all(|d| d.desk.is_some()) {
            break;
        }
        assert!(started.elapsed() < SOON, "not placed: {:?}", state.displays);
        headset.take_for(Duration::from_millis(50)).unwrap();
    }
    Some((dir, pc, headset, bus, entries))
}

/// Where the headset has a display: its centre and middle in its stream's pixels.
fn at(
    id: prostir_proto::state::DisplayId,
    center: i32,
    middle: i32,
) -> prostir_proto::state::Placed {
    prostir_proto::state::Placed {
        id,
        center: Some(center),
        middle: Some(middle),
    }
}

/// Adds a 640x360 monitor through the local API: its id.
fn add_monitor(bus: &Connection) -> u32 {
    let reply: String = call(
        bus,
        "Call",
        &(r#"{"request":"add_monitor","w":640,"h":360}"#,),
    )
    .unwrap()
    .body()
    .deserialize()
    .unwrap();
    let reply: serde_json::Value = serde_json::from_str(&reply).unwrap();
    u32::try_from(reply["id"].as_u64().expect("the new monitor's id")).unwrap()
}

/// Where monitor `id` is on the desktop in every state that came, in order.
fn places(changes: &mpsc::Receiver<(u32, State)>, id: u32) -> Vec<Option<prostir_api::DeskRect>> {
    changes
        .try_iter()
        .filter_map(|(_, s)| s.display(id).map(|d| d.desk))
        .collect()
}

/// A monitor added from the PC while the headset places the monitors: in every state
/// the local API sends, it stands nowhere on the desktop until the headset placed it
/// and the desktop was laid out so, then where it stays, never at the stand-in place
/// the PC gave it before. One the headset does not place shows at the PC's place when
/// the wait for it ends.
#[test]
fn a_monitor_added_from_the_pc_shows_where_the_headset_puts_it() {
    use prostir_proto::ClientMsg;
    use prostir_proto::state::{DisplayId, Row};
    let Some((_dir, mut pc, mut headset, bus, mut entries)) = placed_session("api-adding") else {
        return;
    };
    let changes = state_changes(&bus);
    let before = pc.count_log("fake desktop: placed");
    let new = add_monitor(&bus);
    // Made and streaming, laid out by the PC meanwhile: still nowhere in the API.
    headset
        .wait(START, "the new monitor showing", View::all_monitors_show)
        .unwrap();
    pc.wait_log_after(SOON, "fake desktop: placed", before);
    headset.take_for(Duration::from_millis(300)).unwrap();
    let (_, state) = get_state(&bus);
    let d = state.display(new).unwrap();
    assert_eq!((d.desk, d.placing), (None, true));
    // The headset puts it over the first one.
    entries.insert(0, at(DisplayId(new), 320, -180));
    headset
        .send(&ClientMsg::Arrange {
            rows: vec![
                Row {
                    entries: vec![entries[0]],
                },
                Row {
                    entries: entries[1..].to_vec(),
                },
            ],
        })
        .unwrap();
    let started = Instant::now();
    let (r, state) = loop {
        let (_, state) = get_state(&bus);
        if let Some(r) = state.display(new).and_then(|d| d.desk) {
            break (r, state);
        }
        assert!(started.elapsed() < SOON, "never on the desktop");
        headset.take_for(Duration::from_millis(20)).unwrap();
    };
    let under = state.display(entries[1].id.0).unwrap().desk.unwrap();
    assert!(r.y + r.h as i32 <= under.y, "{r:?} over {under:?}");
    headset.take_for(Duration::from_millis(500)).unwrap();
    let (_, state) = get_state(&bus);
    let d = state.display(new).unwrap();
    assert_eq!((d.desk, d.placing), (Some(r), false), "it did not move");
    let seen = places(&changes, new);
    assert!(seen.iter().any(Option::is_none), "{seen:?}");
    assert!(
        seen.iter().all(|p| p.is_none_or(|p| p == r)),
        "only where it stays: {seen:?}"
    );

    // Not placed by the headset: where the PC put it, once the wait for it ends.
    let added = Instant::now();
    let other = add_monitor(&bus);
    let after = loop {
        let (_, state) = get_state(&bus);
        if state.display(other).and_then(|d| d.desk).is_some() {
            break added.elapsed();
        }
        assert!(added.elapsed() < START, "never on the desktop");
        headset.take_for(Duration::from_millis(20)).unwrap();
    };
    assert!(
        after >= PLACE_WAIT - Duration::from_millis(200)
            && after < PLACE_WAIT + Duration::from_secs(1),
        "shown after {after:?}"
    );
    // The wait over with no layout after it, the control loop sleeps between its
    // clocks: it does not wake at once again and again.
    let busy = control_loop_cpu(&pc);
    headset.take_for(Duration::from_secs(2)).unwrap();
    let spent = control_loop_cpu(&pc) - busy;
    assert!(
        spent < Duration::from_millis(500),
        "the control loop ran {spent:?} of 2 s"
    );
}

/// The processor time the PC app's control loop (its main thread) took so far.
fn control_loop_cpu(pc: &Pc) -> Duration {
    let pid = pc.pid();
    let stat = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/stat")).unwrap();
    // After the name in parentheses: state is field 3, utime and stime 14 and 15.
    let fields: Vec<&str> = stat
        .rsplit_once(')')
        .unwrap()
        .1
        .split_whitespace()
        .collect();
    let ticks: u64 = fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap();
    // SAFETY: sysconf has no preconditions.
    let per_second = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as u64;
    Duration::from_millis(ticks * 1000 / per_second)
}

/// The headset leaves before it placed a monitor added from the PC: the monitors stay
/// (`--linger`) and the PC's place counts at once, without the wait for a headset.
#[test]
fn a_monitor_being_placed_shows_once_the_headset_left() {
    let Some((_dir, mut pc, mut headset, bus, _)) = placed_session("api-adding-left") else {
        return;
    };
    let before = pc.count_log("fake desktop: placed");
    let added = Instant::now();
    let new = add_monitor(&bus);
    headset
        .wait(START, "the new monitor showing", View::all_monitors_show)
        .unwrap();
    pc.wait_log_after(SOON, "fake desktop: placed", before);
    let (_, state) = get_state(&bus);
    assert_eq!(state.display(new).unwrap().desk, None);
    headset.disconnect();
    let left = Instant::now();
    loop {
        let (_, state) = get_state(&bus);
        if state.display(new).and_then(|d| d.desk).is_some() {
            break;
        }
        assert!(
            left.elapsed() < Duration::from_secs(1) && added.elapsed() < PLACE_WAIT,
            "not shown {:?} after the headset left",
            left.elapsed()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The volume set in the settings window: the headset's sound device goes there, the
/// headset is asked once and its step stands on both sides and in the state.
#[test]
fn the_volume_from_the_pc_reaches_the_headset() {
    let (_dir, mut pc, _keys, mut headset) = session("api-volume", &Options::default());
    let Some(bus) = &pc.bus else {
        return;
    };
    let bus = bus.connect();
    crate::sound::wait_first_volume(&mut pc, &mut headset);
    let (_, state) = get_state(&bus);
    assert!(state.sound.volume.is_some(), "{:?}", state.sound);
    let asks = headset.view().volume_asks.len();
    // Not the first level's step (40), so the device moves.
    let level = 73;
    call(
        &bus,
        "Call",
        &(format!(r#"{{"request":"volume","level":{level}}}"#).as_str(),),
    )
    .unwrap();
    let step = prostir_testkit::stepped(
        prostir_testkit::FIRST_VOLUME,
        level,
        prostir_testkit::VOLUME_STEPS,
    );
    headset
        .wait(SOON, "the headset asked for the volume", |v| {
            v.volume_asks.len() > asks
        })
        .unwrap();
    crate::sound::wait_written(&pc, &mut headset, step);
    assert_eq!(headset.volume(), Some(step));
    assert_eq!(headset.view().volume_asks.len(), asks + 1, "asked once");
    let started = Instant::now();
    loop {
        let (_, state) = get_state(&bus);
        if state.sound.volume.map(|v| v.level) == Some(step) {
            break;
        }
        assert!(started.elapsed() < SOON, "{:?}", state.sound);
        headset.take_for(Duration::from_millis(50)).unwrap();
    }
    // Quiet after: the device's echo of the level asks nothing more.
    headset.take_for(Duration::from_secs(1)).unwrap();
    assert_eq!(headset.view().volume_asks.len(), asks + 1);

    // Muted on the PC, then set from the settings: unmuted, as Plasma's slider does.
    let volume = |bus: &Connection| get_state(bus).1.sound.volume.unwrap();
    pc.desktop("volume 60 muted");
    let started = Instant::now();
    while !volume(&bus).muted {
        assert!(started.elapsed() < SOON, "not muted: {:?}", volume(&bus));
        headset.take_for(Duration::from_millis(50)).unwrap();
    }
    call(&bus, "Call", &(r#"{"request":"volume","level":20}"#,)).unwrap();
    let step = prostir_testkit::stepped(60, 20, prostir_testkit::VOLUME_STEPS);
    let started = Instant::now();
    while volume(&bus)
        != (prostir_api::Volume {
            level: step,
            muted: false,
        })
    {
        assert!(started.elapsed() < SOON, "{:?}", volume(&bus));
        headset.take_for(Duration::from_millis(50)).unwrap();
    }
    assert_eq!(headset.volume(), Some(step));

    // Not a level.
    let bad = call(&bus, "Call", &(r#"{"request":"volume","level":101}"#,)).unwrap_err();
    assert_eq!(error_name(&bad), "BadArgument");
}

/// A scale the desktop did not take in place (the call failed): the monitor keeps the
/// one it has, as the desktop has it, and nothing is taken for one given in its
/// settings.
#[test]
fn a_scale_that_could_not_be_set_is_not_taken() {
    let (_dir, mut pc, _keys, _headset) = session("api-refused-scale", &Options::default());
    let Some(bus) = &pc.bus else {
        return;
    };
    let bus = bus.connect();
    let started = Instant::now();
    let a = loop {
        let (_, state) = get_state(&bus);
        if let Some(d) = state.displays.first()
            && d.state == prostir_api::DisplayState::Running
        {
            break d.clone();
        }
        assert!(started.elapsed() < SOON, "no monitor running");
        std::thread::sleep(Duration::from_millis(50));
    };
    let failed = pc.count_log("fake desktop: the scales");
    pc.desktop("refuse-scale");
    pc.wait_log(SOON, "fake desktop: the next scale set fails");
    let request = format!(r#"{{"request":"set_scale","id":{},"scale":150}}"#, a.id);
    call(&bus, "Call", &(request.as_str(),)).unwrap();
    pc.wait_log_after(SOON, "fake desktop: the scales", failed);
    // Ticks enough to take a scale seen twice.
    std::thread::sleep(Duration::from_secs(3));
    let (_, state) = get_state(&bus);
    let d = state.display(a.id).unwrap();
    // Given in place, refused: it keeps the one it has, still running.
    assert_eq!(d.mode.scale, a.mode.scale, "{d:?}");
    assert_eq!(d.state, prostir_api::DisplayState::Running, "{d:?}");
    assert_eq!(pc.count_log("given on the desktop, taken"), 0);
}

/// A scale given to a monitor in the desktop's own settings is taken: the local API and
/// the set kept for next time have it, and the monitor's output stays as it is.
#[test]
fn a_scale_given_on_the_desktop_is_taken() {
    let (dir, pc, _keys, _headset) = session("api-outside-scale", &Options::default());
    let Some(bus) = &pc.bus else {
        return;
    };
    let bus = bus.connect();
    let started = Instant::now();
    let a = loop {
        let (_, state) = get_state(&bus);
        if let Some(d) = state.displays.first()
            && d.desk.is_some()
            && d.state == prostir_api::DisplayState::Running
        {
            break d.clone();
        }
        assert!(started.elapsed() < SOON, "no monitor on the desktop");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(a.mode.scale, 100);
    let made = pc.count_log(&format!("fake desktop: created {}", a.output));
    pc.desktop(&format!("scale {} 150", a.output));
    let want = (
        (f64::from(a.mode.w) / 1.5).round() as u32,
        (f64::from(a.mode.h) / 1.5).round() as u32,
    );
    let started = Instant::now();
    loop {
        let (_, state) = get_state(&bus);
        let d = state.display(a.id).expect("the same id");
        if d.mode.scale == 150 && d.desk.map(|r| (r.w, r.h)) == Some(want) {
            break;
        }
        assert!(started.elapsed() < START, "not taken: {d:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        pc.count_log(&format!("fake desktop: created {}", a.output)),
        made,
        "the output was not made anew"
    );
    let started = Instant::now();
    while !dir.saved("screens.txt").contains("@1.5") {
        assert!(started.elapsed() < SOON, "{}", dir.saved("screens.txt"));
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Back and forth between scales from the PC, 100 % among them: each one is applied,
/// the monitor's place on the desktop as large as its pixels at that scale.
#[test]
fn every_scale_from_the_pc_is_applied_100_too() {
    let (_dir, pc, _keys, _headset) = session("api-scales", &Options::default());
    let Some(bus) = &pc.bus else {
        return;
    };
    let bus = bus.connect();
    let started = Instant::now();
    let a = loop {
        let (_, state) = get_state(&bus);
        if let Some(d) = state.displays.first()
            && d.desk.is_some()
            && d.state == prostir_api::DisplayState::Running
        {
            break d.clone();
        }
        assert!(started.elapsed() < SOON, "no monitor on the desktop");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(a.mode.scale, 100);
    for scale in [150u16, 100, 125, 100] {
        let request = format!(r#"{{"request":"set_scale","id":{},"scale":{scale}}}"#, a.id);
        call(&bus, "Call", &(request.as_str(),)).unwrap();
        let logical = |px: u32| (f64::from(px) * 100.0 / f64::from(scale)).round() as u32;
        let want = (logical(a.mode.w), logical(a.mode.h));
        let started = Instant::now();
        loop {
            let (_, state) = get_state(&bus);
            let d = state.display(a.id).expect("the same id");
            let desk = d.desk.map(|r| (r.w, r.h));
            if d.mode.scale == scale
                && d.state == prostir_api::DisplayState::Running
                && desk == Some(want)
            {
                break;
            }
            assert!(
                started.elapsed() < START,
                "{scale} %: {d:?}, on the desktop {desk:?}, not {want:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// The laptop's lid closed (its panel kept as shown from before, not connected) and
/// one monitor on HDMI: the monitor shows and hides as often as asked, from the
/// headset and from the PC, the absent panel counting for no limit; the local API
/// tells each display as it is, the hidden monitor with its size and place, the
/// absent panel with no size, every virtual monitor running.
#[test]
fn a_closed_laptop_and_a_monitor_on_hdmi() {
    use prostir_api::{DisplayKind, DisplayState as Api};
    use prostir_proto::request::{Outcome, Request};
    use prostir_proto::state::{DisplayId, DisplayState};
    let options = Options {
        physical: "HDMI-A-1=MSI MP251 0@1920x1080".into(),
        ..Options::default()
    };
    let dir = TestDir::new("api-closed-laptop");
    dir.save_screens(
        "next 200\nphysical 100 edid \"BOE 0x0a1c 0\" connector eDP-1 builtin shown \
         seen 1795000000\nrow\nscreen 0 640x360\nscreen 1 480x272\n",
    );
    dir.save_settings("mirror on\n");
    let mut keys = dir.pair();
    let pc = Pc::start(&dir, &options);
    let mut headset = connect(&pc, &mut keys);
    let laptop = DisplayId(100);
    headset
        .wait(START, "the monitors, the absent panel and HDMI", |v| {
            v.all_monitors_show()
                && v.display(laptop)
                    .is_some_and(|d| d.state == DisplayState::Absent && d.is_shown_physical())
                && v.displays()
                    .iter()
                    .any(|d| d.id != laptop && d.is_shown_physical())
        })
        .unwrap();
    let hdmi = headset
        .view()
        .displays()
        .iter()
        .find(|d| d.is_physical() && d.id != laptop)
        .unwrap()
        .id;
    for shown in [false, true, false] {
        let outcome = headset.ask(Request::ShowPhysical { id: hdmi, shown });
        assert!(matches!(outcome, Ok(Outcome::Done)), "{shown}: {outcome:?}");
        headset
            .wait(SOON, "shown or hidden as asked", |v| {
                v.display(hdmi)
                    .is_some_and(|d| d.is_shown_physical() == shown)
            })
            .unwrap();
    }
    let hidden = headset.view().display(hdmi).unwrap().clone();
    assert_eq!(
        (hidden.mode.w, hidden.mode.h),
        (1920, 1080),
        "its size, hidden"
    );
    let Some(bus) = &pc.bus else {
        return;
    };
    let bus = bus.connect();
    let started = Instant::now();
    let state = loop {
        let (_, state) = get_state(&bus);
        let placed = state
            .display(hdmi.0)
            .is_some_and(|d| d.desk.is_some() && !d.shown);
        if placed || started.elapsed() > SOON {
            break state;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let monitors: Vec<_> = state
        .displays
        .iter()
        .filter(|d| d.kind == DisplayKind::Virtual)
        .collect();
    assert_eq!(monitors.len(), 2, "{:?}", state.displays);
    for m in &monitors {
        assert_eq!(m.state, Api::Running, "{m:?}");
        assert!(m.failure.is_none() && m.desk.is_some(), "{m:?}");
    }
    let h = state.display(hdmi.0).unwrap();
    assert_eq!((h.mode.w, h.mode.h), (1920, 1080), "{h:?}");
    assert_eq!(h.state, Api::Running, "{h:?}");
    assert!(h.desk.is_some(), "on the desktop, hidden or not");
    let l = state.display(100).unwrap();
    assert_eq!(l.state, Api::Absent);
    assert_eq!((l.mode.w, l.mode.h), (0, 0), "no size known: none said");
    assert!(
        state.limits.physical_max.unwrap() >= 2,
        "{:?}",
        state.limits
    );
    // From the PC (the settings window's "Show"), with the panel still marked shown.
    let shown = format!(
        r#"{{"request":"show_physical","id":{},"shown":true}}"#,
        hdmi.0
    );
    let reply: String = call(&bus, "Call", &(shown.as_str(),))
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    assert!(reply.contains("revision"), "{reply}");
    headset
        .wait(SOON, "shown again", |v| {
            v.display(hdmi)
                .is_some_and(prostir_proto::state::Display::is_shown_physical)
                && v.streams
                    .get(&hdmi)
                    .is_some_and(prostir_testkit::Stream::has_picture)
        })
        .unwrap();
}

/// A monitor the desktop could not make stands failed in the local API, saying why
/// once (the window names it); "Try again" (`retry_monitor`) makes it again.
#[test]
fn a_failed_monitor_is_tried_again() {
    let options = Options {
        broken_monitors: "2".into(),
        ..Options::default()
    };
    let dir = TestDir::new("api-retry");
    dir.save_monitors(TWO_MONITORS);
    let mut keys = dir.pair();
    let mut pc = Pc::start(&dir, &options);
    let _headset = connect(&pc, &mut keys);
    let Some(bus) = &pc.bus else {
        return;
    };
    let bus = bus.connect();
    let started = Instant::now();
    let failed = loop {
        let (_, state) = get_state(&bus);
        let failed = state
            .displays
            .iter()
            .find(|d| d.state == prostir_api::DisplayState::Failed)
            .cloned();
        if let Some(d) = failed {
            break d;
        }
        assert!(started.elapsed() < START, "{:?}", state.displays);
        std::thread::sleep(Duration::from_millis(50));
    };
    let why = failed.failure.unwrap().text;
    assert!(why.contains("broken"), "{why}");
    assert!(
        !why.contains(&failed.name),
        "named once, by the window: {why}"
    );
    let tries = pc.count_log("fake monitor 2 is broken");
    assert!(tries > 0);
    let request = format!(r#"{{"request":"retry_monitor","id":{}}}"#, failed.id);
    call(&bus, "Call", &(request.as_str(),)).unwrap();
    pc.wait_log_after(SOON, "fake monitor 2 is broken", tries);
}

/// Monitors that stream in H.265 to a headset that decodes it stand running in the
/// local API too: what the PC tells it is judged by no stand-in headset that decodes
/// nothing. Only where the PC encodes H.265 on its graphics card.
#[test]
fn monitors_in_h265_stand_running() {
    use prostir_proto::Codec;
    let options = Options {
        software: false,
        ..Options::default()
    };
    let dir = TestDir::new("api-h265");
    dir.save_screens("next 200\nrow\nscreen 0 640x360\nscreen 1 480x272\n");
    dir.save_settings("codec h265\nsoftware off\n");
    let mut keys = dir.pair();
    let pc = Pc::start(&dir, &options);
    let mut headset = connect(&pc, &mut keys);
    headset
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    let using = headset.view().encoding().unwrap().using;
    if using.map(|(c, _)| c) != Some(Codec::H265) {
        eprintln!("no H.265 on the graphics card here ({using:?}): nothing to try");
        return;
    }
    let Some(bus) = &pc.bus else {
        return;
    };
    let (_, state) = get_state(&bus.connect());
    assert_eq!(state.displays.len(), 2);
    for d in &state.displays {
        assert_eq!(d.state, prostir_api::DisplayState::Running, "{d:?}");
    }
}

/// A request over 64 KiB is refused unread.
#[test]
fn a_huge_request_is_refused() {
    let dir = TestDir::new("api-huge");
    dir.save_monitors(TWO_MONITORS);
    let pc = Pc::start(&dir, &Options::default());
    let Some(bus) = &pc.bus else {
        return;
    };
    let bus = bus.connect();
    let huge = format!(r#"{{"request":"profile","name":"{}"}}"#, "x".repeat(70_000));
    let refused = call(&bus, "Call", &(huge.as_str(),)).unwrap_err();
    assert_eq!(error_name(&refused), "BadArgument");
}

/// When each paired headset last connected or left, and its app then, are in the
/// state and kept for the next run; a headset that never connected says neither.
#[test]
fn when_each_headset_was_seen() {
    let dir = TestDir::new("api-seen");
    dir.save_monitors(TWO_MONITORS);
    let mut keys = dir.pair();
    dir.pair_headset("never-connected");
    let pc = Pc::start(&dir, &Options::default());
    let Some(bus) = &pc.bus else {
        return;
    };
    let bus = bus.connect();
    let mut headset = connect(&pc, &mut keys);
    headset
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    let now = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    };
    let (_, state) = get_state(&bus);
    let connected = state.headsets.iter().find(|h| h.session.is_some()).unwrap();
    let app = prostir_proto::app_version(env!("CARGO_PKG_VERSION"));
    assert_eq!(connected.app, app, "{connected:?}");
    assert!(
        connected.last_seen.is_some_and(|t| t + 60 > now()),
        "{connected:?}"
    );
    let other = state.headsets.iter().find(|h| h.session.is_none()).unwrap();
    assert_eq!(
        (other.app.as_str(), other.last_seen),
        ("", None),
        "{other:?}"
    );
    let key = connected.key.clone();
    headset.disconnect();
    let started = Instant::now();
    loop {
        let (_, state) = get_state(&bus);
        let h = state.headsets.iter().find(|h| h.key == key).unwrap();
        if h.session.is_none() {
            assert!(h.last_seen.is_some() && h.app == app, "{h:?}");
            break;
        }
        assert!(started.elapsed() < SOON, "still connected");
        std::thread::sleep(Duration::from_millis(50));
    }
    pc.quit();
    let saved = dir.saved("headsets.txt");
    assert!(
        saved.starts_with("seen ") && saved.lines().count() == 1,
        "{saved}"
    );
}

/// Counting this PC among users waits for the person's answer, through the local
/// API: unasked at first, a headset that connects meanwhile is not counted, a yes
/// counts the next one (the first time with `/install`), once a day, and the answer
/// is kept in `settings.txt`; `unasked` puts the question back, and an answer that is
/// none of the three is refused. The build with the fake desktop counts into a file
/// (`PROSTIR_FAKE_COUNT`) instead of sending anything.
#[test]
fn counting_waits_for_the_answer() {
    use prostir_api::Counting;
    let dir = TestDir::new("api-counting");
    dir.save_monitors(TWO_MONITORS);
    let mut keys = dir.pair();
    let counted = dir.path().join("counted-paths");
    let options = Options {
        env: vec![
            ("PROSTIR_FAKE_COUNT".into(), counted.display().to_string()),
            ("PROSTIR_NO_COUNT".into(), "0".into()),
        ],
        ..Options::default()
    };
    let pc = Pc::start(&dir, &options);
    let Some(bus) = &pc.bus else {
        return;
    };
    let bus = bus.connect();
    let session = |keys: &mut prostir_testkit::Keys| {
        let mut headset = connect(&pc, keys);
        headset
            .wait(START, "every monitor showing", View::all_monitors_show)
            .unwrap();
        // Time for a count to be written, were one sent.
        headset.take_for(Duration::from_millis(500)).unwrap();
        headset.disconnect();
    };
    let ask = |answer: &str| {
        let request = format!(r#"{{"request":"set_counting","counting":"{answer}"}}"#);
        call(&bus, "Call", &(request.as_str(),))
    };
    let answer = || get_state(&bus).1.counting;
    let paths = || std::fs::read_to_string(&counted).unwrap_or_default();
    assert_eq!(answer(), Some(Counting::Unasked));
    session(&mut keys);
    assert_eq!(paths(), "", "counted before the answer");

    ask("yes").unwrap();
    assert_eq!(answer(), Some(Counting::Yes));
    session(&mut keys);
    let day = paths();
    let lines: Vec<&str> = day.lines().collect();
    assert!(
        lines.len() == 2 && lines[0].starts_with("/day/20") && lines[1] == "/install",
        "{day}"
    );
    // Once a day.
    session(&mut keys);
    assert_eq!(paths(), day);

    let saved = |line: &str| {
        let started = Instant::now();
        while !dir.saved("settings.txt").lines().any(|l| l == line) {
            assert!(
                started.elapsed() < SOON,
                "{line:?} not in settings.txt:\n{}",
                dir.saved("settings.txt")
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    saved("count yes");
    // Not after a no, nor after unasked, also on a day not counted yet.
    let counted_day = dir.path().join("config/prostir/counted");
    let no = pc.ctl(&["count", "no"]).unwrap();
    assert_eq!(no.code, 0, "{no:?}");
    assert_eq!(answer(), Some(Counting::No));
    let status = pc.ctl(&["count", "status"]).unwrap();
    assert_eq!(status.out.trim(), "no", "{status:?}");
    saved("count no");
    std::fs::remove_file(&counted_day).unwrap();
    session(&mut keys);
    assert_eq!(paths(), day);
    ask("unasked").unwrap();
    assert_eq!(answer(), Some(Counting::Unasked));
    saved("count unasked");
    session(&mut keys);
    assert_eq!(paths(), day);
    assert!(!counted_day.exists());
    let bad = ask("maybe").unwrap_err();
    assert_eq!(error_name(&bad), "BadArgument");
    assert_eq!(answer(), Some(Counting::Unasked));
    pc.quit();
    assert_eq!(paths(), day);
}
