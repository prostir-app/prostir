//! The overview page: the PC, the headset, the monitors at a glance.

use super::*;

pub(super) fn overview(ui: &mut Ui, v: &mut View<'_, '_>, s: &State) {
    let tk = v.tk;
    let p = tk.p;
    counting_question(ui, v, s);
    header(ui, v, Page::Overview);
    let gap = tk.th.shape.section_gap;
    let virtual_count = s
        .displays
        .iter()
        .filter(|d| d.kind == DisplayKind::Virtual && d.state != DisplayState::Failed)
        .count();
    let failed = s.displays.iter().find(|d| d.state == DisplayState::Failed);

    w::card(ui, tk, |ui| {
        if let Some((h, session)) = s.connected() {
            let full = ui.available_width();
            ui.allocate_ui_with_layout(vec2(full, 0.0), Layout::right_to_left(Align::Min), |ui| {
                if let Some(b) = session.battery {
                    ui.allocate_ui_with_layout(
                        vec2(170.0, 0.0),
                        Layout::top_down(Align::Max),
                        |ui| {
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                w::battery(ui, tk, b.percent as f32 / 100.0, b.charging);
                                ui.add_space(4.0);
                                text(
                                    ui,
                                    percent(tk, b.percent),
                                    tk.font(tk.th.type_.size_title, true),
                                    p.text,
                                );
                            });
                            let label = if b.charging {
                                "ov-charging"
                            } else {
                                "ov-battery"
                            };
                            text(ui, tk.t(label), tk.small(), p.text_muted);
                        },
                    );
                }
                ui.with_layout(Layout::top_down(Align::Min), |ui| {
                    let name = || ("name", Arg::from(&h.name));
                    if let Some(d) = failed {
                        hero(ui, tk, Some(p.danger), &tk.t("ov-failed-title"));
                        ui.add_space(6.0);
                        // A sentence of its own before "The other monitors work."
                        let reason = d
                            .failure
                            .as_ref()
                            .map_or(String::new(), |f| sentence(&f.text));
                        let reason = reason.as_str();
                        let id = if reason.is_empty() {
                            "ov-failed-no-reason"
                        } else {
                            "ov-failed-body"
                        };
                        let body = tk.f(
                            id,
                            &[
                                ("name", display_name(tk, d).into()),
                                ("reason", reason.into()),
                            ],
                        );
                        text(ui, body, tk.body(), p.text_muted);
                        ui.add_space(tk.th.shape.padding);
                        ui.horizontal(|ui| {
                            if w::button(ui, tk, &tk.t("ov-retry"), Kind::Primary, true).clicked() {
                                v.out.push(Action::Send(Request::RetryMonitor { id: d.id }));
                            }
                            if w::button(ui, tk, &tk.t("ov-open-monitors"), Kind::Secondary, true)
                                .clicked()
                            {
                                v.out.push(Action::Go(Page::Monitors));
                            }
                        });
                    } else if !session.worn {
                        hero(ui, tk, Some(p.text_muted), &tk.t("ov-taken-off-title"));
                        ui.add_space(6.0);
                        text(
                            ui,
                            tk.f("ov-taken-off-body", &[name()]),
                            tk.body(),
                            p.text_muted,
                        );
                    } else {
                        hero(ui, tk, Some(p.ok), &tk.t("ov-connected-title"));
                        ui.add_space(6.0);
                        let id = match session.via {
                            Via::Usb => "ov-connected-usb",
                            _ => "ov-connected-wifi",
                        };
                        let body = tk.f(id, &[name(), ("count", virtual_count.into())]);
                        text(ui, body, tk.body(), p.text_muted);
                        if let Some(wifi) = s.wifi.as_ref().filter(|w| w.power_save == Some(true))
                            && session.via != Via::Usb
                        {
                            ui.add_space(6.0);
                            let hint = tk.f(
                                "ov-wifi-power-save",
                                &[("interface", wifi.interface.clone().into())],
                            );
                            w::status(ui, tk, p.warning, &hint);
                        }
                    }
                });
            });
            w::divider(ui, tk);
            let mic = if !s.sound.mic_on {
                tk.t("off")
            } else {
                tk.t("on")
            };
            let facts = [
                (
                    tk.t("ov-fact-sound"),
                    tk.t(if s.sound.to_headset {
                        "sound-in-headset"
                    } else {
                        "sound-on-pc"
                    }),
                ),
                (tk.t("ov-fact-mic"), mic),
                (
                    tk.t("ov-fact-profile"),
                    session
                        .profile
                        .clone()
                        .unwrap_or_else(|| tk.t("ov-profile-none")),
                ),
            ];
            ui.columns(3, |c| {
                for (i, (k, val)) in facts.into_iter().enumerate() {
                    text(&mut c[i], k, tk.small(), p.text_muted);
                    text(&mut c[i], val, tk.body_m(), p.text);
                }
            });
        } else if let Some(name) = &s.connecting {
            ui.horizontal(|ui| {
                w::spinner(ui, tk, tk.th.type_.size_display);
                ui.add_space(4.0);
                text(
                    ui,
                    tk.f("ov-connecting-title", &[("name", name.into())]),
                    tk.font(tk.th.type_.size_display, true),
                    p.text,
                );
            });
            ui.add_space(6.0);
            text(ui, tk.t("ov-connecting-body"), tk.body(), p.text_muted);
        } else if s.headsets.is_empty() {
            hero(ui, tk, None, &tk.t("ov-no-headsets-title"));
            ui.add_space(6.0);
            text(ui, tk.t("ov-no-headsets-body"), tk.body(), p.text_muted);
            ui.add_space(tk.th.shape.padding);
            if w::button(ui, tk, &tk.t("ov-no-headsets-go"), Kind::Primary, true).clicked() {
                v.out.push(Action::Go(Page::FirstRun));
            }
        } else {
            hero(ui, tk, Some(p.text_muted), &tk.t("ov-waiting-title"));
            ui.add_space(6.0);
            text(ui, tk.t("ov-waiting-body"), tk.body(), p.text_muted);
            ui.add_space(tk.th.shape.padding);
            if w::button(ui, tk, &tk.t("ov-pair-new"), Kind::Secondary, true).clicked() {
                if s.pairing.is_none() {
                    v.out.push(Action::Send(Request::PairNewHeadset));
                }
                v.out.push(Action::Go(Page::Headsets));
            }
        }
    });

    if !s.problems.is_empty() {
        ui.add_space(gap);
        w::section_title(ui, tk, &tk.t("ov-problems"));
        w::card(ui, tk, |ui| {
            for pr in &s.problems {
                w::status(ui, tk, p.warning, &pr.text);
            }
        });
    }

    ui.add_space(gap);
    w::section_title(ui, tk, &tk.t("ov-monitors"));
    w::card(ui, tk, |ui| {
        if s.displays.is_empty() {
            text(ui, tk.t("ov-monitors-empty"), tk.body(), p.text_muted);
            return;
        }
        let n = s.displays.len();
        for (i, d) in s.displays.iter().enumerate() {
            let (c, state) = match (d.state, d.kind) {
                (DisplayState::Failed, _) => (p.danger, "state-failed"),
                (DisplayState::Starting, _) => (p.accent_ink, "state-starting"),
                (DisplayState::Absent, _) => (p.text_muted, "state-absent"),
                (DisplayState::Kept, _) => (p.text_muted, "state-kept"),
                (_, DisplayKind::Physical) if !d.shown => (p.text_muted, "state-hidden"),
                (_, DisplayKind::Physical) => (p.ok, "state-shown"),
                _ => (p.ok, "state-streaming"),
            };
            let desc = size(tk, &d.mode);
            let desc = (!desc.is_empty()).then_some(desc);
            w::row(ui, tk, &display_name(tk, d), desc.as_deref(), |ui| {
                text(ui, tk.t(state), tk.body(), p.text);
                let (r, _) = ui.allocate_exact_size(vec2(12.0, 20.0), Sense::hover());
                w::dot(ui, tk, r.center(), c);
            });
            if i + 1 < n {
                w::divider(ui, tk);
            }
        }
    });

    ui.add_space(gap);
    w::section_title(ui, tk, &tk.t("ov-pc"));
    w::card(ui, tk, |ui| {
        w::row(ui, tk, &tk.t("ov-pc-name"), None, |ui| {
            text(ui, &s.pc.name, tk.body(), p.text);
        });
        if !s.pc.desktop.is_empty() {
            w::divider(ui, tk);
            w::row(ui, tk, &tk.t("ov-pc-desktop"), None, |ui| {
                text(ui, &s.pc.desktop, tk.body(), p.text);
            });
        }
        if let Some(on) = s.autostart {
            w::divider(ui, tk);
            let label = tk.t("ov-autostart");
            let can = !v.ui.unsupported.contains("autostart");
            let desc = if can {
                tk.t("ov-autostart-desc")
            } else {
                tk.t("ov-autostart-here-not")
            };
            w::row(ui, tk, &label, Some(&desc), |ui| {
                if let Some(on) = w::switch(ui, tk, &label, on, can) {
                    v.out.push(Action::Send(Request::Autostart { on }));
                }
            });
        }
        // Once answered (until then the question at the top asks), and only where
        // this build and run count at all: not for an answer this window does not
        // know (an older or newer Prostir).
        if let Some(c) = s
            .counting
            .filter(|c| matches!(c, Counting::Yes | Counting::No))
        {
            w::divider(ui, tk);
            let label = tk.t("ov-counting");
            let can = !v.ui.unsupported.contains("set_counting");
            w::row(ui, tk, &label, Some(&tk.t("ov-counting-desc")), |ui| {
                if let Some(on) = w::switch(ui, tk, &label, c == Counting::Yes, can) {
                    let counting = if on { Counting::Yes } else { Counting::No };
                    v.out.push(Action::Send(Request::SetCounting { counting }));
                }
            });
        }
    });
}

pub(super) fn tiles(
    tk: &Tk<'_>,
    s: &State,
    pending: Option<(u32, i32, i32, Option<u32>)>,
) -> Vec<Tile> {
    s.displays
        .iter()
        .filter_map(|d| {
            // Every display on the desktop: one kept for the next headset is not, nor
            // one of the PC that is not connected (neither has a place there).
            let r = d.desk?;
            if d.state == DisplayState::Kept || r.w == 0 || r.h == 0 {
                return None;
            }
            let mut rect = snap::Rect {
                x: r.x,
                y: r.y,
                w: r.w as i32,
                h: r.h as i32,
            };
            if let Some((id, x, y, _)) = pending
                && id == d.id
            {
                rect = rect.at((x, y));
            }
            Some(Tile {
                id: d.id,
                name: display_name(tk, d),
                size: tile_size(tk, &d.mode),
                rect,
                pc: d.kind == DisplayKind::Physical,
                // Dimmed: hidden from the headset, or not streaming to it.
                shown: (d.kind != DisplayKind::Physical || d.shown)
                    && d.state != DisplayState::Failed,
                main: s.primary == Some(d.id),
            })
        })
        .collect()
}
