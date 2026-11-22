use super::*;
use crate::link::LinkInfo;
use crate::tests::{Seeded, client_samples, host_samples, mutate};
use crate::{ClientMsg, HostMsg};

/// Takes the records a sender sends.
#[derive(Default)]
struct Capture(Vec<Vec<u8>>);

impl RecordSender for Capture {
    fn send(&mut self, parts: &[&[u8]]) -> io::Result<()> {
        let record = parts.concat();
        assert!(record.len() <= MAX_RECORD, "a record holds the packet");
        self.0.push(record);
        Ok(())
    }

    fn finish(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn set_pace(&mut self, _bytes_per_sec: u64) {}

    fn link_info(&mut self) -> Option<LinkInfo> {
        None
    }
}

/// No bytes kept apart from the body.
fn none(_tag: u8) -> usize {
    0
}

const ANY: Limits = Limits {
    message: u32::MAX,
    lanes: 256,
    in_flight: usize::MAX,
};

/// The packets of `msg` in `lane`, `fragment` bytes of it each.
fn packets(lane: u8, msg: Encoded, fragment: usize) -> Vec<Vec<u8>> {
    let mut out = Capture::default();
    send_whole(&mut out, lane, msg, fragment).unwrap();
    out.0
}

/// A packet by hand.
fn packet(flags: u8, lane: u8, body: &[u8]) -> Vec<u8> {
    [&[flags, lane], body].concat()
}

/// A message's header.
fn header(tag: u8, len: u32) -> Vec<u8> {
    [&[tag][..], &len.to_le_bytes()].concat()
}

/// Every message read back from `records`, failing on the first error.
fn assemble(asm: &mut Assembler, records: &[Vec<u8>]) -> Result<Vec<Message>, PacketError> {
    let mut out = Vec::new();
    for r in records {
        out.extend(asm.push(r)?);
    }
    Ok(out)
}

#[test]
fn messages_of_any_size_come_back_whole() {
    let f = FRAGMENT;
    // Around the empty payload, the header and each packet boundary (the header's 5
    // bytes count towards the first packet).
    let sizes = [
        0,
        1,
        4,
        5,
        f - 6,
        f - 5,
        f - 4,
        2 * f - 6,
        2 * f - 5,
        2 * f - 4,
    ];
    for size in sizes {
        let payload: Vec<u8> = (0..size).map(|i| (i % 253) as u8).collect();
        let records = packets(7, Encoded::new(200, &[], payload.clone()).unwrap(), f);
        let expected = (size + MESSAGE_HEADER).div_ceil(f);
        assert_eq!(records.len(), expected, "{size} bytes");
        assert!(records.iter().all(|r| r[1] == 7 && r.len() > HEADER));
        let mut asm = Assembler::new(ANY, none);
        let got = assemble(&mut asm, &records).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(
            (got[0].tag, got[0].payload()),
            (200, payload),
            "{size} bytes"
        );
        assert_eq!(asm.assembling(), 0);
    }
}

/// The largest frame the PC may send in a session, through the real decoder: its
/// data comes out as the body, and its fields as the head.
#[test]
fn the_largest_frame_comes_back_whole() {
    let max = HostMsg::limits(true).message as usize;
    let data: Vec<u8> = (0..max - crate::FRAME_HEAD)
        .map(|i| (i % 249) as u8)
        .collect();
    let frame = HostMsg::Frame {
        id: crate::state::DisplayId(3),
        generation: crate::state::Gen(1),
        seq: 0,
        keyframe: true,
        pts_us: 99,
        data,
    };
    let records = packets(lane::VIDEO + 3, frame.clone().encode().unwrap(), FRAGMENT);
    let mut asm = HostMsg::assembler();
    asm.set_limits(HostMsg::limits(true));
    let got = assemble(&mut asm, &records).unwrap();
    let [m] = <[Message; 1]>::try_from(got).unwrap();
    assert_eq!(m.head().len(), crate::FRAME_HEAD);
    assert_eq!(HostMsg::from_message(m).unwrap(), Some(frame));
    // One byte more is refused before anything is allocated for it.
    let first = packet(FIRST, 0, &header(3, max as u32 + 1));
    assert_eq!(
        asm.push(&first),
        Err(PacketError::TooLarge {
            len: max as u32 + 1,
            max: max as u32
        })
    );
}

/// Messages written in packets read back as they were, from both sides.
#[test]
fn every_message_goes_in_packets() {
    let mut asm = HostMsg::assembler();
    asm.set_limits(HostMsg::limits(true));
    for msg in host_samples() {
        let records = packets(0, msg.clone().encode().unwrap(), 6);
        let got = assemble(&mut asm, &records).unwrap();
        let [m] = <[Message; 1]>::try_from(got).unwrap();
        // The same bytes as the stream format of protocol 1.
        let mut stream = Vec::new();
        msg.write(&mut stream).unwrap();
        assert_eq!(
            [&header(m.tag, m.payload().len() as u32)[..], &m.payload()].concat(),
            stream
        );
        assert_eq!(HostMsg::from_message(m).unwrap(), Some(msg));
    }
    let mut asm = ClientMsg::assembler();
    for msg in client_samples() {
        let records = packets(1, msg.clone().encode().unwrap(), FRAGMENT);
        let got = assemble(&mut asm, &records).unwrap();
        let [m] = <[Message; 1]>::try_from(got).unwrap();
        assert_eq!(ClientMsg::from_message(m).unwrap(), Some(msg));
    }
    // A tag this build does not know is put together and skipped.
    let unknown = packets(0, Encoded::new(250, &[], vec![1, 2, 3]).unwrap(), FRAGMENT);
    let [m] = <[Message; 1]>::try_from(assemble(&mut asm, &unknown).unwrap()).unwrap();
    assert_eq!(ClientMsg::from_message(m).unwrap(), None);
}

/// Messages of different lanes go between each other's packets and come out in the
/// order of their last packets; one lane keeps its order.
#[test]
fn lanes_interleave() {
    let big = |id: u8| Encoded::new(id, &[], vec![id; 3 * FRAGMENT]).unwrap();
    let a = packets(2, big(1), FRAGMENT);
    let b = packets(3, big(2), FRAGMENT);
    let small = packets(0, Encoded::new(9, &[], vec![9]).unwrap(), FRAGMENT);
    let order = [
        &a[0], &b[0], &small[0], &a[1], &b[1], &b[2], &b[3], &a[2], &a[3],
    ];
    let mut asm = Assembler::new(ANY, none);
    let mut tags = Vec::new();
    for r in order {
        if let Some(m) = asm.push(r).unwrap() {
            tags.push(m.tag);
            assert_eq!(m.body, vec![m.tag; m.body.len()]);
        }
        assert!(asm.assembling() <= 2);
    }
    assert_eq!(tags, [9, 2, 1]);
    assert_eq!(asm.assembling(), 0);
}

#[test]
fn every_broken_packet_is_refused() {
    let any = || Assembler::new(ANY, none);
    let msg = header(5, 3);
    let cases: Vec<(Vec<Vec<u8>>, PacketError)> = vec![
        (vec![vec![]], PacketError::NoHeader),
        (vec![vec![FIRST | LAST]], PacketError::NoHeader),
        (
            vec![packet(4 | FIRST | LAST, 0, &msg)],
            PacketError::Flags(4 | FIRST | LAST),
        ),
        (vec![packet(FIRST, 6, &[])], PacketError::Empty { lane: 6 }),
        (
            vec![packet(LAST, 6, b"abc")],
            PacketError::NotStarted { lane: 6 },
        ),
        (
            vec![packet(0, 6, b"abc")],
            PacketError::NotStarted { lane: 6 },
        ),
        (
            vec![packet(FIRST, 6, &msg), packet(FIRST, 6, &msg)],
            PacketError::Busy { lane: 6 },
        ),
        (
            vec![packet(FIRST | LAST, 6, &msg[..4])],
            PacketError::ShortFirst { lane: 6 },
        ),
        (
            vec![packet(FIRST | LAST, 6, &[&msg[..], b"abcd"].concat())],
            PacketError::Overrun { lane: 6 },
        ),
        (
            vec![packet(FIRST, 6, &msg), packet(LAST, 6, b"abcd")],
            PacketError::Overrun { lane: 6 },
        ),
        (
            vec![packet(FIRST | LAST, 6, &[&msg[..], b"ab"].concat())],
            PacketError::EndedEarly { lane: 6 },
        ),
        (
            vec![packet(FIRST, 6, &msg), packet(LAST, 6, b"ab")],
            PacketError::EndedEarly { lane: 6 },
        ),
        (
            vec![packet(FIRST, 6, &[&msg[..], b"abc"].concat())],
            PacketError::NotLast { lane: 6 },
        ),
        (
            vec![packet(FIRST, 6, &msg), packet(0, 6, b"abc")],
            PacketError::NotLast { lane: 6 },
        ),
    ];
    for (records, expected) in cases {
        let mut asm = any();
        let (last, before) = records.split_last().unwrap();
        for r in before {
            assert_eq!(asm.push(r), Ok(None), "{expected:?}");
        }
        assert_eq!(asm.push(last), Err(expected));
    }
    // A message that fits a packet takes no lane.
    let mut asm = Assembler::new(
        Limits {
            message: 10,
            lanes: 1,
            in_flight: 10,
        },
        none,
    );
    asm.push(&packet(FIRST, 1, &msg)).unwrap();
    assert!(
        asm.push(&packet(FIRST | LAST, 2, &[&msg[..], b"abc"].concat()))
            .unwrap()
            .is_some()
    );
    assert_eq!(
        asm.push(&packet(FIRST, 2, &msg)),
        Err(PacketError::TooManyLanes { max: 1 })
    );
    // A failure is an error of the connection.
    let e = io::Error::from(PacketError::Busy { lane: 1 });
    assert_eq!(e.kind(), io::ErrorKind::InvalidData);
}

/// Before the session a PC may send only small messages in one lane; in it, large
/// ones in many. A headset's messages stay small.
#[test]
fn limits_before_and_in_the_session() {
    let first = |lane, len| packet(FIRST, lane, &header(6, len));
    let mut from_pc = HostMsg::assembler();
    assert_eq!(from_pc.push(&first(0, 64 * 1024)), Ok(None));
    assert_eq!(
        from_pc.push(&first(1, 10)),
        Err(PacketError::TooManyLanes { max: 1 })
    );
    let mut from_pc = HostMsg::assembler();
    assert_eq!(
        from_pc.push(&first(0, 64 * 1024 + 1)),
        Err(PacketError::TooLarge {
            len: 64 * 1024 + 1,
            max: 64 * 1024
        })
    );
    from_pc.set_limits(HostMsg::limits(true));
    for lane in 0..32 {
        assert_eq!(from_pc.push(&first(lane, 1 << 20)), Ok(None));
    }
    assert_eq!(
        from_pc.push(&first(32, 10)),
        Err(PacketError::TooManyLanes { max: 32 })
    );
    // A few small first packets cannot make it set aside gigabytes: the messages
    // being put together hold at most twice the largest one.
    let mut from_pc = HostMsg::assembler();
    from_pc.set_limits(HostMsg::limits(true));
    let max = HostMsg::limits(true).message;
    assert_eq!(from_pc.push(&first(0, max)), Ok(None));
    assert_eq!(from_pc.push(&first(1, max)), Ok(None));
    assert_eq!(
        from_pc.push(&first(2, 1)),
        Err(PacketError::TooMuchAtOnce {
            max: 2 * max as usize
        })
    );
    let mut from_headset = ClientMsg::assembler();
    from_headset.set_limits(ClientMsg::limits(true));
    assert_eq!(from_headset.push(&first(0, 64 * 1024)), Ok(None));
    assert_eq!(
        from_headset.push(&first(1, 64 * 1024 + 1)),
        Err(PacketError::TooLarge {
            len: 64 * 1024 + 1,
            max: 64 * 1024
        })
    );
}

/// A connection that ends in the middle of a message takes it along: nothing of it
/// comes out, and the next connection (a new assembler) starts with empty lanes.
#[test]
fn a_message_cut_off_with_its_connection_is_gone() {
    let frame = HostMsg::Frame {
        id: crate::state::DisplayId(0),
        generation: crate::state::Gen(1),
        seq: 0,
        keyframe: true,
        pts_us: 1,
        data: vec![1; 3 * FRAGMENT],
    };
    let records = packets(lane::VIDEO, frame.clone().encode().unwrap(), FRAGMENT);
    let mut asm = HostMsg::assembler();
    asm.set_limits(HostMsg::limits(true));
    assert_eq!(assemble(&mut asm, &records[..2]).unwrap(), []);
    assert_eq!(asm.assembling(), 1);
    drop(asm);
    let mut next = HostMsg::assembler();
    next.set_limits(HostMsg::limits(true));
    // The rest of the old message is no packet the new connection can place.
    assert_eq!(
        next.push(&records[2]),
        Err(PacketError::NotStarted { lane: lane::VIDEO })
    );
    let mut next = HostMsg::assembler();
    next.set_limits(HostMsg::limits(true));
    let [m] = <[Message; 1]>::try_from(assemble(&mut next, &records).unwrap()).unwrap();
    assert_eq!(HostMsg::from_message(m).unwrap(), Some(frame));
}

/// A sender never makes a first packet without the whole header, however small a
/// fragment it is asked for.
#[test]
fn a_first_packet_always_holds_the_header() {
    let frame = HostMsg::Frame {
        id: crate::state::DisplayId(1),
        generation: crate::state::Gen(1),
        seq: 0,
        keyframe: false,
        pts_us: 5,
        data: vec![7; 40],
    };
    let records = packets(2, frame.clone().encode().unwrap(), 1);
    assert!(records[0].len() >= HEADER + MESSAGE_HEADER);
    let mut asm = HostMsg::assembler();
    let [m] = <[Message; 1]>::try_from(assemble(&mut asm, &records).unwrap()).unwrap();
    assert_eq!(HostMsg::from_message(m).unwrap(), Some(frame));
}

/// Random messages, cut at random and sent in random lanes in a random order between
/// lanes, come back exactly; damaged records never crash the assembler, and what
/// comes back before the first damage is exact.
#[test]
fn random_packets_come_back_or_fail_never_panic() {
    let mut rng = Seeded(0x9ac4_e7d1_0000_0003);
    let samples = host_samples();
    let (mut whole, mut refused) = (0, 0);
    for round in 0..2000 {
        // Up to 6 lanes, each with a few messages in order.
        let lanes = 1 + rng.below(6);
        let mut queues: Vec<Vec<Vec<u8>>> = Vec::new();
        let mut sent: Vec<Vec<HostMsg>> = Vec::new();
        for l in 0..lanes {
            let mut records = Vec::new();
            let mut msgs = Vec::new();
            for _ in 0..1 + rng.below(4) {
                let msg = match rng.below(3) {
                    0 => HostMsg::Frame {
                        id: crate::state::DisplayId(l as u32),
                        generation: crate::state::Gen(1),
                        seq: 0,
                        keyframe: rng.below(2) == 0,
                        pts_us: rng.next(),
                        data: (0..rng.below(3 * 1024)).map(|_| rng.next() as u8).collect(),
                    },
                    _ => samples[rng.below(samples.len())].clone(),
                };
                let fragment = 1 + rng.below(700);
                records.extend(packets(l as u8, msg.clone().encode().unwrap(), fragment));
                msgs.push(msg);
            }
            records.reverse();
            queues.push(records);
            sent.push(msgs);
        }
        // Interleaved: each step takes the next record of a random lane.
        let mut records = Vec::new();
        while queues.iter().any(|q| !q.is_empty()) {
            let open: Vec<usize> = (0..lanes).filter(|&l| !queues[l].is_empty()).collect();
            let l = open[rng.below(open.len())];
            records.push(queues[l].pop().unwrap());
        }
        // Damage some rounds: a record changed, dropped, repeated or two swapped.
        let damaged = round % 2 == 1;
        let mut first_damage = records.len();
        if damaged {
            let i = rng.below(records.len());
            first_damage = i;
            match rng.below(4) {
                0 => records[i] = mutate(&mut rng, &records[i]),
                1 => {
                    records.remove(i);
                }
                2 => records.insert(i, records[i].clone()),
                _ if i + 1 < records.len() => records.swap(i, i + 1),
                _ => records[i] = mutate(&mut rng, &records[i]),
            }
        }
        let mut asm = HostMsg::assembler();
        asm.set_limits(HostMsg::limits(true));
        let mut got: Vec<Vec<HostMsg>> = vec![Vec::new(); lanes];
        let mut failed = false;
        for (i, r) in records.iter().enumerate() {
            match asm.push(r) {
                Ok(Some(m)) => {
                    let lane = r[1] as usize;
                    let msg = HostMsg::from_message(m);
                    if i < first_damage {
                        let msg = msg.unwrap().unwrap();
                        assert_eq!(Some(&msg), sent[lane].get(got[lane].len()));
                        got[lane].push(msg);
                    }
                }
                Ok(None) => {}
                Err(_) => {
                    failed = true;
                    break;
                }
            }
        }
        if damaged {
            if failed {
                refused += 1;
            }
        } else {
            assert!(!failed);
            assert_eq!(got, sent, "round {round}");
            whole += 1;
        }
    }
    assert!(
        whole == 1000 && refused > 500,
        "{whole} whole, {refused} refused"
    );
}

/// A body handed back takes the next message that fits in it, in its memory; one too
/// small for it is left for a later one, and a message's body comes back whole once
/// sent.
#[test]
fn bodies_handed_back_are_filled_again() {
    let mut asm = Assembler::new(ANY, none);
    let slice: Vec<u8> = (0..1920).map(|i| (i % 251) as u8).collect();
    let records = packets(3, Encoded::new(12, &[], slice.clone()).unwrap(), FRAGMENT);
    let first = assemble(&mut asm, &records).unwrap().remove(0);
    let at = first.body.as_ptr();
    // One of another tag first, large enough: never taken for this one.
    asm.recycle(13, Vec::with_capacity(4096));
    asm.recycle(12, first.body);
    // A small spare body too, which the next slice does not fit in.
    asm.recycle(12, Vec::with_capacity(16));
    let again = assemble(&mut asm, &records).unwrap().remove(0);
    assert_eq!(again.body, slice);
    assert_eq!(again.body.as_ptr(), at, "the same memory");
    // The small one is taken by a small message.
    let small = packets(3, Encoded::new(12, &[], vec![1, 2, 3]).unwrap(), FRAGMENT);
    let got = assemble(&mut asm, &small).unwrap().remove(0);
    assert_eq!(got.body, [1, 2, 3]);
    assert!(got.body.capacity() >= 16);
    // The other tag's stays for its own.
    assert!(
        asm.spare
            .iter()
            .any(|(t, b)| *t == 13 && b.capacity() >= 4096)
    );
    // Too many handed back: the rest are let go.
    for _ in 0..20 {
        asm.recycle(12, Vec::with_capacity(8));
    }
    assert_eq!(asm.spare.len(), SPARE_BODIES);
    // What was sent comes back as it went.
    let mut out = Outgoing::new(1, Encoded::new(140, &[7], slice.clone()).unwrap());
    while !out.send_next(&mut Capture::default(), FRAGMENT).unwrap() {}
    assert_eq!(out.into_message().into_body(), slice);
}
