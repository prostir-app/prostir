//! What the network thread and the PC say, and what the PC asks for.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use glam::Vec2;
use log::{error, info, warn};
use openxr as xr;
use prostir_proto::ClientMsg;
use prostir_proto::request::{Asked, Command, Outcome};
use prostir_proto::state::{self, ReqId};

use super::{By, Scene, handle_action};
use crate::ids::{DisplayKey, PcId};
use crate::input::Controls;
use crate::main_loop::Monitor;
use crate::net::NetEvent;
use crate::note::Note;
use crate::session::{self, Attempt};
use crate::ui::{self, Action};
use crate::xrutil::{self, Gfx};
use crate::{decoder, passthrough, persist, physical, presence};

/// What the network thread said: the handles it brings (a session's outbox, a stream's
/// frames) stay here, the facts go to the core (`session`, `world`).
#[expect(
    clippy::disallowed_methods,
    reason = "until step 7.7 passes the time in and moves the threads to the shell"
)]
pub(crate) fn handle_net_event(
    scene: &mut Scene,
    session: &xr::Session<Gfx>,
    vm: &jni::JavaVM,
    event: NetEvent,
) {
    let now = Instant::now();
    match event {
        NetEvent::Dialed { step, addr } => scene.session.dialed(step, addr),
        NetEvent::Missed { step } => scene.session.missed(step, now),
        NetEvent::Hosts(hosts) => scene.session.hosts(hosts),
        NetEvent::Reached { step, pc, addr } => {
            // Reached through pairing: said once.
            let paired = match &scene.session.search().attempt {
                Some(Attempt::Pairing { step: s, view, .. }) if *s == step => {
                    Some(view.host.clone())
                }
                _ => None,
            };
            scene.session.reached(step, pc, addr, now);
            if let Some(host) = paired {
                let text = scene.texts().f("note-paired", &[("pc", host.into())]);
                scene.tell(Note::done(None, &text), true, true);
            }
        }
        NetEvent::PairOffer { key, addr, why } => {
            let texts = scene.texts().clone();
            scene.session.offered(key, addr, why, &texts);
        }
        NetEvent::Pairing {
            step,
            key,
            addr,
            code,
        } => {
            let texts = scene.texts().clone();
            scene.session.pairing(step, key, addr, code, &texts);
        }
        NetEvent::PairingFailed => scene.session.pairing_failed(),
        NetEvent::Incompatible { pc_newer, pc_app } => {
            warn!("the PC runs Prostir {pc_app}: versions differ");
            scene.session.incompatible(pc_newer, pc_app);
        }
        NetEvent::HostError(text) => scene.show_error(text),
        NetEvent::Paired(list) => {
            // A PC forgotten as asked: its places go, and the button says it.
            if let Some(f) = scene
                .forgetting
                .take_if(|f| list.iter().all(|p| p.pc != f.pc))
            {
                scene.forget_pc(f.pc);
                let text = scene
                    .texts()
                    .f("note-pc-forgotten", &[("pc", f.name.into())]);
                scene.tell(Note::done(Some(f.on), &text), true, f.in_menu);
            }
            scene.session.paired_list(list);
        }
        NetEvent::ForgetFailed { pc, why } => {
            if let Some(f) = scene.forgetting.take_if(|f| f.pc == pc) {
                scene.tell(Note::failed(Some(f.on), &why), true, f.in_menu);
            }
        }
        NetEvent::Connected {
            pc,
            addr,
            feedback,
            language,
            opened_with,
        } => {
            info!("connected to {addr}, PC {pc}, language {language:?}");
            let texts = scene.texts().clone();
            scene.session.connected_to(pc, addr, &texts);
            scene.outboxes.insert(pc, feedback);
            scene.world.connected(pc);
            if language.is_some() && language != scene.settings.pc_language {
                scene.settings.pc_language = language;
                scene.saves.mark(persist::File::Settings);
            }
            scene.notes.forget_requests(pc);
            // Whether the system lets the app record (the PC shows when it does not).
            scene.mic.refresh_permission();
            // What changed since `Open` went (it carried the state of that moment).
            let now = scene.headset_state();
            if now.up != opened_with.up {
                scene.mic_changed();
            }
            if now.prefs != opened_with.prefs {
                scene.prefs_changed();
            }
            if now.presence != opened_with.presence {
                scene.tell_presence();
            }
            // Asleep already as the session opened (`Open` says only "not worn").
            if scene.presence.state() == presence::State::Asleep {
                scene.tell_all(&ClientMsg::Asleep);
            }
            scene.ip_input = None;
        }
        NetEvent::Part(pc, part) => part_came(scene, pc, part),
        // Taken in the frame loop, where the menu's actions are done (see `take_command`).
        NetEvent::Command { .. } => {}
        NetEvent::Notice(_, text) => scene.show_toast(text),
        NetEvent::Reply { pc, req, outcome } => {
            match (scene.answered(pc, req, &outcome), outcome) {
                (Some(ui::Asked::Profile(_)), Outcome::Failed { .. }) => {
                    scene.world.switch_to(None);
                }
                // Said by `answered`.
                (None, _) | (_, Outcome::Failed { .. } | Outcome::Saved { .. }) => {}
                (_, Outcome::Pointer { id, u, v }) => {
                    let f = |v: u16| f32::from(v) / 65535.0;
                    scene.finder = Some((DisplayKey::new(pc, id), Vec2::new(f(u), f(v)), now));
                }
                (Some(ui::Asked::Profile(slot)), Outcome::Ids { ids }) => {
                    if scene.world.switching_with(pc, req) {
                        // Switched to: the PC made its monitors (which it shows comes
                        // with its list of displays). The menu is closed by now: a toast
                        // says it.
                        scene.active_profile = Some(slot);
                        let name = scene.profile_name(slot);
                        let text = scene
                            .texts()
                            .f("toast-profile-switched", &[("name", name.into())]);
                        let note = Note::done(Some(Action::ApplyProfile(slot)), &text);
                        scene.tell(note, true, true);
                        let keys = ids.into_iter().map(|id| DisplayKey::new(pc, id)).collect();
                        scene
                            .world
                            .profile_made(keys, &scene.settings, scene.head.1, now);
                    }
                }
                (Some(ui::Asked::AddMonitor), Outcome::Added { id }) => {
                    scene
                        .world
                        .adding_made(pc, req, Some(DisplayKey::new(pc, id)));
                }
                (_, Outcome::Done | Outcome::Added { .. } | Outcome::Ids { .. }) => {}
            }
        }
        NetEvent::Problem { error, detail, .. } => {
            warn!("a problem on the PC (code {})", error.code());
            scene.show_error(detail);
        }
        NetEvent::TooBig {
            name,
            size: (w, h),
            codec,
            ..
        } => {
            let text = scene.texts().f(
                "error-too-big",
                &[
                    ("name", name.into()),
                    ("size", format!("{w}×{h}").into()),
                    ("codec", format!("{codec:?}").into()),
                ],
            );
            scene.show_error(text);
        }
        NetEvent::Ended {
            step,
            pc,
            why,
            ended,
            lasted,
        } => {
            let gone = scene.session.ended(step, pc, &ended, lasted, now);
            let Some(pc) = pc else {
                return;
            };
            // What waited for an answer gets none, nor do the commands of that PC.
            for (req, _) in gone.waited {
                scene.given_up(pc, req, ui::Failure::Disconnected);
            }
            scene.notes.forget_requests(pc);
            scene.outboxes.remove(&pc);
            // The next session's list is news, not displays removed.
            scene.world.disconnected(pc);
            if ended.opened
                && let Some(id) = session::disconnect_text(why)
            {
                let note = Note::info(scene.texts().t(id));
                scene.tell(note, true, true);
            }
        }
        NetEvent::StreamStart {
            pc,
            id,
            generation,
            width,
            height,
            codec,
            frames,
            feedback,
        } => {
            let stream = decoder::Stream { id, generation };
            let id = DisplayKey::new(pc, id);
            scene.monitors.remove(&id);
            match xrutil::create_surface_swapchain(session, vm, width, height) {
                Ok((swapchain, window)) => {
                    let stop = Arc::new(AtomicBool::new(false));
                    let decoder = decoder::spawn(decoder::DecoderJob {
                        stream,
                        codec,
                        width,
                        height,
                        window,
                        frames,
                        feedback,
                        stop: stop.clone(),
                    });
                    scene.monitors.insert(
                        id,
                        Monitor {
                            width_px: width,
                            height_px: height,
                            decoder: Some(decoder),
                            stop,
                            swapchain,
                        },
                    );
                    scene.world.stream_started(
                        id,
                        (width, height),
                        &scene.settings,
                        scene.head,
                        now,
                    );
                }
                Err(e) => {
                    scene.world.stream_ended(id);
                    error!("monitor {id}: {e:#}");
                }
            }
        }
        // The stream stopped; the display keeps its place (only the list says it is gone).
        NetEvent::StreamEnd { pc, id } => {
            let k = DisplayKey::new(pc, id);
            scene.monitors.remove(&k);
            scene.world.stream_ended(k);
        }
    }
}

/// PC `pc` listed its displays anew (see `World::listed`): the displays gone end their
/// streams, and the switch of the PC's own displays shows how it stands.
#[expect(
    clippy::disallowed_methods,
    reason = "until step 7.7 passes the time in and moves the threads to the shell"
)]
pub(crate) fn displays_changed(scene: &mut Scene, pc: PcId) {
    let Some(displays) = scene.session.pc(pc).and_then(|p| p.state.displays.as_ref()) else {
        return;
    };
    // As the switch says: its display shown; with none connected, any one kept shown.
    if scene.session.main() == Some(pc) {
        scene.mirror_wanted = physical::switch(displays).map_or_else(
            || {
                displays
                    .displays
                    .iter()
                    .any(state::Display::is_shown_physical)
            },
            state::Display::is_shown_physical,
        );
    }
    let gone = scene
        .world
        .listed(pc, displays, &scene.settings, scene.head, Instant::now());
    for id in gone {
        scene.monitors.remove(id);
    }
}

/// A part of PC `pc`'s state came: kept, and what follows from it done.
pub(crate) fn part_came(scene: &mut Scene, pc: PcId, part: prostir_proto::HostMsg) {
    use prostir_proto::HostMsg as M;
    let listed = matches!(part, M::Displays { .. });
    if let M::HostCaps { part } = &part {
        info!("the PC can: {:?}", part.features);
    }
    // Checked by the network thread already: a part of the PC's state.
    if let Err(e) = scene.session.part(pc, part) {
        warn!("a part of the PC's state: {e}");
    }
    if listed {
        displays_changed(scene, pc);
    }
}

/// Does what the PC asks (its tray, later `prostir ctl` and hotkeys), as the menu would
/// (see `commands`), and answers it once: after the parts it changed (`Prefs`), and for
/// a profile once the PC answered the profile's own request.
pub(crate) fn take_command(
    scene: &mut Scene,
    pc: PcId,
    req: ReqId,
    asked: &Asked<Command>,
    passthrough: &mut Option<passthrough::Passthrough>,
    controls: &mut Controls,
) {
    use crate::commands::Does;
    use prostir_proto::state::ErrorCode;
    info!("the PC asks: {asked:?}");
    let outcome = match crate::commands::what_to_do(asked) {
        Does::Act(action) => {
            handle_action(scene, action, By::Elsewhere, passthrough, controls);
            Outcome::Done
        }
        Does::Profile(i) if scene.profiles.get(i).is_none_or(Option::is_none) => {
            Outcome::failed(ErrorCode::NO_SUCH_PROFILE, &(i + 1).to_string())
        }
        Does::Profile(i) => {
            let last = |scene: &Scene| scene.session.pc(pc).map(|p| p.asks.last());
            let before = last(scene);
            handle_action(
                scene,
                Action::ApplyProfile(i),
                By::Elsewhere,
                passthrough,
                controls,
            );
            let after = last(scene);
            if let Some(after) = after.filter(|a| Some(*a) > before)
                && let Some(p) = scene.session.pc_mut(pc)
            {
                // Answered with the PC's answer to the profile's monitors.
                p.asks.answer_command_after(after, req);
                return;
            }
            Outcome::failed(ErrorCode::OTHER, "no connection")
        }
        Does::Closer(steps) => {
            scene.closer(steps);
            Outcome::Done
        }
        Does::NoLimits(on) => {
            scene.settings.no_limits = on;
            scene.saves.mark(persist::File::Settings);
            scene.prefs_changed();
            Outcome::Done
        }
        Does::Fail(code, detail) => Outcome::failed(code, &detail),
    };
    scene.send_to(pc, ClientMsg::Reply { req, outcome });
}
