//! The settings window's client (`prostir window`'s D-Bus client, the code the
//! window runs) against the real service on a session bus of the test's own: the
//! state, its changes, and the requests the window sends, with the answers it relies
//! on (the revision of the state with the change). Skipped, saying so, without
//! `dbus-daemon`.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use prostir_api::{DisplayKind, DisplayState, ThemeMode};
use prostir_window::api::{Change, Client, ClientError, Request};
use prostir_window::dbus::DbusClient;

use crate::pc::Options;
use crate::{SOON, START, session};

/// Waits for a state (from the client's changes or asked for) that `done` accepts.
fn wait_state(
    client: &DbusClient,
    changes: &mpsc::Receiver<Change>,
    within: Duration,
    what: &str,
    done: impl Fn(&prostir_api::State) -> bool,
) -> (u32, prostir_api::State) {
    let started = Instant::now();
    loop {
        let snap = client.get_state().expect("GetState");
        if done(&snap.state) {
            return (snap.revision, *snap.state);
        }
        assert!(
            started.elapsed() < within,
            "{what}: {:?}",
            snap.state.displays
        );
        let _ = changes.recv_timeout(Duration::from_millis(100));
    }
}

#[test]
fn the_window_s_client_drives_the_service() {
    let (_dir, pc, _keys, _headset) = session("window-client", &Options::default());
    let Some(bus) = &pc.bus else {
        return;
    };
    let client = DbusClient::new(bus.connect()).unwrap();
    let (tx, changes) = mpsc::channel();
    client
        .watch(Box::new(move |c| {
            let _ = tx.send(c);
        }))
        .unwrap();
    assert_eq!(client.present(), Some(true));

    // The two monitors of the session, on the desktop.
    let (_, state) = wait_state(&client, &changes, SOON, "two monitors placed", |s| {
        s.displays
            .iter()
            .filter(|d| d.kind == DisplayKind::Virtual && d.desk.is_some())
            .count()
            == 2
    });
    assert!(state.connected().is_some_and(|(_, s)| s.main));

    // A third monitor: its id comes with the answer, and the state of the answer's
    // revision has it already.
    let size = state.limits.sizes[0];
    let answer = client
        .call(&Request::AddMonitor {
            w: size[0],
            h: size[1],
            scale: 100,
        })
        .unwrap();
    let id = answer.id.expect("the new monitor's id");
    let snap = client.get_state().unwrap();
    assert!(snap.revision >= answer.revision);
    assert!(snap.state.display(id).is_some(), "in the answer's state");
    let (_, state) = wait_state(&client, &changes, START, "the third monitor running", |s| {
        s.display(id)
            .is_some_and(|d| d.state == DisplayState::Running && d.desk.is_some())
    });

    // Placed above the others, as the map does it.
    let top = state
        .displays
        .iter()
        .filter_map(|d| d.desk)
        .map(|r| r.y)
        .min()
        .unwrap();
    let mine = state.display(id).unwrap().desk.unwrap();
    let answer = client
        .call(&Request::Place {
            id,
            x: 0,
            y: top - 2 * mine.h as i32,
        })
        .unwrap();
    assert!(answer.revision > 0);
    wait_state(&client, &changes, SOON, "placed above the others", |s| {
        let me = s.display(id).and_then(|d| d.desk);
        let others = s
            .displays
            .iter()
            .filter(|d| d.id != id)
            .filter_map(|d| d.desk);
        me.is_some_and(|me| others.clone().all(|o| me.y + me.h as i32 <= o.y))
    });

    // Another scale, given in place under the same id.
    let scale = *state
        .limits
        .scales
        .iter()
        .find(|s| **s != 100)
        .expect("another scale");
    client.call(&Request::SetScale { id, scale }).unwrap();
    wait_state(&client, &changes, START, "the new scale running", |s| {
        s.display(id)
            .is_some_and(|d| d.mode.scale == scale && d.state == DisplayState::Running)
    });

    // The window's look, kept by the service.
    client
        .call(&Request::WindowLook {
            theme: ThemeMode::Dark,
            palette: "graphite".into(),
            language: Some("en".into()),
        })
        .unwrap();
    let window = client.get_state().unwrap().state.window;
    assert_eq!(
        (
            window.theme,
            window.palette.as_str(),
            window.language.as_deref()
        ),
        (ThemeMode::Dark, "graphite", Some("en"))
    );

    // The volume, while the headset shares it.
    assert!(client.get_state().unwrap().state.sound.volume.is_some());
    client.call(&Request::Volume { level: 30 }).unwrap();
    // What the service does not do reads as Unsupported, which the window shows as a
    // disabled control rather than a failure.
    let e = client.call(&Request::SetMic { on: false }).unwrap_err();
    assert!(e.unsupported(), "{e:?}");
    // An error of its own kind, by name.
    assert!(matches!(
        client.call(&Request::RemoveMonitor { id: 9999 }),
        Err(ClientError::Refused {
            code: prostir_api::ApiError::UnknownDisplay,
            ..
        })
    ));

    // Changes came as signals, not only by asking.
    let mut signalled = false;
    while let Ok(c) = changes.try_recv() {
        signalled |= matches!(c, Change::State(_));
    }
    assert!(signalled, "StateChanged reached the client");

    // Prostir quits: the client hears it leave.
    client.call(&Request::Quit).unwrap();
    let started = Instant::now();
    loop {
        match changes.recv_timeout(Duration::from_millis(200)) {
            Ok(Change::Vanished) => break,
            _ => assert!(started.elapsed() < SOON, "no Vanished after quit"),
        }
    }
    assert_eq!(client.get_state(), Err(ClientError::NotRunning));
}
