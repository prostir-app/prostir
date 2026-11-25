//! What Android and OpenXR said in a pass, turned into the model's events: the
//! activity paused and resumed, the keys handed back, the session's states, the
//! space moved, the controllers changed, the cameras, the display rate.

use std::time::{Duration, Instant};

use android_activity::{AndroidApp, InputStatus, MainEvent, PollEvent};
use anyhow::Result;
use log::{error, info, warn};
use openxr as xr;

use super::shell::Shell;
use super::{Kept, Selftest};
use crate::app::{Event, PlatformEvent};
use crate::platform::meta;
use crate::platform::sound::mic::Mic;
use crate::platform::store::{Store, StoreCheck};

/// What Android said in a pass.
#[derive(Default)]
pub struct Polled {
    pub destroy: bool,
    pub paused: bool,
    /// Paused (true) or resumed (false), the last one of this pass.
    pub activity: Option<bool>,
    pub input_waiting: bool,
}

/// Android's events of this pass: at once while the session runs, waiting up to
/// 100 ms otherwise.
pub fn android(app: &AndroidApp, session_running: bool, mic: &Mic) -> Polled {
    let mut p = Polled::default();
    let timeout = if session_running {
        Duration::ZERO
    } else {
        Duration::from_millis(100)
    };
    // The microphone hears each pause and return: its prompt's answer shows so.
    app.poll_events(Some(timeout), |event| match event {
        PollEvent::Main(MainEvent::Destroy) => p.destroy = true,
        PollEvent::Main(MainEvent::Pause) => {
            p.paused = true;
            p.activity = Some(true);
            mic.activity(true);
        }
        PollEvent::Main(MainEvent::Resume { .. }) => {
            p.activity = Some(false);
            mic.activity(false);
        }
        PollEvent::Main(MainEvent::InputAvailable) => p.input_waiting = true,
        _ => {}
    });
    p
}

/// What Android's events come to for the model: the microphone's prompt, a pause or
/// a return, and the store's check.
pub fn activity(
    shell: &mut Shell<'_>,
    now: Instant,
    polled: &Polled,
    kept: &mut Kept,
    store: &mut dyn Store,
) {
    // The microphone asked the system for access: the prompt's pause is the app's
    // own, not the user leaving.
    let asks = shell.mic.asks();
    if asks != kept.mic_asks {
        kept.mic_asks = asks;
        shell.handle(now, Event::Platform(PlatformEvent::MicAsked));
    }
    if let Some(paused) = polled.activity {
        // The system may freeze a paused app soon: the PC hears it first.
        shell.handle(now, Event::Platform(PlatformEvent::Paused(paused)));
        shell.presence_now(now, &mut kept.screen_read);
    }
    if polled.paused {
        // The system may end a paused app without a word: what waits is written now.
        shell.flush(now, "paused");
    }
    if let StoreCheck::NotOwned { store: name } = store.poll() {
        shell.handle(
            now,
            Event::Platform(PlatformEvent::NotOwned { store: name }),
        );
    }
}

/// Android waits for every key event (e.g. the volume buttons) to be taken and
/// answers "not responding" after 5 s otherwise. Hand them all back to the system so
/// the volume buttons keep working; whether one was a volume button (the system
/// changes the volume for it).
pub fn hand_back_keys(app: &AndroidApp) -> bool {
    use android_activity::input::{InputEvent, Keycode};
    let mut volume_button = false;
    if let Ok(mut events) = app.input_events_iter() {
        while events.next(|event| {
            volume_button |= matches!(
                event,
                InputEvent::KeyEvent(key) if matches!(
                    key.key_code(),
                    Keycode::VolumeUp | Keycode::VolumeDown | Keycode::VolumeMute
                )
            );
            InputStatus::Unhandled
        }) {}
    }
    volume_button
}

/// Whether the loop goes on.
#[derive(PartialEq, Eq)]
pub enum Flow {
    Go,
    /// The session or the instance is going: the app closes.
    Exit,
}

/// OpenXR's events of this pass.
pub fn openxr(
    instance: &xr::Instance,
    shell: &mut Shell<'_>,
    kept: &mut Kept,
    mut test: Option<&mut Selftest>,
    now: Instant,
) -> Result<Flow> {
    let session = shell.session;
    while let Some(event) = instance.poll_event(&mut kept.event_storage)? {
        match event {
            xr::Event::SessionStateChanged(e) => {
                info!("session state: {:?}", e.state());
                if let Some(t) = test.as_deref_mut() {
                    t.session_state(e.state());
                }
                let now_focused = e.state() == xr::SessionState::FOCUSED;
                if kept.focused && !now_focused {
                    // A drag cannot go on under the system menu.
                    kept.hands.cancel_all(&mut shell.model);
                    shell.apply(now);
                }
                kept.focused = now_focused;
                // Hidden or stopped, the PC stops encoding until the app is shown
                // again; told before the session ends and the files are written.
                let shown = match e.state() {
                    xr::SessionState::VISIBLE | xr::SessionState::FOCUSED => {
                        Some(crate::presence::Session::Shown)
                    }
                    xr::SessionState::READY | xr::SessionState::SYNCHRONIZED => {
                        Some(crate::presence::Session::Hidden)
                    }
                    xr::SessionState::STOPPING | xr::SessionState::IDLE => {
                        Some(crate::presence::Session::Stopped)
                    }
                    _ => None,
                };
                if let Some(shown) = shown {
                    shell.handle(now, Event::Platform(PlatformEvent::Session(shown)));
                    // Told before the session ends and the files are written.
                    shell.presence_now(now, &mut kept.screen_read);
                }
                match e.state() {
                    xr::SessionState::READY => {
                        session.begin(xr::ViewConfigurationType::PRIMARY_STEREO)?;
                        kept.session_running = true;
                        // The rate by the rule, from those the runtime offers.
                        if let Some(hz) = shell.model.wanted_rate() {
                            meta::set_display_rate(session, hz);
                        }
                        if let Some(t) = test.as_deref_mut() {
                            let (offered, now) = meta::display_rates(session);
                            t.refresh(&offered, now);
                        }
                    }
                    xr::SessionState::STOPPING => {
                        session.end()?;
                        kept.session_running = false;
                        // Taken off (the headset may sleep for long): saved now.
                        shell.flush(now, "stopping");
                    }
                    xr::SessionState::IDLE => {
                        shell.flush(now, "idle");
                    }
                    xr::SessionState::EXITING => return Ok(Flow::Exit),
                    // The runtime is going: only out, as for EXITING (what waits is
                    // written, the PC told goodbye); no new session is tried.
                    xr::SessionState::LOSS_PENDING => {
                        warn!("the OpenXR session is being lost: the app closes");
                        return Ok(Flow::Exit);
                    }
                    _ => {}
                }
            }
            xr::Event::ReferenceSpaceChangePending(e) => {
                let space = e.reference_space_type();
                info!(
                    "the {space:?} space moves (pose in the old one {}given)",
                    if e.pose_valid() { "" } else { "not " }
                );
                if space == xr::ReferenceSpaceType::LOCAL {
                    // A drag cannot go on with the room moving under it.
                    if kept.hands.busy() {
                        kept.hands.cancel_all(&mut shell.model);
                        shell.apply(now);
                    }
                    shell.handle(now, Event::Platform(PlatformEvent::SpaceMoved));
                }
            }
            xr::Event::InteractionProfileChanged(_) => {
                // Controllers put down or picked up: what a hand held is let go,
                // and the rays come from the new profile.
                info!("interaction profile changed");
                shell.controls.refresh_profiles(session);
                if kept.hands.busy() {
                    kept.hands.cancel_all(&mut shell.model);
                    shell.apply(now);
                }
            }
            xr::Event::PassthroughStateChangedFB(e) => {
                let flags = e.flags();
                if flags.contains(xr::PassthroughStateChangedFlagsFB::NON_RECOVERABLE_ERROR) {
                    error!("the cameras failed for good");
                    shell.room.lost();
                    shell.handle(now, Event::Platform(PlatformEvent::CamerasLost));
                } else if flags.contains(xr::PassthroughStateChangedFlagsFB::RECOVERABLE_ERROR) {
                    warn!("the cameras stopped for a moment");
                } else if flags.contains(xr::PassthroughStateChangedFlagsFB::RESTORED_ERROR) {
                    info!("the cameras work again");
                }
            }
            xr::Event::DisplayRefreshRateChangedFB(e) => {
                let hz = e.to_display_refresh_rate();
                info!(
                    "display rate {} Hz to {hz} Hz",
                    e.from_display_refresh_rate()
                );
                shell.handle(now, Event::Platform(PlatformEvent::RefreshRate(hz)));
            }
            xr::Event::EventsLost(e) => {
                // What may have been lost is read again: the display rate and the
                // controllers (the session's state comes again with its next
                // change).
                warn!("the runtime lost {} events", e.lost_event_count());
                if let Some(hz) = meta::display_rate(session) {
                    shell.handle(now, Event::Platform(PlatformEvent::RefreshRate(hz)));
                }
                if shell.controls.refresh_profiles(session) && kept.hands.busy() {
                    kept.hands.cancel_all(&mut shell.model);
                    shell.apply(now);
                }
            }
            xr::Event::UserPresenceChangedEXT(e) => {
                let present = e.is_user_present();
                shell.handle(now, Event::Platform(PlatformEvent::UserPresent(present)));
            }
            xr::Event::InstanceLossPending(_) => {
                warn!("the OpenXR instance is being lost: the app closes");
                return Ok(Flow::Exit);
            }
            _ => {}
        }
    }
    Ok(Flow::Go)
}

/// What the system says every pass, also while nothing is drawn: the sensor's
/// "away" counts once it lasted (see `presence::AWAY_GRACE`), the screen is read
/// while the app is put away, and the system may let the app record now, or not any
/// more.
pub fn presence_and_mic(shell: &mut Shell<'_>, now: Instant, kept: &mut Kept) {
    shell.presence_now(now, &mut kept.screen_read);
    let mic_now = (shell.mic.permission(), shell.mic.denied());
    if mic_now != kept.mic_seen {
        kept.mic_seen = mic_now;
        let (permission, denied) = mic_now;
        shell.handle(
            now,
            Event::Platform(PlatformEvent::Mic { permission, denied }),
        );
    }
}
