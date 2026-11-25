//! The numbers of "For development" next to the monitor looked at (the preview build
//! only): worked out, put in words and drawn on a label's picture. A frame takes no
//! memory for them: the words are made once for a language with a mark where a number
//! goes, the numbers are written in between into a text kept from frame to frame, and
//! the picture is drawn again (from a pill kept ready, on a pixmap kept) only when that
//! text changed.

use std::fmt::{Arguments, Write as _};
use std::time::{Duration, Instant};

use tiny_skia::Pixmap;

use crate::app::Model;
use crate::clarity::Metrics;
use crate::ids::DisplayKey;
use crate::lang::Texts;
use crate::layers::METRICS_PX;
use crate::ui::{self, Panel};

/// How often the stream's rate is worked out anew.
const RATE_EVERY: Duration = Duration::from_secs(1);

/// What stands in a text for the number, until the number is written there. A private
/// use character: no text has it.
const MARK: &str = "\u{E000}";

/// One text with a number in it, cut at where the number goes.
struct Words {
    before: String,
    after: String,
    /// The text has its place for the number (a language file may leave it out).
    holds: bool,
}

impl Words {
    fn new(texts: &Texts, id: &str, arg: &str) -> Self {
        let full = texts.f(id, &[(arg, MARK.into())]);
        match full.split_once(MARK) {
            Some((before, after)) => Self {
                before: before.to_owned(),
                after: after.to_owned(),
                holds: true,
            },
            None => Self {
                before: full,
                after: String::new(),
                holds: false,
            },
        }
    }

    /// The text with `number` in its place, after a separator where something is
    /// written already.
    fn write(&self, out: &mut String, number: Arguments<'_>) {
        if !out.is_empty() {
            out.push_str(" · ");
        }
        out.push_str(&self.before);
        if self.holds {
            let _ = out.write_fmt(number);
            out.push_str(&self.after);
        }
    }
}

/// The three texts of a language.
struct Sentences {
    texts: Texts,
    scale: Words,
    rate: Words,
    keyframe: Words,
}

impl Sentences {
    fn new(texts: &Texts) -> Self {
        Self {
            texts: texts.clone(),
            scale: Words::new(texts, "headset-dev-metrics-scale", "scale"),
            rate: Words::new(texts, "headset-dev-metrics-rate", "rate"),
            keyframe: Words::new(texts, "headset-dev-metrics-keyframe", "size"),
        }
    }

    /// `m` in words, one line, into `out` (emptied first).
    fn write(&self, m: &Metrics, out: &mut String) {
        out.clear();
        if let Some(scale) = m.scale {
            self.scale.write(out, format_args!("{scale:.2}"));
        }
        if let Some(mbit) = m.mbit {
            self.rate.write(out, format_args!("{mbit:.1}"));
        }
        if let Some(bytes) = m.keyframe {
            self.keyframe
                .write(out, format_args!("{}", bytes.div_ceil(1024)));
        }
    }
}

/// The label: what it was worked out from, what it says, and its picture.
pub struct Label {
    sentences: Option<Sentences>,
    /// What the picture says, and the text being made (kept for its memory).
    drawn: String,
    next: String,
    /// The stream's bytes last counted: the monitor, when, how many.
    count: Option<(DisplayKey, Instant, u64)>,
    /// Megabits a second over the last [`RATE_EVERY`] (`None`: not yet).
    mbit: Option<f32>,
    /// The empty pill, and the picture: the pill with the text.
    pill: Pixmap,
    picture: Pixmap,
    /// Something was drawn.
    ready: bool,
}

impl Label {
    pub fn new() -> Self {
        let pill = ui::toast_pill(METRICS_PX.0, METRICS_PX.1);
        Self {
            sentences: None,
            drawn: String::with_capacity(256),
            next: String::with_capacity(256),
            count: None,
            mbit: None,
            picture: pill.clone(),
            pill,
            ready: false,
        }
    }

    /// A picture was drawn (what the layer shows, once it is uploaded).
    pub fn ready(&self) -> bool {
        self.ready
    }

    /// What the label says now.
    pub fn text(&self) -> &str {
        &self.drawn
    }

    /// The numbers of the monitor looked at (`Model::metrics_for`): its scale, the
    /// megabits a second of its stream over the last second, its last keyframe's size.
    /// `received` says how many bytes came of a monitor's stream and how big its last
    /// keyframe was. The new picture (premultiplied RGBA, top row first) when what the
    /// label says changed; `None` when it stays as it was. Memory is taken only when
    /// the language changes, and for the glyphs of a letter or a size not drawn before.
    pub fn update(
        &mut self,
        panel: &mut Panel,
        model: &Model,
        received: impl FnOnce(DisplayKey) -> Option<(u64, Option<u32>)>,
        now: Instant,
    ) -> Option<&[u8]> {
        let Some(id) = model.metrics_for() else {
            self.count = None;
            return None;
        };
        let (bytes, keyframe) = received(id).unwrap_or((0, None));
        match self.count {
            Some((was, at, before)) if was == id => {
                let passed = now.saturating_duration_since(at);
                if passed >= RATE_EVERY {
                    let bits = bytes.saturating_sub(before) as f64 * 8.0;
                    self.mbit = Some((bits / passed.as_secs_f64() / 1e6) as f32);
                    self.count = Some((id, now, bytes));
                }
            }
            _ => {
                self.count = Some((id, now, bytes));
                self.mbit = None;
            }
        }
        let texts = model.texts();
        if self.sentences.as_ref().is_none_or(|s| s.texts != *texts) {
            self.sentences = Some(Sentences::new(texts));
        }
        let sentences = self.sentences.as_ref()?;
        sentences.write(
            &Metrics {
                scale: model.world().shown_scale(id),
                mbit: self.mbit,
                keyframe,
            },
            &mut self.next,
        );
        if self.ready && self.next == self.drawn {
            return None;
        }
        std::mem::swap(&mut self.drawn, &mut self.next);
        if !self.ready {
            // The first picture: every digit is drawn once, so that no number made
            // later has a glyph to make.
            for digit in '0'..='9' {
                self.next.clear();
                self.next.extend(
                    self.drawn
                        .chars()
                        .map(|c| if c.is_ascii_digit() { digit } else { c }),
                );
                self.picture.data_mut().copy_from_slice(self.pill.data());
                panel.write_line(&mut self.picture, &self.next);
            }
        }
        self.picture.data_mut().copy_from_slice(self.pill.data());
        panel.write_line(&mut self.picture, &self.drawn);
        self.ready = true;
        Some(self.picture.data())
    }
}

impl Default for Label {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The numbers in words by Fluent, each time anew: what the words made once for a
    /// language must come to.
    fn by_fluent(texts: &Texts, m: &Metrics) -> String {
        let mut parts = Vec::new();
        if let Some(scale) = m.scale {
            parts.push(texts.f(
                "headset-dev-metrics-scale",
                &[("scale", format!("{scale:.2}").into())],
            ));
        }
        if let Some(mbit) = m.mbit {
            parts.push(texts.f(
                "headset-dev-metrics-rate",
                &[("rate", format!("{mbit:.1}").into())],
            ));
        }
        if let Some(bytes) = m.keyframe {
            parts.push(texts.f(
                "headset-dev-metrics-keyframe",
                &[("size", (bytes.div_ceil(1024)).into())],
            ));
        }
        parts.join(" · ")
    }

    /// The numbers in words, each where it is known, as Fluent has them in every
    /// language there is.
    #[test]
    fn the_numbers_in_words() {
        for code in ["en", "uk"] {
            let texts = crate::lang::builtin(code);
            let words = Sentences::new(&texts);
            let mut out = String::new();
            for scale in [None, Some(0.0), Some(1.004), Some(12.5)] {
                for mbit in [None, Some(0.04), Some(31.46), Some(1234.5)] {
                    for keyframe in [None, Some(1), Some(400_000), Some(3_000_000)] {
                        let m = Metrics {
                            scale,
                            mbit,
                            keyframe,
                        };
                        words.write(&m, &mut out);
                        assert_eq!(out, by_fluent(&texts, &m), "{code} {m:?}");
                    }
                }
            }
        }
        let texts = crate::lang::builtin("en");
        let all = Metrics {
            scale: Some(1.004),
            mbit: Some(31.46),
            keyframe: Some(400_000),
        };
        let mut out = String::new();
        Sentences::new(&texts).write(&all, &mut out);
        assert!(out.contains("1.00"), "{out}");
        assert!(out.contains("31.5"), "{out}");
        assert!(out.contains("391"), "{out}");
    }
}
