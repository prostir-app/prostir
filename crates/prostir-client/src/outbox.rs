//! Messages from the headset to the PC, most important first. Sending only queues
//! them: one writer thread puts them on the network packet by packet, so the frame
//! loop, the microphone and the decoders never wait on Wi-Fi.
//!
//! Two classes, each in a lane of its own:
//! - control (requests, settings, the layout, pairing): always sent, in order;
//! - up streams (the microphone) and the "frame shown" acknowledgements: sent after
//!   control, and their data and acknowledgements dropped while the network is stuck
//!   (sound would be late anyway, the acknowledgements are for latency figures only);
//!   the start and end of an up stream go in the same lane, in order, and are never
//!   dropped.

#![expect(
    clippy::disallowed_methods,
    reason = "the queue to the PC belongs to the network thread, part of the shell: it keeps the clock of its pings"
)]

use std::collections::VecDeque;
use std::io;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use prostir_proto::link::RecordSender;
use prostir_proto::packet::{Encoded, FRAGMENT, Outgoing, lane};
use prostir_proto::stats::{Histogram, Pings};
use prostir_proto::{ClientMsg, HeadsetGoodbye};

/// About 200 ms of microphone sound: more than that waiting means the network is
/// stuck, and sound that late is no use (the PC would play it that much behind).
pub const MAX_LOSSY: usize = 20;
/// In a session the headset pings the PC this often: for the round trip, and so the
/// connection never falls silent while it is fine.
pub const PING_EVERY: Duration = Duration::from_secs(1);

/// Messages that go in the sound lane.
fn in_sound_lane(msg: &ClientMsg) -> bool {
    matches!(
        msg,
        ClientMsg::Up { .. }
            | ClientMsg::UpStart { .. }
            | ClientMsg::UpEnd { .. }
            | ClientMsg::FrameShown { .. }
    )
}

/// Messages it costs little to lose when the network is stuck.
fn lossy(msg: &ClientMsg) -> bool {
    matches!(msg, ClientMsg::Up { .. } | ClientMsg::FrameShown { .. })
}

#[derive(Default)]
struct State {
    control: VecDeque<Encoded>,
    /// The sound lane's messages, each with whether it may be dropped.
    lossy: VecDeque<(Encoded, bool)>,
    pings: Pings,
    /// When the next ping is due; `None` until the session is open.
    next_ping: Option<Instant>,
    round_trip: Histogram,
    /// The most lossy messages waiting at once since the last look.
    lossy_max: usize,
    /// A goodbye is queued: control messages go out up to it, nothing else does.
    leaving: bool,
    closed: bool,
    /// Bodies of microphone slices sent, to be filled again (see
    /// [`Outbox::up_buffer`]).
    spare: Vec<Vec<u8>>,
}

/// How many bodies of microphone slices sent are kept to be filled again: as many as
/// may wait, and a few more on their way.
const SPARE_UP: usize = MAX_LOSSY + 4;

#[derive(Default)]
pub struct Outbox {
    state: Mutex<State>,
    ready: Condvar,
}

/// The lane the writer sends a packet in next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    Control,
    Lossy,
}

/// The messages the writer has started: at most one per lane.
#[derive(Default)]
pub struct Lanes {
    control: Option<Outgoing>,
    lossy: Option<Outgoing>,
}

impl Lanes {
    /// Sends the next packet in `pick`'s lane: the message, once that was its last
    /// packet (its body can be filled again, see [`Outbox::sent`]).
    pub fn send(&mut self, pick: Pick, tx: &mut dyn RecordSender) -> io::Result<Option<Encoded>> {
        let out = match pick {
            Pick::Control => &mut self.control,
            Pick::Lossy => &mut self.lossy,
        };
        if let Some(o) = out
            && o.send_next(tx, FRAGMENT)?
        {
            return Ok(out.take().map(Outgoing::into_message));
        }
        Ok(None)
    }
}

impl Outbox {
    /// Queues `msg`. With [`MAX_LOSSY`] waiting in the sound lane, a lossy message
    /// takes the place of the oldest lossy one there (the start and end of an up
    /// stream stay): what goes out after a stall is the newest sound. An error means
    /// the session is over, or the message cannot be sent at all.
    pub fn push(&self, msg: ClientMsg) -> io::Result<()> {
        let (sound, is_lossy) = (in_sound_lane(&msg), lossy(&msg));
        let msg = msg.encode()?;
        let mut s = self.state.lock().unwrap();
        if s.closed || s.leaving {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        if sound {
            if is_lossy && s.lossy.len() >= MAX_LOSSY {
                match s.lossy.iter().position(|(_, lossy)| *lossy) {
                    Some(oldest) => {
                        s.lossy.remove(oldest);
                    }
                    None => return Ok(()),
                }
            }
            s.lossy.push_back((msg, is_lossy));
            s.lossy_max = s.lossy_max.max(s.lossy.len());
        } else {
            s.control.push_back(msg);
        }
        self.ready.notify_one();
        Ok(())
    }

    /// An empty buffer for a slice of the microphone of `bytes`: one of the slices sent
    /// before when there is one, so recording needs no new memory for each.
    pub fn up_buffer(&self, bytes: usize) -> Vec<u8> {
        let spare = self.state.lock().unwrap().spare.pop();
        match spare {
            Some(mut b) => {
                b.clear();
                b.reserve(bytes);
                b
            }
            None => Vec::with_capacity(bytes),
        }
    }

    /// Message `msg` was sent: the body of a slice of the microphone is kept to be
    /// filled again.
    pub fn sent(&self, msg: Encoded) {
        if msg.tag() == ClientMsg::UP_TAG {
            let mut s = self.state.lock().unwrap();
            if s.spare.len() < SPARE_UP {
                s.spare.push(msg.into_body());
            }
        }
    }

    /// The session is open: pings start.
    pub fn start_pings(&self) {
        let mut s = self.state.lock().unwrap();
        s.next_ping = Some(Instant::now());
        self.ready.notify_one();
    }

    /// The PC answered ping `seq`.
    pub fn pong(&self, seq: u32) {
        let mut s = self.state.lock().unwrap();
        if let Some(rtt) = s.pings.pong(seq, Instant::now()) {
            s.round_trip.add(rtt);
        }
    }

    /// The round trips since the last call.
    pub fn take_round_trips(&self) -> Histogram {
        std::mem::take(&mut self.state.lock().unwrap().round_trip)
    }

    /// The most microphone sound and acknowledgements waiting at once since the last
    /// call.
    pub fn take_lossy_max(&self) -> usize {
        std::mem::take(&mut self.state.lock().unwrap().lossy_max)
    }

    /// Ends the session on purpose: the control messages waiting go out, then a
    /// goodbye saying `reason`, and nothing else.
    pub fn say_goodbye(&self, reason: HeadsetGoodbye) {
        let mut s = self.state.lock().unwrap();
        if s.closed || s.leaving {
            return;
        }
        s.lossy.clear();
        if let Ok(goodbye) = (ClientMsg::Goodbye {
            reason: reason.code(),
        })
        .encode()
        {
            s.control.push_back(goodbye);
        }
        s.leaving = true;
        self.ready.notify_all();
    }

    /// Waits until the session is over (the reader saw the PC close the connection),
    /// at most `within`.
    pub fn wait_closed(&self, within: Duration) {
        let s = self.state.lock().unwrap();
        let _ = self.ready.wait_timeout_while(s, within, |s| !s.closed);
    }

    /// The writer sent the goodbye.
    pub fn said_goodbye(&self) -> bool {
        self.state.lock().unwrap().leaving
    }

    /// The session is over: nothing more is taken or sent.
    pub fn close(&self) {
        let mut s = self.state.lock().unwrap();
        s.closed = true;
        s.control.clear();
        s.lossy.clear();
        self.ready.notify_all();
    }

    /// The lane to send the next packet in, waiting until there is one; `None` once
    /// closed. A message taken from the queues moves into `lanes`.
    pub fn next(&self, lanes: &mut Lanes) -> Option<Pick> {
        let mut s = self.state.lock().unwrap();
        loop {
            if s.closed {
                return None;
            }
            if s.leaving {
                // Up to the goodbye, then done.
                return s.pick_control(lanes);
            }
            let now = Instant::now();
            if let Some(at) = s.next_ping
                && now >= at
            {
                s.next_ping = Some(now + PING_EVERY);
                let seq = s.pings.ping(now);
                if let Ok(ping) = (ClientMsg::Ping { seq }).encode() {
                    s.control.push_back(ping);
                }
            }
            if let Some(pick) = s.pick(lanes) {
                return Some(pick);
            }
            s = match s.next_ping {
                Some(at) => {
                    let wait = at.saturating_duration_since(now);
                    self.ready.wait_timeout(s, wait).unwrap().0
                }
                None => self.ready.wait(s).unwrap(),
            };
        }
    }
}

impl State {
    fn pick_control(&mut self, lanes: &mut Lanes) -> Option<Pick> {
        if lanes.control.is_none()
            && let Some(msg) = self.control.pop_front()
        {
            lanes.control = Some(Outgoing::new(lane::CONTROL, msg));
        }
        lanes.control.is_some().then_some(Pick::Control)
    }

    fn pick(&mut self, lanes: &mut Lanes) -> Option<Pick> {
        if let Some(pick) = self.pick_control(lanes) {
            return Some(pick);
        }
        if lanes.lossy.is_none()
            && let Some((msg, _)) = self.lossy.pop_front()
        {
            lanes.lossy = Some(Outgoing::new(lane::SOUND, msg));
        }
        lanes.lossy.is_some().then_some(Pick::Lossy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prostir_proto::link::memory;
    use prostir_proto::request::Request;
    use prostir_proto::state::{DisplayId, Gen, Presence, ReqId, UpFormat, UpKind};

    fn worn(worn: bool) -> ClientMsg {
        ClientMsg::Presence {
            part: Presence { worn },
        }
    }

    /// Sends everything waiting through a link and reads it back as the PC would.
    fn sent(o: &Outbox) -> Vec<(u8, ClientMsg)> {
        let (mut headset, mut pc) = memory::pair(4096);
        let mut lanes = Lanes::default();
        o.drain_into(&mut lanes, &mut *headset.sender);
        headset.closer.close();
        let mut asm = ClientMsg::assembler();
        asm.set_limits(ClientMsg::limits(true));
        let mut out = Vec::new();
        while let Ok(record) = pc.receiver.recv() {
            let lane = record[1];
            if let Some(m) = asm.push(record).unwrap() {
                out.push((lane, ClientMsg::from_message(m).unwrap().unwrap()));
            }
        }
        out
    }

    impl Outbox {
        /// Sends what is waiting, without blocking.
        fn drain_into(&self, lanes: &mut Lanes, tx: &mut dyn RecordSender) {
            loop {
                let pick = self.state.lock().unwrap().pick(lanes);
                match pick {
                    Some(pick) => {
                        if let Some(done) = lanes.send(pick, tx).unwrap() {
                            self.sent(done);
                        }
                    }
                    None => break,
                }
            }
        }
    }

    /// A slice of the microphone sent is filled again: no new memory for each.
    #[test]
    fn microphone_slices_are_filled_again() {
        let o = Outbox::default();
        let first = o.up_buffer(960);
        assert!(first.capacity() >= 960);
        let at = first.as_ptr();
        o.push(ClientMsg::Up { up: 0, data: first }).unwrap();
        o.push(worn(true)).unwrap();
        assert_eq!(sent(&o).len(), 2);
        let again = o.up_buffer(960);
        assert_eq!(again.as_ptr(), at, "the same memory");
        assert!(again.is_empty());
        // Only the microphone's: a control message's body is not kept.
        let fresh = o.up_buffer(960);
        assert_ne!(fresh.as_ptr(), at);
    }

    #[test]
    fn control_goes_first_and_each_class_has_its_lane() {
        let o = Outbox::default();
        o.push(ClientMsg::Up {
            up: 0,
            data: vec![1; 960],
        })
        .unwrap();
        o.push(ClientMsg::FrameShown {
            id: DisplayId(0),
            generation: Gen(1),
            seq: 5,
        })
        .unwrap();
        o.push(worn(true)).unwrap();
        assert_eq!(
            sent(&o),
            [
                (lane::CONTROL, worn(true)),
                (
                    lane::SOUND,
                    ClientMsg::Up {
                        up: 0,
                        data: vec![1; 960],
                    }
                ),
                (
                    lane::SOUND,
                    ClientMsg::FrameShown {
                        id: DisplayId(0),
                        generation: Gen(1),
                        seq: 5,
                    }
                ),
            ]
        );
    }

    /// While the network is stuck, sound and acknowledgements beyond a limit are
    /// dropped, the oldest first; control messages never are.
    #[test]
    fn only_lossy_messages_are_dropped() {
        let o = Outbox::default();
        o.push(ClientMsg::UpStart {
            up: 0,
            kind: UpKind::Mic,
            format: UpFormat::Pcm16 {
                rate: 48_000,
                channels: 1,
            },
        })
        .unwrap();
        for k in 0..MAX_LOSSY + 10 {
            o.push(ClientMsg::Up {
                up: 0,
                data: vec![k as u8; 2],
            })
            .unwrap();
        }
        o.push(ClientMsg::UpEnd { up: 0 }).unwrap();
        for i in 0..MAX_LOSSY + 10 {
            let id = DisplayId((i % 100) as u32);
            o.push(ClientMsg::request(
                ReqId(i as u32),
                Request::RemoveDisplay { id },
            ))
            .unwrap();
        }
        let msgs = sent(&o);
        let mic: Vec<&ClientMsg> = msgs
            .iter()
            .filter(|(l, _)| *l == lane::SOUND)
            .map(|(_, m)| m)
            .collect();
        // The stream's start and end, never dropped, around the data that fit.
        assert_eq!(mic.len(), MAX_LOSSY + 1);
        assert!(matches!(mic[0], ClientMsg::UpStart { .. }));
        // The oldest sound went, the newest stayed.
        let kept: Vec<u8> = mic
            .iter()
            .filter_map(|m| match m {
                ClientMsg::Up { data, .. } => Some(data[0]),
                _ => None,
            })
            .collect();
        let newest = (MAX_LOSSY + 10 - (MAX_LOSSY - 1)) as u8..(MAX_LOSSY + 10) as u8;
        assert_eq!(kept, newest.collect::<Vec<u8>>());
        assert_eq!(mic.last(), Some(&&ClientMsg::UpEnd { up: 0 }));
        assert_eq!(msgs.len() - mic.len(), MAX_LOSSY + 10);
        assert_eq!(o.take_lossy_max(), MAX_LOSSY + 1, "with the stream's end");
        assert_eq!(o.take_lossy_max(), 0, "since the last look");
    }

    /// Pings start with the session, one a second, and pongs give the round trip.
    #[test]
    fn pings_start_with_the_session() {
        let o = Outbox::default();
        let mut lanes = Lanes::default();
        let nothing = o.state.lock().unwrap().pick(&mut lanes);
        assert_eq!(nothing, None, "no ping before the session");
        o.start_pings();
        assert_eq!(o.next(&mut lanes), Some(Pick::Control));
        let (mut headset, mut pc) = memory::pair(8);
        lanes.send(Pick::Control, &mut *headset.sender).unwrap();
        let mut asm = ClientMsg::assembler();
        let m = asm.push(pc.receiver.recv().unwrap()).unwrap().unwrap();
        let Some(ClientMsg::Ping { seq }) = ClientMsg::from_message(m).unwrap() else {
            panic!("a ping")
        };
        o.pong(seq);
        assert_eq!(o.take_round_trips().count(), 1);
        // The next one a second later.
        let start = Instant::now();
        assert_eq!(o.next(&mut lanes), Some(Pick::Control));
        assert!(start.elapsed() >= PING_EVERY - Duration::from_millis(50));
    }

    /// A goodbye goes after the control messages waiting, and nothing after it.
    #[test]
    fn a_goodbye_is_the_last_thing_sent() {
        let o = Outbox::default();
        o.push(ClientMsg::Up {
            up: 0,
            data: vec![1; 960],
        })
        .unwrap();
        o.push(worn(false)).unwrap();
        o.say_goodbye(HeadsetGoodbye::OtherPc);
        assert!(o.push(ClientMsg::Open { parts: Vec::new() }).is_err());
        assert_eq!(
            sent(&o),
            [
                (lane::CONTROL, worn(false)),
                (lane::CONTROL, ClientMsg::Goodbye { reason: 2 }),
            ]
        );
        assert!(o.said_goodbye());
        assert_eq!(o.next(&mut Lanes::default()), None, "done, without waiting");
    }

    #[test]
    fn nothing_is_taken_once_closed() {
        let o = Outbox::default();
        o.close();
        assert!(o.push(ClientMsg::Open { parts: Vec::new() }).is_err());
        assert_eq!(o.next(&mut Lanes::default()), None);
        // Nor what cannot be sent at all.
        let o = Outbox::default();
        let long = ClientMsg::PairBegin {
            headset_shows: true,
            name: "x".repeat(65),
        };
        assert_eq!(
            o.push(long).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
