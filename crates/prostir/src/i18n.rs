//! Interface language of the PC side, from the desktop's locale: Ukrainian for `uk*`,
//! English otherwise. The texts are the `pc-` messages of `lang/<code>.ftl` at the root
//! of the repository (one file per language, shared with the settings window and the
//! headset app), built in and named by id: `t("pc-ctl-done")`.

use std::collections::HashMap;
use std::sync::OnceLock;

use prostir_lang::{Arg, Strings};

/// What the ids of the PC app's messages start with in the shared language files.
const PREFIX: &str = "pc-";

fn ukrainian() -> bool {
    static UK: OnceLock<bool> = OnceLock::new();
    *UK.get_or_init(|| {
        // The first set variable wins, as in gettext.
        ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .find_map(|v| std::env::var(v).ok().filter(|s| !s.is_empty()))
            .is_some_and(|s| s.starts_with("uk"))
    })
}

/// `uk` or `en`, sent to the headset.
pub fn code() -> &'static str {
    if ukrainian() { "uk" } else { "en" }
}

fn strings() -> &'static Strings {
    static STRINGS: OnceLock<Strings> = OnceLock::new();
    STRINGS.get_or_init(|| built_in(code()))
}

/// The ids of the PC's messages that have no placeholders: the ones `t` can show.
fn plain_ids() -> impl Iterator<Item = String> {
    prostir_lang::plain_ids(prostir_lang::BUILTIN[0].1)
        .into_iter()
        .filter(|id| id.starts_with(PREFIX))
}

/// The text of message `id` that has no placeholders, in the PC's language. Every such
/// message is formatted once, at the first call, so a call is a lookup that allocates
/// nothing; an id no file has, or one with placeholders (those go through `f`), shows
/// as itself.
pub fn t(id: &'static str) -> &'static str {
    static TEXTS: OnceLock<HashMap<String, &'static str>> = OnceLock::new();
    let texts = TEXTS.get_or_init(|| {
        plain_ids()
            .map(|id| {
                // Kept for the life of the program, as the texts in the code were.
                let text: &'static str = Box::leak(strings().t(&id).into_boxed_str());
                (id, text)
            })
            .collect()
    });
    texts.get(id).copied().unwrap_or(id)
}

/// The text of message `id` with its placeholders (`{ $name }`) filled in.
pub fn f(id: &str, args: &[(&str, Arg)]) -> String {
    strings().f(id, args)
}

/// Text `id` of `lang/*.ftl` with the count `n`, in the PC's language: its words in
/// the plural form `n` needs ("2 екрани", "5 екранів").
pub fn count(id: &str, n: usize) -> String {
    f(id, &[("n", n.into())])
}

fn built_in(code: &str) -> Strings {
    prostir_lang::Catalog::with_dirs(prostir_lang::BUILTIN, Vec::new()).load(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each language has every text, and the count picks the form.
    #[test]
    fn counts_take_their_plural_forms() {
        for (code, text) in prostir_lang::BUILTIN {
            let report = prostir_lang::check(prostir_lang::BUILTIN[0].1, text);
            assert!(report.is_clean(), "{code}: {report:?}");
        }
        let uk = built_in("uk");
        let at_most = |n: usize| uk.f("pc-limit-physical", &[("n", n.into())]);
        assert_eq!(at_most(1), "Щонайбільше 1 екран ПК");
        assert_eq!(at_most(2), "Щонайбільше 2 екрани ПК");
        assert_eq!(at_most(5), "Щонайбільше 5 екранів ПК");
        assert_eq!(at_most(21), "Щонайбільше 21 екран ПК");
        let streams = uk.f("pc-limit-streams", &[("n", 12.into())]);
        assert_eq!(streams, "Щонайбільше 12 екранів разом");
        let en = built_in("en");
        assert_eq!(
            en.f("pc-limit-monitors", &[("n", 1.into())]),
            "At most 1 monitor"
        );
        assert_eq!(
            en.f("pc-limit-windows", &[("n", 3.into())]),
            "At most 3 windows"
        );
    }

    /// The texts `t` renders up front come out whole in each language: Fluent has
    /// nothing to report for them (a text that needs arguments is not among them).
    #[test]
    fn the_texts_rendered_up_front_have_no_fluent_errors() {
        let plain: Vec<String> = plain_ids().collect();
        assert!(plain.len() > 10, "{plain:?}");
        assert!(!plain.iter().any(|id| id == "pc-quality-custom"));
        for code in ["en", "uk"] {
            let strings = built_in(code);
            for id in &plain {
                let errors = strings.errors_without_args(id);
                assert!(errors.is_empty(), "{code} {id}: {errors:?}");
            }
        }
        // A text that does need arguments does report, so the check can fail.
        let needy = built_in("en").errors_without_args("pc-quality-custom");
        assert!(!needy.is_empty());
        // `t` still gives the text of one without placeholders, and the id of one with.
        assert_ne!(t("pc-ctl-done"), "pc-ctl-done");
        assert_eq!(t("pc-quality-custom"), "pc-quality-custom");
    }

    /// A text is the one of the PC's language, and an id nobody wrote shows as itself.
    #[test]
    fn a_text_is_looked_up_by_its_id() {
        let own = strings().t("pc-ctl-done");
        assert_eq!(own, f("pc-ctl-done", &[]));
        assert_eq!(t("pc-ctl-done"), own);
        assert_eq!(t("no-such-text"), "no-such-text");
        assert!(count("pc-quality-custom", 40).contains("40"));
    }
}
