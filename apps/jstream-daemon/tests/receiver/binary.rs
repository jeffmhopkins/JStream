//! The built `jstream-daemon` binary on its own Linux backends, streaming to the real
//! receiver: no fakes and no in-process wiring. It runs as a user would run it, with its
//! data, config and log folders in the test's scratch folder and no session bus (so the
//! stream key goes to ADR-0071's `0600` file, not the developer's keyring), and a client
//! drives it over its WebSocket API: a colour source on the canvas, an RTMP output with
//! a key, `output.start`.
//!
//! What comes out depends on the machine's H.264 encoders, which the daemon chooses as
//! the product does:
//! - **With one** (VA-API, or Cisco's OpenH264 installed in the daemon's store): the
//!   output goes `active`, and the receiver gets H.264 at the canvas size and AAC, with
//!   red pixels.
//! - **Without one**: the start fails, and the reason tells the user how to get one
//!   (OpenH264, or a VA-API driver). This is CI and the sandbox today: OpenH264's pin has
//!   no recorded SHA-256 yet (LNX2), so no store holds it.
//!
//! `JSTREAM_E2E_OPENH264` (the xtask's `--openh264`) names Cisco's library. Once the pin
//! is recorded the test installs it in the daemon's store, where the daemon checks it
//! against the pin, and then requires the stream. Before that the daemon would refuse it
//! (it never loads an unpinned copy), so the test leaves it out.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use jstream_control::protocol::{
    Action, Batch, Call, CallResult, Event, Hello, OutputId, OutputState, PROTOCOL_VERSION,
    ProtocolError, SecretString,
};
use jstream_control::{Control, ControlError, WsConfig, WsControl};
use jstream_platform_linux::{OPENH264_PIN, OpenH264Store};
use serde_json::{Value, json};

use crate::probe::{Receiver, free_port, pixel, probe};
use crate::{CANVAS, KEY, near, scratch};

/// A running daemon, killed when the test ends.
struct Daemon {
    child: Child,
    url: String,
    token: String,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Where the daemon's data folder is under `home`, as `XDG_DATA_HOME` points it.
fn data_dir(home: &Path) -> PathBuf {
    home.join("data").join("jstream")
}

/// Starts the binary with everything it writes under `dir`, and waits for its
/// `listening on` line. Its stderr (the log) goes to the test's.
fn start_daemon(dir: &Path) -> Daemon {
    let home = dir.join("home");
    std::fs::create_dir_all(&home).expect("the home folder");
    let token_file = dir.join("token");
    let mut child = Command::new(env!("CARGO_BIN_EXE_jstream-daemon"))
        .args(["--port", &free_port().to_string(), "--token-file"])
        .arg(&token_file)
        .arg("--log-dir")
        .arg(dir.join("logs"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &home)
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        // Vulkan's loader and driver choice (the xtask points it at lavapipe).
        .envs(std::env::vars_os().filter(|(name, _)| {
            name.to_str()
                .is_some_and(|n| n.starts_with("VK_") || n == "MESA_VK_DEVICE_SELECT")
        }))
        .env("RUST_LOG", "info")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("the daemon binary runs");
    let stdout = child.stdout.take().expect("piped stdout");
    let (lines, url) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(url) = line.strip_prefix("listening on ") {
                let _ = lines.send(url.to_owned());
            }
        }
    });
    let url = url
        .recv_timeout(Duration::from_secs(20))
        .expect("the daemon serves within 20 s");
    let token = std::fs::read_to_string(&token_file)
        .expect("the daemon wrote its token")
        .trim()
        .to_owned();
    Daemon { child, url, token }
}

/// Puts Cisco's library from `JSTREAM_E2E_OPENH264` in the daemon's store, when the pin
/// has the SHA-256 the daemon checks it against. Whether it did.
fn install_openh264(home: &Path) -> bool {
    let Some(library) = std::env::var_os("JSTREAM_E2E_OPENH264") else {
        return false;
    };
    if OPENH264_PIN.library_sha256.is_none() {
        eprintln!(
            "JSTREAM_E2E_OPENH264 is set, but OpenH264's pin has no SHA-256 yet, so the \
             daemon would refuse the library: testing the daemon without it"
        );
        return false;
    }
    let store = OpenH264Store::new(data_dir(home).join("codecs").join("openh264"));
    let path = store.library_path();
    std::fs::create_dir_all(path.parent().expect("a folder")).expect("the store's folder");
    std::fs::copy(&library, &path).expect("the library is stored");
    true
}

async fn call(control: &WsControl, call: Call) -> Result<CallResult, ControlError> {
    tokio::time::timeout(Duration::from_secs(20), control.call(call))
        .await
        .expect("the daemon answers within 20 s")
        .map(|response| response.result)
}

async fn edit(control: &WsControl, edits: Value) {
    let batch: Batch = serde_json::from_value(json!({ "edits": edits })).expect("a valid batch");
    call(control, Call::Batch(batch))
        .await
        .unwrap_or_else(|e| panic!("the batch was refused: {e}"));
}

/// The real daemon, on its own backends, streams a colour to the receiver, or says how
/// to get the H.264 encoder it lacks.
#[ignore = "needs ffmpeg, ffprobe and a Vulkan adapter: cargo xtask e2e-stream"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_daemon_binary_streams_on_its_linux_backends() {
    let dir = scratch("binary");
    let openh264 = install_openh264(&dir.join("home"));
    let daemon = start_daemon(&dir);
    let control = WsControl::connect(WsConfig::new(daemon.url.clone(), daemon.token.clone()))
        .await
        .expect("connects with the daemon's token");
    control
        .hello(Hello {
            protocol: PROTOCOL_VERSION,
            client_name: "receiver-e2e-binary".into(),
        })
        .await
        .expect("welcome");
    let mut events = control.events();

    let port = free_port();
    edit(
        &control,
        json!([
            {"type": "canvas.set_size", "width": CANVAS.0, "height": CANVAS.1},
            {"type": "scene.create", "scene_id": "main", "name": "Main"},
            {"type": "source.create", "source_id": "red", "name": "Red",
             "kind": {"type": "builtin", "kind": "color"},
             "settings": {"color": "#ff0000ff", "width": CANVAS.0, "height": CANVAS.1}},
            {"type": "item.add", "scene_id": "main", "item_id": "red_1", "source_id": "red"},
            {"type": "output.create", "output_id": "live",
             "kind": {"type": "rtmp", "target": {"type": "generic",
                      "url": format!("rtmp://127.0.0.1:{port}/live")}}},
        ]),
    )
    .await;
    call(
        &control,
        Call::Action {
            action: Action::OutputSetKey {
                output_id: OutputId::new("live"),
                key: SecretString::new(KEY),
            },
        },
    )
    .await
    .expect("the key is stored");
    // ADR-0071's fallback: no session bus, so the key is in the config folder's file.
    let secrets = dir.join("home/config/jstream/secrets");
    let text = std::fs::read_to_string(&secrets).expect("the fallback file holds the key");
    assert!(text.contains(KEY), "the key is in {}", secrets.display());

    let receiver = Receiver::start(port, KEY, &dir.join("binary.flv"));
    let started = call(
        &control,
        Call::Action {
            action: Action::OutputStart {
                output_id: OutputId::new("live"),
            },
        },
    )
    .await;
    // The start is accepted, and then the pipeline opens its encoders: either it goes
    // active, or it fails and says why.
    let failed = started.as_ref().err().map(|e| match e.protocol() {
        Some(ProtocolError::ActionFailed { reason }) => reason.clone(),
        _ => format!("{e}"),
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut active = false;
    while failed.is_none() && !active {
        let left = deadline.saturating_duration_since(Instant::now());
        let event = tokio::time::timeout(left, events.next())
            .await
            .expect("the output goes active or fails within 30 s")
            .expect("the event stream stays open");
        match event {
            Event::OutputStateChanged { output_id, state } if output_id.as_str() == "live" => {
                match state {
                    OutputState::Active => active = true,
                    // The start was accepted and failed later: no reason comes with the
                    // event, so say where to look rather than blame the encoder.
                    OutputState::Failed => panic!(
                        "the output failed after `output.start` was accepted; the reason is \
                         in the daemon's log, {}",
                        dir.join("logs").display()
                    ),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    if let Some(reason) = failed {
        drop(receiver.kill());
        eprintln!("the daemon has no H.264 encoder here: {reason}");
        assert!(
            !openh264,
            "OpenH264 is installed in the daemon's store, so it must stream: {reason}"
        );
        assert!(
            reason.contains("OpenH264") && reason.contains("VA-API"),
            "the reason says how to get an H.264 encoder: {reason}"
        );
        return;
    }

    // Five seconds on the air, then stop and read what arrived.
    tokio::time::sleep(Duration::from_secs(5)).await;
    call(
        &control,
        Call::Action {
            action: Action::OutputStop {
                output_id: OutputId::new("live"),
            },
        },
    )
    .await
    .expect("output.stop");
    let file = receiver.finish();
    let got = probe(&file);
    eprintln!("the daemon binary's stream: {got:?}");
    assert_eq!(got.video_codec, "h264", "{got:?}");
    assert_eq!(
        (got.width, got.height),
        (u64::from(CANVAS.0), u64::from(CANVAS.1)),
        "{got:?}"
    );
    assert_eq!(got.audio_codec.as_deref(), Some("aac"), "{got:?}");
    assert!(got.duration > 3.0, "{got:?}");
    let rgb = pixel(&file, 1.0, u64::from(CANVAS.0 / 2), u64::from(CANVAS.1 / 2));
    assert!(near(rgb, [255, 0, 0]), "red, got {rgb:?}");
}
