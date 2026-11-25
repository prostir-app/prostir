use super::*;
use std::io::Write;
use std::net::TcpListener;

use prostir_proto::handshake::Answer;
use prostir_proto::link::tcp::Plain;
use prostir_proto::link::{StreamReader, StreamWriter};

/// The headset's end of a connection in a test.
struct Headset {
    link: Link,
    packets: Assembler,
    id: Identity,
}

impl Headset {
    fn connect(stream: TcpStream, id: Identity) -> Self {
        let link = NoiseTcp::connect(stream, &id, Arc::new(Plain)).unwrap();
        Self {
            link,
            packets: HostMsg::assembler(),
            id,
        }
    }

    fn send(&mut self, msg: ClientMsg) {
        let msg = msg.encode().unwrap();
        packet::send_whole(&mut *self.link.sender, lane::CONTROL, msg, FRAGMENT).unwrap();
    }

    fn recv(&mut self) -> io::Result<HostMsg> {
        HostMsg::recv(&mut *self.link.receiver, &mut self.packets)
    }

    /// Says hello as this version does; packets follow a welcome.
    fn hello(&mut self, version: u32) -> Answer {
        let hello = ClientMsg::Hello {
            version,
            min: version,
            app: prostir_proto::app_version("0.2.0"),
        };
        // Alone in its record, as protocol 2 says hello.
        let mut bytes = Vec::new();
        hello.write(&mut bytes).unwrap();
        self.write_as_v1(&bytes);
        handshake::read_answer(&mut *self.link.receiver).unwrap()
    }

    /// Bytes as a headset of protocol 1 (0.1.x) writes them: one stream.
    fn write_as_v1(&mut self, bytes: &[u8]) {
        StreamWriter(&mut *self.link.sender)
            .write_all(bytes)
            .unwrap();
    }

    /// The messages a headset of protocol 1 reads, the way it reads them: tag,
    /// length, payload, from one stream.
    fn read_as_v1(&mut self) -> Vec<(u8, Vec<u8>)> {
        use std::io::Read;
        let mut r = StreamReader::new(&mut *self.link.receiver);
        let mut msgs = Vec::new();
        let mut header = [0u8; 5];
        while r.read_exact(&mut header).is_ok() {
            let len = u32::from_le_bytes(header[1..].try_into().unwrap());
            let mut payload = vec![0u8; len as usize];
            r.read_exact(&mut payload).unwrap();
            msgs.push((header[0], payload));
        }
        msgs
    }
}

/// The headset's side of a pairing attempt, as the Quest app does it.
fn headset(mut h: Headset, code: &str) -> bool {
    let peer = h.link.peer.clone();
    let (p, msg) = Pairing::start(code);
    h.send(ClientMsg::PairMsg { msg });
    let HostMsg::PairMsg { msg: theirs } = h.recv().unwrap() else {
        panic!("expected PairMsg")
    };
    let key = p.finish(&theirs).unwrap();
    h.send(ClientMsg::PairConfirm {
        tag: confirmation(&key, "headset", &peer, &h.id.public),
    });
    let HostMsg::PairResult { tag } = h.recv().unwrap() else {
        panic!("expected PairResult")
    };
    tag.is_some_and(|tag| tags_match(&tag, &confirmation(&key, "pc", &peer, &h.id.public)))
}

fn identity(name: &str) -> Identity {
    let dir = std::env::temp_dir().join(format!("prostir-{name}-{}", std::process::id()));
    let id = Identity::load_or_create(&dir.join("key")).unwrap();
    std::fs::remove_dir_all(dir).unwrap();
    id
}

fn run(pc_code: &'static str, headset_code: &'static str) -> (bool, bool) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let id = identity(&format!("pair-h-{pc_code}{headset_code}"));
    let h = std::thread::spawn(move || {
        headset(
            Headset::connect(TcpStream::connect(addr).unwrap(), id),
            headset_code,
        )
    });
    let (s, _) = l.accept().unwrap();
    let id = identity(&format!("pair-p-{pc_code}{headset_code}"));
    let link = NoiseTcp::accept(s, &id, Arc::new(Plain)).unwrap();
    let peer = link.peer.clone();
    let mut conn = Connection {
        link,
        packets: ClientMsg::assembler(),
        app: String::new(),
    };
    let pc = pair(&mut conn, &id, &peer, pc_code).unwrap();
    (pc, h.join().unwrap())
}

#[test]
fn same_code_pairs_both_sides() {
    assert_eq!(run("123456", "123456"), (true, true));
}

#[test]
fn different_code_pairs_neither() {
    assert_eq!(run("123456", "654321"), (false, false));
}

/// A PC in a test: its keys in a directory of its own.
struct TestPc {
    dir: PathBuf,
    security: Security,
}

impl TestPc {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("prostir-{name}-p-{}", std::process::id()));
        let security = Security {
            identity: Identity::load_or_create(&dir.join("key")).unwrap(),
            trusted: Mutex::new(TrustStore::load(&dir.join("trusted"))),
            find: Mutex::new(FindSecrets {
                current: [5; 32],
                ..FindSecrets::default()
            }),
            find_file: find_secret_file(&dir),
        };
        Self { dir, security }
    }
}

/// The find secret is made once and kept; a file that does not read is an error,
/// not a new secret; a new one replaces it in the file and in memory.
#[test]
fn the_find_secret_is_kept_and_made_anew_on_purpose_only() {
    let pc = TestPc::new("find-secret");
    let path = find_secret_file(&pc.dir);
    let first = load_find_secret(&path).unwrap();
    assert_eq!(load_find_secret(&path).unwrap(), first, "kept");
    let security = Security {
        identity: Identity::load_or_create(&pc.dir.join("key")).unwrap(),
        trusted: Mutex::new(TrustStore::load(&pc.dir.join("trusted"))),
        find: Mutex::new(load_find_secrets(&path).unwrap()),
        find_file: path.clone(),
    };
    let second = security.new_find_secret(&[], |_| false);
    assert_ne!(second, first);
    assert_eq!(security.find_secret(), second);
    assert_eq!(load_find_secret(&path).unwrap(), second, "written");
    std::fs::write(&path, "not hex\n").unwrap();
    assert!(load_find_secret(&path).is_err(), "not replaced silently");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "not hex\n");
}

/// Forgetting a headset makes a new secret; the old one stays valid for each trusted
/// headset that was not in a session then, until it gets the new one or is forgotten
/// too, and that is kept across a restart (in a file of its own, so `find_secret`
/// reads as before).
#[test]
fn an_old_find_secret_stays_valid_for_headsets_that_were_off() {
    let pc = TestPc::new("find-owed");
    let path = find_secret_file(&pc.dir);
    let (on, off, other_off, gone) = ([1; 32], [2; 32], [3; 32], [4; 32]);
    let first = load_find_secret(&path).unwrap();
    let security = Security {
        identity: Identity::load_or_create(&pc.dir.join("key")).unwrap(),
        trusted: Mutex::new(TrustStore::load(&pc.dir.join("trusted"))),
        find: Mutex::new(load_find_secrets(&path).unwrap()),
        find_file: path.clone(),
    };
    for key in [on, off, other_off] {
        security.trusted.lock().unwrap().add(key, "h").unwrap();
    }
    // `gone` was forgotten (no longer trusted); `on` is in a session.
    let second = security.new_find_secret(&[gone], |k| *k == on);
    let valid = security.find_secrets();
    assert_eq!(valid.current, second);
    assert_eq!(
        valid.stale(),
        [first],
        "still valid for the headsets that were off"
    );
    let kept = load_find_secrets(&path).unwrap();
    assert_eq!(kept.current, second);
    assert_eq!(
        kept.owed.keys().copied().collect::<Vec<_>>(),
        [off, other_off]
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap().trim().len(),
        64,
        "find_secret holds the current one alone"
    );
    // Forgetting another while `off` is still away: `off` keeps the first one valid,
    // `other_off` (forgotten now) does not.
    security.trusted.lock().unwrap().remove(&other_off).unwrap();
    let third = security.new_find_secret(&[other_off], |k| *k == on);
    let kept = load_find_secrets(&path).unwrap();
    assert_eq!(kept.current, third);
    assert_eq!(kept.owed.get(&off).map(|(s, _)| *s), Some(first));
    assert!(!kept.owed.contains_key(&other_off));
    assert_eq!(security.find_secrets().stale(), [first]);
    // Should more old secrets be owed than are kept, the newest are.
    let mut many = FindSecrets {
        current: [0; 32],
        ..FindSecrets::default()
    };
    for i in 1..=6u8 {
        many.renew([i; 32], [[i; 32]]);
    }
    let newest = many.valid();
    assert_eq!(newest.current, [6; 32]);
    assert_eq!(
        newest.stale(),
        [[5; 32], [4; 32], [3; 32], [2; 32]],
        "the newest four"
    );
    // `off` connects and gets the current one: no old secret is valid any more.
    security.gave_find_secret(&off);
    assert!(security.find_secrets().stale().is_empty());
    assert!(!owed_file(&path).exists(), "nothing owed, no file");
}

impl Drop for TestPc {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Runs `headset` against a PC that accepts one connection; what each side got.
fn connect<T: Send + 'static>(
    pc: &TestPc,
    headset: impl FnOnce(&mut Headset) -> T + Send + 'static,
) -> (Result<()>, T) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let name = pc
        .dir
        .file_name()
        .unwrap()
        .to_string_lossy()
        .replace("-p-", "-h-");
    let h = std::thread::spawn(move || {
        let stream = TcpStream::connect(addr).unwrap();
        let dir = std::env::temp_dir().join(name);
        let id = Identity::load_or_create(&dir.join("key")).unwrap();
        let got = headset(&mut Headset::connect(stream, id));
        std::fs::remove_dir_all(dir).unwrap();
        got
    });
    let (s, _) = l.accept().unwrap();
    // The headset reads until the PC hangs up.
    // A snapshot, as the session would send it, then the PC hangs up.
    let accepted = accept(s, &pc.security, None).map(|(mut conn, _)| {
        let _ = conn.send(HostMsg::Snapshot { parts: Vec::new() });
        conn.link.closer.close();
    });
    (accepted, h.join().unwrap())
}

/// What a PC of protocol 1 or 2 tells a headset of protocol 1 that it turns away:
/// a text naming the side to update (tag 6), then the PC's protocol and app
/// version (tag 17), in the bytes protocol 1 reads.
fn assert_told_to_update(msgs: &[(u8, Vec<u8>)]) {
    let [(6, text), (17, incompatible)] = msgs else {
        panic!("expected an error and Incompatible, got {msgs:?}")
    };
    let text = String::from_utf8(text.clone()).unwrap();
    assert!(text.contains("Prostir"), "{text}");
    let mut expected = PROTOCOL_VERSION.to_le_bytes().to_vec();
    expected.extend_from_slice(prostir_proto::app_version(env!("CARGO_PKG_VERSION")).as_bytes());
    assert_eq!(incompatible, &expected);
}

/// A headset from another protocol version learns which side to update.
#[test]
fn other_protocol_is_told_to_update() {
    let pc = TestPc::new("ver");
    let (refused, answer) = connect(&pc, |h| h.hello(PROTOCOL_VERSION + 1));
    assert!(refused.is_err(), "the session must not start");
    assert_eq!(
        answer,
        Answer::Incompatible {
            protocol: PROTOCOL_VERSION,
            app: prostir_proto::app_version(env!("CARGO_PKG_VERSION")),
        }
    );
    // Older headsets show the text before it; it names the side to update.
    let (_, msgs) = connect(&pc, |h| {
        h.write_as_v1(&{
            let mut hello = Vec::new();
            ClientMsg::Hello {
                version: 9,
                min: 9,
                app: "9.9.9".into(),
            }
            .write(&mut hello)
            .unwrap();
            hello
        });
        h.read_as_v1()
    });
    let [(6, text), (17, _)] = &msgs[..] else {
        panic!("expected an error and Incompatible, got {msgs:?}")
    };
    assert!(String::from_utf8_lossy(text).contains("Prostir"));
}

/// A headset that closes the link before its hello (a USB dial for another PC lets go
/// of this one so) is not an error worth a warning: it is told apart.
#[test]
fn a_hang_up_before_the_hello_is_told_apart() {
    let pc = TestPc::new("no-hello");
    let (refused, ()) = connect(&pc, |h| h.link.closer.close());
    let e = refused.unwrap_err();
    assert!(e.is::<HungUpBeforeHello>(), "{e:#}");
}

/// A development build of another revision speaks the same protocol version but
/// other messages: it is turned away, told by the PC's revision which side is older.
#[test]
fn another_development_revision_is_turned_away() {
    let Some(ours) = prostir_proto::PROTO_REVISION else {
        return;
    };
    let pc = TestPc::new("rev");
    let (refused, answer) = connect(&pc, move |h| {
        let hello = ClientMsg::Hello {
            version: PROTOCOL_VERSION,
            min: PROTOCOL_VERSION,
            app: format!("0.2.0-dev+p{}", ours + 1),
        };
        let mut bytes = Vec::new();
        hello.write(&mut bytes).unwrap();
        h.write_as_v1(&bytes);
        handshake::read_answer(&mut *h.link.receiver).unwrap()
    });
    assert!(refused.is_err(), "the session must not start");
    let Answer::Incompatible { protocol, app } = answer else {
        panic!("expected Incompatible, got {answer:?}");
    };
    assert_eq!(protocol, PROTOCOL_VERSION);
    assert_eq!(prostir_proto::revision_of(&app), Some(ours));
    assert!(
        !prostir_proto::peer_is_newer(protocol, &app),
        "the headset is newer"
    );
}

/// A 0.1 headset says hello in protocol 1 and is told to update, in bytes it reads.
#[test]
fn a_protocol_1_hello_is_told_to_update() {
    let pc = TestPc::new("v1");
    let (refused, msgs) = connect(&pc, |h| {
        // Hello as 0.1.1 sends it: tag 101, length, version 1, min 1, its version.
        let mut hello = vec![101, 13, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0];
        hello.extend_from_slice(b"0.1.1");
        h.write_as_v1(&hello);
        h.read_as_v1()
    });
    assert!(refused.is_err(), "the session must not start");
    assert_told_to_update(&msgs);
}

/// A 0.1 headset that does not trust the PC yet asks to pair before saying hello:
/// it is told to update instead of pairing in a protocol it does not speak.
#[test]
fn a_protocol_1_pairing_request_is_told_to_update() {
    let pc = TestPc::new("v1pair");
    let (refused, msgs) = connect(&pc, |h| {
        // PairBegin as 0.1.1 sends it: the headset shows the code, then its name;
        // right after it, its 33-byte SPAKE2 message, which the PC never reads.
        let mut begin = vec![109, 11, 0, 0, 0, 1];
        begin.extend_from_slice(b"Meta Quest");
        let (_, msg) = Pairing::start("123456");
        begin.extend_from_slice(&[110, 33, 0, 0, 0]);
        begin.extend_from_slice(&msg);
        h.write_as_v1(&begin);
        h.read_as_v1()
    });
    assert!(refused.is_err(), "no pairing and no session");
    assert_told_to_update(&msgs);
    assert!(pc.security.trusted.lock().unwrap().is_empty());
}

/// The tests that show a code in the tray, one at a time (the tray's code is one).
static TRAY_TESTS: Mutex<()> = Mutex::new(());

/// A connection that ends leaves the tray's code alone unless it showed it: one
/// trusted headset connecting does not hide the code another is pairing with.
#[test]
fn only_the_connection_that_showed_the_code_hides_it() {
    let _tray = TRAY_TESTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let pairing = TrayCode::new(None);
    pairing.show("123 456".into());
    drop(TrayCode::new(None));
    assert!(pairing.shows(), "another connection's end hid the code");
    let later = TrayCode::new(None);
    later.show("654 321".into());
    drop(pairing);
    assert!(later.shows(), "an earlier connection hid the newer code");
    drop(later);
    assert_eq!(code_shown(), None);
}

#[test]
fn a_peer_version_is_short_and_plain() {
    assert_eq!(peer_version("0.2.0-beta+1"), "0.2.0-beta+1");
    assert_eq!(
        peer_version("1.0\n\u{1b}[31mfake log line"),
        "1.031mfakelogline"
    );
    assert_eq!(peer_version(&"9".repeat(60_000)).len(), 24);
}

/// Three wrong codes are free; after that every attempt waits longer, up to 15 min.
#[test]
fn wrong_codes_slow_pairing_down() {
    let mut l = Limiter {
        failures: 0,
        not_before: None,
        warned: true, // no notification from tests
    };
    for _ in 0..FREE_FAILURES {
        l.begin();
        let _ = l.failed();
        assert!(l.wait().is_none());
    }
    // Counted from its start: the pause holds while it runs.
    l.begin();
    let first = l.wait().expect("a pause after the free ones");
    assert!(first > Duration::from_secs(25) && first <= Duration::from_secs(30));
    // Its end does not count it again.
    let _ = l.failed();
    assert!(l.wait().unwrap() <= Duration::from_secs(30));
    l.begin();
    assert!(l.wait().unwrap() > first);
    for _ in 0..20 {
        l.begin();
    }
    assert!(l.wait().unwrap() <= Duration::from_secs(900));
    l.succeeded();
    assert!(l.wait().is_none());
}

/// A peer that trickles its Noise handshake (a long length, then a byte now and
/// then, each within the read timeout) is cut off at the setup deadline: it cannot
/// hold the PC, which accepts one connection at a time.
#[test]
fn a_trickling_peer_is_cut_off_in_time() {
    use std::io::Write;
    let pc = TestPc::new("trickle");
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let peer = std::thread::spawn(move || {
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(&[0xff, 0xff]).unwrap();
        while s.write_all(&[0]).is_ok() {
            std::thread::sleep(Duration::from_millis(100));
        }
    });
    let (s, _) = l.accept().unwrap();
    let start = Instant::now();
    let refused = accept_within(s, &pc.security, None, Duration::from_millis(500));
    assert!(refused.is_err(), "no session");
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "{:?}",
        start.elapsed()
    );
    peer.join().unwrap();
}

/// A headset the PC trusts still opens the session first: a session message before
/// `Open` ends the connection, and nothing starts.
#[test]
fn nothing_before_open() {
    let pc = TestPc::new("early");
    // The headset's key, made before it connects, so the PC can trust it.
    let name = pc
        .dir
        .file_name()
        .unwrap()
        .to_string_lossy()
        .replace("-p-", "-h-");
    let key = Identity::load_or_create(&std::env::temp_dir().join(name).join("key"))
        .unwrap()
        .public;
    pc.security
        .trusted
        .lock()
        .unwrap()
        .add(key, "test headset")
        .unwrap();
    let (refused, after) = connect(&pc, |h| {
        assert!(matches!(h.hello(PROTOCOL_VERSION), Answer::Welcome { .. }));
        h.send(ClientMsg::request(
            prostir_proto::state::ReqId(1),
            prostir_proto::request::Request::FindMouse,
        ));
        h.recv().map_err(|e| e.kind())
    });
    let err = refused.unwrap_err().to_string();
    assert!(err.contains("before the session"), "{err}");
    assert_eq!(
        after,
        Err(io::ErrorKind::UnexpectedEof),
        "closed, no snapshot"
    );
}

/// The settings window waits for the code a headset shows, and the headset hangs up
/// meanwhile: the attempt ends at once, the window waits no more, and the pairing is
/// free for the next headset.
#[test]
fn a_headset_that_hangs_up_ends_the_wait_for_its_code() {
    let _tray = TRAY_TESTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let pc = TestPc::new("hangup");
    window_types_codes(true);
    let started = Instant::now();
    let (accepted, ()) = connect(&pc, |h| {
        assert!(matches!(h.hello(PROTOCOL_VERSION), Answer::Welcome { .. }));
        h.send(ClientMsg::Open { parts: Vec::new() });
        assert_eq!(h.recv().unwrap(), HostMsg::PairingRequired);
        h.send(ClientMsg::PairBegin {
            headset_shows: true,
            name: "test headset".into(),
        });
        let (_, msg) = Pairing::start("123456");
        h.send(ClientMsg::PairMsg { msg });
        let until = Instant::now() + Duration::from_secs(5);
        while code_for().is_none() {
            assert!(
                Instant::now() < until,
                "the window shows no headset waiting"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        h.link.closer.close();
    });
    window_types_codes(false);
    let err = format!("{:#}", accepted.unwrap_err());
    assert!(err.contains("hung up"), "{err}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(code_for(), None);
    assert!(PAIRING.try_lock().is_ok(), "the pairing is free");
}

/// Headsets that wait for the pairing while another attempt runs do not slip past the
/// pause the attempts set: after it, the first of them gets its turn (counted), and
/// the next is told to wait.
#[test]
fn headsets_queued_for_the_pairing_wait_out_the_pause() {
    let _tray = TRAY_TESTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let fresh = |failures| Limiter {
        failures,
        not_before: None,
        warned: true, // no notification from tests
    };
    *LIMITER.lock().unwrap() = fresh(FREE_FAILURES - 1);
    let first = take_turn().ok().expect("the first headset's turn");
    LIMITER.lock().unwrap().begin();
    let queued: Vec<_> = (0..2)
        .map(|_| {
            std::thread::spawn(|| match take_turn() {
                Ok(turn) => {
                    LIMITER.lock().unwrap().begin();
                    std::thread::sleep(Duration::from_millis(100));
                    let _ = LIMITER.lock().unwrap().failed();
                    drop(turn);
                    "paired"
                }
                Err(NoTurn::Busy) => "busy",
                Err(NoTurn::Paused(_)) => "paused",
            })
        })
        .collect();
    std::thread::sleep(Duration::from_millis(200));
    let _ = LIMITER.lock().unwrap().failed();
    drop(first);
    let mut got: Vec<&str> = queued.into_iter().map(|t| t.join().unwrap()).collect();
    got.sort_unstable();
    *LIMITER.lock().unwrap() = fresh(0);
    assert_eq!(got, ["paired", "paused"]);
}

/// An attempt that showed nothing (no dialog on the PC) after the free ones warns
/// nobody; the first attempt that fails after them does, once.
#[test]
fn only_a_failed_attempt_warns_of_the_pause() {
    let mut l = Limiter {
        failures: FREE_FAILURES,
        not_before: None,
        warned: false,
    };
    l.begin();
    l.not_attempted();
    assert!(!l.warned, "nothing was shown");
    assert!(l.wait().is_none());
    l.begin();
    assert!(l.failed(), "the first pause is told");
    l.begin();
    assert!(!l.failed(), "once");
}

/// "Code on the PC": the headset asks first, the PC makes and shows its code, the
/// user types it in the headset, and the headset is trusted afterwards.
#[test]
fn code_shown_on_the_pc_pairs() {
    let _tray = TRAY_TESTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let pc = TestPc::new("pcc");
    let (accepted, (ok, opened)) = connect(&pc, |h| {
        assert!(matches!(h.hello(PROTOCOL_VERSION), Answer::Welcome { .. }));
        h.send(ClientMsg::Open { parts: Vec::new() });
        assert_eq!(h.recv().unwrap(), HostMsg::PairingRequired);
        h.send(ClientMsg::PairBegin {
            headset_shows: false,
            name: "test headset".into(),
        });
        let HostMsg::PairMsg { msg: theirs } = h.recv().unwrap() else {
            panic!("expected PairMsg")
        };
        // What the user reads in the tray and types in the headset.
        let code = PC_CODE.lock().unwrap().clone().unwrap().0;
        let (p, msg) = Pairing::start(&code);
        h.send(ClientMsg::PairMsg { msg });
        let key = p.finish(&theirs).unwrap();
        let peer = h.link.peer.clone();
        h.send(ClientMsg::PairConfirm {
            tag: confirmation(&key, "headset", &peer, &h.id.public),
        });
        let HostMsg::PairResult { tag } = h.recv().unwrap() else {
            panic!("expected PairResult")
        };
        h.send(ClientMsg::Open { parts: Vec::new() });
        (tag.is_some(), h.recv().unwrap())
    });
    assert!(ok, "the codes must match");
    assert_eq!(opened, HostMsg::Snapshot { parts: Vec::new() });
    assert!(accepted.is_ok());
    assert_eq!(pc.security.trusted.lock().unwrap().len(), 1);
}
