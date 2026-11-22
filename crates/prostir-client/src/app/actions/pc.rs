//! The PC: finding and pairing, forgetting, its encoding, sound and own displays, the
//! microphone, the mouse.

use std::time::Instant;

use log::info;
use prostir_proto::request::Request;

use super::super::{CONFIRM_FOR, Scene};
use super::Forgetting;
use crate::note::{self, Note};
use crate::ui::{self, Action};
use crate::{net, persist};

#[expect(
    clippy::disallowed_methods,
    reason = "until step 7.7 passes the time in and moves the threads to the shell"
)]
pub(super) fn act(scene: &mut Scene, action: Action) {
    match action {
        Action::GazeMouse(on) => {
            scene.settings.gaze_mouse = on;
            scene.saves.mark(persist::File::Settings);
            scene.prefs_changed();
        }
        Action::SetMic(enabled) => {
            scene.mic_wanted = enabled;
            scene.settings.mic = enabled;
            scene.saves.mark(persist::File::Settings);
            scene.mic.set_allowed(enabled);
            scene.mic_changed();
            let note = note::mic_note(enabled, scene.mic.permission(), scene.texts());
            scene.tell_press(note, false);
        }
        Action::SetAudio(enabled) => {
            // Shown at once; the PC confirms (or corrects) with its `Sound`.
            if let Some(p) = scene.session.main().and_then(|pc| scene.session.pc_mut(pc))
                && let Some(sound) = &mut p.state.sound
            {
                sound.to_headset = enabled;
            }
            scene.request(
                Request::SetSound {
                    to_headset: enabled,
                },
                ui::Asked::Sound,
            );
        }
        Action::SetMirror(enabled) => {
            if let Some(id) = scene.physical_id() {
                scene.mirror_wanted = enabled;
                scene.request_for(
                    id,
                    |id| Request::ShowPhysical { id, shown: enabled },
                    ui::Asked::Mirror,
                );
            }
        }
        Action::AskMic => scene.mic.ask_again(),
        Action::ForgetPc(i) => {
            let confirmed = scene
                .confirm_forget
                .is_some_and(|(c, at)| c == i && at.elapsed() < CONFIRM_FOR);
            if !confirmed {
                scene.confirm_forget = Some((i, Instant::now()));
            } else if let Some(pc) = scene.session.paired().get(i).map(|p| p.pc) {
                scene.confirm_forget = None;
                info!("forgetting PC {pc}");
                // The network thread leaves it if connected (by its key, not its name:
                // two PCs may share one), drops it and says the new list; the places of
                // its displays go once it did (see `NetEvent::Paired`), and the button
                // says how it went then.
                let name = scene.session.paired().get(i).map(|p| p.name.clone());
                scene.session.forget(pc);
                scene.forgetting = Some(Forgetting {
                    pc,
                    name: name.unwrap_or_default(),
                    on: action,
                    in_menu: scene.press_in_menu,
                });
            }
        }
        Action::SetEncoding(change) => {
            // Shown at once; the PC confirms (or corrects) with its `Encoding`.
            if let Some(pc) = scene
                .session
                .main()
                .and_then(|pc| scene.session.pc_mut(pc))
                .and_then(|p| p.state.encoding.as_mut())
            {
                let w = &mut pc.wanted;
                w.bitrate = change.bitrate.unwrap_or(w.bitrate);
                w.codec = change.codec.or(w.codec);
                w.software = change.software.unwrap_or(w.software);
                w.sharp = change.sharp.unwrap_or(w.sharp);
            }
            scene.request(
                Request::SetEncoding {
                    bitrate: change.bitrate,
                    codec: change.codec,
                    software: change.software,
                    sharp: change.sharp,
                },
                ui::Asked::Encoding,
            );
        }
        Action::ConnectTo(i) => {
            if let Some(addr) = scene.session.search().found.get(i).map(|h| h.addr) {
                scene.session.connect_to(addr, Instant::now());
            }
        }
        Action::ShowKeypad => scene.ip_input = Some(String::new()),
        Action::Key(c) if scene.session.pairing_mut().is_some() => {
            if let Some(p) = scene.session.pairing_mut()
                && p.typed.len() < 6
                && c.is_ascii_digit()
            {
                p.typed.push(c);
            }
        }
        Action::Backspace if scene.session.pairing_mut().is_some() => {
            if let Some(p) = scene.session.pairing_mut() {
                p.typed.pop();
            }
        }
        Action::ConnectTyped if scene.session.pairing_mut().is_some() => {
            if let Some(p) = scene.session.pairing_mut() {
                let code = std::mem::take(&mut p.typed);
                p.failed = false;
                scene.session.submit_code(code);
            }
        }
        Action::PairAnswer(pair) => scene.session.answer_offer(pair, Instant::now()),
        Action::PairOtherWay => scene.session.switch_pairing_side(),
        Action::Key(c) => {
            if let Some(t) = &mut scene.ip_input
                && t.len() < 21
            {
                t.push(c);
            }
        }
        Action::Backspace => {
            if let Some(t) = &mut scene.ip_input {
                t.pop();
            }
        }
        Action::ConnectTyped => {
            let typed = scene.ip_input.clone().unwrap_or_default();
            match net::discovery::parse_addr(&typed) {
                Some(addr) => {
                    scene.session.connect_to(addr, Instant::now());
                    scene.ip_input = None;
                }
                None => {
                    let text = scene.texts().f("error-not-ip", &[("typed", typed.into())]);
                    scene.tell_press(Note::failed(Some(action), &text), true);
                }
            }
        }
        Action::CancelKeypad => scene.ip_input = None,
        Action::ChangeHost => scene.session.change_pc(),
        // The others are not this page's (see `handle_action`).
        _ => {}
    }
}
