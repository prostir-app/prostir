//! Packets: how messages travel in the records of a [`crate::link`].
//!
//! One record is one packet. A message (`tag u8, length u32 LE, payload`, as in the
//! crate root) is cut into packets that follow one another in one **lane**:
//!
//! ```text
//! +----------+---------+------------------------------------+
//! | flags u8 | lane u8 | body: the next bytes of the message |
//! +----------+---------+------------------------------------+
//! flags: bit 0 FIRST, bit 1 LAST (a whole message: both), the other bits 0
//! ```
//!
//! * The first packet's body starts with the message's 5-byte header, whole, so the
//!   receiver knows the length at once and checks it before allocating anything.
//!   The bodies joined are exactly the message's bytes.
//! * A lane is a place where one message is put back together, not a kind of
//!   message: the tag says what a message is. A lane holds at most one message at a
//!   time, so messages of one lane arrive in order; between lanes, in the order of
//!   their last packets. The sender picks lanes and how big each packet is.
//! * The transport is reliable, so a packet out of place is a bug or an attack, never
//!   a loss: every [`PacketError`] ends the connection.

use std::fmt;
use std::io;

use crate::link::{MAX_RECORD, RecordSender};

/// Bytes before a packet's body: flags and lane.
pub const HEADER: usize = 2;
/// The most body bytes in one packet.
pub const MAX_BODY: usize = MAX_RECORD - HEADER;
/// Body bytes per packet in the current apps: 1.3 ms of a 100 Mbit/s link, so
/// anything more urgent waits at most that long behind a big frame.
pub const FRAGMENT: usize = 16 * 1024;
/// A message's header: its tag and payload length.
pub const MESSAGE_HEADER: usize = 5;
/// The most bytes of a message kept apart from its body (see [`Encoded`]).
pub const MAX_HEAD: usize = 32;

const FIRST: u8 = 1;
const LAST: u8 = 2;

/// Lanes of the current apps.
pub mod lane {
    /// Control messages, in order.
    pub const CONTROL: u8 = 0;
    /// PC sound, or the headset's microphone.
    pub const SOUND: u8 = 1;
    /// The first lane of video.
    pub const VIDEO: u8 = 2;
    /// The lanes of video streams: the sender takes a free one for each stream, from
    /// its start to its end.
    pub const STREAMS: std::ops::Range<u8> = VIDEO..32;
}

/// What a receiver accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The largest payload of a message.
    pub message: u32,
    /// How many lanes may hold a message being put together at once.
    pub lanes: usize,
    /// How many bytes all the messages being put together may say they hold, together:
    /// what a peer can make the receiver set aside with a few small first packets.
    pub in_flight: usize,
}

/// Why packets cannot be put together into messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PacketError {
    /// A record too short for the packet header.
    NoHeader,
    /// Flag bits that mean nothing.
    Flags(u8),
    /// A packet without a body.
    Empty { lane: u8 },
    /// A packet that is not first, in a lane holding no message.
    NotStarted { lane: u8 },
    /// A first packet in a lane that holds a message already.
    Busy { lane: u8 },
    /// A first packet without the whole message header.
    ShortFirst { lane: u8 },
    /// A message longer than the receiver accepts.
    TooLarge { len: u32, max: u32 },
    /// More bytes than the message header said.
    Overrun { lane: u8 },
    /// The last packet before the message's end.
    EndedEarly { lane: u8 },
    /// The message's bytes are all there, but its last packet does not say so.
    NotLast { lane: u8 },
    /// More lanes at once than the receiver accepts.
    TooManyLanes { max: usize },
    /// The messages being put together would hold more than the receiver accepts.
    TooMuchAtOnce { max: usize },
}

impl fmt::Display for PacketError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoHeader => write!(f, "a record too short for a packet"),
            Self::Flags(flags) => write!(f, "unknown packet flags {flags:#04x}"),
            Self::Empty { lane } => write!(f, "an empty packet in lane {lane}"),
            Self::NotStarted { lane } => {
                write!(f, "a packet in lane {lane} without its first one")
            }
            Self::Busy { lane } => write!(f, "a new message in lane {lane} before the last ended"),
            Self::ShortFirst { lane } => {
                write!(
                    f,
                    "a first packet in lane {lane} without the message header"
                )
            }
            Self::TooLarge { len, max } => {
                write!(f, "a message of {len} bytes where at most {max} fit")
            }
            Self::Overrun { lane } => write!(f, "a message in lane {lane} longer than it said"),
            Self::EndedEarly { lane } => {
                write!(f, "a message in lane {lane} shorter than it said")
            }
            Self::NotLast { lane } => {
                write!(
                    f,
                    "a message in lane {lane} complete without its last packet"
                )
            }
            Self::TooManyLanes { max } => write!(f, "more than {max} lanes at once"),
            Self::TooMuchAtOnce { max } => {
                write!(
                    f,
                    "messages of more than {max} bytes being put together at once"
                )
            }
        }
    }
}

impl std::error::Error for PacketError {}

impl From<PacketError> for io::Error {
    fn from(e: PacketError) -> Self {
        io::Error::new(io::ErrorKind::InvalidData, e)
    }
}

/// A message ready to be sent: its header and the fields before its data (`head`),
/// then the rest (`body`). Frame data and sound are moved into the body as they are,
/// so they are copied only once, into the record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encoded {
    head: [u8; MAX_HEAD],
    head_len: u8,
    body: Vec<u8>,
}

impl Encoded {
    /// The message with `tag` whose payload is `fields` then `body`. Fails for a
    /// payload a length cannot say.
    pub fn new(tag: u8, fields: &[u8], body: Vec<u8>) -> io::Result<Self> {
        assert!(
            fields.len() <= MAX_HEAD - MESSAGE_HEADER,
            "fields before the body fit in the head"
        );
        let len = u32::try_from(fields.len() + body.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "message too large"))?;
        let mut head = [0u8; MAX_HEAD];
        head[0] = tag;
        head[1..MESSAGE_HEADER].copy_from_slice(&len.to_le_bytes());
        head[MESSAGE_HEADER..MESSAGE_HEADER + fields.len()].copy_from_slice(fields);
        Ok(Self {
            head,
            head_len: (MESSAGE_HEADER + fields.len()) as u8,
            body,
        })
    }

    pub fn tag(&self) -> u8 {
        self.head[0]
    }

    /// All its bytes, header included.
    pub fn len(&self) -> usize {
        usize::from(self.head_len) + self.body.len()
    }

    /// Never: a message has at least its header.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Writes all its bytes, as one stream (the frozen version handshake).
    pub fn write_to(&self, w: &mut impl io::Write) -> io::Result<()> {
        w.write_all(&self.head[..usize::from(self.head_len)])?;
        w.write_all(&self.body)
    }

    /// Its body back, once it is sent, to be filled again (see [`Outgoing::into_message`]).
    pub fn into_body(self) -> Vec<u8> {
        self.body
    }

    /// Its bytes from `start`, `n` of them, as two slices.
    fn range(&self, start: usize, n: usize) -> [&[u8]; 2] {
        let head = &self.head[..usize::from(self.head_len)];
        let end = start + n;
        let in_head = &head[start.min(head.len())..end.min(head.len())];
        let body_start = start.saturating_sub(head.len());
        let body_end = end.saturating_sub(head.len());
        [in_head, &self.body[body_start..body_end]]
    }
}

/// A message on its way out in packets of one lane.
#[derive(Debug)]
pub struct Outgoing {
    lane: u8,
    msg: Encoded,
    sent: usize,
}

impl Outgoing {
    pub fn new(lane: u8, msg: Encoded) -> Self {
        Self { lane, msg, sent: 0 }
    }

    pub fn lane(&self) -> u8 {
        self.lane
    }

    /// The message, once sent (or not): its body can be filled again, so a stream of
    /// messages of one size (the microphone's slices) needs no new memory for each.
    pub fn into_message(self) -> Encoded {
        self.msg
    }

    /// Sends its next packet, of at most `fragment` body bytes (but never so few that
    /// the first one lacks the message header). `true` when that was the last one.
    pub fn send_next(&mut self, tx: &mut dyn RecordSender, fragment: usize) -> io::Result<bool> {
        let fragment = fragment.clamp(MAX_HEAD, MAX_BODY);
        let len = self.msg.len();
        let n = fragment.min(len - self.sent);
        let mut flags = 0;
        if self.sent == 0 {
            flags |= FIRST;
        }
        if self.sent + n == len {
            flags |= LAST;
        }
        let [head, body] = self.msg.range(self.sent, n);
        tx.send(&[&[flags, self.lane], head, body])?;
        self.sent += n;
        Ok(flags & LAST != 0)
    }
}

/// Sends `msg` whole in `lane`, packet after packet.
pub fn send_whole(
    tx: &mut dyn RecordSender,
    lane: u8,
    msg: Encoded,
    fragment: usize,
) -> io::Result<()> {
    let mut out = Outgoing::new(lane, msg);
    while !out.send_next(tx, fragment)? {}
    Ok(())
}

/// A message put back together: its tag, the first bytes of its payload that the
/// receiver keeps apart (the fields before frame data, say), and the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub tag: u8,
    head: [u8; MAX_HEAD],
    head_len: u8,
    pub body: Vec<u8>,
}

impl Message {
    /// The message with `tag` and `payload`, its first `head` bytes kept apart, as an
    /// [`Assembler`] would make it.
    pub(crate) fn from_payload(tag: u8, mut payload: Vec<u8>, head: usize) -> Self {
        let head_len = head.min(MAX_HEAD - MESSAGE_HEADER).min(payload.len());
        let mut msg = Message {
            tag,
            head: [0; MAX_HEAD],
            head_len: head_len as u8,
            body: Vec::new(),
        };
        msg.head[..head_len].copy_from_slice(&payload[..head_len]);
        payload.drain(..head_len);
        msg.body = payload;
        msg
    }

    /// The payload's first bytes, as many as the assembler was told to keep apart for
    /// this tag (fewer if the payload is shorter).
    pub fn head(&self) -> &[u8] {
        &self.head[..usize::from(self.head_len)]
    }

    /// The whole payload in one piece.
    pub fn payload(&self) -> Vec<u8> {
        [self.head(), &self.body].concat()
    }
}

/// A message being put together in a lane.
struct Partial {
    msg: Message,
    /// The payload's length, and how much of it came.
    len: usize,
    got: usize,
    /// How many of its first bytes go into the head.
    head_want: usize,
}

impl Partial {
    fn take(&mut self, mut bytes: &[u8]) {
        let into_head = (self.head_want - usize::from(self.msg.head_len)).min(bytes.len());
        let at = usize::from(self.msg.head_len);
        self.msg.head[at..at + into_head].copy_from_slice(&bytes[..into_head]);
        self.msg.head_len += into_head as u8;
        bytes = &bytes[into_head..];
        self.msg.body.extend_from_slice(bytes);
        self.got += into_head + bytes.len();
    }
}

/// Puts packets back together into messages.
pub struct Assembler {
    lanes: Vec<Option<Partial>>,
    open: usize,
    /// The bytes the messages being put together say they hold.
    open_bytes: usize,
    limits: Limits,
    /// For a tag, how many first bytes of the payload to keep apart from the body.
    head: fn(u8) -> usize,
    /// Bodies handed back once their messages were used (see [`Assembler::recycle`]),
    /// taken for the next messages that fit in them.
    spare: Vec<Vec<u8>>,
}

/// How many bodies handed back an assembler keeps at most.
const SPARE_BODIES: usize = 8;

impl Assembler {
    /// Accepts what `limits` allows; `head` says for each tag how many first bytes of
    /// the payload to keep apart from the body (at most `MAX_HEAD - 5`), so that the
    /// body is the message's data as it is.
    pub fn new(limits: Limits, head: fn(u8) -> usize) -> Self {
        Self {
            lanes: (0..=u8::MAX).map(|_| None).collect(),
            open: 0,
            open_bytes: 0,
            limits,
            head,
            spare: Vec::new(),
        }
    }

    /// Hands back the body of a message used up (a slice of sound played): a next
    /// message it is large enough for is put together in it, so a steady stream of
    /// messages of one size needs no new memory for each. Kept only while there are
    /// few, and only a body no larger than the largest message allowed.
    pub fn recycle(&mut self, mut body: Vec<u8>) {
        if self.spare.len() < SPARE_BODIES && body.capacity() <= self.limits.message as usize {
            body.clear();
            self.spare.push(body);
        }
    }

    /// A body for `len` bytes: a spare one it fits in, else a new one of that size.
    fn body_for(&mut self, len: usize) -> Vec<u8> {
        match self.spare.iter().position(|b| b.capacity() >= len) {
            Some(i) => self.spare.swap_remove(i),
            None => Vec::with_capacity(len),
        }
    }

    /// Accepts what `limits` allows from now on (larger messages once the session
    /// is open, say).
    pub fn set_limits(&mut self, limits: Limits) {
        self.limits = limits;
    }

    /// How many lanes hold a message being put together.
    pub fn assembling(&self) -> usize {
        self.open
    }

    /// Takes one record; the message it completes, if any.
    pub fn push(&mut self, record: &[u8]) -> Result<Option<Message>, PacketError> {
        let [flags, lane, ref body @ ..] = *record else {
            return Err(PacketError::NoHeader);
        };
        if flags & !(FIRST | LAST) != 0 {
            return Err(PacketError::Flags(flags));
        }
        if body.is_empty() {
            return Err(PacketError::Empty { lane });
        }
        let last = flags & LAST != 0;
        let slot = &mut self.lanes[usize::from(lane)];
        let partial = if flags & FIRST != 0 {
            if slot.is_some() {
                return Err(PacketError::Busy { lane });
            }
            let Some((header, rest)) = body.split_first_chunk::<MESSAGE_HEADER>() else {
                return Err(PacketError::ShortFirst { lane });
            };
            let [tag, len @ ..] = *header;
            let len = u32::from_le_bytes(len);
            if len > self.limits.message {
                return Err(PacketError::TooLarge {
                    len,
                    max: self.limits.message,
                });
            }
            let len = len as usize;
            // Checked before anything is allocated.
            match (last, rest.len().cmp(&len)) {
                (_, std::cmp::Ordering::Greater) => return Err(PacketError::Overrun { lane }),
                (true, std::cmp::Ordering::Less) => {
                    return Err(PacketError::EndedEarly { lane });
                }
                (false, std::cmp::Ordering::Equal) => return Err(PacketError::NotLast { lane }),
                _ => {}
            }
            if !last && self.open >= self.limits.lanes {
                return Err(PacketError::TooManyLanes {
                    max: self.limits.lanes,
                });
            }
            if !last && self.open_bytes + len > self.limits.in_flight {
                return Err(PacketError::TooMuchAtOnce {
                    max: self.limits.in_flight,
                });
            }
            let head_want = (self.head)(tag).min(MAX_HEAD - MESSAGE_HEADER).min(len);
            // Once, at the length the header says (checked against the limit), or a
            // body handed back that it fits in.
            let body = self.body_for(len - head_want);
            let mut partial = Partial {
                msg: Message {
                    tag,
                    head: [0; MAX_HEAD],
                    head_len: 0,
                    body,
                },
                len,
                got: 0,
                head_want,
            };
            partial.take(rest);
            partial
        } else {
            let Some(mut partial) = slot.take() else {
                return Err(PacketError::NotStarted { lane });
            };
            self.open -= 1;
            self.open_bytes -= partial.len;
            if partial.got + body.len() > partial.len {
                return Err(PacketError::Overrun { lane });
            }
            partial.take(body);
            partial
        };
        match (last, partial.got == partial.len) {
            (true, true) => Ok(Some(partial.msg)),
            (true, false) => Err(PacketError::EndedEarly { lane }),
            (false, true) => Err(PacketError::NotLast { lane }),
            (false, false) => {
                self.open_bytes += partial.len;
                self.lanes[usize::from(lane)] = Some(partial);
                self.open += 1;
                Ok(None)
            }
        }
    }
}

#[cfg(test)]
mod tests;
