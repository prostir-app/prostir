//! KWin's stream of an output, read from PipeWire straight into the encoder's
//! `appsrc` (in place of `pipewiresrc`, which gives no metadata).
//!
//! What stays as it was with `pipewiresrc` (PipeWire 1.6):
//! - KWin is offered the formats and DMA-BUF layouts the encoder takes, asked of the
//!   encoder's elements the same way (see `format.rs`), so GPU buffers go to VA-API as
//!   they are, and shared memory to the CPU encoders, with nothing copied (`frames.rs`).
//! - Only KWin's node: with `node.dont-fallback`, a node that is gone fails at once
//!   ("target not found") and is never swapped for another video source (a webcam).
//! - On a still screen KWin sends nothing; the last picture goes again every
//!   `KEEPALIVE_MS` (as `keepalive-time` did), and at once when a keyframe is asked of
//!   a still screen (as Hyprland's capture does).
//! - Paused, the stream is made inactive (KWin stops sending); resumed, it is
//!   connected again, so KWin sends the picture it has now.
//! - A stream that fails while starting is the source's failure ([`SourceFailed`]),
//!   once it ran an error of the source on the encoder's bus (its source is gone).
//!
//! New: each buffer's metadata, the pointer and the changed parts of the picture
//! ([`meta::Sightings`]), and buffers with the pointer alone are not encoded.
//!
//! Everything of PipeWire happens on the feed's own thread, which sleeps in PipeWire's
//! loop until KWin sends, the encoder's pause changes, GStreamer lets a frame go, or the
//! last picture is due again.

mod format;
mod frames;
pub mod meta;

use std::cell::RefCell;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Result;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use log::{debug, warn};
use pipewire as pw;
use pw::spa::pod::Pod;
use pw::spa::sys as ss;
use pw::stream::{Stream, StreamFlags, StreamRc, StreamState};

use crate::encoder::{Pause, SourceFailed};
use frames::{Pictures, Slots};
use meta::Sightings;

/// How long the start waits for KWin's stream to agree on a format (normally tens of
/// ms) before it goes on without knowing: a failure after it is the encoder's
/// source's, on its bus.
const FIRST_WORD: Duration = Duration::from_secs(3);

/// `PW_STREAM_FLAG_ASYNC`, by its value: the headers of older PipeWire (Debian 12's
/// 0.3.65) do not name it, and a PipeWire that does not know it leaves it be.
const ASYNC: StreamFlags = StreamFlags::from_bits_retain(1 << 10);

/// How long dropping a feed waits for its thread to end (a driver that hangs in a
/// call holds it): then it is left behind, said once in the log.
const STOP_WAIT: Duration = Duration::from_secs(2);

/// What the feed's thread is told, besides frames let go (which wake it through the
/// same file, see `frames.rs`).
enum Say {
    /// The encoder's pause changed, or a picture is asked again.
    Look,
    Stop,
}

/// What other threads tell the feed's thread: a queue, and an eventfd that wakes
/// PipeWire's loop. Not PipeWire's channel: it calls back with its lock held, so a
/// frame let go there (a picture replaced) could never say so.
#[derive(Clone)]
struct Mail {
    queue: mpsc::Sender<Say>,
    wake: Arc<OwnedFd>,
}

impl Mail {
    fn new() -> std::io::Result<(Self, mpsc::Receiver<Say>)> {
        // SAFETY: a new eventfd; checked below.
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: a descriptor just made, owned by nobody else.
        let wake = Arc::new(unsafe { OwnedFd::from_raw_fd(fd) });
        let (queue, read) = mpsc::channel();
        Ok((Self { queue, wake }, read))
    }

    fn send(&self, say: Say) {
        if self.queue.send(say).is_ok() {
            frames::wake(&self.wake);
        }
    }

    /// Clears the wake-up, before the queue is read.
    fn woken(&self) {
        let mut n = 0u64;
        // SAFETY: 8 bytes to fill, alive through the call.
        unsafe { libc::read(self.wake.as_raw_fd(), (&raw mut n).cast(), 8) };
    }
}

/// A running feed of one KWin stream into one encoder.
pub struct Feed {
    say: Mail,
    pause: Arc<Pause>,
    seen: Sightings,
    label: String,
    thread: Option<JoinHandle<()>>,
    /// Says when the thread ended.
    ended: mpsc::Receiver<()>,
}

impl Feed {
    /// Starts feeding KWin's PipeWire node `node` (`label` names it in the log) into
    /// `appsrc`, following the encoder's `pause`; the last picture goes again every
    /// `every` while the screen is still. Fails as the source ([`SourceFailed`]) when
    /// the stream cannot start.
    pub fn start(
        node: u32,
        label: &str,
        appsrc: gst_app::AppSrc,
        pause: Arc<Pause>,
        every: Duration,
    ) -> Result<Self> {
        let (say, hear) = Mail::new()?;
        let (started, first) = mpsc::channel();
        let (end, ended) = mpsc::channel();
        let seen = Sightings::default();
        {
            let say = say.clone();
            pause.tell(Some(Box::new(move || say.send(Say::Look))));
        }
        let thread = {
            let setup = Setup {
                node,
                label: label.to_owned(),
                appsrc,
                pause: pause.clone(),
                every,
                say: say.clone(),
                started,
                seen: seen.clone(),
            };
            std::thread::Builder::new()
                .name(format!("pw-{label}"))
                .spawn(move || {
                    run(setup, hear);
                    let _ = end.send(());
                })?
        };
        let feed = Self {
            say,
            pause,
            seen,
            label: label.to_owned(),
            thread: Some(thread),
            ended,
        };
        match first.recv_timeout(FIRST_WORD) {
            Ok(Ok(())) => Ok(feed),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                warn!("{label}: no format agreed with KWin's stream yet; going on");
                Ok(feed)
            }
            Ok(Err(why)) => Err(SourceFailed(why).into()),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(SourceFailed("the stream's thread ended".to_owned()).into())
            }
        }
    }

    /// The pointer and the changed parts of the picture, as the stream said them.
    #[cfg_attr(not(test), expect(dead_code, reason = "step 2 of the cursor layer"))]
    pub fn sightings(&self) -> Sightings {
        self.seen.clone()
    }
}

impl Drop for Feed {
    fn drop(&mut self) {
        self.pause.tell(None);
        self.say.send(Say::Stop);
        let Some(thread) = self.thread.take() else {
            return;
        };
        match self.ended.recv_timeout(STOP_WAIT) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = thread.join();
            }
            // Left behind: its stream and buffers go when it ends.
            Err(mpsc::RecvTimeoutError::Timeout) => warn!(
                "{}: the stream's thread did not stop in {} s; left behind",
                self.label,
                STOP_WAIT.as_secs()
            ),
        }
    }
}

/// What the feed's thread starts with.
struct Setup {
    node: u32,
    label: String,
    appsrc: gst_app::AppSrc,
    pause: Arc<Pause>,
    every: Duration,
    say: Mail,
    started: mpsc::Sender<Result<(), String>>,
    seen: Sightings,
}

/// The feed's state, on its thread. Never borrowed across a call into PipeWire that
/// can call back (connecting, disconnecting, a param, an error).
struct State {
    label: String,
    appsrc: gst_app::AppSrc,
    seen: Sightings,
    /// Told once whether the stream started; none after.
    started: Option<mpsc::Sender<Result<(), String>>>,
    /// What the encoder takes, asked when connecting.
    possible: gst::Caps,
    /// The layout of the frames of the format agreed on; none before one is.
    layout: Option<gst_video::VideoInfo>,
    slots: Slots,
    pictures: Pictures,
    paused: bool,
    /// Disconnected on purpose (started over, or stopping): not KWin's doing.
    leaving: bool,
    /// The stream failed or KWin ended it: nothing more goes to the encoder.
    over: bool,
}

type Shared = Rc<RefCell<State>>;

/// Says what failed: before the start returned, to it; after, on the encoder's bus
/// as its source's error (the source lost its stream).
fn failed(state: &Shared, why: String) {
    let mut st = state.borrow_mut();
    if st.over {
        return;
    }
    st.over = true;
    st.pictures.forget();
    if let Some(started) = st.started.take() {
        let _ = started.send(Err(why));
        return;
    }
    warn!("{}: {why}", st.label);
    gst::element_error!(st.appsrc, gst::ResourceError::Failed, ("{}", why));
}

/// Connects the stream to KWin's node, offering what the encoder takes now.
fn connect(stream: &Stream, state: &Shared, node: u32) -> Result<(), String> {
    let possible = {
        let st = state.borrow();
        let pad = st
            .appsrc
            .static_pad("src")
            .ok_or("the encoder has no input")?;
        format::sanitize(&pad.peer_query_caps(None))
    };
    let offers = format::offers(&possible);
    if offers.is_empty() {
        return Err(format!(
            "the encoder takes no format of a PipeWire stream: {possible}"
        ));
    }
    debug!("{}: offering KWin {possible}", state.borrow().label);
    {
        let mut st = state.borrow_mut();
        st.possible = possible;
        st.layout = None;
        st.over = false;
    }
    let mut params: Vec<&Pod> = offers.iter().filter_map(|b| Pod::from_bytes(b)).collect();
    // As `pipewiresrc`: linked by the session manager, never again once lost, frames
    // taken from PipeWire's loop.
    let flags = StreamFlags::AUTOCONNECT | StreamFlags::DONT_RECONNECT | ASYNC;
    stream
        .connect(
            pw::spa::utils::Direction::Input,
            Some(node),
            flags,
            &mut params,
        )
        .map_err(|e| format!("cannot connect to PipeWire node {node}: {e}"))
}

/// Puts the stream in error, as `pipewiresrc` does with a format it cannot take.
fn set_error(stream: &Stream, why: &str) {
    let why = std::ffi::CString::new(why).unwrap_or_default();
    // SAFETY: a valid stream; the message is a C string, formatted by `%s`.
    unsafe {
        pw::sys::pw_stream_set_error(
            stream.as_raw_ptr(),
            -libc::EINVAL,
            c"%s".as_ptr(),
            why.as_ptr(),
        );
    }
}

/// The layout of frames of `caps`.
fn layout_of(caps: &gst::CapsRef) -> Option<gst_video::VideoInfo> {
    let s = caps.structure(0)?;
    if s.get::<&str>("format").ok() == Some("DMA_DRM") {
        gst_video::VideoInfoDmaDrm::from_caps(caps)
            .ok()?
            .to_video_info()
            .ok()
    } else {
        gst_video::VideoInfo::from_caps(caps).ok()
    }
}

/// KWin chose a format (none: the one it had is gone).
fn format_chosen(stream: &Stream, state: &Shared, format: Option<&Pod>) {
    let Some(format) = format else {
        let mut st = state.borrow_mut();
        st.layout = None;
        st.pictures.forget();
        return;
    };
    let chosen = {
        let st = state.borrow();
        format::chosen(format.as_bytes(), &st.possible)
    };
    let Some(caps) = chosen else {
        set_error(stream, "unhandled format");
        return;
    };
    let Some(layout) = layout_of(&caps) else {
        set_error(stream, "internal error");
        return;
    };
    let started = {
        let mut st = state.borrow_mut();
        debug!("{}: KWin sends {caps}", st.label);
        // A picture of the format before never goes again under these caps.
        if st.layout.as_ref() != Some(&layout) {
            st.pictures.forget();
        }
        st.appsrc.set_caps(Some(&caps));
        st.layout = Some(layout);
        st.started.take()
    };
    let params = format::buffer_params(format::has_modifier(format.as_bytes()));
    let mut pods: Vec<&Pod> = params.iter().filter_map(|b| Pod::from_bytes(b)).collect();
    if let Err(e) = stream.update_params(&mut pods) {
        warn!("cannot ask KWin's stream for its buffers: {e}");
    }
    if let Some(started) = started {
        let _ = started.send(Ok(()));
    }
}

/// KWin sent frames: the pointer and the changed parts of each are seen, and the
/// newest picture goes to the encoder; the rest go back at once.
fn frames_came(stream: &Stream, state: &Shared) {
    let seen = state.borrow().seen.clone();
    let mut newest: Option<(*mut pw::sys::pw_buffer, Option<frames::Crop>)> = None;
    loop {
        // SAFETY: a stream of this thread.
        let b = unsafe { stream.dequeue_raw_buffer() };
        if b.is_null() {
            break;
        }
        // SAFETY: a buffer just dequeued, valid until queued back.
        let metas = unsafe { meta::metas((*b).buffer) };
        // SAFETY: as above.
        let picture = unsafe { frames::has_picture(b, metas.header) };
        let crop = metas.crop.and_then(frames::read_crop);
        seen.saw_frame(metas.cursor, metas.damage, picture);
        if picture && let Some((older, _)) = newest.replace((b, crop)) {
            // SAFETY: dequeued above, not handed to anyone.
            unsafe { stream.queue_raw_buffer(older) };
        } else if !picture {
            // SAFETY: as above.
            unsafe { stream.queue_raw_buffer(b) };
        }
    }
    let Some((b, crop)) = newest else {
        return;
    };
    let frame = {
        let mut st = state.borrow_mut();
        let st = &mut *st;
        match &st.layout {
            Some(layout) if !st.paused && !st.over => {
                // SAFETY: dequeued, and queued back only once the frame is let go (or
                // when PipeWire removes it).
                let frame = unsafe { st.slots.frame(b, layout, crop) };
                if let Some(f) = &frame {
                    st.pictures.push(f.clone(), Instant::now());
                }
                frame
            }
            _ => None,
        }
    };
    if frame.is_none() {
        // SAFETY: dequeued above, not handed to anyone.
        unsafe { stream.queue_raw_buffer(b) };
    }
}

/// The encoder's pause changed, or a picture is asked again.
fn look(stream: &Stream, state: &Shared, pause: &Pause, node: u32) {
    let on = pause.is_on();
    let fresh = pause.take_fresh();
    let again = pause.take_again();
    if on {
        let mut st = state.borrow_mut();
        if !st.paused {
            st.paused = true;
            st.pictures.rest();
            drop(st);
            let _ = stream.set_active(false);
        }
        return;
    }
    // Resumed: the stream starts over, so KWin sends the picture it has now (what
    // was queued before the pause is flushed by the encoder, see `Encoder::set_paused`).
    // A fresh picture asked of a running stream (a still screen) is the last one
    // again; only with none is the stream started over for it.
    let restart = {
        let mut st = state.borrow_mut();
        let restart = if st.paused {
            true
        } else if (fresh || again) && !st.over && st.pictures.again(Instant::now()) {
            false
        } else {
            fresh
        };
        if restart {
            st.paused = false;
            st.pictures.forget();
            st.leaving = true;
        }
        restart
    };
    if restart {
        let _ = stream.disconnect();
        let connected = connect(stream, state, node);
        state.borrow_mut().leaving = false;
        if let Err(why) = connected {
            failed(state, why);
        }
    }
}

/// The feed's thread: PipeWire's loop, until told to stop.
fn run(setup: Setup, hear: mpsc::Receiver<Say>) {
    let Setup {
        node,
        label,
        appsrc,
        pause,
        every,
        say: mail,
        started,
        seen,
    } = setup;
    let fail = |why: String| {
        let _ = started.send(Err(why));
    };
    let main = match pw::main_loop::MainLoopRc::new(None) {
        Ok(m) => m,
        Err(e) => return fail(format!("no PipeWire loop: {e}")),
    };
    let context = match pw::context::ContextRc::new(&main, None) {
        Ok(c) => c,
        Err(e) => return fail(format!("no PipeWire context: {e}")),
    };
    let core = match context.connect_rc(None) {
        Ok(c) => c,
        Err(e) => return fail(format!("cannot connect to PipeWire: {e}")),
    };
    let props = pw::properties::properties! {
        "node.name" => "prostir",
        "node.description" => label.as_str(),
        // Only KWin's node; gone, it fails at once instead of a webcam taking its
        // place.
        "node.dont-fallback" => "true",
    };
    let stream = match StreamRc::new(core.clone(), "prostir", props) {
        Ok(s) => s,
        Err(e) => return fail(format!("cannot make a PipeWire stream: {e}")),
    };
    let state: Shared = Rc::new(RefCell::new(State {
        label: label.clone(),
        pictures: Pictures::new(appsrc.clone(), every),
        appsrc,
        seen,
        started: Some(started),
        possible: gst::Caps::new_empty(),
        layout: None,
        slots: Slots::new(mail.wake.clone()),
        paused: false,
        leaving: false,
        over: false,
    }));
    let listener = stream
        .add_local_listener_with_user_data(state.clone())
        .state_changed(|_, state, old, new| {
            debug!("{}: stream {old:?} -> {new:?}", state.borrow().label);
            match new {
                StreamState::Error(why) => failed(state, format!("stream error: {why}")),
                StreamState::Unconnected if !state.borrow().leaving => {
                    // KWin ended its stream: nothing more comes (its output says it
                    // closed; `pipewiresrc` said nothing on the bus either).
                    let mut st = state.borrow_mut();
                    st.over = true;
                    st.pictures.forget();
                    if let Some(started) = st.started.take() {
                        let _ = started.send(Err("stream disconnected".to_owned()));
                    }
                }
                _ => {}
            }
        })
        .param_changed(|stream, state, id, param| {
            if id == ss::SPA_PARAM_Format {
                format_chosen(stream, state, param);
            }
        })
        .add_buffer(|_, state, b| {
            // SAFETY: PipeWire hands over a valid buffer.
            unsafe { state.borrow_mut().slots.add(b) };
        })
        .remove_buffer(|stream, state, b| {
            let lent = state.borrow_mut().slots.remove(b);
            if lent {
                // SAFETY: a buffer of this stream, still valid in this call.
                unsafe { stream.queue_raw_buffer(b) };
            }
        })
        .process(|stream, state| frames_came(stream, state))
        .register();
    let listener = match listener {
        Ok(l) => l,
        Err(e) => return failed(&state, format!("cannot listen to the stream: {e}")),
    };
    let _core_errors = {
        let state = state.clone();
        core.add_listener_local()
            .error(move |id, _, res, message| {
                if id == pw::sys::PW_ID_CORE {
                    failed(&state, format!("PipeWire failed ({res}): {message}"));
                }
            })
            .register()
    };
    let quit = Rc::new(std::cell::Cell::new(false));
    let _heard = {
        let (stream, state, quit) = (stream.clone(), state.clone(), quit.clone());
        let wake = mail.wake.as_raw_fd();
        main.loop_()
            .add_io(wake, pw::spa::support::system::IoFlags::IN, move |_| {
                mail.woken();
                while let Ok(said) = hear.try_recv() {
                    match said {
                        Say::Look => look(&stream, &state, &pause, node),
                        Say::Stop => quit.set(true),
                    }
                }
                // Frames GStreamer let go: their buffers go back to KWin.
                state.borrow_mut().slots.take_back(|b| {
                    // SAFETY: a buffer of this stream that PipeWire has not removed
                    // (removing it takes it out of the slots), lent out until now;
                    // queueing it calls nothing back.
                    unsafe { stream.queue_raw_buffer(b) };
                });
            })
    };
    if let Err(why) = connect(&stream, &state, node) {
        failed(&state, why);
    }
    while !quit.get() {
        let wait = state.borrow().pictures.wait(Instant::now());
        main.loop_()
            .iterate(wait.map_or(pw::loop_::Timeout::Infinite, |w| {
                // At least a millisecond: the loop counts in them.
                pw::loop_::Timeout::Finite(w.max(Duration::from_millis(1)))
            }));
        let mut st = state.borrow_mut();
        if !st.paused && !st.over {
            st.pictures.tick(Instant::now());
        }
    }
    {
        let mut st = state.borrow_mut();
        st.leaving = true;
        st.pictures.forget();
    }
    let _ = stream.disconnect();
    drop(listener);
    debug!("{label}: stream closed");
}

#[cfg(test)]
mod tests;
