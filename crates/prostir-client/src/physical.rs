//! The PC's own displays as the menu shows them: each with its switch, what it is
//! called, and whether it is connected now (a closed laptop's panel, a monitor whose
//! cable is out, shows nothing and counts for no limit).

#![forbid(unsafe_code)]

use prostir_proto::state::{DeskRect, Display, DisplayKind, DisplayState, Displays};

use crate::ids::{DisplayKey, PcId};
use crate::lang::Texts;
use crate::ui::PhysicalEntry;

/// Connected now, so showing it shows something.
pub fn connected(d: &Display) -> bool {
    d.is_physical() && d.state != DisplayState::Absent
}

/// Where a display is on the PC's desktop, as the arrangement takes it: x, y, width,
/// height.
pub fn desk_rect(r: DeskRect) -> [i32; 4] {
    let size = |v: u32| i32::try_from(v).unwrap_or(i32::MAX);
    [r.x, r.y, size(r.w), size(r.h)]
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

/// The connector of display `d`, shown smaller under its name, where the name is not
/// already the connector's.
fn connector_under(d: &Display, name: &str) -> Option<String> {
    match &d.kind {
        DisplayKind::Physical { connector, .. }
            if !connector.is_empty() && !name.contains(connector.as_str()) =>
        {
            Some(connector.clone())
        }
        _ => None,
    }
}

/// The displays of PC `pc` itself as the menu shows their switches, those connected
/// first, each part in its list's order: each shown as `asked` says while the PC has
/// not answered (the switch moves at once), else as the PC says.
pub fn entries(
    pc: PcId,
    displays: &Displays,
    asked: &[(DisplayKey, bool)],
    texts: &Texts,
) -> Vec<PhysicalEntry> {
    let mut all: Vec<PhysicalEntry> = displays
        .displays
        .iter()
        .filter(|d| d.is_physical())
        .map(|d| {
            let id = DisplayKey::new(pc, d.id);
            let name = label(d, texts);
            PhysicalEntry {
                id,
                connector: connector_under(d, &name),
                name,
                shown: asked
                    .iter()
                    .rev()
                    .find(|(k, _)| *k == id)
                    .map_or_else(|| d.is_shown_physical(), |(_, shown)| *shown),
                connected: connected(d),
            }
        })
        .collect();
    all.sort_by_key(|e| !e.connected);
    all
}

#[cfg(test)]
mod tests {
    use super::*;
    use prostir_proto::state::{DisplayId, Mode};

    const PC: PcId = PcId([4; 8]);

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

    /// Every display of the PC itself, those connected first, with its switch as the
    /// PC says or, asked and not answered yet, as asked; a closed laptop's panel is
    /// there, not connected, last; monitors made for the headset are not.
    #[test]
    fn every_display_of_the_pc_with_its_switch() {
        let uk = crate::lang::builtin("uk");
        let mut dell = physical(117, "HDMI-A-1", false, false, true);
        dell.name = "DELL U2720Q".into();
        let list = Displays {
            displays: vec![
                monitor(1),
                physical(100, "eDP-1", true, true, false),
                dell,
                physical(118, "DP-2", false, true, true),
            ],
            primary: Some(DisplayId(1)),
        };
        let all = entries(PC, &list, &[], &uk);
        let seen: Vec<(u32, &str, Option<&str>, bool, bool)> = all
            .iter()
            .map(|e| {
                (
                    e.id.id.0,
                    e.name.as_str(),
                    e.connector.as_deref(),
                    e.shown,
                    e.connected,
                )
            })
            .collect();
        assert_eq!(
            seen,
            [
                (117, "DELL U2720Q", Some("HDMI-A-1"), false, true),
                (118, "Екран DP-2", None, true, true),
                (100, "Екран ноутбука", Some("eDP-1"), true, false),
            ],
            "those connected first"
        );
        let k = DisplayKey::new(PC, DisplayId(117));
        let asked = entries(PC, &list, &[(k, false), (k, true)], &uk);
        assert!(asked[0].shown, "as asked last");
        assert_eq!(label(&monitor(1), &uk), "Екран Prostir-1");
        assert_eq!(
            label(
                &physical(100, "eDP-1", true, true, true),
                &crate::lang::builtin("en")
            ),
            "Laptop screen"
        );
    }
}
