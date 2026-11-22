//! The arrangement's hard rule over everything that changes it, on a few hundred
//! random desks (the same ones every run): no two displays ever cut into each other in
//! the room, and the PC's own displays glued on its desktop stand two of its pixels
//! apart. Each desk is laid out as its monitors' pictures come, one by one, then goes
//! through random steps: a monitor dropped next to another with the magnet on.

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
            if dp.abs() < SHRINK && dq.abs() < SHRINK {
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
    /// The PC's own displays shown, with where each is on its desktop.
    pcs: Vec<(u32, (u32, u32), [i32; 4])>,
}

impl Trial {
    fn new(sizes: &[(u32, u32)]) -> Self {
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
            pcs: Vec::new(),
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

    /// The PC's displays side by side on its desktop that stand other than two of its
    /// pixels apart in the room: the pair and the gap, in its pixels.
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
                // A desktop pixel of the group, in metres.
                let px = pa.width / (ra[2] as f32 - 2.0);
                let gap = gap / px;
                if (gap - 2.0).abs() > 0.05 {
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
}

/// A step of an arrangement the desks go through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Drop,
}

/// `cases` random desks of two to four monitors (the same every run), each through six
/// random steps of `steps`: the rule holds after every one.
fn holds_through(steps: &[Step], cases: usize) {
    let mut rng = Lcg(42);
    let mut failures = Vec::new();
    for case in 0..cases {
        let n = 2 + rng.below(3);
        let sizes: Vec<(u32, u32)> = (0..n).map(|_| SIZES[rng.below(SIZES.len())]).collect();
        let mut desk = Trial::new(&sizes);
        let mut done = vec!["laid out".to_owned()];
        let at_first = desk.cutting();
        if !at_first.is_empty() {
            failures.push(format!("case {case} {sizes:?}: laid out: cut {at_first:?}"));
            continue;
        }
        for _ in 0..6 {
            let step = match steps[rng.below(steps.len())] {
                Step::Drop => desk.drop_near(&mut rng),
            };
            let Some(step) = step else { continue };
            desk.sim.after(1000);
            done.push(step);
            let (cutting, unglued) = (desk.cutting(), desk.unglued());
            if !cutting.is_empty() || !unglued.is_empty() {
                failures.push(format!(
                    "case {case} {sizes:?}: {done:?}: cut {cutting:?}, glued apart {unglued:?}"
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
