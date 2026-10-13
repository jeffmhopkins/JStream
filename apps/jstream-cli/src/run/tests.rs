//! Golden output (human and `--json`) and exit codes of whole CLI runs against the mock.

use super::{CLIENT_NAME, Connector, connect, read_token, run, stream_events};
use crate::args::Connection;
use crate::block::block_on;
use crate::exit::{self, CliError};
use jstream_control::protocol::{
    Batch, Call, ClientId, Edit, Event, Hello, ItemId, OutputId, OutputState, PROTOCOL_VERSION,
    Rev, SceneId, SourceId, SourceKind,
};
use jstream_control::{Control, ControlError, MockControl, MockEngine};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

/// What one CLI run printed and returned.
#[derive(Debug, PartialEq)]
struct Outcome {
    code: u8,
    out: String,
    err: String,
}

fn strings(line: &[&str]) -> Vec<String> {
    line.iter().map(|s| (*s).to_owned()).collect()
}

/// Runs the CLI with `line`, connecting through `connector`.
fn run_with(connector: Connector<'_>, line: &[&str]) -> Outcome {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run(&strings(line), connector, &mut out, &mut err);
    Outcome {
        code,
        out: String::from_utf8(out).expect("utf-8 output"),
        err: String::from_utf8(err).expect("utf-8 errors"),
    }
}

/// Runs the CLI with `line` against a new session of `engine`.
fn run_on(engine: &MockEngine, line: &[&str]) -> Outcome {
    run_with(
        &|_: &Connection| -> Result<Box<dyn Control>, CliError> { Ok(Box::new(engine.connect())) },
        line,
    )
}

fn ok(out: &str) -> Outcome {
    Outcome {
        code: exit::OK,
        out: out.into(),
        err: String::new(),
    }
}

fn failed(code: u8, err: &str) -> Outcome {
    Outcome {
        code,
        out: String::new(),
        err: err.into(),
    }
}

fn session(engine: &MockEngine) -> MockControl {
    let control = engine.connect();
    block_on(control.hello(Hello {
        protocol: PROTOCOL_VERSION,
        client_name: "test".into(),
    }))
    .expect("hello");
    control
}

/// An engine at rev 1 holding two scenes, a source placed twice in `main`, and the
/// stopped output `out_1`. The setup session is `mock_1`.
fn populated() -> MockEngine {
    let engine = MockEngine::new().with_output(OutputId::new("out_1"));
    let setup = session(&engine);
    let batch = Batch {
        edits: vec![
            Edit::SceneCreate {
                scene_id: SceneId::new("main"),
                name: "Main".into(),
            },
            Edit::SceneCreate {
                scene_id: SceneId::new("brb"),
                name: "Be right back".into(),
            },
            Edit::SourceCreate {
                source_id: SourceId::new("cam"),
                name: "Camera".into(),
                kind: SourceKind::Builtin {
                    kind: "webcam".into(),
                },
            },
            Edit::ItemAdd {
                scene_id: SceneId::new("main"),
                item_id: ItemId::new("cam_1"),
                source_id: SourceId::new("cam"),
                transform: None,
            },
            Edit::ItemAdd {
                scene_id: SceneId::new("main"),
                item_id: ItemId::new("cam_2"),
                source_id: SourceId::new("cam"),
                transform: None,
            },
        ],
        expected_rev: None,
        idempotency_key: None,
    };
    block_on(setup.call(Call::Batch(batch))).expect("setup batch");
    engine
}

fn refuse(_: &Connection) -> Result<Box<dyn Control>, CliError> {
    Err(CliError::Control(ControlError::Disconnected {
        reason: "refused".into(),
    }))
}

#[test]
fn version_and_help_need_no_connection() {
    let version = env!("CARGO_PKG_VERSION");
    assert_eq!(
        run_with(&refuse, &["version"]),
        ok(&format!("jstream-cli {version}, protocol 0.3\n"))
    );
    assert_eq!(
        run_with(&refuse, &["--json", "version"]),
        ok(&format!(
            "{{\"cli\":\"{version}\",\"protocol\":{{\"major\":0,\"minor\":3}}}}\n"
        ))
    );
    let help = run_with(&refuse, &["help"]);
    assert_eq!(help.code, exit::OK);
    assert_eq!(help.out, crate::args::USAGE);
    assert_eq!(
        run_with(&refuse, &["--mock", "--help"]).out,
        crate::args::USAGE
    );
}

#[test]
fn hello_prints_the_session() {
    let engine = populated();
    assert_eq!(
        run_on(&engine, &["--mock", "hello"]),
        ok("connected: protocol 0.3, client mock_2, rev 1\n")
    );
    assert_eq!(
        run_on(&engine, &["--json", "--mock", "hello"]),
        ok("{\"protocol\":{\"major\":0,\"minor\":3},\"client_id\":\"mock_3\",\"rev\":1}\n")
    );
}

#[test]
fn scene_list_golden() {
    let engine = populated();
    assert_eq!(
        run_on(&engine, &["--mock", "scene", "list"]),
        ok("rev 1\n\
            ID    NAME           ITEMS\n\
            main  Main           2\n\
            brb   Be right back  0\n")
    );
    assert_eq!(
        run_on(&engine, &["--json", "--mock", "scene", "list"]),
        ok(concat!(
            r#"{"id":"req_1","result":{"kind":"query","result":{"type":"scene.list","rev":1,"#,
            r#""scenes":[{"scene_id":"main","name":"Main","item_count":2},"#,
            r#"{"scene_id":"brb","name":"Be right back","item_count":0}]}}}"#,
            "\n"
        ))
    );
    assert_eq!(
        run_on(&MockEngine::new(), &["--mock", "scene", "list"]),
        ok("rev 0\nno scenes\n")
    );
}

#[test]
fn source_list_golden() {
    let engine = populated();
    let nest = [
        "--mock",
        "edit",
        "source.create",
        "nest",
        "Nested BRB",
        "scene:brb",
    ];
    assert_eq!(run_on(&engine, &nest), ok("applied: rev 2 (req_1)\n"));
    assert_eq!(
        run_on(&engine, &["--mock", "source", "list"]),
        ok("rev 2\n\
            ID    NAME        KIND\n\
            cam   Camera      builtin:webcam\n\
            nest  Nested BRB  scene:brb\n")
    );
    assert_eq!(
        run_on(&engine, &["--json", "--mock", "source", "list"]),
        ok(concat!(
            r#"{"id":"req_1","result":{"kind":"query","result":{"type":"source.list","rev":2,"#,
            r#""sources":[{"source_id":"cam","name":"Camera","kind":{"type":"builtin","kind":"webcam"}},"#,
            r#"{"source_id":"nest","name":"Nested BRB","kind":{"type":"scene","scene_id":"brb"}}]}}}"#,
            "\n"
        ))
    );
    assert_eq!(
        run_on(&MockEngine::new(), &["--mock", "source", "list"]),
        ok("rev 0\nno sources\n")
    );
}

#[test]
fn item_list_golden() {
    let engine = populated();
    let line = [
        "--mock",
        "edit",
        "item.set_transform",
        "cam_2",
        "100",
        "50.5",
        "0.5",
        "0.5",
        "-90",
        "item.set_crop",
        "cam_2",
        "0",
        "0",
        "10",
        "0",
        "item.set_visible",
        "cam_2",
        "false",
    ];
    assert_eq!(run_on(&engine, &line), ok("applied: rev 2 (req_1)\n"));
    assert_eq!(
        run_on(&engine, &["--mock", "item", "list", "main"]),
        ok("rev 2, scene main\n\
            ID     SOURCE  VISIBLE  POSITION  SCALE    ROTATION  CROP\n\
            cam_1  cam     true     0,0       1,1      0         0,0,0,0\n\
            cam_2  cam     false    100,50.5  0.5,0.5  -90       0,0,10,0\n")
    );
    assert_eq!(
        run_on(&engine, &["--json", "--mock", "item", "list", "main"]),
        ok(concat!(
            r#"{"id":"req_1","result":{"kind":"query","result":{"type":"item.list","rev":2,"#,
            r#""scene_id":"main","items":[{"item_id":"cam_1","source_id":"cam","#,
            r#""transform":{"position":{"x":0.0,"y":0.0},"scale":{"x":1.0,"y":1.0},"rotation_deg":0.0},"#,
            r#""crop":{"left":0,"top":0,"right":0,"bottom":0},"visible":true},"#,
            r#"{"item_id":"cam_2","source_id":"cam","#,
            r#""transform":{"position":{"x":100.0,"y":50.5},"scale":{"x":0.5,"y":0.5},"rotation_deg":-90.0},"#,
            r#""crop":{"left":0,"top":0,"right":10,"bottom":0},"visible":false}]}}}"#,
            "\n"
        ))
    );
    assert_eq!(
        run_on(&engine, &["--mock", "item", "list", "brb"]),
        ok("rev 2, scene brb\nno items\n")
    );
    assert_eq!(
        run_on(&engine, &["--mock", "item", "list", "nope"]),
        failed(15, "error: scene `nope` does not exist (not_found)\n")
    );
}

#[test]
fn state_get_golden() {
    let engine = populated();
    assert_eq!(
        run_on(&engine, &["--mock", "edit", "item.reorder", "cam_2", "0"]),
        ok("applied: rev 2 (req_1)\n")
    );
    assert_eq!(
        run_on(&engine, &["--mock", "state", "get"]),
        ok("rev 2\n\
            sources:\n\
           \x20 ID   NAME    KIND\n\
           \x20 cam  Camera  builtin:webcam\n\
            scene main (Main):\n\
           \x20 ID     SOURCE  VISIBLE  POSITION  SCALE  ROTATION  CROP\n\
           \x20 cam_2  cam     true     0,0       1,1    0         0,0,0,0\n\
           \x20 cam_1  cam     true     0,0       1,1    0         0,0,0,0\n\
            scene brb (Be right back):\n\
           \x20 no items\n")
    );
    let item = |id: &str| {
        format!(
            r#"{{"item_id":"{id}","source_id":"cam","transform":{{"position":{{"x":0.0,"y":0.0}},"scale":{{"x":1.0,"y":1.0}},"rotation_deg":0.0}},"crop":{{"left":0,"top":0,"right":0,"bottom":0}},"visible":true}}"#
        )
    };
    assert_eq!(
        run_on(&engine, &["--json", "--mock", "state", "get"]),
        ok(&format!(
            "{}{}{},{}{}\n",
            r#"{"id":"req_1","result":{"kind":"query","result":{"type":"state.get","rev":2,"#,
            r#""sources":[{"source_id":"cam","name":"Camera","kind":{"type":"builtin","kind":"webcam"}}],"scenes":[{"scene_id":"main","name":"Main","items":["#,
            item("cam_2"),
            item("cam_1"),
            r#"]},{"scene_id":"brb","name":"Be right back","items":[]}]}}}"#,
        ))
    );
    assert_eq!(
        run_on(&MockEngine::new(), &["--mock", "state", "get"]),
        ok("rev 0\nsources:\n  no sources\nno scenes\n")
    );
}

#[test]
fn edits_go_as_one_batch_golden() {
    let engine = populated();
    let line = [
        "--mock",
        "edit",
        "--expected-rev",
        "1",
        "--idempotency-key",
        "k1",
        "scene.rename",
        "brb",
        "Back soon",
        "item.set_visible",
        "cam_2",
        "false",
        "item.set_crop",
        "cam_2",
        "0",
        "0",
        "10",
        "0",
        "item.set_transform",
        "cam_2",
        "100",
        "50",
        "0.5",
        "0.5",
        "0",
        "item.reorder",
        "cam_2",
        "0",
        "item.remove",
        "cam_1",
    ];
    // Six edits, one revision.
    assert_eq!(run_on(&engine, &line), ok("applied: rev 2 (req_1)\n"));
    assert_eq!(engine.rev(), Rev(2));
    assert_eq!(
        run_on(
            &engine,
            &["--json", "--mock", "edit", "scene.delete", "brb"]
        ),
        ok("{\"id\":\"req_1\",\"result\":{\"kind\":\"batch\",\"rev\":3}}\n")
    );
    assert_eq!(
        run_on(&engine, &["--mock", "scene", "list"]),
        ok("rev 3\nID    NAME  ITEMS\nmain  Main  1\n")
    );
}

#[test]
fn a_failing_edit_applies_nothing() {
    let engine = populated();
    let line = [
        "--mock",
        "edit",
        "scene.create",
        "extra",
        "Extra",
        "item.remove",
        "nope",
    ];
    assert_eq!(
        run_on(&engine, &line),
        failed(15, "error: item `nope` does not exist (not_found)\n")
    );
    assert_eq!(engine.rev(), Rev(1));
}

#[test]
fn engine_errors_golden_and_exit_codes() {
    let engine = populated();
    // A stale revision: nothing is applied.
    let stale = ["edit", "--expected-rev", "0", "scene.delete", "main"];
    assert_eq!(
        run_on(&engine, &[&["--mock"][..], &stale].concat()),
        failed(
            14,
            "error: expected rev 0, but the state is at rev 1 (rev_conflict)\n"
        )
    );
    assert_eq!(
        run_on(&engine, &[&["--json", "--mock"][..], &stale].concat()),
        failed(
            14,
            "{\"error\":{\"code\":\"rev_conflict\",\"current\":1,\"expected\":0,\
             \"message\":\"expected rev 0, but the state is at rev 1\"}}\n"
        )
    );
    assert_eq!(engine.rev(), Rev(1));
    assert_eq!(
        run_on(
            &engine,
            &["--mock", "edit", "scene.create", "main", "Again"]
        ),
        failed(16, "error: scene `main` already exists (already_exists)\n")
    );
    assert_eq!(
        run_on(&engine, &["--mock", "edit", "source.delete", "cam"]),
        failed(
            17,
            "error: invalid `edits.0.source_id`: source `cam` is placed by items; \
             remove them first (invalid_argument)\n"
        )
    );
    assert_eq!(
        run_on(&engine, &["--mock", "output", "stop", "out_1"]),
        failed(
            18,
            "error: action failed: output `out_1` is not running (action_failed)\n"
        )
    );
    assert_eq!(
        run_on(&engine, &["--mock", "output", "status", "nope"]),
        failed(15, "error: output `nope` does not exist (not_found)\n")
    );
}

#[test]
fn outputs_golden() {
    let engine = populated();
    assert_eq!(
        run_on(&engine, &["--mock", "output", "status", "out_1"]),
        ok("out_1: stopped\n")
    );
    assert_eq!(
        run_on(&engine, &["--mock", "output", "start", "out_1"]),
        ok("output.start accepted for out_1 (req_1)\n")
    );
    assert_eq!(
        run_on(&engine, &["--json", "--mock", "output", "status", "out_1"]),
        ok(concat!(
            r#"{"id":"req_1","result":{"kind":"query","result":"#,
            r#"{"type":"output.status","output_id":"out_1","state":"active"}}}"#,
            "\n"
        ))
    );
    assert_eq!(
        run_on(&engine, &["--json", "--mock", "output", "stop", "out_1"]),
        ok("{\"id\":\"req_1\",\"result\":{\"kind\":\"action\"}}\n")
    );
}

#[test]
fn events_golden_unknown_skipped_and_an_ended_stream_is_disconnected() {
    let engine = populated();
    let control = session(&engine);
    let events = control.events();
    engine.emit(Event::Unknown);
    block_on(control.call(Call::Batch(Batch::single(Edit::SceneRename {
        scene_id: SceneId::new("main"),
        name: "Live".into(),
    }))))
    .expect("rename");
    engine.emit(Event::OutputStateChanged {
        output_id: OutputId::new("out_1"),
        state: OutputState::Reconnecting,
    });
    let mut out = Vec::new();
    stream_events(events, Some(2), false, &mut out).expect("two events");
    assert_eq!(
        String::from_utf8_lossy(&out),
        "state.changed rev 2 by mock_2 (req_1)\n\
         output.state_changed out_1 reconnecting\n"
    );

    let events = control.events();
    engine.emit(Event::StateChanged {
        rev: Rev(9),
        client_id: ClientId::new("other"),
        request_id: None,
    });
    drop(control); // the session goes away: the stream ends after what it buffered
    let mut out = Vec::new();
    let result = stream_events(events, None, true, &mut out);
    assert_eq!(
        String::from_utf8_lossy(&out),
        "{\"type\":\"state.changed\",\"rev\":9,\"client_id\":\"other\"}\n"
    );
    let error = result.expect_err("the stream ended");
    assert!(matches!(
        error,
        CliError::Control(ControlError::Disconnected { .. })
    ));
    assert_eq!(error.exit_code(), exit::DISCONNECTED);
}

#[test]
fn events_command_waits_for_events_from_another_thread() {
    let engine = populated();
    let done = Arc::new(AtomicBool::new(false));
    let emitter = {
        let (engine, done) = (engine.clone(), Arc::clone(&done));
        // Emits the same event until the CLI has printed two, so the output doesn't
        // depend on when the CLI subscribed.
        thread::spawn(move || {
            while !done.load(Ordering::SeqCst) {
                engine.emit(Event::OutputStateChanged {
                    output_id: OutputId::new("out_1"),
                    state: OutputState::Active,
                });
                thread::sleep(Duration::from_millis(5));
            }
        })
    };
    let outcome = run_on(&engine, &["--mock", "events", "--count", "2"]);
    done.store(true, Ordering::SeqCst);
    emitter.join().expect("emitter thread");
    assert_eq!(
        outcome,
        ok("output.state_changed out_1 active\noutput.state_changed out_1 active\n")
    );
}

#[test]
fn connection_and_usage_failures() {
    assert_eq!(
        run_with(&connect, &["scene", "list"]),
        failed(
            exit::USAGE,
            "error: no engine to talk to: pass `--url ws://127.0.0.1:<port>` (or `--mock`)\n"
        )
    );
    // Nothing listens at the address: disconnected (loopback only, R-TEST-004).
    let dir = std::env::temp_dir().join(format!("jstream-cli-refused-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temp dir");
    let token = dir.join("token");
    std::fs::write(&token, "t0ken").expect("written");
    let token = token.to_str().expect("a UTF-8 temp path");
    let addr = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .expect("a loopback port");
    let url = format!("ws://{addr}");
    let refused = run_with(
        &connect,
        &["--json", "--url", &url, "--token-file", token, "hello"],
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(refused.code, exit::DISCONNECTED);
    assert!(refused.out.is_empty());
    assert!(
        refused.err.starts_with(
            "{\"error\":{\"code\":\"disconnected\",\"message\":\"no connection to the \
             engine: could not connect to the engine: "
        ),
        "{}",
        refused.err
    );
    assert_eq!(
        run_with(&connect, &["--json", "scene"]),
        failed(
            exit::USAGE,
            "{\"error\":{\"code\":\"usage\",\"message\":\"wrong arguments for `scene`; \
             try `jstream-cli help`\"}}\n"
        )
    );
    assert_eq!(
        run_with(&connect, &["scene"]),
        failed(
            exit::USAGE,
            "error: wrong arguments for `scene`; try `jstream-cli help`\n"
        )
    );
    // Regression (review round 1, B1): `--json` after `--mock-output <id>` counts, for
    // usage errors too.
    assert_eq!(
        run_with(
            &connect,
            &["--mock", "--mock-output", "o", "--json", "scene"]
        ),
        failed(
            exit::USAGE,
            "{\"error\":{\"code\":\"usage\",\"message\":\"wrong arguments for `scene`; \
             try `jstream-cli help`\"}}\n"
        )
    );
    assert_eq!(CLIENT_NAME, "jstream-cli");
}

#[test]
fn json_after_mock_output_reports_engine_errors_as_json() {
    // Regression (review round 1, B1).
    let engine = populated();
    assert_eq!(
        run_on(
            &engine,
            &[
                "--mock",
                "--mock-output",
                "out_1",
                "--json",
                "output",
                "stop",
                "out_1"
            ]
        ),
        failed(
            18,
            "{\"error\":{\"code\":\"action_failed\",\"message\":\"action failed: output \
             `out_1` is not running\",\"reason\":\"output `out_1` is not running\"}}\n"
        )
    );
}

/// With the `mock` feature, the real connector's mock knows the outputs it was given.
#[cfg(feature = "mock")]
#[test]
fn mock_connection_with_the_feature() {
    assert_eq!(
        run_with(
            &connect,
            &["--mock", "--mock-output", "o", "output", "status", "o"]
        ),
        ok("o: stopped\n")
    );
}

/// Without the `mock` feature, `--mock` is a usage error that says how to get it.
#[cfg(not(feature = "mock"))]
#[test]
fn mock_connection_without_the_feature() {
    assert_eq!(
        run_with(&connect, &["--mock", "scene", "list"]),
        failed(
            exit::USAGE,
            "error: `--mock` needs a build with the `mock` feature \
             (`cargo build -p jstream-cli --features mock`)\n"
        )
    );
}

#[test]
fn a_remote_connection_needs_an_address_and_a_token() {
    let usage = |result: Result<Box<dyn Control>, CliError>| match result {
        Err(CliError::Usage(message)) => message,
        Err(other) => panic!("expected a usage error, got {other}"),
        Ok(_) => panic!("expected a usage error, got a session"),
    };
    let no_url = usage(connect(&Connection::Remote {
        url: None,
        token_file: None,
    }));
    assert!(no_url.contains("--url"), "{no_url}");
    let missing = usage(connect(&Connection::Remote {
        url: Some("ws://127.0.0.1:1".into()),
        token_file: Some("/nonexistent/jstream-token".into()),
    }));
    assert!(
        missing.contains("could not read the token file"),
        "{missing}"
    );
    let none = read_token(None).err().map(|e| e.to_string());
    assert!(none.is_some_and(|m| m.contains("JSTREAM_TOKEN_FILE")));
}

#[test]
fn the_token_file_is_read_without_surrounding_whitespace() {
    let dir = std::env::temp_dir().join(format!("jstream-cli-token-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temp dir");
    let file = dir.join("token");
    let path = file.to_str().expect("a UTF-8 temp path").to_owned();
    std::fs::write(&file, "  s3cret\n").expect("written");
    assert_eq!(
        read_token(Some(path.clone())).ok().as_deref(),
        Some("s3cret")
    );
    std::fs::write(&file, "\n").expect("written");
    let empty = read_token(Some(path)).err().map(|e| e.to_string());
    assert!(empty.is_some_and(|m| m.contains("is empty")));
    let _ = std::fs::remove_dir_all(&dir);
}
