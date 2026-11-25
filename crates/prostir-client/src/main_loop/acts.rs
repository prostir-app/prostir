//! What the selftest asks for in a frame, done as the menu would do it.

use std::time::Instant;

use super::shell::Shell;
use super::{Act, Selftest};
use crate::app::{self, Event};
use crate::ui::{Action, Tab};

/// Does what `asked` says (asked in the last pass) at `now`, and forgets it.
pub fn act(shell: &mut Shell<'_>, now: Instant, asked: &mut Act, test: Option<&mut Selftest>) {
    match std::mem::replace(asked, Act::Nothing) {
        Act::MoveMonitor => {
            if let Some(&(id, _, _)) = shell.model.world().monitors().first() {
                let took = Instant::now();
                let action = Action::Move { id, delta: 1 };
                shell.handle(now, Event::Menu(action, app::By::Elsewhere));
                // Handed to the files' writer now, so the time is the whole of what
                // saving costs the frame.
                shell.model.end_pass();
                shell.apply(now);
                if let Some(t) = test {
                    t.saved(took.elapsed());
                }
            }
        }
        Act::OpenMenu => {
            shell.handle(now, Event::Menu(Action::OpenMenu, app::By::Elsewhere));
        }
        Act::SwitchTab => {
            let tab = match shell.model.menu().tab {
                Tab::Monitors => Tab::Quick,
                _ => Tab::Monitors,
            };
            let took = Instant::now();
            shell.handle(now, Event::Menu(Action::Tab(tab), app::By::Elsewhere));
            shell.model.end_pass();
            shell.apply(now);
            if let Some(t) = test {
                t.saved(took.elapsed());
            }
        }
        Act::CloseMenu => shell.handle(now, Event::Menu(Action::Close, app::By::Elsewhere)),
        Act::AddMonitor(width, height) => {
            let add = Action::Add { width, height };
            shell.handle(now, Event::Menu(add, app::By::Elsewhere));
        }
        Act::RemoveMonitor(id) => {
            // Pressed twice, as the menu asks to confirm.
            if let Some(k) = shell.model.key(id) {
                for _ in 0..2 {
                    shell.handle(now, Event::Menu(Action::Remove(k), app::By::Elsewhere));
                }
            }
        }
        Act::Inject(id, fault) => {
            // The fault on purpose, in the real decoder (the selftest's hook).
            #[cfg(all(feature = "selftest", not(feature = "store")))]
            if let Some(k) = shell.model.key(id) {
                shell.video.inject(k, fault);
            }
            #[cfg(not(all(feature = "selftest", not(feature = "store"))))]
            let _ = (id, fault);
        }
        Act::Nothing | Act::Finish => {}
    }
}
