//! The arrangement as its files keep it: `layout.txt` (the desk) and `keyboard.txt`.

use std::collections::{BTreeMap, HashMap};

use glam::{Quat, Vec3};

use super::{Key, Placement};
use crate::ids::{DisplayKey, PcId};
use crate::persist::{self, LoadError, Skipped};

/// The desk arrangement: where the user put the monitors and the PCs' own displays.
/// Read from `layout.txt` once at the start and held in memory from then on (the file
/// follows it and is never read again). Cinema mode shows its own one-screen view
/// instead, and that view never takes the desk arrangement's place here.
#[derive(Default)]
pub struct Desk {
    pub(super) saved: Arrangement,
    /// The screen cinema shows (see [`cinema_choice`]): one picked in the menu.
    cinema: Option<DisplayKey>,
    /// The rows each PC's desktop was last laid out by, top to bottom and each left to
    /// right: its monitors stay neighbours until clearly moved apart, after a restart
    /// too (see `layout::Desktop::history`).
    rows: BTreeMap<PcId, Vec<Vec<DisplayKey>>>,
}

/// A stream's size in pixels, width then height.
pub type PixelSize = (u32, u32);

/// An arrangement as `layout.txt` keeps the desk one, of the displays of every PC.
#[derive(Clone)]
pub struct Arrangement<K = DisplayKey> {
    pub placements: HashMap<K, Placement>,
    /// The stream size each placement was made for.
    pub sizes: HashMap<K, PixelSize>,
    /// The eye and the yaw the arrangement was laid out around.
    pub center: Option<(Vec3, f32)>,
    /// The circle it stands on.
    pub radius: Option<f32>,
    /// How far its frame is tilted up (see [`frame`]): the arrangement's, not the
    /// preset's, so a profile laid out lying down is seen so whatever the preset.
    pub tilt: f32,
}

impl<K> Default for Arrangement<K> {
    fn default() -> Self {
        Self {
            placements: HashMap::new(),
            sizes: HashMap::new(),
            center: None,
            radius: None,
            tilt: 0.0,
        }
    }
}

impl Desk {
    /// The arrangement `layout.txt` holds (of monitors and the PCs' own displays:
    /// window panels are placed afresh when they appear, and never saved):
    ///
    /// ```text
    /// format 2
    /// center 0 1.6 0 0.25
    /// radius 1.3
    /// tilt 0
    /// cinema 0123456789abcdef:3
    /// rows 0123456789abcdef:3 0123456789abcdef:1 / 0123456789abcdef:2
    /// 0123456789abcdef:3 -0.6 1.6 -1.2 0 0.2 0 0.98 0.77 2560x1440
    /// ```
    ///
    /// A `rows` line is the rows one PC's desktop was last laid out by, top to bottom
    /// (`/` between them), each left to right.
    ///
    /// A file without the format line is not read at all. Within, a line this
    /// version does not know, or one whose value it cannot read, is passed over (and
    /// counted in what comes back): a display without its line is placed afresh, the
    /// center and the circle without theirs are the usual ones.
    pub fn parse(text: &str) -> Result<(Self, Skipped), LoadError> {
        let mut saved = Arrangement::default();
        let mut cinema = None;
        let mut rows = BTreeMap::new();
        let mut tilt_read = false;
        let mut skipped = Skipped::default();
        for (n, line) in persist::body(text)? {
            if line.is_empty() {
                continue;
            }
            let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
            let read = match word {
                "center" => {
                    saved.center.is_none() && {
                        saved.center = parse_center(rest);
                        saved.center.is_some()
                    }
                }
                "radius" => {
                    saved.radius.is_none() && {
                        saved.radius = parse_radius(rest);
                        saved.radius.is_some()
                    }
                }
                "tilt" => {
                    !tilt_read && {
                        tilt_read = true;
                        parse_tilt(rest).map(|t| saved.tilt = t).is_some()
                    }
                }
                "cinema" => {
                    cinema.is_none() && {
                        cinema = DisplayKey::parse(rest);
                        cinema.is_some()
                    }
                }
                "rows" => parse_rows(rest)
                    .filter(|(pc, _)| !rows.contains_key(pc))
                    .map(|(pc, r)| rows.insert(pc, r))
                    .is_some(),
                _ => DisplayKey::parse(word)
                    .filter(|key| !saved.placements.contains_key(key))
                    .zip(parse_place(rest, true))
                    .map(|(key, (p, size))| {
                        saved.placements.insert(key, p);
                        if let Some(size) = size {
                            saved.sizes.insert(key, size);
                        }
                    })
                    .is_some(),
            };
            if !read {
                skipped.line(n);
            }
        }
        Ok((
            Self {
                saved,
                cinema,
                rows,
            },
            skipped,
        ))
    }

    /// The desk arrangement as last kept.
    pub fn arrangement(&self) -> &Arrangement {
        &self.saved
    }

    /// Keeps `placements` (made for the stream `sizes`, around `center` with its frame
    /// tilted up by `tilt`, on the circle of `radius`) as the desk arrangement, unless
    /// they are the cinema view; true when
    /// kept, and `layout.txt` is to be written. What is kept is what the file says:
    /// a size only for a placement. Its maps keep their memory from one time to the
    /// next.
    pub fn keep(
        &mut self,
        placements: impl IntoIterator<Item = (DisplayKey, Placement)>,
        sizes: &HashMap<DisplayKey, PixelSize>,
        (center, radius, tilt): ((Vec3, f32), f32, f32),
        cinema: bool,
    ) -> bool {
        if cinema {
            return false;
        }
        let saved = &mut self.saved;
        saved.placements.clear();
        // Only what the file can say: a place that is not one (a float gone wrong) is
        // left out, and the display is placed afresh next time.
        saved
            .placements
            .extend(placements.into_iter().filter(|(_, p)| placed_well(p)));
        saved.sizes.clear();
        saved.sizes.extend(
            sizes
                .iter()
                .filter(|(id, size)| saved.placements.contains_key(id) && size.0 > 0 && size.1 > 0)
                .map(|(id, size)| (*id, *size)),
        );
        saved.center = Some(center).filter(|(eye, yaw)| eye.is_finite() && yaw.is_finite());
        saved.radius = Some(radius).filter(|r| r.is_finite() && *r > 0.1);
        saved.tilt = upright(tilt);
        true
    }

    /// The screen cinema shows, as picked in the menu.
    pub fn cinema(&self) -> Option<DisplayKey> {
        self.cinema
    }

    /// Cinema shows `screen` from now on; true when that is a change, and
    /// `layout.txt` is to be written.
    pub fn pick_cinema(&mut self, screen: Option<DisplayKey>) -> bool {
        std::mem::replace(&mut self.cinema, screen) != screen
    }

    /// Forgets the places of the displays `gone`, and the cinema's pick if it is one of
    /// them, and takes them out of their rows; true when it had one of them, and
    /// `layout.txt` is to be written.
    pub fn forget(&mut self, gone: &[DisplayKey]) -> bool {
        let saved = &mut self.saved;
        let before = saved.placements.len();
        saved.placements.retain(|k, _| !gone.contains(k));
        saved.sizes.retain(|k, _| !gone.contains(k));
        let picked = self.cinema.is_some_and(|k| gone.contains(&k));
        if picked {
            self.cinema = None;
        }
        let mut in_rows = false;
        for rows in self.rows.values_mut() {
            for row in rows.iter_mut() {
                let had = row.len();
                row.retain(|k| !gone.contains(k));
                in_rows |= row.len() != had;
            }
            rows.retain(|r| !r.is_empty());
        }
        self.rows.retain(|_, r| !r.is_empty());
        saved.placements.len() != before || picked || in_rows
    }

    /// The rows PC `pc`'s desktop was last laid out by (empty: none kept).
    pub fn rows(&self, pc: PcId) -> &[Vec<DisplayKey>] {
        self.rows.get(&pc).map_or(&[], Vec::as_slice)
    }

    /// PC `pc`'s desktop is laid out by `rows` now; true when that is a change, and
    /// `layout.txt` is to be written.
    pub fn keep_rows(&mut self, pc: PcId, rows: &[Vec<DisplayKey>]) -> bool {
        let rows = rows.iter().filter(|r| !r.is_empty());
        let same = self.rows(pc).iter().eq(rows.clone());
        if !same {
            let kept = self.rows.entry(pc).or_default();
            kept.clear();
            kept.extend(rows.cloned());
            if kept.is_empty() {
                self.rows.remove(&pc);
            }
        }
        !same
    }

    /// The text of `layout.txt` (see [`Desk::parse`]), added to `out` (a buffer kept
    /// between saves).
    pub fn write_text(&self, out: &mut String) {
        use std::fmt::Write as _;
        let saved = &self.saved;
        persist::write_format(out);
        if let Some((eye, yaw)) = saved.center {
            let _ = writeln!(out, "center {} {} {} {yaw}", eye.x, eye.y, eye.z);
        }
        if let Some(radius) = saved.radius {
            let _ = writeln!(out, "radius {radius}");
        }
        if saved.center.is_some() {
            let _ = writeln!(out, "tilt {}", saved.tilt);
        }
        if let Some(screen) = self.cinema {
            let _ = writeln!(out, "cinema {screen}");
        }
        for rows in self.rows.values() {
            out.push_str("rows");
            for (i, row) in rows.iter().enumerate() {
                if i > 0 {
                    out.push_str(" /");
                }
                for key in row {
                    let _ = write!(out, " {key}");
                }
            }
            out.push('\n');
        }
        for key in ascending(&saved.placements) {
            if let Some(p) = saved.placements.get(&key) {
                let _ = write!(out, "{key}");
                write_place(out, p, saved.sizes.get(&key).copied());
            }
        }
    }
}

/// `k k / k`: the rows of one PC's desktop (see [`Desk::parse`]), each display once,
/// all of that PC.
fn parse_rows(text: &str) -> Option<(PcId, Vec<Vec<DisplayKey>>)> {
    let rows: Vec<Vec<DisplayKey>> = text
        .split(" / ")
        .map(|row| {
            row.split(' ')
                .map(DisplayKey::parse)
                .collect::<Option<Vec<_>>>()
        })
        .collect::<Option<_>>()?;
    let pc = rows.first()?.first()?.pc;
    let mut seen: Vec<DisplayKey> = rows.iter().flatten().copied().collect();
    let count = seen.len();
    seen.sort_unstable();
    seen.dedup();
    let one_pc = seen.iter().all(|k| k.pc == pc);
    (one_pc && seen.len() == count && rows.iter().all(|r| !r.is_empty())).then_some((pc, rows))
}

/// Where the keyboard window lies, as `keyboard.txt` keeps it (none: placed in front
/// of the user when it shows):
///
/// ```text
/// format 2
/// keyboard 0 0.8 -0.4 -0.71 0 0 0.71 0.42
/// ```
pub fn parse_keyboard(text: &str) -> Result<(Option<Placement>, Skipped), LoadError> {
    let mut place = None;
    let mut skipped = Skipped::default();
    for (n, line) in persist::body(text)? {
        if line.is_empty() {
            continue;
        }
        let p = line
            .strip_prefix("keyboard ")
            .and_then(|rest| parse_place(rest, false))
            .filter(|_| place.is_none());
        match p {
            Some((p, _)) => place = Some(p),
            None => skipped.line(n),
        }
    }
    Ok((place, skipped))
}

/// The text of `keyboard.txt` for the window at `place` (see [`parse_keyboard`]),
/// added to `out`.
pub fn write_keyboard(out: &mut String, place: &Placement) {
    persist::write_format(out);
    out.push_str("keyboard");
    write_place(out, place, None);
}

/// The keys of `map` from the smallest up, without a sorted copy (an arrangement has
/// a handful).
pub(super) fn ascending<K: Key, V>(map: &HashMap<K, V>) -> impl Iterator<Item = K> + '_ {
    let mut last: Option<K> = None;
    std::iter::from_fn(move || {
        let next = map
            .keys()
            .copied()
            .filter(|id| last.is_none_or(|l| *id > l))
            .min()?;
        last = Some(next);
        Some(next)
    })
}

/// ` x y z qx qy qz qw width`, then the stream size it was placed for (` 2560x1440`)
/// when known, and the end of the line, added to `out`.
pub(crate) fn write_place(out: &mut String, p: &Placement, size: Option<PixelSize>) {
    use std::fmt::Write as _;
    let (pos, q) = (p.position, p.rotation);
    let _ = write!(
        out,
        " {} {} {} {} {} {} {} {}",
        pos.x, pos.y, pos.z, q.x, q.y, q.z, q.w, p.width
    );
    if let Some((w, h)) = size {
        let _ = write!(out, " {w}x{h}");
    }
    out.push('\n');
}

/// `x y z qx qy qz qw width`, then (when `sized`) the stream size it was placed for
/// if there is one (`2560x1440`), as [`write_place`] writes them: a position and a
/// turn that are numbers, a width above nothing.
pub(crate) fn parse_place(text: &str, sized: bool) -> Option<(Placement, Option<PixelSize>)> {
    let mut words = text.split(' ');
    let n = numbers::<8>(&mut words)?;
    let size = match words.next() {
        None => None,
        Some(w) if sized => {
            let (w, h) = w.split_once('x')?;
            let size: PixelSize = (w.parse().ok()?, h.parse().ok()?);
            if size.0 == 0 || size.1 == 0 {
                return None;
            }
            Some(size)
        }
        Some(_) => return None,
    };
    if words.next().is_some() {
        return None;
    }
    let p = Placement {
        position: Vec3::new(n[0], n[1], n[2]),
        rotation: Quat::from_xyzw(n[3], n[4], n[5], n[6]).normalize(),
        width: n[7],
    };
    placed_well(&p).then_some((p, size))
}

/// A place the files can keep: a position and a turn that are numbers, a width above
/// nothing.
pub(crate) fn placed_well(p: &Placement) -> bool {
    p.position.is_finite()
        && p.rotation.is_finite()
        && p.rotation.length_squared() > 1e-6
        && p.width.is_finite()
        && p.width > 0.0
}

/// The next `N` words as numbers, if they are.
fn numbers<'a, const N: usize>(words: &mut impl Iterator<Item = &'a str>) -> Option<[f32; N]> {
    let mut out = [0.0; N];
    for n in &mut out {
        *n = words.next()?.parse().ok()?;
    }
    Some(out)
}

/// `x y z yaw`: where the arrangement was laid out around.
pub(crate) fn parse_center(text: &str) -> Option<(Vec3, f32)> {
    let mut words = text.split(' ');
    let [x, y, z, yaw] = numbers::<4>(&mut words)?;
    let eye = Vec3::new(x, y, z);
    (words.next().is_none() && eye.is_finite() && yaw.is_finite()).then_some((eye, yaw))
}

/// A tilt up of a frame, in radians: from straight down to straight up.
pub(crate) fn parse_tilt(text: &str) -> Option<f32> {
    let tilt: f32 = text.parse().ok()?;
    (-std::f32::consts::FRAC_PI_2..=std::f32::consts::FRAC_PI_2)
        .contains(&tilt)
        .then_some(tilt)
}

/// `tilt` as a frame can be tilted: a number from straight down to straight up (none
/// at all is no tilt).
pub(crate) fn upright(tilt: f32) -> f32 {
    let up = std::f32::consts::FRAC_PI_2;
    if tilt.is_finite() {
        tilt.clamp(-up, up)
    } else {
        0.0
    }
}

/// A circle's radius: a number over 10 cm.
pub(crate) fn parse_radius(text: &str) -> Option<f32> {
    text.parse::<f32>()
        .ok()
        .filter(|r| r.is_finite() && *r > 0.1)
}
