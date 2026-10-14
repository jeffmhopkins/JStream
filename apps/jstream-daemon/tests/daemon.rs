//! The built binary end to end: it starts on a free `127.0.0.1` port with a token file
//! it creates, `jstream-control`'s WebSocket client drives it (handshake, a batch, a
//! query and the `state.changed` event), and a SIGINT stops it gracefully. A second run
//! reuses the token. Hermetic: loopback only, files under the target directory.

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

fn daemon() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_jstream-daemon"));
    command.env_remove("JSTREAM_TOKEN_FILE");
    command.env_remove("RUST_LOG");
    command
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
    let mut daemon = start(&[
        "--token-file",
        token_arg,
        "--port",
        "0",
        "--output",
        "out_1",
    ]);
    assert!(daemon.url.starts_with("ws://127.0.0.1:"), "{}", daemon.url);
    assert!(daemon.url.ends_with("/api/v1/ws"), "{}", daemon.url);
    assert_eq!(
        daemon.banner,
        [format!("token file: {token_arg} (created, new token)")]
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
    assert!(!log.contains(token), "the token was logged: {log}");
    assert!(
        !log.contains(WRONG_TOKEN),
        "the wrong token was logged: {log}"
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

    // A restart reuses the token in the file, and the engine starts empty again.
    let mut again = start(&["--token-file", token_arg]);
    assert_eq!(
        again.banner,
        [format!("token file: {token_arg} (existing token)")]
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
