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

#[test]
fn mcp_proxies_to_a_running_server_and_survives_its_loss() {
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
    let config = ServerConfig::new(Token::new(TOKEN).expect("a valid token"));
    let server = runtime
        .block_on(Server::bind(config, connector))
        .expect("the server binds");
    let url = server.ws_url();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    runtime.spawn(server.serve_with_shutdown(async move {
        let _ = stopped.await;
    }));

    let token = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("mcp-e2e-token");
    std::fs::write(&token, format!("{TOKEN}\n")).expect("the token file is written");
    let mut child = Command::new(env!("CARGO_BIN_EXE_jstream-cli"))
        .args(["--url", &url, "--token-file"])
        .arg(&token)
        .arg("mcp")
        .env_remove("JSTREAM_TOKEN_FILE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the binary runs");
    let mut stdin = child.stdin.take().expect("piped stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("piped stdout"));

    let init = exchange(
        &mut stdin,
        &mut stdout,
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
