use super::*;
use std::cell::RefCell;

fn args(line: &str) -> Vec<String> {
    line.split_whitespace().map(str::to_owned).collect()
}

fn asks(line: &str) -> Request {
    match parse(&args(line)) {
        Ok((Ctl::Ask(r), _)) => r,
        other => panic!("{line}: {other:?}"),
    }
}

/// Every command reads as its request.
#[test]
fn commands_read_as_requests() {
    assert_eq!(asks("recenter"), Request::Recenter);
    assert_eq!(asks("lineup"), Request::LineUp);
    assert_eq!(asks("showmouse"), Request::ShowMouse);
    assert_eq!(asks("reset"), Request::ResetArrangement);
    assert_eq!(
        asks("profile Робота вдома"),
        Request::Profile {
            name: "Робота вдома".into()
        }
    );
    assert_eq!(
        asks("profile rename 2 Робота вдома"),
        Request::RenameProfile {
            profile: "2".into(),
            name: "Робота вдома".into()
        }
    );
    assert_eq!(
        asks("profile rename Ігри"),
        Request::RenameProfile {
            profile: "Ігри".into(),
            name: String::new()
        }
    );
    assert_eq!(asks("cinema on"), Request::Cinema { on: true });
    assert_eq!(asks("focus off"), Request::Focus { on: false });
    assert_eq!(asks("follow on"), Request::Follow { on: true });
    assert_eq!(asks("mic off"), Request::SetMic { on: false });
    assert_eq!(asks("nolimits on"), Request::NoLimits { on: true });
    assert_eq!(asks("lock on"), Request::Lock { on: true });
    assert_eq!(asks("preset 4"), Request::Preset { preset: 4 });
    let monitor = |w, h, scale| Request::AddMonitor { w, h, scale };
    assert_eq!(asks("monitor add 2560x1440"), monitor(2560, 1440, 100));
    assert_eq!(asks("monitor add 2560x1440@1.25"), monitor(2560, 1440, 125));
    assert_eq!(asks("monitor add 1920X1080@150%"), monitor(1920, 1080, 150));
    assert_eq!(
        asks("monitor remove 103"),
        Request::RemoveMonitor { id: 103 }
    );
    assert_eq!(asks("monitor retry 103"), Request::RetryMonitor { id: 103 });
    assert_eq!(asks("monitor primary 103"), Request::SetPrimary { id: 103 });
    assert_eq!(
        asks("screen show 104"),
        Request::ShowPhysical {
            id: 104,
            shown: true
        }
    );
    assert_eq!(
        asks("screen hide 104"),
        Request::ShowPhysical {
            id: 104,
            shown: false
        }
    );
    assert_eq!(asks("screenshot 103"), Request::Screenshot { id: 103 });
    assert_eq!(asks("quality 50"), Request::SetQuality { mbit: 50 });
    assert_eq!(asks("sharp on"), Request::SetSharp { on: true });
    assert_eq!(
        asks("codec h264 --cpu"),
        Request::SetCodec {
            codec: CodecChoice::H264,
            cpu: true
        }
    );
    // HEVC as people read it, h265 as before.
    for word in ["hevc", "h265"] {
        assert_eq!(
            asks(&format!("codec {word}")),
            Request::SetCodec {
                codec: CodecChoice::H265,
                cpu: false
            }
        );
    }
    assert_eq!(
        asks("codec auto"),
        Request::SetCodec {
            codec: CodecChoice::Auto,
            cpu: false
        }
    );
    assert_eq!(asks("sound pc"), Request::SoundToHeadset { on: false });
    assert_eq!(asks("sound headset"), Request::SoundToHeadset { on: true });
    assert_eq!(asks("pair"), Request::PairNewHeadset);
    assert_eq!(asks("pair stop"), Request::StopPairing);
    assert_eq!(
        asks("forget Quest 3"),
        Request::ForgetHeadset {
            headset: "Quest 3".into()
        }
    );
    assert_eq!(asks("forget --all"), Request::ForgetAll);
    assert_eq!(
        asks("input on Quest 3"),
        Request::SetInput {
            headset: "Quest 3".into(),
            on: true
        }
    );
    assert_eq!(
        asks("input off a1b2c3d4e5f6"),
        Request::SetInput {
            headset: "a1b2c3d4e5f6".into(),
            on: false
        }
    );
    assert_eq!(asks("autostart on"), Request::Autostart { on: true });
    let counting = |counting| Request::SetCounting { counting };
    assert_eq!(asks("count yes"), counting(api::Counting::Yes));
    assert_eq!(asks("count no"), counting(api::Counting::No));
    assert_eq!(asks("count unasked"), counting(api::Counting::Unasked));
    assert_eq!(asks("settings"), Request::ShowSettings);
    assert_eq!(asks("quit"), Request::Quit);
}

/// The commands that only read, `--json` anywhere, and help.
#[test]
fn reading_commands_and_flags() {
    let read = |line: &str| parse(&args(line)).unwrap();
    assert_eq!(read("status"), (Ctl::Status, false));
    assert_eq!(read("status --json"), (Ctl::Status, true));
    assert_eq!(read("--json watch"), (Ctl::Watch, true));
    assert_eq!(read("monitor list"), (Ctl::Monitors, false));
    assert_eq!(read("headsets --json"), (Ctl::Headsets, true));
    assert_eq!(read("autostart status"), (Ctl::AutostartStatus, false));
    assert_eq!(read("count status --json"), (Ctl::CountStatus, true));
    assert_eq!(read(""), (Ctl::Help, false));
    assert_eq!(read("--help"), (Ctl::Help, false));
}

/// What does not read says why.
#[test]
fn wrong_arguments() {
    for line in [
        "fly",
        "cinema",
        "cinema maybe",
        "recenter now",
        "preset 300",
        "monitor add big",
        "monitor add 2560x1440@x",
        "monitor remove",
        "monitor remove one",
        "screen show",
        "quality fast",
        "codec vp9",
        "sound loud",
        "count",
        "count maybe",
        "count off",
        "count on",
        "count status now",
        "pair now",
        "forget",
        "profile",
        "profile rename",
        "status --cpu",
        "recenter --all",
        "autostart maybe",
    ] {
        assert!(parse(&args(line)).is_err(), "{line}");
    }
}

/// A client that answers as a test says and keeps what it was asked.
struct Fake {
    call: Result<Answer, ClientError>,
    state: Result<(u32, String), ClientError>,
    asked: RefCell<Vec<Request>>,
}

impl Fake {
    fn answering(call: Result<Answer, ClientError>) -> Self {
        Self {
            call,
            state: Err(ClientError::NotRunning),
            asked: RefCell::new(Vec::new()),
        }
    }
}

impl CtlClient for Fake {
    fn call(&self, request: &Request) -> Result<Answer, ClientError> {
        self.asked.borrow_mut().push(request.clone());
        self.call.clone()
    }

    fn state(&self) -> Result<(u32, String), ClientError> {
        self.state.clone()
    }

    fn running(&self) -> bool {
        self.state.is_ok()
    }

    fn watch(&self, _each: &mut dyn FnMut(Watched) -> bool) -> Result<(), ClientError> {
        Ok(())
    }
}

/// A Prostir that does not run, asleep or kept from starting by its port.
struct NotRunning {
    asleep: bool,
    taken: bool,
    /// How many times what sleeps for it was asked to quit.
    quits: std::cell::Cell<u32>,
}

impl NotRunning {
    fn new(asleep: bool, taken: bool) -> Self {
        Self {
            asleep,
            taken,
            quits: std::cell::Cell::new(0),
        }
    }
}

impl CtlClient for NotRunning {
    fn call(&self, _request: &Request) -> Result<Answer, ClientError> {
        Err(ClientError::NotRunning)
    }

    fn state(&self) -> Result<(u32, String), ClientError> {
        Err(ClientError::NotRunning)
    }

    fn running(&self) -> bool {
        false
    }

    fn watch(&self, _each: &mut dyn FnMut(Watched) -> bool) -> Result<(), ClientError> {
        Err(ClientError::NotRunning)
    }

    fn asleep(&self) -> bool {
        self.asleep
    }

    fn port_taken(&self) -> bool {
        self.taken
    }

    fn quit_asleep(&self) -> bool {
        self.quits.set(self.quits.get() + 1);
        self.asleep
    }
}

/// `quit` while Prostir sleeps quits what sleeps for it (its sockets' watcher, or
/// systemd's socket): done. Nothing sleeping, Prostir does not run.
#[test]
fn quit_ends_a_sleeping_prostir() {
    let quit = |client: &NotRunning, line: &str| {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(
            &args(line),
            Ok(client),
            &mut Output {
                out: &mut out,
                err: &mut err,
            },
        );
        (code, String::from_utf8(out).unwrap())
    };
    let asleep = NotRunning::new(true, false);
    assert_eq!(quit(&asleep, "quit").0, DONE);
    assert_eq!(asleep.quits.get(), 1);
    let (code, out) = quit(&asleep, "quit --json");
    assert_eq!(code, DONE);
    assert!(out.contains("revision"), "{out}");
    let gone = NotRunning::new(false, false);
    assert_eq!(quit(&gone, "quit").0, NOT_RUNNING);
    // Only quit does.
    let asleep = NotRunning::new(true, false);
    assert_eq!(quit(&asleep, "recenter").0, NOT_RUNNING);
    assert_eq!(asleep.quits.get(), 0);
}

/// `status` without Prostir says whether it sleeps, or whether its port keeps it
/// from starting.
#[test]
fn status_says_asleep_or_port_taken() {
    let status = |asleep, taken, json: bool| {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let line = if json { "status --json" } else { "status" };
        let code = run(
            &args(line),
            Ok(&NotRunning::new(asleep, taken)),
            &mut Output {
                out: &mut out,
                err: &mut err,
            },
        );
        (code, String::from_utf8(out).unwrap())
    };
    let not_running = format!("{}\n", t("pc-ctl-not-running"));
    assert_eq!(
        status(false, false, false),
        (NOT_RUNNING, not_running.clone())
    );
    let asleep = status(true, true, false).1;
    assert_ne!(asleep, not_running);
    let taken = status(false, true, false).1;
    assert!(taken.contains("47830"), "{taken}");
    assert_ne!(taken, asleep);
    let json: serde_json::Value = serde_json::from_str(&status(false, true, true).1).unwrap();
    assert_eq!(json["port_taken"], true);
    let json: serde_json::Value = serde_json::from_str(&status(true, false, true).1).unwrap();
    assert_eq!(json["asleep"], true);
}

fn ctl(line: &str, client: &Fake) -> (i32, String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run(
        &args(line),
        Ok(client),
        &mut Output {
            out: &mut out,
            err: &mut err,
        },
    );
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

fn refused(error: ApiError) -> Result<Answer, ClientError> {
    Err(ClientError::Refused(Failure::new(error, "why")))
}

/// Exit codes for each way a request ends.
#[test]
fn exit_codes() {
    let done = Fake::answering(Ok(Answer {
        id: Some(7),
        revision: 2,
        ..Answer::default()
    }));
    let (code, out, _) = ctl("monitor add 800x600 --json", &done);
    assert_eq!((code, out.trim()), (DONE, r#"{"id":7,"revision":2}"#));
    assert_eq!(
        done.asked.borrow()[0],
        Request::AddMonitor {
            w: 800,
            h: 600,
            scale: 100
        }
    );
    let (code, _, err) = ctl("recenter", &Fake::answering(refused(ApiError::NotAllowed)));
    assert_eq!(code, FAILED);
    assert!(err.contains("NotAllowed: why"), "{err}");
    let (code, out, _) = ctl(
        "recenter --json",
        &Fake::answering(refused(ApiError::NotConnected)),
    );
    assert_eq!(
        code, NOT_RUNNING,
        "no headset for a command for the headset"
    );
    assert_eq!(out.trim(), r#"{"error":"NotConnected","text":"why"}"#);
    let not_connected = Fake::answering(refused(ApiError::NotConnected));
    assert_eq!(ctl("quality 30", &not_connected).0, FAILED);
    assert_eq!(
        ctl("lineup", &Fake::answering(refused(ApiError::TimedOut))).0,
        TIMED_OUT
    );
    assert_eq!(
        ctl("lineup", &Fake::answering(Err(ClientError::TimedOut))).0,
        TIMED_OUT
    );
    assert_eq!(
        ctl("lineup", &Fake::answering(Err(ClientError::NotRunning))).0,
        NOT_RUNNING
    );
    let (code, out, _) = ctl("status --json", &Fake::answering(Ok(Answer::default())));
    assert_eq!(code, NOT_RUNNING);
    let status: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(status["error"], "NotRunning");
    assert_eq!(status["running"], false);
    let (code, out, _) = ctl(
        "recenter --json",
        &Fake::answering(Err(ClientError::NotRunning)),
    );
    assert_eq!(code, NOT_RUNNING);
    let gone: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(gone["error"], "NotRunning");
    assert!(
        gone.get("revision").is_none(),
        "no revision without an answer"
    );
    let (code, _, err) = ctl("cinema maybe", &done);
    assert_eq!(code, USAGE);
    assert!(err.contains("on or off"), "{err}");
}

/// `status` and `monitor list` from the state.
#[test]
fn state_for_people() {
    let state = State {
        v: 1,
        displays: vec![api::Display {
            id: 103,
            name: "Prostir-1".into(),
            mode: api::Mode {
                w: 640,
                h: 360,
                scale: 100,
            },
            state: DisplayState::Running,
            ..api::Display::default()
        }],
        primary: Some(103),
        ..State::default()
    };
    let client = Fake {
        state: Ok((5, state.to_json())),
        ..Fake::answering(Ok(Answer::default()))
    };
    let (code, out, _) = ctl("status", &client);
    assert_eq!(code, DONE);
    assert_eq!(out.trim(), t("pc-ctl-waiting-for-a-headset"));
    let (_, out, _) = ctl("monitor list", &client);
    let line = format!(
        "103\tProstir-1\t{}\t640x360 100%\t{}{}",
        t("pc-ctl-monitor"),
        t("pc-ctl-running"),
        t("pc-ctl-primary")
    );
    assert_eq!(out.trim(), line);
    let (_, out, _) = ctl("autostart status --json", &client);
    assert_eq!(out.trim(), r#"{"autostart":null,"revision":5}"#);
}

/// `open` starts Prostir when it is not running, then asks for the window; a
/// missing window still leaves Prostir running.
#[test]
fn open_starts_prostir_first() {
    let running = Fake {
        state: Ok((1, "{}".into())),
        ..Fake::answering(refused(ApiError::Unsupported))
    };
    let started = std::cell::Cell::new(false);
    let start = || {
        started.set(true);
        std::process::Command::new("true")
            .spawn()
            .map(Woken::Started)
    };
    assert_eq!(open(&running, &start), DONE);
    assert!(!started.get(), "already running");
    assert_eq!(running.asked.borrow()[0], Request::ShowSettings);
    let gone = Fake::answering(Ok(Answer::default()));
    let fail = || Err(std::io::Error::other("no program"));
    assert_eq!(open(&gone, &fail), NOT_RUNNING);
    // Started, but it ends without coming on the bus: said at once.
    let ends = || {
        std::process::Command::new("false")
            .spawn()
            .map(Woken::Started)
    };
    let began = std::time::Instant::now();
    assert_eq!(open(&gone, &ends), NOT_RUNNING);
    assert!(began.elapsed() < Duration::from_secs(5));
    assert!(gone.asked.borrow().is_empty(), "no window asked of nobody");
}
