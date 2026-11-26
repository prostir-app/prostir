//! "Hands (trial)" of "For development" (the preview build only, see `crate::PREVIEW`):
//! what step 8 lets the owner try (`interaction::trial`), the input recorded for 30
//! seconds (`interaction::record`), and `feel.txt` read over the numbers of the input.
//! In memory only, a test of the moment.

use std::time::Instant;

use log::{info, warn};

use super::{Effect, Model, SystemCmd};
use crate::interaction::record::{self, RECORD_FOR, Recorder};
use crate::interaction::trial::Trial;
use crate::interaction::{Feel, FrameInput};
use crate::lang::Moment;
use crate::ui::Action;

/// What "Hands (trial)" keeps.
#[derive(Debug, Default)]
pub struct DevInput {
    pub trial: Trial,
    /// The input being recorded, and the file it goes to.
    recorder: Option<(Recorder, String)>,
    /// The whole seconds of it left, as the menu shows them.
    left: Option<u8>,
    /// The wall clock of the last frame (to name a recording by).
    wall: Option<Moment>,
    /// What reading `feel.txt` came to: found, the numbers changed, the lines not read.
    pub feel_read: Option<(bool, u8, u8)>,
    /// The page of it shown.
    page: u8,
}

impl DevInput {
    /// The seconds of the recording left (`None`: not recording).
    pub fn recording(&self) -> Option<u8> {
        self.left
    }
}

impl Model {
    /// What "Hands (trial)" shows.
    pub fn dev_hands(&self) -> crate::ui::DevHands {
        crate::ui::DevHands {
            trial: self.dev_input.trial,
            recording: self.dev_input.recording(),
            feel: self.dev_input.feel_read,
            page: self.dev_input.page,
        }
    }

    pub(super) fn act_dev_input(&mut self, action: Action) {
        let t = &mut self.dev_input.trial;
        match action {
            Action::DevMonitors(on) => t.monitors = on,
            Action::DevTake(take) => t.take = take,
            Action::DevMovement(movement) => t.movement = movement,
            Action::DevHandLook(look) => t.look = look,
            Action::DevFx(fx) => t.fx = fx,
            Action::DevOwnModels(own) => t.own_models = own,
            Action::DevRecord => self.start_recording(),
            Action::DevReadFeel => self.effects.push(Effect::System(SystemCmd::ReadFeel)),
            Action::DevHandsPage(page) => self.dev_input.page = page,
            // The others are not this part's (see `Model::act`).
            _ => return,
        }
        self.rev += 1;
    }

    fn start_recording(&mut self) {
        if self.dev_input.recorder.is_some() {
            return;
        }
        let name = match self.dev_input.wall {
            Some(w) => format!(
                "input-{}-{:02}-{:02}-{:02}{:02}.rec",
                w.year,
                w.month + 1,
                w.day,
                w.hour,
                w.minute
            ),
            None => "input.rec".to_owned(),
        };
        let head = record::head(env!("CARGO_PKG_VERSION"), &self.feel, &self.input_stage());
        info!("recording the input into {name}");
        self.dev_input.recorder = Some((Recorder::start(self.now, &head), name));
        self.dev_input.left = Some(RECORD_FOR.as_secs() as u8);
    }

    /// This frame's input into the recording, if one is made; at its end, it goes to
    /// the shell to be written. The menu is told once a second.
    pub(super) fn record_frame(&mut self, now: Instant, input: &FrameInput) {
        if input.wall.is_some() {
            self.dev_input.wall = input.wall;
        }
        let Some((rec, _)) = self.dev_input.recorder.as_mut() else {
            return;
        };
        if input.head.is_some() {
            rec.push(now, input);
        }
        if rec.over(now) {
            if let Some((rec, name)) = self.dev_input.recorder.take() {
                info!("recorded {} frames of the input", rec.frames());
                self.effects.push(Effect::Recording {
                    name,
                    bytes: rec.into_bytes(),
                });
            }
            self.dev_input.left = None;
            self.rev += 1;
            return;
        }
        let gone = now.saturating_duration_since(rec.started());
        let left = RECORD_FOR.saturating_sub(gone).as_secs() as u8 + 1;
        if self.dev_input.left != Some(left) {
            self.dev_input.left = Some(left);
            self.rev += 1;
        }
    }

    /// `feel.txt` was read (`None`: there is none): its numbers over the defaults.
    pub(super) fn feel_text(&mut self, text: Option<&str>) {
        let (feel, problems) = text.map_or((Feel::DEFAULT, Vec::new()), Feel::parse);
        for p in &problems {
            warn!("feel.txt: {p}");
        }
        let changed = Feel::FIELDS
            .iter()
            .filter(|(key, _, _)| feel.get(key) != Feel::DEFAULT.get(key))
            .count();
        if text.is_some() {
            info!("feel.txt: {changed} numbers changed");
        }
        self.world.set_bounds(crate::layout::Bounds::of(&feel));
        self.feel = feel;
        let small = |n: usize| u8::try_from(n).unwrap_or(u8::MAX);
        self.dev_input.feel_read = Some((text.is_some(), small(changed), small(problems.len())));
        self.rev += 1;
    }
}
