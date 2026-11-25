//! A service in memory, for tests, pictures and trying the window without Prostir
//! (`prostir window --mock`). It answers the requests the way the service of
//! LOCAL-API.md does now: the answer carries the revision of the state that has the
//! change, monitors asked for while no headset is connected are kept for the next
//! one, and what the service does not do yet (pairing from the window until step
//! 6.8, the volume until 6.6, the microphone's switch ever) is `Unsupported`.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::api::*;

/// How long the pairing window stays open (from step 6.8).
pub const PAIR_SECONDS: u32 = 180;

/// A code that never matches, to try the "wrong code" state.
pub const WRONG_CODE: &str = "000000";

struct Inner {
    state: State,
    revision: u32,
    running: bool,
    sink: Option<Arc<Sink>>,
    /// Real time: the pairing window runs out. Frozen: it stays as made (pictures).
    live: bool,
    /// Pairing from the window as step 6.8 brings it; without, `Unsupported`.
    pairing_steps: bool,
    pairing_since: Option<Instant>,
    calls: Vec<Request>,
    /// The next request is refused with this error.
    refuse: Option<ClientError>,
    /// Asked for the state, it does not answer in time.
    silent: bool,
    /// It owns the name but has not this interface: a Prostir older than the window.
    old: bool,
    /// Monitors added and not placed by the headset yet, as the service has them
    /// (nowhere on the desktop): where the headset puts each, and since when.
    placing: Vec<(u32, DeskRect, Instant)>,
}

/// The mock service.
pub struct Mock {
    inner: Arc<Mutex<Inner>>,
}

/// A second handle on the same mock: switch it on and off, read what was asked.
#[derive(Clone)]
pub struct MockHandle {
    inner: Arc<Mutex<Inner>>,
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn display(id: u32, name: &str, w: u32, h: u32, x: i32, y: i32) -> Display {
    Display {
        id,
        kind: DisplayKind::Virtual,
        name: name.into(),
        output: format!("Virtual-{name}"),
        builtin: false,
        shown: true,
        mode: Mode { w, h, scale: 100 },
        desk: Some(DeskRect { x, y, w, h }),
        placing: false,
        state: DisplayState::Running,
        failure: None,
    }
}

/// A monitor of the set kept for the next headset: no output, nowhere on the desk.
fn kept(id: u32, name: &str, w: u32, h: u32) -> Display {
    Display {
        output: String::new(),
        desk: None,
        state: DisplayState::Kept,
        ..display(id, name, w, h, 0, 0)
    }
}

/// The PC's own screen, as a laptop has.
fn laptop(id: u32) -> Display {
    Display {
        kind: DisplayKind::Physical,
        name: "eDP-1".into(),
        output: "eDP-1".into(),
        builtin: true,
        mode: Mode {
            w: 1920,
            h: 1200,
            scale: 125,
        },
        desk: Some(DeskRect {
            x: 2880,
            y: 2000,
            w: 1536,
            h: 960,
        }),
        ..display(id, "eDP-1", 1920, 1200, 0, 0)
    }
}

fn headset(key: &str, name: &str, hours_ago: u64) -> Headset {
    Headset {
        key: key.into(),
        name: name.into(),
        model: String::new(),
        app: "0.2.0".into(),
        last_seen: Some(now_unix() - hours_ago * 3600),
        role: Role::Main,
        rights: ["input", "mic", "camera", "clipboard", "changes", "private"]
            .map(String::from)
            .to_vec(),
        session: None,
    }
}

fn base() -> State {
    State {
        v: STATE_VERSION,
        pc: Pc {
            name: "workstation".into(),
            app: env!("CARGO_PKG_VERSION").into(),
            desktop: String::new(),
            phase: Phase::Idle,
            addresses: Vec::new(),
        },
        window: WindowLook::default(),
        autostart: Some(true),
        headsets: vec![
            headset("5f2a9c", "Quest 3", 2),
            headset("c81e07", "Quest 3S", 26),
        ],
        connecting: None,
        displays: vec![
            kept(1, "Prostir-1", 2560, 1440),
            kept(2, "Prostir-2", 2560, 1440),
            kept(3, "Prostir-3", 1440, 2560),
            laptop(4),
        ],
        primary: None,
        limits: Limits {
            sizes: vec![
                [1920, 1080],
                [2560, 1440],
                [3840, 2160],
                [3440, 1440],
                [1440, 2560],
            ],
            scales: vec![100, 125, 150],
            displays_max: Some(6),
            physical_max: Some(2),
            streams_max: Some(8),
            min: Some([640, 360]),
            max: Some([3840, 2160]),
        },
        encoding: Encoding {
            bitrate: 30,
            large: None,
            sharp: true,
            sharp_by_size: true,
            codec: Codec::H265,
            cpu: false,
            using: None,
            outputs: Vec::new(),
        },
        sound: Sound {
            to_headset: true,
            volume: None,
            mic_on: false,
            mic: MicState::Off,
            mic_failure: None,
        },
        pairing: None,
        nearby: Vec::new(),
        counting: Some(Counting::Yes),
        problems: Vec::new(),
        wifi: None,
    }
}

impl Mock {
    fn with(state: State, running: bool) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                state,
                revision: 1,
                running,
                sink: None,
                live: false,
                pairing_steps: false,
                pairing_since: None,
                calls: Vec::new(),
                refuse: None,
                silent: false,
                old: false,
                placing: Vec::new(),
            })),
        }
    }

    /// The main headset in a session with three monitors and the laptop's screen shown.
    pub fn connected() -> Self {
        let mut s = base();
        s.pc.desktop = "KDE Plasma".into();
        s.pc.phase = Phase::Live;
        s.headsets[0].model = "Quest 3".into();
        s.headsets[0].session = Some(Session {
            via: Via::Wifi,
            worn: true,
            battery: Some(Battery {
                percent: 72,
                charging: false,
            }),
            profile: Some("1".into()),
            profiles: Vec::new(),
            mic_allowed: Some(true),
            mic_on: true,
            main: true,
            commands: true,
            no_limits: Some(false),
        });
        s.headsets[0].last_seen = None;
        s.displays = vec![
            display(1, "Prostir-1", 2560, 1440, 0, 560),
            display(2, "Prostir-2", 2560, 1440, 2560, 560),
            display(3, "Prostir-3", 1440, 2560, 5120, 0),
            laptop(4),
        ];
        s.primary = Some(2);
        s.encoding.using = Some(Using {
            codec: Codec::H265,
            hardware: true,
        });
        s.sound.mic_on = true;
        s.sound.mic = MicState::Ready;
        Self::with(s, true)
    }

    /// Connected with five monitors at mixed scales, two of them upright, and the
    /// laptop's screen: tiles as large as the desktop has them, some narrow.
    pub fn many_monitors() -> Self {
        let m = Self::connected();
        {
            let mut i = m.inner.lock().unwrap();
            let scaled = |id, name, (w, h): (u32, u32), scale: u16, (x, y)| {
                let logical = |px: u32| px * 100 / u32::from(scale);
                Display {
                    mode: Mode { w, h, scale },
                    desk: Some(DeskRect {
                        x,
                        y,
                        w: logical(w),
                        h: logical(h),
                    }),
                    ..display(id, name, w, h, x, y)
                }
            };
            i.state.displays = vec![
                scaled(1, "Prostir-1", (2560, 1440), 100, (0, 560)),
                scaled(2, "Prostir-2", (2560, 1440), 125, (2560, 704)),
                scaled(3, "Prostir-3", (1440, 2560), 150, (4608, 0)),
                scaled(5, "Prostir-4", (1440, 2560), 100, (5568, 0)),
                scaled(6, "Prostir-12", (1920, 1080), 150, (7008, 1000)),
                laptop(4),
            ];
        }
        m
    }

    /// Paired headsets, none connected: the monitors kept for the next one and the
    /// PC's own screen.
    pub fn waiting() -> Self {
        Self::with(base(), true)
    }

    /// A headset found this PC and the connection is being set up.
    pub fn connecting() -> Self {
        let mut s = base();
        s.connecting = Some("Quest 3".into());
        Self::with(s, true)
    }

    /// Connected, but one monitor could not be made.
    pub fn failed_monitor() -> Self {
        let m = Self::connected();
        {
            let mut i = m.inner.lock().unwrap();
            let mut d = display(5, "Prostir-4", 1920, 1080, 0, 0);
            d.desk = None;
            d.output = String::new();
            d.state = DisplayState::Failed;
            d.failure = Some(Problem {
                code: ApiError::Desktop,
                text: "KWin не відповів за 10 секунд, Prostir спробує ще раз.".into(),
            });
            i.state.displays.insert(3, d);
        }
        m
    }

    /// A PC that has never paired a headset.
    pub fn no_headsets() -> Self {
        let mut s = base();
        s.headsets.clear();
        s.displays = vec![laptop(4)];
        Self::with(s, true)
    }

    /// The service is not on the bus.
    pub fn not_running() -> Self {
        Self::with(base(), false)
    }

    /// Pairing from the window as step 6.8 brings it, open now: a headset shows a
    /// code to type here.
    pub fn pairing(self) -> Self {
        {
            let mut i = self.inner.lock().unwrap();
            i.pairing_steps = true;
            i.live = false;
            i.state.pairing = Some(Pairing {
                seconds_left: 161,
                seconds_total: PAIR_SECONDS,
                pc_code: None,
                code_for: Some("Quest 3S".into()),
                last: None,
            });
            i.state.nearby = vec!["Quest 3S".into()];
        }
        self
    }

    /// The PC shows a code to type in the headset, as the service does now: no time
    /// limit, nothing but the code.
    pub fn pc_code(self, code: &str) -> Self {
        self.inner.lock().unwrap().state.pairing = Some(Pairing {
            pc_code: Some(code.into()),
            ..Default::default()
        });
        self
    }

    /// With the window's look as the service keeps it.
    pub fn look(self, theme: ThemeMode, palette: &str) -> Self {
        self.inner.lock().unwrap().state.window = WindowLook {
            theme,
            palette: palette.into(),
            language: None,
        };
        self
    }

    /// Changes the state before the window sees it (tests).
    pub fn edit(self, f: impl FnOnce(&mut State)) -> Self {
        f(&mut self.inner.lock().unwrap().state);
        self
    }

    /// Time runs: with pairing from the window, its time runs out, and a headset
    /// turns up a few seconds after it opens.
    pub fn live(self) -> Self {
        self.inner.lock().unwrap().live = true;
        self
    }

    pub fn handle(&self) -> MockHandle {
        MockHandle {
            inner: self.inner.clone(),
        }
    }

    /// The state now (tests).
    pub fn state(&self) -> State {
        self.inner.lock().unwrap().state.clone()
    }
}

impl MockHandle {
    /// Starts or stops the service, telling the window as the bus would.
    pub fn set_running(&self, on: bool) {
        let sink = {
            let mut i = self.inner.lock().unwrap();
            i.running = on;
            i.sink.clone()
        };
        if let Some(sink) = sink {
            sink(if on {
                Change::Appeared
            } else {
                Change::Vanished
            });
        }
    }

    /// Refuses the next request with `e`, as a service with a problem would.
    pub fn refuse_next(&self, e: ClientError) {
        self.inner.lock().unwrap().refuse = Some(e);
    }

    /// Starts or stops the service without the bus telling the window: what a missed
    /// signal looks like.
    pub fn set_running_quietly(&self, on: bool) {
        self.inner.lock().unwrap().running = on;
    }

    /// Lets the service stop answering for its state, or answer again.
    pub fn set_silent(&self, on: bool) {
        self.inner.lock().unwrap().silent = on;
    }

    /// Makes it a Prostir older than the window, or this version again.
    pub fn set_old(&self, on: bool) {
        self.inner.lock().unwrap().old = on;
    }

    /// Every request asked so far.
    pub fn calls(&self) -> Vec<Request> {
        self.inner.lock().unwrap().calls.clone()
    }

    pub fn state(&self) -> State {
        self.inner.lock().unwrap().state.clone()
    }

    /// Changes the state and tells the window, as a change on the PC would.
    pub fn change(&self, f: impl FnOnce(&mut State)) {
        let mut i = self.inner.lock().unwrap();
        f(&mut i.state);
        i.publish();
    }

    /// The headset placed the monitors added: they stand where it put them, running.
    pub fn place_new(&self) {
        let mut i = self.inner.lock().unwrap();
        if i.place_new(None) {
            i.publish();
        }
    }

    /// Moves time on: the pairing window, a headset turning up, a monitor added placed
    /// (live mocks only).
    pub fn tick(&self) {
        let mut i = self.inner.lock().unwrap();
        if i.tick() {
            i.publish();
        }
    }
}

impl Inner {
    fn publish(&mut self) {
        self.revision += 1;
        if let Some(sink) = self.sink.clone() {
            let snap = Snapshot {
                revision: self.revision,
                state: Box::new(self.state.clone()),
            };
            sink(Change::State(snap));
        }
    }

    /// Places the monitors added (those added `after` ago at least, if given): whether
    /// any was.
    fn place_new(&mut self, after: Option<Duration>) -> bool {
        let due = |since: &Instant| after.is_none_or(|a| since.elapsed() >= a);
        let (now, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.placing)
            .into_iter()
            .partition(|(_, _, t)| due(t));
        self.placing = later;
        for (id, rect, _) in &now {
            if let Some(d) = self.state.displays.iter_mut().find(|d| d.id == *id) {
                d.desk = Some(*rect);
                d.placing = false;
                d.state = DisplayState::Running;
            }
        }
        !now.is_empty()
    }

    /// The pairing window in real time, a monitor added placed about a second later;
    /// whether anything changed.
    fn tick(&mut self) -> bool {
        let placed = self.live && self.place_new(Some(Duration::from_secs(1)));
        placed | self.pairing_tick()
    }

    fn pairing_tick(&mut self) -> bool {
        let (Some(since), true) = (self.pairing_since, self.live) else {
            return false;
        };
        let gone = since.elapsed().as_secs() as u32;
        let Some(p) = &mut self.state.pairing else {
            return false;
        };
        if gone >= PAIR_SECONDS {
            self.state.pairing = None;
            self.state.nearby.clear();
            self.pairing_since = None;
            return true;
        }
        p.seconds_left = PAIR_SECONDS - gone;
        if gone >= 4 && p.code_for.is_none() {
            p.code_for = Some("Quest 3S".into());
            self.state.nearby = vec!["Quest 3S".into()];
            return true;
        }
        false
    }

    fn display_mut(&mut self, id: u32) -> Result<&mut Display, ClientError> {
        self.state
            .displays
            .iter_mut()
            .find(|d| d.id == id)
            .ok_or_else(|| refused(ApiError::UnknownDisplay, "Такого дисплея немає."))
    }

    /// What the request changes; `Ok(Some(id))` for a monitor made.
    fn apply(&mut self, r: &Request) -> Result<Option<u32>, ClientError> {
        let connected = self.state.connected().is_some();
        let s = &mut self.state;
        let virtuals = s
            .displays
            .iter()
            .filter(|d| d.kind == DisplayKind::Virtual)
            .count();
        match r {
            Request::Recenter
            | Request::LineUp
            | Request::ShowMouse
            | Request::ResetArrangement
            | Request::Profile { .. }
            | Request::Cinema { .. }
            | Request::Focus { .. }
            | Request::Follow { .. }
            | Request::Preset { .. } => {
                if !connected {
                    return Err(refused(ApiError::NotConnected, "Шолом не підключено."));
                }
            }
            Request::NoLimits { on } => {
                let Some(session) = s.headsets.iter_mut().find_map(|h| h.session.as_mut()) else {
                    return Err(refused(ApiError::NotConnected, "Шолом не підключено."));
                };
                session.no_limits = Some(*on);
            }
            Request::RenameProfile { profile, name } => {
                let Some(session) = s.headsets.iter_mut().find_map(|h| h.session.as_mut()) else {
                    return Err(refused(ApiError::NotConnected, "Шолом не підключено."));
                };
                let at = session
                    .profiles
                    .iter()
                    .position(|p| p == profile)
                    .or_else(|| {
                        profile
                            .parse::<usize>()
                            .ok()
                            .and_then(|n| n.checked_sub(1))
                            .filter(|i| *i < session.profiles.len())
                    });
                let Some(at) = at else {
                    return Err(refused(ApiError::NoSuchProfile, "Такого профілю немає."));
                };
                session.profiles[at].clone_from(name);
            }
            Request::AddMonitor { w, h, scale } => {
                if s.limits.displays_max.is_some_and(|m| virtuals as u32 >= m) {
                    return Err(refused(ApiError::Limit, "Більше моніторів не вміщається."));
                }
                let id = s.displays.iter().map(|d| d.id).max().unwrap_or(0) + 1;
                let n = (1..)
                    .find(|n| !s.displays.iter().any(|d| d.name == format!("Prostir-{n}")))
                    .unwrap_or(1);
                let name = format!("Prostir-{n}");
                let mut d = if connected {
                    let right = s
                        .displays
                        .iter()
                        .filter_map(|d| d.desk)
                        .map(|r| r.x + r.w as i32)
                        .max()
                        .unwrap_or(0);
                    // Nowhere until the headset placed it (see `MockHandle::place_new`).
                    let mut d = display(id, &name, *w, *h, right, 0);
                    self.placing
                        .push((id, d.desk.take().unwrap(), Instant::now()));
                    d.state = DisplayState::Starting;
                    d.placing = true;
                    d
                } else {
                    // Kept for the next headset.
                    kept(id, &name, *w, *h)
                };
                d.mode.scale = *scale;
                s.displays.insert(virtuals, d);
                return Ok(Some(id));
            }
            Request::RemoveMonitor { id } => {
                let Some(d) = s.displays.iter().find(|d| d.id == *id) else {
                    return Err(refused(ApiError::UnknownDisplay, "Такого монітора немає."));
                };
                if d.kind != DisplayKind::Virtual {
                    return Err(refused(ApiError::UnknownDisplay, "Такого монітора немає."));
                }
                if connected && virtuals == 1 {
                    return Err(refused(
                        ApiError::LastDisplay,
                        "Останній монітор лишається.",
                    ));
                }
                s.displays.retain(|d| d.id != *id);
                if s.primary == Some(*id) {
                    s.primary = s.displays.first().map(|d| d.id);
                }
            }
            Request::RetryMonitor { id } => {
                let d = self.display_mut(*id)?;
                d.state = DisplayState::Running;
                d.failure = None;
                d.desk = Some(DeskRect {
                    x: 0,
                    y: 0,
                    w: d.mode.w,
                    h: d.mode.h,
                });
            }
            Request::ShowPhysical { id, shown } => self.display_mut(*id)?.shown = *shown,
            Request::SetScale { id, scale } => {
                if !self.state.limits.scales.contains(scale) {
                    return Err(refused(ApiError::BadSize, "Такого масштабу немає."));
                }
                self.display_mut(*id)?.mode.scale = *scale;
            }
            Request::SetPrimary { id } => {
                self.display_mut(*id)?;
                self.state.primary = Some(*id);
            }
            Request::Place { id, x, y } => {
                if !connected {
                    return Err(refused(
                        ApiError::Unsupported,
                        "Монітори ще не на стільниці.",
                    ));
                }
                let d = self.display_mut(*id)?;
                if let Some(r) = &mut d.desk {
                    r.x = *x;
                    r.y = *y;
                }
            }
            Request::SetQuality { mbit } => {
                if !(5..=100).contains(mbit) {
                    return Err(refused(ApiError::BadArgument, "Від 5 до 100 Мбіт/с."));
                }
                s.encoding.bitrate = *mbit;
            }
            Request::SetSharp { on } => {
                s.encoding.sharp_by_size &= *on == s.encoding.sharp;
                s.encoding.sharp = *on;
            }
            Request::SetCodec { codec, cpu } => {
                s.encoding.codec = match codec {
                    CodecChoice::H264 => Codec::H264,
                    CodecChoice::H265 | CodecChoice::Auto => Codec::H265,
                };
                s.encoding.cpu = *cpu;
            }
            Request::SetCounting { counting } => s.counting = Some(*counting),
            Request::PairNewHeadset | Request::StopPairing | Request::PairingCode { .. }
                if !self.pairing_steps =>
            {
                return Err(refused(
                    ApiError::Unsupported,
                    "Сполучення з вікна з'явиться в наступній версії.",
                ));
            }
            Request::PairNewHeadset => {
                s.pairing = Some(Pairing {
                    seconds_left: PAIR_SECONDS,
                    seconds_total: PAIR_SECONDS,
                    ..Default::default()
                });
                self.pairing_since = Some(Instant::now());
            }
            Request::StopPairing => {
                s.pairing = None;
                s.nearby.clear();
                self.pairing_since = None;
            }
            Request::PairingCode { code } => {
                let Some(p) = &mut s.pairing else {
                    return Err(refused(ApiError::NotAllowed, "Сполучення не відкрито."));
                };
                let Some(name) = p.code_for.clone() else {
                    return Err(refused(
                        ApiError::NotAllowed,
                        "Жоден шолом не чекає на код.",
                    ));
                };
                if code == WRONG_CODE || code.len() != 6 {
                    p.last = Some(PairResult::Wrong);
                } else {
                    let taken = s.headsets.iter().filter(|h| h.name == name).count();
                    let shown = if taken == 0 {
                        name
                    } else {
                        format!("{name} ({})", taken + 1)
                    };
                    let key = format!("{:06x}", 0xa0000 + s.headsets.len());
                    s.headsets.push(headset(&key, &shown, 0));
                    // Pairing closes once a headset is paired.
                    s.pairing = None;
                    s.nearby.clear();
                    self.pairing_since = None;
                }
            }
            Request::ForgetHeadset { headset } => {
                let before = s.headsets.len();
                s.headsets
                    .retain(|h| h.key != *headset && h.name != *headset);
                if s.headsets.len() == before {
                    return Err(refused(ApiError::BadArgument, "Такого шолома немає."));
                }
            }
            Request::ForgetAll => s.headsets.clear(),
            Request::SoundToHeadset { on } => s.sound.to_headset = *on,
            // As the service: only while the headset shares its volume.
            Request::Volume { level } => match &mut s.sound.volume {
                Some(v) => v.level = (*level).min(100),
                None => {
                    return Err(refused(
                        ApiError::Unsupported,
                        "Гучність цього шолома з ПК не змінюється: лише його кнопками.",
                    ));
                }
            },
            Request::SetMic { .. } => {
                return Err(refused(
                    ApiError::Unsupported,
                    "Мікрофон вмикають і вимикають у шоломі.",
                ));
            }
            // The window never asks for one; a made-up PC has no picture.
            Request::Screenshot { .. } => {
                return Err(refused(ApiError::Capture, "Знімка немає."));
            }
            Request::Autostart { on } => s.autostart = Some(*on),
            Request::WindowLook {
                theme,
                palette,
                language,
            } => {
                s.window = WindowLook {
                    theme: *theme,
                    palette: palette.clone(),
                    language: language.clone(),
                }
            }
            Request::ShowSettings | Request::Quit => {}
        }
        Ok(None)
    }
}

fn refused(code: ApiError, message: &str) -> ClientError {
    ClientError::Refused {
        code,
        message: message.into(),
    }
}

impl Client for Mock {
    fn get_state(&self) -> Result<Snapshot, ClientError> {
        let mut i = self.inner.lock().unwrap();
        if !i.running {
            return Err(ClientError::NotRunning);
        }
        if i.silent {
            return Err(ClientError::TimedOut);
        }
        if i.old {
            return Err(ClientError::TooOld);
        }
        i.tick();
        Ok(Snapshot {
            revision: i.revision,
            state: Box::new(i.state.clone()),
        })
    }

    fn call(&self, request: &Request) -> Result<Answer, ClientError> {
        let mut i = self.inner.lock().unwrap();
        if !i.running {
            return Err(ClientError::NotRunning);
        }
        i.calls.push(request.clone());
        if let Some(e) = i.refuse.take() {
            return Err(e);
        }
        let id = i.apply(request)?;
        // As the service: the state with the change goes out before the answer.
        i.publish();
        Ok(Answer {
            id,
            seconds_left: matches!(request, Request::PairNewHeadset).then_some(PAIR_SECONDS),
            revision: i.revision,
            ..Default::default()
        })
    }

    fn watch(&self, sink: Sink) -> Result<(), ClientError> {
        let live = {
            let mut i = self.inner.lock().unwrap();
            i.sink = Some(Arc::new(sink));
            i.live
        };
        if live {
            // The pairing window counts down without the window asking.
            let handle = self.handle();
            std::thread::Builder::new()
                .name("mock-clock".into())
                .spawn(move || {
                    loop {
                        std::thread::sleep(Duration::from_millis(500));
                        if Arc::strong_count(&handle.inner) < 2 {
                            return;
                        }
                        handle.tick();
                    }
                })
                .map_err(|e| ClientError::Broken(e.to_string()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_change_the_state_and_answer_its_revision() {
        let m = Mock::connected();
        let a = m
            .call(&Request::AddMonitor {
                w: 1920,
                h: 1080,
                scale: 125,
            })
            .unwrap();
        let id = a.id.unwrap();
        assert_eq!(a.revision, 2);
        let s = m.state();
        let d = s.display(id).unwrap();
        assert_eq!((d.name.as_str(), d.mode.scale), ("Prostir-4", 125));
        assert_eq!(
            (d.desk, d.state),
            (None, DisplayState::Starting),
            "being placed"
        );
        m.handle().place_new();
        let d = m.state().display(id).unwrap().clone();
        assert!(d.desk.is_some() && d.state == DisplayState::Running);
        m.call(&Request::SetPrimary { id }).unwrap();
        let a = m.call(&Request::Place { id, x: -1920, y: 0 }).unwrap();
        assert_eq!(a.revision, 5);
        assert_eq!(m.state().primary, Some(id));
        assert_eq!(m.state().display(id).unwrap().desk.unwrap().x, -1920);
        m.call(&Request::RemoveMonitor { id }).unwrap();
        assert!(m.state().display(id).is_none());
        assert!(matches!(
            m.call(&Request::RemoveMonitor { id: 4 }),
            Err(ClientError::Refused {
                code: ApiError::UnknownDisplay,
                ..
            })
        ));
        assert!(
            m.call(&Request::Volume { level: 3 })
                .unwrap_err()
                .unsupported()
        );
        assert!(
            m.call(&Request::SetMic { on: false })
                .unwrap_err()
                .unsupported()
        );
        assert!(m.call(&Request::PairNewHeadset).unwrap_err().unsupported());
    }

    #[test]
    fn without_a_headset_monitors_are_kept_for_the_next() {
        let m = Mock::waiting();
        let id = m
            .call(&Request::AddMonitor {
                w: 1920,
                h: 1080,
                scale: 100,
            })
            .unwrap()
            .id
            .unwrap();
        let s = m.state();
        let d = s.display(id).unwrap();
        assert_eq!((d.state, d.desk), (DisplayState::Kept, None));
        assert!(
            m.call(&Request::Place { id, x: 0, y: 0 })
                .unwrap_err()
                .unsupported()
        );
    }

    #[test]
    fn pairing_as_step_6_8_takes_a_right_code_and_closes() {
        let m = Mock::connected().pairing();
        m.call(&Request::PairingCode {
            code: WRONG_CODE.into(),
        })
        .unwrap();
        assert_eq!(m.state().pairing.unwrap().last, Some(PairResult::Wrong));
        m.call(&Request::PairingCode {
            code: "482913".into(),
        })
        .unwrap();
        let s = m.state();
        assert!(s.pairing.is_none());
        assert_eq!(s.headsets.last().unwrap().name, "Quest 3S (2)");
    }

    #[test]
    fn not_running_says_so() {
        let m = Mock::not_running();
        assert_eq!(m.get_state(), Err(ClientError::NotRunning));
        assert_eq!(m.call(&Request::StopPairing), Err(ClientError::NotRunning));
    }
}
