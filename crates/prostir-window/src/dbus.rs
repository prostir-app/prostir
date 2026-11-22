//! The local API over the session bus (LOCAL-API.md): `GetState` and `Call` on
//! `io.github.prostir_app.Prostir1`, the `StateChanged` signal, and the name
//! coming and going on the bus.

use std::time::Duration;

use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::message::Type as MessageType;
use zbus::proxy::CacheProperties;
use zbus::{MatchRule, Message};

use crate::api::*;

/// A command for the headset waits up to 30 s for the headset; the service answers
/// within this.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(CALL_SECONDS);

pub struct DbusClient {
    conn: Connection,
    proxy: Proxy<'static>,
}

/// The session bus, with the timeout calls to the service need.
pub fn session() -> Result<Connection, ClientError> {
    zbus::blocking::connection::Builder::session()
        .and_then(|b| b.method_timeout(CALL_TIMEOUT).build())
        .map_err(|e| ClientError::NoBus(e.to_string()))
}

/// A desktop notification (`org.freedesktop.Notifications`), the same on Plasma and on
/// Hyprland's notification daemons; an error where none runs.
pub fn notify(conn: &Connection, summary: &str, body: &str) -> zbus::Result<()> {
    use std::collections::HashMap;
    use zbus::zvariant::Value;
    let hints: HashMap<&str, Value<'_>> = HashMap::new();
    conn.call_method(
        Some("org.freedesktop.Notifications"),
        "/org/freedesktop/Notifications",
        Some("org.freedesktop.Notifications"),
        "Notify",
        &(
            "Prostir",
            0u32,
            "prostir",
            summary,
            body,
            Vec::<&str>::new(),
            hints,
            -1i32,
        ),
    )
    .map(drop)
}

impl DbusClient {
    pub fn new(conn: Connection) -> Result<Self, ClientError> {
        let proxy = zbus::blocking::proxy::Builder::new(&conn)
            .destination(BUS_NAME)
            .and_then(|b| b.path(OBJECT_PATH))
            .and_then(|b| b.interface(INTERFACE))
            .map(|b| b.cache_properties(CacheProperties::No))
            .and_then(zbus::blocking::proxy::Builder::build)
            .map_err(broken)?;
        Ok(Self { conn, proxy })
    }
}

#[allow(clippy::needless_pass_by_value, reason = "made for map_err(broken)")]
fn broken(e: zbus::Error) -> ClientError {
    ClientError::Broken(e.to_string())
}

/// A bus error as the window understands it.
fn error(e: zbus::Error) -> ClientError {
    match e {
        zbus::Error::MethodError(name, detail, _) => {
            let name = name.as_str();
            let message = detail.unwrap_or_default();
            if let Some(code) = name.strip_prefix(ERROR_PREFIX) {
                return ClientError::Refused {
                    code: ApiError::from_name(code),
                    message,
                };
            }
            match name {
                "org.freedesktop.DBus.Error.ServiceUnknown"
                | "org.freedesktop.DBus.Error.NameHasNoOwner" => ClientError::NotRunning,
                // The name is there, the interface or method is not: a Prostir from
                // before this API (as `prostir ctl` reads it too).
                "org.freedesktop.DBus.Error.UnknownObject"
                | "org.freedesktop.DBus.Error.UnknownInterface"
                | "org.freedesktop.DBus.Error.UnknownMethod" => ClientError::TooOld,
                "org.freedesktop.DBus.Error.NoReply" | "org.freedesktop.DBus.Error.Timeout" => {
                    ClientError::TimedOut
                }
                _ => ClientError::Broken(format!("{name}: {message}")),
            }
        }
        zbus::Error::InputOutput(e) if e.kind() == std::io::ErrorKind::TimedOut => {
            ClientError::TimedOut
        }
        e => broken(e),
    }
}

/// The unique name that owns `BUS_NAME` now, if any.
fn name_owner(conn: &Connection) -> Option<String> {
    let bus = zbus::blocking::fdo::DBusProxy::new(conn).ok()?;
    let owner = bus.get_name_owner(BUS_NAME.try_into().ok()?).ok()?;
    Some(owner.as_str().to_owned())
}

fn state_of(msg: &Message) -> Result<Snapshot, ClientError> {
    let (revision, json): (u32, String) = msg.body().deserialize().map_err(broken)?;
    Snapshot::parse(revision, &json)
}

fn iterate(
    conn: &Connection,
    rule: MatchRule<'static>,
    name: &str,
    mut each: impl FnMut(Message) + Send + 'static,
) -> Result<(), ClientError> {
    let messages = MessageIterator::for_match_rule(rule, conn, Some(64)).map_err(broken)?;
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            for msg in messages.flatten() {
                each(msg);
            }
        })
        .map_err(|e| ClientError::Broken(e.to_string()))?;
    Ok(())
}

impl Client for DbusClient {
    fn get_state(&self) -> Result<Snapshot, ClientError> {
        let msg = self.proxy.call_method("GetState", &()).map_err(error)?;
        state_of(&msg)
    }

    fn call(&self, request: &Request) -> Result<Answer, ClientError> {
        let json: String = self
            .proxy
            .call("Call", &(request.to_json(),))
            .map_err(error)?;
        parse_answer(&json)
    }

    fn present(&self) -> Option<bool> {
        let bus = zbus::blocking::fdo::DBusProxy::new(&self.conn).ok()?;
        bus.name_has_owner(BUS_NAME.try_into().ok()?).ok()
    }

    fn watch(&self, sink: Sink) -> Result<(), ClientError> {
        let sink = std::sync::Arc::new(sink);
        // Who owns the name now: a StateChanged from anyone else on the bus is not
        // the service's, whatever its path and interface say.
        let owner = std::sync::Arc::new(std::sync::Mutex::new(name_owner(&self.conn)));
        // Signals from whoever owns the name; matched by path and interface, so the
        // subscription works before the service has appeared.
        let changed = MatchRule::builder()
            .msg_type(MessageType::Signal)
            .interface(INTERFACE)
            .and_then(|b| b.member("StateChanged"))
            .and_then(|b| b.path(OBJECT_PATH))
            .map_err(broken)?
            .build();
        let s = sink.clone();
        let o = owner.clone();
        iterate(&self.conn, changed, "api-signals", move |msg| {
            let header = msg.header();
            let from = header.sender().map(|s| s.as_str().to_owned());
            if from.is_none() || from != *o.lock().unwrap() {
                log::debug!("StateChanged from {from:?}, not the service: ignored");
                return;
            }
            match state_of(&msg) {
                Ok(snap) => s(Change::State(snap)),
                Err(e) => log::warn!("StateChanged that does not read: {e}"),
            }
        })?;
        let owner_changed = MatchRule::builder()
            .msg_type(MessageType::Signal)
            .sender("org.freedesktop.DBus")
            .and_then(|b| b.interface("org.freedesktop.DBus"))
            .and_then(|b| b.member("NameOwnerChanged"))
            .and_then(|b| b.arg(0, BUS_NAME))
            .map_err(broken)?
            .build();
        iterate(&self.conn, owner_changed, "api-owner", move |msg| {
            let Ok((_, _, new)) = msg.body().deserialize::<(String, String, String)>() else {
                return;
            };
            *owner.lock().unwrap() = (!new.is_empty()).then(|| new.clone());
            sink(if new.is_empty() {
                Change::Vanished
            } else {
                Change::Appeared
            });
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    //! Against a private bus (`dbus-daemon --session`) with a service written from the
    //! note's interface; skipped where dbus-daemon is not installed.

    use super::*;
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc;

    pub(crate) struct Bus {
        daemon: Child,
        address: String,
    }

    impl Drop for Bus {
        fn drop(&mut self) {
            let _ = self.daemon.kill();
            let _ = self.daemon.wait();
        }
    }

    pub(crate) fn bus() -> Option<Bus> {
        let mut daemon = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| eprintln!("skipped: no dbus-daemon ({e})"))
            .ok()?;
        let mut line = String::new();
        std::io::BufRead::read_line(
            &mut std::io::BufReader::new(daemon.stdout.take().unwrap()),
            &mut line,
        )
        .ok()?;
        Some(Bus {
            daemon,
            address: line.trim().to_owned(),
        })
    }

    pub(crate) fn connect(bus: &Bus) -> Connection {
        zbus::blocking::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .method_timeout(Duration::from_secs(5))
            .build()
            .unwrap()
    }

    struct Service {
        revision: u32,
    }

    #[zbus::interface(name = "io.github.prostir_app.Prostir1")]
    impl Service {
        fn get_state(&self) -> (u32, String) {
            (self.revision, r#"{"v":1,"pc":{"name":"desk"}}"#.into())
        }

        #[allow(clippy::needless_pass_by_value, reason = "a D-Bus method's argument")]
        fn call(&mut self, request: String) -> Result<String, ServiceError> {
            if request.contains("remove_monitor") {
                return Err(ServiceError::LastDisplay("Keep one monitor.".into()));
            }
            if request.contains("volume") {
                return Err(ServiceError::Unsupported("Not yet.".into()));
            }
            self.revision += 1;
            Ok(format!(r#"{{"id":7,"revision":{}}}"#, self.revision))
        }
    }

    #[derive(Debug, zbus::DBusError)]
    #[zbus(prefix = "io.github.prostir_app.Prostir1.Error")]
    enum ServiceError {
        LastDisplay(String),
        Unsupported(String),
    }

    fn serve(bus: &Bus) -> Connection {
        let conn = connect(bus);
        conn.object_server()
            .at(OBJECT_PATH, Service { revision: 3 })
            .unwrap();
        conn.request_name(BUS_NAME).unwrap();
        conn
    }

    #[test]
    fn talks_to_the_service_and_follows_it() {
        let Some(bus) = bus() else { return };
        let client = DbusClient::new(connect(&bus)).unwrap();
        assert_eq!(client.get_state(), Err(ClientError::NotRunning));
        let (tx, rx) = mpsc::channel();
        client
            .watch(Box::new(move |c| {
                let _ = tx.send(c);
            }))
            .unwrap();
        let next = || rx.recv_timeout(Duration::from_secs(5)).expect("a change");

        let service = serve(&bus);
        assert_eq!(next(), Change::Appeared);
        let s = client.get_state().unwrap();
        assert_eq!((s.revision, s.state.pc.name.as_str()), (3, "desk"));

        let reply = client
            .call(&Request::AddMonitor {
                w: 1920,
                h: 1080,
                scale: 100,
            })
            .unwrap();
        assert_eq!((reply.id, reply.revision), (Some(7), 4));
        assert_eq!(
            client.call(&Request::RemoveMonitor { id: 1 }),
            Err(ClientError::Refused {
                code: ApiError::LastDisplay,
                message: "Keep one monitor.".into()
            })
        );
        assert!(
            client
                .call(&Request::Volume { level: 3 })
                .unwrap_err()
                .unsupported()
        );

        service
            .emit_signal(
                None::<()>,
                OBJECT_PATH,
                INTERFACE,
                "StateChanged",
                &(9u32, r#"{"v":1,"pc":{"name":"desk 2"}}"#),
            )
            .unwrap();
        match next() {
            Change::State(s) => assert_eq!((s.revision, s.state.pc.name.as_str()), (9, "desk 2")),
            other => panic!("{other:?}"),
        }
        // The same signal from someone else on the bus is not taken.
        let stranger = connect(&bus);
        stranger
            .emit_signal(
                None::<()>,
                OBJECT_PATH,
                INTERFACE,
                "StateChanged",
                &(10u32, r#"{"v":1,"pc":{"name":"not the service"}}"#),
            )
            .unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());

        service.release_name(BUS_NAME).unwrap();
        assert_eq!(next(), Change::Vanished);
        assert_eq!(client.get_state(), Err(ClientError::NotRunning));
    }
}
