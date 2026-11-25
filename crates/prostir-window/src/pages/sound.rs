//! The sound page, and the picture quality of the streams.

use super::*;

pub(super) fn sound(ui: &mut Ui, v: &mut View<'_, '_>, s: &State) {
    let tk = v.tk;
    let p = tk.p;
    header(ui, v, Page::Sound);
    let connected = s.connected();
    w::card(ui, tk, |ui| {
        let opts = [tk.t("window-sound-in-headset"), tk.t("window-sound-on-pc")];
        let opts: Vec<&str> = opts.iter().map(String::as_str).collect();
        w::row(
            ui,
            tk,
            &tk.t("window-snd-where"),
            Some(&tk.t("window-snd-where-desc")),
            |ui| {
                if let Some(i) = w::segmented(ui, tk, &opts, (!s.sound.to_headset) as usize, true) {
                    v.out
                        .push(Action::Send(Request::SoundToHeadset { on: i == 0 }));
                }
            },
        );
        w::divider(ui, tk);
        let vol = s
            .sound
            .volume
            .filter(|_| !v.ui.unsupported.contains("volume"));
        let desc = if vol.is_some() {
            tk.t("window-snd-volume-desc")
        } else if connected.is_some() {
            tk.t("window-snd-volume-headset")
        } else {
            tk.t("window-snd-volume-off")
        };
        let label = tk.t("window-snd-volume");
        w::row(ui, tk, &label, Some(&desc), |ui| {
            let level =
                v.ui.volume_drag
                    .or_else(|| vol.map(|v| v.level))
                    .unwrap_or(0);
            ui.allocate_ui_with_layout(
                vec2(56.0, tk.th.shape.control_height),
                Layout::right_to_left(Align::Center),
                |ui| {
                    let c = if vol.is_some() { p.text } else { p.text_muted };
                    text(ui, percent(tk, level), tk.body_m(), c);
                },
            );
            if let Some((lv, done)) = w::slider(ui, tk, &label, level, 240.0, vol.is_some()) {
                if done {
                    v.ui.volume_drag = None;
                    v.out.push(Action::Send(Request::Volume { level: lv }));
                } else {
                    v.ui.volume_drag = Some(lv);
                }
            }
        });
    });
    ui.add_space(tk.th.shape.section_gap);
    w::card(ui, tk, |ui| {
        // The switch is the headset user's: shown here, changed in the headset.
        let on = connected.map_or(s.sound.mic_on, |(_, session)| session.mic_on);
        w::row(
            ui,
            tk,
            &tk.t("window-snd-mic"),
            Some(&tk.t("window-snd-mic-desc")),
            |ui| {
                text(
                    ui,
                    tk.t(if on { "window-on" } else { "window-off" }),
                    tk.body_m(),
                    p.text,
                );
            },
        );
        text(
            ui,
            tk.t("window-snd-mic-in-headset"),
            tk.small(),
            p.text_muted,
        );
        w::divider(ui, tk);
        match connected {
            None if on => w::status(ui, tk, p.text_muted, &tk.t("window-snd-mic-waiting")),
            Some((_, session)) if on && session.mic_allowed == Some(false) => {
                w::status(ui, tk, p.warning, &tk.t("window-snd-mic-not-allowed"));
            }
            _ => match (on, s.sound.mic) {
                (false, _) => w::status(ui, tk, p.text_muted, &tk.t("window-snd-mic-off")),
                (true, MicState::Failed) => {
                    let why = s.sound.mic_failure.as_ref().map_or("", |f| f.text.as_str());
                    w::status(
                        ui,
                        tk,
                        p.danger,
                        &tk.f("window-snd-mic-failed", &[("reason", why.into())]),
                    );
                }
                (true, MicState::Ready) => w::status(ui, tk, p.ok, &tk.t("window-snd-mic-ready")),
                (true, _) => w::status(ui, tk, p.text_muted, &tk.t("window-snd-mic-waiting")),
            },
        }
    });
}

/// Bitrates offered, Mbit/s per monitor.
const BITRATES: [u32; 5] = [10, 20, 30, 50, 80];

pub(super) fn quality(ui: &mut Ui, v: &mut View<'_, '_>, s: &State) {
    let tk = v.tk;
    let p = tk.p;
    header(ui, v, Page::Quality);
    let e = &s.encoding;
    let can = |v: &View<'_, '_>, kind: &str| !v.ui.unsupported.contains(kind);
    w::card(ui, tk, |ui| {
        let labels: Vec<String> = BITRATES
            .iter()
            .map(|b| tk.f("window-q-mbit", &[("n", (*b).into())]))
            .collect();
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        let sel = BITRATES
            .iter()
            .position(|b| *b == e.bitrate)
            .unwrap_or(usize::MAX);
        let desc = tk.f("window-q-bitrate-desc", &[("n", e.bitrate.into())]);
        let ok = can(v, "set_quality");
        w::row(ui, tk, &tk.t("window-q-bitrate"), Some(&desc), |ui| {
            if let Some(i) = w::segmented(ui, tk, &labels, sel, ok) {
                v.out
                    .push(Action::Send(Request::SetQuality { mbit: BITRATES[i] }));
            }
        });
        w::divider(ui, tk);
        let label = tk.t("window-q-sharp");
        let ok = can(v, "set_sharp");
        w::row(ui, tk, &label, Some(&tk.t("window-q-sharp-desc")), |ui| {
            if let Some(on) = w::switch(ui, tk, &label, e.sharp, ok) {
                v.out.push(Action::Send(Request::SetSharp { on }));
            }
        });
    });
    ui.add_space(tk.th.shape.section_gap);
    w::card(ui, tk, |ui| {
        let codecs = [CodecChoice::H264, CodecChoice::H265];
        let labels = ["H.264", "HEVC"];
        let sel = match e.codec {
            Codec::H264 => 0,
            Codec::H265 => 1,
            _ => usize::MAX,
        };
        let ok = can(v, "set_codec");
        w::row(
            ui,
            tk,
            &tk.t("window-q-codec"),
            Some(&tk.t("window-q-codec-desc")),
            |ui| {
                if let Some(i) = w::segmented(ui, tk, &labels, sel, ok) {
                    v.out.push(Action::Send(Request::SetCodec {
                        codec: codecs[i],
                        cpu: e.cpu,
                    }));
                }
            },
        );
        w::divider(ui, tk);
        let label = tk.t("window-q-cpu");
        w::row(ui, tk, &label, Some(&tk.t("window-q-cpu-desc")), |ui| {
            if let Some(cpu) = w::switch(ui, tk, &label, e.cpu, ok) {
                let codec = match e.codec {
                    Codec::H264 => CodecChoice::H264,
                    _ => CodecChoice::H265,
                };
                v.out.push(Action::Send(Request::SetCodec { codec, cpu }));
            }
        });
        w::divider(ui, tk);
        match e.using {
            Some(u) => {
                let codec = match u.codec {
                    Codec::H264 => "H.264",
                    Codec::H265 => "HEVC",
                    Codec::Av1 => "AV1",
                    Codec::Other => "?",
                };
                let id = if u.hardware {
                    "window-q-using-gpu"
                } else {
                    "window-q-using-cpu"
                };
                w::status(ui, tk, p.ok, &tk.f(id, &[("codec", codec.into())]));
            }
            None => w::status(ui, tk, p.text_muted, &tk.t("window-q-using-none")),
        }
    });
}
