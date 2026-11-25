//! "Auto" for the scale of the PC's virtual monitors (`plans/desktop-scale.md`): the
//! main headset picks for each monitor in Auto the smallest scale the PC offers at
//! which a logical pixel of its desktop takes at least [`LINE`] of the headset's
//! pixels where the monitor is shown, and asks the PC for it (`Request::AutoScale`).
//!
//! The number is `k`, headset pixels per logical pixel: the headset pixels a pixel of
//! the stream takes (`clarity::shown`, worked out by the world for each monitor where it
//! goes) times the scale. Monitors glued edge to edge get scales chosen together, so
//! their `k` is as alike as the steps allow and the seam between them stays seamless.
//! Nothing is asked while a monitor is in a hand or being resized: only once the
//! arrangement held for [`SETTLE`], past a margin either way ([`UP_BELOW`],
//! [`DOWN_FROM`]), at most every [`EVERY`] for a monitor, and never again a scale the
//! PC did not give while the monitor keeps its size. The choice depends on where the
//! monitors are shown in the headset only, never on the PC's desktop, so the PC's
//! change (and the layout that follows it) does not come back as another.
//!
//! Nothing is allocated once each monitor has its entry.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::ids::{DisplayKey, PcId};

/// The least headset pixels a logical pixel of the desktop takes in Auto: a stroke of
/// one logical pixel still lands on most of a headset pixel, and text is no worse
/// than at 1:1 with the compositor's sharpening (`plans/clarity-2026-11.md`, 6).
pub const LINE: f32 = 0.85;
/// A larger scale is asked only once `k` fell below this...
pub const UP_BELOW: f32 = 0.82;
/// ... and a smaller one only when it keeps `k` at this or more: a monitor moved about
/// the line does not go back and forth.
pub const DOWN_FROM: f32 = 0.88;
/// How long the arrangement holds, nothing in a hand, before a scale is asked: as the
/// desktop's arrangement (`world::ARRANGE_AFTER`).
pub const SETTLE: Duration = crate::world::ARRANGE_AFTER;
/// A monitor's scale is asked at most this often.
pub const EVERY: Duration = Duration::from_secs(3);
/// A scale asked that the monitor does not have this long after counts as not given
/// (the PC kept another, or gave the nearest its desktop takes).
pub const GIVEN_WITHIN: Duration = Duration::from_secs(5);
/// How often the monitors are looked at (not every frame).
pub const LOOK_EVERY: Duration = Duration::from_millis(200);
/// Monitors looked at together at most, as many as a PC lists.
pub const MAX: usize = 32;

/// `k` of a monitor shown at `p` headset pixels per stream pixel, at `scale` percent.
pub fn k(p: f32, scale: u16) -> f32 {
    p * f32::from(scale) / 100.0
}

/// The smallest of `scales` (percent, 100 or more) at which `k` reaches `line`;
/// where none does, the largest. None without a scale of 100 % or more.
pub fn smallest(p: f32, scales: &[u16], line: f32) -> Option<u16> {
    let ok = || scales.iter().copied().filter(|s| *s >= 100);
    ok().filter(|s| k(p, *s) >= line)
        .min()
        .or_else(|| ok().max())
}

/// The scales of monitors glued together, shown at `ps` (headset pixels per stream
/// pixel), into `out` (as long as `ps`): each its own [`smallest`] at least, raised
/// where that brings the `k` of the group closer together, never past the `k` the
/// one needing the most has anyway. Neighbours of one size get one scale. Nothing is
/// allocated.
pub fn for_group(ps: &[f32], scales: &[u16], line: f32, out: &mut [u16]) {
    let n = ps.len().min(out.len());
    let mut top = 0.0_f32;
    for (i, &p) in ps.iter().enumerate().take(n) {
        out[i] = smallest(p, scales, line).unwrap_or(100);
        top = top.max(k(p, out[i]));
    }
    if n < 2 {
        return;
    }
    // Each `k` some member has at some scale, from the line up to `top`, as the least
    // the group stands at: the one whose spread is smallest (the lower on a tie).
    let mut best: Option<(f32, f32)> = None;
    for &p in &ps[..n] {
        for &s in scales.iter().filter(|s| **s >= 100) {
            let floor = k(p, s);
            if floor < line || floor > top {
                continue;
            }
            let spread = spread_at(&ps[..n], scales, floor);
            let better = match best {
                None => true,
                Some((f, sp)) => spread < sp - 1e-4 || (spread <= sp + 1e-4 && floor < f),
            };
            if better {
                best = Some((floor, spread));
            }
        }
    }
    if let Some((floor, _)) = best {
        for (i, &p) in ps.iter().enumerate().take(n) {
            out[i] = smallest(p, scales, floor).unwrap_or(out[i]);
        }
    }
}

/// How far apart the `k` of a group is when each takes the smallest scale reaching
/// `floor`.
fn spread_at(ps: &[f32], scales: &[u16], floor: f32) -> f32 {
    let (mut lo, mut hi) = (f32::MAX, f32::MIN);
    for &p in ps {
        let s = smallest(p, scales, floor).unwrap_or(100);
        lo = lo.min(k(p, s));
        hi = hi.max(k(p, s));
    }
    hi - lo
}

/// The scale a monitor at `now` percent goes to, shown at `p`, the group choosing `to`
/// for it, `own` its own choice: up only once `k` fell below [`UP_BELOW`] (or to stand
/// as its glued neighbours, `to` above `own`), down only where `k` stays at
/// [`DOWN_FROM`] or more.
pub fn settle_on(p: f32, now: u16, to: u16, own: u16) -> u16 {
    use std::cmp::Ordering::{Equal, Greater, Less};
    match to.cmp(&now) {
        Equal => now,
        Greater if k(p, now) < UP_BELOW || to > own => to,
        Less if k(p, to) >= DOWN_FROM => to,
        _ => now,
    }
}

/// A monitor in Auto as the headset sees it now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Monitor {
    pub key: DisplayKey,
    /// Headset pixels per stream pixel where it goes.
    pub p: f32,
    /// Its scale now, percent.
    pub now: u16,
    /// Its stream's size, pixels.
    pub size: (u32, u32),
    /// Its group: the index in the list of the first monitor it is glued to (through
    /// others too), its own where none.
    pub group: usize,
}

/// What Auto keeps of a monitor.
#[derive(Debug, Clone, Copy, Default)]
struct Track {
    /// The scale wanted (other than the one it has), since when.
    want: Option<(u16, Instant)>,
    /// The scale asked last, when.
    asked: Option<(u16, Instant)>,
    /// A scale the PC did not give, at the size it had then.
    refused: Option<(u16, (u32, u32))>,
}

/// Auto of one headset: what it asked and was refused, for each monitor.
#[derive(Debug, Default)]
pub struct Auto {
    tracks: HashMap<DisplayKey, Track>,
    /// When the monitors were last looked at.
    looked: Option<Instant>,
}

impl Auto {
    /// Whether the monitors are to be looked at `now` (at most every [`LOOK_EVERY`]).
    pub fn look_due(&mut self, now: Instant) -> bool {
        if self
            .looked
            .is_some_and(|at| now.saturating_duration_since(at) < LOOK_EVERY)
        {
            return false;
        }
        self.looked = Some(now);
        true
    }

    /// The scale to ask now for one of `monitors` (in Auto, of one PC, with their
    /// groups) out of `scales` (the PC's), if one is due: the first due, one a call.
    /// `moving`: something is in a hand, nothing is asked and the wait starts again.
    pub fn due(
        &mut self,
        monitors: &[Monitor],
        scales: &[u16],
        moving: bool,
        now: Instant,
    ) -> Option<(DisplayKey, u16)> {
        let n = monitors.len().min(MAX);
        let mut chosen = [100_u16; MAX];
        let mut own = [100_u16; MAX];
        let mut ps = [0.0_f32; MAX];
        let mut members = [0_usize; MAX];
        let mut out = [100_u16; MAX];
        for (g, first) in monitors.iter().enumerate().take(n) {
            // Each group once, from its first monitor.
            if first.group != g {
                continue;
            }
            let mut m = 0;
            for (i, mon) in monitors.iter().enumerate().take(n) {
                if mon.group == g {
                    members[m] = i;
                    ps[m] = mon.p;
                    m += 1;
                }
            }
            for_group(&ps[..m], scales, LINE, &mut out[..m]);
            for (j, &i) in members[..m].iter().enumerate() {
                chosen[i] = out[j];
                own[i] = smallest(ps[j], scales, LINE).unwrap_or(100);
            }
        }
        let mut asked = None;
        for (i, mon) in monitors.iter().enumerate().take(n) {
            let to = settle_on(mon.p, mon.now, chosen[i], own[i]);
            let track = self.tracks.entry(mon.key).or_default();
            // A refusal holds while the monitor keeps its size.
            if track.refused.is_some_and(|(_, size)| size != mon.size) {
                track.refused = None;
            }
            // Asked, answered, and still not at it: not given.
            if let Some((s, at)) = track.asked
                && s != mon.now
                && now.saturating_duration_since(at) >= GIVEN_WITHIN
            {
                track.refused = Some((s, mon.size));
                track.asked = None;
            }
            if to == mon.now || track.refused.is_some_and(|(s, _)| s == to) {
                track.want = None;
                continue;
            }
            let since = match track.want {
                Some((s, since)) if s == to && !moving => since,
                _ => {
                    track.want = Some((to, now));
                    continue;
                }
            };
            let recent = track
                .asked
                .is_some_and(|(_, at)| now.saturating_duration_since(at) < EVERY);
            if asked.is_some() || recent || now.saturating_duration_since(since) < SETTLE {
                continue;
            }
            track.asked = Some((to, now));
            asked = Some((mon.key, to));
        }
        asked
    }

    /// The scale last asked for monitor `key`, while no answer said otherwise.
    pub fn asked(&self, key: DisplayKey) -> Option<u16> {
        self.tracks.get(&key)?.asked.map(|(s, _)| s)
    }

    /// The PC did not give `scale` to monitor `key` of `size`: not asked again while
    /// it keeps that size.
    pub fn refused(&mut self, key: DisplayKey, scale: u16, size: (u32, u32)) {
        let track = self.tracks.entry(key).or_default();
        track.refused = Some((scale, size));
        track.asked = None;
    }

    /// Only the monitors `keep` says stay are remembered (no allocation).
    pub fn retain(&mut self, keep: impl Fn(DisplayKey) -> bool) {
        self.tracks.retain(|k, _| keep(*k));
    }

    /// A new session with PC `pc`, or its end: nothing of before counts.
    pub fn forget_pc(&mut self, pc: PcId) {
        self.tracks.retain(|k, _| k.pc != pc);
    }
}

/// Puts each of `monitors` in the group of the first one it is glued to (through
/// others too), by `glued`, which says whether two are. Nothing is allocated.
pub fn group(monitors: &mut [Monitor], glued: impl Fn(DisplayKey, DisplayKey) -> bool) {
    let n = monitors.len().min(MAX);
    for (i, m) in monitors.iter_mut().enumerate().take(n) {
        m.group = i;
    }
    // Joined until nothing changes: a handful of monitors, a handful of passes.
    loop {
        let mut changed = false;
        for i in 0..n {
            for j in (i + 1)..n {
                let (gi, gj) = (monitors[i].group, monitors[j].group);
                if gi == gj || !glued(monitors[i].key, monitors[j].key) {
                    continue;
                }
                let (low, high) = (gi.min(gj), gi.max(gj));
                for m in monitors.iter_mut().take(n) {
                    if m.group == high {
                        m.group = low;
                    }
                }
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prostir_proto::state::DisplayId;

    const SCALES: [u16; 5] = [100, 125, 150, 175, 200];

    fn key(id: u32) -> DisplayKey {
        DisplayKey::new(PcId([1; 8]), DisplayId(id))
    }

    fn monitor(id: u32, p: f32, now: u16) -> Monitor {
        Monitor {
            key: key(id),
            p,
            now,
            size: (2560, 1440),
            group: 0,
        }
    }

    /// The table of the design: 1:1 stays at 100 %; 1920x1080 at the standard size
    /// 125 %; 2560x1440 standard 125 %, at 60 degrees 150 %; portrait 150 %. Never
    /// below 100 %; the largest where none reaches the line.
    #[test]
    fn the_smallest_scale_that_reaches_the_line() {
        let at = |p| smallest(p, &SCALES, LINE).unwrap();
        assert_eq!(at(1.0), 100);
        assert_eq!(at(0.70), 125);
        assert_eq!(at(0.77), 125);
        assert_eq!(at(0.59), 150);
        assert_eq!(at(0.67), 150);
        assert_eq!(at(1.6), 100);
        assert_eq!(at(0.3), 200);
        assert_eq!(smallest(0.7, &[50, 75], LINE), None);
        // Without 150 % (Hyprland and 2560x1440): the next one up.
        assert_eq!(smallest(0.59, &[100, 125, 200], LINE), Some(200));
    }

    /// Neighbours of one size take one scale; of different density, the scales that
    /// bring their `k` closest without going past the one needing the most.
    #[test]
    fn glued_monitors_choose_together() {
        let mut out = [0; 2];
        for_group(&[0.70, 0.70], &SCALES, LINE, &mut out);
        assert_eq!(out, [125, 125]);
        // 2560x1440 next to 1920x1080 of the same height: 0.59 and 0.79.
        for_group(&[0.59, 0.79], &SCALES, LINE, &mut out);
        assert_eq!(out, [175, 125], "k 1.03 and 0.99");
        let spread = (k(0.59, out[0]) - k(0.79, out[1])).abs();
        assert!(spread < 0.05);
        // Alone: its own.
        let mut one = [0; 1];
        for_group(&[0.59], &SCALES, LINE, &mut one);
        assert_eq!(one, [150]);
    }

    /// Up only below 0.82, down only where 0.88 holds: about the line nothing goes
    /// back and forth; to stand as its neighbours it goes up anyway.
    #[test]
    fn a_margin_either_way() {
        assert_eq!(settle_on(0.83, 100, 125, 125), 100, "0.83 is enough");
        assert_eq!(settle_on(0.81, 100, 125, 125), 125);
        assert_eq!(
            settle_on(0.86, 125, 100, 100),
            125,
            "0.86 at 100 % is too near"
        );
        assert_eq!(settle_on(0.89, 125, 100, 100), 100);
        assert_eq!(settle_on(0.95, 100, 125, 100), 125, "as its neighbours");
        // Moved back and forth about the line: one change at most.
        let mut now = 100;
        for p in [0.84, 0.83, 0.86, 0.84, 0.87, 0.83] {
            let own = smallest(p, &SCALES, LINE).unwrap();
            now = settle_on(p, now, own, own);
        }
        assert_eq!(now, 100);
    }

    /// Nothing while moving; once let go, one request after the arrangement held for
    /// 0.8 s; not again for 3 s; a scale not given is not asked again at that size,
    /// and is once the size changes.
    #[test]
    fn asked_once_after_letting_go() {
        let mut auto = Auto::default();
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let monitors = [monitor(1, 0.59, 100)];
        assert_eq!(auto.due(&monitors, &SCALES, true, at(0)), None);
        assert_eq!(
            auto.due(&monitors, &SCALES, true, at(2000)),
            None,
            "in a hand"
        );
        assert_eq!(
            auto.due(&monitors, &SCALES, false, at(2100)),
            None,
            "just let go"
        );
        assert_eq!(auto.due(&monitors, &SCALES, false, at(2700)), None);
        assert_eq!(
            auto.due(&monitors, &SCALES, false, at(2850)),
            Some((key(1), 150))
        );
        assert_eq!(auto.due(&monitors, &SCALES, false, at(3200)), None, "asked");
        // Given: nothing more.
        let given = [monitor(1, 0.59, 150)];
        assert_eq!(auto.due(&given, &SCALES, false, at(9000)), None);
        // Refused at this size: not asked again; at another size it is.
        auto.refused(key(1), 200, (2560, 1440));
        let smaller = [monitor(1, 0.45, 150)];
        assert_eq!(auto.due(&smaller, &SCALES, false, at(10_000)), None);
        assert_eq!(auto.due(&smaller, &SCALES, false, at(20_000)), None);
        let resized = [Monitor {
            size: (1920, 1080),
            ..monitor(1, 0.45, 150)
        }];
        assert_eq!(auto.due(&resized, &SCALES, false, at(21_000)), None);
        assert_eq!(
            auto.due(&resized, &SCALES, false, at(22_000)),
            Some((key(1), 200))
        );
        // Answered but never at it: not given, not asked again at that size.
        assert_eq!(auto.due(&resized, &SCALES, false, at(28_000)), None);
        assert_eq!(auto.due(&resized, &SCALES, false, at(40_000)), None);
    }

    /// A headset model not in the table has no `p`: the app puts no such monitor in
    /// the list, so nothing is asked. Groups join through a monitor between two.
    #[test]
    fn groups_join_through_others() {
        let mut list = [
            monitor(1, 0.7, 100),
            monitor(2, 0.7, 100),
            monitor(3, 0.7, 100),
            monitor(4, 0.7, 100),
        ];
        let pairs = [(1, 3), (3, 2)];
        group(&mut list, |a, b| {
            pairs
                .iter()
                .any(|&(x, y)| (key(x), key(y)) == (a, b) || (key(y), key(x)) == (a, b))
        });
        let groups: Vec<usize> = list.iter().map(|m| m.group).collect();
        assert_eq!(groups, [0, 0, 0, 3]);
        let mut auto = Auto::default();
        assert_eq!(auto.due(&[], &SCALES, false, Instant::now()), None);
    }
}
