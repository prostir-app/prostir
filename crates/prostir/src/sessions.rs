//! The control loop's sessions: a connection accepted becomes a session (or is turned
//! away), and sessions end, with what happens after the last one.

use super::*;

/// The PC's Wi-Fi, when it reaches its network over Wi-Fi, written in the log: with
/// power saving on, how to turn it off (Prostir changes nothing of the network).
fn look_at_wifi() -> Option<api::Wifi> {
    let link = platform::wifi::look()?;
    let mhz = |v: Option<u32>| v.map_or_else(|| "?".to_owned(), |v| v.to_string());
    let line = format!(
        "this PC is on Wi-Fi ({}, {} MHz, {} MHz wide)",
        link.interface,
        mhz(link.freq_mhz),
        mhz(link.width_mhz)
    );
    match link.power_save {
        Some(true) => warn!(
            "{line} with power saving on, which can hold back what comes for the PC by \
             tens of milliseconds; NetworkManager turns it off for a connection with \
             `nmcli connection modify <name> 802-11-wireless.powersave 2`, or a cable \
             avoids it"
        ),
        Some(false) => info!("{line}, power saving off"),
        None => info!("{line}"),
    }
    Some(api::Wifi {
        interface: link.interface,
        power_save: link.power_save,
        freq_mhz: link.freq_mhz.unwrap_or(0),
        width_mhz: link.width_mhz.unwrap_or(0),
    })
}

impl Brain {
    /// The key of the headset whose session is `id`, while it runs.
    pub(super) fn key_of(&self, id: SessionId) -> Option<[u8; 32]> {
        self.sessions
            .iter()
            .find(|(_, s)| s.id == id)
            .map(|(key, _)| *key)
    }

    /// A connection set up and trusted: its session opens, replacing the headset's
    /// earlier one, unless too many headsets have one.
    pub(super) fn accepted(&mut self, accepted: Accepted) {
        info!(
            "headset connected via {} from {}",
            session::via_name(accepted.conn.link.peer.via),
            accepted.from
        );
        self.open_session(accepted);
    }

    /// The session of a connection set up and trusted opens, once the desktop is
    /// there (see [`Brain::accepted`]).
    pub(super) fn open_session(&mut self, accepted: Accepted) {
        let Accepted { conn, open, from } = accepted;
        let key = conn.link.peer.key;
        // Forgotten after its connection was set up (the tray's item came first).
        if !self.security.trusted.lock().unwrap().contains(&key) {
            info!("the headset from {from} was forgotten meanwhile: turning it away");
            return turn_away(
                conn,
                i18n::t("pc-sessions-this-pc-no-longer-trusts-this").into(),
            );
        }
        match brain::admit(self.sessions.keys(), &key, brain::max_headsets()) {
            Admit::Open => {}
            Admit::Replace => {
                info!("the same headset connected again: its earlier session gives way");
                if let (Some(old), Some(displays)) =
                    (self.sessions.remove(&key), self.displays.as_mut())
                {
                    // It no longer reads (or soon will not): no goodbye. Its new
                    // session takes its place, and its roles.
                    old.finish(None, displays);
                    self.headset_left(&key, api::Reason::Other);
                }
            }
            Admit::Refuse => {
                warn!("another headset has a session: turning {from} away");
                return turn_away(
                    conn,
                    i18n::t("pc-sessions-another-headset-is-already-connected").into(),
                );
            }
        }
        if self.sessions.is_empty() && self.displays.as_ref().is_some_and(|d| !d.healthy()) {
            self.let_go_displays();
        }
        if self.displays.is_none() {
            self.displays = Some(create_displays(
                self.set_primary,
                &mut self.store,
                &self.events,
            ));
        }
        // The session opens once the desktop is there (connecting does not hold the
        // control loop up).
        if !self.displays.as_ref().is_some_and(Displays::is_connected) {
            return self.wait_for_desk(Accepted { conn, open, from });
        }
        // What the new session has of the jobs only one headset can have.
        self.arrivals.connected(key);
        let order = self.in_order(Some((key, self.store.wanted().access_of(&key))));
        let roles = brain::roles(&order, self.roles, self.mic_busy());
        let standing = Standing {
            main: roles.main == Some(key),
            mic: roles.mic == Some(key),
            lead: roles.lead == Some(key),
            no_limits: self.main_no_limits(),
        };
        let others: Vec<_> = self.sessions.values().map(|s| s.caps().clone()).collect();
        let displays = self
            .displays
            .as_mut()
            .expect("made above if it was missing");
        self.last_id += 1;
        let app = conn.app.clone();
        match Session::start(
            self.last_id,
            conn,
            &open,
            standing,
            &others,
            displays,
            &self.store,
            &self.events,
        ) {
            Ok(mut session) => {
                session.set_answers(self.answers.clone());
                // Right after the snapshot: what it asks for this PC with.
                session.send(HostMsg::FindSecret {
                    secret: self.security.find_secret(),
                });
                // An older secret kept valid for it is needed no more. (Should the
                // session break before it arrives, the headset still reaches the PC by
                // its last address and gets it then.)
                self.security.gave_find_secret(&key);
                let via = session.api_session().via;
                if via == api::Via::Wifi {
                    self.wifi = look_at_wifi();
                }
                self.store.headset_seen(&key, unix_now(), Some(&app));
                self.worn_told.insert(key, session.worn());
                self.sessions.insert(key, session);
                self.phase.session_opened(Instant::now());
                self.headsets_changed();
                self.api
                    .send(api::Note::Event(api::Event::HeadsetConnected {
                        headset: prostir_proto::secure::fingerprint(&key),
                        name: self.headset_name(&key),
                        via,
                    }));
            }
            Err(e) => {
                warn!("session ended: {e:#}");
                self.arrivals.left(key);
                // The codec it was fitted to, the encoders and the sound it started:
                // as the sessions left (if any) need them.
                self.sessions_ended(false);
            }
        }
    }

    /// The keys of the headsets with a session and the rights of their lines, in the
    /// order they first connected; with `opening`, one whose session opens now.
    fn in_order(
        &self,
        opening: Option<([u8; 32], prostir_proto::state::Access)>,
    ) -> Vec<([u8; 32], prostir_proto::state::Access)> {
        self.arrivals
            .keys()
            .filter_map(|key| match (self.sessions.get(key), opening) {
                (_, Some((k, access))) if k == *key => Some((k, access)),
                (Some(s), _) => Some((*key, s.configured)),
                (None, _) => None,
            })
            .collect()
    }

    /// The PC's end of the headset microphone is in use: a program records from it,
    /// or the headset whose microphone it is streams it.
    pub(super) fn mic_busy(&self) -> bool {
        self.displays.as_ref().is_some_and(Displays::recording)
            || self
                .roles
                .mic
                .and_then(|key| self.sessions.get(&key))
                .is_some_and(Session::mic_streams)
    }

    /// "No limits" of the main headset, which the PC's limits follow.
    fn main_no_limits(&self) -> bool {
        self.roles
            .main
            .and_then(|key| self.sessions.get(&key))
            .is_some_and(Session::no_limits)
    }

    /// A session opened or ended, or a headset said something about itself: who has
    /// the jobs only one headset can have, whether the encoders run (while any headset
    /// is worn, with a fresh picture for one put on again), and the PC's end of the
    /// microphone (the switch of the headset whose microphone it takes).
    pub(super) fn headsets_changed(&mut self) {
        let roles = brain::roles(&self.in_order(None), self.roles, self.mic_busy());
        if roles.mic != self.roles.mic && self.roles.mic.is_some() {
            info!("the PC's end of the headset microphone follows another headset now");
        }
        self.roles = roles;
        let no_limits = self.main_no_limits();
        let Some(displays) = self.displays.as_mut() else {
            return;
        };
        let mut fresh = Vec::new();
        for (key, s) in &mut self.sessions {
            s.set_standing(Standing {
                main: roles.main == Some(*key),
                mic: roles.mic == Some(*key),
                lead: roles.lead == Some(*key),
                no_limits,
            });
            if !s.may(prostir_proto::state::perms::INPUT) {
                // Without the right (any more), nothing it pressed stays pressed.
                displays.input(input::Ev::Release { from: s.id });
            }
            fresh.extend(s.take_fresh());
        }
        let worn = self.sessions.values().any(Session::worn);
        displays.worn(worn, &fresh);
        let usb = !self.sessions.is_empty() && self.sessions.values().all(Session::by_usb);
        displays.set_usb(usb);
        let mic = roles
            .mic
            .and_then(|key| self.sessions.get(&key))
            .is_some_and(Session::mic_on);
        displays.set_mic(mic);
    }

    /// Shows the state in the tray: of the headsets, the session the tray follows
    /// (the main one, else the first), the microphone of the one the PC takes it from,
    /// and how many others there are.
    pub(super) fn show(&mut self) {
        let line = |key: Option<[u8; 32]>| {
            key.and_then(|key| self.sessions.get(&key))
                .map(Session::tray_line)
                .unwrap_or_default()
        };
        // The microphone as the headset whose microphone the PC takes has it.
        let lead = TrayLine {
            mic_denied: line(self.roles.mic).mic_denied,
            ..line(self.roles.lead)
        };
        let others = self.sessions.len().saturating_sub(1);
        let pairing = self.door.pairing_open();
        self.control.input_ask = self
            .input_asks
            .first_waiting()
            .map(|key| (key, self.headset_label(&key)));
        self.control.show(
            self.displays.as_ref(),
            &self.store,
            &lead,
            others,
            pairing,
            self.desk_unusable.is_some(),
        );
    }

    /// The session commands for the headset go to: the main one, else the first.
    pub(super) fn lead_mut(&mut self) -> Option<&mut Session> {
        let lead = self.roles.lead?;
        self.sessions.get_mut(&lead)
    }

    /// Ends the session of headset `key`: with `goodbye`, saying why. The other
    /// sessions go on.
    pub(super) fn end_session(
        &mut self,
        key: [u8; 32],
        goodbye: Option<PcGoodbye>,
        why: api::Reason,
        result: Result<()>,
    ) {
        if let Some(taken_off) = self.close_session(key, goodbye, why, result) {
            self.sessions_ended(taken_off);
        }
    }

    /// Ends the sessions of `keys` at once, each with `goodbye` (and why, from
    /// `result`): what the others need is looked at once, after the last.
    pub(super) fn end_sessions(
        &mut self,
        keys: Vec<[u8; 32]>,
        goodbye: PcGoodbye,
        why: api::Reason,
        result: impl Fn() -> Result<()>,
    ) {
        let mut ended = None;
        for key in keys {
            ended = self
                .close_session(key, Some(goodbye), why, result())
                .or(ended);
        }
        if let Some(taken_off) = ended {
            self.sessions_ended(taken_off);
        }
    }

    /// Closes the session of headset `key` (see [`Session::finish`]), with nothing
    /// else changed for the others yet: whether its headset was taken off, if it had
    /// a session.
    fn close_session(
        &mut self,
        key: [u8; 32],
        goodbye: Option<PcGoodbye>,
        why: api::Reason,
        result: Result<()>,
    ) -> Option<bool> {
        let session = self.sessions.remove(&key)?;
        self.headset_left(&key, why);
        // Seen until now (a forgotten headset's time goes with it).
        if self.security.trusted.lock().unwrap().contains(&key) {
            self.store.headset_seen(&key, unix_now(), None);
        }
        let displays = self.displays.as_mut()?;
        self.arrivals.left(key);
        let taken_off = session.taken_off_for().is_some();
        if let Some(closing) = session.finish(goodbye, displays) {
            self.closing.push(closing);
        }
        match result {
            Ok(()) => info!("headset disconnected"),
            Err(e) => warn!("session ended: {e:#}"),
        }
        Some(taken_off)
    }

    /// Sessions ended (the last of them taken off with `taken_off`): the encoders and
    /// sound as the sessions left need them, in a codec they all decode; without any,
    /// what becomes of the monitors.
    fn sessions_ended(&mut self, taken_off: bool) {
        if !self.sessions.is_empty()
            && let Some(displays) = self.displays.as_mut()
        {
            // The headsets left may all decode the codec chosen again.
            let headsets = displays.router.headsets();
            displays.fit_codec(&headsets);
        }
        self.headsets_changed();
        if self.sessions.is_empty() {
            self.after_the_last_session(taken_off);
        }
    }

    /// No session is left: the monitors stay for a while, or go now when the last
    /// headset was taken off (`taken_off`: it reconnects by itself once worn again)
    /// or they broke.
    fn after_the_last_session(&mut self, taken_off: bool) {
        self.control.no_headset(&self.answers);
        if let Some(d) = self.displays.as_mut() {
            d.no_headset();
        }
        self.roles = brain::Roles::NONE;
        let how = match &self.displays {
            Some(_) if taken_off => {
                info!("removing virtual monitors until the headset is worn again");
                LastEnded::Drop
            }
            Some(d) if d.healthy() => LastEnded::Keep,
            _ => LastEnded::Drop,
        };
        self.phase
            .last_session_ended(how, Instant::now(), self.linger);
        match self.phase {
            Phase::Lingering { .. } => {
                info!("keeping virtual monitors for {} s", self.linger.as_secs());
            }
            _ => self.drop_displays(),
        }
        self.show();
    }

    /// Removes the monitors (the desktop's worker puts everything back and gives their
    /// memory back).
    pub(super) fn drop_displays(&mut self) {
        self.let_go_displays();
    }
}
