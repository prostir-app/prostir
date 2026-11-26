//! A frame of the headset takes no memory (step 7, 7; owner, 2026-11-21): the model,
//! the hands and the menu's view as the shell runs them each frame, with the network
//! and the system quiet, counted by an allocator that counts this thread's
//! allocations. A quiet frame, a ray on a monitor or on the menu, and a drag of a
//! monitor with the menu closed or open (the main display carrying the one glued
//! beside it, the magnet asked for the two each frame, step 8.5), or over the seam
//! between two glued ones (the magnet turning it there), allocate nothing; the menu's
//! state is made
//! anew only when what it shows changed. Pointing about the menu's picture, once each
//! button was pointed at, draws nothing and takes no memory. Step 8's hands (the
//! preview build's trial): a fingertip touching a monitor, the ray as the mouse with
//! `MouseTo` each frame, a trigger clicking, holding and its stick scrolling (every
//! message of the input queued for the PC as the shell queues them), a fist carrying a
//! monitor past its neighbour (the magnet), the ray giving way to touch and back, the
//! input recorded, all take none. The hands' layer is filled each frame too (step 8.2):
//! a pinch held on a monitor, its rings, the take's wave and the ring where the hand
//! holds it, none; nor do the runtime's models of a hand and a controller (step 8.6).
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
use prostir::interaction::{Button, FrameInput, HandState, Hands, Head, Joints, Menu, Menus};
use prostir::lang::{self, LangState};
use prostir::layers::{self, Plan, Ready};
use prostir::layout::{Desk, Placement, Ray};
use prostir::net::NetEvent;
use prostir::outbox::{Lanes, Outbox};
use prostir::platform::store::Store as _;
use prostir::session::NetCmd;
use prostir::settings::Settings;
use prostir::ui::Action;
use prostir::view::View;
use prostir::view::fx::{Effects, Loaded};
use prostir_proto::link::{LinkInfo, RecordSender};
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
    /// The person's keyboard as the headset tracks it, given in each frame (step 8.7).
    keyboard: Option<prostir::keyboard::Seen>,
    /// The hands, rays and effects drawn (step 8.2), and the most in a frame.
    effects: Effects,
    most_effects: usize,
    /// The runtime's models of the hands and controllers (step 8.6): which the shell
    /// has, and the frames a hand's and a controller's were drawn.
    loaded: Loaded,
    hand_meshes: u64,
    controller_meshes: u64,
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
    /// Step 8: `MouseTo` sent, frames a fingertip pressed a monitor, frames the right
    /// hand's ray was shown and not.
    mouse_tos: u64,
    pressed: u64,
    rays: (u64, u64),
    /// Step 8.4b: the input's messages queued for the PC as the shell queues them (see
    /// [`Loop::queue`]), and how many of each: buttons, scrolls, touches.
    outbox: Outbox,
    lanes: Lanes,
    clicks: u64,
    scrolls: u64,
    touches: u64,
}

/// The network, taking whatever it is given.
struct Sink;

impl RecordSender for Sink {
    fn send(&mut self, _: &[&[u8]]) -> std::io::Result<()> {
        Ok(())
    }
    fn finish(&mut self) -> std::io::Result<()> {
        Ok(())
    }
    fn set_pace(&mut self, _: u64) {}
    fn link_info(&mut self) -> Option<LinkInfo> {
        None
    }
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
    fn pc_cursor(&self, _: PcId, _: u32) -> bool {
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
        self.mouse_tos += self
            .fx
            .iter()
            .filter(|e| matches!(e, Effect::Send(_, ClientMsg::MouseTo { .. })))
            .count() as u64;
        self.queue();
    }

    /// The input's messages among the effects queued for the PC, as the shell queues
    /// them, and sent on (here, on this thread: counted too).
    fn queue(&mut self) {
        for e in &self.fx {
            let Effect::Send(_, msg) = e else { continue };
            match msg {
                ClientMsg::MouseButton { .. } => self.clicks += 1,
                ClientMsg::MouseScroll { .. } => self.scrolls += 1,
                ClientMsg::Touch { .. } => self.touches += 1,
                ClientMsg::MouseTo { .. } => {}
                _ => continue,
            }
            self.outbox.push(msg.clone()).expect("queued");
            let pick = self.outbox.next(&mut self.lanes).expect("open");
            if let Some(sent) = self.lanes.send(pick, &mut Sink).expect("sent") {
                self.outbox.sent(sent);
            }
        }
    }

    fn handle(&mut self, event: Event) {
        self.m.handle(self.now, event);
        self.take();
    }

    fn frame(&mut self, right: HandState) {
        self.frame_with(right, None);
    }

    /// A frame with the right hand as `right` and its joints `joints`.
    fn frame_with(&mut self, right: HandState, joints: Option<&Joints>) {
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
            joints: [None, joints.copied()],
            menu_button: false,
            menu_gesture: false,
            align: false,
            keyboard: self.keyboard,
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
        self.pressed += u64::from(self.hands.near_monitor(1).is_some_and(|n| n.pressed));
        if input.hands[1].ray.is_some() {
            if self.hands.ray(1).is_some() {
                self.rays.0 += 1;
            } else {
                self.rays.1 += 1;
            }
        }
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
        // The hands, rays and effects of the hands' layer.
        self.effects
            .fill(&mut self.m, &self.hands, &input, self.loaded, now);
        let (list, meshes) = self.effects.list();
        self.most_effects = self.most_effects.max(list.len());
        self.hand_meshes += u64::from(meshes.hands[1].is_some());
        self.controller_meshes += u64::from(meshes.controllers[1]);
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

    /// The same as [`Loop::measure`], the right hand's joints too.
    #[cfg(feature = "preview")]
    fn measure_with(
        &mut self,
        frames: u32,
        right: impl Fn(u32) -> (HandState, Option<Joints>),
    ) -> u64 {
        for i in 0..30 {
            let (hand, joints) = right(i);
            self.frame_with(hand, joints.as_ref());
        }
        let before = allocations();
        for i in 0..frames {
            let (hand, joints) = right(30 + i);
            self.frame_with(hand, joints.as_ref());
        }
        allocations() - before
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
    connected_with(layers_max, |_| {})
}

/// [`connected`], the start changed by `change` first.
fn connected_with(layers_max: u8, change: impl FnOnce(&mut Start)) -> Loop {
    let now = Instant::now();
    let mut start = Start {
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
    };
    change(&mut start);
    let m = Model::new(start);
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
        keyboard: None,
        effects: Effects::new(),
        most_effects: 0,
        loaded: Loaded::default(),
        hand_meshes: 0,
        controller_meshes: 0,
        #[cfg(feature = "preview")]
        label: prostir::metrics::Label::new(),
        #[cfg(feature = "preview")]
        panel: prostir::ui::Panel::new(),
        #[cfg(feature = "preview")]
        bytes: 0,
        #[cfg(feature = "preview")]
        redrawn: 0,
        frames: 0,
        mouse_tos: 0,
        pressed: 0,
        rays: (0, 0),
        outbox: Outbox::default(),
        lanes: Lanes::default(),
        clicks: 0,
        scrolls: 0,
        touches: 0,
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
    let second = |l: &Loop| {
        l.m.world()
            .placement(DisplayKey::new(PC, DisplayId(2)))
            .expect("placed")
            .position
    };
    let beside_was = second(&l);
    // The main display, glued beside the other, carries it (step 8.5), the magnet
    // asked for the group every frame.
    assert_eq!(l.measure(300, drag), (0, 0), "a drag, the menu closed");
    assert!(monitor(&l), "the drag held a monitor");
    assert!(
        second(&l).distance(beside_was) > 1e-3,
        "the other went along"
    );
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

/// The PC's mouse pointer moving over the monitors every frame, from one to the other
/// and off them, changing between looks it sent before (an animated pointer): its layer
/// is planned each frame where the PC says, with no memory taken. The looks came
/// before the frames counted (the only time they take any).
#[test]
fn the_pcs_pointer_takes_no_memory() {
    let mut l = connected(16);
    for shape in 1..=3u32 {
        l.handle(Event::Net(NetEvent::CursorShape {
            pc: PC,
            shape,
            look: prostir::cursors::Look {
                w: 32,
                h: 32,
                hot_x: 4,
                hot_y: 2,
                rgba: vec![shape as u8; 32 * 32 * 4],
            },
        }));
    }
    let pointers = |l: &Loop| {
        l.plan
            .layers()
            .iter()
            .filter(|layer| matches!(layer, layers::Layer::PcCursor { .. }))
            .count()
    };
    let mut drawn = 0;
    for round in 0..2 {
        let before = allocations();
        for i in 0u32..300 {
            // Across monitor 1, then 2, then off both for a moment; a look a frame.
            let display = match (i / 100) % 3 {
                0 => Some(DisplayId(1)),
                1 => Some(DisplayId(2)),
                _ => (i % 2 == 0).then_some(DisplayId(1)),
            };
            l.handle(Event::Net(NetEvent::CursorAt {
                pc: PC,
                display,
                u: (i * 211 % 65_536) as u16,
                v: (i * 97 % 65_536) as u16,
                shape: 1 + i % 3,
            }));
            l.frame(HandState::default());
            drawn += pointers(&l);
        }
        // The first round makes room for what the model keeps; the next takes none.
        if round > 0 {
            assert_eq!(allocations() - before, 0, "round {round}");
        }
    }
    assert!(drawn > 400, "drawn in {drawn} frames");
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

/// A third monitor dragged over the seam between the two glued ones: the magnet turns
/// it there as "Line up" would, every frame, and that takes no memory either.
#[test]
fn a_drag_over_a_seam_takes_no_memory() {
    let mut l = connected(16);
    let part = Displays {
        displays: vec![monitor(1), monitor(2), monitor(3)],
        primary: Some(DisplayId(1)),
    };
    l.handle(Event::Net(NetEvent::Part(PC, HostMsg::Displays { part })));
    l.handle(Event::StreamStarted {
        id: DisplayKey::new(PC, DisplayId(3)),
        generation: prostir_proto::state::Gen(1),
        codec: prostir_proto::Codec::H264,
        size: (2560, 1440),
    });
    for _ in 0..200 {
        l.frame(HandState::default());
    }
    let key = |n: u32| DisplayKey::new(PC, DisplayId(n));
    let world = l.m.world();
    let (one, three) = (
        *world.shown(key(1)).expect("drawn"),
        *world.shown(key(3)).expect("drawn"),
    );
    // Over the seam between the first and the one left of it, a hand's 2 cm above.
    let h = |p: &Placement| p.width * 1440.0 / 2560.0;
    let corner = one.position + Vec3::new(-one.width / 2.0, h(&one) / 2.0, 0.0);
    let over = corner + Vec3::new(0.1, h(&three) / 2.0 + 0.02, 0.0);
    // The grip pressed on the third monitor, then held while the ray goes from it to
    // over the seam, and stays there swaying a little. The controller just under the
    // eye, so the monitor, moved on the sphere round the eye as its ray turns, goes
    // there.
    let hand = Vec3::new(0.0, -0.05, 0.0);
    let drag = move |i: u32| {
        let mut grab = Button::default();
        grab.update(1.0);
        if i > 0 {
            grab.update(1.0);
        }
        let t = (i as f32 / 60.0).min(1.0);
        let at = three.position.lerp(over, t) + Vec3::new((i as f32 * 0.05).sin() * 0.01, 0.0, 0.0);
        HandState {
            ray: Some(Ray {
                origin: hand,
                dir: (at - hand).normalize(),
            }),
            grab,
            ..HandState::default()
        }
    };
    for i in 0..90 {
        l.frame(drag(i));
    }
    let held = l.hands.held().find_map(prostir::ids::Thing::display);
    assert_eq!(held, Some(key(3)), "the drag holds the third monitor");
    // Over the first, turned about its own middle to face the axis: no longer square
    // to the edge it hangs from.
    let stick = l.m.magnet(key(3)).expect("the magnet takes it");
    assert_eq!((stick.neighbour, stick.across), (key(1), false));
    let level = |p: &Placement| (p.rotation * Vec3::Z).with_y(0.0).normalize();
    assert!(
        level(&stick.placement).dot(level(&one)) < 0.99,
        "turned: {stick:?}"
    );
    let before = allocations();
    for i in 90..390 {
        l.frame(drag(i));
    }
    assert_eq!(allocations() - before, 0, "a drag over a seam");
}

/// The effects of the hands' layer (step 8.2): a pinch held on a monitor shows a ring
/// under it and one closing in, takes it with a wave, and a ring shows where the hand
/// holds it while it is carried, all without memory. (Taking and letting go change the
/// arrangement, which is no frame's work: they are left out of the count.)
#[test]
fn the_effects_take_no_memory() {
    let mut l = connected(16);
    let target =
        l.m.world()
            .shown(DisplayKey::new(PC, DisplayId(1)))
            .expect("drawn")
            .position;
    // Pinched from the 30th frame and held.
    let pinch = move |i: u32| {
        let select = Button {
            down: i >= 30,
            pressed: i == 30,
            released: false,
        };
        let drift = Vec3::new((i as f32 * 0.03).sin() * 0.002, 0.0, 0.0);
        HandState {
            ray: ray_at(target + drift),
            select,
            is_hand: true,
            ..HandState::default()
        }
    };
    for i in 0..30 {
        l.frame(pinch(i));
    }
    // Held towards taking: a ring under the pinch, then one closing in.
    let before = allocations();
    let mut i = 30;
    while l.hands.held().next().is_none() && i < 60 {
        if l.hands
            .pressing(1)
            .is_some_and(|(_, k)| k.is_some_and(|k| k > 0.9))
        {
            break;
        }
        l.frame(pinch(i));
        i += 1;
    }
    assert_eq!(allocations() - before, 0, "a pinch held towards taking");
    assert!(l.hands.pressing(1).is_some(), "pressed");
    let most = l.most_effects;
    // Taken (not counted), then carried with its wave and the ring where it is held.
    while l.hands.held().next().is_none() && i < 80 {
        l.frame(pinch(i));
        i += 1;
    }
    assert!(
        l.hands.held().any(|t| t.display().is_some()),
        "the pinch holds the monitor"
    );
    let before = allocations();
    for n in i..i + 300 {
        l.frame(pinch(n));
    }
    assert_eq!(allocations() - before, 0, "carrying a monitor");
    assert!(most >= 2 && l.most_effects >= 2, "drawn: {most}");
}

/// A right hand with its index fingertip at `tip`, fingers forward: pointing (the
/// other fingers curled) or a fist.
#[cfg(feature = "preview")]
fn right_hand(tip: Vec3, fist: bool) -> Joints {
    let palm = tip - Vec3::new(0.025, 0.0, -0.1);
    let mut j = [(palm, 0.01); 26];
    j[1].0 = palm + Vec3::new(0.0, 0.0, 0.06);
    j[12].0 = palm + Vec3::new(0.0, 0.0, -0.03);
    for (t, dx) in [(10, 0.025), (15, -0.01), (20, -0.025), (25, -0.04)] {
        let out = t == 10 && !fist;
        j[t].0 = palm
            + if out {
                Vec3::new(dx, 0.0, -0.1)
            } else {
                Vec3::new(dx, -0.02, -0.025)
            };
    }
    j[5].0 = palm + Vec3::new(0.06, 0.0, -0.04);
    j
}

/// Step 8's hands, the preview build's trial on (see the module): touching a monitor,
/// the ray as the mouse, a fist carrying a monitor past its neighbour, the ray giving
/// way to touch and back, and the input recorded, each 300 frames without memory.
#[cfg(feature = "preview")]
#[test]
fn step_8s_hands_take_no_memory() {
    use prostir::interaction::Take;
    use prostir::settings::HandMode;
    let mut l = connected(16);
    let part = HostCaps {
        features: vec![
            prostir_proto::state::caps::pc::GAZE,
            prostir_proto::state::caps::pc::MOVE_MOUSE,
            prostir_proto::state::caps::pc::INPUT,
        ],
    };
    l.handle(Event::Net(NetEvent::Part(PC, HostMsg::HostCaps { part })));
    let part = prostir_proto::state::Access::MAIN;
    l.handle(Event::Net(NetEvent::Part(PC, HostMsg::Access { part })));
    for action in [
        Action::Hands(HandMode::Control),
        Action::DevMonitors(true),
        Action::DevTake(Take::Fist),
    ] {
        l.handle(Event::Menu(action, By::Elsewhere));
    }
    let one =
        *l.m.world()
            .shown(DisplayKey::new(PC, DisplayId(1)))
            .expect("drawn");
    let before = |z: f32| one.position + one.rotation * Vec3::new(0.1, 0.0, z);
    let hand = HandState {
        is_hand: true,
        ..HandState::default()
    };

    // A fingertip in and out of the monitor's surface: touches, again and again.
    let touch = |i: u32| {
        let z = 0.03 - 0.035 * ((i as f32 * 0.2).sin() * 0.5 + 0.5);
        (hand, Some(right_hand(before(z), false)))
    };
    assert_eq!(l.measure_with(300, touch), 0, "touching a monitor");
    assert!(l.pressed > 30, "pressed in {} frames", l.pressed);
    assert!(l.touches > 10, "touches sent: {}", l.touches);
    // The same with the runtime's models (step 8.6): the hand drawn as its model.
    l.loaded = Loaded {
        controllers: [true; 2],
        hands: [true; 2],
    };
    assert_eq!(l.measure_with(300, touch), 0, "a hand's model touching");
    assert!(l.hand_meshes > 250, "hand models: {}", l.hand_meshes);

    // The ray as the mouse: a controller's, a little further each frame.
    let target = one.position;
    let mouse = |i: u32| {
        let t = target + Vec3::new((i as f32 * 0.05).sin() * 0.2, 0.0, 0.0);
        let ray = ray_at(t);
        (
            HandState {
                ray,
                ..HandState::default()
            },
            None,
        )
    };
    let sent = l.mouse_tos;
    assert_eq!(l.measure_with(300, mouse), 0, "the ray as the mouse");
    assert!(
        l.mouse_tos - sent > 250,
        "MouseTo {} times",
        l.mouse_tos - sent
    );
    // A controller's model where it is held, in place of its dot.
    assert!(
        l.controller_meshes > 250,
        "controller models: {}",
        l.controller_meshes
    );
    l.loaded = Loaded::default();

    // A controller's trigger on the monitor: clicks, drags and holds (its place said
    // again while held), and its stick scrolls between the presses.
    let press = |i: u32| {
        let t = target + Vec3::new((i as f32 * 0.02).sin() * 0.1, 0.0, 0.0);
        let at = i % 80;
        let select = Button {
            down: at < 60,
            pressed: at == 0,
            released: at == 60,
        };
        let stick = glam::Vec2::new(0.0, if at < 60 { 0.0 } else { 0.9 });
        (
            HandState {
                ray: ray_at(if (20..50).contains(&at) { target } else { t }),
                select,
                stick,
                ..HandState::default()
            },
            None,
        )
    };
    let (clicks, scrolls, tos) = (l.clicks, l.scrolls, l.mouse_tos);
    assert_eq!(l.measure_with(300, press), 0, "clicks, holds and scrolls");
    assert!(l.clicks - clicks >= 6, "buttons {}", l.clicks - clicks);
    assert!(l.scrolls - scrolls >= 3, "scrolls {}", l.scrolls - scrolls);
    assert!(l.mouse_tos - tos > 50, "MouseTo {}", l.mouse_tos - tos);

    // The ray gives way to touch and comes back, the fingertip 30 to 5 cm away.
    let switch = |i: u32| {
        let z = 0.05 + 0.25 * ((i as f32 * 0.05).sin() * 0.5 + 0.5);
        let joints = right_hand(before(z), false);
        let ray = ray_at(target);
        (
            HandState {
                ray,
                is_hand: true,
                ..HandState::default()
            },
            Some(joints),
        )
    };
    assert_eq!(l.measure_with(300, switch), 0, "touch and the ray in turn");
    assert!(
        l.rays.0 > 30 && l.rays.1 > 30,
        "the ray shown and not: {:?}",
        l.rays
    );

    // Recorded meanwhile.
    l.handle(Event::Menu(Action::DevRecord, By::Elsewhere));
    assert_eq!(l.measure_with(300, switch), 0, "recorded");

    // A fist right after pointing takes monitor 1, then carries it to and fro past
    // monitor 2, the magnet's edge lit when near.
    let two =
        *l.m.world()
            .shown(DisplayKey::new(PC, DisplayId(2)))
            .expect("drawn");
    let palm_at = |i: u32| {
        let k = ((i as f32) * 0.03).sin() * 0.5 + 0.5;
        Vec3::new(0.05 + 0.1 * k, -0.3, -0.35)
    };
    let carry = move |i: u32| {
        let tip = palm_at(i) + Vec3::new(0.025, 0.0, -0.1);
        let ray = (i < 3)
            .then(|| ray_at(two.position.lerp(one.position, 0.9)))
            .flatten();
        (
            HandState {
                ray,
                is_hand: true,
                ..HandState::default()
            },
            Some(right_hand(tip, i >= 3)),
        )
    };
    assert_eq!(l.measure_with(300, carry), 0, "a fist carrying a monitor");
    assert!(
        l.hands.held().any(|t| t.display().is_some()),
        "the fist holds a monitor"
    );
}

/// The window onto the keyboard over the keyboard the headset tracks (step 8.7): eased
/// to it while it moves a little, kept a moment out of view, and the hand's window again
/// once it is gone, with no memory taken.
#[test]
fn a_tracked_keyboard_takes_no_memory() {
    use glam::Quat;
    use prostir::caps::{RoomCaps, RoomWay};
    use prostir::keyboard::Seen;
    let mut l = connected_with(16, |s| {
        s.settings.keyboard_window = true;
        s.caps.room = Some(RoomCaps {
            way: RoomWay::FbPassthrough,
            keyboard_window: true,
        });
        s.caps.keyboard_tracking = true;
    });
    let seen = |i: u32| Seen {
        position: Vec3::new((i as f32 * 0.05).sin() * 0.02, -0.4, -0.45),
        rotation: Quat::IDENTITY,
        width: 0.44,
        depth: 0.14,
    };
    l.keyboard = Some(seen(0));
    l.frame(HandState::default());
    assert!(l.m.keyboard_follows(), "over the keyboard");
    for i in 0..30 {
        l.keyboard = Some(seen(i));
        l.frame(HandState::default());
    }
    let before = allocations();
    for i in 30..330 {
        l.keyboard = Some(seen(i));
        l.frame(HandState::default());
    }
    assert_eq!(allocations() - before, 0, "a keyboard in view");
    l.keyboard = None;
    assert_eq!(
        l.measure(300, |_| HandState::default()),
        (0, 0),
        "out of view, then gone"
    );
    assert!(!l.m.keyboard_follows(), "the hand's window again");
    assert!(l.m.keyboard_window().is_some(), "still shown");
}
