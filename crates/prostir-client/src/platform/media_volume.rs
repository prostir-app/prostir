//! The headset's media volume as the app reads and sets it, over what Android does with
//! it: while muted the system says step 0 (whatever step it comes back to), and
//! setting step 0 mutes it. So a muted volume is said at the level last heard unmuted
//! (the PC keeps its level while muted), and silence the app asked for is silence, not
//! muted (an unmute that sticks). Plain logic over [`Media`], tested with a fake that
//! keeps Android's rules; the system thread drives it with `AudioManager`.

use prostir_proto::volume::{index_of, level_of};

/// The system's media volume: a step from 0 to the volume's steps, and its mute.
pub trait Media {
    /// The step now (0 while muted, as Android says it) and whether it is muted.
    fn read(&mut self) -> Option<(i32, bool)>;
    /// Puts it at step `index`, without the system's own display of it.
    fn set_step(&mut self, index: i32) -> Option<()>;
    /// Mutes it, or unmutes it.
    fn mute(&mut self, muted: bool) -> Option<()>;
}

/// The media volume in percent of its `steps` above silence, as the app follows it.
#[derive(Debug, Clone)]
pub struct MediaVolume {
    steps: u8,
    /// The level last heard (read or set) unmuted: what a muted volume is said at.
    heard: Option<u8>,
    /// The app asked for silence, unmuted: step 0 muted by the system is that.
    silence: bool,
}

impl MediaVolume {
    pub fn new(steps: u8) -> Self {
        Self {
            steps: steps.max(1),
            heard: None,
            silence: false,
        }
    }

    pub fn steps(&self) -> u8 {
        self.steps
    }

    /// The volume now, in percent, and whether it is muted.
    pub fn read(&mut self, media: &mut impl Media) -> Option<(u8, bool)> {
        let (index, muted) = media.read()?;
        let index = u8::try_from(index.max(0)).unwrap_or(u8::MAX);
        let level = level_of(index, self.steps);
        if !muted {
            self.heard = Some(level);
            self.silence = false;
            return Some((level, false));
        }
        if self.silence && index == 0 {
            return Some((0, false));
        }
        self.silence = false;
        Some((self.heard.unwrap_or(level), true))
    }

    /// Puts it at `level` percent, muted or not: the level first, then the mute
    /// (setting a step above 0 unmutes, setting 0 mutes); what it is after.
    pub fn set(&mut self, media: &mut impl Media, level: u8, muted: bool) -> Option<(u8, bool)> {
        let index = index_of(level, self.steps);
        let (_, was_muted) = media.read()?;
        if !muted && was_muted {
            media.mute(false)?;
        }
        media.set_step(i32::from(index))?;
        if muted {
            media.mute(true)?;
        }
        if index > 0 {
            self.heard = Some(level_of(index, self.steps));
        }
        self.silence = index == 0 && !muted;
        self.read(media)
    }
}

/// What the selftest finds of the system's media volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Probe {
    /// Whether a step set came to stand, and the step before came back.
    pub set: bool,
    /// Whether muting and unmuting each came to stand.
    pub mute: bool,
}

/// Sets the media volume one step off and back, then mutes and unmutes it (each read
/// back), and leaves it as it was. `None` when the system could not be asked.
pub fn probe(media: &mut impl Media, steps: u8) -> Option<Probe> {
    let (index, was_muted) = media.read()?;
    if was_muted {
        media.mute(false)?;
    }
    let (index, _) = if was_muted {
        media.read()?
    } else {
        (index, false)
    };
    let other = if index > 0 {
        index - 1
    } else {
        (index + 1).min(i32::from(steps))
    };
    media.set_step(other)?;
    let (became, _) = media.read()?;
    media.set_step(index)?;
    let (back, _) = media.read()?;
    media.mute(true)?;
    let (_, muted) = media.read()?;
    media.mute(false)?;
    let (_, still) = media.read()?;
    media.set_step(index)?;
    if was_muted {
        media.mute(true)?;
    }
    Some(Probe {
        set: became == other && back == index,
        mute: muted && !still,
    })
}

/// A media volume with Android's rules: muted, it says step 0 and keeps the step it
/// comes back to; setting step 0 mutes it, a step above 0 unmutes it; unmuting at step
/// 0 leaves it muted.
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct AndroidRules {
    pub max: i32,
    pub index: i32,
    pub muted: bool,
}

#[cfg(test)]
impl Media for AndroidRules {
    fn read(&mut self) -> Option<(i32, bool)> {
        Some((if self.muted { 0 } else { self.index }, self.muted))
    }

    fn set_step(&mut self, index: i32) -> Option<()> {
        self.index = index.clamp(0, self.max);
        self.muted = self.index == 0;
        Some(())
    }

    fn mute(&mut self, muted: bool) -> Option<()> {
        self.muted = muted || self.index == 0;
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at_step(index: i32) -> AndroidRules {
        AndroidRules {
            max: 15,
            index,
            muted: false,
        }
    }

    /// Muted by the PC at 40, the headset says 40 muted, not silence: the PC keeps
    /// its level; unmuted, it is back at 40, unmuted for good.
    #[test]
    fn muted_keeps_the_level() {
        let mut media = at_step(6);
        let mut v = MediaVolume::new(15);
        assert_eq!(v.read(&mut media), Some((40, false)));
        assert_eq!(v.set(&mut media, 40, true), Some((40, true)));
        assert_eq!(media.read(), Some((0, true)), "as Android says it");
        assert_eq!(v.read(&mut media), Some((40, true)), "read again");
        assert_eq!(v.set(&mut media, 40, false), Some((40, false)));
        assert_eq!((media.index, media.muted), (6, false));
        assert_eq!(v.read(&mut media), Some((40, false)));
        // Muted at another level: said at that one.
        assert_eq!(v.set(&mut media, 73, true), Some((73, true)));
        assert_eq!(v.read(&mut media), Some((73, true)));
    }

    /// Silence asked for, unmuted: Android mutes step 0, and that is the silence asked
    /// for (no change of the headset's own); a level after it unmutes.
    #[test]
    fn silence_is_not_muted() {
        let mut media = at_step(6);
        let mut v = MediaVolume::new(15);
        assert_eq!(v.set(&mut media, 0, false), Some((0, false)));
        assert!(media.muted, "Android muted step 0");
        assert_eq!(v.read(&mut media), Some((0, false)), "and stays silence");
        assert_eq!(v.set(&mut media, 47, false), Some((47, false)));
        assert_eq!((media.index, media.muted), (7, false));
        // Muted at silence: the level heard before it.
        assert_eq!(v.set(&mut media, 0, true), Some((47, true)));
    }

    /// Muted by the system (its menu), the level heard last stands; a volume muted
    /// before the app ever heard it is said at silence.
    #[test]
    fn muted_by_the_system() {
        let mut media = at_step(9);
        let mut v = MediaVolume::new(15);
        assert_eq!(v.read(&mut media), Some((60, false)));
        media.muted = true;
        assert_eq!(v.read(&mut media), Some((60, true)));
        let mut muted = AndroidRules {
            muted: true,
            ..at_step(9)
        };
        assert_eq!(MediaVolume::new(15).read(&mut muted), Some((0, true)));
    }

    /// The selftest's probe: a step off and back, a mute and an unmute, and the volume
    /// left as it was, muted or not.
    #[test]
    fn the_probe_leaves_it_as_it_was() {
        for (index, muted) in [(6, false), (0, false), (15, false), (6, true)] {
            let mut media = AndroidRules {
                muted,
                ..at_step(index)
            };
            let before = (media.index, media.muted);
            let found = probe(&mut media, 15);
            assert_eq!(
                found,
                Some(Probe {
                    set: true,
                    mute: index > 0
                }),
                "{index}, muted {muted}"
            );
            if index > 0 {
                assert_eq!((media.index, media.muted), before);
            }
        }
    }
}
