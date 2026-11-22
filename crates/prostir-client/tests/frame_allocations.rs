//! A frame of the headset takes no memory (step 7, 7; owner, 2026-11-21): the model,
//! the hands and the menu's view as the shell runs them each frame, with the network
//! and the system quiet, counted by an allocator that counts this thread's
//! allocations. A quiet frame, a ray on a monitor or on the menu, and a drag of a
//! monitor with the menu closed or open allocate nothing; the menu's state is made
//! anew only when what it shows changed.

#![expect(
    clippy::disallowed_methods,
    reason = "the test's own starting time; the code under test takes the time as a parameter"
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use glam::Vec3;
use prostir::app::{By, Effect, Event, Model, Start};
use prostir::caps::{self, Caps};
use prostir::ids::{DisplayKey, PcId};
use prostir::interaction::{Button, FrameInput, HandState, Hands, Head, Menu, Menus};
use prostir::lang::{self, LangState};
use prostir::layout::{Desk, Ray};
use prostir::net::NetEvent;
use prostir::platform::store::Store as _;
use prostir::session::NetCmd;
use prostir::settings::Settings;
use prostir::ui::Action;
use prostir::view::View;
use prostir_proto::HostMsg;
use prostir_proto::state::{
    Display, DisplayId, DisplayKind, DisplayState, Displays, Mode, permission,
};

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
}

impl Loop {
    fn handle(&mut self, event: Event) {
        self.m.handle(self.now, event);
        self.m.take_effects(&mut self.fx);
    }

    fn frame(&mut self, right: HandState) {
        self.now += FRAME;
        let now = self.now;
        self.m.end_pass();
        self.m.take_effects(&mut self.fx);
        let input = FrameInput {
            head: Some(Head {
                position: Vec3::ZERO,
                forward: -Vec3::Z,
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
        self.m.take_effects(&mut self.fx);
        self.hands.buttons(now, &input, &mut self.m);
        self.m.take_effects(&mut self.fx);
        let rev = Some(self.m.rev());
        if self.m.world().wrist().is_some() && rev != self.palm_rev {
            let _ = self.view.state(&self.m);
            self.palm_rev = rev;
        }
        self.hands.step(now, &input, &mut self.m, &mut Pictures);
        self.m.take_effects(&mut self.fx);
        self.m.animate(now, &input);
        self.m.take_effects(&mut self.fx);
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

/// The model with a PC in a session, two monitors streaming, a second of frames.
fn connected() -> Loop {
    let now = Instant::now();
    let m = Model::new(Start {
        now,
        settings: Settings::default(),
        desk: Desk::default(),
        keyboard: None,
        profiles: Default::default(),
        // The built-in texts only: no folder to read.
        lang: LangState::new(
            lang::Loader::new(None).load(lang::Lang::FALLBACK),
            Vec::new(),
        ),
        system_lang: None,
        battery: Some((80, false)),
        caps: Caps {
            equirect: true,
            cylinder: true,
            color_scale: true,
            refresh_rates: vec![72.0, 90.0],
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
    for n in [1, 2] {
        l.handle(Event::StreamStarted {
            id: DisplayKey::new(PC, DisplayId(n)),
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
    let mut l = connected();
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

/// The counting itself: making the model and its session takes memory, and making
/// the menu's state does.
#[test]
fn the_allocator_counts() {
    let start = allocations();
    let l = connected();
    let made = allocations();
    assert!(made > start);
    let _ = prostir::view::build(&l.m);
    assert!(allocations() > made);
}
