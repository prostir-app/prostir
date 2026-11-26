//! What a hand does on a PC's monitor (`interaction::PcInput`) as messages to that PC
//! (plans/step-8-interaction.md, sections 3 and 4, step 8.4b): the mouse goes where
//! the PC lets the headset move it (feature 1, `MouseTo`, its pointer drawn there at
//! once); its buttons, the wheel and the fingers go only to a PC that names feature 15
//! and gives this headset the input right, else they are written to the log as
//! before. What stays pressed is said again while it does ([`HELD_EVERY`]), since the
//! PC lets go of what a headset holds after 2 s without a word.
//!
//! Nothing here takes memory, and nothing the menu shows changes (no `rev`).

use std::time::{Duration, Instant};

use log::debug;
use prostir_proto::ClientMsg;
use prostir_proto::input::{Button, TouchPhase as Phase};
use prostir_proto::state::{caps, perms};

use super::{Effect, Model};
use crate::ids::{DisplayKey, PcId};
use crate::interaction::{MouseButton, PcInput, TouchPhase, wire_uv};
use crate::session::Session;

/// While a button or a finger stays pressed, its place is said again this often at
/// the least (the PC lets go of it after 2 s of nothing; see `PROTOCOL.md`, "Input
/// from the headset").
pub const HELD_EVERY: Duration = Duration::from_millis(500);

/// The fingers kept track of, by slot (the hands give one slot each).
const SLOTS: usize = 2;

/// A scroll as a hand makes it (`dy` positive scrolls up) as the wire says it (`dy`
/// positive scrolls down); `dx` positive is to the right on both.
pub fn wire_scroll(dx: i16, dy: i16) -> (i32, i32) {
    (i32::from(dx), -i32::from(dy))
}

fn wire_button(b: MouseButton) -> Button {
    match b {
        MouseButton::Left => Button::Left,
        MouseButton::Right => Button::Right,
    }
}

fn wire_phase(p: TouchPhase) -> Phase {
    match p {
        TouchPhase::Down => Phase::Down,
        TouchPhase::Move => Phase::Move,
        TouchPhase::Up => Phase::Up,
        TouchPhase::Cancel => Phase::Cancel,
    }
}

/// Where something pressed is, and when that was last said.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Spot {
    id: DisplayKey,
    u: u16,
    v: u16,
    said: Instant,
}

/// The PCs whose mouse is kept track of at once (each PC has a mouse of its own).
const MICE: usize = 2;

/// One PC's mouse while one of its buttons is down.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Mouse {
    /// Its buttons down (a bit for each, by its number on the wire).
    buttons: u8,
    /// Where it is (on a display of its own PC).
    spot: Spot,
}

/// What the headset holds pressed on its PCs, as sent (see the module).
#[derive(Debug, Default)]
pub(super) struct Held {
    /// Each PC's mouse with a button down.
    mice: [Option<Mouse>; MICE],
    /// Each finger touching, by slot.
    fingers: [Option<Spot>; SLOTS],
}

impl Held {
    fn button(&mut self, id: DisplayKey, (u, v): (u16, u16), b: Button, down: bool, now: Instant) {
        let bit = 1u8 << (b as u8).min(7);
        let ours = |m: &Option<Mouse>| m.is_some_and(|m| m.spot.id.pc == id.pc);
        let i = (self.mice.iter().position(ours))
            .or_else(|| self.mice.iter().position(Option::is_none));
        let Some(slot) = i.and_then(|i| self.mice.get_mut(i)) else {
            return;
        };
        let was = slot.map_or(0, |m| m.buttons);
        let buttons = if down { was | bit } else { was & !bit };
        *slot = (buttons != 0).then_some(Mouse {
            buttons,
            spot: Spot {
                id,
                u,
                v,
                said: now,
            },
        });
    }

    /// The mouse of `id`'s PC went to `id` at `u`, `v`.
    fn moved(&mut self, id: DisplayKey, (u, v): (u16, u16), now: Instant) {
        for m in self.mice.iter_mut().flatten() {
            if m.spot.id.pc == id.pc {
                m.spot = Spot {
                    id,
                    u,
                    v,
                    said: now,
                };
            }
        }
    }

    fn finger(&mut self, slot: u8, id: DisplayKey, (u, v): (u16, u16), p: Phase, now: Instant) {
        let Some(f) = self.fingers.get_mut(usize::from(slot)) else {
            return;
        };
        *f = matches!(p, Phase::Down | Phase::Move).then_some(Spot {
            id,
            u,
            v,
            said: now,
        });
    }

    /// Nothing is pressed on PC `pc` any more (it is gone, or takes no input).
    pub fn forget(&mut self, pc: PcId) {
        for m in &mut self.mice {
            if m.is_some_and(|m| m.spot.id.pc == pc) {
                *m = None;
            }
        }
        for f in &mut self.fingers {
            if f.is_some_and(|s| s.id.pc == pc) {
                *f = None;
            }
        }
    }

    /// What is to be said again at `now` (see [`HELD_EVERY`]), each to `out` with its
    /// PC; `may(pc)`: that PC still takes the input (else it is forgotten there).
    fn due(
        &mut self,
        now: Instant,
        may: impl Fn(PcId) -> bool,
        mut out: impl FnMut(PcId, ClientMsg),
    ) {
        let stale = |s: &Spot| now.saturating_duration_since(s.said) >= HELD_EVERY;
        for slot in &mut self.mice {
            let Some(Mouse { spot: s, .. }) = slot else {
                continue;
            };
            if !may(s.id.pc) {
                *slot = None;
            } else if stale(s) {
                s.said = now;
                out(
                    s.id.pc,
                    ClientMsg::MouseTo {
                        id: s.id.id,
                        u: s.u,
                        v: s.v,
                    },
                );
            }
        }
        for (slot, f) in self.fingers.iter_mut().enumerate() {
            let Some(s) = f else { continue };
            if !may(s.id.pc) {
                *f = None;
            } else if stale(s) {
                s.said = now;
                out(
                    s.id.pc,
                    ClientMsg::Touch {
                        id: s.id.id,
                        slot: slot as u8,
                        u: s.u,
                        v: s.v,
                        phase: Phase::Move,
                    },
                );
            }
        }
    }

    /// Something is held pressed: a button, a finger.
    pub fn any(&self) -> bool {
        self.mice.iter().any(Option::is_some) || self.fingers.iter().any(Option::is_some)
    }
}

/// PC `pc` takes the headset's mouse buttons, wheel and fingers: in a session, it
/// names feature 15 and gives this headset the input right.
fn takes_input(session: &Session, pc: PcId) -> bool {
    session.pc(pc).is_some_and(|p| {
        p.state
            .caps
            .as_ref()
            .is_some_and(|c| c.has(caps::pc::INPUT))
            && p.state.access.is_some_and(|a| a.may(perms::INPUT))
    })
}

impl Model {
    fn takes_input(&self, pc: PcId) -> bool {
        takes_input(&self.session, pc)
    }

    /// A press on a monitor of PC `pc`, which takes input from the desktop (feature 1)
    /// but does not give this headset the right to it: the user is told once where it
    /// is turned on (until the PC gives the right, then takes it again).
    fn say_input_off(&mut self, pc: PcId) {
        let Some(p) = self.session.pc_mut(pc) else {
            return;
        };
        let moves = p
            .state
            .caps
            .as_ref()
            .is_some_and(|c| c.has(caps::pc::MOVE_MOUSE));
        let right = p.state.access.is_some_and(|a| a.may(perms::INPUT));
        if !moves || right || std::mem::replace(&mut p.input_off_said, true) {
            return;
        }
        let text = self.texts().t("headset-pc-input-off").to_owned();
        self.show_toast(text);
    }

    /// A hand acts on a PC's monitor (see the module).
    pub(super) fn act_on_pc(&mut self, now: Instant, input: PcInput) {
        self.now = now;
        self.pc_acted = Some(now);
        // Taken again: should the right go, the user is told again.
        if self.takes_input(input.pc())
            && let Some(p) = self.session.pc_mut(input.pc())
        {
            p.input_off_said = false;
        }
        match input {
            PcInput::Move { id, uv } => {
                if self.pc_moves_mouse(id.pc) {
                    let (u, v) = wire_uv(uv);
                    self.mouse_to(id, u, v);
                    self.held.moved(id, (u, v), now);
                }
            }
            PcInput::Touch {
                phase: TouchPhase::Move,
                ..
            } if !self.takes_input(input.pc()) => {}
            other if !self.takes_input(other.pc()) => {
                debug!("hands: {other:?} (the PC takes no input from the headset)");
                if matches!(
                    other,
                    PcInput::Button { down: true, .. }
                        | PcInput::Scroll { .. }
                        | PcInput::Touch {
                            phase: TouchPhase::Down,
                            ..
                        }
                ) {
                    self.say_input_off(other.pc());
                }
                // Let go there: nothing is held on it (pressed while it still took
                // input, the PC lets go by itself).
                if matches!(
                    other,
                    PcInput::Button { down: false, .. }
                        | PcInput::Touch {
                            phase: TouchPhase::Up | TouchPhase::Cancel,
                            ..
                        }
                ) {
                    self.held.forget(other.pc());
                }
            }
            PcInput::Button {
                id,
                uv,
                button,
                down,
            } => {
                let at = wire_uv(uv);
                let button = wire_button(button);
                self.send_to(
                    id.pc,
                    ClientMsg::MouseButton {
                        id: id.id,
                        u: at.0,
                        v: at.1,
                        button,
                        down,
                    },
                );
                // The PC moves the mouse there first: its pointer is drawn there now.
                self.cursors.moved_here(id.pc, id.id, at.0, at.1, now);
                self.held.button(id, at, button, down, now);
            }
            PcInput::Scroll { id, dx, dy } => {
                let (dx, dy) = wire_scroll(dx, dy);
                self.send_to(id.pc, ClientMsg::MouseScroll { id: id.id, dx, dy });
            }
            PcInput::Touch {
                id,
                slot,
                uv,
                phase,
            } => {
                let at = wire_uv(uv);
                let phase = wire_phase(phase);
                self.send_to(
                    id.pc,
                    ClientMsg::Touch {
                        id: id.id,
                        slot,
                        u: at.0,
                        v: at.1,
                        phase,
                    },
                );
                self.held.finger(slot, id, at, phase, now);
            }
        }
    }

    /// What stays pressed on a PC is said again, as often as [`HELD_EVERY`]. While
    /// something is, the hands act on the PC (the mouse does not follow the gaze).
    pub(super) fn keep_held(&mut self, now: Instant) {
        if self.held.any() {
            self.pc_acted = Some(now);
        }
        let (held, effects, session) = (&mut self.held, &mut self.effects, &self.session);
        held.due(
            now,
            |pc| takes_input(session, pc),
            |pc, msg| effects.push(Effect::Send(pc, msg)),
        );
    }
}

impl PcInput {
    /// The PC acted on.
    fn pc(&self) -> PcId {
        match *self {
            PcInput::Move { id, .. }
            | PcInput::Button { id, .. }
            | PcInput::Scroll { id, .. }
            | PcInput::Touch { id, .. } => id.pc,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec2;
    use prostir_proto::state::DisplayId;

    const PC: PcId = PcId([3; 8]);
    const OTHER: PcId = PcId([4; 8]);

    fn k(pc: PcId, n: u32) -> DisplayKey {
        DisplayKey::new(pc, DisplayId(n))
    }

    #[test]
    fn points_and_scrolls_as_the_wire_says_them() {
        assert_eq!(wire_uv(Vec2::new(0.0, 1.0)), (0, 65535));
        assert_eq!(wire_uv(Vec2::new(-0.5, 2.0)), (0, 65535));
        assert_eq!(wire_uv(Vec2::new(0.5, 0.25)), (32767, 16383));
        // A hand moving up (a notch up) scrolls up: the wire's dy is below 0.
        assert_eq!(wire_scroll(0, 120), (0, -120));
        assert_eq!(wire_scroll(-240, -60), (-240, 60));
        assert_eq!(wire_scroll(i16::MIN, i16::MIN), (-32768, 32768));
    }

    /// A button held, a finger touching: said again every [`HELD_EVERY`], where they
    /// last were; let go, or their PC gone or no longer taking input, nothing more.
    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "a test's own starting time; the code under test takes the time as a parameter"
    )]
    fn what_stays_pressed_is_said_again() {
        let t0 = Instant::now();
        let mut held = Held::default();
        let mut said = Vec::new();
        let mut due = |held: &mut Held, at: Duration, may: bool| {
            said.clear();
            held.due(t0 + at, |_| may, |pc, m| said.push((pc, m)));
            std::mem::take(&mut said)
        };
        assert!(due(&mut held, Duration::ZERO, true).is_empty());
        held.button(k(PC, 1), (10, 20), Button::Left, true, t0);
        held.finger(1, k(OTHER, 2), (5, 6), Phase::Down, t0);
        assert!(due(&mut held, Duration::from_millis(400), true).is_empty());
        let ms = Duration::from_millis;
        held.moved(k(PC, 2), (11, 21), t0 + ms(100));
        assert_eq!(
            due(&mut held, ms(550), true),
            [(
                OTHER,
                ClientMsg::Touch {
                    id: DisplayId(2),
                    slot: 1,
                    u: 5,
                    v: 6,
                    phase: Phase::Move
                }
            )]
        );
        assert_eq!(
            due(&mut held, ms(650), true),
            [(
                PC,
                ClientMsg::MouseTo {
                    id: DisplayId(2),
                    u: 11,
                    v: 21
                }
            )]
        );
        // Another button down and up: the left one still holds.
        held.button(k(PC, 2), (11, 21), Button::Right, true, t0 + ms(700));
        held.button(k(PC, 2), (11, 21), Button::Right, false, t0 + ms(700));
        assert_eq!(due(&mut held, ms(1250), true).len(), 2);
        held.button(k(PC, 2), (11, 21), Button::Left, false, t0 + ms(1300));
        held.finger(1, k(OTHER, 2), (5, 6), Phase::Cancel, t0 + ms(1300));
        assert!(!held.any());
        assert!(due(&mut held, ms(3000), true).is_empty());
        // Gone with its PC, or once it takes no input.
        held.button(k(PC, 1), (1, 1), Button::Left, true, t0);
        held.finger(0, k(OTHER, 1), (1, 1), Phase::Down, t0);
        held.forget(PC);
        assert_eq!(due(&mut held, ms(600), true).len(), 1);
        assert!(due(&mut held, ms(1200), false).is_empty());
        assert!(!held.any());
        // A button held on each of two PCs: each said again on its own; one let go,
        // the other still.
        held.button(k(PC, 1), (1, 2), Button::Left, true, t0);
        held.button(k(OTHER, 3), (3, 4), Button::Left, true, t0);
        held.button(k(PC, 1), (1, 2), Button::Right, true, t0);
        let mut pcs: Vec<PcId> = due(&mut held, ms(600), true).iter().map(|s| s.0).collect();
        pcs.sort();
        assert_eq!(pcs, [PC, OTHER]);
        held.button(k(PC, 1), (1, 2), Button::Left, false, t0 + ms(700));
        held.button(k(OTHER, 3), (3, 4), Button::Left, false, t0 + ms(700));
        assert_eq!(
            due(&mut held, ms(1300), true),
            [(
                PC,
                ClientMsg::MouseTo {
                    id: DisplayId(1),
                    u: 1,
                    v: 2
                }
            )],
            "the right button of PC still"
        );
        held.forget(PC);
        // A slot beyond the hands' is not kept.
        held.finger(9, k(PC, 1), (1, 1), Phase::Down, t0);
        assert!(!held.any());
    }
}
