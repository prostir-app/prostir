//! What the headset says, carried out: its messages, and what changed about it.

use super::*;
use crate::input;

impl Session {
    /// Follows what the headset said about itself: worn or not (frames and sound go
    /// to it only while worn; the control loop pauses the desktop once no headset is),
    /// its battery, and the watchers its preferences ask for (notifications only for a
    /// headset with the right to private things).
    pub(super) fn headset_changed(&mut self, displays: &Displays) {
        let access = self.publisher.access;
        let headset = &self.publisher.headset;
        let worn = worn(headset);
        self.asleep &= !worn;
        self.taken_off_at = match (worn, self.taken_off_at) {
            (true, _) => None,
            (false, Some(t)) => Some(t),
            (false, None) => Some(Instant::now()),
        };
        let fresh = displays.router.set_worn(self.id, worn);
        if !worn {
            // Nothing stays pressed on the PC by a headset off the head.
            displays.input(input::Ev::Release { from: self.id });
        }
        self.fresh.extend(fresh);
        let denied = self.tray_line().mic_denied;
        if denied && !self.mic_denied {
            warn!("the headset's microphone has no permission: the system does not let it record");
        }
        self.mic_denied = denied;
        if let Some(b) = headset.battery
            && self.standing.lead
            // Once per session, when it gets low.
            && b.percent <= 15
            && !b.charging
            && !self.battery_warned
        {
            self.battery_warned = true;
            notify_battery(b.percent);
        }
        let prefs = self.publisher.prefs();
        // Where every headset on the head has "Focus", the monitors nobody looks at
        // get 5 frames a second.
        displays.router.set_focus_mode(self.id, prefs.focus_mode);
        self.notifications = if prefs.notifications && access.may(perms::PRIVATE) {
            self.notifications.take().or_else(|| {
                let outbox = self.outbox.clone();
                notifications::watch(move |text| {
                    let text = clip(&text, MAX_NOTICE);
                    outbox.control(HostMsg::Notice { text });
                })
            })
        } else {
            None
        };
        self.sound_source = prefs.sound_from_screen.then(|| {
            self.sound_source
                .take()
                .unwrap_or_else(|| sound_source::Watcher::start(displays.window_list()))
        });
    }

    /// One message from the headset. Returns whether the headset's state changed
    /// (worn or not, its microphone, its preferences), which the control loop looks at
    /// for every session.
    pub fn handle(
        &mut self,
        msg: ClientMsg,
        came: Instant,
        displays: &mut Displays,
        store: &mut Store,
    ) -> bool {
        let volume = match &msg {
            ClientMsg::Volume { part } => Some(*part),
            _ => None,
        };
        let Some(msg) = self.publisher.headset.apply(msg) else {
            if let Some(v) = volume {
                self.volume_from_headset(v);
            }
            self.headset_changed(displays);
            return true;
        };
        match msg {
            // Taken by the reader (see `ups.rs`), which logged them; here they only
            // mean the control loop looks at which session has the microphone.
            ClientMsg::UpStart { .. } | ClientMsg::UpEnd { .. } | ClientMsg::Up { .. } => {}
            ClientMsg::Request { req, asked } => {
                let started = Instant::now();
                let what = format!("{asked:?}");
                let answer = match asked {
                    Asked::Known(request) => {
                        let limits = self.publisher.limits(displays, self.standing.no_limits);
                        let asker = requests::Asker {
                            caps: self.publisher.caps(),
                            limits: &limits,
                            access: self.publisher.access,
                        };
                        requests::handle(request, displays, store, &asker)
                    }
                    Asked::Unknown { kind } => {
                        info!("the headset asked for something unknown (kind {kind})");
                        requests::Answer::Now(Outcome::failed(ErrorCode::UNSUPPORTED, ""))
                    }
                };
                let outcome = match answer {
                    requests::Answer::Now(outcome) => outcome,
                    requests::Answer::Later(job) if displays.desk_stuck() => {
                        displays.desk().cancel(job);
                        displays.forget_job(job);
                        Outcome::failed(ErrorCode::DESKTOP, &desktop_not_responding())
                    }
                    requests::Answer::Later(job) => {
                        info!(
                            "request {} ({what}): waited {} ms, answered once the desktop did it",
                            req.0,
                            (started - came).as_millis(),
                        );
                        let until = Instant::now() + crate::worker::answer_within();
                        self.waiting.push((req, job, until));
                        return false;
                    }
                };
                if let Outcome::Failed { detail, .. } = &outcome {
                    warn!("request {}: {detail}", req.0);
                }
                info!(
                    "request {} ({what}): waited {} ms, done in {} ms",
                    req.0,
                    (started - came).as_millis(),
                    started.elapsed().as_millis()
                );
                self.replies.push(HostMsg::Reply { req, outcome });
            }
            // Only the main headset's arrangement is the desktop's.
            ClientMsg::Arrange { rows } if self.standing.main => {
                displays.set_layout(&rows);
                keep_screens(store, displays, false);
            }
            ClientMsg::Arrange { .. } => info!("a guest's arrangement leaves the desktop as it is"),
            ClientMsg::MouseTo { .. }
            | ClientMsg::MouseButton { .. }
            | ClientMsg::MouseScroll { .. }
            | ClientMsg::Touch { .. }
                if !self.publisher.access.may(perms::INPUT) =>
            {
                info!("the headset has no right to move the mouse");
            }
            ClientMsg::MouseTo { id, u, v } => {
                let id = ScreenId::from_wire(id);
                match displays.spot(id, u, v) {
                    Some(at) => displays.input(input::Ev::To { from: self.id, at }),
                    None => debug!("mouse: no {id} on the desktop"),
                }
            }
            ClientMsg::MouseButton {
                id,
                u,
                v,
                button,
                down,
            } => displays.input(input::Ev::Button {
                from: self.id,
                at: displays.spot(ScreenId::from_wire(id), u, v),
                button: mouse_button(button),
                down,
            }),
            ClientMsg::MouseScroll { dx, dy, .. } => displays.input(input::Ev::Scroll {
                from: self.id,
                dx,
                dy,
            }),
            ClientMsg::Touch {
                id,
                slot,
                u,
                v,
                phase,
            } => displays.input(input::Ev::Touch {
                from: self.id,
                slot,
                phase: touch_phase(phase),
                at: displays.spot(ScreenId::from_wire(id), u, v),
            }),
            // Said when the monitor looked at changes, at most four times a second: the
            // monitor looked at gets every frame, the others fewer (see
            // `router/priority.rs`).
            ClientMsg::Gaze { display, u, v } => {
                match display {
                    Some(id) => {
                        debug!("gaze: the headset looks at display {} at {u},{v}", id.0);
                    }
                    None => debug!("gaze: the headset looks at none of the displays"),
                }
                displays
                    .router
                    .gaze(self.id, display.map(ScreenId::from_wire));
            }
            // Off the head and asleep: the session ends without the grace.
            ClientMsg::Asleep if !self.worn() => {
                info!("the headset went to sleep");
                self.asleep = true;
            }
            ClientMsg::Asleep => info!("the headset says it sleeps, though it is worn"),
            ClientMsg::Reply { req, outcome } => match self.commands.remove(&req) {
                Some((what, reply, _)) => {
                    let result = CtlReply::of_outcome(&outcome);
                    info!("command {} ({what}): {result:?}", req.0);
                    if let Some(r) = reply {
                        self.answers.push(r, result);
                    }
                }
                None => info!("an answer to command {}, which nothing waits for", req.0),
            },
            other => warn!("ignoring {other:?}"),
        }
        false
    }
}

/// The PC's button for the headset's.
fn mouse_button(b: prostir_proto::input::Button) -> platform::MouseButton {
    use platform::MouseButton;
    use prostir_proto::input::Button;
    match b {
        Button::Left => MouseButton::Left,
        Button::Right => MouseButton::Right,
        Button::Middle => MouseButton::Middle,
        Button::Back => MouseButton::Back,
        Button::Forward => MouseButton::Forward,
    }
}

fn touch_phase(p: prostir_proto::input::TouchPhase) -> platform::TouchPhase {
    use platform::TouchPhase;
    use prostir_proto::input::TouchPhase as Wire;
    match p {
        Wire::Down => TouchPhase::Down,
        Wire::Move => TouchPhase::Move,
        Wire::Up => TouchPhase::Up,
        Wire::Cancel => TouchPhase::Cancel,
    }
}
