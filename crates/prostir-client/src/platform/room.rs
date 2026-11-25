//! The room behind the monitors, whichever way this headset shows it (chosen by
//! `caps::room_ways` from what the runtime offers): glasses that always let the room
//! through, a frame blended with the cameras' picture by its alpha
//! (`XR_ENVIRONMENT_BLEND_MODE_ALPHA_BLEND`), or a layer of the cameras' picture of one
//! maker's extension (`XR_FB_passthrough`, `XR_HTC_passthrough`). Only one of them at a
//! time, never two together.

use anyhow::Result;
use log::{info, warn};
use openxr as xr;

use super::htc;
use super::meta::passthrough::Passthrough;
use super::openxr::Gfx;
use crate::caps::RoomWay;

/// The room as this headset shows it (see the module), and the blend mode of a frame
/// while it is not blended in.
pub struct Room {
    way: Way,
    plain: xr::EnvironmentBlendMode,
}

/// The way it is shown.
enum Way {
    /// No way to show it.
    None,
    /// The display lets the room through, always (`ADDITIVE`): nothing to switch.
    Glasses,
    /// The frame is blended with the cameras' picture by its alpha while the room is
    /// shown (`ALPHA_BLEND`): where nothing is drawn, the room shows; dimmed by a black
    /// layer that lets part of it through.
    Blend {
        shown: bool,
        opacity: f32,
    },
    Fb(Passthrough),
    Htc(htc::Passthrough),
}

impl Room {
    /// The room by the first of `ways` that can be made in `session`, shown at once
    /// with `shown` (`opacity` of it); a frame without it goes in `plain`.
    pub fn make(
        ways: &[RoomWay],
        plain: xr::EnvironmentBlendMode,
        session: &xr::Session<Gfx>,
        shown: bool,
        opacity: f32,
    ) -> Self {
        let mut room = Room {
            way: Way::None,
            plain,
        };
        for &way in ways {
            let made = match way {
                RoomWay::Glasses => Ok(Way::Glasses),
                RoomWay::Blend => Ok(Way::Blend { shown, opacity }),
                RoomWay::FbPassthrough => Passthrough::new(session, shown).map(Way::Fb),
                RoomWay::HtcPassthrough => htc::Passthrough::new(session).map(Way::Htc),
            };
            match made {
                Ok(made) => {
                    info!("the room: {way:?}");
                    room.way = made;
                    if let Err(e) = room.set(shown, opacity) {
                        warn!("the room by {way:?}: {e:#}");
                    }
                    return room;
                }
                Err(e) => warn!("the room by {way:?} unavailable: {e:#}"),
            }
        }
        room
    }

    /// The cameras broke for good: no room from now on.
    pub fn lost(&mut self) {
        self.way = Way::None;
    }

    /// The way it is shown (`None`: it is not).
    pub fn way(&self) -> Option<RoomWay> {
        match &self.way {
            Way::None => None,
            Way::Glasses => Some(RoomWay::Glasses),
            Way::Blend { .. } => Some(RoomWay::Blend),
            Way::Fb(_) => Some(RoomWay::FbPassthrough),
            Way::Htc(_) => Some(RoomWay::HtcPassthrough),
        }
    }

    /// Shows the room behind the monitors (`on`), `opacity` of it (1: all; less
    /// darkens it towards black), or not.
    pub fn set(&mut self, on: bool, opacity: f32) -> Result<()> {
        match &mut self.way {
            Way::None | Way::Glasses => Ok(()),
            Way::Blend { shown, opacity: o } => {
                *shown = on;
                *o = opacity;
                Ok(())
            }
            Way::Fb(p) => p.set_room(on).and_then(|()| p.set_room_opacity(opacity)),
            Way::Htc(p) => p.set_room(on, opacity),
        }
    }

    /// The way the frame is blended with the world.
    pub fn blend_mode(&self) -> xr::EnvironmentBlendMode {
        match &self.way {
            Way::Glasses => xr::EnvironmentBlendMode::ADDITIVE,
            Way::Blend { shown: true, .. } => xr::EnvironmentBlendMode::ALPHA_BLEND,
            _ => self.plain,
        }
    }

    /// The cameras' room shows behind the monitors (no panorama then; on glasses the
    /// room shows through whatever is drawn).
    pub fn shown(&self) -> bool {
        match &self.way {
            Way::None | Way::Glasses => false,
            Way::Blend { shown, .. } => *shown,
            Way::Fb(p) => p.room_layer().is_some(),
            Way::Htc(p) => p.shown(),
        }
    }

    /// Blended with the cameras' picture and dimmed: how much a black layer over the
    /// room covers of it (0 to 255).
    pub fn dim(&self) -> Option<u8> {
        match &self.way {
            Way::Blend {
                shown: true,
                opacity,
            } if *opacity < 1.0 => Some(((1.0 - opacity.clamp(0.0, 1.0)) * 255.0).round() as u8),
            _ => None,
        }
    }

    /// The cameras of `XR_FB_passthrough` (the window onto the keyboard is theirs).
    pub fn fb(&mut self) -> Option<&mut Passthrough> {
        match &mut self.way {
            Way::Fb(p) => Some(p),
            _ => None,
        }
    }

    /// The same, to look at.
    pub fn fb_ref(&self) -> Option<&Passthrough> {
        match &self.way {
            Way::Fb(p) => Some(p),
            _ => None,
        }
    }

    /// The cameras of `XR_HTC_passthrough`.
    pub fn htc_passthrough(&self) -> Option<&htc::Passthrough> {
        match &self.way {
            Way::Htc(p) => Some(p),
            _ => None,
        }
    }
}
