//! How the screens encode: the codec for the headsets there are, displays too big for
//! a headset, and encoders that failed or did not open.

use super::*;

/// A monitor that failed this many times in a row no longer counts as on its way
/// (see [`Displays::healthy`]); it is still tried again.
const GIVE_UP_AFTER: u32 = 3;

impl Displays {
    /// Monitor `id` is tried again (after a failure): `Starting` until the worker
    /// says what came of it.
    pub(super) fn try_again(&mut self, id: ScreenId) {
        if let Some(s) = self.screens.get_mut(&id) {
            s.attempt += 1;
            s.retry_at = None;
            s.state = ScreenState::Starting;
        }
    }

    /// Tries again each monitor that failed once its pause is over (see
    /// `screen::retry_pause`); the others go on meanwhile. Whether one was.
    pub(super) fn retry_due(&mut self, now: Instant) -> bool {
        let due: Vec<ScreenId> = self
            .monitors()
            .filter(|s| matches!(s.state, ScreenState::Failed { .. }))
            .filter(|s| s.retry_at.is_some_and(|t| t <= now))
            .filter(|s| !self.too_big.contains_key(&s.id))
            .map(|s| s.id)
            .collect();
        for id in &due {
            if let Some(s) = self.screens.get(id) {
                info!(
                    "{}: trying it again (failure {} in a row)",
                    s.name(),
                    s.failures
                );
            }
            self.try_again(*id);
        }
        !due.is_empty()
    }

    /// Displays too big for the last headset or codec are tried again where they fit
    /// the headset and codec now; the others stay failed, without a start that would
    /// only fail again.
    pub(super) fn retry_too_big(&mut self) {
        let codec = self.cfg.codec;
        let fit: Vec<ScreenId> = self
            .too_big
            .iter()
            .filter(|(_, (size, _))| self.router.fits(codec, *size))
            .map(|(id, _)| *id)
            .collect();
        for id in fit {
            self.too_big.remove(&id);
            if self.is_physical(id) {
                // Shown again once the PC's displays are looked at.
                if let Some(s) = self.screens.get_mut(&id) {
                    s.state = ScreenState::Starting;
                }
                self.desk.ask(Job::LookPhysical);
            } else {
                self.try_again(id);
            }
        }
    }

    /// Tries again the monitors that could not start; those that still cannot stay
    /// `Failed` in the list the headset gets.
    pub(super) fn retry_failed(&mut self) {
        let failed: Vec<ScreenId> = self
            .monitors()
            .filter(|s| matches!(s.state, ScreenState::Failed { .. }))
            .filter(|s| !self.too_big.contains_key(&s.id))
            .map(|s| s.id)
            .collect();
        for id in failed {
            // Tried at once, and counted from the first pause again.
            if let Some(s) = self.screens.get_mut(&id) {
                s.failures = 0;
            }
            self.try_again(id);
        }
    }

    /// How each screen that streams encodes, and what to say of it when it is not the
    /// best this PC has for it (see `encoder::note`): for the tray and the local API.
    pub fn encoding_report(&self) -> Vec<OutputEncoding> {
        self.screens
            .values()
            .filter_map(|s| {
                let e = s.encoding.as_ref()?;
                Some(OutputEncoding {
                    id: s.id,
                    name: s.name(),
                    encoder: e.encoder,
                    codec: e.codec,
                    hardware: e.hardware,
                    copies: e.copies,
                    note: e.note.clone(),
                    stats: self.router.stream_stats(s.id),
                })
            })
            .collect()
    }

    /// The tray's line about encoding, when a display is not on the best encoder for it
    /// or encodes on the CPU: the first such, and how many more.
    pub fn encoding_line(&self) -> Option<String> {
        let noted: Vec<OutputEncoding> = self
            .encoding_report()
            .into_iter()
            .filter(|o| o.note.is_some())
            .collect();
        let first = noted.first()?;
        let mut line = format!("{}: {}", first.name, first.note.as_deref().unwrap_or(""));
        if noted.len() > 1 {
            let _ = write!(line, " (+{} {})", noted.len() - 1, t("ще", "more"));
        }
        Some(state::clip(&line, 120))
    }

    /// Stops the streams larger than every headset decodes (see [`Router::attach`]):
    /// a monitor or a display of the PC stands `Failed` with `BAD_SIZE` and why, and
    /// is tried again with the next headset or codec; a window panel closes, its
    /// window back where it was, and the headsets hear why. One that some headset
    /// decodes goes on for it, failed only for the others (see `displays_part`).
    pub fn stop_too_big(&mut self) {
        for (id, (w, h), codec) in self.router.take_too_big() {
            let Some(s) = self.screens.get(&id) else {
                continue;
            };
            if !self.router.still_too_big(id) {
                continue;
            }
            let why = too_big_why((w, h), codec);
            warn!("{}: {why}", s.name());
            if s.is_window() {
                // Said with no display: its name says which.
                let named = format!("{}: {why}", s.name());
                self.close_window_panel(id);
                self.router.send_all(&HostMsg::Problem {
                    error: state::ErrorCode::BAD_SIZE,
                    detail: state::clip(&named, state::MAX_DETAIL),
                    display: None,
                });
                continue;
            }
            if s.is_physical() {
                self.stop_physical(id);
            }
            if let Some(s) = self.screens.get_mut(&id) {
                s.encoding = None;
                s.state = ScreenState::Failed {
                    error: state::ErrorCode::BAD_SIZE,
                    why,
                };
            }
            self.too_big.insert(id, ((w, h), codec));
        }
    }

    /// Whether display `id` waits for a headset or codec it fits.
    pub(super) fn is_too_big(&self, id: ScreenId) -> bool {
        self.too_big.contains_key(&id)
    }

    /// The first display streaming now that `headset` would not decode in `codec`:
    /// its size.
    /// Streams that encode in `codec` already (a codec switch fitted down for another
    /// headset, or kept because the one asked for did not start) stay as they are, so
    /// they are left out.
    pub fn too_big_for(&self, codec: Codec, headset: &state::HeadsetCaps) -> Option<(u32, u32)> {
        self.screens
            .values()
            .filter(|s| s.encoding.as_ref().is_some_and(|e| e.codec != codec))
            .filter_map(|s| {
                self.router
                    .stream_size(s.id)
                    .or_else(|| s.kind.spec().map(|m| (m.width, m.height)))
            })
            .find(|&(w, h)| !headset.fits(codec, w, h))
    }

    /// The codec the streams are in now.
    pub fn codec(&self) -> Codec {
        self.cfg.codec
    }

    /// What the encoders make now: the codec, and whether on the CPU.
    pub fn encoding_now(&self) -> (Codec, bool) {
        (self.cfg.codec, self.cfg.backend == Backend::X264)
    }

    /// The streams go in a codec every headset with a session (`headsets`) decodes:
    /// one in a codec some headset does not decode (a choice saved with another
    /// headset) starts over in H.264, which every headset decodes. The choice stays
    /// saved, and the streams go back to it once every headset decodes it again.
    pub fn fit_codec(&mut self, headsets: &[state::HeadsetCaps]) {
        if headsets.is_empty() {
            return;
        }
        // The one chosen where it was put aside for a headset, else what encodes now.
        let base = if self.fitted_down {
            (self.settings.codec, self.settings.software)
        } else {
            self.encoding_now()
        };
        if let Err(e) = self.encode_for(base.0, base.1, headsets) {
            warn!("{e:#}");
        }
    }

    /// The codec the streams go in when `codec` is chosen and the headsets with a
    /// session are `headsets`: it, where every one of them decodes it, else H.264.
    pub fn codec_for(codec: Codec, headsets: &[state::HeadsetCaps]) -> Codec {
        if codec == Codec::H264 || headsets.iter().all(|h| h.decodes(codec)) {
            codec
        } else {
            Codec::H264
        }
    }

    /// Encodes `codec` (on the CPU with `software`) as far as `headsets` decode it
    /// (see [`Displays::codec_for`]), with one switch at most; the choice stays
    /// `codec`, and the streams go back to it once every headset decodes it.
    pub fn encode_for(
        &mut self,
        codec: Codec,
        software: bool,
        headsets: &[state::HeadsetCaps],
    ) -> Result<()> {
        let fits = Self::codec_for(codec, headsets);
        let now = self.encoding_now().0;
        if fits != codec && now != fits {
            info!("a headset does not decode {codec:?}: streaming H.264");
        } else if self.fitted_down && fits == codec && now != fits {
            info!("every headset decodes {codec:?}: streaming it again");
        }
        self.fitted_down = fits != codec;
        self.encode_with(fits, software)
    }

    /// Another codec or the CPU encoder, asked for in the headset menu: every stream
    /// starts over with what the PC can make of it (the monitors and their windows
    /// stay), once the desktop's worker gets to it. Nothing changes when that is what
    /// encodes now.
    pub fn encode_with(&mut self, codec: Codec, software: bool) -> Result<()> {
        if (codec, software) == self.encoding_now() {
            return Ok(());
        }
        let (backend, codec) = crate::encoder::pick(codec, software).context(UserError::Encoder)?;
        // The same encoder in the same codec (VA-API without H.265 asked for H.265
        // while it makes H.264): nothing starts over.
        if (backend, codec) == (self.cfg.backend, self.cfg.codec) {
            return Ok(());
        }
        info!("encoding with {backend:?} {codec:?} (from the headset menu)");
        self.cfg.backend = backend;
        self.cfg.codec = codec;
        // Displays too big for the codec before may fit this one.
        self.retry_too_big();
        Ok(())
    }

    /// Why no monitor could start, when none streams or is on its way: the first
    /// failure.
    pub fn why_none_started(&self) -> Option<String> {
        if self
            .monitors()
            .any(|s| !matches!(s.state, ScreenState::Failed { .. }))
        {
            return None;
        }
        self.monitors().find_map(|s| match &s.state {
            ScreenState::Failed { why, .. } => Some(format!("{}: {why}", s.name())),
            _ => None,
        })
    }

    /// Some monitor streams or is on its way: one that broke or did not start is
    /// tried again by itself (see [`Displays::retry_due`]), and only when none can
    /// stream at all (every one failed [`GIVE_UP_AFTER`] times in a row) do the
    /// sessions end, the screens made anew for the next one.
    pub fn healthy(&self) -> bool {
        // Monitors too big for this headset are not broken: they wait for another.
        self.monitors().any(|s| {
            s.running()
                || matches!(s.state, ScreenState::Starting)
                || self.too_big.contains_key(&s.id)
                || (s.retry_at.is_some() && s.failures < GIVE_UP_AFTER)
        })
    }
}

/// Why a display does not stream to a headset: its picture of `w`x`h` is larger than
/// the headset decodes in `codec`. Without the display's name: it goes with the
/// display, which the reader sees named already.
pub(super) fn too_big_why((w, h): (u32, u32), codec: Codec) -> String {
    format!(
        "{w}x{h} {} {}",
        t(
            "більше, ніж шолом декодує в",
            "is larger than the headset decodes in"
        ),
        crate::encoder::codec_label(codec)
    )
}
