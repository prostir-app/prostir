//! The model as the hands see it (see `interaction`): what they point at and hold, and
//! what they do, each done at once (a press is the menu's action, as from any button).

use std::time::Instant;

use glam::Vec3;

use super::{By, Effect, Model, SystemCmd};
use crate::ids::{DisplayKey, Target, Thing};
use crate::interaction::Scene;
use crate::layout::{Placement, Ray};
use crate::ui::{self, Action};
use crate::world::Pickable;

impl Scene for Model {
    fn pick(&self, ray: Ray, attach: Option<DisplayKey>) -> Option<Target> {
        let can = Pickable {
            menu: self.menu.open,
            attach,
        };
        let session = &self.session;
        self.world.pick(ray, &self.settings, can, |k| {
            session.waits(ui::Asked::Remove(k))
        })
    }

    fn shows(&self, id: DisplayKey) -> bool {
        self.world.streams(id)
    }

    fn is_window(&self, id: DisplayKey) -> bool {
        self.world.is_window(id)
    }

    fn menu(&self) -> Option<Placement> {
        self.menu.open.then(|| self.world.menu())
    }

    fn large_menu(&self) -> bool {
        self.settings.large_menu
    }

    fn left_handed(&self) -> bool {
        self.settings.left_handed
    }

    fn wrist(&self) -> Option<Placement> {
        self.world.wrist()
    }

    fn place(&self, thing: Thing) -> Option<Placement> {
        self.world.place(thing)
    }

    fn holds(&self, thing: Thing) -> bool {
        self.world.holds(thing)
    }

    fn resizing(&self) -> Option<DisplayKey> {
        self.world.resizing()
    }

    fn palm(&mut self, place: Option<Placement>) {
        self.world.set_wrist(place);
        // The palm menu shows the battery: the system thread reads it afresh when the
        // menu appears, and more often while it shows.
        let shown = place.is_some();
        if shown != self.palm_shown {
            self.palm_shown = shown;
            self.effects
                .push(Effect::System(SystemCmd::PalmMenu(shown)));
        }
    }

    fn press_menu(&mut self, now: Instant, action: Action) {
        self.handle(now, super::Event::Menu(action, By::Menu));
    }

    fn press(&mut self, now: Instant, action: Action) {
        self.handle(now, super::Event::Menu(action, By::Elsewhere));
    }

    fn toggle_menu(&mut self) {
        if self.menu.open {
            self.close_panel();
        } else {
            self.open_panel();
        }
    }

    fn align(&mut self, now: Instant) {
        self.now = now;
        self.rev += 1;
        self.tidy_up();
    }

    fn move_menu(&mut self, position: Vec3, head: Vec3) {
        self.world.move_menu(position, head);
    }

    fn hold(&mut self, thing: Thing) -> Option<Placement> {
        self.rev += 1;
        self.world.hold(thing, &self.settings)
    }

    fn drag(&mut self, thing: Thing, place: Placement, head: Vec3) {
        // The menu's map shows where monitors are.
        if thing.display().is_some() {
            self.rev += 1;
        }
        self.world.drag(&self.settings, thing, place, head);
    }

    fn let_go(&mut self, now: Instant, thing: Thing) -> bool {
        self.now = now;
        self.rev += 1;
        let stuck = self.world.let_go(thing, &self.settings, now);
        self.world_done();
        stuck
    }

    fn resize_start(&mut self, id: DisplayKey) -> Option<f32> {
        self.world.resize_start(id)
    }

    fn resize(&mut self, width: f32) {
        self.rev += 1;
        self.world.resize(&self.settings, width);
    }

    fn resize_end(&mut self) {
        self.world.resize_end();
    }

    fn cancel_holds(&mut self) {
        self.rev += 1;
        self.world.cancel_holds();
    }

    fn haptic(&mut self, hand: usize) {
        self.effects.push(Effect::Haptic(hand));
    }
}
