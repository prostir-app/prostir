//! The PC's own displays as the menu's one switch sees them: which display it shows or
//! hides, and what it is called. A display that is not connected now (a closed
//! laptop's panel, a monitor whose cable is out) is never the one: showing it would
//! show nothing.

#![forbid(unsafe_code)]

use prostir_proto::state::{Display, DisplayId, DisplayKind, DisplayState, Displays};

use crate::lang::Texts;

/// Connected now, so showing it shows something.
fn connected(d: &Display) -> bool {
    d.is_physical() && d.state != DisplayState::Absent
}

/// The display of the PC the headset shows for the switch's sake: a shown one that is
/// connected, else a shown one that is not (its place is kept while the lid is
/// closed).
pub fn mirror(displays: &Displays) -> Option<DisplayId> {
    let shown = || displays.displays.iter().filter(|d| d.is_shown_physical());
    shown()
        .find(|d| connected(d))
        .or_else(|| shown().next())
        .map(|d| d.id)
}

/// The display of the PC the switch shows or hides, among those connected now: one
/// shown already, else the PC's primary display, else an external monitor before
/// the laptop's own panel, else the first in the PC's list. None while no display of
/// the PC is connected.
pub fn switch(displays: &Displays) -> Option<&Display> {
    let here = || displays.displays.iter().filter(|d| connected(d));
    here()
        .find(|d| d.is_shown_physical())
        .or_else(|| here().find(|d| displays.primary == Some(d.id)))
        .or_else(|| here().find(|d| matches!(d.kind, DisplayKind::Physical { builtin: false, .. })))
        .or_else(|| here().next())
}

/// What the person calls display `d` of the PC: the laptop's own panel is the laptop
/// screen, another one goes by the name the PC gives it (its maker and model, as
/// `DELL U2720Q`, from PCs of step 7.9 on), else by its connector (`HDMI-A-1`).
pub fn label(d: &Display, l: &Texts) -> String {
    match &d.kind {
        DisplayKind::Physical { builtin: true, .. } => l.t("laptop-screen").to_owned(),
        DisplayKind::Physical { connector, .. } if !d.name.is_empty() && d.name != *connector => {
            d.name.clone()
        }
        DisplayKind::Physical { connector, .. } if !connector.is_empty() => {
            l.f("pc-display-named", &[("name", connector.into())])
        }
        _ if !d.name.is_empty() => l.f("pc-display-named", &[("name", (&d.name).into())]),
        _ => l.t("pc-display").to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prostir_proto::state::Mode;

    fn physical(id: u32, connector: &str, builtin: bool, shown: bool, here: bool) -> Display {
        Display {
            id: DisplayId(id),
            kind: DisplayKind::Physical {
                builtin,
                connector: connector.into(),
                shown,
            },
            name: connector.into(),
            mode: Mode::new(1920, 1080),
            desk: None,
            state: if here {
                DisplayState::Running
            } else {
                DisplayState::Absent
            },
        }
    }

    fn monitor(id: u32) -> Display {
        Display {
            id: DisplayId(id),
            kind: DisplayKind::Virtual,
            name: format!("Prostir-{id}"),
            mode: Mode::new(2560, 1440),
            desk: None,
            state: DisplayState::Running,
        }
    }

    fn list(displays: Vec<Display>, primary: Option<u32>) -> Displays {
        Displays {
            displays,
            primary: primary.map(DisplayId),
        }
    }

    /// The lid closed (the laptop's panel absent, still marked shown from before) and
    /// one external monitor: the switch is the monitor's, shown or hidden, and never
    /// the absent panel's.
    #[test]
    fn a_closed_laptop_is_never_the_switch() {
        let laptop = || physical(100, "eDP-1", true, true, false);
        let hidden = list(
            vec![
                monitor(1),
                laptop(),
                physical(117, "HDMI-A-1", false, false, true),
            ],
            Some(1),
        );
        assert_eq!(switch(&hidden).map(|d| d.id.0), Some(117));
        assert!(!switch(&hidden).unwrap().is_shown_physical(), "said hidden");
        let shown = list(
            vec![
                monitor(1),
                laptop(),
                physical(117, "HDMI-A-1", false, true, true),
            ],
            Some(1),
        );
        assert_eq!(switch(&shown).map(|d| d.id.0), Some(117));
        assert_eq!(
            mirror(&shown),
            Some(DisplayId(117)),
            "the connected one first"
        );
        assert_eq!(mirror(&hidden), Some(DisplayId(100)), "its place kept");
        assert_eq!(
            label(switch(&shown).unwrap(), &crate::lang::builtin("uk")),
            "Екран HDMI-A-1",
            "named"
        );
        // A PC that says the display's maker and model: by them.
        let mut dell = physical(117, "HDMI-A-1", false, true, true);
        dell.name = "DELL U2720Q".into();
        assert_eq!(label(&dell, &crate::lang::builtin("uk")), "DELL U2720Q");
        let none = list(vec![monitor(1), laptop()], Some(1));
        assert_eq!(switch(&none).map(|d| d.id.0), None);
    }

    /// Among displays connected and hidden: the PC's primary, else an external one
    /// before the laptop's panel, else the first.
    #[test]
    fn the_primary_then_an_external_display() {
        let laptop = physical(100, "eDP-1", true, false, true);
        let hdmi = physical(117, "HDMI-A-1", false, false, true);
        let dp = physical(118, "DP-2", false, false, true);
        let both = |primary| list(vec![laptop.clone(), hdmi.clone(), dp.clone()], primary);
        assert_eq!(switch(&both(Some(100))).map(|d| d.id.0), Some(100));
        assert_eq!(switch(&both(Some(118))).map(|d| d.id.0), Some(118));
        assert_eq!(switch(&both(None)).map(|d| d.id.0), Some(117));
        let only = list(vec![laptop.clone()], None);
        assert_eq!(switch(&only).map(|d| d.id.0), Some(100));
        assert_eq!(label(&laptop, &crate::lang::builtin("en")), "Laptop screen");
    }
}
