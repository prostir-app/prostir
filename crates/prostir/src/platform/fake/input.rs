//! The fake desktop's virtual input: what it is told goes to the log, in the pixels of
//! the output it lands on, for the end-to-end tests to read.
//!
//! Only with `PROSTIR_FAKE_INPUT`: `touch` (or `1`) for a desktop with touch, `mouse`
//! for one without (the core makes mouse actions of touches). A test's commands (see
//! `desktop.rs`): `input-refuse`, the person says no the next time the devices open;
//! `input-close`, the desktop closes the devices open now; `input-stop`, the person
//! stops them (not opened again by themselves).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Result, ensure};
use log::info;

use crate::platform::{
    Injector, Injects, InputRefused, InputStopped, MouseButton, OpenInput, Spot, TouchPhase,
};

static REFUSE: AtomicBool = AtomicBool::new(false);
static CLOSE: AtomicBool = AtomicBool::new(false);
static STOP: AtomicBool = AtomicBool::new(false);

/// The person stops the devices open now (as stopping the desktop's remote control).
pub fn stop() {
    STOP.store(true, Ordering::Relaxed);
}

/// The person says no the next time the devices open.
pub fn refuse_next() {
    REFUSE.store(true, Ordering::Relaxed);
}

/// The desktop closes the devices open now.
pub fn close() {
    CLOSE.store(true, Ordering::Relaxed);
}

/// What the fake desktop's input makes, and how to open it, as the environment says.
pub fn asked() -> Result<Option<(Injects, Arc<dyn OpenInput>)>> {
    let touches = match std::env::var("PROSTIR_FAKE_INPUT").as_deref() {
        Err(_) => return Ok(None),
        Ok("1" | "touch") => true,
        Ok("mouse") => false,
        Ok(other) => anyhow::bail!("PROSTIR_FAKE_INPUT: touch or mouse, not {other:?}"),
    };
    let injects = Injects {
        pointer: true,
        keys: true,
    };
    Ok(Some((injects, Arc::new(FakeOpen { touches }))))
}

struct FakeOpen {
    touches: bool,
}

impl OpenInput for FakeOpen {
    fn open(&self) -> Result<Box<dyn Injector>> {
        if REFUSE.swap(false, Ordering::Relaxed) {
            info!("fake desktop: the person did not allow the virtual input");
            return Err(InputRefused.into());
        }
        CLOSE.store(false, Ordering::Relaxed);
        STOP.store(false, Ordering::Relaxed);
        info!("fake desktop: virtual input open");
        Ok(Box::new(FakeInjector {
            touches: self.touches,
        }))
    }
}

struct FakeInjector {
    touches: bool,
}

impl FakeInjector {
    fn say(&self, what: &str) -> Result<()> {
        if STOP.load(Ordering::Relaxed) {
            return Err(anyhow::Error::new(InputStopped).context("the person stopped it"));
        }
        ensure!(
            !CLOSE.load(Ordering::Relaxed),
            "the fake desktop closed the virtual input"
        );
        info!("fake desktop: {what}");
        Ok(())
    }
}

/// Where `at` is in the pixels of its output (logical, as the fake desktop has scale
/// 1 unless told).
fn pixel(at: &Spot) -> String {
    format!(
        "{} at {},{}",
        at.output,
        (at.x - f64::from(at.rect.x)).floor(),
        (at.y - f64::from(at.rect.y)).floor()
    )
}

impl Injector for FakeInjector {
    fn touches(&self) -> bool {
        self.touches
    }

    fn pointer_to(&mut self, at: &Spot) -> Result<()> {
        self.say(&format!("pointer to {}", pixel(at)))
    }

    fn pointer_by(&mut self, dx: f64, dy: f64) -> Result<()> {
        self.say(&format!("pointer by {dx},{dy}"))
    }

    fn button(&mut self, button: MouseButton, down: bool) -> Result<()> {
        let how = if down { "down" } else { "up" };
        self.say(&format!("button {button:?} {how}"))
    }

    fn scroll(&mut self, dx: i32, dy: i32) -> Result<()> {
        self.say(&format!("scroll {dx},{dy}"))
    }

    fn touch(&mut self, id: u32, phase: TouchPhase, at: &Spot) -> Result<()> {
        self.say(&format!("touch {id} {phase:?} on {}", pixel(at)))
    }

    fn key(&mut self, code: u16, down: bool) -> Result<()> {
        let how = if down { "down" } else { "up" };
        self.say(&format!("key {code} {how}"))
    }
}

impl Drop for FakeInjector {
    fn drop(&mut self) {
        info!("fake desktop: virtual input closed");
    }
}
