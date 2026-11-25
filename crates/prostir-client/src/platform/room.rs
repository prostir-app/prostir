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

/// The room as this headset shows it (see the module).
pub enum Room {
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
    /// with `shown` (`opacity` of it).
    pub fn make(ways: &[RoomWay], session: &xr::Session<Gfx>, shown: bool, opacity: f32) -> Self {
        for &way in ways {
            let made = match way {
                RoomWay::Glasses => Ok(Room::Glasses),
                RoomWay::Blend => Ok(Room::Blend { shown, opacity }),
                RoomWay::FbPassthrough => Passthrough::new(session, shown).map(Room::Fb),
                RoomWay::HtcPassthrough => htc::Passthrough::new(session).map(Room::Htc),
            };
            match made {
                Ok(mut room) => {
                    info!("the room: {way:?}");
                    if let Err(e) = room.set(shown, opacity) {
                        warn!("the room by {way:?}: {e:#}");
                    }
                    return room;
                }
                Err(e) => warn!("the room by {way:?} unavailable: {e:#}"),
            }
        }
        Room::None
    }

    /// The way it is shown (`None`: it is not).
    pub fn way(&self) -> Option<RoomWay> {
        match self {
            Room::None => None,
            Room::Glasses => Some(RoomWay::Glasses),
            Room::Blend { .. } => Some(RoomWay::Blend),
            Room::Fb(_) => Some(RoomWay::FbPassthrough),
            Room::Htc(_) => Some(RoomWay::HtcPassthrough),
        }
    }

    /// Shows the room behind the monitors (`on`), `opacity` of it (1: all; less
    /// darkens it towards black), or not.
    pub fn set(&mut self, on: bool, opacity: f32) -> Result<()> {
        match self {
            Room::None | Room::Glasses => Ok(()),
            Room::Blend { shown, opacity: o } => {
                *shown = on;
                *o = opacity;
                Ok(())
            }
            Room::Fb(p) => p.set_room(on).and_then(|()| p.set_room_opacity(opacity)),
            Room::Htc(p) => p.set_room(on, opacity),
        }
    }

    /// The way the frame is blended with the world.
    pub fn blend_mode(&self) -> xr::EnvironmentBlendMode {
        match self {
            Room::Glasses => xr::EnvironmentBlendMode::ADDITIVE,
            Room::Blend { shown: true, .. } => xr::EnvironmentBlendMode::ALPHA_BLEND,
            _ => xr::EnvironmentBlendMode::OPAQUE,
        }
    }

    /// The cameras' room shows behind the monitors (no panorama then; on glasses the
    /// room shows through whatever is drawn).
    pub fn shown(&self) -> bool {
        match self {
            Room::None | Room::Glasses => false,
            Room::Blend { shown, .. } => *shown,
            Room::Fb(p) => p.room_layer().is_some(),
            Room::Htc(p) => p.shown(),
        }
    }

    /// Blended with the cameras' picture and dimmed: how much a black layer over the
    /// room covers of it (0 to 255).
    pub fn dim(&self) -> Option<u8> {
        match self {
            Room::Blend {
                shown: true,
                opacity,
            } if *opacity < 1.0 => Some(((1.0 - opacity.clamp(0.0, 1.0)) * 255.0).round() as u8),
            _ => None,
        }
    }

    /// The cameras of `XR_FB_passthrough` (the window onto the keyboard is theirs).
    pub fn fb(&mut self) -> Option<&mut Passthrough> {
        match self {
            Room::Fb(p) => Some(p),
            _ => None,
        }
    }

    /// The same, to look at.
    pub fn fb_ref(&self) -> Option<&Passthrough> {
        match self {
            Room::Fb(p) => Some(p),
            _ => None,
        }
    }

    /// The cameras of `XR_HTC_passthrough`.
    pub fn htc_passthrough(&self) -> Option<&htc::Passthrough> {
        match self {
            Room::Htc(p) => Some(p),
            _ => None,
        }
    }
}
