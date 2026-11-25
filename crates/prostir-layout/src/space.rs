//! The room: monitors in 3D, the frame an arrangement is laid out in, and each monitor
//! as the eye sees it (an angular rectangle).
//!
//! Azimuth is measured in the frame, 0 straight ahead (-Z), positive to the right (+X).
//! Elevation is 0 at eye level, positive up (+Y). Both in radians.

use glam::{DVec3, Quat, Vec2, Vec3};

/// Monitor size in metres per stream pixel by default: every monitor gets the same
/// pixel density, so neighbours line up like one big screen (2560 px is 1.25 m).
pub const METRES_PER_PIXEL: f32 = 1.25 / 2560.0;

/// A stream's size in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PixelSize {
    pub w: u32,
    pub h: u32,
}

impl PixelSize {
    pub const fn new(w: u32, h: u32) -> Self {
        Self { w, h }
    }

    /// Height over width.
    pub fn aspect(self) -> f32 {
        self.h as f32 / self.w.max(1) as f32
    }
}

/// Where a monitor floats: the centre of its flat face (for a curved monitor the
/// centre of the chord between its two vertical edges, which it shares with the flat
/// one), its rotation (the face looks along +Z, up is +Y) and its width in metres. Its
/// height follows the stream's aspect ratio. The same as the headset's
/// `layout::Placement`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub position: Vec3,
    pub rotation: Quat,
    pub width: f32,
}

impl Placement {
    /// Height in metres for a stream of `px`.
    pub fn height(&self, px: PixelSize) -> f32 {
        self.width * px.aspect()
    }

    /// The corners for a face `height` high: top left, top right, bottom right,
    /// bottom left.
    pub fn corners(&self, height: f32) -> [Vec3; 4] {
        let (x, y) = (self.width / 2.0, height / 2.0);
        [(-x, y), (x, y), (x, -y), (-x, -y)]
            .map(|(a, b)| self.position + self.rotation * Vec3::new(a, b, 0.0))
    }

    /// The way the face looks (towards the viewer).
    pub fn normal(&self) -> Vec3 {
        self.rotation * Vec3::Z
    }
}

/// A monitor in the room: its key, its stream size and where it floats.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Monitor<K> {
    pub id: K,
    pub px: PixelSize,
    pub placement: Placement,
}

impl<K> Monitor<K> {
    pub fn height(&self) -> f32 {
        self.placement.height(self.px)
    }

    /// Metres per stream pixel of this monitor (the user may have resized it).
    pub fn metres_per_pixel(&self) -> f32 {
        self.placement.width / self.px.w.max(1) as f32
    }
}

/// Rotation that makes a quad (front face along +Z) look at `target`, without roll.
/// The same as the headset's `layout::facing`, operation for operation.
pub fn facing(position: Vec3, target: Vec3) -> Quat {
    let dir = (target - position).normalize_or(Vec3::Z);
    let yaw = dir.x.atan2(dir.z);
    let pitch = dir.y.clamp(-1.0, 1.0).asin();
    Quat::from_rotation_y(yaw) * Quat::from_rotation_x(-pitch)
}

/// The frame an arrangement is laid out in: the eye, and the turn from the frame's
/// axes to the room's. In the frame -Z is straight ahead, +X right and +Y up.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    pub eye: Vec3,
    pub turn: Quat,
}

impl Frame {
    /// Sitting or standing, the arrangement facing `yaw` (the headset's `center.1`:
    /// 0 is -Z, positive turns towards -X).
    pub fn upright(eye: Vec3, yaw: f32) -> Self {
        Self {
            eye,
            turn: Quat::from_rotation_y(yaw),
        }
    }

    /// Lying down: the arrangement facing `yaw`, then tilted up by `pitch` about the
    /// frame's X axis (the headset's overhead preset uses `OVERHEAD_PITCH`).
    pub fn tilted(eye: Vec3, yaw: f32, pitch: f32) -> Self {
        Self {
            eye,
            turn: Quat::from_rotation_y(yaw) * Quat::from_rotation_x(pitch),
        }
    }

    /// A room point in the frame's coordinates.
    pub fn to_local(&self, p: Vec3) -> Vec3 {
        self.turn.inverse() * (p - self.eye)
    }

    /// A frame point in the room.
    pub fn to_world(&self, p: Vec3) -> Vec3 {
        self.eye + self.turn * p
    }

    /// A placement made in the frame's coordinates, in the room.
    pub fn place(&self, p: Placement) -> Placement {
        Placement {
            position: self.to_world(p.position),
            rotation: self.turn * p.rotation,
            width: p.width,
        }
    }

    /// A placement in the room, in the frame's coordinates.
    pub fn unplace(&self, p: Placement) -> Placement {
        Placement {
            position: self.to_local(p.position),
            rotation: self.turn.inverse() * p.rotation,
            width: p.width,
        }
    }
}

/// Azimuth of a frame-local point: 0 ahead, positive to the right.
pub fn azimuth(p: DVec3) -> f64 {
    p.x.atan2(-p.z)
}

/// Elevation of a frame-local point: 0 at eye level, positive up.
pub fn elevation(p: DVec3) -> f64 {
    p.y.atan2((p.x * p.x + p.z * p.z).sqrt())
}

/// How far up or down from eye level the middle of a monitor may be seen and still be
/// in view without tipping the head far back or down (40°): a row over the main one
/// stands lower, a monitor right over the head (a flat one far wider than the circle
/// it stands on, whose middle comes close to the eye) higher.
pub const VIEW_CONE: f64 = 40.0 * std::f64::consts::PI / 180.0;

/// Whether the middle of `p` (a frame-local placement) is seen within [`VIEW_CONE`] of
/// eye level. Around the user every direction counts: a row may go far to either side.
pub fn in_view(p: &Placement) -> bool {
    let c = p.position.as_dvec3();
    p.position.is_finite() && elevation(c).abs() <= VIEW_CONE
}

/// `angle` moved by whole turns to within half a turn of `around`.
pub fn unwrap(angle: f64, around: f64) -> f64 {
    use std::f64::consts::{PI, TAU};
    around + (angle - around + PI).rem_euclid(TAU) - PI
}

/// `angles` moved by whole turns so they run on without a jump: cut in the middle of
/// the widest empty stretch between them around the circle (behind a row, for any row
/// that does not go all the way round). Turning them all by one angle turns the result
/// by the same angle.
pub fn contiguous(angles: &[f64]) -> Vec<f64> {
    use std::f64::consts::TAU;
    let n = angles.len();
    if n < 2 {
        return angles.to_vec();
    }
    let mut sorted: Vec<f64> = angles.iter().map(|a| a.rem_euclid(TAU)).collect();
    sorted.sort_by(f64::total_cmp);
    // The widest stretch: from sorted[i] to the next one around.
    let (mut widest, mut after) = (-1.0, 0.0);
    for i in 0..n {
        let next = if i + 1 < n {
            sorted[i + 1]
        } else {
            sorted[0] + TAU
        };
        if next - sorted[i] > widest {
            widest = next - sorted[i];
            after = sorted[i];
        }
    }
    let cut = after + widest / 2.0;
    angles
        .iter()
        .map(|&a| cut - (cut - a).rem_euclid(TAU))
        .collect()
}

/// A monitor as the eye sees it in the frame: an angular rectangle, radians.
///
/// Its edges are the means of its corners' angles: the left edge is the mean azimuth
/// of the two leftmost corners, the top edge the mean elevation of the two highest,
/// and so on. Corners, not edge midpoints: the middle of a wide flat monitor is nearer
/// the eye than its corners and so seen higher, while the corners of monitors standing
/// on one cylinder around the eye are seen at the same elevation whatever their widths.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Seen<K> {
    pub id: K,
    pub left: f64,
    pub right: f64,
    pub bottom: f64,
    pub top: f64,
}

impl<K> Seen<K> {
    pub fn width(&self) -> f64 {
        self.right - self.left
    }

    pub fn height(&self) -> f64 {
        self.top - self.bottom
    }

    pub fn center_x(&self) -> f64 {
        (self.left + self.right) / 2.0
    }

    pub fn center_y(&self) -> f64 {
        (self.bottom + self.top) / 2.0
    }

    /// How much of the shorter one's height the two share, 0..=1.
    pub fn vertical_overlap<L>(&self, o: &Seen<L>) -> f64 {
        let shared = self.top.min(o.top) - self.bottom.max(o.bottom);
        (shared / self.height().min(o.height()).max(1e-12)).clamp(0.0, 1.0)
    }

    /// How much of the narrower one's width the two share, 0..=1.
    pub fn horizontal_overlap<L>(&self, o: &Seen<L>) -> f64 {
        let shared = self.right.min(o.right) - self.left.max(o.left);
        (shared / self.width().min(o.width()).max(1e-12)).clamp(0.0, 1.0)
    }
}

/// Every monitor as seen from the frame's eye (see [`Seen`]). Azimuths are cut where
/// the widest empty stretch around the user is (behind them, for any sane
/// arrangement), so a row that runs across the back of the circle keeps its order, and
/// the result does not change when everything turns around the user together.
pub fn see<K: Copy>(frame: &Frame, monitors: &[Monitor<K>]) -> Vec<Seen<K>> {
    // Each monitor's corners, azimuths unwrapped around its own centre.
    let raw: Vec<Seen<K>> = monitors
        .iter()
        .map(|m| {
            let local = |p: Vec3| frame.to_local(p).as_dvec3();
            let around = azimuth(local(m.placement.position));
            seen_corners(m.id, m.placement.corners(m.height()).map(local), around)
        })
        .collect();
    let cut = widest_gap(&raw);
    raw.into_iter()
        .map(|s| {
            // Centre into (cut - 2π, cut], the edges with it.
            let c = s.center_x();
            let moved = cut - (cut - c).rem_euclid(std::f64::consts::TAU);
            let shift = moved - c;
            Seen {
                left: s.left + shift,
                right: s.right + shift,
                ..s
            }
        })
        .collect()
}

/// The azimuth in the middle of the widest stretch no monitor covers. Behind the user
/// (π) when nothing is seen or the monitors go all the way around; between two equally
/// wide stretches, the one nearer behind.
fn widest_gap<K>(seen: &[Seen<K>]) -> f64 {
    use std::f64::consts::{PI, TAU};
    // Covered intervals on [0, 2π), split where they wrap.
    let mut spans: Vec<(f64, f64)> = Vec::new();
    for s in seen {
        let width = s.width().min(TAU);
        let start = s.left.rem_euclid(TAU);
        let end = start + width;
        if end <= TAU {
            spans.push((start, end));
        } else {
            spans.push((start, TAU));
            spans.push((0.0, end - TAU));
        }
    }
    if spans.is_empty() {
        return PI;
    }
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
    // Merge, then look at the gaps between merged spans, around the circle.
    let mut merged: Vec<(f64, f64)> = Vec::new();
    for (a, b) in spans {
        match merged.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => merged.push((a, b)),
        }
    }
    let mut best: Option<(f64, f64)> = None; // (width, middle)
    for i in 0..merged.len() {
        let end = merged[i].1;
        let next = if i + 1 < merged.len() {
            merged[i + 1].0
        } else {
            merged[0].0 + TAU
        };
        let width = next - end;
        if width <= 1e-9 {
            continue;
        }
        let middle = (end + width / 2.0).rem_euclid(TAU);
        let behind = |m: f64| (m - PI).abs();
        let better = match best {
            None => true,
            Some((w, m)) => width > w + 1e-9 || (width > w - 1e-9 && behind(middle) < behind(m)),
        };
        if better {
            best = Some((width, middle));
        }
    }
    best.map_or(PI, |(_, middle)| middle)
}

/// Which way `b` lies from `a`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Side {
    Right,
    Left,
    Above,
    Below,
}

impl Side {
    pub fn opposite(self) -> Side {
        match self {
            Side::Right => Side::Left,
            Side::Left => Side::Right,
            Side::Above => Side::Below,
            Side::Below => Side::Above,
        }
    }
}

/// Two monitors that meet: `b` lies on `side` of `a`, and they share `seam` of the
/// shorter edge along where they meet (0..=1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Touch<K> {
    pub a: K,
    pub b: K,
    pub side: Side,
    pub seam: f64,
}

/// The pairs that meet as seen: their facing edges at most `gap` apart (a fraction of
/// the smaller of the two sizes across that gap; overlapping ones meet too), sharing
/// at least `seam` of the shorter edge along where they meet.
pub fn touches<K: Copy>(seen: &[Seen<K>], gap: f64, seam: f64) -> Vec<Touch<K>> {
    let mut out = Vec::new();
    for (i, a) in seen.iter().enumerate() {
        for b in &seen[i + 1..] {
            let gx = (b.left - a.right).max(a.left - b.right);
            let gy = (b.bottom - a.top).max(a.bottom - b.top);
            // Separated across (side by side) when the gap across is the wider one.
            let (side, shared, tolerance) = if gx >= gy {
                let side = if b.center_x() >= a.center_x() {
                    Side::Right
                } else {
                    Side::Left
                };
                (side, a.vertical_overlap(b), gap * a.width().min(b.width()))
            } else {
                let side = if b.center_y() >= a.center_y() {
                    Side::Above
                } else {
                    Side::Below
                };
                (
                    side,
                    a.horizontal_overlap(b),
                    gap * a.height().min(b.height()),
                )
            };
            if gx.max(gy) <= tolerance && shared >= seam {
                out.push(Touch {
                    a: a.id,
                    b: b.id,
                    side,
                    seam: shared,
                });
            }
        }
    }
    out
}

/// A monitor `id` with its `corners` in a frame's own coordinates (the eye at the
/// origin), as seen, its azimuths moved by whole turns to lie around `around`: an edge
/// is the mean of the two corners nearest that way (see [`see`]).
pub fn seen_corners<K>(id: K, corners: [DVec3; 4], around: f64) -> Seen<K> {
    let mut az = corners.map(|c| unwrap(azimuth(c), around));
    let mut el = corners.map(elevation);
    az.sort_by(f64::total_cmp);
    el.sort_by(f64::total_cmp);
    let (left, right) = ((az[0] + az[1]) / 2.0, (az[2] + az[3]) / 2.0);
    let (bottom, top) = ((el[0] + el[1]) / 2.0, (el[2] + el[3]) / 2.0);
    Seen {
        id,
        left,
        right: right.max(left + 1e-9),
        bottom,
        top: top.max(bottom + 1e-9),
    }
}

/// One monitor placed in a frame's own coordinates (eye at the origin, -Z ahead), as
/// seen, its azimuths moved by whole turns to lie around `around`.
pub fn seen_local(p: &Placement, px: PixelSize, around: f64) -> Seen<()> {
    let m = Monitor {
        id: (),
        px,
        placement: *p,
    };
    let s = see(&Frame::upright(Vec3::ZERO, 0.0), &[m])[0];
    let shift = unwrap(s.center_x(), around) - s.center_x();
    Seen {
        left: s.left + shift,
        right: s.right + shift,
        ..s
    }
}

/// Whether one monitor covers another as seen from the eye at the origin of their
/// coordinates (the frame's), by more than `slack` of the smaller one's seen area.
///
/// Exact for flat faces: both are projected through the eye onto the plane facing the
/// direction between them (a flat face stays a four-sided figure there), and the two
/// figures are clipped against each other. A curved monitor lies behind its flat chord,
/// so for it this errs on the side of covering. Two monitors too far apart around the
/// eye to be projected together (a corner 85° or more from the direction between them)
/// fall back to [`overlaps`] of their angular rectangles. Allocates nothing.
pub fn covers(a: &[Vec3; 4], b: &[Vec3; 4], slack: f64) -> bool {
    use glam::DVec2;
    let dir = |c: &[Vec3; 4]| {
        c.iter()
            .map(|p| p.as_dvec3().normalize_or_zero())
            .sum::<DVec3>()
            .normalize_or_zero()
    };
    let d = (dir(a) + dir(b)).normalize_or(DVec3::NEG_Z);
    let all = a.iter().chain(b.iter());
    if all
        .clone()
        .any(|p| p.as_dvec3().normalize_or_zero().dot(d) < 85f64.to_radians().cos())
    {
        // Each around its own middle, then the second moved by whole turns to lie
        // around the first: two seen nearly opposite each other have no direction
        // between them worth the name (their sum points up or down), and around such
        // a one a monitor's corners would fall on both sides of the cut.
        let rect = |c: &[Vec3; 4]| {
            let middle = azimuth(dir(c));
            let mut az = c.map(|p| unwrap(azimuth(p.as_dvec3()), middle));
            let mut el = c.map(|p| elevation(p.as_dvec3()));
            az.sort_by(f64::total_cmp);
            el.sort_by(f64::total_cmp);
            Seen {
                id: (),
                left: (az[0] + az[1]) / 2.0,
                right: (az[2] + az[3]) / 2.0,
                bottom: (el[0] + el[1]) / 2.0,
                top: (el[2] + el[3]) / 2.0,
            }
        };
        let (ra, rb) = (rect(a), rect(b));
        let shift = unwrap(rb.center_x(), ra.center_x()) - rb.center_x();
        let rb = Seen {
            left: rb.left + shift,
            right: rb.right + shift,
            ..rb
        };
        return overlaps(&ra, &rb, slack);
    }
    let u = d.cross(DVec3::Y).normalize_or(DVec3::X);
    let v = u.cross(d);
    let flat = |c: &[Vec3; 4]| -> Poly {
        let mut poly = Poly::default();
        for p in c {
            let p = p.as_dvec3();
            let q = p / p.dot(d);
            poly.push(DVec2::new(q.dot(u), q.dot(v)));
        }
        // Counter-clockwise.
        if area(poly.corners()) < 0.0 {
            poly.corners_mut().reverse();
        }
        poly
    };
    let (pa, pb) = (flat(a), flat(b));
    let smaller = area(pa.corners()).abs().min(area(pb.corners()).abs());
    let shared = clip(&pa, pb.corners());
    smaller > 0.0 && area(shared.corners()).abs() > slack * smaller
}

/// How much of monitor `a` (`ah` high) monitor `b` (`bh` high) is seen over or behind
/// from `eye`, as a share of `a`'s face: the part of `a` that lines of sight through
/// `b` reach. Exact for flat faces wherever they are around the eye: `b` is cut to
/// what lies on `a`'s side of the eye, and that is projected through the eye onto
/// `a`'s plane. Allocates nothing (the magnet asks it every frame of a drag).
pub fn seen_over(a: &Placement, ah: f32, b: &Placement, bh: f32, eye: Vec3) -> f64 {
    seen_against(a, ah, b, bh, eye).0
}

/// How much of monitor `a` (`ah` high) monitor `b` (`bh` high) hides from `eye`, as a
/// share of `a`'s face: [`seen_over`] where `b` is the nearer. Two that do not cut
/// into each other (see [`cuts`]) stand in one order wherever they are seen over each
/// other, so one point of that part tells which is in front. Allocates nothing.
pub fn hidden_by(a: &Placement, ah: f32, b: &Placement, bh: f32, eye: Vec3) -> f64 {
    match seen_against(a, ah, b, bh, eye) {
        (share, true) => share,
        (_, false) => 0.0,
    }
}

/// [`seen_over`], and whether `b` is in front of `a` there.
fn seen_against(a: &Placement, ah: f32, b: &Placement, bh: f32, eye: Vec3) -> (f64, bool) {
    use glam::DVec2;
    let turn = a.rotation.as_dquat().inverse();
    let local = |p: Vec3| -> DVec3 { turn * (p.as_dvec3() - a.position.as_dvec3()) };
    let e = local(eye);
    let (w, h) = (f64::from(a.width), f64::from(ah));
    if e.z.abs() < 1e-9 || w <= 0.0 || h <= 0.0 {
        return (0.0, false);
    }
    // How far towards `a`'s plane from the eye, as a share of the eye's distance from
    // it: a point of `b` with none (level with the eye or behind it) is not seen
    // against `a`.
    let towards = |q: DVec3| (e.z - q.z) / e.z;
    const NEAR: f64 = 1e-6;
    let corners = b.corners(bh).map(local);
    let project = |r: DVec3| {
        let at = e + (r - e) / towards(r);
        DVec2::new(at.x, at.y)
    };
    let mut onto = Poly::default();
    for i in 0..4 {
        let (p, q) = (corners[i], corners[(i + 1) % 4]);
        let (tp, tq) = (towards(p), towards(q));
        if tp >= NEAR {
            onto.push(project(p));
        }
        if (tp >= NEAR) != (tq >= NEAR) {
            onto.push(project(p + (q - p) * ((NEAR - tp) / (tq - tp))));
        }
    }
    if onto.n < 3 {
        return (0.0, false);
    }
    let mut face = Poly::default();
    for (x, y) in [(-0.5, -0.5), (0.5, -0.5), (0.5, 0.5), (-0.5, 0.5)] {
        face.push(DVec2::new(x * w, y * h));
    }
    let shared = clip(&onto, face.corners());
    let share = area(shared.corners()).abs() / (w * h);
    if shared.n == 0 {
        return (share, false);
    }
    // A point of `a` seen behind `b` or over it, the middle of that part, and where the
    // line of sight to it meets `b`'s plane: before it, `b` is in front.
    let mid = shared.corners().iter().copied().sum::<DVec2>() / shared.n as f64;
    let at = DVec3::new(mid.x, mid.y, 0.0);
    let (on_b, b_normal) = (local(b.position), turn * b.normal().as_dvec3());
    let along = (at - e).dot(b_normal);
    let front = along.abs() > 1e-12 && {
        let t = (on_b - e).dot(b_normal) / along;
        t > 0.0 && t < 1.0
    };
    (share, front)
}

/// A convex polygon of up to twelve corners, kept on the stack: a four-sided figure
/// clipped by another (each side it is clipped by adds one corner at most), or one cut
/// by a plane first.
#[derive(Debug, Clone, Copy, Default)]
struct Poly {
    at: [glam::DVec2; 12],
    n: usize,
}

impl Poly {
    fn push(&mut self, p: glam::DVec2) {
        debug_assert!(
            self.n < self.at.len(),
            "a clipped quad has twelve corners at most"
        );
        if let Some(slot) = self.at.get_mut(self.n) {
            *slot = p;
            self.n += 1;
        }
    }

    fn corners(&self) -> &[glam::DVec2] {
        &self.at[..self.n]
    }

    fn corners_mut(&mut self) -> &mut [glam::DVec2] {
        &mut self.at[..self.n]
    }
}

/// Signed area of a polygon (positive counter-clockwise).
fn area(p: &[glam::DVec2]) -> f64 {
    let n = p.len();
    (0..n).map(|i| p[i].perp_dot(p[(i + 1) % n])).sum::<f64>() / 2.0
}

/// `subject` clipped to the convex counter-clockwise polygon `by` (Sutherland and
/// Hodgman).
fn clip(subject: &Poly, by: &[glam::DVec2]) -> Poly {
    let mut out = *subject;
    for i in 0..by.len() {
        let (a, b) = (by[i], by[(i + 1) % by.len()]);
        let inside = |p: glam::DVec2| (b - a).perp_dot(p - a) >= 0.0;
        let taken = std::mem::take(&mut out);
        let input = taken.corners();
        for j in 0..input.len() {
            let (p, q) = (input[j], input[(j + 1) % input.len()]);
            let cross = |p: glam::DVec2, q: glam::DVec2| {
                let (dp, dq) = ((b - a).perp_dot(p - a), (b - a).perp_dot(q - a));
                p + (q - p) * (dp / (dp - dq))
            };
            match (inside(p), inside(q)) {
                (true, true) => out.push(q),
                (true, false) => out.push(cross(p, q)),
                (false, true) => {
                    out.push(cross(p, q));
                    out.push(q);
                }
                (false, false) => {}
            }
        }
        if out.n == 0 {
            break;
        }
    }
    out
}

/// Whether two seen monitors cover each other by more than `slack` of the smaller
/// size, both across and up (touching edges do not count).
pub fn overlaps<K, L>(a: &Seen<K>, b: &Seen<L>, slack: f64) -> bool {
    let across = a.right.min(b.right) - a.left.max(b.left);
    let up = a.top.min(b.top) - a.bottom.max(b.bottom);
    across > slack * a.width().min(b.width()) && up > slack * a.height().min(b.height())
}

/// How far one monitor may reach into another before it counts as cutting it (half a
/// millimetre): edges that meet, glued, and the last bits of a float do not.
pub const CUT_SLACK: f32 = 0.0005;

/// Two edges whose ends lie this close to one line (a centimetre) are on one line, as
/// for a fold (see `Face::folded`).
const FOLD_NEAR: f32 = 0.01;

/// Two faces going the same way from an edge they share, the angle between them under
/// 60 degrees (its cosine), are folded one over the other.
const FOLD_COS: f32 = 0.5;

/// A flat face: where, how turned, half its width and height.
#[derive(Debug, Clone, Copy)]
struct Face {
    p: Placement,
    half: Vec2,
}

impl Face {
    fn new(p: &Placement, height: f32) -> Self {
        Self {
            p: *p,
            half: Vec2::new(p.width / 2.0, height / 2.0),
        }
    }

    /// Half its size, made [`CUT_SLACK`] smaller on every side (none when nothing is
    /// left of it).
    fn inner(&self) -> Option<Vec2> {
        let half = self.half - Vec2::splat(CUT_SLACK);
        (half.x > 0.0 && half.y > 0.0).then_some(half)
    }

    fn corners(&self) -> [Vec3; 4] {
        let (x, y) = (self.half.x, self.half.y);
        [(-x, -y), (x, -y), (x, y), (-x, y)]
            .map(|(a, b)| self.p.position + self.p.rotation * Vec3::new(a, b, 0.0))
    }

    /// A room point in the face's own coordinates (its plane is z = 0).
    fn local(&self, at: Vec3) -> Vec3 {
        self.p.rotation.inverse() * (at - self.p.position)
    }

    /// The part of the line `from + t * dir` (in the face's coordinates, in its plane)
    /// within the face made [`CUT_SLACK`] smaller on every side, as `t` from and to
    /// (Liang and Barsky); `t` itself within `range`.
    fn clip(&self, from: Vec2, dir: Vec2, range: (f32, f32)) -> Option<(f32, f32)> {
        let half = self.inner()?;
        let (mut t0, mut t1) = range;
        for (p, q) in [
            (-dir.x, from.x + half.x),
            (dir.x, half.x - from.x),
            (-dir.y, from.y + half.y),
            (dir.y, half.y - from.y),
        ] {
            if p.abs() < 1e-9 {
                if q <= 0.0 {
                    return None;
                }
                continue;
            }
            let t = q / p;
            if p < 0.0 {
                t0 = t0.max(t);
            } else {
                t1 = t1.min(t);
            }
        }
        (t0 < t1).then_some((t0, t1))
    }

    /// Whether `other`, lying in this face's plane, overlaps this face by more than
    /// [`CUT_SLACK`]: an edge of either goes inside the other, or the two are one (each
    /// edge on the other's, none inside).
    fn over_in_plane(&self, other: &Face) -> bool {
        let inside = |f: &Face, at: Vec3| {
            let l = f.local(at).truncate();
            f.inner()
                .is_some_and(|half| l.x.abs() < half.x && l.y.abs() < half.y)
        };
        self.entered_in_plane(other)
            || other.entered_in_plane(self)
            || inside(self, other.p.position)
            || inside(other, self.p.position)
    }

    /// Whether this face and `other` are folded one over the other: an edge of each on
    /// one line (within [`FOLD_NEAR`]), the two edges sharing more than [`CUT_SLACK`]
    /// of it, and both faces going the same way from it, within 60 degrees of each
    /// other. For two faces in two planes that line is the one the planes meet on; two
    /// hung from it to the same side lie over each other however small the angle
    /// between them, and that angle comes from their heights.
    fn folded(&self, other: &Face) -> bool {
        let edges = |f: &Face| {
            let c = f.corners();
            let middle = f.p.position;
            [0, 1, 2, 3].map(|i| {
                let (from, to) = (c[i], c[(i + 1) % 4]);
                // Into the face from its edge, square to it.
                let inward = (middle - (from + to) / 2.0).normalize_or_zero();
                (from, to, inward)
            })
        };
        let theirs = edges(other);
        edges(self).iter().any(|&(from, to, inward)| {
            let length = from.distance(to);
            let Some(way) = (to - from).try_normalize() else {
                return false;
            };
            theirs.iter().any(|&(f, t, other_inward)| {
                let off = |p: Vec3| (p - from).reject_from_normalized(way).length();
                let (s0, s1) = ((f - from).dot(way), (t - from).dot(way));
                let shared = s0.max(s1).min(length) - s0.min(s1).max(0.0);
                off(f) < FOLD_NEAR
                    && off(t) < FOLD_NEAR
                    && shared > CUT_SLACK
                    && inward.dot(other_inward) > FOLD_COS
            })
        })
    }

    /// Whether an edge of `other`, lying in this face's plane, goes inside this face.
    fn entered_in_plane(&self, other: &Face) -> bool {
        let c = other.corners().map(|c| self.local(c).truncate());
        (0..4).any(|i| {
            let (a, b) = (c[i], c[(i + 1) % 4]);
            self.clip(a, b - a, (0.0, 1.0)).is_some()
        })
    }
}

/// Whether monitor `a` (`ah` high) and monitor `b` (`bh` high) cut into each other by
/// more than [`CUT_SLACK`]: in two planes, the line where the planes meet runs through
/// both of them; in one plane, they overlap; hung from one edge to the same side,
/// whatever the angle between them up to 60 degrees, they are folded one over the
/// other. Edges that meet otherwise (glued, hinged apart) do not count.
pub fn cuts(a: &Placement, ah: f32, b: &Placement, bh: f32) -> bool {
    let (fa, fb) = (Face::new(a, ah), Face::new(b, bh));
    // Hung from one edge to the same side: one lies over the other.
    if fa.folded(&fb) {
        return true;
    }
    let (na, nb) = (a.normal(), b.normal());
    let across = na.cross(nb);
    if across.length_squared() < 1e-8 {
        // Facing the same way (or opposite): only in one plane can they meet.
        if (b.position - a.position).dot(na).abs() >= CUT_SLACK {
            return false;
        }
        return fa.over_in_plane(&fb);
    }
    // The line both planes hold: a point of it, and its way.
    let (ha, hb) = (na.dot(a.position), nb.dot(b.position));
    let from = (nb.cross(across) * ha + across.cross(na) * hb) / across.length_squared();
    let dir = across.normalize();
    let along = |f: &Face| {
        let o = f.local(from).truncate();
        let d = (f.p.rotation.inverse() * dir).truncate();
        f.clip(o, d, (f32::MIN, f32::MAX))
    };
    match (along(&fa), along(&fb)) {
        (Some((a0, a1)), Some((b0, b1))) => a1.min(b1) - a0.max(b0) > CUT_SLACK,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upright(x: f32, z: f32, yaw: f32, width: f32) -> Placement {
        Placement {
            position: Vec3::new(x, 0.0, z),
            rotation: Quat::from_rotation_y(yaw),
            width,
        }
    }

    /// Edge to edge, a corner on an edge, or apart: no cut; one through another, or two
    /// in one plane over each other: a cut.
    #[test]
    fn what_cuts_and_what_meets() {
        let a = upright(0.0, -1.0, 0.0, 1.0);
        // Beside it in its plane, edge to edge, and a hair apart.
        assert!(!cuts(&a, 0.5, &upright(1.0, -1.0, 0.0, 1.0), 0.5));
        assert!(!cuts(&a, 0.5, &upright(1.001, -1.0, 0.0, 1.0), 0.5));
        // Over it by a centimetre, in its plane.
        assert!(cuts(&a, 0.5, &upright(0.99, -1.0, 0.0, 1.0), 0.5));
        // Hinged on its right edge, turned towards the eye: they share the edge.
        let turn = 0.5f32;
        let hinged = upright(0.5 + 0.5 * turn.cos(), -1.0 + 0.5 * turn.sin(), -turn, 1.0);
        assert!(!cuts(&a, 0.5, &hinged, 0.5));
        // The same, 5 cm to the left and 2 cm back: through it.
        let into = Placement {
            position: hinged.position - Vec3::new(0.05, 0.0, 0.02),
            ..hinged
        };
        assert!(cuts(&a, 0.5, &into, 0.5));
        // Square to it, through its middle; and in front of it, not touching.
        let through = upright(0.0, -1.0, std::f32::consts::FRAC_PI_2, 0.4);
        assert!(cuts(&a, 0.5, &through, 0.5));
        assert!(!cuts(&a, 0.5, &upright(0.0, -0.8, 0.0, 0.4), 0.3));
        // Smaller, inside it in its plane.
        assert!(cuts(&a, 0.5, &upright(0.1, -1.0, 0.0, 0.3), 0.2));
        // Two in one place, and one a hair behind the other: each on the other.
        assert!(cuts(&a, 0.5, &a, 0.5));
        assert!(cuts(&a, 0.5, &upright(0.0, -1.0001, 0.0, 1.0), 0.5));
        // Hinged on its top edge, turned half a degree: lying over it, not beside it.
        let lid = |turn: f32, h: f32| {
            let r = Quat::from_rotation_x(turn);
            Placement {
                position: Vec3::new(0.0, 0.25, -1.0) + r * Vec3::new(0.0, -h / 2.0, 0.0),
                rotation: r,
                width: 1.0,
            }
        };
        assert!(cuts(&a, 0.5, &lid(0.5f32.to_radians(), 0.4), 0.4));
        // Hinged on it upwards (above it, a little tilted), and 5 degrees over it: apart.
        let above = |turn: f32| {
            let r = Quat::from_rotation_x(turn);
            Placement {
                position: Vec3::new(0.0, 0.25, -1.0) + r * Vec3::new(0.0, 0.2, 0.0),
                rotation: r,
                width: 1.0,
            }
        };
        assert!(!cuts(&a, 0.5, &above(0.5f32.to_radians()), 0.4));
        assert!(!cuts(&a, 0.5, &above(-3.0f32.to_radians()), 0.4));
    }

    /// Two hung from one edge to the same side are folded one over the other whatever
    /// the angle between them, up to 60 degrees: the magnet turns each to face the eye,
    /// by an angle that depends on its height, so on a mixed desk they fan apart by
    /// anything from nothing to 30 degrees. Square to each other, or each to its own
    /// side, they only meet.
    #[test]
    fn hung_from_one_edge_to_one_side_is_a_fold() {
        // Both hung down from the line y = 0.25, z = -1, turned about it by `turn`.
        let hung = |turn: f32, x: f32, width: f32, h: f32| {
            let r = Quat::from_rotation_x(turn.to_radians());
            Placement {
                position: Vec3::new(x, 0.25, -1.0) + r * Vec3::new(0.0, -h / 2.0, 0.0),
                rotation: r,
                width,
            }
        };
        let a = hung(2.0, 0.0, 1.0, 0.5);
        for turn in [2.0, 2.5, 5.77, 12.0, 32.0, 61.0, -27.0, -57.0] {
            assert!(cuts(&a, 0.5, &hung(turn, 0.3, 1.0, 0.4), 0.4), "{turn}");
            assert!(cuts(&hung(turn, 0.3, 1.0, 0.4), 0.4, &a, 0.5), "{turn}");
        }
        // Square to it, either way, and far apart along the edge: they only meet.
        assert!(!cuts(&a, 0.5, &hung(92.0, 0.3, 1.0, 0.4), 0.4));
        assert!(!cuts(&a, 0.5, &hung(-88.0, 0.3, 1.0, 0.4), 0.4));
        assert!(!cuts(&a, 0.5, &hung(5.0, 1.0, 1.0, 0.4), 0.4));
        // Hung up from the same line instead: one each side of it.
        let up = |turn: f32| {
            let r = Quat::from_rotation_x(turn.to_radians());
            Placement {
                position: Vec3::new(0.3, 0.25, -1.0) + r * Vec3::new(0.0, 0.2, 0.0),
                rotation: r,
                width: 1.0,
            }
        };
        assert!(!cuts(&a, 0.5, &up(-10.0), 0.4));
        assert!(!cuts(&a, 0.5, &up(2.0), 0.4));
    }

    /// How much of one monitor another is seen over or behind, and hides: a small one
    /// halfway to the eye covers twice its size of the one behind, and all of itself is
    /// seen against it; side by side, hinged, nothing; behind the eye, nothing.
    #[test]
    fn what_one_is_seen_over() {
        let a = upright(0.0, -1.0, 0.0, 1.0);
        let small = upright(0.0, -0.5, 0.0, 0.2);
        let close = |x: f64, y: f64| (x - y).abs() < 1e-4;
        assert!(close(
            seen_over(&a, 0.5, &small, 0.1, Vec3::ZERO),
            0.4 * 0.2 / 0.5
        ));
        assert!(close(seen_over(&small, 0.1, &a, 0.5, Vec3::ZERO), 1.0));
        // The small one is in front: it hides that much of `a`, `a` none of it.
        assert!(close(
            hidden_by(&a, 0.5, &small, 0.1, Vec3::ZERO),
            0.4 * 0.2 / 0.5
        ));
        assert_eq!(hidden_by(&small, 0.1, &a, 0.5, Vec3::ZERO), 0.0);
        let turn = 0.5f32;
        let hinged = upright(0.5 + 0.5 * turn.cos(), -1.0 + 0.5 * turn.sin(), -turn, 1.0);
        assert!(seen_over(&a, 0.5, &hinged, 0.5, Vec3::ZERO) < 1e-6);
        assert!(seen_over(&hinged, 0.5, &a, 0.5, Vec3::ZERO) < 1e-6);
        let behind = upright(0.0, 1.0, std::f32::consts::PI, 1.0);
        assert_eq!(seen_over(&a, 0.5, &behind, 0.5, Vec3::ZERO), 0.0);
        // A wall a quarter of a metre left of the eye, from 2 m ahead to 2 m behind: only
        // the part ahead is seen against `a`, a wedge of its left side narrowing to the
        // wall's far end (0.172 of its 0.5 square metres).
        let wall = Placement {
            position: Vec3::new(-0.25, 0.0, 0.0),
            rotation: Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
            width: 4.0,
        };
        assert!(close(
            seen_over(&a, 0.5, &wall, 0.5, Vec3::ZERO),
            0.171875 / 0.5
        ));
    }
}
