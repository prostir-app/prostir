//! The sound part of the platform contract: where the PC's sound goes while it plays
//! in the headset, the output device it is captured from, whether the headset
//! microphone can be a microphone of the PC (and why not), and what the sound system
//! says when it changes (the volume of the headset's device, a program starting or
//! stopping to record from the microphone), as events for the control loop.
//!
//! Each operating system has the devices in its `audio` module (see `platform/mod.rs`):
//! `AudioOut` (PC sound to the headset), `MicIn` (the headset microphone as the PC's),
//! `SinkVolume` (the level of the headset's device, read and set) and `Watcher`, which
//! follows the sound system while Prostir's devices exist and sends a [`SoundEvent`]
//! for each change. The watcher sleeps in the kernel until the sound system says
//! something: nothing is polled, and while the sound is idle nothing wakes.
//!
//! # Linux
//!
//! One long-lived `pactl subscribe` (see `sound_events.rs`), restarted with a growing
//! pause when it ends. Its lines name what changed (the headset's sink, the
//! microphone, a recording stream); the watcher then reads that device again and says
//! what is new. The capture reads the sink's monitor, which comes before the sink's
//! volume and mute (measured: a tone at 30 % and muted is captured as at 100 %), so
//! the level the PC writes for the shared volume does not change the captured sound
//! and the headset applies it once.
//!
//! # Windows (step 11c), from the prototype (`plans/windows-prototype.md`, section 10)
//!
//! The test `windows_shape` below builds what a Windows build would report.
//! - Loopback capture records silence unless it names the output device explicitly:
//!   [`OutputDevice`] holds the endpoint id the capture opens, never "the default".
//! - The PC's speakers can be muted while loopback captures them (it kept the full
//!   level on NVIDIA HDMI): [`Route::HeadsetOnly`]. Where muting silences the capture
//!   too (other drivers, to be checked), the sound plays in the speakers as well:
//!   [`Route::AlsoSpeakers`], which the tray says.
//! - There, loopback comes after the device's volume, so the shared volume would be
//!   applied twice (once on the PC, once in the headset): a Windows `SinkVolume::open`
//!   gives none until the capture makes up for the level. The volume's changes come
//!   from the endpoint's notifications, as events, never polled.
//! - A virtual microphone needs a device to play into that programs record from. An
//!   existing one is used (Steam Streaming Microphone worked, 90 ms); VB-CABLE is
//!   never bundled (SignPath's terms), only linked: [`MicAvailability::NeedsDriver`]
//!   with [`NeedsDriver::VB_CABLE`], which the tray and the settings window show as a
//!   download button and a hint which device to pick.

use std::fmt;
use std::time::Instant;

use prostir_proto::state::{self, ErrorCode, MicDevice};

use crate::i18n::inline_t;

/// Where the PC's sound plays while it goes to the headset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Only in the headset: the PC's own speakers are silent (Linux: the headset's
    /// device is the default output; Windows: the output captured is muted).
    HeadsetOnly,
    /// In the PC's speakers too: muting them would silence the capture.
    AlsoSpeakers,
}

/// The output device the PC's sound is captured from, by the id the sound system
/// knows it by: the sink's name on Linux, the endpoint id on Windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputDevice(pub String);

/// A driver the headset microphone needs on this PC, which Prostir does not ship: the
/// person gets a link to its maker's site and which device to pick once it is there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NeedsDriver {
    pub name: &'static str,
    pub url: &'static str,
    /// The device programs record from once it is installed.
    pub device: &'static str,
}

impl NeedsDriver {
    /// VB-Audio's virtual cable for Windows: the headset microphone plays into
    /// "CABLE Input", programs record from "CABLE Output".
    pub const VB_CABLE: Self = Self {
        name: "VB-CABLE",
        url: "https://vb-audio.com/Cable/",
        device: "CABLE Output",
    };
}

impl fmt::Display for NeedsDriver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = inline_t(
            "Потрібен драйвер {name}: {url}. Після встановлення оберіть у програмах \
             мікрофон «{device}».",
            "Needs the {name} driver: {url}. Once it is installed, choose “{device}” as \
             the microphone in programs.",
        );
        f.write_str(
            &text
                .replace("{name}", self.name)
                .replace("{url}", self.url)
                .replace("{device}", self.device),
        )
    }
}

impl std::error::Error for NeedsDriver {}

/// Whether the headset microphone can be a microphone of the PC, and why not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MicAvailability {
    Ready,
    /// The PC needs a driver for it first (Windows without a virtual cable).
    NeedsDriver(NeedsDriver),
    /// It could not be made: why, for people.
    Failed(String),
}

impl MicAvailability {
    /// Why `MicIn::start` failed: a missing driver (an error that is a
    /// [`NeedsDriver`]) or anything else.
    pub fn of_failure(e: &anyhow::Error) -> Self {
        match e.downcast_ref::<NeedsDriver>() {
            Some(driver) => Self::NeedsDriver(*driver),
            None => Self::Failed(format!("{e:#}")),
        }
    }

    /// As the headset is told it (`Sound.mic`): a missing driver is a lasting failure
    /// the PC cannot fix by itself.
    pub fn device(&self) -> MicDevice {
        match self {
            Self::Ready => MicDevice::Ready,
            Self::NeedsDriver(driver) => MicDevice::Failed {
                error: ErrorCode::UNSUPPORTED,
                detail: state::clip(&driver.to_string(), state::MAX_DETAIL),
            },
            Self::Failed(why) => MicDevice::Failed {
                error: crate::errors::UserError::Sound.code(),
                detail: state::clip(why, state::MAX_DETAIL),
            },
        }
    }
}

/// The PC's sound as it is now, for the log (and, from step 6.7 on, the local API
/// and the settings window).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoundStatus {
    /// Where the PC's sound plays, while it goes to the headset.
    pub route: Option<Route>,
    /// The device it is captured from, while it goes to the headset.
    pub output: Option<OutputDevice>,
    /// The headset microphone on the PC, while the headset's switch wants it.
    pub mic: Option<MicAvailability>,
}

impl fmt::Display for SoundStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.route, &self.output) {
            (Some(route), Some(OutputDevice(id))) => write!(f, "PC sound {route:?} from {id}")?,
            _ => f.write_str("PC sound stays on the PC")?,
        }
        match &self.mic {
            None => f.write_str(", no headset microphone"),
            Some(MicAvailability::Ready) => f.write_str(", headset microphone ready"),
            Some(MicAvailability::NeedsDriver(d)) => {
                write!(f, ", headset microphone needs {}", d.name)
            }
            Some(MicAvailability::Failed(why)) => write!(f, ", no headset microphone: {why}"),
        }
    }
}

/// What the sound system said, for the control loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoundEvent {
    /// The headset's device is at `level` percent (`muted` or not) since `at`: moved
    /// by the slider or the keys, or by the PC itself (its echo). `writes` is how many
    /// times the PC had set the device when it was read: a reading from before the
    /// PC's last write is old news (`SinkVolume::outdated`), and the write's own echo
    /// comes after it. `new_device`: the first reading of a device made anew (the
    /// headset taken off and put on, the sound switched off and on), whose level is
    /// whatever the sound system gives a new device, not the person's.
    Volume {
        level: u8,
        muted: bool,
        at: Instant,
        writes: u64,
        new_device: bool,
    },
    /// A program records from the headset microphone, or no longer does.
    Recording(bool),
}

#[cfg(test)]
mod windows_shape {
    //! What a Windows build would report, built here so the shape stays expressible
    //! without one.
    use super::*;

    #[test]
    fn windows_sound_fits_the_contract() {
        // The output captured, by its endpoint id (loopback of "the default" is
        // silent), muted while the headset plays it.
        let output = OutputDevice("{0.0.0.00000000}.{6f9f0b6c-2a55-4f0f-9c1e-3c2ad2ae0f11}".into());
        let status = SoundStatus {
            route: Some(Route::HeadsetOnly),
            output: Some(output.clone()),
            mic: Some(MicAvailability::Ready),
        };
        assert!(status.to_string().contains(&output.0));
        // A driver where muting silences the capture too.
        let loud = SoundStatus {
            route: Some(Route::AlsoSpeakers),
            ..status
        };
        assert!(loud.to_string().contains("AlsoSpeakers"));
    }

    /// No virtual cable: the microphone needs VB-CABLE, only linked, and the headset
    /// hears why with the link and the device to pick.
    #[test]
    fn a_missing_driver_is_a_link_and_a_hint() {
        let failed = anyhow::Error::new(NeedsDriver::VB_CABLE).context("no headset microphone");
        let mic = MicAvailability::of_failure(&failed);
        assert_eq!(mic, MicAvailability::NeedsDriver(NeedsDriver::VB_CABLE));
        let MicDevice::Failed { error, detail } = mic.device() else {
            panic!("not a failure");
        };
        assert_eq!(error, ErrorCode::UNSUPPORTED);
        assert!(detail.contains("https://vb-audio.com/Cable/"));
        assert!(detail.contains("CABLE Output"));
        let status = SoundStatus {
            route: None,
            output: None,
            mic: Some(mic),
        };
        assert!(status.to_string().contains("needs VB-CABLE"));
    }

    #[test]
    fn any_other_failure_says_what_went_wrong() {
        let mic = MicAvailability::of_failure(&anyhow::anyhow!("pacat is needed"));
        assert_eq!(mic, MicAvailability::Failed("pacat is needed".into()));
        assert!(matches!(
            mic.device(),
            MicDevice::Failed { error, .. } if error == ErrorCode::SOUND
        ));
        assert_eq!(MicAvailability::Ready.device(), MicDevice::Ready);
    }
}
