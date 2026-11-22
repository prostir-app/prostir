//! The menu's actions (and the same done by a command of the PC, the palm menu or the
//! selftest), by the part of the menu they belong to.

mod look;
mod monitors;
mod pc;
mod profiles;
mod settings;

use super::Scene;
use crate::ids::PcId;
use crate::input::Controls;
use crate::passthrough;
use crate::ui::Action;

/// A PC the user asked to forget, and the press that asked it.
pub(crate) struct Forgetting {
    pub(crate) pc: PcId,
    /// As the list of trusted PCs names it.
    pub(crate) name: String,
    pub(crate) on: Action,
    pub(crate) in_menu: bool,
}

/// Where a press comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum By {
    /// A button of the main menu: what it comes to is said under it.
    Menu,
    /// The palm menu, the "Attach" pill, a command of the PC, the selftest.
    Elsewhere,
}

/// Does `action`, pressed `by`.
pub(crate) fn handle_action(
    scene: &mut Scene,
    action: Action,
    by: By,
    passthrough: &mut Option<passthrough::Passthrough>,
    controls: &mut Controls,
) {
    // Changing the setup by hand leaves the profile it came from.
    if action.leaves_profile() {
        scene.active_profile = None;
    }
    // A press of the menu: what was said under the button pressed before goes
    // (another page too), and what this one asks of the PC is its own. A press
    // elsewhere leaves the menu's note as it is, and says what it comes to with a
    // toast where it is worth one.
    scene.press_in_menu = by == By::Menu;
    if scene.press_in_menu {
        scene.notes.press(action);
    } else {
        scene.notes.press_elsewhere();
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
        | Action::DetachWindow
        | Action::WindowHere
        | Action::Screenshot
        | Action::FindMouse => monitors::act(scene, action),
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
        | Action::Magnet(_) => look::act(scene, action, passthrough),
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
        | Action::SetMirror(_)
        | Action::SetMic(_)
        | Action::AskMic
        | Action::GazeMouse(_) => pc::act(scene, action),
        Action::ApplyProfile(_) | Action::SaveProfile(_) => profiles::act(scene, action),
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
        | Action::HideNote => settings::act(scene, action, controls),
    }
    scene.world_done();
}
