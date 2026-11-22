//! The command line: `prostir` (the window), `prostir serve` (the service) and
//! `prostir ctl`; `prostir window` is the window's own process, which `prostir` and
//! the service start.
//!
//! Up to 0.1 a bare `prostir` was the service, with its options (`prostir -m
//! 2560x1440`): those options without a command still mean `prostir serve` with them,
//! hidden from the help.

use std::ffi::OsString;

use clap::parser::ValueSource;
use clap::{ArgMatches, CommandFactory, FromArgMatches};

use super::*;

#[derive(Parser, Debug)]
#[command(
    about = "Virtual monitors for KDE Plasma and Hyprland, streamed to a Meta Quest 3",
    long_about = "Virtual monitors for KDE Plasma and Hyprland, streamed to a Meta Quest 3.\n\n\
                  Without a command: opens the settings window, starting Prostir in the \
                  background if it is not running.",
    args_conflicts_with_subcommands = true
)]
pub(super) struct Cli {
    /// Print the version (as the log says it: `Prostir 0.2.0`) and exit.
    #[arg(short = 'v', long = "version", global = true)]
    pub(crate) version: bool,
    /// `serve`'s options without a command, as 0.1 took them (hidden, see [`Cli::read`]).
    #[command(flatten)]
    pub(crate) serve: Args,
    /// Some of them were given: this is `prostir serve` with them.
    #[arg(skip)]
    pub(crate) old_options: bool,
    #[command(subcommand)]
    pub(crate) command: Option<Sub>,
}

impl Cli {
    /// This program's command line; a wrong one exits with clap's message.
    pub(super) fn read() -> Self {
        Self::read_from(std::env::args_os()).unwrap_or_else(|e| e.exit())
    }

    /// The command line `args` (the program's name first), with `serve`'s options
    /// without a command hidden from the help and noted in [`Cli::old_options`].
    pub(super) fn read_from<I, T>(args: I) -> Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        let matches = command_line().try_get_matches_from(args)?;
        let mut cli = Self::from_arg_matches(&matches)?;
        cli.old_options = serve_options_given(&matches);
        Ok(cli)
    }
}

/// The command line's parser, `serve`'s options at the top hidden.
pub(super) fn command_line() -> clap::Command {
    Cli::command().mut_args(|a| match a.get_id().as_str() {
        "version" | "help" => a,
        _ => a.hide(true),
    })
}

/// Whether one of `serve`'s options was given without a command.
fn serve_options_given(matches: &ArgMatches) -> bool {
    <Args as clap::Args>::augment_args(clap::Command::new("serve"))
        .get_arguments()
        .any(|a| matches.value_source(a.get_id().as_str()) == Some(ValueSource::CommandLine))
}

#[derive(Subcommand, Debug)]
pub(super) enum Sub {
    /// Run in the background: wait for the headset and serve it (autostart and
    /// systemd start it so).
    Serve(Args),
    /// The settings window itself, in a process of its own (`prostir` and the
    /// service start it).
    #[command(hide = true)]
    Window {
        /// A made-up PC with a headset, to try the window without Prostir.
        #[arg(long)]
        mock: bool,
    },
    /// Ask the running Prostir something (`prostir ctl help` lists the commands).
    #[command(disable_help_flag = true)]
    Ctl {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
        args: Vec<String>,
    },
    /// The same over the Model Context Protocol on stdin and stdout, for an agent
    /// (`claude mcp add prostir -- prostir mcp`).
    Mcp,
}

#[derive(clap::Args, Debug)]
pub(super) struct Args {
    /// Monitor in pixels, optionally with KWin scale: 2560x1440 or 3840x2160@1.5. Repeat for
    /// several monitors. Without it, the set from the last session (changed from the headset
    /// menu) is used, or a single 2560x1440.
    #[arg(short, long = "monitor", value_parser = parse_monitor)]
    pub(crate) monitors: Vec<MonitorSpec>,
    /// Leave KDE's primary monitor alone (by default the middle Quest monitor becomes primary).
    #[arg(long)]
    pub(crate) keep_primary: bool,
    /// Video codec. Without it: the one chosen in the headset menu, else HEVC (sharpest
    /// small text). H.264 needs far less memory on Intel (~9 MB against ~70 MB of driver
    /// memory per monitor) at a slightly softer picture.
    #[arg(long, value_enum)]
    pub(crate) codec: Option<CodecArg>,
    /// Bitrate per monitor, Mbit/s (default: the quality chosen in the tray, or 30).
    #[arg(long)]
    pub(crate) bitrate: Option<u32>,
    /// Encode on the CPU with x264 even when the graphics card can (implies H.264).
    /// Without it Prostir uses VA-API (Intel, AMD), then NVENC (NVIDIA), then x264.
    #[arg(long)]
    pub(crate) software: bool,
    #[arg(long, default_value_t = prostir_proto::TCP_PORT)]
    pub(crate) port: u16,
    /// The UDP port headsets ask for this PC on (0: the system picks one, for tests).
    #[arg(long, default_value_t = prostir_proto::DISCOVERY_PORT, hide = true)]
    pub(crate) discovery_port: u16,
    /// Keep the virtual monitors this many seconds after the headset disconnects.
    #[arg(long, default_value_t = 30)]
    pub(crate) linger: u64,
    /// Do not answer headsets that ask for this PC on the network (a headset then
    /// reaches it by its address or over USB).
    #[arg(long)]
    pub(crate) no_discovery: bool,
    /// Stay running while no headset needs Prostir, with its sockets opened here,
    /// instead of sleeping in a small watcher until a headset comes.
    #[arg(long)]
    pub(crate) stay: bool,
    /// Started by the sockets' watcher, with its sockets (it decided to start).
    #[arg(long, hide = true)]
    pub(crate) woken: bool,
    /// Open the pairing window once ready (the button of the notice of a new headset).
    #[arg(long, hide = true)]
    pub(crate) pair: bool,
    /// Do not run `adb reverse` for USB connections.
    #[arg(long)]
    pub(crate) no_adb: bool,
    /// A test mode: draw the time each frame was captured into every monitor, as a
    /// code (PROTOCOL.md, measuring the latency from the PC to the eye).
    #[arg(long, hide = true)]
    pub(crate) time_code: bool,
    /// Open the settings window once Prostir is ready (`prostir` starts it so).
    #[arg(long)]
    pub(crate) open_settings: bool,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub(super) enum CodecArg {
    H264,
    /// HEVC, as the headset's menu calls it; `h265` is the same.
    #[value(name = "hevc", alias = "h265")]
    H265,
}

fn parse_monitor(s: &str) -> Result<MonitorSpec, String> {
    s.parse()
}
