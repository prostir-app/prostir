use std::time::Instant;

use super::*;

fn args(cmd: &Command) -> Vec<String> {
    cmd.get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

/// A remembered mute on the internal input, or a remembered near-zero volume on
/// the stream into it, silenced the microphone on every run.
#[test]
fn the_internal_path_takes_no_remembered_volume() {
    let lb = args(&pw_loopback("mic", "feed"));
    let after = |flag: &str| &lb[lb.iter().position(|a| a == flag).unwrap() + 1];
    assert!(after("--capture-props").contains("state.restore-props=false"));
    // The microphone apps see keeps the user's volume.
    assert!(!after("--playback-props").contains("restore-props"));
    let pa = args(&pacat("feed"));
    assert!(pa.iter().any(|a| a == "--volume=65536"));
    assert!(
        pa.iter()
            .any(|a| a == "--property=state.restore-props=false")
    );
    assert!(pa.iter().any(|a| a == "--device=feed"));
}

/// WirePlumber stores nothing of the headset's sound device (its level is the
/// headset's, a stored one would fight it), nor of a harness's.
#[test]
fn the_sound_device_is_not_remembered() {
    let props = node_props(SINK, "Prostir", "Audio/Sink");
    assert!(props.contains("state.restore-props=false"), "{props}");
    assert!(props.contains("node.name=prostir_headset "), "{props}");
}

/// Nobody takes the sound: the session loop does not wait, at most a pipe's worth
/// waits, and the rest is dropped in whole slices.
#[test]
fn a_full_feed_drops_instead_of_waiting() {
    let mut feed = Feed::spawn(Command::new("sleep").arg("10")).unwrap();
    let slice = vec![1u8; 960];
    let started = Instant::now();
    for _ in 0..100 {
        assert!(feed.write(&slice));
    }
    assert!(started.elapsed() < Duration::from_millis(500));
    let kept = 100 * slice.len() - feed.dropped;
    assert!(kept > 0 && kept <= FEED_QUEUED, "{kept} bytes wait");
    assert_eq!(feed.dropped % slice.len(), 0);
}

/// A burst of pieces that came at once (Wi-Fi held them): three go through, the rest
/// is dropped; a piece larger than the limit still goes into an empty pipe.
#[test]
fn a_burst_keeps_three_pieces_and_a_large_one_goes() {
    for burst in 3..=5 {
        let mut feed = Feed::spawn(Command::new("sleep").arg("10")).unwrap();
        let piece = vec![1u8; 960];
        for _ in 0..burst {
            assert!(feed.write(&piece));
        }
        assert_eq!(
            feed.dropped,
            (burst - 3) * piece.len(),
            "a burst of {burst}"
        );
    }
    let mut feed = Feed::spawn(Command::new("sleep").arg("10")).unwrap();
    assert!(feed.write(&[1u8; FEED_QUEUED + 960]));
    assert_eq!(feed.dropped, 0, "into an empty pipe");
}

#[test]
fn a_stopped_player_is_noticed() {
    let mut feed = Feed::spawn(&mut Command::new("true")).unwrap();
    let _ = feed.pacat.wait();
    // Another test's child forked at the same moment holds the pipe's other end
    // until it runs its program: give it that moment.
    let until = Instant::now() + Duration::from_secs(2);
    while feed.write(&[0; 960]) {
        assert!(Instant::now() < until, "the stopped player was not noticed");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// `pw-cli info` of a node: its number, serial and name.
#[test]
fn a_node_is_read_from_pw_cli() {
    let text = "\tid: 52\n\tpermissions: rwxm\n\ttype: PipeWire:Interface:Node/3\n\
                *\tproperties:\n*\t\tobject.serial = \"57940\"\n\
                *\t\tnode.name = \"prostir_headset\"\n*\t\tobject.id = \"52\"\n";
    assert_eq!(
        parse_info(text),
        Some(NodeInfo {
            id: 52,
            serial: Some(57940),
            name: Some("prostir_headset".into()),
        })
    );
    assert_eq!(parse_info("Error: \"unknown global 'x'\"\n"), None);
}

/// A device let go late (the sound system was slow, and the sound thread went on
/// without waiting) leaves alone the one made after it under the same name, though
/// PipeWire may have given the new one the old one's number. By hand, on this PC's
/// PipeWire: `cargo test -p prostir -- --ignored a_late_drop`. It makes only nodes of
/// its own (`prostir_test_late_drop`) and removes them.
#[test]
#[ignore = "by hand: makes PipeWire nodes of its own"]
fn a_late_drop_leaves_the_new_device_alone() {
    const NAME: &str = "prostir_test_late_drop";
    let old = VirtualNode::create(NAME, "Prostir test", "Audio/Sink").unwrap();
    // Made while the old one is still being let go: it removes the old node as one
    // left over, and may well get its number.
    let new = VirtualNode::create(NAME, "Prostir test", "Audio/Sink").unwrap();
    eprintln!(
        "old #{} {:?}, new #{} {:?}",
        old.id, old.serial, new.id, new.serial
    );
    let new_id = new.id;
    drop(old);
    let there = node_id(NAME);
    drop(new);
    let gone = node_id(NAME);
    assert_eq!(there, Some(new_id), "the late drop removed the new device");
    assert_eq!(gone, None, "the new device goes with its own drop");
}
