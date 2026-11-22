//! The picture of a fake monitor and the GStreamer source the encoder reads it from
//! (`prostirfakescreen screen=NAME`). The source behaves like KWin's outputs streamed
//! through PipeWire, which is what the core has to cope with:
//!
//! - it is live, so encoders work as they do on a real desktop (VA-API keeps frames
//!   back when the source is not live);
//! - started (or started over), it sends the current picture at once;
//! - after that only a changed picture, so an encoder that is paused and resumed
//!   without starting over gets nothing on a still screen;
//! - a change while nobody reads is not queued: the next read gets the newest picture;
//! - with `PROSTIR_FAKE_KEEPALIVE_MS`, the picture sent last is sent again when nothing
//!   changed for that long, in the same memory, as PipeWire's source does with its
//!   `keepalive-time` (without it, never: a test sees a still screen send nothing).
//!
//! Every start is logged (`fake screen NAME: started`), so a test sees how often the
//! core started a stream over.
//!
//! Like a KWin virtual output, a screen can be resized by another program's stream of
//! it ([`Screen::resize`]); the core's own stream keeps the size it was made with, and
//! starting that stream over puts the size back, unless the resize is sticky.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, LazyLock, Mutex};

use anyhow::Result;
use gstreamer as gst;
use gstreamer::glib;
use gstreamer::prelude::*;
use gstreamer::subclass::prelude::*;
use gstreamer_base as gst_base;
use gstreamer_base::prelude::*;
use gstreamer_base::subclass::prelude::*;
use gstreamer_video as gst_video;

/// The GStreamer element name of the source.
pub const ELEMENT: &str = "prostirfakescreen";

/// How long a still screen waits to send its picture again (`PROSTIR_FAKE_KEEPALIVE_MS`).
fn keepalive() -> Option<std::time::Duration> {
    static EVERY: LazyLock<Option<std::time::Duration>> = LazyLock::new(|| {
        std::env::var("PROSTIR_FAKE_KEEPALIVE_MS")
            .ok()?
            .parse()
            .ok()
            .filter(|ms| *ms > 0)
            .map(std::time::Duration::from_millis)
    });
    *EVERY
}

/// Screens by output name, for the sources to find theirs.
static SCREENS: LazyLock<Mutex<HashMap<String, Arc<Screen>>>> = LazyLock::new(Default::default);

/// How many of the next starts of each screen's source fail, by output name, as when
/// KWin's PipeWire node is gone (`fail-source Fake-3 N`, see `desktop.rs`).
pub static FAIL_STARTS: LazyLock<Mutex<HashMap<String, u32>>> = LazyLock::new(Default::default);

/// What a fake monitor shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Picture {
    /// Grey bands that move with every change: small frames, like a quiet desktop.
    Bands,
    /// Random pixels, a band of them new with every change (a busy screen: video
    /// playing, windows moving). An encoder can hardly compress them, so keyframes
    /// are as big as the bitrate lets them be, and other frames a fraction of that.
    Noise,
}

/// One fake monitor's picture, changing as [`Picture`] says.
pub struct Screen {
    name: String,
    info: gst_video::VideoInfo,
    picture: Picture,
    /// How many times the picture changed.
    changes: Mutex<u64>,
    changed: Condvar,
    /// The size another program gave the output, and whether it stays when the stream
    /// starts over.
    resized: Mutex<Option<((u32, u32), bool)>>,
    /// The desktop closed its output (a test's `close`).
    closed: std::sync::atomic::AtomicBool,
}

impl Screen {
    /// A screen named `name`, findable by its source until [`Screen::remove`].
    pub fn add(name: &str, width: u32, height: u32, picture: Picture) -> Result<Arc<Self>> {
        register()?;
        let info = gst_video::VideoInfo::builder(gst_video::VideoFormat::I420, width, height)
            // Frames come when they come, but a nominal rate: with KWin's "variable"
            // (0/1) x264 holds each frame back until the next one arrives, which
            // KWin's keepalive frames hide and a still fake screen would not.
            .fps(gst::Fraction::new(30, 1))
            .build()?;
        let screen = Arc::new(Self {
            name: name.to_owned(),
            info,
            picture,
            changes: Mutex::new(0),
            changed: Condvar::new(),
            resized: Mutex::new(None),
            closed: std::sync::atomic::AtomicBool::new(false),
        });
        SCREENS
            .lock()
            .unwrap()
            .insert(name.to_owned(), screen.clone());
        Ok(screen)
    }

    /// No source finds it any more.
    pub fn remove(self: &Arc<Self>) {
        let mut screens = SCREENS.lock().unwrap();
        if screens
            .get(&self.name)
            .is_some_and(|s| Arc::ptr_eq(s, self))
        {
            screens.remove(&self.name);
        }
    }

    /// The screen of output `name`, while it has one.
    pub fn find(name: &str) -> Option<Arc<Self>> {
        SCREENS.lock().unwrap().get(name).cloned()
    }

    /// The pixel size it was made with.
    pub fn size(&self) -> (u32, u32) {
        (self.info.width(), self.info.height())
    }

    /// Another program's stream makes the output `size`. Starting the stream over puts
    /// the made size back, unless `sticky`.
    pub fn resize(&self, size: (u32, u32), sticky: bool) {
        *self.resized.lock().unwrap() = (size != self.size()).then_some((size, sticky));
    }

    /// The desktop closes the output, as KWin may (a crash of its own, a display
    /// configuration that lost it): it stays closed.
    pub fn close(&self) {
        self.closed
            .store(true, std::sync::atomic::Ordering::Relaxed);
        // Its source, waiting for a picture, hears it now.
        let _changes = self.changes.lock().unwrap();
        self.changed.notify_all();
    }

    /// Whether the desktop closed it.
    pub fn is_closed(&self) -> bool {
        self.closed.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The size another program gave the output, while it has it.
    pub fn resized(&self) -> Option<(u32, u32)> {
        self.resized.lock().unwrap().map(|(size, _)| size)
    }

    /// Something on the screen moved.
    pub fn change(&self) {
        *self.changes.lock().unwrap() += 1;
        self.changed.notify_all();
    }

    /// The picture after `changes` changes, in I420 as GStreamer lays it out.
    fn picture(&self, changes: u64) -> gst::Buffer {
        let (w, h) = (self.info.width() as usize, self.info.height() as usize);
        let stride = self.info.stride()[0] as usize;
        let mut data = vec![128u8; self.info.size()];
        let luma = &mut data[..stride * h];
        match self.picture {
            Picture::Bands => {
                for (y, row) in luma.chunks_exact_mut(stride).enumerate() {
                    for (x, px) in row[..w].iter_mut().enumerate() {
                        *px = ((x + y) / 8 + changes as usize * 4) as u8;
                    }
                }
            }
            Picture::Noise => {
                // The same noise everywhere, but an eighth of the rows, moving down
                // with every change, new: a whole new picture each time would make
                // every frame a keyframe (the encoder sees a scene cut).
                noise(luma, 0);
                let band = h.div_ceil(8);
                let top = (changes as usize % 8) * band;
                let rows = top..(top + band).min(h);
                noise(
                    &mut luma[rows.start * stride..rows.end * stride],
                    changes + 1,
                );
            }
        }
        gst::Buffer::from_mut_slice(data)
    }
}

/// Fills `bytes` with random ones, the same for the same `seed` (xorshift64).
fn noise(bytes: &mut [u8], seed: u64) {
    let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    for chunk in bytes.chunks_mut(8) {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        chunk.copy_from_slice(&x.to_le_bytes()[..chunk.len()]);
    }
}

/// Registers the source with GStreamer, once.
fn register() -> Result<()> {
    static DONE: Mutex<bool> = Mutex::new(false);
    let mut done = DONE.lock().unwrap();
    if !*done {
        gst::Element::register(None, ELEMENT, gst::Rank::NONE, ScreenSrc::static_type())?;
        *done = true;
    }
    Ok(())
}

glib::wrapper! {
    /// The source element for a [`Screen`].
    pub struct ScreenSrc(ObjectSubclass<imp::ScreenSrc>)
        @extends gst_base::PushSrc, gst_base::BaseSrc, gst::Element, gst::Object;
}

mod imp {
    use super::*;

    #[derive(Default)]
    struct State {
        screen: Option<Arc<Screen>>,
        /// The picture sent last (its number of changes); `None` right after a start.
        sent: Option<u64>,
        /// Asked to stop waiting (stopping or pausing).
        flushing: bool,
        /// The picture sent last, held as PipeWire's source holds it: a new picture
        /// never comes in its memory, so the encoder tells a new one from the same
        /// one sent again.
        last: Option<gst::Buffer>,
    }

    #[derive(Default)]
    pub struct ScreenSrc {
        /// Which screen, by output name (the `screen` property).
        name: Mutex<String>,
        state: Mutex<State>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ScreenSrc {
        const NAME: &'static str = "ProstirFakeScreen";
        type Type = super::ScreenSrc;
        type ParentType = gst_base::PushSrc;
    }

    impl ObjectImpl for ScreenSrc {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPS: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
                vec![
                    glib::ParamSpecString::builder("screen")
                        .nick("Screen")
                        .blurb("Name of the fake output to stream")
                        .build(),
                ]
            });
            PROPS.as_ref()
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            if pspec.name() == "screen" {
                *self.name.lock().unwrap() = value
                    .get::<Option<String>>()
                    .ok()
                    .flatten()
                    .unwrap_or_default();
            }
        }

        fn property(&self, _id: usize, _pspec: &glib::ParamSpec) -> glib::Value {
            self.name.lock().unwrap().to_value()
        }

        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_live(true);
            obj.set_format(gst::Format::Time);
            obj.set_do_timestamp(true);
        }
    }

    impl GstObjectImpl for ScreenSrc {}

    impl ElementImpl for ScreenSrc {
        fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
            static META: LazyLock<gst::subclass::ElementMetadata> = LazyLock::new(|| {
                gst::subclass::ElementMetadata::new(
                    "Prostir fake screen",
                    "Source/Video",
                    "A fake monitor's picture, sent when it changes (tests only)",
                    "Prostir contributors",
                )
            });
            Some(&*META)
        }

        fn pad_templates() -> &'static [gst::PadTemplate] {
            static PADS: LazyLock<Vec<gst::PadTemplate>> = LazyLock::new(|| {
                let caps = gst_video::VideoCapsBuilder::new()
                    .format(gst_video::VideoFormat::I420)
                    .build();
                vec![
                    gst::PadTemplate::new(
                        "src",
                        gst::PadDirection::Src,
                        gst::PadPresence::Always,
                        &caps,
                    )
                    .expect("a valid pad template"),
                ]
            });
            PADS.as_ref()
        }
    }

    impl BaseSrcImpl for ScreenSrc {
        fn start(&self) -> Result<(), gst::ErrorMessage> {
            let name = self.name.lock().unwrap().clone();
            let fails = FAIL_STARTS
                .lock()
                .unwrap()
                .get_mut(&name)
                .is_some_and(|left| {
                    let fails = *left > 0;
                    *left = left.saturating_sub(1);
                    fails
                });
            if fails {
                log::info!("fake screen {name}: its stream is gone (fail-source)");
                return Err(gst::error_msg!(
                    gst::ResourceError::NotFound,
                    ["the stream of {name} is gone"]
                ));
            }
            let screen = SCREENS.lock().unwrap().get(&name).cloned().ok_or_else(|| {
                gst::error_msg!(gst::ResourceError::NotFound, ["no fake screen {name}"])
            })?;
            log::info!("fake screen {name}: started");
            // As KWin does when the stream that made the output connects again.
            let mut resized = screen.resized.lock().unwrap();
            if resized.is_some_and(|(_, sticky)| !sticky) {
                *resized = None;
                let (w, h) = screen.size();
                log::info!("fake screen {name}: back to {w}x{h}");
            }
            drop(resized);
            *self.state.lock().unwrap() = State {
                screen: Some(screen),
                sent: None,
                flushing: false,
                last: None,
            };
            Ok(())
        }

        fn stop(&self) -> Result<(), gst::ErrorMessage> {
            *self.state.lock().unwrap() = State::default();
            Ok(())
        }

        fn caps(&self, filter: Option<&gst::Caps>) -> Option<gst::Caps> {
            let name = self.name.lock().unwrap().clone();
            let screen = SCREENS.lock().unwrap().get(&name).cloned()?;
            let caps = screen.info.to_caps().ok()?;
            Some(match filter {
                Some(f) => f.intersect_with_mode(&caps, gst::CapsIntersectMode::First),
                None => caps,
            })
        }

        fn is_seekable(&self) -> bool {
            false
        }

        fn unlock(&self) -> Result<(), gst::ErrorMessage> {
            // `create` takes the screen's lock and then the state's; the state's is let
            // go here before the screen's is taken, so neither waits on the other.
            let screen = {
                let mut state = self.state.lock().unwrap();
                state.flushing = true;
                state.screen.clone()
            };
            if let Some(screen) = screen {
                // Under the screen's lock, `create` is either before its look at
                // `flushing` or already waiting: it cannot miss this wake-up.
                let _changes = screen.changes.lock().unwrap();
                screen.changed.notify_all();
            }
            Ok(())
        }

        fn unlock_stop(&self) -> Result<(), gst::ErrorMessage> {
            self.state.lock().unwrap().flushing = false;
            Ok(())
        }
    }

    impl PushSrcImpl for ScreenSrc {
        /// Waits for a picture the encoder has not had yet.
        fn create(
            &self,
            _buffer: Option<&mut gst::BufferRef>,
        ) -> Result<gst_base::subclass::base_src::CreateSuccess, gst::FlowError> {
            let screen = self
                .state
                .lock()
                .unwrap()
                .screen
                .clone()
                .ok_or(gst::FlowError::Flushing)?;
            let mut changes = screen.changes.lock().unwrap();
            loop {
                // Closed by the desktop: its stream is gone, as KWin's PipeWire node.
                if screen.is_closed() {
                    gst::element_imp_error!(
                        self,
                        gst::ResourceError::NotFound,
                        ["the output {} is closed", screen.name]
                    );
                    return Err(gst::FlowError::Error);
                }
                {
                    let mut state = self.state.lock().unwrap();
                    if state.flushing {
                        return Err(gst::FlowError::Flushing);
                    }
                    if state.sent != Some(*changes) {
                        state.sent = Some(*changes);
                        let now = *changes;
                        drop(state);
                        drop(changes);
                        let picture = screen.picture(now);
                        self.state.lock().unwrap().last = Some(picture.clone());
                        return Ok(gst_base::subclass::base_src::CreateSuccess::NewBuffer(
                            picture,
                        ));
                    }
                }
                // Woken by a change or by `unlock`, or the picture goes again.
                let Some(every) = keepalive() else {
                    changes = screen.changed.wait(changes).unwrap();
                    continue;
                };
                let (woken, waited) = screen.changed.wait_timeout(changes, every).unwrap();
                changes = woken;
                if waited.timed_out() {
                    let state = self.state.lock().unwrap();
                    if let (false, Some(last)) = (state.flushing, state.last.clone()) {
                        return Ok(gst_base::subclass::base_src::CreateSuccess::NewBuffer(last));
                    }
                }
            }
        }
    }
}
