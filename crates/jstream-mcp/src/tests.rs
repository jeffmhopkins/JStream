//! Round trips through [`Server`] against the in-memory `MockEngine`: the handshake-based
//! and stateless eras, the tool list snapshot, error mapping and malformed messages.

use crate::server::Server;
use crate::stdio::block_on;
use crate::{LEGACY_VERSIONS, MODERN_VERSION, serve_stdio};
use jstream_control::{BoxFuture, Control, ControlError, EventStream, MockControl, MockEngine};
use jstream_protocol::{Call, Hello, OutputId, Response, Welcome, json_schema};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// A mock session that records every `hello` and can be made to lose its connection.
struct Recording {
    inner: MockControl,
    hellos: Mutex<Vec<Hello>>,
    lost: AtomicBool,
}

impl Control for Recording {
    fn hello(&self, hello: Hello) -> BoxFuture<'_, Result<Welcome, ControlError>> {
        self.hellos
            .lock()
            .expect("not poisoned")
            .push(hello.clone());
        self.inner.hello(hello)
    }

    fn call(&self, call: Call) -> BoxFuture<'_, Result<Response, ControlError>> {
        if self.lost.load(Ordering::Relaxed) {
            return Box::pin(std::future::ready(Err(ControlError::Disconnected {
                reason: "socket closed".into(),
            })));
        }
        self.inner.call(call)
    }

    fn events(&self) -> EventStream {
        self.inner.events()
    }
}

struct Fixture {
    engine: MockEngine,
    control: Arc<Recording>,
    server: Server,
}

fn fixture() -> Fixture {
    let engine = MockEngine::new().with_output(OutputId::new("out_rec"));
    let control = Arc::new(Recording {
        inner: engine.connect(),
        hellos: Mutex::new(Vec::new()),
        lost: AtomicBool::new(false),
    });
    let server = Server::new(Arc::clone(&control) as Arc<dyn Control>);
    Fixture {
        engine,
        control,
        server,
    }
}

impl Fixture {
    /// Sends one message, returns the parsed reply.
    fn send(&self, message: &Value) -> Option<Value> {
        self.send_text(&message.to_string())
    }

    fn send_text(&self, text: &str) -> Option<Value> {
        block_on(self.server.handle(text)).map(|reply| {
            assert!(!reply.contains('\n'), "a reply must be one line: {reply}");
            serde_json::from_str(&reply).expect("replies are JSON")
        })
    }

    /// A request on the handshake-based revision; returns its `result`.
    fn legacy(&self, id: u64, method: &str, params: Value) -> Value {
        let reply = self
            .send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .expect("requests are answered");
        assert_eq!(reply["id"], id);
        assert!(reply.get("error").is_none(), "{method} failed: {reply}");
        reply["result"].clone()
    }

    fn initialize(&self) -> Value {
        self.legacy(
            0,
            "initialize",
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "agent-x", "version": "1.0"}
            }),
        )
    }

    /// A `tools/call` on the handshake-based revision; returns its `CallToolResult`.
    fn call_tool(&self, name: &str, arguments: Value) -> Value {
        self.legacy(
            9,
            "tools/call",
            json!({"name": name, "arguments": arguments}),
        )
    }

    /// A stateless (2026-07-28) request; returns the whole reply.
    fn modern(&self, method: &str, mut params: Value) -> Value {
        params["_meta"] = json!({
            "io.modelcontextprotocol/protocolVersion": MODERN_VERSION,
            "io.modelcontextprotocol/clientInfo": {"name": "modern-agent", "version": "2.0"},
            "io.modelcontextprotocol/clientCapabilities": {}
        });
        self.send(&json!({"jsonrpc": "2.0", "id": "m1", "method": method, "params": params}))
            .expect("requests are answered")
    }

    fn hello_names(&self) -> Vec<String> {
        let hellos = self.control.hellos.lock().expect("not poisoned");
        hellos.iter().map(|h| h.client_name.clone()).collect()
    }
}

/// The structured result of a tool call that succeeded.
fn ok(result: &Value) -> &Value {
    assert_eq!(result["isError"], false, "{result}");
    let text = result["content"][0]["text"].as_str().expect("a text block");
    let parsed: Value = serde_json::from_str(text).expect("the text is the JSON");
    assert_eq!(parsed, result["structuredContent"]);
    &result["structuredContent"]
}

/// The error object of a tool call that failed.
fn tool_error(result: &Value) -> &Value {
    assert_eq!(result["isError"], true, "{result}");
    assert_eq!(result["content"][0]["type"], "text");
    &result["structuredContent"]["error"]
}

fn rpc_error(reply: &Value) -> (i64, String) {
    assert!(reply.get("result").is_none(), "{reply}");
    (
        reply["error"]["code"].as_i64().expect("an integer code"),
        reply["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
    )
}

#[test]
fn handshake_list_and_call_round_trip() {
    let f = fixture();
    assert_eq!(f.legacy(1, "ping", json!({})), json!({}));
    let init = f.initialize();
    assert_eq!(init["protocolVersion"], "2025-11-25");
    assert_eq!(init["capabilities"], json!({"tools": {}}));
    assert_eq!(init["serverInfo"]["name"], "jstream-mcp");
    assert!(
        init["instructions"]
            .as_str()
            .is_some_and(|s| s.contains("`scene`"))
    );
    let initialized = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    assert_eq!(f.send(&initialized), None);
    assert_eq!(f.legacy(1, "ping", json!({})), json!({}));
    // `hello` waits for the first call that needs the engine.
    assert!(f.hello_names().is_empty());

    let list = f.legacy(2, "tools/list", json!({}));
    let names: Vec<&str> = list["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert_eq!(
        names,
        [
            "batch",
            "describe_api",
            "item",
            "output",
            "scene",
            "source",
            "state"
        ]
    );
    assert!(list.get("ttlMs").is_none() && list.get("resultType").is_none());

    let created = f.call_tool(
        "scene",
        json!({"op": {"type": "scene.create", "scene_id": "scn_1", "name": "Main"}}),
    );
    assert_eq!(
        ok(&created),
        &json!({"id": "req_1", "result": {"kind": "batch", "rev": 1}})
    );
    assert_eq!(f.engine.rev().0, 1);
    let listed = f.call_tool("scene", json!({"op": {"type": "scene.list"}}));
    assert_eq!(
        ok(&listed)["result"]["result"],
        json!({"type": "scene.list", "rev": 1,
               "scenes": [{"scene_id": "scn_1", "name": "Main", "item_count": 0}]})
    );
    let batch = f.call_tool(
        "batch",
        json!({"edits": [
            {"type": "source.create", "source_id": "src_cam", "name": "Camera",
             "kind": {"type": "builtin", "kind": "webcam"}},
            {"type": "item.add", "scene_id": "scn_1", "item_id": "itm_1", "source_id": "src_cam"}
        ], "expected_rev": 1, "idempotency_key": "k1"}),
    );
    assert_eq!(ok(&batch)["result"], json!({"kind": "batch", "rev": 2}));
    // The protocol 0.3 reads, each under its noun's tool.
    let cam = json!({"source_id": "src_cam", "name": "Camera",
                     "kind": {"type": "builtin", "kind": "webcam"}});
    let item = json!({"item_id": "itm_1", "source_id": "src_cam",
                      "transform": {"position": {"x": 0.0, "y": 0.0},
                                    "scale": {"x": 1.0, "y": 1.0}, "rotation_deg": 0.0},
                      "crop": {"left": 0, "top": 0, "right": 0, "bottom": 0},
                      "visible": true});
    let sources = f.call_tool("source", json!({"op": {"type": "source.list"}}));
    assert_eq!(
        ok(&sources)["result"]["result"],
        json!({"type": "source.list", "rev": 2, "sources": [cam]})
    );
    let items = f.call_tool(
        "item",
        json!({"op": {"type": "item.list", "scene_id": "scn_1"}}),
    );
    assert_eq!(
        ok(&items)["result"]["result"],
        json!({"type": "item.list", "rev": 2, "scene_id": "scn_1", "items": [item]})
    );
    let state = f.call_tool("state", json!({"op": {"type": "state.get"}}));
    assert_eq!(
        ok(&state)["result"]["result"],
        json!({"type": "state.get", "rev": 2, "sources": [cam],
               "scenes": [{"scene_id": "scn_1", "name": "Main", "items": [item]}]})
    );
    let started = f.call_tool(
        "output",
        json!({"op": {"type": "output.start", "output_id": "out_rec"}}),
    );
    assert_eq!(ok(&started)["result"], json!({"kind": "action"}));
    // One session, opened once, under the MCP client's name.
    assert_eq!(f.hello_names(), ["agent-x"]);
}

#[test]
fn handshake_negotiates_a_served_revision() {
    let f = fixture();
    for (requested, answered) in [
        ("2025-06-18", "2025-06-18"),
        ("2025-11-25", "2025-11-25"),
        ("2024-11-05", LEGACY_VERSIONS[0]),
        (MODERN_VERSION, LEGACY_VERSIONS[0]),
    ] {
        let init = f.legacy(0, "initialize", json!({"protocolVersion": requested}));
        assert_eq!(init["protocolVersion"], answered, "{requested}");
    }
    let reply = f
        .send(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}))
        .expect("answered");
    assert_eq!(rpc_error(&reply).0, -32602);
}

#[test]
fn stateless_requests_round_trip() {
    let f = fixture();
    let discovered = f.modern("server/discover", json!({}));
    let result = &discovered["result"];
    assert_eq!(result["resultType"], "complete");
    assert_eq!(
        result["supportedVersions"],
        json!([MODERN_VERSION, "2025-11-25", "2025-06-18"])
    );
    assert_eq!(result["capabilities"], json!({"tools": {}}));
    assert_eq!(
        result["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "jstream-mcp"
    );
    assert_eq!(result["cacheScope"], "public");
    assert!(result["ttlMs"].as_u64().is_some());

    let listed = f.modern("tools/list", json!({}));
    assert_eq!(listed["result"]["resultType"], "complete");
    assert_eq!(listed["result"]["tools"].as_array().map(Vec::len), Some(7));
    assert!(listed["result"]["ttlMs"].as_u64().is_some());

    let called = f.modern(
        "tools/call",
        json!({"name": "scene", "arguments": {"op": {"type": "scene.create", "scene_id": "s", "name": "A"}}}),
    );
    assert_eq!(called["id"], "m1");
    assert_eq!(called["result"]["resultType"], "complete");
    assert_eq!(ok(&called["result"])["result"]["rev"], 1);
    assert_eq!(f.hello_names(), ["modern-agent"]);
}

#[test]
fn stateless_requests_are_validated() {
    let f = fixture();
    let meta = |version: &str| {
        json!({"io.modelcontextprotocol/protocolVersion": version,
               "io.modelcontextprotocol/clientCapabilities": {}})
    };
    let request = |params: Value| {
        f.send(&json!({"jsonrpc": "2.0", "id": 4, "method": "tools/list", "params": params}))
            .expect("answered")
    };

    let reply = request(json!({"_meta": meta("1900-01-01")}));
    assert_eq!(rpc_error(&reply).0, -32022);
    assert_eq!(
        reply["error"]["data"],
        json!({"supported": [MODERN_VERSION, "2025-11-25", "2025-06-18"], "requested": "1900-01-01"})
    );
    // A handshake-based version without the handshake.
    assert_eq!(
        rpc_error(&request(json!({"_meta": meta("2025-11-25")}))).0,
        -32022
    );
    // Neither `_meta` nor `initialize`.
    let (code, message) = rpc_error(&request(json!({})));
    assert_eq!(code, -32602);
    assert!(message.contains("initialize"), "{message}");
    // `clientCapabilities` is required.
    let missing = json!({"_meta": {"io.modelcontextprotocol/protocolVersion": MODERN_VERSION}});
    assert_eq!(rpc_error(&request(missing)).0, -32602);
    // `ping` is gone in 2026-07-28; unknown methods are -32601.
    let reply = f
        .send(&json!({"jsonrpc": "2.0", "id": 5, "method": "ping",
                      "params": {"_meta": meta(MODERN_VERSION)}}))
        .expect("answered");
    assert_eq!(rpc_error(&reply).0, -32601);
    // A single-page list has no valid cursor.
    let reply = request(json!({"_meta": meta(MODERN_VERSION), "cursor": "x"}));
    assert_eq!(rpc_error(&reply).0, -32602);
}

/// The tool list as agents see it. Any change to the protocol schema or to the
/// generation shows up as a diff of `crates/jstream-mcp/schema/tools.snap`. Regenerate it
/// with `INSTA_UPDATE=always cargo test -p jstream-mcp` and review the diff.
#[test]
fn tool_list_snapshot() {
    let f = fixture();
    f.initialize();
    let list = f.legacy(1, "tools/list", json!({}));
    let text = serde_json::to_string_pretty(&list).expect("serializes");
    insta::with_settings!({
        snapshot_path => "../schema",
        prepend_module_to_snapshot => false,
        omit_expression => true,
    }, {
        insta::assert_snapshot!("tools", text);
    });
}

#[test]
fn engine_errors_are_tool_errors_with_their_code() {
    let f = fixture();
    f.initialize();
    let conflict = f.call_tool(
        "scene",
        json!({"op": {"type": "scene.create", "scene_id": "s", "name": "A"}, "expected_rev": 5}),
    );
    assert_eq!(
        tool_error(&conflict),
        &json!({"code": "rev_conflict", "expected": 5, "current": 0,
                "message": "expected rev 5, but the state is at rev 0"})
    );
    let missing = f.call_tool(
        "item",
        json!({"op": {"type": "item.remove", "item_id": "nope"}}),
    );
    let error = tool_error(&missing);
    assert_eq!(error["code"], "not_found");
    assert_eq!(error["entity"], "item");
    let empty = f.call_tool("batch", json!({"edits": []}));
    assert_eq!(tool_error(&empty)["code"], "empty_batch");
    let unknown_output = f.call_tool(
        "output",
        json!({"op": {"type": "output.status", "output_id": "out_none"}}),
    );
    assert_eq!(tool_error(&unknown_output)["code"], "not_found");
    assert_eq!(f.engine.rev().0, 0);
}

#[test]
fn bad_arguments_are_malformed_tool_errors() {
    let f = fixture();
    f.initialize();
    let result = f.call_tool(
        "scene",
        json!({"op": {"type": "scene.rename", "scene_id": "s"}}),
    );
    let error = tool_error(&result);
    assert_eq!(error["code"], "malformed");
    assert!(
        error["reason"]
            .as_str()
            .is_some_and(|r| r.contains("missing field `name`")),
        "{error}"
    );
    // Arguments are optional in `CallToolRequest`; `scene` needs `op`.
    let reply = f.legacy(3, "tools/call", json!({"name": "scene"}));
    assert_eq!(tool_error(&reply)["code"], "malformed");
    // Nothing reached the engine.
    assert!(f.hello_names().is_empty());
}

#[test]
fn bad_tool_calls_are_jsonrpc_errors() {
    let f = fixture();
    f.initialize();
    let call = |params: Value| {
        f.send(&json!({"jsonrpc": "2.0", "id": 8, "method": "tools/call", "params": params}))
            .expect("answered")
    };
    let (code, message) = rpc_error(&call(json!({"name": "scene_create", "arguments": {}})));
    assert_eq!(
        (code, message.as_str()),
        (-32602, "Unknown tool: scene_create")
    );
    assert_eq!(rpc_error(&call(json!({"arguments": {}}))).0, -32602);
    assert_eq!(rpc_error(&call(json!({"name": 3}))).0, -32602);
    assert_eq!(
        rpc_error(&call(json!({"name": "scene", "arguments": [1]}))).0,
        -32602
    );
    let reply = f
        .send(&json!({"jsonrpc": "2.0", "id": 9, "method": "resources/list"}))
        .expect("answered");
    assert_eq!(rpc_error(&reply).0, -32601);
}

#[test]
fn a_lost_connection_is_a_tool_error_and_reopens_the_session() {
    let f = fixture();
    f.initialize();
    let op = json!({"op": {"type": "scene.list"}});
    ok(&f.call_tool("scene", op.clone()));
    f.control.lost.store(true, Ordering::Relaxed);
    let result = f.call_tool("scene", op.clone());
    let error = tool_error(&result);
    assert_eq!(error["code"], "disconnected");
    assert!(
        error["message"]
            .as_str()
            .is_some_and(|m| m.contains("socket closed") && m.contains("unknown")),
        "{error}"
    );
    f.control.lost.store(false, Ordering::Relaxed);
    ok(&f.call_tool("scene", op));
    assert_eq!(f.hello_names(), ["agent-x", "agent-x"]);
}

#[test]
fn describe_api_returns_the_protocol_schema() {
    let f = fixture();
    f.initialize();
    let result = f.call_tool("describe_api", json!({}));
    assert_eq!(ok(&result), &json_schema());
    assert!(f.hello_names().is_empty());
}

#[test]
fn malformed_messages_are_answered_or_dropped() {
    let f = fixture();
    let reply = f.send_text("{\"jsonrpc\": \"2.0\", ").expect("answered");
    assert_eq!(rpc_error(&reply).0, -32700);
    assert_eq!(reply["id"], Value::Null);
    let reply = f.send_text("[]").expect("answered");
    assert_eq!(rpc_error(&reply).0, -32600);
    // Notifications and stray responses get no reply, even unknown or invalid ones.
    assert_eq!(f.send_text(r#"{"jsonrpc":"2.0","method":"nope"}"#), None);
    assert_eq!(f.send_text(r#"{"jsonrpc":"2.0","id":3,"result":{}}"#), None);
    let reply = f
        .send_text(r#"{"jsonrpc":"2.0","id":"q","method":"tools/list","params":7}"#)
        .expect("answered");
    assert_eq!(
        (rpc_error(&reply).0, reply["id"].clone()),
        (-32602, json!("q"))
    );
}

#[test]
fn stdio_serves_one_reply_per_request_line() {
    let f = fixture();
    let mut input = Vec::new();
    input.extend_from_slice(
        br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}"#,
    );
    input.extend_from_slice(b"\n\n   \n");
    input.extend_from_slice(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    input.extend_from_slice(b"\n\xff\xfe\n");
    input.extend_from_slice(br#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"scene","arguments":{"op":{"type":"scene.list"}}}}"#);
    // The last line may end without a newline.
    let mut output = Vec::new();
    serve_stdio(&f.server, input.as_slice(), &mut output).expect("serves");
    let text = String::from_utf8(output).expect("UTF-8");
    let replies: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).expect("each line is JSON"))
        .collect();
    assert!(text.ends_with('\n'));
    assert_eq!(replies.len(), 3);
    assert_eq!(replies[0]["id"], 1);
    assert_eq!(rpc_error(&replies[1]).0, -32700);
    assert_eq!(replies[2]["id"], 2);
    assert_eq!(ok(&replies[2]["result"])["result"]["kind"], "query");
}

#[test]
fn stdio_reports_write_failures() {
    struct Broken;
    impl std::io::Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("pipe closed"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let f = fixture();
    let input = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}"#;
    let error = serve_stdio(&f.server, &input[..], Broken).expect_err("fails");
    assert!(error.to_string().contains("pipe closed"), "{error}");
}

#[test]
fn handle_is_send_so_any_executor_can_run_it() {
    fn assert_send<T: Send>(_: &T) {}
    let f = fixture();
    let future = f.server.handle("{}");
    assert_send(&future);
    block_on(future);
}
