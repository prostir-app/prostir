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
    out
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
        assert!(
            !journal.is_empty() || name == "hands-typing",
            "{name}: nothing"
        );
        if name == "hands-typing" {
            assert!(journal.is_empty(), "typing did something: {journal:?}");
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
            _ => &[],
        };
        for what in wanted {
            assert!(has(what), "{name}: no \"{what}\" in {journal:#?}");
        }
        if name == "monitors-held-pinch-takes" {
            assert!(!has("left down"), "{name}: a take is no click");
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
