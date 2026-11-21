//! The one channel into the control loop (`main.rs`): every thread with something for
//! it (the listening socket, a connection being set up, a session's reader, the tray,
//! the local API, the signals) sends an [`Event`], and the control loop alone acts on
//! them. It waits for the next event or its next deadline, whichever comes first, and
//! without a session it has no deadline: it sleeps until something happens.
//!
//! The channel has no bound: events are small, and what is big (frames, sound, the
//! microphone's slices inside `Headset`) is few per event.

use std::net::SocketAddr;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Instant;

use prostir_proto::ClientMsg;
use prostir_proto::state::Part;

use crate::api::{Reply, Request};
use crate::pairing;
use crate::platform::TrayCommand;
use crate::platform::sound::SoundEvent;

/// A session's number, new for every connection: a message of a session that ended
/// (or was replaced) is never taken for one of the next.
pub type SessionId = u64;

/// What happened, for the control loop.
pub enum Event {
    /// A connection is set up and trusted, and its headset asked to open a session.
    Accepted(Box<Accepted>),
    /// A message from the headset of a session, with when it came (`Ping`, `Pong` and
    /// `FrameShown` are answered in the session's reader and never come here).
    Headset(SessionId, Instant, ClientMsg),
    /// The sound system changed: the headset's device moved, or a program began or
    /// stopped recording from the headset microphone.
    Sound(SoundEvent),
    /// A session's connection ended: its end (`UnexpectedEof`), silence (`TimedOut`),
    /// or an error. Nothing more comes from it.
    SessionEnded(SessionId, std::io::Error),
    /// An item of the tray.
    Tray(TrayCommand),
    /// A request of the local API (`prostir ctl`, the settings window, D-Bus), and
    /// where its reply goes.
    Ctl(Request, Reply),
    /// The PC began or stopped showing a pairing code, or a headset began or stopped
    /// waiting for its code to be typed in the settings window.
    Pairing,
    /// A headset paired, named so: the pairing window closes.
    Paired { name: String },
    /// A pairing attempt failed: the codes did not match.
    WrongCode,
    /// A headset that is not one of this PC's asked for it (`crate::find`).
    Asked(crate::find::Asker),
    /// The notice of a new headset nearby: its button was pressed (`true`), or it is
    /// gone.
    Notice(bool),
    /// The settings window came onto the session bus (`true`) or left it: while it is
    /// there, the program stays.
    Window(bool),
    /// The desktop's worker did something (see `worker`): which worker, and what.
    Desk(u64, crate::worker::Done),
    /// The sound thread made or let go of the sound or the microphone (see
    /// `worker/sound.rs`): whose (the desktop worker's number), and what.
    SoundDevices(u64, crate::worker::sound::SoundDone),
    /// A keyframe was asked of stream `id` while its screen is still: its source is to
    /// send a picture now (see `Router::keyframe_asked`).
    StillPicture(crate::screen::ScreenId),
    /// A signal asked the program to quit.
    Quit,
}

/// A connection ready for its session.
pub struct Accepted {
    pub conn: pairing::Connection,
    /// The parts of the headset's state its `Open` carried.
    pub open: Vec<Part>,
    pub from: SocketAddr,
}

/// Where a thread sends what happened. Every thread has its own (it is cloned).
#[derive(Clone)]
pub struct EventSink(Sender<Event>);

impl EventSink {
    /// Sends `event`; `false` once the control loop is gone (the program quits).
    pub fn send(&self, event: Event) -> bool {
        self.0.send(event).is_ok()
    }

    /// Sends an item of the tray.
    pub fn tray(&self, command: TrayCommand) -> bool {
        self.send(Event::Tray(command))
    }
}

/// The channel: the sinks for the threads, and the control loop's end.
pub fn channel() -> (EventSink, Receiver<Event>) {
    let (tx, rx) = mpsc::channel();
    (EventSink(tx), rx)
}
