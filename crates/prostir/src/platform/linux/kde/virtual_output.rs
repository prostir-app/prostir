//! Virtual monitors created by KWin through `zkde_screencast_unstable_v1`.
//!
//! `stream_virtual_output` makes KWin add a real output (visible in System
//! Settings → Display, windows can be moved onto it) and stream its contents
//! into a PipeWire node. Closing the stream removes the output again.
//!
//! One Wayland connection ([`Screencast`]) serves all monitors, which come and
//! go one by one ([`VirtualOutput`]).
//!
//! KWin gives a virtual output the size that any stream of it negotiates, also a
//! stream another program opened (sharing the screen in Discord through the portal).
//! The output's `wl_output` mode shows that at once ([`VirtualOutput::resized`]);
//! our own stream, connected again, makes KWin put the created size back.

use std::collections::HashMap;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use super::kscreen;
use crate::encoder::Input;
use crate::platform::linux::is_panel;
use crate::platform::{OutputKey, PhysicalDisplay, VirtualSpec, needs_scale, scale_of};
use log::{info, warn};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_output, wl_registry};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols::xdg::xdg_output::zv1::client::{
    zxdg_output_manager_v1::ZxdgOutputManagerV1,
    zxdg_output_v1::{self, ZxdgOutputV1},
};
use wayland_protocols_plasma::screencast::v1::client::{
    zkde_screencast_stream_unstable_v1::{self, ZkdeScreencastStreamUnstableV1},
    zkde_screencast_unstable_v1::ZkdeScreencastUnstableV1,
};

/// Pointer mode from the protocol: render the cursor into the stream.
const POINTER_EMBEDDED: u32 = 2;

/// How often the last frame of a still screen is sent again, in ms. The session
/// needs none of it (pings keep it), and with the frame rate and liveness told (see
/// [`input_for`]) no encoder holds a frame back waiting for the next; it is what
/// still pushes out the one keyframe a still screen gets, and each costs an encoding
/// on the PC and a decoding and a wake-up of the radio in the headset.
const KEEPALIVE_MS: u32 = 2000;

/// The refresh rate in mHz of an output whose rate KWin did not say.
const DEFAULT_REFRESH: u32 = 60_000;

/// A refresh rate in mHz as a frame rate for GStreamer: `60/1`, `144/1`, `59940/1000`
/// reduced to `2997/50`.
fn frame_rate(mhz: u32) -> gstreamer::Fraction {
    fn gcd(a: u32, b: u32) -> u32 {
        if b == 0 { a } else { gcd(b, a % b) }
    }
    let mhz = if mhz == 0 { DEFAULT_REFRESH } else { mhz };
    let d = gcd(mhz, 1000);
    // Past i32 (millions of frames a second) no real output's rate.
    gstreamer::Fraction::new(
        i32::try_from(mhz / d).unwrap_or(i32::MAX),
        (1000 / d) as i32,
    )
}

/// The encoder's input for KWin's PipeWire node `node` of an output at `refresh` mHz.
///
/// KWin's stream says no frame rate (`0/1`, frames come when the screen changes),
/// and an encoder without one budgets badly, so it is told the output's refresh rate
/// ([`crate::encoder::tell_frame_rate`]); frames still come only when something
/// changes. Nothing may stand between `pipewiresrc` and the encoder that hides which
/// formats the encoder takes (as `capssetter` does): `pipewiresrc` then offers
/// PipeWire no format and is linked to nothing ("target not found").
///
/// It is live, which `pipewiresrc` says only once its stream is up, after VA-API
/// asked and chose to keep frames back (each picture of a still screen then waited
/// for the keepalive 2 s later): the encoder is told so ([`crate::encoder::tell_live`]).
///
/// Only that node: when it is gone, WirePlumber would otherwise link the stream to
/// the default video source, a webcam (seen here with a node that was not there:
/// linked to one, it failed only on its format). With `node.dont-fallback` it says
/// "defined target not found" at once.
fn input_for(node: u32, refresh: u32) -> Input {
    Input {
        // On a still screen KWin sends nothing; PipeWire's source repeats the last
        // frame every 2 s (see `KEEPALIVE_MS`).
        source: format!(
            "pipewiresrc path={node} do-timestamp=true keepalive-time={KEEPALIVE_MS} \
             always-copy=false stream-properties=\"props,node.dont-fallback=true\""
        ),
        label: format!("node {node}"),
        frame_rate: Some(frame_rate(refresh)),
        live: true,
    }
}

/// Sleeps, with no time limit, until KWin says something on the connection
/// (`connection`) or `wake` is written to (the connection is being dropped): whether
/// there is something to read on the connection (or it broke, which reading says).
/// A signal wakes it too, with neither.
fn sleep_until_said(connection: BorrowedFd<'_>, wake: BorrowedFd<'_>) -> bool {
    let mut fds = [
        libc::pollfd {
            fd: connection.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: wake.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    // SAFETY: two valid pollfds, alive through the call.
    let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
    n > 0 && fds[0].revents != 0
}

#[derive(Default)]
struct StreamState {
    node: Option<u32>,
    failed: Option<String>,
    closed: bool,
}

/// A compositor output as its `wl_output` events describe it.
#[derive(Debug, Default, PartialEq)]
struct OutputInfo {
    /// Connector name: eDP-1, HDMI-A-1, Virtual-Prostir-1…
    name: Option<String>,
    /// Current mode in pixels, as of the last `done`.
    mode: Option<(u32, u32)>,
    /// Current mode announced since that `done`.
    next_mode: Option<(u32, u32)>,
    /// The current mode's refresh rate in mHz, as of the last `done`, and as announced
    /// since.
    refresh: Option<u32>,
    next_refresh: Option<u32>,
    /// What the display says it is (its EDID, as KWin passes it on).
    make: String,
    model: String,
    /// Its size on the desktop in logical pixels (xdg-output), as of the last `done`,
    /// and as announced since.
    logical: Option<(i32, i32)>,
    next_logical: Option<(i32, i32)>,
}

impl OutputInfo {
    fn on_event(&mut self, event: wl_output::Event) {
        match event {
            wl_output::Event::Name { name } => self.name = Some(name),
            wl_output::Event::Geometry { make, model, .. } => {
                (self.make, self.model) = (make, model);
            }
            wl_output::Event::Mode {
                flags: WEnum::Value(flags),
                width,
                height,
                refresh,
            } if flags.contains(wl_output::Mode::Current) => {
                self.next_mode = u32::try_from(width).ok().zip(u32::try_from(height).ok());
                self.next_refresh = u32::try_from(refresh).ok().filter(|r| *r > 0);
            }
            wl_output::Event::Done => {
                if let Some(mode) = self.next_mode.take() {
                    self.mode = Some(mode);
                    self.refresh = self.next_refresh.take();
                }
                // xdg-output 3 and later: its sizes count with the output's `done`.
                if let Some(size) = self.next_logical.take() {
                    self.logical = Some(size);
                }
            }
            _ => {}
        }
    }

    fn on_xdg_event(&mut self, event: &zxdg_output_v1::Event) {
        match *event {
            zxdg_output_v1::Event::LogicalSize { width, height } => {
                self.next_logical = Some((width, height));
            }
            // Before xdg-output 3, its own `done` ends a change.
            zxdg_output_v1::Event::Done => {
                if let Some(size) = self.next_logical.take() {
                    self.logical = Some(size);
                }
            }
            _ => {}
        }
    }

    /// The scale it stands at on the desktop, in percent: its pixels over its logical
    /// size.
    fn scale(&self) -> Option<u16> {
        scale_of(self.mode?, self.logical?)
    }
}

/// The KScreen setting for the output's scale.
fn scale_setting(spec: &VirtualSpec) -> String {
    format!("scale.{}", spec.factor())
}

/// The current size `now` when it is not the size `made` the output was created with.
fn resized(made: (u32, u32), now: Option<(u32, u32)>) -> Option<(u32, u32)> {
    now.filter(|&size| size != made)
}

/// Stream states keyed by monitor number, shared with the event thread.
#[derive(Default)]
struct Shared {
    streams: Mutex<HashMap<u32, StreamState>>,
    changed: Condvar,
    /// Compositor outputs by registry name.
    outputs: Mutex<HashMap<u32, (wl_output::WlOutput, OutputInfo)>>,
    /// KWin's xdg-output, which tells each output's logical size (and so its scale) as
    /// it changes, and the xdg-output of each output by registry name.
    xdg: Mutex<Option<ZxdgOutputManagerV1>>,
    xdg_outputs: Mutex<HashMap<u32, ZxdgOutputV1>>,
    /// Counts the changes of the outputs: one came or went, or one's description is
    /// done (a mode, a scale, a position).
    outputs_changed: std::sync::atomic::AtomicU64,
}

impl Shared {
    fn bind_output(
        &self,
        registry: &wl_registry::WlRegistry,
        name: u32,
        version: u32,
        qh: &QueueHandle<State>,
    ) {
        // wl_output v4 announces the connector name.
        if version < 4 {
            return;
        }
        let output = registry.bind::<wl_output::WlOutput, _, _>(name, 4, qh, name);
        if let Some(xdg) = self.xdg.lock().unwrap().as_ref() {
            let logical = xdg.get_xdg_output(&output, qh, name);
            self.xdg_outputs.lock().unwrap().insert(name, logical);
        }
        self.outputs
            .lock()
            .unwrap()
            .insert(name, (output, OutputInfo::default()));
        self.outputs_changed.fetch_add(1, Ordering::Relaxed);
    }
}

struct State {
    shared: Arc<Shared>,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } if interface == "wl_output" => {
                state.shared.bind_output(registry, name, version, qh);
            }
            wl_registry::Event::GlobalRemove { name } => {
                let logical = state.shared.xdg_outputs.lock().unwrap().remove(&name);
                if let Some(logical) = logical {
                    logical.destroy();
                }
                let mut outputs = state.shared.outputs.lock().unwrap();
                if let Some((output, _)) = outputs.remove(&name) {
                    output.release();
                    state.shared.outputs_changed.fetch_add(1, Ordering::Relaxed);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, u32> for State {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        global: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let done = matches!(event, wl_output::Event::Done);
        if let Some((_, info)) = state.shared.outputs.lock().unwrap().get_mut(global) {
            info.on_event(event);
        }
        if done {
            state.shared.outputs_changed.fetch_add(1, Ordering::Relaxed);
        }
    }
}

impl Dispatch<ZxdgOutputManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZxdgOutputManagerV1,
        _: <ZxdgOutputManagerV1 as Proxy>::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZxdgOutputV1, u32> for State {
    fn event(
        state: &mut Self,
        _: &ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        global: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let Some((_, info)) = state.shared.outputs.lock().unwrap().get_mut(global) {
            info.on_xdg_event(&event);
        }
    }
}

impl Dispatch<ZkdeScreencastUnstableV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZkdeScreencastUnstableV1,
        _: <ZkdeScreencastUnstableV1 as Proxy>::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZkdeScreencastStreamUnstableV1, u32> for State {
    fn event(
        state: &mut Self,
        _: &ZkdeScreencastStreamUnstableV1,
        event: zkde_screencast_stream_unstable_v1::Event,
        number: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let mut streams = state.shared.streams.lock().unwrap();
        let s = streams.entry(*number).or_default();
        match event {
            zkde_screencast_stream_unstable_v1::Event::Created { node } => s.node = Some(node),
            zkde_screencast_stream_unstable_v1::Event::Failed { error } => s.failed = Some(error),
            zkde_screencast_stream_unstable_v1::Event::Closed => {
                warn!("KWin closed virtual output Prostir-{number}");
                s.closed = true;
            }
            _ => {}
        }
        state.shared.changed.notify_all();
    }
}

/// Connection to KWin's screencast interface.
pub struct Screencast {
    conn: Connection,
    screencast: ZkdeScreencastUnstableV1,
    qh: QueueHandle<State>,
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    /// Wakes the thread that reads KWin's events (it sleeps until one comes).
    wake: Arc<OwnedFd>,
    thread: Option<JoinHandle<()>>,
}

impl Screencast {
    pub fn connect() -> Result<Self> {
        let conn = Connection::connect_to_env().context(
            "cannot connect to the Wayland compositor (is this a Plasma Wayland session?)",
        )?;
        let (globals, mut queue) = registry_queue_init::<State>(&conn)?;
        let qh = queue.handle();
        // KWin leaves the interface out of the registry of a program it does not
        // allow (see `access.rs`): told apart from a version it does not know.
        let listed = globals.contents().with_list(|list| {
            list.iter()
                .any(|g| g.interface == ZkdeScreencastUnstableV1::interface().name)
        });
        if !listed {
            // Interfaces only KWin has tell it from another compositor that names itself
            // KDE.
            let kwin = globals.contents().with_list(|list| {
                list.iter().any(|g| {
                    matches!(
                        g.interface.as_str(),
                        "kde_output_device_v2" | "org_kde_kwin_server_decoration_manager"
                    )
                })
            });
            return Err(anyhow::Error::new(super::access::NotExposed { kwin }));
        }
        let screencast: ZkdeScreencastUnstableV1 = globals.bind(&qh, 2..=4, ()).context(
            "KWin offers zkde_screencast_unstable_v1 in a version this program does not speak",
        )?;
        let shared = Arc::new(Shared::default());
        // Each output's logical size, where KWin offers it (without it nothing knows
        // a scale given in the display settings).
        *shared.xdg.lock().unwrap() = globals.bind(&qh, 1..=3, ()).ok();
        for g in globals.contents().clone_list() {
            if g.interface == "wl_output" {
                shared.bind_output(globals.registry(), g.name, g.version, &qh);
            }
        }
        let mut state = State {
            shared: shared.clone(),
        };
        // Collect the connector names before anyone asks for them.
        queue.roundtrip(&mut state)?;

        // Reads events in the background: replies to new monitors, `closed` notices.
        let stop = Arc::new(AtomicBool::new(false));
        // Written once to wake the thread when the connection is dropped.
        // SAFETY: a new eventfd, owned from here on.
        let wake = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC) };
        if wake < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: a descriptor just made, owned by nobody else.
        let wake = Arc::new(unsafe { OwnedFd::from_raw_fd(wake) });
        let thread = {
            let stop = stop.clone();
            let woken = wake.clone();
            std::thread::Builder::new()
                .name("wayland".into())
                .spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        if queue.dispatch_pending(&mut state).is_err() || queue.flush().is_err() {
                            break;
                        }
                        let Some(guard) = queue.prepare_read() else {
                            continue;
                        };
                        if sleep_until_said(guard.connection_fd(), woken.as_fd())
                            && guard.read().is_err()
                        {
                            break;
                        }
                    }
                    // KWin went away (or the connection broke): every stream is over,
                    // so the outputs get recreated on a fresh connection.
                    if !stop.load(Ordering::Relaxed) {
                        warn!("lost the connection to KWin");
                        let mut streams = state.shared.streams.lock().unwrap();
                        for s in streams.values_mut() {
                            s.closed = true;
                        }
                        drop(streams);
                        state.shared.changed.notify_all();
                    }
                })?
        };
        Ok(Self {
            conn,
            screencast,
            qh,
            shared,
            stop,
            wake,
            thread: Some(thread),
        })
    }

    /// How many times the outputs changed so far: a number that only grows.
    pub fn outputs_changed(&self) -> u64 {
        self.shared.outputs_changed.load(Ordering::Relaxed)
    }

    /// The displays of the PC itself that are on (KWin lists no output of a closed
    /// laptop's panel, or of a display switched off); the first by connector name is
    /// the one shown by default.
    pub fn physical_displays(&self) -> Vec<PhysicalDisplay> {
        let outputs = self.shared.outputs.lock().unwrap();
        let mut found: Vec<PhysicalDisplay> = outputs
            .values()
            .filter_map(|(_, info)| {
                let name = info.name.as_ref().filter(|n| !n.starts_with("Virtual-"))?;
                let said: Vec<&str> = [info.make.trim(), info.model.trim()]
                    .into_iter()
                    .filter(|w| !w.is_empty() && *w != "unknown")
                    .collect();
                Some(PhysicalDisplay {
                    key: OutputKey::new(name.clone()),
                    label: crate::platform::display_label(&info.make, &info.model, name),
                    edid: (!said.is_empty()).then(|| said.join(" ")),
                    builtin: is_panel(name),
                    size: info.mode,
                    default: false,
                })
            })
            .collect();
        found.sort_by(|a, b| a.key.cmp(&b.key));
        if let Some(first) = found.first_mut() {
            first.default = true;
        }
        found
    }

    /// Streams an existing display (e.g. the laptop panel) without creating anything.
    /// KWin streams a given display only once per connection, so use a fresh
    /// [`Screencast`] for every mirror.
    pub fn mirror(&self, number: u32, output_name: &str) -> Result<VirtualOutput> {
        let (output, refresh) = self
            .shared
            .outputs
            .lock()
            .unwrap()
            .values()
            .find(|(_, info)| info.name.as_deref() == Some(output_name))
            .map(|(o, info)| (o.clone(), info.refresh))
            .with_context(|| format!("display {output_name} is gone"))?;
        self.shared
            .streams
            .lock()
            .unwrap()
            .insert(number, StreamState::default());
        let stream = self
            .screencast
            .stream_output(&output, POINTER_EMBEDDED, &self.qh, number);
        // Normally ~10 ms; never hold up the headset's requests for long.
        let mirror = self.wait_for_node(
            number,
            stream,
            output_name.to_owned(),
            None,
            refresh.unwrap_or(DEFAULT_REFRESH),
            Duration::from_secs(2),
        )?;
        info!(
            "mirroring {output_name} (stream {number}), PipeWire node {}",
            mirror.node
        );
        Ok(mirror)
    }

    fn wait_for_node(
        &self,
        number: u32,
        stream: ZkdeScreencastStreamUnstableV1,
        name: String,
        made: Option<(u32, u32)>,
        refresh: u32,
        timeout: Duration,
    ) -> Result<VirtualOutput> {
        self.conn.flush()?;
        let deadline = Instant::now() + timeout;
        let mut streams = self.shared.streams.lock().unwrap();
        let node = loop {
            let Some(s) = streams.get(&number) else {
                drop(streams);
                stream.close();
                let _ = self.conn.flush();
                bail!("stream for {name} went away");
            };
            if let Some(error) = &s.failed {
                let error = error.clone();
                streams.remove(&number);
                drop(streams);
                stream.close();
                let _ = self.conn.flush();
                bail!("KWin refused {name}: {error}");
            }
            if let Some(node) = s.node {
                break node;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                streams.remove(&number);
                drop(streams);
                stream.close();
                let _ = self.conn.flush();
                bail!("timed out waiting for KWin to stream {name}");
            }
            streams = self.shared.changed.wait_timeout(streams, left).unwrap().0;
        };
        Ok(VirtualOutput {
            number,
            node,
            name,
            made,
            refresh,
            stream,
            conn: self.conn.clone(),
            shared: self.shared.clone(),
        })
    }

    /// Creates output `Virtual-Prostir-<number>` and waits for its PipeWire node.
    pub fn create(&self, spec: &VirtualSpec) -> Result<VirtualOutput> {
        let number = spec.number;
        self.shared
            .streams
            .lock()
            .unwrap()
            .insert(number, StreamState::default());
        let name = format!("Prostir-{number}");
        // The protocol calls these "logical", but KWin uses them as the output's mode
        // (pixel size); scale is applied separately through KScreen.
        let (w, h) = (spec.w as i32, spec.h as i32);
        let stream = if self.screencast.version() >= 4 {
            let description = format!("Prostir {number}");
            self.screencast.stream_virtual_output_with_description(
                name,
                description,
                w,
                h,
                1.0,
                POINTER_EMBEDDED,
                &self.qh,
                number,
            )
        } else {
            self.screencast.stream_virtual_output(
                name,
                w,
                h,
                1.0,
                POINTER_EMBEDDED,
                &self.qh,
                number,
            )
        };
        let output = self.wait_for_node(
            number,
            stream,
            format!("Virtual-Prostir-{number}"),
            Some((spec.w, spec.h)),
            u32::from(spec.refresh) * 1000,
            Duration::from_secs(10),
        )?;
        info!("Prostir-{number}: {spec}, PipeWire node {}", output.node);
        // Made anew, it has the scale KWin kept for its name: told the one wanted
        // unless it has it already.
        let listed = kscreen::geometries()
            .get(&OutputKey::new(output.output_name()))
            .map(|g| (g.w, g.h));
        if needs_scale(spec, listed) {
            kscreen::set(output.output_name(), &scale_setting(spec));
        }
        Ok(output)
    }
}

impl Drop for Screencast {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let one = 1u64;
        // SAFETY: an eventfd takes 8 bytes, which live through the call.
        unsafe { libc::write(self.wake.as_raw_fd(), (&raw const one).cast(), 8) };
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        self.screencast.destroy();
        let _ = self.conn.flush();
    }
}

/// One KWin output; dropping it removes the output.
pub struct VirtualOutput {
    pub number: u32,
    pub node: u32,
    /// KScreen/connector name: `Virtual-Prostir-N`, or e.g. `eDP-1` for a mirror.
    name: String,
    /// Pixel size KWin made it with; `None` for a mirrored real display, whose size
    /// streams do not change.
    made: Option<(u32, u32)>,
    /// Its refresh rate in mHz: what the encoder is told the frame rate is.
    refresh: u32,
    stream: ZkdeScreencastStreamUnstableV1,
    conn: Connection,
    shared: Arc<Shared>,
}

impl VirtualOutput {
    /// Name KScreen and System Settings use for this output.
    pub fn output_name(&self) -> &str {
        &self.name
    }

    pub fn closed(&self) -> bool {
        self.shared
            .streams
            .lock()
            .unwrap()
            .get(&self.number)
            .is_none_or(|s| s.closed)
    }

    /// The pixel size another program's stream gave this output, when it is no longer
    /// the size it was made with. Connecting our stream again puts that size back.
    pub fn resized(&self) -> Option<(u32, u32)> {
        let made = self.made?;
        let outputs = self.shared.outputs.lock().unwrap();
        // Made anew, the output can briefly be listed twice: the newest one is ours.
        let now = outputs
            .iter()
            .filter(|(_, (_, info))| info.name.as_deref() == Some(self.name.as_str()))
            .max_by_key(|(global, _)| **global)
            .and_then(|(_, (_, info))| info.mode);
        resized(made, now)
    }

    /// The scale KWin has the output at now, in percent, from its events (nothing is
    /// asked).
    pub fn desk_scale(&self) -> Option<u16> {
        let outputs = self.shared.outputs.lock().unwrap();
        // Made anew, the output can briefly be listed twice: the newest one is ours.
        outputs
            .iter()
            .filter(|(_, (_, info))| info.name.as_deref() == Some(self.name.as_str()))
            .max_by_key(|(global, _)| **global)
            .and_then(|(_, (_, info))| info.scale())
    }

    /// The encoder reads the stream from KWin's PipeWire node, told the output's
    /// refresh rate (see [`input_for`]).
    pub fn input(&self) -> Input {
        input_for(self.node, self.refresh)
    }

    /// Makes this the primary output, so the Plasma panel and new windows go there.
    pub fn make_primary(&self) {
        kscreen::set(self.output_name(), "priority.1");
    }
}

impl Drop for VirtualOutput {
    fn drop(&mut self) {
        self.stream.close();
        let _ = self.conn.flush();
        self.shared.streams.lock().unwrap().remove(&self.number);
        info!("{} stream closed", self.name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(width: i32, height: i32, flags: wl_output::Mode) -> wl_output::Event {
        wl_output::Event::Mode {
            flags: WEnum::Value(flags),
            width,
            height,
            refresh: 60000,
        }
    }

    fn info(events: Vec<wl_output::Event>) -> OutputInfo {
        let mut info = OutputInfo::default();
        for event in events {
            info.on_event(event);
        }
        info
    }

    #[test]
    fn the_current_mode_counts_once_done() {
        let mut out = info(vec![
            wl_output::Event::Name {
                name: "Virtual-Prostir-2".into(),
            },
            mode(
                1440,
                2560,
                wl_output::Mode::Current | wl_output::Mode::Preferred,
            ),
        ]);
        assert_eq!(out.mode, None);
        out.on_event(wl_output::Event::Done);
        assert_eq!(out.name.as_deref(), Some("Virtual-Prostir-2"));
        assert_eq!(out.mode, Some((1440, 2560)));

        // A mode that is not current changes nothing.
        out.on_event(mode(320, 240, wl_output::Mode::Preferred));
        out.on_event(wl_output::Event::Done);
        assert_eq!(out.mode, Some((1440, 2560)));

        // Another program's stream resized it.
        out.on_event(mode(320, 240, wl_output::Mode::Current));
        out.on_event(wl_output::Event::Done);
        assert_eq!(out.mode, Some((320, 240)));

        // A `done` for something else (scale, description) keeps the mode.
        out.on_event(wl_output::Event::Scale { factor: 2 });
        out.on_event(wl_output::Event::Done);
        assert_eq!(out.mode, Some((320, 240)));
    }

    /// The scale from the mode and the logical size, each counted once done: with
    /// xdg-output 3 at the output's `done`, before it at its own.
    #[test]
    fn the_scale_from_the_logical_size_once_done() {
        let mut out = info(vec![
            mode(2560, 1440, wl_output::Mode::Current),
            wl_output::Event::Done,
        ]);
        assert_eq!(out.scale(), None, "no logical size yet");
        out.on_xdg_event(&zxdg_output_v1::Event::LogicalSize {
            width: 2048,
            height: 1152,
        });
        assert_eq!(out.scale(), None, "not done");
        out.on_event(wl_output::Event::Done);
        assert_eq!(out.scale(), Some(125));
        out.on_xdg_event(&zxdg_output_v1::Event::LogicalSize {
            width: 2560,
            height: 1440,
        });
        out.on_xdg_event(&zxdg_output_v1::Event::Done);
        assert_eq!(out.scale(), Some(100));
    }

    #[test]
    fn the_frame_rate_of_a_refresh_rate() {
        let rate = |n, d| gstreamer::Fraction::new(n, d);
        assert_eq!(frame_rate(60_000), rate(60, 1));
        assert_eq!(frame_rate(144_000), rate(144, 1));
        assert_eq!(frame_rate(59_940), rate(2997, 50));
        assert_eq!(frame_rate(0), rate(60, 1));
        assert_eq!(input_for(42, 144_000).frame_rate, Some(rate(144, 1)));
        let mut out = info(vec![mode(2560, 1440, wl_output::Mode::Current)]);
        assert_eq!(out.refresh, None, "not done");
        out.on_event(wl_output::Event::Done);
        assert_eq!(out.refresh, Some(60_000));
    }

    /// `pipewiresrc` offers PipeWire the formats the elements after it take, as they
    /// answer its caps query; told "any" (by `capssetter` after it, as once), it
    /// offers no format, WirePlumber links it to nothing ("target not found") and
    /// its start hangs. Skipped without gst-plugin-pipewire; nothing connects to
    /// PipeWire (the pipeline is never started).
    #[test]
    fn pipewire_is_offered_the_formats_the_encoder_takes() {
        use gstreamer::prelude::*;
        gstreamer::init().unwrap();
        if gstreamer::ElementFactory::find("pipewiresrc").is_none() {
            eprintln!("skipped: no pipewiresrc (gst-plugin-pipewire)");
            return;
        }
        let input = input_for(42, 60_000);
        let pipeline = gstreamer::parse::launch(&format!(
            "{} ! queue ! video/x-raw,format=I420 ! fakesink",
            input.source
        ))
        .unwrap()
        .downcast::<gstreamer::Bin>()
        .unwrap();
        let source = pipeline
            .iterate_sources()
            .into_iter()
            .filter_map(Result::ok)
            .find(|e| e.factory().is_some_and(|f| f.name() == "pipewiresrc"))
            .expect("a pipewiresrc");
        let offered = source.static_pad("src").unwrap().peer_query_caps(None);
        assert!(
            !offered.is_any(),
            "the encoder's formats are hidden: {offered}"
        );
        let takes = gstreamer::Caps::builder("video/x-raw")
            .field("format", "I420")
            .build();
        assert!(offered.is_subset(&takes), "{offered}");
    }

    /// A PipeWire video node of a test's own, giving frames as `caps` say (a test
    /// picture, not the desktop), and its id; it goes with the pipeline.
    fn test_node(caps: &str) -> (gstreamer::Element, u32) {
        use gstreamer::prelude::*;
        gstreamer::init().unwrap();
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let name = format!(
            "prostir-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        );
        let provider = gstreamer::parse::launch(&format!(
            "videotestsrc is-live=true pattern=ball ! {caps} ! pipewiresink mode=provide \
             stream-properties=\"props,node.name={name},media.class=Video/Source\""
        ))
        .unwrap();
        provider.set_state(gstreamer::State::Playing).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let listed = std::process::Command::new("pw-cli")
                .args(["ls", "Node"])
                .output()
                .unwrap();
            let listed = String::from_utf8_lossy(&listed.stdout);
            let mut id = None;
            for line in listed.lines().map(str::trim) {
                if let Some(rest) = line.strip_prefix("id ") {
                    id = rest.split(',').next().and_then(|n| n.parse::<u32>().ok());
                }
                if line == format!("node.name = \"{name}\"")
                    && let Some(id) = id
                {
                    return (provider, id);
                }
            }
            assert!(Instant::now() < deadline, "no node {name}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// What an encoder of the KDE input on node `node` gave in `time`: each frame's
    /// way through the encoder, and a failure if one came.
    fn encode_node(
        node: u32,
        backend: crate::encoder::Backend,
        codec: prostir_proto::Codec,
        time: Duration,
    ) -> (Vec<crate::encoder::FrameTiming>, Option<String>) {
        use std::sync::mpsc;

        use crate::encoder::{Encoder, EncoderConfig, EncoderEvent, GpuDevices};

        let frames = Arc::new(Mutex::new(Vec::new()));
        let (failed, failure) = mpsc::channel();
        let cfg = EncoderConfig {
            codec,
            bitrate_kbps: 8000,
            backend,
            sharp: false,
            rows_up: false,
        };
        let kept = frames.clone();
        let encoder = Encoder::start(
            &input_for(node, 60_000),
            &cfg,
            None,
            &GpuDevices::default(),
            move |event| {
                match event {
                    EncoderEvent::Frame { timing, .. } => {
                        kept.lock().unwrap().extend(timing);
                    }
                    EncoderEvent::Failed(why) | EncoderEvent::SourceLost(why) => {
                        let _ = failed.send(why);
                    }
                    EncoderEvent::Unopened(not) => {
                        let _ = failed.send(not.why);
                    }
                    EncoderEvent::Size { .. } => {}
                }
                false
            },
        )
        .unwrap();
        let why = failure.recv_timeout(time).ok();
        drop(encoder);
        let frames = frames.lock().unwrap().clone();
        (frames, why)
    }

    /// KWin's stream as the encoder reads it, from a PipeWire node made here: it is
    /// linked and encoded. Run by hand:
    /// `cargo test -p prostir -- --ignored pipewire_node_is_encoded`; needs PipeWire,
    /// WirePlumber, `pw-cli`, gst-plugin-pipewire and x264.
    #[test]
    #[ignore = "by hand: makes a PipeWire video node of its own"]
    fn a_pipewire_node_is_encoded() {
        use gstreamer::prelude::*;
        let (provider, node) =
            test_node("video/x-raw,format=BGRx,width=640,height=360,framerate=30/1");
        let (frames, why) = encode_node(
            node,
            crate::encoder::Backend::X264,
            prostir_proto::Codec::H264,
            Duration::from_secs(3),
        );
        provider.set_state(gstreamer::State::Null).unwrap();
        assert_eq!(why, None, "the encoder failed");
        assert!(frames.len() >= 10, "{} frames in 3 s", frames.len());
    }

    /// A still screen, a new picture every 2 s from a PipeWire node made here: VA-API
    /// gives each out at once, not when the next comes in 2 s later, and no frame is
    /// made twice. It keeps frames back for a source that is not live, and
    /// `pipewiresrc` says it is only once its stream is up, after VA-API asked: told
    /// it is (see [`crate::encoder::tell_live`]). The time is from the frame entering
    /// the converter to leaving the parser; without it, 4 s (it kept two). Run by hand:
    /// `cargo test -p prostir -- --ignored a_still_screens_picture --nocapture`; needs
    /// PipeWire, WirePlumber, `pw-cli`, gst-plugin-pipewire and VA-API.
    #[test]
    #[ignore = "by hand: makes a PipeWire video node of its own; needs VA-API"]
    fn a_still_screens_picture_is_not_kept_back() {
        use gstreamer::prelude::*;
        let (provider, node) =
            test_node("video/x-raw,format=NV12,width=1280,height=720,framerate=1/2");
        let (frames, why) = encode_node(
            node,
            crate::encoder::Backend::Va,
            prostir_proto::Codec::H265,
            Duration::from_secs(11),
        );
        provider.set_state(gstreamer::State::Null).unwrap();
        assert_eq!(why, None, "the encoder failed");
        let new: Vec<u32> = frames
            .iter()
            .filter(|t| !t.repeat)
            .map(|t| t.encode_us / 1000)
            .collect();
        let again = frames.iter().filter(|t| t.repeat).count();
        eprintln!("new pictures through the encoder in ms: {new:?}; repeats: {again}");
        assert!(new.len() >= 4, "{new:?}");
        // The first opens the encoder.
        assert!(new[1..].iter().all(|ms| *ms < 100), "{new:?}");
        // A repeat is only PipeWire's keepalive (2 s after a picture, as often as the
        // pictures come), none made here.
        assert!(again <= new.len(), "{again} repeats");
    }

    /// KWin's node gone: the encoder's start says its source failed at once (not after
    /// the 10 s limit), links to no other source, and is dropped at once, though
    /// `pipewiresrc` takes 30 s to stop. Run by hand:
    /// `cargo test -p prostir -- --ignored a_gone_node`; needs PipeWire, WirePlumber
    /// and gst-plugin-pipewire.
    #[test]
    #[ignore = "by hand: asks PipeWire for a node that is not there"]
    fn a_gone_node_fails_at_once() {
        use crate::encoder::{
            Backend, Encoder, EncoderConfig, EncoderEvent, GpuDevices, SourceFailed,
        };
        gstreamer::init().unwrap();
        let cfg = EncoderConfig {
            codec: prostir_proto::Codec::H264,
            bitrate_kbps: 4000,
            backend: Backend::X264,
            sharp: false,
            rows_up: false,
        };
        let told = Arc::new(Mutex::new(Vec::new()));
        let kept = told.clone();
        let started = Instant::now();
        let result = Encoder::start(
            &input_for(4_000_000, 60_000),
            &cfg,
            None,
            &GpuDevices::default(),
            move |event| {
                if let EncoderEvent::Failed(why) = event {
                    kept.lock().unwrap().push(why);
                }
                false
            },
        );
        let took = started.elapsed();
        let Err(e) = result else {
            panic!("started on a node that is not there")
        };
        eprintln!("failed after {took:?}: {e:#}");
        let failed = e.downcast_ref::<SourceFailed>().expect("the source failed");
        assert!(failed.0.contains("target not found"), "{failed}");
        assert!(took < Duration::from_secs(3), "{took:?}");
        // Its stop is left to a thread of its own.
        assert!(started.elapsed() < Duration::from_secs(4));
        assert!(told.lock().unwrap().is_empty(), "not told as the encoder's");
    }

    /// The thread reading KWin's events sleeps while nothing is said (no wake-ups),
    /// wakes at once when the connection has something to read, without waiting for
    /// the wake-up file, and when that one is written to; a socket stands in for the
    /// connection.
    #[test]
    fn kwins_events_are_read_at_once_and_nothing_else_wakes_the_reader() {
        use std::io::Write as _;
        use std::os::unix::net::UnixStream;
        use std::sync::mpsc;

        // SAFETY: plain call; the descriptor is checked below.
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC) };
        assert!(fd >= 0, "eventfd: {}", std::io::Error::last_os_error());
        // SAFETY: a new eventfd, owned by nobody else.
        let wake = Arc::new(unsafe { OwnedFd::from_raw_fd(fd) });
        let (mut kwin, connection) = UnixStream::pair().unwrap();
        let sleeper = |connection: UnixStream, wake: Arc<OwnedFd>| {
            let (done, woke) = mpsc::channel();
            std::thread::spawn(move || {
                let said = sleep_until_said(connection.as_fd(), wake.as_fd());
                let _ = done.send(said);
                connection
            });
            woke
        };

        let woke = sleeper(connection.try_clone().unwrap(), wake.clone());
        assert!(
            woke.recv_timeout(Duration::from_millis(300)).is_err(),
            "woken with nothing said"
        );
        let said = Instant::now();
        kwin.write_all(b"x").unwrap();
        assert_eq!(woke.recv_timeout(Duration::from_secs(2)), Ok(true));
        assert!(
            said.elapsed() < Duration::from_millis(100),
            "{:?}",
            said.elapsed()
        );

        // Read, so only the wake-up file is left to wake it.
        let mut byte = [0u8];
        std::io::Read::read_exact(&mut &connection, &mut byte).unwrap();
        let woke = sleeper(connection.try_clone().unwrap(), wake.clone());
        assert!(woke.recv_timeout(Duration::from_millis(200)).is_err());
        let one = 1u64;
        // SAFETY: an eventfd takes 8 bytes, which live through the call.
        let wrote = unsafe { libc::write(wake.as_raw_fd(), (&raw const one).cast(), 8) };
        assert_eq!(wrote, 8, "{}", std::io::Error::last_os_error());
        assert_eq!(woke.recv_timeout(Duration::from_secs(2)), Ok(false));
    }

    #[test]
    fn the_scale_setting() {
        let spec = |scale| VirtualSpec {
            id: crate::screen::ScreenId::saved(3),
            number: 4,
            w: 2560,
            h: 1440,
            scale,
            refresh: 60,
        };
        assert_eq!(scale_setting(&spec(100)), "scale.1");
        assert_eq!(scale_setting(&spec(125)), "scale.1.25");
        assert_eq!(scale_setting(&spec(150)), "scale.1.5");
    }

    #[test]
    fn only_a_size_other_than_the_created_one_counts() {
        let made = (1440, 2560);
        // Not described yet, or the first mode event: the size it was made with.
        assert_eq!(resized(made, None), None);
        assert_eq!(resized(made, Some((1440, 2560))), None);
        assert_eq!(resized(made, Some((320, 240))), Some((320, 240)));
        assert_eq!(resized(made, Some((2560, 1440))), Some((2560, 1440)));
    }
}
