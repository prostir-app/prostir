//! The encoders on the outputs: started with the wanted settings or a fallback,
//! paused, started again when a size changed or one did not open, and stopped
//! without waiting for one that hangs.

use super::*;

impl Worker {
    /// Starts screen `id`'s encoder on its output with the wanted settings, else with
    /// `fallback`. Says what came of it.
    pub(super) fn start(&mut self, id: ScreenId, fallback: Option<&EncoderConfig>) {
        let Some(wanted) = self.wanted.clone() else {
            return;
        };
        let Some((_, w)) = wanted.screens.iter().find(|(i, _)| *i == id) else {
            return;
        };
        let Some(mut output) = self.slots.get_mut(&id).and_then(|s| s.output.take()) else {
            return;
        };
        let asked = (wanted.encoding.backend, wanted.encoding.codec);
        let mut used = asked;
        let mut result = self.start_encoder(id, &w.name, output.as_mut(), &wanted.encoding);
        // A source that fails fails the same with any encoder.
        if let (Err(e), Some(cfg)) = (&result, fallback)
            && e.downcast_ref::<SourceFailed>().is_none()
        {
            warn!(
                "{}: cannot encode with {:?} {:?}: {e:#}; back to {:?} {:?}",
                w.name, wanted.encoding.backend, wanted.encoding.codec, cfg.backend, cfg.codec
            );
            result = self.start_encoder(id, &w.name, output.as_mut(), cfg);
            used = (cfg.backend, cfg.codec);
        }
        let Some(slot) = self.slots.get_mut(&id) else {
            return;
        };
        slot.output = Some(output);
        match result {
            Ok(encoder) => {
                encoder.force_keyframe();
                if !wanted.streaming {
                    encoder.set_paused(true);
                }
                // Went on with what it had: said, and the wanted one tried again later.
                let kept = (used != asked).then(|| kept_note(used.1, asked.1));
                let note = match (kept, encoder.note()) {
                    (Some(kept), Some(note)) => Some(format!("{kept}; {note}")),
                    (kept, note) => kept.or_else(|| note.map(str::to_owned)),
                };
                let encoding = Encoding {
                    encoder: encoder.kind().0.label(),
                    codec: encoder.codec(),
                    hardware: encoder.kind().0.hardware(),
                    copies: encoder.copies(),
                    note,
                };
                self.shared.router.set_keyframer(id, encoder.keyframer());
                slot.encoder = Some(encoder);
                slot.started_for = Some(used);
                slot.rows_up = wanted.encoding.rows_up;
                slot.cursor_apart = self.cursor_apart;
                slot.kept = (used != asked).then(|| (asked, Instant::now() + KEPT_FOR));
                slot.failed = None;
                self.layout_due = true;
                self.done(Done::Started {
                    id,
                    attempt: w.attempt,
                    encoding,
                });
            }
            Err(e) => {
                error!("{}: no stream: {e:#}", w.name);
                self.shared.router.remove_stream(id);
                let e = e.context(UserError::Encoder);
                if slot.is_panel() {
                    // A window panel without a stream closes, its window back.
                    error!("{id}: no stream; the window panel closes");
                    self.remove(id, true);
                    self.done(Done::PanelGone { id });
                    return;
                }
                self.failed(id, w.attempt, &e);
            }
        }
    }

    /// Starts screen `id`'s encoder over with the wanted settings (the old one stops
    /// first, so a headset never gets frames of two codecs for one stream); if they do
    /// not work, with what it had.
    pub(super) fn restart(&mut self, id: ScreenId) {
        let Some(slot) = self.slots.get_mut(&id) else {
            return;
        };
        let before = slot
            .started_for
            .zip(self.wanted.as_ref())
            .map(|((backend, codec), w)| EncoderConfig {
                codec,
                backend,
                ..w.encoding.clone()
            });
        if let Some(output) = slot.output.as_mut() {
            output.detach();
        }
        let encoder = slot.encoder.take();
        slot.started_for = None;
        stop_encoder(encoder, &slot.name, &self.shared.steps, &self.behind);
        self.start(id, before.as_ref());
    }

    pub(super) fn set_paused(&mut self, id: ScreenId, paused: bool) {
        let Some(slot) = self.slots.get_mut(&id) else {
            return;
        };
        let Some(encoder) = slot.encoder.take() else {
            return;
        };
        let what = format!(
            "{} the encoder of {}",
            if paused { "pausing" } else { "resuming" },
            slot.name
        );
        let retired = encoder.retired.clone();
        let kind = encoder.kind();
        self.shared.steps.begin(what.clone());
        let done = timed::call_counted(
            &self.behind,
            kind,
            "encoder-pause",
            &what,
            ENCODER_STOP,
            move || {
                encoder.set_paused(paused);
                encoder
            },
        );
        match done {
            Ok(encoder) => slot.encoder = Some(encoder),
            // Left behind: started anew at the next step.
            Err(_) => {
                retired.store(true, Ordering::Relaxed);
                slot.started_for = None;
                self.shared.router.remove_stream(id);
            }
        }
    }

    /// A new encoder for stream `id` (`name`),
    /// fed by `output`: the best this PC has for where the output's frames are, in
    /// `cfg`'s codec (only x264 when `cfg` encodes on the CPU), else the next one, in
    /// H.264 if need be (see `encoder/choice.rs`). The stream's slot starts over with
    /// it, and stays when this fails.
    pub(super) fn start_encoder(
        &self,
        id: ScreenId,
        name: &str,
        output: &mut dyn Output,
        cfg: &EncoderConfig,
    ) -> Result<Running> {
        let behind = self.behind.total();
        if behind >= BEHIND_AT_MOST {
            bail!(
                "{behind} {}",
                t("pc-worker-run-encoders-encoders-do-not-respond-no-more")
            );
        }
        let mut source = output.source();
        // A source that failed is tried once more, as it is then (not another encoder:
        // each would fail the same on it).
        let mut source_again = false;
        let wanted = Asked {
            codec: cfg.codec,
            software: cfg.backend == Backend::X264,
        };
        let running = self.running_encoders(&[id]);
        let held = |kind: Kind| running.get(&kind).copied().unwrap_or(0);
        let mut tried: Vec<(Choice, String)> = Vec::new();
        let mut again = false;
        let ranked = loop {
            // Ranked anew after each failure: it may have taught a limit.
            let ranked = crate::encoder::catalog().rank(source.memory, wanted, id, &held);
            let Some(choice) = ranked
                .choices
                .iter()
                .find(|c| !tried.iter().any(|(t, _)| t == *c))
                .copied()
            else {
                break ranked;
            };
            let kind = (choice.backend, choice.adapter);
            let hangs = self.behind.hangs(kind);
            if hangs >= HANGS_OF_A_KIND {
                let why = format!(
                    "{} {hangs}",
                    t("pc-worker-run-encoders-did-not-respond-times")
                );
                tried.push((choice, why));
                continue;
            }
            let cfg = EncoderConfig {
                backend: choice.backend,
                codec: choice.codec,
                ..cfg.clone()
            };
            // A step of its own: within its time limit, never "not responding".
            self.shared
                .steps
                .begin(format!("starting the encoder of {name} ({choice})"));
            self.shared
                .router
                .add_stream(id, choice.codec, name.to_owned());
            // A window panel keeps the pointer in its frames (see `CursorSource`).
            let panel = self.wanted.as_ref().is_some_and(|w| {
                w.screens
                    .iter()
                    .any(|(i, s)| *i == id && matches!(s.what, What::Panel { .. }))
            });
            self.shared
                .router
                .set_stream_output(id, output.key().clone(), panel);
            output.set_cursor_apart(self.cursor_apart && !panel);
            let router = self.shared.router.clone();
            let (input, gpu) = (source.input.clone(), self.gpu.clone());
            // Left behind after its time, a late encoder sends nothing; nor does one
            // let go.
            let retired = Arc::new(AtomicBool::new(false));
            let late = retired.clone();
            let started = timed::call_counted(
                &self.behind,
                kind,
                "encoder-start",
                &format!("starting the encoder of {name}"),
                ENCODER_START,
                move || {
                    Encoder::start(&input, &cfg, choice.adapter, &gpu, move |ev| {
                        !late.load(Ordering::Relaxed) && router.on_event(id, ev)
                    })
                },
            )
            .unwrap_or_else(|e| {
                retired.store(true, Ordering::Relaxed);
                Err(e.into())
            });
            // Frames the platform pushes itself start coming now: a stream of them that
            // cannot start fails as the source, as one inside the encoder does.
            let started = match started {
                Ok(encoder) => match output.attach(&encoder) {
                    Ok(()) => Ok(encoder),
                    Err(e) if e.downcast_ref::<SourceFailed>().is_some() => Err(e),
                    Err(e) => return Err(e),
                },
                Err(e) => Err(e),
            };
            let mut encoder = match started {
                Ok(encoder) => encoder,
                Err(e) if e.downcast_ref::<SourceFailed>().is_some() => {
                    // Whatever of it is still running sends nothing.
                    retired.store(true, Ordering::Relaxed);
                    let text = format!("{e:#}");
                    if source_again || output.health() == OutputHealth::Closed {
                        return Err(
                            e.context(t("pc-worker-run-encoders-the-source-of-the-frames-fails"))
                        );
                    }
                    source_again = true;
                    warn!("{name}: its source failed ({text}); trying it once more");
                    // A timed retry of the same source (on KDE the same PipeWire node:
                    // KWin names it by its id only), in case it failed for a moment
                    // (a node made just now, not yet known to WirePlumber).
                    std::thread::sleep(SOURCE_AGAIN);
                    source = output.source();
                    continue;
                }
                // As one that did not open after it started.
                Err(e) => {
                    let text = format!("{e:#}");
                    warn!("{name}: {choice} did not start: {text}");
                    let failure = NotOpened {
                        short: crate::encoder::short_of_sessions(None, &text),
                        why: text.clone(),
                        resumed: false,
                    };
                    // While encoders are left behind, the sessions held are not known:
                    // no limit is learned.
                    let next = if self.behind.total() == 0 {
                        crate::encoder::catalog().did_not_open(
                            kind,
                            choice.codec,
                            id,
                            &failure,
                            held(kind),
                        )
                    } else {
                        Next::Another
                    };
                    if next == Next::Again && !again {
                        again = true;
                    } else {
                        tried.push((choice, text));
                    }
                    continue;
                }
            };
            let note = crate::encoder::note(&choice, wanted, &ranked.skipped, &tried);
            match &note {
                Some(note) => warn!("{name}: encoding with {note}"),
                None => info!("{name}: encoding with {choice}"),
            }
            encoder.set_choice(&choice, note);
            return Ok(Running {
                encoder,
                retired,
                since: Instant::now(),
            });
        };
        let mut why: Vec<String> = ranked
            .skipped
            .iter()
            .map(|((b, _), skip)| format!("{}: {}", b.label(), skip.text()))
            .chain(tried.iter().map(|(c, e)| format!("{c}: {e}")))
            .collect();
        if why.is_empty() {
            why.push(
                crate::encoder::pick(cfg.codec, wanted.software)
                    .err()
                    .map_or_else(
                        || t("pc-worker-run-encoders-no-encoder-for-this-output").to_owned(),
                        |e| e.to_string(),
                    ),
            );
        }
        bail!("{}", why.join("; "))
    }

    /// Sessions each encoder holds now, those of the screens `except` left out: every
    /// stream that opened, or is opening (it runs, and has not said it did not open).
    pub(super) fn running_encoders(&self, except: &[ScreenId]) -> BTreeMap<Kind, u32> {
        // Those left behind may still hold their sessions.
        let mut running = self.behind.running();
        for (_, s) in self.slots.iter().filter(|(id, _)| !except.contains(id)) {
            if let Some(e) = s.encoder.as_ref().filter(|e| e.opened() || !e.is_paused()) {
                *running.entry(e.kind()).or_insert(0) += 1;
            }
        }
        running
    }

    /// Puts back the size of monitors another program changed (on KDE, sharing one in
    /// Discord or OBS can shrink it to what that program asks for). Each monitor's
    /// [`SizeGuard`] decides; the desktop is laid out again once the size is back.
    /// Window panels are left alone: made anew, one would lose its window.
    pub(super) fn check_sizes(&mut self) {
        // Paused encoders connect again on resuming, which puts the size back too.
        if !self.wanted.as_ref().is_some_and(|w| w.streaming) {
            return;
        }
        let now = Instant::now();
        let ids: Vec<ScreenId> = self
            .slots
            .iter()
            .filter(|(_, s)| matches!(s.made, Made::Monitor { .. }) && s.encoder.is_some())
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            let Some(slot) = self.slots.get_mut(&id) else {
                continue;
            };
            let Some(output) = &slot.output else {
                continue;
            };
            let resized = match output.health() {
                OutputHealth::Resized { w, h } => Some((w, h)),
                _ => None,
            };
            let action = slot.size_guard.check(resized, now);
            let name = output.key().to_string();
            let Made::Monitor { spec, number } = slot.made.clone() else {
                continue;
            };
            let (w, h) = resized.unwrap_or_default();
            match action {
                SizeAction::Wait => {}
                SizeAction::Restored => {
                    info!("{name} is {spec} again");
                    self.layout_due = true;
                }
                // Laid out once the size is back: the desktop still lists the wrong one.
                SizeAction::Restart => {
                    warn!(
                        "{name} was resized to {w}x{h} by another program (screen sharing?): restoring {spec}"
                    );
                    self.restart_same(id);
                    show_sharing_tip(number);
                }
                SizeAction::Recreate => {
                    warn!("{name} is still {w}x{h}: making it again at {spec}");
                    self.recreate(id);
                }
                SizeAction::GiveUp => {
                    warn!("{name} keeps being resized by another program: leaving it at {w}x{h}");
                    let body = format!(
                        "{} {}. {}",
                        t("pc-worker-run-encoders-another-program-keeps-changing-the"),
                        number,
                        t("pc-worker-run-encoders-share-an-app-window-in-it")
                    );
                    dialogs::notify(t("pc-worker-run-encoders-monitor-size"), &body);
                }
            }
        }
    }

    /// Starts screen `id`'s encoder over with the settings it has.
    fn restart_same(&mut self, id: ScreenId) {
        let Some(slot) = self.slots.get_mut(&id) else {
            return;
        };
        if let Some(output) = slot.output.as_mut() {
            output.detach();
        }
        stop_encoder(
            slot.encoder.take(),
            &slot.name,
            &self.shared.steps,
            &self.behind,
        );
        slot.started_for = None;
        self.start(id, None);
    }

    /// Streams whose encoder did not open go on with the next encoder for them, or try
    /// the same once more when it was short of sessions the first time (see
    /// `Catalog::did_not_open`, which learns limits from these). Streams that did not
    /// open together are left out of each other's count of sessions held.
    pub(super) fn retry_unopened(&mut self) {
        let unopened = self.shared.router.take_unopened();
        let ids: Vec<ScreenId> = unopened.iter().map(|(id, _)| *id).collect();
        for (id, failure) in unopened {
            let Some(encoder) = self.slots.get(&id).and_then(|s| s.encoder.as_ref()) else {
                continue;
            };
            let (kind, codec) = (encoder.kind(), encoder.codec());
            let open = self.running_encoders(&ids).get(&kind).copied().unwrap_or(0);
            // While encoders are left behind, the sessions held are not known.
            if self.behind.total() == 0 {
                crate::encoder::catalog().did_not_open(kind, codec, id, &failure, open);
            }
            self.restart_same(id);
        }
        if self.behind.total() > 0 {
            return;
        }
        // Encoders seen holding more sessions than a limit says: it rises.
        let mut opened: BTreeMap<Kind, u32> = BTreeMap::new();
        for e in self.slots.values().filter_map(|s| s.encoder.as_ref()) {
            if e.opened() {
                *opened.entry(e.kind()).or_insert(0) += 1;
            }
        }
        if opened.values().any(|n| *n > 1) {
            let mut catalog = crate::encoder::catalog();
            for (kind, n) in opened {
                catalog.seen_open(kind, n);
            }
        }
    }
}

/// Stops `encoder` (of output `name`) as a step of its own (see `steps`), waiting for
/// that a while at most (one left behind counts in `behind`); nothing of it reaches
/// the headsets from now on.
pub(super) fn stop_encoder(
    encoder: Option<Running>,
    name: &str,
    steps: &Steps,
    behind: &Arc<timed::Behind<Kind>>,
) {
    let Some(running) = encoder else {
        return;
    };
    steps.begin(format!("stopping the encoder of {name}"));
    stop_now(running, name, behind);
}

/// Stops every encoder of `encoders` (each with its output's name) at once, as one
/// step: each driver takes its time to let go, and the outputs go together after,
/// not one after the other. Waits at most as long as for one.
pub(super) fn stop_encoders(
    encoders: Vec<(Running, String)>,
    steps: &Steps,
    behind: &Arc<timed::Behind<Kind>>,
) {
    if encoders.is_empty() {
        return;
    }
    steps.begin(format!("stopping {} encoders", encoders.len()));
    std::thread::scope(|scope| {
        for (running, name) in encoders {
            // The encoder goes in the call, so one without a thread of its own is
            // stopped here instead.
            let encoder = Arc::new(std::sync::Mutex::new(Some((running, name))));
            let taken = encoder.clone();
            let spawned = std::thread::Builder::new()
                .name("encoder-stops".into())
                .spawn_scoped(scope, move || {
                    let mine = taken.lock().unwrap().take();
                    if let Some((running, name)) = mine {
                        stop_now(running, &name, behind);
                    }
                });
            if spawned.is_err() {
                let left = encoder.lock().unwrap().take();
                if let Some((running, name)) = left {
                    stop_now(running, &name, behind);
                }
            }
        }
    });
}

/// Stops `running`, waiting for it at most [`ENCODER_STOP`].
fn stop_now(running: Running, name: &str, behind: &Arc<timed::Behind<Kind>>) {
    let Running {
        encoder, retired, ..
    } = running;
    retired.store(true, Ordering::Relaxed);
    let kind = encoder.kind();
    let _ = timed::call_counted(
        behind,
        kind,
        "encoder-stop",
        &format!("stopping the encoder of {name}"),
        ENCODER_STOP,
        move || {
            #[cfg(feature = "fake-desktop")]
            crate::platform::encoder_stops();
            drop(encoder);
        },
    );
}

/// Whether a screen's encoder starts over for the wanted encoder and codec `cfg`: it
/// started for another (`started_for`), unless that one is what it kept because `cfg`
/// did not start, and it is not time to try `cfg` again (`kept`).
pub(super) fn restart_due(
    started_for: Option<(Backend, Codec)>,
    kept: Option<((Backend, Codec), Instant)>,
    cfg: (Backend, Codec),
    now: Instant,
) -> bool {
    started_for != Some(cfg) && !kept.is_some_and(|(asked, until)| asked == cfg && now < until)
}

/// What a screen's encoding says when it went on in `used` because `asked` did not
/// start: "kept H.264: HEVC did not start".
pub(super) fn kept_note(used: Codec, asked: Codec) -> String {
    let name = crate::encoder::codec_label;
    format!(
        "{} {}: {} {}",
        t("pc-worker-run-encoders-kept"),
        name(used),
        name(asked),
        t("pc-worker-run-encoders-did-not-start")
    )
}
