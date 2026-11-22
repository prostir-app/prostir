//! The connections to PCs (step 7 note, 4.5): whom to reach, when and in which order,
//! how many at once ([`MAX_PCS`]), when to rest; the search before a PC is trusted, and
//! one link for each trusted PC with what belongs to it (its state, the menu's requests
//! to it). The network thread only does what this says ([`NetCmd`]) and tells what
//! happened; the files of the trusted PCs are its own, the list of them comes here as
//! [`Session::paired_list`].
//!
//! Before trust everything is in [`Search`], one for the headset: the PCs that answered
//! its queries, a PC that refuses the connection, the hint shown while nothing answers,
//! and one attempt at a time ([`Attempt`]: dialing an address, an unknown PC offered to
//! the user, pairing). After trust everything is in a [`Link`] by the PC's [`PcId`]:
//! connecting, connected (its [`Pc`]), or lost. Only links count towards [`MAX_PCS`].
//!
//! The search asks (step 7 note, 8.1): a query ([`NetCmd::Ask`], as a broadcast and
//! straight to the addresses the network thread keeps) once a second for the first
//! [`ASK_FAST_FOR`] of a search, then every [`ASK_EVERY`]; a trusted PC's answer leads to
//! its connection at once. A round goes USB, the PC picked in the menu, the query, a
//! trusted PC that answered, the typed address (`host.txt`), a trusted PC's last address
//! by TCP (at most every [`LAST_EVERY`] for each: an open port wakes a sleeping PC, and
//! this is how the first connection after an update, or with a find secret gone stale,
//! still works), and the one PC that answered everyone. Nothing is asked while the
//! headset is off the head or as many PCs as allowed are in a session, so a PC in use
//! and the other PCs around are left alone.
//!
//! Plain data with the time passed in: it builds and is tested on any system.

#![forbid(unsafe_code)]

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use prostir_proto::request::Outcome;
use prostir_proto::state::{HeadsetState, PcState, ReqId};
use prostir_proto::{HeadsetGoodbye, PcGoodbye};

use crate::connecting::{Ended, RETRY_PAUSE};
use crate::ids::PcId;
use crate::lang::Texts;
use crate::pending::{Answered, Asks};
use crate::ui::{Asked, OfferWhy, PairingView};

/// How many PCs the headset is in a session with at once: one for now (step 11 of 0.2.0
/// raises it). Everything here already works for more (the tests run two): what only
/// one may have (the sound and the microphone) goes to one link at a time.
pub const MAX_PCS: usize = 1;

/// After the PC said what is wrong (no encoder, say) and hung up, or speaks another
/// version: how long before trying again (retrying every 0.3 s would only rebuild its
/// monitors, or change nothing until one side is updated).
pub const REFUSED_PAUSE: Duration = Duration::from_secs(10);
/// A PC's error this soon before its connection ended counts as its refusal.
const REFUSED_WITHIN: Duration = Duration::from_secs(3);
/// How long a "Pair" answer holds for reconnecting and typing the code.
pub const APPROVAL: Duration = Duration::from_secs(600);
/// Queries go once a second for this long into a search, then every [`ASK_EVERY`].
pub const ASK_FAST_FOR: Duration = Duration::from_secs(30);
pub const ASK_FAST: Duration = Duration::from_secs(1);
pub const ASK_EVERY: Duration = Duration::from_secs(3);
/// A PC that answered is listed this long after its last answer.
pub const HEARD_WITHIN: Duration = Duration::from_secs(6);
/// A trusted PC's last address is dialed at most this often while it does not answer.
pub const LAST_EVERY: Duration = Duration::from_secs(10);
/// With no trusted PC (or the user picking one), no answer for this long brings the
/// hint of what to do on the PC.
pub const NEW_HINT_AFTER: Duration = Duration::from_secs(5);
/// A trusted PC silent for this long (no answer, no connection) brings the hint that
/// it does not answer.
pub const SILENT_HINT_AFTER: Duration = Duration::from_secs(30);
/// The most PCs listed from answers (the rest of a flood of forged ones is dropped).
pub const FOUND_MAX: usize = 8;

/// A PC this headset trusts, as the network thread's file of them says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairedPc {
    pub pc: PcId,
    /// As it was called when the headset paired with it.
    pub name: String,
}

/// A PC that answered a query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundHost {
    pub name: String,
    /// Where it takes the connection: the address the answer came from, and the TCP
    /// port the answer gave.
    pub addr: SocketAddr,
    /// The trusted PC whose tag the answer carried; none for a PC that answers
    /// everyone (unpaired, or with its pairing window open).
    pub pc: Option<PcId>,
    /// Its pairing window is open.
    pub pairing: bool,
    /// When the answer came.
    pub heard: Instant,
}

/// What the search screen says while nothing answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hint {
    /// No trusted PC (or the user picks another), and no PC answered: how to make the
    /// PC answer (open pairing there), type its address or use a cable.
    NewPc,
    /// Trusted PCs, but none answered nor took the connection for a while: check that
    /// Prostir runs on the PC and that it is on the same network.
    Silent,
}

/// Where a query goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskTo {
    /// As a broadcast, and straight to the addresses the network thread keeps (each
    /// trusted PC's last one, `host.txt`) and to `also` (the PC picked in the menu, by
    /// its TCP address: asked at its IP on the discovery port).
    Around { also: Option<SocketAddr> },
    /// To this UDP address alone (the selftest's fake PC).
    Exactly(SocketAddr),
}

/// One step of the search, by its number: what the network thread reports about it
/// says which one it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StepId(pub u32);

/// Where a dial goes. The network thread reads the files of the last two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dest {
    /// The PC over USB (`adb reverse` makes `127.0.0.1` reach it).
    Usb,
    Addr(SocketAddr),
    /// The address the user wrote in `host.txt`.
    HostFile,
    /// The last address this trusted PC had a session at.
    LastOf(PcId),
}

/// What the network thread is to do. It decides nothing itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetCmd {
    /// Reach a PC at `to`. A PC it trusts opens a session; one it does not pairs if its
    /// key is `pair` (the user agreed to it), with the PC showing the code when
    /// `pc_shows_code`, and is offered to the user otherwise. Nothing there:
    /// [`Session::missed`].
    Dial {
        step: StepId,
        to: Dest,
        pair: Option<[u8; 32]>,
        pc_shows_code: bool,
    },
    /// Ask which PCs are there (see [`AskTo`]): each answer is [`Session::heard`], and
    /// the end of the wait for them [`Session::missed`]. `pairing`: the headset looks
    /// for a PC to pair with, so the query carries its name.
    Ask {
        step: StepId,
        to: AskTo,
        pairing: bool,
    },
    /// The attempt before trust (dialing, pairing) is given up: its connection closes.
    Hangup,
    /// Leave this PC, telling it why.
    Leave(PcId, HeadsetGoodbye),
    /// The code the user typed while pairing.
    SubmitCode(String),
    /// Forget this PC: leave it (telling it so), drop it from the trusted ones and its
    /// files, then tell the new list.
    Forget(PcId),
    /// What the next `Open` says about the headset.
    SetHeadset(HeadsetState),
    /// The PC's sound plays and the microphone records for this PC's session only
    /// (none: for no session).
    MediaTo(Option<PcId>),
    /// The app is closing: every session ends, and the thread.
    Quit,
}

/// The search's attempt with a PC before it is trusted (at most one at a time).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attempt {
    /// Connecting to an address: whose it is shows once the PC answers.
    Dialing { step: StepId, addr: SocketAddr },
    /// A PC this headset does not trust: the user is asked whether to pair. Nothing
    /// connects on its own meanwhile.
    PairOffer {
        key: [u8; 32],
        addr: SocketAddr,
        label: String,
        why: OfferWhy,
    },
    /// Pairing with it, as the user agreed.
    Pairing {
        step: StepId,
        key: [u8; 32],
        addr: SocketAddr,
        view: PairingView,
    },
}

/// Everything before trust (see the module).
#[derive(Debug, Default)]
pub struct Search {
    /// PCs that answered lately (within [`HEARD_WITHIN`]), the latest last.
    pub found: Vec<FoundHost>,
    /// A PC that answers but does not accept the connection (a firewall).
    pub unreachable: Option<String>,
    /// What the search screen says while nothing answers.
    pub hint: Option<Hint>,
    pub attempt: Option<Attempt>,
    /// The PC speaks another protocol: (the PC is newer, its app version). Kept until
    /// a session opens.
    pub update: Option<(bool, String)>,
    /// Picked in the menu (or typed in): tried before anything else.
    choice: Option<SocketAddr>,
    /// "Change PC": nothing reconnects on its own until the user picks one.
    picking: bool,
    /// The user agreed to pair with this PC (until when).
    approved: Option<([u8; 32], Instant)>,
    /// Pairing the other way round: the PC shows the code, the headset asks for it.
    pc_shows_code: bool,
    round: Round,
    last_step: u32,
    /// Since when this search goes (worn, nothing connected): for how often it asks,
    /// and for the hints.
    since: Option<Instant>,
    /// When the last query went.
    asked: Option<Instant>,
    /// When each trusted PC's last address was dialed last.
    last_dialed: Vec<(PcId, Instant)>,
}

/// Where the search's round stands.
#[derive(Debug, Clone, Copy, Default)]
enum Round {
    /// The next round starts once it is due (`None`: at once).
    #[default]
    Idle,
    Resting(Instant),
    /// Step `step` is with the network thread; `at` is which of the round's steps.
    Waiting {
        step: StepId,
        at: Plan,
    },
    /// A connection the round made is open before trust: the round goes on once it
    /// ends.
    Open {
        step: StepId,
    },
}

/// The steps of a round, in their order (see the module).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Plan {
    Usb,
    Choice,
    Ask,
    Answer,
    HostFile,
    LastHost,
    Sole,
}

/// A trusted PC's connection.
#[derive(Debug)]
pub enum Link {
    /// It answered and is trusted: the session is opening.
    Connecting {
        addr: SocketAddr,
    },
    Connected(Box<Pc>),
    /// The session ended (the search goes on).
    Lost {
        since: Instant,
    },
}

/// A PC in a session, and what belongs to it alone (step 7 note, 4.5).
#[derive(Debug)]
pub struct Pc {
    pub addr: SocketAddr,
    /// What the user reads for it: its name as it announced itself, "PC over USB", or
    /// its address.
    pub label: String,
    /// Its state as it last said it in this session.
    pub state: PcState,
    /// The menu's requests to it in this session, and its commands waiting for one.
    pub asks: Asks,
}

/// How a connection ended, as the network thread saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disconnect {
    /// The PC ended the session and said why.
    Goodbye(PcGoodbye),
    /// Nothing came from the PC for a while: Wi-Fi gone, or the PC asleep.
    Silence,
    /// The connection closed (or never opened a session), without a word.
    Closed,
}

/// What follows the end of a link for the menu.
#[derive(Debug, Default)]
pub struct LinkEnded {
    /// The requests that waited for an answer, and their controls.
    pub waited: Vec<(ReqId, Asked)>,
    /// It was the last link: nothing is connected now.
    pub offline: bool,
}

/// The headset's connections (see the module).
#[derive(Debug)]
pub struct Session {
    max_pcs: usize,
    /// The selftest's fake PC: the only address ever dialed.
    only: Option<SocketAddr>,
    search: Search,
    links: BTreeMap<PcId, Link>,
    paired: Vec<PairedPc>,
    /// The headset is on the head: only then does the search connect.
    worn: bool,
    quit: bool,
    /// The session the sound and the microphone go with, as last told.
    media_to: Option<PcId>,
    /// Since when nothing is connected (the menu then opens on its own).
    offline_since: Option<Instant>,
    /// For the network thread, in order, until the app hands them over.
    cmds: Vec<NetCmd>,
    /// The state of no PC, for when none is connected.
    empty: PcState,
}

impl Session {
    /// At the start (`now`), not connected; `max_pcs`: [`MAX_PCS`] but in tests;
    /// `only`: the selftest's fake PC, the only address dialed then.
    pub fn new(max_pcs: usize, only: Option<SocketAddr>, now: Instant) -> Self {
        Self {
            max_pcs: max_pcs.max(1),
            only,
            search: Search::default(),
            links: BTreeMap::new(),
            paired: Vec::new(),
            worn: true,
            quit: false,
            media_to: None,
            offline_since: Some(now),
            cmds: Vec::new(),
            empty: PcState::default(),
        }
    }

    // ---- What the app reads ----

    /// The commands for the network thread since the last call, in order.
    pub fn take_cmds(&mut self) -> std::vec::Drain<'_, NetCmd> {
        self.cmds.drain(..)
    }

    pub fn search(&self) -> &Search {
        &self.search
    }

    /// Every link, by PC.
    pub fn links(&self) -> &BTreeMap<PcId, Link> {
        &self.links
    }

    /// The trusted PCs, as the network thread last said.
    pub fn paired(&self) -> &[PairedPc] {
        &self.paired
    }

    /// The PC in a session the menu acts on: the first connected one (with
    /// [`MAX_PCS`] one, the only one).
    pub fn main(&self) -> Option<PcId> {
        self.links
            .iter()
            .find(|(_, l)| matches!(l, Link::Connected(_)))
            .map(|(pc, _)| *pc)
    }

    /// PC `pc`, while in a session.
    pub fn pc(&self, pc: PcId) -> Option<&Pc> {
        match self.links.get(&pc)? {
            Link::Connected(p) => Some(p),
            _ => None,
        }
    }

    pub fn pc_mut(&mut self, pc: PcId) -> Option<&mut Pc> {
        match self.links.get_mut(&pc)? {
            Link::Connected(p) => Some(p),
            _ => None,
        }
    }

    /// The main PC's state (see [`Session::main`]), or that of no PC.
    pub fn state(&self) -> &PcState {
        self.main()
            .and_then(|pc| self.pc(pc))
            .map_or(&self.empty, |p| &p.state)
    }

    /// In a session with PC `pc`.
    pub fn connected(&self, pc: PcId) -> bool {
        self.pc(pc).is_some()
    }

    /// What the menu shows as the PC in use: its label, `None` while searching.
    pub fn host(&self) -> Option<&str> {
        self.main()
            .and_then(|pc| self.pc(pc))
            .map(|p| p.label.as_str())
    }

    /// Nothing has been connected for `after` (since the start or the last session):
    /// once, so the menu opens on its own one time.
    pub fn offline_for(&mut self, now: Instant, after: Duration) -> bool {
        let long = self
            .offline_since
            .is_some_and(|t| now.saturating_duration_since(t) > after);
        if long {
            self.offline_since = None;
        }
        long
    }

    /// The session the sound and the microphone go with (see [`NetCmd::MediaTo`]).
    pub fn media_to(&self) -> Option<PcId> {
        self.media_to
    }

    // ---- The menu's requests ----

    /// Control `what` asks PC `pc` for something at `now`: the number its request goes
    /// with. Without a session with it nothing is sent, and `None` comes back (the menu
    /// says why, see `note`).
    pub fn ask(&mut self, pc: Option<PcId>, what: Asked, now: Instant) -> Option<ReqId> {
        let p = pc.and_then(|pc| self.pc_mut(pc))?;
        Some(p.asks.ask(what, now))
    }

    /// PC `pc` answered request `req`.
    pub fn answered(&mut self, pc: PcId, req: ReqId, outcome: &Outcome, now: Instant) -> Answered {
        match self.pc_mut(pc) {
            Some(p) => p.asks.answered(req, outcome, now),
            None => Answered {
                what: None,
                took: None,
                command: None,
            },
        }
    }

    /// Requests without an answer in time fail as timed out: each one's PC, number and
    /// control.
    pub fn expire(&mut self, now: Instant) -> Vec<(PcId, ReqId, Asked)> {
        let mut late = Vec::new();
        for (pc, link) in &mut self.links {
            if let Link::Connected(p) = link {
                late.extend(
                    p.asks
                        .expire(now)
                        .into_iter()
                        .map(|(req, what)| (*pc, req, what)),
                );
            }
        }
        late
    }

    /// Whether control `what` waits for an answer from any PC.
    pub fn waits(&self, what: Asked) -> bool {
        self.links.values().any(|l| match l {
            Link::Connected(p) => p.asks.waits(what),
            _ => false,
        })
    }

    // ---- The user ----

    /// Put on or taken off: the search connects only while the headset is worn.
    pub fn set_worn(&mut self, worn: bool) {
        self.worn = worn;
    }

    /// What the next `Open` says about the headset.
    pub fn set_headset(&mut self, state: HeadsetState) {
        self.cmds.push(NetCmd::SetHeadset(state));
    }

    /// The user picked the PC at `addr` (in the list, or typed): it is tried first,
    /// and a session with another PC ends.
    pub fn connect_to(&mut self, addr: SocketAddr, now: Instant) {
        self.search.choice = Some(addr);
        self.search.picking = false;
        let others: Vec<PcId> = self
            .links
            .iter()
            .filter(|(_, l)| link_addr(l).is_some_and(|a| a != addr))
            .map(|(pc, _)| *pc)
            .collect();
        for pc in others {
            self.cmds.push(NetCmd::Leave(pc, HeadsetGoodbye::OtherPc));
        }
        if self.attempt_addr().is_some_and(|a| a != addr) {
            self.cmds.push(NetCmd::Hangup);
        }
        self.round_now(now);
    }

    /// "Change PC": the PC in use is left, and nothing connects on its own until the
    /// user picks one. The search starts anew (its hint for a new PC too).
    pub fn change_pc(&mut self) {
        self.search.choice = None;
        self.search.picking = true;
        self.search.since = None;
        self.leave_all(HeadsetGoodbye::OtherPc);
        if self.attempt_addr().is_some() {
            self.cmds.push(NetCmd::Hangup);
        }
    }

    /// The user forgot PC `pc`: the network thread leaves it, telling it so, and drops
    /// it; the headset does not come back to it on its own.
    pub fn forget(&mut self, pc: PcId) {
        if self.links.get(&pc).is_some_and(|l| link_addr(l).is_some()) {
            self.search.choice = None;
            self.search.picking = true;
        }
        self.cmds.push(NetCmd::Forget(pc));
    }

    /// The user's answer to "Pair with this PC?".
    pub fn answer_offer(&mut self, pair: bool, now: Instant) {
        let Some(Attempt::PairOffer { key, addr, .. }) = self.search.attempt.take() else {
            return;
        };
        if pair {
            self.search.approved = Some((key, now + APPROVAL));
            self.search.choice = Some(addr);
            self.search.picking = false;
        } else {
            // Not this PC: wait for the user to pick one.
            self.search.approved = None;
            self.search.choice = None;
            self.search.picking = true;
        }
        self.round_now(now);
    }

    /// Pairing: the user typed `code` for the code the PC shows.
    pub fn submit_code(&mut self, code: String) {
        self.cmds.push(NetCmd::SubmitCode(code));
    }

    /// Pairing: which side shows the code is swapped, and pairing starts over.
    pub fn switch_pairing_side(&mut self) {
        self.search.pc_shows_code = !self.search.pc_shows_code;
        if matches!(self.search.attempt, Some(Attempt::Pairing { .. })) {
            self.search.attempt = None;
        }
        self.cmds.push(NetCmd::Hangup);
    }

    /// Pairing: the code being typed, for the menu's keys.
    pub fn pairing_mut(&mut self) -> Option<&mut PairingView> {
        match &mut self.search.attempt {
            Some(Attempt::Pairing { view, .. }) => Some(view),
            _ => None,
        }
    }

    /// The app is closing: every session ends, and the network thread.
    pub fn quit(&mut self) {
        self.quit = true;
        self.cmds.push(NetCmd::Quit);
    }

    // ---- What the network thread says ----

    /// The trusted PCs, as the network thread's file says (at the start and after each
    /// change).
    pub fn paired_list(&mut self, list: Vec<PairedPc>) {
        self.paired = list;
        let paired = &self.paired;
        self.search
            .last_dialed
            .retain(|(pc, _)| paired.iter().any(|p| p.pc == *pc));
    }

    /// A PC answered a query at `now`: listed (in place of its earlier answer), and a
    /// trusted one is connected to at once while the round waits for answers.
    pub fn heard(&mut self, host: FoundHost, now: Instant) {
        let found = &mut self.search.found;
        found.retain(|h| h.addr != host.addr && (host.pc.is_none() || h.pc != host.pc));
        if found.len() >= FOUND_MAX {
            found.remove(0);
        }
        let (addr, pc) = (host.addr, host.pc);
        found.push(host);
        let Round::Waiting { at: Plan::Ask, .. } = self.search.round else {
            return;
        };
        if let Some(only) = self.only {
            if pc.is_some() {
                self.dial(Plan::Choice, Dest::Addr(only), now);
            }
        } else if self.search.choice.is_none()
            && !self.search.picking
            && pc.is_some_and(|pc| self.wants(pc))
        {
            self.dial(Plan::Answer, Dest::Addr(addr), now);
        }
    }

    /// Nothing answered at step `step` (or the wait for answers to its query is over):
    /// the round goes on.
    pub fn missed(&mut self, step: StepId, now: Instant) {
        let Round::Waiting { step: waiting, at } = self.search.round else {
            return;
        };
        if waiting != step {
            return;
        }
        if let Some(addr) = self.addr_of(at)
            && let Some(h) = self.search.found.iter().find(|h| h.addr == addr)
        {
            // It answers but refuses the connection: most likely a firewall.
            self.search.unreachable = Some(h.name.clone());
        }
        if matches!(
            self.search.attempt,
            Some(Attempt::Dialing { step: s, .. }) if s == step
        ) {
            self.search.attempt = None;
        }
        self.step_after(Some(at), now);
    }

    /// The connection of step `step` is open (TCP): an attempt until the PC's key
    /// says whose it is.
    pub fn dialed(&mut self, step: StepId, addr: SocketAddr) {
        if !matches!(self.search.round, Round::Waiting { step: s, .. } if s == step) {
            return;
        }
        self.search.round = Round::Open { step };
        if !matches!(self.search.attempt, Some(Attempt::PairOffer { .. })) {
            self.search.attempt = Some(Attempt::Dialing { step, addr });
        }
    }

    /// The PC at `addr` of step `step` is trusted (or was just paired with): its
    /// session opens. The round is over.
    pub fn reached(&mut self, step: StepId, pc: PcId, addr: SocketAddr, now: Instant) {
        if matches!(
            self.search.attempt,
            Some(Attempt::Dialing { step: s, .. } | Attempt::Pairing { step: s, .. }) if s == step
        ) {
            self.search.attempt = None;
        }
        // A PC already on its way or in a session keeps that link (the network thread
        // never makes a second connection to it).
        if self
            .links
            .get(&pc)
            .is_none_or(|l| matches!(l, Link::Lost { .. }))
        {
            self.links.insert(pc, Link::Connecting { addr });
        }
        if matches!(self.search.round, Round::Open { step: s } | Round::Waiting { step: s, .. } if s == step)
        {
            self.search.round = Round::Resting(now + RETRY_PAUSE);
        }
    }

    /// A PC this headset does not trust answered: the user is asked whether to pair
    /// with it (`why`); its connection is closed meanwhile.
    pub fn offered(&mut self, key: [u8; 32], addr: SocketAddr, why: OfferWhy, texts: &Texts) {
        let label = offer_label(&self.label(addr, texts), addr);
        self.search.approved = None;
        self.search.attempt = Some(Attempt::PairOffer {
            key,
            addr,
            label,
            why,
        });
    }

    /// Pairing with the PC of step `step` (key `key`) goes on: the code it shows the
    /// user, or (`None`) the user is to type the code the PC shows.
    pub fn pairing(
        &mut self,
        step: StepId,
        key: [u8; 32],
        addr: SocketAddr,
        code: Option<String>,
        texts: &Texts,
    ) {
        let failed = matches!(
            &self.search.attempt,
            Some(Attempt::Pairing { view, .. }) if view.failed
        );
        let host = self.label(addr, texts);
        self.search.attempt = Some(Attempt::Pairing {
            step,
            key,
            addr,
            view: PairingView {
                host,
                code,
                typed: String::new(),
                failed,
            },
        });
    }

    /// The codes did not match; a new attempt follows.
    pub fn pairing_failed(&mut self) {
        if let Some(view) = self.pairing_mut() {
            view.failed = true;
            view.typed.clear();
        }
    }

    /// The PC speaks another protocol version: which side to update.
    pub fn incompatible(&mut self, pc_newer: bool, pc_app: String) {
        self.search.update = Some((pc_newer, pc_app));
    }

    /// The session with PC `pc` opened at `addr`: the link is connected, with its
    /// state from the parts that follow.
    pub fn connected_to(&mut self, pc: PcId, addr: SocketAddr, texts: &Texts) {
        // Paired again inside the session's opening (the PC had forgotten the
        // headset), or dialed there: that attempt is over.
        let its = match &self.search.attempt {
            Some(Attempt::Pairing { key, .. }) => PcId::of(key) == pc,
            Some(Attempt::Dialing { addr: a, .. }) => *a == addr,
            _ => false,
        };
        if its {
            self.search.attempt = None;
        }
        let label = self.label(addr, texts);
        self.links.insert(
            pc,
            Link::Connected(Box::new(Pc {
                addr,
                label,
                state: PcState::default(),
                asks: Asks::default(),
            })),
        );
        self.search.update = None;
        self.search.unreachable = None;
        self.offline_since = None;
        self.media_due();
    }

    /// The connection of step `step` (and of PC `pc`, if it got that far) ended at
    /// `now`, after `lasted`, as `ended` says. The link is lost (what waited for an
    /// answer gets none), and the search rests for as long as the way it ended asks.
    pub fn ended(
        &mut self,
        step: StepId,
        pc: Option<PcId>,
        ended: &Ended,
        lasted: Duration,
        now: Instant,
    ) -> LinkEnded {
        let mut out = LinkEnded::default();
        if let Some(pc) = pc
            && let Some(link) = self.links.get_mut(&pc)
        {
            let was_connected = matches!(link, Link::Connected(_));
            if let Link::Connected(p) = link {
                out.waited = p.asks.disconnected();
            }
            *link = Link::Lost { since: now };
            // Offline only after a session: a link that never opened (the PC asked to
            // pair again, say) changes nothing the user had.
            out.offline = was_connected && self.main().is_none();
            if out.offline {
                self.offline_since = Some(now);
            }
            self.media_due();
        }
        if matches!(
            self.search.attempt,
            Some(Attempt::Dialing { step: s, .. } | Attempt::Pairing { step: s, .. }) if s == step
        ) {
            self.search.attempt = None;
        }
        let pause = if ended
            .refused
            .is_some_and(|t| now.saturating_duration_since(t) < REFUSED_WITHIN)
            || ended.incompatible
        {
            REFUSED_PAUSE
        } else if ended.retry_at_once(lasted) {
            Duration::ZERO
        } else {
            RETRY_PAUSE
        };
        match self.search.round {
            Round::Open { step: s } | Round::Waiting { step: s, .. } if s == step => {
                self.search.round = Round::Resting(now + pause);
            }
            // The round was over once the PC was reached; the next one waits as long.
            Round::Resting(_) | Round::Idle if pc.is_some() => {
                self.search.round = Round::Resting(now + pause);
            }
            _ => {}
        }
        out
    }

    /// Part of PC `pc`'s state came (checked by the network thread already).
    pub fn part(&mut self, pc: PcId, part: prostir_proto::HostMsg) -> Result<(), String> {
        let Some(p) = self.pc_mut(pc) else {
            return Ok(());
        };
        p.state.apply(part).map(|_| ()).map_err(|e| e.to_string())
    }

    // ---- The time ----

    /// Once a pass of the main loop, with or without frames: answers too old are no
    /// longer listed, the hint follows, and the next round of the search starts when it
    /// is due.
    pub fn tick(&mut self, now: Instant) {
        self.search
            .found
            .retain(|h| now.saturating_duration_since(h.heard) < HEARD_WITHIN);
        let searching = self.worn
            && !self.quit
            && !self.links.values().any(|l| !matches!(l, Link::Lost { .. }));
        if searching {
            self.search.since.get_or_insert(now);
        } else {
            self.search.since = None;
        }
        self.search.hint = self.hint(now);
        if let Round::Resting(at) = self.search.round
            && now >= at
        {
            self.search.round = Round::Idle;
        }
        if matches!(self.search.round, Round::Idle) && self.may_search() {
            self.step_after(None, now);
        }
    }

    // ---- Inside ----

    /// Whether a round of the search may start: worn, nothing asked of the user,
    /// fewer links than allowed, not closing.
    fn may_search(&self) -> bool {
        let active = self
            .links
            .values()
            .filter(|l| !matches!(l, Link::Lost { .. }))
            .count();
        self.worn
            && !self.quit
            && active < self.max_pcs
            && !matches!(self.search.attempt, Some(Attempt::PairOffer { .. }))
    }

    /// The next round goes at once (the user picked something).
    fn round_now(&mut self, now: Instant) {
        if let Round::Resting(_) = self.search.round {
            self.search.round = Round::Resting(now);
        }
    }

    /// The next step of the round after `after` (`None`: a new round), handed to the
    /// network thread; the round rests until the next query is due when there is none.
    fn step_after(&mut self, after: Option<Plan>, now: Instant) {
        if !self.may_search() {
            self.search.round = Round::Resting(now + RETRY_PAUSE);
            return;
        }
        if let Some(only) = self.only {
            // The fake PC alone: asked, and dialed whether it answered or not (it
            // answers once it gave its find secret in a session). A real PC over USB
            // (`127.0.0.1:47830`, `adb reverse`) is never asked: USB carries TCP only.
            let usb = only.ip().is_loopback() && only.port() == prostir_proto::TCP_PORT;
            match after {
                None if !usb && self.ask_due(now) => {
                    return self.query(Plan::Ask, AskTo::Exactly(only), now);
                }
                None | Some(Plan::Ask) => return self.dial(Plan::Choice, Dest::Addr(only), now),
                Some(_) => {}
            }
            return self.rest(now);
        }
        let s = &self.search;
        let (choice, picking) = (s.choice, s.picking);
        // The user is choosing, or chose: only their choice, and the list of answers.
        let own_way = !picking && choice.is_none();
        let mut at = after;
        loop {
            let next = match at {
                None => Plan::Usb,
                Some(Plan::Usb) => Plan::Choice,
                Some(Plan::Choice) => Plan::Ask,
                Some(Plan::Ask) => Plan::Answer,
                Some(Plan::Answer) => Plan::HostFile,
                Some(Plan::HostFile) => Plan::LastHost,
                Some(Plan::LastHost) => Plan::Sole,
                Some(Plan::Sole) => break,
            };
            at = Some(next);
            match next {
                // USB: not while the user picks a PC or picked one on the network.
                Plan::Usb if !picking && choice.is_none_or(|c| c.ip().is_loopback()) => {
                    return self.dial(next, Dest::Usb, now);
                }
                Plan::Choice => {
                    if let Some(addr) = choice {
                        return self.dial(next, Dest::Addr(addr), now);
                    }
                }
                Plan::Ask if self.ask_due(now) => {
                    let also = choice.filter(|c| !c.ip().is_loopback());
                    return self.query(next, AskTo::Around { also }, now);
                }
                Plan::Answer if own_way => {
                    if let Some(addr) = self.trusted_answer() {
                        return self.dial(next, Dest::Addr(addr), now);
                    }
                }
                Plan::HostFile if own_way => return self.dial(next, Dest::HostFile, now),
                Plan::LastHost if own_way => {
                    if let Some(pc) = self.last_due(now) {
                        self.search.last_dialed.retain(|(p, _)| *p != pc);
                        self.search.last_dialed.push((pc, now));
                        return self.dial(next, Dest::LastOf(pc), now);
                    }
                }
                // Exactly one PC answered everyone: just use it (unknown, it is offered
                // to the user). Several: wait for a pick.
                Plan::Sole if own_way => {
                    if let Some(addr) = self.sole_other() {
                        return self.dial(next, Dest::Addr(addr), now);
                    }
                }
                _ => {}
            }
        }
        self.rest(now);
    }

    /// The round is over: the next one goes once the next query is due.
    fn rest(&mut self, now: Instant) {
        let due = self.search.asked.map_or(now, |at| at + self.ask_every(now));
        self.search.round = Round::Resting(due.max(now + RETRY_PAUSE));
    }

    /// How often the search asks at `now`.
    fn ask_every(&self, now: Instant) -> Duration {
        let long = self
            .search
            .since
            .is_some_and(|since| now.saturating_duration_since(since) >= ASK_FAST_FOR);
        if long { ASK_EVERY } else { ASK_FAST }
    }

    fn ask_due(&self, now: Instant) -> bool {
        self.search
            .asked
            .is_none_or(|at| now.saturating_duration_since(at) >= self.ask_every(now))
    }

    fn query(&mut self, at: Plan, to: AskTo, now: Instant) {
        let step = self.next_step();
        self.search.asked = Some(now);
        self.search.round = Round::Waiting { step, at };
        self.cmds.push(NetCmd::Ask {
            step,
            to,
            pairing: self.looking_for_new(),
        });
    }

    /// The headset looks for a PC to pair with: it trusts none, or the user picks
    /// another.
    fn looking_for_new(&self) -> bool {
        self.paired.is_empty() || self.search.picking
    }

    /// Whether the search would connect to trusted PC `pc` now: it has no link on its
    /// way or in a session.
    fn wants(&self, pc: PcId) -> bool {
        self.paired.iter().any(|p| p.pc == pc)
            && self
                .links
                .get(&pc)
                .is_none_or(|l| matches!(l, Link::Lost { .. }))
    }

    /// The address of a trusted PC that answered and is wanted, the latest first.
    fn trusted_answer(&self) -> Option<SocketAddr> {
        self.search
            .found
            .iter()
            .rev()
            .find(|h| h.pc.is_some_and(|pc| self.wants(pc)))
            .map(|h| h.addr)
    }

    /// The address of the one PC that answered without being a trusted one (it
    /// answers everyone), if exactly one did.
    fn sole_other(&self) -> Option<SocketAddr> {
        let mut others = self.search.found.iter().filter(|h| h.pc.is_none());
        match (others.next(), others.next()) {
            (Some(one), None) => Some(one.addr),
            _ => None,
        }
    }

    /// A trusted PC whose last address is due to be dialed at `now`: wanted, not
    /// dialed there within [`LAST_EVERY`].
    fn last_due(&self, now: Instant) -> Option<PcId> {
        self.paired.iter().map(|p| p.pc).find(|pc| {
            self.wants(*pc)
                && self
                    .search
                    .last_dialed
                    .iter()
                    .find(|(p, _)| p == pc)
                    .is_none_or(|(_, at)| now.saturating_duration_since(*at) >= LAST_EVERY)
        })
    }

    /// What the search screen says at `now` (see [`Hint`]).
    fn hint(&self, now: Instant) -> Option<Hint> {
        let s = &self.search;
        let long = now.saturating_duration_since(s.since?);
        if self.only.is_some() || s.attempt.is_some() || s.update.is_some() {
            return None;
        }
        if self.looking_for_new() {
            (long >= NEW_HINT_AFTER && s.found.is_empty()).then_some(Hint::NewPc)
        } else {
            let answered = s.found.iter().any(|h| h.pc.is_some());
            (s.choice.is_none() && long >= SILENT_HINT_AFTER && !answered).then_some(Hint::Silent)
        }
    }

    fn dial(&mut self, at: Plan, to: Dest, now: Instant) {
        let step = self.next_step();
        let pair = self
            .search
            .approved
            .filter(|(_, until)| now < *until)
            .map(|(key, _)| key);
        self.search.round = Round::Waiting { step, at };
        self.cmds.push(NetCmd::Dial {
            step,
            to,
            pair,
            pc_shows_code: self.search.pc_shows_code,
        });
    }

    fn next_step(&mut self) -> StepId {
        self.search.last_step += 1;
        StepId(self.search.last_step)
    }

    /// The address step `at` of the round dialed, where the round knows it.
    fn addr_of(&self, at: Plan) -> Option<SocketAddr> {
        match at {
            Plan::Choice => self.search.choice.or(self.only),
            Plan::Sole => self.sole_other(),
            Plan::Answer => self.trusted_answer(),
            _ => None,
        }
    }

    /// The address of the attempt before trust, while one is on its way.
    fn attempt_addr(&self) -> Option<SocketAddr> {
        match &self.search.attempt {
            Some(Attempt::Dialing { addr, .. } | Attempt::Pairing { addr, .. }) => Some(*addr),
            _ => None,
        }
    }

    /// Every link's session ends, telling its PC `why`.
    fn leave_all(&mut self, why: HeadsetGoodbye) {
        for (pc, link) in &self.links {
            if !matches!(link, Link::Lost { .. }) {
                self.cmds.push(NetCmd::Leave(*pc, why));
            }
        }
    }

    /// The sound and the microphone go with the main PC's session: the network thread
    /// hears when that changes.
    fn media_due(&mut self) {
        let to = self.main();
        if to != self.media_to {
            self.media_to = to;
            self.cmds.push(NetCmd::MediaTo(to));
        }
    }

    /// What the user reads for the PC at `addr`: its name as its answer said it, "PC
    /// over USB", or its address.
    fn label(&self, addr: SocketAddr, texts: &Texts) -> String {
        if let Some(h) = self.search.found.iter().find(|h| h.addr == addr) {
            return h.name.clone();
        }
        if addr.ip().is_loopback() {
            texts.t("pc-usb").to_owned()
        } else {
            addr.ip().to_string()
        }
    }
}

/// The address of a link still on its way or in its session.
fn link_addr(link: &Link) -> Option<SocketAddr> {
    match link {
        Link::Connecting { addr } => Some(*addr),
        Link::Connected(p) => Some(p.addr),
        Link::Lost { .. } => None,
    }
}

/// The PC's name with its address, so a look-alike name from the network stands out.
fn offer_label(host: &str, addr: SocketAddr) -> String {
    let ip = addr.ip().to_string();
    if host == ip || addr.ip().is_loopback() {
        host.to_owned()
    } else {
        format!("{host} {ip}")
    }
}

/// The menu's text for how a connection ended; none where there is nothing to say
/// (the headset was off the head, or the PC already said why).
pub fn disconnect_text(why: Disconnect) -> Option<&'static str> {
    Some(match why {
        Disconnect::Goodbye(PcGoodbye::Quit) => "bye-pc-quit",
        Disconnect::Goodbye(PcGoodbye::Forgotten) => "bye-forgotten",
        Disconnect::Goodbye(PcGoodbye::Other) => "bye-pc-ended",
        Disconnect::Silence => "bye-silence",
        Disconnect::Goodbye(PcGoodbye::TakenOff | PcGoodbye::Replaced | PcGoodbye::Failed)
        | Disconnect::Closed => return None,
    })
}
