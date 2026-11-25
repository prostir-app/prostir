//! `screens.txt`: the screens and their rows, the PC's own displays, the numbers
//! given out; and the files of versions before, moved over.

use super::*;

/// How a display of the PC is told from others: by what it says it is (its EDID:
/// maker, model, serial number), or where that says nothing or is not unique, by the
/// connector it is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhysicalKey {
    pub edid: Option<String>,
    pub connector: String,
}

/// A display of the PC itself, as kept: how it is known, whether it is the laptop's
/// own panel, and whether the headset shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Physical {
    pub key: PhysicalKey,
    pub builtin: bool,
    pub shown: bool,
    /// When it was last connected, in seconds since 1970 (0: not known): of those
    /// not connected, the least recently seen are forgotten first.
    pub seen: u64,
}

/// The set of virtual monitors, the PC's own displays it knows, and how the headset
/// shows them.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SavedScreens {
    pub specs: BTreeMap<ScreenId, MonitorSpec>,
    /// The PC's own displays seen so far, connected now or not.
    pub physical: BTreeMap<ScreenId, Physical>,
    /// Rows from top to bottom, left to right, with where each is seen; the PC's own
    /// displays too, where the headset put them.
    pub rows: Vec<Vec<Entry<ScreenId>>>,
    pub primary: Option<ScreenId>,
    /// The monitors whose scale the main headset chooses ("Auto"): new monitors are,
    /// until a scale is chosen by hand. Monitors kept by versions before it are not.
    pub auto: std::collections::BTreeSet<ScreenId>,
}

impl SavedScreens {
    /// One 2560x1440 monitor, for a first start.
    pub fn first() -> Self {
        let id = ScreenId::saved(0);
        Self {
            specs: BTreeMap::from([(id, "2560x1440".parse().expect("a valid size"))]),
            rows: vec![vec![Entry::new(id)]],
            auto: [id].into(),
            ..Self::default()
        }
    }

    /// `specs` side by side in one row, numbered from 0, where nothing is known.
    pub fn of(specs: &[MonitorSpec]) -> Self {
        let ids: Vec<ScreenId> = (0..specs.len() as u32).map(ScreenId::saved).collect();
        Self {
            specs: ids.iter().copied().zip(specs.iter().cloned()).collect(),
            rows: vec![ids.iter().map(|&id| Entry::new(id)).collect()],
            ..Self::default()
        }
    }

    /// The highest number among the screens and in the rows.
    pub(super) fn highest(&self) -> Option<u32> {
        let rows = self.rows.iter().flatten().map(|e| e.id);
        self.specs
            .keys()
            .chain(self.physical.keys())
            .copied()
            .chain(rows)
            .map(ScreenId::number)
            .max()
    }
}

/// The monitors to start with: those from the command line if any (for this run),
/// else the saved ones, else one.
pub fn startup_set(saved: &SavedScreens, args: &[MonitorSpec]) -> Option<SavedScreens> {
    if !args.is_empty() {
        let mut set = SavedScreens::of(&args[..args.len().min(MONITORS_NO_LIMITS)]);
        // The PC's own displays stay as they were, where they were seen.
        set.physical = saved.physical.clone();
        let places: Vec<Entry<ScreenId>> = saved
            .rows
            .iter()
            .flatten()
            .filter(|e| !saved.specs.contains_key(&e.id))
            .copied()
            .collect();
        if !places.is_empty() {
            set.rows.push(places);
        }
        Some(set)
    } else if saved.specs.is_empty() {
        Some(SavedScreens::first())
    } else {
        None
    }
}

/// `at CENTER MIDDLE`, either `next` (unknown centre) or missing (unknown middle).
fn parse_seen<'a>(mut words: impl Iterator<Item = &'a str>) -> (Option<i32>, Option<i32>) {
    let center = words.next().and_then(|w| w.parse().ok());
    let middle = words.next().and_then(|w| w.parse().ok());
    (center, middle)
}

fn seen_text(e: &Entry<ScreenId>) -> String {
    if e.center.is_none() && e.middle.is_none() {
        return String::new();
    }
    let center = e
        .center
        .map_or_else(|| "next".to_owned(), |c| c.to_string());
    match e.middle {
        Some(m) => format!(" at {center} {m}"),
        None => format!(" at {center}"),
    }
}

/// The laptop screen's number in version 1 of `screens.txt` (its `mirror` lines).
pub const LEGACY_LAPTOP: u32 = 100;

/// `ID [edid "TEXT"] connector NAME [builtin] [shown] [seen SECONDS]`: a display of
/// the PC itself.
fn parse_physical(rest: &str) -> Option<(ScreenId, Physical)> {
    let (id, rest) = rest.split_once(' ')?;
    let id = ScreenId::saved(id.parse().ok()?);
    let (edid, rest) = match rest.strip_prefix("edid \"") {
        Some(r) => {
            let (edid, r) = r.split_once('"')?;
            (Some(edid.to_owned()), r.trim_start())
        }
        None => (None, rest),
    };
    let mut words = rest.split_whitespace();
    if words.next()? != "connector" {
        return None;
    }
    let connector = words.next()?.to_owned();
    let (mut builtin, mut shown, mut seen) = (false, false, 0);
    while let Some(w) = words.next() {
        match w {
            "builtin" => builtin = true,
            "shown" => shown = true,
            "seen" => seen = words.next()?.parse().ok()?,
            _ => return None,
        }
    }
    Some((
        id,
        Physical {
            key: PhysicalKey { edid, connector },
            builtin,
            shown,
            seen,
        },
    ))
}

fn physical_text(id: ScreenId, p: &Physical) -> String {
    let mut text = format!("physical {}", id.number());
    if let Some(edid) = &p.key.edid {
        let _ = write!(text, " edid \"{}\"", edid.replace('"', "'"));
    }
    let _ = write!(text, " connector {}", p.key.connector);
    if p.builtin {
        text.push_str(" builtin");
    }
    if p.shown {
        text.push_str(" shown");
    }
    if p.seen > 0 {
        let _ = write!(text, " seen {}", p.seen);
    }
    text.push('\n');
    text
}

/// The file `screens.txt`: `next N` (the counter, see [`Store::new_id`]), `physical …`
/// for each display of the PC itself, then `row` and in each row `screen N WxH[@scale]
/// [at C M] [auto] [primary]` for a monitor (`auto`: its scale is the headset's choice) and `place N [at C M] [primary]` for a display of
/// the PC (version 1 had `mirror [at C M]` for the laptop screen, number 100). The
/// counter read, if there is one.
pub(super) fn parse_screens(text: &str) -> (SavedScreens, Kept, Option<u32>) {
    let mut set = SavedScreens::default();
    let mut kept = Kept::default();
    let mut next = None;
    let place =
        |set: &mut SavedScreens, id: ScreenId, seen: (Option<i32>, Option<i32>), primary: bool| {
            if set.rows.iter().flatten().any(|e| e.id == id) {
                return false;
            }
            if primary {
                set.primary = Some(id);
            }
            if set.rows.is_empty() {
                set.rows.push(Vec::new());
            }
            if let Some(row) = set.rows.last_mut() {
                row.push(Entry {
                    id,
                    center: seen.0,
                    middle: seen.1,
                });
            }
            true
        };
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let mut words = line.split_whitespace();
        let known = match words.next() {
            Some("version") => words
                .next()
                .and_then(|v| v.parse().ok())
                .map(|v| kept.version = v)
                .is_some(),
            Some("next") => words
                .next()
                .and_then(|v| v.parse().ok())
                .map(|v| next = Some(v))
                .is_some(),
            Some("physical") => match line.strip_prefix("physical ").and_then(parse_physical) {
                Some((id, p))
                    if !set.physical.contains_key(&id) && !set.specs.contains_key(&id) =>
                {
                    set.physical.insert(id, p);
                    true
                }
                _ => false,
            },
            Some("row") => {
                set.rows.push(Vec::new());
                true
            }
            Some("screen") => (|| {
                let id = ScreenId::saved(words.next()?.parse().ok()?);
                let spec: MonitorSpec = words.next()?.parse().ok()?;
                let mut rest: Vec<&str> = words.collect();
                let auto = rest.contains(&"auto");
                rest.retain(|w| *w != "auto");
                let primary = rest.last() == Some(&"primary");
                let seen = match rest.first() {
                    Some(&"at") => parse_seen(rest[1..].iter().copied()),
                    _ => (None, None),
                };
                if set.specs.contains_key(&id) || set.physical.contains_key(&id) {
                    return None;
                }
                if !place(&mut set, id, seen, primary) {
                    return None;
                }
                if auto {
                    set.auto.insert(id);
                }
                set.specs.insert(id, spec);
                Some(())
            })()
            .is_some(),
            Some(word @ ("place" | "mirror")) => (|| {
                let id = if word == "mirror" {
                    ScreenId::saved(LEGACY_LAPTOP)
                } else {
                    ScreenId::saved(words.next()?.parse().ok()?)
                };
                let rest: Vec<&str> = words.collect();
                let primary = rest.last() == Some(&"primary");
                let seen = match rest.first() {
                    Some(&"at") => parse_seen(rest[1..].iter().copied()),
                    _ => (None, None),
                };
                place(&mut set, id, seen, primary).then_some(())
            })()
            .is_some(),
            _ => false,
        };
        if !known {
            kept.unknown.push(line.to_owned());
        }
    }
    set.rows.retain(|r| !r.is_empty());
    (set, kept, next)
}

pub(super) fn screens_text(set: &SavedScreens, kept: &Kept, next: u32) -> String {
    let mut text = kept.head();
    let _ = writeln!(text, "next {next}");
    for (id, p) in &set.physical {
        text.push_str(&physical_text(*id, p));
    }
    for row in &set.rows {
        text.push_str("row\n");
        for e in row {
            let primary = if set.primary == Some(e.id) {
                " primary"
            } else {
                ""
            };
            match set.specs.get(&e.id) {
                Some(spec) => {
                    // After the place, which a version before "auto" reads first.
                    let auto = if set.auto.contains(&e.id) {
                        " auto"
                    } else {
                        ""
                    };
                    let _ = writeln!(
                        text,
                        "screen {} {spec}{}{auto}{primary}",
                        e.id.number(),
                        seen_text(e)
                    );
                }
                None => {
                    let _ = writeln!(text, "place {}{}{primary}", e.id.number(), seen_text(e));
                }
            }
        }
    }
    text
}

/// What 0.1 saved in `monitors.txt`: `row` starts a row, then one
/// `WIDTHxHEIGHT[@scale][ at CENTER[ MIDDLE]][ *]` per monitor (`next` for an unknown
/// centre, `*` for the primary one), and the choices as `key value`. Files from
/// before rows have no `row`; from before 0.1.1, no middle. Monitors are numbered in
/// the order of the file, as 0.1 numbered them, so the headset finds them where it
/// saw them last.
pub(super) fn migrate(text: &str) -> (Settings, SavedScreens) {
    let (settings, _) = parse_settings(text);
    let mut set = SavedScreens::default();
    let mut rows: Vec<Vec<Entry<ScreenId>>> = Vec::new();
    let mut loose: Vec<Entry<ScreenId>> = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if line == "row" || line.starts_with("row ") {
            rows.push(Vec::new());
            continue;
        }
        let (line, star) = match line.strip_suffix('*') {
            Some(s) => (s.trim(), true),
            None => (line, false),
        };
        let (spec, seen) = match line.split_once(" at ") {
            Some((spec, at)) => (spec.trim(), parse_seen(at.split_whitespace())),
            None => (line, (None, None)),
        };
        let Ok(spec) = spec.parse::<MonitorSpec>() else {
            continue;
        };
        let id = ScreenId::saved(set.specs.len() as u32);
        set.specs.insert(id, spec);
        if star {
            set.primary = Some(id);
        }
        let entry = Entry {
            id,
            center: seen.0,
            middle: seen.1,
        };
        match rows.last_mut() {
            Some(row) => row.push(entry),
            None => loose.push(entry),
        }
    }

    rows.retain(|r| !r.is_empty());
    if !loose.is_empty() {
        rows.push(loose);
    }
    set.rows = rows;
    (settings, set)
}
