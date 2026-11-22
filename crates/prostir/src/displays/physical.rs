//! The PC's own displays: found by EDID or connector, shown in the headset or not,
//! forgotten when long absent.

use super::*;

impl Displays {
    /// What screen `s` is called where people see it (`Display.name`): a display of the
    /// PC by its label, with its connector added when another display of the PC has
    /// the same label (two monitors of one model); any other by its name.
    pub(super) fn shown_name(&self, s: &Screen) -> String {
        let ScreenKind::Physical { key, label, .. } = &s.kind else {
            return s.name();
        };
        let clash = self.physical().any(|o| {
            o.id != s.id && matches!(&o.kind, ScreenKind::Physical { label: l, .. } if l == label)
        });
        name_apart(label, &key.connector, clash)
    }

    /// Follows the PC's own displays (`seen`, as the desktop listed them): each
    /// connected one is matched to one known (see [`match_physical`]); one seen for the
    /// first time gets a number from `new_id` and is shown if it is the one the desktop
    /// shows by default and the laptop screen is wanted. Shown ones that are connected
    /// stream; one not connected is `Absent`, keeping its number and place.
    pub(super) fn sync_physical(
        &mut self,
        seen: &[PhysicalDisplay],
        new_id: &mut dyn FnMut() -> ScreenId,
    ) {
        let known: Vec<(ScreenId, PhysicalKey)> = self
            .physical()
            .filter_map(|s| match &s.kind {
                ScreenKind::Physical { key, .. } => Some((s.id, key.clone())),
                _ => None,
            })
            .collect();
        let matched = match_physical(&known, seen);
        let default = seen.iter().find(|d| d.default).map(|d| d.key.clone());
        let mut present = Vec::new();
        for (display, id) in seen.iter().zip(matched) {
            let id = match id {
                Some(id) => id,
                None => self.found_physical(display, seen, default.as_ref(), new_id),
            };
            present.push(id);
            if let Some(s) = self.screens.get_mut(&id)
                && let ScreenKind::Physical {
                    key, label, size, ..
                } = &mut s.kind
            {
                label.clone_from(&display.label);
                if display.size.is_some() {
                    *size = display.size;
                }
                if key.connector != display.key.as_str() {
                    info!("{id} is on {} now", display.key);
                    key.connector = display.key.to_string();
                }
                // Known by its connector until now: by its EDID from now on.
                if key.edid.is_none()
                    && let Some(edid) = unique_edid(display, seen)
                {
                    info!("{id} says it is {edid}");
                    key.edid = Some(edid.to_owned());
                }
            }
        }
        let ids: Vec<ScreenId> = self.physical().map(|s| s.id).collect();
        for id in ids {
            let shown = self.screens.get(&id).is_some_and(Screen::is_shown_physical);
            let here = present.contains(&id);
            let Some(s) = self.screens.get_mut(&id) else {
                continue;
            };
            let streaming = s.streaming;
            // Seen now: when it comes or goes, and every hour while connected (each
            // change rewrites screens.txt).
            let seen = self.physical_seen.get(&id).copied().unwrap_or(0);
            if here == matches!(s.state, ScreenState::Absent)
                || (here && now_secs().saturating_sub(seen) >= SEEN_EVERY)
            {
                self.physical_seen.insert(id, now_secs());
            }
            if !here {
                if !matches!(s.state, ScreenState::Absent) {
                    info!("{} is not connected", s.name());
                }
                self.stop_physical(id);
                if let Some(s) = self.screens.get_mut(&id) {
                    s.state = ScreenState::Absent;
                }
            } else if !shown {
                if streaming {
                    self.stop_physical(id);
                }
                if let Some(s) = self.screens.get_mut(&id)
                    && !matches!(s.state, ScreenState::Running)
                {
                    s.state = ScreenState::Running;
                }
            } else if !streaming && !self.is_too_big(id) {
                // One too big for this headset waits for another headset or codec.
                self.start_physical(id);
            }
        }
        self.forget_long_absent();
    }

    /// The displays of the PC connected now were seen now (the app quits: what is
    /// saved says when each was last seen).
    pub fn note_connected(&mut self) {
        let now = now_secs();
        let connected: Vec<ScreenId> = self
            .physical()
            .filter(|s| !matches!(s.state, ScreenState::Absent))
            .map(|s| s.id)
            .collect();
        for id in connected {
            self.physical_seen.insert(id, now);
        }
    }

    /// Forgets the displays of the PC not connected beyond [`ABSENT_KEPT`], the least
    /// recently seen first: they leave the list, the rows and the saved set.
    fn forget_long_absent(&mut self) {
        let absent: Vec<(ScreenId, u64)> = self
            .physical()
            .filter(|s| matches!(s.state, ScreenState::Absent))
            .map(|s| (s.id, self.physical_seen.get(&s.id).copied().unwrap_or(0)))
            .collect();
        let gone = least_recently_seen(absent, ABSENT_KEPT);
        for id in &gone {
            info!("forgetting {id}, not connected for the longest");
            self.screens.remove(id);
            self.physical_seen.remove(id);
            self.forget(*id);
        }
    }

    /// A display of the PC seen for the first time: a new number (the laptop screen of
    /// earlier versions keeps 100 and its place), shown if it is the one the desktop
    /// shows by default and the laptop screen is wanted. Known by its EDID where no
    /// other connected display says the same, else by its connector.
    fn found_physical(
        &mut self,
        display: &PhysicalDisplay,
        seen: &[PhysicalDisplay],
        default: Option<&OutputKey>,
        new_id: &mut dyn FnMut() -> ScreenId,
    ) -> ScreenId {
        let is_default = default == Some(&display.key);
        let legacy = ScreenId::saved(LEGACY_LAPTOP);
        let id = if is_default
            && !self.screens.contains_key(&legacy)
            && self.rows.iter().flatten().any(|e| e.id == legacy)
        {
            legacy
        } else {
            new_id()
        };
        let unique = |edid: &String| {
            seen.iter()
                .filter(|d| d.edid.as_ref() == Some(edid))
                .count()
                == 1
        };
        let key = PhysicalKey {
            edid: display.edid.clone().filter(unique),
            connector: display.key.to_string(),
        };
        let shown = is_default && self.settings.mirror;
        self.physical_seen.insert(id, now_secs());
        info!(
            "{} is {id} ({}), {}",
            display.key,
            key.edid.as_deref().unwrap_or("no EDID"),
            if shown { "shown" } else { "not shown" }
        );
        let kind = ScreenKind::Physical {
            key,
            label: display.label.clone(),
            builtin: display.builtin,
            shown,
            size: display.size,
        };
        self.screens.insert(id, Screen::starting(id, 0, kind));
        id
    }

    /// Streams display `id` of the PC: `Starting` until the worker says it streams.
    fn start_physical(&mut self, id: ScreenId) {
        let number = self.free_number();
        let Some(s) = self.screens.get_mut(&id) else {
            return;
        };
        s.number = number;
        s.streaming = true;
        s.state = ScreenState::Starting;
    }

    /// Stops the stream of display `id` of the PC, if it streams.
    pub(super) fn stop_physical(&mut self, id: ScreenId) {
        let Some(s) = self.screens.get_mut(&id) else {
            return;
        };
        s.streaming = false;
        s.encoding = None;
        s.output = None;
        s.number = 0;
    }

    /// Shows or hides display `id` of the PC in the headset, with at most `max` of them
    /// shown among those connected: one not connected (a closed laptop's panel) shows
    /// nothing, so it counts for none.
    pub fn show_physical(&mut self, id: ScreenId, show: bool, max: usize) -> Result<()> {
        let others = self
            .physical()
            .filter(|s| s.id != id && s.is_shown_physical())
            .filter(|s| !matches!(s.state, ScreenState::Absent))
            .count();
        let Some(ScreenKind::Physical { shown, .. }) =
            self.screens.get_mut(&id).map(|s| &mut s.kind)
        else {
            bail!(UserError::UnknownDisplay);
        };
        if show && others >= max {
            bail!(UserError::Limit {
                max,
                of: Counted::Physical
            });
        }
        *shown = show;
        // Starting or not at once, as the headset is told; the PC's displays are looked
        // at again right away.
        let here = self
            .screens
            .get(&id)
            .is_some_and(|s| !matches!(s.state, ScreenState::Absent));
        let streaming = self.screens.get(&id).is_some_and(|s| s.streaming);
        if show && here && !streaming && !self.is_too_big(id) {
            self.start_physical(id);
        } else if !show && streaming {
            self.stop_physical(id);
            if let Some(s) = self.screens.get_mut(&id) {
                s.state = ScreenState::Running;
            }
        }
        self.desk.ask(Job::LookPhysical);
        Ok(())
    }

    /// The displays of the PC itself, connected or not.
    pub fn physical_ids(&self) -> Vec<ScreenId> {
        self.physical().map(|s| s.id).collect()
    }

    /// Whether `id` is a display of the PC itself that the headset shows.
    pub fn is_shown_physical(&self, id: ScreenId) -> bool {
        self.screens.get(&id).is_some_and(Screen::is_shown_physical)
    }

    /// Whether `id` is a display of the PC itself.
    pub fn is_physical(&self, id: ScreenId) -> bool {
        self.screens.get(&id).is_some_and(Screen::is_physical)
    }

    /// The displays of the PC shown in the headset now.
    pub fn physical_shown(&self) -> usize {
        self.physical().filter(|s| s.is_shown_physical()).count()
    }

    /// Whether `id` is a display of the PC itself connected now.
    pub fn is_connected_physical(&self, id: ScreenId) -> bool {
        self.screens
            .get(&id)
            .is_some_and(|s| s.is_physical() && !matches!(s.state, ScreenState::Absent))
    }
}

/// Which known display of the PC (`known`: number and how it is known) each connected
/// one (`seen`) is: by its EDID where it says one that no other connected display says
/// too; else by its connector, among those known by their connector. Another display
/// in the same connector is another display. `None`: seen for the first time.
pub(super) fn match_physical(
    known: &[(ScreenId, PhysicalKey)],
    seen: &[PhysicalDisplay],
) -> Vec<Option<ScreenId>> {
    let mut taken: Vec<ScreenId> = Vec::new();
    seen.iter()
        .map(|d| {
            let unique = unique_edid(d, seen);
            let free = |id: &ScreenId| !taken.contains(id);
            // By its EDID; else by its connector, one known without an EDID (which
            // may say its EDID only now).
            let found = known
                .iter()
                .find(|(id, key)| free(id) && unique.is_some() && key.edid.as_deref() == unique)
                .or_else(|| {
                    known.iter().find(|(id, key)| {
                        free(id) && key.edid.is_none() && key.connector == d.key.as_str()
                    })
                });
            let id = found.map(|(id, _)| *id);
            taken.extend(id);
            id
        })
        .collect()
}

/// The EDID display `d` says, where no other connected display says the same.
pub(super) fn unique_edid<'a>(d: &'a PhysicalDisplay, seen: &[PhysicalDisplay]) -> Option<&'a str> {
    d.edid.as_deref().filter(|e| {
        seen.iter()
            .filter(|o| o.edid.as_deref() == Some(*e))
            .count()
            == 1
    })
}

/// Of the displays not connected (`absent`, each with when it was last seen), those
/// beyond the `keep` most recently seen.
pub(super) fn least_recently_seen(mut absent: Vec<(ScreenId, u64)>, keep: usize) -> Vec<ScreenId> {
    // The most recent first; of the same moment, the newer number.
    absent.sort_by(|a, b| b.1.cmp(&a.1).then(b.0.cmp(&a.0)));
    absent.into_iter().skip(keep).map(|(id, _)| id).collect()
}

/// A display of the PC's `label`, with its `connector` when it `clash`es with another's
/// (`DELL U2720Q (DP-1)`); as it is when the label is the connector already.
pub(super) fn name_apart(label: &str, connector: &str, clash: bool) -> String {
    if clash && label != connector && !connector.is_empty() {
        format!("{label} ({connector})")
    } else {
        label.to_owned()
    }
}
