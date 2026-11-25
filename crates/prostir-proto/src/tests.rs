use super::*;
use crate::request::Request;
use crate::state::{
    Decoder, DeskRect, Display, DisplayKind, DisplayState, Feature, HeadsetState, MicDevice, Mode,
    PcState, Placed, ProfileName, Wanted,
};

/// A message as it goes on the wire: tag, length, payload.
fn framed(tag: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

fn host_bytes(msg: &HostMsg) -> Vec<u8> {
    let mut out = Vec::new();
    msg.write(&mut out).unwrap();
    out
}

fn client_bytes(msg: &ClientMsg) -> Vec<u8> {
    let mut out = Vec::new();
    msg.write(&mut out).unwrap();
    out
}

/// The generic encoding the macro gives a message, for comparing the hand-written
/// media path with it.
fn generic_host(msg: &HostMsg) -> Vec<u8> {
    let mut payload = Vec::new();
    msg.put_fields(&mut payload).unwrap();
    framed(msg.wire_tag(), &payload)
}

/// Any message from the PC read as one stream (in the session's limits).
fn host_read(r: &mut impl Read) -> io::Result<HostMsg> {
    HostMsg::read_within(r, MAX_HOST_PAYLOAD)
}

/// Any message from a headset read as one stream.
fn client_read(r: &mut impl Read) -> io::Result<ClientMsg> {
    ClientMsg::read_within(r, MAX_CLIENT_PAYLOAD)
}

fn read_host(bytes: &[u8]) -> io::Result<HostMsg> {
    let mut r = bytes;
    let msg = host_read(&mut r)?;
    assert!(r.is_empty(), "the reader took the whole message");
    Ok(msg)
}

fn read_client(bytes: &[u8]) -> io::Result<ClientMsg> {
    let mut r = bytes;
    let msg = client_read(&mut r)?;
    assert!(r.is_empty(), "the reader took the whole message");
    Ok(msg)
}

/// The decoding error inside an io::Error, if it is one.
fn wire_error(e: &io::Error) -> Option<&WireError> {
    e.get_ref()?.downcast_ref()
}

/// Reading failed with exactly `expected`.
#[track_caller]
#[allow(
    clippy::needless_pass_by_value,
    reason = "called with a fresh error each time"
)]
fn assert_wire<T: std::fmt::Debug>(read: io::Result<T>, expected: WireError) {
    let err = read.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert_eq!(wire_error(&err), Some(&expected));
}

fn too_long(len: usize, max: usize) -> WireError {
    WireError::TooLong { len, max }
}

fn display(id: u32, kind: DisplayKind) -> Display {
    Display {
        id: DisplayId(id),
        kind,
        name: format!("Prostir-{id}"),
        mode: Mode::new(2560, 1440),
        desk: Some(DeskRect {
            x: 0,
            y: -1440,
            w: 2560,
            h: 1440,
        }),
        state: DisplayState::Running,
    }
}

/// The PC's state with something in every part.
pub(crate) fn pc_state() -> PcState {
    PcState {
        displays: Some(Displays {
            displays: vec![
                display(3, DisplayKind::Virtual),
                display(
                    100,
                    DisplayKind::Physical {
                        builtin: true,
                        connector: "eDP-1".into(),
                        shown: true,
                    },
                ),
                Display {
                    state: DisplayState::Failed {
                        error: ErrorCode::DESKTOP,
                        detail: "KScreen".into(),
                    },
                    desk: None,
                    ..display(
                        101,
                        DisplayKind::Window {
                            title: "Firefox".into(),
                        },
                    )
                },
            ],
            primary: Some(DisplayId(3)),
        }),
        info: Some(PcInfo {
            name: "desk".into(),
            app: "0.2.0".into(),
            language: "uk".into(),
            os: state::os::LINUX,
            desktop: "KDE Plasma".into(),
        }),
        caps: Some(HostCaps {
            features: vec![state::caps::pc::MOVE_MOUSE, Feature(900)],
        }),
        access: Some(Access::MAIN),
        limits: Some(state::Limits {
            virtual_max: 6,
            windows_max: 3,
            physical_max: 1,
            streams_max: 10,
            min: (320, 240),
            max: (7680, 4320),
            sizes: vec![Mode::new(1920, 1080), Mode::new(5120, 1440)],
            scales: vec![100, 125, 150],
        }),
        encoding: Some(Encoding {
            wanted: Wanted {
                bitrate: 30,
                codec: Some(Codec::H265),
                software: false,
                sharp: true,
            },
            using: Some((Codec::H264, true)),
            choices: vec![(Codec::H264, true), (Codec::H264, false)],
        }),
        sound: Some(Sound {
            to_headset: true,
            mic: MicDevice::Failed {
                error: ErrorCode::SOUND,
                detail: "pw-loopback".into(),
            },
        }),
        up_wanted: std::iter::once(UpWanted {
            kind: UpKind::Mic,
            on: true,
        })
        .map(|u| (u.kind, u))
        .collect(),
        focus: Some(Focus {
            display: Some(DisplayId(100)),
        }),
        sound_from: Some(SoundFrom { display: None }),
        scale_auto: Some(state::ScaleAuto {
            auto: vec![DisplayId(3)],
        }),
    }
}

/// The headset's state with something in every part.
pub(crate) fn headset_state() -> HeadsetState {
    HeadsetState {
        caps: Some(HeadsetCaps {
            features: vec![state::caps::headset::PHYSICAL, Feature(77)],
            decoders: vec![Decoder {
                codec: 1,
                max_w: 8192,
                max_h: 4320,
            }],
            streams_max: 12,
            layers_max: 16,
            model: "Quest 3".into(),
            // In a part of its own (see `the_pixel_budget_is_a_part_of_its_own`).
            pixels: Vec::new(),
        }),
        prefs: Some(Prefs {
            notifications: true,
            sound_from_screen: false,
            focus_mode: false,
            gaze_mouse_ms: Some(600),
            no_limits: true,
        }),
        presence: Some(Presence { worn: true }),
        battery: Some(Battery {
            percent: 72,
            charging: true,
        }),
        volume: Some(Volume {
            level: 40,
            muted: false,
            seq: 3,
            ask: 0,
        }),
        up: [
            UpState {
                kind: UpKind::Mic,
                switch: true,
                permission: state::permission::DENIED,
            },
            UpState {
                kind: UpKind::Camera,
                switch: false,
                permission: state::permission::UNKNOWN,
            },
        ]
        .into_iter()
        .map(|u| (u.kind, u))
        .collect(),
        profiles: Some(Profiles {
            list: vec![
                ProfileName::default(),
                ProfileName {
                    name: "Робота".into(),
                },
            ],
            active: Some(1),
        }),
    }
}

pub(crate) fn host_samples() -> Vec<HostMsg> {
    let mut samples = vec![
        HostMsg::Welcome {
            version: 2,
            language: "uk".into(),
        },
        pc_state().snapshot().unwrap(),
        HostMsg::Focus {
            part: Focus { display: None },
        },
        HostMsg::Reply {
            req: ReqId(1),
            outcome: Outcome::Added { id: DisplayId(9) },
        },
        HostMsg::Reply {
            req: ReqId(u32::MAX),
            outcome: Outcome::failed(ErrorCode::LIMIT, "6"),
        },
        HostMsg::Problem {
            error: ErrorCode::ENCODER,
            detail: "vah265enc".into(),
            display: Some(DisplayId(3)),
        },
        HostMsg::StreamStart {
            id: DisplayId(2),
            generation: Gen(7),
            w: 2560,
            h: 1440,
            codec: Codec::H265,
        },
        HostMsg::Frame {
            id: DisplayId(2),
            generation: Gen(7),
            seq: 41,
            keyframe: true,
            pts_us: 123_456_789,
            data: vec![0, 0, 0, 1, 0x40],
        },
        HostMsg::StreamEnd {
            id: DisplayId(2),
            generation: Gen(7),
        },
        HostMsg::Error {
            text: "Не вдалося".into(),
        },
        HostMsg::Audio {
            pcm: vec![0, 1, 2, 3],
        },
        HostMsg::VolumeAsk {
            asked: VolumeAsk {
                level: 55,
                muted: true,
                seen: 3,
                ask: 9,
            },
        },
        HostMsg::Incompatible {
            protocol: 10,
            app: "0.3.0".into(),
        },
        HostMsg::command(ReqId(5), Command::Recenter),
        HostMsg::command(
            ReqId(6),
            Command::Profile {
                name: "Робота".into(),
            },
        ),
        HostMsg::Notice {
            text: "Нове повідомлення".into(),
        },
        HostMsg::FindSecret { secret: [7; 32] },
        HostMsg::PairingRequired,
        HostMsg::PairMsg { msg: [4; 33] },
        HostMsg::PairResult { tag: Some([1; 32]) },
        HostMsg::PairResult { tag: None },
        HostMsg::Ping { seq: 7 },
        HostMsg::Pong { seq: u32::MAX },
        HostMsg::Goodbye { reason: 3 },
    ];
    samples.extend(pc_state().parts());
    samples
}

pub(crate) fn client_samples() -> Vec<ClientMsg> {
    let mut samples = vec![
        ClientMsg::Hello {
            version: 2,
            min: 2,
            app: "0.2.0".into(),
        },
        headset_state().open().unwrap(),
        ClientMsg::RequestKeyframe {
            id: DisplayId(7),
            generation: Gen(1),
        },
        ClientMsg::FrameShown {
            id: DisplayId(1),
            generation: Gen(3),
            seq: 3_600,
        },
        ClientMsg::request(
            ReqId(1),
            Request::AddDisplay {
                mode: Mode::new(3440, 1440),
            },
        ),
        ClientMsg::request(
            ReqId(4),
            Request::SetScale {
                id: DisplayId(3),
                scale: 150,
            },
        ),
        ClientMsg::request(
            ReqId(7),
            Request::AutoScale {
                id: DisplayId(3),
                scale: 175,
            },
        ),
        ClientMsg::request(
            ReqId(8),
            Request::SetScaleAuto {
                id: DisplayId(3),
                on: true,
            },
        ),
        ClientMsg::request(
            ReqId(2),
            Request::SetEncoding {
                bitrate: Some(50),
                codec: None,
                software: Some(false),
                sharp: None,
            },
        ),
        ClientMsg::Request {
            req: ReqId(3),
            asked: Asked::Unknown { kind: 200 },
        },
        ClientMsg::UpStart {
            up: 0,
            kind: UpKind::Mic,
            format: UpFormat::Pcm16 {
                rate: 48_000,
                channels: 1,
            },
        },
        ClientMsg::Up {
            up: 0,
            data: vec![1, 0, 2, 0],
        },
        ClientMsg::UpEnd { up: 0 },
        ClientMsg::Arrange {
            rows: vec![
                Row {
                    entries: vec![Placed {
                        id: DisplayId(3),
                        center: Some(-1280),
                        middle: Some(-1500),
                    }],
                },
                Row {
                    entries: vec![
                        Placed {
                            id: DisplayId(0),
                            center: Some(-2560),
                            middle: Some(0),
                        },
                        Placed {
                            id: DisplayId(100_000),
                            center: None,
                            middle: None,
                        },
                    ],
                },
            ],
        },
        ClientMsg::Reply {
            req: ReqId(5),
            outcome: Outcome::failed(ErrorCode::NO_SUCH_PROFILE, ""),
        },
        ClientMsg::Gaze {
            display: Some(DisplayId(3)),
            u: 100,
            v: 65535,
        },
        ClientMsg::MouseTo {
            id: DisplayId(2),
            u: 1,
            v: 65535,
        },
        ClientMsg::PairBegin {
            headset_shows: true,
            name: "Meta Quest 3".into(),
        },
        ClientMsg::PairMsg { msg: [1; 33] },
        ClientMsg::PairConfirm { tag: [9; 32] },
        ClientMsg::Ping { seq: 0 },
        ClientMsg::Pong { seq: 3 },
        ClientMsg::Goodbye { reason: 1 },
        ClientMsg::Asleep,
        ClientMsg::DecoderPixels {
            part: DecoderPixels {
                budgets: vec![state::PixelBudget {
                    codec: 0,
                    max_pixels: 3840 * 2160,
                }],
            },
        },
    ];
    samples.extend(headset_state().parts());
    samples
}

/// Every message reads back as it was written, and the samples cover every tag.
#[test]
fn every_message_round_trips() {
    let hosts = host_samples();
    let mut tags: Vec<u8> = hosts.iter().map(HostMsg::wire_tag).collect();
    tags.sort_unstable();
    tags.dedup();
    let mut all = HostMsg::TAGS.to_vec();
    all.sort_unstable();
    assert_eq!(tags, all, "a sample for every PC message");
    let mut stream = Vec::new();
    for m in &hosts {
        assert_eq!(&read_host(&host_bytes(m)).unwrap(), m);
        m.write(&mut stream).unwrap();
    }
    let mut r = stream.as_slice();
    for m in &hosts {
        assert_eq!(&host_read(&mut r).unwrap(), m);
    }

    let clients = client_samples();
    let mut tags: Vec<u8> = clients.iter().map(ClientMsg::wire_tag).collect();
    tags.sort_unstable();
    tags.dedup();
    let mut all = ClientMsg::TAGS.to_vec();
    all.sort_unstable();
    assert_eq!(tags, all, "a sample for every headset message");
    for m in &clients {
        assert_eq!(&read_client(&client_bytes(m)).unwrap(), m);
    }
}

/// The media messages are read and written by hand: the bytes are those the
/// declared fields give.
#[test]
fn media_by_hand_matches_the_declared_fields() {
    let frame = HostMsg::Frame {
        id: DisplayId(4),
        generation: Gen(2),
        seq: 9,
        keyframe: false,
        pts_us: 77,
        data: vec![9; 1000],
    };
    let audio = HostMsg::Audio { pcm: vec![5; 1920] };
    for m in [&frame, &audio] {
        assert_eq!(host_bytes(m), generic_host(m));
        let mut payload = Vec::new();
        m.put_fields(&mut payload).unwrap();
        assert_eq!(&HostMsg::decode(m.wire_tag(), &payload).unwrap(), m);
    }
    assert_eq!(frame.wire_tag(), tag::FRAME);
    assert_eq!(audio.wire_tag(), tag::AUDIO);
    let mic = ClientMsg::Up {
        up: 2,
        data: vec![3; 960],
    };
    assert_eq!(mic.wire_tag(), tag::UP);
    let mut payload = Vec::new();
    mic.put_fields(&mut payload).unwrap();
    assert_eq!(client_bytes(&mic), framed(tag::UP, &payload));
    assert_eq!(read_client(&client_bytes(&mic)).unwrap(), mic);
    // A frame whose keyframe byte is not a boolean: after the header, the id, the
    // generation and the number.
    let mut bad = host_bytes(&frame);
    bad[5 + 12] = 2;
    assert_wire(read_host(&bad), WireError::BadBool(2));
    // Shorter than its header.
    assert_wire(
        read_host(&framed(tag::FRAME, &[4, 0])),
        WireError::Truncated,
    );
}

/// The version handshake keeps the bytes of protocol 1 forever, so every version can
/// tell the user which side to update.
#[test]
fn the_version_handshake_is_frozen() {
    assert_eq!(
        ClientMsg::Hello {
            version: 0,
            min: 0,
            app: String::new()
        }
        .wire_tag(),
        tag::HELLO
    );
    let hello = ClientMsg::Hello {
        version: 1,
        min: 1,
        app: "0.1.1".into(),
    };
    #[rustfmt::skip]
    let hello_bytes = [
        101, 13, 0, 0, 0,
        1, 0, 0, 0,
        1, 0, 0, 0,
        b'0', b'.', b'1', b'.', b'1',
    ];
    assert_eq!(client_bytes(&hello), hello_bytes);
    assert_eq!(read_client(&hello_bytes).unwrap(), hello);
    assert_eq!(
        ClientMsg::read_first(&mut hello_bytes.as_slice()).unwrap(),
        Some(hello)
    );
    // What a headset from before protocol 2 may send first: a request to pair, as
    // protocol 1 wrote it (whether it shows the code, then its name).
    let mut old_pairing = vec![109, 11, 0, 0, 0, 1];
    old_pairing.extend_from_slice(b"Meta Quest");
    let mut r = old_pairing.as_slice();
    assert_eq!(ClientMsg::read_first(&mut r).unwrap(), None);
    assert!(r.is_empty(), "skipped whole");

    let welcome = HostMsg::Welcome {
        version: 2,
        language: "uk".into(),
    };
    let welcome_bytes = [1, 6, 0, 0, 0, 2, 0, 0, 0, b'u', b'k'];
    assert_eq!(host_bytes(&welcome), welcome_bytes);
    assert_eq!(read_host(&welcome_bytes).unwrap(), welcome);

    let incompatible = HostMsg::Incompatible {
        protocol: 2,
        app: "0.2.0".into(),
    };
    #[rustfmt::skip]
    let incompatible_bytes = [
        17, 9, 0, 0, 0,
        2, 0, 0, 0,
        b'0', b'.', b'2', b'.', b'0',
    ];
    assert_eq!(host_bytes(&incompatible), incompatible_bytes);
    assert_eq!(read_host(&incompatible_bytes).unwrap(), incompatible);

    let error = HostMsg::Error {
        text: "Update".into(),
    };
    let error_bytes = [6, 6, 0, 0, 0, b'U', b'p', b'd', b'a', b't', b'e'];
    assert_eq!(host_bytes(&error), error_bytes);
    assert_eq!(read_host(&error_bytes).unwrap(), error);
}

#[test]
fn messages_are_read_strictly() {
    // Bytes left over after a known message.
    let end = HostMsg::StreamEnd {
        id: DisplayId(1),
        generation: Gen(1),
    };
    let mut longer = host_bytes(&end);
    longer[1] += 1;
    longer.push(0);
    assert_wire(read_host(&longer), WireError::Trailing(1));
    // A codec this build does not know, in a stream's start: after the id, the
    // generation and the size.
    let mut payload = Vec::new();
    HostMsg::StreamStart {
        id: DisplayId(1),
        generation: Gen(1),
        w: 640,
        h: 360,
        codec: Codec::H264,
    }
    .put_fields(&mut payload)
    .unwrap();
    payload[16] = 9;
    assert_wire(
        read_host(&framed(2, &payload)),
        WireError::UnknownTag {
            what: "Codec",
            tag: 9,
        },
    );
    // A boolean that is neither 0 nor 1.
    assert_wire(read_client(&framed(115, &[2])), WireError::BadBool(2));
    // `Option` with a tag other than 0 or 1.
    assert_wire(
        read_host(&framed(16, &[2, 0])),
        WireError::UnknownTag {
            what: "Option",
            tag: 2,
        },
    );
    // A text that is not UTF-8.
    assert_wire(
        read_host(&framed(20, &[2, 0, 0xff, 0xfe])),
        WireError::BadUtf8,
    );
    // Frozen text is UTF-8 too.
    assert_wire(read_host(&framed(6, &[0xc3])), WireError::BadUtf8);
    // An arrangement cut off in the middle of an entry: one row, one entry, half its id.
    assert_wire(
        read_client(&framed(133, &[1, 0, 1, 0, 7, 0])),
        WireError::Truncated,
    );
    // A known kind of request with a byte too many.
    let mut add = Vec::new();
    ClientMsg::request(ReqId(1), Request::FindMouse)
        .put_fields(&mut add)
        .unwrap();
    add.push(0);
    assert_wire(read_client(&framed(131, &add)), WireError::Trailing(1));
}

/// A request of a kind this build does not know reads with its number, so it can be
/// answered: its fields are skipped.
#[test]
fn a_request_of_an_unknown_kind_keeps_its_number() {
    let mut payload = 77u32.to_le_bytes().to_vec();
    payload.extend_from_slice(&[250, 1, 2, 3, 4]);
    assert_eq!(
        read_client(&framed(131, &payload)).unwrap(),
        ClientMsg::Request {
            req: ReqId(77),
            asked: Asked::Unknown { kind: 250 },
        }
    );
}

#[test]
fn lists_cannot_hold_more_than_there_can_be() {
    let max = MAX_DISPLAYS;
    // One display more than a list may hold.
    let many = Displays {
        displays: (0..=max as u32)
            .map(|i| display(i, DisplayKind::Virtual))
            .collect(),
        primary: None,
    };
    let mut payload = Vec::new();
    wire::Limited::put(&many.displays, &mut payload, usize::MAX).unwrap();
    payload.push(0);
    assert_wire(read_host(&framed(31, &payload)), too_long(max + 1, max));
    // 100 empty rows: small on the wire, but not an arrangement.
    let mut payload = vec![100, 0];
    payload.extend(std::iter::repeat_n(0, 200));
    assert_wire(read_client(&framed(133, &payload)), too_long(100, max));
    // Each row within its limit, but 34 displays in all.
    let entry = |id| Placed {
        id: DisplayId(id),
        center: None,
        middle: None,
    };
    let row = |from: u32| Row {
        entries: (from..from + 17).map(entry).collect(),
    };
    let mut payload = Vec::new();
    wire::Limited::put(&vec![row(0), row(17)], &mut payload, max).unwrap();
    assert_wire(read_client(&framed(133, &payload)), too_long(34, max));
    // A display in two places.
    let twice = ClientMsg::Arrange {
        rows: vec![
            Row {
                entries: vec![entry(0), entry(1)],
            },
            Row {
                entries: vec![entry(1)],
            },
        ],
    };
    let err = read_client(&client_bytes(&twice)).unwrap_err();
    assert_eq!(wire_error(&err), Some(&WireError::Repeated(1)));
    // A name longer than a name may be.
    let mut payload = vec![1, 65, 0];
    payload.extend(std::iter::repeat_n(b'a', 65));
    assert_wire(read_client(&framed(109, &payload)), too_long(65, 64));
}

/// A message over its limits is not sent: an error for this connection, not a crash.
#[test]
fn a_message_over_its_limits_is_not_sent() {
    let too_many = HostMsg::Displays {
        part: Displays {
            displays: (0..=MAX_DISPLAYS as u32)
                .map(|i| display(i, DisplayKind::Virtual))
                .collect(),
            primary: None,
        },
    };
    let mut out = Vec::new();
    let err = too_many.write(&mut out).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(
        wire_error(&err),
        Some(&WireError::TooLong {
            len: MAX_DISPLAYS + 1,
            max: MAX_DISPLAYS
        })
    );
    assert!(out.is_empty(), "nothing half written");
    let name = ClientMsg::PairBegin {
        headset_shows: true,
        name: "x".repeat(65),
    };
    let err = name.write(&mut out).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
}

/// A desktop notification of up to 8 KiB goes through whole; a longer one is not
/// sent, and not read.
#[test]
fn a_long_notice_goes_through() {
    assert_eq!(MAX_TEXT, 8 * 1024);
    for len in [4 * 1024, MAX_TEXT] {
        let notice = HostMsg::Notice {
            text: "n".repeat(len),
        };
        assert_eq!(
            read_host(&host_bytes(&notice)).unwrap(),
            notice,
            "{len} bytes"
        );
    }
    let too_long_notice = HostMsg::Notice {
        text: "n".repeat(MAX_TEXT + 1),
    };
    let err = too_long_notice.write(&mut Vec::new()).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    let mut payload = Vec::new();
    wire::Limited::put(&"n".repeat(MAX_TEXT + 1), &mut payload, usize::MAX).unwrap();
    assert_wire(
        read_host(&framed(20, &payload)),
        too_long(MAX_TEXT + 1, MAX_TEXT),
    );
}

/// What the PC sets aside for a headset: one message of 64 KiB before the session,
/// one on each of 32 lanes in it, and never more than all of those at once.
#[test]
fn what_a_pc_accepts_from_a_headset() {
    assert_eq!(
        ClientMsg::limits(false),
        Limits {
            message: 64 * 1024,
            lanes: 1,
            in_flight: 64 * 1024,
        }
    );
    assert_eq!(
        ClientMsg::limits(true),
        Limits {
            message: 64 * 1024,
            lanes: 32,
            in_flight: 32 * 64 * 1024,
        }
    );
}

/// Before the session is open the PC is not trusted yet: its messages are kept small.
#[test]
fn a_pc_message_before_the_session_cannot_be_huge() {
    let big = host_bytes(&HostMsg::Error {
        text: "x".repeat(100 * 1024),
    });
    assert!(host_read(&mut big.as_slice()).is_ok());
    let err = HostMsg::read_setup(&mut big.as_slice()).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    let required = host_bytes(&HostMsg::PairingRequired);
    assert_eq!(
        HostMsg::read_setup(&mut required.as_slice()).unwrap(),
        HostMsg::PairingRequired
    );
}

#[test]
fn a_headset_message_cannot_be_huge() {
    let mut big = vec![133];
    big.extend_from_slice(&(1u32 << 20).to_le_bytes());
    big.extend(std::iter::repeat_n(0u8, 1 << 20));
    let too_large = |big: &[u8]| {
        let err = client_read(&mut &big[..]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(err.to_string(), "message too large");
    };
    too_large(&big);
    // Not even one this build would skip.
    big[0] = 250;
    too_large(&big);
}

/// A message from a newer peer is skipped and the next one still reads.
#[test]
fn skips_unknown_messages() {
    let focus = HostMsg::Focus {
        part: Focus {
            display: Some(DisplayId(1)),
        },
    };
    let mut buf = framed(250, &[1, 2, 3]);
    focus.write(&mut buf).unwrap();
    let mut r = buf.as_slice();
    assert_eq!(host_read(&mut r).unwrap(), focus);
    assert!(r.is_empty());

    let mut buf = framed(251, &[7; 10]);
    let end = ClientMsg::UpEnd { up: 1 };
    end.write(&mut buf).unwrap();
    let mut r = buf.as_slice();
    assert_eq!(client_read(&mut r).unwrap(), end);
    // An unknown message cut off is an error, not a skip past the end.
    let err = host_read(&mut framed(250, &[1, 2, 3])[..6].as_ref()).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

/// A goodbye with a reason this build does not know reads as "other": new reasons
/// need no new protocol version.
#[test]
fn goodbye_reasons_round_trip_and_unknown_ones_read_as_other() {
    for r in [
        PcGoodbye::Other,
        PcGoodbye::Quit,
        PcGoodbye::TakenOff,
        PcGoodbye::Forgotten,
        PcGoodbye::Replaced,
        PcGoodbye::Failed,
    ] {
        assert_eq!(PcGoodbye::from_code(r.code()), r);
    }
    for r in [
        HeadsetGoodbye::Other,
        HeadsetGoodbye::Quit,
        HeadsetGoodbye::OtherPc,
        HeadsetGoodbye::Forgotten,
    ] {
        assert_eq!(HeadsetGoodbye::from_code(r.code()), r);
    }
    let unknown = read_host(&framed(27, &[200])).unwrap();
    assert_eq!(unknown, HostMsg::Goodbye { reason: 200 });
    assert_eq!(PcGoodbye::from_code(200), PcGoodbye::Other);
    assert_eq!(HeadsetGoodbye::from_code(9), HeadsetGoodbye::Other);
    assert_eq!((PcGoodbye::Quit.code(), PcGoodbye::Failed.code()), (1, 5));
    assert_eq!(HeadsetGoodbye::Forgotten.code(), 3);
}

#[test]
fn version_ranges() {
    assert!(compatible(PROTOCOL_VERSION, MIN_PROTOCOL));
    // A newer peer that still understands us.
    assert!(compatible(PROTOCOL_VERSION + 3, PROTOCOL_VERSION));
    // A newer peer that dropped what we speak.
    assert!(!compatible(PROTOCOL_VERSION + 3, PROTOCOL_VERSION + 1));
    // An older peer than we still support: 0.1.x speaks protocol 1.
    assert!(!compatible(MIN_PROTOCOL - 1, 0));
    assert!(!compatible(1, 1));
}

/// Development builds carry the revision of their messages in their app version, and
/// only the same revision reads them; which side is newer goes by it too.
#[test]
fn development_revisions() {
    let ours = app_version("0.2.0");
    assert_eq!(revision_of(&ours), PROTO_REVISION);
    assert!(same_revision(&ours));
    assert_eq!(revision_of("0.2.0-dev+p7"), Some(7));
    assert_eq!(revision_of("0.2.0"), None);
    assert_eq!(revision_of("0.2.0-dev+px"), None);
    if let Some(n) = PROTO_REVISION {
        assert_eq!(ours, format!("0.2.0-dev+p{n}"));
        assert!(!same_revision(&format!("0.2.0-dev+p{}", n + 1)));
        assert!(
            !same_revision("0.2.0"),
            "a released build reads other messages"
        );
        assert!(peer_is_newer(
            PROTOCOL_VERSION,
            &format!("0.2.0-dev+p{}", n + 1)
        ));
        assert!(!peer_is_newer(
            PROTOCOL_VERSION,
            &format!("0.2.0-dev+p{}", n - 1)
        ));
    }
    assert!(peer_is_newer(PROTOCOL_VERSION + 1, "9.0.0"));
    assert!(!peer_is_newer(PROTOCOL_VERSION - 1, "0.1.3"));
}

/// The tags of earlier development builds never come back.
#[test]
fn retired_tags_stay_unused() {
    for tag in RETIRED_HOST_TAGS {
        assert!(
            !HostMsg::TAGS.contains(tag),
            "PC message tag {tag} is retired"
        );
    }
    for tag in RETIRED_CLIENT_TAGS {
        assert!(
            !ClientMsg::TAGS.contains(tag),
            "headset message tag {tag} is retired"
        );
    }
}

/// A snapshot holds every part, read by the same code as the part on its own: the
/// state from it is the state the parts make one by one. A part this build does not
/// know is skipped; one twice is an error.
#[test]
fn a_snapshot_is_the_parts_together() {
    let state = pc_state();
    let HostMsg::Snapshot { parts } = state.snapshot().unwrap() else {
        panic!("not a snapshot")
    };
    assert_eq!(parts[0].tag, 31, "the displays first");
    assert_eq!(PcState::from_snapshot(&parts).unwrap(), state);
    let mut one_by_one = PcState::default();
    for msg in state.parts() {
        let bytes = host_bytes(&msg);
        assert_eq!(one_by_one.apply(read_host(&bytes).unwrap()), Ok(None));
    }
    assert_eq!(one_by_one, state);
    // Read back through the wire, whole.
    let HostMsg::Snapshot { parts: back } =
        read_host(&host_bytes(&state.snapshot().unwrap())).unwrap()
    else {
        panic!("not a snapshot")
    };
    assert_eq!(PcState::from_snapshot(&back).unwrap(), state);
    // A newer PC's part, and a message that is not a part, are skipped.
    let mut more = parts.clone();
    more.insert(
        1,
        Part {
            tag: 250,
            body: vec![1, 2, 3],
        },
    );
    more.push(Part {
        tag: 25,
        body: vec![0; 4],
    });
    assert_eq!(PcState::from_snapshot(&more).unwrap(), state);
    let mut twice = parts.clone();
    twice.push(parts[0].clone());
    assert!(PcState::from_snapshot(&twice).is_err());
    // Only what changed goes, the displays first.
    let mut moved = state.clone();
    moved.focus = Some(Focus {
        display: Some(DisplayId(3)),
    });
    moved.displays.as_mut().unwrap().primary = None;
    let changed: Vec<u8> = moved
        .changed(&state)
        .iter()
        .map(HostMsg::wire_tag)
        .collect();
    assert_eq!(changed, [31, 16]);
    assert!(state.changed(&state).is_empty());
}

/// The headset's state comes the same way in `Open`, which must name what it can do.
#[test]
fn open_is_the_headsets_parts_together() {
    let state = headset_state();
    let ClientMsg::Open { parts } = read_client(&client_bytes(&state.open().unwrap())).unwrap()
    else {
        panic!("not Open")
    };
    assert_eq!(HeadsetState::from_open(&parts).unwrap(), state);
    let no_caps = HeadsetState {
        caps: None,
        ..state
    };
    let ClientMsg::Open { parts } = no_caps.open().unwrap() else {
        panic!("not Open")
    };
    assert!(HeadsetState::from_open(&parts).is_err());
}

/// What the other side says is checked: breaking the protocol's rules ends the
/// session like a message that does not parse.
#[test]
fn values_breaking_the_rules_are_refused() {
    let mut state = pc_state();
    // A primary display that is not listed.
    state.displays.as_mut().unwrap().primary = Some(DisplayId(55));
    let msg = HostMsg::Displays {
        part: state.displays.clone().unwrap(),
    };
    assert!(matches!(
        wire_error(&read_host(&host_bytes(&msg)).unwrap_err()),
        Some(WireError::Invalid(_))
    ));
    // The mouse on a display that is not listed.
    let mut state = pc_state();
    state.focus = Some(Focus {
        display: Some(DisplayId(55)),
    });
    let HostMsg::Snapshot { parts } = state.snapshot().unwrap() else {
        panic!("not a snapshot")
    };
    assert!(PcState::from_snapshot(&parts).is_err());
    // A volume over 100 %, a microphone the PC does not take.
    let loud = ClientMsg::Volume {
        part: Volume {
            level: 101,
            muted: false,
            seq: 0,
            ask: 0,
        },
    };
    assert!(read_client(&client_bytes(&loud)).is_err());
    let odd = ClientMsg::UpStart {
        up: 0,
        kind: UpKind::Mic,
        format: UpFormat::Pcm16 {
            rate: 44_100,
            channels: 1,
        },
    };
    assert!(read_client(&client_bytes(&odd)).is_err());
    // A decoder of no size.
    let mut caps = headset_state().caps.unwrap();
    caps.decoders[0].max_h = 0;
    let msg = ClientMsg::HeadsetCaps { part: caps };
    assert!(read_client(&client_bytes(&msg)).is_err());
}

/// The largest snapshot (32 displays with the longest texts) is far below a frame:
/// a check that nothing in it grows out of proportion.
#[test]
fn the_largest_snapshot_is_small() {
    let mut state = pc_state();
    let window = Display {
        kind: DisplayKind::Window {
            title: "t".repeat(state::MAX_TITLE),
        },
        name: "n".repeat(state::MAX_DISPLAY_NAME),
        state: DisplayState::Failed {
            error: ErrorCode::OTHER,
            detail: "d".repeat(MAX_DETAIL),
        },
        ..display(0, DisplayKind::Virtual)
    };
    state.displays = Some(Displays {
        displays: (0..MAX_DISPLAYS as u32)
            .map(|i| Display {
                id: DisplayId(i),
                ..window.clone()
            })
            .collect(),
        primary: None,
    });
    state.focus = None;
    let bytes = host_bytes(&state.snapshot().unwrap());
    assert!(bytes.len() < 32 * 1024, "{} bytes", bytes.len());
}

/// The parts and requests of "Auto" (feature 17) are additions: an older headset
/// skips the part, and an older PC reads the requests as kinds it does not know.
#[test]
fn auto_scale_is_additive() {
    use crate::request::Asked;
    use crate::wire::{decode, encode};
    let part = HostMsg::ScaleAuto {
        part: state::ScaleAuto {
            auto: vec![DisplayId(3), DisplayId(9)],
        },
    };
    let bytes = host_bytes(&part);
    assert_eq!(bytes[0], 44);
    assert_eq!(read_host(&bytes).unwrap(), part);
    assert!(!RETIRED_HOST_TAGS.contains(&44));
    let mut state = PcState::default();
    assert_eq!(state.apply(part).unwrap(), None);
    assert!(state.scale_auto.as_ref().unwrap().has(DisplayId(9)));
    assert!(!state.scale_auto.unwrap().has(DisplayId(4)));
    for (asked, kind) in [
        (
            Request::AutoScale {
                id: DisplayId(3),
                scale: 125,
            },
            14,
        ),
        (
            Request::SetScaleAuto {
                id: DisplayId(3),
                on: false,
            },
            15,
        ),
    ] {
        let bytes = encode(&Asked::Known(asked.clone())).unwrap();
        assert_eq!(bytes[0], kind);
        assert_eq!(
            decode::<Asked<Request>>(&bytes).unwrap(),
            Asked::Known(asked)
        );
    }
    assert_eq!(state::caps::pc::SCALE_IN_PLACE, Feature(17));
}

/// A small random number generator, the same numbers on every run.
pub(crate) struct Seeded(pub(crate) u64);

impl Seeded {
    pub(crate) fn next(&mut self) -> u64 {
        // xorshift64
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    pub(crate) fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// `bytes` changed a little: bytes overwritten, flipped, cut off or added.
pub(crate) fn mutate(rng: &mut Seeded, bytes: &[u8]) -> Vec<u8> {
    let mut b = bytes.to_vec();
    for _ in 0..1 + rng.below(3) {
        match rng.below(5) {
            0 if !b.is_empty() => {
                let i = rng.below(b.len());
                b[i] = rng.next() as u8;
            }
            1 if !b.is_empty() => {
                let i = rng.below(b.len());
                b[i] ^= 1 << rng.below(8);
            }
            2 => b.truncate(rng.below(b.len() + 1)),
            3 => {
                let i = rng.below(b.len() + 1);
                b.insert(i, rng.next() as u8);
            }
            // The length field, where most of the framing is decided.
            _ if b.len() >= 5 => {
                let i = 1 + rng.below(4);
                b[i] = rng.next() as u8;
            }
            _ => {}
        }
    }
    b
}

/// Damaged messages never crash a reader, and whatever still reads is written back
/// to exactly the bytes it was read from: there is one encoding for each message.
#[test]
fn damaged_messages_never_panic_and_read_back_exactly() {
    let mut rng = Seeded(0x5eed_1234_abcd_0001);
    let hosts: Vec<Vec<u8>> = host_samples().iter().map(host_bytes).collect();
    let clients: Vec<Vec<u8>> = client_samples().iter().map(client_bytes).collect();
    let (mut read, mut refused) = (0, 0);
    for _ in 0..20_000 {
        let pick = rng.below(hosts.len());
        let bytes = mutate(&mut rng, &hosts[pick]);
        let mut r = bytes.as_slice();
        match host_read(&mut r) {
            Ok(msg) => {
                read += 1;
                let taken = &bytes[..bytes.len() - r.len()];
                // An unknown message skipped before it is not written back, and an
                // unknown part of a snapshot is not either: what was read is.
                let again = host_bytes(&msg);
                assert!(
                    taken.ends_with(&again) || read_host(&again).unwrap() == msg,
                    "{msg:?} from {bytes:?}"
                );
            }
            Err(_) => refused += 1,
        }
        let _ = HostMsg::read_setup(&mut bytes.as_slice());

        let pick = rng.below(clients.len());
        let bytes = mutate(&mut rng, &clients[pick]);
        let mut r = bytes.as_slice();
        match client_read(&mut r) {
            Ok(msg) => {
                read += 1;
                let taken = &bytes[..bytes.len() - r.len()];
                // A request of an unknown kind keeps only its kind.
                let again = client_bytes(&msg);
                assert!(
                    taken.ends_with(&again) || read_client(&again).unwrap() == msg,
                    "{msg:?} from {bytes:?}"
                );
            }
            Err(_) => refused += 1,
        }
        let mut r = bytes.as_slice();
        if let Ok(Some(msg)) = ClientMsg::read_first(&mut r) {
            assert_eq!(&bytes[..bytes.len() - r.len()], client_bytes(&msg));
        }
    }
    // Both happen, or the mutations test nothing.
    assert!(
        read > 1000 && refused > 1000,
        "{read} read, {refused} refused"
    );
}

/// A stream fits a headset within its decoder for the codec; H.264 without a decoder
/// named fits at any size, another codec without one never.
#[test]
fn a_stream_fits_the_decoder_of_its_codec() {
    use crate::state::HeadsetCaps;
    let caps = HeadsetCaps {
        decoders: vec![Decoder {
            codec: 1,
            max_w: 8192,
            max_h: 4320,
        }],
        ..HeadsetCaps::default()
    };
    assert!(caps.fits(Codec::H265, 5120, 1440));
    assert!(!caps.fits(Codec::H265, 8193, 1440));
    assert!(caps.fits(Codec::H264, 5120, 1440), "nothing said of H.264");
    assert!(!caps.fits(Codec::Av1, 640, 360));
    assert!(
        caps.fits(Codec::H265, 8192, 4320),
        "no budget: the sides alone"
    );
    let h264 = HeadsetCaps {
        decoders: vec![Decoder {
            codec: 0,
            max_w: 4096,
            max_h: 4096,
        }],
        ..HeadsetCaps::default()
    };
    // Quest 3: 8192 by 4320, but at 60 frames a second only 8192x1600 worth of
    // pixels for H.265, 8192x2880 for H.264.
    let quest = HeadsetCaps {
        decoders: vec![
            Decoder {
                codec: 0,
                max_w: 8192,
                max_h: 4320,
            },
            Decoder {
                codec: 1,
                max_w: 8192,
                max_h: 4320,
            },
        ],
        pixels: vec![
            state::PixelBudget {
                codec: 0,
                max_pixels: 8192 * 2880,
            },
            state::PixelBudget {
                codec: 1,
                max_pixels: 8192 * 1600,
            },
        ],
        ..HeadsetCaps::default()
    };
    for codec in [Codec::H264, Codec::H265] {
        for (w, h) in [
            (3840, 2160),
            (1440, 2560),
            (2560, 1440),
            (5120, 1440),
            (8192, 1600),
        ] {
            assert!(quest.fits(codec, w, h), "{codec:?} {w}x{h}");
        }
        assert!(!quest.fits(codec, 7680, 4320), "{codec:?} 8K");
    }
    // A decoder that cannot do 4K at 60.
    let weak = HeadsetCaps {
        pixels: vec![state::PixelBudget {
            codec: 0,
            max_pixels: 2560 * 1600,
        }],
        ..h264.clone()
    };
    assert!(!weak.fits(Codec::H264, 3840, 2160));
    assert!(weak.fits(Codec::H264, 2560, 1440));
    assert!(!h264.fits(Codec::H264, 5120, 1440));
    assert!(h264.fits(Codec::H264, 4096, 2160));
}

/// The pixel budget travels in a part of its own: `HeadsetCaps` keeps its bytes (an
/// older PC reads it as before and skips the new part), and the budget comes back
/// into the caps whichever of the two comes first.
#[test]
fn the_pixel_budget_is_a_part_of_its_own() {
    let mut state = headset_state();
    if let Some(caps) = &mut state.caps {
        caps.pixels = vec![state::PixelBudget {
            codec: 1,
            max_pixels: 8192 * 1600,
        }];
    }
    let caps = state.caps.clone().unwrap();
    let bare = HeadsetCaps {
        pixels: Vec::new(),
        ..caps.clone()
    };
    let bytes = |c: &HeadsetCaps| client_bytes(&ClientMsg::HeadsetCaps { part: c.clone() });
    assert_eq!(bytes(&caps), bytes(&bare), "the caps' bytes as before");
    let ClientMsg::Open { mut parts } = state.open().unwrap() else {
        panic!("not Open")
    };
    assert_eq!(parts[1].tag, 144);
    parts.swap(0, 1);
    assert_eq!(HeadsetState::from_open(&parts).unwrap(), state);
    // Without it: the caps as an older headset says them.
    parts.remove(0);
    let older = HeadsetState::from_open(&parts).unwrap();
    assert_eq!(older.caps, Some(bare));
    // A budget of nothing is the headset's bug.
    let zero = ClientMsg::DecoderPixels {
        part: DecoderPixels {
            budgets: vec![state::PixelBudget {
                codec: 1,
                max_pixels: 0,
            }],
        },
    };
    assert!(read_client(&client_bytes(&zero)).is_err());
}
