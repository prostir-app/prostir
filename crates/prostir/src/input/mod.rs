//! Input from the headsets made on the PC: the mouse moved (`MouseTo`, the gaze), its
//! buttons and wheel, fingers on a monitor (`PROTOCOL.md`, "Input from the headset").
//!
//! One thread (`input`) owns the desktop's virtual devices (an [`Injector`], opened
//! through the desktop's [`OpenInput`]), what each session holds ([`held`]) and the
//! fingers made into mouse actions where the desktop has no touch ([`touch_mouse`]).
//! The sessions hand it their events through a channel, in the order they came, from
//! the control loop: never through the desktop's worker, whose jobs may take seconds.
//! Consecutive moves of one session are taken as the last of them.
//!
//! The devices are opened on the first event that needs them, on a thread of their
//! own, since the desktop may ask the person first (once: it remembers the answer).
//! Events meanwhile go nowhere. A person who said no is not asked again until Prostir
//! starts again; devices the desktop closed are opened again on the next event, after
//! a pause that grows, so a desktop that keeps closing them is not asked in a loop.
//!
//! Nothing stays pressed: a session's end, its headset taken off and two seconds of
//! silence from a session holding something let everything it holds go; devices that
//! are gone let go with them (the desktop does that). A pass of the thread that
//! panics lets go of everything and drops the devices; the thread goes on. Dropped,
//! the handle waits a moment for the thread to let go of everything.
//!
//! Devices the person closed (they stopped the desktop's remote control) count as a
//! no: not opened again by themselves. Only devices that did not open are tried again.
//!
//! Real touches hide the desktop's pointer, as on a touch screen: from a finger down
//! until the mouse moves again ([`Pointing`], for the headsets that draw the pointer).
//!
//! The mouse put against a screen edge is pushed on a few times ([`edge`]), so the
//! desktop's edge (an auto-hiding panel) takes it as a real mouse pushing there.

pub mod edge;
pub mod held;
pub mod touch_mouse;

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use log::{error, info, warn};

use crate::events::SessionId;
use crate::platform::{
    Injector, InputRefused, InputStopped, MouseButton, OpenInput, Spot, TouchPhase,
};
use edge::EdgePush;
use held::{Act, Held};
use touch_mouse::TouchMouse;

/// The first pause before devices the desktop closed are opened again; it doubles
/// with each time, up to [`REOPEN_MAX`].
const REOPEN_FIRST: Duration = Duration::from_secs(2);
const REOPEN_MAX: Duration = Duration::from_secs(120);
/// How often the thread looks at the devices and what is held, while there are any.
const PUMP_EVERY: Duration = Duration::from_millis(100);
/// How often a page thrown with a finger moves on.
const COAST_EVERY: Duration = Duration::from_millis(16);
/// Moves waiting for the thread at most: more are dropped (the newest will do).
const MOVES_WAITING: usize = 32;
/// How long a handle dropped waits for the thread to let go of everything.
const LET_GO_WITHIN: Duration = Duration::from_millis(500);

/// What the person is to hear of the input (each once, until it works again).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum News {
    /// The person did not allow it, or stopped it.
    Refused,
    /// The desktop's devices did not open.
    Unavailable,
}

/// How the input is, for the capabilities named to new sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Open, or not tried yet.
    Fine,
    Refused,
    Unavailable,
}

impl State {
    fn of(v: u8) -> Self {
        match v {
            1 => Self::Refused,
            2 => Self::Unavailable,
            _ => Self::Fine,
        }
    }

    fn code(self) -> u8 {
        match self {
            Self::Fine => 0,
            Self::Refused => 1,
            Self::Unavailable => 2,
        }
    }
}

/// What the input did to whether the desktop's pointer shows: real touches hide it, as
/// on a touch screen (`PROTOCOL.md`, "The mouse pointer", `shape` 0); mouse actions
/// made of touches (a desktop without touch) move it as the mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pointing {
    /// A finger touches the desktop: the pointer hides.
    Touch,
    /// The last finger lifted: the pointer stays hidden until the mouse moves.
    Lifted,
    /// The mouse moved, pressed or scrolled: the pointer shows.
    Mouse,
}

/// What a session asks of the input.
#[derive(Debug, Clone, PartialEq)]
pub enum Ev {
    /// The mouse to `at`.
    To { from: SessionId, at: Spot },
    /// A button down or up, the mouse to `at` first (none: where it is, for a display
    /// gone meanwhile; a button is let go all the same).
    Button {
        from: SessionId,
        at: Option<Spot>,
        button: MouseButton,
        down: bool,
    },
    /// The wheel, in 1/120 of a notch.
    Scroll { from: SessionId, dx: i32, dy: i32 },
    /// Finger `slot` of the session (none: on a display gone meanwhile, which lifts
    /// it where it was), as it came from the headset at `came`: gestures are timed by
    /// when they came, not by when the thread takes them (several at once).
    Touch {
        from: SessionId,
        slot: u8,
        phase: TouchPhase,
        at: Option<Spot>,
        came: Instant,
    },
    /// Everything the session holds, let go (its end, the headset taken off).
    Release { from: SessionId },
}

enum Msg {
    Opener(Option<Arc<dyn OpenInput>>),
    Ev(Ev),
}

/// The person said no (or stopped the devices) in this run of Prostir: kept for the
/// whole process, not only while one set of displays lives, so a later session does
/// not ask again.
static REFUSED: AtomicBool = AtomicBool::new(false);

/// What the thread and its handle share.
struct Shared {
    state: AtomicU8,
    /// Moves sent and not taken yet.
    moves: AtomicUsize,
    /// The person's no: [`REFUSED`] (the tests' own elsewhere).
    refused: &'static AtomicBool,
}

impl Shared {
    fn new(refused: &'static AtomicBool) -> Self {
        Self {
            state: AtomicU8::new(0),
            moves: AtomicUsize::new(0),
            refused,
        }
    }

    fn refused(&self) -> bool {
        self.refused.load(Ordering::Relaxed)
    }
}

/// The control loop's side of the input thread. Dropped, the thread lets go of
/// everything and closes the devices, and the handle waits a moment for that.
pub struct Input {
    tx: Option<Sender<Msg>>,
    shared: Arc<Shared>,
    ended: Option<Receiver<()>>,
}

impl Input {
    /// The input thread, without devices until [`Input::set_opener`]; what the person
    /// is to hear goes to `tell`, what touches and the mouse do to the pointer to
    /// `pointing` (each change once).
    pub fn start(
        tell: impl Fn(News) + Send + 'static,
        pointing: impl Fn(Pointing) + Send + 'static,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let (end_tx, ended) = mpsc::channel();
        let shared = Arc::new(Shared::new(&REFUSED));
        let mut run = Run::new(Box::new(tell), shared.clone());
        run.pointing = Box::new(pointing);
        let spawned = std::thread::Builder::new()
            .name("input".into())
            .spawn(move || {
                run.run(&rx);
                let _ = end_tx.send(());
            });
        if let Err(e) = spawned {
            warn!("no thread for the input from the headset: {e}");
        }
        Self {
            tx: Some(tx),
            shared,
            ended: Some(ended),
        }
    }

    /// How to open the desktop's devices (none: it has none).
    pub fn set_opener(&self, opener: Option<Arc<dyn OpenInput>>) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Msg::Opener(opener));
        }
    }

    /// Hands `ev` over, never waiting; a move while many wait is dropped.
    pub fn send(&self, ev: Ev) {
        if matches!(ev, Ev::To { .. }) {
            if self.shared.moves.load(Ordering::Relaxed) >= MOVES_WAITING {
                return;
            }
            self.shared.moves.fetch_add(1, Ordering::Relaxed);
        }
        if let Some(tx) = &self.tx {
            let _ = tx.send(Msg::Ev(ev));
        }
    }

    /// How the input is now.
    pub fn state(&self) -> State {
        if self.shared.refused() {
            return State::Refused;
        }
        State::of(self.shared.state.load(Ordering::Relaxed))
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        drop(self.tx.take());
        if let Some(ended) = self.ended.take()
            && ended.recv_timeout(LET_GO_WITHIN).is_err()
        {
            warn!("input: the thread did not end in time");
        }
    }
}

/// Opening the devices, on a thread of its own: what came of it.
type Opening = Receiver<anyhow::Result<Box<dyn Injector>>>;

struct Run {
    tell: Box<dyn Fn(News) + Send>,
    shared: Arc<Shared>,
    /// What the person heard last, not said again until the input works.
    said: Option<News>,
    opener: Option<Arc<dyn OpenInput>>,
    injector: Option<Box<dyn Injector>>,
    opening: Option<Opening>,
    /// Not opened again before this, and the pause after it.
    reopen: (Instant, Duration),
    held: Held,
    mice: BTreeMap<SessionId, TouchMouse>,
    /// The mouse held at a screen edge, pushed on.
    edge: EdgePush,
    /// Where what the input does to the pointer goes, and what it said last.
    pointing: Box<dyn Fn(Pointing) + Send>,
    pointed: Option<Pointing>,
}

impl Run {
    fn new(tell: Box<dyn Fn(News) + Send>, shared: Arc<Shared>) -> Self {
        Self {
            tell,
            shared,
            said: None,
            opener: None,
            injector: None,
            opening: None,
            reopen: (Instant::now(), REOPEN_FIRST),
            held: Held::default(),
            mice: BTreeMap::new(),
            edge: EdgePush::default(),
            pointing: Box::new(|_| {}),
            pointed: None,
        }
    }

    /// Says what the input did to the pointer, where that changed.
    fn point(&mut self, p: Pointing) {
        if self.pointed != Some(p) {
            self.pointed = Some(p);
            (self.pointing)(p);
        }
    }

    /// The fingers went with the devices (or with what held them): none hides the
    /// pointer any more, though it stays hidden until the mouse moves.
    fn fingers_gone(&mut self) {
        if self.pointed == Some(Pointing::Touch) {
            self.point(Pointing::Lifted);
        }
    }

    fn run(mut self, rx: &Receiver<Msg>) {
        loop {
            let busy = self.injector.is_some() || self.opening.is_some() || self.held.any();
            let wait = if self.mice.values().any(TouchMouse::moving) {
                COAST_EVERY
            } else if busy {
                PUMP_EVERY
            } else {
                Duration::from_secs(3600)
            };
            let wait = self.edge.next().map_or(wait, |at| {
                wait.min(at.saturating_duration_since(Instant::now()))
            });
            let first = match rx.recv_timeout(wait) {
                Ok(m) => Some(m),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            };
            let batch: Vec<Msg> = first.into_iter().chain(rx.try_iter()).collect();
            let moves = batch
                .iter()
                .filter(|m| matches!(m, Msg::Ev(Ev::To { .. })))
                .count();
            // Each was counted before it was sent, and only this thread takes them off.
            self.shared.moves.fetch_sub(moves, Ordering::Relaxed);
            let pass = catch_unwind(AssertUnwindSafe(|| self.pass(batch)));
            if pass.is_err() {
                self.recover();
            }
        }
        let _ = catch_unwind(AssertUnwindSafe(|| self.release_everything()));
        info!("input: the thread ends");
    }

    /// One pass: the events that came, then what time does.
    fn pass(&mut self, batch: Vec<Msg>) {
        // Devices opened meanwhile take these events.
        self.opened(Instant::now());
        for m in coalesce(batch) {
            match m {
                Msg::Opener(opener) => self.set_opener(opener),
                Msg::Ev(ev) => self.event(ev, Instant::now()),
            }
        }
        self.tick(Instant::now());
    }

    fn release_everything(&mut self) {
        self.edge.stop();
        for from in self.held.sessions() {
            self.release(from);
        }
    }

    /// A pass panicked: everything held let go, as far as that goes, and the devices
    /// dropped (the desktop lets go of what they still held).
    fn recover(&mut self) {
        error!("input: a pass of the input thread failed: letting go of everything");
        let _ = catch_unwind(AssertUnwindSafe(|| self.release_everything()));
        self.injector = None;
        self.edge.stop();
        self.opening = None;
        self.held.clear();
        self.mice.clear();
        self.fingers_gone();
        self.later(Instant::now());
    }

    fn set_opener(&mut self, opener: Option<Arc<dyn OpenInput>>) {
        self.injector = None;
        self.opening = None;
        self.held.clear();
        self.mice.clear();
        self.fingers_gone();
        let refused = self.shared.refused();
        self.said = refused.then_some(News::Refused);
        self.set_state(if refused { State::Refused } else { State::Fine });
        self.reopen = (Instant::now(), REOPEN_FIRST);
        self.opener = opener;
    }

    fn set_state(&self, state: State) {
        self.shared.state.store(state.code(), Ordering::Relaxed);
    }

    /// Tells the person `news`, unless it was the last thing they heard.
    fn say(&mut self, news: News) {
        if self.said != Some(news) {
            self.said = Some(news);
            (self.tell)(news);
        }
    }

    /// The devices, if open; else they start opening, where they may.
    fn devices(&mut self, now: Instant) -> bool {
        if self.injector.is_some() {
            return true;
        }
        if self.opening.is_some() || self.shared.refused() || now < self.reopen.0 {
            return false;
        }
        let Some(opener) = self.opener.clone() else {
            return false;
        };
        info!("input: opening the desktop's virtual devices");
        let (tx, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("input-open".into())
            .spawn(move || {
                let _ = tx.send(opener.open());
            });
        match spawned {
            Ok(_) => self.opening = Some(rx),
            Err(e) => warn!("input: no thread to open the devices on: {e}"),
        }
        false
    }

    /// What came of opening the devices, once it came.
    fn opened(&mut self, now: Instant) {
        let Some(rx) = &self.opening else {
            return;
        };
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err(anyhow::anyhow!("its thread ended")),
        };
        self.opening = None;
        match result {
            Ok(injector) => {
                info!(
                    "input: the desktop's virtual devices are open (touch: {})",
                    if injector.touches() {
                        "real"
                    } else {
                        "as a mouse"
                    }
                );
                self.injector = Some(injector);
                self.reopen.1 = REOPEN_FIRST;
                self.said = None;
                self.set_state(State::Fine);
            }
            Err(e) if refused(&e) => self.refuse(&e),
            Err(e) => {
                warn!("input: the desktop's virtual devices did not open: {e:#}");
                self.later(now);
                self.set_state(State::Unavailable);
                self.say(News::Unavailable);
            }
        }
    }

    /// The person said no, or stopped it: not opened again by itself.
    fn refuse(&mut self, e: &anyhow::Error) {
        warn!("input: {e:#}; not asked again until Prostir starts again");
        self.shared.refused.store(true, Ordering::Relaxed);
        self.set_state(State::Refused);
        self.say(News::Refused);
    }

    /// Not opened again for a while, longer each time.
    fn later(&mut self, now: Instant) {
        self.reopen = (now + self.reopen.1, (self.reopen.1 * 2).min(REOPEN_MAX));
    }

    fn event(&mut self, ev: Ev, now: Instant) {
        let from = match &ev {
            Ev::To { from, .. }
            | Ev::Button { from, .. }
            | Ev::Scroll { from, .. }
            | Ev::Touch { from, .. }
            | Ev::Release { from } => *from,
        };
        self.held.heard(from, now);
        match ev {
            Ev::Release { from } => {
                self.release(from);
                return;
            }
            // Pressed on a display gone meanwhile: nowhere to press.
            Ev::Button {
                at: None,
                down: true,
                ..
            } => return,
            _ => {}
        }
        if !self.devices(now) {
            return;
        }
        let touches = self.injector.as_ref().is_some_and(|i| i.touches());
        let acts = match ev {
            Ev::To { at, .. } => vec![Act::To(at)],
            Ev::Button {
                at, button, down, ..
            } => {
                let to = at.map(Act::To);
                let press = self.held.button(from, button, down, now);
                to.into_iter().chain(press).collect()
            }
            Ev::Scroll { dx, dy, .. } => vec![Act::Scroll(dx, dy)],
            Ev::Touch {
                slot,
                phase,
                at,
                came,
                ..
            } => self.touch(from, slot, phase, at, touches, came),
            Ev::Release { .. } => Vec::new(),
        };
        self.act(acts);
    }

    /// A finger: a touch on the desktop where it has them, else mouse actions.
    fn touch(
        &mut self,
        from: SessionId,
        slot: u8,
        phase: TouchPhase,
        at: Option<Spot>,
        touches: bool,
        now: Instant,
    ) -> Vec<Act> {
        let Some(at) = at else {
            // On a display gone: lifted without counting, where it was.
            let acts = if touches {
                self.held.lift(from, slot).into_iter().collect()
            } else {
                self.mice
                    .get_mut(&from)
                    .map(TouchMouse::cancel)
                    .unwrap_or_default()
            };
            return self.through_held(from, acts, now);
        };
        if touches {
            return self
                .held
                .touch(from, slot, phase, at, now)
                .into_iter()
                .collect();
        }
        let acts = self
            .mice
            .entry(from)
            .or_insert_with(|| TouchMouse::new(touch_mouse::FEEL))
            .touch(slot, phase, &at, now);
        self.through_held(from, acts, now)
    }

    /// Buttons of mouse actions go through what is held, so they are let go too.
    fn through_held(&mut self, from: SessionId, acts: Vec<Act>, now: Instant) -> Vec<Act> {
        acts.into_iter()
            .filter_map(|a| match a {
                Act::Button(b, down) => self.held.button(from, b, down, now),
                other => Some(other),
            })
            .collect()
    }

    /// Everything `from` holds, let go.
    fn release(&mut self, from: SessionId) {
        let now = Instant::now();
        // The mouse at an edge is pushed on no more (by whom it was put there).
        self.edge.stop();
        let dragged = self
            .mice
            .remove(&from)
            .map(|mut m| m.cancel())
            .unwrap_or_default();
        let mut acts = self.through_held(from, dragged, now);
        acts.extend(self.held.release_all(from));
        if !acts.is_empty() {
            info!("input: letting go of what session {from} held");
            self.act(acts);
        }
    }

    /// Tells the desktop `acts`; devices that fail are gone, and what they held. What
    /// they did to the pointer is said after them.
    fn act(&mut self, acts: Vec<Act>) {
        let Some(injector) = self.injector.as_mut() else {
            return;
        };
        let mut pointing = None;
        for a in acts {
            match &a {
                Act::To(at) if injector.relative() => self.edge.moved(at, Instant::now()),
                Act::To(_) => {}
                Act::Touch(..) => self.edge.stop(),
                _ => {}
            }
            let done = match &a {
                Act::To(at) => injector.pointer_to(at),
                Act::Button(b, down) => injector.button(*b, *down),
                Act::Scroll(dx, dy) => injector.scroll(*dx, *dy),
                Act::Touch(id, phase, at) => injector.touch(*id, *phase, at),
                Act::Key(code, down) => injector.key(*code, *down),
            };
            if let Err(e) = done {
                self.lost(&e);
                return;
            }
            pointing = match a {
                Act::Touch(_, TouchPhase::Down | TouchPhase::Move, _) => Some(Pointing::Touch),
                Act::Touch(..) if !self.held.touching() => Some(Pointing::Lifted),
                // A button let go moves nothing (as when everything is let go).
                Act::To(_) | Act::Button(_, true) | Act::Scroll(..) => Some(Pointing::Mouse),
                Act::Touch(..) | Act::Button(_, false) | Act::Key(..) => pointing,
            };
        }
        if let Some(p) = pointing {
            self.point(p);
        }
    }

    fn lost(&mut self, e: &anyhow::Error) {
        warn!("input: the desktop's virtual devices are gone: {e:#}");
        self.injector = None;
        self.held.clear();
        self.mice.clear();
        self.edge.stop();
        self.fingers_gone();
        if refused(e) {
            self.refuse(e);
        } else {
            self.later(Instant::now());
        }
    }

    fn tick(&mut self, now: Instant) {
        self.opened(now);
        for from in self.held.quiet(now) {
            info!("input: nothing from session {from} for a while");
            self.release(from);
        }
        let coasting: Vec<(SessionId, Vec<Act>)> = self
            .mice
            .iter_mut()
            .filter(|(_, m)| m.moving())
            .map(|(s, m)| (*s, m.tick(now)))
            .collect();
        for (_, acts) in coasting {
            self.act(acts);
        }
        // Pushed only by a device whose relative moves the desktop takes as a mouse's.
        if let Some((dx, dy)) = self.edge.due(now)
            && let Some(injector) = self.injector.as_mut()
            && injector.relative()
            && let Err(e) = injector.pointer_by(dx, dy)
        {
            self.lost(&e);
        }
        if let Some(injector) = self.injector.as_mut()
            && let Err(e) = injector.pump()
        {
            self.lost(&e);
        }
    }
}

/// Whether `e` is the person's no (to the dialog, or by stopping the devices).
fn refused(e: &anyhow::Error) -> bool {
    e.downcast_ref::<InputRefused>().is_some() || e.downcast_ref::<InputStopped>().is_some()
}

/// Consecutive moves of one session as the last of them: the mouse goes where it was
/// sent last, and nothing in between is lost (buttons, scrolls and touches stay in
/// order, each between the moves around it).
fn coalesce(batch: Vec<Msg>) -> Vec<Msg> {
    let mut out: Vec<Msg> = Vec::with_capacity(batch.len());
    for m in batch {
        if let (Msg::Ev(Ev::To { from, .. }), Some(Msg::Ev(Ev::To { from: last, .. }))) =
            (&m, out.last())
            && from == last
        {
            out.pop();
        }
        out.push(m);
    }
    out
}

#[cfg(test)]
mod tests;
