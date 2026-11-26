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

use glam::Vec2;
use log::debug;
use prostir_proto::ClientMsg;
use prostir_proto::input::{Button, TouchPhase as Phase};
use prostir_proto::state::{caps, perms};

use super::{Effect, Model};
use crate::ids::{DisplayKey, PcId};
use crate::interaction::{MouseButton, PcInput, TouchPhase};
use crate::session::Session;

/// While a button or a finger stays pressed, its place is said again this often at
/// the least (the PC lets go of it after 2 s of nothing; see `PROTOCOL.md`, "Input
/// from the headset").
pub const HELD_EVERY: Duration = Duration::from_millis(500);

/// The fingers kept track of, by slot (the hands give one slot each).
const SLOTS: usize = 2;

/// A point across a monitor's picture (0 to 1 from the top left) as the wire says it.
pub fn wire_uv(uv: Vec2) -> (u16, u16) {
    let f = |v: f32| (v.clamp(0.0, 1.0) * 65535.0) as u16;
    (f(uv.x), f(uv.y))
}

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

/// What the headset holds pressed on a PC, as sent (see the module).
#[derive(Debug, Default)]
pub(super) struct Held {
    /// The mouse's buttons down (a bit for each, by its number on the wire).
    buttons: u8,
    /// Where the mouse is while one is.
    mouse: Option<Spot>,
    /// Each finger touching, by slot.
    fingers: [Option<Spot>; SLOTS],
}

impl Held {
    fn button(&mut self, id: DisplayKey, (u, v): (u16, u16), b: Button, down: bool, now: Instant) {
        let bit = 1u8 << (b as u8).min(7);
        if down {
            self.buttons |= bit;
        } else {
            self.buttons &= !bit;
        }
        self.mouse = (self.buttons != 0).then_some(Spot {
            id,
            u,
            v,
            said: now,
        });
    }

    /// The mouse went to `id` at `u`, `v`.
    fn moved(&mut self, id: DisplayKey, (u, v): (u16, u16), now: Instant) {
        if let Some(m) = &mut self.mouse
            && m.id.pc == id.pc
        {
            *m = Spot {
                id,
                u,
                v,
                said: now,
            };
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

    /// PC `pc` is gone: what was pressed on it went with it.
    pub fn forget(&mut self, pc: PcId) {
        if self.mouse.is_some_and(|m| m.id.pc == pc) {
            self.mouse = None;
            self.buttons = 0;
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
        if let Some(m) = &mut self.mouse {
            if !may(m.id.pc) {
                self.mouse = None;
                self.buttons = 0;
            } else if stale(m) {
                m.said = now;
                out(
                    m.id.pc,
                    ClientMsg::MouseTo {
                        id: m.id.id,
                        u: m.u,
                        v: m.v,
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

    /// Something is held pressed.
    #[cfg(test)]
    pub fn any(&self) -> bool {
        self.mouse.is_some() || self.fingers.iter().any(Option::is_some)
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

    /// A hand acts on a PC's monitor (see the module).
    pub(super) fn act_on_pc(&mut self, now: Instant, input: PcInput) {
        self.now = now;
        self.pc_acted = Some(now);
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

    /// What stays pressed on a PC is said again, as often as [`HELD_EVERY`].
    pub(super) fn keep_held(&mut self, now: Instant) {
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
        // A slot beyond the hands' is not kept.
        held.finger(9, k(PC, 1), (1, 1), Phase::Down, t0);
        assert!(!held.any());
    }
}
