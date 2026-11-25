//! What can go wrong for the person, in one place: each failure the headset is told
//! about, its code (the headset shows its own text for it, in its language) and the
//! PC's text for it (the log, the tray, the detail under the headset's text).
//!
//! A failure travels as an `anyhow::Error` with a [`UserError`] somewhere in its chain
//! (as the error itself, or as context over what the desktop or GStreamer said);
//! [`outcome`] turns it into the reply.

use prostir_proto::request::Outcome;
use prostir_proto::state::ErrorCode;

use crate::i18n::t;

/// What a display count is of, for [`UserError::Limit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Counted {
    Monitors,
    Windows,
    Physical,
    /// Streams of every kind together, as many as the headset decodes at once.
    Streams,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserError {
    /// This kind of request is not one this PC does (or not for this headset).
    Unsupported,
    /// This headset has no right to it.
    NotAllowed,
    /// No such display (any more).
    UnknownDisplay,
    /// As many as allowed already.
    Limit { max: usize, of: Counted },
    /// A size or scale the PC does not make.
    BadSize { w: u32, h: u32, scale: u16 },
    /// The last monitor stays.
    LastDisplay,
    /// No active window on the PC.
    NoWindow,
    /// The desktop did not do it (an output not made, KScreen stuck).
    Desktop,
    /// The encoder did not start.
    Encoder,
    /// The picture could not be taken.
    Capture,
    /// The sound could not be routed.
    Sound,
    /// The mouse is on none of the displays.
    NotOnDisplays,
    /// An argument out of range.
    BadArgument,
}

impl UserError {
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Unsupported => ErrorCode::UNSUPPORTED,
            Self::NotAllowed => ErrorCode::NOT_ALLOWED,
            Self::UnknownDisplay => ErrorCode::UNKNOWN_DISPLAY,
            Self::Limit { .. } => ErrorCode::LIMIT,
            Self::BadSize { .. } => ErrorCode::BAD_SIZE,
            Self::LastDisplay => ErrorCode::LAST_DISPLAY,
            Self::NoWindow => ErrorCode::NO_WINDOW,
            Self::Desktop => ErrorCode::DESKTOP,
            Self::Encoder => ErrorCode::ENCODER,
            Self::Capture => ErrorCode::CAPTURE,
            Self::Sound => ErrorCode::SOUND,
            Self::NotOnDisplays => ErrorCode::NOT_ON_DISPLAYS,
            Self::BadArgument => ErrorCode::BAD_ARGUMENT,
        }
    }
}

impl std::fmt::Display for UserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported => f.write_str(t("ПК цього не вміє", "The PC cannot do this")),
            Self::NotAllowed => f.write_str(t(
                "Цьому шолому це не дозволено",
                "This headset is not allowed to do this",
            )),
            Self::UnknownDisplay => f.write_str(t("Такого екрана немає", "No such display")),
            Self::Limit { max, of } => {
                let id = match of {
                    Counted::Monitors => "pc-limit-monitors",
                    Counted::Windows => "pc-limit-windows",
                    Counted::Physical => "pc-limit-physical",
                    Counted::Streams => "pc-limit-streams",
                };
                f.write_str(&crate::i18n::count(id, *max))
            }
            Self::BadSize { w, h, scale } => write!(
                f,
                "{} {w}x{h} {scale}%",
                t("ПК не створює монітор", "The PC does not make a monitor of")
            ),
            Self::LastDisplay => f.write_str(t(
                "Останній монітор прибрати не можна",
                "The last monitor cannot be removed",
            )),
            Self::NoWindow => f.write_str(t(
                "Немає активного вікна на ПК",
                "No active window on the PC",
            )),
            Self::Desktop => f.write_str(t(
                "Стільниця ПК цього не зробила",
                "The PC's desktop did not do it",
            )),
            Self::Encoder => {
                f.write_str(t("Кодувальник не запустився", "The encoder did not start"))
            }
            Self::Capture => {
                f.write_str(t("Зображення не знято", "The picture could not be taken"))
            }
            Self::Sound => f.write_str(t("Звук не перемкнувся", "The sound could not be switched")),
            Self::NotOnDisplays => f.write_str(t(
                "Миша не на жодному з екранів",
                "The mouse is on none of the displays",
            )),
            Self::BadArgument => f.write_str(t("Неправильне значення", "A value out of range")),
        }
    }
}

impl std::error::Error for UserError {}

/// The code of what went wrong in `e`: of the [`UserError`] in its chain, else OTHER.
pub fn code_of(e: &anyhow::Error) -> ErrorCode {
    // Finds it as the error itself or as the context of any layer.
    e.downcast_ref::<UserError>()
        .map_or(ErrorCode::OTHER, UserError::code)
}

/// The failed reply for `e`: its code, and the whole text as the detail.
pub fn outcome(e: &anyhow::Error) -> Outcome {
    Outcome::failed(code_of(e), &format!("{e:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;

    /// The code comes from the user error anywhere in the chain; the detail keeps
    /// what lies under it.
    #[test]
    fn the_code_is_found_in_the_chain() {
        let plain = anyhow::anyhow!("kscreen-doctor timed out");
        assert_eq!(code_of(&plain), ErrorCode::OTHER);
        let wrapped = Err::<(), _>(plain).context(UserError::Desktop).unwrap_err();
        assert_eq!(code_of(&wrapped), ErrorCode::DESKTOP);
        let deeper = Err::<(), _>(anyhow::anyhow!("x"))
            .context(UserError::Capture)
            .context("taking a screenshot")
            .unwrap_err();
        assert_eq!(code_of(&deeper), ErrorCode::CAPTURE, "under another layer");
        let Outcome::Failed { error, detail } = outcome(&wrapped) else {
            panic!("a failure")
        };
        assert_eq!(error, ErrorCode::DESKTOP);
        assert!(detail.contains("kscreen-doctor timed out"), "{detail}");
        let limit = anyhow::Error::new(UserError::Limit {
            max: 6,
            of: Counted::Monitors,
        });
        assert_eq!(code_of(&limit), ErrorCode::LIMIT);
        assert!(format!("{limit}").contains('6'));
    }
}
