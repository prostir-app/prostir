//! The monitors page: the map, and the monitor selected on it.

use super::*;

/// Sizes offered when the state names none.
const SIZES: [[u32; 2]; 5] = [
    [1920, 1080],
    [2560, 1440],
    [3840, 2160],
    [3440, 1440],
    [1440, 2560],
];

const SCALES: [u16; 3] = [100, 125, 150];

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
    let labels: Vec<String> = scales.iter().map(|sc| percent(tk, *sc)).collect();
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    let current = scales
        .iter()
        .position(|sc| *sc == d.mode.scale)
        .unwrap_or(usize::MAX);
    w::row(ui, tk, &tk.t("window-mon-scale"), None, |ui| {
        let can =
            !v.ui.unsupported.contains("set_scale") && !matches!(d.kind, DisplayKind::Physical);
        if let Some(i) = w::segmented(ui, tk, &labels, current, can) {
            v.out.push(Action::Send(Request::SetScale {
                id: d.id,
                scale: scales[i],
            }));
        }
    });
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
