//! The look page: theme, palette, language and the size of things.

use super::*;

pub(super) fn look(ui: &mut Ui, v: &mut View<'_, '_>) {
    let tk = v.tk;
    let p = tk.p;
    header(ui, v, Page::Look);
    let live = matches!(v.conn, Conn::Live { .. });
    let current = v.look.clone();
    w::card(ui, tk, |ui| {
        let modes = [ThemeMode::System, ThemeMode::Light, ThemeMode::Dark];
        let labels = [
            tk.t("window-look-system"),
            tk.t("window-look-light"),
            tk.t("window-look-dark"),
        ];
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        let now = (current.theme == ThemeMode::System).then(|| match v.system {
            Some(Scheme::Dark) => tk.t("window-look-now-dark"),
            Some(Scheme::Light) => tk.t("window-look-now-light"),
            None => tk.t("window-look-now-none"),
        });
        let sel = modes.iter().position(|m| *m == current.theme).unwrap_or(0);
        let current = WindowLook {
            theme: modes[sel],
            ..current
        };
        w::row(ui, tk, &tk.t("window-look-theme"), now.as_deref(), |ui| {
            if let Some(i) = w::segmented(ui, tk, &labels, sel, true) {
                v.out.push(Action::Look(WindowLook {
                    theme: modes[i],
                    ..current.clone()
                }));
            }
        });
        w::divider(ui, tk);
        let labels = [
            tk.t("window-look-palette-ink"),
            tk.t("window-look-palette-graphite"),
        ];
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        let sel = PaletteName::ALL
            .iter()
            .position(|p| p.id() == current.palette)
            .unwrap_or(0);
        w::row(
            ui,
            tk,
            &tk.t("window-look-palette"),
            Some(&tk.t("window-look-palette-desc")),
            |ui| {
                if let Some(i) = w::segmented(ui, tk, &labels, sel, true) {
                    v.out.push(Action::Look(WindowLook {
                        palette: PaletteName::ALL[i].id().to_owned(),
                        ..current.clone()
                    }));
                }
            },
        );
        w::divider(ui, tk);
        let code = tk.s.code().to_owned();
        let more = v.lang_dirs.first().map(|d| {
            tk.f(
                "window-look-lang-more",
                &[("dir", prostir_lang::short_path(d).into())],
            )
        });
        w::row(ui, tk, &tk.t("window-look-lang"), more.as_deref(), |ui| {
            // Each language is named in itself.
            let names: Vec<&str> = v.languages.iter().map(|l| l.name.as_str()).collect();
            let sel = v.languages.iter().position(|l| l.code == code).unwrap_or(0);
            let picked = if names.len() <= 4 {
                w::segmented(ui, tk, &names, sel, true)
            } else {
                let mut i = sel;
                egui::ComboBox::from_id_salt("language")
                    .selected_text(names.get(sel).copied().unwrap_or(""))
                    .show_index(ui, &mut i, names.len(), |k| names[k]);
                (i != sel).then_some(i)
            };
            if let Some(i) = picked {
                v.out.push(Action::Look(WindowLook {
                    language: Some(v.languages[i].code.clone()),
                    ..current.clone()
                }));
            }
        });
        if !live {
            w::divider(ui, tk);
            w::status(ui, tk, p.text_muted, &tk.t("window-look-local"));
        }
    });

    let Some(path) = v.theme_file else { return };
    ui.add_space(tk.th.shape.section_gap);
    w::section_title(ui, tk, &tk.t("window-look-file"));
    w::card(ui, tk, |ui| {
        let file = path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        let dir = path.parent().unwrap_or(path).to_path_buf();
        let shown_dir = prostir_lang::short_path(&dir);
        w::row(ui, tk, &file, Some(&shown_dir), |ui| {
            if w::button(ui, tk, &tk.t("window-look-open"), Kind::Secondary, true).clicked() {
                v.out.push(Action::OpenFolder(dir.clone()));
            }
            if !v.theme_file_exists
                && w::button(ui, tk, &tk.t("window-look-create"), Kind::Secondary, true).clicked()
            {
                v.out.push(Action::CreateThemeFile(path.to_path_buf()));
            }
        });
        w::divider(ui, tk);
        let th = tk.th;
        if !v.theme_file_exists {
            w::status(ui, tk, p.text_muted, &tk.t("window-look-file-none"));
        } else if th.issues.is_empty() && th.contrast.is_empty() {
            w::status(ui, tk, p.ok, &tk.t("window-look-file-ok"));
        }
        if !th.issues.is_empty() {
            w::status(ui, tk, p.warning, &tk.t("window-look-file-issues"));
            for i in &th.issues {
                let line = match &i.kind {
                    IssueKind::NotToml(why) => {
                        tk.f("window-issue-not-toml", &[("why", why.into())])
                    }
                    IssueKind::Format(value) => {
                        tk.f("window-issue-format", &[("value", value.into())])
                    }
                    IssueKind::BadValue(value) => tk.f(
                        "window-issue-bad",
                        &[("at", (&i.at).into()), ("value", value.into())],
                    ),
                    IssueKind::Unknown => tk.f("window-issue-unknown", &[("at", (&i.at).into())]),
                    IssueKind::Font(why) => tk.f(
                        "window-issue-font",
                        &[("at", (&i.at).into()), ("why", why.into())],
                    ),
                    IssueKind::Unreadable(why) => {
                        tk.f("window-issue-unreadable", &[("why", why.into())])
                    }
                };
                indented(ui, tk, line);
            }
        }
        if !th.contrast.is_empty() {
            ui.add_space(tk.th.shape.spacing);
            w::status(ui, tk, p.warning, &tk.t("window-look-file-contrast"));
            for c in &th.contrast {
                let scheme = match c.scheme {
                    Scheme::Light => tk.t("window-scheme-light"),
                    Scheme::Dark => tk.t("window-scheme-dark"),
                };
                let line = tk.f(
                    "window-contrast-line",
                    &[
                        ("scheme", scheme.into()),
                        ("fg", c.fg.into()),
                        ("bg", c.bg.into()),
                        ("ratio", decimal(tk, &format!("{:.1}", c.ratio)).into()),
                        ("need", decimal(tk, &format!("{}", c.need)).into()),
                    ],
                );
                indented(ui, tk, line);
            }
        }
    });
}
