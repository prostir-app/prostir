//! The headset's up streams (its microphone), taken by the session's reader thread:
//! a piece of the microphone goes to the PC's end of it as it comes, without waiting
//! for the control loop (which may be in a long pass, while the pipe to the PC's
//! microphone holds only about 40 ms). The control loop only reads which streams there
//! are, and says whether this session's microphone is the PC's.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use log::{info, warn};
use prostir_proto::state::{UpFormat, UpKind};

use crate::displays::MicTap;

/// One session's up streams.
#[derive(Debug, Default)]
pub struct Ups {
    /// Started and not ended: by number, what each carries and in how many channels.
    streams: Mutex<BTreeMap<u8, (UpKind, u8)>>,
    /// The headset has the right to send its microphone (its line in the settings).
    may_mic: bool,
    /// Its microphone is the PC's end of the headset microphone (see `Standing`).
    mic: AtomicBool,
}

impl Ups {
    pub fn new(may_mic: bool, mic: bool) -> Self {
        Self {
            streams: Mutex::default(),
            may_mic,
            mic: AtomicBool::new(mic),
        }
    }

    /// Whether this session's microphone is the PC's, as the control loop says.
    pub fn set_mic(&self, mic: bool) {
        self.mic.store(mic, Ordering::Relaxed);
    }

    /// The headset streams its microphone now.
    pub fn mic_streams(&self) -> bool {
        self.streams
            .lock()
            .unwrap()
            .values()
            .any(|(kind, _)| *kind == UpKind::Mic)
    }

    /// The headset starts up stream `up` of `kind` in `format`.
    pub fn start(&self, up: u8, kind: UpKind, format: UpFormat) {
        let pcm = match format {
            UpFormat::Pcm16 { rate, channels } if rate == prostir_proto::AUDIO_RATE => {
                Some(channels)
            }
            _ => None,
        };
        match pcm {
            _ if kind != UpKind::Mic || !self.may_mic => {
                info!("an up stream of {kind:?} this PC does not take");
            }
            Some(channels @ (1 | 2)) => {
                info!("the headset's microphone streams ({format:?}, up stream {up})");
                // A new recording replaces the one before, whose end may still be on
                // its way: that end then ends nothing.
                let mut streams = self.streams.lock().unwrap();
                streams.retain(|_, (k, _)| *k != UpKind::Mic);
                streams.insert(up, (kind, channels));
            }
            _ => warn!("the headset's microphone in {format:?}, which this PC does not take"),
        }
    }

    /// The headset ended up stream `up`.
    pub fn end(&self, up: u8) {
        if self.streams.lock().unwrap().remove(&up).is_some() {
            info!("the headset's microphone stopped (up stream {up})");
        }
    }

    /// A piece of up stream `up`: the microphone of the session that has it goes to
    /// the PC's end (`tap`), in mono, through `mono` (kept by the reader between
    /// pieces).
    pub fn piece(&self, up: u8, data: &[u8], tap: &MicTap, mono: &mut Vec<u8>) {
        if !self.mic.load(Ordering::Relaxed) {
            return;
        }
        let channels = match self.streams.lock().unwrap().get(&up) {
            Some((UpKind::Mic, channels)) => *channels,
            _ => return,
        };
        if channels == 2 {
            to_mono(data, mono);
            tap.push(mono);
        } else {
            tap.push(data);
        }
    }
}

/// Interleaved 16-bit stereo as mono into `mono`: each pair of samples is their mean.
pub fn to_mono(stereo: &[u8], mono: &mut Vec<u8>) {
    mono.clear();
    mono.extend(
        stereo
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|[l0, l1, r0, r1]| {
                let l = i32::from(i16::from_le_bytes([*l0, *l1]));
                let r = i32::from(i16::from_le_bytes([*r0, *r1]));
                (((l + r) / 2) as i16).to_le_bytes()
            }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mono_of(stereo: &[u8]) -> Vec<u8> {
        let mut mono = Vec::new();
        to_mono(stereo, &mut mono);
        mono
    }

    /// A stereo microphone reaches the PC's mono end as the mean of its channels.
    #[test]
    fn stereo_becomes_mono() {
        let frame = |l: i16, r: i16| [l.to_le_bytes(), r.to_le_bytes()].concat();
        let stereo = [frame(100, 300), frame(-32768, -32768), frame(32767, -32767)].concat();
        let mono: Vec<i16> = mono_of(&stereo)
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b))
            .collect();
        assert_eq!(mono, [200, -32768, 0]);
        assert!(mono_of(&[1, 2, 3]).is_empty(), "no half frame");
    }

    /// Only a microphone the headset may send, in a format the PC takes, of the
    /// session whose microphone is the PC's, counts; a new one replaces the old.
    #[test]
    fn which_microphone_counts() {
        let pcm = |channels| UpFormat::Pcm16 {
            rate: prostir_proto::AUDIO_RATE,
            channels,
        };
        let refused = Ups::new(false, true);
        refused.start(1, UpKind::Mic, pcm(1));
        assert!(!refused.mic_streams(), "no right to it");
        let ups = Ups::new(true, true);
        ups.start(1, UpKind::Mic, pcm(3));
        assert!(!ups.mic_streams(), "three channels");
        ups.start(
            1,
            UpKind::Mic,
            UpFormat::Pcm16 {
                rate: 44_100,
                channels: 1,
            },
        );
        assert!(!ups.mic_streams(), "another rate");
        ups.start(1, UpKind::Mic, pcm(2));
        ups.start(2, UpKind::Mic, pcm(1));
        assert!(ups.mic_streams());
        ups.end(1);
        assert!(ups.mic_streams(), "the old recording's end ends nothing");
        ups.end(2);
        assert!(!ups.mic_streams());
    }
}
