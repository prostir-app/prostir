//! The first run: what to do, step by step, until a headset is paired.

use super::*;

const STEPS: usize = 4;

pub(super) fn first_run(ui: &mut Ui, v: &mut View<'_, '_>, s: &State) {
    let tk = v.tk;
    let p = tk.p;
    counting_question(ui, v, s);
    header(ui, v, Page::FirstRun);
    let step = v.ui.fr_step.min(STEPS - 1);
    text(
        ui,
        tk.f(
            "window-fr-step",
            &[("n", (step + 1).into()), ("total", STEPS.into())],
        ),
        tk.small(),
        p.text_muted,
    );
    ui.add_space(tk.th.shape.spacing);
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 4.0), Sense::hover());
    let gap = tk.th.shape.spacing * 0.75;
    let seg = (r.width() - gap * (STEPS - 1) as f32) / STEPS as f32;
    for i in 0..STEPS {
        let piece = Rect::from_min_size(
            pos2(r.left() + i as f32 * (seg + gap), r.top()),
            vec2(seg, 4.0),
        );
        let c = if i <= step { p.accent_ink } else { p.control };
        ui.painter().rect_filled(piece, CornerRadius::same(2), c);
    }
    ui.add_space(tk.th.shape.section_gap * 0.75);

    let title = |ui: &mut Ui, id: &str| {
        text(ui, tk.t(id), tk.font(tk.th.type_.size_title, true), p.text);
        ui.add_space(tk.th.shape.spacing);
    };
    let revision = match v.conn {
        Conn::Live { snap, .. } => snap.revision,
        _ => 0,
    };
    let paired = match &v.ui.pair.outcome {
        Some(Outcome::Paired(name)) => Some(name.clone()),
        _ => None,
    };
    let mut next_ok = true;
    w::card(ui, tk, |ui| {
        ui.add_space(tk.th.shape.spacing);
        match step {
            0 => {
                w::brand_mark(ui, tk, 72.0);
                ui.add_space(tk.th.shape.padding);
                title(ui, "window-fr-1-title");
                text(ui, tk.t("window-fr-1-body"), tk.body(), p.text_muted);
                ui.add_space(tk.th.shape.padding);
                if w::button(ui, tk, &tk.t("window-fr-1-link"), Kind::Secondary, true).clicked() {
                    v.out.push(Action::OpenUrl(RELEASES.into()));
                }
            }
            1 => {
                title(ui, "window-fr-2-title");
                text(ui, tk.t("window-fr-2-body"), tk.body(), p.text_muted);
                ui.add_space(tk.th.shape.padding);
                let found = s
                    .nearby
                    .first()
                    .or_else(|| s.pairing.as_ref().and_then(|p| p.code_for.as_ref()));
                match found {
                    Some(name) => {
                        w::status(
                            ui,
                            tk,
                            p.ok,
                            &tk.f("window-fr-2-found", &[("name", name.into())]),
                        );
                    }
                    None if v.ui.unsupported.contains("pair_new_headset") => {
                        w::status(ui, tk, p.text_muted, &tk.t("window-fr-2-asks"));
                    }
                    None if s.pairing.is_none() => {
                        w::status(ui, tk, p.text_muted, &tk.t("window-fr-2-closed"));
                        ui.add_space(tk.th.shape.spacing);
                        if w::button(ui, tk, &tk.t("window-pair-reopen"), Kind::Secondary, true)
                            .clicked()
                        {
                            v.send(Request::PairNewHeadset);
                        }
                    }
                    // Next stays open: a headset reached by its address or over USB
                    // is not "seen" first, it asks for the code straight away.
                    None => {
                        ui.horizontal(|ui| {
                            w::spinner(ui, tk, 22.0);
                            text(ui, tk.t("window-fr-2-looking"), tk.body(), p.text);
                        });
                    }
                }
                ui.add_space(tk.th.shape.padding);
                text(ui, tk.t("window-pair-firewall"), tk.small(), p.text_muted);
            }
            2 => {
                title(ui, "window-fr-3-title");
                text(ui, tk.t("window-fr-3-body"), tk.body(), p.text_muted);
                ui.add_space(tk.th.shape.padding);
                if let Some(name) = &paired {
                    w::status(
                        ui,
                        tk,
                        p.ok,
                        &tk.f("window-pair-done", &[("name", name.into())]),
                    );
                } else if let Some(pr) = &s.pairing {
                    pair_body(ui, v, pr, &s.pc.addresses, revision);
                    next_ok = false;
                } else if let Some(h) = s.headsets.last() {
                    w::status(
                        ui,
                        tk,
                        p.ok,
                        &tk.f("window-pair-done", &[("name", (&h.name).into())]),
                    );
                } else if v.ui.unsupported.contains("pair_new_headset") {
                    w::status(ui, tk, p.text_muted, &tk.t("window-fr-3-wait"));
                    next_ok = false;
                } else {
                    text(ui, tk.t("window-pair-expired"), tk.body(), p.text_muted);
                    ui.add_space(tk.th.shape.padding);
                    if w::button(ui, tk, &tk.t("window-pair-reopen"), Kind::Primary, true).clicked()
                    {
                        v.send(Request::PairNewHeadset);
                    }
                    next_ok = !s.headsets.is_empty();
                }
            }
            _ => {
                title(ui, "window-fr-4-title");
                text(ui, tk.t("window-fr-4-body"), tk.body(), p.text_muted);
                if let Some(on) = s.autostart {
                    ui.add_space(tk.th.shape.padding);
                    let label = tk.t("window-ov-autostart");
                    w::row(ui, tk, &label, None, |ui| {
                        if let Some(on) = w::switch(ui, tk, &label, on, true) {
                            v.out.push(Action::Send(Request::Autostart { on }));
                        }
                    });
                }
            }
        }
        ui.add_space(tk.th.shape.section_gap);
        let last = step + 1 == STEPS;
        let label = if last {
            tk.t("window-fr-done")
        } else {
            tk.t("window-fr-next")
        };
        let back = tk.t("window-fr-back");
        // Laid out left to right after a gap, so Tab meets Back before Next, as seen.
        let width = w::button_width(ui, tk, &label)
            + if step > 0 {
                w::button_width(ui, tk, &back) + ui.spacing().item_spacing.x
            } else {
                0.0
            };
        ui.horizontal(|ui| {
            ui.add_space((ui.available_width() - width).max(0.0));
            if step > 0 && w::button(ui, tk, &back, Kind::Quiet, true).clicked() {
                v.ui.fr_step = step - 1;
            }
            {
                if w::button(ui, tk, &label, Kind::Primary, next_ok).clicked() {
                    if last {
                        v.ui.fr_step = 0;
                        v.ui.pair.outcome = None;
                        v.out.push(Action::Go(Page::Overview));
                    } else {
                        // The headset can only find the PC while pairing is open.
                        if step == 0 && s.pairing.is_none() {
                            v.out.push(Action::Send(Request::PairNewHeadset));
                        }
                        v.ui.fr_step = step + 1;
                    }
                }
            }
        });
    });
}
