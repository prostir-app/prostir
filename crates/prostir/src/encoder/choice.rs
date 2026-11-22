//! Which encoder an output gets: the encoders this PC has (as their probes found them,
//! see `probe.rs`), ranked for where the output's frames live.
//!
//! Frames stay on the graphics card wherever an encoder there takes them. On Windows a
//! frame copied to plain memory cost 74 % of a core with a hardware encoder and a whole
//! core with x264, against 7 to 13 % without the copy (`plans/windows-prototype.md`,
//! section 8). So an output gets, in this order:
//! 1. a hardware encoder that takes its frames as they are (VA-API takes Linux GPU
//!    buffers; on Windows the card's own encoder, then Media Foundation on the same
//!    card, which is slower: 53.8 frames a second against 60), or frames in plain
//!    memory, which every hardware encoder uploads;
//! 2. a hardware encoder that needs them copied through the CPU first (NVENC on Linux
//!    reads no GPU buffers of KWin's; a card other than the one the frames are on);
//! 3. x264 on the CPU, the last resort.
//!
//! In each, the encoders go in the order of [`Backend::ORDER`], and for each encoder
//! the codec asked for, then H.264 (which every headset decodes: an output never goes
//! to a codec a headset may not decode). One that already opened as many sessions as
//! it can at once is passed over, and so is one that did not open this run: the output
//! goes to the next.
//!
//! An output larger than an encoder takes is not passed over: VA-API scales a picture
//! wider than its H.264 takes (4096) down, as it always did, which is better than x264
//! at that size.

use std::fmt::Write as _;

use prostir_proto::Codec;

use crate::i18n::t;
use crate::platform::{AdapterId, Memory};

/// Which hardware (or the CPU) encodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Backend {
    /// VA-API: Intel and AMD graphics.
    Va,
    /// NVENC through GStreamer's nvcodec plugin: NVIDIA graphics with its own driver.
    Nvenc,
    /// x264 on the CPU (gst-plugins-ugly): works anywhere, costs CPU time.
    X264,
    /// NVENC taking Direct3D 11 textures (Windows, step 11c; provisional).
    NvencD3d11,
    /// AMD's AMF (Windows, step 11c; provisional).
    Amf,
    /// Intel Quick Sync (Windows, step 11c; provisional).
    Qsv,
    /// Media Foundation, on any card with a hardware encoder MFT: the slower fallback
    /// of the card the frames are on (Windows, step 11c; provisional).
    MediaFoundation,
    /// A stand-in for a hardware encoder in the end-to-end tests: x264 underneath,
    /// with the probe and the session limit `PROSTIR_FAKE_ENCODER` says (see
    /// `probe.rs`).
    #[cfg(feature = "fake-desktop")]
    Fake,
}

/// How frames reach an encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Path {
    /// As they are: GPU frames stay on the card, plain memory frames are uploaded (or
    /// read by x264 where they are).
    Direct,
    /// Copied through the CPU first: frames on the graphics card are read back.
    Copied,
}

impl Backend {
    /// Preference among encoders that take the frames the same way: a card's own
    /// encoder before Media Foundation, any of them before the CPU.
    pub const ORDER: &[Backend] = &[
        #[cfg(feature = "fake-desktop")]
        Backend::Fake,
        Backend::Va,
        Backend::Nvenc,
        Backend::NvencD3d11,
        Backend::Amf,
        Backend::Qsv,
        Backend::MediaFoundation,
        Backend::X264,
    ];

    /// Its name in the cache file and in logs.
    pub fn name(self) -> &'static str {
        match self {
            Self::Va => "va",
            Self::Nvenc => "nvenc",
            Self::X264 => "x264",
            Self::NvencD3d11 => "nvenc-d3d11",
            Self::Amf => "amf",
            Self::Qsv => "qsv",
            Self::MediaFoundation => "mf",
            #[cfg(feature = "fake-desktop")]
            Self::Fake => "fake",
        }
    }

    /// The one named `name` (see [`Backend::name`]).
    pub fn named(name: &str) -> Option<Self> {
        Self::ORDER.iter().copied().find(|b| b.name() == name)
    }

    /// What people see it called.
    pub fn label(self) -> &'static str {
        match self {
            Self::Va => "VA-API",
            Self::Nvenc | Self::NvencD3d11 => "NVENC",
            Self::X264 => "x264",
            Self::Amf => "AMF",
            Self::Qsv => "Quick Sync",
            Self::MediaFoundation => "Media Foundation",
            #[cfg(feature = "fake-desktop")]
            Self::Fake => "fake hardware",
        }
    }

    /// On the graphics card (every one but x264).
    pub fn hardware(self) -> bool {
        self != Self::X264
    }

    /// It opens only so many sessions at once (NVENC on consumer cards, AMF, Quick
    /// Sync, and Media Foundation on top of them), so one that does not open while
    /// others run may be out of them. VA-API drivers (Intel's, Mesa's) count none: a
    /// failure there is memory or a format, never a limit to learn.
    pub fn counts_sessions(self) -> bool {
        match self {
            Self::Va | Self::X264 => false,
            Self::Nvenc | Self::NvencD3d11 | Self::Amf | Self::Qsv | Self::MediaFoundation => true,
            #[cfg(feature = "fake-desktop")]
            Self::Fake => true,
        }
    }

    /// How frames in `memory` reach it when it runs on `adapter` (none: the card the
    /// system picks, or the CPU); none when it cannot take them at all.
    pub fn path(self, memory: Memory, adapter: Option<AdapterId>) -> Option<Path> {
        match (self, memory) {
            // Plain memory: uploaded by every hardware encoder, read where it is by x264.
            (_, Memory::System) => Some(Path::Direct),
            (Self::Va, Memory::DmaBuf) => Some(Path::Direct),
            // nvcodec takes no Linux GPU buffers from PipeWire or wlr-screencopy.
            (Self::Nvenc, Memory::DmaBuf) => Some(Path::Copied),
            (
                Self::NvencD3d11 | Self::Amf | Self::Qsv | Self::MediaFoundation,
                Memory::D3D11 { adapter: on },
            ) => {
                // Textures of another card go through plain memory.
                Some(if adapter == Some(on) {
                    Path::Direct
                } else {
                    Path::Copied
                })
            }
            (Self::X264, _) => Some(Path::Copied),
            #[cfg(feature = "fake-desktop")]
            (Self::Fake, _) => Some(Path::Copied),
            (Self::Va | Self::Nvenc, Memory::D3D11 { .. })
            | (Self::NvencD3d11 | Self::Amf | Self::Qsv | Self::MediaFoundation, Memory::DmaBuf) => {
                None
            }
        }
    }

    /// Whether it takes frames straight from Linux GPU buffers (dmabuf).
    pub fn takes_dmabuf(self) -> bool {
        self.path(Memory::DmaBuf, None) == Some(Path::Direct)
    }
}

/// A codec an encoder makes, up to the largest picture its elements take (for the log
/// and the local API: a larger one is scaled down to it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodecCaps {
    pub codec: Codec,
    pub max_w: u32,
    pub max_h: u32,
}

/// An encoder of this PC, as its probe found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncoderCaps {
    pub backend: Backend,
    /// The card it runs on, where the system tells cards apart (Windows: the LUID);
    /// none on Linux, where it is the card the driver picks.
    pub adapter: Option<AdapterId>,
    /// The codecs it encoded in its probe, sharpest first.
    pub codecs: Vec<CodecCaps>,
    /// Sessions it had open when one more did not open (never guessed: NVENC ran 10 at
    /// once on driver 621, so there is no fixed 8); none while not known.
    pub sessions: Option<u32>,
}

impl EncoderCaps {
    /// Which encoder it is, to count its sessions by.
    pub fn kind(&self) -> Kind {
        (self.backend, self.adapter)
    }

    pub fn codec(&self, codec: Codec) -> Option<&CodecCaps> {
        self.codecs.iter().find(|c| c.codec == codec)
    }
}

/// An encoder on a card: what sessions are counted by.
pub type Kind = (Backend, Option<AdapterId>);

/// What the encoders are asked for: a codec, and whether on the CPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wanted {
    pub codec: Codec,
    /// x264 only (`--software`, or chosen in the headset).
    pub software: bool,
}

impl Wanted {
    /// The codecs an output may stream in: the one asked for, else H.264, which every
    /// headset decodes.
    pub fn codecs(self) -> Vec<Codec> {
        let mut codecs = vec![self.codec];
        if self.codec != Codec::H264 {
            codecs.push(Codec::H264);
        }
        codecs
    }
}

/// An encoder for one output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    pub backend: Backend,
    pub adapter: Option<AdapterId>,
    pub codec: Codec,
    pub path: Path,
}

impl Choice {
    /// Frames on the graphics card go through the CPU on their way to the encoder.
    pub fn copies(&self) -> bool {
        self.path == Path::Copied
    }
}

/// For the log: `VA-API H265`.
impl std::fmt::Display for Choice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {:?}", self.backend.label(), self.codec)
    }
}

impl Choice {
    /// What people see: `VA-API HEVC`.
    pub fn label(&self) -> String {
        format!("{} {}", self.backend.label(), codec_label(self.codec))
    }
}

/// What people see a codec called: H.265 is HEVC, as in the headset's menu.
pub fn codec_label(codec: Codec) -> &'static str {
    match codec {
        Codec::H264 => "H.264",
        Codec::H265 => "HEVC",
        Codec::Av1 => "AV1",
    }
}

/// Why an encoder was passed over for an output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skip {
    /// It has as many sessions open as it opened at most.
    Limit { max: u32 },
    /// It did not open for an output this run (see `Catalog::failed`).
    Failed { why: String },
}

impl Skip {
    /// For people: why, after the encoder's name.
    pub fn text(&self) -> String {
        match self {
            Self::Limit { max } => format!(
                "{} {max} {}",
                t("уже відкрито", "already has"),
                t(
                    "сеансів, більше не відкривається",
                    "sessions open, the most it opens"
                )
            ),
            Self::Failed { why } => format!("{}: {why}", t("не запустився", "did not start")),
        }
    }
}

/// The encoders for one output, best first, and those passed over with why.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ranked {
    pub choices: Vec<Choice>,
    pub skipped: Vec<(Kind, Skip)>,
}

/// The encoders of `encoders` for frames in `memory`, best first (see the top of this
/// file), each in `codecs` in that order; only x264 with `software`. `running` counts
/// the sessions each encoder has open now; `failed` are the encoders and codecs that
/// did not open this run.
pub fn rank(
    encoders: &[EncoderCaps],
    memory: Memory,
    codecs: &[Codec],
    software: bool,
    running: &dyn Fn(Kind) -> u32,
    failed: &[(Kind, Codec, String)],
) -> Ranked {
    let mut order: Vec<(&EncoderCaps, Path)> = encoders
        .iter()
        .filter(|e| !software || !e.backend.hardware())
        .filter_map(|e| Some((e, e.backend.path(memory, e.adapter)?)))
        .collect();
    let place = |b: Backend| Backend::ORDER.iter().position(|o| *o == b);
    order.sort_by_key(|(e, path)| (!e.backend.hardware(), *path, place(e.backend)));
    let mut ranked = Ranked::default();
    for (e, path) in order {
        if let Some(max) = e.sessions
            && running(e.kind()) >= max
        {
            ranked.skipped.push((e.kind(), Skip::Limit { max }));
            continue;
        }
        for &codec in codecs {
            if e.codec(codec).is_none() {
                continue;
            }
            if let Some((_, _, why)) = failed
                .iter()
                .find(|(k, c, _)| *k == e.kind() && *c == codec)
            {
                ranked
                    .skipped
                    .push((e.kind(), Skip::Failed { why: why.clone() }));
                continue;
            }
            ranked.choices.push(Choice {
                backend: e.backend,
                adapter: e.adapter,
                codec,
                path,
            });
        }
    }
    ranked
}

/// What to say of `chosen` for an output, for the log, the tray and the local API:
/// the encoders passed over, those that did not open (`tried`), frames copied through
/// the CPU on their way to a hardware encoder, and x264 when the graphics card was
/// asked for. None when there is nothing to say: the output got the best this PC has
/// for it, without a copy.
pub fn note(
    chosen: &Choice,
    wanted: Wanted,
    skipped: &[(Kind, Skip)],
    tried: &[(Choice, String)],
) -> Option<String> {
    let why: Vec<String> = skipped
        .iter()
        .map(|((b, _), skip)| format!("{}: {}", b.label(), skip.text()))
        .chain(tried.iter().map(|(c, why)| {
            format!(
                "{}: {}: {why}",
                c.label(),
                t("не запустився", "did not start")
            )
        }))
        .collect();
    let cpu = !chosen.backend.hardware() && !wanted.software;
    let copies = chosen.copies() && chosen.backend.hardware();
    if why.is_empty() && !cpu && !copies {
        return None;
    }
    let mut text = chosen.label();
    if cpu {
        let _ = write!(
            text,
            ", {}",
            t("кодування процесором", "encoding on the CPU")
        );
    }
    if copies {
        let _ = write!(
            text,
            ", {}",
            t("кадри йдуть через процесор", "frames go through the CPU")
        );
    }
    if !why.is_empty() {
        let _ = write!(text, " ({})", why.join("; "));
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoder(backend: Backend, adapter: Option<u64>, codecs: &[(Codec, u32)]) -> EncoderCaps {
        EncoderCaps {
            backend,
            adapter: adapter.map(AdapterId),
            codecs: codecs
                .iter()
                .map(|&(codec, max)| CodecCaps {
                    codec,
                    max_w: max,
                    max_h: max,
                })
                .collect(),
            sessions: None,
        }
    }

    /// An Intel or AMD PC with NVIDIA beside it, and x264.
    fn linux() -> Vec<EncoderCaps> {
        vec![
            encoder(
                Backend::Va,
                None,
                &[(Codec::H265, 8192), (Codec::H264, 4096)],
            ),
            encoder(
                Backend::Nvenc,
                None,
                &[(Codec::H265, 8192), (Codec::H264, 4096)],
            ),
            encoder(Backend::X264, None, &[(Codec::H264, 16384)]),
        ]
    }

    fn hardware(codec: Codec) -> Wanted {
        Wanted {
            codec,
            software: false,
        }
    }

    fn none(_: Kind) -> u32 {
        0
    }

    /// The encoders for an output, as `Catalog::rank` asks.
    fn rank_for(
        encoders: &[EncoderCaps],
        memory: Memory,
        wanted: Wanted,
        running: &dyn Fn(Kind) -> u32,
        failed: &[(Kind, Codec, String)],
    ) -> Ranked {
        let codecs = wanted.codecs();
        rank(encoders, memory, &codecs, wanted.software, running, failed)
    }

    fn order(ranked: &Ranked) -> Vec<(Backend, Codec, Path)> {
        ranked
            .choices
            .iter()
            .map(|c| (c.backend, c.codec, c.path))
            .collect()
    }

    /// KWin's and wlr-screencopy's GPU buffers: VA-API takes them as they are, NVENC
    /// after a copy, x264 last; the codec asked for first in each, then H.264.
    #[test]
    fn linux_gpu_buffers() {
        let ranked = rank_for(&linux(), Memory::DmaBuf, hardware(Codec::H265), &none, &[]);
        assert_eq!(
            order(&ranked),
            vec![
                (Backend::Va, Codec::H265, Path::Direct),
                (Backend::Va, Codec::H264, Path::Direct),
                (Backend::Nvenc, Codec::H265, Path::Copied),
                (Backend::Nvenc, Codec::H264, Path::Copied),
                (Backend::X264, Codec::H264, Path::Copied),
            ]
        );
        assert!(ranked.skipped.is_empty());
        let first = ranked.choices[0];
        assert!(!first.copies());
        let said = note(&first, hardware(Codec::H265), &[], &[]);
        assert_eq!(said, None, "the best: nothing to say");
        // H.264 asked for: never H.265, which a headset may not decode.
        let ranked = ranked_h264(&linux());
        assert_eq!(
            order(&ranked),
            vec![
                (Backend::Va, Codec::H264, Path::Direct),
                (Backend::Nvenc, Codec::H264, Path::Copied),
                (Backend::X264, Codec::H264, Path::Copied),
            ]
        );
    }

    fn ranked_h264(encoders: &[EncoderCaps]) -> Ranked {
        rank_for(encoders, Memory::DmaBuf, hardware(Codec::H264), &none, &[])
    }

    /// Plain memory (the fake desktop, shared memory from a compositor): every hardware
    /// encoder uploads it, x264 reads it where it is.
    #[test]
    fn plain_memory() {
        let ranked = rank_for(&linux(), Memory::System, hardware(Codec::H265), &none, &[]);
        let paths: Vec<Path> = ranked.choices.iter().map(|c| c.path).collect();
        assert!(paths.iter().all(|p| *p == Path::Direct), "{paths:?}");
        assert_eq!(ranked.choices[0].backend, Backend::Va);
        assert_eq!(ranked.choices.last().unwrap().backend, Backend::X264);
    }

    /// `--software`, or chosen in the headset: x264 only, and nothing to say of it.
    #[test]
    fn software_is_x264_only() {
        let wanted = Wanted {
            codec: Codec::H265,
            software: true,
        };
        let ranked = rank_for(&linux(), Memory::DmaBuf, wanted, &none, &[]);
        assert_eq!(
            order(&ranked),
            vec![(Backend::X264, Codec::H264, Path::Copied)]
        );
        assert_eq!(note(&ranked.choices[0], wanted, &ranked.skipped, &[]), None);
    }

    /// NVIDIA alone: NVENC with frames copied through the CPU, which is said; x264
    /// when nothing on the card works, which is said too.
    #[test]
    fn copies_and_the_cpu_are_said() {
        let nvidia = vec![
            encoder(Backend::Nvenc, None, &[(Codec::H264, 4096)]),
            encoder(Backend::X264, None, &[(Codec::H264, 16384)]),
        ];
        let wanted = hardware(Codec::H265);
        let ranked = rank_for(&nvidia, Memory::DmaBuf, wanted, &none, &[]);
        let first = ranked.choices[0];
        assert_eq!((first.backend, first.codec), (Backend::Nvenc, Codec::H264));
        assert!(first.copies());
        let said = note(&first, wanted, &ranked.skipped, &[]).unwrap();
        let through = t("кадри йдуть через процесор", "frames go through the CPU");
        assert!(said.contains("NVENC") && said.contains(through), "{said}");
        let cpu = rank_for(&nvidia[1..], Memory::DmaBuf, wanted, &none, &[]);
        let said = note(&cpu.choices[0], wanted, &cpu.skipped, &[]).unwrap();
        let on_cpu = t("кодування процесором", "encoding on the CPU");
        assert!(said.contains(on_cpu), "{said}");
    }

    /// An encoder with as many sessions open as it opens is passed over for the next,
    /// for this output only, and the note says why; with fewer open it is used again.
    #[test]
    fn a_known_session_limit_moves_one_output_on() {
        let mut encoders = linux();
        encoders[0].sessions = Some(2);
        let wanted = hardware(Codec::H265);
        let two = |kind: Kind| u32::from(kind == (Backend::Va, None)) * 2;
        let ranked = rank_for(&encoders, Memory::DmaBuf, wanted, &two, &[]);
        assert_eq!(ranked.choices[0].backend, Backend::Nvenc);
        let limit = vec![((Backend::Va, None), Skip::Limit { max: 2 })];
        assert_eq!(ranked.skipped, limit);
        let said = note(&ranked.choices[0], wanted, &ranked.skipped, &[]).unwrap();
        assert!(said.contains("VA-API") && said.contains('2'), "{said}");
        let one = |kind: Kind| u32::from(kind == (Backend::Va, None));
        let ranked = rank_for(&encoders, Memory::DmaBuf, wanted, &one, &[]);
        assert_eq!(ranked.choices[0].backend, Backend::Va);
        // No limit known: never guessed.
        let ranked = rank_for(&linux(), Memory::DmaBuf, wanted, &|_| 99, &[]);
        assert_eq!(ranked.choices[0].backend, Backend::Va);
    }

    /// An encoder without the codec asked for: the next encoder that has it, after
    /// H.264 on the first (the card that takes the frames as they are wins).
    #[test]
    fn the_card_before_the_codec() {
        let encoders = vec![
            encoder(Backend::Va, None, &[(Codec::H264, 4096)]),
            encoder(Backend::Nvenc, None, &[(Codec::H265, 8192)]),
        ];
        let ranked = rank_for(&encoders, Memory::DmaBuf, hardware(Codec::H265), &none, &[]);
        assert_eq!(
            order(&ranked),
            vec![
                (Backend::Va, Codec::H264, Path::Direct),
                (Backend::Nvenc, Codec::H265, Path::Copied),
            ]
        );
    }

    /// An encoder and codec that did not open this run are passed over, with why.
    #[test]
    fn what_did_not_open_is_passed_over() {
        let failed = vec![((Backend::Va, None), Codec::H265, "no".to_owned())];
        let wanted = hardware(Codec::H265);
        let ranked = rank_for(&linux(), Memory::DmaBuf, wanted, &none, &failed);
        assert_eq!(order(&ranked)[0], (Backend::Va, Codec::H264, Path::Direct));
        assert!(matches!(&ranked.skipped[0].1, Skip::Failed { why } if why == "no"));
        let tried = vec![(ranked.choices[0], "out of memory".to_owned())];
        let said = note(&ranked.choices[1], wanted, &ranked.skipped, &tried).unwrap();
        assert!(
            said.contains("out of memory") && said.contains("no"),
            "{said}"
        );
    }

    /// The names in the cache file are the encoders' own, each once.
    #[test]
    fn names_go_both_ways() {
        for b in Backend::ORDER {
            assert_eq!(Backend::named(b.name()), Some(*b));
        }
        assert_eq!(Backend::named("quicksilver"), None);
        assert!(Backend::Va.takes_dmabuf());
        assert!(!Backend::Nvenc.takes_dmabuf() && !Backend::X264.takes_dmabuf());
    }

    /// What a Windows PC would rank (step 11c), so the model keeps room for it: the
    /// frames' card first (its own encoder, then Media Foundation on it, slower),
    /// another card after a copy, x264 last; nothing of Linux takes textures.
    #[test]
    fn windows_shape() {
        let (nvidia, amd) = (0x0001_2345, 0x0006_789a);
        let encoders = vec![
            encoder(Backend::Va, None, &[(Codec::H264, 4096)]),
            encoder(
                Backend::NvencD3d11,
                Some(nvidia),
                &[(Codec::H265, 8192), (Codec::H264, 4096)],
            ),
            encoder(Backend::Amf, Some(amd), &[(Codec::H264, 4096)]),
            encoder(
                Backend::MediaFoundation,
                Some(nvidia),
                &[(Codec::H264, 4096)],
            ),
            encoder(Backend::X264, None, &[(Codec::H264, 16384)]),
        ];
        let textures = Memory::D3D11 {
            adapter: AdapterId(nvidia),
        };
        let wanted = hardware(Codec::H265);
        let ranked = rank_for(&encoders, textures, wanted, &none, &[]);
        assert_eq!(
            order(&ranked),
            vec![
                (Backend::NvencD3d11, Codec::H265, Path::Direct),
                (Backend::NvencD3d11, Codec::H264, Path::Direct),
                (Backend::MediaFoundation, Codec::H264, Path::Direct),
                (Backend::Amf, Codec::H264, Path::Copied),
                (Backend::X264, Codec::H264, Path::Copied),
            ]
        );
        assert_eq!(ranked.choices[0].adapter, Some(AdapterId(nvidia)));
        // The NVIDIA card's sessions all open: Media Foundation on the same card.
        let mut limited = encoders.clone();
        limited[1].sessions = Some(10);
        let card = (Backend::NvencD3d11, Some(AdapterId(nvidia)));
        let full = |kind: Kind| u32::from(kind == card) * 10;
        let ranked = rank_for(&limited, textures, wanted, &full, &[]);
        assert_eq!(ranked.choices[0].backend, Backend::MediaFoundation);
        assert!(!ranked.choices[0].copies());
        // Frames on the AMD card: AMF takes them as they are.
        let on_amd = Memory::D3D11 {
            adapter: AdapterId(amd),
        };
        let ranked = rank_for(&encoders, on_amd, hardware(Codec::H264), &none, &[]);
        assert_eq!(order(&ranked)[0], (Backend::Amf, Codec::H264, Path::Direct));
        assert_eq!(Backend::Amf.path(Memory::DmaBuf, None), None);
        for b in [
            Backend::NvencD3d11,
            Backend::Amf,
            Backend::Qsv,
            Backend::MediaFoundation,
        ] {
            assert!(b.hardware() && !b.label().is_empty());
            assert_eq!(b.path(Memory::System, None), Some(Path::Direct));
        }
    }
}
