//! Why KWin may not give Prostir the screencast interface, and what to tell the person.
//!
//! KWin hides `zkde_screencast_unstable_v1` from a program that has no permission. Up
//! to Plasma 6.7 the permission is a desktop entry: KWin reads `/proc/<pid>/exe`, looks
//! in the application cache for an entry whose `Exec` program is that file (the first
//! match wins) and reads its `X-KDE-Wayland-Interfaces`. From Plasma 6.8 an ordinary
//! program gets every interface and only sandboxed ones (Flatpak, Snap) are held back.
//! A refused program sees no such interface in the registry and cannot tell why, so
//! when connecting fails this module looks at the environment and says what to do.
//!
//! The decision ([`advise`]) takes plain inputs and is tested without a desktop;
//! [`explain`] gathers them from the process.

use std::path::Path;
use std::time::Duration;

use log::warn;

use crate::i18n;
use crate::platform::{Unusable, run};

/// A KWin version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// The first Plasma that lets an ordinary program use the interface without a desktop
/// entry (kwin!6057, first in 6.7.90, the beta of 6.8).
const NO_ENTRY_NEEDED: Version = Version {
    major: 6,
    minor: 7,
    patch: 90,
};

/// The restricted interface Prostir binds: it is the only one. Windows and the mouse
/// go through KWin scripts and `kscreen-doctor`, not through
/// `org_kde_plasma_window_management` or `org_kde_kwin_fake_input`. The same name is
/// in `X-KDE-Wayland-Interfaces` of `packaging/io.github.prostir_app.Prostir.desktop`.
pub const INTERFACE: &str = "zkde_screencast_unstable_v1";

/// Why connecting to KWin's screencast interface failed, when it was this: the
/// compositor answered and its registry did not list the interface.
#[derive(Debug)]
pub struct NotExposed;

impl std::fmt::Display for NotExposed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "KWin does not expose {INTERFACE} to this program")
    }
}

impl std::error::Error for NotExposed {}

/// Whether the registry of the Wayland compositor listed the interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Registry {
    Listed,
    NotListed,
    /// The compositor was not reached, or its answer was something else.
    Unknown,
}

/// What the decision is made of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inputs {
    /// `XDG_CURRENT_DESKTOP`: names separated by `:`.
    pub desktops: String,
    /// `XDG_SESSION_TYPE`.
    pub session: String,
    /// KWin's version, where it could be found.
    pub kwin: Option<Version>,
    pub registry: Registry,
    /// Where this program is, when it is a build in a source tree (a `target`
    /// directory): the person can then install the shortcut for it by hand. A program
    /// from a package has its entry in the package.
    pub built_here: Option<String>,
}

/// What is wrong and what the person can do about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Advice {
    /// Plasma runs on X11: KWin's interface is of Wayland sessions only.
    NotWayland,
    /// KWin up to 6.7 hides the interface from a program no desktop entry names.
    NeedsShortcut {
        kwin: Option<Version>,
        built_here: Option<String>,
    },
    /// KWin 6.8 or newer hides it, so no entry would help: the program is in a sandbox.
    Sandboxed { kwin: Version },
}

/// Whether `XDG_CURRENT_DESKTOP` names KDE Plasma.
fn is_kde(desktops: &str) -> bool {
    desktops.split(':').any(|d| d.eq_ignore_ascii_case("KDE"))
}

/// What to tell the person, or `None` when this is not a problem of access to KDE's
/// screencast: another desktop, or the interface was there (whatever else failed
/// keeps its own message).
pub fn advise(i: &Inputs) -> Option<Advice> {
    if !is_kde(&i.desktops) || i.registry == Registry::Listed {
        return None;
    }
    if i.session.eq_ignore_ascii_case("x11") {
        return Some(Advice::NotWayland);
    }
    if i.registry != Registry::NotListed {
        return None;
    }
    match i.kwin {
        Some(kwin) if kwin >= NO_ENTRY_NEEDED => Some(Advice::Sandboxed { kwin }),
        kwin => Some(Advice::NeedsShortcut {
            kwin,
            built_here: i.built_here.clone(),
        }),
    }
}

/// The text of the advice in the PC's language.
pub fn text(advice: &Advice) -> String {
    // Which KWin it is, where known, so that a report of the problem carries it.
    let with_kwin = |mut text: String, kwin: &Option<Version>| {
        if let Some(v) = kwin {
            text.push(' ');
            text.push_str(&i18n::f(
                "pc-kde-version",
                &[("version", v.to_string().into())],
            ));
        }
        text
    };
    match advice {
        Advice::NotWayland => i18n::t("pc-kde-needs-wayland").into(),
        Advice::NeedsShortcut {
            kwin,
            built_here: None,
        } => with_kwin(i18n::t("pc-kde-needs-shortcut-installed").into(), kwin),
        Advice::NeedsShortcut {
            kwin,
            built_here: Some(exe),
        } => with_kwin(
            i18n::f("pc-kde-needs-shortcut-built", &[("exe", exe.into())]),
            kwin,
        ),
        Advice::Sandboxed { kwin } => {
            i18n::f("pc-kde-sandboxed", &[("version", kwin.to_string().into())])
        }
    }
}

/// KWin's version from what `kwin_wayland --version` prints (`kwin 6.5.6`).
pub fn parse_version(output: &str) -> Option<Version> {
    let word = output
        .split_whitespace()
        .find(|w| w.starts_with(|c: char| c.is_ascii_digit()))?;
    let mut parts = word.split('.').map(|p| {
        p.chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse::<u32>()
    });
    let major = parts.next()?.ok()?;
    let minor = parts.next()?.ok()?;
    // A version of two numbers has no patch release yet.
    let patch = parts.next().and_then(Result::ok).unwrap_or(0);
    Some(Version {
        major,
        minor,
        patch,
    })
}

/// KWin's version as the installed `kwin_wayland` says it (the one the session runs is
/// the one installed, unless it was updated and not yet restarted).
fn kwin_version() -> Option<Version> {
    let out = run(
        std::process::Command::new("kwin_wayland").arg("--version"),
        Duration::from_secs(2),
    )
    .ok()?;
    parse_version(&String::from_utf8_lossy(&out.stdout))
}

/// A path in a source tree's `target` directory: where `cargo build` puts a program.
fn built_here(exe: &Path) -> Option<String> {
    exe.components()
        .any(|c| c.as_os_str() == "target")
        .then(|| exe.to_string_lossy().into_owned())
}

/// What `connect` failed with, as an error that says what to do when the cause is
/// access to KWin's interface; any other failure as it was.
pub fn explain(error: anyhow::Error) -> anyhow::Error {
    let registry = if error.downcast_ref::<NotExposed>().is_some() {
        Registry::NotListed
    } else {
        Registry::Unknown
    };
    let desktops = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    // Looking for the version starts a program: only where it is of use.
    let kwin = if is_kde(&desktops) && registry == Registry::NotListed {
        kwin_version()
    } else {
        None
    };
    let inputs = Inputs {
        desktops,
        session: std::env::var("XDG_SESSION_TYPE").unwrap_or_default(),
        kwin,
        registry,
        built_here: std::env::current_exe().ok().as_deref().and_then(built_here),
    };
    match advise(&inputs) {
        Some(advice) => {
            warn!("{error:#} ({inputs:?})");
            anyhow::Error::new(Unusable(text(&advice)))
        }
        None => error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(major: u32, minor: u32, patch: u32) -> Version {
        Version {
            major,
            minor,
            patch,
        }
    }

    fn inputs(desktops: &str, session: &str, kwin: Option<Version>, registry: Registry) -> Inputs {
        Inputs {
            desktops: desktops.into(),
            session: session.into(),
            kwin,
            registry,
            built_here: None,
        }
    }

    #[test]
    fn kwin_versions_are_read_from_what_it_prints() {
        assert_eq!(parse_version("kwin 6.5.6\n"), Some(v(6, 5, 6)));
        assert_eq!(parse_version("kwin 6.8.3"), Some(v(6, 8, 3)));
        assert_eq!(parse_version("kwin 6.7.90"), Some(v(6, 7, 90)));
        assert_eq!(parse_version("kwin_wayland 6.0"), Some(v(6, 0, 0)));
        assert_eq!(parse_version("kwin 6.5.5-2"), Some(v(6, 5, 5)));
        assert_eq!(parse_version("kwin"), None);
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("kwin 6"), None);
    }

    #[test]
    fn another_desktop_or_a_listed_interface_is_no_problem_of_access() {
        for (desktops, registry) in [
            ("Hyprland", Registry::NotListed),
            ("GNOME", Registry::NotListed),
            ("", Registry::NotListed),
            ("KDE", Registry::Listed),
            ("ubuntu:GNOME", Registry::NotListed),
        ] {
            assert_eq!(
                advise(&inputs(desktops, "wayland", Some(v(6, 5, 6)), registry)),
                None,
                "{desktops} {registry:?}"
            );
        }
    }

    #[test]
    fn kde_up_to_6_7_needs_a_shortcut() {
        for kwin in [v(6, 0, 0), v(6, 2, 5), v(6, 5, 6), v(6, 7, 6)] {
            assert_eq!(
                advise(&inputs("KDE", "wayland", Some(kwin), Registry::NotListed)),
                Some(Advice::NeedsShortcut {
                    kwin: Some(kwin),
                    built_here: None
                }),
                "{kwin}"
            );
        }
    }

    #[test]
    fn an_unknown_version_is_taken_for_one_that_needs_a_shortcut() {
        assert_eq!(
            advise(&inputs("KDE", "wayland", None, Registry::NotListed)),
            Some(Advice::NeedsShortcut {
                kwin: None,
                built_here: None
            })
        );
    }

    #[test]
    fn from_6_8_no_shortcut_would_help() {
        for kwin in [v(6, 7, 90), v(6, 8, 0), v(6, 8, 3), v(7, 0, 0)] {
            assert_eq!(
                advise(&inputs("KDE", "wayland", Some(kwin), Registry::NotListed)),
                Some(Advice::Sandboxed { kwin }),
                "{kwin}"
            );
        }
    }

    #[test]
    fn a_build_in_a_source_tree_is_told_its_path() {
        let mut i = inputs("KDE", "wayland", Some(v(6, 5, 6)), Registry::NotListed);
        i.built_here = Some("/src/prostir/target/release/prostir".into());
        let Some(Advice::NeedsShortcut { built_here, .. }) = advise(&i) else {
            panic!("a shortcut is needed");
        };
        assert_eq!(
            built_here.as_deref(),
            Some("/src/prostir/target/release/prostir")
        );
        assert_eq!(
            super::built_here(Path::new("/src/prostir/target/debug/prostir")).as_deref(),
            Some("/src/prostir/target/debug/prostir")
        );
        assert_eq!(super::built_here(Path::new("/usr/bin/prostir")), None);
        assert_eq!(
            super::built_here(Path::new("/nix/store/x-prostir/bin/.prostir-wrapped")),
            None
        );
    }

    #[test]
    fn plasma_on_x11_is_not_a_shortcut_problem() {
        assert_eq!(
            advise(&inputs("KDE", "x11", Some(v(6, 5, 6)), Registry::Unknown)),
            Some(Advice::NotWayland)
        );
        assert_eq!(
            advise(&inputs("KDE", "x11", None, Registry::NotListed)),
            Some(Advice::NotWayland)
        );
    }

    #[test]
    fn a_compositor_that_was_not_reached_keeps_its_own_message() {
        assert_eq!(
            advise(&inputs("KDE", "wayland", None, Registry::Unknown)),
            None
        );
    }

    #[test]
    fn every_advice_has_a_text_that_says_what_to_do() {
        let built = "/src/prostir/target/release/prostir".to_owned();
        let advices = [
            Advice::NotWayland,
            Advice::NeedsShortcut {
                kwin: Some(v(6, 5, 6)),
                built_here: None,
            },
            Advice::NeedsShortcut {
                kwin: None,
                built_here: None,
            },
            Advice::NeedsShortcut {
                kwin: Some(v(6, 5, 6)),
                built_here: Some(built.clone()),
            },
            Advice::Sandboxed { kwin: v(6, 8, 3) },
        ];
        for a in advices {
            let t = text(&a);
            assert!(!t.is_empty() && !t.contains("pc-kde-"), "{a:?}: {t}");
            assert!(!t.contains('$'), "{a:?}: {t}");
        }
        let installed = text(&Advice::NeedsShortcut {
            kwin: Some(v(6, 5, 6)),
            built_here: None,
        });
        assert!(installed.contains("6.8"), "{installed}");
        assert!(installed.contains("6.5.6"), "{installed}");
        let unknown = text(&Advice::NeedsShortcut {
            kwin: None,
            built_here: None,
        });
        assert!(!unknown.contains("()"), "{unknown}");
        let source = text(&Advice::NeedsShortcut {
            kwin: Some(v(6, 5, 6)),
            built_here: Some(built.clone()),
        });
        assert!(source.contains("install-kde-shortcut.sh"), "{source}");
        assert!(source.contains(&built), "{source}");
    }
}
