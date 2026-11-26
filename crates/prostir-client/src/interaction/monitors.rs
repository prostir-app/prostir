//! What step 8 adds to the hands (plans/step-8-interaction.md, sections 2 and 3), while
//! the scene's [`Modes`] say so: a fingertip touches a PC's monitor like a touchscreen
//! (hover in front, a press as it crosses the surface, the point pinned until the
//! finger slides, letting go as it comes back, cancelled when pushed through, the drawn
//! hand stopping on the surface); a pinch or a trigger on a monitor is the mouse's
//! button there (the pointer frozen a moment from the press, then a drag); each hand
//! switches between the two by how near its fingertip is to a surface it can touch,
//! with hysteresis; a hand takes a monitor with a fist, a middle pinch or a pinch held
//! still ([`Take`]); a middle pinch scrolls or right-clicks, a controller's stick
//! scrolls; one pointer: the ray of the hand that pressed last moves the PC's mouse.
//!
//! What goes to the PC is a [`PcInput`] said to the scene; how it reaches the PC (and
//! whether it may) is the model's. With the modes off nothing here acts and the hands
//! are as before step 8.

#![forbid(unsafe_code)]

use std::time::Instant;

use glam::{Quat, Vec2, Vec3};

use super::signals::{self, Gestures};
use super::trial::Movement;
use super::{Feel, FrameInput, Hands, Head, INDEX_TIP, Joints, Mark, Phase, Scene};
use crate::ids::{DisplayKey, Target, Thing};
use crate::layout::{Grab, Ray};
use crate::world::{panel_height, panel_width};

use super::feel::ms;

/// How a hand takes a monitor (with [`Modes::monitors`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Take {
    /// All fingers curled.
    Fist,
    /// Thumb and middle finger together.
    Middle,
    /// An index pinch held still a moment.
    #[default]
    Hold,
}

/// What the hands may do beyond the input before step 8 (see the module).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modes {
    /// Monitors are touched and clicked (see the module).
    pub monitors: bool,
    /// How a hand takes a monitor then.
    pub take: Take,
    /// Tracked hands act ("Hands: control"): only then does a fingertip touch.
    pub hands: bool,
    /// How a monitor in a hand moves (whether or not monitors are touched).
    pub movement: Movement,
}

/// A mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
}

/// How a touch goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchPhase {
    Down,
    Move,
    Up,
    /// Pushed through, the tracking lost, the hands no longer the app's: not a tap.
    Cancel,
}

/// What a hand does to a PC's monitor (`uv`: 0 to 1 from the top left of its picture).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PcInput {
    /// The mouse to there.
    Move { id: DisplayKey, uv: Vec2 },
    /// A button down or up there.
    Button {
        id: DisplayKey,
        uv: Vec2,
        button: MouseButton,
        down: bool,
    },
    /// Scrolled, in 120ths of a notch, as a wheel: `dy` positive scrolls up (a hand
    /// moving up, the stick pushed forward), `dx` positive to the right.
    Scroll { id: DisplayKey, dx: i16, dy: i16 },
    /// A fingertip's touch, one slot for each hand.
    Touch {
        id: DisplayKey,
        slot: u8,
        uv: Vec2,
        phase: TouchPhase,
    },
}

/// A point against a PC's monitor (a fingertip near it), as the scene finds it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Touched {
    pub id: DisplayKey,
    /// The flat-equivalent point on it (as `Hit::local`).
    pub local: Vec2,
    /// How far in front (negative: behind).
    pub z: f32,
    /// The point of its surface under it, and the normal there.
    pub point: Vec3,
    pub normal: Vec3,
    /// Where on its picture.
    pub uv: Vec2,
}

/// What a hand holds, and what moves it.
#[derive(Debug, Clone, Copy)]
pub(super) struct Carry {
    pub grab: Grab,
    /// Moved by the ray from the shoulder through the palm, as long as this gesture is
    /// held (a fist or a middle pinch); `None`: by the hand's own ray, held by its
    /// pinch or grip.
    pub palm: Option<Take>,
    /// The palm's ray turned by this, so it pointed at the point taken when it was
    /// taken (the monitor does not jump to where the palm points).
    turn: Quat,
    /// Since when the hand is not seen (it is held where it is a moment).
    lost: Option<Instant>,
}

impl Carry {
    pub fn by_ray(grab: Grab) -> Self {
        Self {
            grab,
            palm: None,
            turn: Quat::IDENTITY,
            lost: None,
        }
    }

    /// The ray from the shoulder through the palm of `j`, turned as when taken.
    fn palm_ray(&self, j: &Joints, head: Head, left: bool, feel: &Feel) -> Ray {
        let r = signals::palm_ray(j, head.position, head.forward, left, feel);
        Ray {
            origin: r.origin,
            dir: self.turn * r.dir,
        }
    }
}

/// A fingertip pressed on a monitor.
#[derive(Debug, Clone, Copy)]
pub(super) struct Poke {
    id: DisplayKey,
    since: Instant,
    pin_local: Vec2,
    pin_point: Vec3,
    normal: Vec3,
    /// The deepest it went, for letting go as it comes back.
    deepest: f32,
    /// Slid past the pin: the touch moves.
    moved: bool,
    /// Where the touch is now (the pin until it moved).
    uv: Vec2,
    /// Since when the fingertip is not seen (the tracking lost).
    lost: Option<Instant>,
}

/// A pinch or a trigger down on a monitor.
#[derive(Debug, Clone, Copy)]
pub(super) struct RayPress {
    id: DisplayKey,
    since: Instant,
    dir0: Vec3,
    uv0: Vec2,
    /// Where the pointer is now.
    uv: Vec2,
    /// The button went down on the PC (a hand's waits for what the press becomes).
    down: bool,
    /// Past the freeze: the pointer follows the ray.
    dragging: bool,
    /// Since when its ray is not seen.
    lost: Option<Instant>,
}

/// A middle pinch on a monitor: scrolling by moving the hand, or a right click.
#[derive(Debug, Clone, Copy)]
pub(super) struct Scroll {
    id: DisplayKey,
    uv: Vec2,
    at: Vec3,
    start_y: f32,
    last_y: f32,
    moved: bool,
    acc: f32,
    /// Since when the hand is not seen.
    lost: Option<Instant>,
}

/// A fingertip near a monitor this frame, for the shell to draw.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NearMonitor {
    /// The point of the surface under the fingertip (the pin while pressed).
    pub point: Vec3,
    pub normal: Vec3,
    /// How far in front.
    pub z: f32,
    pub pressed: bool,
    /// How far a press held still has come to a long press (0 to 1).
    pub held: f32,
}

/// What one hand has of step 8 from frame to frame, and what it shows this frame.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct HandMonitors {
    /// The fingertip touches, the ray is off (see [`Feel::switch_touch_m`]).
    pub touch_mode: bool,
    gestures: Gestures,
    /// What the hand last pointed at or touched on a monitor, and when (a fist or a
    /// middle pinch takes it a moment after).
    latched: Option<(DisplayKey, Vec2, Vec3, Instant)>,
    /// The monitor under the fingertip in the last frame and how far in front.
    last_z: Option<(DisplayKey, f32)>,
    stick_acc: f32,
    // ---- This frame ----
    /// Where it points or presses on a monitor (for the mouse): the ray's direction
    /// (none: not a ray).
    point: Option<(DisplayKey, Vec2, Option<Vec3>)>,
    near: Option<NearMonitor>,
    progress: Option<(Vec3, f32)>,
    push: Option<Vec3>,
    ray: Option<(Ray, f32)>,
    mouse: bool,
}

/// Step 8's state of both hands together.
#[derive(Debug, Default)]
pub(super) struct Shared {
    /// The time of the frame (for cancelling between frames).
    pub now: Option<Instant>,
    pub modes: Modes,
    /// The hand that pressed last moves the mouse; the other only points.
    mouse_hand: Option<usize>,
    /// Where the mouse was last sent: the monitor, the point, the ray's direction, when.
    last_mouse: Option<(DisplayKey, Vec2, Option<Vec3>, Instant)>,
}

impl Shared {
    pub fn begin(&mut self, now: Instant) {
        self.now = Some(now);
    }
}

/// Degrees between two directions.
fn degrees(a: Vec3, b: Vec3) -> f32 {
    a.angle_between(b).to_degrees()
}

impl Hands {
    // ---- What the shell draws ----

    /// What ray hand `h` shows this frame, and how long (none while its fingertip
    /// touches).
    pub fn ray(&self, h: usize) -> Option<(Ray, f32)> {
        self.hands[h].m.ray
    }

    /// Hand `h`'s fingertip near a monitor this frame.
    pub fn near_monitor(&self, h: usize) -> Option<NearMonitor> {
        self.hands[h].m.near
    }

    /// A ring closing in on a point as a press is held (to take, to long press): where,
    /// and how far it came (0 to 1).
    pub fn progress(&self, h: usize) -> Option<(Vec3, f32)> {
        self.hands[h].m.progress
    }

    /// How far hand `h` is drawn moved, so it stops on the monitor it presses rather
    /// than going into it.
    pub fn push(&self, h: usize) -> Option<Vec3> {
        self.hands[h].m.push
    }

    /// Hand `h`'s ray is the PC's mouse this frame, its pointer drawn by the headset:
    /// no dot of its own.
    pub fn is_mouse(&self, h: usize) -> bool {
        self.hands[h].m.mouse
    }

    /// The gestures hand `h` makes (for "For development").
    pub fn gestures(&self, h: usize) -> (bool, bool) {
        let g = self.hands[h].m.gestures;
        (g.middle.down, g.fist.down)
    }

    // ---- A frame ----

    /// Each hand's gestures, and whether its fingertip touches (its ray off) or its ray
    /// points: nearer than [`Feel::switch_touch_m`] to a surface it can touch, touch;
    /// further than [`Feel::switch_ray_m`], the ray. Never in the middle of a press.
    pub(super) fn switch(&mut self, input: &FrameInput, scene: &impl Scene) {
        let f = self.feel;
        let modes = self.m.modes;
        for h in 0..2 {
            let hs = input.hands[h];
            let joints = input.joints[h].filter(|_| modes.monitors && modes.hands && hs.is_hand);
            let hand = &mut self.hands[h];
            hand.m.point = None;
            hand.m.near = None;
            hand.m.progress = None;
            hand.m.push = None;
            hand.m.mouse = false;
            hand.m.gestures.update(joints.as_ref(), &f);
            let Some(j) = joints else {
                if !matches!(hand.phase, Phase::Poke(_)) {
                    hand.m.touch_mode = false;
                }
                continue;
            };
            match hand.phase {
                Phase::Poke(_) => {
                    hand.m.touch_mode = true;
                    continue;
                }
                Phase::Idle | Phase::Touching { .. } => {}
                _ => continue,
            }
            let tip = j[INDEX_TIP].0;
            let mut near = scene
                .touchable(tip, None)
                .filter(|t| t.z > -f.touch_through_m)
                .map_or(f32::INFINITY, |t| t.z.max(0.0));
            if let Some(m) = scene.menu() {
                let large = scene.large_menu();
                let l = m.rotation.inverse() * (tip - m.position);
                if l.x.abs() <= panel_width(large) / 2.0
                    && l.y.abs() <= panel_height(large) / 2.0
                    && l.z > -f.poke_behind_m
                {
                    near = near.min(l.z.max(0.0));
                }
            }
            if near < f.switch_touch_m {
                hand.m.touch_mode = true;
            } else if near > f.switch_ray_m {
                hand.m.touch_mode = false;
            }
        }
    }

    /// The fingertips against the monitors (see the module), for hands that touch.
    pub(super) fn touch_monitors(
        &mut self,
        now: Instant,
        input: &FrameInput,
        scene: &mut impl Scene,
    ) {
        let on = self.m.modes.monitors && self.m.modes.hands;
        let f = self.feel;
        for h in 0..2 {
            let hand = &mut self.hands[h];
            let poke = match hand.phase {
                Phase::Poke(p) if on => Some(p),
                Phase::Idle if on && hand.m.touch_mode && self.poke_hover[h].is_none() => None,
                // Not looked at this frame: a touch then starts only from in front,
                // seen in the frames right before.
                _ => {
                    hand.m.last_z = None;
                    continue;
                }
            };
            let tip = input.joints[h]
                .filter(|_| input.hands[h].is_hand)
                .map(|j| j[INDEX_TIP].0);
            let Some(tip) = tip else {
                // Not seen: a press waits a moment for the hand to come back.
                if let Some(mut p) = poke {
                    let lost = *p.lost.get_or_insert(now);
                    if now.saturating_duration_since(lost) > ms(f.lost_grace_ms) {
                        self.end_poke(h, now, p, TouchPhase::Cancel, scene);
                    } else {
                        self.hands[h].phase = Phase::Poke(p);
                    }
                }
                self.hands[h].m.last_z = None;
                continue;
            };
            let under = scene.touchable(tip, poke.map(|p| p.id));
            let Some(mut p) = poke else {
                self.hover_monitor(h, now, under, scene);
                continue;
            };
            p.lost = None;
            let Some(t) = under else {
                // Slid off its monitor: let go there.
                self.end_poke(h, now, p, TouchPhase::Up, scene);
                continue;
            };
            p.deepest = p.deepest.min(t.z);
            if t.z < -f.touch_through_m {
                self.end_poke(h, now, p, TouchPhase::Cancel, scene);
                continue;
            }
            if t.z > p.deepest + f.touch_recoil_m || t.z > f.touch_release_m {
                self.end_poke(h, now, p, TouchPhase::Up, scene);
                continue;
            }
            if !p.moved && t.local.distance(p.pin_local) > f.touch_pin_m {
                p.moved = true;
            }
            if p.moved && t.uv != p.uv {
                p.uv = t.uv;
                scene.pc(
                    now,
                    PcInput::Touch {
                        id: p.id,
                        slot: h as u8,
                        uv: p.uv,
                        phase: TouchPhase::Move,
                    },
                );
            }
            let held = now.saturating_duration_since(p.since);
            let hand = &mut self.hands[h];
            hand.m.latched = Some((p.id, p.uv, t.point, now));
            hand.m.near = Some(NearMonitor {
                point: if p.moved { t.point } else { p.pin_point },
                normal: p.normal,
                z: t.z,
                pressed: true,
                held: (held.as_secs_f32() / (f.long_press_ms / 1000.0)).min(1.0),
            });
            // The drawn hand stops on the surface.
            if t.z < 0.0 {
                hand.m.push = Some(-t.normal * t.z);
            }
            hand.phase = Phase::Poke(p);
        }
    }

    /// A fingertip in front of a monitor, not pressing: a hover, or a press as it
    /// crosses the surface coming from in front.
    fn hover_monitor(
        &mut self,
        h: usize,
        now: Instant,
        under: Option<Touched>,
        scene: &mut impl Scene,
    ) {
        let f = self.feel;
        let hand = &mut self.hands[h];
        let Some(t) = under else {
            hand.m.last_z = None;
            return;
        };
        let came_from = hand.m.last_z.filter(|(id, _)| *id == t.id).map(|(_, z)| z);
        hand.m.last_z = Some((t.id, t.z));
        if t.z > f.touch_hover_m || t.z < -f.touch_through_m {
            return;
        }
        hand.m.latched = Some((t.id, t.uv, t.point, now));
        let settled = hand
            .touched_at
            .is_none_or(|at| now.saturating_duration_since(at) > ms(f.touch_settle_ms));
        let surface = -f.touch_depth_m;
        let crossed = t.z <= surface
            && t.z > surface - f.touch_enter_m
            && came_from.is_some_and(|c| c > surface);
        if crossed && settled {
            hand.touched_at = Some(now);
            hand.phase = Phase::Poke(Poke {
                id: t.id,
                since: now,
                pin_local: t.local,
                pin_point: t.point,
                normal: t.normal,
                deepest: t.z,
                moved: false,
                uv: t.uv,
                lost: None,
            });
            hand.m.near = Some(NearMonitor {
                point: t.point,
                normal: t.normal,
                z: t.z,
                pressed: true,
                held: 0.0,
            });
            self.m.mouse_hand = Some(h);
            self.rings
                .push((t.point + t.normal * 0.003, now, Mark::Touch));
            scene.pc(
                now,
                PcInput::Touch {
                    id: t.id,
                    slot: h as u8,
                    uv: t.uv,
                    phase: TouchPhase::Down,
                },
            );
            return;
        }
        hand.m.near = Some(NearMonitor {
            point: t.point,
            normal: t.normal,
            z: t.z,
            pressed: false,
            held: 0.0,
        });
    }

    fn end_poke(
        &mut self,
        h: usize,
        now: Instant,
        p: Poke,
        phase: TouchPhase,
        scene: &mut impl Scene,
    ) {
        let hand = &mut self.hands[h];
        hand.phase = Phase::Idle;
        hand.touched_at = Some(now);
        hand.m.last_z = None;
        scene.pc(
            now,
            PcInput::Touch {
                id: p.id,
                slot: h as u8,
                uv: p.uv,
                phase,
            },
        );
    }

    /// Hand `h` with step 8's modes on: a press, a scroll or a carry of its own on a
    /// monitor, or starting one. Whether it was this, not the input before step 8.
    pub(super) fn monitor_hand(
        &mut self,
        now: Instant,
        h: usize,
        input: &FrameInput,
        head: Head,
        scene: &mut impl Scene,
    ) -> bool {
        let modes = self.m.modes;
        if !modes.monitors {
            return false;
        }
        let f = self.feel;
        let hs = input.hands[h];
        // The same as the input before step 8: the source changing, a hand that just
        // touched pinching.
        let switched = self.hands[h].was_hand.is_some_and(|was| was != hs.is_hand);
        let just_touched = hs.is_hand
            && self.hands[h]
                .touched_at
                .is_some_and(|t| now.saturating_duration_since(t) < f.after_touch());
        match self.hands[h].phase {
            Phase::Poke(_) => {
                self.hands[h].was_hand = Some(hs.is_hand);
                return true;
            }
            Phase::RayPress(p) => {
                self.hands[h].was_hand = Some(hs.is_hand);
                self.ray_press(now, h, input, head, p, switched, scene);
                return true;
            }
            Phase::Scroll(s) => {
                self.hands[h].was_hand = Some(hs.is_hand);
                self.scroll(now, h, input, s, switched, scene);
                return true;
            }
            Phase::Carry(c) if c.palm.is_some() => {
                self.hands[h].was_hand = Some(hs.is_hand);
                self.palm_carry(now, h, input, head, c, switched, scene);
                return true;
            }
            Phase::Idle => {}
            _ => return false,
        }
        // What the ray points at on a monitor (not a detached window).
        let on = match self.picks[h] {
            Some(Target {
                thing: Thing::Display(id),
                hit,
            }) if !scene.is_window(id) => scene
                .monitor_uv(id, hit.local)
                .map(|uv| (id, uv, hit.point)),
            _ => None,
        };
        if let Some((id, uv, point)) = on {
            let hand = &mut self.hands[h].m;
            hand.latched = Some((id, uv, point, now));
            hand.point = Some((id, uv, hs.ray.map(|r| r.dir)));
        }
        // A fist or a middle pinch: takes the monitor pointed at, or a moment ago.
        if hs.is_hand && modes.hands && self.take_gesture(now, h, input, head, on, scene) {
            self.hands[h].was_hand = Some(hs.is_hand);
            return true;
        }
        let Some((id, uv, point)) = on else {
            return false;
        };
        // A pinch or a trigger: the mouse's button.
        if hs.select.pressed && !just_touched && !switched {
            self.hands[h].was_hand = Some(hs.is_hand);
            // A hand's button waits for what the press becomes (a click, a drag, a
            // take, the fist it closes into); a controller's goes down at once.
            let wait = hs.is_hand;
            let dir0 = hs.ray.map_or(Vec3::NEG_Z, |r| r.dir);
            self.hands[h].phase = Phase::RayPress(RayPress {
                id,
                since: now,
                dir0,
                uv0: uv,
                uv,
                down: !wait,
                dragging: false,
                lost: None,
            });
            self.m.mouse_hand = Some(h);
            self.rings.push((point, now, Mark::Touch));
            if !wait {
                scene.pc(
                    now,
                    PcInput::Button {
                        id,
                        uv,
                        button: MouseButton::Left,
                        down: true,
                    },
                );
            }
            return true;
        }
        // A controller's stick scrolls what it points at.
        if !hs.is_hand {
            let y = if hs.stick.y.abs() < f.stick_dead {
                0.0
            } else {
                hs.stick.y
            };
            let hand = &mut self.hands[h].m;
            hand.stick_acc += y * f.stick_notches_per_s * input.dt;
            let n = hand.stick_acc.trunc();
            if n != 0.0 {
                hand.stick_acc -= n;
                scene.pc(
                    now,
                    PcInput::Scroll {
                        id,
                        dx: 0,
                        dy: (n * 120.0) as i16,
                    },
                );
            }
        }
        false
    }

    /// A pinch or a trigger down on a monitor: the pointer frozen where it pressed
    /// for [`Feel::freeze_ms`] or until the ray turns [`Feel::freeze_deg`], then it
    /// drags. A hand's button waits: moved past the freeze,
    /// it goes down where it pressed; let go, a click (a right one held long with the
    /// middle pinch take); held still, "hold" takes the monitor.
    #[allow(clippy::too_many_arguments, reason = "the frame's parts, each needed")]
    fn ray_press(
        &mut self,
        now: Instant,
        h: usize,
        input: &FrameInput,
        head: Head,
        mut p: RayPress,
        switched: bool,
        scene: &mut impl Scene,
    ) {
        let f = self.feel;
        let hs = input.hands[h];
        let take = self.m.modes.take;
        let held = now.saturating_duration_since(p.since);
        let ray = hs.ray;
        let lost = match ray {
            Some(_) => {
                p.lost = None;
                false
            }
            None => {
                let since = *p.lost.get_or_insert(now);
                now.saturating_duration_since(since) > ms(f.lost_grace_ms)
            }
        };
        let let_go = !hs.select.down || switched || lost;
        if let_go {
            self.hands[h].phase = Phase::Idle;
            let button = MouseButton::Left;
            if p.down {
                scene.pc(
                    now,
                    PcInput::Button {
                        id: p.id,
                        uv: p.uv,
                        button,
                        down: false,
                    },
                );
            } else if hs.select.released {
                let right = hs.is_hand && take == Take::Middle && held >= ms(f.long_press_ms);
                let button = if right {
                    MouseButton::Right
                } else {
                    MouseButton::Left
                };
                for down in [true, false] {
                    scene.pc(
                        now,
                        PcInput::Button {
                            id: p.id,
                            uv: p.uv0,
                            button,
                            down,
                        },
                    );
                }
                if right {
                    self.rings.push((
                        self.picks[h].map_or(Vec3::ZERO, |t| t.hit.point),
                        now,
                        Mark::Right,
                    ));
                }
            }
            return;
        }
        let Some(ray) = ray else {
            self.hands[h].phase = Phase::RayPress(p);
            return;
        };
        let turned = degrees(ray.dir, p.dir0);
        let point = self.picks[h].map(|t| t.hit.point);
        // A pinch on the way to a fist is no click: the fist takes the monitor.
        if !p.down && hs.is_hand && take == Take::Fist && self.hands[h].m.gestures.fist.pressed {
            self.hands[h].phase = Phase::Idle;
            let on = point.map(|at| (p.id, p.uv0, at));
            self.take_gesture(now, h, input, head, on, scene);
            return;
        }
        if !p.down {
            // The hand's button waits.
            let slop = f.hold_slop_deg.min(f.freeze_deg);
            if take == Take::Hold && held >= ms(f.hold_take_ms) && turned <= slop {
                self.hands[h].phase = Phase::Idle;
                if let Some(grab) = start_grab(scene, p.id, ray) {
                    scene.haptic(h);
                    self.hands[h].phase =
                        Phase::Carry(Carry::by_ray(self.reach(grab, ray, head, h)));
                }
                return;
            }
            if turned > f.freeze_deg {
                p.down = true;
                p.dragging = true;
                scene.pc(
                    now,
                    PcInput::Button {
                        id: p.id,
                        uv: p.uv0,
                        button: MouseButton::Left,
                        down: true,
                    },
                );
            } else if held > ms(f.ring_after_ms)
                && let Some(at) = point
            {
                let full = if take == Take::Hold {
                    f.hold_take_ms
                } else {
                    f.long_press_ms
                };
                let k = (held.as_secs_f32() * 1000.0 / full).min(1.0);
                self.hands[h].m.progress = Some((at, k));
            }
        } else if !p.dragging && (held >= ms(f.freeze_ms) || turned > f.freeze_deg) {
            p.dragging = true;
        }
        if p.dragging
            && let Some(Target {
                thing: Thing::Display(id),
                hit,
            }) = self.picks[h]
            && id == p.id
            && let Some(uv) = scene.monitor_uv(id, hit.local)
        {
            p.uv = uv;
        }
        self.hands[h].m.point = Some((p.id, p.uv, Some(if p.dragging { ray.dir } else { p.dir0 })));
        self.hands[h].phase = Phase::RayPress(p);
    }

    /// A fist or a middle pinch (as [`Modes::take`] says) on a monitor pointed at, or
    /// a moment ago: it is taken; with the fist and "hold" takes, a middle pinch
    /// starts a scroll. Whether one started.
    fn take_gesture(
        &mut self,
        now: Instant,
        h: usize,
        input: &FrameInput,
        head: Head,
        on: Option<(DisplayKey, Vec2, Vec3)>,
        scene: &mut impl Scene,
    ) -> bool {
        let f = self.feel;
        let take = self.m.modes.take;
        let Some(j) = input.joints[h] else {
            return false;
        };
        let hand = &self.hands[h];
        let g = hand.m.gestures;
        let target = on.or_else(|| {
            hand.m
                .latched
                .filter(|l| now.saturating_duration_since(l.3) < ms(f.take_latch_ms))
                .map(|(id, uv, at, _)| (id, uv, at))
        });
        let Some((id, uv, at)) = target else {
            return false;
        };
        let gesture = match take {
            Take::Fist if g.fist.pressed => Some(Take::Fist),
            Take::Middle if g.middle.pressed => Some(Take::Middle),
            _ => None,
        };
        if let Some(gesture) = gesture {
            // From the palm to the point taken, whichever way the palm points.
            let palm = signals::palm_ray(&j, head.position, head.forward, h == 0, &f);
            let toward = (at - palm.origin).normalize_or(palm.dir);
            let ray = Ray {
                origin: palm.origin,
                dir: toward,
            };
            if let Some(grab) = start_grab(scene, id, ray) {
                scene.haptic(h);
                self.hands[h].phase = Phase::Carry(Carry {
                    grab: self.reach(grab, ray, head, h),
                    palm: Some(gesture),
                    turn: Quat::from_rotation_arc(palm.dir, toward),
                    lost: None,
                });
            }
            return true;
        }
        if take != Take::Middle && g.middle.pressed {
            let y = j[signals::PALM].0.y;
            self.hands[h].phase = Phase::Scroll(Scroll {
                id,
                uv,
                at,
                start_y: y,
                last_y: y,
                moved: false,
                acc: 0.0,
                lost: None,
            });
            self.m.mouse_hand = Some(h);
            return true;
        }
        false
    }

    /// A middle pinch on a monitor: moved up or down, it scrolls; let go unmoved, a
    /// right click.
    fn scroll(
        &mut self,
        now: Instant,
        h: usize,
        input: &FrameInput,
        mut s: Scroll,
        switched: bool,
        scene: &mut impl Scene,
    ) {
        let f = self.feel;
        let g = self.hands[h].m.gestures;
        // Not seen: held a moment, then over without a click (the gestures are not
        // known then, so their end is no right click either).
        let Some(j) = input.joints[h].filter(|_| !switched) else {
            let lost = *s.lost.get_or_insert(now);
            if switched || now.saturating_duration_since(lost) > ms(f.lost_grace_ms) {
                self.hands[h].phase = Phase::Idle;
            } else {
                self.hands[h].phase = Phase::Scroll(s);
            }
            return;
        };
        s.lost = None;
        if !g.middle.down {
            self.hands[h].phase = Phase::Idle;
            if !s.moved && g.middle.released {
                for down in [true, false] {
                    scene.pc(
                        now,
                        PcInput::Button {
                            id: s.id,
                            uv: s.uv,
                            button: MouseButton::Right,
                            down,
                        },
                    );
                }
                self.rings.push((s.at, now, Mark::Right));
            }
            return;
        }
        let y = j[signals::PALM].0.y;
        if (y - s.start_y).abs() > f.scroll_start_m {
            s.moved = true;
        }
        if s.moved {
            s.acc += (y - s.last_y) / f.scroll_notch_m;
            let n = s.acc.trunc();
            if n != 0.0 {
                s.acc -= n;
                scene.pc(
                    now,
                    PcInput::Scroll {
                        id: s.id,
                        dx: 0,
                        dy: (n * 120.0) as i16,
                    },
                );
            }
        } else {
            self.hands[h].m.progress = Some((s.at, 1.0));
        }
        s.last_y = y;
        self.hands[h].phase = Phase::Scroll(s);
    }

    /// A monitor taken with a fist or a middle pinch: it moves with the ray from the
    /// shoulder through the palm while the gesture holds, and is let go as it ends.
    #[allow(clippy::too_many_arguments, reason = "the frame's parts, each needed")]
    fn palm_carry(
        &mut self,
        now: Instant,
        h: usize,
        input: &FrameInput,
        head: Head,
        mut c: Carry,
        switched: bool,
        scene: &mut impl Scene,
    ) {
        let f = self.feel;
        let g = self.hands[h].m.gestures;
        let still = match c.palm {
            Some(Take::Middle) => g.middle.down,
            _ => g.fist.down,
        };
        let ray = input.joints[h].map(|j| c.palm_ray(&j, head, h == 0, &f));
        // Not seen: held where it is a moment, then let go there.
        if ray.is_none() && !switched {
            let lost = *c.lost.get_or_insert(now);
            if now.saturating_duration_since(lost) <= ms(f.lost_grace_ms) {
                self.hands[h].phase = Phase::Carry(c);
                return;
            }
        }
        c.lost = None;
        let (true, Some(ray), false) = (still, ray, switched) else {
            self.hands[h].phase = Phase::Idle;
            if scene.let_go(now, c.grab.id) {
                scene.haptic(h);
            }
            self.sync(scene);
            return;
        };
        if let Some(mut p) = scene.place(c.grab.id) {
            c.grab
                .update(&mut p, ray, head.position, Vec2::ZERO, input.dt, &f);
            scene.drag(c.grab.id, p, head.position);
        }
        self.hands[h].phase = Phase::Carry(c);
    }

    /// What hand `h` pressed on a monitor ends as letting go would (`cancel`: a touch
    /// is cancelled, not a tap): its button up, its touch up.
    pub(super) fn end_monitor(
        &mut self,
        h: usize,
        now: Option<Instant>,
        scene: &mut impl Scene,
        cancel: bool,
    ) {
        let Some(now) = now else { return };
        match self.hands[h].phase {
            Phase::Poke(p) => {
                let phase = if cancel {
                    TouchPhase::Cancel
                } else {
                    TouchPhase::Up
                };
                self.end_poke(h, now, p, phase, scene);
            }
            Phase::RayPress(p) => {
                self.hands[h].phase = Phase::Idle;
                if p.down {
                    scene.pc(
                        now,
                        PcInput::Button {
                            id: p.id,
                            uv: p.uv,
                            button: MouseButton::Left,
                            down: false,
                        },
                    );
                }
            }
            Phase::Scroll(_) => self.hands[h].phase = Phase::Idle,
            _ => {}
        }
    }

    /// The modes changed (in "For development"): what a hand does on a monitor by them
    /// ends as cancelling would (a button up, a touch cancelled, a monitor carried by
    /// the palm let go where it is), so nothing stays pressed on the PC.
    pub(super) fn modes_changed(&mut self, now: Instant, scene: &mut impl Scene) {
        for h in 0..2 {
            self.end_monitor(h, Some(now), scene, true);
            if let Phase::Carry(c) = self.hands[h].phase
                && c.palm.is_some()
            {
                self.hands[h].phase = Phase::Idle;
                scene.let_go(now, c.grab.id);
            }
            self.hands[h].m.touch_mode = false;
            self.hands[h].m.last_z = None;
        }
        self.m.mouse_hand = None;
        self.sync(scene);
    }

    /// The one pointer: where the hand that pressed last points (else the other),
    /// unless its ray hardly moved or it moved it a moment ago.
    pub(super) fn mouse(&mut self, now: Instant, scene: &mut impl Scene) {
        if !self.m.modes.monitors {
            return;
        }
        let f = self.feel;
        let has = |h: usize| self.hands[h].m.point.is_some();
        // While the hand that pressed last still presses (a touch, a click, a scroll),
        // the other only points, even where that hand gives no point.
        let pressing = self.m.mouse_hand.filter(|&h| {
            matches!(
                self.hands[h].phase,
                Phase::Poke(_) | Phase::RayPress(_) | Phase::Scroll(_)
            )
        });
        let mover = match pressing {
            Some(h) => Some(h).filter(|&h| has(h)),
            None => self
                .m
                .mouse_hand
                .filter(|&h| has(h))
                .or_else(|| [1, 0].into_iter().find(|&h| has(h))),
        };
        let Some(h) = mover else {
            return;
        };
        let Some((id, uv, dir)) = self.hands[h].m.point else {
            return;
        };
        self.hands[h].m.mouse = scene.mouse_shown(id);
        if let Some((last_id, last_uv, last_dir, at)) = self.m.last_mouse {
            let same_place = last_id == id
                && match (last_dir, dir) {
                    (Some(a), Some(b)) => degrees(a, b) < f.still_deg,
                    _ => last_uv.distance(uv) < 1e-4,
                };
            if same_place || now.saturating_duration_since(at) < ms(f.mouse_every_ms) {
                return;
            }
        }
        self.m.last_mouse = Some((id, uv, dir, now));
        scene.pc(now, PcInput::Move { id, uv });
    }

    /// The ray each hand shows this frame: its own, as long as what it points at (or a
    /// little way), none while its fingertip touches; the palm's while it carries by
    /// it.
    pub(super) fn shown_rays(&mut self, input: &FrameInput, head: Head) {
        let f = self.feel;
        for h in 0..2 {
            let hand = &mut self.hands[h];
            hand.m.ray = match hand.phase {
                Phase::Carry(c) if c.palm.is_some() => input.joints[h]
                    .map(|j| (c.palm_ray(&j, head, h == 0, &f), c.grab.distance.min(4.0))),
                _ if hand.m.touch_mode => None,
                _ => input.hands[h]
                    .ray
                    .map(|r| (r, self.picks[h].map_or(1.2, |p| p.hit.t).min(4.0))),
            };
        }
    }
}

impl Hands {
    /// A monitor taken by hand `h` along `ray`: moved on a sphere where the modes say
    /// so, else by the hand's reach as before step 8.
    pub(super) fn reach(&self, grab: Grab, ray: Ray, head: Head, h: usize) -> Grab {
        let f = &self.feel;
        match self.m.modes.movement {
            Movement::Sphere(p) if grab.id.display().is_some() => {
                let shoulder = signals::shoulder(head.position, head.forward, h == 0, f);
                grab.on_sphere(ray, head.position, shoulder, f32::from(p), f)
            }
            Movement::Sphere(_) | Movement::AsNow => grab.with_reach(ray.origin, head.position, f),
        }
    }
}

/// Takes monitor `id` along `ray`, where the ray meets its plane (so it does not jump).
fn start_grab(scene: &mut impl Scene, id: DisplayKey, ray: Ray) -> Option<Grab> {
    let thing = Thing::Display(id);
    let at = scene.hold(thing)?;
    let n = at.rotation * Vec3::Z;
    let denom = ray.dir.dot(n);
    let t = if denom.abs() > 1e-3 {
        ((at.position - ray.origin).dot(n) / denom).max(0.1)
    } else {
        ray.origin.distance(at.position)
    };
    let local = at.rotation.inverse() * (ray.origin + ray.dir * t - at.position);
    Some(Grab::start(thing, t, Vec2::new(local.x, local.y), &at))
}
