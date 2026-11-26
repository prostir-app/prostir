//! The headsets page and pairing: the pairing window and its code, what a pairing
//! came to, and the headsets paired.

use super::*;

fn countdown(left: f64) -> String {
    let s = left.max(0.0).ceil() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// The pairing window's content: the code to type here or there, or what to do first
/// (with the PC's `addresses`, for a headset that does not find the PC by itself).
/// Shared by the Headsets page and the first run.
pub(super) fn pair_body(
    ui: &mut Ui,
    v: &mut View<'_, '_>,
    pr: &Pairing,
    addresses: &[String],
    revision: u32,
) {
    let tk = v.tk;
    let p = tk.p;
    if let Some(code) = &pr.pc_code {
        text(ui, tk.t("window-pair-pc-code"), tk.body(), p.text_muted);
        ui.add_space(tk.th.shape.spacing);
        w::code_display(ui, tk, code);
    } else if let Some(name) = &pr.code_for {
        text(
            ui,
            tk.f("window-pair-type", &[("name", name.into())]),
            tk.body(),
            p.text_muted,
        );
        ui.add_space(tk.th.shape.spacing);
        let checking =
            v.ui.submitted
                .as_ref()
                .is_some_and(|(c, until)| *c == v.ui.code && until.is_none_or(|r| revision < r));
        let label = tk.t("window-pair-code");
        let enter = w::code_field(ui, tk, &label, &mut v.ui.code, !checking);
        let wrong = pr.last == Some(PairResult::Wrong)
            && v.ui
                .submitted
                .as_ref()
                .is_some_and(|(c, _)| *c == v.ui.code);
        if wrong && !checking {
            ui.add_space(tk.th.shape.spacing);
            text(ui, tk.t("window-pair-wrong"), tk.body(), p.danger);
        }
        ui.add_space(tk.th.shape.padding);
        ui.horizontal(|ui| {
            if checking {
                w::spinner(ui, tk, 22.0);
                text(ui, tk.t("window-pair-checking"), tk.body(), p.text);
            } else {
                let ready = v.ui.code.len() == 6 && !wrong;
                if (w::button(ui, tk, &tk.t("window-pair-go"), Kind::Primary, ready).clicked()
                    || (enter && ready))
                    && ready
                {
                    v.out.push(Action::Send(Request::PairingCode {
                        code: v.ui.code.clone(),
                    }));
                }
            }
        });
    } else {
        text(ui, tk.t("window-pair-wait"), tk.body(), p.text_muted);
        ui.add_space(tk.th.shape.padding);
        ui.horizontal(|ui| {
            w::spinner(ui, tk, 22.0);
            text(ui, tk.t("window-pair-waiting"), tk.body(), p.text);
        });
        if !addresses.is_empty() {
            ui.add_space(tk.th.shape.spacing);
            let list = addresses.join(", ");
            text(
                ui,
                tk.f("window-pair-address", &[("address", list.into())]),
                tk.small(),
                p.text_muted,
            );
        }
        ui.add_space(tk.th.shape.spacing);
        text(ui, tk.t("window-pair-firewall"), tk.small(), p.text_muted);
    }
}

fn pairing_card(
    ui: &mut Ui,
    v: &mut View<'_, '_>,
    pr: &Pairing,
    addresses: &[String],
    revision: u32,
) {
    let tk = v.tk;
    let p = tk.p;
    let left = (v.ui.pair.ends - v.now).max(0.0);
    // 0: pairing has no time limit (a new headset pairs whenever it asks).
    let limited = pr.seconds_total > 0;
    let total = pr.seconds_total.max(1) as f64;
    w::card(ui, tk, |ui| {
        ui.horizontal(|ui| {
            text(ui, tk.t("window-pair-title"), tk.heading(), p.text);
            if limited {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    text(
                        ui,
                        tk.f("window-pair-left", &[("time", countdown(left).into())]),
                        tk.body_m(),
                        p.text_muted,
                    );
                });
            }
        });
        ui.add_space(tk.th.shape.spacing);
        if limited {
            // The time left as a line that shrinks: pairing closes by itself.
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 3.0), Sense::hover());
            ui.painter()
                .rect_filled(r, CornerRadius::same(2), p.control);
            let mut f = r;
            f.set_right(r.left() + r.width() * (left / total).clamp(0.0, 1.0) as f32);
            ui.painter()
                .rect_filled(f, CornerRadius::same(2), p.accent_ink);
        }
        ui.add_space(tk.th.shape.padding);
        pair_body(ui, v, pr, addresses, revision);
        if !v.ui.unsupported.contains("stop_pairing") {
            ui.add_space(tk.th.shape.spacing);
            if w::button(ui, tk, &tk.t("window-pair-stop"), Kind::Quiet, true).clicked() {
                v.send(Request::StopPairing);
            }
        }
    });
}

/// "Pair a new headset", or where this Prostir does not open pairing from the window
/// (before step 6.8), what to do instead.
fn pair_new(ui: &mut Ui, v: &mut View<'_, '_>) {
    let tk = v.tk;
    if v.ui.unsupported.contains("pair_new_headset") {
        w::status(ui, tk, tk.p.text_muted, &tk.t("window-hs-pair-hint"));
    } else if w::button(ui, tk, &tk.t("window-ov-pair-new"), Kind::Primary, true).clicked() {
        v.send(Request::PairNewHeadset);
    }
}

/// Monitors kept for the next headset: not on the desktop, so not on the map.
pub(super) fn kept_monitors(ui: &mut Ui, v: &mut View<'_, '_>, s: &State) {
    let tk = v.tk;
    let p = tk.p;
    let kept: Vec<&Display> = s
        .displays
        .iter()
        .filter(|d| d.state == DisplayState::Kept)
        .collect();
    if kept.is_empty() {
        return;
    }
    ui.add_space(tk.th.shape.section_gap);
    w::section_title(ui, tk, &tk.t("window-mon-kept"));
    w::card(ui, tk, |ui| {
        text(ui, tk.t("window-mon-kept-desc"), tk.small(), p.text_muted);
        for d in kept {
            w::divider(ui, tk);
            let desc = size_scaled(tk, &d.mode);
            w::row(ui, tk, &display_name(tk, d), Some(&desc), |ui| {
                let named = tk.f("window-mon-remove-named", &[("name", (&d.name).into())]);
                if w::button_named(
                    ui,
                    tk,
                    &tk.t("window-mon-remove"),
                    &named,
                    Kind::Danger,
                    true,
                )
                .clicked()
                {
                    v.out
                        .push(Action::Send(Request::RemoveMonitor { id: d.id }));
                }
            });
        }
    });
}

fn outcome_card(ui: &mut Ui, v: &mut View<'_, '_>, o: &Outcome) {
    let tk = v.tk;
    let p = tk.p;
    w::card(ui, tk, |ui| {
        match o {
            Outcome::Paired(name) => {
                w::status(
                    ui,
                    tk,
                    p.ok,
                    &tk.f("window-pair-done", &[("name", name.into())]),
                );
            }
            Outcome::Expired => {
                text(ui, tk.t("window-pair-expired"), tk.body(), p.text_muted);
            }
        }
        ui.add_space(tk.th.shape.padding);
        ui.horizontal(|ui| {
            if *o == Outcome::Expired
                && w::button(ui, tk, &tk.t("window-pair-reopen"), Kind::Primary, true).clicked()
            {
                v.send(Request::PairNewHeadset);
            }
            if w::button(ui, tk, &tk.t("window-pair-close"), Kind::Quiet, true).clicked() {
                v.ui.pair.outcome = None;
            }
        });
    });
}

/// A headset's mouse and keyboard on this PC: the question while it waits for an
/// answer, and the switch.
fn input_right(ui: &mut Ui, v: &mut View<'_, '_>, h: &Headset) {
    if v.ui.unsupported.contains("set_input") {
        return;
    }
    let tk = v.tk;
    let p = tk.p;
    let on = h.rights.iter().any(|r| r == "input");
    let set = |on| {
        Action::Send(Request::SetInput {
            headset: h.key.clone(),
            on,
        })
    };
    ui.add_space(tk.th.shape.padding);
    if h.asks_input && !on {
        text(
            ui,
            tk.f("window-hs-input-ask", &[("name", (&h.name).into())]),
            tk.body_m(),
            p.text,
        );
        ui.add_space(tk.th.shape.spacing);
        ui.horizontal(|ui| {
            if w::button(ui, tk, &tk.t("window-hs-input-allow"), Kind::Primary, true).clicked() {
                v.out.push(set(true));
            }
            if w::button(ui, tk, &tk.t("window-hs-input-later"), Kind::Quiet, true).clicked() {
                v.out.push(set(false));
            }
        });
        ui.add_space(tk.th.shape.padding);
    }
    let label = tk.t("window-hs-input");
    w::row(ui, tk, &label, Some(&tk.t("window-hs-input-desc")), |ui| {
        if let Some(on) = w::switch(ui, tk, &label, on, true) {
            v.out.push(set(on));
        }
    });
}

pub(super) fn headsets(ui: &mut Ui, v: &mut View<'_, '_>, s: &State) {
    let tk = v.tk;
    let p = tk.p;
    header(ui, v, Page::Headsets);
    // Where a headset that does not find the PC by itself reaches it (the pairing card
    // says it there).
    if s.pairing.is_none() && !s.pc.addresses.is_empty() {
        let list = s.pc.addresses.join(", ");
        text(
            ui,
            tk.f("window-hs-address", &[("address", list.into())]),
            tk.small(),
            p.text_muted,
        );
        ui.add_space(tk.th.shape.spacing);
    }
    let revision = match v.conn {
        Conn::Live { snap, .. } => snap.revision,
        _ => 0,
    };
    let gap = tk.th.shape.section_gap;
    if let Some(pr) = &s.pairing {
        pairing_card(ui, v, pr, &s.pc.addresses, revision);
        ui.add_space(gap);
    } else if let Some(o) = v.ui.pair.outcome.clone() {
        outcome_card(ui, v, &o);
        ui.add_space(gap);
    } else if !s.headsets.is_empty() {
        pair_new(ui, v);
        ui.add_space(gap);
    }
    if s.headsets.is_empty() {
        if s.pairing.is_none() {
            w::card(ui, tk, |ui| {
                text(ui, tk.t("window-hs-empty-title"), tk.heading(), p.text);
                ui.add_space(6.0);
                text(ui, tk.t("window-hs-empty-body"), tk.body(), p.text_muted);
                ui.add_space(tk.th.shape.padding);
                pair_new(ui, v);
            });
        }
        return;
    }
    let n = s.headsets.len();
    w::card(ui, tk, |ui| {
        for (i, h) in s.headsets.iter().enumerate() {
            if v.ui.forget_ask.as_deref() == Some(h.key.as_str()) {
                text(
                    ui,
                    tk.f("window-hs-forget-ask", &[("name", (&h.name).into())]),
                    tk.body_m(),
                    p.text,
                );
                ui.add_space(tk.th.shape.spacing);
                ui.horizontal(|ui| {
                    let named = tk.f("window-hs-forget-named", &[("name", (&h.name).into())]);
                    if w::button_named(
                        ui,
                        tk,
                        &tk.t("window-hs-forget"),
                        &named,
                        Kind::Danger,
                        true,
                    )
                    .clicked()
                    {
                        v.out.push(Action::Send(Request::ForgetHeadset {
                            headset: h.key.clone(),
                        }));
                        v.ui.forget_ask = None;
                    }
                    if w::button(ui, tk, &tk.t("window-hs-cancel"), Kind::Quiet, true).clicked() {
                        v.ui.forget_ask = None;
                    }
                });
            } else {
                let full = ui.available_width();
                ui.allocate_ui_with_layout(
                    vec2(full, 0.0),
                    Layout::right_to_left(Align::Center),
                    |ui| {
                        let named = tk.f("window-hs-forget-named", &[("name", (&h.name).into())]);
                        if w::button_named(
                            ui,
                            tk,
                            &tk.t("window-hs-forget"),
                            &named,
                            Kind::Danger,
                            true,
                        )
                        .clicked()
                        {
                            v.ui.forget_ask = Some(h.key.clone());
                        }
                        ui.add_space(tk.th.shape.padding);
                        ui.with_layout(Layout::top_down(Align::Min), |ui| {
                            let name = if h.role == Role::Guest {
                                tk.f("window-hs-guest-name", &[("name", (&h.name).into())])
                            } else {
                                h.name.clone()
                            };
                            text(ui, name, tk.body_m(), p.text);
                            ui.add_space(2.0);
                            if h.session.is_some() {
                                ui.horizontal(|ui| {
                                    let (r, _) =
                                        ui.allocate_exact_size(vec2(10.0, 16.0), Sense::hover());
                                    w::dot(ui, tk, r.center(), p.ok);
                                    text(ui, tk.t("window-hs-now"), tk.small(), p.text_muted);
                                });
                            } else {
                                let when = match h.last_seen {
                                    Some(t) => tk.f(
                                        "window-hs-last",
                                        &[("when", ago(tk, v.now_unix, t).into())],
                                    ),
                                    None => tk.t("window-hs-never"),
                                };
                                text(ui, when, tk.small(), p.text_muted);
                            }
                            // Two apps on one headset (a release and a test build) are
                            // two headsets of one name: the app and the key tell them
                            // apart.
                            let key = ("key", Arg::from(&h.key));
                            let which = if h.app.is_empty() {
                                tk.f("window-hs-key", &[key])
                            } else {
                                tk.f("window-hs-app", &[("app", (&h.app).into()), key])
                            };
                            text(ui, which, tk.small(), p.text_muted);
                        });
                    },
                );
                input_right(ui, v, h);
            }
            if i + 1 < n {
                w::divider(ui, tk);
            }
        }
    });
}
