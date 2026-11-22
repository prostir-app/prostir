//! Profiles: switching to one, saving the setup as one.

use prostir_proto::request::{Primary, Request};
use prostir_proto::state::{self, DisplayId};

use super::super::{CONFIRM_FOR, Model};
use crate::note::Note;
use crate::pending::ProfileSwitch;
use crate::persist;
use crate::ui::{self, Action};

impl Model {
    /// An action of this part of the menu (see `Model::act`).
    pub(super) fn act_profiles(&mut self, action: Action) {
        match action {
            Action::ApplyProfile(i) => {
                // An empty slot has nothing to switch to.
                let Some(p) = self.profiles.get(i).cloned().flatten() else {
                    return;
                };
                // The PC's own displays the profile showed that this PC has.
                let shown_physical: Vec<DisplayId> = p
                    .shown
                    .iter()
                    .filter(|k| self.display(**k).is_some_and(state::Display::is_physical))
                    .map(|k| k.id)
                    .collect();
                let req = self.request(
                    Request::SetDisplays {
                        modes: p.monitors.clone(),
                        primary: Some(Primary::Index { index: p.primary }),
                        shown_physical,
                    },
                    ui::Asked::Profile(i),
                );
                // Placed once the PC answers with its monitors' numbers. Cinema, the
                // background, the curve and the preset are not the profile's: they stay.
                let pc = self.session.main();
                self.world.switch_to(pc.zip(req).map(|(pc, req)| {
                    let switch = ProfileSwitch {
                        frame: p.frame,
                        rows: p.rows,
                        ..ProfileSwitch::new(req, p.places)
                    };
                    (pc, switch)
                }));
                // The monitors changing is the answer: the menu gets out of the way.
                self.close_panel();
            }
            Action::SaveProfile(i) => {
                // Nothing to save without monitors; a profile there is replaced on the
                // second press.
                if self.world.monitors().is_empty() {
                    return;
                }
                let replaces = self.profiles.get(i).is_some_and(Option::is_some);
                let confirmed = !replaces
                    || self.menu.confirm_save.is_some_and(|(c, at)| {
                        c == i && self.now.saturating_duration_since(at) < CONFIRM_FOR
                    });
                if confirmed {
                    self.menu.confirm_save = None;
                    let profile = self.current_profile();
                    if let Some(slot) = self.profiles.get_mut(i) {
                        *slot = Some(profile);
                        self.saves.mark(persist::File::Profiles);
                        self.menu.active_profile = Some(i);
                        let name = self.profile_name(i);
                        let text = self
                            .texts()
                            .f("toast-profile-saved", &[("name", name.into())]);
                        self.tell_press(Note::done(Some(action), &text), true);
                    }
                } else {
                    self.menu.confirm_save = Some((i, self.now));
                }
            }
            // The others are not this part's (see `Model::act`).
            _ => {}
        }
    }
}
