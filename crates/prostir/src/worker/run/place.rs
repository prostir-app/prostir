//! Where the outputs go on the desktop: the plan laid out, the primary monitor kept
//! where it is wanted.

use super::*;

impl Worker {
    /// Takes a scale given to a monitor in the desktop's own settings (KDE's display
    /// settings, say): the monitor is that scale from now on, its output as it is, and
    /// the control loop hears of it. The scale is what the platform knows already
    /// ([`Output::desk_scale`]: KWin's events, nothing asked), looked at on every tick.
    /// Taken only when seen on two ticks in a row, and only away from the scale the
    /// output was made with after it was seen at that one: a scale Prostir could not
    /// set is never taken for the person's choice.
    pub(super) fn check_scales(&mut self) {
        let mut taken = std::mem::take(&mut self.scales_taken);
        taken.clear();
        for (id, slot) in &mut self.slots {
            let Made::Monitor { spec, .. } = &slot.made else {
                continue;
            };
            let seen = slot
                .output
                .as_ref()
                .filter(|o| matches!(o.health(), OutputHealth::Live))
                .and_then(|o| o.desk_scale());
            let Some(seen) = seen else {
                slot.scale_seen = None;
                continue;
            };
            if seen == spec.percent() {
                slot.seen_own = true;
                slot.scale_seen = None;
                continue;
            }
            if slot.seen_own && slot.scale_seen == Some(seen) {
                taken.push((*id, seen));
            }
            slot.scale_seen = Some(seen);
        }
        for &(id, percent) in &taken {
            self.take_scale(id, percent);
        }
        self.scales_taken = taken;
    }

    /// Monitor `id` stands at `percent` on the desktop from now on, unless the desktop
    /// does not give its size that scale (then it is left alone, said once).
    fn take_scale(&mut self, id: ScreenId, percent: u16) {
        let allowed = self.desktop.caps().scales.allows(percent);
        let Some(slot) = self.slots.get_mut(&id) else {
            return;
        };
        let Made::Monitor { spec, .. } = &mut slot.made else {
            return;
        };
        let old = spec.percent();
        let scale = f64::from(percent) / 100.0;
        if !allowed || !(0.5..=4.0).contains(&scale) {
            warn!(
                "{}: scale {percent} % given on the desktop is not one Prostir gives it: left as it is",
                slot.name
            );
            // Not again until it was back at its own.
            slot.seen_own = false;
            slot.scale_seen = None;
            return;
        }
        info!(
            "{}: scale {percent} % given on the desktop, taken",
            slot.name
        );
        spec.scale = scale;
        slot.scale_seen = None;
        if let Some(output) = &slot.output {
            output.scale_taken(percent);
        }
        // What the control loop wants follows at once, and so does what it sends until
        // it heard of the scale: its output is not made anew at the old one.
        self.rescaled.insert(id, (old, percent));
        if let Some(w) = &mut self.wanted {
            follow_rescaled(&mut self.rescaled, w);
        }
        self.layout_due = true;
        self.done(Done::Rescaled { id, percent });
    }

    /// The monitors whose scale is to change in place with the next layout: made at
    /// another scale than wanted now, the same size and number, on a desktop that
    /// changes scales in place, and not at a scale the desktop refused.
    pub(super) fn rescales(&self) -> Vec<Rescale> {
        let Some(wanted) = &self.wanted else {
            return Vec::new();
        };
        if !self.desktop.caps().scales_in_place {
            return Vec::new();
        }
        self.slots
            .iter()
            .filter_map(|(id, slot)| {
                let Made::Monitor { spec, .. } = &slot.made else {
                    return None;
                };
                let key = slot.key()?;
                let (_, w) = wanted.screens.iter().find(|(i, _)| i == id)?;
                let What::Monitor { spec: to, .. } = &w.what else {
                    return None;
                };
                let (from, to_percent) = (spec.percent(), to.percent());
                if !slot.made.is_but_scale(&w.what)
                    || from == to_percent
                    || self.scale_refused.get(id) == Some(&to_percent)
                {
                    return None;
                }
                Some(Rescale {
                    id: *id,
                    key: key.clone(),
                    from,
                    to: to_percent,
                    size: logical_at((spec.width, spec.height), to_percent),
                })
            })
            .collect()
    }

    /// The output that is to be primary, where it is ours to pick.
    fn primary_key(&self) -> Option<OutputKey> {
        if !self.desktop.caps().has_primary {
            return None;
        }
        match self.wanted.as_ref()?.layout.primary.as_ref()? {
            Primary::Screen(id) => self.slots.get(id).and_then(Slot::key).cloned(),
            Primary::Display(key) => Some(key.clone()),
        }
    }

    /// Makes the wanted primary output the primary one, when that changed (one of
    /// ours once it is made; a display of the PC that does not stream is made primary
    /// by the layout).
    pub(super) fn keep_primary_ours(&mut self) {
        let key = self.primary_key();
        if key.is_none() || key == self.primary_made {
            return;
        }
        let ours = self
            .slots
            .values()
            .filter_map(|s| s.output.as_ref())
            .find(|o| Some(o.key()) == key.as_ref());
        match ours {
            Some(output) => {
                self.shared.steps.begin("making the primary output primary");
                output.make_primary();
                self.shared.steps.end();
                self.wallpapers_due = true;
            }
            None if matches!(
                self.wanted.as_ref().and_then(|w| w.layout.primary.as_ref()),
                Some(Primary::Screen(_))
            ) =>
            {
                return;
            }
            None => {}
        }
        self.primary_made = key;
        self.layout_due = true;
    }

    /// Makes sure the desktop's primary output (where the Plasma panel lives) is ours.
    /// Right after outputs come and go (a profile switch) KDE can end up with another.
    pub(super) fn check_primary(&mut self) {
        if self.primary_checked.elapsed() < PRIMARY_EVERY {
            return;
        }
        self.primary_checked = Instant::now();
        if let Some(key) = self.primary_key() {
            self.begin("keeping the primary output ours");
            self.desktop.keep_primary(&key);
        }
    }

    /// Our outputs with their geometry on the desktop.
    pub(super) fn geometries(&self) -> Vec<(OutputKey, Geometry)> {
        let geometries = self.desktop.outputs();
        self.slots
            .values()
            .filter_map(|s| {
                let key = s.key()?;
                Some((key.clone(), *geometries.get(key)?))
            })
            .collect()
    }

    /// Lays the outputs out if anything changed since the last time, once however much
    /// changed. Only outputs the desktop lists already are placed; the others once it
    /// lists them. When the desktop refuses (KScreen stuck, say), the layout stays due
    /// and is tried again after 1, 2, 4... up to 30 s.
    pub(super) fn place(&mut self) {
        if !self.layout_due || Instant::now() < self.layout_retry.0 || self.wanted.is_none() {
            return;
        }
        self.begin("placing the outputs");
        let placed = self.place_now();
        self.end();
        match placed {
            Ok(on_desk) => {
                self.layout_due = self.layout_waits;
                self.layout_retry = (Instant::now(), 0);
                self.done(Done::Placed {
                    on_desk,
                    upto: self.taken,
                });
            }
            Err(e) => {
                let failures = self.layout_retry.1 + 1;
                let wait = Duration::from_secs((1u64 << failures.min(5)).min(30));
                warn!("{e:#}; trying again in {} s", wait.as_secs());
                // Once for each time it starts failing: the headset's map is not the
                // desktop's meanwhile.
                if failures == 1 {
                    self.shared.router.send_all(&HostMsg::Problem {
                        error: ErrorCode::DESKTOP,
                        detail: state::clip(&format!("{e:#}"), state::MAX_DETAIL),
                        display: None,
                    });
                }
                self.layout_retry = (Instant::now() + wait, failures);
            }
        }
    }

    /// The size of the stream of screen `id`: as its encoder says, else as the
    /// monitor was made.
    fn stream_size_of(&self, id: ScreenId) -> Option<(i32, i32)> {
        let (w, h) = self.shared.router.stream_size(id).or_else(|| {
            let spec = self.slots.get(&id)?.made.spec()?;
            Some((spec.width, spec.height))
        })?;
        Some((i32::try_from(w).ok()?, i32::try_from(h).ok()?))
    }

    /// Lays the outputs out on the desktop like they stand in the headset (see
    /// `prostir_layout::desk`): the monitors in the headset's rows, the laptop screen
    /// under the main monitor unless the headset placed it, other real displays next
    /// to it and window panels right of everything. The mouse then crosses screens
    /// where the eyes expect it to. Returns where each output went.
    fn place_now(&mut self) -> Result<BTreeMap<OutputKey, state::DeskRect>> {
        let Some(wanted) = self.wanted.clone() else {
            return Ok(BTreeMap::new());
        };
        let layout = &wanted.layout;
        let geometries = self.desktop.outputs();
        let mut names: BTreeMap<Out, OutputKey> = self
            .slots
            .iter()
            .filter_map(|(id, s)| Some((Out::Screen(*id), s.key()?.clone())))
            .collect();
        // Other real displays (the laptop panel next to a mirrored external monitor,
        // say) go on the bottom row too, so nothing overlaps the Prostir monitors.
        let mut others: Vec<&OutputKey> = geometries
            .iter()
            .filter(|(name, g)| {
                g.enabled && g.w > 0 && !names.values().any(|n| n == *name) && !g.is_virtual
            })
            .map(|(name, _)| name)
            .collect();
        others.sort();
        for (i, name) in others.into_iter().enumerate() {
            names.insert(Out::Other(i), name.clone());
        }
        // Monitors whose scale changes in place, with this layout: laid out at the
        // size they get. Only those the desktop lists.
        let rescales: Vec<Rescale> = self
            .rescales()
            .into_iter()
            .filter(|r| geometries.contains_key(&r.key))
            .collect();
        // Only outputs the desktop lists: the others once it does.
        let size = |out: Out| {
            let key = names.get(&out)?;
            if let Some(r) = rescales.iter().find(|r| &r.key == key) {
                return Some(r.size);
            }
            let g = geometries.get(key)?;
            Some(desk::Size { w: g.w, h: g.h })
        };
        self.layout_waits = self
            .slots
            .values()
            .any(|s| s.key().is_some_and(|n| !geometries.contains_key(n)));
        let has = |id: &ScreenId| self.slots.get(id).is_some_and(|s| s.output.is_some());
        let rows: Vec<Vec<desk::Entry<ScreenId>>> = layout
            .rows
            .iter()
            .map(|r| {
                r.iter()
                    .filter(|e| has(&e.id) || !layout.physical.contains(&e.id))
                    .copied()
                    .collect::<Vec<_>>()
            })
            .filter(|r| !r.is_empty())
            .collect();
        // Displays of the PC shown here: the laptop's panel first, under the main
        // monitor, unless the headset placed them.
        let streaming_physical: Vec<Out> = layout
            .physical
            .iter()
            .filter(|id| has(id))
            .map(|id| Out::Screen(*id))
            .collect();
        let row_ids: Vec<Vec<Out>> = rows
            .iter()
            .map(|r| r.iter().map(|e| Out::Screen(e.id)).collect())
            .collect();
        // Where the headset has each monitor: the boxes of its own plan, in the pixels
        // of the streams (`SetLayout`, see `PROTOCOL.md`).
        let boxes = desk::Boxes::new(
            rows.iter()
                .flatten()
                .filter_map(|e| {
                    let out = Out::Screen(e.id);
                    let on_desk = size(out)?;
                    let (w, h) = self.stream_size_of(e.id).unwrap_or((on_desk.w, on_desk.h));
                    let seen = desk::SeenBox::from_wire(e.center?, e.middle?, desk::Size { w, h });
                    Some((out, seen, on_desk))
                })
                .collect(),
        );
        let fresh = desk::plan(
            &desk::Desk {
                rows: row_ids.clone(),
                main: layout.main.map(Out::Screen),
                under_main: streaming_physical.first().copied(),
                bottom: streaming_physical
                    .iter()
                    .skip(1)
                    .copied()
                    .chain(names.keys().filter(|o| matches!(o, Out::Other(_))).copied())
                    .collect(),
                column: layout
                    .column
                    .iter()
                    .filter(|id| has(id))
                    .map(|id| Out::Screen(*id))
                    .collect(),
            },
            size,
            &boxes,
        );
        // The headset's own plan comes back as it is (planning a plan gives the plan)
        // and is taken, so the desktop is what its map shows. Anything worked out here
        // (outputs scaled, monitors the headset has not placed) replaces the layout the
        // desktop has, in the same rows, only when better by more than a hand's tremor.
        let plan = if is_the_headsets(&fresh, &boxes) || self.applied.0 != row_ids {
            fresh
        } else {
            desk::settled(&self.applied.1, &fresh, &boxes)
        };
        // Every applied change makes KWin reconfigure its outputs, which also cuts the
        // laptop mirror's stream: `place` only sends what actually differs.
        let mut positions: Vec<(OutputKey, i32, i32)> = plan
            .iter()
            .map(|p| (names[&p.id].clone(), p.x, p.y))
            .collect();
        let primary = self.primary_key();
        // A desktop whose primary output is always at (0, 0) moves with it: the whole
        // plan shifts so that it is, the same for the desktop and the headset's map.
        let (dx, dy) = if self.desktop.caps().origin_is_primary {
            let main = primary.clone().or_else(|| {
                geometries
                    .iter()
                    .find(|(_, g)| g.priority == Some(1))
                    .map(|(key, _)| key.clone())
            });
            origin_at(&positions, main.as_ref())
        } else {
            (0, 0)
        };
        for (_, x, y) in &mut positions {
            (*x, *y) = (*x - dx, *y - dy);
        }
        // A step of its own after looking at the outputs.
        self.begin("placing the outputs");
        let scales = rescales.iter().map(|r| (r.key.clone(), r.to)).collect();
        let placed = self.desktop.place(
            &DeskPlan {
                positions,
                primary,
                scales,
            },
            &geometries,
        );
        if let Err(e) = placed {
            if rescales.is_empty() {
                return Err(e);
            }
            // Laid out again at once without the new scales: where the desktop takes
            // that, it refused the scales (not asked again: the monitors stay at the
            // ones they have, and nobody is told the desktop failed); where it does
            // not, the layout failed whatever the scales, which are asked again with
            // the next try.
            warn!("{e:#}: laying the outputs out without the new scales");
            for r in &rescales {
                self.scale_refused.insert(r.id, r.to);
            }
            return match self.place_now() {
                Ok(on_desk) => {
                    for r in &rescales {
                        warn!(
                            "the desktop did not take {} % for {} in place: it stays at {} %",
                            r.to, r.key, r.from
                        );
                        self.done(Done::ScaleRefused {
                            id: r.id,
                            percent: r.from,
                        });
                    }
                    Ok(on_desk)
                }
                Err(again) => {
                    for r in &rescales {
                        self.scale_refused.remove(&r.id);
                    }
                    Err(again)
                }
            };
        }
        for r in &rescales {
            info!("{}: {} % in place (was {} %)", r.key, r.to, r.from);
            if let Some(slot) = self.slots.get_mut(&r.id)
                && let Made::Monitor { spec, .. } = &mut slot.made
            {
                spec.scale = f64::from(r.to) / 100.0;
                // Seen at the new scale before another given on the desktop is taken.
                slot.seen_own = false;
                slot.scale_seen = None;
            }
        }
        let on_desk = plan
            .iter()
            .map(|p| {
                let rect = state::DeskRect {
                    x: p.x - dx,
                    y: p.y - dy,
                    w: p.w.max(0) as u32,
                    h: p.h.max(0) as u32,
                };
                (names[&p.id].clone(), rect)
            })
            .collect();
        self.applied = (row_ids, plan);
        Ok(on_desk)
    }
}

/// A monitor whose scale changes in place, with the layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Rescale {
    pub id: ScreenId,
    pub key: OutputKey,
    /// The scale it has, and the one it gets, in percent.
    pub from: u16,
    pub to: u16,
    /// Its size on the desktop at the new scale, in logical pixels.
    pub size: desk::Size,
}

/// The logical size of a monitor of `pixels` at `percent`.
pub(super) fn logical_at(pixels: (u32, u32), percent: u16) -> desk::Size {
    let at = |px: u32| (f64::from(px) * 100.0 / f64::from(percent.max(1))).round() as i32;
    desk::Size {
        w: at(pixels.0),
        h: at(pixels.1),
    }
}

/// How far a plan moves so that output `main` is at (0, 0): where `positions` put it
/// (nothing, without it).
pub(super) fn origin_at(
    positions: &[(OutputKey, i32, i32)],
    main: Option<&OutputKey>,
) -> (i32, i32) {
    main.and_then(|m| positions.iter().find(|(key, _, _)| key == m))
        .map_or((0, 0), |(_, x, y)| (*x, *y))
}

/// Monitors whose scale was given on the desktop and taken (`old`, `new` in percent):
/// a wanted state that still has the old one, sent before the control loop heard of
/// it, gets the new one. Done once one has the new scale, or another (chosen in
/// Prostir since), or the monitor is gone.
pub(super) fn follow_rescaled(rescaled: &mut BTreeMap<ScreenId, (u16, u16)>, wanted: &mut Wanted) {
    if rescaled.is_empty() {
        return;
    }
    rescaled.retain(|id, &mut (old, new)| {
        let spec = wanted
            .screens
            .iter_mut()
            .find_map(|(i, s)| match &mut s.what {
                What::Monitor { spec, .. } if i == id => Some(spec),
                _ => None,
            });
        match spec {
            Some(spec) if spec.percent() == old => {
                spec.scale = f64::from(new) / 100.0;
                true
            }
            _ => false,
        }
    });
}

/// Whether `plan` is the headset's plan as `boxes` hold it: every output with a box
/// where the box is, give or take one shift of the whole.
pub(super) fn is_the_headsets<K: Copy + Ord>(
    plan: &[desk::Spot<K>],
    boxes: &desk::Boxes<K>,
) -> bool {
    use desk::View;
    let mut shift = None;
    let mut any = false;
    for s in plan {
        if !boxes.boxes.iter().any(|b| b.0 == s.id) {
            continue;
        }
        let Some((x, y)) = boxes.exact(s.id, desk::Size { w: s.w, h: s.h }) else {
            return false;
        };
        if *shift.get_or_insert((s.x - x, s.y - y)) != (s.x - x, s.y - y) {
            return false;
        }
        any = true;
    }
    any
}
