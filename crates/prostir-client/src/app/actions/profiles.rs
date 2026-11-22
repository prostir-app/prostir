//! Profiles: switching to one, saving the setup as one.

use std::time::Instant;

use prostir_proto::request::{Primary, Request};
use prostir_proto::state::{self, DisplayId};

use super::super::{CONFIRM_FOR, Scene};
use crate::note::Note;
use crate::pending::ProfileSwitch;
use crate::persist;
use crate::ui::{self, Action};

#[expect(
    clippy::disallowed_methods,
    reason = "until step 7.7 passes the time in and moves the threads to the shell"
)]
pub(super) fn act(scene: &mut Scene, action: Action) {
    match action {
        Action::ApplyProfile(i) => {
            // An empty slot has nothing to switch to.
            let Some(p) = scene.profiles.get(i).cloned().flatten() else {
                return;
            };
            // The PC's own displays the profile showed that this PC has.
            let shown_physical: Vec<DisplayId> = p
                .shown
                .iter()
                .filter(|k| scene.display(**k).is_some_and(state::Display::is_physical))
                .map(|k| k.id)
                .collect();
            let req = scene.request(
                Request::SetDisplays {
                    modes: p.monitors.clone(),
                    primary: Some(Primary::Index { index: p.primary }),
                    shown_physical,
                },
                ui::Asked::Profile(i),
            );
            // Placed once the PC answers with its monitors' numbers. Cinema, the
            // background, the curve and the preset are not the profile's: they stay.
            let pc = scene.session.main();
            scene.world.switch_to(pc.zip(req).map(|(pc, req)| {
                let switch = ProfileSwitch {
                    frame: p.frame,
                    rows: p.rows,
                    ..ProfileSwitch::new(req, p.places)
                };
                (pc, switch)
            }));
            // The monitors changing is the answer: the menu gets out of the way.
            scene.panel_open = false;
        }
        Action::SaveProfile(i) => {
            // Nothing to save without monitors; a profile there is replaced on the
            // second press.
            if scene.world.monitors().is_empty() {
                return;
            }
            let replaces = scene.profiles.get(i).is_some_and(Option::is_some);
            let confirmed = !replaces
                || scene
                    .confirm_save
                    .is_some_and(|(c, at)| c == i && at.elapsed() < CONFIRM_FOR);
            if confirmed {
                scene.confirm_save = None;
                let profile = scene.current_profile();
                if let Some(slot) = scene.profiles.get_mut(i) {
                    *slot = Some(profile);
                    scene.saves.mark(persist::File::Profiles);
                    scene.active_profile = Some(i);
                    let name = scene.profile_name(i);
                    let text = scene
                        .texts()
                        .f("toast-profile-saved", &[("name", name.into())]);
                    scene.tell_press(Note::done(Some(action), &text), true);
                }
            } else {
                scene.confirm_save = Some((i, Instant::now()));
            }
        }
        // The others are not this page's (see `handle_action`).
        _ => {}
    }
}
