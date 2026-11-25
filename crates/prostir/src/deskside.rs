//! The control loop's side of the desktop's worker (`worker/`): what the worker says,
//! sessions that wait for it to connect, requests that wait for its answer, and the
//! watch on it. Nothing here waits for the worker.

use std::time::Instant;

use log::{error, info, warn};
use prostir_proto::HostMsg;
use prostir_proto::state::{ErrorCode, MAX_DETAIL, clip};

use crate::brain::Phase;
use crate::displays::{DeskNews, Displays, JobAnswer};
use crate::events::Accepted;
use crate::session::{Session, desktop_not_responding};
use crate::worker::Done;
use crate::worker::sound::SoundDone;
use crate::worker::watch::Change;
use crate::{Brain, keep_screens, requests, turn_away};

impl Brain {
    /// What the desktop's worker `generation` said.
    pub(crate) fn desk(&mut self, generation: u64, done: Done) {
        if matches!(done, Done::Gone) {
            // A worker let go is watched no more.
            self.retiring.retain(|r| r.generation != generation);
        }
        let Some(displays) = self.displays.as_mut() else {
            return;
        };
        // A worker let go says no more of the screens.
        if displays.desk().generation() != generation {
            return;
        }
        let store = &mut self.store;
        let news = displays.desk_done(done, &mut || store.new_id());
        // A display of the PC seen for the first time is kept.
        keep_screens(&mut self.store, displays, false);
        match news {
            DeskNews::None => {}
            DeskNews::Connected => {
                self.desk_unusable = None;
                // The sessions that waited for the desktop open now.
                for accepted in std::mem::take(&mut self.waiting_desk) {
                    self.open_session(accepted);
                }
            }
            DeskNews::NoDesktop(e) => {
                error!("{e:#}");
                // What the person can do about it shows in the settings window and
                // the tray until the desktop connects.
                self.desk_unusable = e
                    .downcast_ref::<crate::platform::Unusable>()
                    .map(|u| u.0.clone());
                // Tell the headsets why, instead of a snapshot.
                for accepted in std::mem::take(&mut self.waiting_desk) {
                    turn_away(accepted.conn, format!("{e:#}"));
                }
                if self.sessions.is_empty() {
                    self.let_go_displays();
                    self.phase = Phase::Idle;
                }
            }
            DeskNews::Rescaled => keep_screens(&mut self.store, displays, true),
            DeskNews::Job(job, result, answer) => {
                if self.shot_taken(generation, job, &result, &answer) {
                    return self.look_at_desk(Instant::now());
                }
                let saved = match &answer {
                    JobAnswer::Saved { path, .. } => Some(path.clone()),
                    _ => None,
                };
                let outcome = requests::outcome_of_job(result, answer);
                let taken = self
                    .sessions
                    .values_mut()
                    .any(|s| s.job_done(job, &outcome));
                // A picture that came after whoever asked gave up (and was told it
                // failed) is not kept.
                if !taken && let Some(path) = saved {
                    info!("a screenshot came too late: removing {path:?}");
                    if let Err(e) = std::fs::remove_file(&path) {
                        warn!("cannot remove {path:?}: {e}");
                    }
                }
            }
        }
        self.look_at_desk(Instant::now());
    }

    /// What the sound thread of the desktop worker `generation` made or let go of.
    pub(crate) fn sound_devices(&mut self, generation: u64, done: SoundDone) {
        let Some(displays) = self
            .displays
            .as_mut()
            .filter(|d| d.generation() == generation)
        else {
            return done.let_go();
        };
        displays.sound_done(done);
        // The microphone may be free to go to the main headset now.
        self.headsets_changed();
    }

    /// A connection waits for the desktop's worker to connect before its session
    /// opens (the worker is started for it if there is none); while the desktop does not
    /// respond, it is turned away at once.
    pub(crate) fn wait_for_desk(&mut self, accepted: Accepted) {
        if self.displays.as_ref().is_some_and(Displays::desk_stuck) {
            warn!("the desktop does not respond: turning the headset away");
            return turn_away(accepted.conn, desktop_not_responding());
        }
        self.waiting_desk.push(accepted);
    }

    /// Lets the screens go: the desktop's worker puts everything back, watched until
    /// it is gone.
    pub(crate) fn let_go_displays(&mut self) {
        self.shots_without_monitors();
        if let Some(retired) = self.displays.take().and_then(Displays::let_go) {
            self.retiring.push(retired);
        }
    }

    /// Looks at the desktop's worker at `now`: stuck for 15 s, the desktop does not
    /// respond (the tray and the local API show it, the headsets are told, requests
    /// waiting for it fail); back, the PC is fine again; stuck for 2 min, the program
    /// ends, so that whoever started it starts it again.
    pub(crate) fn look_at_desk(&mut self, now: Instant) {
        // Workers let go: one that hangs putting everything back still ends the program.
        for r in &mut self.retiring {
            let step = r.step();
            match r.dog.look(step, now) {
                Change::Stuck(what) => {
                    warn!("the desktop does not respond: {what}, after the monitors were let go");
                }
                Change::GiveUp(what) => {
                    error!(
                        "the desktop has not responded for too long ({what}): quitting, to be started again"
                    );
                    self.desk_gave_up = true;
                    self.quit = true;
                }
                Change::None | Change::Back => {}
            }
        }
        let Some(displays) = self.displays.as_mut() else {
            return;
        };
        let step = displays.desk().step();
        let (stuck_after, give_up_after) = displays.desk().dog.limits();
        match displays.desk_mut().dog.look(step, now) {
            Change::None => {}
            Change::Stuck(what) => {
                warn!(
                    "the desktop does not respond: {what} has taken more than {} s",
                    stuck_after.as_secs()
                );
                displays.router.send_all(&HostMsg::Problem {
                    error: ErrorCode::DESKTOP,
                    detail: clip(&desktop_not_responding(), MAX_DETAIL),
                    display: None,
                });
                for s in self.sessions.values_mut() {
                    for job in s.give_up_waiting(now, true) {
                        displays.desk().cancel(job);
                        displays.forget_job(job);
                    }
                }
                self.give_up_shots(now, true);
                for accepted in std::mem::take(&mut self.waiting_desk) {
                    turn_away(accepted.conn, desktop_not_responding());
                }
            }
            Change::Back => info!("the desktop responds again"),
            Change::GiveUp(what) => {
                error!(
                    "the desktop has not responded for {} s ({what}): quitting, to be started again",
                    give_up_after.as_secs()
                );
                self.desk_gave_up = true;
                self.quit = true;
            }
        }
    }

    /// When the control loop has to look at the desktop's worker or at requests
    /// waiting for it, if ever.
    pub(crate) fn desk_due(&self) -> Option<Instant> {
        let now = Instant::now();
        let dog = self
            .displays
            .as_ref()
            .and_then(|d| d.desk().dog.due(d.desk().step().map(|(t, _)| t), now));
        let answers = self
            .sessions
            .values()
            .filter_map(Session::waiting_until)
            .chain(self.shots.due())
            .min();
        let retiring = self
            .retiring
            .iter()
            .filter_map(|r| r.dog.due(r.step().map(|(t, _)| t), now))
            .min();
        // A monitor's stand-in place counts: the local API shows it.
        let placing = self.displays.as_ref().and_then(|d| d.placing_due(now));
        crate::brain::earliest([dog, answers, retiring, placing])
    }

    /// Requests that waited for the desktop long enough fail.
    pub(crate) fn desk_answers_due(&mut self, now: Instant) {
        self.give_up_shots(now, false);
        let Some(displays) = self.displays.as_mut() else {
            return;
        };
        for s in self.sessions.values_mut() {
            for job in s.give_up_waiting(now, false) {
                displays.desk().cancel(job);
                displays.forget_job(job);
            }
        }
    }
}
