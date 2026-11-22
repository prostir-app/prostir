//! Profiles: saved arrangements, switched to with one press. A profile keeps which
//! monitors there are (their sizes and scales, the primary one, the PC's own displays
//! shown) and where they stand, nothing more: cinema, the background, the curve and
//! the preset are settings over whatever profile is on, and stay as they are when one
//! is switched to. A list of up to [`MAX_PROFILES`] in the order the menu shows them,
//! each with the name given it from the PC, or none (the menu then calls it by its
//! number, "Profile 2").

#![forbid(unsafe_code)]

use glam::Vec3;
use prostir_layout::space::Frame;
use prostir_proto::state::{MAX_DISPLAY_NAME, Mode};

use crate::ids::{DisplayKey, PcId};
use crate::layout::{self, Arrangement, Key, Placement};
use crate::persist::{self, LoadError, Skipped};

/// Profiles kept at most: more than a menu can list without getting in the way.
pub const MAX_PROFILES: usize = 20;
/// Monitors in a profile: more than any PC makes (it answers with its own limit).
const MAX_MONITORS: usize = 10;

/// The profiles, in the menu's order.
pub type Slots = Vec<Profile>;

/// No profile saved.
pub fn empty() -> Slots {
    Vec::new()
}

/// A name given to a profile, as it is kept: on one line, without spaces around it,
/// at most as long as the protocol carries; none when nothing is left.
pub fn clean_name(name: &str) -> Option<String> {
    let one_line: String = name
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let name = prostir_proto::state::clip(one_line.trim(), MAX_DISPLAY_NAME);
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

/// The profile a command names (`2`, `Profile 2`, `Профіль 2`, or the name it was
/// given): its place in the list. A name given to a profile counts first.
pub fn find(profiles: &Slots, named: &str) -> Option<usize> {
    let named = named.trim();
    profiles
        .iter()
        .position(|p| {
            p.name
                .as_deref()
                .is_some_and(|n| n.to_lowercase() == named.to_lowercase())
        })
        .or_else(|| {
            named
                .split_whitespace()
                .next_back()?
                .parse::<usize>()
                .ok()
                .filter(|n| (1..=profiles.len()).contains(n))
                .map(|n| n - 1)
        })
}

#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    /// The name given to it (from the PC); none: called by its number.
    pub name: Option<String>,
    /// The monitors, left to right: their sizes and scales.
    pub monitors: Vec<Mode>,
    /// Index into `monitors`.
    pub primary: u8,
    /// The displays of the PCs themselves that were shown (a laptop's panel, say).
    pub shown: Vec<DisplayKey>,
    /// Where each monitor stood, by its place in `monitors`: all of them or none (saved
    /// before every monitor had a place: the preset lays them out). With a `frame`,
    /// in the profile's own frame; without one, in the room.
    pub places: Vec<Placement>,
    /// The frame `places` are in, and the circle they stand on.
    pub frame: Option<ProfileFrame>,
    /// The rows the PC had, top to bottom and each left to right, by place in
    /// `monitors` (the PC's own displays are not among them): which monitors stay
    /// neighbours while they are moved about (see `prostir_layout::rows`). Only with
    /// places.
    pub rows: Vec<Vec<usize>>,
}

/// The frame a profile's places are in: the eye at the origin, straight ahead -Z,
/// tilted up by `tilt` (lying down the arrangement is a band overhead, seen from
/// below), and the circle around it they stand on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProfileFrame {
    /// Radians.
    pub tilt: f32,
    pub radius: f32,
}

impl Profile {
    /// The setup as it stands: `monitors` (number, size and scale) left to right, the
    /// `primary` one, the PC's own displays `shown`, the arrangement `live` with the
    /// `rows` the PC has (by number), seen from its center tilted up as it is. In
    /// cinema `desk` is the desk arrangement kept in memory before the cinema view
    /// replaced it, and that is what is saved. Only what the file can say is kept: as
    /// many monitors as any PC makes, places that are places (else none), a center that
    /// is one (else the places as they are in the room).
    pub fn of<K: Key>(
        monitors: &[(K, Mode)],
        primary: Option<K>,
        shown: Vec<DisplayKey>,
        live: &Arrangement<K>,
        desk: Option<&Arrangement<K>>,
        rows: &[Vec<K>],
    ) -> Self {
        let monitors = monitors.get(..MAX_MONITORS).unwrap_or(monitors);
        let arrangement = desk.unwrap_or(live);
        let center = arrangement
            .center
            .filter(|(eye, yaw)| eye.is_finite() && yaw.is_finite());
        let tilt = layout::upright(arrangement.tilt);
        let frame = center.map(|(eye, yaw)| Frame::tilted(eye, yaw, tilt));
        let places: Option<Vec<Placement>> = monitors
            .iter()
            .map(|m| {
                let p = *arrangement.placements.get(&m.0)?;
                let p = frame.map_or(p, |f| f.unplace(p));
                layout::placed_well(&p).then_some(p)
            })
            .collect();
        let index = |id: &K| monitors.iter().position(|m| m.0 == *id);
        let rows = match places {
            Some(_) => rows
                .iter()
                .map(|r| r.iter().filter_map(index).collect::<Vec<usize>>())
                .filter(|r| !r.is_empty())
                .collect(),
            None => Vec::new(),
        };
        let radius = arrangement
            .radius
            .filter(|r| r.is_finite() && *r > 0.1)
            .unwrap_or(layout::RADIUS);
        Self {
            name: None,
            primary: primary
                .and_then(|p| index(&p))
                .and_then(|i| u8::try_from(i).ok())
                .unwrap_or(0),
            monitors: monitors
                .iter()
                .map(|m| Mode {
                    w: m.1.w.max(1),
                    h: m.1.h.max(1),
                    scale: m.1.scale.max(1),
                })
                .collect(),
            shown,
            frame: center
                .filter(|_| places.is_some())
                .map(|_| ProfileFrame { tilt, radius }),
            places: places.unwrap_or_default(),
            rows,
        }
    }

    /// The profile's section of `profiles.txt` (after its `[slot]` line), added to
    /// `out`.
    fn write_text(&self, out: &mut String) {
        use std::fmt::Write as _;
        if let Some(name) = &self.name {
            let _ = writeln!(out, "name {name}");
        }
        out.push_str("monitors");
        for (i, m) in self.monitors.iter().enumerate() {
            let star = if i == usize::from(self.primary) {
                "*"
            } else {
                ""
            };
            let _ = write!(out, " {star}{}x{}", m.w, m.h);
            if m.scale != 100 {
                let _ = write!(out, "@{}", m.scale);
            }
        }
        out.push('\n');
        for k in &self.shown {
            let _ = writeln!(out, "shown {k}");
        }
        for (i, p) in self.places.iter().enumerate() {
            let _ = write!(out, "place {i}");
            layout::write_place(out, p, None);
        }
        if let Some(f) = self.frame.filter(|_| !self.places.is_empty()) {
            let _ = writeln!(out, "frame {} {}", f.tilt, f.radius);
        }
        if !self.rows.is_empty() {
            out.push_str("rows ");
            for (n, row) in self.rows.iter().enumerate() {
                if n > 0 {
                    out.push_str(" / ");
                }
                for (k, i) in row.iter().enumerate() {
                    let space = if k > 0 { " " } else { "" };
                    let _ = write!(out, "{space}{i}");
                }
            }
            out.push('\n');
        }
    }
}

/// PC `pc` was forgotten: no profile shows its displays any more. True when one did,
/// and `profiles.txt` is to be written.
pub fn forget_pc(profiles: &mut Slots, pc: PcId) -> bool {
    let mut changed = false;
    for p in profiles.iter_mut() {
        let before = p.shown.len();
        p.shown.retain(|k| k.pc != pc);
        changed |= p.shown.len() != before;
    }
    changed
}

/// How a line of a profile's section was taken.
enum Took {
    Yes,
    /// Not a line this version knows: passed over.
    Unknown,
    /// A line it knows, with a value it cannot read (or a second one): the profile is
    /// not read.
    Bad,
}

/// A profile's lines as they are read, before they are checked together.
#[derive(Default)]
struct Draft {
    name: Option<String>,
    monitors: Option<(Vec<Mode>, u8)>,
    shown: Vec<DisplayKey>,
    places: Vec<(usize, Placement)>,
    frame: Option<ProfileFrame>,
    rows: Option<Vec<Vec<usize>>>,
    /// A line it knows was not read: the profile is not read.
    bad: bool,
}

impl Draft {
    /// Takes one line of the section.
    fn line(&mut self, line: &str) -> Took {
        let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
        let read = match word {
            "name" => {
                self.name.is_none() && {
                    self.name = clean_name(rest);
                    self.name.is_some()
                }
            }
            "monitors" => {
                self.monitors.is_none() && {
                    self.monitors = parse_monitors(rest);
                    self.monitors.is_some()
                }
            }
            "shown" => DisplayKey::parse(rest)
                .filter(|k| !self.shown.contains(k))
                .map(|k| self.shown.push(k))
                .is_some(),
            "place" => rest
                .split_once(' ')
                .and_then(|(i, p)| {
                    Some((i.parse::<usize>().ok()?, layout::parse_place(p, false)?.0))
                })
                .filter(|(i, _)| !self.places.iter().any(|p| p.0 == *i))
                .map(|place| self.places.push(place))
                .is_some(),
            "frame" => {
                self.frame.is_none() && {
                    self.frame = parse_frame(rest);
                    self.frame.is_some()
                }
            }
            "rows" => {
                self.rows.is_none() && {
                    self.rows = parse_rows(rest);
                    self.rows.is_some()
                }
            }
            _ => return Took::Unknown,
        };
        if read { Took::Yes } else { Took::Bad }
    }

    /// The profile its lines make, if they make one: its monitors (rows of monitors it
    /// has, each once), and a place for each or none. A frame without places, or rows
    /// without them, say nothing and go.
    fn profile(mut self) -> Option<Profile> {
        let (monitors, primary) = self.monitors.filter(|_| !self.bad)?;
        let n = monitors.len();
        self.places.sort_by_key(|p| p.0);
        if !self.places.iter().map(|p| p.0).eq(0..n) {
            self.places.clear();
        }
        let rows = self
            .rows
            .filter(|_| !self.places.is_empty())
            .unwrap_or_default();
        let mut seen: Vec<usize> = rows.iter().flatten().copied().collect();
        seen.sort_unstable();
        if seen.windows(2).any(|w| w[0] == w[1]) || seen.last().is_some_and(|&i| i >= n) {
            return None;
        }
        Some(Profile {
            name: self.name,
            monitors,
            primary,
            shown: self.shown,
            frame: self.frame.filter(|_| !self.places.is_empty()),
            places: self.places.into_iter().map(|p| p.1).collect(),
            rows,
        })
    }
}

/// `2560x1440 *2560x1440@150 1440x2560`: the monitors left to right, the primary one
/// starred, a scale other than 100 % after `@`.
fn parse_monitors(text: &str) -> Option<(Vec<Mode>, u8)> {
    let mut monitors = Vec::new();
    let mut primary = None;
    for (i, m) in text.split(' ').enumerate() {
        let m = match m.strip_prefix('*') {
            Some(m) if primary.is_none() => {
                primary = Some(u8::try_from(i).ok()?);
                m
            }
            Some(_) => return None,
            None => m,
        };
        let (size, scale) = match m.split_once('@') {
            Some((size, scale)) => (size, scale.parse::<u16>().ok().filter(|s| *s > 0)?),
            None => (m, 100),
        };
        let (w, h) = size.split_once('x')?;
        monitors.push(Mode {
            w: w.parse().ok().filter(|w| *w > 0)?,
            h: h.parse().ok().filter(|h| *h > 0)?,
            scale,
        });
    }
    (monitors.len() <= MAX_MONITORS).then_some(())?;
    Some((monitors, primary?))
}

/// `tilt radius`: a tilt between straight down and straight up, a circle.
fn parse_frame(text: &str) -> Option<ProfileFrame> {
    let (tilt, radius) = text.split_once(' ')?;
    Some(ProfileFrame {
        tilt: layout::parse_tilt(tilt)?,
        radius: layout::parse_radius(radius)?,
    })
}

/// `0 1 2 / 3 4`: rows of places in a profile's monitors.
fn parse_rows(text: &str) -> Option<Vec<Vec<usize>>> {
    text.split(" / ")
        .map(|r| {
            r.split(' ')
                .map(|i| i.parse().ok())
                .collect::<Option<Vec<usize>>>()
        })
        .collect()
}

/// A profile's monitors once the PC made them: `made` pairs the numbers the PC gave
/// them with the profile's places, in the order of its monitors. In the profile's
/// `frame`, each stands around the user's center `now` (eye and yaw), tilted as it
/// was, as it stood around the center it was saved with, so whatever is worked out as
/// seen from the eye (the rows the PC gets, "Line up", "In front", the magnet) sees
/// the profile as it was; without a frame, where it was in the room. And its `rows`
/// (by place in its monitors) by the PC's numbers.
pub fn standing<K: Key>(
    made: &[(K, Placement)],
    frame: Option<ProfileFrame>,
    rows: &[Vec<usize>],
    now: (Vec3, f32),
) -> (Vec<(K, Placement)>, Vec<Vec<K>>) {
    let around = frame.map(|f| Frame::tilted(now.0, now.1, f.tilt));
    let placed = made
        .iter()
        .map(|(id, p)| {
            let mut p = around.map_or(*p, |f| f.place(*p));
            p.rotation = p.rotation.normalize();
            (*id, p)
        })
        .collect();
    let rows = rows
        .iter()
        .map(|r| r.iter().filter_map(|&i| made.get(i).map(|m| m.0)).collect())
        .filter(|r: &Vec<K>| !r.is_empty())
        .collect();
    (placed, rows)
}

/// The profiles `profiles.txt` holds, in order:
///
/// ```text
/// format 2
///
/// [0]
/// name Work
/// monitors 2560x1440 *2560x1440@150 1440x2560
/// shown 0123456789abcdef:100
/// place 0 -1.1 0 -0.9 0 0.38 0 0.92 0.77
/// place 1 0 0 -1.2 0 0 0 1 0.77
/// place 2 1 0 -0.95 0 -0.36 0 0.93 0.43
/// frame 0 1.3
/// rows 0 1 2
/// ```
///
/// The numbers of the sections give the order (files of the builds with three
/// slots may skip a number). A file without the format line is not read at all.
/// Within, a line this version does not know is passed over; a profile with a line it
/// knows but cannot read (or a second `[n]` of the same number) is not read, and the
/// others are; past [`MAX_PROFILES`], the last ones are not read. What was passed over
/// comes back counted.
pub fn parse(text: &str) -> Result<(Slots, Skipped), LoadError> {
    let mut read = std::collections::BTreeMap::<usize, (usize, Profile)>::new();
    let mut skipped = Skipped::default();
    // The section being read: its number and the line it starts on; `None` for lines
    // that belong to no section that is read.
    let mut current: Option<(usize, usize, Draft)> = None;
    let finish = |section: Option<(usize, usize, Draft)>,
                  read: &mut std::collections::BTreeMap<usize, (usize, Profile)>,
                  skipped: &mut Skipped| {
        if let Some((slot, n, draft)) = section {
            match draft.profile() {
                Some(profile) => {
                    read.insert(slot, (n, profile));
                }
                None => skipped.line(n),
            }
        }
    };
    let mut numbers = std::collections::BTreeSet::new();
    for (n, line) in persist::body(text)? {
        if line.is_empty() {
            continue;
        }
        if let Some(slot) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            finish(current.take(), &mut read, &mut skipped);
            let slot = slot.parse::<usize>().ok().filter(|i| numbers.insert(*i));
            match slot {
                Some(slot) => current = Some((slot, n, Draft::default())),
                None => skipped.line(n),
            }
            continue;
        }
        match &mut current {
            Some((_, _, draft)) => match draft.line(line) {
                Took::Yes => {}
                Took::Unknown => skipped.line(n),
                Took::Bad => draft.bad = true,
            },
            None => skipped.line(n),
        }
    }
    finish(current, &mut read, &mut skipped);
    let mut out = empty();
    for (n, profile) in read.into_values() {
        if out.len() < MAX_PROFILES {
            out.push(profile);
        } else {
            skipped.line(n);
        }
    }
    Ok((out, skipped))
}

/// The text of `profiles.txt` (see [`parse`]), added to `out` (a buffer kept between
/// saves).
pub fn write_text(out: &mut String, profiles: &Slots) {
    use std::fmt::Write as _;
    persist::write_format(out);
    for (i, p) in profiles.iter().enumerate() {
        let _ = writeln!(out, "\n[{i}]");
        p.write_text(out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Quat;
    use prostir_proto::state::DisplayId;

    fn text_of(profiles: &Slots) -> String {
        let mut text = String::new();
        write_text(&mut text, profiles);
        text
    }

    /// `text` read, with nothing passed over.
    fn read(text: &str) -> Slots {
        let (slots, skipped) = parse(text).unwrap();
        assert!(skipped.is_empty(), "{skipped:?} in {text:?}");
        slots
    }

    fn place(x: f32) -> Placement {
        Placement {
            position: Vec3::new(x, 0.5, -1.25),
            rotation: Quat::IDENTITY,
            width: 1.2,
        }
    }

    fn sample() -> Profile {
        let laptop = DisplayKey::new(PcId([0xab; 8]), DisplayId(100));
        Profile {
            name: None,
            monitors: vec![
                Mode::new(2560, 1440),
                Mode {
                    scale: 150,
                    ..Mode::new(2560, 1440)
                },
                Mode::new(1440, 2560),
            ],
            primary: 1,
            shown: vec![laptop],
            places: vec![place(-1.0), place(0.0), place(1.0)],
            frame: Some(ProfileFrame {
                tilt: 0.0,
                radius: 1.3,
            }),
            rows: vec![vec![0, 1], vec![2]],
        }
    }

    /// The text of the file: the format line, then each profile in order after an
    /// empty line, its name first where it has one.
    #[test]
    fn the_text_of_the_file() {
        let mut profiles = empty();
        assert_eq!(text_of(&profiles), "format 2\n");
        profiles.push(Profile {
            name: Some("Робота вдома".into()),
            ..sample()
        });
        let place = "-1 0.5 -1.25 0 0 0 1 1.2";
        assert_eq!(
            text_of(&profiles),
            format!(
                "format 2\n\n[0]\nname Робота вдома\nmonitors 2560x1440 *2560x1440@150 1440x2560\n\
                 shown abababababababab:100\n\
                 place 0 {place}\nplace 1 0 0.5 -1.25 0 0 0 1 1.2\nplace 2 1 0.5 -1.25 0 0 0 1 1.2\n\
                 frame 0 1.3\nrows 0 1 / 2\n"
            )
        );
    }

    /// A name is kept on one line, without spaces around it, as long as the protocol
    /// carries; an empty one is none.
    #[test]
    fn names_are_kept_clean() {
        assert_eq!(clean_name("  Робота\nвдома\t"), Some("Робота вдома".into()));
        assert_eq!(clean_name(" \n "), None);
        let long = clean_name(&"ї".repeat(100)).unwrap();
        assert_eq!(long.len(), MAX_DISPLAY_NAME);
    }

    /// The files of the builds with three slots, numbers skipped, read in their order;
    /// more than [`MAX_PROFILES`]: the last ones are passed over.
    #[test]
    fn slots_become_a_list() {
        let good = "monitors *2560x1440\n";
        let read_back = read(&format!("format 2\n\n[2]\nname Ігри\n{good}\n[0]\n{good}"));
        assert_eq!(read_back.len(), 2);
        assert_eq!(read_back[0].name, None);
        assert_eq!(read_back[1].name.as_deref(), Some("Ігри"));
        let many = (0..MAX_PROFILES + 2).fold(String::new(), |mut all, i| {
            all.push('[');
            all.push_str(&i.to_string());
            all.push_str("]\n");
            all.push_str(good);
            all
        });
        let (profiles, skipped) = parse(&format!("format 2\n{many}")).unwrap();
        assert_eq!(profiles.len(), MAX_PROFILES);
        assert_eq!(skipped.count, 2);
    }

    #[test]
    fn roundtrip() {
        let mut profiles = empty();
        profiles.push(sample());
        profiles.push(Profile {
            name: Some("Кіно".into()),
            monitors: vec![Mode::new(1920, 1080)],
            primary: 0,
            shown: Vec::new(),
            places: vec![Placement {
                position: Vec3::new(0.0, 0.5, -1.0),
                rotation: Quat::from_rotation_y(0.25),
                width: 1.2,
            }],
            frame: Some(ProfileFrame {
                tilt: layout::OVERHEAD_PITCH,
                radius: 1.35,
            }),
            rows: Vec::new(),
        });
        let back = read(&text_of(&profiles));
        assert_eq!(back[0], profiles[0]);
        let (a, b) = (&back[1], &profiles[1]);
        assert_eq!(a.name, b.name);
        assert!(a.places[0].rotation.angle_between(b.places[0].rotation) < 1e-4);
        assert_eq!(a.frame, b.frame);
        // A profile saved before its monitors had places: none, no frame, no rows.
        let mut unplaced = sample();
        unplaced.places.clear();
        unplaced.rows.clear();
        unplaced.frame = None;
        let text = text_of(&vec![unplaced.clone()]);
        assert!(!text.contains("frame"));
        assert_eq!(read(&text)[0], unplaced);
        // Places without a frame: where they were in the room.
        let mut in_room = sample();
        in_room.frame = None;
        let text = text_of(&vec![sample(), in_room.clone()]);
        assert_eq!(read(&text)[1], in_room);
    }

    /// Files of 0.1 and of the builds before this format are not read at all. Within
    /// one, a line this version does not know is passed over and the rest read as
    /// without it; a profile with a line it knows but cannot read is not read, and
    /// the others are; lines before any slot, a slot twice or out of range are passed
    /// over too. A file of another system's line ends reads the same.
    #[test]
    fn what_is_not_read() {
        let old = "[0]\nmonitors 2560x1440 *1440x2560\nmirror on\npreset wall\n";
        assert_eq!(parse(old).err(), Some(LoadError::Format));
        assert_eq!(parse("").err(), Some(LoadError::Format));
        let place = "0 0 -1 0 0 0 1 1.2";
        let two = format!("monitors 2560x1440 *2560x1440\nplace 0 {place}\nplace 1 {place}\n");
        let good = format!("{two}frame 0 1.3\n");
        let profile = read(&format!("format 2\n[0]\n{good}"))[0].clone();
        // Unknown lines: passed over, the profile read as without them.
        for extra in ["preset wall\n", "mirror on\n", "lens 2\n"] {
            let (slots, skipped) = parse(&format!("format 2\n[0]\n{good}{extra}")).unwrap();
            assert_eq!(slots.first(), Some(&profile), "{extra:?}");
            assert_eq!((skipped.count, skipped.first), (1, Some(7)), "{extra:?}");
        }
        let crlf = format!("format 2\n[0]\n{good}").replace('\n', "\r\n");
        assert_eq!(read(crlf.trim_end()).first(), Some(&profile));
        // Places that are not all there: none, laid out by the preset.
        let one = read(&format!(
            "format 2\n[0]\nmonitors 2560x1440 *2560x1440\nplace 0 {place}\nframe 0 1.3\n"
        ));
        let one = &one[0];
        assert!(one.places.is_empty() && one.frame.is_none());
        // A value it cannot read: that profile only.
        let many = ["1920x1080"; 11].join(" ");
        for bad in [
            "monitors 2560x1440 2560x1440\n".to_owned(),
            "monitors *2560x1440 *2560x1440\n".to_owned(),
            format!("monitors *{many}\n"),
            "monitors *2560x1440@0\n".to_owned(),
            "monitors *0x1440\n".to_owned(),
            "monitors *2560x1440\nshown 3\n".to_owned(),
            format!("monitors 2560x1440 *2560x1440\nplace 0 {place}\nplace 0 {place}\n"),
            format!("monitors *2560x1440\nplace x {place}\n"),
            format!("{good}rows 0 2\n"),
            format!("{good}rows 0 / 0 1\n"),
            format!("{good}rows 0 x\n"),
            format!("{good}frame 0 1.3\n"),
            format!("{two}frame 2 1.3\n"),
            "shown 0123456789abcdef:3\n".to_owned(),
            "name a\nname b\nmonitors *2560x1440\n".to_owned(),
        ] {
            let text = format!("format 2\n[0]\n{bad}\n[2]\n{good}");
            let (slots, skipped) = parse(&text).unwrap();
            assert_eq!(
                slots,
                std::slice::from_ref(&profile),
                "{bad:?}: only the other one"
            );
            assert_eq!(skipped.first, Some(2), "{bad:?}");
        }
        // Lines before any section, a section twice or not a number: passed over.
        let (slots, skipped) = parse(&format!(
            "format 2\n{good}[0]\n{good}[0]\n{good}[x]\n{good}"
        ))
        .unwrap();
        assert_eq!(slots, [profile]);
        assert_eq!(skipped.first, Some(2));
        assert!(skipped.count >= 3);
    }

    /// A PC forgotten leaves the profiles that showed its displays, and only its own.
    #[test]
    fn a_forgotten_pc_leaves_the_profiles() {
        let mut profiles = empty();
        let mut p = sample();
        let other = DisplayKey::new(PcId([1; 8]), DisplayId(100));
        p.shown.push(other);
        profiles.push(sample());
        profiles.push(p);
        assert!(forget_pc(&mut profiles, PcId([0xab; 8])));
        assert_eq!(profiles[1].shown, [other]);
        assert!(profiles[0].shown.is_empty());
        assert!(!forget_pc(&mut profiles, PcId([0xab; 8])));
    }

    /// Saved in its own frame, a profile comes back around wherever the user is
    /// then as it stood around them when it was saved, tilted or not; in cinema the
    /// desk arrangement is saved, not the cinema view.
    #[test]
    fn a_profile_stands_around_the_user_as_saved() {
        let saved_at = (Vec3::new(0.2, 1.2, -0.1), 0.4);
        let later = (Vec3::new(-0.5, 1.0, 0.3), -1.1);
        let monitors = [(7_u32, Mode::new(2560, 1440)), (9, Mode::new(1440, 2560))];
        let room = [(7, place(-0.4)), (9, place(0.6))];
        let live = Arrangement {
            placements: room.into_iter().collect(),
            sizes: std::collections::HashMap::new(),
            center: Some(saved_at),
            radius: Some(1.4),
            tilt: 0.0,
        };
        for tilt in [0.0, layout::OVERHEAD_PITCH] {
            let p = Profile::of(
                &monitors,
                Some(9),
                Vec::new(),
                &Arrangement {
                    tilt,
                    ..live.clone()
                },
                None,
                &[vec![7, 9]],
            );
            assert_eq!(p.primary, 1);
            assert_eq!(p.frame, Some(ProfileFrame { tilt, radius: 1.4 }));
            assert_eq!(p.rows, vec![vec![0, 1]]);
            let made: Vec<(u32, Placement)> = [7, 9].into_iter().zip(p.places.clone()).collect();
            let (back, rows) = standing(&made, p.frame, &p.rows, saved_at);
            assert_eq!(rows, vec![vec![7, 9]]);
            for ((_, b), (_, r)) in back.iter().zip(&room) {
                assert!(b.position.distance(r.position) < 1e-5, "{tilt}");
                assert!(b.rotation.angle_between(r.rotation) < 1e-4);
            }
            // Elsewhere: the same as seen from the eye.
            let (moved, _) = standing(&made, p.frame, &p.rows, later);
            let seen =
                |q: &Placement, at: (Vec3, f32)| Frame::upright(at.0, at.1).unplace(*q).position;
            for ((_, m), (_, r)) in moved.iter().zip(&room) {
                assert!(seen(m, later).distance(seen(r, saved_at)) < 1e-5);
            }
        }
        // Without a frame: where they were in the room, wherever the user is.
        let made: Vec<(u32, Placement)> = room.to_vec();
        let (back, _) = standing(&made, None, &[], later);
        assert_eq!(back, made);
        // In cinema: the desk.
        let view = Arrangement {
            placements: std::iter::once((7, place(0.0))).collect(),
            ..live.clone()
        };
        let p = Profile::of(&monitors, None, Vec::new(), &view, Some(&live), &[]);
        assert_eq!((p.places.len(), p.primary), (2, 0));
        // Not every monitor placed: no places, no rows, no frame.
        let p = Profile::of(&monitors, None, Vec::new(), &view, None, &[vec![7]]);
        assert!(p.places.is_empty() && p.rows.is_empty() && p.frame.is_none());
    }

    /// Whatever the setup (any number of monitors, places that went wrong, a center
    /// that is not one), what is saved is read back whole: nothing passed over.
    #[test]
    fn any_profile_saved_reads_back() {
        struct Rng(u64);
        impl Rng {
            fn next(&mut self) -> u64 {
                self.0 = self
                    .0
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                self.0 >> 33
            }
            fn float(&mut self) -> f32 {
                match self.next() % 12 {
                    0 => f32::NAN,
                    1 => f32::INFINITY,
                    _ => (self.next() % 4000) as f32 / 1000.0 - 2.0,
                }
            }
        }
        let mut rng = Rng(7);
        for case in 0..500 {
            let n = 1 + rng.next() as usize % 15;
            let monitors: Vec<(u32, Mode)> = (0..n as u32)
                .map(|i| {
                    let w = 1 + rng.next() as u32 % 8000;
                    let h = 1 + rng.next() as u32 % 8000;
                    let scale = [100, 125, 150, 200][rng.next() as usize % 4];
                    (i, Mode { w, h, scale })
                })
                .collect();
            let mut placements = std::collections::HashMap::new();
            for m in &monitors {
                if rng.next().is_multiple_of(20) {
                    continue;
                }
                let p = Placement {
                    position: Vec3::new(rng.float(), rng.float(), rng.float()),
                    rotation: Quat::from_rotation_y(rng.float()),
                    width: rng.float().abs(),
                };
                placements.insert(m.0, p);
            }
            let live = Arrangement {
                placements,
                sizes: std::collections::HashMap::new(),
                center: Some((Vec3::new(rng.float(), 1.0, 0.0), rng.float())),
                radius: Some(rng.float()),
                tilt: rng.float(),
            };
            let rows = vec![(0..n as u32).collect()];
            let p = Profile::of(&monitors, Some(0), Vec::new(), &live, None, &rows);
            assert!(p.monitors.len() <= MAX_MONITORS);
            let text = text_of(&vec![p.clone()]);
            let (back, skipped) = parse(&text).unwrap();
            assert!(skipped.is_empty(), "case {case}: {skipped:?}\n{text}");
            let back = &back[0];
            assert_eq!(back.monitors, p.monitors, "case {case}");
            assert_eq!(back.places.len(), p.places.len(), "case {case}");
        }
    }
}
