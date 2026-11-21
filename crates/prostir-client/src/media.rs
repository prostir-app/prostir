//! The network's sessions' sound and microphone on the headset: AAudio (see
//! [`crate::audio`], [`crate::mic`]) behind the network code's [`Media`].

use std::sync::Arc;

use crate::audio::{Gains, Player};
use crate::mic::{Mic, Recorder};
use crate::net::{Feedback, Media, MediaSource};

/// What sessions play the PC's sound and record the microphone with: made once for
/// each start of the app and shared with the frame loop.
pub struct Sound {
    /// Where the sound comes from (the frame loop places it).
    pub gains: Arc<Gains>,
    pub mic: Arc<Mic>,
}

impl MediaSource for Sound {
    fn open(&self) -> Box<dyn Media> {
        Box::new(SessionSound {
            gains: self.gains.clone(),
            mic: self.mic.clone(),
            player: None,
            recorder: None,
        })
    }

    fn raise_priority(&self) {
        crate::xrutil::raise_thread_priority(crate::xrutil::Priority::Video);
    }
}

/// One session's: a player started on the first slice of sound, and a recorder while
/// the microphone records.
struct SessionSound {
    gains: Arc<Gains>,
    mic: Arc<Mic>,
    player: Option<Player>,
    recorder: Option<Recorder>,
}

impl Media for SessionSound {
    fn play(&mut self, pcm: Vec<u8>) {
        let gains = &self.gains;
        self.player
            .get_or_insert_with(|| Player::spawn(gains.clone()))
            .push(pcm);
    }

    fn played(&mut self) -> Option<Vec<u8>> {
        None
    }

    fn silence(&mut self) {
        self.player = None;
    }

    fn record(&mut self, on: bool, up: &Feedback) {
        self.recorder = on.then(|| Recorder::start(self.mic.clone(), up.clone()));
    }

    fn mic_allowed(&self) -> bool {
        self.mic.allowed()
    }
}
