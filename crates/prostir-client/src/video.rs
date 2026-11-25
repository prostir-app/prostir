//! The video of each stream as the core sees it (step 7 note, 4.4 and 9): the rules a
//! decoder follows when it fails, and what the menu shows of a monitor whose picture is
//! in trouble. The frames themselves never come here: the network hands them to the
//! decoder thread, which decodes them into the compositor's surface (the shell's
//! `platform::video`). The model only hears that a stream started, has trouble, shows
//! a picture again or failed for good, each for one generation of it.
//!
//! Two kinds of failure. A passing one (a codec error, a broken frame, a surface gone
//! for a moment) makes the codec again after a pause that grows from 0.2 s to 5 s and
//! then stays at 5 s, asking the PC for a keyframe each time; after
//! [`NEW_SURFACE_AFTER`] in a row the compositor's surface is made again too. It never
//! gives up: once a picture comes, it starts over. When no picture came for
//! [`TROUBLE_AFTER`] since a failure, the menu says the monitor is recovering. A
//! lasting one (no decoder for the codec, a size the codec refuses, no surface for the
//! stream) is said once, with its reason, and not tried again until the PC starts the
//! stream anew.
//!
//! Builds and is tested on any system.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use prostir_proto::Codec;
use prostir_proto::state::Gen;

use crate::ids::{DisplayKey, PcId};
use crate::lang::Texts;

/// The pauses before a codec is made again after its first, second... passing failure
/// in a row; after the last, each further one waits as long as the last.
pub const RETRY_PAUSES: [Duration; 5] = [
    Duration::from_millis(200),
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
];
/// After this many passing failures in a row, the compositor's surface is made again
/// with the codec (and again after as many more): the surface may be what broke.
pub const NEW_SURFACE_AFTER: u32 = 3;
/// A failure with no picture since for this long: the menu says the monitor is
/// recovering.
pub const TROUBLE_AFTER: Duration = Duration::from_secs(2);

/// The pause before the codec is made again after `failures` passing failures in a
/// row (1 for the first).
pub fn retry_pause(failures: u32) -> Duration {
    let i = usize::try_from(failures.saturating_sub(1)).unwrap_or(usize::MAX);
    RETRY_PAUSES
        .get(i)
        .copied()
        .unwrap_or(RETRY_PAUSES[RETRY_PAUSES.len() - 1])
}

/// Whether the surface is made again with the codec after `failures` passing failures
/// in a row.
pub fn new_surface_due(failures: u32) -> bool {
    failures > 0 && failures.is_multiple_of(NEW_SURFACE_AFTER)
}

/// A failure the hook of the selftest and the checks by hand cause on purpose, on the
/// real decoder (see `SurfaceVideo::inject` in the shell).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// The next frame's bytes are spoiled before the codec takes it.
    CorruptFrame,
    /// The codec fails as if it said so itself: made again after its pause.
    DecoderError,
    /// The surface counts as broken: made again with the codec at once.
    SurfaceLost,
}

/// Why a stream cannot be shown at all (a lasting failure).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// The headset has no decoder for the stream's codec.
    NoDecoder,
    /// The decoder refuses the stream's size.
    BadSize,
    /// The compositor gave no surface for the stream.
    NoSurface,
    /// The decoder thread ended without a word (it panicked).
    Stopped,
}

/// What became of a stream's video, as the shell saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Happened {
    /// It failed and no picture came since for [`TROUBLE_AFTER`].
    Trouble,
    /// A picture came again after [`Happened::Trouble`].
    Picture,
    /// It cannot be shown (said once a generation).
    Failed(Why),
}

/// What became of the video of display `id`'s generation `generation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoEvent {
    pub id: DisplayKey,
    pub generation: Gen,
    pub what: Happened,
}

/// The shell's watch over one stream's decoder: told each pass since when the stream
/// fails without a picture (`None`: it shows pictures), it says when the trouble
/// starts to show and when it is over. Nothing is made.
#[derive(Debug, Clone, Copy, Default)]
pub struct Watch {
    trouble: bool,
}

impl Watch {
    /// What changed at `now` for a stream failing since `failing` without a picture.
    pub fn look(&mut self, now: Instant, failing: Option<Instant>) -> Option<Happened> {
        match failing {
            Some(since) if !self.trouble => (now.saturating_duration_since(since) >= TROUBLE_AFTER)
                .then(|| {
                    self.trouble = true;
                    Happened::Trouble
                }),
            None if self.trouble => {
                self.trouble = false;
                Some(Happened::Picture)
            }
            _ => None,
        }
    }
}

/// How a stream's picture is, as the menu shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Health {
    #[default]
    Fine,
    /// No picture since a failure, being recovered.
    Trouble,
    /// It cannot be shown, for this reason.
    Failed(Why),
}

/// One stream as the model keeps it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Kept {
    generation: Gen,
    codec: Codec,
    size: (u32, u32),
    health: Health,
}

/// The model's side: the generation of each display's stream, and how its picture is.
/// What comes of an older generation is let go.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Videos {
    streams: BTreeMap<DisplayKey, Kept>,
}

/// A stream that cannot be shown: its display, size and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Failed {
    pub id: DisplayKey,
    pub codec: Codec,
    pub size: (u32, u32),
    pub why: Why,
}

impl Videos {
    /// Generation `generation` of display `id`'s stream started, `size` pixels in
    /// `codec`: whatever an older one came to is over.
    pub fn started(&mut self, id: DisplayKey, generation: Gen, codec: Codec, size: (u32, u32)) {
        self.streams.insert(
            id,
            Kept {
                generation,
                codec,
                size,
                health: Health::Fine,
            },
        );
    }

    /// What came of a stream's video: taken (`Some`, the stream's health before) only
    /// for the generation that streams now.
    pub fn happened(&mut self, e: &VideoEvent) -> Option<Health> {
        let kept = self
            .streams
            .get_mut(&e.id)
            .filter(|k| k.generation == e.generation)?;
        let before = kept.health;
        // A failure stays until the next generation.
        if matches!(before, Health::Failed(_)) {
            return None;
        }
        kept.health = match e.what {
            Happened::Trouble => Health::Trouble,
            Happened::Picture => Health::Fine,
            Happened::Failed(why) => Health::Failed(why),
        };
        Some(before)
    }

    /// Display `id`'s stream ended, or the display went.
    pub fn ended(&mut self, id: DisplayKey) {
        self.streams.remove(&id);
    }

    /// The session with PC `pc` ended: its streams with it.
    pub fn pc_ended(&mut self, pc: PcId) {
        self.streams.retain(|k, _| k.pc != pc);
    }

    /// How display `id`'s picture is (fine when it has no stream).
    pub fn health(&self, id: DisplayKey) -> Health {
        self.streams.get(&id).map_or(Health::Fine, |k| k.health)
    }

    /// The streams that cannot be shown, by display.
    pub fn failed(&self) -> impl Iterator<Item = Failed> + '_ {
        self.streams.iter().filter_map(|(id, k)| match k.health {
            Health::Failed(why) => Some(Failed {
                id: *id,
                codec: k.codec,
                size: k.size,
                why,
            }),
            _ => None,
        })
    }
}

/// A codec as people know it.
pub fn codec_name(codec: Codec) -> &'static str {
    match codec {
        Codec::H264 => "H.264",
        Codec::H265 => "H.265",
        Codec::Av1 => "AV1",
    }
}

/// Why the stream `f` cannot be shown, in the menu's words.
pub fn why_text(f: &Failed, texts: &Texts) -> String {
    let codec = codec_name(f.codec);
    match f.why {
        Why::NoDecoder => texts.f("headset-video-no-decoder", &[("codec", codec.into())]),
        Why::BadSize => texts.f(
            "headset-video-bad-size",
            &[
                ("codec", codec.into()),
                ("size", format!("{} × {}", f.size.0, f.size.1).into()),
            ],
        ),
        Why::NoSurface => texts.t("headset-video-no-surface").to_owned(),
        Why::Stopped => texts.t("headset-video-stopped").to_owned(),
    }
}

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "a test's own starting time; the code under test takes the time as a parameter"
)]
mod tests {
    use super::*;
    use prostir_proto::state::DisplayId;

    const PC: PcId = PcId([1; 8]);
    const MS: Duration = Duration::from_millis(1);

    fn k(n: u32) -> DisplayKey {
        DisplayKey::new(PC, DisplayId(n))
    }

    /// The pauses grow to 5 s and stay; the surface goes with every third failure.
    #[test]
    fn pauses_grow_and_stay() {
        let pauses: Vec<u64> = (1..=8).map(|n| retry_pause(n).as_millis() as u64).collect();
        assert_eq!(pauses, [200, 500, 1000, 2000, 5000, 5000, 5000, 5000]);
        assert_eq!(retry_pause(0), RETRY_PAUSES[0]);
        assert_eq!(retry_pause(u32::MAX), Duration::from_secs(5));
        let surfaces: Vec<u32> = (0..10).filter(|n| new_surface_due(*n)).collect();
        assert_eq!(surfaces, [3, 6, 9]);
    }

    /// Trouble shows only after 2 s without a picture since a failure, once; the next
    /// picture ends it, once.
    #[test]
    fn trouble_after_two_seconds_without_a_picture() {
        let t0 = Instant::now();
        let mut w = Watch::default();
        assert_eq!(w.look(t0, None), None);
        assert_eq!(w.look(t0 + 1999 * MS, Some(t0)), None);
        assert_eq!(w.look(t0 + 2000 * MS, Some(t0)), Some(Happened::Trouble));
        assert_eq!(w.look(t0 + 9000 * MS, Some(t0)), None, "said once");
        assert_eq!(w.look(t0 + 9001 * MS, None), Some(Happened::Picture));
        assert_eq!(w.look(t0 + 9002 * MS, None), None);
        // A failure that recovered within 2 s never shows.
        let mut w = Watch::default();
        assert_eq!(w.look(t0 + 500 * MS, Some(t0)), None);
        assert_eq!(w.look(t0 + 600 * MS, None), None);
    }

    /// Only the generation that streams now counts; a failure stays until the next
    /// generation, which starts fine.
    #[test]
    fn generations() {
        let mut v = Videos::default();
        let (g1, g2) = (Gen(1), Gen(2));
        v.started(k(1), g1, Codec::H265, (7680, 2160));
        let ev = |generation, what| VideoEvent {
            id: k(1),
            generation,
            what,
        };
        assert_eq!(v.happened(&ev(g1, Happened::Trouble)), Some(Health::Fine));
        assert_eq!(v.health(k(1)), Health::Trouble);
        v.started(k(1), g2, Codec::H265, (7680, 2160));
        assert_eq!(v.health(k(1)), Health::Fine, "a new generation starts fine");
        assert_eq!(v.happened(&ev(g1, Happened::Failed(Why::BadSize))), None);
        assert_eq!(v.health(k(1)), Health::Fine, "the old one is let go");
        assert_eq!(
            v.happened(&ev(g2, Happened::Failed(Why::BadSize))),
            Some(Health::Fine)
        );
        assert_eq!(v.happened(&ev(g2, Happened::Picture)), None, "it stays");
        let failed: Vec<Failed> = v.failed().collect();
        assert_eq!(
            failed,
            [Failed {
                id: k(1),
                codec: Codec::H265,
                size: (7680, 2160),
                why: Why::BadSize
            }]
        );
        // A display nobody started is fine, and what comes of it is let go.
        assert_eq!(v.health(k(9)), Health::Fine);
        v.pc_ended(PC);
        assert_eq!(v.failed().count(), 0);
    }

    /// Each reason has its words, in each language shipped.
    #[test]
    fn every_reason_said() {
        for lang in ["en", "uk"] {
            let texts = crate::lang::builtin(lang);
            for why in [Why::NoDecoder, Why::BadSize, Why::NoSurface, Why::Stopped] {
                let f = Failed {
                    id: k(1),
                    codec: Codec::H265,
                    size: (7680, 2160),
                    why,
                };
                let text = why_text(&f, &texts);
                assert!(
                    !text.is_empty() && !text.contains("headset-"),
                    "{lang}: {text}"
                );
                if why == Why::BadSize {
                    assert!(text.contains("7680") && text.contains("H.265"), "{text}");
                }
            }
        }
    }
}
