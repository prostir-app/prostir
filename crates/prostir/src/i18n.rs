//! Interface language of the PC side, from the desktop's locale: Ukrainian for `uk*`,
//! English otherwise. Strings are written as `inline_t("український", "English")` pairs.

use std::sync::OnceLock;

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

pub fn inline_t<'a>(uk: &'a str, en: &'a str) -> &'a str {
    if ukrainian() { uk } else { en }
}

/// `uk` or `en`, sent to the headset.
pub fn code() -> &'static str {
    inline_t("uk", "en")
}

/// Text `id` of `lang/*.ftl` with the count `n`, in the PC's language: its words in
/// the plural form `n` needs ("2 екрани", "5 екранів").
pub fn count(id: &str, n: usize) -> String {
    static STRINGS: OnceLock<prostir_lang::Strings> = OnceLock::new();
    STRINGS
        .get_or_init(|| counted_in(code()))
        .f(id, &[("n", n.into())])
}

fn counted_in(code: &str) -> prostir_lang::Strings {
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
        let uk = counted_in("uk");
        let at_most = |n: usize| uk.f("pc-limit-physical", &[("n", n.into())]);
        assert_eq!(at_most(1), "Щонайбільше 1 екран ПК");
        assert_eq!(at_most(2), "Щонайбільше 2 екрани ПК");
        assert_eq!(at_most(5), "Щонайбільше 5 екранів ПК");
        assert_eq!(at_most(21), "Щонайбільше 21 екран ПК");
        let streams = uk.f("pc-limit-streams", &[("n", 12.into())]);
        assert_eq!(streams, "Щонайбільше 12 екранів разом");
        let en = counted_in("en");
        assert_eq!(
            en.f("pc-limit-monitors", &[("n", 1.into())]),
            "At most 1 monitor"
        );
        assert_eq!(
            en.f("pc-limit-windows", &[("n", 3.into())]),
            "At most 3 windows"
        );
    }
}
