//! The built binary's `mcp` end to end: a `MockEngine` behind a real `jstream-server` on
//! an ephemeral `127.0.0.1` port, the binary connecting to it with `--url` and a token
//! file, and MCP messages on its stdin and stdout. Hermetic: loopback only.

use jstream_control::MockEngine;
use jstream_control::protocol::Rev;
use jstream_server::{Connector, Server, ServerConfig, Token};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

const TOKEN: &str = "mcp-e2e-token-0123";

/// Writes one request line and reads the reply line.
fn exchange(stdin: &mut impl Write, stdout: &mut impl BufRead, request: &Value) -> Value {
    writeln!(stdin, "{request}").expect("the proxy reads stdin");
    stdin.flush().expect("the proxy reads stdin");
    let mut line = String::new();
    stdout.read_line(&mut line).expect("the proxy replies");
    serde_json::from_str(&line).unwrap_or_else(|e| panic!("not JSON-RPC ({e}): {line:?}"))
}

fn call(id: u64, tool: &str, arguments: Value) -> Value {
    json!({
        "jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": {"name": tool, "arguments": arguments}
    })
}

/// Calls the `events` tool until `done` holds for its result, for up to five seconds:
/// events reach the proxy on the WebSocket client's own thread.
fn events_until(
    stdin: &mut impl Write,
    stdout: &mut impl BufRead,
    id: u64,
    done: impl Fn(&Value) -> bool,
) -> Value {
    for _ in 0..100 {
        let reply = exchange(stdin, stdout, &call(id, "events", json!({})));
        assert_eq!(reply["result"]["isError"], false, "{reply}");
        let page = reply["result"]["structuredContent"].clone();
        if done(&page) {
            return page;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("the events never arrived");
}

/// A `MockEngine` behind a real server configured by `configure`, on its own runtime.
struct Running {
    runtime: tokio::runtime::Runtime,
    engine: MockEngine,
    url: String,
    stop: tokio::sync::oneshot::Sender<()>,
}

fn serve(configure: impl FnOnce(&mut ServerConfig)) -> Running {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("a tokio runtime");
    let engine = MockEngine::new();
    let connector: Connector = {
        let engine = engine.clone();
        Arc::new(move || Box::new(engine.connect()))
    };
    let mut config = ServerConfig::new(Token::new(TOKEN).expect("a valid token"));
    configure(&mut config);
    let server = runtime
        .block_on(Server::bind(config, connector))
        .expect("the server binds");
    let url = server.ws_url();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    runtime.spawn(server.serve_with_shutdown(async move {
        let _ = stopped.await;
    }));
    Running {
        runtime,
        engine,
        url,
        stop,
    }
}

/// The binary's `mcp`, connected to `url` with a token file named after `name`.
struct Proxy {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    token: std::path::PathBuf,
}

fn proxy(url: &str, name: &str) -> Proxy {
    let token = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("{name}-{}", std::process::id()));
    std::fs::write(&token, format!("{TOKEN}\n")).expect("the token file is written");
    let mut child = Command::new(env!("CARGO_BIN_EXE_jstream-cli"))
        .args(["--url", url, "--token-file"])
        .arg(&token)
        .arg("mcp")
        .env_remove("JSTREAM_TOKEN_FILE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the binary runs");
    let stdin = child.stdin.take().expect("piped stdin");
    let stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
    Proxy {
        child,
        stdin,
        stdout,
        token,
    }
}

fn initialize(stdin: &mut impl Write, stdout: &mut impl BufRead) {
    let init = exchange(
        stdin,
        stdout,
        &json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "e2e-agent", "version": "1"}
            }
        }),
    );
    assert_eq!(
        init["result"]["serverInfo"]["name"], "jstream-mcp",
        "{init}"
    );
}

#[test]
fn mcp_proxies_to_a_running_server_and_survives_its_loss() {
    let Running {
        runtime,
        engine,
        url,
        stop,
    } = serve(|_| {});
    let Proxy {
        child,
        mut stdin,
        mut stdout,
        token,
    } = proxy(&url, "mcp-e2e-token");

    initialize(&mut stdin, &mut stdout);
    // The mock engine has no YouTube setup, so the proxy hides YouTube's tool.
    let tools = exchange(
        &mut stdin,
        &mut stdout,
        &json!({"jsonrpc": "2.0", "id": 9, "method": "tools/list"}),
    );
    let names: Vec<_> = tools["result"]["tools"]
        .as_array()
        .map(|tools| tools.iter().filter_map(|t| t["name"].as_str()).collect())
        .unwrap_or_default();
    assert!(names.contains(&"scene"), "{tools}");
    assert!(!names.contains(&"youtube"), "{tools}");
    let created = exchange(
        &mut stdin,
        &mut stdout,
        &call(
            2,
            "scene",
            json!({"op": {"type": "scene.create", "scene_id": "main", "name": "Main"}}),
        ),
    );
    assert_eq!(created["result"]["isError"], false, "{created}");
    assert_eq!(engine.rev(), Rev(1), "the edit reached the engine");
    // The edit's `state.changed` event comes back through the `events` tool, with the
    // session's client ID and the request ID of the call.
    let page = events_until(&mut stdin, &mut stdout, 10, |page| {
        page["events"].as_array().is_some_and(|e| !e.is_empty())
    });
    let event = &page["events"][0];
    assert_eq!(event["seq"], 1, "{page}");
    assert_eq!(event["event"]["type"], "state.changed", "{page}");
    assert_eq!(event["event"]["rev"], 1, "{page}");
    assert_eq!(event["event"]["request_id"], "req_1", "{page}");
    let client_id = event["event"]["client_id"].clone();
    assert!(client_id.is_string(), "{page}");

    // The engine goes away: the proxy answers with the `disconnected` tool error and
    // keeps serving.
    let _ = stop.send(());
    runtime.shutdown_timeout(Duration::from_secs(5));
    let lost = exchange(
        &mut stdin,
        &mut stdout,
        &call(3, "scene", json!({"op": {"type": "scene.list"}})),
    );
    assert_eq!(lost["result"]["isError"], true, "{lost}");
    assert_eq!(
        lost["result"]["structuredContent"]["error"]["code"], "disconnected",
        "{lost}"
    );
    let described = exchange(&mut stdin, &mut stdout, &call(4, "describe_api", json!({})));
    assert_eq!(described["result"]["isError"], false);
    // The next call says `hello` again on the same `WsControl`. That never yields a new
    // `client_id` (which would split the agent's undo history): a closed `WsControl`
    // fails every `hello`, so the call is not sent and the agent is told to restart.
    let reopened = exchange(
        &mut stdin,
        &mut stdout,
        &call(5, "scene", json!({"op": {"type": "scene.list"}})),
    );
    let error = &reopened["result"]["structuredContent"]["error"];
    assert_eq!(error["code"], "disconnected", "{reopened}");
    let message = error["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("not sent") && message.contains("undo history"),
        "{reopened}"
    );
    // The events read before the loss are still there, and the feed says it ended.
    let page = events_until(&mut stdin, &mut stdout, 6, |page| page["ended"] == true);
    assert_eq!(page["events"][0]["event"]["client_id"], client_id, "{page}");

    // End of input ends the proxy, successfully, with nothing on stderr.
    drop(stdin);
    let mut rest = String::new();
    std::io::Read::read_to_string(&mut stdout, &mut rest).expect("stdout closes");
    assert_eq!(rest, "", "nothing but replies on stdout");
    let output = child.wait_with_output().expect("the proxy exits");
    let _ = std::fs::remove_file(&token);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
}

/// The server closes a connection that hasn't completed its handshake within its
/// handshake timeout. The proxy connects when it starts, so it says `hello` at
/// `initialize`: an agent that initializes and then stays idle past the timeout keeps
/// its session.
#[test]
fn an_idle_agent_keeps_its_session_past_the_handshake_timeout() {
    let running = serve(|config| config.handshake_timeout = Duration::from_millis(300));
    let Proxy {
        child,
        mut stdin,
        mut stdout,
        token,
    } = proxy(&running.url, "mcp-idle-token");
    initialize(&mut stdin, &mut stdout);
    // Idle well past the timeout.
    std::thread::sleep(Duration::from_millis(900));
    let created = exchange(
        &mut stdin,
        &mut stdout,
        &call(
            2,
            "scene",
            json!({"op": {"type": "scene.create", "scene_id": "main", "name": "Main"}}),
        ),
    );
    assert_eq!(created["result"]["isError"], false, "{created}");
    assert_eq!(running.engine.rev(), Rev(1), "the edit reached the engine");

    drop(stdin);
    let output = child.wait_with_output().expect("the proxy exits");
    let _ = std::fs::remove_file(&token);
    assert_eq!(output.status.code(), Some(0));
    let _ = running.stop.send(());
}
