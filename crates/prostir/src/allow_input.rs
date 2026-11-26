//! The input right (`perms::INPUT`: the mouse and keyboard from the headset) as the
//! person at the PC gives it: off for a headset without an `access` line, it is asked
//! for the first time such a headset tries to move the mouse or click, once for each
//! headset while Prostir runs: a desktop notification with "Allow" and "Not now",
//! and, until answered, the tray and the settings window. "Allow" writes the right
//! into the headset's line in `settings.txt`; "Not now" leaves it off, and nothing asks
//! again until Prostir starts again. The settings window, `prostir ctl input` and the
//! local API give or take the right at any time; the headset is told at once (its
//! `Access`, and feature 15 named or not).

use std::collections::BTreeMap;
use std::time::Instant;

use log::{info, warn};
use prostir_proto::state::perms;

use crate::events::Event;
use crate::i18n::{self, t};
use crate::platform::{self, Said};
use crate::{Brain, door};

/// Which headsets were asked for the input right while Prostir runs, and whether
/// each question still waits for an answer (the tray and the settings window show
/// it then).
#[derive(Debug, Default)]
pub struct Asks(BTreeMap<[u8; 32], bool>);

impl Asks {
    /// A headset without the right tried to use the mouse: whether to ask now (the
    /// first time only).
    pub fn ask(&mut self, key: [u8; 32]) -> bool {
        if self.0.contains_key(&key) {
            return false;
        }
        self.0.insert(key, true);
        true
    }

    /// Answered, or no longer worth an answer (its session ended): it waits no more,
    /// and is not asked again.
    pub fn done(&mut self, key: &[u8; 32]) {
        if let Some(waits) = self.0.get_mut(key) {
            *waits = false;
        }
    }

    /// The question for this headset waits for an answer.
    pub fn waits(&self, key: &[u8; 32]) -> bool {
        self.0.get(key).copied().unwrap_or(false)
    }

    /// The first headset whose question waits for an answer.
    pub fn first_waiting(&self) -> Option<[u8; 32]> {
        self.0.iter().find(|(_, w)| **w).map(|(key, _)| *key)
    }
}

impl Brain {
    /// Headset `key` tried to use the mouse without the right: the person at the PC
    /// is asked, once (see the module).
    pub(crate) fn ask_input(&mut self, key: [u8; 32]) {
        if self.store.wanted().access_of(&key).may(perms::INPUT) || !self.input_asks.ask(key) {
            return;
        }
        let name = self.headset_label(&key);
        info!(
            "headset {} would use the mouse and keyboard: asking the person at the PC",
            prostir_proto::secure::fingerprint(&key)
        );
        let named = [("name", name.as_str().into())];
        let body = i18n::f("pc-input-ask", &named);
        let plain = i18n::f("pc-input-ask-plain", &named);
        let buttons = [t("pc-input-ask-allow"), t("pc-input-ask-later")];
        let Some(mut notice) =
            platform::dialogs::notice(t("pc-input-ask-title"), &body, &buttons, &plain)
        else {
            return;
        };
        let until = Instant::now() + door::NOTICE_TIME;
        let events = self.events.clone();
        let spawned = std::thread::Builder::new()
            .name("input-ask".into())
            .spawn(move || {
                let mut said = Said::Closed;
                while let Some(left) = until.checked_duration_since(Instant::now()) {
                    match platform::sockets::readable(&[notice.fd()], Some(left)) {
                        Ok(ready) if ready[0] => {
                            if let Some(s) = notice.check() {
                                said = s;
                                break;
                            }
                        }
                        _ => break,
                    }
                }
                drop(notice);
                let answer = match said {
                    Said::Button(0) => Some(true),
                    Said::Button(_) => Some(false),
                    Said::Closed => None,
                };
                events.send(Event::InputAnswer(key, answer));
            });
        if let Err(e) = spawned {
            warn!("no notice asking for the mouse and keyboard: {e}");
        }
    }

    /// What the notice asking for the input right came to (see [`Event::InputAnswer`]).
    pub(crate) fn input_answered(&mut self, key: [u8; 32], answer: Option<bool>) {
        match answer {
            Some(true) => {
                info!("the person at the PC allows the mouse and keyboard: from the notice");
                self.set_input(&key, true);
            }
            Some(false) => {
                info!("the person at the PC does not allow the mouse and keyboard now");
                self.input_asks.done(&key);
            }
            None => {}
        }
    }

    /// Gives headset `key` the input right or takes it, as the person at the PC
    /// chose: kept in `settings.txt`, and its session has it at once (what it holds
    /// pressed is let go without it). Whatever was asked of it is answered. `None`
    /// where its line in `settings.txt` is mistyped (it stays as it is); else whether
    /// the right changed.
    pub(crate) fn set_input(&mut self, key: &[u8; 32], on: bool) -> Option<bool> {
        self.input_asks.done(key);
        let changed = self.store.set_input(key, on)?;
        if changed {
            info!(
                "headset {}: the mouse and keyboard {}",
                prostir_proto::secure::fingerprint(key),
                if on { "allowed" } else { "not allowed" }
            );
            let access = self.store.wanted().access_of(key);
            if let Some(s) = self.sessions.get_mut(key) {
                s.set_configured(access);
            }
            self.headsets_changed();
        }
        Some(changed)
    }

    /// The headset's name as the person knows it, or "Headset" for one without.
    pub(crate) fn headset_label(&self, key: &[u8; 32]) -> String {
        let name = self.headset_name(key);
        if name.is_empty() {
            t("pc-input-a-headset").to_owned()
        } else {
            name
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each headset is asked once while Prostir runs; its question waits until it is
    /// answered or its session ends, and then it is not asked again.
    #[test]
    fn each_headset_is_asked_once() {
        let mut asks = Asks::default();
        let (one, two) = ([1; 32], [2; 32]);
        assert!(asks.ask(one));
        assert!(!asks.ask(one), "once");
        assert!(asks.waits(&one) && !asks.waits(&two));
        assert_eq!(asks.first_waiting(), Some(one));
        assert!(asks.ask(two));
        asks.done(&one);
        assert!(!asks.waits(&one));
        assert!(!asks.ask(one), "not now: not asked again");
        assert_eq!(asks.first_waiting(), Some(two));
        asks.done(&two);
        assert_eq!(asks.first_waiting(), None);
        // Done before it was ever asked: nothing to remember.
        asks.done(&[3; 32]);
        assert!(asks.ask([3; 32]));
    }
}
