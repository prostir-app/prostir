//! The settings and the menu itself: its pages, language, tips, the clock, reminders,
//! the hands.

use std::time::Duration;

use super::super::{Effect, Model, XrCmd, tab_code};
use crate::persist;
use crate::ui::Action;

impl Model {
    /// An action of this part of the menu (see `Model::act`).
    pub(super) fn act_settings(&mut self, action: Action) {
        match action {
            Action::HideNote => {}
            Action::Tab(tab) => {
                self.menu.tab = tab;
                self.settings.last_tab = tab_code(tab);
                self.saves.mark(persist::File::Settings);
            }
            Action::IntroDone => {
                self.settings.intro_seen = true;
                self.saves.mark(persist::File::Settings);
            }
            Action::LanguagePage(page) => self.menu.language_page = page,
            Action::Language(language) => {
                self.settings.language = language;
                self.saves.mark(persist::File::Settings);
                // Another language is said once it is loaded (see `language_shown`).
                self.language_asked = self.press_in_menu;
                let s = &self.settings;
                let shows = crate::lang::resolve(
                    language,
                    s.pc_language,
                    self.system_lang,
                    self.lang.known(),
                );
                if shows == self.texts().lang() {
                    self.language_shown();
                }
            }
            Action::RestEvery(minutes) => {
                self.settings.rest_every = minutes;
                self.saves.mark(persist::File::Settings);
                self.worn_for = Duration::ZERO;
            }
            Action::Clock12h(on) => {
                self.settings.clock_12h = on;
                self.saves.mark(persist::File::Settings);
            }
            Action::DateStyle(style) => {
                self.settings.date_style = style;
                self.saves.mark(persist::File::Settings);
            }
            Action::SoundFromScreen(on) => {
                self.settings.sound_from_screen = on;
                self.saves.mark(persist::File::Settings);
                self.prefs_changed();
            }
            Action::Notifications(on) => {
                self.settings.notifications = on;
                self.saves.mark(persist::File::Settings);
                self.prefs_changed();
            }
            Action::BatterySaver(on) => {
                self.settings.battery_saver = on;
                self.saves.mark(persist::File::Settings);
                self.effects
                    .push(Effect::Xr(XrCmd::RefreshRate { saver: on }));
            }
            Action::LeftHanded(on) => {
                self.settings.left_handed = on;
                self.saves.mark(persist::File::Settings);
            }
            Action::OpenMenu => self.open_panel(),
            // The shell lets tracked hands point and click as the settings say.
            Action::Hands(mode) => {
                self.settings.hands = mode;
                self.saves.mark(persist::File::Settings);
            }
            Action::ShowTips => {
                self.settings.intro_seen = false;
                self.saves.mark(persist::File::Settings);
            }
            Action::Close => {
                self.close_panel();
                // Closing the tips counts as read, or they would open again right away.
                if !self.settings.intro_seen {
                    self.settings.intro_seen = true;
                    self.saves.mark(persist::File::Settings);
                }
            }
            // The others are not this part's (see `Model::act`).
            _ => {}
        }
    }
}
