//! KDE Plasma: KWin makes and streams the outputs, KScreen places them, the Plasma
//! shell keeps its panel and wallpapers.

pub mod access;
pub mod feed;
pub mod kscreen;
pub mod plasma;
pub mod virtual_output;
pub mod windows;

use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use log::info;

use crate::encoder::Encoder;
use crate::platform::{
    AddModel, Cursor, CursorSink, CursorSource, DeskPlan, Desktop, DesktopCaps, FrameSource,
    GpuDevices, Memory, Origin, Output, OutputHealth, OutputKey, Outputs, PhysicalDisplay, Pointer,
    Scales, VirtualSpec, Windows,
};
use feed::Feed;
use feed::pointer::{CursorTap, SinkSlot};
use virtual_output::{Screencast, VirtualOutput};
use windows::KdeWindows;

/// The size range KWin makes virtual outputs in, in pixels.
const WIDTHS: std::ops::RangeInclusive<u32> = 320..=7680;
const HEIGHTS: std::ops::RangeInclusive<u32> = 240..=4320;

/// After the outputs changed, the primary output is looked at once more this much
/// later, and without a change this often (see [`Kde::keep_primary`]).
const PRIMARY_SETTLE: Duration = Duration::from_secs(2);
const PRIMARY_ANYWAY: Duration = Duration::from_secs(10);

/// KDE Plasma as the desktop.
pub struct Kde {
    caps: DesktopCaps,
    screencast: Screencast,
    windows: KdeWindows,
    /// When Plasma was last nudged to follow the primary output.
    primary_nudged: Option<Instant>,
    /// The primary output was last looked at: at which change of the outputs, when,
    /// and when to look once more after a change.
    primary_looked: (u64, Option<Instant>, Option<Instant>),
    /// Last time the laptop panel was switched back on (to avoid fighting the user).
    woke_display_at: Option<Instant>,
    /// Displays of the PC shown so far, to number their streams in the log.
    shown: u32,
    /// The pointer apart from the pictures: where its streams tell it.
    cursor: KdeCursor,
    /// The graphics devices to share (KWin hands buffers over: none taken yet).
    _gpu: GpuDevices,
}

impl Kde {
    pub fn connect(gpu: GpuDevices) -> Result<Self> {
        Ok(Self {
            caps: DesktopCaps {
                name: "KDE Plasma",
                virtual_monitors: Ok(AddModel::OneByOne),
                has_primary: true,
                origin_is_primary: false,
                scales: Scales::Any { min: 50, max: 400 },
                refresh: vec![60],
                // KDE's screenshot tool cannot take one output by name.
                screenshot_one_output: false,
                // KWin scripts cannot move the mouse.
                moves_pointer: false,
                // KWin removes the outputs of a connection that is gone.
                cleans_up_after_crash: true,
            },
            screencast: Screencast::connect().map_err(access::explain)?,
            windows: KdeWindows::new(),
            primary_nudged: None,
            primary_looked: (0, None, None),
            woke_display_at: None,
            shown: 0,
            cursor: KdeCursor::default(),
            _gpu: gpu,
        })
    }
}

/// KWin tells the pointer apart in the metadata of streams made so (`feed/pointer.rs`):
/// each output's stream with the pointer apart tells the core's sink, kept here.
#[derive(Default)]
struct KdeCursor {
    sink: SinkSlot,
}

impl CursorSource for KdeCursor {
    fn set_apart(&mut self, sink: Option<CursorSink>) {
        *self.sink.lock().unwrap() = sink;
    }

    /// KWin chooses how the pointer goes when it makes a stream: an output's encoder
    /// starts over on the stream that has it so (see [`KdeOutput`]).
    fn restarts_streams(&self) -> bool {
        true
    }
}

impl Desktop for Kde {
    fn caps(&self) -> &DesktopCaps {
        &self.caps
    }

    fn outputs(&self) -> Outputs {
        kscreen::geometries()
    }

    fn physical(&self) -> Vec<PhysicalDisplay> {
        self.screencast.physical_displays()
    }

    fn make(&mut self, spec: &VirtualSpec) -> Result<Box<dyn Output>> {
        if !WIDTHS.contains(&spec.w)
            || !HEIGHTS.contains(&spec.h)
            || !self.caps.scales.allows(spec.scale)
        {
            bail!("monitor {spec} is out of range");
        }
        let output = self.screencast.create(spec)?;
        Ok(Box::new(KdeOutput::new(
            output,
            Origin::Monitor,
            None,
            self.cursor.sink.clone(),
        )))
    }

    fn show(&mut self, key: &OutputKey) -> Result<Box<dyn Output>> {
        // KWin streams a given display only once per connection: each one shown gets a
        // connection of its own, kept as long as the output. Its stream number tells
        // the streams of one connection apart, and the log tells them apart by it.
        let connection = Screencast::connect()?;
        self.shown += 1;
        let output = connection.mirror(self.shown, key.as_str(), false)?;
        Ok(Box::new(KdeOutput::new(
            output,
            Origin::Capture,
            Some(connection),
            self.cursor.sink.clone(),
        )))
    }

    /// Only what differs from `current` is sent, in one call.
    fn place(&mut self, plan: &DeskPlan, current: &Outputs) -> Result<()> {
        let mut settings: Vec<String> = plan
            .positions
            .iter()
            .filter(|(key, x, y)| current.get(key).is_none_or(|g| (g.x, g.y) != (*x, *y)))
            .map(|(key, x, y)| format!("output.{key}.position.{x},{y}"))
            .collect();
        if let Some(p) = &plan.primary
            && current.get(p).is_none_or(|g| g.priority != Some(1))
        {
            settings.push(format!("output.{p}.priority.1"));
        }
        kscreen::apply(&settings)
    }

    /// Makes sure KDE's primary output (where the Plasma panel lives) is `ours`. Right
    /// after outputs come and go (a profile switch) KDE can end up with another one.
    ///
    /// Asking KScreen takes a process (`kscreen-doctor`, about 75 ms of CPU), so it is
    /// asked only when the outputs changed, once more 2 s later (KDE settles), and
    /// otherwise every 10 s.
    fn keep_primary(&mut self, ours: &OutputKey) {
        let now = Instant::now();
        let changed = self.screencast.outputs_changed();
        let (seen, last, again) = self.primary_looked;
        let due = changed != seen
            || again.is_some_and(|t| now >= t)
            || last.is_none_or(|t| now.duration_since(t) >= PRIMARY_ANYWAY);
        if !due {
            return;
        }
        let again = (changed != seen).then(|| now + PRIMARY_SETTLE);
        self.primary_looked = (changed, Some(now), again);
        let geometries = kscreen::geometries();
        let kde_primary = geometries
            .iter()
            .filter(|(_, g)| g.enabled && g.w > 0)
            .min_by_key(|(_, g)| g.priority.unwrap_or(u32::MAX))
            .map(|(key, _)| key.clone());
        let Some(g) = geometries.get(ours) else {
            return;
        };
        if kde_primary.as_ref() != Some(ours) {
            info!(
                "KDE's primary output is {}, not {ours}: setting it again",
                kde_primary.as_ref().map_or("none", OutputKey::as_str)
            );
            kscreen::set(ours.as_str(), "priority.1");
            return;
        }
        // KDE agrees; Plasma may still keep the panel on the former primary.
        let Some(first) = plasma::first_screen() else {
            return;
        };
        if first == (g.x, g.y, g.w, g.h)
            || self
                .primary_nudged
                .is_some_and(|t| t.elapsed() < Duration::from_secs(10))
        {
            return;
        }
        let other = geometries
            .iter()
            .find(|(key, g)| g.enabled && g.w > 0 && *key != ours)
            .map(|(key, _)| key.to_string());
        if let Some(other) = other {
            info!("Plasma's first screen is not {ours}: nudging it to follow");
            self.primary_nudged = Some(Instant::now());
            plasma::nudge_primary(ours.to_string(), other);
        }
    }

    /// KDE sometimes leaves the laptop panel off after the lid is opened again while
    /// virtual monitors exist. With the lid open, switch it on (for the mirror).
    fn wake_physical(&mut self) {
        if self
            .woke_display_at
            .is_some_and(|t| t.elapsed() < Duration::from_secs(30))
            || !crate::platform::linux::system::lid_open()
        {
            return;
        }
        let off = kscreen::disabled_physical_outputs();
        if off.is_empty() {
            return;
        }
        self.woke_display_at = Some(Instant::now());
        info!("lid is open but {off:?} is off: switching it on for the mirror");
        let settings: Vec<String> = off.iter().map(|n| format!("output.{n}.enable")).collect();
        if let Err(e) = kscreen::apply(&settings) {
            log::warn!("{e:#}");
        }
    }

    fn sync_wallpapers(&mut self) {
        plasma::sync_wallpapers();
    }

    fn windows(&mut self) -> Option<&mut dyn Windows> {
        Some(&mut self.windows)
    }

    fn pointer(&mut self) -> Option<&mut dyn Pointer> {
        Some(&mut self.windows)
    }

    fn cursor(&mut self) -> Option<&mut dyn CursorSource> {
        Some(&mut self.cursor)
    }
}

/// A KWin output: a virtual one, or a display of the PC streamed as it is.
///
/// Its stream was made with the pointer drawn in. While the pointer goes apart (and it
/// is no window panel), a second stream of it, made with the pointer apart on a
/// connection of its own (KWin streams an output once per connection), feeds the
/// encoder instead and tells the pointer; it goes once the pointer is drawn in again.
struct KdeOutput {
    key: OutputKey,
    origin: Origin,
    // Field order matters: the feed stops before KWin's streams close, and a stream
    // before its own KWin connection.
    /// KWin's frames going into the current encoder.
    feed: Option<Feed>,
    /// The stream with the pointer apart, and its connection.
    apart: Option<(VirtualOutput, Screencast)>,
    stream: VirtualOutput,
    /// The connection of a display of the PC shown (see [`Kde::show`]).
    _connection: Option<Screencast>,
    /// The next encoder is fed with the pointer apart.
    wants_apart: bool,
    sink: SinkSlot,
}

impl KdeOutput {
    fn new(
        stream: VirtualOutput,
        origin: Origin,
        connection: Option<Screencast>,
        sink: SinkSlot,
    ) -> Self {
        Self {
            key: OutputKey::new(stream.output_name()),
            origin,
            feed: None,
            apart: None,
            stream,
            _connection: connection,
            wants_apart: false,
            sink,
        }
    }

    /// The stream with the pointer apart, made if it is not yet.
    fn apart_stream(&mut self) -> Result<&VirtualOutput> {
        if self.apart.is_none() {
            let connection = Screencast::connect()?;
            let stream = connection.mirror(1, self.stream.output_name(), true)?;
            info!(
                "{}: a stream with the pointer apart, PipeWire node {}",
                self.key, stream.node
            );
            self.apart = Some((stream, connection));
        }
        Ok(&self.apart.as_ref().expect("made above").0)
    }
}

impl Output for KdeOutput {
    fn key(&self) -> &OutputKey {
        &self.key
    }

    /// Closed also when the stream with the pointer apart (feeding the encoder now)
    /// was.
    fn health(&self) -> OutputHealth {
        if self.stream.closed() || self.apart.as_ref().is_some_and(|(s, _)| s.closed()) {
            OutputHealth::Closed
        } else if let Some((w, h)) = self.stream.resized() {
            OutputHealth::Resized { w, h }
        } else {
            OutputHealth::Live
        }
    }

    fn source(&self) -> FrameSource {
        FrameSource {
            input: self.stream.input(),
            origin: self.origin,
            // PipeWire hands GPU buffers to an encoder that takes them.
            memory: Memory::DmaBuf,
            cursor: if self.wants_apart {
                Cursor::Separate
            } else {
                Cursor::InFrame
            },
        }
    }

    /// KWin's stream is read from PipeWire into the encoder (see [`feed`]); a stream
    /// that cannot start fails as the source.
    fn attach(&mut self, encoder: &Encoder) -> Result<()> {
        self.feed = None;
        let appsrc = encoder
            .appsrc()
            .ok_or_else(|| anyhow::anyhow!("the encoder takes no pushed frames"))?;
        if !self.wants_apart {
            self.apart = None;
            self.feed = Some(self.stream.feed(appsrc, encoder.paused(), None)?);
            return Ok(());
        }
        let tap = CursorTap::new(self.key.clone(), self.sink.clone());
        let feed = self
            .apart_stream()
            .and_then(|s| s.feed(appsrc.clone(), encoder.paused(), Some(tap)));
        self.feed = Some(match feed {
            Ok(feed) => feed,
            // Better the pointer in the picture than no picture.
            Err(e) => {
                log::warn!("{}: no stream with the pointer apart: {e:#}", self.key);
                self.apart = None;
                self.stream.feed(appsrc, encoder.paused(), None)?
            }
        });
        Ok(())
    }

    fn set_cursor_apart(&mut self, apart: bool) {
        self.wants_apart = apart;
    }

    fn detach(&mut self) {
        self.feed = None;
    }

    fn make_primary(&self) {
        self.stream.make_primary();
    }

    fn desk_scale(&self) -> Option<u16> {
        self.stream.desk_scale()
    }
}
