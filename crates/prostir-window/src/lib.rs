//! Prostir's settings window: a library of the `prostir` program, which shows it in a
//! process of its own (`prostir window`, [`run`]), so the service never starts the
//! toolkit or loads the window's graphics libraries (EGL, X11). It does load
//! libwayland-client, for its own Wayland connections: built into one program with the
//! window, wayland-client uses the system's library (at run time) everywhere. The
//! window keeps no settings of its own: it reads the state of the Prostir running in
//! the background over its local API and asks it for changes.
//!
//! - [`api`]: the state and requests as JSON, and the [`api::Client`] trait;
//!   [`dbus`] talks to the service on the session bus, [`mock`] is one in memory.
//! - [`link`]: the client on its own thread, so the window never waits for it.
//! - [`theme`], [`fonts`]: the look as data; [`portal`]: the desktop's light or dark.
//! - [`app`], [`pages`], [`widgets`], [`map`], [`snap`]: the window itself.
//! - [`instance`]: one window per person.
//! - [`run()`]: the window as a program.
//!
//! Texts are Fluent files in `lang/` at the root of the repository (the window's messages
//! start with `window-`), loaded by `prostir-lang`.

pub mod api;
pub mod app;
pub mod dbus;
pub mod fonts;
pub mod instance;
pub mod launch;
pub mod link;
pub mod map;
pub mod mock;
pub mod pages;
pub mod portal;
mod run;
pub mod snap;
pub mod theme;
pub mod widgets;

pub use app::{App, Options};
pub use run::{APP_ID, run};

/// A client for when there is no session bus at all: every call says so.
pub struct NoBus(pub String);

impl api::Client for NoBus {
    fn get_state(&self) -> Result<api::Snapshot, api::ClientError> {
        Err(api::ClientError::NoBus(self.0.clone()))
    }

    fn call(&self, _: &api::Request) -> Result<api::Answer, api::ClientError> {
        Err(api::ClientError::NoBus(self.0.clone()))
    }

    fn watch(&self, _: api::Sink) -> Result<(), api::ClientError> {
        Ok(())
    }
}
