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
use crate::platform::store::Store as _;
use crate::session::{Disconnect, StepId};
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
        sim.handle(Event::StreamStarted { id: k(n), size });
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
