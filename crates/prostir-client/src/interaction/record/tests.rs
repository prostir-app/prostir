//! Scripted recordings, each a few seconds of one thing a person does, written as a
//! recording, read back and played: the journal must be the one kept next to it in
//! `tests/input/<name>.expected` (made with the input as it was before step 8; after a
//! change on purpose, `PROSTIR_BLESS=1` writes them anew and the difference shows in
//! review). Recordings made in the headset (`tests/input/*.rec`) are played the same
//! way.

#![expect(
    clippy::disallowed_methods,
    reason = "the test's own starting time, and the journals it reads and writes"
)]

use std::path::PathBuf;

use super::*;
use crate::ids::PcId;
use crate::interaction::signals;
use prostir_proto::state::DisplayId;

const FRAME: Duration = Duration::from_millis(11);
const HEAD: Vec3 = Vec3::new(0.0, 1.6, 0.0);

fn display(n: u32) -> DisplayKey {
    DisplayKey::new(PcId([7; 8]), DisplayId(n))
}

fn placed(position: Vec3, width: f32) -> Placement {
    Placement {
        position,
        rotation: facing(position, HEAD),
        width,
    }
}

/// The room of every script: the menu open low on the left, a monitor in front, one to
/// the right, a detached window high on the left, a small monitor within reach low on
/// the right (for touching it).
fn stage() -> Stage {
    let piece = |n, at, width, height, window| Piece {
        thing: Thing::Display(display(n)),
        place: placed(at, width),
        height,
        window,
    };
    Stage::new(
        placed(Vec3::new(-0.35, 1.1, -0.55), panel_width(false)),
        true,
        vec![
            piece(1, Vec3::new(0.0, 1.75, -1.5), 1.2, 0.675, false),
            piece(2, Vec3::new(1.4, 1.6, -1.1), 1.0, 0.56, false),
            piece(5, Vec3::new(-1.2, 1.7, -1.0), 0.5, 0.4, true),
            piece(3, Vec3::new(0.45, 1.0, -0.5), 0.5, 0.3, false),
            Piece {
                thing: Thing::Keyboard,
                place: placed(Vec3::new(0.0, 0.75, -0.45), 0.4),
                height: 0.15,
                window: false,
            },
        ],
    )
}

/// The middle of the menu (a button) and its lower edge (an empty place).
fn menu_button() -> Vec3 {
    stage().menu.position
}

fn menu_empty() -> Vec3 {
    let m = stage().menu;
    m.position + m.rotation * Vec3::new(0.0, -0.4 * panel_height(false), 0.0)
}

fn monitor(n: u32) -> Vec3 {
    stage()
        .piece(Thing::Display(display(n)))
        .expect("on the stage")
        .place
        .position
}

/// A hand's joints, all at `at` but the index fingertip at `tip`.
fn tip_joints(at: Vec3, tip: Vec3) -> Joints {
    let mut j = [(at, 0.01); 26];
    j[super::super::INDEX_TIP].0 = tip;
    j
}

/// An open hand at `palm` with its palm turned to the head (`left`: the left hand).
fn open_palm(palm: Vec3, left: bool) -> Joints {
    let n = (HEAD - palm).normalize();
    let up = Vec3::Y.reject_from_normalized(n).normalize();
    let side = n.cross(up) * if left { 1.0 } else { -1.0 };
    let mut j = [(palm, 0.01); 26];
    j[1].0 = palm - up * 0.04;
    j[7].0 = palm + up * 0.04 + side * 0.03;
    j[22].0 = palm + up * 0.035 - side * 0.03;
    j[12].0 = palm + up * 0.045;
    j[15].0 = palm + up * 0.12;
    j[super::super::INDEX_TIP].0 = palm + up * 0.11 + side * 0.02;
    j
}

/// A script: the frames as they are made, and the hands as they are now.
struct Script {
    frames: Vec<(Duration, FrameInput)>,
    t: Duration,
    input: FrameInput,
    select: [f32; 2],
    grab: [f32; 2],
    left_handed: bool,
    modes: Modes,
}

impl Script {
    fn new() -> Self {
        let mut input = FrameInput::hidden(None);
        input.head = Some(Head {
            position: HEAD,
            forward: -Vec3::Z,
            right: Vec3::X,
        });
        input.focused = true;
        input.dt = FRAME.as_secs_f32();
        Self {
            frames: Vec::new(),
            t: Duration::ZERO,
            input,
            select: [0.0; 2],
            grab: [0.0; 2],
            left_handed: false,
            modes: Modes::default(),
        }
    }

    /// Step 8's modes on, taking a monitor with `take`.
    fn monitors(&mut self, take: Take) -> &mut Self {
        self.modes = Modes {
            monitors: true,
            take,
            hands: true,
            ..Modes::default()
        };
        self
    }

    /// Hand `h` posed (see [`pose`]).
    fn pose(&mut self, h: usize, palm: Vec3, p: Pose) -> &mut Self {
        self.input.hands[h].is_hand = true;
        self.input.joints[h] = Some(pose(palm, p));
        self
    }

    /// Hand `h`'s ray from beside the body to `target` (a controller unless
    /// [`Script::hand`] said otherwise).
    fn ray(&mut self, h: usize, target: Vec3) -> &mut Self {
        let origin = Vec3::new(if h == 0 { -0.2 } else { 0.2 }, 1.3, -0.25);
        self.input.hands[h].ray = Some(Ray {
            origin,
            dir: (target - origin).normalize(),
        });
        self
    }

    fn ray_from(&mut self, h: usize, origin: Vec3, target: Vec3) -> &mut Self {
        self.input.hands[h].ray = Some(Ray {
            origin,
            dir: (target - origin).normalize(),
        });
        self
    }

    /// `n` frames with the ray of hand `h` turning right by `deg` each.
    fn turn(&mut self, h: usize, n: u32, deg: f32) -> &mut Self {
        for _ in 0..n {
            if let Some(r) = self.input.hands[h].ray.as_mut() {
                r.dir = Quat::from_rotation_y(-deg.to_radians()) * r.dir;
            }
            self.frames(1);
        }
        self
    }

    fn no_ray(&mut self, h: usize) -> &mut Self {
        self.input.hands[h].ray = None;
        self
    }

    fn hand(&mut self, h: usize, is_hand: bool) -> &mut Self {
        self.input.hands[h].is_hand = is_hand;
        self
    }

    fn select(&mut self, h: usize, v: f32) -> &mut Self {
        self.select[h] = v;
        self
    }

    fn grab(&mut self, h: usize, v: f32) -> &mut Self {
        self.grab[h] = v;
        self
    }

    fn stick(&mut self, h: usize, x: f32, y: f32) -> &mut Self {
        self.input.hands[h].stick = Vec2::new(x, y);
        self
    }

    fn joints(&mut self, h: usize, j: Option<&Joints>) -> &mut Self {
        self.input.joints[h] = j.copied();
        self
    }

    /// The source of hand `h` changes (a controller put down, a hand taking over):
    /// its input starts afresh.
    fn afresh(&mut self, h: usize, is_hand: bool) -> &mut Self {
        self.input.hands[h] = HandState {
            is_hand,
            ..HandState::default()
        };
        self.select[h] = 0.0;
        self.grab[h] = 0.0;
        self
    }

    /// `n` frames as things are.
    fn frames(&mut self, n: u32) -> &mut Self {
        for _ in 0..n {
            self.t += FRAME;
            for h in 0..2 {
                let s = &mut self.input.hands[h];
                s.select.update(self.select[h]);
                s.grab.update(self.grab[h]);
            }
            self.frames.push((self.t, self.input));
            self.input.menu_button = false;
            self.input.align = false;
        }
        self
    }

    /// `n` frames with the ray of hand `h` moving by `by` each.
    fn sweep(&mut self, h: usize, n: u32, by: Vec3) -> &mut Self {
        for _ in 0..n {
            if let Some(r) = self.input.hands[h].ray.as_mut() {
                r.origin += by;
            }
            self.frames(1);
        }
        self
    }

    /// `n` frames with hand `h`'s index fingertip going from `from` to `to`.
    fn finger(&mut self, h: usize, n: u32, from: Vec3, to: Vec3) -> &mut Self {
        for i in 1..=n {
            let tip = from.lerp(to, i as f32 / n as f32);
            self.input.joints[h] = Some(tip_joints(tip + Vec3::new(0.0, -0.05, 0.08), tip));
            self.frames(1);
        }
        self
    }

    fn recording(&self) -> Vec<u8> {
        let start = Instant::now();
        let mut stage = stage();
        stage.left_handed = self.left_handed;
        stage.modes = self.modes;
        let mut r = Recorder::start(start, &head("test", &Feel::DEFAULT, &stage));
        for (t, input) in &self.frames {
            r.push(start + *t, input);
        }
        r.into_bytes()
    }
}

/// Each script by its name.
fn scripts() -> Vec<(&'static str, Script)> {
    let mut out = Vec::new();
    let mut add = |name, f: &dyn Fn(&mut Script)| {
        let mut s = Script::new();
        f(&mut s);
        out.push((name, s));
    };
    add("trigger-on-menu", &|s| {
        s.ray(1, menu_button()).frames(3);
        s.select(1, 1.0).frames(5).select(1, 0.0).frames(5);
        // Over the empty place: a trigger there presses nothing.
        s.ray(1, menu_empty())
            .select(1, 1.0)
            .frames(3)
            .select(1, 0.0)
            .frames(3);
    });
    add("pinch-on-menu", &|s| {
        s.hand(1, true).ray(1, menu_button()).frames(3);
        // A short pinch clicks as it opens.
        s.select(1, 1.0).frames(9).select(1, 0.0).frames(5);
        // Held, it takes the menu; moved; let go, nothing clicked.
        s.select(1, 1.0).frames(35);
        s.sweep(1, 20, Vec3::new(0.005, 0.0, 0.0));
        s.select(1, 0.0).frames(5);
    });
    add("grip-a-monitor", &|s| {
        s.ray(1, monitor(1)).frames(3).grab(1, 1.0).frames(3);
        s.sweep(1, 20, Vec3::new(0.004, 0.002, 0.0));
        s.stick(1, 0.0, 0.8)
            .frames(20)
            .stick(1, 0.6, 0.0)
            .frames(20);
        s.stick(1, 0.1, 0.1).frames(5).stick(1, 0.0, 0.0);
        s.grab(1, 0.0).frames(5);
    });
    add("pinch-holds-a-monitor", &|s| {
        s.hand(1, true).ray(1, monitor(2)).frames(3);
        s.select(1, 1.0).frames(40);
        // The hand reaches out: the monitor goes further.
        s.sweep(1, 20, Vec3::new(0.0, 0.0, -0.004));
        s.select(1, 0.0).frames(5);
    });
    add("touch-the-menu", &|s| {
        let at = menu_button();
        let n = stage().menu.rotation * Vec3::Z;
        s.finger(1, 8, at + n * 0.12, at + n * 0.02);
        s.finger(1, 4, at + n * 0.02, at - n * 0.004);
        // Back a little and in again: no second press.
        s.finger(1, 3, at - n * 0.004, at + n * 0.03);
        s.finger(1, 3, at + n * 0.03, at - n * 0.004);
        s.finger(1, 6, at - n * 0.004, at + n * 0.15);
        // The same hand pinches right after, on the menu: the hand pulling back.
        s.joints(1, None).hand(1, true).ray(1, menu_button());
        s.select(1, 1.0).frames(3).select(1, 0.0).frames(3);
        s.frames(60);
        s.select(1, 1.0).frames(3).select(1, 0.0).frames(3);
    });
    add("touch-drags-the-menu", &|s| {
        let at = menu_empty();
        let n = stage().menu.rotation * Vec3::Z;
        s.finger(1, 8, at + n * 0.1, at - n * 0.004);
        s.finger(
            1,
            15,
            at - n * 0.004,
            at - n * 0.004 + Vec3::new(0.12, 0.05, 0.0),
        );
        s.finger(1, 6, at + Vec3::new(0.12, 0.05, 0.0), at + n * 0.1);
    });
    add("two-hands-resize", &|s| {
        s.ray_from(0, Vec3::new(-0.2, 1.3, -0.25), monitor(1));
        s.ray_from(1, Vec3::new(0.2, 1.3, -0.25), monitor(1));
        s.frames(2).grab(0, 1.0).frames(3).grab(1, 1.0).frames(3);
        s.sweep(0, 10, Vec3::new(-0.01, 0.0, 0.0));
        s.sweep(1, 10, Vec3::new(0.01, 0.0, 0.0));
        s.grab(1, 0.0).frames(3).grab(0, 0.0).frames(3);
    });
    add("controller-put-down-mid-drag", &|s| {
        s.ray(1, monitor(1)).frames(2).grab(1, 1.0).frames(10);
        s.afresh(1, true).frames(5);
    });
    add("hands-stop-pointing", &|s| {
        s.hand(1, true)
            .ray(1, monitor(1))
            .frames(2)
            .select(1, 1.0)
            .frames(35);
        // The hand's input as the shell gives it once hands no longer point.
        s.afresh(1, true).frames(5);
        // A pinch on a button, then the hands stop: it never clicks.
        s.hand(1, true)
            .ray(1, menu_button())
            .frames(2)
            .select(1, 1.0)
            .frames(3);
        s.afresh(1, true)
            .frames(3)
            .hand(1, true)
            .ray(1, menu_button())
            .frames(3);
    });
    add("palm-menu", &|s| {
        let palm = Vec3::new(-0.15, 1.35, -0.35);
        s.joints(0, Some(&open_palm(palm, true))).frames(30);
        // The other hand's fingertip touches it.
        let n = (HEAD - palm).normalize();
        let on = palm + n * 0.03;
        s.finger(1, 6, on + n * 0.1, on - n * 0.004);
        s.finger(1, 6, on - n * 0.004, on + n * 0.1);
        s.joints(0, None).frames(3);
    });
    add("menu-button-and-line-up", &|s| {
        s.input.menu_button = true;
        s.frames(1).frames(2);
        s.input.menu_gesture = true;
        s.frames(4);
        s.input.menu_gesture = false;
        s.frames(1);
        s.input.align = true;
        s.frames(1).frames(1);
    });
    add("attach-a-window", &|s| {
        let window = monitor(5);
        s.ray(1, window).frames(3);
        // Down to "Attach" under it while it lingers.
        s.ray(1, window - Vec3::Y * 0.24).frames(3);
        s.select(1, 1.0).frames(2).select(1, 0.0).frames(2);
        s.no_ray(1).frames(120);
    });
    add("hands-typing", &|s| {
        for i in 0..90u32 {
            let k = i as f32 * 0.3;
            let left = Vec3::new(-0.15 + 0.01 * k.sin(), 0.9, -0.35);
            let right = Vec3::new(0.15, 0.9 + 0.01 * k.cos(), -0.35);
            s.hand(0, true).hand(1, true);
            s.joints(
                0,
                Some(&tip_joints(left, left + Vec3::new(0.0, -0.02, -0.05))),
            );
            s.joints(
                1,
                Some(&tip_joints(right, right + Vec3::new(0.0, -0.02, -0.05))),
            );
            s.frames(1);
        }
    });
    add("left-handed-palm-menu", &|s| {
        s.left_handed = true;
        let palm = Vec3::new(0.15, 1.35, -0.35);
        s.joints(1, Some(&open_palm(palm, false))).frames(30);
        let n = (HEAD - palm).normalize();
        let on = palm + n * 0.03;
        s.finger(0, 6, on + n * 0.1, on - n * 0.004);
        s.finger(0, 6, on - n * 0.004, on + n * 0.1);
    });
    monitor_scripts(&mut out);
    review_scripts(&mut out);
    out
}

/// What a review of step 8.1 asked for: a fingertip passing the menu while a hand
/// carries or holds a pinch, the keyboard window, the menu toggled under a pinch, a
/// middle pinch losing the hand a frame, a touch coming from behind a monitor.
fn review_scripts(out: &mut Vec<(&'static str, Script)>) {
    let mut add = |name, f: &dyn Fn(&mut Script)| {
        let mut s = Script::new();
        f(&mut s);
        out.push((name, s));
    };
    let m = stage().menu;
    let n = m.rotation * Vec3::Z;
    // Joints with the index fingertip at `tip`.
    let at = |tip: Vec3| tip_joints(tip + Vec3::new(0.0, -0.05, 0.08), tip);
    add("carry-past-the-menu", &move |s| {
        // A pinch held on monitor 1 takes it; then the fingertip passes 2 cm in front
        // of the menu's button and away.
        s.hand(1, true)
            .ray(1, monitor(1))
            .joints(1, Some(&at(Vec3::new(0.3, 1.2, -0.2))));
        s.frames(2).select(1, 1.0).frames(35);
        for i in 0..=10 {
            let tip = m.position + n * 0.02 + Vec3::new(0.03 * (i as f32 - 5.0), 0.0, 0.0);
            s.joints(1, Some(&at(tip))).frames(1);
        }
        s.joints(1, Some(&at(Vec3::new(0.3, 1.2, -0.2)))).frames(2);
        s.select(1, 0.0).frames(3);
    });
    add("pinch-held-drifting-onto-the-menu", &move |s| {
        // A pinch held on monitor 1 while the fingertip drifts 3 cm in front of the
        // menu: the finger is the pointer then, so the hold takes nothing.
        s.hand(1, true)
            .ray(1, monitor(1))
            .joints(1, Some(&at(m.position + n * 0.03)));
        s.frames(2)
            .select(1, 1.0)
            .frames(40)
            .select(1, 0.0)
            .frames(3);
    });
    add("keyboard-window", &|s| {
        let k = stage()
            .piece(Thing::Keyboard)
            .expect("there")
            .place
            .position;
        s.ray(1, k).frames(2).grab(1, 1.0).frames(2);
        s.sweep(1, 10, Vec3::new(0.003, 0.0, 0.0));
        s.grab(1, 0.0).frames(2);
    });
    add("menu-toggled-during-a-pending-pinch", &|s| {
        // A pinch on a menu button; the menu closed and opened by the controller's
        // menu button meanwhile: the pinch opening clicks nothing.
        s.hand(1, true)
            .ray(1, menu_button())
            .frames(2)
            .select(1, 1.0)
            .frames(3);
        s.input.menu_button = true;
        s.frames(2);
        s.input.menu_button = true;
        s.frames(2).select(1, 0.0).frames(3);
    });
    let palm = Vec3::new(0.0, 1.3, -0.3);
    add("monitors-middle-pinch-loses-a-frame", &move |s| {
        s.monitors(Take::Fist).pose(1, palm, Pose::Open);
        s.ray_from(1, palm, monitor(1)).frames(3);
        s.pose(1, palm, Pose::Middle).frames(3);
        // The hand not seen for a frame, then seen still pinching.
        s.joints(1, None).frames(1);
        s.pose(1, palm, Pose::Middle).frames(3);
        s.pose(1, palm, Pose::Open).frames(3);
    });
    add("monitors-touch-from-behind", &|s| {
        // In front of the small monitor, then away (the ray back), then right behind
        // its surface and on through it: no touch, it came from behind.
        let palm_for = |tip: Vec3| tip - Vec3::new(0.025, 0.0, -0.1);
        s.monitors(Take::Hold);
        for z in [0.12, 0.08, 0.03, 0.03] {
            s.pose(1, palm_for(before_near(z)), Pose::Point).frames(1);
        }
        s.pose(1, palm_for(before_near(0.4)), Pose::Point).frames(3);
        for z in [-0.01, -0.004, -0.002, 0.03] {
            s.pose(1, palm_for(before_near(z)), Pose::Point).frames(1);
        }
    });
}

/// How a hand is held in a script.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Pose {
    /// Fingers out.
    Open,
    /// The index out, the rest curled: pointing, to touch.
    Point,
    /// All curled.
    Fist,
    /// Fingers out, thumb on the middle fingertip.
    Middle,
}

/// A hand at `palm`, fingers forward (`-Z`), as `p` says; its index fingertip ends
/// 10 cm in front of the palm when out.
fn pose(palm: Vec3, p: Pose) -> Joints {
    let mut j = [(palm, 0.01); 26];
    j[signals::WRIST].0 = palm + Vec3::new(0.0, 0.0, 0.06);
    j[signals::MIDDLE_PROXIMAL].0 = palm + Vec3::new(0.0, 0.0, -0.03);
    let out = |dx: f32| palm + Vec3::new(dx, 0.0, -0.1);
    let curled = |dx: f32| palm + Vec3::new(dx, -0.02, -0.025);
    let (index_out, others_out) = match p {
        Pose::Open | Pose::Middle => (true, true),
        Pose::Point => (true, false),
        Pose::Fist => (false, false),
    };
    for (tip, dx) in [(10, 0.025), (15, -0.01), (20, -0.025), (25, -0.04)] {
        let o = if tip == 10 { index_out } else { others_out };
        j[tip].0 = if o { out(dx) } else { curled(dx) };
    }
    j[signals::THUMB_TIP].0 = if p == Pose::Middle {
        j[signals::MIDDLE_TIP].0 + Vec3::new(-0.005, 0.0, 0.0)
    } else {
        palm + Vec3::new(0.06, 0.0, -0.04)
    };
    j
}

/// Hand `h` (`left` or the right) at `palm` with its palm turned down, fingers
/// forward, as on a keyboard.
fn palm_down(palm: Vec3, left: bool) -> Joints {
    let mut j = pose(palm, Pose::Open);
    // The index knuckle on the thumb's side: right of the left hand.
    let side = if left { 1.0 } else { -1.0 };
    j[7].0 = palm + Vec3::new(0.03 * side, 0.0, -0.03);
    j[22].0 = palm + Vec3::new(-0.03 * side, 0.0, -0.03);
    j
}

/// `n` frames of both hands at height `y` as on a keyboard, palms down, their rays on
/// monitor 1 wandering a little, the right one pinching now and then.
fn typing(s: &mut Script, y: f32, n: u32) {
    for i in 0..n {
        let k = i as f32 * 0.4;
        for (h, x) in [(0, -0.22), (1, -0.02)] {
            let palm = Vec3::new(x + 0.01 * k.sin(), y, -0.3 + 0.01 * k.cos());
            s.hand(h, true).joints(h, Some(&palm_down(palm, h == 0)));
            let aim = monitor(1) + Vec3::new(0.05 * k.sin(), 0.03 * k.cos(), 0.0);
            s.ray_from(h, palm, aim);
        }
        s.select(1, if i % 30 == 10 { 1.0 } else { 0.0 });
        s.frames(1);
    }
}

/// Where a fingertip is `z` in front of the middle of the monitor within reach (3).
fn before_near(z: f32) -> Vec3 {
    let p = stage()
        .piece(Thing::Display(display(3)))
        .expect("on the stage")
        .place;
    p.position + p.rotation * Vec3::new(0.0, 0.0, z)
}

/// Step 8's scripts: monitors touched, pinched, clicked, scrolled and taken.
fn monitor_scripts(out: &mut Vec<(&'static str, Script)>) {
    let mut add = |name, f: &dyn Fn(&mut Script)| {
        let mut s = Script::new();
        f(&mut s);
        out.push((name, s));
    };
    // The palm such that the index fingertip is at `tip`.
    let palm_for = |tip: Vec3| tip - Vec3::new(0.025, 0.0, -0.1);
    let touch = |s: &mut Script, n: u32, from: Vec3, to: Vec3| {
        for i in 1..=n {
            let tip = from.lerp(to, i as f32 / n as f32);
            s.pose(1, palm_for(tip), Pose::Point).frames(1);
        }
    };
    // Typing with both hands on the keyboard: palms down at the desk, their rays (the
    // runtime calls them ready to pinch) on monitor 1 by chance, a pinch now and then.
    add("monitors-typing-on-the-keyboard", &|s| {
        s.monitors(Take::Hold);
        typing(s, 0.85, 120);
    });
    // The same hand raised, palm still down: it points, and the mouse goes.
    add("monitors-raised-hand-points", &|s| {
        s.monitors(Take::Hold);
        typing(s, 1.3, 20);
    });
    add("monitors-touch-tap", &|s| {
        s.monitors(Take::Hold);
        touch(s, 10, before_near(0.14), before_near(0.01));
        touch(s, 3, before_near(0.01), before_near(-0.004));
        touch(s, 6, before_near(-0.004), before_near(0.06));
        s.joints(1, None).frames(2);
    });
    add("monitors-touch-swipe", &|s| {
        s.monitors(Take::Hold);
        touch(s, 10, before_near(0.14), before_near(-0.004));
        // Up along the monitor's surface.
        let up = before_near(0.0) - before_near(0.0);
        let up = up
            + stage()
                .piece(Thing::Display(display(3)))
                .expect("there")
                .place
                .rotation
                * Vec3::new(0.0, 0.08, 0.0);
        touch(s, 12, before_near(-0.004), before_near(-0.004) + up);
        touch(s, 6, before_near(-0.004) + up, before_near(0.08) + up);
    });
    add("monitors-touch-through", &|s| {
        s.monitors(Take::Hold);
        touch(s, 10, before_near(0.14), before_near(-0.004));
        touch(s, 8, before_near(-0.004), before_near(-0.09));
        touch(s, 8, before_near(-0.09), before_near(0.12));
    });
    // A hand away from what it could touch, its ray from the runtime.
    let palm = Vec3::new(0.0, 1.3, -0.3);
    add("monitors-pinch-click", &move |s| {
        s.monitors(Take::Hold).pose(1, palm, Pose::Open);
        s.ray_from(1, palm, monitor(1)).frames(3);
        s.select(1, 1.0).frames(8).select(1, 0.0).frames(3);
    });
    add("monitors-pinch-drags", &move |s| {
        s.monitors(Take::Hold).pose(1, palm, Pose::Open);
        s.ray_from(1, palm, monitor(1))
            .frames(3)
            .select(1, 1.0)
            .frames(3);
        s.turn(1, 20, 0.15);
        s.select(1, 0.0).frames(3);
    });
    add("monitors-held-pinch-takes", &move |s| {
        s.monitors(Take::Hold).pose(1, palm, Pose::Open);
        s.ray_from(1, palm, monitor(1))
            .frames(3)
            .select(1, 1.0)
            .frames(55);
        s.sweep(1, 10, Vec3::new(0.0, 0.0, -0.004));
        s.select(1, 0.0).frames(3);
    });
    add("monitors-trigger-and-stick", &|s| {
        s.monitors(Take::Hold).ray(1, monitor(1)).frames(3);
        s.select(1, 1.0).frames(4).select(1, 0.0).frames(3);
        s.stick(1, 0.0, 1.0).frames(45).stick(1, 0.0, 0.0).frames(2);
    });
    add("monitors-fist-takes", &move |s| {
        s.monitors(Take::Fist).pose(1, palm, Pose::Open);
        s.ray_from(1, palm, monitor(2)).frames(5);
        // Closing the hand: the runtime's ray goes.
        s.no_ray(1).pose(1, palm, Pose::Fist).frames(3);
        for i in 0..15u32 {
            s.pose(1, palm + Vec3::new(0.004 * i as f32, 0.0, 0.0), Pose::Fist)
                .frames(1);
        }
        s.pose(1, palm + Vec3::new(0.06, 0.0, 0.0), Pose::Open)
            .frames(3);
    });
    add("monitors-middle-pinch-scrolls", &move |s| {
        s.monitors(Take::Fist).pose(1, palm, Pose::Open);
        s.ray_from(1, palm, monitor(1)).frames(3);
        s.pose(1, palm, Pose::Middle).frames(2);
        for i in 0..12u32 {
            s.pose(
                1,
                palm + Vec3::new(0.0, 0.005 * i as f32, 0.0),
                Pose::Middle,
            )
            .frames(1);
        }
        s.pose(1, palm, Pose::Open).frames(3);
        // A middle pinch let go without moving: a right click.
        s.pose(1, palm, Pose::Middle)
            .frames(4)
            .pose(1, palm, Pose::Open)
            .frames(3);
    });
}

fn expected_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/input")
}

/// `journal` against `<name>.expected` (written instead, with `PROSTIR_BLESS=1`).
fn check(name: &str, journal: &[String]) -> Option<String> {
    let path = expected_dir().join(format!("{name}.expected"));
    let mut text = journal.join("\n");
    text.push('\n');
    if std::env::var_os("PROSTIR_BLESS").is_some() {
        std::fs::create_dir_all(expected_dir()).expect("tests/input");
        std::fs::write(&path, &text).expect("written");
        return None;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_default();
    (want != text).then(|| format!("{name}:\n--- expected\n{want}--- played\n{text}"))
}

/// Every script, written, read back and played, gives the journal kept for it.
#[test]
fn the_scripts_play_as_they_did() {
    let mut wrong = Vec::new();
    for (name, script) in scripts() {
        let rec = Recording::read(&script.recording()).expect("read back");
        assert_eq!(rec.frames.len(), script.frames.len(), "{name}");
        let journal = replay(&rec, Instant::now());
        // These do nothing, on purpose.
        let quiet = [
            "hands-typing",
            "pinch-held-drifting-onto-the-menu",
            "monitors-touch-from-behind",
            "monitors-typing-on-the-keyboard",
        ];
        if quiet.contains(&name) {
            assert!(journal.is_empty(), "{name} did something: {journal:?}");
        } else {
            assert!(!journal.is_empty(), "{name}: nothing");
        }
        // What each of step 8's scripts must come to, whatever its numbers.
        let has = |what: &str| journal.iter().any(|l| l.contains(what));
        let wanted: &[&str] = match name {
            "monitors-touch-tap" => &["touch down display 3", "touch up display 3"],
            "monitors-touch-swipe" => &["touch down", "touch move", "touch up"],
            "monitors-touch-through" => &["touch down", "touch cancel"],
            "monitors-pinch-click" => &["mouse display 1", "left down", "left up"],
            "monitors-pinch-drags" => &["left down", "mouse display 1", "left up"],
            "monitors-held-pinch-takes" => &["take display 1", "let-go display 1"],
            "monitors-trigger-and-stick" => &["left down", "left up", "scroll display 1"],
            "monitors-fist-takes" => &["take display 2", "drag display 2", "let-go display 2"],
            "monitors-middle-pinch-scrolls" => &["scroll display 1", "right down", "right up"],
            "carry-past-the-menu" => &["take display 1", "let-go display 1"],
            "keyboard-window" => &["take keyboard", "drag keyboard", "let-go keyboard"],
            "menu-toggled-during-a-pending-pinch" => &["menu closed", "menu open"],
            "monitors-middle-pinch-loses-a-frame" => &["right down", "right up"],
            "monitors-raised-hand-points" => &["mouse display 1"],
            _ => &[],
        };
        for what in wanted {
            assert!(has(what), "{name}: no \"{what}\" in {journal:#?}");
        }
        if name == "monitors-held-pinch-takes" {
            assert!(!has("left down"), "{name}: a take is no click");
        }
        if name == "carry-past-the-menu" || name == "menu-toggled-during-a-pending-pinch" {
            assert!(!has("press-menu"), "{name}: the menu pressed");
        }
        if name == "monitors-middle-pinch-loses-a-frame" {
            let rights = journal.iter().filter(|l| l.contains("right down")).count();
            assert_eq!(rights, 1, "{name}: {journal:#?}");
        }
        if name == "monitors-touch-through" {
            assert!(!has("touch up"), "{name}: pushed through is no tap");
        }
        wrong.extend(check(name, &journal));
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// Recordings made in the headset, each next to its journal.
#[test]
fn the_headsets_recordings_play_as_they_did() {
    let Ok(dir) = std::fs::read_dir(expected_dir()) else {
        return;
    };
    let mut wrong = Vec::new();
    for entry in dir.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "rec") {
            continue;
        }
        let bytes = std::fs::read(&path).expect("readable");
        let rec = Recording::read(&bytes).expect("a recording");
        let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
        wrong.extend(check(name, &replay(&rec, Instant::now())));
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// What goes in comes out: the head, the stage, each frame, bit for bit.
#[test]
fn a_recording_reads_back_as_written() {
    let mut s = Script::new();
    s.hand(1, true)
        .ray(1, monitor(1))
        .stick(0, 0.25, -0.5)
        .select(1, 1.0)
        .joints(0, Some(&open_palm(Vec3::new(-0.1, 1.3, -0.3), true)))
        .frames(3);
    s.input.menu_gesture = true;
    s.input.head = None;
    s.frames(1);
    let mut feel = Feel::DEFAULT;
    feel.hold_to_take_ms = 420.0;
    let start = Instant::now();
    let mut stage = stage();
    stage.large_menu = true;
    let mut r = Recorder::start(start, &head("0.2.0", &feel, &stage));
    for (t, input) in &s.frames {
        r.push(start + *t, input);
    }
    assert_eq!(r.frames(), 4);
    let rec = Recording::read(&r.into_bytes()).expect("read");
    assert_eq!(rec.app, "0.2.0");
    assert_eq!(rec.feel, feel);
    assert_eq!(rec.stage, stage);
    assert_eq!(rec.frames.len(), s.frames.len());
    for ((t0, a), (t1, b)) in s.frames.iter().zip(&rec.frames) {
        assert_eq!(t0, t1);
        assert_eq!(format!("{a:?}"), format!("{b:?}"));
    }
    assert!(Recording::read(b"something else\nframes\n").is_err());
}

/// A recording stops at its time, and never outgrows the room it took at the start.
#[test]
fn a_recording_stops_at_its_time() {
    let start = Instant::now();
    let mut r = Recorder::start(start, &head("t", &Feel::DEFAULT, &stage()));
    let room = r.bytes.capacity();
    let input = Script::new().input;
    let mut t = Duration::ZERO;
    while t < RECORD_FOR + Duration::from_secs(1) {
        r.push(start + t, &input);
        t += Duration::from_millis(8);
    }
    assert!(r.over(start + RECORD_FOR));
    assert_eq!(r.frames(), (RECORD_FOR.as_millis() / 8) as u32);
    assert_eq!(r.bytes.capacity(), room, "no more memory taken");
}

/// Each frame of `script` played against the stage, `look` asked of the hands after
/// each: what it said, frame by frame.
fn watch<T>(script: &Script, look: impl Fn(&super::super::Hands) -> T) -> Vec<T> {
    let start = Instant::now();
    let mut hands = super::super::Hands::new();
    let mut stage = stage();
    stage.modes = script.modes;
    let mut menus = StageMenus;
    script
        .frames
        .iter()
        .map(|(t, input)| {
            let now = start + *t;
            stage.at(start, now);
            hands.buttons(now, input, &mut stage);
            hands.step(now, input, &mut stage, &mut menus);
            look(&hands)
        })
        .collect()
}

/// A hand's ray goes as its fingertip comes nearer a monitor than 15 cm and comes back
/// only past 20 cm (hysteresis); with step 8's modes off it never goes.
#[test]
fn the_ray_gives_way_to_touch_near_a_monitor() {
    let palm_for = |tip: Vec3| tip - Vec3::new(0.025, 0.0, -0.1);
    let mut s = Script::new();
    s.monitors(Take::Hold);
    // In and out again, 1 cm a frame, with the runtime's ray along the finger.
    let zs: Vec<f32> = (0..=25)
        .map(|i| 0.30 - 0.01 * i as f32)
        .chain((0..=25).map(|i| 0.05 + 0.01 * i as f32))
        .collect();
    for &z in &zs {
        let tip = before_near(z);
        s.pose(1, palm_for(tip), Pose::Point);
        s.ray_from(1, palm_for(tip), before_near(0.0)).frames(1);
    }
    let shown = watch(&s, |h| h.ray(1).is_some());
    let first = |z: f32| {
        let i = zs.iter().position(|v| (v - z).abs() < 1e-4).expect("a z");
        shown[i]
    };
    assert!(first(0.30) && first(0.16), "far: the ray");
    assert!(!first(0.14) && !first(0.05), "near: touch");
    let back = zs.len()
        - 1
        - zs.iter()
            .rev()
            .position(|v| (v - 0.18).abs() < 1e-4)
            .expect("z");
    assert!(!shown[back], "18 cm on the way back: still touch");
    assert!(*shown.last().expect("frames"), "30 cm: the ray again");
    s.modes = Modes::default();
    assert!(
        watch(&s, |h| h.ray(1).is_some()).iter().all(|&r| r),
        "modes off"
    );
}

/// The ray on a monitor is the PC's pointer: the mouse follows it, and only the hand
/// that pressed last moves it.
#[test]
fn one_pointer_follows_the_hand_that_pressed_last() {
    let mut s = Script::new();
    s.monitors(Take::Hold);
    s.ray_from(0, Vec3::new(-0.2, 1.3, -0.25), monitor(1));
    s.ray_from(1, Vec3::new(0.2, 1.3, -0.25), monitor(2));
    s.frames(2);
    // The left one presses: from then on it moves the mouse, the right one only points.
    s.select(0, 1.0).frames(2).select(0, 0.0).frames(2);
    s.turn(1, 5, 1.0);
    let rec = Recording::read(&s.recording()).expect("read");
    let journal = replay(&rec, Instant::now());
    let moves: Vec<&String> = journal.iter().filter(|l| l.contains("mouse")).collect();
    let last = moves.last().expect("the mouse moved");
    assert!(last.contains("display 1"), "{journal:#?}");
}

/// Each frame of `script` played against the stage, `between` done to the hands and
/// the stage before frame `i`: the journal.
fn play_with(
    script: &Script,
    between: impl Fn(usize, &mut super::super::Hands, &mut Stage),
) -> Vec<String> {
    let start = Instant::now();
    let mut hands = super::super::Hands::new();
    let mut stage = stage();
    stage.modes = script.modes;
    let mut menus = StageMenus;
    for (i, (t, input)) in script.frames.iter().enumerate() {
        let now = start + *t;
        stage.at(start, now);
        between(i, &mut hands, &mut stage);
        hands.buttons(now, input, &mut stage);
        hands.step(now, input, &mut stage, &mut menus);
    }
    stage.journal()
}

/// The hands no longer the app's (the system menu) mid-touch and mid-click: the touch
/// is cancelled, the button let go, nothing stays pressed.
#[test]
fn cancelling_ends_what_presses_a_monitor() {
    let palm_for = |tip: Vec3| tip - Vec3::new(0.025, 0.0, -0.1);
    let mut s = Script::new();
    s.monitors(Take::Hold);
    for i in 0..=10 {
        let z = 0.12 - 0.0124 * i as f32;
        s.pose(1, palm_for(before_near(z)), Pose::Point).frames(1);
    }
    s.frames(3);
    let journal = play_with(&s, |i, hands, stage| {
        if i == 12 {
            hands.cancel_all(stage);
        }
    });
    let text = journal.join("\n");
    assert!(
        text.contains("touch down") && text.contains("touch cancel"),
        "{text}"
    );
    assert!(!text.contains("touch up"), "{text}");
    assert!(text.contains("cancel-holds"), "{text}");

    let mut s = Script::new();
    s.monitors(Take::Hold)
        .ray(1, monitor(1))
        .frames(2)
        .select(1, 1.0)
        .frames(6);
    let journal = play_with(&s, |i, hands, stage| {
        if i == 5 {
            hands.cancel_all(stage);
        }
    });
    let text = journal.join("\n");
    assert!(
        text.contains("left down") && text.contains("left up"),
        "{text}"
    );
}

/// The modes turned off in the middle of a click or a touch: the button goes up, the
/// touch is cancelled, the hands are not busy.
#[test]
fn turning_the_modes_off_mid_press_lets_go() {
    let mut s = Script::new();
    s.monitors(Take::Hold)
        .ray(1, monitor(1))
        .frames(2)
        .select(1, 1.0)
        .frames(6);
    let busy = std::cell::Cell::new(true);
    let journal = play_with(&s, |i, hands, stage| {
        if i == 5 {
            stage.modes = Modes::default();
        }
        if i == 7 {
            busy.set(hands.busy());
        }
    });
    let text = journal.join("\n");
    assert!(
        text.contains("left down") && text.contains("left up"),
        "{text}"
    );
    assert!(!busy.get(), "busy after the modes went off");
}

/// Where a fingertip is `x` to the side of and `z` in front of the middle of the
/// monitor within reach (3).
fn beside_near(x: f32, z: f32) -> Vec3 {
    let p = stage()
        .piece(Thing::Display(display(3)))
        .expect("on the stage")
        .place;
    p.position + p.rotation * Vec3::new(x, 0.0, z)
}

/// Hand `h` posed pointing, its fingertip from `from` to `to` in `n` frames.
fn point_to(s: &mut Script, h: usize, n: u32, from: Vec3, to: Vec3) {
    for i in 1..=n {
        let tip = from.lerp(to, i as f32 / n as f32);
        s.pose(h, tip - Vec3::new(0.025, 0.0, -0.1), Pose::Point)
            .frames(1);
    }
}

/// The lines of `journal` from `t` on (a line starts with its time in seconds).
fn from_time(journal: &[String], t: Duration) -> Vec<&String> {
    journal
        .iter()
        .filter(|l| {
            let at: f32 = l
                .split(['.', ' '])
                .take(2)
                .collect::<Vec<_>>()
                .join(".")
                .parse()
                .expect("a time");
            at + 1e-4 >= t.as_secs_f32()
        })
        .collect()
}

/// A hand lost in the middle of a touch: back within a moment (150 ms), the touch goes
/// on and lifts as a tap; lost for longer, it is cancelled there, never a tap.
#[test]
fn a_touch_waits_a_moment_for_a_lost_hand_then_is_cancelled() {
    for (lost, tap) in [(5, true), (20, false)] {
        let mut s = Script::new();
        s.monitors(Take::Hold);
        point_to(&mut s, 1, 10, before_near(0.14), before_near(-0.004));
        s.joints(1, None).frames(lost);
        point_to(&mut s, 1, 2, before_near(-0.004), before_near(-0.004));
        point_to(&mut s, 1, 6, before_near(-0.004), before_near(0.08));
        let journal = replay(
            &Recording::read(&s.recording()).expect("read"),
            Instant::now(),
        );
        let has = |what: &str| journal.iter().any(|l| l.contains(what));
        assert!(has("touch down display 3 slot 1"), "{journal:#?}");
        assert_eq!(has("touch up"), tap, "lost {lost} frames: {journal:#?}");
        assert_eq!(
            has("touch cancel"),
            !tap,
            "lost {lost} frames: {journal:#?}"
        );
        let downs = journal.iter().filter(|l| l.contains("touch down")).count();
        assert_eq!(downs, 1, "no second touch as it comes back: {journal:#?}");
    }
}

/// A pinch held on the monitor within reach while the fingertip comes nearer than
/// 15 cm: the ray stays until the pinch is let go (a click, at its point), and only
/// then gives way to touch.
#[test]
fn the_ray_gives_way_to_touch_only_once_let_go() {
    let mut s = Script::new();
    // A fist takes: a pinch held still is no take.
    s.monitors(Take::Fist);
    let ray_on = |s: &mut Script, z: f32| {
        let palm = before_near(z) - Vec3::new(0.025, 0.0, -0.1);
        s.pose(1, palm, Pose::Open)
            .ray_from(1, palm, before_near(0.0));
    };
    ray_on(&mut s, 0.30);
    s.frames(3).select(1, 1.0).frames(2);
    let pressed = s.frames.len();
    for i in 0..=12 {
        ray_on(&mut s, 0.30 - 0.02 * i as f32);
        s.frames(1);
    }
    let held = s.frames.len();
    s.select(1, 0.0).frames(1).frames(3);
    let shown = watch(&s, |h| h.ray(1).is_some());
    assert!(
        shown[pressed..held].iter().all(|&r| r),
        "the ray while pressed: {shown:?}"
    );
    assert!(!*shown.last().expect("frames"), "touch once let go");
    let journal = replay(
        &Recording::read(&s.recording()).expect("read"),
        Instant::now(),
    );
    let text = journal.join("\n");
    assert!(
        text.contains("left down display 3") && text.contains("left up display 3"),
        "{text}"
    );
    assert!(!text.contains("touch"), "{text}");
}

/// One finger touches at a time: the other hand's fingertip crossing the surface while
/// the first one touches only hovers; once the first lifts, it touches, with its own
/// slot.
#[test]
fn one_finger_touches_at_a_time() {
    let mut s = Script::new();
    s.monitors(Take::Hold);
    let (right, left) = (|z| beside_near(0.1, z), |z| beside_near(-0.1, z));
    point_to(&mut s, 1, 10, right(0.14), right(-0.004));
    point_to(&mut s, 0, 10, left(0.14), left(-0.004));
    // The right one lifts; the left one comes out and in again.
    point_to(&mut s, 1, 6, right(-0.004), right(0.3));
    point_to(&mut s, 0, 4, left(-0.004), left(0.04));
    point_to(&mut s, 0, 4, left(0.04), left(-0.004));
    point_to(&mut s, 0, 6, left(-0.004), left(0.3));
    let journal = replay(
        &Recording::read(&s.recording()).expect("read"),
        Instant::now(),
    );
    let downs: Vec<&String> = journal
        .iter()
        .filter(|l| l.contains("touch down"))
        .collect();
    assert_eq!(downs.len(), 2, "{journal:#?}");
    assert!(downs[0].contains("slot 1"), "{journal:#?}");
    assert!(downs[1].contains("slot 0"), "{journal:#?}");
    let first_up = journal
        .iter()
        .position(|l| l.contains("touch up display 3 slot 1"))
        .expect("the right one lifts");
    let second_down = journal
        .iter()
        .position(|l| l.contains("touch down display 3 slot 0"))
        .expect("the left one touches");
    assert!(first_up < second_down, "{journal:#?}");
}

/// The hand that touched last keeps the PC's pointer while its fingertip stays near
/// (its ray off): the other hand's ray passing over a monitor does not move the mouse
/// until that hand goes back past 20 cm.
#[test]
fn a_hand_that_touched_keeps_the_pointer_while_near() {
    let mut s = Script::new();
    s.monitors(Take::Hold);
    point_to(&mut s, 1, 10, before_near(0.14), before_near(-0.004));
    point_to(&mut s, 1, 4, before_near(-0.004), before_near(0.08));
    // A controller in the left hand, its ray across monitor 1.
    s.ray(0, monitor(1)).frames(2);
    s.turn(0, 20, 0.5);
    let back = s.t;
    point_to(&mut s, 1, 6, before_near(0.08), before_near(0.3));
    s.turn(0, 10, 0.5);
    let journal = replay(
        &Recording::read(&s.recording()).expect("read"),
        Instant::now(),
    );
    assert!(
        journal.iter().any(|l| l.contains("touch up display 3")),
        "{journal:#?}"
    );
    let mouse: Vec<&String> = journal.iter().filter(|l| l.contains("mouse")).collect();
    assert!(
        !mouse.is_empty(),
        "the ray moves it in the end: {journal:#?}"
    );
    let later = from_time(&journal, back);
    assert!(
        mouse.iter().all(|l| later.contains(l)),
        "moved before the hand went back: {journal:#?}"
    );
}
