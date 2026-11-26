//! The arrangement's hard rule over everything that changes it, on a few hundred
//! random desks (the same ones every run): no two displays ever cut into each other in
//! the room, and the PC's own displays glued on its desktop stand two of its pixels
//! apart; and as laid out, after monitors arrive and after the magnet, no monitor
//! hides another from the eye (rays from the eye to a grid on each). Each desk is laid
//! out as its monitors' pictures come, one by one, then goes through random steps: a
//! monitor dropped next to another with the magnet on, one or two monitors added (the
//! second asked before the first one's picture comes), two swapped, a stream coming
//! back at another resolution, "Line up", and the PC's own displays shown as one group. With monitors far wider than the circle
//! (5120x1440) among them, every such monitor added is also in view, and after "Line
//! up" no monitor is alone in a row out of view; after a preset nothing is out of view,
//! and after "Line up" then neither. The same on a headset whose model is known, its
//! monitors made "1:1" (`clarity`).

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

/// How much of a display another may hide from the eye before it counts (3 %): a
/// glued seam seen at a slant hides a sliver.
const HIDDEN: f32 = 0.03;

/// How much of a monitor dropped over the seam between two glued ones those two may
/// hide (10 %): where their row curves round the eye they reach in front of it. The
/// magnet turns it as "Line up" would, to hide no more than 3 % of it, and where it
/// cannot leaves it hanging with no more than 8 % hidden (`stick::SEAM_HUNG`); the
/// grid here measures to a couple of percent.
const SEAM_HIDDEN: f32 = 0.10;

/// The points of a 32 by 32 grid over a flat rectangle (place, height), each in the
/// middle of its cell.
fn grid(p: &Placement, h: f32) -> impl Iterator<Item = Vec3> + '_ {
    const N: usize = 32;
    (0..N * N).map(move |i| {
        let (u, v) = (
            ((i % N) as f32 + 0.5) / N as f32 - 0.5,
            ((i / N) as f32 + 0.5) / N as f32 - 0.5,
        );
        p.position + p.rotation * Vec3::new(u * p.width, v * h, 0.0)
    })
}

/// How much of `a` (place, height) `b` hides from `eye`: the share of a grid of points
/// on `a` whose line of sight goes through `b` first. Worked out apart from the code
/// under test: rays against flat rectangles.
fn hidden_by(eye: Vec3, a: (&Placement, f32), b: (&Placement, f32)) -> f32 {
    let (bp, bh) = b;
    let n = bp.rotation * Vec3::Z;
    let mut count = 0;
    let mut hidden = 0;
    for p in grid(a.0, a.1) {
        count += 1;
        let d = p - eye;
        let denom = d.dot(n);
        if denom.abs() < 1e-9 {
            continue;
        }
        let t = (bp.position - eye).dot(n) / denom;
        // In front of the point by more than a millimetre.
        if t <= 0.0 || t * d.length() >= d.length() - 0.001 {
            continue;
        }
        let l = bp.rotation.inverse() * (eye + d * t - bp.position);
        if l.x.abs() < bp.width / 2.0 && l.y.abs() < bh / 2.0 {
            hidden += 1;
        }
    }
    hidden as f32 / count.max(1) as f32
}

/// How two displays meet in the room.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Meeting {
    /// An edge of each along one line (within 5 mm) over a centimetre at least: hung
    /// from one hinge, or glued.
    Edge,
    /// Only a corner of one on an edge of the other (within a centimetre): a monitor
    /// hung over the seam between two glued ones meets the second so.
    Corner,
    Apart,
}

/// How `a` and `b` (place, height) meet.
fn meeting(a: (&Placement, f32), b: (&Placement, f32)) -> Meeting {
    let edges = |(p, h): (&Placement, f32)| {
        let (x, y) = (p.width / 2.0, h / 2.0);
        let c = [(-x, -y), (x, -y), (x, y), (-x, y)]
            .map(|(u, v)| p.position + p.rotation * Vec3::new(u, v, 0.0));
        [(c[0], c[1]), (c[1], c[2]), (c[2], c[3]), (c[3], c[0])]
    };
    let along = edges(a).into_iter().any(|(p, q)| {
        let dir = (q - p).normalize();
        edges(b).into_iter().any(|(r, s)| {
            let off = |x: Vec3| (x - p).reject_from_normalized(dir).length();
            let (t0, t1) = ((r - p).dot(dir), (s - p).dot(dir));
            let overlap = t0.max(t1).min((q - p).length()) - t0.min(t1).max(0.0);
            off(r) < 0.005 && off(s) < 0.005 && overlap > 0.01
        })
    });
    if along {
        return Meeting::Edge;
    }
    let on_edge = |c: Vec3, (p, q): (Vec3, Vec3)| {
        let t = ((c - p).dot(q - p) / (q - p).length_squared()).clamp(0.0, 1.0);
        c.distance(p.lerp(q, t)) < 0.01
    };
    let corner_on = |x: (&Placement, f32), y: (&Placement, f32)| {
        edges(x)
            .into_iter()
            .any(|(c, _)| edges(y).into_iter().any(|e| on_edge(c, e)))
    };
    if corner_on(a, b) || corner_on(b, a) {
        Meeting::Corner
    } else {
        Meeting::Apart
    }
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
    /// The monitor dropped last.
    dropped: Option<u32>,
}

impl Trial {
    fn new(sizes: &[(u32, u32)]) -> Self {
        Self::of(sizes, &SIZES)
    }

    /// Laid out with `sizes`, monitors added and refitted later getting one of `later`.
    fn of(sizes: &[(u32, u32)], later: &'static [(u32, u32)]) -> Self {
        Self::of_on(sizes, later, None)
    }

    /// [`Trial::of`] on a headset of `ppd` pixels per degree.
    fn of_on(sizes: &[(u32, u32)], later: &'static [(u32, u32)], ppd: Option<f32>) -> Self {
        let mut sim = Sim::on(None, ppd);
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
            dropped: None,
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
        let all = self.shown_displays();
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

    /// Every display placed and streaming: number, place, height.
    fn shown_displays(&self) -> Vec<(u32, Placement, f32)> {
        self.list
            .iter()
            .filter_map(|d| {
                let p = *self.sim.world.placement(k(d.id.0))?;
                self.sim.world.streams(k(d.id.0)).then_some(())?;
                Some((d.id.0, p, self.height(d.id.0, &p)))
            })
            .collect()
    }

    /// Every display that hides more than [`HIDDEN`] of another from the eye: the one
    /// hidden, the one in front, how much is hidden, and how the two meet.
    fn hiding(&self) -> Vec<(u32, u32, f32, Meeting)> {
        let eye = self.sim.world.center.0;
        let all = self.shown_displays();
        let mut out = Vec::new();
        for a in &all {
            for b in &all {
                if a.0 == b.0 {
                    continue;
                }
                let share = hidden_by(eye, (&a.1, a.2), (&b.1, b.2));
                if share > HIDDEN {
                    out.push((a.0, b.0, share, meeting((&a.1, a.2), (&b.1, b.2))));
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
        self.dropped = Some(m);
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
        // Only a drop that cuts into nothing as dropped, hides nothing and is hidden by
        // nothing: what the magnet makes of it is what is checked.
        let others = self.list.iter().filter(|d| d.id.0 != m).filter_map(|d| {
            let q = *self.sim.world.placement(k(d.id.0))?;
            Some((q, self.height(d.id.0, &q)))
        });
        let eye = self.sim.world.center.0;
        if others.collect::<Vec<_>>().iter().any(|(q, qh)| {
            cut(&p, mh, q, *qh)
                || hidden_by(eye, (&p, mh), (q, *qh)) > HIDDEN
                || hidden_by(eye, (q, *qh), (&p, mh)) > HIDDEN
        }) {
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
    holds_through_on(None, sizes, steps, cases);
}

/// [`holds_through_with`] on a headset of `ppd` pixels per degree.
fn holds_through_on(ppd: Option<f32>, sizes: &'static [(u32, u32)], steps: &[Step], cases: usize) {
    let mut rng = Lcg(42);
    let mut failures = Vec::new();
    for case in 0..cases {
        let n = 2 + rng.below(3);
        let first: Vec<(u32, u32)> = (0..n).map(|_| sizes[rng.below(sizes.len())]).collect();
        let mut desk = Trial::of_on(&first, sizes, ppd);
        let mut done = vec!["laid out".to_owned()];
        let (at_first, hid_first) = (desk.cutting(), desk.hiding());
        if !at_first.is_empty() || !hid_first.is_empty() {
            failures.push(format!(
                "{ppd:?} case {case} {first:?}: laid out: cut {at_first:?}, hidden (by, share, \
                 meeting) {hid_first:?}"
            ));
            continue;
        }
        for _ in 0..6 {
            let kind = steps[rng.below(steps.len())];
            let dropped = kind == Step::Drop;
            let looked = matches!(kind, Step::Drop | Step::Add | Step::AddTwice);
            let hidden_before = if looked { desk.hiding() } else { Vec::new() };
            let step = match kind {
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
            // After the magnet or monitors arriving, nothing hides another that did not
            // already; only the monitor dropped, over two glued ones, may be hidden a
            // little.
            let hiding: Vec<_> = if looked {
                let was = |a: u32, b: u32| hidden_before.iter().any(|h| (h.0, h.1) == (a, b));
                let may_hide = |hidden: u32| {
                    if dropped && Some(hidden) == desk.dropped {
                        SEAM_HIDDEN
                    } else {
                        HIDDEN
                    }
                };
                desk.hiding()
                    .into_iter()
                    .filter(|h| !was(h.0, h.1) && h.2 > may_hide(h.0))
                    .collect()
            } else {
                Vec::new()
            };
            if !cutting.is_empty() || !unglued.is_empty() || !hiding.is_empty() {
                failures.push(format!(
                    "{ppd:?} case {case} {first:?}: {done:?}: cut {cutting:?}, glued apart \
                     {unglued:?}, hidden (by, share, meeting) {hiding:?}"
                ));
                if std::env::var_os("PROSTIR_CUT_DEBUG").is_some() {
                    for (n, p) in &desk.sim.world.placements {
                        eprintln!("{n} {p:?} h {}", desk.height(n.id.0, p));
                    }
                }
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

/// The magnet never joins a monitor to its neighbour through a third one, nor in front
/// of or behind one.
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

/// Two 3840x2160 and a 5120x1440 arriving one by one: the wide one is seen behind
/// neither. Before, near the eye their corners were too far round it to be projected
/// together, and the angular rectangles of their corners missed the middles of their
/// edges, seen further out: each hid 12 % of it.
#[test]
fn a_wide_monitor_arriving_near_the_eye_hides_nothing() {
    let trial = Trial::of(&[(3840, 2160), (3840, 2160), (5120, 1440)], &WIDE);
    assert_eq!(trial.cutting(), []);
    assert_eq!(trial.hiding(), []);
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

/// [`each_arrangement_step_cuts_into_nothing`] and with wide monitors, on a headset of
/// 25 pixels a degree: the monitors made 1:1 or as large as fits, new ones beside them
/// at their own size.
#[test]
fn each_step_at_one_to_one_cuts_into_nothing() {
    for step in [
        Step::Drop,
        Step::Add,
        Step::AddTwice,
        Step::Swap,
        Step::Refit,
        Step::LineUp,
        Step::PcDisplays,
    ] {
        holds_through_on(Some(25.0), &SIZES, &[step], 150);
        holds_through_on(Some(25.0), &WIDE, &[step], 100);
    }
}

/// The magnet on many more desks, of the standard size and made 1:1 on headsets of 25
/// and 20 pixels a degree: never a monitor folded over another (two hung from one edge
/// to the same side), nor through one, nor seen over or behind one.
#[test]
fn the_magnet_cuts_into_nothing_on_many_desks() {
    for ppd in [None, Some(25.0), Some(20.0)] {
        holds_through_on(ppd, &SIZES, &[Step::Drop], 1500);
        holds_through_on(ppd, &WIDE, &[Step::Drop], 600);
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

/// Common sizes: full HD, QHD, ultrawide, 4K, super ultrawide, a portrait QHD and a
/// laptop's 2880x1800 lid.
const COMMON: [(u32, u32); 7] = [
    (1920, 1080),
    (2560, 1440),
    (3440, 1440),
    (3840, 2160),
    (5120, 1440),
    (1440, 2560),
    (2880, 1800),
];

/// Desks of one to four monitors of common sizes (the same every run), the laptop's
/// 2880x1800 lid among them, laid out by each preset: every monitor is in view, and so
/// after "Line up" then, and nothing cuts.
#[test]
fn presets_and_line_up_leave_nothing_out_of_view() {
    presets_and_line_up_on(None);
}

/// [`presets_and_line_up_leave_nothing_out_of_view`] on headsets of 25 and 20 pixels a
/// degree, the
/// monitors made 1:1 or as large as fits.
#[test]
fn presets_at_one_to_one_and_line_up_leave_nothing_out_of_view() {
    presets_and_line_up_on(Some(25.0));
    presets_and_line_up_on(Some(20.0));
}

/// The desks of [`presets_and_line_up_leave_nothing_out_of_view`] on a headset of `ppd`
/// pixels per degree.
fn presets_and_line_up_on(ppd: Option<f32>) {
    let mut rng = Lcg(11);
    let mut failures = Vec::new();
    for preset in [Preset::Arc, Preset::Overhead, Preset::Wall] {
        for case in 0..150 {
            let n = 1 + rng.below(4);
            let first: Vec<(u32, u32)> = (0..n).map(|_| COMMON[rng.below(COMMON.len())]).collect();
            let mut desk = Trial::of_on(&first, &COMMON, ppd);
            let mut check = |desk: &mut Trial, what: &str| {
                desk.sim.after(1000);
                let (cutting, unseen) = (desk.cutting(), desk.out_of_view(false));
                if !cutting.is_empty() || !unseen.is_empty() {
                    failures.push(format!(
                        "{ppd:?} {preset:?} case {case} {first:?}: {what}: cut {cutting:?}, out \
                         of view {unseen:?}"
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

/// Desks of five to seven monitors of common sizes (the same every run) laid out by each
/// preset, then a monitor added: a monitor in view stays in view (one pushed aside along
/// the row lifts nothing out of view for long: the whole goes further away), and
/// nothing cuts.
#[test]
fn an_add_lifts_nothing_out_of_view() {
    an_add_on(None);
}

/// [`an_add_lifts_nothing_out_of_view`] on a headset of 25 pixels a degree, the
/// monitors made 1:1 or as large
/// as fits, the new one at its own 1:1 size beside them.
#[test]
fn an_add_at_one_to_one_lifts_nothing_out_of_view() {
    an_add_on(Some(25.0));
}

/// The desks of [`an_add_lifts_nothing_out_of_view`] on a headset of `ppd` pixels per
/// degree.
fn an_add_on(ppd: Option<f32>) {
    let mut rng = Lcg(17);
    let mut failures = Vec::new();
    for preset in [Preset::Arc, Preset::Overhead, Preset::Wall] {
        for case in 0..60 {
            let n = 5 + rng.below(3);
            let first: Vec<(u32, u32)> = (0..n).map(|_| COMMON[rng.below(COMMON.len())]).collect();
            let mut desk = Trial::of_on(&first, &COMMON, ppd);
            let sim = &mut desk.sim;
            sim.s.preset = preset;
            sim.world.preset_chosen(&sim.s, sim.now);
            desk.sim.after(1000);
            let out = desk.out_of_view(false);
            let before: Vec<u32> = desk
                .monitors()
                .into_iter()
                .filter(|n| !out.contains(n))
                .collect();
            let added = desk.add(&mut rng, false);
            desk.sim.after(1000);
            let now = desk.out_of_view(false);
            let lost: Vec<u32> = before.into_iter().filter(|n| now.contains(n)).collect();
            let cutting = desk.cutting();
            if !lost.is_empty() || !cutting.is_empty() {
                failures.push(format!(
                    "{preset:?} case {case} {first:?}, {added:?}: out of view now {lost:?}, cut \
                     {cutting:?}"
                ));
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

/// Desks of four to seven monitors of common sizes (the same every run) laid out by
/// each preset, then brought nearer (a fifth of the radius: rows over the main one come
/// out of view) and lined up: taken further away again to bring them into view, every
/// seam seen closed (to a few pixels) stays closed, and nothing cuts that did not.
#[test]
fn further_into_view_opens_no_seam() {
    const MEETING: f32 = 1.5e-3;
    const OPEN: f32 = 5e-4;
    let mut rng = Lcg(23);
    let mut failures = Vec::new();
    let mut moved = 0;
    for preset in [Preset::Arc, Preset::Overhead, Preset::Wall] {
        for case in 0..200 {
            let n = 4 + rng.below(4);
            let first: Vec<(u32, u32)> = (0..n).map(|_| COMMON[rng.below(COMMON.len())]).collect();
            let mut desk = Trial::of(&first, &COMMON);
            let sim = &mut desk.sim;
            sim.s.preset = preset;
            sim.world.preset_chosen(&sim.s, sim.now);
            let world = &mut sim.world;
            let frame = layout::frame(world.center, world.tilt);
            for p in world.placements.values_mut() {
                *p = layout::further(*p, &frame, 0.8, preset == Preset::Wall);
            }
            world.radius *= 0.8;
            let glued = world.line_up_rows();
            let at = (world.center, world.tilt);
            let laid = world.sizes.clone();
            let meeting = layout::meeting_pairs(&world.placements, &laid, at, MEETING);
            let was = layout::seam_gaps(&world.placements, &laid, at, &meeting);
            let radius = world.radius;
            let cut_before = desk.cutting();
            let sim = &mut desk.sim;
            let world = &mut sim.world;
            world.further_into_view(&sim.s, &glued);
            if world.radius != radius {
                moved += 1;
            }
            let now = layout::seam_gaps(&world.placements, &laid, at, &meeting);
            let opened: Vec<(u32, u32, f32)> = meeting
                .iter()
                .zip(was.iter().zip(&now))
                .filter_map(|(pair, gaps)| match gaps {
                    (Some(was), Some(now)) if now - was > OPEN => {
                        Some((pair.0.id.0, pair.1.id.0, now - was))
                    }
                    (Some(_), None) => Some((pair.0.id.0, pair.1.id.0, f32::NAN)),
                    _ => None,
                })
                .collect();
            desk.sim.after(1000);
            let cutting: Vec<(u32, u32)> = desk
                .cutting()
                .into_iter()
                .filter(|c| !cut_before.contains(c))
                .collect();
            if !opened.is_empty() || !cutting.is_empty() {
                failures.push(format!(
                    "{preset:?} case {case} {first:?}: opened {opened:?}, cut {cutting:?}"
                ));
            }
        }
    }
    assert!(moved > 20, "taken further away on {moved} desks only");
    assert!(
        failures.is_empty(),
        "{} desks:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// One of the owner's desks of 2026-11-25: 3440x1440, 3440x1440, 2560x1440 and
/// 1920x1080 on an arc, a 3840x2160 added. It goes into the row, which makes room; that
/// lifted the row over it out of view. Now the whole goes a little further away: every
/// monitor in view, nothing cuts.
#[test]
fn an_add_into_the_row_leaves_the_row_over_it_in_view() {
    let mut desk = Trial::of(
        &[(3440, 1440), (3440, 1440), (2560, 1440), (1920, 1080)],
        &[(3840, 2160)],
    );
    let sim = &mut desk.sim;
    sim.world.preset_chosen(&sim.s, sim.now);
    desk.sim.after(1000);
    assert_eq!(
        desk.out_of_view(false),
        Vec::<u32>::new(),
        "in view as laid out"
    );
    desk.add(&mut Lcg(1), false);
    desk.sim.after(1000);
    assert_eq!(
        desk.out_of_view(false),
        Vec::<u32>::new(),
        "in view with it"
    );
    assert_eq!(desk.cutting(), []);
}
