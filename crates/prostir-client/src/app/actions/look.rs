//! How it looks: cinema, the layout, the curve, the background, the brightness, the
//! menu, how monitors move.

use super::super::{Effect, Model, XrCmd};
use crate::persist;
use crate::settings::Background;
use crate::ui::Action;

impl Model {
    /// An action of this part of the menu (see `Model::act`).
    pub(super) fn act_look(&mut self, action: Action) {
        match action {
            Action::CinemaScreen(screen) => {
                self.world.pick_cinema(screen, &self.settings, self.now);
            }
            Action::Cinema(on) => {
                self.set_cinema(on);
                self.saves.mark(persist::File::Settings);
            }
            Action::KeyboardWindow(on) => {
                self.settings.keyboard_window = on;
                self.saves.mark(persist::File::Settings);
            }
            Action::Focus(on) => {
                self.settings.focus = on;
                self.saves.mark(persist::File::Settings);
                self.prefs_changed();
            }
            Action::LargeMenu(on) => {
                self.settings.large_menu = on;
                self.saves.mark(persist::File::Settings);
                self.world.menu_size(on);
            }
            Action::Brightness(percent) => {
                self.settings.brightness = percent;
                self.saves.mark(persist::File::Settings);
            }
            Action::MenuButton(on) => {
                self.settings.menu_button = on;
                self.saves.mark(persist::File::Settings);
            }
            Action::MoveAll(on) => {
                self.settings.move_all = on;
                self.saves.mark(persist::File::Settings);
            }
            Action::Magnet(on) => {
                self.settings.magnet = on;
                self.saves.mark(persist::File::Settings);
            }
            Action::Follow(on) => {
                self.settings.follow = on;
                self.saves.mark(persist::File::Settings);
                self.world.follow_chosen(&self.settings);
            }
            Action::Preset(preset) => {
                self.settings.preset = preset;
                self.saves.mark(persist::File::Settings);
                self.world.preset_chosen(&self.settings, self.now);
                self.done(action);
            }
            Action::Curve(curve) => {
                self.settings.curve = curve;
                self.saves.mark(persist::File::Settings);
                // Flat it hangs tilted, curved it continues the cylinder.
                self.world.curve_chosen(&self.settings);
            }
            Action::Background(background) => {
                self.settings.background = background;
                self.saves.mark(persist::File::Settings);
                self.set_passthrough();
            }
            // The others are not this part's (see `Model::act`).
            _ => {}
        }
    }
}

impl Model {
    /// The cameras on only for the "room" backgrounds.
    pub(in super::super) fn set_passthrough(&mut self) {
        let background = self.settings.background;
        let opacity = if background == Background::RoomDim {
            0.4
        } else {
            1.0
        };
        self.effects.push(Effect::Xr(XrCmd::Room {
            on: background.is_room(),
            opacity,
        }));
    }
}
