//! What the open menu says by the button that was pressed: that it is done, that it
//! waits for the PC, or why it failed. One note at a time, under the button it is
//! about (`ui::Panel` places it; a note no button of the page stands for goes on the
//! menu's bottom line). It is short-lived, goes when another button is pressed or the
//! page changes, and is said only while the menu is open: while it is closed, the
//! same news is a toast of its own (see [`Told::toast`]).
//!
//! Plain data with the time passed in, so the rules are tested on the PC.

#![forbid(unsafe_code)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use prostir_proto::request::Outcome;

use crate::lang::Texts;
use crate::pending::{ANSWER_WITHIN, FAILURE_SHOWN_FOR};
use crate::ui::{Action, Asked, Failure};

/// How long a note that something is done stays.
pub const DONE_SHOWN_FOR: Duration = Duration::from_secs(4);

/// What a note says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Done,
    /// Asked of the PC, no answer yet.
    Waiting,
    Failed,
}

/// A line under a button of the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    /// The button it is said under; none: on the menu's bottom line (news that came
    /// without a press, such as a pairing that went through).
    pub on: Option<Action>,
    pub kind: Kind,
    /// Made once, when the news comes: the menu compares and copies it every frame
    /// without allocating.
    pub text: Arc<str>,
    /// A second, smaller line: the PC's technical detail under a failure.
    pub detail: Option<Arc<str>>,
}

impl Note {
    pub fn done(on: Option<Action>, text: &str) -> Self {
        Self {
            on,
            kind: Kind::Done,
            text: text.into(),
            detail: None,
        }
    }

    /// Asked of the PC: wait.
    pub fn waiting(on: Option<Action>, texts: &Texts) -> Self {
        Self {
            on,
            kind: Kind::Waiting,
            text: texts.t("ask-waiting").into(),
            detail: None,
        }
    }

    /// It did not happen: `text` says why.
    pub fn failed(on: Option<Action>, text: &str) -> Self {
        Self {
            on,
            kind: Kind::Failed,
            text: text.into(),
            detail: None,
        }
    }

    /// A request failed for `why`: its text, and the PC's `detail` under it; only the
    /// detail where the failure has no text of its own.
    pub fn failure(on: Option<Action>, why: Failure, detail: &str, texts: &Texts) -> Self {
        let (text, detail) = match why.text() {
            Some(id) => (texts.t(id), (!detail.is_empty()).then(|| detail.into())),
            None => (detail, None),
        };
        Self {
            detail,
            ..Self::failed(on, text)
        }
    }

    /// How long it stays: a wait until the answer comes (or the request is given up
    /// on), a failure long enough to read its reason.
    fn shown_for(&self) -> Duration {
        match self.kind {
            Kind::Done => DONE_SHOWN_FOR,
            Kind::Waiting => ANSWER_WITHIN,
            Kind::Failed => FAILURE_SHOWN_FOR,
        }
    }
}

/// A note, and whether it is worth a toast of its own while the menu is closed (a
/// failure, or news the user waits for, such as a saved screenshot).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Told {
    pub note: Note,
    pub toast: bool,
}

/// The menu's notes.
#[derive(Debug, Default)]
pub struct Notes {
    /// The note shown, and until when.
    shown: Option<(Note, Instant)>,
    /// The button pressed last: requests sent now are its.
    pressed: Option<Action>,
    /// Which button asked each request still waited for, to say its answer under it.
    asked_by: Vec<(Asked, Action)>,
}

impl Notes {
    /// Button `action` was pressed (in the menu, or by a command of the PC as the menu
    /// would): what was said before goes, and requests sent now are this button's.
    pub fn press(&mut self, action: Action) {
        self.shown = None;
        self.pressed = Some(action);
    }

    /// The button pressed last: a failure before anything was sent is said under it.
    pub fn pressed(&self) -> Option<Action> {
        self.pressed
    }

    /// Control `what` asked the PC for something: the button pressed for it (none
    /// when nothing was pressed), whose note says the answer.
    pub fn asked(&mut self, what: Asked) -> Option<Action> {
        self.asked_by.retain(|(w, _)| *w != what);
        let on = self.pressed?;
        self.asked_by.push((what, on));
        Some(on)
    }

    /// The request of control `what` was answered, failed or given up on: the button
    /// that asked it, if one did.
    pub fn answered(&mut self, what: Asked) -> Option<Action> {
        let at = self.asked_by.iter().position(|(w, _)| *w == what)?;
        Some(self.asked_by.swap_remove(at).1)
    }

    /// Says `note` from `now`, in place of what was said.
    pub fn say(&mut self, note: Note, now: Instant) {
        let until = now + note.shown_for();
        self.shown = Some((note, until));
    }

    /// Another page of the menu, or the menu opened afresh: nothing said stays.
    pub fn clear(&mut self) {
        self.shown = None;
    }

    /// A note shown long enough goes.
    pub fn expire(&mut self, now: Instant) {
        if self.shown.as_ref().is_some_and(|(_, until)| now >= *until) {
            self.shown = None;
        }
    }

    pub fn shown(&self) -> Option<&Note> {
        self.shown.as_ref().map(|(note, _)| note)
    }
}

/// What the menu says when a button that does it itself (not the PC) is pressed and
/// what it changed does not show on the button: the text's id. None for a switch or
/// a choice, which shows how it stands on its own.
pub fn done_text(action: Action) -> Option<&'static str> {
    Some(match action {
        Action::Add { .. } => "note-monitor-added",
        Action::Remove(_) => "note-monitor-removed",
        Action::CloseWindow(_) => "note-window-closed",
        Action::MakePrimary(_) => "note-primary",
        Action::SetMirror(true) => "note-display-shown",
        Action::SetMirror(false) => "note-display-hidden",
        Action::SetAudio(true) => "note-sound-headset",
        Action::SetAudio(false) => "note-sound-pc",
        Action::SetEncoding(_) => "note-encoding",
        Action::DetachWindow => "note-window-detached",
        Action::WindowHere => "note-window-here",
        Action::Preset(_) => "note-layout",
        Action::Tidy => "note-lined-up",
        Action::Recenter => "note-in-front",
        _ => return None,
    })
}

/// What the menu says when the request of control `what`, asked by button `on`, is
/// answered with `outcome`. A profile switched to is said by the caller, which knows
/// whether the switch still stands.
pub fn answer(what: Asked, on: Option<Action>, outcome: &Outcome, texts: &Texts) -> Option<Told> {
    Some(match outcome {
        Outcome::Failed { error, detail } => Told {
            note: Note::failure(on, Failure::Pc(*error), detail, texts),
            toast: true,
        },
        Outcome::Saved { path } => Told {
            note: Note::done(
                on,
                &texts.f("toast-screenshot-saved", &[("path", path.into())]),
            ),
            toast: true,
        },
        // The mouse found shows itself.
        Outcome::Pointer { .. } => return None,
        Outcome::Ids { .. } if matches!(what, Asked::Profile(_)) => return None,
        Outcome::Done | Outcome::Added { .. } | Outcome::Ids { .. } => Told {
            note: Note::done(on, texts.t(on.and_then(done_text)?)),
            toast: false,
        },
    })
}

/// What the menu says when the microphone switch is set to `on`, with the system's
/// `permission` as it stands (see `prostir_proto::state::permission`).
pub fn mic_note(on: bool, permission: u8, texts: &Texts) -> Note {
    use prostir_proto::state::permission;
    let button = Some(Action::SetMic(on));
    if !on {
        return Note::done(button, texts.t("note-mic-pc"));
    }
    match permission {
        permission::GRANTED => Note::done(button, texts.t("note-mic-headset")),
        permission::DENIED => Note::failed(button, texts.t("note-mic-denied")),
        _ => Note::done(button, texts.t("note-mic-ask")),
    }
}

/// What the menu says when the system's microphone permission changed to
/// `permission` while the switch is on; nothing for one not decided yet.
pub fn permission_note(permission: u8, texts: &Texts) -> Option<Note> {
    use prostir_proto::state::permission;
    let button = Some(Action::SetMic(true));
    match permission {
        permission::GRANTED => Some(Note::done(button, texts.t("note-mic-allowed"))),
        permission::DENIED => Some(Note::failed(button, texts.t("note-mic-denied"))),
        _ => None,
    }
}

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "a test's own starting time; the code under test takes the time as a parameter"
)]
mod tests {
    use super::*;
    use crate::ids::{DisplayKey, PcId};
    use crate::settings::Preset;
    use prostir_proto::state::{DisplayId, ErrorCode, permission};

    fn k(n: u32) -> DisplayKey {
        DisplayKey::new(PcId([3; 8]), DisplayId(n))
    }

    /// A press of `action` that asks the PC as control `what`, the way the menu does
    /// it: the old note goes, the button waits under itself.
    fn ask(notes: &mut Notes, action: Action, what: Asked, texts: &Texts, now: Instant) {
        notes.press(action);
        let on = notes.asked(what);
        assert_eq!(on, Some(action));
        notes.say(Note::waiting(on, texts), now);
    }

    /// Adding a monitor: wait under the size pressed, then "added" under it, then
    /// gone after a few seconds; failing, the reason and the PC's detail, longer.
    #[test]
    fn a_monitor_added_or_not() {
        let texts = crate::lang::builtin("uk");
        let t = Instant::now();
        let mut notes = Notes::default();
        let add = Action::Add {
            width: 2560,
            height: 1440,
        };
        ask(&mut notes, add, Asked::AddMonitor, &texts, t);
        let shown = notes.shown().expect("waiting");
        assert_eq!(
            (shown.on, shown.kind, &*shown.text),
            (Some(add), Kind::Waiting, "Зачекайте…")
        );
        let on = notes.answered(Asked::AddMonitor);
        assert_eq!(on, Some(add));
        let told = answer(
            Asked::AddMonitor,
            on,
            &Outcome::Added { id: DisplayId(5) },
            &texts,
        )
        .expect("said");
        assert!(
            !told.toast,
            "the monitor shows itself while the menu is closed"
        );
        notes.say(told.note, t);
        let shown = notes.shown().expect("done");
        assert_eq!(
            (shown.on, shown.kind, &*shown.text),
            (Some(add), Kind::Done, "Монітор додано")
        );
        notes.expire(t + DONE_SHOWN_FOR - Duration::from_millis(1));
        assert!(notes.shown().is_some());
        notes.expire(t + DONE_SHOWN_FOR);
        assert_eq!(notes.shown(), None);

        // Failed: the reason under the button, the PC's detail under it.
        ask(&mut notes, add, Asked::AddMonitor, &texts, t);
        let on = notes.answered(Asked::AddMonitor);
        let failed = Outcome::failed(ErrorCode::LIMIT, "at most 6");
        let told = answer(Asked::AddMonitor, on, &failed, &texts).expect("said");
        assert!(told.toast, "a failure is told while the menu is closed too");
        notes.say(told.note, t);
        let shown = notes.shown().expect("failed");
        assert_eq!(shown.kind, Kind::Failed);
        assert_eq!(shown.on, Some(add));
        assert_eq!(&*shown.text, "Досягнуто межі");
        assert_eq!(shown.detail.as_deref(), Some("at most 6"));
        notes.expire(t + DONE_SHOWN_FOR);
        assert!(notes.shown().is_some(), "a reason stays longer");
        notes.expire(t + FAILURE_SHOWN_FOR);
        assert_eq!(notes.shown(), None);
    }

    /// The answer goes under the button that asked, even after another button was
    /// pressed meanwhile; a code without a text of its own says the PC's detail.
    #[test]
    fn an_answer_finds_its_button() {
        let texts = crate::lang::builtin("en");
        let t = Instant::now();
        let mut notes = Notes::default();
        let remove = Action::Remove(k(102));
        ask(&mut notes, remove, Asked::Remove(k(102)), &texts, t);
        // A switch shows its own state: pressing it only takes the note away.
        notes.press(Action::Follow(true));
        assert_eq!(notes.shown(), None);
        let on = notes.answered(Asked::Remove(k(102)));
        assert_eq!(on, Some(remove));
        let told = answer(Asked::Remove(k(102)), on, &Outcome::Done, &texts).expect("said");
        assert_eq!(&*told.note.text, "Monitor removed");
        assert_eq!(told.note.on, Some(remove));
        assert_eq!(notes.answered(Asked::Remove(k(102))), None, "answered once");
        let other = Outcome::failed(ErrorCode::OTHER, "the desktop is busy");
        let told = answer(
            Asked::Mirror,
            Some(Action::SetMirror(false)),
            &other,
            &texts,
        )
        .expect("said");
        assert_eq!(&*told.note.text, "the desktop is busy");
        assert_eq!(told.note.detail, None);
        // Not asked from a button (a command of the PC): nothing to say when it is
        // done, the failure on the bottom line.
        let mut notes = Notes::default();
        assert_eq!(notes.asked(Asked::Sound), None);
        assert_eq!(notes.answered(Asked::Sound), None);
        assert_eq!(answer(Asked::Sound, None, &Outcome::Done, &texts), None);
        let failed = Outcome::failed(ErrorCode::SOUND, "");
        let told = answer(Asked::Sound, None, &failed, &texts).expect("said");
        assert_eq!(
            (told.note.on, &*told.note.text),
            (None, "The sound could not be switched")
        );
    }

    /// The PC's answers with a text of their own: a screenshot's path (a toast too
    /// while the menu is closed); the mouse found and a profile say nothing here.
    #[test]
    fn answers_with_their_own_text() {
        let texts = crate::lang::builtin("uk");
        let saved = Outcome::Saved {
            path: "~/Pictures/shot.png".into(),
        };
        let told =
            answer(Asked::Screenshot, Some(Action::Screenshot), &saved, &texts).expect("said");
        assert!(told.toast);
        assert_eq!(&*told.note.text, "Знімок збережено: ~/Pictures/shot.png");
        let pointer = Outcome::Pointer {
            id: DisplayId(1),
            u: 0,
            v: 0,
        };
        assert_eq!(answer(Asked::FindMouse, None, &pointer, &texts), None);
        let ids = Outcome::Ids { ids: Vec::new() };
        let profile = Some(Action::ApplyProfile(1));
        assert_eq!(answer(Asked::Profile(1), profile, &ids, &texts), None);
    }

    /// The menu's own actions: a new layout and lining up say so; a switch does not.
    #[test]
    fn what_needs_saying() {
        assert_eq!(done_text(Action::Preset(Preset::Wall)), Some("note-layout"));
        assert_eq!(done_text(Action::Tidy), Some("note-lined-up"));
        assert_eq!(done_text(Action::SetAudio(false)), Some("note-sound-pc"));
        for quiet in [
            Action::Focus(true),
            Action::Follow(false),
            Action::Tab(crate::ui::Tab::Quick),
            Action::SelectMonitor(k(1)),
        ] {
            assert_eq!(done_text(quiet), None, "{quiet:?}");
        }
        // Every text named is in both languages.
        for lang in ["uk", "en"] {
            let texts = crate::lang::builtin(lang);
            for action in [
                Action::Add {
                    width: 1,
                    height: 1,
                },
                Action::Remove(k(1)),
                Action::CloseWindow(k(1)),
                Action::MakePrimary(k(1)),
                Action::SetMirror(true),
                Action::SetMirror(false),
                Action::SetAudio(true),
                Action::SetAudio(false),
                Action::SetEncoding(crate::ui::EncodingChange::default()),
                Action::DetachWindow,
                Action::WindowHere,
                Action::Preset(Preset::Arc),
                Action::Tidy,
                Action::Recenter,
            ] {
                let id = done_text(action).expect("a text");
                assert_ne!(texts.t(id), id, "{lang}: {id}");
            }
        }
    }

    /// The microphone switch says what the PC's apps hear, and whether the system
    /// lets the headset record; a refusal is a failure under the switch.
    #[test]
    fn the_microphone_says_how_it_stands() {
        let texts = crate::lang::builtin("en");
        let note = mic_note(true, permission::GRANTED, &texts);
        assert_eq!(
            (note.on, note.kind, &*note.text),
            (
                Some(Action::SetMic(true)),
                Kind::Done,
                "PC apps hear the headset microphone"
            )
        );
        let note = mic_note(true, permission::DENIED, &texts);
        assert_eq!(note.kind, Kind::Failed);
        assert_eq!(&*note.text, "No access to the microphone");
        let note = mic_note(true, permission::UNKNOWN, &texts);
        assert_eq!(note.kind, Kind::Done);
        assert_eq!(
            &*mic_note(false, permission::DENIED, &texts).text,
            "PC apps hear the PC microphone"
        );
        assert_eq!(
            permission_note(permission::DENIED, &texts).map(|n| n.kind),
            Some(Kind::Failed)
        );
        assert_eq!(
            permission_note(permission::GRANTED, &texts).map(|n| n.kind),
            Some(Kind::Done)
        );
        assert_eq!(permission_note(permission::UNKNOWN, &texts), None);
    }

    /// A note goes with another page, or when the menu opens afresh; a failure with
    /// a text of its own keeps the PC's detail under it, an empty detail is no line.
    #[test]
    fn pages_and_failures() {
        let texts = crate::lang::builtin("uk");
        let t = Instant::now();
        let mut notes = Notes::default();
        let save = Action::SaveProfile(1);
        notes.press(save);
        notes.say(Note::done(Some(save), "Збережено в «Профіль 2»"), t);
        assert_eq!(notes.shown().map(|n| n.on), Some(Some(save)));
        notes.clear();
        assert_eq!(notes.shown(), None);
        let note = Note::failure(None, Failure::TimedOut, "", &texts);
        assert_eq!((&*note.text, note.detail), ("ПК не відповідає", None));
        let note = Note::failure(None, Failure::NotConnected, "", &texts);
        assert_eq!(&*note.text, "Немає з'єднання з ПК");
    }
}
