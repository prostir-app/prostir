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
use crate::session::{AskTo, Dest, Takes};

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
        takes: Takes::Any,
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

/// Forgetting a PC the file does not have (gone already) is still answered with the
/// list, which the menu waits for.
#[test]
fn forgetting_a_pc_not_kept_still_answers() {
    let dir = temp_dir("forget-unknown");
    let (pc, mut net, rx, _) = start(&dir, false, false);
    wait(&rx, |e| matches!(e, NetEvent::Paired(_)).then_some(()));
    net.send(NetCmd::Forget(PcId::of(&[42; 32])));
    let list = wait(&rx, |e| match e {
        NetEvent::Paired(list) => Some(list),
        _ => None,
    });
    assert_eq!(list.len(), 1, "the PC trusted stays");
    assert!(net.stop(Duration::from_secs(2)));
    pc.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The events until the connection of step `step` ended (within 10 s), in order.
fn until_ended(rx: &Receiver<NetEvent>, step: u32) -> Vec<NetEvent> {
    let until = Instant::now() + Duration::from_secs(10);
    let mut seen = Vec::new();
    loop {
        let left = until.saturating_duration_since(Instant::now());
        let e = rx
            .recv_timeout(left)
            .expect("the connection did not end in time");
        let end = matches!(e, NetEvent::Ended { step: s, .. } if s == StepId(step));
        seen.push(e);
        if end {
            return seen;
        }
    }
}

/// A dial that takes another PC (the cable leads elsewhere than the PC wanted): the
/// PC it reaches is let go after the handshake, before a word, and said so; the next
/// dial that takes it opens its session.
#[test]
fn a_dial_lets_go_a_pc_it_does_not_take() {
    let dir = temp_dir("let-go");
    let (pc, mut net, rx, _) = start(&dir, false, false);
    let id = PcId::of(&pc.key());
    wait(&rx, |e| matches!(e, NetEvent::Paired(_)).then_some(()));
    net.send(NetCmd::Dial {
        step: StepId(1),
        to: Dest::Addr(pc.addr()),
        pair: None,
        pc_shows_code: false,
        takes: Takes::Only(vec![PcId::of(&[42; 32])]),
    });
    let seen = until_ended(&rx, 1);
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, NetEvent::Reached { .. } | NetEvent::PairOffer { .. })),
        "not taken"
    );
    let let_go = seen.iter().find_map(|e| match e {
        NetEvent::Ended {
            ended, pc: None, ..
        } => ended.let_go,
        _ => None,
    });
    assert_eq!(let_go, Some(id));
    net.send(NetCmd::Dial {
        step: StepId(2),
        to: Dest::Addr(pc.addr()),
        pair: None,
        pc_shows_code: false,
        takes: Takes::AllBut(vec![PcId::of(&[42; 32])]),
    });
    wait(&rx, |e| {
        matches!(e, NetEvent::Connected { .. }).then_some(())
    });
    assert!(net.stop(Duration::from_secs(2)));
    pc.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Forgotten while its dial is still on the way (before the PC said who it is): no
/// session opens with it; it is a PC like any other now, offered to the user.
#[test]
fn forgetting_during_a_dial_opens_no_session() {
    let dir = temp_dir("forget-dialing");
    let (pc, mut net, rx, _) = start(&dir, false, false);
    let id = PcId::of(&pc.key());
    wait(&rx, |e| matches!(e, NetEvent::Paired(_)).then_some(()));
    dial(&net, 1, pc.addr());
    net.send(NetCmd::Forget(id));
    let seen = until_ended(&rx, 1);
    assert!(
        !seen.iter().any(|e| matches!(e, NetEvent::Connected { .. })),
        "a session with the PC forgotten"
    );
    assert!(
        seen.iter().any(|e| matches!(
            e,
            NetEvent::PairOffer {
                why: OfferWhy::New,
                ..
            }
        )),
        "offered as a new PC"
    );
    assert!(net.stop(Duration::from_secs(2)));
    pc.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Forgetting a PC whose file cannot be written changes nothing: it is still trusted
/// (the list says so), and its session goes on.
#[test]
fn a_forget_that_cannot_be_kept_changes_nothing() {
    let dir = temp_dir("forget-failed");
    let (pc, mut net, rx, _) = start(&dir, false, false);
    let id = PcId::of(&pc.key());
    dial(&net, 1, pc.addr());
    wait(&rx, |e| {
        matches!(e, NetEvent::Connected { .. }).then_some(())
    });
    // The file's place taken by a folder: it cannot be written.
    let file = dir.join("headset/paired_hosts");
    std::fs::remove_file(&file).unwrap();
    std::fs::create_dir(&file).unwrap();
    std::fs::write(file.join("in-the-way"), "").unwrap();
    net.send(NetCmd::Forget(id));
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let left = until.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left).expect("no word of the forget") {
            NetEvent::Paired(list) => {
                assert!(list.iter().any(|p| p.pc == id), "still trusted: {list:?}");
            }
            NetEvent::ForgetFailed { pc: p, .. } => {
                assert_eq!(p, id);
                break;
            }
            NetEvent::Ended { .. } => panic!("the session ended"),
            _ => {}
        }
    }
    // Still in its session a while later.
    let until = Instant::now() + Duration::from_millis(500);
    while let Some(left) = until.checked_duration_since(Instant::now()) {
        if let Ok(e) = rx.recv_timeout(left) {
            assert!(!matches!(e, NetEvent::Ended { .. }), "the session ended");
        }
    }
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

/// Leaving a PC (another picked, say) ends every stream before the connection, also
/// when the PC's ping crossed the goodbye: nothing is sent any more, and the monitors
/// must not keep their decoders.
#[test]
fn leaving_ends_every_stream() {
    let dir = temp_dir("leave-streams");
    let (pc, mut net, rx, _) = start(&dir, false, false);
    let pc_id = PcId::of(&pc.key());
    dial(&net, 1, pc.addr());
    let stream = wait(&rx, |e| match e {
        NetEvent::StreamStart { pc: p, id, .. } if p == pc_id => Some(id),
        _ => None,
    });
    pc.ping_when_left(true);
    net.send(NetCmd::Leave(pc_id, HeadsetGoodbye::OtherPc));
    let seen = until_ended(&rx, 1);
    assert!(
        seen.iter().any(
            |e| matches!(e, NetEvent::StreamEnd { pc: p, id: s } if *p == pc_id && *s == stream)
        ),
        "the stream did not end"
    );
    assert!(net.stop(Duration::from_secs(2)));
    pc.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A PC that takes the connection and the versions, then refuses each request to pair
/// with `text` (as the PC app does when it has no dialog for the code, or another
/// headset is pairing), and its answer that ends the attempt unless `no_result`: its
/// address and key, and what the headset asked of it, by the messages' names, once the
/// headset is gone.
fn refusing_pc(
    dir: &Path,
    text: &'static str,
    no_result: bool,
) -> (SocketAddr, [u8; 32], JoinHandle<Vec<String>>) {
    use prostir_proto::handshake;
    use prostir_proto::link::tcp::{NoiseTcp, Plain};
    use prostir_proto::packet::{self, lane};
    let identity = Identity::load_or_create(&dir.join("pc-identity")).unwrap();
    let key = identity.public;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let pc = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut link = NoiseTcp::accept(stream, &identity, Arc::new(Plain)).unwrap();
        handshake::read_first(&mut *link.receiver).unwrap();
        handshake::send_from_pc(
            &mut *link.sender,
            &HostMsg::Welcome {
                version: prostir_proto::PROTOCOL_VERSION,
                language: "en".into(),
            },
        )
        .unwrap();
        let mut packets = ClientMsg::assembler();
        let mut asked = Vec::new();
        while let Ok(msg) = ClientMsg::recv(&mut *link.receiver, &mut packets) {
            let name = format!("{msg:?}");
            asked.push(name.split([' ', '{', '(']).next().unwrap_or("").to_owned());
            if matches!(msg, ClientMsg::PairBegin { .. }) {
                let replies = [
                    HostMsg::Error { text: text.into() },
                    HostMsg::PairResult { tag: None },
                ];
                let n = if no_result { 1 } else { 2 };
                for reply in replies.into_iter().take(n) {
                    let sent = reply.encode().and_then(|m| {
                        packet::send_whole(&mut *link.sender, lane::CONTROL, m, 1200)
                    });
                    if sent.is_err() {
                        return asked;
                    }
                }
            }
        }
        asked
    });
    (addr, key, pc)
}

/// The PC refuses to pair (no dialog for the code, or another headset pairing): the
/// headset says the PC's words and asks the user again at once, with no new request
/// and no count of wrong codes; the search pauses as after a refusal. Whichever side
/// was to show the code: also while the user waits for the PC's code to type. Its
/// answer that ends the attempt is waited for a moment only (not the minutes a code
/// takes).
#[test]
fn a_pc_that_refuses_to_pair_is_not_asked_again() {
    for (pc_shows_code, no_result) in [(false, false), (true, false), (false, true), (true, true)] {
        let dir = temp_dir(&format!("refused-{pc_shows_code}-{no_result}"));
        let headset_dir = dir.join("headset");
        std::fs::create_dir_all(&headset_dir).unwrap();
        let (addr, key, pc) = refusing_pc(&dir, "No dialog", no_result);
        let (tx, rx) = mpsc::channel();
        let mut net = Net::start(
            Config {
                discovery_port: None,
                broadcast: false,
                ..Config::in_dir(&headset_dir, None, "Test headset")
            },
            tx,
            Arc::new(FakeSource(Arc::default())),
            HeadsetState::default(),
        )
        .unwrap();
        net.send(NetCmd::Dial {
            step: StepId(1),
            to: Dest::Addr(addr),
            pair: Some(key),
            pc_shows_code,
            takes: Takes::Any,
        });
        let seen = until_ended(&rx, 1);
        let said = seen
            .iter()
            .any(|e| matches!(e, NetEvent::HostError(t) if t == "No dialog"));
        assert!(said, "the PC's words: {pc_shows_code}");
        let offers: Vec<OfferWhy> = seen
            .iter()
            .filter_map(|e| match e {
                NetEvent::PairOffer { why, .. } => Some(*why),
                _ => None,
            })
            .collect();
        assert_eq!(offers.len(), 1, "asked again: {pc_shows_code}");
        assert_eq!(
            offers[0],
            OfferWhy::PcRefused {
                pc_shows: pc_shows_code
            },
            "not wrong codes"
        );
        let refused = seen
            .iter()
            .any(|e| matches!(e, NetEvent::Ended { ended, .. } if ended.refused.is_some()));
        assert!(refused, "the search pauses: {pc_shows_code}");
        assert!(net.stop(Duration::from_secs(2)));
        let asked = pc.join().unwrap();
        let begins = asked.iter().filter(|m| *m == "PairBegin").count();
        assert_eq!(begins, 1, "{asked:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A PC the headset does not trust is offered (before anything is sent); one that no
/// longer trusts the headset too, when it says so, as a PC that forgot it.
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
    assert_eq!(why, OfferWhy::Forgotten, "a PC of its own that forgot it");
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
        takes: Takes::Any,
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
        takes: Takes::Any,
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
        takes: Takes::Any,
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
