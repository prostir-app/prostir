//! The headset's network code on the desktop against the testkit's fake PC (step 7
//! note, 10.2): a session from the dial to its parts and streams, a dropped
//! connection and the next one, leaving and forgetting with the right goodbye, an
//! unknown PC offered to the user, a PC that no longer trusts the headset, nothing at
//! an address.

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
use crate::session::Dest;

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
    })
    .unwrap();
    let (tx, rx) = mpsc::channel();
    let played = Arc::new(Played::default());
    let net = Net::start(
        Config {
            identity_file: headset_dir.join("identity"),
            trust_file: headset_dir.join("paired_hosts"),
            host_file: None,
            last_host_file: headset_dir.join("last_host"),
            name: "Test headset".into(),
        },
        tx,
        Arc::new(FakeSource(played.clone())),
        HeadsetState {
            caps: Some(crate::device::headset_caps("Test headset")),
            ..HeadsetState::default()
        },
    )
    .unwrap();
    (pc, net, rx, played)
}

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
