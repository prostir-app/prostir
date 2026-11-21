//! How fast the connection to the headset may send: up to what the network carries
//! without a queue building in the Wi-Fi access point, and never slower than the
//! streams need.
//!
//! Unpaced, TCP pushes a whole keyframe into the access point's queue at once, and
//! sound waits behind it there, where nothing on the PC can overtake it. TCP hides
//! lost packets behind retransmissions, so the sign of a full link is that queue: the
//! round trip rising above its lowest. The pace follows it, as congestion control
//! does (BBR's way of looking, much simplified):
//!
//! - **Start.** Nothing is known yet: not paced, until the round trip first rises.
//! - **Hold.** Once the round trip rises clearly over the lowest of the last 10 s while
//!   the PC was sending as fast as allowed, what the network delivered just then is
//!   what the link carries: the pace goes 15 % under it and stays there.
//! - **Probe.** After a few seconds held, the pace goes a tenth over the held one
//!   (Wi-Fi gets faster too, say when the user walks up to the access point). If the
//!   round trip rises, the held pace is back. If the PC sends at that pace for a while
//!   (200 ms) and the network delivers it with the round trip low, the link carries
//!   it: that pace is held now, and the next step up follows at once.
//! - **Ramp.** Such steps, one after the other while each one shows the round trip
//!   low: the pace grows a tenth for every 200 ms that the link carried it, so it
//!   never gets far over what was carried, and stops at the first rise.
//!
//! Whatever the link seems to carry, the pace never goes under what the streams need
//! (a fifth more than what was queued for the headset over the last 2 s, and at
//! least 20 Mbit/s): a link slower than the video is for the bitrate to adapt to
//! (0.2.0 step 10), not for pacing to starve the video. A round trip that rises while
//! the PC sends less than the pace allows is somebody else's queue (or Wi-Fi being
//! slow for a moment) and says nothing about the pace.
//!
//! The round trip is TCP's (smoothed, in `TCP_INFO`), crossed with the session's own
//! pings; the delivery rate only says what the link carried when the round trip rose.
//! [`Pacer`] is a pure state machine over these measures (time comes in with them);
//! the pace for a target and a floor is [`pace_for`]. The link estimator of step 10
//! replaces it.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use prostir_proto::link::{LinkInfo, Via};

/// The floor: this many tenths of what the streams need.
const FLOOR_TENTHS: u64 = 12;
/// Never paced slower (20 Mbit/s).
const MIN_PACE: u64 = 20_000_000 / 8;
/// What the streams need is what was queued for the headset over about this long.
const DEMAND_WINDOW: Duration = Duration::from_secs(2);
/// The lowest round trip is the lowest over this long.
const MIN_RTT_WINDOW: Duration = Duration::from_secs(10);
/// The round trip has risen when it is this much over the lowest, or half the lowest
/// if that is more.
const QUEUE_DELAY: Duration = Duration::from_millis(10);
/// A round trip rising counts against the pace only when the pace held the PC back
/// this recently.
const OURS: Duration = Duration::from_millis(200);
/// What the link carried when the round trip rose: the middle of the delivery rates
/// measured over this long while the PC was held back.
const CARRIED: Duration = Duration::from_millis(500);
/// Held this share (in hundredths) of what the link carried.
const HOLD_SHARE: u64 = 85;
/// After backing off, the queue drains: while the round trip stays up, the next back
/// off waits this long.
const DRAIN: Duration = Duration::from_millis(500);
/// Held this long before a probe.
const HOLD_FOR: Duration = Duration::from_secs(3);
/// A step up (a probe, or the ramp) paces this share (in hundredths) of the held pace.
const STEP_SHARE: u64 = 110;
/// A step up holds once the PC was held back this long at its pace, the network
/// delivering it (at least [`CARRIED_SHARE`]) with the round trip low: long enough for
/// a tenth too much to queue past [`QUEUE_DELAY`] and show. Until then (it may take a
/// while: the pace holds the PC back only while a keyframe goes out, say) the step
/// stays on, which costs nothing while nothing queues.
const STEP_PROOF: Duration = Duration::from_millis(200);
/// The network delivered the pace: at least this share of it (in hundredths). A step
/// a tenth over the link gets only the link delivered, about 91 % of the step: under
/// this share, so such a step never holds before the round trip shows it.
const CARRIED_SHARE: u64 = 95;
/// A new pace that differs less than this share (in hundredths) is not worth setting.
const CHANGE: u64 = 3;

/// The pace for a link held at `target` bytes a second (`None`: not paced yet) when
/// the streams need `floor`: 0 means no limit.
pub fn pace_for(target: Option<u64>, floor: u64) -> u64 {
    target.map_or(0, |t| t.max(floor))
}

/// What the streams need with room to spare, when `demand` bytes a second are queued
/// for the headset.
pub fn floor_for(demand: u64) -> u64 {
    (demand / 10 * FLOOR_TENTHS).max(MIN_PACE)
}

/// What the pacer does now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Not paced yet: the round trip has not risen.
    Start,
    /// Held under what the link carried, until `until`.
    Hold { until: Instant },
    /// A step over the held pace after a hold (`ramp`: false) or after the step
    /// before it held (true); held back at it with the round trip low for `proof`
    /// so far.
    Step { ramp: bool, proof: Duration },
}

impl std::fmt::Display for Phase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Phase::Start => "not paced yet",
            Phase::Hold { .. } => "held under the link",
            Phase::Step { ramp: false, .. } => "probing",
            Phase::Step { ramp: true, .. } => "ramping up",
        })
    }
}

/// The lowest of a measure over a sliding window, kept per second.
#[derive(Default)]
struct WindowMin {
    /// Each second's lowest, with when the second began.
    seconds: VecDeque<(Instant, Duration)>,
}

impl WindowMin {
    fn add(&mut self, now: Instant, v: Duration) {
        while self
            .seconds
            .front()
            .is_some_and(|&(at, _)| now.saturating_duration_since(at) > MIN_RTT_WINDOW)
        {
            self.seconds.pop_front();
        }
        match self.seconds.back_mut() {
            Some((at, min)) if now.saturating_duration_since(*at) < Duration::from_secs(1) => {
                *min = (*min).min(v);
            }
            _ => self.seconds.push_back((now, v)),
        }
    }

    fn get(&self) -> Option<Duration> {
        self.seconds.iter().map(|&(_, v)| v).min()
    }

    /// `v` is clearly over the lowest: a queue.
    fn risen(&self, v: Duration) -> bool {
        self.get()
            .is_some_and(|min| v > min + QUEUE_DELAY.max(min / 2))
    }
}

/// What was queued for the headset lately, in slices of a quarter of the window.
#[derive(Default)]
struct Demand {
    slices: VecDeque<(Instant, u64)>,
}

impl Demand {
    fn add(&mut self, now: Instant, bytes: usize) {
        while self
            .slices
            .front()
            .is_some_and(|&(at, _)| now.saturating_duration_since(at) > DEMAND_WINDOW)
        {
            self.slices.pop_front();
        }
        match self.slices.back_mut() {
            Some((at, b)) if now.saturating_duration_since(*at) < DEMAND_WINDOW / 4 => {
                *b += bytes as u64;
            }
            _ => self.slices.push_back((now, bytes as u64)),
        }
    }

    /// Bytes a second, over the time the slices cover (at least half the
    /// window, so the first bytes do not read as a flood).
    fn rate(&self, now: Instant) -> u64 {
        let Some(&(first, _)) = self.slices.front() else {
            return 0;
        };
        let covered = now.saturating_duration_since(first).max(DEMAND_WINDOW / 2);
        let bytes: u64 = self.slices.iter().map(|&(_, b)| b).sum();
        (bytes as f64 / covered.as_secs_f64()) as u64
    }
}

/// One look at the connection, every few tens of milliseconds while the PC sends.
#[derive(Debug, Clone, Copy)]
pub struct Look {
    pub now: Instant,
    /// What TCP measured.
    pub link: LinkInfo,
    /// The network (or the pace) held the PC back since the last look: the writer
    /// waited on the socket most of the time.
    pub held_back: bool,
    /// What was queued for the headset since the last look, bytes.
    pub queued: usize,
}

/// What the pacer did since its stats were last taken.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PacerStats {
    /// Times the round trip rose and the pace backed off.
    pub backoffs: u32,
    /// Probes, and the ones that found the link carrying more.
    pub probes: u32,
    pub probes_up: u32,
    /// The slowest and fastest pace set, and the highest floor, bytes a second.
    pub pace_min: u64,
    pub pace_max: u64,
    pub floor_max: u64,
    /// How long the PC was held back, and how much of that at a pace over a quarter
    /// more than the link delivered (where the link delivered the floor, and the pace
    /// was over it: the pace's own doing).
    pub held_back: Duration,
    pub over_link: Duration,
    /// Segments TCP sent again (each one held up what came after it on the
    /// connection), and the most bytes waiting unsent in the kernel at a look.
    pub retransmits: u32,
    pub unsent_max: u32,
}

impl PacerStats {
    /// `20 to 85 Mbit/s (the streams need up to 60); backed off 3 times, probed 2 (1
    /// found more); held back 1.2 s, 4 % of it over what the link delivered; TCP sent
    /// 3 segments again, at most 31 KB unsent`.
    pub fn summary(&self) -> String {
        let mbit = |b: u64| b * 8 / 1_000_000;
        let pace = match self.pace_max {
            0 => "not paced".to_string(),
            max => format!("{} to {} Mbit/s", mbit(self.pace_min), mbit(max)),
        };
        let held = self.held_back.as_secs_f64();
        let over = match held {
            0.0 => 0.0,
            _ => self.over_link.as_secs_f64() / held * 100.0,
        };
        format!(
            "{pace} (the streams need up to {}); backed off {} times, probed {} ({} found \
             more); held back {held:.1} s, {over:.0} % of it over what the link delivered; \
             TCP sent {} segments again, at most {} KB unsent",
            mbit(self.floor_max),
            self.backoffs,
            self.probes,
            self.probes_up,
            self.retransmits,
            self.unsent_max / 1024,
        )
    }
}

/// Turns what TCP measures into a pace for one connection.
pub struct Pacer {
    /// Over USB nothing queues the way Wi-Fi does: no pacing.
    usb: bool,
    phase: Phase,
    /// The pace held (and probed from), bytes a second.
    held: u64,
    floor: u64,
    /// The pace set on the connection (0: none).
    pace: u64,
    last: Option<Instant>,
    /// When the pace last held the PC back.
    held_back_at: Option<Instant>,
    /// When it last backed off.
    backed_off_at: Option<Instant>,
    rtt: WindowMin,
    ping: WindowMin,
    /// A pong came back late since the last look.
    ping_risen: bool,
    demand: Demand,
    /// Delivery rates measured while the PC was held back, with when.
    delivered: VecDeque<(Instant, u64)>,
    stats: PacerStats,
    /// TCP's count of segments sent again, at the last look.
    retransmits: Option<u32>,
}

impl Pacer {
    pub fn new(via: Via) -> Self {
        Self {
            usb: via == Via::Usb,
            phase: Phase::Start,
            held: 0,
            floor: MIN_PACE,
            pace: 0,
            last: None,
            held_back_at: None,
            backed_off_at: None,
            rtt: WindowMin::default(),
            ping: WindowMin::default(),
            ping_risen: false,
            demand: Demand::default(),
            delivered: VecDeque::new(),
            stats: PacerStats::default(),
            retransmits: None,
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// The pace now, bytes a second (0: none).
    #[cfg(test)]
    pub fn pace(&self) -> u64 {
        self.pace
    }

    /// What the streams need with room to spare: the pace never goes under it.
    pub fn floor(&self) -> u64 {
        self.floor
    }

    /// The lowest round trip of the last 10 s.
    pub fn min_rtt(&self) -> Option<Duration> {
        self.rtt.get()
    }

    /// What happened since the last call.
    pub fn take_stats(&mut self) -> PacerStats {
        std::mem::take(&mut self.stats)
    }

    /// A ping's round trip: one that comes back clearly later than the lowest of the
    /// last 10 s also means a queue.
    pub fn pong(&mut self, now: Instant, rtt: Duration) {
        self.ping_risen |= self.ping.risen(rtt);
        self.ping.add(now, rtt);
    }

    /// Takes a look at the connection: the new pace when it should change.
    pub fn look(&mut self, look: Look) -> Option<u64> {
        let Look {
            now,
            link,
            held_back,
            queued,
        } = look;
        let dt = self
            .last
            .replace(now)
            .map_or(Duration::ZERO, |last| now.saturating_duration_since(last));
        self.demand.add(now, queued);
        self.floor = floor_for(self.demand.rate(now));
        if let Some(before) = self.retransmits.replace(link.retransmits) {
            self.stats.retransmits += link.retransmits.saturating_sub(before);
        }
        self.stats.unsent_max = self.stats.unsent_max.max(link.unsent);
        if self.usb || link.rtt.is_zero() {
            return None;
        }
        let risen = self.rtt.risen(link.rtt) || std::mem::take(&mut self.ping_risen);
        self.rtt.add(now, link.rtt);
        if held_back {
            self.held_back_at = Some(now);
            self.delivered.push_back((now, link.delivery_rate));
            self.stats.held_back += dt;
            if self.pace > self.floor
                && link.delivery_rate >= self.floor
                && self.pace / 100 * 80 > link.delivery_rate
            {
                self.stats.over_link += dt;
            }
        }
        while self
            .delivered
            .front()
            .is_some_and(|&(at, _)| now.saturating_duration_since(at) > CARRIED)
        {
            self.delivered.pop_front();
        }
        let ours = self
            .held_back_at
            .is_some_and(|at| now.saturating_duration_since(at) <= OURS);
        let drained = self
            .backed_off_at
            .is_none_or(|at| now.saturating_duration_since(at) >= DRAIN);
        let effective = self.pace.max(self.floor);
        if risen && ours && drained {
            self.back_off(now);
        } else if !risen && held_back && link.delivery_rate >= effective / 100 * CARRIED_SHARE {
            self.carried_the_pace(dt, effective);
        }
        self.time_passes(now);
        self.set(self.target())
    }

    /// The round trip rose while the PC sent as fast as allowed: the link is full.
    fn back_off(&mut self, now: Instant) {
        self.backed_off_at = Some(now);
        self.stats.backoffs += 1;
        let carried = self.carried();
        let under = carried / 100 * HOLD_SHARE;
        self.held = match self.phase {
            Phase::Start => under,
            // Held already: the link got slower.
            Phase::Hold { .. } => self.held.min(under),
            // Back to the pace that was fine.
            Phase::Step { .. } => self.held,
        };
        self.phase = Phase::Hold {
            until: now + HOLD_FOR,
        };
    }

    /// Held back, the link delivered the pace with the round trip low: it carries it.
    /// A step that did long enough holds, and the next one follows.
    fn carried_the_pace(&mut self, dt: Duration, effective: u64) {
        if let Phase::Step { ramp, proof } = &mut self.phase {
            *proof += dt;
            if *proof >= STEP_PROOF {
                if !*ramp {
                    self.stats.probes_up += 1;
                }
                self.held = effective;
                self.phase = Phase::Step {
                    ramp: true,
                    proof: Duration::ZERO,
                };
            }
        }
    }

    /// A hold ends in a probe.
    fn time_passes(&mut self, now: Instant) {
        if let Phase::Hold { until } = self.phase
            && now >= until
        {
            self.stats.probes += 1;
            self.phase = Phase::Step {
                ramp: false,
                proof: Duration::ZERO,
            };
        }
    }

    /// What the link carried lately while the PC was held back: the middle of the
    /// delivery rates (a single one can read well over or under, when
    /// acknowledgements come in clumps).
    fn carried(&self) -> u64 {
        let mut rates: Vec<u64> = self.delivered.iter().map(|&(_, r)| r).collect();
        rates.sort_unstable();
        rates.get(rates.len() / 2).copied().unwrap_or(self.held)
    }

    /// The pace the phase asks for, before the floor.
    fn target(&self) -> Option<u64> {
        match self.phase {
            Phase::Start => None,
            Phase::Hold { .. } => Some(self.held),
            Phase::Step { .. } => Some(self.held.max(self.floor) / 100 * STEP_SHARE),
        }
    }

    fn set(&mut self, target: Option<u64>) -> Option<u64> {
        let pace = pace_for(target, self.floor);
        // The floor always holds; otherwise small changes are not worth a call.
        let changed = pace.abs_diff(self.pace) > self.pace / 100 * CHANGE
            || (pace == 0) != (self.pace == 0)
            || (pace > 0 && self.pace < self.floor);
        if !changed {
            return None;
        }
        self.pace = pace;
        if pace > 0 {
            let s = &mut self.stats;
            s.pace_min = if s.pace_min == 0 {
                pace
            } else {
                s.pace_min.min(pace)
            };
            s.pace_max = s.pace_max.max(pace);
        }
        self.stats.floor_max = self.stats.floor_max.max(self.floor);
        Some(pace)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MBIT: u64 = 1_000_000 / 8;
    const LOOK: Duration = Duration::from_millis(20);

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    fn mbit(bytes_per_sec: u64) -> u64 {
        bytes_per_sec / MBIT
    }

    /// The pace is the target, never under the floor; no target, no limit.
    #[test]
    fn the_pace_is_the_target_never_under_the_floor() {
        assert_eq!(pace_for(Some(90 * MBIT), 30 * MBIT), 90 * MBIT);
        assert_eq!(pace_for(Some(10 * MBIT), 30 * MBIT), 30 * MBIT);
        assert_eq!(pace_for(None, 30 * MBIT), 0);
        assert_eq!(floor_for(0), 20 * MBIT, "never under 20 Mbit/s");
        assert_eq!(floor_for(60 * MBIT), 72 * MBIT);
    }

    /// A Wi-Fi link in a few lines, with the PC's writer and its streams: what the PC
    /// sends goes into the access point's queue, which empties at the link's rate,
    /// and the round trip is the base one plus that queue.
    struct Sim {
        pacer: Pacer,
        now: Instant,
        start: Instant,
        /// The link, bytes a second, and its round trip when empty.
        rate: u64,
        base: Duration,
        /// Bytes in the access point's queue, and the most it holds.
        queue: f64,
        limit: f64,
        /// What the streams make: bytes a second, and a keyframe of every stream (in
        /// all, bytes) this often.
        demand: u64,
        keyframes: u64,
        keyframes_every: Duration,
        next_keyframes: Instant,
        /// Bytes waiting on the PC.
        backlog: f64,
        srtt: f64,
        /// What happened, look by look.
        trace: Vec<Step>,
    }

    #[derive(Debug, Clone, Copy)]
    struct Step {
        at: Duration,
        pace: u64,
        floor: u64,
        rate: u64,
        held_back: bool,
        queue_delay: Duration,
    }

    impl Sim {
        fn new(rate_mbit: u64, demand_mbit: u64) -> Self {
            let now = Instant::now();
            Self {
                pacer: Pacer::new(Via::Network),
                now,
                start: now,
                rate: rate_mbit * MBIT,
                base: ms(3),
                // 200 packets of 1500 bytes, as the shaped link of the end-to-end test.
                queue: 0.0,
                limit: 300_000.0,
                demand: demand_mbit * MBIT,
                keyframes: 1_000_000,
                keyframes_every: Duration::from_secs(2),
                next_keyframes: now,
                backlog: 0.0,
                srtt: 0.003,
                trace: Vec::new(),
            }
        }

        /// One look of the writer.
        fn step(&mut self) {
            let dt = LOOK.as_secs_f64();
            let mut queued = self.demand as f64 * dt;
            if self.now >= self.next_keyframes {
                queued += self.keyframes as f64;
                self.next_keyframes += self.keyframes_every;
            }
            // Frames that wait too long are dropped on the PC.
            self.backlog = (self.backlog + queued).min(8_000_000.0);
            // Unpaced, the PC's network card (1 Gbit/s) is the limit.
            let pace = match self.pacer.pace() {
                0 => 1000 * MBIT,
                p => p,
            } as f64;
            // TCP fills the queue up to where it loses packets, no further.
            let drains = self.rate as f64 * dt;
            let room = (self.limit - self.queue + drains).max(0.0);
            let sent = self.backlog.min(pace * dt).min(room);
            self.backlog -= sent;
            let held_back = self.backlog > 1.0;
            let drained = (self.queue + sent).min(drains);
            self.queue = (self.queue + sent - drained).min(self.limit);
            let rtt = self.base.as_secs_f64() + self.queue / self.rate as f64;
            self.srtt += (rtt - self.srtt) / 2.0;
            self.now += LOOK;
            let link = LinkInfo {
                delivery_rate: (drained / dt) as u64,
                rtt: Duration::from_secs_f64(self.srtt),
                ..LinkInfo::default()
            };
            self.pacer.look(Look {
                now: self.now,
                link,
                held_back,
                queued: queued as usize,
            });
            self.trace.push(Step {
                at: self.now - self.start,
                pace: self.pacer.pace(),
                floor: self.pacer.floor(),
                rate: self.rate,
                held_back,
                queue_delay: Duration::from_secs_f64(self.queue / self.rate as f64),
            });
        }

        fn run(&mut self, for_: Duration) {
            let until = self.now + for_;
            while self.now < until {
                self.step();
            }
        }

        /// The steps from `from` on.
        fn after(&self, from: Duration) -> impl Iterator<Item = &Step> {
            self.trace.iter().filter(move |s| s.at >= from)
        }
    }

    /// The longest the access point's queue held the PC's data, from `from` on.
    fn worst_queue(sim: &Sim, from: Duration) -> Duration {
        sim.after(from).map(|s| s.queue_delay).max().unwrap()
    }

    /// Never paced under the floor, once paced at all.
    fn assert_floor(sim: &Sim) {
        for s in &sim.trace {
            assert!(s.pace == 0 || s.pace >= s.floor, "{s:?}");
        }
    }

    /// A first burst unpaced shows where the link is full; the pace then holds under
    /// it, and the queue stays short.
    #[test]
    fn the_pace_finds_the_link_and_holds_under_it() {
        let mut sim = Sim::new(100, 30);
        sim.run(Duration::from_secs(30));
        assert_floor(&sim);
        let paces = || sim.after(Duration::from_secs(5)).map(|s| mbit(s.pace));
        let (low, high) = (paces().min().unwrap(), paces().max().unwrap());
        assert!(low >= 60 && high <= 125, "paced {low} to {high}");
        let worst = worst_queue(&sim, Duration::from_secs(5));
        assert!(worst <= ms(20), "queued up to {worst:?}");
    }

    /// A fast link is not slowed down: unpaced while nothing queues, or paced near
    /// its speed.
    #[test]
    fn a_fast_link_is_not_slowed_down() {
        let mut sim = Sim::new(400, 60);
        sim.run(Duration::from_secs(30));
        let pace = mbit(sim.pacer.pace());
        assert!(pace == 0 || pace >= 300, "paced at {pace}");
        assert!(worst_queue(&sim, Duration::from_secs(5)) <= ms(10));
        // With a shorter queue, the first keyframes fill it, and the pace finds the
        // link.
        let mut sim = Sim::new(400, 60);
        sim.limit = 1_000_000.0;
        sim.keyframes = 3_000_000;
        sim.run(Duration::from_secs(30));
        assert_floor(&sim);
        let pace = mbit(sim.pacer.pace());
        assert!((300..=480).contains(&pace), "paced at {pace}");
    }

    /// The link gets faster: probes find out and the pace follows.
    #[test]
    fn probes_find_a_faster_link() {
        let mut sim = Sim::new(60, 20);
        sim.run(Duration::from_secs(10));
        assert!(mbit(sim.pacer.pace()) <= 66);
        sim.rate = 200 * MBIT;
        sim.run(Duration::from_secs(20));
        let pace = mbit(sim.pacer.pace());
        assert!(pace >= 100, "paced at {pace}");
        assert!(worst_queue(&sim, Duration::from_secs(10)) <= ms(15));
    }

    /// The owner's Wi-Fi, 2026-11-19: the estimate went 117, 150, 191 in 4 s while
    /// 6 monitors (about 55 Mbit/s, and keyframes of all of them every second as
    /// frames were dropped) went over a link of about 100 with a deep queue, and the
    /// session stalled. Paced by the round trip, the pace stays near the link, or at
    /// the floor where the streams need more.
    #[test]
    fn a_deep_queue_does_not_lift_the_pace() {
        let mut sim = Sim::new(100, 55);
        sim.limit = 4_000_000.0;
        sim.keyframes = 2_400_000;
        sim.keyframes_every = Duration::from_secs(1);
        sim.run(Duration::from_secs(30));
        assert_floor(&sim);
        for s in sim.after(Duration::from_secs(2)) {
            // The pace changes only by more than 3 %.
            assert!(
                s.pace <= (s.floor / 100 * 104).max(s.rate / 100 * 115),
                "{s:?}"
            );
        }
        let top = sim
            .after(Duration::from_secs(2))
            .map(|s| mbit(s.pace))
            .max();
        assert!(top < Some(150), "paced up to {top:?}");
    }

    /// The same Wi-Fi after the reconnect: what the link delivered read 143, 85 and 17
    /// Mbit/s while the round trip rose, and the pace fell to 20 while the monitors
    /// needed 55. It stays over what they need.
    #[test]
    fn a_collapse_does_not_starve_the_video() {
        let mut p = Pacer::new(Via::Network);
        let start = Instant::now();
        let need = 55 * MBIT;
        let mut paces = Vec::new();
        for i in 0..300u32 {
            let now = start + LOOK * i;
            let (rate, rtt) = match i {
                0..50 => (143, 4),
                50..100 => (85, 40),
                100..200 => (17, 130),
                _ => (28, 60),
            };
            p.look(Look {
                now,
                link: LinkInfo {
                    delivery_rate: rate * MBIT,
                    rtt: ms(rtt),
                    ..LinkInfo::default()
                },
                held_back: true,
                queued: (need as f64 * LOOK.as_secs_f64()) as usize,
            });
            if i >= 50 {
                paces.push(mbit(p.pace()));
            }
        }
        assert!(paces.iter().all(|&m| m >= 66), "{paces:?}");
    }

    /// The Wi-Fi-like link: its speed changes every 3 s between 40 and 150 Mbit/s,
    /// the round trip jitters, and the streams make about 55. The pace never goes
    /// under the floor, and while it holds the PC back, it rarely sits far over what
    /// the link delivers.
    #[test]
    fn a_varying_link_is_followed() {
        let mut sim = Sim::new(150, 50);
        sim.keyframes = 1_500_000;
        let rates = [150, 90, 40, 120, 60, 150, 70, 100, 45, 130];
        let mut jitter = 1u64;
        for _ in 0..3 {
            for r in rates {
                sim.rate = r * MBIT;
                for _ in 0..150 {
                    jitter = jitter
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(1442695040888963407);
                    sim.base = ms(2) + Duration::from_micros((jitter >> 33) % 3000);
                    sim.step();
                }
            }
        }
        assert_floor(&sim);
        // Where the link carries what the streams need, that is: under that, nothing
        // pacing does helps.
        let held: Vec<&Step> = sim
            .after(Duration::from_secs(3))
            .filter(|s| s.held_back && s.pace > s.floor && s.rate >= s.floor)
            .collect();
        let over = held.iter().filter(|s| s.pace > s.rate / 100 * 125).count();
        assert!(
            over * 10 <= held.len(),
            "over the link {over} of {} looks",
            held.len()
        );
        // Where the link carries the floor, the queue stays short most of the time.
        let mut delays: Vec<Duration> = sim
            .after(Duration::from_secs(3))
            .filter(|s| s.rate >= s.floor)
            .map(|s| s.queue_delay)
            .collect();
        delays.sort_unstable();
        let p95 = delays[delays.len() * 95 / 100];
        assert!(p95 <= ms(20), "queue p95 {p95:?}");
    }

    /// A pong that comes back late means a queue too.
    #[test]
    fn a_late_pong_backs_off() {
        let mut p = Pacer::new(Via::Network);
        let start = Instant::now();
        let link = LinkInfo {
            delivery_rate: 100 * MBIT,
            rtt: ms(3),
            ..LinkInfo::default()
        };
        let look = |p: &mut Pacer, i: u32| {
            p.look(Look {
                now: start + LOOK * i,
                link,
                held_back: true,
                queued: 0,
            })
        };
        for i in 0..20 {
            look(&mut p, i);
        }
        assert_eq!(p.pace(), 0, "unpaced while nothing rose");
        for s in 0..5 {
            p.pong(start + Duration::from_secs(s), ms(4));
        }
        p.pong(start + Duration::from_secs(5), ms(40));
        assert_eq!(look(&mut p, 20), Some(85 * MBIT));
        assert!(matches!(p.phase(), Phase::Hold { .. }));
    }

    /// Nothing is paced over USB, and a round trip that rises while the pace does not
    /// hold the PC back is not the pace's doing.
    #[test]
    fn only_our_own_queue_counts() {
        let start = Instant::now();
        let look = |i: u32, rtt: u64, held_back: bool| Look {
            now: start + LOOK * i,
            link: LinkInfo {
                delivery_rate: 50 * MBIT,
                rtt: ms(rtt),
                ..LinkInfo::default()
            },
            held_back,
            queued: 0,
        };
        let mut usb = Pacer::new(Via::Usb);
        assert_eq!(usb.look(look(0, 3, true)), None);
        assert_eq!(usb.look(look(1, 50, true)), None);
        let mut p = Pacer::new(Via::Network);
        assert_eq!(p.look(look(0, 3, false)), None);
        assert_eq!(p.look(look(20, 50, false)), None, "not ours");
        assert_eq!(p.look(look(40, 3, true)), None);
        assert_eq!(
            p.look(look(41, 50, true)),
            Some(pace_for(Some(42 * MBIT + MBIT / 2), MIN_PACE))
        );
    }
}
