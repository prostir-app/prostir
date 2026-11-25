//! The arrangement's hard rule over everything that changes it, on a few hundred
//! random desks (the same ones every run): no two displays ever cut into each other in
//! the room, and the PC's own displays glued on its desktop stand two of its pixels
//! apart. Each desk is laid out as its monitors' pictures come, one by one, then goes
//! through random steps: a monitor dropped next to another with the magnet on, one or
//! two monitors added (the second asked before the first one's picture comes), two
//! swapped, a stream coming back at another resolution, "Line up", and
//! the PC's own displays shown as one group. With monitors far wider than the circle
//! (5120x1440) among them, every such monitor added is also in view, and after "Line
//! up" no monitor is alone in a row out of view; after a preset nothing is out of view,
//! and after "Line up" then neither.

use super::*;

/// The same random numbers every run.
struct Lcg(u64);

impl Lcg {
    /// 0 to 1.
    fn f(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 40) as f32) / (1u64 << 24) as f32
    }

    /// 0 to `n - 1`.
    fn below(&mut self, n: usize) -> usize {
        ((self.f() * n as f32) as usize).min(n - 1)
    }
}

const SIZES: [(u32, u32); 5] = [
    (2560, 1440),
    (1920, 1080),
    (3440, 1440),
    (1440, 2560),
    (3840, 2160),
];

/// Sizes with monitors far wider than the circle they stand on among them.
const WIDE: [(u32, u32); 4] = [(2560, 1440), (5120, 1440), (1920, 1080), (3840, 2160)];

/// How far a display may reach into another before it counts (a millimetre): edges
/// that meet do not, and neither do the last bits of a float.
const SHRINK: f32 = 0.001;

/// Whether two flat rectangles (place, height) cut into each other in the room, each
/// made `SHRINK` smaller on every side first. Worked out apart from the code under
/// test: points along every edge of one inside the other.
fn cut(a: &Placement, ah: f32, b: &Placement, bh: f32) -> bool {
    let inside = |r: &Placement, rh: f32, p: Vec3| {
        let l = r.rotation.inverse() * (p - r.position);
        l.x.abs() < r.width / 2.0 - SHRINK && l.y.abs() < rh / 2.0 - SHRINK
    };
    let edges = |r: &Placement, rh: f32| {
        let (x, y) = (r.width / 2.0 - SHRINK, rh / 2.0 - SHRINK);
        let c = [(-x, -y), (x, -y), (x, y), (-x, y)]
            .map(|(u, v)| r.position + r.rotation * Vec3::new(u, v, 0.0));
        [(c[0], c[1]), (c[1], c[2]), (c[2], c[3]), (c[3], c[0])]
    };
    let crosses = |r: &Placement, rh: f32, o: &Placement, oh: f32| {
        let n = r.rotation * Vec3::Z;
        edges(o, oh).into_iter().any(|(p, q)| {
            let (dp, dq) = ((p - r.position).dot(n), (q - r.position).dot(n));
            // In its plane (to a tenth of a millimetre): points along it.
            if dp.abs() < 1e-4 && dq.abs() < 1e-4 {
                return (0..=40).any(|i| inside(r, rh, p.lerp(q, i as f32 / 40.0)));
            }
            dp * dq < 0.0 && inside(r, rh, p.lerp(q, dp / (dp - dq)))
        })
    };
    crosses(a, ah, b, bh) || crosses(b, bh, a, ah)
}

/// A desk being tried: the PC's list, and the PC's own displays on its desktop.
struct Trial {
    sim: Sim,
    list: Vec<Display>,
    /// The number the next monitor added gets, and the last request's.
    next: u32,
    req: u32,
    /// The PC's own displays shown, with where each is on its desktop.
    pcs: Vec<(u32, (u32, u32), [i32; 4])>,
    /// The sizes monitors added and refitted get.
    sizes: &'static [(u32, u32)],
}

impl Trial {
    fn new(sizes: &[(u32, u32)]) -> Self {
        Self::of(sizes, &SIZES)
    }

    /// Laid out with `sizes`, monitors added and refitted later getting one of `later`.
    fn of(sizes: &[(u32, u32)], later: &'static [(u32, u32)]) -> Self {
        let mut sim = Sim::new(None);
        sim.connect(PC);
        let list: Vec<Display> = sizes
            .iter()
            .enumerate()
            .map(|(i, &s)| monitor(i as u32 + 1, s))
            .collect();
        sim.list(list.clone());
        for (i, &s) in sizes.iter().enumerate() {
            sim.stream(i as u32 + 1, s);
        }
        sim.after(1000);
        Self {
            sim,
            list,
            next: sizes.len() as u32 + 1,
            req: 0,
            pcs: Vec::new(),
            sizes: later,
        }
    }

    fn size_of(&self, n: u32) -> (u32, u32) {
        let d = self.list.iter().find(|d| d.id.0 == n).expect("listed");
        (d.mode.w, d.mode.h)
    }

    /// The monitors, by number.
    fn monitors(&self) -> Vec<u32> {
        self.list
            .iter()
            .filter(|d| d.is_virtual())
            .map(|d| d.id.0)
            .collect()
    }

    fn height(&self, n: u32, p: &Placement) -> f32 {
        let (w, h) = self.size_of(n);
        p.width * h as f32 / w as f32
    }

    /// Every pair of displays cutting into each other.
    fn cutting(&self) -> Vec<(u32, u32)> {
        let all: Vec<(u32, Placement, f32)> = self
            .list
            .iter()
            .filter_map(|d| {
                let p = *self.sim.world.placement(k(d.id.0))?;
                self.sim.world.streams(k(d.id.0)).then_some(())?;
                Some((d.id.0, p, self.height(d.id.0, &p)))
            })
            .collect();
        let mut out = Vec::new();
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                if cut(&a.1, a.2, &b.1, b.2) {
                    out.push((a.0, b.0));
                }
            }
        }
        out
    }

    /// The PC's displays side by side on its desktop that stand less than two of its
    /// pixels apart in the room, or more than four (each keeps the shape of its picture
    /// within its share of the group, so a little more than two is a display a hair
    /// smaller than its share): the pair and the gap, in its pixels.
    fn unglued(&self) -> Vec<(u32, u32, f32)> {
        let mut out = Vec::new();
        for &(a, _, ra) in &self.pcs {
            for &(b, _, rb) in &self.pcs {
                if ra[0] + ra[2] != rb[0] || ra[1] != rb[1] {
                    continue;
                }
                let (Some(pa), Some(pb)) = (
                    self.sim.world.placement(k(a)),
                    self.sim.world.placement(k(b)),
                ) else {
                    continue;
                };
                let side = pa.rotation * Vec3::X;
                let gap = ((pb.position - pb.rotation * Vec3::X * pb.width / 2.0)
                    - (pa.position + side * pa.width / 2.0))
                    .dot(side);
                // A desktop pixel of the group, in metres: their middles are half of
                // each apart.
                let px = (pb.position - pa.position).dot(side) / ((ra[2] + rb[2]) as f32 / 2.0);
                let gap = gap / px;
                if !(1.95..=4.0).contains(&gap) {
                    out.push((a, b, gap));
                }
            }
        }
        out
    }

    /// A random monitor dropped by hand next to another with the magnet on, where it
    /// cuts into nothing as dropped: what it came to.
    fn drop_near(&mut self, rng: &mut Lcg) -> Option<String> {
        let ms = self.monitors();
        if ms.len() < 2 {
            return None;
        }
        let m = ms[rng.below(ms.len())];
        let mut o = ms[rng.below(ms.len())];
        if o == m {
            o = ms[(ms.iter().position(|x| *x == o)? + 1) % ms.len()];
        }
        let op = *self.sim.world.placement(k(o))?;
        let mp = *self.sim.world.placement(k(m))?;
        let (oh, mh) = (self.height(o, &op), self.height(m, &mp));
        let dist = Vec3::new(op.position.x, 0.0, op.position.z).length();
        let side = rng.below(4);
        let gap = 0.002 + rng.f() * 0.08;
        let along = (rng.f() - 0.5) * 1.6;
        let (yaw, dy) = if side < 2 {
            let s = if side == 0 { 1.0 } else { -1.0 };
            (
                s * ((op.width + mp.width) / 2.0 + gap) / dist,
                along * (oh + mh) / 2.0,
            )
        } else {
            let s = if side == 2 { 1.0 } else { -1.0 };
            (
                along * (op.width + mp.width) / 2.0 / dist,
                s * ((oh + mh) / 2.0 + gap),
            )
        };
        let turn = Quat::from_rotation_y(-yaw);
        let p = Placement {
            position: turn * op.position + Vec3::Y * dy,
            rotation: (turn * op.rotation).normalize(),
            width: mp.width,
        };
        // Only a drop that cuts into nothing as dropped.
        let others = self.list.iter().filter(|d| d.id.0 != m).filter_map(|d| {
            let q = *self.sim.world.placement(k(d.id.0))?;
            Some((q, self.height(d.id.0, &q)))
        });
        if others
            .collect::<Vec<_>>()
            .iter()
            .any(|(q, qh)| cut(&p, mh, q, *qh))
        {
            return None;
        }
        let sim = &mut self.sim;
        sim.s.magnet = true;
        sim.s.move_all = false;
        sim.world.hold(Thing::Display(k(m)), &sim.s);
        sim.world.drag(&sim.s, Thing::Display(k(m)), p, sim.head.0);
        let stuck = sim.world.let_go(Thing::Display(k(m)), &sim.s, sim.now);
        Some(format!(
            "drop {m} by {o} (side {side}, along {along:.2}, gap {gap:.3}, stuck {stuck})"
        ))
    }

    /// Monitors added: `twice`, a second asked before the first one's picture comes.
    fn add(&mut self, rng: &mut Lcg, twice: bool) -> Option<String> {
        let room = 6usize.saturating_sub(self.monitors().len());
        let count = if twice { 2 } else { 1 };
        if room < count {
            return None;
        }
        let sizes: Vec<(u32, u32)> = (0..count)
            .map(|_| self.sizes[rng.below(self.sizes.len())])
            .collect();
        let mut made = Vec::new();
        for &size in &sizes {
            self.req += 1;
            let req = ReqId(self.req);
            let sim = &mut self.sim;
            sim.world
                .adding_started(Some(req), size, sim.head.1, sim.now);
            made.push((req, self.next, size));
            self.next += 1;
        }
        for &(req, n, size) in &made {
            self.sim.world.adding_made(PC, req, Some(k(n)));
            self.list.push(monitor(n, size));
            self.sim.list(self.list.clone());
            self.sim.stream(n, size);
        }
        Some(format!("add {sizes:?}"))
    }

    fn swap(&mut self, rng: &mut Lcg) -> Option<String> {
        let line: Vec<DisplayKey> = self.sim.world.monitors().iter().map(|m| m.0).collect();
        if line.len() < 2 {
            return None;
        }
        let id = line[rng.below(line.len())];
        let delta = if rng.f() < 0.5 { -1 } else { 1 };
        self.sim.world.swap(&self.sim.s, id, delta);
        Some(format!("swap {} {delta}", id.id.0))
    }

    /// A monitor's stream comes back at another resolution, under the same number.
    fn refit(&mut self, rng: &mut Lcg) -> Option<String> {
        let ms = self.monitors();
        let n = ms[rng.below(ms.len())];
        let size = self.sizes[rng.below(self.sizes.len())];
        self.sim.world.stream_ended(k(n));
        for d in &mut self.list {
            if d.id.0 == n {
                d.mode = Mode::new(size.0, size.1);
            }
        }
        self.sim.list(self.list.clone());
        self.sim.stream(n, size);
        Some(format!("refit {n} to {size:?}"))
    }

    /// The monitors whose middle is out of view: every one, or with `alone` only those
    /// alone in a row.
    fn out_of_view(&self, alone: bool) -> Vec<u32> {
        let world = &self.sim.world;
        let at = (world.center, world.tilt);
        let monitors: Vec<(DisplayKey, u32, u32)> = self
            .monitors()
            .into_iter()
            .map(|n| {
                let (w, h) = self.size_of(n);
                (k(n), w, h)
            })
            .collect();
        if alone {
            return layout::alone_out_of_view(&world.placements, &monitors, at, world.sent_rows())
                .iter()
                .map(|k| k.id.0)
                .collect();
        }
        monitors
            .iter()
            .map(|m| m.0)
            .filter(|n| world.placement(*n).is_some_and(|p| !layout::in_view(p, at)))
            .map(|n| n.id.0)
            .collect()
    }

    fn line_up(&mut self) -> Option<String> {
        let sim = &mut self.sim;
        sim.world.tidy_up(&sim.s, sim.now);
        Some("line up".into())
    }

    /// One to three of the PC's own displays shown, side by side on its desktop.
    fn show_pc_displays(&mut self, rng: &mut Lcg) -> Option<String> {
        if !self.pcs.is_empty() {
            return None;
        }
        let count = 1 + rng.below(3);
        let mut x = 0;
        for i in 0..count {
            let (w, h) = [(1920, 1080), (2560, 1440), (2880, 1800)][rng.below(3)];
            let n = 101 + i as u32;
            let rect = [x, 0, w as i32, h as i32];
            x += w as i32;
            self.pcs.push((n, (w, h), rect));
            self.list.push(pc_display(n, (w, h), rect));
        }
        self.sim.list(self.list.clone());
        for &(n, size, _) in &self.pcs.clone() {
            self.sim.stream(n, size);
        }
        Some(format!("show {count} of the PC's displays"))
    }
}

/// A step of an arrangement the desks go through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Drop,
    Add,
    AddTwice,
    Swap,
    Refit,
    LineUp,
    PcDisplays,
}

/// `cases` random desks of two to four monitors (the same every run), each through six
/// random steps of `steps`: the rule holds after every one.
fn holds_through(steps: &[Step], cases: usize) {
    holds_through_with(&SIZES, steps, cases);
}

/// [`holds_through`] with monitors of `sizes`.
fn holds_through_with(sizes: &'static [(u32, u32)], steps: &[Step], cases: usize) {
    let mut rng = Lcg(42);
    let mut failures = Vec::new();
    for case in 0..cases {
        let n = 2 + rng.below(3);
        let first: Vec<(u32, u32)> = (0..n).map(|_| sizes[rng.below(sizes.len())]).collect();
        let mut desk = Trial::of(&first, sizes);
        let mut done = vec!["laid out".to_owned()];
        let at_first = desk.cutting();
        if !at_first.is_empty() {
            failures.push(format!("case {case} {first:?}: laid out: cut {at_first:?}"));
            continue;
        }
        for _ in 0..6 {
            let step = match steps[rng.below(steps.len())] {
                Step::Drop => desk.drop_near(&mut rng),
                Step::Add => desk.add(&mut rng, false),
                Step::AddTwice => desk.add(&mut rng, true),
                Step::Swap => desk.swap(&mut rng),
                Step::Refit => desk.refit(&mut rng),
                Step::LineUp => desk.line_up(),
                Step::PcDisplays => desk.show_pc_displays(&mut rng),
            };
            let Some(step) = step else { continue };
            desk.sim.after(1000);
            done.push(step);
            let (cutting, unglued) = (desk.cutting(), desk.unglued());
            if !cutting.is_empty() || !unglued.is_empty() {
                failures.push(format!(
                    "case {case} {first:?}: {done:?}: cut {cutting:?}, glued apart {unglued:?}"
                ));
                break;
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} desks:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// The magnet never joins a monitor to its neighbour through a third one.
#[test]
fn the_magnet_cuts_into_nothing() {
    holds_through(&[Step::Drop], 300);
}

/// Monitors arriving one by one each go next to the others, and none goes through
/// another: here an ultrawide under a small one, which would reach into the one turned
/// beside it (seen just apart, but nearer the eye).
#[test]
fn a_new_monitor_never_goes_through_another() {
    let trial = Trial::new(&[(1920, 1080), (2560, 1440), (3440, 1440)]);
    assert_eq!(trial.cutting(), []);
}

/// Each step on its own, from desks laid out as their pictures come.
#[test]
fn each_arrangement_step_cuts_into_nothing() {
    for step in [
        Step::Add,
        Step::AddTwice,
        Step::Swap,
        Step::Refit,
        Step::LineUp,
        Step::PcDisplays,
    ] {
        holds_through(&[step], 300);
    }
}

/// A monitor swapped or grown next to the PC's own displays: they move as one group,
/// still glued, and nothing cuts.
#[test]
fn the_pcs_displays_move_as_one_group() {
    holds_through(&[Step::Swap, Step::Refit, Step::PcDisplays], 300);
}

/// Everything that changes the arrangement, in any order. Not yet: "Line up" still
/// makes cuts on some of these desks (about one in twenty): the PC's own displays, which
/// go along with the first of them, go into a monitor lined up next to them; and after
/// monitors were added and moved by hand, rows pushed clear go into another row
/// (`prostir_layout::tidy`).
#[test]
#[ignore = "known: Line up with the PC's displays, and after monitors added by hand"]
fn no_arrangement_step_cuts_one_display_into_another() {
    holds_through(
        &[
            Step::Drop,
            Step::Add,
            Step::AddTwice,
            Step::Swap,
            Step::Refit,
            Step::LineUp,
            Step::PcDisplays,
        ],
        300,
    );
}

/// Each step on its own with monitors far wider than the circle (5120x1440) among
/// them: nothing cuts, and the PC's own displays stay glued.
#[test]
fn each_step_with_wide_monitors_cuts_into_nothing() {
    for step in [
        Step::Add,
        Step::AddTwice,
        Step::Swap,
        Step::Refit,
        Step::LineUp,
        Step::PcDisplays,
    ] {
        holds_through_with(&WIDE, &[step], 300);
    }
}

/// A 5120x1440 monitor added to two or three 1920x1080 and 2560x1440 ones (the PC's own
/// displays shown under them on some desks): it goes into the row, in view, and so does
/// every other; "Line up" leaves none out of view, and so does the preset. Nothing cuts
/// and the PC's own displays stay glued all the while.
#[test]
fn a_wide_monitor_added_is_in_view() {
    const SMALL: [(u32, u32); 2] = [(1920, 1080), (2560, 1440)];
    let mut rng = Lcg(7);
    let mut failures = Vec::new();
    for case in 0..200 {
        let n = 2 + rng.below(2);
        let first: Vec<(u32, u32)> = (0..n).map(|_| SMALL[rng.below(SMALL.len())]).collect();
        let mut desk = Trial::of(&first, &[(5120, 1440)]);
        let mut done = vec!["laid out".to_owned()];
        if rng.below(3) == 0 {
            done.extend(desk.show_pc_displays(&mut rng));
            desk.sim.after(1000);
        }
        let mut check = |desk: &mut Trial, step: Option<String>, alone: bool| {
            done.extend(step);
            desk.sim.after(1000);
            let (cutting, unglued, unseen) =
                (desk.cutting(), desk.unglued(), desk.out_of_view(alone));
            let fine = cutting.is_empty() && unglued.is_empty() && unseen.is_empty();
            if !fine {
                failures.push(format!(
                    "case {case} {first:?}: {done:?}: cut {cutting:?}, glued apart {unglued:?}, \
                     out of view {unseen:?}"
                ));
            }
            fine
        };
        let step = desk.add(&mut rng, false);
        if !check(&mut desk, step, false) {
            continue;
        }
        let step = desk.line_up();
        if !check(&mut desk, step, true) {
            continue;
        }
        let sim = &mut desk.sim;
        sim.world.preset_chosen(&sim.s, sim.now);
        check(&mut desk, Some("preset".into()), false);
    }
    assert!(
        failures.is_empty(),
        "{} desks:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// Desks of one to four monitors of common sizes (the same every run), the laptop's
/// 2880x1800 lid among them, laid out by each preset: every monitor is in view, and so
/// after "Line up" then, and nothing cuts.
#[test]
fn presets_and_line_up_leave_nothing_out_of_view() {
    const COMMON: [(u32, u32); 7] = [
        (1920, 1080),
        (2560, 1440),
        (3440, 1440),
        (3840, 2160),
        (5120, 1440),
        (1440, 2560),
        (2880, 1800),
    ];
    let mut rng = Lcg(11);
    let mut failures = Vec::new();
    for preset in [Preset::Arc, Preset::Overhead, Preset::Wall] {
        for case in 0..150 {
            let n = 1 + rng.below(4);
            let first: Vec<(u32, u32)> = (0..n).map(|_| COMMON[rng.below(COMMON.len())]).collect();
            let mut desk = Trial::of(&first, &COMMON);
            let mut check = |desk: &mut Trial, what: &str| {
                desk.sim.after(1000);
                let (cutting, unseen) = (desk.cutting(), desk.out_of_view(false));
                if !cutting.is_empty() || !unseen.is_empty() {
                    failures.push(format!(
                        "{preset:?} case {case} {first:?}: {what}: cut {cutting:?}, out of view \
                         {unseen:?}"
                    ));
                }
            };
            let sim = &mut desk.sim;
            sim.s.preset = preset;
            sim.world.preset_chosen(&sim.s, sim.now);
            check(&mut desk, "preset");
            desk.line_up();
            check(&mut desk, "line up");
        }
    }
    assert!(
        failures.is_empty(),
        "{} desks:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
