//! Window panels and the jobs that answer later: a window detached into the headset,
//! brought back, a screenshot, the mouse.

use super::*;

impl Displays {
    /// The desktop's list of windows, for a thread of its own.
    pub fn window_list(&self) -> Option<Arc<dyn WindowList>> {
        self.facts.as_ref()?.window_list.as_ref()?.upgrade()
    }

    /// Asks where the mouse is (the answer comes as the job's).
    pub fn find_mouse(&mut self) -> JobId {
        let job = self.desk.job_id();
        let until = Instant::now() + worker::answer_within();
        self.desk.ask(Job::FindMouse { job, until });
        job
    }

    /// The point `u`, `v` across the image of screen `id` (0..=65535 from the top
    /// left) on the desktop, while it is there: the middle of its first pixel to the
    /// middle of its last, as `CursorAt` says it back (see `cursor.rs`).
    pub fn spot(&self, id: ScreenId, u: u16, v: u16) -> Option<Spot> {
        // Only a display the headsets are shown.
        if !matches!(self.screens.get(&id)?.state, ScreenState::Running) {
            return None;
        }
        let rect = self.desk_rect(id)?;
        let output = OutputKey::new(self.output_of(id)?);
        let at = crate::cursor::point_of;
        let desk = self.on_desk.values().fold(rect, |all, r| {
            let (x, y) = (all.x.min(r.x), all.y.min(r.y));
            let right = (all.x + all.w as i32).max(r.x + r.w as i32);
            let bottom = (all.y + all.h as i32).max(r.y + r.h as i32);
            state::DeskRect {
                x,
                y,
                w: (right - x) as u32,
                h: (bottom - y) as u32,
            }
        });
        Some(Spot {
            x: at(rect.x, rect.w, u),
            y: at(rect.y, rect.h, v),
            output,
            rect,
            desk,
            open: Sides::open(rect, self.on_desk.values().copied()),
        })
    }

    /// Hands `ev` to the input thread, never waiting (see `crate::input`).
    pub fn input(&self, ev: crate::input::Ev) {
        self.input.send(ev);
    }

    /// Puts the PC's active window on a panel of its own, numbered `id` (a number never
    /// given before): an output its size, which the headset shows as a floating panel.
    /// The answer comes as the job's.
    pub fn detach_window(&mut self, id: ScreenId) -> Result<JobId> {
        if self.window_count() >= WINDOWS_MAX {
            bail!(UserError::Limit {
                max: WINDOWS_MAX,
                of: Counted::Windows
            });
        }
        if !self.facts.as_ref().is_some_and(|f| f.has_windows) {
            bail!(UserError::NoWindow);
        }
        // Made by the worker as part of the wanted state, after any output it removes.
        let number = self.free_number();
        let job = self.desk.job_id();
        self.detaching.insert(id, (number, job));
        Ok(job)
    }

    /// The worker made window panel `id`.
    pub(super) fn panel_made(
        &mut self,
        id: ScreenId,
        spec: MonitorSpec,
        key: OutputKey,
        encoding: worker::Encoding,
    ) {
        let Some((number, _)) = self.detaching.remove(&id) else {
            return;
        };
        let mut screen = Screen::starting(id, number, ScreenKind::Window { spec });
        screen.output = Some(key);
        screen.encoding = Some(encoding);
        screen.state = ScreenState::Running;
        self.screens.insert(id, screen);
    }

    /// Removes a window panel; its window goes back where it was detached from (or,
    /// when that monitor is gone, to the others), as the worker closes it.
    pub fn close_window_panel(&mut self, id: ScreenId) {
        if self.screens.get(&id).is_some_and(Screen::is_window) {
            self.screens.remove(&id);
        }
    }

    /// Moves the PC's active window to monitor `id` (the one the user looks at); the
    /// answer comes as the job's.
    pub fn window_here(&mut self, id: ScreenId) -> Result<JobId> {
        if !self
            .screens
            .get(&id)
            .is_some_and(|s| s.is_virtual() || (s.is_physical() && s.streaming))
        {
            bail!(UserError::UnknownDisplay);
        }
        if !self.facts.as_ref().is_some_and(|f| f.has_windows) {
            bail!(UserError::NoWindow);
        }
        let job = self.desk.job_id();
        let until = Instant::now() + worker::answer_within();
        self.desk.ask(Job::WindowHere { job, id, until });
        Ok(job)
    }

    /// Saves a picture of monitor `id` (all of them where only that is possible); the
    /// answer comes as the job's.
    pub fn screenshot(&mut self, id: ScreenId) -> Result<JobId> {
        if !self
            .screens
            .get(&id)
            .is_some_and(|s| s.is_virtual() || s.is_window())
        {
            bail!(UserError::UnknownDisplay);
        }
        let job = self.desk.job_id();
        let until = Instant::now() + worker::answer_within();
        self.desk.ask(Job::Screenshot { job, id, until });
        Ok(job)
    }
}
