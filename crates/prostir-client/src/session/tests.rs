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
            NetCmd::Dial { step, .. } | NetCmd::Ask { step, .. } => *step,
            other => panic!("not a step: {other:?}"),
        };
        (step, cmd)
    }

    /// The headset trusts the PCs `ns`, as the network thread says.
    fn paired(&mut self, ns: &[u8]) {
        let list = ns
            .iter()
            .map(|n| PairedPc {
                pc: pc(*n),
                name: format!("PC {n}"),
            })
            .collect();
        self.s.paired_list(list);
    }

    /// An answer to the query under way: of trusted PC `n`, or (`None`) of one that
    /// answers everyone.
    fn answer(&mut self, n: Option<u8>, at: SocketAddr) {
        let host = found(
            n.map_or_else(|| "Desk".into(), |n| format!("PC {n}")),
            at,
            n.map(pc),
            self.now,
        );
        self.s.heard(host, self.now, &self.texts);
    }

    /// The step is a query, sent around (and to `also`); whether it carries the name.
    fn asked(&mut self, also: Option<SocketAddr>) -> (StepId, bool) {
        match self.step() {
            (s, NetCmd::Ask { to, pairing, .. }) => {
                assert_eq!(to, AskTo::Around { also });
                (s, pairing)
            }
            other => panic!("not a query: {other:?}"),
        }
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

/// An answer as the network thread tells it.
fn found(name: String, addr: SocketAddr, pc: Option<PcId>, heard: Instant) -> FoundHost {
    FoundHost {
        name,
        addr,
        pc,
        pairing: false,
        heard,
        at_last: false,
        label: String::new(),
        ip: String::new(),
        usb: false,
    }
}

/// Every step asked for meanwhile is missed, and so is each that follows: what was
/// asked for, in order.
fn miss_all(sim: &mut Sim) -> Vec<NetCmd> {
    let mut all = Vec::new();
    loop {
        let cmds = sim.cmds();
        if cmds.is_empty() {
            return all;
        }
        for cmd in cmds {
            if let NetCmd::Ask { step, .. } | NetCmd::Dial { step, .. } = &cmd {
                sim.missed(*step);
            }
            all.push(cmd);
        }
    }
}

fn dest(cmd: &NetCmd) -> Dest {
    match cmd {
        NetCmd::Dial { to, .. } => *to,
        other => panic!("not a dial: {other:?}"),
    }
}

/// A round goes: USB, a query, the typed address, the trusted PC's last address; then
/// a pause until the next query is due (a second at first). The last address goes
/// once in ten seconds at most. A late word about an older step changes nothing.
#[test]
fn a_round_tries_each_way_in_turn() {
    let mut sim = Sim::new(MAX_PCS);
    sim.paired(&[1]);
    sim.after(MS);
    let (s, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::Usb);
    sim.missed(s);
    let (s, pairing) = sim.asked(None);
    assert!(!pairing, "a trusted PC: the query carries no name");
    sim.missed(s);
    let (s, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::HostFile);
    sim.missed(s);
    let (s, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::LastOf(pc(1)), "its last address, at once");
    sim.missed(s);
    assert!(sim.cmds().is_empty(), "a pause");
    sim.after(ASK_FAST - Duration::from_millis(2));
    assert!(sim.cmds().is_empty());
    sim.after(Duration::from_millis(2));
    let (s, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::Usb);
    sim.missed(StepId(1));
    assert!(sim.cmds().is_empty());
    sim.missed(s);
    let (s, _) = sim.asked(None);
    sim.missed(s);
    let (s, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::HostFile);
    sim.missed(s);
    assert!(
        sim.cmds().is_empty(),
        "the last address not again within 10 s"
    );
    // Ten seconds on, the last address again.
    let mut last = 0;
    for _ in 0..10 {
        sim.after(ASK_FAST);
        last += miss_all(&mut sim)
            .iter()
            .filter(|c| {
                matches!(
                    c,
                    NetCmd::Dial {
                        to: Dest::LastOf(_),
                        ..
                    }
                )
            })
            .count();
    }
    assert_eq!(last, 1, "once in ten seconds");
}

/// A trusted PC's answer leads to its connection at once, while the query still waits
/// for more; the one PC that answers everyone is dialed at the end of the round, and
/// refusing the connection in two rounds in a row, it is most likely behind a firewall.
#[test]
fn answers_lead_to_the_pc() {
    let mut sim = Sim::new(MAX_PCS);
    sim.paired(&[1]);
    sim.after(MS);
    let (s, _) = sim.step();
    sim.missed(s);
    let (_, _) = sim.asked(None);
    sim.answer(Some(1), addr(5));
    let (s, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::Addr(addr(5)), "at once");
    sim.reach(s, 1, addr(5));
    assert_eq!(sim.s.host(), Some("PC 1"), "named as it answered");
    let _ = sim.cmds();
    // A session: no queries at all, however long.
    for _ in 0..60 {
        sim.after(Duration::from_secs(1));
        assert!(sim.cmds().is_empty(), "nothing asked during a session");
    }
    // Unpaired, one PC answers everyone: it is dialed after the typed address.
    let mut sim = Sim::new(MAX_PCS);
    sim.after(MS);
    let (s, _) = sim.step();
    sim.missed(s);
    let (s, pairing) = sim.asked(None);
    assert!(
        pairing,
        "no trusted PC: the query carries the headset's name"
    );
    sim.answer(None, addr(7));
    assert!(
        sim.cmds().is_empty(),
        "an unknown PC waits for the end of the query"
    );
    sim.missed(s);
    let (s, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::HostFile);
    sim.missed(s);
    let (s, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::Addr(addr(7)));
    sim.missed(s);
    assert_eq!(
        sim.s.search().unreachable,
        None,
        "once may be a connection slower than a dial waits"
    );
    // The next round the same.
    sim.after(ASK_FAST);
    let (s, _) = sim.step();
    sim.missed(s);
    let (s, _) = sim.asked(None);
    sim.answer(None, addr(7));
    sim.missed(s);
    let (s, _) = sim.step();
    sim.missed(s);
    let (s, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::Addr(addr(7)));
    sim.missed(s);
    assert_eq!(
        sim.s.search().unreachable.as_deref(),
        Some("Desk"),
        "it answers but refuses again: a firewall"
    );
    // Any connection that opens takes it back.
    sim.s.dialed(StepId(999), addr(7));
    assert_eq!(sim.s.search().unreachable, None);
    // Two of them: none is picked for the user.
    sim.after(ASK_FAST);
    let (s, _) = sim.step();
    sim.missed(s);
    let (s, _) = sim.asked(None);
    sim.answer(None, addr(7));
    sim.answer(None, addr(8));
    sim.missed(s);
    let (s, _) = sim.step();
    sim.missed(s);
    assert!(sim.cmds().is_empty(), "two PCs: the user picks");
    assert_eq!(sim.s.search().found.len(), 2);
    // Answers go from the list once not heard for a while.
    sim.after(HEARD_WITHIN);
    assert!(sim.s.search().found.is_empty());
}

/// Once a second for the first 30 s of a search, then every 3 s.
#[test]
fn queries_go_less_often_after_a_while() {
    let mut sim = Sim::new(MAX_PCS);
    sim.paired(&[1]);
    let mut asked = Vec::new();
    let start = sim.now;
    for _ in 0..600 {
        sim.after(Duration::from_millis(100));
        let at = (sim.now - start).as_secs_f32().round() as u32;
        for cmd in miss_all(&mut sim) {
            if matches!(cmd, NetCmd::Ask { .. }) {
                asked.push(at);
            }
        }
    }
    let early: Vec<u32> = asked.iter().copied().filter(|t| *t <= 30).collect();
    assert!(early.len() >= 29, "about once a second: {early:?}");
    let late: Vec<u32> = asked.iter().copied().filter(|t| *t > 33).collect();
    assert!(
        late.windows(2).all(|w| w[1] - w[0] == 3),
        "every 3 s: {late:?}"
    );
}

/// The hints: without a trusted PC, five seconds without an answer say how to make
/// the PC answer, until one answers; with one, thirty seconds of its silence say it
/// does not answer, until it does.
#[test]
fn the_search_says_why_nothing_comes() {
    let mut sim = Sim::new(MAX_PCS);
    let answer_all = |sim: &mut Sim| {
        miss_all(sim);
    };
    sim.after(MS);
    for _ in 0..48 {
        answer_all(&mut sim);
        sim.after(Duration::from_millis(100));
    }
    assert_eq!(sim.s.search().hint, None, "not yet");
    for _ in 0..3 {
        answer_all(&mut sim);
        sim.after(Duration::from_millis(100));
    }
    assert_eq!(sim.s.search().hint, Some(Hint::NewPc));
    sim.answer(None, addr(7));
    sim.after(MS);
    assert_eq!(sim.s.search().hint, None, "a PC answered");
    // Trusted: silent for 30 s.
    let mut sim = Sim::new(MAX_PCS);
    sim.paired(&[1]);
    sim.after(MS);
    for _ in 0..290 {
        answer_all(&mut sim);
        sim.after(Duration::from_millis(100));
    }
    assert_eq!(
        sim.s.search().hint,
        None,
        "not yet, and never the one for a new PC"
    );
    for _ in 0..20 {
        answer_all(&mut sim);
        sim.after(Duration::from_millis(100));
    }
    assert_eq!(sim.s.search().hint, Some(Hint::Silent));
    sim.answer(Some(1), addr(5));
    sim.after(MS);
    assert_eq!(sim.s.search().hint, None, "it answered");
    // "Change PC" with a trusted PC: the hint for a new one.
    let mut sim = Sim::new(MAX_PCS);
    sim.paired(&[1]);
    sim.s.change_pc();
    sim.after(MS);
    for _ in 0..60 {
        answer_all(&mut sim);
        sim.after(Duration::from_millis(100));
    }
    assert_eq!(sim.s.search().hint, Some(Hint::NewPc));
}

/// The selftest's fake PC is the only one asked and dialed: asked first, dialed
/// whether it answered or not, and again at once when it answers.
#[test]
fn the_selftest_asks_its_fake_pc_alone() {
    let now = Instant::now();
    let mut s = Session::new(MAX_PCS, Some(addr(9)), now);
    s.tick(now);
    let cmds: Vec<NetCmd> = s.take_cmds().collect();
    let [NetCmd::Ask { step, to, .. }] = cmds.as_slice() else {
        panic!("{cmds:?}");
    };
    assert_eq!(*to, AskTo::Exactly(addr(9)));
    s.missed(*step, now);
    let cmds: Vec<NetCmd> = s.take_cmds().collect();
    let [NetCmd::Dial { step, to, .. }] = cmds.as_slice() else {
        panic!("{cmds:?}");
    };
    assert_eq!(
        *to,
        Dest::Addr(addr(9)),
        "not answered: dialed all the same"
    );
    s.missed(*step, now);
    assert_eq!(s.take_cmds().count(), 0);
    let later = now + ASK_FAST;
    s.tick(later);
    let cmds: Vec<NetCmd> = s.take_cmds().collect();
    let [NetCmd::Ask { .. }] = cmds.as_slice() else {
        panic!("{cmds:?}");
    };
    s.heard(
        found("Prostir selftest".into(), addr(9), Some(pc(1)), later),
        later,
        &lang::builtin("en"),
    );
    let cmds: Vec<NetCmd> = s.take_cmds().collect();
    assert!(
        matches!(cmds.as_slice(), [NetCmd::Dial { to: Dest::Addr(a), .. }] if *a == addr(9)),
        "{cmds:?}"
    );
    assert_eq!(s.search().hint, None, "no hint in the selftest");
    // Against a real PC over USB: never asked, dialed straight away.
    let mut s = Session::new(MAX_PCS, Some(usb()), now);
    s.tick(now);
    let cmds: Vec<NetCmd> = s.take_cmds().collect();
    assert!(
        matches!(cmds.as_slice(), [NetCmd::Dial { to: Dest::Addr(a), .. }] if *a == usb()),
        "{cmds:?}"
    );
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
    sim.s.switch_pairing_side(sim.now);
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
    // "Not now": wait for the user to pick a PC, only asking (to list the PCs).
    let mut sim = Sim::new(MAX_PCS);
    sim.after(MS);
    let (s, _) = sim.step();
    sim.s.dialed(s, usb());
    sim.s
        .offered(key(5), usb(), OfferWhy::Forgotten, &sim.texts);
    sim.end(s, None, &Ended::default(), MS);
    sim.s.answer_offer(false, sim.now);
    sim.after(RETRY_PAUSE);
    let (s, pairing) = sim.asked(None);
    assert!(pairing);
    sim.missed(s);
    assert!(sim.cmds().is_empty(), "nothing dialed");
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
    // Asked straight too (a VPN address gets no broadcast).
    let (s, _) = sim.asked(Some(addr(7)));
    sim.missed(s);
    assert!(sim.cmds().is_empty(), "its choice unreachable: only asking");
    // The choice answers this time.
    sim.after(ASK_FAST);
    let (s, _) = sim.step();
    sim.reach(s, 2, addr(7));
    let _ = sim.cmds();
    sim.s.change_pc();
    assert_eq!(sim.cmds(), [NetCmd::Leave(pc(2), HeadsetGoodbye::OtherPc)]);
    sim.end(s, Some(2), &Ended::default(), Duration::from_secs(9));
    sim.after(ASK_FAST);
    // A cable is a choice of its own: tried first, as at the start (nothing there).
    let (s, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::Usb);
    assert_eq!(takes(&cmd), Takes::AllBut(Vec::new()));
    sim.missed(s);
    let (s, pairing) = sim.asked(None);
    assert!(pairing, "picking another PC: the query carries the name");
    sim.missed(s);
    assert!(
        sim.cmds().is_empty(),
        "picking: nothing dialed on the network"
    );
    sim.s.connect_to(usb(), sim.now);
    sim.after(Duration::ZERO);
    let (s, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::Usb, "a pick of USB goes first");
    sim.reach(s, 1, usb());
    let _ = sim.cmds();
    sim.s.forget(pc(1));
    assert_eq!(sim.cmds(), [NetCmd::Forget(pc(1))]);
    sim.end(s, Some(1), &Ended::default(), Duration::from_secs(9));
    sim.after(ASK_FAST);
    // USB still, but never for the PC just forgotten (it would ask to pair again).
    let (s, cmd) = sim.step();
    assert_eq!(takes(&cmd), Takes::AllBut(vec![pc(1)]));
    sim.missed(s);
    let (s, _) = sim.asked(None);
    // Even should it still answer (its answer has no trusted tag now): not dialed.
    sim.answer(None, usb());
    sim.missed(s);
    assert!(sim.cmds().is_empty(), "forgotten: not back on its own");
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
    // The step under way goes on; its end does not start another, and nothing is
    // asked.
    sim.missed(s);
    sim.after(Duration::from_secs(5));
    assert!(sim.cmds().is_empty());
    assert_eq!(sim.s.search().hint, None, "no hint off the head");
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
    sim.paired(&[1, 2]);
    sim.after(MS);
    let (s1, _) = sim.step();
    sim.reach(s1, 1, usb());
    assert_eq!(sim.cmds(), [NetCmd::MediaTo(Some(pc(1)))]);
    // The search goes on for a second PC: it answers.
    sim.after(RETRY_PAUSE);
    let (s, _) = sim.step();
    sim.missed(s);
    let (_, _) = sim.asked(None);
    sim.answer(Some(2), addr(7));
    let (s2, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::Addr(addr(7)));
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
    sim.answer(Some(2), addr(8));
    let _ = sim.cmds();
    sim.after(Duration::from_secs(1));
    assert!(sim.cmds().is_empty(), "the other PC only listed");
    assert_eq!(sim.s.search().found.len(), 1);
    sim.after(Duration::from_secs(30));
    assert!(sim.cmds().is_empty(), "and nothing asked");
}

/// The PC forgot the headset: it asks to pair again when the session opens, inside the
/// trusted link. Pairing then (with the user's approval), and the session after it
/// ends the attempt; a second dial of the same PC changes nothing of its link.
#[test]
fn pairing_again_inside_the_link() {
    let mut sim = Sim::new(MAX_PCS);
    sim.after(MS);
    let (s, _) = sim.step();
    sim.s.dialed(s, usb());
    sim.s.reached(s, pc(1), usb(), sim.now);
    // Asked to pair: the user is asked, the link ends without a session.
    sim.s.offered(key(1), usb(), OfferWhy::New, &sim.texts);
    let ended = sim.end(s, Some(1), &Ended::default(), Duration::from_millis(50));
    assert!(!ended.offline, "no session was had");
    sim.s.answer_offer(true, sim.now);
    sim.after(Duration::ZERO);
    let (s, cmd) = sim.step();
    assert!(matches!(cmd, NetCmd::Dial { pair: Some(k), .. } if k == key(1)));
    sim.s.dialed(s, usb());
    sim.s.reached(s, pc(1), usb(), sim.now);
    sim.s.pairing(s, key(1), usb(), None, &sim.texts);
    assert!(sim.s.pairing_mut().is_some());
    // The same PC again, by another way: its link stays as it is.
    sim.s.reached(StepId(99), pc(1), addr(7), sim.now);
    assert!(matches!(sim.s.links()[&pc(1)], Link::Connecting { addr: a } if a == usb()));
    sim.s.connected_to(pc(1), usb(), &sim.texts);
    assert!(sim.s.search().attempt.is_none(), "the pairing is over");
    assert!(sim.s.connected(pc(1)));
}

/// With a trusted PC, one that answers everyone is listed but not dialed on its own;
/// "Not now" to an unknown PC leaves that one alone, and the trusted PC is still
/// connected to by itself.
#[test]
fn trusted_pcs_go_on_after_not_now() {
    let mut sim = Sim::new(MAX_PCS);
    sim.paired(&[1]);
    sim.after(MS);
    let (s, _) = sim.step();
    sim.missed(s);
    let (s, _) = sim.asked(None);
    sim.answer(None, addr(7));
    sim.missed(s);
    for cmd in miss_all(&mut sim) {
        assert!(
            !matches!(cmd, NetCmd::Dial { to: Dest::Addr(a), .. } if a == addr(7)),
            "not dialed: a trusted PC is waited for"
        );
    }
    assert_eq!(sim.s.search().found.len(), 1, "listed");
    // An unknown PC over USB: offered, and "Not now".
    sim.after(ASK_FAST);
    let (s, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::Usb);
    sim.s.dialed(s, usb());
    sim.s.offered(key(5), usb(), OfferWhy::New, &sim.texts);
    sim.end(s, None, &Ended::default(), MS);
    sim.s.answer_offer(false, sim.now);
    sim.after(ASK_FAST);
    // USB is not dialed again; the query goes, and the trusted PC's answer is taken.
    let (_, pairing) = sim.asked(None);
    assert!(!pairing, "not looking for a new PC");
    sim.answer(Some(1), addr(5));
    let (_, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::Addr(addr(5)));
}

/// A PC that answers again keeps its place in the list (a press picks the PC shown),
/// and the list says what it shows once, when the answer comes; the time alone
/// changing the list or the hint is told.
#[test]
fn the_list_keeps_its_order() {
    let mut sim = Sim::new(MAX_PCS);
    sim.after(MS);
    let mut pairing = found("Desk".into(), addr(7), None, sim.now);
    pairing.pairing = true;
    sim.s.heard(pairing, sim.now, &sim.texts);
    sim.answer(None, addr(8));
    sim.now += Duration::from_secs(2);
    sim.answer(None, addr(7));
    let listed: Vec<_> = sim
        .s
        .search()
        .found
        .iter()
        .map(|h| (h.addr, h.label.as_str(), h.ip.as_str()))
        .collect();
    assert_eq!(
        listed,
        [
            (addr(7), "Desk", "192.168.1.7"),
            (addr(8), "Desk", "192.168.1.8")
        ]
    );
    let mut open = found("Desk".into(), addr(8), None, sim.now);
    open.pairing = true;
    sim.s.heard(open, sim.now, &sim.texts);
    assert_eq!(sim.s.search().found[1].label, "Desk · pairing open");
    // While the answers are fresh, nothing changes with the time.
    assert!(!sim.s.tick(sim.now), "nothing changed");
    sim.now += HEARD_WITHIN;
    assert!(sim.s.tick(sim.now), "the answers went: the menu changes");
    assert!(sim.s.search().found.is_empty());
}

/// A decline is the PC's, not its address's: a trusted PC's answer from the address of
/// a PC declined is taken; the declined PC offered again is let go without asking.
/// A trusted PC's answer from its last address stays before a copy from elsewhere.
#[test]
fn a_decline_is_the_pcs_and_the_last_address_wins() {
    let mut sim = Sim::new(MAX_PCS);
    sim.paired(&[1]);
    sim.after(MS);
    let (s, _) = sim.step();
    sim.s.dialed(s, usb());
    sim.s.offered(key(5), usb(), OfferWhy::New, &sim.texts);
    sim.end(s, None, &Ended::default(), MS);
    sim.s.answer_offer(false, sim.now);
    // The declined PC comes again (another way): let go, nothing asked.
    sim.s.offered(key(5), addr(9), OfferWhy::New, &sim.texts);
    assert!(sim.s.search().attempt.is_none());
    assert_eq!(sim.cmds(), [NetCmd::Hangup]);
    // The trusted PC answers.
    sim.after(ASK_FAST);
    let (_, _) = sim.asked(None);
    let mut at_last = found("PC 1".into(), addr(5), Some(pc(1)), sim.now);
    at_last.at_last = true;
    // First a copy of its answer from elsewhere, then from its last address.
    sim.s.heard(
        found("PC 1".into(), addr(6), Some(pc(1)), sim.now),
        sim.now,
        &sim.texts,
    );
    let (_, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::Addr(addr(6)), "the only one so far");
    sim.s.heard(at_last, sim.now, &sim.texts);
    sim.s.heard(
        found("PC 1".into(), addr(6), Some(pc(1)), sim.now),
        sim.now,
        &sim.texts,
    );
    let listed: Vec<_> = sim.s.search().found.iter().map(|h| h.addr).collect();
    assert_eq!(listed, [addr(5)], "its last address kept");
    let mut sim = Sim::new(MAX_PCS);
    sim.paired(&[1]);
    sim.after(MS);
    let (s, _) = sim.step();
    sim.s.dialed(s, usb());
    sim.s.offered(key(5), usb(), OfferWhy::New, &sim.texts);
    sim.end(s, None, &Ended::default(), MS);
    sim.s.answer_offer(false, sim.now);
    sim.after(ASK_FAST);
    let (_, _) = sim.asked(None);
    sim.answer(Some(1), usb());
    let (_, cmd) = sim.step();
    assert_eq!(
        dest(&cmd),
        Dest::Addr(usb()),
        "a trusted PC at the declined address"
    );
}

/// For `secs` seconds of rounds, each query answered by trusted PC `n` from `at` and
/// every step missed: whether `at` (or the PC's last address) was dialed, and whether
/// any dial was to pair.
fn dials_its_answer(sim: &mut Sim, n: u8, at: SocketAddr, secs: u32) -> (bool, bool) {
    let (mut dialed, mut pairs) = (false, false);
    for _ in 0..secs {
        sim.after(Duration::from_secs(1));
        loop {
            let cmds = sim.cmds();
            if cmds.is_empty() {
                break;
            }
            for cmd in cmds {
                match cmd {
                    NetCmd::Ask { step, .. } => {
                        sim.answer(Some(n), at);
                        sim.missed(step);
                    }
                    NetCmd::Dial { step, to, pair, .. } => {
                        dialed |= to == Dest::Addr(at) || to == Dest::LastOf(pc(n));
                        pairs |= pair.is_some();
                        sim.missed(step);
                    }
                    _ => {}
                }
            }
        }
    }
    (dialed, pairs)
}

/// A pick is for one connection: paired over USB as the user agreed, once that
/// session is over (the cable pulled) the PC's answers over Wi-Fi are dialed, and no
/// dial pairs on the old agreement.
#[test]
fn after_pairing_over_usb_the_pc_is_found_on_wifi() {
    let mut sim = Sim::new(MAX_PCS);
    sim.after(MS);
    let (s, _) = sim.step();
    sim.s.dialed(s, usb());
    sim.s.offered(key(5), usb(), OfferWhy::New, &sim.texts);
    sim.end(s, None, &Ended::default(), Duration::from_millis(80));
    sim.s.answer_offer(true, sim.now);
    sim.after(Duration::ZERO);
    let (s, _) = sim.step();
    sim.s.dialed(s, usb());
    sim.s
        .pairing(s, key(5), usb(), Some("123 456".into()), &sim.texts);
    sim.paired(&[5]);
    sim.reach(s, 5, usb());
    let _ = sim.cmds();
    let opened = Ended {
        opened: true,
        ..Ended::default()
    };
    sim.end(s, Some(5), &opened, Duration::from_secs(60));
    let (dialed, pairs) = dials_its_answer(&mut sim, 5, addr(9), 5);
    assert!(dialed, "the trusted PC answering over Wi-Fi is dialed");
    assert!(!pairs, "the agreement to pair was for that connection");
}

/// A PC picked in the list after "Change PC" is the user's choice until its session
/// opens; when its address changes later (DHCP), its answer from the new one is
/// dialed.
#[test]
fn a_picked_pc_that_moves_is_found_again() {
    let mut sim = Sim::new(MAX_PCS);
    sim.paired(&[1]);
    sim.after(MS);
    let (s, _) = sim.step();
    sim.reach(s, 1, addr(7));
    let _ = sim.cmds();
    sim.s.change_pc();
    let _ = sim.cmds();
    sim.end(s, Some(1), &Ended::default(), Duration::from_secs(9));
    sim.s.connect_to(addr(7), sim.now);
    sim.after(RETRY_PAUSE);
    let (s, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::Addr(addr(7)), "the pick first");
    sim.reach(s, 1, addr(7));
    let _ = sim.cmds();
    let opened = Ended {
        opened: true,
        ..Ended::default()
    };
    sim.end(s, Some(1), &opened, Duration::from_secs(600));
    let (dialed, _) = dials_its_answer(&mut sim, 1, addr(8), 5);
    assert!(dialed, "the picked PC at its new address is dialed");
}

/// Forgetting the PC in use fails (its file cannot be written): the search goes on as
/// before, and once the session ends the PC is connected to again on its own.
#[test]
fn a_failed_forget_changes_nothing() {
    let mut sim = Sim::new(MAX_PCS);
    sim.paired(&[1]);
    sim.after(MS);
    let (s, _) = sim.step();
    sim.reach(s, 1, usb());
    let _ = sim.cmds();
    sim.s.forget(pc(1));
    assert_eq!(sim.cmds(), [NetCmd::Forget(pc(1))]);
    sim.s.forget_failed(pc(1));
    let broke = Ended {
        opened: true,
        ..Ended::default()
    };
    sim.end(s, Some(1), &broke, Duration::from_secs(60));
    sim.after(Duration::ZERO);
    let (_, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::Usb, "not picking: it goes its own way");
}

/// The PC refused to pair (no dialog for the code): "the other way round" on the
/// offer pairs again at once, with the other side showing the code.
#[test]
fn the_other_way_after_a_refusal_pairs_again() {
    let mut sim = Sim::new(MAX_PCS);
    sim.after(MS);
    let (s, _) = sim.step();
    sim.s.dialed(s, usb());
    let why = OfferWhy::PcRefused { pc_shows: false };
    sim.s.offered(key(5), usb(), why, &sim.texts);
    let refused = Ended {
        refused: Some(sim.now),
        ..Ended::default()
    };
    sim.end(s, None, &refused, MS);
    sim.after(Duration::from_secs(1));
    assert!(sim.cmds().is_empty(), "waiting for the user");
    sim.s.switch_pairing_side(sim.now);
    sim.after(Duration::ZERO);
    let (_, cmd) = sim.step();
    assert!(
        matches!(
            cmd,
            NetCmd::Dial {
                pair: Some(k),
                pc_shows_code: true,
                ..
            } if k == key(5)
        ),
        "{cmd:?}"
    );
}

/// A PC picked while another is in use over USB: once the pick's session opened and
/// ended, the search looks for the PC picked (its answers, its last address), never
/// USB first, nor the other PC's answers: the user's pick holds until the next.
#[test]
fn a_pick_holds_after_its_session() {
    let mut sim = Sim::new(MAX_PCS);
    sim.paired(&[1, 2]);
    sim.after(MS);
    let (s, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::Usb);
    sim.reach(s, 1, usb());
    let _ = sim.cmds();
    sim.s.connect_to(addr(7), sim.now);
    let _ = sim.cmds();
    sim.end(s, Some(1), &Ended::default(), Duration::from_secs(9));
    sim.after(RETRY_PAUSE);
    let (s, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::Addr(addr(7)));
    sim.reach(s, 2, addr(7));
    let _ = sim.cmds();
    let opened = Ended {
        opened: true,
        ..Ended::default()
    };
    sim.end(s, Some(2), &opened, Duration::from_secs(60));
    sim.after(Duration::ZERO);
    // USB first, for the PC picked alone: the other PC is there, and let go.
    let (s, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::Usb);
    assert_eq!(takes(&cmd), Takes::Only(vec![pc(2)]));
    sim.s.dialed(s, usb());
    sim.end(s, None, &let_go(1), MS);
    assert!(!sim.s.connected(pc(1)), "not taken instead");
    let (s, cmd) = sim.step();
    assert!(
        matches!(cmd, NetCmd::Ask { .. }),
        "the round goes on: {cmd:?}"
    );
    // The other PC answers: listed with the cable's mark, not dialed; the one picked is.
    sim.answer(Some(1), addr(5));
    assert!(sim.cmds().is_empty(), "the other PC is not taken instead");
    let marked = &sim.s.search().found[0];
    assert!(
        marked.usb && marked.ip == "192.168.1.5 · cable",
        "{marked:?}"
    );
    sim.answer(Some(2), addr(8));
    assert!(!sim.s.search().found[1].usb);
    let (d, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::Addr(addr(8)), "the PC picked, moved");
    sim.missed(d);
    sim.missed(s);
    // The PC let go is not knocked on over USB every second.
    sim.after(ASK_FAST);
    let cmds = miss_all(&mut sim);
    assert!(
        !cmds
            .iter()
            .any(|c| matches!(c, NetCmd::Dial { to: Dest::Usb, .. }))
    );
    sim.after(LAST_EVERY);
    let cmds = miss_all(&mut sim);
    assert!(
        cmds.iter()
            .any(|c| matches!(c, NetCmd::Dial { to: Dest::Usb, .. }))
    );
}

fn takes(cmd: &NetCmd) -> Takes {
    match cmd {
        NetCmd::Dial { takes, .. } => takes.clone(),
        other => panic!("not a dial: {other:?}"),
    }
}

/// How the network thread ends a dial whose PC (`n`) it was not to take.
fn let_go(n: u8) -> Ended {
    Ended {
        let_go: Some(pc(n)),
        ..Ended::default()
    }
}

/// A PC picked over Wi-Fi whose session ends while the cable reaches it: the next
/// dial is USB, and it is taken there (the steadier link); while its session runs
/// over Wi-Fi nothing is dialed at all, so it is never moved by itself. Another PC at
/// the cable's end is let go instead.
#[test]
fn a_picked_pc_comes_back_over_usb() {
    for at_cable in [1, 2] {
        let mut sim = Sim::new(MAX_PCS);
        sim.paired(&[1, 2]);
        sim.s.change_pc();
        sim.s.connect_to(addr(7), sim.now);
        sim.after(MS);
        let (s, cmd) = sim.step();
        assert_eq!(dest(&cmd), Dest::Addr(addr(7)));
        sim.reach(s, 1, addr(7));
        let _ = sim.cmds();
        sim.after(Duration::from_secs(30));
        assert!(
            sim.cmds().is_empty(),
            "a running Wi-Fi session stays as it is"
        );
        let opened = Ended {
            opened: true,
            ..Ended::default()
        };
        sim.end(s, Some(1), &opened, Duration::from_secs(60));
        sim.after(Duration::ZERO);
        let (s, cmd) = sim.step();
        assert_eq!(dest(&cmd), Dest::Usb, "USB first");
        assert_eq!(takes(&cmd), Takes::Only(vec![pc(1)]));
        sim.s.dialed(s, usb());
        if at_cable == 1 {
            sim.reach(s, 1, usb());
            assert!(sim.s.connected(pc(1)));
            assert!(matches!(&sim.s.links()[&pc(1)], Link::Connected(p) if p.addr == usb()));
        } else {
            sim.end(s, None, &let_go(2), MS);
            assert!(
                sim.s.links().get(&pc(2)).is_none(),
                "the other PC is not taken"
            );
            let (_, cmd) = sim.step();
            assert!(
                matches!(cmd, NetCmd::Ask { .. }),
                "the round goes on: {cmd:?}"
            );
        }
    }
}

/// Connected over Wi-Fi with the cable in: "Change PC" and no pick, and the next
/// round tries USB first, as a fresh start does, taking any PC there; from a session
/// over USB, not the PC just left there.
#[test]
fn change_pc_tries_usb_first() {
    let mut sim = Sim::new(MAX_PCS);
    sim.paired(&[1]);
    sim.after(MS);
    let (s, _) = sim.step();
    sim.missed(s);
    let (_, _) = sim.asked(None);
    sim.answer(Some(1), addr(7));
    let (s, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::Addr(addr(7)));
    sim.reach(s, 1, addr(7));
    let _ = sim.cmds();
    sim.s.change_pc();
    assert_eq!(sim.cmds(), [NetCmd::Leave(pc(1), HeadsetGoodbye::OtherPc)]);
    sim.end(s, Some(1), &Ended::default(), Duration::from_secs(9));
    sim.after(RETRY_PAUSE);
    let (s, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::Usb, "USB first after Change PC");
    assert_eq!(takes(&cmd), Takes::AllBut(Vec::new()));
    sim.reach(s, 1, usb());
    assert!(sim.s.connected(pc(1)), "now over the cable");
    let _ = sim.cmds();
    // Changing again from the cable: USB is still tried, but not for this PC.
    sim.s.change_pc();
    let _ = sim.cmds();
    sim.end(s, Some(1), &Ended::default(), Duration::from_secs(9));
    sim.after(RETRY_PAUSE);
    let (_, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::Usb);
    assert_eq!(takes(&cmd), Takes::AllBut(vec![pc(1)]));
}

/// A PC listed that the cable reaches is marked so, and picking it goes over USB,
/// taking that PC alone: here the PC "Change PC" left over the cable, which the USB
/// dial does not take back on its own.
#[test]
fn a_pc_at_the_cable_is_picked_over_usb() {
    let mut sim = Sim::new(MAX_PCS);
    sim.paired(&[1]);
    sim.after(MS);
    let (s, _) = sim.step();
    sim.reach(s, 1, usb());
    let _ = sim.cmds();
    sim.s.change_pc();
    let _ = sim.cmds();
    sim.end(s, Some(1), &Ended::default(), Duration::from_secs(9));
    sim.after(RETRY_PAUSE);
    let (s, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::Usb);
    assert_eq!(takes(&cmd), Takes::AllBut(vec![pc(1)]));
    sim.s.dialed(s, usb());
    sim.end(s, None, &let_go(1), MS);
    let (a, _) = sim.asked(None);
    sim.answer(Some(1), addr(9));
    sim.missed(a);
    assert!(sim.s.search().found[0].usb);
    assert!(sim.cmds().is_empty(), "picking: the user picks");
    sim.s.pick_found(0, sim.now);
    sim.after(Duration::ZERO);
    let (s, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::Usb, "picked at the cable: over USB");
    assert_eq!(takes(&cmd), Takes::Only(vec![pc(1)]));
    // The cable pulled: the mark goes.
    sim.missed(s);
    assert!(!sim.s.search().found[0].usb);
}

/// Two PCs allowed: PC 1 at the cable and PC 2 on Wi-Fi, both trusted and wanted,
/// both end up linked, PC 1 over USB.
#[test]
fn two_pcs_one_over_usb() {
    let mut sim = Sim::new(2);
    sim.paired(&[1, 2]);
    sim.after(MS);
    let (s1, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::Usb);
    assert_eq!(takes(&cmd), Takes::Any);
    sim.reach(s1, 1, usb());
    let _ = sim.cmds();
    sim.after(RETRY_PAUSE);
    // USB again: the network thread does not dial a busy address twice.
    let (s, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::Usb);
    sim.missed(s);
    let (_, _) = sim.asked(None);
    sim.answer(Some(2), addr(7));
    let (s2, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::Addr(addr(7)));
    sim.reach(s2, 2, addr(7));
    assert!(matches!(&sim.s.links()[&pc(1)], Link::Connected(p) if p.addr == usb()));
    assert!(matches!(&sim.s.links()[&pc(2)], Link::Connected(p) if p.addr == addr(7)));
    sim.after(Duration::from_secs(10));
    assert!(sim.cmds().is_empty(), "two links: no more search");
}

/// "Change PC" from a session over Wi-Fi, and the USB dial reaches the PC: the cable
/// counts as the user's choice, so once that session ends (the cable pulled) the
/// PC's answer over Wi-Fi is dialed, with no tap in the menu.
#[test]
fn after_change_pc_and_usb_the_pc_is_found_on_wifi() {
    let mut sim = Sim::new(MAX_PCS);
    sim.paired(&[1]);
    sim.after(MS);
    let (s, _) = sim.step();
    sim.missed(s);
    let (_, _) = sim.asked(None);
    sim.answer(Some(1), addr(7));
    let (s, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::Addr(addr(7)));
    sim.reach(s, 1, addr(7));
    let _ = sim.cmds();
    sim.s.change_pc();
    let _ = sim.cmds();
    sim.end(s, Some(1), &Ended::default(), Duration::from_secs(9));
    sim.after(RETRY_PAUSE);
    let (s, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::Usb);
    sim.reach(s, 1, usb());
    let _ = sim.cmds();
    let opened = Ended {
        opened: true,
        ..Ended::default()
    };
    sim.end(s, Some(1), &opened, Duration::from_secs(60));
    let (dialed, _) = dials_its_answer(&mut sim, 1, addr(7), 5);
    assert!(dialed, "the PC over Wi-Fi is dialed on its own");
}

/// A stale `adb reverse` (Prostir stopped on the PC, adbd still reversing): the USB
/// dial opens and ends at once with no PC. The round goes on to the query and the
/// last address instead of starting over at USB.
#[test]
fn a_stale_usb_reverse_leads_on_to_wifi() {
    let mut sim = Sim::new(MAX_PCS);
    sim.paired(&[1]);
    sim.after(MS);
    let (s, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::Usb);
    sim.s.dialed(s, usb());
    sim.end(s, None, &Ended::default(), MS);
    let (s, _) = sim.asked(None);
    sim.missed(s);
    let (s, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::HostFile);
    sim.missed(s);
    let (_, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::LastOf(pc(1)));
}

/// A PC picked in the list by a cable mark gone stale (the cable pulled since): the
/// USB dial takes that PC alone, and when nothing is at the cable the pick is dialed
/// over Wi-Fi, where it answered.
#[test]
fn a_pick_by_a_stale_cable_mark_falls_back_to_wifi() {
    let mut sim = Sim::new(MAX_PCS);
    sim.paired(&[1]);
    sim.after(MS);
    let (s, _) = sim.step();
    sim.reach(s, 1, usb());
    let _ = sim.cmds();
    sim.s.change_pc();
    let _ = sim.cmds();
    sim.end(s, Some(1), &Ended::default(), Duration::from_secs(9));
    sim.after(RETRY_PAUSE);
    let (s, _) = sim.step();
    sim.s.dialed(s, usb());
    sim.end(s, None, &let_go(1), MS);
    let (a, _) = sim.asked(None);
    sim.answer(Some(1), addr(9));
    sim.missed(a);
    assert!(sim.s.search().found[0].usb);
    sim.s.pick_found(0, sim.now);
    sim.after(Duration::ZERO);
    let (s, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::Usb);
    assert_eq!(takes(&cmd), Takes::Only(vec![pc(1)]));
    sim.missed(s);
    let (s, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::Addr(addr(9)), "the pick over Wi-Fi");
    assert_eq!(takes(&cmd), Takes::Only(vec![pc(1)]));
    sim.reach(s, 1, addr(9));
    assert!(sim.s.connected(pc(1)));
}

/// A USB link that ends takes the cable's mark with it: the PC may be on Wi-Fi only
/// by now, and the next USB dial says again who is at the cable.
#[test]
fn the_cable_mark_goes_with_the_usb_link() {
    let mut sim = Sim::new(MAX_PCS);
    sim.paired(&[1]);
    sim.after(MS);
    let (s, _) = sim.step();
    sim.reach(s, 1, usb());
    sim.answer(Some(1), addr(9));
    assert!(sim.s.search().found[0].usb);
    sim.end(s, Some(1), &Ended::default(), Duration::from_secs(9));
    assert!(!sim.s.search().found[0].usb);
}

/// The user picked USB: the round dials it once, at its USB step, and not again as
/// the pick.
#[test]
fn a_pick_of_usb_is_dialed_once_a_round() {
    let mut sim = Sim::new(MAX_PCS);
    sim.paired(&[1]);
    sim.s.connect_to(usb(), sim.now);
    sim.after(MS);
    let (s, cmd) = sim.step();
    assert_eq!(dest(&cmd), Dest::Usb);
    sim.missed(s);
    let (_, cmd) = sim.step();
    assert!(matches!(cmd, NetCmd::Ask { .. }), "not USB again: {cmd:?}");
}

/// A PC the headset does not trust at the cable's end (let go by a dial for the
/// picked PC alone) is not marked: the mark is for a trusted PC in the list.
#[test]
fn an_untrusted_pc_is_not_marked_at_the_cable() {
    let mut sim = Sim::new(MAX_PCS);
    sim.paired(&[1]);
    sim.s.connect_to(addr(7), sim.now);
    sim.after(MS);
    let (s, _) = sim.step();
    sim.reach(s, 1, addr(7));
    let _ = sim.cmds();
    let opened = Ended {
        opened: true,
        ..Ended::default()
    };
    sim.end(s, Some(1), &opened, Duration::from_secs(60));
    sim.after(Duration::ZERO);
    let (s, cmd) = sim.step();
    assert_eq!(takes(&cmd), Takes::Only(vec![pc(1)]));
    sim.s.dialed(s, usb());
    sim.end(s, None, &let_go(5), MS);
    assert_eq!(sim.s.search().usb_pc, None);
}
