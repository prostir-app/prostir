//! The menu's actions (and the same done by a command of the PC, the palm menu or the
//! selftest), by the part of the menu they belong to.

#[cfg(feature = "preview")]
mod dev;
mod look;
mod monitors;
mod pc;
mod profiles;
mod settings;

use super::Model;
use crate::ids::PcId;
use crate::ui::Action;

/// A PC the user asked to forget, and the press that asked it.
#[derive(Debug)]
pub(super) struct Forgetting {
    pub(super) pc: PcId,
    /// As the list of trusted PCs names it.
    pub(super) name: String,
    pub(super) on: Action,
    pub(super) in_menu: bool,
}

/// Where a press comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum By {
    /// A button of the main menu: what it comes to is said under it.
    Menu,
    /// The palm menu, the "Attach" pill, a command of the PC, the selftest.
    Elsewhere,
}

impl Model {
    /// Does `action`, pressed `by`.
    pub(super) fn act(&mut self, action: Action, by: By) {
        // A press of the menu: what was said under the button pressed before goes
        // (another page too), and what this one asks of the PC is its own. A press
        // elsewhere leaves the menu's note as it is, and says what it comes to with a
        // toast where it is worth one.
        self.press_in_menu = by == By::Menu;
        if self.press_in_menu {
            self.menu.notes.press(action);
        } else {
            self.menu.notes.press_elsewhere();
        }
        match action {
            Action::Add { .. }
            | Action::Remove(_)
            | Action::CloseWindow(_)
            | Action::MakePrimary(_)
            | Action::Move { .. }
            | Action::SelectMonitor(_)
            | Action::Tidy
            | Action::Recenter
            | Action::ResetArrangement
            | Action::DetachWindow
            | Action::WindowHere
            | Action::Screenshot
            | Action::AddScale(_)
            | Action::AddAuto
            | Action::SetScale(..)
            | Action::ScaleAuto(_)
            | Action::FindMouse => self.act_monitors(action),
            Action::Cinema(_)
            | Action::CinemaScreen(_)
            | Action::Preset(_)
            | Action::Curve(_)
            | Action::Background(_)
            | Action::Brightness(_)
            | Action::LargeMenu(_)
            | Action::KeyboardWindow(_)
            | Action::Focus(_)
            | Action::MenuButton(_)
            | Action::Follow(_)
            | Action::MoveAll(_)
            | Action::Magnet(_)
            | Action::Lock(_) => self.act_look(action),
            Action::ConnectTo(_)
            | Action::ShowKeypad
            | Action::Key(_)
            | Action::Backspace
            | Action::ConnectTyped
            | Action::CancelKeypad
            | Action::ChangeHost
            | Action::PairAnswer(_)
            | Action::PairOtherWay
            | Action::ForgetPc(_)
            | Action::SetEncoding(_)
            | Action::SetAudio(_)
            | Action::ShowPhysical(..)
            | Action::PhysicalPage(_)
            | Action::NoLimits(_)
            | Action::SetMic(_)
            | Action::AskMic
            | Action::GazeMouse(_) => self.act_pc(action),
            Action::ApplyProfile(_)
            | Action::SaveProfile(_)
            | Action::NewProfile
            | Action::DeleteProfile(_)
            | Action::ProfilePage(_) => self.act_profiles(action),
            Action::Tab(_)
            | Action::IntroDone
            | Action::ShowTips
            | Action::LanguagePage(_)
            | Action::Language(_)
            | Action::RestEvery(_)
            | Action::Clock12h(_)
            | Action::DateStyle(_)
            | Action::SoundFromScreen(_)
            | Action::Notifications(_)
            | Action::BatterySaver(_)
            | Action::LeftHanded(_)
            | Action::Hands(_)
            | Action::OpenMenu
            | Action::Close
            | Action::HideNote => self.act_settings(action),
            #[cfg(feature = "preview")]
            Action::DevSize(_)
            | Action::DevSizeReset
            | Action::DevSuperSampling(_)
            | Action::DevSharpening(_)
            | Action::DevMetrics(_)
            | Action::DevAutoLine(_) => self.act_dev(action),
            #[cfg(feature = "preview")]
            Action::DevMonitors(_)
            | Action::DevTake(_)
            | Action::DevMovement(_)
            | Action::DevHandLook(_)
            | Action::DevFx(_)
            | Action::DevOwnModels(_)
            | Action::DevRecord
            | Action::DevReadFeel => self.act_dev_input(action),
        }
        self.world_done();
    }
}
