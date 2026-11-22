//! The settings and the menu itself: its pages, language, tips, the clock, reminders,
//! the hands.

use std::time::Duration;

use super::super::{Scene, tab_code};
use crate::input::Controls;
use crate::persist;
use crate::settings::HandMode;
use crate::ui::Action;

pub(super) fn act(scene: &mut Scene, action: Action, controls: &mut Controls) {
    match action {
        Action::HideNote => {}
        Action::Tab(tab) => {
            scene.tab = tab;
            scene.settings.last_tab = tab_code(tab);
            scene.saves.mark(persist::File::Settings);
        }
        Action::IntroDone => {
            scene.settings.intro_seen = true;
            scene.saves.mark(persist::File::Settings);
        }
        Action::LanguagePage(page) => scene.language_page = page,
        Action::Language(language) => {
            scene.settings.language = language;
            scene.saves.mark(persist::File::Settings);
            // Another language is said once it is loaded (see `language_shown`).
            scene.language_asked = scene.press_in_menu;
            let s = &scene.settings;
            let shows = crate::lang::resolve(
                language,
                s.pc_language,
                scene.system_lang,
                scene.lang.known(),
            );
            if shows == scene.texts().lang() {
                scene.language_shown();
            }
        }
        Action::RestEvery(minutes) => {
            scene.settings.rest_every = minutes;
            scene.saves.mark(persist::File::Settings);
            scene.worn_for = Duration::ZERO;
        }
        Action::Clock12h(on) => {
            scene.settings.clock_12h = on;
            scene.saves.mark(persist::File::Settings);
        }
        Action::DateStyle(style) => {
            scene.settings.date_style = style;
            scene.saves.mark(persist::File::Settings);
        }
        Action::SoundFromScreen(on) => {
            scene.settings.sound_from_screen = on;
            scene.saves.mark(persist::File::Settings);
            scene.prefs_changed();
        }
        Action::Notifications(on) => {
            scene.settings.notifications = on;
            scene.saves.mark(persist::File::Settings);
            scene.prefs_changed();
        }
        Action::BatterySaver(on) => {
            scene.settings.battery_saver = on;
            scene.saves.mark(persist::File::Settings);
            scene.refresh_due = true;
        }
        Action::LeftHanded(on) => {
            scene.settings.left_handed = on;
            scene.saves.mark(persist::File::Settings);
        }
        Action::OpenMenu => {
            let (head, forward) = scene.head;
            let touch = scene.hands_in_view;
            scene.open_panel(head, forward, touch);
        }
        Action::Hands(mode) => {
            scene.settings.hands = mode;
            scene.saves.mark(persist::File::Settings);
            controls.hands_enabled = mode == HandMode::Control;
        }
        Action::ShowTips => {
            scene.settings.intro_seen = false;
            scene.saves.mark(persist::File::Settings);
        }
        Action::Close => {
            scene.panel_open = false;
            // Closing the tips counts as read, or they would open again right away.
            if !scene.settings.intro_seen {
                scene.settings.intro_seen = true;
                scene.saves.mark(persist::File::Settings);
            }
        }
        // The others are not this page's (see `handle_action`).
        _ => {}
    }
}
