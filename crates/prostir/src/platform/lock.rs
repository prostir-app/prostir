//! The lock file of the one Prostir of a user: held (`flock`) for the life of the
//! process that took it, and saying who that is (`watcher 1234`), so `prostir`
//! can wake the sockets' watcher. The same on every Linux desktop and the fake one.
//!
//! A `flock` belongs to the open file, which a child started by another thread shares
//! from its `fork` until its `exec` closes it (the file is close-on-exec): closing our
//! descriptor alone would leave the lock held for that moment, which under load is
//! long. So the lock is let go with `LOCK_UN`, which lets go of it for every copy.

use std::io::{Seek, Write};
use std::os::fd::AsRawFd;
use std::path::Path;

use anyhow::{Context, Result, bail};

/// Another process of the user holds the lock: Prostir runs already. Says who.
#[derive(Debug)]
pub struct Held(pub String);

impl std::fmt::Display for Held {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "held by {}", self.0)
    }
}

impl std::error::Error for Held {}

/// Whether `e` says Prostir runs already (its lock is held by another process).
#[allow(
    clippy::redundant_closure_for_method_calls,
    reason = "the method of a trait object reads worse as a path"
)]
pub fn is_held(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.is::<Held>())
}

/// The lock taken: held until it is dropped.
#[derive(Debug)]
pub struct Lock(std::fs::File);

impl Drop for Lock {
    fn drop(&mut self) {
        unlock(&self.0);
    }
}

/// Lets go of the `flock` on `file`, whoever else shares the open file.
fn unlock(file: &std::fs::File) {
    // SAFETY: flock on a file descriptor the caller owns.
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
}

/// Takes the lock at `path` for `role`.
pub fn take(path: &Path, role: &str) -> Result<Lock> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    // SAFETY: flock on a file descriptor this function owns.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let held = std::fs::read_to_string(path).unwrap_or_default();
        bail!(Held(held.trim().to_owned()));
    }
    let mut lock = Lock(file);
    lock.0.set_len(0)?;
    lock.0.rewind()?;
    write!(lock.0, "{role} {}", std::process::id())?;
    Ok(lock)
}

/// The process id of whoever holds the lock at `path` as `role`, if anyone does.
pub fn holder(path: &Path, role: &str) -> Option<u32> {
    let file = std::fs::File::open(path).ok()?;
    // SAFETY: flock on a file descriptor this function owns; taken only to see
    // whether it can be, and let go at once (for a child that shares it too: else
    // it would keep another program from taking the lock).
    let free = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) } == 0;
    if free {
        unlock(&file);
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    let (who, pid) = text.trim().split_once(' ')?;
    (who == role).then(|| pid.parse().ok()).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One holder at a time, which says who it is; free again once dropped.
    #[test]
    fn one_holder_who_says_who_it_is() {
        let dir = std::env::temp_dir().join(format!("prostir-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("prostir.lock");
        assert_eq!(holder(&path, "watcher"), None);
        let held = take(&path, "watcher").unwrap();
        assert_eq!(holder(&path, "watcher"), Some(std::process::id()));
        assert_eq!(holder(&path, "serve"), None, "another role");
        let again = take(&path, "serve").unwrap_err();
        assert!(format!("{again:#}").contains("watcher"), "{again:#}");
        assert!(is_held(&again.context("Prostir is already running")));
        drop(held);
        // Free at once, also while a program another test starts shares the file.
        assert_eq!(holder(&path, "watcher"), None, "let go");
        let serve = take(&path, "serve").unwrap();
        assert_eq!(holder(&path, "serve"), Some(std::process::id()));
        drop(serve);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
