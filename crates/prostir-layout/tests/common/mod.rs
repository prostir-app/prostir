//! What the layout tests share: a seeded generator of arrangements (the same failing
//! case comes back from the same seed), the headset's pipeline, a checker for every
//! invariant of `plans/layout-math.md`, the continuity measure, and a shrinker that
//! turns a failing case into a smallest one.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fmt::Write as _;

use glam::{Quat, Vec3};
use prostir_layout::circle::{self, Limits, OVERHEAD_PITCH, Preset, on_cylinder};
use prostir_layout::desk::{self, Boxes, Desk, Size, Spot, View, need};
use prostir_layout::rows::{KEEP, rows_of};
use prostir_layout::space::{
    Frame, METRES_PER_PIXEL, Monitor, PixelSize, Placement, Seen, Side, see,
};
use prostir_layout::view::Room;

/// splitmix64.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// `0..n`.
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }

    /// `lo..hi`.
    pub fn float(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }

    pub fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    pub fn pick<T: Copy>(&mut self, of: &[T]) -> T {
        of[self.below(of.len())]
    }
}

/// How many cases a property test runs, of the `full` count `plans/layout-math.md`
/// measures with: all of them with `PROSTIR_LAYOUT_FULL=1` in the environment, else a
/// fifth, so `cargo test --workspace` stays quick. The seeds are the same either way: a
/// quick run checks the first cases of a full one.
///
/// The full run: `PROSTIR_LAYOUT_FULL=1 cargo test --release -p prostir-layout -- --nocapture`
/// (`--nocapture` prints the measures).
pub fn cases(full: usize) -> usize {
    if std::env::var_os("PROSTIR_LAYOUT_FULL").is_some_and(|v| v == "1") {
        full
    } else {
        full.div_ceil(5)
    }
}

/// The stream sizes of the tests.
pub const PIXELS: [(u32, u32); 6] = [
    (1920, 1080),
    (2560, 1440),
    (3440, 1440),
    (3840, 2160),
    (5120, 1440),
    (1440, 2560),
];
/// The laptop screen's key.
pub const MIRROR: u8 = 100;

pub fn random_px(rng: &mut Rng) -> PixelSize {
    let (w, h) = rng.pick(&PIXELS);
    PixelSize::new(w, h)
}

/// Monitors in the room seen from a frame, and the primary.
#[derive(Debug, Clone)]
pub struct Scene {
    pub monitors: Vec<Monitor<u8>>,
    pub frame: Frame,
    pub main: Option<u8>,
}

impl Scene {
    pub fn seen(&self) -> Vec<Seen<u8>> {
        see(&self.frame, &self.monitors)
    }

    pub fn room(&self) -> Room<u8> {
        Room::new(&self.frame, &self.monitors)
    }

    /// Stream pixels: the headset's plan is in them.
    pub fn size(&self, k: u8) -> Option<Size> {
        self.monitors.iter().find(|m| m.id == k).map(|m| Size {
            w: m.px.w as i32,
            h: m.px.h as i32,
        })
    }

    pub fn get(&self, k: u8) -> Option<&Monitor<u8>> {
        self.monitors.iter().find(|m| m.id == k)
    }

    pub fn get_mut(&mut self, k: u8) -> Option<&mut Monitor<u8>> {
        self.monitors.iter_mut().find(|m| m.id == k)
    }

    /// The scene moved and turned as a whole (the frame too).
    pub fn moved(&self, turn: Quat, shift: Vec3) -> Scene {
        let around = self.frame.eye;
        let mv = |p: Vec3| around + shift + turn * (p - around);
        Scene {
            monitors: self
                .monitors
                .iter()
                .map(|m| Monitor {
                    placement: Placement {
                        position: mv(m.placement.position),
                        rotation: turn * m.placement.rotation,
                        width: m.placement.width,
                    },
                    ..*m
                })
                .collect(),
            frame: Frame {
                eye: around + shift,
                turn: turn * self.frame.turn,
            },
            main: self.main,
        }
    }

    /// Short text for a failing case.
    pub fn describe(&self) -> String {
        let mut s = String::new();
        let (yaw, pitch, _) = self.frame.turn.to_euler(glam::EulerRot::YXZ);
        let _ = writeln!(
            s,
            "frame eye {:.3?} yaw {:.1}° pitch {:.1}°, main {:?}",
            self.frame.eye,
            yaw.to_degrees(),
            pitch.to_degrees(),
            self.main
        );
        for m in &self.monitors {
            let l = self.frame.unplace(m.placement);
            let n = l.normal();
            let u = l.rotation * Vec3::Y;
            let _ = writeln!(
                s,
                "  {:>3} {}x{} at ({:.3}, {:.3}, {:.3}) m, facing ({:.3}, {:.3}, {:.3}), up ({:.3}, {:.3}, {:.3}), {:.3} m wide",
                m.id,
                m.px.w,
                m.px.h,
                l.position.x,
                l.position.y,
                l.position.z,
                n.x,
                n.y,
                n.z,
                u.x,
                u.y,
                u.z,
                l.width
            );
        }
        s
    }
}

/// A preset arrangement (the widening circle): 1 to 14 monitors, any sizes, Arc, Wall
/// or Overhead, a random primary, maybe the laptop screen, maybe curved, the whole
/// thing turned and moved anywhere in the room.
pub fn preset_scene(rng: &mut Rng) -> (Scene, Preset) {
    let (scene, preset, _) = preset_scene_curved(rng);
    (scene, preset)
}

/// [`preset_scene`] and the curve it was laid out with (a multiple of the radius).
pub fn preset_scene_curved(rng: &mut Rng) -> (Scene, Preset, Option<f32>) {
    let n = if rng.chance(70) {
        1 + rng.below(6)
    } else {
        1 + rng.below(14)
    };
    let sizes: Vec<(u8, PixelSize)> = (0..n as u8).map(|i| (i, random_px(rng))).collect();
    let preset = match rng.below(10) {
        0..6 => Preset::Arc,
        6..8 => Preset::Wall,
        _ => Preset::Overhead,
    };
    let primary = (!rng.chance(10)).then(|| rng.below(n) as u8);
    let mirror = rng.chance(40).then(|| {
        (
            MIRROR,
            rng.pick(&[
                PixelSize::new(1920, 1080),
                PixelSize::new(2880, 1800),
                PixelSize::new(2560, 1600),
            ]),
        )
    });
    let curve = match rng.below(3) {
        0 => None,
        1 => Some(2.0),
        _ => Some(1.0),
    };
    let placed = circle::arrange(preset, &sizes, primary, mirror, curve, &Limits::default());
    let mut monitors: Vec<Monitor<u8>> = sizes
        .iter()
        .chain(mirror.iter())
        .filter_map(|&(id, px)| {
            Some(Monitor {
                id,
                px,
                placement: *placed.get(&id)?,
            })
        })
        .collect();
    monitors.sort_by_key(|m| m.id);
    let frame = match preset {
        Preset::Overhead => Frame::tilted(Vec3::ZERO, 0.0, OVERHEAD_PITCH),
        _ => Frame::upright(Vec3::ZERO, 0.0),
    };
    let scene = Scene {
        monitors,
        frame,
        main: primary,
    };
    let turn = Quat::from_rotation_y(rng.float(-3.1, 3.1));
    let shift = Vec3::new(
        rng.float(-2.0, 2.0),
        rng.float(-0.5, 0.5),
        rng.float(-2.0, 2.0),
    );
    (scene.moved(turn, shift), preset, curve)
}

/// Rows built by hand on a cylinder around the eye: 1 to 3 rows of 1 to 5 monitors,
/// side by side, each offset from its neighbour by lining up (tops, middles, bottoms)
/// or on purpose (a sixth to two fifths of the shorter height, `deliberate`), every row
/// above resting on what is under it; then `noise` metres of a hand's noise on every
/// monitor. The first row made is the main one; with `below`, one more row hangs under
/// it.
pub fn rows_scene(rng: &mut Rng, deliberate: bool, noise: f32, below: bool) -> Scene {
    let k = 1 + rng.below(3);
    rows_scene_of(rng, k, None, deliberate, noise, below, false)
}

/// [`rows_scene_of`] with every row starting where the main one starts (in columns,
/// when the monitors are of one size).
pub fn rows_in_columns(rng: &mut Rng, k: usize, only: PixelSize) -> Scene {
    rows_scene_of(rng, k, Some(only), false, 0.0, false, true)
}

/// [`rows_scene`] with `k` rows (and one more under the main one with `below`), every
/// monitor of size `only` when given; with `columns`, every row starts at the same
/// azimuth as the main one. All it depends on comes in: tests running at once never
/// see each other's scenes.
pub fn rows_scene_of(
    rng: &mut Rng,
    k: usize,
    only: Option<PixelSize>,
    deliberate: bool,
    noise: f32,
    below: bool,
    columns: bool,
) -> Scene {
    let mut radius = rng.float(0.9, 1.6);
    let mut rows: Vec<Vec<(u8, PixelSize)>> = Vec::new();
    let mut id = 0u8;
    for _ in 0..k + usize::from(below) {
        let n = 1 + rng.below(5);
        rows.push(
            (0..n)
                .map(|_| {
                    id += 1;
                    (id - 1, only.unwrap_or_else(|| random_px(rng)))
                })
                .collect(),
        );
    }
    let m = METRES_PER_PIXEL;
    // Wide enough for the widest row to stay within 300°.
    for row in &rows {
        let widths: Vec<f32> = row.iter().map(|r| r.1.w as f32 * m).collect();
        let limits = Limits {
            radius,
            arc: 300f32.to_radians(),
            ..Limits::default()
        };
        radius = radius.max(circle::fit_radius(&widths, &limits));
    }
    let mut placed: BTreeMap<u8, Placement> = BTreeMap::new();
    let tops_of = |row: &[(u8, PixelSize)], rng: &mut Rng| -> Vec<f32> {
        let mut tops = vec![0.0f32];
        for i in 1..row.len() {
            let (ha, hb) = (row[i - 1].1.h as f32 * m, row[i].1.h as f32 * m);
            let shorter = ha.min(hb);
            let prev = tops[i - 1];
            let top = if deliberate && rng.chance(35) {
                let sign = if rng.chance(50) { 1.0 } else { -1.0 };
                let off = sign * rng.float(1.0 / 6.0, 0.4) * shorter;
                // Middles offset by `off`.
                prev - ha / 2.0 + hb / 2.0 + off
            } else {
                match rng.below(3) {
                    0 => prev,
                    1 => prev - ha / 2.0 + hb / 2.0,
                    _ => prev - ha + hb,
                }
            };
            tops.push(top);
        }
        tops
    };
    let lay = |row: &[(u8, PixelSize)],
               tops: &[f32],
               center: f32,
               lift: f32,
               out: &mut BTreeMap<u8, Placement>| {
        let total: f32 = row
            .iter()
            .map(|r| circle::angular_width(r.1.w as f32 * m, radius))
            .sum();
        let mut az = center - total / 2.0;
        for (i, &(id, px)) in row.iter().enumerate() {
            let (w, h) = (px.w as f32 * m, px.h as f32 * m);
            let a = circle::angular_width(w, radius);
            out.insert(
                id,
                on_cylinder(az + a / 2.0, tops[i] + lift - h / 2.0, w, radius),
            );
            az += a;
        }
    };
    // The main row, its first monitor's top at eye level.
    let main_row = rows[0].clone();
    let tops = tops_of(&main_row, rng);
    lay(&main_row, &tops, 0.0, 0.0, &mut placed);
    let azimuth = |p: &Placement| p.position.x.atan2(-p.position.z);
    let span = |id: u8, px: PixelSize, at: &BTreeMap<u8, Placement>| {
        let a = circle::angular_width(px.w as f32 * m, radius) / 2.0;
        let c = azimuth(&at[&id]);
        (c - a, c + a)
    };
    let mut below_ids: Vec<(u8, PixelSize)> = main_row.clone();
    let others: Vec<(Vec<(u8, PixelSize)>, f32)> = rows[1..k]
        .iter()
        .map(|r| (r.clone(), 1.0))
        .chain(below.then(|| (rows[k].clone(), -1.0)))
        .collect();
    for (row, up) in others {
        let tops = tops_of(&row, rng);
        let main_span = {
            let l = main_row
                .iter()
                .map(|&(id, px)| span(id, px, &placed).0)
                .fold(f32::INFINITY, f32::min);
            let r = main_row
                .iter()
                .map(|&(id, px)| span(id, px, &placed).1)
                .fold(f32::NEG_INFINITY, f32::max);
            (l, r)
        };
        let center = if columns {
            // Starting where the main row starts.
            let total: f32 = row
                .iter()
                .map(|r| circle::angular_width(r.1.w as f32 * m, radius))
                .sum();
            main_span.0 + total / 2.0
        } else {
            rng.float(main_span.0 * 0.6, main_span.1 * 0.6)
        };
        let mut trial = BTreeMap::new();
        lay(&row, &tops, center, 0.0, &mut trial);
        // Rest on (hang under) what it is over (under), as seen: each monitor as far as
        // it needs to be seen clear of what is under (over) it, the row by the most.
        let support: Vec<(u8, PixelSize)> = if up > 0.0 {
            below_ids.clone()
        } else {
            main_row.clone()
        };
        let flip = |p: Placement| Placement {
            position: Vec3::new(p.position.x, -p.position.y, p.position.z),
            ..p
        };
        let lower: Vec<(Placement, f32)> = support
            .iter()
            .map(|&(p, ppx)| {
                (
                    if up > 0.0 {
                        placed[&p]
                    } else {
                        flip(placed[&p])
                    },
                    ppx.h as f32 * m,
                )
            })
            .collect();
        let mut lift: Option<f32> = None;
        for &(u, upx) in &row {
            let uh = upx.h as f32 * m;
            let p = if up > 0.0 { trial[&u] } else { flip(trial[&u]) };
            if let Some(clear) = circle::clear_of(&[(p, uh)], &lower) {
                let need_lift = clear - (p.position.y - uh / 2.0);
                lift = Some(lift.map_or(need_lift, |l| l.max(need_lift)));
            }
        }
        let lift = lift.unwrap_or(0.0) * up;
        let _ = span;
        lay(&row, &tops, center, lift, &mut placed);
        if up > 0.0 {
            below_ids.extend(row.iter().copied());
        }
    }
    // A hand's noise.
    if noise > 0.0 {
        for p in placed.values_mut() {
            let r = Vec3::new(p.position.x, 0.0, p.position.z).length();
            let turn = Quat::from_rotation_y(rng.float(-noise, noise) / r);
            p.position = turn * p.position + Vec3::Y * rng.float(-noise, noise);
            p.rotation = turn * p.rotation;
        }
    }
    let monitors: Vec<Monitor<u8>> = rows
        .iter()
        .flatten()
        .map(|&(id, px)| Monitor {
            id,
            px,
            placement: placed[&id],
        })
        .collect();
    let main = Some(rows[0][rng.below(rows[0].len())].0);
    Scene {
        monitors,
        frame: Frame::upright(Vec3::ZERO, 0.0),
        main,
    }
}

/// A scene with the hand's mischief on top: nudges, a monitor hung lower or higher on
/// purpose, tilted, resized, pushed into a neighbour, sent far away, lifted into the
/// next row; the whole thing maybe lying down.
pub fn hand_scene(rng: &mut Rng) -> Scene {
    let mut scene = if rng.chance(50) {
        preset_scene(rng).0
    } else {
        let (deliberate, below) = (rng.chance(50), rng.chance(20));
        rows_scene(rng, deliberate, 0.0, below)
    };
    let n = scene.monitors.len();
    for _ in 0..1 + rng.below(3) {
        let i = rng.below(n);
        let frame = scene.frame;
        let h = scene.monitors[i].height();
        let mut p = frame.unplace(scene.monitors[i].placement);
        match rng.below(8) {
            0 => {
                p.position += Vec3::new(
                    rng.float(-0.03, 0.03),
                    rng.float(-0.03, 0.03),
                    rng.float(-0.03, 0.03),
                );
            }
            1 => p.position.y += h * rng.pick(&[0.25, -0.25, 0.33, -0.33]),
            2 => p.rotation = Quat::from_axis_angle(p.normal(), rng.float(-0.1, 0.1)) * p.rotation,
            3 => p.width *= rng.float(0.8, 1.25),
            4 => {
                let j = rng.below(n);
                let q = frame.unplace(scene.monitors[j].placement);
                p.position = p.position.lerp(q.position, 0.3);
            }
            5 => p.position *= rng.float(1.5, 2.5),
            6 => p.position.y += h,
            _ => {
                let turn = Quat::from_rotation_y(rng.float(-0.4, 0.4));
                p.position = turn * p.position;
                p.rotation = turn * p.rotation;
            }
        }
        scene.monitors[i].placement = frame.place(p);
    }
    if rng.chance(15) {
        // Lying down: everything and the frame tilted up.
        let tilt = Quat::from_rotation_x(rng.float(0.5, 1.2));
        scene = scene.moved(tilt, Vec3::ZERO);
    }
    scene
}

/// The headset's pipeline without history: rows from the view, the plan from the room.
pub fn pipeline(scene: &Scene) -> (Vec<Vec<u8>>, Vec<Spot<u8>>) {
    let room = scene.room();
    let ids: Vec<u8> = scene.monitors.iter().map(|m| m.id).collect();
    let rows = rows_of(&ids, &room, None);
    let desk = Desk {
        rows: rows.clone(),
        main: scene.main,
        ..Desk::default()
    };
    let plan = desk::plan(&desk, |k| scene.size(k), &room);
    (rows, plan)
}

pub fn spot(plan: &[Spot<u8>], id: u8) -> Option<Spot<u8>> {
    plan.iter().find(|s| s.id == id).copied()
}

/// Relative to `reference`, sorted by id.
pub fn relative(plan: &[Spot<u8>], reference: u8) -> Vec<Spot<u8>> {
    let r = spot(plan, reference).expect("reference placed");
    let mut v: Vec<Spot<u8>> = plan
        .iter()
        .map(|s| Spot {
            x: s.x - r.x,
            y: s.y - r.y,
            ..*s
        })
        .collect();
    v.sort_by_key(|s| s.id);
    v
}

// ---------------------------------------------------------------- the invariants

/// D1: nothing overlaps.
pub fn no_overlap(plan: &[Spot<u8>]) -> Result<(), String> {
    for (i, a) in plan.iter().enumerate() {
        for b in &plan[i + 1..] {
            if a.overlaps(b) {
                return Err(format!("D1: {a:?} overlaps {b:?}"));
            }
        }
    }
    Ok(())
}

/// D2: every output once, at its size.
pub fn complete(plan: &[Spot<u8>], wanted: &[(u8, Size)]) -> Result<(), String> {
    let mut got: Vec<(u8, Size)> = plan
        .iter()
        .map(|s| (s.id, Size { w: s.w, h: s.h }))
        .collect();
    got.sort_by_key(|g| g.0);
    let mut want = wanted.to_vec();
    want.sort_by_key(|g| g.0);
    want.dedup_by_key(|g| g.0);
    if got != want {
        return Err(format!("D2: placed {got:?}, wanted {want:?}"));
    }
    Ok(())
}

/// D3: the desktop starts at 0,0.
pub fn at_origin(plan: &[Spot<u8>]) -> Result<(), String> {
    let x = plan.iter().map(|s| s.x).min();
    let y = plan.iter().map(|s| s.y).min();
    if plan.is_empty() || (x == Some(0) && y == Some(0)) {
        Ok(())
    } else {
        Err(format!("D3: starts at {x:?},{y:?}"))
    }
}

/// D4 and D5: within each piece of a row (see `desk::pieces_of`), neighbours side by
/// side, left to right, sharing at least `need` of their edge (never a corner-only
/// touch); D11 for the pieces of a row: left to right without overlapping.
pub fn rows_side_by_side(plan: &[Spot<u8>], pieces: &[Vec<Vec<u8>>]) -> Result<(), String> {
    for row in pieces {
        for piece in row {
            for w in piece.windows(2) {
                let (Some(a), Some(b)) = (spot(plan, w[0]), spot(plan, w[1])) else {
                    continue;
                };
                if b.x != a.right() {
                    return Err(format!("D4: {a:?} and {b:?} of a row are not side by side"));
                }
                if a.along(&b) < need(a.h, b.h) {
                    return Err(format!(
                        "D5: {a:?} and {b:?} share {} of an edge",
                        a.along(&b)
                    ));
                }
            }
        }
        for w in row.windows(2) {
            let (Some(a), Some(b)) = (
                w[0].last().and_then(|&k| spot(plan, k)),
                w[1].first().and_then(|&k| spot(plan, k)),
            ) else {
                continue;
            };
            if b.x < a.right() {
                return Err(format!(
                    "D11: pieces {:?} and {:?} of a row overlap across",
                    w[0], w[1]
                ));
            }
        }
    }
    Ok(())
}

/// D7: a row above another is above it where they overlap across.
pub fn rows_in_order(plan: &[Spot<u8>], rows: &[Vec<u8>]) -> Result<(), String> {
    let row_of = |id: u8| rows.iter().position(|r| r.contains(&id));
    for a in plan {
        for b in plan {
            if let (Some(ra), Some(rb)) = (row_of(a.id), row_of(b.id))
                && ra < rb
                && a.across(b) > 0
                && a.bottom() > b.y
            {
                return Err(format!(
                    "D7: {a:?} (row {ra}) is not above {b:?} (row {rb})"
                ));
            }
        }
    }
    Ok(())
}

/// D8: the mouse reaches every output from every other through edges it can cross.
pub fn connected(plan: &[Spot<u8>]) -> Result<(), String> {
    if plan.len() <= 1 {
        return Ok(());
    }
    let mut reached = vec![false; plan.len()];
    reached[0] = true;
    let mut stack = vec![0];
    while let Some(i) = stack.pop() {
        for j in 0..plan.len() {
            if !reached[j] && plan[i].crossing(&plan[j]).is_some() {
                reached[j] = true;
                stack.push(j);
            }
        }
    }
    let lost: Vec<u8> = plan
        .iter()
        .zip(&reached)
        .filter(|(_, r)| !**r)
        .map(|(s, _)| s.id)
        .collect();
    if lost.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "D8: {lost:?} cannot be reached from {}",
            plan[0].id
        ))
    }
}

/// D11: within a row, left to right as listed.
pub fn rows_in_their_order(plan: &[Spot<u8>], rows: &[Vec<u8>]) -> Result<(), String> {
    for row in rows {
        let xs: Vec<i32> = row
            .iter()
            .filter_map(|&id| spot(plan, id))
            .map(|s| s.x)
            .collect();
        if xs.windows(2).any(|w| w[0] >= w[1]) {
            return Err(format!("D11: row {row:?} is out of order: {xs:?}"));
        }
    }
    Ok(())
}

/// D12: planning the plan again (as the PC does from `SetLayout`) gives the same plan.
pub fn idempotent(
    desk: &Desk<u8>,
    plan: &[Spot<u8>],
    size: impl Fn(u8) -> Option<Size>,
) -> Result<(), String> {
    let again = desk::plan(desk, size, &desk::as_boxes(plan));
    let mut a = plan.to_vec();
    let mut b = again;
    a.sort_by_key(|s| s.id);
    b.sort_by_key(|s| s.id);
    if a == b {
        Ok(())
    } else {
        let moved: Vec<String> = a
            .iter()
            .zip(&b)
            .filter(|(x, y)| x != y)
            .map(|(x, y)| format!("{} {},{} -> {},{}", x.id, x.x, x.y, y.x, y.y))
            .collect();
        Err(format!("D12: planned again, moved {moved:?}"))
    }
}

/// R1: neighbours in a row share at least `least` of the shorter of their facing edges
/// as seen, the right one seen right of the left one.
pub fn rows_are_rows(view: &impl View<u8>, rows: &[Vec<u8>], least: f64) -> Result<(), String> {
    for row in rows {
        for w in row.windows(2) {
            match view.relate(w[0], w[1], Side::Right) {
                Some(r) if r.seam >= least - 1e-9 => {}
                Some(r) => {
                    return Err(format!(
                        "R1: {} and {} share {:.3} of the shorter edge in row {row:?}",
                        w[0], w[1], r.seam
                    ));
                }
                None => {
                    return Err(format!(
                        "R1: {} is listed left of {} but is not seen left of it",
                        w[0], w[1]
                    ));
                }
            }
        }
    }
    Ok(())
}

/// R2: every monitor in exactly one row.
pub fn rows_partition(ids: &[u8], rows: &[Vec<u8>]) -> Result<(), String> {
    let mut listed: Vec<u8> = rows.iter().flatten().copied().collect();
    listed.sort_unstable();
    let mut want = ids.to_vec();
    want.sort_unstable();
    want.dedup();
    if listed == want {
        Ok(())
    } else {
        Err(format!("R2: rows {rows:?} for {want:?}"))
    }
}

pub const ROW_KEEP: f64 = KEEP;

// ---------------------------------------------------------------- continuity

/// How the mouse crosses where monitors meet as seen.
#[derive(Debug, Clone, Default)]
pub struct Continuity {
    /// Pairs that meet as seen but do not share that edge on the desktop.
    pub lost: Vec<(u8, u8, Side)>,
    /// Points of seams that meet as seen where the mouse is stopped on the desktop.
    pub blocked: usize,
    pub samples: usize,
    /// The largest placement error at the middle of a seam, in pixels of the monitor
    /// entered: between neighbours of a row (side by side), and between rows.
    pub side_error: f64,
    pub stack_error: f64,
    /// The pair with the largest side error.
    pub side_worst: Option<(u8, u8, f64)>,
    /// The largest of these errors over what is lined up there (`desk::step` of the
    /// shorter edge along a seam in a row, `desk::noise` between rows, plus a pixel): at
    /// most 1 where the plan reproduces the seam as seen.
    pub side_ratio: f64,
    pub stack_ratio: f64,
    /// The largest jump anywhere along a seam where the mouse crosses: pixels of the
    /// monitor it enters, degrees as seen, metres on that monitor. Beyond the error at
    /// the middle, this is how differently the two monitors spread their pixels over
    /// the angle they share (the geometry, not the plan).
    pub max_px: f64,
    pub max_deg: f64,
    pub max_m: f64,
    /// The pair with the largest jump, and the jump in pixels.
    pub worst: Option<(u8, u8, Side, f64)>,
}

impl Continuity {
    pub fn add(&mut self, o: &Continuity) {
        self.lost.extend(o.lost.iter().copied());
        self.blocked += o.blocked;
        self.samples += o.samples;
        if o.side_error > self.side_error {
            self.side_error = o.side_error;
            self.side_worst = o.side_worst;
        }
        self.stack_error = self.stack_error.max(o.stack_error);
        self.side_ratio = self.side_ratio.max(o.side_ratio);
        self.stack_ratio = self.stack_ratio.max(o.stack_ratio);
        if o.max_px > self.max_px {
            self.max_px = o.max_px;
            self.worst = o.worst;
        }
        self.max_deg = self.max_deg.max(o.max_deg);
        self.max_m = self.max_m.max(o.max_m);
    }
}

/// For every pair that meets as seen (edges within `gap`, sharing `seam` of the shorter
/// edge, see `Room::touches`): whether the desktop has them meeting on the same side,
/// and where the mouse crosses, how far (in the monitor it enters) from the point seen
/// next to where it left: at the middle of the seam (the plan's placement error) and
/// anywhere along it.
pub fn continuity(scene: &Scene, plan: &[Spot<u8>], gap: f64, seam: f64) -> Continuity {
    let room = scene.room();
    let mut c = Continuity::default();
    for t in room.touches(gap, seam) {
        let (Some(a), Some(b)) = (spot(plan, t.a), spot(plan, t.b)) else {
            continue;
        };
        let meets = match t.side {
            Side::Right => b.x == a.right() && a.along(&b) > 0,
            Side::Left => b.right() == a.x && a.along(&b) > 0,
            Side::Above => b.bottom() == a.y && a.across(&b) > 0,
            Side::Below => b.y == a.bottom() && a.across(&b) > 0,
        };
        if !meets {
            c.lost.push((t.a, t.b, t.side));
            continue;
        }
        let b_side = t.side.opposite();
        let mpp = room.metres_per_pixel(t.b, b_side).unwrap_or(0.0);
        let points = room.seam(t.a, t.b, t.side, 9);
        for (j, (pa, pb)) in points.iter().copied().enumerate() {
            c.samples += 1;
            // Where the cursor goes on the desktop, in `b`'s pixels along its edge.
            let (entered, extent) = match t.side {
                Side::Left | Side::Right => (f64::from(a.y) + pa - f64::from(b.y), f64::from(b.h)),
                Side::Above | Side::Below => (f64::from(a.x) + pa - f64::from(b.x), f64::from(b.w)),
            };
            let px = (entered - pb).abs();
            if j == points.len() / 2 {
                match t.side {
                    Side::Left | Side::Right => {
                        if px > c.side_error {
                            c.side_error = px;
                            c.side_worst = Some((t.a, t.b, px));
                        }
                        let allowed = f64::from(desk::step(a.h.min(b.h)) + 1);
                        c.side_ratio = c.side_ratio.max(px / allowed);
                    }
                    Side::Above | Side::Below => {
                        c.stack_error = c.stack_error.max(px);
                        let allowed = f64::from(desk::noise(a.w.min(b.w)) + 1);
                        c.stack_ratio = c.stack_ratio.max(px / allowed);
                    }
                }
            }
            if entered < 0.0 || entered >= extent {
                c.blocked += 1;
                continue;
            }
            let deg = match (
                room.angle_along(t.b, b_side, entered),
                room.angle_along(t.b, b_side, pb),
            ) {
                (Some(x), Some(y)) => (x - y).abs().to_degrees(),
                _ => 0.0,
            };
            if px > c.max_px {
                c.max_px = px;
                c.worst = Some((t.a, t.b, t.side, px));
            }
            c.max_deg = c.max_deg.max(deg);
            c.max_m = c.max_m.max(px * mpp);
        }
    }
    c
}

// ---------------------------------------------------------------- shrinking

/// The smallest scene (fewest monitors) for which `fails` still holds: monitors are taken
/// out one at a time while it does.
pub fn shrink(scene: &Scene, fails: impl Fn(&Scene) -> bool) -> Scene {
    let mut best = scene.clone();
    'outer: loop {
        for i in 0..best.monitors.len() {
            if best.monitors.len() <= 1 {
                break 'outer;
            }
            let mut s = best.clone();
            let gone = s.monitors.remove(i).id;
            if s.main == Some(gone) {
                s.main = s.monitors.first().map(|m| m.id);
            }
            if fails(&s) {
                best = s;
                continue 'outer;
            }
        }
        break;
    }
    best
}

pub fn sizes_of(scene: &Scene) -> Vec<(u8, Size)> {
    scene
        .monitors
        .iter()
        .map(|m| {
            (
                m.id,
                Size {
                    w: m.px.w as i32,
                    h: m.px.h as i32,
                },
            )
        })
        .collect()
}

/// The plan as a short text.
pub fn describe_plan(plan: &[Spot<u8>]) -> String {
    let mut v = plan.to_vec();
    v.sort_by_key(|s| s.id);
    v.iter()
        .map(|s| format!("{}:{}x{}@{},{}", s.id, s.w, s.h, s.x, s.y))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A desk of random boxes (anything, nonsense too) for the plan alone: up to 10
/// monitors in up to 4 rows at random scales, maybe a laptop screen, other displays and
/// window panels.
pub fn random_desk(rng: &mut Rng) -> (Desk<u8>, Boxes<u8>, BTreeMap<u8, Size>) {
    let n = 1 + rng.below(10);
    let mut sizes = BTreeMap::new();
    let mut rows: Vec<Vec<u8>> = vec![Vec::new(); 1 + rng.below(n.min(4))];
    let mut boxes = Vec::new();
    let logical = |rng: &mut Rng, px: PixelSize| {
        let scale = rng.pick(&[1.0f64, 1.25, 1.5, 2.0]);
        Size {
            w: (f64::from(px.w) / scale).round() as i32,
            h: (f64::from(px.h) / scale).round() as i32,
        }
    };
    for id in 0..n as u8 {
        let px = random_px(rng);
        sizes.insert(id, logical(rng, px));
        let r = rng.below(rows.len());
        rows[r].push(id);
        if !rng.chance(15) {
            let far = rng.chance(10);
            let (sx, sy) = if far {
                (100_000.0, 100_000.0)
            } else {
                (8000.0, 3000.0)
            };
            boxes.push((
                id,
                desk::SeenBox {
                    x: f64::from(rng.float(-sx, sx)),
                    y: f64::from(rng.float(-sy, sy)),
                    w: f64::from(px.w),
                    h: f64::from(px.h),
                },
                sizes[&id],
            ));
        }
    }
    let mut desk = Desk {
        rows,
        main: rng.chance(85).then(|| rng.below(n) as u8),
        ..Desk::default()
    };
    if rng.chance(50) {
        let px = rng.pick(&[PixelSize::new(1920, 1080), PixelSize::new(2880, 1800)]);
        sizes.insert(MIRROR, logical(rng, px));
        desk.under_main = Some(MIRROR);
    }
    for id in 101..101 + rng.below(3) as u8 {
        let px = random_px(rng);
        sizes.insert(id, logical(rng, px));
        desk.bottom.push(id);
    }
    for id in 10..10 + rng.below(3) as u8 {
        sizes.insert(
            id,
            Size {
                w: 320 + rng.below(1200) as i32,
                h: 200 + rng.below(900) as i32,
            },
        );
        desk.column.push(id);
    }
    (desk, Boxes::new(boxes), sizes)
}

pub fn sizes_list(sizes: &BTreeMap<u8, Size>) -> Vec<(u8, Size)> {
    sizes.iter().map(|(k, v)| (*k, *v)).collect()
}

// ------------------------------------------------------------------ moving by hand

/// The owner's three of 2026-11-20: 2560x1440 (0), the 1440x2560 primary (1) and
/// 2560x1440 (2), side by side as the arc preset puts them.
pub fn portrait_between_two() -> Scene {
    let px = [
        (0u8, PixelSize::new(2560, 1440)),
        (1, PixelSize::new(1440, 2560)),
        (2, PixelSize::new(2560, 1440)),
    ];
    let placed = circle::arrange(Preset::Arc, &px, Some(1), None, None, &Limits::default());
    Scene {
        monitors: px
            .iter()
            .map(|&(id, px)| Monitor {
                id,
                px,
                placement: placed[&id],
            })
            .collect(),
        frame: Frame::upright(Vec3::ZERO, 0.0),
        main: Some(1),
    }
}

/// `scene` with monitor `id` turned `right` radians around the eye (to the right when
/// positive) and moved `up` metres.
pub fn moved(scene: &Scene, id: u8, right: f32, up: f32) -> Scene {
    let turn = Quat::from_rotation_y(-right);
    Scene {
        monitors: scene
            .monitors
            .iter()
            .map(|m| {
                let mut m = *m;
                if m.id == id {
                    m.placement.position = turn * m.placement.position + Vec3::Y * up;
                    m.placement.rotation = (turn * m.placement.rotation).normalize();
                }
                m
            })
            .collect(),
        frame: scene.frame,
        main: scene.main,
    }
}

/// A hand moving `scene`'s monitors a little, one at a time: each drag takes one monitor
/// in 30 frames from where it is to up to `up` metres up or down and up to `side`
/// radians sideways (into its neighbour or away) from where it started, with a tremor
/// of 2 mm. Every frame, as the eye sees it.
pub fn small_drags(scene: &Scene, rng: &mut Rng, drags: usize, side: f32, up: f32) -> Vec<Scene> {
    let ids: Vec<u8> = scene.monitors.iter().map(|m| m.id).collect();
    let mut at: BTreeMap<u8, (f32, f32)> = ids.iter().map(|&k| (k, (0.0, 0.0))).collect();
    let tremor = 0.002;
    let mut frames = Vec::new();
    for _ in 0..drags {
        let id = rng.pick(&ids);
        let from = at[&id];
        let to = (rng.float(-side, side), rng.float(-up, up));
        for f in 1..=30 {
            let t = f as f32 / 30.0;
            let now = (
                from.0 + (to.0 - from.0) * t + rng.float(-tremor, tremor) / 1.1,
                from.1 + (to.1 - from.1) * t + rng.float(-tremor, tremor),
            );
            at.insert(id, now);
            let mut s = moved(scene, id, 0.0, 0.0);
            for (&k, &(r, u)) in &at {
                s = moved(&s, k, r, u);
            }
            frames.push(s);
        }
    }
    frames
}

/// How many times the rows change from one frame to the next.
pub fn flips(rows: &[Vec<Vec<u8>>]) -> usize {
    rows.windows(2).filter(|w| w[0] != w[1]).count()
}

// ------------------------------------------------------------------ lining up

/// A "Line up": the placements it makes of a scene, `None` for one it does not handle.
pub type LineUp<'a> = &'a dyn Fn(&Scene) -> Option<BTreeMap<u8, Placement>>;

/// The scene with its monitors where `out` puts them (the others as they were).
pub fn lined(scene: &Scene, out: &BTreeMap<u8, Placement>) -> Scene {
    Scene {
        monitors: scene
            .monitors
            .iter()
            .map(|m| Monitor {
                placement: out.get(&m.id).copied().unwrap_or(m.placement),
                ..*m
            })
            .collect(),
        frame: scene.frame,
        main: scene.main,
    }
}

/// The scene's monitors turned about the eye by `turn`; the frame stays.
pub fn turned(scene: &Scene, turn: Quat) -> Scene {
    let eye = scene.frame.eye;
    Scene {
        monitors: scene
            .monitors
            .iter()
            .map(|m| Monitor {
                placement: turn_about(&m.placement, eye, turn),
                ..*m
            })
            .collect(),
        frame: scene.frame,
        main: scene.main,
    }
}

/// `p` turned about `eye` by `turn`.
pub fn turn_about(p: &Placement, eye: Vec3, turn: Quat) -> Placement {
    Placement {
        position: eye + turn * (p.position - eye),
        rotation: (turn * p.rotation).normalize(),
        width: p.width,
    }
}

/// The farthest any corner of any monitor of `scene` is between placements `a` and `b`
/// (each monitor at its own height), metres.
pub fn farthest(scene: &Scene, a: &BTreeMap<u8, Placement>, b: &BTreeMap<u8, Placement>) -> f32 {
    scene
        .monitors
        .iter()
        .filter_map(|m| {
            let (p, q) = (a.get(&m.id)?, b.get(&m.id)?);
            let h = m.height();
            Some(
                p.corners(h)
                    .iter()
                    .zip(q.corners(h))
                    .map(|(x, y)| x.distance(y))
                    .fold(0.0f32, f32::max),
            )
        })
        .fold(0.0f32, f32::max)
}

/// The placements of a scene.
pub fn placements(scene: &Scene) -> BTreeMap<u8, Placement> {
    scene.monitors.iter().map(|m| (m.id, m.placement)).collect()
}

/// The mirror image of `p` about the plane x = 0 (in the coordinates it is given in):
/// its left and right swap, so its right edge points the mirrored way back.
pub fn mirror(p: &Placement) -> Placement {
    let m = |v: Vec3| Vec3::new(-v.x, v.y, v.z);
    let right = -m(p.rotation * Vec3::X);
    let up = m(p.rotation * Vec3::Y);
    let face = m(p.rotation * Vec3::Z);
    Placement {
        position: m(p.position),
        rotation: Quat::from_mat3(&glam::Mat3::from_cols(right, up, face)).normalize(),
        width: p.width,
    }
}

/// A mirror-symmetric arrangement about a vertical plane through the eye, and its
/// mirror pairs (a monitor on the plane is its own): a row side by side on a circle
/// around the eye with sizes mirrored about the middle (a monitor on the plane or
/// none), maybe one more on the plane over it, the primary on the plane or none; each
/// monitor of the right half nudged, turned and tilted by hand and its partner the
/// mirror image; then the whole turned by any yaw about the eye. The plane is the
/// frame's x = 0 turned by the returned yaw.
pub fn symmetric_scene(rng: &mut Rng) -> (Scene, Vec<(u8, u8)>, f32) {
    let radius = rng.float(1.0, 1.6);
    let middle = rng.chance(60);
    let angle = |px: PixelSize| {
        2.0 * (px.w as f32 * METRES_PER_PIXEL / 2.0 / radius)
            .min(0.95)
            .asin()
    };
    let centre = random_px(rng);
    // The right half, out from the middle while it stays within 150°.
    let mut right_half: Vec<PixelSize> = Vec::new();
    let mut at = if middle { angle(centre) / 2.0 } else { 0.0 };
    for _ in 0..1 + rng.below(3) {
        let px = random_px(rng);
        if at + angle(px) > 150f32.to_radians() {
            break;
        }
        at += angle(px);
        right_half.push(px);
    }
    if right_half.is_empty() {
        right_half.push(PixelSize::new(1920, 1080));
    }
    let half = right_half.len();
    let mut px: Vec<PixelSize> = right_half.iter().rev().copied().collect();
    if middle {
        px.push(centre);
    }
    px.extend(right_half.iter().copied());
    let n = px.len();
    let mut monitors: Vec<(u8, PixelSize)> =
        px.iter().enumerate().map(|(i, &p)| (i as u8, p)).collect();
    let y = -0.08;
    let mut local: BTreeMap<u8, Placement> = BTreeMap::new();
    let mut yaw = if middle { angle(centre) / 2.0 } else { 0.0 };
    if middle {
        local.insert(
            half as u8,
            on_cylinder(0.0, y, centre.w as f32 * METRES_PER_PIXEL, radius),
        );
    }
    for (i, &p) in right_half.iter().enumerate() {
        let id = (n - half + i) as u8;
        let a = angle(p);
        local.insert(
            id,
            on_cylinder(yaw + a / 2.0, y, p.w as f32 * METRES_PER_PIXEL, radius),
        );
        yaw += a;
    }
    let mut pairs = Vec::new();
    let over_px = random_px(rng);
    if middle && rng.chance(40) && (over_px.w as f32 * METRES_PER_PIXEL) < radius {
        // One more over the middle one, on the plane.
        let px = over_px;
        let w = px.w as f32 * METRES_PER_PIXEL;
        let over = y
            + centre.h as f32 * METRES_PER_PIXEL / 2.0
            + w * px.aspect() / 2.0
            + rng.float(0.0, 0.1);
        let id = n as u8;
        monitors.push((id, px));
        local.insert(id, on_cylinder(0.0, over, w, radius));
        pairs.push((id, id));
    }
    let primary = middle.then_some(half as u8);
    for i in 0..n / 2 {
        let (left, right) = (i as u8, (n - 1 - i) as u8);
        pairs.push((left, right));
        let mut p = local[&right];
        if rng.chance(70) {
            p.position += Vec3::new(
                rng.float(-0.03, 0.03),
                rng.float(-0.03, 0.03),
                rng.float(-0.03, 0.03),
            );
        }
        if rng.chance(50) {
            let turn = Quat::from_euler(
                glam::EulerRot::YXZ,
                rng.float(-0.05, 0.05),
                rng.float(-0.05, 0.05),
                rng.float(-0.05, 0.05),
            );
            p.rotation = (p.rotation * turn).normalize();
        }
        local.insert(right, p);
        local.insert(left, mirror(&p));
    }
    if middle {
        pairs.push((half as u8, half as u8));
    }
    let yaw = rng.float(-3.0, 3.0);
    let frame = Frame::upright(
        Vec3::new(rng.float(-1.0, 1.0), 1.2, rng.float(-1.0, 1.0)),
        0.0,
    );
    let turn = Quat::from_rotation_y(yaw);
    let scene = Scene {
        monitors: monitors
            .iter()
            .map(|&(id, px)| Monitor {
                id,
                px,
                placement: frame.place(turn_about(&local[&id], Vec3::ZERO, turn)),
            })
            .collect(),
        frame,
        main: primary,
    };
    (scene, pairs, yaw)
}

/// How far a lined-up symmetric scene is from symmetric: the farthest corner of a
/// monitor from where its partner's mirror image puts it, metres.
pub fn asymmetry(
    scene: &Scene,
    out: &BTreeMap<u8, Placement>,
    pairs: &[(u8, u8)],
    yaw: f32,
) -> f32 {
    let unturn = Quat::from_rotation_y(-yaw);
    let local = |k: u8| turn_about(&scene.frame.unplace(out[&k]), Vec3::ZERO, unturn);
    pairs
        .iter()
        .map(|&(a, b)| {
            let h = scene.get(a).unwrap().height();
            let (pa, pb) = (mirror(&local(a)), local(b));
            pa.corners(h)
                .iter()
                .zip(pb.corners(h))
                .map(|(x, y)| x.distance(y))
                .fold(0.0f32, f32::max)
        })
        .fold(0.0f32, f32::max)
}

/// One invariant of "Line up" measured over many scenes: how often it broke, the worst
/// value seen, and the smallest scene (fewest monitors) that broke it.
#[derive(Debug, Default)]
pub struct Check {
    pub name: &'static str,
    pub broken: usize,
    pub total: usize,
    /// Scenes left out (the line-up does not handle them, or the rows differ).
    pub skipped: usize,
    pub worst: f32,
    pub example: Option<(usize, String)>,
}

impl Check {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            ..Self::default()
        }
    }

    /// One scene: `value` against `bound` (broken when over it).
    pub fn record(&mut self, value: f32, bound: f32, scene: &Scene, note: impl FnOnce() -> String) {
        self.total += 1;
        self.worst = self.worst.max(value);
        if value > bound {
            self.broken += 1;
            let n = scene.monitors.len();
            if self.example.as_ref().is_none_or(|e| n < e.0) {
                self.example = Some((n, format!("{}\n{}", note(), scene.describe())));
            }
        }
    }

    pub fn report(&self) -> String {
        let mut s = format!(
            "{}: broken in {} of {} (skipped {}), worst {:.4}",
            self.name, self.broken, self.total, self.skipped, self.worst
        );
        if let Some((_, e)) = &self.example {
            let _ = write!(s, "\n  smallest: {}", e.replace('\n', "\n  "));
        }
        s
    }
}

/// The rows of a scene as the eye sees it, afresh.
pub fn rows_now(scene: &Scene) -> Vec<Vec<u8>> {
    let ids: Vec<u8> = scene.monitors.iter().map(|m| m.id).collect();
    rows_of(&ids, &scene.room(), None)
}

/// The axis a lined-up row stands on (frame coordinates): square to its monitors'
/// right edges (the two pointing most apart), else its first monitor's up; pointing up.
pub fn row_axis(scene: &Scene, row: &[u8]) -> Vec3 {
    let rights: Vec<Vec3> = row
        .iter()
        .filter_map(|&k| {
            scene
                .get(k)
                .map(|m| scene.frame.unplace(m.placement).rotation * Vec3::X)
        })
        .collect();
    let mut best = Vec3::ZERO;
    for i in 0..rights.len() {
        for j in i + 1..rights.len() {
            let c = rights[i].cross(rights[j]);
            if c.length() > best.length() {
                best = c;
            }
        }
    }
    let axis = if best.length() >= 10f32.to_radians().sin() {
        best.normalize()
    } else {
        row.first()
            .and_then(|&k| scene.get(k))
            .map_or(Vec3::Y, |m| {
                scene.frame.unplace(m.placement).rotation * Vec3::Y
            })
    };
    if axis.y < 0.0 { -axis } else { axis }
}

/// The mean direction of `centres` (frame coordinates) around `axis`: their directions
/// cut behind them and averaged.
pub fn mean_direction(centres: &[Vec3], axis: Vec3) -> f64 {
    let x = axis.any_orthonormal_vector();
    let z = x.cross(axis);
    let az: Vec<f64> = centres
        .iter()
        .map(|c| f64::from(c.dot(x)).atan2(f64::from(-c.dot(z))))
        .collect();
    prostir_layout::space::contiguous(&az).iter().sum::<f64>() / centres.len().max(1) as f64
}

/// Tops and bottoms of a row's monitors along `axis` (frame coordinates).
fn spans_along(scene: &Scene, row: &[u8], axis: Vec3) -> Vec<(f32, f32)> {
    row.iter()
        .filter_map(|&k| {
            let m = scene.get(k)?;
            let c = scene
                .frame
                .unplace(m.placement)
                .corners(m.height())
                .map(|p| p.dot(axis));
            Some((
                c.iter().copied().fold(f32::MIN, f32::max),
                c.iter().copied().fold(f32::MAX, f32::min),
            ))
        })
        .collect()
}

/// The line a row's monitors stand on along `axis` after a line-up: the mean of their
/// middles, tops or bottoms, whichever all share (to a millimetre), and which one;
/// `None` when they share none.
pub fn row_line(scene: &Scene, row: &[u8], axis: Vec3) -> Option<(usize, f32)> {
    let spans = spans_along(scene, row, axis);
    let lines = [
        spans
            .iter()
            .map(|s| (s.0 + s.1) / 2.0)
            .collect::<Vec<f32>>(),
        spans.iter().map(|s| s.0).collect(),
        spans.iter().map(|s| s.1).collect(),
    ];
    let n = spans.len().max(1) as f32;
    lines.iter().enumerate().find_map(|(i, l)| {
        let spread =
            l.iter().copied().fold(f32::MIN, f32::max) - l.iter().copied().fold(f32::MAX, f32::min);
        (spread <= 0.001).then(|| (i, l.iter().sum::<f32>() / n))
    })
}

/// The same line of a row before a line-up (`after` it): the mean of the middles, tops
/// or bottoms (`which` as [`row_line`] gives it) along `axis`, each monitor's centre as
/// it stood with its extent as it stands after (a crooked monitor's corners reach
/// further than its straight self, which is what the line is made of).
pub fn row_line_before(scene: &Scene, after: &Scene, row: &[u8], which: usize, axis: Vec3) -> f32 {
    let values: Vec<f32> = row
        .iter()
        .zip(spans_along(after, row, axis))
        .map(|(&k, (top, bottom))| {
            let was = scene
                .frame
                .unplace(scene.get(k).unwrap().placement)
                .position
                .dot(axis);
            let now = after
                .frame
                .unplace(after.get(k).unwrap().placement)
                .position
                .dot(axis);
            (top - now + was, bottom - now + was)
        })
        .map(|(top, bottom)| match which {
            0 => (top + bottom) / 2.0,
            1 => top,
            _ => bottom,
        })
        .collect();
    values.iter().sum::<f32>() / values.len().max(1) as f32
}

/// Every invariant of "Line up" (`plans/layout-math.md`, L1 to L9) measured for `line`
/// over `cases` scenes of each kind, from `seed`.
pub fn measure_line_up(line: LineUp<'_>, cases: usize, seed: u64) -> Vec<Check> {
    let mut rng = Rng::new(seed);
    let mut checks: Vec<Check> = Vec::new();
    let degrees = |d: f32| d.to_radians();

    // L1: turned about the eye and lined up = lined up and turned.
    let mut c = Check::new(
        "L1 turning the whole about the eye commutes (yaw any, pitch and roll to 10°), m",
    );
    for _ in 0..cases {
        let scene = hand_scene(&mut rng);
        let turn = Quat::from_euler(
            glam::EulerRot::YXZ,
            rng.float(-3.1, 3.1),
            rng.float(-1.0, 1.0) * degrees(10.0),
            rng.float(-1.0, 1.0) * degrees(10.0),
        );
        let other = turned(&scene, turn);
        let (Some(a), Some(b)) = (line(&scene), line(&other)) else {
            c.skipped += 1;
            continue;
        };
        if rows_now(&scene) != rows_now(&other) {
            c.skipped += 1;
            continue;
        }
        let a: BTreeMap<u8, Placement> = a
            .iter()
            .map(|(k, p)| (*k, turn_about(p, scene.frame.eye, turn)))
            .collect();
        c.record(farthest(&scene, &a, &b), 0.001, &scene, || "turned".into());
    }
    checks.push(c);

    // L2: every row keeps its direction; the main row its height.
    let mut dir = Check::new("L2 a row keeps its mean direction, degrees");
    let mut height = Check::new("L2 the main row keeps its line up and down, m");
    for _ in 0..cases {
        let scene = hand_scene(&mut rng);
        // Two monitors, one leaning: no way to tell which way is up for the whole.
        let leaning = scene.monitors.iter().any(|m| {
            let r = scene.frame.unplace(m.placement).rotation * Vec3::X;
            r.y.abs() > 0.5f32.to_radians().sin()
        });
        if scene.monitors.len() < 3 && leaning {
            dir.skipped += 1;
            continue;
        }
        let Some(out) = line(&scene) else {
            dir.skipped += 1;
            continue;
        };
        let after = lined(&scene, &out);
        let rows = rows_now(&scene);
        let centres = |s: &Scene, row: &[u8]| -> Vec<Vec3> {
            row.iter()
                .filter_map(|&k| s.get(k).map(|m| s.frame.unplace(m.placement).position))
                .collect()
        };
        for row in &rows {
            if row.iter().all(|&k| k == MIRROR) {
                continue;
            }
            let row: Vec<u8> = row.iter().copied().filter(|&k| k != MIRROR).collect();
            // A flat row (a wall): the mean of its centres along the wall, as an angle
            // from the eye; any other: the mean of their directions.
            let faces: Vec<Vec3> = row
                .iter()
                .map(|&k| {
                    scene
                        .frame
                        .unplace(scene.get(k).unwrap().placement)
                        .normal()
                })
                .collect();
            let depth = |k: u8| {
                -scene
                    .frame
                    .unplace(scene.get(k).unwrap().placement)
                    .position
                    .dot(faces[0])
            };
            let flat = row.len() > 1
                && faces
                    .iter()
                    .all(|f| f.angle_between(faces[0]) < 3f32.to_radians())
                && row.iter().all(|&k| (depth(k) - depth(row[0])).abs() < 0.02);
            let axis = row_axis(&after, &row);
            let turn = if flat {
                let mean = |s: &Scene| {
                    let c = centres(s, &row);
                    c.iter().copied().sum::<Vec3>() / c.len() as f32
                };
                let (b, a) = (mean(&scene), mean(&after));
                let along = |v: Vec3| v - faces[0] * v.dot(faces[0]) - axis * v.dot(axis);
                along(a - b).length() / b.length().max(0.1)
            } else {
                let (b, a) = (
                    mean_direction(&centres(&scene, &row), axis),
                    mean_direction(&centres(&after, &row), axis),
                );
                prostir_layout::space::unwrap(a - b, 0.0).abs() as f32
            }
            .to_degrees();
            dir.record(turn, 0.06, &scene, || {
                format!("row {row:?} turned {turn:.3}° (flat {flat})")
            });
        }
        let main_row = scene
            .main
            .and_then(|m| rows.iter().find(|r| r.contains(&m)))
            .cloned()
            .unwrap_or_else(|| rows[0].clone());
        let main_row: Vec<u8> = main_row.into_iter().filter(|&k| k != MIRROR).collect();
        let axis = row_axis(&after, &main_row);
        match row_line(&after, &main_row, axis) {
            Some((which, now)) => {
                let was = row_line_before(&scene, &after, &main_row, which, axis);
                height.record((now - was).abs(), 0.002, &scene, || {
                    format!("main row {main_row:?}: line {which} from {was:.4} to {now:.4}")
                });
            }
            None => height.record(1.0, 0.002, &scene, || {
                let spans: Vec<String> = main_row
                    .iter()
                    .map(|&k| {
                        let m = after.get(k).unwrap();
                        let c = after
                            .frame
                            .unplace(m.placement)
                            .corners(m.height())
                            .map(|p| p.y);
                        format!("{k}: {c:.4?}")
                    })
                    .collect();
                format!("main row {main_row:?} not on one line: {spans:?}")
            }),
        }
    }
    checks.push(dir);
    checks.push(height);

    // L3: a mirror-symmetric arrangement stays symmetric.
    let mut c = Check::new("L3 symmetric stays symmetric, m");
    for _ in 0..cases {
        let (scene, pairs, yaw) = symmetric_scene(&mut rng);
        let Some(out) = line(&scene) else {
            c.skipped += 1;
            continue;
        };
        let value = asymmetry(&scene, &out, &pairs, yaw);
        c.record(value, 0.001, &scene, || {
            format!("pairs {pairs:?}, plane at {:.1}°", yaw.to_degrees())
        });
    }
    checks.push(c);

    // L4: lining up twice changes nothing the second time.
    let mut c = Check::new("L4 twice changes nothing, m");
    for _ in 0..cases {
        let scene = hand_scene(&mut rng);
        let Some(once) = line(&scene) else {
            c.skipped += 1;
            continue;
        };
        // Only where lining up kept the rows (L9 counts where it did not).
        if rows_now(&lined(&scene, &once)) != rows_now(&scene) {
            c.skipped += 1;
            continue;
        }
        let Some(twice) = line(&lined(&scene, &once)) else {
            c.skipped += 1;
            continue;
        };
        c.record(farthest(&scene, &once, &twice), 0.0001, &scene, || {
            "twice".into()
        });
    }
    checks.push(c);

    // L5: a 1 cm nudge before moves the result by no more than that.
    let mut c = Check::new("L5 a 1 cm nudge moves no centre of the result by more than 1.2 cm, m");
    for _ in 0..cases {
        let scene = hand_scene(&mut rng);
        let i = rng.below(scene.monitors.len());
        let d = loop {
            let v = Vec3::new(
                rng.float(-1.0, 1.0),
                rng.float(-1.0, 1.0),
                rng.float(-1.0, 1.0),
            );
            if (0.1..=1.0).contains(&v.length()) {
                break v.normalize() * 0.01;
            }
        };
        let mut nudged = lined(&scene, &placements(&scene));
        nudged.monitors[i].placement.position += d;
        if rows_now(&scene) != rows_now(&nudged) {
            c.skipped += 1;
            continue;
        }
        let (Some(a), Some(b)) = (line(&scene), line(&nudged)) else {
            c.skipped += 1;
            continue;
        };
        let id = scene.monitors[i].id;
        let moved = scene
            .monitors
            .iter()
            .map(|m| a[&m.id].position.distance(b[&m.id].position))
            .fold(0.0f32, f32::max);
        c.record(moved, 0.012, &scene, || {
            format!("monitor {id} nudged by {d:.3?}: a centre moved {moved:.4}")
        });
    }
    checks.push(c);

    // L6: a monitor hanging crooked straightens about its own centre.
    let mut c = Check::new("L6 a crooked monitor straightens in place, m");
    for _ in 0..cases {
        let scene = tidy_row(&mut rng);
        let Some(tidy) = line(&scene) else {
            c.skipped += 1;
            continue;
        };
        let start = lined(&scene, &tidy);
        let i = rng.below(start.monitors.len());
        let roll = rng.float(2.0, 9.0).to_radians() * if rng.chance(50) { 1.0 } else { -1.0 };
        let mut crooked = lined(&start, &placements(&start));
        let p = &mut crooked.monitors[i].placement;
        p.rotation = (Quat::from_axis_angle(p.normal(), roll) * p.rotation).normalize();
        if rows_now(&crooked) != rows_now(&start) {
            c.skipped += 1;
            continue;
        }
        let Some(out) = line(&crooked) else {
            c.skipped += 1;
            continue;
        };
        let id = start.monitors[i].id;
        c.record(farthest(&start, &tidy, &out), 0.001, &crooked, || {
            format!("monitor {id} rolled {:.1}°", roll.to_degrees())
        });
    }
    checks.push(c);

    // L7: a row tilted back as a whole (10°, 20°, 35°, a little roll) keeps its tilt.
    let mut c = Check::new("L7 a row tilted back as a whole keeps its tilt (10°, 20°, 35°), m");
    for k in 0..cases {
        let scene = tidy_row(&mut rng);
        let Some(tidy) = line(&scene) else {
            c.skipped += 1;
            continue;
        };
        let start = lined(&scene, &tidy);
        let back = [10.0f32, 20.0, 35.0][k % 3].to_radians();
        let roll = rng.float(-5.0, 5.0).to_radians();
        let turn = scene.frame.turn
            * Quat::from_rotation_z(roll)
            * Quat::from_rotation_x(-back)
            * scene.frame.turn.inverse();
        let tilted = turned(&start, turn);
        let Some(out) = line(&tilted) else {
            c.skipped += 1;
            continue;
        };
        c.record(
            farthest(&tilted, &placements(&tilted), &out),
            0.001,
            &tilted,
            || {
                format!(
                    "tilted back {:.0}°, rolled {:.1}°",
                    back.to_degrees(),
                    roll.to_degrees()
                )
            },
        );
    }
    checks.push(c);

    // L7: monitors each tilted back in place (turned to face a lower head) keep it.
    let mut c =
        Check::new("L7 monitors tilted back in place keep their tilt (10°, 20°, 35°), degrees");
    for k in 0..cases {
        let scene = tidy_row(&mut rng);
        let Some(tidy) = line(&scene) else {
            c.skipped += 1;
            continue;
        };
        let back = [10.0f32, 20.0, 35.0][k % 3].to_radians();
        let mut tilted = lined(&scene, &tidy);
        for m in &mut tilted.monitors {
            m.placement.rotation =
                (m.placement.rotation * Quat::from_rotation_x(-back)).normalize();
        }
        let Some(out) = line(&tilted) else {
            c.skipped += 1;
            continue;
        };
        // Measured against the frame's up, the axis of the tidy row: each keeps its tilt
        // and does not lean (closing a gap turns a monitor about the axis, which is fine).
        let up = tilted.frame.turn * Vec3::Y;
        let tilt = |p: &Placement| p.normal().dot(up).clamp(-1.0, 1.0).asin();
        let lean = |p: &Placement| (p.rotation * Vec3::X).dot(up).clamp(-1.0, 1.0).asin();
        let off = tilted
            .monitors
            .iter()
            .map(|m| {
                let after = &out[&m.id];
                ((tilt(after) - tilt(&m.placement)).abs())
                    .max(lean(after).abs())
                    .to_degrees()
            })
            .fold(0.0f32, f32::max);
        c.record(off, 0.1, &tilted, || {
            format!(
                "each tilted back {:.0}°: one off by {off:.2}°",
                back.to_degrees()
            )
        });
    }
    checks.push(c);

    // L9: lining up keeps the rows as the eye sees them.
    let mut c = Check::new("L9 the rows stay the rows (1 when they change)");
    for _ in 0..cases {
        let scene = hand_scene(&mut rng);
        let Some(out) = line(&scene) else {
            c.skipped += 1;
            continue;
        };
        let (before, after) = (rows_now(&scene), rows_now(&lined(&scene, &out)));
        let changed = if before == after { 0.0 } else { 1.0 };
        c.record(changed, 0.5, &scene, || {
            format!("rows {before:?} became {after:?}")
        });
    }
    checks.push(c);

    // L8: what a preset made is lined up already.
    let mut c = Check::new("L8 a preset stays as it is, m");
    for _ in 0..cases {
        let (scene, preset) = preset_scene(&mut rng);
        let Some(out) = line(&scene) else {
            c.skipped += 1;
            continue;
        };
        c.record(
            farthest(&scene, &placements(&scene), &out),
            0.001,
            &scene,
            || format!("{preset:?}"),
        );
    }
    checks.push(c);
    checks
}

/// One row of the arc preset (three to six monitors of any sizes, a random primary; two
/// rows when they do not fit one), in an upright frame anywhere in the room: tidy as it
/// stands.
pub fn tidy_row(rng: &mut Rng) -> Scene {
    let n = 3 + rng.below(4);
    let px: Vec<(u8, PixelSize)> = (0..n).map(|i| (i as u8, random_px(rng))).collect();
    let primary = Some(rng.below(n) as u8);
    let placed = circle::arrange(Preset::Arc, &px, primary, None, None, &Limits::default());
    let frame = Frame::upright(
        Vec3::new(rng.float(-1.0, 1.0), 1.2, rng.float(-1.0, 1.0)),
        rng.float(-3.0, 3.0),
    );
    Scene {
        monitors: px
            .iter()
            .map(|&(id, px)| Monitor {
                id,
                px,
                placement: frame.place(placed[&id]),
            })
            .collect(),
        frame,
        main: primary,
    }
}
