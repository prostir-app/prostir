//! Running other programs: to the end within a time limit ([`run`]), or for as long as
//! Prostir runs ([`spawn_tied`]). Shared by the Linux platform and, for a test's
//! stand-in `pactl`, the fake desktop.

use std::io::{ErrorKind, Read};
use std::process::{Child, Command, Output, Stdio};
use std::sync::OnceLock;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use log::warn;

/// Runs a program to the end like `Command::output` (no input, its output and errors
/// captured), but stops it after `timeout`: a tool that hangs must not hang Prostir.
pub fn run(cmd: &mut Command, timeout: Duration) -> std::io::Result<Output> {
    run_until(cmd, timeout, &|| false)
}

/// As [`run`], and stopped too once `stop` says so (looked at each time the program
/// is): an error of kind `Interrupted` then.
pub fn run_until(
    cmd: &mut Command,
    timeout: Duration,
    stop: &dyn Fn() -> bool,
) -> std::io::Result<Output> {
    let deadline = Instant::now() + timeout;
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    // Read while it runs: a program that writes a lot would block on a full pipe.
    let stdout = read_in_background(child.stdout.take());
    let stderr = read_in_background(child.stderr.take());
    let mut pause = Duration::from_millis(1);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let program = cmd.get_program().to_string_lossy();
            warn!("{program} did not finish within {timeout:?}: stopped it");
            return Err(std::io::Error::new(
                ErrorKind::TimedOut,
                format!("{program} did not finish within {timeout:?}"),
            ));
        }
        if stop() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                ErrorKind::Interrupted,
                format!("{} stopped", cmd.get_program().to_string_lossy()),
            ));
        }
        std::thread::sleep(pause);
        pause = (pause * 2).min(Duration::from_millis(20));
    };
    // Something it started may keep the pipes open: wait only until the deadline.
    let collect = |rx: Receiver<Vec<u8>>| {
        rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap_or_default()
    };
    Ok(Output {
        status,
        stdout: collect(stdout),
        stderr: collect(stderr),
    })
}

/// All that comes out of `pipe`, read on a thread of its own.
fn read_in_background(pipe: Option<impl Read + Send + 'static>) -> Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    if let Some(mut pipe) = pipe {
        let _ = std::thread::Builder::new()
            .name("child-output".into())
            .spawn(move || {
                let mut data = Vec::new();
                let _ = pipe.read_to_end(&mut data);
                let _ = tx.send(data);
            });
    }
    rx
}

/// Spawns a helper that dies with Prostir, even if Prostir crashes. The kernel sends
/// that signal (`PR_SET_PDEATHSIG`) when the thread that spawned the helper ends, not
/// the process: so every helper is spawned on one thread that lives as long as Prostir
/// does. Spawned on the thread of a call with a time limit (see `worker/timed.rs`),
/// which ends right after, the microphone's `pw-loopback` and each recording's `pacat`
/// were stopped as soon as they were made.
pub fn spawn_tied(mut cmd: Command) -> std::io::Result<Child> {
    use std::os::unix::process::CommandExt;
    // SAFETY: prctl only.
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    let Some(spawner) = spawner() else {
        return cmd.spawn();
    };
    let (reply, answer) = mpsc::sync_channel(1);
    if let Err(mpsc::SendError((mut cmd, _))) = spawner.send((cmd, reply)) {
        return cmd.spawn();
    }
    answer.recv().unwrap_or_else(|_| {
        Err(std::io::Error::other(
            "the thread that starts helpers ended",
        ))
    })
}

/// A helper to start, and where its process goes.
type SpawnJob = (Command, mpsc::SyncSender<std::io::Result<Child>>);

/// The thread that starts the helpers, made with the first one and never ended. None
/// when no thread could be made: each helper is then started on its caller's thread.
fn spawner() -> Option<&'static mpsc::Sender<SpawnJob>> {
    static SPAWNER: OnceLock<Option<mpsc::Sender<SpawnJob>>> = OnceLock::new();
    SPAWNER
        .get_or_init(|| {
            let (jobs, taken) = mpsc::channel::<SpawnJob>();
            std::thread::Builder::new()
                .name("helpers".into())
                .spawn(move || {
                    for (mut cmd, reply) in taken {
                        let _ = reply.send(cmd.spawn());
                    }
                })
                .inspect_err(|e| warn!("no thread to start helpers on: {e}"))
                .ok()
                .map(|_| jobs)
        })
        .as_ref()
}

/// Writes `script` to `path` as a program for a test to run. It is copied there by
/// `cp`, so this process never has the program open for writing: a program another
/// test starts at that moment would hold that open until its own exec, and running
/// the script then fails ("Text file busy").
#[cfg(all(
    test,
    target_os = "linux",
    feature = "desktop",
    not(feature = "fake-desktop")
))]
pub fn write_program(path: &std::path::Path, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    let draft = path.with_extension("draft");
    std::fs::write(&draft, script).unwrap();
    let copied = Command::new("cp").arg(&draft).arg(path).status().unwrap();
    assert!(copied.success(), "cp {}", path.display());
    std::fs::remove_file(&draft).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A helper started on a thread that ends right after (as the sound thread's calls
    /// with a time limit do) goes on running: only Prostir's end ends it.
    #[test]
    fn a_helper_outlives_the_thread_that_started_it() {
        let mut helper = std::thread::spawn(|| {
            let mut sleep = Command::new("sleep");
            sleep.arg("30");
            spawn_tied(sleep)
        })
        .join()
        .unwrap()
        .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let ended = helper.try_wait().unwrap();
        let _ = helper.kill();
        let _ = helper.wait();
        assert_eq!(
            ended, None,
            "the helper ended with the thread that started it"
        );
    }

    #[test]
    fn run_returns_the_output() {
        let out = run(
            Command::new("sh").args(["-c", "echo hi; echo oops >&2; exit 3"]),
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(out.stdout, b"hi\n");
        assert_eq!(out.stderr, b"oops\n");
        assert_eq!(out.status.code(), Some(3));
        // More than a pipe holds.
        let out = run(
            Command::new("sh").args(["-c", "head -c 1000000 /dev/zero"]),
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(out.stdout.len(), 1_000_000);
    }

    #[test]
    fn run_stops_a_program_that_hangs() {
        let started = Instant::now();
        let e = run(Command::new("sleep").arg("10"), Duration::from_millis(200)).unwrap_err();
        assert_eq!(e.kind(), ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(2));
        let missing = run(
            &mut Command::new("prostir-no-such-tool"),
            Duration::from_secs(1),
        );
        assert_eq!(missing.unwrap_err().kind(), ErrorKind::NotFound);
    }
}
