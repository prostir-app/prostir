//! The headset app's own network code (`prostir-client`'s `net`, with a fake for its
//! sound and microphone) against the PC app (step 7 note, 10.2): a session with every
//! monitor streaming and the PC's sound, the PC quitting and coming back, leaving and
//! forgetting the PC with the right goodbye, and a PC that does not trust the headset
//! offered to the user. The search for the PC by queries comes with step 7.9.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use headset::net::{Config, Disconnect, Feedback, Media, MediaSource, Net, NetEvent};
use headset::session::{Dest, NetCmd, StepId};
use headset::ui::OfferWhy;
use prostir_proto::state::HeadsetState;
use prostir_proto::{HeadsetGoodbye, HostMsg, PcGoodbye};

use crate::pc::{Options, Pc, TestDir};
use crate::{START, TWO_MONITORS};

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

/// The headset's network thread with its files in `dir` (as the headset app keeps
/// them: `identity`, `paired_hosts`).
fn headset(dir: &Path) -> (Net, Receiver<NetEvent>, Arc<Played>) {
    let (tx, rx) = mpsc::channel();
    let played = Arc::new(Played::default());
    let net = Net::start(
        Config {
            identity_file: dir.join("identity"),
            trust_file: dir.join("paired_hosts"),
            host_file: None,
            last_host_file: dir.join("last_host"),
            name: "e2e headset".into(),
        },
        tx,
        Arc::new(FakeSource(played.clone())),
        HeadsetState {
            caps: Some(headset::device::headset_caps("e2e headset")),
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
/// headset offers it to the user and does not open a session.
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
    assert_eq!(why, OfferWhy::New);
    let opened = wait(&rx, START, |e| match e {
        NetEvent::Ended { ended, .. } => Some(ended.opened),
        _ => None,
    });
    assert!(!opened);
    assert!(net.stop(Duration::from_secs(2)));
    pc.quit();
}
