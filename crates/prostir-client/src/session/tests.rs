#![expect(
    clippy::disallowed_methods,
    reason = "a test's own starting time; the code under test takes the time as a parameter"
)]

use std::net::{Ipv4Addr, SocketAddr};

use prostir_proto::request::Outcome;
use prostir_proto::state::ErrorCode;

use super::*;
use crate::lang;

fn addr(last: u8) -> SocketAddr {
    SocketAddr::new(
        Ipv4Addr::new(192, 168, 1, last).into(),
        prostir_proto::TCP_PORT,
    )
}

fn usb() -> SocketAddr {
    SocketAddr::new(Ipv4Addr::LOCALHOST.into(), prostir_proto::TCP_PORT)
}

fn key(n: u8) -> [u8; 32] {
    [n; 32]
}

fn pc(n: u8) -> PcId {
    PcId::of(&key(n))
}

const MS: Duration = Duration::from_millis(1);

/// A session with the clock in its hands, and what it told the network thread.
struct Sim {
    s: Session,
    now: Instant,
    texts: Texts,
}

impl Sim {
    fn new(max_pcs: usize) -> Self {
        let now = Instant::now();
        Self {
            s: Session::new(max_pcs, None, now),
            now,
            texts: lang::builtin("en"),
        }
    }

    fn after(&mut self, d: Duration) {
        self.now += d;
        self.s.tick(self.now);
    }

    /// The commands since the last look.
    fn cmds(&mut self) -> Vec<NetCmd> {
        self.s.take_cmds().collect()
    }

    /// The one step the network thread was asked for, and its number.
    fn step(&mut self) -> (StepId, NetCmd) {
        let cmds: Vec<NetCmd> = self
            .cmds()
            .into_iter()
            .filter(|c| !matches!(c, NetCmd::MediaTo(_)))
            .collect();
        assert_eq!(cmds.len(), 1, "one step: {cmds:?}");
        let cmd = cmds.into_iter().next().unwrap();
        let step = match &cmd {
            NetCmd::Dial { step, .. } | NetCmd::Listen { step } | NetCmd::Scan { step } => *step,
            other => panic!("not a step: {other:?}"),
        };
        (step, cmd)
    }

    fn missed(&mut self, step: StepId) {
        self.s.missed(step, self.now);
    }

    /// The PC `n` answers step `step` at `at` and its session opens.
    fn reach(&mut self, step: StepId, n: u8, at: SocketAddr) {
        self.s.dialed(step, at);
        self.s.reached(step, pc(n), at, self.now);
        self.s.connected_to(pc(n), at, &self.texts);
    }

    fn end(&mut self, step: StepId, n: Option<u8>, ended: &Ended, lasted: Duration) -> LinkEnded {
        self.s.ended(step, n.map(pc), ended, lasted, self.now)
    }
}

fn dest(cmd: &NetCmd) -> Dest {
    match cmd {
        NetCmd::Dial { to, .. } => *to,
        other => panic!("not a dial: {other:?}"),
    }
}

/// A round goes as the headset of 0.1 went: USB, listening, the typed address, the
/// last PC's, the one PC heard, a probe of the network; then a pause, and again.
#[test]
fn a_round_tries_each_way_in_turn() {
    let mut sim = Sim::new(MAX_PCS);
    sim.after(MS);
    let (s, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::Usb);
    sim.missed(s);
    let (s, cmd) = sim.step();
    assert!(matches!(cmd, NetCmd::Listen { .. }));
    sim.missed(s);
    let (s, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::HostFile);
    sim.missed(s);
    let (s, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::LastHost);
    sim.missed(s);
    // Nobody heard: no sole PC; the first probe of the network is due at once.
    let (s, cmd) = sim.step();
    assert!(matches!(cmd, NetCmd::Scan { .. }));
    sim.missed(s);
    assert!(sim.cmds().is_empty(), "a pause");
    sim.after(RETRY_PAUSE - MS);
    assert!(sim.cmds().is_empty());
    sim.after(MS);
    let (s, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::Usb);
    // A late word about an older step changes nothing.
    sim.missed(StepId(1));
    assert!(sim.cmds().is_empty());
    sim.missed(s);
    let (s, _) = sim.step();
    // One PC heard: the round tries it after the files; the probe is not due again yet.
    sim.s.hosts(vec![FoundHost {
        name: "Desk".into(),
        addr: addr(7),
    }]);
    sim.missed(s);
    let (s, _) = sim.step();
    sim.missed(s);
    let (s, _) = sim.step();
    sim.missed(s);
    let (s, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::Addr(addr(7)));
    sim.missed(s);
    assert_eq!(
        sim.s.search().unreachable.as_deref(),
        Some("Desk"),
        "heard but refusing: a firewall"
    );
    assert!(sim.cmds().is_empty(), "no probe yet");
}

/// The selftest's fake PC is the only address dialed, again and again.
#[test]
fn the_selftest_dials_its_fake_pc_alone() {
    let now = Instant::now();
    let mut s = Session::new(MAX_PCS, Some(addr(9)), now);
    s.tick(now);
    let cmds: Vec<NetCmd> = s.take_cmds().collect();
    let [NetCmd::Dial { step, to, .. }] = cmds.as_slice() else {
        panic!("{cmds:?}");
    };
    assert_eq!(*to, Dest::Addr(addr(9)));
    s.missed(*step, now);
    assert_eq!(s.take_cmds().count(), 0);
    s.tick(now + RETRY_PAUSE);
    let cmds: Vec<NetCmd> = s.take_cmds().collect();
    assert!(matches!(cmds.as_slice(), [NetCmd::Dial { to: Dest::Addr(a), .. }] if *a == addr(9)));
}

/// Connected, the search stops (one PC at a time); the session ending starts it again,
/// at once after a session that ran a while and broke off, after the usual pause
/// otherwise, and after 10 s when the PC refused or speaks another version.
#[test]
fn after_a_session_the_search_rests_as_it_ended() {
    let mut sim = Sim::new(MAX_PCS);
    sim.after(MS);
    let (s, _) = sim.step();
    sim.reach(s, 1, usb());
    assert_eq!(sim.s.main(), Some(pc(1)));
    assert_eq!(sim.s.host(), Some("PC (USB)"));
    assert_eq!(sim.cmds(), [NetCmd::MediaTo(Some(pc(1)))]);
    sim.after(Duration::from_secs(60));
    assert!(sim.cmds().is_empty(), "no search during the session");
    // Broke off after a minute: again at once.
    let broke = Ended {
        opened: true,
        ..Ended::default()
    };
    let ended = sim.end(s, Some(1), &broke, Duration::from_secs(60));
    assert!(ended.offline);
    assert!(matches!(sim.s.links()[&pc(1)], Link::Lost { .. }));
    assert_eq!(sim.cmds(), [NetCmd::MediaTo(None)]);
    sim.after(Duration::ZERO);
    let (s, _) = sim.step();
    sim.reach(s, 1, usb());
    let _ = sim.cmds();
    // Right after it opened: the usual pause.
    sim.end(s, Some(1), &broke, Duration::from_millis(500));
    let _ = sim.cmds();
    sim.after(RETRY_PAUSE - MS);
    assert!(sim.cmds().is_empty());
    sim.after(MS);
    let (s, _) = sim.step();
    // The PC said what is wrong and hung up: 10 s.
    sim.s.dialed(s, usb());
    let refused = Ended {
        refused: Some(sim.now),
        ..Ended::default()
    };
    sim.end(s, None, &refused, Duration::from_millis(50));
    sim.after(REFUSED_PAUSE - MS);
    assert!(sim.cmds().is_empty());
    sim.after(MS);
    let (s, _) = sim.step();
    sim.s.dialed(s, usb());
    sim.s.incompatible(true, "0.3.0".into());
    let other = Ended {
        incompatible: true,
        ..Ended::default()
    };
    sim.end(s, None, &other, Duration::from_millis(50));
    assert_eq!(sim.s.search().update, Some((true, "0.3.0".into())));
    sim.after(REFUSED_PAUSE - MS);
    assert!(sim.cmds().is_empty());
    sim.after(MS);
    let (s, _) = sim.step();
    sim.reach(s, 1, usb());
    assert_eq!(
        sim.s.search().update,
        None,
        "a session opened: nothing to update"
    );
}

/// An unknown PC is offered to the user, and nothing connects until they answer;
/// "Pair" dials it again with its key, for as long as the answer holds.
#[test]
fn an_unknown_pc_waits_for_the_users_answer() {
    let mut sim = Sim::new(MAX_PCS);
    sim.s.hosts(vec![FoundHost {
        name: "Desk".into(),
        addr: addr(7),
    }]);
    sim.after(MS);
    let (s, _) = sim.step();
    sim.s.dialed(s, usb());
    sim.s.offered(key(5), usb(), OfferWhy::New, &sim.texts);
    sim.end(s, None, &Ended::default(), Duration::from_millis(80));
    assert!(matches!(
        &sim.s.search().attempt,
        Some(Attempt::PairOffer { label, why: OfferWhy::New, .. }) if label == "PC (USB)"
    ));
    sim.after(Duration::from_secs(30));
    assert!(sim.cmds().is_empty(), "waiting for the answer");
    sim.s.answer_offer(true, sim.now);
    sim.after(Duration::ZERO);
    let (s, cmd) = sim.step();
    let NetCmd::Dial { to, pair, .. } = cmd else {
        unreachable!()
    };
    assert_eq!((to, pair), (Dest::Usb, Some(key(5))));
    // Pairing, with the PC showing the code: swapped, it starts over.
    sim.s.dialed(s, usb());
    sim.s.pairing(s, key(5), usb(), None, &sim.texts);
    sim.s.pairing_mut().unwrap().typed.push('1');
    sim.s.switch_pairing_side();
    assert_eq!(sim.cmds(), [NetCmd::Hangup]);
    sim.end(s, None, &Ended::default(), Duration::from_secs(5));
    sim.after(RETRY_PAUSE);
    let (s, cmd) = sim.step();
    assert!(matches!(
        cmd,
        NetCmd::Dial {
            pc_shows_code: true,
            pair: Some(_),
            ..
        }
    ));
    sim.s.dialed(s, usb());
    sim.s
        .pairing(s, key(5), usb(), Some("123 456".into()), &sim.texts);
    sim.s.pairing_failed();
    sim.s
        .pairing(s, key(5), usb(), Some("654 321".into()), &sim.texts);
    assert!(
        sim.s.pairing_mut().unwrap().failed,
        "the last codes did not match"
    );
    sim.reach(s, 5, usb());
    assert!(sim.s.search().attempt.is_none());
    assert!(sim.s.connected(pc(5)));
    // Later, the answer no longer holds: an unknown PC is offered again.
    sim.end(s, Some(5), &Ended::default(), Duration::from_secs(1));
    sim.after(APPROVAL);
    let (_, cmd) = sim.step();
    assert!(matches!(cmd, NetCmd::Dial { pair: None, .. }));
    // "Not now": wait for the user to pick a PC, listening only.
    let mut sim = Sim::new(MAX_PCS);
    sim.after(MS);
    let (s, _) = sim.step();
    sim.s.dialed(s, usb());
    sim.s
        .offered(key(5), usb(), OfferWhy::Forgotten, &sim.texts);
    sim.end(s, None, &Ended::default(), MS);
    sim.s.answer_offer(false, sim.now);
    sim.after(RETRY_PAUSE);
    let (_, cmd) = sim.step();
    assert!(matches!(cmd, NetCmd::Listen { .. }), "{cmd:?}");
}

/// The user's picks: another PC is tried first and the one in use left; "Change PC"
/// leaves it and only listens until the user picks one; forgetting the PC in use
/// leaves it and does not come back.
#[test]
fn the_users_picks_steer_the_search() {
    let mut sim = Sim::new(MAX_PCS);
    sim.after(MS);
    let (s, _) = sim.step();
    sim.reach(s, 1, usb());
    let _ = sim.cmds();
    sim.s.connect_to(addr(7), sim.now);
    assert_eq!(sim.cmds(), [NetCmd::Leave(pc(1), HeadsetGoodbye::OtherPc)]);
    sim.end(s, Some(1), &Ended::default(), Duration::from_secs(9));
    sim.after(RETRY_PAUSE);
    // Not USB: the choice is on the network.
    let (s, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::Addr(addr(7)));
    sim.missed(s);
    let (s, cmd) = sim.step();
    assert!(matches!(cmd, NetCmd::Listen { .. }));
    sim.missed(s);
    assert!(
        sim.cmds().is_empty(),
        "its choice unreachable: only listening"
    );
    // The choice answers this time.
    sim.after(RETRY_PAUSE);
    let (s, _) = sim.step();
    sim.reach(s, 2, addr(7));
    let _ = sim.cmds();
    sim.s.change_pc();
    assert_eq!(sim.cmds(), [NetCmd::Leave(pc(2), HeadsetGoodbye::OtherPc)]);
    sim.end(s, Some(2), &Ended::default(), Duration::from_secs(9));
    sim.after(RETRY_PAUSE);
    let (s, cmd) = sim.step();
    assert!(matches!(cmd, NetCmd::Listen { .. }), "picking: no USB");
    sim.missed(s);
    sim.s.connect_to(usb(), sim.now);
    sim.after(Duration::ZERO);
    let (s, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::Usb, "a pick of USB goes first");
    sim.reach(s, 1, usb());
    let _ = sim.cmds();
    sim.s.forget(pc(1));
    assert_eq!(sim.cmds(), [NetCmd::Forget(pc(1))]);
    sim.end(s, Some(1), &Ended::default(), Duration::from_secs(9));
    sim.after(RETRY_PAUSE);
    let (_, cmd) = sim.step();
    assert!(
        matches!(cmd, NetCmd::Listen { .. }),
        "forgotten: not back on its own"
    );
}

/// Taken off, the search does not connect (the PC can give its desktop back to the
/// laptop); a session in progress goes on.
#[test]
fn off_the_head_nothing_connects() {
    let mut sim = Sim::new(MAX_PCS);
    sim.s.set_worn(false);
    sim.after(Duration::from_secs(5));
    assert!(sim.cmds().is_empty());
    sim.s.set_worn(true);
    sim.after(MS);
    let (s, _) = sim.step();
    sim.s.set_worn(false);
    // The step under way goes on; its end does not start another.
    sim.missed(s);
    sim.after(Duration::from_secs(5));
    assert!(sim.cmds().is_empty());
}

/// The menu's requests belong to the PC asked: without one nothing is sent; a
/// session that ends gives back what waited.
#[test]
fn requests_belong_to_their_pc() {
    let mut sim = Sim::new(MAX_PCS);
    let now = sim.now;
    assert_eq!(sim.s.ask(None, Asked::AddMonitor, now), None);
    assert_eq!(
        sim.s.ask(Some(pc(1)), Asked::AddMonitor, now),
        None,
        "not connected"
    );
    sim.after(MS);
    let (s, _) = sim.step();
    sim.reach(s, 1, usb());
    let req = sim.s.ask(Some(pc(1)), Asked::AddMonitor, sim.now).unwrap();
    assert_eq!(req, ReqId(1));
    assert!(sim.s.waits(Asked::AddMonitor));
    // Another PC's answer is not this one's.
    let answered = sim.s.answered(pc(2), req, &Outcome::Done, sim.now);
    assert_eq!(answered.what, None);
    let failed = Outcome::failed(ErrorCode::LIMIT, "");
    let answered = sim.s.answered(pc(1), req, &failed, sim.now);
    assert_eq!(answered.what, Some(Asked::AddMonitor));
    let req = sim.s.ask(Some(pc(1)), Asked::Screenshot, sim.now).unwrap();
    assert_eq!(req, ReqId(2));
    let ended = sim.end(s, Some(1), &Ended::default(), Duration::from_secs(9));
    assert_eq!(ended.waited, [(req, Asked::Screenshot)]);
    assert!(!sim.s.waits(Asked::Screenshot));
}

/// Scenario 16 (step 7 note, 10.1): a request waits while no frame is drawn (the
/// headset off the head): the passes of the main loop without frames time it out.
#[test]
fn requests_time_out_without_frames() {
    let mut sim = Sim::new(MAX_PCS);
    sim.after(MS);
    let (s, _) = sim.step();
    sim.reach(s, 1, usb());
    let req = sim.s.ask(Some(pc(1)), Asked::Encoding, sim.now).unwrap();
    sim.s.set_worn(false);
    let mut late = Vec::new();
    for _ in 0..300 {
        sim.after(Duration::from_millis(100));
        late.extend(sim.s.expire(sim.now));
    }
    assert_eq!(late, [(pc(1), req, Asked::Encoding)]);
}

/// Scenario 14 (step 7 note, 10.1), with two PCs allowed: each link has its own state
/// and requests; one ending leaves the other as it is; the sound and the microphone go
/// with one of them. With one allowed, a second PC is not reached while the first is
/// connected.
#[test]
fn two_links_keep_apart() {
    let mut sim = Sim::new(2);
    sim.after(MS);
    let (s1, _) = sim.step();
    sim.reach(s1, 1, usb());
    assert_eq!(sim.cmds(), [NetCmd::MediaTo(Some(pc(1)))]);
    // The search goes on for a second PC.
    sim.after(RETRY_PAUSE);
    let (s, _) = sim.step();
    sim.missed(s);
    let (s2, _) = sim.step();
    assert_ne!(s1, s2);
    sim.reach(s2, 2, addr(7));
    assert!(sim.cmds().is_empty(), "the media stay with the first");
    assert_eq!(sim.s.media_to(), Some(pc(1)));
    sim.after(Duration::from_secs(10));
    assert!(sim.cmds().is_empty(), "two links: no more search");
    let displays = prostir_proto::state::Displays {
        primary: Some(prostir_proto::state::DisplayId(3)),
        ..Default::default()
    };
    sim.s
        .part(pc(2), prostir_proto::HostMsg::Displays { part: displays })
        .unwrap();
    assert!(sim.s.pc(pc(1)).unwrap().state.displays.is_none());
    assert!(sim.s.pc(pc(2)).unwrap().state.displays.is_some());
    // Numbers are each PC's own.
    let a = sim.s.ask(Some(pc(1)), Asked::Sound, sim.now).unwrap();
    let b = sim.s.ask(Some(pc(2)), Asked::Sound, sim.now).unwrap();
    assert_eq!((a, b), (ReqId(1), ReqId(1)));
    // The first one ends: the second goes on, and gets the media.
    let ended = sim.end(s1, Some(1), &Ended::default(), Duration::from_secs(20));
    assert!(!ended.offline);
    assert_eq!(ended.waited, [(a, Asked::Sound)]);
    assert!(sim.s.waits(Asked::Sound), "the second still waits");
    assert_eq!(sim.s.main(), Some(pc(2)));
    assert_eq!(sim.cmds(), [NetCmd::MediaTo(Some(pc(2)))]);
    // One allowed: the second PC is never dialed while the first is connected.
    let mut sim = Sim::new(1);
    sim.after(MS);
    let (s, _) = sim.step();
    sim.reach(s, 1, usb());
    sim.s.hosts(vec![FoundHost {
        name: "Other".into(),
        addr: addr(8),
    }]);
    let _ = sim.cmds();
    sim.after(Duration::from_secs(30));
    assert!(sim.cmds().is_empty(), "the other PC only listed");
    assert_eq!(sim.s.search().found.len(), 1);
}
