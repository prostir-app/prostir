//! Plays a recording of the headset's input (made with "Record the input, 30 s" in the
//! preview build's "Hands (trial)", taken off the headset with `adb pull`) with the
//! hands of this build, and prints what they asked of the room, line by line.
//! cargo run -p prostir-client --example replay -- input-2026-11-26-1530.rec

#![expect(
    clippy::disallowed_methods,
    reason = "a tool on the desktop: it reads the file it is given and takes its own start time"
)]

use std::time::Instant;

use prostir::interaction::Feel;
use prostir::interaction::record::{Recording, replay};

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: replay <file.rec>");
        std::process::exit(2);
    };
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("{path}: {e}");
            std::process::exit(1);
        }
    };
    let rec = match Recording::read(&bytes) {
        Ok(rec) => rec,
        Err(e) => {
            eprintln!("{path}: {e}");
            std::process::exit(1);
        }
    };
    let seconds = rec.frames.last().map_or(0.0, |(t, _)| t.as_secs_f32());
    println!(
        "{path}: version {}, {} frames, {seconds:.1} s, {:?}",
        rec.app,
        rec.frames.len(),
        rec.stage.modes
    );
    let mut changed = String::new();
    rec.feel.write_changed(&mut changed);
    if rec.feel != Feel::DEFAULT {
        print!("feel.txt:\n{changed}");
    }
    for line in replay(&rec, Instant::now()) {
        println!("{line}");
    }
}
