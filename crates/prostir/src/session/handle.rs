//! What the headset says, carried out: its messages, and what changed about it.

use super::*;

impl Session {
    /// Follows what the headset said about itself: worn or not (frames and sound go
    /// to it only while worn; the control loop pauses the desktop once no headset is),
    /// its battery, and the watchers its preferences ask for (notifications only for a
    /// headset with the right to private things).
    pub(super) fn headset_changed(&mut self, displays: &Displays) {
        let access = self.publisher.access;
        let headset = &self.publisher.headset;
        let worn = worn(headset);
        self.taken_off_at = match (worn, self.taken_off_at) {
            (true, _) => None,
            (false, Some(t)) => Some(t),
            (false, None) => Some(Instant::now()),
        };
        let fresh = displays.router.set_worn(self.id, worn);
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
            ClientMsg::MouseTo { id, u, v } if self.publisher.access.may(perms::INPUT) => {
                displays.mouse_to(ScreenId::from_wire(id), u, v);
            }
            ClientMsg::MouseTo { .. } => info!("the headset has no right to move the mouse"),
            // Used for priorities from step 10 on.
            ClientMsg::Gaze { .. } => {}
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
