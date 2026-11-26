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
//! are gone let go with them (the desktop does that).

pub mod held;
pub mod touch_mouse;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use log::{info, warn};
use prostir_proto::HostMsg;
use prostir_proto::state::ErrorCode;

use crate::events::SessionId;
use crate::i18n::t;
use crate::platform::{Injector, InputRefused, MouseButton, OpenInput, Spot, TouchPhase};
use crate::router::Router;
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
    /// it where it was).
    Touch {
        from: SessionId,
        slot: u8,
        phase: TouchPhase,
        at: Option<Spot>,
    },
    /// Everything the session holds, let go (its end, the headset taken off).
    Release { from: SessionId },
}

enum Msg {
    Opener(Option<Arc<dyn OpenInput>>),
    Ev(Ev),
}

/// The control loop's side of the input thread. Dropped, the thread lets go of
/// everything and closes the devices.
pub struct Input {
    tx: Sender<Msg>,
}

impl Input {
    /// The input thread, without devices until [`Input::set_opener`]; what goes wrong
    /// for the person is said to every headset through `router`.
    pub fn start(router: Arc<Router>) -> Self {
        let (tx, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("input".into())
            .spawn(move || Run::new(router).run(&rx));
        if let Err(e) = spawned {
            warn!("no thread for the input from the headset: {e}");
        }
        Self { tx }
    }

    /// How to open the desktop's devices (none: it has none).
    pub fn set_opener(&self, opener: Option<Arc<dyn OpenInput>>) {
        let _ = self.tx.send(Msg::Opener(opener));
    }

    pub fn send(&self, ev: Ev) {
        let _ = self.tx.send(Msg::Ev(ev));
    }
}

/// Opening the devices, on a thread of its own: what came of it.
type Opening = Receiver<anyhow::Result<Box<dyn Injector>>>;

struct Run {
    router: Arc<Router>,
    opener: Option<Arc<dyn OpenInput>>,
    injector: Option<Box<dyn Injector>>,
    opening: Option<Opening>,
    /// The person said no: not asked again.
    refused: bool,
    /// Not opened again before this, and the pause after it.
    reopen: (Instant, Duration),
    held: Held,
    mice: BTreeMap<SessionId, TouchMouse>,
}

impl Run {
    fn new(router: Arc<Router>) -> Self {
        Self {
            router,
            opener: None,
            injector: None,
            opening: None,
            refused: false,
            reopen: (Instant::now(), REOPEN_FIRST),
            held: Held::default(),
            mice: BTreeMap::new(),
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
            let first = match rx.recv_timeout(wait) {
                Ok(m) => Some(m),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            };
            // Devices opened meanwhile take these events.
            self.opened(Instant::now());
            let batch: Vec<Msg> = first.into_iter().chain(rx.try_iter()).collect();
            for m in coalesce(batch) {
                match m {
                    Msg::Opener(opener) => self.set_opener(opener),
                    Msg::Ev(ev) => self.event(ev, Instant::now()),
                }
            }
            self.tick(Instant::now());
        }
        for from in self.held.sessions() {
            self.release(from);
        }
        info!("input: the thread ends");
    }

    fn set_opener(&mut self, opener: Option<Arc<dyn OpenInput>>) {
        self.injector = None;
        self.opening = None;
        self.held.clear();
        self.mice.clear();
        self.refused = false;
        self.reopen = (Instant::now(), REOPEN_FIRST);
        self.opener = opener;
    }

    /// The devices, if open; else they start opening, where they may.
    fn devices(&mut self, now: Instant) -> bool {
        if self.injector.is_some() {
            return true;
        }
        if self.opening.is_some() || self.refused || now < self.reopen.0 {
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
            }
            Err(e) if e.downcast_ref::<InputRefused>().is_some() => {
                warn!("input: {e:#}; not asked again until Prostir starts again");
                self.refused = true;
                self.problem(ErrorCode::NOT_ALLOWED, t("pc-input-not-allowed"));
            }
            Err(e) => {
                warn!("input: the desktop's virtual devices did not open: {e:#}");
                self.later(now);
                self.problem(ErrorCode::DESKTOP, t("pc-input-unavailable"));
            }
        }
    }

    /// Not opened again for a while, longer each time.
    fn later(&mut self, now: Instant) {
        self.reopen = (now + self.reopen.1, (self.reopen.1 * 2).min(REOPEN_MAX));
    }

    fn problem(&self, error: ErrorCode, detail: &str) {
        self.router.send_all(&HostMsg::Problem {
            error,
            detail: detail.into(),
            display: None,
        });
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
        if let Ev::Release { from } = ev {
            self.release(from);
            return;
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
                slot, phase, at, ..
            } => self.touch(from, slot, phase, at, touches, now),
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
            // On a display gone: lifted without counting.
            let acts = if touches {
                Vec::new()
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

    /// Tells the desktop `acts`; devices that fail are gone, and what they held.
    fn act(&mut self, acts: Vec<Act>) {
        let Some(injector) = self.injector.as_mut() else {
            return;
        };
        for a in acts {
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
        }
    }

    fn lost(&mut self, e: &anyhow::Error) {
        warn!("input: the desktop's virtual devices are gone: {e:#}");
        self.injector = None;
        self.held.clear();
        self.mice.clear();
        self.later(Instant::now());
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
        if let Some(injector) = self.injector.as_mut()
            && let Err(e) = injector.pump()
        {
            self.lost(&e);
        }
    }
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
