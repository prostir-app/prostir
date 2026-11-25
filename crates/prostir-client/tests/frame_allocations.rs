//! A frame of the headset takes no memory (step 7, 7; owner, 2026-11-21): the model,
//! the hands and the menu's view as the shell runs them each frame, with the network
//! and the system quiet, counted by an allocator that counts this thread's
//! allocations. A quiet frame, a ray on a monitor or on the menu, and a drag of a
//! monitor with the menu closed or open allocate nothing; the menu's state is made
//! anew only when what it shows changed. Pointing about the menu's picture, once each
//! button was pointed at, draws nothing and takes no memory.
//!
//! On the desktop only: the app built for Android has its own counting allocator, and
//! a program has one.
#![cfg(not(target_os = "android"))]
#![expect(
    clippy::disallowed_methods,
    reason = "the test's own starting time; the code under test takes the time as a parameter"
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use glam::Vec3;
use prostir::app::{By, Effect, Event, Model, PlatformEvent, Start};
use prostir::caps::{self, Caps};
use prostir::ids::{DisplayKey, PcId};
use prostir::interaction::{Button, FrameInput, HandState, Hands, Head, Menu, Menus};
use prostir::lang::{self, LangState};
use prostir::layers::{self, Plan, Ready};
use prostir::layout::{Desk, Ray};
use prostir::net::NetEvent;
use prostir::platform::store::Store as _;
use prostir::session::NetCmd;
use prostir::settings::Settings;
use prostir::ui::Action;
use prostir::view::View;
use prostir_proto::state::{
    Display, DisplayId, DisplayKind, DisplayState, Displays, HostCaps, Mode, permission,
};
use prostir_proto::{ClientMsg, HostMsg};

thread_local! {
    /// Allocations made on this thread (other tests run on threads of their own).
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
}

/// The system's allocator, counting.
struct Counting;

fn count() {
    let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
}

// SAFETY: every call is passed on to the system's allocator as it came; counting
// touches only a thread-local number, which needs no memory.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        // SAFETY: as the caller promised for `alloc`.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: as the caller promised for `dealloc`.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        count();
        // SAFETY: as the caller promised for `realloc`.
        unsafe { System.realloc(ptr, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn allocations() -> u64 {
    ALLOCS.with(Cell::get)
}

const PC: PcId = PcId([9; 8]);
const FRAME: Duration = Duration::from_millis(11);

fn addr() -> SocketAddr {
    SocketAddr::new(
        Ipv4Addr::new(192, 168, 1, 20).into(),
        prostir_proto::TCP_PORT,
    )
}

fn monitor(n: u32) -> Display {
    Display {
        id: DisplayId(n),
        kind: DisplayKind::Virtual,
        name: String::new(),
        mode: Mode::new(2560, 1440),
        desk: None,
        state: DisplayState::Running,
    }
}

/// The menus' pictures, with no button anywhere.
struct Pictures;

impl Menus for Pictures {
    fn hover(&mut self, _: Menu, _: Option<(f32, f32)>) {}
    fn press(&mut self, _: Menu, _: (f32, f32)) -> Option<Action> {
        None
    }
}

/// The model, the hands and the menu's view, run as the shell runs them.
struct Loop {
    m: Model,
    hands: Hands,
    view: View,
    fx: Vec<Effect>,
    now: Instant,
    palm_rev: Option<u64>,
    main_rev: Option<u64>,
    /// How many times the menu's state was made.
    builds: u64,
    /// Where the head looks.
    forward: Vec3,
    /// How many `Gaze` messages the model sent.
    gazes: u64,
    /// The frame's layers.
    plan: Plan,
    /// The numbers of "For development" (the preview build): their label and the
    /// menu's panel they are drawn by, the bytes the stream counts, how many times the
    /// label was drawn again.
    #[cfg(feature = "preview")]
    label: prostir::metrics::Label,
    #[cfg(feature = "preview")]
    panel: prostir::ui::Panel,
    #[cfg(feature = "preview")]
    bytes: u64,
    #[cfg(feature = "preview")]
    redrawn: u64,
    frames: u64,
}

/// What the shell has for the layers: every picture drawn.
struct Drawn;

impl Ready for Drawn {
    fn background(&self) -> bool {
        true
    }
    fn keyboard_window(&self) -> bool {
        false
    }
    fn picture(&self, _: DisplayKey) -> Option<(u32, u32)> {
        Some((2560, 1440))
    }
    fn trouble(&self, _: DisplayKey) -> bool {
        false
    }
    fn placeholder(&self) -> bool {
        true
    }
    fn menu(&self) -> bool {
        true
    }
    fn palm(&self) -> bool {
        true
    }
    fn attach(&self) -> bool {
        true
    }
    #[cfg(feature = "preview")]
    fn metrics(&self) -> bool {
        true
    }
    fn hands(&self) -> bool {
        true
    }
}

impl Loop {
    fn take(&mut self) {
        self.m.take_effects(&mut self.fx);
        self.gazes += self
            .fx
            .iter()
            .filter(|e| matches!(e, Effect::Send(_, ClientMsg::Gaze { .. })))
            .count() as u64;
    }

    fn handle(&mut self, event: Event) {
        self.m.handle(self.now, event);
        self.take();
    }

    fn frame(&mut self, right: HandState) {
        self.now += FRAME;
        let now = self.now;
        self.m.end_pass();
        self.take();
        let input = FrameInput {
            head: Some(Head {
                position: Vec3::ZERO,
                forward: self.forward,
                right: Vec3::X,
            }),
            dt: FRAME.as_secs_f32(),
            wall: None,
            focused: true,
            hands: [HandState::default(), right],
            joints: [None, None],
            menu_button: false,
            menu_gesture: false,
            align: false,
        };
        self.m.tick(now, &input);
        self.take();
        self.hands.buttons(now, &input, &mut self.m);
        self.take();
        let rev = Some(self.m.rev());
        if self.m.world().wrist().is_some() && rev != self.palm_rev {
            let _ = self.view.state(&self.m);
            self.palm_rev = rev;
        }
        self.hands.step(now, &input, &mut self.m, &mut Pictures);
        self.take();
        self.m.animate(now, &input);
        self.take();
        if self.m.menu().open {
            let rev = Some(self.m.rev());
            if rev != self.main_rev {
                let _ = self.view.state(&self.m);
                self.main_rev = rev;
                self.builds += 1;
            }
        }
        for thing in self.hands.held() {
            if let Some(id) = thing.display() {
                let _ = self.m.magnet(id);
            }
        }
        // The frame's layers, and the model told when monitors do not fit.
        layers::plan(&self.m, &self.hands, &Drawn, &mut self.plan);
        if let Some(hidden) = self.plan.news() {
            self.handle(Event::Platform(PlatformEvent::LayersHidden(hidden)));
        }
        // The numbers by the monitor looked at: a stream of a different rate each
        // second, and a keyframe now and then.
        self.frames += 1;
        #[cfg(feature = "preview")]
        {
            self.bytes += 40_000 + (self.frames / 91 % 17) * 9_000;
            let (bytes, key) = (self.bytes, 380_000 + (self.frames / 400) as u32 * 70_000);
            let drawn =
                self.label
                    .update(&mut self.panel, &self.m, |_| Some((bytes, Some(key))), now);
            self.redrawn += u64::from(drawn.is_some());
        }
    }

    /// `frames` frames with the right hand as `right` says for each, after 30 to
    /// settle: the allocations and the menu's states made in them.
    fn measure(&mut self, frames: u32, right: impl Fn(u32) -> HandState) -> (u64, u64) {
        for i in 0..30 {
            self.frame(right(i));
        }
        let builds = self.builds;
        let before = allocations();
        for i in 0..frames {
            self.frame(right(30 + i));
        }
        (allocations() - before, self.builds - builds)
    }
}

/// A ray from the right hand to `target`.
fn ray_at(target: Vec3) -> Option<Ray> {
    let origin = Vec3::new(0.2, -0.3, -0.2);
    Some(Ray {
        origin,
        dir: (target - origin).normalize(),
    })
}

/// The model with a PC in a session, two monitors streaming, a second of frames, on a
/// headset whose compositor takes `layers_max` layers.
fn connected(layers_max: u8) -> Loop {
    let now = Instant::now();
    let m = Model::new(Start {
        now,
        settings: Settings::default(),
        desk: Desk::default(),
        keyboard: None,
        profiles: Vec::new(),
        // The built-in texts only: no folder to read.
        lang: LangState::new(
            lang::Loader::new(None).load(lang::Lang::FALLBACK),
            Vec::new(),
        ),
        system_lang: None,
        battery: Some((80, false)),
        volume: Some((40, false)),
        caps: Caps {
            equirect: true,
            cylinder: true,
            color_scale: true,
            refresh_rates: vec![72.0, 90.0],
            layers_max,
            ..Caps::without_extensions(
                "Test headset".into(),
                caps::DECODERS_UNSAID.to_vec(),
                caps::STREAMS_UNSAID,
                None,
            )
        },
        refresh_hz: Some(90.0),
        custom_panorama: false,
        update_from: prostir::platform::store::NoStore.update_hint(),
        mic_permission: permission::UNKNOWN,
        only: Some(addr()),
    });
    let mut l = Loop {
        m,
        hands: Hands::new(),
        view: View::new(),
        fx: Vec::new(),
        now,
        palm_rev: None,
        main_rev: None,
        builds: 0,
        forward: -Vec3::Z,
        gazes: 0,
        plan: Plan::new(),
        #[cfg(feature = "preview")]
        label: prostir::metrics::Label::new(),
        #[cfg(feature = "preview")]
        panel: prostir::ui::Panel::new(),
        #[cfg(feature = "preview")]
        bytes: 0,
        #[cfg(feature = "preview")]
        redrawn: 0,
        frames: 0,
    };
    l.now += Duration::from_millis(1);
    l.m.tick(l.now, &FrameInput::hidden(None));
    l.m.take_effects(&mut l.fx);
    // The fake PC is asked first; no answer (no find secret yet): then dialed.
    let asked = l.fx.iter().find_map(|e| match e {
        Effect::Net(NetCmd::Ask { step, .. }) => Some(*step),
        _ => None,
    });
    if let Some(step) = asked {
        l.handle(Event::Net(NetEvent::Missed { step }));
    }
    let step =
        l.fx.iter()
            .find_map(|e| match e {
                Effect::Net(NetCmd::Dial { step, .. }) => Some(*step),
                _ => None,
            })
            .expect("the round dials the only address");
    l.handle(Event::Net(NetEvent::Dialed { step, addr: addr() }));
    l.handle(Event::Net(NetEvent::Reached {
        step,
        pc: PC,
        addr: addr(),
    }));
    let opened_with = l.m.headset_state();
    l.handle(Event::Connected {
        pc: PC,
        addr: addr(),
        language: None,
        opened_with,
    });
    let part = Displays {
        displays: vec![monitor(1), monitor(2)],
        primary: Some(DisplayId(1)),
    };
    l.handle(Event::Net(NetEvent::Part(PC, HostMsg::Displays { part })));
    // The PC takes the gaze: every frame works out which monitor is looked at.
    let part = HostCaps {
        features: vec![prostir_proto::state::caps::pc::GAZE],
    };
    l.handle(Event::Net(NetEvent::Part(PC, HostMsg::HostCaps { part })));
    for n in [1, 2] {
        l.handle(Event::StreamStarted {
            id: DisplayKey::new(PC, DisplayId(n)),
            generation: prostir_proto::state::Gen(1),
            codec: prostir_proto::Codec::H264,
            size: (2560, 1440),
        });
    }
    // The first run's tips open the menu: read.
    l.handle(Event::Menu(Action::Close, By::Elsewhere));
    for _ in 0..100 {
        l.frame(HandState::default());
    }
    l
}

/// One test, so nothing else runs on this thread between the counts.
#[test]
fn a_frame_takes_no_memory() {
    let mut l = connected(16);
    let target =
        l.m.world()
            .shown(DisplayKey::new(PC, DisplayId(1)))
            .expect("drawn")
            .position;
    assert_eq!(l.measure(300, |_| HandState::default()), (0, 0), "quiet");
    let on_monitor = HandState {
        ray: ray_at(target),
        ..HandState::default()
    };
    assert_eq!(l.measure(300, |_| on_monitor), (0, 0), "a ray on a monitor");
    l.handle(Event::Menu(Action::OpenMenu, By::Elsewhere));
    assert_eq!(
        l.measure(300, |_| on_monitor),
        (0, 0),
        "the menu open, a ray on a monitor"
    );
    let menu = l.m.world().menu().position;
    let on_menu = HandState {
        ray: ray_at(menu),
        ..HandState::default()
    };
    assert_eq!(l.measure(300, |_| on_menu), (0, 0), "a ray on the menu");
    l.handle(Event::Menu(Action::Close, By::Elsewhere));

    // The grip pressed on the monitor, then held while the ray goes to and fro a
    // little (the monitor stays on its side of the map).
    let drag_at = move |target: Vec3, i: u32| {
        let mut grab = Button::default();
        grab.update(1.0);
        if i > 30 {
            grab.update(1.0);
        }
        let t = target + Vec3::new((i as f32 * 0.05).sin() * 0.05, 0.0, 0.0);
        HandState {
            ray: ray_at(t),
            grab,
            ..HandState::default()
        }
    };
    let drag = move |i: u32| drag_at(target, i);
    let monitor = |l: &Loop| l.hands.held().any(|t| t.display().is_some());
    assert_eq!(l.measure(300, drag), (0, 0), "a drag, the menu closed");
    assert!(monitor(&l), "the drag held a monitor");
    let mut grab = Button::default();
    grab.update(1.0);
    grab.update(0.0);
    l.frame(HandState { grab, ..drag(0) });
    assert!(!monitor(&l), "and let it go");
    // The menu opens in front of the first monitor: the second one, beside it.
    l.handle(Event::Menu(Action::OpenMenu, By::Elsewhere));
    let beside =
        l.m.world()
            .shown(DisplayKey::new(PC, DisplayId(2)))
            .expect("drawn")
            .position;
    let drag = move |i: u32| drag_at(beside, i);
    assert_eq!(l.measure(300, drag), (0, 0), "a drag, the menu open");
    assert!(monitor(&l), "the drag held a monitor");
}

/// The gaze goes from one monitor to the other and to none, and back, for a PC that
/// takes it: the monitor looked at is worked out each frame and the PC told of each
/// change, with no memory taken.
#[test]
fn telling_the_gaze_takes_no_memory() {
    let mut l = connected(16);
    let at = |l: &Loop, n: u32| {
        let p = l.m.world().shown(DisplayKey::new(PC, DisplayId(n)));
        (p.expect("drawn").position - Vec3::ZERO).normalize()
    };
    let (one, two) = (at(&l, 1), at(&l, 2));
    // Every 25 frames (275 ms) the head turns to the next of the two monitors and the
    // ceiling.
    let turns = [one, two, Vec3::Y];
    let gazes = l.gazes;
    for round in 0..4 {
        let before = allocations();
        for i in 0..90 {
            l.forward = turns[(i / 25 + round) % 3];
            l.frame(HandState::default());
        }
        // The first round makes room for what the model keeps; later ones take none.
        if round > 0 {
            assert_eq!(allocations() - before, 0, "round {round}");
        }
    }
    assert!(l.gazes - gazes > 6, "told {}", l.gazes - gazes);
}

/// The preview build's numbers by the monitor looked at, in a session as it runs: the
/// stream's rate changes each second and the label says it, the label's picture is
/// drawn again each time (10 s in all, as the log counts), with no memory taken. In
/// the frames before, the label was made and every digit drawn once.
#[cfg(feature = "preview")]
#[test]
fn the_numbers_by_the_monitor_take_no_memory() {
    let mut l = connected(16);
    assert!(l.m.metrics_for().is_some(), "a monitor looked at");
    assert!(l.label.ready(), "drawn");
    assert!(l.label.text().contains("Mbit/s"), "{}", l.label.text());
    let redrawn = l.redrawn;
    assert_eq!(l.measure(900, |_| HandState::default()), (0, 0), "10 s");
    assert!(
        l.redrawn - redrawn >= 8,
        "drawn {} times",
        l.redrawn - redrawn
    );
    // The menu open over it, and a ray on a monitor: the same.
    l.handle(Event::Menu(Action::OpenMenu, By::Elsewhere));
    let target =
        l.m.world()
            .shown(DisplayKey::new(PC, DisplayId(1)))
            .expect("drawn")
            .position;
    let on_monitor = HandState {
        ray: ray_at(target),
        ..HandState::default()
    };
    assert_eq!(l.measure(300, |_| on_monitor), (0, 0), "the menu open");
}

/// On a compositor that takes fewer layers than the frame wants, the plan leaves the
/// farthest monitor out every frame, still without memory, and the person is told
/// before that (not in the frames counted).
#[test]
fn a_frame_over_the_layers_budget_takes_no_memory() {
    let mut l = connected(4);
    assert!(l.plan.hidden() > 0, "a monitor left out");
    assert_eq!(l.plan.layers().len(), 4);
    assert!(l.m.toast().is_some(), "said");
    assert_eq!(l.measure(300, |_| HandState::default()), (0, 0), "quiet");
    let target =
        l.m.world()
            .shown(DisplayKey::new(PC, DisplayId(2)))
            .expect("drawn")
            .position;
    let on_monitor = HandState {
        ray: ray_at(target),
        ..HandState::default()
    };
    assert_eq!(l.measure(300, |_| on_monitor), (0, 0), "a ray on a monitor");
    assert!(l.plan.hidden() > 0);
}

/// The menu's picture as the ray goes over it, button to button and off it, twice: the
/// first time each button's part is drawn (and kept), the second time it is only
/// copied, without memory (`ui::Panel::render_if_dirty`). Each change is a part, never
/// the whole picture.
#[test]
fn pointing_about_the_menu_takes_no_memory() {
    let mut l = connected(16);
    l.handle(Event::Menu(Action::OpenMenu, By::Elsewhere));
    for tab in [
        prostir::ui::Tab::Quick,
        prostir::ui::Tab::Monitors,
        prostir::ui::Tab::Settings(prostir::ui::Section::Pc),
    ] {
        l.handle(Event::Menu(Action::Tab(tab), By::Elsewhere));
        let mut panel = prostir::ui::Panel::new();
        panel.set_state(l.view.state(&l.m));
        assert!(panel.render_if_dirty().is_some());
        assert_eq!(panel.damage(), None, "drawn whole first");
        // Across the picture, row by row, and off it now and then.
        let points: Vec<Option<(f32, f32)>> = (0..prostir::ui::HEIGHT)
            .step_by(24)
            .flat_map(|y| {
                (0..prostir::ui::WIDTH)
                    .step_by(24)
                    .map(move |x| Some((x as f32, y as f32)))
                    .chain([None])
            })
            .collect();
        let pass = |panel: &mut prostir::ui::Panel| {
            let mut parts = 0;
            for &at in &points {
                panel.hover(at);
                if panel.render_if_dirty().is_some() {
                    let part = panel.damage().expect("a part, not the whole picture");
                    assert!(part.w * part.h < prostir::ui::WIDTH * prostir::ui::HEIGHT / 3);
                    parts += 1;
                }
            }
            parts
        };
        let first = pass(&mut panel);
        assert!(first > 20, "{tab:?}: {first} buttons pointed at");
        let before = allocations();
        let second = pass(&mut panel);
        assert_eq!(allocations() - before, 0, "{tab:?}");
        assert_eq!(second, first);
    }
}

/// The counting itself: making the model and its session takes memory, and making
/// the menu's state does.
#[test]
fn the_allocator_counts() {
    let start = allocations();
    #[cfg_attr(not(feature = "preview"), expect(unused_mut))]
    let mut l = connected(16);
    let made = allocations();
    assert!(made > start);
    let _ = prostir::view::build(&l.m);
    assert!(allocations() > made);
    // What the label took each second before it was kept: its words by Fluent, its
    // picture on a pixmap made anew.
    #[cfg(feature = "preview")]
    {
        let before = allocations();
        let text =
            l.m.texts()
                .f("headset-dev-metrics-rate", &[("rate", "31.5".into())]);
        let _ = l.panel.toast_image(&text, 1024, 88);
        assert!(allocations() - before > 5, "{}", allocations() - before);
    }
}
