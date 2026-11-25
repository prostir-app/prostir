//! One volume with the PC (the step 7 note, 8.2): the headset's system volume is the
//! level, and the PC's slider follows it. A request of the PC goes to the headset's
//! next step the way the slider moved (see `prostir_proto::volume::Steps`), is set
//! at once and answered at once, without waiting for the system; what the system then
//! says it is, if it is something else (its own steps, a limit of its own, its menu,
//! the buttons), is a change of the headset's own, as for the buttons.

use std::time::Duration;

use log::info;
use prostir_proto::ClientMsg;
use prostir_proto::state::{Volume, VolumeAsk};
use prostir_proto::volume::{HeadsetVolume, Steps};

use super::{Effect, Model, SoundCmd, SystemCmd};
use crate::ids::PcId;
use crate::presence;

/// How often the system's volume is read in a session (its buttons and menu change it
/// with no word to the app), and without one (so the next `Open` says it as it is).
/// Not while the headset is not worn: nobody changes it then, and it sleeps.
pub const READ_IN_SESSION: Duration = Duration::from_millis(250);
pub const READ_WITHOUT_SESSION: Duration = Duration::from_secs(1);

/// The headset's volume, shared with the PC.
#[derive(Debug)]
pub(super) struct SharedVolume {
    level: HeadsetVolume,
    steps: Steps,
    /// The sets asked of the system so far: a reading from before the newest is done
    /// is old (its own reading follows).
    sets: u32,
    /// How often the system's volume is read, as the shell was last told (`None`:
    /// not told yet).
    watched: Option<Option<Duration>>,
    /// Changed since the next `Open` was last told.
    changed: bool,
}

impl SharedVolume {
    /// At `level` (muted or not) now, on a volume of `steps` above silence.
    pub(super) fn new(steps: u8, (level, muted): (u8, bool)) -> Self {
        Self {
            level: HeadsetVolume::new(level, muted),
            steps: Steps::new(steps),
            sets: 0,
            watched: None,
            changed: false,
        }
    }

    /// The level now, as `Open` carries it.
    pub(super) fn now(&self) -> Volume {
        self.level.now()
    }

    /// How often the system's volume is read now (`None`: not now).
    #[cfg(test)]
    pub(super) fn watched(&self) -> Option<Duration> {
        self.watched.flatten()
    }
}

impl Model {
    /// PC `pc` asks the headset to take its slider's level: taken (unless a change of
    /// the headset's own crossed it) at the next step the way it moved, the system
    /// asked to set it, and answered at once with the level that stands.
    pub(super) fn volume_asked(&mut self, pc: PcId, asked: &VolumeAsk) {
        let Some(v) = &mut self.volume else {
            return;
        };
        let now = self.now;
        let before = v.level.now();
        let SharedVolume {
            level,
            steps,
            sets,
            changed,
            ..
        } = v;
        let effects = &mut self.effects;
        let answer = level.asked(asked, |to, muted| {
            let to = steps.take(before.level, to, now);
            if (to, muted) != (before.level, before.muted) {
                *sets += 1;
                effects.push(Effect::Sound(SoundCmd::SetVolume {
                    level: to,
                    muted,
                    n: *sets,
                }));
                *changed = true;
            }
            (to, muted)
        });
        self.effects
            .push(Effect::Tell(pc, ClientMsg::Volume { part: answer }));
    }

    /// The system's volume is `level` (muted or not), read after its `done`th set: if
    /// that is not the level the PC was told, it changed on the headset (the buttons,
    /// the system's menu, or the system took a set otherwise), and the PC hears.
    pub(super) fn volume_read(&mut self, level: u8, muted: bool, done: u32) {
        let Some(v) = &mut self.volume else {
            return;
        };
        if done < v.sets {
            return;
        }
        if let Some(part) = v.level.changed_here(level, muted) {
            info!(
                "volume: {level}{} on the headset",
                if muted { ", muted" } else { "" }
            );
            v.changed = true;
            self.tell_all(&ClientMsg::Volume { part });
        }
    }

    /// The system's volume is read every [`READ_IN_SESSION`] while a session is open,
    /// every [`READ_WITHOUT_SESSION`] without one, and not while the headset is not worn
    /// (it is read at once when it is again). Once a pass; the shell hears of a change.
    pub(super) fn watch_volume(&mut self) {
        let every = match self.presence.state() {
            presence::State::Worn if !self.sessions.is_empty() => Some(READ_IN_SESSION),
            presence::State::Worn => Some(READ_WITHOUT_SESSION),
            presence::State::Off | presence::State::Asleep => None,
        };
        let Some(v) = &mut self.volume else {
            return;
        };
        if v.watched != Some(every) {
            v.watched = Some(every);
            let steps = v.steps.steps();
            self.effects
                .push(Effect::System(SystemCmd::WatchVolume { steps, every }));
        }
    }

    /// The level changed since the next `Open` was told: whether to tell it again
    /// (once, at the end of a pass).
    pub(super) fn volume_changed(&mut self) -> bool {
        self.volume
            .as_mut()
            .is_some_and(|v| std::mem::take(&mut v.changed))
    }
}
