//! `prostir ctl`: the local API from a terminal, a script or a hot key, and `prostir
//! open` (the menu entry). Every command is one request to the running Prostir (see
//! `crate::api`) through a [`CtlClient`]: D-Bus on Linux, a named pipe on Windows
//! later.
//!
//! Without `--json` the result is one line for people; with it, one JSON object (one
//! a line for `watch`). Exit codes: [`DONE`], [`FAILED`], [`NOT_RUNNING`],
//! [`TIMED_OUT`], [`USAGE`].

use std::fmt::Write as _;
use std::time::Duration;

use crate::api::{
    self, Answer, ApiError, CALL_SECONDS, CodecChoice, DisplayState, Failure, Request, State,
};
use crate::i18n::t;

/// Done.
pub const DONE: i32 = 0;
/// It did not happen (the error's name is in the text or the JSON).
pub const FAILED: i32 = 1;
/// Prostir is not running, or no headset is connected for a command for the headset.
pub const NOT_RUNNING: i32 = 2;
/// No answer in time.
pub const TIMED_OUT: i32 = 3;
/// Arguments that do not read (as `EX_USAGE`).
pub const USAGE: i32 = 64;

/// How long `ctl` waits for a reply: a command for the headset takes up to 30 s.
pub const WAIT: Duration = Duration::from_secs(CALL_SECONDS);

/// What a client sees while it watches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Watched {
    State(u32, String),
    Event(api::Event),
    /// Prostir came on the bus.
    Appeared,
    /// Prostir left the bus.
    Vanished,
}

/// Why a client got no answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientError {
    /// No bus (or pipe) to talk on.
    NoBus(String),
    /// Prostir is not running.
    NotRunning,
    /// Prostir said no.
    Refused(Failure),
    /// No answer in time.
    TimedOut,
    /// An answer that does not read, or the connection broke.
    Broken(String),
}

/// The way to the running Prostir.
pub trait CtlClient {
    fn call(&self, request: &Request) -> Result<Answer, ClientError>;
    /// The whole state: its revision and its JSON.
    fn state(&self) -> Result<(u32, String), ClientError>;
    /// Whether Prostir is there now.
    fn running(&self) -> bool;
    /// Calls `each` with what happens until it returns `false`.
    fn watch(&self, each: &mut dyn FnMut(Watched) -> bool) -> Result<(), ClientError>;
    /// While Prostir is not running: whether it sleeps until a headset comes (its
    /// sockets' watcher, or systemd's socket, waits for one). Looked at without
    /// waking it.
    fn asleep(&self) -> bool {
        false
    }
    /// While Prostir is neither running nor asleep: whether its port is taken by
    /// another program (another user's Prostir, say), which keeps it from starting.
    fn port_taken(&self) -> bool {
        false
    }
    /// While Prostir is not running: quits what sleeps for it (its sockets' watcher,
    /// or systemd's socket until the next login), as `quit` does while it runs;
    /// whether anything did. A Prostir that runs on another session bus (another
    /// session's) is not asleep: nothing of it quits from here.
    fn quit_asleep(&self) -> bool {
        false
    }
}

/// What `ctl` was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ctl {
    Ask(Request),
    Status,
    Watch,
    Monitors,
    Headsets,
    AutostartStatus,
    CountStatus,
    Help,
}

/// The arguments after `ctl`, and whether `--json` was among them.
pub fn parse(args: &[String]) -> Result<(Ctl, bool), String> {
    let json = args.iter().any(|a| a == "--json");
    let cpu = args.iter().any(|a| a == "--cpu");
    let all = args.iter().any(|a| a == "--all");
    let words: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|a| !matches!(*a, "--json" | "--cpu" | "--all"))
        .collect();
    let unexpected = |w: &str| format!("unexpected {w:?}");
    let on = |w: Option<&&str>| match w.copied() {
        Some("on") => Ok(true),
        Some("off") => Ok(false),
        Some(other) => Err(format!("expected on or off, not {other:?}")),
        None => Err("expected on or off".to_owned()),
    };
    let number = |w: Option<&&str>, what: &str| -> Result<u32, String> {
        let w = w.ok_or_else(|| format!("expected {what}"))?;
        w.parse()
            .map_err(|_| format!("{what}: not a number: {w:?}"))
    };
    let ask = |r: Request| Ok(Ctl::Ask(r));
    let (first, rest) = match words.split_first() {
        Some((first, rest)) => (*first, rest),
        None => return Ok((Ctl::Help, json)),
    };
    // Each takes as many words as it needs; more is a mistake.
    let mut takes = 0;
    let result: Result<Ctl, String> = match first {
        "help" | "--help" | "-h" => {
            takes = rest.len();
            Ok(Ctl::Help)
        }
        "status" => Ok(Ctl::Status),
        "watch" => Ok(Ctl::Watch),
        "recenter" => ask(Request::Recenter),
        "lineup" => ask(Request::LineUp),
        "showmouse" => ask(Request::ShowMouse),
        "reset" => ask(Request::ResetArrangement),
        "profile" => {
            takes = rest.len();
            match rest.split_first() {
                Some((&"rename", [profile, name @ ..])) => ask(Request::RenameProfile {
                    profile: (*profile).to_owned(),
                    name: name.join(" "),
                }),
                Some((&"rename", [])) => {
                    Err("expected the profile's number or name, then its new name".to_owned())
                }
                Some(_) => ask(Request::Profile {
                    name: rest.join(" "),
                }),
                None => Err(
                    "expected the profile's name, or its number as the headset menu lists them"
                        .to_owned(),
                ),
            }
        }
        "cinema" | "focus" | "follow" | "mic" | "nolimits" => {
            takes = 1;
            let on = on(rest.first())?;
            ask(match first {
                "cinema" => Request::Cinema { on },
                "focus" => Request::Focus { on },
                "follow" => Request::Follow { on },
                "mic" => Request::SetMic { on },
                _ => Request::NoLimits { on },
            })
        }
        "preset" => {
            takes = 1;
            let n = number(rest.first(), "a preset")?;
            let preset = u8::try_from(n).map_err(|_| format!("preset: 0 to 255, not {n}"))?;
            ask(Request::Preset { preset })
        }
        "monitor" => match rest.first().copied() {
            Some("list") => {
                takes = 1;
                Ok(Ctl::Monitors)
            }
            Some("add") => {
                takes = 2;
                let (w, h, scale) = size(rest.get(1).copied().ok_or("expected WxH")?)?;
                ask(Request::AddMonitor { w, h, scale })
            }
            Some(word @ ("remove" | "retry" | "primary")) => {
                takes = 2;
                let id = number(rest.get(1), "a display's id")?;
                ask(match word {
                    "remove" => Request::RemoveMonitor { id },
                    "retry" => Request::RetryMonitor { id },
                    _ => Request::SetPrimary { id },
                })
            }
            _ => Err("expected monitor list, add WxH, remove ID, retry ID or primary ID".into()),
        },
        "screen" => {
            takes = 2;
            let shown = match rest.first().copied() {
                Some("show") => true,
                Some("hide") => false,
                _ => return Err("expected screen show ID or screen hide ID".into()),
            };
            let id = number(rest.get(1), "a display's id")?;
            ask(Request::ShowPhysical { id, shown })
        }
        "screenshot" => {
            takes = 1;
            let id = number(rest.first(), "a display's id")?;
            ask(Request::Screenshot { id })
        }
        "quality" => {
            takes = 1;
            let mbit = number(rest.first(), "Mbit/s")?;
            ask(Request::SetQuality { mbit })
        }
        "sharp" => {
            takes = 1;
            ask(Request::SetSharp {
                on: on(rest.first())?,
            })
        }
        "codec" => {
            takes = 1;
            let codec = match rest.first().copied() {
                Some("h264") => CodecChoice::H264,
                Some("hevc" | "h265") => CodecChoice::H265,
                Some("auto") => CodecChoice::Auto,
                _ => return Err("expected codec h264, hevc or auto".into()),
            };
            ask(Request::SetCodec { codec, cpu })
        }
        "sound" => {
            takes = 1;
            match rest.first().copied() {
                Some("headset") => ask(Request::SoundToHeadset { on: true }),
                Some("pc") => ask(Request::SoundToHeadset { on: false }),
                _ => Err("expected sound headset or sound pc".into()),
            }
        }
        "pair" => match rest.first().copied() {
            None => ask(Request::PairNewHeadset),
            Some("stop") => {
                takes = 1;
                ask(Request::StopPairing)
            }
            Some(other) => Err(unexpected(other)),
        },
        "headsets" => Ok(Ctl::Headsets),
        "forget" if all => ask(Request::ForgetAll),
        "forget" => {
            takes = rest.len();
            let headset = rest.join(" ");
            if headset.is_empty() {
                Err("expected a headset (its key or name) or --all".to_owned())
            } else {
                ask(Request::ForgetHeadset { headset })
            }
        }
        "autostart" => {
            takes = 1;
            match rest.first().copied() {
                Some("status") => Ok(Ctl::AutostartStatus),
                w => ask(Request::Autostart {
                    on: on(w.as_ref())?,
                }),
            }
        }
        "count" => {
            takes = 1;
            let counting = |counting| ask(Request::SetCounting { counting });
            match rest.first().copied() {
                Some("status") => Ok(Ctl::CountStatus),
                Some("yes") => counting(api::Counting::Yes),
                Some("no") => counting(api::Counting::No),
                Some("unasked") => counting(api::Counting::Unasked),
                Some(other) => Err(format!(
                    "expected yes, no, unasked or status, not {other:?}"
                )),
                None => Err("expected yes, no, unasked or status".to_owned()),
            }
        }
        "settings" => ask(Request::ShowSettings),
        "quit" => ask(Request::Quit),
        other => Err(format!("no command {other:?}")),
    };
    let ctl = result?;
    if (cpu && first != "codec") || (all && first != "forget") {
        return Err("--cpu goes with codec, --all with forget".into());
    }
    if let Some(extra) = rest.get(takes) {
        return Err(unexpected(extra));
    }
    Ok((ctl, json))
}

/// `2560x1440`, `2560x1440@1.25` or `2560x1440@125%`: the size and the scale in
/// percent.
fn size(text: &str) -> Result<(u32, u32, u16), String> {
    let bad = || format!("expected WxH, WxH@1.25 or WxH@125%, not {text:?}");
    let (size, scale) = match text.split_once('@') {
        Some((size, scale)) => {
            let percent = match scale.strip_suffix('%') {
                Some(p) => p.parse::<f64>().map_err(|_| bad())?,
                None => scale.parse::<f64>().map_err(|_| bad())? * 100.0,
            };
            (size, percent.round())
        }
        None => (text, 100.0),
    };
    let (w, h) = size.split_once(['x', 'X']).ok_or_else(bad)?;
    let w = w.parse().map_err(|_| bad())?;
    let h = h.parse().map_err(|_| bad())?;
    if !(1.0..=1000.0).contains(&scale) {
        return Err(bad());
    }
    Ok((w, h, scale as u16))
}

const HELP: &str = "\
prostir ctl COMMAND [--json]

  status                         what Prostir is doing
  watch                          what happens, a line each, until Ctrl+C
  recenter | lineup | showmouse | reset
  profile N|NAME                 a profile of the headset: its number as its menu lists
                                 them, or its name
  profile rename N NEW NAME      gives profile N (its number, or a one-word name) the
                                 name NEW NAME; with none, its number again
  cinema | focus | follow | mic | nolimits  on|off
  preset N
  monitor list | add WxH[@1.25|@125%] | remove ID | retry ID | primary ID
  screen show ID | screen hide ID
  screenshot ID                  a picture of a monitor: where it was saved
  quality MBIT | sharp on|off | codec h264|hevc|auto [--cpu]
  sound headset|pc
  pair | pair stop
  headsets | forget HEADSET | forget --all
  autostart on|off|status
  count yes|no|unasked|status    count this PC among users (unasked: ask again)
  settings
  quit

Exit codes: 0 done, 1 failed, 2 Prostir not running or no headset connected,
3 no answer in time, 64 wrong arguments.
";

/// Where `ctl` writes: the lines for people and JSON to `out`, problems to `err`.
pub struct Output<'a> {
    pub out: &'a mut dyn std::io::Write,
    pub err: &'a mut dyn std::io::Write,
}

/// Runs `ctl` with `args` against `client`: the exit code.
pub fn run(
    args: &[String],
    client: Result<&dyn CtlClient, ClientError>,
    io: &mut Output<'_>,
) -> i32 {
    let (ctl, json) = match parse(args) {
        Ok(parsed) => parsed,
        Err(e) => {
            let _ = writeln!(io.err, "prostir ctl: {e}\n\n{HELP}");
            return USAGE;
        }
    };
    if ctl == Ctl::Help {
        let _ = write!(io.out, "{HELP}");
        return DONE;
    }
    let client = match client {
        Ok(c) => c,
        Err(e) => return failed(&e, json, io, false),
    };
    match ctl {
        Ctl::Help => DONE,
        Ctl::Ask(request) => {
            let for_headset = request.for_headset();
            match ask(client, &request) {
                Ok(answer) => {
                    if json {
                        let _ = writeln!(io.out, "{}", answer.to_json());
                    } else {
                        let _ = writeln!(io.out, "{}", done_line(&answer));
                    }
                    DONE
                }
                Err(e) => failed(&e, json, io, for_headset),
            }
        }
        Ctl::Status | Ctl::Monitors | Ctl::Headsets | Ctl::AutostartStatus | Ctl::CountStatus => {
            match client.state() {
                Ok((revision, text)) => show_state(&ctl, revision, &text, json, io),
                Err(e @ ClientError::NotRunning) if ctl == Ctl::Status => {
                    let asleep = client.asleep();
                    let taken = !asleep && client.port_taken();
                    if json {
                        let (_, name, text) = what_failed(&e, false);
                        let mut value = error_json(name, &text, Some(false));
                        if asleep {
                            value["asleep"] = true.into();
                        }
                        if taken {
                            value["port_taken"] = true.into();
                        }
                        let _ = writeln!(io.out, "{value}");
                    } else if asleep {
                        let _ = writeln!(io.out, "{}", t("pc-ctl-waiting-for-the-headset-asleep"));
                    } else if taken {
                        let _ = writeln!(
                            io.out,
                            "{} {} {}",
                            t("pc-ctl-not-running-port"),
                            prostir_proto::TCP_PORT,
                            t("pc-ctl-is-taken-by-another-program")
                        );
                    } else {
                        let _ = writeln!(io.out, "{}", t("pc-ctl-not-running"));
                    }
                    NOT_RUNNING
                }
                Err(e) => failed(&e, json, io, false),
            }
        }
        Ctl::Watch => watch(client, json, io),
    }
}

/// Asks the running Prostir for `request`. `quit` while Prostir sleeps quits what
/// sleeps for it, so nothing waits for the headset afterwards, as after `quit` while it
/// runs.
pub fn ask(client: &dyn CtlClient, request: &Request) -> Result<Answer, ClientError> {
    match client.call(request) {
        Err(ClientError::NotRunning) if *request == Request::Quit && client.quit_asleep() => {
            Ok(Answer::default())
        }
        asked => asked,
    }
}

/// The exit code for `e`, saying why.
fn failed(e: &ClientError, json: bool, io: &mut Output<'_>, for_headset: bool) -> i32 {
    let (code, name, text) = what_failed(e, for_headset);
    if json {
        let _ = writeln!(io.out, "{}", error_json(name, &text, None));
    }
    let _ = writeln!(io.err, "prostir ctl: {name}: {text}");
    code
}

/// The exit code, the error's name and its text for `e`.
pub fn what_failed(e: &ClientError, for_headset: bool) -> (i32, &'static str, String) {
    match e {
        ClientError::NoBus(why) => (
            NOT_RUNNING,
            "NoBus",
            format!("{}: {why}", t("pc-ctl-no-session-bus")),
        ),
        ClientError::NotRunning => (
            NOT_RUNNING,
            "NotRunning",
            t("pc-ctl-prostir-is-not-running").to_owned(),
        ),
        ClientError::TimedOut => (
            TIMED_OUT,
            "TimedOut",
            t("pc-ctl-no-answer-in-time").to_owned(),
        ),
        ClientError::Broken(why) => (FAILED, ApiError::Other.name(), why.clone()),
        ClientError::Refused(f) => {
            let code = match f.error {
                ApiError::TimedOut => TIMED_OUT,
                ApiError::NotConnected if for_headset => NOT_RUNNING,
                _ => FAILED,
            };
            (code, f.error.name(), f.text.clone())
        }
    }
}

/// `{"error": name, "text": text}`, with `running` where it says so.
fn error_json(name: &str, text: &str, running: Option<bool>) -> serde_json::Value {
    let mut value = serde_json::json!({ "error": name, "text": text });
    if let Some(running) = running {
        value["running"] = running.into();
    }
    value
}

/// The line for people when a request was done.
fn done_line(answer: &Answer) -> String {
    if let Some(path) = &answer.path {
        let whole = if answer.whole == Some(true) {
            t("pc-ctl-whole-desktop")
        } else {
            ""
        };
        return format!("{} {path}{whole}", t("pc-ctl-saved"));
    }
    match (answer.id, answer.seconds_left) {
        (Some(id), _) => format!("{} {id}", t("pc-ctl-added-id")),
        (_, Some(left)) => format!(
            "{} {left} {}",
            t("pc-ctl-pairing-is-open-for"),
            t("pc-ctl-s")
        ),
        _ => t("pc-ctl-done").to_owned(),
    }
}

fn show_state(ctl: &Ctl, revision: u32, text: &str, json: bool, io: &mut Output<'_>) -> i32 {
    let state = match State::from_json(text) {
        Ok(s) => s,
        Err(e) => {
            let _ = writeln!(io.err, "prostir ctl: {e}");
            return FAILED;
        }
    };
    let out = &mut io.out;
    let _ = match (ctl, json) {
        (Ctl::Status, true) => writeln!(out, "{text}"),
        (Ctl::Status, false) => {
            let _ = writeln!(out, "{}", status_line(&state));
            match &state.wifi {
                Some(w) if w.power_save == Some(true) => writeln!(out, "{}", wifi_line(w)),
                _ => Ok(()),
            }
        }
        (Ctl::Monitors, true) => writeln!(out, "{}", to_json(&state.displays)),
        (Ctl::Monitors, false) => {
            for d in &state.displays {
                let mut line = display_line(d, state.primary);
                if let Some(s) = state
                    .encoding
                    .outputs
                    .iter()
                    .find(|o| o.display == d.id)
                    .and_then(|o| o.stats)
                {
                    line.push('\t');
                    line.push_str(&stats_line(&s));
                }
                let _ = writeln!(out, "{line}");
            }
            Ok(())
        }
        (Ctl::Headsets, true) => writeln!(out, "{}", to_json(&state.headsets)),
        (Ctl::Headsets, false) => {
            for h in &state.headsets {
                let _ = writeln!(out, "{}", headset_line(h));
            }
            Ok(())
        }
        (Ctl::CountStatus, true) => writeln!(
            out,
            r#"{{"counting":{},"revision":{revision}}}"#,
            to_json(&state.counting)
        ),
        (Ctl::CountStatus, false) => writeln!(
            out,
            "{}",
            match state.counting {
                Some(api::Counting::Yes) => t("pc-ctl-yes"),
                Some(api::Counting::No) => t("pc-ctl-no"),
                Some(api::Counting::Unasked) => t("pc-ctl-not-asked-yet"),
                Some(api::Counting::Other) => t("pc-ctl-unknown"),
                None => t("pc-ctl-not-available"),
            }
        ),
        (_, true) => writeln!(
            out,
            r#"{{"autostart":{},"revision":{revision}}}"#,
            to_json(&state.autostart)
        ),
        (_, false) => writeln!(
            out,
            "{}",
            match state.autostart {
                Some(true) => t("pc-ctl-on"),
                Some(false) => t("pc-ctl-off"),
                None => t("pc-ctl-not-available"),
            }
        ),
    };
    DONE
}

fn to_json<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

/// One line on what Prostir is doing.
pub fn status_line(state: &State) -> String {
    let monitors = state
        .displays
        .iter()
        .filter(|d| d.kind == api::DisplayKind::Virtual)
        .count();
    match (state.connected(), state.pc.phase) {
        (Some((headset, session)), _) => {
            let via = match session.via {
                api::Via::Usb => "USB",
                _ => "Wi-Fi",
            };
            let worn = if session.worn {
                ""
            } else {
                t("pc-ctl-taken-off")
            };
            format!(
                "{} {} ({via}{worn}), {}: {monitors}",
                t("pc-ctl-connected"),
                headset.name,
                t("pc-ctl-monitors"),
            )
        }
        (None, api::Phase::Lingering) => format!(
            "{}: {monitors}",
            t("pc-ctl-waiting-for-the-headset-monitors")
        ),
        _ => t("pc-ctl-waiting-for-a-headset").to_owned(),
    }
}

/// The PC's Wi-Fi with power saving on, and what helps.
fn wifi_line(w: &api::Wifi) -> String {
    format!(
        "{} ({}): {}",
        t("pc-ctl-power-saving-is-on-for-this"),
        w.interface,
        t("pc-ctl-it-can-hold-the-picture-back")
    )
}

fn display_line(d: &api::Display, primary: Option<u32>) -> String {
    let kind = match d.kind {
        api::DisplayKind::Virtual => t("pc-ctl-monitor"),
        api::DisplayKind::Physical => t("pc-ctl-pc-display"),
        api::DisplayKind::Window => t("pc-ctl-window"),
        api::DisplayKind::Other => "",
    };
    let state = match d.state {
        DisplayState::Starting => t("pc-ctl-starting"),
        DisplayState::Running => t("pc-ctl-running"),
        DisplayState::Failed => t("pc-ctl-failed"),
        DisplayState::Absent => t("pc-ctl-not-connected"),
        DisplayState::Kept => t("pc-ctl-kept-for-the-headset"),
        DisplayState::Other => "",
    };
    let mut line = format!(
        "{}\t{}\t{kind}\t{}x{} {}%\t{state}",
        d.id, d.name, d.mode.w, d.mode.h, d.mode.scale
    );
    if d.kind == api::DisplayKind::Physical {
        line.push_str(if d.shown {
            t("pc-ctl-shown")
        } else {
            t("pc-ctl-hidden")
        });
    }
    if primary == Some(d.id) {
        line.push_str(t("pc-ctl-primary"));
    }
    if let Some(f) = &d.failure {
        let _ = write!(line, ": {}", f.text);
    }
    line
}

/// What a display's encoder made: `60.0 frames/s (0.5 repeated), 24.1 Mbit/s,
/// encoder 4.3 ms (max 9.1)`.
fn stats_line(s: &api::StreamStats) -> String {
    let secs = (f64::from(s.window_ms) / 1e3).max(1e-3);
    format!(
        "{:.1} {} ({:.1} {}), {:.1} {}, {} {:.1} {} ({} {:.1})",
        f64::from(s.frames) / secs,
        t("pc-ctl-frames-s"),
        f64::from(s.repeats) / secs,
        t("pc-ctl-repeated"),
        s.bytes as f64 * 8.0 / secs / 1e6,
        t("pc-ctl-mbit-s"),
        t("pc-ctl-encoder"),
        f64::from(s.encode_us_avg) / 1e3,
        t("pc-ctl-ms"),
        t("pc-ctl-max"),
        f64::from(s.encode_us_max) / 1e3,
    )
}

fn headset_line(h: &api::Headset) -> String {
    let role = match h.role {
        api::Role::Guest => t("pc-ctl-guest"),
        _ => t("pc-ctl-main"),
    };
    let connected = if h.session.is_some() {
        t("pc-ctl-connected-2")
    } else {
        ""
    };
    format!("{}\t{}\t{role}{connected}", h.key, h.name)
}

/// `watch`: a line for each event and each change of what `status` says, until
/// stopped; while Prostir is not running it says so and waits for it.
fn watch(client: &dyn CtlClient, json: bool, io: &mut Output<'_>) -> i32 {
    let mut said = String::new();
    let say = |io: &mut Output<'_>, line: String| {
        let _ = writeln!(io.out, "{line}");
        let _ = io.out.flush();
    };
    let waiting = t("pc-ctl-prostir-is-not-running-waiting");
    if !client.running() {
        let line = if json {
            r#"{"event":"vanished"}"#.to_owned()
        } else {
            waiting.to_owned()
        };
        say(io, line);
    } else if let Ok((revision, text)) = client.state() {
        let line = state_line(revision, &text, json, &mut said);
        if let Some(line) = line {
            say(io, line);
        }
    }
    let result = client.watch(&mut |seen| {
        let line = match seen {
            Watched::State(revision, text) => state_line(revision, &text, json, &mut said),
            Watched::Event(event) if json => Some(to_json(&event)),
            Watched::Event(event) => event_line(&event),
            Watched::Appeared if json => Some(r#"{"event":"appeared"}"#.to_owned()),
            Watched::Appeared => Some(t("pc-ctl-prostir-is-running").to_owned()),
            Watched::Vanished if json => Some(r#"{"event":"vanished"}"#.to_owned()),
            Watched::Vanished => {
                said.clear();
                Some(waiting.to_owned())
            }
        };
        if let Some(line) = line {
            say(io, line);
        }
        true
    });
    match result {
        Ok(()) => DONE,
        Err(e) => failed(&e, json, io, false),
    }
}

/// What a new state shows: with `--json` the whole of it, else the status line when
/// it is not the one said last.
fn state_line(revision: u32, text: &str, json: bool, said: &mut String) -> Option<String> {
    if json {
        return Some(format!(
            r#"{{"event":"state_changed","revision":{revision},"state":{text}}}"#
        ));
    }
    let line = status_line(&State::from_json(text).ok()?);
    if line == *said {
        return None;
    }
    said.clone_from(&line);
    Some(line)
}

fn event_line(event: &api::Event) -> Option<String> {
    Some(match event {
        api::Event::HeadsetConnected { headset, name, via } => format!(
            "{} {name} ({headset}, {})",
            t("pc-ctl-headset-connected"),
            via.name()
        ),
        api::Event::HeadsetDisconnected { headset, reason } => format!(
            "{} {headset} ({})",
            t("pc-ctl-headset-disconnected"),
            reason.name()
        ),
        api::Event::HeadsetWorn { headset, worn } => format!(
            "{} {headset}",
            if *worn {
                t("pc-ctl-headset-put-on")
            } else {
                t("pc-ctl-headset-taken-off")
            }
        ),
        api::Event::Other => return None,
    })
}

/// How Prostir was woken for the window (`prostir`).
pub enum Woken {
    /// Started anew (`prostir serve --open-settings`): it opens the window itself.
    Started(std::process::Child),
    /// What holds its sockets while it sleeps (the sockets' watcher, systemd) starts
    /// it; `opens_window`: it then opens the window itself, else it is asked to.
    Asleep { opens_window: bool },
}

/// How long `prostir` waits for a sleeping Prostir to come on the bus.
const WAKE_WAIT: Duration = Duration::from_secs(30);

/// `prostir` (the menu entry): the settings window, from Prostir. When Prostir
/// is not running, `start` starts it (`prostir serve --open-settings`, which opens the
/// window itself once it is on the bus), and this waits until it is on the bus, for
/// [`WAKE_WAIT`] at most ([`TIMED_OUT`] after that), [`NOT_RUNNING`] when it ends
/// first.
pub fn open(client: &dyn CtlClient, wake: &dyn Fn() -> std::io::Result<Woken>) -> i32 {
    if !client.running() {
        let woken = match wake() {
            Ok(woken) => woken,
            Err(e) => {
                eprintln!("prostir: {e}");
                return NOT_RUNNING;
            }
        };
        let until = std::time::Instant::now() + WAKE_WAIT;
        let (mut child, opens_window) = match woken {
            Woken::Started(child) => (Some(child), true),
            Woken::Asleep { opens_window } => (None, opens_window),
        };
        loop {
            if client.running() {
                if opens_window {
                    return DONE;
                }
                break;
            }
            let ended = child
                .as_mut()
                .and_then(|child| child.try_wait().ok().flatten())
                .map(|s| s.to_string());
            if ended.is_none() && std::time::Instant::now() >= until {
                eprintln!("prostir: {}", t("pc-ctl-prostir-did-not-come-on-the"));
                return TIMED_OUT;
            }
            if let Some(how) = ended {
                eprintln!(
                    "prostir: {} ({how})",
                    t("pc-ctl-prostir-ended-before-it-started")
                );
                return NOT_RUNNING;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    match client.call(&Request::ShowSettings) {
        Ok(_) => DONE,
        // Prostir runs, which is what matters without the window.
        Err(ClientError::Refused(f)) => {
            eprintln!("prostir: {f}");
            DONE
        }
        Err(ClientError::TimedOut) => TIMED_OUT,
        Err(_) => NOT_RUNNING,
    }
}

#[cfg(test)]
mod tests;
