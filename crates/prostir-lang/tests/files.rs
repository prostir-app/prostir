//! The language files in `lang/` at the root of the repository, one per language, shared
//! by the PC app, the settings window and the headset app: each file is readable, has
//! the messages of English and nothing else, with the same placeholders; every message
//! a program names exists, and every message of a program is named by it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use fluent_syntax::ast::{
    CallArguments, Entry, Expression, InlineExpression, Pattern, PatternElement,
};

/// A program's messages: the prefix of their ids and the folder of its code.
const PROGRAMS: &[(&str, &str)] = &[
    ("pc-", "crates/prostir"),
    ("window-", "crates/prostir-window"),
    ("headset-", "crates/prostir-client"),
];

/// Quoted strings in the code that start with a program's prefix but are not messages.
const NOT_MESSAGES: &[&str] = &[
    // The name of a file.
    "window-closed-note",
];

/// The language that has every message every other language falls back to.
const REFERENCE: &str = "en";

/// Languages that must have every message too; others may have some (the rest is English).
const COMPLETE: &[&str] = &["en", "uk"];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every `lang/<code>.ftl`: (code, text).
fn files() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(root().join("lang")).expect("lang/ at the root") {
        let path = entry.expect("a directory entry").path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let Some(code) = name.strip_suffix(".ftl") else {
            panic!("lang/{name}: only <code>.ftl files belong in lang/");
        };
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("lang/{name}: {e}"));
        out.push((code.to_owned(), text));
    }
    out.sort();
    out
}

fn english() -> String {
    files()
        .into_iter()
        .find(|(code, _)| code == REFERENCE)
        .expect("lang/en.ftl")
        .1
}

/// The ids of a file's messages, each with the placeholders its text and attributes use.
fn variables(text: &str) -> BTreeMap<String, BTreeSet<String>> {
    let resource = fluent_syntax::parser::parse(text).unwrap_or_else(|(_, e)| panic!("{e:?}"));
    let mut out = BTreeMap::new();
    for entry in &resource.body {
        if let Entry::Message(m) = entry {
            let mut names = BTreeSet::new();
            if let Some(p) = &m.value {
                in_pattern(p, &mut names);
            }
            for a in &m.attributes {
                in_pattern(&a.value, &mut names);
            }
            out.insert(m.id.name.to_owned(), names);
        }
    }
    out
}

fn in_pattern(p: &Pattern<&str>, names: &mut BTreeSet<String>) {
    for element in &p.elements {
        if let PatternElement::Placeable { expression } = element {
            in_expression(expression, names);
        }
    }
}

fn in_expression(e: &Expression<&str>, names: &mut BTreeSet<String>) {
    match e {
        Expression::Select { selector, variants } => {
            in_inline(selector, names);
            for v in variants {
                in_pattern(&v.value, names);
            }
        }
        Expression::Inline(i) => in_inline(i, names),
    }
}

fn in_inline(i: &InlineExpression<&str>, names: &mut BTreeSet<String>) {
    match i {
        InlineExpression::VariableReference { id } => {
            names.insert(id.name.to_owned());
        }
        InlineExpression::FunctionReference { arguments, .. } => in_arguments(arguments, names),
        InlineExpression::TermReference {
            arguments: Some(arguments),
            ..
        } => in_arguments(arguments, names),
        InlineExpression::Placeable { expression } => in_expression(expression, names),
        _ => {}
    }
}

fn in_arguments(a: &CallArguments<&str>, names: &mut BTreeSet<String>) {
    for p in &a.positional {
        in_inline(p, names);
    }
    for n in &a.named {
        in_inline(&n.value, names);
    }
}

fn program_of(id: &str) -> Option<&'static str> {
    PROGRAMS
        .iter()
        .find(|(prefix, _)| id.starts_with(prefix))
        .map(|(prefix, _)| *prefix)
}

#[test]
fn every_file_is_a_language_built_into_the_programs() {
    let on_disk: Vec<String> = files().into_iter().map(|(code, _)| code).collect();
    let built_in: Vec<&str> = prostir_lang::BUILTIN
        .iter()
        .map(|(code, _)| *code)
        .collect();
    assert_eq!(
        on_disk, built_in,
        "lang/ and prostir_lang::BUILTIN list different languages: a new file is added to both"
    );
    for (code, text) in files() {
        let embedded = prostir_lang::BUILTIN.iter().find(|(c, _)| *c == code);
        assert_eq!(embedded.map(|(_, t)| *t), Some(text.as_str()), "{code}");
    }
}

#[test]
fn every_language_has_what_it_should_and_nothing_else() {
    let en = english();
    for (code, text) in files() {
        let r = prostir_lang::check(&en, &text);
        assert!(r.errors.is_empty(), "{code}.ftl: {:?}", r.errors);
        assert!(
            r.extra.is_empty(),
            "{code}.ftl: not in English: {:?}",
            r.extra
        );
        if COMPLETE.contains(&code.as_str()) {
            assert!(r.missing.is_empty(), "{code}.ftl lacks {:?}", r.missing);
        }
    }
}

#[test]
fn every_id_belongs_to_a_program() {
    for (code, text) in files() {
        for id in variables(&text).keys() {
            assert!(
                id == prostir_lang::NAME_ID || program_of(id).is_some(),
                "{code}.ftl: {id} starts with no program's prefix ({:?})",
                PROGRAMS.iter().map(|(p, _)| *p).collect::<Vec<_>>()
            );
        }
        assert!(
            variables(&text).contains_key(prostir_lang::NAME_ID),
            "{code}.ftl does not name its language ({})",
            prostir_lang::NAME_ID
        );
    }
}

#[test]
fn placeholders_are_the_same_in_every_language() {
    let en = variables(&english());
    for (code, text) in files() {
        for (id, names) in variables(&text) {
            assert_eq!(
                Some(&names),
                en.get(&id),
                "{code}.ftl {id}: placeholders differ"
            );
        }
    }
}

/// Messages that keep a long dash: the names of the sound devices, moved from the code
/// as they were, and shown by the sound settings of the desktop.
const LONG_DASH_KEPT: &[&str] = &[
    "pc-audio-prostir-headset",
    "pc-audio-prostir-headset-microphone",
];

#[test]
fn no_long_dashes_in_the_texts() {
    for (code, text) in files() {
        for (n, line) in text.lines().enumerate() {
            if LONG_DASH_KEPT.iter().any(|id| {
                line.strip_prefix(id)
                    .is_some_and(|rest| rest.starts_with(" ="))
            }) {
                continue;
            }
            assert!(
                !line.contains('\u{2014}') && !line.contains('\u{2013}'),
                "{code}.ftl:{}: a long dash: {line}",
                n + 1
            );
        }
    }
}

/// Every `.rs` file of a program's `src/`, with its text; the tests (`tests.rs`,
/// `tests/`) only if `with_tests`.
fn code_of(dir: &str, with_tests: bool) -> Vec<(PathBuf, String)> {
    fn walk(dir: &Path, with_tests: bool, out: &mut Vec<(PathBuf, String)>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
            .flatten()
            .map(|e| e.path())
            .collect();
        entries.sort();
        for path in entries {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if path.is_dir() {
                if with_tests || name != "tests" {
                    walk(&path, with_tests, out);
                }
            } else if name.ends_with(".rs") && (with_tests || name != "tests.rs") {
                let text = std::fs::read_to_string(&path).expect("a source file");
                out.push((path, text));
            }
        }
    }
    let mut out = Vec::new();
    walk(&root().join(dir).join("src"), with_tests, &mut out);
    out
}

/// The ids with `prefix` the code names: each `"prefix-word"` literal, and the stem of
/// each `"prefix-word-{..}"` literal (a family of messages numbered by the code).
fn named(sources: &[(PathBuf, String)], prefix: &str) -> (BTreeSet<String>, Vec<String>) {
    let mut ids = BTreeSet::new();
    let mut stems = Vec::new();
    for (_, text) in sources {
        let mut rest = text.as_str();
        while let Some(at) = rest.find(&format!("\"{prefix}")) {
            rest = &rest[at + 1..];
            let Some(end) = rest.find('"') else { break };
            let literal = &rest[..end];
            let plain = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-';
            if literal.ends_with('-') || NOT_MESSAGES.contains(&literal) {
                // A prefix, as the code compares ids with.
            } else if literal.chars().all(plain) {
                ids.insert(literal.to_owned());
            } else if let Some((stem, tail)) = literal.split_once('{')
                && tail.ends_with('}')
                && stem.chars().all(plain)
            {
                stems.push(stem.to_owned());
            }
            rest = &rest[end..];
        }
    }
    (ids, stems)
}

/// A message only the tests show has `-tests-` in its id (`pc-worker-run-tests-...`).
const TESTS_ONLY: &str = "-tests-";

#[test]
fn the_code_and_the_messages_match() {
    let en = variables(&english());
    for (prefix, dir) in PROGRAMS {
        let sources = code_of(dir, false);
        assert!(!sources.is_empty(), "{dir}: no code found");
        let (shown, stems) = named(&sources, prefix);
        let (in_tests, test_stems) = named(&code_of(dir, true), prefix);
        let numbered = |id: &str, stems: &[String]| {
            stems.iter().any(|stem| {
                id.strip_prefix(stem.as_str())
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            })
        };
        let missing: Vec<_> = in_tests
            .iter()
            .filter(|id| !en.contains_key(*id) && !numbered(id, &test_stems))
            .collect();
        assert!(
            missing.is_empty(),
            "{dir} names messages that lang/en.ftl lacks: {missing:?}"
        );
        let unused: Vec<_> = en
            .keys()
            .filter(|id| id.starts_with(prefix) && !id.contains(TESTS_ONLY))
            .filter(|id| !shown.contains(*id) && !numbered(id, &stems))
            .collect();
        assert!(
            unused.is_empty(),
            "lang/en.ftl has messages {dir} never shows: {unused:?}"
        );
        let test_only_unused: Vec<_> = en
            .keys()
            .filter(|id| id.starts_with(prefix) && id.contains(TESTS_ONLY))
            .filter(|id| !in_tests.contains(*id))
            .collect();
        assert!(
            test_only_unused.is_empty(),
            "lang/en.ftl has messages for the tests of {dir} that no test names: {test_only_unused:?}"
        );
        let hidden: Vec<_> = in_tests
            .iter()
            .filter(|id| !shown.contains(*id) && !id.contains(TESTS_ONLY))
            .filter(|id| !numbered(id, &stems))
            .collect();
        assert!(
            hidden.is_empty(),
            "only the tests of {dir} name these messages (a message only the tests show has \"{TESTS_ONLY}\" in its id): {hidden:?}"
        );
    }
}
