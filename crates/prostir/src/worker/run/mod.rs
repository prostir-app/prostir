//! The worker's thread: it owns the desktop, the outputs and their encoders, and
//! brings them to the wanted state one step at a time (see `mod.rs`).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use log::{error, info, warn};
use prostir_layout::desk;
use prostir_proto::state::{self, ErrorCode};
use prostir_proto::{Codec, HostMsg};

use super::inbox::Inbox;
use super::watch::Steps;
use super::{
    Connected, Done, Encoding, Gone, Job, JobId, JobOk, Primary, Start, Wanted, What, timed,
};
use crate::encoder::{
    Backend, Choice, Encoder, EncoderConfig, GpuDevices, Kind, Next, NotOpened, SourceFailed,
    Wanted as Asked,
};
use crate::errors::{UserError, code_of};
use crate::events::{Event, EventSink};
use crate::i18n::inline_t;
use crate::layout::panel_size;
use crate::platform::{
    ActiveWindow, DeskPlan, Desktop, Geometry, MonitorSpec, Output, OutputHealth, OutputKey,
    VirtualSpec, dialogs,
};
use crate::router::Router;
use crate::screen::{ScreenId, SizeAction, SizeGuard};

mod encoders;
mod jobs;
mod outputs;
mod place;

use encoders::{restart_due, stop_encoder, stop_encoders};
use outputs::show_sharing_tip;

/// How long an encoder may take to start, and to stop, before the worker goes on
/// without it.
const ENCODER_START: Duration = Duration::from_secs(10);
const ENCODER_STOP: Duration = Duration::from_secs(5);
/// A source that failed is tried once more after this long, as it is (see
/// `start_encoder`).
const SOURCE_AGAIN: Duration = Duration::from_millis(200);
/// A screen that went on with the encoder it had, the one wanted not starting, tries
/// the wanted one again after this.
const KEPT_FOR: Duration = Duration::from_secs(60);
/// An encoder (by kind) left behind this many times is not used any more.
const HANGS_OF_A_KIND: u32 = 3;
/// An encoder that broke this many times in a row on one screen, each soon after it
/// started ([`BROKE_SOON`]), is not used for it for a while: the next one is.
const BREAKS_OF_A_KIND: u32 = 2;
const BROKE_SOON: Duration = Duration::from_secs(60);
/// With this many encoders left behind and still running, no more are started.
const BEHIND_AT_MOST: u32 = 6;
/// How long a display of the PC whose stream stopped (or did not start) rests before
/// it is shown again.
const PHYSICAL_REST: Duration = Duration::from_secs(2);
/// How often the PC's own displays are looked at, while live.
const PHYSICAL_EVERY: Duration = Duration::from_secs(2);
/// How often the desktop's primary output is compared with ours.
const PRIMARY_EVERY: Duration = Duration::from_secs(2);
/// How often window panels are checked for a window that left.
const PANELS_EVERY: Duration = Duration::from_secs(1);
/// How often a layout waiting for the desktop to list new outputs looks again.
const LISTING_LOOK: Duration = Duration::from_millis(500);

/// What the control loop and the worker share.
pub struct Shared {
    pub inbox: Arc<Inbox<Wanted, Job>>,
    pub steps: Arc<Steps>,
    pub router: Arc<Router>,
    pub events: EventSink,
    /// Which worker this is (see [`super::Desk::generation`]).
    pub generation: u64,
}

/// Starts the worker's thread: once the worker `before` is gone, it connects and works
/// until asked to end; then `gone` is set.
pub fn spawn(shared: Shared, start: Start, before: Option<Gone>, gone: &Gone) {
    let mark_gone = {
        let gone = gone.clone();
        move || {
            let (lock, ring) = &*gone;
            *lock.lock().unwrap() = true;
            ring.notify_all();
        }
    };
    let spawned = std::thread::Builder::new()
        .name("desk".into())
        .spawn(move || {
            let (events, generation) = (shared.events.clone(), shared.generation);
            if let Some(before) = before {
                // A step like any other: a worker before that hangs shows here too.
                shared
                    .steps
                    .begin("waiting for the last desktop worker to finish");
                let (lock, ring) = &*before;
                let _done = ring.wait_while(lock.lock().unwrap(), |done| !*done);
                shared.steps.end();
            }
            if let Some(worker) = Worker::connect(shared, &start) {
                worker.run();
            }
            mark_gone();
            events.send(Event::Desk(generation, Done::Gone));
        });
    if let Err(e) = spawned {
        error!("no thread for the desktop: {e}");
        let (lock, ring) = &**gone;
        *lock.lock().unwrap() = true;
        ring.notify_all();
    }
}

/// What an output was made as.
#[derive(Debug, Clone)]
enum Made {
    Monitor {
        spec: MonitorSpec,
        number: u32,
    },
    Physical {
        key: OutputKey,
        number: u32,
    },
    Panel {
        origin: ActiveWindow,
        spec: MonitorSpec,
        number: u32,
    },
}

impl Made {
    /// Whether it is what `what` asks for.
    fn is(&self, what: &What) -> bool {
        match (self, what) {
            (Made::Monitor { spec, number }, What::Monitor { spec: s, number: n }) => {
                spec == s && number == n
            }
            (Made::Physical { key, number }, What::Physical { key: k, number: n }) => {
                key == k && number == n
            }
            (Made::Panel { number, .. }, What::Panel { number: n, .. }) => number == n,
            _ => false,
        }
    }

    fn spec(&self) -> Option<&MonitorSpec> {
        match self {
            Made::Monitor { spec, .. } | Made::Panel { spec, .. } => Some(spec),
            Made::Physical { .. } => None,
        }
    }
}

/// An encoder of the worker's, and the gate its frames and events go through: shut as
/// it is let go, so one that takes long to stop (or to start, and is left behind)
/// sends nothing more, and a headset never gets frames of two encoders for one stream.
struct Running {
    encoder: Encoder,
    retired: Arc<AtomicBool>,
    /// When it started.
    since: Instant,
}

impl std::ops::Deref for Running {
    type Target = Encoder;
    fn deref(&self) -> &Encoder {
        &self.encoder
    }
}

/// A screen's output and encoder.
struct Slot {
    made: Made,
    name: String,
    // Field order matters: the encoder stops before the output goes away.
    encoder: Option<Running>,
    output: Option<Box<dyn Output>>,
    /// The encoder and codec wanted when its encoder started.
    started_for: Option<(Backend, Codec)>,
    /// The attempt it failed at (tried again only at another).
    failed: Option<u32>,
    /// A display of the PC is shown again from then.
    rest_until: Option<Instant>,
    /// The encoder and codec wanted that did not start, so it went on with the one it
    /// had (`started_for`), and when to try the wanted one again.
    kept: Option<((Backend, Codec), Instant)>,
    size_guard: SizeGuard,
    /// Its encoder broke soon after it started this many times in a row, with this
    /// encoder and codec (see [`Worker::encoder_broke`]).
    breaks: Option<((Kind, Codec), u32)>,
    /// A scale the desktop was seen to have it at on the last tick, not the one it was
    /// made with; and whether it was seen at that one since it was made.
    scale_seen: Option<u16>,
    seen_own: bool,
}

impl Slot {
    fn new(made: Made, name: String) -> Self {
        Self {
            made,
            name,
            encoder: None,
            output: None,
            started_for: None,
            failed: None,
            rest_until: None,
            kept: None,
            size_guard: SizeGuard::default(),
            breaks: None,
            scale_seen: None,
            seen_own: false,
        }
    }

    fn key(&self) -> Option<&OutputKey> {
        self.output.as_ref().map(|o| o.key())
    }

    fn is_physical(&self) -> bool {
        matches!(self.made, Made::Physical { .. })
    }

    fn is_panel(&self) -> bool {
        matches!(self.made, Made::Panel { .. })
    }
}

/// One step towards the wanted state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// The screen is not wanted, or as something else: its output goes.
    Remove(ScreenId),
    /// It streams no more (too big for every headset): the encoder stops.
    StopStream(ScreenId),
    Make(ScreenId),
    Start(ScreenId),
    /// Another encoder or codec is wanted.
    Restart(ScreenId),
    Pause(ScreenId, bool),
    /// The active window onto a new panel, for job `JobId`.
    Detach(ScreenId, JobId),
}

/// An output on the desktop: a screen, or another real display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Out {
    Screen(ScreenId),
    Other(usize),
}

pub struct Worker {
    // Field order matters: the outputs go before the desktop connection.
    slots: BTreeMap<ScreenId, Slot>,
    desktop: Box<dyn Desktop>,
    /// One graphics device per card for every encoder (and capture) of the desktop.
    gpu: GpuDevices,
    shared: Shared,
    /// The wanted state worked on; none until the first comes.
    wanted: Option<Wanted>,
    /// The bitrate and the text preset the encoders have.
    tuning: Option<(u32, bool)>,
    /// The primary output as last made primary.
    primary_made: Option<OutputKey>,
    /// A monitor or panel was made: the others' wallpaper goes to it.
    wallpapers_due: bool,
    /// How many things handed over the worker took so far.
    taken: u64,
    /// Encoders left behind (a start, stop or pause that ran out of time), by kind.
    behind: Arc<timed::Behind<Kind>>,
    /// Panels it made (or tried to) for these jobs: each is made once.
    detached: std::collections::BTreeSet<JobId>,
    /// The desktop is to be laid out again.
    layout_due: bool,
    /// The rows and the layout the desktop took last.
    applied: (Vec<Vec<Out>>, Vec<desk::Spot<Out>>),
    /// Laid out without an output the desktop did not list yet: again once it does.
    layout_waits: bool,
    /// After the desktop refused a layout: when to try again, and how many times it did.
    layout_retry: (Instant, u32),
    primary_checked: Instant,
    /// Monitors whose scale given on the desktop was taken: old and new, in percent
    /// (see [`place::follow_rescaled`]).
    rescaled: BTreeMap<ScreenId, (u16, u16)>,
    /// The scales taken on a tick, kept for the next.
    scales_taken: Vec<(ScreenId, u16)>,
    panels_checked: Instant,
    physical_due: Instant,
}

impl Worker {
    /// Connects to the desktop and picks how to encode; says what came of it.
    fn connect(shared: Shared, start: &Start) -> Option<Self> {
        shared.steps.begin("connecting to the desktop");
        let connected = (|| -> Result<_> {
            let gpu = GpuDevices::default();
            let mut desktop = crate::platform::connect(&gpu)?;
            let (backend, codec) = crate::encoder::pick(start.codec, start.software)?;
            info!("encoding with {backend:?} {codec:?}");
            let caps = desktop.caps().clone();
            let scales = start
                .sizes
                .iter()
                .map(|&(w, h)| ((w, h), desktop.scales_for(w, h)))
                .collect();
            let windows = desktop.windows();
            let has_windows = windows.is_some();
            let window_list = windows.map(|w| Arc::downgrade(&w.list()));
            let connected = Connected {
                caps,
                scales,
                window_list,
                has_windows,
                backend,
                codec,
                choices: crate::encoder::available(),
                physical: desktop.physical(),
            };
            Ok((desktop, gpu, connected))
        })();
        shared.steps.end();
        let (desktop, gpu, connected) = match connected {
            Ok(c) => c,
            Err(e) => {
                shared
                    .events
                    .send(Event::Desk(shared.generation, Done::NoDesktop(e)));
                return None;
            }
        };
        shared.events.send(Event::Desk(
            shared.generation,
            Done::Connected(Box::new(connected)),
        ));
        Some(Self::with(shared, desktop, gpu))
    }

    /// A worker of `desktop`.
    fn with(shared: Shared, desktop: Box<dyn Desktop>, gpu: GpuDevices) -> Self {
        let now = Instant::now();
        Self {
            slots: BTreeMap::new(),
            desktop,
            gpu,
            shared,
            wanted: None,
            tuning: None,
            primary_made: None,
            wallpapers_due: false,
            taken: 0,
            detached: std::collections::BTreeSet::new(),
            behind: Arc::default(),
            layout_due: false,
            applied: (Vec::new(), Vec::new()),
            layout_waits: false,
            layout_retry: (now, 0),
            primary_checked: now,
            rescaled: BTreeMap::new(),
            scales_taken: Vec::new(),
            panels_checked: now,
            physical_due: now,
        }
    }

    /// Works until asked to end, then puts everything back.
    fn run(mut self) {
        loop {
            let mail = self.shared.inbox.take(self.own_time());
            if mail.quit {
                break;
            }
            self.taken = self.taken.max(mail.upto);
            if let Some(w) = mail.wanted {
                self.take(w);
            }
            for job in mail.jobs {
                self.job(job);
            }
            if !self.work() {
                break;
            }
            self.done(Done::Settled(self.taken));
        }
        self.close();
    }

    fn done(&self, done: Done) {
        self.shared
            .events
            .send(Event::Desk(self.shared.generation, done));
    }

    /// When the worker has something to do by itself: a layout the desktop refused
    /// or did not list yet. None: it sleeps until given something.
    fn own_time(&self) -> Option<Instant> {
        if !self.layout_due || self.wanted.is_none() {
            return None;
        }
        if self.layout_waits {
            return Some(self.layout_retry.0.max(Instant::now() + LISTING_LOOK));
        }
        Some(self.layout_retry.0)
    }

    /// Takes the newest wanted state.
    fn take(&mut self, mut wanted: Wanted) {
        place::follow_rescaled(&mut self.rescaled, &mut wanted);
        let was = self.wanted.as_ref();
        if was.is_none_or(|w| w.layout != wanted.layout) {
            self.layout_due = true;
        }
        let tuning = (wanted.encoding.bitrate_kbps, wanted.encoding.sharp);
        if self.tuning.is_some_and(|t| t != tuning) {
            for e in self.slots.values().filter_map(|s| s.encoder.as_ref()) {
                if tuning.0 != self.tuning.unwrap_or_default().0 {
                    e.set_bitrate(tuning.0);
                }
                if tuning.1 != self.tuning.unwrap_or_default().1 {
                    e.set_target_usage(wanted.encoding.target_usage());
                }
            }
        }
        self.tuning = Some(tuning);
        self.wanted = Some(wanted);
    }

    /// Steps towards the wanted state until there is none to take, taking newer
    /// states and jobs between steps; then the primary output, wallpapers and the
    /// layout. Whether to go on (not asked to end).
    fn work(&mut self) -> bool {
        loop {
            let (newer, jobs, quit, upto) = self.shared.inbox.peek_newer();
            if quit {
                return false;
            }
            self.taken = self.taken.max(upto);
            if let Some(w) = newer {
                self.take(w);
            }
            for job in jobs {
                self.job(job);
            }
            let Some(step) = self.next_step() else {
                break;
            };
            self.step(step);
        }
        self.keep_primary_ours();
        if std::mem::take(&mut self.wallpapers_due) {
            self.begin("giving new monitors the wallpaper");
            self.desktop.sync_wallpapers();
            self.end();
        }
        self.place();
        true
    }

    fn begin(&self, what: impl Into<String>) {
        self.shared.steps.begin(what);
    }

    fn end(&self) {
        self.shared.steps.end();
    }

    /// The first difference between what is there and what is wanted.
    fn next_step(&self) -> Option<Step> {
        let wanted = self.wanted.as_ref()?;
        let now = Instant::now();
        let find = |id: &ScreenId| wanted.screens.iter().find(|(i, _)| i == id);
        // Outputs not wanted, or wanted as something else, go first (to stay within
        // what the desktop and the encoders take).
        for (id, slot) in &self.slots {
            match find(id) {
                Some((_, w)) if slot.made.is(&w.what) => {}
                _ => return Some(Step::Remove(*id)),
            }
        }
        let cfg = (wanted.encoding.backend, wanted.encoding.codec);
        for (id, w) in &wanted.screens {
            let slot = self.slots.get(id);
            let resting = slot.is_some_and(|s| s.rest_until.is_some_and(|t| now < t));
            let failed = slot.is_some_and(|s| s.failed == Some(w.attempt));
            let has_output = slot.is_some_and(|s| s.output.is_some());
            let encoder = slot.and_then(|s| s.encoder.as_ref());
            match &w.what {
                What::Panel {
                    detach: Some(job), ..
                } if slot.is_none() => {
                    if !self.detached.contains(job) {
                        return Some(Step::Detach(*id, *job));
                    }
                    continue;
                }
                What::Panel { .. } if slot.is_none() => continue,
                What::Physical { .. } if !w.stream => continue,
                _ => {}
            }
            if !has_output {
                if !failed && !resting {
                    return Some(Step::Make(*id));
                }
                continue;
            }
            let Some(encoder) = encoder else {
                if w.stream && !failed && !resting {
                    return Some(Step::Start(*id));
                }
                continue;
            };
            if !w.stream {
                return Some(Step::StopStream(*id));
            }
            if slot.is_some_and(|s| restart_due(s.started_for, s.kept, cfg, now)) {
                return Some(Step::Restart(*id));
            }
            if encoder.is_paused() == wanted.streaming {
                return Some(Step::Pause(*id, !wanted.streaming));
            }
        }
        None
    }

    fn step(&mut self, step: Step) {
        let name = |id: &ScreenId| {
            self.wanted
                .as_ref()
                .and_then(|w| w.screens.iter().find(|(i, _)| i == id))
                .map_or_else(|| id.to_string(), |(_, w)| w.name.clone())
        };
        match step {
            Step::Remove(id) => {
                self.begin(format!("removing {id}"));
                // A window panel no longer wanted closes, its window back where it was.
                let panel = self.slots.get(&id).is_some_and(Slot::is_panel);
                self.remove(id, panel);
            }
            Step::Detach(id, job) => {
                self.begin(format!("putting the active window on panel {id}"));
                self.detached.insert(job);
                let number = self.wanted.as_ref().and_then(|w| {
                    w.screens.iter().find_map(|(i, s)| match s.what {
                        What::Panel { number, .. } if *i == id => Some(number),
                        _ => None,
                    })
                });
                let result = match number {
                    Some(number) => self.detach(id, number),
                    None => Err(anyhow!("no panel {id} wanted")),
                };
                self.done(Done::Job { job, result });
            }
            Step::StopStream(id) => {
                self.begin(format!("stopping the stream of {}", name(&id)));
                if let Some(slot) = self.slots.get_mut(&id) {
                    let encoder = slot.encoder.take();
                    slot.started_for = None;
                    stop_encoder(encoder, &slot.name, &self.shared.steps, &self.behind);
                }
                self.shared.router.remove_stream(id);
                self.layout_due = true;
                self.done(Done::Stopped { id });
            }
            Step::Make(id) => {
                self.begin(format!("making the output of {}", name(&id)));
                self.make(id);
            }
            Step::Start(id) => {
                self.begin(format!("starting the encoder of {}", name(&id)));
                self.start(id, None);
            }
            Step::Restart(id) => {
                self.begin(format!("starting the encoder of {} over", name(&id)));
                self.restart(id);
            }
            Step::Pause(id, paused) => {
                self.begin(format!(
                    "{} the encoder of {}",
                    if paused { "pausing" } else { "resuming" },
                    name(&id)
                ));
                self.set_paused(id, paused);
            }
        }
        self.end();
    }

    /// Housekeeping of a live PC.
    fn upkeep(&mut self) {
        self.begin("looking after the outputs");
        self.check_sizes();
        self.check_scales();
        self.check_closed();
        self.retry_unopened();
        self.check_primary();
        self.check_panels();
        self.end();
        self.look_at_physical();
    }

    /// Puts everything back: window panels first (each window back where it was), then
    /// the outputs and their encoders, then the desktop.
    fn close(mut self) {
        self.begin("putting the desktop back");
        // Every encoder stops at once (each driver takes its time to let go), then
        // the outputs go one after the other, each at once.
        let encoders: Vec<(Running, String)> = self
            .slots
            .values_mut()
            .filter_map(|s| {
                s.started_for = None;
                Some((s.encoder.take()?, s.name.clone()))
            })
            .collect();
        stop_encoders(encoders, &self.shared.steps, &self.behind);
        self.begin("removing the outputs");
        let panels: Vec<ScreenId> = self
            .slots
            .iter()
            .filter(|(_, s)| s.is_panel())
            .map(|(id, _)| *id)
            .collect();
        for id in panels {
            self.remove(id, true);
        }
        let ids: Vec<ScreenId> = self.slots.keys().copied().collect();
        for id in ids {
            self.remove(id, false);
        }
        let steps = self.shared.steps.clone();
        drop(self);
        crate::platform::give_memory_back();
        steps.end();
    }
}

#[cfg(test)]
mod tests;
