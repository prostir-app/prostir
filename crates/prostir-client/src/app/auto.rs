//! "Auto" for the scale of the PC's monitors (see `crate::autoscale`): the main
//! headset looks at the monitors in Auto now and then, and asks the PC for the scale
//! each one's place in the room wants. Nothing is allocated once the list is long
//! enough.

use log::{info, warn};
use prostir_proto::request::{Outcome, Request};
use prostir_proto::state::{DisplayId, DisplayKind, caps};

use super::Model;
use crate::autoscale::{self, Monitor};
use crate::ids::{DisplayKey, PcId};
use crate::ui;

impl Model {
    /// The main PC changes a monitor's scale in place (its capability 17): the menu
    /// offers Auto, and the headset chooses for monitors in it.
    pub fn can_auto(&self) -> bool {
        self.pc()
            .caps
            .as_ref()
            .is_some_and(|c| c.has(caps::pc::SCALE_IN_PLACE))
    }

    /// The scale a monitor of `size` pixels added now in Auto is made at, so it needs
    /// no second change once shown: the smallest of the PC's scales its place in the
    /// room wants; 100 % where the headset's pixels per degree are not known.
    pub(super) fn new_auto_scale(&self, size: (u32, u32)) -> u16 {
        let Some(limits) = self.pc().limits.as_ref() else {
            return 100;
        };
        self.world
            .shown_if_added(size, self.head.1, &self.settings)
            .map_or(100, |p| self.auto.new_scale(p, &limits.scales))
    }

    /// The line Auto's scale reaches, in hundredths, as "For development" shows it.
    #[cfg(feature = "preview")]
    pub fn auto_line(&self) -> u8 {
        self.auto.line_percent()
    }

    /// Whether monitor `k` is in Auto, as its PC says.
    pub fn is_auto(&self, k: DisplayKey) -> bool {
        self.session
            .pc(k.pc)
            .and_then(|p| p.state.scale_auto.as_ref())
            .is_some_and(|a| a.has(k.id))
    }

    /// Looks at the main PC's monitors in Auto (every `autoscale::LOOK_EVERY` at most)
    /// and asks for the scale one of them wants, if one is due: only this headset is
    /// the main one there, the PC changes scales in place and the headset knows its
    /// pixels per degree (else no monitor has a `p`, and nothing is asked).
    pub(super) fn auto_scale(&mut self) {
        let now = self.now;
        if !self.auto.look_due(now) {
            return;
        }
        let Some(pc) = self.session.main().filter(|pc| self.session.connected(*pc)) else {
            return;
        };
        let Some(state) = self.session.pc(pc).map(|p| &p.state) else {
            return;
        };
        let main = state.access.is_some_and(|a| a.is_main());
        let (Some(auto), Some(limits), Some(displays)) = (
            state.scale_auto.as_ref(),
            state.limits.as_ref(),
            state.displays.as_ref(),
        ) else {
            return;
        };
        if !main || !self.can_auto() || auto.auto.is_empty() {
            return;
        }
        let list = &mut self.auto_list;
        list.clear();
        for &(key, w, h) in self.world.monitors() {
            if key.pc != pc || !auto.has(key.id) || list.len() >= autoscale::MAX {
                continue;
            }
            let Some(d) = displays
                .get(key.id)
                .filter(|d| d.kind == DisplayKind::Virtual)
            else {
                continue;
            };
            let Some(p) = self.world.shown_scale(key) else {
                continue;
            };
            list.push(Monitor {
                key,
                p,
                now: d.mode.scale,
                size: (w, h),
                group: 0,
            });
        }
        let world = &self.world;
        autoscale::group(list, |a, b| world.glued(a, b));
        let moving = world.moving();
        let due = self.auto.due(list, &limits.scales, moving, now);
        // Gone monitors are forgotten (no allocation).
        let listed = |k: DisplayKey| k.pc != pc || auto.has(k.id);
        self.auto.retain(listed);
        if let Some((key, scale)) = due {
            self.ask_auto_scale(key, scale);
        }
    }

    /// Asks the PC for `scale` for monitor `key` in Auto: no note in the menu, which
    /// the person did not press.
    fn ask_auto_scale(&mut self, key: DisplayKey, scale: u16) {
        let Some(req) = self
            .session
            .ask(Some(key.pc), ui::Asked::AutoScale(key), self.now)
        else {
            return;
        };
        info!(
            "request {}: {scale} % for monitor {} (Auto)",
            req.0, key.id.0
        );
        self.send_to(
            key.pc,
            prostir_proto::ClientMsg::request(req, Request::AutoScale { id: key.id, scale }),
        );
    }

    /// The PC answered a scale asked in Auto for monitor `key`: one it did not give
    /// is not asked again while the monitor keeps its size.
    pub(super) fn auto_scale_answered(&mut self, key: DisplayKey, outcome: &Outcome) {
        if let Outcome::Failed { error, detail } = outcome {
            warn!(
                "the PC did not take the scale for monitor {} (code {}): {detail}",
                key.id.0,
                error.code()
            );
            let size = self
                .world
                .monitors()
                .iter()
                .find(|m| m.0 == key)
                .map_or((0, 0), |m| (m.1, m.2));
            let asked = self.auto.asked(key);
            if let Some(scale) = asked {
                self.auto.refused(key, scale, size);
            }
        }
    }

    /// Monitor `key` in Auto, or not (the person chose it in the menu).
    pub(super) fn set_scale_auto(&mut self, key: DisplayKey, on: bool) {
        let sent = self.request_for(
            key,
            |id| Request::SetScaleAuto { id, on },
            ui::Asked::Scale(key),
        );
        if sent.is_some() {
            self.leave_profile();
        }
    }

    /// Profile `slot` was switched to, its monitors made as `ids` (in its order) on PC
    /// `pc`: each in Auto as the profile has it, and one not in Auto at its scale (a
    /// monitor of its size kept from before may have another, chosen in Auto). Where
    /// the PC changes scales in place only; the PC's lists came before the answer.
    pub(super) fn profile_scales(&mut self, pc: PcId, slot: usize, ids: &[DisplayId]) {
        let in_place = self
            .session
            .pc(pc)
            .and_then(|p| p.state.caps.as_ref())
            .is_some_and(|c| c.has(caps::pc::SCALE_IN_PLACE));
        let Some(profile) = self.profiles.get(slot).filter(|_| in_place) else {
            return;
        };
        let mut asks = Vec::new();
        for (i, (&id, mode)) in ids.iter().zip(&profile.monitors).enumerate() {
            let key = DisplayKey::new(pc, id);
            let auto = profile.auto.contains(&i);
            let now = self.display(key).map(|d| d.mode.scale);
            match (auto, self.is_auto(key)) {
                (true, false) => asks.push(Request::SetScaleAuto { id, on: true }),
                // A scale chosen by hand also takes it out of Auto.
                (false, _) if now.is_some_and(|s| s != mode.scale) => {
                    asks.push(Request::SetScale {
                        id,
                        scale: mode.scale,
                    });
                }
                (false, true) => asks.push(Request::SetScaleAuto { id, on: false }),
                _ => {}
            }
        }
        for asked in asks {
            let id = match &asked {
                Request::SetScale { id, .. } | Request::SetScaleAuto { id, .. } => *id,
                _ => continue,
            };
            let key = DisplayKey::new(pc, id);
            if let Some(req) = self.session.ask(Some(pc), ui::Asked::Scale(key), self.now) {
                info!("request {}: {asked:?} (profile)", req.0);
                self.send_to(pc, prostir_proto::ClientMsg::request(req, asked));
            }
        }
    }

    /// A monitor added at a scale chosen by hand (not Auto) was made, `id` on PC `pc`:
    /// it leaves Auto, which the PC gives every new monitor.
    pub(super) fn added_fixed(&mut self, pc: PcId, id: DisplayId) {
        if !self
            .session
            .pc(pc)
            .and_then(|p| p.state.caps.as_ref())
            .is_some_and(|c| c.has(caps::pc::SCALE_IN_PLACE))
        {
            return;
        }
        let key = DisplayKey::new(pc, id);
        if let Some(req) = self.session.ask(Some(pc), ui::Asked::Scale(key), self.now) {
            self.send_to(
                pc,
                prostir_proto::ClientMsg::request(req, Request::SetScaleAuto { id, on: false }),
            );
        }
    }
}
