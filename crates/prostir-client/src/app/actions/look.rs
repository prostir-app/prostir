//! How it looks: cinema, the layout, the curve, the background, the brightness, the
//! menu, how monitors move.

use std::time::Instant;

use super::super::Scene;
use crate::settings::Background;
use crate::ui::Action;
use crate::{passthrough, persist};

#[expect(
    clippy::disallowed_methods,
    reason = "until step 7.7 passes the time in and moves the threads to the shell"
)]
pub(super) fn act(
    scene: &mut Scene,
    action: Action,
    passthrough: &mut Option<passthrough::Passthrough>,
) {
    match action {
        Action::CinemaScreen(screen) => {
            scene
                .world
                .pick_cinema(screen, &scene.settings, Instant::now());
        }
        Action::Cinema(on) => {
            scene.set_cinema(on);
            scene.saves.mark(persist::File::Settings);
        }
        Action::KeyboardWindow(on) => {
            scene.settings.keyboard_window = on;
            scene.saves.mark(persist::File::Settings);
        }
        Action::Focus(on) => {
            scene.settings.focus = on;
            scene.saves.mark(persist::File::Settings);
            scene.prefs_changed();
        }
        Action::LargeMenu(on) => {
            scene.settings.large_menu = on;
            scene.saves.mark(persist::File::Settings);
            scene.world.menu_size(on);
        }
        Action::Brightness(percent) => {
            scene.settings.brightness = percent;
            scene.saves.mark(persist::File::Settings);
        }
        Action::MenuButton(on) => {
            scene.settings.menu_button = on;
            scene.saves.mark(persist::File::Settings);
        }
        Action::MoveAll(on) => {
            scene.settings.move_all = on;
            scene.saves.mark(persist::File::Settings);
        }
        Action::Magnet(on) => {
            scene.settings.magnet = on;
            scene.saves.mark(persist::File::Settings);
        }
        Action::Follow(on) => {
            scene.settings.follow = on;
            scene.saves.mark(persist::File::Settings);
            scene.world.follow_chosen(&scene.settings);
        }
        Action::Preset(preset) => {
            scene.settings.preset = preset;
            scene.saves.mark(persist::File::Settings);
            scene.world.preset_chosen(&scene.settings, Instant::now());
            scene.done(action);
        }
        Action::Curve(curve) => {
            scene.settings.curve = curve;
            scene.saves.mark(persist::File::Settings);
            // Flat it hangs tilted, curved it continues the cylinder.
            scene.world.curve_chosen(&scene.settings);
        }
        Action::Background(background) => {
            scene.settings.background = background;
            scene.saves.mark(persist::File::Settings);
            set_passthrough(scene, passthrough);
        }
        // The others are not this page's (see `handle_action`).
        _ => {}
    }
}

/// Passthrough cameras on only for the "room" background.
pub(crate) fn set_passthrough(
    scene: &mut Scene,
    passthrough: &mut Option<passthrough::Passthrough>,
) {
    let background = scene.settings.background;
    let opacity = if background == Background::RoomDim {
        0.4
    } else {
        1.0
    };
    if let Some(pt) = passthrough
        && let Err(e) = pt
            .set_room(background.is_room())
            .and_then(|()| pt.set_room_opacity(opacity))
    {
        let text = scene
            .texts()
            .f("error-cameras", &[("error", format!("{e:#}").into())]);
        scene.show_error(text);
    }
}
