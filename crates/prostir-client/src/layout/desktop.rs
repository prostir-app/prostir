//! The PC's desktop made of the rows the monitors are seen in (what `Arrange` carries).

use std::cell::{Ref, RefCell};
use std::collections::HashMap;

use glam::Vec3;
use prostir_layout::desk::{self, Spot};
use prostir_layout::view::Room;
use prostir_proto::state::{DisplayId, Placed, Row};

use super::arrange::{frame, placed, rows_seen};
use super::{Key, Placement};

/// What the PC lays its desktop out by: the rows the monitors are seen in, and the
/// desktop made of them, in the pixels of the streams. The PC makes the same desktop of
/// it, to the pixel, and the map in the menu shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Desktop<K> {
    /// Top to bottom, each left to right.
    pub rows: Vec<Vec<K>>,
    pub plan: Vec<Spot<K>>,
}

impl<K: Key> Desktop<K> {
    /// The desktop for `monitors` as they stand in `placements`, seen from `center`,
    /// the `primary` monitor's row first. `sent`: the desktop the PC has. Its rows keep
    /// their monitors until one is clearly moved out, and while the rows stay its plan
    /// does as long as it fits what is seen as well, give or take a hand's tremor (see
    /// `prostir_layout::desk::settled`): a nudge changes nothing on the PC.
    pub fn of(
        placements: &HashMap<K, Placement>,
        monitors: &[(K, u32, u32)],
        center: (Vec3, f32),
        tilt: f32,
        primary: Option<K>,
        sent: Option<&Desktop<K>>,
    ) -> Self {
        let all = placed(placements, monitors);
        let room = Room::new(&frame(center, tilt), &all);
        let rows = rows_seen(&room, &all, sent.map_or(&[], |d| &d.rows));
        let size = |id: K| {
            let m = all.iter().find(|m| m.id == id)?;
            Some(desk::Size {
                w: i32::try_from(m.px.w).ok()?,
                h: i32::try_from(m.px.h).ok()?,
            })
        };
        let wanted = desk::Desk {
            rows: rows.clone(),
            main: primary,
            ..desk::Desk::default()
        };
        let fresh = desk::plan(&wanted, size, &room);
        let plan = match sent {
            Some(s) if s.rows == rows => desk::settled(&s.plan, &fresh, &room),
            _ => fresh,
        };
        Self { rows, plan }
    }

    /// What [`Desktop::of`] takes as the PC's desktop when only its `rows` are known (a
    /// profile switched to keeps its rows, not its plan): their pairs of neighbours stay
    /// pairs until one is clearly moved away, and the plan is made afresh. `None` for no
    /// rows: rows then start afresh too.
    pub fn history(rows: Vec<Vec<K>>) -> Option<Self> {
        (!rows.is_empty()).then(|| Self {
            rows,
            plan: Vec::new(),
        })
    }

    /// As `Arrange` carries it: the rows, and where each monitor's spot is centred
    /// (see `PROTOCOL.md`, "Layout"), of the monitors `number` gives a number on the
    /// PC it goes to (a PC hears of its own displays only).
    pub fn wire(&self, number: impl Fn(K) -> Option<DisplayId>) -> Vec<Row> {
        self.rows
            .iter()
            .map(|row| Row {
                entries: row
                    .iter()
                    .filter_map(|&k| {
                        let at = self.plan.iter().find(|s| s.id == k).map(Spot::wire);
                        Some(Placed {
                            id: number(k)?,
                            center: at.map(|a| a.0),
                            middle: at.map(|a| a.1),
                        })
                    })
                    .collect(),
            })
            .filter(|row| !row.entries.is_empty())
            .collect()
    }

    /// The monitors for the map in the menu: centre and half size in pixels, up
    /// positive.
    pub fn spots(&self) -> HashMap<K, [f32; 4]> {
        self.plan
            .iter()
            .map(|s| {
                let (w, h) = (s.w as f32, s.h as f32);
                let (x, y) = (s.x as f32 + w / 2.0, s.y as f32 + h / 2.0);
                (s.id, [x, -y, w / 2.0, h / 2.0])
            })
            .collect()
    }
}

/// What a [`Desktop`] is made of.
#[derive(Debug, Clone, PartialEq)]
struct DesktopInputs<K> {
    /// None made yet: nothing is the same as this.
    made: bool,
    placed: Vec<(K, Placement, u32, u32)>,
    center: (Vec3, f32),
    tilt: f32,
    primary: Option<K>,
    sent: Option<Desktop<K>>,
}

impl<K: Key> DesktopInputs<K> {
    /// The desktop made of these is the one [`Desktop::of`] would make of those, looked
    /// at in place (nothing is allocated).
    fn same(
        &self,
        placements: &HashMap<K, Placement>,
        monitors: &[(K, u32, u32)],
        center: (Vec3, f32),
        tilt: f32,
        primary: Option<K>,
        sent: Option<&Desktop<K>>,
    ) -> bool {
        let mut known = self.placed.iter();
        let placed = monitors
            .iter()
            .filter_map(|&(id, w, h)| Some((id, *placements.get(&id)?, w, h)))
            .all(|m| known.next() == Some(&m));
        self.made
            && placed
            && known.next().is_none()
            && self.center == center
            && self.tilt == tilt
            && self.primary == primary
            && self.sent.as_ref() == sent
    }

    /// Now these, in the memory kept from before.
    fn keep(
        &mut self,
        placements: &HashMap<K, Placement>,
        monitors: &[(K, u32, u32)],
        center: (Vec3, f32),
        tilt: f32,
        primary: Option<K>,
        sent: Option<&Desktop<K>>,
    ) {
        self.made = true;
        self.placed.clear();
        self.placed.extend(
            monitors
                .iter()
                .filter_map(|&(id, w, h)| Some((id, *placements.get(&id)?, w, h))),
        );
        self.center = center;
        self.tilt = tilt;
        self.primary = primary;
        match (&mut self.sent, sent) {
            (Some(kept), Some(sent)) => kept.clone_from(sent),
            (kept, sent) => *kept = sent.cloned(),
        }
    }
}

/// [`Desktop::of`], made again only when what it is made of changes: the headset asks
/// for it every frame, and planning takes about a millisecond for six monitors. While
/// nothing changes, asking allocates nothing.
#[derive(Debug)]
pub struct DesktopMemo<K>(RefCell<(DesktopInputs<K>, Desktop<K>)>);

impl<K> Default for DesktopMemo<K> {
    fn default() -> Self {
        Self(RefCell::new((
            DesktopInputs {
                made: false,
                placed: Vec::new(),
                center: (Vec3::ZERO, 0.0),
                tilt: 0.0,
                primary: None,
                sent: None,
            },
            Desktop {
                rows: Vec::new(),
                plan: Vec::new(),
            },
        )))
    }
}

impl<K: Key> DesktopMemo<K> {
    /// [`Desktop::of`] these.
    pub fn get(
        &self,
        placements: &HashMap<K, Placement>,
        monitors: &[(K, u32, u32)],
        center: (Vec3, f32),
        tilt: f32,
        primary: Option<K>,
        sent: Option<&Desktop<K>>,
    ) -> Desktop<K> {
        self.get_ref(placements, monitors, center, tilt, primary, sent)
            .clone()
    }

    /// [`Desktop::of`] these, as kept: a copy is made only when it changed.
    pub fn get_ref(
        &self,
        placements: &HashMap<K, Placement>,
        monitors: &[(K, u32, u32)],
        center: (Vec3, f32),
        tilt: f32,
        primary: Option<K>,
        sent: Option<&Desktop<K>>,
    ) -> Ref<'_, Desktop<K>> {
        let same = self
            .0
            .borrow()
            .0
            .same(placements, monitors, center, tilt, primary, sent);
        if !same {
            let desktop = Desktop::of(placements, monitors, center, tilt, primary, sent);
            let mut memo = self.0.borrow_mut();
            memo.0
                .keep(placements, monitors, center, tilt, primary, sent);
            memo.1 = desktop;
        }
        Ref::map(self.0.borrow(), |m| &m.1)
    }
}
