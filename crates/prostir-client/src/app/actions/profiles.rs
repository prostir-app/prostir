//! Profiles: switching to one, saving the setup as one (a new one, or over one),
//! deleting one, naming one (from the PC).

use prostir_proto::request::{Primary, Request};
use prostir_proto::state::{self, DisplayId};

use super::super::{CONFIRM_FOR, Model};
use crate::note::Note;
use crate::pending::ProfileSwitch;
use crate::persist;
use crate::profiles::MAX_PROFILES;
use crate::ui::{self, Action};

impl Model {
    /// An action of this part of the menu (see `Model::act`).
    pub(super) fn act_profiles(&mut self, action: Action) {
        match action {
            Action::ApplyProfile(i) => {
                // Nothing there to switch to.
                let Some(p) = self.profiles.get(i).cloned() else {
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
                if self.world.monitors().is_empty() || i >= self.profiles.len() {
                    return;
                }
                let confirmed = self.menu.confirm_save.is_some_and(|(c, at)| {
                    c == i && self.now.saturating_duration_since(at) < CONFIRM_FOR
                });
                if !confirmed {
                    self.menu.confirm_save = Some((i, self.now));
                    return;
                }
                self.menu.confirm_save = None;
                // Saved over, it keeps its name.
                let mut profile = self.current_profile();
                if let Some(old) = self.profiles.get_mut(i) {
                    profile.name = old.name.take();
                    *old = profile;
                }
                self.profile_saved(i, action);
            }
            Action::NewProfile => {
                if self.world.monitors().is_empty() {
                    return;
                }
                if self.profiles.len() >= MAX_PROFILES {
                    let text = self
                        .texts()
                        .f("note-profiles-full", &[("count", MAX_PROFILES.into())]);
                    self.tell_press(Note::failed(Some(action), &text), true);
                    return;
                }
                // What asked to be pressed again was about the list before.
                self.menu.confirm_save = None;
                self.menu.confirm_delete = None;
                let profile = self.current_profile();
                self.profiles.push(profile);
                let i = self.profiles.len() - 1;
                // The page with the new one.
                self.menu.profile_page = u8::try_from(i / ui::PROFILES_PER_PAGE).unwrap_or(0);
                self.profile_saved(i, action);
            }
            Action::DeleteProfile(i) => {
                if i >= self.profiles.len() {
                    return;
                }
                let confirmed = self.menu.confirm_delete.is_some_and(|(c, at)| {
                    c == i && self.now.saturating_duration_since(at) < CONFIRM_FOR
                });
                if !confirmed {
                    self.menu.confirm_delete = Some((i, self.now));
                    return;
                }
                self.menu.confirm_delete = None;
                self.menu.confirm_save = None;
                // A switch waited for to this one or one after it: the profile it was
                // for is not where it was.
                if (i..=self.profiles.len()).any(|j| self.session.waits(ui::Asked::Profile(j))) {
                    self.world.switch_to(None);
                }
                let name = self.profile_name(i);
                self.profiles.remove(i);
                // The one in use, if another one, keeps being it.
                self.menu.active_profile = match self.menu.active_profile {
                    Some(a) if a == i => None,
                    Some(a) if a > i => Some(a - 1),
                    other => other,
                };
                let pages = self.profiles.len().div_ceil(ui::PROFILES_PER_PAGE).max(1);
                if usize::from(self.menu.profile_page) >= pages {
                    self.menu.profile_page = u8::try_from(pages - 1).unwrap_or(0);
                }
                self.saves.mark(persist::File::Profiles);
                self.profiles_changed();
                let text = self
                    .texts()
                    .f("note-profile-deleted", &[("name", name.into())]);
                self.tell_press(Note::done(Some(action), &text), true);
            }
            Action::ProfilePage(page) => self.menu.profile_page = page,
            // The others are not this part's (see `Model::act`).
            _ => {}
        }
    }

    /// Profile `i` was saved by `action`: it is the one in use, the file and the PC
    /// hear of it, and the button says where it went.
    fn profile_saved(&mut self, i: usize, action: Action) {
        self.saves.mark(persist::File::Profiles);
        self.menu.active_profile = Some(i);
        self.profiles_changed();
        let name = self.profile_name(i);
        let text = self
            .texts()
            .f("toast-profile-saved", &[("name", name.into())]);
        self.tell_press(Note::done(Some(action), &text), true);
    }
}
