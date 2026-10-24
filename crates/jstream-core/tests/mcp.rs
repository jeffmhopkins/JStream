//! An MCP agent drives the real engine: `jstream-mcp`'s server over an
//! `InProcessControl` session, the way a host app wires them (R-ARCH-003: the client
//! sees only `dyn Control`). Hermetic: no sockets, no runtime.

use jstream_control::Control;
use jstream_control::protocol::{Event, OutputId, OutputState, Rev};
use jstream_core::Engine;
use jstream_mcp::Server;
use serde_json::{Value, json};
use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

/// Runs a future that never waits: the MCP server only waits on the engine's futures
/// (the hello wait's timer never starts when `hello` is ready at once), which are ready at
/// once for every call this test makes (only `video.snapshot`
/// waits, on the engine's snapshot thread).
fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("nothing on this path waits"),
    }
}

fn request(server: &Server, id: u64, method: &str, params: Value) -> Value {
    let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    let reply = block_on(server.handle(&message.to_string())).expect("requests are answered");
    let reply: Value = serde_json::from_str(&reply).expect("replies are JSON");
    assert_eq!(reply["id"], id, "{reply}");
    assert!(reply.get("error").is_none(), "{method} failed: {reply}");
    reply["result"].clone()
}

fn call_tool(server: &Server, name: &str, arguments: Value) -> Value {
    request(
        server,
        7,
        "tools/call",
        json!({"name": name, "arguments": arguments}),
    )
}

/// The structured content of a tool call, and whether it was an error.
fn content(result: &Value) -> (bool, &Value) {
    (
        result["isError"].as_bool().expect("isError"),
        &result["structuredContent"],
    )
}

#[test]
fn an_mcp_agent_drives_the_engine_in_process() {
    let engine = Engine::new().with_output(OutputId::new("out_rec"));
    let session = Arc::new(engine.connect());
    let mut events = session.events();
    let server = Server::new(Arc::clone(&session) as Arc<dyn Control>);

    let init = request(
        &server,
        0,
        "initialize",
        json!({"protocolVersion": "2025-11-25", "capabilities": {},
               "clientInfo": {"name": "agent-x", "version": "1.0"}}),
    );
    assert_eq!(init["protocolVersion"], "2025-11-25");

    let created = call_tool(
        &server,
        "batch",
        json!({"edits": [
            {"type": "scene.create", "scene_id": "scn_1", "name": "Main"},
            {"type": "source.create", "source_id": "src_cam", "name": "Camera",
             "kind": {"type": "builtin", "kind": "webcam"}},
            {"type": "item.add", "scene_id": "scn_1", "item_id": "itm_1", "source_id": "src_cam"}
        ], "expected_rev": 0, "idempotency_key": "k1"}),
    );
    let (failed, body) = content(&created);
    assert!(!failed, "{created}");
    assert_eq!(body["result"], json!({"kind": "batch", "rev": 1}));
    assert_eq!(engine.rev(), Rev(1));

    // A stale `expected_rev` reaches the agent as the engine's `rev_conflict`.
    let stale = call_tool(
        &server,
        "scene",
        json!({"op": {"type": "scene.create", "scene_id": "scn_2", "name": "Other"},
               "expected_rev": 0}),
    );
    let (failed, body) = content(&stale);
    assert!(failed, "{stale}");
    assert_eq!(body["error"]["code"], "rev_conflict", "{stale}");
    assert_eq!(engine.rev(), Rev(1));

    let listed = call_tool(
        &server,
        "item",
        json!({"op": {"type": "item.list", "scene_id": "scn_1"}}),
    );
    let (failed, body) = content(&listed);
    assert!(!failed, "{listed}");
    assert_eq!(body["result"]["result"]["rev"], 1);
    assert_eq!(body["result"]["result"]["items"][0]["item_id"], "itm_1");

    // The first scene went on program; switching to it again changes nothing.
    let studio = call_tool(&server, "studio", json!({"op": {"type": "studio.get"}}));
    let (failed, body) = content(&studio);
    assert!(!failed, "{studio}");
    assert_eq!(
        body["result"]["result"],
        json!({"type": "studio.get", "program_scene": "scn_1", "preview_scene": "scn_1"})
    );
    let switched = call_tool(
        &server,
        "scene",
        json!({"op": {"type": "scene.switch", "scene_id": "scn_1"}}),
    );
    let (failed, body) = content(&switched);
    assert!(!failed, "{switched}");
    assert_eq!(body["result"], json!({"kind": "action"}));

    let started = call_tool(
        &server,
        "output",
        json!({"op": {"type": "output.start", "output_id": "out_rec"}}),
    );
    let (failed, body) = content(&started);
    assert!(!failed, "{started}");
    assert_eq!(body["result"], json!({"kind": "action"}));

    // The session's own subscription saw the batch, the first scene going on program and
    // the output's placeholder steps.
    let mut seen = Vec::new();
    while let Poll::Ready(Some(event)) = events
        .as_mut()
        .poll_next(&mut Context::from_waker(Waker::noop()))
    {
        seen.push(event);
    }
    assert!(
        matches!(
            seen.as_slice(),
            [
                Event::StateChanged { rev: Rev(1), .. },
                Event::StudioChanged { .. },
                Event::OutputStateChanged {
                    state: OutputState::Starting,
                    ..
                },
                Event::OutputStateChanged {
                    state: OutputState::Active,
                    ..
                },
            ]
        ),
        "{seen:?}"
    );
}
