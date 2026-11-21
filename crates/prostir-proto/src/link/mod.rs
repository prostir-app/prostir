//! The transport: a reliable, ordered pipe of **records** (pieces of bytes up to
//! [`MAX_RECORD`]) to one peer, already encrypted and authenticated. It knows nothing
//! of messages, packets or priorities.
//!
//! * [`tcp::NoiseTcp`]: one TCP connection (Wi-Fi, or USB through `adb reverse`) with a
//!   Noise `XX` handshake.
//! * [`memory`]: a pair of linked ends in one process, for tests.
//!
//! Everything blocks (no async runtime): each half lives on a thread of its own.
//! Accepting connections stays outside: a transport is built from a connection that
//! is already there.

pub mod memory;
pub mod tcp;

use std::io::{self, Read, Write};
use std::sync::Arc;
use std::time::Duration;

/// Most bytes in one record: a Noise message (65535 bytes) less its 16-byte tag.
pub const MAX_RECORD: usize = 65535 - 16;

/// The sending half of a link.
pub trait RecordSender: Send {
    /// Sends one record made of `parts`, joined (at most [`MAX_RECORD`] bytes in all).
    /// Waits until the network takes it; an error (a write blocked for 5 s, say)
    /// means the connection is over.
    fn send(&mut self, parts: &[&[u8]]) -> io::Result<()>;

    /// Sends no more: the peer reads the end of the connection after the last record,
    /// while this side still receives (so it can read what the peer sends until it
    /// closes too, and nothing sent is lost to a reset).
    fn finish(&mut self) -> io::Result<()>;

    /// How many bytes a second to give the network at most (0: no limit), so a big
    /// burst does not fill the queue of a Wi-Fi access point at once.
    fn set_pace(&mut self, bytes_per_sec: u64);

    /// What the transport last measured about the network (TCP's delivery rate and
    /// round trip); `None` where it cannot tell.
    fn link_info(&mut self) -> Option<LinkInfo>;
}

/// What TCP measured about the connection, as last seen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LinkInfo {
    /// How fast the network delivered what this side sent, bytes a second (its latest
    /// sample).
    pub delivery_rate: u64,
    /// The round trip, smoothed, and how much it varies.
    pub rtt: Duration,
    pub rtt_var: Duration,
    /// The shortest round trip seen on this connection.
    pub min_rtt: Duration,
    /// Segments TCP sent again on this connection so far (lost on the way, or their
    /// acknowledgement was), and bytes written but not sent yet.
    pub retransmits: u32,
    pub unsent: u32,
}

/// The receiving half of a link.
pub trait RecordReceiver: Send {
    /// The next record, valid until the next call. The peer closing the connection
    /// between records gives [`io::ErrorKind::UnexpectedEof`]; nothing at all for
    /// longer than the idle limit gives [`io::ErrorKind::TimedOut`].
    fn recv(&mut self) -> io::Result<&[u8]>;

    /// How long the peer may stay silent (`None`: for ever).
    fn set_idle_limit(&mut self, idle: Option<Duration>);
}

/// Closes the connection for both halves, waking calls blocked in either of them.
/// Whoever notices first that the session is over (a read failing, silence, a
/// goodbye) calls it.
#[derive(Clone)]
pub struct Closer(Arc<dyn Fn() + Send + Sync>);

impl Closer {
    pub fn new(close: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Arc::new(close))
    }

    pub fn close(&self) {
        (self.0)();
    }
}

/// How the peer is reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    /// A USB cable (`adb reverse` to the PC's loopback).
    Usb,
    /// Wi-Fi or Ethernet.
    Network,
}

/// What the handshake established about the other side.
#[derive(Debug, Clone)]
pub struct PeerInfo {
    /// The peer's long-term key.
    pub key: [u8; 32],
    /// Unique to this connection (the Noise handshake hash): pairing confirmations
    /// are bound to it, so they cannot be relayed into another connection.
    pub binding: Vec<u8>,
    pub via: Via,
}

/// An open connection to one peer.
pub struct Link {
    pub sender: Box<dyn RecordSender>,
    pub receiver: Box<dyn RecordReceiver>,
    pub closer: Closer,
    pub peer: PeerInfo,
}

/// Reads records as one byte stream, the way protocol 1 framed everything. Only the
/// frozen version handshake still is (see [`crate::handshake`]): its messages may run
/// across record boundaries, and an empty record carries nothing.
pub struct StreamReader<'a> {
    receiver: &'a mut dyn RecordReceiver,
    record: Vec<u8>,
    pos: usize,
}

impl<'a> StreamReader<'a> {
    pub fn new(receiver: &'a mut dyn RecordReceiver) -> Self {
        Self {
            receiver,
            record: Vec::new(),
            pos: 0,
        }
    }

    /// Everything read so far ended with a record: nothing of the current one is
    /// left over.
    pub fn at_record_end(&self) -> bool {
        self.pos == self.record.len()
    }
}

/// Writes a byte stream as records, one per write of at most [`MAX_RECORD`] bytes (the
/// frozen version handshake, see [`crate::handshake`]).
pub struct StreamWriter<'a>(pub &'a mut dyn RecordSender);

impl Write for StreamWriter<'_> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let chunk = &data[..data.len().min(MAX_RECORD)];
        self.0.send(&[chunk])?;
        Ok(chunk.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Read for StreamReader<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.at_record_end() {
            match self.receiver.recv() {
                Ok(record) => {
                    self.record.clear();
                    self.record.extend_from_slice(record);
                    self.pos = 0;
                }
                // A connection that closed between records is the end of the stream.
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(0),
                Err(e) => return Err(e),
            }
        }
        let n = out.len().min(self.record.len() - self.pos);
        out[..n].copy_from_slice(&self.record[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stream runs across records, skips empty ones and knows where a record ends.
    #[test]
    fn a_stream_runs_across_records() {
        let (mut a, mut b) = memory::pair(8);
        a.sender.send(&[b"hel", b"lo"]).unwrap();
        a.sender.send(&[]).unwrap();
        a.sender.send(&[b" world"]).unwrap();
        a.closer.close();
        let mut stream = StreamReader::new(&mut *b.receiver);
        let mut hello = [0u8; 5];
        stream.read_exact(&mut hello).unwrap();
        assert_eq!(&hello, b"hello");
        assert!(stream.at_record_end());
        let mut space = [0u8; 1];
        stream.read_exact(&mut space).unwrap();
        assert!(!stream.at_record_end());
        let mut rest = Vec::new();
        stream.read_to_end(&mut rest).unwrap();
        assert_eq!(rest, b"world");
    }
}
