//! The platform contract: what the core asks of a desktop, of the outputs it streams,
//! of its windows and of its mouse. A desktop is picked once, when a session starts
//! (`platform::connect`), and the core holds it as a `Box<dyn Desktop>`: it never asks
//! which desktop it is, and never sees a desktop's own names for its outputs.
//!
//! What differs between desktops and operating systems is said as data where one
//! answer fits all (the capabilities in [`DesktopCaps`], the kind of memory frames live
//! in, the state of an output), not as more traits.
//!
//! Positions and sizes crossing the contract are in logical pixels (an output's pixels
//! divided by its scale), whatever the desktop works in: one in pixels converts in
//! [`Desktop::outputs`], [`Desktop::place`], [`Windows::active`] and
//! [`Pointer::position`]. A window panel's pixel size is its logical size times its
//! scale (`layout::panel_size`), on every desktop.
//!
//! # What is not in the contract
//!
//! Services of the operating system that do not depend on the desktop, and run from
//! the start of the program to its end, are plain functions with one implementation
//! per build (see `platform/mod.rs`), started once by the core: the tray, quitting on a
//! signal, and `keep_adb_reverse`, which keeps `adb reverse` set up for a headset on
//! USB. They have no trait because no build has two of them; another operating system
//! (or the fake platform) gives the same function, doing nothing where there is
//! nothing to do. Each sleeps in the kernel until something happens (a udev event, a
//! signal, a click), so an idle PC is not woken by them. A desktop, in turn, exists
//! only while a session needs it.
//!
//! # What Windows does differently, and where the contract says it
//!
//! From the Windows prototype (`plans/windows-prototype.md`, sections 6 and 10). The
//! fields marked for Windows are provisional until its desktop is written (step 11c);
//! on Linux they hold the plain answer. The test `windows_shape` below builds what a
//! Windows desktop would report.
//!
//! 1. The display driver may be missing: [`DesktopCaps::virtual_monitors`] is an
//!    [`Unavailable`] saying why; the PC's own displays work all the same, and adding
//!    a monitor answers with its [`Unavailable::error`].
//! 2. Adding a monitor may restart all of them (an unchanged Virtual Display Driver):
//!    [`AddModel::RestartsAll`], so their streams breaking together is no failure of
//!    each. Our own driver, modelled on SudoVDA, is [`AddModel::OneByOne`]; the
//!    unchanged Virtual Display Driver is ruled out, even as a fallback.
//! 3. Refresh rates: [`VirtualSpec::refresh`], one of [`DesktopCaps::refresh`]
//!    (Linux: 60 only).
//! 4. Scales in steps, through an undocumented call: [`VirtualSpec::scale`] in percent,
//!    [`Scales::Steps`], which may depend on the mode ([`Desktop::scales_for`]); the
//!    scales offered to the headset are those the desktop gives every size offered,
//!    and the desktop checks its own. Where it changes a monitor's scale in place
//!    ([`DesktopCaps::scales_in_place`], [`DeskPlan::scales`]) the output stays.
//! 5. Physical pixels, the primary output always at (0, 0): the desktop converts to and
//!    from logical pixels itself (see above), and with
//!    [`DesktopCaps::origin_is_primary`] the core shifts its whole plan so the primary
//!    output is at (0, 0) before [`Desktop::place`], and tells the headset the shifted
//!    places.
//! 6. Output names that change (`\\.\DISPLAYn`, `HMONITOR`, the adapter's LUID):
//!    [`OutputKey`], which the desktop makes and keeps (the serial number our driver
//!    writes from [`VirtualSpec::id`], the device path or EDID of a real display).
//!    A display of the PC is shown to people by its [`PhysicalDisplay::label`].
//!    Whether an output is virtual is the desktop's to tell, in
//!    [`Geometry::is_virtual`], by the LUID of our driver's adapter or the hardware id
//!    of another's (both drivers say HDMI); whether a display is a laptop's own panel,
//!    in [`PhysicalDisplay::builtin`].
//! 7. An output out for a while (the lock screen, a UAC prompt, a mode change; Desktop
//!    Duplication comes back by itself after up to 2.3 s): [`OutputHealth::Paused`],
//!    neither started over nor laid out again.
//! 8. The graphics card is part of an output: [`FrameSource::memory`]
//!    ([`Memory::D3D11`], whose adapter [`FrameSource::adapter`] reads), for an encoder
//!    picked per output on the same card (`encoder/choice.rs`: the card's own
//!    encoder, then Media Foundation on it, then one that needs a copy, x264 last). One
//!    device per card is shared by every capture and encoder ([`GpuDevices`], given to
//!    the desktop when it connects): neither an output nor an encoder makes its own.
//! 9. A limit on encoder sessions: learned when an encoder does not open while others
//!    of it run, and kept with the probes in the cache (`encoder/probe.rs`); not a
//!    field here. The output goes on with the next encoder.
//! 10. The cursor in the frames or apart: [`FrameSource::cursor`]; told apart to the
//!     headsets (its look and place, the headset drawing it) through
//!     [`Desktop::cursor`], as on Linux.
//! 11. Sound also in the speakers, and no microphone without VB-CABLE: the sound part
//!     of the platform (`sound.rs`: `Route`, `MicAvailability`, an explicit
//!     `OutputDevice` to capture from), not the desktop.
//! 12. No signals: the system module (`on_quit_request`, step 6.8). Cleaning up after a
//!     crash is the desktop's when it connects, and nothing at all where
//!     [`DesktopCaps::cleans_up_after_crash`] (our driver's watchdog).
//! 13. No desktop notifications without a package identity: `notifications::watch`
//!     gives none (already so).
//! 14. Calls that can hang (`SetDisplayConfig`, `DeviceIoControl`): every method here
//!     may block. Only the desktop's worker (`worker/`) calls them, so the control loop
//!     never waits for one; one that hangs shows as the worker stuck (the PC says the
//!     desktop does not respond after 15 s, and starts again after 2 min). Programs
//!     run with a time limit (`process::run`); the contract asks nothing more.
//! 15. `broadcast_addresses` and virtual network adapters: gone with the beacon (step
//!     7.9); the PC answers whoever asked. `local_addresses` (shown where a person
//!     pairs, so they can type one into the headset) leaves out the adapters of
//!     containers and virtual machines; not the desktop's.
//!
//! Also from the prototype:
//! - Our monitors' frames come straight from the driver's swap chain, real displays
//!   through Desktop Duplication: [`Origin::Monitor`] beside [`Origin::Capture`].
//! - Windows moves monitors by itself after each one comes or goes (it closes gaps and
//!   brings back the places it saved): [`Desktop::make`] and the drop of an [`Output`]
//!   put Prostir's layout back before they return. A real display can also lose its
//!   mode when all virtual ones go at once: the drop of an [`Output`] puts the modes of
//!   the real displays back.
//!
//! # What GNOME does differently
//!
//! - Virtual monitors through Mutter's `ScreenCast.RecordVirtual`, streamed over
//!   PipeWire like KWin's: [`Origin::Monitor`], [`Memory::DmaBuf`]; they go with the
//!   D-Bus connection ([`DesktopCaps::cleans_up_after_crash`]).
//! - Placement through `DisplayConfig.ApplyMonitorsConfig`, whose layout mode may be
//!   physical (converted by the desktop) and whose scales are those each mode supports
//!   ([`Desktop::scales_for`]); it has a primary monitor ([`DesktopCaps::has_primary`]).
//! - No way to move windows or find the mouse without a Shell extension:
//!   [`Desktop::windows`] and [`Desktop::pointer`] give none, so there are no window
//!   panels. The mouse is moved and clicked through the RemoteDesktop portal, as on
//!   KDE ([`Desktop::input`]).
//! - Without [`Windows`] there is no screenshot tool either: with
//!   [`DesktopCaps::screenshot_one_output`] false, a monitor's picture comes from its
//!   own stream.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;

use prostir_proto::state::DeskRect;

use crate::encoder::{Encoder, Input};
use crate::errors::UserError;
use crate::screen::ScreenId;

/// A desktop: KDE Plasma, Hyprland or the fake one of the tests.
pub trait Desktop: Send {
    /// What it can do, found when it was connected.
    fn caps(&self) -> &DesktopCaps;

    /// Every output on the desktop now, the PC's own displays and the virtual ones
    /// (ours and other programs'), by key. Empty when the desktop does not answer.
    fn outputs(&self) -> Outputs;

    /// The displays of the PC itself connected and on now (the panel of a closed laptop
    /// is not); [`PhysicalDisplay::default`] marks the one to show in the headset when
    /// nothing else was chosen.
    fn physical(&self) -> Vec<PhysicalDisplay>;

    /// The scales it gives an output of `w`x`h` pixels, in percent: where scales
    /// depend on the mode (Windows, GNOME), those that mode supports; else
    /// [`DesktopCaps::scales`].
    fn scales_for(&self, _w: u32, _h: u32) -> Scales {
        self.caps().scales.clone()
    }

    /// A new virtual monitor. Dropping the output removes it from the desktop.
    fn make(&mut self, spec: &VirtualSpec) -> Result<Box<dyn Output>>;

    /// Streams display `key` of the PC as it is. Dropping the output stops the stream;
    /// the display stays.
    fn show(&mut self, key: &OutputKey) -> Result<Box<dyn Output>>;

    /// Moves outputs where `plan` says, gives the monitors of [`DeskPlan::scales`]
    /// their new scale in the same call (so outputs never cover each other on the
    /// way), and makes its primary the primary output, where the desktop has one.
    /// Only what differs from `current` (as [`Desktop::outputs`] said a moment ago) is
    /// sent. An error when the desktop did not take it.
    fn place(&mut self, plan: &DeskPlan, current: &Outputs) -> Result<()>;

    /// Checks now and then that `primary` is still the primary output, and makes it so
    /// again if the desktop moved it elsewhere.
    fn keep_primary(&mut self, _primary: &OutputKey) {}

    /// No display of the PC is on although one should be (a laptop's panel left off
    /// after its lid was opened): switch it on, where the desktop needs that.
    fn wake_physical(&mut self) {}

    /// Gives new monitors the wallpaper of the others, where the desktop needs that.
    fn sync_wallpapers(&mut self) {}

    /// Its windows; none where the desktop has no window panels.
    fn windows(&mut self) -> Option<&mut dyn Windows>;

    /// Its mouse; none where the mouse is neither found nor moved.
    fn pointer(&mut self) -> Option<&mut dyn Pointer>;

    /// The mouse pointer apart from the frames; none where the desktop draws it into
    /// them and cannot say how it looks and where it is (then it stays in the frames).
    fn cursor(&mut self) -> Option<&mut dyn CursorSource> {
        None
    }

    /// How to open its virtual input (see [`Injector`]); none where it has none
    /// ([`DesktopCaps::injects`] says nothing).
    fn input(&mut self) -> Option<Arc<dyn OpenInput>> {
        None
    }
}

/// The mouse pointer told apart from the frames, so each headset draws it itself
/// (`CursorShape` and `CursorAt` in the protocol): moving it then changes no frame,
/// and it moves in the headset with every frame the headset shows. KWin says it in
/// its streams' metadata, wlroots desktops in a pointer session of
/// `ext-image-copy-capture`, Windows in Desktop Duplication.
pub trait CursorSource: Send {
    /// With a sink: from now on the pointer is left out of the frames of every output
    /// but window panels, and `sink` hears how it looks and where it is, at once (as
    /// they are now) and on every change, from any thread. Without: it is drawn into
    /// the frames again, and nothing more is told.
    ///
    /// A window panel keeps the pointer in its frames all the same: it shows one
    /// window, which the person works in at the PC as much as in the headset, and the
    /// core takes the pointer on a panel for on none of the displays.
    fn set_apart(&mut self, sink: Option<CursorSink>);

    /// Whether a change of [`CursorSource::set_apart`] reaches only streams started
    /// after it (KWin chooses how the pointer goes when it makes a stream): the core
    /// then starts the running ones over. Where it does not, switching costs nothing.
    fn restarts_streams(&self) -> bool {
        false
    }
}

/// What a [`CursorSource`] says of the pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorEvent {
    /// It looks so now, and shows (if it was hidden).
    Shape(CursorImage),
    /// Its point is at `x`, `y` in the pixels of the picture of `output` as the
    /// desktop makes it, `size` of them (turned as its frames are, before any
    /// scaling down for an encoder); outside that picture when it is off it.
    Moved {
        output: OutputKey,
        x: i32,
        y: i32,
        size: (u32, u32),
    },
    /// It is hidden (a video full screen, a game, touch), until a look comes again.
    Hidden,
    /// It left the picture of `output` (for another output's, or none): where it was
    /// on that output no longer counts.
    Left { output: OutputKey },
}

/// A look of the pointer: `w`x`h` pixels of 8-bit RGBA in sRGB, not premultiplied, top
/// row first, pointing at `hot_x`, `hot_y`. In the pixels of the outputs' pictures as
/// the desktop makes them (the core scales it with a picture scaled down to stream).
#[derive(Clone, PartialEq, Eq)]
pub struct CursorImage {
    pub w: u16,
    pub h: u16,
    pub hot_x: u16,
    pub hot_y: u16,
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for CursorImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}x{} pointing at {},{}",
            self.w, self.h, self.hot_x, self.hot_y
        )
    }
}

/// Where a [`CursorSource`] tells what it has to say: the core's, called from any
/// thread, never waiting for anything.
#[derive(Clone)]
pub struct CursorSink(Arc<dyn Fn(CursorEvent) + Send + Sync>);

impl CursorSink {
    pub fn new(tell: impl Fn(CursorEvent) + Send + Sync + 'static) -> Self {
        Self(Arc::new(tell))
    }

    pub fn send(&self, event: CursorEvent) {
        (self.0)(event);
    }
}

impl std::fmt::Debug for CursorSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CursorSink")
    }
}

/// An output streamed to the headset: a virtual monitor, a window panel, or a display of
/// the PC shown as it is.
pub trait Output: Send {
    /// Its stable key (not a name the desktop may give to another output later).
    fn key(&self) -> &OutputKey;

    /// The scale the desktop has it at now, in percent, where the platform knows it
    /// without asking the desktop each time (from the desktop's own events, or a look
    /// that costs next to nothing); none where it does not.
    fn desk_scale(&self) -> Option<u16> {
        None
    }

    /// A scale given to it in the desktop's own settings is the one wanted from now
    /// on: kept where the platform puts its own settings back (Hyprland's monitor
    /// rules, after its config is read again).
    fn scale_taken(&self, _percent: u16) {}

    /// How it is doing now.
    fn health(&self) -> OutputHealth;

    /// Where its frames come from, to build the encoder on.
    fn source(&self) -> FrameSource;

    /// Starts feeding `encoder`, for sources whose frames the platform pushes itself.
    fn attach(&mut self, encoder: &Encoder) -> Result<()>;

    /// Stops feeding the current encoder (before it is replaced).
    fn detach(&mut self);

    /// The next encoder [`Output::attach`] feeds gets frames with the pointer left out
    /// (`apart`, see [`CursorSource`]) or drawn in, where the desktop makes that per
    /// stream (KWin); at once, also for the running one, where its
    /// [`CursorSource::restarts_streams`] is false (wlroots desktops: each frame is
    /// captured with the pointer in or out). Never apart for a window panel, which
    /// keeps the pointer.
    fn set_cursor_apart(&mut self, _apart: bool) {}

    /// Makes it the primary output, where the desktop has one.
    fn make_primary(&self);
}

/// The PC's windows: the active one, moving it to an output (and making it fill one, for
/// a window panel), noticing when it is gone, and pictures of the screen.
pub trait Windows {
    /// The window that has the keyboard.
    fn active(&mut self) -> Option<ActiveWindow>;

    /// The output the user works on (the one with the mouse, or with the active window,
    /// as the desktop counts it).
    fn active_output(&mut self) -> Option<OutputKey>;

    /// Moves window `w` to `output`; `fill`: full screen there (a window panel).
    fn move_to(&mut self, w: &ActiveWindow, output: &OutputKey, fill: bool) -> Result<()>;

    /// Puts window `w` back on `output` as an ordinary window (out of full screen): a
    /// window panel being closed.
    fn put_back(&mut self, w: &ActiveWindow, output: &OutputKey) -> Result<()>;

    /// Stops watching the window of panel `output`.
    fn forget_panel(&mut self, _output: &OutputKey) {}

    /// Panel outputs (of `panels`) whose window closed or left since the last call.
    fn vacated(&mut self, panels: &[OutputKey]) -> Vec<OutputKey>;

    /// Saves a picture of `output` (or of the whole desktop, where it cannot take one
    /// output; see [`DesktopCaps::screenshot_one_output`]) to the Pictures folder.
    fn screenshot(&mut self, output: Option<&OutputKey>) -> Result<PathBuf>;

    /// Every window, for a thread of its own (the one that looks where sound comes
    /// from). The desktop keeps it: such a thread holds it weakly, and stops once the
    /// desktop is gone.
    fn list(&self) -> Arc<dyn WindowList>;
}

/// Every normal window of the desktop, asked from any thread.
pub trait WindowList: Send + Sync {
    fn all(&self) -> Vec<AppWindow>;
}

/// A window, as [`WindowList::all`] lists it.
#[derive(Debug, Clone)]
pub struct AppWindow {
    /// The process that shows it.
    pub pid: u32,
    /// Its program, as the desktop calls it (Wayland app id or X11 class).
    pub class: String,
    /// The output it is on.
    pub output: OutputKey,
}

/// Where the mouse is. Moving it is the virtual input's ([`Injector::pointer_to`]).
pub trait Pointer {
    /// Where it is on the desktop, in logical pixels.
    fn position(&mut self) -> Option<(i32, i32)>;
}

/// What a desktop's virtual input makes, as if from devices of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Injects {
    /// The mouse: moved to a point, its buttons and its wheel. Touches too: as touches
    /// where [`Injector::touches`], else the core makes mouse actions of them.
    pub pointer: bool,
    /// Keys (evdev codes; the desktop's own keyboard layout applies).
    pub keys: bool,
}

impl Injects {
    pub const NONE: Self = Self {
        pointer: false,
        keys: false,
    };
}

/// Opens a desktop's virtual input. Called on the input thread (`crate::input`), never
/// the worker's: it may wait for the person, as the desktop's own dialog asks once
/// whether Prostir may control the mouse and the keyboard.
pub trait OpenInput: Send + Sync {
    /// The virtual devices, once the desktop made them; an error with [`InputRefused`]
    /// inside where the person said no (not asked again by itself).
    fn open(&self) -> Result<Box<dyn Injector>>;
}

/// The person did not let Prostir control the mouse and the keyboard.
#[derive(Debug)]
pub struct InputRefused;

impl std::fmt::Display for InputRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the person did not allow controlling the mouse and keyboard")
    }
}

impl std::error::Error for InputRefused {}

/// The person stopped the devices that were open (the desktop's remote control
/// ended by them): as [`InputRefused`], not opened again by itself.
#[derive(Debug)]
pub struct InputStopped;

impl std::fmt::Display for InputStopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the control of the mouse and keyboard was stopped on the PC")
    }
}

impl std::error::Error for InputStopped {}

/// A point of the desktop for the mouse or a finger: `x`, `y` in logical pixels of the
/// desktop (with their fractions), on output `output`, which is at `rect`; `desk` holds
/// every output, for a device that spans them all.
#[derive(Debug, Clone, PartialEq)]
pub struct Spot {
    pub output: OutputKey,
    pub rect: DeskRect,
    pub desk: DeskRect,
    pub x: f64,
    pub y: f64,
}

impl Spot {
    /// The point across its output, each way from 0 to 1.
    pub fn across(&self) -> (f64, f64) {
        let part = |v: f64, start: i32, len: u32| {
            ((v - f64::from(start)) / f64::from(len.max(1))).clamp(0.0, 1.0)
        };
        (
            part(self.x, self.rect.x, self.rect.w),
            part(self.y, self.rect.y, self.rect.h),
        )
    }
}

/// A mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

impl MouseButton {
    /// Its code in Linux's `input-event-codes.h` (`BTN_LEFT`...), which libei, wlroots
    /// and uinput take.
    pub fn evdev(self) -> u16 {
        match self {
            Self::Left => 0x110,
            Self::Right => 0x111,
            Self::Middle => 0x112,
            Self::Back => 0x113,
            Self::Forward => 0x114,
        }
    }
}

/// What a finger does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TouchPhase {
    Down,
    Move,
    Up,
    /// Gone without counting.
    Cancel,
}

/// A desktop's virtual input devices: the mouse, a touch screen, a keyboard. Each call
/// is one frame of its own, said to the desktop before it returns. The core keeps what
/// is held and lets it all go when its source goes (`crate::input`): the injector only
/// makes what it is told. An error means the devices are gone (the desktop closed
/// them): the core drops the injector and opens a new one when needed. Dropped, its
/// devices go, and the desktop lets go of what they held.
pub trait Injector: Send {
    /// It makes real touches; else the core makes mouse actions of them.
    fn touches(&self) -> bool;
    /// The mouse to `at`.
    fn pointer_to(&mut self, at: &Spot) -> Result<()>;
    /// The mouse moved by `dx`, `dy` logical pixels, without the desktop's acceleration.
    fn pointer_by(&mut self, dx: f64, dy: f64) -> Result<()>;
    fn button(&mut self, button: MouseButton, down: bool) -> Result<()>;
    /// The wheel, in 1/120 of a notch: `dy` above 0 scrolls down, `dx` above 0 right.
    fn scroll(&mut self, dx: i32, dy: i32) -> Result<()>;
    /// Finger `id` (one number for each finger touching at once) at `at`.
    fn touch(&mut self, id: u32, phase: TouchPhase, at: &Spot) -> Result<()>;
    /// Key `code` (evdev) pressed or let go.
    fn key(&mut self, code: u16, down: bool) -> Result<()>;
    /// Called at least every 100 ms while it is open: reads what the desktop said
    /// (some desktops ask whether the program is still there).
    fn pump(&mut self) -> Result<()> {
        Ok(())
    }
}

/// The key of an output, stable while it exists: on Linux the connector name the
/// desktop lists (`Virtual-Prostir-1`, `Prostir-2`, `eDP-1`), which does not change.
/// A desktop whose names do change makes its own key and keeps it to itself. Shared,
/// not copied: the pointer's every move names its output.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OutputKey(Arc<str>);

impl OutputKey {
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into().into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::borrow::Borrow<str> for OutputKey {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for OutputKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// How an output is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputHealth {
    /// Streams as made.
    Live,
    /// Exists, but sends no frames for a while, and comes back by itself: nothing to
    /// start over and nothing to lay out again.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "no Linux desktop pauses an output")
    )]
    Paused { why: String },
    /// Another program gave it this pixel size, which is not the size it was made with
    /// (on KDE, sharing it in Discord or OBS does that). Starting its encoder over puts
    /// the size back.
    Resized { w: u32, h: u32 },
    /// The desktop closed it.
    Closed,
}

/// Where an output's frames come from.
#[derive(Debug, Clone)]
pub struct FrameSource {
    /// The start of the encoder's pipeline.
    pub input: Input,
    /// Who makes the frames.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "no encoder depends on who makes the frames")
    )]
    pub origin: Origin,
    /// Where the frames can be handed over without a copy: an encoder that takes this
    /// keeps them on the graphics card.
    pub memory: Memory,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "every Linux source draws the pointer in")
    )]
    pub cursor: Cursor,
}

impl FrameSource {
    /// The graphics card the frames live on, where the memory says one (said once, in
    /// [`FrameSource::memory`]).
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the encoders read the card from the memory itself"
        )
    )]
    pub fn adapter(&self) -> Option<AdapterId> {
        match self.memory {
            Memory::D3D11 { adapter } => Some(adapter),
            Memory::System | Memory::DmaBuf => None,
        }
    }
}

/// Who makes an output's frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The virtual monitor's own maker hands them over: KWin's stream of a virtual
    /// output, the swap chain of our display driver on Windows.
    Monitor,
    /// They are copied off a screen: wlr-screencopy, KWin's stream of a real display,
    /// Desktop Duplication.
    Capture,
}

/// Where frames live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Memory {
    /// Plain memory.
    System,
    /// Linux GPU buffers; also plain memory for an encoder that takes no others.
    DmaBuf,
    /// Direct3D 11 textures on one graphics card.
    D3D11 { adapter: AdapterId },
}

/// Connecting to a desktop failed for a reason the person can fix, and this says what
/// to do, already in their language: the text goes to the headset, the settings window
/// and the tray as it is, instead of a failure to be worded again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unusable(pub String);

impl std::fmt::Display for Unusable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unusable {}

/// A graphics card, as the system tells them apart (on Windows its LUID, which
/// changes between boots, so it is never saved).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AdapterId(pub u64);

/// The graphics devices captures and encoders share: one per card, for the life of a
/// desktop. It is handed to the desktop when it connects ([`crate::platform::connect`]),
/// so an output that captures on a card (Desktop Duplication, our driver's swap chain)
/// takes the card's device from here, and the encoders take the same one: neither an
/// output nor an encoder makes a device of its own (six processes, each with its own
/// CUDA context, fell to a tenth of the frames). In GStreamer it answers each
/// pipeline's `need-context` (`gst.va.display.handle`, `gst.cuda.context`,
/// `gst.d3d11.device.handle` by LUID): a capture's pipeline calls
/// [`GpuDevices::share_with`] as the encoder's does (see `encoder/gpu.rs`).
///
/// On Linux no output makes a device (KWin and wlr-screencopy hand over buffers): NVENC
/// encoders share one CUDA context per card; VA-API encoders keep a display each, which
/// measured as fast or faster than one shared (see `encoder/gpu.rs`).
pub use crate::encoder::GpuDevices;

/// Where the mouse pointer is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cursor {
    /// Into the frames.
    InFrame,
    /// Apart from them (KWin's stream with the pointer apart, Desktop Duplication, a
    /// driver's hardware cursor).
    #[cfg_attr(
        all(not(test), feature = "fake-desktop"),
        expect(dead_code, reason = "the fake desktop draws no pointer in")
    )]
    Separate,
}

/// What a desktop can do.
#[derive(Debug, Clone)]
pub struct DesktopCaps {
    /// For people and the headset: `KDE Plasma`, `Hyprland`.
    pub name: &'static str,
    /// Whether virtual monitors can be made, and how; or why not (the display driver is
    /// not installed). The PC's own displays work either way.
    pub virtual_monitors: Result<AddModel, Unavailable>,
    /// It has a primary output (where the panel and new windows go).
    pub has_primary: bool,
    /// The primary output is always at (0, 0): the core shifts its whole plan so that
    /// it is (the places the headset is told shift with it).
    pub origin_is_primary: bool,
    /// The scales it gives an output of any size, in percent (see
    /// [`Desktop::scales_for`] for those of one size).
    pub scales: Scales,
    /// It changes the scale of a monitor of ours in place ([`DeskPlan::scales`]), the
    /// output, its stream and its windows as they are; else a monitor is made anew at
    /// another scale.
    pub scales_in_place: bool,
    /// Refresh rates it makes virtual monitors with, in Hz.
    pub refresh: Vec<u16>,
    /// [`Windows::screenshot`] can take a single output; if not, a monitor's picture is
    /// decoded from its own stream.
    pub screenshot_one_output: bool,
    /// What its virtual input makes ([`Desktop::input`]); nothing where it has none.
    pub injects: Injects,
    /// Its virtual monitors go by themselves when Prostir is gone (KWin does that; a
    /// driver's watchdog would), so nothing is left to clean up after a crash.
    pub cleans_up_after_crash: bool,
}

impl DesktopCaps {
    /// The refresh rate a new virtual monitor gets: 60 Hz where the desktop makes that,
    /// else the first it makes.
    pub fn refresh_for_new(&self) -> u16 {
        if self.refresh.contains(&60) {
            60
        } else {
            self.refresh.first().copied().unwrap_or(60)
        }
    }
}

/// How adding a virtual monitor goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddModel {
    /// One at a time: the others stay as they are.
    OneByOne,
    /// Adding or removing one makes every virtual monitor start over (a driver that
    /// restarts its adapter): all their streams break at once, which is no failure of
    /// each. No driver Prostir ships works so: the unchanged Virtual Display Driver
    /// does, and the owner ruled it out even as a fallback (it loses the layout and
    /// the names). Kept so a desktop can say it of a driver it finds.
    RestartsAll,
}

/// Why something the desktop would do cannot be done here.
#[derive(Debug, Clone)]
pub struct Unavailable {
    /// What the headset is told.
    pub error: UserError,
    /// What the person at the PC is told.
    pub why: String,
}

/// The scales a desktop gives an output, in percent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scales {
    /// Any from `min` to `max`.
    Any { min: u16, max: u16 },
    /// Only these.
    Steps(Vec<u16>),
}

impl Scales {
    pub fn allows(&self, scale: u16) -> bool {
        match self {
            Self::Any { min, max } => (*min..=*max).contains(&scale),
            Self::Steps(steps) => steps.contains(&scale),
        }
    }
}

/// A virtual monitor to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualSpec {
    /// The screen it is for, the same in every run: a driver can make it the
    /// monitor's serial number, so the system remembers its place and scale.
    pub id: ScreenId,
    /// The lowest number free when it was made: its name on the desktop (`Prostir-N`),
    /// so the desktop finds what it remembers for that name.
    pub number: u32,
    /// Its picture in pixels.
    pub w: u32,
    pub h: u32,
    /// Its scale in percent (as `Mode.scale` in the protocol), 100 as well as any
    /// other: the desktop is told it even where it remembers another for the name.
    pub scale: u16,
    /// Hz, one of [`DesktopCaps::refresh`].
    pub refresh: u16,
}

impl VirtualSpec {
    /// The scale as a factor (1.25).
    pub fn factor(&self) -> f64 {
        f64::from(self.scale) / 100.0
    }

    /// Its size on the desktop at its scale, in logical pixels.
    pub fn logical(&self) -> (i32, i32) {
        let at = |px: u32| (f64::from(px) / self.factor()).round() as i32;
        (at(self.w), at(self.h))
    }
}

/// Whether a monitor just made is to be told its scale: unless the desktop lists it at
/// `spec`'s scale already (`listed`: its logical size there). A desktop that keeps each
/// output's settings by name, as KWin does, makes it at the scale that name had last,
/// 125 % a moment ago say, also where 100 % is wanted now.
pub fn needs_scale(spec: &VirtualSpec, listed: Option<(i32, i32)>) -> bool {
    let (w, h) = spec.logical();
    listed.is_none_or(|(lw, lh)| (lw - w).abs() > 1 || (lh - h).abs() > 1)
}

/// The scale in percent a monitor of `pixels` stands at, listed by the desktop at the
/// logical size `listed`: a multiple of 5 where it is that within 1. None where the two
/// sides do not give the same scale (another program's stream resized it, say).
pub fn scale_of(pixels: (u32, u32), listed: (i32, i32)) -> Option<u16> {
    if listed.0 <= 0 || listed.1 <= 0 {
        return None;
    }
    let across = f64::from(pixels.0) * 100.0 / f64::from(listed.0);
    let down = f64::from(pixels.1) * 100.0 / f64::from(listed.1);
    // A logical pixel rounded either way on a side of 240 is 0.4 %.
    if (across - down).abs() > across * 0.01 + 1.0 {
        return None;
    }
    let percent = across.round();
    let fives = (percent / 5.0).round() * 5.0;
    let percent = if (percent - fives).abs() <= 1.0 {
        fives
    } else {
        percent
    };
    u16::try_from(percent as i64).ok()
}

/// `2560x1440`, or `3840x2160@1.5` with a scale.
impl std::fmt::Display for VirtualSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}x{}", self.w, self.h)?;
        if self.scale != 100 {
            write!(f, "@{}", self.factor())?;
        }
        Ok(())
    }
}

/// Where outputs go on the desktop.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeskPlan {
    /// Each output's top-left corner, in logical pixels.
    pub positions: Vec<(OutputKey, i32, i32)>,
    /// The output to make primary, where the desktop has one.
    pub primary: Option<OutputKey>,
    /// Monitors of ours whose scale changes, in percent (one the desktop gives their
    /// size): only where [`DesktopCaps::scales_in_place`], never a display of the PC
    /// or a window panel. Their places in `positions` are for their new size.
    pub scales: Vec<(OutputKey, u16)>,
}

/// Every output on the desktop, by key.
pub type Outputs = HashMap<OutputKey, Geometry>;

/// Where an output is on the desktop: position and size in logical pixels, and its
/// priority (1 = primary; only KDE has priorities).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub priority: Option<u32>,
    /// `false` for an output the desktop lists as disabled (a closed laptop lid).
    pub enabled: bool,
    /// Virtual (ours or another program's), not a display of the PC.
    pub is_virtual: bool,
}

/// A display of the PC itself, connected now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhysicalDisplay {
    /// Its key, to tell it from the others and to save; nothing to show to people.
    /// On Linux its connector (`eDP-1`, `HDMI-A-1`).
    pub key: OutputKey,
    /// What people see it called: its maker and model as the display says them
    /// (`DELL U2720Q`), else its connector on Linux, the name the system gives it on
    /// Windows. The headset shows it (`Display.name`).
    pub label: String,
    /// Maker, model and serial number as the display says them (its EDID), as far as
    /// the desktop tells; none when it says nothing.
    pub edid: Option<String>,
    /// The laptop's own panel.
    pub builtin: bool,
    /// Its picture in pixels, when known.
    pub size: Option<(u32, u32)>,
    /// The one to show in the headset when nothing else was chosen: an external
    /// monitor before the laptop's panel, never the panel of a closed laptop.
    pub default: bool,
}

/// The window that has the keyboard, as found when asked.
#[derive(Debug, Clone)]
pub struct ActiveWindow {
    /// The desktop's id for it (Hyprland address, KWin internal id).
    #[cfg_attr(
        feature = "fake-desktop",
        expect(dead_code, reason = "the fake desktop has no windows")
    )]
    pub id: String,
    /// Size on the desktop, in logical pixels.
    pub width: i32,
    pub height: i32,
    /// Scale of the output it is on.
    pub scale: f64,
    /// The output it is on, when the desktop says.
    pub output: Option<OutputKey>,
}

/// What a Windows desktop would report, built here so the contract keeps room for it:
/// these values must stay expressible (and every field readable) without a Windows
/// build.
#[cfg(test)]
mod windows_shape {
    use super::*;

    /// A Windows PC with our display driver: monitors one at a time, physical pixels with
    /// the primary at (0, 0), scales in steps, refresh rates the driver offers.
    fn with_driver() -> DesktopCaps {
        DesktopCaps {
            name: "Windows",
            virtual_monitors: Ok(AddModel::OneByOne),
            has_primary: true,
            origin_is_primary: true,
            scales: Scales::Steps(vec![100, 125, 150, 175, 200]),
            scales_in_place: true,
            refresh: vec![60, 72, 90, 120],
            screenshot_one_output: true,
            injects: Injects {
                pointer: true,
                keys: true,
            },
            cleans_up_after_crash: true,
        }
    }

    #[test]
    fn a_windows_desktop_fits_the_contract() {
        let caps = with_driver();
        assert_eq!(
            caps.virtual_monitors.as_ref().ok(),
            Some(&AddModel::OneByOne)
        );
        assert!(caps.has_primary && caps.origin_is_primary && caps.cleans_up_after_crash);
        assert!(caps.scales.allows(125) && !caps.scales.allows(130));
        assert_eq!(caps.refresh_for_new(), 60);
        assert!(caps.screenshot_one_output && caps.injects.pointer && caps.injects.keys);

        // No driver: the PC's own displays still work, adding a monitor says why not.
        let without = DesktopCaps {
            virtual_monitors: Err(Unavailable {
                error: UserError::Unsupported,
                why: "The Prostir display driver is not installed".into(),
            }),
            cleans_up_after_crash: false,
            ..with_driver()
        };
        let no = without.virtual_monitors.unwrap_err();
        assert_eq!(no.error, UserError::Unsupported);
        assert!(!no.why.is_empty());

        // A driver that restarts its adapter for every change.
        assert_ne!(AddModel::RestartsAll, AddModel::OneByOne);
    }

    #[test]
    fn windows_frames_fit_the_contract() {
        let gpu = AdapterId(0x0001_2345);
        // Our monitor's frames straight from the driver's swap chain, on its card.
        let driver = FrameSource {
            input: Input {
                source: "appsrc name=src".into(),
                label: "Prostir-1".into(),
                frame_rate: None,
                live: false,
            },
            origin: Origin::Monitor,
            memory: Memory::D3D11 { adapter: gpu },
            cursor: Cursor::Separate,
        };
        // A display of the PC through Desktop Duplication.
        let duplication = FrameSource {
            input: Input {
                source: "d3d11screencapturesrc capture-api=dxgi".into(),
                label: "DISPLAY1".into(),
                frame_rate: None,
                live: false,
            },
            origin: Origin::Capture,
            memory: Memory::D3D11 { adapter: gpu },
            cursor: Cursor::InFrame,
        };
        for source in [&driver, &duplication] {
            assert_eq!(source.adapter(), Some(gpu));
            assert!(matches!(source.memory, Memory::D3D11 { adapter } if adapter == gpu));
            assert!(!source.input.source.is_empty());
        }
        assert_ne!(driver.origin, duplication.origin);
        assert_ne!(driver.cursor, duplication.cursor);
        assert_ne!(Memory::System, Memory::DmaBuf);

        // The lock screen or a UAC prompt: no frames for a while, nothing broken.
        let locked = OutputHealth::Paused {
            why: "the lock screen is shown".into(),
        };
        assert!(matches!(&locked, OutputHealth::Paused { why } if !why.is_empty()));
        assert_ne!(locked, OutputHealth::Closed);
    }

    fn spec(w: u32, h: u32, scale: u16) -> VirtualSpec {
        VirtualSpec {
            id: ScreenId::saved(3),
            number: 4,
            w,
            h,
            scale,
            refresh: 60,
        }
    }

    /// Made anew at the scale its name had last, a monitor is told the one wanted,
    /// 100 % too; one listed at it already needs nothing.
    #[test]
    fn a_scale_is_set_unless_the_desktop_has_it() {
        let s = spec(2560, 1440, 100);
        assert!(needs_scale(&s, None), "not listed yet");
        assert!(needs_scale(&s, Some((2048, 1152))), "at 125 % from before");
        assert!(!needs_scale(&s, Some((2560, 1440))));
        let s = spec(2560, 1440, 125);
        assert!(needs_scale(&s, Some((2560, 1440))));
        assert!(!needs_scale(&s, Some((2048, 1152))));
        // Rounded either way by the desktop.
        let s = spec(1920, 1080, 150);
        assert!(!needs_scale(&s, Some((1280, 721))));
    }

    #[test]
    fn the_scale_a_desktop_lists_an_output_at() {
        assert_eq!(scale_of((2560, 1440), (2560, 1440)), Some(100));
        assert_eq!(scale_of((2560, 1440), (2048, 1152)), Some(125));
        assert_eq!(scale_of((1440, 2560), (960, 1707)), Some(150));
        assert_eq!(scale_of((640, 360), (427, 240)), Some(150));
        assert_eq!(scale_of((1920, 1080), (1097, 617)), Some(175));
        assert_eq!(scale_of((2560, 1440), (2327, 1309)), Some(110));
        // Resized by another program: the sides disagree.
        assert_eq!(scale_of((2560, 1440), (320, 240)), None);
        assert_eq!(scale_of((2560, 1440), (0, 0)), None);
    }

    /// Desktop Duplication says the pointer apart: what it says reaches the core's sink
    /// as it was said, from any thread.
    #[test]
    fn a_pointer_apart_fits_the_contract() {
        let heard = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = {
            let heard = heard.clone();
            CursorSink::new(move |e| heard.lock().unwrap().push(e))
        };
        let image = CursorImage {
            w: 32,
            h: 32,
            hot_x: 0,
            hot_y: 0,
            rgba: vec![0; 32 * 32 * 4],
        };
        let events = [
            CursorEvent::Shape(image),
            CursorEvent::Moved {
                output: OutputKey::new("DISPLAY1"),
                x: -3,
                y: 2000,
                size: (3840, 2160),
            },
            CursorEvent::Hidden,
        ];
        let sent = events.clone();
        std::thread::spawn(move || sent.into_iter().for_each(|e| sink.send(e)))
            .join()
            .unwrap();
        assert_eq!(*heard.lock().unwrap(), events);
    }

    #[test]
    fn a_windows_monitor_spec_fits_the_contract() {
        let spec = VirtualSpec {
            id: ScreenId::saved(7),
            number: 1,
            w: 2560,
            h: 1440,
            scale: 125,
            refresh: 120,
        };
        assert_eq!(spec.id, ScreenId::saved(7));
        assert_eq!(spec.to_string(), "2560x1440@1.25");
        assert!(with_driver().refresh.contains(&spec.refresh));
    }
}
