//! The model as the hands see it (see `interaction`): what they point at and hold, and
//! what they do, each done at once (a press is the menu's action, as from any button).

use std::time::Instant;

use glam::{Vec2, Vec3};
use prostir_proto::state::caps;

use super::{By, Effect, Model, SystemCmd};
use crate::ids::{DisplayKey, PcId, Target, Thing};
use crate::interaction::record::{Piece, Stage};
use crate::interaction::{Feel, Modes, PcInput, Scene, Touched};
use crate::layout::{self, Placement, Ray};
use crate::ui::{self, Action};
use crate::world::Pickable;

impl Model {
    /// PC `pc` lets the headset move its mouse.
    pub(super) fn pc_moves_mouse(&self, pc: PcId) -> bool {
        self.session.pc(pc).is_some_and(|p| {
            p.state
                .caps
                .as_ref()
                .is_some_and(|c| c.has(caps::pc::MOVE_MOUSE))
        })
    }

    /// The cylinder monitor `id` is drawn on (none: flat).
    fn curve_of(&self, id: DisplayKey) -> Option<f32> {
        self.world
            .curve_radius(&self.settings)
            .filter(|_| !self.world.is_window(id))
    }

    /// What stands in the room now, as a recording of the input keeps it.
    pub fn input_stage(&self) -> Stage {
        let w = &self.world;
        let pieces = w
            .draw_list()
            .iter()
            .filter_map(|&(_, id)| {
                Some(Piece {
                    thing: Thing::Display(id),
                    place: *w.shown(id)?,
                    height: w.height_of(id),
                    window: w.is_window(id),
                })
            })
            .collect();
        let mut stage = Stage::new(w.menu(), self.menu.open, pieces);
        stage.large_menu = self.settings.large_menu;
        stage.left_handed = self.settings.left_handed;
        stage.modes = Scene::modes(self);
        stage
    }
}

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

    fn feel(&self) -> &Feel {
        &self.feel
    }

    fn modes(&self) -> Modes {
        let t = self.trial();
        Modes {
            monitors: t.monitors,
            take: t.take,
            hands: self.settings.hands == crate::settings::HandMode::Control,
            movement: t.movement,
        }
    }

    fn touchable(&self, at: Vec3, only: Option<DisplayKey>) -> Option<Touched> {
        let w = &self.world;
        let mut best: Option<Touched> = None;
        for &(_, id) in w.draw_list() {
            if w.is_window(id) || only.is_some_and(|o| o != id) {
                continue;
            }
            let Some(p) = w.shown(id) else { continue };
            let (height, curve) = (w.height_of(id), self.curve_of(id));
            let Some(a) = layout::against(p, height, curve, at) else {
                continue;
            };
            if only.is_none() && a.z < -self.feel.touch_through_m {
                continue;
            }
            if best.is_none_or(|b| a.z.abs() < b.z.abs()) {
                best = Some(Touched {
                    id,
                    local: a.local,
                    z: a.z,
                    point: a.point,
                    normal: a.normal,
                    uv: layout::image_uv(p, height, curve, a.local),
                });
            }
        }
        best
    }

    fn monitor_uv(&self, id: DisplayKey, local: Vec2) -> Option<Vec2> {
        let p = self.world.shown(id)?;
        Some(layout::image_uv(
            p,
            self.world.height_of(id),
            self.curve_of(id),
            local,
        ))
    }

    fn mouse_shown(&self, id: DisplayKey) -> bool {
        self.pc_moves_mouse(id.pc) && self.cursors.on(id, self.now).is_some()
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
            self.close_menu();
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
        // Taken: the map waits for the hand to let go; where each monitor stands now is
        // what the drag is compared with.
        self.rev += 1;
        let at = self.world.hold(thing, &self.settings);
        // Locked: a monitor is not taken, and the user is told why.
        if at.is_none() && self.settings.locked && thing.display().is_some() {
            let text = self.texts().t("headset-note-locked").to_owned();
            self.show_toast(text);
        }
        self.map_moved();
        at
    }

    fn drag(&mut self, thing: Thing, place: Placement, head: Vec3) {
        self.world.drag(&self.settings, thing, place, head);
        // The menu shows on which side each monitor stands (its map waits for the hand
        // to let go): changed only when that does, so a drag makes nothing anew.
        if thing.display().is_some() && self.map_moved() {
            self.rev += 1;
        }
    }

    fn let_go(&mut self, now: Instant, thing: Thing) -> bool {
        self.now = now;
        self.rev += 1;
        let stuck = self.world.let_go(thing, &self.settings, now);
        self.world_done();
        stuck
    }

    fn resize_start(&mut self, id: DisplayKey) -> Option<f32> {
        self.rev += 1;
        let width = self.world.resize_start(id);
        self.map_moved();
        width
    }

    fn resize(&mut self, width: f32) {
        self.world.resize(&self.settings, width, self.head.0);
        if self.map_moved() {
            self.rev += 1;
        }
    }

    fn resize_end(&mut self) {
        // The map shows the desktop anew.
        self.rev += 1;
        self.world.resize_end();
        self.world_done();
    }

    fn cancel_holds(&mut self) {
        self.rev += 1;
        self.world.cancel_holds();
    }

    fn haptic(&mut self, hand: usize) {
        self.effects.push(Effect::Haptic(hand));
    }

    /// The mouse goes where the hand sends it, where the PC lets the headset move it
    /// (one pointer, drawn at once where the ray is); its buttons, scrolls and touches
    /// to a PC that takes them (see `pc_input`).
    fn pc(&mut self, now: Instant, input: PcInput) {
        self.act_on_pc(now, input);
    }
}
