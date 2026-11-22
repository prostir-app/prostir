//! The desktop's worker (`desk`): the one thread that calls the desktop and starts and
//! stops encoders, so the control loop never waits for either.
//!
//! The control loop owns the model of the screens (`displays/`): which there are,
//! their numbers, rows and states. It hands the worker the whole state it wants
//! ([`Wanted`]), not commands; the worker compares it with what is there and does the
//! difference one step at a time (make an output, start its encoder, place the
//! desktop, remove one), saying after each what came of it ([`Done`], as an event).
//! A newer wanted state replaces one not started on, and is taken between two steps:
//! a profile of four monitors asked for while the desktop thinks is laid out once,
//! at the end. One-off things go as [`Job`]s, in order; those whose answer a headset
//! waits for carry a [`JobId`], and their answer comes as [`Done::Job`].
//!
//! Every call that may hang has a limit: other programs are run with one (see
//! `platform::process::run`), encoders start and stop on a thread of their own and
//! are left behind after theirs (`timed.rs`). What has none (a call into the desktop
//! that never returns) shows in the step under way, which the control loop watches
//! (`watch.rs`): stuck for 15 s, the PC says the desktop does not respond; for 2 min,
//! the program ends, to be started again.
//!
//! The worker sleeps until it is given something (or a layout the desktop refused is
//! due again), so an idle PC is not woken by it.

mod inbox;
mod run;
pub mod sound;
mod timed;
pub mod watch;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

use prostir_layout::desk;
use prostir_proto::Codec;
use prostir_proto::state::{DeskRect, ErrorCode};

use crate::encoder::{Backend, EncoderConfig};
use crate::events::EventSink;
use crate::platform::{DesktopCaps, MonitorSpec, OutputKey, PhysicalDisplay, Scales, WindowList};
use crate::router::Router;
use crate::screen::ScreenId;

use inbox::{Inbox, Mergeable};
use watch::{Steps, Watchdog};

/// What a screen is to be on the desktop.
#[derive(Debug, Clone, PartialEq)]
pub enum What {
    /// A virtual monitor, as output `number` (`Prostir-N`).
    Monitor { spec: MonitorSpec, number: u32 },
    /// A display of the PC, streamed as it is, numbered `number` while it streams.
    Physical { key: OutputKey, number: u32 },
    /// A window panel, as output `number`. With `detach`, it is still to be made: the
    /// active window goes onto it, and what came of that is the answer to job
    /// `detach` (made once, however long the state asks for it).
    Panel { number: u32, detach: Option<JobId> },
}

/// One screen of the wanted state.
#[derive(Debug, Clone, PartialEq)]
pub struct WantedScreen {
    pub what: What,
    /// Its picture goes to the headsets (not while it is too big for all of them).
    pub stream: bool,
    /// Raised to try again a screen that failed (a failure is not tried again on its
    /// own before).
    pub attempt: u32,
    /// For the log and the router.
    pub name: String,
}

/// The primary output: a screen's, or a display of the PC by its key (shown or not).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Primary {
    Screen(ScreenId),
    Display(OutputKey),
}

/// How the desktop is to be laid out (see `prostir_layout::desk`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Layout {
    /// The headset's rows, every monitor once, displays of the PC where it put them.
    pub rows: Vec<Vec<desk::Entry<ScreenId>>>,
    /// The monitor the others are laid out around.
    pub main: Option<ScreenId>,
    /// The displays of the PC that stream, the laptop's panel first.
    pub physical: Vec<ScreenId>,
    /// The window panels, right of everything.
    pub column: Vec<ScreenId>,
    /// The primary output, where the desktop has one and it is ours to pick.
    pub primary: Option<Primary>,
}

/// The whole state the control loop wants of the desktop.
#[derive(Debug, Clone, PartialEq)]
pub struct Wanted {
    /// Every screen to be there, in the order of the rows, then the rest.
    pub screens: Vec<(ScreenId, WantedScreen)>,
    pub encoding: EncoderConfig,
    /// A headset with a session is worn: the encoders run (else they pause).
    pub streaming: bool,
    pub layout: Layout,
    /// A display of the PC is to be shown: switch one on where none is.
    pub wake_physical: bool,
}

/// The number of a job whose answer someone waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct JobId(pub u64);

/// Something to do once.
#[derive(Debug)]
pub enum Job {
    /// The housekeeping of a live PC (about twice a second): outputs another program
    /// resized or the desktop closed, encoders that did not open, the primary output,
    /// window panels whose window left, the PC's own displays.
    Tick,
    /// Which output the user works on ([`Done::Focus`]).
    Focus,
    /// The PC's own displays now ([`Done::Physical`]).
    LookPhysical,
    /// These streams start over, for a fresh picture.
    Fresh(BTreeSet<ScreenId>),
    /// The encoders that failed on these screens are tried for them again (a new
    /// session, or a retry asked for).
    Forgive(BTreeSet<ScreenId>),
    /// A job with an answer someone waits for until `until`: past it, nobody does,
    /// and it is dropped instead of done late.
    Screenshot {
        job: JobId,
        id: ScreenId,
        until: Instant,
    },
    FindMouse {
        job: JobId,
        until: Instant,
    },
    MouseTo {
        id: ScreenId,
        x: u16,
        y: u16,
    },
    WindowHere {
        job: JobId,
        id: ScreenId,
        until: Instant,
    },
}

impl Job {
    /// The number of a job someone waits for.
    fn id(&self) -> Option<JobId> {
        match self {
            Job::Screenshot { job, .. }
            | Job::FindMouse { job, .. }
            | Job::WindowHere { job, .. } => Some(*job),
            _ => None,
        }
    }
}

impl Mergeable for Job {
    fn merge(&mut self, other: &Self) -> bool {
        match (self, other) {
            (Job::Tick, Job::Tick)
            | (Job::Focus, Job::Focus)
            | (Job::LookPhysical, Job::LookPhysical) => true,
            (Job::Fresh(ids), Job::Fresh(more)) | (Job::Forgive(ids), Job::Forgive(more)) => {
                ids.extend(more);
                true
            }
            _ => false,
        }
    }
}

/// How a screen encodes, as its encoder started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encoding {
    /// For people: `VA-API`, `x264`.
    pub encoder: &'static str,
    pub codec: Codec,
    pub hardware: bool,
    pub copies: bool,
    /// Why it is not the best this PC has for the output, when it is not.
    pub note: Option<String>,
}

/// What the worker found when it connected to the desktop.
pub struct Connected {
    pub caps: DesktopCaps,
    /// The scales it gives each size offered to headsets.
    pub scales: Vec<((u32, u32), Scales)>,
    /// Every window, for the thread that looks where sound comes from; gone with the
    /// desktop.
    pub window_list: Option<Weak<dyn WindowList>>,
    /// It tells which output the user works on.
    pub has_windows: bool,
    /// The encoder and codec the PC encodes with, and what it can encode with.
    pub backend: Backend,
    pub codec: Codec,
    pub choices: Vec<(Codec, bool)>,
    /// The PC's own displays, connected and on now.
    pub physical: Vec<PhysicalDisplay>,
}

/// What came of a job.
#[derive(Debug)]
pub enum JobOk {
    Done,
    /// A picture saved at `path`; `whole`: of the whole desktop, which was all the
    /// desktop could take.
    Saved {
        path: PathBuf,
        whole: bool,
    },
    /// The mouse is on this output, at this point across it (0..=65535).
    Pointer {
        key: OutputKey,
        u: u16,
        v: u16,
    },
    /// A window panel, made and streaming: screen `id`.
    Panel {
        id: ScreenId,
        spec: MonitorSpec,
        key: OutputKey,
        encoding: Encoding,
    },
}

/// What the worker did, as an event for the control loop.
pub enum Done {
    Connected(Box<Connected>),
    /// No desktop to work with (or no encoder at all): why.
    NoDesktop(anyhow::Error),
    /// Screen `id` has its output.
    Made {
        id: ScreenId,
        key: OutputKey,
    },
    /// Screen `id` streams (its `attempt`, as the wanted state had it).
    Started {
        id: ScreenId,
        attempt: u32,
        encoding: Encoding,
    },
    /// Screen `id` could not be made or start streaming at `attempt`.
    Failed {
        id: ScreenId,
        attempt: u32,
        error: ErrorCode,
        why: String,
    },
    /// Screen `id` streams no more (the wanted state said so).
    Stopped {
        id: ScreenId,
    },
    /// Screen `id` lost its output and its stream: a display of the PC whose stream
    /// stopped (it is shown again shortly), or a monitor that failed (made again
    /// after its pause).
    Lost {
        id: ScreenId,
    },
    /// The window of panel `id` closed or left it: the panel is gone.
    PanelGone {
        id: ScreenId,
    },
    /// The desktop is laid out: where each output is, from what the worker took up
    /// to `upto` (see `Inbox::want`).
    Placed {
        on_desk: BTreeMap<OutputKey, DeskRect>,
        upto: u64,
    },
    /// The desktop has monitor `id` at another scale than it was made with, given in
    /// the desktop's own settings: it is taken (in percent), the output stays.
    Rescaled {
        id: ScreenId,
        percent: u16,
    },
    /// The PC's own displays, connected and on now.
    Physical(Vec<PhysicalDisplay>),
    /// The output the user works on.
    Focus(Option<OutputKey>),
    Job {
        job: JobId,
        result: anyhow::Result<JobOk>,
    },
    /// Nothing left to do of what was handed over, up to the count given (see
    /// `Inbox::want`).
    Settled(u64),
    /// The worker put everything back and ended.
    Gone,
}

/// What the worker starts from.
pub struct Start {
    /// The codec chosen, and whether on the CPU.
    pub codec: Codec,
    pub software: bool,
    /// The sizes offered to headsets, to find their scales.
    pub sizes: Vec<(u32, u32)>,
}

/// How long a request waits for the worker's answer: 20 s; in a build for the
/// end-to-end tests, the third number of `PROSTIR_TEST_DESK_LIMITS` if it has one.
pub fn answer_within() -> Duration {
    #[cfg(feature = "fake-desktop")]
    if let Some(secs) = std::env::var("PROSTIR_TEST_DESK_LIMITS")
        .ok()
        .and_then(|v| v.split(',').nth(2)?.trim().parse().ok())
    {
        return Duration::from_secs(secs);
    }
    Duration::from_secs(20)
}

/// A worker that ended (or will): set once it put everything back.
type Gone = Arc<(Mutex<bool>, Condvar)>;

/// The last worker started: a new one waits for it to be gone first, so two never
/// make outputs of the same names at once.
static LAST: Mutex<Option<Gone>> = Mutex::new(None);

/// Waits at most `limit` for the last worker to put everything back and end; whether
/// it did.
pub fn wait_gone(limit: Duration) -> bool {
    let Some(gone) = LAST.lock().unwrap().clone() else {
        return true;
    };
    let (lock, ring) = &*gone;
    let done = ring
        .wait_timeout_while(lock.lock().unwrap(), limit, |done| !*done)
        .unwrap()
        .0;
    *done
}

/// A worker let go, watched until it is gone: one that hangs while putting everything
/// back still ends the program after 2 min, with no session left to notice.
pub struct Retired {
    pub generation: u64,
    steps: Arc<Steps>,
    pub dog: Watchdog,
}

impl Retired {
    /// The step under way: since when, and what.
    pub fn step(&self) -> Option<(Instant, String)> {
        self.steps.now()
    }
}

/// The control loop's side of the worker. Dropped, it asks the worker to put
/// everything back (window panels first) and end; nothing waits for that.
pub struct Desk {
    inbox: Arc<Inbox<Wanted, Job>>,
    steps: Arc<Steps>,
    gone: Gone,
    /// Whether the worker is stuck, as the control loop sees it.
    pub dog: Watchdog,
    /// The wanted state handed over last.
    sent: Option<Wanted>,
    last_job: u64,
    generation: u64,
    /// How many things were handed over.
    posted: u64,
}

impl Desk {
    /// A worker of its own: it connects to the desktop (once the one before it is
    /// gone), says [`Done::Connected`] or [`Done::NoDesktop`] to `events`, and waits
    /// for work.
    pub fn start(start: Start, router: Arc<Router>, events: EventSink) -> Self {
        let inbox: Arc<Inbox<Wanted, Job>> = Arc::default();
        let steps = Arc::new(Steps::default());
        let gone: Gone = Arc::default();
        let before = LAST.lock().unwrap().replace(gone.clone());
        static GENERATION: AtomicU64 = AtomicU64::new(0);
        let generation = GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
        run::spawn(
            run::Shared {
                inbox: inbox.clone(),
                steps: steps.clone(),
                router,
                events,
                generation,
            },
            start,
            before,
            &gone,
        );
        let mut dog = Watchdog::default();
        // Connecting is work handed over.
        dog.sent();
        Self {
            inbox,
            steps,
            gone,
            dog,
            sent: None,
            last_job: 0,
            generation,
            posted: 0,
        }
    }

    /// Which worker this is: what an earlier one says after it was let go is not news
    /// of this one.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Hands over the state wanted now, if it is not the one handed over last.
    pub fn want(&mut self, wanted: Wanted) {
        if self.sent.as_ref() == Some(&wanted) {
            return;
        }
        self.sent = Some(wanted.clone());
        self.posted = self.inbox.want(wanted);
        self.dog.sent();
    }

    pub fn ask(&mut self, job: Job) {
        self.posted = self.inbox.ask(job);
        self.dog.sent();
    }

    /// How many things were handed over so far: a [`Done::Placed`] up to this count
    /// laid out what was handed over by now.
    pub fn posted(&self) -> u64 {
        self.posted
    }

    /// The worker did all it was given up to `upto`: once that is all, it waits for
    /// nothing.
    pub fn settled(&mut self, upto: u64) {
        if upto >= self.posted {
            self.dog.settled();
        }
    }

    /// A number for a job whose answer someone waits for.
    pub fn job_id(&mut self) -> JobId {
        self.last_job += 1;
        JobId(self.last_job)
    }

    /// The step under way: since when, and what.
    pub fn step(&self) -> Option<(Instant, String)> {
        self.steps.now()
    }

    /// Takes back a job not taken yet, nobody waiting for it any more.
    pub fn cancel(&self, job: JobId) {
        if self.inbox.cancel(|j| j.id() == Some(job)) {
            log::info!("the desktop's job {} is not needed any more", job.0);
        }
    }

    /// Lets the worker go (it puts everything back and ends): what to watch until it
    /// is gone, unless it is already.
    pub fn let_go(self) -> Option<Retired> {
        let gone = *self.gone.0.lock().unwrap();
        let mut dog = Watchdog::default();
        // Putting everything back is work handed over.
        dog.sent();
        (!gone).then(|| Retired {
            generation: self.generation,
            steps: self.steps.clone(),
            dog,
        })
    }
}

impl Drop for Desk {
    fn drop(&mut self) {
        self.inbox.quit();
    }
}
