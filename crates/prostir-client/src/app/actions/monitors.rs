//! The monitors: adding (at a scale), removing, another scale, the order, the window
//! panels, "Line up" and "In front".

use prostir_proto::request::Request;
use prostir_proto::state::Mode;

use super::super::{CONFIRM_FOR, Model};
use crate::ui::{self, Action};

impl Model {
    /// An action of this part of the menu (see `Model::act`).
    pub(super) fn act_monitors(&mut self, action: Action) {
        match action {
            Action::Add { width, height } => {
                // At the scale chosen, where the PC makes it.
                let offered =
                    self.pc().limits.as_ref().is_some_and(|l| {
                        self.can_scale() && l.scales.contains(&self.menu.add_scale)
                    });
                let scale = if offered { self.menu.add_scale } else { 100 };
                let req = self.request(
                    Request::AddDisplay {
                        mode: Mode {
                            scale,
                            ..Mode::new(width, height)
                        },
                    },
                    ui::Asked::AddMonitor,
                );
                // Shown at once where it will appear; the monitor takes that place once
                // its picture comes (not in cinema, where new monitors are not placed).
                self.world
                    .adding_started(req, (width, height), self.head.1, self.now);
            }
            Action::AddScale(scale) => self.menu.add_scale = scale,
            Action::SetScale(id, scale) => {
                self.request_for(
                    id,
                    |id| Request::SetScale { id, scale },
                    ui::Asked::Scale(id),
                );
            }
            Action::Remove(id) => {
                let confirmed = self.menu.confirm_remove.is_some_and(|(c, at)| {
                    c == id && self.now.saturating_duration_since(at) < CONFIRM_FOR
                });
                if confirmed {
                    self.menu.confirm_remove = None;
                    self.request_for(
                        id,
                        |id| Request::RemoveDisplay { id },
                        ui::Asked::Remove(id),
                    );
                } else {
                    self.menu.confirm_remove = Some((id, self.now));
                }
            }
            Action::Recenter => {
                self.recenter();
                // An open menu comes along, in front again; a closed one stays closed (the
                // palm menu has this button too).
                if self.menu.open {
                    self.open_panel();
                }
                self.done(action);
            }
            Action::DetachWindow => {
                self.request(Request::DetachWindow, ui::Asked::DetachWindow);
                // The panel appears in front: the menu steps aside.
                self.close_panel();
            }
            Action::WindowHere => match self.monitor_in_front() {
                Some(id) => {
                    self.request_for(id, |id| Request::WindowHere { id }, ui::Asked::WindowHere);
                }
                None => self.no_monitor_in_front(action),
            },
            Action::Screenshot => match self.monitor_in_front() {
                Some(id) => {
                    self.request_for(id, |id| Request::Screenshot { id }, ui::Asked::Screenshot);
                }
                None => self.no_monitor_in_front(action),
            },
            Action::CloseWindow(id) => {
                self.request_for(
                    id,
                    |id| Request::RemoveDisplay { id },
                    ui::Asked::Remove(id),
                );
            }
            Action::Tidy => {
                self.tidy_up();
                self.done(action);
            }
            Action::SelectMonitor(id) => {
                self.menu.selected = Some(id);
                self.menu.confirm_remove = None;
            }
            Action::FindMouse => {
                self.request(Request::FindMouse, ui::Asked::FindMouse);
            }
            Action::MakePrimary(id) => {
                self.request_for(
                    id,
                    |id| Request::SetPrimary { id },
                    ui::Asked::MakePrimary(id),
                );
            }
            // Swaps the places of a monitor and its neighbour in the order; the others
            // stay.
            Action::Move { id, delta } => self.world.swap(&self.settings, id, delta),
            // The others are not this part's (see `Model::act`).
            _ => {}
        }
    }
}
