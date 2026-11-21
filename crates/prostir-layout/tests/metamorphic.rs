//! Metamorphic tests: change the arrangement in a way whose effect is known, and check
//! the plan changes exactly so (or not at all).

mod common;

use std::collections::BTreeMap;

use common::*;
use glam::{Quat, Vec3};
use prostir_layout::circle::Limits;
use prostir_layout::desk::{self, Desk, Spot, View};
use prostir_layout::place::place_new;
use prostir_layout::rows::rows_of;
use prostir_layout::space::Side;
use prostir_layout::space::{Monitor, Placement, covers};
use prostir_layout::tidy::{OVERLAP_MAX, glued_pairs, line_up};
use prostir_layout::view::reach_pair;

/// The headset's pipeline with history: rows kept as they were within the hysteresis,
/// the plan kept while it is the same in all but a hand's tremor.
fn with_history(
    scene: &Scene,
    rows: Option<&[Vec<u8>]>,
    plan: Option<&[Spot<u8>]>,
) -> (Vec<Vec<u8>>, Vec<Spot<u8>>) {
    let room = scene.room();
    let ids: Vec<u8> = scene.monitors.iter().map(|m| m.id).collect();
    let rows = rows_of(&ids, &room, rows);
    let desk = Desk {
        rows: rows.clone(),
        main: scene.main,
        ..Desk::default()
    };
    let fresh = desk::plan(&desk, |k| scene.size(k), &room);
    let plan = match plan {
        Some(p) => desk::settled(p, &fresh, &room),
        None => fresh,
    };
    (rows, plan)
}

fn regular(rng: &mut Rng) -> Scene {
    if rng.chance(50) {
        preset_scene(rng).0
    } else {
        let d = rng.chance(50);
        rows_scene(rng, d, 0.0, false)
    }
}

/// M1: a monitor placed next to the arrangement and taken away again leaves the rows and
/// the desktop exactly as they were (with the headset's history all along).
#[test]
fn adding_then_removing_a_monitor_changes_nothing() {
    let mut rng = Rng::new(0x3e7a_0001);
    for i in 0..cases(1000) {
        let scene = regular(&mut rng);
        if scene.monitors.len() >= 14 {
            continue;
        }
        let (rows0, plan0) = with_history(&scene, None, None);
        let px = random_px(&mut rng);
        let gaze = scene.frame.turn * Vec3::new(rng.float(-0.5, 0.5), rng.float(-0.2, 0.3), -1.0);
        let placement = place_new(
            &scene.monitors,
            &scene.frame,
            gaze,
            px,
            Some(&rows0),
            &Limits::default(),
        );
        let mut more = scene.clone();
        more.monitors.push(Monitor {
            id: 50,
            px,
            placement,
        });
        let (rows1, plan1) = with_history(&more, Some(&rows0), Some(&plan0));
        let (rows2, plan2) = with_history(&scene, Some(&rows1), Some(&plan1));
        assert_eq!(rows2, rows0, "case {i}: rows\n{}", scene.describe());
        let (a, b) = (relative(&plan2, plan0[0].id), relative(&plan0, plan0[0].id));
        assert_eq!(a, b, "case {i}: desktop\n{}", scene.describe());
    }
}

/// M2: nudging any monitor by up to a centimetre changes nothing on the desktop, or one
/// monitor (where the nudge crosses a threshold: a contact the mouse needs appears or
/// goes), with the headset's history. How often nothing changes is reported.
#[test]
fn a_nudge_changes_nothing_or_one_monitor() {
    let mut rng = Rng::new(0x3e7a_0002);
    let (mut checked, mut nothing, mut more) = (0, 0, 0);
    let mut first: Option<String> = None;
    for i in 0..cases(2000) {
        let scene = regular(&mut rng);
        let (rows0, plan0) = with_history(&scene, None, None);
        let mut nudged = scene.clone();
        let k = rng.below(nudged.monitors.len());
        let m = &mut nudged.monitors[k];
        let dir = Vec3::new(
            rng.float(-1.0, 1.0),
            rng.float(-1.0, 1.0),
            rng.float(-1.0, 1.0),
        )
        .normalize_or(Vec3::X);
        m.placement.position += dir * rng.float(0.0, 0.01);
        let (rows1, plan1) = with_history(&nudged, Some(&rows0), Some(&plan0));
        if rows1 != rows0 {
            more += 1;
            first.get_or_insert_with(|| {
                format!(
                    "case {i}: rows {rows0:?} -> {rows1:?}\n{}",
                    scene.describe()
                )
            });
            checked += 1;
            continue;
        }
        let reference = plan0[0].id;
        let (a, b) = (relative(&plan0, reference), relative(&plan1, reference));
        let moved = a.iter().zip(&b).filter(|(x, y)| x != y).count();
        if moved == 0 {
            nothing += 1;
        } else if moved > 1 {
            more += 1;
            first.get_or_insert_with(|| {
                format!(
                    "case {i}: {moved} moved\n{}before {}\nafter  {}",
                    scene.describe(),
                    describe_plan(&plan0),
                    describe_plan(&plan1)
                )
            });
        }
        checked += 1;
    }
    eprintln!(
        "a nudge of up to 1 cm with history: nothing moves in {nothing} of {checked}, more than one in {more}"
    );
    if let Some(f) = &first {
        eprintln!("first with more than one: {f}");
    }
    assert!(checked > cases(2000) * 3 / 4, "{checked}");
    assert!(more * 100 <= checked, "{more} of {checked}");
}

/// M2 without history (the PC alone, or a first plan): how often a 1 cm nudge moves more
/// than the nudged monitor (reported).
#[test]
fn a_nudge_without_history_is_reported() {
    let mut rng = Rng::new(0x3e7a_0003);
    let (mut nothing, mut only_it, mut more) = (0, 0, 0);
    for _ in 0..cases(2000) {
        let scene = regular(&mut rng);
        let (_, plan0) = pipeline(&scene);
        let mut nudged = scene;
        let k = rng.below(nudged.monitors.len());
        let id = nudged.monitors[k].id;
        let m = &mut nudged.monitors[k];
        let dir = Vec3::new(
            rng.float(-1.0, 1.0),
            rng.float(-1.0, 1.0),
            rng.float(-1.0, 1.0),
        )
        .normalize_or(Vec3::X);
        m.placement.position += dir * 0.01;
        let (_, plan1) = pipeline(&nudged);
        let reference = plan0.iter().map(|s| s.id).find(|&r| r != id).unwrap_or(id);
        let (a, b) = (relative(&plan0, reference), relative(&plan1, reference));
        let changed: Vec<u8> = a
            .iter()
            .zip(&b)
            .filter(|(x, y)| x != y)
            .map(|(x, _)| x.id)
            .collect();
        match changed.as_slice() {
            [] => nothing += 1,
            [c] if *c == id => only_it += 1,
            _ => more += 1,
        }
    }
    eprintln!(
        "a 1 cm nudge, no history: nothing moves {nothing}, only it {only_it}, more {more} (of {})",
        cases(2000)
    );
}

/// M3: turning and moving the whole scene (the frame with it) changes nothing; turning
/// the monitors around the user without the frame changes nothing either (rows are cut
/// at the widest empty stretch, not at the frame's back).
#[test]
fn turning_everything_changes_nothing() {
    let mut rng = Rng::new(0x3e7a_0004);
    let (mut checked, mut differ) = (0, 0);
    let mut first: Option<String> = None;
    for i in 0..cases(1000) {
        let scene = regular(&mut rng);
        // Where the widest empty stretch around the user is clear (at least 60°, and 10°
        // wider than the next): otherwise where a row is cut may change with the turn.
        if !clear_gap(&scene.seen(), 60f64.to_radians(), 10f64.to_radians()) {
            continue;
        }
        checked += 1;
        let (rows0, plan0) = pipeline(&scene);
        let turn = Quat::from_rotation_y(rng.float(-3.1, 3.1));
        let shift = Vec3::new(
            rng.float(-3.0, 3.0),
            rng.float(-1.0, 1.0),
            rng.float(-3.0, 3.0),
        );
        let (rows1, plan1) = pipeline(&scene.moved(turn, shift));
        // Only the monitors, about the frame's vertical through the eye.
        let about = scene.frame.turn * turn * scene.frame.turn.inverse();
        let mut only = scene.moved(about, Vec3::ZERO);
        only.frame = scene.frame;
        let (rows2, plan2) = pipeline(&only);
        let same = rows1 == rows0 && plan1 == plan0 && rows2 == rows0 && plan2 == plan0;
        if !same {
            differ += 1;
            first.get_or_insert_with(|| {
                format!(
                    "case {i}: rows {rows0:?} / {rows1:?} / {rows2:?}\n{}",
                    scene.describe()
                )
            });
        }
    }
    eprintln!("turning everything: {differ} of {checked} differ (float rounding at a near tie)");
    if let Some(f) = &first {
        eprintln!("first: {f}");
    }
    assert!(checked > cases(1000) * 4 / 5, "{checked}");
    assert!(differ * 100 <= checked, "{differ} of {checked}");
}

/// Whether the widest stretch no monitor covers is at least `least` wide and `margin`
/// wider than the next.
fn clear_gap(seen: &[prostir_layout::space::Seen<u8>], least: f64, margin: f64) -> bool {
    use std::f64::consts::TAU;
    let mut spans: Vec<(f64, f64)> = seen.iter().map(|s| (s.left, s.right)).collect();
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut merged: Vec<(f64, f64)> = Vec::new();
    for (a, b) in spans {
        match merged.last_mut() {
            Some(l) if a <= l.1 => l.1 = l.1.max(b),
            _ => merged.push((a, b)),
        }
    }
    let mut gaps: Vec<f64> = (0..merged.len())
        .map(|i| {
            let next = if i + 1 < merged.len() {
                merged[i + 1].0
            } else {
                merged[0].0 + TAU
            };
            next - merged[i].1
        })
        .collect();
    gaps.sort_by(|a, b| b.total_cmp(a));
    match gaps.as_slice() {
        [] => false,
        [w] => *w >= least,
        [w, next, ..] => *w >= least && w - next >= margin,
    }
}

/// M4 (the owner's): moving one monitor up by a quarter of its height moves only its
/// rect, by a quarter of its height in pixels. The monitor has room over it (nothing seen
/// over it), and it does not alone hold its row on the row under it. Always the full
/// count: the share of exceptions over the first fifth of the cases is not the share
/// over all of them.
#[test]
fn a_quarter_up_moves_only_that_monitor() {
    let mut rng = Rng::new(0x3e7a_0005);
    let (mut checked, mut wrong) = (0, 0);
    let mut first: Option<String> = None;
    for i in 0..3000 {
        let scene = rows_scene(&mut rng, true, 0.0, false);
        let (rows, plan0) = pipeline(&scene);
        let room = scene.room();
        let k = rng.below(scene.monitors.len());
        let id = scene.monitors[k].id;
        // Nothing seen over it (nor within its height above it).
        let frame = scene.frame;
        let mut moved = scene.clone();
        let h = moved.monitors[k].height();
        let mut local = frame.unplace(moved.monitors[k].placement);
        // Room over it: a quarter of its height and the touching distance more.
        let mut higher = local;
        higher.position.y += h / 4.0 + h * desk::TOUCH_GAP as f32;
        local.position.y += h / 4.0;
        moved.monitors[k].placement = frame.place(local);
        let mut probe = scene.clone();
        probe.monitors[k].placement = frame.place(higher);
        let corners = |s: &Scene, k: usize| {
            s.frame
                .unplace(s.monitors[k].placement)
                .corners(s.monitors[k].height())
        };
        let free = (0..moved.monitors.len())
            .filter(|&j| j != k)
            .all(|j| !covers(&corners(&probe, k), &corners(&probe, j), 0.0));
        let row = rows.iter().find(|r| r.contains(&id)).unwrap();
        let row_index = rows.iter().position(|r| r.contains(&id)).unwrap();
        let main_row = rows
            .iter()
            .position(|r| scene.main.is_some_and(|m| r.contains(&m)))
            .unwrap();
        // Holding its row: in a row over (under) the main one, the only one of it seen
        // touching the row next to it.
        let touches_next = |m: u8| {
            room.touches(desk::TOUCH_GAP, 0.25).iter().any(|t| {
                (t.a == m || t.b == m) && {
                    let other = if t.a == m { t.b } else { t.a };
                    !row.contains(&other)
                }
            })
        };
        let holders = row.iter().filter(|&&m| touches_next(m)).count();
        let holds_alone = row_index != main_row && holders <= 1 && touches_next(id);
        // A monitor over the moved one (rows above rest on it) moves with it: skip.
        let carries = rows[..row_index].iter().flatten().any(|&u| {
            room.relate(id, u, Side::Above)
                .is_some_and(|r| r.shared > 0.0)
        });
        if !free || holds_alone || carries || row.len() < 2 {
            continue;
        }
        let (rows1, plan1) = pipeline(&moved);
        if rows1 != rows {
            // Lifted out of its row (shares under half the edge now): another case.
            continue;
        }
        // Lifted off the row under it, or onto the row over it: what touches across
        // the rows is not what it was, and the rows are laid against each other anew
        // (see the plan): another case.
        let across = |s: &Scene| {
            let mut t: Vec<(u8, u8)> = s
                .room()
                .touches(desk::TOUCH_GAP, 0.25)
                .iter()
                .filter(|t| !rows.iter().any(|r| r.contains(&t.a) && r.contains(&t.b)))
                .map(|t| (t.a.min(t.b), t.a.max(t.b)))
                .collect();
            t.sort_unstable();
            t
        };
        if across(&moved) != across(&scene) {
            continue;
        }
        let reference = row.iter().copied().find(|&m| m != id).unwrap();
        let (a, b) = (relative(&plan0, reference), relative(&plan1, reference));
        let px_h = scene.monitors[k].px.h as i32;
        let mut ok = true;
        for (x, y) in a.iter().zip(&b) {
            if x.id == id {
                // A quarter of its height in pixels, give or take a step lined up
                // where it stood before or stands now.
                let dy = x.y - y.y;
                ok &= x.x == y.x && (dy - px_h / 4).abs() <= desk::step(px_h) + 1;
            } else {
                ok &= x == y;
            }
        }
        if !ok {
            wrong += 1;
            first.get_or_insert_with(|| {
                format!(
                    "case {i}: {id}\n{}before {}\nafter  {}",
                    scene.describe(),
                    describe_plan(&plan0),
                    describe_plan(&plan1)
                )
            });
        }
        checked += 1;
    }
    eprintln!("a quarter up: {checked} cases checked, {wrong} moved more than that monitor");
    if let Some(f) = &first {
        eprintln!("first: {f}");
    }
    assert!(checked > 300, "{checked}");
    // The rest move by little more than a lined-up step: a lift can take a seam out of
    // a step the hand left (lined up before, kept after) or into one, and the two seams
    // of the lifted monitor see the same lift in different pixels (they are at
    // different distances from the eye), so the monitors after it in the row may follow
    // by that much.
    assert!(wrong * 100 <= checked * 15, "{wrong} of {checked}");
}

/// P1 to P4: a new monitor goes next to the arrangement, moves nothing, covers nothing,
/// touches something, and a row it extends stays within the widest arc.
#[test]
fn a_new_monitor_goes_next_to_the_others() {
    let mut rng = Rng::new(0x3e7a_0006);
    let limits = Limits::default();
    let (mut touching, mut total) = (0, 0);
    for i in 0..cases(1500) {
        let scene = regular(&mut rng);
        let px = random_px(&mut rng);
        let gaze = scene.frame.turn * Vec3::new(rng.float(-0.8, 0.8), rng.float(-0.3, 0.5), -1.0);
        let before = scene.monitors.clone();
        let p = place_new(&scene.monitors, &scene.frame, gaze, px, None, &limits);
        assert_eq!(scene.monitors, before, "P1");
        let mut more = scene.clone();
        more.monitors.push(Monitor {
            id: 50,
            px,
            placement: p,
        });
        let local = |m: &Monitor<u8>| more.frame.unplace(m.placement).corners(m.height());
        let new = local(more.monitors.last().unwrap());
        for m in &scene.monitors {
            assert!(
                !covers(&local(m), &new, 0.02),
                "case {i}: P2, covers {}\n{}",
                m.id,
                scene.describe()
            );
        }
        total += 1;
        let room = more.room();
        if room
            .touches(desk::TOUCH_GAP, 0.1)
            .iter()
            .any(|t| t.a == 50 || t.b == 50)
        {
            touching += 1;
        }
    }
    eprintln!("a new monitor: touches the arrangement in {touching} of {total}");
    assert!(touching * 100 >= total * 80, "P3: {touching} of {total}");
}

/// How far the monitors of `upper` (where `at` puts them) go down the frame of `scene`
/// before one covers one of `lower` as seen from its eye, to a tenth of a millimetre
/// (metres; 1 when not within 1 m).
fn drop_to_meet(scene: &Scene, at: &BTreeMap<u8, Placement>, upper: &[u8], lower: &[u8]) -> f32 {
    let down = scene.frame.turn * Vec3::NEG_Y;
    let corners = |k: u8, by: f32| {
        let m = scene.get(k).unwrap();
        let mut p = at[&k];
        p.position += down * by - scene.frame.eye;
        p.corners(m.height())
    };
    let meets = |by: f32| {
        upper.iter().any(|&u| {
            lower
                .iter()
                .any(|&l| covers(&corners(l, 0.0), &corners(u, by), 1e-7))
        })
    };
    if meets(0.0) {
        return 0.0;
    }
    // A centimetre at a time, then halving between the last free and the first met.
    let mut free = 0.0f32;
    while !meets(free + 0.01) {
        free += 0.01;
        if free >= 1.0 {
            return 1.0;
        }
    }
    let mut met = free + 0.01;
    while met - free > 1e-4 {
        let mid = (free + met) / 2.0;
        if meets(mid) {
            met = mid;
        } else {
            free = mid;
        }
    }
    free
}

/// Rows over rows for M8: lined up, the rows they are seen in, and a turn and a shift
/// to carry them away by.
fn carried_rows(rng: &mut Rng) -> Option<(Scene, Vec<Vec<u8>>, Quat, Vec3)> {
    let k = 2 + rng.below(2);
    let scene = rows_scene_of(rng, k, None, false, 0.0, false, false);
    let rows = rows_now(&scene);
    let turn = Quat::from_rotation_y(rng.float(-3.1, 3.1));
    let shift = Vec3::new(
        rng.float(-0.4, 0.4),
        rng.float(-0.2, 0.2),
        rng.float(-0.4, 0.4),
    );
    if rows.len() < 2 {
        return None;
    }
    let kept = lined(
        &scene,
        &line_up(&scene.monitors, &scene.frame, scene.main, Some(&rows), None),
    );
    let rows = rows_now(&kept);
    Some((kept, rows, turn, shift))
}

/// M8 (the owner's, 2026-11-21: monitors glued one over another flew off after a
/// profile and "Line up"): rows one over another, lined up, carried away together with
/// the eye they were laid out around (moved and turned, as a profile now comes back
/// around the user wherever they sit) line up exactly as lining up and carrying them
/// does, and the desktop is the same.
#[test]
fn rows_over_rows_line_up_the_same_carried_with_their_eye() {
    let mut rng = Rng::new(0x3e7a_0009);
    let (mut checked, mut differ, mut moved) = (0, 0, 0);
    let mut first: Option<String> = None;
    for i in 0..cases(500) {
        let Some((kept, rows, turn, shift)) = carried_rows(&mut rng) else {
            continue;
        };
        let carried = kept.moved(turn, shift);
        checked += 1;
        // Lined up there = lined up here and carried.
        let here = line_up(&kept.monitors, &kept.frame, kept.main, Some(&rows), None);
        let there = line_up(
            &carried.monitors,
            &carried.frame,
            carried.main,
            Some(&rows),
            None,
        );
        let here = placements(&lined(&kept, &here).moved(turn, shift));
        let off = farthest(&carried, &here, &there);
        // At a near tie (lining up decides, and the last bits of a float decide it): in
        // one of the 500 full scenes, the arrangement wrapping round the user, before the
        // glue too (79a6a0b).
        if off >= 1e-3 {
            moved += 1;
            first.get_or_insert_with(|| format!("case {i}: {off} m\n{}", kept.describe()));
        }
        let ((rows0, plan0), (rows1, plan1)) = (pipeline(&kept), pipeline(&carried));
        let reference = plan0[0].id;
        if rows0 != rows1 || relative(&plan0, reference) != relative(&plan1, reference) {
            differ += 1;
            first.get_or_insert_with(|| format!("case {i}: desktop\n{}", kept.describe()));
        }
    }
    eprintln!(
        "carried with the eye: {moved} of {checked} lined up otherwise, {differ} desktops differ (float rounding at a near tie)"
    );
    if let Some(f) = &first {
        eprintln!("first: {f}");
    }
    assert!(checked * 2 >= cases(500), "{checked}");
    assert!(differ * 100 <= checked, "{differ} of {checked}");
    assert!(moved * 100 <= checked, "{moved} of {checked}");
}

/// Reported (`--ignored`): M8's rows over rows carried away without their eye (a
/// profile that kept its monitors in the room but not the eye they were laid out
/// around), lined up from the eye left behind. Lining up fits every row anew around
/// that eye, each on its own: rows side by side close up, rows one over another keep
/// their heights. Of the rows resting on the row under them as that eye sees them
/// (within 5 mm of covering it), how many lining up leaves 2 cm or more apart: before
/// and after measured alike, from that eye, with the same rows.
#[test]
#[ignore = "a report, not a check"]
fn report_rows_over_rows_lined_up_from_an_eye_left_behind() {
    let mut rng = Rng::new(0x3e7a_0009);
    let (mut resting, mut apart, mut worst) = (0, 0, 0.0f32);
    for _ in 0..cases(500) {
        let Some((kept, rows, turn, shift)) = carried_rows(&mut rng) else {
            continue;
        };
        let mut stayed = kept.moved(turn, shift);
        stayed.frame = kept.frame;
        let out = line_up(
            &stayed.monitors,
            &stayed.frame,
            stayed.main,
            Some(&rows),
            None,
        );
        let gaps = |at: &BTreeMap<u8, Placement>| -> Vec<f32> {
            rows.windows(2)
                .map(|w| drop_to_meet(&stayed, at, &w[0], &w[1]))
                .collect()
        };
        for (b, a) in gaps(&placements(&stayed)).iter().zip(gaps(&out)) {
            if *b <= 0.005 {
                resting += 1;
                worst = worst.max(a);
                if a >= 0.02 {
                    apart += 1;
                }
            }
        }
    }
    eprintln!(
        "from the eye left behind: {apart} of {resting} resting rows come 2 cm or more apart, the farthest {worst:.3} m"
    );
}

/// T1 to T5: "Line up" keeps the rows, covers nothing, puts neighbours side by side,
/// keeps sizes, and lining up again changes nothing (the primary is not an anchor: each
/// row keeps its own direction, see `line_up.rs`). The same with every monitor moved and
/// turned by up to a micrometre (and a microradian): what another machine's last bits of
/// a sine do to the scenes and to lining them up must not change the counts.
#[test]
fn lining_up_keeps_rows_and_settles() {
    let plain = lining_up_counts(None);
    for seed in [0x51, 0x52] {
        let shaken = lining_up_counts(Some((1e-6, seed)));
        assert!(
            shaken.abs_diff(plain) <= 1 + plain / 10,
            "{plain} broken as they are, {shaken} moved by a micrometre"
        );
    }
}

/// [`lining_up_keeps_rows_and_settles`] over the hand scenes, every monitor moved and
/// turned by up to `jitter.0` (from the seed `jitter.1`): how many broke T1, T2 or T5.
fn lining_up_counts(jitter: Option<(f32, u64)>) -> usize {
    let mut rng = Rng::new(0x3e7a_0007);
    let mut shake = jitter.map(|(_, seed)| Rng::new(seed));
    let (mut t1, mut t2, mut t5, mut total) = (0, 0, 0, 0);
    let mut first: Option<String> = None;
    let mut glued_overlaps: Vec<f64> = Vec::new();
    for i in 0..cases(800) {
        let mut scene = hand_scene(&mut rng);
        if let (Some((by, _)), Some(r)) = (jitter, shake.as_mut()) {
            for m in &mut scene.monitors {
                let mut v = || r.float(-by, by);
                m.placement.position += Vec3::new(v(), v(), v());
                let turn = Quat::from_euler(glam::EulerRot::YXZ, v(), v(), v());
                m.placement.rotation = (turn * m.placement.rotation).normalize();
            }
        }
        let room = scene.room();
        let ids: Vec<u8> = scene.monitors.iter().map(|m| m.id).collect();
        let rows = rows_of(&ids, &room, None);
        let lid = scene.get(MIRROR).map(|m| m.id);
        let done = line_up(&scene.monitors, &scene.frame, scene.main, Some(&rows), lid);
        let mut lined = scene.clone();
        for m in &mut lined.monitors {
            let p = done[&m.id];
            assert!((p.width - m.placement.width).abs() < 1e-6, "T4: size");
            m.placement = p;
        }
        // T1: the same rows (with history).
        let rows1 = rows_of(&ids, &lined.room(), Some(&rows));
        if rows1 != rows {
            t1 += 1;
            first.get_or_insert_with(|| {
                format!("case {i}: T1 {rows:?} -> {rows1:?}\n{}", scene.describe())
            });
        }
        // T2: lining up makes no monitor cover one of another row as seen (one that
        // covered it as the user left them may still, and a row glued to another
        // overlaps it a little at the ends of its seams, as the owner chose).
        let glued = glued_pairs(&scene.monitors, &scene.frame, scene.main, Some(&rows), lid);
        let row_of = |k: &u8| rows.iter().position(|r| r.contains(k));
        let glued_rows = |a: &u8, b: &u8| {
            glued.iter().any(|(p, u)| {
                let (rp, ru) = (row_of(p), row_of(u));
                (row_of(a) == rp && row_of(b) == ru) || (row_of(a) == ru && row_of(b) == rp)
            })
        };
        let corners_of = |s: &Scene| -> Vec<(u8, [Vec3; 4])> {
            s.monitors
                .iter()
                .map(|m| (m.id, s.frame.unplace(m.placement).corners(m.height())))
                .collect()
        };
        let (corners, before) = (corners_of(&lined), corners_of(&scene));
        for (a, (ia, ca)) in corners.iter().enumerate() {
            for (b, (ib, cb)) in corners.iter().enumerate().skip(a + 1) {
                let same_row = rows.iter().any(|r| r.contains(ia) && r.contains(ib));
                let was = covers(&before[a].1, &before[b].1, 0.01);
                // Glued: as deep as OVERLAP_MAX into each other, no deeper.
                if !same_row && !was && glued_rows(ia, ib) {
                    let seen = |k: &u8| {
                        let m = *lined.get(*k).unwrap();
                        Monitor {
                            placement: lined.frame.unplace(m.placement),
                            ..m
                        }
                    };
                    let (ma, mb) = (seen(ia), seen(ib));
                    let deepest = [Side::Above, Side::Below]
                        .into_iter()
                        .flat_map(|side| {
                            [
                                reach_pair(&ma, &mb, side, 17),
                                reach_pair(&mb, &ma, side.opposite(), 17),
                            ]
                        })
                        .flatten()
                        .fold(0.0f64, f64::max);
                    glued_overlaps.push(deepest);
                    if deepest > f64::from(OVERLAP_MAX) + 1e-3 {
                        t2 += 1;
                        first.get_or_insert_with(|| {
                            format!(
                                "case {i}: T2 glued {ia} {ib} {deepest}\n{}",
                                scene.describe()
                            )
                        });
                    }
                    continue;
                }
                if !same_row && !was && covers(ca, cb, 0.01) {
                    t2 += 1;
                    first.get_or_insert_with(|| {
                        format!("case {i}: T2 {ia} covers {ib}\n{}", scene.describe())
                    });
                }
            }
        }
        // T5: lining up again changes nothing (to a tenth of a millimetre).
        let again = line_up(&lined.monitors, &lined.frame, lined.main, Some(&rows), lid);
        if lined
            .monitors
            .iter()
            .any(|m| again[&m.id].position.distance(m.placement.position) >= 1e-4)
        {
            t5 += 1;
        }
        total += 1;
    }
    let shaken = jitter.map_or(String::new(), |(by, seed)| {
        format!(" (moved by {by:e}, {seed:#x})")
    });
    eprintln!(
        "lining up {total}{shaken}: rows changed {t1}, covering {t2}, not settled after twice {t5}"
    );
    glued_overlaps.sort_by(f64::total_cmp);
    if let Some(&worst) = glued_overlaps.last() {
        let at = |q: f64| glued_overlaps[((glued_overlaps.len() - 1) as f64 * q) as usize];
        eprintln!(
            "glued pairs overlapping as seen, {} of them: median {:.3}, p95 {:.3}, the deepest {worst:.3} of the height reached into",
            glued_overlaps.len(),
            at(0.5),
            at(0.95)
        );
    }
    if let Some(f) = &first {
        eprintln!("first: {f}");
    }
    assert!(
        (t1 + t2 + t5) * 20 <= total,
        "{t1} {t2} {t5} of {total}{shaken}"
    );
    t1 + t2 + t5
}

/// T6: after "Line up", each row's neighbours meet on the desktop exactly where they meet
/// as seen (the middles of the seams to within noise), and no pair seen touching is lost.
#[test]
fn after_lining_up_the_desktop_matches_the_view() {
    let mut rng = Rng::new(0x3e7a_0008);
    let mut total = Continuity::default();
    for _ in 0..cases(800) {
        let scene = hand_scene(&mut rng);
        let ids: Vec<u8> = scene.monitors.iter().map(|m| m.id).collect();
        let rows = rows_of(&ids, &scene.room(), None);
        let lid = scene.get(MIRROR).map(|m| m.id);
        let done = line_up(&scene.monitors, &scene.frame, scene.main, Some(&rows), lid);
        let mut lined = scene;
        for m in &mut lined.monitors {
            m.placement = done[&m.id];
        }
        let (_, plan) = pipeline(&lined);
        total.add(&continuity(&lined, &plan, desk::TOUCH_GAP, 0.25));
    }
    eprintln!(
        "after lining up: {} seam points, {} pairs lost, {} blocked; side error {:.1} px (x{:.2}), between rows {:.1} px (x{:.2})",
        total.samples,
        total.lost.len(),
        total.blocked,
        total.side_error,
        total.side_ratio,
        total.stack_error,
        total.stack_ratio
    );
    // Ceilings where lining up stands now (2026-11-21, full count: 39 lost and 176
    // blocked of 32 013 points, 1436 px beside and 4800 px between rows at the worst,
    // one arrangement whose glued rows come to be seen in another order): no worse.
    assert!(
        total.lost.len() * 500 <= total.samples,
        "{} lost",
        total.lost.len()
    );
    assert!(
        total.blocked * 100 <= total.samples,
        "{} blocked",
        total.blocked
    );
    assert!(total.side_error <= 1500.0, "{} px beside", total.side_error);
    assert!(
        total.stack_error <= 5000.0,
        "{} px between rows",
        total.stack_error
    );
}
