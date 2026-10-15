//! The built binary end to end: it starts on a free `127.0.0.1` port with a token file
//! it creates, `jstream-control`'s WebSocket client drives it (handshake, a batch, a
//! query and the `state.changed` event), an MCP client reads the result over streamable
//! HTTP, and a SIGINT stops it gracefully. A second run reuses the token. Hermetic: loopback only, files under the target directory (the
//! platform data directory, where logs go by default, is pointed there too).

use futures_util::StreamExt;
use jstream_control::protocol::{
    Batch, Call, CallResult, Edit, Event, Hello, PROTOCOL_VERSION, Query, QueryResult, Rev, SceneId,
};
use jstream_control::{Control, ControlError, WsConfig, WsControl};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// A fresh, empty directory for one test.
fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("daemon-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the scratch directory is created");
    dir
}

/// POSTs one JSON-RPC message to the daemon's MCP endpoint at `authority`, with `token`
/// and the session ID if given, and returns the status, the `Mcp-Session-Id` answered and
/// the body.
fn mcp_post(
    authority: &str,
    token: &str,
    session: Option<&str>,
    body: &str,
) -> (u16, Option<String>, String) {
    use std::io::Write;
    let mut stream = std::net::TcpStream::connect(authority).expect("connects");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("a read timeout");
    let session = session
        .map(|id| format!("Mcp-Session-Id: {id}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "POST /mcp HTTP/1.1\r\nHost: {authority}\r\nAuthorization: Bearer {token}\r\n\
         Content-Type: application/json\r\n{session}Connection: close\r\n\
         Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).expect("sends");
    let mut answer = String::new();
    stream
        .read_to_string(&mut answer)
        .expect("reads the answer");
    let (head, body) = answer.split_once("\r\n\r\n").unwrap_or((&answer, ""));
    let status = head
        .split(' ')
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let session = head.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("mcp-session-id")
            .then(|| value.trim().to_owned())
    });
    (status, session, body.to_owned())
}

/// A running daemon, killed if the test fails before stopping it.
struct Daemon {
    child: Child,
    /// Its stdout, line by line (newlines kept), from a reader thread, so a daemon that
    /// hangs fails the test after a deadline instead of blocking it.
    stdout: Receiver<String>,
    url: String,
    /// The lines it printed before the URL line.
    banner: Vec<String>,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The daemon's command, with its default data directory under the target directory,
/// so a test that passes no `--log-dir` doesn't write into the real one.
fn daemon() -> Command {
    let home = Path::new(env!("CARGO_TARGET_TMPDIR")).join("daemon-home");
    let mut command = Command::new(env!("CARGO_BIN_EXE_jstream-daemon"));
    command.env_remove("JSTREAM_TOKEN_FILE");
    command.env_remove("RUST_LOG");
    command.env_remove("XDG_DATA_HOME");
    command.env("HOME", &home);
    command.env("APPDATA", &home);
    command
}

/// The daemon's log files in `dir`, oldest first.
fn log_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .map(|entry| entry.expect("an entry").path())
                .filter(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| {
                            name.starts_with("jstream-daemon-") && name.ends_with(".log")
                        })
                })
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

/// Starts the daemon with `args` and waits for its `listening on <url>` line.
fn start(args: &[&str]) -> Daemon {
    let mut child = daemon()
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the binary runs");
    let mut reader = BufReader::new(child.stdout.take().expect("piped stdout"));
    let (lines, stdout) = mpsc::channel();
    std::thread::spawn(move || {
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if lines.send(line).is_err() {
                        break;
                    }
                }
            }
        }
    });
    // Owned by the guard from here on, so a failing start doesn't leave it running.
    let mut daemon = Daemon {
        child,
        stdout,
        url: String::new(),
        banner: Vec::new(),
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let line = match daemon
            .stdout
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            Ok(line) => line,
            Err(RecvTimeoutError::Timeout) => {
                panic!("the daemon printed no `listening on` line within 10 s")
            }
            Err(RecvTimeoutError::Disconnected) => {
                let mut stderr = String::new();
                let _ = daemon
                    .child
                    .stderr
                    .take()
                    .map(|mut e| e.read_to_string(&mut stderr));
                panic!("the daemon exited before serving: {stderr}");
            }
        };
        if let Some(url) = line.trim_end().strip_prefix("listening on ") {
            daemon.url = url.to_owned();
            return daemon;
        }
        daemon.banner.push(line.trim_end().to_owned());
    }
}

/// The rest of `daemon`'s stdout, once it has closed (within 10 s).
#[cfg(unix)]
fn rest_of_stdout(daemon: &Daemon) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut rest = String::new();
    loop {
        match daemon
            .stdout
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            Ok(line) => rest.push_str(&line),
            Err(RecvTimeoutError::Disconnected) => return rest,
            Err(RecvTimeoutError::Timeout) => panic!("stdout did not close within 10 s"),
        }
    }
}

/// Waits up to 10 s for the process to exit.
fn wait(child: &mut Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().expect("the status is readable") {
            return status;
        }
        assert!(Instant::now() < deadline, "the daemon did not exit");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Fails the test after 10 s instead of hanging.
async fn timely<T>(future: impl Future<Output = T>) -> T {
    match tokio::time::timeout(Duration::from_secs(10), future).await {
        Ok(value) => value,
        Err(_) => panic!("timed out"),
    }
}

/// A token the daemon refuses; it must never show up in its log.
const WRONG_TOKEN: &str = "n0t-the-t0ken";

fn hello() -> Hello {
    Hello {
        protocol: PROTOCOL_VERSION,
        client_name: "daemon-e2e".into(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serves_the_engine_and_stops_on_a_signal() {
    let dir = scratch("serve");
    let token_file = dir.join("token");
    let token_arg = token_file.to_str().expect("a UTF-8 path");
    let logs = dir.join("logs");
    let logs_arg = logs.to_str().expect("a UTF-8 path");
    let mut daemon = start(&[
        "--token-file",
        token_arg,
        "--port",
        "0",
        "--output",
        "out_1",
        "--log-dir",
        logs_arg,
    ]);
    assert!(daemon.url.starts_with("ws://127.0.0.1:"), "{}", daemon.url);
    assert!(daemon.url.ends_with("/api/v1/ws"), "{}", daemon.url);
    let authority = daemon
        .url
        .strip_prefix("ws://")
        .and_then(|rest| rest.strip_suffix("/api/v1/ws"))
        .expect("ws://<authority>/api/v1/ws")
        .to_owned();
    assert_eq!(
        daemon.banner,
        [
            format!("token file: {token_arg} (created, new token)"),
            format!("mcp endpoint: http://{authority}/mcp"),
        ]
    );
    let token = std::fs::read_to_string(&token_file).expect("the token file exists");
    let token = token.trim();
    assert_eq!(token.len(), 64);

    // A wrong token is refused.
    match timely(WsControl::connect(WsConfig::new(
        daemon.url.clone(),
        WRONG_TOKEN,
    )))
    .await
    {
        Err(ControlError::Disconnected { .. }) => {}
        Err(other) => panic!("expected a refusal, got {other:?}"),
        Ok(_) => panic!("a wrong token was accepted"),
    }

    let control = timely(WsControl::connect(WsConfig::new(daemon.url.clone(), token)))
        .await
        .expect("connects with the token from the file");
    let welcome = timely(control.hello(hello())).await.expect("welcome");
    assert_eq!(welcome.rev, Rev(0), "the engine starts empty");
    let mut events = control.events();

    let applied = timely(control.call(Call::Batch(Batch::single(Edit::SceneCreate {
        scene_id: SceneId::new("main"),
        name: "Main".to_owned(),
    }))))
    .await
    .expect("applied");
    assert_eq!(applied.result, CallResult::Batch { rev: Rev(1) });
    match timely(events.next()).await {
        Some(Event::StateChanged {
            rev,
            client_id,
            request_id,
            cause: None,
        }) => {
            assert_eq!(rev, Rev(1));
            assert_eq!(client_id, welcome.client_id);
            assert_eq!(request_id, Some(applied.id.clone()), "the client's own ID");
        }
        other => panic!("expected state.changed, got {other:?}"),
    }
    let listed = timely(control.call(Call::Query {
        query: Query::SceneList,
    }))
    .await
    .expect("answered");
    match listed.result {
        CallResult::Query {
            result: QueryResult::SceneList { rev, scenes },
        } => {
            assert_eq!(rev, Rev(1));
            let ids: Vec<_> = scenes.iter().map(|scene| scene.scene_id.as_str()).collect();
            assert_eq!(ids, ["main"]);
        }
        other => panic!("expected scene.list, got {other:?}"),
    }

    // An MCP client over streamable HTTP is its own engine client and sees the scene.
    let initialize = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"daemon-mcp-test","version":"1"}}}"#;
    let (status, session, body) = mcp_post(&authority, token, None, initialize);
    assert_eq!(status, 200, "{body}");
    assert!(
        body.contains("\"protocolVersion\":\"2025-11-25\""),
        "{body}"
    );
    let session = session.expect("an Mcp-Session-Id");
    let list = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"scene","arguments":{"op":{"type":"scene.list"}}}}"#;
    let (status, _, body) = mcp_post(&authority, token, Some(&session), list);
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"isError\":false"), "{body}");
    assert!(body.contains("main"), "{body}");
    let (status, _, _) = mcp_post(&authority, WRONG_TOKEN, None, initialize);
    assert_eq!(status, 401, "the MCP endpoint needs the token too");

    stop(&mut daemon);
    // The log on stderr (default level `info`) shows the refusal and the accepted
    // connection, and neither token (R-SEC-001).
    let mut log = String::new();
    if let Some(mut stderr) = daemon.child.stderr.take() {
        stderr.read_to_string(&mut log).expect("stderr is readable");
    }
    assert!(log.contains("connection refused"), "{log}");
    assert!(log.contains("missing or wrong control token"), "{log}");
    assert!(log.contains("connection accepted"), "{log}");
    assert!(log.contains("mcp session opened"), "{log}");
    assert!(!log.contains(token), "the token was logged: {log}");
    assert!(
        !log.contains(WRONG_TOKEN),
        "the wrong token was logged: {log}"
    );
    // The same lines are in the run's log file, with neither token either.
    let files = log_files(&logs);
    assert_eq!(files.len(), 1, "{files:?}");
    let file = std::fs::read_to_string(&files[0]).expect("the log file is readable");
    assert!(file.contains("jstream-daemon starting"), "{file}");
    assert!(file.contains("connection refused"), "{file}");
    assert!(file.contains("connection accepted"), "{file}");
    assert!(!file.contains(token), "the token was logged: {file}");
    assert!(
        !file.contains(WRONG_TOKEN),
        "the wrong token was logged: {file}"
    );
    assert_eq!(timely(events.next()).await, None, "the event stream ends");
    match timely(control.call(Call::Query {
        query: Query::SceneList,
    }))
    .await
    {
        Err(ControlError::Disconnected { .. }) => {}
        other => panic!("expected Disconnected, got {other:?}"),
    }

    // A restart reuses the token in the file, and the engine starts empty again. It
    // logs to a new file, at the level `--log-level` sets.
    let mut again = start(&[
        "--token-file",
        token_arg,
        "--log-dir",
        logs_arg,
        "--log-level",
        "warn",
    ]);
    assert_eq!(
        again.banner,
        [
            format!("token file: {token_arg} (existing token)"),
            format!(
                "mcp endpoint: {}",
                again
                    .url
                    .replace("ws://", "http://")
                    .replace("/api/v1/ws", "/mcp")
            ),
        ]
    );
    assert_eq!(
        std::fs::read_to_string(&token_file)
            .expect("still there")
            .trim(),
        token
    );
    let control = timely(WsControl::connect(WsConfig::new(again.url.clone(), token)))
        .await
        .expect("the same token still works");
    let welcome = timely(control.hello(hello())).await.expect("welcome");
    assert_eq!(welcome.rev, Rev(0), "no persistence yet");
    stop(&mut again);
    let first = files[0].clone();
    let files = log_files(&logs);
    assert_eq!(files.len(), 2, "{files:?}");
    // Both runs can start in the same second, when the names order by process ID, so
    // the new file is the one that is not the first run's.
    let newer = files
        .iter()
        .find(|f| **f != first)
        .expect("a second log file");
    let file = std::fs::read_to_string(newer).expect("the log file is readable");
    assert!(
        !file.contains("connection accepted"),
        "info is filtered out: {file}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Stops the daemon gracefully where the test can send a signal (SIGINT, as Ctrl-C does
/// on Unix) and checks it exits 0 after `stopped`; elsewhere it is killed.
fn stop(daemon: &mut Daemon) {
    #[cfg(unix)]
    {
        let status = Command::new("kill")
            .args(["-INT", &daemon.child.id().to_string()])
            .status()
            .expect("`kill` runs");
        assert!(status.success(), "SIGINT was sent");
        assert_eq!(wait(&mut daemon.child).code(), Some(0), "a graceful stop");
        assert_eq!(rest_of_stdout(daemon), "stopped\n");
    }
    #[cfg(not(unix))]
    {
        let _ = daemon.child.kill();
        let _ = wait(&mut daemon.child);
    }
}

#[test]
fn usage_errors_exit_2_and_start_nothing() {
    let dir = scratch("usage");
    let token_file = dir.join("token");
    let token_arg = token_file.to_str().expect("a UTF-8 path");
    for (args, message) in [
        (
            vec!["--token-file", token_arg, "--collection", "c.json"],
            "`--collection` is not supported yet",
        ),
        (
            vec!["--token-file", token_arg, "--port", "http"],
            "`--port` must be a number",
        ),
        (vec![], "no control token file"),
    ] {
        let output = daemon().args(&args).output().expect("the binary runs");
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.starts_with(&format!("error: {message}")),
            "{args:?}: {stderr}"
        );
        assert!(output.stdout.is_empty());
    }
    // A `RUST_LOG` that doesn't parse is a usage error too, reported before the token
    // file is touched.
    let output = daemon()
        .args(["--token-file", token_arg])
        .env("RUST_LOG", "jstream_server=loud")
        .output()
        .expect("the binary runs");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.starts_with("error: RUST_LOG: "), "{stderr}");
    assert!(
        !token_file.exists(),
        "no token file is created on a usage error"
    );
    // So is a `--log-level` that doesn't parse, which wins over a valid `RUST_LOG`; and
    // no log directory is created.
    let logs = dir.join("logs");
    let output = daemon()
        .args([
            "--token-file",
            token_arg,
            "--log-level",
            "jstream_core=loud",
        ])
        .arg("--log-dir")
        .arg(&logs)
        .env("RUST_LOG", "info")
        .output()
        .expect("the binary runs");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.starts_with("error: --log-level: "), "{stderr}");
    assert!(!token_file.exists() && !logs.exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A log directory that can't be created is a startup failure, before the token file
/// is touched.
#[test]
fn an_unusable_log_directory_exits_1() {
    let dir = scratch("unusable-logs");
    let token_file = dir.join("token");
    let not_a_dir = dir.join("file");
    std::fs::write(&not_a_dir, "").expect("written");
    let output = daemon()
        .arg("--token-file")
        .arg(&token_file)
        .arg("--log-dir")
        .arg(not_a_dir.join("logs"))
        .output()
        .expect("the binary runs");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.starts_with("error: could not create the log directory"),
        "{stderr}"
    );
    assert!(!token_file.exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Without `--log-dir`, the logs go to `logs` in the platform data directory:
/// `$XDG_DATA_HOME/jstream` here.
#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn logs_go_to_the_data_directory_by_default() {
    let dir = scratch("default-logs");
    let token_file = dir.join("token");
    std::fs::write(&token_file, "\n").expect("written");
    {
        use std::os::unix::fs::PermissionsExt;
        let user_only = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(&token_file, user_only).expect("chmod");
    }
    // The empty token file makes it exit 1 right after logging starts.
    let output = daemon()
        .arg("--token-file")
        .arg(&token_file)
        .env("XDG_DATA_HOME", &dir)
        .output()
        .expect("the binary runs");
    assert_eq!(output.status.code(), Some(1));
    let files = log_files(&dir.join("jstream/logs"));
    assert_eq!(files.len(), 1, "{files:?}");
    let file = std::fs::read_to_string(&files[0]).expect("readable");
    assert!(file.contains("jstream-daemon starting"), "{file}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unusable_token_file_exits_1() {
    let dir = scratch("unusable");
    let token_file = dir.join("token");
    std::fs::write(&token_file, "\n").expect("written");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let user_only = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(&token_file, user_only).expect("chmod");
    }
    let output = daemon()
        .arg("--token-file")
        .arg(&token_file)
        .output()
        .expect("the binary runs");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("is empty"), "{stderr}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn help_and_version_print_to_stdout() {
    let help = daemon().arg("--help").output().expect("the binary runs");
    assert_eq!(help.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&help.stdout).starts_with("jstream-daemon: "));
    let version = daemon().arg("--version").output().expect("the binary runs");
    assert_eq!(version.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&version.stdout),
        format!(
            "jstream-daemon {}, protocol {PROTOCOL_VERSION}\n",
            env!("CARGO_PKG_VERSION")
        )
    );
}
