//! The worker's outputs: made and removed, made again when the desktop lost them, and
//! the PC's own displays looked at.

use super::*;

impl Worker {
    /// Removes screen `id`'s output (a window panel's window goes back where it was
    /// with `put_back`).
    pub(super) fn remove(&mut self, id: ScreenId, put_back: bool) {
        let Some(mut slot) = self.slots.remove(&id) else {
            return;
        };
        if slot.is_panel()
            && let Some(windows) = self.desktop.windows()
        {
            if let Some(key) = slot.key() {
                windows.forget_panel(key);
            }
            if put_back
                && let Made::Panel { origin, .. } = &slot.made
                && let Some(on) = &origin.output
                && let Err(e) = windows.put_back(origin, on)
            {
                warn!("cannot put the window back on {on}: {e:#}");
            }
        }
        if slot.key().is_some() && slot.key() == self.primary_made.as_ref() {
            self.primary_made = None;
        }
        stop_encoder(
            slot.encoder.take(),
            &slot.name,
            &self.shared.steps,
            &self.behind,
        );
        self.shared.router.remove_stream(id);
        self.begin(format!("removing the output of {}", slot.name));
        drop(slot);
        self.layout_due = true;
    }

    /// Makes screen `id`'s output: a virtual monitor, or a display of the PC shown.
    pub(super) fn make(&mut self, id: ScreenId) {
        let Some((_, w)) = self
            .wanted
            .as_ref()
            .and_then(|w| w.screens.iter().find(|(i, _)| *i == id))
            .cloned()
        else {
            return;
        };
        let made = match &w.what {
            What::Monitor { spec, number } => {
                let made = self.make_monitor(id, *number, spec);
                (
                    Made::Monitor {
                        spec: spec.clone(),
                        number: *number,
                    },
                    made.context(UserError::Desktop),
                )
            }
            What::Physical { key, number } => (
                Made::Physical {
                    key: key.clone(),
                    number: *number,
                },
                self.desktop.show(key).context(UserError::Capture),
            ),
            What::Panel { .. } => return,
        };
        let slot = self
            .slots
            .entry(id)
            .or_insert_with(|| Slot::new(made.0.clone(), w.name.clone()));
        slot.made = made.0;
        slot.name.clone_from(&w.name);
        match made.1 {
            Ok(output) => {
                let key = output.key().clone();
                // Listed at its own scale as it was made: seen at that one already, so
                // a scale the person gives before the next look is theirs, and taken.
                let own = match &slot.made {
                    Made::Monitor { spec, .. } => output.desk_scale() == Some(spec.percent()),
                    _ => false,
                };
                slot.output = Some(output);
                slot.failed = None;
                slot.rest_until = None;
                slot.closed_told = false;
                slot.size_guard = SizeGuard::default();
                // Made anew: seen at its scale again before another is taken.
                slot.scale_seen = None;
                slot.seen_own = own;
                if !slot.is_physical() {
                    self.wallpapers_due = true;
                }
                self.layout_due = true;
                self.done(Done::Made { id, key });
            }
            Err(e) => {
                error!("{} cannot start: {e:#}", w.name);
                self.failed(id, w.attempt, &e);
            }
        }
    }

    /// Screen `id` could not be made or start at `attempt`: a monitor waits for the
    /// next attempt, a display of the PC is tried again after a rest.
    pub(super) fn failed(&mut self, id: ScreenId, attempt: u32, e: &anyhow::Error) {
        if let Some(slot) = self.slots.get_mut(&id) {
            if slot.is_physical() {
                stop_encoder(
                    slot.encoder.take(),
                    &slot.name,
                    &self.shared.steps,
                    &self.behind,
                );
                slot.output = None;
                slot.rest_until = Some(Instant::now() + PHYSICAL_REST);
            } else {
                slot.failed = Some(attempt);
            }
        }
        self.done(Done::Failed {
            id,
            attempt,
            error: code_of(e),
            why: format!("{e:#}"),
        });
    }

    /// A new virtual monitor (or window panel) for screen `id`, as output `number`.
    pub(super) fn make_monitor(
        &mut self,
        id: ScreenId,
        number: u32,
        spec: &MonitorSpec,
    ) -> Result<Box<dyn Output>> {
        let caps = self.desktop.caps();
        if let Err(no) = &caps.virtual_monitors {
            return Err(anyhow!("{}", no.why).context(no.error.clone()));
        }
        let spec = VirtualSpec {
            id,
            number,
            w: spec.width,
            h: spec.height,
            scale: spec.percent(),
            refresh: caps.refresh_for_new(),
        };
        self.desktop.make(&spec)
    }

    /// The PC's own displays, every 2 s: said to the control loop; where none is on
    /// though one is to be shown (a laptop's panel left off), one is switched on.
    pub(super) fn look_at_physical(&mut self) {
        if Instant::now() < self.physical_due {
            return;
        }
        self.physical_due = Instant::now() + PHYSICAL_EVERY;
        self.begin("looking at the PC's displays");
        let seen = self.desktop.physical();
        if seen.is_empty() && self.wanted.as_ref().is_some_and(|w| w.wake_physical) {
            self.desktop.wake_physical();
        }
        self.end();
        self.done(Done::Physical(seen));
    }

    /// Makes monitor `id`'s output anew with the same number and spec, when nothing
    /// else gives it back its size. Its windows move to the other screens meanwhile.
    pub(super) fn recreate(&mut self, id: ScreenId) {
        let Some(slot) = self.slots.get_mut(&id) else {
            return;
        };
        let guard = std::mem::take(&mut slot.size_guard);
        stop_encoder(
            slot.encoder.take(),
            &slot.name,
            &self.shared.steps,
            &self.behind,
        );
        slot.started_for = None;
        drop(slot.output.take());
        self.begin(format!("making the output of {id} again"));
        self.make(id);
        if let Some(slot) = self.slots.get_mut(&id) {
            slot.size_guard = guard;
        }
        if self.slots.get(&id).is_some_and(|s| s.output.is_some()) {
            self.start(id, None);
            if self.primary_made.as_ref() == self.slots.get(&id).and_then(Slot::key) {
                self.primary_made = None;
            }
        } else {
            // Not made again: no headset waits for the stream it had.
            self.shared.router.remove_stream(id);
        }
    }

    /// Monitors the desktop closed are said once (the control loop makes them anew for
    /// the next session); displays of the PC whose stream stopped rest and are shown
    /// again.
    pub(super) fn check_closed(&mut self) {
        let mut stopped = self.shared.router.take_broken();
        for (id, slot) in &mut self.slots {
            let closed = slot
                .output
                .as_ref()
                .is_some_and(|o| o.health() == OutputHealth::Closed);
            if !closed {
                continue;
            }
            if slot.is_physical() {
                stopped.push(*id);
            } else if !std::mem::replace(&mut slot.closed_told, true) {
                warn!("the desktop closed {}", slot.name);
                self.shared.events.send(Event::Desk(
                    self.shared.generation,
                    Done::Closed { id: *id },
                ));
            }
        }
        stopped.sort_unstable();
        stopped.dedup();
        for id in stopped {
            let Some(slot) = self.slots.get_mut(&id) else {
                continue;
            };
            info!(
                "the stream of {} stopped, starting it again shortly",
                slot.name
            );
            stop_encoder(
                slot.encoder.take(),
                &slot.name,
                &self.shared.steps,
                &self.behind,
            );
            slot.started_for = None;
            drop(slot.output.take());
            slot.rest_until = Some(Instant::now() + PHYSICAL_REST);
            self.shared.router.remove_stream(id);
            self.layout_due = true;
            self.done(Done::Lost { id });
        }
    }
}

/// The first time in a run that a screen sharing program resizes a monitor (output
/// `Prostir-number`, as the desktop names it): says the size is back, and how to
/// share without the jumps.
pub(super) fn show_sharing_tip(number: u32) {
    static SHOWN: AtomicBool = AtomicBool::new(false);
    if SHOWN.swap(true, Ordering::Relaxed) {
        return;
    }
    let body = format!(
        "{} {}. {}",
        t(
            "Програма, що показує екран, змінила розмір монітора",
            "A screen sharing program changed the size of monitor"
        ),
        number,
        t(
            "Розмір повернено, але показ може стрибати. Щоб цього не було, показуйте вікно програми, а не монітор.",
            "Its size is back, but the share may jump. To avoid that, share an app window instead of the monitor."
        )
    );
    dialogs::notify(t("Показ екрана", "Screen sharing"), &body);
}
