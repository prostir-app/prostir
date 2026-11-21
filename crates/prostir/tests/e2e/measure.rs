//! What a session costs the PC, measured by hand: the PC app with the fake desktop and
//! a fake headset, the process read from `/proc` over a window (CPU time per second,
//! wake-ups per second, the busiest threads), and the router's lines for the same
//! window (frames, Mbit/s and keyframes per monitor, slices of sound). Run optimised,
//! on a quiet PC:
//!
//! `cargo test --release -p prostir --no-default-features --features fake-desktop
//! --test e2e measure -- --ignored --nocapture`
//!
//! `PROSTIR_MEASURE_SECS` (default 20) sets the window; `PROSTIR_MEASURE_MONITORS`
//! (default `1280x720,1280x720`) the monitors. The numbers compare one build with
//! another on the same PC; the fake desktop's x264 is not a real desktop's encoder.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use prostir_testkit::View;

use crate::START;
use crate::pc::{Options, Pc, TestDir};

/// CPU time and wake-ups of one thread (or of the whole process).
#[derive(Debug, Clone, Copy, Default)]
struct Use {
    /// Clock ticks in user and system mode.
    ticks: u64,
    /// Voluntary context switches (the thread went to sleep and woke) and others.
    voluntary: u64,
    involuntary: u64,
}

/// Every thread of process `pid` by its id, with its name.
fn threads(pid: u32) -> BTreeMap<u32, (String, Use)> {
    let mut out = BTreeMap::new();
    let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return out;
    };
    for task in tasks.flatten() {
        let Ok(tid) = task.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let path = task.path();
        let Ok(stat) = std::fs::read_to_string(path.join("stat")) else {
            continue;
        };
        let Ok(status) = std::fs::read_to_string(path.join("status")) else {
            continue;
        };
        // `pid (name) state ...`: the name may hold spaces, the fields after it do not.
        let Some((head, rest)) = stat.rsplit_once(')') else {
            continue;
        };
        let name = head.split_once('(').map_or("", |(_, n)| n).to_owned();
        let fields: Vec<&str> = rest.split_whitespace().collect();
        // utime and stime are fields 14 and 15 of the line, 12 and 13 after the name.
        let ticks = fields
            .get(11..13)
            .map_or(0, |f| f.iter().filter_map(|v| v.parse::<u64>().ok()).sum());
        let count = |key: &str| {
            status
                .lines()
                .find_map(|l| l.strip_prefix(key))
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0)
        };
        out.insert(
            tid,
            (
                name,
                Use {
                    ticks,
                    voluntary: count("voluntary_ctxt_switches:"),
                    involuntary: count("nonvoluntary_ctxt_switches:"),
                },
            ),
        );
    }
    out
}

/// What the process did between two readings, over `secs`: the whole, and by thread
/// name (threads of one name added up), busiest first.
fn report(before: &BTreeMap<u32, (String, Use)>, after: &BTreeMap<u32, (String, Use)>, secs: f64) {
    // SAFETY: a plain query of a constant.
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as f64;
    let mut by_name: BTreeMap<String, (u64, u64, u64, u32)> = BTreeMap::new();
    for (tid, (name, now)) in after {
        let was = before.get(tid).map(|(_, u)| *u).unwrap_or_default();
        let e = by_name.entry(name.clone()).or_default();
        e.0 += now.ticks.saturating_sub(was.ticks);
        e.1 += now.voluntary.saturating_sub(was.voluntary);
        e.2 += now.involuntary.saturating_sub(was.involuntary);
        e.3 += 1;
    }
    let total_ticks: u64 = by_name.values().map(|v| v.0).sum();
    let total_wakes: u64 = by_name.values().map(|v| v.1 + v.2).sum();
    eprintln!(
        "  process: {:.1} ms CPU/s, {:.0} wake-ups/s, {} threads",
        total_ticks as f64 / hz * 1e3 / secs,
        total_wakes as f64 / secs,
        after.len()
    );
    let mut rows: Vec<_> = by_name.into_iter().collect();
    rows.sort_by_key(|r| std::cmp::Reverse((r.1.0, r.1.1)));
    for (name, (ticks, vol, invol, n)) in rows.iter().take(14) {
        if *ticks == 0 && vol + invol == 0 {
            continue;
        }
        eprintln!(
            "    {name:<16} x{n:<2} {:>7.1} ms CPU/s {:>7.1} wake-ups/s",
            *ticks as f64 / hz * 1e3 / secs,
            (vol + invol) as f64 / secs
        );
    }
}

/// One scenario: `monitors` with `fps` picture changes a second, measured over
/// `secs` once every monitor shows.
fn scenario(name: &str, monitors: &str, options: &Options, secs: u64) {
    let dir = TestDir::new(&format!("measure-{name}"));
    let mut text = String::from("row\n");
    for m in monitors.split(',') {
        text.push_str(m.trim());
        text.push('\n');
    }
    text.push_str("mirror off\n");
    dir.save_monitors(&text);
    let mut keys = dir.pair();
    let pc = Pc::start(&dir, options);
    let mut headset = crate::connect(&pc, &mut keys);
    headset
        .wait(START, "every monitor showing", View::all_monitors_show)
        .unwrap();
    // Settled: the encoders opened, the first keyframes went. The headset reads all
    // along, as a real one does (else the PC's writer would wait on the socket).
    let read = |headset: &mut prostir_testkit::FakeHeadset, secs: u64| {
        let until = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < until {
            let _ = headset.recv(Duration::from_millis(50));
        }
    };
    read(&mut headset, 3);
    let pid = pc.program().unwrap_or_else(|| pc.pid());
    let reports = pc.count_log("link: round trip");
    let before = threads(pid);
    let started = Instant::now();
    let bytes0 = headset.view().audio_bytes;
    read(&mut headset, secs);
    let after = threads(pid);
    let took = started.elapsed().as_secs_f64();
    let sound = headset.view().audio_bytes - bytes0;
    eprintln!(
        "\n{name}: {monitors}, {} picture changes/s, {took:.1} s",
        options.fps
    );
    report(&before, &after, took);
    eprintln!(
        "  sound to the headset: {:.0} kbit/s",
        sound as f64 * 8.0 / took / 1e3
    );
    // The router's lines of the windows that ended meanwhile (every 10 s).
    let lines: Vec<String> = pc
        .lines("")
        .into_iter()
        .filter(|l| {
            l.contains(": sent ") || l.contains("sound: sent") || l.contains("link: round trip")
        })
        .collect();
    let first = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.contains("link: round trip"))
        .nth(reports)
        .map_or(lines.len(), |(i, _)| i + 1);
    for l in lines.iter().skip(first) {
        let l = l.split_once("] ").map_or(l.as_str(), |(_, rest)| rest);
        eprintln!("  {l}");
    }
    for l in pc.lines("encoder avg") {
        let l = l.split_once("] ").map_or(l.as_str(), |(_, rest)| rest);
        eprintln!("  {l}");
    }
    drop(headset);
    pc.quit();
}

#[test]
#[ignore = "by hand: a measurement, run optimised on a quiet PC"]
fn measure() {
    let secs = std::env::var("PROSTIR_MEASURE_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let monitors =
        std::env::var("PROSTIR_MEASURE_MONITORS").unwrap_or_else(|_| "1280x720,1280x720".into());
    let base = Options {
        bitrate: 20,
        env: vec![("RUST_LOG".into(), "info,prostir::encoder=debug".into())],
        ..Options::default()
    };
    scenario("still", &monitors, &base, secs);
    let silent = Options {
        env: vec![
            ("RUST_LOG".into(), "info,prostir::encoder=debug".into()),
            ("PROSTIR_FAKE_SOUND".into(), "silence".into()),
        ],
        ..base
    };
    scenario("still-silence", &monitors, &silent, secs);
    let moving = Options { fps: 30, ..silent };
    scenario("moving", &monitors, &moving, secs);
}
