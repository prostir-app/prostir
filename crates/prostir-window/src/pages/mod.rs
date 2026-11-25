//! The pages. Each draws from the service's state and says what it wants done as
//! [`Action`]s; the window carries them out after the frame.

use std::path::{Path, PathBuf};

use eframe::egui::{self, Align, Color32, CornerRadius, Layout, Rect, Sense, Ui, pos2, vec2};
use prostir_lang::{Arg, Language};

use crate::api::*;
use crate::app::{Conn, Outcome, Page, Start, UiState};
use crate::map::{self, MapEvent, Tile};
use crate::snap;
use crate::theme::{IssueKind, PaletteName, Scheme};
use crate::widgets::{self as w, Kind, Tk, text};

mod first_run;
mod headsets;
mod look;
mod monitors;
mod overview;
mod sound;

use first_run::first_run;
use headsets::{headsets, kept_monitors, pair_body};
use look::look;
use monitors::monitors;
use overview::{overview, tiles};
use sound::{quality, sound};

/// Where the releases are, for "Install Prostir in the headset".
pub const RELEASES: &str = "https://github.com/prostir-app/prostir/releases";

pub enum Action {
    Send(Request),
    /// Ask the service for its whole state again.
    Refresh,
    Go(Page),
    Look(WindowLook),
    Start,
    OpenUrl(String),
    OpenFolder(PathBuf),
    CreateThemeFile(PathBuf),
    Dismiss,
    /// Quit the whole program, as the tray's "Quit" does; the window closes with it.
    QuitProstir,
}

/// What a page sees and where it puts what it wants.
pub struct View<'a, 'b> {
    pub tk: &'a Tk<'b>,
    pub conn: &'a Conn,
    pub ui: &'a mut UiState,
    pub out: &'a mut Vec<Action>,
    /// The window's time in seconds.
    pub now: f64,
    pub now_unix: u64,
    pub look: &'a WindowLook,
    pub languages: &'a [Language],
    pub lang_dirs: &'a [PathBuf],
    pub system: Option<Scheme>,
    pub theme_file: Option<&'a Path>,
    pub theme_file_exists: bool,
}

impl View<'_, '_> {
    fn send(&mut self, r: Request) {
        self.out.push(Action::Send(r));
    }
}

pub fn show(ui: &mut Ui, v: &mut View<'_, '_>) {
    let page = v.ui.page;
    if page == Page::Look {
        return look(ui, v);
    }
    let conn = v.conn;
    match conn {
        Conn::Live { snap, .. } => {
            let s = &snap.state;
            match page {
                Page::Overview => overview(ui, v, s),
                Page::Monitors => monitors(ui, v, s, snap.revision),
                Page::Headsets => headsets(ui, v, s),
                Page::Sound => sound(ui, v, s),
                Page::Quality => quality(ui, v, s),
                Page::FirstRun => first_run(ui, v, s),
                Page::Look => unreachable!(),
            }
        }
        Conn::Loading => {
            header(ui, v, page);
            w::card(ui, v.tk, |ui| {
                ui.horizontal(|ui| {
                    w::spinner(ui, v.tk, v.tk.th.type_.size_title);
                    text(
                        ui,
                        v.tk.t("window-loading-title"),
                        v.tk.heading(),
                        v.tk.p.text,
                    );
                });
            });
        }
        Conn::Absent { why, start } => {
            header(ui, v, page);
            not_running(ui, v, why, start);
        }
    }
}

fn header(ui: &mut Ui, v: &View<'_, '_>, page: Page) {
    let (title, sub) = match page {
        Page::Overview => ("window-ov-title", None),
        Page::Monitors => ("window-mon-title", Some("window-mon-sub")),
        Page::Headsets => ("window-hs-title", Some("window-hs-sub")),
        Page::Sound => ("window-snd-title", Some("window-snd-sub")),
        Page::Quality => ("window-q-title", Some("window-q-sub")),
        Page::Look => ("window-look-title", Some("window-look-sub")),
        Page::FirstRun => ("window-fr-title", None),
    };
    let sub = sub.map(|s| v.tk.t(s));
    w::page_header(ui, v.tk, &v.tk.t(title), sub.as_deref());
}

/// "Count this PC among users?", above everything else on the overview and the first
/// run until it is answered: why it helps, what would be sent, and two equal answers
/// (the same accent buttons, the same size, neither chosen or focused before). The
/// rest of the page works meanwhile; nothing is counted until the answer is yes.
fn counting_question(ui: &mut Ui, v: &mut View<'_, '_>, s: &State) {
    if s.counting != Some(Counting::Unasked) {
        return;
    }
    let tk = v.tk;
    let p = tk.p;
    w::card_accent(ui, tk, |ui| {
        ui.horizontal(|ui| {
            w::count_icon(ui, tk, tk.th.type_.size_heading);
            ui.add_space(tk.th.shape.spacing);
            text(ui, tk.t("window-count-ask-title"), tk.heading(), p.text);
        });
        ui.add_space(tk.th.shape.spacing);
        text(ui, tk.t("window-count-ask-body"), tk.body(), p.text);
        ui.add_space(tk.th.shape.spacing * 0.5);
        text(ui, tk.t("window-count-ask-later"), tk.small(), p.text_muted);
        ui.add_space(tk.th.shape.padding);
        let full = ui.available_width();
        ui.allocate_ui_with_layout(
            vec2(full, tk.th.shape.control_height),
            Layout::left_to_right(Align::Center),
            |ui| {
                let (yes, no) = (tk.t("window-count-ask-yes"), tk.t("window-count-ask-no"));
                let names = (
                    tk.t("window-count-ask-yes-name"),
                    tk.t("window-count-ask-no-name"),
                );
                let answers = [(yes.as_str(), names.0.as_str()), (&no, &names.1)];
                let counting = match w::equal_buttons(ui, tk, &answers, Kind::Primary) {
                    Some(0) => Some(Counting::Yes),
                    Some(_) => Some(Counting::No),
                    None => None,
                };
                if let Some(counting) = counting {
                    v.out.push(Action::Send(Request::SetCounting { counting }));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if w::link(ui, tk, &tk.t("window-count-ask-privacy")).clicked() {
                        v.out
                            .push(Action::OpenUrl(tk.t("window-count-ask-privacy-url")));
                    }
                });
            },
        );
    });
    ui.add_space(tk.th.shape.section_gap);
}

/// A large title with a coloured dot for the state in front of it.
fn hero(ui: &mut Ui, tk: &Tk<'_>, color: Option<Color32>, title: &str) {
    ui.horizontal(|ui| {
        if let Some(c) = color {
            let (r, _) =
                ui.allocate_exact_size(vec2(16.0, tk.th.type_.size_display * 1.2), Sense::hover());
            ui.painter()
                .circle_filled(r.center(), tk.th.type_.size_body * 0.4, c);
            ui.add_space(4.0);
        }
        text(
            ui,
            title,
            tk.font(tk.th.type_.size_display, true),
            tk.p.text,
        );
    });
}

fn not_running(ui: &mut Ui, v: &mut View<'_, '_>, why: &ClientError, start: &Start) {
    let tk = v.tk;
    let p = tk.p;
    w::card(ui, tk, |ui| {
        if let ClientError::NoBus(_) = why {
            hero(ui, tk, Some(p.danger), &tk.t("window-nr-title"));
            ui.add_space(6.0);
            text(ui, tk.t("window-nr-no-bus"), tk.body(), p.text_muted);
            return;
        }
        if *why == ClientError::TooOld {
            hero(ui, tk, Some(p.warning), &tk.t("window-nr-old-title"));
            ui.add_space(6.0);
            text(ui, tk.t("window-nr-old-body"), tk.body(), p.text_muted);
            ui.add_space(tk.th.shape.padding);
            if w::button(ui, tk, &tk.t("window-nr-retry"), Kind::Secondary, true).clicked() {
                v.out.push(Action::Refresh);
            }
            return;
        }
        if silent(why) {
            hero(ui, tk, Some(p.warning), &tk.t("window-nr-silent-title"));
            ui.add_space(6.0);
            text(ui, tk.t("window-nr-silent-body"), tk.body(), p.text_muted);
            ui.add_space(tk.th.shape.padding);
            if w::button(ui, tk, &tk.t("window-nr-retry"), Kind::Secondary, true).clicked() {
                v.out.push(Action::Refresh);
            }
            return;
        }
        match start {
            Start::Waiting { .. } => {
                ui.horizontal(|ui| {
                    w::spinner(ui, tk, tk.th.type_.size_display);
                    ui.add_space(4.0);
                    text(
                        ui,
                        tk.t("window-nr-starting"),
                        tk.font(tk.th.type_.size_display, true),
                        p.text,
                    );
                });
                ui.add_space(6.0);
                text(ui, tk.t("window-nr-body"), tk.body(), p.text_muted);
            }
            Start::AlreadyRunning => {
                hero(ui, tk, Some(p.warning), &tk.t("window-nr-old-title"));
                ui.add_space(6.0);
                text(ui, tk.t("window-nr-old-body"), tk.body(), p.text_muted);
                ui.add_space(tk.th.shape.padding);
                if w::button(ui, tk, &tk.t("window-nr-retry"), Kind::Secondary, true).clicked() {
                    v.out.push(Action::Refresh);
                }
            }
            Start::Idle | Start::NoProgram | Start::TimedOut => {
                let failed = *start != Start::Idle;
                let title = if failed {
                    "window-nr-failed-title"
                } else {
                    "window-nr-title"
                };
                hero(
                    ui,
                    tk,
                    Some(if failed { p.danger } else { p.warning }),
                    &tk.t(title),
                );
                ui.add_space(6.0);
                let body = match start {
                    Start::NoProgram => "window-nr-no-program",
                    Start::TimedOut => "window-nr-failed-body",
                    _ => "window-nr-body",
                };
                text(ui, tk.t(body), tk.body(), p.text_muted);
                ui.add_space(tk.th.shape.padding);
                if w::button(ui, tk, &tk.t("window-nr-start"), Kind::Primary, true).clicked() {
                    v.out.push(Action::Start);
                }
            }
        }
    });
}

/// The service is there but did not answer (rather than not there at all).
pub fn silent(why: &ClientError) -> bool {
    matches!(
        why,
        ClientError::TimedOut | ClientError::Broken(_) | ClientError::Refused { .. }
    )
}

fn percent(tk: &Tk<'_>, n: impl Into<Arg>) -> String {
    tk.f("window-percent", &[("n", n.into())])
}

/// `1920 × 1080`; nothing for a size not known (a display of the PC not seen yet).
fn size(tk: &Tk<'_>, m: &Mode) -> String {
    if m.w == 0 || m.h == 0 {
        return String::new();
    }
    tk.f("window-mon-size", &[("w", m.w.into()), ("h", m.h.into())])
}

/// On a tile of the map: `2560 × 1440`, and `2560 × 1440 · 125 %` with a scale (the
/// tile is as large as the desktop has it, smaller at a larger scale).
fn tile_size(tk: &Tk<'_>, m: &Mode) -> String {
    if m.w == 0 || m.h == 0 || m.scale == 100 {
        return size(tk, m);
    }
    tk.f(
        "window-mon-size-at-scale",
        &[
            ("w", m.w.into()),
            ("h", m.h.into()),
            ("scale", m.scale.into()),
        ],
    )
}

/// `1920 × 1080, 125 %`; nothing for a size not known.
fn size_scaled(tk: &Tk<'_>, m: &Mode) -> String {
    if m.w == 0 || m.h == 0 {
        return String::new();
    }
    tk.f(
        "window-mon-size-scaled",
        &[
            ("w", m.w.into()),
            ("h", m.h.into()),
            ("scale", m.scale.into()),
        ],
    )
}

/// `text` as a sentence: a full stop after it unless it ends in one already.
fn sentence(text: &str) -> String {
    let text = text.trim();
    if text.is_empty() || text.ends_with(['.', '!', '?', '…']) {
        text.to_owned()
    } else {
        format!("{text}.")
    }
}

/// How a display is named to a person.
fn display_name(tk: &Tk<'_>, d: &Display) -> String {
    match (d.kind, d.builtin) {
        (DisplayKind::Physical, true) => tk.t("window-mon-laptop"),
        (DisplayKind::Physical, false) if d.name.is_empty() => tk.t("window-mon-pc-screen"),
        _ => d.name.clone(),
    }
}

fn ago(tk: &Tk<'_>, now: u64, then: u64) -> String {
    let s = now.saturating_sub(then);
    match s {
        0..60 => tk.t("window-ago-now"),
        60..3600 => tk.f("window-ago-minutes", &[("n", (s / 60).into())]),
        3600..86400 => tk.f("window-ago-hours", &[("n", (s / 3600).into())]),
        _ => tk.f("window-ago-days", &[("n", (s / 86400).into())]),
    }
}

/// A number written with a point, with the language's decimal separator.
fn decimal(tk: &Tk<'_>, n: &str) -> String {
    n.replace('.', &tk.t("window-decimal-separator"))
}

fn indented(ui: &mut Ui, tk: &Tk<'_>, line: String) {
    ui.horizontal(|ui| {
        ui.add_space(tk.th.shape.padding);
        text(ui, line, tk.small(), tk.p.text);
    });
}
