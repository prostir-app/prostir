//! Virtual input over libei's protocol, as a sender (the `reis` crate): the devices
//! the desktop's EIS gives (KWin and Mutter through the RemoteDesktop portal), each
//! event in a frame of its own.
//!
//! The desktop decides which devices there are: one with everything, or one for each
//! of the mouse, the keyboard and touch. An event goes to a resumed device that has
//! what it needs; with none (no touch screen, say) it goes nowhere, which is no
//! failure. Positions are the desktop's logical pixels, as its regions are.

use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use log::{debug, info};
use reis::PendingRequestResult;
use reis::ei::{self, button::ButtonState, keyboard::KeyState};
use reis::event::{DeviceCapability, EiEvent, EiEventConverter};

use crate::platform::{Injector, InputStopped, MouseButton, Spot, TouchPhase};

/// A device of the desktop and whether it takes events now.
struct Dev {
    device: reis::event::Device,
    resumed: bool,
    /// Started emulating since it was last resumed.
    emulating: bool,
}

/// A connection to the desktop's EIS as a sender.
pub struct Ei {
    context: ei::Context,
    converter: EiEventConverter,
    devices: Vec<Dev>,
    sequence: u32,
    /// Said once in the log: an event no device takes.
    missing: Vec<DeviceCapability>,
}

// SAFETY: `EiEventConverter` is not `Send` only for the callbacks of its
// `add_callback_handler` (boxed closures that need not be `Send`), which this never
// adds: what it holds besides is behind `Arc` and `Mutex`. `Ei` is made on the thread
// that opens the input, then used on the input thread alone.
unsafe impl Send for Ei {}

/// Waits up to `limit` for `fd` to have something to read.
fn readable(fd: &impl AsRawFd, limit: Duration) -> bool {
    let mut poll = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let ms = i32::try_from(limit.as_millis()).unwrap_or(i32::MAX);
    // SAFETY: one valid pollfd, for the length given.
    unsafe { libc::poll(&raw mut poll, 1, ms) > 0 }
}

/// The handshake as a sender, given up at `until`.
fn handshake(context: &ei::Context, until: Instant) -> Result<reis::handshake::HandshakeResp> {
    let mut handshaker =
        reis::handshake::EiHandshaker::new("Prostir", ei::handshake::ContextType::Sender);
    loop {
        let left = until.saturating_duration_since(Instant::now());
        anyhow::ensure!(
            !left.is_zero() && readable(context, left),
            "libei: the desktop did not answer the handshake"
        );
        match context.read() {
            Ok(_) => {}
            Err(e) => return Err(e).context("libei: the handshake"),
        }
        while let Some(result) = context.pending_event() {
            match result {
                PendingRequestResult::Request(event) => {
                    if let Some(resp) = handshaker
                        .handle_event(event)
                        .map_err(|e| anyhow::anyhow!("libei: the handshake: {e}"))?
                    {
                        return Ok(resp);
                    }
                }
                PendingRequestResult::ParseError(e) => bail!("libei: the handshake: {e}"),
                PendingRequestResult::InvalidObject(_) => {}
            }
        }
    }
}

/// Now on the monotonic clock in microseconds, as libei's frames take it.
fn now_us() -> u64 {
    let mut t = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: a valid timespec to fill.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut t) };
    t.tv_sec as u64 * 1_000_000 + t.tv_nsec as u64 / 1_000
}

impl Ei {
    /// Connects over `socket` (from the portal's `ConnectToEIS`), and waits up to
    /// `limit` for a device that moves the mouse.
    pub fn connect(socket: UnixStream, limit: Duration) -> Result<Self> {
        let context = ei::Context::new(socket).context("libei: the socket")?;
        let until = Instant::now() + limit;
        let resp = handshake(&context, until)?;
        let converter = EiEventConverter::new(&context, resp);
        let mut ei = Self {
            context,
            converter,
            devices: Vec::new(),
            sequence: 0,
            missing: Vec::new(),
        };
        // What came with the handshake is read already: taken before waiting for more.
        loop {
            ei.read()?;
            if ei.has(DeviceCapability::PointerAbsolute) {
                break;
            }
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                bail!("libei: the desktop gave no device that moves the mouse");
            }
            readable(&ei.context, left);
        }
        info!(
            "libei: {} device(s); touch: {}",
            ei.devices.len(),
            ei.has(DeviceCapability::Touch)
        );
        Ok(ei)
    }

    fn has(&self, cap: DeviceCapability) -> bool {
        self.devices
            .iter()
            .any(|d| d.resumed && d.device.has_capability(cap))
    }

    /// Reads what the desktop said, without waiting.
    fn read(&mut self) -> Result<()> {
        match self.context.read() {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Err(anyhow::Error::new(InputStopped)
                    .context("libei: the desktop closed the connection"));
            }
            Err(e) => return Err(e).context("libei: reading"),
        }
        while let Some(result) = self.context.pending_event() {
            match result {
                PendingRequestResult::Request(event) => self
                    .converter
                    .handle_event(event)
                    .map_err(|e| anyhow::anyhow!("libei: {e}"))?,
                PendingRequestResult::ParseError(e) => bail!("libei: {e}"),
                PendingRequestResult::InvalidObject(_) => {}
            }
        }
        while let Some(event) = self.converter.next_event() {
            self.on(event)?;
        }
        self.context.flush().context("libei: sending")?;
        Ok(())
    }

    fn on(&mut self, event: EiEvent) -> Result<()> {
        match event {
            EiEvent::SeatAdded(s) => s.seat.bind_capabilities(
                DeviceCapability::Pointer
                    | DeviceCapability::PointerAbsolute
                    | DeviceCapability::Button
                    | DeviceCapability::Scroll
                    | DeviceCapability::Keyboard
                    | DeviceCapability::Touch,
            ),
            EiEvent::DeviceAdded(d) => self.devices.push(Dev {
                device: d.device,
                resumed: false,
                emulating: false,
            }),
            EiEvent::DeviceResumed(d) => self.mark(&d.device, true),
            EiEvent::DevicePaused(d) => self.mark(&d.device, false),
            EiEvent::DeviceRemoved(d) => self.devices.retain(|x| x.device != d.device),
            EiEvent::Disconnected(d) => {
                return Err(anyhow::Error::new(InputStopped).context(format!(
                    "libei: the desktop disconnected ({:?}{})",
                    d.reason,
                    d.explanation.map(|e| format!(": {e}")).unwrap_or_default()
                )));
            }
            _ => {}
        }
        Ok(())
    }

    fn mark(&mut self, device: &reis::event::Device, resumed: bool) {
        for d in self.devices.iter_mut().filter(|d| d.device == *device) {
            d.resumed = resumed;
            d.emulating = false;
        }
    }

    /// The device to send an event needing `cap` to, emulating; none where no device
    /// takes it now.
    fn device(&mut self, cap: DeviceCapability) -> Option<reis::event::Device> {
        let serial = self.converter.connection().serial();
        let Some(d) = self
            .devices
            .iter_mut()
            .find(|d| d.resumed && d.device.has_capability(cap))
        else {
            if !self.missing.contains(&cap) {
                self.missing.push(cap);
                info!("libei: no device of the desktop takes {cap:?} now");
            }
            return None;
        };
        if !d.emulating {
            d.device.device().start_emulating(serial, self.sequence);
            self.sequence = self.sequence.wrapping_add(1);
            d.emulating = true;
        }
        Some(d.device.clone())
    }

    /// Ends the frame of `device` and sends it.
    fn frame(&self, device: &reis::event::Device) -> Result<()> {
        let serial = self.converter.connection().serial();
        device.device().frame(serial, now_us());
        self.context.flush().context("libei: sending")?;
        Ok(())
    }

    /// Sends what `f` makes of the interface `T` of a device with `cap`.
    fn send<T: ei::Interface>(&mut self, cap: DeviceCapability, f: impl FnOnce(&T)) -> Result<()> {
        let Some(device) = self.device(cap) else {
            return Ok(());
        };
        let Some(interface) = device.interface::<T>() else {
            debug!("libei: the device has no {}", T::NAME);
            return Ok(());
        };
        f(&interface);
        self.frame(&device)
    }
}

impl Injector for Ei {
    fn touches(&self) -> bool {
        self.has(DeviceCapability::Touch)
    }

    fn pointer_to(&mut self, at: &Spot) -> Result<()> {
        self.send(
            DeviceCapability::PointerAbsolute,
            |p: &ei::PointerAbsolute| {
                p.motion_absolute(at.x as f32, at.y as f32);
            },
        )
    }

    fn pointer_by(&mut self, dx: f64, dy: f64) -> Result<()> {
        self.send(DeviceCapability::Pointer, |p: &ei::Pointer| {
            p.motion_relative(dx as f32, dy as f32);
        })
    }

    fn button(&mut self, button: MouseButton, down: bool) -> Result<()> {
        let state = if down {
            ButtonState::Press
        } else {
            ButtonState::Released
        };
        self.send(DeviceCapability::Button, |b: &ei::Button| {
            b.button(u32::from(button.evdev()), state);
        })
    }

    fn scroll(&mut self, dx: i32, dy: i32) -> Result<()> {
        self.send(DeviceCapability::Scroll, |s: &ei::Scroll| {
            s.scroll_discrete(dx, dy);
        })
    }

    fn touch(&mut self, id: u32, phase: TouchPhase, at: &Spot) -> Result<()> {
        let (x, y) = (at.x as f32, at.y as f32);
        self.send(DeviceCapability::Touch, |t: &ei::Touchscreen| match phase {
            TouchPhase::Down => t.down(id, x, y),
            TouchPhase::Move => t.motion(id, x, y),
            TouchPhase::Up => t.up(id),
            // Cancelling came with version 2; before it, lifted.
            TouchPhase::Cancel if t.version() >= 2 => t.cancel(id),
            TouchPhase::Cancel => t.up(id),
        })
    }

    fn key(&mut self, code: u16, down: bool) -> Result<()> {
        let state = if down {
            KeyState::Press
        } else {
            KeyState::Released
        };
        self.send(DeviceCapability::Keyboard, |k: &ei::Keyboard| {
            k.key(u32::from(code), state);
        })
    }

    fn pump(&mut self) -> Result<()> {
        self.read()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::OutputKey;
    use prostir_proto::state::DeskRect;
    use reis::eis;
    use reis::request::{EisRequest, EisRequestConverter};

    /// A desktop's EIS in a thread: a seat with everything, and on bind a device for
    /// the mouse and one for touch (as KWin gives them), each resumed at once. What it
    /// was sent, as text, once the sender is gone.
    fn desktop(socket: UnixStream) -> std::thread::JoinHandle<Vec<String>> {
        std::thread::spawn(move || {
            let context = eis::Context::new(socket).unwrap();
            let mut handshaker = reis::handshake::EisHandshaker::new(&context, 1);
            let resp = 'shake: loop {
                assert!(readable(&context, Duration::from_secs(5)));
                context.read().unwrap();
                while let Some(PendingRequestResult::Request(r)) = context.pending_request() {
                    if let Some(resp) = handshaker.handle_request(r).unwrap() {
                        break 'shake resp;
                    }
                }
            };
            let mut converter = EisRequestConverter::new(&context, resp, 1);
            let connection = converter.handle().clone();
            let _seat = connection.add_seat(
                Some("default"),
                DeviceCapability::Pointer
                    | DeviceCapability::PointerAbsolute
                    | DeviceCapability::Button
                    | DeviceCapability::Scroll
                    | DeviceCapability::Keyboard
                    | DeviceCapability::Touch,
            );
            connection.flush().unwrap();
            let mut heard = Vec::new();
            loop {
                readable(&context, Duration::from_secs(5));
                if context.read().is_err() {
                    return heard;
                }
                while let Some(PendingRequestResult::Request(r)) = context.pending_request() {
                    converter.handle_request(r).unwrap();
                }
                while let Some(request) = converter.next_request() {
                    match request {
                        EisRequest::Bind(b) => {
                            for caps in [
                                DeviceCapability::PointerAbsolute
                                    | DeviceCapability::Button
                                    | DeviceCapability::Scroll,
                                DeviceCapability::Touch.into(),
                            ] {
                                let d = b.seat.add_device(
                                    Some("prostir-test"),
                                    eis::device::DeviceType::Virtual,
                                    caps,
                                    |_| {},
                                );
                                d.resumed();
                            }
                        }
                        EisRequest::PointerMotionAbsolute(m) => {
                            heard.push(format!("to {},{}", m.dx_absolute, m.dy_absolute));
                        }
                        EisRequest::Button(b) => {
                            heard.push(format!("button {:#x} {:?}", b.button, b.state));
                        }
                        EisRequest::ScrollDiscrete(s) => {
                            heard.push(format!("scroll {},{}", s.discrete_dx, s.discrete_dy));
                        }
                        EisRequest::TouchDown(t) => {
                            heard.push(format!("touch {} down {},{}", t.touch_id, t.x, t.y));
                        }
                        EisRequest::TouchCancel(t) => {
                            heard.push(format!("touch {} cancel", t.touch_id));
                        }
                        EisRequest::KeyboardKey(k) => heard.push(format!("key {}", k.key)),
                        _ => {}
                    }
                }
                connection.flush().unwrap();
            }
        })
    }

    fn at(x: f64, y: f64) -> Spot {
        let rect = DeskRect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        };
        Spot {
            output: OutputKey::new("Virtual-Prostir-1"),
            rect,
            desk: rect,
            x,
            y,
        }
    }

    /// A sender reaches the desktop's devices: the mouse in logical pixels, a button by
    /// its evdev code, the wheel in 1/120 of a notch, a finger down and cancelled. A
    /// key, with no keyboard among the devices, goes nowhere and fails nothing.
    #[test]
    fn events_reach_the_desktops_devices() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let desktop = desktop(theirs);
        let mut ei = Ei::connect(ours, Duration::from_secs(5)).unwrap();
        // The touch device may come a moment after the mouse.
        let until = Instant::now() + Duration::from_secs(5);
        while !ei.touches() && Instant::now() < until {
            readable(&ei.context, Duration::from_millis(50));
            ei.pump().unwrap();
        }
        assert!(ei.touches());
        ei.pointer_to(&at(100.5, 50.0)).unwrap();
        ei.button(MouseButton::Left, true).unwrap();
        ei.button(MouseButton::Left, false).unwrap();
        ei.scroll(0, -240).unwrap();
        ei.touch(3, TouchPhase::Down, &at(10.0, 20.0)).unwrap();
        ei.touch(3, TouchPhase::Cancel, &at(10.0, 20.0)).unwrap();
        ei.key(30, true).unwrap();
        drop(ei);
        assert_eq!(
            desktop.join().unwrap(),
            [
                "to 100.5,50",
                "button 0x110 Press",
                "button 0x110 Released",
                "scroll 0,-240",
                "touch 3 down 10,20",
                "touch 3 cancel",
            ]
        );
    }
}
