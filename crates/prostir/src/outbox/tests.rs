use super::*;
use prostir_proto::link::LinkInfo;
use prostir_proto::state::Gen;

fn d(n: u32) -> DisplayId {
    DisplayId(n)
}

/// Takes the records the writer sends, and reads them back as the headset would.
#[derive(Default)]
pub(crate) struct Capture {
    records: Vec<Vec<u8>>,
}

impl RecordSender for Capture {
    fn send(&mut self, parts: &[&[u8]]) -> io::Result<()> {
        self.records.push(parts.concat());
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

impl Capture {
    /// The lane of each packet sent so far.
    fn lanes(&self) -> Vec<u8> {
        self.records.iter().map(|r| r[1]).collect()
    }

    /// The messages the packets make, in the order they completed.
    pub(crate) fn messages(&self) -> Vec<HostMsg> {
        let mut asm = HostMsg::assembler();
        asm.set_limits(HostMsg::limits(true));
        self.records
            .iter()
            .filter_map(|r| asm.push(r).unwrap())
            .filter_map(|m| HostMsg::from_message(m).unwrap())
            .collect()
    }
}

fn frame(id: u32, keyframe: bool, bytes: usize) -> HostMsg {
    HostMsg::Frame {
        id: d(id),
        generation: Gen(1),
        seq: 0,
        pts_us: 0,
        keyframe,
        data: vec![id as u8; bytes],
    }
}

/// Sends `n` packets (fewer if nothing is left).
fn send(o: &Outbox, lanes: &mut Lanes, out: &mut Capture, n: usize) {
    for _ in 0..n {
        let Some(pick) = o.try_next(lanes) else {
            return;
        };
        lanes.send(pick, out).unwrap();
    }
}

#[test]
fn control_and_sound_go_before_video() {
    let o = Outbox::default();
    assert!(o.video(d(0), true, frame(0, true, 10)));
    o.audio(HostMsg::Audio { pcm: vec![1] });
    let focus = HostMsg::Focus {
        part: prostir_proto::state::Focus {
            display: Some(d(0)),
        },
    };
    o.control(focus.clone());
    assert_eq!(
        o.sent(),
        [focus, HostMsg::Audio { pcm: vec![1] }, frame(0, true, 10)]
    );
}

/// Sound queued while a keyframe is going out goes between its packets; so does a
/// control message.
#[test]
fn sound_goes_between_the_packets_of_a_keyframe() {
    let o = Outbox::default();
    let (mut lanes, mut out) = (Lanes::default(), Capture::default());
    assert!(o.video(d(3), true, frame(3, true, 10 * FRAGMENT)));
    send(&o, &mut lanes, &mut out, 2);
    o.audio(HostMsg::Audio { pcm: vec![7; 1920] });
    send(&o, &mut lanes, &mut out, 2);
    o.control(HostMsg::PairingRequired);
    send(&o, &mut lanes, &mut out, 100);
    // The first stream takes the first lane.
    let video = lane::VIDEO;
    assert_eq!(
        out.lanes()[..6],
        [video, video, lane::SOUND, video, lane::CONTROL, video]
    );
    assert_eq!(out.lanes().len(), 11 + 2);
    let msgs = out.messages();
    assert!(matches!(
        msgs[..],
        [
            HostMsg::Audio { .. },
            HostMsg::PairingRequired,
            HostMsg::Frame { .. }
        ]
    ));
}

/// Streams take turns, a packet each.
#[test]
fn streams_take_turns() {
    let o = Outbox::default();
    for id in [1, 4, 9] {
        assert!(o.video(d(id), true, frame(id, true, 3 * FRAGMENT)));
    }
    let (mut lanes, mut out) = (Lanes::default(), Capture::default());
    send(&o, &mut lanes, &mut out, 100);
    let [a, b, c] = [2, 3, 4];
    assert_eq!(out.lanes(), [a, b, c, a, b, c, a, b, c, a, b, c]);
    assert_eq!(out.messages().len(), 3);
}

/// A keyframe drops the frames of its stream still waiting, not those of others;
/// a frame already going out is finished first.
#[test]
fn a_keyframe_replaces_older_frames() {
    let o = Outbox::default();
    let (mut lanes, mut out) = (Lanes::default(), Capture::default());
    assert!(o.video(d(1), false, frame(1, false, 2 * FRAGMENT)));
    send(&o, &mut lanes, &mut out, 1);
    assert!(o.video(d(1), false, frame(1, false, 20)));
    assert!(o.video(d(2), false, frame(2, false, 20)));
    assert!(o.video(d(1), true, frame(1, true, 30)));
    send(&o, &mut lanes, &mut out, 100);
    let sent: Vec<(u32, usize)> = out
        .messages()
        .iter()
        .map(|m| match m {
            HostMsg::Frame { id, data, .. } => (id.0, data.len()),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(sent, [(2, 20), (1, 2 * FRAGMENT), (1, 30)]);
    let stats = o.take_stats();
    assert_eq!(stats.video[&d(1)].dropped.replaced, 1);
    assert_eq!(stats.video[&d(2)].dropped.total(), 0);
    // Replaced by a keyframe: nothing to ask the encoder for.
    assert!(o.take_dropped().is_empty());
}

/// A stream's start goes in its own lane, after the frame of the old size that is
/// going out, and drops those still waiting.
#[test]
fn a_stream_start_follows_the_frame_being_sent() {
    let o = Outbox::default();
    let (mut lanes, mut out) = (Lanes::default(), Capture::default());
    assert!(o.video(d(5), true, frame(5, true, 3 * FRAGMENT)));
    send(&o, &mut lanes, &mut out, 1);
    assert!(o.video(d(5), false, frame(5, false, 10)));
    let start = HostMsg::StreamStart {
        id: d(5),
        generation: Gen(2),
        w: 640,
        h: 360,
        codec: prostir_proto::Codec::H264,
    };
    o.control(start.clone());
    send(&o, &mut lanes, &mut out, 100);
    assert!(out.lanes().iter().all(|&l| l == lane::VIDEO));
    assert_eq!(out.messages(), [frame(5, true, 3 * FRAGMENT), start]);
    assert_eq!(o.take_stats().video[&d(5)].dropped.restart, 1);
}

#[test]
fn late_frames_are_dropped_until_a_keyframe() {
    let o = Outbox::default();
    o.video(d(1), false, frame(1, false, 10));
    o.video(d(1), false, frame(1, false, 10));
    // Another stream is not affected.
    std::thread::sleep(MAX_AGE + Duration::from_millis(20));
    o.video(d(2), false, frame(2, false, 10));
    assert_eq!(o.sent(), [frame(2, false, 10)]);
    assert_eq!(o.take_dropped(), BTreeSet::from([d(1)]));
    // Lost: the stream takes only a keyframe now.
    assert!(!o.video(d(1), false, frame(1, false, 10)));
    assert!(o.video(d(1), true, frame(1, true, 10)));
    assert!(o.video(d(1), false, frame(1, false, 10)));
    assert_eq!(o.sent(), [frame(1, true, 10), frame(1, false, 10)]);
    let stats = o.take_stats();
    let one = &stats.video[&d(1)];
    assert_eq!(
        one.dropped,
        Drops {
            late: 1,
            after_loss: 2,
            ..Drops::default()
        }
    );
    assert_eq!((one.frames, one.keyframes), (2, 1));
    assert_eq!(stats.video[&d(2)].frames, 1);
    assert!(stats.video[&d(2)].wait.max() < MAX_AGE);
    // Taken: counting starts over.
    assert!(o.take_stats().video.is_empty());
}

#[test]
fn drops_are_counted_by_reason() {
    let o = Outbox::default();
    assert!(o.video(d(4), true, frame(4, true, MAX_VIDEO_BYTES)));
    // Full: only keyframes still get in.
    assert!(!o.video(d(4), false, frame(4, false, 10)));
    assert!(o.video(d(6), true, frame(6, true, 10)));
    o.control(HostMsg::StreamEnd {
        id: d(6),
        generation: Gen(1),
    });
    assert!(o.video(d(4), true, frame(4, true, 10)));
    let stats = o.take_stats();
    assert_eq!(
        stats.video[&d(4)].dropped,
        Drops {
            full: 1,
            replaced: 1,
            ..Drops::default()
        }
    );
    assert_eq!(stats.video[&d(6)].dropped.restart, 1);
    assert_eq!(stats.video[&d(4)].dropped.total(), 2);
}

#[test]
fn a_summary_names_rates_and_drops() {
    let mut v = VideoStats {
        frames: 300,
        bytes: 15_000_000,
        keyframes: 2,
        keyframe_bytes: 400 * 1024,
        keyframe_max: 300 * 1024,
        ..VideoStats::default()
    };
    v.wait.add(Duration::from_millis(1));
    v.dropped.late = 4;
    assert_eq!(
        v.summary(Duration::from_secs(10)),
        "sent 300 frames (30.0/s), 12.0 Mbit/s; waited in outbox p50 1.0 p95 1.0 max 1.0 ms; \
         keyframes 2 (12/min), avg 200 KB, max 300 KB; dropped 4 late, 0 after a loss, \
         0 queue full, 0 at restart, 0 for a newer keyframe"
    );
}

#[test]
fn sound_keeps_the_newest_slices() {
    let o = Outbox::default();
    for i in 0..(MAX_AUDIO as u8 + 3) {
        o.audio(HostMsg::Audio { pcm: vec![i] });
    }
    let sent = o.sent();
    assert_eq!(sent.len(), MAX_AUDIO);
    assert_eq!(sent[0], HostMsg::Audio { pcm: vec![3] });
    let stats = o.take_stats();
    assert_eq!(stats.audio_dropped, 3);
    assert_eq!(stats.audio_wait.count(), MAX_AUDIO as u32);
}

/// A headset that reads nothing: control messages pile up, and past their limit
/// the session ends instead of the PC holding them for ever.
#[test]
fn too_many_control_messages_end_the_session() {
    let o = Outbox::default();
    let notice = || HostMsg::Notice {
        text: "x".repeat(8 * 1024),
    };
    let fit = MAX_CONTROL_BYTES / (8 * 1024 + 7);
    for _ in 0..fit {
        assert!(o.control(notice()));
    }
    assert!(!o.control(notice()), "over the limit");
    assert_eq!(o.next(&mut Lanes::default()), None, "closed");
}

/// A ping goes out every second, and its pong gives the round trip.
#[test]
fn pings_go_out_every_second() {
    let o = Outbox::default();
    let mut lanes = Lanes::default();
    let start = Instant::now();
    assert_eq!(o.next(&mut lanes), Some(Pick::Control));
    assert!(start.elapsed() >= PING_EVERY - Duration::from_millis(5));
    let mut out = Capture::default();
    lanes.send(Pick::Control, &mut out).unwrap();
    let [HostMsg::Ping { seq }] = out.messages()[..] else {
        panic!("a ping: {:?}", out.messages())
    };
    o.pong(seq);
    assert_eq!(o.take_stats().round_trip.count(), 1);
}

/// A goodbye follows the control messages waiting; sound and video are dropped, a
/// frame half sent stays half sent, and nothing is taken after it.
#[test]
fn a_goodbye_is_the_last_thing_sent() {
    let o = Outbox::default();
    let (mut lanes, mut out) = (Lanes::default(), Capture::default());
    assert!(o.video(d(1), true, frame(1, true, 3 * FRAGMENT)));
    send(&o, &mut lanes, &mut out, 1);
    o.audio(HostMsg::Audio { pcm: vec![1] });
    o.control(HostMsg::PairingRequired);
    o.say_goodbye(PcGoodbye::TakenOff);
    assert!(
        !o.control(HostMsg::PairingRequired),
        "nothing after the goodbye"
    );
    assert!(!o.video(d(1), true, frame(1, true, 1)));
    send(&o, &mut lanes, &mut out, 100);
    assert_eq!(
        out.messages(),
        [HostMsg::PairingRequired, HostMsg::Goodbye { reason: 2 }]
    );
    assert!(o.said_goodbye());
    assert_eq!(o.next(&mut lanes), None, "done, without waiting");
}

/// A stream keeps its lane from its start to its end; then another stream takes
/// it, but not while the end is still going out.
#[test]
fn streams_take_free_lanes_and_give_them_back() {
    let start = |id| HostMsg::StreamStart {
        id: d(id),
        generation: Gen(id),
        w: 64,
        h: 32,
        codec: prostir_proto::Codec::H264,
    };
    let end = |id| HostMsg::StreamEnd {
        id: d(id),
        generation: Gen(id),
    };
    // The lane of each start and end sent, by stream.
    let lanes_of = |out: &Capture| -> Vec<(u32, u8)> {
        out.records
            .iter()
            .filter(|r| r[0] == 3)
            .filter_map(|r| match HostMsg::read_setup(&mut &r[2..]).ok()? {
                HostMsg::StreamStart { id, .. } | HostMsg::StreamEnd { id, .. } => {
                    Some((id.0, r[1]))
                }
                _ => None,
            })
            .collect()
    };
    let o = Outbox::default();
    let (mut lanes, mut out) = (Lanes::default(), Capture::default());
    // Numbers far apart get neighbouring lanes.
    o.control(start(70_000));
    o.control(start(3));
    send(&o, &mut lanes, &mut out, 10);
    assert_eq!(lanes_of(&out), [(3, 2), (70_000, 3)]);
    o.control(end(70_000));
    o.control(start(9));
    send(&o, &mut lanes, &mut out, 10);
    o.control(start(11));
    send(&o, &mut lanes, &mut out, 10);
    let mut later = lanes_of(&out)[2..].to_vec();
    assert_eq!(
        later.pop(),
        Some((11, 3)),
        "the ended stream's lane is free after it"
    );
    later.sort_unstable();
    assert_eq!(later, [(9, 4), (70_000, 3)], "the end in its own lane");
    // Every lane taken: the next stream waits for one.
    let o = Outbox::default();
    let (mut lanes, mut out) = (Lanes::default(), Capture::default());
    for id in 0..31 {
        o.control(start(id));
    }
    send(&o, &mut lanes, &mut out, 100);
    assert_eq!(out.records.len(), 30, "one stream waits for a lane");
    o.control(end(0));
    send(&o, &mut lanes, &mut out, 100);
    assert_eq!(lanes_of(&out)[30..], [(0, 2), (30, 2)]);
}

/// A message that cannot be sent ends the session, and nothing else is sent.
#[test]
fn a_message_over_its_limits_ends_the_session() {
    let o = Outbox::default();
    let too_long = HostMsg::Notice {
        text: "x".repeat(8 * 1024 + 1),
    };
    assert!(!o.control(too_long));
    assert!(!o.video(d(0), true, frame(0, true, 1)));
}

fn cursor_at(u: u16, shape: u32) -> HostMsg {
    HostMsg::CursorAt {
        display: Some(d(1)),
        u,
        v: 0,
        shape,
    }
}

fn cursor_shape(shape: u32) -> HostMsg {
    HostMsg::CursorShape {
        shape,
        w: 2,
        h: 2,
        hot_x: 0,
        hot_y: 0,
        rgba: vec![255; 16],
    }
}

/// Only the newest place of the pointer waits; it goes after the control messages
/// queued before it (the look it names among them), and before sound and video.
#[test]
fn the_pointer_keeps_only_the_newest_after_its_look() {
    let o = Outbox::default();
    assert!(o.video(d(0), true, frame(0, true, 10)));
    o.audio(HostMsg::Audio { pcm: vec![1] });
    o.cursor(cursor_at(1, 7));
    assert!(o.control(cursor_shape(8)));
    o.cursor(cursor_at(2, 8));
    o.cursor(cursor_at(3, 8));
    let sent = o.sent();
    assert_eq!(sent.len(), 4, "{sent:?}");
    assert_eq!(sent[0], cursor_shape(8));
    assert_eq!(sent[1], cursor_at(3, 8));
    assert!(matches!(sent[2], HostMsg::Audio { .. }));
    assert!(matches!(sent[3], HostMsg::Frame { .. }));
}

/// A place queued while a big control message goes out waits for it, whole: it never
/// starts in the control lane while another message is half way there.
#[test]
fn the_pointer_waits_for_a_control_message_half_sent() {
    let o = Outbox::default();
    let big = HostMsg::CursorShape {
        shape: 4,
        w: 256,
        h: 256,
        hot_x: 0,
        hot_y: 0,
        rgba: vec![1; 256 * 256 * 4],
    };
    assert!(o.control(big.clone()));
    assert!(o.control(cursor_shape(5)));
    let (mut lanes, mut out) = (Lanes::default(), Capture::default());
    send(&o, &mut lanes, &mut out, 1);
    o.cursor(cursor_at(9, 5));
    send(&o, &mut lanes, &mut out, 100);
    assert!(out.records.len() > 3, "the big look in several packets");
    let sent = out.messages();
    assert_eq!(sent, [big, cursor_shape(5), cursor_at(9, 5)]);
    assert!(out.lanes().iter().all(|l| *l == lane::CONTROL));
}

/// A goodbye drops the pointer's place, as it drops sound and video.
#[test]
fn a_goodbye_drops_the_pointer() {
    let o = Outbox::default();
    o.cursor(cursor_at(1, 1));
    o.say_goodbye(PcGoodbye::Quit);
    o.cursor(cursor_at(2, 1));
    assert_eq!(
        o.sent(),
        [HostMsg::Goodbye {
            reason: PcGoodbye::Quit.code()
        }]
    );
}
