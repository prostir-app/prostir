//! The budget's rule on lists made by hand: what goes first when not all fit, and
//! that the rest keep their order. The plan of a real model (the order, a drag drawn
//! in the same frame, the person told) is in `app`'s tests.

use glam::{Quat, Vec3};
use prostir_proto::state::DisplayId;

use super::*;
use crate::ids::PcId;

fn k(n: u32) -> DisplayKey {
    DisplayKey::new(PcId([7; 8]), DisplayId(n))
}

fn quad() -> Quad {
    Quad {
        at: Placement {
            position: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            width: 1.0,
        },
        height: 0.5,
    }
}

fn monitor(n: u32) -> Layer {
    Layer::Monitor(Monitor {
        id: k(n),
        quad: quad(),
        curve: None,
        bright: 1.0,
        alpha: 1.0,
    })
}

/// A frame as the plan makes it: the background, monitors 1 to 4 far to near (4 m to
/// 1 m), recovering words over monitor 2, the button, the menu, the toast, the hands
/// and two dots.
fn frame() -> Plan {
    let mut p = Plan::new();
    fill(&mut p);
    p
}

/// The layers of [`frame`] into `p`.
fn fill(p: &mut Plan) {
    p.push(Layer::Background, Rank::Room(1));
    for n in 1..=4 {
        p.push(monitor(n), Rank::Monitor(nearness(5.0 - n as f32)));
    }
    p.push(
        Layer::Trouble {
            id: k(2),
            quad: quad(),
        },
        Rank::Over(nearness(3.0)),
    );
    p.push(Layer::MenuButton(quad()), Rank::Own(1));
    p.push(Layer::Menu(quad()), Rank::Own(3));
    p.push(Layer::Toast(quad()), Rank::Said(2));
    p.push(Layer::Hands, Rank::Own(4));
    p.push(Layer::Cursor(quad()), Rank::Own(0));
    p.push(Layer::Cursor(quad()), Rank::Own(0));
}

/// What the layers are, in order, by a short name each.
fn names(p: &Plan) -> Vec<String> {
    p.layers()
        .iter()
        .map(|l| match l {
            Layer::Background => "bg".into(),
            Layer::KeyboardWindow => "keys".into(),
            Layer::Monitor(m) => format!("m{}", m.id.id.0),
            Layer::Trouble { id, .. } => format!("t{}", id.id.0),
            Layer::Placeholder(_) => "adding".into(),
            Layer::MenuButton(_) => "button".into(),
            Layer::Menu(_) => "menu".into(),
            Layer::Toast(_) => "toast".into(),
            Layer::Attach(_) => "attach".into(),
            Layer::Palm(_) => "palm".into(),
            Layer::Hands => "hands".into(),
            Layer::Cursor(_) => "dot".into(),
        })
        .collect()
}

#[test]
fn all_fit_as_they_are() {
    let mut p = frame();
    p.budget(16);
    assert_eq!(p.wanted(), 12);
    assert_eq!(p.hidden(), 0);
    assert_eq!(
        names(&p),
        [
            "bg", "m1", "m2", "m3", "m4", "t2", "button", "menu", "toast", "hands", "dot", "dot"
        ]
    );
    assert_eq!(p.news(), None, "nothing hidden, nothing to say");
}

/// One short: the words over a monitor that still shows go first.
#[test]
fn the_words_over_a_picture_go_first() {
    let mut p = frame();
    p.budget(11);
    assert_eq!(p.hidden(), 0);
    assert_eq!(
        names(&p),
        [
            "bg", "m1", "m2", "m3", "m4", "button", "menu", "toast", "hands", "dot", "dot"
        ]
    );
}

/// Then the monitors, the farthest first, and each one hidden is counted.
#[test]
fn the_farthest_monitors_go_next() {
    let mut p = frame();
    p.budget(9);
    assert_eq!(p.hidden(), 2);
    assert_eq!(
        names(&p),
        [
            "bg", "m3", "m4", "button", "menu", "toast", "hands", "dot", "dot"
        ]
    );
    assert_eq!(p.news(), Some(2));
    assert_eq!(p.news(), None, "said once");
}

/// With no room for a monitor at all, the background, then the toast; the person's own
/// (the dots, the button, the menu, the hands) stay to the last.
#[test]
fn the_persons_own_stay_to_the_last() {
    let mut p = frame();
    p.budget(6);
    assert_eq!(p.hidden(), 4);
    assert_eq!(
        names(&p),
        ["button", "menu", "toast", "hands", "dot", "dot"]
    );
    let mut p = frame();
    p.budget(5);
    assert_eq!(names(&p), ["button", "menu", "hands", "dot", "dot"]);
    let mut p = frame();
    p.budget(4);
    assert_eq!(
        names(&p),
        ["button", "menu", "hands", "dot"],
        "the last dot drawn stays"
    );
    let mut p = frame();
    p.budget(2);
    assert_eq!(names(&p), ["menu", "hands"]);
}

/// The reason in the place of a monitor that cannot be shown counts as that monitor:
/// by its distance, among the monitors.
#[test]
fn a_reason_in_a_monitors_place_counts_as_a_monitor() {
    let mut p = Plan::new();
    p.push(monitor(1), Rank::Monitor(nearness(3.0)));
    p.push(
        Layer::Trouble {
            id: k(2),
            quad: quad(),
        },
        Rank::Monitor(nearness(1.0)),
    );
    p.push(monitor(3), Rank::Monitor(nearness(2.0)));
    p.push(Layer::Hands, Rank::Own(4));
    p.budget(3);
    assert_eq!(names(&p), ["t2", "m3", "hands"]);
    assert_eq!(p.hidden(), 1);
}

/// The monitor in a hand (drawn last, at -1 m) is the nearest of all.
#[test]
fn the_monitor_in_a_hand_stays() {
    let mut p = Plan::new();
    p.push(monitor(1), Rank::Monitor(nearness(1.0)));
    p.push(monitor(2), Rank::Monitor(nearness(-1.0)));
    p.budget(1);
    assert_eq!(names(&p), ["m2"]);
}

/// The next frame starts afresh, and says when the number changes, also back to none.
#[test]
fn each_frame_is_planned_anew() {
    let mut p = frame();
    p.budget(9);
    assert_eq!(p.news(), Some(2));
    p.clear();
    fill(&mut p);
    p.budget(8);
    assert_eq!(p.news(), Some(3), "one more");
    p.clear();
    fill(&mut p);
    p.budget(16);
    assert_eq!(p.layers().len(), 12);
    assert_eq!(p.news(), Some(0), "every monitor shows again");
    assert_eq!(p.news(), None);
}
