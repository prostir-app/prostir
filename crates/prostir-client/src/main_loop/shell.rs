//! The model and the shell's handles its effects are done with: what the model asks
//! is done here, in the order it asked, and what the threads say goes to it.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use log::warn;
use openxr as xr;

use super::Selftest;
use super::pictures::custom_panorama_path;
use crate::app::{
    Effect, Event, MicCmd, Model, PlatformEvent, SoundCmd, SystemCmd, VideoCmd, XrCmd,
};
use crate::ids::{DisplayKey, PcId};
use crate::net::{Feedback, NetEvent};
use crate::persist::PersistEvent;
use crate::platform::android;
use crate::platform::android::system::{System, SystemEvent};
use crate::platform::meta;
use crate::platform::openxr::Gfx;
use crate::platform::openxr::input::Controls;
use crate::platform::room::Room;
use crate::platform::sound::audio::Gains;
use crate::platform::sound::mic::Mic;
use crate::platform::video::{self, SurfaceVideo};
use crate::video::VideoEvent;
use crate::{net, persist};

/// While the app is put away, how often the screen is read (see
/// `system::interactive`) until the headset is known to be asleep.
const SCREEN_EVERY: Duration = Duration::from_millis(250);

/// The model and what its effects are done with: the shell's handles (see
/// [`Shell::apply`]).
pub struct Shell<'s> {
    pub model: Model,
    pub session: &'s xr::Session<Gfx>,
    /// The streams shown, each with its surface and decoder.
    pub video: SurfaceVideo,
    /// What the video said in a pass, kept to be filled again.
    pub video_events: Vec<VideoEvent>,
    /// Messages to each PC in a session.
    pub outboxes: BTreeMap<PcId, Feedback>,
    /// The files' writer of this start: what is changed last is written as this goes
    /// (see `Drop`), however `run` ends.
    pub saver: persist::Saver,
    pub network: net::Net,
    pub system: System,
    pub mic: Arc<Mic>,
    /// Where the PC's sound comes from, for the audio thread.
    pub gains: Arc<Gains>,
    pub controls: Controls,
    /// The room behind the monitors, whichever way this headset shows it.
    pub room: Room,
    /// Where the person's own files are (none in a selftest).
    pub external_dir: Option<PathBuf>,
    /// The effects being done, kept to be filled again.
    pub fx: Vec<Effect>,
    /// What doing them came to, for the model, after them.
    pub later: Vec<Event>,
    /// The model asked the app to close.
    pub exit: bool,
}

impl Drop for Shell<'_> {
    /// What the last pass changed is written, and the files' writer ends, before the
    /// app does, also when `run` ends early with an error or a panic.
    fn drop(&mut self) {
        self.model.end_pass();
        self.apply(Instant::now());
        self.saver.stop(persist::Saver::STOPS_WITHIN);
    }
}

impl Shell<'_> {
    /// `event` to the model, then what it asks done.
    pub fn handle(&mut self, now: Instant, event: Event) {
        self.model.handle(now, event);
        self.apply(now);
    }

    /// Does what the model asked, in the order it asked, until it asks nothing more
    /// (what doing it came to goes back to it as events).
    pub fn apply(&mut self, now: Instant) {
        loop {
            self.model.take_effects(&mut self.fx);
            if self.fx.is_empty() {
                return;
            }
            let mut fx = std::mem::take(&mut self.fx);
            for effect in fx.drain(..) {
                self.effect(effect);
            }
            self.fx = fx;
            let mut later = std::mem::take(&mut self.later);
            for event in later.drain(..) {
                self.model.handle(now, event);
            }
            self.later = later;
        }
    }

    fn effect(&mut self, effect: Effect) {
        match effect {
            Effect::Send(pc, msg) => match self.outboxes.get(&pc).map(|f| f.send(msg)) {
                Some(Ok(())) => {}
                Some(Err(e)) => self.later.push(Event::SendFailed {
                    pc,
                    why: Some(e.to_string()),
                }),
                None => self.later.push(Event::SendFailed { pc, why: None }),
            },
            Effect::Tell(pc, msg) => {
                if let Some(f) = self.outboxes.get(&pc) {
                    let _ = f.send(msg);
                }
            }
            Effect::Net(cmd) => self.network.send(cmd),
            Effect::Save(file) => {
                let model = &self.model;
                self.saver.save(file, |out| model.write_file(file, out));
            }
            Effect::Haptic(hand) => self.controls.pulse(self.session, hand),
            Effect::Xr(XrCmd::RefreshRate(hz)) => meta::set_display_rate(self.session, hz),
            Effect::Xr(XrCmd::Room { on, opacity }) => {
                if let Err(e) = self.room.set(on, opacity) {
                    self.later
                        .push(Event::Platform(PlatformEvent::CamerasFailed(format!(
                            "{e:#}"
                        ))));
                }
            }
            Effect::Sound(SoundCmd::Place(angle)) => self.gains.place(angle),
            Effect::Sound(SoundCmd::SetVolume { level, muted, n }) => {
                self.system.set_volume(level, muted, n);
            }
            Effect::System(SystemCmd::WatchVolume(steps)) => self.system.watch_volume(steps),
            Effect::Mic(MicCmd::Allowed(on)) => self.mic.set_allowed(on),
            Effect::Mic(MicCmd::AskAgain) => self.mic.ask_again(),
            Effect::Mic(MicCmd::RefreshPermission) => self.mic.refresh_permission(),
            Effect::Video(VideoCmd::Drop(id)) => self.video.end(id),
            Effect::System(SystemCmd::LoadLanguage(lang)) => self.system.load_language(lang),
            Effect::System(SystemCmd::PalmMenu(shown)) => self.system.palm_menu(shown),
            Effect::System(SystemCmd::LookForFiles) => {
                // The person may have copied a picture onto the headset meanwhile, or
                // a language file (listed again on the system thread).
                let custom = custom_panorama_path(self.external_dir.as_deref()).is_some();
                self.later
                    .push(Event::Platform(PlatformEvent::CustomPanorama(custom)));
                self.system.list_languages();
            }
            Effect::Exit => self.exit = true,
        }
    }

    /// Hands over what changed and waits until it is written (for
    /// [`persist::Saver::STOPS_WITHIN`] at most): the app is `why` (paused, or the
    /// headset taken off), and may not come back.
    pub fn flush(&mut self, now: Instant, why: &str) {
        self.model.end_pass();
        self.apply(now);
        if !self.saver.flush(persist::Saver::STOPS_WITHIN) {
            warn!(
                "{why}: the files were not all written within {} ms",
                persist::Saver::STOPS_WITHIN.as_millis()
            );
        }
    }

    /// What the threads said since the last pass, all of it: the network, the
    /// streams' video, the system thread and the files' writer.
    pub fn news(
        &mut self,
        now: Instant,
        net_rx: &mpsc::Receiver<NetEvent>,
        vm: &jni::JavaVM,
        mut test: Option<&mut Selftest>,
    ) {
        while let Ok(event) = net_rx.try_recv() {
            if let Some(t) = test.as_deref_mut() {
                t.net_event(&event);
            }
            self.net_event(now, event, vm);
        }
        self.video_now(now, vm);
        // Only the newest battery reading matters (several wait after a pause).
        let mut newest = None;
        while let Some(event) = self.system.try_recv() {
            match event {
                SystemEvent::Battery(reading) => newest = Some(reading),
                SystemEvent::Texts(texts) => {
                    self.handle(now, Event::Platform(PlatformEvent::Texts(texts)));
                }
                SystemEvent::Languages(known) => {
                    self.handle(now, Event::Platform(PlatformEvent::Languages(known)));
                }
                SystemEvent::Volume { level, muted, done } => {
                    let read = PlatformEvent::Volume { level, muted, done };
                    self.handle(now, Event::Platform(read));
                }
            }
        }
        if let Some(reading) = newest {
            self.handle(now, Event::Platform(PlatformEvent::Battery(reading)));
        }
        while let Some(event) = self.saver.try_recv() {
            match event {
                PersistEvent::Failed { .. } => self.handle(now, Event::Persist(event)),
            }
        }
    }

    /// What the network thread said: the handles it brings (a session's outbox, a
    /// stream's frames) stay here, the facts go to the model.
    fn net_event(&mut self, now: Instant, event: NetEvent, vm: &jni::JavaVM) {
        match event {
            NetEvent::Connected {
                pc,
                addr,
                feedback,
                language,
                opened_with,
            } => {
                self.outboxes.insert(pc, feedback);
                self.handle(
                    now,
                    Event::Connected {
                        pc,
                        addr,
                        language,
                        opened_with,
                    },
                );
            }
            NetEvent::StreamStart {
                pc,
                id,
                generation,
                width,
                height,
                codec,
                frames,
                feedback,
            } => {
                let id = DisplayKey::new(pc, id);
                let said = self
                    .model
                    .headset()
                    .caps
                    .decoder(codec)
                    .map(|d| (d.max_w, d.max_h));
                let spec = video::Spec {
                    id,
                    generation,
                    codec,
                    width,
                    height,
                    said,
                };
                let mut events = std::mem::take(&mut self.video_events);
                self.video
                    .start(self.session, vm, spec, frames, feedback, &mut events);
                let size = (width, height);
                self.handle(
                    now,
                    Event::StreamStarted {
                        id,
                        generation,
                        codec,
                        size,
                    },
                );
                for e in events.drain(..) {
                    self.handle(now, Event::Video(e));
                }
                self.video_events = events;
            }
            event => {
                // A session ended: its outbox goes with it.
                if let NetEvent::Ended { pc: Some(pc), .. } = &event {
                    self.outboxes.remove(pc);
                }
                self.handle(now, Event::Net(event));
            }
        }
    }

    /// What the streams' video came to since the last pass (see
    /// `SurfaceVideo::poll`): a look at each stream, nothing made unless a decoder
    /// thread ended.
    fn video_now(&mut self, now: Instant, vm: &jni::JavaVM) {
        let mut events = std::mem::take(&mut self.video_events);
        self.video.poll(now, self.session, vm, &mut events);
        for e in events.drain(..) {
            self.handle(now, Event::Video(e));
        }
        self.video_events = events;
    }

    /// Reads the screen where presence wants it (only while the app is put away, at
    /// once and then at most every `SCREEN_EVERY`; see `Presence::wants_screen`), then
    /// the PC is told what changed.
    pub fn presence_now(&mut self, now: Instant, screen_read: &mut Option<Instant>) {
        if !self.model.wants_screen() {
            *screen_read = None;
        } else if screen_read.is_none_or(|at| now.saturating_duration_since(at) >= SCREEN_EVERY) {
            *screen_read = Some(now);
            if let Some(on) = android::interactive() {
                self.handle(now, Event::Platform(PlatformEvent::Interactive(on)));
                return;
            }
        }
        self.model.presence_due(now);
        self.apply(now);
    }
}
