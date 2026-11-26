//! The last way to make input: devices of the kernel's `uinput`, for a desktop that
//! has neither the RemoteDesktop portal nor virtual pointers. Only where the user may
//! write `/dev/uinput` (a udev rule giving the person at the PC `uaccess`; Prostir
//! installs none: such a rule lets every program of the user make input).
//!
//! The mouse is an absolute pointer spanning every output (as a virtual machine's
//! tablet is), its buttons and a wheel with steps of 1/120 notch; the keyboard is a
//! device of its own. No touch: the core makes mouse actions of touches.

use std::fs::File;
use std::io::Write as _;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;

use anyhow::{Context, Result};
use log::info;

use crate::platform::{Injector, MouseButton, OpenInput, Spot, TouchPhase};

const PATH: &str = "/dev/uinput";

// From Linux's `uinput.h` and `input-event-codes.h`.
const UI_SET_EVBIT: libc::c_ulong = 0x4004_5564;
const UI_SET_KEYBIT: libc::c_ulong = 0x4004_5565;
const UI_SET_RELBIT: libc::c_ulong = 0x4004_5566;
const UI_SET_ABSBIT: libc::c_ulong = 0x4004_5567;
const UI_DEV_SETUP: libc::c_ulong = 0x405c_5503;
const UI_ABS_SETUP: libc::c_ulong = 0x401c_5504;
const UI_DEV_CREATE: libc::c_ulong = 0x5501;
const UI_DEV_DESTROY: libc::c_ulong = 0x5502;
const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_REL: u16 = 2;
const EV_ABS: u16 = 3;
const SYN_REPORT: u16 = 0;
const ABS_X: u16 = 0;
const ABS_Y: u16 = 1;
const REL_HWHEEL: u16 = 6;
const REL_WHEEL: u16 = 8;
const REL_WHEEL_HI_RES: u16 = 11;
const REL_HWHEEL_HI_RES: u16 = 12;
const BUS_VIRTUAL: u16 = 6;
/// The range of the pointer's position each way.
const RANGE: i32 = 65535;

/// Whether the user may make devices.
pub fn available() -> bool {
    open().is_ok()
}

fn open() -> std::io::Result<File> {
    std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(PATH)
}

#[repr(C)]
struct Setup {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
    name: [u8; 80],
    ff_effects_max: u32,
}

#[repr(C)]
struct AbsSetup {
    code: u16,
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

fn ioctl(f: &File, request: libc::c_ulong, arg: libc::c_ulong) -> Result<()> {
    // SAFETY: a request of uinput's with an integer, or a pointer to the struct it
    // takes, alive for the call.
    let r = unsafe { libc::ioctl(f.as_raw_fd(), request, arg) };
    if r < 0 {
        return Err(std::io::Error::last_os_error()).context("uinput");
    }
    Ok(())
}

/// A device made with `keys`, relative axes `rels` and, with `abs`, a position.
fn device(name: &str, keys: &[u16], rels: &[u16], abs: bool) -> Result<File> {
    let f = open().with_context(|| format!("{PATH} cannot be written"))?;
    ioctl(&f, UI_SET_EVBIT, EV_KEY.into())?;
    for k in keys {
        ioctl(&f, UI_SET_KEYBIT, (*k).into())?;
    }
    if !rels.is_empty() {
        ioctl(&f, UI_SET_EVBIT, EV_REL.into())?;
        for r in rels {
            ioctl(&f, UI_SET_RELBIT, (*r).into())?;
        }
    }
    if abs {
        ioctl(&f, UI_SET_EVBIT, EV_ABS.into())?;
        for code in [ABS_X, ABS_Y] {
            ioctl(&f, UI_SET_ABSBIT, code.into())?;
            let setup = AbsSetup {
                code,
                value: 0,
                minimum: 0,
                maximum: RANGE,
                fuzz: 0,
                flat: 0,
                resolution: 0,
            };
            ioctl(&f, UI_ABS_SETUP, (&raw const setup) as libc::c_ulong)?;
        }
    }
    let mut setup = Setup {
        bustype: BUS_VIRTUAL,
        vendor: 0x1209,
        product: 0x7072,
        version: 1,
        name: [0; 80],
        ff_effects_max: 0,
    };
    let n = name.len().min(79);
    setup.name[..n].copy_from_slice(&name.as_bytes()[..n]);
    ioctl(&f, UI_DEV_SETUP, (&raw const setup) as libc::c_ulong)?;
    ioctl(&f, UI_DEV_CREATE, 0)?;
    Ok(f)
}

/// One `input_event` of the kernel, as it is written: the time (left 0, the kernel
/// stamps it), type, code and value.
fn event(kind: u16, code: u16, value: i32) -> [u8; 24] {
    let mut e = [0u8; 24];
    e[16..18].copy_from_slice(&kind.to_ne_bytes());
    e[18..20].copy_from_slice(&code.to_ne_bytes());
    e[20..24].copy_from_slice(&value.to_ne_bytes());
    e
}

/// Events, then the report that ends them.
fn report(events: &[(u16, u16, i32)]) -> Vec<u8> {
    events
        .iter()
        .chain([&(EV_SYN, SYN_REPORT, 0)])
        .flat_map(|(t, c, v)| event(*t, *c, *v))
        .collect()
}

/// The position of `at` on the pointer's axes, which span every output.
fn position(at: &Spot) -> (i32, i32) {
    let whole = Spot {
        rect: at.desk,
        ..at.clone()
    };
    let (x, y) = whole.across();
    let r = f64::from(RANGE);
    ((x * r).round() as i32, (y * r).round() as i32)
}

pub struct UinputOpen;

impl OpenInput for UinputOpen {
    fn open(&self) -> Result<Box<dyn Injector>> {
        let buttons: Vec<u16> = [
            MouseButton::Left,
            MouseButton::Right,
            MouseButton::Middle,
            MouseButton::Back,
            MouseButton::Forward,
        ]
        .into_iter()
        .map(MouseButton::evdev)
        .collect();
        let pointer = device(
            "Prostir pointer",
            &buttons,
            &[REL_WHEEL, REL_HWHEEL, REL_WHEEL_HI_RES, REL_HWHEEL_HI_RES],
            true,
        )?;
        // Every key of a keyboard (1 to 248).
        let keys: Vec<u16> = (1..=248).collect();
        let keyboard = device("Prostir keyboard", &keys, &[], false)?;
        info!("input: through uinput devices");
        Ok(Box::new(Uinput {
            pointer,
            keyboard,
            wheel: (0, 0),
        }))
    }
}

struct Uinput {
    pointer: File,
    keyboard: File,
    /// Parts of a notch of the wheel not sent as whole notches yet.
    wheel: (i32, i32),
}

impl Uinput {
    fn send(f: &mut File, events: &[(u16, u16, i32)]) -> Result<()> {
        f.write_all(&report(events)).context("uinput: writing")
    }
}

impl Drop for Uinput {
    fn drop(&mut self) {
        for f in [&self.pointer, &self.keyboard] {
            let _ = ioctl(f, UI_DEV_DESTROY, 0);
        }
    }
}

impl Injector for Uinput {
    fn touches(&self) -> bool {
        false
    }

    fn pointer_to(&mut self, at: &Spot) -> Result<()> {
        let (x, y) = position(at);
        Self::send(&mut self.pointer, &[(EV_ABS, ABS_X, x), (EV_ABS, ABS_Y, y)])
    }

    fn pointer_by(&mut self, _dx: f64, _dy: f64) -> Result<()> {
        anyhow::bail!("the uinput pointer moves to points only")
    }

    fn button(&mut self, button: MouseButton, down: bool) -> Result<()> {
        Self::send(
            &mut self.pointer,
            &[(EV_KEY, button.evdev(), i32::from(down))],
        )
    }

    /// The fine steps, and whole notches for programs that read only those.
    fn scroll(&mut self, dx: i32, dy: i32) -> Result<()> {
        let notch = prostir_proto::input::SCROLL_NOTCH;
        self.wheel = (
            self.wheel.0.saturating_add(dx),
            self.wheel.1.saturating_add(dy),
        );
        let whole = (self.wheel.0 / notch, self.wheel.1 / notch);
        self.wheel = (self.wheel.0 % notch, self.wheel.1 % notch);
        // The wheel counts up for scrolling up, the protocol for down.
        let mut events = vec![
            (EV_REL, REL_HWHEEL_HI_RES, dx),
            (EV_REL, REL_WHEEL_HI_RES, -dy),
        ];
        if whole.0 != 0 {
            events.push((EV_REL, REL_HWHEEL, whole.0));
        }
        if whole.1 != 0 {
            events.push((EV_REL, REL_WHEEL, -whole.1));
        }
        Self::send(&mut self.pointer, &events)
    }

    /// Never called: it has no touch ([`Injector::touches`]).
    fn touch(&mut self, _id: u32, _phase: TouchPhase, _at: &Spot) -> Result<()> {
        Ok(())
    }

    fn key(&mut self, code: u16, down: bool) -> Result<()> {
        Self::send(&mut self.keyboard, &[(EV_KEY, code, i32::from(down))])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::OutputKey;
    use prostir_proto::state::DeskRect;

    /// The structs passed to the kernel have its sizes (`UI_DEV_SETUP` and
    /// `UI_ABS_SETUP` carry them in their numbers: 92 and 28 bytes).
    #[test]
    fn structs_have_the_kernels_sizes() {
        assert_eq!(size_of::<Setup>(), 92);
        assert_eq!(size_of::<AbsSetup>(), 28);
        assert_eq!((UI_DEV_SETUP >> 16) & 0x3fff, 92);
        assert_eq!((UI_ABS_SETUP >> 16) & 0x3fff, 28);
    }

    /// A report is the events, then `SYN_REPORT`, each 24 bytes with the time left to
    /// the kernel.
    #[test]
    fn a_report_ends_in_a_sync() {
        let bytes = report(&[(EV_KEY, 0x110, 1)]);
        assert_eq!(bytes.len(), 48);
        assert_eq!(&bytes[..16], &[0; 16]);
        assert_eq!(u16::from_ne_bytes([bytes[16], bytes[17]]), EV_KEY);
        assert_eq!(u16::from_ne_bytes([bytes[18], bytes[19]]), 0x110);
        assert_eq!(i32::from_ne_bytes(bytes[20..24].try_into().unwrap()), 1);
        assert_eq!(&bytes[40..42], &EV_SYN.to_ne_bytes());
    }

    /// A point spans every output: the second of two side by side starts halfway.
    #[test]
    fn the_pointer_spans_every_output() {
        let rect = DeskRect {
            x: 1000,
            y: 0,
            w: 1000,
            h: 500,
        };
        let at = Spot {
            output: OutputKey::new("Prostir-2"),
            rect,
            desk: DeskRect {
                x: 0,
                y: 0,
                w: 2000,
                h: 1000,
            },
            x: 1000.0,
            y: 500.0,
        };
        assert_eq!(position(&at), (32768, 32768));
    }
}
