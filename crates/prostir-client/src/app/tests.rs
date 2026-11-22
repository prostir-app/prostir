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
    Display, DisplayKind, DisplayState, Displays, ErrorCode, Mode, permission,
};

use super::*;
use crate::connecting::Ended;
use crate::ids::Thing;
use crate::interaction::{HandState, Hands, Menu, Menus};
use crate::layout::Desk;
use crate::session::{Disconnect, StepId};
use crate::store::Store as _;
use crate::ui::UiState;

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
    fn new() -> Self {
        let now = Instant::now();
        let m = Model::new(Start {
            now,
            settings: Settings::default(),
            desk: Desk::default(),
            keyboard: None,
            profiles: Default::default(),
            lang: LangState::new(crate::lang::builtin("en"), Vec::new()),
            system_lang: None,
            battery: Some((80, false)),
            model: "Test headset".into(),
            can: Can {
                room: false,
                keyboard_window: false,
                panorama: true,
                curve: true,
                custom_panorama: false,
                update_from: crate::store::NoStore.update_hint(),
            },
            mic_permission: permission::UNKNOWN,
            only: Some(addr()),
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
        let step = self
            .take()
            .into_iter()
            .find_map(|e| match e {
                Effect::Net(NetCmd::Dial { step, .. }) => Some(step),
                _ => None,
            })
            .expect("the round dials the only address");
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
            self.handle(Event::StreamStarted {
                id: k(*n),
                size: (2560, 1440),
            });
        }
        self.frames(Duration::from_secs(1));
        self.take();
        self.saves();
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
            Effect::Send(pc, msg) if *pc == PC => Some(msg),
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
    let before = sim.m.world().placement(k(1)).copied();
    sim.command(7, Command::Distance { steps: -2 });
    let fx = sim.take();
    assert_eq!(reply_of(&fx, 7), Some(&Outcome::Done));
    assert_ne!(sim.m.world().placement(k(1)).copied(), before, "it moved");

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
    // The system menu.
    hands.cancel_all(&mut sim.m);
    assert!(!sim.m.world().holding());
    assert_eq!(sim.saves(), [persist::File::Desk]);
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

/// A message to a PC the shell has no session with comes back as failed: the menu
/// says it, without a session there is nothing to send to.
#[test]
fn a_message_not_sent_is_said() {
    let mut sim = Sim::new();
    sim.handle(Event::SendFailed { pc: PC, why: None });
    let shows = crate::view::build(&sim.m).error;
    assert_eq!(
        shows.as_deref(),
        Some(sim.m.texts().t("error-not-connected"))
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
