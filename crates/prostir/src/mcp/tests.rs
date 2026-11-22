use super::*;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

/// A Prostir that answers as a test says, keeps what it was asked, and shows the
/// states it is given one after the other (the last one stays).
#[derive(Clone)]
struct Fake {
    answer: Result<Answer, ClientError>,
    states: Rc<RefCell<VecDeque<(u32, String)>>>,
    asked: Rc<RefCell<Vec<Request>>>,
}

impl Fake {
    fn new() -> Self {
        Self {
            answer: Ok(Answer {
                revision: 7,
                ..Answer::default()
            }),
            states: Rc::new(RefCell::new(VecDeque::from([(1, state("starting"))]))),
            asked: Rc::default(),
        }
    }
}

impl CtlClient for Fake {
    fn call(&self, request: &Request) -> Result<Answer, ClientError> {
        self.asked.borrow_mut().push(request.clone());
        self.answer.clone()
    }

    fn state(&self) -> Result<(u32, String), ClientError> {
        let mut states = self.states.borrow_mut();
        let state = if states.len() > 1 {
            states.pop_front()
        } else {
            states.front().cloned()
        };
        state.ok_or(ClientError::NotRunning)
    }

    fn running(&self) -> bool {
        !self.states.borrow().is_empty()
    }

    fn watch(&self, _each: &mut dyn FnMut(Watched) -> bool) -> Result<(), ClientError> {
        Ok(())
    }
}

/// A state with monitor 103 standing so.
fn state(display: &str) -> String {
    json!({"v": 1, "primary": 103, "displays": [
        {"id": 7, "kind": "physical", "state": "running"},
        {"id": 103, "kind": "virtual", "name": "Prostir-1", "state": display},
    ]})
    .to_string()
}

/// A folder of this test's own, empty.
fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("prostir-mcp-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn server(fake: &Fake) -> Server {
    let fake = fake.clone();
    let connect: Connect = Box::new(move || Ok(Box::new(fake.clone())));
    let start: Start = Box::new(|| Ok("Prostir is running".to_owned()));
    let mut server = Server::new(connect, start, None, std::env::temp_dir());
    server.poll = Duration::from_millis(5);
    server
}

/// The answer to one request, as JSON.
fn ask(server: &mut Server, method: &str, params: &Value) -> Value {
    let line = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let reply = server.handle_line(&line.to_string()).expect("an answer");
    serde_json::from_str(&reply).unwrap()
}

/// What tool `name` gave with `args`: whether it failed, and its content.
fn tool(server: &mut Server, name: &str, args: &Value) -> (bool, Vec<Value>) {
    let reply = ask(
        server,
        "tools/call",
        &json!({"name": name, "arguments": args}),
    );
    let result = &reply["result"];
    assert!(result.is_object(), "{reply}");
    let failed = result["isError"].as_bool().unwrap_or(false);
    (failed, result["content"].as_array().unwrap().clone())
}

/// The JSON of the first text of a tool's content.
fn said(content: &[Value]) -> Value {
    assert_eq!(content[0]["type"], "text");
    serde_json::from_str(content[0]["text"].as_str().unwrap()).unwrap()
}

/// What `serve` wrote, kept for the test.
#[derive(Clone, Default)]
struct Written(Arc<Mutex<Vec<u8>>>);

impl Write for Written {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Written {
    /// Each line, as JSON (every line is).
    fn lines(&self) -> Vec<Value> {
        String::from_utf8(self.0.lock().unwrap().clone())
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
            .collect()
    }
}

/// Serves `input` to its end: every line written.
fn serve(server: &mut Server, input: Vec<u8>) -> Vec<Value> {
    let written = Written::default();
    let out = Arc::new(Out::new(written.clone()));
    server
        .serve(Box::new(std::io::Cursor::new(input)), &out)
        .unwrap();
    written.lines()
}

/// `initialize` takes the client's version where it is one of ours, else gives the
/// newest; it says there are tools.
#[test]
fn initialize_agrees_on_a_version() {
    let mut server = server(&Fake::new());
    let init = |s: &mut Server, version: &str| {
        ask(
            s,
            "initialize",
            &json!({"protocolVersion": version, "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}),
        )
    };
    let reply = init(&mut server, "2025-06-18");
    assert_eq!(reply["jsonrpc"], "2.0");
    assert_eq!(reply["id"], 1);
    assert_eq!(reply["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(reply["result"]["serverInfo"]["name"], "prostir");
    assert!(reply["result"]["capabilities"]["tools"].is_object());
    let reply = init(&mut server, "2099-01-01");
    assert_eq!(reply["result"]["protocolVersion"], VERSIONS[0]);
    assert_eq!(ask(&mut server, "ping", &json!({}))["result"], json!({}));
}

/// Every tool is listed with a description and a closed schema of its arguments,
/// with the bounds and words they may take.
#[test]
fn every_tool_is_listed() {
    let mut server = server(&Fake::new());
    let reply = ask(&mut server, "tools/list", &json!({}));
    let tools = reply["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), TOOLS.len());
    assert_eq!(tools.len(), 36);
    let mut names = std::collections::BTreeSet::new();
    for t in tools {
        let name = t["name"].as_str().unwrap();
        assert!(names.insert(name.to_owned()), "{name} twice");
        assert!(!t["description"].as_str().unwrap().is_empty(), "{name}");
        let schema = &t["inputSchema"];
        assert_eq!(schema["type"], "object", "{name}");
        assert_eq!(schema["additionalProperties"], false, "{name}");
        for needed in schema["required"].as_array().unwrap() {
            let needed = needed.as_str().unwrap();
            assert!(
                schema["properties"].get(needed).is_some(),
                "{name}: {needed}"
            );
        }
    }
    for wanted in [
        "get_state",
        "list_monitors",
        "add_monitor",
        "remove_monitor",
        "place_monitor",
        "set_scale",
        "show_pc_display",
        "recenter",
        "no_limits",
        "set_quality",
        "set_codec",
        "pair_new_headset",
        "stop_pairing",
        "screenshot_monitor",
        "wait_for_state",
        "start",
    ] {
        assert!(names.contains(wanted), "{wanted}");
    }
    let find = |name: &str| tools.iter().find(|t| t["name"] == name).unwrap();
    assert_eq!(find("quit")["annotations"]["destructiveHint"], true);
    assert_eq!(
        find("screenshot_monitor")["annotations"]["readOnlyHint"],
        false
    );
    let quality = &find("set_quality")["inputSchema"]["properties"]["mbit"];
    assert_eq!(
        (&quality["minimum"], &quality["maximum"]),
        (&json!(5), &json!(100))
    );
    let codec = &find("set_codec")["inputSchema"]["properties"]["codec"];
    assert_eq!(codec["enum"], json!(["h264", "h265", "auto"]));
    let timeout = &find("wait_for_state")["inputSchema"]["properties"]["timeout"];
    assert_eq!(timeout["maximum"], json!(50));
}

/// Each tool that is a request asks for it with the tool's arguments: every request
/// it names is one the local API knows.
#[test]
fn tools_ask_for_their_requests() {
    for t in TOOLS {
        if let Does::Ask(request) = t.does {
            assert!(
                crate::api::REQUESTS.contains(&request),
                "{}: {request}",
                t.name
            );
        }
    }
    let fake = Fake::new();
    let mut server = server(&fake);
    let (failed, content) = tool(&mut server, "add_monitor", &json!({"w": 1920, "h": 1080}));
    assert!(!failed);
    assert_eq!(said(&content), json!({"revision": 7}));
    let (failed, _) = tool(
        &mut server,
        "place_monitor",
        &json!({"id": 103, "x": -10, "y": 0}),
    );
    assert!(!failed);
    tool(
        &mut server,
        "show_pc_display",
        &json!({"id": 7, "shown": false}),
    );
    tool(&mut server, "recenter", &json!({}));
    tool(
        &mut server,
        "call",
        &json!({"request": {"request": "set_sharp", "on": true}}),
    );
    assert_eq!(
        *fake.asked.borrow(),
        [
            Request::AddMonitor {
                w: 1920,
                h: 1080,
                scale: 100
            },
            Request::Place {
                id: 103,
                x: -10,
                y: 0
            },
            Request::ShowPhysical {
                id: 7,
                shown: false
            },
            Request::Recenter,
            Request::SetSharp { on: true },
        ]
    );
}

/// Arguments that do not read (or that a tool does not take), a refusal, and
/// Prostir not running are the tool's errors, named as `ctl --json` names them;
/// nothing is asked for wrong arguments.
#[test]
fn errors_of_tools() {
    let mut fake = Fake::new();
    let mut s = server(&fake);
    let (failed, content) = tool(&mut s, "cinema", &json!({}));
    assert!(failed);
    assert_eq!(said(&content)["error"], "BadArgument");
    let (failed, content) = tool(&mut s, "cinema", &json!({"on": true, "loud": 1}));
    assert!(failed);
    let got = said(&content);
    assert_eq!(got["error"], "BadArgument");
    assert!(got["text"].as_str().unwrap().contains("loud"), "{got}");
    let (failed, content) = tool(&mut s, "call", &json!({"request": {"request": "fly"}}));
    assert!(failed);
    assert_eq!(said(&content)["error"], "Unsupported");
    // Counting needs the person's own yes: an agent can say no or unasked only.
    for asked in [
        tool(&mut s, "set_counting", &json!({"counting": "yes"})),
        tool(
            &mut s,
            "call",
            &json!({"request": {"request": "set_counting", "counting": "yes"}}),
        ),
    ] {
        let (failed, content) = asked;
        assert!(failed);
        let got = said(&content);
        assert_eq!(got["error"], "BadArgument");
        assert!(got["text"].as_str().unwrap().contains("person"), "{got}");
    }
    assert!(fake.asked.borrow().is_empty());
    let (failed, _) = tool(&mut s, "set_counting", &json!({"counting": "no"}));
    assert!(!failed);
    assert_eq!(
        fake.asked.borrow_mut().drain(..).collect::<Vec<_>>(),
        [Request::SetCounting {
            counting: crate::api::Counting::No
        }]
    );

    fake.answer = Err(ClientError::Refused(crate::api::Failure::new(
        ApiError::NotConnected,
        "No headset is connected",
    )));
    let mut s = server(&fake);
    let (failed, content) = tool(&mut s, "recenter", &json!({}));
    assert!(failed);
    assert_eq!(
        said(&content),
        json!({"error": "NotConnected", "text": "No headset is connected"})
    );

    fake.answer = Err(ClientError::NotRunning);
    let mut s = server(&fake);
    let (failed, content) = tool(&mut s, "add_monitor", &json!({"w": 640, "h": 360}));
    assert!(failed);
    assert_eq!(said(&content)["error"], "NotRunning");
}

/// Without a bus a tool says so, and the next one connects again; `start` says its
/// error by its name.
#[test]
fn the_bus_is_connected_when_needed() {
    let fake = Fake::new();
    let tries = Rc::new(Cell::new(0));
    let connect: Connect = {
        let tries = tries.clone();
        Box::new(move || {
            tries.set(tries.get() + 1);
            if tries.get() == 1 {
                Err(ClientError::NoBus("none".into()))
            } else {
                Ok(Box::new(fake.clone()))
            }
        })
    };
    let start: Start = Box::new(|| Err(("TimedOut", "not in time".to_owned())));
    let mut s = Server::new(connect, start, None, std::env::temp_dir());
    assert_eq!(tries.get(), 0, "nothing connects before a tool needs it");
    let (failed, content) = tool(&mut s, "get_state", &json!({}));
    assert!(failed);
    assert_eq!(said(&content)["error"], "NoBus");
    let (failed, _) = tool(&mut s, "get_state", &json!({}));
    assert!(!failed);
    tool(&mut s, "list_monitors", &json!({}));
    assert_eq!(tries.get(), 2, "connected once, kept");
    let (failed, content) = tool(&mut s, "start", &json!({}));
    assert!(failed);
    assert_eq!(said(&content)["error"], "TimedOut");
}

/// What is not MCP gets JSON-RPC's errors; notifications and responses get nothing;
/// a batch an answer for each of its requests.
#[test]
fn the_protocol_around_the_tools() {
    let mut s = server(&Fake::new());
    let code = |reply: Option<String>| -> i64 {
        let reply: Value = serde_json::from_str(&reply.expect("an answer")).unwrap();
        reply["error"]["code"].as_i64().unwrap()
    };
    assert_eq!(code(s.handle_line("{not json")), PARSE_ERROR);
    assert_eq!(code(s.handle_line("[]")), INVALID_REQUEST);
    assert_eq!(code(s.handle_line("7")), INVALID_REQUEST);
    assert_eq!(
        code(s.handle_line(r#"{"id":1,"method":"ping"}"#)),
        INVALID_REQUEST
    );
    assert_eq!(
        code(s.handle_line(r#"{"jsonrpc":"2.0","id":2,"method":"resources/list"}"#)),
        METHOD_NOT_FOUND
    );
    assert_eq!(
        code(s.handle_line(
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"fly"}}"#
        )),
        INVALID_PARAMS
    );
    assert_eq!(
        s.handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#),
        None
    );
    assert_eq!(
        s.handle_line(r#"{"jsonrpc":"2.0","id":9,"result":{}}"#),
        None
    );
    let batch = s
        .handle_line(
            r#"[{"jsonrpc":"2.0","id":"a","method":"ping"},
                {"jsonrpc":"2.0","method":"notifications/initialized"},
                {"jsonrpc":"2.0","id":"b","method":"ping"}]"#,
        )
        .unwrap();
    let batch: Value = serde_json::from_str(&batch).unwrap();
    let ids: Vec<&Value> = batch.as_array().unwrap().iter().map(|r| &r["id"]).collect();
    assert_eq!(ids, [&json!("a"), &json!("b")]);
}

/// `serve` answers a line for each request and nothing else on stdout; a line that
/// is not even UTF-8 gets a parse error, and serving goes on.
#[test]
fn serve_answers_line_by_line() {
    let mut s = server(&Fake::new());
    let mut input = Vec::new();
    input.extend_from_slice(
        br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}"#,
    );
    input.extend_from_slice(b"\n\xff\xfe not text\n");
    input.extend_from_slice(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    input.extend_from_slice(b"\n\n");
    input.extend_from_slice(
        br#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"get_state"}}"#,
    );
    input.push(b'\n');
    let lines = serve(&mut s, input);
    assert_eq!(lines.len(), 3, "{lines:?}");
    let by_id = |id: Value| lines.iter().find(|l| l["id"] == id).cloned().unwrap();
    assert_eq!(by_id(json!(1))["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(by_id(Value::Null)["error"]["code"], PARSE_ERROR);
    let content = by_id(json!(2))["result"]["content"]
        .as_array()
        .unwrap()
        .clone();
    let got = said(&content);
    assert_eq!(got["revision"], 1);
    assert_eq!(got["state"]["primary"], 103);
}

/// A `ping` is answered while a wait runs, and a cancelled wait ends soon, without
/// an answer; what comes after it is answered.
#[test]
fn a_wait_is_cancelled() {
    let mut s = server(&Fake::new());
    let lines = [
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
            "name": "wait_for_state",
            "arguments": {"pointer": "/primary", "equals": 1, "timeout": 30}}}),
        json!({"jsonrpc": "2.0", "id": 4, "method": "ping"}),
        json!({"jsonrpc": "2.0", "method": "notifications/cancelled",
               "params": {"requestId": 3, "reason": "the test"}}),
        json!({"jsonrpc": "2.0", "id": 5, "method": "tools/list"}),
    ];
    let input: String = lines.iter().map(|l| l.to_string() + "\n").collect();
    let began = Instant::now();
    let written = serve(&mut s, input.into_bytes());
    assert!(began.elapsed() < Duration::from_secs(5), "the wait ended");
    let ids: Vec<&Value> = written.iter().map(|l| &l["id"]).collect();
    assert!(
        !ids.contains(&&json!(3)),
        "a cancelled request is not answered"
    );
    assert!(
        ids.contains(&&json!(4)) && ids.contains(&&json!(5)),
        "{ids:?}"
    );
}

/// A wait the client follows says how long it has waited, about once a second.
#[test]
fn a_wait_tells_its_progress() {
    let mut s = server(&Fake::new());
    let call = json!({"jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": {
        "name": "wait_for_state",
        "arguments": {"pointer": "/primary", "equals": 1, "timeout": 1.5},
        "_meta": {"progressToken": "w"}}});
    let written = serve(&mut s, format!("{call}\n").into_bytes());
    let progress: Vec<&Value> = written
        .iter()
        .filter(|l| l["method"] == "notifications/progress")
        .collect();
    assert!(!progress.is_empty(), "{written:?}");
    assert_eq!(progress[0]["params"]["progressToken"], "w");
    assert_eq!(progress[0]["params"]["total"], 1.5);
    let done = written.iter().find(|l| l["id"] == 6).unwrap();
    assert_eq!(done["result"]["isError"], true);
    assert_eq!(
        said(done["result"]["content"].as_array().unwrap())["error"],
        "TimedOut"
    );
}

/// The conditions `wait_for_state` reads, and when each holds.
#[test]
fn conditions() {
    let read = |v: Value| Condition::read(v.as_object().unwrap());
    let c =
        read(json!({"pointer": "/displays", "contains": {"id": 103, "state": "running"}})).unwrap();
    let displays: Value =
        serde_json::from_str::<Value>(&state("running")).unwrap()["displays"].clone();
    assert!(c.holds(Some(&displays), false));
    let starting: Value =
        serde_json::from_str::<Value>(&state("starting")).unwrap()["displays"].clone();
    assert!(!c.holds(Some(&starting), true));
    assert!(!c.holds(None, true));
    let equals = read(json!({"pointer": "/primary", "equals": 103})).unwrap();
    assert!(equals.holds(Some(&json!(103)), false));
    assert!(!equals.holds(Some(&json!(104)), false));
    let gone = read(json!({"pointer": "/pairing", "exists": false})).unwrap();
    assert!(gone.holds(None, false));
    assert!(!gone.holds(Some(&json!({})), false));
    let any = read(json!({})).unwrap();
    assert_eq!(any.test, Test::Changed);
    assert!(!any.holds(None, false) && any.holds(None, true));
    assert_eq!(read(json!({"exists": null})).unwrap().test, Test::Changed);
    assert!(read(json!({"pointer": "displays"})).is_err());
    assert!(read(json!({"equals": 1, "exists": true})).is_err());
    assert!(read(json!({"exists": "yes"})).is_err());
    assert!(contains(
        &json!("The desktop does not respond"),
        &json!("respond")
    ));
    assert!(contains(&json!([[1, 2], [3]]), &json!([3])));
    assert!(!contains(&json!({"a": 1}), &json!({"a": 1, "b": 2})));
    // A monitor placed on the desktop, as the tools' descriptions say to wait for it.
    let placed = json!({"desk": {}});
    assert!(!contains(&json!([{"id": 1, "desk": null}]), &placed));
    assert!(contains(&json!([{"id": 1, "desk": {"x": 0}}]), &placed));
}

/// `wait_for_state` looks until the condition holds, answering the value; in time
/// or not, and not at all while Prostir is not running.
#[test]
fn waiting_for_the_state() {
    let fake = Fake::new();
    fake.states
        .borrow_mut()
        .extend([(2, state("starting")), (3, state("running"))]);
    let mut s = server(&fake);
    let running = json!({"pointer": "/displays/1/state", "equals": "running", "timeout": 5});
    let (failed, content) = tool(&mut s, "wait_for_state", &running);
    assert!(!failed, "{content:?}");
    let got = said(&content);
    assert_eq!(
        (got["revision"].as_u64(), &got["value"]),
        (Some(3), &json!("running"))
    );

    let failed_state = json!({"pointer": "/displays/1/state", "equals": "failed", "timeout": 0.05});
    let (failed, content) = tool(&mut s, "wait_for_state", &failed_state);
    assert!(failed);
    let got = said(&content);
    assert_eq!(got["error"], "TimedOut");
    assert_eq!(got["value"], "running");

    for timeout in [51, -1] {
        let (failed, content) = tool(&mut s, "wait_for_state", &json!({"timeout": timeout}));
        assert!(failed);
        assert_eq!(said(&content)["error"], "BadArgument");
    }

    fake.states.borrow_mut().clear();
    let (failed, content) = tool(&mut s, "wait_for_state", &running);
    assert!(failed);
    assert_eq!(said(&content)["error"], "NotRunning");
}

/// Prostir going away and coming back during a wait for any change is a change,
/// even with the same revision (it counts from 1 again).
#[test]
fn a_restart_is_a_change() {
    /// Running, then gone once, then running again with the same revision.
    struct Restarting(Cell<u32>);
    impl CtlClient for Restarting {
        fn call(&self, _: &Request) -> Result<Answer, ClientError> {
            Err(ClientError::NotRunning)
        }
        fn state(&self) -> Result<(u32, String), ClientError> {
            self.0.set(self.0.get() + 1);
            match self.0.get() {
                2 => Err(ClientError::NotRunning),
                _ => Ok((1, state("running"))),
            }
        }
        fn running(&self) -> bool {
            true
        }
        fn watch(&self, _: &mut dyn FnMut(Watched) -> bool) -> Result<(), ClientError> {
            Ok(())
        }
    }
    let connect: Connect = Box::new(|| Ok(Box::new(Restarting(Cell::new(0)))));
    let start: Start = Box::new(|| Ok(String::new()));
    let mut s = Server::new(connect, start, None, std::env::temp_dir());
    s.poll = Duration::from_millis(5);
    let (failed, content) = tool(&mut s, "wait_for_state", &json!({"timeout": 2}));
    assert!(!failed, "{content:?}");
    assert_eq!(said(&content)["revision"], 1);
}

/// A screenshot comes as a PNG image read from the file Prostir saved, which goes
/// unless kept; a file that is not a picture Prostir saved there is left as it is,
/// and one too large to send is kept and named.
#[test]
fn a_screenshot_is_an_image() {
    let shots = folder("shots");
    let path = shots.join("1.png");
    let png = b"\x89PNG\r\n\x1a\nnot really";
    let mut fake = Fake::new();
    let answer = |path: &Path| {
        Ok(Answer {
            path: Some(path.to_string_lossy().into_owned()),
            whole: Some(true),
            revision: 8,
            ..Answer::default()
        })
    };
    fake.answer = answer(&path);
    let mut s = server(&fake);
    s.shots.clone_from(&shots);
    for keep in [true, false] {
        std::fs::write(&path, png).unwrap();
        let (failed, content) = tool(
            &mut s,
            "screenshot_monitor",
            &json!({"id": 103, "keep": keep}),
        );
        assert!(!failed, "{content:?}");
        let got = said(&content);
        assert_eq!(got["whole"], true);
        assert_eq!(got["bytes"], png.len());
        assert_eq!(got["path"].is_string(), keep);
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["mimeType"], "image/png");
        assert_eq!(content[1]["data"], base64(png));
        assert_eq!(path.exists(), keep);
    }
    assert_eq!(fake.asked.borrow()[0], Request::Screenshot { id: 103 });
    let (failed, content) = tool(&mut s, "screenshot_monitor", &json!({"id": 103}));
    assert!(failed, "no file any more");
    assert_eq!(said(&content)["error"], "Capture");
    let (failed, content) = tool(&mut s, "screenshot_monitor", &json!({"id": "x"}));
    assert!(failed);
    assert_eq!(said(&content)["error"], "BadArgument");

    // Not one Prostir saved: elsewhere, a link, not a PNG, not .png.
    let elsewhere = folder("elsewhere").join("1.png");
    std::fs::write(&elsewhere, png).unwrap();
    let link = shots.join("link.png");
    std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
    let text = shots.join("text.png");
    std::fs::write(&text, b"hello").unwrap();
    let named = shots.join("1.txt");
    std::fs::write(&named, png).unwrap();
    for wrong in [&elsewhere, &link, &text, &named] {
        fake.answer = answer(wrong);
        let mut s = server(&fake);
        s.shots.clone_from(&shots);
        let (failed, content) = tool(&mut s, "screenshot_monitor", &json!({"id": 103}));
        assert!(failed, "{wrong:?}");
        assert_eq!(said(&content)["error"], "Capture", "{wrong:?}");
        assert!(wrong.exists(), "{wrong:?} is left");
    }

    let large = shots.join("large.png");
    let mut big = png.to_vec();
    big.resize(IMAGE_MAX + 1, 0);
    std::fs::write(&large, &big).unwrap();
    fake.answer = answer(&large);
    let mut s = server(&fake);
    s.shots.clone_from(&shots);
    let (failed, content) = tool(&mut s, "screenshot_monitor", &json!({"id": 103}));
    assert!(!failed);
    assert_eq!(content.len(), 1, "no image");
    assert_eq!(said(&content)["path"], large.to_string_lossy().as_ref());
    assert!(large.exists(), "kept");
    let _ = std::fs::remove_dir_all(&shots);
    let _ = std::fs::remove_dir_all(elsewhere.parent().unwrap());
}

#[test]
fn base64_as_the_rfc_has_it() {
    for (bytes, encoded) in [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg=="),
        ("fooba", "Zm9vYmE="),
        ("foobar", "Zm9vYmFy"),
    ] {
        assert_eq!(base64(bytes.as_bytes()), encoded, "{bytes}");
    }
    assert_eq!(base64(&[0xff, 0xfe, 0xfd]), "//79");
}
