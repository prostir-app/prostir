//! Nothing runs while no headset needs the PC, and a headset finds its PC by asking:
//! whom the PC answers (everyone until it is paired, then only its own headsets, and
//! everyone again while its pairing window is open), the notice of a new headset, the
//! pairing window, the sockets' watcher that sleeps until a headset comes, and the
//! start by systemd's socket activation. Every query here goes straight to the PC's
//! address, as a headset's does to a PC it knows (a VPN address too).

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use prostir_api::{BUS_NAME, INTERFACE, OBJECT_PATH, SETTINGS_NAME, State};
use prostir_proto::discovery::{Answer, MIN_QUERY, Query, flags};
use prostir_testkit::{FakeHeadset, Keys, View, find};

use crate::pc::{Activate, Options, Pc, TestDir, wakeups_of};
use crate::{SOON, START, TWO_MONITORS, connect};

/// An answer comes within this, if one comes.
const ANSWER: Duration = Duration::from_secs(2);
/// No answer within this: none comes.
const SILENCE: Duration = Duration::from_millis(700);
/// How long an asleep PC is watched for wakeups.
const WATCH: Duration = Duration::from_secs(3);

fn asks(to: SocketAddr, keys: &Keys, name: &str) -> Option<Answer> {
    find::ask(to, &find::query(keys, name), ANSWER)
}

fn silent(to: SocketAddr, keys: &Keys, name: &str) -> bool {
    find::ask(to, &find::query(keys, name), SILENCE).is_none()
}

/// Asks until the PC answers no more, for up to [`SOON`]. A pause between the
/// questions: the PC answers 20 a second at most, and more would leave the next
/// question unanswered.
fn until_silent(to: SocketAddr, keys: &Keys, what: &str) {
    let started = Instant::now();
    while !silent(to, keys, "") {
        assert!(started.elapsed() < SOON, "{what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The PC app's state on its local API, `None` without a session bus.
fn state(pc: &Pc) -> Option<State> {
    let ctl = pc.ctl(&["status", "--json"])?;
    let mut json = ctl.json();
    if json.get("state").is_some() {
        json = json["state"].take();
    }
    Some(serde_json::from_value(json).unwrap())
}

/// Waits for `what` of the PC app's state, `None` without a session bus.
fn wait_state(pc: &Pc, what: &str, done: impl Fn(&State) -> bool) -> Option<State> {
    let started = Instant::now();
    loop {
        let s = state(pc)?;
        if done(&s) {
            return Some(s);
        }
        assert!(started.elapsed() < SOON, "{what}: {s:?}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Unpaired, the PC answers everyone, saying so; paired, only its own headsets, whom
/// the answer's tag tells which PC answered. A stranger brings the notice, once a
/// minute at most, and is nearby. A query under 128 bytes gets nothing, and an
/// answer is always smaller than the query.
#[test]
fn the_pc_answers_everyone_until_paired_then_only_its_own() {
    let dir = TestDir::new("find-who");
    let stranger = dir.keys_of("stranger");
    let options = Options {
        discovery: true,
        ..Options::default()
    };
    let pc = Pc::start(&dir, &options);
    let to = pc.discovery.unwrap();
    let a = asks(to, &stranger, "Quest 3S").expect("an unpaired PC answers everyone");
    assert_eq!(a.flags, flags::ANSWERS_ALL);
    assert_eq!((a.port, a.name.as_str()), (pc.addr.port(), "fake-pc"));
    assert!(a.from_pc(&dir.find_secret()));
    pc.quit();

    let keys = dir.pair();
    let mut other = dir.pair_headset("other");
    let mut pc = Pc::start(&dir, &options);
    let to = pc.discovery.unwrap();
    let a = asks(to, &keys, "").expect("its own headset gets an answer");
    assert_eq!(a.flags, 0);
    assert!(a.from_pc(&dir.find_secret()));
    assert!(
        !a.from_pc(&dir.pc_key()),
        "not made with the PC's public key"
    );
    // The PC's public key (anyone who connects sees it) makes no tag it answers.
    let with_public_key = Query::new([&dir.pc_key()], "");
    assert!(find::ask(to, &with_public_key, SILENCE).is_none());
    assert!(silent(to, &stranger, "Quest 3S"), "a stranger gets none");
    pc.wait_log(SOON, "notification: New headset nearby: Pair it? [Pair]");
    assert!(silent(to, &stranger, "Quest 3S"));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        pc.count_log("notification: New headset nearby"),
        1,
        "once a minute"
    );
    if let Some(s) = state(&pc) {
        assert_eq!(s.nearby, ["Quest 3S"]);
        assert!(s.pairing.is_none());
    }

    // The smallest query, and one byte less.
    let own = find::query(&keys, "").encode();
    assert_eq!(own.len(), MIN_QUERY);
    let back = find::send(to, &own, ANSWER).expect("an answer");
    assert!(
        back.len() < own.len(),
        "{} bytes for {}",
        back.len(),
        own.len()
    );
    assert!(find::send(to, &own[..MIN_QUERY - 1], SILENCE).is_none());

    // Forgotten: its tags no longer work. The other headset gets the new secret in
    // its session and is answered again.
    if let Some(ctl) = pc.ctl(&[
        "forget",
        &prostir_proto::secure::to_hex(&keys.identity.public),
    ]) {
        assert_eq!(ctl.code, 0, "{ctl:?}");
        pc.wait_log(SOON, "a new find secret");
        assert!(silent(to, &keys, ""), "the forgotten headset");
        assert!(silent(to, &other, ""), "the old secret");
        let mut headset = connect(&pc, &mut other);
        headset.learn_find_secret(&mut other, SOON).unwrap();
        assert!(asks(to, &other, "").is_some(), "with the new secret");
    }
    pc.quit();
}

/// The pairing window answers strangers: opened from the notice's button, the tray
/// and the local API; closed by "Stop pairing" and at its time. Open, it stays so for
/// as long as a release keeps it (a busy machine can take seconds between opening it
/// and asking); that it closes at its time is seen with a short one, on a PC started
/// again with it.
#[test]
fn the_pairing_window_lets_strangers_find_the_pc() {
    let dir = TestDir::new("find-pairing");
    let stranger = dir.keys_of("stranger");
    dir.pair();
    let options = Options {
        discovery: true,
        // Each query and whether it was answered, for a look when it fails.
        env: vec![("RUST_LOG".into(), "info,prostir::find=debug".into())],
        ..Options::default()
    };
    let mut pc = Pc::start(&dir, &options);
    let to = pc.discovery.unwrap();
    assert!(silent(to, &stranger, ""));
    pc.wait_log(SOON, "notification: New headset nearby");
    pc.notice("click");
    pc.wait_log(SOON, "pairing a new headset: from the notice");
    let a = asks(to, &stranger, "").expect("the window is open");
    assert_eq!(a.flags, flags::ANSWERS_ALL | flags::PAIRING);
    pc.tray("pair stop");
    until_silent(to, &stranger, "stopped");

    pc.tray("pair");
    pc.wait_log(SOON, "pairing a new headset: from the tray");
    assert!(asks(to, &stranger, "").is_some());
    pc.tray("pair stop");
    until_silent(to, &stranger, "stopped");

    if let Some(ctl) = pc.ctl(&["pair", "--json"]) {
        assert_eq!(ctl.code, 0, "{ctl:?}");
        assert_eq!(ctl.json()["seconds_left"], 180);
        assert!(asks(to, &stranger, "").is_some());
        let s = state(&pc).unwrap();
        let p = s.pairing.expect("pairing");
        assert_eq!(p.seconds_total, 180);
        assert_eq!(pc.ctl(&["pair", "stop"]).unwrap().code, 0);
        assert!(silent(to, &stranger, ""));
    }
    pc.quit();

    // A window of a second closes by itself.
    let short = Options {
        discovery: true,
        env: vec![("PROSTIR_TEST_PAIR_SECONDS".into(), "1".into())],
        ..Options::default()
    };
    let mut pc = Pc::start(&dir, &short);
    let to = pc.discovery.unwrap();
    pc.tray("pair");
    pc.wait_log(SOON, "pairing a new headset: from the tray");
    until_silent(to, &stranger, "the window closed at its time");
    pc.quit();
}

/// A new headset pairs while the settings window is open: the window has its name and
/// types the code it shows, and the pairing window closes once it paired.
#[test]
fn a_headset_pairs_through_the_settings_window() {
    let dir = TestDir::new("find-window");
    dir.pair();
    let options = Options {
        discovery: true,
        ..Options::default()
    };
    let mut pc = Pc::start(&dir, &options);
    let Some(bus) = &pc.bus else {
        eprintln!("no session bus: pairing through the settings window is not tested");
        return;
    };
    // The settings window, as far as the PC app sees it: its name on the bus.
    let window = bus.connect();
    let api = bus.connect();
    window.request_name(SETTINGS_NAME).unwrap();
    pc.wait_log(SOON, "the settings window is on the session bus");
    let call = |request: &str| {
        api.call_method(
            Some(BUS_NAME),
            OBJECT_PATH,
            Some(INTERFACE),
            "Call",
            &(request.to_owned(),),
        )
        .map(|m| {
            let json: String = m.body().deserialize().unwrap();
            serde_json::from_str::<serde_json::Value>(&json).unwrap()
        })
    };
    let opened = call(r#"{"request": "pair_new_headset"}"#).unwrap();
    assert_eq!(opened["seconds_left"], 180);
    // Nobody waits for a code yet.
    assert!(call(r#"{"request": "pairing_code", "code": "123456"}"#).is_err());

    let mut new = dir.keys_of("new");
    let addr = pc.addr;
    let pairing = std::thread::spawn(move || {
        let mut code = || "123456".to_owned();
        FakeHeadset::connect(addr, &mut new, Some(&mut code)).map(|h| (h, new))
    });
    let s = wait_state(&pc, "a headset waits for its code", |s| {
        s.pairing.as_ref().and_then(|p| p.code_for.as_deref()) == Some("prostir-testkit")
    })
    .unwrap();
    assert_eq!(s.pairing.unwrap().seconds_total, 180);
    call(r#"{"request": "pairing_code", "code": "123 456"}"#).unwrap();
    let (mut headset, mut new) = pairing.join().unwrap().expect("paired and connected");
    // Its session gives it what it finds the PC with from now on.
    headset.learn_find_secret(&mut new, SOON).unwrap();
    let to = pc.discovery.unwrap();
    let a = asks(to, &new, "").expect("the new headset finds its PC");
    assert!(a.from_pc(&dir.find_secret()));
    pc.wait_log(SOON, "a headset paired: the pairing window closes");
    let s = wait_state(&pc, "pairing closed", |s| s.pairing.is_none()).unwrap();
    assert_eq!(s.headsets.len(), 2);
    drop(window);
    pc.wait_log(SOON, "the settings window left the session bus");
    pc.quit();
}

type Wakeups = BTreeMap<u32, (String, u64)>;

/// The threads that woke between `before` and `after`, with how often.
fn woke(before: &Wakeups, after: &Wakeups) -> Vec<(String, u64)> {
    after
        .iter()
        .filter_map(|(tid, (name, n))| {
            let was = before.get(tid).map_or(0, |(_, n)| *n);
            (*n > was).then(|| (format!("{name} ({tid})"), n - was))
        })
        .collect()
}

/// Waits until process `pid` keeps its threads for a second and only those named in
/// `waking` wake in it.
fn settle(pid: u32, waking: &[&str], what: &str) {
    let started = Instant::now();
    loop {
        let before = wakeups_of(pid);
        std::thread::sleep(Duration::from_secs(1));
        let after = wakeups_of(pid);
        let others = after.iter().any(|(tid, (name, n))| {
            !waking.contains(&name.as_str()) && before.get(tid).is_none_or(|(_, was)| n > was)
        });
        if before.keys().eq(after.keys()) && !others {
            return;
        }
        assert!(
            started.elapsed() < SOON,
            "{what}: did not settle: {:?}",
            woke(&before, &after)
        );
    }
}

/// Waits until process `pid` does not wake for a second, then checks it does not
/// wake at all for [`WATCH`].
fn assert_asleep(pid: u32, what: &str) {
    settle(pid, &[], what);
    let before = wakeups_of(pid);
    std::thread::sleep(WATCH);
    let after = wakeups_of(pid);
    let only_in = |a: &Wakeups, b: &Wakeups| {
        a.iter()
            .filter(|(tid, _)| !b.contains_key(tid))
            .map(|(tid, (name, _))| format!("{name} ({tid})"))
            .collect::<Vec<_>>()
    };
    assert!(
        before.keys().eq(after.keys()),
        "{what}: threads came {:?} or went {:?}",
        only_in(&after, &before),
        only_in(&before, &after)
    );
    let woke = woke(&before, &after);
    assert!(woke.is_empty(), "{what}: woke in {WATCH:?}: {woke:?}");
}

/// A process's own memory, kB: what it has that is not a file mapped in.
fn own_memory_kb(pid: u32) -> (u64, u64) {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    let field = |name: &str| {
        status
            .lines()
            .find_map(|l| l.strip_prefix(name))
            .and_then(|v| v.split_whitespace().next())
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    };
    (field("RssAnon:"), field("VmRSS:"))
}

/// Waits until the PC app's full program runs (under the watcher), or does not.
fn wait_program(pc: &Pc, running: bool) -> u32 {
    let started = Instant::now();
    loop {
        let program = pc.program().unwrap();
        if (program != pc.pid()) == running {
            return program;
        }
        assert!(
            started.elapsed() < START,
            "the full program {}",
            if running {
                "did not start"
            } else {
                "did not leave"
            }
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Without systemd, `prostir serve` sleeps in its small watcher, waking for nothing;
/// it answers its own headset, and starts the full program ahead of the connection
/// that follows; once nothing needs it, the full program leaves and the watcher
/// sleeps again. A full program that crashes is started again for the next headset.
/// "Quit" ends the watcher too.
#[test]
fn the_watcher_sleeps_until_a_headset_comes() {
    let dir = TestDir::new("watcher");
    dir.save_monitors(TWO_MONITORS);
    let mut keys = dir.pair();
    let stranger = dir.keys_of("stranger");
    let options = Options {
        watcher: true,
        discovery: true,
        linger: 0,
        env: vec![("PROSTIR_TEST_QUIET_SECONDS".into(), "2".into())],
        ..Options::default()
    };
    let mut pc = Pc::start(&dir, &options);
    let to = pc.discovery.unwrap();
    assert_eq!(pc.program(), Some(pc.pid()), "only the watcher runs");
    assert_asleep(pc.pid(), "the watcher");
    let (own, rss) = own_memory_kb(pc.pid());
    eprintln!("the watcher: {own} kB of its own memory, {rss} kB resident");
    assert!(own < 5 * 1024, "the watcher's own memory: {own} kB");

    // A stranger: no answer, the notice, nothing started.
    assert!(silent(to, &stranger, ""));
    pc.wait_log(SOON, "notification: New headset nearby");
    pc.notice("close");
    assert_eq!(pc.program(), Some(pc.pid()));

    // Its own headset asks: answered, and the full program starts ahead.
    assert!(asks(to, &keys, "").is_some());
    pc.wait_log(SOON, "starting Prostir: Headset");
    // `prostir` while it starts (not on the bus yet): the watcher passes the wish
    // for the window on, and the program takes it once it can.
    // SAFETY: a signal to the PC app's own process.
    unsafe { libc::kill(pc.pid() as libc::pid_t, libc::SIGUSR1) };
    pc.wait_log(START, "SIGUSR1 received: the settings window");
    wait_program(&pc, true);
    // The full program shows no second notice within the minute: the watcher's counts.
    pc.wait_log_after(START, "discovery on UDP port", 1);
    assert!(silent(to, &stranger, ""));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        pc.count_log("notification: New headset nearby"),
        1,
        "once a minute, whichever program asks"
    );
    let mut headset = connect(&pc, &mut keys);
    headset
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    headset.leave(prostir_proto::HeadsetGoodbye::Quit).unwrap();
    pc.wait_log(START, "Prostir went to sleep: waiting for the headset");
    wait_program(&pc, false);
    assert_asleep(pc.pid(), "the watcher after the full program left");

    // A connection wakes it too; a crash mid-session is not the end.
    let mut headset = connect(&pc, &mut keys);
    headset
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    let program = wait_program(&pc, true);
    // SAFETY: a signal to the PC app's own child.
    unsafe { libc::kill(program as libc::pid_t, libc::SIGKILL) };
    pc.wait_log(SOON, "Prostir ended (killed)");
    drop(headset);
    let started = Instant::now();
    let mut headset = loop {
        match FakeHeadset::connect(pc.addr, &mut keys, None) {
            Ok(h) => break h,
            Err(e) => assert!(started.elapsed() < START, "reconnecting: {e}"),
        }
        std::thread::sleep(Duration::from_millis(300));
    };
    headset
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();

    // "Quit" (the local API's, else the tray's): the full program and the watcher end.
    match pc.ctl(&["quit"]) {
        Some(ctl) => assert_eq!(ctl.code, 0, "{ctl:?}"),
        None => pc.tray("quit"),
    }
    let status = pc.wait_end(SOON);
    assert!(status.success(), "{status}");
    pc.wait_log(SOON, "Prostir was asked to quit: the watcher quits too");
}

/// Under systemd: a stranger's query starts nothing heavy (the program answers and
/// leaves at once); one of its own headsets asking starts the full program ahead, which
/// leaves once its session ended; a connection starts it too. The sockets come in
/// systemd's way (`systemd-socket-activate`).
#[test]
fn socket_activation_starts_the_pc_for_its_headset_only() {
    let dir = TestDir::new("activated");
    dir.save_monitors(TWO_MONITORS);
    let stranger = dir.keys_of("stranger");
    let unpaired = |by_connection| Options {
        discovery: true,
        linger: 0,
        env: vec![("PROSTIR_TEST_QUIET_SECONDS".into(), "2".into())],
        activate: Activate::new(by_connection),
        ..Options::default()
    };
    // Unpaired, strangers' queries, more than systemd starts a service for in 10 s
    // (5 by default): every one answered by the one light program, which stays while
    // they come and leaves once they stopped, starting nothing heavy.
    let options = unpaired(false);
    if options.activate.is_none() {
        return;
    }
    let mut pc = Pc::start(&dir, &options);
    let pid = pc.pid();
    for i in 0..8 {
        assert!(
            asks(pc.discovery.unwrap(), &stranger, "").is_some(),
            "query {i}"
        );
        std::thread::sleep(Duration::from_millis(250));
    }
    assert_eq!(pc.program(), Some(pid), "the same program all along");
    let status = pc.wait_end(SOON);
    assert_eq!(status.code(), Some(0));
    assert_eq!(pc.count_log("nothing for Prostir to do: back to sleep"), 1);
    assert_eq!(
        pc.count_log("waiting for the headset on port"),
        0,
        "nothing heavy"
    );

    // Paired: its own headset asks, the full program starts and waits for it.
    let mut keys = dir.pair();
    let mut pc = Pc::start(&dir, &unpaired(false));
    assert!(asks(pc.discovery.unwrap(), &keys, "").is_some());
    pc.wait_log(START, "started: Headset");
    let mut headset = connect(&pc, &mut keys);
    headset
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    // Held by the settings window, the full program sleeps while idle.
    let window = pc.bus.as_ref().map(|bus| {
        let window = bus.connect();
        window.request_name(SETTINGS_NAME).unwrap();
        window
    });
    headset.leave(prostir_proto::HeadsetGoodbye::Quit).unwrap();
    if window.is_some() {
        pc.wait_log(SOON, "the settings window is on the session bus");
        pc.wait_log(SOON, "headset disconnected");
        assert_asleep(pc.pid(), "the socket-activated program, idle");
    }
    drop(window);
    let status = pc.wait_end(START);
    assert_eq!(status.code(), Some(0), "it leaves once nothing needs it");
    pc.wait_log(
        SOON,
        "nothing needs Prostir now: leaving until a headset comes",
    );

    // A connection that says nothing (a port scan) is let go: nothing heavy starts.
    let mut pc = Pc::start(&dir, &unpaired(true));
    let silent_peer = std::net::TcpStream::connect(pc.addr).unwrap();
    let status = pc.wait_end(SOON);
    assert_eq!(status.code(), Some(0));
    assert_eq!(pc.count_log("waiting for the headset on port"), 0);
    drop(silent_peer);

    // A connection starts it.

    let pc = Pc::start(&dir, &unpaired(true));
    let mut headset = connect(&pc, &mut keys);
    headset
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    pc.quit();
}

/// While the full program runs (with `--stay`, all along), its beacon wakes it once a
/// second, and nothing else does: the price of being found by headsets that do not
/// ask yet, until the headset asks (step 7 of 0.2.0) and the beacon goes.
#[test]
fn the_beacon_is_the_only_thing_that_wakes_a_running_pc() {
    let dir = TestDir::new("beacon");
    let heard = std::net::UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    heard
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let options = Options {
        discovery: true,
        beacon_to: Some(heard.local_addr().unwrap()),
        ..Options::default()
    };
    let mut pc = Pc::start(&dir, &options);
    let mut buf = [0u8; 512];
    let n = heard.recv(&mut buf).expect("a beacon");
    let (port, name) = prostir_proto::decode_beacon(&buf[..n]).expect("a beacon");
    assert_eq!((port, name.as_str()), (pc.addr.port(), "fake-pc"));
    // Settled, once the encoders' probe at the start is over, every wakeup is the
    // beacon's, about one a second.
    pc.wait_log(START, "video encoders probed in");
    settle(pc.pid(), &["beacon"], "the running PC");
    let before = wakeups_of(pc.pid());
    std::thread::sleep(WATCH);
    let after = wakeups_of(pc.pid());
    let mut beacon = 0;
    for (who, n) in woke(&before, &after) {
        assert!(
            who.starts_with("beacon ("),
            "only the beacon wakes: {who} woke {n}"
        );
        beacon += n;
    }
    assert!(
        (2..=5).contains(&beacon),
        "the beacon woke {beacon} times in {WATCH:?}"
    );
    pc.quit();
}
