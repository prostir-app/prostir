//! The headset's requests: each changes the PC's state and gets exactly one outcome.
//! The parts it changed go out before the reply (see `publish`). A request this
//! headset has no right to gets `NOT_ALLOWED`; arguments out of range get their own
//! code, and nothing changes. A request that makes displays answers with all it made
//! at once, `Starting`, while the desktop's worker makes them; one that does not start
//! then stands in the list as `Failed`, keeping its place. A request whose answer only
//! the desktop has (a screenshot, where the mouse is, the active window) is answered
//! once the worker did it ([`Answer::Later`]).

use anyhow::{Context, Result, bail};
use log::info;
use prostir_proto::Codec;
use prostir_proto::request::{Outcome, Primary, Request};
use prostir_proto::state::{Access, HeadsetCaps, Limits, Mode, caps, perms};

use crate::displays::{Displays, JobAnswer};
use crate::errors::{self, Counted, UserError};
use crate::keep_screens;
use crate::platform::MonitorSpec;
use crate::screen::ScreenId;
use crate::store::Store;
use crate::worker::JobId;

/// What a request gets.
pub enum Answer {
    Now(Outcome),
    /// Once the desktop's worker has done job `JobId` (see [`outcome_of_job`]).
    Later(JobId),
}

/// The outcome of a request answered once its job was done: `id` and `answer` as the
/// control loop found them, or the job's failure.
pub fn outcome_of_job(
    result: Result<prostir_proto::state::DisplayId>,
    answer: JobAnswer,
) -> Outcome {
    match result {
        Err(e) => errors::outcome(&e),
        Ok(id) => match answer {
            JobAnswer::Done => Outcome::Done,
            JobAnswer::Added => Outcome::Added { id },
            JobAnswer::Saved { path, .. } => Outcome::Saved {
                path: prostir_proto::state::clip(
                    &home_relative(&path),
                    prostir_proto::request::MAX_PATH,
                ),
            },
            JobAnswer::Pointer(u, v) => Outcome::Pointer { id, u, v },
        },
    }
}

/// Who asks: what the headset can do, what the PC allows it, its rights.
pub struct Asker<'a> {
    pub caps: &'a HeadsetCaps,
    pub limits: &'a Limits,
    pub access: Access,
}

/// The rights a request needs.
fn needs(request: &Request) -> u32 {
    match request {
        Request::AddDisplay { .. }
        | Request::RemoveDisplay { .. }
        | Request::SetDisplays { .. }
        | Request::SetPrimary { .. }
        | Request::ShowPhysical { .. }
        | Request::SetScale { .. }
        | Request::AutoScale { .. }
        | Request::SetScaleAuto { .. }
        | Request::SetEncoding { .. }
        | Request::SetSound { .. } => perms::CHANGES,
        Request::DetachWindow => perms::CHANGES | perms::PRIVATE,
        Request::WindowHere { .. } => perms::INPUT,
        Request::Screenshot { .. } => perms::PRIVATE,
        Request::FindMouse => 0,
    }
}

/// Carries out `request` for `asker`: its outcome.
pub fn handle(
    request: Request,
    displays: &mut Displays,
    store: &mut Store,
    asker: &Asker<'_>,
) -> Answer {
    handle_checked(request, displays, store, asker)
        .unwrap_or_else(|e| Answer::Now(errors::outcome(&e)))
}

/// As [`handle`], with a failure as the error itself (its `UserError` and what lay
/// under it), for the PC's own requests.
pub fn handle_checked(
    request: Request,
    displays: &mut Displays,
    store: &mut Store,
    asker: &Asker<'_>,
) -> Result<Answer> {
    if !asker.access.may(needs(&request)) {
        info!("the headset has no right to {request:?}");
        bail!(UserError::NotAllowed);
    }
    carry_out(request, displays, store, asker)
}

fn carry_out(
    request: Request,
    displays: &mut Displays,
    store: &mut Store,
    asker: &Asker<'_>,
) -> Result<Answer> {
    let limits = asker.limits;
    Ok(Answer::Now(match request {
        Request::AddDisplay { mode } => {
            let spec = checked(mode, asker, displays.codec())?;
            let max = usize::from(limits.virtual_max);
            if displays.monitor_count() >= max {
                bail!(UserError::Limit {
                    max,
                    of: Counted::Monitors
                });
            }
            streams_within(displays.stream_count() + 1, limits)?;
            let id = store.new_id();
            displays.add(id, spec, max)?;
            info!("headset added {id}");
            keep_screens(store, displays, true);
            Outcome::Added { id: id.wire() }
        }
        Request::RemoveDisplay { id } => {
            let id = ScreenId::from_wire(id);
            let monitor = displays.is_monitor(id);
            displays.remove(id)?;
            info!("headset removed {id}");
            keep_screens(store, displays, monitor);
            Outcome::Done
        }
        Request::SetDisplays {
            modes,
            primary,
            shown_physical,
        } => {
            // Everything checked before anything changes.
            let max = usize::from(limits.virtual_max);
            if modes.is_empty() {
                bail!(UserError::BadArgument);
            }
            if modes.len() > max {
                bail!(UserError::Limit {
                    max,
                    of: Counted::Monitors
                });
            }
            let specs = modes
                .into_iter()
                .map(|m| checked(m, asker, displays.codec()))
                .collect::<Result<Vec<MonitorSpec>>>()?;
            let physical = displays.physical_ids();
            let physical_max = usize::from(limits.physical_max);
            let shown: Vec<ScreenId> = shown_physical
                .into_iter()
                .map(ScreenId::from_wire)
                .collect();
            if shown.iter().any(|id| !physical.contains(id)) {
                bail!(UserError::UnknownDisplay);
            }
            // One not connected now (a closed laptop's panel) streams nothing: it
            // counts for neither limit.
            let connected = shown
                .iter()
                .filter(|id| displays.is_connected_physical(**id))
                .count();
            if connected > physical_max {
                bail!(UserError::Limit {
                    max: physical_max,
                    of: Counted::Physical
                });
            }
            streams_within(specs.len() + connected + displays.window_count(), limits)?;
            let (index, main) = match primary {
                Some(Primary::Index { index }) if usize::from(index) < specs.len() => {
                    (Some(usize::from(index)), None)
                }
                Some(Primary::Display { id }) if physical.contains(&ScreenId::from_wire(id)) => {
                    (None, Some(ScreenId::from_wire(id)))
                }
                Some(Primary::Index { .. }) => bail!(UserError::BadArgument),
                Some(Primary::Display { .. }) => bail!(UserError::UnknownDisplay),
                None => (None, None),
            };
            if asker.caps.has(caps::headset::PHYSICAL) {
                // Those to hide first, so those to show stay within the limit.
                for show in [false, true] {
                    for id in physical.iter().filter(|id| shown.contains(id) == show) {
                        displays.show_physical(*id, show, physical_max)?;
                    }
                }
            }
            let ids = displays.set_monitors(&specs, index, &mut || store.new_id());
            if let (Ok(_), Some(id)) = (&ids, main) {
                displays.set_primary(id)?;
            }
            displays.force_keyframes();
            keep_screens(store, displays, true);
            let ids = ids?;
            info!("headset switched to {} monitor(s)", ids.len());
            Outcome::Ids {
                ids: ids.into_iter().map(ScreenId::wire).collect(),
            }
        }
        Request::SetPrimary { id } => {
            let id = ScreenId::from_wire(id);
            displays.set_primary(id)?;
            info!("headset made {id} the primary display");
            keep_screens(store, displays, false);
            Outcome::Done
        }
        Request::ShowPhysical { id, shown } => {
            if !asker.caps.has(caps::headset::PHYSICAL) {
                bail!(UserError::Unsupported);
            }
            let id = ScreenId::from_wire(id);
            if shown && !displays.is_shown_physical(id) && displays.is_connected_physical(id) {
                streams_within(displays.stream_count() + 1, limits)?;
            }
            displays.show_physical(id, shown, usize::from(limits.physical_max))?;
            // What a display seen for the first time gets, from now on.
            store.change(|s| s.mirror = shown);
            displays.apply(&store.wanted());
            keep_screens(store, displays, false);
            Outcome::Done
        }
        Request::SetScale { id, scale } => {
            let id = ScreenId::from_wire(id);
            // One of the scales the PC offers, as for a monitor added; the desktop's
            // own are checked by `rescale`.
            if !limits.scales.contains(&scale) {
                let (w, h) = displays.monitor_size(id).ok_or(UserError::UnknownDisplay)?;
                bail!(UserError::BadSize { w, h, scale });
            }
            displays.rescale(id, scale)?;
            info!("headset set {id} to {scale} %");
            keep_screens(store, displays, true);
            Outcome::Done
        }
        Request::AutoScale { id, scale } => {
            // The main headset's choice, where the desktop changes scales in place.
            if !displays.scales_in_place() {
                bail!(UserError::Unsupported);
            }
            if !asker.access.is_main() {
                bail!(UserError::NotAllowed);
            }
            let id = ScreenId::from_wire(id);
            if !limits.scales.contains(&scale) {
                let (w, h) = displays.monitor_size(id).ok_or(UserError::UnknownDisplay)?;
                bail!(UserError::BadSize { w, h, scale });
            }
            if displays.auto_scale(id, scale, &limits.scales)? {
                info!("headset chose {scale} % for {id} (Auto)");
                keep_screens(store, displays, false);
            }
            Outcome::Done
        }
        Request::SetScaleAuto { id, on } => {
            let id = ScreenId::from_wire(id);
            if displays.set_scale_auto(id, on)? {
                info!(
                    "headset turned Auto {} for {id}",
                    if on { "on" } else { "off" }
                );
                keep_screens(store, displays, false);
            }
            Outcome::Done
        }
        Request::SetEncoding {
            bitrate,
            codec,
            software,
            sharp,
        } => {
            if codec.is_some_and(|c| c != Codec::H264 && !asker.caps.decodes(c)) {
                bail!(UserError::BadArgument);
            }
            let now = store.wanted();
            // Kept as asked: the codec stays chosen when another one encodes.
            let bitrate = bitrate.map_or(now.bitrate, |b| u32::from(b).clamp(5, 100));
            let codec = codec.unwrap_or(now.codec);
            let software = software.unwrap_or(now.software);
            // What the PC would encode with: the codec chosen where every headset with
            // a session decodes it, else H.264 (the choice stays saved). Nothing
            // changes while a display streaming now would be too big for the headset
            // in it.
            let headsets = displays.router.headsets();
            let fits = Displays::codec_for(codec, &headsets);
            let (_, would) = crate::encoder::pick(fits, software).context(UserError::Encoder)?;
            if let Some((w, h)) = displays.too_big_for(would, asker.caps) {
                bail!(UserError::BadSize { w, h, scale: 100 });
            }
            store.set_encoding(bitrate, sharp.unwrap_or(now.sharp), codec, software);
            // Quality and clearer text apply even when the codec cannot change.
            displays.apply(&store.wanted());
            let result = displays.encode_for(codec, software, &headsets);
            // A monitor whose stream could not start again is gone.
            keep_screens(store, displays, false);
            result?;
            Outcome::Done
        }
        Request::SetSound { to_headset } => {
            store.change(|s| s.audio = to_headset);
            displays.apply(&store.wanted());
            Outcome::Done
        }
        Request::DetachWindow => {
            if !asker.caps.has(caps::headset::WINDOWS) {
                bail!(UserError::Unsupported);
            }
            streams_within(displays.stream_count() + 1, limits)?;
            let id = store.new_id();
            return Ok(Answer::Later(displays.detach_window(id)?));
        }
        Request::WindowHere { id } => {
            return Ok(Answer::Later(
                displays.window_here(ScreenId::from_wire(id))?,
            ));
        }
        Request::Screenshot { id } => {
            return Ok(Answer::Later(displays.screenshot(ScreenId::from_wire(id))?));
        }
        Request::FindMouse => return Ok(Answer::Later(displays.find_mouse())),
    }))
}

/// `LIMIT` when `streams` would be more than the headset decodes at once
/// (`Limits.streams_max`, from its `HeadsetCaps.streams_max`).
fn streams_within(streams: usize, limits: &Limits) -> Result<()> {
    let max = usize::from(limits.streams_max);
    if streams > max {
        bail!(UserError::Limit {
            max,
            of: Counted::Streams
        });
    }
    Ok(())
}

/// The monitor `mode` asks for, if the PC makes it: a size within its limits and one
/// of its scales, which the asker decodes in `codec` (the one the PC sends in now):
/// within its decoder's sides and pixel budget, where it names a decoder for it.
fn checked(mode: Mode, asker: &Asker<'_>, codec: Codec) -> Result<MonitorSpec> {
    let limits = asker.limits;
    let decodes = asker.caps.decoder(codec).is_none() || asker.caps.fits(codec, mode.w, mode.h);
    let fits = (limits.min.0..=limits.max.0).contains(&mode.w)
        && (limits.min.1..=limits.max.1).contains(&mode.h)
        && limits.scales.contains(&mode.scale)
        && decodes;
    let spec = MonitorSpec {
        width: mode.w,
        height: mode.h,
        scale: f64::from(mode.scale) / 100.0,
    };
    if !fits || spec.validate().is_err() {
        bail!(UserError::BadSize {
            w: mode.w,
            h: mode.h,
            scale: mode.scale
        });
    }
    Ok(spec)
}

/// `~/Pictures/…` rather than the full path, for showing.
fn home_relative(path: &std::path::Path) -> String {
    let shown = path.to_string_lossy().into_owned();
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && shown.starts_with(&home) => {
            format!("~{}", &shown[home.len()..])
        }
        _ => shown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prostir_proto::state::ErrorCode;

    fn limits(caps: &HeadsetCaps) -> Limits {
        crate::publish::limits(prostir_proto::state::Prefs::default(), caps, Codec::H264)
    }

    /// A headset with every right, its caps `caps` and the limits `limits`.
    fn asker<'a>(caps: &'a HeadsetCaps, limits: &'a Limits) -> Asker<'a> {
        Asker {
            caps,
            limits,
            access: Access::MAIN,
        }
    }

    /// Only sizes within the PC's limits and only its scales.
    #[test]
    fn sizes_are_checked() {
        let caps = HeadsetCaps::default();
        let l = limits(&caps);
        let a = asker(&caps, &l);
        assert!(checked(Mode::new(2560, 1440), &a, Codec::H264).is_ok());
        let larger_text = Mode {
            scale: 150,
            ..Mode::new(2560, 1440)
        };
        assert_eq!(checked(larger_text, &a, Codec::H264).unwrap().scale, 1.5);
        for bad in [
            Mode::new(100, 100),
            Mode::new(9000, 1440),
            Mode {
                scale: 110,
                ..Mode::new(1920, 1080)
            },
        ] {
            let e = checked(bad, &a, Codec::H264).unwrap_err();
            assert_eq!(errors::code_of(&e), ErrorCode::BAD_SIZE, "{bad:?}");
        }
    }

    /// A size within the sides of the headset's decoder but over its pixel budget (a
    /// 4K monitor for a decoder that keeps up with 2560x1600 at 60 frames a second) is
    /// not made in the codec the PC sends; within the budget, either way round, it is.
    /// A codec without a budget goes by its decoder's sides.
    #[test]
    fn sizes_stay_within_the_pixel_budget() {
        let caps = HeadsetCaps {
            decoders: vec![
                prostir_proto::state::Decoder {
                    codec: 0,
                    max_w: 4096,
                    max_h: 4096,
                },
                prostir_proto::state::Decoder {
                    codec: 1,
                    max_w: 4096,
                    max_h: 4096,
                },
            ],
            pixels: vec![prostir_proto::state::PixelBudget {
                codec: 0,
                max_pixels: 2560 * 1600,
            }],
            ..HeadsetCaps::default()
        };
        let l = limits(&caps);
        let a = asker(&caps, &l);
        let e = checked(Mode::new(3840, 2160), &a, Codec::H264).unwrap_err();
        assert_eq!(errors::code_of(&e), ErrorCode::BAD_SIZE);
        assert!(checked(Mode::new(2560, 1440), &a, Codec::H264).is_ok());
        assert!(checked(Mode::new(1440, 2560), &a, Codec::H264).is_ok());
        assert!(checked(Mode::new(3840, 2160), &a, Codec::H265).is_ok());
    }

    /// Each kind of request needs its right; finding the mouse needs none.
    #[test]
    fn rights_per_request() {
        let guest = Access {
            role: prostir_proto::state::role::GUEST,
            perms: perms::INPUT,
        };
        assert!(guest.may(needs(&Request::FindMouse)));
        assert!(guest.may(needs(&Request::WindowHere {
            id: prostir_proto::state::DisplayId(1)
        })));
        assert!(!guest.may(needs(&Request::AddDisplay {
            mode: Mode::new(1920, 1080)
        })));
        assert!(!guest.may(needs(&Request::Screenshot {
            id: prostir_proto::state::DisplayId(1)
        })));
        let changes = Access {
            perms: perms::CHANGES,
            ..guest
        };
        assert!(
            !changes.may(needs(&Request::DetachWindow)),
            "titles are private"
        );
        assert!(Access::MAIN.may(needs(&Request::DetachWindow)));
    }
}
