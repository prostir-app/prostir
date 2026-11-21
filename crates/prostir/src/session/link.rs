//! A session's threads on the connection: the writer (the outbox to the network,
//! paced) and the reader (the headset's messages to the control loop).

use super::*;

/// Ends the session once the writer thread stops, however it stops: the outbox takes
/// nothing more and the connection closes, so the reader and the headset notice at once.
/// After a goodbye the connection stays open for the control loop to close.
struct EndSession {
    outbox: Arc<outbox::Outbox>,
    closer: Option<Closer>,
}

impl Drop for EndSession {
    fn drop(&mut self) {
        self.outbox.close();
        if let Some(closer) = &self.closer {
            closer.close();
        }
    }
}

/// The writer thread: the only one sending, packet after packet, most important first,
/// pacing the connection by what it measures of the network.
pub(super) fn spawn_writer(
    outbox: Arc<outbox::Outbox>,
    mut sender: Box<dyn prostir_proto::link::RecordSender>,
    closer: Closer,
    via: Via,
) -> Result<JoinHandle<()>> {
    Ok(std::thread::Builder::new()
        .name("net-writer".into())
        .spawn(move || {
            let mut end = EndSession {
                outbox: outbox.clone(),
                closer: Some(closer),
            };
            let mut lanes = outbox::Lanes::default();
            let mut pacer = pacing::Pacer::new(via);
            let mut next_look = Instant::now();
            let mut report = Period::new(PACING_REPORT_EVERY);
            // The pace last logged: changes of a quarter or more go into the log.
            let mut logged = 0u64;
            // How long sending waited on the socket since the last look: most of the
            // time means the network (or the pace) held the PC back.
            let mut waited = Duration::ZERO;
            let mut last_look = Instant::now();
            while let Some(pick) = outbox.next(&mut lanes) {
                let sending = Instant::now();
                if let Err(e) = lanes.send(pick, &mut *sender) {
                    info!("connection closed while sending: {e}");
                    return;
                }
                let now = Instant::now();
                waited += now - sending;
                if now < next_look {
                    continue;
                }
                next_look = now + LOOK_AT_THE_LINK;
                let held_back = waited * 2 >= now - last_look;
                (waited, last_look) = (Duration::ZERO, now);
                let queued = outbox.take_queued();
                if let Some(rtt) = outbox.take_round_trip() {
                    pacer.pong(now, rtt);
                }
                let Some(link) = sender.link_info() else {
                    continue;
                };
                let look = pacing::Look {
                    now,
                    link,
                    held_back,
                    queued,
                };
                if let Some(pace) = pacer.look(look) {
                    sender.set_pace(pace);
                    let ms = |d: Duration| d.as_secs_f64() * 1e3;
                    let line = format!(
                        "pacing at {} Mbit/s, {} (the streams need {}; round trip {:.1} ms, \
                         lowest {:.1} ms)",
                        mbit(pace),
                        pacer.phase(),
                        mbit(pacer.floor()),
                        ms(link.rtt),
                        ms(pacer.min_rtt().unwrap_or_default()),
                    );
                    if pace.abs_diff(logged) > logged / 4 {
                        logged = pace;
                        info!("{line}");
                    } else {
                        debug!("{line}");
                    }
                }
                if report.due(now).is_some() {
                    info!("pacing: {}", pacer.take_stats().summary());
                }
            }
            // After the goodbye nothing more goes out, but what the headset sends
            // until it closes the connection is still read (see `Closing`).
            if outbox.said_goodbye() {
                let _ = sender.finish();
                end.closer = None;
            }
        })?)
}

/// What the reader shares with the rest of the PC app.
pub(super) struct Shared {
    pub outbox: Arc<outbox::Outbox>,
    pub router: Arc<Router>,
    /// The session's up streams.
    pub ups: Arc<ups::Ups>,
    /// The PC's end of the headset microphone.
    pub mic: Arc<crate::displays::MicTap>,
}

/// The reader thread: what is instant is answered here, not in the control loop
/// (which may wait on a desktop tool for a while), so the round trip is the
/// network's, and the microphone goes to the PC as it comes; the rest goes to the
/// control loop as events of session `id`.
pub(super) fn spawn_reader(
    id: SessionId,
    mut receiver: Box<dyn prostir_proto::link::RecordReceiver>,
    mut packets: prostir_proto::packet::Assembler,
    shared: Shared,
    events: EventSink,
) -> Result<()> {
    receiver.set_idle_limit(Some(SILENCE));
    let Shared {
        outbox,
        router,
        ups,
        mic,
    } = shared;
    std::thread::Builder::new()
        .name("net-reader".into())
        .spawn(move || {
            // A stereo microphone's piece in mono, kept between pieces.
            let mut mono = Vec::new();
            loop {
                let msg = match ClientMsg::recv(&mut *receiver, &mut packets) {
                    Ok(msg) => msg,
                    Err(e) => {
                        events.send(Event::SessionEnded(id, e));
                        return;
                    }
                };
                match msg {
                    ClientMsg::Ping { seq } => {
                        outbox.control(HostMsg::Pong { seq });
                    }
                    ClientMsg::Pong { seq } => outbox.pong(seq),
                    // Also here: how long a frame took to show is the headset's and the
                    // network's time, not the control loop's.
                    ClientMsg::FrameShown {
                        id: display,
                        generation,
                        seq,
                    } => router.frame_shown(id, ScreenId::from_wire(display), generation, seq),
                    // A keyframe waits for nobody: asked of the encoder from here.
                    ClientMsg::RequestKeyframe {
                        id: display,
                        generation,
                    } => {
                        let display = ScreenId::from_wire(display);
                        if router.is_current(id, display, generation) {
                            router.keyframe_asked(display);
                        } else {
                            info!(
                                "{display}: a keyframe asked for {generation:?}, not the generation it \
                                 streams now: none made"
                            );
                        }
                    }
                    ClientMsg::Up { up, data } => ups.piece(up, &data, &mic, &mut mono),
                    msg => {
                        // Which up streams there are counts at once; the control loop
                        // then looks at which session has the microphone.
                        match &msg {
                            ClientMsg::UpStart { up, kind, format } => {
                                ups.start(*up, *kind, *format);
                            }
                            ClientMsg::UpEnd { up } => ups.end(*up),
                            _ => {}
                        }
                        if !events.send(Event::Headset(id, Instant::now(), msg)) {
                            return;
                        }
                    }
                }
            }
        })?;
    Ok(())
}
