//! The PC app under test: the `prostir` binary built with the fake desktop, started in
//! a directory of its own (settings, keys, runtime files), on a port the system picks,
//! with a session bus of its own for the local API (`dbus-daemon --session`, where
//! it is installed).

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use prostir_proto::secure::{Identity, TrustStore};
use prostir_testkit::Keys;

/// One of the places for tests that run at once: at most [`jobs`] tests run their PC
/// apps together, however many threads the test harness runs (one per CPU by
/// default). Each PC app starts GStreamer, probes its encoders and encodes, and
/// besides the tests' own sleeps the PC apps of one per CPU only add to what the
/// machine has to do while it is busy with something else too. A test takes its place
/// with its first directory and holds it until its thread ends.
struct Slot;

/// The places taken now, and the wait for one.
static SLOTS: (Mutex<usize>, std::sync::Condvar) = (Mutex::new(0), std::sync::Condvar::new());

thread_local! {
    static HELD: std::cell::RefCell<Option<Slot>> = const { std::cell::RefCell::new(None) };
}

/// How many tests run at once: `PROSTIR_E2E_JOBS`, else half the CPUs, four at
/// least (as many as a CI runner has).
fn jobs() -> usize {
    std::env::var("PROSTIR_E2E_JOBS")
        .ok()
        .and_then(|n| n.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or_else(|| {
            let cpus = std::thread::available_parallelism().map_or(1, usize::from);
            (cpus / 2).max(4)
        })
}

impl Slot {
    /// Waits for a place for this test, unless it has one.
    fn take() {
        if HELD.with_borrow(Option::is_some) {
            return;
        }
        let (taken, freed) = &SLOTS;
        let max = jobs();
        let mut n = taken
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *n >= max {
            n = freed
                .wait(n)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        *n += 1;
        HELD.set(Some(Slot));
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let (taken, freed) = &SLOTS;
        *taken
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) -= 1;
        freed.notify_one();
    }
}

/// A directory for one test under Cargo's temporary directory. Removed when the test
/// passes; kept (with the PC app's log) when it fails, to look at.
pub struct TestDir(PathBuf);

impl TestDir {
    pub fn new(name: &str) -> Self {
        Slot::take();
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("e2e-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    /// The PC app's settings directory (its `XDG_CONFIG_HOME`).
    fn config(&self) -> PathBuf {
        self.0.join("config")
    }

    /// The fake headset's keys.
    pub fn headset_keys(&self) -> Keys {
        self.keys_of("headset")
    }

    /// The keys of another fake headset, `name`.
    pub fn keys_of(&self, name: &str) -> Keys {
        Keys::load(&self.0.join(name)).unwrap()
    }

    /// Makes the PC and the headset trust each other, as after pairing.
    pub fn pair(&self) -> Keys {
        self.pair_headset("headset")
    }

    /// Makes the PC and another fake headset, `name`, trust each other.
    pub fn pair_headset(&self, name: &str) -> Keys {
        let pc = Identity::load_or_create(&self.config().join("prostir/identity")).unwrap();
        let mut keys = self.keys_of(name);
        keys.trusted.add(pc.public, "fake PC").unwrap();
        TrustStore::load(&self.config().join("prostir/paired_headsets"))
            .add(keys.identity.public, name)
            .unwrap();
        // And the find secret its first session would give it.
        keys.learn_find_secret(pc.public, self.find_secret())
            .unwrap();
        keys
    }

    /// The PC's find secret, made as the PC makes it when there is none yet.
    pub fn find_secret(&self) -> prostir_proto::discovery::FindSecret {
        let path = self.config().join("prostir/find_secret");
        if let Ok(text) = std::fs::read_to_string(&path) {
            let text = text.trim();
            let mut secret = [0u8; 32];
            for (i, b) in secret.iter_mut().enumerate() {
                *b = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).unwrap();
            }
            return secret;
        }
        let secret = prostir_proto::discovery::new_secret();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let hex = prostir_proto::secure::to_hex(&secret);
        prostir_proto::secure::write_atomic(&path, format!("{hex}\n").as_bytes()).unwrap();
        secret
    }

    /// The PC's key (made on first use), as headsets know it.
    pub fn pc_key(&self) -> [u8; 32] {
        Identity::load_or_create(&self.config().join("prostir/identity"))
            .unwrap()
            .public
    }

    /// Makes the PC forget every headset it paired with (the tray's "Forget paired
    /// headsets").
    pub fn forget_headsets(&self) {
        TrustStore::load(&self.config().join("prostir/paired_headsets"))
            .clear()
            .unwrap();
    }

    /// The monitor set the PC app starts with, as 0.1 saved it (`monitors.txt`, moved
    /// over on the first start).
    pub fn save_monitors(&self, text: &str) {
        let path = self.config().join("prostir/monitors.txt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// The set of screens the PC app starts with, as this version saves it.
    pub fn save_screens(&self, text: &str) {
        self.save("screens.txt", text);
    }

    /// The settings the PC app starts with, as this version saves them.
    pub fn save_settings(&self, text: &str) {
        self.save("settings.txt", text);
    }

    fn save(&self, name: &str, text: &str) {
        let path = self.config().join("prostir").join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// A file the PC app keeps in its settings directory (`screens.txt`, say).
    pub fn saved(&self, name: &str) -> String {
        std::fs::read_to_string(self.config().join("prostir").join(name)).unwrap_or_default()
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("kept {} for a look", self.0.display());
        } else {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

/// How to start the PC app.
pub struct Options {
    /// Picture changes per second on the fake monitors (0: a still screen).
    pub fps: u32,
    /// Random pixels instead of grey bands: frames as big as the bitrate allows.
    pub noise: bool,
    /// `--bitrate`: Mbit/s per monitor.
    pub bitrate: u32,
    /// How long making each fake monitor takes.
    pub slow_monitors: Duration,
    /// How long connecting to the fake desktop takes.
    pub slow_connect: Duration,
    /// How long a step of the desktop's worker takes before the desktop counts as not
    /// responding, and before the PC app gives up and ends, in seconds (none: 15 s
    /// and 2 min, as in a release).
    pub desk_limits: Option<(u64, u64)>,
    /// How long a request waits for the desktop's answer, in seconds (none: 20 s).
    pub desk_answer: Option<u64>,
    /// `--linger`: seconds the monitors stay after the headset leaves.
    pub linger: u64,
    /// The pairing code the person at the PC types, if asked.
    pub typed_code: Option<String>,
    /// How long the person at the PC takes to type it.
    pub typed_code_delay: Duration,
    /// Monitors (by number, from 1) the fake desktop cannot make.
    pub broken_monitors: String,
    /// The fake PC's own displays (`PROSTIR_FAKE_PHYSICAL`: `eDP-1=EDID@1920x1080;…`).
    pub physical: String,
    /// `--software`: encode on the CPU whatever the settings say (the default, the same
    /// everywhere); without it, as `settings.txt` says.
    pub software: bool,
    /// How many headsets may have a session at once (`PROSTIR_TEST_MAX_HEADSETS`, a
    /// build for these tests only); 0: as a release allows.
    pub headsets: usize,
    /// A fake hardware encoder (`PROSTIR_FAKE_ENCODER`: `works`, `sessions=N`,
    /// `refuses=N`, `broken`, `hangs`, `slow`), tried before every other; none: the PC's own
    /// encoders only.
    pub encoder: Option<&'static str>,
    /// More of the PC app's environment (`PROSTIR_FAKE_NO_SOUND`, `PATH`).
    pub env: Vec<(String, String)>,
    /// Asleep in its sockets' watcher until a headset comes, as `prostir serve`
    /// runs; without it, `--stay`: the full program all along.
    pub watcher: bool,
    /// Answers headsets that ask for it, on a UDP port the system picks (it sends
    /// nothing on its own); without it, `--no-discovery`.
    pub discovery: bool,
    /// Started as systemd starts it, by `systemd-socket-activate` holding its sockets.
    pub activate: Option<Activate>,
}

/// The PC app started by `systemd-socket-activate`, as `prostir.socket` starts it: it
/// holds the TCP and UDP sockets (on ports of their own on 127.0.0.1) and starts the
/// PC app with them when something comes. One `systemd-socket-activate` listens on one
/// kind of socket, so two run, one inside the other, passing the outer one's socket on.
/// The outer one listens on the socket that starts it; the inner one finds that socket
/// ready at once and starts the PC app. No `--now`: systemd 255 (Ubuntu 24.04, CI) has
/// none.
pub struct Activate {
    pub tcp: SocketAddr,
    pub udp: SocketAddr,
    /// A connection starts it (else a datagram).
    pub by_connection: bool,
}

impl Activate {
    /// Two free ports on 127.0.0.1, and `systemd-socket-activate` to hold them; `None`,
    /// saying so loudly, without it (it must be there on CI).
    pub fn new(by_connection: bool) -> Option<Self> {
        if let Err(e) = Command::new("systemd-socket-activate")
            .arg("--version")
            .stdout(Stdio::null())
            .status()
        {
            assert!(
                std::env::var_os("CI").is_none(),
                "no systemd-socket-activate ({e}), which CI must have"
            );
            eprintln!(
                "\n*** no systemd-socket-activate ({e}): the socket activation test is SKIPPED ***\n"
            );
            return None;
        }
        let tcp = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let udp = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        Some(Self {
            tcp: tcp.local_addr().unwrap(),
            udp: udp.local_addr().unwrap(),
            by_connection,
        })
    }

    /// `cmd` (the PC app with its arguments and environment) started by
    /// `systemd-socket-activate`, which passes on only the environment it is told.
    fn wrap(&self, cmd: &Command) -> Command {
        let mut env: BTreeMap<String, String> = std::env::vars().collect();
        for (k, v) in cmd.get_envs() {
            let k = k.to_string_lossy().into_owned();
            match v {
                Some(v) => env.insert(k, v.to_string_lossy().into_owned()),
                None => env.remove(&k),
            };
        }
        let (tcp, udp) = (self.tcp.to_string(), self.udp.to_string());
        let (first, then): (&[&str], &[&str]) = if self.by_connection {
            (&["-l", &tcp], &["-d", "-l", &udp])
        } else {
            (&["-d", "-l", &udp], &["-l", &tcp])
        };
        let mut outer = Command::new("systemd-socket-activate");
        outer
            .args(first)
            .args(["--", "systemd-socket-activate"])
            .args(then);
        for (k, v) in env {
            outer.arg(format!("--setenv={k}={v}"));
        }
        outer
            .arg("--")
            .arg(cmd.get_program())
            .args(cmd.get_args())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        outer
    }
}

impl Default for Options {
    fn default() -> Self {
        Self {
            fps: 0,
            noise: false,
            bitrate: 8,
            slow_monitors: Duration::ZERO,
            slow_connect: Duration::ZERO,
            desk_limits: None,
            desk_answer: None,
            linger: 30,
            typed_code: None,
            typed_code_delay: Duration::ZERO,
            broken_monitors: String::new(),
            physical: String::new(),
            software: true,
            headsets: 0,
            encoder: None,
            env: Vec::new(),
            watcher: false,
            discovery: false,
            activate: None,
        }
    }
}

/// A session bus of a test's own: a `dbus-daemon` stopped when dropped.
pub struct Bus {
    daemon: Child,
    pub address: String,
}

impl Bus {
    /// A new bus with its socket in `dir`; `None` (saying so) without `dbus-daemon`.
    pub fn start(dir: &Path) -> Option<Self> {
        let socket = dir.join("bus");
        let _ = std::fs::remove_file(&socket);
        let spawned = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address"])
            .arg(format!("--address=unix:path={}", socket.display()))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let mut daemon = match spawned {
            Ok(d) => d,
            // CI installs it: there the tests of the local API must run.
            Err(e) if std::env::var_os("CI").is_some() => {
                panic!("no dbus-daemon ({e}), which CI installs for the local API's tests")
            }
            Err(e) => {
                eprintln!(
                    "\n*** no dbus-daemon ({e}): the tests of the local API are SKIPPED ***\n"
                );
                return None;
            }
        };
        let mut line = String::new();
        BufReader::new(daemon.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        Some(Self {
            daemon,
            address: line.trim().to_owned(),
        })
    }

    /// A connection to it, as a client of the PC app.
    pub fn connect(&self) -> zbus::blocking::Connection {
        zbus::blocking::connection::Builder::address(self.address.as_str())
            .unwrap()
            .method_timeout(Duration::from_secs(40))
            .build()
            .unwrap()
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

/// What `prostir ctl` did: its exit code and what it wrote.
#[derive(Debug)]
pub struct Ctl {
    pub code: i32,
    pub out: String,
    pub err: String,
}

impl Ctl {
    /// Its output as JSON (it was run with `--json`).
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(self.out.trim())
            .unwrap_or_else(|e| panic!("not JSON ({e}): {:?}", self.out))
    }
}

/// Runs `prostir ctl ARGS` on the session bus at `address`.
pub fn ctl_on(address: &str, args: &[&str]) -> Ctl {
    let out = Command::new(env!("CARGO_BIN_EXE_prostir"))
        .arg("ctl")
        .args(args)
        .env("DBUS_SESSION_BUS_ADDRESS", address)
        .env("LANGUAGE", "en")
        .stdin(Stdio::null())
        .output()
        .expect("run prostir ctl");
    Ctl {
        code: out.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&out.stdout).into_owned(),
        err: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// The port number after `after` in a log line.
pub fn port_in(line: &str, after: &str) -> u16 {
    let rest = line.split_once(after).expect("a port in the line").1;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits
        .parse()
        .unwrap_or_else(|_| panic!("no port in {line:?}"))
}

/// GStreamer's plugin registry, which the PC apps of every test share. Brought up to
/// date once, before the first PC app starts: without it (a new target directory, CI)
/// or after GStreamer or a plugin changed, every PC app would scan the plugins at the
/// same moment, which on a busy machine takes longer than a headset waits for its
/// session.
fn registry() -> PathBuf {
    static SCANNED: std::sync::Once = std::sync::Once::new();
    let registry = Path::new(env!("CARGO_TARGET_TMPDIR")).join("gst-registry.bin");
    SCANNED.call_once(|| scan_plugins(&registry));
    registry
}

/// Has `gst-inspect-1.0` bring `registry` up to date, with an empty home as every PC
/// app has (no plugins of the user's); without it, says so loudly (CI installs it).
fn scan_plugins(registry: &Path) {
    let home = Path::new(env!("CARGO_TARGET_TMPDIR")).join("e2e-gst-home");
    std::fs::create_dir_all(&home).unwrap();
    let scanned = Command::new("gst-inspect-1.0")
        .arg("--version")
        .env("GST_REGISTRY", registry)
        .env("HOME", &home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match scanned {
        Ok(status) if status.success() => {}
        Ok(status) => eprintln!("gst-inspect-1.0 --version: {status}"),
        Err(e) => {
            assert!(
                std::env::var_os("CI").is_none(),
                "no gst-inspect-1.0 ({e}), which CI must have"
            );
            eprintln!(
                "\n*** no gst-inspect-1.0 ({e}): the PC apps scan GStreamer's plugins themselves ***\n"
            );
        }
    }
}

/// A running PC app. Stopped (killed if it has to be) when dropped.
pub struct Pc {
    child: Child,
    pub addr: SocketAddr,
    /// Where it answers headsets that ask for it, with `Options::discovery`.
    pub discovery: Option<SocketAddr>,
    log: Arc<Mutex<Vec<String>>>,
    /// Its log was read to its end: the program and everything it started ended.
    read_all: Arc<std::sync::atomic::AtomicBool>,
    /// Its `XDG_RUNTIME_DIR`.
    run_dir: PathBuf,
    /// Its session bus, where `dbus-daemon` is installed.
    pub bus: Option<Bus>,
    /// Its environment, for the same program started again ([`Pc::again`]).
    env: Vec<(std::ffi::OsString, Option<std::ffi::OsString>)>,
}

impl Pc {
    /// The PC app started again while this one runs, with `args` and the same
    /// environment (as the menu, a terminal or an old autostart line starts it): what
    /// it did, once it ended (within 30 s, else it is killed and the test fails).
    pub fn again(&self, args: &[&str]) -> Ctl {
        self.again_with(args, &[])
    }

    /// The PC app to start again with `args` and the same environment, with `env` set
    /// besides; its stdout and stderr piped.
    pub fn again_command(&self, args: &[&str], env: &[(&str, &str)]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_prostir"));
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in &self.env {
            match v {
                Some(v) => cmd.env(k, v),
                None => cmd.env_remove(k),
            };
        }
        cmd.envs(env.iter().copied());
        cmd
    }

    /// [`Pc::again`] with `env` set besides (an autostart's variables, say).
    pub fn again_with(&self, args: &[&str], env: &[(&str, &str)]) -> Ctl {
        let mut child = self
            .again_command(args, env)
            .spawn()
            .expect("start the PC app again");
        let until = Instant::now() + Duration::from_secs(30);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() > until {
                let _ = child.kill();
                panic!("`prostir {}` did not end", args.join(" "));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let out = child.wait_with_output().unwrap();
        Ctl {
            code: out.status.code().unwrap_or(-1),
            out: String::from_utf8_lossy(&out.stdout).into_owned(),
            err: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    pub fn start(dir: &TestDir, options: &Options) -> Self {
        let root = dir.path();
        for sub in ["config", "run", "home", "cache"] {
            std::fs::create_dir_all(root.join(sub)).unwrap();
        }
        let bus = Bus::start(&root.join("run"));
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_prostir"));
        cmd.args(["serve", "--port", "0", "--no-adb"])
            .args(if options.discovery {
                ["--discovery-port", "0"].as_slice()
            } else {
                ["--no-discovery"].as_slice()
            })
            .args((!options.watcher).then_some("--stay"))
            .args(options.software.then_some("--software"))
            .args(["--bitrate", &options.bitrate.to_string()])
            .args(["--linger", &options.linger.to_string()])
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_RUNTIME_DIR", root.join("run"))
            // What the encoders' probes found, kept for the next start of this test.
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env("HOME", root.join("home"))
            .env("PROSTIR_FAKE_DESKTOP", "1")
            .env("PROSTIR_NO_COUNT", "1")
            .env("PROSTIR_FAKE_FPS", options.fps.to_string())
            .env(
                "PROSTIR_FAKE_SLOW_MONITORS_MS",
                options.slow_monitors.as_millis().to_string(),
            )
            .env(
                "PROSTIR_FAKE_SLOW_CONNECT_MS",
                options.slow_connect.as_millis().to_string(),
            )
            .env("PROSTIR_FAKE_BROKEN_MONITORS", &options.broken_monitors)
            .env("PROSTIR_FAKE_PHYSICAL", &options.physical)
            .env(
                "PROSTIR_FAKE_TYPED_CODE_DELAY_MS",
                options.typed_code_delay.as_millis().to_string(),
            )
            .env(
                "PROSTIR_FAKE_PICTURE",
                if options.noise { "noise" } else { "bands" },
            )
            .env("LANGUAGE", "en")
            .env("RUST_LOG", "info")
            // One plugin registry for all tests, so GStreamer scans its plugins once.
            .env("GST_REGISTRY", registry())
            // Nothing of the real desktop session, should anything look.
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DISPLAY")
            .env_remove("HYPRLAND_INSTANCE_SIGNATURE")
            // Not autostarted, even where the tests' terminal was.
            .env_remove("DESKTOP_AUTOSTART_ID")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        match &bus {
            Some(bus) => cmd.env("DBUS_SESSION_BUS_ADDRESS", &bus.address),
            None => cmd.env_remove("DBUS_SESSION_BUS_ADDRESS"),
        };
        match options.headsets {
            0 => cmd.env_remove("PROSTIR_TEST_MAX_HEADSETS"),
            n => cmd.env("PROSTIR_TEST_MAX_HEADSETS", n.to_string()),
        };
        match options.encoder {
            Some(mode) => cmd.env("PROSTIR_FAKE_ENCODER", mode),
            None => cmd.env_remove("PROSTIR_FAKE_ENCODER"),
        };
        match (options.desk_limits, options.desk_answer) {
            (None, None) => cmd.env_remove("PROSTIR_TEST_DESK_LIMITS"),
            (limits, answer) => {
                let (stuck, give_up) = limits.unwrap_or((15, 120));
                let answer = answer.unwrap_or(20);
                cmd.env(
                    "PROSTIR_TEST_DESK_LIMITS",
                    format!("{stuck},{give_up},{answer}"),
                )
            }
        };
        match &options.typed_code {
            Some(code) => cmd.env("PROSTIR_FAKE_TYPED_CODE", code),
            None => cmd.env_remove("PROSTIR_FAKE_TYPED_CODE"),
        };
        cmd.envs(options.env.iter().map(|(k, v)| (k, v)));
        let env = cmd
            .get_envs()
            .map(|(k, v)| (k.to_owned(), v.map(ToOwned::to_owned)))
            .collect();
        if let Some(activate) = &options.activate {
            cmd = activate.wrap(&cmd);
        }
        let mut child = cmd.spawn().expect("start the PC app");
        let log = Arc::new(Mutex::new(Vec::new()));
        let stderr = child.stderr.take().unwrap();
        let lines = log.clone();
        let read_all = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let all = read_all.clone();
        let file = root.join("pc.log");
        std::thread::spawn(move || {
            let mut copy = std::fs::File::create(file).ok();
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if let Some(f) = &mut copy {
                    use std::io::Write;
                    let _ = writeln!(f, "{line}");
                }
                lines.lock().unwrap().push(line);
            }
            all.store(true, std::sync::atomic::Ordering::Release);
        });
        let mut pc = Self {
            child,
            addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            discovery: None,
            log,
            read_all,
            run_dir: root.join("run"),
            bus,
            env,
        };
        if let Some(activate) = &options.activate {
            // Nothing of it runs until something comes: once the socket that starts
            // it is there.
            pc.addr = activate.tcp;
            pc.discovery = Some(activate.udp);
            let first = if activate.by_connection {
                activate.tcp
            } else {
                activate.udp
            };
            pc.wait_log(Duration::from_secs(10), &format!("Listening on {first} as"));
            return pc;
        }
        let line = pc.wait_log(Duration::from_secs(30), "waiting for the headset on port");
        pc.addr.set_port(port_in(&line, "on port "));
        if options.discovery {
            let line = pc.wait_log(Duration::from_secs(10), "discovery on UDP port ");
            let port = port_in(&line, "discovery on UDP port ");
            pc.discovery = Some(SocketAddr::from((Ipv4Addr::LOCALHOST, port)));
        }
        if pc.bus.is_some() && !options.watcher {
            pc.wait_log(Duration::from_secs(10), "local API on D-Bus as");
        }
        pc
    }

    /// The process id of its full program: itself, or under its sockets' watcher the
    /// watcher's child, while one runs.
    pub fn program(&self) -> Option<u32> {
        let watcher = self.child.id();
        let children =
            std::fs::read_to_string(format!("/proc/{watcher}/task/{watcher}/children")).ok()?;
        match children.split_whitespace().next() {
            Some(child) => child.parse().ok(),
            None => Some(watcher),
        }
    }

    /// Its own process id (the sockets' watcher's, when it has one).
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Stops its session bus and starts another at the same address, as a session
    /// bus restarted would be; `false` without one.
    pub fn restart_bus(&mut self) -> bool {
        if self.bus.take().is_none() {
            return false;
        }
        self.bus = Bus::start(&self.run_dir);
        self.bus.is_some()
    }

    /// Runs `prostir ctl ARGS` against this PC app; `None` without a session bus
    /// (no `dbus-daemon`: the test is skipped then, saying so).
    pub fn ctl(&self, args: &[&str]) -> Option<Ctl> {
        let Some(bus) = &self.bus else {
            eprintln!("no session bus: `prostir ctl {}` skipped", args.join(" "));
            return None;
        };
        Some(ctl_on(&bus.address, args))
    }

    /// The first log line containing `needle`, waiting up to `timeout` for it.
    pub fn wait_log(&mut self, timeout: Duration, needle: &str) -> String {
        self.wait_log_after(timeout, needle, 0)
    }

    /// The log line containing `needle` after the first `skip` such lines (taken with
    /// [`Pc::count_log`] before something happened, to wait for a line logged after
    /// it), waiting up to `timeout` for it.
    pub fn wait_log_after(&mut self, timeout: Duration, needle: &str, skip: usize) -> String {
        let until = Instant::now() + timeout;
        loop {
            let found = self
                .log
                .lock()
                .unwrap()
                .iter()
                .filter(|l| l.contains(needle))
                .nth(skip)
                .cloned();
            if let Some(line) = found {
                return line;
            }
            // Ended, once what it wrote last was read too.
            if let Ok(Some(status)) = self.child.try_wait()
                && self.read_all.load(std::sync::atomic::Ordering::Acquire)
                && self.count_log(needle) <= skip
            {
                panic!("the PC app ended ({status}) before logging {needle:?}");
            }
            assert!(
                Instant::now() < until,
                "the PC app did not log {needle:?} within {timeout:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Every log line containing `needle`, in order.
    pub fn lines(&self, needle: &str) -> Vec<String> {
        let log = self.log.lock().unwrap();
        log.iter().filter(|l| l.contains(needle)).cloned().collect()
    }

    /// How many log lines contain `needle`.
    pub fn count_log(&self, needle: &str) -> usize {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|l| l.contains(needle))
            .count()
    }

    /// How many times the picture source of each fake monitor started so far, by
    /// output name: each start sends the current picture.
    pub fn source_starts(&self) -> BTreeMap<String, usize> {
        let mut starts = BTreeMap::new();
        for line in self.log.lock().unwrap().iter() {
            if let Some(name) = line
                .split_once("fake screen ")
                .and_then(|(_, rest)| rest.strip_suffix(": started"))
            {
                *starts.entry(name.to_owned()).or_default() += 1;
            }
        }
        starts
    }

    /// Clicks `item` in the PC app's tray (see the fake tray for the items).
    pub fn tray(&self, item: &str) {
        self.order("fake-tray", item);
    }

    /// Writes `text` to the file `name` the fake desktop watches, whole at once: made
    /// under another name and moved there, so the PC app never reads it half written
    /// (it would take an empty file and remove it, and the command would be lost).
    fn order(&self, name: &str, text: &str) {
        let path = self.run_dir.join(name);
        let new = self.run_dir.join(format!("{name}.new"));
        std::fs::write(&new, format!("{text}\n")).unwrap();
        std::fs::rename(&new, &path).unwrap();
    }

    /// Presses the button of the notification the PC app shows (`click`), or closes it
    /// (`close`).
    pub fn notice(&self, what: &str) {
        self.order("fake-notice", what);
    }

    /// Has another program do `command` on the fake desktop (see the fake desktop
    /// for the commands, e.g. `resize Fake-2 320x240`).
    pub fn desktop(&self, command: &str) {
        self.order("fake-desktop", command);
    }

    /// How many times each thread of the PC app went to sleep and woke since it
    /// started (`voluntary_ctxt_switches`), by thread id, with the thread's name.
    pub fn wakeups(&self) -> BTreeMap<u32, (String, u64)> {
        wakeups_of(self.child.id())
    }
}

/// How many times each thread of process `pid` went to sleep and woke since it
/// started (`voluntary_ctxt_switches`), by thread id, with the thread's name.
pub fn wakeups_of(pid: u32) -> BTreeMap<u32, (String, u64)> {
    let mut threads = BTreeMap::new();
    let tasks = format!("/proc/{pid}/task");
    for task in std::fs::read_dir(tasks).into_iter().flatten().flatten() {
        let Ok(tid) = task.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let name = std::fs::read_to_string(task.path().join("comm")).unwrap_or_default();
        let status = std::fs::read_to_string(task.path().join("status")).unwrap_or_default();
        let woke = status
            .lines()
            .find_map(|l| l.strip_prefix("voluntary_ctxt_switches:"))
            .and_then(|n| n.trim().parse().ok());
        if let Some(woke) = woke {
            threads.insert(tid, (name.trim().to_owned(), woke));
        }
    }
    threads
}

impl Pc {
    /// The files the PC app has open now that are graphics devices (`/dev/dri/...`):
    /// one per card it encodes on, however many monitors it streams.
    pub fn gpu_files(&self) -> Vec<String> {
        let fds = format!("/proc/{}/fd", self.child.id());
        std::fs::read_dir(fds)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|fd| std::fs::read_link(fd.path()).ok())
            .map(|target| target.to_string_lossy().into_owned())
            .filter(|target| target.starts_with("/dev/dri/"))
            .collect()
    }

    /// Kills the PC app (`SIGKILL`), as a crash or a power cut would.
    pub fn kill(mut self) {
        self.signal(libc::SIGKILL);
        let _ = self.child.wait();
    }

    /// Stops the PC app dead (`SIGSTOP`), as a PC going to sleep does.
    pub fn freeze(&self) {
        self.signal(libc::SIGSTOP);
    }

    /// Lets a frozen PC app run on (`SIGCONT`), as after waking up.
    pub fn thaw(&self) {
        self.signal(libc::SIGCONT);
    }

    fn signal(&self, signal: libc::c_int) {
        // SAFETY: a signal to our own child process.
        unsafe { libc::kill(self.child.id() as libc::pid_t, signal) };
    }

    /// Waits up to `timeout` for the PC app to end by itself: how it ended. Its log
    /// is read to the end by then, as far as the time allows.
    pub fn wait_end(&mut self, timeout: Duration) -> std::process::ExitStatus {
        let until = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                while !self.read_all.load(std::sync::atomic::Ordering::Acquire)
                    && Instant::now() < until
                {
                    std::thread::sleep(Duration::from_millis(10));
                }
                return status;
            }
            assert!(
                Instant::now() < until,
                "the PC app did not end within {timeout:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Asks the PC app to quit as a user would (SIGTERM) and checks that it does.
    pub fn quit(mut self) {
        self.signal(libc::SIGTERM);
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "the PC app quit with {status}");
                return;
            }
            assert!(
                Instant::now() < until,
                "the PC app did not quit within 10 s"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Pc {
    fn drop(&mut self) {
        // A frozen one does not die of SIGKILL before it runs again.
        self.thaw();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
