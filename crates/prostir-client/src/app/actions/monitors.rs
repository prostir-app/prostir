//! The monitors: adding, removing, the order, the window panels, "Line up" and
//! "In front".

use std::time::Instant;

use prostir_proto::request::Request;
use prostir_proto::state::Mode;

use super::super::{CONFIRM_FOR, Scene};
use crate::ui::{self, Action};

#[expect(
    clippy::disallowed_methods,
    reason = "until step 7.7 passes the time in and moves the threads to the shell"
)]
pub(super) fn act(scene: &mut Scene, action: Action) {
    match action {
        Action::Add { width, height } => {
            let req = scene.request(
                Request::AddDisplay {
                    mode: Mode::new(width, height),
                },
                ui::Asked::AddMonitor,
            );
            // Shown at once where it will appear; the monitor takes that place once
            // its picture comes (not in cinema, where new monitors are not placed).
            scene
                .world
                .adding_started(req, (width, height), scene.head.1, Instant::now());
        }
        Action::Remove(id) => {
            let confirmed = scene
                .confirm_remove
                .is_some_and(|(c, at)| c == id && at.elapsed() < CONFIRM_FOR);
            if confirmed {
                scene.confirm_remove = None;
                scene.request_for(
                    id,
                    |id| Request::RemoveDisplay { id },
                    ui::Asked::Remove(id),
                );
            } else {
                scene.confirm_remove = Some((id, Instant::now()));
            }
        }
        Action::Recenter => {
            scene.recenter();
            // An open menu comes along, in front again; a closed one stays closed (the
            // palm menu has this button too).
            if scene.panel_open {
                let (head, forward) = scene.head;
                let touch = scene.hands_in_view;
                scene.open_panel(head, forward, touch);
            }
            scene.done(action);
        }
        Action::DetachWindow => {
            scene.request(Request::DetachWindow, ui::Asked::DetachWindow);
            // The panel appears in front: the menu steps aside.
            scene.panel_open = false;
        }
        Action::WindowHere => match scene.monitor_in_front() {
            Some(id) => {
                scene.request_for(id, |id| Request::WindowHere { id }, ui::Asked::WindowHere);
            }
            None => scene.no_monitor_in_front(action),
        },
        Action::Screenshot => match scene.monitor_in_front() {
            Some(id) => {
                scene.request_for(id, |id| Request::Screenshot { id }, ui::Asked::Screenshot);
            }
            None => scene.no_monitor_in_front(action),
        },
        Action::CloseWindow(id) => {
            scene.request_for(
                id,
                |id| Request::RemoveDisplay { id },
                ui::Asked::Remove(id),
            );
        }
        Action::Tidy => {
            scene.tidy_up();
            scene.done(action);
        }
        Action::SelectMonitor(id) => {
            scene.selected = Some(id);
            scene.confirm_remove = None;
        }
        Action::FindMouse => {
            scene.request(Request::FindMouse, ui::Asked::FindMouse);
        }
        Action::MakePrimary(id) => {
            scene.request_for(
                id,
                |id| Request::SetPrimary { id },
                ui::Asked::MakePrimary(id),
            );
        }
        // Swaps the places of a monitor and its neighbour in the order; the others
        // stay.
        Action::Move { id, delta } => scene.world.swap(id, delta),
        // The others are not this page's (see `handle_action`).
        _ => {}
    }
}
