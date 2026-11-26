//! The tray: the icon and what it shows, its commands, and the commands for the
//! headset waiting to go.

use super::*;

/// The tray icon, what it shows, and the commands for the headset waiting to go.
pub(super) struct Control {
    pub(crate) tray: Option<tray::Handle>,
    pub(crate) shown: Option<TrayStatus>,
    /// The settings the tray shows: quality, the text preset, the counter.
    pub(crate) shown_settings: Option<(u32, bool, bool)>,
    /// The battery the tray shows.
    pub(crate) shown_battery: Option<Option<(u8, bool)>>,
    /// Commands for the headset from the tray and the local API, and who waits for
    /// what came of each.
    pub(crate) for_headset: VecDeque<(Asked<Command>, Option<api::Reply>)>,
    /// The name of the headset the tray asks about: may it use the mouse and
    /// keyboard (see `allow_input`).
    pub(crate) input_ask: Option<String>,
}

impl Control {
    /// Commands for the headset with no headset to take them.
    pub(crate) fn no_headset(&mut self, answers: &api::Answers) {
        for (asked, reply) in self.for_headset.drain(..) {
            info!("{asked:?}: no headset is connected");
            if let Some(r) = reply {
                answers.push(
                    r,
                    api::CtlReply::failed(
                        api::ApiError::NotConnected,
                        i18n::t("pc-control-no-headset-is-connected"),
                    ),
                );
            }
        }
    }

    /// Shows the current state and settings in the tray (only what changed): of the
    /// headsets, the one the tray follows (`headset`) and how many others there are.
    pub(crate) fn show(
        &mut self,
        displays: Option<&Displays>,
        store: &Store,
        headset: &TrayLine,
        others: usize,
        pairing: bool,
        desktop_unusable: bool,
    ) {
        let status = TrayStatus {
            no_limits: headset.no_limits,
            mic_denied: headset.mic_denied,
            connected: headset.via,
            others,
            monitors: displays.map_or(0, Displays::monitor_count),
            mirror: displays.is_some_and(Displays::mirror_active),
            paused: displays.is_some_and(|d| !d.streaming()),
            encoding: displays.and_then(Displays::encoding_line),
            pairing,
            desktop_stuck: displays.is_some_and(Displays::desk_stuck),
            usb_large: displays.and_then(Displays::large_mbit),
            desktop_unusable,
            input_ask: self.input_ask.clone(),
        };
        if self.shown.as_ref() != Some(&status) {
            tray::set_status(self.tray.as_ref(), status.clone());
            self.shown = Some(status);
        }
        if self.shown_battery != Some(headset.battery) {
            tray::set_battery(self.tray.as_ref(), headset.battery);
            self.shown_battery = Some(headset.battery);
        }
        let wanted = store.wanted();
        let counting = wanted.count == prostir_api::Counting::Yes;
        let settings = (wanted.bitrate, wanted.clear_text(), counting);
        if self.shown_settings != Some(settings) {
            tray::set_settings(self.tray.as_ref(), settings.0, settings.1, settings.2);
            self.shown_settings = Some(settings);
        }
    }
}

impl Brain {
    /// An item of the tray.
    pub(super) fn tray(&mut self, command: TrayCommand) {
        match command {
            TrayCommand::SetBitrate(mbps) => {
                info!("video quality: {mbps} Mbit/s per monitor");
                self.store.set_bitrate(mbps);
            }
            TrayCommand::SetSharp(sharp) => {
                info!("clearer text: {sharp}");
                self.store.change(|s| s.choose_clear_text(sharp));
            }
            // The tray's item answers the question too: checked is a yes.
            TrayCommand::SetCounting(on) => {
                let count = if on {
                    prostir_api::Counting::Yes
                } else {
                    prostir_api::Counting::No
                };
                self.set_counting(count);
            }
            TrayCommand::SetAutostart(on) => self.set_autostart(on),
            TrayCommand::Headset(asked) => {
                self.control.for_headset.push_back((asked, None));
                if self.sessions.is_empty() {
                    self.control.no_headset(&self.answers);
                }
            }
            TrayCommand::ShowSettings => {
                if let Err(e) = local::open_settings() {
                    warn!("{e}");
                }
            }
            TrayCommand::ForgetHeadsets => {
                let mut trusted = self.security.trusted.lock().unwrap();
                info!("forgetting {} paired headset(s)", trusted.len());
                if let Err(e) = trusted.clear() {
                    warn!("cannot forget headsets: {e}");
                }
                drop(trusted);
                self.keep_seen_of_paired();
                // None trusted is left: no old secret stays valid.
                self.security.new_find_secret(&[], |_| true);
                let keys: Vec<[u8; 32]> = self.sessions.keys().copied().collect();
                self.end_sessions(
                    keys,
                    PcGoodbye::Forgotten,
                    api::Reason::Forgotten,
                    || Ok(()),
                );
            }
            TrayCommand::PairNewHeadset => {
                self.door.open_pairing(Instant::now(), door::pairing_time());
                info!("pairing a new headset: from the tray");
            }
            TrayCommand::StopPairing => self.door.stop_pairing(),
            TrayCommand::AllowInput => {
                if let Some(key) = self.input_asks.first_waiting() {
                    info!("the person at the PC allows the mouse and keyboard: from the tray");
                    self.set_input(&key, true);
                }
            }
            TrayCommand::Quit => {
                self.quit = true;
                self.quit_asked = true;
            }
        }
        if let Some(d) = self.displays.as_mut() {
            d.apply(&self.store.wanted());
        }
    }
}
