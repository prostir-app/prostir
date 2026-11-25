use super::super::WantedScreen;
use super::encoders::kept_note;
use super::place::{follow_rescaled, is_the_headsets, origin_at};
use super::*;
use crate::encoder::Input;
use crate::platform::{
    AddModel, Cursor, DesktopCaps, FrameSource, Memory, Origin, Outputs, PhysicalDisplay, Pointer,
    Scales, Windows,
};
use std::sync::{Condvar, Mutex};

/// What the stub desktop was asked to do, in order.
type Calls = Arc<Mutex<Vec<String>>>;

/// A desktop whose `make` waits while its gate is shut, as KScreen thinking.
struct Stub {
    caps: DesktopCaps,
    calls: Calls,
    gate: Arc<(Mutex<bool>, Condvar)>,
    outputs: Arc<Mutex<Outputs>>,
    /// The scale the desktop lists its outputs at: the one an output is made with,
    /// until a test gives another, as a person in the desktop's settings.
    scale: DeskScale,
    /// Making a monitor fails while set.
    refuse: Arc<AtomicBool>,
}

type DeskScale = Arc<Mutex<Option<u16>>>;

struct StubOutput {
    key: OutputKey,
    calls: Calls,
    outputs: Arc<Mutex<Outputs>>,
    scale: DeskScale,
}

impl Drop for StubOutput {
    fn drop(&mut self) {
        self.outputs.lock().unwrap().remove(&self.key);
        self.calls
            .lock()
            .unwrap()
            .push(format!("remove {}", self.key));
    }
}

impl Output for StubOutput {
    fn key(&self) -> &OutputKey {
        &self.key
    }
    fn desk_scale(&self) -> Option<u16> {
        *self.scale.lock().unwrap()
    }
    fn scale_taken(&self, percent: u16) {
        self.calls
            .lock()
            .unwrap()
            .push(format!("scale taken {percent}"));
    }
    fn health(&self) -> OutputHealth {
        OutputHealth::Live
    }
    fn source(&self) -> FrameSource {
        FrameSource {
            input: Input {
                source: String::new(),
                label: self.key.to_string(),
                frame_rate: None,
                live: false,
            },
            origin: Origin::Monitor,
            memory: Memory::System,
            cursor: Cursor::InFrame,
        }
    }
    fn attach(&mut self, _encoder: &Encoder) -> Result<()> {
        Ok(())
    }
    fn detach(&mut self) {}
    fn make_primary(&self) {}
}

impl Desktop for Stub {
    fn caps(&self) -> &DesktopCaps {
        &self.caps
    }
    fn outputs(&self) -> Outputs {
        self.outputs.lock().unwrap().clone()
    }
    fn physical(&self) -> Vec<PhysicalDisplay> {
        Vec::new()
    }
    fn make(&mut self, spec: &VirtualSpec) -> Result<Box<dyn Output>> {
        let key = OutputKey::new(format!("Prostir-{}", spec.number));
        self.calls.lock().unwrap().push(format!("make {key}"));
        if self.refuse.load(Ordering::Relaxed) {
            bail!("the stub desktop makes no {key} now");
        }
        let (shut, ring) = &*self.gate;
        let _open = ring.wait_while(shut.lock().unwrap(), |shut| *shut);
        let geometry = Geometry {
            x: 0,
            y: 0,
            w: spec.w as i32,
            h: spec.h as i32,
            priority: None,
            enabled: true,
            is_virtual: true,
        };
        self.outputs.lock().unwrap().insert(key.clone(), geometry);
        *self.scale.lock().unwrap() = Some(spec.scale);
        Ok(Box::new(StubOutput {
            key,
            calls: self.calls.clone(),
            outputs: self.outputs.clone(),
            scale: self.scale.clone(),
        }))
    }
    fn show(&mut self, key: &OutputKey) -> Result<Box<dyn Output>> {
        bail!("no display {key}")
    }
    fn place(&mut self, _plan: &DeskPlan, _current: &Outputs) -> Result<()> {
        self.calls.lock().unwrap().push("place".into());
        Ok(())
    }
    fn windows(&mut self) -> Option<&mut dyn Windows> {
        None
    }
    fn pointer(&mut self) -> Option<&mut dyn Pointer> {
        None
    }
}

/// Monitors `specs` (id, number, width), none streaming (no encoders here).
fn wanted(specs: &[(u32, u32, u32)]) -> Wanted {
    let screens = specs
        .iter()
        .map(|&(id, number, w)| {
            (
                ScreenId::saved(id),
                WantedScreen {
                    what: What::Monitor {
                        spec: MonitorSpec {
                            width: w,
                            height: 360,
                            scale: 1.0,
                        },
                        number,
                    },
                    stream: false,
                    attempt: 0,
                    name: format!("Prostir-{number}"),
                },
            )
        })
        .collect();
    Wanted {
        screens,
        encoding: EncoderConfig {
            codec: Codec::H264,
            bitrate_kbps: 8000,
            backend: Backend::X264,
            sharp: false,
            rows_up: false,
        },
        streaming: true,
        layout: super::super::Layout {
            rows: vec![
                specs
                    .iter()
                    .map(|&(id, _, _)| desk::Entry::new(ScreenId::saved(id)))
                    .collect(),
            ],
            ..Default::default()
        },
        wake_physical: false,
        cursor_apart: false,
    }
}

/// Wanted states that came while the desktop was busy are replaced by the newest:
/// the worker makes what the newest says and never what a state between asked
/// for, and lays the desktop out once, at the end. The step it hangs in shows.
#[test]
fn the_newest_wanted_state_wins_and_the_desktop_is_placed_once() {
    let calls = Calls::default();
    let gate = Arc::new((Mutex::new(true), Condvar::new()));
    let stub = Stub {
        caps: DesktopCaps {
            name: "stub",
            virtual_monitors: Ok(AddModel::OneByOne),
            has_primary: false,
            origin_is_primary: false,
            scales: Scales::Any { min: 50, max: 400 },
            refresh: vec![60],
            screenshot_one_output: true,
            moves_pointer: false,
            cleans_up_after_crash: true,
        },
        calls: calls.clone(),
        gate: gate.clone(),
        outputs: Arc::default(),
        scale: Arc::default(),
        refuse: Arc::default(),
    };
    let (events, received) = crate::events::channel();
    let inbox: Arc<Inbox<Wanted, Job>> = Arc::default();
    let steps = Arc::new(Steps::default());
    let shared = Shared {
        inbox: inbox.clone(),
        steps: steps.clone(),
        router: Arc::new(Router::new()),
        events,
        generation: 1,
    };
    let worker = Worker::with(shared, Box::new(stub), GpuDevices::default());
    let thread = std::thread::spawn(move || worker.run());

    inbox.want(wanted(&[(1, 1, 640)]));
    // The worker hangs in making the first monitor: the step shows.
    let until = Instant::now() + Duration::from_secs(5);
    while !steps
        .now()
        .is_some_and(|(_, what)| what.contains("making the output"))
    {
        assert!(Instant::now() < until, "the step never showed");
        std::thread::sleep(Duration::from_millis(5));
    }
    // Two more states while it is busy: only the last counts.
    inbox.want(wanted(&[(1, 1, 640), (2, 2, 800)]));
    let upto = inbox.want(wanted(&[(1, 1, 640), (3, 2, 320)]));
    *gate.0.lock().unwrap() = false;
    gate.1.notify_all();

    let mut made = Vec::new();
    loop {
        match received.recv_timeout(Duration::from_secs(5)).unwrap() {
            Event::Desk(1, Done::Made { id, .. }) => made.push(id.number()),
            Event::Desk(1, Done::Settled(n)) if n >= upto => break,
            _ => {}
        }
    }
    assert_eq!(made, [1, 3], "never the state in between");
    assert_eq!(
        *calls.lock().unwrap(),
        ["make Prostir-1", "make Prostir-2", "place"],
        "screen 3 took the number 2; one layout"
    );
    assert!(steps.now().is_none(), "no step under way");

    inbox.quit();
    thread.join().unwrap();
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 5, "{calls:?}");
    assert!(calls[3..].iter().all(|c| c.starts_with("remove")));
}

/// A screen that kept its encoder because the wanted one did not start tries the
/// wanted one again only after a while; another wanted one, at once.
#[test]
fn a_kept_encoder_is_tried_again_later() {
    let now = Instant::now();
    let x264 = (Backend::X264, Codec::H264);
    let va = (Backend::Va, Codec::H265);
    assert!(!restart_due(Some(x264), None, x264, now), "as wanted");
    assert!(restart_due(Some(x264), None, va, now), "another wanted");
    let kept = Some((va, now + KEPT_FOR));
    assert!(!restart_due(Some(x264), kept, va, now), "not again at once");
    assert!(restart_due(Some(x264), kept, va, now + KEPT_FOR), "later");
    let other = (Backend::Va, Codec::H264);
    assert!(
        restart_due(Some(x264), kept, other, now),
        "another wanted at once"
    );
    assert_eq!(
        kept_note(Codec::H264, Codec::H265),
        t("pc-worker-run-tests-kept-h-264-hevc-did-not")
    );
}

/// On a desktop whose primary output is always at (0, 0), the plan moves with it.
#[test]
fn a_plan_moves_with_a_primary_at_the_origin() {
    let key = OutputKey::new;
    let positions = [
        (key("a"), -1920, 0),
        (key("b"), 0, 0),
        (key("c"), 640, 1080),
    ];
    assert_eq!(origin_at(&positions, Some(&key("c"))), (640, 1080));
    assert_eq!(origin_at(&positions, Some(&key("x"))), (0, 0));
    assert_eq!(origin_at(&positions, None), (0, 0));
}

/// The headset's plan read back from `SetLayout` is recognised as its plan,
/// shifted as a whole too; a plan that moved one monitor is not.
#[test]
fn the_headsets_plan_is_recognised() {
    let spot = |id, x, y| desk::Spot {
        id,
        x,
        y,
        w: 640,
        h: 360,
    };
    let headset = [spot(0u8, 0, 360), spot(1, 640, 300)];
    let boxes = desk::Boxes::new(
        headset
            .iter()
            .map(|s| {
                let (c, m) = s.wire();
                let size = desk::Size { w: s.w, h: s.h };
                (s.id, desk::SeenBox::from_wire(c, m, size), size)
            })
            .collect(),
    );
    assert!(is_the_headsets(&headset, &boxes));
    let shifted = [spot(0, 0, 60), spot(1, 640, 0)];
    assert!(is_the_headsets(&shifted, &boxes));
    let moved = [spot(0, 0, 360), spot(1, 640, 0)];
    assert!(!is_the_headsets(&moved, &boxes));
    assert!(!is_the_headsets(&moved, &desk::Boxes::new(Vec::new())));
}

/// A monitor made anew (its size did not come back) that cannot be made again leaves
/// no stream behind: the headsets that had it hear that it ended.
#[test]
fn a_monitor_that_cannot_be_made_again_ends_its_stream() {
    let refuse = Arc::new(AtomicBool::new(false));
    let stub = Stub {
        caps: DesktopCaps {
            name: "stub",
            virtual_monitors: Ok(AddModel::OneByOne),
            has_primary: false,
            origin_is_primary: false,
            scales: Scales::Any { min: 50, max: 400 },
            refresh: vec![60],
            screenshot_one_output: true,
            moves_pointer: false,
            cleans_up_after_crash: true,
        },
        calls: Calls::default(),
        gate: Arc::new((Mutex::new(false), Condvar::new())),
        outputs: Arc::default(),
        scale: DeskScale::default(),
        refuse: refuse.clone(),
    };
    let (events, _received) = crate::events::channel();
    let router = Arc::new(Router::new());
    let shared = Shared {
        inbox: Arc::default(),
        steps: Arc::default(),
        router: router.clone(),
        events,
        generation: 1,
    };
    let mut worker = Worker::with(shared, Box::new(stub), GpuDevices::default());
    worker.take(wanted(&[(1, 1, 640)]));
    let id = ScreenId::saved(1);
    worker.make(id);
    assert!(worker.slots[&id].output.is_some());
    // Its stream, as a headset has it.
    let outbox = Arc::new(crate::outbox::Outbox::default());
    let headset = state::HeadsetCaps {
        decoders: vec![state::Decoder {
            codec: 0,
            max_w: 7680,
            max_h: 4320,
        }],
        ..state::HeadsetCaps::default()
    };
    router.attach(1, outbox.clone(), headset, true);
    router.publish(1, &std::collections::BTreeSet::from([id]));
    router.add_stream(id, Codec::H264, "Prostir-1".into());
    router.on_event(
        id,
        crate::encoder::EncoderEvent::Size {
            width: 64,
            height: 32,
        },
    );
    router.on_event(
        id,
        crate::encoder::EncoderEvent::Frame {
            pts_us: 0,
            keyframe: true,
            data: vec![0],
            timing: None,
        },
    );
    assert!(
        outbox
            .sent()
            .iter()
            .any(|m| matches!(m, HostMsg::StreamStart { .. }))
    );

    refuse.store(true, Ordering::Relaxed);
    worker.recreate(id);
    assert!(worker.slots[&id].output.is_none(), "not made again");
    assert!(
        outbox
            .sent()
            .iter()
            .any(|m| matches!(m, HostMsg::StreamEnd { .. })),
        "the stream it had stays"
    );
}

/// A worker of a stub desktop that never waits, on a thread of its own.
struct Harness {
    calls: Calls,
    scale: DeskScale,
    inbox: Arc<Inbox<Wanted, Job>>,
    received: std::sync::mpsc::Receiver<Event>,
    thread: std::thread::JoinHandle<()>,
}

impl Harness {
    fn start() -> Self {
        let calls = Calls::default();
        let scale = DeskScale::default();
        let stub = Stub {
            caps: DesktopCaps {
                name: "stub",
                virtual_monitors: Ok(AddModel::OneByOne),
                has_primary: false,
                origin_is_primary: false,
                scales: Scales::Any { min: 50, max: 400 },
                refresh: vec![60],
                screenshot_one_output: true,
                moves_pointer: false,
                cleans_up_after_crash: true,
            },
            calls: calls.clone(),
            gate: Arc::new((Mutex::new(false), Condvar::new())),
            outputs: Arc::default(),
            scale: scale.clone(),
            refuse: Arc::default(),
        };
        let (events, received) = crate::events::channel();
        let inbox: Arc<Inbox<Wanted, Job>> = Arc::default();
        let shared = Shared {
            inbox: inbox.clone(),
            steps: Arc::default(),
            router: Arc::new(Router::new()),
            events,
            generation: 1,
        };
        let worker = Worker::with(shared, Box::new(stub), GpuDevices::default());
        let thread = std::thread::spawn(move || worker.run());
        Self {
            calls,
            scale,
            inbox,
            received,
            thread,
        }
    }

    /// Hands `wanted` over and waits until the worker did all of it: the jobs it
    /// answered meanwhile.
    fn settle(&self, wanted: Wanted) -> Vec<(JobId, bool)> {
        let upto = self.inbox.want(wanted);
        self.until(upto)
    }

    /// One look after the outputs (as the control loop asks each second), waited for.
    fn tick(&self) {
        let upto = self.inbox.ask(Job::Tick);
        self.until(upto);
    }

    /// Waits until the worker took everything up to `upto`: the jobs it answered.
    fn until(&self, upto: u64) -> Vec<(JobId, bool)> {
        let mut jobs = Vec::new();
        loop {
            match self.received.recv_timeout(Duration::from_secs(5)).unwrap() {
                Event::Desk(_, Done::Job { job, result }) => jobs.push((job, result.is_ok())),
                Event::Desk(_, Done::Settled(n)) if n >= upto => return jobs,
                _ => {}
            }
        }
    }

    fn end(self) -> Vec<String> {
        self.inbox.quit();
        self.thread.join().unwrap();
        self.calls.lock().unwrap().clone()
    }
}

/// A scale given in the desktop's settings before the worker first looked is still
/// taken: the output was listed at its own scale as it was made.
#[test]
fn a_scale_given_before_the_first_look_is_taken() {
    let harness = Harness::start();
    harness.settle(wanted(&[(1, 1, 640)]));
    *harness.scale.lock().unwrap() = Some(150);
    // Taken when seen on two looks in a row.
    harness.tick();
    harness.tick();
    let calls = harness.end();
    assert!(calls.iter().any(|c| c == "scale taken 150"), "{calls:?}");
}

/// A number given again (a monitor removed, another added at once) is made only
/// after the output that had it is gone: the difference removes first.
#[test]
fn a_number_is_made_again_only_after_its_output_is_gone() {
    let harness = Harness::start();
    harness.settle(wanted(&[(1, 1, 640), (2, 2, 800)]));
    harness.settle(wanted(&[(1, 1, 640), (3, 2, 320)]));
    let calls = harness.end();
    let remove = calls.iter().position(|c| c == "remove Prostir-2").unwrap();
    let again = calls.iter().rposition(|c| c == "make Prostir-2").unwrap();
    assert!(remove < again, "{calls:?}");
}

/// A window panel asked for is tried once, however long the state asks for it, and
/// its answer comes once (here: the stub desktop has no windows).
#[test]
fn a_panel_is_tried_once() {
    let harness = Harness::start();
    let mut with_panel = wanted(&[(1, 1, 640)]);
    with_panel.screens.push((
        ScreenId::saved(9),
        WantedScreen {
            what: What::Panel {
                number: 2,
                detach: Some(JobId(7)),
            },
            stream: true,
            attempt: 0,
            name: "Prostir-2".into(),
        },
    ));
    assert_eq!(harness.settle(with_panel.clone()), [(JobId(7), false)]);
    with_panel.streaming = false;
    assert_eq!(harness.settle(with_panel), [], "not tried again");
    harness.end();
}

/// The scale of monitor `id` in `w`, in percent.
fn percent_of(w: &Wanted, id: u32) -> Option<u16> {
    w.screens.iter().find_map(|(i, s)| match &s.what {
        What::Monitor { spec, .. } if *i == ScreenId::saved(id) => Some(spec.percent()),
        _ => None,
    })
}

fn with_scale(mut w: Wanted, id: u32, scale: f64) -> Wanted {
    for (i, s) in &mut w.screens {
        if let What::Monitor { spec, .. } = &mut s.what
            && *i == ScreenId::saved(id)
        {
            spec.scale = scale;
        }
    }
    w
}

/// A scale taken on the desktop stands in what the control loop sent before it heard
/// of it, until it sends the new one, another chosen in Prostir, or no such monitor.
#[test]
fn a_scale_taken_on_the_desktop_stands_until_the_control_loop_has_it() {
    let taken = || BTreeMap::from([(ScreenId::saved(1), (100, 150))]);
    let mut rescaled = taken();
    let mut w = wanted(&[(1, 1, 640), (2, 2, 640)]);
    follow_rescaled(&mut rescaled, &mut w);
    assert_eq!(
        (percent_of(&w, 1), percent_of(&w, 2)),
        (Some(150), Some(100))
    );
    assert_eq!(rescaled.len(), 1, "still followed");
    let mut w = wanted(&[(1, 1, 640)]);
    follow_rescaled(&mut rescaled, &mut w);
    assert_eq!(percent_of(&w, 1), Some(150), "another sent before it heard");
    assert_eq!(rescaled.len(), 1);

    let mut w = with_scale(wanted(&[(1, 1, 640)]), 1, 1.5);
    follow_rescaled(&mut rescaled, &mut w);
    assert!(rescaled.is_empty(), "the control loop has it");

    let mut rescaled = taken();
    let mut w = with_scale(wanted(&[(1, 1, 640)]), 1, 1.25);
    follow_rescaled(&mut rescaled, &mut w);
    assert_eq!(percent_of(&w, 1), Some(125), "chosen in Prostir since");
    assert!(rescaled.is_empty());

    let mut rescaled = taken();
    let mut w = wanted(&[(2, 2, 640)]);
    follow_rescaled(&mut rescaled, &mut w);
    assert!(rescaled.is_empty(), "no such monitor any more");
}
