//! `prostir mcp`: the local API as tools of the Model Context Protocol, so an agent
//! (Claude Code, say) drives the running Prostir and looks at what came of it: the
//! state, the monitors, their arrangement, a picture of a monitor, the commands for
//! the headset. A thin wrapper: each tool is one request of the local API (see
//! `crate::api` and `LOCAL-API.md`) through a [`CtlClient`], as `prostir ctl` makes
//! them, with the same answers and errors.
//!
//! MCP runs over stdio: JSON-RPC 2.0, one message a line. Only the protocol's
//! messages go to stdout; the log goes to stderr. A thread of its own reads stdin: it
//! answers `ping` at once and takes `notifications/cancelled`, while the requests go
//! to the server one after the other; a tool that waits (a command for the headset,
//! `wait_for_state`) holds the next ones meanwhile, and a wait ends when it is
//! cancelled. Nothing starts Prostir but the tool `start`.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use crate::api::{Answer, ApiError, Request};
use crate::ctl::{self, ClientError, CtlClient, Watched};

/// The versions of MCP this server speaks, the newest first.
const VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// JSON-RPC's error codes.
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// How long `wait_for_state` waits by default, and at most.
const WAIT_DEFAULT: f64 = 10.0;
const WAIT_MAX: f64 = 50.0;
/// The longest a wait sleeps at once, so that a cancellation ends it soon.
const SLICE: Duration = Duration::from_millis(100);
/// How often a wait says how long it has waited, where the client asked for that.
const PROGRESS_EVERY: Duration = Duration::from_secs(1);
/// A picture larger than this is not sent: its file is kept, and the answer names it.
const IMAGE_MAX: usize = 5 * 1024 * 1024;
/// How a PNG file begins.
const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

const INSTRUCTIONS: &str = "\
Prostir streams virtual monitors of this PC (KDE Plasma, Hyprland) to a Meta Quest \
headset. These tools are the local API of the Prostir running for this user: each one \
is one request, answered with the revision of the state that already shows the change \
(LOCAL-API.md describes every field). Look at the result with get_state, \
list_monitors, wait_for_state and screenshot_monitor. Prostir runs only while \
something needs it (a headset, monitors waiting for one, its settings window). While \
it is not running, the tools answer NotRunning; only the start tool starts it. \
Commands for the headset need a connected headset (NotConnected otherwise) and wait up \
to 30 s for it.";

/// Where the protocol's messages go (stdout): whole lines, from any thread.
pub struct Out(Mutex<Box<dyn Write + Send>>);

impl Out {
    pub fn new(to: impl Write + Send + 'static) -> Self {
        Self(Mutex::new(Box::new(to)))
    }

    fn send(&self, message: &Value) {
        let mut to = self.0.lock().unwrap();
        // A client that went away is seen as the end of stdin.
        let _ = writeln!(to, "{message}").and_then(|()| to.flush());
    }
}

/// Counts the state's changes the bus told of, so a wait wakes on each.
#[derive(Default)]
pub struct Changes {
    seen: Mutex<u64>,
    told: Condvar,
}

impl Changes {
    /// Something changed.
    pub fn bump(&self) {
        *self.seen.lock().unwrap() += 1;
        self.told.notify_all();
    }

    fn count(&self) -> u64 {
        *self.seen.lock().unwrap()
    }

    /// Sleeps until a change after `since`, `timeout` at most: whether one came.
    fn wait(&self, since: u64, timeout: Duration) -> bool {
        let seen = self.seen.lock().unwrap();
        let (seen, _) = self
            .told
            .wait_timeout_while(seen, timeout, |n| *n == since)
            .unwrap();
        *seen != since
    }
}

/// Starts following the state's changes into the given [`Changes`].
pub type Follow = Box<dyn FnOnce(Arc<Changes>)>;
/// A new connection to Prostir's bus.
pub type Connect = Box<dyn Fn() -> Result<Box<dyn CtlClient>, ClientError>>;
/// Starts Prostir (the tool `start`): what to tell the agent, or the error's name and
/// why.
pub type Start = Box<dyn Fn() -> Result<String, (&'static str, String)>>;

/// What the request being handled is told while it runs.
#[derive(Default)]
struct Asked {
    /// The client cancelled it.
    cancelled: Arc<AtomicBool>,
    /// Where the client asked to hear how it goes (`_meta.progressToken`).
    progress: Option<Value>,
}

/// What the thread reading stdin and the server share: the request being handled and
/// its cancellation, and requests cancelled before they began. Under one lock: a
/// cancellation that comes as its request begins is seen either way.
#[derive(Default)]
struct Running(Mutex<Requests>);

#[derive(Default)]
struct Requests {
    now: Option<(Value, Arc<AtomicBool>)>,
    early: Vec<Value>,
}

impl Running {
    /// The client cancelled request `id`.
    fn cancel(&self, id: &Value) {
        let mut requests = self.0.lock().unwrap();
        if let Some((now, flag)) = requests.now.as_ref()
            && now == id
        {
            flag.store(true, Ordering::Release);
            return;
        }
        // Only a few are kept: one cancelled long ago never comes.
        if requests.early.len() >= 64 {
            requests.early.remove(0);
        }
        requests.early.push(id.clone());
    }

    /// Request `id` begins: `None` when it was cancelled already.
    fn begin(&self, id: &Value) -> Option<Arc<AtomicBool>> {
        let mut requests = self.0.lock().unwrap();
        if let Some(i) = requests.early.iter().position(|e| e == id) {
            requests.early.remove(i);
            return None;
        }
        let flag = Arc::new(AtomicBool::new(false));
        requests.now = Some((id.clone(), flag.clone()));
        Some(flag)
    }

    fn end(&self) {
        self.0.lock().unwrap().now.take();
    }
}

/// The MCP server: the way to Prostir, and what a session of the protocol keeps.
pub struct Server {
    connect: Connect,
    /// Connected when a tool first needs it, and again after a connection broke.
    client: Option<Box<dyn CtlClient>>,
    start: Start,
    /// Starts following the state's changes, once a wait needs it.
    follow: Option<Follow>,
    changes: Option<Arc<Changes>>,
    /// How often a wait looks at the state itself, besides waking on each change.
    poll: Duration,
    /// Where progress goes, while serving.
    out: Option<Arc<Out>>,
    /// The folder Prostir saves screenshots to: only a picture there is read.
    shots: PathBuf,
}

impl Server {
    pub fn new(connect: Connect, start: Start, follow: Option<Follow>, shots: PathBuf) -> Self {
        let poll = if follow.is_some() {
            Duration::from_secs(1)
        } else {
            Duration::from_millis(200)
        };
        Self {
            connect,
            client: None,
            start,
            follow,
            changes: None,
            poll,
            out: None,
            shots,
        }
    }

    /// Reads messages from `input` until it ends, answering each to `out`.
    pub fn serve(&mut self, input: Box<dyn BufRead + Send>, out: &Arc<Out>) -> std::io::Result<()> {
        self.out = Some(out.clone());
        let running = Arc::new(Running::default());
        let (to_server, messages) = mpsc::channel();
        let reader = {
            let (out, running) = (out.clone(), running.clone());
            std::thread::Builder::new()
                .name("mcp-stdin".into())
                .spawn(move || read(input, &out, &running, &to_server))?
        };
        for message in messages {
            if let Some(reply) = self.message(message, &running) {
                out.send(&reply);
            }
        }
        let _ = reader.join();
        Ok(())
    }

    /// One line of input: the line to answer with, if any (not for notifications).
    #[cfg(test)]
    fn handle_line(&mut self, line: &str) -> Option<String> {
        let reply = match serde_json::from_slice::<Value>(line.as_bytes()) {
            Ok(message) => self.message(message, &Running::default()),
            Err(e) => Some(not_json(&e)),
        };
        reply.map(|r| r.to_string())
    }

    /// One message from the client (a request, a notification or a batch): the answer,
    /// if there is one. A request cancelled before or while it ran gets none.
    fn message(&mut self, message: Value, running: &Running) -> Option<Value> {
        let message = match message {
            // A batch (MCP up to 2025-03-26): an answer for each request in it.
            Value::Array(batch) if batch.is_empty() => {
                return Some(failure(&Value::Null, INVALID_REQUEST, "an empty batch"));
            }
            Value::Array(batch) => {
                let replies: Vec<Value> = batch
                    .into_iter()
                    .filter_map(|m| self.handle(m, &Asked::default()))
                    .collect();
                return (!replies.is_empty()).then_some(Value::Array(replies));
            }
            message => message,
        };
        let id = message
            .get("id")
            .filter(|id| id.is_string() || id.is_number())
            .cloned();
        let Some(id) = id.filter(|_| message.get("method").is_some()) else {
            return self.handle(message, &Asked::default());
        };
        let cancelled = running.begin(&id)?;
        let asked = Asked {
            cancelled: cancelled.clone(),
            progress: message
                .pointer("/params/_meta/progressToken")
                .filter(|t| t.is_string() || t.is_number())
                .cloned(),
        };
        let reply = self.handle(message, &asked);
        running.end();
        // The client gave up on it: it expects no answer.
        reply.filter(|_| !cancelled.load(Ordering::Acquire))
    }

    /// One message: its answer, `None` for a notification or a response.
    fn handle(&mut self, message: Value, asked: &Asked) -> Option<Value> {
        let Value::Object(mut message) = message else {
            return Some(failure(&Value::Null, INVALID_REQUEST, "not an object"));
        };
        let id = message.remove("id");
        if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Some(failure(
                &id.unwrap_or_default(),
                INVALID_REQUEST,
                "not JSON-RPC 2.0",
            ));
        }
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let params = message.remove("params").unwrap_or_else(|| json!({}));
        match (method, id) {
            (Some(method), Some(id)) if id.is_string() || id.is_number() => {
                Some(match self.method(&method, &params, asked) {
                    Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                    Err((code, text)) => failure(&id, code, &text),
                })
            }
            (Some(_), Some(id)) => Some(failure(
                &id,
                INVALID_REQUEST,
                "an id is a string or a number",
            )),
            // Notifications (`notifications/initialized`; `notifications/cancelled` is
            // taken as it comes): nothing to answer, nothing to do.
            (Some(method), None) => {
                log::debug!("mcp: {method}");
                None
            }
            // A response to a request of ours: this server sends none.
            (None, Some(_)) if message.contains_key("result") || message.contains_key("error") => {
                None
            }
            (None, id) => Some(failure(
                &id.unwrap_or_default(),
                INVALID_REQUEST,
                "no method",
            )),
        }
    }

    fn method(
        &mut self,
        method: &str,
        params: &Value,
        asked: &Asked,
    ) -> Result<Value, (i64, String)> {
        match method {
            "initialize" => {
                let wanted = params.get("protocolVersion").and_then(Value::as_str);
                let version = wanted
                    .filter(|v| VERSIONS.contains(v))
                    .unwrap_or(VERSIONS[0]);
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {
                        "name": "prostir",
                        "title": "Prostir",
                        "version": prostir_proto::app_version(env!("CARGO_PKG_VERSION")),
                    },
                    "instructions": INSTRUCTIONS,
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => {
                Ok(json!({"tools": TOOLS.iter().map(Tool::listed).collect::<Vec<_>>()}))
            }
            "tools/call" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let Some(tool) = TOOLS.iter().find(|t| t.name == name) else {
                    return Err((INVALID_PARAMS, format!("no tool {name:?}")));
                };
                let args = match params.get("arguments") {
                    None | Some(Value::Null) => Map::new(),
                    Some(Value::Object(args)) => args.clone(),
                    Some(_) => return Err((INVALID_PARAMS, "arguments are an object".into())),
                };
                Ok(match self.call(tool, &args, asked) {
                    Ok(content) => json!({"content": content}),
                    Err(e) => json!({"content": [text(&e.to_string())], "isError": true}),
                })
            }
            _ => Err((METHOD_NOT_FOUND, format!("no method {method:?}"))),
        }
    }

    /// The connection to Prostir's bus, made now if there is none.
    fn connected(&mut self) -> Result<&dyn CtlClient, Value> {
        if self.client.is_none() {
            let client = (self.connect)().map_err(|e| refused(&e, false))?;
            self.client = Some(client);
        }
        match self.client.as_deref() {
            Some(client) => Ok(client),
            None => Err(refused(&ClientError::NotRunning, false)),
        }
    }

    /// What `e` says, the connection let go when it broke (made again next time).
    fn failed(&mut self, e: &ClientError, for_headset: bool) -> Value {
        if matches!(e, ClientError::Broken(_) | ClientError::NoBus(_)) {
            self.client = None;
        }
        refused(e, for_headset)
    }

    /// Carries out `tool` with `args`: what it gives, or the error as JSON.
    fn call(
        &mut self,
        tool: &Tool,
        args: &Map<String, Value>,
        asked: &Asked,
    ) -> Result<Vec<Value>, Value> {
        if let Some(unknown) = args
            .keys()
            .find(|k| !tool.args.iter().any(|a| a.name == *k))
        {
            return Err(bad(&format!("{}: no argument {unknown:?}", tool.name)));
        }
        match tool.does {
            Does::Ask(request) => {
                let mut fields = args.clone();
                fields.insert("request".into(), request.into());
                let request = Request::from_json(&Value::Object(fields).to_string())
                    .map_err(|f| json!({"error": f.error.name(), "text": f.text}))?;
                not_a_yes_to_counting(&request)?;
                let answer = self.ask(&request)?;
                Ok(vec![text(&answer.to_json())])
            }
            Does::Call => {
                let Some(request) = args.get("request").filter(|r| r.is_object()) else {
                    return Err(bad("request: a request object"));
                };
                let request = Request::from_json(&request.to_string())
                    .map_err(|f| json!({"error": f.error.name(), "text": f.text}))?;
                not_a_yes_to_counting(&request)?;
                let answer = self.ask(&request)?;
                Ok(vec![text(&answer.to_json())])
            }
            Does::State => {
                let (revision, state) = self.state()?;
                Ok(vec![text(
                    &json!({"revision": revision, "state": state}).to_string(),
                )])
            }
            Does::Monitors => {
                let (revision, state) = self.state()?;
                let shown = json!({
                    "revision": revision,
                    "primary": state.get("primary"),
                    "displays": state.get("displays"),
                });
                Ok(vec![text(&shown.to_string())])
            }
            Does::Screenshot => self.screenshot(args),
            Does::Wait => self.wait(args, asked),
            Does::Start => match (self.start)() {
                Ok(said) => Ok(vec![text(
                    &json!({"started": true, "text": said}).to_string(),
                )]),
                Err((name, why)) => Err(json!({"error": name, "text": why})),
            },
        }
    }

    fn ask(&mut self, request: &Request) -> Result<Answer, Value> {
        let asked = ctl::ask(self.connected()?, request);
        asked.map_err(|e| self.failed(&e, request.for_headset()))
    }

    /// The state now: its revision and its JSON.
    fn state(&mut self) -> Result<(u32, Value), Value> {
        let got = self.connected()?.state();
        let (revision, text) = got.map_err(|e| self.failed(&e, false))?;
        let state = serde_json::from_str(&text).map_err(|e| {
            json!({"error": ApiError::Other.name(), "text": format!("the state does not read: {e}")})
        })?;
        Ok((revision, state))
    }

    /// `screenshot_monitor`: the picture, read from the file Prostir saved it to
    /// (removed afterwards, unless `keep`, or it is too large to send).
    fn screenshot(&mut self, args: &Map<String, Value>) -> Result<Vec<Value>, Value> {
        let id = args
            .get("id")
            .and_then(Value::as_u64)
            .and_then(|id| u32::try_from(id).ok())
            .ok_or_else(|| bad("id: a display's id"))?;
        let keep = match args.get("keep") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(keep)) => *keep,
            Some(_) => return Err(bad("keep: true or false")),
        };
        let answer = self.ask(&Request::Screenshot { id })?;
        let Some(path) = answer.path.clone() else {
            return Err(
                json!({"error": ApiError::Other.name(), "text": "the answer names no file"}),
            );
        };
        let capture = |text: String| json!({"error": ApiError::Capture.name(), "text": text});
        let png = picture_at(Path::new(&path), &self.shots).map_err(capture)?;
        let large = png.len() > IMAGE_MAX;
        let keep = keep || large;
        if !keep && let Err(e) = std::fs::remove_file(&path) {
            log::warn!("mcp: cannot remove {path}: {e}");
        }
        let mut said = json!({
            "id": id,
            "whole": answer.whole.unwrap_or(false),
            "path": keep.then_some(&path),
            "bytes": png.len(),
            "revision": answer.revision,
        });
        if large {
            said["text"] = format!(
                "too large to send (more than {} MB): kept at path",
                IMAGE_MAX / (1024 * 1024)
            )
            .into();
            return Ok(vec![text(&said.to_string())]);
        }
        Ok(vec![
            text(&said.to_string()),
            json!({"type": "image", "data": base64(&png), "mimeType": "image/png"}),
        ])
    }

    /// `wait_for_state`: until the condition holds in the state, the time is up, or
    /// the client cancelled it.
    fn wait(&mut self, args: &Map<String, Value>, asked: &Asked) -> Result<Vec<Value>, Value> {
        let condition = Condition::read(args).map_err(|e| bad(&e))?;
        let timeout = match args.get("timeout") {
            None | Some(Value::Null) => WAIT_DEFAULT,
            Some(t) => t
                .as_f64()
                .filter(|t| (0.0..=WAIT_MAX).contains(t))
                .ok_or_else(|| bad("timeout: 0 to 50 seconds"))?,
        };
        // Not running now: said at once, as every other tool says it.
        if !self.connected()?.running() {
            return Err(refused(&ClientError::NotRunning, false));
        }
        if self.changes.is_none()
            && let Some(follow) = self.follow.take()
        {
            let changes = Arc::new(Changes::default());
            follow(changes.clone());
            self.changes = Some(changes);
        }
        let Some(client) = self.client.as_deref() else {
            return Err(refused(&ClientError::NotRunning, false));
        };
        let started = Instant::now();
        let until = started + Duration::from_secs_f64(timeout);
        let mut told = started;
        let mut first = None;
        // Prostir was away during the wait: back, its state is a change too.
        let mut was_away = false;
        let mut last: (Option<u32>, Option<Value>) = (None, None);
        loop {
            let seen = self.changes.as_ref().map(|c| c.count());
            match client.state() {
                Ok((revision, json)) => {
                    let state: Value = serde_json::from_str(&json).unwrap_or_default();
                    let value = state.pointer(&condition.pointer).cloned();
                    let first = *first.get_or_insert(revision);
                    if condition.holds(value.as_ref(), was_away || revision != first) {
                        let waited = started.elapsed().as_millis() as u64;
                        let done =
                            json!({"revision": revision, "value": value, "waited_ms": waited});
                        return Ok(vec![text(&done.to_string())]);
                    }
                    last = (Some(revision), value);
                }
                // Gone meanwhile (restarting, say): looked at again until the time is up.
                Err(ClientError::NotRunning) => {
                    was_away = true;
                    last.0 = None;
                }
                Err(e) => return Err(refused(&e, false)),
            }
            if Instant::now() >= until {
                let why = if last.0.is_none() {
                    "Prostir is not running"
                } else {
                    "the condition did not hold in time"
                };
                return Err(json!({
                    "error": ApiError::TimedOut.name(),
                    "text": format!("{why} (waited {timeout} s)"),
                    "revision": last.0,
                    "value": last.1,
                }));
            }
            // Asleep until the state changes, the next look of its own, the end, or a
            // cancellation.
            let look = Instant::now() + self.poll;
            loop {
                if asked.cancelled.load(Ordering::Acquire) {
                    return Err(json!({"error": ApiError::Other.name(), "text": "cancelled"}));
                }
                let now = Instant::now();
                if let (Some(token), Some(out)) = (&asked.progress, &self.out)
                    && now.duration_since(told) >= PROGRESS_EVERY
                {
                    told = now;
                    out.send(&json!({
                        "jsonrpc": "2.0",
                        "method": "notifications/progress",
                        "params": {
                            "progressToken": token,
                            "progress": now.duration_since(started).as_secs_f64(),
                            "total": timeout,
                            "message": "waiting for the state",
                        },
                    }));
                }
                let next = look.min(until);
                if now >= next {
                    break;
                }
                let slice = SLICE.min(next - now);
                match (&self.changes, seen) {
                    (Some(changes), Some(seen)) => {
                        if changes.wait(seen, slice) {
                            break;
                        }
                    }
                    _ => std::thread::sleep(slice),
                }
            }
        }
    }
}

/// Reads stdin, a message a line: a `ping` is answered and a cancellation taken here
/// at once, everything else goes to the server. A line that is no JSON (not even
/// UTF-8) is answered with a parse error, and reading goes on.
fn read(
    mut input: Box<dyn BufRead + Send>,
    out: &Out,
    running: &Running,
    to_server: &mpsc::Sender<Value>,
) {
    let mut line = Vec::new();
    loop {
        line.clear();
        match input.read_until(b'\n', &mut line) {
            Ok(0) => return,
            Ok(_) => {}
            Err(e) => {
                log::warn!("mcp: stdin: {e}");
                return;
            }
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let message: Value = match serde_json::from_slice(&line) {
            Ok(message) => message,
            Err(e) => {
                out.send(&not_json(&e));
                continue;
            }
        };
        let rpc = message.get("jsonrpc").and_then(Value::as_str) == Some("2.0");
        let id = message
            .get("id")
            .filter(|id| id.is_string() || id.is_number());
        match (rpc, message.get("method").and_then(Value::as_str), id) {
            (true, Some("ping"), Some(id)) => {
                out.send(&json!({"jsonrpc": "2.0", "id": id, "result": {}}));
            }
            (true, Some("notifications/cancelled"), None) => {
                if let Some(id) = message.pointer("/params/requestId") {
                    running.cancel(id);
                }
            }
            _ => {
                if to_server.send(message).is_err() {
                    return;
                }
            }
        }
    }
}

/// The answer to a line that is no JSON.
fn not_json(e: &serde_json::Error) -> Value {
    failure(&Value::Null, PARSE_ERROR, &format!("not JSON: {e}"))
}

/// The picture in the file at `path`, if it is one Prostir saved: a regular file (not
/// a link) ending in `.png` right in `shots`, the folder screenshots go to, that begins
/// as a PNG does. Anything else is left as it is.
fn picture_at(path: &Path, shots: &Path) -> Result<Vec<u8>, String> {
    let said = |why: &str| format!("{}: {why}", path.display());
    let meta = std::fs::symlink_metadata(path).map_err(|e| said(&e.to_string()))?;
    if !meta.file_type().is_file() {
        return Err(said("not a regular file"));
    }
    if path.extension().and_then(|e| e.to_str()) != Some("png") {
        return Err(said("not a .png file"));
    }
    let folder = path.parent().and_then(|p| p.canonicalize().ok());
    if folder.is_none() || folder != shots.canonicalize().ok() {
        return Err(said(&format!(
            "not in {}, where Prostir saves screenshots",
            shots.display()
        )));
    }
    let png = std::fs::read(path).map_err(|e| said(&e.to_string()))?;
    if !png.starts_with(PNG_MAGIC) {
        return Err(said("not a PNG picture"));
    }
    Ok(png)
}

/// A JSON-RPC error answer.
fn failure(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// Text content of a tool's result.
fn text(text: &str) -> Value {
    json!({"type": "text", "text": text})
}

/// Arguments that do not read.
/// Counting this PC needs the person's own yes: an agent never gives it, by the
/// tool or by `call`.
fn not_a_yes_to_counting(request: &Request) -> Result<(), Value> {
    match request {
        Request::SetCounting {
            counting: crate::api::Counting::Yes,
        } => Err(bad(
            "set_counting: yes is the person's own answer, given in the settings window, \
             the tray or `prostir ctl count yes`; an agent can set no or unasked",
        )),
        _ => Ok(()),
    }
}

fn bad(text: &str) -> Value {
    json!({"error": ApiError::BadArgument.name(), "text": text})
}

/// The error of a tool as `ctl --json` says it: `{"error": NAME, "text": ...}`.
fn refused(e: &ClientError, for_headset: bool) -> Value {
    let (_, name, text) = ctl::what_failed(e, for_headset);
    json!({"error": name, "text": text})
}

/// What `wait_for_state` waits for: the value at a JSON Pointer.
#[derive(Debug, PartialEq)]
struct Condition {
    pointer: String,
    test: Test,
}

#[derive(Debug, PartialEq)]
enum Test {
    Equals(Value),
    Contains(Value),
    Exists(bool),
    /// Any change of the state.
    Changed,
}

impl Condition {
    fn read(args: &Map<String, Value>) -> Result<Self, String> {
        let pointer = match args.get("pointer") {
            None | Some(Value::Null) => String::new(),
            Some(Value::String(p)) if p.is_empty() || p.starts_with('/') => p.clone(),
            Some(_) => return Err("pointer: a JSON Pointer, \"\" or starting with /".into()),
        };
        let mut tests = Vec::new();
        if let Some(v) = args.get("equals") {
            tests.push(Test::Equals(v.clone()));
        }
        if let Some(v) = args.get("contains") {
            tests.push(Test::Contains(v.clone()));
        }
        match args.get("exists") {
            None | Some(Value::Null) => {}
            Some(Value::Bool(b)) => tests.push(Test::Exists(*b)),
            Some(_) => return Err("exists: true or false".into()),
        }
        if tests.len() > 1 {
            return Err("one of equals, contains and exists at most".into());
        }
        Ok(Self {
            pointer,
            test: tests.pop().unwrap_or(Test::Changed),
        })
    }

    /// Whether it holds for `value` (`None`: nothing there); `changed`: the state is
    /// not the one the wait began with.
    fn holds(&self, value: Option<&Value>, changed: bool) -> bool {
        match &self.test {
            Test::Equals(wanted) => value == Some(wanted),
            Test::Contains(wanted) => value.is_some_and(|v| contains(v, wanted)),
            Test::Exists(wanted) => value.is_some() == *wanted,
            Test::Changed => changed,
        }
    }
}

/// Whether `value` has `wanted` in it: an array an element that has it; an object its
/// fields (each having what `wanted`'s has); a string the text; else the same value.
fn contains(value: &Value, wanted: &Value) -> bool {
    match (value, wanted) {
        (Value::Array(items), Value::Array(all)) => all
            .iter()
            .all(|w| items.iter().any(|item| contains(item, w))),
        (Value::Array(items), _) => items.iter().any(|item| contains(item, wanted)),
        (Value::Object(fields), Value::Object(wanted)) => wanted
            .iter()
            .all(|(k, w)| fields.get(k).is_some_and(|v| contains(v, w))),
        (Value::String(text), Value::String(part)) => text.contains(part.as_str()),
        _ => value == wanted,
    }
}

/// Standard base64 with padding, as MCP carries an image.
fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ABC[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// What a tool does.
#[derive(Debug, Clone, Copy)]
enum Does {
    /// The request of this name, its arguments the tool's.
    Ask(&'static str),
    /// Any request, given whole.
    Call,
    State,
    Monitors,
    Screenshot,
    Wait,
    Start,
}

/// How a tool touches the PC, for the client to decide what to ask the person.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Touch {
    Reads,
    Changes,
    Destroys,
}

struct Tool {
    name: &'static str,
    description: &'static str,
    args: &'static [Arg],
    does: Does,
    touch: Touch,
}

/// One argument of a tool, for its schema.
#[derive(Clone, Copy)]
struct Arg {
    name: &'static str,
    /// Its JSON type; empty for any.
    kind: &'static str,
    needed: bool,
    about: &'static str,
    min: Option<i64>,
    max: Option<i64>,
    /// The words it may be, where only those.
    words: &'static [&'static str],
}

impl Arg {
    const fn of(name: &'static str, kind: &'static str, about: &'static str) -> Self {
        Self {
            name,
            kind,
            needed: true,
            about,
            min: None,
            max: None,
            words: &[],
        }
    }

    const fn optional(self) -> Self {
        Self {
            needed: false,
            ..self
        }
    }

    const fn within(self, min: i64, max: i64) -> Self {
        Self {
            min: Some(min),
            max: Some(max),
            ..self
        }
    }

    const fn one_of(self, words: &'static [&'static str]) -> Self {
        Self { words, ..self }
    }

    fn schema(&self) -> Value {
        let mut schema = json!({"description": self.about});
        if !self.kind.is_empty() {
            schema["type"] = self.kind.into();
        }
        if let Some(min) = self.min {
            schema["minimum"] = min.into();
        }
        if let Some(max) = self.max {
            schema["maximum"] = max.into();
        }
        if !self.words.is_empty() {
            schema["enum"] = self.words.into();
        }
        schema
    }
}

impl Tool {
    /// As `tools/list` gives it.
    fn listed(&self) -> Value {
        let properties: Map<String, Value> = self
            .args
            .iter()
            .map(|a| (a.name.to_owned(), a.schema()))
            .collect();
        let required: Vec<&str> = self
            .args
            .iter()
            .filter(|a| a.needed)
            .map(|a| a.name)
            .collect();
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": {
                "type": "object",
                "properties": properties,
                "required": required,
                "additionalProperties": false,
            },
            "annotations": {
                "readOnlyHint": self.touch == Touch::Reads,
                "destructiveHint": self.touch == Touch::Destroys,
                "idempotentHint": self.touch == Touch::Reads,
                "openWorldHint": false,
            },
        })
    }
}

const ID: Arg = Arg::of(
    "id",
    "integer",
    "the display's id (displays[].id in the state)",
)
.within(0, u32::MAX as i64);
const ON: Arg = Arg::of("on", "boolean", "on (true) or off");

/// A command for the headset without arguments.
const fn command(name: &'static str, description: &'static str) -> Tool {
    asks(name, description, &[])
}

/// A tool that is the request of the same name.
const fn asks(name: &'static str, description: &'static str, args: &'static [Arg]) -> Tool {
    Tool {
        name,
        description,
        args,
        does: Does::Ask(name),
        touch: Touch::Changes,
    }
}

/// How far a monitor is placed at most, as the PC holds it.
const PLACE: i64 = crate::displays::PLACE_LIMIT as i64;

static TOOLS: &[Tool] = &[
    Tool {
        name: "get_state",
        description: "The whole state of Prostir as JSON with its revision: the PC, paired and \
                      connected headsets, displays (virtual monitors, the PC's own displays, window \
                      panels) with their state and place on the desktop, limits, encoding, sound, \
                      pairing, problems. LOCAL-API.md describes every field. It includes the \
                      pairing code while one is shown.",
        args: &[],
        does: Does::State,
        touch: Touch::Reads,
    },
    Tool {
        name: "list_monitors",
        description: "The displays (id, kind virtual/physical/window, name, output, mode with \
                      w, h and scale, desk rectangle in logical pixels, state \
                      starting/running/failed/absent/kept, failure) and the primary display's id. \
                      A new monitor can stand starting or running with desk null and placing \
                      true for up to 4 seconds while the headset places it: that is not off the \
                      desktop; wait for its desk (pointer /displays, contains {\"id\": ID, \
                      \"desk\": {}}).",
        args: &[],
        does: Does::Monitors,
        touch: Touch::Reads,
    },
    asks(
        "add_monitor",
        "Adds a virtual monitor of w x h pixels at a scale in percent; answers its id. It \
         stands starting until it runs (running), failed if it could not start. With no \
         headset connected and no monitors on the desktop it joins the set kept for the next \
         headset (kept). Wait for it with wait_for_state. It can have desk null (placing \
         true) for up to 4 seconds while the headset places it: wait for its desk too (pointer /displays, \
         contains {\"id\": ID, \"desk\": {}}) rather than take it as off the desktop.",
        &[
            Arg::of("w", "integer", "width in pixels").within(1, 16384),
            Arg::of("h", "integer", "height in pixels").within(1, 16384),
            Arg::of(
                "scale",
                "integer",
                "scale in percent, 100 by default (limits.scales lists them)",
            )
            .within(25, 400)
            .optional(),
        ],
    ),
    Tool {
        name: "remove_monitor",
        description: "Removes a virtual monitor (LastDisplay for the last one while a headset \
                      is connected).",
        args: &[ID],
        does: Does::Ask("remove_monitor"),
        touch: Touch::Destroys,
    },
    asks("retry_monitor", "Tries a failed monitor again now.", &[ID]),
    asks(
        "set_primary",
        "Makes a display the desktop's primary one.",
        &[ID],
    ),
    Tool {
        name: "place_monitor",
        description: "Moves a display on the desktop to about x, y (its top-left corner, logical \
                      pixels): into the row it is then in, or a row of its own above or under the \
                      others; the desktop's arrangement lines it up. Only while the monitors are on \
                      the desktop.",
        args: &[
            ID,
            Arg::of("x", "integer", "left edge, logical pixels").within(-PLACE, PLACE),
            Arg::of("y", "integer", "top edge, logical pixels").within(-PLACE, PLACE),
        ],
        does: Does::Ask("place"),
        touch: Touch::Changes,
    },
    asks(
        "set_scale",
        "Another scale for a monitor, in percent: its output is made anew with the same id \
         (starting meanwhile).",
        &[
            ID,
            Arg::of("scale", "integer", "scale in percent").within(25, 400),
        ],
    ),
    Tool {
        name: "show_pc_display",
        description: "Shows one of the PC's own displays (kind physical) in the headset, or hides it.",
        args: &[ID, Arg::of("shown", "boolean", "shown in the headset")],
        does: Does::Ask("show_physical"),
        touch: Touch::Changes,
    },
    command(
        "recenter",
        "Headset command: puts the monitors in front of the person again.",
    ),
    command(
        "line_up",
        "Headset command: lines the monitors up in front of the person.",
    ),
    command("show_mouse", "Headset command: shows where the mouse is."),
    command(
        "reset_arrangement",
        "Headset command: the monitors' arrangement as it first was.",
    ),
    asks(
        "profile",
        "Headset command: switches to a profile saved in the headset (its names are \
         headsets[].session.profiles in the state).",
        &[Arg::of(
            "name",
            "string",
            "the profile's name, or its number from 1 as the headset menu lists them",
        )],
    ),
    asks(
        "rename_profile",
        "Headset command: gives a profile saved in the headset another name.",
        &[
            Arg::of(
                "profile",
                "string",
                "the profile's name, or its number from 1 as the headset menu lists them",
            ),
            Arg::of(
                "name",
                "string",
                "its new name; empty: called by its number again",
            ),
        ],
    ),
    asks("cinema", "Headset command: cinema mode on or off.", &[ON]),
    asks("focus", "Headset command: focus mode on or off.", &[ON]),
    asks(
        "follow",
        "Headset command: the monitors follow the head, or stay.",
        &[ON],
    ),
    asks("no_limits", "Headset command: no limits on or off.", &[ON]),
    asks(
        "distance",
        "Headset command: the monitors closer (steps below 0) or further.",
        &[Arg::of("steps", "integer", "steps, below 0 closer").within(-128, 127)],
    ),
    asks(
        "preset",
        "Headset command: the headset's preset of this number.",
        &[Arg::of("preset", "integer", "the preset's number").within(0, 255)],
    ),
    asks(
        "set_quality",
        "The bitrate per monitor.",
        &[Arg::of("mbit", "integer", "Mbit/s").within(5, 100)],
    ),
    asks(
        "set_sharp",
        "The slower encoder preset that keeps small text clearer.",
        &[ON],
    ),
    asks(
        "set_codec",
        "The video codec.",
        &[
            Arg::of(
                "codec",
                "string",
                "the codec; h265 is HEVC; auto is HEVC where every side can",
            )
            .one_of(&["h264", "h265", "auto"]),
            Arg::of(
                "cpu",
                "boolean",
                "encode on the CPU even where the graphics card could",
            )
            .optional(),
        ],
    ),
    asks(
        "sound_to_headset",
        "The PC's sound plays in the headset (true) or on the PC.",
        &[ON],
    ),
    asks(
        "set_counting",
        "Counting this PC among Prostir's users (once a day one request with only the \
         date) needs the person's own yes, given in the settings window, the tray or \
         `prostir ctl count yes`: an agent can only say no, or put the question back in \
         the settings window (unasked). The state's counting has the answer, null where \
         this build or run never counts.",
        &[Arg::of("counting", "string", "no or unasked").one_of(&["no", "unasked"])],
    ),
    asks(
        "autostart",
        "Starts Prostir with the session, or not (kept in settings.txt).",
        &[ON],
    ),
    asks(
        "pair_new_headset",
        "Opens the pairing window for 3 minutes; answers seconds_left. The state shows the pairing \
         code while one is shown.",
        &[],
    ),
    asks("stop_pairing", "Closes the pairing window.", &[]),
    Tool {
        name: "forget_headset",
        description: "Forgets a paired headset (it has to pair again); its session ends.",
        args: &[Arg::of(
            "headset",
            "string",
            "its key, its whole key or its name",
        )],
        does: Does::Ask("forget_headset"),
        touch: Touch::Destroys,
    },
    Tool {
        name: "forget_all",
        description: "Forgets every paired headset.",
        args: &[],
        does: Does::Ask("forget_all"),
        touch: Touch::Destroys,
    },
    asks(
        "show_settings",
        "Opens Prostir's settings window on the PC.",
        &[],
    ),
    Tool {
        name: "quit",
        description: "Quits Prostir, as the tray's Quit: nothing waits for the headset afterwards.",
        args: &[],
        does: Does::Ask("quit"),
        touch: Touch::Destroys,
    },
    Tool {
        name: "call",
        description: "Any request of the local API, given whole as LOCAL-API.md writes it, for \
                      what no other tool covers: {\"request\": \"window_look\", \"theme\": \"dark\", \
                      \"palette\": \"ink\"}.",
        args: &[Arg::of(
            "request",
            "object",
            "the request, its name in the field request",
        )],
        does: Does::Call,
        touch: Touch::Destroys,
    },
    Tool {
        name: "screenshot_monitor",
        description: "A picture of a virtual monitor or a window panel, as a PNG image. On KDE \
                      Plasma while the monitor does not stream to a headset, the picture is of \
                      the whole desktop: every screen and every window on it, and whole says \
                      so. The file Prostir saves to the Pictures folder is removed afterwards \
                      unless keep; a picture over 5 MB is not sent, its file is kept and its path \
                      given.",
        args: &[
            ID,
            Arg::of("keep", "boolean", "keep the file (its path is given)").optional(),
        ],
        does: Does::Screenshot,
        touch: Touch::Changes,
    },
    Tool {
        name: "wait_for_state",
        description: "Waits until the value at a JSON Pointer (RFC 6901) in the state holds a \
                      condition: equals (exactly that), contains (an object with these fields, \
                      an array with such an element, a string with this text) or exists. Without \
                      one, waits for any change of the state (Prostir coming back counts). \
                      Example: pointer /displays, contains {\"id\": 103, \"state\": \"running\"}. \
                      Answers the revision and the value; TimedOut with the last value if it did \
                      not hold in time.",
        args: &[
            Arg::of(
                "pointer",
                "string",
                "a JSON Pointer into the state, \"\" (the whole state) by default",
            )
            .optional(),
            Arg::of("equals", "", "the value wanted").optional(),
            Arg::of("contains", "", "what the value has in it").optional(),
            Arg::of("exists", "boolean", "whether there is a value there").optional(),
            Arg::of("timeout", "number", "seconds, 10 by default")
                .within(0, 50)
                .optional(),
        ],
        does: Does::Wait,
        touch: Touch::Reads,
    },
    Tool {
        name: "start",
        description: "Starts Prostir if it is not running, as opening it from the menu does: its \
                      settings window opens too, and keeps it running while open.",
        args: &[],
        does: Does::Start,
        touch: Touch::Changes,
    },
];

/// `prostir mcp`: serves stdin and stdout until stdin ends; its exit code.
pub fn run() -> i32 {
    use crate::platform::local_api::Client;
    let connect: Connect =
        Box::new(|| Client::connect(ctl::WAIT).map(|c| -> Box<dyn CtlClient> { Box::new(c) }));
    let start: Start = Box::new(|| {
        let client = Client::connect(ctl::WAIT).map_err(|e| {
            let (_, name, text) = ctl::what_failed(&e, false);
            (name, text)
        })?;
        if client.running() {
            return Ok("Prostir is running already".to_owned());
        }
        let why = std::cell::RefCell::new(None);
        let wake = || {
            crate::local::wake_for_agent().inspect_err(|e| *why.borrow_mut() = Some(e.to_string()))
        };
        match ctl::open(&client, &wake) {
            ctl::DONE => Ok("Prostir is running".to_owned()),
            ctl::TIMED_OUT => Err((
                ApiError::TimedOut.name(),
                "Prostir did not come on the bus in time (its log says why)".to_owned(),
            )),
            _ => Err((
                ctl::what_failed(&ClientError::NotRunning, false).1,
                why.take()
                    .unwrap_or_else(|| "Prostir did not start (its log says why)".to_owned()),
            )),
        }
    });
    let follow: Follow = Box::new(|changes: Arc<Changes>| {
        let spawned = std::thread::Builder::new()
            .name("mcp-watch".into())
            .spawn(move || {
                let client = match Client::connect(ctl::WAIT) {
                    Ok(client) => client,
                    Err(e) => {
                        let (_, name, text) = ctl::what_failed(&e, false);
                        return log::warn!(
                            "mcp: the state's changes are not followed ({name}: {text}); \
                             waits look at the state once a second"
                        );
                    }
                };
                let watched = client.watch(&mut |seen| {
                    if !matches!(seen, Watched::Event(_)) {
                        changes.bump();
                    }
                    true
                });
                if let Err(e) = watched {
                    let (_, name, text) = ctl::what_failed(&e, false);
                    log::warn!("mcp: the state's changes are followed no more ({name}: {text})");
                }
            });
        if let Err(e) = spawned {
            log::warn!("mcp: the state's changes are not followed: {e}");
        }
    });
    let shots = crate::platform::screenshot_dir();
    let mut server = Server::new(connect, start, Some(follow), shots);
    let out = Arc::new(Out::new(std::io::stdout()));
    let input = Box::new(std::io::BufReader::new(std::io::stdin()));
    match server.serve(input, &out) {
        Ok(()) => ctl::DONE,
        Err(e) => {
            eprintln!("prostir mcp: {e}");
            ctl::FAILED
        }
    }
}

#[cfg(test)]
mod tests;
