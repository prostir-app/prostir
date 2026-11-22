//! Jobs someone waits for: where the mouse is, a window brought to a monitor or
//! detached into a panel, a screenshot; and panels whose window left.

use super::*;

impl Worker {
    /// One job.
    pub(super) fn job(&mut self, job: Job) {
        match job {
            Job::Tick => self.upkeep(),
            Job::Focus => {
                let key = self
                    .desktop
                    .windows()
                    .and_then(crate::platform::Windows::active_output);
                self.done(Done::Focus(key));
            }
            Job::LookPhysical => {
                self.physical_due = Instant::now();
                self.look_at_physical();
            }
            Job::Fresh(ids) => {
                for id in ids {
                    let running = self
                        .slots
                        .get(&id)
                        .and_then(|s| s.encoder.as_ref())
                        .is_some_and(|e| !e.is_paused());
                    if running {
                        self.begin(format!("starting the stream of {id} over"));
                        self.set_paused(id, false);
                        self.end();
                    }
                }
            }
            Job::Screenshot { until, .. }
            | Job::FindMouse { until, .. }
            | Job::WindowHere { until, .. }
                if Instant::now() > until =>
            {
                info!("a job of the desktop came too late for whoever asked: dropped");
            }
            Job::Screenshot { job, id, .. } => {
                self.begin(format!("a picture of {id}"));
                let result = self.screenshot(id);
                self.end();
                self.done(Done::Job { job, result });
            }
            Job::FindMouse { job, .. } => {
                info!("looking where the mouse is");
                self.begin("finding the mouse");
                let result = self.find_mouse();
                self.end();
                self.done(Done::Job { job, result });
            }
            Job::MouseTo { id, x, y } => {
                self.begin("moving the mouse");
                self.mouse_to(id, x, y);
                self.end();
            }
            Job::WindowHere { job, id, .. } => {
                self.begin(format!("moving the active window to {id}"));
                let result = self.window_here(id).map(|()| JobOk::Done);
                self.end();
                self.done(Done::Job { job, result });
            }
        }
    }

    /// A panel whose window was closed (or moved away) goes too.
    pub(super) fn check_panels(&mut self) {
        if !self.slots.values().any(Slot::is_panel) || self.panels_checked > Instant::now() {
            return;
        }
        self.panels_checked = Instant::now() + PANELS_EVERY;
        let keys: Vec<OutputKey> = self
            .slots
            .values()
            .filter(|s| s.is_panel())
            .filter_map(|s| s.key().cloned())
            .collect();
        self.begin("looking whether the panels' windows are there");
        let vacated = match self.desktop.windows() {
            Some(w) => w.vacated(&keys),
            None => Vec::new(),
        };
        for key in vacated {
            let id = self
                .slots
                .iter()
                .find(|(_, s)| s.key() == Some(&key))
                .map(|(id, _)| *id);
            if let Some(id) = id {
                info!("the window on panel {id} is gone: closing the panel");
                self.remove(id, false);
                self.done(Done::PanelGone { id });
            }
        }
    }

    /// Where the mouse is: the output and the point across its image (0..=65535).
    fn find_mouse(&mut self) -> Result<JobOk> {
        let Some((x, y)) = self
            .desktop
            .pointer()
            .and_then(crate::platform::Pointer::position)
        else {
            bail!(UserError::NotOnDisplays);
        };
        let frac = |v: i32, start: i32, len: i32| {
            ((f64::from(v - start) / f64::from(len.max(1))).clamp(0.0, 1.0) * 65535.0) as u16
        };
        self.geometries()
            .into_iter()
            .find(|(_, g)| x >= g.x && x < g.x + g.w && y >= g.y && y < g.y + g.h)
            .map(|(key, g)| JobOk::Pointer {
                key,
                u: frac(x, g.x, g.w),
                v: frac(y, g.y, g.h),
            })
            .ok_or_else(|| anyhow::Error::new(UserError::NotOnDisplays))
    }

    /// Puts the mouse on screen `id` at `x`/`y` across its image (0..=65535).
    fn mouse_to(&mut self, id: ScreenId, x: u16, y: u16) {
        let key = self.slots.get(&id).and_then(Slot::key).cloned();
        let Some((_, g)) = self
            .geometries()
            .into_iter()
            .find(|(k, _)| Some(k) == key.as_ref())
        else {
            warn!("mouse: no {id} on the desktop");
            return;
        };
        let at =
            |start: i32, len: i32, f: u16| start + (f64::from(len) * f64::from(f) / 65535.0) as i32;
        let (cx, cy) = (at(g.x, g.w - 1, x), at(g.y, g.h - 1, y));
        if self.desktop.pointer().is_some_and(|p| p.move_to(cx, cy)) {
            info!("mouse moved to {id} at {cx},{cy}");
        } else {
            warn!("could not move the mouse to {cx},{cy}");
        }
    }

    /// Moves the PC's active window to screen `id` (a monitor, or a display of the PC
    /// that streams).
    fn window_here(&mut self, id: ScreenId) -> Result<()> {
        let Some(output) = self
            .slots
            .get(&id)
            .filter(|s| !s.is_panel())
            .and_then(Slot::key)
            .cloned()
        else {
            bail!(UserError::UnknownDisplay);
        };
        let Some(windows) = self.desktop.windows() else {
            bail!(UserError::NoWindow);
        };
        let Some(window) = windows.active() else {
            bail!(UserError::NoWindow);
        };
        windows
            .move_to(&window, &output, false)
            .context(UserError::Desktop)
    }

    /// Saves a picture of monitor `id` (all of them where only that is possible).
    fn screenshot(&mut self, id: ScreenId) -> Result<JobOk> {
        let Some(slot) = self.slots.get(&id).filter(|s| !s.is_physical()) else {
            bail!(UserError::UnknownDisplay);
        };
        let key = slot.key().cloned();
        let codec = slot.encoder.as_ref().map(|e| e.codec());
        let streaming = self.wanted.as_ref().is_some_and(|w| w.streaming);
        // Where the desktop's screenshot tool cannot take one output by name, the
        // monitor's own stream can. A fresh keyframe, decoded into a PNG; the whole
        // desktop if not.
        if !self.desktop.caps().screenshot_one_output
            && let Some(codec) = codec
            && streaming
        {
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            *self.shared.router.snapshot.lock().unwrap() = Some((id, tx));
            if self.shared.router.flowing(id) {
                if let Some(encoder) = self.slots.get(&id).and_then(|s| s.encoder.as_ref()) {
                    encoder.force_keyframe();
                }
            } else {
                // A still screen sends nothing until its keepalive (2 s on KDE, as long
                // as the wait below): its stream starts over, as for a headset waiting
                // for a fresh picture, and the source sends its picture at once.
                info!("{id} is still: its stream starts over for the picture");
                self.set_paused(id, false);
            }
            let frame = rx.recv_timeout(Duration::from_secs(2));
            self.shared.router.snapshot.lock().unwrap().take();
            match frame
                .map_err(anyhow::Error::from)
                .and_then(|data| crate::encoder::keyframe_to_png(&data, codec))
            {
                Ok(path) => return Ok(JobOk::Saved { path, whole: false }),
                Err(e) => warn!("screenshot from the stream: {e:#}; taking the whole desktop"),
            }
        }
        let caps = self.desktop.caps();
        let (name, whole) = (caps.name, !caps.screenshot_one_output || key.is_none());
        let Some(windows) = self.desktop.windows() else {
            return Err(anyhow!("the {name} desktop takes no screenshots"))
                .context(UserError::Capture);
        };
        let path = windows
            .screenshot(key.as_ref())
            .context(UserError::Capture)?;
        Ok(JobOk::Saved { path, whole })
    }

    /// Puts the PC's active window on a panel of its own, screen `id`, output
    /// `number`: an output its size, which the headset shows as a floating panel.
    pub(super) fn detach(&mut self, id: ScreenId, number: u32) -> Result<JobOk> {
        let Some(window) = self
            .desktop
            .windows()
            .and_then(crate::platform::Windows::active)
        else {
            bail!(UserError::NoWindow);
        };
        let Some(wanted) = self.wanted.clone() else {
            bail!(UserError::Desktop);
        };
        // Sized and planned with the scale its output gets, in whole percent.
        let scale = crate::platform::whole_percent(window.scale);
        let (width, height) = panel_size(&ActiveWindow {
            scale,
            ..window.clone()
        });
        let spec = MonitorSpec {
            width,
            height,
            scale,
        };
        let name = format!("Prostir-{number}");
        let mut output = self
            .make_monitor(id, number, &spec)
            .context(UserError::Desktop)?;
        let encoder = self
            .start_encoder(id, &name, false, output.as_mut(), &wanted.encoding)
            .context(UserError::Encoder)
            .inspect_err(|_| self.shared.router.remove_stream(id))?;
        let key = output.key().clone();
        let encoding = Encoding {
            encoder: encoder.kind().0.label(),
            codec: encoder.codec(),
            hardware: encoder.kind().0.hardware(),
            copies: encoder.copies(),
            note: encoder.note().map(str::to_owned),
        };
        self.shared.router.set_keyframer(id, encoder.keyframer());
        let mut slot = Slot::new(
            Made::Panel {
                origin: window.clone(),
                spec: spec.clone(),
                number,
            },
            name,
        );
        slot.output = Some(output);
        slot.encoder = Some(encoder);
        slot.started_for = Some((wanted.encoding.backend, wanted.encoding.codec));
        self.slots.insert(id, slot);
        self.layout_due = true;
        self.wallpapers_due = true;
        let moved = match self.desktop.windows() {
            Some(w) => w.move_to(&window, &key, true),
            None => Err(anyhow!("the desktop has no windows")),
        };
        if let Err(e) = moved {
            self.remove(id, true);
            return Err(e.context(UserError::Desktop));
        }
        if !wanted.streaming
            && let Some(e) = self.slots.get(&id).and_then(|s| s.encoder.as_ref())
        {
            e.set_paused(true);
        }
        self.panels_checked = Instant::now() + PANELS_EVERY;
        info!("window panel {id} on {key} ({width}x{height})");
        Ok(JobOk::Panel {
            id,
            spec,
            key,
            encoding,
        })
    }
}
