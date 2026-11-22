//! Which monitors there are and how they are arranged: numbers, rows, the primary
//! monitor, scale and position on the desktop, and what is kept for next time.

use super::*;

impl Displays {
    /// The virtual monitors.
    pub(super) fn monitors(&self) -> impl Iterator<Item = &Screen> {
        self.screens.values().filter(|s| s.is_virtual())
    }

    /// The window panels.
    pub(super) fn windows(&self) -> impl Iterator<Item = &Screen> {
        self.screens.values().filter(|s| s.is_window())
    }

    /// The PC's own displays, connected or not.
    pub(super) fn physical(&self) -> impl Iterator<Item = &Screen> {
        self.screens.values().filter(|s| s.is_physical())
    }

    /// The lowest output number no screen has now (nor a window panel on its way).
    pub(super) fn free_number(&self) -> u32 {
        (1..)
            .find(|n| {
                !self
                    .screens
                    .values()
                    .any(|s| s.number == *n && (!s.is_physical() || s.streaming))
                    && !self.detaching.values().any(|(d, _)| d == n)
            })
            .unwrap_or(1)
    }

    /// Whether `id` is one of the virtual monitors.
    pub fn is_monitor(&self, id: ScreenId) -> bool {
        self.screens.get(&id).is_some_and(Screen::is_virtual)
    }

    /// Adds monitor `id` (a number never given before), at most `max` of them; its
    /// output takes the lowest free `Prostir-N`, so KScreen finds what it remembers
    /// for that name. It is made by the desktop's worker: one that
    /// cannot start stays, `Failed`, and keeps its place (it is tried again at the
    /// next session).
    pub fn add(&mut self, id: ScreenId, spec: MonitorSpec, max: usize) -> Result<()> {
        if self.monitor_count() >= max {
            bail!(UserError::Limit {
                max,
                of: Counted::Monitors
            });
        }
        let number = self.free_number();
        self.add_as(id, number, spec);
        // A headset that places the monitors gives this one its place once it shows
        // it; meanwhile the desktop has it at a stand-in place.
        if self.headset_places {
            self.placing.add(id, Instant::now());
        }
        Ok(())
    }

    /// Whether the desktop has monitor `id` where it stays (else at a stand-in place
    /// until the headset placed it, for up to [`placing::PLACE_WAIT`]).
    pub fn desk_known(&self, id: ScreenId, now: Instant) -> bool {
        self.placing.known(id, now)
    }

    /// When a stand-in place starts to count, if one is waited for.
    pub fn placing_due(&self, now: Instant) -> Option<Instant> {
        self.placing.due(now)
    }

    /// No headset is connected any more: no monitor waits for its place, and the next
    /// main headset places the monitors once it sends an arrangement.
    pub fn no_headset(&mut self) {
        self.headset_places = false;
        self.placing.clear();
    }

    /// Adds monitor `id` (output `number`) to the set, at the end of the bottom row
    /// unless the rows have it, `Starting`.
    pub(super) fn add_as(&mut self, id: ScreenId, number: u32, spec: MonitorSpec) {
        self.screens.insert(
            id,
            Screen::starting(id, number, ScreenKind::Virtual { spec }),
        );
        if !self.rows.iter().flatten().any(|e| e.id == id) {
            match self.rows.last_mut() {
                Some(row) => row.push(desk::Entry::new(id)),
                None => self.rows.push(vec![desk::Entry::new(id)]),
            }
        }
    }

    /// The window panels (and those on their way).
    pub fn window_count(&self) -> usize {
        self.windows().count() + self.detaching.len()
    }

    /// The streams there are, or are to be: every monitor and window panel, and every
    /// display of the PC that is shown and connected.
    pub fn stream_count(&self) -> usize {
        self.screens
            .values()
            .filter(|s| {
                !s.is_physical()
                    || (s.is_shown_physical() && !matches!(s.state, ScreenState::Absent))
            })
            .count()
            + self.detaching.len()
    }

    pub fn monitor_count(&self) -> usize {
        self.monitors().count()
    }

    /// Whether a display of the PC streams to the headset.
    pub fn mirror_active(&self) -> bool {
        self.physical().any(|s| s.streaming)
    }

    /// The screen whose output is `key`.
    pub fn id_of_output(&self, key: &OutputKey) -> Option<ScreenId> {
        self.screens
            .values()
            .find(|s| s.output_key() == Some(key))
            .map(|s| s.id)
    }

    /// Removes monitor `id`, or closes window panel `id`.
    pub fn remove(&mut self, id: ScreenId) -> Result<()> {
        if self.screens.get(&id).is_some_and(Screen::is_window) {
            self.close_window_panel(id);
            return Ok(());
        }
        if !self.is_monitor(id) {
            bail!(UserError::UnknownDisplay);
        }
        if self.monitor_count() == 1 {
            bail!(UserError::LastDisplay);
        }
        self.screens.remove(&id);
        self.forget(id);
        Ok(())
    }

    /// Monitor `id`, no longer among the screens, leaves the order and the desktop's
    /// arrangement.
    pub(super) fn forget(&mut self, id: ScreenId) {
        self.too_big.remove(&id);
        self.placing.remove(id);
        for row in &mut self.rows {
            row.retain(|e| e.id != id);
        }
        self.rows.retain(|r| !r.is_empty());
        if self.primary == Some(id) {
            self.primary = None;
        }
        self.ensure_primary();
    }

    /// Every screen in the order of the rows, then those the rows leave out.
    pub(super) fn display_order(&self) -> Vec<ScreenId> {
        let mut order: Vec<ScreenId> = self
            .effective_rows()
            .iter()
            .flatten()
            .map(|e| e.id)
            .collect();
        let rest: Vec<ScreenId> = self
            .screens
            .keys()
            .filter(|id| !order.contains(id))
            .copied()
            .collect();
        order.extend(rest);
        order
    }

    /// The monitors left to right, row by row from the top: as the rows have them.
    pub(super) fn order(&self) -> Vec<ScreenId> {
        self.effective_rows()
            .iter()
            .flatten()
            .map(|e| e.id)
            .filter(|id| self.is_monitor(*id))
            .collect()
    }

    /// Makes monitor or display of the PC `id` the desktop's primary one (where the
    /// panel lives).
    pub fn set_primary(&mut self, id: ScreenId) -> Result<()> {
        if !self
            .screens
            .get(&id)
            .is_some_and(|s| s.is_virtual() || s.is_physical())
        {
            bail!(UserError::UnknownDisplay);
        }
        self.primary = Some(id);
        Ok(())
    }

    /// Makes the monitors match `specs` left to right (a headset profile), and gives
    /// their numbers in that order. Monitors of a matching size are kept, so their
    /// windows stay where they are.
    pub fn set_monitors(
        &mut self,
        specs: &[MonitorSpec],
        primary: Option<usize>,
        new_id: &mut dyn FnMut() -> ScreenId,
    ) -> Result<Vec<ScreenId>> {
        if specs.is_empty() || specs.len() > MONITORS_NO_LIMITS {
            bail!("1 to {MONITORS_NO_LIMITS} monitors");
        }
        for s in specs {
            s.validate()?;
        }
        let mut unused: Vec<ScreenId> = self.order();
        let mut plan: Vec<Option<ScreenId>> = specs
            .iter()
            .map(|s| {
                let i = unused.iter().position(|id| {
                    self.screens
                        .get(id)
                        .and_then(|m| m.kind.spec())
                        .is_some_and(|m| m == s)
                })?;
                Some(unused.remove(i))
            })
            .collect();
        // Remove before adding (to stay under the limit), but never the last one.
        let mut last = None;
        for id in unused {
            if self.monitor_count() > 1 {
                self.remove(id)?;
            } else {
                last = Some(id);
            }
        }
        for (slot, spec) in plan.iter_mut().zip(specs) {
            if slot.is_none() {
                let id = new_id();
                self.add(id, spec.clone(), usize::MAX)?;
                *slot = Some(id);
            }
        }
        if let Some(id) = last {
            self.remove(id)?;
        }
        let ids: Vec<ScreenId> = plan.into_iter().flatten().collect();
        self.set_order(&ids)?;
        if let Some(&p) = primary.and_then(|i| ids.get(i)) {
            self.set_primary(p)?;
        }
        Ok(ids)
    }

    /// The headset's rows, kept in step with the monitors (see [`rows_in_step`]), with
    /// the displays of the PC where the headset put them, while they stream.
    pub(super) fn effective_rows(&self) -> Vec<Vec<desk::Entry<ScreenId>>> {
        self.rows_with(|s| s.is_physical() && s.streaming)
    }

    /// The rows with every monitor, and the other screens `kept` says.
    fn rows_with(&self, kept: impl Fn(&Screen) -> bool) -> Vec<Vec<desk::Entry<ScreenId>>> {
        let monitors: Vec<ScreenId> = self.monitors().map(|s| s.id).collect();
        rows_in_step(
            &self.rows,
            &monitors,
            |id| self.is_monitor(id),
            |id| self.screens.get(&id).is_some_and(&kept),
        )
    }

    /// Arranges the desktop's outputs in the rows the headset shows them in.
    pub fn set_layout(&mut self, rows: &[state::Row]) {
        // Offsets are desktop pixels; anything this far out is nonsense.
        let far = |v: Option<i32>| v.map(|v| v.clamp(-desk::FAR, desk::FAR));
        let mut rows: Vec<Vec<desk::Entry<ScreenId>>> = rows
            .iter()
            .map(|r| {
                r.entries
                    .iter()
                    .map(|e| desk::Entry {
                        id: ScreenId::from_wire(e.id),
                        center: far(e.center),
                        middle: far(e.middle),
                    })
                    .collect()
            })
            .collect();
        // A laptop screen's stream is cut for a moment whenever KDE rearranges its
        // outputs, and a headset that sends a layout meanwhile leaves it out; a display
        // of the PC that is not connected is not in the headset's rows either. Their
        // spots are kept, or they would come back elsewhere, the headset would move
        // them back, and that would cut them again, round and round.
        let resting: Vec<ScreenId> = self
            .physical()
            .filter(|s| !s.streaming)
            .map(|s| s.id)
            .collect();
        for id in resting {
            if rows.iter().flatten().any(|e| e.id == id) {
                continue;
            }
            for (i, old) in self.rows.iter().enumerate() {
                let Some(spot) = old.iter().find(|e| e.id == id) else {
                    continue;
                };
                let at = if i < rows.len() {
                    i
                } else {
                    rows.push(Vec::new());
                    rows.len() - 1
                };
                rows[at].push(*spot);
                break;
            }
        }
        // Monitors the headset left out stay, at the end of the bottom row, in the order
        // they had.
        let missing: Vec<desk::Entry<ScreenId>> = self
            .order()
            .into_iter()
            .filter(|id| !rows.iter().flatten().any(|e| e.id == *id))
            .map(desk::Entry::new)
            .collect();
        if !missing.is_empty() {
            match rows.last_mut() {
                Some(row) => row.extend(missing),
                None => rows.push(missing),
            }
        }
        // A headset that sends where the monitors stand places a new one itself: where
        // it stays once the desktop is laid out with it.
        let placed = |id: ScreenId| {
            rows.iter()
                .flatten()
                .any(|e| e.id == id && e.center.is_some() && e.middle.is_some())
        };
        self.headset_places |= rows
            .iter()
            .flatten()
            .any(|e| e.center.is_some() && e.middle.is_some());
        self.placing.arranged(placed);
        self.rows = rows;
    }

    fn set_order(&mut self, ids: &[ScreenId]) -> Result<()> {
        let mut sorted = ids.to_vec();
        sorted.sort_unstable();
        let current: Vec<ScreenId> = self.monitors().map(|s| s.id).collect();
        if sorted != current {
            bail!(
                "{}",
                t(
                    "Порядок не збігається з моніторами",
                    "The order does not match the monitors"
                )
            );
        }
        // One row, in this order.
        self.rows = vec![ids.iter().map(|&id| desk::Entry::new(id)).collect()];
        Ok(())
    }

    /// Without a chosen primary, the middle monitor gets the Plasma panel.
    pub(super) fn ensure_primary(&mut self) {
        if self
            .primary
            .is_some_and(|p| self.is_monitor(p) || self.is_physical(p))
        {
            return;
        }
        let ids = self.order();
        if let Some(&id) = ids.get(ids.len().saturating_sub(1) / 2) {
            let _ = self.set_primary(id);
        }
    }

    pub fn primary(&self) -> Option<ScreenId> {
        self.primary
    }

    /// The desktop's name for the output of screen `id`, while it has one.
    pub fn output_of(&self, id: ScreenId) -> Option<String> {
        let screen = self.screens.get(&id)?;
        match &screen.kind {
            ScreenKind::Physical { key, .. } => Some(key.connector.clone()),
            _ => screen.output_key().map(|k| k.as_str().to_owned()),
        }
    }

    /// Where screen `id` is on the desktop now, if it is there.
    fn desk_rect(&self, id: ScreenId) -> Option<state::DeskRect> {
        let screen = self.screens.get(&id)?;
        let name = match &screen.kind {
            ScreenKind::Physical { key, .. } => key.connector.as_str(),
            _ => screen.output_key()?.as_str(),
        };
        self.on_desk.get(name).copied()
    }

    /// Gives monitor `id` the scale `percent` (one the desktop gives its size): its
    /// output is made anew under the same name, so the desktop keeps its place.
    pub fn rescale(&mut self, id: ScreenId, percent: u16) -> Result<()> {
        let Some(spec) = self
            .screens
            .get(&id)
            .filter(|s| s.is_virtual())
            .and_then(|s| s.kind.spec())
            .cloned()
        else {
            bail!(UserError::UnknownDisplay);
        };
        let wanted = MonitorSpec {
            scale: f64::from(percent) / 100.0,
            ..spec
        };
        let allowed = self.scales_for(spec.width, spec.height).allows(percent);
        if !allowed || wanted.validate().is_err() {
            bail!(UserError::BadSize {
                w: spec.width,
                h: spec.height,
                scale: percent
            });
        }
        if wanted.percent() == spec.percent() {
            return Ok(());
        }
        if let Some(screen) = self.screens.get_mut(&id) {
            // The worker makes its output anew.
            screen.encoding = None;
            screen.kind = ScreenKind::Virtual { spec: wanted };
            screen.state = ScreenState::Starting;
        }
        Ok(())
    }

    /// Moves screen `id` on the desktop to about `x`, `y` (its top-left corner, in
    /// logical pixels): into the row it then is in, between the neighbours left and
    /// right of it. The desktop's arrangement then lines it up (see `layout::desk`),
    /// until the main headset arranges the monitors again.
    pub fn move_to(&mut self, id: ScreenId, x: i32, y: i32) -> Result<()> {
        let Some(me) = self.desk_rect(id) else {
            bail!(UserError::UnknownDisplay);
        };
        if !self.rows.iter().flatten().any(|e| e.id == id) {
            bail!(UserError::UnknownDisplay);
        }
        let (cx, cy) = (x + me.w as i32 / 2, y + me.h as i32 / 2);
        let mut rows = self.rows.clone();
        for row in &mut rows {
            row.retain(|e| e.id != id);
        }
        rows.retain(|r| !r.is_empty());
        // Each row's height on the desktop, from what of it is there.
        let span = |row: &Vec<desk::Entry<ScreenId>>| {
            row.iter()
                .filter_map(|e| self.desk_rect(e.id))
                .map(|r| (r.y, r.y + r.h as i32))
                .reduce(|(a, b), (c, d)| (a.min(c), b.max(d)))
        };
        let spans: Vec<Option<(i32, i32)>> = rows.iter().map(span).collect();
        let row = spans
            .iter()
            .position(|s| s.is_some_and(|(top, bottom)| (top..bottom).contains(&cy)));
        let entry = desk::Entry::new(id);
        match row {
            Some(r) => {
                let at = rows[r]
                    .iter()
                    .filter(|e| {
                        self.desk_rect(e.id)
                            .is_some_and(|d| d.x + (d.w as i32) / 2 < cx)
                    })
                    .count();
                rows[r].insert(at, entry);
            }
            None => {
                // Above every row or under them: a row of its own there.
                let above = spans.iter().flatten().all(|(top, _)| cy < *top);
                if above {
                    rows.insert(0, vec![entry]);
                } else {
                    rows.push(vec![entry]);
                }
            }
        }
        self.rows = rows;
        Ok(())
    }

    /// Tries monitor `id` again now, if it failed (`UnknownDisplay` if there is none);
    /// what came of it shows in its state.
    pub fn retry(&mut self, id: ScreenId) -> Result<()> {
        let Some(screen) = self.screens.get(&id) else {
            bail!(UserError::UnknownDisplay);
        };
        // One too big for a headset waits for one it fits instead.
        let failed = matches!(screen.state, ScreenState::Failed { .. });
        if failed && screen.is_virtual() && !self.too_big.contains_key(&id) {
            // Asked for: at once, and counted from the first pause again.
            if let Some(s) = self.screens.get_mut(&id) {
                s.failures = 0;
            }
            self.try_again(id);
        }
        Ok(())
    }

    /// The set and its rows as they are now, to keep; none without monitors (none
    /// could stream again), so the set kept before stays for the next start.
    pub fn saved_screens(&self) -> Option<SavedScreens> {
        let specs: BTreeMap<ScreenId, MonitorSpec> = self
            .monitors()
            .filter_map(|s| Some((s.id, s.kind.spec()?.clone())))
            .collect();
        if specs.is_empty() {
            return None;
        }
        let physical = self
            .physical()
            .filter_map(|s| match &s.kind {
                ScreenKind::Physical {
                    key,
                    builtin,
                    shown,
                    ..
                } => Some((
                    s.id,
                    Physical {
                        key: key.clone(),
                        builtin: *builtin,
                        shown: *shown,
                        seen: self.physical_seen.get(&s.id).copied().unwrap_or(0),
                    },
                )),
                _ => None,
            })
            .collect();
        Some(SavedScreens {
            specs,
            physical,
            // The PC's displays keep their places while not connected or not shown.
            rows: self.rows_with(Screen::is_physical),
            primary: self.primary,
        })
    }
}
