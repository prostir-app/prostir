//! Builds without the selftest (the store's): the same calls, for a selftest that
//! never exists, so they do nothing and cost nothing.

use std::path::{Path, PathBuf};
use std::time::Duration;

use android_activity::AndroidApp;
use openxr as xr;
use prostir_proto::state::DisplayId;

use crate::caps::Caps;
use crate::net::NetEvent;
use crate::platform::openxr::Gfx;

pub fn count_run() -> u32 {
    0
}

#[expect(
    dead_code,
    reason = "made only by a selftest, which this build has not"
)]
pub enum Act {
    Nothing,
    MoveMonitor,
    OpenMenu,
    SwitchTab,
    CloseMenu,
    AddMonitor(u32, u32),
    RemoveMonitor(DisplayId),
    Inject(DisplayId, crate::video::Fault),
    Finish,
}

pub struct FrameStart;

/// No value of it can be made.
pub enum Selftest {}

impl Selftest {
    pub fn requested(_app: &AndroidApp, _run: u32) -> Option<Self> {
        None
    }

    pub fn finish(self, _: Option<&Path>, _: Option<&anyhow::Error>) -> (bool, i32) {
        match self {}
    }

    pub fn caps_minimal(&self) -> bool {
        match *self {}
    }

    pub fn runtime(&self, _: &xr::Instance, _: &xr::ExtensionSet, _: bool, _: bool) {
        match *self {}
    }

    pub fn caps(&self, _: &Caps) {
        match *self {}
    }

    pub fn set_perf(&self, _: &xr::Session<Gfx>) {
        match *self {}
    }

    pub fn data_dir(&self, _: &Path) -> PathBuf {
        match *self {}
    }

    pub fn start_pc(&self, _: &AndroidApp, _: &Path) -> Option<std::net::SocketAddr> {
        match *self {}
    }

    pub fn over(&self) -> bool {
        match *self {}
    }

    pub fn session_state(&self, _: xr::SessionState) {
        match *self {}
    }

    pub fn refresh(&self, _: &[f32], _: Option<f32>) {
        match *self {}
    }

    pub fn net_event(&self, _: &NetEvent) {
        match *self {}
    }

    pub fn loop_top(&self) {
        match *self {}
    }

    pub fn step(&self) -> Act {
        match *self {}
    }

    pub fn frame_start(&self) -> FrameStart {
        match *self {}
    }

    pub fn frame_end(&self, _: &FrameStart, _: i64, _: Duration, _: Option<Duration>, _: usize) {
        match *self {}
    }

    pub fn saved(&self, _: Duration) {
        match *self {}
    }
}
