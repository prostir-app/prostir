//! The PC's sound to the headset and the headset microphone on the PC, made and let go
//! on a thread of their own (`sound`): making them runs `pw-cli`, `pactl`,
//! `pw-loopback` and `pacat`, letting them go `pactl` again, and a sound system that
//! hangs must not stall the control loop. As with the desktop's worker, the control
//! loop hands over the state it wants (the newest wins) and hears what came of it as
//! events; each start and stop has a time limit. The microphone made here is shared
//! with the control loop, which feeds it the headset's sound (`MicIn::push` waits for
//! nothing); the control loop lets its share go before it asks for the microphone to
//! go, so the last share, and what letting it go runs, is this thread's.

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use log::{info, warn};

use super::inbox::{Inbox, Mergeable};
use super::timed;
use crate::events::{Event, EventSink};
use crate::platform::audio::{AudioOut, MicIn};
use crate::platform::sound::{MicAvailability, OutputDevice, Route};
use crate::router::Router;

/// How long starting the sound or the microphone may take, and letting it go.
const START_WITHIN: Duration = Duration::from_secs(10);
const STOP_WITHIN: Duration = Duration::from_secs(5);
/// While the sound or the microphone is there, it is looked at this often: a sound
/// system that went away (restarted, or Prostir's device removed) says nothing, and
/// the capture of a device that went takes the PC's speakers instead.
const CHECK_EVERY: Duration = Duration::from_secs(2);

/// What the control loop wants of the sound.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SoundWanted {
    /// The PC's sound goes to the headset.
    pub audio: bool,
    /// The headset microphone is a microphone of the PC.
    pub mic: bool,
    /// A program records from it: it gets a way in for the headset's sound.
    pub recording: bool,
    /// Raised to have a recording's way in looked at again (one that broke is made
    /// anew).
    pub recheck: u32,
    /// Raised when the person switched something on: what failed is tried at once,
    /// not after its pause.
    pub by_hand: u32,
}

/// What came of it, as an event for the control loop.
pub enum SoundDone {
    /// The PC's sound goes to the headset: where it plays, and the device captured.
    Audio(Route, OutputDevice),
    /// It does not (any more).
    AudioOff,
    /// The headset microphone is there.
    Mic(Arc<MicIn>),
    /// It could not be made: why.
    MicFailed(MicAvailability),
    /// It is gone.
    MicOff,
}

impl SoundDone {
    /// Lets this news go without the control loop running what letting a microphone
    /// go runs (`pactl`, stopping `pw-loopback`), should its share be the last: news
    /// no longer wanted (the sound thread may have let its own share go already).
    pub fn let_go(self) {
        if let SoundDone::Mic(mic) = self {
            let_go_of_mic(mic);
        }
    }
}

/// Lets a share of the microphone go on a thread of its own.
pub fn let_go_of_mic(mic: Arc<MicIn>) {
    let spawned = std::thread::Builder::new()
        .name("mic-stop".into())
        .spawn(move || drop(mic));
    if let Err(e) = spawned {
        warn!("no thread to let the microphone go: {e}");
    }
}

/// No jobs: the wanted state says everything.
pub enum NoJob {}

impl Mergeable for NoJob {
    fn merge(&mut self, other: &Self) -> bool {
        match *other {}
    }
}

/// Retry timing after failures: 2 s, then doubling up to 2 min.
#[derive(Default)]
struct Backoff {
    failures: u32,
    next: Option<Instant>,
}

impl Backoff {
    fn due(&self) -> bool {
        self.next.is_none_or(|t| Instant::now() >= t)
    }

    fn failed(&mut self) {
        self.failures += 1;
        let secs = (2u64 << self.failures.min(6)).min(120);
        self.next = Some(Instant::now() + Duration::from_secs(secs));
    }

    fn succeeded(&mut self) {
        *self = Self::default();
    }
}

/// A sound thread that ended (or will): set once it let everything go.
type Gone = Arc<(Mutex<bool>, Condvar)>;

/// The last sound thread started: quitting waits for it to give the sound back.
static LAST: Mutex<Option<Gone>> = Mutex::new(None);

/// Waits at most `limit` for the last sound thread to let everything go; whether it
/// did.
pub fn wait_gone(limit: Duration) -> bool {
    let Some(gone) = LAST.lock().unwrap().clone() else {
        return true;
    };
    let (lock, ring) = &*gone;
    *ring
        .wait_timeout_while(lock.lock().unwrap(), limit, |done| !*done)
        .unwrap()
        .0
}

/// The control loop's side of the sound thread. Dropped, the thread lets everything
/// go (the sound goes back to the PC) and ends.
pub struct Sound {
    inbox: Arc<Inbox<SoundWanted, NoJob>>,
    sent: Option<SoundWanted>,
}

impl Sound {
    /// A sound thread of its own: what it does goes to `events` as `generation`'s;
    /// the PC's sound goes to the headsets through `router`.
    pub fn start(router: Arc<Router>, events: EventSink, generation: u64) -> Self {
        let inbox: Arc<Inbox<SoundWanted, NoJob>> = Arc::default();
        let gone: Gone = Arc::default();
        *LAST.lock().unwrap() = Some(gone.clone());
        let thread = Thread {
            inbox: inbox.clone(),
            router,
            events,
            generation,
            wanted: SoundWanted::default(),
            audio: None,
            mic: None,
            audio_retry: Backoff::default(),
            mic_retry: Backoff::default(),
            recorded: None,
            checked: Instant::now(),
        };
        let mark_gone = move || {
            let (lock, ring) = &*gone;
            *lock.lock().unwrap() = true;
            ring.notify_all();
        };
        let spawned = std::thread::Builder::new()
            .name("sound".into())
            .spawn(move || {
                thread.run();
                mark_gone();
            });
        if let Err(e) = spawned {
            warn!("no thread for the sound: {e}");
        }
        Self { inbox, sent: None }
    }

    /// Hands over the state wanted now, if it is not the one handed over last.
    pub fn want(&mut self, wanted: SoundWanted) {
        if self.sent.as_ref() != Some(&wanted) {
            self.sent = Some(wanted.clone());
            self.inbox.want(wanted);
        }
    }
}

impl Drop for Sound {
    fn drop(&mut self) {
        self.inbox.quit();
    }
}

/// The sound thread.
struct Thread {
    inbox: Arc<Inbox<SoundWanted, NoJob>>,
    router: Arc<Router>,
    events: EventSink,
    generation: u64,
    wanted: SoundWanted,
    audio: Option<AudioOut>,
    mic: Option<Arc<MicIn>>,
    audio_retry: Backoff,
    mic_retry: Backoff,
    /// What the microphone was last told of recordings.
    recorded: Option<(bool, u32)>,
    /// When the sound and the microphone were last looked at.
    checked: Instant,
}

impl Thread {
    fn run(mut self) {
        loop {
            let mail = self.inbox.take(self.retry_at());
            if mail.quit {
                break;
            }
            if let Some(wanted) = mail.wanted {
                if wanted.by_hand != self.wanted.by_hand {
                    self.audio_retry.succeeded();
                    self.mic_retry.succeeded();
                }
                self.wanted = wanted;
            }
            self.sync();
        }
        self.wanted = SoundWanted::default();
        self.sync();
    }

    fn say(&self, done: SoundDone) {
        self.events.send(Event::SoundDevices(self.generation, done));
    }

    /// When something that failed is tried again, while it is wanted, and when what is
    /// there is looked at again.
    fn retry_at(&self) -> Option<Instant> {
        let audio = (self.wanted.audio && self.audio.is_none())
            .then_some(self.audio_retry.next)
            .flatten();
        let mic = (self.wanted.mic && self.mic.is_none())
            .then_some(self.mic_retry.next)
            .flatten();
        let check =
            (self.audio.is_some() || self.mic.is_some()).then(|| self.checked + CHECK_EVERY);
        crate::brain::earliest([audio, mic, check])
    }

    /// Lets go of the sound or the microphone that broke (the sound system went away,
    /// or removed Prostir's device), and says so, so the status tells the truth; what
    /// is still wanted is made again right after, as one that failed to start is (the
    /// pauses come from those starts). Left as it is, the PC's sound would play on its
    /// speakers, and the headset microphone would reach no program.
    fn check(&mut self) {
        self.checked = Instant::now();
        if let Some(audio) = self.audio.take_if(|a| !a.alive()) {
            warn!("the PC's sound no longer reaches the headset's device: making it again");
            let _ = timed::call(
                "sound-stop",
                "letting go of the PC's sound that broke",
                STOP_WITHIN,
                move || drop(audio),
            );
            self.say(SoundDone::AudioOff);
        }
        if let Some(mic) = self.mic.take_if(|m| !m.alive()) {
            warn!("the headset microphone is gone from the PC: making it again");
            // Closed here, for every share: the control loop's, let go when it hears
            // of it, has nothing left to run.
            let _ = timed::call(
                "mic-stop",
                "letting go of the microphone that broke",
                STOP_WITHIN,
                move || mic.close(),
            );
            self.recorded = None;
            self.say(SoundDone::MicOff);
        }
    }

    /// Makes or lets go of the sound and the microphone as wanted.
    fn sync(&mut self) {
        self.check();
        if self.wanted.audio && self.audio.is_none() && self.audio_retry.due() {
            let router = self.router.clone();
            let started = timed::call(
                "sound-start",
                "starting the PC's sound to the headset",
                START_WITHIN,
                move || AudioOut::start(move |pcm| router.send_audio(pcm)),
            )
            .map_err(anyhow::Error::from)
            .and_then(|r| r);
            match started {
                Ok(audio) => {
                    self.audio_retry.succeeded();
                    self.say(SoundDone::Audio(audio.route(), audio.device()));
                    self.audio = Some(audio);
                }
                Err(e) => {
                    warn!("no sound in the headset: {e:#}");
                    self.audio_retry.failed();
                    self.say(SoundDone::AudioOff);
                }
            }
        } else if !self.wanted.audio
            && let Some(audio) = self.audio.take()
        {
            let _ = timed::call(
                "sound-stop",
                "giving the PC's sound back",
                STOP_WITHIN,
                move || drop(audio),
            );
            self.say(SoundDone::AudioOff);
        }
        if self.wanted.mic && self.mic.is_none() && self.mic_retry.due() {
            let started = timed::call(
                "mic-start",
                "making the headset microphone",
                START_WITHIN,
                MicIn::start,
            )
            .map_err(anyhow::Error::from)
            .and_then(|r| r);
            match started {
                Ok(mic) => {
                    self.mic_retry.succeeded();
                    let mic = Arc::new(mic);
                    self.say(SoundDone::Mic(mic.clone()));
                    self.mic = Some(mic);
                    self.recorded = None;
                }
                Err(e) => {
                    warn!("no headset microphone: {e:#}");
                    self.mic_retry.failed();
                    self.say(SoundDone::MicFailed(MicAvailability::of_failure(&e)));
                }
            }
        } else if !self.wanted.mic
            && let Some(mic) = self.mic.take()
        {
            // The control loop let its share go before it asked: the last one goes
            // here, on a thread with a time limit.
            let _ = timed::call(
                "mic-stop",
                "giving the microphone back",
                STOP_WITHIN,
                move || drop(mic),
            );
            self.recorded = None;
            self.say(SoundDone::MicOff);
        }
        let told = (self.wanted.recording, self.wanted.recheck);
        if let Some(mic) = &self.mic
            && self.recorded != Some(told)
        {
            self.recorded = Some(told);
            let (mic, on) = (mic.clone(), self.wanted.recording);
            let set = timed::call(
                "mic-recording",
                "telling the microphone of a recording",
                STOP_WITHIN,
                move || mic.set_recording(on),
            );
            if set.is_err() {
                info!("the microphone is told of the recording again later");
                self.recorded = None;
            }
        }
    }
}
