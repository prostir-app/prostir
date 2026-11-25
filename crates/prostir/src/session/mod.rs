//! One headset's session on the PC: its connection's two threads (the writer, which
//! paces what goes out, and the reader, which answers what is instant and hands the
//! rest to the control loop as events), what the headset said and was told, and the
//! watchers its preferences ask for. The control loop (`main.rs`) owns every session
//! and calls into it; nothing here waits for the headset.
//!
//! Each session is one headset's, next to the others: its own outbox, what it was
//! told, whether it is worn. What only one headset can have (the desktop's
//! arrangement, the PC's volume, the PC's end of the microphone, "No limits" for the
//! PC's limits) the control loop gives one session at a time (see [`Standing`]).

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use log::{debug, info, warn};
use prostir_proto::link::{Closer, Link, Via};
use prostir_proto::request::{Asked, Command, Outcome};
use prostir_proto::state::{
    self, Access, ErrorCode, HeadsetState, Part, ReqId, UpKind, caps, clip, permission, perms, role,
};
use prostir_proto::stats::Period;
use prostir_proto::volume::{Act, PcVolume};
use prostir_proto::{ClientMsg, HeadsetGoodbye, HostMsg, PcGoodbye};

use crate::api::{self, Answers, ApiError, CtlReply, Reply};
use crate::displays::Displays;
use crate::events::{Event, EventSink, SessionId};
use crate::platform::audio::SinkVolume;
use crate::platform::{notifications, sound_source};
use crate::router::Router;
use crate::screen::ScreenId;
use crate::store::Store;
use crate::{i18n, keep_screens, outbox, pacing, pairing, platform, publish, requests};

mod handle;
mod link;
mod ups;
mod volume;

use link::{spawn_reader, spawn_writer};

/// Longest notification passed to the headset, in bytes.
const MAX_NOTICE: usize = 8 * 1024;

/// How often the writer looks at what TCP measures, while it sends.
const LOOK_AT_THE_LINK: Duration = Duration::from_millis(20);
/// How often the writer logs how it paced.
const PACING_REPORT_EVERY: Duration = Duration::from_secs(10);

/// After a goodbye, how long the headset has to read it and close the connection.
const GOODBYE_WAIT: Duration = Duration::from_secs(1);

/// Nothing at all from the headset for this long (it pings every second): the session
/// is over.
pub const SILENCE: Duration = Duration::from_secs(5);

/// A command the headset does not answer within this is given up (a late answer is
/// let go).
const COMMAND_ANSWER: Duration = Duration::from_secs(30);

/// Bytes a second in Mbit/s, for the log.
fn mbit(bytes_per_sec: u64) -> u64 {
    bytes_per_sec * 8 / 1_000_000
}

/// What the headset's goodbye means, for the log.
pub fn headset_left(reason: u8) -> &'static str {
    match HeadsetGoodbye::from_code(reason) {
        HeadsetGoodbye::Quit => "the headset app was closed",
        HeadsetGoodbye::OtherPc => "the headset chose another PC",
        HeadsetGoodbye::Forgotten => "the headset forgot this PC",
        HeadsetGoodbye::Other => "the headset ended the session",
    }
}

/// How the tray names the way the headset is connected.
pub fn via_name(via: Via) -> &'static str {
    match via {
        Via::Usb => "USB",
        Via::Network => "Wi-Fi",
    }
}

/// What a session has of what only one headset can have at a time, as the control
/// loop gives it (see `brain::roles`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Standing {
    /// The main session: its arrangement is the desktop's, its volume the PC's, and its
    /// "No limits" the PC's limits.
    pub main: bool,
    /// Its microphone is the PC's end of the headset microphone.
    pub mic: bool,
    /// The tray shows this headset, commands go to it, and it says when its battery
    /// is low: the main session, else the first.
    pub lead: bool,
    /// "No limits" of the main session, for the PC's limits.
    pub no_limits: bool,
}

/// What the tray shows of a headset.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrayLine {
    /// How it is connected.
    pub via: Option<&'static str>,
    /// Its "No limits", where it takes commands.
    pub no_limits: Option<bool>,
    /// Its microphone is on, but its system does not let it record.
    pub mic_denied: bool,
    /// Its battery: percent and whether it charges.
    pub battery: Option<(u8, bool)>,
}

/// The role and rights a headset has in its session: those of its line in
/// `settings.txt` (`configured`), but the main role only in the main session.
fn access_in(configured: Access, main: bool) -> Access {
    Access {
        role: if main && configured.is_main() {
            role::MAIN
        } else {
            role::GUEST
        },
        perms: configured.perms,
    }
}

/// One session's state on the PC: what the headset said and was told, the watchers
/// its preferences ask for, and the replies waiting for this pass's state to go first.
pub struct Session {
    pub id: SessionId,
    publisher: publish::Publisher,
    /// The role and rights of its line in `settings.txt`.
    pub configured: Access,
    standing: Standing,
    outbox: Arc<outbox::Outbox>,
    closer: Closer,
    writer: Option<JoinHandle<()>>,
    /// How the headset is connected, for the tray.
    via: &'static str,
    /// When the headset was taken off (as last reported by it).
    taken_off_at: Option<Instant>,
    /// Off the head and asleep (it said `Asleep`), until it says it is worn again.
    asleep: bool,
    /// Streams the headset needs a fresh picture of, put on again.
    fresh: Vec<ScreenId>,
    /// The PC's volume is shared with this headset.
    shares_volume: bool,
    /// PC notifications passed to the headset, while it asks for them.
    notifications: Option<notifications::Watcher>,
    /// Which monitor the sound comes from, while the headset wants to know.
    sound_source: Option<sound_source::Watcher>,
    /// The monitor the sound comes from, as last looked at.
    sound_from: Option<ScreenId>,
    battery_warned: bool,
    /// Its microphone is on, but its system does not let it record.
    mic_denied: bool,
    /// Replies to send once the parts they changed went out.
    replies: Vec<HostMsg>,
    /// Requests whose answer waits for a job of the desktop's worker: the request,
    /// the job, and until when it may take.
    waiting: Vec<(ReqId, crate::worker::JobId, Instant)>,
    /// Commands sent to the headset without an answer yet: what each was, who waits
    /// for what came of it, and since when.
    commands: BTreeMap<ReqId, (String, Option<Reply>, Instant)>,
    /// Where the answers to them wait for the control loop's pass (see `set_answers`).
    answers: Answers,
    last_command: u32,
    /// The headset's sound device and the logic of one volume, where both can.
    volume: Option<(SinkVolume, PcVolume)>,
    /// Up streams the headset started, which its reader takes (see `ups.rs`).
    ups: Arc<ups::Ups>,
}

/// A session that said goodbye: the headset has a moment to read it and close the
/// connection, and what it still sends is read and dropped meanwhile, so a reset
/// cannot throw the goodbye away.
pub struct Closing {
    pub id: SessionId,
    /// Closed by then whatever the headset does.
    pub until: Instant,
    closer: Closer,
    writer: Option<JoinHandle<()>>,
}

impl Closing {
    /// Closes the connection and waits for the writer, which has nothing left to send.
    pub fn done(mut self) {
        self.closer.close();
        if let Some(w) = self.writer.take() {
            let _ = w.join();
        }
    }
}

/// The headset's battery is low: say so on the PC, where the user is looking.
fn notify_battery(percent: u8) {
    let body = format!("{} {percent}%.", i18n::t("pc-session-headset-battery"));
    platform::dialogs::notify(i18n::t("pc-session-the-headset-battery-is-low"), &body);
}

impl Session {
    /// Opens session `id` on a connection set up and trusted, with what it has of the
    /// jobs only one headset can have (`standing`): the snapshot first, then the writer
    /// and the reader, and every stream starts over for this headset. The streams go
    /// in a codec this headset and those of the other sessions (`others`) decode.
    #[expect(
        clippy::too_many_arguments,
        reason = "what a session starts from, each from its own owner"
    )]
    pub fn start(
        id: SessionId,
        conn: pairing::Connection,
        open: &[Part],
        standing: Standing,
        others: &[state::HeadsetCaps],
        displays: &mut Displays,
        store: &Store,
        events: &EventSink,
    ) -> Result<Self> {
        let pairing::Connection {
            link:
                Link {
                    sender,
                    receiver,
                    closer,
                    peer,
                },
            mut packets,
            ..
        } = conn;
        let via = via_name(peer.via);
        // What the headset said about itself; without it there is no session.
        let headset = HeadsetState::from_open(open).context("the headset's Open")?;
        let configured = store.wanted().access_of(&peer.key);
        let main = standing.main && configured.is_main();
        let standing = Standing {
            main,
            // The main session's own.
            no_limits: if main {
                headset.prefs.unwrap_or_default().no_limits
            } else {
                standing.no_limits
            },
            ..standing
        };
        let mut publisher = publish::Publisher::new(headset, access_in(configured, main));
        let mut headsets = others.to_vec();
        headsets.push(publisher.caps().clone());
        displays.fit_codec(&headsets);
        let outbox = Arc::new(outbox::Outbox::default());
        // The session opens now that it can be served: the monitors exist, and the
        // writer (which also pings) starts below. The headset counts silence from
        // here. The snapshot is the first message, before any stream can start.
        // The headset's sound device, where the PC can follow its volume: with the main
        // headset only (a guest's volume does not move the PC's, nor is it asked to).
        let sink = SinkVolume::open().filter(|_| main);
        let shares_volume = sink.is_some();
        let snapshot = publisher.snapshot(&publish::Sources {
            displays,
            wanted: &store.wanted(),
            focus: None,
            sound_from: None,
            volume: shares_volume,
            mic: standing.mic,
            no_limits: standing.no_limits,
        })?;
        outbox.control(snapshot);
        packets.set_limits(ClientMsg::limits(true));
        let writer = spawn_writer(outbox.clone(), sender, closer.clone(), peer.via)?;
        let ups = Arc::new(ups::Ups::new(
            publisher.access.may(perms::MIC),
            standing.mic,
        ));
        let reader = spawn_reader(
            id,
            receiver,
            packets,
            link::Shared {
                outbox: outbox.clone(),
                router: displays.router.clone(),
                ups: ups.clone(),
                mic: displays.mic_tap(),
            },
            events.clone(),
        );
        if let Err(e) = reader {
            outbox.close();
            closer.close();
            let _ = writer.join();
            return Err(e);
        }

        let worn = worn(&publisher.headset);
        displays
            .router
            .attach(id, outbox.clone(), publisher.caps().clone(), worn);
        // Streams too big for every headset stand failed in the first `Displays` after
        // the snapshot, before anything of them could go.
        displays.stop_too_big();
        // The streams the snapshot listed may start (each starts over just below).
        let listed = publisher.streamable().map(ScreenId::from_wire).collect();
        displays.router.publish(id, &listed);
        displays.headset_connected();
        let mut session = Session {
            id,
            publisher,
            configured,
            standing,
            outbox,
            closer,
            writer: Some(writer),
            via,
            taken_off_at: (!worn).then(Instant::now),
            asleep: false,
            fresh: Vec::new(),
            shares_volume,
            notifications: None,
            sound_source: None,
            sound_from: None,
            battery_warned: false,
            mic_denied: false,
            replies: Vec::new(),
            waiting: Vec::new(),
            commands: BTreeMap::new(),
            answers: Answers::default(),
            last_command: 0,
            volume: None,
            ups,
        };
        // One volume for the PC's slider and the headset's buttons, where both can.
        if session.publisher.caps().has(caps::headset::VOLUME) {
            session.volume = sink.map(|sink| (sink, PcVolume::default()));
        }
        // The first headset brings its level to the PC; one that comes while others
        // listen takes the PC's instead, so their sound does not jump.
        let opened_with = session.publisher.headset.volume;
        match opened_with {
            Some(v) if others.is_empty() => session.volume_from_headset(v),
            _ => session.take_pc_volume(),
        }
        session.headset_changed(displays);
        session.publish(displays, store, None);
        crate::usage::headset_connected(store.wanted().count);
        Ok(session)
    }

    /// The headset's own "No limits".
    pub fn no_limits(&self) -> bool {
        self.publisher.prefs().no_limits
    }

    /// Its microphone's switch is on (it reaches the PC only in the session the
    /// control loop gives the microphone, and with the right to it).
    pub fn mic_on(&self) -> bool {
        self.publisher
            .headset
            .up
            .get(&UpKind::Mic)
            .is_some_and(|m| m.switch)
    }

    /// The headset streams its microphone now.
    pub fn mic_streams(&self) -> bool {
        self.ups.mic_streams()
    }

    /// What the headset can do.
    pub fn caps(&self) -> &state::HeadsetCaps {
        self.publisher.caps()
    }

    /// The headset is connected over USB.
    pub fn by_usb(&self) -> bool {
        self.via == via_name(Via::Usb)
    }

    /// On the head (a headset that never said counts as worn).
    pub fn worn(&self) -> bool {
        self.taken_off_at.is_none()
    }

    /// Off the head and asleep: it does not come back by itself soon.
    pub fn asleep(&self) -> bool {
        self.asleep
    }

    /// How long the headset has been off the head, if it is.
    pub fn taken_off_for(&self) -> Option<Duration> {
        self.taken_off_at.map(|t| t.elapsed())
    }

    /// Since when the headset has been off the head, if it is.
    pub fn taken_off_at(&self) -> Option<Instant> {
        self.taken_off_at
    }

    /// The streams this headset needs a fresh picture of since the last call (it was
    /// put on again).
    pub fn take_fresh(&mut self) -> Vec<ScreenId> {
        std::mem::take(&mut self.fresh)
    }

    /// Where the answers to commands go: they wait for the end of the control loop's
    /// pass, so a reply never goes out before the state that shows what it changed.
    pub fn set_answers(&mut self, answers: Answers) {
        self.answers = answers;
    }

    /// Sends `msg` to the headset in the control lane, after what went before it.
    pub fn send(&self, msg: HostMsg) {
        self.outbox.control(msg);
    }

    /// What the PC allows this headset, as its limits say now.
    pub fn limits(&self, displays: &Displays) -> state::Limits {
        self.publisher.limits(displays, self.standing.no_limits)
    }

    /// The session as the local API shows it.
    pub fn api_session(&self) -> api::Session {
        let headset = &self.publisher.headset;
        let line = self.tray_line();
        let mic = headset.up.get(&UpKind::Mic);
        let profiles: Vec<String> = headset
            .profiles
            .iter()
            .flat_map(|p| &p.list)
            .map(|p| p.name.clone())
            .collect();
        // The one in use by what `profile` takes: its name, else its number.
        let profile = headset
            .profiles
            .as_ref()
            .and_then(|p| p.active)
            .map(usize::from)
            .filter(|i| *i < profiles.len())
            .map(|i| match profiles[i].as_str() {
                "" => (i + 1).to_string(),
                name => name.to_owned(),
            });
        api::Session {
            via: match self.via {
                "USB" => api::Via::Usb,
                _ => api::Via::Wifi,
            },
            worn: self.worn(),
            battery: line
                .battery
                .map(|(percent, charging)| api::Battery { percent, charging }),
            profile,
            profiles,
            mic_allowed: mic.and_then(|m| match m.permission {
                permission::GRANTED => Some(true),
                permission::DENIED => Some(false),
                _ => None,
            }),
            mic_on: self.mic_on(),
            main: self.standing.main,
            commands: self.publisher.caps().has(caps::headset::COMMANDS),
            no_limits: line.no_limits,
        }
    }

    /// The headset's volume, while the PC's follows it.
    pub fn shared_volume(&self) -> Option<api::Volume> {
        self.volume.as_ref()?;
        let v = self.publisher.headset.volume?;
        Some(api::Volume {
            level: v.level,
            muted: v.muted,
        })
    }

    /// What the tray shows of this headset.
    pub fn tray_line(&self) -> TrayLine {
        let headset = &self.publisher.headset;
        let mic = headset.up.get(&UpKind::Mic);
        TrayLine {
            via: Some(self.via),
            no_limits: self
                .publisher
                .caps()
                .has(caps::headset::COMMANDS)
                .then(|| self.no_limits()),
            mic_denied: mic.is_some_and(|m| m.switch && m.permission == permission::DENIED),
            battery: headset.battery.map(|b| (b.percent, b.charging)),
        }
    }

    /// What the session has now of the jobs only one headset can have. Made the main
    /// one, it takes the PC's volume over (from its own level); no longer the main
    /// one, its volume is its own again. Its `Access` and the PC's capabilities go out
    /// with the next pass.
    pub fn set_standing(&mut self, standing: Standing) {
        let main = standing.main && self.configured.is_main();
        let standing = Standing { main, ..standing };
        if standing == self.standing {
            return;
        }
        let was_main = self.standing.main;
        self.standing = standing;
        self.ups.set_mic(standing.mic);
        self.publisher.access = access_in(self.configured, main);
        if main == was_main {
            return;
        }
        info!(
            "session {}: {}",
            self.id,
            if main {
                "the main headset now"
            } else {
                "a guest now"
            }
        );
        let sink = SinkVolume::open().filter(|_| main);
        self.shares_volume = sink.is_some();
        self.volume = sink
            .filter(|_| self.publisher.caps().has(caps::headset::VOLUME))
            .map(|sink| (sink, PcVolume::default()));
        // The PC's sound does not jump to this headset's level: the headset is asked
        // to take the PC's.
        self.take_pc_volume();
    }

    /// Ends the session. With `goodbye` the headset is told why and gets a moment to
    /// read it (the [`Closing`] returned); without, the connection closes now. The
    /// other sessions go on: only this one's headset leaves the router.
    pub fn finish(mut self, goodbye: Option<PcGoodbye>, displays: &Displays) -> Option<Closing> {
        self.end();
        displays.router.detach(self.id);
        let closing = Closing {
            id: self.id,
            until: Instant::now() + GOODBYE_WAIT,
            closer: self.closer.clone(),
            writer: self.writer.take(),
        };
        match goodbye {
            Some(reason) => {
                info!("saying goodbye to the headset ({reason:?})");
                self.outbox.say_goodbye(reason);
                Some(closing)
            }
            None => {
                self.outbox.close();
                closing.done();
                None
            }
        }
    }

    /// Sends the commands waiting for the headset; gives up on those it did not answer
    /// in time.
    pub fn commands_due(&mut self, waiting: &mut VecDeque<(Asked<Command>, Option<Reply>)>) {
        let takes = self.publisher.caps().has(caps::headset::COMMANDS);
        for (asked, reply) in waiting.drain(..) {
            let what = format!("{asked:?}");
            if !takes {
                info!("{what}: the headset takes no commands");
                if let Some(r) = reply {
                    self.answers.push(
                        r,
                        CtlReply::failed(
                            ApiError::Unsupported,
                            i18n::t("pc-session-the-headset-takes-no-commands"),
                        ),
                    );
                }
                continue;
            }
            // One that cannot go as it is gets an answer, and the session goes on.
            if let Asked::Known(command) = &asked
                && let Err(why) = crate::local::fits_the_wire(command)
            {
                warn!("{what}: {}", why.text);
                if let Some(r) = reply {
                    self.answers.push(r, CtlReply::Failed(why));
                }
                continue;
            }
            self.last_command += 1;
            let req = ReqId(self.last_command);
            info!("command {} for the headset: {what}", req.0);
            self.outbox.control(HostMsg::Command { req, asked });
            self.commands.insert(req, (what, reply, Instant::now()));
        }
        let late: Vec<ReqId> = self
            .commands
            .iter()
            .filter(|(_, (_, _, at))| at.elapsed() >= COMMAND_ANSWER)
            .map(|(req, _)| *req)
            .collect();
        for req in late {
            if let Some((what, reply, _)) = self.commands.remove(&req) {
                warn!("command {} ({what}): the headset does not answer", req.0);
                if let Some(r) = reply {
                    self.answers.push(
                        r,
                        CtlReply::failed(
                            ApiError::TimedOut,
                            i18n::t("pc-session-the-headset-does-not-answer"),
                        ),
                    );
                }
            }
        }
    }

    /// The session is over: commands still waiting get no answer.
    fn end(&mut self) {
        for (_, (what, reply, _)) in std::mem::take(&mut self.commands) {
            info!("{what}: the headset left");
            if let Some(r) = reply {
                self.answers.push(
                    r,
                    CtlReply::failed(
                        ApiError::Disconnected,
                        i18n::t("pc-session-the-headset-disconnected"),
                    ),
                );
            }
        }
    }

    /// The answer to a request that waited for job `job` of the desktop's worker:
    /// whether it was this session's.
    pub fn job_done(&mut self, job: crate::worker::JobId, outcome: &Outcome) -> bool {
        let Some(i) = self.waiting.iter().position(|(_, j, _)| *j == job) else {
            return false;
        };
        let (req, _, _) = self.waiting.remove(i);
        if let Outcome::Failed { detail, .. } = outcome {
            warn!("request {}: {detail}", req.0);
        }
        info!("request {}: answered", req.0);
        self.replies.push(HostMsg::Reply {
            req,
            outcome: outcome.clone(),
        });
        true
    }

    /// When a request waiting for the desktop has waited long enough.
    pub fn waiting_until(&self) -> Option<Instant> {
        self.waiting.iter().map(|(_, _, until)| *until).min()
    }

    /// Requests that waited for the desktop until `now` (all of them with `all`, the
    /// desktop not responding) fail with `DESKTOP`: their jobs, which nobody waits for
    /// now.
    pub fn give_up_waiting(&mut self, now: Instant, all: bool) -> Vec<crate::worker::JobId> {
        let (over, still): (Vec<_>, Vec<_>) = std::mem::take(&mut self.waiting)
            .into_iter()
            .partition(|(_, _, until)| all || *until <= now);
        self.waiting = still;
        let jobs = over.iter().map(|(_, job, _)| *job).collect();
        for (req, _, _) in over {
            warn!("request {}: the desktop does not respond", req.0);
            self.replies.push(HostMsg::Reply {
                req,
                outcome: Outcome::failed(ErrorCode::DESKTOP, &desktop_not_responding()),
            });
        }
        jobs
    }

    /// Tells the headset what changed, then the replies.
    pub fn publish(&mut self, displays: &mut Displays, store: &Store, focus: Option<ScreenId>) {
        let sources = publish::Sources {
            displays,
            wanted: &store.wanted(),
            focus,
            sound_from: self.sound_from,
            volume: self.shares_volume,
            mic: self.standing.mic,
            no_limits: self.standing.no_limits,
        };
        for msg in self.publisher.changed(&sources) {
            self.outbox.control(msg);
        }
        let listed = self
            .publisher
            .streamable()
            .map(ScreenId::from_wire)
            .collect();
        for id in displays.router.publish(self.id, &listed) {
            displays.fresh_picture(id);
        }
        for reply in self.replies.drain(..) {
            self.outbox.control(reply);
        }
    }
}

/// What the headset and the person at the PC are told while the desktop does not
/// respond.
pub fn desktop_not_responding() -> String {
    i18n::t("pc-session-the-desktop-does-not-respond").into()
}

/// Whether the headset is on the head, as it last said (one that never said counts as
/// worn).
fn worn(headset: &HeadsetState) -> bool {
    headset.presence.is_none_or(|p| p.worn)
}
