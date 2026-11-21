use std::collections::HashMap;

use glam::{Quat, Vec2, Vec3};
use prostir_layout::circle;
use prostir_layout::desk::{self, Spot};
use prostir_layout::space::Frame;
use prostir_layout::tidy;
use prostir_layout::view::Room;
use prostir_proto::state::DisplayId;

use super::arrange::placed;
use super::saved::ascending;
use super::stick::corners;
use super::*;
use crate::ids::{DisplayKey, PcId};
use crate::persist::{self, LoadError};
use crate::settings::Preset;

/// Display `n` of the one PC of these tests.
fn k(n: u32) -> DisplayKey {
    DisplayKey::new(PcId([5; 8]), DisplayId(n))
}

/// `map`, by the displays of the PC of these tests.
fn keyed<V>(map: HashMap<u32, V>) -> HashMap<DisplayKey, V> {
    map.into_iter().map(|(n, v)| (k(n), v)).collect()
}

/// `map`, by the numbers of its displays.
fn numbered<V: Clone>(map: &HashMap<DisplayKey, V>) -> HashMap<u32, V> {
    map.iter().map(|(k, v)| (k.id.0, v.clone())).collect()
}

/// Every number as it is on the PC.
fn as_is(n: u32) -> Option<DisplayId> {
    Some(DisplayId(n))
}

#[test]
fn arc_has_no_gaps_and_faces_user() {
    let l = arrange(
        Preset::Arc,
        &[(0, 2560, 1440), (1, 2560, 1440), (2, 1920, 1080)],
        Some(1),
        None,
        None,
    );
    for p in l.values() {
        let normal = p.rotation * Vec3::Z;
        let to_axis = (Vec3::new(0.0, p.position.y, 0.0) - p.position).normalize();
        assert!(normal.dot(to_axis) > 0.999);
    }
    // Right edge of one monitor is the left edge of the next.
    let edge = |p: &Placement, side: f32| {
        p.position + p.rotation * Vec3::new(side * p.width / 2.0, 0.0, 0.0)
    };
    for (a, b) in [(0, 1), (1, 2)] {
        let (ea, eb) = (edge(&l[&a], 1.0), edge(&l[&b], -1.0));
        assert!(
            Vec2::new(ea.x - eb.x, ea.z - eb.z).length() < 1e-4,
            "gap between {a} and {b}"
        );
    }
    assert!(l[&0].position.x < l[&1].position.x && l[&1].position.x < l[&2].position.x);
}

/// Where each monitor is seen from the head: yaw span and height span.
fn seen_spans(p: &Placement, h: f32) -> ((f32, f32), (f32, f32)) {
    let flat = Vec2::new(p.position.x, p.position.z).length();
    let yaw = p.position.x.atan2(-p.position.z);
    let half = (p.width / 2.0 / flat.max(0.1)).atan();
    (
        (yaw - half, yaw + half),
        (p.position.y - h / 2.0, p.position.y + h / 2.0),
    )
}

/// Any monitors in a preset: none in front of another as seen from the head, every
/// row within the arc.
#[test]
fn presets_never_overlap() {
    let sizes = [
        (1920, 1080),
        (2560, 1440),
        (3440, 1440),
        (5120, 1440),
        (3840, 2160),
        (1440, 2560),
    ];
    let mut seed = 0x1234_u64;
    for case in 0..2000 {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let n = 1 + (seed >> 33) as usize % 10;
        let mons: Vec<(u32, u32, u32)> = (0..n)
            .map(|i| {
                let (w, h) = sizes[(seed >> (i * 3 % 30 + 2)) as usize % sizes.len()];
                (i as u32, w, h)
            })
            .collect();
        let primary = Some(((seed >> 7) as usize % n) as u32);
        let l = arrange(Preset::Arc, &mons, primary, None, None);
        let spans: Vec<_> = mons
            .iter()
            .map(|&(id, w, h)| {
                let p = l[&id];
                seen_spans(&p, p.width * h as f32 / w as f32)
            })
            .collect();
        for i in 0..n {
            for j in i + 1..n {
                let ((a0, a1), (b0, b1)) = spans[i];
                let ((c0, c1), (d0, d1)) = spans[j];
                let across = a0 < c1 - 1e-3 && c0 < a1 - 1e-3;
                let up = b0 < d1 - 1e-4 && d0 < b1 - 1e-4;
                assert!(
                    !(across && up),
                    "case {case}: {mons:?}: {i} and {j} overlap"
                );
            }
        }
        // The circle they stand on is the preset's radius.
        let radius = preset_radius(Preset::Arc, &mons);
        for (&(_, w, _), p) in mons.iter().map(|m| (m, l[&m.0])) {
            let half = w as f32 * METRES_PER_PIXEL / 2.0;
            let edge =
                (Vec2::new(p.position.x, p.position.z).length().powi(2) + half * half).sqrt();
            assert!(
                (edge - radius).abs() < 1e-3,
                "case {case}: {edge} vs {radius}"
            );
        }
    }
}

#[test]
fn mirror_goes_under_primary() {
    let l = arrange(
        Preset::Arc,
        &[(0, 2560, 1440), (1, 2560, 1440), (2, 2560, 1440)],
        Some(1),
        Some((100, 2880, 1800)),
        None,
    );
    let (m, p) = (l[&100], l[&1]);
    assert!(m.position.y < p.position.y - 0.5);
    assert!((m.position.x - p.position.x).abs() < 1e-3);
    assert!(m.width <= circle::MIRROR_MAX_WIDTH + 1e-6);
    // Leans back to face the eyes.
    assert!((m.rotation * Vec3::Z).y > 0.2);
}

#[test]
fn every_preset_places_everything() {
    let mons = [
        (0, 2560, 1440),
        (1, 1920, 1080),
        (2, 1440, 2560),
        (3, 3440, 1440),
    ];
    for preset in [Preset::Arc, Preset::Wall, Preset::Overhead] {
        let l = arrange(preset, &mons, Some(0), Some((100, 2880, 1800)), None);
        assert_eq!(l.len(), 5, "{preset:?}");
        assert!(
            l.values()
                .all(|p| p.position.is_finite() && p.rotation.is_finite())
        );
    }
}

#[test]
fn strong_curve_on_arc_is_centred_on_the_head() {
    let l = arrange(
        Preset::Arc,
        &[(0, 2560, 1440), (1, 2560, 1440)],
        None,
        None,
        None,
    );
    for p in l.values() {
        let c = Cylinder::new(p, RADIUS);
        assert!(
            Vec2::new(c.center.x, c.center.z).length() < 1e-3,
            "{:?}",
            c.center
        );
    }
}

#[test]
fn curved_hit_matches_flat_at_centre_and_edges() {
    let p = Placement {
        position: Vec3::new(0.0, 0.0, -1.0),
        rotation: Quat::IDENTITY,
        width: 1.2,
    };
    let ray = Ray {
        origin: Vec3::ZERO,
        dir: -Vec3::Z,
    };
    let h = hit_surface(ray, &p, 0.7, Some(1.5)).unwrap();
    assert!(h.local.length() < 1e-4);
    assert!(h.t > 1.0, "the arc bulges away from the viewer");
    assert!(h.normal.dot(Vec3::Z) > 0.999);
    // Aiming at the flat edge hits the curved edge (shared chord endpoints).
    let edge = Ray {
        origin: Vec3::ZERO,
        dir: Vec3::new(0.599, 0.0, -1.0).normalize(),
    };
    let h = hit_surface(edge, &p, 0.7, Some(1.5)).unwrap();
    assert!((h.local.x - 0.599).abs() < 0.01, "{:?}", h.local);
    assert!(
        hit_surface(
            Ray {
                origin: Vec3::ZERO,
                dir: Vec3::Z
            },
            &p,
            0.7,
            Some(1.5)
        )
        .is_none()
    );
}

#[test]
fn order_follows_given_sequence_and_directions() {
    // Portrait in the middle: order [0, 2, 1].
    let l = arrange(
        Preset::Arc,
        &[(0, 2560, 1440), (2, 1440, 2560), (1, 2560, 1440)],
        Some(2),
        None,
        None,
    );
    assert!(l[&0].position.x < l[&2].position.x && l[&2].position.x < l[&1].position.x);
    assert!(l[&2].position.x.abs() < 1e-3, "portrait is straight ahead");
}

#[test]
fn ray_hits_center() {
    let p = Placement {
        position: Vec3::new(0.0, 0.0, -1.0),
        rotation: Quat::IDENTITY,
        width: 1.0,
    };
    let (t, local) = hit(
        Ray {
            origin: Vec3::ZERO,
            dir: -Vec3::Z,
        },
        &p,
        0.5,
    )
    .unwrap();
    assert!((t - 1.0).abs() < 1e-5 && local.length() < 1e-5);
    assert!(
        hit(
            Ray {
                origin: Vec3::ZERO,
                dir: Vec3::Z
            },
            &p,
            0.5
        )
        .is_none()
    );
}

#[test]
fn cinema_shows_one_monitor() {
    // Row order: portrait in the middle.
    let monitors = [(0, 2560, 1440), (2, 1440, 2560), (1, 3840, 2160)];
    let out = cinema(&monitors, Some(2));
    assert_eq!(out.keys().copied().collect::<Vec<_>>(), vec![2], "picked");
    assert!(out[&2].position.z < -2.0, "further away than the desk");
    // Nothing picked (or a stale pick): the largest landscape monitor, not a portrait one.
    assert_eq!(cinema_choice(&monitors, None), Some(1));
    assert_eq!(cinema_choice(&monitors, Some(9)), Some(1));
    assert_eq!(cinema_choice(&[(4, 1440, 2560)], None), Some(4));
    assert!(cinema::<u32>(&[], None).is_empty());
}

#[test]
fn mirror_hangs_from_the_monitor_above() {
    let monitors = [(0, 2560, 1440), (1, 1440, 2560), (2, 2560, 1440)];
    for primary in [0, 1] {
        let out = arrange(
            Preset::Arc,
            &monitors,
            Some(primary),
            Some((100, 2880, 1800)),
            None,
        );
        let above = out[&primary];
        let h_above = monitors.iter().find(|m| m.0 == primary).unwrap().2 as f32 * METRES_PER_PIXEL;
        let mirror = out[&100];
        let h =
            1800.0 * METRES_PER_PIXEL * (circle::MIRROR_MAX_WIDTH / (2880.0 * METRES_PER_PIXEL));
        let top_edge = mirror.position + mirror.rotation * Vec3::new(0.0, h / 2.0, 0.0);
        let bottom_edge = above.position - Vec3::Y * h_above / 2.0;
        // The portrait one in the middle hangs lowest; under it there is no gap.
        if primary == 1 {
            assert!(
                top_edge.distance(bottom_edge) < 1e-3,
                "{top_edge} vs {bottom_edge}"
            );
        }
        assert!(
            top_edge.y <= bottom_edge.y + 1e-3,
            "never above the monitor's bottom"
        );
        // Tilted back: its normal points up towards the eyes.
        assert!((mirror.rotation * Vec3::Z).y > 0.05);
    }
}

#[test]
fn curved_mirror_continues_the_cylinder() {
    let monitors = [(0, 2560, 1440), (1, 2560, 1440), (2, 2560, 1440)];
    let radius = RADIUS;
    let out = arrange(
        Preset::Arc,
        &monitors,
        Some(1),
        Some((100, 2880, 1800)),
        Some(1.0),
    );
    let (above, mirror) = (out[&1], out[&100]);
    let a = Cylinder::new(&above, radius);
    let m = Cylinder::new(&mirror, radius);
    // Same axis (horizontally) and same radius: the edges meet along the whole width.
    assert!(Vec2::new(a.center.x - m.center.x, a.center.z - m.center.z).length() < 1e-3);
    let h_above = 1440.0 * METRES_PER_PIXEL;
    let h = 1800.0 * METRES_PER_PIXEL * (circle::MIRROR_MAX_WIDTH / (2880.0 * METRES_PER_PIXEL));
    assert!(((above.position.y - h_above / 2.0) - (mirror.position.y + h / 2.0)).abs() < 1e-3);
}

/// The desktop for `placements` as the PC gets it, seen from straight ahead.
fn desktop_of(
    placements: &HashMap<u32, Placement>,
    m: &[(u32, u32, u32)],
    preset: Preset,
    primary: Option<u32>,
) -> Desktop<u32> {
    Desktop::of(
        placements,
        m,
        (Vec3::ZERO, 0.0),
        tilt(preset),
        primary,
        None,
    )
}

#[test]
fn rows_follow_what_the_user_sees() {
    // An arc: one row, left to right.
    let three = [(0, 2560, 1440), (1, 1440, 2560), (2, 2560, 1440)];
    let arc = arrange(Preset::Arc, &three, Some(1), None, None);
    assert_eq!(
        desktop_of(&arc, &three, Preset::Arc, Some(1)).rows,
        vec![vec![0, 1, 2]],
        "a portrait monitor in the arc is in the same row"
    );

    // Lift the right monitor above the middle one: two rows, it is on top.
    let mut lifted = arc.clone();
    let mid = arc[&1];
    let mut top = arc[&2];
    top.position = Vec3::new(mid.position.x, mid.position.y + 1.0, mid.position.z);
    top.rotation = facing(top.position, Vec3::ZERO);
    lifted.insert(2, top);
    let desktop = desktop_of(&lifted, &three, Preset::Arc, Some(1));
    assert_eq!(desktop.rows, vec![vec![2], vec![0, 1]]);
    // It is seen right over the middle monitor, not over the right edge.
    let rows = desktop.wire(as_is);
    let seen_at = |row: usize, i: usize| rows[row].entries[i].center.unwrap();
    let middle = seen_at(1, 1);
    assert!((seen_at(0, 0) - middle).abs() < 150, "{rows:?}");
    assert!(seen_at(1, 0) < middle - 1500, "{rows:?}");

    // A wall: one row, left to right.
    let wall = arrange(Preset::Wall, &three, None, None, None);
    assert_eq!(
        desktop_of(&wall, &three, Preset::Wall, None).rows,
        vec![vec![0, 1, 2]]
    );
}

const H: f32 = 1440.0 / 2560.0;

/// How far the nearest corners of two monitors are from lying on one shared edge.
fn seam(a: &Placement, b: &Placement) -> f32 {
    let (ca, cb) = (corners(a, a.width * H), corners(b, b.width * H));
    // Each of b's two corners nearest to a must be on a's edge line.
    let mut d: Vec<f32> = cb
        .iter()
        .map(|c| {
            ca.iter()
                .zip(ca.iter().cycle().skip(1))
                .map(|(p, q)| {
                    let t = ((c - p).dot(q - p) / (q - p).length_squared()).clamp(-5.0, 5.0);
                    c.distance(p + (q - p) * t)
                })
                .fold(f32::MAX, f32::min)
        })
        .collect();
    d.sort_by(f32::total_cmp);
    d[1]
}

#[test]
fn the_magnet_needs_a_monitor_almost_against_another() {
    let two = [(0, 2560, 1440), (1, 2560, 1440)];
    let arc = arrange(Preset::Arc, &two, None, None, None);
    let moved = |by: Vec3| {
        let mut p = arc.clone();
        let m = p.get_mut(&1).unwrap();
        m.position += m.rotation * by;
        p
    };
    // Let go 3 cm off: sticks back, edge to edge.
    let p = moved(Vec3::new(0.03, 0.01, 0.0));
    let s = stick(&p, &two, Vec3::ZERO, 1, &mut StickScratch::default()).expect("sticks");
    assert!(s.across && s.side > 0.0);
    let mut after = p;
    after.insert(1, s.placement);
    assert!(seam(&after[&0], &after[&1]) < 1e-3);
    // A clear gap left on purpose (40 cm): no magnet.
    assert!(
        stick(
            &moved(Vec3::new(0.4, 0.0, 0.0)),
            &two,
            Vec3::ZERO,
            1,
            &mut StickScratch::default()
        )
        .is_none()
    );
    // Pulled half a metre nearer on purpose: no magnet.
    assert!(
        stick(
            &moved(Vec3::new(0.0, 0.0, 0.5)),
            &two,
            Vec3::ZERO,
            1,
            &mut StickScratch::default()
        )
        .is_none()
    );
}

#[test]
fn dropped_on_a_seam_it_joins_the_nearer_monitor() {
    let three = [(0, 2560, 1440), (1, 2560, 1440), (2, 2560, 1440)];
    let mut p = arrange(
        Preset::Arc,
        &[(1, 2560, 1440), (2, 2560, 1440)],
        None,
        None,
        None,
    );
    let over2 = p[&2].position + p[&2].rotation * Vec3::new(0.4, 0.0, 0.0);
    p.insert(
        0,
        Placement {
            position: over2,
            rotation: facing(over2, Vec3::new(0.0, over2.y, 0.0)),
            width: p[&1].width,
        },
    );
    let s = stick(&p, &three, Vec3::ZERO, 0, &mut StickScratch::default()).expect("sticks");
    assert_eq!(s.neighbour, 2);
}

#[test]
fn on_a_wall_the_magnet_stays_in_the_plane() {
    let two = [(0, 2560, 1440), (1, 2560, 1440)];
    let mut p = arrange(Preset::Wall, &two, None, None, None);
    let w = p[&0].width;
    p.get_mut(&1).unwrap().position = p[&0].position + Vec3::new(w + 0.1, 0.02, 0.05);
    let s = stick_on_wall(&p, &two, (Vec3::ZERO, 0.0), 1, &mut StickScratch::default())
        .expect("sticks");
    assert!((s.placement.position.z - p[&0].position.z).abs() < 1e-5);
    assert!((s.placement.position.x - (p[&0].position.x + w)).abs() < 1e-4);
    assert!((s.placement.position.y - p[&0].position.y).abs() < 1e-4);
}

#[test]
fn lying_down_the_monitors_are_one_row() {
    let three = [(0, 2560, 1440), (1, 2560, 1440), (2, 2560, 1440)];
    let overhead = arrange(Preset::Overhead, &three, None, None, None);
    let desktop = desktop_of(&overhead, &three, Preset::Overhead, None);
    assert_eq!(desktop.rows, vec![vec![0, 1, 2]]);
}

/// A wall: A closes the gap and the step a hand left, in the wall's plane.
#[test]
fn a_wall_lines_up_in_its_plane() {
    let center = (Vec3::ZERO, 0.0);
    let two = [(0, 2560, 1440), (1, 2560, 1440)];
    let mut rough = arrange(Preset::Wall, &two, None, None, None);
    rough.get_mut(&1).unwrap().position += Vec3::new(0.06, 0.01, 0.0);
    let t = line_up(&rough, &two, center, tilt(Preset::Wall), Some(0), &[], None);
    assert!((t[&0].position.z - t[&1].position.z).abs() < 1e-4);
    assert!(t[&0].rotation.angle_between(t[&1].rotation) < 1e-4);
    assert!((t[&0].position.y - t[&1].position.y).abs() < 1e-4);
    let gap = (t[&1].position.x - t[&1].width / 2.0) - (t[&0].position.x + t[&0].width / 2.0);
    assert!(gap.abs() < 1e-3, "gap {gap}");
}

/// What the PC makes of `desktop` as `SetLayout` brings it (as
/// `crates/prostir/src/displays/` does): boxes from the wire, planned again.
fn on_the_pc(
    desktop: &Desktop<u32>,
    m: &[(u32, u32, u32)],
    primary: Option<u32>,
) -> Vec<Spot<u32>> {
    let size = |id: u32| {
        let &(_, w, h) = m.iter().find(|x| x.0 == id)?;
        Some(desk::Size {
            w: w as i32,
            h: h as i32,
        })
    };
    let boxes = desk::Boxes::new(
        desktop
            .wire(as_is)
            .iter()
            .flat_map(|r| &r.entries)
            .filter_map(|e| {
                let s = size(e.id.0)?;
                let seen = desk::SeenBox::from_wire(e.center?, e.middle?, s);
                Some((e.id.0, seen, s))
            })
            .collect(),
    );
    let wanted = desk::Desk {
        rows: desktop.rows.clone(),
        main: primary,
        ..desk::Desk::default()
    };
    let mut plan = desk::plan(&wanted, size, &boxes);
    plan.sort_by_key(|s| s.id);
    plan
}

/// The PC's desktop is the map's, to the pixel.
fn same_on_the_pc(desktop: &Desktop<u32>, m: &[(u32, u32, u32)], primary: Option<u32>) {
    let mut mine = desktop.plan.clone();
    mine.sort_by_key(|s| s.id);
    assert_eq!(on_the_pc(desktop, m, primary), mine, "{:?}", desktop.rows);
}

/// Whether two monitors cover each other as seen from `center`.
fn covered(placements: &HashMap<u32, Placement>, m: &[(u32, u32, u32)]) -> bool {
    let all = placed(placements, m);
    let seen = prostir_layout::space::see(&frame((Vec3::ZERO, 0.0), tilt(Preset::Arc)), &all);
    seen.iter().enumerate().any(|(i, a)| {
        seen[i + 1..]
            .iter()
            .any(|b| prostir_layout::space::overlaps(a, b, 0.01))
    })
}

/// Every row of `desktop` lies on one line (tops, middles or bottoms), side by side.
fn rows_on_a_line(desktop: &Desktop<u32>) {
    for row in &desktop.rows {
        let spots: Vec<Spot<u32>> = row
            .iter()
            .filter_map(|id| desktop.plan.iter().find(|s| s.id == *id).copied())
            .collect();
        let line =
            |f: &dyn Fn(&Spot<u32>) -> i32| spots.iter().all(|s| (f(s) - f(&spots[0])).abs() <= 1);
        assert!(
            line(&|s| s.y) || line(&|s| s.bottom()) || line(&|s| 2 * s.y + s.h),
            "one line: {:?}",
            desktop.plan
        );
        for w in spots.windows(2) {
            assert_eq!(w[0].right(), w[1].x, "side by side: {:?}", desktop.plan);
        }
    }
}

/// The six monitors as the headset had them on 2026-11-19 (one row on a 1.1 m
/// circle, overlapping): the desktop the PC makes of them has no staircase (the
/// old one spread over -1720..7600 with neighbours meeting at corners). Laid out
/// again by the preset now: two rows on a wider circle, nothing overlapping.
#[test]
fn six_overlapping_monitors_make_no_staircase() {
    let m = [
        (0, 3440, 1440),
        (1, 2560, 1440),
        (2, 1440, 2560),
        (3, 3440, 1440),
        (4, 2560, 1440),
        (5, 3440, 1440),
    ];
    // The old preset: angles cut at asin(0.95), so they overlapped.
    let angle = |w: f32| 2.0 * (w / 2.0 / RADIUS).min(0.95).asin();
    let widths: Vec<f32> = m.iter().map(|x| x.1 as f32 * METRES_PER_PIXEL).collect();
    let total: f32 = widths.iter().map(|w| angle(*w)).sum();
    let mut yaw = -total / 2.0;
    let mut p = HashMap::new();
    for (x, w) in m.iter().zip(&widths) {
        let a = angle(*w);
        let half = (w / 2.0 / RADIUS).min(0.95).asin();
        let r = RADIUS * half.cos();
        let at = yaw + a / 2.0;
        let position = Vec3::new(r * at.sin(), -0.08, -r * at.cos());
        p.insert(
            x.0,
            Placement {
                position,
                rotation: facing(position, Vec3::new(0.0, -0.08, 0.0)),
                width: *w,
            },
        );
        yaw += a;
    }
    assert!(covered(&p, &m), "they did overlap");
    let desktop = desktop_of(&p, &m, Preset::Arc, Some(2));
    rows_on_a_line(&desktop);
    same_on_the_pc(&desktop, &m, Some(2));
    let fresh = arrange(Preset::Arc, &m, Some(2), None, None);
    assert!(!covered(&fresh, &m));
    assert!(preset_radius(Preset::Arc, &m) > RADIUS);
    let desktop = desktop_of(&fresh, &m, Preset::Arc, Some(2));
    assert_eq!(desktop.rows.len(), 2, "{:?}", desktop.rows);
    rows_on_a_line(&desktop);
    same_on_the_pc(&desktop, &m, Some(2));
}

/// Two rows made by hand live through everything that happens to them: a monitor
/// added and removed, the headset app restarted (layout.txt), "Line up", a profile
/// and back. Every time the same rows, and the same desktop for the PC.
#[test]
fn two_rows_survive_everything() {
    let center = (Vec3::ZERO, 0.0);
    let m = [
        (0, 2560, 1440),
        (1, 2560, 1440),
        (2, 1920, 1080),
        (3, 1920, 1080),
        (4, 1440, 2560),
    ];
    let four = &m[..4];
    let new = |p: &HashMap<u32, Placement>, of: &[(u32, u32, u32)], gaze: Vec3| {
        let id = of[of.len() - 1];
        let others = &of[..of.len() - 1];
        place_new(
            p,
            others,
            (id.1, id.2),
            center,
            tilt(Preset::Arc),
            gaze,
            &[],
            RADIUS,
        )
    };
    // Two side by side, two more put over them by hand.
    let mut p = arrange(Preset::Arc, &m[..2], Some(0), None, None);
    p.extend(raised(
        &arrange(Preset::Arc, &m[2..4], None, None, None),
        &p,
        &m,
    ));
    let p = line_up(&p, four, center, tilt(Preset::Arc), Some(0), &[], None);
    let desktop = desktop_of(&p, four, Preset::Arc, Some(0));
    assert_eq!(desktop.rows.len(), 2, "{:?}", desktop.rows);
    same_on_the_pc(&desktop, four, Some(0));
    let same = |q: &HashMap<u32, Placement>, what: &str| {
        let again = Desktop::of(q, four, center, tilt(Preset::Arc), Some(0), Some(&desktop));
        assert_eq!(again, desktop, "{what}");
    };
    // A monitor added: the others stay, and so do their rows.
    let mut with = p.clone();
    with.insert(4, new(&p, &m, -Vec3::Z));
    for (id, place) in &p {
        assert_eq!(with[id], *place, "{id} moved");
    }
    assert!(!covered(&with, &m), "the new one covers nothing");
    let five = Desktop::of(
        &with,
        &m,
        center,
        tilt(Preset::Arc),
        Some(0),
        Some(&desktop),
    );
    let kept: Vec<Vec<u32>> = five
        .rows
        .iter()
        .map(|r| r.iter().copied().filter(|id| *id != 4).collect::<Vec<_>>())
        .filter(|r| !r.is_empty())
        .collect();
    assert_eq!(
        kept, desktop.rows,
        "a new monitor joins, nobody leaves a row"
    );
    same_on_the_pc(&five, &m, Some(0));
    // And removed again.
    with.remove(&4);
    same(&with, "removed");
    // The headset app restarted.
    let mut desk = Desk::default();
    desk.keep(
        keyed(p.clone()),
        &HashMap::new(),
        (center, RADIUS, 0.0),
        false,
    );
    same(&numbered(&reread(&desk).saved.placements), "restarted");
    // Lined up again.
    same(
        &line_up(
            &p,
            four,
            center,
            tilt(Preset::Arc),
            Some(0),
            &desktop.rows,
            None,
        ),
        "lined up",
    );
    // Saved in a profile, another profile, and back: the places by their order.
    let profile = saved_profile(four, Some(0), &p, center, &desktop.rows);
    let (back, _) = switched_to(&profile, four, center);
    same(&back, "profile and back");
}

/// `top`, moved up as a whole to stand over `below` with a hand's gap.
fn raised(
    top: &HashMap<u32, Placement>,
    below: &HashMap<u32, Placement>,
    m: &[(u32, u32, u32)],
) -> HashMap<u32, Placement> {
    let height = |p: &HashMap<u32, Placement>, id: u32| {
        let (_, w, h) = m.iter().find(|x| x.0 == id).copied().unwrap();
        p[&id].width * h as f32 / w as f32
    };
    let tallest =
        |p: &HashMap<u32, Placement>| p.keys().map(|id| height(p, *id)).fold(0.0f32, f32::max);
    let lift = (tallest(top) + tallest(below)) / 2.0 + 0.05;
    let mut out = top.clone();
    for id in top.keys() {
        out = moved(&out, *id, 0.0, lift);
    }
    out
}

/// Rows of `p` as the PC gets them, and the row that holds `id`.
fn row_of(rows: &[Vec<u32>], id: u32) -> usize {
    rows.iter().position(|r| r.contains(&id)).unwrap()
}

/// 2026-11-20, the owner's report: a new monitor went into a row of its own over
/// the others, and the next one over that. Four monitors in two rows and the
/// laptop's panel under them: whether the user looks at the menu, at a monitor or
/// up, a new one joins a row, beside the others.
#[test]
fn a_new_monitor_joins_a_row() {
    let center = (Vec3::ZERO, 0.0);
    let m = [
        (3, 2560, 1440),
        (1, 2560, 1440),
        (100, 2880, 1800),
        (0, 2560, 1440),
        (2, 2560, 1440),
        (104, 2560, 1440),
    ];
    let mut p = arrange(Preset::Arc, &m[..2], Some(1), Some(m[2]), None);
    p.extend(raised(
        &arrange(Preset::Arc, &m[3..5], None, None, None),
        &p,
        &m,
    ));
    let rows = desktop_of(&p, &m[..5], Preset::Arc, Some(1)).rows;
    assert!(!covered(&p, &m[..5]));
    assert_eq!(rows.len(), 3, "{rows:?}");
    for gaze in [
        Vec3::new(0.0, -0.5, -1.0).normalize(),
        p[&0].position.normalize(),
        (p[&2].position + Vec3::Y).normalize(),
        Vec3::Y,
    ] {
        let mut with = p.clone();
        with.insert(
            104,
            place_new(
                &p,
                &m[..5],
                (m[5].1, m[5].2),
                center,
                tilt(Preset::Arc),
                gaze,
                &rows,
                RADIUS,
            ),
        );
        assert!(!covered(&with, &m), "it covers nothing");
        let after = desktop_of(&with, &m, Preset::Arc, Some(1)).rows;
        assert_eq!(after.len(), rows.len(), "no new row: {after:?}");
        let row = &after[row_of(&after, 104)];
        assert_eq!(row.len(), 3, "beside two others: {after:?}");
    }
}

/// Three big monitors fill the row around the user (210 degrees) with the laptop's
/// panel under them: a new one has room only over them, and the one after it
/// joins it there instead of starting yet another row.
#[test]
fn with_a_full_row_new_monitors_share_one_row_over_it() {
    let center = (Vec3::ZERO, 0.0);
    let m = [
        (3, 2560, 1440),
        (1, 2560, 1440),
        (0, 2560, 1440),
        (100, 2880, 1800),
        (104, 2560, 1440),
        (105, 2560, 1440),
    ];
    let mut p = arrange(Preset::Arc, &m[..3], Some(1), Some(m[3]), None);
    let ahead = Vec3::new(0.0, -0.3, -1.0).normalize();
    let mut rows = desktop_of(&p, &m[..4], Preset::Arc, Some(1)).rows;
    assert_eq!(rows.len(), 2, "{rows:?}");
    for n in [4, 5] {
        let at = place_new(
            &p,
            &m[..n],
            (m[n].1, m[n].2),
            center,
            tilt(Preset::Arc),
            ahead,
            &rows,
            RADIUS,
        );
        p.insert(m[n].0, at);
        assert!(!covered(&p, &m[..=n]), "it covers nothing");
        rows = desktop_of(&p, &m[..=n], Preset::Arc, Some(1)).rows;
    }
    assert_eq!(rows.len(), 3, "one row more, not two: {rows:?}");
    assert_eq!(row_of(&rows, 104), row_of(&rows, 105), "{rows:?}");
}

/// A small seeded generator, so a failing case comes back the same.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    /// `lo..hi`.
    fn float(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * (self.next() as f32 / (1u64 << 31) as f32)
    }
}

/// `placements` with monitor `id` turned `right` radians around the eye (to the
/// right when positive) and moved `up` metres.
fn moved(
    placements: &HashMap<u32, Placement>,
    id: u32,
    right: f32,
    up: f32,
) -> HashMap<u32, Placement> {
    let turn = Quat::from_rotation_y(-right);
    let mut out = placements.clone();
    let p = out.get_mut(&id).unwrap();
    p.position = turn * p.position + Vec3::Y * up;
    p.rotation = (turn * p.rotation).normalize();
    out
}

/// 2026-11-20, the owner's drag: 2560x1440 | 1440x2560 primary | 2560x1440 in one
/// row, each moved by hand up to 8 cm up and down and 1.5° into its neighbour or
/// away, with a 2 mm tremor. In every frame, as the headset sends it (rows after
/// the rows sent before), one row: no flipping, and the PC puts the portrait
/// between the two, side by side, as the map shows.
#[test]
fn a_portrait_between_two_stays_in_their_row_while_they_move() {
    let center = (Vec3::ZERO, 0.0);
    let three = [(0, 2560, 1440), (1, 1440, 2560), (2, 2560, 1440)];
    let start = arrange(Preset::Arc, &three, Some(1), None, None);
    let mut rng = Rng(0x0f11);
    let mut at: HashMap<u32, (f32, f32)> = HashMap::new();
    let mut sent: Option<Desktop<u32>> = None;
    let mut flips = 0;
    for drag in 0..60 {
        let id = drag % 3;
        let from = at.get(&id).copied().unwrap_or_default();
        let to = (rng.float(-1.5, 1.5).to_radians(), rng.float(-0.08, 0.08));
        for f in 1..=20 {
            let t = f as f32 / 20.0;
            let now = (
                from.0 + (to.0 - from.0) * t + rng.float(-0.002, 0.002) / 1.1,
                from.1 + (to.1 - from.1) * t + rng.float(-0.002, 0.002),
            );
            at.insert(id, now);
            let mut p = start.clone();
            for (&k, &(r, u)) in &at {
                p = moved(&p, k, r, u);
            }
            let desktop = Desktop::of(
                &p,
                &three,
                center,
                tilt(Preset::Arc),
                Some(1),
                sent.as_ref(),
            );
            assert_eq!(desktop.rows, vec![vec![0, 1, 2]], "drag {drag} frame {f}");
            if sent.as_ref().is_some_and(|s| s.rows != desktop.rows) {
                flips += 1;
            }
            let pc = on_the_pc(&desktop, &three, Some(1));
            let spot = |id: u32| pc.iter().find(|s| s.id == id).copied().unwrap();
            assert_eq!(
                spot(0).crossing(&spot(1)),
                Some(prostir_layout::space::Side::Right)
            );
            assert_eq!(
                spot(1).crossing(&spot(2)),
                Some(prostir_layout::space::Side::Right)
            );
            same_on_the_pc(&desktop, &three, Some(1));
            sent = Some(desktop);
        }
    }
    assert_eq!(flips, 0);
}

/// `placements` turned by `turn` about the eye at `eye`.
fn turned_about(
    placements: &HashMap<u32, Placement>,
    eye: Vec3,
    turn: Quat,
) -> HashMap<u32, Placement> {
    placements
        .iter()
        .map(|(id, p)| {
            let q = Placement {
                position: eye + turn * (p.position - eye),
                rotation: (turn * p.rotation).normalize(),
                width: p.width,
            };
            (*id, q)
        })
        .collect()
}

/// The owner's rows of 2026-11-20 tilted back as a whole by 10°, 20° and 35° for a
/// reclined pose: "Line up" keeps them tilted where they are, and after a hand
/// disturbed one (a gap, a step, a monitor turned or hanging crooked) gives the
/// tilted row back, seams closed, the tilt kept, one row on the desktop.
#[test]
fn line_up_keeps_a_row_tilted_back() {
    let eye = Vec3::new(0.3, 1.2, -0.4);
    let center = (eye, 0.4);
    for m in [
        [(0u32, 2560u32, 1440u32), (1, 2560, 1440), (2, 2560, 1440)],
        [(0, 2560, 1440), (1, 1440, 2560), (2, 2560, 1440)],
    ] {
        let upright = {
            let turn = Quat::from_rotation_y(center.1);
            turned_about(
                &arrange(Preset::Arc, &m, Some(1), None, None),
                Vec3::ZERO,
                turn,
            )
            .into_iter()
            .map(|(id, mut p)| {
                p.position += eye;
                (id, p)
            })
            .collect::<HashMap<u32, Placement>>()
        };
        for back in [10.0f32, 20.0, 35.0] {
            let yaw = Quat::from_rotation_y(center.1);
            let turn = yaw * Quat::from_rotation_x(-back.to_radians()) * yaw.inverse();
            let tilted = turned_about(&upright, eye, turn);
            let out = line_up(&tilted, &m, center, tilt(Preset::Arc), Some(1), &[], None);
            for (id, p) in &tilted {
                assert!(
                    out[id].position.distance(p.position) < 1e-3,
                    "{back}°: {id} moved"
                );
            }
            let mut hand = tilted;
            let axis = turn * (yaw * Vec3::Y);
            let left = turn * (yaw * Vec3::NEG_X);
            hand.get_mut(&0).unwrap().position += left * 0.03;
            let p = hand.get_mut(&2).unwrap();
            p.position += axis * 0.04;
            p.rotation = (Quat::from_axis_angle(axis, 2f32.to_radians()) * p.rotation).normalize();
            let p = hand.get_mut(&1).unwrap();
            p.rotation = (Quat::from_axis_angle(p.rotation * Vec3::Z, 5f32.to_radians())
                * p.rotation)
                .normalize();
            let out = line_up(&hand, &m, center, tilt(Preset::Arc), Some(1), &[], None);
            for id in [0u32, 1, 2] {
                let up = out[&id].rotation * Vec3::Y;
                assert!(
                    up.angle_between(axis).to_degrees() < 0.1,
                    "{back}°: {id} leans"
                );
            }
            for (a, b) in [(0u32, 1u32), (1, 2)] {
                let h = |id: u32| {
                    let s = m.iter().find(|x| x.0 == id).unwrap();
                    out[&id].width * s.2 as f32 / s.1 as f32
                };
                let (ca, cb) = (corners(&out[&a], h(a)), corners(&out[&b], h(b)));
                // `corners`: bottom left, bottom right, top right, top left.
                let right_of_a = (ca[1] + ca[2]) / 2.0;
                let left_of_b = (cb[0] + cb[3]) / 2.0;
                assert!(
                    right_of_a.distance(left_of_b) < 1e-3,
                    "{back}°: seam {a}-{b}"
                );
            }
            let desktop = Desktop::of(&out, &m, center, tilt(Preset::Arc), Some(1), None);
            assert_eq!(desktop.rows, vec![vec![0, 1, 2]], "{back}°");
            rows_on_a_line(&desktop);
            same_on_the_pc(&desktop, &m, Some(1));
        }
    }
}

/// `p` mirrored in the vertical plane through the origin at azimuth `at` (0 ahead,
/// positive right).
fn mirrored(p: &Placement, at: f32) -> Placement {
    let n = Vec3::new(at.cos(), 0.0, at.sin());
    let m = |v: Vec3| v - 2.0 * v.dot(n) * n;
    let right = -m(p.rotation * Vec3::X);
    let up = m(p.rotation * Vec3::Y);
    let face = m(p.rotation * Vec3::Z);
    Placement {
        position: m(p.position),
        rotation: Quat::from_mat3(&glam::Mat3::from_cols(right, up, face)).normalize(),
        width: p.width,
    }
}

/// Two 3440 monitors standing mirror-symmetric (each a little off and crooked the
/// same way, mirrored), no primary: lined up they are still mirror images. Step 4
/// kept the one nearest straight ahead and moved the other, up to 3.8 m.
#[test]
fn line_up_keeps_a_symmetric_pair_symmetric() {
    let center = (Vec3::ZERO, 0.0);
    let two = [(0u32, 3440u32, 1440u32), (1, 3440, 1440)];
    for at in [0.0f32, 0.5, -2.84] {
        let plane = Quat::from_rotation_y(-at);
        let start = arrange(Preset::Arc, &two, None, None, None);
        // The left one turned towards the plane's direction, then disturbed.
        let mut a = start[&0];
        a.position = plane * a.position + Vec3::new(0.0, 0.03, 0.0);
        a.rotation = (Quat::from_rotation_z(0.04) * plane * a.rotation).normalize();
        a.position += plane * Vec3::new(-0.02, 0.0, 0.01);
        let placements = HashMap::from([(0, a), (1, mirrored(&a, at))]);
        let out = line_up(
            &placements,
            &two,
            center,
            tilt(Preset::Arc),
            None,
            &[],
            None,
        );
        let back = mirrored(&out[&0], at);
        assert!(
            back.position.distance(out[&1].position) < 1e-3,
            "{at}: {back:?} {:?}",
            out[&1]
        );
        assert!(back.rotation.angle_between(out[&1].rotation) < 1e-3, "{at}");
    }
}

/// One monitor of a row hanging crooked (rolled 6°): "Line up" straightens it about
/// its own centre and leaves the others where they are.
#[test]
fn line_up_straightens_a_crooked_monitor_in_place() {
    let center = (Vec3::ZERO, 0.0);
    let three = [(0u32, 2560u32, 1440u32), (1, 2560, 1440), (2, 2560, 1440)];
    let start = arrange(Preset::Arc, &three, Some(1), None, None);
    let mut crooked = start.clone();
    let p = crooked.get_mut(&2).unwrap();
    p.rotation =
        (Quat::from_axis_angle(p.rotation * Vec3::Z, 6f32.to_radians()) * p.rotation).normalize();
    let out = line_up(
        &crooked,
        &three,
        center,
        tilt(Preset::Arc),
        Some(1),
        &[],
        None,
    );
    for (id, p) in &start {
        assert!(out[id].position.distance(p.position) < 1e-3, "{id} moved");
        assert!(
            out[id].rotation.angle_between(p.rotation) < 1e-3,
            "{id} turned"
        );
    }
}

/// The memo gives what `Desktop::of` gives, and follows a moved monitor.
#[test]
fn the_desktop_memo_follows_the_arrangement() {
    let center = (Vec3::ZERO, 0.0);
    let three = [(0, 2560, 1440), (1, 1440, 2560), (2, 2560, 1440)];
    let memo = DesktopMemo::default();
    let mut p = arrange(Preset::Arc, &three, Some(1), None, None);
    let of = |p: &HashMap<u32, Placement>| {
        Desktop::of(p, &three, center, tilt(Preset::Arc), Some(1), None)
    };
    let get =
        |p: &HashMap<u32, Placement>| memo.get(p, &three, center, tilt(Preset::Arc), Some(1), None);
    assert_eq!(get(&p), of(&p));
    assert_eq!(get(&p), of(&p));
    p.get_mut(&2).unwrap().position += Vec3::new(0.0, 0.3, 0.0);
    assert_eq!(get(&p), of(&p));
}

/// `desk` as the app finds it after a restart: written to its file and read back.
fn reread(desk: &Desk) -> Desk {
    let mut text = String::new();
    desk.write_text(&mut text);
    reading(&text)
}

/// `text` read as `layout.txt`, with nothing passed over.
fn reading(text: &str) -> Desk {
    let (desk, skipped) = Desk::parse(text).unwrap();
    assert!(skipped.is_empty(), "{skipped:?} in {text:?}");
    desk
}

/// `profiles` written to their file and read back.
fn reread_profiles(profiles: &crate::profiles::Slots) -> crate::profiles::Slots {
    let mut text = String::new();
    crate::profiles::write_text(&mut text, profiles);
    let (back, skipped) = crate::profiles::parse(&text).unwrap();
    assert!(skipped.is_empty(), "{skipped:?}");
    back
}

/// `m` (number, width, height) as a profile takes the monitors: at 100 %.
fn modes<K: Copy>(m: &[(K, u32, u32)]) -> Vec<(K, prostir_proto::state::Mode)> {
    m.iter()
        .map(|&(k, w, h)| (k, prostir_proto::state::Mode::new(w, h)))
        .collect()
}

/// The cinema view never replaces the desk arrangement: leaving cinema finds the
/// monitors where the user put them.
#[test]
fn cinema_view_keeps_the_desk_arrangement() {
    let mut desk = Desk::default();
    let two = [(0, 2560, 1440), (1, 1920, 1080)];
    let sizes = HashMap::from([(0, (2560, 1440)), (1, (1920, 1080))]);
    let center = (Vec3::ZERO, 0.0);
    let sizes = keyed(sizes);
    let mut arranged = arrange(Preset::Arc, &two, Some(0), None, None);
    arranged.get_mut(&1).unwrap().position += Vec3::new(0.1, 0.3, 0.0);
    assert!(desk.keep(
        keyed(arranged.clone()),
        &sizes,
        (center, RADIUS, 0.0),
        false
    ));
    assert!(!desk.keep(
        keyed(cinema(&two, None)),
        &sizes,
        (center, RADIUS, 0.0),
        true
    ));
    for kept in [desk.arrangement(), reread(&desk).arrangement()] {
        assert_eq!(kept.placements.len(), 2);
        for (id, p) in &arranged {
            assert!(kept.placements[&k(*id)].position.distance(p.position) < 1e-5);
        }
    }
}

/// Cinema on, the cinema view moved, cinema off, all in one pass of the loop, with
/// no write of `layout.txt` done yet (they go on a thread of their own): the desk
/// is the one from before cinema, as nothing reads the file back. And a profile
/// saved in cinema takes that desk, not the cinema view.
#[test]
fn cinema_in_one_pass_needs_no_file() {
    let two = [(0, 2560, 1440), (1, 1920, 1080)];
    let sizes = keyed(HashMap::from([(0, (2560, 1440)), (1, (1920, 1080))]));
    let center = (Vec3::ZERO, 0.0);
    // Started with an older desk on disk.
    let mut desk = reread(&{
        let mut was = Desk::default();
        was.keep(
            keyed(arrange(Preset::Wall, &two, Some(0), None, None)),
            &sizes,
            (center, RADIUS, 0.0),
            false,
        );
        was
    });
    let mut at_desk = arrange(Preset::Arc, &two, Some(0), None, None);
    at_desk.get_mut(&0).unwrap().position += Vec3::new(-0.2, 0.1, 0.0);
    // Cinema on: the desk is kept first (`World::set_cinema`).
    assert!(desk.keep(keyed(at_desk.clone()), &sizes, (center, RADIUS, 0.0), false));
    // The cinema view, moved by hand, kept as cinema is not.
    let mut view = keyed(cinema(&two, None));
    for p in view.values_mut() {
        p.position += Vec3::new(0.3, 0.0, -0.4);
    }
    assert!(!desk.keep(view.clone(), &sizes, (center, RADIUS, 0.0), true));
    // Saved as a profile while in cinema.
    let monitors = modes(&[(k(0), 2560, 1440), (k(1), 1920, 1080)]);
    let live = Arrangement {
        placements: view,
        sizes: sizes.clone(),
        center: Some(center),
        radius: Some(RADIUS),
        tilt: 0.0,
    };
    let profile = crate::profiles::Profile::of(
        &monitors,
        Some(k(0)),
        Vec::new(),
        &live,
        Some(desk.arrangement()),
        &[],
    );
    assert_eq!(profile.places.len(), 2);
    // Cinema off: what `World::adopt_desk` takes.
    let back = desk.arrangement();
    for (i, id) in [0, 1].iter().enumerate() {
        assert!(
            back.placements[&k(*id)]
                .position
                .distance(at_desk[id].position)
                < 1e-6
        );
        assert!(profile.places[i].position.distance(at_desk[id].position) < 1e-6);
    }
}

/// The stream sizes and the way the arrangement faces come back as saved; a size
/// without a placement is not kept (the file has no line for it).
#[test]
fn desk_keeps_sizes_and_center() {
    let mut desk = Desk::default();
    let two = [(0, 2560, 1440), (1, 1440, 2560)];
    let arranged = arrange(Preset::Arc, &two, Some(0), None, None);
    let sizes = keyed(HashMap::from([
        (0, (2560, 1440)),
        (1, (1440, 2560)),
        (7, (800, 600)),
    ]));
    let center = (Vec3::new(0.1, 0.0, -0.2), 0.7);
    desk.keep(keyed(arranged), &sizes, (center, RADIUS, 0.0), false);
    let reread = reread(&desk);
    for saved in [desk.arrangement(), reread.arrangement()] {
        assert_eq!(saved.sizes.len(), 2);
        assert_eq!(saved.sizes[&k(1)], (1440, 2560));
        assert_eq!(saved.radius, Some(RADIUS));
        let (eye, yaw) = saved.center.unwrap();
        assert!(eye.distance(center.0) < 1e-6 && (yaw - center.1).abs() < 1e-6);
        assert_eq!(saved.placements.len(), 2);
    }
    // Written in order of the numbers, the same text each time.
    let mut text = String::new();
    desk.write_text(&mut text);
    let mut again = String::new();
    reread.write_text(&mut again);
    assert_eq!(text, again);
    let ids: Vec<&str> = text
        .lines()
        .skip(4)
        .filter_map(|l| l.split(' ').next())
        .collect();
    assert_eq!(ids, [k(0).to_string(), k(1).to_string()]);
}

/// The numbers of a map from the smallest up, each once.
#[test]
fn ascending_numbers() {
    let map = HashMap::from([(9, ()), (0, ()), (4, ()), (u32::MAX, ()), (2, ())]);
    assert_eq!(ascending(&map).collect::<Vec<_>>(), [0, 2, 4, 9, u32::MAX]);
    assert_eq!(ascending(&HashMap::<u32, ()>::new()).count(), 0);
}

/// Where the keyboard window lies comes back as saved.
#[test]
fn keyboard_roundtrip() {
    let place = Placement {
        position: Vec3::new(0.1, 1.2, -0.4),
        rotation: Quat::from_rotation_x(-1.2),
        width: 0.5,
    };
    let mut text = String::new();
    write_keyboard(&mut text, &place);
    let back = parse_keyboard(&text).unwrap().0.unwrap();
    assert!(back.position.distance(place.position) < 1e-6);
    assert!(back.rotation.angle_between(place.rotation) < 1e-4);
    assert!((back.width - place.width).abs() < 1e-6);
    assert_eq!(text.lines().next(), Some(persist::FORMAT));
    assert!(matches!(parse_keyboard(""), Err(LoadError::Format)));
    let (none, skipped) = parse_keyboard("format 2\ngarbage\n").unwrap();
    assert!(none.is_none() && skipped.first == Some(2));
    let (none, skipped) = parse_keyboard("format 2\n").unwrap();
    assert!(none.is_none() && skipped.is_empty());
    // Twice: the first.
    let twice = format!("{text}keyboard 0 0 -1 0 0 0 1 0.9\n");
    let (first, skipped) = parse_keyboard(&twice).unwrap();
    assert!((first.unwrap().width - 0.5).abs() < 1e-6 && skipped.first == Some(3));
    // As 0.1 wrote it.
    assert!(matches!(
        parse_keyboard("0 0.1 1.2 -0.4 -0.56 0 0 0.83 0.5\n"),
        Err(LoadError::Format)
    ));
}

/// What `layout.txt` holds in format 2 comes back. A file of 0.1 (lines of a
/// number and eight more) is not read at all. Within one, what this version does
/// not know or cannot read is passed over and counted, the rest read as without
/// it: a display twice keeps its first place, a line of a newer version, a number
/// without its PC, a center or a radius twice or that is not one, a size of
/// nothing, a width of nothing, a turn of nothing, words after the size.
#[test]
fn desk_format_2() {
    let text = "format 2\ncenter 0 1.6 0 0.25\nradius 1.3\ntilt 0.5\ncinema 0123456789abcdef:12\n\
                0123456789abcdef:3 -0.5 1.5 -0.9 0 0.2 0 0.98 1.38 2560x1440\n\
                0123456789abcdef:12 0.5 1.5 -0.9 0 -0.2 0 0.98 1.03\n";
    let desk = reading(text);
    let saved = desk.arrangement();
    let pc = PcId::parse("0123456789abcdef").unwrap();
    let three = DisplayKey::new(pc, DisplayId(3));
    let twelve = DisplayKey::new(pc, DisplayId(12));
    assert_eq!(saved.placements.len(), 2);
    assert_eq!(saved.sizes.get(&three), Some(&(2560, 1440)));
    assert_eq!(saved.sizes.get(&twelve), None);
    assert!((saved.placements[&twelve].width - 1.03).abs() < 1e-6);
    assert_eq!(saved.radius, Some(1.3));
    assert_eq!(saved.center, Some((Vec3::new(0.0, 1.6, 0.0), 0.25)));
    assert_eq!(desk.cinema(), Some(twelve));
    assert_eq!(saved.tilt, 0.5);
    let mut again = String::new();
    desk.write_text(&mut again);
    let back = reading(&again);
    for (key, p) in &saved.placements {
        let q = back.arrangement().placements[key];
        assert!(q.position.distance(p.position) < 1e-6 && (q.width - p.width).abs() < 1e-6);
        assert!(q.rotation.angle_between(p.rotation) < 1e-4);
    }
    assert!(again.starts_with(
        "format 2\ncenter 0 1.6 0 0.25\nradius 1.3\ntilt 0.5\ncinema 0123456789abcdef:12\n"
    ));
    assert_eq!(back.cinema(), Some(twelve));
    // Another system's line ends, no end to the last line: the same.
    let crlf = text.replace('\n', "\r\n");
    let other = reading(crlf.trim_end());
    assert_eq!(other.arrangement().placements.len(), 2);
    assert_eq!(other.cinema(), Some(twelve));
    // A display gone takes the cinema's pick with it.
    let mut desk = back;
    assert!(desk.forget(&[twelve]));
    assert_eq!(desk.cinema(), None);
    assert!(!desk.forget(&[twelve]));
    assert!(desk.pick_cinema(Some(three)) && !desk.pick_cinema(Some(three)));
    let old = "0 -0.5 -0.08 -0.9 0 0.2 0 0.98 1.38\n1 0.5 -0.08 -0.9 0 -0.2 0 0.98 1.03\n";
    assert!(matches!(Desk::parse(old), Err(LoadError::Format)));
    let place = "-0.5 1.5 -0.9 0 0.2 0 0.98 1.38";
    let first = format!("0123456789abcdef:3 {place}\n");
    for bad in [
        "0123456789abcdef:3 0 0 -1 0 0 0 1 9\n".to_owned(),
        format!("3 {place}\n"),
        "lens 2\n".to_owned(),
        "center 0 1.6 0 0\n".to_owned(),
        "center 0 1.6 0\n".to_owned(),
        "radius 0\n".to_owned(),
        "tilt 9\n".to_owned(),
        "cinema 12\n".to_owned(),
        format!("0123456789abcdef:4 {place} 2560\n"),
        format!("0123456789abcdef:4 {place} 0x0\n"),
        "0123456789abcdef:4 0 0 -1 0 0 0 1 0\n".to_owned(),
        "0123456789abcdef:4 0 0 -1 0 0 0 0 1\n".to_owned(),
        format!("0123456789abcdef:4 {place} 2560x1440 more\n"),
    ] {
        let text = format!("format 2\ncenter 0 1.6 0 0.25\n{first}{bad}");
        let (desk, skipped) = Desk::parse(&text).unwrap();
        assert_eq!((skipped.count, skipped.first), (1, Some(4)), "{bad:?}");
        let saved = desk.arrangement();
        assert_eq!(saved.placements.len(), 1, "{bad:?}");
        assert!(
            (saved.placements[&three].width - 1.38).abs() < 1e-6,
            "{bad:?}"
        );
        assert_eq!(
            saved.center,
            Some((Vec3::new(0.0, 1.6, 0.0), 0.25)),
            "{bad:?}"
        );
    }
    // Nothing kept yet: only the format line.
    let mut empty = String::new();
    Desk::default().write_text(&mut empty);
    assert_eq!(empty, "format 2\n");
    assert!(reading(&empty).arrangement().placements.is_empty());
}

/// Two PCs with a display 3 each: two places, both kept and read back, and each
/// PC hears of its own displays only (a row of the other's alone is no row).
#[test]
fn two_pcs_with_a_display_of_one_number() {
    let (a, b) = (PcId([0xa; 8]), PcId([0xb; 8]));
    let (a3, b3) = (
        DisplayKey::new(a, DisplayId(3)),
        DisplayKey::new(b, DisplayId(3)),
    );
    let p = |x: f32| Placement {
        position: Vec3::new(x, 0.0, -1.2),
        rotation: Quat::IDENTITY,
        width: 0.8,
    };
    let mut desk = Desk::default();
    desk.keep(
        [(a3, p(-0.5)), (b3, p(0.5))],
        &HashMap::new(),
        ((Vec3::ZERO, 0.0), RADIUS, 0.0),
        false,
    );
    let back = reread(&desk);
    assert_eq!(back.arrangement().placements.len(), 2);
    assert!((back.arrangement().placements[&a3].position.x + 0.5).abs() < 1e-6);
    assert!((back.arrangement().placements[&b3].position.x - 0.5).abs() < 1e-6);
    let monitors = [(a3, 2560, 1440), (b3, 2560, 1440)];
    let placements = back.arrangement().placements.clone();
    let mut desktop = Desktop::of(
        &placements,
        &monitors,
        (Vec3::ZERO, 0.0),
        tilt(Preset::Arc),
        Some(a3),
        None,
    );
    assert_eq!(desktop.rows, vec![vec![a3, b3]]);
    let to = |pc: PcId| move |k: DisplayKey| (k.pc == pc).then_some(k.id);
    let for_a = desktop.wire(to(a));
    assert_eq!(for_a.len(), 1);
    assert_eq!(for_a[0].ids().collect::<Vec<_>>(), [DisplayId(3)]);
    // B's display alone in a row: that row is no row of A's.
    desktop.rows = vec![vec![b3], vec![a3]];
    let for_a = desktop.wire(to(a));
    assert_eq!(for_a.len(), 1);
    assert_eq!(for_a[0].ids().collect::<Vec<_>>(), [DisplayId(3)]);
    assert!(desktop.wire(to(PcId([0xc; 8]))).is_empty());
}

/// Whatever the arrangement (places that went wrong, sizes of nothing, a center
/// or a circle that is not one, displays of several PCs), what is kept is read back
/// whole: nothing passed over.
#[test]
fn any_desk_kept_reads_back() {
    let mut rng = Rng(11);
    let float = |rng: &mut Rng| match rng.next() % 12 {
        0 => f32::NAN,
        1 => f32::NEG_INFINITY,
        _ => rng.float(-3.0, 3.0),
    };
    for case in 0..300 {
        let n = rng.next() % 20;
        let mut places = HashMap::new();
        let mut sizes = HashMap::new();
        for _ in 0..n {
            let key = DisplayKey::new(
                PcId([(rng.next() % 3) as u8; 8]),
                DisplayId((rng.next() % 1000) as u32),
            );
            let p = Placement {
                position: Vec3::new(float(&mut rng), float(&mut rng), float(&mut rng)),
                rotation: Quat::from_rotation_y(float(&mut rng)),
                width: float(&mut rng).abs(),
            };
            places.insert(key, p);
            sizes.insert(
                key,
                ((rng.next() % 3000) as u32, (rng.next() % 3000) as u32),
            );
        }
        let mut desk = Desk::default();
        let center = (Vec3::new(float(&mut rng), 1.0, 0.0), float(&mut rng));
        desk.keep(places, &sizes, (center, float(&mut rng), 0.0), false);
        let mut text = String::new();
        desk.write_text(&mut text);
        let (back, skipped) = Desk::parse(&text).unwrap();
        assert!(skipped.is_empty(), "case {case}: {skipped:?}\n{text}");
        assert_eq!(
            back.arrangement().placements.len(),
            desk.arrangement().placements.len(),
            "case {case}"
        );
        assert_eq!(
            back.arrangement().sizes,
            desk.arrangement().sizes,
            "case {case}"
        );
    }
}

/// A monitor that comes back with another resolution keeps the size of a pixel.
#[test]
fn refit_keeps_the_size_of_a_pixel() {
    let landscape = arrange(Preset::Arc, &[(0, 2560, 1440)], Some(0), None, None)[&0];
    let rotated = refit(landscape, (2560, 1440), (1440, 2560));
    let density = |p: &Placement, w: u32| p.width / w as f32;
    assert!((density(&rotated, 1440) - density(&landscape, 2560)).abs() < 1e-9);
    assert_eq!(rotated.position, landscape.position);
    let unchanged = refit(landscape, (2560, 1440), (2560, 1440));
    assert_eq!(unchanged.width, landscape.width);
}

/// An arrangement laid out facing `yaw` reads back as facing `yaw`.
#[test]
fn facing_yaw_of_an_arrangement() {
    let row = arrange(
        Preset::Arc,
        &[(0, 2560, 1440), (1, 2560, 1440), (2, 2560, 1440)],
        Some(1),
        None,
        None,
    );
    let yaw = 0.8f32;
    let turn = Quat::from_rotation_y(yaw);
    let turned: HashMap<u32, Placement> = row
        .iter()
        .map(|(id, p)| {
            (
                *id,
                Placement {
                    position: turn * p.position,
                    rotation: turn * p.rotation,
                    width: p.width,
                },
            )
        })
        .collect();
    let got = facing_yaw(&turned, Vec3::ZERO).unwrap();
    assert!((got - yaw).abs() < 1e-4, "got {got}");
}

/// `p` with every monitor carried from `from` to `to` (see [`carried`]).
fn carried_all(
    p: &HashMap<u32, Placement>,
    from: (Vec3, f32),
    to: (Vec3, f32),
) -> HashMap<u32, Placement> {
    p.iter().map(|(k, q)| (*k, carried(q, from, to))).collect()
}

/// How far the corners of `a` seen from the eye of `ca` are from those of `b` seen
/// from the eye of `cb`, each turned so the first monitor of `m` is straight ahead
/// (metres): 0 when the user sees the one arrangement from the one eye as the other
/// from the other.
fn seen_apart(
    a: &HashMap<u32, Placement>,
    ca: (Vec3, f32),
    b: &HashMap<u32, Placement>,
    cb: (Vec3, f32),
    m: &[(u32, u32, u32)],
) -> f32 {
    let origin = (Vec3::ZERO, 0.0);
    let first = Some(m[0].0);
    let ca = (ca.0, ahead(a, &[], first, ca.0).unwrap());
    let cb = (cb.0, ahead(b, &[], first, cb.0).unwrap());
    m.iter()
        .map(|&(id, w, h)| {
            let (p, q) = (carried(&a[&id], ca, origin), carried(&b[&id], cb, origin));
            let tall = |x: &Placement| x.width * h as f32 / w as f32;
            corners(&p, tall(&p))
                .iter()
                .zip(corners(&q, tall(&q)))
                .map(|(x, y)| x.distance(y))
                .fold(0.0f32, f32::max)
        })
        .fold(0.0f32, f32::max)
}

/// "In front" as the headset does it: the arrangement turned about the eye of
/// `center` so the primary monitor is straight ahead of a head at `head` looking
/// along `yaw`, and carried to it; the new center.
fn in_front(
    p: &HashMap<u32, Placement>,
    m: &[(u32, u32, u32)],
    primary: Option<u32>,
    center: (Vec3, f32),
    head: (Vec3, f32),
) -> (HashMap<u32, Placement>, (Vec3, f32)) {
    let ids: Vec<u32> = m.iter().map(|x| x.0).collect();
    let ahead = ahead(p, &ids, primary, center.0).unwrap_or(center.1);
    (carried_all(p, (center.0, ahead), head), head)
}

/// `id` let go by the magnet over (`up` 1) or under (-1) `under`, its middle `along`
/// metres to the right of `under`'s, a hand's 3 cm off: glued edge to edge.
fn glued(
    p: &mut HashMap<u32, Placement>,
    m: &[(u32, u32, u32)],
    id: u32,
    under: u32,
    up: f32,
    along: f32,
) {
    let tall = |id: u32, width: f32| {
        let &(_, w, h) = m.iter().find(|x| x.0 == id).unwrap();
        width * h as f32 / w as f32
    };
    let &(_, w, _) = m.iter().find(|x| x.0 == id).unwrap();
    let width = w as f32 * METRES_PER_PIXEL;
    let n = p[&under];
    let lift = (tall(under, n.width) + tall(id, width)) / 2.0 + 0.03;
    let at = n.position + n.rotation * Vec3::new(along, up * lift, 0.0);
    p.insert(
        id,
        Placement {
            position: at,
            rotation: facing(at, Vec3::new(0.0, at.y, 0.0)),
            width,
        },
    );
    let s = stick(p, m, Vec3::ZERO, id, &mut StickScratch::default()).expect("the magnet takes it");
    assert!(
        !s.across,
        "{id} glued over or under {}, not beside",
        s.neighbour
    );
    p.insert(id, s.placement);
}

/// The desktops of `a` seen from `ca` and of `b` seen from `cb` are the same: the
/// same rows, and every monitor within a few pixels of the same spot (the eye carried
/// along leaves the last bits of the floats).
fn same_desktop(
    a: &HashMap<u32, Placement>,
    ca: (Vec3, f32),
    b: &HashMap<u32, Placement>,
    cb: (Vec3, f32),
    m: &[(u32, u32, u32)],
    primary: Option<u32>,
    what: &str,
) -> Desktop<u32> {
    let (da, db) = (
        Desktop::of(a, m, ca, tilt(Preset::Arc), primary, None),
        Desktop::of(b, m, cb, tilt(Preset::Arc), primary, None),
    );
    assert_eq!(da.rows, db.rows, "{what}: rows");
    for s in &da.plan {
        let t = db.plan.iter().find(|t| t.id == s.id).unwrap();
        assert!(
            (s.x - t.x).abs() <= 2 && (s.y - t.y).abs() <= 2,
            "{what}: {} at {},{} instead of {},{}",
            s.id,
            s.x,
            s.y,
            t.x,
            t.y
        );
    }
    da
}

/// Where each monitor stands.
type Placements = HashMap<u32, Placement>;

/// The owner's monitors of 2026-11-21: 1440x2560 | 2560x1440 primary | 2560x1440 |
/// 1440x2560 by the preset, and the 3840x2160 let go by the magnet over the seam of
/// the two landscape ones.
fn owners_stack() -> (Vec<(u32, u32, u32)>, Placements) {
    let owner = vec![
        (2, 1440, 2560),
        (1, 2560, 1440),
        (3, 2560, 1440),
        (4, 1440, 2560),
        (6, 3840, 2160),
    ];
    let mut stack = arrange(Preset::Arc, &owner[..4], Some(1), None, None);
    let seam = stack[&1].width / 2.0;
    glued(&mut stack, &owner, 6, 1, 1.0, seam);
    (owner, stack)
}

/// The profile of `placements` seen from `center` with the rows the PC has
/// (`Scene::current_profile`), written to its file and read back.
fn saved_profile(
    m: &[(u32, u32, u32)],
    primary: Option<u32>,
    placements: &HashMap<u32, Placement>,
    center: (Vec3, f32),
    rows: &[Vec<u32>],
) -> crate::profiles::Profile {
    let live = Arrangement {
        placements: placements.clone(),
        sizes: HashMap::new(),
        center: Some(center),
        radius: Some(RADIUS),
        tilt: 0.0,
    };
    let profile = crate::profiles::Profile::of(&modes(m), primary, Vec::new(), &live, None, rows);
    let mut all = crate::profiles::empty();
    all[0] = Some(profile);
    let [back, ..] = reread_profiles(&all);
    back.unwrap()
}

/// `profile` switched to with the user's center at `now`, the PC giving its
/// monitors the numbers they had (as it does for monitors of the same size): the
/// placements and the rows the headset then has (`Scene::profile_made`).
fn switched_to(
    profile: &crate::profiles::Profile,
    m: &[(u32, u32, u32)],
    now: (Vec3, f32),
) -> (HashMap<u32, Placement>, Option<Desktop<u32>>) {
    let made: Vec<(u32, Placement)> = m
        .iter()
        .map(|x| x.0)
        .zip(profile.places.iter().copied())
        .collect();
    let (placed, rows) = crate::profiles::standing(&made, profile.frame, &profile.rows, now);
    (placed.into_iter().collect(), Desktop::history(rows))
}

/// 2026-11-21, the owner (dev build 74dfe1d): after switching to a profile, "Line
/// up" or "In front" sent monitors glued one over another flying, while monitors
/// glued side by side held. A profile kept its places in the room but not the eye
/// they were laid out around: once the user had turned the arrangement in front
/// of them elsewhere (or the headset's room was set anew), everything worked out as
/// seen from the eye (the rows the PC gets, "Line up", "In front") saw the profile
/// from another place. Rows side by side close up whatever the eye, rows one over
/// another are kept apart as seen and so came apart.
///
/// What this covers: a profile switched to is seen from the user's eye as it was
/// from the eye it was saved around, so what lining up and "In front" do to it after
/// the switch is what they did to it before (in either order), and the PC gets the
/// desktop it had. For stacks, rows side by side, portraits among landscapes and a
/// 3840x2160 over two monitors, each lined up once before it is saved. What the
/// first "Line up" does to a monitor the magnet glued is not covered here (see
/// `report_first_line_up_of_a_3840_over_two`).
#[test]
fn a_profile_switched_to_lines_up_and_turns_as_it_did_before() {
    // Where the user sat when saving, and later (turned in front elsewhere, or
    // the headset's room set anew): 35 cm aside, lower, further back, turned 75°.
    let saved_at = (Vec3::new(0.2, 1.15, -0.1), 0.4);
    let later = (Vec3::new(0.55, 1.03, 0.15), 0.4 + 1.3);
    let head = |c: (Vec3, f32)| (c.0 + Vec3::new(0.04, 0.02, -0.03), c.1 - 0.6);
    let (owner, stack) = owners_stack();
    // Side by side only.
    let row = [(0, 2560, 1440), (1, 1440, 2560), (2, 2560, 1440)];
    let side = arrange(Preset::Arc, &row, Some(1), None, None);
    // Mixed: two landscape side by side, a 1920x1080 glued over the left one and a
    // 2560x1440 under the right one.
    let mixed_m = [
        (0, 2560, 1440),
        (1, 2560, 1440),
        (5, 1920, 1080),
        (7, 2560, 1440),
    ];
    let mut mixed = arrange(Preset::Arc, &mixed_m[..2], Some(0), None, None);
    glued(&mut mixed, &mixed_m, 5, 0, 1.0, 0.0);
    glued(&mut mixed, &mixed_m, 7, 1, -1.0, -0.1);
    // Two rows by the preset (six do not fit one).
    let six = [
        (0, 2560, 1440),
        (1, 2560, 1440),
        (2, 1440, 2560),
        (3, 2560, 1440),
        (4, 2560, 1440),
        (5, 3840, 2160),
    ];
    let two_rows = arrange(Preset::Arc, &six, Some(2), None, None);
    let scenes = [
        ("a 3840x2160 over two", &owner[..], 1, stack),
        ("side by side", &row[..], 1, side),
        ("over and under", &mixed_m[..], 0, mixed),
        ("two rows of a preset", &six[..], 2, two_rows),
    ];
    for (name, m, primary, by_hand) in scenes {
        let primary = Some(primary);
        let origin = (Vec3::ZERO, 0.0);
        // Lined up once, as the user keeps it, where they sat.
        let kept = carried_all(
            &line_up(&by_hand, m, origin, tilt(Preset::Arc), primary, &[], None),
            origin,
            saved_at,
        );
        let desktop = same_desktop(&kept, saved_at, &kept, saved_at, m, primary, name);
        if name == "a 3840x2160 over two" {
            // Over the two, touching both on the desktop.
            assert_eq!(desktop.rows, vec![vec![6], vec![2, 1, 3, 4]]);
            let spot = |id| desktop.plan.iter().find(|s| s.id == id).copied().unwrap();
            for below in [1, 3] {
                assert_eq!(
                    spot(6).crossing(&spot(below)),
                    Some(prostir_layout::space::Side::Below),
                    "{name}: 6 over {below}"
                );
            }
        }
        let check = |p: &HashMap<u32, Placement>, at: (Vec3, f32), what: &str| {
            let off = seen_apart(p, at, &kept, saved_at, m);
            assert!(off < 2e-3, "{name}, {what}: {off} m from how it was");
            same_desktop(
                p,
                at,
                &kept,
                saved_at,
                m,
                primary,
                &format!("{name}, {what}"),
            );
        };
        let profile = saved_profile(m, primary, &kept, saved_at, &desktop.rows);
        for now in [saved_at, later] {
            let (back, sent) = switched_to(&profile, m, now);
            assert_eq!(
                sent.as_ref().map(|d| &d.rows),
                Some(&desktop.rows),
                "{name}: its rows"
            );
            check(&back, now, "switched to");
            // Then "Line up".
            let rows = Desktop::of(&back, m, now, tilt(Preset::Arc), primary, sent.as_ref()).rows;
            let lined = line_up(&back, m, now, tilt(Preset::Arc), primary, &rows, None);
            check(&lined, now, "switched to, lined up");
            // Then "In front", and "Line up" after it.
            let (front, at) = in_front(&back, m, primary, now, head(now));
            check(&front, at, "switched to, in front");
            let rows = Desktop::of(&front, m, at, tilt(Preset::Arc), primary, sent.as_ref()).rows;
            let lined = line_up(&front, m, at, tilt(Preset::Arc), primary, &rows, None);
            check(&lined, at, "switched to, in front, lined up");
            // The other order: "In front" and "Line up" first, saved, and back.
            let (front, at) = in_front(&kept, m, primary, saved_at, head(now));
            let rows = Desktop::of(&front, m, at, tilt(Preset::Arc), primary, None).rows;
            let lined = line_up(&front, m, at, tilt(Preset::Arc), primary, &rows, None);
            let again = saved_profile(m, primary, &lined, at, &rows);
            let (back, _) = switched_to(&again, m, later);
            check(&back, later, "in front, lined up, saved, switched to");
        }
    }
}

/// In cinema the profile keeps the desk arrangement saved before it (with its
/// center and circle), not the cinema view; otherwise the arrangement as it stands.
#[test]
fn in_cinema_a_profile_keeps_the_desk_saved_before_it() {
    let three = [(0, 2560, 1440), (1, 2560, 1440), (2, 2560, 1440)];
    let at_desk = arrange(Preset::Arc, &three, Some(1), None, None);
    let desk_center = (Vec3::new(0.1, 1.2, 0.3), 0.5);
    let desk = Arrangement {
        placements: at_desk.clone(),
        sizes: HashMap::new(),
        center: Some(desk_center),
        radius: Some(1.3),
        tilt: 0.0,
    };
    let view = cinema(&three, None);
    let live = |placements: &HashMap<u32, Placement>| Arrangement {
        placements: placements.clone(),
        sizes: HashMap::new(),
        center: Some((Vec3::ZERO, 0.0)),
        radius: Some(RADIUS),
        tilt: 0.0,
    };
    let rows = vec![vec![0, 1, 2]];
    let m = modes(&three);
    let of = |live: &Arrangement<u32>, desk: Option<&Arrangement<u32>>| {
        crate::profiles::Profile::of(&m, Some(1), Vec::new(), live, desk, &rows)
    };
    let profile = of(&live(&view), Some(&desk));
    assert_eq!(profile.places.len(), 3);
    let frame = Frame::upright(desk_center.0, desk_center.1);
    for (x, p) in three.iter().zip(&profile.places) {
        let saved = frame.unplace(at_desk[&x.0]);
        assert!(
            p.position.distance(saved.position) < 1e-5
                && p.rotation.abs_diff_eq(saved.rotation, 1e-5),
            "{}",
            x.0
        );
    }
    assert_eq!(profile.frame.map(|f| f.radius), Some(1.3));
    assert_eq!(profile.rows, vec![vec![0, 1, 2]]);
    // Not in cinema: as it stands.
    let profile = of(&live(&at_desk), None);
    assert_eq!(profile.frame.map(|f| f.radius), Some(RADIUS));
    assert!(profile.places[1].position.distance(at_desk[&1].position) < 1e-5);
    // Not every monitor placed: no places, no rows.
    let profile = of(&live(&view), None);
    assert!(profile.places.is_empty() && profile.rows.is_empty());
}

/// A profile laid out lying down keeps its frame's tilt: switched to elsewhere,
/// with the preset "around me" chosen meanwhile, the PC gets the desktop it had
/// when the profile was saved, and "Line up" does to it what it did then, both
/// seen from its tilted frame (the arrangement's tilt, not the preset's).
#[test]
fn a_profile_laid_out_lying_down_is_seen_lying_down() {
    let m = [
        (0, 2560, 1440),
        (1, 2560, 1440),
        (2, 1920, 1080),
        (3, 1440, 2560),
    ];
    let saved_at = (Vec3::new(0.1, 1.1, 0.2), 0.3);
    let later = (Vec3::new(-0.4, 0.9, 0.5), -1.2);
    let lying = tilt(Preset::Overhead);
    let mut kept = carried_all(
        &arrange(Preset::Overhead, &m, Some(1), None, None),
        (Vec3::ZERO, 0.0),
        saved_at,
    );
    kept.get_mut(&2).unwrap().position += Vec3::new(0.05, 0.08, 0.0);
    let then = Desktop::of(&kept, &m, saved_at, lying, Some(1), None);
    let lined_then = line_up(&kept, &m, saved_at, lying, Some(1), &then.rows, None);
    let live = Arrangement {
        placements: kept.clone(),
        sizes: HashMap::new(),
        center: Some(saved_at),
        radius: Some(RADIUS),
        tilt: lying,
    };
    let profile =
        crate::profiles::Profile::of(&modes(&m), Some(1), Vec::new(), &live, None, &then.rows);
    let [profile, ..] = reread_profiles(&[Some(profile), None, None]);
    let profile = profile.unwrap();
    let saved = profile.frame.unwrap();
    assert_eq!(saved.tilt, lying);
    // Switched to elsewhere: the tilt the scene takes is the profile's.
    let (back, sent) = switched_to(&profile, &m, later);
    let now = Desktop::of(&back, &m, later, saved.tilt, Some(1), sent.as_ref());
    assert_eq!(now.rows, then.rows);
    for s in &then.plan {
        let t = now.plan.iter().find(|t| t.id == s.id).unwrap();
        assert!(
            (s.x - t.x).abs() <= 2 && (s.y - t.y).abs() <= 2,
            "{} moved",
            s.id
        );
    }
    let lined_now = line_up(&back, &m, later, saved.tilt, Some(1), &now.rows, None);
    let seen = |p: &Placement, at: (Vec3, f32)| frame(at, lying).unplace(*p).position;
    for (id, p) in &lined_then {
        let off = seen(&lined_now[id], later).distance(seen(p, saved_at));
        assert!(off < 2e-3, "{id}: {off}");
    }
    // Seen upright (the preset's frame), the same arrangement makes another desktop:
    // the tilt is what keeps it.
    let upright = Desktop::of(&back, &m, later, 0.0, Some(1), None);
    assert_ne!(upright.plan, now.plan);
}

/// The rows of a profile switched to are its own. The PC gives a monitor of the same
/// size the number it had in the profile before, so the rows sent then would keep
/// a pair of neighbours of that arrangement a pair in this one: the 3840x2160 over
/// the landscape ones, its edge beside the top of the portrait on the right, taken
/// into the portrait's row. With the rows it was saved with (or, saved without
/// them, none) it is over the others, as it was.
#[test]
fn a_profile_switched_to_keeps_its_own_rows() {
    let center = (Vec3::ZERO, 0.0);
    let owner = [
        (2, 1440, 2560),
        (1, 2560, 1440),
        (3, 2560, 1440),
        (4, 1440, 2560),
        (6, 3840, 2160),
    ];
    let mut p = arrange(Preset::Arc, &owner[..4], Some(1), None, None);
    // Over 3, its right edge where 3's is, its bottom on 3's top.
    let under = p[&3];
    let (w, h) = (
        3840.0 * METRES_PER_PIXEL,
        3840.0 * METRES_PER_PIXEL * 2160.0 / 3840.0,
    );
    let at = under.position
        + under.rotation
            * Vec3::new(
                (under.width - w) / 2.0,
                (under.width * 1440.0 / 2560.0 + h) / 2.0 + 0.002,
                0.0,
            );
    p.insert(
        6,
        Placement {
            position: at,
            rotation: under.rotation,
            width: w,
        },
    );
    let fresh = Desktop::of(&p, &owner, center, tilt(Preset::Arc), Some(1), None);
    assert_eq!(fresh.rows, vec![vec![6], vec![2, 1, 3, 4]]);
    // The rows sent for the profile before, where 6 was the portrait's neighbour.
    let before = Desktop::history(vec![vec![2, 1, 3, 6, 4]]);
    let stale = Desktop::of(
        &p,
        &owner,
        center,
        tilt(Preset::Arc),
        Some(1),
        before.as_ref(),
    );
    assert_ne!(stale.rows, fresh.rows, "the history of another arrangement");
    // Saved with its rows, switched to: its rows.
    let profile = saved_profile(&owner, Some(1), &p, center, &fresh.rows);
    let (back, sent) = switched_to(&profile, &owner, center);
    let own = Desktop::of(
        &back,
        &owner,
        center,
        tilt(Preset::Arc),
        Some(1),
        sent.as_ref(),
    );
    assert_eq!(own.rows, fresh.rows);
    assert_eq!(own.plan, fresh.plan, "the plan made afresh");
    // Saved without rows: they start afresh.
    let mut without = profile;
    without.rows.clear();
    let (back, sent) = switched_to(&without, &owner, center);
    assert!(sent.is_none());
    assert_eq!(
        Desktop::of(&back, &owner, center, tilt(Preset::Arc), Some(1), None).rows,
        fresh.rows
    );
}

/// The gaps as seen between the owner's 3840x2160 (6) and each of the two landscape
/// monitors under it (1, 3), in the middle of the edge each shares with it, as a
/// share of that one's height (negative where they overlap).
fn seams_of_6(p: &Placements, owner: &[(u32, u32, u32)]) -> Vec<f64> {
    use prostir_layout::desk::View;
    let all = placed(p, owner);
    let room = Room::new(&frame((Vec3::ZERO, 0.0), tilt(Preset::Arc)), &all);
    [1, 3]
        .iter()
        .map(|&below| {
            room.relate(below, 6, prostir_layout::space::Side::Above)
                .filter(|r| r.shared > 0.0)
                .expect("6 over it")
                .apart
        })
        .collect()
}

/// 2026-11-21, the owner's decision: a monitor glued by the magnet over or under a
/// row stays glued when "Line up" straightens it. The owner's 3840x2160 over the
/// two landscape monitors of an arc, let go as the hand leaves it (turned to face the
/// axis) and stood upright on their tops, each time glued by the magnet: the first
/// "Line up" leaves the middle of the wider seam closed and the other overlapping a
/// little (before, let go by hand, it lifted it 13 cm and opened both by 3.8 and 5 %
/// of their height), the ends of the flat monitor overlapping the two a little as seen, and
/// a second changes nothing. Stood upright on their tops without the magnet it is not
/// glued, and is pushed clear of them as before, a gap in the middle of both seams.
#[test]
fn line_up_keeps_a_monitor_glued_over_two_glued() {
    let center = (Vec3::ZERO, 0.0);
    let (owner, by_hand) = owners_stack();
    let row = arrange(Preset::Arc, &owner[..4], Some(1), None, None);
    let mut upright = row.clone();
    {
        let (one, three) = (row[&1], row[&3]);
        let shelf = one.position.y + one.width * 1440.0 / 2560.0 / 2.0;
        let mid = (one.position + three.position) / 2.0;
        let h = 3840.0 * METRES_PER_PIXEL * 2160.0 / 3840.0;
        let at = Vec3::new(mid.x, shelf + h / 2.0, mid.z);
        upright.insert(
            6,
            Placement {
                position: at,
                rotation: facing(at, Vec3::new(0.0, at.y, 0.0)),
                width: 3840.0 * METRES_PER_PIXEL,
            },
        );
    }
    let mut glued_upright = upright.clone();
    let s = stick(
        &glued_upright,
        &owner,
        Vec3::ZERO,
        6,
        &mut StickScratch::default(),
    )
    .expect("sticks");
    assert!(!s.across);
    glued_upright.insert(6, s.placement);
    let lined = |p: &Placements| {
        let rows = Desktop::of(p, &owner, center, tilt(Preset::Arc), Some(1), None).rows;
        assert_eq!(rows, vec![vec![6], vec![2, 1, 3, 4]]);
        line_up(p, &owner, center, tilt(Preset::Arc), Some(1), &rows, None)
    };
    for (name, p) in [("let go by hand", by_hand), ("upright", glued_upright)] {
        let once = lined(&p);
        let seams = seams_of_6(&once, &owner);
        let widest = seams.iter().copied().fold(f64::MIN, f64::max);
        assert!(widest.abs() < 1e-3, "{name}: seams {seams:?}");
        assert!(seams.iter().all(|s| *s > -0.03), "{name}: seams {seams:?}");
        let rise = once[&6].position.y - p[&6].position.y;
        // Only as far as closing the seam takes it (the magnet left it over the right
        // one, as seen): it was 13 cm, with the seams open.
        assert!(rise.abs() < 0.1, "{name}: rose {rise} m");
        for id in [2, 1, 3, 4] {
            assert!(once[&id].position.distance(p[&id].position) < 1e-4);
        }
        let twice = lined(&once);
        assert!(
            twice[&6].position.distance(once[&6].position) < 1e-4,
            "{name}"
        );
        // Its ends overlap the two as seen, by no more than OVERLAP_MAX.
        let all = placed(&once, &owner);
        let of = |id: u32| *all.iter().find(|m| m.id == id).unwrap();
        let deepest = [1, 3]
            .iter()
            .flat_map(|&below| {
                [
                    prostir_layout::view::reach_pair(
                        &of(below),
                        &of(6),
                        prostir_layout::space::Side::Above,
                        17,
                    ),
                    prostir_layout::view::reach_pair(
                        &of(6),
                        &of(below),
                        prostir_layout::space::Side::Below,
                        17,
                    ),
                ]
            })
            .flatten()
            .fold(0.0f64, f64::max);
        assert!(
            deepest <= f64::from(tidy::OVERLAP_MAX),
            "{name}: overlaps {deepest}"
        );
        eprintln!(
            "{name}: rose {rise:.3} m, seams {seams:.4?}, overlapping {:.1} % at most",
            deepest * 100.0
        );
    }
    // Not glued: pushed clear as before, covering neither.
    let once = lined(&upright);
    let corners = |p: &Placements, id: u32| {
        let &(_, w, h) = owner.iter().find(|m| m.0 == id).unwrap();
        let q = p[&id];
        Placement {
            position: q.position,
            rotation: q.rotation,
            width: q.width,
        }
        .corners(q.width * h as f32 / w as f32)
    };
    for below in [1, 3] {
        assert!(!prostir_layout::space::covers(
            &corners(&once, below),
            &corners(&once, 6),
            0.001
        ));
    }
    assert!(
        once[&6].position.y > upright[&6].position.y,
        "pushed up clear"
    );
    let seams = seams_of_6(&once, &owner);
    assert!(
        seams.iter().all(|s| *s > 0.0),
        "a gap in the middle: {seams:?}"
    );
}

/// The farthest any corner of `a` is from the same corner in `b` (metres).
fn farthest_corner(a: &Placements, b: &Placements, m: &[(u32, u32, u32)]) -> f32 {
    m.iter()
        .map(|&(id, w, h)| {
            let (p, q) = (a[&id], b[&id]);
            corners(&p, p.width * h as f32 / w as f32)
                .iter()
                .zip(corners(&q, q.width * h as f32 / w as f32))
                .map(|(x, y)| x.distance(y))
                .fold(0.0f32, f32::max)
        })
        .fold(0.0f32, f32::max)
}

/// The draw order keeps two monitors at about one distance from the head in the
/// order they had (a monitor glued over a row and that row): the head moving so
/// their distances cross back and forth within DRAW_HOLD never swaps them, so where
/// they overlap never flickers; one clearly nearer goes last; one held goes last of
/// all, and new ones go by distance.
/// New monitors at one distance are drawn in the order of their numbers, however
/// they come; an arrangement faces the same way to the bit, whatever the order of its
/// map.
#[test]
fn the_same_arrangement_gives_the_same_order() {
    for listed in [[3, 1, 2], [2, 3, 1], [1, 2, 3]] {
        let mut order: Vec<(f32, u32)> = listed.iter().map(|&k| (1.5, k)).collect();
        draw_order(&mut order, &[]);
        assert_eq!(order.iter().map(|o| o.1).collect::<Vec<_>>(), [1, 2, 3]);
    }
    let three = [(0, 2560, 1440), (1, 1440, 2560), (2, 3440, 1440)];
    let laid = arrange(Preset::Arc, &three, Some(1), None, None);
    let yaw = facing_yaw(&laid, Vec3::new(0.1, 0.0, 0.2)).unwrap();
    for _ in 0..20 {
        let again: HashMap<u32, Placement> = laid.iter().map(|(k, p)| (*k, *p)).collect();
        let other = facing_yaw(&again, Vec3::new(0.1, 0.0, 0.2)).unwrap();
        assert_eq!(other.to_bits(), yaw.to_bits());
    }
}

#[test]
fn the_draw_order_holds_for_monitors_at_one_distance() {
    let mut before: Vec<u32> = Vec::new();
    let mut order: Vec<(f32, u32)> = Vec::new();
    let mut draw = |d: &[(f32, u32)], before: &mut Vec<u32>| {
        order.clear();
        order.extend_from_slice(d);
        draw_order(&mut order, before);
        before.clear();
        before.extend(order.iter().map(|o| o.1));
        before.clone()
    };
    // New: far to near.
    assert_eq!(
        draw(&[(0.8, 6), (1.2, 1), (1.0, 3)], &mut before),
        vec![1, 3, 6]
    );
    // 6 and 3 crossing by up to 4 cm each way, frame after frame: no swap.
    for f in 0..200 {
        let wobble = if f % 2 == 0 { 0.02 } else { -0.02 };
        let got = draw(
            &[(1.0 + wobble, 6), (1.2, 1), (1.0 - wobble, 3)],
            &mut before,
        );
        assert_eq!(got, vec![1, 3, 6], "frame {f}");
    }
    // 3 clearly nearer: it goes last.
    assert_eq!(
        draw(&[(1.0, 6), (1.2, 1), (0.9, 3)], &mut before),
        vec![1, 6, 3]
    );
    // Held in a hand: last of all.
    assert_eq!(
        draw(&[(1.0, 6), (-1.0, 1), (0.9, 3)], &mut before),
        vec![6, 3, 1]
    );
    // A new one far away: first.
    assert_eq!(
        draw(&[(1.0, 6), (1.2, 1), (0.9, 3), (2.0, 9)], &mut before),
        vec![9, 1, 6, 3]
    );
}

/// Follow mode carries the arrangement after the head a little every frame: after
/// a long while of it (60 000 frames, about 14 minutes at 72 Hz, turning and moving
/// back and forth) the owner's 3840x2160 glued over the two is still seen glued, and
/// "Line up" moves nothing, the last bits of the floats notwithstanding.
#[test]
fn a_long_follow_leaves_a_glued_seam_glued() {
    let origin = (Vec3::ZERO, 0.0);
    let (owner, by_hand) = owners_stack();
    let rows = Desktop::of(&by_hand, &owner, origin, tilt(Preset::Arc), Some(1), None).rows;
    let kept = line_up(
        &by_hand,
        &owner,
        origin,
        tilt(Preset::Arc),
        Some(1),
        &rows,
        None,
    );
    let (mut p, mut center) = (kept.clone(), origin);
    let mut rng = Rng(0xf011);
    for _ in 0..60_000 {
        // As `Scene::follow_head` steps: a turn and a shift of a frame's easing.
        let turn = rng.float(-0.01, 0.01);
        let shift = Vec3::new(rng.float(-0.003, 0.003), 0.0, rng.float(-0.003, 0.003));
        let next = (center.0 + shift, center.1 + turn);
        for q in p.values_mut() {
            *q = carried(q, (center.0, 0.0), (next.0, turn));
        }
        center = next;
    }
    assert!(
        seen_apart(&p, center, &kept, origin, &owner) < 1e-3,
        "followed as one piece"
    );
    let lined = line_up(&p, &owner, center, tilt(Preset::Arc), Some(1), &rows, None);
    let moved = farthest_corner(&p, &lined, &owner);
    assert!(moved < 1e-4, "Line up after following moves {moved} m");
}
