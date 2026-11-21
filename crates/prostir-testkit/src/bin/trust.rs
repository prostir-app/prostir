//! Makes a headset and a PC trust each other, as after pairing, by their files: for the
//! headset's selftest against a real PC app (`scripts/selftest-headset.sh --pc`).
//!
//! Usage: prostir-trust HEADSET_DIR PC_DIR
//!
//! `HEADSET_DIR` gets the headset's `identity` and `paired_hosts` (as the headset app
//! keeps them), `PC_DIR` the PC app's `identity` and `paired_headsets` (as in its
//! settings directory, `prostir/` under `XDG_CONFIG_HOME`). Keys there are kept; missing
//! ones are made. Prints both fingerprints.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use prostir_proto::secure::{self, Identity, TrustStore};

fn main() -> Result<()> {
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let [headset_dir, pc_dir] = args.as_slice() else {
        bail!("usage: prostir-trust HEADSET_DIR PC_DIR");
    };
    for dir in [headset_dir, pc_dir] {
        std::fs::create_dir_all(dir).with_context(|| format!("making {}", dir.display()))?;
    }
    let headset =
        Identity::load_or_create(&headset_dir.join("identity")).context("the headset's key")?;
    let pc = Identity::load_or_create(&pc_dir.join("identity")).context("the PC's key")?;
    let mut trusted = TrustStore::load(&headset_dir.join("paired_hosts"));
    if !trusted.contains(&pc.public) {
        trusted.add(pc.public, "Prostir selftest PC")?;
    }
    let mut paired = TrustStore::load(&pc_dir.join("paired_headsets"));
    if !paired.contains(&headset.public) {
        paired.add(headset.public, "Prostir selftest")?;
    }
    println!("headset {}", secure::fingerprint(&headset.public));
    println!("pc {}", secure::fingerprint(&pc.public));
    Ok(())
}
