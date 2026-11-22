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

    /// Screen `id` could not be made or start at `attempt` (or the desktop closed
    /// it): a display of the PC is tried again after a rest; a monitor's output goes
    /// too (no output on the desktop that nobody sees, where windows would vanish),
    /// and it waits for the next attempt, which the control loop gives after a pause.
    pub(super) fn failed(&mut self, id: ScreenId, attempt: u32, e: &anyhow::Error) {
        if let Some(slot) = self.slots.get_mut(&id) {
            stop_encoder(
                slot.encoder.take(),
                &slot.name,
                &self.shared.steps,
                &self.behind,
            );
            slot.started_for = None;
            let had_output = slot.output.take().is_some();
            if slot.is_physical() {
                slot.rest_until = Some(Instant::now() + PHYSICAL_REST);
            } else {
                slot.failed = Some(attempt);
                if had_output {
                    self.shared.router.remove_stream(id);
                    self.layout_due = true;
                    self.done(Done::Lost { id });
                }
            }
        }
        self.say_failed(id, attempt, e);
    }

    /// Says that screen `id` failed at `attempt`, and why.
    fn say_failed(&self, id: ScreenId, attempt: u32, e: &anyhow::Error) {
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

    /// Each screen whose stream broke or whose output the desktop closed is put right
    /// by itself, the others go on: a display of the PC rests and is shown again; a
    /// monitor whose encoder broke keeps its output and its encoder starts again, one
    /// the desktop closed is made again, each after its pause (see
    /// [`Worker::encoder_broke`] and [`Worker::failed`]); a window panel closes, its
    /// window back where it was.
    pub(super) fn check_closed(&mut self) {
        let mut stopped = Vec::new();
        let mut gone: Vec<(ScreenId, Option<String>)> = Vec::new();
        for (id, why) in self.shared.router.take_broken() {
            match self.slots.get(&id) {
                Some(slot) if slot.is_physical() => stopped.push(id),
                Some(_) => gone.push((id, Some(why))),
                None => {}
            }
        }
        for (id, slot) in &self.slots {
            let closed = slot
                .output
                .as_ref()
                .is_some_and(|o| o.health() == OutputHealth::Closed);
            if !closed {
                continue;
            }
            if slot.is_physical() {
                stopped.push(*id);
            } else {
                warn!("the desktop closed {}", slot.name);
                gone.push((*id, None));
            }
        }
        for (id, broke) in gone {
            let panel = self.slots.get(&id).is_some_and(Slot::is_panel);
            if panel {
                warn!("{id}: its stream or output is gone: the window panel closes");
                self.remove(id, true);
                self.done(Done::PanelGone { id });
                continue;
            }
            let attempt = self.attempt_of(id);
            match broke {
                Some(why) => self.encoder_broke(id, attempt, &why),
                None => {
                    let e = anyhow!("{}", t("стільниця закрила його", "the desktop closed it"))
                        .context(UserError::Desktop);
                    self.failed(id, attempt, &e);
                }
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

impl Worker {
    /// The attempt the wanted state has screen `id` at.
    fn attempt_of(&self, id: ScreenId) -> u32 {
        self.wanted
            .as_ref()
            .and_then(|w| w.screens.iter().find(|(i, _)| *i == id))
            .map_or(0, |(_, w)| w.attempt)
    }

    /// The encoder of monitor `id` broke at `attempt` (`why`): it stops, the output
    /// stays, and it starts again at the next attempt (after its pause, see
    /// `screen::retry_pause`). One that broke soon after it started, again with the
    /// same encoder, is not used for this monitor for a while: the next encoder is
    /// (`Catalog::did_not_open`), so a failure that comes back each time does not
    /// come back for ever.
    pub(super) fn encoder_broke(&mut self, id: ScreenId, attempt: u32, why: &str) {
        let Some(slot) = self.slots.get_mut(&id) else {
            return;
        };
        let Some(encoder) = slot.encoder.take() else {
            return;
        };
        let (kind, codec, soon) = (
            encoder.kind(),
            encoder.codec(),
            encoder.since.elapsed() < BROKE_SOON,
        );
        slot.started_for = None;
        stop_encoder(Some(encoder), &slot.name, &self.shared.steps, &self.behind);
        self.shared.router.remove_stream(id);
        let breaks = match slot.breaks {
            Some((k, n)) if k == (kind, codec) && soon => n + 1,
            _ => 1,
        };
        // While encoders are left behind, the sessions held are not known.
        let teach = breaks >= BREAKS_OF_A_KIND && self.behind.total() == 0;
        slot.breaks = (!teach).then_some(((kind, codec), breaks));
        slot.failed = Some(attempt);
        warn!(
            "{}: its encoder broke ({breaks} in a row): starting it again shortly",
            slot.name
        );
        if teach {
            let held = self
                .running_encoders(&[id])
                .get(&kind)
                .copied()
                .unwrap_or(0);
            let failure = NotOpened {
                short: false,
                why: why.to_owned(),
                resumed: false,
            };
            crate::encoder::catalog().did_not_open(kind, codec, id, &failure, held);
        }
        self.layout_due = true;
        let e = anyhow!("{why}").context(UserError::Encoder);
        self.say_failed(id, attempt, &e);
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
