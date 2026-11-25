//! The headset's `Gaze` (step 7.12): the PC names feature 14 (it takes the gaze), and
//! what the headset says of the monitor it looks at reaches the PC's log, which is all
//! the PC does with it for now.

use prostir_proto::ClientMsg;
use prostir_proto::state::caps;
use prostir_testkit::FakeHeadset;

use crate::pc::Options;
use crate::{SOON, session};

/// The PC says it takes the gaze, and a `Gaze` of the headset comes to its log, for a
/// monitor and for none.
#[test]
fn the_pc_takes_the_gaze_and_logs_it() {
    let (_dir, mut pc, _keys, mut headset) = session("gaze", &Options::default());
    let pc_caps = headset.view().pc.caps.clone().unwrap();
    assert!(pc_caps.has(caps::pc::GAZE), "{pc_caps:?}");
    // The fake headset says it can tell where the user looks.
    let said = FakeHeadset::state().caps.unwrap();
    assert!(said.has(caps::headset::GAZE));

    let id = headset.view().order()[0];
    headset
        .send(&ClientMsg::Gaze {
            display: Some(id),
            u: 1234,
            v: 54321,
        })
        .unwrap();
    pc.wait_log(
        SOON,
        &format!("gaze: the headset looks at display {} at 1234,54321", id.0),
    );
    headset
        .send(&ClientMsg::Gaze {
            display: None,
            u: 0,
            v: 0,
        })
        .unwrap();
    pc.wait_log(SOON, "gaze: the headset looks at none of the displays");
    pc.quit();
}
