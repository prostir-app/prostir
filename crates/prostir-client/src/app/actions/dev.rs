//! "For development" (the preview build only, see `crate::PREVIEW`): how large the
//! monitors are made, the compositor's filters for their layers, and the numbers next
//! to the monitor looked at. Kept in memory only, a test of the moment.

use super::super::Model;
use crate::ui::Action;

impl Model {
    pub(super) fn act_dev(&mut self, action: Action) {
        let mut dev = *self.world.dev();
        match action {
            Action::DevSize(size) => dev.size = size,
            Action::DevSuperSampling(filter) => dev.super_sampling = filter,
            Action::DevSharpening(filter) => dev.sharpening = filter,
            Action::DevMetrics(on) => dev.metrics = on,
            Action::DevSizeReset => {
                self.world.reset_size(self.now);
                return;
            }
            // The others are not this part's (see `Model::act`).
            _ => return,
        }
        self.world.set_dev(&self.settings, dev, self.now);
    }
}
