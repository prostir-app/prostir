use super::parts::within_limit;
use super::physical::{least_recently_seen, match_physical, name_apart, unique_edid};
use super::*;

/// Two displays of the PC of one model are told apart by their connectors; one alone
/// goes by its model, one known by its connector by that.
#[test]
fn displays_of_one_model_are_told_apart() {
    assert_eq!(
        name_apart("DELL U2720Q", "DP-1", true),
        "DELL U2720Q (DP-1)"
    );
    assert_eq!(name_apart("DELL U2720Q", "DP-1", false), "DELL U2720Q");
    assert_eq!(name_apart("HDMI-A-1", "HDMI-A-1", true), "HDMI-A-1");
}

/// The streams go in the codec chosen only where every headset decodes it, and in
/// H.264 otherwise; without a headset, as chosen.
#[test]
fn the_codec_is_one_every_headset_decodes() {
    let decoder = |codec| state::Decoder {
        codec,
        max_w: 4096,
        max_h: 4096,
    };
    let both = state::HeadsetCaps {
        decoders: vec![decoder(0), decoder(1)],
        ..state::HeadsetCaps::default()
    };
    let h264 = state::HeadsetCaps {
        decoders: vec![decoder(0)],
        ..state::HeadsetCaps::default()
    };
    assert_eq!(Displays::codec_for(Codec::H265, &[]), Codec::H265);
    assert_eq!(
        Displays::codec_for(Codec::H265, std::slice::from_ref(&both)),
        Codec::H265
    );
    assert_eq!(
        Displays::codec_for(Codec::H265, &[both.clone(), h264.clone()]),
        Codec::H264
    );
    assert_eq!(Displays::codec_for(Codec::H264, &[both, h264]), Codec::H264);
}

/// The rows go bottom first only when every headset with a session asks for it.
#[test]
fn rows_bottom_first_only_for_all() {
    let rows_up = state::HeadsetCaps {
        features: vec![state::caps::headset::ROWS_UP],
        ..state::HeadsetCaps::default()
    };
    let plain = state::HeadsetCaps::default();
    assert!(Displays::rows_up_for(std::slice::from_ref(&rows_up)));
    assert!(!Displays::rows_up_for(&[rows_up, plain.clone()]));
    assert!(!Displays::rows_up_for(&[plain]));
    assert!(!Displays::rows_up_for(&[]));
}

/// A display of the PC is known by its EDID wherever it is plugged in; one that says
/// nothing, or the same as another, by its connector; another display in the same
/// connector is another display.
#[test]
fn displays_of_the_pc_are_known_by_edid_else_by_connector() {
    let id = ScreenId::saved;
    let display = |connector: &str, edid: Option<&str>| PhysicalDisplay {
        key: OutputKey::new(connector),
        label: connector.into(),
        edid: edid.map(Into::into),
        builtin: false,
        size: None,
        default: false,
    };
    let key = |edid: Option<&str>, connector: &str| PhysicalKey {
        edid: edid.map(Into::into),
        connector: connector.into(),
    };
    let known = [
        (id(100), key(Some("BOE 1"), "eDP-1")),
        (id(101), key(Some("DEL 7"), "HDMI-A-1")),
        (id(102), key(None, "DP-2")),
    ];
    // The Dell moved to DP-1; a display without EDID on DP-2; another one in
    // HDMI-A-1.
    let seen = [
        display("eDP-1", Some("BOE 1")),
        display("DP-1", Some("DEL 7")),
        display("DP-2", None),
        display("HDMI-A-1", Some("LG 3")),
    ];
    assert_eq!(
        match_physical(&known, &seen),
        [Some(id(100)), Some(id(101)), Some(id(102)), None]
    );
    // Two of the same model and serial: by connector, and none of them is the one
    // known by that EDID.
    let twins = [
        display("DP-1", Some("DEL 7")),
        display("DP-2", Some("DEL 7")),
    ];
    assert_eq!(match_physical(&known, &twins), [None, Some(id(102))]);
    // Each known display matches once.
    let two_on_dp2 = [display("DP-2", None), display("DP-2", None)];
    assert_eq!(match_physical(&known, &two_on_dp2), [Some(id(102)), None]);
    // The one known by its connector says its EDID now: still that one (and
    // `sync_physical` keeps the EDID from now on).
    let says = [display("DP-2", Some("SAM 9"))];
    assert_eq!(match_physical(&known, &says), [Some(id(102))]);
    assert_eq!(unique_edid(&says[0], &says), Some("SAM 9"));
}

/// Of the displays of the PC not connected, the 8 seen last stay; the others
/// (the oldest, and those never dated) are forgotten.
#[test]
fn displays_of_the_pc_long_absent_are_forgotten() {
    let id = ScreenId::saved;
    let absent: Vec<(ScreenId, u64)> = (0..12).map(|n| (id(100 + n), 1000 + n as u64)).collect();
    let mut gone = least_recently_seen(absent.clone(), ABSENT_KEPT);
    gone.sort();
    assert_eq!(gone, [id(100), id(101), id(102), id(103)]);
    assert!(least_recently_seen(absent[..8].to_vec(), ABSENT_KEPT).is_empty());
    let undated = vec![(id(1), 0), (id(2), 5)];
    assert_eq!(least_recently_seen(undated, 1), [id(1)]);
}

/// `Displays` never holds more than it may: absent displays of the PC are left out
/// first, so the part always goes out (it failed to encode before, ending every
/// session).
#[test]
fn the_list_of_displays_always_fits() {
    use prostir_proto::state::{Display, DisplayKind, DisplayState, Mode};
    let display = |n: u32, state: DisplayState| Display {
        id: state::DisplayId(n),
        kind: DisplayKind::Virtual,
        name: format!("Prostir-{n}"),
        mode: Mode::new(640, 360),
        desk: None,
        state,
    };
    let mut list: Vec<Display> = (0..10).map(|n| display(n, DisplayState::Running)).collect();
    list.extend((10..40).map(|n| display(n, DisplayState::Absent)));
    let kept = within_limit(list);
    assert_eq!(kept.len(), state::MAX_DISPLAYS);
    assert_eq!(
        kept.iter()
            .filter(|d| d.state == DisplayState::Running)
            .count(),
        10,
        "every running one stays"
    );
    let part = state::Displays {
        displays: kept,
        primary: None,
    };
    assert!(prostir_proto::wire::encode(&part).is_ok());
    let many: Vec<Display> = (0..40).map(|n| display(n, DisplayState::Running)).collect();
    assert_eq!(within_limit(many).len(), state::MAX_DISPLAYS);
}

/// Every monitor is in the rows once, in the first place listed: an order made
/// from them never repeats one (or grows past what a message holds).
#[test]
fn rows_list_each_monitor_once() {
    const MIRROR: u8 = 100;
    let at = |id, center| desk::Entry {
        id,
        center: Some(center),
        middle: None,
    };
    let rows = vec![
        vec![at(0, -100), at(0, 0), at(MIRROR, 50)],
        vec![at(1, 10), at(0, 20), at(9, 30)],
    ];
    let monitor = |id| id < 3;
    let kept = rows_in_step(&rows, &[2, 0, 1, 2], monitor, |id| id == MIRROR);
    let ids: Vec<Vec<u8>> = kept
        .iter()
        .map(|r| r.iter().map(|e| e.id).collect())
        .collect();
    assert_eq!(ids, vec![vec![0, MIRROR], vec![1, 2]]);
    assert_eq!(kept[0][0], at(0, -100), "the first place counts");
    let many = vec![vec![at(0, 0); 16]];
    let kept = rows_in_step(&many, &[0, 1], monitor, |_| false);
    assert_eq!(kept[0].iter().map(|e| e.id).collect::<Vec<_>>(), vec![0, 1]);
}

/// The headset's choice in Auto where the desktop does not give that size the scale:
/// the next one up it gives, else the next one down; as asked where it gives it.
#[test]
fn auto_takes_the_nearest_scale_the_desktop_gives() {
    use super::arrange::nearest_scale;
    let offered = [100, 125, 150, 175, 200];
    // Hyprland and 2560x1440: whole logical pixels only.
    let hyprland = |p: u16| matches!(p, 100 | 125 | 200);
    assert_eq!(nearest_scale(150, &offered, hyprland), Some(200));
    assert_eq!(nearest_scale(175, &offered, hyprland), Some(200));
    assert_eq!(nearest_scale(125, &offered, hyprland), Some(125));
    let up_to_150 = |p: u16| p <= 150;
    assert_eq!(nearest_scale(200, &offered, up_to_150), Some(150));
    assert_eq!(nearest_scale(150, &offered, |_| false), None);
}
