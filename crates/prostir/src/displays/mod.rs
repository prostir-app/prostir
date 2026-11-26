//! The screens the headsets see, as the control loop keeps them: which there are,
//! their numbers, rows and states, the codec, sound and microphone. The desktop's
//! outputs and their encoders are the desktop's worker's (`worker/`): this model
//! tells it the whole state it wants ([`Displays::push`]) and follows what it says
//! back ([`Displays::desk_done`]); nothing here waits for the desktop or an encoder.
//! Delivery to the headsets is the [`Router`]'s.

use crate::i18n::t;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use log::{info, warn};
use prostir_layout::desk;
use prostir_proto::state;
use prostir_proto::{Codec, HostMsg};

use crate::encoder::{Backend, EncoderConfig};
use crate::errors::{Counted, UserError};
use crate::events::EventSink;
use crate::platform::audio::{self, MicIn};
use crate::platform::sound::{MicAvailability, OutputDevice, Route, SoundStatus};
use crate::platform::{
    DesktopCaps, MonitorSpec, OutputKey, PhysicalDisplay, Scales, Spot, WindowList,
};
use crate::router::Router;
use crate::screen::{Screen, ScreenId, ScreenKind, ScreenState};
use crate::store::{LEGACY_LAPTOP, Physical, PhysicalKey, SavedScreens, Settings};
use crate::worker::sound::{Sound, SoundDone, SoundWanted};
use crate::worker::{self, Desk, Done, Job, JobId, Layout, Primary, Wanted, WantedScreen, What};

mod arrange;
mod encoding;
mod parts;
mod physical;
mod placing;
mod sound;

pub use sound::MicTap;
mod windows;

use encoding::too_big_why;
use parts::rows_in_step;

/// Monitors of versions before the counter were numbered from 0 (and their outputs
/// `Prostir-(n + 1)`), below this.
const LEGACY_IDS: u32 = 100;
/// Virtual monitors a headset gets: more streams than this overwhelm its hardware
/// decoders.
pub const MONITORS_MAX: usize = 6;
/// Virtual monitors with "No limits", for a headset that decodes more: the most the PC
/// ever makes (the limit of the headset asking is checked before, see `requests`).
pub const MONITORS_NO_LIMITS: usize = 10;
/// How far from the origin of the desktop a monitor is placed at most (logical
/// pixels, either way): a place asked for beyond it holds at it.
pub const PLACE_LIMIT: i32 = 65_536;
/// Window panels at once: the headset has only so many video decoders.
pub const WINDOWS_MAX: usize = 3;
/// Displays of the PC kept while not connected: beyond these, the least recently seen
/// are forgotten (a laptop that met many projectors would gather them for ever).
pub const ABSENT_KEPT: usize = 8;

/// While a display of the PC is connected, when it was seen is kept this often (in
/// seconds), and when it goes and the app quits.
const SEEN_EVERY: u64 = 3600;

/// Now, in seconds since 1970, for when a display of the PC was last seen.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// How one screen encodes (see [`Displays::encoding_report`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputEncoding {
    pub id: ScreenId,
    /// Its name, for people (`Prostir-2`).
    pub name: String,
    /// The encoder, for people (`VA-API`, `x264`).
    pub encoder: &'static str,
    pub codec: Codec,
    /// On the graphics card.
    pub hardware: bool,
    /// Its frames go through the CPU on their way to the encoder.
    pub copies: bool,
    /// Why it is not the best this PC has for it, when it is not.
    pub note: Option<String>,
    /// Its encoder fell back to the faster setting for dropping frames (see
    /// `Router::slowed`).
    pub slowed: bool,
    /// What its encoder made in the last window of the router's report.
    pub stats: Option<crate::router::StreamStats>,
}

/// A screen that did not change for this long gets one keyframe (see
/// [`Router::still_keyframes`]).
const STILL: Duration = Duration::from_secs(3);
/// The same after a big change (another file, another tab), whose picture went out
/// coarse, where the encoder's keyframe is sharper (see `Router::still_keyframes`):
/// asked for this soon, on time (the control loop wakes for it, see
/// [`Displays::still_due`]). A capture that pushes its own frames sends its picture at
/// once (KWin's stream through the feed too; through `pipewiresrc` it came with the
/// keepalive, 0.75 s after the last frame), so the keyframe comes then. A keyframe is the only way to sharpen a picture on VA-API
/// (a change of bitrate on the way makes one), and one of `target-usage` 4 is about as
/// good as one with a constant quantiser of 10: measured on 2560x1440 text at 33
/// Mbit/s, from 34 to 53 dB (PSNR of the text), 3 s sooner than before.
const STILL_EARLY: Duration = Duration::from_millis(650);

/// A headset that waited this long for a fresh picture of a stream that sent nothing
/// meanwhile gets it from the stream starting over.
const STARVED: Duration = Duration::from_secs(3);

/// What the desktop's worker found when it connected.
struct Facts {
    caps: DesktopCaps,
    scales: Vec<((u32, u32), Scales)>,
    window_list: Option<std::sync::Weak<dyn WindowList>>,
    has_windows: bool,
    /// It tells the mouse pointer apart from the pictures.
    cursor: bool,
}

/// What came of something the worker did, that the control loop acts on.
pub enum DeskNews {
    /// Nothing more to do than show it.
    None,
    /// The desktop is there: sessions waiting for it may open.
    Connected,
    /// There is no desktop to work with: why.
    NoDesktop(anyhow::Error),
    /// The answer to a job someone waits for.
    Job(JobId, Result<state::DisplayId, anyhow::Error>, JobAnswer),
    /// A monitor's scale was given in the desktop's own settings and taken: the set
    /// kept for next time has it.
    Rescaled,
}

/// What a job's answer carries for the headset (or the local API).
pub enum JobAnswer {
    Done,
    Added,
    /// A picture saved at `path`; `whole`: of the whole desktop.
    Saved {
        path: std::path::PathBuf,
        whole: bool,
    },
    Pointer(u16, u16),
}

pub struct Displays {
    /// Every screen: virtual monitors, window panels, and every display of the PC
    /// itself seen so far, connected now or not.
    screens: BTreeMap<ScreenId, Screen>,
    /// Window panels being made by the worker: their output numbers, and the job.
    detaching: BTreeMap<ScreenId, (u32, JobId)>,
    /// How the headset arranges the monitors in rows: every monitor, once, and the
    /// laptop screen where the headset put it (see `effective_rows`). The one source of
    /// their order.
    rows: Vec<Vec<desk::Entry<ScreenId>>>,
    /// Where each output is on the desktop as laid out last, by name: Prostir's and the
    /// PC's own displays, shown or not.
    on_desk: BTreeMap<OutputKey, state::DeskRect>,
    /// Monitors just added while the headset places the monitors itself: their place
    /// on the desktop is a stand-in until the headset's arrangement has them and the
    /// desktop was laid out with it (see [`Displays::desk_known`]).
    placing: placing::Placings,
    /// The main headset of these sessions sent where the monitors stand: it places a
    /// monitor added later itself.
    headset_places: bool,
    /// When each display of the PC was last connected, in seconds since 1970 (0: not
    /// known), to forget the least recently seen of those not connected.
    physical_seen: BTreeMap<ScreenId, u64>,
    /// The monitors whose scale the main headset chooses ("Auto"): only virtual
    /// monitors, never a display of the PC or a window panel.
    auto: std::collections::BTreeSet<ScreenId>,
    /// Displays larger than the headset decodes (see [`Displays::stop_too_big`]): the
    /// size and codec they did not fit in, until a headset or codec they fit.
    too_big: BTreeMap<ScreenId, ((u32, u32), Codec)>,
    /// The settings as last applied from the store (see [`Displays::apply`]).
    settings: Settings,
    /// Where the PC's sound goes to the headset, while it does (the sound thread
    /// makes it, see `worker/sound.rs`).
    audio: Option<(Route, OutputDevice)>,
    /// The headset microphone on the PC, shared with the sound thread, which made it
    /// and lets it go (this share goes first).
    mic: Option<Arc<MicIn>>,
    /// The same, for the sessions' readers.
    mic_tap: Arc<MicTap>,
    /// Raised when the person switches sound or the microphone on: tried at once.
    by_hand: u32,
    /// Raised to have a recording's way in looked at again.
    recheck: u32,
    /// The headset's microphone switch is on: the PC's end of it is there.
    mic_wanted: bool,
    /// Why the headset microphone could not be made on the PC, while it cannot.
    mic_failed: Option<MicAvailability>,
    /// A program on the PC records from the headset microphone, as the sound system
    /// last said (see [`Displays::set_recording`]).
    recording: bool,
    /// Follows the sound system while sound or the microphone is there; its changes
    /// go to `sound_to` (the control loop).
    sound_events: Option<audio::Watcher>,
    sound_to: Option<EventSink>,
    /// The sound as last written in the log.
    sound_logged: Option<SoundStatus>,
    /// What the PC can encode with (see `encoder::available`), found once.
    choices: Vec<(Codec, bool)>,
    /// A headset with a session is worn (encoders running).
    streaming: bool,
    /// The streams go in H.264 because a headset did not decode the codec chosen:
    /// they go back to it once every headset with a session does.
    fitted_down: bool,
    /// When a screen last streamed (or the session began): nothing streaming for
    /// `encoding::NOTHING_STREAMS` ends the sessions (see [`Displays::healthy`]).
    streamed_at: Instant,
    pub router: Arc<Router>,
    cfg: EncoderConfig,
    /// Every headset with a session is connected over USB (see [`Displays::set_usb`]).
    usb: bool,
    primary: Option<ScreenId>,
    /// Whether to pick KDE's primary monitor ourselves.
    manage_primary: bool,
    /// The output the user works on, as the desktop last said.
    focus: Option<ScreenId>,
    /// What the desktop is, once the worker connected to it.
    facts: Option<Facts>,
    /// The sound thread (after `mic`: its share of the microphone goes last).
    sound: Sound,
    /// The input from the headsets (see `crate::input`): before the worker, so its
    /// devices go first.
    input: crate::input::Input,
    /// The desktop's worker: dropped, it puts everything back (window panels first).
    desk: Desk,
}

impl Displays {
    /// The screens of `set`, each with its own number; sound, microphone and the
    /// laptop screen as `settings` say. The desktop's worker starts connecting; the
    /// screens are made once it did (see [`Displays::connected`]), and their changes
    /// go to the control loop through `events`.
    pub fn create(
        set: &SavedScreens,
        settings: &Settings,
        manage_primary: bool,
        events: &EventSink,
    ) -> Self {
        let router = Arc::new(Router::new());
        router.send_events_to(events.clone());
        let desk = Desk::start(
            worker::Start {
                codec: settings.codec,
                software: settings.software,
                sizes: crate::publish::SIZES.to_vec(),
            },
            router.clone(),
            events.clone(),
        );
        let sound = Sound::start(router.clone(), events.clone(), desk.generation());
        let input = {
            let events = events.clone();
            let router = router.clone();
            crate::input::Input::start(
                move |news| {
                    events.send(crate::events::Event::Input(news));
                },
                move |p| router.pointing(p),
            )
        };
        let mut displays = Self {
            screens: BTreeMap::new(),
            detaching: BTreeMap::new(),
            rows: set.rows.clone(),
            on_desk: BTreeMap::new(),
            placing: placing::Placings::default(),
            headset_places: false,
            physical_seen: set.physical.iter().map(|(id, p)| (*id, p.seen)).collect(),
            auto: set
                .auto
                .iter()
                .filter(|id| set.specs.contains_key(id))
                .copied()
                .collect(),
            too_big: BTreeMap::new(),
            settings: settings.clone(),
            audio: None,
            mic: None,
            mic_tap: Arc::default(),
            by_hand: 0,
            recheck: 0,
            mic_wanted: false,
            mic_failed: None,
            recording: false,
            sound_events: None,
            sound_to: Some(events.clone()),
            sound_logged: None,
            choices: Vec::new(),
            streaming: true,
            fitted_down: false,
            streamed_at: Instant::now(),
            router,
            cfg: EncoderConfig {
                codec: settings.codec,
                bitrate_kbps: settings.bitrate * 1000,
                large_kbps: None,
                backend: Backend::X264,
                sharp: settings.sharp,
                rows_up: false,
            },
            usb: false,
            primary: None,
            manage_primary,
            focus: None,
            facts: None,
            sound,
            input,
            desk,
        };
        // In the order of the rows, then any the rows leave out.
        let mut ids: Vec<ScreenId> = set
            .rows
            .iter()
            .flatten()
            .map(|e| e.id)
            .filter(|id| set.specs.contains_key(id))
            .collect();
        let rest: Vec<ScreenId> = set
            .specs
            .keys()
            .filter(|id| !ids.contains(id))
            .copied()
            .collect();
        ids.extend(rest);
        // The PC's own displays as they were known, absent until seen.
        for (id, p) in &set.physical {
            let kind = ScreenKind::Physical {
                key: p.key.clone(),
                label: p.key.connector.clone(),
                builtin: p.builtin,
                shown: p.shown,
                size: None,
            };
            let mut screen = Screen::starting(*id, 0, kind);
            screen.state = ScreenState::Absent;
            displays.screens.insert(*id, screen);
        }
        // Monitors keep the output names they had: monitor 0 is `Prostir-1` as
        // before, others the lowest free number.
        let ids: Vec<ScreenId> = ids.into_iter().take(MONITORS_NO_LIMITS).collect();
        let mut numbers: BTreeMap<ScreenId, u32> = BTreeMap::new();
        for id in &ids {
            let n = id.number() + 1;
            if id.number() < LEGACY_IDS && !numbers.values().any(|v| *v == n) {
                numbers.insert(*id, n);
            }
        }
        for id in &ids {
            if !numbers.contains_key(id) {
                let n = (1..)
                    .find(|n| !numbers.values().any(|v| v == n))
                    .unwrap_or(1);
                numbers.insert(*id, n);
            }
        }
        for id in ids {
            displays.add_as(id, numbers[&id], set.specs[&id].clone());
        }
        match set.primary {
            Some(id) if displays.is_monitor(id) || displays.is_physical(id) => {
                let _ = displays.set_primary(id);
            }
            _ => displays.ensure_primary(),
        }
        displays
    }

    /// The desktop's worker connected: what the desktop can do, how the PC encodes,
    /// and the PC's own displays (one seen for the first time gets its number from
    /// `new_id`). The screens are made from here on.
    fn connected(&mut self, c: worker::Connected, new_id: &mut dyn FnMut() -> ScreenId) {
        info!("desktop: {}", c.caps.name);
        // Only some desktops (KDE) have a primary output.
        self.manage_primary &= c.caps.has_primary;
        self.cfg.backend = c.backend;
        self.cfg.codec = c.codec;
        self.choices = c.choices;
        self.router.set_cursor_source(c.cursor);
        self.input.set_opener(c.input);
        self.facts = Some(Facts {
            caps: c.caps,
            scales: c.scales,
            window_list: c.window_list,
            has_windows: c.has_windows,
            cursor: c.cursor,
        });
        self.sync_physical(&c.physical, new_id);
        self.push();
    }

    /// Whether the desktop's worker connected.
    pub fn is_connected(&self) -> bool {
        self.facts.is_some()
    }

    /// The whole state wanted of the desktop now, to its worker (only when it changed,
    /// and once it connected).
    pub fn push(&mut self) {
        if self.facts.is_some() {
            let wanted = self.wanted();
            self.desk.want(wanted);
            self.placing.handed(self.desk.posted());
            self.placing.prune(Instant::now());
        }
    }

    /// The desktop's worker, for the watch on it.
    pub fn desk(&self) -> &Desk {
        &self.desk
    }

    pub fn desk_mut(&mut self) -> &mut Desk {
        &mut self.desk
    }

    /// Lets the screens go: the desktop's worker puts everything back and ends; what to
    /// watch until it is gone (see [`Desk::let_go`]).
    pub fn let_go(self) -> Option<worker::Retired> {
        let Self { desk, .. } = self;
        desk.let_go()
    }

    /// The desktop does not respond (see `worker/watch.rs`).
    pub fn desk_stuck(&self) -> bool {
        self.desk.dog.stuck()
    }

    /// The state wanted of the desktop: every screen, how to encode, whether the
    /// encoders run, the layout.
    fn wanted(&self) -> Wanted {
        let mut screens: Vec<(ScreenId, WantedScreen)> = self
            .display_order()
            .into_iter()
            .filter_map(|id| {
                let s = self.screens.get(&id)?;
                let what = match &s.kind {
                    ScreenKind::Virtual { spec } => What::Monitor {
                        spec: spec.clone(),
                        number: s.number,
                    },
                    ScreenKind::Window { .. } => What::Panel {
                        number: s.number,
                        detach: None,
                    },
                    ScreenKind::Physical { key, .. } if s.streaming => What::Physical {
                        key: OutputKey::new(key.connector.as_str()),
                        number: s.number,
                    },
                    ScreenKind::Physical { .. } => return None,
                };
                let wanted = WantedScreen {
                    what,
                    stream: !self.too_big.contains_key(&id),
                    attempt: s.attempt,
                    name: s.name(),
                };
                Some((id, wanted))
            })
            .collect();
        for (id, (number, job)) in &self.detaching {
            screens.push((
                *id,
                WantedScreen {
                    what: What::Panel {
                        number: *number,
                        detach: Some(*job),
                    },
                    stream: true,
                    attempt: 0,
                    name: format!("Prostir-{number}"),
                },
            ));
        }
        // Displays of the PC that stream: the laptop's panel first.
        let mut physical: Vec<(bool, ScreenId)> = self
            .physical()
            .filter(|s| s.streaming)
            .map(|s| {
                (
                    !matches!(s.kind, ScreenKind::Physical { builtin: true, .. }),
                    s.id,
                )
            })
            .collect();
        physical.sort();
        let primary = self
            .primary
            .filter(|_| self.manage_primary)
            .and_then(|p| self.screens.get(&p))
            .map(|s| match &s.kind {
                // A display of the PC is the primary one under its connector, shown
                // or not.
                ScreenKind::Physical { key, .. } => {
                    Primary::Display(OutputKey::new(key.connector.as_str()))
                }
                _ => Primary::Screen(s.id),
            });
        Wanted {
            screens,
            encoding: self.cfg.clone(),
            streaming: self.streaming,
            layout: Layout {
                rows: self.effective_rows(),
                main: self.primary.or_else(|| self.order().first().copied()),
                physical: physical.into_iter().map(|(_, id)| id).collect(),
                column: self.windows().map(|s| s.id).collect(),
                primary,
            },
            wake_physical: self.physical().any(Screen::is_shown_physical),
            // Every headset with a session draws the pointer itself (the router
            // knows them at once; a session opening is attached before this).
            cursor_apart: self.tells_cursor() && self.router.cursor_apart(),
        }
    }

    /// What the desktop's worker said; one seen for the first time of the PC's own
    /// displays gets its number from `new_id`.
    pub fn desk_done(&mut self, done: Done, new_id: &mut dyn FnMut() -> ScreenId) -> DeskNews {
        match done {
            Done::Connected(c) => {
                // Connecting is done.
                self.desk.settled(0);
                self.connected(*c, new_id);
                return DeskNews::Connected;
            }
            Done::NoDesktop(e) => {
                self.desk.settled(0);
                return DeskNews::NoDesktop(e);
            }
            Done::Made { id, key } => {
                if let Some(s) = self.screens.get_mut(&id) {
                    s.output = Some(key);
                }
            }
            Done::Started {
                id,
                attempt,
                encoding,
            } => {
                // One that is to stream no more (too big for every headset) stops soon.
                let wanted = self.wants_stream(id);
                if let Some(s) = self.screens.get_mut(&id).filter(|_| wanted) {
                    if attempt != s.attempt {
                        info!("{}: streams, as tried before", s.name());
                    }
                    s.encoding = Some(encoding);
                    s.retry_at = None;
                    s.running_since = Some(Instant::now());
                    if !matches!(s.state, ScreenState::Absent) {
                        s.state = ScreenState::Running;
                    }
                }
            }
            Done::Failed {
                id,
                attempt,
                error,
                why,
            } => {
                // A failure of an earlier attempt, or of a stream no longer wanted,
                // says nothing of now.
                let wanted = self.wants_stream(id);
                if let Some(s) = self
                    .screens
                    .get_mut(&id)
                    .filter(|s| wanted && s.attempt == attempt)
                {
                    s.encoding = None;
                    if s.is_physical() {
                        warn!("cannot show {}: {why}", s.name());
                    }
                    // A monitor is tried again by itself, after its pause.
                    if s.is_virtual() {
                        s.failed_at(Instant::now());
                    }
                    s.state = ScreenState::Failed { error, why };
                }
            }
            Done::Stopped { id } => {
                if let Some(s) = self.screens.get_mut(&id) {
                    s.encoding = None;
                }
            }
            Done::Lost { id } => {
                if let Some(s) = self.screens.get_mut(&id) {
                    s.encoding = None;
                    s.output = None;
                    if s.streaming {
                        s.state = ScreenState::Starting;
                    }
                }
            }
            Done::PanelGone { id } => {
                if self.screens.get(&id).is_some_and(Screen::is_window) {
                    self.screens.remove(&id);
                }
            }
            Done::Placed { on_desk, upto } => {
                self.on_desk = on_desk;
                self.placing.laid_out(upto, Instant::now());
            }
            Done::Rescaled { id, percent } => {
                if let Some(Screen {
                    kind: ScreenKind::Virtual { spec },
                    ..
                }) = self.screens.get_mut(&id)
                {
                    spec.scale = f64::from(percent) / 100.0;
                    // Chosen by the person, in the desktop's settings.
                    if self.auto.remove(&id) {
                        info!("{id}: the scale given on the desktop ends its Auto");
                    }
                    return DeskNews::Rescaled;
                }
            }
            Done::ScaleRefused { id, percent } => {
                if let Some(Screen {
                    kind: ScreenKind::Virtual { spec },
                    ..
                }) = self.screens.get_mut(&id)
                {
                    warn!("{id}: the desktop kept it at {percent} %");
                    spec.scale = f64::from(percent) / 100.0;
                    return DeskNews::Rescaled;
                }
            }
            Done::Physical(seen) => self.sync_physical(&seen, new_id),
            Done::Focus(key) => self.focus = key.and_then(|k| self.id_of_output(&k)),
            Done::Job { job, result } => return self.job_done(job, result),
            Done::Settled(upto) => self.desk.settled(upto),
            // Said after everything else of this worker; the control loop looks at it
            // before this (see `Brain::desk`).
            Done::Gone => {}
        }
        DeskNews::None
    }

    /// Whether screen `id` is to stream: not one too big for every headset, nor a
    /// display of the PC that is not shown or not connected.
    fn wants_stream(&self, id: ScreenId) -> bool {
        self.screens
            .get(&id)
            .is_some_and(|s| (!s.is_physical() || s.streaming) && !self.too_big.contains_key(&id))
    }

    /// Nobody waits for job `job` any more (its request failed): a window panel it
    /// would make is not wanted.
    pub fn forget_job(&mut self, job: JobId) {
        self.detaching.retain(|_, (_, j)| *j != job);
    }

    /// A job someone waits for is done.
    fn job_done(&mut self, job: JobId, result: Result<worker::JobOk>) -> DeskNews {
        use worker::JobOk;
        let news = |r: Result<JobAnswer>| match r {
            Ok(answer) => DeskNews::Job(job, Ok(state::DisplayId(0)), answer),
            Err(e) => DeskNews::Job(job, Err(e), JobAnswer::Done),
        };
        match result {
            Ok(JobOk::Done) => news(Ok(JobAnswer::Done)),
            Ok(JobOk::Saved { path, whole }) => news(Ok(JobAnswer::Saved { path, whole })),
            Ok(JobOk::Pointer { key, u, v }) => match self.id_of_output(&key) {
                Some(id) => DeskNews::Job(job, Ok(id.wire()), JobAnswer::Pointer(u, v)),
                None => news(Err(anyhow::Error::new(UserError::NotOnDisplays))),
            },
            Ok(JobOk::Panel {
                id,
                spec,
                key,
                encoding,
            }) => {
                if !self.detaching.contains_key(&id) {
                    // Made too late (the headset was told it failed): the worker closes
                    // it, no longer wanted, and puts the window back.
                    info!("window panel {id} came too late: closing it");
                    return DeskNews::None;
                }
                self.panel_made(id, spec, key, encoding);
                DeskNews::Job(job, Ok(id.wire()), JobAnswer::Added)
            }
            Err(e) => {
                // A panel that could not be made is no more on its way.
                self.detaching.retain(|_, (_, j)| *j != job);
                news(Err(e))
            }
        }
    }

    /// Housekeeping, about twice a second: logs what the streams sent now and then,
    /// stops streams too big for every headset, starts over those a headset waits for
    /// a picture of in vain, and has the worker look after the outputs (sizes, the
    /// primary output, window panels, the PC's own displays).
    pub fn tick(&mut self) {
        self.router.report();
        self.router.pending_keyframes();
        self.sync_audio();
        // A way in for a recording that broke is made anew (nothing is asked), on the
        // sound thread.
        if self.recording && self.mic.is_some() {
            self.recheck += 1;
            self.sync_audio();
        }
        self.stop_too_big();
        if self.some_stream() {
            self.streamed_at = Instant::now();
        }
        if self.retry_due(Instant::now()) {
            self.push();
        }
        // A fresh picture asked of a stream that then stood still never came.
        let starved = self.router.starved(STARVED);
        if self.streaming && !starved.is_empty() {
            for id in &starved {
                info!("{id}: no fresh picture came for a headset: starting its stream over");
            }
            self.desk.ask(Job::Fresh(starved.into_iter().collect()));
        }
        self.desk.ask(Job::Tick);
    }

    /// Follows the settings `wanted` (as the store has them now): quality and the text
    /// preset on every encoder (through the worker), sound, microphone. Another codec
    /// is [`Displays::encode_with`].
    pub fn apply(&mut self, wanted: &Settings) {
        let was = std::mem::replace(&mut self.settings, wanted.clone());
        self.cfg.bitrate_kbps = wanted.bitrate * 1000;
        self.cfg.large_kbps = self.large_kbps();
        self.cfg.sharp = wanted.sharp;
        // Switched on by hand: tried right away.
        if wanted.audio != was.audio {
            self.by_hand += 1;
        }
        self.sync_audio();
    }

    /// Every headset with a session is connected over USB (`usb`) or not: while the
    /// quality is not chosen, monitors of 2560x1440 or more stream at
    /// [`crate::store::USB_LARGE`] then.
    pub fn set_usb(&mut self, usb: bool) {
        if usb == self.usb {
            return;
        }
        self.usb = usb;
        self.cfg.large_kbps = self.large_kbps();
        if let Some(kbps) = self.cfg.large_kbps {
            info!(
                "over USB: {} Mbit/s for each monitor of 2560x1440 or more",
                kbps / 1000
            );
        }
    }

    /// The bitrate of a picture of 2560x1440 or more, where it is another: over USB,
    /// while the quality is not chosen and is less.
    fn large_kbps(&self) -> Option<u32> {
        Self::large_kbps_for(self.usb, &self.settings)
    }

    /// The same for `settings`, over USB (`usb`) or not.
    fn large_kbps_for(usb: bool, settings: &Settings) -> Option<u32> {
        let large = crate::store::USB_LARGE;
        (usb && !settings.bitrate_chosen && large > settings.bitrate).then_some(large * 1000)
    }

    /// Mbit/s for each monitor of 2560x1440 or more now, where another than the quality
    /// (see [`Displays::set_usb`]).
    pub fn large_mbit(&self) -> Option<u32> {
        self.cfg.large_kbps.map(|k| k / 1000)
    }

    /// The desktop worker's number, which the sound thread's news carry too.
    pub fn generation(&self) -> u64 {
        self.desk.generation()
    }

    /// The screen the user works on, as the desktop last said.
    pub fn active_output(&self) -> Option<ScreenId> {
        self.focus
    }

    /// Asks the desktop again which screen the user works on (where it can tell).
    pub fn look_at_focus(&mut self) {
        if self.facts.as_ref().is_some_and(|f| f.has_windows) {
            self.desk.ask(Job::Focus);
        }
    }

    /// Streams while a headset with a session is worn; otherwise the encoders pause
    /// and sound and microphone go back to the PC (the monitors and windows stay).
    pub fn set_streaming(&mut self, on: bool) {
        if on != self.streaming {
            self.streaming = on;
            info!(
                "{}",
                if on {
                    "streaming"
                } else {
                    "paused: headset not worn or gone"
                }
            );
        }
        self.sync_audio();
    }

    /// A headset just connected (the router is attached): every stream starts over
    /// once, so each monitor sends its picture now, monitors kept from the last
    /// session too (the other headsets get a keyframe of it). The encoders run from
    /// here; [`Displays::worn`] pauses them again if no headset is worn.
    pub fn headset_connected(&mut self) {
        // A new session tries every encoder again for each monitor.
        let monitors = self.monitors().map(|s| s.id).collect();
        self.desk.ask(Job::Forgive(monitors));
        self.streamed_at = Instant::now();
        self.retry_too_big();
        self.retry_failed();
        // Paused ones (kept from before) start over as the worker resumes them;
        // running ones (just made, or streaming to other headsets) get a fresh picture
        // here.
        if self.streaming {
            let ids: Vec<ScreenId> = self.streams().collect();
            for id in ids {
                self.fresh_picture(id);
            }
        }
        self.set_streaming(true);
    }

    /// The screens that stream now.
    fn streams(&self) -> impl Iterator<Item = ScreenId> + '_ {
        self.screens
            .values()
            .filter(|s| s.encoding.is_some())
            .map(|s| s.id)
    }

    /// Whether a headset with a session is worn (`any`): the encoders run while one
    /// is. `fresh` are the streams a headset put on again needs a fresh picture of,
    /// which resuming the encoders gives anyway.
    pub fn worn(&mut self, any: bool, fresh: &[ScreenId]) {
        let resumes = any && !self.streaming;
        self.set_streaming(any);
        if !resumes {
            for id in fresh {
                self.fresh_picture(*id);
            }
        }
    }

    pub fn streaming(&self) -> bool {
        self.streaming
    }

    /// A fresh picture of screen `id` as a keyframe, though nothing on it moves: the
    /// headset missed what it sent before.
    ///
    /// A stream whose picture moves and that goes to another headset already sends a
    /// keyframe with its next frame, so that headset sees no hiccup; any other starts
    /// over to send its picture (and should a moving one stop, `tick` starts it over
    /// all the same, see [`Router::starved`]).
    pub fn fresh_picture(&mut self, id: ScreenId) {
        if !self.streaming || !self.screens.get(&id).is_some_and(|s| s.encoding.is_some()) {
            return;
        }
        if self.router.live(id) {
            self.router.force_keyframe(id);
        } else {
            self.desk.ask(Job::Fresh([id].into()));
        }
    }

    /// When a still screen's keyframe is next to be asked for (see
    /// [`Displays::still_keyframes`]): the control loop wakes then.
    pub fn still_due(&self) -> Option<Instant> {
        self.router.still_due(STILL, STILL_EARLY)
    }

    /// Asks for the keyframes of still screens that are due (see
    /// [`Router::still_keyframes`]).
    pub fn still_keyframes(&self) {
        self.router.still_keyframes(STILL, STILL_EARLY);
    }

    /// Asks the encoder of screen `id` for a keyframe, without waiting for anyone.
    pub fn force_keyframe(&self, id: ScreenId) {
        self.router.force_keyframe(id);
    }

    pub fn force_keyframes(&self) {
        self.router.force_keyframes();
    }
}

/// Streams at once at most: a lane each (`2..32`).
pub const STREAMS_MAX: u8 = 30;

/// How many streams a headset that can do what `headset` says takes at once.
pub fn streams_max(headset: &state::HeadsetCaps) -> usize {
    usize::from(match headset.streams_max {
        0 => STREAMS_MAX,
        n => n.min(STREAMS_MAX),
    })
}

#[cfg(test)]
mod tests;
