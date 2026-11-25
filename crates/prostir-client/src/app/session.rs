//! What the network thread and the PC say, and what the PC asks for (its commands).

use std::net::SocketAddr;

use glam::Vec2;
use log::{info, warn};
use prostir_proto::ClientMsg;
use prostir_proto::request::{Asked, Command, Outcome};
use prostir_proto::state::{HeadsetState, ReqId};

use super::{By, Effect, Model, VideoCmd};
use crate::ids::{DisplayKey, PcId};
use crate::lang::Lang;
use crate::net::NetEvent;
use crate::note::Note;
use crate::session::{self, Attempt};
use crate::ui::{self, Action};
use crate::{persist, presence};

impl Model {
    /// What the network thread said (the shell kept the handles it brought).
    pub(super) fn net_event(&mut self, event: NetEvent) {
        let now = self.now;
        match event {
            NetEvent::Dialed { step, addr } => self.session.dialed(step, addr),
            NetEvent::Missed { step } => self.session.missed(step, now),
            NetEvent::Heard(host) => {
                let texts = self.texts().clone();
                self.session.heard(host, now, &texts);
            }
            NetEvent::Reached { step, pc, addr } => {
                // Reached through pairing: said once.
                let paired = match &self.session.search().attempt {
                    Some(Attempt::Pairing { step: s, view, .. }) if *s == step => {
                        Some(view.host.clone())
                    }
                    _ => None,
                };
                self.session.reached(step, pc, addr, now);
                if let Some(host) = paired {
                    self.say_paired(host);
                }
            }
            NetEvent::PairOffer { key, addr, why } => {
                let texts = self.texts().clone();
                self.session.offered(key, addr, why, &texts);
            }
            NetEvent::Pairing {
                step,
                key,
                addr,
                code,
            } => {
                let texts = self.texts().clone();
                self.session.pairing(step, key, addr, code, &texts);
            }
            NetEvent::PairingFailed => self.session.pairing_failed(),
            NetEvent::Incompatible { pc_newer, pc_app } => {
                warn!("the PC runs Prostir {pc_app}: versions differ");
                self.session.incompatible(pc_newer, pc_app);
            }
            NetEvent::HostError(text) => self.show_error(text),
            NetEvent::Paired(list) => {
                // A PC forgotten as asked: its places go, and the button says it.
                if let Some(f) = self
                    .forgetting
                    .take_if(|f| list.iter().all(|p| p.pc != f.pc))
                {
                    self.forget_pc(f.pc);
                    let text = self
                        .texts()
                        .f("headset-note-pc-forgotten", &[("pc", f.name.into())]);
                    self.tell(Note::done(Some(f.on), &text), true, f.in_menu);
                }
                self.session.paired_list(list);
            }
            NetEvent::ForgetFailed { pc, why } => {
                self.session.forget_failed(pc);
                if let Some(f) = self.forgetting.take_if(|f| f.pc == pc) {
                    self.tell(Note::failed(Some(f.on), &why), true, f.in_menu);
                }
            }
            // The shell makes these `Event::Connected` and `Event::StreamStarted`,
            // keeping their handles.
            NetEvent::Connected { .. } | NetEvent::StreamStart { .. } => {
                warn!("a network event with a handle, not taken by the shell");
            }
            NetEvent::Part(pc, part) => self.part_came(pc, part),
            NetEvent::Command { pc, req, asked } => self.take_command(pc, req, &asked),
            NetEvent::VolumeAsk { pc, asked } => self.volume_asked(pc, &asked),
            NetEvent::Notice(_, text) => self.show_toast(text),
            NetEvent::Reply { pc, req, outcome } => {
                match (self.answered(pc, req, &outcome), outcome) {
                    // Only the switch it asked for: another one asked since goes on.
                    (Some(ui::Asked::Profile(_)), Outcome::Failed { .. })
                        if self.world.switching_with(pc, req) =>
                    {
                        self.world.switch_to(None);
                    }
                    // Said by `answered`.
                    (None, _) | (_, Outcome::Failed { .. } | Outcome::Saved { .. }) => {}
                    (_, Outcome::Pointer { id, u, v }) => {
                        let f = |v: u16| f32::from(v) / 65535.0;
                        self.finder = Some((DisplayKey::new(pc, id), Vec2::new(f(u), f(v)), now));
                    }
                    (Some(ui::Asked::Profile(slot)), Outcome::Ids { ids }) => {
                        if self.world.switching_with(pc, req) {
                            // Switched to: the PC made its monitors (which it shows comes
                            // with its list of displays). The menu is closed by now: a
                            // toast says it.
                            self.menu.active_profile = Some(slot);
                            self.profiles_changed();
                            let name = self.profile_name(slot);
                            let text = self
                                .texts()
                                .f("headset-toast-profile-switched", &[("name", name.into())]);
                            let note = Note::done(Some(Action::ApplyProfile(slot)), &text);
                            self.tell(note, true, true);
                            let keys = ids.into_iter().map(|id| DisplayKey::new(pc, id)).collect();
                            self.world
                                .profile_made(keys, &self.settings, self.head.1, now);
                        }
                    }
                    (Some(ui::Asked::AddMonitor), Outcome::Added { id }) => {
                        self.world
                            .adding_made(pc, req, Some(DisplayKey::new(pc, id)));
                    }
                    (_, Outcome::Done | Outcome::Added { .. } | Outcome::Ids { .. }) => {}
                }
            }
            NetEvent::Problem { error, detail, .. } => {
                warn!("a problem on the PC (code {})", error.code());
                self.show_error(detail);
            }
            NetEvent::TooBig {
                name,
                size: (w, h),
                codec,
                ..
            } => {
                let text = self.texts().f(
                    "headset-error-too-big",
                    &[
                        ("name", name.into()),
                        ("size", format!("{w}×{h}").into()),
                        ("codec", format!("{codec:?}").into()),
                    ],
                );
                self.show_error(text);
            }
            NetEvent::Ended {
                step,
                pc,
                why,
                ended,
                lasted,
            } => {
                let gone = self.session.ended(step, pc, &ended, lasted, now);
                let Some(pc) = pc else {
                    return;
                };
                // What waited for an answer gets none, nor do the commands of that PC.
                for (req, _) in gone.waited {
                    self.given_up(pc, req, ui::Failure::Disconnected);
                }
                self.menu.notes.forget_requests(pc);
                self.menu.physical_asked.retain(|(k, _)| k.pc != pc);
                self.sessions.remove(&pc);
                self.watch_volume();
                self.videos.pc_ended(pc);
                // Its streams went with it, also one whose end the network never said.
                self.effects.extend(
                    self.world
                        .streams_of(pc)
                        .map(|k| Effect::Video(VideoCmd::Drop(k))),
                );
                // The next session's list is news, not displays removed.
                self.world.disconnected(pc);
                if ended.opened
                    && let Some(id) = session::disconnect_text(why)
                {
                    let note = Note::info(self.texts().t(id));
                    self.tell(note, true, true);
                }
            }
            // The stream stopped; the display keeps its place (only the list says it is
            // gone).
            NetEvent::StreamEnd { pc, id } => {
                let k = DisplayKey::new(pc, id);
                self.effects.push(Effect::Video(VideoCmd::Drop(k)));
                self.videos.ended(k);
                self.world.stream_ended(k);
            }
        }
    }

    /// The session with PC `pc` is open (its outbox is the shell's from now on).
    pub(super) fn connected(
        &mut self,
        pc: PcId,
        addr: SocketAddr,
        language: Option<Lang>,
        opened_with: &HeadsetState,
    ) {
        info!("connected to {addr}, PC {pc}, language {language:?}");
        // Paired again inside the session's opening (the PC had forgotten the
        // headset): said as for a new PC.
        let paired = match &self.session.search().attempt {
            Some(Attempt::Pairing { key, view, .. }) if PcId::of(key) == pc => {
                Some(view.host.clone())
            }
            _ => None,
        };
        let texts = self.texts().clone();
        self.session.connected_to(pc, addr, &texts);
        if let Some(host) = paired {
            self.say_paired(host);
        }
        self.sessions.insert(pc);
        self.watch_volume();
        self.world.connected(pc);
        if language.is_some() && language != self.settings.pc_language {
            self.settings.pc_language = language;
            self.saves.mark(persist::File::Settings);
        }
        self.menu.notes.forget_requests(pc);
        // Whether the system lets the app record (the PC shows when it does not): the
        // shell looks, and says what it found as `PlatformEvent::Mic`.
        self.effects
            .push(Effect::Mic(super::MicCmd::RefreshPermission));
        // What changed since `Open` went (it carried the state of that moment).
        let now = self.headset_state();
        if now.up != opened_with.up {
            self.mic_changed();
        }
        if now.prefs != opened_with.prefs {
            self.prefs_changed();
        }
        if now.presence != opened_with.presence {
            self.tell_presence();
        }
        if now.profiles != opened_with.profiles {
            self.tell_profiles();
        }
        if let Some(part) = now.volume
            && now.volume != opened_with.volume
        {
            self.effects
                .push(Effect::Tell(pc, ClientMsg::Volume { part }));
        }
        // Asleep already as the session opened (`Open` says only "not worn").
        if self.presence.state() == presence::State::Asleep {
            self.tell_all(&ClientMsg::Asleep);
        }
        self.menu.ip_input = None;
    }

    /// The headset paired with the PC called `host`: said once.
    fn say_paired(&mut self, host: String) {
        let text = self
            .texts()
            .f("headset-note-paired", &[("pc", host.into())]);
        self.tell(Note::done(None, &text), true, true);
    }

    /// PC `pc` listed its displays anew (see `World::listed`): the displays gone end
    /// their streams.
    fn displays_changed(&mut self, pc: PcId) {
        let Some(displays) = self.session.pc(pc).and_then(|p| p.state.displays.as_ref()) else {
            return;
        };
        let gone = self
            .world
            .listed(pc, displays, &self.settings, self.head, self.now);
        for id in gone {
            self.videos.ended(*id);
            self.effects.push(Effect::Video(VideoCmd::Drop(*id)));
        }
    }

    /// A part of PC `pc`'s state came: kept, and what follows from it done.
    fn part_came(&mut self, pc: PcId, part: prostir_proto::HostMsg) {
        use prostir_proto::HostMsg as M;
        let listed = matches!(part, M::Displays { .. });
        if let M::HostCaps { part } = &part {
            info!("the PC can: {:?}", part.features);
        }
        // What the PC says of its encoding or sound is how it stands: what was shown
        // before a request about it is not put back any more.
        match &part {
            M::Encoding { .. } => {
                self.menu.encoding_before.take_if(|(p, ..)| *p == pc);
            }
            M::Sound { .. } => {
                self.menu.sound_before.take_if(|(p, ..)| *p == pc);
            }
            _ => {}
        }
        // Checked by the network thread already: a part of the PC's state.
        if let Err(e) = self.session.part(pc, part) {
            warn!("a part of the PC's state: {e}");
        }
        if listed {
            self.displays_changed(pc);
        }
    }

    /// Does what the PC asks (its tray, `prostir ctl`, hotkeys), as the menu would (see
    /// `commands`), and answers it once: after the parts it changed (`Prefs`), and for a
    /// profile once the PC answered the profile's own request.
    fn take_command(&mut self, pc: PcId, req: ReqId, asked: &Asked<Command>) {
        use crate::commands::Does;
        use prostir_proto::state::ErrorCode;
        info!("the PC asks: {asked:?}");
        let texts = self.texts().clone();
        let menu_name = |n: usize| texts.f("headset-profile-n", &[("n", n.into())]);
        let outcome = match crate::commands::what_to_do(asked, &self.profiles, &menu_name) {
            Does::Act(action) => {
                self.act(action, By::Elsewhere);
                Outcome::Done
            }
            // The names are the person's: only the PC the menu acts on names them.
            Does::Rename(..) if self.session.main() != Some(pc) => {
                Outcome::failed(ErrorCode::NOT_ALLOWED, "")
            }
            Does::Rename(i, name) => {
                if let Some(p) = self.profiles.get_mut(i) {
                    info!("profile {} named anew", i + 1);
                    p.name = name;
                    self.saves.mark(persist::File::Profiles);
                    // The PC hears the new name before the answer.
                    self.rev += 1;
                    self.tell_profiles();
                }
                Outcome::Done
            }
            Does::Profile(i) => {
                let last = |m: &Self| m.session.pc(pc).map(|p| p.asks.last());
                let before = last(self);
                self.act(Action::ApplyProfile(i), By::Elsewhere);
                let after = last(self);
                if let Some(after) = after.filter(|a| Some(*a) > before)
                    && let Some(p) = self.session.pc_mut(pc)
                {
                    // Answered with the PC's answer to the profile's monitors.
                    p.asks.answer_command_after(after, req);
                    return;
                }
                Outcome::failed(ErrorCode::OTHER, "no connection")
            }
            Does::Fail(code, detail) => Outcome::failed(code, &detail),
        };
        self.send_to(pc, ClientMsg::Reply { req, outcome });
    }
}
