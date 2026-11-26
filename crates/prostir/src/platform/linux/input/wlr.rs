//! Virtual input on Hyprland and other wlroots desktops: `zwlr_virtual_pointer_v1`
//! for the mouse (one pointer for each output, so a point is given across its output
//! and needs no layout) and `zwp_virtual_keyboard_v1` for keys, with the keyboard
//! layout the desktop gives its own seat. They have no touch: the core makes mouse
//! actions of touches. The wheel goes in whole notches (the protocol has no finer
//! steps); what is left of one waits for the next.

use std::collections::HashMap;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use log::info;
use wayland_client::globals::{GlobalList, GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_keyboard, wl_output, wl_pointer, wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, delegate_noop};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
};

use crate::platform::{Injector, MouseButton, OpenInput, OutputKey, Spot, TouchPhase};

/// The extent a point across an output is given in.
const EXTENT: u32 = 65535;
/// A notch of the wheel as wlroots counts it in `axis` (as a mouse wheel's).
const NOTCH_VALUE: f64 = 15.0;

/// Whether the desktop offers virtual pointers (the keyboard is not needed to say yes).
pub fn available() -> bool {
    let Ok(conn) = Connection::connect_to_env() else {
        return false;
    };
    let Ok((globals, _queue)) = registry_queue_init::<State>(&conn) else {
        return false;
    };
    globals.contents().with_list(|l| {
        l.iter()
            .any(|g| g.interface == "zwlr_virtual_pointer_manager_v1")
    })
}

#[derive(Default)]
struct State {
    /// Outputs by their global's name: the output and the connector name it said.
    outputs: HashMap<u32, (wl_output::WlOutput, Option<String>)>,
    /// The keyboard layout the desktop gave the seat's keyboard.
    keymap: Option<(u32, OwnedFd, u32)>,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
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
        if let wl_output::Event::Name { name } = event
            && let Some(entry) = state.outputs.get_mut(global)
        {
            entry.1 = Some(name);
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Keymap { format, fd, size } = event {
            let format = match format {
                WEnum::Value(f) => f as u32,
                WEnum::Unknown(f) => f,
            };
            state.keymap = Some((format, fd, size));
        }
    }
}

delegate_noop!(State: ignore wl_seat::WlSeat);
delegate_noop!(State: ignore ZwlrVirtualPointerManagerV1);
delegate_noop!(State: ignore ZwlrVirtualPointerV1);
delegate_noop!(State: ignore ZwpVirtualKeyboardManagerV1);
delegate_noop!(State: ignore ZwpVirtualKeyboardV1);

/// Opens the virtual pointer and keyboard.
pub struct WlrOpen;

impl OpenInput for WlrOpen {
    fn open(&self) -> Result<Box<dyn Injector>> {
        let conn = Connection::connect_to_env().context("no Wayland display")?;
        let (globals, mut queue) = registry_queue_init::<State>(&conn)?;
        let qh = queue.handle();
        let pointers: ZwlrVirtualPointerManagerV1 = globals
            .bind(&qh, 1..=2, ())
            .context("the desktop has no virtual pointers")?;
        let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=7, ()).context("no seat")?;
        let keyboards: Option<ZwpVirtualKeyboardManagerV1> = globals.bind(&qh, 1..=1, ()).ok();
        let mut state = State::default();
        bind_outputs(&globals, &qh, &mut state);
        queue.roundtrip(&mut state)?;
        info!("input: through wlroots' virtual pointer");
        Ok(Box::new(Wlr {
            conn,
            globals,
            queue,
            state,
            pointers,
            seat,
            keyboards,
            by_output: HashMap::new(),
            plain: None,
            last: None,
            keyboard: None,
            down_on: HashMap::new(),
            wheel: (0, 0),
            started: Instant::now(),
        }))
    }
}

fn bind_outputs(globals: &GlobalList, qh: &QueueHandle<State>, state: &mut State) {
    for g in globals.contents().clone_list() {
        if g.interface == "wl_output" && g.version >= 4 && !state.outputs.contains_key(&g.name) {
            let output = globals
                .registry()
                .bind::<wl_output::WlOutput, _, _>(g.name, 4, qh, g.name);
            state.outputs.insert(g.name, (output, None));
        }
    }
}

struct Wlr {
    conn: Connection,
    globals: GlobalList,
    queue: EventQueue<State>,
    state: State,
    pointers: ZwlrVirtualPointerManagerV1,
    seat: wl_seat::WlSeat,
    keyboards: Option<ZwpVirtualKeyboardManagerV1>,
    /// A pointer for each output, made when the mouse first goes there.
    by_output: HashMap<OutputKey, ZwlrVirtualPointerV1>,
    /// One for the whole desktop, where the desktop cannot make one for an output.
    plain: Option<ZwlrVirtualPointerV1>,
    /// The pointer that moved last: buttons and the wheel go through it.
    last: Option<ZwlrVirtualPointerV1>,
    keyboard: Option<ZwpVirtualKeyboardV1>,
    /// The pointer each button held went down on.
    down_on: HashMap<MouseButton, ZwlrVirtualPointerV1>,
    /// Parts of a notch of the wheel not sent yet, in 1/120 of one.
    wheel: (i32, i32),
    started: Instant,
}

impl Wlr {
    /// The time of an event, in milliseconds.
    fn time(&self) -> u32 {
        self.started.elapsed().as_millis() as u32
    }

    fn flush(&self) -> Result<()> {
        self.conn.flush().context("Wayland: sending")
    }

    /// The pointer for `output`, made if needed: one of its own where the desktop can.
    fn pointer_for(&mut self, output: &OutputKey) -> Result<(ZwlrVirtualPointerV1, bool)> {
        if let Some(p) = self.by_output.get(output) {
            return Ok((p.clone(), true));
        }
        let qh = self.queue.handle();
        if self.pointers.version() >= 2 {
            let mut found = self.find_output(output);
            if found.is_none() {
                // An output made since: bound now.
                self.queue.roundtrip(&mut self.state)?;
                bind_outputs(&self.globals, &qh, &mut self.state);
                self.queue.roundtrip(&mut self.state)?;
                found = self.find_output(output);
            }
            if let Some(o) = found {
                let p = self.pointers.create_virtual_pointer_with_output(
                    Some(&self.seat),
                    Some(&o),
                    &qh,
                    (),
                );
                self.by_output.insert(output.clone(), p.clone());
                return Ok((p, true));
            }
        }
        let p = self
            .plain
            .get_or_insert_with(|| {
                self.pointers
                    .create_virtual_pointer(Some(&self.seat), &qh, ())
            })
            .clone();
        Ok((p, false))
    }

    fn find_output(&self, output: &OutputKey) -> Option<wl_output::WlOutput> {
        self.state
            .outputs
            .values()
            .find(|(_, n)| n.as_deref() == Some(output.as_str()))
            .map(|(o, _)| o.clone())
    }

    /// The pointer that moved last; before any moved, the one for the whole desktop.
    fn last(&mut self) -> ZwlrVirtualPointerV1 {
        if let Some(p) = &self.last {
            return p.clone();
        }
        let qh = self.queue.handle();
        let p = self
            .plain
            .get_or_insert_with(|| {
                self.pointers
                    .create_virtual_pointer(Some(&self.seat), &qh, ())
            })
            .clone();
        self.last = Some(p.clone());
        p
    }

    /// The virtual keyboard, made with the seat's layout on first use.
    fn keyboard(&mut self) -> Result<ZwpVirtualKeyboardV1> {
        if let Some(k) = &self.keyboard {
            return Ok(k.clone());
        }
        let manager = self
            .keyboards
            .clone()
            .context("the desktop has no virtual keyboards")?;
        let qh = self.queue.handle();
        let real = self.seat.get_keyboard(&qh, ());
        let until = Instant::now() + Duration::from_secs(2);
        while self.state.keymap.is_none() && Instant::now() < until {
            self.queue.roundtrip(&mut self.state)?;
        }
        if real.version() >= 3 {
            real.release();
        }
        let (format, fd, size) = self
            .state
            .keymap
            .take()
            .context("the desktop gave no keyboard layout")?;
        let k = manager.create_virtual_keyboard(&self.seat, &qh, ());
        k.keymap(format, fd.as_fd(), size);
        self.keyboard = Some(k.clone());
        Ok(k)
    }
}

impl Injector for Wlr {
    fn touches(&self) -> bool {
        false
    }

    fn pointer_to(&mut self, at: &Spot) -> Result<()> {
        let (p, own) = self.pointer_for(&at.output)?;
        let (fx, fy) = if own {
            at.across()
        } else {
            // Across the whole desktop.
            let whole = Spot {
                rect: at.desk,
                ..at.clone()
            };
            whole.across()
        };
        let x = (fx * f64::from(EXTENT)).round() as u32;
        let y = (fy * f64::from(EXTENT)).round() as u32;
        p.motion_absolute(self.time(), x, y, EXTENT, EXTENT);
        p.frame();
        self.last = Some(p);
        self.flush()
    }

    fn pointer_by(&mut self, dx: f64, dy: f64) -> Result<()> {
        let p = self.last();
        p.motion(self.time(), dx, dy);
        p.frame();
        self.flush()
    }

    /// Let go on the pointer it went down on (the mouse may have moved to another
    /// output's pointer meanwhile).
    fn button(&mut self, button: MouseButton, down: bool) -> Result<()> {
        let p = if down {
            let p = self.last();
            self.down_on.insert(button, p.clone());
            p
        } else {
            match self.down_on.remove(&button) {
                Some(p) => p,
                None => self.last(),
            }
        };
        let state = if down {
            wl_pointer::ButtonState::Pressed
        } else {
            wl_pointer::ButtonState::Released
        };
        p.button(self.time(), u32::from(button.evdev()), state);
        p.frame();
        self.flush()
    }

    fn scroll(&mut self, dx: i32, dy: i32) -> Result<()> {
        let p = self.last();
        self.wheel = (
            self.wheel.0.saturating_add(dx),
            self.wheel.1.saturating_add(dy),
        );
        let notch = prostir_proto::input::SCROLL_NOTCH;
        let whole = (self.wheel.0 / notch, self.wheel.1 / notch);
        if whole == (0, 0) {
            return Ok(());
        }
        self.wheel = (self.wheel.0 % notch, self.wheel.1 % notch);
        let time = self.time();
        p.axis_source(wl_pointer::AxisSource::Wheel);
        for (axis, n) in [
            (wl_pointer::Axis::HorizontalScroll, whole.0),
            (wl_pointer::Axis::VerticalScroll, whole.1),
        ] {
            if n != 0 {
                p.axis_discrete(time, axis, NOTCH_VALUE * f64::from(n), n);
            }
        }
        p.frame();
        self.flush()
    }

    /// Never called: it has no touch ([`Injector::touches`]).
    fn touch(&mut self, _id: u32, _phase: TouchPhase, _at: &Spot) -> Result<()> {
        Ok(())
    }

    fn key(&mut self, code: u16, down: bool) -> Result<()> {
        let k = self.keyboard()?;
        k.key(self.time(), u32::from(code), u32::from(down));
        self.flush()
    }

    /// Reads what the desktop said (an error closes the connection).
    fn pump(&mut self) -> Result<()> {
        self.queue.dispatch_pending(&mut self.state)?;
        self.flush()?;
        if let Some(guard) = self.queue.prepare_read() {
            let mut pfd = libc::pollfd {
                fd: guard.connection_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one valid pollfd for the duration of the call.
            if unsafe { libc::poll(&raw mut pfd, 1, 0) } > 0 {
                match guard.read() {
                    Ok(_) => {}
                    Err(wayland_client::backend::WaylandError::Io(e))
                        if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(e).context("Wayland: reading"),
                }
            }
        }
        self.queue.dispatch_pending(&mut self.state)?;
        Ok(())
    }
}

/// Buttons still down let go where they went down, before the connection closes.
impl Drop for Wlr {
    fn drop(&mut self) {
        let time = self.time();
        for (button, p) in self.down_on.drain() {
            p.button(
                time,
                u32::from(button.evdev()),
                wl_pointer::ButtonState::Released,
            );
            p.frame();
        }
        let _ = self.conn.flush();
    }
}
