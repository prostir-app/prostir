//! The PC: finding and pairing, forgetting, its encoding, sound and own displays, the
//! microphone, the mouse, "No limits".

use log::info;
use prostir_proto::request::Request;

use super::super::{CONFIRM_FOR, Effect, MicCmd, Model};
use super::Forgetting;
use crate::note::{self, Note};
use crate::ui::{self, Action};
use crate::{net, persist};

impl Model {
    /// An action of this part of the menu (see `Model::act`).
    pub(super) fn act_pc(&mut self, action: Action) {
        match action {
            Action::GazeMouse(on) => {
                self.settings.gaze_mouse = on;
                self.saves.mark(persist::File::Settings);
                self.prefs_changed();
            }
            Action::SetMic(enabled) => {
                self.headset.mic_wanted = enabled;
                self.settings.mic = enabled;
                self.saves.mark(persist::File::Settings);
                self.effects.push(Effect::Mic(MicCmd::Allowed(enabled)));
                self.mic_changed();
                let note = note::mic_note(enabled, self.headset.mic_permission, self.texts());
                self.tell_press(note, false);
            }
            Action::SetAudio(enabled) => {
                // Shown at once; the PC confirms (or corrects) with its `Sound`, and a
                // refusal puts it back.
                let pc = self.session.main();
                let mut before = None;
                if let Some(p) = pc.and_then(|pc| self.session.pc_mut(pc))
                    && let Some(sound) = &mut p.state.sound
                {
                    before = Some(sound.to_headset);
                    sound.to_headset = enabled;
                }
                let req = self.request(
                    Request::SetSound {
                        to_headset: enabled,
                    },
                    ui::Asked::Sound,
                );
                if let (Some(pc), Some(req), Some(before)) = (pc, req, before) {
                    self.menu.sound_before = Some((pc, req, before));
                } else if let (Some(pc), Some(before)) = (pc, before) {
                    // Not sent: as it was.
                    if let Some(sound) =
                        self.session.pc_mut(pc).and_then(|p| p.state.sound.as_mut())
                    {
                        sound.to_headset = before;
                    }
                }
            }
            Action::ShowPhysical(k, shown) => {
                // The switch moves at once; the PC's list says how it stands after.
                self.menu.physical_asked.retain(|(a, _)| *a != k);
                self.menu.physical_asked.push((k, shown));
                let sent = self.request_for(
                    k,
                    |id| Request::ShowPhysical { id, shown },
                    ui::Asked::Physical(k),
                );
                if sent.is_none() {
                    self.menu.physical_asked.retain(|(a, _)| *a != k);
                } else {
                    self.leave_profile();
                }
            }
            Action::PhysicalPage(page) => self.menu.physical_page = page,
            Action::NoLimits(on) => {
                self.settings.no_limits = on;
                self.saves.mark(persist::File::Settings);
                self.prefs_changed();
                self.done(action);
            }
            Action::AskMic => self.effects.push(Effect::Mic(MicCmd::AskAgain)),
            Action::ForgetPc(i) => {
                let confirmed = self.menu.confirm_forget.is_some_and(|(c, at)| {
                    c == i && self.now.saturating_duration_since(at) < CONFIRM_FOR
                });
                if !confirmed {
                    self.menu.confirm_forget = Some((i, self.now));
                } else if let Some(pc) = self.session.paired().get(i).map(|p| p.pc) {
                    self.menu.confirm_forget = None;
                    info!("forgetting PC {pc}");
                    // The network thread leaves it if connected (by its key, not its name:
                    // two PCs may share one), drops it and says the new list; the places of
                    // its displays go once it did (see `NetEvent::Paired`), and the button
                    // says how it went then.
                    let name = self.session.paired().get(i).map(|p| p.name.clone());
                    self.session.forget(pc);
                    self.forgetting = Some(Forgetting {
                        pc,
                        name: name.unwrap_or_default(),
                        on: action,
                        in_menu: self.press_in_menu,
                    });
                }
            }
            Action::SetEncoding(change) => {
                // Shown at once; the PC confirms (or corrects) with its `Encoding`, and a
                // refusal puts it back.
                let pc = self.session.main();
                let mut before = None;
                if let Some(e) = pc
                    .and_then(|pc| self.session.pc_mut(pc))
                    .and_then(|p| p.state.encoding.as_mut())
                {
                    let w = &mut e.wanted;
                    before = Some(*w);
                    w.bitrate = change.bitrate.unwrap_or(w.bitrate);
                    w.codec = change.codec.or(w.codec);
                    w.software = change.software.unwrap_or(w.software);
                    w.sharp = change.sharp.unwrap_or(w.sharp);
                }
                let req = self.request(
                    Request::SetEncoding {
                        bitrate: change.bitrate,
                        codec: change.codec,
                        software: change.software,
                        sharp: change.sharp,
                    },
                    ui::Asked::Encoding,
                );
                match (pc, req, before) {
                    (Some(pc), Some(req), Some(before)) => {
                        self.menu.encoding_before = Some((pc, req, before));
                    }
                    // Not sent: as it was.
                    (Some(pc), None, Some(before)) => {
                        if let Some(e) = self
                            .session
                            .pc_mut(pc)
                            .and_then(|p| p.state.encoding.as_mut())
                        {
                            e.wanted = before;
                        }
                    }
                    _ => {}
                }
            }
            Action::ConnectTo(i) => self.session.pick_found(i, self.now),
            Action::ShowKeypad => self.menu.ip_input = Some(String::new()),
            Action::Key(c) if self.session.pairing_mut().is_some() => {
                if let Some(p) = self.session.pairing_mut()
                    && p.typed.len() < 6
                    && c.is_ascii_digit()
                {
                    p.typed.push(c);
                }
            }
            Action::Backspace if self.session.pairing_mut().is_some() => {
                if let Some(p) = self.session.pairing_mut() {
                    p.typed.pop();
                }
            }
            Action::ConnectTyped if self.session.pairing_mut().is_some() => {
                if let Some(p) = self.session.pairing_mut() {
                    let code = std::mem::take(&mut p.typed);
                    p.failed = false;
                    self.session.submit_code(code);
                }
            }
            Action::PairAnswer(pair) => self.session.answer_offer(pair, self.now),
            Action::PairOtherWay => self.session.switch_pairing_side(self.now),
            Action::Key(c) => {
                if let Some(t) = &mut self.menu.ip_input
                    && t.len() < 21
                {
                    t.push(c);
                }
            }
            Action::Backspace => {
                if let Some(t) = &mut self.menu.ip_input {
                    t.pop();
                }
            }
            Action::ConnectTyped => {
                let typed = self.menu.ip_input.clone().unwrap_or_default();
                match net::discovery::parse_addr(&typed) {
                    Some(addr) => {
                        self.session.connect_to(addr, self.now);
                        self.menu.ip_input = None;
                    }
                    None => {
                        let text = self.texts().f("error-not-ip", &[("typed", typed.into())]);
                        self.tell_press(Note::failed(Some(action), &text), true);
                    }
                }
            }
            Action::CancelKeypad => self.menu.ip_input = None,
            Action::ChangeHost => self.session.change_pc(),
            // The others are not this part's (see `Model::act`).
            _ => {}
        }
    }
}
