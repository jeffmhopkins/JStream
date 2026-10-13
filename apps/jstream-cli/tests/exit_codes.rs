//! The built binary's exit codes and output streams, as a script sees them. Hermetic: it
//! runs the binary against its in-memory `--mock` engine, or against a loopback port
//! nothing listens on, with no network.

use std::process::{Command, Output};

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_jstream-cli"))
        .args(args)
        .env_remove("JSTREAM_TOKEN_FILE")
        .output()
        .expect("the binary runs")
}

/// A loopback address nothing listens on: bound, then released.
fn unused_port() -> std::net::SocketAddr {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .expect("a loopback port")
}

fn code(output: &Output) -> Option<i32> {
    output.status.code()
}

#[test]
fn success_writes_stdout_only() {
    let out = cli(&["--mock", "edit", "scene.create", "main", "Main"]);
    assert_eq!(code(&out), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "applied: rev 1 (req_1)\n"
    );
    assert!(out.stderr.is_empty());

    let out = cli(&["--help"]);
    assert_eq!(code(&out), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("jstream-cli: "));
}

#[test]
fn each_failure_kind_has_its_exit_code() {
    // Usage.
    assert_eq!(code(&cli(&[])), Some(2));
    assert_eq!(code(&cli(&["--mock", "scene", "explode"])), Some(2));
    // No engine address, or no token: usage.
    assert_eq!(code(&cli(&["scene", "list"])), Some(2));
    let token = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("exit-codes-token");
    std::fs::write(&token, "t0ken\n").expect("the token file is written");
    let token = token.to_str().expect("a UTF-8 temp path");
    let url = format!("ws://{}", unused_port());
    assert_eq!(code(&cli(&["--url", &url, "scene", "list"])), Some(2));
    // No connection: nothing listens at the address.
    let out = cli(&["--url", &url, "--token-file", token, "scene", "list"]);
    assert_eq!(code(&out), Some(3));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("error: no connection"));
    // Engine errors, one code per protocol error code.
    assert_eq!(
        code(&cli(&[
            "--mock",
            "edit",
            "--expected-rev",
            "7",
            "scene.create",
            "a",
            "A"
        ])),
        Some(14)
    );
    assert_eq!(
        code(&cli(&["--mock", "output", "status", "nope"])),
        Some(15)
    );
    let dup = [
        "--mock",
        "edit",
        "scene.create",
        "a",
        "A",
        "scene.create",
        "a",
        "B",
    ];
    assert_eq!(code(&cli(&dup)), Some(16));
    assert_eq!(
        code(&cli(&["--mock", "edit", "scene.create", "a", " "])),
        Some(17)
    );
    let stop = ["--mock", "--mock-output", "o", "output", "stop", "o"];
    assert_eq!(code(&cli(&stop)), Some(18));
}

#[test]
fn json_errors_go_to_stderr() {
    let out = cli(&["--json", "--mock", "output", "status", "nope"]);
    assert_eq!(code(&out), Some(15));
    assert!(out.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "{\"error\":{\"code\":\"not_found\",\"entity\":\"output\",\"id\":\"nope\",\
         \"message\":\"output `nope` does not exist\"}}\n"
    );
}
