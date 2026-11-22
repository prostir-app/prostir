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
        text(ui, tk.t("pair-pc-code"), tk.body(), p.text_muted);
        ui.add_space(tk.th.shape.spacing);
        w::code_display(ui, tk, code);
    } else if let Some(name) = &pr.code_for {
        text(
            ui,
            tk.f("pair-type", &[("name", name.into())]),
            tk.body(),
            p.text_muted,
        );
        ui.add_space(tk.th.shape.spacing);
        let checking =
            v.ui.submitted
                .as_ref()
                .is_some_and(|(c, until)| *c == v.ui.code && until.is_none_or(|r| revision < r));
        let label = tk.t("pair-code");
        let enter = w::code_field(ui, tk, &label, &mut v.ui.code, !checking);
        let wrong = pr.last == Some(PairResult::Wrong)
            && v.ui
                .submitted
                .as_ref()
                .is_some_and(|(c, _)| *c == v.ui.code);
        if wrong && !checking {
            ui.add_space(tk.th.shape.spacing);
            text(ui, tk.t("pair-wrong"), tk.body(), p.danger);
        }
        ui.add_space(tk.th.shape.padding);
        ui.horizontal(|ui| {
            if checking {
                w::spinner(ui, tk, 22.0);
                text(ui, tk.t("pair-checking"), tk.body(), p.text);
            } else {
                let ready = v.ui.code.len() == 6 && !wrong;
                if (w::button(ui, tk, &tk.t("pair-go"), Kind::Primary, ready).clicked()
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
        text(ui, tk.t("pair-wait"), tk.body(), p.text_muted);
        ui.add_space(tk.th.shape.padding);
        ui.horizontal(|ui| {
            w::spinner(ui, tk, 22.0);
            text(ui, tk.t("pair-waiting"), tk.body(), p.text);
        });
        if !addresses.is_empty() {
            ui.add_space(tk.th.shape.spacing);
            let list = addresses.join(", ");
            text(
                ui,
                tk.f("pair-address", &[("address", list.into())]),
                tk.small(),
                p.text_muted,
            );
        }
        ui.add_space(tk.th.shape.spacing);
        text(ui, tk.t("pair-firewall"), tk.small(), p.text_muted);
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
            text(ui, tk.t("pair-title"), tk.heading(), p.text);
            if limited {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    text(
                        ui,
                        tk.f("pair-left", &[("time", countdown(left).into())]),
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
            if w::button(ui, tk, &tk.t("pair-stop"), Kind::Quiet, true).clicked() {
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
        w::status(ui, tk, tk.p.text_muted, &tk.t("hs-pair-hint"));
    } else if w::button(ui, tk, &tk.t("ov-pair-new"), Kind::Primary, true).clicked() {
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
    w::section_title(ui, tk, &tk.t("mon-kept"));
    w::card(ui, tk, |ui| {
        text(ui, tk.t("mon-kept-desc"), tk.small(), p.text_muted);
        for d in kept {
            w::divider(ui, tk);
            let desc = size_scaled(tk, &d.mode);
            w::row(ui, tk, &display_name(tk, d), Some(&desc), |ui| {
                let named = tk.f("mon-remove-named", &[("name", (&d.name).into())]);
                if w::button_named(ui, tk, &tk.t("mon-remove"), &named, Kind::Danger, true)
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
                w::status(ui, tk, p.ok, &tk.f("pair-done", &[("name", name.into())]));
            }
            Outcome::Expired => {
                text(ui, tk.t("pair-expired"), tk.body(), p.text_muted);
            }
        }
        ui.add_space(tk.th.shape.padding);
        ui.horizontal(|ui| {
            if *o == Outcome::Expired
                && w::button(ui, tk, &tk.t("pair-reopen"), Kind::Primary, true).clicked()
            {
                v.send(Request::PairNewHeadset);
            }
            if w::button(ui, tk, &tk.t("pair-close"), Kind::Quiet, true).clicked() {
                v.ui.pair.outcome = None;
            }
        });
    });
}

pub(super) fn headsets(ui: &mut Ui, v: &mut View<'_, '_>, s: &State) {
    let tk = v.tk;
    let p = tk.p;
    header(ui, v, Page::Headsets);
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
                text(ui, tk.t("hs-empty-title"), tk.heading(), p.text);
                ui.add_space(6.0);
                text(ui, tk.t("hs-empty-body"), tk.body(), p.text_muted);
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
                    tk.f("hs-forget-ask", &[("name", (&h.name).into())]),
                    tk.body_m(),
                    p.text,
                );
                ui.add_space(tk.th.shape.spacing);
                ui.horizontal(|ui| {
                    let named = tk.f("hs-forget-named", &[("name", (&h.name).into())]);
                    if w::button_named(ui, tk, &tk.t("hs-forget"), &named, Kind::Danger, true)
                        .clicked()
                    {
                        v.out.push(Action::Send(Request::ForgetHeadset {
                            headset: h.key.clone(),
                        }));
                        v.ui.forget_ask = None;
                    }
                    if w::button(ui, tk, &tk.t("hs-cancel"), Kind::Quiet, true).clicked() {
                        v.ui.forget_ask = None;
                    }
                });
            } else {
                let full = ui.available_width();
                ui.allocate_ui_with_layout(
                    vec2(full, 0.0),
                    Layout::right_to_left(Align::Center),
                    |ui| {
                        let named = tk.f("hs-forget-named", &[("name", (&h.name).into())]);
                        if w::button_named(ui, tk, &tk.t("hs-forget"), &named, Kind::Danger, true)
                            .clicked()
                        {
                            v.ui.forget_ask = Some(h.key.clone());
                        }
                        ui.add_space(tk.th.shape.padding);
                        ui.with_layout(Layout::top_down(Align::Min), |ui| {
                            let name = if h.role == Role::Guest {
                                tk.f("hs-guest-name", &[("name", (&h.name).into())])
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
                                    text(ui, tk.t("hs-now"), tk.small(), p.text_muted);
                                });
                            } else {
                                let when = match h.last_seen {
                                    Some(t) => {
                                        tk.f("hs-last", &[("when", ago(tk, v.now_unix, t).into())])
                                    }
                                    None => tk.t("hs-never"),
                                };
                                text(ui, when, tk.small(), p.text_muted);
                            }
                            // Two apps on one headset (a release and a test build) are
                            // two headsets of one name: the app and the key tell them
                            // apart.
                            let key = ("key", Arg::from(&h.key));
                            let which = if h.app.is_empty() {
                                tk.f("hs-key", &[key])
                            } else {
                                tk.f("hs-app", &[("app", (&h.app).into()), key])
                            };
                            text(ui, which, tk.small(), p.text_muted);
                        });
                    },
                );
            }
            if i + 1 < n {
                w::divider(ui, tk);
            }
        }
    });
}
