//! The headset app's own network code (`prostir-client`'s `net`, with a fake for its
//! sound and microphone) against the PC app (step 7 note, 10.2): a session with every
//! monitor streaming and the PC's sound, the PC quitting and coming back, leaving and
//! forgetting the PC with the right goodbye, and a PC that does not trust the headset
//! offered to the user. The search by queries (step 7.9): the PC found by asking,
//! straight at its address as through a VPN (never as a broadcast: nothing here goes
//! to the network); the first connection without the find secret, by its address, and
//! the next by asking; a find secret gone stale while another headset was forgotten;
//! and a sleeping PC woken by a query.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use headset::net::{Config, Disconnect, Feedback, FoundHost, Media, MediaSource, Net, NetEvent};
use headset::session::{AskTo, Dest, NetCmd, StepId};
use headset::ui::OfferWhy;
use prostir_proto::state::HeadsetState;
use prostir_proto::{HeadsetGoodbye, HostMsg, PcGoodbye};

use crate::pc::{Options, Pc, TestDir};
use crate::{SOON, START, TWO_MONITORS};

/// The PC's sound slices played, and the microphone switched on and off.
#[derive(Default)]
struct Played {
    slices: AtomicUsize,
    recording: Mutex<Vec<bool>>,
}

struct FakeMedia(Arc<Played>);

impl Media for FakeMedia {
    fn play(&mut self, _pcm: Vec<u8>) {
        self.0.slices.fetch_add(1, Ordering::Relaxed);
    }

    fn played(&mut self) -> Option<Vec<u8>> {
        None
    }

    fn silence(&mut self) {}

    fn record(&mut self, on: bool, _up: &Feedback) {
        self.0.recording.lock().unwrap().push(on);
    }

    fn mic_allowed(&self) -> bool {
        true
    }
}

struct FakeSource(Arc<Played>);

impl MediaSource for FakeSource {
    fn open(&self) -> Box<dyn Media> {
        Box::new(FakeMedia(self.0.clone()))
    }
}

/// The code the headset shows when it pairs in these tests, which the person at the
/// fake PC types.
const CODE: &str = "314159";

/// The headset's network thread with its files in `dir` (as the headset app keeps
/// them: `identity`, `paired_hosts`, `find_secrets`, `last_hosts`). No query goes
/// anywhere.
fn headset(dir: &Path) -> (Net, Receiver<NetEvent>, Arc<Played>) {
    headset_asking(dir, None)
}

/// [`headset`], asking the PCs at their addresses on `port` (the test PC's discovery
/// port): never as a broadcast.
fn headset_asking(dir: &Path, port: Option<u16>) -> (Net, Receiver<NetEvent>, Arc<Played>) {
    let (tx, rx) = mpsc::channel();
    let played = Arc::new(Played::default());
    let net = Net::start(
        Config {
            pairing_code: || CODE.to_owned(),
            discovery_port: port,
            broadcast: false,
            ..Config::in_dir(dir, None, "e2e headset")
        },
        tx,
        Arc::new(FakeSource(played.clone())),
        HeadsetState {
            caps: Some(
                headset::caps::Caps::without_extensions(
                    "e2e headset".into(),
                    headset::caps::DECODERS_UNSAID.to_vec(),
                    headset::caps::STREAMS_UNSAID,
                    None,
                )
                .headset_caps(),
            ),
            ..HeadsetState::default()
        },
    )
    .unwrap();
    (net, rx, played)
}

/// The next event that `what` picks within `within` (the others are passed over).
fn wait<T>(rx: &Receiver<NetEvent>, within: Duration, what: impl Fn(NetEvent) -> Option<T>) -> T {
    let until = Instant::now() + within;
    loop {
        let left = until.saturating_duration_since(Instant::now());
        let event = rx
            .recv_timeout(left)
            .expect("the headset's network thread did not get there");
        if let Some(t) = what(event) {
            return t;
        }
    }
}

fn dial(net: &Net, step: u32, pc: &Pc) {
    net.send(NetCmd::Dial {
        step: StepId(step),
        to: Dest::Addr(pc.addr),
        pair: None,
        pc_shows_code: false,
    });
}

/// Every monitor streams and the PC's sound plays; the PC quitting ends the session
/// with its goodbye, and the PC started again is reached again.
#[test]
fn the_headsets_network_code_streams_and_comes_back() {
    let dir = TestDir::new("headset-net");
    dir.save_monitors(TWO_MONITORS);
    let keys = dir.pair();
    let pc_key = dir.pc_key();
    let id = headset::ids::PcId::of(&pc_key);
    let (mut net, rx, played) = headset(&dir.path().join("headset"));
    let pc = Pc::start(&dir, &Options::default());
    net.send(NetCmd::MediaTo(Some(id)));
    dial(&net, 1, &pc);
    let reached = wait(&rx, START, |e| match e {
        NetEvent::Reached { pc, .. } => Some(pc),
        _ => None,
    });
    assert_eq!(reached, id);
    let shown = wait(&rx, START, |e| match e {
        NetEvent::Part(_, HostMsg::Displays { part }) if part.displays.len() >= 2 => {
            Some(part.displays.len())
        }
        _ => None,
    });
    assert_eq!(shown, 2);
    let mut sizes = Vec::new();
    while sizes.len() < 2 {
        sizes.push(wait(&rx, START, |e| match e {
            NetEvent::StreamStart { width, .. } => Some(width),
            _ => None,
        }));
    }
    sizes.sort_unstable();
    assert_eq!(sizes, [480, 640]);
    let until = Instant::now() + START;
    while played.slices.load(Ordering::Relaxed) == 0 {
        assert!(Instant::now() < until, "no sound played");
        std::thread::sleep(Duration::from_millis(20));
    }
    // The PC quits: its goodbye ends the session.
    pc.quit();
    let (why, opened) = wait(&rx, START, |e| match e {
        NetEvent::Ended {
            step: StepId(1),
            why,
            ended,
            ..
        } => Some((why, ended.opened)),
        _ => None,
    });
    assert_eq!(why, Disconnect::Goodbye(PcGoodbye::Quit));
    assert!(opened);
    // Started again (on a port of its own), it is reached again, and leaving it says
    // why.
    let pc = Pc::start(&dir, &Options::default());
    dial(&net, 2, &pc);
    wait(&rx, START, |e| {
        matches!(e, NetEvent::Connected { .. }).then_some(())
    });
    net.send(NetCmd::Leave(id, HeadsetGoodbye::OtherPc));
    let why = wait(&rx, START, |e| match e {
        NetEvent::Ended {
            step: StepId(2),
            why,
            ..
        } => Some(why),
        _ => None,
    });
    assert_ne!(why, Disconnect::Silence);
    drop(keys);
    assert!(net.stop(Duration::from_secs(2)));
    pc.quit();
}

/// Forgetting the PC in use leaves it (the PC hears the headset forgot it) and drops it
/// from the trusted ones; dialed again, it is offered to the user.
#[test]
fn the_headset_forgets_the_pc() {
    let dir = TestDir::new("headset-net-forget");
    dir.save_monitors(TWO_MONITORS);
    let _keys = dir.pair();
    let id = headset::ids::PcId::of(&dir.pc_key());
    let (mut net, rx, _) = headset(&dir.path().join("headset"));
    let mut pc = Pc::start(&dir, &Options::default());
    dial(&net, 1, &pc);
    wait(&rx, START, |e| {
        matches!(e, NetEvent::Connected { .. }).then_some(())
    });
    net.send(NetCmd::Forget(id));
    let left = wait(&rx, START, |e| match e {
        NetEvent::Paired(list) => Some(list),
        _ => None,
    });
    assert!(left.is_empty());
    wait(&rx, START, |e| {
        matches!(
            e,
            NetEvent::Ended {
                step: StepId(1),
                ..
            }
        )
        .then_some(())
    });
    pc.wait_log(START, "the headset forgot this PC");
    dial(&net, 2, &pc);
    let why = wait(&rx, START, |e| match e {
        NetEvent::PairOffer { why, .. } => Some(why),
        _ => None,
    });
    assert_eq!(why, OfferWhy::New);
    assert!(net.stop(Duration::from_secs(2)));
    pc.quit();
}

/// A PC that does not trust the headset (the headset trusts it): told to pair, the
/// headset offers it to the user as a PC that forgot it, and opens no session.
#[test]
fn a_pc_that_does_not_trust_the_headset_is_offered() {
    let dir = TestDir::new("headset-net-stranger");
    dir.save_monitors(TWO_MONITORS);
    let _keys = dir.pair();
    dir.forget_headsets();
    let (mut net, rx, _) = headset(&dir.path().join("headset"));
    let pc = Pc::start(&dir, &Options::default());
    dial(&net, 1, &pc);
    let why = wait(&rx, START, |e| match e {
        NetEvent::PairOffer { why, .. } => Some(why),
        _ => None,
    });
    assert_eq!(why, OfferWhy::Forgotten, "its own PC, which forgot it");
    let opened = wait(&rx, START, |e| match e {
        NetEvent::Ended { ended, .. } => Some(ended.opened),
        _ => None,
    });
    assert!(!opened);
    assert!(net.stop(Duration::from_secs(2)));
    pc.quit();
}

/// Dials `pc` again with the user's yes to pair with it, the PC or the headset showing
/// the code.
fn dial_to_pair(net: &Net, step: u32, pc: &Pc, key: [u8; 32], pc_shows_code: bool) {
    net.send(NetCmd::Dial {
        step: StepId(step),
        to: Dest::Addr(pc.addr),
        pair: Some(key),
        pc_shows_code,
    });
}

/// The code the PC shows now (its status through `prostir ctl`), once it shows one.
fn pc_code(pc: &Pc) -> String {
    let until = Instant::now() + SOON;
    loop {
        let status = pc
            .ctl(&["status", "--json"])
            .expect("a bus for prostir ctl");
        if let Some(code) = status.json()["pairing"]["pc_code"].as_str() {
            return code.to_owned();
        }
        assert!(
            Instant::now() < until,
            "the PC shows no code: {}",
            status.out
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The headset shows its code and the PC's dialog for it is up (the person at the PC
/// has not typed it yet); the person picks "the other way round" on the headset, which
/// hangs up and dials again for the PC's code. The PC lets go of the first attempt as
/// its connection ends, closing its dialog, and shows its code to the new one.
#[test]
fn the_other_way_round_while_the_pc_dialog_is_up() {
    let dir = TestDir::new("headset-net-other-way");
    dir.save_monitors(TWO_MONITORS);
    let (mut net, rx, _) = headset(&dir.path().join("headset"));
    let mut pc = Pc::start(
        &dir,
        &Options {
            typed_code: Some(CODE.into()),
            typed_code_delay: Duration::from_secs(60),
            ..Options::default()
        },
    );
    dial(&net, 1, &pc);
    let key = wait(&rx, START, |e| match e {
        NetEvent::PairOffer { key, .. } => Some(key),
        _ => None,
    });
    let ended = |n| {
        move |e| {
            matches!(
                e,
                NetEvent::Ended {
                    step: StepId(s),
                    ..
                } if s == n
            )
            .then_some(())
        }
    };
    wait(&rx, START, ended(1));
    dial_to_pair(&net, 2, &pc, key, false);
    wait(&rx, START, |e| match e {
        NetEvent::Pairing { code: Some(_), .. } => Some(()),
        _ => None,
    });
    pc.wait_log(SOON, "the person at the PC types the code");
    net.send(NetCmd::Hangup);
    wait(&rx, START, ended(2));
    pc.wait_log(SOON, "the headset hung up while its code was asked for");
    dial_to_pair(&net, 3, &pc, key, true);
    if pc.bus.is_some() {
        let code = pc_code(&pc).replace(' ', "");
        assert_eq!(code.len(), 6, "{code}");
    } else {
        pc.wait_log(SOON, "pairing code shown in the tray");
    }
    let busy = pc.lines("another headset is pairing");
    assert!(
        busy.is_empty(),
        "the new connection was turned away: {busy:?}"
    );
    let _ = net.stop(Duration::from_secs(2));
    pc.quit();
}

/// A new PC: offered, then paired as the user agreed, the headset showing the code
/// (the person at the PC types it); both sides trust each other, and the session opens.
#[test]
fn the_headset_pairs_with_a_new_pc() {
    let dir = TestDir::new("headset-net-pair");
    dir.save_monitors(TWO_MONITORS);
    let pc_key = dir.pc_key();
    let id = headset::ids::PcId::of(&pc_key);
    let (mut net, rx, _) = headset(&dir.path().join("headset"));
    let pc = Pc::start(
        &dir,
        &Options {
            typed_code: Some(CODE.into()),
            ..Options::default()
        },
    );
    dial(&net, 1, &pc);
    let key = wait(&rx, START, |e| match e {
        NetEvent::PairOffer {
            key,
            why: OfferWhy::New,
            ..
        } => Some(key),
        _ => None,
    });
    assert_eq!(key, pc_key);
    wait(&rx, START, |e| {
        matches!(
            e,
            NetEvent::Ended {
                step: StepId(1),
                ..
            }
        )
        .then_some(())
    });
    dial_to_pair(&net, 2, &pc, key, false);
    let code = wait(&rx, START, |e| match e {
        NetEvent::Pairing { code, .. } => Some(code),
        _ => None,
    });
    assert_eq!(code.as_deref(), Some("314 159"));
    let paired = wait(&rx, START, |e| match e {
        NetEvent::Paired(list) if !list.is_empty() => Some(list),
        _ => None,
    });
    assert_eq!(paired[0].pc, id);
    wait(&rx, START, |e| {
        matches!(e, NetEvent::Connected { pc, .. } if pc == id).then_some(())
    });
    assert!(net.stop(Duration::from_secs(2)));
    pc.quit();
}

/// The PC forgot the headset (the headset still trusts it): it asks to pair when the
/// session opens, inside the trusted PC's link. With the user's yes the PC shows the
/// code this time, the code typed reaches that link, and the session opens.
#[test]
fn the_headset_pairs_again_when_the_pc_forgot_it() {
    let dir = TestDir::new("headset-net-pc-forgot");
    dir.save_monitors(TWO_MONITORS);
    let _keys = dir.pair();
    dir.forget_headsets();
    let pc_key = dir.pc_key();
    let id = headset::ids::PcId::of(&pc_key);
    let (mut net, rx, _) = headset(&dir.path().join("headset"));
    let pc = Pc::start(&dir, &Options::default());
    if pc.bus.is_none() {
        eprintln!("no session bus: the PC's code cannot be read, skipped");
        return;
    }
    dial(&net, 1, &pc);
    wait(&rx, START, |e| {
        matches!(e, NetEvent::Reached { .. }).then_some(())
    });
    let key = wait(&rx, START, |e| match e {
        NetEvent::PairOffer { key, .. } => Some(key),
        _ => None,
    });
    wait(&rx, START, |e| {
        matches!(
            e,
            NetEvent::Ended {
                step: StepId(1),
                ..
            }
        )
        .then_some(())
    });
    dial_to_pair(&net, 2, &pc, key, true);
    wait(&rx, START, |e| {
        matches!(
            e,
            NetEvent::Reached {
                step: StepId(2),
                ..
            }
        )
        .then_some(())
    });
    let code = wait(&rx, START, |e| match e {
        NetEvent::Pairing { step, code, .. } => Some((step, code)),
        _ => None,
    });
    assert_eq!(code, (StepId(2), None), "the PC shows the code");
    net.send(NetCmd::SubmitCode(pc_code(&pc)));
    wait(&rx, START, |e| {
        matches!(e, NetEvent::Connected { pc, .. } if pc == id).then_some(())
    });
    assert!(net.stop(Duration::from_secs(2)));
    pc.quit();
}

/// A query of step `step` around (to the addresses the headset keeps, and `also`):
/// the answers, once its wait is over.
fn ask(net: &Net, rx: &Receiver<NetEvent>, step: u32, also: Option<SocketAddr>) -> Vec<FoundHost> {
    net.send(NetCmd::Ask {
        step: StepId(step),
        to: AskTo::Around { also },
        pairing: false,
    });
    let mut heard = Vec::new();
    loop {
        match wait(rx, START, |e| match e {
            NetEvent::Heard(h) => Some(Some(h)),
            NetEvent::Missed { step: s } if s == StepId(step) => Some(None),
            _ => None,
        }) {
            Some(h) => heard.push(h),
            None => return heard,
        }
    }
}

/// `file` in the headset's `dir`, once it holds `what`.
fn wait_file(dir: &Path, file: &str, what: &str) -> String {
    let until = Instant::now() + SOON;
    loop {
        let text = std::fs::read_to_string(dir.join(file)).unwrap_or_default();
        if text.contains(what) {
            return text;
        }
        assert!(Instant::now() < until, "{file}: {text:?}, not {what:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The PC is found by asking, straight at its address (as at a VPN address): the
/// answer carries its tag (the headset knows which PC it is before it connects) and
/// where to connect, and the session opens there.
#[test]
fn the_headset_finds_its_pc_by_asking() {
    let dir = TestDir::new("headset-net-ask");
    dir.save_monitors(TWO_MONITORS);
    let _keys = dir.pair();
    let id = headset::ids::PcId::of(&dir.pc_key());
    let options = Options {
        discovery: true,
        ..Options::default()
    };
    let pc = Pc::start(&dir, &options);
    let port = pc.discovery.unwrap().port();
    let (mut net, rx, _) = headset_asking(&dir.path().join("headset"), Some(port));
    let heard = ask(&net, &rx, 1, Some(pc.addr));
    assert_eq!(heard.len(), 1, "{heard:?}");
    assert_eq!(
        (heard[0].pc, heard[0].addr, heard[0].name.as_str()),
        (Some(id), pc.addr, "fake-pc")
    );
    net.send(NetCmd::Dial {
        step: StepId(2),
        to: Dest::Addr(heard[0].addr),
        pair: None,
        pc_shows_code: false,
    });
    wait(&rx, START, |e| {
        matches!(e, NetEvent::Connected { pc, .. } if pc == id).then_some(())
    });
    assert!(net.stop(Duration::from_secs(2)));
    pc.quit();
}

/// Right after an update the headset has no find secret: the PC (paired) does not
/// answer its query, but its address takes the connection, and the session gives the
/// secret; kept with that address, the next search finds the PC by asking alone.
#[test]
fn the_first_connection_without_the_secret_then_by_asking() {
    let dir = TestDir::new("headset-net-first");
    dir.save_monitors(TWO_MONITORS);
    let _keys = dir.pair();
    let headset_dir = dir.path().join("headset");
    std::fs::remove_file(headset_dir.join("find_secrets")).unwrap();
    let id = headset::ids::PcId::of(&dir.pc_key());
    let options = Options {
        discovery: true,
        ..Options::default()
    };
    let pc = Pc::start(&dir, &options);
    let port = pc.discovery.unwrap().port();
    let (mut net, rx, _) = headset_asking(&headset_dir, Some(port));
    assert!(
        ask(&net, &rx, 1, Some(pc.addr)).is_empty(),
        "no tag, no answer"
    );
    dial(&net, 2, &pc);
    wait(&rx, START, |e| {
        matches!(e, NetEvent::Connected { pc, .. } if pc == id).then_some(())
    });
    let secret = prostir_proto::secure::to_hex(&dir.find_secret());
    wait_file(&headset_dir, "find_secrets", &secret);
    let pc_hex = prostir_proto::secure::to_hex(&dir.pc_key());
    wait_file(&headset_dir, "last_hosts", &format!("{pc_hex} {}", pc.addr));
    net.send(NetCmd::Leave(id, HeadsetGoodbye::OtherPc));
    wait(&rx, START, |e| {
        matches!(
            e,
            NetEvent::Ended {
                step: StepId(2),
                ..
            }
        )
        .then_some(())
    });
    // Asked with nothing else to go on: the address kept, and the secret.
    let heard = ask(&net, &rx, 3, None);
    assert_eq!(heard.iter().map(|h| h.pc).collect::<Vec<_>>(), [Some(id)]);
    assert!(net.stop(Duration::from_secs(2)));
    pc.quit();
}

/// The PC forgot another headset while this one was off: it asks with the old find
/// secret, which the PC keeps valid for it, and is answered (with that secret's tag);
/// its session gives it the new one, and the old one is valid no more.
#[test]
fn a_stale_secret_still_finds_the_pc() {
    let dir = TestDir::new("headset-net-stale");
    dir.save_monitors(TWO_MONITORS);
    let old_keys = dir.pair();
    let _other = dir.pair_headset("other");
    let id = headset::ids::PcId::of(&dir.pc_key());
    let old = dir.find_secret();
    let options = Options {
        discovery: true,
        ..Options::default()
    };
    let mut pc = Pc::start(&dir, &options);
    let Some(ctl) = pc.ctl(&["forget", "other"]) else {
        pc.quit();
        return;
    };
    assert_eq!(ctl.code, 0, "{ctl:?}");
    pc.wait_log(SOON, "a new find secret");
    assert_ne!(dir.find_secret(), old);
    let headset_dir = dir.path().join("headset");
    let (mut net, rx, _) = headset_asking(&headset_dir, Some(pc.discovery.unwrap().port()));
    let heard = ask(&net, &rx, 1, Some(pc.addr));
    assert_eq!(heard.iter().map(|h| h.pc).collect::<Vec<_>>(), [Some(id)]);
    dial(&net, 2, &pc);
    wait(&rx, START, |e| {
        matches!(e, NetEvent::Connected { pc, .. } if pc == id).then_some(())
    });
    pc.wait_log(SOON, "has the current find secret now");
    let new = prostir_proto::secure::to_hex(&dir.find_secret());
    wait_file(&headset_dir, "find_secrets", &new);
    // The old secret (as the files had it before) gets nothing any more.
    let to = pc.discovery.unwrap();
    let with_old = prostir_testkit::find::query(&old_keys, "");
    assert!(prostir_testkit::find::ask(to, &with_old, Duration::from_millis(700)).is_none());
    assert!(net.stop(Duration::from_secs(2)));
    pc.quit();
}

/// Asleep in its sockets' watcher, the PC wakes for a query of its own headset: the
/// watcher answers it and starts the full program, which takes the connection that
/// follows. Once the headset left and the program went back to sleep, a query is
/// answered again by the watcher.
#[test]
fn a_sleeping_pc_wakes_for_its_headsets_query() {
    let dir = TestDir::new("headset-net-asleep");
    dir.save_monitors(TWO_MONITORS);
    let _keys = dir.pair();
    let id = headset::ids::PcId::of(&dir.pc_key());
    let options = Options {
        watcher: true,
        discovery: true,
        linger: 0,
        env: vec![("PROSTIR_TEST_QUIET_SECONDS".into(), "2".into())],
        ..Options::default()
    };
    let mut pc = Pc::start(&dir, &options);
    assert_eq!(pc.program(), Some(pc.pid()), "only the watcher runs");
    let port = pc.discovery.unwrap().port();
    let (mut net, rx, _) = headset_asking(&dir.path().join("headset"), Some(port));
    let heard = ask(&net, &rx, 1, Some(pc.addr));
    assert_eq!(heard.iter().map(|h| h.pc).collect::<Vec<_>>(), [Some(id)]);
    pc.wait_log(SOON, "starting Prostir: Headset");
    net.send(NetCmd::Dial {
        step: StepId(2),
        to: Dest::Addr(heard[0].addr),
        pair: None,
        pc_shows_code: false,
    });
    wait(&rx, START, |e| {
        matches!(e, NetEvent::Connected { pc, .. } if pc == id).then_some(())
    });
    net.send(NetCmd::Leave(id, HeadsetGoodbye::Quit));
    pc.wait_log(START, "Prostir went to sleep: waiting for the headset");
    let heard = ask(&net, &rx, 3, Some(pc.addr));
    assert_eq!(heard.len(), 1, "answered by the watcher again");
    assert!(net.stop(Duration::from_secs(2)));
    pc.quit();
}

/// What the headset says about pairing until the connection of step `step` ends, and
/// whether it ended as refused.
fn pairing_news(rx: &Receiver<NetEvent>, step: u32) -> (Vec<String>, bool) {
    let mut seen = Vec::new();
    loop {
        match rx.recv_timeout(START).expect("the connection did not end") {
            NetEvent::HostError(t) => seen.push(format!("error: {t}")),
            NetEvent::PairingFailed => seen.push("pairing failed".into()),
            NetEvent::PairOffer { why, .. } => seen.push(format!("offer {why:?}")),
            NetEvent::Pairing { code, .. } => seen.push(format!("pairing {code:?}")),
            NetEvent::Ended { step: s, ended, .. } if s == StepId(step) => {
                return (seen, ended.refused.is_some());
            }
            _ => {}
        }
    }
}

/// The PC has no dialog for the code the headset shows: the headset says the PC's
/// words and asks the user again at once (with the other way round), without a new
/// request, nor a count of wrong codes; the other way round then pairs.
#[test]
fn a_pc_without_a_dialog_is_not_asked_again() {
    let dir = TestDir::new("headset-net-no-dialog");
    dir.save_monitors(TWO_MONITORS);
    let id = headset::ids::PcId::of(&dir.pc_key());
    let (mut net, rx, _) = headset(&dir.path().join("headset"));
    let pc = Pc::start(&dir, &Options::default());
    dial(&net, 1, &pc);
    let key = wait(&rx, START, |e| match e {
        NetEvent::PairOffer { key, .. } => Some(key),
        _ => None,
    });
    wait(&rx, START, |e| {
        matches!(
            e,
            NetEvent::Ended {
                step: StepId(1),
                ..
            }
        )
        .then_some(())
    });
    dial_to_pair(&net, 2, &pc, key, false);
    let (seen, refused) = pairing_news(&rx, 2);
    assert_eq!(seen.len(), 3, "{seen:?}");
    assert_eq!(seen[0], "pairing Some(\"314 159\")");
    assert!(seen[1].starts_with("error: "), "the PC's words: {seen:?}");
    assert_eq!(seen[2], "offer PcRefused { pc_shows: false }");
    assert!(refused, "the search pauses as after a refusal");
    if pc.bus.is_none() {
        eprintln!("no session bus: the PC's code cannot be read, the other way skipped");
    } else {
        dial_to_pair(&net, 3, &pc, key, true);
        let code = wait(&rx, START, |e| match e {
            NetEvent::Pairing { code, .. } => Some(code),
            _ => None,
        });
        assert_eq!(code, None, "the PC shows the code");
        net.send(NetCmd::SubmitCode(pc_code(&pc)));
        wait(&rx, START, |e| {
            matches!(e, NetEvent::Connected { pc, .. } if pc == id).then_some(())
        });
    }
    assert!(net.stop(Duration::from_secs(2)));
    pc.quit();
}

/// The headset shows its code and the PC's dialog for it is up; the user picks "the
/// other way round" on the headset before anyone typed: the PC shows its code for the
/// new connection (the old dialog ends with its connection), and pairing goes on.
#[test]
#[ignore = "needs the PC side of audit N-2 (the code wait ends with its connection)"]
fn the_other_way_round_while_the_pc_dialog_is_up() {
    let dir = TestDir::new("headset-net-other-way");
    dir.save_monitors(TWO_MONITORS);
    let id = headset::ids::PcId::of(&dir.pc_key());
    let (mut net, rx, _) = headset(&dir.path().join("headset"));
    let pc = Pc::start(
        &dir,
        &Options {
            typed_code: Some(CODE.into()),
            typed_code_delay: Duration::from_secs(60),
            ..Options::default()
        },
    );
    if pc.bus.is_none() {
        eprintln!("no session bus: the PC's code cannot be read, skipped");
        return;
    }
    dial(&net, 1, &pc);
    let key = wait(&rx, START, |e| match e {
        NetEvent::PairOffer { key, .. } => Some(key),
        _ => None,
    });
    wait(&rx, START, |e| {
        matches!(
            e,
            NetEvent::Ended {
                step: StepId(1),
                ..
            }
        )
        .then_some(())
    });
    dial_to_pair(&net, 2, &pc, key, false);
    wait(&rx, START, |e| match e {
        NetEvent::Pairing { code: Some(_), .. } => Some(()),
        _ => None,
    });
    // The PC's dialog is up now.
    std::thread::sleep(Duration::from_millis(500));
    net.send(NetCmd::Hangup);
    wait(&rx, START, |e| {
        matches!(
            e,
            NetEvent::Ended {
                step: StepId(2),
                ..
            }
        )
        .then_some(())
    });
    dial_to_pair(&net, 3, &pc, key, true);
    let first = wait(&rx, START, |e| match e {
        NetEvent::Pairing {
            step: StepId(3),
            code: None,
            ..
        } => Some(Ok(())),
        NetEvent::HostError(text) => Some(Err(text)),
        NetEvent::PairOffer { why, .. } => Some(Err(format!("{why:?}"))),
        _ => None,
    });
    assert_eq!(first, Ok(()), "the PC refused the new connection");
    net.send(NetCmd::SubmitCode(pc_code(&pc)));
    wait(&rx, START, |e| {
        matches!(e, NetEvent::Connected { pc, .. } if pc == id).then_some(())
    });
    assert!(pc.lines("another headset is pairing").is_empty());
    assert!(net.stop(Duration::from_secs(2)));
    pc.quit();
}
