//! The RemoteDesktop portal (`org.freedesktop.portal.RemoteDesktop`, version 2 or
//! later): a session with the keyboard, the mouse and touch, then libei's socket from
//! `ConnectToEIS` (see `ei.rs`). KWin's and Mutter's portals show their own dialog
//! the first time; the answer is kept as a restore token, so later sessions start
//! without one. Without the token (it was not given, or no longer fits) the dialog
//! comes again, only when the person does something that needs the input (see
//! `crate::input`).
//!
//! The token lives in Prostir's settings folder, readable by the user alone
//! (`input-token`). A no, no answer in time, or the person stopping the control (on
//! the desktop's own indicator) forgets it: the next time asks again, never
//! silently. A session that did not start is closed, with its request, so no dialog
//! is left on the screen.

use std::collections::HashMap;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use log::{info, warn};
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{ObjectPath, OwnedFd, OwnedValue, Value};

use super::ei::Ei;
use crate::platform::{
    Injector, InputRefused, InputStopped, MouseButton, OpenInput, Spot, TouchPhase,
};

const DEST: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const IFACE: &str = "org.freedesktop.portal.RemoteDesktop";

/// Device types of `SelectDevices`.
const KEYBOARD: u32 = 1;
const POINTER: u32 = 2;
const TOUCHSCREEN: u32 = 4;
/// Kept until the person takes it back (`persist_mode`).
const PERSISTENT: u32 = 2;

/// How long libei may take to give its devices once the session started.
const DEVICES_WITHIN: Duration = Duration::from_secs(5);
/// How long the portal may take to answer a request that asks nobody.
const ANSWER_WITHIN: Duration = Duration::from_secs(30);
/// How long the desktop's dialog may wait for the person.
const PERSON_WITHIN: Duration = Duration::from_secs(300);

/// Whether the session bus has the portal with libei (`ConnectToEIS`, version 2).
pub fn available() -> bool {
    let version = (|| -> Result<u32> {
        let conn = Connection::session()?;
        let portal = Proxy::new(&conn, DEST, PATH, IFACE)?;
        Ok(portal.get_property::<u32>("version")?)
    })();
    match version {
        Ok(v) if v >= 2 => true,
        Ok(v) => {
            info!("input: the RemoteDesktop portal is version {v}, without libei");
            false
        }
        Err(e) => {
            info!("input: no RemoteDesktop portal: {e}");
            false
        }
    }
}

/// Opens the portal's input.
pub struct PortalOpen;

impl OpenInput for PortalOpen {
    fn open(&self) -> Result<Box<dyn Injector>> {
        let conn = Connection::session().context("the session bus")?;
        let portal = Proxy::new(&conn, DEST, PATH, IFACE)?;
        let version: u32 = portal.get_property("version")?;
        ensure!(
            version >= 2,
            "the RemoteDesktop portal is version {version}"
        );
        let types: u32 = portal
            .get_property("AvailableDeviceTypes")
            .unwrap_or(KEYBOARD | POINTER);
        let session = create_session(&conn, &portal)?;
        let opened = (|| -> Result<Ei> {
            start_session(&conn, &portal, &session, types)?;
            let path = ObjectPath::try_from(session.as_str())?;
            let fd: OwnedFd = portal
                .call("ConnectToEIS", &(&path, HashMap::<&str, Value<'_>>::new()))
                .context("ConnectToEIS")?;
            let socket = UnixStream::from(std::os::fd::OwnedFd::from(fd));
            Ei::connect(socket, DEVICES_WITHIN)
        })();
        match opened {
            Ok(ei) => {
                info!("input: through the RemoteDesktop portal and libei");
                Ok(Box::new(PortalInput { conn, session, ei }))
            }
            Err(e) => {
                // No session left behind, and no dialog of it on the screen.
                close(&conn, &session, "org.freedesktop.portal.Session");
                if said_no(&e) {
                    forget_token();
                }
                Err(e)
            }
        }
    }
}

/// No answer to a request in time (the request is closed).
#[derive(Debug)]
struct NoAnswer;

impl std::fmt::Display for NoAnswer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the portal did not answer in time")
    }
}

impl std::error::Error for NoAnswer {}

/// Whether `e` is the person's no, or their stop.
fn said_no(e: &anyhow::Error) -> bool {
    e.downcast_ref::<InputRefused>().is_some() || e.downcast_ref::<InputStopped>().is_some()
}

/// Closes the portal's object at `path` (a `Request` or a `Session`).
fn close(conn: &Connection, path: &str, iface: &str) {
    let _ = conn.call_method(Some(DEST), path, Some(iface), "Close", &());
}

/// A token for a portal request, new each time.
fn token() -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    format!("prostir{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

/// Calls `method` of the portal with `body` (whose options carry `handle_token`
/// `token`), and waits for the answer of its request: the code (0 done, 1 the person
/// said no, 2 other) and the results.
fn ask<B>(
    conn: &Connection,
    portal: &Proxy<'_>,
    method: &str,
    token: &str,
    body: &B,
    within: Duration,
) -> Result<(u32, HashMap<String, OwnedValue>)>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    let sender = conn
        .unique_name()
        .context("no name on the session bus")?
        .trim_start_matches(':')
        .replace('.', "_");
    let path = format!("{PATH}/request/{sender}/{token}");
    let request = Proxy::new(conn, DEST, path.as_str(), "org.freedesktop.portal.Request")?;
    let mut answers = request.receive_signal("Response")?;
    portal
        .call_method(method, body)
        .with_context(|| method.to_owned())?;
    // The iterator has no time limit: it waits on a thread of its own, left behind
    // (until the portal answers or goes) when the answer is too late.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("input-portal".into())
        .spawn(move || {
            let _ = tx.send(answers.next());
        })?;
    let Ok(answer) = rx.recv_timeout(within) else {
        // Its dialog, if any, goes with it.
        close(conn, &path, "org.freedesktop.portal.Request");
        return Err(anyhow::Error::new(NoAnswer).context(method.to_owned()));
    };
    let answer = answer.context("the portal went away")?;
    Ok(answer.body().deserialize()?)
}

/// Creates a session: its handle.
fn create_session(conn: &Connection, portal: &Proxy<'_>) -> Result<String> {
    let (t, s) = (token(), token());
    let options = HashMap::from([
        ("handle_token", Value::from(t.as_str())),
        ("session_handle_token", Value::from(s.as_str())),
    ]);
    let (code, results) = ask(
        conn,
        portal,
        "CreateSession",
        &t,
        &(options,),
        ANSWER_WITHIN,
    )?;
    ensure!(code == 0, "the portal did not create a session ({code})");
    let session = results
        .get("session_handle")
        .and_then(|v| String::try_from(v.clone()).ok())
        .context("the portal gave no session")?;
    Ok(session)
}

/// Picks the session's devices (with the token kept, if any) and starts it, which
/// shows the desktop's dialog when there is no token. No answer from the person in
/// time counts as a no: asked again only on their next action after Prostir starts
/// again.
fn start_session(conn: &Connection, portal: &Proxy<'_>, session: &str, types: u32) -> Result<()> {
    let path = ObjectPath::try_from(session)?;

    let t = token();
    let kept = load_token();
    let mut options = HashMap::from([
        ("handle_token", Value::from(t.as_str())),
        (
            "types",
            Value::from(types & (KEYBOARD | POINTER | TOUCHSCREEN)),
        ),
        ("persist_mode", Value::from(PERSISTENT)),
    ]);
    if let Some(kept) = &kept {
        options.insert("restore_token", Value::from(kept.as_str()));
    }
    let (code, _) = ask(
        conn,
        portal,
        "SelectDevices",
        &t,
        &(&path, options),
        ANSWER_WITHIN,
    )?;
    ensure!(code == 0, "the portal did not take the devices ({code})");

    let t = token();
    let options = HashMap::from([("handle_token", Value::from(t.as_str()))]);
    if kept.is_none() {
        info!("input: the desktop asks the person whether Prostir may control the input");
    }
    let (code, results) = match ask(
        conn,
        portal,
        "Start",
        &t,
        &(&path, "", options),
        PERSON_WITHIN,
    ) {
        Err(e) if e.downcast_ref::<NoAnswer>().is_some() => {
            return Err(
                anyhow::Error::new(InputRefused).context("no answer from the person in time")
            );
        }
        other => other?,
    };
    match code {
        0 => {}
        1 => return Err(InputRefused.into()),
        // Ended another way (the dialog closed): as the person's no.
        _ => {
            return Err(anyhow::Error::new(InputStopped)
                .context(format!("the portal did not start the session ({code})")));
        }
    }
    match results
        .get("restore_token")
        .and_then(|v| String::try_from(v.clone()).ok())
    {
        Some(new) => save_token(&new),
        None => info!("input: the desktop keeps no permission for next time"),
    }
    Ok(())
}

fn token_file() -> Option<PathBuf> {
    Some(
        crate::platform::config_home()?
            .join("prostir")
            .join("input-token"),
    )
}

/// The kept permission is no more: the person said no, or stopped the control. The
/// next session asks again (with the desktop's dialog), never silently.
fn forget_token() {
    if let Some(file) = token_file()
        && std::fs::remove_file(&file).is_ok()
    {
        info!("input: the desktop's kept permission is forgotten");
    }
}

fn load_token() -> Option<String> {
    let text = std::fs::read_to_string(token_file()?).ok()?;
    let t = text.trim();
    (!t.is_empty()).then(|| t.to_owned())
}

/// Keeps `token` for the next session, readable by the user alone.
fn save_token(token: &str) {
    let Some(file) = token_file() else {
        return;
    };
    let saved = (|| -> std::io::Result<()> {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let new = file.with_extension("new");
        // One left behind could be readable by others: made anew, for the user alone.
        let _ = std::fs::remove_file(&new);
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&new)?;
        f.write_all(token.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&new, &file)
    })();
    if let Err(e) = saved {
        warn!("input: the desktop's permission was not kept: {e}");
    }
}

/// The portal's session and libei's devices in it. Dropped, the session closes.
struct PortalInput {
    conn: Connection,
    session: String,
    ei: Ei,
}

impl Drop for PortalInput {
    fn drop(&mut self) {
        close(&self.conn, &self.session, "org.freedesktop.portal.Session");
    }
}

impl PortalInput {
    /// What came of a call; stopped by the person, the kept permission goes.
    fn checked<T>(r: Result<T>) -> Result<T> {
        if let Err(e) = &r
            && said_no(e)
        {
            forget_token();
        }
        r
    }
}

impl Injector for PortalInput {
    fn touches(&self) -> bool {
        self.ei.touches()
    }
    fn pointer_to(&mut self, at: &Spot) -> Result<()> {
        Self::checked(self.ei.pointer_to(at))
    }
    fn pointer_by(&mut self, dx: f64, dy: f64) -> Result<()> {
        Self::checked(self.ei.pointer_by(dx, dy))
    }
    fn button(&mut self, button: MouseButton, down: bool) -> Result<()> {
        Self::checked(self.ei.button(button, down))
    }
    fn scroll(&mut self, dx: i32, dy: i32) -> Result<()> {
        Self::checked(self.ei.scroll(dx, dy))
    }
    fn touch(&mut self, id: u32, phase: TouchPhase, at: &Spot) -> Result<()> {
        Self::checked(self.ei.touch(id, phase, at))
    }
    fn key(&mut self, code: u16, down: bool) -> Result<()> {
        Self::checked(self.ei.key(code, down))
    }
    fn pump(&mut self) -> Result<()> {
        Self::checked(self.ei.pump())
    }
}
