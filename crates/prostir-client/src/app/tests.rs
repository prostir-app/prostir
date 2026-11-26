//! Scenarios of the model on the desktop (step 7, 10.1), with the time passed in: what
//! the PC says and asks, the menu's actions and the hands go in as the shell gives
//! them, and what comes out is checked: the effects in their order. Every step also
//! checks the menu's view: while [`Model::rev`] stays, what the menu shows stays.

#![expect(
    clippy::disallowed_methods,
    reason = "a test's own starting time; the code under test takes the time as a parameter"
)]

use std::net::{Ipv4Addr, SocketAddr};

use glam::Vec3;
use prostir_proto::HostMsg;
use prostir_proto::request::{Asked, Command};
use prostir_proto::state::{
    Display, DisplayKind, DisplayState, Displays, ErrorCode, Mode, Row, permission,
};

use super::*;
use crate::connecting::Ended;
use crate::ids::Thing;
use crate::interaction::{HandState, Hands, Menu, Menus};
use crate::layout::{Desk, Ray};
use crate::platform::store::Store as _;
use crate::session::{Disconnect, StepId};
use crate::ui::UiState;

mod input;

const MS: Duration = Duration::from_millis(1);
const PC: PcId = PcId([9; 8]);

fn addr() -> SocketAddr {
    SocketAddr::new(
        Ipv4Addr::new(192, 168, 1, 20).into(),
        prostir_proto::TCP_PORT,
    )
}

fn k(n: u32) -> DisplayKey {
    DisplayKey::new(PC, DisplayId(n))
}

/// A headset as the tests had it: panoramas and curved layers, no cameras, two display
/// rates.
fn test_caps() -> Caps {
    Caps {
        equirect: true,
        cylinder: true,
        color_scale: true,
        refresh_rates: vec![72.0, 90.0],
        ..Caps::without_extensions(
            "Test headset".into(),
            crate::caps::DECODERS_UNSAID.to_vec(),
            crate::caps::STREAMS_UNSAID,
            None,
        )
    }
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

fn head() -> Head {
    Head {
        position: Vec3::ZERO,
        forward: -Vec3::Z,
        right: Vec3::X,
    }
}

/// A frame's input: the head, and the right controller as given.
fn frame(right: HandState) -> FrameInput {
    FrameInput {
        head: Some(head()),
        dt: 0.011,
        wall: None,
        focused: true,
        hands: [HandState::default(), right],
        joints: [None, None],
        menu_button: false,
        menu_gesture: false,
        align: false,
        keyboard: None,
    }
}

/// The menus' pictures: every press of the main menu is on this button.
struct Pictures(Option<Action>);

impl Menus for Pictures {
    fn hover(&mut self, _menu: Menu, _at: Option<(f32, f32)>) {}
    fn press(&mut self, menu: Menu, _at: (f32, f32)) -> Option<Action> {
        (menu == Menu::Main).then_some(self.0).flatten()
    }
}

/// The model, the clock, the effects it asked for since the last look, and the menu's
/// view as last made.
struct Sim {
    m: Model,
    now: Instant,
    fx: Vec<Effect>,
    view: Option<(u64, UiState)>,
}

impl Sim {
    /// The model of a selftest: its fake PC is the only address asked and dialed.
    fn new() -> Self {
        Self::with(Some(addr()))
    }

    /// The model, with `only` its fake PC or (`None`) searching the network.
    fn with(only: Option<SocketAddr>) -> Self {
        Self::with_caps(only, test_caps())
    }

    /// The model of a selftest on a headset that can do what `caps` says.
    fn of(caps: Caps) -> Self {
        Self::with_caps(Some(addr()), caps)
    }

    fn with_caps(only: Option<SocketAddr>, caps: Caps) -> Self {
        let now = Instant::now();
        let m = Model::new(Start {
            now,
            settings: Settings::default(),
            desk: Desk::default(),
            keyboard: None,
            profiles: Vec::new(),
            lang: LangState::new(crate::lang::builtin("en"), Vec::new()),
            system_lang: None,
            battery: Some((80, false)),
            volume: Some((40, false)),
            caps,
            refresh_hz: Some(90.0),
            custom_panorama: false,
            update_from: crate::platform::store::NoStore.update_hint(),
            mic_permission: permission::UNKNOWN,
            only,
        });
        let mut sim = Self {
            m,
            now,
            fx: Vec::new(),
            view: None,
        };
        sim.check_view();
        sim
    }

    /// While the model's change stays the same, what the menu shows stays the same.
    fn check_view(&mut self) {
        let made = crate::view::build(&self.m);
        if let Some((rev, before)) = &self.view
            && *rev == self.m.rev()
        {
            assert_eq!(
                before, &made,
                "what the menu shows changed, and the model did not say so"
            );
        }
        self.view = Some((self.m.rev(), made));
    }

    /// What the model asked for since the last look.
    fn take(&mut self) -> Vec<Effect> {
        let mut out = Vec::new();
        self.m.take_effects(&mut out);
        self.fx.append(&mut out);
        std::mem::take(&mut self.fx)
    }

    fn handle(&mut self, event: Event) {
        self.m.handle(self.now, event);
        self.check_view();
    }

    fn net(&mut self, event: NetEvent) {
        self.handle(Event::Net(event));
    }

    fn menu(&mut self, action: Action) {
        self.handle(Event::Menu(action, By::Menu));
    }

    /// `d` passes without frames.
    fn after(&mut self, d: Duration) {
        self.now += d;
        self.m.tick(self.now, &FrameInput::hidden(None));
        self.check_view();
    }

    /// Frames of 11 ms for `d`, the head still.
    fn frames(&mut self, d: Duration) {
        let end = self.now + d;
        while self.now < end {
            self.now += 11 * MS;
            let input = frame(HandState::default());
            self.m.tick(self.now, &input);
            self.m.animate(self.now, &input);
            self.check_view();
        }
    }

    /// The end of a pass: the files it changed.
    fn saves(&mut self) -> Vec<persist::File> {
        self.m.end_pass();
        self.take()
            .into_iter()
            .filter_map(|e| match e {
                Effect::Save(f) => Some(f),
                _ => None,
            })
            .collect()
    }

    /// The session with the PC opens (its only address, as the selftest's), with
    /// `monitors` listed and streaming; the effects so far are taken.
    fn connect(&mut self, monitors: &[u32]) {
        self.after(MS);
        // The fake PC is asked first; no answer (it gave no find secret yet): dialed.
        let mut step = None;
        for _ in 0..2 {
            for e in self.take() {
                match e {
                    Effect::Net(NetCmd::Ask { step: s, .. }) => {
                        self.net(NetEvent::Missed { step: s });
                    }
                    Effect::Net(NetCmd::Dial { step: s, .. }) => step = Some(s),
                    _ => {}
                }
            }
        }
        let step = step.expect("the round dials the only address");
        self.net(NetEvent::Dialed { step, addr: addr() });
        self.net(NetEvent::Reached {
            step,
            pc: PC,
            addr: addr(),
        });
        let opened_with = self.m.headset_state();
        self.handle(Event::Connected {
            pc: PC,
            addr: addr(),
            language: None,
            opened_with,
        });
        self.list(monitors);
        for n in monitors {
            self.stream(*n, Gen(1), (2560, 1440));
        }
        self.frames(Duration::from_secs(1));
        self.take();
        self.saves();
    }

    /// Generation `generation` of monitor `n`'s stream starts, `size` pixels.
    fn stream(&mut self, n: u32, generation: Gen, size: (u32, u32)) {
        self.handle(Event::StreamStarted {
            id: k(n),
            generation,
            codec: Codec::H264,
            size,
        });
    }

    fn list(&mut self, monitors: &[u32]) {
        let part = Displays {
            displays: monitors.iter().map(|n| monitor(*n)).collect(),
            primary: monitors.first().map(|n| DisplayId(*n)),
        };
        self.net(NetEvent::Part(PC, HostMsg::Displays { part }));
    }

    /// The PC asks `command` as request `req`.
    fn command(&mut self, req: u32, command: Command) {
        self.net(NetEvent::Command {
            pc: PC,
            req: ReqId(req),
            asked: Asked::Known(command),
        });
    }
}

/// The messages sent to the PC among `fx`.
fn sent(fx: &[Effect]) -> Vec<&ClientMsg> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Send(pc, msg) | Effect::Tell(pc, msg) if *pc == PC => Some(msg),
            _ => None,
        })
        .collect()
}

fn reply_of(fx: &[Effect], req: u32) -> Option<&Outcome> {
    sent(fx).into_iter().find_map(|m| match m {
        ClientMsg::Reply { req: r, outcome } if r.0 == req => Some(outcome),
        _ => None,
    })
}

/// Scenario 7: the PC's commands are done as the menu does them and answered once:
/// "In front" after the monitors moved, "No limits" after the `Prefs` it changed, and a
/// command this headset does not know as unsupported.
#[test]
fn the_pcs_commands_are_answered() {
    let mut sim = Sim::new();
    sim.connect(&[1, 2]);
    sim.command(8, Command::Recenter);
    let fx = sim.take();
    assert_eq!(reply_of(&fx, 8), Some(&Outcome::Done));
    assert_eq!(sim.saves(), [persist::File::Desk]);

    sim.command(9, Command::NoLimits { on: true });
    let fx = sim.take();
    let msgs = sent(&fx);
    let prefs = msgs
        .iter()
        .position(|m| matches!(m, ClientMsg::Prefs { part } if part.no_limits));
    let reply = msgs
        .iter()
        .position(|m| matches!(m, ClientMsg::Reply { req, .. } if req.0 == 9));
    assert!(
        prefs.is_some() && prefs < reply,
        "Prefs, then the answer: {msgs:?}"
    );
    assert!(sim.m.settings().no_limits);
    assert_eq!(sim.saves(), [persist::File::Settings]);

    sim.net(NetEvent::Command {
        pc: PC,
        req: ReqId(10),
        asked: Asked::Unknown { kind: 200 },
    });
    let fx = sim.take();
    assert!(matches!(
        reply_of(&fx, 10),
        Some(Outcome::Failed { error, .. }) if *error == ErrorCode::UNSUPPORTED
    ));
    // A profile there is none of.
    sim.command(11, Command::Profile { name: "2".into() });
    let fx = sim.take();
    assert!(matches!(
        reply_of(&fx, 11),
        Some(Outcome::Failed { error, .. }) if *error == ErrorCode::NO_SUCH_PROFILE
    ));
}

/// Step 8.3: "Reset" puts a monitor moved and made larger by hand back where the
/// layout places it, at its size; from the menu only on the second press (the first
/// asks again, and the asking goes after a while), from the PC (command 11) at once,
/// answered done after the monitors moved.
#[test]
fn reset_puts_the_arrangement_back() {
    use crate::interaction::Scene as _;
    let mut sim = Sim::new();
    sim.connect(&[1, 2]);
    // As the layout lays them out.
    let preset = sim.m.settings().preset;
    sim.menu(Action::Preset(preset));
    let thing = Thing::Display(k(1));
    let laid = *sim.m.world().placement(k(1)).expect("placed");
    let other = *sim.m.world().placement(k(2)).expect("placed");
    // Where the layout puts a monitor: one of the two places (the order of the PC's
    // desktop follows what the hand did).
    let laid_out = |p: &Placement| {
        [laid, other]
            .iter()
            .any(|l| l.position.distance(p.position) < 1e-3)
    };
    let at = sim.m.hold(thing).expect("taken");
    let moved = Placement {
        position: at.position + Vec3::new(0.1, 0.5, 0.3),
        width: at.width * 1.3,
        ..at
    };
    sim.m.drag(thing, moved, Vec3::ZERO);
    sim.m.let_go(sim.now, thing);
    let here = |sim: &Sim| *sim.m.world().placement(k(1)).expect("placed");
    assert!(here(&sim).position.distance(laid.position) > 0.3, "moved");

    // The first press asks again; once that is over, the next press asks again too.
    sim.menu(Action::ResetArrangement);
    assert!(sim.m.menu().confirm_reset.is_some());
    assert!(
        here(&sim).position.distance(laid.position) > 0.3,
        "asked only"
    );
    sim.after(CONFIRM_FOR + Duration::from_millis(10));
    assert!(sim.m.menu().confirm_reset.is_none(), "the asking went");
    sim.menu(Action::ResetArrangement);
    sim.menu(Action::ResetArrangement);
    assert!(sim.m.menu().confirm_reset.is_none());
    let back = here(&sim);
    assert!(laid_out(&back), "{back:?} {laid:?}");
    assert!((back.width - laid.width).abs() < 1e-4, "its size");

    // From the PC, at once.
    let at = sim.m.hold(thing).expect("taken");
    sim.m.drag(
        thing,
        Placement {
            position: at.position + Vec3::new(0.5, 0.0, 0.0),
            ..at
        },
        Vec3::ZERO,
    );
    sim.m.let_go(sim.now, thing);
    sim.take();
    sim.command(12, Command::ResetArrangement);
    let fx = sim.take();
    assert_eq!(reply_of(&fx, 12), Some(&Outcome::Done));
    assert!(laid_out(&here(&sim)));
}

/// "Locked" (step 8.5), off at first: from the PC (command 16) on, answered done, the
/// monitor a hand carries let go, and kept in the settings; a hand then takes no
/// monitor and the user is told why, the menu is taken still; choosing how a hand
/// moves monitors in the menu unlocks them, and the PC turns it off too.
#[test]
fn locked_from_the_pc_and_the_menu() {
    use crate::interaction::Scene as _;
    let mut sim = Sim::new();
    sim.connect(&[1, 2]);
    assert!(!sim.m.settings().locked, "off at first");
    // Locked while a hand carries a monitor: it is let go.
    assert!(sim.m.hold(Thing::Display(k(2))).is_some());
    sim.take();
    sim.command(16, Command::Lock { on: true });
    let fx = sim.take();
    assert_eq!(reply_of(&fx, 16), Some(&Outcome::Done));
    assert!(sim.m.settings().locked);
    assert!(!sim.m.holds(Thing::Display(k(2))), "let go");
    let mut text = String::new();
    sim.m.write_file(persist::File::Settings, &mut text);
    assert!(text.contains("locked on"), "kept: {text}");
    let thing = Thing::Display(k(1));
    assert_eq!(sim.m.hold(thing), None);
    let said = sim.m.toast().map(|t| t.text.clone());
    assert_eq!(
        said.as_deref(),
        Some(sim.m.texts().t("headset-note-locked"))
    );
    assert!(sim.m.hold(Thing::Menu).is_some(), "the menu is taken");
    sim.m.let_go(sim.now, Thing::Menu);
    sim.menu(Action::MoveAll(false));
    assert!(!sim.m.settings().locked, "unlocked by the menu");
    assert!(sim.m.hold(thing).is_some(), "taken again");
    sim.m.let_go(sim.now, thing);
    sim.command(17, Command::Lock { on: true });
    sim.command(18, Command::Lock { on: false });
    let fx = sim.take();
    assert_eq!(reply_of(&fx, 18), Some(&Outcome::Done));
    assert!(!sim.m.settings().locked);
}

/// Scenario 9: a hundred changes in a pass are one save of each file changed, said at
/// the end of the pass, and nothing when nothing changed.
#[test]
fn a_hundred_changes_are_one_save_each() {
    let mut sim = Sim::new();
    sim.connect(&[1, 2]);
    for i in 0..100u8 {
        sim.menu(Action::Brightness(i));
        sim.menu(Action::Move {
            id: k(1),
            delta: if i % 2 == 0 { 1 } else { -1 },
        });
    }
    sim.menu(Action::Tab(Tab::Monitors));
    let fx = sim.take();
    assert!(
        !fx.iter().any(|e| matches!(e, Effect::Save(_))),
        "only at the end"
    );
    let mut saves = sim.saves();
    saves.sort_by_key(|f| f.name());
    assert_eq!(saves, [persist::File::Desk, persist::File::Settings]);
    assert!(sim.saves().is_empty());
    let mut text = String::new();
    sim.m.write_file(persist::File::Settings, &mut text);
    assert!(text.contains("brightness 99"), "{text}");
}

/// Scenario 4 with the hands: the system menu over the app in the middle of a drag
/// lets the monitor go where it is, the arrangement is saved exactly once, and the hand
/// moves nothing after.
#[test]
fn the_system_menu_ends_a_drag_with_one_save() {
    let mut sim = Sim::new();
    sim.connect(&[1, 2]);
    // The first run's tips opened the menu, in front of the monitors.
    sim.menu(Action::Close);
    sim.saves();
    let mut hands = Hands::new();
    let at = sim.m.world().shown(k(1)).expect("drawn").position;
    let ray = Some(Ray {
        origin: Vec3::new(0.0, -0.3, 0.0),
        dir: (at - Vec3::new(0.0, -0.3, 0.0)).normalize(),
    });
    let mut right = HandState {
        ray,
        ..HandState::default()
    };
    let step = |sim: &mut Sim, hands: &mut Hands, right: &mut HandState, origin: Vec3| {
        sim.now += 11 * MS;
        right.grab.update(1.0);
        right.ray = right.ray.map(|r| Ray { origin, ..r });
        let input = frame(*right);
        sim.m.tick(sim.now, &input);
        hands.buttons(sim.now, &input, &mut sim.m);
        hands.step(sim.now, &input, &mut sim.m, &mut Pictures(None));
        sim.m.animate(sim.now, &input);
        sim.check_view();
    };
    step(&mut sim, &mut hands, &mut right, Vec3::new(0.0, -0.3, 0.0));
    assert!(sim.m.world().holds(Thing::Display(k(1))));
    assert!(sim.take().contains(&Effect::Haptic(1)));
    step(&mut sim, &mut hands, &mut right, Vec3::new(0.2, -0.3, 0.0));
    let moved = sim.m.world().placement(k(1)).copied();
    sim.saves();
    // The shell lets go (on the system menu, the recenter, other controllers) while
    // the hands are busy.
    assert!(hands.busy());
    // The system menu.
    hands.cancel_all(&mut sim.m);
    assert!(!hands.busy());
    assert!(!sim.m.world().holding());
    assert_eq!(sim.saves(), [persist::File::Desk]);
    // Once more, with nothing held (the system's recenter, another profile of
    // controllers): nothing to save.
    hands.cancel_all(&mut sim.m);
    assert_eq!(sim.saves(), []);
    step(&mut sim, &mut hands, &mut right, Vec3::new(0.5, -0.3, 0.0));
    assert_eq!(
        sim.m.world().placement(k(1)).copied(),
        moved,
        "not moved after"
    );
    assert!(!sim.saves().contains(&persist::File::Desk));
}

/// A click on the menu goes through the menu's picture: the hand says where, the
/// picture says which action, and the model does it, as a press of the menu.
#[test]
fn a_click_on_the_menu_is_the_pictures_action() {
    let mut sim = Sim::new();
    sim.frames(11 * MS);
    sim.menu(Action::OpenMenu);
    let menu = sim.m.world().menu().position;
    let origin = Vec3::new(0.1, -0.2, 0.0);
    let mut right = HandState {
        ray: Some(Ray {
            origin,
            dir: (menu - origin).normalize(),
        }),
        ..HandState::default()
    };
    let mut hands = Hands::new();
    let mut menus = Pictures(Some(Action::Tab(Tab::Monitors)));
    for value in [0.0, 1.0] {
        sim.now += 11 * MS;
        right.select.update(value);
        let input = frame(right);
        sim.m.tick(sim.now, &input);
        hands.buttons(sim.now, &input, &mut sim.m);
        hands.step(sim.now, &input, &mut sim.m, &mut menus);
        sim.check_view();
    }
    assert_eq!(sim.m.menu().tab, Tab::Monitors);
    assert!(sim.take().contains(&Effect::Haptic(1)));
    assert_eq!(sim.saves(), [persist::File::Settings]);
}

/// The keyboard seen while a hand moves the hand-placed window: the hand lets go of it,
/// and the window goes over the keyboard.
#[test]
fn a_tracked_keyboard_cuts_a_drag_short() {
    use crate::interaction::Scene as _;
    let mut caps = full_caps();
    caps.keyboard_tracking = true;
    let mut sim = Sim::of(caps);
    sim.menu(Action::KeyboardWindow(true));
    sim.frames_seeing(50 * MS, None);
    let at = sim.m.hold(Thing::Keyboard).expect("taken");
    sim.m.drag(Thing::Keyboard, at, Vec3::ZERO);
    assert!(sim.m.world().holds(Thing::Keyboard));
    sim.frames_seeing(50 * MS, Some(keyboard_on_desk()));
    assert!(!sim.m.world().holds(Thing::Keyboard), "let go");
    assert!(sim.m.keyboard_follows());
}

/// A stream that ends, and a display gone from the PC's list, drop their streams; a
/// stream that starts again for the same display is the shell's to keep.
#[test]
fn streams_gone_are_dropped() {
    let mut sim = Sim::new();
    sim.connect(&[1, 2]);
    sim.net(NetEvent::StreamEnd {
        pc: PC,
        id: DisplayId(2),
    });
    assert_eq!(sim.take(), [Effect::Video(VideoCmd::Drop(k(2)))]);
    assert!(!sim.m.world().streams(k(2)));
    sim.list(&[2]);
    let fx = sim.take();
    assert!(fx.contains(&Effect::Video(VideoCmd::Drop(k(1)))), "{fx:?}");
}

/// A session that ends drops every stream of its PC, also one whose end the network
/// never said: no monitor keeps a decoder of a connection that is gone.
#[test]
fn a_session_that_ends_drops_its_streams() {
    let mut sim = Sim::new();
    sim.connect(&[1, 2]);
    sim.net(NetEvent::StreamEnd {
        pc: PC,
        id: DisplayId(1),
    });
    sim.take();
    sim.net(NetEvent::Ended {
        step: StepId(1),
        pc: Some(PC),
        why: Disconnect::Closed,
        ended: Ended {
            opened: true,
            ..Ended::default()
        },
        lasted: Duration::from_secs(60),
    });
    let fx = sim.take();
    assert!(fx.contains(&Effect::Video(VideoCmd::Drop(k(2)))), "{fx:?}");
    assert!(!sim.m.world().streams(k(2)));
}

/// Paired again inside the session's opening (the PC had forgotten the headset): the
/// note says it, as for a PC paired with before its session.
#[test]
fn pairing_again_inside_the_session_is_said() {
    let mut sim = Sim::new();
    sim.after(MS);
    let mut step = None;
    for _ in 0..2 {
        for e in sim.take() {
            match e {
                Effect::Net(NetCmd::Ask { step: s, .. }) => sim.net(NetEvent::Missed { step: s }),
                Effect::Net(NetCmd::Dial { step: s, .. }) => step = Some(s),
                _ => {}
            }
        }
    }
    let step = step.expect("a dial");
    let key = [3; 32];
    let pc = PcId::of(&key);
    sim.net(NetEvent::Dialed { step, addr: addr() });
    sim.net(NetEvent::Reached {
        step,
        pc,
        addr: addr(),
    });
    sim.net(NetEvent::Pairing {
        step,
        key,
        addr: addr(),
        code: Some("123 456".into()),
    });
    let opened_with = sim.m.headset_state();
    sim.handle(Event::Connected {
        pc,
        addr: addr(),
        language: None,
        opened_with,
    });
    let said = sim.m.toast().map(|t| t.text.clone());
    assert_eq!(said.as_deref(), Some("Paired with 192.168.1.20"));
}

/// A message to a PC the shell has no session with comes back as failed: the menu
/// says it, without a session there is nothing to send to.
#[test]
fn a_message_not_sent_is_said() {
    let mut sim = Sim::new();
    sim.handle(Event::SendFailed { pc: PC, why: None });
    let shows = crate::view::build(&sim.m).error;
    assert_eq!(
        shows.as_deref(),
        Some(sim.m.texts().t("headset-error-not-connected"))
    );
    // Shown for a while, then gone.
    sim.after(ERROR_SHOWN_FOR);
    assert_eq!(crate::view::build(&sim.m).error, None);
}

/// The microphone: the switch goes to the microphone and the PC hears it; what the
/// system allows, once it changes, too.
#[test]
fn the_microphone_switch_and_permission() {
    let mut sim = Sim::new();
    sim.connect(&[1]);
    sim.menu(Action::SetMic(true));
    let fx = sim.take();
    assert!(fx.contains(&Effect::Mic(MicCmd::Allowed(true))));
    assert!(
        sent(&fx)
            .iter()
            .any(|m| matches!(m, ClientMsg::UpState { part } if part.switch))
    );
    sim.handle(Event::Platform(PlatformEvent::Mic {
        permission: permission::GRANTED,
        denied: false,
    }));
    let fx = sim.take();
    assert!(sent(&fx).iter().any(
        |m| matches!(m, ClientMsg::UpState { part } if part.permission == permission::GRANTED)
    ));
    // The same again says nothing.
    sim.handle(Event::Platform(PlatformEvent::Mic {
        permission: permission::GRANTED,
        denied: false,
    }));
    assert!(sent(&sim.take()).is_empty());
}

/// The store says the app is not the person's: the menu opens with its message, and
/// the app closes a few seconds after it showed.
#[test]
fn not_owned_closes_after_saying_so() {
    let mut sim = Sim::new();
    sim.frames(11 * MS);
    let not_owned = || Event::Platform(PlatformEvent::NotOwned { store: "A store" });
    sim.handle(not_owned());
    assert!(sim.m.menu().open);
    let fx = sim.take();
    assert!(!fx.contains(&Effect::Exit));
    assert!(
        crate::view::build(&sim.m)
            .error
            .is_some_and(|e| e.contains("A store"))
    );
    sim.now += NOT_OWNED_FOR;
    sim.handle(not_owned());
    assert!(!sim.take().contains(&Effect::Exit));
    sim.now += MS;
    sim.handle(not_owned());
    assert!(sim.take().contains(&Effect::Exit));
}

/// The time goes on without frames: a request the PC never answers is given up after
/// its time, and the button that asked says so; the link's end lets the next go.
#[test]
fn a_request_times_out_without_frames() {
    let mut sim = Sim::new();
    sim.connect(&[1]);
    sim.menu(Action::FindMouse);
    let fx = sim.take();
    assert!(sent(&fx).iter().any(|m| matches!(
        m,
        ClientMsg::Request {
            asked: Asked::Known(Request::FindMouse),
            ..
        }
    )));
    assert!(sim.m.session().waits(ui::Asked::FindMouse));
    sim.after(pending::ANSWER_WITHIN + MS);
    assert!(!sim.m.session().waits(ui::Asked::FindMouse));
    assert!(sim.m.menu().notes.shown().is_some() || sim.m.toast().is_some());
    // The PC goes: its outbox goes with it, and messages to it are not sent.
    sim.net(NetEvent::Ended {
        step: StepId(1),
        pc: Some(PC),
        why: Disconnect::Silence,
        ended: Ended::default(),
        lasted: Duration::from_secs(60),
    });
    assert!(!sim.m.session().connected(PC));
}

/// What is drawn and what the menu shows go on with the frames: a toast is placed in
/// front once a frame comes and goes after its time; nothing in a quiet frame makes
/// the menu's state anew.
#[test]
fn quiet_frames_change_nothing_the_menu_shows() {
    let mut sim = Sim::new();
    sim.connect(&[1, 2]);
    sim.net(NetEvent::Notice(PC, "Hello".into()));
    assert!(sim.m.toast().is_some_and(|t| t.place.is_none()));
    sim.frames(11 * MS);
    let place = sim.m.toast().and_then(|t| t.place).expect("placed");
    assert!(place.position.z < 0.0, "in front: {place:?}");
    sim.frames(TOAST_FOR);
    assert!(sim.m.toast().is_none());
    // Quiet now: the frames say nothing new to the menu.
    sim.frames(Duration::from_secs(2));
    let rev = sim.m.rev();
    sim.frames(Duration::from_secs(1));
    assert_eq!(sim.m.rev(), rev);
}

/// What the headset says of itself (its settings for the PC, the microphone, the
/// battery) is quiet: a message of it that cannot be sent is let go, and the menu says
/// nothing. Only the menu's requests are said when they are not sent.
#[test]
fn the_headsets_own_news_is_quiet() {
    let mut sim = Sim::new();
    sim.connect(&[1]);
    sim.menu(Action::Focus(true));
    sim.menu(Action::SetMic(true));
    sim.handle(Event::Platform(PlatformEvent::Battery(Some((50, true)))));
    let fx = sim.take();
    let quiet = |m: &ClientMsg| {
        matches!(
            m,
            ClientMsg::Prefs { .. } | ClientMsg::UpState { .. } | ClientMsg::Battery { .. }
        )
    };
    assert!(
        fx.iter()
            .any(|e| matches!(e, Effect::Tell(_, m) if quiet(m)))
    );
    assert!(
        !fx.iter()
            .any(|e| matches!(e, Effect::Send(_, m) if quiet(m))),
        "{fx:?}"
    );
    sim.menu(Action::FindMouse);
    let fx = sim.take();
    assert!(
        fx.iter()
            .any(|e| matches!(e, Effect::Send(_, ClientMsg::Request { .. })))
    );
}

/// Following the head: the arrangement glides after the turned head, the menu is
/// told of each step, and of the last one too (the one that stops and keeps it).
#[test]
fn following_tells_the_menu_to_its_last_step() {
    let mut sim = Sim::new();
    sim.connect(&[1, 2]);
    sim.menu(Action::Follow(true));
    let turned = Head {
        position: Vec3::ZERO,
        forward: Vec3::new(-1.0, 0.0, -1.0).normalize(),
        right: Vec3::new(1.0, 0.0, -1.0).normalize(),
    };
    let mut followed = false;
    for _ in 0..300 {
        sim.now += 11 * MS;
        let input = FrameInput {
            head: Some(turned),
            ..frame(HandState::default())
        };
        let (was, rev) = (sim.m.world().following(), sim.m.rev());
        sim.m.tick(sim.now, &input);
        sim.m.animate(sim.now, &input);
        sim.check_view();
        followed |= was;
        if was {
            assert_ne!(sim.m.rev(), rev, "a step of following, the last one too");
        }
        if followed && !sim.m.world().following() {
            break;
        }
    }
    assert!(
        followed && !sim.m.world().following(),
        "it followed and stopped"
    );
    assert!(sim.saves().contains(&persist::File::Desk));
}

/// A drag with the menu open: the menu says on which side the monitor stands as soon
/// as that changes, and nothing else while the hand moves it (its map shows the PC's
/// desktop as it was until the hand lets go, then the new one).
#[test]
fn a_drag_tells_the_menu_only_what_it_shows() {
    let mut sim = Sim::new();
    sim.connect(&[1, 2]);
    sim.menu(Action::Close);
    let at = sim.m.world().shown(k(1)).expect("drawn").position;
    let origin = Vec3::new(0.0, -0.3, 0.0);
    let mut right = HandState {
        ray: Some(Ray {
            origin,
            dir: (at - origin).normalize(),
        }),
        ..HandState::default()
    };
    let mut hands = Hands::new();
    let mut step = |sim: &mut Sim, hands: &mut Hands, grab: f32, dir: Vec3| -> u64 {
        sim.now += 11 * MS;
        right.grab.update(grab);
        right.ray = Some(Ray { origin, dir });
        let input = frame(right);
        let rev = sim.m.rev();
        sim.m.tick(sim.now, &input);
        hands.buttons(sim.now, &input, &mut sim.m);
        hands.step(sim.now, &input, &mut sim.m, &mut Pictures(None));
        sim.m.animate(sim.now, &input);
        sim.check_view();
        sim.m.rev() - rev
    };
    let straight = (at - origin).normalize();
    step(&mut sim, &mut hands, 1.0, straight);
    assert!(sim.m.world().holds(Thing::Display(k(1))));
    sim.menu(Action::OpenMenu);
    let place = |sim: &Sim| {
        crate::view::build(&sim.m)
            .monitors
            .iter()
            .find(|m| m.id == k(1))
            .map(|m| m.place)
    };
    let before = place(&sim);
    // A little to and fro: the menu shows nothing new.
    for i in 0..20 {
        let wobble = Vec3::new((i as f32 * 0.3).sin() * 0.01, 0.0, 0.0);
        assert_eq!(
            step(&mut sim, &mut hands, 1.0, (straight + wobble).normalize()),
            0
        );
    }
    // Far to the left: it says so.
    let left = Vec3::new(-1.0, 0.1, -0.4).normalize();
    let mut told = 0;
    for _ in 0..30 {
        told += step(&mut sim, &mut hands, 1.0, left);
    }
    assert!(told > 0);
    assert_ne!(place(&sim), before);
    // Let go: the map shows the desktop anew.
    assert!(step(&mut sim, &mut hands, 0.0, left) > 0);
    assert!(!sim.m.world().holding());
}

/// The connection screen follows the time alone: with no trusted PC and no answer, the
/// hint for a new PC after 5 s; a PC that answered leaves the list 6 s after its answer;
/// with a trusted PC silent for 30 s, the hint that it does not answer. Each time the
/// model says so (the view check of every step guards it).
#[test]
fn the_search_screen_follows_the_time() {
    use crate::session::{FoundHost, Hint, PairedPc};
    let mut sim = Sim::with(None);
    let miss_all = |sim: &mut Sim| {
        for e in sim.take() {
            if let Effect::Net(NetCmd::Ask { step, .. } | NetCmd::Dial { step, .. }) = e {
                sim.net(NetEvent::Missed { step });
            }
        }
    };
    let hint = |sim: &Sim| crate::view::build(&sim.m).hint;
    for _ in 0..48 {
        sim.after(100 * MS);
        miss_all(&mut sim);
    }
    assert_eq!(hint(&sim), None);
    for _ in 0..4 {
        sim.after(100 * MS);
        miss_all(&mut sim);
    }
    assert_eq!(hint(&sim), Some(Hint::NewPc));
    // A PC answers everyone: listed, the hint gone; 6 s later it is gone again.
    sim.net(NetEvent::Heard(FoundHost {
        name: "Desk".into(),
        addr: addr(),
        pc: None,
        pairing: true,
        heard: sim.now,
        at_last: false,
        label: String::new(),
        ip: String::new(),
        usb: false,
    }));
    let view = crate::view::build(&sim.m);
    assert_eq!(view.hint, None);
    assert_eq!(
        view.found,
        [("Desk · pairing open".to_owned(), "192.168.1.20".to_owned())]
    );
    miss_all(&mut sim);
    for _ in 0..61 {
        sim.after(100 * MS);
        miss_all(&mut sim);
    }
    let view = crate::view::build(&sim.m);
    assert!(view.found.is_empty(), "6 s after its answer");
    assert_eq!(view.hint, Some(Hint::NewPc));
    // A trusted PC that does not answer: after 30 s, the hint says so.
    let mut sim = Sim::with(None);
    sim.net(NetEvent::Paired(vec![PairedPc {
        pc: PC,
        name: "Desk".into(),
    }]));
    for _ in 0..290 {
        sim.after(100 * MS);
        miss_all(&mut sim);
    }
    assert_eq!(hint(&sim), None);
    for _ in 0..20 {
        sim.after(100 * MS);
        miss_all(&mut sim);
    }
    assert_eq!(hint(&sim), Some(Hint::Silent));
}

/// A display of the PC itself: `connector`, the laptop's panel or not, shown or not,
/// where it is on the desktop.
fn physical(n: u32, connector: &str, builtin: bool, shown: bool, desk: [i32; 4]) -> Display {
    Display {
        id: DisplayId(n),
        kind: DisplayKind::Physical {
            builtin,
            connector: connector.into(),
            shown,
        },
        name: connector.into(),
        mode: Mode::new(1920, 1080),
        desk: Some(state::DeskRect {
            x: desk[0],
            y: desk[1],
            w: desk[2].unsigned_abs(),
            h: desk[3].unsigned_abs(),
        }),
        state: DisplayState::Running,
    }
}

impl Sim {
    /// The PC lists monitor 1 and `physical`, its primary monitor 1.
    fn list_with(&mut self, physical: Vec<Display>) {
        let mut displays = vec![monitor(1)];
        displays.extend(physical);
        let part = Displays {
            displays,
            primary: Some(DisplayId(1)),
        };
        self.net(NetEvent::Part(PC, HostMsg::Displays { part }));
    }

    /// The PC's limits, with `physical_max` of its own displays and three scales; it
    /// makes monitors at another scale where `scale` (its capability 10).
    fn limits(&mut self, physical_max: u8, scale: bool) {
        let part = state::Limits {
            virtual_max: 6,
            windows_max: 3,
            physical_max,
            streams_max: 16,
            min: (640, 480),
            max: (5120, 2880),
            sizes: vec![Mode::new(2560, 1440)],
            scales: vec![100, 125, 150],
        };
        self.net(NetEvent::Part(PC, HostMsg::Limits { part }));
        let features = if scale {
            vec![caps::pc::SCALE]
        } else {
            Vec::new()
        };
        let part = state::HostCaps { features };
        self.net(NetEvent::Part(PC, HostMsg::HostCaps { part }));
    }

    /// The PC answers request `req` with `outcome`.
    fn reply(&mut self, req: ReqId, outcome: Outcome) {
        self.net(NetEvent::Reply {
            pc: PC,
            req,
            outcome,
        });
    }
}

/// The requests sent to the PC among `fx`, with their numbers.
fn requests(fx: &[Effect]) -> Vec<(ReqId, Request)> {
    sent(fx)
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Request {
                req,
                asked: Asked::Known(r),
            } => Some((*req, r.clone())),
            _ => None,
        })
        .collect()
}

/// Each display of the PC itself has its switch, named as the PC names it: pressed, it
/// moves at once and asks the PC; a refusal (the PC's limit) puts it back and says why
/// under it; the PC's list shows it as it is once answered.
#[test]
fn each_display_of_the_pc_has_its_switch() {
    let mut sim = Sim::new();
    sim.connect(&[1]);
    sim.limits(1, false);
    let laptop = physical(100, "eDP-1", true, true, [0, 1080, 1920, 1080]);
    let hdmi = physical(117, "HDMI-A-1", false, false, [0, 0, 1920, 1080]);
    sim.list_with(vec![laptop, hdmi.clone()]);
    let shown = |sim: &Sim| -> Vec<(u32, bool)> {
        crate::view::build(&sim.m)
            .physical
            .iter()
            .map(|d| (d.id.id.0, d.shown))
            .collect()
    };
    assert_eq!(shown(&sim), [(100, true), (117, false)]);
    sim.take();
    sim.menu(Action::ShowPhysical(k(117), true));
    let asked = requests(&sim.take());
    let [(req, Request::ShowPhysical { id, shown: true })] = asked.as_slice() else {
        panic!("{asked:?}")
    };
    assert_eq!(*id, DisplayId(117));
    assert_eq!(shown(&sim), [(100, true), (117, true)], "at once");
    sim.reply(*req, Outcome::failed(ErrorCode::LIMIT, "1"));
    assert_eq!(shown(&sim), [(100, true), (117, false)], "put back");
    let note = crate::view::build(&sim.m).note.expect("why");
    assert_eq!(note.on, Some(Action::ShowPhysical(k(117), true)));
    assert_eq!(note.kind, note::Kind::Failed);

    // Hidden the laptop's panel, shown the monitor: the PC's list, then its answer.
    sim.menu(Action::ShowPhysical(k(100), false));
    let [(req, _)] = requests(&sim.take())[..] else {
        panic!("one request")
    };
    let hidden = physical(100, "eDP-1", true, false, [0, 1080, 1920, 1080]);
    sim.list_with(vec![hidden, hdmi]);
    sim.reply(req, Outcome::Done);
    assert_eq!(shown(&sim), [(100, false), (117, false)]);
    let note = crate::view::build(&sim.m).note.expect("done");
    assert_eq!(&*note.text, "The PC display is hidden");
}

/// Scenario 15 (step 7 note, 10.1): a stream started again in a new generation for the
/// same display counts from then on, and what comes of the old one is let go; a
/// lasting failure keeps the monitor where it stands, shows it in the menu with its
/// reason, is said once and asks nothing again, until the next generation.
#[test]
fn streams_by_generation_and_a_failure_said_once() {
    use crate::video::{Happened, VideoEvent, Why};
    let mut sim = Sim::new();
    sim.connect(&[1, 2]);
    let video = |generation, what| {
        Event::Video(VideoEvent {
            id: k(1),
            generation: Gen(generation),
            what,
        })
    };
    let picture = |sim: &Sim, n| {
        crate::view::build(&sim.m)
            .monitors
            .into_iter()
            .find(|m| m.id == k(n))
            .map(|m| m.picture)
    };
    assert_eq!(picture(&sim, 1), Some(ui::Picture::Shows));
    sim.handle(video(1, Happened::Trouble));
    assert_eq!(picture(&sim, 1), Some(ui::Picture::Recovering));
    assert_eq!(picture(&sim, 2), Some(ui::Picture::Shows), "only its own");
    // The PC starts the stream again: the new generation shows, the old one's news
    // is let go.
    sim.stream(1, Gen(2), (7680, 2160));
    assert_eq!(picture(&sim, 1), Some(ui::Picture::Shows));
    let rev = sim.m.rev();
    sim.handle(video(1, Happened::Failed(Why::BadSize)));
    assert!(
        sim.m.world().streams(k(1)),
        "an old generation's failure is let go"
    );
    assert_eq!(picture(&sim, 1), Some(ui::Picture::Shows));
    assert!(sim.m.toast().is_none());
    assert!(sim.m.rev() > rev);
    sim.take();
    // The new one fails for good: where it stood, in the menu with why, said once.
    sim.handle(video(2, Happened::Failed(Why::BadSize)));
    assert!(sim.m.world().streams(k(1)), "it keeps its place");
    let Some(ui::Picture::NotShown(why)) = picture(&sim, 1) else {
        panic!("the menu shows it with its reason");
    };
    assert!(
        why.contains("7680 × 2160") && why.contains("H.264"),
        "{why}"
    );
    let toast = sim.m.toast().expect("said").n;
    assert!(sim.m.toast().unwrap().text.contains("7680 × 2160"));
    // Nothing is asked again: no retry, no message to the PC, no video command.
    let fx = sim.take();
    assert!(
        !fx.iter()
            .any(|e| matches!(e, Effect::Video(_) | Effect::Send(..))),
        "{fx:?}"
    );
    sim.handle(video(2, Happened::Failed(Why::BadSize)));
    sim.handle(video(2, Happened::Trouble));
    assert_eq!(sim.m.toast().unwrap().n, toast, "said once");
    let Some(ui::Picture::NotShown(_)) = picture(&sim, 1) else {
        panic!("the failure stays until the next generation");
    };
    // It can be picked in the menu, and taken away there.
    sim.menu(Action::SelectMonitor(k(1)));
    assert_eq!(crate::view::build(&sim.m).selected, Some(k(1)));
    // The next generation shows again, where it stood.
    sim.stream(1, Gen(3), (2560, 1440));
    assert!(sim.m.world().streams(k(1)));
    assert_eq!(picture(&sim, 1), Some(ui::Picture::Shows));
    let shown = crate::view::build(&sim.m);
    assert_eq!(shown.monitors.iter().filter(|m| m.id == k(1)).count(), 1);
    // A failed stream of a PC whose session ended is gone from the menu.
    sim.handle(video(3, Happened::Failed(Why::NoDecoder)));
    sim.net(NetEvent::StreamEnd {
        pc: PC,
        id: DisplayId(1),
    });
    assert_eq!(sim.m.videos().failed().count(), 0);
}

/// A monitor whose video fails for good keeps its place in the rows the PC hears: the
/// PC's desktop does not move the others into its place.
#[test]
fn a_failed_monitor_keeps_its_place_on_the_pcs_desktop() {
    use crate::video::{Happened, VideoEvent, Why};
    let mut sim = Sim::new();
    sim.connect(&[1, 2, 3]);
    sim.handle(Event::Video(VideoEvent {
        id: k(2),
        generation: Gen(1),
        what: Happened::Failed(Why::NoDecoder),
    }));
    // Nothing to tell for it: the PC's desktop stays as it is.
    sim.frames(Duration::from_secs(3));
    let fx = sim.take();
    assert!(
        !sent(&fx)
            .iter()
            .any(|m| matches!(m, ClientMsg::Arrange { .. })),
        "{:?}",
        sent(&fx)
    );
    // The arrangement told again, changed by hand: with the failed one in it.
    sim.menu(Action::Move { id: k(1), delta: 1 });
    sim.frames(Duration::from_secs(3));
    let fx = sim.take();
    let rows = sent(&fx)
        .into_iter()
        .rev()
        .find_map(|m| match m {
            ClientMsg::Arrange { rows } => Some(rows.clone()),
            _ => None,
        })
        .expect("the arrangement told");
    let mut ids: Vec<u32> = rows.iter().flat_map(Row::ids).map(|id| id.0).collect();
    ids.sort_unstable();
    assert_eq!(ids, [1, 2, 3], "{rows:?}");
}

/// The PC's own displays shown stand under the primary monitor as one group, as they
/// stand to each other on the PC's desktop: the monitor above the laptop's panel,
/// side by side ones side by side.
#[test]
fn the_pcs_displays_stand_as_on_its_desktop() {
    let mut sim = Sim::new();
    sim.connect(&[1]);
    // The laptop's panel left, a monitor right of it and higher.
    let laptop = physical(100, "eDP-1", true, true, [0, 400, 1920, 1080]);
    let right = physical(117, "HDMI-A-1", false, true, [1920, 0, 2560, 1440]);
    sim.list_with(vec![laptop, right]);
    for (n, size) in [(100, (1920, 1080)), (117, (2560, 1440))] {
        sim.stream(n, Gen(1), size);
    }
    sim.frames(Duration::from_secs(1));
    let world = sim.m.world();
    let at = |n| *world.placement(k(n)).expect("placed");
    let (monitor, laptop, right) = (at(1), at(100), at(117));
    // Both under the monitor, the panel on the left.
    assert!(laptop.position.y < monitor.position.y && right.position.y < monitor.position.y);
    let side = monitor.rotation * Vec3::X;
    assert!(
        (right.position - laptop.position).dot(side) > 0.0,
        "left to right"
    );
    // As on the desktop: the monitor's top higher, sizes in proportion.
    assert!(right.position.y > laptop.position.y);
    assert!((right.width / laptop.width - 2560.0 / 1920.0).abs() < 0.01);
    // Next to each other, a hair apart: the panel's right edge at the monitor's left.
    let edge =
        |p: &Placement, s: f32| p.position + p.rotation * Vec3::new(s * p.width / 2.0, 0.0, 0.0);
    let gap = (edge(&right, -1.0) - edge(&laptop, 1.0)).dot(side);
    assert!(gap > 0.0 && gap < 0.01, "a hair apart: {gap}");
}

/// A monitor added gets the scale chosen, where the PC makes monitors at its scales;
/// another scale for a monitor is asked of the PC and leaves the profile in use.
#[test]
fn scales_from_the_menu() {
    let mut sim = Sim::new();
    sim.connect(&[1]);
    sim.limits(4, false);
    sim.menu(Action::AddScale(150));
    sim.menu(Action::Add {
        width: 2560,
        height: 1440,
    });
    let asked = requests(&sim.take());
    assert!(
        matches!(&asked[..], [(_, Request::AddDisplay { mode })] if mode.scale == 100),
        "a PC without the capability: {asked:?}"
    );
    sim.limits(4, true);
    sim.menu(Action::Add {
        width: 2560,
        height: 1440,
    });
    let asked = requests(&sim.take());
    assert!(
        matches!(&asked[..], [(_, Request::AddDisplay { mode })] if mode.scale == 150),
        "{asked:?}"
    );
    assert!(crate::view::build(&sim.m).can_scale);
    sim.m.menu.active_profile = Some(0);
    sim.menu(Action::SetScale(k(1), 125));
    let asked = requests(&sim.take());
    assert!(
        matches!(
            &asked[..],
            [(_, Request::SetScale { id, scale: 125 })] if *id == DisplayId(1)
        ),
        "{asked:?}"
    );
    assert_eq!(sim.m.menu().active_profile, None);
}

/// "No limits" from the menu: the PC hears at once, the setting is kept, and the
/// button says what it does.
#[test]
fn no_limits_from_the_menu() {
    let mut sim = Sim::new();
    sim.connect(&[1]);
    sim.menu(Action::NoLimits(true));
    let fx = sim.take();
    assert!(
        sent(&fx)
            .iter()
            .any(|m| matches!(m, ClientMsg::Prefs { part } if part.no_limits))
    );
    assert!(sim.m.settings().no_limits);
    assert_eq!(sim.saves(), [persist::File::Settings]);
    let note = crate::view::build(&sim.m).note.expect("said");
    assert_eq!(&*note.text, "More monitors and displays allowed");
}

/// Profiles as a list: a new one saved from the setup, one saved over (asked twice,
/// keeping its name), one deleted (asked twice); the PC hears the list and the one in
/// use at the end of each pass, a name given from the PC before its answer; a command
/// finds a profile by its name.
#[test]
fn profiles_are_a_list_the_pc_hears_of() {
    let mut sim = Sim::new();
    sim.connect(&[1, 2]);
    let told = |fx: &[Effect]| -> Vec<state::Profiles> {
        sent(fx)
            .into_iter()
            .filter_map(|m| match m {
                ClientMsg::Profiles { part } => Some(part.clone()),
                _ => None,
            })
            .collect()
    };
    let named = |list: &[&str], active: Option<u8>| state::Profiles {
        list: list
            .iter()
            .map(|n| state::ProfileName { name: (*n).into() })
            .collect(),
        active,
    };
    sim.menu(Action::NewProfile);
    sim.menu(Action::NewProfile);
    assert_eq!(sim.m.profiles().len(), 2);
    assert_eq!(sim.saves(), [persist::File::Profiles]);
    let fx = sim.take();
    assert!(told(&fx).is_empty(), "taken with the saves");
    sim.menu(Action::NewProfile);
    sim.m.end_pass();
    assert_eq!(told(&sim.take()), [named(&["", "", ""], Some(2))]);
    let view = crate::view::build(&sim.m);
    assert_eq!(view.profiles, ["Profile 1", "Profile 2", "Profile 3"]);
    let note = view.note.expect("said");
    assert_eq!(&*note.text, "Saved to \"Profile 3\"");

    // A name from the PC: the list first, then the answer.
    sim.command(
        20,
        Command::RenameProfile {
            profile: "1".into(),
            name: " Work\n".into(),
        },
    );
    let fx = sim.take();
    let msgs = sent(&fx);
    let list = msgs
        .iter()
        .position(|m| matches!(m, ClientMsg::Profiles { .. }));
    let reply = msgs
        .iter()
        .position(|m| matches!(m, ClientMsg::Reply { req, .. } if req.0 == 20));
    assert!(list.is_some() && list < reply, "{msgs:?}");
    assert_eq!(told(&fx), [named(&["Work", "", ""], Some(2))]);
    assert_eq!(crate::view::build(&sim.m).profiles[0], "Work");
    assert_eq!(sim.saves(), [persist::File::Profiles]);

    // Saved over: asked first, its name kept.
    sim.menu(Action::SaveProfile(0));
    assert_eq!(sim.saves(), []);
    sim.menu(Action::SaveProfile(0));
    assert_eq!(sim.saves(), [persist::File::Profiles]);
    assert_eq!(sim.m.profiles()[0].name.as_deref(), Some("Work"));
    assert_eq!(sim.m.menu().active_profile, Some(0));

    // Deleted: asked first; the one in use after it keeps being it.
    sim.menu(Action::ApplyProfile(2));
    sim.take();
    sim.m.menu.active_profile = Some(2);
    sim.menu(Action::DeleteProfile(1));
    assert_eq!(sim.m.profiles().len(), 3);
    sim.menu(Action::DeleteProfile(1));
    assert_eq!(sim.m.profiles().len(), 2);
    assert_eq!(sim.m.menu().active_profile, Some(1));
    sim.m.end_pass();
    assert_eq!(told(&sim.take()), [named(&["Work", ""], Some(1))]);

    // By its name, from the PC: its monitors asked of the PC.
    sim.command(
        21,
        Command::Profile {
            name: "Work".into(),
        },
    );
    let asked = requests(&sim.take());
    assert!(
        matches!(&asked[..], [(_, Request::SetDisplays { modes, .. })] if modes.len() == 2),
        "{asked:?}"
    );
    sim.command(
        22,
        Command::RenameProfile {
            profile: "Games".into(),
            name: "x".into(),
        },
    );
    assert!(matches!(
        reply_of(&sim.take(), 22),
        Some(Outcome::Failed { error, .. }) if *error == ErrorCode::NO_SUCH_PROFILE
    ));
    // The next session starts with them.
    let active = sim
        .m
        .menu()
        .active_profile
        .and_then(|a| u8::try_from(a).ok());
    assert_eq!(
        sim.m.headset_state().profiles,
        Some(named(&["Work", ""], active))
    );
}

/// A headset with every optional extension: the cameras, their window onto the
/// keyboard, hands, two display rates.
fn full_caps() -> Caps {
    Caps {
        room: Some(crate::caps::RoomCaps {
            way: crate::caps::RoomWay::FbPassthrough,
            keyboard_window: true,
        }),
        hands: true,
        hand_aim: true,
        menu_gesture: true,
        perf_levels: true,
        volume_steps: Some(15),
        refresh_rates: vec![72.0, 80.0, 90.0, 120.0],
        ..test_caps()
    }
}

/// The room by whichever way the headset shows it: blended with the cameras' picture,
/// the "Room" backgrounds as by a layer (no window onto the keyboard); on glasses, where
/// the room always shows, none to choose.
#[test]
fn the_room_by_any_way_and_none_on_glasses() {
    use crate::caps::{RoomCaps, RoomWay};
    let with = |way| {
        let caps = Caps {
            room: Some(RoomCaps {
                way,
                keyboard_window: false,
            }),
            ..full_caps()
        };
        crate::view::build(&Sim::of(caps).m)
    };
    for way in [
        RoomWay::Blend,
        RoomWay::FbPassthrough,
        RoomWay::HtcPassthrough,
    ] {
        let view = with(way);
        assert!(view.room_available && !view.keyboard_available, "{way:?}");
    }
    let view = with(RoomWay::Glasses);
    assert!(!view.room_available && !view.keyboard_available);
    assert!(view.panorama_available, "the rest stays");
}

/// The menu offers only what the headset can do (the step 7 note, 4.4): everything
/// with every extension, nothing optional with none (`caps=minimal`), and each item
/// with its own field.
#[test]
fn the_menu_shows_what_the_headset_can() {
    let view = crate::view::build(&Sim::of(full_caps()).m);
    assert!(view.room_available && view.keyboard_available && view.hands_available);
    assert!(view.panorama_available && view.curve_available);
    assert_eq!(view.display_rates, Some((90, 72)));
    assert_eq!(view.display_rate_now, Some(90));

    let minimal = Caps::without_extensions(
        "Test headset".into(),
        crate::caps::DECODERS_UNSAID.to_vec(),
        4,
        Some(15),
    );
    let sim = Sim::of(minimal);
    let view = crate::view::build(&sim.m);
    assert!(!view.room_available && !view.keyboard_available && !view.hands_available);
    assert!(!view.panorama_available && !view.curve_available);
    assert_eq!(view.display_rates, None);
    // What the PC hears comes from the same caps, the system's decoders among them.
    let told = sim.m.headset_state().caps.expect("caps in the state");
    assert_eq!(told.streams_max, 4);
    assert_eq!(told.decoders.len(), 2);
    assert_eq!(told.layers_max, crate::caps::LAYERS_AT_LEAST);

    // One rate offered: nothing to choose.
    let view = crate::view::build(
        &Sim::of(Caps {
            refresh_rates: vec![90.0],
            ..full_caps()
        })
        .m,
    );
    assert_eq!(view.display_rates, None);
    // Cameras without their keyboard window.
    let view = crate::view::build(
        &Sim::of(Caps {
            room: Some(crate::caps::RoomCaps {
                way: crate::caps::RoomWay::FbPassthrough,
                keyboard_window: false,
            }),
            ..full_caps()
        })
        .m,
    );
    assert!(view.room_available && !view.keyboard_available);
}

/// Saving the battery asks for the rate the rule picks from those offered, and the
/// usual one back; without rates nothing is asked.
#[test]
fn the_display_rate_by_the_rule() {
    let rates = |sim: &mut Sim| -> Vec<f32> {
        sim.take()
            .into_iter()
            .filter_map(|e| match e {
                Effect::Xr(XrCmd::RefreshRate(hz)) => Some(hz),
                _ => None,
            })
            .collect()
    };
    let mut sim = Sim::of(full_caps());
    assert_eq!(sim.m.wanted_rate(), Some(90.0));
    sim.menu(Action::BatterySaver(true));
    assert_eq!(rates(&mut sim), [72.0]);
    assert_eq!(sim.m.wanted_rate(), Some(72.0));
    sim.menu(Action::BatterySaver(false));
    assert_eq!(rates(&mut sim), [90.0]);
    let mut sim = Sim::of(Caps {
        refresh_rates: Vec::new(),
        ..full_caps()
    });
    sim.menu(Action::BatterySaver(true));
    assert!(rates(&mut sim).is_empty());
    assert_eq!(sim.m.wanted_rate(), None);
}

/// The system changed the display rate: the menu says what it shows now.
#[test]
fn the_rate_the_system_chose_shows() {
    let mut sim = Sim::of(full_caps());
    sim.handle(Event::Platform(PlatformEvent::RefreshRate(120.0)));
    let view = crate::view::build(&sim.m);
    assert_eq!(view.display_rate_now, Some(120));
    assert_eq!(sim.m.headset().refresh_hz, Some(120.0));
}

/// The cameras broke for good: the "Room" backgrounds and the window onto the keyboard
/// go from the menu and the scene, said once; the setting chosen stays for the next
/// start.
#[test]
fn cameras_lost_hide_the_room() {
    let mut sim = Sim::of(full_caps());
    sim.menu(Action::KeyboardWindow(true));
    sim.frames(Duration::from_millis(50));
    assert!(sim.m.keyboard_window().is_some());
    sim.saves();
    sim.handle(Event::Platform(PlatformEvent::CamerasLost));
    let view = crate::view::build(&sim.m);
    assert!(!view.room_available && !view.keyboard_available);
    assert!(view.error.is_some());
    assert!(sim.m.keyboard_window().is_none());
    sim.frames(Duration::from_millis(50));
    assert!(sim.m.keyboard_window().is_none());
    // Kept as chosen, nothing saved.
    assert!(sim.m.settings().keyboard_window);
    assert_eq!(sim.saves(), []);
    // Said once.
    sim.after(ERROR_SHOWN_FOR + MS);
    sim.handle(Event::Platform(PlatformEvent::CamerasLost));
    assert!(crate::view::build(&sim.m).error.is_none());
}

/// The system moved the room's origin: nothing is saved or laid out anew, the monitors
/// keep their places in the room's terms.
#[test]
fn the_origin_moved_keeps_the_arrangement() {
    let mut sim = Sim::new();
    sim.connect(&[1, 2]);
    let before = sim.m.world().placement(k(1)).copied();
    sim.handle(Event::Platform(PlatformEvent::SpaceMoved));
    sim.frames(Duration::from_millis(100));
    assert_eq!(sim.m.world().placement(k(1)).copied(), before);
    assert_eq!(sim.saves(), []);
}

/// Switched to one profile, then to another before the PC answered: the first one's
/// failure does not end the switch to the second.
#[test]
fn a_failed_older_profile_switch_leaves_the_newer_one() {
    let mut sim = Sim::new();
    sim.connect(&[1, 2]);
    sim.menu(Action::NewProfile);
    sim.menu(Action::NewProfile);
    sim.take();
    sim.menu(Action::ApplyProfile(0));
    let first = requests(&sim.take()).first().expect("asked").0;
    sim.menu(Action::ApplyProfile(1));
    let second = requests(&sim.take()).first().expect("asked").0;
    assert!(sim.m.world().switching_with(PC, second));
    sim.net(NetEvent::Reply {
        pc: PC,
        req: first,
        outcome: Outcome::failed(ErrorCode::DESKTOP, "stuck"),
    });
    assert!(
        sim.m.world().switching_with(PC, second),
        "the newer switch goes on"
    );
}

/// A profile in use, then the arrangement changed by hand (a monitor moved, "Line up"):
/// no profile is in use any more, and its button can bring its places back.
/// A press that changes nothing (a monitor taken and let go where it was, "In front")
/// leaves it in use.
#[test]
fn changed_by_hand_the_profile_is_left() {
    use crate::interaction::Scene as _;
    let mut sim = Sim::new();
    sim.connect(&[1, 2]);
    sim.menu(Action::NewProfile);
    assert_eq!(sim.m.menu().active_profile, Some(0));
    // Taken and let go where it was.
    let thing = Thing::Display(k(1));
    let at = sim.m.hold(thing).expect("taken");
    sim.m.let_go(sim.now, thing);
    sim.menu(Action::Recenter);
    assert_eq!(sim.m.menu().active_profile, Some(0), "nothing changed");
    // Moved by hand.
    let at = sim.m.hold(thing).unwrap_or(at);
    let moved = Placement {
        position: at.position + Vec3::new(-1.0, 0.3, 0.0),
        ..at
    };
    sim.m.drag(thing, moved, Vec3::ZERO);
    sim.m.let_go(sim.now, thing);
    assert_eq!(sim.m.menu().active_profile, None, "moved by hand");
    sim.menu(Action::NewProfile);
    sim.menu(Action::Tidy);
    assert_eq!(sim.m.menu().active_profile, None, "lined up");
}

/// What only asks, or asks nothing, leaves the profile in use: the first press of ×
/// (asking to confirm), Add or a display of the PC shown while there is no PC.
#[test]
fn what_changes_nothing_leaves_the_profile() {
    let mut sim = Sim::new();
    sim.connect(&[1, 2]);
    sim.menu(Action::NewProfile);
    assert_eq!(sim.m.menu().active_profile, Some(0));
    sim.menu(Action::Remove(k(1)));
    assert_eq!(sim.m.menu().active_profile, Some(0), "nothing removed yet");
    sim.menu(Action::Remove(k(1)));
    assert_eq!(sim.m.menu().active_profile, None, "removed");

    let mut sim = Sim::new();
    sim.connect(&[1, 2]);
    sim.menu(Action::NewProfile);
    let other_pc = DisplayKey::new(PcId([4; 8]), DisplayId(7));
    sim.menu(Action::ShowPhysical(other_pc, true));
    assert_eq!(
        sim.m.menu().active_profile,
        Some(0),
        "not asked: no such PC"
    );
    sim.menu(Action::Add {
        width: 1920,
        height: 1080,
    });
    assert_eq!(sim.m.menu().active_profile, None, "asked");
}

/// A profile switched to by the PC's command, whose request the PC never answers: the
/// command is answered too once the request is given up on, not left waiting.
#[test]
fn a_profile_command_is_answered_when_its_request_times_out() {
    let mut sim = Sim::new();
    sim.connect(&[1]);
    sim.menu(Action::NewProfile);
    sim.saves();
    sim.take();
    sim.command(5, Command::Profile { name: "1".into() });
    let fx = sim.take();
    assert_eq!(reply_of(&fx, 5), None, "waits for the profile's own answer");
    sim.after(pending::ANSWER_WITHIN + MS);
    let fx = sim.take();
    assert!(
        matches!(reply_of(&fx, 5), Some(Outcome::Failed { .. })),
        "the PC's command is answered: {fx:?}"
    );
}

/// Taken off for 10 minutes, the app hidden and no frame drawn meanwhile: that is a
/// rest, and no reminder comes 5 minutes after it is put back on.
#[test]
fn off_the_head_without_frames_is_a_rest() {
    let mut sim = Sim::new();
    sim.connect(&[1]);
    sim.menu(Action::Close);
    sim.menu(Action::RestEvery(20));
    let frames = |sim: &mut Sim, d: Duration| {
        let end = sim.now + d;
        while sim.now < end {
            sim.now += 100 * MS;
            let input = FrameInput {
                dt: 0.1,
                ..frame(HandState::default())
            };
            sim.m.tick(sim.now, &input);
            sim.m.animate(sim.now, &input);
        }
    };
    frames(&mut sim, Duration::from_secs(15 * 60));
    assert!(sim.m.toast().is_none());
    sim.handle(Event::Platform(PlatformEvent::Session(
        presence::Session::Hidden,
    )));
    sim.after(MS);
    assert!(!sim.m.worn());
    for _ in 0..600 {
        sim.after(Duration::from_secs(1));
    }
    sim.handle(Event::Platform(PlatformEvent::Session(
        presence::Session::Shown,
    )));
    sim.after(MS);
    assert!(sim.m.worn());
    let end = sim.now + Duration::from_secs(6 * 60);
    while sim.now < end {
        frames(&mut sim, Duration::from_secs(1));
        assert!(
            sim.m
                .toast()
                .is_none_or(|t| t.text != sim.m.texts().t("headset-rest-eyes")),
            "a reminder 5 min after a 10 min rest"
        );
    }
}

/// The encoding and the PC's sound show at once as asked; the PC refuses: they show
/// what they were before, as the PC has them.
#[test]
fn a_refused_encoding_or_sound_shows_what_was() {
    use prostir_proto::Codec;
    use prostir_proto::state::{Encoding, MicDevice, Sound, Wanted};
    let mut sim = Sim::new();
    sim.connect(&[1]);
    let wanted = Wanted {
        bitrate: 40,
        codec: Some(Codec::H264),
        software: false,
        sharp: false,
    };
    sim.net(NetEvent::Part(
        PC,
        HostMsg::Encoding {
            part: Encoding {
                wanted,
                using: None,
                choices: Vec::new(),
            },
        },
    ));
    sim.take();
    sim.menu(Action::SetEncoding(ui::EncodingChange {
        bitrate: Some(80),
        codec: None,
        software: None,
        sharp: None,
    }));
    let bitrate = |sim: &Sim| sim.m.pc().encoding.as_ref().map(|e| e.wanted.bitrate);
    assert_eq!(bitrate(&sim), Some(80), "shown at once");
    let req = requests(&sim.take()).first().expect("asked").0;
    sim.net(NetEvent::Reply {
        pc: PC,
        req,
        outcome: Outcome::failed(ErrorCode::ENCODER, "no"),
    });
    assert_eq!(bitrate(&sim), Some(40), "refused: as it was");
    // The PC says how it stands before refusing: that is what shows.
    sim.menu(Action::SetEncoding(ui::EncodingChange {
        bitrate: Some(80),
        codec: None,
        software: None,
        sharp: None,
    }));
    let req = requests(&sim.take()).first().expect("asked").0;
    sim.net(NetEvent::Part(
        PC,
        HostMsg::Encoding {
            part: Encoding {
                wanted: Wanted {
                    bitrate: 60,
                    ..wanted
                },
                using: None,
                choices: Vec::new(),
            },
        },
    ));
    sim.net(NetEvent::Reply {
        pc: PC,
        req,
        outcome: Outcome::failed(ErrorCode::ENCODER, "no"),
    });
    assert_eq!(bitrate(&sim), Some(60), "as the PC said last");

    let to_headset = |sim: &Sim| sim.m.pc().sound.as_ref().map(|s| s.to_headset);
    sim.net(NetEvent::Part(
        PC,
        HostMsg::Sound {
            part: Sound {
                to_headset: true,
                mic: MicDevice::Off,
            },
        },
    ));
    sim.take();
    sim.menu(Action::SetAudio(false));
    assert_eq!(to_headset(&sim), Some(false), "shown at once");
    let req = requests(&sim.take()).first().expect("asked").0;
    sim.net(NetEvent::Reply {
        pc: PC,
        req,
        outcome: Outcome::failed(ErrorCode::SOUND, "no"),
    });
    assert_eq!(to_headset(&sim), Some(true), "refused: as it was");
}

/// The menu button (or its gesture, or the round button) closes the first-run tips as
/// their own × does: they count as read, and do not open again in front.
#[test]
fn the_menu_button_closes_the_tips() {
    let mut sim = Sim::new();
    sim.connect(&[1]);
    assert!(sim.m.menu().open, "the tips opened the menu");
    assert!(!sim.m.settings().intro_seen);
    crate::interaction::Scene::toggle_menu(&mut sim.m);
    assert!(!sim.m.menu().open);
    sim.frames(11 * MS);
    assert!(!sim.m.menu().open, "closed, and it stays closed");
    assert!(sim.m.settings().intro_seen);
}

/// The keyboard window cannot be shown (the runtime refused it): the menu says so,
/// it is not offered for the rest of this run, and the setting stays as chosen, so the
/// next start tries again.
#[test]
fn a_keyboard_window_that_fails_is_said_and_kept() {
    let mut caps = test_caps();
    caps.room = Some(crate::caps::RoomCaps {
        way: crate::caps::RoomWay::FbPassthrough,
        keyboard_window: true,
    });
    let mut sim = Sim::of(caps);
    sim.connect(&[1]);
    sim.menu(Action::KeyboardWindow(true));
    sim.saves();
    sim.frames(11 * MS);
    assert!(sim.m.keyboard_window().is_some(), "shown");
    sim.handle(Event::Platform(PlatformEvent::KeyboardWindowFailed));
    sim.frames(11 * MS);
    assert!(sim.m.keyboard_window().is_none(), "not shown");
    let view = crate::view::build(&sim.m);
    assert_eq!(
        view.error.as_deref(),
        Some(sim.m.texts().t("headset-error-keyboard-window"))
    );
    assert!(!view.keyboard_available, "not offered now");
    assert!(sim.m.settings().keyboard_window, "kept as chosen");
    assert_eq!(sim.saves(), [], "nothing changed to save");
}

impl Sim {
    /// The PC says it can do `features`.
    fn host_can(&mut self, features: Vec<state::Feature>) {
        let part = state::HostCaps { features };
        self.net(NetEvent::Part(PC, HostMsg::HostCaps { part }));
    }

    /// Frames of 11 ms for `d` with the head turned to look along `forward`.
    fn look(&mut self, forward: Vec3, d: Duration) {
        let end = self.now + d;
        while self.now < end {
            self.now += 11 * MS;
            let mut input = frame(HandState::default());
            input.head = Some(Head { forward, ..head() });
            self.m.tick(self.now, &input);
            self.m.animate(self.now, &input);
            self.check_view();
        }
    }

    /// The direction from the head to monitor `n`.
    fn towards(&self, n: u32) -> Vec3 {
        let at = self.m.world().shown(k(n)).expect("drawn").position;
        (at - self.m.head.0).normalize()
    }
}

/// The `Gaze` messages sent to the PC among `fx`.
fn gazes(fx: &[Effect]) -> Vec<(Option<DisplayId>, u16, u16)> {
    sent(fx)
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Gaze { display, u, v } => Some((*display, *u, *v)),
            _ => None,
        })
        .collect()
}

/// A PC that takes the gaze (feature 14) hears which monitor is looked at, once for each
/// time it changes (not every frame), and that none is looked at any more; the menu
/// shows none of it.
#[test]
fn the_pc_that_takes_the_gaze_hears_the_monitor_looked_at() {
    let mut sim = Sim::new();
    sim.connect(&[1, 2]);
    // The first run's tips opened the menu in front of the monitors.
    sim.menu(Action::Close);
    sim.host_can(vec![caps::pc::GAZE]);
    let (one, two) = (sim.towards(1), sim.towards(2));
    assert_ne!(one, two);
    let rev = sim.m.rev();

    sim.look(one, Duration::from_secs(2));
    let fx = sim.take();
    let told = gazes(&fx);
    assert_eq!(told.len(), 1, "once, not every frame: {told:?}");
    assert_eq!(told[0].0, Some(DisplayId(1)));
    sim.look(one, Duration::from_secs(2));
    assert_eq!(gazes(&sim.take()), [], "nothing while it stays");

    sim.look(two, Duration::from_secs(2));
    let told = gazes(&sim.take());
    assert_eq!(told.len(), 1, "{told:?}");
    assert_eq!(told[0].0, Some(DisplayId(2)));

    // Over the monitors: none looked at.
    sim.look(Vec3::Y, Duration::from_secs(2));
    assert_eq!(gazes(&sim.take()), [(None, 0, 0)]);
    sim.look(Vec3::Y, Duration::from_secs(2));
    assert_eq!(gazes(&sim.take()), []);
    assert_eq!(sim.m.rev(), rev, "the menu shows none of it");
}

/// A PC that did not name feature 14 never hears `Gaze`, whatever is looked at; one that
/// names it later hears where the gaze is then.
#[test]
fn a_pc_that_does_not_take_the_gaze_hears_none() {
    let mut sim = Sim::new();
    sim.connect(&[1, 2]);
    // The first run's tips opened the menu in front of the monitors.
    sim.menu(Action::Close);
    sim.host_can(vec![caps::pc::SCALE]);
    let (one, two) = (sim.towards(1), sim.towards(2));
    for forward in [one, two, Vec3::Y, one] {
        sim.look(forward, Duration::from_secs(2));
    }
    assert_eq!(gazes(&sim.take()), []);
    sim.host_can(vec![caps::pc::GAZE]);
    sim.look(one, Duration::from_secs(1));
    let told = gazes(&sim.take());
    assert_eq!(told.len(), 1, "{told:?}");
    assert_eq!(told[0].0, Some(DisplayId(1)));
}

/// Without a frame (the headset off the head) the gaze is on no monitor.
#[test]
fn a_pass_without_a_frame_looks_at_none() {
    let mut sim = Sim::new();
    sim.connect(&[1, 2]);
    // The first run's tips opened the menu in front of the monitors.
    sim.menu(Action::Close);
    sim.host_can(vec![caps::pc::GAZE]);
    let one = sim.towards(1);
    sim.look(one, Duration::from_secs(1));
    assert_eq!(gazes(&sim.take()).len(), 1);
    sim.after(Duration::from_secs(1));
    sim.after(Duration::from_secs(1));
    assert_eq!(gazes(&sim.take()), [(None, 0, 0)]);
}

/// A session that ends and opens again starts with the PC knowing nothing: the monitor
/// looked at is told again, though it has not changed.
#[test]
fn a_pc_that_comes_back_hears_the_gaze_again() {
    let mut sim = Sim::new();
    sim.connect(&[1, 2]);
    // The first run's tips opened the menu in front of the monitors.
    sim.menu(Action::Close);
    sim.host_can(vec![caps::pc::GAZE]);
    let one = sim.towards(1);
    sim.look(one, Duration::from_secs(1));
    assert_eq!(gazes(&sim.take()).len(), 1);
    // The session ends (nothing is told to a PC that is gone), then opens again.
    sim.net(NetEvent::Ended {
        step: StepId(1),
        pc: Some(PC),
        why: Disconnect::Closed,
        ended: Ended {
            opened: true,
            ..Ended::default()
        },
        lasted: Duration::from_secs(60),
    });
    assert!(!sim.m.session().connected(PC));
    sim.look(one, Duration::from_secs(1));
    assert_eq!(gazes(&sim.take()), [], "nothing to a PC that is gone");
    let opened_with = sim.m.headset_state();
    sim.handle(Event::Connected {
        pc: PC,
        addr: addr(),
        language: None,
        opened_with,
    });
    sim.list(&[1, 2]);
    for n in [1, 2] {
        sim.stream(n, Gen(2), (2560, 1440));
    }
    sim.host_can(vec![caps::pc::GAZE]);
    let one = sim.towards(1);
    sim.look(one, Duration::from_secs(2));
    let told = gazes(&sim.take());
    assert_eq!(told.len(), 1, "{told:?}");
    assert_eq!(told[0].0, Some(DisplayId(1)));
}

const PC_B: PcId = PcId([4; 8]);

impl Sim {
    /// A second PC opens a session, listing `monitors` and streaming them, and takes the
    /// gaze (the search holds one PC, but a session may open as this one does).
    fn connect_second(&mut self, monitors: &[u32]) {
        let opened_with = self.m.headset_state();
        self.handle(Event::Connected {
            pc: PC_B,
            addr: SocketAddr::new(
                Ipv4Addr::new(192, 168, 1, 21).into(),
                prostir_proto::TCP_PORT,
            ),
            language: None,
            opened_with,
        });
        let part = Displays {
            displays: monitors.iter().map(|n| monitor(*n)).collect(),
            primary: monitors.first().map(|n| DisplayId(*n)),
        };
        self.net(NetEvent::Part(PC_B, HostMsg::Displays { part }));
        for n in monitors {
            self.handle(Event::StreamStarted {
                id: DisplayKey::new(PC_B, DisplayId(*n)),
                generation: Gen(1),
                codec: Codec::H264,
                size: (2560, 1440),
            });
        }
        let part = state::HostCaps {
            features: vec![caps::pc::GAZE],
        };
        self.net(NetEvent::Part(PC_B, HostMsg::HostCaps { part }));
        self.frames(Duration::from_secs(1));
        self.take();
    }

    /// The direction from the head to monitor `id`.
    fn towards_key(&self, id: DisplayKey) -> Vec3 {
        let at = self.m.world().shown(id).expect("drawn").position;
        (at - self.m.head.0).normalize()
    }
}

/// The `Gaze` messages sent to PC `pc` among `fx`.
fn gazes_to(fx: &[Effect], pc: PcId) -> Vec<Option<DisplayId>> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Send(to, ClientMsg::Gaze { display, .. }) if *to == pc => Some(*display),
            _ => None,
        })
        .collect()
}

/// The gaze goes from a monitor of one PC to a monitor of another: the first hears that
/// none of its monitors is looked at, the second which one is. A PC that is gone while
/// the gaze leaves it hears nothing.
#[test]
fn the_gaze_going_from_one_pc_to_another_tells_both() {
    let mut sim = Sim::new();
    sim.connect(&[1, 2]);
    sim.menu(Action::Close);
    sim.host_can(vec![caps::pc::GAZE]);
    sim.connect_second(&[7]);
    sim.menu(Action::Close);
    let (a, b) = (k(1), DisplayKey::new(PC_B, DisplayId(7)));
    let (to_a, to_b) = (sim.towards_key(a), sim.towards_key(b));
    assert_ne!(to_a, to_b);

    // The head looked straight ahead, at A, while B opened, and A was told: look away to start from none.
    sim.look(Vec3::Y, Duration::from_secs(2));
    sim.take();
    sim.look(to_a, Duration::from_secs(2));
    let fx = sim.take();
    assert_eq!(gazes_to(&fx, PC), [Some(DisplayId(1))]);
    assert_eq!(gazes_to(&fx, PC_B), []);

    sim.look(to_b, Duration::from_secs(2));
    let fx = sim.take();
    assert_eq!(gazes_to(&fx, PC), [None], "the one it left");
    assert_eq!(gazes_to(&fx, PC_B), [Some(DisplayId(7))]);

    // Back to A, then A goes: looking at B again, B hears of it and A of nothing.
    sim.look(to_a, Duration::from_secs(2));
    sim.take();
    sim.net(NetEvent::Ended {
        step: StepId(1),
        pc: Some(PC),
        why: Disconnect::Closed,
        ended: Ended {
            opened: true,
            ..Ended::default()
        },
        lasted: Duration::from_secs(60),
    });
    sim.look(to_b, Duration::from_secs(2));
    let fx = sim.take();
    assert_eq!(gazes_to(&fx, PC), []);
    assert_eq!(gazes_to(&fx, PC_B), [Some(DisplayId(7))]);
}

/// What the headset told the PC of its volume among `fx`.
fn volumes(fx: &[Effect]) -> Vec<state::Volume> {
    sent(fx)
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Volume { part } => Some(*part),
            _ => None,
        })
        .collect()
}

/// What the system was asked to set its volume to among `fx`.
fn volume_sets(fx: &[Effect]) -> Vec<(u8, bool, u32)> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Sound(SoundCmd::SetVolume { level, muted, n }) => Some((*level, *muted, *n)),
            _ => None,
        })
        .collect()
}

fn volume_ask(level: u8, seen: u32, ask: u32) -> NetEvent {
    NetEvent::VolumeAsk {
        pc: PC,
        asked: state::VolumeAsk {
            level,
            muted: false,
            seen,
            ask,
        },
    }
}

fn volume_read(level: u8, done: u32) -> Event {
    Event::Platform(PlatformEvent::Volume {
        level,
        muted: false,
        done,
    })
}

fn heard(level: u8, seq: u32, ask: u32) -> state::Volume {
    state::Volume {
        level,
        muted: false,
        seq,
        ask,
    }
}

/// One volume with the PC on a headset whose system volume goes in 15 steps: `Open`
/// names it and carries the level; the PC's request goes to the next step the way it
/// moved, set at once and answered at once; the system's reading of it says nothing
/// more, nor one from before the newest set; a reading that differs is a change of the
/// headset's own; a request that did not see it is not taken; the next `Open` carries
/// the level; the system's volume is followed only while the session is open.
#[test]
fn one_volume_with_the_pc() {
    let mut sim = Sim::of(Caps {
        volume_steps: Some(15),
        ..test_caps()
    });
    let open = sim.m.headset_state();
    assert!(open.caps.unwrap().has(caps::headset::VOLUME));
    assert_eq!(open.volume, Some(heard(40, 0, 0)));
    sim.connect(&[1]);
    let read_every = |sim: &Sim| {
        sim.m
            .volume
            .as_ref()
            .and_then(volume::SharedVolume::watched)
    };
    assert_eq!(read_every(&sim), Some(volume::READ_IN_SESSION));

    // Up by 1 %: the step above, at once.
    sim.net(volume_ask(41, 0, 1));
    let fx = sim.take();
    assert_eq!(volume_sets(&fx), [(47, false, 1)]);
    assert_eq!(volumes(&fx), [heard(47, 0, 1)]);
    sim.handle(volume_read(47, 1));
    assert!(sim.take().is_empty(), "the set read back");

    // Later, down: the step below. A reading from before that set is old.
    sim.after(Duration::from_millis(300));
    sim.net(volume_ask(30, 0, 2));
    let fx = sim.take();
    assert_eq!(volume_sets(&fx), [(27, false, 2)]);
    assert_eq!(volumes(&fx), [heard(27, 0, 2)]);
    sim.handle(volume_read(47, 1));
    sim.handle(volume_read(27, 2));
    assert!(
        volumes(&sim.take()).is_empty(),
        "old, then the set read back"
    );

    // The same level again: nothing to set.
    sim.after(Duration::from_millis(300));
    sim.net(volume_ask(27, 0, 3));
    let fx = sim.take();
    assert!(volume_sets(&fx).is_empty());
    assert_eq!(volumes(&fx), [heard(27, 0, 3)]);

    // The buttons: a change of the headset's own, told as such.
    sim.handle(volume_read(60, 2));
    assert_eq!(volumes(&sim.take()), [heard(60, 1, 0)]);
    // A request that had not seen it stands back; the answer says the level.
    sim.net(volume_ask(20, 0, 4));
    let fx = sim.take();
    assert!(volume_sets(&fx).is_empty());
    assert_eq!(volumes(&fx), [heard(60, 1, 4)]);

    // The system took a set otherwise (a limit of its own): its level stands.
    sim.after(Duration::from_millis(300));
    sim.net(volume_ask(80, 1, 5));
    let fx = sim.take();
    assert_eq!(volume_sets(&fx), [(80, false, 3)]);
    assert_eq!(volumes(&fx), [heard(80, 1, 5)]);
    sim.handle(volume_read(73, 3));
    assert_eq!(volumes(&sim.take()), [heard(73, 2, 0)]);

    // The next `Open` says it, once, at the end of the pass.
    sim.m.end_pass();
    let told: Vec<_> = sim
        .take()
        .into_iter()
        .filter_map(|e| match e {
            Effect::Net(NetCmd::SetHeadset(state)) => state.volume,
            _ => None,
        })
        .collect();
    assert_eq!(told, [heard(73, 2, 0)]);
    sim.m.end_pass();
    assert!(sim.take().is_empty(), "told once");

    // The session ends: read less often, so the next `Open` says it as it is.
    sim.net(NetEvent::Ended {
        step: StepId(1),
        pc: Some(PC),
        why: Disconnect::Closed,
        ended: Ended::default(),
        lasted: Duration::from_secs(60),
    });
    sim.m.end_pass();
    let watch = SystemCmd::WatchVolume {
        steps: 15,
        every: Some(volume::READ_WITHOUT_SESSION),
    };
    assert!(sim.take().contains(&Effect::System(watch)));
    // Taken off: not read at all until it is worn again.
    sim.handle(Event::Platform(PlatformEvent::Paused(true)));
    sim.after(Duration::from_secs(10));
    sim.m.end_pass();
    let watch = SystemCmd::WatchVolume {
        steps: 15,
        every: None,
    };
    assert!(sim.take().contains(&Effect::System(watch)), "not worn");
    assert_eq!(read_every(&sim), None);
}

/// The PC mutes and unmutes, and asks for silence, on a headset whose system keeps
/// Android's rules (muted it says step 0, step 0 mutes): what the system then says is
/// what the headset answered, never a change of the headset's own, so the PC keeps its
/// level while muted, and an unmute stays.
#[test]
fn mute_and_silence_on_androids_rules() {
    use crate::platform::media_volume::{AndroidRules, MediaVolume};
    let mut sim = Sim::of(Caps {
        volume_steps: Some(15),
        ..test_caps()
    });
    sim.connect(&[1]);
    let mut media = AndroidRules {
        max: 15,
        index: 6,
        muted: false,
    };
    let mut system = MediaVolume::new(15);
    assert_eq!(system.read(&mut media), Some((40, false)));
    let asked = [
        ((40, true), (40, true)),
        ((40, false), (40, false)),
        ((0, false), (0, false)),
        ((41, false), (47, false)),
        ((47, true), (47, true)),
        ((47, false), (47, false)),
    ];
    for (ask, ((level, muted), answered)) in (1..).zip(asked) {
        sim.after(Duration::from_millis(300));
        sim.net(NetEvent::VolumeAsk {
            pc: PC,
            asked: state::VolumeAsk {
                level,
                muted,
                seen: 0,
                ask,
            },
        });
        let fx = sim.take();
        let case = format!("asked {level}, muted {muted}");
        let answer = volumes(&fx);
        assert_eq!(
            answer
                .iter()
                .map(|v| (v.level, v.muted))
                .collect::<Vec<_>>(),
            [answered],
            "{case}"
        );
        for (l, m, n) in volume_sets(&fx) {
            // As the system thread says it: right after the set, and at the next read.
            let (l, m) = system.set(&mut media, l, m).unwrap();
            sim.handle(Event::Platform(PlatformEvent::Volume {
                level: l,
                muted: m,
                done: n,
            }));
            let (l, m) = system.read(&mut media).unwrap();
            sim.handle(Event::Platform(PlatformEvent::Volume {
                level: l,
                muted: m,
                done: n,
            }));
        }
        let told = volumes(&sim.take());
        assert!(told.is_empty(), "a change of its own ({case}): {told:?}");
    }
    assert_eq!((media.index, media.muted), (7, false), "the unmute stays");
}

/// A headset whose system volume the app cannot set: `Open` neither names the shared
/// volume nor carries a level, and the PC's request (were it to come) does nothing.
#[test]
fn a_fixed_volume_is_not_shared() {
    let mut sim = Sim::of(test_caps());
    let open = sim.m.headset_state();
    assert!(!open.caps.unwrap().has(caps::headset::VOLUME));
    assert_eq!(open.volume, None);
    sim.connect(&[1]);
    sim.net(volume_ask(41, 0, 1));
    assert!(sim.take().is_empty());
    // One the app could set but could not read at the start: neither.
    let steps = Caps {
        volume_steps: Some(15),
        ..test_caps()
    };
    let open = headset_state(&Settings::default(), true, None, None, 0, &steps);
    assert!(!open.caps.unwrap().has(caps::headset::VOLUME));
    assert_eq!(open.volume, None);
    let open = headset_state(&Settings::default(), true, None, Some((7, true)), 0, &steps);
    assert!(open.caps.unwrap().has(caps::headset::VOLUME));
    assert_eq!(
        open.volume,
        Some(state::Volume {
            level: 7,
            muted: true,
            seq: 0,
            ask: 0
        })
    );
}

/// What the shell has for the plan of the frame's layers: a picture for every monitor,
/// every picture drawn, nothing wrong with any.
struct AllReady;

impl crate::layers::Ready for AllReady {
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
    // The numbers of "For development" (the preview build) are not counted here.
    #[cfg(feature = "preview")]
    fn metrics(&self) -> bool {
        false
    }
    fn hands(&self) -> bool {
        true
    }
    fn pc_cursor(&self, _: PcId, _: u32) -> bool {
        true
    }
}

/// The PC's mouse pointer (PC feature 16): a look and where it is make a layer right
/// over its monitor, the size it has in the picture, its point where the PC says; the
/// menu hears none of it. Hidden, off the displays, or in a look that never came, none
/// is drawn; its looks go with the session.
#[test]
fn the_pcs_pointer_is_drawn_over_its_monitor() {
    use crate::layers::{Layer, Plan};
    let mut sim = Sim::new();
    sim.connect(&[1, 2]);
    sim.menu(Action::Close);
    sim.frames(Duration::from_millis(100));
    let rev = sim.m.rev();
    let look = crate::cursors::Look {
        w: 32,
        h: 32,
        hot_x: 0,
        hot_y: 0,
        rgba: vec![255; 32 * 32 * 4],
    };
    sim.net(NetEvent::CursorShape {
        pc: PC,
        shape: 7,
        look,
    });
    let at = |sim: &mut Sim, display: Option<u32>, u: u16, v: u16, shape: u32| {
        sim.net(NetEvent::CursorAt {
            pc: PC,
            display: display.map(DisplayId),
            u,
            v,
            shape,
        });
    };
    at(&mut sim, Some(2), 0, 0, 7);
    assert_eq!(sim.m.rev(), rev, "the menu shows nothing of the pointer");
    let mut plan = Plan::new();
    crate::layers::plan(&sim.m, &Hands::new(), &AllReady, &mut plan);
    let layers = plan.layers();
    let i = layers
        .iter()
        .position(|l| matches!(l, Layer::PcCursor { .. }))
        .expect("the pointer is drawn");
    let (
        Layer::Monitor(m),
        Layer::PcCursor {
            pc,
            shape,
            quad,
            bright,
            alpha,
        },
    ) = (layers[i - 1], layers[i])
    else {
        panic!("right after its monitor: {layers:?}");
    };
    assert_eq!((m.id, pc, shape), (k(2), PC, 7));
    assert_eq!(
        (bright, alpha),
        (m.bright, m.alpha),
        "dimmed with its monitor"
    );
    // The whole picture the look is drawn in: 256 of the picture's 2560 pixels wide
    // (along the arc on a curve), 256 of its 1440 high.
    let p = m.quad.at;
    let width = layout::image_width(&p, m.curve);
    assert!(
        (quad.at.width - width * 256.0 / 2560.0).abs() < 1e-5,
        "{quad:?}"
    );
    assert!((quad.height - m.quad.height * 256.0 / 1440.0).abs() < 1e-6);
    // Its top left corner (the pixel it points with) at the picture's top left.
    let corner = quad.at.position
        + quad.at.rotation * Vec3::new(-quad.at.width / 2.0, quad.height / 2.0, 0.0);
    let (top_left, _) = layout::image_frame(&p, m.quad.height, m.curve, Vec2::ZERO);
    assert!(corner.distance(top_left) < 0.001, "{corner} {top_left}");
    let pointers = |sim: &Sim, plan: &mut Plan| {
        crate::layers::plan(&sim.m, &Hands::new(), &AllReady, plan);
        plan.layers()
            .iter()
            .filter(|l| matches!(l, Layer::PcCursor { .. }))
            .count()
    };
    at(&mut sim, Some(2), 100, 100, 0);
    assert_eq!(pointers(&sim, &mut plan), 0, "hidden");
    at(&mut sim, None, 0, 0, 7);
    assert_eq!(pointers(&sim, &mut plan), 0, "on none of the displays");
    at(&mut sim, Some(1), 30_000, 30_000, 8);
    assert_eq!(pointers(&sim, &mut plan), 0, "a look that never came");
    at(&mut sim, Some(1), 30_000, 30_000, 7);
    assert_eq!(pointers(&sim, &mut plan), 1);
    assert_eq!(sim.m.rev(), rev);
    // The session ends: its pointer goes with it.
    sim.net(NetEvent::Ended {
        step: StepId(1),
        pc: Some(PC),
        why: Disconnect::Silence,
        ended: Ended::default(),
        lasted: Duration::from_secs(5),
    });
    assert!(!sim.m.cursors().has(PC, 7));
}

/// A PC's pointer goes with its monitor when the compositor's layers are too few for
/// every monitor: never drawn over nothing, and taking no place then.
#[test]
fn a_pointer_goes_with_its_monitor_out_of_the_layers() {
    use crate::layers::{Layer, Plan};
    let mut caps = test_caps();
    caps.layers_max = 5;
    let mut sim = Sim::of(caps);
    sim.connect(&[1, 2, 3]);
    sim.menu(Action::Close);
    sim.frames(Duration::from_millis(100));
    sim.net(NetEvent::CursorShape {
        pc: PC,
        shape: 7,
        look: crate::cursors::Look {
            w: 2,
            h: 2,
            hot_x: 0,
            hot_y: 0,
            rgba: vec![255; 16],
        },
    });
    let mut plan = Plan::new();
    let monitors = |plan: &Plan| -> Vec<DisplayKey> {
        plan.layers()
            .iter()
            .filter_map(|l| match l {
                Layer::Monitor(m) => Some(m.id),
                _ => None,
            })
            .collect()
    };
    let at = |sim: &mut Sim, n: u32, shape: u32| {
        sim.net(NetEvent::CursorAt {
            pc: PC,
            display: Some(DisplayId(n)),
            u: 1000,
            v: 1000,
            shape,
        });
    };
    at(&mut sim, 1, 0);
    crate::layers::plan(&sim.m, &Hands::new(), &AllReady, &mut plan);
    let without = monitors(&plan);
    assert!(without.len() < 3, "a monitor left out: {without:?}");
    let mut drawn = 0;
    for n in [1, 2, 3] {
        at(&mut sim, n, 7);
        crate::layers::plan(&sim.m, &Hands::new(), &AllReady, &mut plan);
        let layers = plan.layers();
        match layers
            .iter()
            .position(|l| matches!(l, Layer::PcCursor { .. }))
        {
            Some(i) => {
                assert!(
                    matches!(layers[i - 1], Layer::Monitor(m) if m.id == k(n)),
                    "over its monitor: {layers:?}"
                );
                drawn += 1;
            }
            // Its monitor left out: as if it were not there.
            None => assert_eq!(monitors(&plan), without, "the pointer on {n}"),
        }
    }
    assert!(drawn < 3, "not drawn over a monitor left out");
}

/// Where monitor `id` is drawn in `plan`.
fn planned(plan: &crate::layers::Plan, id: DisplayKey) -> Option<Placement> {
    plan.layers().iter().find_map(|l| match l {
        crate::layers::Layer::Monitor(m) if m.id == id => Some(m.quad.at),
        _ => None,
    })
}

/// Scenario 17: a drag and the frame's layers in one pass of the loop, as the shell
/// runs them: the monitor is drawn where the hand put it in this very frame, never
/// where it was the frame before; and a monitor still gliding to its place is taken
/// where it is drawn, without a jump.
#[test]
fn a_dragged_monitor_is_drawn_where_the_hand_put_it_in_the_same_frame() {
    let mut sim = Sim::new();
    sim.connect(&[1, 2]);
    sim.menu(Action::Close);
    // The first monitor of the row glides to the second one's place.
    let id = sim.m.world().monitors()[0].0;
    sim.menu(Action::Move { id, delta: 1 });
    let input = frame(HandState::default());
    sim.now += 11 * MS;
    sim.m.tick(sim.now, &input);
    sim.m.animate(sim.now, &input);
    let world = sim.m.world();
    let drawn = *world.shown(id).expect("drawn");
    let target = *world.placement(id).expect("placed");
    assert!(
        drawn.position.distance(target.position) > 0.05,
        "still gliding"
    );
    let origin = Vec3::new(0.0, -0.3, 0.0);
    let mut right = HandState {
        ray: Some(Ray {
            origin,
            dir: (drawn.position - origin).normalize(),
        }),
        ..HandState::default()
    };
    let mut hands = Hands::new();
    let mut plan = crate::layers::Plan::new();
    // One pass: the hands, the end of the frame, the layers.
    let mut pass = |sim: &mut Sim, hands: &mut Hands, dir: Vec3| -> Option<Placement> {
        sim.now += 11 * MS;
        right.grab.update(1.0);
        right.ray = Some(Ray { origin, dir });
        let input = frame(right);
        sim.m.tick(sim.now, &input);
        hands.buttons(sim.now, &input, &mut sim.m);
        hands.step(sim.now, &input, &mut sim.m, &mut Pictures(None));
        sim.m.animate(sim.now, &input);
        crate::layers::plan(&sim.m, hands, &AllReady, &mut plan);
        sim.check_view();
        planned(&plan, id)
    };
    let straight = (drawn.position - origin).normalize();
    let grabbed = pass(&mut sim, &mut hands, straight).expect("planned");
    assert!(sim.m.world().holds(Thing::Display(id)));
    assert!(
        grabbed.position.distance(drawn.position) < 0.02,
        "taken where it was drawn: {grabbed:?}, drawn {drawn:?}"
    );
    // The hand moves: each frame draws where the hand put the monitor in it.
    let mut before = grabbed;
    for i in 1..=20 {
        let turn = glam::Quat::from_rotation_y(-0.01 * i as f32);
        let now = pass(&mut sim, &mut hands, turn * straight).expect("planned");
        let placed = *sim.m.world().placement(id).expect("placed");
        assert_eq!(now, placed, "frame {i}: drawn where the hand put it");
        assert!(
            now.position.distance(before.position) > 1e-4,
            "frame {i}: it moved in this frame"
        );
        before = now;
    }
}

/// With fewer layers than the frame wants, the farthest monitor is not drawn and the
/// person is told once; told again only after a while with every monitor shown.
#[test]
fn monitors_that_do_not_fit_are_said_once() {
    let mut sim = Sim::of(Caps {
        layers_max: 6,
        ..test_caps()
    });
    sim.connect(&[1, 2, 3, 4]);
    sim.menu(Action::Close);
    sim.frames(Duration::from_secs(1));
    let mut plan = crate::layers::Plan::new();
    let hands = Hands::new();
    crate::layers::plan(&sim.m, &hands, &AllReady, &mut plan);
    // The background, four monitors, the menu's button and the hands: one too many.
    assert_eq!(plan.wanted(), 7);
    assert_eq!(plan.layers().len(), 6);
    assert_eq!(plan.hidden(), 1);
    let world = sim.m.world();
    let farthest = world
        .draw_list()
        .iter()
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .map(|d| d.1)
        .expect("drawn");
    assert_eq!(planned(&plan, farthest), None, "the farthest is not drawn");
    assert!(
        world
            .draw_list()
            .iter()
            .all(|&(_, id)| id == farthest || planned(&plan, id).is_some())
    );
    assert!(matches!(
        plan.layers().last(),
        Some(crate::layers::Layer::Hands)
    ));
    let said = |sim: &Sim| sim.m.toast().map(|t| (t.n, t.text.clone()));
    let hidden = plan.news().expect("said");
    sim.handle(Event::Platform(PlatformEvent::LayersHidden(hidden)));
    let told = said(&sim).expect("told");
    assert_eq!(
        told.1,
        sim.m
            .texts()
            .f("headset-layers-hidden", &[("count", 1.into())])
    );
    assert_eq!(plan.news(), None);
    // The toast itself takes a layer once placed, but the count stays: no more said.
    sim.frames(Duration::from_millis(50));
    crate::layers::plan(&sim.m, &hands, &AllReady, &mut plan);
    assert!(
        plan.layers()
            .iter()
            .any(|l| matches!(l, crate::layers::Layer::Toast(_)))
    );
    assert_eq!(plan.hidden(), 1);
    assert_eq!(plan.news(), None);
    // Shown again for a moment, and hidden again: not said again.
    sim.handle(Event::Platform(PlatformEvent::LayersHidden(0)));
    sim.after(Duration::from_secs(2));
    sim.handle(Event::Platform(PlatformEvent::LayersHidden(1)));
    assert_eq!(said(&sim).map(|t| t.0), Some(told.0), "the same toast");
    // One more hidden: said.
    sim.handle(Event::Platform(PlatformEvent::LayersHidden(2)));
    let more = said(&sim).expect("told");
    assert_ne!(more.0, told.0);
    // A minute with every monitor shown: the next one is said again.
    sim.handle(Event::Platform(PlatformEvent::LayersHidden(0)));
    sim.after(HIDDEN_SAID_AGAIN);
    sim.handle(Event::Platform(PlatformEvent::LayersHidden(1)));
    assert_ne!(said(&sim).map(|t| t.0), Some(more.0), "said again");
}

/// The layers in their order: the background, the monitors, the menu's button, the
/// menu, the toast and the palm menu, the hands, the dots.
#[test]
fn the_layers_go_back_to_front() {
    use crate::layers::Layer;
    let mut sim = Sim::new();
    sim.connect(&[1, 2]);
    sim.menu(Action::OpenMenu);
    sim.frames(Duration::from_millis(500));
    let mut plan = crate::layers::Plan::new();
    let hands = Hands::new();
    crate::layers::plan(&sim.m, &hands, &AllReady, &mut plan);
    let kinds: Vec<&str> = plan
        .layers()
        .iter()
        .map(|l| match l {
            Layer::Background => "background",
            Layer::Monitor(_) => "monitor",
            Layer::MenuButton(_) => "button",
            Layer::Menu(_) => "menu",
            Layer::Hands => "hands",
            _ => "other",
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "background",
            "monitor",
            "monitor",
            "button",
            "menu",
            "hands"
        ]
    );
    assert_eq!(plan.hidden(), 0);
    // Where each monitor is drawn, as the world draws it, in its order.
    let order: Vec<DisplayKey> = sim.m.world().draw_list().iter().map(|d| d.1).collect();
    let monitors: Vec<DisplayKey> = plan
        .layers()
        .iter()
        .filter_map(|l| match l {
            Layer::Monitor(m) => Some(m.id),
            _ => None,
        })
        .collect();
    assert_eq!(monitors, order);
    for id in order {
        assert_eq!(planned(&plan, id), sim.m.world().shown(id).copied());
    }
}

/// On a headset of 25 pixels a degree (in the table) the monitors are made 1:1 and
/// their layers only sharpened. "For
/// development" (shown in the preview build only) lays them out again at another size
/// at once, its filters change the layers' with the next frame, and "Reset" puts the
/// monitors back; the menu hears of each (its change moves).
#[cfg(feature = "preview")]
#[test]
fn for_development_changes_the_sizes_and_the_filters() {
    use crate::clarity::{Filter, MonitorSize, Quality};
    let mut sim = Sim::of(Caps {
        model: crate::platform::pixels::MODELS[0].0.into(),
        ..test_caps()
    });
    sim.connect(&[1, 2]);
    assert!(crate::view::build(&sim.m).dev.is_some());
    let world = sim.m.world();
    assert_eq!(world.pixels_per_degree(), Some(25.0));
    let scale = world.shown_scale(k(1)).expect("known");
    assert!((scale - 1.0).abs() < 0.02, "{scale}");
    let only_sharpened = Quality {
        super_sampling: false,
        sharpening: true,
    };
    assert_eq!(world.layer_quality(k(1)), only_sharpened);
    let before = *world.placement(k(1)).expect("placed");
    let rev = sim.m.rev();
    sim.menu(Action::DevSize(MonitorSize::Standard));
    assert!(sim.m.rev() > rev);
    sim.frames(20 * MS);
    let world = sim.m.world();
    assert_eq!(world.dev().size, MonitorSize::Standard);
    let standard = *world.placement(k(1)).expect("placed");
    assert!(standard.width < before.width * 0.9);
    assert_eq!(world.layer_quality(k(1)), Quality::BOTH, "shown smaller");
    sim.menu(Action::DevSuperSampling(Filter::Off));
    sim.menu(Action::DevSharpening(Filter::Off));
    sim.frames(20 * MS);
    assert_eq!(
        sim.m.world().layer_quality(k(1)),
        Quality {
            super_sampling: false,
            sharpening: false
        }
    );
    sim.menu(Action::DevSizeReset);
    sim.frames(20 * MS);
    let world = sim.m.world();
    assert_eq!(world.dev().size, MonitorSize::OneToOne);
    assert_eq!(*world.placement(k(1)).expect("placed"), before);
    sim.menu(Action::DevMetrics(false));
    assert!(!sim.m.world().dev().metrics);
    assert_eq!(sim.m.metrics_place(), None);
}

/// Auto, wired: the main headset of a known model asks a PC that changes scales in
/// place for the scale a monitor in Auto wants, once the arrangement held, and not
/// again at once; nothing for a monitor not in Auto, from a guest, or from a PC
/// without the capability.
#[test]
fn auto_asks_the_scale_a_monitor_wants() {
    let mut sim = Sim::of(Caps {
        model: crate::platform::pixels::MODELS[0].0.into(),
        ..test_caps()
    });
    sim.connect(&[1, 2]);
    // 1:1 in the headset, at 150 % on the PC: 100 % is enough.
    let at_150 = |n| Display {
        mode: Mode {
            scale: 150,
            ..Mode::new(2560, 1440)
        },
        ..monitor(n)
    };
    let part = Displays {
        displays: vec![at_150(1), at_150(2)],
        primary: Some(DisplayId(1)),
    };
    sim.net(NetEvent::Part(PC, HostMsg::Displays { part }));
    sim.limits(4, true);
    let auto = |ids: Vec<u32>| HostMsg::ScaleAuto {
        part: state::ScaleAuto {
            auto: ids.into_iter().map(DisplayId).collect(),
        },
    };
    sim.net(NetEvent::Part(PC, auto(vec![1])));
    sim.net(NetEvent::Part(
        PC,
        HostMsg::Access {
            part: state::Access::MAIN,
        },
    ));
    sim.frames(Duration::from_secs(2));
    assert!(
        requests(&sim.take()).is_empty(),
        "a PC without the capability"
    );
    sim.host_can(vec![caps::pc::SCALE, caps::pc::SCALE_IN_PLACE]);
    sim.frames(Duration::from_secs(2));
    let asked = requests(&sim.take());
    assert!(
        matches!(
            &asked[..],
            [(_, Request::AutoScale { id, scale: 100 })] if *id == DisplayId(1)
        ),
        "monitor 1 only, once: {asked:?}"
    );
    assert!(crate::view::build(&sim.m).can_auto);
    assert_eq!(crate::view::build(&sim.m).auto, [k(1)]);
    sim.frames(Duration::from_secs(1));
    assert!(requests(&sim.take()).is_empty(), "not again at once");
    // A guest asks nothing.
    sim.net(NetEvent::Part(
        PC,
        HostMsg::Access {
            part: state::Access {
                role: state::role::GUEST,
                perms: state::perms::ALL,
            },
        },
    ));
    sim.net(NetEvent::Part(PC, auto(vec![1, 2])));
    sim.frames(Duration::from_secs(5));
    assert!(requests(&sim.take()).is_empty(), "a guest");
}

/// With Auto: a profile keeps which monitors were in it, and switched to, gives the
/// monitors it made Auto or the profile's own scale; a monitor added in Auto is made
/// at the scale its place in the room wants, not at 100 % to change once shown.
#[test]
fn auto_in_profiles_and_new_monitors() {
    let mut sim = Sim::of(Caps {
        model: crate::platform::pixels::MODELS[0].0.into(),
        ..test_caps()
    });
    sim.connect(&[1, 2]);
    sim.limits(4, true);
    sim.host_can(vec![caps::pc::SCALE, caps::pc::SCALE_IN_PLACE]);
    sim.net(NetEvent::Part(
        PC,
        HostMsg::Access {
            part: state::Access::MAIN,
        },
    ));
    let auto = |ids: Vec<u32>| HostMsg::ScaleAuto {
        part: state::ScaleAuto {
            auto: ids.into_iter().map(DisplayId).collect(),
        },
    };
    sim.net(NetEvent::Part(PC, auto(vec![2])));
    sim.menu(Action::NewProfile);
    // The profile's monitors are in the order of the line-up.
    let order: Vec<DisplayId> = sim.m.world().monitors().iter().map(|m| m.0.id).collect();
    let second = order.iter().position(|id| *id == DisplayId(2)).unwrap();
    assert_eq!(sim.m.profiles()[0].auto, [second], "monitor 2");
    // Now monitor 1 in Auto, 2 not: switched back, 1 leaves Auto at its scale, 2
    // takes Auto again.
    sim.net(NetEvent::Part(PC, auto(vec![1])));
    sim.take();
    sim.menu(Action::ApplyProfile(0));
    let switch = requests(&sim.take()).first().expect("asked").0;
    sim.reply(switch, Outcome::Ids { ids: order });
    let asked: Vec<Request> = requests(&sim.take()).into_iter().map(|r| r.1).collect();
    assert!(
        asked.contains(&Request::SetScaleAuto {
            id: DisplayId(1),
            on: false
        }),
        "{asked:?}"
    );
    assert!(
        asked.contains(&Request::SetScaleAuto {
            id: DisplayId(2),
            on: true
        }),
        "{asked:?}"
    );
    // Added in Auto: at the scale its place wants (the headset knows its pixels).
    sim.frames(Duration::from_secs(1));
    sim.take();
    let p = sim
        .m
        .world()
        .shown_if_added((3840, 2160), sim.m.head.1, &sim.m.settings)
        .expect("known");
    let want = crate::autoscale::smallest(p, &[100, 125, 150], crate::autoscale::LINE).unwrap();
    sim.menu(Action::Add {
        width: 3840,
        height: 2160,
    });
    let asked = requests(&sim.take());
    assert!(
        matches!(&asked[..], [(_, Request::AddDisplay { mode })] if mode.scale == want),
        "{want}: {asked:?}"
    );
}

/// A tracked keyboard on the desk, a little to the right.
fn keyboard_on_desk() -> crate::keyboard::Seen {
    crate::keyboard::Seen {
        position: Vec3::new(0.2, -0.5, -0.4),
        rotation: glam::Quat::IDENTITY,
        width: 0.44,
        depth: 0.14,
    }
}

impl Sim {
    /// Frames of 11 ms for `d` with `keyboard` in view (or not).
    fn frames_seeing(&mut self, d: Duration, keyboard: Option<crate::keyboard::Seen>) {
        let end = self.now + d;
        while self.now < end {
            self.now += 11 * MS;
            let input = FrameInput {
                keyboard,
                ..frame(HandState::default())
            };
            self.m.tick(self.now, &input);
            self.m.animate(self.now, &input);
            self.check_view();
        }
    }
}

/// With a keyboard the headset tracks, the window settles over it with the margin, stays
/// there a moment when the keyboard is out of view, then is the hand's window again;
/// nothing is saved for it.
#[test]
fn the_keyboard_window_settles_over_a_tracked_keyboard() {
    use crate::interaction::Scene as _;
    let mut caps = full_caps();
    caps.keyboard_tracking = true;
    let mut sim = Sim::of(caps);
    sim.menu(Action::KeyboardWindow(true));
    assert!(sim.m.keyboard_wanted());
    sim.saves();
    // Nothing seen yet: the window is the one placed by hand.
    sim.frames_seeing(50 * MS, None);
    let by_hand = sim.m.keyboard_window().expect("shown");
    assert!((sim.m.keyboard_depth() - by_hand.width * KEYBOARD_ASPECT).abs() < 1e-6);
    sim.saves();
    // Seen: it goes there.
    let kb = keyboard_on_desk();
    sim.frames_seeing(Duration::from_secs(1), Some(kb));
    let at = sim.m.keyboard_window().expect("shown");
    assert!(at.position.distance(kb.position) < 1e-3);
    assert!((at.width - (0.44 + 2.0 * crate::keyboard::MARGIN)).abs() < 1e-3);
    assert!((sim.m.keyboard_depth() - (0.14 + 2.0 * crate::keyboard::MARGIN)).abs() < 1e-3);
    assert!(sim.m.keyboard_follows());
    // Out of view a moment: it stays.
    sim.frames_seeing(Duration::from_secs(1), None);
    assert_eq!(sim.m.keyboard_window(), Some(at));
    assert!(sim.m.keyboard_follows());
    // Out of view for long: gone, and the window is the hand's again, which a hand
    // can take.
    sim.frames_seeing(Duration::from_secs(3), None);
    assert_eq!(sim.m.keyboard_window(), Some(by_hand));
    assert!(!sim.m.keyboard_follows());
    assert!(sim.m.hold(Thing::Keyboard).is_some(), "taken by a hand");
    sim.m.let_go(sim.now, Thing::Keyboard);
    sim.saves();
    // Hidden by the person: gone.
    sim.menu(Action::KeyboardWindow(false));
    sim.frames_seeing(50 * MS, Some(kb));
    assert!(sim.m.keyboard_window().is_none());
    assert!(!sim.m.keyboard_wanted());
    assert_eq!(sim.saves(), [persist::File::Settings]);
}

/// A headset that does not track keyboards ignores what it is given: the window stays
/// where it was put. A runtime that gives up tracking sends the window back there.
#[test]
fn without_tracking_the_window_stays_where_it_was_put() {
    let mut sim = Sim::of(full_caps());
    sim.menu(Action::KeyboardWindow(true));
    assert!(!sim.m.keyboard_wanted());
    sim.frames_seeing(50 * MS, Some(keyboard_on_desk()));
    let by_hand = sim.m.keyboard_window().expect("shown");
    assert!(by_hand.position.distance(keyboard_on_desk().position) > 0.1);

    let mut caps = full_caps();
    caps.keyboard_tracking = true;
    let mut sim = Sim::of(caps);
    sim.menu(Action::KeyboardWindow(true));
    sim.frames_seeing(Duration::from_secs(1), Some(keyboard_on_desk()));
    let tracked = sim.m.keyboard_window().expect("shown");
    sim.handle(Event::Platform(PlatformEvent::KeyboardTrackingFailed));
    assert!(!sim.m.keyboard_wanted());
    let said = sim.m.toast().map(|t| t.text.clone());
    assert_eq!(
        said.as_deref(),
        Some(sim.m.texts().t("headset-note-keyboard-untracked")),
        "said once"
    );
    sim.frames_seeing(50 * MS, Some(keyboard_on_desk()));
    let after = sim.m.keyboard_window().expect("shown");
    assert_ne!(after, tracked);
    assert!(after.position.distance(keyboard_on_desk().position) > 0.1);
}
