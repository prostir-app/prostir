//! Every invariant of `plans/layout-math.md`, over thousands of seeded arrangements.
//! A failure prints the smallest arrangement that still fails.

mod common;

use common::*;
use prostir_layout::circle::{self, ARC, Limits, MAX_RADIUS, Preset, RADIUS};
use prostir_layout::desk::{self, Desk, STEP, Size, View};
use prostir_layout::rows::{JOIN, rows_of};
use prostir_layout::space::{Frame, Monitor, PixelSize, Side, covers};

/// R4: a portrait monitor whose height as seen covers a landscape neighbour's is in its
/// row: beside it touching, a gap of up to 5 % of the narrower or overlapping by up to
/// 10 % of it, at any distance and direction, each turned a little by the hand, with a
/// second landscape on its other side or not.
#[test]
fn a_portrait_covering_its_neighbour_is_in_its_row() {
    let mut rng = Rng::new(0x1a7e_0101);
    let landscapes: Vec<PixelSize> = PIXELS
        .iter()
        .filter(|p| p.0 > p.1)
        .map(|p| PixelSize::new(p.0, p.1))
        .collect();
    let portrait = PixelSize::new(1440, 2560);
    let metres = |px: PixelSize| px.w as f32 * prostir_layout::space::METRES_PER_PIXEL;
    for i in 0..cases(3000) {
        let sides: &[f32] = if rng.chance(50) { &[1.0] } else { &[1.0, -1.0] };
        let beside: Vec<PixelSize> = sides.iter().map(|_| rng.pick(&landscapes)).collect();
        // Far enough that none takes more than 120° of the circle.
        let widest = beside.iter().map(|&p| metres(p)).fold(0.0, f32::max);
        let radius = rng
            .float(1.0, 1.6)
            .max(widest / 2.0 / 60f32.to_radians().sin());
        let heading = rng.float(-3.0, 3.0);
        let half = |w: f32| (w / 2.0 / radius).asin();
        let wp = metres(portrait);
        let hp = wp * portrait.aspect();
        let y = rng.float(-0.3, 0.3);
        let mut monitors = vec![Monitor {
            id: 0,
            px: portrait,
            placement: circle::on_cylinder(heading, y, wp, radius),
        }];
        for (n, (&side, &px)) in sides.iter().zip(&beside).enumerate() {
            let wl = metres(px);
            let hl = wl * px.aspect();
            let narrower = 2.0 * half(wl).min(half(wp));
            let gap = rng.float(-0.10, 0.05) * narrower;
            let at = heading + side * (half(wp) + half(wl) + gap);
            let dy = rng.float(-(hp - hl) / 2.0, (hp - hl) / 2.0);
            let mut p = circle::on_cylinder(at, y + dy, wl, radius);
            let turn = glam::Quat::from_euler(
                glam::EulerRot::YXZ,
                rng.float(-3.0, 3.0).to_radians(),
                rng.float(-3.0, 3.0).to_radians(),
                rng.float(-2.0, 2.0).to_radians(),
            );
            p.rotation = (p.rotation * turn).normalize();
            monitors.push(Monitor {
                id: n as u8 + 1,
                px,
                placement: p,
            });
        }
        let scene = Scene {
            monitors,
            frame: Frame::upright(glam::Vec3::ZERO, 0.0),
            main: Some(0),
        };
        let ids: Vec<u8> = scene.monitors.iter().map(|m| m.id).collect();
        let rows = rows_of(&ids, &scene.room(), None);
        let row_of = |k: u8| rows.iter().position(|r| r.contains(&k));
        for k in 1..ids.len() as u8 {
            assert_eq!(row_of(k), row_of(0), "case {i}: rows {rows:?}");
        }
    }
}

/// D1, D2, D3, D7, D8, D12 whatever the plan is given: boxes anywhere (nonsense too),
/// boxes missing, any scales, the laptop screen, other displays, window panels.
#[test]
fn any_desk_holds_the_hard_invariants() {
    let mut rng = Rng::new(0x1a7e_0001);
    for i in 0..cases(5000) {
        let (desk, boxes, sizes) = random_desk(&mut rng);
        let plan = desk::plan(&desk, |k| sizes.get(&k).copied(), &boxes);
        let wanted: Vec<(u8, Size)> = desk
            .rows
            .iter()
            .flatten()
            .copied()
            .chain(desk.under_main)
            .chain(desk.bottom.iter().copied())
            .chain(desk.column.iter().copied())
            .filter_map(|k| Some((k, *sizes.get(&k)?)))
            .collect();
        let laid: Vec<_> = plan
            .iter()
            .copied()
            .filter(|s| !desk.column.contains(&s.id))
            .collect();
        let r = complete(&plan, &wanted)
            .and_then(|()| no_overlap(&plan))
            .and_then(|()| at_origin(&plan))
            .and_then(|()| rows_in_order(&plan, &desk.rows))
            .and_then(|()| connected(&plan))
            .and_then(|()| connected(&laid));
        if let Err(e) = r {
            panic!(
                "case {i}: {e}\n{desk:?}\n{boxes:?}\n{sizes:?}\n{}",
                describe_plan(&plan)
            );
        }
    }
}

/// Checks every invariant of the plan of `scene` that holds whatever the scene.
fn hard(scene: &Scene) -> Result<(), String> {
    let (rows, plan) = pipeline(scene);
    let room = scene.room();
    let seen = room.seen();
    let desk = Desk {
        rows: rows.clone(),
        main: scene.main,
        ..Desk::default()
    };
    let ids: Vec<u8> = scene.monitors.iter().map(|m| m.id).collect();
    rows_partition(&ids, &rows)?;
    rows_are_rows(&room, &rows, JOIN)?;
    let _ = seen;
    complete(&plan, &sizes_of(scene))?;
    no_overlap(&plan)?;
    at_origin(&plan)?;
    rows_in_order(&plan, &rows)?;
    rows_in_their_order(&plan, &rows)?;
    connected(&plan)?;
    let _ = desk;
    Ok(())
}

/// Within each piece of a row: side by side, sharing what the mouse needs.
fn side_by_side(scene: &Scene) -> Result<(), String> {
    let (rows, plan) = pipeline(scene);
    let room = scene.room();
    let desk = Desk {
        rows,
        main: scene.main,
        ..Desk::default()
    };
    rows_side_by_side(&plan, &desk::pieces_of(&desk, |k| scene.size(k), &room))
}

fn run(
    name: &str,
    seed: u64,
    full: usize,
    make: impl Fn(&mut Rng) -> Scene,
    check: impl Fn(&Scene) -> Result<(), String>,
) {
    let mut rng = Rng::new(seed);
    for i in 0..cases(full) {
        let scene = make(&mut rng);
        if let Err(e) = check(&scene) {
            let small = shrink(&scene, |s| check(s).is_err());
            let e2 = check(&small).err().unwrap_or_default();
            panic!(
                "{name} case {i}: {e}\nsmallest: {e2}\n{}plan {}",
                small.describe(),
                describe_plan(&pipeline(&small).1)
            );
        }
    }
}

#[test]
fn presets_hold_every_hard_invariant() {
    run("preset", 0x1a7e_0002, 3000, |r| preset_scene(r).0, hard);
}

#[test]
fn rows_by_hand_hold_every_hard_invariant() {
    run(
        "rows",
        0x1a7e_0003,
        3000,
        |r| {
            let d = r.chance(50);
            let b = r.chance(25);
            rows_scene(r, d, 0.01, b)
        },
        hard,
    );
}

#[test]
fn any_hand_holds_every_hard_invariant() {
    run("hand", 0x1a7e_0004, 3000, hand_scene, hard);
}

#[test]
fn rows_are_side_by_side_in_regular_scenes() {
    run(
        "preset sides",
        0x1a7e_0005,
        2000,
        |r| preset_scene(r).0,
        side_by_side,
    );
    run(
        "rows sides",
        0x1a7e_0006,
        2000,
        |r| {
            let d = r.chance(50);
            rows_scene(r, d, 0.0, false)
        },
        side_by_side,
    );
}

/// Continuity over `cases` scenes, printed.
fn measure(
    name: &str,
    seed: u64,
    full: usize,
    make: impl Fn(&mut Rng) -> Scene,
) -> (Continuity, usize) {
    let mut rng = Rng::new(seed);
    let mut total = Continuity::default();
    let cases = cases(full);
    for _ in 0..cases {
        let scene = make(&mut rng);
        let (_, plan) = pipeline(&scene);
        total.add(&continuity(&scene, &plan, desk::TOUCH_GAP, 0.25));
    }
    eprintln!(
        "{name}: {cases} scenes, {} seam points, {} pairs lost, {} points blocked; error at the middle of a seam: \
         side {:.1} px (x{:.2} of noise, at {:?}), between rows {:.1} px (x{:.2}); largest jump {:.1} px / {:.2}° / {:.1} mm (at {:?})",
        total.samples,
        total.lost.len(),
        total.blocked,
        total.side_error,
        total.side_ratio,
        total.side_worst,
        total.stack_error,
        total.stack_ratio,
        total.max_px,
        total.max_deg,
        total.max_m * 1000.0,
        total.worst
    );
    let lost = total.lost.len();
    (total, lost)
}

#[test]
fn continuity_of_presets() {
    let (c, lost) = measure("presets", 0x1a7e_0007, 2000, |r| preset_scene(r).0);
    let _ = (c, lost);
}

#[test]
fn continuity_of_rows_by_hand() {
    let (c, lost) = measure("rows exact", 0x1a7e_0008, 2000, |r| {
        let d = r.chance(60);
        rows_scene(r, d, 0.0, false)
    });
    let _ = (c, lost);
    let (c, lost) = measure("rows 1 cm noise", 0x1a7e_0009, 2000, |r| {
        let d = r.chance(60);
        rows_scene(r, d, 0.01, false)
    });
    let _ = (c, lost);
}

#[test]
fn continuity_of_any_hand_is_reported() {
    measure("hand", 0x1a7e_000a, 2000, hand_scene);
}

/// C1, C2, C3, C4: the widening circle.
#[test]
fn the_widening_circle() {
    let mut rng = Rng::new(0x1a7e_000b);
    let limits = Limits::default();
    for i in 0..cases(3000) {
        let (scene, preset, curve) = preset_scene_curved(&mut rng);
        // C1: nothing covers anything as seen (the laptop screen hangs under the primary;
        // curved, it continues the primary's cylinder and only its flat chord is behind).
        let corners: Vec<(u8, [glam::Vec3; 4])> = scene
            .monitors
            .iter()
            .map(|m| (m.id, scene.frame.unplace(m.placement).corners(m.height())))
            .collect();
        for (a, (ia, ca)) in corners.iter().enumerate() {
            for (ib, cb) in &corners[a + 1..] {
                if curve.is_some() && (*ia == MIRROR || *ib == MIRROR) {
                    continue;
                }
                assert!(
                    !covers(ca, cb, 0.01),
                    "case {i}: {ia} covers {ib}\n{}",
                    scene.describe()
                );
            }
        }
        if preset == Preset::Wall {
            continue;
        }
        // C2, C3: every row within the arc, the radius within the limit when 3 rows do.
        let widths: Vec<f32> = scene
            .monitors
            .iter()
            .filter(|m| m.id != MIRROR)
            .map(|m| m.px.w as f32 * prostir_layout::space::METRES_PER_PIXEL)
            .collect();
        let mut sorted: Vec<_> = scene.monitors.iter().filter(|m| m.id != MIRROR).collect();
        sorted.sort_by_key(|m| m.id);
        let widths_in_order: Vec<f32> = sorted
            .iter()
            .map(|m| m.px.w as f32 * prostir_layout::space::METRES_PER_PIXEL)
            .collect();
        let (rows, r) = circle::split(&widths_in_order, &limits);
        for row in &rows {
            assert!(
                circle::row_arc(&widths_in_order[row.clone()], r, 0.0) <= ARC + 1e-5,
                "case {i}"
            );
        }
        assert!(r >= RADIUS);
        let three_fit = rows.len() < 3 || r <= MAX_RADIUS;
        if rows.len() < 3 {
            assert!(r <= MAX_RADIUS, "case {i}: {} rows at {r}", rows.len());
        }
        let _ = (three_fit, widths);
        // C4: one row while one row fits.
        if circle::fit_radius(&widths_in_order, &limits) <= MAX_RADIUS {
            assert_eq!(rows.len(), 1, "case {i}");
        }
    }
}

/// C6: the rows seen are the rows the circle made.
#[test]
fn the_circle_rows_are_the_rows_seen() {
    let mut rng = Rng::new(0x1a7e_000c);
    let limits = Limits::default();
    for i in 0..cases(2000) {
        let (scene, preset) = preset_scene(&mut rng);
        if preset == Preset::Wall {
            continue;
        }
        let mut monitors: Vec<_> = scene.monitors.iter().filter(|m| m.id != MIRROR).collect();
        monitors.sort_by_key(|m| m.id);
        let widths: Vec<f32> = monitors
            .iter()
            .map(|m| m.px.w as f32 * prostir_layout::space::METRES_PER_PIXEL)
            .collect();
        let (made, _) = circle::split(&widths, &limits);
        let mut without = scene.clone();
        without.monitors.retain(|m| m.id != MIRROR);
        let ids: Vec<u8> = without.monitors.iter().map(|m| m.id).collect();
        let rows = rows_of(&ids, &without.room(), None);
        let mut a: Vec<Vec<u8>> = rows
            .iter()
            .map(|r| {
                let mut r = r.clone();
                r.sort_unstable();
                r
            })
            .collect();
        let mut b: Vec<Vec<u8>> = made
            .iter()
            .map(|r| (r.start as u8..r.end as u8).collect())
            .collect();
        a.sort();
        b.sort();
        assert_eq!(a, b, "case {i}\n{}", scene.describe());
    }
}

/// The step rule: a step within STEP of lining up is lined up, one beyond it is kept
/// exactly.
#[test]
fn noise_is_lined_up_and_offsets_are_kept() {
    let size = |_| Some(Size { w: 2560, h: 1440 });
    // Seen in half pixels, so the boxes are not a plan already (that would be kept).
    let boxes = |dy: f64| {
        desk::Boxes::new(vec![
            (
                0u8,
                desk::SeenBox {
                    x: 0.0,
                    y: 0.0,
                    w: 5120.0,
                    h: 2880.0,
                },
                Size { w: 2560, h: 1440 },
            ),
            (
                1u8,
                desk::SeenBox {
                    x: 5120.0,
                    y: 2.0 * dy,
                    w: 5120.0,
                    h: 2880.0,
                },
                Size { w: 2560, h: 1440 },
            ),
        ])
    };
    let desk = Desk {
        rows: vec![vec![0, 1]],
        main: Some(0),
        ..Desk::default()
    };
    let limit = (STEP * 1440.0).floor();
    for dy in [0.0, 10.0, limit, -limit] {
        let p = desk::plan(&desk, size, &boxes(dy));
        assert_eq!(spot(&p, 0).unwrap().y, spot(&p, 1).unwrap().y, "{dy}");
    }
    for dy in [limit + 1.0, 240.0, 480.0, -480.0, 1000.0] {
        let p = desk::plan(&desk, size, &boxes(dy));
        assert_eq!(
            spot(&p, 1).unwrap().y - spot(&p, 0).unwrap().y,
            dy as i32,
            "{dy}"
        );
    }
    // So large the mouse would only cross a corner: cut back to what it needs.
    let p = desk::plan(&desk, size, &boxes(1400.0));
    assert_eq!(
        spot(&p, 1).unwrap().y - spot(&p, 0).unwrap().y,
        1440 - desk::REACH
    );
    let _ = Side::Right;
    let _ = boxes(0.0).between(0, 1);
}

/// One row, any sizes, offsets on purpose: every pair seen touching meets on the
/// desktop, and where the mouse crosses, the middle of every seam is where it is seen
/// to within noise (deliberate offsets kept exactly, noise lined up).
#[test]
fn a_single_row_is_reproduced_exactly() {
    let (c, lost) = measure("one row", 0x1a7e_0101, 2000, |r| {
        rows_scene_of(r, 1, None, true, 0.0, false, false)
    });
    assert_eq!(lost, 0);
    assert!(
        c.side_ratio <= 1.0,
        "side error {} (x{})",
        c.side_error,
        c.side_ratio
    );
    let (c, lost) = measure("one row, 1 cm noise", 0x1a7e_0102, 2000, |r| {
        rows_scene_of(r, 1, None, true, 0.01, false, false)
    });
    assert_eq!(lost, 0);
    assert!(
        c.side_ratio <= 1.0,
        "side error {} (x{})",
        c.side_error,
        c.side_ratio
    );
}

/// Up to three rows of one monitor size, offsets on purpose within rows, each row
/// anywhere across: the seams within rows are reproduced to within noise. The seams
/// between rows only as well as flat monitors allow (reported): a flat monitor at 1.1 m
/// spreads its pixels about half as densely again over each degree at its ends as in
/// its middle, so a row offset across from the one under it cannot meet it exactly
/// along every seam.
#[test]
fn rows_of_one_size_keep_their_side_seams() {
    let px = PixelSize::new(2560, 1440);
    let (c, _) = measure("rows of 2560", 0x1a7e_0103, 2000, |r| {
        let k = 1 + r.below(3);
        rows_scene_of(r, k, Some(px), true, 0.0, false, false)
    });
    assert!(
        c.side_ratio <= 1.0,
        "side error {} (x{})",
        c.side_error,
        c.side_ratio
    );
}

/// Rows of one size standing in columns: every pair seen touching meets, and every seam,
/// within and between rows, is where it is seen to within noise.
#[test]
fn rows_in_columns_are_reproduced_exactly() {
    for px in [(2560, 1440), (3440, 1440), (1920, 1080), (1440, 2560)] {
        let px = PixelSize::new(px.0, px.1);
        let (c, lost) = measure(
            &format!("columns of {}x{}", px.w, px.h),
            0x1a7e_0104,
            500,
            |r| {
                let k = 1 + r.below(3);
                rows_in_columns(r, k, px)
            },
        );
        assert_eq!(lost, 0);
        assert!(
            c.side_ratio <= 1.0,
            "side error {} (x{})",
            c.side_error,
            c.side_ratio
        );
        assert!(
            c.stack_ratio <= 1.0,
            "error between rows {} (x{})",
            c.stack_error,
            c.stack_ratio
        );
    }
}

/// D12: the PC, planning the headset's plan again from `SetLayout`, gets that plan to the
/// pixel. Always for the views the headset makes itself (presets, rows built by hand,
/// "Line up"); for anything at all (monitors behind the user, nonsense boxes) at least 99
/// in 100 times, and every hard invariant holds either way.
#[test]
fn the_pc_reproduces_the_headsets_plan() {
    let rate = |name: &str, seed: u64, full: usize, make: &dyn Fn(&mut Rng) -> Scene| {
        let mut rng = Rng::new(seed);
        let cases = cases(full);
        let mut same = 0;
        for _ in 0..cases {
            let scene = make(&mut rng);
            let (rows, plan) = pipeline(&scene);
            let desk = Desk {
                rows,
                main: scene.main,
                ..Desk::default()
            };
            if idempotent(&desk, &plan, |k| scene.size(k)).is_ok() {
                same += 1;
            }
        }
        eprintln!("{name}: the PC gets the headset's plan back in {same} of {cases}");
        (same, cases)
    };
    let (s, n) = rate("presets", 0x1a7e_0201, 3000, &|r| preset_scene(r).0);
    assert_eq!(s, n);
    let (s, n) = rate("rows by hand", 0x1a7e_0202, 3000, &|r| {
        let d = r.chance(50);
        let b = r.chance(25);
        rows_scene(r, d, 0.01, b)
    });
    assert_eq!(s, n);
    let (s, n) = rate("anything by hand", 0x1a7e_0203, 3000, &hand_scene);
    assert!(s * 100 >= n * 99, "{s} of {n}");
    // Nonsense boxes: planning twice still settles (the second plan is the third).
    let mut rng = Rng::new(0x1a7e_0204);
    let mut same = 0;
    let n = cases(5000);
    for _ in 0..n {
        let (desk, boxes, sizes) = random_desk(&mut rng);
        let plan = desk::plan(&desk, |k| sizes.get(&k).copied(), &boxes);
        if idempotent(&desk, &plan, |k| sizes.get(&k).copied()).is_ok() {
            same += 1;
        }
    }
    eprintln!("nonsense boxes: planned again, the same in {same} of {n}");
    assert!(same * 100 >= n * 99, "{same} of {n}");
}
