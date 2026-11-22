//! Makes a headset and a PC trust each other, as after pairing, by their files: for the
//! headset's selftest against a real PC app (`scripts/selftest-headset.sh --pc`).
//!
//! Usage: prostir-trust HEADSET_DIR PC_DIR
//!
//! `HEADSET_DIR` gets the headset's `identity` and `paired_hosts` (as the headset app
//! keeps them), `PC_DIR` the PC app's `identity` and `paired_headsets` (as in its
//! settings directory, `prostir/` under `XDG_CONFIG_HOME`). Keys there are kept; missing
//! ones are made. Prints both fingerprints. The PC app's own settings of this user are
//! refused: a test's headset is never made trusted there.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use prostir_proto::secure::{self, Identity, TrustStore};

/// This user's own settings directory of the PC app (`prostir/` under
/// `XDG_CONFIG_HOME`, or `~/.config`).
fn own_settings() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(config.join("prostir"))
}

fn main() -> Result<()> {
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let [headset_dir, pc_dir] = args.as_slice() else {
        bail!("usage: prostir-trust HEADSET_DIR PC_DIR");
    };
    for dir in [headset_dir, pc_dir] {
        std::fs::create_dir_all(dir).with_context(|| format!("making {}", dir.display()))?;
    }
    if let Some(own) = own_settings()
        && std::fs::canonicalize(pc_dir).ok() == std::fs::canonicalize(&own).ok()
    {
        bail!(
            "{} is this user's own Prostir settings: give a test's directory",
            pc_dir.display()
        );
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
