//! The listening socket, and the setup of each connection on a thread of its own: a
//! headset pairing (with a dialog that waits for the person at the PC) or trickling
//! its bytes holds only its own thread, never the PC. A connection set up and trusted
//! goes to the control loop as [`Event::Accepted`].

use std::net::{SocketAddr, TcpListener, TcpStream};

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use log::{debug, warn};

use crate::events::{Accepted, Event, EventSink};
use crate::pairing;
use crate::platform::sockets::readable;
use crate::platform::tray;

/// Connections set up at once, at most: more are closed at once, so a peer opening
/// connection after connection cannot make the PC start threads without end.
const MAX_SETUPS: usize = 16;

/// One connection being set up; its place is free again when it is dropped.
struct Setup(Arc<AtomicUsize>);

impl Setup {
    /// A place, if one is free.
    fn take(setups: &Arc<AtomicUsize>) -> Option<Self> {
        let place = Self(setups.clone());
        // Over the limit, the place is given back as it is dropped.
        (setups.fetch_add(1, Ordering::AcqRel) < MAX_SETUPS).then_some(place)
    }
}

impl Drop for Setup {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// The listening thread, while it runs: how many connections are being set up, and
/// how to stop it.
pub struct Listening {
    listener: TcpListener,
    setups: Arc<AtomicUsize>,
    security: Arc<pairing::Security>,
    tray: Option<tray::Handle>,
    events: EventSink,
    /// Written to stop the thread, which then ends.
    stop: Option<(std::fs::File, std::thread::JoinHandle<()>)>,
}

impl Listening {
    /// Whether a connection is being set up now (it comes as `Event::Accepted`, or
    /// fails).
    pub fn busy(&self) -> bool {
        self.setups.load(Ordering::Acquire) > 0
    }

    /// Stops accepting connections, once the thread is done with the one it may be
    /// accepting: the program is about to leave, and a connection that comes from now
    /// on waits in the socket for the next start.
    pub fn stop(&mut self) {
        if let Some((mut stop, thread)) = self.stop.take() {
            use std::io::Write;
            let _ = stop.write_all(b"x");
            let _ = thread.join();
        }
    }

    /// Accepts connections again (one came as the program was about to leave).
    pub fn resume(&mut self) -> Result<()> {
        if self.stop.is_none() {
            self.stop = Some(start(
                self.listener.try_clone()?,
                self.setups.clone(),
                self.security.clone(),
                self.tray.clone(),
                self.events.clone(),
            )?);
        }
        Ok(())
    }
}

/// Accepts connections on `listener` until stopped (see [`Listening`]).
pub fn spawn(
    listener: TcpListener,
    first: Option<TcpStream>,
    security: Arc<pairing::Security>,
    tray: Option<tray::Handle>,
    events: EventSink,
) -> Result<Listening> {
    let setups = Arc::new(AtomicUsize::new(0));
    // A connection the light phase accepted already: set up first.
    if let Some(stream) = first
        && let Some(setup) = Setup::take(&setups)
    {
        let from = stream.peer_addr()?;
        set_up(
            stream,
            from,
            setup,
            security.clone(),
            tray.clone(),
            events.clone(),
        );
    }
    let stop = start(
        listener.try_clone()?,
        setups.clone(),
        security.clone(),
        tray.clone(),
        events.clone(),
    )?;
    Ok(Listening {
        listener,
        setups,
        security,
        tray,
        events,
        stop: Some(stop),
    })
}

/// The listening thread, and the end of a pipe that stops it.
fn start(
    listener: TcpListener,
    setups: Arc<AtomicUsize>,
    security: Arc<pairing::Security>,
    tray: Option<tray::Handle>,
    events: EventSink,
) -> Result<(std::fs::File, std::thread::JoinHandle<()>)> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let mut fds = [0; 2];
    // SAFETY: pipe2 fills the two descriptors, owned right below.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: new descriptors nothing else owns.
    let (stopped, stop) = unsafe {
        (
            std::fs::File::from_raw_fd(fds[0]),
            std::fs::File::from_raw_fd(fds[1]),
        )
    };
    let thread = std::thread::Builder::new()
        .name("listen".into())
        .spawn(move || {
            // When the log last said connections were closed for want of a place.
            let mut said_full: Option<Instant> = None;
            loop {
                // Asleep in the kernel until a connection or the word to stop comes.
                let ready = match readable(&[listener.as_raw_fd(), stopped.as_raw_fd()], None) {
                    Ok(ready) => ready,
                    Err(e) => {
                        warn!("waiting for connections failed: {e}");
                        std::thread::sleep(Duration::from_millis(500));
                        continue;
                    }
                };
                if ready[1] {
                    return;
                }
                let (stream, from) = match listener.accept() {
                    Ok(accepted) => accepted,
                    // Pending network errors (ENETDOWN, EHOSTUNREACH…) or running out
                    // of file descriptors: not a reason to stop listening.
                    Err(e) => {
                        warn!("accepting a connection failed: {e}");
                        std::thread::sleep(Duration::from_millis(500));
                        continue;
                    }
                };
                let Some(setup) = Setup::take(&setups) else {
                    if said_full.is_none_or(|t| t.elapsed() >= Duration::from_secs(60)) {
                        said_full = Some(Instant::now());
                        warn!(
                            "{MAX_SETUPS} connections are being set up: closing the one from \
                             {from} (and others for a minute without saying so)"
                        );
                    }
                    continue;
                };
                set_up(
                    stream,
                    from,
                    setup,
                    security.clone(),
                    tray.clone(),
                    events.clone(),
                );
            }
        })?;
    Ok((stop, thread))
}

/// Sets the connection `stream` from `from` up on a thread of its own, which holds
/// `setup` until it is done.
fn set_up(
    stream: TcpStream,
    from: SocketAddr,
    setup: Setup,
    security: Arc<pairing::Security>,
    tray: Option<tray::Handle>,
    events: EventSink,
) {
    let spawned = std::thread::Builder::new()
        .name("setup".into())
        .spawn(move || {
            let _setup = setup;
            match pairing::accept(stream, &security, tray.as_ref()) {
                Ok((conn, open)) => {
                    let accepted = Accepted { conn, open, from };
                    events.send(Event::Accepted(Box::new(accepted)));
                }
                Err(e)
                    if e.is::<pairing::TurnedAwayAgain>()
                        || e.is::<pairing::HungUpBeforeHello>() =>
                {
                    debug!("rejected connection from {from}: {e:#}");
                }
                Err(e) => warn!("rejected connection from {from}: {e:#}"),
            }
        });
    if let Err(e) = spawned {
        warn!("connection from {from} dropped: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// At most `MAX_SETUPS` places, each free again once its connection is done.
    #[test]
    fn connections_set_up_at_once_are_few() {
        let setups = Arc::new(AtomicUsize::new(0));
        let taken: Vec<Setup> = (0..MAX_SETUPS)
            .map(|_| Setup::take(&setups).expect("a free place"))
            .collect();
        assert!(Setup::take(&setups).is_none(), "one too many");
        drop(taken);
        assert_eq!(setups.load(Ordering::Acquire), 0);
        assert!(Setup::take(&setups).is_some());
    }
}
