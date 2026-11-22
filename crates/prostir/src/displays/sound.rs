//! Sound and the microphone as the control loop keeps them: what the sound thread is
//! asked for, and what it made.

use super::*;

/// The PC's end of the headset microphone as the sessions' reader threads reach it:
/// a piece goes in as it comes, without the control loop. Only the control loop
/// changes what is in it, and the readers never hold a share of it, so the
/// microphone is always let go where `Displays` lets it go.
#[derive(Default)]
pub struct MicTap(Mutex<Option<Arc<MicIn>>>);

impl MicTap {
    /// Plays a piece of the headset's microphone into the PC's end, while there is one.
    pub fn push(&self, pcm: &[u8]) {
        if let Some(mic) = &*self.0.lock().unwrap() {
            mic.push(pcm);
        }
    }

    fn set(&self, mic: Option<Arc<MicIn>>) {
        let old = std::mem::replace(&mut *self.0.lock().unwrap(), mic);
        drop(old);
    }
}

impl Displays {
    /// The headset's microphone switch (its `UpState`): the PC's end of the microphone
    /// is there while it is on.
    pub fn set_mic(&mut self, on: bool) {
        if on != self.mic_wanted {
            self.mic_wanted = on;
            // Switched on by hand: tried right away.
            self.by_hand += 1;
            self.sync_audio();
        }
    }

    /// A program on the PC records from the headset's microphone, as the sound system
    /// last said.
    pub fn recording(&self) -> bool {
        self.recording && self.mic.is_some()
    }

    /// The sound system said a program began (`on`) or stopped recording from the
    /// headset's microphone: a recording gets its own way in for the headset's sound.
    pub fn set_recording(&mut self, on: bool) {
        if self.mic.is_none() {
            return;
        }
        if on != self.recording {
            info!(
                "{}",
                if on {
                    "a program on the PC records from the headset microphone"
                } else {
                    "no program on the PC records from the headset microphone"
                }
            );
        }
        self.recording = on;
        // The sound thread tells the microphone (that runs `pactl`).
        self.sync_audio();
    }

    /// What the sound thread made or let go of.
    pub fn sound_done(&mut self, done: SoundDone) {
        match done {
            SoundDone::Audio(route, device) => {
                self.audio = (self.streaming && self.settings.audio).then_some((route, device));
            }
            SoundDone::AudioOff => self.audio = None,
            // Taken only while it is still wanted; else this share goes at once, and
            // the thread lets the microphone go once it hears it is not wanted.
            SoundDone::Mic(mic) => {
                if self.streaming && self.mic_wanted {
                    self.mic_tap.set(Some(mic.clone()));
                    self.mic = Some(mic);
                    self.mic_failed = None;
                } else {
                    worker::sound::let_go_of_mic(mic);
                }
            }
            SoundDone::MicFailed(why) => {
                self.mic_tap.set(None);
                self.mic = None;
                self.mic_failed = Some(why);
            }
            // Also when it broke: the sound thread closed it, and this share goes on a
            // thread of its own all the same.
            SoundDone::MicOff => {
                self.mic_tap.set(None);
                if let Some(share) = self.mic.take() {
                    worker::sound::let_go_of_mic(share);
                }
            }
        }
        self.sync_audio();
    }

    /// Where the PC's sound goes, and the headset microphone on the PC.
    pub fn sound_status(&self) -> SoundStatus {
        let mic = match (&self.mic, &self.mic_failed) {
            _ if !(self.streaming && self.mic_wanted) => None,
            (Some(_), _) => Some(MicAvailability::Ready),
            (None, Some(why)) => Some(why.clone()),
            (None, None) => Some(MicAvailability::Failed(
                t("Мікрофон ще не готовий", "The microphone is not ready yet").into(),
            )),
        };
        SoundStatus {
            route: self.audio.as_ref().map(|(route, _)| *route),
            output: self.audio.as_ref().map(|(_, device)| device.clone()),
            mic,
        }
    }

    /// Starts or stops PC sound and microphone to match the wishes and the state, and
    /// the watcher of the sound system with them (it starts first, so it sees them
    /// come).
    pub(super) fn sync_audio(&mut self) {
        let audio = self.streaming && self.settings.audio;
        let mic = self.streaming && self.mic_wanted;
        if (audio || mic)
            && self.sound_events.is_none()
            && let Some(events) = &self.sound_to
        {
            self.sound_events = audio::watch(events.clone());
        }
        if !audio {
            self.audio = None;
        }
        // This share of the microphone goes before the thread is told, so letting it go
        // (`pactl`, `pw-loopback`) happens there; on a thread of its own all the same,
        // should the sound thread have let its share go first.
        if !mic && let Some(share) = self.mic.take() {
            self.mic_tap.set(None);
            worker::sound::let_go_of_mic(share);
        }
        if self.mic.is_none() {
            self.recording = false;
        }
        self.sound.want(SoundWanted {
            audio,
            mic,
            recording: self.recording,
            recheck: self.recheck,
            by_hand: self.by_hand,
        });
        // Kept while sound or the microphone is wanted, even while neither could be
        // made (they are tried again): a watcher that came and went with every try
        // would start `pactl` again and again.
        if !(audio || mic) {
            self.sound_events = None;
        }
        let status = self.sound_status();
        if self.sound_logged.as_ref() != Some(&status) {
            info!("sound: {status}");
            self.sound_logged = Some(status);
        }
    }

    /// Where the sessions' readers put the headset's microphone.
    pub fn mic_tap(&self) -> Arc<MicTap> {
        self.mic_tap.clone()
    }
}
