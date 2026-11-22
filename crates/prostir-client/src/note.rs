//! What the open menu says by the button that was pressed: that it is done, that it
//! waits for the PC, or why it failed. One note at a time, under the button it is
//! about (`ui::Panel` places it; a wait, and a note no button of the page stands for,
//! go on the menu's bottom line). It is short-lived, goes when another button of the
//! menu is pressed or the page changes, and is said only for presses in the open
//! menu: while the menu is closed, or for a press elsewhere (the palm menu, a command
//! of the PC), the same news is a toast where it is worth one (see [`said`]).
//!
//! Plain data with the time passed in, so the rules are tested on the PC.

#![forbid(unsafe_code)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use prostir_proto::request::Outcome;
use prostir_proto::state::ReqId;

use crate::ids::PcId;
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
    /// News that is neither, such as the PC ending the connection.
    Info,
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
            kind: Kind::Waiting,
            ..Self::done(on, texts.t("ask-waiting"))
        }
    }

    /// It did not happen: `text` says why.
    pub fn failed(on: Option<Action>, text: &str) -> Self {
        Self {
            kind: Kind::Failed,
            ..Self::done(on, text)
        }
    }

    /// News on the bottom line.
    pub fn info(text: &str) -> Self {
        Self {
            kind: Kind::Info,
            ..Self::done(None, text)
        }
    }

    /// A request failed for `why`: its text, and the PC's `detail` under it; only the
    /// detail where the failure has no text of its own, and a plain "it did not work"
    /// where there is not even that.
    pub fn failure(on: Option<Action>, why: Failure, detail: &str, texts: &Texts) -> Self {
        let (text, detail) = match why.text() {
            Some(id) => (texts.t(id), (!detail.is_empty()).then(|| detail.into())),
            None if detail.is_empty() => (texts.t("failure-other"), None),
            None => (detail, None),
        };
        Self {
            detail,
            ..Self::failed(on, text)
        }
    }

    /// The text of a toast saying the same: the PC's detail after the reason.
    pub fn toast_text(&self) -> String {
        match &self.detail {
            Some(detail) => format!("{} ({detail})", self.text),
            None => self.text.to_string(),
        }
    }

    /// How long it stays: a wait until the answer comes (or the request is given up
    /// on), a failure long enough to read its reason.
    fn shown_for(&self) -> Duration {
        match self.kind {
            Kind::Done => DONE_SHOWN_FOR,
            Kind::Waiting => ANSWER_WITHIN,
            Kind::Failed | Kind::Info => FAILURE_SHOWN_FOR,
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

/// Where news is said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Said {
    /// In the open menu (see [`Notes::say`]).
    Note,
    /// A toast of its own, in front of the user.
    Toast,
    Nothing,
}

/// Where news worth a `toast` (or not) is said: in the open menu when it is about a
/// press `in_menu` (or the menu's own news); else, with the menu closed or for a press
/// elsewhere (the palm menu, a command of the PC), a toast if it is worth one.
pub fn said(menu_open: bool, in_menu: bool, toast: bool) -> Said {
    if menu_open && in_menu {
        Said::Note
    } else if toast {
        Said::Toast
    } else {
        Said::Nothing
    }
}

/// Who asked a request that was answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Asker {
    /// The menu's button that asked; none when no button of the menu did (a press
    /// elsewhere, a command of the PC, or a request already given up on).
    pub on: Option<Action>,
    /// A newer request of the same control waits: an older one that went through says
    /// nothing (its failure still does).
    pub newer_waits: bool,
}

/// The menu's notes.
#[derive(Debug, Default)]
pub struct Notes {
    /// The note shown, and until when.
    shown: Option<(Note, Instant)>,
    /// The menu's button pressed last, while requests sent now are its; none after a
    /// press elsewhere.
    pressed: Option<Action>,
    /// Which button of the menu asked each request still waited for (by its PC and
    /// the request's number in that PC's session), and for which control: its answer
    /// is said under it.
    asked_by: Vec<(PcId, ReqId, Asked, Action)>,
}

impl Notes {
    /// Button `action` of the menu was pressed: what was said before goes, and
    /// requests sent now are this button's.
    pub fn press(&mut self, action: Action) {
        self.shown = None;
        self.pressed = Some(action);
    }

    /// Something was pressed elsewhere (the palm menu, a command of the PC): the
    /// menu's note stays, and requests sent now are not of the menu.
    pub fn press_elsewhere(&mut self) {
        self.pressed = None;
    }

    /// The menu's button pressed last, if the last press was the menu's: a failure
    /// before anything was sent is said under it.
    pub fn pressed(&self) -> Option<Action> {
        self.pressed
    }

    /// Request `req` of control `what` was sent to PC `pc`: the menu's button pressed
    /// for it (none for a press elsewhere), whose note says the answer.
    pub fn asked(&mut self, pc: PcId, req: ReqId, what: Asked) -> Option<Action> {
        let on = self.pressed?;
        self.asked_by.push((pc, req, what, on));
        Some(on)
    }

    /// Request `req` of PC `pc` was answered, failed or given up on: who asked it.
    pub fn answered(&mut self, pc: PcId, req: ReqId) -> Asker {
        let Some(at) = self
            .asked_by
            .iter()
            .position(|(p, r, ..)| *p == pc && *r == req)
        else {
            return Asker::default();
        };
        let (_, _, what, on) = self.asked_by.swap_remove(at);
        let newer_waits = self
            .asked_by
            .iter()
            .any(|(p, r, w, _)| *p == pc && *w == what && r.0 > req.0);
        Asker {
            on: Some(on),
            newer_waits,
        }
    }

    /// A new session with PC `pc`, or its connection broke: no request of before to it
    /// is answered, and its numbers start again.
    pub fn forget_requests(&mut self, pc: PcId) {
        self.asked_by.retain(|(p, ..)| *p != pc);
    }

    /// Says `note` from `now`, in place of what was said; but a reason still shown
    /// stays over news of another button (pressing a button takes it away).
    pub fn say(&mut self, note: Note, now: Instant) {
        if let Some((shown, until)) = &self.shown
            && shown.kind == Kind::Failed
            && now < *until
            && note.kind != Kind::Failed
            && shown.on != note.on
        {
            return;
        }
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
        Action::ShowPhysical(_, true) => "note-display-shown",
        Action::ShowPhysical(_, false) => "note-display-hidden",
        Action::SetScale(..) => "note-scale",
        Action::NoLimits(true) => "note-no-limits-on",
        Action::NoLimits(false) => "note-no-limits-off",
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

/// What the menu says when the request of control `what`, asked as `asker` says, is
/// answered with `outcome`. A profile switched to is said by the caller, which knows
/// whether the switch still stands.
pub fn answer(what: Asked, asker: Asker, outcome: &Outcome, texts: &Texts) -> Option<Told> {
    let on = asker.on;
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
        // An older request gone through while a newer one of the same control waits:
        // the newer one's answer says how it stands.
        Outcome::Done | Outcome::Added { .. } | Outcome::Ids { .. } if asker.newer_waits => {
            return None;
        }
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
pub fn permission_note(permission: u8, texts: &Texts) -> Option<Told> {
    use prostir_proto::state::permission;
    let button = Some(Action::SetMic(true));
    match permission {
        permission::GRANTED => Some(Told {
            note: Note::done(button, texts.t("note-mic-allowed")),
            toast: false,
        }),
        permission::DENIED => Some(Told {
            note: Note::failed(button, texts.t("note-mic-denied")),
            toast: true,
        }),
        _ => None,
    }
}

/// What the menu says once it shows the language chosen (`chosen`): some lines of its
/// file could not be read (`warning`, worth a toast too), or, after the user chose it
/// (`asked`), that it changed. Nothing for a language that came by itself (the PC's
/// or the system's changed).
pub fn language_note(
    chosen: crate::lang::Language,
    asked: bool,
    warning: Option<&str>,
    texts: &Texts,
) -> Option<Told> {
    let on = Some(Action::Language(chosen));
    match warning {
        Some(warning) => Some(Told {
            note: Note::failed(on, warning),
            toast: true,
        }),
        None if asked => Some(Told {
            note: Note::done(on, texts.t("note-language")),
            toast: false,
        }),
        None => None,
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
    use crate::lang::Language;
    use crate::settings::Preset;
    use prostir_proto::state::{DisplayId, ErrorCode, permission};

    /// The PC of these requests.
    const PC: PcId = PcId([3; 8]);

    fn k(n: u32) -> DisplayKey {
        DisplayKey::new(PC, DisplayId(n))
    }

    /// A press of `action` in the menu that asks the PC as control `what` with request
    /// `req`, the way the menu does it: the old note goes, the button waits.
    fn ask(notes: &mut Notes, action: Action, req: u32, what: Asked, texts: &Texts, t: Instant) {
        notes.press(action);
        let on = notes.asked(PC, ReqId(req), what);
        assert_eq!(on, Some(action));
        notes.say(Note::waiting(on, texts), t);
    }

    /// Adding a monitor: wait, then "added" under the size pressed, then gone after a
    /// few seconds; failing, the reason and the PC's detail, longer.
    #[test]
    fn a_monitor_added_or_not() {
        let texts = crate::lang::builtin("uk");
        let t = Instant::now();
        let mut notes = Notes::default();
        let add = Action::Add {
            width: 2560,
            height: 1440,
        };
        ask(&mut notes, add, 1, Asked::AddMonitor, &texts, t);
        let shown = notes.shown().expect("waiting");
        assert_eq!(
            (shown.on, shown.kind, &*shown.text),
            (Some(add), Kind::Waiting, "Зачекайте…")
        );
        let asker = notes.answered(PC, ReqId(1));
        assert_eq!(asker.on, Some(add));
        let added = Outcome::Added { id: DisplayId(5) };
        let told = answer(Asked::AddMonitor, asker, &added, &texts).expect("said");
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
        ask(&mut notes, add, 2, Asked::AddMonitor, &texts, t);
        let asker = notes.answered(PC, ReqId(2));
        let failed = Outcome::failed(ErrorCode::LIMIT, "at most 6");
        let told = answer(Asked::AddMonitor, asker, &failed, &texts).expect("said");
        assert!(told.toast, "a failure is told while the menu is closed too");
        assert_eq!(told.note.toast_text(), "Досягнуто межі (at most 6)");
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
    /// pressed meanwhile; a code without a text of its own says the PC's detail, or
    /// that it did not work when there is none.
    #[test]
    fn an_answer_finds_its_button() {
        let texts = crate::lang::builtin("en");
        let t = Instant::now();
        let mut notes = Notes::default();
        let remove = Action::Remove(k(102));
        ask(&mut notes, remove, 1, Asked::Remove(k(102)), &texts, t);
        // A switch shows its own state: pressing it only takes the note away.
        notes.press(Action::Follow(true));
        assert_eq!(notes.shown(), None);
        let asker = notes.answered(PC, ReqId(1));
        assert_eq!(asker.on, Some(remove));
        let told = answer(Asked::Remove(k(102)), asker, &Outcome::Done, &texts).expect("said");
        assert_eq!(&*told.note.text, "Monitor removed");
        assert_eq!(told.note.on, Some(remove));
        assert_eq!(
            notes.answered(PC, ReqId(1)),
            Asker::default(),
            "answered once"
        );
        let mirror = Asker {
            on: Some(Action::ShowPhysical(k(4), false)),
            newer_waits: false,
        };
        let other = Outcome::failed(ErrorCode::OTHER, "the desktop is busy");
        let told = answer(Asked::Physical(k(4)), mirror, &other, &texts).expect("said");
        assert_eq!(&*told.note.text, "the desktop is busy");
        assert_eq!(told.note.detail, None);
        let bare = Outcome::failed(ErrorCode::OTHER, "");
        let told = answer(Asked::Physical(k(4)), mirror, &bare, &texts).expect("said");
        assert_eq!(&*told.note.text, "It did not work");
    }

    /// Two requests of one control answered the other way round, and one of them
    /// timing out: each answer finds the button that asked it, an older success while a
    /// newer request waits says nothing, and a failure always speaks.
    #[test]
    fn requests_of_one_control_by_their_numbers() {
        let texts = crate::lang::builtin("en");
        let t = Instant::now();
        let mut notes = Notes::default();
        let on = Action::SetAudio(true);
        let off = Action::SetAudio(false);
        ask(&mut notes, on, 1, Asked::Sound, &texts, t);
        ask(&mut notes, off, 2, Asked::Sound, &texts, t);
        // The newer one first: it says how the sound stands now.
        let asker = notes.answered(PC, ReqId(2));
        assert_eq!(
            asker,
            Asker {
                on: Some(off),
                newer_waits: false
            }
        );
        let told = answer(Asked::Sound, asker, &Outcome::Done, &texts).expect("said");
        assert_eq!(
            (told.note.on, &*told.note.text),
            (Some(off), "PC sound plays on the PC")
        );
        // The older one after it: its own button.
        let asker = notes.answered(PC, ReqId(1));
        assert_eq!(asker.on, Some(on));
        // A third and a fourth: the third goes through while the fourth waits, then the
        // fourth times out (the caller's `given_up`, with the request's number).
        ask(&mut notes, on, 3, Asked::Sound, &texts, t);
        ask(&mut notes, off, 4, Asked::Sound, &texts, t);
        let asker = notes.answered(PC, ReqId(3));
        assert!(asker.newer_waits);
        assert_eq!(answer(Asked::Sound, asker, &Outcome::Done, &texts), None);
        let failed = Outcome::failed(ErrorCode::SOUND, "");
        assert!(answer(Asked::Sound, asker, &failed, &texts).is_some());
        let asker = notes.answered(PC, ReqId(4));
        let note = Note::failure(asker.on, Failure::TimedOut, "", &texts);
        assert_eq!(
            (note.on, &*note.text),
            (Some(off), "The PC does not answer")
        );
        // A new session: the numbers start again, nothing of before is found.
        ask(&mut notes, on, 1, Asked::Sound, &texts, t);
        // Another PC's session ending leaves this one's requests.
        notes.forget_requests(PcId([9; 8]));
        assert_eq!(notes.answered(PcId([9; 8]), ReqId(1)), Asker::default());
        ask(&mut notes, on, 2, Asked::Sound, &texts, t);
        notes.forget_requests(PC);
        assert_eq!(notes.answered(PC, ReqId(2)), Asker::default());
    }

    /// A press elsewhere (the palm menu, a command of the PC) keeps the menu's note,
    /// and its requests have no button: done, nothing is said; a failure is a toast.
    #[test]
    fn a_press_elsewhere() {
        let texts = crate::lang::builtin("en");
        let t = Instant::now();
        let mut notes = Notes::default();
        let save = Action::SaveProfile(1);
        notes.press(save);
        notes.say(Note::done(Some(save), "Saved"), t);
        notes.press_elsewhere();
        assert!(notes.shown().is_some(), "the menu's note stays");
        assert_eq!(notes.pressed(), None);
        assert_eq!(notes.asked(PC, ReqId(5), Asked::Screenshot), None);
        let asker = notes.answered(PC, ReqId(5));
        assert_eq!(asker, Asker::default());
        assert_eq!(
            answer(Asked::Screenshot, asker, &Outcome::Done, &texts),
            None
        );
        let failed = Outcome::failed(ErrorCode::CAPTURE, "");
        let told = answer(Asked::Screenshot, asker, &failed, &texts).expect("said");
        assert_eq!(said(true, asker.on.is_some(), told.toast), Said::Toast);
    }

    /// Where news goes: in the open menu for its own presses; a toast when it is worth
    /// one with the menu closed or for a press elsewhere; else nowhere.
    #[test]
    fn where_news_goes() {
        assert_eq!(said(true, true, false), Said::Note);
        assert_eq!(said(true, true, true), Said::Note);
        assert_eq!(said(false, true, true), Said::Toast);
        assert_eq!(said(true, false, true), Said::Toast);
        assert_eq!(said(false, true, false), Said::Nothing);
        assert_eq!(said(true, false, false), Said::Nothing);
    }

    /// A reason still shown is not replaced by other news of another button, nor by
    /// news without a button; a newer failure, news of its own button, a press or its
    /// time up take it away.
    #[test]
    fn a_reason_stays_over_other_news() {
        let texts = crate::lang::builtin("en");
        let t = Instant::now();
        let mut notes = Notes::default();
        let add = Action::Add {
            width: 1920,
            height: 1080,
        };
        let primary = Action::MakePrimary(k(1));
        notes.say(Note::failure(Some(add), Failure::TimedOut, "", &texts), t);
        notes.say(Note::done(Some(primary), "Now the primary monitor"), t);
        notes.say(Note::info("The PC ended the connection"), t);
        assert_eq!(notes.shown().map(|n| n.on), Some(Some(add)));
        notes.say(Note::done(Some(add), "Monitor added"), t);
        assert_eq!(notes.shown().map(|n| n.kind), Some(Kind::Done));
        notes.say(Note::failure(Some(add), Failure::TimedOut, "", &texts), t);
        notes.say(
            Note::failure(Some(primary), Failure::NotConnected, "", &texts),
            t,
        );
        assert_eq!(notes.shown().map(|n| n.on), Some(Some(primary)));
        let later = t + FAILURE_SHOWN_FOR;
        notes.say(Note::done(Some(add), "Monitor added"), later);
        assert_eq!(notes.shown().map(|n| n.on), Some(Some(add)), "time up");
        notes.say(
            Note::failure(Some(primary), Failure::TimedOut, "", &texts),
            t,
        );
        notes.press(Action::Tab(crate::ui::Tab::Monitors));
        assert_eq!(notes.shown(), None, "another page");
    }

    /// The PC's answers with a text of their own: a screenshot's path (a toast too
    /// while the menu is closed); the mouse found and a profile say nothing here.
    #[test]
    fn answers_with_their_own_text() {
        let texts = crate::lang::builtin("uk");
        let saved = Outcome::Saved {
            path: "~/Pictures/shot.png".into(),
        };
        let asker = Asker {
            on: Some(Action::Screenshot),
            newer_waits: false,
        };
        let told = answer(Asked::Screenshot, asker, &saved, &texts).expect("said");
        assert!(told.toast);
        assert_eq!(&*told.note.text, "Знімок збережено: ~/Pictures/shot.png");
        let pointer = Outcome::Pointer {
            id: DisplayId(1),
            u: 0,
            v: 0,
        };
        assert_eq!(
            answer(Asked::FindMouse, Asker::default(), &pointer, &texts),
            None
        );
        let ids = Outcome::Ids { ids: Vec::new() };
        let profile = Asker {
            on: Some(Action::ApplyProfile(1)),
            newer_waits: false,
        };
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
                Action::ShowPhysical(k(1), true),
                Action::ShowPhysical(k(1), false),
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
    /// lets the headset record; a refusal is a failure under the switch, and a toast
    /// with the menu closed.
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
        let denied = permission_note(permission::DENIED, &texts).expect("said");
        assert_eq!((denied.note.kind, denied.toast), (Kind::Failed, true));
        let granted = permission_note(permission::GRANTED, &texts).expect("said");
        assert_eq!((granted.note.kind, granted.toast), (Kind::Done, false));
        assert_eq!(permission_note(permission::UNKNOWN, &texts), None);
    }

    /// The menu's language: "changed" only after the user chose one; lines of its
    /// file that could not be read are said whoever changed it.
    #[test]
    fn the_language_says_it_changed() {
        let texts = crate::lang::builtin("en");
        let chosen = Language::LikeSystem;
        let told = language_note(chosen, true, None, &texts).expect("said");
        assert_eq!(
            (told.note.on, &*told.note.text, told.toast),
            (
                Some(Action::Language(chosen)),
                "Menu language changed",
                false
            )
        );
        assert_eq!(language_note(chosen, false, None, &texts), None);
        let told = language_note(chosen, false, Some("Some lines"), &texts).expect("said");
        assert_eq!((told.note.kind, told.toast), (Kind::Failed, true));
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
