//! Wire protocol between the PC host and the Quest client.
//!
//! The client connects over TCP (USB via `adb reverse`, or Wi-Fi after discovering the
//! host through a UDP broadcast beacon or by asking, see [`discovery`]), and both
//! sides run an encrypted handshake ([`link::tcp`]); who trusts whom is [`secure`].
//! After it every message is framed as `[tag: u8][len: u32 LE][payload: len bytes]`,
//! and travels in [`packet`]s after the version handshake ([`handshake`]). A payload
//! is the message's fields encoded as [`wire`] describes, and must be read whole:
//! bytes left over are an error. A message with a tag a reader does not know is
//! skipped, so a newer peer can add messages without a new protocol version.
//!
//! # The version handshake (frozen)
//!
//! The headset's first message is [`ClientMsg::Hello`], before anything else (pairing
//! too). The PC answers with [`HostMsg::Welcome`] when the versions fit, or with
//! [`HostMsg::Error`] and [`HostMsg::Incompatible`] when they do not, and then closes
//! the connection. These four messages keep the bytes they had in protocol 1 in every
//! version, so any two versions can tell the user which side to update: the fixed
//! fields, then a UTF-8 text running to the end of the payload. They must never
//! change; what a new version needs goes into other messages.
//!
//! Only the media messages ([`HostMsg::Frame`], [`HostMsg::Audio`], [`ClientMsg::Up`])
//! end in bytes running to the end of the payload too. They are read and written by
//! hand, so their data is never copied on the way.
//!
//! # State, requests and replies
//!
//! Each side tells the other its state in parts ([`state`]): a message of its own for
//! each change, always the whole part, and all of them at once at the start of a
//! session, in [`ClientMsg::Open`] and [`HostMsg::Snapshot`] (see [`state::PcState`]).
//! What the headset asks the PC to change goes in [`ClientMsg::Request`], answered by
//! exactly one [`HostMsg::Reply`] ([`request`]); the parts it changed come before it.

// Everything here parses data from the network: no unsafe code at all.
#![forbid(unsafe_code)]

pub mod discovery;
pub mod handshake;
pub mod link;
pub mod packet;
pub mod request;
pub mod secure;
pub mod state;
pub mod stats;
pub mod volume;
pub mod wire;

use std::io::{self, Read, Write};

use link::RecordReceiver;
use packet::{Assembler, Encoded, Limits, Message};
use request::{Asked, Command, Outcome, Request};
use state::{
    Access, Battery, DisplayId, Displays, Encoding, ErrorCode, Focus, Gen, HeadsetCaps, HostCaps,
    MAX_DETAIL, MAX_DISPLAYS, MAX_GROWING, Part, PcInfo, Prefs, Presence, ReqId, Row, Sound,
    SoundFrom, UpFormat, UpKind, UpState, UpWanted, Volume, VolumeAsk,
};
use wire::{Reader, Wire, WireError, wire_enum};

pub const TCP_PORT: u16 = 47830;
pub const DISCOVERY_PORT: u16 = 47831;
/// Newest protocol this build speaks. Messages added later are skipped by older peers,
/// so a new optional message does not need a new version.
pub const PROTOCOL_VERSION: u32 = 2;
/// Oldest peer protocol this build can still talk to. Raise it only for a change an
/// older peer cannot just skip.
pub const MIN_PROTOCOL: u32 = 2;

/// Whether a peer that speaks `version` and still understands down to `min` can talk
/// to this build.
pub fn compatible(version: u32, min: u32) -> bool {
    version >= MIN_PROTOCOL && min <= PROTOCOL_VERSION
}

/// The revision of the messages on a development branch, raised with every change of
/// them there that an older development build cannot skip (a message it knows read
/// differently; a new message it skips is not one). Two development builds both say [`PROTOCOL_VERSION`] but may read
/// different messages: the revision in their app versions ([`app_version`]) lets the
/// PC turn a build of another revision away with a clear word instead of a session
/// that fails to parse. `None` in released builds, where the protocol version alone
/// decides.
pub const PROTO_REVISION: Option<u32> = Some(6);

/// The app version this build says in the handshake: `base` (the crate version), with
/// `-dev+pN` on a development branch (see [`PROTO_REVISION`]).
pub fn app_version(base: &str) -> String {
    match PROTO_REVISION {
        Some(n) => format!("{base}-dev+p{n}"),
        None => base.to_owned(),
    }
}

/// The message revision a peer's app version carries (`0.2.0-dev+p5` gives 5); `None`
/// for a released build.
pub fn revision_of(app: &str) -> Option<u32> {
    app.rsplit_once("+p")?.1.parse().ok()
}

/// Whether a peer that said `app` reads the messages this build sends. A released
/// build does not look; a development build needs the same revision.
pub fn same_revision(app: &str) -> bool {
    PROTO_REVISION.is_none_or(|ours| revision_of(app) == Some(ours))
}

/// Whether a peer that turned this build away (its protocol and app version) is the
/// newer one, so this side is the one to update: by the protocol, else by the revision
/// of two development builds.
pub fn peer_is_newer(protocol: u32, app: &str) -> bool {
    match protocol.cmp(&PROTOCOL_VERSION) {
        std::cmp::Ordering::Equal => revision_of(app) > PROTO_REVISION,
        other => other.is_gt(),
    }
}
/// PC sound sent to the headset: interleaved signed 16-bit little-endian stereo.
pub const AUDIO_RATE: u32 = 48_000;
pub const AUDIO_CHANNELS: u32 = 2;
/// Headset microphone sent to the PC (an up stream, [`ClientMsg::UpStart`]):
/// interleaved signed 16-bit little-endian mono.
pub const MIC_CHANNELS: u32 = 1;

/// Why the PC ends a session ([`HostMsg::Goodbye`]). A code a reader does not know
/// reads as [`PcGoodbye::Other`], so a new reason needs no new protocol version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PcGoodbye {
    Other = 0,
    /// The PC app is closing (from its tray, a signal, the PC shutting down).
    Quit = 1,
    /// The headset was not worn for a while: the desktop goes back to the laptop. It
    /// connects again once it is worn.
    TakenOff = 2,
    /// The PC forgot this headset ("Forget paired headsets"): it has to pair again.
    Forgotten = 3,
    /// A new connection of this headset took the place of this one.
    Replaced = 4,
    /// The monitors or encoders stopped working; an [`HostMsg::Error`] said why.
    Failed = 5,
}

impl PcGoodbye {
    pub fn code(self) -> u8 {
        self as u8
    }

    pub fn from_code(code: u8) -> Self {
        match code {
            1 => Self::Quit,
            2 => Self::TakenOff,
            3 => Self::Forgotten,
            4 => Self::Replaced,
            5 => Self::Failed,
            _ => Self::Other,
        }
    }
}

/// Why the headset ends a session ([`ClientMsg::Goodbye`]); unknown codes read as
/// [`HeadsetGoodbye::Other`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadsetGoodbye {
    Other = 0,
    /// The headset app is closing.
    Quit = 1,
    /// The user chose another PC.
    OtherPc = 2,
    /// The user forgot this PC in the headset.
    Forgotten = 3,
}

impl HeadsetGoodbye {
    pub fn code(self) -> u8 {
        self as u8
    }

    pub fn from_code(code: u8) -> Self {
        match code {
            1 => Self::Quit,
            2 => Self::OtherPc,
            3 => Self::Forgotten,
            _ => Self::Other,
        }
    }
}

/// Prefix of the UDP discovery beacon: `MAGIC ++ tcp_port (u16 LE) ++ hostname (utf-8)`.
pub const BEACON_MAGIC: &[u8; 8] = b"PROSTIR1";

/// Largest message from the PC (a video frame can be several megabytes).
const MAX_HOST_PAYLOAD: u32 = 64 * 1024 * 1024;
/// Largest message from the PC before the session is open ([`HostMsg::read_setup`]):
/// the PC is not trusted yet, and nothing it sends then is big.
const MAX_SETUP_PAYLOAD: u32 = 64 * 1024;
/// Largest message from the headset: the biggest real one is `Open` with the
/// headset's state (a few hundred bytes) or a slice of microphone sound (under 1 KiB).
/// A peer is not trusted yet when its first messages arrive, so this keeps what it can
/// make the PC allocate small.
const MAX_CLIENT_PAYLOAD: u32 = 64 * 1024;
/// Lanes that may hold a message being put together at once, in the session: one for
/// control, one for sound and one for each stream (the sender picks a free one in
/// `2..32` for each), with room to spare. Before the session one is enough.
const SESSION_LANES: usize = 32;
/// Longest name of a headset, in bytes.
const MAX_NAME: usize = 64;
/// Longest notice, in bytes (a desktop notification).
const MAX_TEXT: usize = 8 * 1024;

wire_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub enum Codec {
        0 => H264,
        1 => H265,
        /// On the wire only to a headset that decodes it (see `PROTOCOL.md`,
        /// "Capabilities"); no PC encodes it yet.
        2 => Av1,
    }
}

impl Codec {
    pub fn mime(self) -> &'static str {
        match self {
            Codec::H264 => "video/avc",
            Codec::H265 => "video/hevc",
            Codec::Av1 => "video/av01",
        }
    }
}

wire_enum! {
    message
    /// From the PC to the headset.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum HostMsg {
        /// The versions fit: the answer to [`ClientMsg::Hello`]. Frozen (see the crate
        /// root).
        1 => Welcome {
            /// The PC's [`PROTOCOL_VERSION`].
            version: u32,
            /// The PC's interface language (`uk`, `en`), which the headset follows by
            /// default.
            #[wire(rest)]
            language: String,
        },
        /// The session started: the answer to [`ClientMsg::Open`] from a headset the
        /// PC trusts, with every part of the PC's state (see [`state::PcState`]).
        28 => Snapshot {
            #[wire(max = MAX_GROWING)]
            parts: Vec<Part>,
        },
        /// Part: the PC's displays (see [`state::Displays`]).
        31 => Displays { part: Displays },
        /// Part: how the PC encodes.
        32 => Encoding { part: Encoding },
        /// Part: where the PC's sound goes, and its end of the microphone.
        33 => Sound { part: Sound },
        /// Part: what the PC allows.
        34 => Limits { part: state::Limits },
        /// Part: this headset's role and rights.
        35 => Access { part: Access },
        /// Part: about the PC.
        36 => PcInfo { part: PcInfo },
        /// Part: what the PC can do (in `Snapshot` only).
        37 => HostCaps { part: HostCaps },
        /// Part: the display the mouse is on.
        16 => Focus { part: Focus },
        /// Part: the display the sound comes from, while the headset asks for it.
        23 => SoundFrom { part: SoundFrom },
        /// Part, one for each kind: a program on the PC records from the headset's
        /// microphone (or camera) now, so the headset streams it.
        40 => UpWanted { part: UpWanted },
        /// The PC's volume slider or keys moved: the headset takes the level, unless it
        /// changed its own since the PC last saw (see [`volume`]).
        39 => VolumeAsk { asked: VolumeAsk },
        /// The answer to [`ClientMsg::Request`] `req`, after the parts it changed.
        29 => Reply { req: ReqId, outcome: Outcome },
        /// Something the PC (its tray, later `prostir ctl` and hotkeys) asks the
        /// headset to do, answered by one [`ClientMsg::Reply`]. Only to a headset that
        /// names the capability.
        30 => Command { req: ReqId, asked: Asked<Command> },
        /// Something went wrong that nobody asked for and that does not last (what
        /// lasts is in the state: a display `Failed`, the microphone `Failed`).
        38 => Problem {
            error: ErrorCode,
            #[wire(max = MAX_DETAIL)]
            detail: String,
            display: Option<DisplayId>,
        },
        /// The stream of display `id` starts, or starts over (a new generation): its
        /// size in pixels and codec. In the stream's own lane, after `Displays` lists
        /// the display.
        2 => StreamStart {
            id: DisplayId,
            generation: Gen,
            w: u32,
            h: u32,
            codec: Codec,
        },
        /// One encoded access unit (Annex-B byte stream, parameter sets in-band on
        /// keyframes), number `seq` of its generation, from 0.
        3 => Frame {
            id: DisplayId,
            generation: Gen,
            seq: u32,
            keyframe: bool,
            pts_us: u64,
            #[wire(rest)]
            data: Vec<u8>,
        },
        /// The stream of this generation stopped (never "the display is gone": only
        /// `Displays` says that).
        4 => StreamEnd { id: DisplayId, generation: Gen },
        /// Something failed before the session (pairing, opening it); shown in the
        /// headset menu. Frozen (see the crate root): it also tells headsets older than
        /// [`HostMsg::Incompatible`] what to update.
        6 => Error {
            #[wire(rest)]
            text: String,
        },
        /// A slice of PC sound (see [`AUDIO_RATE`]).
        12 => Audio {
            #[wire(rest)]
            pcm: Vec<u8>,
        },
        /// The headset speaks another protocol version: one side has to be updated.
        /// Sent instead of [`HostMsg::Welcome`], after an [`HostMsg::Error`] saying the
        /// same; the PC closes the connection after it. Frozen (see the crate root).
        17 => Incompatible {
            /// The PC's protocol version.
            protocol: u32,
            /// The PC app's version, e.g. `0.2.0`.
            #[wire(rest)]
            app: String,
        },
        /// A desktop notification of the PC, while the headset asks for them.
        20 => Notice {
            #[wire(max = MAX_TEXT)]
            text: String,
        },
        /// The PC's find secret, which the headset makes its discovery tags for this PC
        /// with (see [`discovery`]): right after the [`HostMsg::Snapshot`] of every
        /// session, so a headset always has the latest (the PC makes a new one when it
        /// forgets a headset). The headset keeps it with the PC's key.
        41 => FindSecret { secret: discovery::FindSecret },
        /// The PC does not know this headset yet: pair first (the answer to
        /// [`ClientMsg::Open`]).
        9 => PairingRequired,
        /// The PC's SPAKE2 message (see [`secure::Pairing`]).
        10 => PairMsg { msg: [u8; secure::PAIR_MSG_LEN] },
        /// Sent every second in the session: answer with [`ClientMsg::Pong`] at once.
        25 => Ping { seq: u32 },
        /// The answer to [`ClientMsg::Ping`] with its `seq`.
        26 => Pong { seq: u32 },
        /// The PC closes the session on purpose, for `reason` (a [`PcGoodbye`] code);
        /// the connection closes after it.
        27 => Goodbye { reason: u8 },
        /// Whether the codes matched: the PC's confirmation tag if they did.
        11 => PairResult { tag: Option<[u8; secure::CONFIRM_LEN]> },
    }
}

wire_enum! {
    message
    /// From the headset to the PC.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum ClientMsg {
        /// The first message of every connection. Frozen (see the crate root).
        101 => Hello {
            /// Newest protocol the headset speaks ([`PROTOCOL_VERSION`]).
            version: u32,
            /// Oldest protocol it still understands ([`MIN_PROTOCOL`]).
            min: u32,
            /// The headset app's version (e.g. `0.2.0`), to say what to update.
            #[wire(rest)]
            app: String,
        },
        /// Start the session, once the headset trusts the PC, with every part of the
        /// headset's state (see [`state::HeadsetState`]; `HeadsetCaps` always).
        /// Answered with [`HostMsg::Snapshot`], or [`HostMsg::PairingRequired`].
        127 => Open {
            #[wire(max = MAX_GROWING)]
            parts: Vec<Part>,
        },
        /// Part: what the headset can do (in `Open` only).
        138 => HeadsetCaps { part: HeadsetCaps },
        /// Part: the headset's settings the PC acts on.
        134 => Prefs { part: Prefs },
        /// Part: whether the headset is worn; streaming pauses while it is not.
        115 => Presence { part: Presence },
        /// Part: the headset's battery, for the PC's tray.
        119 => Battery { part: Battery },
        /// Part: the headset's volume (see [`volume`]): on its buttons, and in answer to
        /// [`HostMsg::VolumeAsk`].
        135 => Volume { part: Volume },
        /// Part, one for each kind: the headset's switch for its microphone (or camera),
        /// and whether the system lets it record.
        136 => UpState { part: UpState },
        /// The headset starts an up stream `up` (the microphone) of `kind`, in `format`.
        /// In the sound lane, so it never overtakes the stream's data.
        139 => UpStart { up: u8, kind: UpKind, format: UpFormat },
        /// A piece of up stream `up` (10 ms of the microphone).
        140 => Up {
            up: u8,
            #[wire(rest)]
            data: Vec<u8>,
        },
        /// Up stream `up` ended.
        141 => UpEnd { up: u8 },
        /// How the displays stand around the user, for the desktop's arrangement: rows
        /// from top to bottom (see [`state::Row`]); the PC lays its outputs out to match.
        /// At most [`MAX_DISPLAYS`] displays in all rows together, each once.
        133 => Arrange {
            #[wire(max = MAX_DISPLAYS)]
            rows: Vec<Row>,
        },
        /// The display the user looks at, and where on it (0..=65535 from the top left).
        137 => Gaze {
            display: Option<DisplayId>,
            u: u16,
            v: u16,
        },
        /// A change of the PC's state from the menu, answered by one [`HostMsg::Reply`].
        131 => Request { req: ReqId, asked: Asked<Request> },
        /// The answer to [`HostMsg::Command`] `req`, after the parts it changed.
        132 => Reply { req: ReqId, outcome: Outcome },
        /// Put the mouse on display `id`, at `u`/`v` across its image (0..=65535).
        124 => MouseTo { id: DisplayId, u: u16, v: u16 },
        /// The decoder lost frames of this generation: a keyframe, please.
        102 => RequestKeyframe { id: DisplayId, generation: Gen },
        /// The decoder handed frame `seq` of this generation to the compositor.
        103 => FrameShown { id: DisplayId, generation: Gen, seq: u32 },
        /// Start pairing. `headset_shows`: the headset displays the code and the PC
        /// asks for it; otherwise the PC displays it and the headset asks.
        109 => PairBegin {
            headset_shows: bool,
            #[wire(max = MAX_NAME)]
            name: String,
        },
        /// The headset's SPAKE2 message.
        110 => PairMsg { msg: [u8; secure::PAIR_MSG_LEN] },
        /// Proof that the headset derived the same pairing key.
        111 => PairConfirm { tag: [u8; secure::CONFIRM_LEN] },
        /// Sent every second in the session: answer with [`HostMsg::Pong`] at once.
        128 => Ping { seq: u32 },
        /// The answer to [`HostMsg::Ping`] with its `seq`.
        129 => Pong { seq: u32 },
        /// The headset closes the session on purpose, for `reason` (a
        /// [`HeadsetGoodbye`] code); the connection closes after it.
        130 => Goodbye { reason: u8 },
        /// Right after [`Presence`] `worn: false`: the headset went to sleep, or the
        /// system put the app away, so it does not come back by itself soon. The PC
        /// may end the session at once instead of waiting for it; the next `worn: true`
        /// undoes it. An older PC skips it and waits as for any headset taken off.
        142 => Asleep,
    }
}

/// Tags no message of this build has any more, never to be used again: messages of
/// earlier development builds of protocol 2 (never released), whose bytes another
/// development build could still send.
pub const RETIRED_HOST_TAGS: &[u8] = &[5, 7, 8, 13, 14, 15, 18, 19, 21, 22, 24];
pub const RETIRED_CLIENT_TAGS: &[u8] = &[
    104, 105, 106, 107, 108, 112, 113, 114, 116, 117, 118, 120, 121, 122, 123, 125, 126,
];

impl ClientMsg {
    /// Request `asked`, numbered `req`.
    pub fn request(req: ReqId, asked: Request) -> Self {
        Self::Request {
            req,
            asked: Asked::Known(asked),
        }
    }
}

impl HostMsg {
    /// Command `asked`, numbered `req`.
    pub fn command(req: ReqId, asked: Command) -> Self {
        Self::Command {
            req,
            asked: Asked::Known(asked),
        }
    }
}

/// Tags the code below needs by name (the tests check them against the
/// declarations): the media messages, read and written by hand, and the hello.
mod tag {
    pub const FRAME: u8 = 3;
    pub const AUDIO: u8 = 12;
    pub const UP: u8 = 140;
    pub const HELLO: u8 = 101;
}

/// [`HostMsg::Frame`] before its data: id, generation, seq, keyframe, pts_us.
const FRAME_HEAD: usize = 4 + 4 + 4 + 1 + 8;

/// The first payload bytes of a message from the PC kept apart from its data, so
/// the data of a frame is the body as it came (see [`packet::Assembler`]).
fn host_head(tag: u8) -> usize {
    if tag == tag::FRAME { FRAME_HEAD } else { 0 }
}

/// [`ClientMsg::Up`] keeps its stream's number apart from its data.
fn client_head(tag: u8) -> usize {
    usize::from(tag == tag::UP)
}

/// A message this side cannot send: a field over its limit, or too large as a whole.
/// The sender's mistake, so it ends this connection and nothing else.
fn unsendable(e: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, e)
}

/// The tag and payload length of the next message.
fn read_header(r: &mut impl Read, max: u32) -> io::Result<(u8, usize)> {
    let mut header = [0u8; 5];
    r.read_exact(&mut header)?;
    let [tag, len @ ..] = header;
    let len = u32::from_le_bytes(len);
    if len > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message too large",
        ));
    }
    Ok((tag, len as usize))
}

fn read_bytes(r: &mut impl Read, len: usize) -> io::Result<Vec<u8>> {
    let mut bytes = vec![0u8; len];
    r.read_exact(&mut bytes)?;
    Ok(bytes)
}

/// Reads past a payload without keeping it.
fn skip(r: &mut impl Read, len: usize) -> io::Result<()> {
    let skipped = io::copy(&mut r.by_ref().take(len as u64), &mut io::sink())?;
    if skipped < len as u64 {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    Ok(())
}

impl HostMsg {
    /// What a headset accepts from the PC: before the session (when the PC may not
    /// be trusted yet), or in it (`opened`, from the moment the headset sends `Open`
    /// to a PC it trusts, so the snapshot of the PC's state has no ceiling of its own).
    pub fn limits(opened: bool) -> Limits {
        match opened {
            false => Limits {
                message: MAX_SETUP_PAYLOAD,
                lanes: 1,
                in_flight: MAX_SETUP_PAYLOAD as usize,
            },
            // Room for the largest frame and another one beside it.
            true => Limits {
                message: MAX_HOST_PAYLOAD,
                lanes: SESSION_LANES,
                in_flight: 2 * MAX_HOST_PAYLOAD as usize,
            },
        }
    }

    /// Puts messages from the PC together, with the limits from before the session.
    pub fn assembler() -> Assembler {
        Assembler::new(Self::limits(false), host_head)
    }

    /// The message's bytes, to send in packets. Frame data and sound move into them
    /// as they are.
    pub fn encode(self) -> io::Result<Encoded> {
        match self {
            HostMsg::Frame {
                id,
                generation,
                seq,
                keyframe,
                pts_us,
                data,
            } => {
                let mut head = Vec::with_capacity(FRAME_HEAD);
                id.put(&mut head)
                    .and_then(|()| generation.put(&mut head))
                    .and_then(|()| seq.put(&mut head))
                    .and_then(|()| keyframe.put(&mut head))
                    .and_then(|()| pts_us.put(&mut head))
                    .map_err(unsendable)?;
                Encoded::new(tag::FRAME, &head, data)
            }
            HostMsg::Audio { pcm } => Encoded::new(tag::AUDIO, &[], pcm),
            other => {
                let mut fields = Vec::new();
                other.put_fields(&mut fields).map_err(unsendable)?;
                Encoded::new(other.wire_tag(), &[], fields)
            }
        }
    }

    /// The message put together in `m`; `None` for a tag this build does not know
    /// (skipped, so a newer PC can add messages).
    pub fn from_message(m: Message) -> io::Result<Option<Self>> {
        Ok(Some(match m.tag {
            tag::FRAME => {
                let mut h = Reader::new(m.head());
                let (id, generation, seq) = (
                    DisplayId::get(&mut h)?,
                    Gen::get(&mut h)?,
                    u32::get(&mut h)?,
                );
                let (keyframe, pts_us) = (bool::get(&mut h)?, u64::get(&mut h)?);
                h.finish()?;
                HostMsg::Frame {
                    id,
                    generation,
                    seq,
                    keyframe,
                    pts_us,
                    data: m.body,
                }
            }
            tag::AUDIO => HostMsg::Audio { pcm: m.body },
            tag if Self::TAGS.contains(&tag) => Self::decode(tag, &m.body)?,
            _ => return Ok(None),
        }))
    }

    /// The next message from the PC this build knows: records from `rx`, put
    /// together by `packets`.
    pub fn recv(rx: &mut dyn RecordReceiver, packets: &mut Assembler) -> io::Result<Self> {
        loop {
            if let Some(m) = packets.push(rx.recv()?)?
                && let Some(msg) = Self::from_message(m)?
            {
                return Ok(msg);
            }
        }
    }

    /// Writes the message as one stream, the frozen handshake's format: the same bytes
    /// its packets carry.
    pub fn write(&self, w: &mut impl Write) -> io::Result<()> {
        self.clone().encode()?.write_to(w)
    }

    /// The next message from the PC read as one stream, during the version handshake:
    /// the PC may not be trusted yet, so a message over 64 KiB is an error. Messages
    /// from a newer PC are skipped.
    pub fn read_setup(r: &mut impl Read) -> io::Result<Self> {
        Self::read_within(r, MAX_SETUP_PAYLOAD)
    }

    fn read_within(r: &mut impl Read, max: u32) -> io::Result<Self> {
        loop {
            let (tag, len) = read_header(r, max)?;
            if !Self::TAGS.contains(&tag) {
                skip(r, len)?;
                continue;
            }
            let m = Message::from_payload(tag, read_bytes(r, len)?, host_head(tag));
            if let Some(msg) = Self::from_message(m)? {
                return Ok(msg);
            }
        }
    }

    /// The message with `tag` whose fields are `payload`, checked (see
    /// [`HostMsg::check`]).
    pub(crate) fn decode(tag: u8, payload: &[u8]) -> Result<Self, WireError> {
        let mut r = Reader::new(payload);
        let msg = Self::get_fields(tag, &mut r)?.ok_or(WireError::UnknownTag {
            what: "HostMsg",
            tag,
        })?;
        r.finish()?;
        msg.check()?;
        Ok(msg)
    }

    /// Values the format allows but the protocol does not: a bug of the PC, which
    /// ends the session like any message that does not parse.
    fn check(&self) -> Result<(), WireError> {
        match self {
            Self::Displays { part } => part.check(),
            Self::Limits { part } => part.check(),
            Self::VolumeAsk { asked } => asked.check(),
            _ => Ok(()),
        }
    }

    /// The part this message carries, as a part of `Snapshot`; `None` for a message
    /// that is not a part of the PC's state.
    pub fn to_part(&self) -> Option<Result<Part, WireError>> {
        state::PcState::is_part(self.wire_tag()).then(|| {
            let mut body = Vec::new();
            self.put_fields(&mut body)?;
            Ok(Part {
                tag: self.wire_tag(),
                body,
            })
        })
    }
}

impl ClientMsg {
    /// What the PC accepts from a headset: before the session, or in it (`opened`).
    pub fn limits(opened: bool) -> Limits {
        let lanes = if opened { SESSION_LANES } else { 1 };
        Limits {
            message: MAX_CLIENT_PAYLOAD,
            lanes,
            in_flight: lanes * MAX_CLIENT_PAYLOAD as usize,
        }
    }

    /// Puts messages from a headset together, with the limits from before the session.
    pub fn assembler() -> Assembler {
        Assembler::new(Self::limits(false), client_head)
    }

    /// The message's bytes, to send in packets. The data of an up stream moves into
    /// them as it is.
    pub fn encode(self) -> io::Result<Encoded> {
        match self {
            ClientMsg::Up { up, data } => Encoded::new(tag::UP, &[up], data),
            other => {
                let mut fields = Vec::new();
                other.put_fields(&mut fields).map_err(unsendable)?;
                Encoded::new(other.wire_tag(), &[], fields)
            }
        }
    }

    /// The message put together in `m`; `None` for a tag this build does not know.
    pub fn from_message(m: Message) -> io::Result<Option<Self>> {
        Ok(Some(match m.tag {
            tag::UP => {
                let mut h = Reader::new(m.head());
                let up = u8::get(&mut h)?;
                h.finish()?;
                ClientMsg::Up { up, data: m.body }
            }
            tag if Self::TAGS.contains(&tag) => Self::decode(tag, &m.body)?,
            _ => return Ok(None),
        }))
    }

    /// The next message from the headset this build knows.
    pub fn recv(rx: &mut dyn RecordReceiver, packets: &mut Assembler) -> io::Result<Self> {
        loop {
            if let Some(m) = packets.push(rx.recv()?)?
                && let Some(msg) = Self::from_message(m)?
            {
                return Ok(msg);
            }
        }
    }

    /// Writes the message as one stream, the frozen handshake's format: the same bytes
    /// its packets carry.
    pub fn write(&self, w: &mut impl Write) -> io::Result<()> {
        self.clone().encode()?.write_to(w)
    }

    /// The first message of a connection, which every headset since protocol 2 makes
    /// [`ClientMsg::Hello`]. Any other gives `None`, skipped unread: a headset from
    /// before protocol 2 may ask to pair before saying hello, in messages this build
    /// cannot read.
    pub fn read_first(r: &mut impl Read) -> io::Result<Option<Self>> {
        let (tag, len) = read_header(r, MAX_CLIENT_PAYLOAD)?;
        if tag != tag::HELLO {
            skip(r, len)?;
            return Ok(None);
        }
        Ok(Some(Self::decode(tag, &read_bytes(r, len)?)?))
    }

    /// Any message from a headset read as one stream, as packets would carry it: for
    /// the tests, which compare the two formats.
    #[cfg(test)]
    fn read_within(r: &mut impl Read, max: u32) -> io::Result<Self> {
        loop {
            let (tag, len) = read_header(r, max)?;
            if !Self::TAGS.contains(&tag) {
                skip(r, len)?;
                continue;
            }
            let m = Message::from_payload(tag, read_bytes(r, len)?, client_head(tag));
            if let Some(msg) = Self::from_message(m)? {
                return Ok(msg);
            }
        }
    }

    /// The message with `tag` whose fields are `payload`, checked (see
    /// [`ClientMsg::check`]).
    pub(crate) fn decode(tag: u8, payload: &[u8]) -> Result<Self, WireError> {
        let mut r = Reader::new(payload);
        let msg = Self::get_fields(tag, &mut r)?.ok_or(WireError::UnknownTag {
            what: "ClientMsg",
            tag,
        })?;
        r.finish()?;
        msg.check()?;
        Ok(msg)
    }

    /// Values the format allows but the protocol does not: a bug of the headset,
    /// which ends the session like any message that does not parse. Bad arguments
    /// of a request are not this: they get a failed reply.
    fn check(&self) -> Result<(), WireError> {
        match self {
            // Each display once, so the order the PC makes from it fits in a message.
            Self::Arrange { rows } => state::check_rows(rows),
            Self::HeadsetCaps { part } => part.check(),
            Self::Volume { part } => part.check(),
            Self::UpStart { format, .. } => format.check(),
            _ => Ok(()),
        }
    }

    /// The part this message carries, as a part of `Open`; `None` for a message that
    /// is not a part of the headset's state.
    pub fn to_part(&self) -> Option<Result<Part, WireError>> {
        state::HeadsetState::is_part(self.wire_tag()).then(|| {
            let mut body = Vec::new();
            self.put_fields(&mut body)?;
            Ok(Part {
                tag: self.wire_tag(),
                body,
            })
        })
    }
}

/// Builds a discovery beacon payload.
pub fn encode_beacon(tcp_port: u16, hostname: &str) -> Vec<u8> {
    let mut v = BEACON_MAGIC.to_vec();
    v.extend_from_slice(&tcp_port.to_le_bytes());
    v.extend_from_slice(hostname.as_bytes());
    v
}

/// Parses a discovery beacon, returning `(tcp_port, hostname)`.
pub fn decode_beacon(buf: &[u8]) -> Option<(u16, String)> {
    let rest = buf.strip_prefix(BEACON_MAGIC.as_slice())?;
    let port = u16::from_le_bytes(rest.get(..2)?.try_into().ok()?);
    Some((port, String::from_utf8_lossy(&rest[2..]).into_owned()))
}

#[cfg(test)]
mod tests;
