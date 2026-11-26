//! The monitors page: the map, the monitor selected on it, and the profiles (saved
//! arrangements) of the headset connected.

use super::*;

/// Sizes offered when the state names none.
const SIZES: [[u32; 2]; 5] = [
    [1920, 1080],
    [2560, 1440],
    [3840, 2160],
    [3440, 1440],
    [1440, 2560],
];

const SCALES: [u16; 5] = [100, 125, 150, 175, 200];

pub(super) fn monitors(ui: &mut Ui, v: &mut View<'_, '_>, s: &State, _revision: u32) {
    let tk = v.tk;
    let p = tk.p;
    header(ui, v, Page::Monitors);
    let online = s.connected().is_some();
    let hint = if v.ui.arc_view {
        "window-mon-hint-arc"
    } else {
        "window-mon-hint-flat"
    };
    let views = [tk.t("window-mon-flat"), tk.t("window-mon-arc")];
    let views: Vec<&str> = views.iter().map(String::as_str).collect();
    if let Some(i) = w::segmented(ui, tk, &views, v.ui.arc_view as usize, true) {
        v.ui.arc_view = i == 1;
    }
    ui.add_space(tk.th.shape.spacing * 1.5);

    let tiles = tiles(tk, s, v.ui.pending_place);
    // The map takes a good part of the window, and leaves the controls below it in view
    // in the smallest one.
    let height = (ui.ctx().content_rect().height() * 0.36).clamp(200.0, 420.0);
    if tiles.is_empty() {
        w::card(ui, tk, |ui| {
            text(ui, tk.t("window-mon-empty"), tk.body(), p.text_muted);
        });
    } else {
        let events = if v.ui.arc_view {
            map::arc(ui, tk, &mut v.ui.map, &tiles, v.ui.selected, height)
        } else {
            map::flat(ui, tk, &mut v.ui.map, &tiles, v.ui.selected, height)
        };
        for e in events {
            match e {
                MapEvent::Select(id) => v.ui.selected = Some(id),
                MapEvent::Place { id, x, y } => v.send(Request::Place { id, x, y }),
            }
        }
        ui.add_space(tk.th.shape.spacing);
        text(ui, tk.t(hint), tk.small(), p.text_muted);
    }
    being_placed(ui, v, s);
    kept_monitors(ui, v, s);
    ui.add_space(tk.th.shape.section_gap);

    let virtual_count = s
        .displays
        .iter()
        .filter(|d| d.kind == DisplayKind::Virtual)
        .count() as u32;
    let full = s.limits.displays_max.is_some_and(|m| virtual_count >= m);
    ui.horizontal_top(|ui| {
        let total = ui.available_width();
        let gap = tk.th.shape.padding;
        let left = (total - gap) * 0.55;
        ui.allocate_ui_with_layout(vec2(left, 0.0), Layout::top_down(Align::Min), |ui| {
            w::section_title(ui, tk, &tk.t("window-mon-selected"));
            w::card(ui, tk, |ui| selected_monitor(ui, v, s));
        });
        ui.add_space(gap - tk.th.shape.spacing);
        ui.allocate_ui_with_layout(
            vec2(ui.available_width(), 0.0),
            Layout::top_down(Align::Min),
            |ui| {
                w::section_title(ui, tk, &tk.t("window-mon-add"));
                w::card(ui, tk, |ui| {
                    let note = if full {
                        tk.f("window-mon-add-full", &[("max", virtual_count.into())])
                    } else if !online {
                        tk.t("window-mon-add-offline")
                    } else {
                        tk.t("window-mon-add-desc")
                    };
                    text(ui, note, tk.small(), p.text_muted);
                    ui.add_space(tk.th.shape.spacing * 1.5);
                    let sizes = if s.limits.sizes.is_empty() {
                        &SIZES[..]
                    } else {
                        &s.limits.sizes[..]
                    };
                    ui.horizontal_wrapped(|ui| {
                        for [wd, ht] in sizes {
                            let label = tk.f(
                                "window-mon-size",
                                &[("w", (*wd).into()), ("h", (*ht).into())],
                            );
                            if w::button(ui, tk, &label, Kind::Secondary, !full).clicked() {
                                v.send(Request::AddMonitor {
                                    w: *wd,
                                    h: *ht,
                                    scale: 100,
                                });
                            }
                        }
                    });
                });
            },
        );
    });
    // The arrangements saved in a headset connected now that takes commands.
    for h in &s.headsets {
        if let Some(session) = h
            .session
            .as_ref()
            .filter(|s| s.commands && !s.profiles.is_empty())
        {
            profiles_card(ui, v, h, session);
        }
    }
}

/// Monitors just added that the headset has not placed yet: not on the map until it
/// has, a line each under it meanwhile.
fn being_placed(ui: &mut Ui, v: &mut View<'_, '_>, s: &State) {
    let tk = v.tk;
    let p = tk.p;
    let placing = s.displays.iter().filter(|d| d.placing);
    for d in placing {
        ui.add_space(tk.th.shape.spacing);
        let line = vec2(ui.available_width(), tk.th.shape.control_height);
        ui.allocate_ui_with_layout(line, Layout::left_to_right(Align::Center), |ui| {
            text(
                ui,
                tk.f("window-mon-placing", &[("name", (&d.name).into())]),
                tk.small(),
                p.text_muted,
            );
            let named = tk.f("window-mon-remove-named", &[("name", (&d.name).into())]);
            if w::button_named(
                ui,
                tk,
                &tk.t("window-mon-remove"),
                &named,
                Kind::Quiet,
                true,
            )
            .clicked()
            {
                v.out
                    .push(Action::Send(Request::RemoveMonitor { id: d.id }));
            }
        });
    }
}

fn selected_monitor(ui: &mut Ui, v: &mut View<'_, '_>, s: &State) {
    let tk = v.tk;
    let p = tk.p;
    let Some(d) = v.ui.selected.and_then(|id| s.display(id)) else {
        text(ui, tk.t("window-mon-none"), tk.body(), p.text_muted);
        return;
    };
    let mut name = display_name(tk, d);
    if d.kind == DisplayKind::Physical && !d.output.is_empty() && d.output != name {
        name = format!("{name} ({})", d.output);
    }
    text(ui, name, tk.heading(), p.text);
    let line = size_scaled(tk, &d.mode);
    if !line.is_empty() {
        text(ui, line, tk.small(), p.text_muted);
    }
    if let (DisplayState::Failed, Some(f)) = (d.state, &d.failure) {
        ui.add_space(tk.th.shape.spacing);
        w::status(
            ui,
            tk,
            p.danger,
            &tk.f("window-mon-failed", &[("reason", (&f.text).into())]),
        );
    }
    w::divider(ui, tk);
    let scales = if s.limits.scales.is_empty() {
        &SCALES[..]
    } else {
        &s.limits.scales[..]
    };
    // Auto, for Prostir's own monitors (never the PC's displays): a switch, the scale
    // it chose under it.
    let can = !v.ui.unsupported.contains("set_scale") && !matches!(d.kind, DisplayKind::Physical);
    if d.kind == DisplayKind::Virtual && !v.ui.unsupported.contains("set_scale_auto") {
        let label = tk.t("window-mon-scale-auto");
        let desc = if d.auto_scale {
            tk.f(
                "window-mon-scale-auto-at",
                &[("scale", d.mode.scale.into())],
            )
        } else {
            tk.t("window-mon-scale-auto-desc")
        };
        w::row(ui, tk, &label, Some(&desc), |ui| {
            if let Some(on) = w::switch(ui, tk, &label, d.auto_scale, true) {
                v.out
                    .push(Action::Send(Request::SetScaleAuto { id: d.id, on }));
            }
        });
        ui.add_space(tk.th.shape.padding * 0.5);
    }
    // More than three scales take a line of their own, as numbers.
    let wide = scales.len() > 3;
    let labels: Vec<String> = scales
        .iter()
        .map(|sc| {
            if wide {
                sc.to_string()
            } else {
                percent(tk, *sc)
            }
        })
        .collect();
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    let current = scales
        .iter()
        .position(|sc| *sc == d.mode.scale)
        .unwrap_or(usize::MAX);
    let chosen = if wide {
        text(ui, tk.t("window-mon-scale-percent"), tk.body_m(), p.text);
        ui.add_space(tk.th.shape.spacing * 0.5);
        let within = ui.available_width();
        w::segmented_within(ui, tk, &labels, current, can, within)
    } else {
        let mut chosen = None;
        w::row(ui, tk, &tk.t("window-mon-scale"), None, |ui| {
            chosen = w::segmented(ui, tk, &labels, current, can);
        });
        chosen
    };
    if let Some(i) = chosen {
        v.out.push(Action::Send(Request::SetScale {
            id: d.id,
            scale: scales[i],
        }));
    }
    w::divider(ui, tk);
    if d.kind == DisplayKind::Physical {
        let opts = [tk.t("window-mon-shown"), tk.t("window-mon-hidden")];
        let opts: Vec<&str> = opts.iter().map(String::as_str).collect();
        w::row(ui, tk, &tk.t("window-mon-in-headset"), None, |ui| {
            if let Some(i) = w::segmented(ui, tk, &opts, (!d.shown) as usize, true) {
                v.out.push(Action::Send(Request::ShowPhysical {
                    id: d.id,
                    shown: i == 0,
                }));
            }
        });
        w::divider(ui, tk);
    }
    let main = s.primary == Some(d.id);
    let label = tk.t("window-mon-main");
    w::row(ui, tk, &label, Some(&tk.t("window-mon-main-desc")), |ui| {
        // The main one cannot be switched off: another monitor is made main instead.
        if w::switch(ui, tk, &label, main, !main) == Some(true) {
            v.out.push(Action::Send(Request::SetPrimary { id: d.id }));
        }
    });
    if d.kind == DisplayKind::Virtual {
        ui.add_space(tk.th.shape.padding * 0.75);
        ui.horizontal(|ui| {
            if d.state == DisplayState::Failed
                && w::button(ui, tk, &tk.t("window-ov-retry"), Kind::Primary, true).clicked()
            {
                v.out.push(Action::Send(Request::RetryMonitor { id: d.id }));
            }
            if w::button(ui, tk, &tk.t("window-mon-remove"), Kind::Danger, true).clicked() {
                v.out
                    .push(Action::Send(Request::RemoveMonitor { id: d.id }));
            }
        });
    }
}

/// Whether `name` is a number only: what names a profile by its place, never a name.
fn is_number(name: &str) -> bool {
    let name = name.trim();
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit())
}

/// The most a profile's name takes, in bytes (as the headset protocol carries it).
const PROFILE_NAME_MAX: usize = 64;

/// `name` cut to [`PROFILE_NAME_MAX`] bytes at a character, without spaces around it.
fn profile_name_to_send(name: &str) -> String {
    let name = name.trim();
    let mut end = name.len().min(PROFILE_NAME_MAX);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    name[..end].trim_end().to_owned()
}

/// The profiles saved in headset `h`, in its menu's order: each by its name (or its
/// number, as the headset's menu calls one without a name), the one in use said
/// under it, and "Rename" to give it another name (an empty one gives it back its
/// number).
fn profiles_card(ui: &mut Ui, v: &mut View<'_, '_>, h: &Headset, session: &Session) {
    let tk = v.tk;
    let p = tk.p;
    ui.add_space(tk.th.shape.section_gap);
    w::section_title(
        ui,
        tk,
        &tk.f("window-hs-profiles", &[("name", (&h.name).into())]),
    );
    let can = !v.ui.unsupported.contains("rename_profile");
    let n = session.profiles.len();
    // The one in use: by its number where it has no name, else the first of its name.
    let active_row = session.profile.as_deref().and_then(|a| {
        if is_number(a) {
            a.parse::<usize>().ok()?.checked_sub(1)
        } else {
            session.profiles.iter().position(|p| p == a)
        }
    });
    // What names each to the headset: its name, else its number. The one renamed is
    // kept by it, so a list changed meanwhile renames that one or none; of two of one
    // name, the first, as the headset finds it.
    let ident = |i: usize, name: &String| {
        if name.is_empty() {
            (i + 1).to_string()
        } else {
            name.clone()
        }
    };
    let edit_row = v.ui.rename.as_ref().and_then(|(key, which, _)| {
        (*key == h.key)
            .then(|| {
                session
                    .profiles
                    .iter()
                    .enumerate()
                    .position(|(i, n)| ident(i, n) == *which)
            })
            .flatten()
    });
    w::card(ui, tk, |ui| {
        for (i, name) in session.profiles.iter().enumerate() {
            let ident = ident(i, name);
            let shown = if name.is_empty() {
                tk.f("window-hs-profile-n", &[("n", (i + 1).into())])
            } else {
                name.clone()
            };
            let active = active_row == Some(i);
            let editing = edit_row == Some(i);
            if editing {
                let label = tk.f("window-hs-profile-name", &[("name", shown.as_str().into())]);
                ui.horizontal(|ui| {
                    let enter = if let Some((_, _, typed)) = v.ui.rename.as_mut() {
                        let resp = ui.add(
                            egui::TextEdit::singleline(typed)
                                .font(tk.body())
                                .desired_width(280.0),
                        );
                        let value = typed.clone();
                        resp.widget_info(|| {
                            let mut info =
                                egui::WidgetInfo::text_edit(true, String::new(), value.clone(), "");
                            info.label = Some(label.clone());
                            info
                        });
                        resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
                    } else {
                        false
                    };
                    ui.add_space(tk.th.shape.spacing);
                    // A number only names a profile by its place: never a name.
                    let typed_number = v.ui.rename.as_ref().is_some_and(|r| is_number(&r.2));
                    let save = w::button(
                        ui,
                        tk,
                        &tk.t("window-hs-profile-save"),
                        Kind::Primary,
                        !typed_number,
                    );
                    if (save.clicked() || enter) && !typed_number {
                        let typed = v.ui.rename.take().map(|r| r.2).unwrap_or_default();
                        v.send(Request::RenameProfile {
                            profile: ident.clone(),
                            name: profile_name_to_send(&typed),
                        });
                    }
                    if w::button(ui, tk, &tk.t("window-hs-cancel"), Kind::Quiet, true).clicked() {
                        v.ui.rename = None;
                    }
                });
                if v.ui.rename.as_ref().is_some_and(|r| is_number(&r.2)) {
                    text(
                        ui,
                        tk.t("window-hs-profile-number"),
                        tk.small(),
                        p.text_muted,
                    );
                }
            } else {
                let desc = active.then(|| tk.t("window-hs-profile-in-use"));
                w::row(ui, tk, &shown, desc.as_deref(), |ui| {
                    let named = tk.f(
                        "window-hs-profile-rename-named",
                        &[("name", shown.as_str().into())],
                    );
                    if w::button_named(
                        ui,
                        tk,
                        &tk.t("window-hs-profile-rename"),
                        &named,
                        Kind::Secondary,
                        can,
                    )
                    .clicked()
                    {
                        v.ui.rename = Some((h.key.clone(), ident.clone(), name.clone()));
                    }
                });
            }
            if i + 1 < n {
                w::divider(ui, tk);
            }
        }
        ui.add_space(tk.th.shape.spacing);
        text(
            ui,
            tk.t("window-hs-profiles-note"),
            tk.small(),
            p.text_muted,
        );
    });
}
