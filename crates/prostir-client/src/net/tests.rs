//! The headset's network code on the desktop against the testkit's fake PC (step 7
//! note, 10.2): a session from the dial to its parts and streams, a dropped
//! connection and the next one, leaving and forgetting with the right goodbye, an
//! unknown PC offered to the user, a PC that no longer trusts the headset, nothing at
//! an address; finding the PC by asking once its session gave the find secret, by its
//! last address before, and neither once it is forgotten; the last address of earlier
//! builds kept as its PC's.

#![expect(
    clippy::disallowed_methods,
    reason = "a test of the shell: its own clock, threads and files"
)]

use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use prostir_proto::state::HeadsetState;
use prostir_proto::{HeadsetGoodbye, HostMsg};
use prostir_testkit::pc::{Clip, FakePc, Happened, PcConfig, trust_each_other};

use super::*;
use crate::session::{AskTo, Dest};

/// What the fake media did, for the checks.
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

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("prostir-net-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The shipped clips, as the selftest plays them.
fn clips() -> Vec<Clip> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/selftest");
    let mut clips = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        if name.starts_with("h264-1280") {
            clips.push(Clip::named(&name, std::fs::read(&path).unwrap()).unwrap());
        }
    }
    assert!(!clips.is_empty());
    clips
}

/// A fake PC (trusting the headset unless `stranger`) and the headset's network
/// thread with its files in `dir` (trusting the PC unless `unknown`).
fn start(
    dir: &Path,
    stranger: bool,
    unknown: bool,
) -> (FakePc, Net, Receiver<NetEvent>, Arc<Played>) {
    let headset_dir = dir.join("headset");
    let pc_key = dir.join("pc-identity");
    let (headset, _) = trust_each_other(&headset_dir, &pc_key, "Fake").unwrap();
    if unknown {
        std::fs::remove_file(headset_dir.join("paired_hosts")).unwrap();
    }
    let pc = FakePc::start(PcConfig {
        identity: pc_key,
        trusted: if stranger { Vec::new() } else { vec![headset] },
        clips: clips(),
        monitors: 1,
        name: "Fake".into(),
        find_secret: SECRET,
    })
    .unwrap();
    let (tx, rx) = mpsc::channel();
    let played = Arc::new(Played::default());
    let net = Net::start(
        Config {
            // The fake PC answers queries on the UDP port of its TCP port's number;
            // nothing goes as a broadcast, nor to any other PC on this machine.
            discovery_port: Some(pc.addr().port()),
            broadcast: false,
            ..Config::in_dir(&headset_dir, None, "Test headset")
        },
        tx,
        Arc::new(FakeSource(played.clone())),
        HeadsetState {
            caps: Some(
                crate::caps::Caps::without_extensions(
                    "Test headset".into(),
                    crate::caps::DECODERS_UNSAID.to_vec(),
                    crate::caps::STREAMS_UNSAID,
                    None,
                )
                .headset_caps(),
            ),
            ..HeadsetState::default()
        },
    )
    .unwrap();
    (pc, net, rx, played)
}

/// The fake PC's find secret.
const SECRET: [u8; 32] = [7; 32];

/// The next event that `what` picks, within 10 s (the others are passed over).
fn wait<T>(rx: &Receiver<NetEvent>, what: impl Fn(NetEvent) -> Option<T>) -> T {
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let left = until.saturating_duration_since(Instant::now());
        let event = rx
            .recv_timeout(left)
            .expect("the network thread did not get there");
        if let Some(t) = what(event) {
            return t;
        }
    }
}

fn dial(net: &Net, step: u32, addr: SocketAddr) {
    net.send(NetCmd::Dial {
        step: StepId(step),
        to: Dest::Addr(addr),
        pair: None,
        pc_shows_code: false,
    });
}

fn wait_pc(pc: &FakePc, what: impl Fn(&Happened) -> bool) {
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let left = until.saturating_duration_since(Instant::now());
        let e = pc
            .events()
            .recv_timeout(left)
            .expect("the fake PC did not get there");
        if what(&e.what) {
            return;
        }
    }
}

#[test]
fn a_session_opens_streams_and_comes_back() {
    let dir = temp_dir("session");
    let (pc, mut net, rx, _) = start(&dir, false, false);
    let paired = wait(&rx, |e| match e {
        NetEvent::Paired(list) => Some(list),
        _ => None,
    });
    let id = PcId::of(&pc.key());
    assert_eq!(
        paired,
        [PairedPc {
            pc: id,
            name: "Fake".into()
        }]
    );
    net.send(NetCmd::MediaTo(Some(id)));
    dial(&net, 1, pc.addr());
    wait(&rx, |e| {
        matches!(
            e,
            NetEvent::Dialed {
                step: StepId(1),
                ..
            }
        )
        .then_some(())
    });
    let reached = wait(&rx, |e| match e {
        NetEvent::Reached { step, pc, .. } => Some((step, pc)),
        _ => None,
    });
    assert_eq!(reached, (StepId(1), id));
    wait(&rx, |e| match e {
        NetEvent::Connected { pc, .. } => Some(pc),
        _ => None,
    });
    wait(&rx, |e| {
        matches!(e, NetEvent::Part(p, HostMsg::Displays { .. }) if p == id).then_some(())
    });
    let (w, h) = wait(&rx, |e| match e {
        NetEvent::StreamStart {
            pc, width, height, ..
        } if pc == id => Some((width, height)),
        _ => None,
    });
    assert_eq!((w, h), (1280, 720));
    // The PC hangs up without a word: the stream ends, then the connection.
    assert!(pc.drop_connection());
    wait(&rx, |e| {
        matches!(e, NetEvent::StreamEnd { .. }).then_some(())
    });
    let (pc_of, why, ended) = wait(&rx, |e| match e {
        NetEvent::Ended { pc, why, ended, .. } => Some((pc, why, ended)),
        _ => None,
    });
    assert_eq!(pc_of, Some(id));
    assert_eq!(why, Disconnect::Closed);
    assert!(ended.opened);
    // The next dial finds it again.
    dial(&net, 2, pc.addr());
    wait(&rx, |e| {
        matches!(e, NetEvent::Connected { .. }).then_some(())
    });
    // Leaving says why.
    net.send(NetCmd::Leave(id, HeadsetGoodbye::OtherPc));
    wait_pc(&pc, |h| {
        *h == Happened::Goodbye {
            reason: HeadsetGoodbye::OtherPc.code(),
        }
    });
    wait(&rx, |e| {
        matches!(
            e,
            NetEvent::Ended {
                step: StepId(2),
                ..
            }
        )
        .then_some(())
    });
    assert!(net.stop(Duration::from_secs(2)));
    pc.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn forgetting_leaves_and_drops_the_pc() {
    let dir = temp_dir("forget");
    let (pc, mut net, rx, _) = start(&dir, false, false);
    let id = PcId::of(&pc.key());
    dial(&net, 1, pc.addr());
    wait(&rx, |e| {
        matches!(e, NetEvent::Connected { .. }).then_some(())
    });
    net.send(NetCmd::Forget(id));
    wait_pc(&pc, |h| {
        *h == Happened::Goodbye {
            reason: HeadsetGoodbye::Forgotten.code(),
        }
    });
    let list = wait(&rx, |e| match e {
        NetEvent::Paired(list) => Some(list),
        _ => None,
    });
    assert!(list.is_empty());
    let kept = std::fs::read_to_string(dir.join("headset/paired_hosts")).unwrap();
    assert!(kept.trim().is_empty(), "{kept:?}");
    wait(&rx, |e| {
        matches!(
            e,
            NetEvent::Ended {
                step: StepId(1),
                ..
            }
        )
        .then_some(())
    });
    // Not trusted any more: dialed again, it is offered to the user.
    dial(&net, 2, pc.addr());
    let why = wait(&rx, |e| match e {
        NetEvent::PairOffer { why, .. } => Some(why),
        _ => None,
    });
    assert_eq!(why, OfferWhy::New);
    wait(&rx, |e| {
        matches!(
            e,
            NetEvent::Ended {
                step: StepId(2),
                pc: None,
                ..
            }
        )
        .then_some(())
    });
    assert!(net.stop(Duration::from_secs(2)));
    pc.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A PC the headset does not trust is offered (before anything is sent); one that no
/// longer trusts the headset too, when it says so.
#[test]
fn strangers_are_offered_to_the_user() {
    let dir = temp_dir("unknown");
    let (pc, mut net, rx, _) = start(&dir, false, true);
    dial(&net, 1, pc.addr());
    let (key, why) = wait(&rx, |e| match e {
        NetEvent::PairOffer { key, why, .. } => Some((key, why)),
        _ => None,
    });
    assert_eq!((key, why), (pc.key(), OfferWhy::New));
    assert!(net.stop(Duration::from_secs(2)));
    pc.stop();
    let dir = temp_dir("stranger");
    let (pc, mut net, rx, _) = start(&dir, true, false);
    dial(&net, 1, pc.addr());
    wait_pc(&pc, |h| *h == Happened::NotTrusted);
    let why = wait(&rx, |e| match e {
        NetEvent::PairOffer { why, .. } => Some(why),
        _ => None,
    });
    assert_eq!(why, OfferWhy::New);
    let ended = wait(&rx, |e| match e {
        NetEvent::Ended { pc, ended, .. } => Some((pc, ended.opened)),
        _ => None,
    });
    assert_eq!(ended, (Some(PcId::of(&pc.key())), false));
    assert!(net.stop(Duration::from_secs(2)));
    pc.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Nothing at an address, or a file without one: missed at once.
#[test]
fn nothing_there_is_missed() {
    let dir = temp_dir("missed");
    let (pc, mut net, rx, _) = start(&dir, false, false);
    // A port nobody listens on any more.
    let free = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = free.local_addr().unwrap();
    drop(free);
    dial(&net, 7, addr);
    wait(&rx, |e| {
        matches!(e, NetEvent::Missed { step: StepId(7) }).then_some(())
    });
    net.send(NetCmd::Dial {
        step: StepId(8),
        to: Dest::HostFile,
        pair: None,
        pc_shows_code: false,
    });
    wait(&rx, |e| {
        matches!(e, NetEvent::Missed { step: StepId(8) }).then_some(())
    });
    // An address already in a session is not dialed twice.
    dial(&net, 9, pc.addr());
    wait(&rx, |e| {
        matches!(e, NetEvent::Connected { .. }).then_some(())
    });
    dial(&net, 10, pc.addr());
    wait(&rx, |e| {
        matches!(e, NetEvent::Missed { step: StepId(10) }).then_some(())
    });
    // Closing ends the session with a goodbye.
    assert!(net.stop(Duration::from_secs(2)));
    wait_pc(&pc, |h| {
        *h == Happened::Goodbye {
            reason: HeadsetGoodbye::Quit.code(),
        }
    });
    pc.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

fn ask(net: &Net, step: u32, to: AskTo, pairing: bool) {
    net.send(NetCmd::Ask {
        step: StepId(step),
        to,
        pairing,
    });
}

/// What the answers to the query of step `step` were, once its wait is over.
fn answers(rx: &Receiver<NetEvent>, step: u32) -> Vec<FoundHost> {
    let mut heard = Vec::new();
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let left = until.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left).expect("the query did not end") {
            NetEvent::Heard(h) => heard.push(h),
            NetEvent::Missed { step: s } if s == StepId(step) => return heard,
            _ => {}
        }
    }
}

/// `file` in `dir`, once it holds `what` (it is written by the network thread).
fn wait_file(dir: &Path, file: &str, what: &str) -> String {
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let text = std::fs::read_to_string(dir.join(file)).unwrap_or_default();
        if text.contains(what) {
            return text;
        }
        assert!(Instant::now() < until, "{file}: {text:?}, not {what:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A trusted PC that gave no find secret yet is not found by asking (it answers its
/// own headsets only), but by its last address once its session opened there; its
/// session gives the secret, and from then on a query finds it, with its tag. Forgotten,
/// neither: its secret and address are gone from the files.
#[test]
fn the_pc_is_found_by_asking_once_it_gave_its_secret() {
    let dir = temp_dir("asking");
    let (pc, mut net, rx, _) = start(&dir, false, false);
    let id = PcId::of(&pc.key());
    let headset = dir.join("headset");
    // Nothing known: a query around goes nowhere (no broadcast in tests); straight to
    // the PC it carries no tag of it, and gets no answer.
    ask(&net, 1, AskTo::Around { also: None }, false);
    assert!(answers(&rx, 1).is_empty());
    ask(&net, 2, AskTo::Exactly(pc.addr()), false);
    assert!(answers(&rx, 2).is_empty());
    wait_pc(&pc, |h| {
        *h == Happened::Asked {
            ours: false,
            answered: false,
        }
    });
    // The first session: its secret and its address are kept.
    dial(&net, 3, pc.addr());
    wait(&rx, |e| {
        matches!(e, NetEvent::Connected { .. }).then_some(())
    });
    let secrets = wait_file(&headset, "find_secrets", &secure::to_hex(&SECRET));
    assert!(secrets.starts_with("format 2\n"), "{secrets:?}");
    let pc_hex = secure::to_hex(&pc.key());
    wait_file(&headset, "last_hosts", &format!("{pc_hex} {}", pc.addr()));
    assert!(pc.drop_connection());
    wait(&rx, |e| matches!(e, NetEvent::Ended { .. }).then_some(()));
    // Now a query to the addresses known finds it, by its tag, and says where.
    ask(&net, 4, AskTo::Around { also: None }, false);
    let heard = answers(&rx, 4);
    assert_eq!(heard.len(), 1, "{heard:?}");
    assert_eq!(
        (
            heard[0].pc,
            heard[0].addr,
            heard[0].name.as_str(),
            heard[0].pairing
        ),
        (Some(id), pc.addr(), "Fake", false)
    );
    assert!(heard[0].at_last, "from the address it was kept at");
    wait_pc(&pc, |h| {
        *h == Happened::Asked {
            ours: true,
            answered: true,
        }
    });
    // Its last address by TCP (what the search does while it does not answer).
    net.send(NetCmd::Dial {
        step: StepId(5),
        to: Dest::LastOf(id),
        pair: None,
        pc_shows_code: false,
    });
    wait(&rx, |e| {
        matches!(e, NetEvent::Connected { pc: p, .. } if p == id).then_some(())
    });
    // Forgotten: its secret and address go; asked again, nothing answers for it.
    net.send(NetCmd::Forget(id));
    wait(&rx, |e| {
        matches!(
            e,
            NetEvent::Ended {
                step: StepId(5),
                ..
            }
        )
        .then_some(())
    });
    assert!(!wait_file(&headset, "find_secrets", "format 2").contains(&pc_hex));
    assert!(!wait_file(&headset, "last_hosts", "format 2").contains(&pc_hex));
    ask(&net, 6, AskTo::Around { also: None }, false);
    assert!(answers(&rx, 6).is_empty(), "nowhere to ask, nothing to tag");
    ask(&net, 7, AskTo::Exactly(pc.addr()), true);
    assert!(answers(&rx, 7).is_empty());
    net.send(NetCmd::Dial {
        step: StepId(8),
        to: Dest::LastOf(id),
        pair: None,
        pc_shows_code: false,
    });
    wait(&rx, |e| {
        matches!(e, NetEvent::Missed { step: StepId(8) }).then_some(())
    });
    assert!(net.stop(Duration::from_secs(2)));
    pc.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The one last address earlier builds kept (`last_host`: the PC's number and address)
/// becomes that PC's in `last_hosts`, and the old file goes; the PC is then found by
/// it.
#[test]
fn the_last_address_of_earlier_builds_is_kept() {
    let dir = temp_dir("old-last");
    let headset = dir.join("headset");
    let pc_key = dir.join("pc-identity");
    let (_, key) = trust_each_other(&headset, &pc_key, "Fake").unwrap();
    let id = PcId::of(&key);
    std::fs::write(headset.join("last_host"), format!("{id} 127.0.0.1:1\n")).unwrap();
    let (pc, mut net, rx, _) = start(&dir, false, false);
    let pc_hex = secure::to_hex(&pc.key());
    wait_file(&headset, "last_hosts", &format!("{pc_hex} 127.0.0.1:1"));
    assert!(!headset.join("last_host").exists());
    // Its session at its real address replaces it.
    dial(&net, 1, pc.addr());
    wait(&rx, |e| {
        matches!(e, NetEvent::Connected { .. }).then_some(())
    });
    wait_file(&headset, "last_hosts", &format!("{pc_hex} {}", pc.addr()));
    assert!(net.stop(Duration::from_secs(2)));
    pc.stop();
    let _ = std::fs::remove_dir_all(&dir);
}
