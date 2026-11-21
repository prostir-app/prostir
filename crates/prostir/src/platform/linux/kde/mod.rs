//! KDE Plasma: KWin makes and streams the outputs, KScreen places them, the Plasma
//! shell keeps its panel and wallpapers.

pub mod kscreen;
pub mod plasma;
pub mod virtual_output;
pub mod windows;

use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use log::info;

use crate::encoder::Encoder;
use crate::platform::{
    AddModel, Cursor, DeskPlan, Desktop, DesktopCaps, FrameSource, GpuDevices, Memory, Origin,
    Output, OutputHealth, OutputKey, Outputs, PhysicalDisplay, Pointer, Scales, VirtualSpec,
    Windows,
};
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
            screencast: Screencast::connect()?,
            windows: KdeWindows::new(),
            primary_nudged: None,
            primary_looked: (0, None, None),
            woke_display_at: None,
            shown: 0,
            _gpu: gpu,
        })
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
        Ok(Box::new(KdeOutput::new(output, Origin::Monitor, None)))
    }

    fn show(&mut self, key: &OutputKey) -> Result<Box<dyn Output>> {
        // KWin streams a given display only once per connection: each one shown gets a
        // connection of its own, kept as long as the output. Its stream number tells
        // the streams of one connection apart, and the log tells them apart by it.
        let connection = Screencast::connect()?;
        self.shown += 1;
        let output = connection.mirror(self.shown, key.as_str())?;
        Ok(Box::new(KdeOutput::new(
            output,
            Origin::Capture,
            Some(connection),
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
}

/// A KWin output: a virtual one, or a display of the PC streamed as it is.
struct KdeOutput {
    key: OutputKey,
    origin: Origin,
    // Field order matters: the stream closes before its own KWin connection.
    stream: VirtualOutput,
    /// The connection of a display of the PC shown (see [`Kde::show`]).
    _connection: Option<Screencast>,
}

impl KdeOutput {
    fn new(stream: VirtualOutput, origin: Origin, connection: Option<Screencast>) -> Self {
        Self {
            key: OutputKey::new(stream.output_name()),
            origin,
            stream,
            _connection: connection,
        }
    }
}

impl Output for KdeOutput {
    fn key(&self) -> &OutputKey {
        &self.key
    }

    fn health(&self) -> OutputHealth {
        if self.stream.closed() {
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
            cursor: Cursor::InFrame,
        }
    }

    /// KWin feeds the encoder through PipeWire by itself.
    fn attach(&mut self, _encoder: &Encoder) -> Result<()> {
        Ok(())
    }

    fn detach(&mut self) {}

    fn make_primary(&self) {
        self.stream.make_primary();
    }

    fn desk_scale(&self) -> Option<u16> {
        self.stream.desk_scale()
    }
}
